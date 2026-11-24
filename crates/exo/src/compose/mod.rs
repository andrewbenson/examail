//! Compose: new messages, replies, reply all and forwards, drafts kept in Gmail, and sending
//! with undo send.
//!
//! A draft open in compose is an [`Open`]: its record, an `exo-editor` for the body, and the
//! form's state. It shows inline under the message it answers ([`crate::thread`] lays it out
//! through [`Inline`]) or, for a new message, in a pane of its own
//! ([`crate::Mail::show_compose_pane`]). Edits are saved within a second
//! ([`crate::Mail::compose_tick`]) to the store, which mirrors them to a Gmail draft. Sending
//! holds the draft for 20 s. During the hold, the toast's Undo takes the draft back, hovering the
//! toast buys 30 s more, and Instant Send sends at once.
//!
//! Send later picks a time, and Send then schedules the draft for it ([`schedule`]). Remind me
//! picks a reminder that the message's thread gets at Send, and Undo takes the reminder back
//! ([`reminder`]). Compose leaves out AI, sharing and every other server feature, as for an
//! account without them.

mod assistant;
pub(crate) mod auto_bcc;
mod availability;
mod checks;
mod chips;
pub(crate) mod contacts;
pub(crate) mod discard;
mod draft;
mod factory;
mod failed;
mod files;
mod form;
mod images;
mod intro;
mod keys;
mod lifecycle;
mod mailto;
mod media;
mod outgoing;
mod picker;
mod plain;
mod popout;
pub(crate) mod recipients;
mod reminder;
mod schedule;
mod sent;
mod signature;
mod snippet_editor;
pub(crate) mod snippets;
mod spelling;
mod subject;
mod surfaces;
mod undo_alert;
mod unsubscribe;

use std::sync::Arc;

use exo_commands::{Hint, Region, Toast};
use exo_store::{Address, Alias, DraftAction, MessageDetail, ThreadDetail};
use jiff::Timestamp;

pub(crate) use draft::{Field, Open};
pub(crate) use factory::{Fields, uuid_v4};
use form::from_menu;
pub(crate) use form::{
    SNIPPET_NAME_ROOM, Surface,
    footer::{
        ACTION_DOWN as FOOTER_ACTION_DOWN, ACTION_HEIGHT as FOOTER_ACTION_HEIGHT, discard_button,
        draft_attachments_to_parts, submit_button,
    },
    header::ReadOnlyHeader,
    paint_rule,
};
pub(crate) use keys::{Shortcuts, register_compose_to, register_shortcuts};
use lifecycle::{
    Discarded, Sending, adopted, clean_up, discard_open, save, save_changes, send_hint, send_toast,
};
pub(crate) use mailto::fields as mailto_fields;
pub(crate) use reminder::team_domains;
pub(crate) use sent::synthetic_message;
pub(crate) use snippet_editor::Leaving;
pub(crate) use surfaces::Inline;
use surfaces::set_reminder;

/// What compose's commands, clicks and toasts ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// `C`, `Compose`: a new message.
    New,
    /// `Note to self`.
    NoteToSelf,
    /// `R`, `A`, `F`, Enter: reply, reply all or forward to the focused (or highlighted)
    /// conversation's message.
    Reply(DraftAction),
    /// `⌘⇧O/C/B/F/S` with no draft focused: a Reply All with that field focused.
    ReplyAllWith(Field),
    /// `⌘⇧O/C/B/F/S` in a draft, or a click on a field: focus it.
    Focus(Field),
    /// A click on a reply's subject, Enter on it, or the palette's `Change Subject`: the reply
    /// becomes a new message, popped out. On a new message, the subject is only focused.
    EditSubject,
    /// The palette's `Reply All, Change Subject` with no draft open: a Reply All of the focused
    /// message as a new message.
    ReplyAllEditSubject,
    /// A click on From's address, or Enter on it: the From list opens or closes.
    FromToggle,
    /// ↓ / ↑ (and `j`/`n`, `k`/`p`) in the From list.
    FromMove(isize),
    /// Enter or Tab in the From list (the highlighted alias), or a click on one.
    FromChoose(Option<usize>),
    /// Escape in the From list: it closes.
    FromClose,
    /// A formatting key in the body.
    Format(exo_editor::FormatCommand),
    /// ⌥⇧` in the body with Backtick as Escape on: a backtick.
    TypeBacktick,
    /// The body's caret came onto a detected date that says this, so the right pane shows that
    /// date's day.
    DateActive(String),
    /// ⌘⇧– (-1) or ⌘⇧= (1) with the caret on a date: the date then says a day earlier or later.
    ShiftDate(i64),
    /// ↓ / ↑ in the `;` picker.
    SnippetMove(isize),
    /// Enter or Tab in the `;` picker (the highlighted snippet), or a press on a row.
    SnippetPick(Option<usize>),
    /// Escape in the `;` picker: it closes and the text stays.
    SnippetClose,
    /// The footer's `{ }`: a `;` in the body, as `Use Snippet Inline` types.
    SnippetInline,
    /// ⌘⇧A or the footer's calendar: the Share Availability menu opens at the caret (`true`) or
    /// closes.
    AvailabilityMenu(bool),
    /// Enter, Tab or a click on the menu's `Insert free times`.
    AvailabilityPick,
    /// A press outside the menu: the menu closes, and the text typed into it is removed.
    AvailabilityDismiss,
    /// The close icon on the `You can now hit ⌘ shift A…` tooltip: the tooltip doesn't show
    /// again.
    DismissAvailabilityTip,
    /// `⌘Enter` / `⌘⇧Enter`.
    Send { mark_done: bool },
    /// A click on Send, which also hints at `⌘Enter`. With ⌘⇧ held, it marks done too.
    ClickSend { mark_done: bool },
    /// `⌘⇧,`, the trash icon.
    Discard,
    /// Escape.
    Escape,
    /// Tab / ⇧Tab between fields.
    Tab(isize),
    /// Enter, `,` or `;` in a recipient field: the typed text becomes a chip.
    Commit,
    /// Backspace in an empty recipient field: select the last chip, or remove the selected ones.
    Backspace,
    /// ⌘⇧P: pop the focused draft out, or back in.
    Pop,
    /// A click on the header's pop button: [`Action::Pop`], which also hints at ⌘⇧P.
    ClickPop,
    /// Escape: the pop-out closes and its draft is kept.
    ClosePopout,
    /// A click on the pop-out's cross: [`Action::ClosePopout`], which also hints at Escape.
    ClickClosePopout,
    /// ⇧C: a new message, popped out.
    NewPoppedOut,
    /// ⇧R, ⇧A (⇧Enter), ⇧F: a reply, reply all or forward, popped out.
    ReplyPoppedOut(DraftAction),
    /// ⌘D: the keyboard moves between the pop-out and the rest of the app.
    CycleFocus,
    /// `Include Original Attachment(s)`: a reply takes the files of the message it answers.
    IncludeOriginals,
    /// Attach (⌘⇧U, the paperclip): the file picker.
    Attach,
    /// `Insert Image`: the picker for images, one of which goes in the body.
    InsertImage,
    /// The cross on an attachment card.
    RemoveAttachment(String),
    /// A click on an attachment card: Quick Look (`preview`), or a copy in Downloads.
    OpenAttachment { uuid: String, preview: bool },
    /// ↑ / ↓ in the suggestions.
    MoveSuggestion(isize),
    /// The pointer moving over a suggestion other than the highlighted one.
    HighlightSuggestion(usize),
    /// The pointer over From's address, which shows the alias in the contact pane.
    HoverFrom,
    /// A suggestion picked with the pointer.
    PickSuggestion(usize),
    /// `Remove Suggestion` on a suggestion.
    RemoveSuggestion(usize),
    /// The chips `chips`, dragged from `from` and dropped into `into` at `at`.
    DropChips {
        from: draft::List,
        chips: Vec<usize>,
        into: draft::List,
        at: usize,
    },
    /// Undo for a removed suggestion: this address is suggested again.
    UndoRemoveSuggestion(String),
    /// Escape with suggestions showing: they close until the text changes.
    CloseSuggestions,
    /// ← / → in a recipient field with nothing typed: move the chip selection.
    MoveSelection(isize),
    /// The `…`: reveal the quote and signature.
    RevealQuote,
    /// A click on the `…`: it reveals the quote, and a hint names the keys.
    ClickRevealQuote,
    /// Backspace on the `…`: drop the quote and signature.
    DeleteQuote,
    /// A click in the body's formatting toolbar or link popover, for its hint.
    Toolbar(exo_editor::ToolbarAction),
    /// The link popover's URL clicked: open it (a `mailto:` in a new draft).
    OpenLink(String),
    /// The toast's Undo on a send.
    UndoSend(u64),
    /// The toast's Instant Send.
    InstantSend(u64),
    /// The toast's Undo on a discard.
    UndoDiscard(u64),
    /// `⌘⇧,` in the list: discard the actionable conversations' drafts.
    DiscardListed,
    /// The toast's Undo on several drafts discarded from the list.
    UndoDiscards(u64),
    /// The toast's Undo on a discarded snippet.
    UndoDiscardSnippet(u64),
    /// An alert's action.
    Alert(usize),
    /// ← / → / Tab in an alert.
    AlertSelect(isize),
    /// Escape or the back gutter in the new-message pane without the keyboard: back to where the
    /// pane was opened from.
    LeavePane,
    /// A click on a failed or waiting send's status, by its message: `RecoverDraft`.
    Recover(String),
    /// `RecoverDraft`'s `Edit Message` on a failed draft.
    EditFailed(String),
    /// `RecoverDraft`'s `Discard` on a failed draft.
    DiscardFailed(String),
    /// `RecoverDraft`'s `Edit Message` on a send waiting to go: it's taken back.
    EditSending(String),
    /// `RecoverDraft`'s `Discard` on a send waiting to go.
    DiscardSending(String),
    /// ⌘⇧L, the palette's `Send Later`, the action pane's Send later: the picker.
    SendLater,
    /// ⌘⇧H, the palette's `Remind Me`, the action pane's Remind button: the Remind me picker for
    /// the draft.
    RemindMe,
    /// A Send later row for `draft`, with its time (`None` for Unschedule and for `now`),
    /// `if no reply`, and the row to remember as `last used`.
    Schedule {
        draft: String,
        at: Option<Timestamp>,
        abort_on_reply: bool,
        remember: Option<exo_store::PreviousSchedule>,
    },
    /// `RecoverDraft`'s `Reschedule` or `Edit Message` on a scheduled send: it's taken back to
    /// edit and keeps its time.
    EditScheduled(String),
    /// `RecoverDraft`'s `Discard` on a scheduled send.
    DiscardScheduled(String),
    /// The Undo in the `Reminder set` toast, on a reminder that the follow-up hint set for a
    /// message just sent as this draft.
    UnremindSent(String),
    /// ⌘⇧I in a draft: Instant Intro's next step.
    InstantIntro,
    /// ⌘⇧I on a conversation: the next Instant Intro step in its draft, or a Reply All that
    /// takes the first step.
    InstantIntroReply,
    /// A key on the recipient list's chips.
    Chip(chips::Key),
    /// A key or press on the `:` emoji or `+`/`@`/`-` contact dropdown.
    Assistant(assistant::Key),
    /// `Use Emoji`: `:hello` in the body, opening the picker.
    UseEmoji,
    /// `Remove External Recipients` and the rest: everyone in that group comes off the draft.
    RemoveRecipients(assistant::Person),
    /// ⌘/ in a draft: the draft pops out (unless it already is), and search opens empty.
    PopAndSearch,
    /// `Compose to {email}`: a new message to the contact pane's person.
    ComposeTo(Address),
    /// `Exclude from Auto Bcc`: its picker, for this sender.
    OpenAutoBccExclusion(String),
    /// An address or domain picked there.
    ExcludeFromAutoBcc(String),
}

/// An alert compose is showing.
pub(crate) struct Alert {
    title: Option<&'static str>,
    text: String,
    actions: Vec<&'static str>,
    selected: usize,
    /// The action Escape takes. It's the last one, except in `Save Snippet?`, where it's Cancel.
    escape: usize,
    /// The draft it's about.
    draft: String,
    then: AlertThen,
    /// The alert's text has the follow-up hint's `H` keycap.
    key: bool,
}

impl Alert {
    fn new(
        title: Option<&'static str>,
        text: impl Into<String>,
        actions: Vec<&'static str>,
        draft: impl Into<String>,
        then: AlertThen,
    ) -> Self {
        Self {
            title,
            text: text.into(),
            escape: actions.len() - 1,
            actions,
            selected: 0,
            draft: draft.into(),
            then,
            key: false,
        }
    }
}

/// What an alert's first action does.
enum AlertThen {
    /// "Send anyway" with these checks waved through, or "Cancel".
    SendAnyway(checks::Bypass, bool),
    /// "OK", focusing From when it's a From problem.
    Ok { focus_from: bool },
    /// "See Message": the draft is reopened.
    SeeMessage,
    /// `Save Snippet?`: Save, Cancel, or Delete / Don't save.
    SaveSnippet,
    /// `Discard changes?` on a snippet: Discard or Cancel.
    DiscardSnippet,
    /// `Snippets`, when there are none to use: Create Snippet or Cancel.
    CreateSnippet,
    /// The follow-up hint after a send: `Set reminder` or `Maybe later`.
    FollowUp,
    /// The Undo Send alert after the send with this toast tag: `Got it`.
    UndoSend(u64),
}

/// A contact a draft shows in the contact pane itself: the suggestion highlighted, or From's alias
/// while it's hovered or once it's chosen.
struct Shown {
    draft: String,
    /// In the new-message pane, whom the pane showed for the draft otherwise
    /// ([`pane_own`]). The contact shows until that changes.
    over: Option<Address>,
    contact: Address,
}

/// Whom the new-message pane's draft has the contact pane show: the chip at the selection's edge
/// while chips are selected, else its last recipient, To through Bcc.
fn pane_own(open: &Open) -> Option<&Address> {
    chips::edge(open).or_else(|| {
        open.record
            .to
            .iter()
            .chain(&open.record.cc)
            .chain(&open.record.bcc)
            .last()
    })
}

/// A reply asked for before its thread was loaded (`R` from the list).
struct PendingReply {
    thread: String,
    action: DraftAction,
    field: Option<Field>,
    /// Popped out once made (⇧R, ⇧A, ⇧F).
    pop_out: bool,
    /// What its body starts with (Share Availability's Insert).
    body: Option<String>,
    /// The text selected in the message it answers, as HTML, which it quotes.
    quote: Option<String>,
    /// A snippet to apply once the reply opens (`⌘;` in a loading thread).
    snippet: Option<exo_store::Snippet>,
}

/// Every draft compose has open, which one has the keyboard, and the sends being held.
#[derive(Default)]
pub(crate) struct Compose {
    drafts: Vec<Open>,
    /// The draft with the keyboard.
    focus: Option<String>,
    /// The new-message pane's draft, and the thread it was opened from.
    pane: Option<(String, Option<String>)>,
    /// The popped-out draft.
    popped: Option<String>,
    /// How many of the answered message's files the focused reply doesn't have.
    missing_originals: usize,
    sends: Vec<Sending>,
    discarded: Option<Discarded>,
    /// Several drafts the list discarded, which a toast can bring back.
    discards: Option<(u64, Vec<Open>)>,
    /// Unopened threads read for `Discard Draft(s)` when a list row holds foreign drafts.
    discard_reads: discard::Reads,
    alert: Option<Alert>,
    pending: Option<PendingReply>,
    /// The thread whose drafts were last opened.
    loaded: Option<String>,
    /// A press landed in the focused draft's form this frame.
    pressed_inside: bool,
    /// The address book as last read, indexed and wrapped in [`Arc`] for reuse across subsystems.
    book: Option<(Arc<Vec<exo_store::ScoredContact>>, Arc<contacts::Index>)>,
    /// The address book has been asked for (again).
    book_asked: bool,
    /// The `excluded_contacts` the book was indexed without.
    book_excluded: std::collections::BTreeMap<String, String>,
    /// The open thread's participants, indexed.
    thread_people: Option<(String, contacts::Index)>,
    /// Replies being sent, shown in their thread.
    in_thread: sent::ShownSends,
    /// A discarded snippet the toast's Undo brings back.
    discarded_snippet: Option<(u64, exo_store::Snippet)>,
    /// The focused draft's recipients as of the last frame, to tell when one is added.
    recipients_seen: Option<(String, Vec<String>)>,
    /// The recipient the focused draft got last, for the contact pane to show.
    just_added: Option<Address>,
    /// The contact a draft last showed in the contact pane itself.
    shown: Option<Shown>,
    /// The follow-up hint holding its send while it's open.
    follow_up: Option<reminder::FollowUp>,
    /// The Undo Send alert holding its send while it's open.
    undo_alert: Option<undo_alert::Held>,
    /// The threads that drafts sent this session went into (a few), the latest last.
    sent_threads: Vec<(String, String)>,
    /// The account's `Send + Mark Done` (`sendAndDone`), as of this frame.
    send_and_done: bool,
    /// The account's `Backtick as Escape` (`enableBacktickAsEscape`), as of this frame.
    backtick_escape: bool,
    /// The thread an Instant Intro's Reply All is waiting for, to take its first step.
    intro_pending: Option<String>,
    /// The drafts' images loading.
    images: images::Loads,
    /// The pointer rests over the contact pane (or over the pop-out, once the pop-out slid for
    /// the pane), so the pop-out slides out of the pane's way.
    still_over_right_pane: bool,
    /// The pop-out's card as last drawn, so the wheel over the card goes to the pop-out.
    popout_card: Option<egui::Rect>,
    /// The command palette has the keyboard this frame, so no draft takes it back.
    pub(crate) palette_open: bool,
    /// Share Availability's `You can now hit ⌘ shift A…` may show on a reply this frame.
    availability_tip: bool,
    next_tag: u64,
}

impl Compose {
    fn tag(&mut self) -> u64 {
        self.next_tag += 1;
        self.next_tag
    }

    /// Gives `view` the thread's latest copy, with the replies being sent in it.
    pub fn set_thread_detail(
        &mut self,
        view: &mut crate::thread::ThreadView,
        detail: Arc<ThreadDetail>,
    ) {
        self.in_thread.set_detail(view, detail);
    }

    /// The region compose holds the keyboard in, if it does.
    pub fn region(&self) -> Option<Region> {
        if self.alert.is_some() {
            return Some(Region::Alert);
        }
        let Some(open) = self.focused() else {
            // The new-message pane is a thread pane.
            return self.pane.is_some().then_some(Region::ThreadMessages);
        };
        Some(if open.state.field.list().is_some() {
            Region::RecipientList
        } else {
            Region::ComposeForm
        })
    }

    /// Whether `message` is a reply just sent, shown in its thread until Gmail's copy takes its
    /// place.
    pub fn is_sent_reply(&self, message: &str) -> bool {
        self.in_thread.draft_of(message).is_some()
    }

    /// The draft with the keyboard.
    pub fn focused(&self) -> Option<&Open> {
        let id = self.focus.as_deref()?;
        self.drafts.iter().find(|open| open.id == id)
    }

    /// The focused draft, and the address book beside it.
    fn focused_with_book(&mut self) -> Option<(&mut Open, Option<&contacts::Index>)> {
        let id = self.focus.clone()?;
        let book = self.book.as_ref().map(|(_, index)| index.as_ref());
        let open = self.drafts.iter_mut().find(|open| open.id == id)?;
        Some((open, book))
    }

    fn focused_mut(&mut self) -> Option<&mut Open> {
        let id = self.focus.clone()?;
        self.drafts.iter_mut().find(|open| open.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Open> {
        self.drafts.iter_mut().find(|open| open.id == id)
    }

    /// Who called off the scheduled sends of the open drafts by replying.
    pub fn unscheduled_by(&self) -> Vec<String> {
        self.drafts
            .iter()
            .filter_map(|open| open.delivery.unscheduled_by.clone())
            .collect()
    }

    /// The focused draft's To addresses and subject.
    pub fn focus_summary(&self) -> Option<(Vec<String>, String)> {
        let open = self.focused()?;
        Some((
            open.record
                .to
                .iter()
                .map(|address| address.email.clone())
                .collect(),
            open.record.subject.clone(),
        ))
    }

    /// How many drafts are open in `thread`.
    pub fn open_in(&self, thread: &str) -> usize {
        self.drafts
            .iter()
            .filter(|open| open.thread.as_deref() == Some(thread))
            .count()
    }

    /// The address book indexed for suggestions, once read ([`crate::Mail::index_address_book`]).
    pub fn book(&self) -> Option<&contacts::Index> {
        self.book.as_ref().map(|(_, index)| index.as_ref())
    }

    /// The new-message pane's draft id, when it's showing.
    pub fn pane(&self) -> Option<&str> {
        self.pane.as_ref().map(|(id, _)| id.as_str())
    }

    /// Whom the new message has the contact pane show while its pane shows (`Some(None)` for
    /// you): the chip at the selection's edge while chips are selected, else its last recipient,
    /// To through Bcc. A contact the draft showed itself ([`Shown`]) shows until that changes.
    pub fn pane_recipient(&self) -> Option<Option<&exo_store::Address>> {
        let (id, _) = self.pane.as_ref()?;
        let open = self.drafts.iter().find(|open| &open.id == id)?;
        let own = pane_own(open);
        if let Some(shown) = self
            .shown
            .as_ref()
            .filter(|shown| shown.draft == *id && shown.over.as_ref() == own)
        {
            return Some(Some(&shown.contact));
        }
        Some(own)
    }

    /// Whom the contact pane is to show for the focused draft, when that draft isn't the
    /// new-message pane's ([`Self::pane_recipient`] covers that one). It's the chip at the
    /// selection's edge, else the recipient just added or the contact the draft showed itself
    /// ([`Shown`]), which this takes.
    pub fn shown_recipient(&mut self) -> Option<Address> {
        let added = self.just_added.take();
        let pane = self.pane().map(str::to_owned);
        let shown = self
            .shown
            .take_if(|shown| pane.as_deref() != Some(shown.draft.as_str()));
        let open = self
            .focused()
            .filter(|open| pane.as_deref() != Some(open.id.as_str()))?;
        let shown = shown
            .filter(|shown| shown.draft == open.id)
            .map(|shown| shown.contact);
        chips::edge(open).cloned().or(added).or(shown)
    }

    /// The focused draft shows `contact` in the contact pane.
    fn show_contact(&mut self, contact: Address) {
        let Some(open) = self.focused() else {
            return;
        };
        let over = (self.pane() == Some(open.id.as_str()))
            .then(|| pane_own(open).cloned())
            .flatten();
        self.shown = Some(Shown {
            draft: open.id.clone(),
            over,
            contact,
        });
    }

    /// The focused draft's highlighted suggestion shows in the contact pane.
    fn show_suggestion(&mut self) {
        let highlighted = self.focused().and_then(|open| {
            let input = open.input(open.state.field.list()?);
            input.suggestions.get(input.highlight).cloned()
        });
        if let Some(contact) = highlighted {
            self.show_contact(contact);
        }
    }

    /// The focused draft's From alias shows in the contact pane.
    fn show_from(&mut self) {
        if let Some(from) = self
            .focused()
            .and_then(|open| open.record.from.clone())
            .filter(|from| !from.email.is_empty())
        {
            self.show_contact(from);
        }
    }

    /// The popped-out draft's id.
    pub fn popped(&self) -> Option<&str> {
        self.popped.as_deref()
    }

    /// Drops every inline draft of `thread` (it was closed), saving or silently discarding each
    /// as [`clean_up`] does.
    fn close_thread_drafts(
        &mut self,
        thread: &str,
        account: &crate::account::Account,
        aliases: &[Alias],
        now: &jiff::Zoned,
    ) {
        let pane = self.pane().map(str::to_owned);
        let popped = self.popped.clone();
        let (closing, kept): (Vec<Open>, Vec<Open>) = std::mem::take(&mut self.drafts)
            .into_iter()
            .partition(|open| {
                open.thread.as_deref() == Some(thread)
                    && pane.as_deref() != Some(open.id.as_str())
                    && popped.as_deref() != Some(open.id.as_str())
            });
        self.drafts = kept;
        for mut open in closing {
            clean_up(&mut open, account, aliases, now);
        }
        if self
            .focus
            .as_ref()
            .is_some_and(|id| !self.drafts.iter().any(|open| &open.id == id))
        {
            self.focus = None;
        }
        self.loaded = None;
    }
}

/// Whether `from` was on `message` as Bcc, so replying all would tell everyone. Addresses
/// compare without case, as mailboxes do.
fn bcc_reply(message: &MessageDetail, from: &str) -> bool {
    message
        .bcc
        .iter()
        .any(|bcc| bcc.email.eq_ignore_ascii_case(from))
}

impl crate::Mail {
    /// The factory's view of the account.
    fn compose_account(&self) -> factory::Account<'_> {
        factory::Account {
            aliases: &self.snapshot.aliases,
            email: self.account.email(),
            tz: self.now.time_zone().clone(),
        }
    }

    /// Carries out a compose action.
    pub(crate) fn compose_run(&mut self, action: Action, now: f64) {
        match action {
            Action::New => self.compose_new(Fields::default(), None),
            Action::ComposeTo(address) => self.compose_new(
                Fields {
                    to: vec![address],
                    ..Fields::default()
                },
                None,
            ),
            Action::PopAndSearch => self.pop_out_and_search(now),
            Action::OpenAutoBccExclusion(email) => self.open_auto_bcc_exclusion(&email),
            Action::ExcludeFromAutoBcc(entry) => self.exclude_from_auto_bcc(entry),
            Action::NoteToSelf => {
                // Note to self goes to the default alias, named `Me`, with the subject selected.
                let from = self.compose_account().default_from();
                let fields = Fields {
                    to: vec![Address {
                        name: Some("Me".into()),
                        email: from.email.clone(),
                    }],
                    subject: "Note to self".into(),
                    ..Fields::default()
                };
                self.compose_new(fields, Some(Field::Subject));
            }
            Action::Reply(action) => self.compose_reply(action, None),
            // `Reply All, Change From` with one alias goes to the body.
            Action::ReplyAllWith(Field::From)
                if form::active_aliases(&self.snapshot.aliases) <= 1 =>
            {
                self.compose_reply(DraftAction::ReplyAll, Some(Field::Body))
            }
            Action::ReplyAllWith(field) => self.compose_reply(DraftAction::ReplyAll, Some(field)),
            Action::EditSubject => self.edit_reply_subject(now),
            Action::ReplyAllEditSubject => self.reply_all_as_new_message(),
            Action::FromToggle
            | Action::FromMove(_)
            | Action::FromChoose(_)
            | Action::FromClose => {
                let aliases = self.snapshot.aliases.clone();
                if let Some(open) = self.compose.focused_mut() {
                    from_menu(open, &aliases, &action, now);
                }
                // The alias chosen shows in the contact pane.
                if matches!(action, Action::FromChoose(_)) {
                    self.compose.show_from();
                }
            }
            Action::HoverFrom => self.compose.show_from(),
            Action::SnippetMove(delta) => {
                if let Some(open) = self
                    .compose
                    .focused_mut()
                    .filter(|open| !open.popups.snippets.rows.is_empty())
                {
                    let snippets = &mut open.popups.snippets;
                    snippets.pick = (snippets.pick as isize + delta)
                        .rem_euclid(snippets.rows.len() as isize)
                        as usize;
                }
            }
            Action::SnippetClose => {
                if let Some(open) = self.compose.focused_mut() {
                    if open.state.field == Field::Subject {
                        subject::dismiss(open);
                    } else {
                        open.editor.dismiss_trigger();
                    }
                    open.popups.snippets.dismiss();
                }
            }
            Action::SnippetPick(index) => self.pick_inline_snippet(index, now),
            Action::SnippetInline => self.snippet_inline(now),
            Action::AvailabilityMenu(open) => self.compose_availability_menu(open),
            Action::AvailabilityDismiss => self.compose_availability_dismiss(),
            Action::DismissAvailabilityTip => self
                .account
                .update_settings(|settings| settings.has_shown_share_availability_tooltip = true),
            Action::AvailabilityPick => self.compose_availability_pick(),
            Action::Format(command) => {
                if let Some(open) = self.compose.focused_mut() {
                    command.run(&mut open.editor);
                    open.touch(now);
                }
            }
            Action::TypeBacktick => {
                if let Some(open) = self.compose.focused_mut() {
                    open.editor.type_text("`");
                    open.touch(now);
                }
            }
            Action::DateActive(text) => {
                if let Some(day) = crate::thread::dates::resolve(&text, &self.now) {
                    self.calendar_show_day(day);
                }
            }
            Action::ShiftDate(days) => {
                let today = self.now.clone();
                if let Some(open) = self.compose.focused_mut()
                    && let Some(date) = open.editor.active_date()
                    && let Some(text) = crate::thread::dates::shift(&date.text, days, &today)
                {
                    open.editor.shift_date(&date, &text);
                    open.touch(now);
                }
            }
            Action::LeavePane => {
                self.leave_pane_to_origin();
            }
            Action::Pop => self.compose_pop(),
            // A click pops the draft out or in, with the hint for its keys.
            Action::ClickPop => {
                self.compose_pop();
                let what = if self.compose.popped.is_some() {
                    "to pop out your draft!"
                } else {
                    "to pop in your draft!"
                };
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["⌘", "shift", "P"])
                        .text(what),
                );
            }
            Action::ClosePopout => self.close_popout(),
            Action::ClickClosePopout => {
                self.close_popout();
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["esc"])
                        .text("to instantly close your draft!"),
                );
            }
            Action::NewPoppedOut => self.compose_new_popped(Fields::default()),
            Action::ReplyPoppedOut(action) => self.compose_reply_popped(action),
            Action::CycleFocus => self.cycle_focus(),
            Action::IncludeOriginals => {
                if let Some(message) = self.focused_original().cloned()
                    && let Some(open) = self.compose.focused_mut()
                {
                    factory::copy_attachments(&mut open.record, &message, true);
                    open.touch(now);
                }
            }
            Action::Focus(field) => {
                if self.compose.focus.is_none()
                    && let Some((id, _)) = &self.compose.pane
                {
                    self.compose.focus = Some(id.clone());
                }
                let aliases = self.snapshot.aliases.clone();
                if let Some(open) = self.compose.focused_mut() {
                    let only_alias = form::active_aliases(&aliases) <= 1;
                    let field = if field == Field::From && only_alias {
                        Field::Body
                    } else {
                        field
                    };
                    let was = open.state.field;
                    match field {
                        Field::SnippetName
                        | Field::Body
                        | Field::QuotingStart
                        | Field::CollapsedHeader
                        | Field::Send
                        | Field::SendLater
                        | Field::RemindMe
                        | Field::Attach
                        | Field::Discard
                        | Field::ExpandRecipients
                        | Field::PopAction
                        | Field::PopClose => open.state.field = field,
                        other => open.state.expand(other),
                    }
                    // Focusing From opens its list. ⌘⇧F in From moves the highlight on.
                    if field == Field::From {
                        let cycle = if was == Field::From {
                            Action::FromMove(1)
                        } else {
                            Action::FromToggle
                        };
                        from_menu(open, &aliases, &cycle, now);
                    }
                }
            }
            Action::Send { mark_done } => self.compose_send(mark_done, checks::Bypass::default()),
            Action::ClickSend { mark_done } => {
                self.compose_send(mark_done, checks::Bypass::default());
                if self.compose.alert.is_none() {
                    self.commands.hint(send_hint());
                }
            }
            Action::Discard => self.compose_discard(),
            Action::Attach => {
                let paths = picker::pick_files(&[]);
                self.compose_attach(&paths, now);
            }
            // Insert Image picks from the image types. One image picked goes in the body, and more
            // go on the draft, as dropped files do.
            Action::InsertImage => {
                let paths = picker::pick_files(&[
                    "jpg", "jpeg", "jif", "jfif", "gif", "png", "bmp", "webp",
                ]);
                self.compose_add_files(&paths, now);
            }
            Action::OpenAttachment { uuid, preview } => self.open_draft_file(&uuid, preview),
            Action::RemoveAttachment(uuid) => {
                // Removing a file asks for no confirmation and has no undo.
                if let Some(open) = self.compose.focused_mut() {
                    open.record
                        .attachments
                        .retain(|attachment| attachment.uuid != uuid);
                    open.touch(now);
                }
            }
            Action::Escape => self.compose_escape(),
            Action::Tab(delta) => {
                let aliases = self.snapshot.aliases.clone();
                let popped = self.compose.popped.clone();
                if let Some((open, book)) = self.compose.focused_with_book() {
                    form::commit_input(open, now, book);
                    let surface = if popped.as_deref() == Some(open.id.as_str()) {
                        Surface::Popout
                    } else {
                        Surface::Inline
                    };
                    open.state.field = form::next_field(open, &aliases, surface, delta);
                    if open.state.field == Field::From {
                        from_menu(open, &aliases, &Action::FromToggle, now);
                    }
                }
            }
            Action::Commit => {
                if let Some((open, book)) = self.compose.focused_with_book() {
                    form::commit_input(open, now, book);
                }
            }
            Action::MoveSuggestion(delta) => {
                if let Some(input) = self
                    .compose
                    .focused_mut()
                    .and_then(|open| open.state.field.list().map(|list| open.input_mut(list)))
                {
                    let count = input.suggestions.len().max(1);
                    input.highlight =
                        (input.highlight as isize + delta).rem_euclid(count as isize) as usize;
                }
                self.compose.show_suggestion();
            }
            Action::HighlightSuggestion(index) => {
                if let Some(input) = self
                    .compose
                    .focused_mut()
                    .and_then(|open| open.state.field.list().map(|list| open.input_mut(list)))
                    .filter(|input| index < input.suggestions.len())
                {
                    input.highlight = index;
                }
                self.compose.show_suggestion();
            }
            Action::PickSuggestion(index) => {
                if let Some((open, book)) = self.compose.focused_with_book()
                    && let Some(list) = open.state.field.list()
                {
                    open.input_mut(list).highlight = index;
                    form::commit_input(open, now, book);
                }
            }
            Action::RemoveSuggestion(index) => self.remove_suggestion(index),
            Action::DropChips {
                from,
                chips,
                into,
                at,
            } => {
                let Some(open) = self.compose.focused_mut() else {
                    return;
                };
                // Chips dropped into a reply's Bcc hint at ⌘⇧I. The recipients are counted before
                // the drop.
                let hint = into == draft::List::Bcc
                    && open.record.in_reply_to.is_some()
                    && open.recipients().count() > 1;
                chips::drop(open, from, &chips, into, at, now);
                self.compose_run(Action::Focus(into.field()), now);
                if hint {
                    self.commands.hint(
                        Hint::new()
                            .text("Next time, hit")
                            .keys(["⌘", "shift", "I"])
                            .text("to instantly move contacts to BCC!"),
                    );
                }
            }
            Action::UndoRemoveSuggestion(email) => self.undo_remove_suggestion(&email),
            Action::CloseSuggestions => {
                if let Some(input) = self
                    .compose
                    .focused_mut()
                    .and_then(|open| open.state.field.list().map(|list| open.input_mut(list)))
                {
                    input.closed = true;
                }
            }
            Action::Backspace => {
                if let Some(open) = self.compose.focused_mut() {
                    form::backspace(open, now);
                }
            }
            Action::MoveSelection(delta) => {
                if let Some((open, book)) = self.compose.focused_with_book() {
                    form::move_selection(open, delta, book, now);
                }
            }
            Action::RevealQuote => {
                let aliases = self.snapshot.aliases.clone();
                if let Some(open) = self.compose.focused_mut() {
                    form::reveal_quote(open, &aliases, now);
                }
            }
            Action::Toolbar(action) => {
                if let Some((before, keys, after)) = action.hint() {
                    self.commands.hint(
                        Hint::new()
                            .text(before)
                            .keys(keys.iter().copied())
                            .text(after),
                    );
                }
            }
            Action::OpenLink(url)
                if url
                    .get(..7)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:")) =>
            {
                self.compose_mailto(&url, false)
            }
            Action::OpenLink(url) => self.open_url(&url),
            Action::ClickRevealQuote => {
                self.compose_run(Action::RevealQuote, now);
                self.commands.hint(
                    Hint::new()
                        .text("Go faster next time: use")
                        .keys(["↓"])
                        .text("to focus")
                        .keys(["…"])
                        .text(", then hit")
                        .keys(["enter"])
                        .text("!"),
                );
            }
            Action::DeleteQuote => {
                if let Some(open) = self.compose.focused_mut() {
                    open.record.quoted_inlined = true;
                    open.state.field = Field::Body;
                    open.touch(now);
                }
            }
            Action::UndoSend(tag) => self.undo_send(tag),
            Action::InstantSend(tag) => {
                if let Some(sending) = self
                    .compose
                    .sends
                    .iter_mut()
                    .find(|sending| sending.tag == tag)
                {
                    sending.until = Timestamp::now();
                    self.account.send_draft(&sending.open.id, sending.until);
                    self.commands
                        .toast(Toast::new("Send accelerated!").tag(tag));
                }
            }
            Action::UndoDiscard(tag) => {
                if let Some(discarded) = self
                    .compose
                    .discarded
                    .take_if(|discarded| discarded.tag == tag)
                {
                    let Discarded {
                        mut open, surface, ..
                    } = discarded;
                    if open.is_stored() {
                        save(
                            &mut open,
                            &self.account,
                            &self.snapshot.aliases,
                            &self.now,
                            false,
                        );
                    }
                    self.reopen(open, surface);
                }
            }
            Action::DiscardListed => self.discard_listed(),
            Action::Recover(message) => self.recover(&message),
            Action::EditFailed(id) => self.edit_failed(&id),
            Action::DiscardFailed(id) => self.discard_failed(&id),
            Action::EditSending(id) => {
                if let Some(tag) = self
                    .compose
                    .sends
                    .iter()
                    .find(|sending| sending.open.id == id)
                    .map(|sending| sending.tag)
                {
                    self.undo_send(tag);
                }
            }
            Action::DiscardSending(id) => self.discard_sending(&id),
            Action::SendLater => self.open_send_later(),
            Action::RemindMe => self.open_draft_reminder(),
            Action::Schedule {
                draft,
                at,
                abort_on_reply,
                remember,
            } => self.schedule_picked(&draft, at, abort_on_reply, remember),
            Action::EditScheduled(id) => self.edit_scheduled(&id),
            Action::DiscardScheduled(id) => self.discard_failed(&id),
            Action::UnremindSent(id) => self.unremind_sent(&id),
            Action::InstantIntro => self.instant_intro(now),
            Action::InstantIntroReply => self.instant_intro_reply(now),
            Action::Assistant(key) => self.assistant_key(key, now),
            Action::UseEmoji => {
                if let Some(open) = self.compose.focused_mut() {
                    assistant::use_emoji(open, now);
                }
            }
            Action::RemoveRecipients(group) => {
                let team = reminder::team_domains(
                    self.account.settings(),
                    &self.snapshot.aliases,
                    self.account.email(),
                );
                if let Some(open) = self.compose.focused_mut() {
                    assistant::remove(open, &group, &team);
                    open.touch(now);
                }
            }
            Action::Chip(key) => {
                if let Some((open, book)) = self.compose.focused_with_book()
                    && let Some(text) = chips::run(open, key, book, now)
                {
                    self.ctx.copy_text(text);
                }
            }
            Action::UndoDiscards(tag) => self.undo_discards(tag),
            Action::UndoDiscardSnippet(tag) => self.undo_discard_snippet(tag),
            Action::Alert(index) => self.alert_action(index),
            Action::AlertSelect(delta) => {
                if let Some(alert) = &mut self.compose.alert {
                    alert.selected = exo_ui::Alert::move_selection(
                        alert.selected,
                        alert.actions.len(),
                        delta,
                        true,
                    );
                }
            }
        }
    }

    /// Keeps the address book (asked for once compose opens) and the open thread's
    /// participants indexed for suggestions.
    pub(super) fn compose_address_book(&mut self) {
        if self.compose.drafts.is_empty() {
            return;
        }
        let book = self.shared_book();
        let Some(detail) = self
            .thread
            .as_ref()
            .and_then(crate::thread::ThreadView::detail)
        else {
            return;
        };
        if self
            .compose
            .thread_people
            .as_ref()
            .is_some_and(|(id, _)| id == detail.id.as_str())
        {
            return;
        }
        let people: Vec<Address> = detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .flat_map(|message| message.from.iter().chain(&message.to).chain(&message.cc))
            .map(|contact| Address {
                name: contact.name.clone(),
                email: contact.email.clone(),
            })
            .collect();
        let empty = contacts::Index::default();
        let index = contacts::participants(&people, book.as_deref().unwrap_or(&empty));
        self.compose.thread_people = Some((detail.id.as_str().to_owned(), index));
    }

    /// Keeps the address book indexed for suggestions (compose's, and the contact pickers'),
    /// asking for it the first time.
    pub(crate) fn index_address_book(&mut self) {
        let excluded = &self.account.settings().excluded_contacts;
        match &self.snapshot.address_book {
            None if !self.compose.book_asked => {
                self.compose.book_asked = true;
                self.account.load_address_book();
            }
            Some(book)
                if self
                    .compose
                    .book
                    .as_ref()
                    .is_none_or(|(indexed, _)| !Arc::ptr_eq(indexed, book))
                    || self.compose.book_excluded != *excluded =>
            {
                self.compose.book = Some((
                    book.clone(),
                    Arc::new(contacts::Index::book_without(book, excluded)),
                ));
                self.compose.book_excluded.clone_from(excluded);
            }
            _ => {}
        }
    }

    /// Returns the shared [`Arc<contacts::Index>`] over the account's address book, building or
    /// requesting it on first use so callers across subsystems (such as calendar guest pickers)
    /// share the same index without duplicating it.
    pub(crate) fn shared_book(&mut self) -> Option<Arc<contacts::Index>> {
        self.index_address_book();
        self.compose
            .book
            .as_ref()
            .map(|(_, index)| Arc::clone(index))
    }
}
