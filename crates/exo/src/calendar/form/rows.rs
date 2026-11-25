//! Form field rows and single-line input rendering.

use egui::{Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use exo_ui::{Family, Font, TextStyle};

use super::{
    EventForm, Field, Meeting, Mode, SMALL, Shown, TipClick, UNDERLINE,
    overlays::{
        CalendarRow, Dropdown, Overlay, calendar_highlight, calendar_label, calendar_rows,
        calendar_step,
    },
};
use crate::calendar::{places::Place, repeat, when, when_edit};

/// The meeting link's pill: 5px either side of its text, 3px above it and 22px tall, its cross an
/// 8.47px glyph 2px after the text in `#a3a3a3`, and `#aeb1dd` round it with the keyboard.
const PILL_PADDING: f32 = 5.0;
const PILL_ABOVE: f32 = 3.0;
const PILL_HEIGHT: f32 = 22.0;
const PILL_CROSS_GAP: f32 = 2.0;
const PILL_CROSS_WIDTH: f32 = 8.47;
const PILL_CROSS: Color32 = Color32::from_rgb(0xa3, 0xa3, 0xa3);
const ACCENT: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// About (the compose editor) has 12px/16px lines of the email font (`Family::SansEmail`), with
/// the font's content area 15px from the line's top. The placeholder, painted here, is 1px lower,
/// at `.35`. The text stops short of the scrollbar's 11px gutter (`scrollbar-gutter: stable`), over
/// `padding-bottom: 2px` and a 2px border. Hovered, a box 6px wider each side takes `#f5f7fa` /
/// `#363a40`.
pub(super) const ABOUT_TEXT: exo_editor::BodyText = exo_editor::BodyText {
    style: TextStyle::new(Font::new(Family::SansEmail, 400), 12.0, 16.0),
    content_top: 0.0,
    content_height: 15.0,
    placeholder_opacity: 0.35 / 0.9,
};
const ABOUT_GUTTER: f32 = 11.0;
const ABOUT_HOVER_OUTSET: f32 = 6.0;

/// Per-frame form context shared by field row painters.
pub(super) struct RowContext<'a, 's> {
    pub shown: &'a Shown<'s>,
    pub form: Rect,
    pub pane: bool,
    pub overlay: Option<Overlay>,
}

/// Visual and focus properties for a single-line form [`input`].
pub(super) struct InputSpec<'a> {
    pub style: &'a TextStyle,
    pub placeholder: &'a str,
    pub error: bool,
    pub focus: bool,
    pub spellcheck: bool,
}

impl EventForm {
    /// When: for a new event, the typed time with its suggestions listed across the `form`, and
    /// for an edit, the event's dates, times and zone ([`when_edit::WhenEdit`]).
    pub(super) fn when_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        if self.mode == Mode::Edit {
            return self.edited_when(ui, value, shown);
        }
        let palette = exo_ui::palette(ui.ctx());
        let id = Field::When.id(ui.ctx());
        if focus {
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        self.when
            .sync(self.values.when.as_ref(), self.values.all_day, shown.now);
        let had_focus = ui.memory(|memory| memory.has_focus(id));
        let (up, down, enter, tab) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::Tab),
            )
        });
        let before = self.when.text.clone();
        let response = input(
            ui,
            id,
            &mut self.when.text,
            value,
            InputSpec {
                style: &SMALL,
                placeholder: "tomorrow 2 pm 30 min new york",
                error: self.when.error,
                focus: false,
                spellcheck: false,
            },
        );
        if response.gained_focus() {
            self.when.focused(self.values.when.is_some(), shown.now);
        }
        if self.when.text != before {
            self.when.edited(shown.now);
            self.when_edited();
        }
        if had_focus && !self.when.choices().is_empty() {
            if up {
                self.when.step(-1);
            } else if down {
                self.when.step(1);
            } else if enter || tab {
                self.commit_when(self.when.selected, shown.now);
            }
        } else if had_focus && enter {
            // With nothing to take, the field keeps the keyboard, which the input gave up.
            self.focus = Some(Field::When);
        }
        if let Some(line) =
            self.when
                .local_line(self.values.when.as_ref(), self.values.all_day, shown.now)
        {
            // The time here, under the field, in Classic's .35.
            SMALL.paint_line(
                ui.painter(),
                pos2(value.left(), value.top() + when::ZONE_LINE_TOP),
                &line,
                palette.ink(0.35, 0.35),
                Some(value.width()),
            );
        }
        if response.has_focus() && !self.when.choices().is_empty() {
            row.overlay = Some(Overlay::When {
                below: value,
                form: row.form,
            });
        }
        response.has_focus()
    }

    /// An edited event's When, with its changes taken.
    fn edited_when(&mut self, ui: &mut Ui, value: Rect, shown: &Shown<'_>) -> bool {
        let Some((start, end)) = self.values.when.clone() else {
            return false;
        };
        let edit = when_edit::Shown {
            start: &start,
            end: &end,
            all_day: self.values.all_day,
            today: shown.now.date(),
            week_start: shown.week_start,
        };
        let (changes, focused) = self.when_edit.show(ui, value.min, value.width(), &edit);
        for change in changes {
            match change {
                when_edit::Change::Times(start, end) => self.values.when = Some((start, end)),
                when_edit::Change::AllDay => self.toggle_all_day(),
                // A new zone keeps the same clock times in it.
                when_edit::Change::Zone(tz) => {
                    if let Some((start, end)) = &self.values.when
                        && let (Ok(start), Ok(end)) = (
                            start.datetime().to_zoned(tz.clone()),
                            end.datetime().to_zoned(tz),
                        )
                    {
                        self.values.when = Some((start, end));
                    }
                }
            }
        }
        focused
    }

    /// Where: the input, and the places found for it while it has the keyboard. ↓ and ↑ move
    /// through them, and Enter takes the highlighted one or else moves on.
    pub(super) fn location_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        let id = Field::Location.id(ui.ctx());
        let had_focus = ui.memory(|memory| memory.has_focus(id));
        let (up, down, enter) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
            )
        });
        let before = self.values.location.clone();
        let response = input(
            ui,
            id,
            &mut self.values.location,
            value,
            InputSpec {
                style: &SMALL,
                placeholder: "Enter a location",
                error: false,
                focus,
                spellcheck: false,
            },
        );
        if self.values.location != before {
            self.place = None;
            self.places_closed = false;
        }
        let listed = !self.places_closed
            && !self.values.location.trim().is_empty()
            && !shown.places.is_empty();
        if had_focus && listed {
            if down {
                self.place = Some(
                    self.place
                        .map_or(0, |place| (place + 1).min(shown.places.len() - 1)),
                );
            } else if up {
                self.place = self.place.and_then(|place| place.checked_sub(1));
            }
        }
        if had_focus && enter {
            match self
                .place
                .and_then(|place| shown.places.get(place))
                .filter(|_| listed)
            {
                Some(place) => self.pick_place(place),
                None => self.focus = self.next(Field::Location),
            }
        }
        if response.has_focus() && listed {
            row.overlay = Some(Overlay::Places {
                below: value,
                items: shown.places.to_vec(),
                form: row.form,
                pane: row.pane,
            });
        }
        response.has_focus()
    }

    /// Takes `place` as Where, with its list closed until the text changes.
    pub(super) fn pick_place(&mut self, place: &Place) {
        self.values.location = place.location();
        self.place = None;
        self.places_closed = true;
        self.focus = Some(Field::Location);
    }

    /// The meeting link: `Add a meeting link`, or the link (or its state) with a ×. A link shows
    /// its copy tooltip on hover.
    pub(super) fn meeting_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        let palette = exo_ui::palette(ui.ctx());
        let id = Field::Meeting.id(ui.ctx());
        let response = ui
            .interact(value, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if focus {
            response.request_focus();
        }
        let keys = ui.input(|input| {
            (
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::Backspace) || input.key_pressed(egui::Key::Delete),
            )
        });
        let mut just_opened = false;
        match self.values.meeting.clone() {
            Meeting::None => {
                SMALL.paint_line(
                    ui.painter(),
                    value.min,
                    "Add a meeting link",
                    palette.text_base_accent,
                    None,
                );
                // A click focuses the trigger, as the button takes focus, so its list's keys
                // work. With the list open, Enter is the list's.
                if response.clicked() {
                    response.request_focus();
                }
                if (response.clicked() && !keys.0)
                    || (response.has_focus() && keys.0 && self.dropdown.is_none())
                {
                    if self
                        .dropdown
                        .is_some_and(|(open, _)| open == Dropdown::Meeting)
                    {
                        self.dropdown = None;
                    } else {
                        self.dropdown = Some((Dropdown::Meeting, 0));
                        just_opened = true;
                    }
                }
            }
            meeting => {
                // The pill takes the keyboard on a click, for Backspace and ⌘C.
                if response.clicked() {
                    response.request_focus();
                }
                // The link, or what will become of it, in a pill that shows when hovered or with the
                // keyboard: 5px either side of the text and 3px over it, 22px tall, with its cross
                // (2px after the text) only then.
                let text = match &meeting {
                    Meeting::Existing(link) => link
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .to_owned(),
                    _ if shown.online => "Link will be added when saved".to_owned(),
                    _ => "Link will be added when online".to_owned(),
                };
                let carbon = palette.theme == exo_ui::Theme::Carbon;
                let text_at = value.min;
                let width = SMALL.width(ui.ctx(), &text);
                let cross = Rect::from_min_size(
                    text_at + vec2(width + PILL_CROSS_GAP, 0.0),
                    vec2(PILL_CROSS_WIDTH, SMALL.line_height),
                );
                let pill = Rect::from_min_max(
                    text_at - vec2(PILL_PADDING, PILL_ABOVE),
                    pos2(
                        cross.right() + PILL_PADDING,
                        text_at.y - PILL_ABOVE + PILL_HEIGHT,
                    ),
                );
                let focused = response.has_focus();
                let lit = focused || ui.rect_contains_pointer(pill);
                if lit {
                    // `#fafafa` (Carbon `#555b64`) in a 1px border, `#aeb1dd` with the keyboard and
                    // otherwise `#f3f3f3` (Carbon `#636973`).
                    let fill = if carbon {
                        Color32::from_rgb(0x55, 0x5b, 0x64)
                    } else {
                        Color32::from_rgb(0xfa, 0xfa, 0xfa)
                    };
                    let border = match (focused, carbon) {
                        (true, _) => ACCENT,
                        (false, true) => Color32::from_rgb(0x63, 0x69, 0x73),
                        (false, false) => Color32::from_rgb(0xf3, 0xf3, 0xf3),
                    };
                    ui.painter().rect(
                        exo_ui::pixel_snap(pill),
                        CornerRadius::same(2),
                        fill,
                        Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                }
                // The link at `.9` (Carbon `.85`), a state at `.35`.
                let ink = if matches!(meeting, Meeting::Existing(_)) {
                    palette.ink(0.9, 0.85)
                } else {
                    palette.ink(0.35, 0.35)
                };
                SMALL.paint_line(ui.painter(), text_at, &text, ink, None);
                let close = "legacy/DEPRECATED_closeV2";
                let removed = lit && {
                    // The glyph's square fills the cross's width.
                    exo_ui::paint_icon(
                        ui,
                        close,
                        Rect::from_center_size(cross.center(), Vec2::splat(PILL_CROSS_WIDTH)),
                        PILL_CROSS,
                    );
                    ui.interact(
                        cross,
                        exo_ui::id(ui.ctx(), "calendar-form-meeting-remove"),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                };
                if removed || (response.has_focus() && keys.1) {
                    self.values.meeting = Meeting::None;
                }
                let tip = self
                    .meeting_link(shown.calendars, shown.me)
                    .and_then(|copied| {
                        ui.rect_contains_pointer(pill).then(|| exo_ui::CopyTip {
                            target: pill,
                            shown: text.clone(),
                            copied,
                        })
                    });
                let id = exo_ui::id(ui.ctx(), "calendar-form-meeting-tip");
                if let Some((click, tip)) = self.link_tip.show(ui, id, tip) {
                    self.tip_clicked = Some(match click {
                        exo_ui::CopyClick::Text => TipClick::Open(tip.copied),
                        exo_ui::CopyClick::Icon => TipClick::Copy(tip.copied),
                    });
                }
            }
        }
        if response.has_focus() && self.values.meeting == Meeting::None {
            underline(ui, value, palette.border_brand_default);
        }
        if let Some((Dropdown::Meeting, highlighted)) = self.dropdown {
            row.overlay = Some(Overlay::Menu {
                below: value,
                trigger_hovered: response.hovered(),
                just_opened,
                which: Dropdown::Meeting,
                items: vec!["Google Meet".to_owned()],
                highlighted,
            });
        }
        response.has_focus()
    }

    /// Repeat: the rule's name and, when clicked, the presets for the event's start, with `Custom`
    /// opening its dialog.
    pub(super) fn repeat_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        let palette = exo_ui::palette(ui.ctx());
        let anchor = self.anchor(shown.now);
        let text = repeat::label(self.values.repeat.as_ref(), anchor);
        let width = SMALL.width(ui.ctx(), &text);
        let trigger = Rect::from_min_size(value.min, vec2(width, value.height()));
        let response = ui
            .interact(trigger, Field::Repeat.id(ui.ctx()), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        // A click focuses the trigger, as the button takes focus, so its list's keys work.
        if focus || response.clicked() {
            response.request_focus();
        }
        // A placeholder at `.35` while it doesn't repeat.
        let ink = if self.values.repeat.is_none() {
            palette.ink(0.35, 0.35)
        } else {
            palette.text_base_default
        };
        SMALL.paint_line(ui.painter(), value.min, &text, ink, None);
        if response.has_focus() {
            // Focused: Classic's `#aeb1dd`, 2px under the trigger's 2px padding.
            ui.painter().rect_filled(
                Rect::from_min_size(
                    pos2(trigger.left(), trigger.top() + SMALL.line_height + 2.0),
                    vec2(width, 2.0),
                ),
                0.0,
                Color32::from_rgb(0xae, 0xb1, 0xdd),
            );
        }
        // Enter on the focused trigger also clicks it (egui's). That opens the list, and with the
        // list open, Enter is the list's.
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
        let mut just_opened = false;
        if (response.clicked() && !enter)
            || (response.has_focus() && enter && self.dropdown.is_none())
        {
            if self
                .dropdown
                .is_some_and(|(open, _)| open == Dropdown::Repeat)
            {
                self.dropdown = None;
            } else {
                self.dropdown = Some((Dropdown::Repeat, 0));
                just_opened = true;
            }
        }
        if response.has_focus() && self.dropdown.is_some() {
            // ↑ and ↓ move through the open list, not to the next field (egui's focus moves).
            let filter = egui::EventFilter {
                vertical_arrows: true,
                ..Default::default()
            };
            let id = Field::Repeat.id(ui.ctx());
            ui.memory_mut(|memory| memory.set_focus_lock_filter(id, filter));
        }
        if let Some((Dropdown::Repeat, highlighted)) = self.dropdown {
            let items = repeat::presets(anchor)
                .into_iter()
                .map(|(_, name)| name)
                .collect();
            row.overlay = Some(Overlay::Menu {
                below: value,
                trigger_hovered: response.hovered(),
                just_opened,
                which: Dropdown::Repeat,
                items,
                highlighted,
            });
        }
        response.has_focus()
    }

    /// The calendar it goes on: a field showing the calendar's name, which empties as it takes the
    /// keyboard and lists the writable calendars by account under it, filtered by what's typed.
    /// ↑/↓ move through them round the ends, and Enter or a click takes one and moves on to Save.
    /// Leaving the field empties it.
    pub(super) fn calendar_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        let id = Field::Calendar.id(ui.ctx());
        let had_focus = ui.memory(|memory| memory.has_focus(id));
        let (up, down, enter) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if had_focus {
            let rows = calendar_rows(shown.calendars, &self.calendar_input);
            let at = calendar_highlight(&rows, self.calendar_selected);
            if up || down {
                self.calendar_selected = calendar_step(&rows, at, up);
                self.calendar_keep_in_view(&rows, ui.ctx());
            } else if enter {
                if let Some(CalendarRow::Calendar(calendar)) = rows.get(at) {
                    let id = calendar.id.clone();
                    self.pick_calendar(id);
                } else {
                    // With nothing to take, the field keeps the keyboard (egui's Enter lets it
                    // go).
                    self.focus = Some(Field::Calendar);
                }
            }
        }
        // Without the keyboard it shows the calendar's name, and with it (or taking it back)
        // what's typed.
        let typing = had_focus || focus;
        let mut text = if typing {
            std::mem::take(&mut self.calendar_input)
        } else {
            calendar_label(shown.calendars, &self.values.calendar)
        };
        let before = text.clone();
        let response = input(
            ui,
            id,
            &mut text,
            value,
            InputSpec {
                style: &SMALL,
                placeholder: "Enter calendar name",
                error: false,
                focus,
                spellcheck: false,
            },
        );
        if typing {
            if text != before {
                self.calendar_selected = 0;
                self.calendar_scroll = 0.0;
            }
            self.calendar_input = text;
        }
        if response.has_focus() && !had_focus {
            // Taking the keyboard starts from the top, with the first calendar highlighted.
            self.calendar_selected = 0;
            self.calendar_scroll = 0.0;
        } else if had_focus && !response.has_focus() && self.focus != Some(Field::Calendar) {
            self.calendar_input.clear();
        }
        if response.has_focus() && !shown.calendars.is_empty() {
            row.overlay = Some(Overlay::Calendars {
                below: value,
                form: row.form,
                pane: row.pane,
            });
        }
        response.has_focus()
    }

    /// Takes calendar `id`. The field empties and Save takes the keyboard.
    pub(super) fn pick_calendar(&mut self, id: String) {
        self.values.calendar = id;
        self.calendar_input.clear();
        self.focus_save = true;
    }

    /// About: the compose editor in `rect`, with its border while focused and its hovered box
    /// while not.
    /// Returns whether it has the keyboard.
    pub(super) fn about_row(&mut self, ui: &mut Ui, rect: Rect, focus: bool) -> bool {
        let palette = exo_ui::palette(ui.ctx());
        let text = Rect::from_min_max(
            rect.min,
            pos2(rect.right() - ABOUT_GUTTER, rect.bottom() - 2.0 - UNDERLINE),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text)
                .id_salt("calendar-form-about"),
        );
        let focused = self.focused == Some(Field::About);
        if !focused && ui.rect_contains_pointer(rect) {
            let hover = if palette.theme == exo_ui::Theme::Carbon {
                Color32::from_rgb(0x36, 0x3a, 0x40)
            } else {
                Color32::from_rgb(0xf5, 0xf7, 0xfa)
            };
            ui.painter()
                .rect_filled(rect.expand2(vec2(ABOUT_HOVER_OUTSET, 0.0)), 0.0, hover);
        }
        // The placeholder sits 1px under where the text starts.
        if self.about.is_empty() {
            ABOUT_TEXT.style.paint_line(
                ui.painter(),
                text.min + vec2(0.0, 1.0),
                "Enter agenda or notes",
                palette.ink(0.35, 0.35),
                Some(text.width()),
            );
        }
        let style = exo_editor::EditorStyle {
            placeholder: None,
            min_height: ABOUT_TEXT.style.line_height,
        };
        let edited = self.about.ui(&mut child, &style);
        crate::spelling::note_editor(
            child.ctx(),
            crate::spelling::EditorTarget::CalendarAbout,
            &edited,
        );
        if focus {
            edited.response.request_focus();
        }
        self.about_height = edited.response.rect.height();
        if edited.changed {
            self.values.about = about_html(&self.about);
        }
        let focused = edited.response.has_focus() || focus;
        if focused {
            // The box's focused border, Classic's `#aeb1dd`.
            ui.painter().rect_filled(
                Rect::from_min_size(
                    pos2(rect.left(), text.top() + self.about_height + 2.0),
                    vec2(rect.width(), UNDERLINE),
                ),
                0.0,
                Color32::from_rgb(0xae, 0xb1, 0xdd),
            );
        }
        focused
    }
}

/// A single-line input on `line` with its placeholder and, while focused (or `error`), its
/// underline. `focus` takes the keyboard to it; with `spellcheck`, it's spellchecked.
pub(super) fn input(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    line: Rect,
    spec: InputSpec<'_>,
) -> egui::Response {
    let palette = exo_ui::palette(ui.ctx());
    if spec.focus {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    if text.is_empty() {
        spec.style.paint_line(
            ui.painter(),
            line.min,
            spec.placeholder,
            palette.text_base_accent,
            Some(line.width()),
        );
    }
    // The form's inputs are `rgba(0, 0, 0, .9)`, Carbon's `hsla(0, 0%, 100%, .85)`.
    let ink = palette.ink(0.9, 0.85);
    let response = if spec.spellcheck {
        crate::spelling::text_field(
            ui,
            id,
            text,
            line,
            spec.style,
            ink,
            palette.text_selection_default,
        )
    } else {
        exo_ui::text_field(
            ui,
            id,
            text,
            line,
            spec.style,
            ink,
            palette.text_selection_default,
        )
    };
    if spec.error {
        underline(ui, line, palette.border_critical_default);
    } else if response.has_focus() {
        underline(ui, line, palette.border_brand_default);
    }
    response
}

/// The editor's HTML, or nothing for an empty one.
pub(super) fn about_html(editor: &exo_editor::Editor) -> String {
    if editor.is_empty() {
        String::new()
    } else {
        editor.html()
    }
}

/// The 2px underline under an input's `line`.
fn underline(ui: &Ui, line: Rect, color: Color32) {
    let bottom = line.bottom() + 2.0;
    ui.painter().rect_filled(
        Rect::from_min_max(
            pos2(line.left(), bottom),
            pos2(line.right(), bottom + UNDERLINE),
        ),
        0.0,
        color,
    );
}
