//! The event form: the title over Who, When, (when creating) Repeat, the meeting link, Where,
//! About and (when creating) the calendar, with Save and Cancel below. The week view shows it as
//! a 464px modal beside the event; the right pane shows it over a day of the calendar.
//!
//! The form holds what it edits as [`Values`]. Editing an event compares them with the event's
//! own to know what changed.

mod guests;
mod overlays;
mod rows;
#[cfg(test)]
mod tests;
mod values;

pub use values::{
    CalendarOption, Edited, Field, FormAction, Guest, Meeting, Mode, Shown, TipClick, Values,
};

use egui::{Color32, Pos2, Rect, Stroke, Ui, Vec2, pos2, vec2};
use exo_ui::{Family, Font, KeyTooltip, TextStyle};
use jiff::{ToSpan, Zoned, civil::Weekday};

use self::{
    overlays::Dropdown,
    rows::{ABOUT_TEXT, InputSpec, RowContext, about_html, input},
};
use super::{
    repeat::{Custom, CustomAction},
    when::{self, When},
    when_edit::{self, WhenEdit},
};

/// The week view's modal: 464px wide.
pub const MODAL_WIDTH: f32 = 464.0;
/// The header: `padding: 16px 20px 12px` (32px on top in the right pane), the colour square and a
/// 12px gap before the title.
const PADDING_X: f32 = 20.0;
const HEADER_TOP_MODAL: f32 = 16.0;
const HEADER_TOP_PANE: f32 = 32.0;
const HEADER_BOTTOM: f32 = 12.0;
const SQUARE: f32 = 10.0;
const TITLE_LEFT: f32 = PADDING_X + 3.0 + SQUARE + 3.0 + 12.0;
/// The title: 16px/20px nudged `top: 2px`, 2px of padding over a 2px underline, in a 24px box.
const TITLE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 16.0, 20.0);
const TITLE_HEIGHT: f32 = 24.0;
/// The fields: `padding-top: 4px`, rows 20px tall 12px apart, 20px under the last in the modal (the
/// right pane's day follows the last directly).
const BODY_TOP: f32 = 4.0;
pub(super) const ROW: f32 = 20.0;
const ROW_GAP: f32 = 12.0;
const BODY_BOTTOM: f32 = 20.0;
/// A row's 16px icon and its value 12px after it.
const ICON: f32 = 16.0;
const VALUE_LEFT: f32 = PADDING_X + ICON + 12.0;
/// The fields' text: 12px/16px, the input nudged `top: 1px`, an underline 2px under it.
pub(super) const SMALL: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
pub(super) const UNDERLINE: f32 = 2.0;
/// The footer: a 1px border (the modal's), `padding: 15px 36px 16px` around Classic's 32px
/// dialog-style buttons 12px apart (`top: 2.5px`).
pub const FOOTER_HEIGHT: f32 = 1.0 + 15.0 + 32.0 + 16.0;
/// The right pane's footer, without the border, over the app's footer.
pub const PANE_FOOTER_HEIGHT: f32 = FOOTER_HEIGHT - 1.0;
const FOOTER_X: f32 = 36.0;
const BUTTON_NUDGE: f32 = 2.5;
const BUTTON_GAP: f32 = 12.0;

/// The form and its own UI state.
pub struct EventForm {
    pub mode: Mode,
    pub values: Values,
    original: Values,
    pub edited: Option<Edited>,
    /// Shown in the week view (a modal) rather than the right pane.
    pub week: bool,
    guest_input: String,
    guest_error: bool,
    /// The colours of the guests' calendars, and the column a hover expanded.
    pub guest_colors: super::guests::Colors,
    /// The highlighted suggestion, or (`chip`) guest.
    suggestion: usize,
    chip: Option<usize>,
    /// The guest's `Name <email>` tooltip and the meeting link's, and what a click on one asked
    /// for.
    chip_tip: exo_ui::CopyTooltip,
    link_tip: exo_ui::CopyTooltip,
    pub tip_clicked: Option<TipClick>,
    dropdown: Option<(Dropdown, usize)>,
    /// A field to focus on the next frame.
    focus: Option<Field>,
    /// The field that had the keyboard last frame, and the last one that had it.
    focused: Option<Field>,
    last_focused: Field,
    /// The right pane's form has given the keyboard to the mail (a click there, ⌘D). Its keys
    /// no longer work, and it says how to come back ([`paint_away_hint`]).
    pub away: bool,
    /// When's list has come over the day under the right pane's form.
    when_covered: bool,
    scroll: f32,
    /// The Save button's tooltip says to try again.
    pub failed: bool,
    /// A new event's When, typed.
    pub when: When,
    /// An event's When, edited.
    when_edit: WhenEdit,
    /// About's editor, and its height as last laid out.
    about: exo_editor::Editor,
    about_height: f32,
    /// The place highlighted under Where, and whether Escape closed its list (until the text
    /// changes).
    place: Option<usize>,
    places_closed: bool,
    /// What's typed in the calendar's field, which starts empty each time it takes the keyboard,
    /// and its list's highlighted row and scroll.
    calendar_input: String,
    calendar_selected: usize,
    calendar_scroll: f32,
    calendar_scrollbar: exo_ui::Scrollbar,
    /// Save takes the keyboard on the next frame (a calendar was picked).
    focus_save: bool,
    /// Repeat's Custom dialog, while it's up.
    pub custom: Option<Custom>,
}

/// The hint while the right pane's form has given the keyboard to the mail:
/// `To instantly move back here, hit` `⌘` `D`, 24px under the form (`margin-top: 24px`). Its
/// 24px line sits 2px in from the fields' left and 1px down, at `.35`, with the keys in blue caps,
/// and it fades in over 150ms. Returns the room it takes above the day.
pub fn paint_away_hint(ui: &Ui, form_bottom_left: Pos2, away: bool) -> f32 {
    const HINT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 24.0);
    const MARGIN_TOP: f32 = 24.0;
    let ctx = ui.ctx();
    let fade = exo_ui::motion::transition_in(
        ctx,
        exo_ui::id(ctx, "calendar-form-away-hint"),
        away,
        0.15,
        exo_ui::motion::CubicBezier::EASE,
    );
    if !away {
        return 0.0;
    }
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(fade);
    let palette = exo_ui::palette(ctx);
    let line = form_bottom_left + vec2(PADDING_X + 2.0, MARGIN_TOP + 1.0);
    let text = "To instantly move back here, hit";
    let width = HINT.paint_line(&painter, line, text, palette.ink(0.35, 0.35), None);
    let caps = exo_ui::Keycaps::new(
        ctx,
        &["⌘", "D"],
        exo_ui::KeycapColors::shortcuts_pane(palette),
    );
    caps.paint(&painter, line.x + width, line.y + 2.0);
    MARGIN_TOP + HINT.line_height
}

impl EventForm {
    /// A new event's form, with the title focused.
    pub fn create(values: Values, week: bool) -> Self {
        Self::new(
            Mode::Create,
            values.clone(),
            values,
            None,
            week,
            Field::Title,
        )
    }

    /// `event`'s form, with `focus` focused (for When, its start time, as the details' time opens
    /// it).
    pub fn edit(values: Values, edited: Edited, week: bool, focus: Field) -> Self {
        let mut form = Self::new(
            Mode::Edit,
            values.clone(),
            values,
            Some(edited),
            week,
            focus,
        );
        if focus == Field::When {
            form.when_edit.focus(when_edit::Part::StartTime);
        }
        form
    }

    fn new(
        mode: Mode,
        mut values: Values,
        mut original: Values,
        edited: Option<Edited>,
        week: bool,
        focus: Field,
    ) -> Self {
        // About holds the editor's HTML, so an untouched one isn't a change.
        let mut about = if values.about.is_empty() {
            exo_editor::Editor::new()
        } else {
            exo_editor::Editor::from_html(&values.about)
        };
        about.set_body_text(ABOUT_TEXT);
        // About doesn't detect dates, so `see you tomorrow` isn't marked as one.
        about.set_settings(exo_editor::Settings {
            skip_dates: true,
            ..exo_editor::Settings::default()
        });
        values.about = about_html(&about);
        original.about = values.about.clone();
        Self {
            mode,
            values,
            original,
            edited,
            week,
            guest_input: String::new(),
            guest_error: false,
            guest_colors: super::guests::Colors::default(),
            suggestion: 0,
            chip: None,
            chip_tip: exo_ui::CopyTooltip::default(),
            link_tip: exo_ui::CopyTooltip::default(),
            tip_clicked: None,
            dropdown: None,
            focus: Some(focus),
            focused: None,
            last_focused: focus,
            away: false,
            when_covered: false,
            scroll: 0.0,
            failed: false,
            when: When::default(),
            when_edit: WhenEdit::default(),
            about,
            about_height: ABOUT_TEXT.style.line_height,
            place: None,
            places_closed: false,
            calendar_input: String::new(),
            calendar_selected: 0,
            calendar_scroll: 0.0,
            calendar_scrollbar: exo_ui::Scrollbar::default(),
            focus_save: false,
            custom: None,
        }
    }

    /// The fields that differ from the event's.
    pub fn dirty(&self) -> Vec<Field> {
        let (now, then) = (&self.values, &self.original);
        let mut dirty = Vec::new();
        let mut differs = |field: Field, changed: bool| {
            if changed {
                dirty.push(field);
            }
        };
        differs(Field::Title, now.title != then.title);
        differs(
            Field::When,
            now.when != then.when || now.all_day != then.all_day,
        );
        differs(Field::Guests, now.guests != then.guests);
        differs(Field::Location, now.location != then.location);
        differs(Field::About, now.about != then.about);
        differs(Field::Meeting, now.meeting != then.meeting);
        differs(Field::Calendar, now.calendar != then.calendar);
        differs(Field::Repeat, now.repeat != then.repeat);
        dirty
    }

    /// Nothing worth asking about before it's thrown away: an edit with no changes, or a new event
    /// without a title, guests, place or notes.
    pub fn is_empty(&self) -> bool {
        match self.mode {
            Mode::Edit => self.dirty().is_empty(),
            Mode::Create => {
                self.values.title.trim().is_empty()
                    && self.values.guests.is_empty()
                    && self.values.location.trim().is_empty()
                    && self.values.about.trim().is_empty()
            }
        }
    }

    /// Whether the day under the right pane's form makes room for When's list. It does once the
    /// list has come within 10px of the day's top (`day_top`) while something's typed, as long as
    /// When has the keyboard and isn't emptied.
    pub fn when_covers(&mut self, day_top: f32) -> bool {
        let typing = self.focused == Some(Field::When) && !self.when.text.is_empty();
        if !typing {
            self.when_covered = false;
        } else if !self.when_covered {
            self.when_covered = self
                .when
                .list_bottom
                .is_some_and(|bottom| bottom + 10.0 > day_top);
        }
        self.when_covered
    }

    /// Gives the keyboard to the mail, so the field that had it lets go.
    pub fn give_up_keyboard(&mut self, ctx: &egui::Context) {
        self.away = true;
        if self.focused.is_some() {
            ctx.memory_mut(|memory| {
                if let Some(id) = memory.focused() {
                    memory.surrender_focus(id);
                }
            });
        }
    }

    /// Takes the keyboard back to the field that had it last.
    pub fn take_keyboard(&mut self) {
        self.focus(self.last_focused);
    }

    /// Puts the keyboard on `field` (the palette's `Edit …` rows, a row of the details), making
    /// it the form's again if the form had given it up. An edited event's When takes it on its
    /// start date, its first input.
    pub fn focus(&mut self, field: Field) {
        self.away = false;
        self.focus = Some(field);
        if field == Field::When && self.mode == Mode::Edit {
            self.when_edit.focus(when_edit::Part::StartDate);
        }
    }

    /// Whether a dropdown or suggestions are open, or When or the calendar's field holds text,
    /// which Escape takes first.
    pub fn has_dropdown(&self) -> bool {
        self.dropdown.is_some()
            || (!self.guest_input.trim().is_empty() && self.focused == Some(Field::Guests))
            || (self.typing_when() && !self.when.text.is_empty())
            || (!self.calendar_input.is_empty() && self.focused == Some(Field::Calendar))
            || (self.mode == Mode::Edit && self.when_edit.has_dropdown())
            || self.location_query().is_some()
    }

    /// What Where asks places for while it has the keyboard and its list isn't closed.
    pub fn location_query(&self) -> Option<&str> {
        let text = self.values.location.trim();
        (self.focused == Some(Field::Location) && !self.places_closed && !text.is_empty())
            .then_some(text)
    }

    /// Escape closes the dropdown or the suggestions, and in When and the calendar's field it
    /// clears the text.
    pub fn close_dropdown(&mut self) {
        if self.typing_when() && !self.when.text.is_empty() {
            self.when.clear();
            self.when_edited();
            return;
        }
        if self.focused == Some(Field::Calendar) && !self.calendar_input.is_empty() {
            // The field keeps the keyboard, which egui's Escape takes.
            self.calendar_input.clear();
            self.focus = Some(Field::Calendar);
            return;
        }
        self.dropdown = None;
        self.guest_input.clear();
        self.when_edit.close_dropdown();
        self.places_closed = true;
        self.place = None;
    }

    /// A new event's When has the keyboard.
    fn typing_when(&self) -> bool {
        self.mode == Mode::Create && self.focused == Some(Field::When)
    }

    /// When's text changed. In the right pane the draft's time goes until a suggestion is taken,
    /// while the week view keeps it.
    pub(super) fn when_edited(&mut self) {
        if !self.week {
            self.values.when = None;
            self.when.zone = None;
            self.when.mark_written(None, self.values.all_day);
        }
    }

    /// The suggestion the right pane's day previews: the highlighted one while When lists some.
    pub fn when_preview(&self) -> Option<&when::Choice> {
        if self.week || !self.typing_when() {
            return None;
        }
        self.when.choices().get(self.when.selected)
    }

    /// Takes When's suggestion `index` and moves to the next field.
    pub(super) fn commit_when(&mut self, index: usize, now: &Zoned) {
        if let Some(choice) = self.when.commit(index, self.values.all_day, now) {
            self.values.when = Some((choice.start, choice.end));
            self.values.all_day = choice.all_day;
            self.focus = self.next(Field::When);
        }
    }

    /// The draft's times as the calendar shows them, and the time its block shows when it was
    /// typed in another zone. In the right pane, these come from the highlighted suggestion while
    /// When lists some.
    pub fn draft_when(&self, now: &Zoned) -> Option<(Zoned, Zoned, Option<String>)> {
        if let Some(choice) = self.when_preview() {
            return Some((
                choice.start.clone(),
                choice.end.clone(),
                choice
                    .zone
                    .as_ref()
                    .map(|zone| when::zone_block_time(zone, now)),
            ));
        }
        let (start, end) = self.values.when.clone()?;
        let tz = now.time_zone();
        // An edited event's times put in another zone are labelled in it.
        let zone = match self.mode {
            Mode::Create => self.when.zone.clone(),
            Mode::Edit => (start.time_zone() != tz && !self.values.all_day).then(|| {
                crate::time_suggest::ZoneTime {
                    label: String::new(),
                    tz: Some(start.time_zone().clone()),
                    start: start
                        .datetime()
                        .to_zoned(tz.clone())
                        .map_or(start.timestamp(), |at| at.timestamp()),
                    end: end
                        .datetime()
                        .to_zoned(tz.clone())
                        .map_or(end.timestamp(), |at| at.timestamp()),
                }
            }),
        };
        Some((
            start.with_time_zone(tz.clone()),
            end.with_time_zone(tz.clone()),
            zone.as_ref().map(|zone| when::zone_block_time(zone, now)),
        ))
    }

    /// The form's height: the header and the rows, without the footer.
    pub fn content_height(&self, pane: bool) -> f32 {
        let header = if pane {
            HEADER_TOP_PANE
        } else {
            HEADER_TOP_MODAL
        } + TITLE_HEIGHT
            + HEADER_BOTTOM;
        let rows: f32 = Field::rows(self.mode)
            .iter()
            .map(|field| self.row_height(*field, pane) - field.pulled_up())
            .sum::<f32>()
            + ROW_GAP * (Field::rows(self.mode).len() - 1) as f32;
        header + BODY_TOP + rows + if pane { 0.0 } else { BODY_BOTTOM }
    }

    fn row_height(&self, field: Field, pane: bool) -> f32 {
        match field {
            Field::Guests => self.guests_row_height(pane),
            Field::When
                if self.mode == Mode::Create
                    && self.when.zone.is_some()
                    && self.values.when.is_some() =>
            {
                ROW + when::ZONE_LINE
            }
            Field::When if self.mode == Mode::Edit && self.values.when.is_some() => {
                when_edit::HEIGHT
            }
            // The editor's lines, its padding and border (as last laid out).
            Field::About => self.about_height + 2.0 + UNDERLINE,
            _ => ROW,
        }
    }

    /// Paints the form's header and rows in `rect` (`pane` in the right pane).
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, shown: &Shown<'_>, pane: bool) {
        self.when.list_bottom = None;
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        // A new area's first pass sizes it with its widgets disabled, which drops their focus.
        let focus = if ui.is_sizing_pass() || !ui.is_enabled() {
            None
        } else {
            self.focus.take()
        };
        let mut focused = None;

        // Header: the title, after the colour square's room. The square itself stays empty, since
        // no way of opening the form gives it a colour.
        let top = rect.top()
            + if pane {
                HEADER_TOP_PANE
            } else {
                HEADER_TOP_MODAL
            };
        let title_line = Rect::from_min_max(
            pos2(rect.left() + TITLE_LEFT, top + 2.0),
            pos2(rect.right() - PADDING_X, top + 2.0 + TITLE.line_height),
        );
        let title = input(
            ui,
            Field::Title.id(ui.ctx()),
            &mut self.values.title,
            title_line,
            InputSpec {
                style: &TITLE,
                placeholder: "Event title",
                error: false,
                focus: focus == Some(Field::Title),
                spellcheck: true,
            },
        );
        if title.has_focus() {
            focused = Some(Field::Title);
        }
        if title.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.focus = Some(Field::rows(self.mode)[0]);
        }
        let header_bottom = top + TITLE_HEIGHT + HEADER_BOTTOM;

        // The rows, scrolling under the header when they don't fit.
        let body = Rect::from_min_max(pos2(rect.left(), header_bottom), rect.max);
        let content: f32 = self.content_height(pane) - (header_bottom - rect.top());
        if ui.rect_contains_pointer(body) {
            self.scroll -= ui.input(|input| input.smooth_scroll_delta.y);
        }
        // Find scrolls at once to a match out of view.
        let find = exo_ui::id(ui.ctx(), "calendar-form-find");
        self.scroll += exo_ui::find::reveal(ui.ctx(), find).map_or(0.0, |delta| delta.y);
        self.scroll = self.scroll.clamp(0.0, (content - body.height()).max(0.0));
        let found = exo_ui::find::Scope::begin(ui.ctx(), find, body, self.scroll);
        let mut y = body.top() + BODY_TOP - self.scroll;
        let mut row_ctx = RowContext {
            shown,
            form: rect,
            pane,
            overlay: None,
        };
        // A focused input's underline hangs 1px under its row (the box starts 1px down). The right
        // pane's last row ends the form on that underline.
        let clip = ui
            .clip_rect()
            .intersect(body.with_max_y(body.bottom() + 1.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
        child.set_clip_rect(clip);
        for &field in Field::rows(self.mode) {
            y -= field.pulled_up();
            let height = self.row_height(field, pane);
            let row = Rect::from_min_max(
                pos2(rect.left() + PADDING_X, y),
                pos2(rect.right() - PADDING_X, y + height),
            );
            // The row's icon: `rgba(0,0,0,.35)` / `hsla(0,0%,100%,.35)`.
            let label = Rect::from_min_size(row.min, Vec2::splat(ICON));
            let icon = exo_ui::icon_rect(child.ctx(), field.icon(), label);
            let icon = icon.translate(vec2(0.0, row.top() + field.icon_top() - icon.top()));
            exo_ui::paint_icon(&child, field.icon(), icon, palette.ink(0.35, 0.35));
            let value = Rect::from_min_max(
                pos2(rect.left() + VALUE_LEFT, y + 1.0),
                pos2(row.right(), y + 1.0 + SMALL.line_height),
            );
            let has_focus = match field {
                Field::Guests => {
                    self.guests_row(&mut child, value, focus == Some(field), &mut row_ctx)
                }
                Field::When => self.when_row(&mut child, value, focus == Some(field), &mut row_ctx),
                Field::Repeat => {
                    self.repeat_row(&mut child, value, focus == Some(field), &mut row_ctx)
                }
                Field::Meeting => {
                    self.meeting_row(&mut child, value, focus == Some(field), &mut row_ctx)
                }
                Field::Location => {
                    self.location_row(&mut child, value, focus == Some(field), &mut row_ctx)
                }
                Field::About => self.about_row(
                    &mut child,
                    Rect::from_min_max(row.min + vec2(VALUE_LEFT - PADDING_X, 0.0), row.max),
                    focus == Some(field),
                ),
                Field::Calendar => {
                    self.calendar_row(&mut child, value, focus == Some(field), &mut row_ctx)
                }
                Field::Title => false,
            };
            if has_focus {
                focused = Some(field);
            }
            y += height + ROW_GAP;
        }
        drop(found);
        if self.focused == Some(Field::Guests) && focused != Some(Field::Guests) {
            // Leaving Who takes what's typed.
            self.commit_guests();
            self.chip = None;
        }
        self.focused = focused;
        if let Some(field) = focused {
            self.last_focused = field;
        }
        if let Some(paint) = row_ctx.overlay {
            self.paint_overlay(ui, paint, shown);
        }
    }

    /// Paints Save and Cancel in `footer`, with its top border unless in the right pane. Save
    /// glows while it has the keyboard (`:focus`). Returns what was clicked.
    pub fn footer(&mut self, ui: &Ui, footer: Rect, pane: bool) -> Option<FormAction> {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        if !pane {
            ui.painter().hline(
                footer.x_range(),
                footer.top() + 0.5,
                Stroke::new(1.0, palette.border_base_default),
            );
        }
        let top = footer.top() + if pane { 0.0 } else { 1.0 } + 15.0 + BUTTON_NUDGE;
        let enabled = self.mode == Mode::Create || !self.dirty().is_empty();
        let carbon = palette.theme == exo_ui::Theme::Carbon;
        let mut x = footer.left() + FOOTER_X;
        let mut clicked = None;
        for (label, action) in [("Save", FormAction::Save), ("Cancel", FormAction::Cancel)] {
            let save = action == FormAction::Save;
            let id = exo_ui::id(ui.ctx(), ("calendar-form-button", label));
            // Save and Cancel are Classic's uppercase dialog buttons. A disabled Save is unfilled
            // in Carbon too (`background: none`).
            let button = exo_ui::DialogAction::new(
                label,
                if save {
                    exo_ui::ActionKind::Primary
                } else {
                    exo_ui::ActionKind::Plain
                },
            )
            .enabled(!save || enabled)
            .focused(save && enabled && ctx.memory(|memory| memory.has_focus(id)))
            .disabled_fill(Color32::TRANSPARENT)
            .dark(carbon);
            let rect = Rect::from_min_size(
                pos2(x, top),
                vec2(button.width(&ctx), exo_ui::DIALOG_ACTION_HEIGHT),
            );
            x = rect.right() + BUTTON_GAP;
            let response = button.show(ui, id, rect);
            // Save takes the keyboard only once the pointer is up, because a click elsewhere takes
            // the keyboard from what has it.
            if save && self.focus_save && !ui.input(|input| input.pointer.any_down()) {
                self.focus_save = false;
                response.request_focus();
            }
            let (tip, keys): (&str, &[&str]) = match (action, self.mode) {
                (FormAction::Save, _) if self.failed => ("Please try to save again.", &[]),
                (FormAction::Save, _) => ("Save Event", &["⌘", "enter"]),
                (FormAction::Cancel, Mode::Create) => ("Cancel Event", &["⌘", "shift", ","]),
                (FormAction::Cancel, Mode::Edit) => ("Discard changes", &["⌘", "shift", ","]),
            };
            // Shown 16px over the button while it has the keyboard.
            KeyTooltip { text: tip, keys }.show_at_focused(
                ui,
                &response,
                exo_ui::Placement::Above(-16.0),
                response.has_focus(),
                None,
            );
            if response.clicked() {
                clicked = Some(action);
            }
        }
        clicked
    }

    /// Makes the event all day, over the start's days, or takes it back to a time, half an hour
    /// from 9:00 on its start's day.
    pub fn toggle_all_day(&mut self) {
        let Some((start, end)) = self.values.when.clone() else {
            return;
        };
        self.values.all_day = !self.values.all_day;
        let tz = start.time_zone().clone();
        self.values.when = if self.values.all_day {
            let first = start.date().to_zoned(tz.clone()).ok();
            let last = end
                .checked_sub(1.nanosecond())
                .ok()
                .map(|end| end.date())
                .unwrap_or(start.date());
            let after = last.tomorrow().ok().and_then(|day| day.to_zoned(tz).ok());
            first.zip(after).or(Some((start, end)))
        } else {
            let from = start.date().at(9, 0, 0, 0).to_zoned(tz).ok();
            from.and_then(|from| from.checked_add(30.minutes()).ok().map(|to| (from, to)))
                .or(Some((start, end)))
        };
    }

    /// The day a rule repeats from: the event's start, else today.
    pub(super) fn anchor(&self, now: &Zoned) -> jiff::civil::Date {
        self.values
            .when
            .as_ref()
            .map_or_else(|| now.date(), |(start, _)| start.date())
    }

    /// Paints Repeat's Custom dialog over `page` while it's up, and takes its rule.
    pub fn show_custom(
        &mut self,
        ctx: &egui::Context,
        page: Rect,
        now: &Zoned,
        week_start: Weekday,
    ) {
        let Some(custom) = &mut self.custom else {
            return;
        };
        match custom.show(ctx, page, now.date(), week_start) {
            Some(CustomAction::Done(rule)) => {
                self.values.repeat = Some(rule);
                self.custom = None;
                self.focus = Some(Field::Repeat);
            }
            Some(CustomAction::Cancel) => {
                self.custom = None;
                self.focus = Some(Field::Repeat);
            }
            None => {}
        }
    }

    /// ⌘Enter in the Custom dialog: Done, when it can.
    pub fn finish_custom(&mut self) {
        if let Some(rule) = self.custom.as_ref().and_then(Custom::done) {
            self.values.repeat = Some(rule);
            self.custom = None;
            self.focus = Some(Field::Repeat);
        }
    }

    /// Escape in the Custom dialog closes its open list, or else the dialog.
    pub fn escape_custom(&mut self) {
        if let Some(custom) = &mut self.custom
            && !custom.escape()
        {
            self.custom = None;
            self.focus = Some(Field::Repeat);
        }
    }

    /// About's editor.
    pub fn about(&self) -> &exo_editor::Editor {
        &self.about
    }

    /// About's editor, for the formatting keys.
    pub fn about_mut(&mut self) -> &mut exo_editor::Editor {
        &mut self.about
    }

    /// Takes About's HTML again after an edit made outside the form (a spelling correction).
    pub fn sync_about(&mut self) {
        self.values.about = about_html(&self.about);
    }

    /// About has the keyboard, so its formatting keys apply, and Escape is the editor's when the
    /// editor has something open.
    pub fn about_focused(&self) -> bool {
        self.focused == Some(Field::About)
    }

    /// Whether the event's own meeting link has the keyboard, so ⌘C copies it.
    pub fn meeting_link_focused(&self) -> bool {
        self.focused == Some(Field::Meeting) && matches!(self.values.meeting, Meeting::Existing(_))
    }

    /// The event's own meeting link as it opens and copies: a Meet link as the account of the
    /// event's calendar among `calendars` (`authuser`), else as `me`.
    pub fn meeting_link(&self, calendars: &[CalendarOption], me: &str) -> Option<String> {
        let Meeting::Existing(link) = &self.values.meeting else {
            return None;
        };
        let Ok(mut url) = url::Url::parse(link) else {
            return Some(link.clone());
        };
        if url.host_str() == Some("meet.google.com") {
            let account = calendars
                .iter()
                .find(|calendar| calendar.id == self.values.calendar)
                .map_or(me, |calendar| calendar.account.as_str());
            url.query_pairs_mut().append_pair("authuser", account);
        }
        Some(url.to_string())
    }

    /// The field after `field` in the mode's order.
    pub(super) fn next(&self, field: Field) -> Option<Field> {
        let rows = Field::rows(self.mode);
        rows.iter()
            .position(|row| *row == field)
            .and_then(|at| rows.get(at + 1))
            .copied()
    }
}
