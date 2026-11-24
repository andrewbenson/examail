//! Drafts: Exo's record of each draft, kept locally and mirrored to a Gmail draft, and the held
//! sends behind undo send.
//!
//! Compose saves with [`Store::save_draft`], a local write the UI sees at once.
//! [`Store::push_drafts`] then brings Gmail up to date: it creates or updates the Gmail draft,
//! deletes discarded ones, and sends (`drafts.send`) those whose hold is over. A draft being
//! typed in is written at most every 5 s (30 s with files), which coalesces its saves. Its latest
//! save goes once the pace allows. A save that Gmail already has, with its content unchanged,
//! isn't written at all. Like the outbox, a push that fails for a reason that retrying can fix
//! backs off and tries again. Held sends survive a restart and go out once due. A send Gmail
//! won't take is a failed draft until it's edited, sent again or discarded.

mod files;
mod marks;
mod model;
mod sync;

use jiff::Timestamp;

pub(crate) use marks::{EditableDraft, Marks, editable, marks};
pub(crate) use model::UNTIL_COMPLETE;
pub use model::{
    Address, AttachmentSource, Draft, DraftAction, DraftAttachment, DraftFailure, DraftRecord,
    DraftReminder, DraftReport, SendFailure, SendLater, SentDraft, StoredDraft, Upload, Uploads,
};
pub(crate) use sync::content;

use self::files::local_files;
use crate::db::{DraftRow, MessageRow};
use crate::error::Result;
use crate::store::{Store, reindex};

impl Store {
    /// Saves `draft` locally, to be written to Gmail by the next
    /// [`push_drafts`](Self::push_drafts) unless Gmail has its content already. Saving a
    /// discarded draft brings it back (undo discard), saving a held one keeps its hold, and a
    /// failed one is a draft again.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_draft(&self, draft: Draft) -> Result<()> {
        let Draft {
            id,
            thread,
            adopted,
            record,
            raw,
            complete,
            sending,
        } = draft;
        let not_before = (!complete).then_some(UNTIL_COMPLETE);
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let thread = thread.map(|thread| thread.as_str().to_owned());
        let files = local_files(&record);
        let thread = match DraftRow::filter_by_id(&id)
            .first()
            .exec(write.exec())
            .await?
        {
            Some(row) => {
                // When Gmail has this content already, the save is Gmail's too, with nothing to
                // write. A write of an older revision that's under way marks only that revision
                // as Gmail's when it ends, so this one is written after it.
                let unchanged =
                    complete && !sending && self.inner.gmail_has(&row) == Some(content(&raw));
                self.inner.note_draft(&row);
                if unchanged {
                    self.inner.set_gmail_files(&id, files);
                }
                let revision = row.revision + 1;
                let thread = thread.or(row.thread_id);
                DraftRow::update_by_id(&id)
                    .thread_id(thread.clone())
                    .record(record)
                    .raw(raw)
                    .revision(revision)
                    .pushed(if unchanged { revision } else { row.pushed })
                    .discarded(false)
                    .failures(0)
                    .not_before(not_before)
                    .failure(None)
                    .failed_at(None)
                    .exec(write.exec())
                    .await?;
                thread
            }
            None => {
                // Gmail has none of a new draft's files, and a draft Gmail refuses keeps them
                // waiting.
                self.inner.uploads.send_modify(|uploads| {
                    uploads.insert(id.clone(), Upload::default());
                });
                // A draft taken over holds the message it took over until its first push, which
                // keeps compose from taking that message over a second time.
                DraftRow::create()
                    .id(&id)
                    .thread_id(thread.clone())
                    .gmail_id(None)
                    .message_id(adopted)
                    .record(record)
                    .raw(raw)
                    .revision(1)
                    .pushed(0)
                    .discarded(false)
                    .send_at(None)
                    .failures(0)
                    .not_before(not_before)
                    .failure(None)
                    .failed_at(None)
                    .exec(write.exec())
                    .await?;
                thread
            }
        };
        // The thread's search index has the draft's text, from its record. A failed draft that
        // becomes a draft again also changes the thread's lists and row.
        if let Some(thread) = &thread {
            reindex(write.exec(), thread, &self.inner.router()).await?;
        }
        write.publish().await
    }

    /// The draft `id`, unless discarded.
    ///
    /// # Errors
    /// On database failures.
    pub async fn draft(&self, id: &str) -> Result<Option<StoredDraft>> {
        let mut db = self.inner.db.clone();
        let row = DraftRow::filter_by_id(id).first().exec(&mut db).await?;
        Ok(row
            .filter(|row| !row.discarded)
            .inspect(|row| self.inner.note_draft(row))
            .map(StoredDraft::from))
    }

    /// Every draft compose has, discarded ones aside.
    ///
    /// # Errors
    /// On database failures.
    pub async fn drafts(&self) -> Result<Vec<StoredDraft>> {
        let mut db = self.inner.db.clone();
        let rows = DraftRow::all().exec(&mut db).await?;
        Ok(rows
            .into_iter()
            .filter(|row| !row.discarded)
            .inspect(|row| self.inner.note_draft(row))
            .map(StoredDraft::from)
            .collect())
    }

    /// Discards draft `id`. It's gone from reads at once, and its Gmail draft is deleted on the
    /// next push. [`save_draft`](Self::save_draft) brings it back.
    ///
    /// # Errors
    /// On database failures.
    pub async fn discard_draft(&self, id: &str) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        else {
            return write.commit().await;
        };
        DraftRow::update_by_id(id)
            .discarded(true)
            .send_at(None)
            .failures(0)
            .not_before(None)
            .failure(None)
            .failed_at(None)
            .exec(write.exec())
            .await?;
        if let Some(thread) = &row.thread_id {
            reindex(write.exec(), thread, &self.inner.router()).await?;
        }
        write.publish().await
    }

    /// Holds draft `id` for sending at `at`, when a later push sends it. A held draft's hold
    /// moves to `at`, and a failed one is sent again. A draft due now is tried on the next push,
    /// even while its saves back off (Instant Send, quitting, Google signing the account out).
    ///
    /// # Errors
    /// On database failures.
    pub async fn send_draft(&self, id: &str, at: Timestamp) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        if let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        {
            let not_before = if at <= Timestamp::now() {
                row.not_before.filter(|at| *at == UNTIL_COMPLETE)
            } else {
                row.not_before
            };
            DraftRow::update_by_id(id)
                .send_at(Some(at))
                .not_before(not_before)
                .failure(None)
                .failed_at(None)
                .exec(write.exec())
                .await?;
            if let Some(thread) = &row.thread_id {
                reindex(write.exec(), thread, &self.inner.router()).await?;
            }
        }
        write.publish().await?;
        self.inner.give_reminder(id).await?;
        Ok(())
    }

    /// Schedules draft `id` for its Send later time. It's held until then, as a Gmail draft
    /// meanwhile, and the push sends it once due (at the next launch if Exo isn't running then).
    /// The thread's messages are recorded, so a reply that calls off an `if no reply` send can be
    /// told apart. A draft without a time isn't touched.
    ///
    /// # Errors
    /// On database failures.
    pub async fn schedule_draft(&self, id: &str) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        else {
            return write.commit().await;
        };
        let mut record = row.record.0;
        let Some(later) = record.send_later.as_mut() else {
            return write.commit().await;
        };
        later.current_message_ids = match &row.thread_id {
            Some(thread) => {
                MessageRow::filter_by_thread_id(thread)
                    .select(MessageRow::fields().id())
                    .exec(write.exec())
                    .await?
            }
            None => Vec::new(),
        };
        let at = later.at;
        let not_before = if at <= Timestamp::now() {
            row.not_before.filter(|at| *at == UNTIL_COMPLETE)
        } else {
            row.not_before
        };
        DraftRow::update_by_id(id)
            .record(record)
            .send_at(Some(at))
            .not_before(not_before)
            .failure(None)
            .failed_at(None)
            .exec(write.exec())
            .await?;
        if let Some(thread) = &row.thread_id {
            reindex(write.exec(), thread, &self.inner.router()).await?;
        }
        write.publish().await?;
        self.inner.give_reminder(id).await?;
        Ok(())
    }

    /// Makes failed draft `id` a draft again, to edit (`Edit Message`). Its first edit is
    /// written at once, not at the pace of the write its send just made.
    ///
    /// # Errors
    /// On database failures.
    pub async fn recover_draft(&self, id: &str) -> Result<()> {
        self.inner
            .written
            .lock()
            .expect("never poisoned")
            .remove(id);
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        if let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
            .filter(|row| row.failure.is_some())
        {
            DraftRow::update_by_id(id)
                .failure(None)
                .failed_at(None)
                .exec(write.exec())
                .await?;
            if let Some(thread) = &row.thread_id {
                reindex(write.exec(), thread, &self.inner.router()).await?;
            }
        }
        write.publish().await
    }

    /// Gives held draft `id` `reminder` (or takes it off), and its thread with it. This is Remind
    /// me picked for a message just sent (`Set reminder` in the follow-up hint, and its Undo).
    /// Returns whether it was still unsent, so `false` means it already went (or isn't there).
    ///
    /// # Errors
    /// On database failures.
    pub async fn remind_draft(&self, id: &str, reminder: Option<DraftReminder>) -> Result<bool> {
        // A send in flight finishes first, as for undo send.
        let sending = self.inner.sending.lock().await;
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let Some(row) = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
            .filter(|row| row.send_at.is_some() && !row.discarded)
        else {
            write.commit().await?;
            return Ok(false);
        };
        let mut record = row.record.0;
        record.reminder = reminder;
        DraftRow::update_by_id(id)
            .record(record)
            .exec(write.exec())
            .await?;
        write.publish().await?;
        drop(sending);
        if reminder.is_some() {
            self.inner.give_reminder(id).await?;
        } else {
            self.inner.take_back_reminder(id).await?;
        }
        Ok(true)
    }

    /// Undo send, which takes back a held send ([`cancel_send`](Self::cancel_send)) and the
    /// reminder it put on its thread. The thread gets back the reminder it had. Returns whether
    /// the send was still unsent.
    ///
    /// # Errors
    /// On database failures.
    pub async fn undo_send(&self, id: &str) -> Result<bool> {
        let held = self.cancel_send(id).await?;
        if held {
            self.inner.take_back_reminder(id).await?;
        }
        Ok(held)
    }

    /// Takes back a held send, or one that failed meanwhile, leaving the reminder it put on its
    /// thread (`Edit Message` on a scheduled send). [`undo_send`](Self::undo_send) takes that
    /// reminder back too. Returns whether it was still unsent, so `false` means it already went
    /// (or isn't there).
    ///
    /// # Errors
    /// On database failures.
    pub async fn cancel_send(&self, id: &str) -> Result<bool> {
        // A send in flight finishes first, after which it either went (the row is gone) or
        // didn't.
        let _sending = self.inner.sending.lock().await;
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let row = DraftRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?;
        let held = row
            .as_ref()
            .is_some_and(|row| (row.send_at.is_some() || row.failure.is_some()) && !row.discarded);
        if held {
            DraftRow::update_by_id(id)
                .send_at(None)
                .failure(None)
                .failed_at(None)
                .exec(write.exec())
                .await?;
            if let Some(thread) = row.and_then(|row| row.thread_id) {
                reindex(write.exec(), &thread, &self.inner.router()).await?;
            }
        }
        write.publish().await?;
        Ok(held)
    }
}
