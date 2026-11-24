//! What the UI reads: thread summaries for lists, full threads, labels and counts.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::drafts::SendFailure;
use crate::ids::ThreadId;
use crate::mutation::Mutation;

/// A person on a message: `From`, `To`, `Cc`, `Bcc` or `Reply-To`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Display name, when the header has one.
    pub name: Option<String>,
    /// Address as written in the header.
    pub email: String,
    /// One of the account's own addresses (the primary or a send-as alias Gmail treats as one).
    pub is_me: bool,
}

impl From<Contact> for gmail::mime::Mailbox {
    fn from(contact: Contact) -> Self {
        Self {
            name: contact.name,
            email: contact.email,
        }
    }
}

impl From<&Contact> for gmail::mime::Mailbox {
    fn from(contact: &Contact) -> Self {
        Self {
            name: contact.name.clone(),
            email: contact.email.clone(),
        }
    }
}

/// A send-as address (from Gmail's `sendAs`), with its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    /// The address in `From:`.
    pub email: String,
    /// The name in `From:`. `None` means the account's name.
    pub name: Option<String>,
    /// Its Gmail signature (HTML, as Gmail stores it).
    pub signature: Option<String>,
    /// The default `From:` for new mail.
    pub default: bool,
    /// The account's own address.
    pub primary: bool,
    /// Usable: the default, the primary, or verified.
    pub active: bool,
}

/// One of a contact's latest conversations, for the contact pane's Mail section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentThread {
    /// The thread.
    pub id: ThreadId,
    /// Its subject, from its first message. Empty when it has none.
    pub subject: String,
    /// The date of its latest message outside drafts, Spam and Trash.
    pub date: Timestamp,
}

/// A row of a thread list.
///
/// Drafts that Exo didn't write (in Gmail on the web or in another client) are left out of a row
/// everywhere but Drafts, where they're the point. "Drafts" below means the ones the row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    /// The thread.
    pub id: ThreadId,
    /// The thread's subject, from its first message.
    pub subject: String,
    /// Plain-text excerpt of the latest message, drafts included, with HTML entities decoded. A
    /// draft's `{PLACEHOLDER}` braces are dropped.
    pub snippet: String,
    /// Who to show in the From column, in order. These are the senders in chronological order,
    /// de-duplicated, without you unless you're the only one. When the thread has a draft they're
    /// the latest draft's recipients, and otherwise in Sent they're the recipients. Name
    /// formatting and overflow are the UI's job.
    pub participants: Vec<Contact>,
    /// The thread's date in this list, from its latest relevant message. Your own replies don't
    /// bump a thread, and in Sent it's your latest sent message. Lists are ordered by it, newest
    /// first.
    pub date: Timestamp,
    /// Some message is unread.
    pub unread: bool,
    /// Some message is starred.
    pub starred: bool,
    /// Muted, which means it has the mute label and no newer message added you as a recipient.
    pub muted: bool,
    /// Done: not routed to the inbox.
    pub done: bool,
    /// Every label id on the thread's messages, including pending changes. The UI picks which
    /// ones become chips.
    pub labels: Vec<String>,
    /// What the row's attachment icon shows.
    pub attachments: Attachments,
    /// Its attached files, which a search result shows as cards. These are the attachments of the
    /// messages the row shows, without inline files and invites, one per name (the last
    /// message's).
    pub files: Vec<Attachment>,
    /// Whom the contact pane shows for the thread: the first sender who isn't you, looking at the
    /// unread messages oldest first and then the rest newest first.
    pub contact: Option<Contact>,
    /// Messages the row shows, drafts included.
    pub messages: usize,
    /// Drafts the row shows, failed ones aside.
    pub drafts: usize,
    /// Why the thread's failed draft failed, when it has one, which the row shows instead of the
    /// date. It's [`SendFailure::Other`] when the thread's failed drafts don't share a reason.
    pub failure: Option<SendFailure>,
    /// One of the thread's sends is held or waiting to go. While offline, the row says it will go
    /// when online.
    pub sending: bool,
    /// The time of the thread's earliest scheduled send, and whether Gmail has the latest version
    /// of each of its scheduled sends. The row says `Scheduled: {date}`, or `Schedule: {date}`
    /// until Gmail does.
    pub scheduled: Option<(Timestamp, bool)>,
    /// The triage mutations that would take the thread out of the list it was read from, as
    /// [`Store::apply`](crate::Store::apply) would apply them, so its row can slide out the moment
    /// one is applied. Adding a label never takes a thread out of a list.
    pub removed_by: Vec<Mutation>,
    /// Its reminder, while the row shows it.
    pub reminder: Option<ReminderStatus>,
}

/// Where a thread's reminder stands, as its row and header show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderStatus {
    /// Set: the thread comes back at `at`.
    Set {
        /// When.
        at: Timestamp,
        /// A reply doesn't cancel it (the picker's `regardless`).
        keep_on_reply: bool,
    },
    /// It brought the thread back at `at`, and nothing has come in since.
    Returned {
        /// When.
        at: Timestamp,
        /// Not read since, so the row's dot is pink.
        unread: bool,
    },
    /// Someone replied, which cancelled an "if no reply" reminder.
    Aborted {
        /// Who replied.
        by: Contact,
    },
}

/// The files a thread carries, as its row shows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Attachments {
    /// Nothing attached.
    #[default]
    None,
    /// Real (non-inline) files, shown as the paperclip.
    Files,
    /// Only a calendar invite, shown as the calendar icon.
    CalendarInvite,
}

/// One page of a thread list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPage {
    /// The threads, newest first.
    pub threads: Vec<ThreadSummary>,
    /// Where the next page starts, when there are more local threads in the backfilled range.
    pub next: Option<Cursor>,
    /// Gmail may have threads older than what's stored for this list.
    /// [`Store::backfill`](crate::Store::backfill) extends the range.
    pub more_remote: bool,
}

/// A position in a thread list (keyset pagination on the list's date, then thread id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub(crate) sort: i64,
    pub(crate) thread: String,
}

/// Thread counts for a list, over what's stored locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListCount {
    /// Threads in the list.
    pub threads: usize,
    /// Of those, unread ones.
    pub unread: usize,
    /// Every thread of the list is stored, because its backfill or All Mail's has finished, so the
    /// counts are exact.
    pub exact: bool,
}

/// A thread with every message, for the thread view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadDetail {
    /// The thread.
    pub id: ThreadId,
    /// The thread's subject, from its first message.
    pub subject: String,
    /// Every label id on the thread's messages, including pending changes.
    pub labels: Vec<String>,
    /// Muted, which means it has the mute label and no newer message added you as a recipient.
    pub muted: bool,
    /// Its reminder, while the header shows it.
    pub reminder: Option<ReminderStatus>,
    /// Messages, oldest first, drafts included.
    pub messages: Vec<MessageDetail>,
}

/// One message of a [`ThreadDetail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDetail {
    /// Gmail message id.
    pub id: String,
    /// Label ids, including pending changes.
    pub labels: Vec<String>,
    /// When Gmail received it (`internalDate`).
    pub date: Timestamp,
    /// `From`.
    pub from: Option<Contact>,
    /// `To`.
    pub to: Vec<Contact>,
    /// `Cc`.
    pub cc: Vec<Contact>,
    /// `Bcc` (only on messages you sent).
    pub bcc: Vec<Contact>,
    /// `Reply-To`.
    pub reply_to: Vec<Contact>,
    /// `Subject`.
    pub subject: String,
    /// Gmail's plain-text excerpt, HTML entities decoded.
    pub snippet: String,
    /// `Message-ID`, without angle brackets.
    pub rfc822_id: Option<String>,
    /// `In-Reply-To`, without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References`, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// `List-Id`.
    pub list_id: Option<String>,
    /// `List-Unsubscribe`.
    pub list_unsubscribe: Option<String>,
    /// `List-Unsubscribe-Post` (RFC 8058 one-click).
    pub list_unsubscribe_post: Option<String>,
    /// Every top-level header, in message order.
    pub headers: Vec<Header>,
    /// The body's HTML and plain-text parts.
    pub body: Body,
    /// Attachment metadata. Fetch the bytes with Gmail's `attachments.get`.
    pub attachments: Vec<Attachment>,
    /// A failed draft's reason. The thread shows a failed draft as a message, not a draft to edit.
    pub failure: Option<SendFailure>,
    /// A scheduled send's time. The thread shows a scheduled send as a message to go then, not a
    /// draft to edit.
    pub scheduled: Option<Timestamp>,
    /// A send being held or waiting to go, shown as it will be sent. The stored thread never has
    /// one, because compose adds it.
    pub sending: bool,
}

impl MessageDetail {
    /// Has `label`.
    #[must_use]
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|id| id == label)
    }

    /// A Gmail draft.
    #[must_use]
    pub fn is_draft(&self) -> bool {
        self.has_label(crate::ids::labels::DRAFT)
    }

    /// Unread.
    #[must_use]
    pub fn is_unread(&self) -> bool {
        self.has_label(crate::ids::labels::UNREAD)
    }

    /// Starred.
    #[must_use]
    pub fn is_starred(&self) -> bool {
        self.has_label(crate::ids::labels::STARRED)
    }
}

/// A message header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Name as written.
    pub name: String,
    /// Decoded value.
    pub value: String,
}

/// A message body as sent, not sanitized or quote-split yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    /// The `text/html` parts, concatenated.
    pub html: Option<String>,
    /// The `text/plain` parts, concatenated.
    pub text: Option<String>,
}

/// An attachment's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The message it belongs to.
    pub message_id: String,
    /// Gmail's attachment id, for `attachments.get`. Absent when the data was inline.
    pub attachment_id: Option<String>,
    /// MIME part id (`1`, `1.2`, ...).
    pub part_id: String,
    /// File name.
    pub filename: String,
    /// MIME type, e.g. `application/pdf`.
    pub mime_type: String,
    /// Size in bytes.
    pub size: u32,
    /// `Content-ID` without angle brackets, for `cid:` references in the HTML.
    pub content_id: Option<String>,
    /// Shown in the body rather than listed, because the HTML references its `Content-ID` or it's
    /// a Gmail emoji.
    pub inline: bool,
}

/// A Gmail label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// Label id (`INBOX`, `Label_12`).
    pub id: String,
    /// Name, with `/` separating nesting levels. System labels keep Gmail's id-like names.
    pub name: String,
    /// System or user label.
    pub kind: LabelKind,
    /// Gmail's text color, `#rrggbb`.
    pub text_color: Option<String>,
    /// Gmail's background color, `#rrggbb`.
    pub background_color: Option<String>,
    /// Hidden from label lists (`labelHide`), like Exo's own `[Exo]/…` labels.
    pub hidden: bool,
}

/// Whether Gmail or the user created a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// `INBOX`, `UNREAD`, `CATEGORY_*`, ...
    System,
    /// Created by the user (or by Exo).
    User,
}
