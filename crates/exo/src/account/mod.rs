//! One Gmail account's mail, as the UI sees it.
//!
//! The UI is immediate-mode and synchronous, while the store is async. An [`Account`] runs the
//! store, its sync and outbox loops and every Gmail call on a tokio runtime of its own. The UI
//! sends it requests (show a list, apply a mutation, undo) that return at once, and each frame
//! reads the latest [`Snapshot`] of the list on screen without waiting. Each new snapshot asks egui
//! to repaint. Nothing in `ui()` blocks on the network or the database.
//!
//! Each page that shows the account (a tab's) has its own [`Account`]. That's a handle on the
//! account the pages share, with a view of its own in the account's worker (its list, conversation,
//! search, filter and contact) and snapshots of its own ([`Account::view`]). The account closes
//! when its last handle goes.

mod publish;
mod sends;
mod ui_tasks;
mod worker;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use exo_store::{
    Connection, DeviceSettings, Draft, DraftFailure, Filter, Label, ListId, Mutation, SentDraft,
    Settings, Store, ThreadDetail, ThreadId,
};
use gmail::TokenProvider;
pub use publish::{SearchResults, Snapshot, snippet_tokens};
pub(crate) use sends::QuitFlush;
use sends::{STOP_GRACE, drain_before_reload, flush_loop};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch},
};
pub use ui_tasks::Loader;
use ui_tasks::{DOWNLOAD_TIMEOUT, Requests, Wake};
pub(crate) use ui_tasks::{Repaint, UiSender, UiTasks};
pub(crate) use worker::Health;
use worker::{
    Pace, Request, ViewId, ViewRequest, Worker, connection_loop, evict_loop, prerender_loop,
    reminder_loop, sync_loop,
};

use crate::{
    notifications::Notifier,
    power::{Power, PowerSource},
    recovery::{self, Restarts},
};

/// Where the Gmail parts of `email` are kept: `~/Library/Caches/Exo/<account>/parts`. macOS may
/// empty this cache, and signing out and Clear Cache delete it.
pub(crate) fn parts_cache(email: &str) -> Option<PathBuf> {
    std::env::home_dir().map(|home| {
        home.join("Library/Caches/Examail")
            .join(email)
            .join("parts")
    })
}

/// Where the photos of `email`'s contacts are kept: `~/Library/Caches/Exo/<account>/photos`,
/// which signing out and Clear Cache delete with the parts.
pub(crate) fn photos_cache(email: &str) -> Option<PathBuf> {
    std::env::home_dir().map(|home| {
        home.join("Library/Caches/Examail")
            .join(email)
            .join("photos")
    })
}

/// Who an account is and how to reach its mail. Sign-in builds these from the account's grant, and
/// the tests build them for a mock server.
#[derive(Clone)]
pub struct Credentials {
    /// The account's address, which also names its store.
    pub email: String,
    /// Where Gmail is.
    pub endpoints: gmail::Endpoints,
    /// Access tokens for it.
    pub tokens: Arc<dyn TokenProvider>,
}

/// How an account signs its calendar accounts in: Exo's sign-in, which keeps their grants, and
/// the browser it shows Google's consent in.
#[derive(Clone)]
pub struct CalendarAuth {
    pub auth: exo_auth::Auth,
    pub browser: crate::sign_in::Browser,
}

/// A calendar sign-in in flight. Dropping it abandons it, closing its loopback listener.
struct CalendarSignIn {
    task: tokio::task::JoinHandle<()>,
    outcome: tokio::sync::oneshot::Receiver<Result<String, exo_auth::Error>>,
}

impl Drop for CalendarSignIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Whether the window shows the account and is in front, which sets how often mail syncs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// On screen, the window focused.
    Focused,
    /// On screen, the window visible but not focused.
    Visible,
    /// On screen, the window minimized or hidden.
    Background,
    /// Another account is on screen, and no tab shows this one.
    OffScreen,
}

/// Why an account couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Its runtime couldn't start.
    #[error("starting the account's runtime: {0}")]
    Runtime(#[from] std::io::Error),
    /// Its store couldn't be opened.
    #[error(transparent)]
    Store(#[from] exo_store::Error),
}

/// Names the undo of one [`Account::apply`] (a toast's Undo).
pub type UndoToken = u64;

/// Something the UI should tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Examail Cloud minted an MCP token (`Some((token, url))`), or revoked them (`None`), or
    /// couldn't (why).
    McpToken(Result<Option<(String, String)>, String>),
    /// The stored settings changed from `from` to `to` other than through the UI (Examail Cloud
    /// brought another Mac's changes). [`Account::events`] applies it to the UI's copy itself.
    SettingsChanged {
        from: serde_json::Value,
        to: serde_json::Value,
    },
    /// A mutation (or its undo) couldn't be applied or sent, and its effect is gone.
    Failed {
        /// What the user did.
        mutation: Mutation,
        /// Whether it was the undo that failed.
        undo: bool,
    },
    /// Gmail has a draft's latest version.
    DraftSaved(String),
    /// A held draft went out.
    DraftSent(SentDraft),
    /// Gmail refused to save a draft, or its send failed.
    DraftFailed(DraftFailure),
    /// A draft is waiting for the network.
    DraftsOffline,
    /// Gmail didn't take a held send this time, for a reason other than the network, and it's
    /// tried again later.
    SendRetrying(String),
    /// Drafts, by id, that an earlier run saved without some of their files. Saving each again now
    /// that Gmail answers fetches the files, and writes the draft to Gmail once it has them all.
    Incomplete(Vec<String>),
    /// Some of a draft's attachments couldn't be read or fetched, so its save left them off.
    AttachmentsFailed {
        /// The draft.
        id: String,
        /// How many.
        count: usize,
    },
    /// An undo send came back, saying whether the draft was still unsent (held, or failed
    /// meanwhile).
    SendTakenBack {
        /// The draft.
        id: String,
        /// It was still held.
        held: bool,
    },
    /// [`Account::create_label`] created it.
    LabelCreated(Label),
    /// [`Account::create_label`] couldn't.
    LabelNotCreated {
        /// Because Gmail was out of reach.
        offline: bool,
    },
    /// What Gmail doesn't have yet, as [`Account::ask_unsent`] asked for.
    Unsent(exo_store::Unsent),
    /// A conversation opened that the store didn't have couldn't be fetched: Gmail can't be reached
    /// (`offline`), or doesn't have it.
    Unavailable {
        /// The conversation.
        id: String,
        /// Whether Gmail couldn't be reached.
        offline: bool,
    },
    /// The thread [`Account::read_thread`] read (its detail is `None` when it isn't stored).
    Thread {
        /// The thread asked for.
        id: String,
        /// The thread, with every message.
        detail: Option<Arc<ThreadDetail>>,
    },
}

/// One account as one page sees it. It's a handle on the account (its store and the tasks that keep
/// it in sync, seen from the UI thread) with a view of its own, whose snapshots carry the page's
/// list, conversation, search, filter and contact. Another page of the account has another handle
/// ([`Account::view`]).
pub struct Account {
    shared: Rc<Shared>,
    view: ViewId,
    snapshot: watch::Receiver<Arc<Snapshot>>,
    /// The account's events this handle hasn't read yet ([`Account::events`]).
    events: Rc<RefCell<Vec<Event>>>,
    /// The account's settings, as this handle last saw them.
    settings: Versioned,
}

/// The account's settings as last changed, numbered, so a handle can tell when another page
/// changed them since it last looked.
#[derive(Clone)]
struct Versioned {
    version: u64,
    settings: Arc<Settings>,
    device: Arc<DeviceSettings>,
}

/// The account's events, read by each of its handles ([`Account::events`]).
struct Events {
    receiver: std::sync::mpsc::Receiver<Event>,
    /// Each handle's events not read yet.
    readers: Vec<std::rc::Weak<RefCell<Vec<Event>>>>,
}

/// The calendar accounts signed in besides the mail account's own.
#[derive(Default)]
struct CalendarAccounts {
    /// Signs them in, once the app says ([`Account::sign_in_calendars_with`]).
    auth: Option<CalendarAuth>,
    /// The accounts read, with their tokens, which say when Google refuses a grant.
    tokens: Vec<(String, exo_auth::Tokens)>,
    signing_in: Option<CalendarSignIn>,
}

/// The account itself, which every handle on it shares.
struct Shared {
    email: String,
    /// The folder that holds the bytes of draft attachments.
    files: PathBuf,
    /// Wakes the task that writes to Gmail.
    flush: Wake,
    /// Wakes the task that syncs.
    sync_now: Wake,
    /// Wakes the task that checks the connection, to check it now.
    network: Wake,
    runtime: Arc<Runtime>,
    /// Set once [`Account::discard`] or [`Account::take_quit_flush`] has torn the account down.
    closed: AtomicBool,
    /// Set during `View ▸ Reload` so closing the sole handle keeps Undo-Send and Calendar holds.
    reload_close: Cell<bool>,
    requests: Requests,
    /// The store's revision.
    changes: watch::Receiver<u64>,
    /// The store revision the latest snapshot read.
    published: Arc<AtomicU64>,
    presence: watch::Sender<Presence>,
    /// The Mac's power source, as [`Account::follow_power`] says.
    power: watch::Sender<Power>,
    events: RefCell<Events>,
    loader: Loader,
    /// Sends the account's events from the UI's own background work ([`Account::mint_mcp_token`]).
    event_sender: std::sync::mpsc::Sender<Event>,
    /// How to reach Examail Cloud, once [`Account::start_cloud`] has.
    cloud: RefCell<Option<crate::cloud::Keys>>,
    /// The UI's copy of the settings. Changes apply here at once and are saved in the background.
    settings: RefCell<Versioned>,
    /// How many times [`Account::apply`] was called.
    applies: AtomicU64,
    /// The calendar's data.
    calendar: crate::calendar::CalendarSync,
    /// Bulk actions, queued and running.
    bulk: crate::bulk::Jobs,
    /// Where the account's notifications go, once the app says ([`Account::set_notifier`]).
    notifier: watch::Sender<Option<Notifier>>,
    /// Where one-click unsubscribes may post.
    reach: crate::unsubscribe::Reach,
    /// What time it is for the reminder scheduler, which is the app's clock once the app has set
    /// one.
    clock: Arc<std::sync::RwLock<crate::Clock>>,
    /// Wakes the reminder scheduler, to look at the clock now.
    reminders: Wake,
    /// Asks for frames while the account is on screen.
    repaint: Repaint,
    /// The account's loops and worker, stopped before its runtime is (`recovery`).
    restarts: Arc<Restarts>,
    /// Whether its store still works.
    health: Arc<Health>,
    /// How far history is applied.
    progress: watch::Receiver<exo_store::SyncProgress>,
    /// How drafts' files stand with Gmail.
    uploads: watch::Receiver<exo_store::Uploads>,
    calendar_accounts: RefCell<CalendarAccounts>,
    /// The number the next view gets.
    next_view: Cell<ViewId>,
    /// The number the next undo gets, which the worker and the bulk jobs know it by.
    next_undo: Cell<UndoToken>,
}

/// The default splits for the account `email` (`SplitInbox::defaults`). Team covers the account's
/// domain, unless that's a webmail one.
fn default_splits(email: &str) -> Vec<exo_store::SplitInbox> {
    let me = [email.to_owned()];
    exo_store::SplitInbox::defaults(
        crate::splits::team_domain(email).as_deref(),
        exo_store::Definitions {
            labels: &[],
            me: &me,
        },
    )
}

/// Builds a multi-thread Tokio runtime shared across all accounts, sign-in, and profile photos
/// within a single [`crate::App`] session (while keeping parallel test [`crate::App`] instances
/// isolated from each other's SQLite lock/I/O waits).
pub(crate) fn new_runtime() -> std::io::Result<Arc<Runtime>> {
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("exo-runtime")
            .enable_all()
            .build()?,
    ))
}

/// Process-wide Tokio runtime for app-wide background utilities (photo decoding and the updater).
pub(crate) fn shared_runtime() -> std::io::Result<Arc<Runtime>> {
    static SHARED_RUNTIME: std::sync::OnceLock<Arc<Runtime>> = std::sync::OnceLock::new();
    if let Some(runtime) = SHARED_RUNTIME.get() {
        return Ok(runtime.clone());
    }
    let built = new_runtime()?;
    let _ = SHARED_RUNTIME.set(built);
    Ok(SHARED_RUNTIME
        .get()
        .expect("shared runtime was just initialized")
        .clone())
}

/// Runs `future` to completion on `runtime`, using `block_in_place` if already inside a
/// multi-thread worker thread.
pub(crate) fn block_on_runtime<F: std::future::Future>(runtime: &Runtime, future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
        Err(_) => runtime.block_on(future),
    }
}

/// Subscribes to the single OS network reachability watcher shared by every open account.
fn shared_reachability() -> watch::Receiver<u64> {
    static REACHABILITY: std::sync::OnceLock<watch::Sender<u64>> = std::sync::OnceLock::new();
    REACHABILITY
        .get_or_init(|| {
            let (flips, _) = watch::channel(0);
            let sender = flips.clone();
            let watch = crate::reachability::Reachability::watch(move || {
                sender.send_modify(|count| *count += 1);
            });
            // Kept alive for the life of the process so SCNetworkReachability stays registered.
            std::mem::forget(watch);
            flips
        })
        .subscribe()
}

impl Account {
    /// Opens the store for `credentials` in `dir` and starts syncing it. `ctx` repaints on new data
    /// while the account is on screen ([`Account::set_on_screen`]), which it is to start with.
    ///
    /// # Errors
    /// When the store can't be opened, or the runtime can't start.
    pub fn open(
        dir: &Path,
        credentials: Credentials,
        ctx: egui::Context,
    ) -> Result<Self, OpenError> {
        Self::open_on(dir, credentials, ctx, new_runtime()?)
    }

    /// Opens the store on an existing Tokio `runtime` shared across an [`crate::App`] session.
    pub(crate) fn open_on(
        dir: &Path,
        credentials: Credentials,
        ctx: egui::Context,
        runtime: Arc<Runtime>,
    ) -> Result<Self, OpenError> {
        let Credentials {
            email,
            endpoints,
            tokens,
        } = credentials;
        let reach = crate::unsubscribe::Reach::of(&endpoints);
        // Remote content may come from this Mac only when Gmail does too, as mock-servers' images
        // do.
        let local_gmail = crate::remote_content::serves_gmail_locally(&endpoints);
        let store = block_on_runtime(&runtime, Store::open(dir, &email, endpoints, tokens))?;
        // On mock-servers, the parts cache sits beside the store, so a test's or a screenshot's cache
        // is its own.
        let parts = if local_gmail {
            Some(dir.join(format!("{email}.parts")))
        } else {
            parts_cache(&email)
        };
        if let Some(parts) = parts {
            store.cache_parts(exo_store::PartsCache::new(parts));
        }
        // The search index holds what the thread view shows above the `…`.
        store.index_text_with(Arc::new(crate::body::visible_texts));
        // Read before the first frame, so it already has the account's theme.
        let (mut settings, device_settings): (Settings, DeviceSettings) =
            block_on_runtime(&runtime, async {
                Ok::<_, exo_store::Error>((store.settings().await?, store.settings().await?))
            })?;
        if settings.set_up_splits(|| default_splits(&email)) {
            block_on_runtime(&runtime, store.save_settings(&settings))?;
        }

        let (sender, receiver) = mpsc::unbounded_channel();
        let requests = Requests {
            sender,
            busy: Arc::new(AtomicUsize::new(0)),
        };
        let (snapshot_sender, snapshot) = watch::channel(Arc::new(Snapshot {
            list: ListId::Inbox,
            filter: None,
            threads: Vec::new(),
            count: None,
            inbox: None,
            counts: crate::splits::Counts::new(),
            muted: false,
            more: false,
            loading: true,
            complete: false,
            labels: Vec::new(),
            contact: None,
            contact_brand: None,
            thread: None,
            bodiless: Vec::new(),
            drafts: Vec::new(),
            aliases: Vec::new(),
            address_book: None,
            search: None,
            applied: 0,
            connection: Connection::default(),
            uploading: None,
        }));
        let (presence, presence_receiver) = watch::channel(Presence::Focused);
        let (power, power_receiver) = watch::channel(Power::Plugged);
        let (notifier, notifier_receiver) = watch::channel(None);
        let (event_sender, events) = std::sync::mpsc::channel();
        let (flush, flushes) = Wake::new(&requests);
        let (sync_now, syncs) = Wake::new(&requests);
        let (synced, first_sync) = watch::channel(false);
        let (reminders, reminder_wakes) = Wake::new(&requests);
        let clock = Arc::new(std::sync::RwLock::new(crate::Clock::system()));
        let (network, networks) = Wake::new(&requests);
        let changes = store.changes();
        let published = Arc::new(AtomicU64::new(*changes.borrow()));
        // The account's background, restarted after a panic (`recovery`).
        let restarts = Arc::new(Restarts::new(ctx.clone()));
        let repaint = Repaint {
            ctx,
            on_screen: Arc::new(AtomicBool::new(true)),
        };
        let health = Arc::new(Health::new(repaint.clone()));

        // On mock-servers, the photos sit beside the store, as the parts do.
        let photos = match photos_cache(&email) {
            Some(cache) if !local_gmail => cache,
            _ => dir.join(format!("{email}.photos")),
        };
        // Brand logos are looked up in DNS, but not for mock-servers' made-up domains.
        let brands = crate::brands::Brands::new(
            &repaint.ctx,
            &email,
            store.clone(),
            photos.clone(),
            !local_gmail,
        );
        let guard = runtime.enter();
        // The logos earlier runs validated show from the start, checked against their CAs'
        // revocation lists then and each day after.
        recovery::spawn(&restarts, {
            let brands = brands.clone();
            move || {
                let brands = brands.clone();
                async move { brands.watch().await }
            }
        });
        let files = store.draft_files().to_owned();
        let progress = store.sync_progress();
        let uploads = store.uploads();
        let pruning = store.clone();
        restarts.spawn_task(runtime.handle(), async move {
            if let Err(error) = pruning
                .prune_draft_files_older_than(std::time::Duration::from_secs(3600))
                .await
            {
                log::warn!("pruning draft files: {error}");
            }
        });
        sync_now.wake();
        // What an earlier run left unsent goes now: a send held when Exo crashed, or when
        // Google signed the account out.
        flush.wake();
        // The contact pane and the calendar show Google's people from the start.
        requests.post(Request::LoadGoogleContacts);
        // A loop that runs again after a panic starts woken, as at opening, because its timers went
        // with it.
        recovery::spawn(&restarts, {
            let (store, requests, presence, power, notify, sync_now, health) = (
                store.clone(),
                requests.clone(),
                presence_receiver.clone(),
                power_receiver,
                (email.clone(), notifier_receiver.clone(), brands.clone()),
                sync_now.clone(),
                health.clone(),
            );
            move || {
                sync_now.wake();
                let pace = Pace {
                    presence: presence.clone(),
                    power: power.clone(),
                };
                sync_loop(
                    store.clone(),
                    requests.clone(),
                    pace,
                    syncs.clone(),
                    notify.clone(),
                    synced.clone(),
                    health.clone(),
                )
            }
        });
        recovery::spawn(&restarts, {
            let (store, first_sync) = (store.clone(), first_sync.clone());
            move || evict_loop(store.clone(), first_sync.clone())
        });
        recovery::spawn(&restarts, {
            let (store, requests, first_sync) =
                (store.clone(), requests.clone(), first_sync.clone());
            move || prerender_loop(store.clone(), requests.clone(), first_sync.clone())
        });
        recovery::spawn(&restarts, {
            let (store, clock, flush, notify) = (
                store.clone(),
                clock.clone(),
                flush.clone(),
                (email.clone(), notifier_receiver.clone(), brands.clone()),
            );
            move || {
                reminder_loop(
                    store.clone(),
                    clock.clone(),
                    first_sync.clone(),
                    reminder_wakes.clone(),
                    flush.clone(),
                    notify.clone(),
                )
            }
        });
        recovery::spawn(&restarts, {
            let (store, email) = (store.clone(), email.clone());
            move || {
                crate::notifications::reminders::run(
                    store.clone(),
                    email.clone(),
                    notifier_receiver.clone(),
                )
            }
        });
        recovery::spawn(&restarts, {
            let (store, requests, sync_now, events, repaint, flush) = (
                store.clone(),
                requests.clone(),
                sync_now.clone(),
                event_sender.clone(),
                repaint.clone(),
                flush.clone(),
            );
            move || {
                flush.wake();
                flush_loop(
                    store.clone(),
                    requests.clone(),
                    flushes.clone(),
                    sync_now.clone(),
                    events.clone(),
                    repaint.clone(),
                    reach,
                )
            }
        });
        recovery::spawn(&restarts, {
            let (store, presence, sync_now, flush) = (
                store.clone(),
                presence_receiver.clone(),
                sync_now.clone(),
                flush.clone(),
            );
            move || {
                connection_loop(
                    store.clone(),
                    presence.clone(),
                    networks.clone(),
                    sync_now.clone(),
                    flush.clone(),
                )
            }
        });
        let mut reachability = shared_reachability();
        if *reachability.borrow_and_update() % 2 == 1 {
            network.wake();
        }
        restarts.spawn_task(runtime.handle(), {
            let network = network.clone();
            async move {
                while reachability.changed().await.is_ok() {
                    let _ = *reachability.borrow_and_update();
                    network.wake();
                }
            }
        });
        let loader = Loader {
            runtime: runtime.handle().clone(),
            restarts: restarts.clone(),
            store: store.clone(),
            http: crate::remote_content::Client::new(DOWNLOAD_TIMEOUT, local_gmail),
            requests: requests.clone(),
        };
        let calendar = crate::calendar::CalendarSync::start(
            store.clone(),
            loader.clone(),
            presence_receiver,
            repaint.clone(),
            &restarts,
        );
        // Before the first frame, so Bulk Actions lists them.
        let logged = block_on_runtime(&runtime, store.bulk_log()).unwrap_or_else(|error| {
            log::error!("reading the bulk actions' log: {error}");
            Vec::new()
        });
        let bulk = crate::bulk::Jobs::start(store.clone(), repaint.clone(), &restarts, logged);
        let worker = Worker {
            email: email.clone(),
            address_book: None,
            mail_contacts: None,
            google_contacts: None,
            listing_google: false,
            brands: brands.clone(),
            photos: crate::people::Photos::new(
                &repaint.ctx,
                &email,
                loader.clone(),
                photos,
                local_gmail,
            ),
            incomplete: HashMap::new(),
            fetching_files: false,
            settings: settings.clone(),
            changes: changes.clone(),
            published: published.clone(),
            connection: store.connection(),
            progress: store.sync_progress(),
            uploads: store.uploads(),
            store,
            repaint: repaint.clone(),
            requests: requests.clone(),
            events: event_sender.clone(),
            views: HashMap::from([(0, publish::ViewState::new(snapshot_sender))]),
            focused: 0,
            opening: HashSet::new(),
            prefetching: HashSet::new(),
            orphans: false,
            synced: false,
            resaved: false,
            backfilling: HashSet::new(),
            filled: HashSet::new(),
            undos: Vec::new(),
            sender_jobs: Vec::new(),
            applied: 0,
            flush: flush.clone(),
            searches: 0,
            restarts: restarts.clone(),
            health: health.clone(),
        };
        recovery::spawn_once(&restarts, worker.run(receiver));
        requests.post(Request::Refresh);
        drop(guard);

        let shared = Shared {
            email,
            files,
            flush,
            sync_now,
            network,
            runtime,
            closed: AtomicBool::new(false),
            reload_close: Cell::new(false),
            requests,
            changes,
            published,
            presence,
            power,
            events: RefCell::new(Events {
                receiver: events,
                readers: Vec::new(),
            }),
            event_sender,
            cloud: RefCell::new(None),
            loader,
            settings: RefCell::new(Versioned {
                version: 0,
                settings: Arc::new(settings),
                device: Arc::new(device_settings),
            }),
            applies: AtomicU64::new(0),
            calendar,
            bulk,
            notifier,
            reach,
            clock,
            reminders,
            repaint,
            restarts,
            health,
            progress,
            uploads,
            calendar_accounts: RefCell::default(),
            next_view: Cell::new(1),
            next_undo: Cell::new(0),
        };
        Ok(Self::handle(Rc::new(shared), 0, snapshot))
    }

    /// A handle on `shared` for view `view`, whose snapshots come on `snapshot`.
    fn handle(shared: Rc<Shared>, view: ViewId, snapshot: watch::Receiver<Arc<Snapshot>>) -> Self {
        let events = Rc::new(RefCell::new(Vec::new()));
        shared
            .events
            .borrow_mut()
            .readers
            .push(Rc::downgrade(&events));
        let settings = shared.settings.borrow().clone();
        Self {
            shared,
            view,
            snapshot,
            events,
            settings,
        }
    }

    /// Another handle on the account, for another page. Its view is its own, on the inbox with
    /// nothing open, and its first snapshot is this handle's latest without its list, conversation,
    /// search and contact.
    pub fn view(&self) -> Self {
        let view = self
            .shared
            .next_view
            .replace(self.shared.next_view.get() + 1);
        let first = Snapshot {
            list: ListId::Inbox,
            filter: None,
            threads: Vec::new(),
            count: None,
            more: false,
            loading: true,
            complete: false,
            contact: None,
            contact_brand: None,
            thread: None,
            bodiless: Vec::new(),
            search: None,
            ..Snapshot::clone(&self.snapshot())
        };
        let (sender, snapshot) = watch::channel(Arc::new(first));
        self.shared.requests.post(Request::AddView(view, sender));
        Self::handle(self.shared.clone(), view, snapshot)
    }

    /// Posts `request` for this handle's view.
    fn ask(&self, request: ViewRequest) {
        self.shared.requests.post(Request::View(self.view, request));
    }

    /// Takes up the settings another page of the account changed since this one last looked. A page
    /// does this as it starts a frame or comes on screen.
    pub fn catch_up(&mut self) {
        let latest = self.shared.settings.borrow();
        if latest.version != self.settings.version {
            self.settings = latest.clone();
        }
    }

    /// Makes this handle's settings the account's.
    fn share_settings(&mut self) {
        let mut latest = self.shared.settings.borrow_mut();
        self.settings.version = latest.version + 1;
        *latest = self.settings.clone();
    }

    /// A number for an undo ([`Account::apply`], [`Account::sender_job`], [`Account::bulk`]),
    /// unique to the account, so no other page's undo has it.
    pub fn undo_token(&self) -> UndoToken {
        self.shared
            .next_undo
            .replace(self.shared.next_undo.get() + 1)
    }

    /// Makes the account reachable through Examail's MCP server ([`crate::mcp`]) while it's open.
    pub fn start_mcp(&self) {
        let loader = &self.shared.loader;
        let flush = self.shared.flush.clone();
        let registration = crate::mcp::register(
            &self.shared.email,
            crate::mcp::Entry {
                store: loader.store.clone(),
                runtime: loader.runtime.clone(),
                flush: Arc::new(move || flush.wake()),
            },
        );
        // Held by a task that lives as long as the account's loops, so closing the account ends it.
        loader.restarts.spawn_task(&loader.runtime, async move {
            let _registration = registration;
            std::future::pending::<()>().await;
        });
    }

    /// Hands the account's reminders and scheduled sends to Examail Cloud, and keeps them
    /// there, while the account is open ([`crate::cloud`]).
    pub fn start_cloud(&self, mut access: crate::cloud::Access) {
        let loader = &self.shared.loader;
        *self.shared.cloud.borrow_mut() = Some(crate::cloud::Keys {
            config: access.config.clone(),
            email: access.email.clone(),
            tokens: access.tokens.clone(),
        });
        let requests = loader.requests.clone();
        access.merge_settings = Box::new(move |from, to| {
            let (done, applied) = tokio::sync::oneshot::channel();
            requests.post(Request::MergeSettings { from, to, done });
            applied
        });
        loader.restarts.spawn_task(
            &loader.runtime,
            crate::cloud::run(loader.store.clone(), access),
        );
    }

    /// Whether Examail Cloud is set up for the account (it may not be connected yet).
    pub fn has_cloud(&self) -> bool {
        self.shared.cloud.borrow().is_some()
    }

    /// Mints a token for Examail Cloud's MCP server, or revokes them all (`revoke`), in the
    /// background. [`Event::McpToken`] answers.
    pub fn mint_mcp_token(&self, revoke: bool) {
        let Some(keys) = self.shared.cloud.borrow().clone() else {
            return;
        };
        let events = self.shared.event_sender.clone();
        let ctx = self.shared.repaint.ctx.clone();
        self.shared.loader.runtime.spawn(async move {
            let answer = if revoke {
                crate::cloud::revoke_mcp_tokens(keys).await.map(|()| None)
            } else {
                crate::cloud::mint_mcp_token(keys).await.map(Some)
            };
            let _ = events.send(Event::McpToken(answer));
            ctx.request_repaint();
        });
    }

    /// Signs calendar accounts in with `auth` from now on, and reads the ones the settings list as
    /// signed in, with their grants. The first time, the grants come from the Keychain, which can
    /// block, as opening the mail's grant does. An account whose grant is gone is listed as signed
    /// out, and a grant the settings don't list any more is forgotten.
    pub fn sign_in_calendars_with(&mut self, auth: CalendarAuth) {
        let email = self.shared.email.clone();
        let held = auth
            .auth
            .accounts()
            .into_iter()
            .find(|account| account.email == email)
            .map(|account| account.calendars)
            .unwrap_or_default();
        for calendar in held
            .iter()
            .filter(|calendar| !self.settings().calendar_accounts.contains_key(*calendar))
        {
            if let Err(error) = auth.auth.forget_calendar(&email, calendar) {
                log::warn!("forgetting a calendar account's grant: {error}");
            }
        }
        let listed: Vec<String> = self
            .settings()
            .calendar_accounts
            .iter()
            .filter(|(calendar, account)| **calendar != email && account.authed)
            .map(|(calendar, _)| calendar.clone())
            .collect();
        let mut signed_out = Vec::new();
        let mut read = Vec::new();
        for calendar in listed {
            match auth.auth.calendar_tokens(&email, &calendar) {
                Ok(tokens) if !tokens.needs_sign_in() => {
                    self.shared
                        .calendar
                        .read_account(&calendar, tokens.clone(), false);
                    read.push((calendar, tokens));
                }
                Ok(_) => signed_out.push(calendar),
                Err(error) => {
                    log::warn!("reading a calendar account's grant: {error}");
                    signed_out.push(calendar);
                }
            }
        }
        self.update_settings(|settings| {
            for email in &signed_out {
                if let Some(account) = settings.calendar_accounts.get_mut(email) {
                    account.authed = false;
                }
            }
        });
        let mut calendars = self.calendar_accounts();
        calendars.tokens.extend(read);
        calendars.auth = Some(auth);
    }

    /// The calendar accounts, to change.
    fn calendar_accounts(&self) -> std::cell::RefMut<'_, CalendarAccounts> {
        self.shared.calendar_accounts.borrow_mut()
    }

    /// Starts signing a calendar account in, with `hint` naming the one asked for
    /// (by `Sign in again`, or an invite's). Abandons a sign-in in flight. Returns whether it
    /// started, which it can't in a session without Exo's sign-in.
    pub fn sign_in_calendar(&mut self, hint: Option<&str>) -> bool {
        let mut calendars = self.calendar_accounts();
        let Some(CalendarAuth { auth, browser }) = calendars.auth.clone() else {
            return false;
        };
        let (owner, hint, ctx) = (
            self.shared.email.clone(),
            hint.map(str::to_owned),
            self.shared.repaint.ctx.clone(),
        );
        let (sender, outcome) = tokio::sync::oneshot::channel();
        let task = self.shared.loader.runtime.spawn(async move {
            let _ = sender.send(
                auth.sign_in_calendar(&owner, hint.as_deref(), |url| browser(url))
                    .await,
            );
            ctx.request_repaint();
        });
        calendars.signing_in = Some(CalendarSignIn { task, outcome });
        true
    }

    /// The outcome of the calendar sign-in in flight, once it has one. A calendar account that
    /// signs in is listed (`authed`) and read, and its calendars start loading. Signing in as the
    /// mail account's own address changes nothing.
    pub fn poll_calendar_sign_in(&mut self) -> Option<Result<String, exo_auth::Error>> {
        let mut calendars = self.calendar_accounts();
        let outcome = calendars.signing_in.as_mut()?.outcome.try_recv().ok()?;
        calendars.signing_in = None;
        let auth = calendars.auth.clone();
        drop(calendars);
        if let Ok(email) = &outcome
            && *email != self.shared.email
            && let Some(auth) = auth
        {
            match auth.auth.calendar_tokens(&self.shared.email, email) {
                Ok(tokens) => {
                    let mut calendars = self.calendar_accounts();
                    calendars.tokens.retain(|(known, _)| known != email);
                    calendars.tokens.push((email.clone(), tokens.clone()));
                    drop(calendars);
                    self.shared.calendar.read_account(email, tokens, true);
                    self.update_settings(|settings| {
                        settings
                            .calendar_accounts
                            .entry(email.clone())
                            .or_insert_with(|| exo_store::CalendarAccount {
                                email: email.clone(),
                                ..exo_store::CalendarAccount::default()
                            })
                            .authed = true;
                    });
                }
                Err(error) => return Some(Err(error)),
            }
        }
        Some(outcome)
    }

    /// Lists the calendar accounts whose grant Google refused as signed out (`authed: false`),
    /// and stops reading them, until they sign in again.
    pub fn check_calendar_grants(&mut self) {
        let refused: Vec<String> = {
            let mut calendars = self.calendar_accounts();
            let refused = calendars
                .tokens
                .iter()
                .filter(|(_, tokens)| tokens.needs_sign_in())
                .map(|(email, _)| email.clone())
                .collect::<Vec<_>>();
            calendars
                .tokens
                .retain(|(email, _)| !refused.contains(email));
            refused
        };
        if refused.is_empty() {
            return;
        }
        for email in &refused {
            log::warn!("a calendar account was signed out of Google");
            self.shared.calendar.forget_account(email, false);
        }
        self.update_settings(|settings| {
            for email in &refused {
                if let Some(account) = settings.calendar_accounts.get_mut(email) {
                    account.authed = false;
                }
            }
        });
    }

    /// Removes calendar account `email`. Its grant is forgotten, it leaves the settings, and its
    /// calendars go, with the events only they had. The mail account's own can't be removed.
    pub fn remove_calendar_account(&mut self, email: &str) {
        if email == self.shared.email {
            return;
        }
        let mut calendars = self.calendar_accounts();
        if let Some(auth) = &calendars.auth
            && let Err(error) = auth.auth.forget_calendar(&self.shared.email, email)
        {
            log::warn!("forgetting a calendar account's grant: {error}");
        }
        calendars.tokens.retain(|(known, _)| known != email);
        drop(calendars);
        self.shared.calendar.forget_account(email, true);
        self.update_settings(|settings| {
            settings.calendar_accounts.remove(email);
        });
    }

    /// Posts the account's notifications with `notifier` from now on: new mail, calendar
    /// reminders. With none, it posts none.
    pub fn set_notifier(&self, notifier: Option<Notifier>) {
        self.shared.notifier.send_replace(notifier);
    }

    /// Paces the account's syncs off screen by `source` from now on (`sync_every`). Until then,
    /// the account counts as plugged in.
    pub fn follow_power(&self, source: &PowerSource) {
        let (own, mut power) = (self.shared.power.clone(), source.subscribe());
        self.shared
            .restarts
            .spawn_task(&self.shared.loader.runtime, async move {
                loop {
                    let now = *power.borrow_and_update();
                    own.send_if_modified(|current| std::mem::replace(current, now) != now);
                    if power.changed().await.is_err() {
                        return;
                    }
                }
            });
    }

    /// Has the reminder scheduler look at the clock now, for when the minute changed, maybe by more
    /// than the scheduler slept (a Mac asleep).
    pub fn check_reminders(&self) {
        self.shared.reminders.wake();
    }

    /// Dates reminders by `clock`, the app's (fixed in screenshots and tests).
    pub fn set_clock(&self, clock: crate::Clock) {
        *self
            .shared
            .clock
            .write()
            .expect("the clock lock is never poisoned") = clock;
    }

    /// The account's address.
    pub fn email(&self) -> &str {
        &self.shared.email
    }

    /// The latest snapshot of the list on screen.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.borrow().clone()
    }

    /// Shows `list` (snapshots follow it from now on).
    pub fn show(&self, list: ListId) {
        self.ask(ViewRequest::Show(list));
    }

    /// Opens `thread` (snapshots carry it from now on), or closes the open one.
    pub fn open_thread(&self, thread: Option<&str>) {
        self.ask(ViewRequest::Open(thread.map(ThreadId::new)));
    }

    /// Fetches the bodies of `thread` ahead of its opening, when they aren't stored, because the
    /// list's cursor rests on it.
    pub fn prefetch(&self, thread: &str) {
        self.shared
            .requests
            .post(Request::Prefetch(ThreadId::new(thread)));
    }

    /// Fetches the bytes behind attachments and remote images.
    pub fn loader(&self) -> Loader {
        self.shared.loader.clone()
    }

    /// Reads further down the list on screen (or the search's results), fetching from Gmail past
    /// what's stored.
    pub fn more(&self) {
        self.ask(ViewRequest::More);
    }

    /// Runs `query` (snapshots carry its results from now on, excerpts `snippet_tokens` words
    /// long), or stops searching. Running the query already running keeps its results.
    pub fn search(&self, query: Option<&str>, snippet_tokens: i64) {
        self.ask(ViewRequest::Search(
            query.map(str::to_owned),
            snippet_tokens,
        ));
    }

    /// Runs `query` ahead of being asked for. Gmail's first page of its matches is fetched without
    /// holding up what's on screen, so that running the query with [`search`](Self::search) shows
    /// them at once. One preload runs at a time, and a newer one replaces it.
    pub fn preload_search(&self, query: &str, snippet_tokens: i64) {
        self.ask(ViewRequest::Preload(query.to_owned(), snippet_tokens));
    }

    /// Narrows the list on screen with `filter` (snapshots follow it from now on), or shows all
    /// of it.
    pub fn filter(&self, filter: Option<Filter>) {
        self.ask(ViewRequest::Filter(filter));
    }

    /// Lets go of the rows that stayed in the list after an action took them out of it (as entering
    /// search does).
    pub fn unstick(&self) {
        self.ask(ViewRequest::Unstick);
    }

    /// Applies `mutation` to `threads`, undone with the token `undo` while its toast shows. Returns
    /// the call's number. Snapshots whose [`Snapshot::applied`] reaches it show its effect.
    pub fn apply(&self, threads: &[String], mutation: Mutation, undo: Option<UndoToken>) -> u64 {
        let threads = threads.iter().map(|id| ThreadId::new(id.clone())).collect();
        self.shared.requests.post(Request::Apply {
            view: self.view,
            threads,
            mutation,
            undo,
        });
        self.shared.applies.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Runs `job` (blocks a sender, or sends one to Important or Other). [`Account::undo`] with
    /// `undo` undoes it, even while it runs.
    pub fn sender_job(&self, job: crate::senders::SenderJob, undo: UndoToken) {
        self.shared.requests.post(Request::Sender { job, undo });
    }

    /// Deletes the Gmail filters blocking `from`, an address or domain (Unblock in Blocked
    /// Senders). Updating the block list is up to the caller.
    pub fn unblock(&self, from: &str) {
        self.shared.requests.post(Request::Unblock(from.to_owned()));
    }

    /// Creates the label `name` in Gmail. [`Event::LabelCreated`] or [`Event::LabelNotCreated`]
    /// follows.
    pub fn create_label(&self, name: &str) {
        self.shared
            .requests
            .post(Request::CreateLabel(name.to_owned()));
    }

    /// Reads thread `id` from the store, for [`Event::Thread`]. An action on a conversation that
    /// isn't open reads its messages this way.
    pub fn read_thread(&self, id: &str) {
        self.shared
            .requests
            .post(Request::ReadThread(ThreadId::new(id.to_owned())));
    }

    /// Unsubscribes with a one-click POST to `url` (RFC 8058), now, or once Gmail can be reached
    /// when offline. A quit meanwhile doesn't lose it.
    pub fn unsubscribe(&self, url: &url::Url) {
        self.shared
            .requests
            .post(Request::Unsubscribe(url.to_string()));
    }

    /// Where one-click unsubscribes may post.
    pub fn reach(&self) -> crate::unsubscribe::Reach {
        self.shared.reach
    }

    /// Undoes the [`Account::apply`], sender job or bulk action that carried `token`.
    pub fn undo(&self, token: UndoToken) {
        if !self.shared.bulk.undo(token) {
            self.shared.requests.post(Request::Undo(token));
        }
    }

    /// Queues a bulk action. [`Account::undo`] with `token` undoes it.
    pub fn bulk(&self, token: UndoToken, request: crate::bulk::Request) {
        self.shared.bulk.submit(token, request);
    }

    /// This session's bulk actions and those of the last week before it, oldest first.
    pub fn bulk_actions(&self) -> Vec<crate::bulk::Summary> {
        self.shared.bulk.summaries()
    }

    /// Puts `email`'s latest conversations in snapshots from now on (the contact pane's Mail
    /// section), read from local mail and topped up from Gmail once a session.
    pub fn show_contact(&self, email: &str) {
        self.show_contact_in(email, None);
    }

    /// [`Self::show_contact`] with the conversation `thread` on screen, whose headers say whether
    /// `email`'s message passed DMARC.
    pub(crate) fn show_contact_in(&self, email: &str, thread: Option<&str>) {
        self.ask(ViewRequest::Contact(
            email.to_lowercase(),
            thread.map(ThreadId::new),
        ));
    }

    /// Checks now whether Gmail still answers after the network changed, as a change to the OS's
    /// network does ([`exo_store::Store::network_changed`]).
    pub fn network_changed(&self) {
        self.shared.network.wake();
    }

    /// Sets how often to sync. Gaining focus syncs at once.
    pub fn set_presence(&self, presence: Presence) {
        let changed = self
            .shared
            .presence
            .send_if_modified(|current| std::mem::replace(current, presence) != presence);
        if changed && presence == Presence::Focused {
            self.shared.sync_now.wake();
        }
    }

    /// Whether the account's mail is on screen, with this handle's page showing it. Off screen, the
    /// account's new data asks for no frame. Coming on screen asks for one, for what came
    /// meanwhile, and the store's hooks for the list on screen follow this handle's view.
    pub fn set_on_screen(&self, on_screen: bool) {
        if !self
            .shared
            .repaint
            .on_screen
            .swap(on_screen, Ordering::Relaxed)
            && on_screen
        {
            self.shared.repaint.ctx.request_repaint();
        }
        if on_screen {
            self.ask(ViewRequest::Focus);
        }
    }

    /// What happened since this handle's last call that the user should hear about. Every
    /// handle hears every event, each page acting on its own.
    pub fn events(&self) -> Vec<Event> {
        let mut events = self.shared.events.borrow_mut();
        let Events { receiver, readers } = &mut *events;
        readers.retain(|reader| reader.strong_count() > 0);
        for event in receiver.try_iter() {
            for reader in readers.iter().filter_map(std::rc::Weak::upgrade) {
                reader.borrow_mut().push(event.clone());
            }
        }
        drop(events);
        let mut events = self.events.take();
        events.retain(|event| match event {
            Event::SettingsChanged { from, to } => {
                self.settings_changed(from, to);
                false
            }
            _ => true,
        });
        events
    }

    /// Applies settings changed elsewhere to the UI's copy, keeping what the UI changed since
    /// they were read, and saves the result when that kept something.
    fn settings_changed(&self, from: &serde_json::Value, to: &serde_json::Value) {
        let mut latest = self.shared.settings.borrow_mut();
        let Ok(current) = serde_json::to_value(&*latest.settings) else {
            return;
        };
        let merged = exo_store::merge_settings(from, &current, to);
        if merged == current {
            return;
        }
        let settings: Settings = match serde_json::from_value(merged.clone()) {
            Ok(settings) => settings,
            Err(error) => {
                log::warn!("settings from Examail Cloud don't read: {error}");
                return;
            }
        };
        if merged != *to {
            self.shared
                .requests
                .post(Request::SaveSettings(Box::new(settings.clone())));
        }
        latest.settings = Arc::new(settings);
        latest.version += 1;
    }

    /// Whether nothing is in flight, meaning no request, sync, flush, backfill or fetch is running
    /// or asked for, and the latest snapshot has read every change to the store. For screenshots
    /// and tests.
    pub fn is_idle(&self) -> bool {
        self.shared.requests.busy.load(Ordering::SeqCst) == 0
            && self.shared.published.load(Ordering::SeqCst) == *self.shared.changes.borrow()
            && self.shared.calendar.is_idle()
            && self.shared.bulk.is_idle()
    }

    /// The Tokio runtime driving this account's background tasks.
    pub(crate) fn runtime(&self) -> &Arc<Runtime> {
        &self.shared.runtime
    }

    /// How far Gmail's history is applied this session (the footer's `Updating: k of n`).
    pub fn sync_progress(&self) -> exo_store::SyncProgress {
        *self.shared.progress.borrow()
    }

    /// How the files of draft `id` stand with Gmail (compose's cards). None are up for a draft the
    /// store doesn't know of yet.
    pub fn upload(&self, id: &str) -> exo_store::Upload {
        self.shared
            .uploads
            .borrow()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Why the account's store stopped working since it opened, if it did. The app shows the
    /// teardown for it, as for a store that wouldn't open.
    pub fn unusable(&self) -> Option<exo_store::Unusable> {
        self.shared.health.unusable()
    }

    /// The calendar's data.
    pub fn calendar(&self) -> &crate::calendar::CalendarSync {
        &self.shared.calendar
    }

    /// The account's settings.
    pub fn settings(&self) -> &Settings {
        &self.settings.settings
    }

    /// Changes the account's settings, at once for the UI on every page of the account, and saves
    /// them in the background.
    pub fn update_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        self.catch_up();
        let mut settings = Settings::clone(&self.settings.settings);
        change(&mut settings);
        if settings != *self.settings.settings {
            self.shared
                .requests
                .post(Request::SaveSettings(Box::new(settings.clone())));
            self.settings.settings = Arc::new(settings);
            self.share_settings();
        }
    }

    /// Saves `draft` as `email`, locally at once and to Gmail once it has every file and its pace
    /// allows (at most every 5 s while it's typed in, or every 30 s with files).
    pub fn save_draft(&self, draft: Draft, email: gmail::mime::Email) {
        self.shared.requests.post(Request::SaveDraft {
            draft: Box::new(draft),
            email: Box::new(email),
        });
    }

    /// The folder that holds the bytes of draft attachments, each named by its uuid.
    pub fn draft_files(&self) -> &Path {
        &self.shared.files
    }

    /// Discards draft `id` (bring it back by saving it again).
    pub fn discard_draft(&self, id: &str) {
        self.shared
            .requests
            .post(Request::DiscardDraft(id.to_owned()));
    }

    /// Holds draft `id` for sending at `at`, or moves its hold there.
    pub fn send_draft(&self, id: &str, at: jiff::Timestamp) {
        self.shared.requests.post(Request::SendDraft {
            id: id.to_owned(),
            at,
        });
    }

    /// Holds draft `id` for its Send later time ([`exo_store::Store::schedule_draft`]).
    pub fn schedule_draft(&self, id: &str) {
        self.shared
            .requests
            .post(Request::ScheduleDraft(id.to_owned()));
    }

    /// Makes failed draft `id` a draft to edit again (`Edit Message`).
    pub fn recover_draft(&self, id: &str) {
        self.shared
            .requests
            .post(Request::RecoverDraft(id.to_owned()));
    }

    /// Writes every draft Gmail lacks to Gmail now, rather than at its pace, as switching away
    /// does, so Gmail web and the phone have them.
    pub fn write_drafts_now(&self) {
        self.shared.requests.post(Request::WriteDraftsNow);
    }

    /// Reads (again) everyone compose can suggest into snapshots, from stored mail (scored) and
    /// Google contacts.
    pub fn load_address_book(&self) {
        self.shared.requests.post(Request::LoadAddressBook);
    }

    /// Takes back a held send. [`Event::SendTakenBack`] says whether it was still held.
    pub fn cancel_send(&self, id: &str) {
        self.shared
            .requests
            .post(Request::CancelSend(id.to_owned()));
    }

    /// Undo send. Takes back a held send and the reminder it put on its thread, answered as
    /// [`cancel_send`](Self::cancel_send) is.
    pub fn undo_send(&self, id: &str) {
        self.shared.requests.post(Request::UndoSend(id.to_owned()));
    }

    /// Gives `reminder` to the message just sent as draft `id`, or takes the reminder off
    /// (`Set reminder` on the follow-up hint, and Undo on its toast). While the send is held, the
    /// draft and its thread get the reminder, as at Send. Once the send has gone, `thread` gets
    /// it, without being marked done.
    pub fn remind_sent(
        &self,
        id: &str,
        thread: Option<String>,
        reminder: Option<exo_store::DraftReminder>,
    ) {
        self.shared.requests.post(Request::RemindSent {
            draft: id.to_owned(),
            thread,
            reminder,
        });
    }

    /// Asks what Gmail doesn't have yet (signing out asks first), once the drafts waiting out their
    /// pace are written. [`Event::Unsent`] answers.
    pub fn ask_unsent(&self) {
        self.shared.requests.post(Request::Unsent);
    }

    /// Stops the account for good without sending what it holds, as signing out does. Held sends,
    /// queued changes and the calendar's held changes stay unsent. The account's tasks are stopped
    /// when this returns, and its store closes once every handle on the account is dropped.
    pub fn discard(&mut self) {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.calendar_accounts().signing_in = None;
        block_on_runtime(&self.shared.runtime, self.shared.restarts.stop(STOP_GRACE));
    }

    /// Marks whether closing the account's last handle is a mid-session `View ▸ Reload` rather than
    /// quitting Exo, so Undo-Send and Calendar Undo holds stay scheduled instead of flushing early.
    pub(crate) fn set_reload_close(&self, reload: bool) {
        self.shared.reload_close.set(reload);
    }

    /// Claims this account's quit flush so multiple open accounts can flush concurrently in
    /// [`std::thread::scope`]. Returns `None` if already claimed or discarded.
    pub(crate) fn take_quit_flush(&self) -> Option<QuitFlush<'_>> {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return None;
        }
        self.calendar_accounts().signing_in = None;
        Some(QuitFlush {
            runtime: &self.shared.runtime,
            calendar: &self.shared.calendar,
            requests: &self.shared.requests,
            flush: &self.shared.flush,
            restarts: &self.shared.restarts,
        })
    }

    /// This device's settings for the account.
    pub fn device_settings(&self) -> &DeviceSettings {
        &self.settings.device
    }

    /// Changes this device's settings for the account, as [`Account::update_settings`] does.
    pub fn update_device_settings(&mut self, change: impl FnOnce(&mut DeviceSettings)) {
        self.catch_up();
        let mut device = DeviceSettings::clone(&self.settings.device);
        change(&mut device);
        if device != *self.settings.device {
            self.shared
                .requests
                .post(Request::SaveDeviceSettings(device.clone()));
            self.settings.device = Arc::new(device);
            self.share_settings();
        }
    }
}

impl Drop for Account {
    /// The handle's view goes. The account closes with its last handle (when [`Shared`] drops).
    fn drop(&mut self) {
        self.shared.requests.post(Request::RemoveView(self.view));
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // Its people show no more, and what loads their photos lets go of the store.
        crate::people::forget(&self.repaint.ctx, &self.email);
        if *self.closed.get_mut() {
            return;
        }
        self.calendar_accounts.get_mut().signing_in = None;
        if self.reload_close.get() {
            drain_before_reload(&self.runtime, &self.requests, &self.restarts);
            return;
        }
        // Queued mutations are in the store's outbox and go out on the next launch. Held sends go
        // now, for a few seconds at most, rather than wait for the next launch, and so do the
        // drafts the app saved on its way out and the calendar's changes still held for Undo. What
        // Calendar can't take now stays queued for the next launch.
        QuitFlush {
            runtime: &self.runtime,
            calendar: &self.calendar,
            requests: &self.requests,
            flush: &self.flush,
            restarts: &self.restarts,
        }
        .run();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::mailbox::ThreadSummary;
    use ui_tasks::until_then;
    use worker::check_every;

    #[test]
    fn a_pause_is_waited_out_until_it_ends() {
        let left = until_then(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(40));
        assert!(
            (Duration::from_secs(39)..=Duration::from_secs(40)).contains(&left),
            "{left:?}"
        );
        assert_eq!(
            until_then(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1)),
            Duration::ZERO,
            "already over"
        );
    }

    /// Waits for `account` to be idle.
    fn settle(account: &Account) {
        let started = Instant::now();
        while !account.is_idle() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the account never went idle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn database_failure(code: std::ffi::c_int) -> exo_store::Error {
        exo_store::Error::Database(toasty::Error::driver_operation_failed(
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None),
        ))
    }

    #[test]
    fn a_store_stops_on_corruption_a_full_disk_or_five_io_failures_in_a_row() {
        let health = || {
            Health::new(Repaint {
                ctx: egui::Context::default(),
                on_screen: Arc::new(AtomicBool::new(false)),
            })
        };
        let io = database_failure(rusqlite::ffi::SQLITE_IOERR);
        let passing = health();
        for _ in 0..4 {
            passing.failed(&io);
        }
        passing.ok();
        for _ in 0..4 {
            passing.failed(&io);
        }
        assert_eq!(
            passing.unusable(),
            None,
            "an answer between them starts the count over"
        );
        passing.failed(&io);
        assert_eq!(
            passing.unusable(),
            Some(exo_store::Unusable::Corrupt),
            "five in a row"
        );
        let corrupt = health();
        corrupt.failed(&database_failure(rusqlite::ffi::SQLITE_CORRUPT));
        assert_eq!(
            corrupt.unusable(),
            Some(exo_store::Unusable::Corrupt),
            "at once"
        );
        let full = health();
        full.failed(&database_failure(rusqlite::ffi::SQLITE_FULL));
        full.failed(&database_failure(rusqlite::ffi::SQLITE_CORRUPT));
        assert_eq!(
            full.unusable(),
            Some(exo_store::Unusable::DiskFull),
            "the first reason stays"
        );
        let busy = health();
        busy.failed(&database_failure(rusqlite::ffi::SQLITE_BUSY));
        busy.failed(&exo_store::Error::NotSynced("labels"));
        assert_eq!(busy.unusable(), None);
    }

    #[test]
    fn a_wake_is_busy_until_the_loop_is_done_with_it() {
        let requests = Requests {
            sender: mpsc::unbounded_channel().0,
            busy: Arc::default(),
        };
        let busy = || requests.busy.load(Ordering::SeqCst);
        let (wake, mut wakes) = Wake::new(&requests);
        wake.wake();
        wake.wake();
        assert_eq!(busy(), 2, "busy from the call, before the loop runs");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let woken = runtime.block_on(wakes.next()).expect("woken");
        assert_eq!(busy(), 1, "one run answers both");
        drop(woken);
        assert_eq!(busy(), 0);
        drop(wakes);
        wake.wake();
        assert_eq!(busy(), 0, "a wake for a loop that's gone isn't counted");
    }

    #[test]
    fn regaining_focus_is_busy_until_its_sync_is_on_screen() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-account-focus-{}", std::process::id()));
        let tokens = Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into()));
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: server.endpoints(),
            tokens: tokens.clone(),
        };
        let account = Account::open(&dir, credentials, egui::Context::default()).expect("opens");
        settle(&account);
        let id = account
            .snapshot()
            .threads
            .first()
            .expect("the inbox has mail")
            .id
            .clone();
        let gmail = gmail::Client::new(server.endpoints(), tokens);
        let archive = gmail::ModifyThreadRequest {
            add_label_ids: Vec::new(),
            remove_label_ids: vec!["INBOX".into()],
        };
        runtime
            .block_on(async { gmail.gmail().modify_thread(&id, &archive).await })
            .expect("archived elsewhere");

        account.set_presence(Presence::Visible);
        account.set_presence(Presence::Focused);
        assert!(
            !account.is_idle(),
            "busy from the moment focus asks for a sync"
        );
        settle(&account);
        assert!(
            !account
                .snapshot()
                .threads
                .iter()
                .any(|thread| thread.id == id),
            "the sync took it out of the list"
        );
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reminder_coming_back_notifies_its_thread_though_its_read() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!(
            "exo-account-reminder-notify-{}",
            std::process::id()
        ));
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: server.endpoints(),
            tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
        };
        let account = Account::open(&dir, credentials, egui::Context::default()).expect("opens");
        let notifier = Notifier::recording(crate::desktop::Launcher::default());
        account.set_notifier(Some(notifier.clone()));
        let now = jiff::Zoned::now();
        account.set_clock(crate::Clock::fixed(now.clone()));
        // Only Important's mail notifies while Important and Other are on.
        account.show(ListId::Important);
        settle(&account);
        let mail = || {
            notifier
                .posted()
                .into_iter()
                .filter(|notice| notice.key.starts_with("EMAIL-"))
                .collect::<Vec<_>>()
        };
        // Its newest message is someone else's, because the user's own never notify.
        let theirs = |thread: &ThreadSummary| {
            runtime
                .block_on(account.shared.loader.store.latest_arrival(&thread.id))
                .expect("reads")
                .is_some_and(|arrival| arrival.from.is_some_and(|from| !from.is_me))
        };
        let thread = account
            .snapshot()
            .threads
            .iter()
            .find(|thread| !thread.unread && thread.drafts == 0 && theirs(thread))
            .cloned()
            .expect("a read Important thread");
        let at = now
            .timestamp()
            .checked_add(jiff::SignedDuration::from_mins(1))
            .expect("a time");
        account.apply(
            std::slice::from_ref(&thread.id),
            Mutation::Remind {
                at,
                keep_on_reply: true,
                snooze: true,
            },
            None,
        );
        settle(&account);
        assert!(
            !account
                .snapshot()
                .threads
                .iter()
                .any(|row| row.id == thread.id),
            "snoozed"
        );
        let before = mail().len();

        account.set_clock(crate::Clock::fixed(
            now.checked_add(jiff::SignedDuration::from_mins(2))
                .expect("a time"),
        ));
        account.check_reminders();
        settle(&account);
        assert!(
            account
                .snapshot()
                .threads
                .iter()
                .any(|row| row.id == thread.id),
            "back in the inbox"
        );
        let posted = mail();
        let [notice] = &posted[before..] else {
            panic!("one notification: {posted:?}")
        };
        let newest = runtime
            .block_on(account.shared.loader.store.latest_arrival(&thread.id))
            .expect("reads")
            .expect("stored");
        assert_eq!(
            notice.key,
            format!("EMAIL-{}", newest.message),
            "for the thread's newest message"
        );
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mail_that_arrives_after_the_first_sync_notifies_once() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-account-notify-{}", std::process::id()));
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: server.endpoints(),
            tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
        };
        let account = Account::open(&dir, credentials, egui::Context::default()).expect("opens");
        let notifier = Notifier::recording(crate::desktop::Launcher::default());
        account.set_notifier(Some(notifier.clone()));
        settle(&account);
        let sync = || {
            account.set_presence(Presence::Visible);
            account.set_presence(Presence::Focused);
            settle(&account);
        };
        // The mock's calendar is near today, so reminders may come too. Only mail counts here.
        let mail = || {
            notifier
                .posted()
                .into_iter()
                .filter(|notice| notice.key.starts_with("EMAIL-"))
                .collect::<Vec<_>>()
        };
        assert!(mail().is_empty(), "the first sync catches up quietly");

        let deliver = |body: serde_json::Value| {
            runtime.block_on(async {
                reqwest::Client::new()
                    .post(server.url().join("/__mock/deliver").expect("path"))
                    .json(&body)
                    .send()
                    .await
                    .expect("delivered")
                    .json::<serde_json::Value>()
                    .await
                    .expect("a message")
            })
        };
        let delivered = deliver(
            serde_json::json!({ "from": "Priya Raman <priya@brightmoor.co>", "subject": "Launch checklist", "text": "Before 3?" }),
        );
        deliver(
            serde_json::json!({ "from": "deals@shop.example", "subject": "Sale", "labelIds": ["INBOX", "UNREAD", "CATEGORY_PROMOTIONS"] }),
        );
        sync();
        let posted = mail();
        let [notice] = posted.as_slice() else {
            panic!("one notification: {posted:?}")
        };
        assert_eq!(
            notice.key,
            format!("EMAIL-{}", delivered["id"].as_str().expect("an id"))
        );
        assert_eq!(
            (notice.title.as_str(), notice.body.as_str()),
            ("Priya Raman", "Launch checklist")
        );
        sync();
        assert_eq!(mail().len(), 1, "once");
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mail_that_comes_while_the_first_sync_fails_still_notifies() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir =
            std::env::temp_dir().join(format!("exo-account-late-sync-{}", std::process::id()));
        let open = || {
            let credentials = Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints: server.endpoints(),
                tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
            };
            Account::open(&dir, credentials, egui::Context::default()).expect("opens")
        };
        let post = |path: &str, body: serde_json::Value| {
            runtime.block_on(async {
                reqwest::Client::new()
                    .post(server.url().join(path).expect("path"))
                    .json(&body)
                    .send()
                    .await
                    .expect("posted")
            });
        };
        // Gmail dates each message as it comes: the mock's clock is set to now first.
        let deliver = |from: &str, subject: &str| {
            let now = jiff::Timestamp::now().to_string();
            post("/__mock/clock", serde_json::json!({ "now": now }));
            post(
                "/__mock/deliver",
                serde_json::json!({ "from": from, "subject": subject, "text": "3?" }),
            );
        };
        // A first run fills the store, so the next one starts from Gmail's history.
        let account = open();
        settle(&account);
        drop(account);
        // What comes while Exo is closed is caught up quietly.
        deliver("Priya Raman <priya@brightmoor.co>", "While closed");
        // This run's first sync fails, and mail comes before the next one.
        post(
            "/__mock/faults",
            serde_json::json!({ "path": "/gmail/v1/users/me/history", "status": 400, "message": "Bad Request", "reason": "badRequest" }),
        );
        let account = open();
        let notifier = Notifier::recording(crate::desktop::Launcher::default());
        account.set_notifier(Some(notifier.clone()));
        settle(&account);
        deliver("Tom Becker <tom@brightmoor.co>", "Since opening");
        account.set_presence(Presence::Visible);
        account.set_presence(Presence::Focused);
        settle(&account);
        let mail: Vec<String> = notifier
            .posted()
            .into_iter()
            .filter(|notice| notice.key.starts_with("EMAIL-"))
            .map(|notice| notice.body)
            .collect();
        assert_eq!(mail, ["Since opening"]);
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_notification_shows_the_senders_photo_fetched_if_need_be() {
        let runtime = Runtime::new().expect("runtime");
        let config = mock_servers::Config {
            contact_photos: true,
            ..mock_servers::Config::default()
        };
        let server = runtime
            .block_on(mock_servers::MockServer::start(config))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-account-photo-{}", std::process::id()));
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: server.endpoints(),
            tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
        };
        let ctx = egui::Context::default();
        let account = Account::open(&dir, credentials, ctx.clone()).expect("opens");
        let notifier = Notifier::recording(crate::desktop::Launcher::default());
        account.set_notifier(Some(notifier.clone()));
        settle(&account);
        // Priya's photo is asked for, as the contact pane does, which saves it. Tom's isn't.
        let started = Instant::now();
        while crate::people::photo(&ctx, "priya@brightmoor.co").is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "no photo");
            std::thread::sleep(Duration::from_millis(10));
        }
        for from in [
            "Priya Raman <priya@brightmoor.co>",
            "Tom Becker <tom@brightmoor.co>",
            "Yusuf Karaca <yusuf@tinkerform.io>",
        ] {
            runtime.block_on(async {
                reqwest::Client::new()
                    .post(server.url().join("/__mock/deliver").expect("path"))
                    .json(&serde_json::json!({ "from": from, "subject": "Launch", "text": "3?" }))
                    .send()
                    .await
                    .expect("delivered")
            });
        }
        account.set_presence(Presence::Visible);
        account.set_presence(Presence::Focused);
        settle(&account);
        let images: Vec<(String, Option<Vec<u8>>)> = notifier
            .posted()
            .into_iter()
            .filter(|notice| notice.key.starts_with("EMAIL-"))
            .map(|notice| {
                let image = notice
                    .image
                    .map(|file| std::fs::read(file).expect("a copy"));
                (notice.title, image)
            })
            .collect();
        let shown: Vec<(&str, bool)> = images
            .iter()
            .map(|(title, image)| (title.as_str(), image.is_some()))
            .collect();
        assert_eq!(shown.len(), 3, "a notification for each: {shown:?}");
        for (title, photo) in [
            ("Priya Raman", true),
            ("Tom Becker", true),
            ("Yusuf Karaca", false),
        ] {
            let image = images
                .iter()
                .find(|(shown, _)| shown == title)
                .and_then(|(_, image)| image.as_deref());
            assert_eq!(
                image.is_some_and(|image| image::load_from_memory(image).is_ok()),
                photo,
                "{title}: {shown:?}"
            );
        }
        // Tom's, fetched for his notification, is saved and drawn too.
        assert!(crate::people::photo(&ctx, "tom@brightmoor.co").is_some());
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_listing_asks_google_only_for_what_changed_until_the_token_expires() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-account-sync-{}", std::process::id()));
        let open = || {
            let credentials = Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints: server.endpoints(),
                tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
            };
            let account =
                Account::open(&dir, credentials, egui::Context::default()).expect("opens");
            settle(&account);
            account
        };
        let kept = |account: &Account| {
            runtime
                .block_on(account.shared.loader.store.google_contacts())
                .expect("reads")
                .expect("kept")
        };
        let emails = |list: &exo_store::GoogleList| -> Vec<String> {
            list.contacts
                .iter()
                .map(|contact| contact.email.clone())
                .collect()
        };
        let change = |body: serde_json::Value| {
            runtime.block_on(async {
                reqwest::Client::new()
                    .post(server.url().join("/__mock/people").expect("path"))
                    .json(&body)
                    .send()
                    .await
                    .expect("changed")
            });
        };
        // Kept as stale, with an entry Google never listed, which only a full listing drops.
        let age = |account: &Account| {
            let mut listing = kept(account);
            listing.listed -= jiff::SignedDuration::from_hours(2);
            listing.directory.contacts.push(exo_store::GoogleContact {
                person: "people/kept".into(),
                email: "kept@brightmoor.co".into(),
                ..exo_store::GoogleContact::default()
            });
            runtime
                .block_on(account.shared.loader.store.save_google_contacts(&listing))
                .expect("saves");
        };

        let account = open();
        let listing = kept(&account);
        assert!(listing.saved.sync_token.is_some() && listing.directory.sync_token.is_some());
        assert!(emails(&listing.directory).contains(&"noor@brightmoor.co".to_owned()));
        change(serde_json::json!({ "list": "directory", "put": {
            "resourceName": "people/9001",
            "names": [{ "displayName": "Ivy Chen" }],
            "emailAddresses": [{ "value": "ivy@brightmoor.co" }],
        } }));
        change(serde_json::json!({ "list": "directory", "remove": "noor@brightmoor.co" }));
        age(&account);
        drop(account);

        let account = open();
        let directory = emails(&kept(&account).directory);
        assert!(
            directory.contains(&"ivy@brightmoor.co".to_owned()),
            "{directory:?}"
        );
        assert!(!directory.contains(&"noor@brightmoor.co".to_owned()));
        assert!(
            directory.contains(&"kept@brightmoor.co".to_owned()),
            "only the changes were asked for"
        );
        change(serde_json::json!({ "expire": true }));
        age(&account);
        drop(account);

        let account = open();
        let listing = kept(&account);
        let directory = emails(&listing.directory);
        assert!(
            !directory.contains(&"kept@brightmoor.co".to_owned()),
            "listed in full once the token expired"
        );
        assert!(directory.contains(&"ivy@brightmoor.co".to_owned()));
        assert!(listing.directory.sync_token.is_some());
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// How long after a sync that ended now the next is due, the pace starting at `start` and
    /// becoming `then.1` after `then.0`.
    async fn due(
        start: (Presence, Power),
        then: Option<(Duration, (Presence, Power))>,
    ) -> Duration {
        let (presence_sender, presence) = watch::channel(start.0);
        let (power_sender, power) = watch::channel(start.1);
        let ended = tokio::time::Instant::now();
        if let Some((after, (presence, power))) = then {
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                presence_sender.send_replace(presence);
                power_sender.send_replace(power);
                std::future::pending::<()>().await;
            });
        }
        Pace { presence, power }.due(ended).await;
        ended.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn the_pace_follows_the_window_and_off_screen_the_power_source() {
        let secs = Duration::from_secs;
        let (on_mains, on_battery) = (
            (Presence::OffScreen, Power::Plugged),
            (Presence::OffScreen, Power::Battery),
        );
        assert_eq!(
            due((Presence::Focused, Power::Battery), None).await,
            secs(25)
        );
        assert_eq!(
            due((Presence::Visible, Power::Battery), None).await,
            secs(60)
        );
        assert_eq!(
            due((Presence::Background, Power::Battery), None).await,
            secs(60),
            "a hidden window's account syncs as a shown one's"
        );
        assert_eq!(due(on_mains, None).await, secs(60));
        assert_eq!(due(on_battery, None).await, secs(300));
        assert_eq!(
            due(on_battery, Some((secs(30), on_mains))).await,
            secs(60),
            "plugged in, due a minute after the last sync"
        );
        assert_eq!(
            due(on_battery, Some((secs(90), on_mains))).await,
            secs(90),
            "and at once when that's past"
        );
        assert_eq!(due(on_mains, Some((secs(30), on_battery))).await, secs(300));
        assert_eq!(
            due(
                on_battery,
                Some((secs(10), (Presence::Focused, Power::Battery)))
            )
            .await,
            secs(25),
            "coming on screen"
        );
        assert_eq!(
            (
                check_every(Presence::Visible),
                check_every(Presence::Background),
                check_every(Presence::OffScreen)
            ),
            (secs(10), secs(60), secs(60))
        );
    }

    #[test]
    fn an_account_off_screen_syncs_every_minute_on_mains_and_every_five_on_battery() {
        let runtime = Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-account-power-{}", std::process::id()));
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: server.endpoints(),
            tokens: Arc::new(gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into())),
        };
        let account = Account::open(&dir, credentials, egui::Context::default()).expect("opens");
        let power = PowerSource::fixed(Power::Battery);
        account.follow_power(&power);
        account.set_on_screen(false);
        account.set_presence(Presence::OffScreen);
        settle(&account);
        let quota = server.url().join("/__mock/quota").expect("path");
        // What sync spent looking for new mail. The pre-render's pass a minute in isn't sync.
        let units = || {
            runtime.block_on(async {
                reqwest::get(quota.clone())
                    .await
                    .expect("answers")
                    .json::<serde_json::Value>()
                    .await
                    .expect("json")["methods"]["history.list"]["units"]
                    .as_u64()
                    .unwrap_or_default()
            })
        };
        let synced = units();

        std::thread::sleep(Duration::from_secs(62));
        assert_eq!(units(), synced, "on battery, no sync a minute later");
        // Plugged in, the sync due a minute after the last goes at once, as one `history.list`.
        power.set(Power::Plugged);
        let plugged = Instant::now();
        while units() == synced {
            assert!(
                plugged.elapsed() < Duration::from_secs(5),
                "no sync once plugged in"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        settle(&account);
        assert_eq!(units() - synced, 2);
        drop(account);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reload_close_keeps_undo_send_hold_instead_of_flushing_early() {
        let dir =
            std::env::temp_dir().join(format!("exo-account-reload-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let credentials = Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints: gmail::Endpoints::google(),
            tokens: Arc::new(gmail::StaticToken("unused".into())),
        };
        let account = Account::open(&dir, credentials, egui::Context::default()).expect("opens");
        let send_at = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(120);
        account.save_draft(
            Draft {
                id: "draft00000000000099".into(),
                thread: None,
                adopted: None,
                record: exo_store::DraftRecord {
                    action: exo_store::DraftAction::Compose,
                    from: Some(exo_store::Address::new("Maya", "maya@brightmoor.co")),
                    to: vec![exo_store::Address::address("tom@brightmoor.co")],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "Hold me".into(),
                    body: "<div>Wait for undo</div>".into(),
                    quoted_content: None,
                    quoted_inlined: false,
                    in_reply_to: None,
                    in_reply_to_rfc822: None,
                    references: Vec::new(),
                    rfc822_id: "hold99@exo".into(),
                    fingerprint: String::new(),
                    attachments: Vec::new(),
                    send_later: None,
                    reminder: None,
                },
                raw: String::new(),
                complete: true,
                sending: false,
            },
            gmail::mime::Email {
                from: Some(gmail::mime::Mailbox {
                    name: Some("Maya".into()),
                    email: "maya@brightmoor.co".into(),
                }),
                to: vec![gmail::mime::Mailbox {
                    name: None,
                    email: "tom@brightmoor.co".into(),
                }],
                subject: "Hold me".into(),
                html: Some("<div>Wait for undo</div>".into()),
                ..gmail::mime::Email::default()
            },
        );
        account.send_draft("draft00000000000099", send_at);
        account.set_reload_close(true);
        drop(account);

        let runtime = shared_runtime().expect("runtime");
        let drafts = block_on_runtime(&runtime, async {
            let store = Store::open(
                &dir,
                "maya@brightmoor.co",
                gmail::Endpoints::google(),
                gmail::StaticToken("unused".into()),
            )
            .await
            .expect("reopens");
            store.drafts().await.expect("reads drafts")
        });
        assert_eq!(drafts.len(), 1);
        assert_eq!(
            drafts[0].send_at,
            Some(send_at),
            "reload close keeps the future Undo-Send hold timestamp"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_tasks_track_busy_state_across_send_and_drain() {
        let ctx = egui::Context::default();
        let tasks = UiTasks::<Vec<u8>>::default();
        assert!(tasks.is_idle());

        let tx = tasks.sender(&ctx);
        assert!(
            tasks.is_idle(),
            "long-lived UiSender handles do not pin UiTasks busy while idle"
        );

        tx.send(|out| out.push(1));
        tx.send(|out| out.push(2));
        assert!(
            !tasks.is_idle(),
            "queued tasks keep UiTasks busy until drained"
        );

        let mut out = Vec::new();
        tasks.drain(&mut out);
        assert_eq!(out, vec![1, 2]);
        assert!(tasks.is_idle());

        let pending = tasks.pending.clone();
        drop(tasks);
        tx.send(|out| out.push(3));
        assert_eq!(
            pending.load(Ordering::SeqCst),
            0,
            "sending to a dropped UiTasks releases the Busy guard immediately"
        );
    }
}
