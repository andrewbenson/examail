//! The print view, a WKWebView that's never shown. It loads the conversation's page, its fonts and
//! what its bodies load through its own URL schemes, then prints the page with an
//! `NSPrintOperation` (WebKit lays it out and paginates it as Chromium would). While the print
//! panel is up, new paper in it lays the page out again for that paper. Everything here runs on
//! the main thread, and loads finish there too.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    path::PathBuf,
    time::Duration,
};

use block2::RcBlock;
use dispatch2::DispatchTime;
use exo_store::Attachment;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message as _, define_class,
    msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSModalResponseOK, NSPaperOrientation, NSPrintInfo,
    NSPrintJobSavingURL, NSPrintOperation, NSPrintSaveJob, NSSavePanel, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSCopying, NSData, NSError, NSNotificationCenter, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL, NSURLResponse,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationDelegate, WKURLSchemeHandler, WKURLSchemeTask,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore,
};

use super::{Destination, Done, Page, Printout, Resource, document};
use crate::account::Loader;

/// Moves each body's template into a shadow root on its `.Shadowbox` host, once the page is
/// parsed. It's a script rather than declarative shadow DOM, which WebKit only parses from macOS
/// 13.3. It runs in its own world because the page has no scripts.
const SHADOW_ROOTS: &str = "for (const template of document.querySelectorAll('.Shadowbox > template')) { template.parentElement.attachShadow({ mode: 'open' }).appendChild(template.content); template.remove(); }";
/// What the page does once loaded, before it prints. It waits for its fonts (the load event has
/// waited for images), then lays out each image that didn't load the way Chromium lays out a
/// broken one. An image with both dimension attributes keeps that size if it has no alt text or is
/// a block. An inline image that has alt text or lacks either attribute is a 16px square. A blocked
/// image keeps the box `body.css` gives it, and any other shows nothing. Last, it marks the bodies
/// of three lines or fewer, which Chromium can't break while keeping the hairline's padding with
/// the last two lines (`print.css`): the body's wrapper, or the body alone under an invitation's
/// card, which breaks between its rows.
const BEFORE_PRINT: &str = r#"
await document.fonts.ready;
const size = (value) => { const number = parseFloat(value); return number > 0 && !String(value).includes('%') ? number : 0; };
for (const host of document.querySelectorAll('.Shadowbox')) {
  for (const image of host.shadowRoot ? host.shadowRoot.querySelectorAll('img') : []) {
    if (!image.complete || image.naturalWidth > 0) continue;
    const [width, height] = [size(image.getAttribute('width')), size(image.getAttribute('height'))];
    const inline = getComputedStyle(image).display === 'inline';
    const box = width && height && (!image.alt || !inline) ? [width, height] : inline ? [16, 16] : null;
    if (box) {
      image.style.setProperty('width', box[0] + 'px', 'important');
      image.style.setProperty('height', box[1] + 'px', 'important');
    }
    if (!image.hasAttribute('data-exo-blocked')) image.style.setProperty('visibility', 'hidden', 'important');
  }
  const body = host.shadowRoot && host.shadowRoot.querySelector('.ShadowBody');
  if (body && body.getBoundingClientRect().height <= 3 * parseFloat(getComputedStyle(body).lineHeight)) (host.parentElement.querySelector('.CalendarInvite') ? host : host.parentElement).classList.add('isShort');
}
return true;
"#;
/// The schemes the page loads from (`document`'s, and `exo_mail`'s for bodies).
const SCHEMES: [&str; 4] = [document::SCHEME, "exo-https", "exo-http", "cid"];

thread_local! {
    /// Print jobs under way, which hold their web views, windows and delegates.
    static JOBS: RefCell<Vec<Job>> = const { RefCell::new(Vec::new()) };
    /// Print jobs waiting on the account's `RemoteContentBlocking` setting before building HTML.
    static PENDING: RefCell<HashMap<u64, PendingJob>> = RefCell::new(HashMap::new());
    /// Loads under way, by id, until they finish or WebKit stops them.
    static TASKS: RefCell<HashMap<u64, Retained<ProtocolObject<dyn WKURLSchemeTask>>>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

struct PendingJob {
    printout: Printout,
    loader: Loader,
    destination: Destination,
    done: Option<Done>,
}

fn next_id() -> u64 {
    NEXT_ID.with(|next| {
        next.set(next.get() + 1);
        next.get()
    })
}

/// How often the print panel's paper is checked while it's up.
const PAPER_CHECK: Duration = Duration::from_millis(250);

/// One print job's objects, kept until it's done.
struct Job {
    id: u64,
    web_view: Retained<WKWebView>,
    _window: Retained<NSWindow>,
    printer: Retained<Printer>,
    _handler: Retained<Handler>,
}

/// The paper the print panel starts on, which is the default printer's.
fn default_page() -> Page {
    page(NSPrintInfo::sharedPrintInfo().paperSize())
}

/// The page for paper `size`, upright (`@page { size: portrait }`).
fn page(NSSize { width, height }: NSSize) -> Page {
    let (width, height) = (width as f32, height as f32);
    Page {
        paper: [width.min(height), width.max(height)],
    }
}

/// Asks where to save the PDF (a sheet titled `Save as PDF` proposing `file_name`, PDF files
/// only), then hands the path to `chosen`.
pub(super) fn choose_pdf(file_name: &str, chosen: impl FnOnce(PathBuf) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("Save as PDF opens its panel on the main thread only");
        return;
    };
    let panel = NSSavePanel::savePanel(mtm);
    panel.setTitle(Some(&NSString::from_str("Save as PDF")));
    panel.setNameFieldStringValue(&NSString::from_str(file_name));
    // PDF files only. `allowedContentTypes` would need UTType's bindings
    // (objc2-uniform-type-identifiers) for this one call. The extension list does the same.
    #[expect(
        deprecated,
        reason = "the extension list needs no UTType bindings and still works"
    )]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
        "pdf",
    )])));
    let chosen = Cell::new(Some(chosen));
    let answered = panel.clone();
    let handler = RcBlock::new(move |response| {
        let path = (response == NSModalResponseOK)
            .then(|| answered.URL())
            .flatten()
            .and_then(|url| url.to_file_path());
        if let (Some(path), Some(chosen)) = (path, chosen.take()) {
            chosen(path);
        }
    });
    match NSApplication::sharedApplication(mtm).mainWindow() {
        Some(window) => panel.beginSheetModalForWindow_completionHandler(&window, &handler),
        None => handler.call((panel.runModal(),)),
    }
}

/// Lays `printout` out in a new print view, for the paper the panel starts on (or Letter for a
/// PDF), and prints it to `destination` once everything it shows has loaded. `done` hears
/// whether it printed.
pub(super) fn print(
    printout: Printout,
    loader: Loader,
    destination: Destination,
    done: Option<Done>,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("printing happens on the main thread only");
        if let Some(done) = done {
            done(false);
        }
        return;
    };
    let id = next_id();
    if let Some(block_drafts) = printout.blocks_draft_remote_content() {
        start_print(mtm, id, printout, loader, destination, done, block_drafts);
        return;
    }
    let settings_loader = loader.clone();
    PENDING.with(|pending| {
        pending.borrow_mut().insert(
            id,
            PendingJob {
                printout,
                loader,
                destination,
                done,
            },
        );
    });
    settings_loader.run(
        |store| async move {
            match store.settings::<exo_store::Settings>().await {
                Ok(settings) => {
                    settings.remote_content_blocking == Some(exo_store::RemoteContentBlocking::All)
                }
                Err(error) => {
                    log::warn!("printing: reading the images setting: {error}");
                    // Fail closed so draft remote content isn't fetched when settings can't be read.
                    true
                }
            }
        },
        move |block_drafts| {
            dispatch2::DispatchQueue::main().exec_async(move || {
                let Some(job) = PENDING.with(|pending| pending.borrow_mut().remove(&id)) else {
                    return;
                };
                let Some(mtm) = MainThreadMarker::new() else {
                    if let Some(done) = job.done {
                        done(false);
                    }
                    return;
                };
                start_print(
                    mtm,
                    id,
                    job.printout,
                    job.loader,
                    job.destination,
                    job.done,
                    block_drafts,
                );
            });
        },
    );
}

fn start_print(
    mtm: MainThreadMarker,
    id: u64,
    printout: Printout,
    loader: Loader,
    destination: Destination,
    done: Option<Done>,
    block_drafts: bool,
) {
    let page = match destination {
        Destination::Panel => default_page(),
        Destination::Pdf(_) => Page::LETTER,
    };
    let conversation = printout.on(page);
    let rendered = document::html(&conversation, block_drafts);
    let inline = crate::body::fetch::by_content_id(conversation.inline.clone());
    let handler = Handler::new(
        mtm,
        Loads {
            loader,
            inline,
            allow_remote: rendered.allow_remote,
        },
    );
    let printer = Printer::new(
        mtm,
        Printing {
            id,
            destination,
            title: conversation.title,
            page: Cell::new(page),
            printout,
            block_drafts,
            operation: RefCell::new(None),
            done: Cell::new(done),
        },
    );

    // SAFETY: a configuration built here, its handler registered for schemes WebKit doesn't
    // handle itself, and a script that only moves the page's own templates.
    let web_view = unsafe {
        let configuration = WKWebViewConfiguration::new(mtm);
        configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
        configuration
            .defaultWebpagePreferences()
            .setAllowsContentJavaScript(false);
        for scheme in SCHEMES {
            configuration.setURLSchemeHandler_forURLScheme(
                Some(ProtocolObject::from_ref(&*handler)),
                &NSString::from_str(scheme),
            );
        }
        let scripts = WKUserContentController::new(mtm);
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
            WKUserScript::alloc(mtm),
            &NSString::from_str(SHADOW_ROOTS),
            WKUserScriptInjectionTime::AtDocumentEnd,
            true,
            &WKContentWorld::defaultClientWorld(mtm),
        );
        scripts.addUserScript(&script);
        configuration.setUserContentController(&scripts);
        // As wide as the page, which is the width a body's media queries see (WebKit's media
        // queries see the view's).
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(conversation.page.width()), 1000.0),
        );
        WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &configuration)
    };
    // A window of its own that's never shown, so WebKit lays the view out.
    // SAFETY: a borderless window holding the view. `releasedWhenClosed` is off, as Rust owns it.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            web_view.frame(),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.setContentView(Some(&web_view));
    // SAFETY: the delegate is kept alive in `JOBS` for as long as the view is.
    unsafe { web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*printer))) };
    load(&web_view, &rendered.html);
    JOBS.with(|jobs| {
        jobs.borrow_mut().push(Job {
            id,
            web_view,
            _window: window,
            printer,
            _handler: handler,
        })
    });
}

/// Loads `html` into `web_view`, its relative URLs resolving on the remote scheme.
fn load(web_view: &WKWebView, html: &str) {
    // SAFETY: the page is Exo's own, and its loads go through the job's handler.
    unsafe {
        let base = NSURL::URLWithString(&NSString::from_str(document::BASE_URL));
        web_view.loadHTMLString_baseURL(&NSString::from_str(html), base.as_deref());
    }
}

/// While job `id` is under way, checks its paper every [`PAPER_CHECK`].
fn watch_paper(id: u64) {
    let Ok(when) = DispatchTime::try_from(PAPER_CHECK) else {
        return;
    };
    let _ = dispatch2::DispatchQueue::main().after(when, move || {
        let job = JOBS.with(|jobs| {
            jobs.borrow()
                .iter()
                .find(|job| job.id == id)
                .map(|job| (job.web_view.clone(), job.printer.clone()))
        });
        if let Some((web_view, printer)) = job {
            printer.check_paper(&web_view);
            watch_paper(id);
        }
    });
}

/// Has the print panel paginate the page again and redraw its preview, as WebKit's print view
/// does once it has new pages (AppKit's notification for changed print settings, which it
/// doesn't declare).
fn repaginate() {
    // SAFETY: a notification with no object, on the main thread.
    unsafe {
        NSNotificationCenter::defaultCenter()
            .postNotificationName_object(&NSString::from_str("NSPrintInfoDidChange"), None);
    }
}

/// Ends job `id`, after the callback that finished it has returned.
fn finish_job(id: u64) {
    dispatch2::DispatchQueue::main()
        .exec_async(move || JOBS.with(|jobs| jobs.borrow_mut().retain(|job| job.id != id)));
}

/// What a print job does once its page has loaded.
struct Printing {
    id: u64,
    destination: Destination,
    title: String,
    /// The paper the page is laid out for.
    page: Cell<Page>,
    /// The conversation, to lay out again for new paper.
    printout: Printout,
    /// Whether open drafts block remote content for this job.
    block_drafts: bool,
    /// The operation, once it runs.
    operation: RefCell<Option<Retained<NSPrintOperation>>>,
    done: Cell<Option<Done>>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Printer` doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ExoPrinter"]
    #[ivars = Printing]
    struct Printer;

    unsafe impl NSObjectProtocol for Printer {}

    unsafe impl WKNavigationDelegate for Printer {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let (printer, view) = (self.retain(), web_view.retain());
            let ready = RcBlock::new(move |_result: *mut AnyObject, error: *mut NSError| {
                if !error.is_null() {
                    log::warn!("printing: getting the page ready failed");
                }
                // The page laid out again for new paper goes to the operation under way.
                if printer.ivars().operation.borrow().is_some() {
                    repaginate();
                } else {
                    printer.run(&view);
                }
            });
            // SAFETY: the script only awaits the page's fonts, lays out and hides the images that
            // didn't load and marks the short bodies, in the client world.
            unsafe {
                web_view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                    &NSString::from_str(BEFORE_PRINT),
                    None,
                    None,
                    &WKContentWorld::defaultClientWorld(self.mtm()),
                    Some(&ready),
                );
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_terminated(&self, _web_view: &WKWebView) {
            log::warn!("printing: WebKit's content process quit");
            self.end(false);
        }
    }

    impl Printer {
        /// `NSPrintOperation`'s `didRunSelector`.
        #[unsafe(method(printOperationDidRun:success:contextInfo:))]
        fn did_run(&self, _operation: &NSPrintOperation, success: bool, _context: *mut c_void) {
            self.end(success);
        }
    }
);

impl Printer {
    fn new(mtm: MainThreadMarker, printing: Printing) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(printing);
        // SAFETY: `NSObject`'s designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Prints the loaded `web_view`, through the print panel as a sheet on Exo's window or straight
    /// into the PDF file.
    fn run(&self, web_view: &WKWebView) {
        let mtm = self.mtm();
        let printing = self.ivars();
        let info = NSPrintInfo::sharedPrintInfo().copy();
        let [top, side] = Page::MARGINS.map(f64::from);
        info.setTopMargin(top);
        info.setBottomMargin(top);
        info.setLeftMargin(side);
        info.setRightMargin(side);
        info.setHorizontallyCentered(false);
        info.setVerticallyCentered(false);
        info.setOrientation(NSPaperOrientation::Portrait);
        let window = match &printing.destination {
            Destination::Panel => NSApplication::sharedApplication(mtm).mainWindow(),
            Destination::Pdf(path) => {
                let [width, height] = printing.page.get().paper.map(f64::from);
                info.setPaperSize(NSSize::new(width, height));
                let Some(url) = NSURL::from_file_path(path) else {
                    log::warn!("printing: can't write to {}", path.display());
                    return self.end(false);
                };
                // SAFETY: `NSPrintSaveJob` with the URL to save to, as `NSPrintJobSavingURL` takes.
                unsafe {
                    info.setJobDisposition(NSPrintSaveJob);
                    info.dictionary()
                        .setObject_forKey(&url, ProtocolObject::from_ref(NSPrintJobSavingURL));
                }
                web_view.window()
            }
        };
        let Some(window) = window else {
            log::warn!("printing: no window to print from");
            return self.end(false);
        };
        // SAFETY: the operation prints `web_view`, which outlives it (it's held in `JOBS`).
        let operation = unsafe { web_view.printOperationWithPrintInfo(&info) };
        printing.operation.replace(Some(operation.clone()));
        let panel = printing.destination == Destination::Panel;
        operation.setShowsPrintPanel(panel);
        operation.setShowsProgressPanel(panel);
        operation.setJobTitle(Some(&NSString::from_str(&printing.title)));
        // WebKit's printing view starts without a size, and prints nothing until it has one.
        if let Some(view) = operation.view() {
            view.setFrame(web_view.bounds());
        }
        // SAFETY: the delegate answers `printOperationDidRun:success:contextInfo:` and lives in
        // `JOBS` until it has. No context is passed.
        unsafe {
            operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                &window,
                Some(self),
                Some(sel!(printOperationDidRun:success:contextInfo:)),
                std::ptr::null_mut(),
            );
        }
        if panel {
            watch_paper(printing.id);
        }
    }

    /// Lays the page out again when the panel's paper has changed, which the operation takes up
    /// once it has loaded ([`repaginate`]).
    fn check_paper(&self, web_view: &WKWebView) {
        let printing = self.ivars();
        let Some(paper) = printing
            .operation
            .borrow()
            .as_ref()
            .map(|operation| operation.printInfo().paperSize())
        else {
            return;
        };
        let page = page(paper);
        if page != printing.page.get() {
            printing.page.set(page);
            load(
                web_view,
                &document::html(&printing.printout.on(page), printing.block_drafts).html,
            );
        }
    }

    /// The page didn't load. Before the operation runs that ends the job; after, it's a page laid
    /// out again that a newer one stopped, or that failed, and the panel keeps the last one.
    fn failed(&self, error: &NSError) {
        if self.ivars().operation.borrow().is_some() {
            log::info!(
                "printing: a page for new paper didn't load: {}",
                error.localizedDescription()
            );
        } else {
            log::warn!(
                "printing: the page didn't load: {}",
                error.localizedDescription()
            );
            self.end(false);
        }
    }

    /// Reports how the job went (once) and lets it go.
    fn end(&self, printed: bool) {
        self.ivars().operation.take();
        if let Some(done) = self.ivars().done.take() {
            done(printed);
        }
        finish_job(self.ivars().id);
    }
}

/// What the page's loads go through.
struct Loads {
    loader: Loader,
    /// The thread's inline attachments, by lower-cased `Content-ID`.
    inline: HashMap<String, Attachment>,
    /// Whether this print job permits `exo-http:` / `exo-https:` fetches at all.
    allow_remote: bool,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Handler` doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ExoPrintResources"]
    #[ivars = Loads]
    struct Handler;

    unsafe impl NSObjectProtocol for Handler {}

    unsafe impl WKURLSchemeHandler for Handler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, _web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            // SAFETY: a task's request and its URL are there for as long as the task.
            let url = unsafe { task.request().URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string())
                .unwrap_or_default();
            let id = next_id();
            TASKS.with(|tasks| tasks.borrow_mut().insert(id, task.retain()));
            let loads = self.ivars();
            match super::resource(&url, &loads.inline, loads.allow_remote) {
                Some(Resource::Font(bytes)) => {
                    respond(id, Some((bytes.to_vec(), "font/ttf".to_owned())))
                }
                Some(Resource::Remote(url)) => loads.loader.url(&url, move |bytes| {
                    on_main(
                        id,
                        bytes.map(|bytes| {
                            let mime = super::sniff(&bytes).to_owned();
                            (bytes, mime)
                        }),
                    )
                }),
                Some(Resource::Inline(attachment)) => {
                    let mime = attachment.mime_type.clone();
                    loads.loader.attachment(attachment, move |bytes| {
                        on_main(id, bytes.ok().map(|bytes| (bytes, mime)))
                    });
                }
                None => respond(id, None),
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self, _web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            // After this, WebKit mustn't hear about the task again.
            TASKS.with(|tasks| {
                tasks
                    .borrow_mut()
                    .retain(|_, kept| !std::ptr::eq(&**kept, task))
            });
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker, loads: Loads) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(loads);
        // SAFETY: `NSObject`'s designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Answers task `id` on the main thread, from wherever its load finished.
fn on_main(id: u64, answer: Option<(Vec<u8>, String)>) {
    dispatch2::DispatchQueue::main().exec_async(move || respond(id, answer));
}

/// Answers task `id` with `answer`'s bytes and type, or fails it, unless WebKit stopped it.
fn respond(id: u64, answer: Option<(Vec<u8>, String)>) {
    let Some(task) = TASKS.with(|tasks| tasks.borrow_mut().remove(&id)) else {
        return;
    };
    // SAFETY: the task hasn't been stopped (it was still in `TASKS`), and it's answered once.
    unsafe {
        let Some((bytes, mime)) = answer else {
            task.didFailWithError(&NSError::errorWithDomain_code_userInfo(
                &NSString::from_str("Exo"),
                0,
                None,
            ));
            return;
        };
        let request = task.request();
        let Some(url) = request.URL() else {
            return task.didFailWithError(&NSError::errorWithDomain_code_userInfo(
                &NSString::from_str("Exo"),
                0,
                None,
            ));
        };
        let response = NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(
            NSURLResponse::alloc(),
            &url,
            Some(&NSString::from_str(&mime)),
            bytes.len() as isize,
            None,
        );
        task.didReceiveResponse(&response);
        task.didReceiveData(&NSData::with_bytes(&bytes));
        task.didFinish();
    }
}
