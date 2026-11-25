//! Draft domain types, failure codes, and row predicates.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db::DraftRow;
use crate::ids::ThreadId;

/// What a draft is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftAction {
    /// A new message.
    Compose,
    /// A reply to the sender.
    Reply,
    /// A reply to everyone.
    ReplyAll,
    /// A forward.
    Forward,
    /// A reply whose subject was changed, which starts a new thread.
    EditSubjectLineReply,
}

/// A name and address on a draft.
pub type Address = gmail::mime::Mailbox;

/// A draft as compose keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRecord {
    /// What it's for.
    pub action: DraftAction,
    /// The send-as alias it's from.
    pub from: Option<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Bcc`.
    pub bcc: Vec<Address>,
    /// The subject.
    pub subject: String,
    /// The editor's HTML: the body only, without signature or quote until they're revealed.
    pub body: String,
    /// The quoted message of a reply or forward, outside the body until revealed.
    pub quoted_content: Option<String>,
    /// The quote and signature are in the body (revealed with "…", or deleted).
    pub quoted_inlined: bool,
    /// The quote carries the draft's own signature, as a draft written in another app does
    /// ([`DraftRecord::quoted_content`] holds all that followed its text), so none is added.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quote_signed: bool,
    /// The Gmail message replied to or forwarded.
    pub in_reply_to: Option<String>,
    /// That message's `Message-ID`, without angle brackets.
    pub in_reply_to_rfc822: Option<String>,
    /// `References`, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// The draft's own `Message-ID`, without angle brackets, kept across saves.
    pub rfc822_id: String,
    /// To, Cc and the files as created (`fingerprint`). A reply whose recipients and files are
    /// untouched is unmodified.
    pub fingerprint: String,
    /// Its files, in the order they were added.
    pub attachments: Vec<DraftAttachment>,
    /// The time Send later picked, which Send schedules the message for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_later: Option<SendLater>,
    /// Remind me's choice for a draft. Its thread gets the reminder at Send (or Schedule), or
    /// once it's stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder: Option<DraftReminder>,
}

/// A draft's reminder: when the thread comes back, and whether a reply cancels it. Nothing is set
/// until the message is sent or scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReminder {
    /// When.
    pub at: Timestamp,
    /// `regardless`, which means a reply doesn't cancel it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_on_reply: bool,
    /// Automatic Reminders put it there, so it comes off again once no recipient is external.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub automatic: bool,
}

/// Send later's choice for a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendLater {
    /// When to send.
    pub at: Timestamp,
    /// `if no reply`, which means a reply before then calls the send off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abort_on_reply: bool,
    /// The thread's messages when the send was scheduled, which [`crate::Store::schedule_draft`]
    /// records. Any other message that isn't the user's or a draft is the reply that calls an
    /// `abort_on_reply` send off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_message_ids: Vec<String>,
}

/// A file on a draft. Its bytes are kept under its `uuid` in [`crate::Store::draft_files`]. A
/// Gmail part is fetched there the first time it's needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    /// Exo's id for it (a v4 UUID).
    pub uuid: String,
    /// The file name.
    pub name: String,
    /// Its MIME type.
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
    /// The `Content-ID` that an inline image is shown by, or `None` for a listed file.
    pub content_id: Option<String>,
    /// Where its bytes come from.
    pub source: AttachmentSource,
}

/// Where an attachment's bytes come from (`source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentSource {
    /// A file the user added, copied into the draft files.
    Local,
    /// A part of a Gmail message (a forward's files, a quoted message's inline images).
    Gmail {
        /// The message.
        message_id: String,
        /// Gmail's attachment id, or `None` when the part's data comes with the message.
        attachment_id: Option<String>,
        /// The MIME part.
        part_id: String,
    },
}

/// How a draft's files stand with Gmail, for compose's cards: which of them Gmail's copy of the
/// draft has, and how far the write carrying the others has come. A file goes up with the first
/// write of the draft that has it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Upload {
    /// The ids of the files Gmail's copy of the draft has.
    pub gmail_has: HashSet<String>,
    /// While a write is under way, the ids of the files it carries that Gmail lacks, and the
    /// share of its message sent so far (0–100).
    pub sending: Option<(HashSet<String>, f32)>,
}

impl Upload {
    /// How far `attachment` has gone up (0–100) while Gmail's copy of the draft lacks it, or 0
    /// while no write carries it yet. `None` once Gmail has it, or for a Gmail part, which never
    /// goes up.
    #[must_use]
    pub fn progress(&self, attachment: &DraftAttachment) -> Option<f32> {
        if attachment.source != AttachmentSource::Local || self.gmail_has.contains(&attachment.uuid)
        {
            return None;
        }
        Some(
            self.sending
                .as_ref()
                .filter(|(files, _)| files.contains(&attachment.uuid))
                .map_or(0.0, |(_, percent)| *percent),
        )
    }
}

/// Every draft's [`Upload`] the store knows of this session, by draft id.
pub type Uploads = HashMap<String, Upload>;

/// A draft to save.
#[derive(Debug, Clone)]
pub struct Draft {
    /// Exo's draft id.
    pub id: String,
    /// The Gmail thread it belongs to, when known. A reply has one, and a new message gets one on
    /// its first save.
    pub thread: Option<ThreadId>,
    /// On its first save, the Gmail message of the draft that it takes over, written elsewhere
    /// (Gmail web). That Gmail draft is the one written, found by its message when the draft is
    /// pushed.
    pub adopted: Option<String>,
    /// What compose shows.
    pub record: DraftRecord,
    /// The RFC 822 message that the Gmail draft holds, which is exactly what sending sends.
    pub raw: String,
    /// Whether `raw` has every file. A draft missing one isn't written to Gmail until a save has
    /// them all, since Gmail's copy would lose the missing file.
    pub complete: bool,
    /// Whether it's saved to be sent. Such a draft is written to Gmail even when Gmail has its
    /// content already, since that write dates the message (`Date:`) it sends.
    pub sending: bool,
}

/// `not_before` of a draft saved without some of its files, which isn't pushed until a save has
/// them.
pub(crate) const UNTIL_COMPLETE: Timestamp = Timestamp::MAX;

/// A saved draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDraft {
    /// Exo's draft id.
    pub id: String,
    /// Its Gmail thread, once it has one.
    pub thread: Option<ThreadId>,
    /// Its Gmail draft, once created.
    pub gmail_id: Option<String>,
    /// The Gmail message of the latest version Gmail has.
    pub message_id: Option<String>,
    /// What compose shows.
    pub record: DraftRecord,
    /// The message it sends.
    pub raw: String,
    /// Held for sending until then.
    pub send_at: Option<Timestamp>,
    /// Gmail has the latest version.
    pub synced: bool,
    /// Its last save had every file ([`Draft::complete`]). A draft whose last save didn't waits
    /// for a save that has them before it's written to Gmail.
    pub complete: bool,
    /// Why its send failed, if it did (a failed draft). A failed draft shows in its thread as a
    /// message until it's edited, sent again or discarded.
    pub failure: Option<SendFailure>,
}

/// A draft Gmail sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentDraft {
    /// Exo's draft id.
    pub id: String,
    /// The thread the message went into.
    pub thread: ThreadId,
    /// The sent message.
    pub message: String,
}

/// Why a send failed, as far as Gmail's answers tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    /// Google signed the account out (`invalid_auth`).
    LoggedOut,
    /// The account's sending limit (`quota_exceeded`).
    QuotaExceeded,
    /// Over Gmail's size limit (`message_too_large`).
    TooLarge,
    /// A recipient Gmail refuses (`invalid_recipients`).
    InvalidRecipients,
    /// Gmail is rate limiting the account for longer than a send waits (`provider-rate-limit`).
    RateLimited,
    /// Gmail kept failing for a while (`provider-transient-error`).
    Temporary,
    /// Anything else (no reason).
    Other,
}

impl SendFailure {
    /// The failure's code, as it's stored.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::LoggedOut => "invalid_auth",
            Self::QuotaExceeded => "quota_exceeded",
            Self::TooLarge => "message_too_large",
            Self::InvalidRecipients => "invalid_recipients",
            Self::RateLimited => "provider-rate-limit",
            Self::Temporary => "provider-transient-error",
            Self::Other => "",
        }
    }

    pub(super) fn from_code(code: &str) -> Self {
        [
            Self::LoggedOut,
            Self::QuotaExceeded,
            Self::TooLarge,
            Self::InvalidRecipients,
            Self::RateLimited,
            Self::Temporary,
        ]
        .into_iter()
        .find(|reason| reason.code() == code)
        .unwrap_or(Self::Other)
    }
}

/// A draft Gmail didn't take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftFailure {
    /// Saving it was refused (`Sorry! Failed to save draft.`). The next save tries again.
    Save {
        /// Exo's draft id.
        id: String,
        /// Gmail's error.
        error: String,
    },
    /// Its send failed, so it's a failed draft and no longer held ([`StoredDraft::failure`]).
    Send {
        /// Exo's draft id.
        id: String,
        /// Why.
        reason: SendFailure,
    },
}

/// What a [`crate::Store::push_drafts`] did.
#[derive(Debug, Default)]
pub struct DraftReport {
    /// Drafts whose latest version Gmail now has.
    pub saved: Vec<String>,
    /// Drafts Gmail sent.
    pub sent: Vec<SentDraft>,
    /// Drafts Gmail refused.
    pub failed: Vec<DraftFailure>,
    /// Some draft is waiting for the network.
    pub offline: bool,
    /// Held sends whose write or send Gmail didn't take this time, for a reason other than the
    /// network, which are tried again later.
    pub retrying: Vec<String>,
    /// When to push again: a backoff ending, or a held send coming due.
    pub retry_at: Option<Timestamp>,
    /// A held send's reminder went on its thread, and its label is queued for a flush.
    pub reminded: bool,
}

impl From<DraftRow> for StoredDraft {
    fn from(row: DraftRow) -> Self {
        Self {
            synced: row.pushed >= row.revision,
            complete: complete(&row),
            id: row.id,
            thread: row.thread_id.map(ThreadId::new),
            gmail_id: row.gmail_id,
            message_id: row.message_id,
            record: row.record.0,
            raw: row.raw,
            send_at: row.send_at,
            failure: row.failure.as_deref().map(SendFailure::from_code),
        }
    }
}

/// Whether the draft's last save had every file ([`Draft::complete`]).
pub(crate) fn complete(row: &DraftRow) -> bool {
    row.not_before != Some(UNTIL_COMPLETE)
}

/// When draft `row` is scheduled to go, if it's a send held for its Send later time.
pub(crate) fn scheduled(row: &DraftRow) -> Option<Timestamp> {
    row.send_at
        .filter(|_| !row.discarded && row.failure.is_none() && row.record.0.send_later.is_some())
}
