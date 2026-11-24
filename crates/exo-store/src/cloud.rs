//! Reminders and scheduled sends handed to Examail Cloud, which does them while Exo is closed.
//!
//! The app lists what to hand off ([`Store::cloud_jobs`]), sends it, and says which jobs the
//! service holds ([`Store::set_handed_off`]). Exo stays the fallback for those: it does a
//! handed-off job itself only [`CLOUD_GRACE`] after it's due, and then from what Gmail says, not
//! from what Exo last stored, because the service may have done it meanwhile:
//!
//! - A send: the draft is gone and its message is in Sent, which the send already checks for.
//! - A reminder: its thread no longer has `[Exo]/Is Snoozed`, which Exo reads from Gmail before
//!   bringing a handed-off thread back. The reminder then only ends here.

use std::collections::{BTreeSet, HashSet};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use crate::db::{self, DraftRow};
use crate::error::Result;
use crate::quota::Lane;
use crate::store::{Inner, Store};

/// How long after a handed-off job is due Exo waits for Examail Cloud before doing it itself.
pub const CLOUD_GRACE: SignedDuration = SignedDuration::from_mins(2);

/// How long ago a job may have been due and still be handed off. The service refuses one due
/// more than a day ago.
const OLDEST: SignedDuration = SignedDuration::from_hours(23);

/// Where the handed-off jobs' keys are kept in `kv`, so the wait holds from launch, before the
/// service has been reached.
pub(crate) const HANDED_OFF: &str = "cloud_handed_off";

/// One job for Examail Cloud: its key, stable for the thread or draft, and the request
/// (`PUT /v1/jobs/{key}`).
#[derive(Debug, Clone, PartialEq)]
pub struct CloudJob {
    pub key: String,
    pub request: Value,
}

/// The key of the reminder on `thread`.
#[must_use]
pub fn remind_key(thread: &str) -> String {
    format!("remind:{thread}")
}

/// The key of draft `draft`'s scheduled send.
#[must_use]
pub fn send_key(draft: &str) -> String {
    format!("send:{draft}")
}

impl Inner {
    /// When Exo does job `key`, due `at`, itself: then, or [`CLOUD_GRACE`] later when Examail
    /// Cloud holds it.
    pub(crate) fn due_here(&self, key: &str, at: Timestamp) -> Timestamp {
        if self.handed_off(key) {
            at.checked_add(self.cloud_grace).unwrap_or(at)
        } else {
            at
        }
    }

    pub(crate) fn handed_off(&self, key: &str) -> bool {
        self.handed_off
            .read()
            .expect("never poisoned")
            .contains(key)
    }

    /// Of `threads`, whose handed-off reminders are due here, the ones Gmail says have ended: the
    /// thread lost `[Exo]/Is Snoozed` (Examail Cloud brought it back, or a reply called it off) or
    /// is gone. A thread Gmail can't be asked about now is in neither list, and waits.
    pub(crate) async fn ended_in_gmail(
        &self,
        threads: &[String],
    ) -> (HashSet<String>, HashSet<String>) {
        let mut ended = HashSet::new();
        let mut asked = HashSet::new();
        let Some(snoozed) = self.router().me.snoozed_label.clone() else {
            return (ended, asked);
        };
        for thread in threads {
            match self
                .api
                .call(Lane::Act, 2, |gmail| {
                    gmail.thread(thread, gmail::Format::Minimal, &[])
                })
                .await
            {
                Ok(found) => {
                    asked.insert(thread.clone());
                    let snoozed_now = found
                        .messages
                        .iter()
                        .any(|message| message.label_ids.contains(&snoozed));
                    if !snoozed_now {
                        ended.insert(thread.clone());
                    }
                }
                Err(failure) if failure.is_not_found() => {
                    asked.insert(thread.clone());
                    ended.insert(thread.clone());
                }
                Err(failure) => {
                    log::info!(
                        "can't ask Gmail about handed-off reminder {thread} now: {}",
                        failure.message()
                    );
                }
            }
        }
        (ended, asked)
    }
}

impl Store {
    /// The reminders and scheduled sends to hand to Examail Cloud: every reminder waiting, and
    /// every scheduled send whose current version Gmail has. A send being written, or a reminder
    /// before the first sync has made Exo's labels, waits for the next call.
    ///
    /// # Errors
    /// On database failures.
    pub async fn cloud_jobs(&self) -> Result<Vec<CloudJob>> {
        let now = Timestamp::now();
        let oldest = now.checked_sub(OLDEST).unwrap_or(now);
        let me = self.inner.router().me.clone();
        let mut db = self.inner.db.clone();
        let mut jobs = Vec::new();
        if let (Some(snoozed), Some(reminded)) = (&me.snoozed_label, &me.reminder_unread_label) {
            for row in crate::reminders::waiting(&mut db).await? {
                if row.at < oldest {
                    continue;
                }
                jobs.push(CloudJob {
                    key: remind_key(&row.thread_id),
                    request: json!({
                        "kind": "remind",
                        "thread_id": row.thread_id,
                        "snoozed_label": snoozed,
                        "reminded_label": reminded,
                        "at": row.at.to_string(),
                        "if_no_reply": !row.keep_on_reply,
                        "message_ids": row.message_ids,
                    }),
                });
            }
        }
        for row in DraftRow::all().exec(&mut db).await? {
            let Some(at) = crate::drafts::scheduled(&row) else {
                continue;
            };
            let (Some(draft_id), Some(message_id)) = (&row.gmail_id, &row.message_id) else {
                continue;
            };
            let record = &row.record.0;
            let Some(later) = &record.send_later else {
                continue;
            };
            if row.pushed < row.revision || at < oldest || record.rfc822_id.is_empty() {
                continue;
            }
            jobs.push(CloudJob {
                key: send_key(&row.id),
                request: json!({
                    "kind": "send",
                    "thread_id": row.thread_id,
                    "draft_id": draft_id,
                    "rfc822_id": record.rfc822_id,
                    "message_id": message_id,
                    "at": at.to_string(),
                    "if_no_reply": later.abort_on_reply,
                    "message_ids": later.current_message_ids,
                }),
            });
        }
        Ok(jobs)
    }

    /// Says which jobs Examail Cloud now holds, by key, so Exo waits [`CLOUD_GRACE`] past their
    /// times before doing them itself. Every other job is Exo's alone again, and done on time.
    ///
    /// # Errors
    /// On database failures.
    pub async fn set_handed_off(&self, keys: impl IntoIterator<Item = String>) -> Result<()> {
        let keys: BTreeSet<String> = keys.into_iter().collect();
        let changed = {
            let mut held = self.inner.handed_off.write().expect("never poisoned");
            let next: HashSet<String> = keys.iter().cloned().collect();
            let changed = *held != next;
            *held = next;
            changed
        };
        if changed {
            let mut db = self.inner.db.clone();
            let mut write = self.inner.begin(&mut db).await?;
            db::kv_set(write.exec(), HANDED_OFF, &keys).await?;
            write.commit().await?;
            // A job taken back may be due now, so the loops that do them look again.
            self.inner.notify();
        }
        Ok(())
    }

    /// The keys of the jobs Examail Cloud holds, as last said.
    #[must_use]
    pub fn handed_off(&self) -> HashSet<String> {
        self.inner
            .handed_off
            .read()
            .expect("never poisoned")
            .clone()
    }
}

/// The handed-off keys kept in `kv`.
pub(crate) async fn load(exec: &mut dyn toasty::Executor) -> Result<HashSet<String>> {
    Ok(db::kv_get::<BTreeSet<String>>(exec, HANDED_OFF)
        .await?
        .unwrap_or_default()
        .into_iter()
        .collect())
}

/// A reminder waiting to bring its thread back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingReminder {
    pub thread_id: String,
    pub at: Timestamp,
    /// A reply calls it off.
    pub if_no_reply: bool,
}

impl Store {
    /// The reminders waiting to bring their threads back, soonest first.
    ///
    /// # Errors
    /// On database failures.
    pub async fn waiting_reminders(&self) -> Result<Vec<WaitingReminder>> {
        let mut db = self.inner.db.clone();
        let mut rows: Vec<WaitingReminder> = crate::reminders::waiting(&mut db)
            .await?
            .into_iter()
            .map(|row| WaitingReminder {
                thread_id: row.thread_id,
                at: row.at,
                if_no_reply: !row.keep_on_reply,
            })
            .collect();
        rows.sort_by_key(|row| row.at);
        Ok(rows)
    }
}
