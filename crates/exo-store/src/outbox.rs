//! The outbox, which persists the user's changes as per-thread FIFO queues of Gmail calls.
//!
//! [`Store::apply`] writes a row and the UI sees its effect at once, because every read
//! applies the pending steps on top of Gmail's copy. [`Store::flush`] sends rows one step at a
//! time, folding each acknowledged step into Gmail's copy and dropping it from the row.
//! Failures back off. A row that can't succeed is dropped, which rolls its overlay back.

use std::collections::HashSet;

use futures::{StreamExt, stream};
use gmail::{BatchModifyMessagesRequest, ModifyMessageRequest, ModifyThreadRequest};
use jiff::Timestamp;
use toasty::Executor;

use crate::api::Failure;
use crate::db::{DraftRow, MessageRow, OutboxRow, ReminderRow};
use crate::error::{Error, Result};
use crate::ids::ThreadId;
use crate::mutation::{Mutation, Plan, Step, resolve};
use crate::provider;
use crate::queue::{self, Disposition};
use crate::quota::Lane;
use crate::store::{Inner, Store, load_view, reindex};
use crate::sync::{Rows, fetch_rows};
use crate::view::{Identity, ThreadView};

/// Counted failures after which a mutation is given up on.
const MAX_ATTEMPTS: u32 = 300;
/// Queues persisted at once.
const CONCURRENCY: usize = 4;

/// What [`Store::unsent`] counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Unsent {
    /// Messages being sent, whether held for undo or due and not yet accepted.
    pub sends: usize,
    /// Everything else not yet acknowledged.
    pub changes: usize,
}

/// Undoes one [`Store::apply`]. Pass it to [`Store::undo`], which consumes it so that nothing
/// is undone twice.
#[derive(Debug)]
pub struct Undo {
    entries: Vec<UndoEntry>,
}

#[derive(Debug)]
struct UndoEntry {
    thread: String,
    /// The outbox row the mutation went into, if it had anything to send.
    op: Option<u64>,
    mutation: Mutation,
    inverse: Plan,
    /// The reminder the thread had, which undoing a reminder change puts back.
    reminder: Option<Option<ReminderRow>>,
}

impl Undo {
    /// Nothing changed, because every thread was already in the target state ("Already marked
    /// as Done.").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The threads the mutation changed.
    pub fn threads(&self) -> impl Iterator<Item = ThreadId> + '_ {
        self.entries
            .iter()
            .map(|entry| ThreadId::new(entry.thread.clone()))
    }

    /// The reminder replaced on the first thread, if this undo came from a reminder mutation.
    pub(crate) fn prior_reminder(&self) -> Option<&ReminderRow> {
        self.entries
            .first()
            .and_then(|entry| entry.reminder.as_ref())
            .and_then(Option::as_ref)
    }
}

/// What a [`Store::flush`] did.
#[derive(Debug, Default)]
pub struct FlushReport {
    /// Mutations Gmail fully acknowledged.
    pub completed: usize,
    /// Mutations given up on, whose optimistic changes are gone. Show the error.
    pub failed: Vec<FailedMutation>,
    /// When the earliest postponed mutation is due again. Flush then.
    pub retry_at: Option<Timestamp>,
}

/// A mutation the outbox gave up on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedMutation {
    /// The thread it was for.
    pub thread: ThreadId,
    /// What the user did.
    pub mutation: Mutation,
    /// It was the undo of `mutation` that failed.
    pub undo: bool,
    /// Gmail's error.
    pub error: String,
}

enum Outcome {
    Done,
    Postponed,
    Failed(FailedMutation),
    /// Undone before it started.
    Gone,
}

/// What an acknowledged step changes in Gmail's copy.
enum Effect {
    /// The step's own label change.
    Labels,
    /// The thread as Gmail has it now.
    Replace(Rows),
    /// Gmail no longer has the thread.
    Deleted,
}

impl Store {
    /// Applies `mutation` to `threads`, queued for Gmail and visible to reads immediately.
    /// Threads already in the target state are left alone. Call [`flush`](Self::flush) next.
    ///
    /// # Errors
    /// [`Error::UnknownThread`] for a thread that isn't stored (nothing is applied then), and
    /// [`Error::NotSynced`] for mute changes before the first sync created the mute label.
    pub async fn apply(&self, threads: &[ThreadId], mutation: Mutation) -> Result<Undo> {
        self.inner.apply(threads, mutation).await
    }

    /// Undoes an [`apply`](Self::apply). A mutation that hasn't been sent to Gmail yet is
    /// dropped, and one given up on has nothing to undo. Otherwise its inverse is queued behind
    /// it.
    ///
    /// # Errors
    /// On database failures.
    pub async fn undo(&self, undo: Undo) -> Result<()> {
        self.inner.undo(undo).await
    }

    /// Sends queued mutations to Gmail, taking the head of every thread's queue, up to four
    /// queues at once, until every queue is empty or waiting out a backoff. It first waits for
    /// a flush that's already running.
    ///
    /// # Errors
    /// On database failures. Gmail failures are retried later or reported in the result.
    pub async fn flush(&self) -> Result<FlushReport> {
        let inner = &self.inner;
        let _flush = inner.flush.lock().await;
        let mut report = FlushReport::default();
        loop {
            let mut db = inner.db.clone();
            let rows = OutboxRow::all()
                .order_by(OutboxRow::fields().id().asc())
                .exec(&mut db)
                .await?;
            let batch = queue::select_due(
                rows,
                Timestamp::now(),
                false,
                |row| row.queue.clone(),
                |row| row.not_before,
            );
            report.retry_at = batch.retry_at;
            if batch.due.is_empty() {
                return Ok(report);
            }
            let outcomes: Vec<Result<Outcome>> =
                stream::iter(batch.due.into_iter().map(|row| row.id))
                    .map(|id| inner.persist(id))
                    .buffer_unordered(CONCURRENCY)
                    .collect()
                    .await;
            for outcome in outcomes {
                match outcome? {
                    Outcome::Done => report.completed += 1,
                    Outcome::Failed(failure) => report.failed.push(failure),
                    Outcome::Postponed | Outcome::Gone => {}
                }
            }
        }
    }

    /// Mutations not yet acknowledged by Gmail.
    ///
    /// # Errors
    /// On database failures.
    pub async fn pending(&self) -> Result<usize> {
        let mut db = self.inner.db.clone();
        Ok(usize::try_from(OutboxRow::all().count().exec(&mut db).await?).unwrap_or(usize::MAX))
    }

    /// What Gmail doesn't have yet, as signing out counts it: sends still held or going out, and
    /// every other change (queued mutations, draft saves and discards, the calendar's queued
    /// changes).
    ///
    /// # Errors
    /// On database failures.
    pub async fn unsent(&self) -> Result<Unsent> {
        let mut db = self.inner.db.clone();
        let mut unsent = Unsent {
            sends: 0,
            changes: self.pending().await? + self.calendar_pending().await?,
        };
        let f = DraftRow::fields();
        for (discarded, send_at, pushed, revision) in DraftRow::all()
            .select((f.discarded(), f.send_at(), f.pushed(), f.revision()))
            .exec(&mut db)
            .await?
        {
            if send_at.is_some() && !discarded {
                unsent.sends += 1;
            } else if discarded || pushed < revision {
                unsent.changes += 1;
            }
        }
        Ok(unsent)
    }
}

impl Store {
    /// What the footer counts as on its way to Google: sends held for undo or due, and every
    /// change Gmail and Calendar can take now (queued mutations, the calendar's queued changes,
    /// draft saves and discards). It leaves out a send scheduled for later and a draft waiting
    /// for its files.
    ///
    /// # Errors
    /// On database failures.
    pub async fn uploading(&self) -> Result<Unsent> {
        let mut db = self.inner.db.clone();
        let mut uploading = Unsent {
            sends: 0,
            changes: self.pending().await? + self.calendar_pending().await?,
        };
        let f = DraftRow::fields();
        for (discarded, send_at, pushed, revision, not_before, failure, record) in DraftRow::all()
            .select((
                f.discarded(),
                f.send_at(),
                f.pushed(),
                f.revision(),
                f.not_before(),
                f.failure(),
                f.record(),
            ))
            .exec(&mut db)
            .await?
        {
            let is_scheduled = send_at.is_some()
                && !discarded
                && failure.is_none()
                && record.0.send_later.is_some();
            if failure.is_some() || is_scheduled {
                continue;
            }
            if send_at.is_some() && !discarded {
                uploading.sends += 1;
            } else if discarded
                || (pushed < revision && not_before != Some(crate::drafts::UNTIL_COMPLETE))
            {
                uploading.changes += 1;
            }
        }
        Ok(uploading)
    }
}

/// Resolves `plan` against `view` and queues the steps on the thread's queue, as the undo of
/// `mutation` when `undo` is set. A plan with a replace key takes the place of the queue's
/// unstarted tail when the tail has the same key, so it's resolved as if the tail weren't there.
/// Returns the row, or `None` when there's nothing to send.
pub(crate) async fn enqueue(
    exec: &mut dyn Executor,
    view: &ThreadView,
    plan: &Plan,
    mutation: &Mutation,
    undo: bool,
    me: &Identity,
) -> Result<Option<u64>> {
    let mut without_tail = None;
    if plan.replace_key.is_some() {
        let tail = OutboxRow::filter_by_queue(&view.id)
            .order_by(OutboxRow::fields().id().desc())
            .first()
            .exec(exec)
            .await?;
        if let Some(tail) =
            tail.filter(|tail| tail.started_at.is_none() && tail.replace_key == plan.replace_key)
        {
            tail.delete().exec(exec).await?;
            without_tail = load_view(exec, &view.id).await?;
        }
    }
    let steps = resolve(&plan.intents, without_tail.as_ref().unwrap_or(view), me);
    if steps.iter().all(|step| *step == Step::Refresh) {
        return Ok(None);
    }
    let row = OutboxRow::create()
        .queue(&view.id)
        .replace_key(plan.replace_key.clone())
        .mutation(mutation.clone())
        .undo(undo)
        .steps(steps)
        .attempts(0)
        .failures(0)
        .exec(exec)
        .await?;
    Ok(Some(row.id))
}

impl Inner {
    /// [`Store::apply`].
    pub(crate) async fn apply(&self, threads: &[ThreadId], mutation: Mutation) -> Result<Undo> {
        let router = self.router();
        let me = &router.me;
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let mut entries = Vec::new();
        for thread in threads {
            let view = load_view(write.exec(), thread.as_str())
                .await?
                .ok_or_else(|| Error::UnknownThread(thread.clone()))?;
            let Some((forward, inverse)) = mutation.plan(&view, me)? else {
                continue;
            };
            let reminder =
                crate::reminders::apply(write.exec(), &view, &mutation, Timestamp::now()).await?;
            let op = enqueue(write.exec(), &view, &forward, &mutation, false, me).await?;
            if op.is_none() && reminder.is_none() {
                continue;
            }
            reindex(write.exec(), &view.id, &router).await?;
            entries.push(UndoEntry {
                thread: view.id,
                op,
                mutation: mutation.clone(),
                inverse,
                reminder,
            });
        }
        write.publish().await?;
        Ok(Undo { entries })
    }

    /// [`Store::undo`].
    pub(crate) async fn undo(&self, undo: Undo) -> Result<()> {
        let router = self.router();
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        for entry in undo.entries {
            let op = match entry.op {
                Some(op) => {
                    OutboxRow::filter_by_id(op)
                        .first()
                        .exec(write.exec())
                        .await?
                }
                None => None,
            };
            let given_up = entry.op.is_some_and(|op| {
                self.given_up
                    .lock()
                    .expect("given-up lock is never poisoned")
                    .contains(&op)
            });
            match op {
                Some(op) if op.started_at.is_none() => op.delete().exec(write.exec()).await?,
                // Nothing reached Gmail, so its overlay is gone already.
                _ if entry.op.is_none() || given_up => {}
                _ => {
                    let UndoEntry {
                        thread,
                        mutation,
                        inverse,
                        ..
                    } = &entry;
                    if let Some(view) = load_view(write.exec(), thread).await? {
                        enqueue(write.exec(), &view, inverse, mutation, true, &router.me).await?;
                    }
                }
            }
            if let Some(previous) = entry.reminder {
                crate::reminders::restore(write.exec(), &entry.thread, previous).await?;
            }
            reindex(write.exec(), &entry.thread, &router).await?;
        }
        write.publish().await
    }

    /// Sends one row's remaining steps, in order.
    async fn persist(&self, id: u64) -> Result<Outcome> {
        let mut db = self.db.clone();
        // Claim it, unless an undo removed it since it was read.
        let mut write = self.begin(&mut db).await?;
        let Some(mut row) = OutboxRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
        else {
            return Ok(Outcome::Gone);
        };
        if row.started_at.is_none() {
            OutboxRow::update_by_id(id)
                .started_at(Timestamp::now())
                .exec(write.exec())
                .await?;
        }
        write.commit().await?;
        let mut removed = false;
        while let Some(step) = row.steps.first().cloned() {
            // A refresh fetches the thread's rows, and fetches in full only the messages that
            // aren't stored yet.
            let stored: HashSet<String> = match step {
                Step::Refresh => MessageRow::filter_by_thread_id(&row.queue)
                    .select(MessageRow::fields().id())
                    .exec(&mut db)
                    .await?
                    .into_iter()
                    .collect(),
                _ => HashSet::new(),
            };
            let effect = match self.send(&row.queue, &step, &stored).await {
                Ok(effect) => effect,
                Err(failure) => return self.fail(row, failure).await,
            };
            row.steps.remove(0);
            row.failures = 0;
            let mut write = self.begin(&mut db).await?;
            if row.steps.is_empty() {
                OutboxRow::filter_by_id(id)
                    .delete()
                    .exec(write.exec())
                    .await?;
                removed = true;
            } else {
                OutboxRow::update_by_id(id)
                    .steps(row.steps.to_vec())
                    .failures(0)
                    .exec(write.exec())
                    .await?;
            }
            match effect {
                Effect::Labels => provider::apply_step(write.exec(), &row.queue, &step).await?,
                Effect::Replace(rows) => {
                    provider::save_rows(write.exec(), &rows).await?;
                }
                Effect::Deleted => provider::delete_thread(write.exec(), &row.queue).await?,
            }
            reindex(write.exec(), &row.queue, &self.router()).await?;
            write.publish().await?;
        }
        if !removed {
            self.remove(&row).await?;
        }
        Ok(Outcome::Done)
    }

    /// One Gmail call. For a refresh, that call fetches the rows of `thread`, without fetching
    /// its `stored` messages in full again.
    async fn send(
        &self,
        thread: &str,
        step: &Step,
        stored: &HashSet<String>,
    ) -> Result<Effect, Failure> {
        let sent = match step {
            Step::RemoveFromMessages { ids, labels } => {
                let request = BatchModifyMessagesRequest {
                    ids: ids.clone(),
                    remove_label_ids: labels.clone(),
                    ..BatchModifyMessagesRequest::default()
                };
                self.api
                    .call(Lane::Act, 1, |gmail| gmail.batch_modify_messages(&request))
                    .await
            }
            Step::ModifyThread { add, remove } => {
                let request = ModifyThreadRequest {
                    add_label_ids: add.clone(),
                    remove_label_ids: remove.clone(),
                };
                unless_gone(
                    self.api
                        .call(Lane::Act, 1, |gmail| gmail.modify_thread(thread, &request))
                        .await,
                )
            }
            Step::AddToMessage { id, label } => {
                let request = ModifyMessageRequest {
                    add_label_ids: vec![label.clone()],
                    ..ModifyMessageRequest::default()
                };
                unless_gone(
                    self.api
                        .call(Lane::Act, 1, |gmail| gmail.modify_message(id, &request))
                        .await,
                )
            }
            Step::Refresh => {
                return Ok(
                    match fetch_rows(&self.api, Lane::Act, 1, thread, stored).await? {
                        Some(rows) => Effect::Replace(rows),
                        None => Effect::Deleted,
                    },
                );
            }
        };
        sent.map(|()| Effect::Labels)
    }

    /// Backs off, or gives up and rolls the overlay back. Being offline, throttled or signed
    /// out doesn't count toward giving up. An error that retrying can't fix gives up at once.
    async fn fail(&self, row: OutboxRow, failure: Failure) -> Result<Outcome> {
        let counted = matches!(failure, Failure::Transient(_) | Failure::Permanent(_));
        let attempts = row.attempts + u32::from(counted);
        let failures = row.failures.saturating_add(1);
        let message = failure.message();
        let not_before = match queue::dispose_failure(&self.api, failure, failures) {
            Disposition::Postponed(until) if attempts <= MAX_ATTEMPTS => until,
            Disposition::Postponed(_) | Disposition::Refused(_) => {
                log::warn!(
                    "giving up on {:?} for thread {}: {message}",
                    row.mutation.0,
                    row.queue,
                );
                self.given_up
                    .lock()
                    .expect("given-up lock is never poisoned")
                    .insert(row.id);
                self.remove(&row).await?;
                return Ok(Outcome::Failed(FailedMutation {
                    thread: ThreadId::new(row.queue),
                    mutation: row.mutation.0,
                    undo: row.undo,
                    error: message,
                }));
            }
        };
        log::debug!(
            "postponing outbox row {} until {not_before}: {message}",
            row.id
        );
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        OutboxRow::update_by_id(row.id)
            .attempts(attempts)
            .failures(failures)
            .not_before(not_before)
            .exec(write.exec())
            .await?;
        write.commit().await?;
        Ok(Outcome::Postponed)
    }

    /// Drops a row, recomputing its thread without it.
    async fn remove(&self, row: &OutboxRow) -> Result<()> {
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        OutboxRow::filter_by_id(row.id)
            .delete()
            .exec(write.exec())
            .await?;
        reindex(write.exec(), &row.queue, &self.router()).await?;
        write.publish().await
    }
}

/// A label change on a thread, message or label that no longer exists has nothing left to do.
fn unless_gone<T>(sent: Result<T, Failure>) -> Result<(), Failure> {
    match sent {
        Err(failure)
            if failure.is_not_found() || failure.message().contains("labelId not found") =>
        {
            Ok(())
        }
        sent => sent.map(drop),
    }
}
