use std::sync::{Arc, atomic::Ordering};

use exo_store::{
    Alias, Connection, Filter, GmailPage, Label, ListCount, ListId, RECENT_THREADS, ScoredContact,
    Search, Store, StoredDraft, ThreadDetail, ThreadId,
};
use tokio::sync::watch;

use super::{
    ui_tasks::Requests,
    worker::{PAGE, Request, ViewId, Worker},
};
use crate::{
    mailbox::{Chips, RecentThread, ThreadSummary},
    recovery::Restarts,
};

/// The list on screen, as the async side last read it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The list.
    pub list: ListId,
    /// The filter its rows passed.
    pub filter: Option<Filter>,
    /// Its rows, newest first.
    pub threads: Vec<ThreadSummary>,
    /// How many threads the list has locally.
    pub count: Option<ListCount>,
    /// How many threads the Inbox has locally (the folder drawer's count).
    pub inbox: Option<ListCount>,
    /// How many threads the inbox's tabs have locally: Inbox, Important, Other and each enabled
    /// split.
    pub counts: crate::splits::Counts,
    /// Whether some thread is muted, which makes the folder drawer list Muted.
    pub muted: bool,
    /// More rows exist past `threads`, locally or in Gmail.
    pub more: bool,
    /// The list is still being fetched (the first sync, or a backfill of it).
    pub loading: bool,
    /// Whether every thread of the list is stored, meaning Gmail has none older than the store has.
    pub complete: bool,
    /// The account's labels.
    pub labels: Vec<Label>,
    /// The contact pane's address, lower-cased, and that contact's latest conversations in local
    /// mail (the pane's Mail section), newest first.
    pub contact: Option<(String, Vec<RecentThread>)>,
    /// Whether the message on screen for the contact pane's address passed DMARC.
    pub(crate) contact_brand: Option<(String, Option<String>, Option<bool>)>,
    /// The open thread, with every message, when it's stored locally.
    pub thread: Option<Arc<ThreadDetail>>,
    /// Those of the open thread's messages stored without their body, which show placeholders until
    /// the bodies are in.
    pub bodiless: Vec<String>,
    /// Every draft compose has saved.
    pub drafts: Vec<StoredDraft>,
    /// The account's send-as aliases, with their signatures.
    pub aliases: Vec<Alias>,
    /// Everyone compose can suggest, once [`Account::load_address_book`](super::Account::load_address_book) has read them.
    pub address_book: Option<Arc<Vec<ScoredContact>>>,
    /// The search being run, if any.
    pub search: Option<Arc<SearchResults>>,
    /// How many [`Account::apply`](super::Account::apply) calls the snapshot reflects.
    pub applied: u64,
    /// The connection to Gmail, as the offline bar shows it.
    pub connection: Connection,
    /// What's on its way to Gmail, as the footer counts it ([`Store::uploading`]), once read.
    pub uploading: Option<exo_store::Unsent>,
}

/// What a search has found.
#[derive(Clone, Debug)]
pub struct SearchResults {
    /// The query.
    pub query: String,
    /// The conversations, newest first.
    pub threads: Vec<ThreadSummary>,
    /// More can be loaded past them.
    pub more: bool,
    /// Both the local index and Gmail have answered.
    pub finished: bool,
    /// Gmail couldn't be reached, so there may be more than the local index found.
    pub offline: bool,
}

/// What one of the account's views (a page's) shows, which its snapshots follow.
pub(super) struct ViewState {
    pub(super) snapshot: watch::Sender<Arc<Snapshot>>,
    pub(super) list: ListId,
    /// The thread open, if one is.
    pub(super) open: Option<ThreadId>,
    /// How many of the list's rows it reads.
    pub(super) limit: usize,
    /// Whose conversations snapshots carry, and the conversation on screen for them.
    pub(super) contact: Option<(String, Option<ThreadId>)>,
    /// The search being run.
    pub(super) search: Option<Searching>,
    /// The search run ahead of being asked for, which snapshots don't show.
    pub(super) preload: Option<Searching>,
    /// The search whose preload is waiting on a Gmail page, maybe a search since replaced. A
    /// preload asks Gmail only when no such page is on its way.
    pub(super) preloading: Option<u64>,
    /// The list's filter.
    pub(super) filter: Option<Filter>,
    /// Threads the filtered list has shown, which stay until the list or filter changes
    /// (`stick`, for Unread and Starred).
    pub(super) sticky: Vec<ThreadId>,
    /// Threads starred or unstarred in Starred, which stay even once out of it until the list
    /// changes or an action dismisses them.
    pub(super) held: Vec<ThreadId>,
}

impl ViewState {
    /// A view on the inbox with nothing open, publishing on `snapshot`.
    pub(super) fn new(snapshot: watch::Sender<Arc<Snapshot>>) -> Self {
        Self {
            snapshot,
            list: ListId::Inbox,
            open: None,
            limit: PAGE,
            contact: None,
            search: None,
            preload: None,
            preloading: None,
            filter: None,
            sticky: Vec::new(),
            held: Vec::new(),
        }
    }

    /// Whether the Gmail page of search `id` is this view's: its search's, its preload's, or one it
    /// waits on.
    pub(super) fn searches(&self, id: u64) -> bool {
        self.preloading == Some(id)
            || [&self.search, &self.preload]
                .into_iter()
                .flatten()
                .any(|search| search.id == id)
    }
}

type SearchCacheKey = (u64, usize, bool, bool, i64, usize);

/// A search the worker runs: the store's session, how many results to read, and where Gmail's
/// part of it is.
pub(super) struct Searching {
    /// Which search it is, for Gmail's pages to find it.
    pub(super) id: u64,
    pub(super) session: Search,
    pub(super) raw: String,
    pub(super) limit: usize,
    pub(super) snippet_tokens: i64,
    /// A Gmail page is being fetched.
    pub(super) fetching: bool,
    /// Gmail couldn't be reached.
    pub(super) offline: bool,
    /// Gmail pages merged into `session` so far.
    pub(super) gmail_pages: usize,
    /// Last evaluated `(store_rev, limit, fetching, offline, snippet_tokens, gmail_pages)` and
    /// its built [`SearchResults`].
    pub(super) cached: Option<(SearchCacheKey, Arc<SearchResults>)>,
}

impl Searching {
    /// A new search for `raw`, numbered from `searches`, the account's count.
    pub(super) fn new(searches: &mut u64, raw: &str, session: Search, snippet_tokens: i64) -> Self {
        *searches += 1;
        Self {
            id: *searches,
            session,
            raw: raw.to_owned(),
            limit: PAGE,
            snippet_tokens,
            fetching: false,
            offline: false,
            gmail_pages: 0,
            cached: None,
        }
    }
}

/// Whether two searches are the same list, meaning the same query, one trailing space aside.
pub(super) fn same_search(a: &str, b: &str) -> bool {
    a.strip_suffix(' ').unwrap_or(a) == b.strip_suffix(' ').unwrap_or(b)
}

/// How many words a search excerpt holds at a window width.
pub fn snippet_tokens(window_width: f32) -> i64 {
    match window_width {
        width if width <= 1200.0 => 10,
        width if width <= 1700.0 => 15,
        width if width <= 2200.0 => 26,
        _ => 35,
    }
}

/// Whether `email`'s message in `thread` (or else their newest stored message) passed DMARC,
/// along with the headers that passed when it did.
async fn contact_brand_status(
    store: &Store,
    email: &str,
    thread: Option<&ThreadId>,
) -> Result<(Option<bool>, Option<Vec<exo_store::Header>>), exo_store::Error> {
    if let Some(id) = thread
        && let Some(detail) = store.thread_headers(id).await?
    {
        let message = detail.messages.iter().rev().find(|message| {
            !message.is_draft()
                && message
                    .from
                    .as_ref()
                    .is_some_and(|from| from.email.eq_ignore_ascii_case(email))
        });
        if let Some(message) = message {
            let passed = crate::brands::passed_for(&message.headers, email);
            return Ok((Some(passed), passed.then(|| message.headers.clone())));
        }
    }
    match store.latest_headers_from(email).await? {
        Some(headers) => {
            let passed = crate::brands::passed_for(&headers, email);
            Ok((Some(passed), passed.then_some(headers)))
        }
        None => Ok((None, None)),
    }
}

impl Worker {
    /// Asks Gmail for the first page of the search `view` preloads, once no other preload's page is
    /// on its way. Preloads run ahead as the user types, so they'd otherwise pile up behind Gmail's
    /// pace.
    pub(super) fn fetch_preload_page(
        store: &Store,
        requests: &Requests,
        restarts: &Restarts,
        view: &mut ViewState,
    ) {
        let Some(preload) = view
            .preload
            .as_mut()
            .filter(|preload| !preload.offline && preload.session.next_page().is_none())
        else {
            return;
        };
        if view.preloading.is_none() && Self::fetch_page(store, requests, restarts, preload) {
            view.preloading = Some(preload.id);
        }
    }

    /// Fetches the next page of `search` in the background, unless one is on its way or Gmail has
    /// no more. Returns whether it did.
    pub(super) fn fetch_page(
        store: &Store,
        requests: &Requests,
        restarts: &Restarts,
        search: &mut Searching,
    ) -> bool {
        if search.fetching || search.session.gmail_done() || search.session.query().is_empty() {
            return false;
        }
        search.fetching = true;
        let (store, requests, busy, session, id) = (
            store.clone(),
            requests.clone(),
            requests.busy(),
            search.session.clone(),
            search.id,
        );
        restarts.spawn_task(&tokio::runtime::Handle::current(), async move {
            let page = match store.search_gmail(&session).await {
                Ok(page) => Some(page),
                Err(error) => {
                    log::warn!("searching Gmail: {error}");
                    // An unreachable or throttled Gmail leaves the results partial. Any other error
                    // ends Gmail's part of the search.
                    if matches!(
                        error,
                        exo_store::Error::Offline(_) | exo_store::Error::Throttled { .. }
                    ) {
                        None
                    } else {
                        Some(GmailPage::default())
                    }
                }
            };
            requests.post(Request::SearchPage(id, page));
            drop(busy);
        });
        true
    }

    /// Puts `email`'s latest conversations in snapshots from now on, read from local mail at
    /// once, and asks Gmail for older ones the first time this session (they arrive as a store
    /// change).
    pub(super) async fn show_contact(
        &mut self,
        view: ViewId,
        email: String,
        thread: Option<ThreadId>,
    ) {
        let (allowed, passing_headers) =
            match contact_brand_status(&self.store, &email, thread.as_ref()).await {
                Ok(status) => status,
                Err(error) => {
                    log::warn!("checking a contact's authentication headers: {error}");
                    (Some(false), None)
                }
            };
        // Their brand's logo, earned when the message on screen passed DMARC, shows once it's had.
        // It's asked for each time they show until then, so a change to the images setting applies.
        if self.brands.logo(&email).is_none()
            && let Some(headers) = passing_headers
        {
            let (brands, sender) = (self.brands.clone(), email.clone());
            self.restarts
                .spawn_task(&tokio::runtime::Handle::current(), async move {
                    brands.logo_for_sender(&sender, &headers).await;
                });
        }
        if self.filled.insert(email.clone()) {
            let (store, requests, busy, email) = (
                self.store.clone(),
                self.requests.clone(),
                self.requests.busy(),
                email.clone(),
            );
            self.restarts
                .spawn_task(&tokio::runtime::Handle::current(), async move {
                    if let Err(error) = store.fetch_recent_threads(&email).await {
                        log::warn!("loading a contact's conversations: {error}");
                        requests.post(Request::ContactFillFailed(email));
                    }
                    drop(busy);
                });
        }
        let threads = match self
            .store
            .recent_threads(&email, RECENT_THREADS as usize)
            .await
        {
            Ok(threads) => threads.into_iter().map(RecentThread::new).collect(),
            Err(error) => return log::error!("reading a contact's conversations: {error}"),
        };
        let Some(view) = self.views.get_mut(&view) else {
            return;
        };
        view.contact = Some((email.clone(), thread.clone()));
        let mut snapshot = Snapshot::clone(&view.snapshot.borrow());
        snapshot.contact = Some((email.clone(), threads));
        snapshot.contact_brand = Some((email, thread.map(|id| id.as_str().to_owned()), allowed));
        view.snapshot.send_replace(Arc::new(snapshot));
        self.repaint.request();
    }

    /// Reads what each view shows and publishes it. What belongs to the account is read once, then
    /// each view's list, conversation, contact and search.
    pub(super) async fn publish(&mut self) {
        let rev = *self.changes.borrow_and_update();
        self.published.store(rev, Ordering::SeqCst);
        let store = &self.store;
        let settings = &self.settings;
        let account = async {
            let labels = store.labels().await?;
            let inbox = store.count(&ListId::Inbox).await?;
            let mut counts = crate::splits::Counts::new();
            for list in [ListId::Important, ListId::Other].into_iter().chain(
                settings
                    .enabled_splits()
                    .map(|split| ListId::Split(split.id.clone())),
            ) {
                counts.insert(list.clone(), store.count(&list).await?);
            }
            counts.insert(ListId::Inbox, inbox);
            let muted = !store
                .list(&ListId::Muted, None, 1)
                .await?
                .threads
                .is_empty();
            let drafts = store.drafts().await?;
            let uploading = store.uploading().await?;
            let aliases = store.aliases().await?;
            Ok::<_, exo_store::Error>((labels, inbox, counts, muted, drafts, uploading, aliases))
        };
        let (labels, inbox, counts, muted, drafts, uploading, aliases) = match account.await {
            Ok(read) => read,
            Err(error) => {
                self.health.failed(&error);
                return log::error!("reading the account: {error}");
            }
        };
        let chips = Chips::new(&labels);
        let focused = self.focused;
        for (&view_id, view) in &mut self.views {
            let is_focused = view_id == focused;
            let read = async {
                let page = store
                    .list_filtered(
                        &view.list,
                        view.filter,
                        &view.sticky,
                        &view.held,
                        None,
                        view.limit,
                    )
                    .await?;
                let count = match counts.get(&view.list).copied() {
                    Some(count) => count,
                    None => store.count(&view.list).await?,
                };
                let (thread, bodiless) = match &view.open {
                    Some(id) => (store.thread(id).await?, store.bodiless(id).await?),
                    None => (None, Vec::new()),
                };
                let contact = match &view.contact {
                    Some((email, thread_id)) => {
                        let (allowed, _) =
                            contact_brand_status(store, email, thread_id.as_ref()).await?;
                        Some((
                            email.clone(),
                            thread_id.as_ref().map(|id| id.as_str().to_owned()),
                            allowed,
                            store.recent_threads(email, RECENT_THREADS as usize).await?,
                        ))
                    }
                    None => None,
                };
                let search = match &mut view.search {
                    Some(search) => {
                        let key: SearchCacheKey = (
                            rev,
                            search.limit,
                            search.fetching,
                            search.offline,
                            search.snippet_tokens,
                            search.gmail_pages,
                        );
                        if let Some((cached_key, cached)) = &search.cached
                            && (*cached_key == key || !is_focused)
                        {
                            Some(cached.clone())
                        } else {
                            let found = store
                                .search(
                                    &mut search.session,
                                    search.limit,
                                    !search.offline,
                                    search.snippet_tokens,
                                )
                                .await?;
                            let gmail_done = search.session.gmail_done();
                            let built = Arc::new(SearchResults {
                                query: search.raw.clone(),
                                more: found.more_local || !(gmail_done || search.offline),
                                finished: !search.fetching && (gmail_done || search.offline),
                                offline: search.offline,
                                threads: found
                                    .results
                                    .into_iter()
                                    .map(|result| {
                                        ThreadSummary::search_result(
                                            result,
                                            &found.labels,
                                            &found.names,
                                            &chips,
                                        )
                                    })
                                    .collect(),
                            });
                            search.cached = Some((key, built.clone()));
                            Some(built)
                        }
                    }
                    None => None,
                };
                Ok::<_, exo_store::Error>((page, count, (thread, bodiless), contact, search))
            };
            let (page, count, (thread, bodiless), contact, search) = match read.await {
                Ok(read) => read,
                Err(error) => {
                    self.health.failed(&error);
                    return log::error!("reading {:?}: {error}", view.list);
                }
            };
            if matches!(view.filter, Some(Filter::Unread | Filter::Starred)) {
                for thread in &page.threads {
                    if !view.sticky.contains(&thread.id) {
                        view.sticky.push(thread.id.clone());
                    }
                }
            }
            let loading =
                self.backfilling.contains(&view.list) || (!self.synced && page.threads.is_empty());
            let (contact, contact_brand) = match contact {
                Some((email, thread_id, allowed, threads)) => (
                    Some((
                        email.clone(),
                        threads.into_iter().map(RecentThread::new).collect(),
                    )),
                    Some((email, thread_id, allowed)),
                ),
                None => (None, None),
            };
            let snapshot = Snapshot {
                threads: page
                    .threads
                    .into_iter()
                    .map(|thread| ThreadSummary::new(thread, &view.list, &chips))
                    .collect(),
                list: view.list.clone(),
                filter: view.filter,
                count: Some(count),
                inbox: Some(inbox),
                counts: counts.clone(),
                muted,
                more: page.next.is_some() || page.more_remote,
                loading,
                complete: !page.more_remote,
                labels: labels.clone(),
                contact,
                contact_brand,
                thread: thread.map(Arc::new),
                bodiless,
                drafts: drafts.clone(),
                aliases: aliases.clone(),
                address_book: self.address_book.clone(),
                search,
                applied: self.applied,
                connection: *self.connection.borrow(),
                uploading: Some(uploading),
            };
            view.snapshot.send_replace(Arc::new(snapshot));
        }
        self.health.ok();
        self.repaint.request();
    }
}
