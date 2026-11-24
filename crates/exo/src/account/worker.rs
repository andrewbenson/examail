use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use exo_store::{
    Connection, DeviceSettings, Draft, DraftFailure, Filter, GmailPage, GoogleContacts, ListId,
    Mutation, ScoredContact, Search, SendFailure, Settings, Store, ThreadId, Undo,
};
use futures::FutureExt as _;
use tokio::sync::{mpsc, watch};

use super::{
    Event, Presence, UndoToken,
    publish::{Searching, Snapshot, ViewState, same_search},
    ui_tasks::{Busy, Repaint, Requests, Wake, Wakes},
};
use crate::{
    notifications::{Notifier, mail as mail_notifications},
    power::Power,
    recovery::Restarts,
};

/// How often to sync ([`sync_every`]): about every 25 s while the window is focused (for want of
/// push); every minute (`FAST`) while the window shows the account otherwise, and for an account
/// off screen on mains; every five minutes (`MEDIUM`) for an account off screen on battery.
const FOCUSED: Duration = Duration::from_secs(25);
const FAST: Duration = Duration::from_secs(60);
const MEDIUM: Duration = Duration::from_secs(300);
/// While Gmail is unreachable, how soon to try again (the connection coming back also syncs at
/// once).
const OFFLINE_RETRY: Duration = Duration::from_secs(5);
/// A sync this long after the last one that reached Gmail follows a gap (asleep or offline), so
/// only the newest of its new mail notifies. This is twice the slowest pace, off screen on battery.
const NOTIFY_GAP: jiff::SignedDuration = jiff::SignedDuration::from_mins(10);
/// While something is down, how often to check whether it's back. Every 10 s while the window
/// shows, and every minute while it's hidden.
const CHECK_VISIBLE: Duration = Duration::from_secs(10);
const CHECK_HIDDEN: Duration = Duration::from_secs(60);
/// Rows a list reads at first, and how many more each time it scrolls near its end.
pub(super) const PAGE: usize = 100;
/// Undos kept for toasts. Only the newest toast's undo can run, but a mutation and its toast can
/// cross in flight.
const UNDOS: usize = 8;
/// The longest the reminder scheduler sleeps before looking at the clock again, because a Mac
/// that's asleep doesn't advance the timer the scheduler sleeps on.
const REMINDER_RECHECK: Duration = Duration::from_secs(60);
/// How long after the first sync the store first evicts what it no longer keeps ([`Store::evict`]),
/// to stay out of the launch's way, and how often it evicts after that.
const EVICT_AFTER: Duration = Duration::from_secs(60);
const EVICT_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// How many of the conversations stored for their rows alone each pre-render fetches the bodies of,
/// and how often a pre-render runs (100 rows every ten minutes). The first runs a minute after the
/// first sync, out of the launch's way like eviction.
const PRERENDER: usize = 100;
const PRERENDER_AFTER: Duration = Duration::from_secs(60);
const PRERENDER_EVERY: Duration = Duration::from_secs(10 * 60);
/// How long a listing of Google contacts is used before it's listed again.
const GOOGLE_CONTACTS_FRESH: jiff::SignedDuration = jiff::SignedDuration::from_hours(1);
/// A listing counts as work under way for this long at most, so screenshots and tests wait for a
/// quick answer without hanging on a People that doesn't answer.
const GOOGLE_CONTACTS_BUSY: Duration = Duration::from_secs(5);
/// Failures that may pass, in a row, after which the store counts as corrupt.
const PASSING_FAILURES: usize = 5;

/// Names one of the account's views ([`Account::view`](super::Account::view)).
pub(super) type ViewId = u64;

/// What a view asks of the worker.
pub(super) enum ViewRequest {
    Show(ListId),
    Open(Option<ThreadId>),
    More,
    Contact(String, Option<ThreadId>),
    /// Run this search (or stop searching), excerpts this many words long.
    Search(Option<String>, i64),
    /// Run this search ahead of being asked for (instant search), excerpts this many words long.
    Preload(String, i64),
    /// Narrow the view's list with this filter, or show all of it.
    Filter(Option<Filter>),
    /// Let go of the list's sticky rows.
    Unstick,
    /// The view's page came on screen, so the store's hooks for the list on screen follow this
    /// view.
    Focus,
}

pub(super) enum Request {
    /// For one view.
    View(ViewId, ViewRequest),
    /// A new view, publishing its snapshots on this channel.
    AddView(ViewId, watch::Sender<Arc<Snapshot>>),
    RemoveView(ViewId),
    Apply {
        /// The view it was applied from, whose rows it may hold.
        view: ViewId,
        threads: Vec<ThreadId>,
        mutation: Mutation,
        undo: Option<UndoToken>,
    },
    Undo(UndoToken),
    CreateLabel(String),
    /// Read a thread for [`Event::Thread`].
    ReadThread(ThreadId),
    /// Queue a one-click unsubscribe from this link and post it.
    Unsubscribe(String),
    /// Block a sender, or send them to Important or Other, undone with the token `undo`.
    Sender {
        job: crate::senders::SenderJob,
        undo: UndoToken,
    },
    /// Delete the Gmail filters that block this address or domain.
    Unblock(String),
    /// From the account's own tasks.
    ContactFillFailed(String),
    /// From the sync loop, when a round ended. `reached` says whether it reached Gmail.
    Synced {
        reached: bool,
    },
    /// From the account's own tasks, once the files that incomplete drafts lacked were fetched
    /// again, except these (by uuid), which trying again won't get.
    FilesFetched(Vec<String>),
    Backfilled(ListId),
    /// From the account's own tasks, once the open thread's bodies were fetched (or couldn't be).
    /// For a thread that wasn't stored, `unavailable` says it couldn't be had, and whether because
    /// Gmail couldn't be reached.
    BodiesLoaded {
        thread: ThreadId,
        unavailable: Option<bool>,
    },
    /// Fetch this thread's bodies if they aren't stored, because the list's cursor rests on it.
    Prefetch(ThreadId),
    /// From the account's own tasks, when bringing back snoozed threads with no reminder failed.
    /// The next sync tries again.
    OrphansFailed,
    /// From the account's own tasks, once a thread's bodies were prefetched (or couldn't be).
    Prefetched(ThreadId),
    Refresh,
    /// Boxed, because the settings document is several times the size of the other requests.
    SaveSettings(Box<Settings>),
    SaveDeviceSettings(DeviceSettings),
    /// Save a draft as `email` with its attachments' bytes added.
    SaveDraft {
        draft: Box<Draft>,
        email: Box<gmail::mime::Email>,
    },
    DiscardDraft(String),
    SendDraft {
        id: String,
        at: jiff::Timestamp,
    },
    /// Hold the draft for its Send later time.
    ScheduleDraft(String),
    CancelSend(String),
    /// Cancel the held send and take back the reminder it put on its thread.
    UndoSend(String),
    /// Give the message just sent as this draft a reminder, or take its reminder off. The reminder
    /// goes on the held draft, or on the thread if the draft already went.
    RemindSent {
        draft: String,
        thread: Option<String>,
        reminder: Option<exo_store::DraftReminder>,
    },
    RecoverDraft(String),
    /// Write every draft Gmail lacks now, not at its pace, because the account is being left.
    WriteDraftsNow,
    /// Write every draft Gmail lacks now, then count what it still doesn't have, for
    /// [`Event::Unsent`].
    Unsent,
    /// Read the address book, from stored mail now and from Google contacts in the background when
    /// those are missing or stale.
    LoadAddressBook,
    /// Read the Google contacts listed last, and list them again in the background when they're
    /// missing or stale.
    LoadGoogleContacts,
    /// From the task that scored stored mail.
    MailContacts(Arc<Vec<ScoredContact>>),
    /// From the task that listed Google contacts, colleagues and "other contacts", and whether it
    /// listed them all.
    GoogleContacts(GoogleContacts, bool),
    /// On quitting, send every draft held for undo now, write the drafts saved on the way out, and
    /// say when done.
    SendHeldNow(tokio::sync::oneshot::Sender<()>),
    /// On `View ▸ Reload`, finish handling queued local draft saves without expiring Undo-Send
    /// holds or blocking on Gmail writes.
    DrainBeforeReload(tokio::sync::oneshot::Sender<()>),
    /// From the account's own tasks, a page of Gmail's matches for the search with this number
    /// (`None` when Gmail couldn't be reached).
    SearchPage(u64, Option<GmailPage>),
}

/// Whether the account's store still works once open, judged from its database's errors. A corrupt
/// file or a full disk stops the account at once, and failures that may pass (I/O) stop it after
/// five in a row. The app then shows the same teardown as for a store that won't open (`teardown`).
/// These errors surface in the worker's reads, which follow every change and flush, and in sync's
/// writes.
pub(crate) struct Health {
    unusable: std::sync::Mutex<Option<exo_store::Unusable>>,
    /// Failures that may pass, in a row.
    passing: AtomicUsize,
    repaint: Repaint,
}

impl Health {
    pub(super) fn new(repaint: Repaint) -> Self {
        Self {
            unusable: std::sync::Mutex::new(None),
            passing: AtomicUsize::new(0),
            repaint,
        }
    }

    /// The store answered.
    pub(super) fn ok(&self) {
        self.passing.store(0, Ordering::Relaxed);
    }

    /// The store failed with `error`.
    pub(super) fn failed(&self, error: &exo_store::Error) {
        let unusable = match error.failing() {
            Some(exo_store::Failing::Unusable(unusable)) => unusable,
            Some(exo_store::Failing::Passing)
                if self.passing.fetch_add(1, Ordering::Relaxed) + 1 >= PASSING_FAILURES =>
            {
                exo_store::Unusable::Corrupt
            }
            Some(exo_store::Failing::Passing) | None => return,
        };
        let mut known = self.unusable.lock().expect("health lock is never poisoned");
        if known.is_none() {
            log::error!("the account's store stopped working ({unusable:?}): {error}");
            *known = Some(unusable);
            self.repaint.request();
        }
    }

    pub(super) fn unusable(&self) -> Option<exo_store::Unusable> {
        *self.unusable.lock().expect("health lock is never poisoned")
    }
}

/// A sender job's progress, shared with the task running it.
#[derive(Default)]
pub(super) struct SenderJobState {
    /// What it changed, once it's done.
    done: Option<crate::senders::Done>,
    /// Whether undo was asked for, in which case the task undoes the job as soon as it's done.
    undo: bool,
}

/// The task that owns what's on screen. It handles requests in order and publishes snapshots.
pub(super) struct Worker {
    pub(super) store: Store,
    /// What compose suggests from (mail's contacts with Google's merged in).
    pub(super) address_book: Option<Arc<Vec<ScoredContact>>>,
    /// Stored mail's contacts, as last scored.
    pub(super) mail_contacts: Option<Arc<Vec<ScoredContact>>>,
    /// Google contacts and when they were listed, once they have been.
    pub(super) google_contacts: Option<Arc<GoogleContacts>>,
    /// A Google listing is under way.
    pub(super) listing_google: bool,
    /// Where the photos of the Google contacts come from.
    pub(super) photos: crate::people::Photos,
    /// The senders' brands, whose logos the contact pane shows.
    pub(super) brands: crate::brands::Brands,
    /// Drafts whose last save left attachments off, with that save (the message without its files)
    /// to make again once the files can be had. The save is `None` when retrying won't get them.
    pub(super) incomplete: HashMap<String, Option<Box<(Draft, gmail::mime::Email)>>>,
    /// The files of the incomplete drafts are being fetched again, off this task.
    pub(super) fetching_files: bool,
    /// Cached account settings so [`Worker::publish`] doesn't deserialize the settings row on
    /// every store tick.
    pub(super) settings: Settings,
    pub(super) repaint: Repaint,
    pub(super) requests: Requests,
    pub(super) changes: watch::Receiver<u64>,
    /// The store revision the latest snapshot read.
    pub(super) published: Arc<AtomicU64>,
    /// The connection to Gmail, which snapshots carry.
    pub(super) connection: watch::Receiver<Connection>,
    /// How far history is applied, which the footer reads.
    pub(super) progress: watch::Receiver<exo_store::SyncProgress>,
    /// How drafts' files stand with Gmail, which compose's cards show.
    pub(super) uploads: watch::Receiver<exo_store::Uploads>,
    pub(super) events: std::sync::mpsc::Sender<Event>,
    /// The account's views, one per page, with what each one's snapshots carry.
    pub(super) views: HashMap<ViewId, ViewState>,
    /// The view on screen, which the store's hooks for the list on screen follow.
    pub(super) focused: ViewId,
    /// Open threads whose bodies are being fetched.
    pub(super) opening: HashSet<ThreadId>,
    /// Threads whose bodies are being prefetched.
    pub(super) prefetching: HashSet<ThreadId>,
    /// Snoozed threads with no reminder in this store were brought back, or are being.
    pub(super) orphans: bool,
    /// The first round of sync finished, so an empty inbox really is empty.
    pub(super) synced: bool,
    /// A sync round reached Gmail this session, so the drafts an earlier run left without their
    /// files were handed back to be saved again ([`Event::Incomplete`]).
    pub(super) resaved: bool,
    pub(super) backfilling: HashSet<ListId>,
    /// Addresses whose conversations were asked of Gmail this session (or are being).
    pub(super) filled: HashSet<String>,
    pub(super) undos: Vec<(UndoToken, Undo)>,
    /// Sender jobs by undo token, with what each changed once it's done and whether undo was asked
    /// for before then.
    pub(super) sender_jobs: Vec<(UndoToken, Arc<std::sync::Mutex<SenderJobState>>)>,
    /// How many apply requests were handled.
    pub(super) applied: u64,
    pub(super) flush: Wake,
    /// The number the next search gets.
    pub(super) searches: u64,
    /// The failures of the account's background (`recovery`).
    pub(super) restarts: Arc<Restarts>,
    /// Whether the store still works.
    pub(super) health: Arc<Health>,
}

impl Worker {
    /// Keeps the rows that `mutation` stars or unstars in Starred where they are in `view`, the
    /// view it was applied from (starring sticks them and doesn't dismiss them). Any other mutation
    /// but marking read or unread lets go of the held rows it acts on, in every view, because such
    /// a mutation dismisses a row that's no longer starred and unsticks it.
    fn hold(&mut self, view: ViewId, threads: &[ThreadId], mutation: &Mutation) {
        match mutation {
            Mutation::Star | Mutation::Unstar => {
                let Some(view) = self
                    .views
                    .get_mut(&view)
                    .filter(|view| view.list == ListId::Starred && view.search.is_none())
                else {
                    return;
                };
                for thread in threads {
                    if !view.held.contains(thread) {
                        view.held.push(thread.clone());
                    }
                }
            }
            Mutation::MarkRead | Mutation::MarkUnread => {}
            _ => {
                for view in self.views.values_mut() {
                    view.held.retain(|id| !threads.contains(id));
                }
            }
        }
    }

    /// Brings Google contacts up to date in the background, unless that's under way or the last
    /// listing is fresh. Google is slow and changes little, so they're listed once a session and
    /// again when stale (only what changed, `people::listing`), and never waited for. Each listing
    /// counts as busy only for its first moments, so the UI's quiet doesn't hang on it.
    fn list_google_contacts(&mut self) {
        let stale = self.google_contacts.as_ref().is_none_or(|google| {
            jiff::Timestamp::now().duration_since(google.listed) > GOOGLE_CONTACTS_FRESH
        });
        if !stale || self.listing_google {
            return;
        }
        self.listing_google = true;
        let (store, requests, busy) = (
            self.store.clone(),
            self.requests.clone(),
            self.requests.busy(),
        );
        let (account, kept) = (
            self.photos.account().to_owned(),
            self.google_contacts.clone(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                let listed = jiff::Timestamp::now();
                let listing =
                    crate::people::listing::list(&store, &account, kept.as_deref(), listed);
                tokio::pin!(listing);
                let (contacts, complete) = tokio::select! {
                    listing = &mut listing => listing,
                    () = tokio::time::sleep(GOOGLE_CONTACTS_BUSY) => {
                        drop(busy);
                        listing.await
                    }
                };
                requests.post(Request::GoogleContacts(contacts, complete));
            });
    }

    /// Suggests `contacts` in compose, and shows their photos and companies.
    fn show_google_contacts(&mut self, contacts: GoogleContacts) {
        self.photos.publish(contacts.contacts());
        self.google_contacts = Some(Arc::new(contacts));
        self.merge_address_book();
    }

    /// Publishes the address book from what's been read so far.
    fn merge_address_book(&mut self) {
        let Some(mail) = &self.mail_contacts else {
            return;
        };
        let book = match &self.google_contacts {
            Some(google) => merge_contacts(mail, google.contacts()),
            None => mail.to_vec(),
        };
        self.address_book = Some(Arc::new(book));
    }

    pub(super) async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        loop {
            tokio::select! {
                biased;
                request = requests.recv() => {
                    let Some(first) = request else { return };
                    let mut batch = vec![first];
                    while let Ok(next) = requests.try_recv() {
                        batch.push(next);
                    }
                    let count = batch.len();
                    let mut reread = false;
                    let mut panicked = false;
                    // A request that panics is dropped, and the ones after it are still answered
                    // (`recovery`).
                    for request in batch {
                        match AssertUnwindSafe(self.handle(request)).catch_unwind().await {
                            Ok(needs_publish) => reread |= needs_publish,
                            Err(_) => panicked = true,
                        }
                    }
                    if reread
                        && AssertUnwindSafe(self.publish()).catch_unwind().await.is_err()
                    {
                        panicked = true;
                    }
                    for _ in 0..count {
                        self.requests.handled();
                    }
                    if panicked && !self.recovered().await {
                        return;
                    }
                }
                Ok(()) = self.changes.changed() => {
                    let _busy = self.requests.busy();
                    if AssertUnwindSafe(self.publish()).catch_unwind().await.is_err() && !self.recovered().await {
                        return;
                    }
                }
                Ok(()) = self.connection.changed() => {
                    let connection = *self.connection.borrow_and_update();
                    for view in self.views.values() {
                        let mut snapshot = Snapshot::clone(&view.snapshot.borrow());
                        snapshot.connection = connection;
                        view.snapshot.send_replace(Arc::new(snapshot));
                    }
                    self.repaint.request();
                }
                // The footer's `Updating: k of n`.
                Ok(()) = self.progress.changed() => self.repaint.request(),
                // The upload bars on compose's cards.
                Ok(()) = self.uploads.changed() => self.repaint.request(),
            }
        }
    }

    /// After a panic, waits as [`Restarts::failed`] says and returns `true`, or returns `false`
    /// once Exo gives up.
    async fn recovered(&self) -> bool {
        let Some(wait) = self.restarts.failed() else {
            return false;
        };
        tokio::time::sleep(wait).await;
        true
    }

    /// Handles `request`, returning whether snapshots must be re-read and published.
    async fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::View(view, request) => self.handle_view(view, request).await,
            Request::AddView(view, snapshot) => {
                self.views.insert(view, ViewState::new(snapshot));
                true
            }
            Request::RemoveView(view) => {
                self.views.remove(&view);
                false
            }
            Request::BodiesLoaded {
                thread,
                unavailable,
            } => {
                self.opening.remove(&thread);
                if let Some(offline) = unavailable {
                    let _ = self.events.send(Event::Unavailable {
                        id: thread.as_str().to_owned(),
                        offline,
                    });
                    self.repaint.request();
                }
                false
            }
            Request::Prefetch(thread) => {
                if self.opening.contains(&thread) || !self.prefetching.insert(thread.clone()) {
                    return false;
                }
                let (store, requests, busy) = (
                    self.store.clone(),
                    self.requests.clone(),
                    self.requests.busy(),
                );
                self.restarts
                    .spawn_task(&tokio::runtime::Handle::current(), async move {
                        if let Err(error) = store.prefetch(&thread).await {
                            log::debug!(
                                "prefetching the bodies of thread {}: {error}",
                                thread.as_str()
                            );
                        }
                        requests.post(Request::Prefetched(thread));
                        drop(busy);
                    });
                false
            }
            Request::Prefetched(thread) => {
                self.prefetching.remove(&thread);
                false
            }
            Request::OrphansFailed => {
                self.orphans = false;
                false
            }
            Request::SearchPage(id, page) => {
                let Some(view) = self.views.values_mut().find(|view| view.searches(id)) else {
                    return false;
                };
                if view.preloading == Some(id) {
                    view.preloading = None;
                }
                if let Some(preload) = view.preload.as_mut().filter(|preload| preload.id == id) {
                    // A search run ahead stops at Gmail's first page until it's asked for.
                    preload.fetching = false;
                    match page {
                        Some(page) => {
                            preload.session.add_gmail(page);
                            preload.gmail_pages += 1;
                        }
                        None => preload.offline = true,
                    }
                    return false;
                }
                Self::fetch_preload_page(&self.store, &self.requests, &self.restarts, view);
                let Some(search) = view.search.as_mut().filter(|search| search.id == id) else {
                    return false;
                };
                search.fetching = false;
                match page {
                    Some(page) => {
                        search.offline = false;
                        search.session.add_gmail(page);
                        search.gmail_pages += 1;
                        if search.session.gmail_hits() < search.limit {
                            Self::fetch_page(&self.store, &self.requests, &self.restarts, search);
                        }
                    }
                    None => search.offline = true,
                }
                true
            }
            Request::Apply {
                view,
                threads,
                mutation,
                undo,
            } => {
                self.hold(view, &threads, &mutation);
                let result = self.store.apply(&threads, mutation.clone()).await;
                self.applied += 1;
                match result {
                    Ok(applied) => {
                        if let Some(token) = undo.filter(|_| !applied.is_empty()) {
                            self.undos.push((token, applied));
                            let excess = self.undos.len().saturating_sub(UNDOS);
                            self.undos.drain(..excess);
                        }
                        self.flush.wake();
                    }
                    Err(error) => {
                        log::error!("applying {mutation:?}: {error}");
                        let _ = self.events.send(Event::Failed {
                            mutation,
                            undo: false,
                        });
                    }
                }
                true
            }
            Request::CreateLabel(name) => {
                let (event, created) = match self.store.create_label(&name).await {
                    Ok(label) => {
                        for view in self.views.values_mut() {
                            if let Some(search) = &mut view.search {
                                search.cached = None;
                            }
                        }
                        (Event::LabelCreated(label), true)
                    }
                    Err(error) => {
                        log::warn!("creating a label: {error}");
                        (
                            Event::LabelNotCreated {
                                offline: matches!(error, exo_store::Error::Offline(_)),
                            },
                            false,
                        )
                    }
                };
                let _ = self.events.send(event);
                if !created {
                    self.repaint.request();
                }
                created
            }
            Request::ReadThread(id) => {
                let detail = match self.store.thread(&id).await {
                    Ok(detail) => detail.map(Arc::new),
                    Err(error) => {
                        log::error!("reading thread {id:?}: {error}");
                        None
                    }
                };
                let _ = self.events.send(Event::Thread {
                    id: id.as_str().to_owned(),
                    detail,
                });
                self.repaint.request();
                false
            }
            Request::Unsubscribe(url) => match self.store.queue_unsubscribe(&url).await {
                Ok(()) => {
                    self.flush.wake();
                    true
                }
                Err(error) => {
                    log::error!("queueing an unsubscribe: {error}");
                    false
                }
            },
            Request::Sender { job, undo } => {
                let state = Arc::new(std::sync::Mutex::new(SenderJobState::default()));
                self.sender_jobs.push((undo, state.clone()));
                let excess = self.sender_jobs.len().saturating_sub(UNDOS);
                self.sender_jobs.drain(..excess);
                let (store, busy) = (self.store.clone(), self.requests.busy());
                self.restarts
                    .spawn_task(&tokio::runtime::Handle::current(), async move {
                        let _busy = busy;
                        let done = match crate::senders::run(&store, &job).await {
                            Ok(done) => done,
                            Err(error) => return log::error!("a sender job: {error}"),
                        };
                        // Undone while it ran, so undo it now.
                        let done = {
                            let mut state =
                                state.lock().expect("sender job lock is never poisoned");
                            if !state.undo {
                                state.done = Some(done);
                                return;
                            }
                            done
                        };
                        if let Err(error) = crate::senders::undo(&store, done).await {
                            log::error!("undoing a sender job: {error}");
                        }
                    });
                false
            }
            Request::Unblock(from) => {
                let (store, busy) = (self.store.clone(), self.requests.busy());
                self.restarts
                    .spawn_task(&tokio::runtime::Handle::current(), async move {
                        let _busy = busy;
                        if let Err(error) = store.delete_block_filters(&from).await {
                            log::error!("unblocking a sender: {error}");
                        }
                    });
                false
            }
            Request::Undo(token) => {
                if let Some(at) = self
                    .sender_jobs
                    .iter()
                    .position(|(candidate, _)| *candidate == token)
                {
                    let (_, state) = self.sender_jobs.remove(at);
                    let done = {
                        let mut state = state.lock().expect("sender job lock is never poisoned");
                        state.undo = true;
                        state.done.take()
                    };
                    if let Some(done) = done {
                        let (store, busy) = (self.store.clone(), self.requests.busy());
                        self.restarts
                            .spawn_task(&tokio::runtime::Handle::current(), async move {
                                let _busy = busy;
                                if let Err(error) = crate::senders::undo(&store, done).await {
                                    log::error!("undoing a sender job: {error}");
                                }
                            });
                    }
                }
                let Some(at) = self
                    .undos
                    .iter()
                    .position(|(candidate, _)| *candidate == token)
                else {
                    return false;
                };
                let (_, undo) = self.undos.remove(at);
                match self.store.undo(undo).await {
                    Ok(()) => {
                        self.flush.wake();
                        true
                    }
                    Err(error) => {
                        log::error!("undoing: {error}");
                        false
                    }
                }
            }
            // Asked again the next time the pane shows them.
            Request::ContactFillFailed(email) => {
                self.filled.remove(&email);
                false
            }
            Request::Synced { reached } => {
                if !self.synced {
                    self.synced = true;
                    let lists: Vec<(ListId, usize)> = self
                        .views
                        .values()
                        .map(|view| (view.list.clone(), view.limit))
                        .collect();
                    for (list, limit) in lists {
                        self.backfill_if_short(&list, limit).await;
                    }
                }
                if reached {
                    self.fetch_missing_files();
                    self.resave_incomplete().await;
                    self.return_orphaned_snoozes();
                    // Threads opened while Gmail couldn't be reached get their bodies now.
                    let open: HashSet<ThreadId> = self
                        .views
                        .values()
                        .filter_map(|view| view.open.clone())
                        .collect();
                    for thread in open {
                        self.load_bodies(thread).await;
                    }
                }
                true
            }
            Request::FilesFetched(gone) => {
                self.fetching_files = false;
                for retry in self.incomplete.values_mut() {
                    if retry.as_ref().is_some_and(|retry| {
                        retry
                            .0
                            .record
                            .attachments
                            .iter()
                            .any(|attachment| gone.contains(&attachment.uuid))
                    }) {
                        *retry = None;
                    }
                }
                self.complete_drafts().await;
                true
            }
            Request::Backfilled(list) => self.backfilling.remove(&list),
            Request::Refresh => true,
            Request::SaveSettings(settings) => {
                if let Err(error) = self.store.save_settings(&*settings).await {
                    log::error!("saving settings: {error}");
                }
                self.settings = *settings;
                true
            }
            Request::SaveDeviceSettings(settings) => {
                if let Err(error) = self.store.save_settings(&settings).await {
                    log::error!("saving device settings: {error}");
                }
                false
            }
            Request::SaveDraft { draft, email } => {
                let id = draft.id.clone();
                let missing = self.save_draft(*draft, *email).await;
                if missing > 0 {
                    let _ = self
                        .events
                        .send(Event::AttachmentsFailed { id, count: missing });
                    self.repaint.request();
                }
                true
            }
            Request::DiscardDraft(id) => {
                // Saving it again would bring it back.
                self.incomplete.remove(&id);
                match self.store.discard_draft(&id).await {
                    Ok(()) => {
                        self.flush.wake();
                        true
                    }
                    Err(error) => {
                        log::error!("discarding draft {id}: {error}");
                        false
                    }
                }
            }
            Request::SendDraft { id, .. } if self.incomplete.contains_key(&id) => {
                // Sending would leave files off, so the user gets the draft back instead.
                let reason = SendFailure::Other;
                let _ = self
                    .events
                    .send(Event::DraftFailed(DraftFailure::Send { id, reason }));
                self.repaint.request();
                false
            }
            Request::SendDraft { id, at } => match self.store.send_draft(&id, at).await {
                Ok(()) => {
                    self.flush.wake();
                    true
                }
                Err(error) => {
                    log::error!("sending draft {id}: {error}");
                    false
                }
            },
            Request::ScheduleDraft(id) if self.incomplete.contains_key(&id) => {
                let reason = SendFailure::Other;
                let _ = self
                    .events
                    .send(Event::DraftFailed(DraftFailure::Send { id, reason }));
                self.repaint.request();
                false
            }
            Request::ScheduleDraft(id) => match self.store.schedule_draft(&id).await {
                Ok(()) => {
                    self.flush.wake();
                    true
                }
                Err(error) => {
                    log::error!("scheduling draft {id}: {error}");
                    false
                }
            },
            Request::RecoverDraft(id) => match self.store.recover_draft(&id).await {
                Ok(()) => true,
                Err(error) => {
                    log::error!("recovering draft {id}: {error}");
                    false
                }
            },
            Request::RemindSent {
                draft,
                thread,
                reminder,
            } => {
                self.remind_sent(draft, thread, reminder).await;
                true
            }
            Request::CancelSend(id) => {
                self.cancel_send(id).await;
                true
            }
            Request::UndoSend(id) => {
                self.undo_send(id).await;
                true
            }
            Request::WriteDraftsNow => {
                self.spawn_write_drafts_now();
                false
            }
            Request::Unsent => {
                self.spawn_unsent();
                false
            }
            Request::LoadAddressBook => {
                let (store, requests, busy) = (
                    self.store.clone(),
                    self.requests.clone(),
                    self.requests.busy(),
                );
                self.restarts
                    .spawn_task(&tokio::runtime::Handle::current(), async move {
                        match store.address_book().await {
                            Ok(book) => requests.post(Request::MailContacts(Arc::new(book))),
                            Err(error) => log::error!("reading the address book: {error}"),
                        }
                        drop(busy);
                    });
                self.list_google_contacts();
                false
            }
            Request::LoadGoogleContacts => {
                let mut changed = false;
                if self.google_contacts.is_none() {
                    match self.store.google_contacts().await {
                        Ok(Some(saved)) => {
                            self.show_google_contacts(saved);
                            changed = self.mail_contacts.is_some();
                        }
                        Ok(None) => {}
                        Err(error) => log::warn!("reading the Google contacts: {error}"),
                    }
                }
                self.list_google_contacts();
                changed
            }
            Request::MailContacts(contacts) => {
                self.mail_contacts = Some(contacts);
                self.merge_address_book();
                true
            }
            Request::GoogleContacts(contacts, complete) => {
                self.listing_google = false;
                if complete {
                    if let Err(error) = self.store.save_google_contacts(&contacts).await {
                        log::warn!("saving the Google contacts: {error}");
                    }
                    self.show_google_contacts(contacts);
                    self.mail_contacts.is_some()
                } else if self.google_contacts.is_none() {
                    // A listing that failed part way shows what it got, but isn't kept and doesn't
                    // replace a whole one, and the next read of the address book lists again.
                    self.show_google_contacts(GoogleContacts {
                        listed: jiff::Timestamp::UNIX_EPOCH,
                        ..contacts
                    });
                    self.mail_contacts.is_some()
                } else {
                    false
                }
            }
            Request::SendHeldNow(done) => {
                self.send_held_now(done).await;
                false
            }
            Request::DrainBeforeReload(done) => {
                let _ = done.send(());
                false
            }
        }
    }

    /// Handles `request` for view `id`, returning whether snapshots must be re-read and published.
    async fn handle_view(&mut self, id: ViewId, request: ViewRequest) -> bool {
        let focused = id == self.focused;
        let Some(view) = self.views.get_mut(&id) else {
            return false;
        };
        match request {
            ViewRequest::Focus => {
                let was_focused = self.focused == id;
                self.focused = id;
                self.store.show(&view.list);
                self.store.loaded(view.limit);
                self.store.reading(view.open.as_ref()).await;
                !was_focused && view.search.is_some()
            }
            ViewRequest::Show(list) => {
                if focused {
                    self.store.show(&list);
                }
                if list != view.list {
                    view.list = list;
                    view.limit = PAGE;
                    view.sticky.clear();
                    view.held.clear();
                }
                if focused {
                    self.store.loaded(view.limit);
                }
                let (list, limit) = (view.list.clone(), view.limit);
                self.backfill_if_short(&list, limit).await;
                true
            }
            ViewRequest::Filter(filter) => {
                if filter != view.filter {
                    view.filter = filter;
                    view.sticky.clear();
                    view.held.clear();
                    true
                } else {
                    false
                }
            }
            ViewRequest::Unstick => {
                let had_rows = !view.sticky.is_empty() || !view.held.is_empty();
                view.sticky.clear();
                view.held.clear();
                had_rows
            }
            ViewRequest::Open(thread) => {
                view.open = thread.clone();
                if focused {
                    self.store.reading(thread.as_ref()).await;
                }
                if let Some(thread) = thread {
                    self.load_bodies(thread).await;
                }
                true
            }
            ViewRequest::More => {
                if let Some(search) = &mut view.search {
                    search.limit += PAGE;
                    Self::fetch_page(&self.store, &self.requests, &self.restarts, search);
                    return true;
                }
                view.limit += PAGE;
                if focused {
                    self.store.loaded(view.limit);
                }
                let (list, limit) = (view.list.clone(), view.limit);
                self.backfill_if_short(&list, limit).await;
                true
            }
            ViewRequest::Search(None, _) => {
                let had_search = view.search.take().is_some();
                view.preload = None;
                had_search
            }
            ViewRequest::Search(Some(raw), snippet_tokens) => {
                if view.search.as_ref().is_some_and(|search| {
                    search.raw == raw && search.snippet_tokens == snippet_tokens
                }) {
                    return false;
                }
                // The search run ahead for this query shows what it has at once, and any other
                // preload is dropped.
                let search = match view
                    .preload
                    .take()
                    .filter(|preload| same_search(&preload.raw, &raw))
                {
                    Some(mut preload) => {
                        preload.session.ask();
                        Searching {
                            raw,
                            snippet_tokens,
                            cached: None,
                            ..preload
                        }
                    }
                    None => {
                        Searching::new(&mut self.searches, &raw, Search::new(&raw), snippet_tokens)
                    }
                };
                let fetch = search.session.gmail_hits() < search.limit;
                let search = view.search.insert(search);
                if fetch {
                    Self::fetch_page(&self.store, &self.requests, &self.restarts, search);
                }
                true
            }
            ViewRequest::Preload(raw, snippet_tokens) => {
                let asked = |search: &Option<Searching>| {
                    search
                        .as_ref()
                        .is_some_and(|search| same_search(&search.raw, &raw))
                };
                if asked(&view.search) || asked(&view.preload) {
                    return false;
                }
                // Like a search run for the screen, the preload reads the local index as it starts.
                // What the index finds shows as found there, not as Gmail's hits.
                let mut preload = Searching::new(
                    &mut self.searches,
                    &raw,
                    Search::preload(&raw),
                    snippet_tokens,
                );
                if let Err(error) = self
                    .store
                    .search(&mut preload.session, preload.limit, true, snippet_tokens)
                    .await
                {
                    log::warn!("searching ahead: {error}");
                }
                view.preload = Some(preload);
                Self::fetch_preload_page(&self.store, &self.requests, &self.restarts, view);
                false
            }
            ViewRequest::Contact(email, thread) => {
                self.show_contact(id, email, thread).await;
                false
            }
        }
    }

    /// Once a session, has the store bring back the snoozed threads it has no reminder for (such as
    /// a new store's), off this task. A round that fails tries again at the next sync.
    fn return_orphaned_snoozes(&mut self) {
        if std::mem::replace(&mut self.orphans, true) {
            return;
        }
        let (store, requests, flush, busy) = (
            self.store.clone(),
            self.requests.clone(),
            self.flush.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                match store.return_orphaned_snoozes().await {
                    Ok(returned) if !returned.is_empty() => flush.wake(),
                    Ok(_) => {}
                    Err(error) => {
                        log::warn!("bringing back snoozed threads: {error}");
                        requests.post(Request::OrphansFailed);
                    }
                }
                drop(busy);
            });
    }

    /// Fetches an open thread's bodies when they aren't stored (the thread was stored for its row
    /// alone), or the whole thread when it isn't stored at all (opened from a link or a
    /// notification), unless they're on their way. Meanwhile, snapshots carry the thread with
    /// placeholders for the messages that lack bodies, or with nothing.
    async fn load_bodies(&mut self, thread: ThreadId) {
        if self.opening.contains(&thread) {
            return;
        }
        let stored = match self.store.has_thread(&thread).await {
            Ok(stored) => stored,
            Err(error) => return log::error!("reading thread {}: {error}", thread.as_str()),
        };
        if stored {
            match self.store.bodies_stored(&thread).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => return log::error!("reading thread {}: {error}", thread.as_str()),
            }
        }
        self.opening.insert(thread.clone());
        let (store, requests, busy) = (
            self.store.clone(),
            self.requests.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                let unavailable = if stored {
                    if let Err(error) = store.load_bodies(&thread).await {
                        log::warn!("fetching the bodies of thread {}: {error}", thread.as_str());
                    }
                    None
                } else {
                    match store.load_thread(&thread).await {
                        Ok(found) => (!found).then_some(false),
                        Err(error) => {
                            log::warn!("fetching thread {}: {error}", thread.as_str());
                            Some(matches!(error, exo_store::Error::Offline(_)))
                        }
                    }
                };
                requests.post(Request::BodiesLoaded {
                    thread,
                    unavailable,
                });
                drop(busy);
            });
    }

    /// Fetches more of a view's `list` from Gmail when what's stored doesn't fill `limit`.
    async fn backfill_if_short(&mut self, list: &ListId, limit: usize) {
        // Until the first round of sync is done, sync's own backfill fills the inbox lists.
        if !self.synced || self.backfilling.contains(list) {
            return;
        }
        let page = match self.store.list(list, None, limit).await {
            Ok(page) => page,
            Err(error) => return log::error!("reading {list:?}: {error}"),
        };
        if page.next.is_some() || !page.more_remote {
            return;
        }
        let list = list.clone();
        let wanted = limit.saturating_sub(page.threads.len()).max(1);
        self.backfilling.insert(list.clone());
        let (store, requests, busy) = (
            self.store.clone(),
            self.requests.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                if let Err(error) = store.backfill(&list, wanted).await {
                    log::warn!("backfilling {list:?}: {error}");
                }
                requests.post(Request::Backfilled(list));
                drop(busy);
            });
    }
}

/// How long after a sync the next one is due. While the window shows the account, that's about 25 s
/// when focused, else a minute on either power source, even hidden or minimized, because a hidden
/// window still shows the account. Off screen, it's a minute on mains and five on battery.
fn sync_every(presence: Presence, power: Power) -> Duration {
    match (presence, power) {
        (Presence::Focused, _) => FOCUSED,
        (Presence::Visible | Presence::Background, _) | (Presence::OffScreen, Power::Plugged) => {
            FAST
        }
        (Presence::OffScreen, Power::Battery) => MEDIUM,
    }
}

/// How often to check whether a lost connection is back: every 10 s while the window shows the
/// account, and every minute while it's hidden or the account is off screen.
pub(super) fn check_every(presence: Presence) -> Duration {
    match presence {
        Presence::Focused | Presence::Visible => CHECK_VISIBLE,
        Presence::Background | Presence::OffScreen => CHECK_HIDDEN,
    }
}

/// What sets how often the account syncs: the window, and off screen the Mac's power source.
pub(super) struct Pace {
    pub(super) presence: watch::Receiver<Presence>,
    pub(super) power: watch::Receiver<Power>,
}

impl Pace {
    /// Waits until the next sync after one that ended at `ended` is due, at the pace as it is
    /// then. A change of pace counts from that sync: plugging in five minutes after a sync on
    /// battery syncs at once.
    pub(super) async fn due(&mut self, ended: tokio::time::Instant) {
        loop {
            let every = sync_every(
                *self.presence.borrow_and_update(),
                *self.power.borrow_and_update(),
            );
            tokio::select! {
                () = tokio::time::sleep_until(ended + every) => return,
                Ok(()) = self.presence.changed() => {}
                Ok(()) = self.power.changed() => {}
            }
        }
    }
}

/// Where an account's notifications go: its address, the app's notifier once there is one, and
/// the senders' brands, whose context has the senders' photos ([`crate::people`]).
pub(super) type Notify = (
    String,
    watch::Receiver<Option<Notifier>>,
    crate::brands::Brands,
);

/// Syncs at the [`Pace`], at once when focused or asked, and continuously while the store
/// backfills. The new mail each round brings is posted to the notifier of the account `notify`
/// names, except what Gmail got before the loop started, which the first round to reach Gmail
/// catches up on quietly. `synced` turns true once the first round has been tried.
pub(super) async fn sync_loop(
    store: Store,
    requests: Requests,
    mut pace: Pace,
    mut now: Wakes,
    notify: Notify,
    synced: watch::Sender<bool>,
    health: Arc<Health>,
) {
    // Opening the account asks for the first sync.
    let Some(mut busy) = now.next().await else {
        return;
    };
    // When the loop started (the account opening), and when a sync last reached Gmail. Mail Gmail
    // got since the start notifies even when the first round to reach Gmail comes late, after
    // one that failed (offline, say).
    let opened = jiff::Timestamp::now();
    let mut reached: Option<jiff::Timestamp> = None;
    loop {
        let wait = {
            let _busy = busy;
            let result = loop {
                let result = store.sync().await;
                synced.send_replace(true);
                // Every list has its first page after one round, so an empty one is empty, and the
                // list on screen can backfill on its own while the rounds go on.
                requests.post(Request::Synced {
                    reached: result.is_ok(),
                });
                if let Ok(report) = &result {
                    let synced = jiff::Timestamp::now();
                    let last = reached.replace(synced);
                    let arrived: Vec<exo_store::Arrival> = report
                        .arrived
                        .iter()
                        .filter(|arrival| last.is_some() || arrival.date >= opened)
                        .cloned()
                        .collect();
                    let gap = synced.duration_since(last.unwrap_or(opened)) > NOTIFY_GAP;
                    notify_arrivals(&store, &notify, &arrived, gap).await;
                }
                match result {
                    Ok(report) if report.backfilling => {}
                    other => break other,
                }
            };
            match result {
                Ok(_) => None,
                Err(exo_store::Error::Throttled { until }) => {
                    let left = until.duration_since(jiff::Timestamp::now());
                    Some(Duration::try_from(left).unwrap_or(OFFLINE_RETRY))
                }
                // The offline bar says so, and a line every retry would fill the log (`logging`).
                Err(exo_store::Error::Offline(_)) => Some(OFFLINE_RETRY),
                Err(error) => {
                    health.failed(&error);
                    log::warn!("sync: {error}");
                    Some(OFFLINE_RETRY)
                }
            }
        };
        let ended = tokio::time::Instant::now();
        let due = async {
            match wait {
                Some(wait) => tokio::time::sleep_until(ended + wait).await,
                None => pace.due(ended).await,
            }
        };
        busy = tokio::select! {
            () = due => requests.busy(),
            woken = now.next() => match woken {
                Some(busy) => busy,
                None => return,
            },
        };
    }
}

/// Evicts the mail and events the store no longer keeps, [`EVICT_AFTER`] the first sync and then
/// every [`EVICT_EVERY`].
pub(super) async fn evict_loop(store: Store, mut first_sync: watch::Receiver<bool>) {
    if first_sync.wait_for(|synced| *synced).await.is_err() {
        return;
    }
    tokio::time::sleep(EVICT_AFTER).await;
    loop {
        if let Err(error) = store.evict(&exo_store::Eviction::default()).await {
            log::warn!("evicting: {error}");
        }
        tokio::time::sleep(EVICT_EVERY).await;
    }
}

/// Fetches the bodies of [`PRERENDER`] conversations stored for their rows alone, only those
/// eviction keeps, then their parts and the search index's text of every body that came in. The
/// first pass runs [`PRERENDER_AFTER`] the first sync (not at once, while the launch is backfilling
/// the lists), then one every [`PRERENDER_EVERY`]. Offline, the fetching stops and waits for the
/// next pass.
pub(super) async fn prerender_loop(
    store: Store,
    requests: Requests,
    mut first_sync: watch::Receiver<bool>,
) {
    if first_sync.wait_for(|synced| *synced).await.is_err() {
        return;
    }
    tokio::time::sleep(PRERENDER_AFTER).await;
    loop {
        {
            let _busy = requests.busy();
            match store
                .prerender(&exo_store::Eviction::default(), PRERENDER)
                .await
            {
                Ok(_) | Err(exo_store::Error::Offline(_) | exo_store::Error::Throttled { .. }) => {}
                Err(error) => log::warn!("pre-rendering: {error}"),
            }
            // Then their images and the inbox's files.
            match store.prefetch_parts(PRERENDER).await {
                Ok(_) | Err(exo_store::Error::Offline(_) | exo_store::Error::Throttled { .. }) => {}
                Err(error) => log::warn!("prefetching attachments: {error}"),
            }
            // And what the renders show, for the search index.
            if let Err(error) = store.index_texts().await {
                log::warn!("indexing visible text: {error}");
            }
        }
        tokio::time::sleep(PRERENDER_EVERY).await;
    }
}

/// Notifies about `thread`, which a reminder just brought back, in the account `notify` names. The
/// notification is for its newest message, even though it's been read, unless the thread is muted
/// or out of the inbox, notifications are off, or the message is the user's own.
async fn notify_returned(store: &Store, (account, notifier, brands): &Notify, thread: &ThreadId) {
    let notifier = notifier.borrow().clone();
    let Some(notifier) = notifier else { return };
    let found = async {
        Ok::<_, exo_store::Error>((
            store.latest_arrival(thread.as_str()).await?,
            store.settings::<Settings>().await?,
            store.settings::<DeviceSettings>().await?,
        ))
    };
    match found.await {
        Ok((Some(arrival), settings, device))
            if mail_notifications::notifies(&arrival, &settings, &device, true) =>
        {
            notifier.post(vec![mail_notice(store, &arrival, account, brands).await])
        }
        Ok(_) => {}
        Err(error) => log::warn!("notifying about a reminder: {error}"),
    }
}

/// The notification for `arrival` in `account`, with the sender's photo when they have one, else
/// their brand's logo when `arrival` itself passed DMARC, either within
/// [`crate::people::NOTIFICATION_WAIT`].
async fn mail_notice(
    store: &Store,
    arrival: &exo_store::Arrival,
    account: &str,
    brands: &crate::brands::Brands,
) -> crate::notifications::Notice {
    let mut notice = mail_notifications::notice(arrival, account);
    let Some(from) = &arrival.from else {
        return notice;
    };
    notice.image = crate::people::notification_photo(brands.ctx(), &from.email, &notice.key).await;
    if notice.image.is_none() {
        let logo = async {
            let headers = store.headers(&arrival.message).await.ok()??;
            brands.logo_for_sender(&from.email, &headers).await
        };
        let logo = tokio::time::timeout(crate::people::NOTIFICATION_WAIT, logo).await;
        notice.image = logo
            .ok()
            .flatten()
            .and_then(|png| crate::people::notification_copy(&png, &notice.key));
    }
    notice
}

/// Posts the notifications a sync's new mail calls for in the account `notify` names, when the
/// account has a notifier. After a `gap`, only the newest arrival notifies
/// ([`mail_notifications::notifying`]).
async fn notify_arrivals(
    store: &Store,
    (account, notifier, brands): &Notify,
    arrived: &[exo_store::Arrival],
    gap: bool,
) {
    let notifier = notifier.borrow().clone();
    let Some(notifier) = notifier.filter(|_| !arrived.is_empty()) else {
        return;
    };
    let settings = async {
        Ok::<_, exo_store::Error>((
            store.settings::<Settings>().await?,
            store.settings::<DeviceSettings>().await?,
        ))
    };
    let (settings, device) = match settings.await {
        Ok(settings) => settings,
        Err(error) => return log::warn!("reading notification settings: {error}"),
    };
    // The photos come together, so the round waits at most one photo's time.
    let notices = mail_notifications::notifying(arrived, &settings, &device, gap)
        .into_iter()
        .map(|arrival| mail_notice(store, arrival, account, brands));
    notifier.post(futures::future::join_all(notices).await);
}

/// Brings threads back when their reminders come due, on the device. Threads due while Exo was
/// closed come back at once, and the rest as each comes due, as long as Exo runs, each with a
/// notification to the notifier of the account `notify` names. It waits for the first sync, so a
/// reply that came meanwhile cancels an "if no reply" reminder before it would fire, and looks
/// again whenever the store changes.
pub(super) async fn reminder_loop(
    store: Store,
    clock: Arc<std::sync::RwLock<crate::Clock>>,
    mut first_sync: watch::Receiver<bool>,
    mut wakes: Wakes,
    flush: Wake,
    notify: Notify,
) {
    if first_sync.wait_for(|synced| *synced).await.is_err() {
        return;
    }
    let mut changes = store.changes();
    // A wake counts as busy until the look it asked for is done.
    let mut busy: Option<Busy> = None;
    loop {
        let now = clock
            .read()
            .expect("the clock lock is never poisoned")
            .now()
            .timestamp();
        match store.return_reminders(now).await {
            Ok(returned) if !returned.is_empty() => {
                flush.wake();
                for thread in &returned {
                    notify_returned(&store, &notify, thread).await;
                }
            }
            Ok(_) | Err(exo_store::Error::NotSynced(_)) => {}
            Err(error) => log::warn!("bringing reminders back: {error}"),
        }
        let next = store.next_reminder().await.unwrap_or_else(|error| {
            log::warn!("reading reminders: {error}");
            None
        });
        let wait = next.map_or(REMINDER_RECHECK, |at| {
            Duration::try_from(at.duration_since(now))
                .unwrap_or_default()
                .min(REMINDER_RECHECK)
        });
        drop(busy.take());
        busy = tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
                None
            }
            woken = wakes.next() => match woken {
                Some(woken) => Some(woken),
                None => return,
            },
            () = tokio::time::sleep(wait) => None,
        };
    }
}

/// While the connection is down, checks every so often whether it's back; checks at once when
/// the OS says the network changed; and when it comes back, wakes sync and the outbox.
pub(super) async fn connection_loop(
    store: Store,
    presence: watch::Receiver<Presence>,
    changes: Wakes,
    sync_now: Wake,
    flush: Wake,
) {
    let connection = store.connection();
    drive_connection_loop(
        connection,
        presence,
        changes,
        sync_now,
        flush,
        || {
            let store = store.clone();
            async move { store.network_changed().await }
        },
        || {
            let store = store.clone();
            async move { store.check_connection().await }
        },
    )
    .await;
}

async fn drive_connection_loop<NetFut, CheckFut>(
    mut connection: watch::Receiver<Connection>,
    mut presence: watch::Receiver<Presence>,
    mut changes: Wakes,
    sync_now: Wake,
    flush: Wake,
    mut on_network_changed: impl FnMut() -> NetFut,
    mut on_check_connection: impl FnMut() -> CheckFut,
) where
    NetFut: std::future::Future<Output = ()>,
    CheckFut: std::future::Future<Output = ()>,
{
    let mut down_since: Option<tokio::time::Instant> = None;
    loop {
        let down = !connection.borrow_and_update().is_online();
        if down {
            down_since.get_or_insert_with(tokio::time::Instant::now);
        } else if down_since.take().is_some() {
            sync_now.wake();
            flush.wake();
        }
        let every = check_every(*presence.borrow_and_update());
        let deadline = down_since.map(|since| since + every);
        tokio::select! {
            changed = connection.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = presence.changed(), if down => {
                if changed.is_err() {
                    return;
                }
            }
            woken = changes.next() => {
                let Some(busy) = woken else { return };
                on_network_changed().await;
                if down_since.is_some() {
                    down_since = Some(tokio::time::Instant::now());
                }
                drop(busy);
            }
            () = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                }
            }, if deadline.is_some() => {
                on_check_connection().await;
                down_since = Some(tokio::time::Instant::now());
            }
        }
    }
}

/// Mail's contacts with Google's added (unscored, unless mail scored them) and lending their
/// names to addresses mail has none for.
fn merge_contacts<'a>(
    mail: &[ScoredContact],
    google: impl IntoIterator<Item = &'a exo_store::GoogleContact>,
) -> Vec<ScoredContact> {
    let mut book = mail.to_vec();
    let mut known: HashMap<String, usize> = book
        .iter()
        .enumerate()
        .map(|(at, contact)| (contact.email.to_lowercase(), at))
        .collect();
    for contact in google {
        match known.get(&contact.email.to_lowercase()) {
            Some(&at) => {
                if book[at].name.is_none() {
                    book[at].name.clone_from(&contact.name);
                }
            }
            None => {
                known.insert(contact.email.to_lowercase(), book.len());
                book.push(ScoredContact {
                    email: contact.email.clone(),
                    name: contact.name.clone(),
                    score: 0.0,
                });
            }
        }
    }
    book
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offline() -> Connection {
        Connection {
            outage: Some(exo_store::Outage::Offline),
            disk_low: false,
        }
    }

    fn requests() -> Requests {
        let (sender, _receiver) = mpsc::unbounded_channel();
        Requests {
            sender,
            busy: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn self_initiated_probe_reconnect_wakes_sync_and_flush() {
        let (conn_tx, conn_rx) = watch::channel(offline());
        let (_presence_tx, presence_rx) = watch::channel(Presence::Focused);
        let reqs = requests();
        let (_changes_wake, changes) = Wake::new(&reqs);
        let (sync_wake, mut sync_wakes) = Wake::new(&reqs);
        let (flush_wake, mut flush_wakes) = Wake::new(&reqs);

        let probe_conn = conn_tx.clone();
        let task = tokio::spawn(drive_connection_loop(
            conn_rx,
            presence_rx,
            changes,
            sync_wake,
            flush_wake,
            || async {},
            move || {
                let probe_conn = probe_conn.clone();
                async move {
                    let _ = probe_conn.send(Connection::default());
                }
            },
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(CHECK_VISIBLE).await;
        let sync_busy = tokio::time::timeout(Duration::from_secs(1), sync_wakes.next())
            .await
            .expect("sync woken after probe reconnect");
        let flush_busy = tokio::time::timeout(Duration::from_secs(1), flush_wakes.next())
            .await
            .expect("flush woken after probe reconnect");
        assert!(sync_busy.is_some());
        assert!(flush_busy.is_some());

        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn network_changed_reconnect_wakes_sync_and_flush() {
        let (conn_tx, conn_rx) = watch::channel(offline());
        let (_presence_tx, presence_rx) = watch::channel(Presence::Background);
        let reqs = requests();
        let (changes_wake, changes) = Wake::new(&reqs);
        let (sync_wake, mut sync_wakes) = Wake::new(&reqs);
        let (flush_wake, mut flush_wakes) = Wake::new(&reqs);

        let net_conn = conn_tx.clone();
        let task = tokio::spawn(drive_connection_loop(
            conn_rx,
            presence_rx,
            changes,
            sync_wake,
            flush_wake,
            move || {
                let net_conn = net_conn.clone();
                async move {
                    let _ = net_conn.send(Connection::default());
                }
            },
            || async {},
        ));

        tokio::task::yield_now().await;
        changes_wake.wake();
        let sync_busy = tokio::time::timeout(Duration::from_secs(1), sync_wakes.next())
            .await
            .expect("sync woken after network_changed reconnect");
        let flush_busy = tokio::time::timeout(Duration::from_secs(1), flush_wakes.next())
            .await
            .expect("flush woken after network_changed reconnect");
        assert!(sync_busy.is_some());
        assert!(flush_busy.is_some());

        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn focusing_hidden_offline_window_resets_probe_from_60s_to_10s() {
        let (conn_tx, conn_rx) = watch::channel(offline());
        let (presence_tx, presence_rx) = watch::channel(Presence::Background);
        let reqs = requests();
        let (_changes_wake, changes) = Wake::new(&reqs);
        let (sync_wake, mut sync_wakes) = Wake::new(&reqs);
        let (flush_wake, mut flush_wakes) = Wake::new(&reqs);
        let checks = Arc::new(AtomicUsize::new(0));

        let probe_conn = conn_tx.clone();
        let probe_checks = checks.clone();
        let task = tokio::spawn(drive_connection_loop(
            conn_rx,
            presence_rx,
            changes,
            sync_wake,
            flush_wake,
            || async {},
            move || {
                let probe_conn = probe_conn.clone();
                let probe_checks = probe_checks.clone();
                async move {
                    probe_checks.fetch_add(1, Ordering::SeqCst);
                    let _ = probe_conn.send(Connection::default());
                }
            },
        ));

        // While hidden (60s cadence), 2 seconds is not enough to trigger a probe.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(checks.load(Ordering::SeqCst), 0);

        // Focusing switches the cadence to 10s, so advancing 9s more (11s total, well under 60s)
        // triggers the probe and wakes sync + flush on reconnect.
        let _ = presence_tx.send(Presence::Focused);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;

        let sync_busy = tokio::time::timeout(Duration::from_secs(1), sync_wakes.next())
            .await
            .expect("sync woken after foreground probe reconnect");
        let flush_busy = tokio::time::timeout(Duration::from_secs(1), flush_wakes.next())
            .await
            .expect("flush woken after foreground probe reconnect");
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        assert!(sync_busy.is_some());
        assert!(flush_busy.is_some());

        task.abort();
    }
}
