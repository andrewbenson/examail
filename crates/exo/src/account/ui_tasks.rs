use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use exo_store::{Attachment, Store, ThreadDetail, ThreadId};
use tokio::sync::mpsc;

use super::worker::Request;
use crate::recovery::Restarts;

/// How long a remote image may take before the body gives up on it.
pub(super) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// The most a remote image may weigh. Past it, the body gives up on the image rather than let a
/// sender fill memory (a 30 s download can be gigabytes).
const MAX_DOWNLOAD: usize = 25 << 20;

/// Asks the window for a frame when an account's data changes, while the account is on screen.
/// An account off screen draws nothing, so what it syncs, sends and fetches costs the window no
/// frames.
#[derive(Clone)]
pub(crate) struct Repaint {
    pub(super) ctx: egui::Context,
    pub(super) on_screen: Arc<AtomicBool>,
}

impl Repaint {
    /// A frame, if the account is on screen.
    pub(crate) fn request(&self) {
        if self.on_screen.load(Ordering::Relaxed) {
            self.ctx.request_repaint();
        }
    }
}

/// The request channel, counting what's in flight so tests can wait for quiet.
#[derive(Clone)]
pub(super) struct Requests {
    pub(super) sender: mpsc::UnboundedSender<Request>,
    pub(super) busy: Arc<AtomicUsize>,
}

impl Requests {
    pub(super) fn post(&self, request: Request) {
        self.busy.fetch_add(1, Ordering::SeqCst);
        if self.sender.send(request).is_err() {
            self.handled();
        }
    }

    /// The worker is done with a posted request.
    pub(super) fn handled(&self) {
        self.busy.fetch_sub(1, Ordering::SeqCst);
    }

    /// Counts a task as busy until the guard drops.
    pub(super) fn busy(&self) -> Busy {
        self.busy.fetch_add(1, Ordering::SeqCst);
        Busy(self.busy.clone())
    }
}

pub(super) struct Busy(pub(super) Arc<AtomicUsize>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One completion queued from a background task to run on `&mut T` during a UI frame.
pub(crate) struct UiTask<T: ?Sized> {
    work: Box<dyn FnOnce(&mut T) + Send>,
    _busy: Busy,
}

impl<T: ?Sized> UiTask<T> {
    pub(crate) fn run(self, target: &mut T) {
        (self.work)(target);
    }
}

/// Clonable handle that background tasks use to post UI completions onto [`UiTasks`] and request a
/// repaint.
pub(crate) struct UiSender<T: ?Sized> {
    sender: std::sync::mpsc::Sender<UiTask<T>>,
    pending: Arc<AtomicUsize>,
    ctx: egui::Context,
}

impl<T: ?Sized> Clone for UiSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            pending: self.pending.clone(),
            ctx: self.ctx.clone(),
        }
    }
}

impl<T: ?Sized> UiSender<T> {
    pub(crate) fn send(&self, work: impl FnOnce(&mut T) + Send + 'static) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let task = UiTask {
            work: Box::new(work),
            _busy: Busy(self.pending.clone()),
        };
        if self.sender.send(task).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

/// Queue of UI-thread completions posted from background tasks, drained once per frame.
pub(crate) struct UiTasks<T: ?Sized> {
    sender: std::sync::mpsc::Sender<UiTask<T>>,
    receiver: std::sync::mpsc::Receiver<UiTask<T>>,
    pub(super) pending: Arc<AtomicUsize>,
}

impl<T: ?Sized> Default for UiTasks<T> {
    fn default() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            sender,
            receiver,
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl<T: ?Sized> UiTasks<T> {
    pub(crate) fn sender(&self, ctx: &egui::Context) -> UiSender<T> {
        UiSender {
            sender: self.sender.clone(),
            pending: self.pending.clone(),
            ctx: ctx.clone(),
        }
    }

    pub(crate) fn pop(&self) -> Option<UiTask<T>> {
        self.receiver.try_recv().ok()
    }

    pub(crate) fn drain(&self, target: &mut T) {
        while let Some(task) = self.pop() {
            task.run(target);
        }
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }
}

/// Wakes one of the account's loops. A wake counts as busy from the call, not from when the
/// loop gets round to it, so the account is never idle between asking for a sync or a flush and
/// its start.
#[derive(Clone)]
pub(super) struct Wake {
    sender: mpsc::UnboundedSender<Busy>,
    requests: Requests,
}

impl Wake {
    /// A wake, and the loop's end of it.
    pub(super) fn new(requests: &Requests) -> (Self, Wakes) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender,
                requests: requests.clone(),
            },
            Wakes(Arc::new(tokio::sync::Mutex::new(receiver))),
        )
    }

    pub(super) fn wake(&self) {
        // Fails only once the loop is gone, and the count goes with the message.
        let _ = self.sender.send(self.requests.busy());
    }
}

/// The woken loop's end of a [`Wake`], shared by the runs of a loop that restarts after a panic
/// (`recovery`).
#[derive(Clone)]
pub(super) struct Wakes(Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Busy>>>);

impl Wakes {
    /// Waits for a wake and takes every one made since, because one run of the loop answers them
    /// all. The run counts as busy until the returned guard drops. `None` once nothing can wake the
    /// loop.
    pub(super) async fn next(&mut self) -> Option<Busy> {
        let mut receiver = self.0.lock().await;
        let busy = receiver.recv().await?;
        while receiver.try_recv().is_ok() {}
        Some(busy)
    }
}

/// How much of a part `size` bytes long has arrived, in percent. The percentage is of the whole
/// answer when Gmail says how long it is, else of the base64 JSON it sends for that size (about
/// 1.33 × the size).
fn percent(transfer: gmail::Transfer, size: u64) -> f32 {
    let total = transfer.total.unwrap_or(4 * size.div_ceil(3) + 32).max(1);
    (100.0 * transfer.done as f32 / total as f32).min(100.0)
}

/// How long until `at`, or nothing when it's past.
pub(super) fn until_then(at: jiff::Timestamp) -> Duration {
    Duration::try_from(at.duration_since(jiff::Timestamp::now())).unwrap_or_default()
}

/// Loads bytes for the UI without blocking it: attachments through Gmail, and remote images
/// straight from the sender's server. Cheap to clone. What it has in flight keeps the account from
/// being [idle](super::Account::is_idle).
#[derive(Clone)]
pub struct Loader {
    pub(super) runtime: tokio::runtime::Handle,
    /// Stops what it started with the account's loops.
    pub(super) restarts: Arc<Restarts>,
    pub(super) store: Store,
    pub(super) http: crate::remote_content::Client,
    pub(super) requests: Requests,
}

impl Loader {
    /// Fetches `attachment`'s bytes, retrying the way sync does, and hands them (or why they
    /// couldn't be had) to `done`.
    pub fn attachment(
        &self,
        attachment: &Attachment,
        done: impl FnOnce(Result<Vec<u8>, exo_store::Error>) + Send + 'static,
    ) {
        self.fetch(attachment, None, false, done);
    }

    /// [`attachment`](Self::attachment) for an image a body shows (`cid:`), which waits out a
    /// rate-limit pause however long it lasts and then tries again: nobody would see it fail, and
    /// the body never asks for it again. While it waits, the account isn't busy.
    pub fn inline_image(
        &self,
        attachment: &Attachment,
        done: impl FnOnce(Result<Vec<u8>, exo_store::Error>) + Send + 'static,
    ) {
        self.fetch(attachment, None, true, done);
    }

    /// [`attachment`](Self::attachment), telling `progress` the percentage of Gmail's answer
    /// that has arrived as it comes. A kept part reports nothing.
    pub fn download(
        &self,
        attachment: &Attachment,
        progress: impl Fn(f32) + Send + Sync + 'static,
        done: impl FnOnce(Result<Vec<u8>, exo_store::Error>) + Send + 'static,
    ) {
        let size = u64::from(attachment.size);
        self.fetch(
            attachment,
            Some(Arc::new(move |transfer| progress(percent(transfer, size)))),
            false,
            done,
        );
    }

    /// Fetches `attachment`, and with `patient`, again whenever a rate-limit pause made it fail.
    fn fetch(
        &self,
        attachment: &Attachment,
        progress: Option<gmail::Progress>,
        patient: bool,
        done: impl FnOnce(Result<Vec<u8>, exo_store::Error>) + Send + 'static,
    ) {
        let (store, attachment, requests) = (
            self.store.clone(),
            attachment.clone(),
            self.requests.clone(),
        );
        let mut busy = requests.busy();
        self.restarts.spawn_task(&self.runtime, async move {
            let bytes = loop {
                let bytes = store
                    .download_part(
                        &attachment.message_id,
                        attachment.attachment_id.as_deref(),
                        &attachment.part_id,
                        progress.clone(),
                    )
                    .await;
                match bytes {
                    Err(exo_store::Error::Throttled { until }) if patient => {
                        drop(busy);
                        tokio::time::sleep(until_then(until)).await;
                        busy = requests.busy();
                    }
                    bytes => break bytes,
                }
            };
            if let Err(error) = &bytes {
                log::warn!(
                    "loading an attachment of {}: {error}",
                    attachment.message_id
                );
            }
            done(bytes);
            drop(busy);
        });
    }

    /// Part `part` of message `message` (an attachment, an inline image) if the account keeps it,
    /// read now without asking Gmail. This lets a draft's or a message's inline image draw at once,
    /// and offline. [`attachment`](Self::attachment) reads the same cache first.
    pub fn cached_part(&self, message: &str, part: &str) -> Option<Vec<u8>> {
        self.store.cached_part(message, part)
    }

    /// Runs `task` on the account's store off the UI thread and hands its result to `done`.
    pub(crate) fn run<T: Send + 'static, F: std::future::Future<Output = T> + Send + 'static>(
        &self,
        task: impl FnOnce(Store) -> F + Send + 'static,
        done: impl FnOnce(T) + Send + 'static,
    ) {
        let (store, busy) = (self.store.clone(), self.requests.busy());
        self.restarts.spawn_task(&self.runtime, async move {
            done(task(store).await);
            drop(busy);
        });
    }

    /// Ensures conversation `id` has its message bodies fetched into the store when reachable,
    /// then reads its [`ThreadDetail`] off the UI thread.
    pub(crate) fn read_thread_with_bodies(
        &self,
        id: String,
        purpose: &'static str,
        done: impl FnOnce(String, Option<Arc<ThreadDetail>>) + Send + 'static,
    ) {
        self.run(
            move |store| async move {
                let thread = ThreadId::new(id.clone());
                if let Err(error) = store.load_bodies(&thread).await {
                    log::info!("fetching the bodies of {id} {purpose}: {error}");
                }
                let detail = store
                    .thread(&thread)
                    .await
                    .inspect_err(|error| log::error!("reading thread {id}: {error}"))
                    .ok()
                    .flatten();
                (id, detail.map(Arc::new))
            },
            move |(id, detail)| done(id, detail),
        );
    }

    /// Fetches `url` (http or https) from the public internet (`crate::remote_content`) and hands
    /// the body to `done` (`None` on any failure, or past [`MAX_DOWNLOAD`]).
    pub fn url(&self, url: &str, done: impl FnOnce(Option<Vec<u8>>) + Send + 'static) {
        let (loader, url, busy) = (self.clone(), url.to_owned(), self.requests.busy());
        self.restarts.spawn_task(&self.runtime, async move {
            done(loader.get(&url).await);
            drop(busy);
        });
    }

    /// [`url`](Self::url)'s fetch, awaited where it's asked for.
    pub(crate) async fn get(&self, url: &str) -> Option<Vec<u8>> {
        match self.http.bytes(url, MAX_DOWNLOAD).await {
            Ok(Some(bytes)) => Some(bytes),
            Ok(None) => {
                log::debug!("loading remote content: over {MAX_DOWNLOAD} bytes");
                None
            }
            Err(error) => {
                log::debug!("loading remote content: {error}");
                None
            }
        }
    }
}
