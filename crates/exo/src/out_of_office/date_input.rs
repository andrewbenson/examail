//! The dialog's date fields: the shared [`exo_ui::DateInput`] and its popup, the input drawn as the
//! dialog styles it.

use egui::{Color32, Id, Pos2, Ui};
use exo_ui::{DateField, DateFieldStyle, DateInput, DatePick, Family, Font, TextStyle};
use jiff::civil::{Date, Weekday};

/// The input's text: the UI face at 600 12px/16px, `top: 1px` (the dialog's light look doesn't
/// reach it).
pub const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 12.0, 16.0);
/// Its placeholder is at normal weight.
const PLACEHOLDER: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
/// `padding-bottom: 3px; border-bottom: 1px`, `2px` of each while focused.
const RULE_GAP: f32 = 3.0;
/// `min-width: 20px` (`field-sizing: content`).
const MIN_WIDTH: f32 = 20.0;
/// Classic's colours: the text `rgba(0,0,0,.9)`, `.4` disabled; the placeholder `.55`, `.2`
/// disabled; the rule `.1`, the focused one `#aeb1dd`, the wrong one `#f67b7b`.
const INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 230);
const INK_DISABLED: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 102);
const PLACEHOLDER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 140);
const PLACEHOLDER_DISABLED: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 51);
const RULE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 26);
const RULE_FOCUSED: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
const RULE_ERROR: Color32 = Color32::from_rgb(0xf6, 0x7b, 0x7b);

/// What happened in a date field this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateEvent {
    /// It was clicked or focused, so it has the keyboard and its popup opens.
    Focused,
    /// A day was picked in the popup.
    Picked(Date),
    /// What's typed changed. An empty field that may be empty clears the date.
    Cleared,
    /// A popup chevron was clicked, and the field keeps the keyboard.
    MonthChanged,
}

/// How a date field draws this frame.
pub struct Field<'a> {
    /// Its id, for the text edit and the popup.
    pub id: Id,
    /// Its state.
    pub input: &'a mut DateInput,
    /// It has the keyboard.
    pub focused: bool,
    /// The form is off (`disabled`).
    pub disabled: bool,
    /// `Select date`, for the last day.
    pub placeholder: &'a str,
    /// Days before this can't be picked.
    pub min: Date,
    /// Today, which the popup marks.
    pub today: Date,
}

impl Field<'_> {
    /// The input's width: its text's, or its placeholder's while empty
    /// (`field-sizing: content`).
    pub fn width(&self, ctx: &egui::Context) -> f32 {
        let width = if self.input.text.is_empty() {
            PLACEHOLDER.width(ctx, self.placeholder)
        } else {
            TEXT.width(ctx, &self.input.text)
        };
        width.max(MIN_WIDTH)
    }

    /// Draws the input with its line box's top left at `at`, and returns what happened.
    pub fn show(&mut self, ui: &mut Ui, at: Pos2) -> Option<DateEvent> {
        let style = DateFieldStyle {
            text: TEXT.without_ligatures(),
            placeholder: PLACEHOLDER,
            min_width: MIN_WIDTH,
            max_width: f32::INFINITY,
            ink: if self.disabled { INK_DISABLED } else { INK },
            placeholder_ink: if self.disabled {
                PLACEHOLDER_DISABLED
            } else {
                PLACEHOLDER_INK
            },
            selection: ui.visuals().selection.bg_fill,
            unfocused_rule: Some((RULE_GAP, 1.0, RULE)),
            focused_rule: (RULE_GAP - 1.0, 2.0, RULE_FOCUSED),
            error_color: RULE_ERROR,
            popup_anchor_below: RULE_GAP + 1.0,
        };
        let out = DateField {
            id: self.id,
            input: self.input,
            style,
            placeholder: self.placeholder,
            rule_focused: self.focused,
            show_popup: self.focused,
            disabled: self.disabled,
            extend_hit_to_rule: true,
            today: self.today,
            min: Some(self.min),
            week_start: Weekday::Sunday,
        }
        .show_with(ui, at, crate::spelling::text_field);

        let mut event = None;
        if !self.disabled {
            if out.clicked || (out.gained_focus && !self.focused) {
                event = Some(DateEvent::Focused);
            } else {
                super::dialog::hold_focus(ui, self.focused, self.id);
            }
            if out.text_changed {
                event = Some(DateEvent::Cleared);
            }
        }
        match out.pick {
            Some(DatePick::Picked(day)) => event = Some(DateEvent::Picked(day)),
            Some(DatePick::MonthChanged) => event = Some(DateEvent::MonthChanged),
            None => {}
        }
        event
    }
}
