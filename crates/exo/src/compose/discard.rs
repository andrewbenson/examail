//! Discard Draft(s) from the list or a thread. `⌘⇧,` discards every draft in the conversations
//! that actions apply to and shows a toast. Undo brings them all back, and a single draft reopens
//! where it was written.

use std::collections::{HashMap, HashSet};

use exo_commands::{Command, Commands, Region, Toast};
use exo_store::{MessageDetail, StoredDraft, ThreadDetail, ThreadId};

use super::{Action, Discarded, Open, Surface, adopted, discard_open, save};
use crate::{Mail, commands, thread::ThreadView};

/// Pending batches of already-discarded drafts waiting on unopened foreign draft threads.
#[derive(Default)]
pub(super) struct Reads {
    next_batch: u64,
    pending: HashMap<u64, Vec<Open>>,
}

/// The regions of the list and the thread. In a draft, compose's own `Discard Draft` has the keys.
const REGIONS: &[Region] = &[
    Region::ThreadList,
    Region::ThreadMessagesHeader,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ActionPane,
    Region::ReadOnlySnippet,
];

/// Registers `Discard Draft(s)` (`DISCARD_DRAFT`) while `threads` of the conversations that
/// actions apply to have drafts, in the plural when more than one does. For a selection that
/// reaches rows not loaded (`exact` false), registers only its keys, which say why it can't.
pub(crate) fn register(commands: &mut Commands<commands::Action>, threads: usize, exact: bool) {
    if threads == 0 {
        return;
    }
    let action = commands::Action::Compose(Action::DiscardListed);
    let keys = ["mod+shift+,", "mod+shift+."];
    if !exact {
        commands.add(
            Command::shortcut("DISCARD_DRAFT", action)
                .keys(keys)
                .regions(REGIONS),
        );
        return;
    }
    let (title, aliases) = if threads > 1 {
        (
            "Discard Drafts",
            ["Delete Drafts", "Trash Drafts", "Abandon Drafts"],
        )
    } else {
        (
            "Discard Draft",
            ["Delete Draft", "Trash Draft", "Abandon Draft"],
        )
    };
    commands.add(
        Command::new("DISCARD_DRAFT", title, action)
            .keys(keys)
            .key_code(["⌘", "shift", ","])
            .aliases(aliases)
            .default_score(6.1)
            .scale(1.1)
            .icon("data/discard")
            .multiple()
            .regions(REGIONS),
    );
}

impl Mail {
    /// Drafts in the conversations that actions apply to, leaving out sends being held, and how
    /// many conversations have one (Exo's stored drafts, open drafts in compose, and foreign
    /// Gmail drafts in the open conversation or listed rows).
    pub(crate) fn actionable_drafts(&self) -> (Vec<StoredDraft>, usize) {
        let rows = crate::actionable(self.thread.as_ref(), &self.inbox);
        let ids: Vec<String> = match &self.thread {
            Some(open) if rows.is_empty() => vec![open.id().to_owned()],
            _ => rows.iter().map(|thread| thread.id.clone()).collect(),
        };
        let sending: HashSet<&str> = self
            .compose
            .sends
            .iter()
            .map(|sending| sending.open.id.as_str())
            .collect();
        let drafts: Vec<StoredDraft> = self
            .snapshot
            .drafts
            .iter()
            .filter(|draft| {
                draft.send_at.is_none()
                    && !sending.contains(draft.id.as_str())
                    && draft
                        .thread
                        .as_ref()
                        .is_some_and(|thread| ids.iter().any(|id| id == thread.as_str()))
            })
            .cloned()
            .collect();
        let mut threads: HashSet<&str> = drafts
            .iter()
            .filter_map(|draft| draft.thread.as_ref().map(ThreadId::as_str))
            .collect();
        for open in &self.compose.drafts {
            if open.persists_to_gmail()
                && !sending.contains(open.id.as_str())
                && let Some(thread) = open
                    .thread
                    .as_deref()
                    .filter(|thread| ids.iter().any(|id| id == *thread))
            {
                threads.insert(thread);
            }
        }
        if let Some(detail) = self.open_thread_detail(&ids) {
            let ours = self.our_draft_messages();
            if detail
                .messages
                .iter()
                .any(|message| message.is_draft() && !ours.contains(message.id.as_str()))
            {
                threads.insert(detail.id.as_str());
            }
        }
        for row in &rows {
            if row.drafts > 0 {
                threads.insert(row.id.as_str());
            }
        }
        let count = threads.len();
        (drafts, count)
    }

    /// Gmail message ids of every draft `exo-store` already tracks.
    fn our_draft_messages(&self) -> HashSet<&str> {
        self.snapshot
            .drafts
            .iter()
            .filter_map(|draft| draft.message_id.as_deref())
            .collect()
    }

    /// The open thread's [`ThreadDetail`], when it's one of `ids`.
    fn open_thread_detail(&self, ids: &[String]) -> Option<&ThreadDetail> {
        let detail = self
            .thread
            .as_ref()
            .and_then(ThreadView::detail)
            .or(self.snapshot.thread.as_deref())?;
        ids.iter()
            .any(|id| id == detail.id.as_str())
            .then_some(detail)
    }

    /// `⌘⇧,` in the list or a thread. Discards the drafts of the actionable conversations or, for
    /// a selection that reaches rows not loaded, says it can't.
    pub(crate) fn discard_listed(&mut self) {
        if self.refuse_unloaded("discard drafts") {
            return;
        }
        let rows = crate::actionable(self.thread.as_ref(), &self.inbox);
        let row_counts: Vec<(String, usize)> = rows
            .iter()
            .map(|row| (row.id.clone(), row.drafts))
            .collect();
        let ids: Vec<String> = match &self.thread {
            Some(open) if row_counts.is_empty() => vec![open.id().to_owned()],
            _ => row_counts.iter().map(|(id, _)| id.clone()).collect(),
        };
        let (drafts, threads) = self.actionable_drafts();
        if threads == 0 {
            return;
        }
        let sending: HashSet<String> = self
            .compose
            .sends
            .iter()
            .map(|sending| sending.open.id.clone())
            .collect();
        let aliases = self.snapshot.aliases.clone();
        let mut discarded: Vec<Open> = Vec::new();
        let mut adopted_messages: HashSet<String> = HashSet::new();
        for draft in &drafts {
            // A draft open in compose closes there.
            let open = match self
                .compose
                .drafts
                .iter()
                .position(|open| open.id == draft.id)
            {
                Some(at) => {
                    let mut open = self.compose.drafts.remove(at);
                    open.sync_body();
                    open
                }
                None => Open::stored(draft),
            };
            self.clear_discarded_focus(&open.id);
            self.account.discard_draft(&draft.id);
            discarded.push(open);
        }
        // Open drafts not yet in `snapshot.drafts` (such as an untouched adopted Gmail draft, or
        // an inline draft before its first save).
        let mut at = 0;
        while at < self.compose.drafts.len() {
            let matches = self.compose.drafts[at].persists_to_gmail()
                && !sending.contains(&self.compose.drafts[at].id)
                && self.compose.drafts[at]
                    .thread
                    .as_deref()
                    .is_some_and(|thread| ids.iter().any(|id| id == thread));
            if !matches {
                at += 1;
                continue;
            }
            let mut open = self.compose.drafts.remove(at);
            open.sync_body();
            if let Some(message) = open.adopted_message() {
                adopted_messages.insert(message.to_owned());
            }
            self.clear_discarded_focus(&open.id);
            discard_open(&mut open, &self.account, &aliases, &self.now);
            discarded.push(open);
        }
        // Foreign Gmail draft messages in the open thread that compose hasn't adopted yet.
        let open_id = self
            .open_thread_detail(&ids)
            .map(|detail| detail.id.as_str().to_owned());
        if let Some(detail) = self.open_thread_detail(&ids).cloned() {
            let ours: HashSet<String> = self
                .our_draft_messages()
                .into_iter()
                .map(str::to_owned)
                .collect();
            for message in detail.messages.iter().filter(|message| {
                message.is_draft()
                    && !ours.contains(&message.id)
                    && !adopted_messages.contains(&message.id)
            }) {
                let mut open = adopted(message, &detail);
                discard_open(&mut open, &self.account, &aliases, &self.now);
                discarded.push(open);
            }
        }
        // Unopened rows in Drafts whose `row.drafts` includes foreign Gmail drafts.
        let missing: Vec<String> = row_counts
            .into_iter()
            .filter(|(id, expected)| {
                *expected > 0
                    && open_id.as_deref() != Some(id.as_str())
                    && discarded
                        .iter()
                        .filter(|open| open.thread.as_deref() == Some(id.as_str()))
                        .count()
                        < *expected
            })
            .map(|(id, _)| id)
            .collect();
        if missing.is_empty() {
            self.finish_discard_listed(discarded);
            return;
        }
        self.compose.discard_reads.next_batch += 1;
        let batch = self.compose.discard_reads.next_batch;
        self.compose.discard_reads.pending.insert(batch, discarded);
        let ui = self.ui_sender();
        self.account.loader().run(
            move |store| async move {
                let mut details = Vec::with_capacity(missing.len());
                for id in missing {
                    let thread = ThreadId::new(id);
                    let _ = store.load_bodies(&thread).await;
                    if let Ok(Some(detail)) = store.thread(&thread).await {
                        details.push(detail);
                    }
                }
                details
            },
            move |details| {
                ui.send(move |mail| mail.complete_discard_listed(batch, details));
            },
        );
    }

    /// Finishes batch `batch` of `Discard Draft(s)` once unopened foreign draft threads have been
    /// read from the store.
    fn complete_discard_listed(&mut self, batch: u64, details: Vec<ThreadDetail>) {
        let mut discarded = self
            .compose
            .discard_reads
            .pending
            .remove(&batch)
            .unwrap_or_default();
        let aliases = self.snapshot.aliases.clone();
        let ours: HashSet<String> = self
            .our_draft_messages()
            .into_iter()
            .map(str::to_owned)
            .collect();
        for detail in &details {
            for message in detail
                .messages
                .iter()
                .filter(|message| message.is_draft() && !ours.contains(&message.id))
            {
                let mut open = adopted(message, detail);
                discard_open(&mut open, &self.account, &aliases, &self.now);
                discarded.push(open);
            }
        }
        self.finish_discard_listed(discarded);
    }

    fn clear_discarded_focus(&mut self, id: &str) {
        if self.compose.focus.as_deref() == Some(id) {
            self.compose.focus = None;
        }
        if self.compose.popped.as_deref() == Some(id) {
            self.compose.popped = None;
        }
    }

    fn finish_discard_listed(&mut self, mut discarded: Vec<Open>) {
        if discarded.is_empty() {
            return;
        }
        let empty_open_thread = self
            .thread
            .as_ref()
            .and_then(ThreadView::detail)
            .is_some_and(|detail| {
                !detail.messages.is_empty()
                    && detail.messages.iter().all(MessageDetail::is_draft)
                    && discarded
                        .iter()
                        .any(|open| open.thread.as_deref() == Some(detail.id.as_str()))
            });
        if empty_open_thread {
            self.close_thread();
        }
        let tag = self.compose.tag();
        let toast = match discarded.len() {
            1 => {
                let open = discarded.remove(0);
                // Undo reopens the draft where drafts open: a new message (including an
                // edit-subject reply fork) in its pane, a reply under its message.
                let surface = if open.is_new_email() {
                    Surface::Pane
                } else {
                    Surface::Inline
                };
                self.compose.discarded = Some(Discarded { tag, open, surface });
                Toast::new("Draft discarded.")
                    .undo(commands::Action::Compose(Action::UndoDiscard(tag)))
            }
            count => {
                self.compose.discards = Some((tag, discarded));
                Toast::new(format!("{count} drafts discarded."))
                    .undo(commands::Action::Compose(Action::UndoDiscards(tag)))
            }
        };
        self.commands.toast(toast.tag(tag));
    }

    /// The toast's Undo on several discarded drafts saves each one again.
    pub(crate) fn undo_discards(&mut self, tag: u64) {
        let Some((_, drafts)) = self
            .compose
            .discards
            .take_if(|(discarded, _)| *discarded == tag)
        else {
            return;
        };
        let aliases = self.snapshot.aliases.clone();
        for mut open in drafts {
            save(&mut open, &self.account, &aliases, &self.now, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };

    use exo_store::{Body, Contact, DraftAction, MessageDetail, ThreadDetail, ThreadId};
    use jiff::Timestamp;

    use super::*;
    use crate::{
        Clock, Mail,
        account::{Account, Credentials},
    };

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_mail(name: &str) -> (Mail, TempDir) {
        let ctx = egui::Context::default();
        let _ = exo_ui::install(&ctx);
        let dir = std::env::temp_dir().join(format!(
            "exo-discard-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints =
            gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
        let account = Account::open(
            &dir,
            Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints,
                tokens: Arc::new(gmail::StaticToken("dev".into())),
            },
            ctx.clone(),
        )
        .expect("account opens");
        let mail = Mail::new(&ctx, &crate::assets_dir(), account, Clock::system());
        (mail, TempDir(dir))
    }

    fn thread_with_foreign_reply_draft() -> ThreadDetail {
        let received = MessageDetail {
            id: "msg-original".into(),
            labels: vec!["INBOX".into()],
            date: Timestamp::UNIX_EPOCH,
            from: Some(Contact {
                name: Some("Tom Becker".into()),
                email: "tom@brightmoor.co".into(),
                is_me: false,
            }),
            to: vec![Contact {
                name: Some("Maya Okafor".into()),
                email: "maya@brightmoor.co".into(),
                is_me: true,
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Quarterly plan".into(),
            snippet: "Thoughts?".into(),
            rfc822_id: Some("orig@brightmoor.co".into()),
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: Body {
                html: Some("<div>Thoughts?</div>".into()),
                text: Some("Thoughts?".into()),
            },
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        };
        let draft = MessageDetail {
            id: "msg-gmail-draft".into(),
            labels: vec!["DRAFT".into()],
            date: Timestamp::UNIX_EPOCH,
            from: Some(Contact {
                name: Some("Maya Okafor".into()),
                email: "maya@brightmoor.co".into(),
                is_me: true,
            }),
            to: vec![Contact {
                name: Some("Tom Becker".into()),
                email: "tom@brightmoor.co".into(),
                is_me: false,
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Re: Quarterly plan".into(),
            snippet: "Looks good".into(),
            rfc822_id: Some("draft@brightmoor.co".into()),
            in_reply_to: Some("orig@brightmoor.co".into()),
            references: vec!["orig@brightmoor.co".into()],
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: Body {
                html: Some("<div>Looks good</div>".into()),
                text: Some("Looks good".into()),
            },
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        };
        ThreadDetail {
            id: ThreadId::new("thread-1"),
            subject: "Quarterly plan".into(),
            labels: vec!["INBOX".into(), "DRAFT".into()],
            muted: false,
            reminder: None,
            messages: vec![received, draft],
        }
    }

    fn wait_for_draft_row(dir: &TempDir, id: &str, expected_discarded: bool) -> Option<String> {
        let db_path = dir.0.join("maya@brightmoor.co.sqlite3");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(conn) = rusqlite::Connection::open(&db_path)
                && let Ok((message_id, discarded)) = conn.query_row(
                    "SELECT message_id, discarded FROM drafts WHERE id = ?1",
                    [id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?)),
                )
                && discarded == expected_discarded
            {
                return message_id;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for draft {id} (discarded={expected_discarded})");
    }

    #[test]
    fn discarding_focused_unedited_adopted_draft_persists_and_discards_with_undo() {
        let (mut mail, dir) = test_mail("focused-adopted");
        let detail = thread_with_foreign_reply_draft();
        let open = adopted(&detail.messages[1], &detail);
        let id = open.id.clone();
        assert_eq!(open.adopted_message(), Some("msg-gmail-draft"));
        assert!(open.is_stored());

        mail.compose.focus = Some(id.clone());
        mail.compose.drafts.push(open);
        mail.compose_run(Action::Discard, 0.0);

        assert_eq!(mail.toast_text(), Some("Draft discarded."));
        let message_id = wait_for_draft_row(&dir, &id, true);
        assert_eq!(
            message_id.as_deref(),
            Some("msg-gmail-draft"),
            "adopted Gmail message id is recorded so push deletes the Gmail draft"
        );

        let tag = mail.compose.discarded.as_ref().expect("discarded").tag;
        mail.compose_run(Action::UndoDiscard(tag), 0.0);
        let restored = wait_for_draft_row(&dir, &id, false);
        assert_eq!(restored.as_deref(), Some("msg-gmail-draft"));
        assert_eq!(mail.compose.focus.as_deref(), Some(id.as_str()));
        mail.discard();
    }

    #[test]
    fn changing_reply_subject_on_unedited_adopted_draft_discards_original_and_forks() {
        let (mut mail, dir) = test_mail("fork-adopted");
        let detail = thread_with_foreign_reply_draft();
        let open = adopted(&detail.messages[1], &detail);
        let original_id = open.id.clone();

        mail.compose.focus = Some(original_id.clone());
        mail.compose.drafts.push(open);
        mail.compose_run(Action::EditSubject, 1.0);

        let message_id = wait_for_draft_row(&dir, &original_id, true);
        assert_eq!(
            message_id.as_deref(),
            Some("msg-gmail-draft"),
            "original adopted reply is saved and marked discarded"
        );
        let forked = mail.compose.focused().expect("forked draft has focus");
        assert_ne!(forked.id, original_id);
        assert_eq!(forked.record.action, DraftAction::EditSubjectLineReply);
        assert!(forked.record.in_reply_to.is_none());
        assert!(forked.header.select_subject);
        assert!(mail.compose_popped_out());
        mail.discard();
    }

    #[test]
    fn discard_listed_discards_untouched_foreign_draft_in_open_thread() {
        let (mut mail, dir) = test_mail("listed-adopted");
        let detail = Arc::new(thread_with_foreign_reply_draft());
        let mut view = ThreadView::new(
            "thread-1".into(),
            mail.now.clone(),
            "maya@brightmoor.co".into(),
            mail.images.clone(),
        );
        view.set_detail(detail);
        mail.thread = Some(view);
        mail.compose_sync();

        assert_eq!(
            mail.compose.drafts.len(),
            1,
            "foreign draft adopted on open"
        );
        assert!(mail.compose.focus.is_none(), "messages keep the keyboard");
        assert_eq!(mail.actionable_drafts().1, 1);

        let adopted_id = mail.compose.drafts[0].id.clone();
        mail.discard_listed();

        assert!(mail.compose.drafts.is_empty());
        assert_eq!(mail.toast_text(), Some("Draft discarded."));
        let message_id = wait_for_draft_row(&dir, &adopted_id, true);
        assert_eq!(message_id.as_deref(), Some("msg-gmail-draft"));
        mail.discard();
    }

    #[test]
    fn edit_subject_reply_discards_with_pane_undo_surface_and_batches_stay_isolated() {
        let (mut mail, _dir) = test_mail("edit-subject-and-batches");
        let make_draft = |id: &str, action: DraftAction| {
            let record = exo_store::DraftRecord {
                action,
                from: None,
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "New subject".into(),
                body: String::new(),
                quoted_content: None,
                quoted_inlined: false,
                in_reply_to: None,
                in_reply_to_rfc822: None,
                references: Vec::new(),
                rfc822_id: format!("{id}@brightmoor.co"),
                fingerprint: String::new(),
                attachments: Vec::new(),
                send_later: None,
                reminder: None,
            };
            Open::new(id.into(), None, record)
        };

        mail.finish_discard_listed(vec![make_draft(
            "fork-1",
            DraftAction::EditSubjectLineReply,
        )]);
        let discarded = mail.compose.discarded.as_ref().expect("discarded");
        assert_eq!(
            discarded.surface,
            Surface::Pane,
            "EditSubjectLineReply must reopen in Surface::Pane"
        );

        // Two overlapping foreign-read batches keep separate pending lists without clobbering.
        mail.compose.discard_reads.next_batch += 1;
        let batch1 = mail.compose.discard_reads.next_batch;
        mail.compose
            .discard_reads
            .pending
            .insert(batch1, vec![make_draft("d1", DraftAction::Compose)]);

        mail.compose.discard_reads.next_batch += 1;
        let batch2 = mail.compose.discard_reads.next_batch;
        mail.compose.discard_reads.pending.insert(
            batch2,
            vec![
                make_draft("d2", DraftAction::Compose),
                make_draft("d3", DraftAction::Compose),
            ],
        );

        mail.complete_discard_listed(batch2, Vec::new());
        assert_eq!(mail.toast_text(), Some("2 drafts discarded."));
        assert!(
            mail.compose.discard_reads.pending.contains_key(&batch1),
            "completing batch 2 must not clobber batch 1"
        );

        mail.complete_discard_listed(batch1, Vec::new());
        assert_eq!(mail.toast_text(), Some("Draft discarded."));
        assert_eq!(
            mail.compose.discarded.as_ref().map(|d| d.open.id.as_str()),
            Some("d1")
        );
        mail.discard();
    }
}
