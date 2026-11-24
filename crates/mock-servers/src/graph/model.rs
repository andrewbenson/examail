//! How the mailbox's Gmail labels become Outlook's model: one folder per message, read state,
//! the follow-up flag, categories and Focused Inbox.

use std::collections::BTreeSet;

/// A mail folder: one of Outlook's well-known ones, or a user folder backed by a Gmail label.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Folder {
    /// "Top of Information Store", the parent of the folders mail clients show.
    Root,
    Inbox,
    Drafts,
    SentItems,
    DeletedItems,
    JunkEmail,
    Archive,
    Outbox,
    ConversationHistory,
    /// A user folder, by the ID of the Gmail label that holds its messages.
    User(String),
}

/// The well-known folders the mailbox has, with their well-known names and display names. The
/// names work in place of IDs, in any case.
pub const WELL_KNOWN: &[(Folder, &str, &str)] = &[
    (Folder::Root, "msgfolderroot", "Top of Information Store"),
    (Folder::Archive, "archive", "Archive"),
    (
        Folder::ConversationHistory,
        "conversationhistory",
        "Conversation History",
    ),
    (Folder::DeletedItems, "deleteditems", "Deleted Items"),
    (Folder::Drafts, "drafts", "Drafts"),
    (Folder::Inbox, "inbox", "Inbox"),
    (Folder::JunkEmail, "junkemail", "Junk Email"),
    (Folder::Outbox, "outbox", "Outbox"),
    (Folder::SentItems, "sentitems", "Sent Items"),
];

impl Folder {
    /// The folder a well-known name names.
    pub fn well_known(name: &str) -> Option<Self> {
        WELL_KNOWN
            .iter()
            .find(|(_, known, _)| known.eq_ignore_ascii_case(name))
            .map(|(folder, ..)| folder.clone())
    }

    /// A well-known folder's index in [`WELL_KNOWN`], which its ID encodes.
    pub fn index(&self) -> Option<usize> {
        WELL_KNOWN.iter().position(|(folder, ..)| folder == self)
    }

    /// A well-known folder's display name.
    pub fn display_name(&self) -> Option<&'static str> {
        WELL_KNOWN
            .iter()
            .find(|(folder, ..)| folder == self)
            .map(|(.., name)| *name)
    }
}

/// Gmail's category tabs, which Focused Inbox reads.
pub const CATEGORIES: &[&str] = &[
    "CATEGORY_PERSONAL",
    "CATEGORY_SOCIAL",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

/// The folder a message with `labels` sits in. A message carries many labels but sits in one
/// folder, so the first that applies wins: TRASH (Deleted Items), SPAM (Junk Email), DRAFT
/// (Drafts), INBOX (Inbox), a user folder's label (the earliest created), SENT (Sent Items), and
/// otherwise Archive, where archived Gmail mail goes.
pub fn folder_of(labels: &BTreeSet<String>, user_folders: &[String]) -> Folder {
    let has = |label: &str| labels.contains(label);
    if has("TRASH") {
        Folder::DeletedItems
    } else if has("SPAM") {
        Folder::JunkEmail
    } else if has("DRAFT") {
        Folder::Drafts
    } else if has("INBOX") {
        Folder::Inbox
    } else if let Some(label) = user_folders.iter().find(|label| has(label)) {
        Folder::User(label.clone())
    } else if has("SENT") {
        Folder::SentItems
    } else {
        Folder::Archive
    }
}

/// The label changes that put a message into `target` under [`folder_of`]'s precedence, or
/// `None` for folders no message can be moved into (the root, Drafts, Outbox and Conversation
/// History).
pub fn move_into(target: &Folder, user_folders: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let mut remove: Vec<String> = ["INBOX", "TRASH", "SPAM"].map(String::from).to_vec();
    let mut add = Vec::new();
    match target {
        Folder::Inbox => {
            remove.retain(|label| label != "INBOX");
            add.push("INBOX".to_owned());
        }
        Folder::DeletedItems => {
            remove.retain(|label| label != "TRASH");
            add.push("TRASH".to_owned());
        }
        Folder::JunkEmail => {
            remove.retain(|label| label != "SPAM");
            add.push("SPAM".to_owned());
        }
        Folder::SentItems => add.push("SENT".to_owned()),
        Folder::Archive => remove.push("SENT".to_owned()),
        Folder::User(label) => add.push(label.clone()),
        Folder::Root | Folder::Drafts | Folder::Outbox | Folder::ConversationHistory => {
            return None;
        }
    }
    // A message sits in one user folder. Leaving Deleted Items or Junk Email keeps its folder
    // label only when it goes back to that folder.
    remove.extend(
        user_folders
            .iter()
            .filter(|label| !add.contains(label))
            .cloned(),
    );
    Some((add, remove))
}

/// Focused Inbox's classification: mail Gmail files under Primary (its personal category, or no
/// category at all) or marks important is focused, and the rest is other.
pub fn is_focused(labels: &BTreeSet<String>) -> bool {
    labels.contains("IMPORTANT")
        || labels.contains("CATEGORY_PERSONAL")
        || !CATEGORIES.iter().any(|category| labels.contains(*category))
}

/// The label changes that classify a message as focused or other under [`is_focused`].
pub fn classify(labels: &BTreeSet<String>, focused: bool) -> (Vec<String>, Vec<String>) {
    if focused {
        let remove = CATEGORIES[1..].iter().map(|c| (*c).to_owned()).collect();
        (vec!["CATEGORY_PERSONAL".to_owned()], remove)
    } else {
        let has_other = CATEGORIES[1..]
            .iter()
            .any(|category| labels.contains(*category));
        let add = if has_other {
            Vec::new()
        } else {
            vec!["CATEGORY_UPDATES".to_owned()]
        };
        (
            add,
            vec!["CATEGORY_PERSONAL".to_owned(), "IMPORTANT".to_owned()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn one_folder_by_precedence() {
        let folders = ["Label_20".to_owned()];
        let cases: &[(&[&str], Folder)] = &[
            (&["INBOX", "TRASH", "UNREAD"], Folder::DeletedItems),
            (&["SPAM", "INBOX"], Folder::JunkEmail),
            (&["DRAFT", "Label_20"], Folder::Drafts),
            (&["INBOX", "SENT", "Label_20"], Folder::Inbox),
            (&["SENT", "Label_20"], Folder::User("Label_20".into())),
            (&["SENT", "Label_1"], Folder::SentItems),
            (&["Label_1", "IMPORTANT"], Folder::Archive),
        ];
        for (ids, folder) in cases {
            assert_eq!(&folder_of(&labels(ids), &folders), folder, "{ids:?}");
        }
    }

    #[test]
    fn moves_land_where_they_aim() {
        let folders = ["Label_20".to_owned(), "Label_21".to_owned()];
        let start = labels(&["SENT", "Label_21", "UNREAD"]);
        for target in [
            Folder::Inbox,
            Folder::Archive,
            Folder::DeletedItems,
            Folder::JunkEmail,
            Folder::SentItems,
            Folder::User("Label_20".into()),
        ] {
            let (add, remove) = move_into(&target, &folders).unwrap();
            let mut moved = start.clone();
            moved.retain(|label| !remove.contains(label));
            moved.extend(add);
            assert_eq!(folder_of(&moved, &folders), target);
            assert!(moved.contains("UNREAD"), "moves keep read state");
        }
        assert!(move_into(&Folder::Drafts, &folders).is_none());
    }

    #[test]
    fn focused_follows_primary_and_importance() {
        assert!(is_focused(&labels(&["CATEGORY_PERSONAL"])));
        assert!(is_focused(&labels(&["SENT"])), "no category is Primary");
        assert!(is_focused(&labels(&["CATEGORY_PROMOTIONS", "IMPORTANT"])));
        assert!(!is_focused(&labels(&["CATEGORY_PROMOTIONS"])));
        for (start, focused) in [
            (labels(&["CATEGORY_PROMOTIONS"]), true),
            (labels(&["CATEGORY_PERSONAL", "IMPORTANT"]), false),
        ] {
            let (add, remove) = classify(&start, focused);
            let mut changed = start.clone();
            changed.retain(|label| !remove.contains(label));
            changed.extend(add);
            assert_eq!(is_focused(&changed), focused);
        }
    }
}
