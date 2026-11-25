//! A draft open in compose: its record, its editor, the form's state, and when it counts as
//! empty.

use exo_editor::Editor;
use exo_store::{Address, Alias, DraftAction, DraftRecord, Snippet, StoredDraft};

use super::{assistant, auto_bcc, chips, factory, intro, recipients, signature};

/// A field or button of the form, which can have the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    /// A snippet's name (`Enter description`).
    SnippetName,
    /// The collapsed `Draft to …` header.
    CollapsedHeader,
    To,
    Cc,
    Bcc,
    From,
    Subject,
    Body,
    /// The `…` under the body.
    QuotingStart,
    /// The action pane's Send.
    Send,
    /// The action pane's Send later.
    SendLater,
    /// The action pane's Remind button.
    RemindMe,
    /// The paperclip.
    Attach,
    /// The trash can.
    Discard,
    /// The header's caret that shows Cc, Bcc and From.
    ExpandRecipients,
    /// The header's pop out (or pop in) button.
    PopAction,
    /// The pop-out's close button.
    PopClose,
}

impl Field {
    /// The field's recipient list, if it's a recipient field.
    pub fn list(self) -> Option<List> {
        match self {
            Self::To => Some(List::To),
            Self::Cc => Some(List::Cc),
            Self::Bcc => Some(List::Bcc),
            _ => None,
        }
    }
}

/// A recipient list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum List {
    To,
    Cc,
    Bcc,
}

impl List {
    pub const ALL: [Self; 3] = [Self::To, Self::Cc, Self::Bcc];

    pub fn field(self) -> Field {
        match self {
            Self::To => Field::To,
            Self::Cc => Field::Cc,
            Self::Bcc => Field::Bcc,
        }
    }

    /// The label.
    pub fn label(self) -> &'static str {
        match self {
            Self::To => "To",
            Self::Cc => "Cc",
            Self::Bcc => "Bcc",
        }
    }
}

/// The form's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    /// To (and the rest) shown rather than `Draft to …`.
    pub header_expanded: bool,
    /// Cc, Bcc and From shown.
    pub cc_bcc_shown: bool,
    /// The field that has, or gets, the keyboard.
    pub field: Field,
}

impl State {
    /// The state a draft opens in. A reply that has recipients opens collapsed at its body, a
    /// subject-line edit at its subject, and anything else at To. Cc and Bcc show when the header
    /// is expanded and they aren't empty.
    pub fn default_for(record: &DraftRecord) -> Self {
        let has_recipients =
            !record.to.is_empty() || !record.cc.is_empty() || !record.bcc.is_empty();
        let reply = matches!(record.action, DraftAction::Reply | DraftAction::ReplyAll);
        let (header_expanded, field) = match record.action {
            DraftAction::EditSubjectLineReply => (true, Field::Subject),
            DraftAction::Forward => (true, Field::To),
            _ if reply && has_recipients => (false, Field::Body),
            _ => (true, Field::To),
        };
        let cc_bcc_shown = header_expanded && (!record.cc.is_empty() || !record.bcc.is_empty());
        Self {
            header_expanded,
            cc_bcc_shown,
            field,
        }
    }

    /// Shows everything and moves to `field`.
    pub fn expand(&mut self, field: Field) {
        self.header_expanded = true;
        self.cc_bcc_shown = true;
        self.field = field;
    }
}

/// What a recipient field is doing: the text being typed, which chips are selected, and the
/// suggestions for the text.
#[derive(Clone, Debug, Default)]
pub(crate) struct Input {
    pub text: String,
    /// The selected chips' indices, sorted, or empty when the caret is in the input.
    pub selected: Vec<usize>,
    /// The chip a shift-selection extends from.
    pub anchor: Option<usize>,
    /// The text `suggestions` are for.
    pub query: String,
    /// Suggested contacts for the text, best first.
    pub suggestions: Vec<Address>,
    /// Which suggestions are the thread's own participants, which can't be removed.
    pub from_thread: Vec<bool>,
    /// The suggestion just removed, which ⌘Z brings back until
    /// something's typed.
    pub just_excluded: Option<Address>,
    /// The highlighted suggestion.
    pub highlight: usize,
    /// Escape closed the suggestions, and typing opens them again.
    pub closed: bool,
    /// The suggestion list's width since it opened, which only grows while the list shows.
    pub list_width: f32,
    /// The chip being edited where it stands rather than at the end. The input stands in for the
    /// chip, holding the chip's text.
    pub editing: Option<usize>,
    /// The input's text is to be selected whole as it next gets the keyboard.
    pub select_text: bool,
    /// Where the caret was in the text as last drawn, in characters, when nothing in the text was
    /// selected. At either end, ← or → finishes the edit.
    pub caret: Option<usize>,
}

impl Input {
    /// Whether the suggestions show.
    pub fn suggesting(&self) -> bool {
        !self.closed && !self.suggestions.is_empty()
    }
}

/// The sender and recipient lists a draft's `People` were built from.
pub(crate) type PeopleSource = (Option<Address>, Vec<Address>, Vec<Address>, Vec<Address>);

/// Persistence, foreign-draft adoption, autosave timing, and Gmail upload status.
pub(crate) struct SyncState {
    /// For a draft written elsewhere that Exo hasn't saved yet, its Gmail message.
    pub adopt: Option<String>,
    /// When it first changed since the last save.
    pub changed: Option<f64>,
    /// When it was last saved.
    pub saved_at: f64,
    /// The record as the store has it, once the store has one: what it was last saved as, or
    /// opened from. A save that wouldn't change this record isn't made, so a draft is saved only
    /// when a field is set to a different value.
    pub saved: Option<DraftRecord>,
    /// How the draft's files stand with Gmail, as the store last said, and whether Gmail is in
    /// reach, for the file cards' upload bars and tooltips.
    pub upload: exo_store::Upload,
    pub online: bool,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            adopt: None,
            changed: None,
            saved_at: f64::NEG_INFINITY,
            saved: None,
            upload: exo_store::Upload::default(),
            online: true,
        }
    }
}

/// Transient UI and undo state for the header's recipient rows, `From` picker, and subject line.
#[derive(Default)]
pub(crate) struct HeaderState {
    pub inputs: [Input; 3],
    /// The From list is open, with this row highlighted.
    pub from_menu: Option<usize>,
    /// The subject's whole text is selected when it next gets the keyboard.
    pub select_subject: bool,
    /// The subject's last Unicode replacement, which Backspace right after it undoes.
    pub subject_replacement: Option<SubjectReplacement>,
    /// The recipients' undo and redo.
    pub history: chips::History,
    /// Chips being dragged, while the button is down.
    pub chip_drag: Option<chips::Drag>,
    /// The header's scroll while the recipients pass the header's max height.
    pub scroll: super::form::BodyScroll,
}

/// Transient layout, scroll, and editor-sync state for the body area.
pub(crate) struct BodyState {
    /// The editor's last height, for the layout above it.
    pub height: f32,
    /// The body's scroll while the form is at its max height.
    pub scroll: super::form::BodyScroll,
    /// The detected date the body's caret was last on, while it had the keyboard.
    pub date: Option<exo_editor::ActiveDate>,
    /// `includeSignatureOnReplies`, as the account's settings last said
    /// ([`super::signature::includes_content`]).
    pub signature_on_replies: bool,
    /// The address book the body's names were last taken from (the address of the book's `Arc`,
    /// or `None` before the book is read). Autocorrect leaves those names alone. `None` until the
    /// names are taken.
    pub names_from: Option<Option<usize>>,
    /// The sender and recipients `editor`'s `People` were last built from.
    pub people_source: Option<PeopleSource>,
}

impl Default for BodyState {
    fn default() -> Self {
        Self {
            height: 0.0,
            scroll: super::form::BodyScroll::default(),
            date: None,
            signature_on_replies: true,
            names_from: None,
            people_source: None,
        }
    }
}

/// State of the inline `;` snippet picker.
#[derive(Default)]
pub(crate) struct SnippetPicker {
    /// The `;` being typed in the body or subject, as last reported.
    pub trigger: Option<exo_editor::Trigger>,
    /// The snippets the `;` finds, best first.
    pub rows: Vec<SnippetRow>,
    /// There are no snippets at all, so `To create a Snippet, hit ⌘ K → "Snippets"` shows instead.
    pub hint: bool,
    /// The highlighted row of the `;` picker.
    pub pick: usize,
}

impl SnippetPicker {
    pub fn dismiss(&mut self) {
        self.trigger = None;
        self.rows.clear();
        self.hint = false;
    }
}

/// Inline autocomplete and caret popups attached to the subject or body.
#[derive(Default)]
pub(crate) struct Popups {
    /// The `:` emoji and `+`/`@`/`-` contact dropdowns.
    pub assistant: assistant::State,
    /// The subject's `:` and `;` pickers.
    pub subject: super::subject::State,
    /// The `;` inline snippet picker.
    pub snippets: SnippetPicker,
    /// Share Availability's menu, open at the caret.
    pub availability: Option<super::availability::Menu>,
}

/// Send-time, scheduling, reminder, Auto Bcc, and Instant Intro bookkeeping.
#[derive(Default)]
pub(crate) struct DeliveryState {
    /// The snippets used in the draft, whose sends are counted once the draft is sent.
    pub applied_snippets: Vec<String>,
    /// Who replied and so called off the draft's scheduled send, as the Send later button names
    /// them in `Unscheduled: {name} replied`.
    pub unscheduled_by: Option<String>,
    /// Automatic Reminders' view of the draft.
    pub automatic: super::reminder::Automatic,
    /// Auto Bcc's bookkeeping.
    pub auto_bcc: auto_bcc::State,
    /// Where Instant Intro's cycle is.
    pub intro: intro::Cycle,
}

/// A draft compose has open.
pub(crate) struct Open {
    /// Exo's draft id.
    pub id: String,
    /// Its Gmail thread, once it has one.
    pub thread: Option<String>,
    /// Everything but the body, which is the editor's.
    pub record: DraftRecord,
    pub editor: Editor,
    pub state: State,
    /// Where the body's caret was last drawn.
    pub caret: egui::Rect,
    /// The snippet this edits, when the draft is a snippet. A snippet is never a Gmail draft.
    snippet: Option<SnippetEdit>,
    pub sync: SyncState,
    pub header: HeaderState,
    pub body: BodyState,
    pub popups: Popups,
    pub delivery: DeliveryState,
}

/// A row of the `;` picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnippetRow {
    pub id: String,
    pub name: String,
    pub preview: String,
}

/// A snippet open in compose: its name, and the snippet as last saved (`None` until it's
/// first saved), to compare unsaved changes against.
#[derive(Clone, Debug)]
struct SnippetEdit {
    name: String,
    saved: Option<Snippet>,
}

/// A Unicode replacement just made in the subject.
#[derive(Clone, Debug)]
pub(crate) struct SubjectReplacement {
    /// Where the replacement left the caret.
    pub caret: usize,
    /// The subject as typed, before the replacement.
    pub typed: String,
    /// The caret in that text.
    pub typed_caret: usize,
}

impl Open {
    /// A draft just made by the factory, not saved yet.
    pub fn new(id: String, thread: Option<String>, record: DraftRecord) -> Self {
        let editor = if record.body.is_empty() {
            Editor::new()
        } else {
            Editor::from_html(&record.body)
        };
        let state = State::default_for(&record);
        Self {
            id,
            thread,
            record,
            editor,
            state,
            caret: egui::Rect::NOTHING,
            snippet: None,
            sync: SyncState::default(),
            header: HeaderState::default(),
            body: BodyState::default(),
            popups: Popups::default(),
            delivery: DeliveryState::default(),
        }
    }

    /// A saved draft, opened again.
    pub fn stored(draft: &StoredDraft) -> Self {
        let mut open = Self::new(
            draft.id.clone(),
            draft
                .thread
                .as_ref()
                .map(|thread| thread.as_str().to_owned()),
            draft.record.clone(),
        );
        open.mark_saved();
        open
    }

    /// Configures this open form to edit a private snippet rather than a Gmail draft.
    pub fn set_snippet(&mut self, name: String, saved: Option<Snippet>, header_shown: bool) {
        self.state = State {
            header_expanded: header_shown,
            cc_bcc_shown: header_shown,
            field: Field::SnippetName,
        };
        self.snippet = Some(SnippetEdit { name, saved });
    }

    /// Mutable access to the snippet's name when editing a snippet.
    pub fn snippet_name_mut(&mut self) -> Option<&mut String> {
        self.snippet.as_mut().map(|edit| &mut edit.name)
    }

    /// Whether this is a snippet being edited.
    pub fn is_snippet(&self) -> bool {
        self.snippet.is_some()
    }

    /// Whether this draft syncs to and discards from Gmail (snippets live in settings instead).
    pub fn persists_to_gmail(&self) -> bool {
        !self.is_snippet()
    }

    /// Whether this is a snippet that has never been saved yet.
    pub fn is_new_snippet(&self) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|edit| edit.saved.is_none())
    }

    /// Whether this is an existing saved snippet opened for editing.
    pub fn is_saved_snippet(&self) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|edit| edit.saved.is_some())
    }

    /// Whether a snippet being edited has nothing in any field.
    pub fn is_snippet_empty(&self) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|edit| edit.name.trim().is_empty())
            && self.is_body_empty()
            && self.recipients().next().is_none()
            && self.record.subject.trim().is_empty()
            && self.record.attachments.is_empty()
    }

    /// Whether a snippet being edited differs from what was last saved (or has anything in it when
    /// new).
    pub fn is_snippet_dirty(&self) -> bool {
        let Some(edit) = &self.snippet else {
            return false;
        };
        match &edit.saved {
            None => !self.is_snippet_empty(),
            Some(saved) => {
                let record = &self.record;
                edit.name != saved.name
                    || record.subject != saved.subject
                    || self.editor.html() != saved.body
                    || record.to != saved.to
                    || record.cc != saved.cc
                    || record.bcc != saved.bcc
                    || record.from != saved.from
                    || record.attachments != saved.attachments
            }
        }
    }

    /// Builds the [`Snippet`] value that `Done` saves to settings (without date and analytics).
    pub fn to_snippet(&self) -> Option<Snippet> {
        let edit = self.snippet.as_ref()?;
        let record = &self.record;
        Some(Snippet {
            id: self.id.clone(),
            name: edit.name.clone(),
            subject: record.subject.clone(),
            body: self.editor.html(),
            to: record.to.clone(),
            cc: record.cc.clone(),
            bcc: record.bcc.clone(),
            from: record.from.clone(),
            attachments: record.attachments.clone(),
            date: String::new(),
            analytics: exo_store::SnippetAnalytics::default(),
        })
    }

    /// The Gmail message id of a foreign draft that Exo hasn't saved yet, if any.
    pub fn adopted_message(&self) -> Option<&str> {
        self.sync.adopt.as_deref()
    }

    /// Whether this draft was adopted from Gmail and hasn't been saved by Exo yet.
    pub fn is_adopted(&self) -> bool {
        self.sync.adopt.is_some()
    }

    /// Whether closing or leaving this draft should silently drop it rather than keep it in Gmail.
    pub fn can_discard_silently(&self) -> bool {
        self.persists_to_gmail() && !self.is_adopted() && self.is_unmodified()
    }

    /// Returns when this draft first changed since its last Gmail save, clearing `changed` if it
    /// is still an untouched unsaved draft that doesn't warrant a Gmail draft yet.
    pub fn pending_autosave(&mut self) -> Option<f64> {
        if !self.persists_to_gmail() {
            return None;
        }
        let since = self.sync.changed?;
        if !self.is_stored() && self.can_discard_silently() {
            self.sync.changed = None;
            return None;
        }
        Some(since)
    }

    /// The store has it.
    pub fn is_stored(&self) -> bool {
        self.sync.saved.is_some()
    }

    /// Takes its record, the editor's body included, as what the store has.
    pub fn mark_saved(&mut self) {
        self.sync_body();
        self.sync.saved = Some(self.record.clone());
    }

    /// Whether it differs from what the store has, taking the editor's body into
    /// the record first.
    pub fn is_dirty(&mut self) -> bool {
        self.sync_body();
        self.sync.saved.as_ref() != Some(&self.record)
    }

    /// A recipient list's chips.
    pub fn list(&self, list: List) -> &Vec<Address> {
        match list {
            List::To => &self.record.to,
            List::Cc => &self.record.cc,
            List::Bcc => &self.record.bcc,
        }
    }

    pub fn list_mut(&mut self, list: List) -> &mut Vec<Address> {
        match list {
            List::To => &mut self.record.to,
            List::Cc => &mut self.record.cc,
            List::Bcc => &mut self.record.bcc,
        }
    }

    pub fn input(&self, list: List) -> &Input {
        &self.header.inputs[list as usize]
    }

    pub fn input_mut(&mut self, list: List) -> &mut Input {
        &mut self.header.inputs[list as usize]
    }

    /// Notes a change, to be saved within the second.
    pub fn touch(&mut self, now: f64) {
        self.sync.changed.get_or_insert(now);
    }

    /// Whether the field with the keyboard takes typing (the body, the subject, a recipient
    /// list, a snippet's name), where `;` is text.
    pub fn takes_text(&self) -> bool {
        match self.state.field {
            Field::To | Field::Cc | Field::Bcc | Field::Body | Field::SnippetName => true,
            Field::Subject => self.is_new_email(),
            _ => false,
        }
    }

    /// Whether this is a new message rather than a reply or forward.
    pub fn is_new_email(&self) -> bool {
        matches!(
            self.record.action,
            DraftAction::Compose | DraftAction::EditSubjectLineReply
        )
    }

    /// Whether the body has nothing in it.
    pub fn is_body_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// Whether it's untouched: a new message with nothing in it, or a reply whose body is empty
    /// and whose recipients and files are as created, Auto Bcc's addresses aside. Such a draft is
    /// discarded silently when it closes.
    pub fn is_unmodified(&self) -> bool {
        if !self.is_body_empty() {
            return false;
        }
        let bcc_untouched = self.record.bcc.iter().all(|address| {
            self.delivery
                .auto_bcc
                .emails
                .contains(&address.email.to_lowercase())
        });
        if self.is_new_email() {
            self.record.to.is_empty()
                && self.record.cc.is_empty()
                && bcc_untouched
                && self.record.subject.trim().is_empty()
                && self.record.attachments.is_empty()
        } else {
            bcc_untouched && factory::fingerprint(&self.record) == self.record.fingerprint
        }
    }

    /// There's a quote or signature behind a `…`.
    pub fn has_hidden_quote(&self, aliases: &[Alias]) -> bool {
        self.persists_to_gmail()
            && !self.record.quoted_inlined
            && (self.record.quoted_content.is_some()
                || signature::of(self.record.from.as_ref(), aliases).is_some())
    }

    /// The draft's Send later time, unless a reply called the send off.
    pub fn scheduled_at(&self) -> Option<jiff::Timestamp> {
        self.record
            .send_later
            .as_ref()
            .filter(|_| self.delivery.unscheduled_by.is_none())
            .map(|later| later.at)
    }

    /// Every recipient, To, Cc then Bcc.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.record
            .to
            .iter()
            .chain(&self.record.cc)
            .chain(&self.record.bcc)
    }

    /// Whether it has a recipient, and all of them are valid.
    pub fn has_valid_recipients(&self) -> bool {
        self.recipients().next().is_some() && self.recipients().all(recipients::sendable)
    }

    /// Takes the editor's body into the record, as saving does.
    pub fn sync_body(&mut self) {
        self.record.body = self.editor.html();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::DraftRecord;

    fn record(action: DraftAction, to: &[&str]) -> DraftRecord {
        let mut record = DraftRecord {
            action,
            from: None,
            to: to
                .iter()
                .map(|email| Address {
                    name: None,
                    email: (*email).into(),
                })
                .collect(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "x@y".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        record.fingerprint = factory::fingerprint(&record);
        record
    }

    #[test]
    fn a_reply_opens_at_its_body_and_the_rest_at_to() {
        let reply = State::default_for(&record(DraftAction::Reply, &["a@x.com"]));
        assert_eq!((reply.header_expanded, reply.field), (false, Field::Body));
        let lonely = State::default_for(&record(DraftAction::Reply, &[]));
        assert_eq!((lonely.header_expanded, lonely.field), (true, Field::To));
        let forward = State::default_for(&record(DraftAction::Forward, &[]));
        assert_eq!((forward.header_expanded, forward.field), (true, Field::To));
        let new = State::default_for(&record(DraftAction::Compose, &[]));
        assert_eq!(
            (new.header_expanded, new.cc_bcc_shown, new.field),
            (true, false, Field::To)
        );
    }

    #[test]
    fn untouched_drafts_are_unmodified() {
        let mut reply = Open::new("d".into(), None, record(DraftAction::Reply, &["a@x.com"]));
        assert!(reply.is_unmodified());
        reply.record.to.push(Address {
            name: None,
            email: "b@x.com".into(),
        });
        assert!(!reply.is_unmodified(), "recipients changed");
        let mut new = Open::new("d".into(), None, record(DraftAction::Compose, &[]));
        assert!(new.is_unmodified());
        new.record.subject = "Hi".into();
        assert!(!new.is_unmodified());
        let mut typed = Open::new("d".into(), None, record(DraftAction::Compose, &[]));
        typed.editor.type_text("Hello");
        assert!(!typed.is_unmodified());
    }

    #[test]
    fn a_file_added_modifies_a_draft() {
        let file = exo_store::DraftAttachment {
            uuid: "f".into(),
            name: "notes.txt".into(),
            mime_type: "text/plain".into(),
            size: 5,
            content_id: None,
            source: exo_store::AttachmentSource::Local,
        };
        for action in [DraftAction::Reply, DraftAction::Compose] {
            let mut open = Open::new(
                "d".into(),
                None,
                record(
                    action,
                    &["a@x.com"][..usize::from(action == DraftAction::Reply)],
                ),
            );
            open.record.attachments.push(file.clone());
            assert!(!open.is_unmodified(), "{action:?} with a file");
        }
        // A forward made with its message's files is untouched until they change.
        let mut forward = record(DraftAction::Forward, &[]);
        forward.attachments.push(file);
        forward.fingerprint = factory::fingerprint(&forward);
        assert!(Open::new("d".into(), None, forward).is_unmodified());
    }

    #[test]
    fn snippets_and_adopted_drafts_follow_their_own_lifecycle() {
        let mut draft = Open::new("d".into(), None, record(DraftAction::Compose, &[]));
        assert!(draft.persists_to_gmail() && draft.can_discard_silently());
        draft.touch(1.0);
        assert_eq!(
            draft.pending_autosave(),
            None,
            "an untouched unsaved draft clears changed without autosaving"
        );
        draft.sync.adopt = Some("gmail-msg".into());
        assert!(draft.is_adopted() && !draft.can_discard_silently());

        let mut snippet = Open::new("s".into(), None, record(DraftAction::Compose, &[]));
        snippet.set_snippet(String::new(), None, false);
        assert!(snippet.is_snippet() && snippet.is_new_snippet() && !snippet.persists_to_gmail());
        assert!(snippet.is_snippet_empty() && !snippet.is_snippet_dirty());
        snippet.touch(2.0);
        assert_eq!(snippet.pending_autosave(), None, "snippets never autosave");
        if let Some(name) = snippet.snippet_name_mut() {
            *name = "Intro".into();
        }
        assert!(!snippet.is_snippet_empty() && snippet.is_snippet_dirty());
    }
}
