//! Reminders and Send later, fired while the app is closed.
//!
//! The app records each one here as a job (`PUT /v1/jobs/{key}`) and keeps it up to date; the
//! per-minute tick fires the jobs that are due, doing to Gmail what the app would have:
//!
//! - **Remind me**: the thread comes back to the inbox with the "reminded" label, and the
//!   snoozed label comes off. An "if no reply" reminder that a reply called off only loses the
//!   snoozed label, as does a thread in Spam or Trash.
//! - **Send later**: the Gmail draft is sent, unless an "if no reply" send was called off by a
//!   reply, or it's more than [`LATE_SEND`] late (a surprise a day on is worse than not sending).
//!
//! A job is due while its `due` field, an RFC 3339 time, is at most now; a job that's finished
//! has [`FINISHED`] there, which sorts after every time. The tick claims a job by moving `due` a
//! lease ahead with a write conditional on the job not having changed since it was read, so two
//! ticks never fire one job twice. A tick that dies leaves the lease to run out, and the next
//! tick after that tries again, up to [`ATTEMPTS`] times.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::App;
use crate::firestore::Document;

pub const COLLECTION: &str = "jobs";
/// A finished job's `due`: after every RFC 3339 time, so due queries never find it.
pub const FINISHED: &str = "~";
/// How long a claimed job is left to its tick before another may take it.
const LEASE: SignedDuration = SignedDuration::from_mins(5);
/// How many ticks may try a job before it's failed.
const ATTEMPTS: u32 = 5;
/// How late a scheduled send may still go out.
const LATE_SEND: SignedDuration = SignedDuration::from_hours(24);
/// How many jobs one tick takes on.
const BATCH: u32 = 50;

const INBOX: &str = "INBOX";
const SENT: &str = "SENT";
const DRAFT: &str = "DRAFT";
const SPAM: &str = "SPAM";
const TRASH: &str = "TRASH";

/// What the app asks for: one reminder or one scheduled send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Work {
    Remind {
        thread_id: String,
        /// The app's `[Exo]/Is Snoozed` and `[Exo]/ru` label ids for this account.
        snoozed_label: String,
        reminded_label: String,
    },
    Send {
        thread_id: Option<String>,
        /// The Gmail draft to send.
        draft_id: String,
        /// The message's `Message-ID`, to tell whether it already went (sent elsewhere, or an
        /// attempt whose answer was lost).
        rfc822_id: String,
    },
}

/// A job as the app sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    #[serde(flatten)]
    pub work: Work,
    /// When it's due, in RFC 3339.
    pub at: Timestamp,
    /// "If no reply": a reply before then calls it off.
    #[serde(default)]
    pub if_no_reply: bool,
    /// The thread's messages when it was set. Any other message that isn't the user's or a draft
    /// is a reply.
    #[serde(default)]
    pub message_ids: Vec<String>,
}

/// How a job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for its time, or for a tick to try it again.
    Pending,
    /// Done: the thread came back, or the draft was sent.
    Done,
    /// A reply called it off ("if no reply").
    Aborted,
    /// It couldn't be done; `error` says why.
    Failed,
}

/// A job as it's kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub sub: String,
    pub key: String,
    #[serde(flatten)]
    pub work: Work,
    /// When it's due, `YYYY-MM-DDTHH:MM:SSZ`.
    pub at: String,
    pub if_no_reply: bool,
    pub message_ids: Vec<String>,
    pub status: Status,
    /// When a tick may next take it; [`FINISHED`] once it's done.
    pub due: String,
    pub attempts: u32,
    pub error: Option<String>,
    /// When it finished.
    pub finished_at: Option<String>,
    /// The sent message, for a send.
    pub sent_message_id: Option<String>,
}

/// A time as jobs keep them: whole seconds in UTC, so they sort as strings.
pub fn stamp(at: Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl Job {
    /// A new job for `sub`'s `key`, or the app's new version of it, waiting for its time.
    pub fn new(sub: &str, key: &str, request: Request) -> Self {
        let at = stamp(request.at);
        Self {
            sub: sub.to_owned(),
            key: key.to_owned(),
            work: request.work,
            due: at.clone(),
            at,
            if_no_reply: request.if_no_reply,
            message_ids: request.message_ids,
            status: Status::Pending,
            attempts: 0,
            error: None,
            finished_at: None,
            sent_message_id: None,
        }
    }

    fn finish(&mut self, status: Status, error: Option<String>, now: Timestamp) {
        self.status = status;
        self.error = error;
        self.due = FINISHED.to_owned();
        self.finished_at = Some(stamp(now));
    }
}

/// The document id of `sub`'s job `key`.
pub fn document_id(sub: &str, key: &str) -> String {
    format!("{sub}:{key}")
}

/// Whether `key` is one the app may use: short, and safe in a document id and a URL.
pub fn valid_key(key: &str) -> bool {
    (1..=200).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// What firing a job comes to, decided from the thread as Gmail has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Change the thread's labels, then finish with `status`.
    Labels {
        add: Vec<String>,
        remove: Vec<String>,
        status: Status,
    },
    /// Send the draft.
    Send,
    /// Finish with `status` without touching Gmail.
    Finish(Status, Option<String>),
}

/// A message of the thread, as far as firing needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub id: String,
    pub labels: Vec<String>,
}

/// Whether `thread` has a reply the job wasn't set on: a message that isn't the user's (sent)
/// or a draft.
fn replied(job: &Job, thread: &[Seen]) -> bool {
    let known: HashSet<&str> = job.message_ids.iter().map(String::as_str).collect();
    thread.iter().any(|message| {
        !known.contains(message.id.as_str())
            && !message
                .labels
                .iter()
                .any(|label| label == SENT || label == DRAFT)
    })
}

/// What firing `job` does, given its thread's messages (`None`: the thread is gone) at `now`.
pub fn plan(job: &Job, thread: Option<&[Seen]>, now: Timestamp) -> Plan {
    let replied = job.if_no_reply && thread.is_some_and(|thread| replied(job, thread));
    match &job.work {
        Work::Remind {
            snoozed_label,
            reminded_label,
            ..
        } => {
            let Some(thread) = thread else {
                return Plan::Finish(Status::Done, Some("the thread is gone".into()));
            };
            let has = |label: &str| {
                thread
                    .iter()
                    .any(|message| message.labels.iter().any(|have| have == label))
            };
            let off = |status| Plan::Labels {
                add: Vec::new(),
                remove: vec![snoozed_label.clone()],
                status,
            };
            if replied {
                off(Status::Aborted)
            } else if has(SPAM) || has(TRASH) {
                off(Status::Done)
            } else {
                Plan::Labels {
                    add: vec![INBOX.to_owned(), reminded_label.clone()],
                    remove: vec![snoozed_label.clone()],
                    status: Status::Done,
                }
            }
        }
        Work::Send { .. } => {
            let late = job
                .at
                .parse::<Timestamp>()
                .is_ok_and(|at| now.duration_since(at) > LATE_SEND);
            if replied {
                Plan::Finish(Status::Aborted, None)
            } else if late {
                Plan::Finish(Status::Failed, Some("more than a day late".into()))
            } else {
                Plan::Send
            }
        }
    }
}

/// Fires the jobs that are due. Returns how many it finished.
pub async fn tick(app: &App) -> Result<usize> {
    let now = Timestamp::now();
    let due = app
        .store
        .db()
        .at_most(COLLECTION, "due", &serde_json::Value::from(stamp(now)), BATCH)
        .await?;
    let mut finished = 0;
    let mut accounts: HashMap<String, Option<gmail::Client>> = HashMap::new();
    for document in due {
        let Some(mut job) = claim(app, &document, now).await? else {
            continue;
        };
        if job.status != Status::Pending {
            finished += 1;
            continue;
        }
        let gmail = match accounts.get(&job.sub) {
            Some(gmail) => gmail.clone(),
            None => {
                let gmail = account_gmail(app, &job.sub).await?;
                accounts.insert(job.sub.clone(), gmail.clone());
                gmail
            }
        };
        let Some(gmail) = gmail else {
            job.finish(Status::Failed, Some("the account isn't connected".into()), now);
            app.store.put_job(&document.id, &job).await?;
            finished += 1;
            continue;
        };
        match fire(&gmail, &mut job, now).await {
            Ok(()) => {
                app.store.put_job(&document.id, &job).await?;
                finished += 1;
                log::info!("job {} {:?}", document.id, job.status);
            }
            // The lease runs out and a later tick tries again.
            Err(error) => log::warn!("job {} attempt {}: {error:#}", document.id, job.attempts),
        }
    }
    Ok(finished)
}

/// Takes `document` for this tick: its lease moves ahead and its attempts go up, or it's failed
/// once it has run out of attempts. `None` when another tick took it first.
async fn claim(app: &App, document: &Document, now: Timestamp) -> Result<Option<Job>> {
    let mut job: Job = serde_json::from_value(document.fields.clone())
        .with_context(|| format!("reading job {}", document.id))?;
    if job.attempts >= ATTEMPTS {
        job.finish(Status::Failed, Some(format!("{ATTEMPTS} attempts failed")), now);
    } else {
        job.attempts += 1;
        job.due = stamp(now.checked_add(LEASE)?);
    }
    let taken = app
        .store
        .db()
        .set_if_unchanged(COLLECTION, &document.id, &job, &document.update_time)
        .await?;
    Ok(taken.then_some(job))
}

/// A Gmail client acting as `sub`, from the service's sealed grant. `None` when the account
/// isn't connected, or Google no longer honors the grant (it's then marked revoked).
async fn account_gmail(app: &App, sub: &str) -> Result<Option<gmail::Client>> {
    let Some(grant) = app.store.grant(sub).await? else {
        return Ok(None);
    };
    let refresh = String::from_utf8(app.google.open(&grant.sealed_refresh_token).await?)?;
    match app.google.oauth.refresh(&refresh).await {
        Ok(tokens) => Ok(Some(gmail::Client::new(
            gmail::Endpoints::google(),
            gmail::StaticToken(tokens.access_token),
        ))),
        Err(gmail::Error::OAuth(error)) if error.error == "invalid_grant" => {
            if let Some(mut account) = app.store.account(sub).await? {
                account.status = "revoked".into();
                app.store.put_account(sub, &account).await?;
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Does `job` to Gmail. An error leaves it for another attempt.
async fn fire(gmail: &gmail::Client, job: &mut Job, now: Timestamp) -> Result<()> {
    let thread_id = match &job.work {
        Work::Remind { thread_id, .. } => Some(thread_id.clone()),
        Work::Send { thread_id, .. } if job.if_no_reply => thread_id.clone(),
        Work::Send { .. } => None,
    };
    let thread = match &thread_id {
        Some(id) => thread(gmail, id).await?,
        None => Some(Vec::new()),
    };
    match plan(job, thread.as_deref(), now) {
        Plan::Finish(status, error) => job.finish(status, error, now),
        Plan::Labels {
            add,
            remove,
            status,
        } => {
            let id = thread_id.context("a label change needs a thread")?;
            let request = gmail::ModifyThreadRequest {
                add_label_ids: add,
                remove_label_ids: remove,
            };
            match gmail.gmail().modify_thread(&id, &request).await {
                Ok(_) => job.finish(status, None, now),
                Err(error) if not_found(&error) => {
                    job.finish(Status::Done, Some("the thread is gone".into()), now);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Plan::Send => {
            let Work::Send {
                draft_id,
                rfc822_id,
                ..
            } = &job.work
            else {
                unreachable!("only a send plans to send");
            };
            let sent = match gmail.gmail().send_draft(draft_id).await {
                Ok(message) => message.id,
                // The draft is gone: sent from elsewhere, an attempt whose answer was lost, or
                // deleted. Its message tells which.
                Err(error) if not_found(&error) => sent_message(gmail, rfc822_id).await?,
                Err(error) => return Err(error.into()),
            };
            match sent {
                Some(id) => {
                    job.sent_message_id = Some(id);
                    job.finish(Status::Done, None, now);
                }
                None => job.finish(Status::Failed, Some("the draft is gone".into()), now),
            }
        }
    }
    Ok(())
}

/// The thread's messages and their labels, or `None` when it's gone.
async fn thread(gmail: &gmail::Client, id: &str) -> Result<Option<Vec<Seen>>> {
    match gmail.gmail().thread(id, gmail::Format::Minimal, &[]).await {
        Ok(thread) => Ok(Some(
            thread
                .messages
                .into_iter()
                .filter_map(|message| {
                    Some(Seen {
                        id: message.id?,
                        labels: message.label_ids,
                    })
                })
                .collect(),
        )),
        Err(error) if not_found(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The id of the sent message whose `Message-ID` is `rfc822_id`, if there's one.
async fn sent_message(gmail: &gmail::Client, rfc822_id: &str) -> Result<Option<String>> {
    let query = gmail::ListQuery {
        q: Some(format!("rfc822msgid:{rfc822_id}")),
        label_ids: vec![SENT.to_owned()],
        max_results: Some(1),
        ..gmail::ListQuery::default()
    };
    let found = gmail.gmail().messages(&query).await?;
    Ok(found.messages.into_iter().find_map(|message| message.id))
}

fn not_found(error: &gmail::Error) -> bool {
    matches!(error, gmail::Error::Api(api) if api.code == 404)
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::{FINISHED, Job, Plan, Request, Seen, Status, Work, plan, stamp, valid_key};

    fn remind(if_no_reply: bool) -> Job {
        Job::new(
            "sub",
            "remind-t1",
            Request {
                work: Work::Remind {
                    thread_id: "t1".into(),
                    snoozed_label: "Label_snoozed".into(),
                    reminded_label: "Label_ru".into(),
                },
                at: "2026-11-23T09:00:00Z".parse().unwrap(),
                if_no_reply,
                message_ids: vec!["m1".into()],
            },
        )
    }

    fn send(if_no_reply: bool) -> Job {
        Job::new(
            "sub",
            "send-d1",
            Request {
                work: Work::Send {
                    thread_id: Some("t1".into()),
                    draft_id: "r-1".into(),
                    rfc822_id: "<x@example.com>".into(),
                },
                at: "2026-11-23T09:00:00Z".parse().unwrap(),
                if_no_reply,
                message_ids: vec!["m1".into()],
            },
        )
    }

    fn seen(id: &str, labels: &[&str]) -> Seen {
        Seen {
            id: id.into(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        }
    }

    fn now() -> Timestamp {
        "2026-11-23T09:00:30Z".parse().unwrap()
    }

    #[test]
    fn a_reminder_brings_its_thread_back_to_the_inbox() {
        let thread = [seen("m1", &["Label_snoozed"])];
        assert_eq!(
            plan(&remind(false), Some(&thread), now()),
            Plan::Labels {
                add: vec!["INBOX".into(), "Label_ru".into()],
                remove: vec!["Label_snoozed".into()],
                status: Status::Done,
            }
        );
    }

    #[test]
    fn a_reply_calls_off_an_if_no_reply_reminder_but_not_the_users_own_messages() {
        let mine = [seen("m1", &[]), seen("m2", &["SENT"]), seen("m3", &["DRAFT"])];
        assert!(matches!(
            plan(&remind(true), Some(&mine), now()),
            Plan::Labels { status: Status::Done, .. }
        ));
        let replied = [seen("m1", &[]), seen("m4", &["INBOX"])];
        assert_eq!(
            plan(&remind(true), Some(&replied), now()),
            Plan::Labels {
                add: Vec::new(),
                remove: vec!["Label_snoozed".into()],
                status: Status::Aborted,
            }
        );
        // Without "if no reply", a reply changes nothing.
        assert!(matches!(
            plan(&remind(false), Some(&replied), now()),
            Plan::Labels { status: Status::Done, .. }
        ));
    }

    #[test]
    fn a_thread_in_the_trash_only_loses_its_snoozed_label() {
        let thread = [seen("m1", &["TRASH"])];
        assert_eq!(
            plan(&remind(false), Some(&thread), now()),
            Plan::Labels {
                add: Vec::new(),
                remove: vec!["Label_snoozed".into()],
                status: Status::Done,
            }
        );
    }

    #[test]
    fn a_send_goes_unless_replied_to_or_a_day_late() {
        let quiet = [seen("m1", &[])];
        assert_eq!(plan(&send(true), Some(&quiet), now()), Plan::Send);
        let replied = [seen("m1", &[]), seen("m2", &["INBOX"])];
        assert_eq!(
            plan(&send(true), Some(&replied), now()),
            Plan::Finish(Status::Aborted, None)
        );
        let late = "2026-11-24T09:00:01Z".parse().unwrap();
        assert!(matches!(
            plan(&send(false), Some(&[]), late),
            Plan::Finish(Status::Failed, Some(_))
        ));
    }

    #[test]
    fn times_sort_as_strings_and_finished_sorts_last() {
        let early = stamp("2026-11-23T09:00:00.5Z".parse().unwrap());
        let later = stamp("2026-11-23T09:00:01Z".parse().unwrap());
        assert_eq!(early, "2026-11-23T09:00:00Z");
        assert!(early < later && later.as_str() < FINISHED);
    }

    #[test]
    fn keys_are_short_and_safe() {
        assert!(valid_key("remind:18c2f0a1b2"));
        assert!(!valid_key(""));
        assert!(!valid_key("a/b"));
        assert!(!valid_key(&"x".repeat(201)));
    }
}
