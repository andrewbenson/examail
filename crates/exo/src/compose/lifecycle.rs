//! Draft lifecycle orchestration: creation, foreign-draft adoption, thread sync, autosave,
//! send / undo-send holds, discard / reopen, and account draft events.

use exo_commands::{Hint, Toast, ToastKind};
use exo_store::{
    Address, Alias, DraftAction, DraftFailure, MessageDetail, Mutation, SentDraft, ThreadDetail,
};
use jiff::{SignedDuration, Timestamp};

use super::{
    Action, Alert, AlertThen, Field, Fields, Open, PendingReply, Surface, availability, bcc_reply,
    checks, chips, draft, factory, form, outgoing, recipients,
};
use crate::{account::Event, commands};

/// How long a send is held for undo.
const HOLD: SignedDuration = SignedDuration::from_secs(20);
/// Hovering the toast holds the send this much longer…
const HOVER_HOLD: SignedDuration = SignedDuration::from_secs(30);
/// …asked for at most this often.
const HOVER_EVERY: f64 = 5.0;
/// Edits are saved at most this often.
const SAVE_EVERY: f64 = 1.0;

/// A send being held for undo, and its toast.
pub(super) struct Sending {
    pub(super) tag: u64,
    /// The draft as it was sent, to reopen on undo.
    pub(super) open: Open,
    /// Where it was: inline in this thread, or the new-message pane.
    pub(super) surface: Surface,
    /// The mark done that went with it, to undo with it.
    pub(super) done: Option<crate::account::UndoToken>,
    /// When the hold ends, as last asked for.
    pub(super) until: Timestamp,
    /// Gmail has the message, so the toast reads `Message sent.`.
    pub(super) acked: bool,
    /// Gmail didn't take it on a try and it's being tried again, so the toast reads
    /// `Still sending…`.
    pub(super) retrying: bool,
    pub(super) undoing: bool,
    /// When the hover last held the send longer.
    pub(super) held_at: f64,
    /// Scheduled for its Send later time, so there's no Instant Send and no hover hold.
    pub(super) scheduled: bool,
}

impl Sending {
    /// The toast once it's sent.
    pub(super) fn sent_text(&self) -> &'static str {
        if self.scheduled {
            return "Message scheduled.";
        }
        match (self.open.record.action, self.done.is_some()) {
            (DraftAction::Forward, true) => "Forwarded and Marked Done.",
            (DraftAction::Forward, false) => "Message forwarded.",
            (DraftAction::Reply | DraftAction::ReplyAll, true) => {
                "Reply sent and conversation Marked Done."
            }
            (DraftAction::Reply | DraftAction::ReplyAll, false) => "Reply sent.",
            _ => "Message sent.",
        }
    }

    /// The toast when it waits to go online.
    fn offline_text(&self) -> &'static str {
        if self.scheduled {
            return "Message will schedule when online.";
        }
        match self.open.record.action {
            DraftAction::Forward => "Message will forward when online.",
            DraftAction::Reply | DraftAction::ReplyAll => "Reply will send when online.",
            _ => "Message will send when online.",
        }
    }
}

/// A discarded draft a toast can bring back.
pub(super) struct Discarded {
    pub(super) tag: u64,
    pub(super) open: Open,
    pub(super) surface: Surface,
}

/// Leaves a draft. An unmodified draft is discarded silently, and any other is saved if it
/// changed.
pub(super) fn clean_up(
    open: &mut Open,
    account: &crate::account::Account,
    aliases: &[Alias],
    now: &jiff::Zoned,
) {
    if open.can_discard_silently() {
        if open.is_stored() {
            account.discard_draft(&open.id);
        }
    } else {
        save_changes(open, account, aliases, now);
    }
}

/// Saves `open` if it differs from what the store has, so a touch that changed nothing, such as
/// Tab past an empty recipient input, saves nothing.
pub(super) fn save_changes(
    open: &mut Open,
    account: &crate::account::Account,
    aliases: &[Alias],
    local: &jiff::Zoned,
) {
    if open.is_dirty() {
        save(open, account, aliases, local, false);
    } else {
        open.sync.changed = None;
    }
}

/// Discards `open` in the store. A draft adopted from elsewhere that Exo hasn't saved yet is
/// saved first with its message id so the store marks it discarded, deletes Gmail's draft and
/// can bring it back on Undo.
pub(super) fn discard_open(
    open: &mut Open,
    account: &crate::account::Account,
    aliases: &[Alias],
    local: &jiff::Zoned,
) {
    if open.is_adopted() {
        save(open, account, aliases, local, false);
    }
    if open.is_stored() {
        account.discard_draft(&open.id);
    }
}

/// Saves `open` now, dated by the system's clock in `local`'s time zone. `sending` says it's
/// saved to be sent, which Gmail gets even when unchanged, since the save dates the message. A
/// snippet never goes to Gmail, because Done saves it to the settings.
pub(super) fn save(
    open: &mut Open,
    account: &crate::account::Account,
    aliases: &[Alias],
    local: &jiff::Zoned,
    sending: bool,
) {
    if !open.persists_to_gmail() {
        return;
    }
    open.sync_body();
    let email = outgoing::message(
        open,
        aliases,
        None,
        &Timestamp::now().to_zoned(local.time_zone().clone()),
    );
    account.save_draft(
        exo_store::Draft {
            id: open.id.clone(),
            thread: open.thread.as_deref().map(exo_store::ThreadId::new),
            adopted: open.sync.adopt.take(),
            record: open.record.clone(),
            // The account adds the attachments, writes `raw` and says whether it has them all.
            raw: String::new(),
            complete: false,
            sending,
        },
        email,
    );
    open.sync.changed = None;
    open.sync.saved = Some(open.record.clone());
}

/// The draft record of a draft written elsewhere (Gmail web, Superhuman), from its message
/// (`adopt`). What was written is the body, and its quote, with all that follows it (the other
/// app's signature too), waits behind the `…` as an Examail reply's does. A draft with no quote
/// is all body. Its files are its own, and the first save fetches them, since that save replaces
/// the message they're in.
pub(super) fn adopted(message: &MessageDetail, thread: &ThreadDetail) -> Open {
    let address = |contact: &exo_store::Contact| Address {
        name: contact.name.clone(),
        email: contact.email.clone(),
    };
    let answers = message.in_reply_to.as_deref().and_then(|rfc| {
        thread
            .messages
            .iter()
            .find(|other| other.rfc822_id.as_deref() == Some(rfc))
    });
    let action = if message.subject.to_lowercase().starts_with("fwd:") {
        DraftAction::Forward
    } else if message.in_reply_to.is_some() {
        if message.cc.is_empty() {
            DraftAction::Reply
        } else {
            DraftAction::ReplyAll
        }
    } else {
        DraftAction::Compose
    };
    let html = match (&message.body.html, &message.body.text) {
        (Some(html), _) => html.clone(),
        (None, Some(text)) => exo_mail::text_to_html(text),
        (None, None) => String::new(),
    };
    let (body, quoted_content) = match exo_mail::split_draft_quote(&html) {
        // The editor sanitizes the body as it loads it, but not the quote it reveals.
        Some((body, quote)) => (body, Some(exo_mail::sanitize_fragment(&quote))),
        None => (html, None),
    };
    let quote_signed = quoted_content.is_some();
    let mut record = exo_store::DraftRecord {
        action,
        from: message.from.as_ref().map(address),
        to: message.to.iter().map(address).collect(),
        cc: message.cc.iter().map(address).collect(),
        bcc: message.bcc.iter().map(address).collect(),
        subject: message.subject.clone(),
        body,
        quoted_inlined: quoted_content.is_none(),
        quoted_content,
        quote_signed,
        in_reply_to: answers.map(|answers| answers.id.clone()),
        in_reply_to_rfc822: message.in_reply_to.clone(),
        references: message.references.clone(),
        rfc822_id: message.rfc822_id.clone().unwrap_or_else(|| {
            factory::new_rfc822_id(
                &message
                    .from
                    .as_ref()
                    .map(|from| from.email.clone())
                    .unwrap_or_default(),
                Timestamp::now(),
            )
        }),
        fingerprint: String::new(),
        attachments: Vec::new(),
        send_later: None,
        reminder: None,
    };
    factory::copy_attachments(&mut record, message, true);
    record.fingerprint = factory::fingerprint(&record);
    let mut open = Open::new(
        factory::new_id(),
        Some(thread.id.as_str().to_owned()),
        record,
    );
    open.sync.adopt = Some(message.id.clone());
    open.mark_saved();
    open
}

/// The quoted body of `message`, which is the `<body>` contents and attributes of its render,
/// sanitized, or its raw HTML sanitized when it hasn't been rendered.
fn quoted_body(message: &MessageDetail, facts: Option<&crate::body::Facts>) -> factory::QuotedBody {
    match facts {
        Some(facts) => factory::QuotedBody {
            inner_html: body_inner(&facts.html),
            class: facts.body_attributes.class.clone().unwrap_or_default(),
            style: facts.body_attributes.style.clone().unwrap_or_default(),
            id: facts.body_attributes.id.clone().unwrap_or_default(),
        },
        None => {
            let html = match (&message.body.html, &message.body.text) {
                (Some(html), _) => html.clone(),
                (None, Some(text)) => exo_mail::text_to_html(text),
                (None, None) => String::new(),
            };
            factory::QuotedBody {
                inner_html: exo_mail::sanitize_fragment(&html),
                ..factory::QuotedBody::default()
            }
        }
    }
}

/// The contents of `html`'s `<body>`, serialized.
fn body_inner(html: &str) -> String {
    let document = exo_mail::dom::parse(html);
    let out = exo_mail::dom::body(&document)
        .map_or_else(String::new, |body| exo_mail::dom::inner_html(&body));
    exo_mail::dom::drop_tree(document);
    out
}

/// The send toast: `Undo` and, revealed on hover, `Instant Send ⚡` (not for a scheduled send).
pub(super) fn send_toast(text: &'static str, tag: u64, instant: bool) -> Toast<commands::Action> {
    let toast = Toast::new(text)
        .undo(commands::Action::Compose(Action::UndoSend(tag)))
        .tag(tag);
    if !instant {
        return toast;
    }
    toast
        .action(
            "Instant Send ⚡",
            Some("mod+shift+z"),
            commands::Action::Compose(Action::InstantSend(tag)),
        )
        .hidden()
        .tooltip("Send message right now")
        .keep_on_action()
}

/// The hint after Send is clicked.
pub(super) fn send_hint() -> Hint {
    Hint::new()
        .text("Next time, hit")
        .keys(["⌘", "enter"])
        .text("to send even faster!")
}

impl crate::Mail {
    /// A new message in the thread pane, from `fields`.
    pub(crate) fn compose_new(&mut self, fields: Fields, field: Option<Field>) {
        let record = factory::compose(&self.compose_account(), fields, Timestamp::now());
        self.open_new(Open::new(factory::new_id(), None, record), field);
    }

    /// Shows `open`, a new message, in the thread pane with the keyboard (on `field`).
    pub(super) fn open_new(&mut self, mut open: Open, field: Option<Field>) {
        if self.snippet_unsaved() {
            self.ask_leaving_snippet(Box::new(move |mail| mail.open_new(open, field)));
            return;
        }
        self.with_auto_bcc(&mut open);
        // Leaving any other new message first.
        self.leave_pane();
        if let Some(field) = field {
            open.state.field = field;
        }
        let from = self.thread.as_ref().map(|thread| thread.id().to_owned());
        self.close_thread();
        self.compose.focus = Some(open.id.clone());
        self.compose.pane = Some((open.id.clone(), from));
        self.compose.drafts.push(open);
    }

    /// Carries out Change Subject. Gmail threads by subject, so a reply whose subject is to change
    /// becomes a new message. The reply is discarded, and an unthreaded copy of it pops out with
    /// its subject selected. On a new message, the subject is only focused.
    pub(super) fn edit_reply_subject(&mut self, now: f64) {
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        let Some(at) = self.compose.drafts.iter().position(|open| open.id == id) else {
            return;
        };
        if self.compose.drafts[at].is_new_email() {
            self.compose.drafts[at].state.expand(Field::Subject);
            return;
        }
        let mut reply = self.compose.drafts.remove(at);
        reply.sync_body();
        discard_open(&mut reply, &self.account, &self.snapshot.aliases, &self.now);
        self.compose.focus = None;
        if self.compose.popped.as_deref() == Some(id.as_str()) {
            self.compose.popped = None;
        }
        let mut record = reply.record;
        factory::unthread(&mut record);
        let mut open = Open::new(factory::new_id(), None, record);
        open.header.select_subject = true;
        open.touch(now);
        let new = open.id.clone();
        self.compose.drafts.push(open);
        self.pop_out(&new);
    }

    /// `Reply All, Change Subject` from the palette with no draft open: a Reply All of the
    /// focused message, as a new message with its subject selected.
    pub(super) fn reply_all_as_new_message(&mut self) {
        let Some(detail) = self
            .thread
            .as_ref()
            .and_then(crate::thread::ThreadView::detail)
        else {
            return;
        };
        let focused = self
            .thread
            .as_ref()
            .and_then(crate::thread::ThreadView::focused);
        let Some(message) = detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .find(|message| Some(message.id.as_str()) == focused)
            .cloned()
        else {
            return;
        };
        let body = quoted_body(&message, self.bodies.facts(&message.id));
        let mut record = factory::reply(
            &self.compose_account(),
            DraftAction::ReplyAll,
            &message,
            message.has_label("SENT"),
            &body,
            Timestamp::now(),
        );
        factory::unthread(&mut record);
        let mut open = Open::new(factory::new_id(), None, record);
        open.header.select_subject = true;
        self.open_new(open, Some(Field::Subject));
    }

    /// A reply, reply all or forward of the focused message in the open (or highlighted)
    /// conversation. An existing draft on that message is reused.
    pub(super) fn compose_reply(&mut self, action: DraftAction, field: Option<Field>) {
        let thread = match &self.thread {
            Some(thread) => thread.id().to_owned(),
            None => {
                // From the list: open the highlighted conversation (marking it read) and reply
                // once it's loaded.
                let Some(id) = self.inbox.cursor().map(|thread| thread.id.clone()) else {
                    return;
                };
                self.open_thread(id.clone());
                id
            }
        };
        // A selection in the focused message is quoted, unless a field was asked for.
        let quote = field
            .is_none()
            .then(|| {
                self.thread
                    .as_ref()
                    .and_then(|view| self.bodies.selected_html(view.focused()?))
            })
            .flatten()
            .map(str::to_owned);
        if quote.is_some() {
            // The draft takes the keyboard, and with it the page's one selection.
            self.bodies.clear_selection();
        }
        self.compose.pending = Some(PendingReply {
            thread,
            action,
            field,
            pop_out: false,
            body: None,
            quote,
            snippet: None,
        });
        self.create_pending_reply();
    }

    /// Attaches `snippet` to the queued reply waiting for its thread/body to load so it applies
    /// once the reply opens.
    pub(crate) fn compose_attach_pending_snippet(&mut self, snippet: exo_store::Snippet) {
        if let Some(pending) = &mut self.compose.pending {
            pending.snippet = Some(snippet);
        }
    }

    /// Creates the reply asked for once its thread has loaded.
    pub(super) fn create_pending_reply(&mut self) {
        let Some(pending) = &self.compose.pending else {
            return;
        };
        let Some(view) = &self.thread else {
            self.compose.pending = None;
            return;
        };
        if view.id() != pending.thread {
            self.compose.pending = None;
            return;
        }
        let Some(detail) = view.detail().cloned() else {
            return;
        };
        // The message answered: the focused one, else the first unread, else the last.
        let shown: Vec<&MessageDetail> = detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .collect();
        let message = view
            .focused()
            .and_then(|id| shown.iter().find(|message| message.id == id))
            .or_else(|| shown.iter().find(|message| message.is_unread()))
            .or_else(|| shown.last())
            .copied();
        // The reply's quote needs the message's body, rendered, so the reply waits for the body.
        // Offline, with no Gmail to fetch the body from, the reply doesn't start.
        if message.is_some_and(|message| self.snapshot.bodiless.contains(&message.id)) {
            if !self.snapshot.connection.is_online() {
                self.compose.pending = None;
            }
            return;
        }
        let pending = self.compose.pending.take().expect("checked above");
        let (action, field) = (pending.action, pending.field);
        let Some(message) = message.cloned() else {
            return;
        };
        let in_sent = message.has_label("SENT");
        let body = quoted_body(&message, self.bodies.facts(&message.id));
        let thread_id = detail.id.as_str().to_owned();
        let existing = self.compose.drafts.iter().position(|open| {
            open.thread.as_deref() == Some(thread_id.as_str())
                && open.record.in_reply_to.as_deref() == Some(message.id.as_str())
        });
        // Quoting into a draft with Reply All keeps the draft's own action.
        let keep_action = pending.quote.is_some() && action == DraftAction::ReplyAll;
        let id = match existing {
            Some(at) => {
                let account = factory::Account {
                    aliases: &self.snapshot.aliases,
                    email: self.account.email(),
                    tz: self.now.time_zone().clone(),
                };
                let open = &mut self.compose.drafts[at];
                if open.record.action != action && !open.is_adopted() && !keep_action {
                    factory::retarget(&mut open.record, &account, action, &message, in_sent, &body);
                    open.state = draft::State::default_for(&open.record);
                    open.touch(0.0);
                }
                open.id.clone()
            }
            None => {
                let record = factory::reply(
                    &self.compose_account(),
                    action,
                    &message,
                    in_sent,
                    &body,
                    Timestamp::now(),
                );
                let mut open = Open::new(factory::new_id(), Some(thread_id), record);
                self.with_auto_bcc(&mut open);
                let id = open.id.clone();
                self.compose.drafts.push(open);
                id
            }
        };
        if let (Some(open), Some(field)) = (self.compose.get_mut(&id), field) {
            open.state.expand(field);
        }
        if let (Some(open), Some(body)) = (self.compose.get_mut(&id), &pending.body) {
            availability::insert_at_caret(open, body);
        }
        if let (Some(open), Some(quote)) = (self.compose.get_mut(&id), &pending.quote) {
            open.editor.add_quote(quote);
            open.touch(0.0);
        }
        self.compose.focus = Some(id.clone());
        if let Some(view) = &mut self.thread {
            view.reveal_draft(&message.id);
        }
        self.pop_out_created(&pending, &id);
        if let Some(snippet) = pending.snippet {
            let snippet_id = snippet.id.clone();
            let now = self.ctx.input(|input| input.time);
            self.compose_apply_snippet(&snippet, false, now);
            self.record_snippet_used(&snippet_id);
        }
        let from = self
            .compose
            .get_mut(&id)
            .and_then(|open| open.record.from.clone());
        if action == DraftAction::ReplyAll
            && from.is_some_and(|from| bcc_reply(&message, &from.email))
        {
            self.commands.hint(
                Hint::new()
                    .text("Careful, you were on BCC! Reply instead of Reply All?")
                    .keys(["esc", "R"]),
            );
        }
    }

    /// Opens the drafts of the open thread once it's loaded: Exo's own, and drafts written
    /// elsewhere (which Exo takes over on their first save). Then any reply waiting for it.
    pub(crate) fn compose_sync(&mut self) {
        self.compose.send_and_done = self.account.settings().send_and_done;
        self.compose.backtick_escape = self.account.settings().backtick_as_escape();
        self.compose.availability_tip = self.availability_tip_due();
        let Some(view) = &self.thread else { return };
        let Some(detail) = view.detail().cloned() else {
            return;
        };
        let thread = detail.id.as_str().to_owned();
        if self.compose.loaded.as_deref() != Some(thread.as_str()) {
            // A draft written elsewhere is taken up with its body, so until the body is in, the
            // thread loads again.
            let waiting = detail
                .messages
                .iter()
                .any(|message| message.is_draft() && self.snapshot.bodiless.contains(&message.id));
            if !waiting {
                self.compose.loaded = Some(thread.clone());
            }
            let sending: Vec<&str> = self
                .compose
                .sends
                .iter()
                .map(|sending| sending.open.id.as_str())
                .collect();
            // A failed draft is a message of the thread until it's edited.
            for stored in self.snapshot.drafts.iter().filter(|draft| {
                draft
                    .thread
                    .as_ref()
                    .is_some_and(|id| id.as_str() == thread)
                    && draft.send_at.is_none()
                    && draft.failure.is_none()
            }) {
                if !self.compose.drafts.iter().any(|open| open.id == stored.id)
                    && !sending.contains(&stored.id.as_str())
                {
                    self.compose.drafts.push(Open::stored(stored));
                }
            }
            let ours: Vec<&str> = self
                .snapshot
                .drafts
                .iter()
                .filter_map(|draft| draft.message_id.as_deref())
                .collect();
            let foreign: Vec<Open> = detail
                .messages
                .iter()
                .filter(|message| {
                    message.is_draft()
                        && !ours.contains(&message.id.as_str())
                        && !self.snapshot.bodiless.contains(&message.id)
                })
                .filter(|message| {
                    !self
                        .compose
                        .drafts
                        .iter()
                        .any(|open| open.adopted_message() == Some(message.id.as_str()))
                })
                .map(|message| adopted(message, &detail))
                .collect();
            self.compose.drafts.extend(foreign);
        } else {
            // A reply that called off a scheduled send makes it a draft again, in its thread.
            let unscheduled: Vec<Open> = self
                .snapshot
                .drafts
                .iter()
                .filter(|draft| {
                    draft
                        .thread
                        .as_ref()
                        .is_some_and(|id| id.as_str() == thread)
                        && draft.send_at.is_none()
                        && draft.failure.is_none()
                })
                .filter(|draft| {
                    draft.record.send_later.as_ref().is_some_and(|later| {
                        later.abort_on_reply && !later.current_message_ids.is_empty()
                    })
                })
                .filter(|draft| {
                    !self.compose.drafts.iter().any(|open| open.id == draft.id)
                        && !self
                            .compose
                            .sends
                            .iter()
                            .any(|sending| sending.open.id == draft.id)
                })
                .map(Open::stored)
                .collect();
            self.compose.drafts.extend(unscheduled);
        }
        self.create_pending_reply();
    }

    /// Opens a draft-only thread by showing its draft in the new-message pane.
    pub(crate) fn open_draft_thread(&mut self, thread: &str) -> bool {
        let Some(stored) = self.snapshot.drafts.iter().find(|draft| {
            draft
                .thread
                .as_ref()
                .is_some_and(|id| id.as_str() == thread)
                && draft.send_at.is_none()
                && draft.failure.is_none()
        }) else {
            return false;
        };
        if self.compose.popped.as_deref() == Some(stored.id.as_str()) {
            self.pop_in();
            return true;
        }
        let open = Open::stored(stored);
        self.leave_pane();
        self.compose.focus = Some(open.id.clone());
        self.compose.pane = Some((open.id.clone(), None));
        self.compose.drafts.retain(|other| other.id != open.id);
        self.compose.drafts.push(open);
        true
    }

    /// Closes the new-message pane, saving or silently discarding its draft.
    pub(crate) fn leave_pane(&mut self) {
        let Some((id, _)) = self.compose.pane.take() else {
            return;
        };
        if let Some(at) = self.compose.drafts.iter().position(|open| open.id == id) {
            let mut open = self.compose.drafts.remove(at);
            clean_up(&mut open, &self.account, &self.snapshot.aliases, &self.now);
        }
        if self.compose.focus.as_deref() == Some(id.as_str()) {
            self.compose.focus = None;
        }
    }

    /// Saves or drops the drafts of `thread`, which is closing.
    pub(crate) fn compose_close_thread(&mut self, thread: &str) {
        self.compose
            .close_thread_drafts(thread, &self.account, &self.snapshot.aliases, &self.now);
        self.compose.in_thread.close(thread);
        self.compose.pending = None;
    }

    /// Escape in a draft. An unmodified draft goes. Any other stays in its form, and the keyboard
    /// goes back to the thread.
    pub(super) fn compose_escape(&mut self) {
        if self.compose.focused().is_some_and(Open::is_snippet) {
            self.escape_snippet();
            return;
        }
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        if self.compose.popped.as_deref() == Some(id.as_str()) {
            self.close_popout();
            return;
        }
        let Some(at) = self.compose.drafts.iter().position(|open| open.id == id) else {
            return;
        };
        let unmodified = self.compose.drafts[at].can_discard_silently();
        self.compose.focus = None;
        if unmodified {
            let open = self.compose.drafts.remove(at);
            if open.is_stored() {
                self.account.discard_draft(&open.id);
            }
            if self.compose.pane() == Some(id.as_str()) {
                self.close_pane_to_origin();
            }
        } else {
            let open = &mut self.compose.drafts[at];
            save_changes(open, &self.account, &self.snapshot.aliases, &self.now);
        }
    }

    /// Leaves the new-message pane for wherever it was opened from.
    pub(super) fn close_pane_to_origin(&mut self) {
        let Some((id, from)) = self.compose.pane.take() else {
            return;
        };
        self.compose
            .drafts
            .retain(|open| open.id != id || open.sync.changed.is_some());
        if let Some(thread) = from {
            self.open_thread(thread);
        }
    }

    /// Discard, with its toast. The draft goes (from Gmail too), and Undo brings it back where
    /// it was.
    pub(super) fn compose_discard(&mut self) {
        if self.compose.focused().is_some_and(Open::is_snippet) {
            self.ask_discard_snippet();
            return;
        }
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        let Some(at) = self.compose.drafts.iter().position(|open| open.id == id) else {
            return;
        };
        let mut open = self.compose.drafts.remove(at);
        open.sync_body();
        self.compose.focus = None;
        discard_open(&mut open, &self.account, &self.snapshot.aliases, &self.now);
        let surface = if self.compose.pane() == Some(id.as_str()) {
            self.close_pane_to_origin();
            Surface::Pane
        } else if self
            .compose
            .popped
            .take_if(|popped| *popped == id)
            .is_some()
        {
            Surface::Popout
        } else {
            Surface::Inline
        };
        let tag = self.compose.tag();
        self.compose.discarded = Some(Discarded { tag, open, surface });
        self.commands.toast(
            Toast::new("Draft discarded.")
                .undo(commands::Action::Compose(Action::UndoDiscard(tag)))
                .tag(tag),
        );
    }

    /// Puts a draft back where it was (after an undo), with the keyboard. When the draft goes
    /// into the pane or a thread, it takes the place of a snippet being edited only once
    /// `Save Snippet?` has asked, so the snippet's changes are never dropped unasked.
    pub(super) fn reopen(&mut self, open: Open, surface: Surface) {
        if surface != Surface::Popout && self.snippet_unsaved() {
            self.ask_leaving_snippet(Box::new(move |mail| mail.reopen(open, surface)));
            return;
        }
        let id = open.id.clone();
        let thread = open.thread.clone();
        let anchor = open.record.in_reply_to.clone();
        self.compose.drafts.retain(|other| other.id != id);
        match surface {
            Surface::Popout => {
                self.compose.drafts.push(open);
                self.pop_out(&id);
            }
            Surface::Pane => {
                self.leave_pane();
                self.close_thread();
                self.compose.pane = Some((id.clone(), None));
                self.compose.drafts.push(open);
            }
            Surface::Inline => {
                if let Some(thread) = &thread
                    && self.thread.as_ref().is_none_or(|view| view.id() != thread)
                {
                    self.open_thread(thread.clone());
                }
                // Loading the thread opens its stored drafts, but compose has this one already.
                self.compose.loaded = thread.clone();
                self.compose.drafts.push(open);
                if let (Some(view), Some(anchor)) = (&mut self.thread, anchor) {
                    view.reveal_draft(&anchor);
                }
            }
        }
        self.compose.focus = Some(id);
    }

    /// Send: the checks, then the send (held for undo) and its toast.
    pub(super) fn compose_send(&mut self, mark_done: bool, bypass: checks::Bypass) {
        if self.compose.focused().is_some_and(Open::is_snippet) {
            self.save_snippet();
            return;
        }
        let aliases = self.snapshot.aliases.clone();
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        let Some(at) = self.compose.drafts.iter().position(|open| open.id == id) else {
            return;
        };
        let now = self.ctx.input(|input| input.time);
        {
            let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
            let open = &mut self.compose.drafts[at];
            form::commit_all(open, now, book);
            let html = outgoing::html(open, &aliases);
            let from_ok = open.record.from.as_ref().is_none_or(|from| {
                aliases.is_empty()
                    || aliases.iter().any(|alias| {
                        alias.active && recipients::same_address(&alias.email, &from.email)
                    })
            });
            if let Some(problem) = checks::check(open, &html, from_ok, bypass, self.now.timestamp())
            {
                let (actions, then) = match problem.kind {
                    checks::Kind::SendAnyway(bypass) => (
                        vec![checks::anyway(open), "Cancel"],
                        AlertThen::SendAnyway(bypass, mark_done),
                    ),
                    checks::Kind::Ok { focus_from } => (vec!["OK"], AlertThen::Ok { focus_from }),
                };
                self.compose.alert =
                    Some(Alert::new(problem.title, problem.text, actions, id, then));
                return;
            }
        }
        let mut open = self.compose.drafts.remove(at);
        let surface = if self.compose.pane() == Some(id.as_str()) {
            Surface::Pane
        } else if self
            .compose
            .popped
            .take_if(|popped| *popped == id)
            .is_some()
        {
            Surface::Popout
        } else {
            Surface::Inline
        };
        // A send that a reply called off goes now. Its time is dropped with the old job.
        if open.delivery.unscheduled_by.take().is_some() {
            open.record.send_later = None;
        }
        let scheduled = open.record.send_later.as_ref().map(|later| later.at);
        // Writing to a removed suggestion brings it back (its `excluded_contacts` entry goes).
        let sent_to: Vec<String> = open
            .recipients()
            .map(|address| address.email.clone())
            .collect();
        self.account.update_settings(|settings| {
            settings
                .excluded_contacts
                .retain(|email, _| !sent_to.iter().any(|sent| sent.eq_ignore_ascii_case(email)))
        });
        save(&mut open, &self.account, &aliases, &self.now, true);
        let until = match scheduled {
            Some(at) => {
                // Held for its time. Its thread shows Gmail's copy as scheduled.
                self.account.schedule_draft(&open.id);
                at
            }
            None => {
                let until = Timestamp::now().checked_add(HOLD).unwrap_or(Timestamp::MAX);
                self.account.send_draft(&open.id, until);
                until
            }
        };
        self.compose.focus = None;
        // A reply shows in its thread as sent at once, focused at the end.
        if !open.is_new_email()
            && scheduled.is_none()
            && let Some(shown) = self.compose.in_thread.show(
                &open,
                &outgoing::html(&open, &aliases),
                &aliases,
                self.now.timestamp().checked_add(HOLD).unwrap_or(until),
            )
            && let Some(view) = self
                .thread
                .as_mut()
                .filter(|view| open.thread.as_deref() == Some(view.id()))
        {
            view.reveal_sent(&shown);
        }
        // Outside the new-message pane, a send marks its conversation done when asked
        // (`Send & Mark Done`, ⌘⇧) or with `Send + Mark Done` on. A conversation that's only this
        // draft has nothing to mark.
        let mark_done =
            surface != Surface::Pane && (mark_done || self.account.settings().send_and_done);
        let thread = open
            .thread
            .clone()
            .filter(|_| mark_done && !open.is_new_email());
        let done = thread.map(|thread| self.mark_done_with_send(&thread, now));
        let tag = self.compose.tag();
        let reminded = open.record.reminder.is_some();
        let sending = Sending {
            tag,
            open,
            surface,
            done,
            until,
            acked: false,
            retrying: false,
            undoing: false,
            held_at: f64::NEG_INFINITY,
            scheduled: scheduled.is_some(),
        };
        let text = if sending.scheduled {
            "Scheduling…"
        } else {
            "Sending…"
        };
        let scheduled = sending.scheduled;
        let instant = !scheduled && !self.undo_alert_pending();
        self.commands.toast(send_toast(text, tag, instant).sticky());
        self.compose.sends.push(sending);
        if surface == Surface::Pane {
            self.close_pane_to_origin();
        }
        self.show_undo_alert(&id, tag, scheduled, now);
        self.follow_up_hint(&id, reminded, now);
    }

    /// Marks `thread` done as part of a send, without a toast of its own. The conversation
    /// leaves the list as `e` would take it, Auto Advance and all. Returns the undo token.
    fn mark_done_with_send(&mut self, thread: &str, now: f64) -> crate::account::UndoToken {
        let token = self.account.undo_token();
        self.account
            .apply(&[thread.to_owned()], Mutation::MarkDone, Some(token));
        let leaving = self
            .inbox
            .threads()
            .iter()
            .any(|row| row.id == thread && row.removed_by.contains(&Mutation::MarkDone));
        if leaving {
            self.dismiss_and_advance(&[thread.to_owned()], 0, now);
        }
        token
    }

    /// The toast's Undo: the send is taken back if it's still held.
    pub(super) fn undo_send(&mut self, tag: u64) {
        let Some(sending) = self
            .compose
            .sends
            .iter_mut()
            .find(|sending| sending.tag == tag)
        else {
            return;
        };
        sending.undoing = true;
        self.account.undo_send(&sending.open.id);
        self.commands
            .toast(Toast::new("Undoing…").tag(tag).sticky());
        if self
            .compose
            .undo_alert
            .as_ref()
            .is_some_and(|held| held.tag == tag)
        {
            self.finish_undo_alert();
        }
    }

    /// An alert's action.
    pub(super) fn alert_action(&mut self, index: usize) {
        let Some(alert) = self.compose.alert.take() else {
            return;
        };
        let first = index == 0;
        match alert.then {
            AlertThen::SendAnyway(bypass, mark_done) if first => {
                self.compose.focus = Some(alert.draft);
                self.compose_send(mark_done, bypass);
            }
            AlertThen::SendAnyway(..) => self.compose.focus = Some(alert.draft),
            AlertThen::Ok { focus_from } => {
                self.compose.focus = Some(alert.draft);
                if focus_from && let Some(open) = self.compose.focused_mut() {
                    open.state.expand(Field::From);
                }
            }
            AlertThen::SeeMessage => {
                if let Some(at) = self
                    .compose
                    .sends
                    .iter()
                    .position(|sending| sending.open.id == alert.draft)
                {
                    let sending = self.compose.sends.remove(at);
                    if !self.see_failed(&alert.draft) {
                        self.reopen(sending.open, sending.surface);
                    }
                }
            }
            AlertThen::SaveSnippet => self.answer_save_snippet(index, alert.draft),
            AlertThen::DiscardSnippet if first => self.discard_snippet(),
            AlertThen::DiscardSnippet => self.compose.focus = Some(alert.draft),
            AlertThen::CreateSnippet if first => self.create_snippet(String::new()),
            AlertThen::CreateSnippet => {}
            AlertThen::FollowUp if first => self.follow_up_set_reminder(alert.draft),
            AlertThen::FollowUp => self.compose.follow_up = None,
            AlertThen::UndoSend(tag) => self.dismiss_undo_alert(tag),
        }
    }

    /// Saves what changed, at most once a second per draft, and keeps held sends' toasts up
    /// to date (hovering one holds the send longer).
    pub(crate) fn compose_tick(&mut self, now: f64) {
        self.compose_address_book();
        // What typing does follows the settings.
        let settings = self.account.settings();
        let editor = exo_editor::Settings {
            hyphen_starts_list: settings.convert_hyphen_to_bullet_list,
            ..exo_editor::Settings::default()
        };
        let signature_on_replies = settings.include_signature_on_replies();
        let online = self.snapshot.connection.is_online();
        for open in &mut self.compose.drafts {
            // A snippet's body detects no dates. Whether this draft or another is popped out sets
            // how far the date tooltip keeps from the window's edges.
            let popped_out = self.compose.popped.as_deref() == Some(open.id.as_str());
            let another_popped_out = self.compose.popped.is_some() && !popped_out;
            open.editor.set_settings(exo_editor::Settings {
                skip_dates: open.is_snippet(),
                popped_out,
                another_popped_out,
                ..editor
            });
            open.body.signature_on_replies = signature_on_replies;
            open.sync.upload = self.account.upload(&open.id);
            open.sync.online = online;
        }
        self.compose.missing_originals = match (self.focused_original(), self.compose.focused()) {
            (Some(message), Some(open)) if open.record.action != DraftAction::Forward => {
                factory::missing_originals(&open.record, message)
            }
            _ => 0,
        };
        self.include_attachments_hint();
        self.inline_snippet_rows();
        self.assistant_rows();
        self.postpone_for_follow_up(now);
        self.hold_for_undo_alert(now);
        self.automatic_reminders(now);
        self.note_unscheduled();
        self.unschedule_passed();
        let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
        for open in &mut self.compose.drafts {
            let has_keyboard = self.compose.focus.as_deref() == Some(open.id.as_str());
            chips::blur(open, has_keyboard, book, now);
            chips::note(open);
        }
        self.auto_bcc_tick();
        for open in &mut self.compose.drafts {
            chips::settle(open);
        }
        self.start_pending_intro(now);
        if let Some(wait) = self.compose_save_changes(now, true) {
            self.ctx.request_repaint_after_secs(wait as f32);
        }
        let showing = self.commands.toast_tag();
        let hovered = self.commands.toast_hovered();
        // A scheduled send's toast is its only way back here. Once the toast is gone, the send
        // waits in the store, and its thread shows it. A scheduled send that a reply called off
        // is a draft again.
        let unscheduled: Vec<&str> = self
            .snapshot
            .drafts
            .iter()
            .filter(|draft| {
                draft.send_at.is_none()
                    && draft
                        .record
                        .send_later
                        .as_ref()
                        .is_some_and(|later| !later.current_message_ids.is_empty())
            })
            .map(|draft| draft.id.as_str())
            .collect();
        self.compose.sends.retain(|sending| {
            !(sending.scheduled
                && sending.acked
                && !sending.undoing
                && (showing != Some(sending.tag)
                    || unscheduled.contains(&sending.open.id.as_str())))
        });
        for sending in self
            .compose
            .sends
            .iter_mut()
            .filter(|sending| !sending.scheduled)
        {
            if Some(sending.tag) == showing
                && hovered
                && sending.acked
                && !sending.undoing
                && now - sending.held_at >= HOVER_EVERY
            {
                sending.held_at = now;
                let wanted = Timestamp::now()
                    .checked_add(HOVER_HOLD)
                    .unwrap_or(Timestamp::MAX);
                if wanted > sending.until {
                    sending.until = wanted;
                    self.account.send_draft(&sending.open.id, wanted);
                }
            }
        }
        if self.compose.sends.iter().any(|sending| {
            Some(sending.tag) == showing && hovered && sending.acked && !sending.scheduled
        }) {
            self.ctx.request_repaint_after_secs(HOVER_EVERY as f32);
        }
    }

    /// Saves the drafts with changes, at time `now`: once a second per draft when `throttled`,
    /// else every one at once. Returns how long until the next throttled save is due.
    fn compose_save_changes(&mut self, now: f64, throttled: bool) -> Option<f64> {
        let aliases = self.snapshot.aliases.clone();
        let mut wait = None::<f64>;
        for open in &mut self.compose.drafts {
            let Some(since) = open.pending_autosave() else {
                continue;
            };
            let due = since.max(open.sync.saved_at) + SAVE_EVERY;
            if now >= due || !throttled {
                save_changes(open, &self.account, &aliases, &self.now);
                open.sync.saved_at = now;
            } else {
                wait = Some(wait.map_or(due - now, |wait: f64| wait.min(due - now)));
            }
        }
        wait
    }

    /// Saves every open draft's latest changes at once, rather than within the second. Quitting
    /// saves this way before the account sends what it holds, and so does leaving the account for
    /// another.
    pub fn save_drafts(&mut self) {
        let now = self.ctx.input(|input| input.time);
        self.compose_save_changes(now, false);
    }

    /// Runs after the frame is drawn. A press anywhere but the focused draft's form takes the
    /// keyboard from the draft, which is saved and stays open.
    pub(crate) fn compose_blur_on_press(&mut self, ctx: &egui::Context, content: egui::Rect) {
        let pressed = ctx.input(|input| input.pointer.any_pressed());
        let inside = std::mem::take(&mut self.compose.pressed_inside);
        if !pressed || inside || self.compose.alert.is_some() {
            return;
        }
        let Some(id) = self.compose.focus.take() else {
            return;
        };
        // A press in the content pane takes the keyboard from the pop-out, with a hint at ⌘D.
        if self.compose.popped.as_deref() == Some(id.as_str())
            && ctx
                .input(|input| input.pointer.press_origin())
                .is_some_and(|at| content.contains(at))
        {
            self.commands.hint(
                Hint::new()
                    .text("Next time, hit")
                    .keys(["⌘", "D"])
                    .text("to instantly move from your draft!"),
            );
        }
        if let Some(open) = self.compose.get_mut(&id)
            && open.sync.changed.is_some()
            && (open.is_stored() || !open.can_discard_silently())
        {
            save_changes(open, &self.account, &self.snapshot.aliases, &self.now);
        }
        ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
        });
    }

    /// The include-attachments hint. When someone who wasn't on the message a reply answers was
    /// just added to the reply, and that message has files the reply hasn't, the hint offers
    /// `Include Original Attachment(s)`.
    fn include_attachments_hint(&mut self) {
        let Some(open) = self.compose.focused() else {
            self.compose.recipients_seen = None;
            return;
        };
        let recipients: Vec<Address> = open.recipients().cloned().collect();
        let now: Vec<String> = recipients
            .iter()
            .map(|address| address.email.to_lowercase())
            .collect();
        let reply = matches!(
            open.record.action,
            DraftAction::Reply | DraftAction::ReplyAll
        );
        let id = open.id.clone();
        let before = self
            .compose
            .recipients_seen
            .replace((id.clone(), now.clone()))
            .filter(|(seen, _)| *seen == id)
            .map(|(_, before)| before);
        // The contact pane shows whom the draft just got.
        if let Some(before) = &before
            && let Some(added) = recipients.into_iter().rfind(|address| {
                !address.email.is_empty() && !before.contains(&address.email.to_lowercase())
            })
        {
            self.compose.just_added = Some(added);
        }
        let Some(before) = before.filter(|_| reply && self.compose.missing_originals > 0) else {
            return;
        };
        let Some(original) = self.focused_original() else {
            return;
        };
        if factory::adds_newcomer(&now, &before, original) {
            // Singular or plural by the message's own file count.
            let files = original
                .attachments
                .iter()
                .filter(|attachment| !attachment.inline)
                .count();
            let (noun, command) = if files == 1 {
                ("attachment", "Attachment")
            } else {
                ("attachments", "Attachments")
            };
            self.commands.hint(
                Hint::new()
                    .text(format!("Want to include the original {noun}? Hit"))
                    .keys(["⌘", "K"])
                    .text(format!("→ Include Original {command}!")),
            );
        }
    }

    /// The message the focused draft answers, when its thread is open.
    pub(super) fn focused_original(&self) -> Option<&MessageDetail> {
        let open = self.compose.focused()?;
        let answers = open.record.in_reply_to.as_deref()?;
        let detail = self.thread.as_ref()?.detail()?;
        detail.messages.iter().find(|message| message.id == answers)
    }

    /// What the account says about drafts.
    pub(crate) fn compose_event(&mut self, event: Event) {
        let showing = self.commands.toast_tag();
        let undo_alert = self.undo_alert_pending();
        match event {
            Event::DraftSaved(id) => {
                if let Some(sending) = self
                    .compose
                    .sends
                    .iter_mut()
                    .find(|sending| sending.open.id == id && !sending.acked && !sending.undoing)
                {
                    sending.acked = true;
                    if showing == Some(sending.tag) {
                        let text = sending.sent_text();
                        let instant = !sending.scheduled && !undo_alert;
                        let toast = send_toast(text, sending.tag, instant);
                        // The Undo Send alert keeps the toast up while it's open.
                        let held = self
                            .compose
                            .undo_alert
                            .as_ref()
                            .is_some_and(|held| held.tag == sending.tag);
                        self.commands
                            .toast(if held { toast.sticky() } else { toast });
                    }
                }
            }
            Event::SendRetrying(id) => {
                if let Some(sending) = self.compose.sends.iter_mut().find(|sending| {
                    sending.open.id == id && !sending.acked && !sending.undoing && !sending.retrying
                }) {
                    sending.retrying = true;
                    // A scheduled send keeps saying `Scheduling…`.
                    if showing == Some(sending.tag) && !sending.scheduled {
                        self.commands
                            .toast(send_toast("Still sending…", sending.tag, !undo_alert).sticky());
                    }
                }
            }
            Event::DraftsOffline => {
                for sending in self.compose.sends.iter().filter(|sending| {
                    !sending.acked && !sending.undoing && showing == Some(sending.tag)
                }) {
                    self.commands.toast(
                        Toast::new(sending.offline_text())
                            .undo(commands::Action::Compose(Action::UndoSend(sending.tag)))
                            .tag(sending.tag),
                    );
                }
            }
            Event::DraftSent(SentDraft {
                id,
                thread,
                message,
            }) => {
                // Its recipients join the address book on send.
                self.account.load_address_book();
                self.compose
                    .sent_threads
                    .push((id.clone(), thread.as_str().to_owned()));
                let excess = self.compose.sent_threads.len().saturating_sub(4);
                self.compose.sent_threads.drain(..excess);
                self.compose.in_thread.sent(
                    &id,
                    &message,
                    self.thread.as_ref().map(crate::thread::ThreadView::id),
                );
                if let Some(at) = self
                    .compose
                    .sends
                    .iter()
                    .position(|sending| sending.open.id == id)
                {
                    let sending = self.compose.sends.remove(at);
                    // Each snippet used in the message counts a send in the snippet's analytics.
                    self.snippets_sent(&sending.open.delivery.applied_snippets, &thread, &message);
                    if showing == Some(sending.tag) && !sending.undoing {
                        self.commands
                            .toast(Toast::new(sending.sent_text()).tag(sending.tag));
                    }
                }
            }
            Event::SendTakenBack { id, held } => {
                let Some(at) = self
                    .compose
                    .sends
                    .iter()
                    .position(|sending| sending.open.id == id)
                else {
                    return;
                };
                if held {
                    self.compose.in_thread.forget(&id);
                    let sending = self.compose.sends.remove(at);
                    if let Some(token) = sending.done {
                        self.account.undo(token);
                    }
                    let text = if sending.scheduled {
                        "Scheduling undone."
                    } else if sending.done.is_some() {
                        "Sending undone and conversation moved to Inbox."
                    } else {
                        "Sending undone."
                    };
                    self.commands.toast(Toast::new(text).tag(sending.tag));
                    self.reopen(sending.open, sending.surface);
                } else {
                    self.commands.hide_toast();
                    self.show_failure_alert(
                        "Undo Send failed due to poor internet connectivity…".into(),
                        &id,
                        false,
                    );
                }
            }
            Event::Incomplete(ids) => {
                for id in ids {
                    match self.compose.drafts.iter_mut().find(|open| open.id == id) {
                        Some(open) => save(
                            open,
                            &self.account,
                            &self.snapshot.aliases,
                            &self.now,
                            false,
                        ),
                        None => {
                            let Some(stored) =
                                self.snapshot.drafts.iter().find(|draft| draft.id == id)
                            else {
                                continue;
                            };
                            save(
                                &mut Open::stored(stored),
                                &self.account,
                                &self.snapshot.aliases,
                                &self.now,
                                false,
                            );
                        }
                    }
                }
            }
            Event::AttachmentsFailed { id, count } => {
                if self.compose.drafts.iter().any(|open| open.id == id) {
                    let text = if count > 1 {
                        "Sorry! Failed to save attachments. Please try reuploading."
                    } else {
                        "Sorry! Failed to save attachment. Please try reuploading."
                    };
                    self.commands.toast(Toast::new(text).kind(ToastKind::Error));
                }
            }
            Event::DraftFailed(DraftFailure::Save { id, .. }) => {
                if self.compose.drafts.iter().any(|open| open.id == id) {
                    self.commands
                        .toast(Toast::new("Sorry! Failed to save draft.").kind(ToastKind::Error));
                }
            }
            Event::DraftFailed(DraftFailure::Send { id, reason }) => {
                // Gmail's copy shows in the thread instead, as a failed draft.
                self.compose.in_thread.forget(&id);
                // Only a send from this session has an observer to say so at once.
                let Some(at) = self
                    .compose
                    .sends
                    .iter()
                    .position(|sending| sending.open.id == id)
                else {
                    return;
                };
                match crate::format::failure::alert(reason) {
                    // The alert's See Message takes the send from there.
                    Some(text) => {
                        self.commands.hide_toast();
                        self.show_failure_alert(text.into(), &id, true);
                    }
                    // Only the failed draft says so. The send's toast, if still up, goes with
                    // the send it would hold or speed up.
                    None => {
                        let sending = self.compose.sends.remove(at);
                        if showing == Some(sending.tag) {
                            self.commands.hide_toast();
                        }
                    }
                }
            }
            Event::Failed { .. }
            | Event::LabelCreated(_)
            | Event::LabelNotCreated { .. }
            | Event::Unsent(_)
            | Event::Thread { .. }
            | Event::Unavailable { .. }
            | Event::SettingsChanged { .. }
            | Event::McpToken(_) => {}
        }
    }

    fn show_failure_alert(&mut self, text: String, draft: &str, see_message: bool) {
        let (actions, then) = if see_message {
            (vec!["See Message"], AlertThen::SeeMessage)
        } else {
            (vec!["OK"], AlertThen::Ok { focus_from: false })
        };
        self.compose.alert = Some(Alert::new(Some("Sorry!"), text, actions, draft, then));
    }
}
