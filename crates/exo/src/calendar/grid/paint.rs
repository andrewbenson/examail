use egui::{Color32, Painter, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use exo_ui::{Family, Font, TextStyle};
use jiff::{SignedDuration, Zoned};

use super::{
    DAY_HEIGHT, DRAFT, FREE_SLOT, Frame, Grid, GridAction, GridMode, GridView, SLOT, Variant,
    ZONE_GAP, ZONE_LABEL,
    drag::{RESIZE_EDGE, jiggle},
    hour_label, shown,
};
use crate::calendar::{
    block::{self, Look},
    event::Event,
    layout::{self, HOUR_HEIGHT},
};

/// The hour labels: 12px/12px, right-aligned, `top: 7px`.
const AXIS_TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 12.0);
const AXIS_TEXT_TOP: f32 = 7.0;
/// The time axis's rows: `padding-right: 4px; border-right: 1px`.
const AXIS_PADDING_RIGHT: f32 = 4.0;
/// The time zones' header: `padding: 0 5px 12px 12px`.
const TIMEZONE_PADDING_RIGHT: f32 = 5.0;
/// A day's events: `width: calc(100% - 4px)`.
const EVENTS_INSET: f32 = 4.0;
/// All-day rows: `padding-top: 5px`, blocks 23px tall with a 1px margin.
const ALL_DAY_PADDING_TOP: f32 = 5.0;
const ALL_DAY_BLOCK: f32 = 23.0;
const ALL_DAY_ROW: f32 = 24.0;
/// The now line is 2px tall and red. In the week view it has an 8px bulb at its left end
/// (`top: -3px; left: -4.5px`).
const NOW_LINE: Color32 = Color32::from_rgb(0xff, 0x56, 0x5e);
const NOW_BULB: f32 = 8.0;
/// A weekday header's title: 60px wide, 22px tall, `padding-top: 28px`, 14px/22px nudged
/// `padding-top: 1px`.
const DAY_TITLE_TOP: f32 = 28.0;
const DAY_TITLE: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 14.0, 22.0);
const DAY_TITLE_WIDTH: f32 = 60.0;
/// The weekday headers' names, Sunday first (`Thur`, not `Thu`).
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thur", "Fri", "Sat"];
/// The time zone labels: `top: 3px` outside the right pane, above `padding-bottom: 12px`.
const TIMEZONE_TEXT_TOP: f32 = 3.0;
const TIMEZONE_PADDING_BOTTOM: f32 = 12.0;
/// The add-event line: a 3px border tiled with a pill `rgb(191,191,191)` 3×1.5px every 4.5px.
const INDICATOR: Color32 = Color32::from_rgb(191, 191, 191);

/// A cell of an all-day row: an event spanning `days` columns, or an empty column.
pub(super) struct Cell<'a> {
    pub(super) event: Option<&'a Event>,
    pub(super) days: usize,
}

/// How a block in a guests' column draws.
#[derive(Clone, Copy, Debug)]
struct InColumn {
    /// Its column is collapsed to bars, without text.
    collapsed: bool,
    /// It shows its time.
    time: bool,
    /// A guest's, so a click opens its details and it doesn't move.
    guest: bool,
}

/// The all-day events in rows: each event under its first day, spanning the days it lasts,
/// longest first, and each row filled left to right with what fits.
pub(super) fn all_day_rows<'a>(grid: &Grid<'a>, extra: Option<&'a Event>) -> Vec<Vec<Cell<'a>>> {
    let count = grid.days.len();
    let mut by_day: Vec<Vec<&Event>> = vec![Vec::new(); count];
    for event in shown(grid, extra)
        .into_iter()
        .filter(|event| event.in_all_day_row())
    {
        if let Some(first) = grid.days.iter().position(|day| event.on(*day)) {
            by_day[first].push(event);
        }
    }
    for events in &mut by_day {
        events.sort_by_key(|event| {
            std::cmp::Reverse(
                event
                    .end
                    .timestamp()
                    .duration_since(event.start.timestamp()),
            )
        });
    }
    let total: usize = by_day.iter().map(Vec::len).sum();
    let mut rows = vec![Vec::new()];
    let (mut placed, mut day) = (0, 0);
    while placed < total {
        if day >= count {
            day = 0;
            rows.push(Vec::new());
            continue;
        }
        let row = rows.last_mut().expect("a row");
        if by_day[day].is_empty() {
            row.push(Cell {
                event: None,
                days: 1,
            });
            day += 1;
            continue;
        }
        let event = by_day[day].remove(0);
        // The days from here it covers, at least one.
        let days = grid.days[day..]
            .iter()
            .take_while(|next| {
                next.to_zoned(grid.tz.clone())
                    .is_ok_and(|midnight| midnight < event.end)
            })
            .count()
            .max(1);
        row.push(Cell {
            event: Some(event),
            days,
        });
        day += days;
        placed += 1;
    }
    let row = rows.last_mut().expect("a row");
    row.extend((day..count).map(|_| Cell {
        event: None,
        days: 1,
    }));
    rows
}

/// The weekday headers: `Tue 17` centred over each column, today's on a fill. Classic sets them
/// all semibold, at `.6` (Carbon `.5`), today's at `.8` on `rgba(0,0,0,.05)` (Carbon white) with
/// 2px corners.
pub(super) fn paint_day_headers(painter: &Painter, frame: &Frame, grid: &Grid<'_>) {
    let palette = exo_ui::palette(painter.ctx());
    let width = frame.columns.width() / grid.days.len().max(1) as f32;
    for (index, day) in grid.days.iter().enumerate() {
        let center = frame.columns.left() + (index as f32 + 0.5) * width;
        let title = Rect::from_min_size(
            pos2(
                center - DAY_TITLE_WIDTH / 2.0,
                frame.headers.top() + DAY_TITLE_TOP,
            ),
            vec2(DAY_TITLE_WIDTH, DAY_TITLE.line_height),
        );
        let today = *day == grid.now.date();
        if today {
            painter.rect_filled(exo_ui::pixel_snap(title), 2.0, palette.ink(0.05, 0.05));
        }
        let text = format!(
            "{} {}",
            WEEKDAYS[day.weekday().to_sunday_zero_offset() as usize],
            day.day()
        );
        let text_width = DAY_TITLE.width(painter.ctx(), &text);
        let color = if today {
            palette.ink(0.8, 0.8)
        } else {
            palette.ink(0.6, 0.5)
        };
        DAY_TITLE.paint_line(
            painter,
            pos2(title.center().x - text_width / 2.0, title.top() + 1.0),
            &text,
            color,
            None,
        );
    }
}

impl GridView {
    /// The gutter's header: the time zones' abbreviations, the secondary ones first, and in the
    /// right pane, borders below and to the right. Returns whether one of the week's abbreviations
    /// was clicked. They're buttons to Calendar Settings, but not while the form or Share
    /// Availability has the week.
    pub(super) fn paint_timezone(
        &self,
        ui: &Ui,
        painter: &Painter,
        frame: &Frame,
        grid: &Grid<'_>,
    ) -> bool {
        let palette = exo_ui::palette(painter.ctx());
        let header = Rect::from_min_max(
            frame.gutter.min,
            pos2(frame.gutter.right(), frame.headers.bottom()),
        );
        let first = grid
            .days
            .first()
            .and_then(|day| day.to_zoned(grid.tz.clone()).ok());
        let abbreviation = first
            .as_ref()
            .map(|day| day.strftime("%Z").to_string())
            .unwrap_or_default();
        let width = AXIS_TEXT.width(painter.ctx(), &abbreviation);
        if frame.variant == Variant::Week {
            // Right-aligned at the bottom, inside `padding: 0 5px 12px 12px`, nudged down 3px.
            let top = header.bottom() - TIMEZONE_PADDING_BOTTOM - AXIS_TEXT.line_height
                + TIMEZONE_TEXT_TOP;
            let secondary = grid.zones.iter().map(|zone| {
                first
                    .as_ref()
                    .map(|day| crate::calendar::availability::abbreviation(zone, day.timestamp()))
                    .unwrap_or_default()
            });
            let labels: Vec<String> = secondary.chain([abbreviation]).collect();
            let mut right = header.right() - TIMEZONE_PADDING_RIGHT;
            let buttons = grid.mode.allows_zone_settings();
            let mut clicked = false;
            for (index, label) in labels.iter().enumerate().rev() {
                let width = AXIS_TEXT.width(painter.ctx(), label);
                AXIS_TEXT.paint_line(
                    painter,
                    pos2(right - width, top),
                    label,
                    palette.text_base_subdued,
                    None,
                );
                let button = Rect::from_min_max(
                    pos2(right - width.max(ZONE_LABEL), top),
                    pos2(right, top + AXIS_TEXT.line_height),
                );
                clicked |= buttons
                    && ui
                        .interact(
                            button,
                            ui.id().with(("calendar-zone-label", index)),
                            Sense::click(),
                        )
                        .clicked();
                right -= width.max(ZONE_LABEL) + ZONE_GAP;
            }
            return clicked;
        }
        // Right-aligned in a 36px box at the bottom, above `padding-bottom: 2px` and the border.
        let right = header.right() - 1.0 - TIMEZONE_PADDING_RIGHT;
        AXIS_TEXT.paint_line(
            painter,
            pos2(
                right - width,
                header.bottom() - 1.0 - 2.0 - AXIS_TEXT.line_height,
            ),
            &abbreviation,
            palette.text_base_subdued,
            None,
        );
        // Its borders are a `border-image` gradient: light runs `#fff`, `#fff 30%`, `#f2f2f2`;
        // dark `#27292d`, `#27292d` (50%), `rgba(255,255,255,.09)`. The right border takes the
        // gradient's end.
        let (from, hold, to) = match exo_ui::theme(painter.ctx()) {
            exo_ui::Theme::Snow => (Color32::WHITE, 0.3, Color32::from_rgb(0xf2, 0xf2, 0xf2)),
            exo_ui::Theme::Carbon => (
                Color32::from_rgb(0x27, 0x29, 0x2d),
                0.5,
                Color32::from_white_alpha(23),
            ),
        };
        painter.vline(
            snap(header.right()) - 0.5,
            header.y_range(),
            Stroke::new(1.0, to),
        );
        let bottom = Rect::from_min_max(pos2(header.left(), header.bottom() - 1.0), header.max);
        painter.rect_filled(
            Rect::from_min_max(
                bottom.min,
                pos2(bottom.left() + hold * bottom.width(), bottom.bottom()),
            ),
            0.0,
            from,
        );
        paint_gradient(
            painter,
            Rect::from_min_max(
                pos2(bottom.left() + hold * bottom.width(), bottom.top()),
                bottom.max,
            ),
            from,
            to,
        );
        false
    }

    /// The hour labels down the gutter, scrolled with the columns, and its right border. The
    /// hovered quarter hour's label shows in the link colour. On the hour, each secondary time
    /// zone's own hour shows before it.
    pub(super) fn paint_axis(
        &self,
        painter: &Painter,
        frame: &Frame,
        grid: &Grid<'_>,
        hovered: Option<i32>,
    ) {
        let palette = exo_ui::palette(painter.ctx());
        // The axis: `height: calc(100% + 1px); margin-top: -1px`.
        let axis = Rect::from_min_max(
            pos2(frame.gutter.left(), frame.bodies.top() - 1.0),
            pos2(frame.gutter.right(), frame.bodies.bottom()),
        );
        let painter = painter.with_clip_rect(axis);
        painter.vline(
            snap(axis.right()) - 0.5,
            axis.y_range(),
            Stroke::new(1.0, palette.border_base_muted),
        );
        let right = axis.right() - 1.0 - AXIS_PADDING_RIGHT;
        let quarters = (1..96).filter(|quarter| quarter % 4 == 0 || hovered == Some(quarter * 15));
        // While find searches, the labels out of view are painted too, for it to find.
        let searching = exo_ui::find::searching(painter.ctx());
        for quarter in quarters {
            // A label sits at the bottom of its 11px row, nudged down 7px.
            let row_bottom = axis.top() - self.scroll + quarter as f32 * SLOT;
            let top = row_bottom - AXIS_TEXT.line_height + AXIS_TEXT_TOP;
            if !searching && (top > axis.bottom() || top + AXIS_TEXT.line_height < axis.top()) {
                continue;
            }
            let label = hour_label(quarter / 4, quarter % 4 * 15);
            let width = AXIS_TEXT.width(painter.ctx(), &label);
            // The hour labels: Classic's `rgba(0,0,0,.5)`, Carbon `hsla(0,0%,100%,.5)`.
            let ink = palette.ink(0.5, 0.5);
            let color = if hovered == Some(quarter * 15) {
                palette.link_text_primary
            } else {
                ink
            };
            AXIS_TEXT.paint_line(&painter, pos2(right - width, top), &label, color, None);
            if quarter % 4 != 0 {
                continue;
            }
            let at = grid
                .days
                .first()
                .and_then(|day| day.to_zoned(grid.tz.clone()).ok())
                .and_then(|midnight| {
                    midnight
                        .checked_add(SignedDuration::from_mins(i64::from(quarter) * 15))
                        .ok()
                });
            let mut zone_right = right - width.max(ZONE_LABEL) - ZONE_GAP;
            for zone in grid.zones.iter().rev() {
                let Some(there) = at.as_ref().map(|at| at.with_time_zone(zone.clone())) else {
                    continue;
                };
                let label = hour_label(i32::from(there.hour()), i32::from(there.minute()));
                let width = AXIS_TEXT.width(painter.ctx(), &label);
                AXIS_TEXT.paint_line(&painter, pos2(zone_right - width, top), &label, ink, None);
                zone_right -= width.max(ZONE_LABEL) + ZONE_GAP;
            }
        }
    }

    /// The day columns: hour blocks (white in the right pane), the now line, the events.
    pub(super) fn paint_columns(
        &mut self,
        ui: &Ui,
        frame: &Frame,
        grid: &Grid<'_>,
        hovered: Option<i32>,
        actions: &mut Vec<GridAction>,
    ) {
        let palette = exo_ui::palette(ui.ctx());
        let painter = ui.painter_at(frame.columns);
        let count = grid.days.len().max(1) as f32;
        let width = frame.columns.width() / count;
        let top = frame.columns.top() - self.scroll;
        for (index, day) in grid.days.iter().enumerate() {
            let left = frame.columns.left() + index as f32 * width;
            let column = Rect::from_min_max(pos2(left, top), pos2(left + width, top + DAY_HEIGHT));
            // Hour blocks: 44px with a 1px bottom border, inside the column's right border.
            let inner = Rect::from_min_max(column.min, pos2(column.right() - 1.0, column.bottom()));
            if frame.variant == Variant::Pane {
                painter.rect_filled(
                    exo_ui::pixel_snap(inner),
                    0.0,
                    palette.background_base_default,
                );
            }
            for hour in 1..=24 {
                let y = top + hour as f32 * HOUR_HEIGHT - 0.5;
                painter.hline(
                    snap(inner.left())..=snap(inner.right()),
                    snap(y + 0.5) - 0.5,
                    Stroke::new(1.0, palette.border_base_muted),
                );
            }
            painter.vline(
                snap(column.right()) - 0.5,
                frame.columns.y_range(),
                Stroke::new(1.0, palette.border_base_muted),
            );
            let Ok(midnight) = day.to_zoned(grid.tz.clone()) else {
                continue;
            };
            if grid.mode.is_find_time() {
                // The hours outside work, shaded.
                for (from, to) in crate::calendar::meet::disabled_hours(*day) {
                    let shade = Rect::from_min_max(
                        pos2(inner.left(), top + f32::from(from) * HOUR_HEIGHT),
                        pos2(inner.right(), top + f32::from(to) * HOUR_HEIGHT),
                    );
                    painter.rect_filled(
                        shade,
                        0.0,
                        crate::calendar::meet::disabled_fill(exo_ui::theme(ui.ctx())),
                    );
                }
            }
            let dragged = self.dragged(grid);
            self.paint_events(ui, frame, inner, &midnight, grid, dragged.as_ref(), actions);
            if grid.now.date() == *day {
                // `top: calc({fraction of the day}% - 1px)`.
                let seconds = grid
                    .now
                    .time()
                    .duration_since(jiff::civil::Time::midnight())
                    .as_secs_f64() as f32;
                let y = top + seconds / 86_400.0 * DAY_HEIGHT - 1.0;
                painter.rect_filled(
                    Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 2.0)),
                    0.0,
                    NOW_LINE,
                );
                if frame.variant == Variant::Week {
                    painter.circle_filled(
                        pos2(
                            inner.left() - 4.5 + NOW_BULB / 2.0,
                            y - 3.0 + NOW_BULB / 2.0,
                        ),
                        NOW_BULB / 2.0,
                        NOW_LINE,
                    );
                }
            }
        }
        if let Some(minutes) = hovered {
            paint_indicator(
                &painter,
                frame.columns,
                top + minutes as f32 / 15.0 * SLOT - 1.5,
            );
        }
    }

    /// The all-day row: a band (white in the right pane) with a bottom border, the events in it,
    /// each row's cells with a right border.
    pub(super) fn paint_all_day(
        &mut self,
        ui: &Ui,
        painter: &Painter,
        frame: &Frame,
        grid: &Grid<'_>,
        rows: &[Vec<Cell<'_>>],
        actions: &mut Vec<GridAction>,
    ) {
        let palette = exo_ui::palette(ui.ctx());
        let band = Rect::from_min_max(
            frame.all_day.min,
            pos2(frame.all_day.right(), frame.all_day.bottom() - 1.0),
        );
        if frame.variant == Variant::Pane {
            painter.rect_filled(band, 0.0, palette.background_base_default);
        }
        painter.hline(
            snap(frame.all_day.left())..=snap(frame.all_day.right()),
            snap(frame.all_day.bottom()) - 0.5,
            Stroke::new(1.0, palette.border_base_muted),
        );
        let painter = painter.with_clip_rect(band);
        let column = band.width() / grid.days.len().max(1) as f32;
        let editing_draft = grid.mode.is_editing_draft();
        let selected = grid.mode.selected();
        for (index, row) in rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.iter().any(|cell| cell.event.is_some()))
        {
            let top = band.top() + ALL_DAY_PADDING_TOP + index as f32 * ALL_DAY_ROW;
            let mut left = band.left();
            for cell in row {
                let width = cell.days as f32 * column;
                painter.vline(
                    snap(left + width) - 0.5,
                    top..=top + ALL_DAY_ROW,
                    Stroke::new(1.0, palette.border_base_muted),
                );
                if let Some(event) = cell.event {
                    // `width: calc(100% - 6px); margin: 0 1px` inside the cell's border.
                    let rect = Rect::from_min_size(
                        pos2(left + 1.0, top),
                        vec2(width - 1.0 - 6.0, ALL_DAY_BLOCK),
                    );
                    let id = exo_ui::id(
                        ui.ctx(),
                        ("calendar-block", frame.variant, &event.key, "all-day"),
                    );
                    let response = ui.interact(rect.intersect(band), id, Sense::click_and_drag());
                    let focused = event.key == DRAFT;
                    if focused {
                        self.draft_anchor = Some(rect);
                    }
                    if response.clicked() && grid.mode.selects_events() {
                        actions.push(GridAction::Select {
                            key: event.key.clone(),
                            anchor: rect,
                        });
                    }
                    self.take_event(ui, &response, event, rect, actions);
                    let rect = rect.translate(jiggle(ui, &response, event, Vec2::X, actions));
                    let hovered = response.hovered();
                    if event.everyone_else_declined && !editing_draft {
                        exo_ui::KeyTooltip {
                            text: "Everyone else declined",
                            keys: &[],
                        }
                        .show_below(ui, &response, 8.0);
                    }
                    let active = grid
                        .invite
                        .is_some_and(|uid| event.ical_uid.as_deref() == Some(uid))
                        || selected == Some(event.key.as_str());
                    let lifted = (hovered || active) && !focused;
                    let look = Look {
                        glowing: block::glow(ui.ctx(), id, lifted),
                        lifted,
                        past: event.end < *grid.now,
                        all_day: true,
                        preview: editing_draft,
                        focused,
                        ..Look::default()
                    };
                    block::paint(ui, band, rect, event, SignedDuration::from_hours(24), look);
                }
                left += width;
            }
        }
    }

    /// The all-day row while guests are overlaid. Each day's columns sit side by side across the
    /// day less 5px (`flex: 1; margin-right: 5px`), the open ones at 50%, with each column's events
    /// stacked from its top, 2px narrower than the column. There are no borders between the days.
    /// A click opens an event's details (the events don't move), a collapsed column hides their
    /// text, and resting on a column expands it.
    pub(super) fn paint_attendee_all_day(
        &mut self,
        ui: &Ui,
        painter: &Painter,
        frame: &Frame,
        grid: &Grid<'_>,
        days: &[Vec<crate::calendar::guests::AllDayColumn<'_>>],
        actions: &mut Vec<GridAction>,
    ) {
        let palette = exo_ui::palette(ui.ctx());
        let band = Rect::from_min_max(
            frame.all_day.min,
            pos2(frame.all_day.right(), frame.all_day.bottom() - 1.0),
        );
        if frame.variant == Variant::Pane {
            painter.rect_filled(band, 0.0, palette.background_base_default);
        }
        painter.hline(
            snap(frame.all_day.left())..=snap(frame.all_day.right()),
            snap(frame.all_day.bottom()) - 0.5,
            Stroke::new(1.0, palette.border_base_muted),
        );
        let day_width = band.width() / grid.days.len().max(1) as f32;
        let editing_draft = grid.mode.is_editing_draft();
        let selected = grid.mode.selected();
        for ((index, columns), day) in days.iter().enumerate().zip(grid.days) {
            let container = Rect::from_min_size(
                pos2(
                    band.left() + index as f32 * day_width,
                    band.top() + ALL_DAY_PADDING_TOP,
                ),
                vec2(day_width - 5.0, band.height() - ALL_DAY_PADDING_TOP),
            );
            let widths = crate::calendar::guests::widths(
                &columns
                    .iter()
                    .map(|(_, collapsed, _)| *collapsed)
                    .collect::<Vec<_>>(),
            );
            let mut left = container.left();
            for ((guest, collapsed, events), share) in columns.iter().zip(widths) {
                let width = container.width() * share;
                let rect = Rect::from_min_size(
                    pos2(left, container.top()),
                    vec2(width, container.height()),
                );
                self.hover_column(ui, band, rect, (*day, guest.clone()));
                for (row, event) in events.iter().enumerate() {
                    let block = Rect::from_min_size(
                        pos2(left + 1.0, container.top() + row as f32 * ALL_DAY_ROW),
                        vec2(width - 2.0, ALL_DAY_BLOCK),
                    );
                    let id = exo_ui::id(
                        ui.ctx(),
                        (
                            "calendar-block",
                            frame.variant,
                            &event.key,
                            "all-day",
                            *day,
                            guest.as_deref(),
                        ),
                    );
                    let response = ui.interact(block.intersect(band), id, Sense::click());
                    let focused = event.key == DRAFT;
                    if focused {
                        self.draft_anchor = Some(block);
                    }
                    if response.clicked() && grid.mode.selects_events() && !focused {
                        actions.push(GridAction::Select {
                            key: event.key.clone(),
                            anchor: block,
                        });
                    }
                    let active = grid
                        .invite
                        .is_some_and(|uid| event.ical_uid.as_deref() == Some(uid))
                        || selected == Some(event.key.as_str());
                    let lifted = (response.hovered() || active) && !focused && !collapsed;
                    let look = Look {
                        glowing: block::glow(ui.ctx(), id, lifted),
                        lifted,
                        past: event.end < *grid.now,
                        all_day: true,
                        preview: editing_draft,
                        focused,
                        no_text: *collapsed,
                        clipped: true,
                        ..Look::default()
                    };
                    block::paint(ui, band, block, event, SignedDuration::from_hours(24), look);
                }
                left += width;
            }
        }
    }

    /// A day column's timed events, laid out, hovered ones on top, the one the form makes or a drag
    /// moves (`extra`) over them all.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame, the column and its day, what's shown, and what came of it"
    )]
    fn paint_events(
        &mut self,
        ui: &Ui,
        frame: &Frame,
        column: Rect,
        midnight: &Zoned,
        grid: &Grid<'_>,
        extra: Option<&Event>,
        actions: &mut Vec<GridAction>,
    ) {
        let clip = frame.columns;
        let day = midnight.date();
        let all: Vec<&Event> = shown(grid, extra)
            .into_iter()
            .filter(|event| !event.in_all_day_row() && event.on(day))
            .collect();
        let width = column.width() - EVENTS_INSET;
        let day_end = midnight
            .tomorrow()
            .map_or(midnight.timestamp(), |next| next.timestamp());
        // Shared slots span the column under the rest, and events overlapping them are laid out
        // from 5% in (`inset: 0 1px auto 5%`). With guests' calendars shown, the draft spans the
        // day over their columns too, and nothing is indented.
        let columns_shown = !grid.guests.is_empty();
        let (slots, events): (Vec<&Event>, Vec<&Event>) = all.into_iter().partition(|event| {
            event.share.is_some()
                || (columns_shown && (event.key == DRAFT || event.key.starts_with(FREE_SLOT)))
        });
        let overlapping = if columns_shown {
            Vec::new()
        } else {
            overlapping_slots(&slots, &events)
        };
        let (indented, events): (Vec<&Event>, Vec<&Event>) = events
            .into_iter()
            .partition(|event| overlapping.contains(&event.key));
        let mut boxes: Vec<(&Event, Rect, bool, Option<InColumn>)> = Vec::new();
        // The guests' columns: mine, then theirs, side by side.
        let guest_columns = columns_shown.then(|| {
            crate::calendar::guests::columns(
                grid.mine.then(|| events.clone()),
                grid.guests,
                grid.expanded,
                midnight,
            )
        });
        if let Some(columns) = &guest_columns {
            let widths = crate::calendar::guests::widths(
                &columns
                    .iter()
                    .map(|column| column.collapsed)
                    .collect::<Vec<_>>(),
            );
            let mut left = column.left();
            for (guest_column, share) in columns.iter().zip(&widths) {
                let room = width * share;
                let rect = Rect::from_min_max(
                    pos2(left, column.top()),
                    pos2(left + room, column.bottom()),
                );
                self.hover_column(ui, frame.bodies, rect, (day, guest_column.guest.clone()));
                let spans: Vec<_> = guest_column
                    .events
                    .iter()
                    .map(|event| (event.start.timestamp(), event.end.timestamp()))
                    .collect();
                for place in layout::day_layout(&spans, midnight) {
                    let block = Rect::from_min_size(
                        pos2(
                            left + place.left / 100.0 * room + 1.0,
                            column.top() + place.top,
                        ),
                        vec2(place.width / 100.0 * room - 2.0, place.height),
                    );
                    // Guests' events show their time while there are two columns at most.
                    let time = guest_column.guest.is_none() || columns.len() <= 2;
                    boxes.push((
                        &guest_column.events[place.event],
                        block,
                        place.time_on_same_line,
                        Some(InColumn {
                            collapsed: guest_column.collapsed,
                            time,
                            guest: guest_column.guest.is_some(),
                        }),
                    ));
                }
                left += room;
            }
        }
        for slot in &slots {
            let top = (slot
                .start
                .timestamp()
                .max(midnight.timestamp())
                .duration_since(midnight.timestamp())
                .as_secs_f64()
                / 3600.0) as f32
                * HOUR_HEIGHT;
            let bottom = (slot
                .end
                .timestamp()
                .min(day_end)
                .duration_since(midnight.timestamp())
                .as_secs_f64()
                / 3600.0) as f32
                * HOUR_HEIGHT;
            // Over guests' columns the block reaches the day's edge. Without them, it leaves room
            // to click beside it.
            let right = if columns_shown {
                column.right()
            } else {
                column.left() + width - 1.0
            };
            let same_line = slot.share.is_none()
                && layout::day_layout(&[(slot.start.timestamp(), slot.end.timestamp())], midnight)
                    .first()
                    .is_some_and(|place| place.time_on_same_line);
            boxes.push((
                slot,
                Rect::from_min_max(
                    pos2(column.left() + 1.0, column.top() + top),
                    pos2(right, column.top() + bottom - 1.0),
                ),
                same_line,
                None,
            ));
        }
        let groups: [(&Vec<&Event>, f32, f32); 2] = [
            (&events, 0.0, width),
            (&indented, width * 0.05, width * 0.95 - 1.0),
        ];
        for (group, left, room) in groups.into_iter().filter(|_| !columns_shown) {
            let spans: Vec<_> = group
                .iter()
                .map(|event| (event.start.timestamp(), event.end.timestamp()))
                .collect();
            for place in layout::day_layout(&spans, midnight) {
                // `left: {x}%; width: calc({w}% - 2px); margin: 0 1px`.
                let rect = Rect::from_min_size(
                    pos2(
                        column.left() + left + place.left / 100.0 * room + 1.0,
                        column.top() + place.top,
                    ),
                    vec2(place.width / 100.0 * room - 2.0, place.height),
                );
                boxes.push((group[place.event], rect, place.time_on_same_line, None));
            }
        }
        // The columns' blocks' fills, for what the draft blends with.
        let theme = exo_ui::theme(ui.ctx());
        let editing_draft = grid.mode.is_editing_draft();
        let selected = grid.mode.selected();
        let preview = matches!(
            grid.mode,
            GridMode::EditingDraft { .. } | GridMode::FindTime { .. }
        ) || grid.events.iter().any(|event| event.share.is_some());
        let column_blocks: Vec<(Rect, Color32)> = boxes
            .iter()
            .filter(|(.., in_column)| in_column.is_some())
            .map(|(event, rect, ..)| {
                let palette = exo_ui::BlockPalette {
                    unaccepted: event.style() == crate::calendar::event::Style::Bordered,
                    create: preview,
                    past: event.end.timestamp().min(day_end) < grid.now.timestamp(),
                    ..exo_ui::BlockPalette::default()
                };
                (*rect, event.color.block(theme, palette).fill)
            })
            .collect();
        let mut glowing = Vec::new();
        for (event, rect, time_on_same_line, in_column) in boxes {
            // The right pane's grid and the week's can both show a block, on different layers.
            let id = exo_ui::id(ui.ctx(), ("calendar-block", frame.variant, &event.key, day));
            // The moved copy of a dragged event is drawn, not interacted with.
            let copy = extra.is_some_and(|extra| std::ptr::eq(extra, event)) && event.key != DRAFT;
            let id = if copy { id.with("moved") } else { id };
            // A guest's event opens its details but doesn't move, and a collapsed column's bar only
            // shows.
            let collapsed = in_column.is_some_and(|placed| placed.collapsed);
            let guest = in_column.is_some_and(|placed| placed.guest);
            let inert = copy || collapsed || guest;
            let id = if collapsed || guest {
                id.with("column")
            } else {
                id
            };
            let sense = if copy || collapsed {
                Sense::hover()
            } else if guest {
                Sense::click()
            } else {
                Sense::click_and_drag()
            };
            let response = ui.interact(rect.intersect(clip), id, sense);
            // The day's events (`width: calc(100% - 4px)`), 1px in from each side. `column` already
            // leaves out the day's right border.
            let anchor = Rect::from_x_y_ranges(
                column.left() + 1.0..=column.right() - EVENTS_INSET - 1.0,
                rect.y_range(),
            );
            if event.key == DRAFT {
                // Over guests' columns the draft spans the day, and the form goes beside that, 1px
                // in.
                self.draft_anchor = Some(if columns_shown {
                    Rect::from_x_y_ranges(
                        column.left() + 1.0..=column.right() - 1.0,
                        anchor.y_range(),
                    )
                } else {
                    anchor
                });
            }
            // A slot's cross (`padding: 6px 6px 4px 4px` around it) removes the slot. A click
            // elsewhere on the slot does nothing.
            let cross = Rect::from_min_max(
                pos2(rect.right() - 18.0, rect.top()),
                pos2(rect.right(), rect.top() + 18.0),
            );
            if response.clicked() && event.share.is_some() {
                if response
                    .interact_pointer_pos()
                    .is_some_and(|at| cross.contains(at))
                {
                    actions.push(GridAction::Remove(event.key.clone()));
                }
            } else if (response.clicked() || self.select.as_ref() == Some(&event.key))
                && grid.mode.selects_events()
            {
                self.select = None;
                actions.push(GridAction::Select {
                    key: event.key.clone(),
                    anchor,
                });
            }
            if !inert {
                self.take_event(ui, &response, event, rect, actions);
            }
            let rect = if inert {
                rect
            } else {
                rect.translate(jiggle(ui, &response, event, Vec2::Y, actions))
            };
            if event.editable && !inert && !editing_draft {
                // `cursor: ns-resize` at the bottom edge.
                if response
                    .hover_pos()
                    .is_some_and(|at| at.y > rect.bottom() - RESIZE_EDGE)
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
            }
            let hovered = response.hovered();
            // The event's own tooltip, else `Everyone else declined` (not on previews), under the
            // block.
            if let Some(text) = event.tooltip.or_else(|| {
                (event.everyone_else_declined && !preview).then_some("Everyone else declined")
            }) {
                exo_ui::KeyTooltip { text, keys: &[] }.show_below(ui, &response, 8.0);
            }
            let focused = event.key == DRAFT || copy || event.key.starts_with(FREE_SLOT);
            let active = grid
                .invite
                .is_some_and(|uid| event.ical_uid.as_deref() == Some(uid))
                || selected == Some(event.key.as_str());
            let start = event.start.timestamp().max(midnight.timestamp());
            let end = event.end.timestamp().min(day_end);
            let lifted = (hovered || active) && !focused && event.share.is_none();
            let look = Look {
                glowing: block::glow(ui.ctx(), id, lifted),
                lifted,
                past: end < grid.now.timestamp(),
                time_on_same_line,
                // With slots picked, the rest are previews.
                preview,
                // A slot that an event starts within half an hour of shows its time and cross only
                // on hover.
                covered: event.share.is_some()
                    && !hovered
                    && indented.iter().any(|other| {
                        other.start < event.end
                            && other.end > event.start
                            && other.start.timestamp()
                                < event.start.timestamp() + SignedDuration::from_mins(30)
                    }),
                focused,
                bare: event.key.starts_with(FREE_SLOT),
                // A column's event under the draft hides its text.
                no_text: in_column.is_some_and(|placed| {
                    placed.collapsed
                        || (!hovered
                            && slots.iter().any(|slot| {
                                (slot.key == DRAFT || slot.share.is_some())
                                    && overlaps_draft(slot, event)
                            }))
                }),
                no_time: in_column.is_some_and(|placed| !placed.time),
                ..Look::default()
            };
            // A hovered slot comes over the events.
            if event.share.is_some() && hovered {
                glowing.push((rect, event, end.duration_since(start), look));
            } else if event.share.is_some() {
                block::paint(ui, clip, rect, event, end.duration_since(start), look);
            } else if look.glowing > 0.0 || focused {
                glowing.push((rect, event, end.duration_since(start), look));
            } else {
                block::paint(ui, clip, rect, event, end.duration_since(start), look);
            }
        }
        // Hovered and active blocks go over the rest, and the focused one over them.
        glowing.sort_by_key(|(_, _, _, look)| look.focused);
        for (rect, event, span, look) in glowing {
            block::paint(ui, clip, rect, event, span, look);
            // Over guests' columns the draft and the shared slots blend with what's under them
            // (`mix-blend-mode: multiply`, Carbon's `screen`).
            if columns_shown && (event.key == DRAFT || event.share.is_some()) {
                blend_over(ui, clip, rect, event, span, look, &column_blocks);
            }
        }
    }
}

/// While the pointer is over the grid, a dotted line at the quarter hour nearest it.
fn paint_indicator(painter: &Painter, columns: Rect, y: f32) {
    let mut x = columns.left();
    while x < columns.right() {
        painter.rect_filled(
            Rect::from_min_size(pos2(x + 0.75, y), vec2(3.0, 1.5)),
            0.75,
            INDICATOR,
        );
        x += 4.5;
    }
}

/// Repaints the parts of `event`'s block (drawn in `rect` with `look`) over `under`'s blocks as
/// `mix-blend-mode` mixes them: multiplied with the block's fill in Snow, screened in Carbon.
fn blend_over(
    ui: &Ui,
    clip: Rect,
    rect: Rect,
    event: &Event,
    span: SignedDuration,
    look: Look,
    under: &[(Rect, Color32)],
) {
    let theme = exo_ui::theme(ui.ctx());
    let own = if look.focused {
        event.color.block(
            theme,
            exo_ui::BlockPalette {
                focused: true,
                ..exo_ui::BlockPalette::default()
            },
        )
    } else {
        event.color.block(
            theme,
            exo_ui::BlockPalette {
                create: look.preview,
                ..exo_ui::BlockPalette::default()
            },
        )
    };
    let mix = |top: Color32, bottom: Color32| {
        let channel = |a: u8, b: u8| match theme {
            exo_ui::Theme::Snow => (u16::from(a) * u16::from(b) / 255) as u8,
            exo_ui::Theme::Carbon => {
                255 - ((255 - u16::from(a)) * (255 - u16::from(b)) / 255) as u8
            }
        };
        Color32::from_rgb(
            channel(top.r(), bottom.r()),
            channel(top.g(), bottom.g()),
            channel(top.b(), bottom.b()),
        )
    };
    // Screened over Carbon's dark page, the whole block lightens, while multiplied over Snow's
    // white it stays as it is.
    let page = exo_ui::palette(ui.ctx()).background_base_default;
    let base = (exo_ui::pixel_snap(rect), page);
    for (block_rect, beneath) in std::iter::once(&base)
        .filter(|_| theme == exo_ui::Theme::Carbon)
        .chain(under)
    {
        let overlap = exo_ui::pixel_snap(*block_rect)
            .intersect(exo_ui::pixel_snap(rect))
            .intersect(clip);
        if overlap.width() <= 0.0 || overlap.height() <= 0.0 {
            continue;
        }
        let beneath = *beneath;
        let colors = exo_ui::BlockColors {
            text: mix(own.text, beneath),
            fill: mix(own.fill, beneath),
            border: mix(own.fill, beneath),
        };
        block::paint(
            ui,
            overlap,
            rect,
            event,
            span,
            Look {
                colors: Some(colors),
                ..look
            },
        );
    }
}

/// Whether `event` overlaps the draft and starts within three hours of the draft's start.
fn overlaps_draft(draft: &Event, event: &Event) -> bool {
    event.start < draft.end
        && event.end > draft.start
        && event.start.timestamp() < draft.start.timestamp() + SignedDuration::from_hours(3)
}

/// The events overlapping a slot: in each run of events that overlap one another, every one but
/// the slots, when the run has a slot.
fn overlapping_slots(slots: &[&Event], events: &[&Event]) -> Vec<String> {
    if slots.is_empty() || events.is_empty() {
        return Vec::new();
    }
    let mut all: Vec<(&Event, bool)> = slots
        .iter()
        .map(|slot| (*slot, true))
        .chain(events.iter().map(|event| (*event, false)))
        .collect();
    all.sort_by_key(|(event, _)| event.start.timestamp());
    let mut found = Vec::new();
    let mut run: Vec<(&Event, bool)> = Vec::new();
    let mut run_end = all[0].0.end.timestamp();
    for (event, slot) in all {
        if !run.is_empty() && event.start.timestamp() >= run_end {
            if run.iter().any(|(_, slot)| *slot) {
                found.extend(
                    run.iter()
                        .filter(|(_, slot)| !*slot)
                        .map(|(event, _)| event.key.clone()),
                );
            }
            run.clear();
        }
        run_end = if run.is_empty() {
            event.end.timestamp()
        } else {
            run_end.max(event.end.timestamp())
        };
        run.push((event, slot));
    }
    if run.iter().any(|(_, slot)| *slot) {
        found.extend(
            run.iter()
                .filter(|(_, slot)| !*slot)
                .map(|(event, _)| event.key.clone()),
        );
    }
    found
}

/// Where Chromium paints a border edge laid out at `at`, which is on the whole px.
fn snap(at: f32) -> f32 {
    (at + 0.5).floor()
}

/// A 1px line in `color` that fades in from transparent over its first `ramp` px.
pub(super) fn paint_fade_in(painter: &Painter, line: Rect, ramp: f32, color: Color32) {
    let mid = (line.left() + ramp).min(line.right());
    paint_gradient(
        painter,
        Rect::from_min_max(line.min, pos2(mid, line.bottom())),
        Color32::TRANSPARENT,
        color,
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(mid, line.top()), line.max),
        0.0,
        color,
    );
}

/// `rect` filled with a left-to-right gradient.
fn paint_gradient(painter: &Painter, rect: Rect, from: Color32, to: Color32) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let mut mesh = Mesh::default();
    for (pos, color) in [
        (rect.left_top(), from),
        (rect.right_top(), to),
        (rect.right_bottom(), to),
        (rect.left_bottom(), from),
    ] {
        mesh.vertices.push(Vertex {
            pos,
            uv: WHITE_UV,
            color,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}
