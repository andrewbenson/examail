//! Reminders (Remind me). Each thread's reminder is kept in `reminders`, and Exo brings threads
//! back itself when they're due, from [`Store::return_reminders`]. The app calls that when a
//! reminder is due while it runs, and once when it starts, so a reminder due while Exo was
//! closed comes back at the next launch.
//!
//! Gmail sees reminders as labels. A thread has `[Exo]/Is Snoozed` while a reminder is set, and
//! Remind me's snooze also takes it out of the inbox. When the thread comes back, it's in the
//! inbox again and has `[Exo]/ru` until it's read.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use toasty::Executor;

use crate::db::{self, DraftRow, GIVEN_REMINDERS, ReminderRow};
use crate::drafts::DraftReminder;
use crate::error::{Error, Result};
use crate::ids::ThreadId;
use crate::ids::labels::{SH_SNOOZED, SPAM, TRASH};
use crate::model::ReminderStatus;
use crate::mutation::{Mutation, Plan, change};
use crate::outbox::enqueue;
use crate::store::{Inner, Store, load_view, reindex};
use crate::view::ThreadView;

/// Persisted record of the reminder a held send put on its thread, kept in `kv[GIVEN_REMINDERS]`
/// so [`Inner::take_back_reminder`] can roll it back even after an app restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GivenReminder {
    pub reminder: DraftReminder,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<SavedReminder>,
}

/// Serializable snapshot of a [`ReminderRow`] displaced by a held send's reminder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedReminder {
    pub at: Timestamp,
    pub keep_on_reply: bool,
    pub message_ids: Vec<String>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<Timestamp>,
    #[serde(default)]
    pub aborted: bool,
}

impl From<&ReminderRow> for SavedReminder {
    fn from(row: &ReminderRow) -> Self {
        Self {
            at: row.at,
            keep_on_reply: row.keep_on_reply,
            message_ids: row.message_ids.clone(),
            created_at: row.created_at,
            fired_at: row.fired_at,
            aborted: row.aborted,
        }
    }
}

impl SavedReminder {
    fn into_row(self, thread_id: String) -> ReminderRow {
        ReminderRow {
            thread_id,
            at: self.at,
            keep_on_reply: self.keep_on_reply,
            message_ids: self.message_ids,
            created_at: self.created_at,
            fired_at: self.fired_at,
            aborted: self.aborted,
        }
    }
}

/// Backward-compatible decoder for `kv[GIVEN_REMINDERS]`: older builds stored bare
/// [`DraftReminder`] values without `thread_id` or `previous`.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredGiven {
    Full(GivenReminder),
    Legacy(DraftReminder),
}

/// Makes the reminder change that `mutation` makes to the thread of `view`. Returns what the
/// thread had before, for undo, or `None` for a mutation that doesn't touch reminders.
pub(crate) async fn apply(
    exec: &mut dyn Executor,
    view: &ThreadView,
    mutation: &Mutation,
    now: Timestamp,
) -> Result<Option<Option<ReminderRow>>> {
    let previous = view.reminder.clone();
    match mutation {
        Mutation::Remind {
            at, keep_on_reply, ..
        } => {
            // Set on every message the thread has now, so a reply is any other message.
            let row = reminder_row(view, *at, *keep_on_reply, None, now);
            restore(exec, &view.id, Some(row)).await?;
        }
        Mutation::CancelReminder { .. } => restore(exec, &view.id, None).await?,
        // Moving out of Reminders removes it.
        Mutation::Move {
            from: Some(from), ..
        } if from == SH_SNOOZED => restore(exec, &view.id, None).await?,
        _ => return Ok(None),
    }
    Ok(Some(previous))
}

/// A reminder for the thread of `view` at `at`, set on `set_on`, or on the thread's messages now
/// when that's `None`.
fn reminder_row(
    view: &ThreadView,
    at: Timestamp,
    keep_on_reply: bool,
    set_on: Option<Vec<String>>,
    now: Timestamp,
) -> ReminderRow {
    ReminderRow {
        thread_id: view.id.clone(),
        at,
        keep_on_reply,
        message_ids: set_on.unwrap_or_else(|| {
            view.messages
                .iter()
                .filter(|m| !m.is_draft())
                .map(|m| m.row.id.clone())
                .collect()
        }),
        created_at: now,
        fired_at: None,
        aborted: false,
    }
}

/// Gives `thread` the reminder `row`, or none.
pub(crate) async fn restore(
    exec: &mut dyn Executor,
    thread: &str,
    row: Option<ReminderRow>,
) -> Result<()> {
    ReminderRow::filter_by_thread_id(thread)
        .delete()
        .exec(exec)
        .await?;
    if let Some(row) = row {
        ReminderRow::create()
            .thread_id(row.thread_id)
            .at(row.at)
            .keep_on_reply(row.keep_on_reply)
            .message_ids(row.message_ids)
            .created_at(row.created_at)
            .fired_at(row.fired_at)
            .aborted(row.aborted)
            .exec(exec)
            .await?;
    }
    Ok(())
}

impl Store {
    /// When the soonest reminder is due, if one is set.
    ///
    /// # Errors
    /// On database failures.
    pub async fn next_reminder(&self) -> Result<Option<Timestamp>> {
        let mut db = self.inner.db.clone();
        Ok(waiting(&mut db)
            .await?
            .iter()
            .map(|row| {
                self.inner
                    .due_here(&crate::cloud::remind_key(&row.thread_id), row.at)
            })
            .min())
    }

    /// Brings back every thread whose reminder is due at `now`. The thread goes into the inbox,
    /// sorted as if it had just come in, and is "reminder unread" until it's read. A reminder
    /// that a reply cancelled ("if no reply") doesn't bring its thread back, and its label comes
    /// off. The label also comes off a thread in Spam or Trash, which has nothing to come back
    /// to. Returns the threads brought back, oldest reminder first. Flush afterwards.
    ///
    /// # Errors
    /// On database failures, or [`Error::NotSynced`](crate::Error::NotSynced) before the first
    /// sync has made the reminder labels.
    pub async fn return_reminders(&self, now: Timestamp) -> Result<Vec<ThreadId>> {
        let router = self.inner.router();
        let me = &router.me;
        let mut db = self.inner.db.clone();
        // Most looks find nothing to do, so they don't need the write lock.
        let rows = waiting(&mut db).await?;
        let ids: Vec<String> = rows.iter().map(|row| row.thread_id.clone()).collect();
        let views = crate::store::load_views(&mut db, &ids).await?;
        let due = |thread: &str, at: Timestamp| {
            self.inner.due_here(&crate::cloud::remind_key(thread), at) <= now
        };
        let ends = |view: Option<&ThreadView>, thread: &str, at: Timestamp| match view
            .map(|view| view.reminder_status(me))
        {
            None | Some(Some(ReminderStatus::Aborted { .. })) => true,
            Some(Some(ReminderStatus::Set { .. })) => due(thread, at),
            Some(_) => false,
        };
        if !rows
            .iter()
            .any(|row| ends(views.get(&row.thread_id), &row.thread_id, row.at))
        {
            return Ok(Vec::new());
        }
        // Handed-off reminders Exo is to do now, as the fallback: Gmail says whether Examail Cloud
        // did them meanwhile. Asked before the write, which mustn't wait on Gmail.
        let handed: Vec<String> = rows
            .iter()
            .filter(|row| {
                self.inner
                    .handed_off(&crate::cloud::remind_key(&row.thread_id))
                    && due(&row.thread_id, row.at)
            })
            .map(|row| row.thread_id.clone())
            .collect();
        let (ended_in_gmail, asked) = if handed.is_empty() {
            Default::default()
        } else {
            self.inner.ended_in_gmail(&handed).await
        };
        let mut write = self.inner.begin(&mut db).await?;
        let mut rows = waiting(write.exec()).await?;
        rows.sort_by_key(|row| row.at);
        let mut returned = Vec::new();
        let mut changed = false;
        for row in rows {
            let Some(view) = load_view(write.exec(), &row.thread_id).await? else {
                restore(write.exec(), &row.thread_id, None).await?;
                continue;
            };
            let take_label_off = |aborted: bool| ReminderRow {
                aborted,
                ..row.clone()
            };
            let handed_off = self.inner.handed_off(&crate::cloud::remind_key(&view.id));
            if handed_off && due(&view.id, row.at) {
                if ended_in_gmail.contains(&view.id) {
                    // Examail Cloud did it, or a reply called it off there: it only ends here.
                    log::info!("Examail Cloud already ended the reminder on {}", view.id);
                    restore(write.exec(), &view.id, None).await?;
                    reindex(write.exec(), &view.id, &router).await?;
                    changed = true;
                    continue;
                }
                if !asked.contains(&view.id) {
                    continue;
                }
            }
            let ended = match view.reminder_status(me) {
                Some(ReminderStatus::Aborted { .. }) => Some(Some(take_label_off(true))),
                Some(ReminderStatus::Set { at, .. })
                    if due(&view.id, at) && (view.has(SPAM) || view.has(TRASH)) =>
                {
                    Some(None)
                }
                Some(ReminderStatus::Set { at, .. }) if due(&view.id, at) => {
                    let Some((forward, _)) = Mutation::ReturnFromReminder.plan(&view, me)? else {
                        continue;
                    };
                    restore(
                        write.exec(),
                        &view.id,
                        Some(ReminderRow {
                            fired_at: Some(now),
                            ..row
                        }),
                    )
                    .await?;
                    enqueue(
                        write.exec(),
                        &view,
                        &forward,
                        &Mutation::ReturnFromReminder,
                        false,
                        me,
                    )
                    .await?;
                    reindex(write.exec(), &view.id, &router).await?;
                    returned.push(ThreadId::new(view.id));
                    changed = true;
                    continue;
                }
                _ => None,
            };
            let Some(kept) = ended else { continue };
            restore(write.exec(), &view.id, kept).await?;
            if let Some(label) = me.snoozed_label.as_deref().filter(|label| view.has(label)) {
                let plan = Plan {
                    intents: vec![change(&[], &[label])],
                    replace_key: None,
                };
                enqueue(
                    write.exec(),
                    &view,
                    &plan,
                    &Mutation::CancelReminder { to_inbox: false },
                    false,
                    me,
                )
                .await?;
            }
            reindex(write.exec(), &view.id, &router).await?;
            changed = true;
        }
        if changed {
            write.publish().await?;
        } else {
            write.commit().await?;
        }
        Ok(returned)
    }
}

impl Store {
    /// Brings back to the inbox, once, the threads Gmail has under `[Exo]/Is Snoozed` that a new
    /// store has no reminder for. Their reminders were in a store that was signed out or
    /// cleared, or on another Mac, so nothing here would ever return them. Until it has done
    /// this once, a call lists the label, stores the threads it lacks and returns them the way
    /// due reminders return. After that, it does nothing. Returns the threads it brought back.
    ///
    /// # Errors
    /// [`Error::Offline`] and the like when Gmail can't be reached (it tries again next time), and
    /// on database failures.
    pub async fn return_orphaned_snoozes(&self) -> Result<Vec<ThreadId>> {
        let inner = &self.inner;
        let mut db = inner.db.clone();
        if db::kv_get::<bool>(&mut db, db::ORPHANED_SNOOZES)
            .await?
            .is_none()
        {
            return Ok(Vec::new());
        }
        inner.bootstrapped().await?;
        let router = inner.router();
        let me = &router.me;
        let label = me
            .snoozed_label
            .clone()
            .ok_or(Error::NotSynced("the reminder labels"))?;
        let mut listed: Vec<(String, u64)> = Vec::new();
        let mut query = gmail::ListQuery {
            label_ids: vec![label],
            max_results: Some(100),
            ..gmail::ListQuery::default()
        };
        loop {
            let page = inner
                .api
                .call(crate::quota::Lane::Sync, crate::sync::ATTEMPTS, |gmail| {
                    gmail.threads(&query)
                })
                .await?;
            listed.extend(page.threads.iter().filter_map(|thread| {
                Some((thread.id.clone()?, thread.history_id.unwrap_or_default()))
            }));
            match page.next_page_token {
                Some(token) => query.page_token = Some(token),
                None => break,
            }
        }
        let fetched = inner.fetch_stale(crate::quota::Lane::Sync, &listed).await?;
        let mut write = inner.begin(&mut db).await?;
        // Re-check inside the write transaction so concurrent calls cannot enqueue duplicate
        // `ReturnFromReminder` mutations.
        if db::kv_get::<bool>(write.exec(), db::ORPHANED_SNOOZES)
            .await?
            .is_none()
        {
            write.commit().await?;
            return Ok(Vec::new());
        }
        for rows in &fetched {
            if let Some(id) = crate::provider::save_rows(write.exec(), rows).await? {
                reindex(write.exec(), &id, &router).await?;
            }
        }
        let remembered: HashSet<String> = ReminderRow::all()
            .exec(write.exec())
            .await?
            .into_iter()
            .map(|row| row.thread_id)
            .collect();
        let mut returned = Vec::new();
        for (thread, _) in listed
            .iter()
            .filter(|(thread, _)| !remembered.contains(thread))
        {
            let Some(view) = load_view(write.exec(), thread).await? else {
                continue;
            };
            let Some((forward, _)) = Mutation::ReturnFromReminder.plan(&view, me)? else {
                continue;
            };
            enqueue(
                write.exec(),
                &view,
                &forward,
                &Mutation::ReturnFromReminder,
                false,
                me,
            )
            .await?;
            reindex(write.exec(), &view.id, &router).await?;
            returned.push(ThreadId::new(view.id));
        }
        crate::db::KvRow::filter_by_key(db::ORPHANED_SNOOZES)
            .delete()
            .exec(write.exec())
            .await?;
        write.publish().await?;
        if !returned.is_empty() {
            log::info!(
                "brought back {} snoozed threads this store has no reminder for",
                returned.len()
            );
        }
        Ok(returned)
    }
}

impl Inner {
    /// Puts the reminder of held send `id` on its thread, as Send and Schedule do, replacing the
    /// thread's earlier reminder and leaving the thread where it is. From then on the thread
    /// shows the reminder and is in Reminders, and Gmail has `[Exo]/Is Snoozed`. A thread that
    /// isn't stored yet (a new message's thread, before Gmail has it) gets the reminder once
    /// it's stored, at the draft's first write or else at its send. Returns whether it changed
    /// the thread. Flush when it did.
    pub(crate) async fn give_reminder(&self, id: &str) -> Result<bool> {
        let _giving = self.giving.lock().await;
        let mut db = self.db.clone();
        let Some(row) = DraftRow::filter_by_id(id).first().exec(&mut db).await? else {
            return Ok(false);
        };
        let held = row.send_at.is_some() && !row.discarded && row.failure.is_none();
        let (true, Some(reminder), Some(thread)) = (held, row.record.0.reminder, row.thread_id)
        else {
            return Ok(false);
        };
        let existing = given(&mut db).await?;
        if existing.get(id).map(|entry| entry.reminder) == Some(reminder) {
            return Ok(false);
        }
        let had_earlier_in_kv = existing.contains_key(id);
        let inherited_previous = existing.get(id).and_then(|entry| entry.previous.clone());
        // If a reminder was given before in this session (a send called off by Edit Message and
        // scheduled again), that one comes off first, so undo puts back what the thread had
        // before either.
        let earlier = self.given.lock().expect("never poisoned").remove(id);
        let had_session_undo = earlier.is_some();
        if let Some(earlier) = earlier {
            self.undo(earlier).await?;
        }
        let mutation = Mutation::Remind {
            at: reminder.at,
            keep_on_reply: reminder.keep_on_reply,
            snooze: false,
        };
        let undo = match self.apply(&[ThreadId::new(thread.clone())], mutation).await {
            Ok(undo) => undo,
            // The thread isn't stored yet, or the first sync hasn't made the reminder labels, so
            // the reminder waits until later.
            Err(Error::UnknownThread(_) | Error::NotSynced(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let previous = if had_earlier_in_kv && !had_session_undo {
            inherited_previous
        } else {
            undo.prior_reminder().map(SavedReminder::from)
        };
        let mut write = self.begin(&mut db).await?;
        let mut given = given(write.exec()).await?;
        let drafts: HashSet<String> = DraftRow::all()
            .select(DraftRow::fields().id())
            .exec(write.exec())
            .await?
            .into_iter()
            .collect();
        given.retain(|draft, _| drafts.contains(draft));
        given.insert(
            id.to_owned(),
            GivenReminder {
                reminder,
                thread_id: Some(thread),
                previous,
            },
        );
        db::kv_set(write.exec(), GIVEN_REMINDERS, &given).await?;
        write.commit().await?;
        self.given
            .lock()
            .expect("never poisoned")
            .insert(id.to_owned(), undo);
        Ok(true)
    }

    /// Undo send for held send `id`. The reminder it put on its thread comes off, and the one
    /// the thread had comes back. Works across app restarts using the persisted entry in
    /// `kv[GIVEN_REMINDERS]` when the in-memory [`Undo`](crate::Undo) is gone.
    pub(crate) async fn take_back_reminder(&self, id: &str) -> Result<()> {
        let _giving = self.giving.lock().await;
        let undo = self.given.lock().expect("never poisoned").remove(id);
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let mut given = given(write.exec()).await?;
        let removed = given.remove(id);
        if removed.is_some() {
            db::kv_set(write.exec(), GIVEN_REMINDERS, &given).await?;
        }
        write.commit().await?;
        if let Some(undo) = undo {
            return self.undo(undo).await;
        }
        let Some(entry) = removed else {
            return Ok(());
        };
        let thread = match entry.thread_id {
            Some(thread) => Some(thread),
            None => DraftRow::filter_by_id(id)
                .select(DraftRow::fields().thread_id())
                .first()
                .exec(&mut db)
                .await?
                .flatten(),
        };
        let Some(thread) = thread else {
            return Ok(());
        };
        self.take_back_persisted(&thread, entry.previous).await
    }

    /// Rolls back a held send's reminder after a restart when the session's `Undo` is gone:
    /// cancels the reminder when the thread had none before, or restores `previous`.
    async fn take_back_persisted(
        &self,
        thread: &str,
        previous: Option<SavedReminder>,
    ) -> Result<()> {
        let Some(previous) = previous else {
            return match self
                .apply(
                    &[ThreadId::new(thread.to_owned())],
                    Mutation::CancelReminder { to_inbox: false },
                )
                .await
            {
                Ok(_) | Err(Error::UnknownThread(_) | Error::NotSynced(_)) => Ok(()),
                Err(error) => Err(error),
            };
        };
        let router = self.router();
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let Some(view) = load_view(write.exec(), thread).await? else {
            return write.commit().await;
        };
        let mutation = if previous.fired_at.is_some() || previous.aborted {
            Mutation::CancelReminder { to_inbox: false }
        } else {
            Mutation::Remind {
                at: previous.at,
                keep_on_reply: previous.keep_on_reply,
                snooze: false,
            }
        };
        match mutation.plan(&view, &router.me) {
            Ok(Some((forward, _))) => {
                enqueue(write.exec(), &view, &forward, &mutation, false, &router.me).await?;
            }
            Ok(None) | Err(Error::NotSynced(_)) => {}
            Err(error) => return Err(error),
        }
        restore(
            write.exec(),
            &view.id,
            Some(previous.into_row(view.id.clone())),
        )
        .await?;
        reindex(write.exec(), &view.id, &router).await?;
        write.publish().await
    }

    /// Gives `thread` the reminder a draft carried, once Gmail has sent the draft into the
    /// thread, when the send couldn't because its thread wasn't stored yet. It replaces the
    /// thread's earlier reminder, and like Remind me's reminder mode it leaves the thread where it
    /// is. The reminder is set on `set_on`, the messages a scheduled send recorded (so a reply
    /// since then cancels an "if no reply" reminder), or else on the thread's messages now.
    pub(crate) async fn remind_sent(
        &self,
        thread: &str,
        reminder: DraftReminder,
        set_on: Option<Vec<String>>,
    ) -> Result<()> {
        let router = self.router();
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        let Some(view) = load_view(write.exec(), thread).await? else {
            return write.commit().await;
        };
        let mutation = Mutation::Remind {
            at: reminder.at,
            keep_on_reply: reminder.keep_on_reply,
            snooze: false,
        };
        let Some((forward, _)) = mutation.plan(&view, &router.me)? else {
            return write.commit().await;
        };
        restore(
            write.exec(),
            &view.id,
            Some(reminder_row(
                &view,
                reminder.at,
                reminder.keep_on_reply,
                set_on,
                Timestamp::now(),
            )),
        )
        .await?;
        enqueue(write.exec(), &view, &forward, &mutation, false, &router.me).await?;
        reindex(write.exec(), &view.id, &router).await?;
        write.publish().await
    }
}

/// The held sends whose reminder is on their thread, and what each one gave (with thread id and
/// replaced reminder so taking it back survives an app restart).
pub(crate) async fn given(exec: &mut dyn Executor) -> Result<HashMap<String, GivenReminder>> {
    let raw: HashMap<String, StoredGiven> =
        db::kv_get(exec, GIVEN_REMINDERS).await?.unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|(id, stored)| {
            let entry = match stored {
                StoredGiven::Full(full) => full,
                StoredGiven::Legacy(reminder) => GivenReminder {
                    reminder,
                    thread_id: None,
                    previous: None,
                },
            };
            (id, entry)
        })
        .collect())
}

/// The reminders that haven't brought their thread back or been cancelled by a reply.
pub(crate) async fn waiting(exec: &mut dyn Executor) -> Result<Vec<ReminderRow>> {
    let mut rows = ReminderRow::all().exec(exec).await?;
    rows.retain(|row| row.fired_at.is_none() && !row.aborted);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafts::{Draft, DraftAction, DraftRecord};
    use crate::ids::labels::INBOX;
    use crate::view::Identity;

    fn sample_record(reminder: Option<DraftReminder>) -> DraftRecord {
        DraftRecord {
            action: DraftAction::Reply,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Follow up".into(),
            body: "<div>ping</div>".into(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "d1@exo".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder,
        }
    }

    async fn seed_store(name: &str) -> (std::path::PathBuf, Store) {
        let dir =
            std::env::temp_dir().join(format!("exo-store-reminders-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        let identity = Identity {
            addresses: vec!["maya@brightmoor.co".into()],
            snoozed_label: Some("Label_Snoozed".into()),
            reminder_unread_label: Some("Label_RU".into()),
            ..Identity::default()
        };
        let mut db = store.inner.db.clone();
        let mut write = store.inner.begin(&mut db).await.expect("tx");
        db::kv_set(write.exec(), db::IDENTITY, &identity)
            .await
            .expect("sets identity");
        let msg = gmail::Message {
            id: Some("m1".into()),
            thread_id: Some("t1".into()),
            internal_date: Some(1_794_900_000_000),
            label_ids: vec![INBOX.into()],
            payload: Some(gmail::MessagePart {
                headers: vec![
                    gmail::MessagePartHeader {
                        name: "Subject".into(),
                        value: "Follow up".into(),
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
        write.commit().await.expect("commits");
        *store.inner.identity.write().expect("lock") = identity;
        (dir, store)
    }

    #[tokio::test]
    async fn take_back_reminder_works_after_restart_without_and_with_prior_reminder() {
        let (dir, store) = seed_store("restart").await;
        let t1 = ThreadId::new("t1");
        let first_at: Timestamp = "2026-12-01T10:00:00Z".parse().expect("ts");
        let draft_at: Timestamp = "2026-12-05T10:00:00Z".parse().expect("ts");

        // 1. Thread with NO prior reminder: hold send with reminder, simulate restart, undo send.
        store
            .save_draft(Draft {
                id: "draft001".into(),
                thread: Some(t1.clone()),
                adopted: None,
                record: sample_record(Some(DraftReminder {
                    at: draft_at,
                    keep_on_reply: false,
                    automatic: false,
                })),
                raw: String::new(),
                complete: true,
                sending: true,
            })
            .await
            .expect("saves");
        store
            .send_draft("draft001", draft_at)
            .await
            .expect("holds send");
        assert_eq!(store.next_reminder().await.expect("next"), Some(draft_at));

        // Reopen store (simulates app restart: in-memory `self.given` is empty).
        drop(store);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("reopens");
        assert!(store.undo_send("draft001").await.expect("undo send"));
        assert_eq!(
            store.next_reminder().await.expect("next"),
            None,
            "reminder cancelled across restart when no prior reminder existed"
        );
        let mut db = store.inner.db.clone();
        let view = load_view(&mut db, "t1").await.expect("load").expect("t1");
        assert!(!view.has("Label_Snoozed"), "snoozed label removed");

        // 2. Thread WITH a prior reminder: hold send replaces it; restart + undo restores prior.
        store
            .apply(
                std::slice::from_ref(&t1),
                Mutation::Remind {
                    at: first_at,
                    keep_on_reply: true,
                    snooze: false,
                },
            )
            .await
            .expect("sets prior reminder");
        store
            .send_draft("draft001", draft_at)
            .await
            .expect("holds send");
        assert_eq!(store.next_reminder().await.expect("next"), Some(draft_at));

        drop(db);
        drop(store);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("reopens");
        assert!(store.undo_send("draft001").await.expect("undo send"));
        assert_eq!(
            store.next_reminder().await.expect("next"),
            Some(first_at),
            "prior reminder restored across restart"
        );
        let mut db = store.inner.db.clone();
        let restored = ReminderRow::filter_by_thread_id("t1")
            .first()
            .exec(&mut db)
            .await
            .expect("reads")
            .expect("prior row restored");
        assert!(restored.keep_on_reply);

        // 3. Legacy GIVEN_REMINDERS format (bare DraftReminder without thread_id/previous) still
        // deserializes cleanly and falls back to the draft row's thread_id.
        let legacy = serde_json::json!({
            "draft001": { "at": draft_at.to_string() }
        });
        let mut write = store.inner.begin(&mut db).await.expect("tx");
        db::kv_set(write.exec(), GIVEN_REMINDERS, &legacy)
            .await
            .expect("writes legacy");
        write.commit().await.expect("commits");
        let parsed = given(&mut db).await.expect("reads legacy");
        assert_eq!(parsed["draft001"].reminder.at, draft_at);
        assert_eq!(parsed["draft001"].thread_id, None);
        store
            .inner
            .take_back_reminder("draft001")
            .await
            .expect("takes back legacy");
        assert_eq!(store.next_reminder().await.expect("next"), None);

        drop(db);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
