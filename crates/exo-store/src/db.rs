//! The SQLite schema (toasty models), opening a database file, and the key-value table.
//!
//! Provider truth (`threads`, `messages`, `attachments`, `bodies`, `labels`) is kept apart
//! from the local overlay (`outbox`). `list_ids` is materialized from both on every change, so
//! list reads are a single indexed range scan.
//!
//! Changing a model here changes the schema, so files written before the change need a step in
//! [`crate::migrations`], which says how.

use std::borrow::Cow;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use toasty::codegen_support::core as toasty_core;
use toasty::stmt::Value;
use toasty::{Db, Executor, Json};
use toasty_driver_sqlite::Sqlite;

/// Maximum number of bound items per `IN (...)` query chunk, staying well below SQLite's
/// default host parameter limit (999/32766).
pub(crate) const IN_LIST_CHUNK: usize = 500;
/// Milliseconds a SQLite connection waits on a busy lock before returning `SQLITE_BUSY`.
const BUSY_TIMEOUT_MS: i64 = 5_000;

use crate::error::{Error, Result};
use crate::ids::ListId;
use crate::ids::labels::{INBOX, SH_ALL};
use crate::migrations;
use crate::model::Header;

/// Where the history cursor is kept in `kv`.
pub(crate) const HISTORY_ID: &str = "history_id";
/// Where the account's identity is kept in `kv`.
pub(crate) const IDENTITY: &str = "identity";
/// Where the account's send-as aliases are kept in `kv`.
pub(crate) const ALIASES: &str = "aliases";
/// A JSON map in `kv` from each held send whose reminder is already on its thread to that
/// reminder, so the thread's reminder is left alone when Gmail sends them.
pub(crate) const GIVEN_REMINDERS: &str = "given_reminders";
/// Set in `kv` by a new store's first sync until the store has brought back the threads that
/// Gmail has under `[Exo]/Is Snoozed` but that have no reminder
/// ([`Store::return_orphaned_snoozes`]).
///
/// [`Store::return_orphaned_snoozes`]: crate::Store::return_orphaned_snoozes
pub(crate) const ORPHANED_SNOOZES: &str = "orphaned_snoozes";
/// The newest message date, in epoch milliseconds, of the conversations that eviction let go of,
/// kept in `kv`. The pre-render fetches nothing that old back.
pub(crate) const EVICTED_BEFORE: &str = "evicted_before";

/// Thread-level provider facts.
#[derive(Debug, toasty::Model)]
#[table = "threads"]
pub(crate) struct ThreadRow {
    #[key]
    pub id: String,
    /// The newest `historyId` stored for this thread. A listed `historyId` above it means the
    /// thread is stale.
    pub history_id: u64,
}

/// A Gmail message as Gmail last reported it (drafts included).
#[derive(Debug, toasty::Model)]
#[table = "messages"]
pub(crate) struct MessageRow {
    #[key]
    pub id: String,
    #[index]
    pub thread_id: String,
    pub history_id: u64,
    /// `internalDate` in epoch milliseconds, the message's date for sorting.
    pub internal_date: i64,
    pub label_ids: Vec<String>,
    pub subject: String,
    /// Gmail's snippet, still HTML-escaped.
    pub snippet: String,
    /// `From` (`from` would clash with toasty's generated builders).
    #[column(type = text)]
    pub sender: Json<Option<Mailbox>>,
    /// The raw `From` value, for the importance rule's "via Google" test.
    pub sender_raw: String,
    #[column(type = text)]
    pub to: Json<Vec<Mailbox>>,
    #[column(type = text)]
    pub cc: Json<Vec<Mailbox>>,
    #[column(type = text)]
    pub bcc: Json<Vec<Mailbox>>,
    #[column(type = text)]
    pub reply_to: Json<Vec<Mailbox>>,
    pub rfc822_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub list_id: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
    #[column(type = text)]
    pub headers: Json<Vec<Header>>,
    pub size: u32,
    /// A real (non-inline) attachment or a calendar invite.
    pub has_attachments: bool,
}

pub(crate) use gmail::mime::Mailbox;

#[derive(Debug, toasty::Model)]
#[table = "attachments"]
#[key(message_id, part_id)]
pub(crate) struct AttachmentRow {
    pub message_id: String,
    pub part_id: String,
    pub attachment_id: Option<String>,
    pub filename: String,
    pub mime_type: String,
    pub size: u32,
    pub content_id: Option<String>,
    pub inline: bool,
}

/// A message's body parts as sent. Kept apart from `messages` so lists never load them, and so
/// they can be evicted without losing metadata.
#[derive(Debug, toasty::Model)]
#[table = "bodies"]
pub(crate) struct BodyRow {
    #[key]
    pub message_id: String,
    pub html: Option<String>,
    pub text: Option<String>,
    /// The text a reader sees, without the quoted reply and the signature that the thread view
    /// hides behind the `…`. This is what the search index holds. `None` until it's worked out
    /// ([`crate::Store::index_text_with`]), and until then the index has the whole text.
    pub visible: Option<String>,
}

/// A conversation that has a stored body whose [`BodyRow::visible`] text isn't worked out yet.
/// It's queued when the body is stored ([`crate::Store::index_text_with`]).
#[derive(Debug, toasty::Model)]
#[table = "text_due"]
pub(crate) struct TextDue {
    #[key]
    pub thread_id: String,
}

#[derive(Debug, toasty::Model)]
#[table = "labels"]
pub(crate) struct LabelRow {
    #[key]
    pub id: String,
    pub name: String,
    pub system: bool,
    pub hidden: bool,
    pub text_color: Option<String>,
    pub background_color: Option<String>,
}

/// Materialized list membership: one row per list a thread is in, with its sort key there.
#[derive(Debug, toasty::Model)]
#[table = "list_ids"]
#[key(list_id, thread_id)]
#[index(list_id, sort)]
pub(crate) struct ListEntry {
    pub list_id: String,
    #[index]
    pub thread_id: String,
    pub sort: i64,
}

/// Who took part in each thread, for a contact's latest conversations. There's one row per thread
/// and lower-cased address, with the thread's date. Materialized with `list_ids`.
#[derive(Debug, toasty::Model)]
#[table = "participants"]
#[key(email, thread_id)]
#[index(email, sort)]
pub(crate) struct ParticipantEntry {
    pub email: String,
    #[index]
    pub thread_id: String,
    pub sort: i64,
}

/// A list's backfill cursor.
#[derive(Debug, toasty::Model)]
#[table = "lists"]
pub(crate) struct ListCursor {
    #[key]
    pub id: String,
    /// The next `threads.list` page.
    pub page_token: Option<String>,
    /// The oldest thread date the backfill has reached. It's only ever lowered.
    pub min_sort: Option<i64>,
    /// Gmail has no more pages, so everything is covered.
    pub complete: bool,
    /// Threads listed so far, for the per-list cap.
    pub seen: u32,
    /// The pass lists only threads with mail before this date, in epoch milliseconds (Gmail's
    /// `before:`). Eviction moved the list's coverage forward to this date, so the pass takes up
    /// where the stored threads end rather than listing them all again.
    pub before: Option<i64>,
}

/// One queued mutation.
#[derive(Debug, toasty::Model)]
#[table = "outbox"]
pub(crate) struct OutboxRow {
    /// Insertion order, which is the queue order.
    #[key]
    #[auto]
    pub id: u64,
    /// The thread id, for one FIFO per thread.
    #[index]
    pub queue: String,
    /// Coalescing key. A new row replaces an unstarted row with the same key at the queue's
    /// tail.
    pub replace_key: Option<String>,
    #[column(type = text)]
    pub mutation: Json<crate::Mutation>,
    /// This row undoes `mutation`.
    pub undo: bool,
    /// The Gmail calls still to make, in order. Each is removed once Gmail acknowledges it.
    #[column(type = text)]
    pub steps: Json<Vec<crate::mutation::Step>>,
    /// Failures that count toward giving up (offline and auth failures don't).
    pub attempts: u32,
    /// Consecutive failures of any kind, for the backoff.
    pub failures: u32,
    pub not_before: Option<jiff::Timestamp>,
    pub started_at: Option<jiff::Timestamp>,
}

/// A draft compose is writing, and how far its Gmail draft has caught up with it. Not provider
/// truth, like the outbox.
#[derive(Debug, toasty::Model)]
#[table = "drafts"]
pub(crate) struct DraftRow {
    /// Exo's draft id (`draft00…`), stable for the draft's life.
    #[key]
    pub id: String,
    /// The Gmail thread, once there is one.
    #[index]
    pub thread_id: Option<String>,
    /// The Gmail draft, once created.
    pub gmail_id: Option<String>,
    /// The Gmail message of the latest version Gmail has.
    pub message_id: Option<String>,
    #[column(type = text)]
    pub record: Json<crate::drafts::DraftRecord>,
    /// The RFC 822 message that the Gmail draft should hold, which is what sending sends.
    pub raw: String,
    /// Bumped by every save.
    pub revision: u64,
    /// The revision Gmail has (0: none).
    pub pushed: u64,
    /// Discarded, so its Gmail draft is to be deleted, and the row with it.
    pub discarded: bool,
    /// Held for sending until then (undo send).
    pub send_at: Option<jiff::Timestamp>,
    /// Consecutive failures, for the backoff.
    pub failures: u32,
    pub not_before: Option<jiff::Timestamp>,
    /// Its send failed, as [`SendFailure::code`](crate::SendFailure::code) has it (`""` for no
    /// reason).
    pub failure: Option<String>,
    /// When its send failed.
    pub failed_at: Option<jiff::Timestamp>,
}

/// A thread's reminder: when it brings the thread back and what cancels it. Not provider truth,
/// since Gmail only has the hidden labels.
#[derive(Debug, Clone, toasty::Model)]
#[table = "reminders"]
pub(crate) struct ReminderRow {
    #[key]
    pub thread_id: String,
    /// When it brings the thread back.
    pub at: jiff::Timestamp,
    /// A reply doesn't cancel it (the picker's `regardless`).
    pub keep_on_reply: bool,
    /// The thread's messages when it was set. A reply is a message not among them.
    pub message_ids: Vec<String>,
    /// When it was set.
    pub created_at: jiff::Timestamp,
    /// When it brought the thread back (the trigger message's date). The thread then sorts at
    /// this time.
    pub fired_at: Option<jiff::Timestamp>,
    /// A reply cancelled it and the scheduler took its label off.
    pub aborted: bool,
}

/// A calendar on the Google Calendar list of one of the account's calendar accounts (the mail
/// account's own among them), as Calendar last listed it.
#[derive(Debug, toasty::Model)]
#[table = "calendars"]
#[key(account, id)]
pub(crate) struct CalendarRow {
    /// The calendar account whose list it's on.
    pub account: String,
    pub id: String,
    /// Its place in the list.
    pub position: u32,
    #[column(type = text)]
    pub entry: Json<gmail::calendar::CalendarListEntry>,
}

/// An event (or one instance of a recurring event) as Calendar last listed it.
#[derive(Debug, toasty::Model)]
#[table = "events"]
#[key(calendar_id, id)]
#[index(calendar_id, start)]
pub(crate) struct EventRow {
    pub calendar_id: String,
    pub id: String,
    /// Start and end, in epoch milliseconds. An all-day event's are the UTC midnights of its
    /// dates.
    pub start: i64,
    #[index]
    pub end: i64,
    pub all_day: bool,
    #[column(type = text)]
    pub event: Json<gmail::calendar::Event>,
}

/// A change to the calendar that Calendar doesn't have yet. Not provider truth, like the outbox.
#[derive(Debug, toasty::Model)]
#[table = "calendar_changes"]
pub(crate) struct CalendarChangeRow {
    /// Insertion order, which is the queue order.
    #[key]
    #[auto]
    pub id: u64,
    /// What the caller queued it under, to take it back with.
    #[index]
    pub token: u64,
    /// The event and its series, for one FIFO per event.
    #[index]
    pub queue: String,
    #[column(type = text)]
    pub change: Json<crate::CalendarChange>,
    /// Consecutive postponements, for the backoff.
    pub failures: u32,
    /// Held for Undo, or backing off, until then.
    pub not_before: Option<jiff::Timestamp>,
}

/// A bulk action of the last week ([`crate::bulk_log`]). Not provider truth, like the outbox.
#[derive(Debug, toasty::Model)]
#[table = "bulk_actions"]
pub(crate) struct BulkRow {
    /// Insertion order, which is the order they started in.
    #[key]
    #[auto]
    pub id: u64,
    /// When it started, in epoch milliseconds.
    pub started_at: i64,
    #[column(type = text)]
    pub record: Json<crate::BulkRecord>,
    /// What it has changed, which its undo puts back. Kept apart from `record` because a running
    /// action writes it as it goes.
    #[column(type = text)]
    pub changed: Json<crate::Relabelled>,
}

/// Small persistent values: the history cursor, the account's identity.
#[derive(Debug, toasty::Model)]
#[table = "kv"]
pub(crate) struct KvRow {
    #[key]
    pub key: String,
    pub value: String,
}

/// The user's settings, a JSON document per [`crate::settings::Document`]. Not a cache.
#[derive(Debug, toasty::Model)]
#[table = "settings"]
pub(crate) struct SettingsRow {
    #[key]
    pub key: String,
    pub value: String,
}

/// Opens the database at `path`, making it, or bringing a file an older Exo wrote up to this
/// build's schema ([`crate::migrations`]).
pub(crate) async fn open(path: &Path) -> Result<Db> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|source| Error::Io {
            path: dir.to_owned(),
            source,
        })?;
    }
    let mut db = connect(path).await?;
    migrations::migrate(&mut db, path).await?;
    // Without it, eviction only frees pages for the file to reuse. A file that can't be rebuilt
    // now (a disk too full for the copy) opens anyway, and the next open tries again.
    if let Err(error) = crate::evict::reclaimable(&db).await {
        log::warn!("the store keeps the space eviction frees for now: {error}");
    }
    Ok(db)
}

/// Makes the database at `path` again, empty. When the old one still reads, it keeps what Gmail
/// doesn't have: its settings, drafts and queued changes, in the columns both schemas have. What
/// can't be kept is dropped and logged.
pub(crate) async fn clear(path: &Path) -> Result<()> {
    let old = PathBuf::from(format!("{}.cleared", path.display()));
    if old.exists() {
        // A clear stopped halfway, and its old file still holds what to keep.
        remove(path)?;
    } else {
        for suffix in FILES {
            let (from, to) = (
                format!("{}{suffix}", path.display()),
                format!("{}{suffix}", old.display()),
            );
            match std::fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: from.into(),
                        source,
                    });
                }
            }
        }
    }
    let db = open(path).await?;
    if old.exists() {
        keep(&db, &old).await;
    }
    drop(db);
    remove(&old)
}

/// What Clear Cache and Restart copies from the old database, which is what Gmail doesn't have: the
/// settings, the drafts (held and scheduled sends among them), the queued changes, the reminders'
/// schedule, the calendar's queued changes and the bulk actions' log.
/// `every_table_is_kept_or_refilled` in `tests/clear_cache.rs` lists the rest.
const KEPT_TABLES: [&str; 6] = [
    "settings",
    "drafts",
    "outbox",
    "reminders",
    "calendar_changes",
    "bulk_actions",
];
/// The `kv` entries kept with them, Exo's own records: which held sends gave their reminder, and
/// the unsubscribes still to retry.
const KEPT_KEYS: [&str; 2] = [GIVEN_REMINDERS, crate::unsubscribes::KEY];

/// Copies [`KEPT_TABLES`] and [`KEPT_KEYS`] of the database at `old` into `db`.
async fn keep(db: &Db, old: &Path) {
    // An attached database is the connection's, so one connection does it all.
    let mut connection = match db.connection().await {
        Ok(connection) => connection,
        Err(error) => return log::error!("keeping nothing of the old database: {error}"),
    };
    if let Err(error) = toasty::sql::statement("ATTACH DATABASE ?1 AS old")
        .bind(old.display().to_string())
        .exec(&mut connection)
        .await
    {
        return log::error!("keeping nothing of the old database: {error}");
    }
    for table in KEPT_TABLES {
        let columns = query_rows::<(String,)>(
            &mut connection,
            toasty::sql::query(r#"SELECT group_concat('"' || new.name || '"', ', ') FROM pragma_table_info(?1, 'main') new JOIN pragma_table_info(?1, 'old') old ON old.name = new.name"#)
                .bind(table),
        )
        .await;
        let columns = match columns {
            Ok(rows) => match rows.into_iter().next() {
                Some((columns,)) => columns,
                None => continue,
            },
            Err(error) => {
                log::error!("dropping the {table} the old database can't hand over: {error}");
                continue;
            }
        };
        match toasty::sql::statement(format!(
            r#"INSERT INTO main."{table}" ({columns}) SELECT {columns} FROM old."{table}""#
        ))
        .exec(&mut connection)
        .await
        {
            Ok(rows) => log::info!("kept {rows} rows of {table}"),
            Err(error) => {
                log::error!("dropping the {table} the old database can't hand over: {error}")
            }
        }
    }
    let keys = KEPT_KEYS.map(|key| format!("'{key}'")).join(", ");
    match toasty::sql::statement(format!(r#"INSERT INTO main."kv" ("key", "value") SELECT "key", "value" FROM old."kv" WHERE "key" IN ({keys})"#)).exec(&mut connection).await {
        Ok(rows) => log::info!("kept {rows} rows of kv"),
        Err(error) => log::error!("dropping the kv the old database can't hand over: {error}"),
    }
    if let Err(error) = toasty::sql::statement("DETACH DATABASE old")
        .exec(&mut connection)
        .await
    {
        log::warn!("detaching the old database: {error}");
    }
}

/// The files SQLite keeps for a database: the database itself, then its WAL, shared memory and
/// journal, each named for it with a suffix.
const FILES: [&str; 4] = ["", "-wal", "-shm", "-journal"];

/// Deletes the database at `path` and the files beside it.
fn remove(path: &Path) -> Result<()> {
    for suffix in FILES {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::Io { path: file, source }),
        }
    }
    Ok(())
}

/// Wraps `Sqlite` so every pooled connection configures `PRAGMA busy_timeout` upon opening.
/// Unlike `journal_mode = WAL`, `busy_timeout` is per-connection in SQLite.
#[derive(Debug)]
struct BusySqlite {
    inner: Sqlite,
    schema: Arc<toasty_core::Schema>,
}

impl BusySqlite {
    fn open(path: &Path) -> Self {
        let schema = toasty_core::schema::Builder::new()
            .build(
                toasty_core::schema::app::Schema::default(),
                &toasty_core::driver::Capability::SQLITE,
            )
            .expect("empty schema builds for SQLite");
        Self {
            inner: Sqlite::open(path),
            schema: Arc::new(schema),
        }
    }
}

impl toasty_core::driver::Driver for BusySqlite {
    fn url(&self) -> Cow<'_, str> {
        self.inner.url()
    }

    fn capability(&self) -> &'static toasty_core::driver::Capability {
        self.inner.capability()
    }

    fn connect<'life0, 'life1, 'async_trait>(
        &'life0 self,
        cx: &'life1 toasty_core::driver::ConnectContext,
    ) -> Pin<
        Box<
            dyn Future<Output = toasty::Result<Box<dyn toasty_core::driver::Connection>>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let mut conn = self.inner.connect(cx).await?;
            conn.exec(
                &self.schema,
                toasty_core::driver::operation::Operation::RawSql(
                    toasty_core::driver::operation::RawSql {
                        sql: format!("PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}"),
                        params: Vec::new(),
                        ret: toasty_core::driver::operation::RawSqlRet::Infer,
                    },
                ),
            )
            .await?;
            Ok(conn)
        })
    }

    fn max_connections(&self) -> Option<usize> {
        self.inner.max_connections()
    }

    fn generate_migration(
        &self,
        schema_diff: &toasty_core::schema::diff::Schema<'_>,
    ) -> toasty_core::schema::db::Migration {
        self.inner.generate_migration(schema_diff)
    }

    fn reset_db<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = toasty::Result<()>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        self.inner.reset_db()
    }
}

/// Connects to the file at `path` as it is, neither created nor upgraded.
pub(crate) async fn connect(path: &Path) -> Result<Db> {
    let mut db = Db::builder()
        .models(toasty::models!(crate::*))
        .build(BusySqlite::open(path))
        .await?;
    // WAL lets the UI read while sync writes. The mode is stored in the file, so every pooled
    // connection gets it.
    toasty::sql::query("PRAGMA journal_mode = WAL")
        .exec(&mut db)
        .await?;
    Ok(db)
}

/// Extracts one typed column from a raw SQL [`Value`].
pub(crate) trait FromSqlValue: Sized {
    fn from_sql_value(value: Value) -> Option<Self>;
}

impl FromSqlValue for String {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::String(text) => Some(text),
            _ => None,
        }
    }
}

impl FromSqlValue for i64 {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::I64(number) => Some(number),
            _ => None,
        }
    }
}

impl FromSqlValue for u64 {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::U64(number) => Some(number),
            Value::I64(number) => Self::try_from(number).ok(),
            _ => None,
        }
    }
}

impl FromSqlValue for u32 {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::U32(number) => Some(number),
            Value::I64(number) => Self::try_from(number).ok(),
            _ => None,
        }
    }
}

impl FromSqlValue for jiff::Timestamp {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::Timestamp(ts) => Some(ts),
            Value::String(text) => text.parse().ok(),
            _ => None,
        }
    }
}

impl<T: DeserializeOwned> FromSqlValue for Json<T> {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::String(text) => serde_json::from_str(&text).ok().map(Json),
            _ => None,
        }
    }
}

impl FromSqlValue for bool {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::Bool(flag) => Some(flag),
            Value::I64(number) => Some(number != 0),
            _ => None,
        }
    }
}

impl<T: FromSqlValue> FromSqlValue for Option<T> {
    fn from_sql_value(value: Value) -> Option<Self> {
        match value {
            Value::Null => Some(None),
            other => T::from_sql_value(other).map(Some),
        }
    }
}

/// Extracts a positional tuple of columns from a raw SQL `Value::Record`.
pub(crate) trait FromRecord: Sized {
    fn from_record(fields: Vec<Value>) -> Option<Self>;
}

impl<A: FromSqlValue> FromRecord for (A,) {
    fn from_record(fields: Vec<Value>) -> Option<Self> {
        let [a]: [Value; 1] = fields.try_into().ok()?;
        Some((A::from_sql_value(a)?,))
    }
}

impl<A: FromSqlValue, B: FromSqlValue> FromRecord for (A, B) {
    fn from_record(fields: Vec<Value>) -> Option<Self> {
        let [a, b]: [Value; 2] = fields.try_into().ok()?;
        Some((A::from_sql_value(a)?, B::from_sql_value(b)?))
    }
}

impl<A: FromSqlValue, B: FromSqlValue, C: FromSqlValue> FromRecord for (A, B, C) {
    fn from_record(fields: Vec<Value>) -> Option<Self> {
        let [a, b, c]: [Value; 3] = fields.try_into().ok()?;
        Some((
            A::from_sql_value(a)?,
            B::from_sql_value(b)?,
            C::from_sql_value(c)?,
        ))
    }
}

impl<A: FromSqlValue, B: FromSqlValue, C: FromSqlValue, D: FromSqlValue> FromRecord
    for (A, B, C, D)
{
    fn from_record(fields: Vec<Value>) -> Option<Self> {
        let [a, b, c, d]: [Value; 4] = fields.try_into().ok()?;
        Some((
            A::from_sql_value(a)?,
            B::from_sql_value(b)?,
            C::from_sql_value(c)?,
            D::from_sql_value(d)?,
        ))
    }
}

/// Decodes raw [`Value`] rows into typed records, skipping rows whose shape doesn't match `T`.
pub(crate) fn decode_rows<T: FromRecord>(rows: Vec<Value>) -> Vec<T> {
    rows.into_iter()
        .filter_map(|row| match row {
            Value::Record(record) => T::from_record(record.fields),
            _ => None,
        })
        .collect()
}

/// Runs `stmt` and decodes its rows as `T`.
pub(crate) async fn query_rows<T: FromRecord>(
    exec: &mut dyn Executor,
    stmt: toasty::sql::Query,
) -> Result<Vec<T>> {
    Ok(decode_rows(stmt.exec(exec).await?))
}

/// Runs `stmt` and extracts each row's single column as `T`.
pub(crate) async fn query_col<T: FromSqlValue>(
    exec: &mut dyn Executor,
    stmt: toasty::sql::Query,
) -> Result<Vec<T>> {
    Ok(query_rows::<(T,)>(exec, stmt)
        .await?
        .into_iter()
        .map(|(value,)| value)
        .collect())
}

/// Runs a query returning one integer.
pub(crate) async fn query_i64(db: &mut dyn Executor, sql: &str) -> Result<i64> {
    Ok(query_rows::<(i64,)>(db, toasty::sql::query(sql))
        .await?
        .into_iter()
        .next()
        .map_or(0, |(value,)| value))
}

/// Reads a JSON value from `kv`.
pub(crate) async fn kv_get<T: DeserializeOwned>(
    exec: &mut dyn Executor,
    key: &str,
) -> Result<Option<T>> {
    let row = KvRow::filter_by_key(key).first().exec(exec).await?;
    Ok(row.and_then(|row| match serde_json::from_str(&row.value) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn!("ignoring unreadable kv {key:?}: {error}");
            None
        }
    }))
}

/// Writes a JSON value to `kv`.
pub(crate) async fn kv_set<T: Serialize>(
    exec: &mut dyn Executor,
    key: &str,
    value: &T,
) -> Result<()> {
    let value = serde_json::to_string(value).expect("kv values serialize");
    KvRow::upsert_by_key(key).value(value).exec(exec).await?;
    Ok(())
}

/// The lowest list date `list` may show without a gap, which is as far back as the list's own
/// backfill or All Mail's reaches, whichever is further. `i64::MIN` once either is complete, since
/// then every thread is stored, and `None` before either has started. A split only ever holds
/// inbox threads, so the Inbox's backfill covers it too.
pub(crate) async fn coverage(exec: &mut dyn Executor, list: &ListId) -> Result<Option<i64>> {
    // Every scheduled send and reminder is Exo's own, so it's stored.
    if matches!(list, ListId::Scheduled | ListId::Reminders) {
        return Ok(Some(i64::MIN));
    }
    let mut ids = vec![list.key(), SH_ALL];
    if matches!(list, ListId::Split(_)) {
        ids.push(INBOX);
    }
    let cursors = ListCursor::filter(ListCursor::fields().id().in_list(ids))
        .exec(exec)
        .await?;
    Ok(cursors
        .iter()
        .map(|cursor| {
            if cursor.complete {
                i64::MIN
            } else {
                cursor.min_sort.unwrap_or(i64::MAX)
            }
        })
        .min())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn query_rows_decodes_tuples_options_and_booleans() {
        let dir = std::env::temp_dir().join(format!("exo-store-db-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut db = open(&dir.join("rows.sqlite3")).await.expect("opens");

        let one = query_rows::<(String,)>(&mut db, toasty::sql::query("SELECT 'alpha'"))
            .await
            .expect("queries 1-tuple");
        assert_eq!(one, vec![("alpha".to_owned(),)]);

        let two = query_rows::<(i64, Option<i64>)>(
            &mut db,
            toasty::sql::query("SELECT 7, NULL UNION ALL SELECT 9, 42"),
        )
        .await
        .expect("queries 2-tuple");
        assert_eq!(two, vec![(7, None), (9, Some(42))]);

        let three = query_rows::<(String, Option<String>, bool)>(
            &mut db,
            toasty::sql::query("SELECT 'm1', NULL, 0 UNION ALL SELECT 'm2', 'att1', 1"),
        )
        .await
        .expect("queries 3-tuple");
        assert_eq!(
            three,
            vec![
                ("m1".to_owned(), None, false),
                ("m2".to_owned(), Some("att1".to_owned()), true),
            ]
        );

        let four = query_rows::<(String, String, Option<String>, i64)>(
            &mut db,
            toasty::sql::query("SELECT 'msg', '1.2', 'id9', 1024"),
        )
        .await
        .expect("queries 4-tuple");
        assert_eq!(
            four,
            vec![(
                "msg".to_owned(),
                "1.2".to_owned(),
                Some("id9".to_owned()),
                1024
            )]
        );

        assert_eq!(FromSqlValue::from_sql_value(Value::Bool(true)), Some(true));
        assert_eq!(
            <Option<String> as FromSqlValue>::from_sql_value(Value::I64(5)),
            None,
            "mismatched column types do not coerce to None"
        );

        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn every_pooled_connection_configures_busy_timeout() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-busy-timeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = open(&dir.join("busy.sqlite3")).await.expect("opens");
        // Hold two distinct pooled connections simultaneously so the pool must open at least two.
        let mut c1 = db.connection().await.expect("conn 1");
        let mut c2 = db.connection().await.expect("conn 2");
        assert_eq!(
            query_i64(&mut c1, "PRAGMA busy_timeout").await.expect("p1"),
            BUSY_TIMEOUT_MS
        );
        assert_eq!(
            query_i64(&mut c2, "PRAGMA busy_timeout").await.expect("p2"),
            BUSY_TIMEOUT_MS
        );
        drop(c1);
        drop(c2);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
