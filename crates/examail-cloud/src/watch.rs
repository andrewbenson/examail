//! The mail watcher: what the service does as mail arrives while every Mac is closed.
//!
//! Gmail publishes each watched mailbox's INBOX changes to a Pub/Sub topic (`users.watch`, which
//! lapses after a week, so the tick renews it), and Pub/Sub pushes them to the worker's
//! `/hooks/gmail`. A push only says the mailbox moved on: the service reads what changed since the
//! history position it last kept, and the tick does the same every minute, so a lost push only
//! delays things.
//!
//! For each message newly in the inbox that isn't the user's own:
//!
//! - A reply on a thread the user muted goes back out of the inbox (`[Exo]/Muted` added, INBOX
//!   taken off), as Examail hides it, unless it added one of the user's addresses as a new
//!   recipient.
//! - A reminder or scheduled send that a reply calls off ends now rather than when due.
//! - With an ntfy topic set in the settings, the phone hears of it, as the Mac would.
//! - Split labels ([`crate::splits`]) are put on it once Gmail's search has it.

use std::collections::{BTreeSet, HashSet};

use anyhow::{Context, Result};
use base64::Engine as _;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::firestore::Precondition;
use crate::jobs::{self, Job, Seen, Status, Work};
use crate::settings::Context as SettingsContext;

pub const COLLECTION: &str = "watches";
/// The label a muted thread carries ([`exo_store`'s `EXO_MUTED`]).
pub const MUTED: &str = "[Exo]/Muted";
/// The one ntfy server phone notifications go to.
const NTFY: &str = "https://ntfy.sh";
/// How long one run may hold an account's watch before another may take it over.
const LEASE: SignedDuration = SignedDuration::from_mins(2);
/// A watch is renewed once it has less than this left.
const RENEW_WITHIN: SignedDuration = SignedDuration::from_hours(24);
/// The most arrivals one run acts on; the rest wait for the next.
const MOST: usize = 200;

/// An account's Gmail watch, and how far its history has been read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Watch {
    /// The mailbox's address, lowercased, which a push names it by.
    pub email: String,
    /// The history position read up to, as a decimal string.
    pub history_id: String,
    /// When Gmail stops publishing unless the watch is renewed.
    pub expires_at: String,
    /// Until when a run holds it.
    pub lease_until: String,
}

/// One message newly in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub id: String,
    pub thread_id: String,
}

/// What a Pub/Sub push from Gmail says: whose mailbox moved, and to where.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Notice {
    email_address: String,
}

/// Reads the mailbox a Pub/Sub push names. `None` when it doesn't read.
pub fn pushed_email(body: &[u8]) -> Option<String> {
    let push: Value = serde_json::from_slice(body).ok()?;
    let data = push["message"]["data"].as_str()?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let notice: Notice = serde_json::from_slice(&data).ok()?;
    Some(notice.email_address.to_lowercase())
}

/// A push came for `email`: read what changed.
pub async fn pushed(app: &App, email: &str) -> Result<()> {
    let found: Vec<(String, Watch)> = app
        .store
        .db()
        .where_eq(COLLECTION, "email", &Value::from(email))
        .await?;
    for (sub, _) in found {
        catch_up(app, &sub).await?;
    }
    Ok(())
}

/// The tick's part: starts or renews each connected account's watch, and reads what changed for
/// each, in case a push was lost.
pub async fn sweep(app: &App) -> Result<()> {
    let Some(topic) = app.config.gmail_topic.clone() else {
        return Ok(());
    };
    let accounts: Vec<(String, crate::store::Account)> = app
        .store
        .db()
        .where_eq("accounts", "status", &Value::from("active"))
        .await?;
    let now = Timestamp::now();
    for (sub, account) in accounts {
        let watch: Option<Watch> = app.store.db().get(COLLECTION, &sub).await?;
        let renew = watch.as_ref().is_none_or(|watch| {
            watch
                .expires_at
                .parse::<Timestamp>()
                .map_or(true, |at| at.duration_since(now) < RENEW_WITHIN)
        });
        if renew && let Err(error) = start(app, &sub, &account.email, &topic, watch).await {
            log::warn!("watching {sub}'s mailbox: {error:#}");
            continue;
        }
        if let Err(error) = catch_up(app, &sub).await {
            log::warn!("reading {sub}'s mail: {error:#}");
        }
    }
    Ok(())
}

/// Asks Gmail to publish `sub`'s inbox changes, keeping how far its history was read (or
/// starting from now, for a new watch).
async fn start(app: &App, sub: &str, email: &str, topic: &str, watch: Option<Watch>) -> Result<()> {
    let Some(gmail) = jobs::account_gmail(app, sub).await? else {
        return Ok(());
    };
    let request = gmail::WatchRequest {
        topic_name: topic.to_owned(),
        label_ids: vec!["INBOX".into()],
        label_filter_behavior: Some("include".into()),
    };
    let answer = gmail.gmail().watch(&request).await?;
    let expires_at = answer
        .expiration
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .unwrap_or_else(|| Timestamp::now() + SignedDuration::from_hours(24 * 7));
    let mut watch = watch.unwrap_or_default();
    watch.email = email.to_lowercase();
    watch.expires_at = jobs::stamp(expires_at);
    if watch.history_id.is_empty() {
        watch.history_id = answer.history_id.unwrap_or_default().to_string();
    }
    app.store
        .db()
        .set(COLLECTION, sub, &watch, Precondition::None)
        .await?;
    log::info!("watching {sub}'s inbox until {}", watch.expires_at);
    Ok(())
}

/// Reads `sub`'s inbox arrivals since its watch's history position and acts on them, unless
/// another run holds the watch.
pub async fn catch_up(app: &App, sub: &str) -> Result<()> {
    let Some(document) = app.store.db().get_document(COLLECTION, sub).await? else {
        return Ok(());
    };
    let mut watch: Watch = serde_json::from_value(document.fields.clone())?;
    let now = Timestamp::now();
    if watch
        .lease_until
        .parse::<Timestamp>()
        .is_ok_and(|until| until > now)
    {
        return Ok(());
    }
    let Ok(start) = watch.history_id.parse::<u64>() else {
        return Ok(());
    };
    watch.lease_until = jobs::stamp(now + LEASE);
    if !app
        .store
        .db()
        .set_if_unchanged(COLLECTION, sub, &watch, &document.update_time)
        .await?
    {
        return Ok(());
    }
    let result = read_and_act(app, sub, start).await;
    let next = match &result {
        Ok(Some(next)) => *next,
        _ => start,
    };
    watch.history_id = next.to_string();
    watch.lease_until = String::new();
    // Only if the watch is still there: a disconnect meanwhile deleted it, and it stays deleted.
    match app
        .store
        .db()
        .set(COLLECTION, sub, &watch, Precondition::Present)
        .await
    {
        Ok(()) | Err(crate::firestore::Error::Gone) => {}
        Err(crate::firestore::Error::Exists) => {}
        Err(crate::firestore::Error::Other(error)) => return Err(error),
    }
    result.map(|_| ())
}

/// Reads the arrivals after `start` and acts on them. The history position to keep, or `None`
/// when the account can't be acted for now.
async fn read_and_act(app: &App, sub: &str, start: u64) -> Result<Option<u64>> {
    let Some(gmail) = jobs::account_gmail(app, sub).await? else {
        return Ok(None);
    };
    let (arrivals, next) = match arrivals(&gmail, start).await {
        Ok(read) => read,
        // Too far back: Gmail no longer has it. Start over from now, acting on nothing missed,
        // which the Macs catch up on.
        Err(error)
            if error
                .downcast_ref::<gmail::Error>()
                .is_some_and(jobs::not_found) =>
        {
            let profile = gmail.gmail().profile().await?;
            return Ok(profile.history_id);
        }
        Err(error) => return Err(error),
    };
    if arrivals.is_empty() {
        return Ok(Some(next));
    }
    let (settings, context) = crate::settings::load(app, sub)
        .await?
        .unwrap_or_else(|| (Value::Null, SettingsContext::default()));
    let pending: Vec<Job> = app
        .store
        .jobs(sub)
        .await?
        .into_iter()
        .filter(|job| job.status == Status::Pending && job.if_no_reply)
        .collect();
    for arrival in &arrivals {
        if let Err(error) = act(app, sub, &gmail, &settings, &context, &pending, arrival).await {
            log::warn!("acting on {sub}'s message {}: {error:#}", arrival.id);
        }
    }
    Ok(Some(next))
}

/// The messages added to the inbox after `start`, oldest first, at most [`MOST`], and the
/// history position after them.
async fn arrivals(gmail: &gmail::Client, start: u64) -> Result<(Vec<Arrival>, u64)> {
    let mut query = gmail::HistoryQuery {
        start_history_id: start,
        history_types: vec![gmail::HistoryType::MessageAdded],
        label_id: Some("INBOX".into()),
        max_results: Some(500),
        page_token: None,
    };
    let mut arrivals = Vec::new();
    let mut seen = HashSet::new();
    let mut next = start;
    loop {
        let page = gmail.gmail().history(&query).await?;
        for record in page.history {
            for added in record.messages_added {
                let (Some(id), Some(thread_id)) = (added.message.id, added.message.thread_id)
                else {
                    continue;
                };
                if arrivals.len() < MOST && seen.insert(id.clone()) {
                    arrivals.push(Arrival { id, thread_id });
                }
            }
            if let Some(id) = record.id
                && arrivals.len() < MOST
            {
                next = next.max(id);
            }
        }
        if arrivals.len() >= MOST {
            break;
        }
        if let Some(history_id) = page.history_id {
            next = next.max(history_id);
        }
        match page.next_page_token {
            Some(token) => query.page_token = Some(token),
            None => break,
        }
    }
    Ok((arrivals, next))
}

/// What the service does for one arrival.
async fn act(
    app: &App,
    sub: &str,
    gmail: &gmail::Client,
    settings: &Value,
    context: &SettingsContext,
    pending: &[Job],
    arrival: &Arrival,
) -> Result<()> {
    let thread = match gmail
        .gmail()
        .thread(
            &arrival.thread_id,
            gmail::Format::Metadata,
            &["From", "To", "Cc", "Subject"],
        )
        .await
    {
        Ok(thread) => thread,
        Err(error) if jobs::not_found(&error) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let Some(message) = thread
        .messages
        .iter()
        .find(|message| message.id.as_deref() == Some(arrival.id.as_str()))
    else {
        return Ok(());
    };
    let labels = &message.label_ids;
    let has = |label: &str| labels.iter().any(|have| have == label);
    if !has("INBOX") || has("SENT") || has("DRAFT") {
        return Ok(());
    }
    let me: Vec<String> = context.addresses.clone();
    let from = message.header("From").map(addresses).unwrap_or_default();
    if from.iter().any(|address| me.contains(address)) {
        return Ok(());
    }

    // A reply on a muted thread goes back out of the inbox.
    let muted = context
        .labels
        .iter()
        .find(|(_, name)| name.as_str() == MUTED)
        .map(|(id, _)| id.clone());
    if let Some(muted) = &muted
        && is_muted(&thread.messages, &arrival.id, muted, &me)
    {
        let request = gmail::ModifyMessageRequest {
            add_label_ids: vec![muted.clone()],
            remove_label_ids: vec!["INBOX".into()],
            ..gmail::ModifyMessageRequest::default()
        };
        gmail.gmail().modify_message(&arrival.id, &request).await?;
        log::info!(
            "kept {sub}'s muted thread {} out of the inbox",
            arrival.thread_id
        );
        return Ok(());
    }

    // A reply calls off what waits for none.
    let seen: Vec<Seen> = thread
        .messages
        .iter()
        .filter_map(|message| {
            Some(Seen {
                id: message.id.clone()?,
                labels: message.label_ids.clone(),
            })
        })
        .collect();
    for job in pending
        .iter()
        .filter(|job| on_thread(job, &arrival.thread_id))
    {
        if jobs::replied(job, &seen) {
            let mut job = job.clone();
            job.due = jobs::stamp(Timestamp::now());
            app.store
                .put_job(&jobs::document_id(sub, &job.key), &job)
                .await?;
            log::info!("a reply ends {sub}'s job {} early", job.key);
        }
    }

    // The phone hears of it.
    if let Some(ntfy) = Ntfy::from_settings(settings)
        && notifies(settings, labels)
    {
        let sender = message.header("From").map(display_name).unwrap_or_default();
        let subject = message.header("Subject").unwrap_or_default();
        if let Err(error) = ntfy.post(&app.http, &sender, subject).await {
            log::warn!("notifying {sub}'s phone: {error:#}");
        }
    }

    // Split labels, once Gmail's search has the message.
    crate::splits::queue(app, sub, arrival).await?;
    Ok(())
}

fn on_thread(job: &Job, thread_id: &str) -> bool {
    match &job.work {
        Work::Remind { thread_id: id, .. } => id == thread_id,
        Work::Send { thread_id: id, .. } => id.as_deref() == Some(thread_id),
    }
}

/// Whether `arrival` came on a muted thread: an earlier message that isn't a draft has the
/// muted label, and `arrival` didn't add one of the user's addresses as a new recipient
/// (as Examail's own `ThreadView::is_muted` decides).
pub fn is_muted(messages: &[gmail::Message], arrival: &str, muted: &str, me: &[String]) -> bool {
    let earlier = messages
        .iter()
        .filter(|message| message.id.as_deref() != Some(arrival))
        .filter(|message| !message.label_ids.iter().any(|label| label == "DRAFT"));
    let mut was_muted = false;
    let mut recipients: BTreeSet<String> = BTreeSet::new();
    for message in earlier {
        was_muted |= message.label_ids.iter().any(|label| label == muted);
        recipients.extend(recipients_of(message));
    }
    if !was_muted {
        return false;
    }
    let Some(message) = messages
        .iter()
        .find(|message| message.id.as_deref() == Some(arrival))
    else {
        return false;
    };
    !recipients_of(message)
        .iter()
        .any(|address| me.contains(address) && !recipients.contains(address))
}

fn recipients_of(message: &gmail::Message) -> Vec<String> {
    ["To", "Cc"]
        .iter()
        .filter_map(|name| message.header(name))
        .flat_map(addresses)
        .collect()
}

/// The addresses in an address header, lowercased.
pub fn addresses(header: &str) -> Vec<String> {
    header
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>' | '"'))
        .filter(|token| {
            let at = token.find('@');
            at.is_some_and(|at| at > 0 && at + 1 < token.len())
        })
        .map(|token| token.trim_matches(|c| c == '(' || c == ')').to_lowercase())
        .collect()
}

/// A `From` header's name, else its address.
fn display_name(header: &str) -> String {
    match header.find('<') {
        Some(at) if at > 0 => header[..at].trim().trim_matches('"').trim().to_owned(),
        _ => header
            .trim()
            .trim_matches(|c| c == '<' || c == '>')
            .to_owned(),
    }
}

/// Whether the Mac would notify of a message with `labels`: notifications on, unread, and in
/// Important unless the account has one Inbox (`crates/exo/src/notifications/mail.rs`).
pub fn notifies(settings: &Value, labels: &[String]) -> bool {
    let has = |label: &str| labels.iter().any(|have| have == label);
    if settings["disableNotifications"] == true || !has("UNREAD") {
        return false;
    }
    settings["unifiedInbox"] == true || (has("CATEGORY_PERSONAL") && !has("CATEGORY_SOCIAL"))
}

/// Where the account's phone notifications go: a topic on ntfy.sh, from the settings' `ntfy`
/// (`{"topic": "…"}`; a `server` there is ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ntfy {
    pub server: String,
    pub topic: String,
}

impl Ntfy {
    pub fn from_settings(settings: &Value) -> Option<Self> {
        let ntfy = &settings["ntfy"];
        let topic = ntfy["topic"].as_str()?.trim();
        if topic.is_empty()
            || !topic
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        // Only ntfy.sh: the service posts nowhere a setting could point it.
        Some(Self {
            server: NTFY.to_owned(),
            topic: topic.to_owned(),
        })
    }

    async fn post(&self, http: &reqwest::Client, sender: &str, subject: &str) -> Result<()> {
        let body = json!({
            "topic": self.topic,
            "title": sender,
            "message": if subject.is_empty() { "(no subject)" } else { subject },
            "tags": ["email"],
        });
        http.post(&self.server)
            .json(&body)
            .send()
            .await
            .context("reaching ntfy")?
            .error_for_status()
            .context("ntfy refused it")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, labels: &[&str], to: &str) -> gmail::Message {
        gmail::Message {
            id: Some(id.into()),
            label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
            payload: Some(gmail::MessagePart {
                headers: vec![gmail::MessagePartHeader {
                    name: "To".into(),
                    value: to.into(),
                }],
                ..gmail::MessagePart::default()
            }),
            ..gmail::Message::default()
        }
    }

    #[test]
    fn a_push_names_its_mailbox() {
        let data = base64::engine::general_purpose::STANDARD
            .encode(br#"{"emailAddress":"Me@Example.com","historyId":"42"}"#);
        let body = format!(r#"{{"message":{{"data":"{data}"}}}}"#);
        assert_eq!(
            pushed_email(body.as_bytes()).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(pushed_email(b"{}"), None);
    }

    #[test]
    fn addresses_come_out_of_headers() {
        assert_eq!(
            addresses(r#""Okeke, Maria" <Maria@Example.com>, jim@example.org"#),
            ["maria@example.com", "jim@example.org"]
        );
        assert_eq!(
            display_name(r#""Maria Okeke" <maria@example.com>"#),
            "Maria Okeke"
        );
        assert_eq!(display_name("<maria@example.com>"), "maria@example.com");
    }

    #[test]
    fn a_reply_on_a_muted_thread_stays_muted_unless_it_adds_me() {
        let me = vec!["me@example.com".to_owned()];
        let thread = vec![
            message("1", &["Label_9"], "list@example.com"),
            message("2", &["INBOX", "UNREAD"], "list@example.com"),
        ];
        assert!(is_muted(&thread, "2", "Label_9", &me));
        let thread = vec![
            message("1", &["Label_9"], "list@example.com"),
            message("2", &["INBOX"], "list@example.com, me@example.com"),
        ];
        assert!(!is_muted(&thread, "2", "Label_9", &me), "it added me");
        let thread = vec![
            message("1", &[], "list@example.com"),
            message("2", &["INBOX"], "list@example.com"),
        ];
        assert!(!is_muted(&thread, "2", "Label_9", &me), "never muted");
    }

    #[test]
    fn notifications_follow_the_macs_rules() {
        let personal = ["UNREAD".to_owned(), "CATEGORY_PERSONAL".to_owned()];
        let promotions = ["UNREAD".to_owned(), "CATEGORY_PROMOTIONS".to_owned()];
        assert!(notifies(&json!({}), &personal));
        assert!(!notifies(&json!({}), &promotions));
        assert!(notifies(&json!({ "unifiedInbox": true }), &promotions));
        assert!(!notifies(
            &json!({ "disableNotifications": true }),
            &personal
        ));
        assert!(
            !notifies(&json!({}), &["CATEGORY_PERSONAL".to_owned()]),
            "read"
        );
    }

    #[test]
    fn ntfy_needs_a_plain_topic_and_an_https_server() {
        assert_eq!(Ntfy::from_settings(&json!({})), None);
        assert_eq!(
            Ntfy::from_settings(&json!({ "ntfy": { "topic": "abc-123" } })),
            Some(Ntfy {
                server: "https://ntfy.sh".into(),
                topic: "abc-123".into()
            })
        );
        assert_eq!(
            Ntfy::from_settings(&json!({ "ntfy": { "topic": "a/b" } })),
            None
        );
        assert_eq!(
            Ntfy::from_settings(&json!({ "ntfy": { "server": "https://169.254.169.254", "topic": "t" } }))
                .unwrap()
                .server,
            "https://ntfy.sh"
        );
    }
}
