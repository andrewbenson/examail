//! The AppKit side, which makes winit's window delegate a destination for text as well as files,
//! and is the source of a selection's drag.

use std::{cell::RefCell, sync::OnceLock};

use egui::{Id, Pos2, Rect, pos2};
use exo_ui::text_drag::{Dropped, Outgoing, Over, Report};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool, Imp, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSImage, NSPasteboard,
    NSPasteboardType, NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypeString,
    NSPasteboardWriting, NSWindow,
};
use objc2_foundation::{
    NSArray, NSData, NSObject, NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSSize, NSString,
};

/// The class of winit's window delegate, which AppKit asks about drags over its window.
const DELEGATE: &std::ffi::CStr = c"WinitWindowDelegate";
/// What the dragged HTML starts with, so that whatever reads it takes it as UTF-8.
const HTML_HEADER: &str = "<meta http-equiv=\"Content-Type\" content=\"text/html;charset=UTF-8\">";

/// The window as its last frame left it.
struct Window {
    /// egui's points per Mac point (the zoom).
    zoom: f32,
    /// Where text can be let go, in egui's points.
    targets: Vec<Rect>,
}

/// A drag of an editor's selection that Exo started.
struct Session {
    source: Retained<Source>,
    /// The editor it started in.
    editor: Id,
    /// Whether it was let go in the window as a move (without ⌥).
    moved_here: bool,
    /// Who took that drop.
    taken: Option<Id>,
    /// Whether AppKit says it's over.
    over: bool,
}

#[derive(Default)]
struct State {
    ctx: Option<egui::Context>,
    window: Option<Window>,
    /// Text over the window, and let go on it, for its next frame.
    over: Option<Over>,
    dropped: Option<Dropped>,
    session: Option<Session>,
    /// A drag that couldn't start or is over, for its editor's next frame, with whether it moved.
    ended: Option<(Id, bool)>,
    /// A finished drag's source, let go of once AppKit is done with it.
    finished: Option<Retained<Source>>,
    /// Whether the window is a destination for text.
    registered: bool,
    /// What winit's delegate answered for files on entering (an `NSDragOperation`). AppKit asks
    /// again as the drag moves, and winit has no `draggingUpdated:`, so Exo gives this answer
    /// again.
    files: usize,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

/// How winit declared `draggingEntered:` (`NSDragOperation` integer vs `BOOL`).
#[derive(Clone, Copy)]
enum Entered {
    Operation(unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> usize),
    Bool(unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> Bool),
}

/// winit's own destination methods, which Exo's methods call for files.
struct Winit {
    entered: Entered,
    perform: unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> Bool,
    exited: unsafe extern "C-unwind" fn(&AnyObject, Sel, Option<&AnyObject>),
}

static WINIT: OnceLock<Option<Winit>> = OnceLock::new();

/// See [`super::begin`]. Returns whether it ran, which it does on the main thread once winit's
/// delegate exists.
pub(super) fn begin(ctx: &egui::Context) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    if WINIT.get_or_init(install).is_none() {
        return false;
    }
    register(mtm);
    let (over, dropped, ended) = with(|state| {
        state.ctx = Some(ctx.clone());
        state.finished = None;
        let dropped = state.dropped.take();
        // A drag's end reaches its editor a frame after its drop has been taken.
        if dropped.is_none()
            && let Some(session) = state.session.take_if(|session| session.over)
        {
            let moved =
                session.moved_here && session.taken.is_some_and(|taker| taker != session.editor);
            state.ended = Some((session.editor, moved));
            state.finished = Some(session.source);
        }
        (state.over, dropped, state.ended.take())
    });
    exo_ui::text_drag::begin(ctx, true, over, dropped, ended);
    true
}

/// See [`super::end`].
pub(super) fn end(ctx: &egui::Context, report: Report) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let outgoing = with(|state| {
        state.window = Some(Window {
            zoom: ctx.zoom_factor(),
            targets: report.targets,
        });
        if let (Some(session), Some(taker)) = (&mut state.session, report.taken) {
            session.taken = Some(taker);
        }
        report.outgoing
    });
    if let Some(outgoing) = outgoing {
        let editor = outgoing.source;
        if !start(mtm, ctx, outgoing) {
            with(|state| state.ended = Some((editor, false)));
            ctx.request_repaint();
        }
    }
}

/// Makes Exo's window a destination for text, beside winit's files.
fn register(mtm: MainThreadMarker) {
    if with(|state| state.registered) {
        return;
    }
    let Some(delegate) = AnyClass::get(DELEGATE) else {
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        // SAFETY: `delegate` returns the window's delegate or nil.
        let ours: Option<Retained<AnyObject>> = unsafe { msg_send![&*window, delegate] };
        if !ours.is_some_and(|ours| ours.class() == delegate) {
            continue;
        }
        // SAFETY: the pasteboard type statics are AppKit's constants.
        #[expect(
            deprecated,
            reason = "winit takes files by `NSFilenamesPboardType`, kept registered beside text's"
        )]
        let types = unsafe {
            NSArray::from_slice(&[
                objc2_app_kit::NSFilenamesPboardType,
                NSPasteboardTypeString,
                NSPasteboardTypeHTML,
            ])
        };
        window.registerForDraggedTypes(&types);
        with(|state| state.registered = true);
    }
}

/// Points the destination methods of winit's delegate at Exo's, which hand files on to winit's.
/// `None` when the class or its methods aren't there.
fn install() -> Option<Winit> {
    let class = AnyClass::get(DELEGATE)?;
    let find = |sel: Sel| {
        class
            .instance_methods()
            .iter()
            .copied()
            .find(|method| method.name() == sel)
    };
    let (entered, perform, exited) = (
        find(sel!(draggingEntered:))?,
        find(sel!(performDragOperation:))?,
        find(sel!(draggingExited:))?,
    );
    type Operation = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> usize;
    type Perform = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> Bool;
    type Exited = unsafe extern "C-unwind" fn(&AnyObject, Sel, Option<&AnyObject>);
    enum EnteredEncoding {
        Operation,
        Bool,
    }
    let entered_encoding = match entered.return_type().to_bytes() {
        b"Q" | b"L" | b"I" => EnteredEncoding::Operation,
        b"B" | b"c" => EnteredEncoding::Bool,
        other => {
            log::warn!(
                "drags: unexpected draggingEntered: return encoding {:?}",
                String::from_utf8_lossy(other)
            );
            return None;
        }
    };
    // SAFETY: each replacement has the signature AppKit calls the method with (`Q@:@`, `B@:@`,
    // `v@:@`), and winit's `draggingEntered:` is called with the return ABI its own Objective-C
    // encoding declares. `draggingUpdated:` is added because winit has none.
    unsafe {
        let set = |method: &objc2::runtime::Method, imp: Imp| method.set_implementation(imp);
        let entered_imp = set(
            entered,
            std::mem::transmute::<Operation, Imp>(dragging_entered),
        );
        let entered = match entered_encoding {
            EnteredEncoding::Operation => {
                Entered::Operation(std::mem::transmute::<Imp, Operation>(entered_imp))
            }
            EnteredEncoding::Bool => {
                Entered::Bool(std::mem::transmute::<Imp, Perform>(entered_imp))
            }
        };
        let winit = Winit {
            entered,
            perform: std::mem::transmute::<Imp, Perform>(set(
                perform,
                std::mem::transmute::<Perform, Imp>(perform_drag),
            )),
            exited: std::mem::transmute::<Imp, Exited>(set(
                exited,
                std::mem::transmute::<Exited, Imp>(dragging_exited),
            )),
        };
        let added = objc2::ffi::class_addMethod(
            (class as *const AnyClass).cast_mut(),
            sel!(draggingUpdated:),
            std::mem::transmute::<Operation, Imp>(dragging_updated),
            c"Q@:@".as_ptr(),
        );
        if !added.as_bool() {
            log::warn!("drags: {DELEGATE:?} has a draggingUpdated: already");
        }
        Some(winit)
    }
}

/// Whether the drag that `info` describes carries files, which winit handles.
fn has_files(info: &AnyObject) -> bool {
    // SAFETY: `info` is an `NSDraggingInfo`.
    let pasteboard: Retained<NSPasteboard> = unsafe { msg_send![info, draggingPasteboard] };
    // SAFETY: AppKit's constant.
    pasteboard
        .availableTypeFromArray(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]))
        .is_some()
}

unsafe extern "C-unwind" fn dragging_entered(
    delegate: &AnyObject,
    sel: Sel,
    info: &AnyObject,
) -> usize {
    if has_files(info) {
        let Some(Some(winit)) = WINIT.get() else {
            return 0;
        };
        // SAFETY: winit's implementation, with AppKit's arguments and winit's declared return ABI.
        let files = match winit.entered {
            Entered::Operation(entered) => NSDragOperation(unsafe { entered(delegate, sel, info) }),
            Entered::Bool(entered) => {
                if unsafe { entered(delegate, sel, info) }.as_bool() {
                    NSDragOperation::Copy
                } else {
                    NSDragOperation::None
                }
            }
        };
        with(|state| state.files = files.0);
        return files.0;
    }
    text_over(info).0
}

unsafe extern "C-unwind" fn dragging_updated(
    _delegate: &AnyObject,
    _sel: Sel,
    info: &AnyObject,
) -> usize {
    if has_files(info) {
        return with(|state| state.files);
    }
    text_over(info).0
}

unsafe extern "C-unwind" fn perform_drag(delegate: &AnyObject, sel: Sel, info: &AnyObject) -> Bool {
    if has_files(info) {
        let Some(Some(winit)) = WINIT.get() else {
            return Bool::NO;
        };
        // SAFETY: winit's implementation, with AppKit's arguments.
        return unsafe { (winit.perform)(delegate, sel, info) };
    }
    Bool::new(text_dropped(info))
}

unsafe extern "C-unwind" fn dragging_exited(
    delegate: &AnyObject,
    sel: Sel,
    info: Option<&AnyObject>,
) {
    if let Some(Some(winit)) = WINIT.get() {
        // SAFETY: winit's implementation, with AppKit's arguments.
        unsafe { (winit.exited)(delegate, sel, info) };
    }
    if with(|state| state.over.take()).is_some() {
        repaint();
    }
}

/// Where a drag is, in the window.
struct Place {
    at: Pos2,
    /// Whether it's over somewhere that takes text.
    target: bool,
    /// The editor whose selection it is, when the drag is Exo's.
    source: Option<Id>,
    /// What it does here.
    operation: NSDragOperation,
}

/// Where the drag that `info` describes is in the window, and what it would do there.
fn place(info: &AnyObject) -> Option<Place> {
    // SAFETY: `info` is an `NSDraggingInfo`.
    let (window, location, source, mask): (
        Option<Retained<NSWindow>>,
        NSPoint,
        Option<Retained<AnyObject>>,
        NSDragOperation,
    ) = unsafe {
        (
            msg_send![info, draggingDestinationWindow],
            msg_send![info, draggingLocation],
            msg_send![info, draggingSource],
            msg_send![info, draggingSourceOperationMask],
        )
    };
    let view = window?.contentView()?;
    let in_view = view.convertPoint_fromView(location, None);
    let y = if view.isFlipped() {
        in_view.y
    } else {
        view.frame().size.height - in_view.y
    };
    with(|state| {
        let window = state.window.as_ref()?;
        let at = pos2(in_view.x as f32, y as f32) / window.zoom;
        let target = window.targets.iter().any(|target| target.contains(at));
        let ours = state.session.as_ref().filter(|session| {
            source.as_deref().is_some_and(|source| {
                std::ptr::eq(
                    source as *const AnyObject,
                    Retained::as_ptr(&session.source).cast(),
                )
            })
        });
        let operation = if !target {
            NSDragOperation::None
        } else if ours.is_some() && mask.contains(NSDragOperation::Move) {
            NSDragOperation::Move
        } else if mask.contains(NSDragOperation::Copy) {
            NSDragOperation::Copy
        } else {
            mask & NSDragOperation::Generic
        };
        Some(Place {
            at,
            target,
            source: ours.map(|session| session.editor),
            operation,
        })
    })
}

/// Records where text dragged over the window is, for its next frame, and returns what the drop
/// would do.
fn text_over(info: &AnyObject) -> NSDragOperation {
    let Some(place) = place(info) else {
        return NSDragOperation::None;
    };
    with(|state| {
        state.over = Some(Over {
            at: place.at,
            source: place.source,
        })
    });
    repaint();
    place.operation
}

/// Records the text let go over the window, for its next frame. Returns whether something there
/// takes it.
fn text_dropped(info: &AnyObject) -> bool {
    let Some(place) =
        place(info).filter(|place| place.target && place.operation != NSDragOperation::None)
    else {
        return false;
    };
    // SAFETY: `info` is an `NSDraggingInfo`.
    let pasteboard: Retained<NSPasteboard> = unsafe { msg_send![info, draggingPasteboard] };
    // SAFETY: AppKit's constants.
    let (html, text) = unsafe {
        (
            pasteboard.stringForType(NSPasteboardTypeHTML),
            pasteboard.stringForType(NSPasteboardTypeString),
        )
    };
    let (html, text) = (
        html.map(|html| html.to_string()),
        text.map(|text| text.to_string()),
    );
    if html.is_none() && text.is_none() {
        return false;
    }
    let copy = place.operation != NSDragOperation::Move;
    with(|state| {
        state.over = None;
        state.dropped = Some(Dropped {
            at: place.at,
            html,
            text: text.unwrap_or_default(),
            source: place.source,
            copy,
        });
        if let Some(session) = state.session.as_mut().filter(|_| place.source.is_some()) {
            session.moved_here = !copy;
        }
    });
    repaint();
    true
}

fn repaint() {
    if let Some(ctx) = with(|state| state.ctx.clone()) {
        ctx.request_repaint();
    }
}

/// What a selection's drag carries.
struct Payload {
    html: String,
    text: String,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Source` doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ExoTextDragSource"]
    #[ivars = Payload]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSDraggingSource for Source {
        // Within Exo the drag can move the text. Other apps copy it.
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            context: NSDraggingContext,
        ) -> NSDragOperation {
            if context == NSDraggingContext::WithinApplication {
                NSDragOperation::Copy | NSDragOperation::Move
            } else {
                NSDragOperation::Copy
            }
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(
            &self,
            _session: &NSDraggingSession,
            _point: NSPoint,
            _operation: NSDragOperation,
        ) {
            let ours = with(|state| {
                state.session.as_mut()?.over = true;
                Some(())
            });
            if ours.is_some() {
                super::release();
                repaint();
            }
        }
    }

    unsafe impl NSPasteboardWriting for Source {
        #[unsafe(method_id(writableTypesForPasteboard:))]
        fn writable_types(
            &self,
            _pasteboard: &NSPasteboard,
        ) -> Retained<NSArray<NSPasteboardType>> {
            // SAFETY: AppKit's constants.
            unsafe { NSArray::from_slice(&[NSPasteboardTypeHTML, NSPasteboardTypeString]) }
        }

        #[unsafe(method_id(pasteboardPropertyListForType:))]
        fn property_list(&self, kind: &NSPasteboardType) -> Option<Retained<AnyObject>> {
            let Payload { html, text } = self.ivars();
            // SAFETY: AppKit's constant.
            let value = if kind == unsafe { NSPasteboardTypeHTML } {
                format!("{HTML_HEADER}{html}")
            } else {
                text.clone()
            };
            Some(Retained::into_super(Retained::into_super(
                NSString::from_str(&value),
            )))
        }
    }
);

/// Starts the Mac's drag of `outgoing` from the window (the key window), at the pointer. Returns
/// whether it could.
fn start(mtm: MainThreadMarker, ctx: &egui::Context, outgoing: Outgoing) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.keyWindow() else {
        return false;
    };
    let Some(view) = window.contentView() else {
        return false;
    };
    let Some(pointer) = ctx.input(|input| input.pointer.latest_pos()) else {
        return false;
    };
    let zoom = ctx.zoom_factor();
    let source = Source::alloc(mtm).set_ivars(Payload {
        html: outgoing.html,
        text: outgoing.text,
    });
    // SAFETY: `NSObject`'s designated initializer.
    let source: Retained<Source> = unsafe { msg_send![super(source), init] };
    let item = NSDraggingItem::initWithPasteboardWriter(
        NSDraggingItem::alloc(),
        ProtocolObject::from_ref(&*source),
    );
    let flipped = view.isFlipped();
    let height = view.frame().size.height;
    let to_view = |rect: Rect| {
        let y = if flipped {
            f64::from(rect.min.y * zoom)
        } else {
            height - f64::from(rect.max.y * zoom)
        };
        NSRect::new(
            NSPoint::new(f64::from(rect.min.x * zoom), y),
            NSSize::new(
                f64::from(rect.width() * zoom),
                f64::from(rect.height() * zoom),
            ),
        )
    };
    let picture = outgoing
        .image
        .and_then(|(image, at)| Some((ns_image(&image)?, at)));
    match picture {
        // SAFETY: an `NSImage` is what a dragging item's contents may be.
        Some((image, at)) => unsafe { item.setDraggingFrame_contents(to_view(at), Some(&image)) },
        None => item.setDraggingFrame(to_view(Rect::from_center_size(
            pointer,
            egui::Vec2::splat(1.0),
        ))),
    }
    let y = if flipped {
        f64::from(pointer.y * zoom)
    } else {
        height - f64::from(pointer.y * zoom)
    };
    let point = NSPoint::new(f64::from(pointer.x * zoom), y);
    // AppKit wants the mouse event that the drag comes from. egui has had that event already, so a
    // dragged event at the pointer stands in for it.
    let Some(event) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        view.convertPoint_toView(point, None),
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    ) else {
        return false;
    };
    with(|state| {
        state.session = Some(Session {
            source: source.clone(),
            editor: outgoing.source,
            moved_here: false,
            taken: None,
            over: false,
        });
    });
    view.beginDraggingSessionWithItems_event_source(
        &NSArray::from_retained_slice(&[item]),
        &event,
        ProtocolObject::from_ref(&*source),
    );
    true
}

/// `image` (premultiplied) as an `NSImage`, through a PNG.
fn ns_image(image: &egui::ColorImage) -> Option<Retained<NSImage>> {
    let pixels = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_srgba_unmultiplied())
        .collect();
    let rgba = image::RgbaImage::from_raw(
        u32::try_from(image.size[0]).ok()?,
        u32::try_from(image.size[1]).ok()?,
        pixels,
    )?;
    let mut png = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut png, image::ImageFormat::Png).ok()?;
    NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png.get_ref()))
}
