//! A date field's model and popup: a text input showing the day as `Thu, Nov 26` that takes a typed
//! date, and the month it opens below itself while it has the keyboard. Each screen draws the input
//! in its own style (the Out of Office dialog's, the event form's When); the popup and the typed
//! dates are the same everywhere.

use egui::{Color32, CornerRadius, Id, Pos2, Rect, Sense, Vec2, pos2, vec2};
use jiff::{
    ToSpan,
    civil::{Date, Weekday},
};

use crate::{BoxShadow, Family, Font, IconButton, TextStyle, Theme};

/// The popup: 316×256, `padding: 16px 12px`, with 4px corners.
pub(crate) const DATE_PICKER_SIZE: Vec2 = vec2(316.0, 256.0);
/// The popup hangs 6px below the input's box and 24px left of it.
pub const DATE_PICKER_OFFSET: Vec2 = vec2(-24.0, 6.0);
const PADDING: Vec2 = vec2(12.0, 16.0);
const SHADOWS: [BoxShadow; 2] = [
    BoxShadow::new(
        [3.0, 0.0],
        16.0,
        0.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 28),
    ),
    BoxShadow::new(
        [0.0, 3.0],
        16.0,
        0.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 31),
    ),
];
/// The month's caption, padded `0 0 8px 16px`, in 600 12px/16px with `letter-spacing: .18px`.
const CAPTION: TextStyle =
    TextStyle::new(Font::new(Family::Sans, 600), 12.0, 16.0).letter_spacing(0.18);
const CAPTION_LEFT: f32 = 16.0;
const CAPTION_TOP: f32 = 1.0;
/// The chevrons: two 24px buttons 4px apart, ending 8px in from the right, 4px above the caption.
const NAV: f32 = 24.0;
const NAV_GAP: f32 = 4.0;
const NAV_RIGHT: f32 = 8.0;
const NAV_TOP: f32 = -4.0;
/// Seven columns across the month's 292px; the weekday row 25px down and 32px tall; weeks of
/// 28px; each day a 24px circle 2px down in its cell.
const COLUMNS: f32 = 292.0 / 7.0;
const WEEKDAYS_TOP: f32 = 25.0;
const WEEKDAY_HEIGHT: f32 = 31.0;
const WEEK: f32 = 28.0;
const DAY: f32 = 24.0;
/// 12px/16px, weekdays `padding-top: 8px` (the row's `top: 1px` is in its top), each day's
/// text 6px into its circle (measured).
const NUMBERS: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const WEEKDAY_TEXT_TOP: f32 = 8.0;
const DAY_TEXT_TOP: f32 = 6.0;
/// Days before the first allowed are at `.5`, the other months' at `.4`.
const DISABLED_OPACITY: f32 = 0.5;
const OUTSIDE_OPACITY: f32 = 0.4;

/// The popup's colours. It hangs outside whatever opened it, so it takes the app's theme.
struct PopupColors {
    background: Color32,
    text: Color32,
    chevrons: (Color32, Color32),
    today: Color32,
    selected: (Color32, Color32),
    hover: Color32,
}

impl PopupColors {
    fn of(theme: Theme) -> Self {
        match theme {
            Theme::Snow => Self {
                background: Color32::WHITE,
                text: Color32::from_rgba_premultiplied(0, 0, 0, 230),
                chevrons: (
                    Color32::from_black_alpha(77),
                    Color32::from_black_alpha(128),
                ),
                today: Color32::from_rgb(0xed, 0xed, 0xed),
                selected: (Color32::from_rgb(0xae, 0xb1, 0xdd), Color32::WHITE),
                hover: Color32::from_rgb(0xf2, 0xf4, 0xfa),
            },
            Theme::Carbon => Self {
                background: Color32::from_rgb(0x27, 0x29, 0x2d),
                text: Color32::from_white_alpha(191),
                chevrons: (
                    Color32::from_white_alpha(77),
                    Color32::from_white_alpha(128),
                ),
                today: Color32::from_rgb(0x5f, 0x61, 0x65),
                selected: (Color32::from_rgb(0xa0, 0xa4, 0xd9), Color32::BLACK),
                hover: Color32::from_rgb(0x46, 0x4b, 0x53),
            },
        }
    }
}

/// A date field's state.
#[derive(Debug, Clone, Default)]
pub struct DateInput {
    /// What the input shows: the date as formatted, or what's being typed.
    pub text: String,
    /// What's typed parses to this (the popup's selection), before it's committed.
    pub parsed: Option<Date>,
    /// What's typed isn't a date, or is before the first allowed.
    pub error: bool,
    /// The popup's month, when it's open.
    pub month: Option<Date>,
}

impl DateInput {
    /// The field showing `date` in [`format_date`]'s way, with its popup closed.
    pub fn showing(date: Option<Date>, today: Date) -> Self {
        Self {
            text: date
                .map(|date| format_date(date, today))
                .unwrap_or_default(),
            parsed: date,
            error: false,
            month: None,
        }
    }

    /// Opens the month popup on `self.parsed`, or `fallback` when nothing is parsed yet.
    pub fn open_month(&mut self, fallback: Date) {
        self.month = Some(self.parsed.unwrap_or(fallback).first_of_month());
    }

    /// Paints the tethered [`DatePicker`] popup while `self.month` is `Some`, updating the stored
    /// month when a navigation chevron is clicked.
    pub fn show_popup(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        anchor_bottom_left: Pos2,
        today: Date,
        min: Option<Date>,
        week_start: Weekday,
    ) -> Option<DatePick> {
        let mut month = self.month?;
        let pick = DatePicker {
            id,
            month: &mut month,
            selected: self.parsed,
            today,
            min,
            week_start,
        }
        .show(ctx, anchor_bottom_left + DATE_PICKER_OFFSET);
        if pick == Some(DatePick::MonthChanged) {
            self.month = Some(month);
        }
        pick
    }

    /// The input as typed changed to `text`. It parses against `reference` (the field's date, or
    /// today), and is wrong when it's not a date or is before `min`. Returns whether the field was
    /// emptied while `nullable`.
    pub fn typed(
        &mut self,
        reference: Date,
        today: Date,
        min: Option<Date>,
        nullable: bool,
    ) -> bool {
        if self.text.trim().is_empty() && nullable {
            self.error = false;
            self.parsed = None;
            return true;
        }
        match parse_date(&self.text, reference, today) {
            Some(date) if min.is_none_or(|min| date >= min) => {
                self.parsed = Some(date);
                self.error = false;
                self.month = Some(date.first_of_month());
            }
            Some(_) => self.error = true,
            None => {
                self.error = true;
                self.parsed = Some(reference);
            }
        }
        false
    }
}

/// `E, MMM d`, or `E, MMM d, yyyy` outside the current year.
pub fn format_date(date: Date, today: Date) -> String {
    let pattern = if date.year() == today.year() {
        "%a, %b %-d"
    } else {
        "%a, %b %-d, %Y"
    };
    date.strftime(pattern).to_string()
}

/// A typed date, with commas dropped: a weekday name (`EEEE`), the next one if that's past; then a
/// month and maybe a day (`M`, `M/d`, `MMMM`, `MMMM d`, `EEEE MMMM d`) in `reference`'s year, next
/// year if that's past; then those with a year, and `yyyy-MM-dd`. Names may be abbreviated, and a
/// weekday with a date moves it into that day's week.
pub fn parse_date(text: &str, reference: Date, today: Date) -> Option<Date> {
    let words: Vec<String> = text
        .replace(',', " ")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let past = |date: Date| reference >= today && date < today;
    // `EEEE`: that weekday in the reference's week (Sunday first).
    if let [day] = words[..]
        && let Some(weekday) = weekday(day)
    {
        let date = in_week(reference, weekday)?;
        return Some(if past(date) {
            date.checked_add(7.days()).ok()?
        } else {
            date
        });
    }
    if let Some(date) = month_day(&words, reference.year()) {
        return Some(if past(date) {
            date.checked_add(1.year()).ok()?
        } else {
            date
        });
    }
    with_year(&words)
}

/// `M`, `M/d`, `MMMM`, `MMMM d` or `EEEE MMMM d` in `year`.
fn month_day(words: &[&str], year: i16) -> Option<Date> {
    match words {
        [numeric] if numeric.starts_with(|c: char| c.is_ascii_digit()) => {
            let mut parts = numeric.split('/');
            let month = number(parts.next()?, 12)?;
            let day = parts.next().map_or(Some(1), |day| number(day, 31))?;
            parts.next().is_none().then_some(())?;
            Date::new(year, month, day).ok()
        }
        [name] => Date::new(year, month(name)?, 1).ok(),
        [name, day] => Date::new(year, month(name)?, number(day, 31)?).ok(),
        [weekday_name, name, day] => in_week(
            Date::new(year, month(name)?, number(day, 31)?).ok()?,
            weekday(weekday_name)?,
        ),
        _ => None,
    }
}

/// `M/d/yy`, `M/d/yyyy`, `MMMM d yy(yy)`, `EEEE MMMM d yy(yy)`, `yy-MM-dd`, `yyyy-MM-dd`.
fn with_year(words: &[&str]) -> Option<Date> {
    match words {
        [numeric] if numeric.contains('-') => {
            let [year, month, day] = numeric.split('-').collect::<Vec<_>>()[..] else {
                return None;
            };
            Date::new(full_year(year)?, number(month, 12)?, number(day, 31)?).ok()
        }
        [numeric] => {
            let [month, day, year] = numeric.split('/').collect::<Vec<_>>()[..] else {
                return None;
            };
            Date::new(full_year(year)?, number(month, 12)?, number(day, 31)?).ok()
        }
        [name, day, year] => Date::new(full_year(year)?, month(name)?, number(day, 31)?).ok(),
        [weekday_name, name, day, year] => in_week(
            Date::new(full_year(year)?, month(name)?, number(day, 31)?).ok()?,
            weekday(weekday_name)?,
        ),
        _ => None,
    }
}

/// `date` moved to `weekday` in its week (weeks starting on Sunday).
fn in_week(date: Date, weekday: Weekday) -> Option<Date> {
    let offset = i64::from(weekday.to_sunday_zero_offset())
        - i64::from(date.weekday().to_sunday_zero_offset());
    date.checked_add(offset.days()).ok()
}

/// A number from 1 to `max`.
fn number(text: &str, max: i8) -> Option<i8> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i8>()
        .ok()
        .filter(|value| (1..=max).contains(value))
}

/// `yy` (this century) or `yyyy`.
fn full_year(text: &str) -> Option<i16> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i16 = text.parse().ok()?;
    match text.len() {
        2 => Some(2000 + year),
        4 => Some(year),
        _ => None,
    }
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// A month's name, in full, abbreviated (`Nov`, `Sept`) or by its first letters.
fn month(name: &str) -> Option<i8> {
    let name = name.trim_end_matches('.');
    let found = MONTHS
        .iter()
        .position(|month| name.len() >= 3 && month.starts_with(name))?;
    i8::try_from(found + 1).ok()
}

/// A weekday's name: in full, or its first two or three letters (`Th`, `Thu`).
fn weekday(name: &str) -> Option<Weekday> {
    const DAYS: [(&str, Weekday); 7] = [
        ("sunday", Weekday::Sunday),
        ("monday", Weekday::Monday),
        ("tuesday", Weekday::Tuesday),
        ("wednesday", Weekday::Wednesday),
        ("thursday", Weekday::Thursday),
        ("friday", Weekday::Friday),
        ("saturday", Weekday::Saturday),
    ];
    let name = name.trim_end_matches('.');
    let mut matches = DAYS
        .iter()
        .filter(|(day, _)| name.len() >= 2 && day.starts_with(name));
    let (_, found) = matches.next()?;
    // `T` and `S` alone are ambiguous, but `th`/`tu` and `sa`/`su` aren't.
    matches.next().is_none().then_some(*found)
}

/// What happened in the popup this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePick {
    /// A day was picked.
    Picked(Date),
    /// A chevron was clicked, and the field keeps the keyboard.
    MonthChanged,
}

/// The popup showing a month, with its day picked by the pointer.
pub struct DatePicker<'a> {
    pub id: Id,
    /// The month shown, which the chevrons move.
    pub month: &'a mut Date,
    pub selected: Option<Date>,
    /// Marked as today.
    pub today: Date,
    /// Days before this can't be picked.
    pub min: Option<Date>,
    /// The first column's weekday.
    pub week_start: Weekday,
}

impl DatePicker<'_> {
    /// Paints the popup with its top left at `at`, over everything, and returns a pick or a
    /// month change.
    pub fn show(self, ctx: &egui::Context, at: Pos2) -> Option<DatePick> {
        let colors = PopupColors::of(crate::app_theme(ctx));
        let rect = crate::pixel_snap(Rect::from_min_size(at, DATE_PICKER_SIZE));
        egui::Area::new(self.id)
            .order(egui::Order::Tooltip)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.allocate_rect(rect, Sense::click());
                let painter = ui.painter();
                for shadow in SHADOWS {
                    shadow.paint(painter, rect, CornerRadius::same(4));
                }
                painter.rect_filled(rect, CornerRadius::same(4), colors.background);
                let origin = rect.min + PADDING;
                let first = self.month.first_of_month();
                CAPTION.paint_line(
                    painter,
                    origin + vec2(CAPTION_LEFT, CAPTION_TOP),
                    &first.strftime("%B %Y").to_string(),
                    colors.text,
                    None,
                );
                let mut event = None;
                for (step, icon) in [
                    (-1i8, "legacy/DEPRECATED_leftChevron"),
                    (1, "legacy/DEPRECATED_rightChevron"),
                ] {
                    let right = rect.right()
                        - PADDING.x
                        - NAV_RIGHT
                        - if step < 0 { NAV + NAV_GAP } else { 0.0 };
                    let hit = Rect::from_min_size(
                        pos2(right - NAV, origin.y + NAV_TOP),
                        Vec2::splat(NAV),
                    );
                    let response = IconButton::new(icon)
                        .colors(colors.chevrons.0, colors.chevrons.1)
                        .show(
                            ui,
                            self.id.with(("nav", step)),
                            hit,
                            crate::icon_rect(ui.ctx(), icon, hit),
                        );
                    if response.clicked() {
                        *self.month = first.checked_add(i64::from(step).months()).unwrap_or(first);
                        event = Some(DatePick::MonthChanged);
                    }
                }
                let painter = ui.painter();
                let first_column = self.week_start.to_sunday_zero_offset();
                for column in 0..7 {
                    let initial = &"SMTWTFS"[usize::from((first_column + column) as u8 % 7)..][..1];
                    let width = NUMBERS.width(ctx, initial);
                    let x = origin.x + f32::from(column) * COLUMNS + (COLUMNS - width) / 2.0;
                    NUMBERS.paint_line(
                        painter,
                        pos2(x, origin.y + WEEKDAYS_TOP + WEEKDAY_TEXT_TOP),
                        initial,
                        colors.text,
                        None,
                    );
                }
                let lead = i64::from(
                    (first.weekday().to_sunday_zero_offset() - first_column).rem_euclid(7),
                );
                for index in 0..42i64 {
                    let offset = index - lead;
                    let Ok(day) = (if offset < 0 {
                        first.checked_sub((-offset).days())
                    } else {
                        first.checked_add(offset.days())
                    }) else {
                        continue;
                    };
                    let (row, column) = (index / 7, index % 7);
                    let cell = Rect::from_min_size(
                        origin
                            + vec2(
                                column as f32 * COLUMNS,
                                WEEKDAYS_TOP + WEEKDAY_HEIGHT + row as f32 * WEEK,
                            ),
                        vec2(COLUMNS, WEEK),
                    );
                    let button = crate::pixel_snap(Rect::from_min_size(
                        pos2(cell.center().x - DAY / 2.0, cell.top() + 2.0),
                        Vec2::splat(DAY),
                    ));
                    let disabled = self.min.is_some_and(|min| day < min);
                    let response = ui.interact(
                        button,
                        self.id.with(("day", day)),
                        if disabled {
                            Sense::hover()
                        } else {
                            Sense::click()
                        },
                    );
                    let response = if disabled {
                        response
                    } else {
                        response.on_hover_cursor(egui::CursorIcon::PointingHand)
                    };
                    let opacity = if day.month() != first.month() {
                        OUTSIDE_OPACITY
                    } else if disabled {
                        DISABLED_OPACITY
                    } else {
                        1.0
                    };
                    let (fill, color) = if Some(day) == self.selected {
                        (Some(colors.selected.0), colors.selected.1)
                    } else if day == self.today {
                        (Some(colors.today), colors.text)
                    } else if response.hovered() && !disabled {
                        (Some(colors.hover), colors.text)
                    } else {
                        (None, colors.text)
                    };
                    let painter = ui.painter();
                    if let Some(fill) = fill {
                        painter.circle_filled(
                            button.center(),
                            DAY / 2.0,
                            fill.gamma_multiply(opacity),
                        );
                    }
                    let label = day.day().to_string();
                    let width = NUMBERS.width(ctx, &label);
                    NUMBERS.paint_line(
                        painter,
                        pos2(button.center().x - width / 2.0, button.top() + DAY_TEXT_TOP),
                        &label,
                        color.gamma_multiply(opacity),
                        None,
                    );
                    if response.clicked() {
                        event = Some(DatePick::Picked(day));
                    }
                }
                event
            })
            .inner
    }
}

/// Visual metrics and colors of a content-sized [`DateField`].
#[derive(Clone, Copy, Debug)]
pub struct DateFieldStyle {
    /// Text style for the date string.
    pub text: TextStyle,
    /// Text style for an empty field's placeholder.
    pub placeholder: TextStyle,
    /// Minimum width of the field.
    pub min_width: f32,
    /// Maximum width of the field (`f32::INFINITY` when unconstrained).
    pub max_width: f32,
    /// Foreground color of the date text.
    pub ink: Color32,
    /// Foreground color of the placeholder text.
    pub placeholder_ink: Color32,
    /// Selection highlight fill.
    pub selection: Color32,
    /// Unfocused rule `(gap_below_line, thickness, color)`, or `None` when unfocused fields show no
    /// rule.
    pub unfocused_rule: Option<(f32, f32, Color32)>,
    /// Focused rule `(gap_below_line, thickness, color)`.
    pub focused_rule: (f32, f32, Color32),
    /// Error rule color (`input.error`).
    pub error_color: Color32,
    /// Distance below `line.bottom()` to the bottom-left anchor of the [`DatePicker`] popup.
    pub popup_anchor_below: f32,
}

/// Outcome of drawing a [`DateField`] for one frame.
#[derive(Clone, Copy, Debug)]
pub struct DateFieldOutput {
    /// Measured width of the input this frame.
    pub width: f32,
    /// Whether the underlying `TextEdit` currently holds egui focus.
    pub has_focus: bool,
    /// Whether the `TextEdit` gained focus this frame.
    pub gained_focus: bool,
    /// Whether the `TextEdit` lost focus this frame.
    pub lost_focus: bool,
    /// Whether the input or its bottom padding/rule hit box was clicked.
    pub clicked: bool,
    /// Whether `input.text` was edited this frame.
    pub text_changed: bool,
    /// What happened in the open [`DatePicker`] popup, if any.
    pub pick: Option<DatePick>,
}

/// Shared content-sized date text input, bottom rule, and tethered [`DatePicker`] popup (used by
/// Out of Office and Calendar `when_edit`).
#[must_use]
pub struct DateField<'a> {
    pub id: Id,
    pub input: &'a mut DateInput,
    pub style: DateFieldStyle,
    pub placeholder: &'a str,
    pub rule_focused: bool,
    pub show_popup: bool,
    pub disabled: bool,
    pub extend_hit_to_rule: bool,
    pub today: Date,
    pub min: Option<Date>,
    pub week_start: Weekday,
}

impl DateField<'_> {
    /// Content-sized width of the field for its current text or placeholder.
    pub fn width(&self, ctx: &egui::Context) -> f32 {
        let raw = if self.input.text.is_empty() && !self.placeholder.is_empty() {
            self.style.placeholder.width(ctx, self.placeholder)
        } else {
            self.style.text.width(ctx, &self.input.text)
        };
        raw.clamp(self.style.min_width, self.style.max_width)
    }

    /// Paints the date field at `at` using [`crate::text_field`].
    pub fn show(self, ui: &mut egui::Ui, at: Pos2) -> DateFieldOutput {
        self.show_with(ui, at, crate::text_field)
    }

    /// Paints the date field at `at` using `paint_field` (for example, a spellchecked text field).
    pub fn show_with(
        self,
        ui: &mut egui::Ui,
        at: Pos2,
        paint_field: impl FnOnce(
            &mut egui::Ui,
            Id,
            &mut String,
            Rect,
            &TextStyle,
            Color32,
            Color32,
        ) -> egui::Response,
    ) -> DateFieldOutput {
        let width = self.width(ui.ctx());
        let line = Rect::from_min_size(at, vec2(width, self.style.text.line_height));
        if self.input.text.is_empty() && !self.placeholder.is_empty() {
            self.style.placeholder.paint_line(
                ui.painter(),
                line.min,
                self.placeholder,
                self.style.placeholder_ink,
                None,
            );
        }

        let mut has_focus = false;
        let mut gained_focus = false;
        let mut lost_focus = false;
        let mut clicked = false;
        let mut text_changed = false;

        if self.disabled {
            self.style.text.paint_line(
                ui.painter(),
                line.min,
                &self.input.text,
                self.style.ink,
                None,
            );
        } else {
            let before = self.input.text.clone();
            let response = paint_field(
                ui,
                self.id,
                &mut self.input.text,
                line,
                &self.style.text,
                self.style.ink,
                self.style.selection,
            );
            has_focus = response.has_focus();
            gained_focus = response.gained_focus();
            lost_focus = response.lost_focus();
            clicked = response.clicked();
            text_changed = self.input.text != before;

            if self.extend_hit_to_rule && self.style.popup_anchor_below > 0.0 {
                let hit = Rect::from_min_max(
                    pos2(line.left(), line.bottom()),
                    pos2(line.right(), line.bottom() + self.style.popup_anchor_below),
                );
                let pointer = ui
                    .interact(hit, self.id.with("hit"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::Text);
                clicked |= pointer.clicked();
            }
        }

        let rule_spec = if self.rule_focused {
            Some(self.style.focused_rule)
        } else if self.input.error {
            Some(self.style.unfocused_rule.unwrap_or(self.style.focused_rule))
        } else {
            self.style.unfocused_rule
        };
        if let Some((gap, thickness, base_color)) = rule_spec {
            let color = if self.input.error {
                self.style.error_color
            } else if self.rule_focused {
                self.style.focused_rule.2
            } else {
                base_color
            };
            let rule = Rect::from_min_size(
                pos2(line.left(), line.bottom() + gap),
                vec2(width, thickness),
            );
            ui.painter()
                .rect_filled(crate::pixel_snap(rule), 0.0, color);
        }

        let pick = if self.show_popup && !self.disabled {
            self.input.show_popup(
                ui.ctx(),
                self.id.with("popup"),
                pos2(line.left(), line.bottom() + self.style.popup_anchor_below),
                self.today,
                self.min,
                self.week_start,
            )
        } else {
            None
        };

        DateFieldOutput {
            width,
            has_focus,
            gained_focus,
            lost_focus,
            clicked,
            text_changed,
            pick,
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    const TODAY: Date = date(2026, 11, 17);

    #[test]
    fn dates_show_without_the_year_in_this_one() {
        assert_eq!(format_date(date(2026, 11, 26), TODAY), "Thu, Nov 26");
        assert_eq!(format_date(date(2027, 1, 4), TODAY), "Mon, Jan 4, 2027");
    }

    #[test]
    fn typed_dates_parse_in_the_reference_year_and_week() {
        let reference = date(2026, 11, 26);
        assert_eq!(
            parse_date("Thu, Nov 26", reference, TODAY),
            Some(date(2026, 11, 26)),
            "what the field shows"
        );
        assert_eq!(
            parse_date("11/30", reference, TODAY),
            Some(date(2026, 11, 30))
        );
        assert_eq!(
            parse_date("dec 3", reference, TODAY),
            Some(date(2026, 12, 3))
        );
        assert_eq!(
            parse_date("January 5", reference, TODAY),
            Some(date(2027, 1, 5)),
            "past, so next year"
        );
        assert_eq!(
            parse_date("friday", reference, TODAY),
            Some(date(2026, 11, 27)),
            "in the reference's week"
        );
        assert_eq!(
            parse_date("mon", date(2026, 11, 17), TODAY),
            Some(date(2026, 11, 23)),
            "past, so next week"
        );
        assert_eq!(
            parse_date("1/5/27", reference, TODAY),
            Some(date(2027, 1, 5))
        );
        assert_eq!(
            parse_date("2027-02-01", reference, TODAY),
            Some(date(2027, 2, 1))
        );
        assert_eq!(
            parse_date("Fri Nov 26", reference, TODAY),
            Some(date(2026, 11, 27)),
            "the weekday wins, in its week"
        );
        assert_eq!(parse_date("13/1", reference, TODAY), None);
        assert_eq!(parse_date("soon", reference, TODAY), None);
        assert_eq!(
            parse_date("t", reference, TODAY),
            None,
            "Tuesday or Thursday"
        );
    }

    #[test]
    fn a_day_before_the_first_allowed_is_wrong() {
        // A past day without a year is next year's, and a past day with one is wrong.
        let mut input = DateInput {
            text: "11/10/2026".into(),
            ..DateInput::default()
        };
        assert!(!input.typed(date(2026, 11, 26), TODAY, Some(TODAY), false));
        assert!(input.error);
        input.text = "Nov 20".into();
        input.typed(date(2026, 11, 26), TODAY, Some(TODAY), false);
        assert_eq!(
            (input.error, input.parsed),
            (false, Some(date(2026, 11, 20)))
        );
        input.text.clear();
        assert!(
            input.typed(date(2026, 11, 26), TODAY, Some(TODAY), true),
            "a last day can be emptied"
        );
    }

    #[test]
    fn signed_years_and_numbers_are_rejected() {
        let reference = date(2026, 11, 26);
        for text in [
            "1/5/-100",
            "1/5/-5",
            "1/5/+5",
            "1/5/+100",
            "Nov 5 -100",
            "Nov 5 -5",
            "Nov 5 +5",
            "-100-01-05",
            "+2026-01-05",
            "+1/+5/2026",
        ] {
            assert_eq!(parse_date(text, reference, TODAY), None, "{text}");
        }
    }

    #[test]
    fn extreme_year_boundaries_do_not_overflow_picker_or_weekday_parse() {
        let max_day = date(9999, 12, 15);
        assert_eq!(
            parse_date("9999-12-15", max_day, TODAY),
            Some(max_day),
            "9999-12-15 parses cleanly"
        );
        // 9999-12-31 is a Friday; Saturday in its week and a past weekday shifted +7d overflow.
        let last_day = date(9999, 12, 31);
        assert_eq!(parse_date("saturday", last_day, last_day), None);
        assert_eq!(parse_date("sunday", last_day, last_day), None);
        assert_eq!(parse_date("friday", last_day, last_day), Some(last_day));

        let ctx = egui::Context::default();
        crate::install(&ctx).expect("system fonts");
        for extreme in [max_day, date(-9999, 1, 15)] {
            let mut month = extreme.first_of_month();
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let pick = DatePicker {
                    id: Id::new(("extreme-picker", extreme.year())),
                    month: &mut month,
                    selected: Some(extreme),
                    today: extreme,
                    min: None,
                    week_start: Weekday::Sunday,
                }
                .show(ui.ctx(), pos2(20.0, 20.0));
                assert_eq!(pick, None);
            })
            .drop_without_applying_deltas();
        }
    }

    #[test]
    fn extend_hit_to_rule_leaves_text_edit_interactive_and_clicks_below_line() {
        let ctx = egui::Context::default();
        crate::install(&ctx).expect("system fonts");
        let id = Id::new("date-field-hit-test");
        let at = pos2(40.0, 40.0);
        let style = DateFieldStyle {
            text: TextStyle::new(Font::new(Family::Sans, 400), 13.0, 20.0),
            placeholder: TextStyle::new(Font::new(Family::Sans, 400), 13.0, 20.0),
            min_width: 100.0,
            max_width: 200.0,
            ink: Color32::BLACK,
            placeholder_ink: Color32::GRAY,
            selection: Color32::LIGHT_BLUE,
            unfocused_rule: Some((8.0, 1.0, Color32::GRAY)),
            focused_rule: (7.0, 2.0, Color32::BLUE),
            error_color: Color32::RED,
            popup_anchor_below: 9.0,
        };
        let mut input = DateInput::showing(Some(date(2026, 11, 26)), TODAY);
        let run = |input: &mut DateInput, events: Vec<egui::Event>| -> DateFieldOutput {
            let mut out = None;
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    out = Some(
                        DateField {
                            id,
                            input,
                            style,
                            placeholder: "",
                            rule_focused: false,
                            show_popup: false,
                            disabled: false,
                            extend_hit_to_rule: true,
                            today: TODAY,
                            min: None,
                            week_start: Weekday::Sunday,
                        }
                        .show(ui, at),
                    );
                },
            )
            .drop_without_applying_deltas();
            out.expect("field ran")
        };

        // Prime layout.
        let _ = run(&mut input, vec![]);

        // Click inside `line` (y = 50): must reach `TextEdit` (focus + click), not be swallowed.
        let on_line = pos2(at.x + 25.0, at.y + 10.0);
        let _ = run(
            &mut input,
            vec![
                egui::Event::PointerMoved(on_line),
                egui::Event::PointerButton {
                    pos: on_line,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let out = run(
            &mut input,
            vec![egui::Event::PointerButton {
                pos: on_line,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(out.has_focus, "clicking inside line focuses TextEdit");
        assert!(out.clicked, "clicking inside line reports clicked");

        // Right-click inside `line`: must reach `text_field` and open the editable field menu.
        let _ = run(
            &mut input,
            vec![egui::Event::PointerButton {
                pos: on_line,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let _ = run(
            &mut input,
            vec![egui::Event::PointerButton {
                pos: on_line,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(
            crate::take_field_menu(&ctx, Some(id)).is_some(),
            "right-click inside line reaches TextEdit"
        );

        // Click in the strip below `line` (y = 64, between line.bottom=60 and rule=69).
        let below_line = pos2(at.x + 25.0, at.y + style.text.line_height + 4.0);
        let _ = run(
            &mut input,
            vec![
                egui::Event::PointerMoved(below_line),
                egui::Event::PointerButton {
                    pos: below_line,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let out = run(
            &mut input,
            vec![egui::Event::PointerButton {
                pos: below_line,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(out.clicked, "clicking strip below line reports clicked");
    }
}
