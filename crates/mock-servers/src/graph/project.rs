//! The mailbox rendered as Graph resources: messages, folders and attachments.

use std::collections::BTreeSet;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gmail::mime::Mailbox as Address;
use gmail::{LabelType, MessagePart};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::ids;
use super::model::{self, Folder};
use super::odata::{self, BodyType, Prefer};
use super::state::{Item, Outlook};
use crate::mail::{Mailbox, StoredMessage};

/// Every property of `message` (with `outlookItem`'s and `entity`'s).
pub const MESSAGE_PROPERTIES: &[&str] = &[
    "id",
    "createdDateTime",
    "lastModifiedDateTime",
    "changeKey",
    "categories",
    "receivedDateTime",
    "sentDateTime",
    "hasAttachments",
    "internetMessageId",
    "subject",
    "bodyPreview",
    "importance",
    "parentFolderId",
    "conversationId",
    "conversationIndex",
    "isDeliveryReceiptRequested",
    "isReadReceiptRequested",
    "isRead",
    "isDraft",
    "webLink",
    "inferenceClassification",
    "body",
    "sender",
    "from",
    "toRecipients",
    "ccRecipients",
    "bccRecipients",
    "replyTo",
    "flag",
    "uniqueBody",
    "internetMessageHeaders",
];

/// What a message carries without `$select`: everything but `uniqueBody` and
/// `internetMessageHeaders`, which Graph returns only when selected.
const MESSAGE_DEFAULT: usize = MESSAGE_PROPERTIES.len() - 2;

pub const FOLDER_PROPERTIES: &[&str] = &[
    "id",
    "displayName",
    "parentFolderId",
    "childFolderCount",
    "unreadItemCount",
    "totalItemCount",
    "isHidden",
];

pub const ATTACHMENT_PROPERTIES: &[&str] = &[
    "id",
    "lastModifiedDateTime",
    "name",
    "contentType",
    "size",
    "isInline",
    "contentId",
    "contentLocation",
    "contentBytes",
];

/// Everything a Graph response reads, borrowed from the locked world.
pub struct View<'a> {
    pub mail: &'a Mailbox,
    pub outlook: &'a Outlook,
    pub origin: &'a url::Url,
    pub prefer: Prefer,
    pub tz: &'a TimeZone,
    pub calendars: &'a crate::calendar::Calendars,
    /// The signed-in account, whose object ID `@odata.context` names.
    pub who: &'a super::Account,
}

/// An attachment, as found in a message's MIME tree.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub index: u16,
    pub name: String,
    pub content_type: String,
    pub inline: bool,
    pub content_id: Option<String>,
    pub bytes: Vec<u8>,
}

impl View<'_> {
    pub fn context(&self, fragment: &str) -> String {
        odata::context(self.origin, &format!("users('{}')/{fragment}", self.who.id))
    }

    /// A message's item and Gmail message, by key.
    pub fn message(&self, key: u64) -> Result<(&Item, &StoredMessage)> {
        let item = self
            .outlook
            .items
            .get(&key)
            .ok_or_else(GraphError::not_found)?;
        let message = self
            .mail
            .message(&item.gmail)
            .map_err(|_| GraphError::not_found())?;
        Ok((item, message))
    }

    /// The ID a response gives an item, per `Prefer: IdType`.
    pub fn message_id(&self, key: u64, item: &Item) -> String {
        ids::message(key, item.generation, self.prefer.immutable_ids)
    }

    /// A message as Graph renders it, with the selected properties or the default ones.
    pub fn message_json(&self, key: u64, select: Option<&[String]>) -> Result<Map<String, Value>> {
        let (item, message) = self.message(key)?;
        let (plain, casts) = super::events::split_select(select)?;
        let wanted: Vec<&str> = match &plain {
            Some(select) => {
                odata::check_properties(select, MESSAGE_PROPERTIES, "message")?;
                MESSAGE_PROPERTIES
                    .iter()
                    .copied()
                    .filter(|name| select.iter().any(|s| s.eq_ignore_ascii_case(name)))
                    .collect()
            }
            None => MESSAGE_PROPERTIES[..MESSAGE_DEFAULT].to_vec(),
        };
        let meeting = super::events::meeting(message);
        let mut json = Map::new();
        if let Some(itip) = &meeting {
            json.insert(
                "@odata.type".into(),
                format!("#microsoft.graph.{}", super::events::meeting_type(itip)).into(),
            );
        }
        json.insert(
            "@odata.etag".into(),
            format!("W/\"{}\"", ids::change_key(key, item.version)).into(),
        );
        json.insert("id".into(), self.message_id(key, item).into());
        for name in wanted.into_iter().filter(|name| *name != "id") {
            json.insert(name.to_owned(), self.property(name, key, item, message));
        }
        if let Some(itip) = &meeting {
            for name in super::events::meeting_properties(itip, select.is_none(), &casts) {
                json.insert(name.to_owned(), self.meeting_property(name, itip, message));
            }
        }
        Ok(json)
    }

    /// One property of a message.
    pub fn property(&self, name: &str, key: u64, item: &Item, message: &StoredMessage) -> Value {
        if let Some(value) = self
            .outlook
            .stored
            .get(&key)
            .and_then(|stored| stored.get(name))
        {
            return value.clone();
        }
        let header = |name: &str| message.mime.payload.header(name);
        let received = Timestamp::from_millisecond(message.internal_date).unwrap_or_default();
        match name {
            "createdDateTime" => odata::datetime(item.created).into(),
            "lastModifiedDateTime" => odata::datetime(item.modified).into(),
            "changeKey" => ids::change_key(key, item.version).into(),
            "categories" => self.categories(&item.labels).into(),
            "receivedDateTime" => odata::datetime(received).into(),
            "sentDateTime" => {
                let sent = message
                    .mime
                    .date
                    .and_then(|ms| Timestamp::from_millisecond(ms).ok())
                    .unwrap_or(received);
                odata::datetime(sent).into()
            }
            "hasAttachments" => attachments(message).iter().any(|a| !a.inline).into(),
            "internetMessageId" => message
                .mime
                .rfc822_id
                .as_ref()
                .map_or(Value::Null, |id| format!("<{id}>").into()),
            "subject" => message.mime.subject.clone().into(),
            "bodyPreview" => preview(message).into(),
            "importance" => importance(message).into(),
            "parentFolderId" => ids::folder(&item.folder).into(),
            "conversationId" => ids::conversation(&message.thread_id).into(),
            "conversationIndex" => STANDARD.encode(self.conversation_index(message)).into(),
            "isDeliveryReceiptRequested" => header("Return-Receipt-To").is_some().into(),
            "isReadReceiptRequested" => header("Disposition-Notification-To").is_some().into(),
            "isRead" => (!item.labels.contains("UNREAD")).into(),
            "isDraft" => item.labels.contains("DRAFT").into(),
            "webLink" => {
                let id: String = url::form_urlencoded::byte_serialize(
                    ids::message(key, item.generation, false).as_bytes(),
                )
                .collect();
                format!(
                    "{}owa/?ItemID={id}&exvsurl=1&viewmodel=ReadMessageItem",
                    self.origin
                )
                .into()
            }
            "inferenceClassification" => if model::is_focused(&item.labels) {
                "focused"
            } else {
                "other"
            }
            .into(),
            "body" | "uniqueBody" => self.body(message),
            "sender" => header("Sender")
                .or(header("From"))
                .map_or(Value::Null, |from| recipient(&Address::parse(from))),
            "from" => header("From").map_or(Value::Null, |from| recipient(&Address::parse(from))),
            "toRecipients" => recipients(header("To")),
            "ccRecipients" => recipients(header("Cc")),
            "bccRecipients" => recipients(header("Bcc")),
            "replyTo" => recipients(header("Reply-To")),
            "flag" => self.flag(key, &item.labels),
            "internetMessageHeaders" => message
                .mime
                .payload
                .headers
                .iter()
                .map(|header| json!({ "name": header.name, "value": header.value }))
                .collect::<Vec<_>>()
                .into(),
            _ => Value::Null,
        }
    }

    /// The names of the user labels a message carries that aren't folders, in label order.
    pub fn categories(&self, labels: &BTreeSet<String>) -> Vec<String> {
        let folders = self.outlook.folder_labels();
        self.mail
            .labels()
            .iter()
            .filter(|label| label.kind == Some(LabelType::User))
            .filter_map(|label| {
                let id = label.id.as_deref()?;
                (labels.contains(id) && !folders.iter().any(|f| f == id))
                    .then(|| label.name.clone())
                    .flatten()
            })
            .collect()
    }

    fn flag(&self, key: u64, labels: &BTreeSet<String>) -> Value {
        let stored = self.outlook.flags.get(&key);
        let status = if labels.contains("STARRED") {
            "flagged"
        } else if stored
            .and_then(|flag| flag.get("flagStatus"))
            .and_then(Value::as_str)
            == Some("complete")
        {
            "complete"
        } else {
            "notFlagged"
        };
        let mut flag = Map::new();
        flag.insert("flagStatus".into(), status.into());
        if status != "notFlagged"
            && let Some(stored) = stored
        {
            for (name, value) in stored.iter().filter(|(name, _)| *name != "flagStatus") {
                if name != "completedDateTime" || status == "complete" {
                    flag.insert(name.clone(), value.clone());
                }
            }
        }
        Value::Object(flag)
    }

    /// The body in the format `Prefer` asks for, HTML otherwise.
    fn body(&self, message: &StoredMessage) -> Value {
        let (html, text) = bodies(&message.mime.payload);
        match self.prefer.body {
            Some(BodyType::Text) => {
                let content = text.unwrap_or_else(|| {
                    html.map(|html| mail_parser::decoders::html::html_to_text(&html))
                        .unwrap_or_default()
                });
                json!({ "contentType": "text", "content": content })
            }
            _ => {
                let content = html.unwrap_or_else(|| text_to_html(&text.unwrap_or_default()));
                json!({ "contentType": "html", "content": content })
            }
        }
    }

    /// Outlook's conversation index: a 22-byte header for the conversation, then five bytes for
    /// each message before this one in it.
    fn conversation_index(&self, message: &StoredMessage) -> Vec<u8> {
        let thread = self.mail.thread(&message.thread_id).unwrap_or_default();
        let position = thread
            .iter()
            .position(|m| m.id == message.id)
            .unwrap_or_default();
        let start = thread
            .first()
            .map_or(message.internal_date, |m| m.internal_date);
        let mut index = vec![0x01];
        index.extend_from_slice(&start.to_be_bytes()[3..]);
        let guid = ids::guid(&message.thread_id).replace('-', "");
        index.extend(
            (0..16).filter_map(|i| u8::from_str_radix(guid.get(i * 2..i * 2 + 2)?, 16).ok()),
        );
        for step in 0..position {
            index.extend_from_slice(&(u32::try_from(step).unwrap_or_default() + 1).to_be_bytes());
            index.push(0);
        }
        index
    }

    /// A folder, with its counts.
    pub fn folder_json(&self, folder: &Folder) -> Map<String, Value> {
        let (mut total, mut unread) = (0u32, 0u32);
        for item in self
            .outlook
            .items
            .values()
            .filter(|item| &item.folder == folder)
        {
            total += 1;
            unread += u32::from(item.labels.contains("UNREAD"));
        }
        let children = self.children(folder).len();
        let parent = match folder {
            Folder::Root => None,
            Folder::User(label) => self
                .outlook
                .folders
                .iter()
                .find(|f| &f.label == label)
                .map(|f| f.parent.clone()),
            _ => Some(Folder::Root),
        };
        let hidden = match folder {
            Folder::User(label) => self
                .outlook
                .folders
                .iter()
                .any(|f| &f.label == label && f.hidden),
            _ => false,
        };
        let mut json = Map::new();
        json.insert("id".into(), ids::folder(folder).into());
        json.insert("displayName".into(), self.folder_name(folder).into());
        json.insert(
            "parentFolderId".into(),
            parent.map_or_else(
                || ids::guid("root parent").into(),
                |p| ids::folder(&p).into(),
            ),
        );
        json.insert("childFolderCount".into(), children.into());
        json.insert("unreadItemCount".into(), unread.into());
        json.insert("totalItemCount".into(), total.into());
        json.insert("isHidden".into(), hidden.into());
        json
    }

    /// A folder's display name: a user folder's is the last part of its label's name.
    pub fn folder_name(&self, folder: &Folder) -> String {
        match folder {
            Folder::User(label) => self
                .mail
                .labels()
                .iter()
                .find(|l| l.id.as_deref() == Some(label))
                .and_then(|l| l.name.as_deref())
                .map(|name| name.rsplit('/').next().unwrap_or(name).to_owned())
                .unwrap_or_default(),
            known => known.display_name().unwrap_or_default().to_owned(),
        }
    }

    /// A folder's child folders, the well-known ones first (by name), then user folders in
    /// creation order.
    pub fn children(&self, folder: &Folder) -> Vec<Folder> {
        let mut children: Vec<Folder> = match folder {
            Folder::Root => model::WELL_KNOWN[1..]
                .iter()
                .map(|(f, ..)| f.clone())
                .collect(),
            _ => Vec::new(),
        };
        children.extend(
            self.outlook
                .folders
                .iter()
                .filter(|f| &f.parent == folder)
                .map(|f| Folder::User(f.label.clone())),
        );
        children
    }

    /// Whether `folder` exists.
    pub fn has_folder(&self, folder: &Folder) -> bool {
        match folder {
            Folder::User(label) => self.outlook.folders.iter().any(|f| &f.label == label),
            _ => true,
        }
    }

    /// An attachment as Graph renders it.
    pub fn attachment_json(
        &self,
        key: u64,
        item: &Item,
        attachment: &Attachment,
        with_bytes: bool,
    ) -> Map<String, Value> {
        let mut json = Map::new();
        json.insert(
            "@odata.type".into(),
            "#microsoft.graph.fileAttachment".into(),
        );
        json.insert(
            "@odata.mediaContentType".into(),
            attachment.content_type.clone().into(),
        );
        json.insert(
            "id".into(),
            ids::attachment(
                key,
                item.generation,
                attachment.index,
                self.prefer.immutable_ids,
            )
            .into(),
        );
        json.insert(
            "lastModifiedDateTime".into(),
            odata::datetime(item.modified).into(),
        );
        json.insert("name".into(), attachment.name.clone().into());
        json.insert("contentType".into(), attachment.content_type.clone().into());
        json.insert("size".into(), attachment.bytes.len().into());
        json.insert("isInline".into(), attachment.inline.into());
        json.insert(
            "contentId".into(),
            attachment
                .content_id
                .clone()
                .map_or(Value::Null, Value::from),
        );
        json.insert("contentLocation".into(), Value::Null);
        if with_bytes {
            json.insert(
                "contentBytes".into(),
                STANDARD.encode(&attachment.bytes).into(),
            );
        }
        json
    }
}

/// A message's attachments, in MIME order.
pub fn attachments(message: &StoredMessage) -> Vec<Attachment> {
    let mut found = Vec::new();
    collect_attachments(&message.mime.payload, message, &mut found);
    found
}

fn collect_attachments(part: &MessagePart, message: &StoredMessage, found: &mut Vec<Attachment>) {
    if let Some(id) = part
        .body
        .as_ref()
        .and_then(|body| body.attachment_id.as_deref())
    {
        let disposition = part
            .header("Content-Disposition")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let content_id = part
            .header("Content-ID")
            .map(|id| id.trim().trim_matches(['<', '>']).to_owned());
        found.push(Attachment {
            index: u16::try_from(found.len()).unwrap_or(u16::MAX),
            name: part
                .filename
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "attachment".to_owned()),
            content_type: part
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            inline: disposition.starts_with("inline")
                || (content_id.is_some() && !disposition.starts_with("attachment")),
            content_id,
            bytes: message
                .mime
                .attachments
                .get(id)
                .cloned()
                .unwrap_or_default(),
        });
    }
    for child in &part.parts {
        collect_attachments(child, message, found);
    }
}

/// The first HTML and plain-text bodies in a MIME tree.
pub fn bodies(part: &MessagePart) -> (Option<String>, Option<String>) {
    let mut html = None;
    let mut text = None;
    let mut stack = vec![part];
    while let Some(part) = stack.pop() {
        if let Some(data) = part.body.as_ref().and_then(|body| body.data.as_ref())
            && part.filename.as_deref().is_none_or(str::is_empty)
        {
            let content = String::from_utf8_lossy(data).into_owned();
            match part.mime_type.as_deref() {
                Some("text/html") if html.is_none() => html = Some(content),
                Some("text/plain") if text.is_none() => text = Some(content),
                _ => {}
            }
        }
        stack.extend(part.parts.iter().rev());
    }
    (html, text)
}

/// The first 255 characters of the body as text, whitespace collapsed.
pub fn preview(message: &StoredMessage) -> String {
    let (html, text) = bodies(&message.mime.payload);
    let text = text
        .or_else(|| html.map(|html| mail_parser::decoders::html::html_to_text(&html)))
        .unwrap_or_default();
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(255).collect()
}

/// Plain text as the HTML body Outlook makes of it.
pub fn text_to_html(text: &str) -> String {
    let escaped = crate::mail::mime::escape_html_text(text)
        .replace("\r\n", "\n")
        .replace('\n', "<br>\r\n");
    format!(
        "<html><head>\r\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head><body><div>{escaped}</div></body></html>"
    )
}

/// `Importance:` (or `X-Priority:`), as Outlook's low, normal or high.
pub fn importance(message: &StoredMessage) -> &'static str {
    let header = |name: &str| {
        message
            .mime
            .payload
            .header(name)
            .map(str::to_ascii_lowercase)
    };
    match header("Importance").as_deref() {
        Some("high") => return "high",
        Some("low") => return "low",
        Some(_) => return "normal",
        None => {}
    }
    match header("X-Priority")
        .as_deref()
        .and_then(|p| p.trim().chars().next())
    {
        Some('1' | '2') => "high",
        Some('4' | '5') => "low",
        _ => "normal",
    }
}

pub fn recipient(address: &Address) -> Value {
    json!({ "emailAddress": {
        "name": address.name.clone().unwrap_or_else(|| address.email.clone()),
        "address": address.email,
    }})
}

fn recipients(header: Option<&str>) -> Value {
    Address::parse_list(header.unwrap_or_default())
        .iter()
        .map(recipient)
        .collect::<Vec<_>>()
        .into()
}

/// The Windows name Outlook gives a time zone, or the IANA name when it has none here.
pub fn windows_zone(iana: &str) -> String {
    gmail::ics::iana_to_windows(iana).unwrap_or(iana).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_address_headers() {
        let list = Address::parse_list(
            "\"Reyes, \\\"Dan\\\" Daniel\" <daniel@brightmoor.co> (eng (core)), priya@brightmoor.co, Lena <lena@brightmoor.co>",
        );
        let emails: Vec<&str> = list.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(
            emails,
            [
                "daniel@brightmoor.co",
                "priya@brightmoor.co",
                "lena@brightmoor.co"
            ]
        );
        assert_eq!(list[0].name.as_deref(), Some("Reyes, \"Dan\" Daniel"));
        assert!(Address::parse_list("").is_empty());
    }

    #[test]
    fn windows_zone_uses_shared_cldr_table() {
        assert_eq!(windows_zone("America/New_York"), "Eastern Standard Time");
        assert_eq!(windows_zone("UTC"), "UTC");
        assert_eq!(windows_zone("Etc/GMT"), "UTC");
        assert_eq!(windows_zone("Europe/Kyiv"), "FLE Standard Time");
        assert_eq!(windows_zone("Europe/Kiev"), "FLE Standard Time");
        assert_eq!(
            windows_zone("America/Tijuana"),
            "Pacific Standard Time (Mexico)"
        );
        assert_eq!(
            windows_zone("Pacific/Chatham"),
            "Chatham Islands Standard Time"
        );
        assert_eq!(windows_zone("Mars/Olympus"), "Mars/Olympus");
    }
}
