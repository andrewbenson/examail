//! Copy Attachments, which puts a message's files on the clipboard as references to their Gmail
//! parts, with the account they're in. ⌘V in a draft adds the files to it. Files from another
//! account are refused, because Gmail parts can only be sent from their own mailbox.

use exo_store::{Attachment, AttachmentSource, DraftAttachment};
use serde::{Deserialize, Serialize};

/// What Copy Attachments puts on the clipboard (`{attachments, account}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Copied {
    /// The address of the account the files are in.
    pub(crate) account: String,
    pub(crate) attachments: Vec<File>,
}

/// One file's metadata, which is enough to send it from its Gmail message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct File {
    pub(crate) message_id: String,
    pub(crate) attachment_id: Option<String>,
    pub(crate) part_id: String,
    pub(crate) name: String,
    pub(crate) mime_type: String,
    pub(crate) size: u64,
}

/// The alert a paste of another account's files shows.
pub(crate) const OTHER_ACCOUNT: &str = "Sorry! Attachments cannot be copy & pasted between email accounts. If you'd like to include attachments, first download them and then attach them to this draft.";

/// The files Copy Attachments copies from a message, which are all but its inline images.
pub(crate) fn copied(attachments: &[Attachment]) -> impl Iterator<Item = &Attachment> {
    attachments
        .iter()
        .filter(|attachment| !is_inline_image(attachment))
}

/// An inline part that's an image, or of no known type but shown by a Content-ID.
fn is_inline_image(attachment: &Attachment) -> bool {
    attachment.inline
        && (super::is_image(attachment)
            || (attachment.mime_type == "application/octet-stream"
                && attachment.content_id.is_some()))
}

impl File {
    /// Whether this entry's Gmail identifiers are single safe tokens (no path separators, `..`, or
    /// control characters from a crafted clipboard payload).
    pub(crate) fn has_safe_ids(&self) -> bool {
        super::is_safe_id(&self.message_id)
            && super::is_safe_id(&self.part_id)
            && self.attachment_id.as_deref().is_none_or(super::is_safe_id)
    }
}

impl Copied {
    /// `attachments` of `account`'s message, for the clipboard.
    pub(crate) fn new(account: &str, attachments: &[Attachment]) -> Self {
        let attachments = copied(attachments)
            .map(|attachment| File {
                message_id: attachment.message_id.clone(),
                attachment_id: attachment.attachment_id.clone(),
                part_id: attachment.part_id.clone(),
                name: attachment.filename.clone(),
                mime_type: attachment.mime_type.clone(),
                size: u64::from(attachment.size),
            })
            .collect();
        Self {
            account: account.to_owned(),
            attachments,
        }
    }

    /// The files as a draft's, by reference to their Gmail parts, each with a new id. Entries
    /// whose Gmail part identifiers contain path-traversal or control characters are dropped.
    pub(crate) fn into_draft_attachments(self) -> Vec<DraftAttachment> {
        self.attachments
            .into_iter()
            .filter(File::has_safe_ids)
            .map(|file| DraftAttachment {
                uuid: crate::compose::uuid_v4(),
                name: super::file_name(&file.name),
                mime_type: file.mime_type,
                size: file.size,
                content_id: None,
                source: AttachmentSource::Gmail {
                    message_id: file.message_id,
                    attachment_id: file.attachment_id,
                    part_id: file.part_id,
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(part: &str, filename: &str, mime_type: &str, inline: bool) -> Attachment {
        Attachment {
            message_id: "18f3a".into(),
            attachment_id: Some(format!("att-{part}")),
            part_id: part.into(),
            filename: filename.into(),
            mime_type: mime_type.into(),
            size: 3,
            content_id: inline.then(|| format!("cid-{part}")),
            inline,
        }
    }

    #[test]
    fn a_messages_files_are_copied_but_not_its_inline_images() {
        let attachments = [
            attachment("1", "deck.pdf", "application/pdf", false),
            attachment("2", "logo.png", "image/png", true),
            attachment("3", "photo.jpg", "image/jpeg", false),
        ];
        let copied = Copied::new("maya@brightmoor.co", &attachments);
        assert_eq!(
            copied
                .attachments
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            ["deck.pdf", "photo.jpg"]
        );
        let json = serde_json::to_string(&copied).expect("serializes");
        assert_eq!(
            serde_json::from_str::<Copied>(&json).expect("parses"),
            copied
        );
        let draft = copied.into_draft_attachments();
        assert_eq!(
            draft[0].source,
            AttachmentSource::Gmail {
                message_id: "18f3a".into(),
                attachment_id: Some("att-1".into()),
                part_id: "1".into()
            }
        );
        assert_ne!(draft[0].uuid, draft[1].uuid);
    }

    #[test]
    fn pasted_attachments_drop_path_traversal_identifiers_and_sanitize_names() {
        let copied = Copied {
            account: "maya@brightmoor.co".into(),
            attachments: vec![
                File {
                    message_id: "../../etc".into(),
                    attachment_id: Some("att-1".into()),
                    part_id: "1".into(),
                    name: "deck.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size: 3,
                },
                File {
                    message_id: "18f3a".into(),
                    attachment_id: Some("att-2".into()),
                    part_id: "../2".into(),
                    name: "deck.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size: 3,
                },
                File {
                    message_id: "18f3a".into(),
                    attachment_id: Some("../att-3".into()),
                    part_id: "3".into(),
                    name: "deck.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size: 3,
                },
                File {
                    message_id: "18f3a".into(),
                    attachment_id: Some("att-4".into()),
                    part_id: "4".into(),
                    name: "../../.zshrc".into(),
                    mime_type: "text/plain".into(),
                    size: 3,
                },
            ],
        };
        let draft = copied.into_draft_attachments();
        assert_eq!(draft.len(), 1);
        assert_eq!(draft[0].name, "_.._.zshrc");
        assert_eq!(
            draft[0].source,
            AttachmentSource::Gmail {
                message_id: "18f3a".into(),
                attachment_id: Some("att-4".into()),
                part_id: "4".into(),
            }
        );
    }
}
