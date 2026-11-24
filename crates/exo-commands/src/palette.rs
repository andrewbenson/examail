//! The ⌘K palette: the main palette and every sub-palette (Label, Move, …), which are the same
//! widget with other rows.
//!
//! A 680px panel whose top sits 199px above the content area's middle, 16px top padding, a 36px
//! header with a hairline, a 32px input row with 24px margins and a 3px purple bar, then rows 60px
//! tall overlapping by 8px in a list at most 244px tall.

use std::{borrow::Cow, sync::Arc};

use egui::{
    Area, Color32, Context, CornerRadius, Event, Galley, Key, Modifiers, Order, Painter, Pos2,
    Rect, Sense, Ui, Vec2,
    os::OperatingSystem,
    text::{CCursor, CCursorRange, LayoutJob, TextWrapping},
};
use exo_ui::{
    BoxShadow, Family, Font, KeycapColors, Keycaps, TextStyle, focus_field, layout as layout_job,
    paint_icon, paint_text, palette, text_field,
};

use crate::{
    Act, Hint,
    command::Command,
    dropdown::{Dropdown, DropdownState},
    keys::Chord,
    rank::{Ranked, rank_main, rank_rows},
};

const WIDTH: f32 = 680.0;
const PADDING_TOP: f32 = 16.0;
const GUTTER: f32 = 36.0;
const HEADER_HEIGHT: f32 = 36.0;
const INPUT_MARGIN: f32 = 24.0;
const INPUT_HEIGHT: f32 = 32.0;
const LIST_MAX_HEIGHT: f32 = 244.0;
const ROW_PADDING: f32 = 20.0;
/// A row's icon and the header's: a 20px box with the command's white 18px glyph centred in it.
const ICON: f32 = 20.0;
const GLYPH: f32 = 18.0;
/// An account row's avatar: 24px with `margin: 0 10px 0 -2px`, its 12px bold initial in `#27292d`
/// with `dy: .375em`.
const AVATAR: f32 = 24.0;
const AVATAR_INITIAL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
const AVATAR_INK: Color32 = Color32::from_rgb(0x27, 0x29, 0x2d);
/// Only this many rows render until the user moves through or over the list.
const FIRST_ROWS: usize = 10;

const HEADER: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 20.0);
/// `-0.0225em` at 21px.
const INPUT: TextStyle =
    TextStyle::new(Font::new(Family::Mono, 400), 21.0, 28.0).letter_spacing(-0.4725);
/// `-0.0225em` at 16px.
const TITLE_LETTER_SPACING: f32 = -0.36;
const TITLE: TextStyle =
    TextStyle::new(Font::new(Family::Mono, 400), 16.0, 20.0).letter_spacing(TITLE_LETTER_SPACING);
const SIDE: TextStyle =
    TextStyle::new(Font::new(Family::Mono, 400), 14.0, 20.0).letter_spacing(-0.315);

/// The palette's text colour, which the header text takes at 70%.
const HEADER_COLOR: Color32 = Color32::from_rgb(0xfa, 0xfa, 0xfa);
/// The bar left of the input: `inset 3px 0 0 #aeb1dd`.
const INPUT_BAR: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
/// The input's selection colour.
const SELECTION: Color32 = Color32::from_rgba_premultiplied(114, 192, 234, 254);

/// The id the main palette saves its query under.
const MAIN: &str = "COMMAND_PALETTE";

/// A sub-palette: the palette with its own type (`LABEL`, `MOVE`), prompt and rows. Rows are
/// commands, usually with a title and action only. The palette filters and ranks them with
/// `command-score`.
#[must_use]
pub struct SubPalette<A> {
    id: &'static str,
    prompt: Cow<'static, str>,
    icon: &'static str,
    placeholder: Cow<'static, str>,
    /// What the input starts with, the caret at its end.
    initial: Option<String>,
    hide_input: bool,
    rows: Rows<A>,
    footer: Option<Footer<A>>,
    /// Inert text under the divider when a typed query lists nothing.
    empty: Option<&'static str>,
    /// Inert text under the divider when there are no rows and nothing is typed.
    blank: Option<&'static str>,
    dropdown: Option<Dropdown>,
    /// A later stage of the sub-palette whose row opens it ([`SubPalette::stage`]).
    stage: bool,
}

type Footer<A> = Box<dyn Fn(&str) -> Option<Command<A>>>;
type RowsFor<A> = Box<dyn Fn(&str, usize) -> Vec<Command<A>>>;

/// A sub-palette's rows.
enum Rows<A> {
    /// Fixed rows the palette ranks for the query.
    Ranked(Vec<Command<A>>),
    /// Rows the sub-palette matches and orders itself, for the query as typed and the
    /// dropdown's choice.
    Ordered(RowsFor<A>),
}

impl<A: 'static> SubPalette<A> {
    /// A sub-palette of type `id` (`LABEL`) with header `prompt` and `icon`. Reopening the same
    /// type restores an unfinished query.
    pub fn new(
        id: &'static str,
        prompt: impl Into<Cow<'static, str>>,
        icon: &'static str,
        rows: Vec<Command<A>>,
    ) -> Self {
        Self {
            id,
            prompt: prompt.into(),
            icon,
            placeholder: Cow::Borrowed(""),
            initial: None,
            hide_input: false,
            rows: Rows::Ranked(rows),
            footer: None,
            empty: None,
            blank: None,
            dropdown: None,
            stage: false,
        }
    }

    /// A sub-palette for pickers with their own ranking rules (Label, Move). `rows` matches and
    /// orders the rows for each query, and they're shown as it returns them.
    pub fn ordered(
        id: &'static str,
        prompt: impl Into<Cow<'static, str>>,
        icon: &'static str,
        rows: impl Fn(&str) -> Vec<Command<A>> + 'static,
    ) -> Self {
        Self {
            rows: Rows::Ordered(Box::new(move |query, _| rows(query))),
            ..Self::new(id, prompt, icon, Vec::new())
        }
    }

    /// [`ordered`](Self::ordered), with `dropdown` beside the input. `rows` also takes the
    /// dropdown's choice, by index.
    pub fn ordered_with(
        id: &'static str,
        prompt: impl Into<Cow<'static, str>>,
        icon: &'static str,
        dropdown: Dropdown,
        rows: impl Fn(&str, usize) -> Vec<Command<A>> + 'static,
    ) -> Self {
        Self {
            rows: Rows::Ordered(Box::new(rows)),
            dropdown: Some(dropdown),
            ..Self::new(id, prompt, icon, Vec::new())
        }
    }

    /// Grey text in the empty input (`Try: 8 am, 3 days, aug 7`).
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'static, str>>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Starts with `query` typed, the caret at its end.
    pub fn initial(mut self, query: impl Into<String>) -> Self {
        self.initial = Some(query.into());
        self
    }

    /// A later stage of the sub-palette whose row opens it, drawn as the same palette moving on
    /// (New Split Inbox's `Edit Subject`). Escape goes back to the main palette if the earlier
    /// stage's Escape did.
    pub fn stage(mut self) -> Self {
        self.stage = true;
        self
    }

    /// No input, so the rows follow the header (Move to Important / Other).
    pub fn hide_input(mut self) -> Self {
        self.hide_input = true;
        self
    }

    /// A row under a divider that depends on the query (`Create label “{query}”`).
    pub fn footer(mut self, footer: impl Fn(&str) -> Option<Command<A>> + 'static) -> Self {
        self.footer = Some(Box::new(footer));
        self
    }

    /// Inert text under the divider when a typed query lists nothing (`No matching date found`).
    pub fn empty_note(mut self, note: &'static str) -> Self {
        self.empty = Some(note);
        self
    }

    /// Inert text under the divider when there are no rows and nothing is typed
    /// (`No links or attachments`).
    pub fn blank_note(mut self, note: &'static str) -> Self {
        self.blank = Some(note);
        self
    }

    /// The titles of the rows listed for `query`, in order, then the footer's (for tests).
    pub fn titles(&self, query: &str) -> Vec<String> {
        let title = |row: &Command<A>| row.title.as_deref().unwrap_or_default().to_owned();
        let choice = self.dropdown.as_ref().map_or(0, Dropdown::selected);
        let mut titles: Vec<String> = match &self.rows {
            Rows::Ranked(rows) => rank_rows(rows, query)
                .into_iter()
                .map(|row| row.title.into_owned())
                .collect(),
            Rows::Ordered(rows) => rows(query, choice).iter().map(title).collect(),
        };
        titles.extend(
            self.footer
                .as_ref()
                .and_then(|footer| footer(query))
                .as_ref()
                .map(title),
        );
        if titles.is_empty() {
            titles.extend(self.note(query).map(str::to_owned));
        }
        titles
    }

    /// The rows listed for `query` with the dropdown on `choice`, as title, side title and
    /// action (for tests).
    pub fn rows(&self, query: &str, choice: usize) -> Vec<(String, Option<String>, A)>
    where
        A: Clone,
    {
        let row = |row: &Command<A>| {
            (
                row.title.as_deref().unwrap_or_default().to_owned(),
                row.side_title.as_deref().map(str::to_owned),
                row.action.clone(),
            )
        };
        match &self.rows {
            Rows::Ranked(rows) => rank_rows(rows, query)
                .iter()
                .map(|ranked| row(ranked.command))
                .collect(),
            Rows::Ordered(rows) => rows(query, choice).iter().map(row).collect(),
        }
    }

    pub(crate) fn into_act(self) -> SubPalette<Act<A>> {
        let footer = self.footer.map(|footer| -> Footer<Act<A>> {
            Box::new(move |query| footer(query).map(|row| row.map(Act::App)))
        });
        SubPalette {
            id: self.id,
            prompt: self.prompt,
            icon: self.icon,
            placeholder: self.placeholder,
            initial: self.initial,
            hide_input: self.hide_input,
            empty: self.empty,
            blank: self.blank,
            dropdown: self.dropdown,
            stage: self.stage,
            rows: match self.rows {
                Rows::Ranked(rows) => {
                    Rows::Ranked(rows.into_iter().map(|row| row.map(Act::App)).collect())
                }
                Rows::Ordered(rows) => Rows::Ordered(Box::new(move |query, choice| {
                    rows(query, choice)
                        .into_iter()
                        .map(|row| row.map(Act::App))
                        .collect()
                })),
            },
            footer,
        }
    }
}

impl<A> SubPalette<A> {
    /// The inert note for no rows listed for `query`.
    fn note(&self, query: &str) -> Option<&'static str> {
        if query.trim().is_empty() {
            self.blank
        } else {
            self.empty
        }
    }
}

/// The palette's state between frames.
pub(crate) struct Palette<A> {
    open: Option<Open<A>>,
    /// The query a palette type was dismissed with, restored (selected) when it reopens.
    saved: Option<(&'static str, String)>,
    /// A row ran this frame, so a sub-palette opened in response goes back here on Escape.
    ran_main: bool,
    /// A row of a sub-palette that goes back to the main palette ran this frame.
    ran_from_main: bool,
    /// A hint the dropdown asked for this frame, for the app's hint tooltip.
    hint: Option<Hint>,
    /// The main palette's nudge, its placeholder while nothing is typed.
    nudge: Option<Cow<'static, str>>,
}

struct Open<A> {
    sub: Option<SubPalette<A>>,
    /// Escape from this sub-palette returns to the main palette.
    from_main: bool,
    state: State,
}

/// What the user has done to an open palette.
struct State {
    query: String,
    highlight: usize,
    show_all: bool,
    scroll: f32,
    /// The highlight moved by keyboard, so scroll it into view.
    reveal: bool,
    /// Focus the input (and select or place the caret) on the next frame.
    focus: Option<Selection>,
    /// The dropdown's choice, menu highlight, and flash state.
    dropdown: DropdownState,
    /// The main palette's nudge this frame ([`Palette::set_nudge`]).
    nudge: Option<Cow<'static, str>>,
}

#[derive(Clone, Copy)]
enum Selection {
    All,
    End,
}

/// A row that ran, and how.
pub(crate) struct Ran<A> {
    pub(crate) action: A,
    /// The modifiers held as it ran (Enter's, or the click's).
    pub(crate) modifiers: Modifiers,
    /// It ran with ⌘ held and asked to leave the palette open then.
    stays_open: bool,
}

impl<A: Clone> Ran<A> {
    fn new(command: &Command<A>, modifiers: Modifiers) -> Self {
        Self {
            action: command.action.clone(),
            modifiers,
            stays_open: command.stays_open_on_command && modifiers.command,
        }
    }
}

/// What ends a frame of an open palette.
enum Outcome<A> {
    /// A row ran.
    Run(Ran<A>),
    /// Dismissed.
    Close,
    /// Escape in a sub-palette opened from the main palette.
    BackToMain,
}

impl<A: Clone + 'static> Palette<A> {
    /// The open sub-palette's prompt and titles for the query typed.
    pub(crate) fn sub_rows(&self) -> Option<(String, Vec<String>)> {
        let open = self.open.as_ref()?;
        let sub = open.sub.as_ref()?;
        Some((sub.prompt.to_string(), sub.titles(&open.state.query)))
    }
}

impl<A: Clone> Palette<A> {
    pub(crate) const fn new() -> Self {
        Self {
            open: None,
            saved: None,
            ran_main: false,
            ran_from_main: false,
            hint: None,
            nudge: None,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The hint the dropdown asked for, once.
    pub(crate) fn take_hint(&mut self) -> Option<Hint> {
        self.hint.take()
    }

    /// The open palette's type: `COMMAND_PALETTE` for the main one, else the sub-palette's id.
    pub(crate) fn open_id(&self) -> Option<&'static str> {
        self.open
            .as_ref()
            .map(|open| open.sub.as_ref().map_or(MAIN, |sub| sub.id))
    }

    /// Shows `nudge` in the main palette's empty input, in the nudge's gradient.
    pub(crate) fn set_nudge(&mut self, nudge: Option<Cow<'static, str>>) {
        self.nudge = nudge;
    }

    pub(crate) fn is_main_open(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.sub.is_none())
    }

    /// Whether the sub-palette of type `id` is open.
    pub(crate) fn is_sub_open(&self, id: &str) -> bool {
        self.open
            .as_ref()
            .and_then(|open| open.sub.as_ref())
            .is_some_and(|sub| sub.id == id)
    }

    /// Opens the main palette, with `initial` typed (caret at the end) or else the query it was
    /// last dismissed with (selected).
    pub(crate) fn open_main(&mut self, initial: Option<&str>) {
        self.open = Some(self.opened(MAIN, None, initial, false));
    }

    pub(crate) fn open_sub(&mut self, sub: SubPalette<A>) {
        let from_main = self.ran_main || self.is_main_open() || (sub.stage && self.ran_from_main);
        let choice = sub.dropdown.as_ref().map_or(0, Dropdown::selected);
        let initial = sub.initial.clone();
        let mut open = self.opened(sub.id, Some(sub), initial.as_deref(), from_main);
        open.state.dropdown.choice = choice;
        self.open = Some(open);
    }

    fn opened(
        &self,
        id: &'static str,
        sub: Option<SubPalette<A>>,
        initial: Option<&str>,
        from_main: bool,
    ) -> Open<A> {
        let restored = self
            .saved
            .as_ref()
            .filter(|(saved, _)| *saved == id)
            .map(|(_, query)| query.clone());
        let (query, focus) = match (initial, restored) {
            (Some(initial), _) => (initial.to_owned(), Selection::End),
            (None, Some(restored)) => (restored, Selection::All),
            (None, None) => (String::new(), Selection::End),
        };
        Open {
            sub,
            from_main,
            state: State {
                query,
                highlight: 0,
                show_all: false,
                scroll: 0.0,
                reveal: false,
                focus: Some(focus),
                dropdown: DropdownState::default(),
                nudge: None,
            },
        }
    }

    /// Closes the palette. `save` keeps a non-empty query for the next time this type opens.
    /// Dismissing by ⌘K or a click outside saves it, and running a row doesn't.
    pub(crate) fn close(&mut self, save: bool) {
        let Some(open) = self.open.take() else {
            return;
        };
        if save && !open.state.query.is_empty() {
            self.saved = Some((
                open.sub.as_ref().map_or(MAIN, |sub| sub.id),
                open.state.query,
            ));
        }
    }

    /// Handles the palette's keys and draws it over `area` (the window below the tab bar), and
    /// returns the action of the row that ran. `main` is the main palette's candidates, in
    /// registration order.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        area: Rect,
        main: &[&Command<A>],
    ) -> Option<Ran<A>> {
        (self.ran_main, self.ran_from_main) = (false, false);
        let open = self.open.as_mut()?;
        let (sub, state) = (open.sub.as_ref(), &mut open.state);
        state.nudge = if sub.is_none() {
            self.nudge.clone()
        } else {
            None
        };
        // The dropdown's keys go first, because they change the rows.
        let dropdown = sub.and_then(|sub| sub.dropdown.as_ref());
        if let Some(dropdown) = dropdown {
            let before = state.dropdown.choice;
            dropdown.keys(ctx, &mut state.dropdown);
            if state.dropdown.choice != before {
                state.highlight = 0;
                state.scroll = 0.0;
            }
        }
        let ordered = ordered_rows(sub, &state.query, state.dropdown.choice);
        let rows = build_rows(sub, main, &state.query, &ordered);
        let footer = build_footer(sub, &state.query, rows.len());
        let count = item_count(rows.len(), footer.as_ref());
        state.highlight = state.highlight.min(count.saturating_sub(1));

        let opens_menu = dropdown.is_some() && !rows.is_empty();
        let mut cleared = false;
        let outcome = match keyboard(ctx, state, count, opens_menu) {
            Some((Key::Escape, _)) if !state.query.is_empty() => {
                state.query.clear();
                state.highlight = 0;
                state.scroll = 0.0;
                state.reveal = false;
                cleared = true;
                None
            }
            Some((Key::Escape, _)) if sub.is_some() && open.from_main => Some(Outcome::BackToMain),
            Some((Key::Escape, _)) => Some(Outcome::Close),
            Some((Key::Enter, modifiers)) => run(&rows, footer.as_ref(), state.highlight)
                .map(|command| Outcome::Run(Ran::new(command, modifiers))),
            _ => None,
        };
        let outcome = match outcome {
            Some(outcome) => outcome,
            None => {
                let choice_before = state.dropdown.choice;
                let outcome = if cleared {
                    let ordered = ordered_rows(sub, &state.query, state.dropdown.choice);
                    let rows = build_rows(sub, main, &state.query, &ordered);
                    let footer = build_footer(sub, &state.query, rows.len());
                    let count = item_count(rows.len(), footer.as_ref());
                    state.highlight = state.highlight.min(count.saturating_sub(1));
                    draw(ctx, area, sub, state, &rows, footer.as_ref())
                } else {
                    draw(ctx, area, sub, state, &rows, footer.as_ref())
                };
                if state.dropdown.choice != choice_before {
                    state.highlight = 0;
                    state.scroll = 0.0;
                }
                if let (Some(picked), Some((option, hint))) = (
                    state.dropdown.picked.take(),
                    dropdown.and_then(Dropdown::hint),
                ) {
                    self.hint = (picked == *option).then(|| hint.clone());
                }
                outcome?
            }
        };
        match outcome {
            Outcome::Run(ran) => {
                self.ran_main = self.is_main_open();
                self.ran_from_main = self
                    .open
                    .as_ref()
                    .is_some_and(|open| open.sub.is_some() && open.from_main);
                if !ran.stays_open {
                    // Running anything forgets the saved query.
                    self.saved = None;
                    self.close(false);
                }
                Some(ran)
            }
            Outcome::Close => {
                self.close(true);
                None
            }
            Outcome::BackToMain => {
                self.open = Some(self.opened(MAIN, None, None, false));
                None
            }
        }
    }
}

/// A row under the list, after a 1px divider.
enum FooterRow<A> {
    /// A sub-palette's footer command.
    Command(Box<Command<A>>),
    /// Inert text.
    Text(&'static str),
    /// `Hint: hit` `control` `J` `to move down!`.
    Hint,
}

impl<A> FooterRow<A> {
    fn is_actionable(&self) -> bool {
        matches!(self, Self::Command(_))
    }
}

/// The main palette's footer: the `ctrl+j` hint when the query ends in `j` and there are rows to
/// move through, else a suggestion when nothing matched.
fn main_footer<A>(query: &str, rows: usize) -> Option<FooterRow<A>> {
    if query.to_lowercase().ends_with('j') && rows > 1 {
        Some(FooterRow::Hint)
    } else if rows == 0 && !query.is_empty() {
        Some(FooterRow::Text("Try: “search”, “remind me”, “sent”"))
    } else {
        None
    }
}

fn ordered_rows<A>(sub: Option<&SubPalette<A>>, query: &str, choice: usize) -> Vec<Command<A>> {
    match sub.map(|sub| &sub.rows) {
        Some(Rows::Ordered(rows)) => rows(query, choice),
        _ => Vec::new(),
    }
}

fn build_rows<'a, A>(
    sub: Option<&'a SubPalette<A>>,
    main: &[&'a Command<A>],
    query: &str,
    ordered: &'a [Command<A>],
) -> Vec<Ranked<'a, A>> {
    match sub.map(|sub| &sub.rows) {
        Some(Rows::Ranked(rows)) => rank_rows(rows, query),
        Some(Rows::Ordered(_)) => ordered.iter().map(Ranked::unranked).collect(),
        None => rank_main(main, query),
    }
}

fn build_footer<A>(sub: Option<&SubPalette<A>>, query: &str, rows: usize) -> Option<FooterRow<A>> {
    match sub {
        Some(sub) => sub
            .footer
            .as_ref()
            .and_then(|footer| footer(query))
            .map(|row| FooterRow::Command(Box::new(row)))
            .or_else(|| sub.note(query).filter(|_| rows == 0).map(FooterRow::Text)),
        None => main_footer(query, rows),
    }
}

fn item_count<A>(rows: usize, footer: Option<&FooterRow<A>>) -> usize {
    rows + usize::from(footer.is_some_and(FooterRow::is_actionable))
}

/// The highlighted row (the footer's, at `rows.len()`).
fn run<'a, A>(
    rows: &[Ranked<'a, A>],
    footer: Option<&'a FooterRow<A>>,
    highlight: usize,
) -> Option<&'a Command<A>> {
    match (rows.get(highlight), footer) {
        (Some(row), _) => Some(row.command),
        (None, Some(FooterRow::Command(command))) if highlight == rows.len() => Some(command),
        _ => None,
    }
}

/// The input's id.
const INPUT_ID: &str = "exo_commands::palette_input";

/// Whether the input's caret is at the end of `query`, with nothing selected.
fn caret_at_end(ctx: &Context, query: &str) -> bool {
    let end = query.chars().count();
    egui::text_edit::TextEditState::load(ctx, exo_ui::id(ctx, INPUT_ID))
        .and_then(|state| state.cursor.char_range())
        .is_none_or(|range| range.primary.index.0 == end && range.secondary.index.0 == end)
}

/// Consumes the palette's keys before the input sees them, moving the highlight. Returns Escape or
/// Enter, with the modifiers held, for the caller to act on. With a dropdown that has rows to apply
/// to (`opens_menu`), Tab and → at the input's end open its menu instead.
fn keyboard(
    ctx: &Context,
    state: &mut State,
    count: usize,
    opens_menu: bool,
) -> Option<(Key, Modifiers)> {
    let at_end = opens_menu && caret_at_end(ctx, &state.query);
    let os = ctx.os();
    let mac = os == OperatingSystem::Mac;
    let mut result = None;
    ctx.input_mut(|input| {
        let mut swallow_backtick = false;
        input.events.retain(|event| {
            if swallow_backtick && matches!(event, Event::Text(text) if text == "`") {
                swallow_backtick = false;
                return false;
            }
            let Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            let chord = Chord::from_event(*key, *modifiers);
            let ctralt = if mac {
                chord.ctrl && !chord.alt
            } else {
                chord.alt && !chord.ctrl
            };
            let plain = !chord.shift && !chord.ctrl && !chord.alt && !chord.meta;
            let command = if mac { chord.meta } else { chord.ctrl }
                && !chord.shift
                && !chord.alt
                && !(mac && chord.ctrl);
            let step = |state: &mut State, to: usize| {
                state.highlight = to;
                state.show_all = true;
                state.reveal = true;
            };
            match chord.key {
                Key::ArrowDown | Key::ArrowUp if command && count > 0 => {
                    step(
                        state,
                        if chord.key == Key::ArrowDown {
                            count - 1
                        } else {
                            0
                        },
                    );
                }
                Key::ArrowDown if plain && count > 0 => step(state, (state.highlight + 1) % count),
                Key::ArrowUp if plain && count > 0 => {
                    step(state, (state.highlight + count - 1) % count)
                }
                Key::J | Key::N if ctralt && !chord.shift && !chord.meta && count > 0 => {
                    step(state, (state.highlight + 1) % count)
                }
                Key::K | Key::P if ctralt && !chord.shift && !chord.meta && count > 0 => {
                    step(state, (state.highlight + count - 1) % count)
                }
                Key::Tab | Key::ArrowRight
                    if opens_menu && (chord.key == Key::Tab || (plain && at_end)) =>
                {
                    state.dropdown.menu = Some(state.dropdown.choice)
                }
                Key::Enter | Key::Tab if !*repeat => result = Some((Key::Enter, *modifiers)),
                Key::Enter | Key::Tab => {}
                _ if chord.is_escape(os) => {
                    swallow_backtick = chord.key == Key::Backtick;
                    result = Some((Key::Escape, *modifiers));
                }
                _ => return true,
            }
            false
        });
    });
    result
}

/// Draws the palette and handles the mouse.
fn draw<A: Clone>(
    ctx: &Context,
    area: Rect,
    sub: Option<&SubPalette<A>>,
    state: &mut State,
    rows: &[Ranked<'_, A>],
    footer: Option<&FooterRow<A>>,
) -> Option<Outcome<A>> {
    let id = exo_ui::id(ctx, "exo_commands::palette");
    Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(area.min)
        .show(ctx, |ui| {
            // The palette is a transparent layer over the whole content area. A click on it closes
            // the palette, and nothing underneath gets the click.
            let backdrop = ui.allocate_rect(area, Sense::click());
            let layout = Layout::new(
                ui,
                area,
                state,
                rows,
                footer,
                sub.is_some_and(|sub| sub.hide_input),
            );
            let panel = ui.allocate_rect(layout.panel, Sense::click());
            let ran = paint(ui, &layout, sub, state, rows, footer);
            if panel.clicked() || panel.drag_stopped() {
                state.focus.get_or_insert(Selection::End);
            }
            ran.map(Outcome::Run)
                .or_else(|| backdrop.clicked().then_some(Outcome::Close))
        })
        .inner
}

/// Cached text shaping for one rendered palette row so measurement and painting shape once.
struct RowLayout {
    top: f32,
    height: f32,
    meta_width: f32,
    title: Arc<Galley>,
    description: Option<(f32, Arc<Galley>)>,
    side: Option<Arc<Galley>>,
    keycaps: Option<Keycaps>,
}

impl RowLayout {
    fn new<A>(ctx: &Context, row: &Ranked<'_, A>, top: f32) -> Self {
        let command = row.command;
        let tick = if command.in_use { 14.0 } else { 0.0 };
        let (side, keycaps, meta_width) = if let Some(side) = &command.side_title {
            let galley = layout_job(
                ctx,
                LayoutJob::single_section(side.to_string(), SIDE.format(Color32::WHITE)),
            );
            let width = tick + 32.0 + galley.size().x;
            (Some(galley), None, width)
        } else {
            let keys = command.display_keys();
            if keys.is_empty() {
                (None, None, tick)
            } else {
                let caps = Keycaps::new(ctx, &keys, KeycapColors::base(palette(ctx)));
                let width = tick + caps.width() + if command.in_use { 4.0 } else { 0.0 };
                (None, Some(caps), width)
            }
        };

        let icon = if command.icon.is_some() {
            ICON + 13.0
        } else if command.avatar.is_some() {
            AVATAR - 2.0 + 10.0
        } else {
            0.0
        };
        let title_budget = WIDTH - 2.0 * GUTTER - icon - (meta_width + 12.0);
        let title = shape_title_galley(ctx, row, Color32::WHITE, title_budget);
        let description = command.description.as_ref().and_then(|desc| {
            let desc_offset = title.size().x + TITLE_LETTER_SPACING + DESCRIPTION_GAP;
            let desc_width = title_budget - desc_offset;
            (desc_width > 0.0).then(|| {
                let mut job =
                    LayoutJob::single_section(desc.to_string(), SIDE.format(Color32::WHITE));
                job.wrap = TextWrapping::truncate_at_width(desc_width);
                (desc_offset, layout_job(ctx, job))
            })
        });

        let avatar = command.avatar.as_ref().map_or(0.0, |avatar| {
            if avatar.photo.is_some() {
                AVATAR - 2.0
            } else {
                AVATAR
            }
        });
        let lines = title.rows.len().clamp(1, 2) as f32;
        let content = (lines * TITLE.line_height).max(avatar);
        let height = 2.0 * ROW_PADDING + content;

        Self {
            top,
            height,
            meta_width,
            title,
            description,
            side,
            keycaps,
        }
    }
}

/// Where everything goes this frame.
struct Layout {
    panel: Rect,
    list: Rect,
    /// Each rendered row's geometry and pre-shaped galleys.
    rows: Vec<RowLayout>,
    content_height: f32,
    footer: Option<Rect>,
    footer_row: Option<RowLayout>,
    /// How far the panel moved to land on a whole pixel. Row icons (SVGs in the list's
    /// composited layer) stay where layout put them, so they don't move with it.
    snap: f32,
}

impl Layout {
    fn new<A>(
        ui: &Ui,
        area: Rect,
        state: &State,
        rows: &[Ranked<'_, A>],
        footer: Option<&FooterRow<A>>,
        hide_input: bool,
    ) -> Self {
        let ctx = ui.ctx();
        let left = area.center().x - WIDTH / 2.0;
        // `top: calc(50% - 199px)`. Chromium paints the panel on a whole CSS pixel (measured: 221.5
        // in an 800px window draws at 222), but not the row icons (see `snap`).
        let unsnapped = area.center().y - 199.0;
        let top = unsnapped.round();
        // Without its input the empty input row is 8px under the header (measured).
        let input = if hide_input {
            8.0
        } else {
            INPUT_MARGIN + INPUT_HEIGHT + INPUT_MARGIN
        };
        let list_top = top + PADDING_TOP + HEADER_HEIGHT + input;
        let shown = if state.show_all {
            rows.len()
        } else {
            rows.len().min(FIRST_ROWS)
        };
        let mut y = 0.0;
        let row_layouts = rows[..shown]
            .iter()
            .map(|row| {
                let laid = RowLayout::new(ctx, row, y);
                y += laid.height - 8.0;
                laid
            })
            .collect::<Vec<_>>();
        // The last row has `margin-bottom: 8px` instead of −8px.
        let content_height = if shown == 0 { 0.0 } else { y + 16.0 };
        let list_height = content_height.min(LIST_MAX_HEIGHT);
        let list = Rect::from_min_size(Pos2::new(left, list_top), Vec2::new(WIDTH, list_height));
        // The footer: a 1px break, then a row padded `19px 36px 20px` with the last child's 8px
        // bottom margin.
        let footer_rect = footer.map(|_| {
            Rect::from_min_size(Pos2::new(left, list.bottom()), Vec2::new(WIDTH, 1.0 + 59.0))
        });
        let footer_row = match footer {
            Some(FooterRow::Command(command)) => {
                Some(RowLayout::new(ctx, &Ranked::unranked(command), 0.0))
            }
            _ => None,
        };
        let bottom = footer_rect.map_or(list.bottom(), |footer| footer.bottom() + 8.0);
        let panel = Rect::from_min_max(
            Pos2::new(left, top),
            Pos2::new(left + WIDTH, bottom.min(area.bottom())),
        );
        Self {
            panel,
            list,
            rows: row_layouts,
            content_height,
            footer: footer_rect,
            footer_row,
            snap: top - unsnapped,
        }
    }
}

/// The gap between a title and its description.
const DESCRIPTION_GAP: f32 = 28.0;

fn shape_title_galley<A>(
    ctx: &Context,
    row: &Ranked<'_, A>,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(row.title.clone().into_owned(), TITLE.format(color));
    // Beside a description the title keeps to one line (`white-space: nowrap`) and gives way
    // only once the description is gone (`flex: 0 1 auto` against its `1 99999999999 auto`).
    // Chromium wraps a title at its spaces only, so one without (a URL) is cut short on its
    // first line, where egui would break it after a `/`.
    let described = row.command.description.is_some();
    let one_line = described || !row.title.contains(char::is_whitespace);
    job.wrap = TextWrapping {
        max_width: if described {
            (width - DESCRIPTION_GAP).max(0.0)
        } else {
            width
        },
        max_rows: if one_line { 1 } else { 2 },
        break_anywhere: one_line,
        overflow_character: Some('…'),
    };
    layout_job(ctx, job)
}

fn tint_galley(galley: &Arc<Galley>, color: Color32) -> Arc<Galley> {
    let mut out = galley.clone();
    let made = Arc::make_mut(&mut out);
    let mut job = (*made.job).clone();
    for section in &mut job.sections {
        section.format.color = color;
    }
    made.job = Arc::new(job);
    for placed in &mut made.rows {
        let row = Arc::make_mut(&mut placed.row);
        for glyph in &row.glyphs {
            let first = glyph.first_vertex as usize;
            if glyph.uv_rect.is_nothing()
                || glyph.uv_rect.size.x <= 0.0
                || !row.visuals.glyph_vertex_range.contains(&first)
            {
                continue;
            }
            for vertex in &mut row.visuals.mesh.vertices[first..first + 4] {
                vertex.color = color;
            }
        }
    }
    out
}

/// Paints the palette and handles the mouse over it, and returns the clicked row.
fn paint<A: Clone>(
    ui: &mut Ui,
    layout: &Layout,
    sub: Option<&SubPalette<A>>,
    state: &mut State,
    rows: &[Ranked<'_, A>],
    footer: Option<&FooterRow<A>>,
) -> Option<Ran<A>> {
    let ctx = ui.ctx().clone();
    let tokens = palette(&ctx);
    let panel = layout.panel;
    // 4px corners. Carbon's shadows are deeper.
    let radius = CornerRadius::same(4);
    let painter = ui.painter().clone();
    let shadows = match tokens.theme {
        exo_ui::Theme::Snow => [
            BoxShadow::new([0.0, 15.0], 25.0, 0.0, Color32::from_black_alpha(26)),
            BoxShadow::new([0.0, 19.0], 45.0, 0.0, Color32::from_black_alpha(77)),
        ],
        exo_ui::Theme::Carbon => [
            BoxShadow::new([0.0, 15.0], 50.0, 0.0, Color32::from_black_alpha(179)),
            BoxShadow::new([0.0, 19.0], 90.0, 0.0, Color32::BLACK),
        ],
    };
    for shadow in shadows {
        shadow.paint(&painter, panel, radius);
    }
    painter.rect_filled(panel, radius, tokens.background_dark_default);
    let painter = painter.with_clip_rect(panel);

    // Header: icon, prompt, hairline.
    let left = panel.left() + GUTTER;
    let header_top = panel.top() + PADDING_TOP;
    let (icon, prompt) = sub.map_or(("data/icon", "Exo Command"), |sub| {
        (sub.icon, sub.prompt.as_ref())
    });
    // The header's glyph: centred in its box, 1px down, at 70%.
    let glyph = Rect::from_center_size(
        Pos2::new(left + ICON / 2.0, header_top + 1.0 + ICON / 2.0),
        Vec2::splat(GLYPH),
    );
    paint_icon(ui, icon, glyph, Color32::WHITE.gamma_multiply(0.7));
    let prompt = HEADER.single_line(&painter, prompt, HEADER_COLOR.gamma_multiply(0.7), None);
    paint_text(
        &painter,
        Pos2::new(left + 33.0, header_top + 3.0),
        prompt,
        &HEADER,
    );
    let hairline = Rect::from_min_size(
        Pos2::new(left, header_top + HEADER_HEIGHT - 1.0),
        Vec2::new(WIDTH - 2.0 * GUTTER, 1.0),
    );
    painter.rect_filled(hairline, 0.0, Color32::WHITE.gamma_multiply(0.1));

    let mut menu = None;
    if !sub.is_some_and(|sub| sub.hide_input) {
        let input_top = header_top + HEADER_HEIGHT + INPUT_MARGIN;
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(panel.left(), input_top),
                Vec2::new(3.0, INPUT_HEIGHT),
            ),
            0.0,
            INPUT_BAR,
        );
        let nudge = state.nudge.clone();
        let placeholder = nudge.as_deref().map_or_else(
            || sub.map_or("", |sub| sub.placeholder.as_ref()),
            |nudge| nudge,
        );
        // The dropdown takes its width from the right, `margin-right: 36px`, and the input the
        // rest.
        let dropdown = sub.and_then(|sub| sub.dropdown.as_ref()).map(|dropdown| {
            let right = panel.right() - GUTTER;
            let width = dropdown.width(ui, &state.dropdown);
            menu = Some((
                dropdown,
                Rect::from_min_size(
                    Pos2::new(right - width, input_top),
                    Vec2::new(width, INPUT_HEIGHT),
                ),
            ));
            width
        });
        let width = WIDTH - 2.0 * GUTTER - dropdown.map_or(0.0, |width| width + GUTTER);
        input(
            ui,
            &painter,
            state,
            Pos2::new(left, input_top + 4.0),
            placeholder,
            nudge.is_some(),
            width,
        );
        if let Some((dropdown, rect)) = menu {
            dropdown.paint_button(ui, &painter, &mut state.dropdown, rect, !rows.is_empty());
        }
    }

    // Rows.
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
    let over_list = ui
        .input(|input| input.pointer.hover_pos())
        .is_some_and(|pos| layout.list.contains(pos));
    let wheel = if over_list {
        ui.input(|input| input.smooth_scroll_delta.y)
    } else {
        0.0
    };
    if over_list && (pointer_moved || wheel != 0.0) {
        state.show_all = true;
    }
    state.scroll =
        (state.scroll - wheel).clamp(0.0, (layout.content_height - layout.list.height()).max(0.0));
    if state.reveal {
        state.reveal = false;
        if let Some(laid) = layout.rows.get(state.highlight) {
            // Scrolled in at the nearest edge.
            if laid.top < state.scroll {
                state.scroll = laid.top;
            } else if laid.top + laid.height > state.scroll + layout.list.height() {
                state.scroll = laid.top + laid.height - layout.list.height();
            }
        }
    }

    let mut ran = None;
    let modifiers = ui.input(|input| input.modifiers);
    let mut row_rects = Vec::with_capacity(layout.rows.len());
    for (index, (laid, row)) in layout.rows.iter().zip(rows).enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(panel.left(), layout.list.top() + laid.top - state.scroll),
            Vec2::new(WIDTH, laid.height),
        );
        let response = ui.interact(
            rect.intersect(layout.list),
            exo_ui::id(ui.ctx(), ("exo_commands::palette_row", index)),
            Sense::click(),
        );
        if (response.hovered() && pointer_moved) || response.is_pointer_button_down_on() {
            state.highlight = index;
        }
        if response.clicked() {
            ran = Some(Ran::new(row.command, modifiers));
        }
        row_rects.push((index, rect, laid, row));
    }

    if let (Some(rect), Some(FooterRow::Command(command))) = (layout.footer, footer) {
        let row_rect = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 1.0), rect.max);
        let response = ui.interact(
            row_rect,
            exo_ui::id(ui.ctx(), "exo_commands::palette_footer"),
            Sense::click(),
        );
        if (response.hovered() && pointer_moved) || response.is_pointer_button_down_on() {
            state.highlight = rows.len();
        }
        if response.clicked() {
            ran = Some(Ran::new(command, modifiers));
        }
    }

    let mut list_ui = ui.new_child(egui::UiBuilder::new());
    list_ui.set_clip_rect(layout.list);
    for (index, rect, laid, row) in row_rects {
        let active = index == state.highlight;
        if active {
            list_ui
                .painter()
                .rect_filled(rect, 0.0, Color32::WHITE.gamma_multiply(0.1));
        }
        paint_row(&list_ui, rect, laid, row, active, layout.snap);
    }

    if let (Some(rect), Some(footer)) = (layout.footer, footer) {
        let divider = Rect::from_min_size(
            Pos2::new(rect.left() + GUTTER, rect.top()),
            Vec2::new(WIDTH - 2.0 * GUTTER, 1.0),
        );
        painter.rect_filled(divider, 0.0, Color32::WHITE.gamma_multiply(0.1));
        let active = state.highlight == rows.len() && footer.is_actionable();
        let row_rect = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 1.0), rect.max);
        if active {
            painter.rect_filled(row_rect, 0.0, Color32::WHITE.gamma_multiply(0.1));
        }
        let color = Color32::WHITE.gamma_multiply(if active { 0.9 } else { 0.5 });
        // Footer rows are padded `19px 36px 20px`.
        let line_top = Pos2::new(rect.left() + GUTTER, row_rect.top() + 19.0 + 1.0);
        match footer {
            FooterRow::Command(command) => {
                if let Some(footer_laid) = &layout.footer_row {
                    // Footer rows are padded 19px on top, rows 20px.
                    let shifted = row_rect.translate(Vec2::Y * -1.0);
                    paint_row(
                        ui,
                        shifted,
                        footer_laid,
                        &Ranked::unranked(command),
                        active,
                        layout.snap,
                    );
                }
            }
            FooterRow::Text(text) => paint_text(
                &painter,
                line_top,
                TITLE.single_line(&painter, text, color, None),
                &TITLE,
            ),
            FooterRow::Hint => paint_hint(ui, &painter, line_top, color),
        }
    }
    // The open menu is over the rows.
    if let (Some((dropdown, rect)), Some(highlight)) = (menu, state.dropdown.menu) {
        dropdown.paint_menu(ui, &mut state.dropdown, highlight, rect);
    }
    ran
}

/// The nudge's gradient. The input's background, `linear-gradient(90deg, #8ea6ff 0%, #ffbcd5 25%,
/// #8ea6ff 75%, #ffbcd5 100%)`, is four times its `width` wide and slides from its end to its start
/// every 5s. It shows through `galley`'s glyphs (`background-clip: text`), under the placeholder's
/// 30% white.
fn gradient_text(galley: &Arc<Galley>, width: f32, time: f64) -> Arc<Galley> {
    const STOPS: [(f32, [f32; 3]); 4] = [(0.0, BLUE), (0.25, PINK), (0.75, BLUE), (1.0, PINK)];
    const BLUE: [f32; 3] = [142.0, 165.903_85, 255.0];
    const PINK: [f32; 3] = [255.0, 188.0, 212.512_2];
    // `background-position` goes from 100% to 0%, so the image starts `3 × width` left of the
    // input.
    let position = 1.0 - (time % 5.0 / 5.0) as f32;
    let color = |x: f32| {
        let at = ((x + 3.0 * width * position) / (4.0 * width)).clamp(0.0, 1.0);
        let (from, to) = STOPS
            .windows(2)
            .map(|pair| (pair[0], pair[1]))
            .find(|(_, to)| at <= to.0)
            .unwrap_or((STOPS[2], STOPS[3]));
        let t = (at - from.0) / (to.0 - from.0);
        let channel = |i: usize| (from.1[i] + (to.1[i] - from.1[i]) * t) * 0.7 + 255.0 * 0.3;
        Color32::from_rgb(
            channel(0).round() as u8,
            channel(1).round() as u8,
            channel(2).round() as u8,
        )
    };
    let mut tinted = (**galley).clone();
    for placed in &mut tinted.rows {
        let left = placed.pos.x;
        for vertex in &mut Arc::make_mut(&mut placed.row).visuals.mesh.vertices {
            vertex.color = color(left + vertex.pos.x);
        }
    }
    Arc::new(tinted)
}

/// The input: Exo Mono 21px/28px, its line box 4px below the input row's top.
fn input(
    ui: &mut Ui,
    painter: &Painter,
    state: &mut State,
    line_top_left: Pos2,
    placeholder: &str,
    gradient: bool,
    width: f32,
) {
    let ctx = ui.ctx().clone();
    let id = exo_ui::id(&ctx, INPUT_ID);
    if state.query.is_empty() && !placeholder.is_empty() {
        let galley = INPUT.single_line(
            painter,
            placeholder,
            Color32::WHITE.gamma_multiply(0.3),
            None,
        );
        let galley = if gradient {
            ctx.request_repaint();
            gradient_text(&galley, width, ctx.input(|input| input.time))
        } else {
            galley
        };
        paint_text(painter, line_top_left, galley, &INPUT);
    }
    let line = Rect::from_min_size(line_top_left, Vec2::new(width, INPUT.line_height));
    let before = state.query.clone();
    text_field(
        ui,
        id,
        &mut state.query,
        line,
        &INPUT,
        palette(&ctx).text_light_default,
        SELECTION,
    );
    if state.query != before {
        // Every query change puts the highlight back on the first row.
        state.highlight = 0;
        state.scroll = 0.0;
    }
    if let Some(selection) = state.focus.take() {
        let end = CCursor::new(state.query.chars().count());
        let range = match selection {
            Selection::All => CCursorRange::two(CCursor::new(0), end),
            Selection::End => CCursorRange::one(end),
        };
        focus_field(&ctx, id, range);
    }
}

/// Paints one pre-laid-out row with `ui`'s painter (clipped to the list).
fn paint_row<A>(
    ui: &Ui,
    rect: Rect,
    laid: &RowLayout,
    row: &Ranked<'_, A>,
    active: bool,
    snap: f32,
) {
    let painter = ui.painter();
    let tokens = palette(ui.ctx());
    let command = row.command;
    let color = Color32::WHITE.gamma_multiply(if active { 0.9 } else { 0.5 });
    let mut x = rect.left() + GUTTER;
    let content_top = rect.top() + ROW_PADDING;
    if let Some(icon) = command.icon {
        // A white glyph at 70%, whole on the active row.
        let icon_color = Color32::WHITE.gamma_multiply(if active { 1.0 } else { 0.7 });
        // `top: 2px`, from where layout put the row (see `Layout::snap`). The glyph sits at the
        // box's top left (`background-repeat: no-repeat`).
        let icon_top = content_top + 2.0 - snap;
        paint_icon(
            ui,
            icon,
            Rect::from_min_size(Pos2::new(x, icon_top), Vec2::splat(GLYPH)),
            icon_color,
        );
        x += ICON + 13.0;
    } else if let Some(avatar) = &command.avatar {
        let rect = Rect::from_min_size(Pos2::new(x - 2.0, content_top), Vec2::splat(AVATAR));
        match &avatar.photo {
            Some(photo) => exo_ui::paint_photo_avatar(
                painter,
                rect,
                photo,
                2.0,
                avatar.fill,
                if active { 1.0 } else { 0.7 },
            ),
            None => exo_ui::paint_initial_avatar(
                painter,
                rect,
                avatar.fill,
                &avatar.initial,
                &AVATAR_INITIAL,
                AVATAR_INK,
                0.375 * 12.0,
            ),
        }
        x += AVATAR - 2.0 + 10.0;
    }
    if let Some((desc_offset, desc_galley)) = &laid.description {
        let ink = Color32::WHITE.gamma_multiply(if active { 0.7 } else { 0.35 });
        paint_text(
            painter,
            Pos2::new(x + desc_offset, content_top + 1.0),
            tint_galley(desc_galley, ink),
            &SIDE,
        );
    }
    paint_text(
        painter,
        Pos2::new(x, content_top + 1.0),
        tint_galley(&laid.title, color),
        &TITLE,
    );

    // Right side, right-aligned at the gutter.
    let right = rect.right() - GUTTER;
    let mut meta_left = right - laid.meta_width;
    if command.in_use {
        let tick = Pos2::new(meta_left.floor(), content_top + 6.0);
        let points =
            [[1.0, 5.142], [4.858, 9.0], [12.445, 1.413]].map(|[x, y]| tick + Vec2::new(x, y));
        painter.add(egui::Shape::line(
            points.to_vec(),
            egui::Stroke::new(2.0, Color32::WHITE),
        ));
        meta_left += 14.0;
    }
    if let Some(side_galley) = &laid.side {
        paint_text(
            painter,
            Pos2::new(meta_left + 32.0, content_top + 1.0),
            tint_galley(side_galley, color),
            &SIDE,
        );
        return;
    }
    if let Some(base_caps) = &laid.keycaps {
        let keycaps = base_caps.with_colors(KeycapColors {
            text: color,
            divider: color,
            ..KeycapColors::base(tokens)
        });
        let (gap, drop) = if command.in_use {
            (4.0, 1.0)
        } else {
            (0.0, 0.0)
        };
        keycaps.paint(painter, meta_left + gap, content_top + 2.0 + drop);
    }
}

/// `Hint: hit` `control` `J` `to move down!`, with inline keycaps.
fn paint_hint(ui: &Ui, painter: &Painter, line_top_left: Pos2, color: Color32) {
    let ctralt = if ui.ctx().os() == OperatingSystem::Mac {
        "control"
    } else {
        "alt"
    };
    let mut x = line_top_left.x;
    let space = TITLE.single_line(painter, " ", color, None).size().x;
    for (i, piece) in ["Hint: hit", ctralt, "J", "to move down!"]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            x += space;
        }
        let at = Pos2::new(x, line_top_left.y);
        x += if i == 1 || i == 2 {
            paint_key_hint(painter, at, piece)
        } else {
            TITLE.paint_line(painter, at, piece, color, None)
        };
    }
}

/// An inline keycap in the hint, `hsla(0,0%,100%,.15)`, Exo Sans 14px/24px bold.
fn paint_key_hint(painter: &Painter, line_top_left: Pos2, label: &str) -> f32 {
    const LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 24.0);
    let color = Color32::WHITE.gamma_multiply(0.9);
    let label_width = LABEL.single_line(painter, label, color, None).size().x;
    let width = (label_width + 10.0).max(19.0);
    // `margin-left: -2px; top: 3px`, 18px tall.
    let rect = Rect::from_min_size(line_top_left + Vec2::new(-2.0, 3.0), Vec2::new(width, 18.0));
    // `#555b64` in Carbon.
    painter.rect_filled(
        rect,
        CornerRadius::same(3),
        palette(painter.ctx()).background_key_default,
    );
    // The label `top: -3px` in a 24px line box under the cap's 1px padding.
    LABEL.paint_line(
        painter,
        Pos2::new(rect.center().x - label_width / 2.0, rect.top() + 1.0 - 3.0),
        label,
        color,
        None,
    );
    width - 2.0
}

#[cfg(test)]
mod tests {
    use egui::{RawInput, pos2, vec2};

    use super::*;

    #[test]
    fn hovering_a_sub_palette_footer_command_highlights_it_so_enter_runs_it() {
        let ctx = Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));
        let mut palette = Palette::<&'static str>::new();
        let sub = SubPalette::new(
            "LABEL",
            "Add label",
            "data/label",
            vec![
                Command::new("ONE", "Alpha", "alpha"),
                Command::new("TWO", "Beta", "beta"),
            ],
        )
        .footer(|_| Some(Command::new("CREATE", "Create label", "create")));
        palette.open_sub(sub);

        // Initial pass + sizing settle pass: highlight starts on row 0 (`Alpha`).
        for pos in [pos2(10.0, 10.0), pos2(12.0, 10.0)] {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(area),
                    events: vec![Event::PointerMoved(pos)],
                    ..Default::default()
                },
                |ui| {
                    let _ = palette.show(ui.ctx(), area, &[]);
                },
            )
            .drop_without_applying_deltas();
        }
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(0)
        );

        // Panel top = 800/2 - 199 = 201; list top = 201 + 16 + 36 + 80 = 333;
        // 2 rows (60 + 52 + 8 = 120) => footer starts at y = 453 (`454..513`).
        ctx.run_ui(
            RawInput {
                screen_rect: Some(area),
                events: vec![Event::PointerMoved(pos2(640.0, 480.0))],
                ..Default::default()
            },
            |ui| {
                let _ = palette.show(ui.ctx(), area, &[]);
            },
        )
        .drop_without_applying_deltas();
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(2),
            "moving over FooterRow::Command updates highlight to rows.len()"
        );

        let mut ran = None;
        ctx.run_ui(
            RawInput {
                screen_rect: Some(area),
                events: vec![Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                ran = palette.show(ui.ctx(), area, &[]).map(|r| r.action);
            },
        )
        .drop_without_applying_deltas();
        assert_eq!(ran, Some("create"));
    }

    fn key_press(key: Key, modifiers: Modifiers) -> Vec<Event> {
        vec![
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }

    fn step_palette<A: Clone>(
        ctx: &Context,
        area: Rect,
        palette: &mut Palette<A>,
        main: &[&Command<A>],
        events: Vec<Event>,
    ) -> (Option<A>, Vec<String>) {
        let mut ran = None;
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(area),
                events,
                ..Default::default()
            },
            |ui| {
                ran = palette.show(ui.ctx(), area, main).map(|r| r.action);
            },
        );
        let texts = painted_texts(&output.shapes);
        output.drop_without_applying_deltas();
        (ran, texts)
    }

    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(children) => {
                    for child in children {
                        collect(child, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn run_only_selects_footer_at_exact_rows_len() {
        let first = Command::new("ONE", "One", "one");
        let second = Command::new("TWO", "Two", "two");
        let rows = [Ranked::unranked(&first), Ranked::unranked(&second)];
        let footer = FooterRow::Command(Box::new(Command::new("FOOTER", "Footer", "footer")));

        assert_eq!(run(&rows, Some(&footer), 0).map(|c| c.action), Some("one"));
        assert_eq!(run(&rows, Some(&footer), 1).map(|c| c.action), Some("two"));
        assert_eq!(
            run(&rows, Some(&footer), 2).map(|c| c.action),
            Some("footer"),
            "exact rows.len() selects the footer command"
        );
        assert!(
            run(&rows, Some(&footer), 3).is_none(),
            "out-of-bounds highlight past rows.len() must not run the footer"
        );
        assert!(run(&rows, Some(&footer), 99).is_none());

        let empty: [Ranked<'_, &str>; 0] = [];
        assert_eq!(
            run(&empty, Some(&footer), 0).map(|c| c.action),
            Some("footer")
        );
        assert!(run(&empty, Some(&footer), 1).is_none());
    }

    #[test]
    fn dropdown_switch_shrinking_rows_clamps_highlight_and_does_not_run_footer() {
        let ctx = Context::default();
        ctx.set_os(OperatingSystem::Mac);
        exo_ui::install(&ctx).expect("the system fonts load");
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));
        let cmd_shift = Modifiers::MAC_CMD
            .plus(Modifiers::COMMAND)
            .plus(Modifiers::SHIFT);

        let make_sub = || {
            let dropdown = Dropdown::new(vec!["many", "few"], 0, "mod+shift+l");
            SubPalette::ordered_with(
                "DROPDOWN_TEST",
                "Pick option",
                "data/snooze",
                dropdown,
                |_, choice| {
                    if choice == 0 {
                        vec![
                            Command::new("ONE", "One", "one"),
                            Command::new("TWO", "Two", "two"),
                            Command::new("THREE", "Three", "three"),
                            Command::new("FOUR", "Four", "four"),
                        ]
                    } else {
                        vec![Command::new("ONLY", "Only", "only")]
                    }
                },
            )
            .footer(|_| Some(Command::new("FOOTER", "Footer action", "footer")))
        };

        let mut palette = Palette::<&'static str>::new();
        palette.open_sub(make_sub());
        step_palette(&ctx, area, &mut palette, &[], Vec::new());

        // Move highlight to row 3 (`Four`), past the shrunk choice's row + footer count (2).
        for _ in 0..3 {
            step_palette(
                &ctx,
                area,
                &mut palette,
                &[],
                key_press(Key::ArrowDown, Modifiers::NONE),
            );
        }
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(3)
        );

        // Cycle dropdown to `"few"` (1 row + 1 footer = count 2): highlight resets/clamps inside
        // valid rows and does not accidentally select the footer.
        step_palette(&ctx, area, &mut palette, &[], key_press(Key::L, cmd_shift));
        let open = palette.open.as_ref().expect("palette remains open");
        assert_eq!(open.state.dropdown.choice, 1);
        assert_eq!(open.state.highlight, 0);

        let (ran, _) = step_palette(
            &ctx,
            area,
            &mut palette,
            &[],
            key_press(Key::Enter, Modifiers::NONE),
        );
        assert_eq!(
            ran,
            Some("only"),
            "Enter after shrinking rows must run the highlighted row, not the footer"
        );

        // Explicitly highlighting the footer after switching still runs the footer on Enter.
        palette.open_sub(make_sub());
        step_palette(&ctx, area, &mut palette, &[], Vec::new());
        step_palette(&ctx, area, &mut palette, &[], key_press(Key::L, cmd_shift));
        step_palette(
            &ctx,
            area,
            &mut palette,
            &[],
            key_press(Key::ArrowDown, Modifiers::NONE),
        );
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(1),
            "ArrowDown explicitly moves highlight onto the footer at rows.len()"
        );
        let (ran, _) = step_palette(
            &ctx,
            area,
            &mut palette,
            &[],
            key_press(Key::Enter, Modifiers::NONE),
        );
        assert_eq!(ran, Some("footer"));

        // Shrinking candidate rows without a dropdown change also clamps highlight to count - 1.
        let commands = [
            Command::new("A", "Alpha", "alpha").default_score(4.0),
            Command::new("B", "Beta", "beta").default_score(3.0),
            Command::new("C", "Gamma", "gamma").default_score(2.0),
            Command::new("D", "Delta", "delta").default_score(1.0),
        ];
        let four: Vec<_> = commands.iter().collect();
        let two: Vec<_> = commands[..2].iter().collect();
        palette.open_main(None);
        step_palette(&ctx, area, &mut palette, &four, Vec::new());
        for _ in 0..3 {
            step_palette(
                &ctx,
                area,
                &mut palette,
                &four,
                key_press(Key::ArrowDown, Modifiers::NONE),
            );
        }
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(3)
        );
        step_palette(&ctx, area, &mut palette, &two, Vec::new());
        assert_eq!(
            palette.open.as_ref().map(|open| open.state.highlight),
            Some(1),
            "highlight clamps to count.saturating_sub(1) when rows shrink"
        );
    }

    #[test]
    fn escape_clearing_query_resets_scroll_and_immediately_renders_updated_rows() {
        let ctx = Context::default();
        ctx.set_os(OperatingSystem::Mac);
        exo_ui::install(&ctx).expect("the system fonts load");
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));

        let mut rows = vec![Command::new("ALPHA", "Alpha", "alpha")];
        for i in 1..=8 {
            rows.push(Command::new(
                format!("ITEM_{i}"),
                format!("Item {i}"),
                "item",
            ));
        }

        let mut palette = Palette::<&'static str>::new();
        palette
            .open_sub(SubPalette::new("ESCAPE_TEST", "Choose", "data/label", rows).initial("item"));
        step_palette(&ctx, area, &mut palette, &[], Vec::new());

        // Move down past the 244px list viewport so `state.scroll > 0.0` while filtered to "item".
        let mut texts = Vec::new();
        for _ in 0..6 {
            (_, texts) = step_palette(
                &ctx,
                area,
                &mut palette,
                &[],
                key_press(Key::ArrowDown, Modifiers::NONE),
            );
        }
        let open = palette.open.as_ref().expect("palette open");
        assert_eq!(open.state.query, "item");
        assert_eq!(open.state.highlight, 6);
        assert!(
            open.state.scroll > 0.0,
            "list scrolled down while moving through filtered rows"
        );
        assert!(
            !texts.iter().any(|text| text == "Alpha"),
            "pre-Escape frame only renders rows matching the query"
        );

        // Single Escape frame clears query, resets scroll, and paints the full unfiltered list
        // immediately on that same frame.
        let (ran, texts) = step_palette(
            &ctx,
            area,
            &mut palette,
            &[],
            key_press(Key::Escape, Modifiers::NONE),
        );
        assert!(ran.is_none());
        let open = palette.open.as_ref().expect("Escape only cleared query");
        assert!(open.state.query.is_empty());
        assert_eq!(open.state.highlight, 0);
        assert_eq!(open.state.scroll, 0.0);
        assert!(
            texts.iter().any(|text| text == "Alpha"),
            "Escape frame immediately renders recomputed unfiltered rows, got {texts:?}"
        );
    }
}
