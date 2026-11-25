//! What the split step of account setup previews. It shows which inbox threads each candidate
//! split would take and what Important and Other would be left with, found by routing the inbox
//! Exo has with the candidates turned on.

use jiff::Timestamp;

use crate::db::{LabelRow, ListEntry};
use crate::error::Result;
use crate::ids::ThreadId;
use crate::ids::labels::{INBOX, SH_IMPORTANT, SH_OTHER};
use crate::split::{SplitInbox, Splits};
use crate::store::{Store, load_attachments_by_thread, load_views};
use crate::view::{Router, ThreadView};

/// A conversation as the preview lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEmail {
    /// The thread.
    pub id: ThreadId,
    /// The sender of its latest received message, by name, else by address.
    pub sender: String,
    /// Its subject.
    pub subject: String,
    /// When its latest message arrived.
    pub date: Timestamp,
}

/// The threads of one tab of the preview, newest first, and how many there are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewList {
    /// How many inbox threads it holds.
    pub count: usize,
    /// The newest of them, up to the limit asked for.
    pub threads: Vec<PreviewEmail>,
}

/// The preview in account setup. It holds a list for each candidate, in the candidates' order,
/// and the lists for Important and Other with every candidate on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitPreview {
    /// Each candidate's list, in the order asked for.
    pub splits: Vec<PreviewList>,
    /// Important's list (`SH_IMPORTANT`).
    pub important: PreviewList,
    /// Other's list (`SH_OTHER`).
    pub other: PreviewList,
}

impl Store {
    /// Routes the inbox the store has with `candidates` turned on, as well as the account's own
    /// splits, and lists the newest `limit` threads of each candidate, Important and Other.
    ///
    /// # Errors
    /// On database failures.
    pub async fn split_preview(
        &self,
        candidates: &[SplitInbox],
        limit: usize,
    ) -> Result<SplitPreview> {
        let mut db = self.inner.db.clone();
        let fields = ListEntry::fields();
        let entries = ListEntry::filter_by_list_id(INBOX)
            .order_by((fields.sort().desc(), fields.thread_id().desc()))
            .exec(&mut db)
            .await?;
        let ids: Vec<String> = entries
            .iter()
            .map(|entry| entry.thread_id.clone())
            .collect();
        let views = load_views(&mut db, &ids).await?;
        let attachments_by_thread = load_attachments_by_thread(&mut db, &views).await?;
        let labels = LabelRow::all().exec(&mut db).await?;
        let current = self.inner.router();
        let mut splits: Vec<SplitInbox> = current.splits.defined.clone();
        splits.extend(candidates.iter().cloned().map(|split| SplitInbox {
            is_disabled: false,
            ..split
        }));
        let router = Router {
            me: current.me,
            splits: std::sync::Arc::new(Splits::new(
                splits,
                labels
                    .iter()
                    .map(|label| (label.id.as_str(), label.name.as_str())),
            )),
        };
        let mut preview = SplitPreview {
            splits: vec![PreviewList::default(); candidates.len()],
            ..SplitPreview::default()
        };
        for id in &ids {
            let Some(view) = views.get(id) else { continue };
            let attachments = attachments_by_thread.get(id).map_or(&[][..], Vec::as_slice);
            let lists = router.list_ids(view, attachments);
            let add = |list: &mut PreviewList| {
                list.count += 1;
                if list.threads.len() < limit
                    && let Some(email) = preview_email(view, &router)
                {
                    list.threads.push(email);
                }
            };
            for (candidate, list) in candidates.iter().zip(preview.splits.iter_mut()) {
                if lists.contains(&candidate.id.as_str()) {
                    add(list);
                }
            }
            if lists.contains(&SH_IMPORTANT) {
                add(&mut preview.important);
            }
            if lists.contains(&SH_OTHER) {
                add(&mut preview.other);
            }
        }
        Ok(preview)
    }
}

/// `view` as the preview lists it, from its latest message from someone else, or else from its
/// latest message.
fn preview_email(view: &ThreadView, router: &Router) -> Option<PreviewEmail> {
    let shown = view.messages.iter().filter(|message| !message.is_draft());
    let received = shown
        .clone()
        .filter(|message| {
            message
                .row
                .sender
                .0
                .as_ref()
                .is_none_or(|sender| !router.me.is_me(&sender.email))
        })
        .max_by_key(|message| message.row.internal_date);
    let latest = received.or_else(|| shown.max_by_key(|message| message.row.internal_date))?;
    let sender = latest
        .row
        .sender
        .0
        .as_ref()
        .map(|sender| {
            sender
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| sender.email.clone())
        })
        .unwrap_or_default();
    Some(PreviewEmail {
        id: ThreadId::new(view.id.clone()),
        sender,
        subject: view.subject(),
        date: Timestamp::from_millisecond(latest.row.internal_date).ok()?,
    })
}
