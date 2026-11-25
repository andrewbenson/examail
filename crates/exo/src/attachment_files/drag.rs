//! Dragging a card out of the window. The card goes out as a native drag of a file promise, which
//! Finder (or any app that takes files) fulfils by asking for the file where it was dropped. Exo
//! fetches the part (from the parts cache first), or reads the kept file, when the promise is
//! called in.

use std::path::PathBuf;

use block2::RcBlock;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSFilePromiseProvider,
    NSFilePromiseProviderDelegate, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSError, NSObject, NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSSize, NSString,
    NSURL, ns_string,
};

use super::Part;
use crate::account::Loader;

/// What a promise needs to keep: the file and how to fetch it.
pub(crate) struct Promise {
    part: Part,
    loader: Loader,
}

/// AppKit's completion handler, called from the fetch's thread.
struct Completion(RcBlock<dyn Fn(*mut NSError)>);

// SAFETY: a file promise's completion handler may be called from any thread once the file is
// written (`NSFilePromiseProviderDelegate`), and this one is called exactly once.
unsafe impl Send for Completion {}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Source` doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ExoAttachmentDragSource"]
    #[ivars = Promise]
    pub(crate) struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSDraggingSource for Source {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        // AppKit's drag loop took the button's release, so the window hears about it here.
        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(
            &self,
            _session: &NSDraggingSession,
            _point: NSPoint,
            _operation: NSDragOperation,
        ) {
            crate::desktop::drag::release();
        }
    }

    unsafe impl NSFilePromiseProviderDelegate for Source {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&super::file_name(self.ivars().part.name()))
        }

        // Called on the main queue (there's no `operationQueueForFilePromiseProvider:`), so it
        // only starts the fetch. The completion handler is called from the fetch's thread.
        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let completion = Completion(completion.copy());
            let Some(path) = url.to_file_path() else {
                fail(&completion);
                return;
            };
            let done = move |bytes: Option<Vec<u8>>| {
                if bytes.is_some_and(|bytes| write(&path, &bytes)) {
                    completion.0.call((std::ptr::null_mut(),))
                } else {
                    fail(&completion)
                }
            };
            let Promise { part, loader } = self.ivars();
            match part {
                Part::Gmail(attachment) => {
                    loader.attachment(attachment, move |bytes| done(bytes.ok()))
                }
                Part::Kept(_, kept) => {
                    let kept = kept.clone();
                    std::thread::spawn(move || done(std::fs::read(kept).ok()));
                }
            }
        }
    }
);

/// Writes `bytes` to `path`, quarantined as a download. Logs why it couldn't.
fn write(path: &PathBuf, bytes: &[u8]) -> bool {
    match std::fs::write(path, bytes) {
        Ok(()) => {
            super::quarantine(path);
            true
        }
        Err(error) => {
            log::warn!("writing a dragged attachment: {error}");
            false
        }
    }
}

/// Tells AppKit the promise wasn't kept.
fn fail(completion: &Completion) {
    let error = NSError::new(-1, ns_string!("ExoAttachmentDrag"));
    completion.0.call((Retained::as_ptr(&error).cast_mut(),));
}

/// Starts dragging file `part` from `at` (egui points in the window) and returns the drag's
/// source, which must live until the drop has been written. Returns `None` off the main thread or
/// without a window.
pub(crate) fn start(
    loader: &Loader,
    part: Part,
    at: egui::Pos2,
    zoom: f32,
) -> Option<Retained<Source>> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app.keyWindow().or_else(|| app.mainWindow())?;
    let view = window.contentView()?;
    let safe_name = super::file_name(part.name());
    let ext = super::extension(&safe_name);
    let uti = promise_uti(ext.as_deref().unwrap_or_default(), part_mime_type(&part));
    let icon_kind = ext.as_deref().unwrap_or(uti);
    #[expect(
        deprecated,
        reason = "`iconForFileType:` resolves extension and UTI icons without requiring the file on disk"
    )]
    let icon = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(icon_kind));
    let source = Source::alloc(mtm).set_ivars(Promise {
        part,
        loader: loader.clone(),
    });
    // SAFETY: `NSObject`'s designated initializer.
    let source: Retained<Source> = unsafe { msg_send![super(source), init] };
    let provider = NSFilePromiseProvider::initWithFileType_delegate(
        NSFilePromiseProvider::alloc(),
        &NSString::from_str(uti),
        ProtocolObject::from_ref(&*source),
    );
    let item = NSDraggingItem::initWithPasteboardWriter(
        NSDraggingItem::alloc(),
        ProtocolObject::from_ref(&*provider),
    );
    let y = if view.isFlipped() {
        f64::from(at.y * zoom)
    } else {
        view.frame().size.height - f64::from(at.y * zoom)
    };
    let point = NSPoint::new(f64::from(at.x * zoom), y);
    // SAFETY: an `NSImage` is what a dragging item's contents may be.
    unsafe {
        item.setDraggingFrame_contents(
            NSRect::new(
                NSPoint::new(point.x - 16.0, point.y - 16.0),
                NSSize::new(32.0, 32.0),
            ),
            Some(&icon),
        )
    };
    // AppKit wants the mouse event the drag comes from. egui has had that event already, so a
    // dragged event at the pointer stands in for it.
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        view.convertPoint_toView(point, None),
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )?;
    view.beginDraggingSessionWithItems_event_source(
        &NSArray::from_retained_slice(&[item]),
        &event,
        ProtocolObject::from_ref(&*source),
    );
    Some(source)
}

fn part_mime_type(part: &Part) -> Option<&str> {
    match part {
        Part::Gmail(attachment) => Some(attachment.mime_type.as_str()),
        Part::Kept(..) => None,
    }
}

/// Uniform Type Identifier conforming to `public.data` for `NSFilePromiseProvider`.
fn promise_uti(extension: &str, mime_type: Option<&str>) -> &'static str {
    match extension {
        "pdf" => return "com.adobe.pdf",
        "png" => return "public.png",
        "jpg" | "jpeg" | "jif" | "jfif" => return "public.jpeg",
        "gif" => return "com.compuserve.gif",
        "webp" => return "org.webmproject.webp",
        "heic" => return "public.heic",
        "svg" => return "public.svg-image",
        "bmp" => return "com.microsoft.bmp",
        "tif" | "tiff" => return "public.tiff",
        "txt" | "log" => return "public.plain-text",
        "md" | "markdown" => return "net.daringfireball.markdown",
        "html" | "htm" => return "public.html",
        "csv" => return "public.comma-separated-values-text",
        "json" => return "public.json",
        "xml" => return "public.xml",
        "rtf" => return "public.rtf",
        "eml" => return "com.apple.mail.email",
        "ics" => return "com.apple.ical.ics",
        "vcf" => return "public.vcard",
        "zip" => return "public.zip-archive",
        "gz" | "tgz" => return "org.gnu.gnu-zip-archive",
        "mp3" => return "public.mp3",
        "wav" => return "com.microsoft.waveform-audio",
        "m4a" => return "com.apple.m4a-audio",
        "mp4" => return "public.mpeg-4",
        "mov" => return "com.apple.quicktime-movie",
        "doc" => return "com.microsoft.word.doc",
        "docx" => return "org.openxmlformats.wordprocessingml.document",
        "xls" => return "com.microsoft.excel.xls",
        "xlsx" => return "org.openxmlformats.spreadsheetml.sheet",
        "ppt" => return "com.microsoft.powerpoint.ppt",
        "pptx" => return "org.openxmlformats.presentationml.presentation",
        _ => {}
    }
    let Some(mime) = mime_type.map(|m| m.split(';').next().unwrap_or(m).trim()) else {
        return "public.data";
    };
    match mime.to_ascii_lowercase().as_str() {
        "application/pdf" => "com.adobe.pdf",
        "image/png" => "public.png",
        "image/jpeg" | "image/jpg" | "image/jif" | "image/jfif" => "public.jpeg",
        "image/gif" => "com.compuserve.gif",
        "image/webp" => "org.webmproject.webp",
        "image/heic" => "public.heic",
        "image/svg+xml" => "public.svg-image",
        "image/bmp" => "com.microsoft.bmp",
        "image/tiff" => "public.tiff",
        "text/plain" => "public.plain-text",
        "text/html" => "public.html",
        "text/csv" => "public.comma-separated-values-text",
        "application/json" => "public.json",
        "application/xml" | "text/xml" => "public.xml",
        "application/rtf" | "text/rtf" => "public.rtf",
        "message/rfc822" => "com.apple.mail.email",
        "text/calendar" => "com.apple.ical.ics",
        "text/vcard" => "public.vcard",
        "application/zip" => "public.zip-archive",
        "application/gzip" => "org.gnu.gnu-zip-archive",
        "audio/mpeg" => "public.mp3",
        "video/mp4" => "public.mpeg-4",
        "video/quicktime" => "com.apple.quicktime-movie",
        _ => "public.data",
    }
}

#[cfg(test)]
mod tests {
    use super::promise_uti;

    #[test]
    fn maps_extensions_and_mime_types_to_data_utis() {
        assert_eq!(promise_uti("pdf", None), "com.adobe.pdf");
        assert_eq!(promise_uti("jpeg", None), "public.jpeg");
        assert_eq!(promise_uti("", Some("image/png; name=logo")), "public.png");
        assert_eq!(
            promise_uti("unknown", Some("application/octet-stream")),
            "public.data"
        );
        assert_eq!(promise_uti("app", None), "public.data");
    }
}
