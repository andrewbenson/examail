//! Pushing local drafts to Gmail: create/update, send, discard deletion, and failure recovery.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use jiff::{SignedDuration, Timestamp};

use super::files::local_files;
use super::model::{
    DraftFailure, DraftRecord, DraftReport, SendFailure, SendLater, SentDraft, UNTIL_COMPLETE,
    complete, scheduled,
};
use crate::api::Failure;
use crate::db::{self, DraftRow, GIVEN_REMINDERS, MessageRow, ThreadRow};
use crate::error::Result;
use crate::ids::ThreadId;
use crate::ids::labels::{DRAFT, SENT};
use crate::parse::parse;
use crate::provider;
use crate::queue::{self, Disposition};
use crate::quota::Lane;
use crate::store::{Inner, Store, reindex, reindex_many};
use crate::sync::{fetch_message, fetch_row, fetch_rows};

/// Attempts at `drafts.send` before Gmail's server errors fail the send (with the call's backoff,
/// about 7 s of trying).
const SEND_ATTEMPTS: u32 = 4;
/// The longest rate limit that a due send waits out. Gmail asking for longer fails the send.
const SEND_WAITS: SignedDuration = SignedDuration::from_mins(2);
/// A rate limit this long is Gmail's daily sending limit ("multiple hours", in Gmail's usage
/// limits).
const SENDING_LIMIT: SignedDuration = SignedDuration::from_hours(1);

/// Metadata of a draft row used during [`Store::push_drafts_with`] scheduling, loaded without the
/// RFC 822 `raw` blob.
struct DraftPushMeta {
    id: String,
    gmail_id: Option<String>,
    record: DraftRecord,
    revision: u64,
    pushed: u64,
    discarded: bool,
    send_at: Option<Timestamp>,
    failures: u32,
    not_before: Option<Timestamp>,
}

impl db::FromRecord for DraftPushMeta {
    fn from_record(fields: Vec<toasty::stmt::Value>) -> Option<Self> {
        let [
            id,
            gmail_id,
            record,
            revision,
            pushed,
            discarded,
            send_at,
            failures,
            not_before,
        ]: [toasty::stmt::Value; 9] = fields.try_into().ok()?;
        let record: toasty::Json<DraftRecord> = db::FromSqlValue::from_sql_value(record)?;
        Some(Self {
            id: db::FromSqlValue::from_sql_value(id)?,
            gmail_id: db::FromSqlValue::from_sql_value(gmail_id)?,
            record: record.0,
            revision: db::FromSqlValue::from_sql_value(revision)?,
            pushed: db::FromSqlValue::from_sql_value(pushed)?,
            discarded: db::FromSqlValue::from_sql_value(discarded)?,
            send_at: db::FromSqlValue::from_sql_value(send_at)?,
            failures: db::FromSqlValue::from_sql_value(failures)?,
            not_before: db::FromSqlValue::from_sql_value(not_before)?,
        })
    }
}

impl DraftPushMeta {
    fn needs_push(&self, now: Timestamp) -> bool {
        self.discarded
            || self.pushed < self.revision
            || self.send_at.is_some_and(|at| at <= now)
            || self.failures > 0
    }
}

impl Store {
    /// Writes what Gmail doesn't have yet: discarded drafts are deleted, saved ones created or
    /// updated, and held ones whose time has come sent. A draft's edits wait out its pace
    /// ([`Options::draft_pace`](crate::Options::draft_pace)) after its last write, but a send's
    /// edits don't. One push runs at a time.
    ///
    /// # Errors
    /// On database failures. Gmail failures are retried later or reported in the result.
    pub async fn push_drafts(&self) -> Result<DraftReport> {
        self.push_drafts_with(false).await
    }

    /// [`push_drafts`](Self::push_drafts) without waiting out paces or backoffs, for an account
    /// closing or left (quitting, Google signing it out, switching to another, signing out).
    /// Each draft is tried once more, so Gmail has every edit, and a send due then goes or fails
    /// now rather than on the next launch.
    ///
    /// # Errors
    /// On database failures.
    pub async fn push_drafts_now(&self) -> Result<DraftReport> {
        self.push_drafts_with(true).await
    }

    async fn push_drafts_with(&self, closing: bool) -> Result<DraftReport> {
        let inner = &self.inner;
        let _flush = inner.draft_flush.lock().await;
        let mut report = DraftReport::default();
        let mut db = inner.db.clone();
        let now = Timestamp::now();
        let rows = db::query_rows::<DraftPushMeta>(
            &mut db,
            toasty::sql::query(
                "SELECT id, gmail_id, record, revision, pushed, discarded, send_at, failures, not_before FROM drafts",
            ),
        )
        .await?;
        inner
            .written
            .lock()
            .expect("never poisoned")
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
        inner
            .acked
            .lock()
            .expect("never poisoned")
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
        inner.uploads.send_if_modified(|uploads| {
            let before = uploads.len();
            uploads.retain(|id, _| rows.iter().any(|row| row.id == *id));
            uploads.len() != before
        });
        for row in &rows {
            inner.note_draft_meta(&row.id, row.pushed, row.revision, &row.record);
        }
        let batch = queue::select_due(
            rows.into_iter()
                .filter(|row| row.not_before != Some(UNTIL_COMPLETE)),
            now,
            closing,
            |row| row.id.clone(),
            |row| row.not_before,
        );
        report.retry_at = batch.retry_at;
        for meta in batch.due {
            if let Some(at) = inner.paced_meta(&meta, now).filter(|_| !closing) {
                report.retry_at = queue::earliest(report.retry_at, at);
                continue;
            }
            if !meta.needs_push(now) {
                continue;
            }
            let Some(row) = DraftRow::filter_by_id(&meta.id)
                .first()
                .exec(&mut db)
                .await?
            else {
                continue;
            };
            if !complete(&row) {
                continue;
            }
            let id = row.id.clone();
            let failures = row.failures;
            let held = row.send_at.is_some();
            let was_discarded = row.discarded;
            if let Err((failed_revision, failure)) = inner.push(row, now, &mut report).await? {
                let offline = matches!(failure, Failure::Offline(_));
                report.offline |= offline;
                if held && !offline {
                    report.retrying.push(id.clone());
                }
                let message = failure.message();
                let at = match queue::dispose_failure(&inner.api, failure, failures) {
                    Disposition::Postponed(until) => until,
                    Disposition::Refused(_) => inner.api.postpone(failures),
                };
                log::debug!("postponing draft {id} until {at}: {message}");
                let mut write = inner.begin(&mut db).await?;
                if let Some(current) = DraftRow::filter_by_id(&id)
                    .first()
                    .exec(write.exec())
                    .await?
                    && current.revision == failed_revision
                    && current.discarded == was_discarded
                    && complete(&current)
                {
                    DraftRow::update_by_id(&id)
                        .failures(failures.saturating_add(1))
                        .not_before(Some(at))
                        .exec(write.exec())
                        .await?;
                }
                write.commit().await?;
                report.retry_at = queue::earliest(report.retry_at, at);
            }
        }
        // Held sends coming due.
        let f = DraftRow::fields();
        for (discarded, send_at) in DraftRow::all()
            .select((f.discarded(), f.send_at()))
            .exec(&mut db)
            .await?
        {
            if let Some(at) = send_at.filter(|at| *at > now && !discarded) {
                report.retry_at = queue::earliest(report.retry_at, at);
            }
        }
        Ok(report)
    }
}

/// `call`, reporting its upload to `progress` when there's one.
fn reporting<'a>(
    call: gmail::Call<'a, gmail::Draft>,
    progress: Option<&gmail::Progress>,
) -> gmail::Call<'a, gmail::Draft> {
    match progress {
        Some(progress) => call.on_progress(progress.clone()),
        None => call,
    }
}

/// A hash of what Gmail keeps of a draft's message `raw`, which is all of it but the `Date:`
/// that every save stamps afresh. The rest follows from the draft (its Message-ID is kept across
/// saves, and MIME boundaries are derived from the content).
pub(crate) fn content(raw: &str) -> u64 {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let mut hasher = DefaultHasher::new();
    head.split("\r\n")
        .filter(|line| !line.starts_with("Date:"))
        .for_each(|line| line.hash(&mut hasher));
    body.hash(&mut hasher);
    hasher.finish()
}

impl Inner {
    /// The [`content`] of what Gmail holds for `row`, when Exo knows it. That's what Exo last
    /// wrote of it this session while Gmail's draft still holds that message, or else the row's
    /// own content once Gmail has its latest revision.
    pub(super) fn gmail_has(&self, row: &DraftRow) -> Option<u64> {
        let message = row.message_id.as_ref()?;
        let acked = self
            .acked
            .lock()
            .expect("never poisoned")
            .get(&row.id)
            .filter(|(acked, _)| acked == message)
            .map(|(_, content)| *content);
        acked.or_else(|| (row.pushed >= row.revision).then(|| content(&row.raw)))
    }

    /// When `row`'s edits may be written to Gmail, if not now. A draft being typed in waits its
    /// pace after its last write this session ([`Options::draft_pace`](crate::Options::draft_pace),
    /// or `draft_files_pace` with files). These writes go at once: one that makes its Gmail
    /// draft, a send's, a discard and one carrying a file Gmail lacks.
    fn paced_meta(&self, row: &DraftPushMeta, now: Timestamp) -> Option<Timestamp> {
        if row.pushed >= row.revision
            || row.discarded
            || row.send_at.is_some()
            || row.gmail_id.is_none()
            || self.uploads_files_meta(&row.id, &row.record)
        {
            return None;
        }
        let pace = if row.record.attachments.is_empty() {
            self.draft_pace
        } else {
            self.draft_files_pace
        };
        let written = *self.written.lock().expect("never poisoned").get(&row.id)?;
        written.checked_add(pace).ok().filter(|at| *at > now)
    }

    /// Brings one draft's Gmail copy up to date. A failure worth retrying is returned with the
    /// draft revision it failed on so the caller can back off without clobbering concurrent saves.
    /// Database errors are the outer `Result`.
    async fn push(
        &self,
        row: DraftRow,
        now: Timestamp,
        report: &mut DraftReport,
    ) -> Result<Result<(), (u64, Failure)>> {
        if row.discarded {
            let revision = row.revision;
            return Ok(self
                .delete_gmail_draft(row)
                .await?
                .map_err(|failure| (revision, failure)));
        }
        let mut row = row;
        if row.send_at.is_some_and(|at| at <= now) && row.record.0.send_later.is_some() {
            // A draft rewritten in Gmail web since Exo last wrote it goes as Gmail has it, edits
            // and all, rather than with Exo's copy written over them.
            let rewritten = if row.pushed >= row.revision {
                match self.rewritten_elsewhere(&row).await {
                    Ok(rewritten) => rewritten,
                    Err(failure) => return Ok(Err((row.revision, failure))),
                }
            } else {
                false
            };
            if rewritten {
                log::info!(
                    "draft {} was rewritten outside Exo: sending Gmail's copy",
                    row.id
                );
            } else if !self.redate(&mut row, now).await? {
                return Ok(Ok(()));
            }
        }
        if row.pushed < row.revision {
            let (id, revision, send_at) = (row.id.clone(), row.revision, row.send_at);
            match self.write_gmail_draft(row).await? {
                Ok(written) => {
                    row = written;
                    report.saved.push(row.id.clone());
                    // A new message's thread is stored now, so a held send's reminder can go on
                    // it.
                    if row.send_at.is_some() {
                        report.reminded |= self.give_reminder(&row.id).await?;
                    }
                }
                Err(failure) => {
                    let refused = failure.is_permanent();
                    if refused {
                        log::warn!("Gmail refused draft {id}: {}", failure.message());
                        // Given up for this version. The next save tries again; preserve any
                        // concurrent save's `not_before` (e.g. `UNTIL_COMPLETE`).
                        let mut db = self.db.clone();
                        let mut write = self.begin(&mut db).await?;
                        if let Some(current) = DraftRow::filter_by_id(&id)
                            .first()
                            .exec(write.exec())
                            .await?
                        {
                            let mut update =
                                DraftRow::update_by_id(&id).pushed(current.pushed.max(revision));
                            if current.revision == revision {
                                update = update.failures(0).not_before(None);
                            }
                            update.exec(write.exec()).await?;
                        }
                        write.publish().await?;
                    }
                    // When the message a send holds can't be written, the send fails. A save's
                    // server errors are retried, as for any draft.
                    let failed = send_at.and_then(|at| match &failure {
                        Failure::Transient(_) => None,
                        failure => verdict(failure, at <= now, now),
                    });
                    if let Some(reason) = failed {
                        self.fail_send(&id, revision, reason, report).await?;
                        return Ok(Ok(()));
                    }
                    if !refused {
                        return Ok(Err((revision, failure)));
                    }
                    report.failed.push(DraftFailure::Save {
                        id,
                        error: failure.message(),
                    });
                    return Ok(Ok(()));
                }
            }
        }
        if row.send_at.is_some_and(|at| at <= now) {
            return self.send_gmail_draft(&row.id, now, report).await;
        }
        if row.failures > 0 {
            let mut db = self.db.clone();
            let mut write = self.begin(&mut db).await?;
            if let Some(current) = DraftRow::filter_by_id(&row.id)
                .first()
                .exec(write.exec())
                .await?
                && current.revision == row.revision
            {
                DraftRow::update_by_id(&row.id)
                    .failures(0)
                    .not_before(None)
                    .exec(write.exec())
                    .await?;
            }
            write.commit().await?;
        }
        Ok(Ok(()))
    }

    /// Dates a scheduled send's message now, as it goes, rather than when Schedule was pressed,
    /// because Gmail sends a draft's `Date:` as written and recipients see it. The new date is a
    /// new revision, so the push writes it before sending. A date within a minute of now is left
    /// as it is. Returns `false` if a concurrent edit, discard, or unschedule superseded `row`.
    async fn redate(&self, row: &mut DraftRow, now: Timestamp) -> Result<bool> {
        let zoned = now.to_zoned(jiff::tz::TimeZone::system());
        let fresh = |line: &str| {
            line.strip_prefix("Date: ")
                .and_then(|date| jiff::fmt::rfc2822::parse(date).ok())
                .is_some_and(|date| {
                    date.timestamp().duration_until(now).abs() < SignedDuration::from_mins(1)
                })
        };
        let (head, body) = row.raw.split_once("\r\n\r\n").unwrap_or((&row.raw, ""));
        if head.split("\r\n").any(fresh) {
            return Ok(true);
        }
        let Ok(date) = jiff::fmt::rfc2822::to_string(&zoned) else {
            return Ok(true);
        };
        let mut lines: Vec<String> = head
            .split("\r\n")
            .filter(|line| !line.starts_with("Date:"))
            .map(str::to_owned)
            .collect();
        lines.insert(0, format!("Date: {date}"));
        let redated_raw = format!("{}\r\n\r\n{body}", lines.join("\r\n"));
        let next_revision = row.revision + 1;
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let Some(current) = DraftRow::filter_by_id(&row.id)
            .first()
            .exec(write.exec())
            .await?
        else {
            write.commit().await?;
            return Ok(false);
        };
        if current.revision != row.revision
            || current.discarded
            || !current.send_at.is_some_and(|at| at <= now)
            || !complete(&current)
        {
            write.commit().await?;
            return Ok(false);
        }
        DraftRow::update_by_id(&row.id)
            .raw(redated_raw.clone())
            .revision(next_revision)
            .exec(write.exec())
            .await?;
        write.commit().await?;
        row.raw = redated_raw;
        row.revision = next_revision;
        Ok(true)
    }

    /// Whether `row`'s Gmail draft holds another message than the one Exo last wrote (someone
    /// edited it in Gmail web, say). A draft that's gone is the send's to find out about.
    async fn rewritten_elsewhere(&self, row: &DraftRow) -> std::result::Result<bool, Failure> {
        let (Some(gmail_id), Some(message)) = (&row.gmail_id, &row.message_id) else {
            return Ok(false);
        };
        match self
            .api
            .call(Lane::Act, 2, |gmail| {
                gmail.draft(gmail_id, gmail::Format::Minimal)
            })
            .await
        {
            Ok(draft) => Ok(draft
                .message
                .and_then(|held| held.id)
                .is_some_and(|held| held != *message)),
            Err(failure) if failure.is_not_found() => Ok(false),
            Err(failure) => Err(failure),
        }
    }

    /// The reply that calls off scheduled send `row`, if the send is `if no reply` and the thread
    /// as Gmail has it now has one. Such a reply is a message the thread didn't have when the
    /// send was scheduled, and isn't a draft or the user's (Gmail labels the user's own `SENT`).
    async fn aborting_reply(&self, row: &DraftRow) -> std::result::Result<Option<String>, Failure> {
        let (Some(later), Some(thread)) = (
            row.record
                .0
                .send_later
                .as_ref()
                .filter(|later| later.abort_on_reply && !later.current_message_ids.is_empty()),
            &row.thread_id,
        ) else {
            return Ok(None);
        };
        let fetched = match self
            .api
            .call(Lane::Act, 2, |gmail| {
                gmail.thread(thread, gmail::Format::Minimal, &[])
            })
            .await
        {
            Err(failure) if failure.is_not_found() => return Ok(None),
            fetched => fetched?,
        };
        Ok(fetched
            .messages
            .into_iter()
            .find(|message| {
                is_reply(
                    later,
                    message.id.as_deref().unwrap_or_default(),
                    &message.label_ids,
                )
            })
            .and_then(|message| message.id))
    }

    /// Calls off draft `id`'s scheduled send because a reply came before it (`if no reply`). It's
    /// a draft again with its time kept, which compose shows as `Unscheduled: {name} replied`.
    async fn unschedule(&self, id: &str) -> Result<()> {
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        else {
            return write.commit().await;
        };
        log::info!("a reply called off draft {id}'s scheduled send");
        let not_before = row.not_before.filter(|at| *at == UNTIL_COMPLETE);
        DraftRow::update_by_id(id)
            .send_at(None)
            .failures(0)
            .not_before(not_before)
            .exec(write.exec())
            .await?;
        if let Some(thread) = &row.thread_id {
            reindex(write.exec(), thread, &self.router()).await?;
        }
        write.publish().await
    }

    /// Calls off the `if no reply` sends scheduled in `threads` that a stored reply came before,
    /// once sync has brought it in.
    pub(crate) async fn unschedule_replied(&self, threads: &HashSet<String>) -> Result<()> {
        let mut db = self.db.clone();
        let f = DraftRow::fields();
        let rows = DraftRow::all()
            .select((
                f.id(),
                f.thread_id(),
                f.record(),
                f.discarded(),
                f.send_at(),
            ))
            .exec(&mut db)
            .await?;
        let scheduled = rows.iter().filter(|(_, thread_id, _, discarded, send_at)| {
            send_at.is_some()
                && !*discarded
                && thread_id
                    .as_ref()
                    .is_some_and(|thread| threads.contains(thread))
        });
        for (id, thread_id, record, _, _) in scheduled {
            let (Some(later), Some(thread)) =
                (
                    record.0.send_later.as_ref().filter(|later| {
                        later.abort_on_reply && !later.current_message_ids.is_empty()
                    }),
                    thread_id,
                )
            else {
                continue;
            };
            let stored = MessageRow::filter_by_thread_id(thread)
                .exec(&mut db)
                .await?;
            if stored
                .iter()
                .any(|message| is_reply(later, &message.id, &message.label_ids))
            {
                self.unschedule(id).await?;
            }
        }
        Ok(())
    }

    /// The row's Gmail draft: the one it made, or the one holding the message it took over
    /// (none if that's gone since).
    async fn gmail_draft_id(&self, row: &DraftRow) -> std::result::Result<Option<String>, Failure> {
        let (None, Some(message)) = (&row.gmail_id, &row.message_id) else {
            return Ok(row.gmail_id.clone());
        };
        let mut page_token = None;
        loop {
            let query = gmail::DraftQuery {
                max_results: Some(500),
                page_token: page_token.take(),
                ..gmail::DraftQuery::default()
            };
            let page = self
                .api
                .call(Lane::Act, 3, |gmail| gmail.drafts(&query))
                .await?;
            let found = page
                .drafts
                .into_iter()
                .find(|draft| draft.message.as_ref().and_then(|m| m.id.as_ref()) == Some(message));
            if let Some(draft) = found {
                return Ok(draft.id);
            }
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(None),
            }
        }
    }

    /// Creates or updates the Gmail draft with the row's message, and returns the row as Gmail
    /// now has it. A Gmail draft deleted elsewhere is created again.
    async fn write_gmail_draft(&self, row: DraftRow) -> Result<Result<DraftRow, Failure>> {
        let raw = row.raw.clone().into_bytes();
        let thread = row.thread_id.as_deref();
        let gmail_id = match self.gmail_draft_id(&row).await {
            Ok(gmail_id) => gmail_id,
            Err(failure) => return Ok(Err(failure)),
        };
        let progress = self.upload_progress(&row);
        // Updating the Gmail draft that holds the row's message replaces that message. A draft
        // made anew leaves it as it was (sent, say).
        let attempt = async {
            Ok::<_, crate::Error>(match &gmail_id {
                Some(gmail_id) => match self
                    .api
                    .call(Lane::Act, 1, |gmail| {
                        reporting(
                            gmail.update_draft(gmail_id, raw.clone(), thread),
                            progress.as_ref(),
                        )
                    })
                    .await
                {
                    Err(failure) if failure.is_not_found() => (
                        self.create_gmail_draft(&row, &raw, thread, progress.as_ref())
                            .await?,
                        None,
                    ),
                    written => (written, row.message_id.as_deref()),
                },
                None => (
                    self.create_gmail_draft(&row, &raw, thread, progress.as_ref())
                        .await?,
                    None,
                ),
            })
        }
        .await;
        if !matches!(attempt, Ok((Ok(_), _))) {
            self.upload_ended(&row.id);
        }
        let (written, replaced) = attempt?;
        let draft = match written {
            Ok(draft) => draft,
            Err(failure) => return Ok(Err(failure)),
        };
        self.set_gmail_files(&row.id, local_files(&row.record.0));
        let message = draft.message.unwrap_or_default();
        let thread_id = message.thread_id.or_else(|| row.thread_id.clone());
        // Recorded before the row says Gmail has its message, so that a save made meanwhile
        // which matches what Gmail had before isn't taken for one that Gmail has.
        if let Some(message) = &message.id {
            self.acked
                .lock()
                .expect("never poisoned")
                .insert(row.id.clone(), (message.clone(), content(&row.raw)));
        }
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        // Saves made while Gmail was being written keep their newer revision and `not_before`
        // (including `UNTIL_COMPLETE`), to push next.
        if let Some(current) = DraftRow::filter_by_id(&row.id)
            .first()
            .exec(write.exec())
            .await?
        {
            let mut update = DraftRow::update_by_id(&row.id)
                .gmail_id(draft.id.clone())
                .message_id(message.id.clone())
                .thread_id(thread_id.clone().or(current.thread_id))
                .pushed(row.revision);
            if current.revision == row.revision {
                update = update.failures(0).not_before(None);
            }
            update.exec(write.exec()).await?;
        }
        if let Some(id) = &message.id
            && let Some(stored) = MessageRow::filter_by_id(id)
                .first()
                .exec(write.exec())
                .await?
        {
            reindex(write.exec(), &stored.thread_id, &self.router()).await?;
        }
        write.commit().await?;
        match (&message.id, &thread_id, replaced) {
            (Some(message), Some(thread), Some(replaced)) => {
                // A listed file shows on the draft's row, and only the message's parts tell of
                // it. A scheduled send shows in its thread as a message.
                let full = scheduled(&row).is_some()
                    || row
                        .record
                        .0
                        .attachments
                        .iter()
                        .any(|attachment| attachment.content_id.is_none());
                self.refresh_message(
                    thread,
                    row.thread_id.as_deref(),
                    message,
                    Some(replaced),
                    full,
                )
                .await?;
            }
            // For a new Gmail draft, the thread's rows say what else is there now. A scheduled
            // draft's thread shows it as a message, body and all.
            (message, Some(thread), None) => {
                self.refresh_thread(thread).await?;
                if let Some(message) = message.as_deref().filter(|_| scheduled(&row).is_some()) {
                    self.refresh_message(thread, Some(thread), message, None, true)
                        .await?;
                }
            }
            _ => {}
        }
        // The pace counts from the write's end.
        self.written
            .lock()
            .expect("never poisoned")
            .insert(row.id.clone(), Timestamp::now());
        self.notify();
        Ok(Ok(DraftRow {
            gmail_id: draft.id,
            message_id: message.id,
            thread_id,
            pushed: row.revision,
            ..row
        }))
    }

    /// Creates `row`'s Gmail draft. After a failed attempt, which may have created it with only
    /// the answer lost, the Gmail draft with its Message-ID is updated instead, if there's one
    /// that no other draft of Exo's has (a reply made a new message keeps the reply's
    /// Message-ID).
    async fn create_gmail_draft(
        &self,
        row: &DraftRow,
        raw: &[u8],
        thread: Option<&str>,
        progress: Option<&gmail::Progress>,
    ) -> Result<std::result::Result<gmail::Draft, Failure>> {
        if row.failures > 0 && !row.record.0.rfc822_id.is_empty() {
            let query = gmail::DraftQuery {
                q: Some(format!("rfc822msgid:{}", row.record.0.rfc822_id)),
                ..gmail::DraftQuery::default()
            };
            let found = match self
                .api
                .call(Lane::Act, 2, |gmail| gmail.drafts(&query))
                .await
            {
                Ok(found) => found.drafts,
                Err(failure) => return Ok(Err(failure)),
            };
            let mut db = self.db.clone();
            let f = DraftRow::fields();
            let others: HashSet<String> = DraftRow::all()
                .select((f.id(), f.gmail_id()))
                .exec(&mut db)
                .await?
                .into_iter()
                .filter(|(id, _)| *id != row.id)
                .filter_map(|(_, gmail_id)| gmail_id)
                .collect();
            if let Some(id) = found
                .into_iter()
                .filter_map(|draft| draft.id)
                .find(|id| !others.contains(id))
            {
                return Ok(self
                    .api
                    .call(Lane::Act, 1, |gmail| {
                        reporting(gmail.update_draft(&id, raw.to_vec(), thread), progress)
                    })
                    .await);
            }
        }
        Ok(self
            .api
            .call(Lane::Act, 1, |gmail| {
                reporting(gmail.create_draft(raw.to_vec(), thread), progress)
            })
            .await)
    }

    /// Sends draft `id`'s Gmail draft (`drafts.send`) and drops the row, unless an undo, a
    /// discard or a save got to it after the push read it. To tell, the row is read again under
    /// the lock that undo send takes.
    async fn send_gmail_draft(
        &self,
        id: &str,
        now: Timestamp,
        report: &mut DraftReport,
    ) -> Result<Result<(), (u64, Failure)>> {
        let sending = self.sending.lock().await;
        let mut db = self.db.clone();
        let Some(row) = DraftRow::filter_by_id(id).first().exec(&mut db).await? else {
            return Ok(Ok(()));
        };
        // Taken back, held longer, discarded, or saved again (sent once Gmail has that version).
        if row.discarded || row.pushed < row.revision || !row.send_at.is_some_and(|at| at <= now) {
            return Ok(Ok(()));
        }
        let Some(gmail_id) = row.gmail_id.clone() else {
            return Ok(Ok(()));
        };
        // A reply that sync hasn't brought in yet (Exo was closed, say) still calls the send off.
        match self.aborting_reply(&row).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                drop(sending);
                self.unschedule(id).await?;
                return Ok(Ok(()));
            }
            Err(failure) => return Ok(Err((row.revision, failure))),
        }
        let mut result = self
            .api
            .call(Lane::Act, SEND_ATTEMPTS, |gmail| {
                gmail.send_draft(&gmail_id)
            })
            .await;
        if result.as_ref().is_err_and(|failure| {
            failure.is_not_found() || matches!(failure, Failure::Transient(_))
        }) {
            // The draft is gone, or a server error may have lost Gmail's answer. If Gmail sent it
            // (an attempt whose answer was lost, or Send pressed elsewhere), it went.
            match self.sent_message(&row.record.0.rfc822_id).await {
                Ok(Some(message)) => result = Ok(message),
                Ok(None) => {}
                Err(failure) => return Ok(Err((row.revision, failure))),
            }
        }
        let sent = match result {
            Ok(sent) => sent,
            Err(failure) => {
                let Some(reason) = verdict(&failure, true, now) else {
                    return Ok(Err((row.revision, failure)));
                };
                log::warn!(
                    "Gmail wouldn't send draft {}: {}",
                    row.id,
                    failure.message()
                );
                self.fail_send(&row.id, row.revision, reason, report)
                    .await?;
                return Ok(Ok(()));
            }
        };
        let thread = sent
            .thread_id
            .clone()
            .or(row.thread_id.clone())
            .unwrap_or_default();
        // A reminder being given meanwhile is given before the row goes, or finds the row gone.
        let giving = self.giving.lock().await;
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        DraftRow::filter_by_id(&row.id)
            .delete()
            .exec(write.exec())
            .await?;
        let mut given = crate::reminders::given(write.exec()).await?;
        let given_reminder = given.remove(&row.id).map(|entry| entry.reminder);
        if given_reminder.is_some() {
            db::kv_set(write.exec(), GIVEN_REMINDERS, &given).await?;
        }
        write.publish().await?;
        self.given.lock().expect("never poisoned").remove(&row.id);
        drop(giving);
        drop(sending);
        if let Some(message) = sent.id.as_deref().filter(|_| !thread.is_empty()) {
            // The thread shows the sent message, so it's fetched whole.
            self.refresh_message(
                &thread,
                row.thread_id.as_deref(),
                message,
                row.message_id.as_deref(),
                true,
            )
            .await?;
        }
        let record = row.record.0;
        // The send put the reminder on its thread already, unless the thread wasn't stored then.
        if let Some(reminder) = record
            .reminder
            .filter(|reminder| !thread.is_empty() && given_reminder != Some(*reminder))
        {
            let set_on = record
                .send_later
                .map(|later| later.current_message_ids)
                .filter(|ids| !ids.is_empty());
            match self.remind_sent(&thread, reminder, set_on).await {
                Ok(()) => report.reminded = true,
                Err(error) => {
                    log::error!("setting the reminder sent with draft {}: {error}", row.id)
                }
            }
        }
        report.sent.push(SentDraft {
            id: row.id,
            thread: ThreadId::new(thread),
            message: sent.id.unwrap_or_default(),
        });
        Ok(Ok(()))
    }

    /// Gives up on draft `id`'s send for `reason`. It becomes a failed draft, which its thread
    /// shows by Gmail's copy until it's edited, sent again or discarded. A draft whose save waits
    /// for sign-in shows once Gmail has that copy. A draft whose only save Gmail refused will
    /// have no copy to show, so it's only a draft again.
    async fn fail_send(
        &self,
        id: &str,
        expected_revision: u64,
        reason: SendFailure,
        report: &mut DraftReport,
    ) -> Result<()> {
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        else {
            return write.commit().await;
        };
        if row.revision != expected_revision || row.discarded || row.send_at.is_none() {
            return write.commit().await;
        }
        let kept = row.message_id.is_some() || row.pushed < row.revision;
        let not_before = row.not_before.filter(|at| *at == UNTIL_COMPLETE);
        DraftRow::update_by_id(id)
            .send_at(None)
            .failures(0)
            .not_before(not_before)
            .failure(kept.then(|| reason.code().to_owned()))
            .failed_at(kept.then(Timestamp::now))
            .exec(write.exec())
            .await?;
        if let Some(thread) = &row.thread_id {
            reindex(write.exec(), thread, &self.router()).await?;
        }
        write.publish().await?;
        // The thread shows it as a message now, so it needs the body its saves didn't store.
        if kept && let (Some(thread), Some(message)) = (&row.thread_id, &row.message_id) {
            self.refresh_message(thread, Some(thread), message, None, true)
                .await?;
        }
        report.failed.push(DraftFailure::Send {
            id: id.to_owned(),
            reason,
        });
        Ok(())
    }

    /// Gmail's sent message with Message-ID `rfc822_id`, if there is one.
    async fn sent_message(
        &self,
        rfc822_id: &str,
    ) -> std::result::Result<Option<gmail::Message>, Failure> {
        if rfc822_id.is_empty() {
            return Ok(None);
        }
        let query = gmail::ListQuery {
            q: Some(format!("rfc822msgid:{rfc822_id}")),
            label_ids: vec![SENT.to_owned()],
            ..gmail::ListQuery::default()
        };
        let page = self
            .api
            .call(Lane::Act, 2, |gmail| gmail.messages(&query))
            .await?;
        Ok(page.messages.into_iter().next())
    }

    /// Deletes a discarded draft's Gmail draft, then the row (unless it was saved again since).
    async fn delete_gmail_draft(&self, row: DraftRow) -> Result<Result<(), Failure>> {
        let gmail_id = match self.gmail_draft_id(&row).await {
            Ok(gmail_id) => gmail_id,
            Err(failure) => return Ok(Err(failure)),
        };
        if let Some(gmail_id) = &gmail_id {
            match self
                .api
                .call(Lane::Act, 1, |gmail| gmail.delete_draft(gmail_id))
                .await
            {
                Ok(()) => {}
                Err(failure) if failure.is_not_found() => {}
                Err(failure @ Failure::Permanent(_)) => log::warn!(
                    "Gmail wouldn't delete draft {}: {}",
                    row.id,
                    failure.message()
                ),
                Err(failure) => return Ok(Err(failure)),
            }
        }
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        match DraftRow::filter_by_id(&row.id)
            .first()
            .exec(write.exec())
            .await?
        {
            Some(current) if current.discarded => {
                DraftRow::filter_by_id(&row.id)
                    .delete()
                    .exec(write.exec())
                    .await?;
                // Its message goes with it, because the row no longer hides the message. Also, a
                // thread whose newest message was the draft can come back from Gmail with an
                // older history id, which leaves the stored copy alone until history says it's
                // gone.
                if let (Some(message), Some(thread)) = (&current.message_id, &row.thread_id) {
                    provider::delete_messages(write.exec(), std::slice::from_ref(message)).await?;
                    reindex(write.exec(), thread, &self.router()).await?;
                }
            }
            // If the discard was undone meanwhile, the Gmail draft is gone, so the next push
            // creates it again.
            Some(_) => {
                DraftRow::update_by_id(&row.id)
                    .gmail_id(None)
                    .message_id(None)
                    .pushed(0)
                    .exec(write.exec())
                    .await?;
            }
            None => {}
        }
        write.publish().await?;
        if let Some(thread) = &row.thread_id {
            self.refresh_thread(thread).await?;
        }
        Ok(Ok(()))
    }

    /// Drops the drafts whose Gmail draft was sent, deleted or rewritten elsewhere: those whose
    /// message is in `undrafted` (gone from Gmail's drafts, by its history), that Gmail had the
    /// latest version of, and that aren't held or discarded. Compose then opens what Gmail has,
    /// if anything, as a draft written elsewhere, instead of Exo's stale copy (which saving would
    /// write back, or Send send again). A push under way replaces messages itself, so while one
    /// is running, the drafts are looked at again once it's done.
    pub(crate) async fn forget_drafts_gone(&self, undrafted: &HashSet<String>) -> Result<()> {
        type GoneMeta = (
            String,
            Option<String>,
            Option<String>,
            bool,
            Option<Timestamp>,
            u64,
            u64,
        );
        let gone = |rows: Vec<GoneMeta>| -> Vec<(String, Option<String>)> {
            rows.into_iter()
                .filter(|(_, _, message_id, discarded, send_at, pushed, revision)| {
                    !*discarded
                        && send_at.is_none()
                        && pushed >= revision
                        && message_id
                            .as_ref()
                            .is_some_and(|message| undrafted.contains(message))
                })
                .map(|(id, thread_id, ..)| (id, thread_id))
                .collect()
        };
        async fn load_gone(exec: &mut dyn toasty::Executor) -> Result<Vec<GoneMeta>> {
            let f = DraftRow::fields();
            Ok(DraftRow::all()
                .select((
                    f.id(),
                    f.thread_id(),
                    f.message_id(),
                    f.discarded(),
                    f.send_at(),
                    f.pushed(),
                    f.revision(),
                ))
                .exec(exec)
                .await?)
        }
        // Gmail no longer holds those messages, so a save matching one has changes to write.
        self.acked
            .lock()
            .expect("never poisoned")
            .retain(|_, (message, _)| !undrafted.contains(message));
        let mut db = self.db.clone();
        if undrafted.is_empty() || gone(load_gone(&mut db).await?).is_empty() {
            return Ok(());
        }
        let _flush = self.draft_flush.lock().await;
        let mut write = self.begin(&mut db).await?;
        let mut threads = Vec::new();
        for (id, thread_id) in gone(load_gone(write.exec()).await?) {
            log::info!("draft {id} was sent, deleted or rewritten outside Exo");
            DraftRow::filter_by_id(&id)
                .delete()
                .exec(write.exec())
                .await?;
            // Its text leaves the thread's search index with it.
            if let Some(thread) = thread_id {
                threads.push(thread);
            }
        }
        reindex_many(write.exec(), &threads, &self.router()).await?;
        write.publish().await
    }

    /// Replaces the local copy of `thread` with Gmail's rows (stored messages aren't fetched in
    /// full again), so lists show a draft's change without waiting for sync. A failure here is
    /// left to the next sync.
    async fn refresh_thread(&self, thread: &str) -> Result<()> {
        let mut db = self.db.clone();
        let stored: HashSet<String> = MessageRow::filter_by_thread_id(thread)
            .select(MessageRow::fields().id())
            .exec(&mut db)
            .await?
            .into_iter()
            .collect();
        let fetched = match fetch_rows(&self.api, Lane::Act, 2, thread, &stored).await {
            Ok(fetched) => fetched,
            Err(failure) => {
                log::debug!(
                    "refreshing thread {thread} after a draft change: {}",
                    failure.message()
                );
                return Ok(());
            }
        };
        let mut write = self.begin(&mut db).await?;
        match fetched {
            Some(fetched) => {
                provider::save_rows(write.exec(), &fetched).await?;
            }
            None => provider::delete_thread(write.exec(), thread).await?,
        }
        reindex(write.exec(), thread, &self.router()).await?;
        write.publish().await
    }

    /// Stores `message`, which Gmail just wrote into `thread` (asked to keep it in `requested`)
    /// in place of `replaced`, the draft version it replaced, so lists show it without waiting
    /// for sync. It's fetched whole when `full` is set (a sent message, which the thread shows,
    /// or a draft with files). Otherwise only its row is fetched, since compose shows Exo's
    /// drafts from their record. `replaced` goes at once, since compose would take up a draft
    /// message that isn't Exo's latest as one written elsewhere. A failure to fetch is left to
    /// the next sync.
    async fn refresh_message(
        &self,
        thread: &str,
        requested: Option<&str>,
        message: &str,
        replaced: Option<&str>,
        full: bool,
    ) -> Result<()> {
        let mut db = self.db.clone();
        // A thread that Gmail started for the message holds only that message, but the thread
        // asked for may hold more. If that thread isn't stored, its rows bring the rest.
        if requested == Some(thread)
            && ThreadRow::filter_by_id(thread)
                .first()
                .exec(&mut db)
                .await?
                .is_none()
        {
            return self.refresh_thread(thread).await;
        }
        let fetched = if full {
            fetch_message(&self.api, Lane::Act, 2, message).await
        } else {
            fetch_row(&self.api, Lane::Act, 2, message).await
        };
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(failure) => {
                log::debug!(
                    "fetching message {message} after a draft change: {}",
                    failure.message()
                );
                return Ok(());
            }
        };
        let mut write = self.begin(&mut db).await?;
        let mut changed = vec![thread.to_owned()];
        if let Some(replaced) = replaced.filter(|replaced| *replaced != message) {
            if let Some(old) = MessageRow::filter_by_id(replaced)
                .first()
                .exec(write.exec())
                .await?
                && old.thread_id != thread
            {
                changed.push(old.thread_id);
            }
            provider::delete_messages(write.exec(), &[replaced.to_owned()]).await?;
        }
        if let Some(parsed) = fetched.as_ref().and_then(|fetched| parse(fetched, full)) {
            provider::save_message(write.exec(), parsed).await?;
        }
        for thread in &changed {
            if MessageRow::filter_by_thread_id(thread)
                .first()
                .exec(write.exec())
                .await?
                .is_none()
            {
                provider::delete_thread(write.exec(), thread).await?;
            } else {
                reindex(write.exec(), thread, &self.router()).await?;
            }
        }
        write.publish().await
    }
}

/// Whether message `id`, labelled `labels`, is a reply that calls off scheduled send `later`.
/// Such a reply is a message the thread didn't have when the send was scheduled, and isn't a
/// draft or the user's.
fn is_reply(later: &SendLater, id: &str, labels: &[String]) -> bool {
    !later.current_message_ids.iter().any(|known| known == id)
        && !labels.iter().any(|label| label == DRAFT || label == SENT)
}

/// What a Gmail failure means for a send, `due` or still held. Returns `None` to try again
/// later, or why the send fails. While the send is held, only Gmail refusing its message fails
/// it. Being offline never fails it, and the send goes once Gmail is back.
fn verdict(failure: &Failure, due: bool, now: Timestamp) -> Option<SendFailure> {
    match failure {
        Failure::Permanent(error) => Some(send_failure(error)),
        _ if !due => None,
        Failure::Offline(_) => None,
        Failure::Unauthorized(_) => Some(SendFailure::LoggedOut),
        Failure::Transient(_) => Some(SendFailure::Temporary),
        Failure::Throttled(until) | Failure::SendingLimited(until) => {
            let wait = now.duration_until(*until);
            if wait >= SENDING_LIMIT {
                Some(SendFailure::QuotaExceeded)
            } else if wait > SEND_WAITS {
                Some(SendFailure::RateLimited)
            } else {
                None
            }
        }
    }
}

/// The send failure Gmail's refusal stands for.
fn send_failure(error: &gmail::Error) -> SendFailure {
    // Google's upload front end may answer an oversized message with a page, not its JSON.
    if error.status() == Some(413) {
        return SendFailure::TooLarge;
    }
    let gmail::Error::Api(api) = error else {
        return SendFailure::Other;
    };
    let reason = |wanted: &str| api.errors.iter().any(|e| e.reason == wanted);
    if reason("dailyLimitExceeded") || reason("quotaExceeded") {
        SendFailure::QuotaExceeded
    } else if api.message.to_lowercase().contains("too large") {
        SendFailure::TooLarge
    } else if api.message.contains("Invalid To header")
        || api.message.contains("Invalid Cc header")
        || api.message.contains("Invalid Bcc header")
        || api.message.contains("Recipient address required")
    {
        SendFailure::InvalidRecipients
    } else {
        SendFailure::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafts::{Draft, DraftAction};

    fn sample_record() -> DraftRecord {
        DraftRecord {
            action: DraftAction::Compose,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Scheduled".into(),
            body: "<div>hello</div>".into(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "d-race@exo".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: Some(SendLater {
                at: Timestamp::now() - SignedDuration::from_hours(1),
                abort_on_reply: false,
                current_message_ids: Vec::new(),
            }),
            reminder: None,
        }
    }

    #[tokio::test]
    async fn redate_bails_on_concurrent_revision_and_unschedule_and_fail_send_preserve_until_complete()
     {
        let dir = std::env::temp_dir().join(format!(
            "exo-store-draft-sync-guards-{}",
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
        let now = Timestamp::now();
        let old_raw = "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\nSubject: Scheduled\r\n\r\nold body";
        store
            .save_draft(Draft {
                id: "draft_race".into(),
                thread: None,
                adopted: None,
                record: sample_record(),
                raw: old_raw.into(),
                complete: true,
                sending: false,
            })
            .await
            .expect("saves v1");
        store
            .send_draft("draft_race", now - SignedDuration::from_mins(5))
            .await
            .expect("holds");

        let mut db = store.inner.db.clone();
        let mut stale = DraftRow::filter_by_id("draft_race")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("row");

        // Concurrent `save_draft` updates raw, bumps revision, and marks draft incomplete.
        let new_raw =
            "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\nSubject: Scheduled\r\n\r\nnewer body";
        store
            .save_draft(Draft {
                id: "draft_race".into(),
                thread: None,
                adopted: None,
                record: sample_record(),
                raw: new_raw.into(),
                complete: false,
                sending: false,
            })
            .await
            .expect("saves v2");

        // `redate` with the stale row must bail and leave v2's `raw`, `revision`, and
        // `UNTIL_COMPLETE` intact.
        assert!(
            !store.inner.redate(&mut stale, now).await.expect("redate"),
            "redate skips when current.revision != row.revision"
        );
        let current = DraftRow::filter_by_id("draft_race")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("row");
        assert_eq!(current.raw, new_raw);
        assert_eq!(current.revision, 2);
        assert_eq!(current.not_before, Some(UNTIL_COMPLETE));

        // `fail_send` with stale revision is a no-op; with current revision preserves
        // `UNTIL_COMPLETE`.
        let mut report = DraftReport::default();
        store
            .inner
            .fail_send("draft_race", 1, SendFailure::Other, &mut report)
            .await
            .expect("stale fail_send");
        let after_stale = DraftRow::filter_by_id("draft_race")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("row");
        assert!(after_stale.send_at.is_some(), "stale fail_send ignored");
        assert!(report.failed.is_empty());

        // `unschedule` preserves `UNTIL_COMPLETE`.
        store
            .inner
            .unschedule("draft_race")
            .await
            .expect("unschedule");
        let after_unschedule = DraftRow::filter_by_id("draft_race")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("row");
        assert!(after_unschedule.send_at.is_none());
        assert_eq!(
            after_unschedule.not_before,
            Some(UNTIL_COMPLETE),
            "unschedule preserves UNTIL_COMPLETE"
        );

        // Re-hold and fail with current revision: `UNTIL_COMPLETE` must still be preserved.
        store
            .send_draft("draft_race", now - SignedDuration::from_mins(5))
            .await
            .expect("re-holds");
        store
            .inner
            .fail_send("draft_race", 2, SendFailure::TooLarge, &mut report)
            .await
            .expect("fail_send");
        let after_fail = DraftRow::filter_by_id("draft_race")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("row");
        assert_eq!(
            after_fail.not_before,
            Some(UNTIL_COMPLETE),
            "fail_send preserves UNTIL_COMPLETE"
        );
        assert_eq!(after_fail.failure.as_deref(), Some("message_too_large"));

        // Exercise metadata-only draft scans (`unsent`, `uploading`, `push_drafts`,
        // `unschedule_replied`, `forget_drafts_gone`) at runtime without loading `raw`.
        let unsent = store.unsent().await.expect("unsent");
        assert_eq!(unsent.changes, 1);
        let uploading = store.uploading().await.expect("uploading");
        assert_eq!(
            uploading.changes, 0,
            "failed draft is excluded from uploading"
        );
        // Incomplete draft (UNTIL_COMPLETE) is skipped cleanly by `push_drafts`.
        let report = store.push_drafts().await.expect("push_drafts");
        assert!(report.saved.is_empty() && report.sent.is_empty());
        let empty = HashSet::new();
        store
            .inner
            .unschedule_replied(&empty)
            .await
            .expect("unschedule_replied");
        let gone = HashSet::from(["nonexistent_msg".to_owned()]);
        store
            .inner
            .forget_drafts_gone(&gone)
            .await
            .expect("forget_drafts_gone");

        drop(db);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
