//! Floating dropdowns, suggestion lists, and the calendar picker overlay.

use egui::{Color32, CornerRadius, Rect, Ui, Vec2, pos2, vec2};
use exo_ui::{
    AnchoredCard, AnchoredHover, AnchoredList, ContactRow, ContactSelectorList, Menu,
    StackedContactStyle, paint_matched,
};

use super::{CalendarOption, EventForm, Field, Meeting, SMALL, Shown, guests::Suggestion};
use crate::calendar::{
    places::Place,
    repeat::{self, Custom},
};

/// Who's suggestions sit 12px under the input's box. In the week's modal they start 14px left of
/// the input and are at most 420px wide with 12px of padding. In the right pane they span the form
/// less 10px each side. The 52px items (name over address, 12px/16px nudged `top: 1px`, between
/// 10px paddings) start 3px down (and are nudged `top: 1px`), and the list ends 6px under the last.
const SUGGESTION: f32 = 10.0 + 16.0 + 16.0 + 10.0;
const SUGGESTIONS_BELOW: f32 = 20.0 + 12.0;
const SUGGESTIONS_TOP: f32 = 3.0 + 1.0;
const SUGGESTIONS_BOTTOM: f32 = 6.0;
/// The calendar's list is 276px wide, at most 40vh tall before it scrolls, and 12px under the
/// input's box. In the right pane it starts 10px inside the form, and in the week's modal it's
/// tethered under the input. Its rows are 12px/16px between 10px paddings (two lines and more for a
/// name that wraps, `overflow-wrap: anywhere`), laid out from 3px down to 6px under the last and
/// nudged `top: 1px`. A calendar's 9px swatch (2px corners, 2px down) sits at the row's right, 16px
/// clear of the name.
const CALENDAR_LIST_WIDTH: f32 = 276.0;
const CALENDAR_LIST_MAX_HEIGHT: f32 = 0.4;
const CALENDAR_ROW_PADDING: f32 = 10.0;
const CALENDAR_SWATCH: f32 = 9.0;
const CALENDAR_SWATCH_GAP: f32 = 16.0;

/// A dropdown open under a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dropdown {
    Meeting,
    Repeat,
}

/// Something drawn over the form.
pub(super) enum Overlay {
    Suggestions {
        below: Rect,
        items: Vec<Suggestion>,
        form: Rect,
        pane: bool,
    },
    /// Where's places, under its input in the `form`.
    Places {
        below: Rect,
        items: Vec<Place>,
        form: Rect,
        pane: bool,
    },
    Menu {
        below: Rect,
        trigger_hovered: bool,
        just_opened: bool,
        which: Dropdown,
        items: Vec<String>,
        highlighted: usize,
    },
    /// When's suggestions, under its input in the `form`.
    When { below: Rect, form: Rect },
    /// The calendar's list, under its input in the `form`.
    Calendars { below: Rect, form: Rect, pane: bool },
}

/// A row of the calendar's list.
#[derive(Clone, Copy)]
pub(super) enum CalendarRow<'a> {
    /// An account's address over its calendars, while more than one account's
    /// are listed.
    Header(&'a str),
    Calendar(&'a CalendarOption),
}

impl EventForm {
    /// Scrolls the calendar list to show its highlighted row, to the top for the first row and
    /// otherwise as little as it takes.
    pub(super) fn calendar_keep_in_view(&mut self, rows: &[CalendarRow<'_>], ctx: &egui::Context) {
        let (top, bottom) = calendar_row_span(ctx, rows, self.calendar_selected);
        let (_, viewport) = calendar_list_heights(ctx, rows);
        if self.calendar_selected == 0 {
            self.calendar_scroll = 0.0;
        } else if top < self.calendar_scroll {
            self.calendar_scroll = top;
        } else if bottom > self.calendar_scroll + viewport {
            self.calendar_scroll = bottom - viewport;
        }
    }

    /// Paints a dropdown or the suggestions over everything, and takes their picks.
    pub(super) fn paint_overlay(&mut self, ui: &Ui, overlay: Overlay, shown: &Shown<'_>) {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        let keys = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
            )
        });
        match overlay {
            Overlay::Suggestions {
                below,
                items,
                form,
                pane,
            } => {
                let (left, right) = if pane {
                    (form.left() + 10.0, form.right() - 10.0)
                } else {
                    (
                        below.left() - 14.0,
                        (below.left() - 14.0 + 420.0).min(form.right() - 10.0),
                    )
                };
                let top = below.top() + SUGGESTIONS_BELOW;
                let padding = if pane {
                    2.0 * exo_ui::rem(ctx.content_rect().width(), false) - 10.0
                } else {
                    12.0
                };
                let (background, selected) = suggestion_fills(palette, false);
                let (rest, matched) = (palette.ink(0.5, 0.5), palette.ink(0.9, 0.9));
                let formatted: Vec<(String, String, bool)> = items
                    .iter()
                    .map(|item| match item {
                        Suggestion::Contact(contact) => (
                            contact
                                .name
                                .clone()
                                .unwrap_or_else(|| contact.email.clone()),
                            contact.email.clone(),
                            false,
                        ),
                        Suggestion::Everybody(people) => (
                            "Everybody in this conversation".to_owned(),
                            people
                                .iter()
                                .map(|person| person.email.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            true,
                        ),
                    })
                    .collect();
                let all_matched = formatted.iter().position(|(_, _, all)| *all);
                let rows: Vec<ContactRow<'_>> = formatted
                    .iter()
                    .map(|(name, email, _)| ContactRow {
                        name,
                        email,
                        disabled: false,
                        avatar: None,
                    })
                    .collect();
                let style = StackedContactStyle {
                    card: AnchoredCard::elevated(background, selected),
                    row: SUGGESTION,
                    pad_top: SUGGESTIONS_TOP,
                    pad_bottom: SUGGESTIONS_BOTTOM - 1.0,
                    pad_x: padding,
                    inset_x: 0.0,
                    name_style: SMALL,
                    name_top: 11.0,
                    name_inks: (rest, matched),
                    email_style: SMALL,
                    email_top: 27.0,
                    email_inks: (rest, matched),
                    match_query: true,
                    hover_mode: AnchoredHover::Always,
                };
                let out = ContactSelectorList::stacked(
                    &rows,
                    &self.guest_input,
                    Some(self.suggestion),
                    style,
                )
                .all_matched_row(all_matched)
                .show(
                    &ctx,
                    exo_ui::id(&ctx, "calendar-form-suggestions"),
                    pos2(left, top),
                    right - left,
                    egui::Order::Tooltip,
                );
                if let Some(hovered) = out.hovered {
                    self.suggestion = hovered;
                }
                if let Some(pressed) = out.pressed {
                    self.values.guests.extend(items[pressed].guests());
                    self.guest_input.clear();
                    self.focus = Some(Field::Guests);
                }
            }
            Overlay::Places {
                below,
                items,
                form,
                pane,
            } => {
                // In the week's modal, Where's list is 420px wide and 4px under the input's box,
                // from its left. In the right pane it's placed as Who's.
                let (left, right, top) = if pane {
                    (
                        form.left() + 10.0,
                        form.right() - 10.0,
                        below.top() + SUGGESTIONS_BELOW,
                    )
                } else {
                    (below.left(), below.left() + 420.0, below.top() + 20.0 + 4.0)
                };
                let list = Rect::from_min_max(
                    pos2(left, top),
                    pos2(
                        right,
                        top + SUGGESTIONS_TOP - 1.0
                            + items.len() as f32 * SUGGESTION
                            + SUGGESTIONS_BOTTOM,
                    ),
                );
                let padding = 2.0 * exo_ui::rem(ctx.content_rect().width(), false) - 10.0;
                let (background, selected) = suggestion_fills(palette, !pane);
                let (dim, matched, address) = (
                    palette.ink(0.35, 0.35),
                    palette.ink(0.9, 0.9),
                    palette.ink(0.5, 0.5),
                );
                let out = AnchoredList::new(
                    exo_ui::id(&ctx, "calendar-form-places"),
                    list,
                    AnchoredCard::elevated(background, selected),
                )
                .padding(SUGGESTIONS_TOP, SUGGESTIONS_BOTTOM - 1.0)
                .highlighted(self.place)
                .hover_mode(AnchoredHover::Always)
                .show_uniform(
                    &ctx,
                    items.len(),
                    SUGGESTION,
                    |painter, row, index| {
                        let place = &items[index];
                        let room = row.width() - 2.0 * padding;
                        paint_matched(
                            painter,
                            pos2(row.left() + padding, row.top() + 11.0),
                            &place.name,
                            &self.values.location,
                            &SMALL,
                            dim,
                            matched,
                            room,
                        );
                        SMALL.paint_line(
                            painter,
                            pos2(row.left() + padding, row.top() + 27.0),
                            &place.address,
                            address,
                            Some(room),
                        );
                    },
                );
                if let Some(hovered) = out.hovered {
                    self.place = Some(hovered);
                }
                if let Some(pressed) = out.pressed {
                    self.pick_place(&items[pressed]);
                }
            }
            Overlay::Menu {
                below,
                trigger_hovered,
                just_opened,
                which,
                items,
                highlighted,
            } => {
                let labels: Vec<&str> = items.iter().map(String::as_str).collect();
                let menu = Menu {
                    items: &labels,
                    highlighted,
                };
                let id = exo_ui::id(&ctx, ("calendar-form-menu", which as u8));
                // Repeat's list is padded `4px 0`, 18px left of the trigger and 8px under it.
                let menu = if which == Dropdown::Repeat {
                    menu.show_compact(
                        ui,
                        id,
                        pos2(below.left() - 18.0, below.bottom() + 11.0),
                        16.0,
                        0.0,
                    )
                } else {
                    menu.show(ui, id, pos2(below.left() - 8.0, below.bottom() + 6.0))
                };
                let mut highlighted = menu.hovered.unwrap_or(highlighted);
                if keys.0 {
                    highlighted = highlighted.saturating_sub(1);
                } else if keys.1 {
                    highlighted = (highlighted + 1).min(labels.len().saturating_sub(1));
                }
                self.dropdown = Some((which, highlighted));
                let picked = menu.clicked.or((!just_opened
                    && keys.2
                    && self.focused.is_some_and(|field| field != Field::Title))
                .then_some(highlighted));
                if let Some(picked) = picked {
                    self.dropdown = None;
                    match which {
                        Dropdown::Meeting => self.values.meeting = Meeting::NewMeet,
                        Dropdown::Repeat => {
                            let anchor = self.anchor(shown.now);
                            match repeat::presets(anchor)
                                .get(picked)
                                .map(|(preset, _)| *preset)
                            {
                                Some(repeat::Preset::Custom) => {
                                    self.custom =
                                        Some(Custom::new(anchor, self.values.repeat.as_ref()))
                                }
                                Some(preset) => self.values.repeat = preset.rule(),
                                None => {}
                            }
                        }
                    }
                } else if menu.outside_released(ui, trigger_hovered) {
                    self.dropdown = None;
                }
            }
            Overlay::When { below, form } => {
                if let Some(index) = self.when.paint_list(ui, below, form, !self.week, shown.now) {
                    self.commit_when(index, shown.now);
                }
            }
            Overlay::Calendars { below, form, pane } => {
                self.paint_calendars(&ctx, below, form, pane, shown.calendars)
            }
        }
    }

    /// The calendar's list under its input `below`: hovering a calendar highlights it, pressing one
    /// takes it, the wheel scrolls the list.
    fn paint_calendars(
        &mut self,
        ctx: &egui::Context,
        below: Rect,
        form: Rect,
        pane: bool,
        calendars: &[CalendarOption],
    ) {
        let rows = calendar_rows(calendars, &self.calendar_input);
        if rows.is_empty() {
            return;
        }
        let palette = exo_ui::palette(ctx);
        let lines = calendar_row_lines(ctx, &rows);
        let (content, height) = calendar_list_heights(ctx, &rows);
        let window = ctx.content_rect();
        // 12px under the input's 20px box. In the modal it turns over to 12px above the input
        // when it would leave the window, and it stays inside the window.
        let mut top = below.top() + SUGGESTIONS_BELOW;
        let left = if pane {
            form.left() + 10.0
        } else {
            below.left()
        };
        if !pane {
            let above = below.top() - 12.0 - height;
            if top + height > window.bottom() && above >= window.top() {
                top = above;
            }
            top = top.min(window.bottom() - height).max(window.top());
        }
        let list = Rect::from_min_size(pos2(left, top), vec2(CALENDAR_LIST_WIDTH, height));
        self.calendar_scroll = self.calendar_scroll.clamp(0.0, content - height);
        let (background, selected) = suggestion_fills(palette, !pane);
        let (ink, header_ink) = (palette.ink(0.9, 0.9), palette.ink(0.35, 0.35));
        let padding = calendar_list_padding(ctx);
        let highlight = calendar_highlight(&rows, self.calendar_selected);
        let heights: Vec<f32> = lines
            .iter()
            .map(|row_lines| {
                2.0 * CALENDAR_ROW_PADDING + row_lines.len() as f32 * SMALL.line_height
            })
            .collect();
        let out = AnchoredList::new(
            exo_ui::id(ctx, "calendar-form-calendars"),
            list,
            AnchoredCard::elevated(background, selected),
        )
        .padding(SUGGESTIONS_TOP, SUGGESTIONS_BOTTOM - 1.0)
        .highlighted(Some(highlight))
        .scroll(&mut self.calendar_scroll)
        .scrollbar(
            exo_ui::id(ctx, "calendar-form-calendars-scrollbar"),
            &mut self.calendar_scrollbar,
        )
        .show_variable(
            ctx,
            &heights,
            |index| matches!(rows[index], CalendarRow::Calendar(_)),
            |painter, rect, index| {
                let color = match rows[index] {
                    CalendarRow::Header(_) => header_ink,
                    CalendarRow::Calendar(calendar) => {
                        let swatch = Rect::from_min_size(
                            pos2(
                                rect.right() - padding - CALENDAR_SWATCH,
                                rect.top() + CALENDAR_ROW_PADDING + 2.0,
                            ),
                            Vec2::splat(CALENDAR_SWATCH),
                        );
                        painter.rect_filled(swatch, CornerRadius::same(2), calendar.color.base());
                        ink
                    }
                };
                for (line, text) in lines[index].iter().enumerate() {
                    SMALL.paint_line(
                        painter,
                        pos2(
                            rect.left() + padding,
                            rect.top() + CALENDAR_ROW_PADDING + line as f32 * SMALL.line_height,
                        ),
                        text,
                        color,
                        None,
                    );
                }
            },
        );
        if let Some(index) = out.hovered.filter(|index| *index != highlight) {
            self.calendar_selected = index;
            self.calendar_keep_in_view(&rows, ctx);
        }
        if let Some(CalendarRow::Calendar(calendar)) = out.pressed.and_then(|index| rows.get(index))
        {
            self.pick_calendar(calendar.id.clone());
        }
    }
}

/// The calendar's list for what's `typed`. With nothing typed, it's every calendar as `calendars`
/// lists them (the mail account's first), and otherwise those whose name matches (`command-score`
/// above 0), best first. Each account's calendars follow its address, in the order the accounts
/// first come, once there's more than one account.
pub(super) fn calendar_rows<'a>(
    calendars: &'a [CalendarOption],
    typed: &str,
) -> Vec<CalendarRow<'a>> {
    let mut matching: Vec<(f64, &CalendarOption)> = calendars
        .iter()
        .map(|calendar| {
            (
                if typed.is_empty() {
                    1.0
                } else {
                    exo_commands::command_score(&calendar.name, typed)
                },
                calendar,
            )
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
    matching.sort_by(|(a, _), (b, _)| b.total_cmp(a));
    let mut accounts: Vec<(&str, Vec<&CalendarOption>)> = Vec::new();
    for (_, calendar) in matching {
        match accounts
            .iter_mut()
            .find(|(account, _)| *account == calendar.account)
        {
            Some((_, listed)) => listed.push(calendar),
            None => accounts.push((&calendar.account, vec![calendar])),
        }
    }
    let headed = accounts.len() > 1;
    accounts
        .into_iter()
        .flat_map(|(account, listed)| {
            headed
                .then_some(CalendarRow::Header(account))
                .into_iter()
                .chain(listed.into_iter().map(CalendarRow::Calendar))
        })
        .collect()
}

/// The highlighted row: `row` when it's a calendar, else the first calendar.
pub(super) fn calendar_highlight(rows: &[CalendarRow<'_>], row: usize) -> usize {
    if matches!(rows.get(row), Some(CalendarRow::Calendar(_))) {
        row
    } else {
        rows.iter()
            .position(|row| matches!(row, CalendarRow::Calendar(_)))
            .unwrap_or(0)
    }
}

/// The calendar ↑ (`up`) or ↓ takes the highlight to from `row`, round the ends, past the
/// headers.
pub(super) fn calendar_step(rows: &[CalendarRow<'_>], row: usize, up: bool) -> usize {
    let count = rows.len();
    (1..=count)
        .map(|step| {
            if up {
                (row + count - step) % count
            } else {
                (row + step) % count
            }
        })
        .find(|at| *at == row || matches!(rows[*at], CalendarRow::Calendar(_)))
        .unwrap_or(row)
}

/// The field's text without the keyboard: the calendar's name, then its account's address once
/// calendars of more than one account are listed, unless it's named as its account
/// (`{name} · {account}`).
pub(super) fn calendar_label(calendars: &[CalendarOption], id: &str) -> String {
    let grouped = calendars
        .iter()
        .any(|calendar| calendar.account != calendars[0].account);
    match calendars.iter().find(|calendar| calendar.id == id) {
        Some(calendar) if grouped && calendar.name != calendar.account => {
            format!("{} · {}", calendar.name, calendar.account)
        }
        Some(calendar) => calendar.name.clone(),
        None => id.to_owned(),
    }
}

/// The side padding of the list's rows, `calc(2rem - 10px)`.
fn calendar_list_padding(ctx: &egui::Context) -> f32 {
    2.0 * exo_ui::rem(ctx.content_rect().width(), false) - 10.0
}

/// Each row's lines: a header's address, a calendar's name wrapped in the room the swatch leaves.
fn calendar_row_lines(ctx: &egui::Context, rows: &[CalendarRow<'_>]) -> Vec<Vec<String>> {
    let room = CALENDAR_LIST_WIDTH
        - 2.0 * calendar_list_padding(ctx)
        - CALENDAR_SWATCH_GAP
        - CALENDAR_SWATCH;
    rows.iter()
        .map(|row| match row {
            CalendarRow::Header(account) => vec![(*account).to_owned()],
            CalendarRow::Calendar(calendar) => SMALL.wrap(ctx, &calendar.name, room),
        })
        .collect()
}

/// Each row's height.
fn calendar_row_heights(ctx: &egui::Context, rows: &[CalendarRow<'_>]) -> Vec<f32> {
    calendar_row_lines(ctx, rows)
        .iter()
        .map(|lines| 2.0 * CALENDAR_ROW_PADDING + lines.len() as f32 * SMALL.line_height)
        .collect()
}

/// Where row `row` is drawn in the list's content: under the 3px margin, nudged 1px down.
fn calendar_row_span(ctx: &egui::Context, rows: &[CalendarRow<'_>], row: usize) -> (f32, f32) {
    let heights = calendar_row_heights(ctx, rows);
    let top = SUGGESTIONS_TOP + heights.iter().take(row).sum::<f32>();
    (top, top + heights.get(row).copied().unwrap_or(0.0))
}

/// The list's content height, and the height it shows (`max-height: 40vh`).
fn calendar_list_heights(ctx: &egui::Context, rows: &[CalendarRow<'_>]) -> (f32, f32) {
    let content = SUGGESTIONS_TOP - 1.0
        + calendar_row_heights(ctx, rows).iter().sum::<f32>()
        + SUGGESTIONS_BOTTOM;
    (
        content,
        content.min(CALENDAR_LIST_MAX_HEIGHT * ctx.content_rect().height()),
    )
}

/// The suggestion lists' fills and their highlighted row's, which are Classic's literals. A list
/// `tethered` under its input in the week's modal is outside the form, on its own Carbon
/// `#2f3237`.
fn suggestion_fills(palette: &exo_ui::Palette, tethered: bool) -> (Color32, Color32) {
    match palette.theme {
        exo_ui::Theme::Snow => (Color32::WHITE, Color32::from_rgb(0xf2, 0xf4, 0xfa)),
        exo_ui::Theme::Carbon if tethered => (
            Color32::from_rgb(0x2f, 0x32, 0x37),
            Color32::from_rgb(0x46, 0x4b, 0x53),
        ),
        exo_ui::Theme::Carbon => (
            Color32::from_rgb(0x27, 0x29, 0x2d),
            Color32::from_rgb(0x46, 0x4b, 0x53),
        ),
    }
}
