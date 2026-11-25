//! The compose assistant shows the emoji and contact dropdowns in the body. `:smi` offers emoji
//! by name. `+pri` or `@pri` offers contacts, and picking one writes `+Priya` and puts her in To.
//! `-pri` offers the recipients (and `all external recipients`, `all internal recipients`,
//! `all @domain`), and picking one writes `−Priya` and takes them off the draft. The trigger the
//! editor reports ([`exo_editor::Trigger`]) opens the dropdowns. The `;` snippet picker lives in
//! [`super::snippets`].

use std::collections::HashMap;

use egui::{Color32, Id, Rect, Ui, pos2, vec2};
use exo_commands::{Command, Commands, Hint, Region};
use exo_editor::{Trigger, TriggerKind};
use exo_store::{Address, EmojiSkinTone};
use exo_ui::{
    AnchoredCard, AnchoredList, ContactRow, ContactSelectorList, Family, Font, TextStyle, Theme,
    measure_contact_columns,
};

use super::{Action, Open, contacts, recipients};
use crate::{commands, emoji};

/// The emoji picker: 200px wide, 36px rows inside `padding: 8px 0`, 12px in; the emoji at 24px
/// in a 25px column, its name 16px after, 14px/24px.
const EMOJI_WIDTH: f32 = 200.0;
const EMOJI_ROW: f32 = 36.0;
const EMOJI_PADDING: f32 = 8.0;
const EMOJI_INSET: f32 = 12.0;
/// A window under 550px tall shortens the picker to 100px.
const SHORTENED_BELOW: f32 = 550.0;
const SHORTENED_HEIGHT: f32 = 100.0;
const PREVIEW: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 24.0, 33.0);
const PREVIEW_WIDTH: f32 = 25.0;
const NAME: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 24.0);
const NAME_GAP: f32 = 16.0;
/// The contact list: 7px above and below its rows, each row a 28px line set 4px down, cells
/// padded 16px, the name column at least 165px wide, and `font-kerning: none`.
const CONTACT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 28.0);
const CONTACT_ROW: f32 = 32.0;
const CONTACT_PADDING: f32 = 7.0;
const CELL_PADDING: f32 = 16.0;
const NAME_MIN: f32 = 165.0;
/// How far each dropdown sits to the left of its trigger character's left edge.
fn left_offset(kind: TriggerKind) -> f32 {
    match kind {
        TriggerKind::Emoji => 50.0,
        TriggerKind::Contact('+') => 9.0,
        TriggerKind::Contact('@') => 5.0,
        _ => 12.0,
    }
}
/// How many emoji the picker offers: eight, or five in a popped-out draft.
const EMOJI_LIMIT: usize = 8;
const EMOJI_LIMIT_POPPED: usize = 5;

/// A draft's To, Cc and Bcc.
type Recipients = [Vec<Address>; 3];

/// A contact row: someone, or one of the groups that `−` offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Person {
    Contact(Address),
    /// `all external recipients`.
    External,
    /// `all internal recipients`.
    Internal,
    /// `all @{domain}`.
    Domain(String),
}

/// What the dropdown offers.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Rows {
    #[default]
    None,
    Emoji(Vec<emoji::Found>),
    Contacts(Vec<Person>),
    /// No one matched, but what's typed starts an address. The row shows that text alone,
    /// lower-cased, and highlighted when it's a valid address.
    NewAddress {
        text: String,
        valid: bool,
    },
}

/// A draft's assistant.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    /// The `:` or `+`/`@`/`-` being typed, as the editor last reported it.
    pub trigger: Option<Trigger>,
    pub rows: Rows,
    /// The highlighted row.
    pub index: usize,
    /// The query the rows are for.
    query: Option<(TriggerKind, String)>,
    /// The name of the To recipient whose mention the caret backspaced into. The person picked
    /// instead takes that recipient's place.
    partial: Option<String>,
    /// Recipients before and after each change a mention made, keyed by the mention's undo mark,
    /// so undoing the mention in the body undoes the change.
    marks: HashMap<u64, (Recipients, Recipients)>,
    next_mark: u64,
    /// How far the shortened emoji picker is scrolled.
    scroll: f32,
    /// ↑ or ↓ moved the highlight, so the shortened picker scrolls the highlighted row to the top.
    reveal: bool,
}

impl State {
    fn shown(&self) -> bool {
        self.trigger.is_some() && self.rows != Rows::None
    }
}

/// What a key does to the dropdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// ↓ (1) or ↑ (-1), wrapping.
    Move(isize),
    /// Enter or Tab picks the highlighted row, and a press on a row picks that row.
    Pick(Option<usize>),
    /// `:` after an emoji's name picks the emoji named exactly that, else the highlighted one.
    Close,
    /// Escape closes the dropdown and leaves the text.
    Dismiss,
}

/// Takes the editor's trigger for this frame (emoji and contacts only).
pub(super) fn note_trigger(open: &mut Open, trigger: Option<&Trigger>) {
    open.popups.assistant.trigger = trigger
        .filter(|trigger| matches!(trigger.kind, TriggerKind::Emoji | TriggerKind::Contact(_)))
        .cloned();
    if open.popups.assistant.trigger.is_none() {
        open.popups.assistant.rows = Rows::None;
        open.popups.assistant.query = None;
        open.popups.assistant.partial = None;
    }
}

/// Backspace (or ⌘Z) at the end of a `+Name` mention whose Name is in To opens the contacts
/// again while the mention is edited, so that recipient can be swapped. Call before the body
/// sees the key.
pub(super) fn edit_mention(open: &mut Open, book: Option<&contacts::Index>) {
    let Some(name) = open.editor.mention_before_caret() else {
        return;
    };
    let lower = name.to_lowercase();
    let named = |address: &Address| {
        contacts::best_known_first_name(address, book)
            .is_some_and(|first| first.to_lowercase() == lower)
            || address
                .name
                .as_deref()
                .is_some_and(|full| full.to_lowercase() == lower)
            || address.email.to_lowercase() == lower
    };
    if open.record.to.iter().any(named) {
        open.popups.assistant.partial = Some(name);
        open.editor.arm_trigger();
    }
}

/// The To recipient that `name` stands for, when only one does. It matches by first name, else
/// full name, else address.
fn unambiguous(to: &[Address], name: &str, book: Option<&contacts::Index>) -> Option<Address> {
    let lower = name.to_lowercase();
    let only = |matches: Vec<&Address>| {
        if let [one] = matches[..] {
            Some(one.clone())
        } else {
            None
        }
    };
    only(
        to.iter()
            .filter(|address| {
                contacts::best_known_first_name(address, book)
                    .is_some_and(|first| first.to_lowercase() == lower)
            })
            .collect(),
    )
    .or_else(|| {
        only(
            to.iter()
                .filter(|address| {
                    address
                        .name
                        .as_deref()
                        .is_some_and(|full| full.to_lowercase() == lower)
                })
                .collect(),
        )
    })
    .or_else(|| {
        only(
            to.iter()
                .filter(|address| address.email.to_lowercase() == lower)
                .collect(),
        )
    })
}

/// Puts the recipients back as they were before (or after) the mentions whose undo marks the
/// editor handed back.
pub(super) fn undo_marks(open: &mut Open, undone: &[u64], redone: &[u64]) {
    for (marks, before) in [(undone, true), (redone, false)] {
        for mark in marks {
            if let Some((was, became)) = open.popups.assistant.marks.get(mark).cloned() {
                let [to, cc, bcc] = if before { was } else { became };
                open.record.to = to;
                open.record.cc = cc;
                open.record.bcc = bcc;
            }
        }
    }
}

/// Registers `Use Emoji` (`USE_EMOJI`, after `Use Snippet`) for the draft with the keyboard.
pub(super) fn register_use_emoji(commands: &mut Commands<commands::Action>) {
    commands.add(
        Command::new(
            "USE_EMOJI",
            "Use Emoji",
            commands::Action::Compose(Action::UseEmoji),
        )
        .follow("USE_SNIPPET")
        .aliases(["Insert Emoji"])
        .icon("data/emoji")
        .regions(&[Region::ComposeForm, Region::RecipientList]),
    );
}

/// `Use Emoji` types `:hello`, which opens the picker. In a new message's subject it goes in at
/// the caret ([`super::subject::use_emoji`]). Otherwise the body gets the keyboard (with the caret
/// at the end unless the body already had the keyboard), and a space goes first if the caret
/// follows a character.
pub(super) fn use_emoji(open: &mut Open, now: f64) {
    if open.state.field == super::Field::Subject && open.is_new_email() {
        super::subject::use_emoji(open);
        open.touch(now);
        return;
    }
    if open.state.field != super::Field::Body {
        let end = open.editor.document().end();
        open.editor.set_selection(exo_editor::Selection::caret(end));
        open.state.field = super::Field::Body;
    }
    let caret = open.editor.selection().focus;
    let before = open.editor.document().blocks[caret.block]
        .text()
        .chars()
        .take(caret.offset)
        .last();
    if before.is_some_and(|ch| !ch.is_whitespace()) {
        open.editor.type_text("\u{a0}");
    }
    open.editor.type_text(":hello");
    open.touch(now);
}

/// Registers the dropdown's keys, over the body's, while the dropdown shows. `backtick` is the
/// Backtick as Escape setting, without which a backtick is typed rather than closing the
/// dropdown. While the dropdown is hidden on a date ([`show`]), it doesn't take the keys, so they
/// go to the body.
pub(super) fn register(commands: &mut Commands<commands::Action>, open: &Open, backtick: bool) {
    let assistant = &open.popups.assistant;
    if !assistant.shown() || open.body.date.is_some() {
        return;
    }
    const BODY: &[Region] = &[Region::ComposeForm];
    let key = |id: &'static str, key: Key, keys: &[&'static str]| {
        Command::shortcut(id, commands::Action::Compose(Action::Assistant(key)))
            .keys(keys.iter().copied())
            .regions(BODY)
    };
    let picks: &[&str] = if matches!(assistant.rows, Rows::NewAddress { .. }) {
        &["enter", "tab", "space"]
    } else {
        &["enter", "tab"]
    };
    commands.add(key("ASSISTANT_PICK", Key::Pick(None), picks));
    commands.add(key("ASSISTANT_ESCAPE", Key::Dismiss, &["esc"]).backtick_escape(backtick));
    if !matches!(assistant.rows, Rows::NewAddress { .. }) {
        commands.add(key("ASSISTANT_DOWN", Key::Move(1), &["down"]));
        commands.add(key("ASSISTANT_UP", Key::Move(-1), &["up"]));
    }
    if matches!(assistant.rows, Rows::Emoji(_)) {
        commands.add(key("ASSISTANT_COLON", Key::Close, &[":"]));
    }
}

/// The emoji rows for a `:` query, which needs two characters at least, except for queries
/// starting with `a`, `b`, `m`, `o`, `v`, `x`, `+` or `-`.
fn emoji_rows(query: &str, popped: bool, tone: Option<EmojiSkinTone>) -> Rows {
    let short =
        query.chars().count() < 2 && !query.starts_with(['a', 'b', 'm', 'o', 'v', 'x', '+', '-']);
    if short {
        return Rows::None;
    }
    let found = emoji::search(
        query,
        if popped {
            EMOJI_LIMIT_POPPED
        } else {
            EMOJI_LIMIT
        },
        tone,
    );
    if found.is_empty() {
        Rows::None
    } else {
        Rows::Emoji(found)
    }
}

/// The contacts for a `+`/`@` query: up to five suggestions as the recipient fields have them,
/// leaving out the draft's From. When none match, the address being typed.
fn contact_rows(
    open: &Open,
    query: &str,
    book: Option<&contacts::Index>,
    thread: Option<&contacts::Index>,
) -> Rows {
    let from = open
        .record
        .from
        .as_ref()
        .map(|from| from.email.to_lowercase());
    let found: Vec<Person> = match book {
        Some(book) => contacts::suggest(book, thread.unwrap_or(&contacts::Index::default()), query)
            .into_iter()
            .filter(|address| Some(address.email.to_lowercase()) != from)
            .map(Person::Contact)
            .collect(),
        None => Vec::new(),
    };
    if !found.is_empty() {
        return Rows::Contacts(found);
    }
    if recipients::is_address_start(query) {
        return Rows::NewAddress {
            text: query.to_lowercase(),
            valid: recipients::is_valid(query),
        };
    }
    Rows::None
}

/// The rows for a `−` query, five at most: the groups (external and internal recipients, each
/// domain), then To and Cc, keeping each whose name, address or domain starts with the query.
/// Auto Bcc's addresses and Bcc aren't counted.
fn removal_rows(open: &Open, query: &str, team: &[String]) -> Rows {
    let others: Vec<&Address> = open
        .record
        .to
        .iter()
        .chain(&open.record.cc)
        .filter(|address| {
            recipients::is_valid(&address.email)
                && !open
                    .delivery
                    .auto_bcc
                    .emails
                    .contains(&address.email.to_lowercase())
        })
        .collect();
    let domain = |address: &Address| {
        address
            .email
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_lowercase())
            .unwrap_or_default()
    };
    let internal = |address: &Address| team.contains(&domain(address));
    let mut candidates: Vec<(Vec<String>, Person)> = Vec::new();
    if others.iter().any(|address| !internal(address)) {
        candidates.push((vec!["external".into()], Person::External));
    }
    if others.iter().any(|address| internal(address)) {
        candidates.push((vec!["internal".into()], Person::Internal));
    }
    let mut domains: Vec<String> = Vec::new();
    for address in &others {
        let domain = domain(address);
        if !domain.is_empty() && !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    candidates.extend(
        domains
            .into_iter()
            .map(|domain| (vec![domain.clone()], Person::Domain(domain))),
    );
    for address in &others {
        let mut keys = vec![address.email.to_lowercase(), domain(address)];
        if let Some(name) = address.name.as_deref().map(str::to_lowercase) {
            let words: Vec<&str> = name.split(' ').collect();
            keys.extend((0..words.len()).map(|from| words[from..].join(" ")));
        }
        candidates.push((keys, Person::Contact((*address).clone())));
    }
    let query = query.to_lowercase();
    let found: Vec<Person> = candidates
        .into_iter()
        .filter(|(keys, _)| keys.iter().any(|key| key.starts_with(&query)))
        .map(|(_, person)| person)
        .take(contacts::LIMIT)
        .collect();
    if found.is_empty() {
        Rows::None
    } else {
        Rows::Contacts(found)
    }
}

impl crate::Mail {
    /// Finds the dropdown rows for the focused draft's trigger when the trigger changes.
    pub(super) fn assistant_rows(&mut self) {
        let tone = self.account.settings().emoji_skin_tone;
        let popped = self.compose.popped.clone();
        let team = super::reminder::team_domains(
            self.account.settings(),
            &self.snapshot.aliases,
            self.account.email(),
        );
        let thread_people = self.compose.thread_people.as_ref().map(|(_, index)| index);
        let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        let Some(open) = self.compose.drafts.iter_mut().find(|open| open.id == id) else {
            return;
        };
        let Some(trigger) = open.popups.assistant.trigger.clone() else {
            return;
        };
        let asked = (trigger.kind, trigger.query.clone());
        if open.popups.assistant.query.as_ref() == Some(&asked) {
            return;
        }
        let rows = match trigger.kind {
            TriggerKind::Emoji => {
                emoji_rows(&trigger.query, popped.as_deref() == Some(id.as_str()), tone)
            }
            TriggerKind::Contact('-' | '\u{2212}') => removal_rows(open, &trigger.query, &team),
            TriggerKind::Contact(_) => contact_rows(
                open,
                &trigger.query,
                book,
                thread_people.filter(|_| open.thread.is_some()),
            ),
            TriggerKind::Snippet => Rows::None,
        };
        // A new query starts from the top.
        let assistant = &mut open.popups.assistant;
        let same = assistant
            .query
            .as_ref()
            .is_some_and(|(_, query)| *query == asked.1);
        assistant.index = if same {
            assistant.index.min(count(&rows).saturating_sub(1))
        } else {
            0
        };
        if !same {
            assistant.scroll = 0.0;
        }
        assistant.rows = rows;
        assistant.query = Some(asked);
    }

    /// Carries out a dropdown key.
    pub(super) fn assistant_key(&mut self, key: Key, now: f64) {
        let Some(open) = self.compose.focused_mut() else {
            return;
        };
        match key {
            Key::Move(delta) => {
                let assistant = &mut open.popups.assistant;
                let rows = count(&assistant.rows).max(1);
                assistant.index =
                    (assistant.index as isize + delta).rem_euclid(rows as isize) as usize;
                assistant.reveal = true;
            }
            Key::Dismiss => {
                if open.state.field == super::Field::Subject {
                    super::subject::dismiss(open);
                } else {
                    open.editor.dismiss_trigger();
                }
                open.popups.assistant.trigger = None;
                open.popups.assistant.rows = Rows::None;
                open.popups.assistant.partial = None;
            }
            Key::Pick(index) => {
                if let Some(index) = index {
                    open.popups.assistant.index = index;
                }
                self.assistant_insert(None, now);
            }
            Key::Close => {
                let exact = open
                    .popups
                    .assistant
                    .trigger
                    .as_ref()
                    .and_then(|trigger| emoji::get(&trigger.query));
                self.assistant_insert(exact, now);
            }
        }
    }

    /// The highlighted row (or `emoji`) replaces the trigger's text.
    fn assistant_insert(&mut self, emoji: Option<String>, now: f64) {
        let tone_unset = self.account.settings().emoji_skin_tone.is_none();
        let team = super::reminder::team_domains(
            self.account.settings(),
            &self.snapshot.aliases,
            self.account.email(),
        );
        let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
        let Some(id) = self.compose.focus.clone() else {
            return;
        };
        let Some(open) = self.compose.drafts.iter_mut().find(|open| open.id == id) else {
            return;
        };
        let Some(trigger) = open.popups.assistant.trigger.take() else {
            return;
        };
        let rows = std::mem::take(&mut open.popups.assistant.rows);
        open.popups.assistant.query = None;
        let index = open.popups.assistant.index;
        let mut set_tone = false;
        match (&rows, trigger.kind) {
            (Rows::Emoji(found), _) => {
                let Some(text) =
                    emoji.or_else(|| found.get(index).map(|found| found.emoji.clone()))
                else {
                    return;
                };
                // The emoji replaces `:` and the name. A closing `:` comes only from the key, so
                // it was never typed.
                if open.state.field == super::Field::Subject {
                    super::subject::replace(open, &trigger, &text);
                } else {
                    open.editor.replace(trigger.start, trigger.end, &text);
                }
                set_tone = tone_unset && emoji::takes_tone(&text);
            }
            (Rows::Contacts(people), TriggerKind::Contact(ch)) => {
                let Some(person) = people.get(index).cloned() else {
                    return;
                };
                let removing = matches!(ch, '-' | '\u{2212}');
                let mark = mention(open, &trigger, &person, removing, &team, book);
                open.editor.mark_undo(mark);
            }
            (Rows::NewAddress { text, .. }, TriggerKind::Contact(ch)) => {
                // A new address writes what's typed and puts that address in To.
                let shown = format!("{}{}", trigger_char(ch), trigger.query.trim());
                let before = recipients_of(open);
                open.editor.replace(trigger.start, trigger.end, &shown);
                move_to_to(open, &recipients::parse(text));
                let mark = open.popups.assistant.remember(before, recipients_of(open));
                open.editor.mark_undo(mark);
            }
            _ => return,
        }
        open.touch(now);
        if set_tone {
            // The first emoji that takes a tone shows the Emoji Settings hint and sets the tone
            // to `NONE` from then on.
            self.account
                .update_settings(|settings| settings.emoji_skin_tone = Some(EmojiSkinTone::None));
            self.commands.hint(
                Hint::new()
                    .text("Want to personalize your emoji? Hit")
                    .keys(["⌘", "K"])
                    .text("→ Emoji Settings!"),
            );
        }
    }
}

impl State {
    fn remember(&mut self, before: Recipients, after: Recipients) -> u64 {
        self.next_mark += 1;
        self.marks.insert(self.next_mark, (before, after));
        self.next_mark
    }
}

fn recipients_of(open: &Open) -> Recipients {
    [
        open.record.to.clone(),
        open.record.cc.clone(),
        open.record.bcc.clone(),
    ]
}

/// The trigger as a mention writes it, with `-` written as `−`.
fn trigger_char(ch: char) -> char {
    if ch == '-' { '\u{2212}' } else { ch }
}

/// Picks a contact row. The trigger and the person's first name replace what was typed, or their
/// full name, else address, when another recipient shares the first name. `+` or `@` puts them
/// in To, and `−` takes them (or the group) off the draft. Returns the change's undo mark.
fn mention(
    open: &mut Open,
    trigger: &Trigger,
    person: &Person,
    removing: bool,
    team: &[String],
    book: Option<&contacts::Index>,
) -> u64 {
    let before = recipients_of(open);
    // Backspacing into a mention swaps the recipient the mention was for.
    let replaced = open
        .popups
        .assistant
        .partial
        .take()
        .filter(|_| !removing)
        .and_then(|name| unambiguous(&open.record.to, &name, book));
    let name = match person {
        Person::Contact(address) => {
            let first = contacts::best_known_first_name(address, book).unwrap_or_default();
            let staying = |other: &&Address| {
                replaced
                    .as_ref()
                    .is_none_or(|replaced| !other.email.eq_ignore_ascii_case(&replaced.email))
            };
            let shared = open.recipients().filter(staying).any(|other| {
                !other.email.eq_ignore_ascii_case(&address.email)
                    && contacts::best_known_first_name(other, book).as_deref()
                        == Some(first.as_str())
            });
            if shared {
                address
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| address.email.clone())
            } else {
                first
            }
        }
        Person::External => "external".into(),
        Person::Internal => "internal".into(),
        Person::Domain(domain) => domain.clone(),
    };
    let TriggerKind::Contact(ch) = trigger.kind else {
        return 0;
    };
    open.editor.replace(
        trigger.start,
        trigger.end,
        &format!("{}{name}", trigger_char(ch)),
    );
    if removing {
        remove(open, person, team);
    } else if let Person::Contact(address) = person {
        // The swapped recipient leaves To, then this one joins it.
        if let Some(replaced) = replaced {
            open.record
                .to
                .retain(|other| !other.email.eq_ignore_ascii_case(&replaced.email));
        }
        move_to_to(open, address);
    }
    let after = recipients_of(open);
    open.popups.assistant.remember(before, after)
}

/// Takes `person` (or everyone in the group) off the draft. A group spares the Auto Bcc
/// addresses in Bcc. `team` is the team's domains.
pub(super) fn remove(open: &mut Open, person: &Person, team: &[String]) {
    let auto = open.delivery.auto_bcc.emails.clone();
    let gone = |address: &Address| -> bool {
        let domain = address
            .email
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_lowercase())
            .unwrap_or_default();
        match person {
            Person::Contact(person) => address.email.eq_ignore_ascii_case(&person.email),
            Person::External => {
                recipients::is_valid(&address.email)
                    && !auto.contains(&address.email.to_lowercase())
                    && !team.contains(&domain)
            }
            Person::Internal => recipients::is_valid(&address.email) && team.contains(&domain),
            Person::Domain(wanted) => domain == *wanted,
        }
    };
    open.record.to.retain(|address| !gone(address));
    open.record.cc.retain(|address| !gone(address));
    let group = !matches!(person, Person::Contact(_));
    open.record.bcc.retain(|address| {
        !gone(address) || (group && auto.contains(&address.email.to_lowercase()))
    });
}

/// Registers `Remove External Recipients`, `Remove Internal Recipients` and
/// `Remove Recipients from {domain}` for the recipients the draft has, leaving out Auto Bcc's
/// addresses.
pub(super) fn register_removals(
    commands: &mut Commands<commands::Action>,
    open: &Open,
    team: &[String],
) {
    const FORM: &[Region] = &[Region::ComposeForm, Region::RecipientList];
    let auto = &open.delivery.auto_bcc.emails;
    let counted: Vec<&Address> = open
        .record
        .to
        .iter()
        .chain(&open.record.cc)
        .chain(
            open.record
                .bcc
                .iter()
                .filter(|address| !auto.contains(&address.email.to_lowercase())),
        )
        .collect();
    let domain = |address: &Address| {
        address
            .email
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_owned())
            .unwrap_or_default()
    };
    let valid = |address: &&&Address| recipients::is_valid(&address.email);
    let internal = |address: &Address| team.contains(&domain(address).to_lowercase());
    let command = |id: String, title: String, person: Person| {
        Command::new(
            id,
            title,
            commands::Action::Compose(Action::RemoveRecipients(person)),
        )
        .default_score(94.0)
        .icon("data/change-recipients")
        .regions(FORM)
    };
    if counted
        .iter()
        .filter(valid)
        .any(|address| !internal(address))
    {
        commands.add(command(
            "REMOVE_EXTERNAL_RECIPIENTS".into(),
            "Remove External Recipients".into(),
            Person::External,
        ));
    }
    if counted
        .iter()
        .filter(valid)
        .any(|address| internal(address))
    {
        commands.add(command(
            "REMOVE_INTERNAL_RECIPIENTS".into(),
            "Remove Internal Recipients".into(),
            Person::Internal,
        ));
    }
    let mut domains: Vec<String> = Vec::new();
    for address in &counted {
        let domain = domain(address);
        if !domain.is_empty() && !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    for domain in domains {
        commands.add(command(
            format!("COMMANDS.REMOVE_DOMAIN_RECIPIENTS:{domain}"),
            format!("Remove Recipients from {domain}"),
            Person::Domain(domain.to_lowercase()),
        ));
    }
}

/// Moves `address` to To: out of Cc and Bcc, and at the end of To unless already there.
fn move_to_to(open: &mut Open, address: &Address) {
    let same = |other: &Address| {
        !address.email.is_empty() && other.email.eq_ignore_ascii_case(&address.email)
    };
    if open.record.to.iter().any(same) {
        return;
    }
    open.record.cc.retain(|other| !same(other));
    open.record.bcc.retain(|other| !same(other));
    open.record.to.push(address.clone());
}

fn count(rows: &Rows) -> usize {
    match rows {
        Rows::None => 0,
        Rows::Emoji(found) => found.len(),
        Rows::Contacts(people) => people.len(),
        Rows::NewAddress { .. } => 1,
    }
}

/// How far the dropdown stays from its layer's edges: at least 8px from the left and 22.5px from
/// the right. Moved left to fit, it comes no closer than 22.5px to the left edge.
const LAYER_LEFT: f32 = 8.0;
const LAYER_MARGIN: f32 = 22.5;

/// Draws the dropdown under the trigger, or above it when there's no room below, kept inside
/// the extent `within` of the layer it's in (the thread's messages). Returns the row pressed:
/// mouse-down picks, and hovering with the mouse moving highlights. Draws nothing while the
/// caret is on a date.
pub(super) fn show(ui: &Ui, open: &mut Open, within: egui::Rangef, id: Id) -> Option<usize> {
    let trigger = open.popups.assistant.trigger.clone()?;
    if !open.popups.assistant.shown() || open.body.date.is_some() {
        return None;
    }
    let ctx = ui.ctx().clone();
    let width = match &open.popups.assistant.rows {
        Rows::Emoji(_) => EMOJI_WIDTH,
        Rows::Contacts(people) => {
            let texts: Vec<(String, String)> = people.iter().map(row_text).collect();
            let rows: Vec<ContactRow<'_>> = texts
                .iter()
                .map(|(name, email)| ContactRow {
                    name,
                    email,
                    disabled: false,
                    avatar: None,
                })
                .collect();
            measure_contact_columns(&ctx, &rows, &CONTACT, CELL_PADDING, NAME_MIN)
                .iter()
                .sum()
        }
        Rows::NewAddress { text, .. } => {
            let rows = [ContactRow {
                name: "",
                email: text,
                disabled: false,
                avatar: None,
            }];
            measure_contact_columns(&ctx, &rows, &CONTACT, CELL_PADDING, NAME_MIN)
                .iter()
                .sum()
        }
        Rows::None => 0.0,
    };
    // The subject's dropdown hangs from the trigger's left.
    let in_subject = open.state.field == super::Field::Subject;
    let anchor = if in_subject {
        trigger.anchor.left()
    } else {
        trigger.anchor.left() - left_offset(trigger.kind)
    };
    let mut left = anchor.max(within.min + LAYER_LEFT);
    if left > within.max - LAYER_MARGIN - width {
        left = (within.max - LAYER_MARGIN - width).max(within.min + LAYER_MARGIN);
    }
    // 3px under the trigger's range. In the body that range is the text's content area, which
    // ends 4px above the bottom of its 24px line (`BodyText::COMPOSE`), as for the snippet
    // picker. In the subject the dropdown sits under the subject's line.
    let top = if in_subject {
        trigger.anchor.bottom()
    } else {
        trigger.anchor.bottom() - 1.0
    };
    let State {
        rows,
        index,
        scroll,
        reveal,
        ..
    } = &mut open.popups.assistant;
    match rows {
        Rows::Emoji(found) => emoji_picker(
            &ctx,
            found,
            index,
            (scroll, std::mem::take(reveal)),
            pos2(left, top),
            trigger.anchor.top(),
            id,
        ),
        Rows::Contacts(people) => {
            let shown: Vec<(String, String)> = people.iter().map(row_text).collect();
            let response = contact_list(
                &ctx,
                &shown,
                &trigger.query,
                Some(*index),
                pos2(left, top),
                id,
            );
            // The pointer moving onto a row highlights it.
            if let Some(hovered) = response.hovered {
                *index = hovered;
            }
            response.pressed
        }
        Rows::NewAddress { text, valid } => {
            let shown = vec![(String::new(), text.clone())];
            // Only the address cell, all of it darker when the address is valid.
            contact_list(
                &ctx,
                &shown,
                if *valid { text } else { "" },
                valid.then_some(0),
                pos2(left, top),
                id,
            )
            .pressed
        }
        Rows::None => None,
    }
}

/// A contact row's name and address cells.
fn row_text(person: &Person) -> (String, String) {
    match person {
        Person::Contact(address) => (
            address
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| address.email.clone()),
            address.email.clone(),
        ),
        Person::External => ("external".into(), "all external recipients".into()),
        Person::Internal => ("internal".into(), "all internal recipients".into()),
        Person::Domain(domain) => (domain.clone(), format!("all @{domain}")),
    }
}

/// The emoji picker with its top left at `at`, flipped above `above` when it doesn't fit.
fn emoji_picker(
    ctx: &egui::Context,
    found: &[emoji::Found],
    index: &mut usize,
    (scroll, reveal): (&mut f32, bool),
    at: egui::Pos2,
    above: f32,
    id: Id,
) -> Option<usize> {
    let palette = exo_ui::palette(ctx);
    let rows_height = found.len() as f32 * EMOJI_ROW;
    // In a window under 550px tall the rows scroll in 100px, without a scrollbar.
    let shown = if ctx.content_rect().height() < SHORTENED_BELOW {
        rows_height.min(SHORTENED_HEIGHT)
    } else {
        rows_height
    };
    let most = rows_height - shown;
    if reveal {
        // Scrolls the highlighted row to the top, as far as the rows can scroll.
        *scroll = *index as f32 * EMOJI_ROW;
    }
    *scroll = scroll.clamp(0.0, most);
    let height = 2.0 * EMOJI_PADDING + shown;
    let top = if at.y + height > ctx.content_rect().bottom() && above - height >= 0.0 {
        above - height
    } else {
        at.y
    };
    let card = Rect::from_min_size(pos2(at.x, top), vec2(EMOJI_WIDTH, height));
    let (fill, selected, ink) = match palette.theme {
        Theme::Snow => (
            Color32::WHITE,
            Color32::from_rgb(0xf4, 0xf6, 0xfb),
            Color32::from_black_alpha(217),
        ),
        Theme::Carbon => (
            Color32::from_rgb(0x27, 0x29, 0x2d),
            Color32::from_rgb(0x46, 0x4b, 0x53),
            Color32::WHITE,
        ),
    };
    let out = AnchoredList::new(id, card, AnchoredCard::tooltip(fill, selected))
        .order(egui::Order::Foreground)
        .uniform_padding(EMOJI_PADDING)
        .clip_rows()
        .highlighted(Some(*index))
        .scroll(scroll)
        .show_uniform(ctx, found.len(), EMOJI_ROW, |painter, rect, at| {
            let row = &found[at];
            PREVIEW.paint_line(
                painter,
                pos2(rect.left() + EMOJI_INSET + 1.0, rect.top() + 5.0),
                &row.emoji,
                ink,
                None,
            );
            let name_left = rect.left() + EMOJI_INSET + PREVIEW_WIDTH + NAME_GAP;
            NAME.paint_line(
                painter,
                pos2(name_left, rect.top() + 7.0),
                &row.name,
                ink,
                Some(rect.right() - EMOJI_INSET - name_left),
            );
        });
    if let Some(hovered) = out.hovered {
        *index = hovered;
    }
    out.pressed
}

/// The contact list with its top left at `at`, showing name and address cells with what's typed
/// darker in each.
fn contact_list(
    ctx: &egui::Context,
    rows: &[(String, String)],
    query: &str,
    highlight: Option<usize>,
    at: egui::Pos2,
    id: Id,
) -> exo_ui::ContactListResponse {
    let shown: Vec<ContactRow<'_>> = rows
        .iter()
        .map(|(name, email)| ContactRow {
            name,
            email,
            disabled: false,
            avatar: None,
        })
        .collect();
    let (list, width) = ContactSelectorList::auto_columns(
        ctx,
        &shown,
        query,
        highlight,
        CONTACT,
        CONTACT_ROW,
        CONTACT_PADDING,
        CELL_PADDING,
        NAME_MIN,
    );
    list.show(ctx, id, at, width, egui::Order::Foreground)
}

#[cfg(test)]
mod tests {
    use exo_store::{DraftAction, DraftRecord};

    use super::*;

    fn person(name: &str, email: &str) -> Address {
        Address {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    fn draft(to: &[Address], cc: &[Address]) -> Open {
        let record = DraftRecord {
            action: DraftAction::Compose,
            from: Some(person("Me", "me@acme.com")),
            to: to.to_vec(),
            cc: cc.to_vec(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
            quoted_content: None,
            quoted_inlined: false,
            quote_signed: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "x@y".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        Open::new("d".into(), None, record)
    }

    fn typed(open: &mut Open, text: &str) -> Trigger {
        open.editor.type_text(text);
        open.editor.trigger().cloned().expect("a trigger")
    }

    fn emails(list: &[Address]) -> Vec<&str> {
        list.iter().map(|address| address.email.as_str()).collect()
    }

    #[test]
    fn a_plus_mention_writes_the_first_name_and_moves_them_to_to() {
        let priya = person("Priya Raman", "priya@x.com");
        let mut open = draft(&[], std::slice::from_ref(&priya));
        let trigger = typed(&mut open, "Thanks +pri");
        mention(
            &mut open,
            &trigger,
            &Person::Contact(priya.clone()),
            false,
            &[],
            None,
        );
        assert_eq!(open.editor.document().blocks[0].text(), "Thanks +Priya");
        assert_eq!(
            (emails(&open.record.to), emails(&open.record.cc)),
            (vec!["priya@x.com"], vec![])
        );
        // With another Priya on the draft, the mention writes the full name.
        let other = person("Priya Shah", "shah@y.com");
        let mut open = draft(std::slice::from_ref(&other), &[]);
        let trigger = typed(&mut open, "+pri");
        mention(
            &mut open,
            &trigger,
            &Person::Contact(priya),
            false,
            &[],
            None,
        );
        assert_eq!(open.editor.document().blocks[0].text(), "+Priya Raman");
    }

    #[test]
    fn backspacing_into_a_mention_swaps_the_recipient_it_was_for() {
        let (priya, pam) = (
            person("Priya Raman", "priya@x.com"),
            person("Pam Lee", "pam@x.com"),
        );
        let mut open = draft(std::slice::from_ref(&priya), &[]);
        let trigger = typed(&mut open, "+pri");
        mention(
            &mut open,
            &trigger,
            &Person::Contact(priya.clone()),
            false,
            &[],
            None,
        );
        open.editor.type_text(" ");
        open.editor.backspace();
        // At the end of `+Priya`, who's in To, Backspace opens her contacts again.
        edit_mention(&mut open, None);
        open.editor.backspace();
        let trigger = open
            .editor
            .trigger()
            .cloned()
            .expect("the mention's contacts");
        assert_eq!(
            (trigger.kind, trigger.query.as_str()),
            (TriggerKind::Contact('+'), "Priy")
        );
        mention(&mut open, &trigger, &Person::Contact(pam), false, &[], None);
        assert_eq!(open.editor.document().blocks[0].text(), "+Pam");
        assert_eq!(
            emails(&open.record.to),
            ["pam@x.com"],
            "Pam takes Priya's place"
        );
        // Someone not in To isn't swapped, so their dismissed mention stays closed.
        let mut open = draft(&[], &[]);
        open.editor.type_text("+Zed");
        open.editor.dismiss_trigger();
        edit_mention(&mut open, None);
        open.editor.backspace();
        assert_eq!(open.editor.trigger(), None);
    }

    #[test]
    fn a_minus_mention_takes_them_off_and_undo_puts_them_back() {
        let (ann, bob, cy) = (
            person("Ann", "ann@acme.com"),
            person("Bob", "bob@client.com"),
            person("Cy", "cy@client.com"),
        );
        let mut open = draft(&[ann.clone(), bob.clone()], std::slice::from_ref(&cy));
        let team = ["acme.com".to_owned()];
        assert_eq!(
            removal_rows(&open, "ext", &team),
            Rows::Contacts(vec![Person::External])
        );
        // The domain first, then whoever's at it.
        assert_eq!(
            removal_rows(&open, "cl", &team),
            Rows::Contacts(vec![
                Person::Domain("client.com".into()),
                Person::Contact(bob.clone()),
                Person::Contact(cy.clone())
            ])
        );
        assert_eq!(
            removal_rows(&open, "b", &team),
            Rows::Contacts(vec![Person::Contact(bob.clone())])
        );
        let trigger = typed(&mut open, "-cl");
        let mark = mention(
            &mut open,
            &trigger,
            &Person::Domain("client.com".into()),
            true,
            &team,
            None,
        );
        assert_eq!(
            open.editor.document().blocks[0].text(),
            "\u{2212}client.com"
        );
        assert_eq!(
            (emails(&open.record.to), emails(&open.record.cc)),
            (vec!["ann@acme.com"], vec![])
        );
        undo_marks(&mut open, &[mark], &[]);
        assert_eq!(emails(&open.record.to), ["ann@acme.com", "bob@client.com"]);
        undo_marks(&mut open, &[], &[mark]);
        assert_eq!(emails(&open.record.to), ["ann@acme.com"]);
    }

    #[test]
    fn emoji_need_two_characters_but_for_a_few_letters() {
        assert_eq!(emoji_rows("s", false, None), Rows::None);
        assert!(
            matches!(emoji_rows("o", false, None), Rows::Emoji(found) if found[0].name == "ok_hand")
        );
        assert!(matches!(emoji_rows("smi", true, None), Rows::Emoji(found) if found.len() == 5));
    }

    #[test]
    fn an_address_being_typed_is_its_own_row() {
        let open = draft(&[], &[]);
        assert_eq!(
            contact_rows(&open, "zed@new.co", None, None),
            Rows::NewAddress {
                text: "zed@new.co".into(),
                valid: true
            }
        );
        assert_eq!(
            contact_rows(&open, "zed", None, None),
            Rows::NewAddress {
                text: "zed".into(),
                valid: false
            }
        );
    }
}
