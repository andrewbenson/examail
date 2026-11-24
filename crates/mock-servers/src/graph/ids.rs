//! Graph's opaque IDs, minted from the mock's own keys so they decode back to them. A message's
//! default ID names its key and how many times it has moved (Outlook's REST IDs change when a
//! message changes folder); its immutable ID (`Prefer: IdType="ImmutableId"`) names the key alone.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::model::Folder;

/// Default (REST) IDs of messages, attachments, folders and contacts look like Outlook's: the
/// mailbox's GUID, then the item's own part.
const REST: &str = "AAMkAGI2NGVhZTk0LTVhODgtNGQ0NC1hYTI2LThlNzYzMmM1NzFjZABGAAAAAADc";
/// Immutable IDs look like Outlook's `restImmutableEntryId`s.
const IMMUTABLE: &str = "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A";
const CONVERSATION: &str = "AAQkAGI2NGVhZTk0LTVhODgtNGQ0NC1hYTI2LThlNzYzMmM1NzFjZAAQA";

const MESSAGE: u8 = b'M';
const MESSAGE_IMMUTABLE: u8 = b'I';
const ATTACHMENT: u8 = b'A';
const ATTACHMENT_IMMUTABLE: u8 = b'a';
const FOLDER: u8 = b'F';
const CONTACT: u8 = b'P';
const THREAD: u8 = b'C';
const EVENT: u8 = b'E';
const CALENDAR: u8 = b'K';

/// What an ID names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    /// A message, by key, and its move count when it's a default ID.
    Message {
        key: u64,
        generation: Option<u32>,
    },
    /// One of a message's attachments, by position.
    Attachment {
        key: u64,
        generation: Option<u32>,
        index: u16,
    },
    Folder(Folder),
    /// A saved contact, by its People resource name.
    Contact(String),
    /// A conversation, by its Gmail thread ID.
    Conversation(String),
    /// An event, by its calendar's ID and its own.
    Event {
        calendar: String,
        event: String,
    },
    /// A calendar, by its ID.
    Calendar(String),
}

pub fn message(key: u64, generation: u32, immutable: bool) -> String {
    if immutable {
        encode(IMMUTABLE, MESSAGE_IMMUTABLE, &key.to_be_bytes())
    } else {
        encode(
            REST,
            MESSAGE,
            &[&key.to_be_bytes()[..], &generation.to_be_bytes()].concat(),
        )
    }
}

pub fn attachment(key: u64, generation: u32, index: u16, immutable: bool) -> String {
    if immutable {
        let payload = [&key.to_be_bytes()[..], &index.to_be_bytes()].concat();
        encode(IMMUTABLE, ATTACHMENT_IMMUTABLE, &payload)
    } else {
        let payload = [
            &key.to_be_bytes()[..],
            &generation.to_be_bytes(),
            &index.to_be_bytes(),
        ]
        .concat();
        encode(REST, ATTACHMENT, &payload)
    }
}

pub fn folder(folder: &Folder) -> String {
    let payload = match folder {
        Folder::User(label) => [&[0xff][..], label.as_bytes()].concat(),
        known => vec![u8::try_from(known.index().unwrap_or_default()).unwrap_or_default()],
    };
    encode(REST, FOLDER, &payload)
}

pub fn contact(resource: &str) -> String {
    encode(REST, CONTACT, resource.as_bytes())
}

/// An event's ID. Events don't change ID, so it serves as the immutable one too.
pub fn event(calendar: &str, event: &str) -> String {
    encode(REST, EVENT, format!("{calendar}\n{event}").as_bytes())
}

/// A calendar's ID.
pub fn calendar(id: &str) -> String {
    encode(REST, CALENDAR, id.as_bytes())
}

pub fn conversation(thread: &str) -> String {
    encode(CONVERSATION, THREAD, thread.as_bytes())
}

/// The ID's meaning, or `None` when it isn't one the mock minted.
pub fn decode(id: &str) -> Option<Id> {
    let rest = [REST, IMMUTABLE, CONVERSATION]
        .iter()
        .find_map(|prefix| id.strip_prefix(prefix))?;
    let bytes = URL_SAFE_NO_PAD.decode(rest).ok()?;
    let (&check, body) = bytes.split_last()?;
    if checksum(body) != check {
        return None;
    }
    let (&kind, payload) = body.split_first()?;
    let u64_at = |at: usize| {
        Some(u64::from_be_bytes(
            payload.get(at..at + 8)?.try_into().ok()?,
        ))
    };
    let u32_at = |at: usize| {
        Some(u32::from_be_bytes(
            payload.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    let u16_at = |at: usize| {
        Some(u16::from_be_bytes(
            payload.get(at..at + 2)?.try_into().ok()?,
        ))
    };
    let text = || String::from_utf8(payload.to_vec()).ok();
    Some(match kind {
        MESSAGE if payload.len() == 12 => Id::Message {
            key: u64_at(0)?,
            generation: Some(u32_at(8)?),
        },
        MESSAGE_IMMUTABLE if payload.len() == 8 => Id::Message {
            key: u64_at(0)?,
            generation: None,
        },
        ATTACHMENT if payload.len() == 14 => Id::Attachment {
            key: u64_at(0)?,
            generation: Some(u32_at(8)?),
            index: u16_at(12)?,
        },
        ATTACHMENT_IMMUTABLE if payload.len() == 10 => Id::Attachment {
            key: u64_at(0)?,
            generation: None,
            index: u16_at(8)?,
        },
        FOLDER => match payload.split_first()? {
            (0xff, label) => Id::Folder(Folder::User(String::from_utf8(label.to_vec()).ok()?)),
            (&index, []) => Id::Folder(super::model::WELL_KNOWN.get(usize::from(index))?.0.clone()),
            _ => return None,
        },
        CONTACT => Id::Contact(text()?),
        EVENT => {
            let text = text()?;
            let (calendar, event) = text.split_once('\n')?;
            Id::Event {
                calendar: calendar.to_owned(),
                event: event.to_owned(),
            }
        }
        CALENDAR => Id::Calendar(text()?),
        THREAD => Id::Conversation(text()?),
        _ => return None,
    })
}

fn encode(prefix: &str, kind: u8, payload: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(payload.len() + 2);
    bytes.push(kind);
    bytes.extend_from_slice(payload);
    bytes.push(checksum(&bytes));
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes
        .iter()
        .fold(0u32, |sum, &byte| (sum * 31 + u32::from(byte)) % 251);
    u8::try_from(sum).unwrap_or_default()
}

/// A GUID derived from `seed`, stable across runs: user and tenant IDs, request IDs, category and
/// override IDs.
pub fn guid(seed: &str) -> String {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("a SHA-256 has 32 bytes");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A change key (and, quoted, an ETag) for the `version`th state of item `key`.
pub fn change_key(key: u64, version: u64) -> String {
    let bytes = [&key.to_be_bytes()[..], &version.to_be_bytes()].concat();
    format!("CQAAABYAAA{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        assert_eq!(
            decode(&message(7, 2, false)),
            Some(Id::Message {
                key: 7,
                generation: Some(2)
            })
        );
        assert_eq!(
            decode(&message(7, 2, true)),
            Some(Id::Message {
                key: 7,
                generation: None
            })
        );
        assert_ne!(message(7, 2, false), message(7, 3, false));
        assert_eq!(message(7, 2, true), message(7, 3, true));
        assert_eq!(
            decode(&attachment(7, 1, 3, false)),
            Some(Id::Attachment {
                key: 7,
                generation: Some(1),
                index: 3
            })
        );
        for folder in [Folder::Inbox, Folder::Root, Folder::User("Label_13".into())] {
            assert_eq!(decode(&self::folder(&folder)), Some(Id::Folder(folder)));
        }
        assert_eq!(
            decode(&event("maya@brightmoor.co", "abc_20261118T150000Z")),
            Some(Id::Event {
                calendar: "maya@brightmoor.co".into(),
                event: "abc_20261118T150000Z".into()
            })
        );
        assert_eq!(
            decode(&calendar("maya@brightmoor.co")),
            Some(Id::Calendar("maya@brightmoor.co".into()))
        );
        assert_eq!(
            decode(&conversation("19a2f")),
            Some(Id::Conversation("19a2f".into()))
        );
    }

    #[test]
    fn foreign_ids_are_rejected() {
        let id = message(7, 2, false);
        let at = id.len() - 4;
        let swapped = if &id[at..=at] == "Q" { "R" } else { "Q" };
        let tampered = format!("{}{swapped}{}", &id[..at], &id[at + 1..]);
        assert_eq!(decode(&tampered), None);
        assert_eq!(
            decode(
                "AAMkAGVmMDEzMTM4LTZmYWUtNDdkNC1hMDZiLTU1OGY5OTZhYmY4OAAuAAAAAAAiQ8W967B7TKBjgx9rVEURAQAiIsqMbYjsT5e-T7KzowPTAAAAAAFNAAA="
            ),
            None
        );
        assert_eq!(decode("inbox"), None);
    }

    #[test]
    fn guids_are_stable_and_well_formed() {
        let id = guid("maya@brightmoor.co");
        assert_eq!(id, guid("maya@brightmoor.co"));
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }
}
