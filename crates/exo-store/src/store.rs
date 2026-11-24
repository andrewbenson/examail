//! The store handle: opening an account, reads, and the shared plumbing the outbox and sync
//! use (transactions, change notification, recomputing list membership).

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use gmail::{Client, Endpoints, TokenProvider};
use toasty::{Db, Executor, Transaction};
use tokio::sync::{Mutex, MutexGuard, watch};

use crate::api::{Api, Failure};
use crate::connection::{Connection, Debounce, Outage};
use crate::db::{
    self, AttachmentRow, BodyRow, IDENTITY, LabelRow, ListCursor, ListEntry, MessageRow, OutboxRow,
    ParticipantEntry, ReminderRow,
};
use crate::error::{Error, Result};
use crate::ids::labels::{INBOX, UNREAD};
use crate::ids::{Filter, ListId, ThreadId};
use crate::model::{Cursor, Label, LabelKind, ListCount, RecentThread, ThreadDetail, ThreadPage};
use crate::queue::FlushGate;
use crate::quota::{Lane, Quota};
use crate::settings::{self, Settings};
use crate::split::Splits;
use crate::view::{Identity, Router, ThreadView};

/// An account's local mail store: a SQLite cache of Gmail, the sync engine that fills it, and
/// the outbox that sends the user's changes. Cheap to clone; clones share everything.
///
/// Nothing runs in the background. The app drives it:
/// - [`sync`](Self::sync) on a timer (about every 20–30 s while focused, 60 s while visible,
///   5 min in the background), on focus, on network recovery, after
///   [`flush`](Self::flush) sends something, and repeatedly while it reports backfilling;
/// - [`flush`](Self::flush) after [`apply`](Self::apply)/[`undo`](Self::undo) and whenever a
///   flush reports a retry time;
/// - [`load_bodies`](Self::load_bodies) when a thread opens whose bodies aren't stored
///   ([`bodies_stored`](Self::bodies_stored)), calling [`reading`](Self::reading) first (sync
///   stores most mail for its rows alone);
/// - [`evict`](Self::evict) every day, which lets old mail go;
/// - [`changes`](Self::changes) to know when to re-read lists and threads;
/// - [`check_connection`](Self::check_connection) every so often while
///   [`connection`](Self::connection) says something is down, and
///   [`network_changed`](Self::network_changed) when the OS says the network changed.
#[derive(Clone)]
pub struct Store {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Db,
    pub api: Api,
    /// The account's address.
    pub account: String,
    /// The clients of its other calendar accounts that are signed in, by address
    /// ([`Store::add_calendar_account`]).
    pub calendar_clients: RwLock<HashMap<String, Client>>,
    /// The calendar account each person's calendar was last read through
    /// ([`Store::attendee_events`]).
    pub attendee_accounts: RwLock<HashMap<String, String>>,
    /// Serializes write transactions. SQLite has one writer, and this lock keeps that writer from
    /// ever being contended or upgraded mid-transaction.
    write: Mutex<()>,
    /// One sync or backfill page at a time.
    pub sync: Mutex<()>,
    /// Labels and identity were refreshed since this store was opened (or rebuilt).
    pub bootstrapped: AtomicBool,
    /// One flush at a time, so no queue is sent twice at once.
    pub flush: FlushGate,
    /// Serializes draft pushes and history cleanup apart from the triage outbox so multi-MB
    /// draft uploads never head-of-line block triage flushes.
    pub draft_flush: FlushGate,
    /// Per-store flush and in-flight create tracking for the calendar outbox.
    pub calendar_outbox: crate::calendar::outbox::CalendarOutboxSync,
    /// Serializes one-click unsubscribe flushes so concurrent flushes cannot race on `kv`.
    pub unsubscribe_flush: FlushGate,
    /// Held across `drafts.send` and by undo send, so an undo either comes before the send or
    /// finds out it went.
    pub sending: Mutex<()>,
    pub identity: RwLock<Identity>,
    /// The account's splits, as last saved.
    splits: RwLock<Arc<Splits>>,
    /// The list on screen, whose background pass goes first in every round.
    pub on_screen: RwLock<Option<ListId>>,
    /// How many rows of the list on screen the app has loaded. Sync fetches the bodies of all of
    /// them.
    pub loaded_rows: std::sync::atomic::AtomicU32,
    /// Where Gmail's parts are kept, once the app says ([`Store::cache_parts`]).
    pub parts: std::sync::OnceLock<crate::parts::PartsCache>,
    /// What works out the search index's visible text, once the app says
    /// ([`Store::index_text_with`]).
    pub index_text: std::sync::OnceLock<crate::IndexText>,
    /// The spans of the calendar read this session (epoch milliseconds). Eviction keeps their
    /// events, so the calendar shows them without fetching them again.
    pub calendar_read: std::sync::Mutex<Vec<(i64, i64)>>,
    /// The conversation open on screen, whose bodies eviction leaves alone. A batch of eviction
    /// holds this lock ([`Store::reading`]).
    pub reading: Mutex<Option<String>>,
    /// Inbox threads whose bodies Gmail wouldn't give this session. They wait until they're
    /// opened.
    pub skipped_bodies: std::sync::Mutex<HashSet<String>>,
    /// The outbox rows given up on this session, whose mutation never reached Gmail, so undoing
    /// one has nothing to take back. Row ids are never reused (`AUTOINCREMENT`), and an
    /// [`Undo`](crate::Undo) lives no longer than the session.
    pub given_up: std::sync::Mutex<HashSet<u64>>,
    changes: watch::Sender<u64>,
    /// How far history is applied ([`Store::sync_progress`]).
    pub progress: watch::Sender<crate::SyncProgress>,
    /// The folder draft attachments' bytes are kept in.
    pub files: PathBuf,
    /// When each draft was last written to Gmail this session, which paces its next write.
    pub written: std::sync::Mutex<HashMap<String, jiff::Timestamp>>,
    /// What Gmail holds of each draft Exo wrote this session: the message the write made and its
    /// [`content`](crate::drafts::content). A save that Gmail already has matches that content.
    pub acked: std::sync::Mutex<HashMap<String, (String, u64)>>,
    /// How each draft's files stand with Gmail ([`Store::uploads`]).
    pub uploads: watch::Sender<crate::drafts::Uploads>,
    /// How to take back the reminder that each held send put on its thread this session, as undo
    /// send does.
    pub given: std::sync::Mutex<HashMap<String, crate::outbox::Undo>>,
    /// One held send's reminder given or taken back at a time, so none is given twice.
    pub giving: Mutex<()>,
    /// [`Options::draft_pace`] and [`Options::draft_files_pace`].
    pub draft_pace: Duration,
    pub draft_files_pace: Duration,
}

/// Tuning knobs. The defaults are what the app runs with: the backoff, Gmail's quota, the offline
/// debounce and the drafts' pace. Tests shorten the backoff and the debounce, and turn the drafts'
/// pace off or shorten it.
#[derive(Debug, Clone)]
pub struct Options {
    /// First retry delay; later retries double it (capped at 32 s per call, 120 s in the
    /// outbox).
    pub backoff: Duration,
    /// The quota that Gmail calls are paced under. `None` sends them as they come, for a server
    /// that enforces none (like mock-servers unless asked to).
    pub quota: Option<Quota>,
    /// The shortest time an outage lasts before [`Store::connection`] says it's over.
    pub debounce: Debounce,
    /// How soon after a draft is written to Gmail its next edits can be written, at the earliest.
    /// Each save is kept on the device at once, and the latest one goes once the pace allows.
    pub draft_pace: Duration,
    /// [`draft_pace`](Self::draft_pace) for a draft with files, which every write uploads again.
    pub draft_files_pace: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            backoff: Duration::from_secs(1),
            quota: Some(Quota::GMAIL),
            debounce: Debounce::default(),
            draft_pace: Duration::from_secs(5),
            draft_files_pace: Duration::from_secs(30),
        }
    }
}

impl Store {
    /// Opens `account`'s store, one SQLite file in `dir`, talking to Gmail at `endpoints`.
    /// Calls to Google are paced under Gmail's quota. Calls anywhere else (mock-servers) aren't,
    /// since the mock enforces no quota unless asked to and tests shouldn't wait one out.
    ///
    /// # Errors
    /// When the directory or database can't be created or read.
    pub async fn open(
        dir: impl AsRef<Path>,
        account: &str,
        endpoints: Endpoints,
        tokens: impl TokenProvider + 'static,
    ) -> Result<Self> {
        let quota = (endpoints == Endpoints::google()).then_some(Quota::GMAIL);
        Self::open_with(
            dir,
            account,
            endpoints,
            tokens,
            Options {
                quota,
                ..Options::default()
            },
        )
        .await
    }

    /// [`open`](Self::open) with non-default [`Options`].
    ///
    /// # Errors
    /// When the directory or database can't be created or read.
    pub async fn open_with(
        dir: impl AsRef<Path>,
        account: &str,
        endpoints: Endpoints,
        tokens: impl TokenProvider + 'static,
        options: Options,
    ) -> Result<Self> {
        check_account(account)?;
        let mut db = db::open(&dir.as_ref().join(format!("{account}.sqlite3"))).await?;
        let identity = db::kv_get(&mut db, IDENTITY).await?.unwrap_or_default();
        let splits = load_splits(&mut db).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                api: Api::new(
                    Client::new(endpoints, tokens),
                    options.backoff,
                    options.quota,
                    options.debounce,
                    dir.as_ref(),
                ),
                account: account.to_owned(),
                calendar_clients: RwLock::default(),
                attendee_accounts: RwLock::default(),
                write: Mutex::new(()),
                sync: Mutex::new(()),
                bootstrapped: AtomicBool::new(false),
                flush: FlushGate::default(),
                draft_flush: FlushGate::default(),
                calendar_outbox: crate::calendar::outbox::CalendarOutboxSync::default(),
                unsubscribe_flush: FlushGate::default(),
                sending: Mutex::new(()),
                identity: RwLock::new(identity),
                splits: RwLock::new(Arc::new(splits)),
                on_screen: RwLock::new(None),
                loaded_rows: std::sync::atomic::AtomicU32::new(0),
                parts: std::sync::OnceLock::new(),
                index_text: std::sync::OnceLock::new(),
                calendar_read: std::sync::Mutex::default(),
                reading: Mutex::new(None),
                skipped_bodies: std::sync::Mutex::default(),
                given_up: std::sync::Mutex::default(),
                changes: watch::channel(0).0,
                progress: watch::Sender::new(crate::SyncProgress::default()),
                files: dir.as_ref().join(format!("{account}.files")),
                written: std::sync::Mutex::default(),
                acked: std::sync::Mutex::default(),
                uploads: watch::Sender::new(crate::drafts::Uploads::new()),
                given: std::sync::Mutex::default(),
                giving: Mutex::new(()),
                draft_pace: options.draft_pace,
                draft_files_pace: options.draft_files_pace,
            }),
        })
    }

    /// Whether `account` has a store in `dir`, which it does when it has opened on this Mac since
    /// it last signed out.
    #[must_use]
    pub fn exists(dir: impl AsRef<Path>, account: &str) -> bool {
        check_account(account).is_ok() && dir.as_ref().join(format!("{account}.sqlite3")).exists()
    }

    /// Deletes `account`'s store in `dir` (its database, its drafts' attachments and any parts
    /// kept beside them), as signing out does. Close every [`Store`] on it first. A store that
    /// isn't there deletes fine.
    ///
    /// # Errors
    /// [`Error::InvalidAccount`], or [`Error::Delete`] for a file the OS wouldn't delete.
    pub fn delete(dir: impl AsRef<Path>, account: &str) -> Result<()> {
        check_account(account)?;
        let dir = dir.as_ref();
        let gone = |path: PathBuf, result: std::io::Result<()>| match result {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(Error::Delete {
                path,
                source: error,
            }),
            _ => Ok(()),
        };
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let file = dir.join(format!("{account}.sqlite3{suffix}"));
            gone(file.clone(), std::fs::remove_file(&file))?;
        }
        // A store kept beside its files keeps its parts there too (mock-servers', the app's).
        for folder in ["files", "parts"] {
            let folder = dir.join(format!("{account}.{folder}"));
            gone(folder.clone(), std::fs::remove_dir_all(&folder))?;
        }
        Ok(())
    }

    /// Clears `account`'s cache in `dir` (Clear Cache and Restart). Its database is made again,
    /// empty, and the next sync downloads its mail again. When the old database still reads,
    /// what Gmail doesn't have is kept: the settings, the drafts (held and scheduled sends and
    /// their files among them), the queued changes, the reminders and the unsubscribes to retry.
    /// Close every [`Store`] on it first.
    ///
    /// # Errors
    /// [`Error::InvalidAccount`], or when the old files can't be moved aside or the new database
    /// made.
    pub async fn clear_cache(dir: impl AsRef<Path>, account: &str) -> Result<()> {
        check_account(account)?;
        db::clear(&dir.as_ref().join(format!("{account}.sqlite3"))).await
    }

    /// The connection to Gmail as the offline bar shows it, which is what the last calls found
    /// down. It changes as calls fail and get through, not with [`changes`](Self::changes).
    #[must_use]
    pub fn connection(&self) -> watch::Receiver<Connection> {
        self.inner.api.connection()
    }

    /// Checks whether Gmail answers a request for its profile. The app checks every 10 s while
    /// something is down (60 s while hidden). An answer brings back what has been down long
    /// enough.
    pub async fn check_connection(&self) {
        if let Err(failure) = self.inner.api.ping().await {
            log::debug!("checking Gmail: {}", failure.message());
        }
    }

    /// Checks whether Gmail answers after the OS says the network changed. When nothing does,
    /// the connection is [`Outage::Offline`] until something does.
    pub async fn network_changed(&self) {
        if let Err(Failure::Offline(error)) = self.inner.api.ping().await {
            log::info!("the network changed and Gmail can't be reached: {error}");
            self.inner.api.down(Outage::Offline);
        }
    }

    /// Calls a Google API other than Gmail and Calendar (People's contacts) through the store's
    /// retry policy, `attempts` times at most. Like Calendar's calls, it tells the connection
    /// how the call ended, and no answer means the network is down.
    ///
    /// # Errors
    /// [`Error::Offline`] and the like, as for Gmail's calls.
    pub async fn google<'a, T: serde::de::DeserializeOwned + 'a>(
        &'a self,
        attempts: u32,
        make: impl FnMut(&'a Client) -> gmail::Call<'a, T>,
    ) -> Result<T> {
        Ok(self.inner.api.request(attempts, make).await?)
    }

    /// A revision counter that ticks after every change the UI can see (synced mail, applied
    /// or persisted mutations, undos). Re-read what's on screen when it changes.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Up to `limit` threads of `list` after `after`, newest first (threads of the same date sort
    /// by id, the higher first), with pending mutations applied. Only the range backfilled without
    /// gaps is returned; see [`ThreadPage::more_remote`].
    ///
    /// # Errors
    /// On database failures.
    pub async fn list(
        &self,
        list: &ListId,
        after: Option<&Cursor>,
        limit: usize,
    ) -> Result<ThreadPage> {
        self.list_filtered(list, None, &[], &[], after, limit).await
    }

    /// [`list`](Self::list) narrowed by `filter`, with sticky rows. It returns only threads also
    /// in the filter's list, plus those in `keep` as long as they're still in `list` (a thread
    /// marked read in an Unread view stays until the view changes). It also returns the threads
    /// in `held` even once they've left `list`, where they'd sort in it (a conversation unstarred
    /// in Starred stays until the view changes).
    ///
    /// # Errors
    /// On database failures.
    pub async fn list_filtered(
        &self,
        list: &ListId,
        filter: Option<Filter>,
        keep: &[ThreadId],
        held: &[ThreadId],
        after: Option<&Cursor>,
        limit: usize,
    ) -> Result<ThreadPage> {
        let mut db = self.inner.db.clone();
        let Some(bound) = db::coverage(&mut db, list).await? else {
            return Ok(ThreadPage {
                threads: Vec::new(),
                next: None,
                more_remote: true,
            });
        };
        let narrow = filter;
        let fields = ListEntry::fields();
        let mut filter_expr = fields
            .list_id()
            .in_list(list.members())
            .and(fields.sort().ge(bound));
        if let Some(after) = after {
            filter_expr = filter_expr.and(
                fields.sort().lt(after.sort).or(fields
                    .sort()
                    .eq(after.sort)
                    .and(fields.thread_id().lt(&after.thread))),
            );
        }
        let query = ListEntry::filter(filter_expr)
            .order_by((fields.sort().desc(), fields.thread_id().desc()));
        let mut entries = match narrow {
            None => query.limit(limit + 1).exec(&mut db).await?,
            Some(narrow) => {
                filtered_entries(&mut db, list, narrow, keep, bound, after, limit).await?
            }
        };
        let router = self.inner.router();
        // While Gmail can't be reached, the sends waiting for it are listed in Scheduled too,
        // held there like the threads in `held`.
        let waiting: Vec<ThreadId> =
            if *list == ListId::Scheduled && !self.inner.api.connection().borrow().is_online() {
                crate::drafts::marks(&mut db)
                    .await?
                    .sending
                    .into_iter()
                    .map(ThreadId::new)
                    .collect()
            } else {
                Vec::new()
            };
        let held: Vec<&ThreadId> = held.iter().chain(&waiting).collect();
        if !held.is_empty() {
            let held: Vec<&str> = held.iter().map(|id| id.as_str()).collect();
            let mut listed = HashSet::new();
            for chunk in held.chunks(db::IN_LIST_CHUNK) {
                listed.extend(
                    ListEntry::filter(
                        fields
                            .list_id()
                            .in_list(list.members())
                            .and(fields.thread_id().in_list(chunk)),
                    )
                    .select(fields.thread_id())
                    .exec(&mut db)
                    .await?,
                );
            }
            let gone: Vec<String> = held
                .iter()
                .filter(|id| !listed.contains(**id))
                .map(|id| (*id).to_owned())
                .collect();
            let sort_key = list.members()[0];
            for (thread_id, view) in load_views(&mut db, &gone).await? {
                let sort = view.sort_for(sort_key, &router.me);
                let in_page = after.is_none_or(|after| {
                    sort < after.sort || (sort == after.sort && thread_id < after.thread)
                });
                if sort >= bound && in_page {
                    entries.push(ListEntry {
                        list_id: sort_key.to_owned(),
                        thread_id,
                        sort,
                    });
                }
            }
            entries.sort_by(|a, b| (b.sort, &b.thread_id).cmp(&(a.sort, &a.thread_id)));
            entries.truncate(limit + 1);
        }
        let next = if entries.len() > limit {
            entries.truncate(limit);
            entries.last().map(|entry| Cursor {
                sort: entry.sort,
                thread: entry.thread_id.clone(),
            })
        } else {
            None
        };
        let ids: Vec<String> = entries
            .iter()
            .map(|entry| entry.thread_id.clone())
            .collect();
        let mut views = load_views(&mut db, &ids).await?;
        let attachments_by_thread = load_attachments_by_thread(&mut db, &views).await?;
        Ok(ThreadPage {
            threads: entries
                .iter()
                .filter_map(|entry| {
                    let view = views.get_mut(&entry.thread_id)?;
                    let attachments = attachments_by_thread
                        .get(&entry.thread_id)
                        .map_or(&[][..], Vec::as_slice);
                    Some(view.summary(list, entry.sort, &router, attachments))
                })
                .collect(),
            next,
            more_remote: bound != i64::MIN,
        })
    }

    /// How many local threads `list` has, and how many are unread.
    ///
    /// # Errors
    /// On database failures.
    pub async fn count(&self, list: &ListId) -> Result<ListCount> {
        let mut db = self.inner.db.clone();
        let stmt = match list {
            ListId::Inbox => toasty::sql::query(
                "SELECT count(*), coalesce(sum(EXISTS (SELECT 1 FROM list_ids u WHERE u.list_id = ?3 AND u.thread_id = d.thread_id)), 0) FROM (SELECT DISTINCT thread_id FROM list_ids WHERE list_id IN (?1, ?2)) d",
            )
            .bind(crate::ids::labels::SH_IMPORTANT)
            .bind(crate::ids::labels::SH_OTHER)
            .bind(UNREAD),
            other => toasty::sql::query(
                "SELECT count(*), coalesce(sum(EXISTS (SELECT 1 FROM list_ids u WHERE u.list_id = ?2 AND u.thread_id = l.thread_id)), 0) FROM list_ids l WHERE l.list_id = ?1",
            )
            .bind(other.key())
            .bind(UNREAD),
        };
        let (threads, unread) = db::query_rows::<(i64, i64)>(&mut db, stmt)
            .await?
            .into_iter()
            .next()
            .map_or((0, 0), |(threads, unread)| {
                (
                    usize::try_from(threads).unwrap_or(0),
                    usize::try_from(unread).unwrap_or(0),
                )
            });
        Ok(ListCount {
            threads,
            unread,
            exact: db::coverage(&mut db, list).await? == Some(i64::MIN),
        })
    }

    /// A thread with every message, body and attachment, pending mutations applied. `None`
    /// when it isn't stored locally.
    ///
    /// # Errors
    /// On database failures.
    pub async fn thread(&self, id: &ThreadId) -> Result<Option<ThreadDetail>> {
        let mut db = self.inner.db.clone();
        let Some(view) = load_view(&mut db, id.as_str()).await? else {
            return Ok(None);
        };
        // Drafts written elsewhere are included too, because the thread shows them in compose.
        let ids: Vec<String> = view.all_messages().map(|m| m.row.id.clone()).collect();
        let mut attachments = Vec::new();
        let mut bodies = Vec::new();
        for chunk in ids.chunks(db::IN_LIST_CHUNK) {
            attachments.extend(
                AttachmentRow::filter(AttachmentRow::fields().message_id().in_list(chunk))
                    .exec(&mut db)
                    .await?,
            );
            bodies.extend(
                BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
                    .exec(&mut db)
                    .await?,
            );
        }
        Ok(Some(view.detail(attachments, bodies, &self.inner.me())))
    }

    /// [`Store::thread`] without bodies or attachments: who each message is from and to, its
    /// labels and headers, for callers that need no more (making a split from conversations).
    ///
    /// # Errors
    /// On database failures.
    pub async fn thread_headers(&self, id: &ThreadId) -> Result<Option<ThreadDetail>> {
        let mut db = self.inner.db.clone();
        let Some(view) = load_view(&mut db, id.as_str()).await? else {
            return Ok(None);
        };
        Ok(Some(view.detail(Vec::new(), Vec::new(), &self.inner.me())))
    }

    /// `email`'s latest conversations in local mail, newest first, at most `limit`. These are the
    /// threads where they sent or received a message, outside drafts, Spam and Trash.
    /// [`fetch_recent_threads`](Self::fetch_recent_threads) brings in older ones from Gmail.
    ///
    /// # Errors
    /// On database failures.
    pub async fn recent_threads(&self, email: &str, limit: usize) -> Result<Vec<RecentThread>> {
        let mut db = self.inner.db.clone();
        let fields = ParticipantEntry::fields();
        let entries = ParticipantEntry::filter(fields.email().eq(email.to_lowercase()))
            .order_by((fields.sort().desc(), fields.thread_id().asc()))
            .limit(limit)
            .exec(&mut db)
            .await?;
        let ids: Vec<String> = entries
            .iter()
            .map(|entry| entry.thread_id.clone())
            .collect();
        let views = load_views(&mut db, &ids).await?;
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                let view = views.get(&entry.thread_id)?;
                Some(RecentThread {
                    subject: view.subject(),
                    id: ThreadId::new(entry.thread_id),
                    date: jiff::Timestamp::from_millisecond(entry.sort).unwrap_or_default(),
                })
            })
            .collect())
    }

    /// The account's send-as aliases, primary first, as of the last sync.
    ///
    /// # Errors
    /// On database failures.
    pub async fn aliases(&self) -> Result<Vec<crate::model::Alias>> {
        let mut db = self.inner.db.clone();
        Ok(db::kv_get(&mut db, db::ALIASES).await?.unwrap_or_default())
    }

    /// The account's Gmail labels, as of the last sync.
    ///
    /// # Errors
    /// On database failures.
    pub async fn labels(&self) -> Result<Vec<Label>> {
        let mut db = self.inner.db.clone();
        Ok(LabelRow::all()
            .exec(&mut db)
            .await?
            .into_iter()
            .map(|row| Label {
                id: row.id,
                name: row.name,
                kind: if row.system {
                    LabelKind::System
                } else {
                    LabelKind::User
                },
                text_color: row.text_color,
                background_color: row.background_color,
                hidden: row.hidden,
            })
            .collect())
    }

    /// Creates the user label `name` (shown in Gmail's label and message lists) and stores it, so
    /// [`labels`](Self::labels) has it at once. Gmail decides what a valid name is.
    ///
    /// # Errors
    /// [`Error::Offline`] without a network; [`Error::Gmail`] when Gmail refuses the name (taken,
    /// reserved or invalid).
    pub async fn create_label(&self, name: &str) -> Result<Label> {
        let request = gmail::Label {
            name: Some(name.to_owned()),
            label_list_visibility: Some(gmail::LabelListVisibility::LabelShow),
            message_list_visibility: Some(gmail::MessageListVisibility::Show),
            ..gmail::Label::default()
        };
        let created = self
            .inner
            .api
            .call(Lane::Act, 3, |gmail| gmail.create_label(&request))
            .await?;
        let label = Label {
            id: created.id.unwrap_or_default(),
            name: created.name.unwrap_or_else(|| name.to_owned()),
            kind: LabelKind::User,
            text_color: None,
            background_color: None,
            hidden: false,
        };
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        LabelRow::create()
            .id(&label.id)
            .name(&label.name)
            .system(false)
            .hidden(false)
            .text_color(None::<String>)
            .background_color(None::<String>)
            .exec(write.exec())
            .await?;
        write.publish().await?;
        Ok(label)
    }

    /// The bytes of message `message`'s MIME part `part` (an attachment, or an inline image),
    /// fetched from Gmail with sync's retries. `attachment` is the attachment id that the part
    /// had when it was stored. Gmail hands out a new one every time it returns the message, so
    /// when Gmail refuses a stored id (or there is none, because the part's data came inline),
    /// the part is read from the message as Gmail returns it now.
    ///
    /// Someone is waiting on these bytes (an image in a body, a file they clicked), so they're
    /// asked for ahead of sync. As for every Gmail call, a rate-limit pause is waited out when it
    /// ends within the longest wait a call sleeps through. Such a pause is often one that sync
    /// ran into while backfilling.
    ///
    /// # Errors
    /// [`Error::Offline`] without a network, [`Error::Throttled`] while Gmail rate limits the
    /// account for longer, [`Error::MissingPart`] when the message no longer has the part, and
    /// [`Error::Gmail`] for anything else Gmail refuses.
    pub async fn part_data(
        &self,
        message: &str,
        attachment: Option<&str>,
        part: &str,
    ) -> Result<Vec<u8>> {
        self.download_part(message, attachment, part, None).await
    }

    /// [`part_data`](Self::part_data), reporting to `progress` how much of Gmail's answer has
    /// arrived (for a file being downloaded, its card's bar and its notification). A part that
    /// is already kept reports nothing.
    ///
    /// # Errors
    /// As [`part_data`](Self::part_data).
    pub async fn download_part(
        &self,
        message: &str,
        attachment: Option<&str>,
        part: &str,
        progress: Option<gmail::Progress>,
    ) -> Result<Vec<u8>> {
        if let Some(kept) = self.cached_part(message, part) {
            return Ok(kept);
        }
        let data = self
            .fetch_part(Lane::Act, message, attachment, part, progress)
            .await?;
        self.keep_part(message, part, &data);
        Ok(data)
    }

    /// [`part_data`](Self::part_data) from Gmail, in `lane`. A part read by its attachment id
    /// reports its transfer to `progress`.
    pub(crate) async fn fetch_part(
        &self,
        lane: Lane,
        message: &str,
        attachment: Option<&str>,
        part: &str,
        progress: Option<gmail::Progress>,
    ) -> Result<Vec<u8>> {
        let api = &self.inner.api;
        fn report<'a>(
            call: gmail::Call<'a, gmail::MessagePartBody>,
            progress: Option<&gmail::Progress>,
        ) -> gmail::Call<'a, gmail::MessagePartBody> {
            match progress {
                Some(progress) => call.on_progress(progress.clone()),
                None => call,
            }
        }
        if let Some(attachment) = attachment {
            match api
                .call(lane, 3, |gmail| {
                    report(gmail.attachment(message, attachment), progress.as_ref())
                })
                .await
            {
                Ok(body) => return Ok(body.data.unwrap_or_default()),
                Err(failure) if failure.is_permanent() => {
                    log::info!(
                        "Gmail refused part {part} of {message} by its stored id ({}); fetching the message",
                        failure.message()
                    );
                }
                Err(failure) => return Err(failure.into()),
            }
        }
        let missing = || Error::MissingPart {
            message: message.to_owned(),
            part: part.to_owned(),
        };
        let fetched = api
            .call(lane, 3, |gmail| {
                gmail.message(message, gmail::Format::Full, &[])
            })
            .await?;
        let body = fetched
            .payload
            .as_ref()
            .and_then(|payload| {
                payload
                    .walk()
                    .find(|candidate| candidate.part_id.as_deref() == Some(part))
            })
            .and_then(|found| found.body.clone())
            .ok_or_else(missing)?;
        match (body.data, body.attachment_id) {
            (Some(data), _) => Ok(data),
            (None, Some(fresh)) => Ok(api
                .call(lane, 3, |gmail| {
                    report(gmail.attachment(message, &fresh), progress.as_ref())
                })
                .await?
                .data
                .unwrap_or_default()),
            (None, None) => Err(missing()),
        }
    }
}

impl Inner {
    pub fn me(&self) -> Identity {
        self.identity
            .read()
            .expect("identity lock is never poisoned")
            .clone()
    }

    /// Who the user is and the account's splits, for routing threads.
    pub fn router(&self) -> Router {
        Router {
            me: self.me(),
            splits: self
                .splits
                .read()
                .expect("splits lock is never poisoned")
                .clone(),
        }
    }

    /// Reads the splits again (after settings or labels change). When `reroute` is set and the
    /// enabled splits changed, every inbox thread is routed again.
    pub async fn reload_splits(&self, reroute: bool) -> Result<()> {
        let mut db = self.db.clone();
        let splits = Arc::new(load_splits(&mut db).await?);
        let changed = {
            let current = self.splits.read().expect("splits lock is never poisoned");
            current.routed_ids() != splits.routed_ids()
        };
        if !(reroute && changed) {
            *self.splits.write().expect("splits lock is never poisoned") = splits;
            return Ok(());
        }
        // Swapped first, so whatever routes next already sees the new splits.
        *self.splits.write().expect("splits lock is never poisoned") = splits;
        let router = self.router();
        let threads: Vec<String> = ListEntry::filter_by_list_id(INBOX)
            .select(ListEntry::fields().thread_id())
            .exec(&mut db)
            .await?;
        let mut write = self.begin(&mut db).await?;
        reindex_many(write.exec(), &threads, &router).await?;
        // The Inbox, Important and Other searches subtract the splits, so a page token from the
        // old search can't continue the new one. Their passes start again from the top and keep
        // how far back they've covered.
        let inbox_lists = [ListId::Inbox, ListId::Important, ListId::Other];
        let keys: Vec<&str> = inbox_lists.iter().map(ListId::key).collect();
        ListCursor::filter(
            ListCursor::fields()
                .id()
                .in_list(keys)
                .and(ListCursor::fields().complete().eq(false)),
        )
        .update()
        .page_token(None::<String>)
        .seen(0)
        .exec(write.exec())
        .await?;
        write.publish().await
    }

    /// Tells subscribers to re-read.
    pub fn notify(&self) {
        self.changes.send_modify(|revision| *revision += 1);
    }

    /// Starts a write transaction on `db`, a clone of the store's handle.
    pub async fn begin<'a>(&'a self, db: &'a mut Db) -> Result<Write<'a>> {
        let lock = self.write.lock().await;
        let tx = db.transaction().await?;
        Ok(Write {
            tx,
            _lock: lock,
            inner: self,
        })
    }
}

/// A write transaction. It holds the store's write lock until it ends, and rolls back if
/// dropped. SQLite has one writer, and taking turns in-process means that writer is never
/// contended.
pub(crate) struct Write<'a> {
    tx: Transaction<'a>,
    _lock: MutexGuard<'a, ()>,
    inner: &'a Inner,
}

impl Write<'_> {
    pub fn exec(&mut self) -> &mut dyn Executor {
        &mut self.tx
    }

    /// Commits a change the UI doesn't show (cursors, retry bookkeeping).
    pub async fn commit(self) -> Result<()> {
        self.tx.commit().await?;
        Ok(())
    }

    /// Commits, then tells subscribers to re-read.
    pub async fn publish(self) -> Result<()> {
        let inner = self.inner;
        self.commit().await?;
        inner.notify();
        Ok(())
    }
}

/// A thread as the user sees it: stored messages plus queued mutations. `None` when no
/// message is stored.
pub(crate) async fn load_view(exec: &mut dyn Executor, thread: &str) -> Result<Option<ThreadView>> {
    Ok(load_views(exec, &[thread.to_owned()]).await?.remove(thread))
}

/// [`load_view`] for many threads at once.
pub(crate) async fn load_views(
    exec: &mut dyn Executor,
    threads: &[String],
) -> Result<HashMap<String, ThreadView>> {
    if threads.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = crate::drafts::marks(exec).await?;
    load_views_with_marks(exec, threads, &marks).await
}

/// Executes a filtered list page query in SQL (`EXISTS` against `list_ids`), returning up to
/// `limit + 1` matching entries newest first without loading either list into memory.
async fn filtered_entries(
    exec: &mut dyn Executor,
    list: &ListId,
    narrow: Filter,
    keep: &[ThreadId],
    bound: i64,
    after: Option<&Cursor>,
    limit: usize,
) -> Result<Vec<ListEntry>> {
    const INLINE_KEEP_MAX: usize = 400;
    let members = list.members();
    let inline_keep = keep.len() <= INLINE_KEEP_MAX;
    let mut sql = String::from("SELECT l.list_id, l.thread_id, l.sort FROM list_ids l WHERE ");
    let mut params: Vec<toasty::stmt::Value> = Vec::new();
    if members.len() == 1 {
        params.push(members[0].into());
        sql.push_str("l.list_id = ?1");
    } else {
        let placeholders: Vec<String> = members
            .iter()
            .map(|member| {
                params.push((*member).into());
                format!("?{}", params.len())
            })
            .collect();
        sql.push_str(&format!("l.list_id IN ({})", placeholders.join(", ")));
    }
    params.push(bound.into());
    sql.push_str(&format!(" AND l.sort >= ?{}", params.len()));
    if let Some(after) = after {
        params.push(after.sort.into());
        let sort_p = params.len();
        params.push(after.thread.clone().into());
        let thread_p = params.len();
        sql.push_str(&format!(
            " AND (l.sort < ?{sort_p} OR (l.sort = ?{sort_p} AND l.thread_id < ?{thread_p}))"
        ));
    }
    params.push(narrow.key().into());
    let narrow_p = params.len();
    if keep.is_empty() || !inline_keep {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM list_ids f WHERE f.list_id = ?{narrow_p} AND f.thread_id = l.thread_id)"
        ));
    } else {
        let placeholders: Vec<String> = keep
            .iter()
            .map(|id| {
                params.push(id.as_str().into());
                format!("?{}", params.len())
            })
            .collect();
        sql.push_str(&format!(
            " AND (EXISTS (SELECT 1 FROM list_ids f WHERE f.list_id = ?{narrow_p} AND f.thread_id = l.thread_id) OR l.thread_id IN ({}))",
            placeholders.join(", ")
        ));
    }
    params.push(
        i64::try_from(limit.saturating_add(1))
            .unwrap_or(i64::MAX)
            .into(),
    );
    sql.push_str(&format!(
        " ORDER BY l.sort DESC, l.thread_id DESC LIMIT ?{}",
        params.len()
    ));
    let mut stmt = toasty::sql::query(sql);
    for param in params {
        stmt = stmt.bind(param);
    }
    let mut entries: Vec<ListEntry> = db::query_rows::<(String, String, i64)>(exec, stmt)
        .await?
        .into_iter()
        .map(|(list_id, thread_id, sort)| ListEntry {
            list_id,
            thread_id,
            sort,
        })
        .collect();
    if !inline_keep {
        let mut seen: HashSet<(String, String)> = entries
            .iter()
            .map(|e| (e.list_id.clone(), e.thread_id.clone()))
            .collect();
        let fields = ListEntry::fields();
        let keep_ids: Vec<&str> = keep.iter().map(ThreadId::as_str).collect();
        for chunk in keep_ids.chunks(db::IN_LIST_CHUNK) {
            let mut expr = fields
                .list_id()
                .in_list(members.as_slice())
                .and(fields.thread_id().in_list(chunk))
                .and(fields.sort().ge(bound));
            if let Some(after) = after {
                expr = expr.and(
                    fields.sort().lt(after.sort).or(fields
                        .sort()
                        .eq(after.sort)
                        .and(fields.thread_id().lt(&after.thread))),
                );
            }
            for entry in ListEntry::filter(expr).exec(exec).await? {
                if seen.insert((entry.list_id.clone(), entry.thread_id.clone())) {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by(|a, b| (b.sort, &b.thread_id).cmp(&(a.sort, &a.thread_id)));
        entries.truncate(limit + 1);
    }
    Ok(entries)
}

/// Loads attachment rows for every message in `views`, chunked to stay below SQLite's bind
/// parameter limit and grouped by thread id so per-thread summaries only scan their own rows.
pub(crate) async fn load_attachments_by_thread(
    exec: &mut dyn Executor,
    views: &HashMap<String, ThreadView>,
) -> Result<HashMap<String, Vec<AttachmentRow>>> {
    let mut msg_to_thread: HashMap<&str, &str> = HashMap::new();
    for (thread_id, view) in views {
        for message in view.all_messages() {
            msg_to_thread.insert(message.row.id.as_str(), thread_id.as_str());
        }
    }
    let messages: Vec<&str> = msg_to_thread.keys().copied().collect();
    let mut by_thread: HashMap<String, Vec<AttachmentRow>> = HashMap::new();
    for chunk in messages.chunks(db::IN_LIST_CHUNK) {
        for row in AttachmentRow::filter(AttachmentRow::fields().message_id().in_list(chunk))
            .exec(exec)
            .await?
        {
            if let Some(&thread_id) = msg_to_thread.get(row.message_id.as_str()) {
                by_thread.entry(thread_id.to_owned()).or_default().push(row);
            }
        }
    }
    Ok(by_thread)
}

async fn load_views_with_marks(
    exec: &mut dyn Executor,
    threads: &[String],
    marks: &crate::drafts::Marks,
) -> Result<HashMap<String, ThreadView>> {
    if threads.is_empty() {
        return Ok(HashMap::new());
    }
    let mut seen = HashSet::with_capacity(threads.len());
    let unique: Vec<&str> = threads
        .iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect();
    let mut rows: HashMap<String, Vec<MessageRow>> = HashMap::new();
    let mut ops: HashMap<String, Vec<OutboxRow>> = HashMap::new();
    let mut reminders: HashMap<String, ReminderRow> = HashMap::new();
    for chunk in unique.chunks(db::IN_LIST_CHUNK) {
        for row in MessageRow::filter(MessageRow::fields().thread_id().in_list(chunk))
            .exec(exec)
            .await?
        {
            rows.entry(row.thread_id.clone()).or_default().push(row);
        }
        for op in OutboxRow::filter(OutboxRow::fields().queue().in_list(chunk))
            .order_by(OutboxRow::fields().id().asc())
            .exec(exec)
            .await?
        {
            ops.entry(op.queue.clone()).or_default().push(op);
        }
        for row in ReminderRow::filter(ReminderRow::fields().thread_id().in_list(chunk))
            .exec(exec)
            .await?
        {
            reminders.insert(row.thread_id.clone(), row);
        }
    }
    Ok(rows
        .into_iter()
        .filter_map(|(id, mut rows)| {
            // A discarded or sending draft is gone from reads before Gmail hears about it.
            rows.retain(|row| !marks.hidden.contains(&row.id));
            if rows.is_empty() {
                return None;
            }
            let ops = ops.remove(&id).unwrap_or_default();
            let mut view = ThreadView::new(id.clone(), rows, &ops, &marks.exo_drafts);
            for message in &mut view.messages {
                message.failed = marks.failed.get(&message.row.id).copied();
                message.scheduled = marks.scheduled.get(&message.row.id).copied();
            }
            view.sending = marks.sending.contains(&id);
            view.sending_snippet = marks.sending_snippets.get(&id).cloned();
            view.scheduled = marks.scheduled_threads.get(&id).copied();
            view.reminder = reminders.remove(&id);
            Some((id, view))
        })
        .collect())
}

/// The account's splits as saved, with the labels they can name.
async fn load_splits(exec: &mut dyn Executor) -> Result<Splits> {
    let settings: Settings = settings::read(exec).await?;
    let labels = LabelRow::all().exec(exec).await?;
    Ok(Splits::new(
        settings.splits().cloned().collect(),
        labels
            .iter()
            .map(|label| (label.id.as_str(), label.name.as_str())),
    ))
}

const REINDEX_CHUNK: usize = 64;

/// Recomputes a thread's list membership and participants from its messages and queued
/// mutations.
pub(crate) async fn reindex(exec: &mut dyn Executor, thread: &str, router: &Router) -> Result<()> {
    reindex_many(exec, &[thread.to_owned()], router).await
}

/// [`reindex`] for many threads at once, reading the draft table once for the batch and
/// loading views and attachments in chunks.
pub(crate) async fn reindex_many(
    exec: &mut dyn Executor,
    thread_ids: &[String],
    router: &Router,
) -> Result<()> {
    let mut seen = HashSet::with_capacity(thread_ids.len());
    let unique: Vec<String> = thread_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();
    if unique.is_empty() {
        return Ok(());
    }
    let marks = crate::drafts::marks(exec).await?;
    let me = &router.me;
    for chunk in unique.chunks(REINDEX_CHUNK) {
        ListEntry::filter(ListEntry::fields().thread_id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
        ParticipantEntry::filter(ParticipantEntry::fields().thread_id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
        let mut views = load_views_with_marks(exec, chunk, &marks).await?;
        let mut by_message: HashMap<String, Vec<AttachmentRow>> = HashMap::new();
        {
            let messages: Vec<&str> = views
                .values()
                .flat_map(|view| view.messages.iter().map(|m| m.row.id.as_str()))
                .collect();
            for message_chunk in messages.chunks(db::IN_LIST_CHUNK) {
                for row in AttachmentRow::filter(
                    AttachmentRow::fields().message_id().in_list(message_chunk),
                )
                .exec(exec)
                .await?
                {
                    by_message
                        .entry(row.message_id.clone())
                        .or_default()
                        .push(row);
                }
            }
        }
        let mut entries = ListEntry::create_many();
        let mut has_entries = false;
        let mut participants = ParticipantEntry::create_many();
        let mut has_participants = false;
        for thread in chunk {
            let Some(view) = views.remove(thread) else {
                crate::search::unindex(exec, thread).await?;
                continue;
            };
            let attachments: Vec<AttachmentRow> = view
                .messages
                .iter()
                .filter_map(|m| by_message.remove(&m.row.id))
                .flatten()
                .collect();
            let lists = router.list_ids(&view, &attachments);
            for list in &lists {
                let sort = view.sort_for(list, me);
                entries = entries
                    .with_item(|entry| entry.list_id(*list).thread_id(thread.as_str()).sort(sort));
                has_entries = true;
            }
            let (emails, sort) = view.participants(me);
            for email in &emails {
                participants = participants
                    .with_item(|entry| entry.email(email).thread_id(thread.as_str()).sort(sort));
                has_participants = true;
            }
            crate::search::index(exec, &view, &lists, &attachments).await?;
        }
        if has_entries {
            entries.exec(exec).await?;
        }
        if has_participants {
            participants.exec(exec).await?;
        }
    }
    Ok(())
}

/// Refuses account names that would leave the store's folder (`../x`) or hide its files.
fn check_account(account: &str) -> Result<()> {
    if account.is_empty() || account.starts_with('.') || account.contains(['/', '\\', '\0']) {
        return Err(Error::InvalidAccount(account.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::{Definitions, SplitInbox};

    #[tokio::test]
    async fn changing_the_splits_restarts_the_inbox_passes() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-split-cursors-{}", std::process::id()));
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        for (id, complete) in [(INBOX, false), ("SH_OTHER", true), ("SH_ALL", false)] {
            ListCursor::create()
                .id(id)
                .page_token(Some("page-3".to_owned()))
                .min_sort(Some(42))
                .complete(complete)
                .seen(150)
                .exec(&mut db)
                .await
                .expect("writes");
        }
        let split = SplitInbox::new(
            "GitHub",
            "from:github.com",
            None,
            None,
            Definitions {
                labels: &[],
                me: &[],
            },
        );
        let mut settings = Settings::default();
        settings.add_split(split);
        store.save_settings(&settings).await.expect("saves");
        let cursor = |id: &'static str| {
            let mut db = store.inner.db.clone();
            async move {
                ListCursor::filter_by_id(id)
                    .first()
                    .exec(&mut db)
                    .await
                    .expect("reads")
                    .expect("kept")
            }
        };
        let inbox = cursor(INBOX).await;
        assert_eq!(
            (inbox.page_token, inbox.min_sort, inbox.seen),
            (None, Some(42), 0),
            "from the top, keeping its coverage"
        );
        assert_eq!(
            cursor("SH_OTHER").await.page_token.as_deref(),
            Some("page-3"),
            "a finished pass stays finished"
        );
        assert_eq!(
            cursor("SH_ALL").await.page_token.as_deref(),
            Some("page-3"),
            "All Mail doesn't subtract splits"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn filtered_list_runs_in_sql_and_load_views_chunks_past_in_list_limit() {
        let dir = std::env::temp_dir().join(format!(
            "exo-store-sql-filter-and-chunk-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        // Mark All Mail complete so `list_filtered` has full coverage.
        ListCursor::create()
            .id(crate::ids::labels::SH_ALL)
            .page_token(None::<String>)
            .min_sort(None::<i64>)
            .complete(true)
            .seen(0)
            .exec(&mut db)
            .await
            .expect("cursor");

        // Create 520 threads (exceeds `IN_LIST_CHUNK = 500`), every 10th unread, plus one read
        // thread we'll pass in `keep`.
        let mut ids = Vec::with_capacity(520);
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            for i in 0..520_i64 {
                let tid = format!("t{i:04}");
                let mid = format!("m{i:04}");
                let mut labels = vec![
                    INBOX.to_owned(),
                    crate::ids::labels::CATEGORY_PERSONAL.to_owned(),
                ];
                if i % 10 == 0 {
                    labels.push(UNREAD.to_owned());
                }
                let msg = gmail::Message {
                    id: Some(mid),
                    thread_id: Some(tid.clone()),
                    internal_date: Some(1_794_900_000_000 + i * 1_000),
                    label_ids: labels,
                    payload: Some(gmail::MessagePart {
                        headers: vec![
                            gmail::MessagePartHeader {
                                name: "Subject".into(),
                                value: format!("Subject {i}"),
                            },
                            gmail::MessagePartHeader {
                                name: "From".into(),
                                value: "Priya <priya@brightmoor.co>".into(),
                            },
                        ],
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let parsed = crate::parse::parse(&msg, false).expect("parses");
                crate::provider::save_message(write.exec(), parsed)
                    .await
                    .expect("saves");
                ids.push(tid);
            }
            reindex_many(write.exec(), &ids, &store.inner.router())
                .await
                .expect("reindexes");
            write.commit().await.expect("commits");
        }

        // Chunked `load_views` handles >500 threads (even with duplicate ids).
        let mut dup_ids = ids.clone();
        dup_ids.push("t0000".to_owned());
        let loaded = load_views(&mut db, &dup_ids)
            .await
            .expect("loads >500 views");
        assert_eq!(loaded.len(), 520);
        assert_eq!(loaded["t0000"].messages.len(), 1, "no duplicate messages");

        // SQL-filtered `list_filtered` with `Filter::Unread`, sticky `keep`, and pagination.
        let kept = ThreadId::new("t0515");
        let page1 = store
            .list_filtered(
                &ListId::Inbox,
                Some(Filter::Unread),
                std::slice::from_ref(&kept),
                &[],
                None,
                5,
            )
            .await
            .expect("page 1");
        assert_eq!(page1.threads.len(), 5);
        assert!(page1.next.is_some());
        let page1_ids: Vec<&str> = page1.threads.iter().map(|t| t.id.as_str()).collect();
        // Newest unread are t0510, t0500, t0490, t0480; kept sticky t0515 sorts ahead of t0510.
        assert_eq!(page1_ids, ["t0515", "t0510", "t0500", "t0490", "t0480"]);

        let page2 = store
            .list_filtered(
                &ListId::Inbox,
                Some(Filter::Unread),
                std::slice::from_ref(&kept),
                &[],
                page1.next.as_ref(),
                5,
            )
            .await
            .expect("page 2");
        let page2_ids: Vec<&str> = page2.threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(page2_ids, ["t0470", "t0460", "t0450", "t0440", "t0430"]);

        drop(db);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
