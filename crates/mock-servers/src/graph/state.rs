//! What Graph keeps beside the Gmail mailbox: each message's Outlook identity (a key that
//! survives draft edits and sending, and a move count for its default ID), the change log delta
//! queries read, user folders, and the Outlook-only state Gmail has no place for.
//!
//! Gmail's history is the source of truth for messages. Before each Graph request,
//! [`Outlook::catch_up`] reads the history since the last request, so a message delivered by
//! `/__mock/deliver` or changed through Gmail's API shows up in the folder deltas.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use serde_json::{Map, Value};

use super::model::{self, Folder};
use crate::mail::{Mailbox, Selection};

/// The mailbox as Outlook sees it.
#[derive(Debug, Default)]
pub struct Outlook {
    /// The Gmail history ID read up to, and the history floor then.
    seen: u64,
    history_floor: u64,
    pub items: BTreeMap<u64, Item>,
    by_gmail: HashMap<String, u64>,
    next_key: u64,
    /// The change log's last sequence number, and the oldest a delta token may start from.
    pub seq: u64,
    pub floor: u64,
    log: Vec<Change>,
    folder_log: Vec<(u64, Folder)>,
    /// User folders, in creation order.
    pub folders: Vec<UserFolder>,
    /// Follow-up flags as set through Graph (their dates, and `complete`), by item.
    pub flags: HashMap<u64, Map<String, Value>>,
    /// Properties Outlook stores on a message that Gmail can't (`importance` and receipt requests
    /// on received mail), by item.
    pub stored: HashMap<u64, Map<String, Value>>,
    /// Labels taken off the master category list, which messages may still carry.
    pub off_master: BTreeSet<String>,
    /// Category colors set through Graph, by label.
    pub colors: HashMap<String, String>,
    pub overrides: Vec<Override>,
    pub rules: BTreeMap<String, Rule>,
    pub settings: Map<String, Value>,
    /// The external auto-reply, when it differs from the internal one (Gmail has one).
    pub external_reply: Option<String>,
    pub uploads: HashMap<u64, Upload>,
    pub next_upload: u64,
    /// Properties Outlook keeps on an event that the calendar can't (`categories`, `importance`,
    /// a Teams meeting…), by calendar and event ID.
    pub events: HashMap<(String, String), Map<String, Value>>,
    /// Calendar view delta rounds, which their tokens name by index.
    pub rounds: Vec<Round>,
    pub sessions: super::login::Sessions,
    /// Graph requests being served, for the concurrency limit.
    pub in_flight: u32,
    pub requests: u64,
}

/// A message as Outlook knows it.
#[derive(Debug, Clone)]
pub struct Item {
    /// The Gmail message that holds it now. Saving a draft or sending it makes a new one.
    pub gmail: String,
    pub labels: BTreeSet<String>,
    pub folder: Folder,
    /// How many times it has changed folder, which its default ID carries.
    pub generation: u32,
    pub created: Timestamp,
    pub modified: Timestamp,
    pub version: u64,
}

/// Where an item was, for a delta's `@removed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub folder: Folder,
    pub generation: u32,
}

#[derive(Debug, Clone)]
struct Change {
    seq: u64,
    key: u64,
    before: Option<Place>,
    after: Option<Place>,
}

/// One item's changes over a delta window.
#[derive(Debug, Clone)]
pub struct Delta {
    pub before: Option<Place>,
    pub after: Option<Place>,
}

#[derive(Debug, Clone)]
pub struct UserFolder {
    /// The Gmail label that holds its messages.
    pub label: String,
    pub parent: Folder,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct Override {
    pub id: String,
    pub focused: bool,
    pub name: Option<String>,
    pub address: String,
}

/// A message rule's Outlook-only parts. Gmail's filter holds what it does; a disabled rule's
/// filter waits here, off Gmail's list, until it's enabled again.
#[derive(Debug, Clone, Default)]
pub struct Rule {
    pub display_name: Option<String>,
    pub sequence: Option<i64>,
    pub conditions: Option<Value>,
    pub actions: Option<Value>,
    pub disabled: Option<gmail::Filter>,
}

/// One round of a calendar view's delta: what it sends, and what the view held at its end.
#[derive(Debug, Clone)]
pub struct Round {
    /// The calendar, and the view's window.
    pub calendar: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// The calendar's revision when the round was taken.
    pub revision: u64,
    /// The round's entries, in order: an event ID and whether it was removed.
    pub entries: Vec<(String, bool)>,
    /// The view's events at the round's end, by event ID, with their change keys.
    pub seen: BTreeMap<String, String>,
}

/// A large attachment being uploaded to a draft.
#[derive(Debug, Clone)]
pub struct Upload {
    pub key: u64,
    /// The uploader, as the session's URL names them (`{id}@{tenant}`).
    pub user: String,
    pub message_id: String,
    pub token: String,
    pub name: String,
    pub content_type: String,
    pub inline: bool,
    pub content_id: Option<String>,
    pub size: usize,
    pub bytes: Vec<u8>,
    pub expires: Timestamp,
}

impl Outlook {
    /// Outlook's view of `mail` as built: every message an item, keyed newest first, with
    /// `folders` as its user folders (and every other user label a category).
    pub fn new(mail: &Mailbox, folders: Vec<UserFolder>) -> Self {
        let mut outlook = Self {
            seen: mail.history.current(),
            history_floor: mail.history.floor(),
            folders,
            ..Self::default()
        };
        let folder_labels = outlook.folder_labels();
        let all = mail.select_messages(&Selection {
            query: None,
            label_ids: Vec::new(),
            include_spam_trash: true,
        });
        for message in all {
            let at = Timestamp::from_millisecond(message.internal_date).unwrap_or_default();
            outlook.next_key += 1;
            let key = outlook.next_key;
            outlook.by_gmail.insert(message.id.clone(), key);
            outlook.items.insert(
                key,
                Item {
                    gmail: message.id.clone(),
                    folder: model::folder_of(&message.labels, &folder_labels),
                    labels: message.labels.clone(),
                    generation: 0,
                    created: at,
                    modified: at,
                    version: 1,
                },
            );
        }
        outlook
    }

    /// The labels of user folders, in creation order.
    pub fn folder_labels(&self) -> Vec<String> {
        self.folders
            .iter()
            .map(|folder| folder.label.clone())
            .collect()
    }

    pub fn key_of(&self, gmail: &str) -> Option<u64> {
        self.by_gmail.get(gmail).copied()
    }

    /// Reads Gmail's history since the last request into the change log. When history has
    /// expired (`/__mock/history/expire`), every message is compared instead, and every delta
    /// token given so far expires with it.
    pub fn catch_up(&mut self, mail: &mut Mailbox, now: Timestamp) {
        let expired = mail.history.floor() != self.history_floor;
        let touched: BTreeSet<String> = match mail.history.since(self.seen) {
            Some(records) if !expired => records.map(|record| record.message_id.clone()).collect(),
            _ => {
                self.history_floor = mail.history.floor();
                self.floor = self.seq;
                let all = mail.select_messages(&Selection {
                    query: None,
                    label_ids: Vec::new(),
                    include_spam_trash: true,
                });
                all.into_iter()
                    .map(|message| message.id.clone())
                    .chain(self.by_gmail.keys().cloned())
                    .collect()
            }
        };
        self.seen = mail.history.current();
        self.sync_folders(mail);
        for gmail in touched {
            self.reconcile(mail, &gmail, now);
        }
    }

    /// Folders whose label Gmail deleted are gone.
    fn sync_folders(&mut self, mail: &Mailbox) {
        let gone: Vec<String> = self
            .folders
            .iter()
            .filter(|folder| {
                !mail
                    .labels()
                    .iter()
                    .any(|label| label.id.as_deref() == Some(&folder.label))
            })
            .map(|folder| folder.label.clone())
            .collect();
        for label in gone {
            self.folders.retain(|folder| folder.label != label);
            self.folder_changed(Folder::User(label));
        }
    }

    fn reconcile(&mut self, mail: &mut Mailbox, gmail: &str, now: Timestamp) {
        let folders = self.folder_labels();
        let message = mail
            .message(gmail)
            .ok()
            .map(|message| message.labels.clone());
        match (message, self.by_gmail.get(gmail).copied()) {
            (Some(labels), Some(key)) => {
                let Some(item) = self.items.get_mut(&key) else {
                    return;
                };
                if item.labels == labels {
                    return;
                }
                let before = Place {
                    folder: item.folder.clone(),
                    generation: item.generation,
                };
                item.labels = labels;
                let folder = model::folder_of(&item.labels, &folders);
                if folder != item.folder {
                    item.folder = folder;
                    item.generation += 1;
                }
                self.log(key, Some(before), now);
            }
            (Some(mut labels), None) => {
                if let Some(focused) = self.override_for(mail, gmail, &labels) {
                    let (add, remove) = model::classify(&labels, focused);
                    mail.apply_labels(gmail, &add, &remove);
                    labels = mail
                        .message(gmail)
                        .map(|m| m.labels.clone())
                        .unwrap_or(labels);
                    self.seen = mail.history.current();
                }
                self.next_key += 1;
                let key = self.next_key;
                self.by_gmail.insert(gmail.to_owned(), key);
                self.items.insert(
                    key,
                    Item {
                        gmail: gmail.to_owned(),
                        folder: model::folder_of(&labels, &folders),
                        labels,
                        generation: 0,
                        created: now,
                        modified: now,
                        version: 0,
                    },
                );
                self.log(key, None, now);
            }
            (None, Some(key)) => {
                self.by_gmail.remove(gmail);
                if let Some(item) = self.items.remove(&key) {
                    self.seq += 1;
                    self.log.push(Change {
                        seq: self.seq,
                        key,
                        before: Some(Place {
                            folder: item.folder,
                            generation: item.generation,
                        }),
                        after: None,
                    });
                }
            }
            (None, None) => {}
        }
    }

    /// An inbox override's classification for a newly delivered message from its sender.
    fn override_for(&self, mail: &Mailbox, gmail: &str, labels: &BTreeSet<String>) -> Option<bool> {
        if !labels.contains("INBOX") || labels.contains("SENT") || labels.contains("DRAFT") {
            return None;
        }
        let from = mail
            .message(gmail)
            .ok()?
            .mime
            .payload
            .header("From")?
            .to_owned();
        let address = gmail::mime::Mailbox::parse(&from).email;
        self.overrides
            .iter()
            .find(|rule| rule.address.eq_ignore_ascii_case(&address))
            .map(|rule| rule.focused)
    }

    /// Records that `key` changed, after being at `before`.
    fn log(&mut self, key: u64, before: Option<Place>, now: Timestamp) {
        let Some(item) = self.items.get_mut(&key) else {
            return;
        };
        item.modified = now;
        item.version += 1;
        self.seq += 1;
        self.log.push(Change {
            seq: self.seq,
            key,
            before,
            after: Some(Place {
                folder: item.folder.clone(),
                generation: item.generation,
            }),
        });
    }

    /// Records a change Gmail doesn't see (a completed flag, a stored property, a draft's new
    /// content).
    pub fn touch(&mut self, key: u64, now: Timestamp) {
        let before = self.items.get(&key).map(|item| Place {
            folder: item.folder.clone(),
            generation: item.generation,
        });
        if before.is_some() {
            self.log(key, before, now);
        }
    }

    /// Points `key` at the Gmail message that now holds it, as saving or sending a draft makes.
    pub fn repoint(&mut self, key: u64, gmail: &str) {
        if let Some(item) = self.items.get_mut(&key) {
            self.by_gmail.remove(&item.gmail);
            item.gmail = gmail.to_owned();
            self.by_gmail.insert(gmail.to_owned(), key);
        }
    }

    /// Records a folder created, renamed or deleted.
    pub fn folder_changed(&mut self, folder: Folder) {
        self.seq += 1;
        self.folder_log.push((self.seq, folder));
    }

    /// Each item's changes after `since` up to `until`: where it was before the first, and after
    /// the last.
    pub fn changes(&self, since: u64, until: u64) -> BTreeMap<u64, Delta> {
        let first = self.log.partition_point(|change| change.seq <= since);
        let mut deltas: BTreeMap<u64, Delta> = BTreeMap::new();
        for change in self.log[first..]
            .iter()
            .take_while(|change| change.seq <= until)
        {
            deltas
                .entry(change.key)
                .and_modify(|delta| delta.after = change.after.clone())
                .or_insert_with(|| Delta {
                    before: change.before.clone(),
                    after: change.after.clone(),
                });
        }
        deltas
    }

    /// Folders created, renamed or deleted after `since` up to `until`.
    pub fn folder_changes(&self, since: u64, until: u64) -> BTreeSet<Folder> {
        self.folder_log
            .iter()
            .filter(|(seq, _)| *seq > since && *seq <= until)
            .map(|(_, folder)| folder.clone())
            .collect()
    }

    /// The next request's ID, for `request-id` and `innerError`.
    pub fn next_request(&mut self) -> u64 {
        self.requests += 1;
        self.requests
    }
}
