//! Eviction: the store lets old mail go, so its file stays the size of a few months' mail.
//!
//! - A conversation quiet for longer than the age goes whole, rows and all, unless something
//!   keeps it (below). Each list it was in covers less, because the list's coverage boundary
//!   moves forward past it (`db::coverage`). The list shows what's stored without a gap, and
//!   scrolling back past the boundary fetches the conversation from Gmail again, as backfill did
//!   the first time, starting where the stored mail ends (`ListCursor::before`).
//! - Of the rest, the bodies of those quiet for longer than the age go, and past the budget the
//!   oldest bodies go too. Their rows stay, like a conversation that sync stored for its rows
//!   alone, and opening one fetches its bodies ([`Store::load_bodies`]).
//! - The calendar's events that ended before the age go, except those on the days the calendar
//!   read this session.
//!
//! The file gives the space back to the disk (`auto_vacuum = INCREMENTAL`).
//!
//! Never evicted: the open conversation, the first threads of the lists that sync keeps bodies
//! for (it would fetch them again), and a conversation with a draft or a send of its own. The
//! rows, though not the bodies, of the inbox lists' first [`LIST_CAP`] threads (the reach of
//! their background passes) are kept too, and so are those of a conversation with a reminder or
//! a change Gmail hasn't had. Nothing Gmail doesn't have is evicted either: drafts and their
//! files, the outbox, held and scheduled sends, reminders, settings, unsubscribes, and the
//! calendar's queued changes.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use toasty::{Db, Executor, Json};

use crate::db::{
    self, AttachmentRow, BodyRow, CalendarChangeRow, EVICTED_BEFORE, EventRow, ListCursor,
    ListEntry, MessageRow, ThreadRow, query_col, query_i64, query_rows,
};
use crate::error::Result;
use crate::ids::ThreadId;
use crate::ids::labels::{DRAFT, INBOX, SH_ALL, SH_IMPORTANT, SH_OTHER, SH_SCHEDULED};
use crate::model::Header;
use crate::store::{Inner, Store, reindex};
use crate::sync::{LIST_CAP, ROW_HEADERS};
use crate::view::Router;

/// Conversations evicted per transaction. That's a few milliseconds of work, which is as long as
/// a sync or a write by the user waits for one.
const BATCH: usize = 20;
/// Events let go of per transaction.
const EVENT_BATCH: usize = 500;
/// The pause after each batch and each vacuum step, so sync and the user's writes go first.
const PAUSE: Duration = Duration::from_millis(50);
/// Pages each vacuum step gives back, and each merge step of the search index writes (4 MB at
/// SQLite's 4 KB pages).
const STEP: i64 = 1024;
/// Every conversation with a body stored, oldest first: its id, its newest message's date and
/// its bodies' bytes. `octet_length` reads a value's size from its record, not the value.
const STORED: &str = "SELECT s.thread_id, (SELECT max(n.internal_date) FROM messages n WHERE n.thread_id = s.thread_id) AS newest, s.bytes FROM (SELECT m.thread_id, sum(coalesce(octet_length(b.html), 0) + coalesce(octet_length(b.text), 0)) AS bytes FROM bodies b JOIN messages m ON m.id = b.message_id GROUP BY m.thread_id) s ORDER BY newest, s.thread_id";
/// Every conversation with no message since `?1` (epoch milliseconds), oldest first: its id and
/// its newest message's date.
const QUIET: &str = "SELECT thread_id, max(internal_date) AS newest FROM messages GROUP BY thread_id HAVING newest < ?1 ORDER BY newest, thread_id";
/// The conversations with a draft or a held or scheduled send. They're the ones in Drafts or
/// Scheduled, and the ones that one of Exo's drafts names, by thread or by message.
const DRAFTED: &str = "SELECT thread_id FROM list_ids WHERE list_id IN (?1, ?2) UNION SELECT thread_id FROM drafts WHERE thread_id IS NOT NULL UNION SELECT m.thread_id FROM drafts d JOIN messages m ON m.id = d.message_id";
/// The conversations with a reminder or a change Gmail hasn't had.
const PENDING: &str = "SELECT thread_id FROM reminders UNION SELECT queue FROM outbox";
/// The first `?2` threads of list `?1`.
const FIRST: &str = "SELECT thread_id FROM list_ids WHERE list_id = ?1 ORDER BY sort DESC LIMIT ?2";
/// The events that ended before `?1` (epoch milliseconds).
const ENDED: &str = r#"SELECT calendar_id, id, start, "end" FROM events WHERE "end" < ?1"#;

/// What a store keeps: conversations active in the last `age`, their bodies as long as they fit
/// in `budget`, and the calendar's events since then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    /// A conversation with no message in this long goes, or loses its bodies when something
    /// keeps it. So does an event that ended this long ago.
    pub age: SignedDuration,
    /// The most the bodies may take, in bytes of HTML and text. Past it the oldest go too, down
    /// to nine tenths of it, so the next day's mail doesn't start another pass.
    pub budget: u64,
}

impl Default for Eviction {
    /// Three months of conversations, within 256 MB.
    fn default() -> Self {
        Self {
            age: SignedDuration::from_hours(90 * 24),
            budget: 256 << 20,
        }
    }
}

/// What one [`Store::evict`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evicted {
    /// Conversations that went whole, rows and all.
    pub rows: usize,
    /// Conversations whose bodies went, their rows kept.
    pub threads: usize,
    /// The bytes of HTML and text in the bodies that went, with whole conversations or alone.
    pub bytes: u64,
    /// The calendar's events that went.
    pub events: usize,
    /// Bytes the database file gave back to the disk, from pages freed by eviction or anything
    /// else.
    pub reclaimed: u64,
}

/// A conversation with a body stored.
struct Stored {
    thread: String,
    /// Its newest message's date, epoch milliseconds.
    newest: i64,
    bytes: u64,
}

/// A conversation quiet since before eviction's age.
struct Quiet {
    thread: String,
    /// Its newest message's date, epoch milliseconds.
    newest: i64,
}

/// What [`kept`] keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    Bodies,
    Rows,
}

impl Store {
    /// Says which conversation is open on screen (`None` for none), which
    /// [`evict`](Self::evict) leaves alone. Call it before checking
    /// [`bodies_stored`](Self::bodies_stored), because it waits for a batch of eviction in
    /// progress, so what that check finds is what stays.
    pub async fn reading(&self, thread: Option<&ThreadId>) {
        *self.inner.reading.lock().await = thread.map(|thread| thread.as_str().to_owned());
    }

    /// Evicts the conversations, bodies and events `eviction` doesn't keep, oldest first, then
    /// compacts the search index and gives the space back to the disk. It works in small batches
    /// and steps with pauses between them, each holding the sync lock, so sync and the user's
    /// writes wait for one at most. It calls Gmail for nothing. It tells
    /// [`changes`](Self::changes) once when conversations went, since lists then cover less. The
    /// conversation on screen and the rows that the list on screen has loaded aren't touched.
    ///
    /// # Errors
    /// On database failures. What was evicted before the failure stays evicted.
    pub async fn evict(&self, eviction: &Eviction) -> Result<Evicted> {
        let inner = &self.inner;
        let mut db = inner.db.clone();
        // What the file gives back is measured from here, because the pass itself writes pages
        // too.
        let pages = query_i64(&mut db, "PRAGMA page_count").await?;
        let now = Timestamp::now();
        let cutoff = (now - eviction.age).as_millisecond();
        let reading = inner.reading.lock().await.clone();
        let quiet = quiet(&mut db, cutoff).await?;
        let kept_rows = kept(&mut db, inner, reading.as_deref(), Keep::Rows).await?;
        let gone: Vec<&Quiet> = quiet
            .iter()
            .filter(|quiet| !kept_rows.contains(&quiet.thread))
            .collect();
        let mut stored = stored(&mut db).await?;
        let bytes: HashMap<String, u64> = stored
            .iter()
            .map(|stored| (stored.thread.clone(), stored.bytes))
            .collect();
        let going: HashSet<&str> = gone.iter().map(|quiet| quiet.thread.as_str()).collect();
        stored.retain(|stored| !going.contains(stored.thread.as_str()));
        let doomed = plan(
            &stored,
            &kept(&mut db, inner, reading.as_deref(), Keep::Bodies).await?,
            eviction,
            now,
        );
        let mut evicted = Evicted::default();
        for batch in gone.chunks(BATCH) {
            let mut parts = Vec::new();
            {
                let _sync = inner.sync.lock().await;
                let router = inner.router();
                let mut write = inner.begin(&mut db).await?;
                let reading = inner.reading.lock().await;
                // Kept if opened, loaded, drafted or changed since the plan.
                let kept = kept(write.exec(), inner, reading.as_deref(), Keep::Rows).await?;
                let mut latest: HashMap<String, i64> = HashMap::new();
                for quiet in batch.iter().filter(|quiet| !kept.contains(&quiet.thread)) {
                    if let Some(removed) = evict_rows(write.exec(), quiet, &router).await? {
                        evicted.rows += 1;
                        evicted.bytes += bytes.get(&quiet.thread).copied().unwrap_or_default();
                        for entry in removed.entries {
                            let sort = latest.entry(entry.list_id).or_insert(entry.sort);
                            *sort = (*sort).max(entry.sort);
                        }
                        parts.extend(removed.parts);
                    }
                }
                // In the same transaction, so no list ever shows a gap.
                uncover(write.exec(), &latest, &router).await?;
                write.commit().await?;
            }
            // Their files go with them.
            self.forget_parts(&parts);
            tokio::time::sleep(PAUSE).await;
        }
        // The newest conversation let go. The pre-render doesn't fetch it, or anything older,
        // back.
        let mut before = db::kv_get::<i64>(&mut db, EVICTED_BEFORE)
            .await?
            .unwrap_or(i64::MIN);
        for batch in doomed.chunks(BATCH) {
            let mut parts = Vec::new();
            {
                let _sync = inner.sync.lock().await;
                let router = inner.router();
                let mut write = inner.begin(&mut db).await?;
                let reading = inner.reading.lock().await;
                // Kept if opened or drafted since the plan.
                let kept = kept(write.exec(), inner, reading.as_deref(), Keep::Bodies).await?;
                for stored in batch.iter().filter(|stored| !kept.contains(&stored.thread)) {
                    if let Some(messages) = evict_thread(write.exec(), stored, &router).await? {
                        evicted.threads += 1;
                        evicted.bytes += stored.bytes;
                        before = before.max(stored.newest);
                        for chunk in messages.chunks(db::IN_LIST_CHUNK) {
                            let rows = AttachmentRow::filter(
                                AttachmentRow::fields().message_id().in_list(chunk),
                            )
                            .exec(write.exec())
                            .await?;
                            parts.extend(rows.into_iter().map(|row| (row.message_id, row.part_id)));
                        }
                    }
                }
                if evicted.threads > 0 {
                    db::kv_set(write.exec(), EVICTED_BEFORE, &before).await?;
                }
                write.commit().await?;
            }
            self.forget_parts(&parts);
            tokio::time::sleep(PAUSE).await;
        }
        evicted.events = evict_events(inner, &mut db, cutoff).await?;
        if evicted.rows > 0 || evicted.threads > 0 {
            compact_index(inner, &mut db).await?;
        }
        evicted.reclaimed = reclaim(inner, &mut db, pages).await?;
        if evicted.rows > 0 {
            inner.notify();
        }
        if evicted.rows > 0 || evicted.threads > 0 || evicted.events > 0 {
            log::info!(
                "evicted {} conversations and the bodies of {} more ({} bytes), and {} events; the file gave back {} bytes",
                evicted.rows,
                evicted.threads,
                evicted.bytes,
                evicted.events,
                evicted.reclaimed
            );
        }
        Ok(evicted)
    }
}

/// Makes the file give back the pages that eviction frees (`auto_vacuum = INCREMENTAL`), once. A
/// file without the setting is rebuilt with it (`VACUUM`, which can't run in a migration's
/// transaction). That takes no time for a new file, and about as long as a copy for a file an
/// older Exo wrote.
pub(crate) async fn reclaimable(db: &Db) -> Result<()> {
    let mut connection = db.connection().await?;
    // `auto_vacuum` takes effect with the `VACUUM` on the connection that set it.
    if query_i64(&mut connection, "PRAGMA auto_vacuum").await? == AUTO_VACUUM_INCREMENTAL {
        return Ok(());
    }
    toasty::sql::statement("PRAGMA auto_vacuum = INCREMENTAL")
        .exec(&mut connection)
        .await?;
    toasty::sql::statement("VACUUM")
        .exec(&mut connection)
        .await?;
    // The rebuild went through the WAL, so empty it.
    toasty::sql::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .exec(&mut connection)
        .await?;
    Ok(())
}

/// `PRAGMA auto_vacuum`'s value for `INCREMENTAL`.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Every conversation with a body stored ([`STORED`]).
async fn stored(exec: &mut dyn Executor) -> Result<Vec<Stored>> {
    Ok(
        query_rows::<(String, i64, i64)>(exec, toasty::sql::query(STORED))
            .await?
            .into_iter()
            .map(|(thread, newest, bytes)| Stored {
                thread,
                newest,
                bytes: u64::try_from(bytes).unwrap_or_default(),
            })
            .collect(),
    )
}

/// Every conversation with no message since `cutoff` ([`QUIET`]).
async fn quiet(exec: &mut dyn Executor, cutoff: i64) -> Result<Vec<Quiet>> {
    Ok(
        query_rows::<(String, i64)>(exec, toasty::sql::query(QUIET).bind(cutoff))
            .await?
            .into_iter()
            .map(|(thread, newest)| Quiet { thread, newest })
            .collect(),
    )
}

/// The conversations that eviction leaves alone: the open one, the first threads of each list
/// that sync fetches bodies for ([`Inner::prefetched_lists`]), and those with a draft or a send.
/// For their rows it also leaves the inbox lists' first [`LIST_CAP`], which their passes list (if
/// evicted, the next pass would fetch them again), and those with a reminder or a change Gmail
/// hasn't had.
async fn kept(
    exec: &mut dyn Executor,
    inner: &Inner,
    reading: Option<&str>,
    keep: Keep,
) -> Result<HashSet<String>> {
    let mut kept: HashSet<String> = reading.map(str::to_owned).into_iter().collect();
    let router = inner.router();
    let inbox: Vec<&str> = [INBOX, SH_IMPORTANT, SH_OTHER]
        .into_iter()
        .chain(router.splits.routed_ids())
        .collect();
    for (list, depth) in inner.prefetched_lists() {
        let depth = if keep == Keep::Rows && inbox.contains(&list.as_str()) {
            depth.max(LIST_CAP)
        } else {
            depth
        };
        kept.extend(
            query_col::<String>(
                exec,
                toasty::sql::query(FIRST).bind(list).bind(i64::from(depth)),
            )
            .await?,
        );
    }
    kept.extend(
        query_col::<String>(
            exec,
            toasty::sql::query(DRAFTED).bind(DRAFT).bind(SH_SCHEDULED),
        )
        .await?,
    );
    if keep == Keep::Rows {
        kept.extend(query_col::<String>(exec, toasty::sql::query(PENDING)).await?);
    }
    Ok(kept)
}

/// Which of `stored` (oldest first) go: every one with no message in `eviction.age`, then,
/// when the bodies take more than the budget, the oldest of the rest until they fit in nine
/// tenths of it. Those `kept` stay, and their bytes count.
fn plan<'a>(
    stored: &'a [Stored],
    kept: &HashSet<String>,
    eviction: &Eviction,
    now: Timestamp,
) -> Vec<&'a Stored> {
    let total: u64 = stored.iter().map(|stored| stored.bytes).sum();
    let target = if total > eviction.budget {
        eviction.budget / 10 * 9
    } else {
        u64::MAX
    };
    let cutoff = (now - eviction.age).as_millisecond();
    let mut left = total;
    let mut doomed = Vec::new();
    for stored in stored
        .iter()
        .filter(|stored| !kept.contains(&stored.thread))
    {
        if stored.newest >= cutoff && left <= target {
            break;
        }
        left -= stored.bytes;
        doomed.push(stored);
    }
    doomed
}

/// Drops the conversation's bodies and the headers that came with them, and indexes it again
/// without their text. That leaves its messages as sync stores them for their rows, but with
/// their attachment rows kept, since lists show them. Returns its messages' ids if it went ahead.
/// A conversation with mail since the plan stays.
async fn evict_thread(
    exec: &mut dyn Executor,
    stored: &Stored,
    router: &Router,
) -> Result<Option<Vec<String>>> {
    let messages = MessageRow::filter_by_thread_id(&stored.thread)
        .exec(exec)
        .await?;
    if messages.iter().map(|message| message.internal_date).max() != Some(stored.newest) {
        return Ok(None);
    }
    let ids: Vec<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    for chunk in ids.chunks(db::IN_LIST_CHUNK) {
        BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
    }
    for message in &messages {
        let headers: Vec<Header> = message
            .headers
            .iter()
            .filter(|header| {
                ROW_HEADERS
                    .iter()
                    .any(|name| header.name.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        if headers.len() < message.headers.len() {
            MessageRow::update_by_id(&message.id)
                .headers(Json(headers))
                .exec(exec)
                .await?;
        }
    }
    reindex(exec, &stored.thread, router).await?;
    Ok(Some(
        messages.into_iter().map(|message| message.id).collect(),
    ))
}

/// What [`evict_rows`] took out of the lists and the parts cache.
struct Removed {
    /// The conversation's list membership, with its sort in each list.
    entries: Vec<ListEntry>,
    /// Its messages' parts, by message and part id.
    parts: Vec<(String, String)>,
}

/// Drops the conversation: its messages with their bodies and attachment rows, its list
/// membership, participants and search row. Returns what it was in if it went ahead. A
/// conversation with mail since the plan stays.
async fn evict_rows(
    exec: &mut dyn Executor,
    quiet: &Quiet,
    router: &Router,
) -> Result<Option<Removed>> {
    let messages: Vec<(String, i64)> = MessageRow::filter_by_thread_id(&quiet.thread)
        .select((
            MessageRow::fields().id(),
            MessageRow::fields().internal_date(),
        ))
        .exec(exec)
        .await?;
    if messages.iter().map(|(_, date)| *date).max() != Some(quiet.newest) {
        return Ok(None);
    }
    let ids: Vec<String> = messages.into_iter().map(|(id, _)| id).collect();
    let entries = ListEntry::filter_by_thread_id(&quiet.thread)
        .exec(exec)
        .await?;
    let mut parts = Vec::new();
    for chunk in ids.chunks(db::IN_LIST_CHUNK) {
        parts.extend(
            AttachmentRow::filter(AttachmentRow::fields().message_id().in_list(chunk))
                .exec(exec)
                .await?
                .into_iter()
                .map(|row| (row.message_id, row.part_id)),
        );
    }
    crate::provider::delete_messages(exec, &ids).await?;
    ThreadRow::filter_by_id(&quiet.thread)
        .delete()
        .exec(exec)
        .await?;
    // With no message left, its lists, participants and search row go.
    reindex(exec, &quiet.thread, router).await?;
    Ok(Some(Removed { entries, parts }))
}

/// Moves the coverage of each list that held a conversation just evicted forward past it, so
/// the list shows what's stored without a gap (`db::coverage`). `latest` holds, for each list,
/// the latest sort at which it had an evicted conversation. A list's coverage is its own pass's
/// reach or All Mail's, whichever is further. So All Mail's coverage moves past every evicted
/// conversation, and the Inbox's, which its tabs share, moves past its tabs' and splits'. A pass
/// this moves restarts from where the stored mail ends (`before:`).
async fn uncover(
    exec: &mut dyn Executor,
    latest: &HashMap<String, i64>,
    router: &Router,
) -> Result<()> {
    let Some(&newest) = latest.values().max() else {
        return Ok(());
    };
    let splits = router.splits.routed_ids();
    for cursor in ListCursor::all().exec(exec).await? {
        let held = match cursor.id.as_str() {
            SH_ALL => Some(newest),
            INBOX => [INBOX, SH_IMPORTANT, SH_OTHER]
                .into_iter()
                .chain(splits.iter().copied())
                .filter_map(|list| latest.get(list))
                .max()
                .copied(),
            list => latest.get(list).copied(),
        };
        let Some(held) = held else { continue };
        let floor = held + 1;
        // A pass that hasn't reached it yet lists it when it does.
        if !cursor.complete && cursor.min_sort.is_none_or(|reached| reached >= floor) {
            continue;
        }
        ListCursor::filter_by_id(&cursor.id)
            .update()
            .page_token(None::<String>)
            .min_sort(Some(floor))
            .complete(false)
            .before(Some(floor))
            .exec(exec)
            .await?;
    }
    Ok(())
}

/// Lets go of the calendar's events that ended before `cutoff`, except those on the days the
/// calendar read this session, which it shows again without fetching them. It lets go of none
/// while a change to an event waits to go, since the change shows over the event it changes.
/// Returns how many went.
async fn evict_events(inner: &Inner, db: &mut Db, cutoff: i64) -> Result<usize> {
    let read = inner
        .calendar_read
        .lock()
        .expect("calendar-read lock is never poisoned")
        .clone();
    let ended =
        query_rows::<(String, String, i64, i64)>(&mut *db, toasty::sql::query(ENDED).bind(cutoff))
            .await?;
    let mut gone: HashMap<String, Vec<String>> = HashMap::new();
    for (calendar, id, start, end) in ended {
        if !read.iter().any(|&(from, to)| start < to && end > from) {
            gone.entry(calendar).or_default().push(id);
        }
    }
    let mut evicted = 0;
    for (calendar, ids) in &gone {
        for ids in ids.chunks(EVENT_BATCH) {
            let mut write = inner.begin(db).await?;
            if CalendarChangeRow::all()
                .first()
                .exec(write.exec())
                .await?
                .is_some()
            {
                return Ok(evicted);
            }
            EventRow::filter(
                EventRow::fields()
                    .calendar_id()
                    .eq(calendar)
                    .and(EventRow::fields().id().in_list(ids)),
            )
            .delete()
            .exec(write.exec())
            .await?;
            write.commit().await?;
            evicted += ids.len();
            tokio::time::sleep(PAUSE).await;
        }
    }
    Ok(evicted)
}

/// Merges the search index into one segment a step at a time. FTS5 only marks a deleted row's
/// words as gone and drops them when it merges them, so until then evicted text still takes its
/// space, and the marks take more.
async fn compact_index(inner: &Inner, db: &mut Db) -> Result<()> {
    loop {
        let merged = {
            let _sync = inner.sync.lock().await;
            let mut write = inner.begin(db).await?;
            let before = query_i64(write.exec(), "SELECT total_changes()").await?;
            // A negative step merges every segment, however few, into one (FTS5's `merge`
            // command).
            toasty::sql::statement(format!(
                "INSERT INTO search_fts (search_fts, rank) VALUES ('merge', -{STEP})"
            ))
            .exec(write.exec())
            .await?;
            let after = query_i64(write.exec(), "SELECT total_changes()").await?;
            write.commit().await?;
            // FTS5's own test for a merge that did nothing.
            after - before >= 2
        };
        if !merged {
            return Ok(());
        }
        tokio::time::sleep(PAUSE).await;
    }
}

/// Gives the file's free pages back to the disk a step at a time, then empties the WAL the
/// steps went through. Returns how many bytes the database file shrank by from `before` pages.
async fn reclaim(inner: &Inner, db: &mut Db, before: i64) -> Result<u64> {
    let page = query_i64(db, "PRAGMA page_size").await?;
    // Only what's free now. Pages freed meanwhile wait for the next pass.
    let mut free = query_i64(db, "PRAGMA freelist_count").await?;
    while free > 0 {
        {
            let _sync = inner.sync.lock().await;
            let mut write = inner.begin(db).await?;
            toasty::sql::query(format!("PRAGMA incremental_vacuum({STEP})"))
                .exec(write.exec())
                .await?;
            write.commit().await?;
        }
        free -= STEP;
        tokio::time::sleep(PAUSE).await;
    }
    // The checkpoint is busy while another connection reads, and then the next pass, or
    // SQLite's own checkpoint, empties the WAL.
    toasty::sql::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .exec(db)
        .await?;
    let after = query_i64(db, "PRAGMA page_count").await?;
    Ok(u64::try_from((before - after) * page).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(thread: &str, days_ago: i64, bytes: u64) -> Stored {
        let newest = (Timestamp::UNIX_EPOCH + SignedDuration::from_hours(24 * (1000 - days_ago)))
            .as_millisecond();
        Stored {
            thread: thread.to_owned(),
            newest,
            bytes,
        }
    }

    fn doomed(stored: &[Stored], kept: &[&str], age_days: i64, budget: u64) -> Vec<String> {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(24 * 1000);
        let kept = kept.iter().map(|id| (*id).to_owned()).collect();
        let eviction = Eviction {
            age: SignedDuration::from_hours(24 * age_days),
            budget,
        };
        plan(stored, &kept, &eviction, now)
            .into_iter()
            .map(|stored| stored.thread.clone())
            .collect()
    }

    #[test]
    fn conversations_quiet_for_longer_than_the_age_go() {
        let all = [
            stored("a", 200, 10),
            stored("b", 91, 10),
            stored("c", 89, 10),
            stored("d", 1, 10),
        ];
        assert_eq!(doomed(&all, &[], 90, 1000), ["a", "b"]);
        assert_eq!(
            doomed(&all, &["a"], 90, 1000),
            ["b"],
            "kept ones stay however old"
        );
    }

    #[test]
    fn past_the_budget_the_oldest_go_until_nine_tenths_of_it_is_left() {
        let all = [
            stored("a", 30, 400),
            stored("b", 20, 300),
            stored("c", 10, 200),
            stored("d", 1, 100),
        ];
        assert!(
            doomed(&all, &[], 90, 1000).is_empty(),
            "1000 bytes fit in 1000"
        );
        // 1000 bytes in a 900 budget: down to 810.
        assert_eq!(doomed(&all, &[], 90, 900), ["a"]);
        // In 500: down to 450, the newest's 100 and the kept 300 left.
        assert_eq!(doomed(&all, &["b"], 90, 500), ["a", "c"]);
        // What's kept counts, and stays even when nothing else is left to go.
        assert_eq!(doomed(&all, &["a", "b", "c"], 90, 100), ["d"]);
    }
}
