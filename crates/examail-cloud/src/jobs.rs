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
/// How many jobs an account may have waiting, so one can't fill the queue.
pub const MOST_PENDING: usize = 1_000;
/// How far ahead a job may be set.
const FURTHEST: SignedDuration = SignedDuration::from_hours(5 * 366 * 24);

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
        /// The Gmail message the draft held when the app last wrote it. A draft holding another
        /// (edited since, in Examail or in Gmail web) isn't sent: the job fails, and the app,
        /// which knows the new version, sends the job again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// The message the app scheduled, once a tick has dated the draft afresh (a new message,
        /// now `message_id`). The app resending the job with this one is resending the same job.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dated_from: Option<String>,
    },
}

impl Work {
    /// Whether `self`, as the app sends it, is `kept` as a tick has left it: the same, but for
    /// the date a tick gave the draft.
    pub fn same_as_kept(&self, kept: &Work) -> bool {
        match (self, kept) {
            (
                Work::Send {
                    thread_id,
                    draft_id,
                    rfc822_id,
                    message_id: Some(scheduled),
                    dated_from: None,
                },
                Work::Send {
                    thread_id: kept_thread,
                    draft_id: kept_draft,
                    rfc822_id: kept_rfc822,
                    dated_from: Some(from),
                    ..
                },
            ) => {
                thread_id == kept_thread
                    && draft_id == kept_draft
                    && rfc822_id == kept_rfc822
                    && scheduled == from
            }
            _ => self == kept,
        }
    }
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

/// Why `request` can't be a job at `now`, if it can't: due more than a day ago (the app fires
/// those itself), or further ahead than [`FURTHEST`].
pub fn refuse(request: &Request, now: Timestamp) -> Option<&'static str> {
    if now.duration_since(request.at) > SignedDuration::from_hours(24) {
        Some("it was due more than a day ago")
    } else if request.at.duration_since(now) > FURTHEST {
        Some("it's more than five years away")
    } else {
        None
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
pub(crate) fn replied(job: &Job, thread: &[Seen]) -> bool {
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
            if !has(snoozed_label) {
                // Ended elsewhere: the reminder taken off in Gmail web or on a phone, or Examail
                // brought the thread back itself. Nothing to do.
                Plan::Finish(Status::Done, Some("the thread is no longer snoozed".into()))
            } else if replied {
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

/// Fires the jobs that are due. Returns how many it finished. One job's or account's trouble is
/// logged and left for a later tick, never stopping the others.
pub async fn tick(app: &App) -> Result<usize> {
    let now = Timestamp::now();
    let due = app
        .store
        .db()
        .at_most(
            COLLECTION,
            "due",
            &serde_json::Value::from(stamp(now)),
            BATCH,
        )
        .await?;
    let mut finished = 0;
    // Each account's Gmail, made once a tick; `None` for one that can't be acted for now.
    let mut accounts: HashMap<String, Option<gmail::Client>> = HashMap::new();
    for document in due {
        match run(app, &document, now, &mut accounts).await {
            Ok(true) => finished += 1,
            Ok(false) => {}
            Err(error) => log::warn!("job {}: {error:#}", document.id),
        }
    }
    Ok(finished)
}

/// Claims and fires one due job. `true` when it finished.
async fn run(
    app: &App,
    document: &Document,
    now: Timestamp,
    accounts: &mut HashMap<String, Option<gmail::Client>>,
) -> Result<bool> {
    let Some(mut job) = claim(app, document, now).await? else {
        return Ok(false);
    };
    if job.status != Status::Pending {
        return Ok(true);
    }
    let gmail = match accounts.get(&job.sub) {
        Some(gmail) => gmail.clone(),
        None => {
            // An account whose grant can't be read or refreshed now waits out this tick, with
            // its other jobs, rather than failing them.
            let gmail = match account_gmail(app, &job.sub).await {
                Ok(Some(gmail)) => Some(gmail),
                Ok(None) => {
                    job.finish(
                        Status::Failed,
                        Some("the account isn't connected".into()),
                        now,
                    );
                    app.store.put_job(&document.id, &job).await?;
                    return Ok(true);
                }
                Err(error) => {
                    accounts.insert(job.sub.clone(), None);
                    return Err(error.context("acting for the account"));
                }
            };
            accounts.insert(job.sub.clone(), gmail.clone());
            gmail
        }
    };
    let Some(gmail) = gmail else {
        return Ok(false);
    };
    // The lease runs out and a later tick tries again. What the attempt learned (the draft's
    // new message, once dated) is kept for it.
    let before = job.work.clone();
    if let Err(error) = fire(&gmail, &mut job, now).await {
        if job.work != before {
            app.store.put_job(&document.id, &job).await?;
        }
        return Err(error.context(format!("attempt {}", job.attempts)));
    }
    app.store.put_job(&document.id, &job).await?;
    log::info!("job {} {:?}", document.id, job.status);
    Ok(true)
}

/// Takes `document` for this tick: its lease moves ahead and its attempts go up, or it's failed
/// once it has run out of attempts. `None` when another tick took it first.
async fn claim(app: &App, document: &Document, now: Timestamp) -> Result<Option<Job>> {
    let Ok(mut job) = serde_json::from_value::<Job>(document.fields.clone()) else {
        // A job that doesn't read would be found first by every tick: take it off the queue.
        log::error!("job {} doesn't read; taking it off the queue", document.id);
        let mut fields = document.fields.clone();
        fields["due"] = FINISHED.into();
        fields["status"] = "failed".into();
        app.store
            .db()
            .set_if_unchanged(COLLECTION, &document.id, &fields, &document.update_time)
            .await?;
        return Ok(None);
    };
    if job.attempts >= ATTEMPTS {
        job.finish(
            Status::Failed,
            Some(format!("{ATTEMPTS} attempts failed")),
            now,
        );
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
pub(crate) async fn account_gmail(app: &App, sub: &str) -> Result<Option<gmail::Client>> {
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
                message_id,
                dated_from,
                ..
            } = &mut job.work
            else {
                unreachable!("only a send plans to send");
            };
            let (draft_id, rfc822_id) = (draft_id.clone(), rfc822_id.clone());
            let sent = match ready(gmail, &draft_id, message_id.as_deref(), now).await? {
                Ready::Changed => {
                    job.finish(
                        Status::Failed,
                        Some("the draft changed after it was scheduled".into()),
                        now,
                    );
                    return Ok(());
                }
                Ready::Gone => sent_message(gmail, &rfc822_id).await?,
                Ready::Yes(dated) => {
                    // Dating the draft made it a new message: later attempts, and the app's
                    // copy of the job, are checked against that one.
                    if let Some(dated) = dated {
                        if dated_from.is_none() {
                            *dated_from = message_id.clone();
                        }
                        *message_id = Some(dated);
                    }
                    match gmail.gmail().send_draft(&draft_id).await {
                        Ok(message) => message.id,
                        // The draft is gone: sent from elsewhere, an attempt whose answer was
                        // lost, or deleted. Its message tells which.
                        Err(error) if not_found(&error) => sent_message(gmail, &rfc822_id).await?,
                        Err(error) => return Err(error.into()),
                    }
                }
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

/// Whether a scheduled draft can go as it is.
enum Ready {
    /// Yes, dated now: the new message dating made, if it made one.
    Yes(Option<String>),
    /// It holds another message than the one scheduled.
    Changed,
    /// It's gone.
    Gone,
}

/// Readies draft `draft_id` to go at `now`: it must still hold `message_id` (when that's known),
/// and its `Date:` becomes now, because Gmail sends a draft's date as written and recipients see
/// it. Dating it is a new version of the draft, so it's checked first.
async fn ready(
    gmail: &gmail::Client,
    draft_id: &str,
    message_id: Option<&str>,
    now: Timestamp,
) -> Result<Ready> {
    let draft = match gmail.gmail().draft(draft_id, gmail::Format::Raw).await {
        Ok(draft) => draft,
        Err(error) if not_found(&error) => return Ok(Ready::Gone),
        Err(error) => return Err(error.into()),
    };
    let Some(message) = draft.message else {
        return Ok(Ready::Gone);
    };
    if message_id.is_some_and(|scheduled| message.id.as_deref() != Some(scheduled)) {
        return Ok(Ready::Changed);
    }
    let Some(raw) = message.raw else {
        return Ok(Ready::Yes(None));
    };
    let dated = redate(&raw, now);
    match gmail
        .gmail()
        .update_draft(draft_id, dated, message.thread_id.as_deref())
        .await
    {
        Ok(draft) => Ok(Ready::Yes(draft.message.and_then(|message| message.id))),
        Err(error) if not_found(&error) => Ok(Ready::Gone),
        Err(error) => Err(error.into()),
    }
}

/// `raw` with its `Date:` header (and any folded lines of it) replaced by one for `now`. Only the
/// header, the lines before the first empty one, is rewritten; the empty line and everything after
/// it are kept byte for byte. A message without an empty line is returned as it is, undated,
/// rather than guessed at.
pub fn redate(raw: &[u8], now: Timestamp) -> Vec<u8> {
    // The header's lines, each with its own line ending, up to the empty line.
    let mut header = Vec::new();
    let mut at = 0;
    let body_at = loop {
        let Some(end) = raw[at..].iter().position(|byte| *byte == b'\n') else {
            return raw.to_vec();
        };
        let line = &raw[at..at + end + 1];
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if text.is_empty() {
            break at;
        }
        header.push((text, line));
        at += end + 1;
    };
    let ending: &[u8] = if header
        .first()
        .is_some_and(|(_, line)| line.ends_with(b"\r\n"))
    {
        b"\r\n"
    } else {
        b"\n"
    };
    let date =
        jiff::fmt::rfc2822::to_string(&now.to_zoned(jiff::tz::TimeZone::UTC)).unwrap_or_default();
    let mut out = format!("Date: {date}").into_bytes();
    out.extend_from_slice(ending);
    let mut in_date = false;
    for (text, line) in header {
        let folded = text
            .first()
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t');
        if folded && in_date {
            continue;
        }
        in_date = !folded && text.len() >= 5 && text[..5].eq_ignore_ascii_case(b"date:");
        if !in_date {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(&raw[body_at..]);
    out
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

pub(crate) fn not_found(error: &gmail::Error) -> bool {
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
                    message_id: Some("m9".into()),
                    dated_from: None,
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
        let mine = [
            seen("m1", &["Label_snoozed"]),
            seen("m2", &["SENT"]),
            seen("m3", &["DRAFT"]),
        ];
        assert!(matches!(
            plan(&remind(true), Some(&mine), now()),
            Plan::Labels {
                status: Status::Done,
                ..
            }
        ));
        let replied = [seen("m1", &["Label_snoozed"]), seen("m4", &["INBOX"])];
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
            Plan::Labels {
                status: Status::Done,
                ..
            }
        ));
    }

    #[test]
    fn a_reminder_ended_elsewhere_changes_nothing() {
        let thread = [seen("m1", &["INBOX"])];
        assert!(matches!(
            plan(&remind(false), Some(&thread), now()),
            Plan::Finish(Status::Done, Some(_))
        ));
    }

    #[test]
    fn a_thread_in_the_trash_only_loses_its_snoozed_label() {
        let thread = [seen("m1", &["TRASH", "Label_snoozed"])];
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
    fn jobs_too_old_or_too_far_off_are_refused() {
        let request = |at: &str| Request {
            work: Work::Remind {
                thread_id: "t1".into(),
                snoozed_label: "s".into(),
                reminded_label: "r".into(),
            },
            at: at.parse().unwrap(),
            if_no_reply: false,
            message_ids: Vec::new(),
        };
        let now = now();
        assert_eq!(super::refuse(&request("2026-11-23T08:00:00Z"), now), None);
        assert!(super::refuse(&request("2026-11-21T09:00:00Z"), now).is_some());
        assert!(super::refuse(&request("2032-11-23T09:00:00Z"), now).is_some());
    }

    #[test]
    fn the_app_resending_a_dated_send_resends_the_same_job() {
        let send = |message: &str, from: Option<&str>| Work::Send {
            thread_id: Some("t1".into()),
            draft_id: "r-1".into(),
            rfc822_id: "x@example.com".into(),
            message_id: Some(message.into()),
            dated_from: from.map(str::to_owned),
        };
        let kept = send("m10", Some("m9"));
        assert!(send("m9", None).same_as_kept(&kept));
        assert!(!send("m11", None).same_as_kept(&kept));
        assert!(send("m9", None).same_as_kept(&send("m9", None)));
    }

    #[test]
    fn a_send_is_dated_when_it_goes() {
        let raw = b"From: a@example.com\r\nDate: Mon, 2 Nov 2026 10:00:00 -0800\r\n (folded)\r\nSubject: Hi\r\n\r\nBody\r\nDate: in the body\r\n";
        let dated = String::from_utf8(super::redate(raw, now())).unwrap();
        assert_eq!(
            dated,
            "Date: Mon, 23 Nov 2026 09:00:30 +0000\r\nFrom: a@example.com\r\nSubject: Hi\r\n\r\nBody\r\nDate: in the body\r\n"
        );
        // A message without a date gets one.
        let undated = String::from_utf8(super::redate(b"Subject: Hi\r\n\r\nBody", now())).unwrap();
        assert_eq!(
            undated,
            "Date: Mon, 23 Nov 2026 09:00:30 +0000\r\nSubject: Hi\r\n\r\nBody"
        );
        // Bare newlines: the first empty line still ends the header, and the body is kept.
        let bare = String::from_utf8(super::redate(
            b"Date: x\nSubject: Hi\n\nBody\r\n\r\nDate: in the body\n",
            now(),
        ))
        .unwrap();
        assert_eq!(
            bare,
            "Date: Mon, 23 Nov 2026 09:00:30 +0000\nSubject: Hi\n\nBody\r\n\r\nDate: in the body\n"
        );
        // Without an empty line it goes as it is.
        assert_eq!(
            super::redate(b"Subject: Hi\nBody", now()),
            b"Subject: Hi\nBody"
        );
    }

    #[test]
    fn keys_are_short_and_safe() {
        assert!(valid_key("remind:18c2f0a1b2"));
        assert!(!valid_key(""));
        assert!(!valid_key("a/b"));
        assert!(!valid_key(&"x".repeat(201)));
    }
}
