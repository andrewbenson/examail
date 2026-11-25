//! The time grid: a gutter of hour labels beside a column per day, 44px an hour, an all-day row
//! above, the events laid out in their columns, and the line at the current time.

mod drag;
mod paint;

use egui::{Rect, Sense, Ui, pos2, vec2};
use exo_ui::Scrollbar;
use jiff::{
    SignedDuration, ToSpan, Zoned,
    civil::{Date, Weekday},
    tz::TimeZone,
};

use super::{event::Event, layout::HOUR_HEIGHT};

/// A day's height.
pub(super) const DAY_HEIGHT: f32 = 24.0 * HOUR_HEIGHT;
/// A time-axis row: 15 minutes.
pub(super) const SLOT: f32 = HOUR_HEIGHT / 4.0;
/// The time zones' header in the right pane: `padding-top: 12px; padding-bottom: 2px` and a bottom
/// border.
const PANE_TIMEZONE_HEIGHT: f32 = 12.0 + 12.0 + 2.0 + 1.0;
/// The all-day row's height for one, two, and three or more rows, and the right pane's
/// `margin-top: -3px`.
const ALL_DAY_HEIGHTS: [f32; 3] = [30.0, 54.0, 66.0];
const PANE_ALL_DAY_OVERLAP: f32 = 3.0;
/// The gutter is 64px, or 60px in the right pane. With a secondary time zone it's 93px, or 111px
/// when that zone is off the hour from this one.
const WEEK_GUTTER: f32 = 64.0;
const PANE_GUTTER: f32 = 60.0;
const ZONES_GUTTER: f32 = 93.0;
const MINUTE_ZONES_GUTTER: f32 = 111.0;
/// The hour and time zone labels: each zone's is at least 36px wide (the first 54px when off the
/// hour), right-aligned and 4px apart.
pub(super) const ZONE_LABEL: f32 = 36.0;
pub(super) const ZONE_GAP: f32 = 4.0;
/// A weekday header: `padding-top: 28px; padding-bottom: 15px`, a 60px title 22px tall
/// with `margin-bottom: 2px`, 14px/22px nudged `padding-top: 1px`.
const DAY_HEADER: f32 = 28.0 + 22.0 + 2.0 + 15.0;
/// The key of the event the form makes (`POTENTIAL_EVENT`).
pub const DRAFT: &str = "POTENTIAL_EVENT";
/// What Find Time's free slots' keys start with.
pub const FREE_SLOT: &str = "FIND_TIME_";

/// Interaction mode of the calendar [`Grid`].
#[derive(Clone, Copy, Debug, Default)]
pub enum GridMode<'a> {
    /// Normal day/week browsing: clicking empty grid creates an event, clicking an event opens
    /// its details.
    #[default]
    Browse,
    /// Event details popover is open for `selected`; clicking empty grid dismisses it.
    DetailsOpen { selected: &'a str },
    /// Event create/edit form is open with an optional `draft` block and `hidden` source event.
    EditingDraft {
        draft: Option<&'a Event>,
        hidden: Option<&'a str>,
    },
    /// Meet With's Find Time mode: hours outside work are shaded and the empty grid is inert.
    FindTime { selected: Option<&'a str> },
    /// Share Availability mode: clicking/dragging empty future hours picks `minutes`-long slots.
    ShareAvailability {
        minutes: i64,
        selected: Option<&'a str>,
    },
}

impl<'a> GridMode<'a> {
    pub(super) fn selected(self) -> Option<&'a str> {
        match self {
            Self::Browse | Self::EditingDraft { .. } => None,
            Self::DetailsOpen { selected } => Some(selected),
            Self::FindTime { selected } | Self::ShareAvailability { selected, .. } => selected,
        }
    }

    pub(super) fn draft(self) -> Option<&'a Event> {
        match self {
            Self::EditingDraft { draft, .. } => draft,
            _ => None,
        }
    }

    pub(super) fn hidden(self) -> Option<&'a str> {
        match self {
            Self::EditingDraft { hidden, .. } => hidden,
            _ => None,
        }
    }

    pub(super) fn is_editing_draft(self) -> bool {
        matches!(self, Self::EditingDraft { .. })
    }

    pub(super) fn is_find_time(self) -> bool {
        matches!(self, Self::FindTime { .. })
    }

    pub(super) fn sharing_minutes(self) -> Option<i64> {
        match self {
            Self::ShareAvailability { minutes, .. } => Some(minutes),
            _ => None,
        }
    }

    pub(super) fn shows_hover_indicator(self) -> bool {
        match self {
            Self::Browse | Self::EditingDraft { .. } => true,
            Self::ShareAvailability { selected, .. } => selected.is_none(),
            Self::DetailsOpen { .. } | Self::FindTime { .. } => false,
        }
    }

    pub(super) fn allows_zone_settings(self) -> bool {
        !matches!(
            self,
            Self::EditingDraft { .. } | Self::ShareAvailability { .. }
        )
    }

    pub(super) fn selects_events(self) -> bool {
        !self.is_editing_draft()
    }
}

/// What the grid shows.
pub struct Grid<'a> {
    /// The days, one column each.
    pub days: &'a [Date],
    /// Events to draw (declined ones already left out).
    pub events: &'a [Event],
    /// The current time, for the line and for past events.
    pub now: &'a Zoned,
    pub tz: &'a TimeZone,
    /// The iCalendar UID of the invite being read, whose block glows.
    pub invite: Option<&'a str>,
    /// Which interaction state owns the grid.
    pub mode: GridMode<'a>,
    /// Guests' calendars overlaid as columns: each guest's events, in their colour
    /// ([`super::guests::guest_events`]).
    pub guests: &'a [(String, Vec<Event>)],
    /// The column a hover expanded: a guest's address, or mine otherwise.
    pub expanded: Option<&'a str>,
    /// My events take a column beside the guests'.
    pub mine: bool,
    /// Secondary time zones, whose hours and names the week's gutter shows before this one's.
    pub zones: &'a [TimeZone],
}

/// The grid's scroll and pointer state, kept across frames.
#[derive(Default)]
pub struct GridView {
    pub(super) scroll: f32,
    /// The all-day row count the initial scroll was made for.
    scrolled_for: Option<usize>,
    /// The hour an event the grid opened for ends at.
    reveal: Option<i8>,
    /// An event whose details open once the grid draws it (after a calendar notification's
    /// click).
    pub(super) select: Option<String>,
    scrollbar: Scrollbar,
    /// What's being dragged.
    pub(super) drag: Option<Drag>,
    /// Where the draft was drawn last, which the form is placed beside.
    pub draft_anchor: Option<Rect>,
    /// When the grid last scrolled itself during a drag.
    pub(super) scrolled_at: Option<f64>,
    /// The guests' column under the pointer this frame (its day and calendar, `None` for
    /// mine), and hover intent on the columns.
    column_under: Option<(Date, Option<String>)>,
    column_intent: exo_ui::HoverIntent<(Date, Option<String>)>,
}

/// A drag in the grid.
#[derive(Clone, Debug)]
pub(super) enum Drag {
    /// Drafting a new event on day `day`: from the quarter hour it started on to the one under
    /// the pointer, in minutes.
    New { day: usize, from: i32, to: i32 },
    /// Drafting an all-day event across the days `from` to `to`.
    NewAllDay { from: usize, to: usize },
    /// Moving event `key` (or with `resize`, its end) to `start`–`end`. `grab` is where the
    /// pointer took it after its start (in civil days `grab_days` for header-band blocks).
    Event {
        key: String,
        resize: bool,
        span_row: bool,
        grab_days: i64,
        grab: SignedDuration,
        start: Zoned,
        end: Zoned,
        original: (Zoned, Zoned),
    },
}

/// Where the grid is: the right pane's one day, or the week view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The right pane's: no weekday headers, a white grid between hairlines.
    Pane,
    /// The week view's: weekday headers, the now line's bulb.
    Week,
}

/// What the pointer did in the grid.
#[derive(Clone, Debug, PartialEq)]
pub enum GridAction {
    /// Clicked event `key`. Its details go beside `anchor`, which is the block's day column at
    /// the block's height, or an all-day block itself.
    Select { key: String, anchor: Rect },
    /// Clicked empty grid or all-day row, outside the events.
    Background,
    /// Started dragging an event I can't change.
    Restricted,
    /// Clicked or dragged out empty time, for a new event there or the draft moved there.
    Slot {
        start: Zoned,
        end: Zoned,
        all_day: bool,
    },
    /// Moved or resized event `key` (the draft's too, [`DRAFT`]) to `start`–`end`.
    Change {
        key: String,
        start: Zoned,
        end: Zoned,
    },
    /// Double-clicked event `key`, which I can change, for its form.
    Open(String),
    /// Started picking a time before now while sharing availability.
    Past,
    /// The cross on shared slot `key`.
    Remove(String),
    /// A time zone's name in the gutter, for Calendar Settings at the secondary time zone.
    ZoneSettings,
    /// The pointer rested on a guests' column, which expands: a guest's address, or `None` for
    /// mine.
    Expand(Option<String>),
}

/// The parts of the grid's box.
pub(super) struct Frame {
    pub(super) variant: Variant,
    pub(super) gutter: Rect,
    pub(super) headers: Rect,
    pub(super) all_day: Rect,
    pub(super) bodies: Rect,
    pub(super) columns: Rect,
}

impl GridView {
    /// Scrolls to the start again the next time the grid shows (a new mount).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether something is being dragged.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Escape while dragging, which drops the drag where it started.
    pub fn cancel_drag(&mut self) {
        self.drag = None;
    }

    /// Once the grid has scrolled to the morning, scrolls to the bottom if an event ending at
    /// `hour` would end below the view (an invite's day).
    pub fn reveal(&mut self, hour: i8) {
        self.reveal = Some(hour);
    }

    /// Opens event `key`'s details once the grid draws it.
    pub fn select(&mut self, key: String) {
        self.select = Some(key);
    }

    /// Draws the grid in `rect`. Returns what the pointer did.
    pub fn show(
        &mut self,
        ui: &Ui,
        rect: Rect,
        rem: f32,
        grid: &Grid<'_>,
        variant: Variant,
    ) -> Vec<GridAction> {
        self.draft_anchor = None;
        let dragged = self.dragged(grid);
        let rows = paint::all_day_rows(grid, dragged.as_ref());
        // With guests overlaid, as many rows as the fullest column of a day has events.
        let attendee_rows = (!grid.guests.is_empty()).then(|| {
            super::guests::all_day_columns(
                &shown(grid, dragged.as_ref()),
                grid.guests,
                grid.expanded,
                grid.days,
            )
        });
        let row_count = attendee_rows.as_ref().map_or(rows.len(), |days| {
            days.iter()
                .flatten()
                .map(|(.., events)| events.len())
                .max()
                .unwrap_or(0)
        });
        let all_day_height = ALL_DAY_HEIGHTS[row_count.clamp(1, 3) - 1];
        let frame = match variant {
            Variant::Pane => {
                // `padding: 0 2rem 0 calc(2rem - 26px)`.
                let content = Rect::from_min_max(
                    pos2(rect.left() + 2.0 * rem - 26.0, rect.top()),
                    pos2(rect.right() - 2.0 * rem, rect.bottom()),
                );
                let headers = Rect::from_min_size(
                    content.min,
                    vec2(
                        content.width(),
                        PANE_TIMEZONE_HEIGHT.max(all_day_height - PANE_ALL_DAY_OVERLAP),
                    ),
                );
                let bodies =
                    Rect::from_min_max(pos2(content.left(), headers.bottom()), content.max);
                Frame {
                    variant,
                    gutter: Rect::from_min_max(
                        content.min,
                        pos2(content.left() + PANE_GUTTER, content.bottom()),
                    ),
                    headers,
                    all_day: Rect::from_min_max(
                        pos2(
                            content.left() + PANE_GUTTER,
                            headers.bottom() - all_day_height,
                        ),
                        pos2(content.right(), headers.bottom()),
                    ),
                    bodies,
                    columns: Rect::from_min_max(
                        pos2(content.left() + PANE_GUTTER, bodies.top()),
                        bodies.max,
                    ),
                }
            }
            Variant::Week => {
                let headers =
                    Rect::from_min_size(rect.min, vec2(rect.width(), DAY_HEADER + all_day_height));
                let bodies = Rect::from_min_max(pos2(rect.left(), headers.bottom()), rect.max);
                let gutter = week_gutter(grid);
                Frame {
                    variant,
                    gutter: Rect::from_min_max(rect.min, pos2(rect.left() + gutter, rect.bottom())),
                    headers,
                    all_day: Rect::from_min_max(
                        pos2(rect.left() + gutter, headers.top() + DAY_HEADER),
                        pos2(rect.right(), headers.bottom()),
                    ),
                    bodies,
                    columns: Rect::from_min_max(
                        pos2(rect.left() + gutter, bodies.top()),
                        bodies.max,
                    ),
                }
            }
        };
        self.scroll_to_start(rows.len(), frame.bodies.height());
        self.scroll(ui, &frame);
        let painter = ui.painter_at(rect);
        let palette = exo_ui::palette(ui.ctx());
        let hovered = self
            .hovered_minute(ui, &frame)
            .filter(|_| grid.mode.shows_hover_indicator() && self.drag.is_none());
        let mut actions = Vec::new();
        if self.paint_timezone(ui, &painter, &frame, grid) {
            actions.push(GridAction::ZoneSettings);
        }
        if variant == Variant::Week {
            paint::paint_day_headers(&painter, &frame, grid);
        }
        // Find scrolls the hours to a match out of view, in the axis or an event.
        let find = ui.id().with(("calendar-find", variant == Variant::Week));
        let scope = exo_ui::find::Scope::begin(ui.ctx(), find, frame.bodies, self.scroll);
        self.paint_axis(&painter, &frame, grid, hovered);
        drop(scope);
        // Under the events, which take their own clicks.
        let band = Rect::from_min_max(
            frame.all_day.min,
            pos2(frame.all_day.right(), frame.all_day.bottom()),
        );
        let all_day_background = ui.interact(
            band,
            ui.id()
                .with(("calendar-all-day-background", variant == Variant::Week)),
            Sense::click_and_drag(),
        );
        let body = Rect::from_min_max(
            frame.columns.min,
            pos2(frame.columns.right() - 1.0, frame.columns.bottom()),
        );
        let background = ui.interact(
            body,
            ui.id()
                .with(("calendar-background", variant == Variant::Week)),
            Sense::click_and_drag(),
        );
        self.drag_background(
            ui,
            &frame,
            grid,
            &background,
            &all_day_background,
            &mut actions,
        );
        match &attendee_rows {
            Some(days) => {
                self.paint_attendee_all_day(ui, &painter, &frame, grid, days, &mut actions)
            }
            None => self.paint_all_day(ui, &painter, &frame, grid, &rows, &mut actions),
        }
        let scope = exo_ui::find::Scope::begin(ui.ctx(), find, frame.bodies, self.scroll);
        self.paint_columns(ui, &frame, grid, hovered, &mut actions);
        drop(scope);
        if variant == Variant::Week && self.draft_anchor.is_none() {
            self.draft_anchor = self.hidden_day_anchor(&frame, grid);
        }
        self.expand_column(ui, &mut actions);
        self.follow_drag(ui, &frame, grid, &mut actions);
        self.scrollbar.show(
            ui,
            ui.id()
                .with(("calendar-scrollbar", variant == Variant::Week)),
            frame.columns,
            self.scroll,
            DAY_HEIGHT,
        );
        if variant == Variant::Pane {
            // Hairlines at the top and bottom, `width: calc(100% - 55px); right: 1rem`, fading in
            // over their first 16px.
            for y in [rect.top(), rect.bottom() - 1.0] {
                let line = Rect::from_min_max(
                    pos2(rect.right() - rem - (rect.width() - 55.0), y),
                    pos2(rect.right() - rem, y + 1.0),
                );
                paint::paint_fade_in(&painter, line, 16.0, palette.border_base_muted);
            }
        }
        actions
    }

    /// Where the form goes for a draft on a weekend day the week leaves out: a line at the
    /// columns' left edge for a day before the first shown, else at their right edge, from the
    /// draft's start time to its end time (none past midnight).
    fn hidden_day_anchor(&self, frame: &Frame, grid: &Grid<'_>) -> Option<Rect> {
        let draft = grid.mode.draft()?;
        let day = draft.start.date();
        let weekend = matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday);
        if !weekend || grid.days.contains(&day) {
            return None;
        }
        let x = if grid.days.first().is_some_and(|first| day < *first) {
            frame.columns.left()
        } else {
            frame.columns.right()
        };
        let fraction = |at: &Zoned| {
            at.time()
                .duration_since(jiff::civil::Time::midnight())
                .as_secs_f32()
                / 86_400.0
        };
        let start = fraction(&draft.start);
        let top = frame.columns.top() - self.scroll + start * DAY_HEIGHT;
        let height = (fraction(&draft.end) - start).max(0.0) * DAY_HEIGHT;
        Some(Rect::from_x_y_ranges(x..=x, top..=top + height))
    }

    /// The initial scroll: about 6:30 am at the top, lower by a row's height per all-day row (up
    /// to two and a half).
    fn scroll_to_start(&mut self, rows: usize, viewport: f32) {
        let rows = rows.clamp(1, 3);
        if self.scrolled_for == Some(rows) {
            return;
        }
        self.scrolled_for = Some(rows);
        let target = DAY_HEIGHT * (6.5 / 24.0) + 2.0 + 24.0 * (rows as f32).min(2.5);
        self.scroll = target.min((DAY_HEIGHT - viewport).max(0.0));
        if let Some(hour) = self.reveal.take()
            && f32::from(hour) / 24.0 * DAY_HEIGHT > target + viewport
        {
            self.scroll = (DAY_HEIGHT - viewport).max(0.0);
        }
    }

    /// The wheel over the bodies scrolls the columns and the time axis together, as find does to
    /// bring a match into view.
    fn scroll(&mut self, ui: &Ui, frame: &Frame) {
        if ui.rect_contains_pointer(frame.bodies) {
            self.scroll -= ui.input(|input| input.smooth_scroll_delta.y);
        }
        let find = ui
            .id()
            .with(("calendar-find", frame.variant == Variant::Week));
        if let Some(delta) = exo_ui::find::reveal(ui.ctx(), find) {
            self.scroll += delta.y;
        }
        let scale = ui.ctx().pixels_per_point();
        self.scroll = ((self
            .scroll
            .clamp(0.0, (DAY_HEIGHT - frame.bodies.height()).max(0.0)))
            * scale)
            .round()
            / scale;
    }

    /// The quarter hour under the pointer, in minutes from midnight, but not midnight at either
    /// end.
    fn hovered_minute(&self, ui: &Ui, frame: &Frame) -> Option<i32> {
        // Not through a popover or toast above the grid.
        let pointer = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|_| ui.rect_contains_pointer(frame.bodies))?;
        let top = frame.columns.top() - self.scroll;
        let minutes = (((pointer.y - top) / DAY_HEIGHT * 1440.0 / 15.0).round() * 15.0)
            .clamp(0.0, 1440.0) as i32;
        (minutes > 0 && minutes < 1440).then_some(minutes)
    }

    /// Notes the guests' column in `rect` (`guest`'s, or mine) when the pointer is over it there
    /// and in `within`, for [`Self::expand_column`].
    pub(super) fn hover_column(
        &mut self,
        ui: &Ui,
        within: Rect,
        rect: Rect,
        column: (Date, Option<String>),
    ) {
        // Nothing is over the grid there. The week's own area doesn't count as covering it.
        let uncovered = |at: egui::Pos2| {
            ui.ctx().layer_id_at(at).is_none_or(|layer| {
                layer == ui.layer_id() || layer.order == egui::Order::Background
            })
        };
        if ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|at| rect.contains(at) && within.contains(at) && uncovered(at))
            && self.drag.is_none()
        {
            self.column_under = Some(column);
        }
    }

    /// Expands the guests' column the pointer has settled on (100ms polls, under 7px between two),
    /// once each time it does.
    fn expand_column(&mut self, ui: &Ui, actions: &mut Vec<GridAction>) {
        if let Some((_, guest)) = self
            .column_intent
            .update(ui.ctx(), self.column_under.take())
        {
            actions.push(GridAction::Expand(guest.clone()));
        }
    }
}

/// The events drawn: all but the one being edited, then the draft or the dragged copy.
pub(super) fn shown<'a>(grid: &Grid<'a>, extra: Option<&'a Event>) -> Vec<&'a Event> {
    let dragging_draft = extra.is_some_and(|extra| extra.key == DRAFT);
    let hidden = grid.mode.hidden();
    let mut events: Vec<&Event> = grid
        .events
        .iter()
        .filter(|event| Some(event.key.as_str()) != hidden)
        .collect();
    if !dragging_draft {
        events.extend(grid.mode.draft());
    }
    events.extend(extra);
    events
}

/// `day`'s minutes `from` to `to` as times.
pub(super) fn span(grid: &Grid<'_>, day: usize, from: i32, to: i32) -> Option<(Zoned, Zoned)> {
    let midnight = grid.days.get(day)?.to_zoned(grid.tz.clone()).ok()?;
    Some((
        midnight.checked_add(i64::from(from).minutes()).ok()?,
        midnight.checked_add(i64::from(to).minutes()).ok()?,
    ))
}

/// The days `first` to `last` as an all-day span: midnight to the midnight after.
pub(super) fn days_span(grid: &Grid<'_>, first: usize, last: usize) -> Option<(Zoned, Zoned)> {
    let start = grid.days.get(first)?.to_zoned(grid.tz.clone()).ok()?;
    let end = grid
        .days
        .get(last)?
        .tomorrow()
        .ok()?
        .to_zoned(grid.tz.clone())
        .ok()?;
    Some((start, end))
}

/// A new event being dragged out, drawn as the draft is.
pub(super) fn draft_like(grid: &Grid<'_>, start: Zoned, end: Zoned, all_day: bool) -> Event {
    let color = grid
        .mode
        .draft()
        .map_or(exo_ui::CalendarColor::BLUE, |draft| draft.color);
    Event::draft(
        DRAFT.to_owned(),
        "(No title)".to_owned(),
        start,
        end,
        all_day,
        color,
    )
}

/// The week's gutter: wider with secondary time zones, and wider still when one is off the
/// hour from this one on the week's first day.
fn week_gutter(grid: &Grid<'_>) -> f32 {
    if grid.zones.is_empty() {
        return WEEK_GUTTER;
    }
    let at = grid
        .days
        .first()
        .and_then(|day| day.to_zoned(grid.tz.clone()).ok())
        .map(|day| day.timestamp());
    let local = at.map_or(0, |at| grid.tz.to_offset(at).seconds());
    let off_hour = grid
        .zones
        .iter()
        .any(|zone| at.is_some_and(|at| (zone.to_offset(at).seconds() - local) % 3600 != 0));
    if off_hour {
        MINUTE_ZONES_GUTTER
    } else {
        ZONES_GUTTER
    }
}

/// An hour label: `9 am` on the hour, `9:15 am` otherwise.
pub fn hour_label(hour: i32, minute: i32) -> String {
    let (twelve, half) = match hour.rem_euclid(24) {
        0 => (12, "am"),
        hour @ 1..=11 => (hour, "am"),
        12 => (12, "pm"),
        hour => (hour - 12, "pm"),
    };
    if minute == 0 {
        format!("{twelve} {half}")
    } else {
        format!("{twelve}:{minute:02} {half}")
    }
}

/// Shifts `(start, end)` by civil days so the day grabbed at `grab_days` after `start.date()`
/// lands on `target_day`, preserving local wall-clock / midnight across 23h and 25h DST days.
pub(super) fn shift_span_days(
    (start, end): &(Zoned, Zoned),
    target_day: Date,
    grab_days: i64,
) -> Option<(Zoned, Zoned)> {
    let first_date = target_day.checked_sub(grab_days.days()).ok()?;
    let delta = i64::from(start.date().until(first_date).ok()?.get_days());
    Some((
        start.checked_add(delta.days()).ok()?,
        end.checked_add(delta.days()).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_band_uses_in_all_day_row_and_civil_day_shifts_survive_dst() {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let z = |text: &str| {
            text.parse::<jiff::civil::DateTime>()
                .expect("civil")
                .to_zoned(tz.clone())
                .expect("zoned")
        };
        let long_timed = Event::draft(
            "long".into(),
            "Hackathon".into(),
            z("2026-12-09T09:00:00"),
            z("2026-12-10T11:00:00"),
            false,
            exo_ui::CalendarColor::BLUE,
        );
        assert!(!long_timed.all_day && long_timed.in_all_day_row());
        let days = [Date::constant(2026, 12, 9), Date::constant(2026, 12, 10)];
        let now = z("2026-12-09T09:00:00");
        let grid = Grid {
            days: &days,
            events: &[long_timed],
            now: &now,
            tz: &tz,
            invite: None,
            mode: GridMode::Browse,
            guests: &[],
            expanded: None,
            mine: true,
            zones: &[],
        };
        let rows = paint::all_day_rows(&grid, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].days, 2);

        // Moving a 1-day all-day event off a 23h spring-forward day (2026-03-08) onto 2026-03-09
        // keeps midnight-to-midnight instead of ending at 23:00.
        let spring = (z("2026-03-08T00:00:00"), z("2026-03-09T00:00:00"));
        let (moved_start, moved_end) =
            shift_span_days(&spring, Date::constant(2026, 3, 9), 0).expect("shifts");
        assert_eq!(moved_start, z("2026-03-09T00:00:00"));
        assert_eq!(moved_end, z("2026-03-10T00:00:00"));
    }

    #[test]
    fn a_secondary_zone_widens_the_gutter_more_when_off_the_hour() {
        let new_york = TimeZone::get("America/New_York").expect("zone");
        let now = Date::constant(2026, 11, 17)
            .at(9, 0, 0, 0)
            .to_zoned(new_york.clone())
            .expect("a time");
        let days = [Date::constant(2026, 11, 15)];
        let gutter = |zones: &[TimeZone]| {
            let grid = Grid {
                days: &days,
                events: &[],
                now: &now,
                tz: &new_york,
                invite: None,
                mode: GridMode::Browse,
                guests: &[],
                expanded: None,
                mine: true,
                zones,
            };
            week_gutter(&grid)
        };
        assert_eq!(gutter(&[]), WEEK_GUTTER);
        assert_eq!(
            gutter(&[TimeZone::get("Europe/London").expect("zone")]),
            ZONES_GUTTER
        );
        assert_eq!(
            gutter(&[TimeZone::get("Asia/Kolkata").expect("zone")]),
            MINUTE_ZONES_GUTTER
        );
    }

    #[test]
    fn hour_labels_show_minutes_only_off_the_hour() {
        assert_eq!(hour_label(8, 0), "8 am");
        assert_eq!(hour_label(12, 0), "12 pm");
        assert_eq!(hour_label(13, 15), "1:15 pm");
        assert_eq!(hour_label(0, 45), "12:45 am");
    }

    #[test]
    fn grid_mode_encodes_mutually_exclusive_states() {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let start = Date::constant(2026, 11, 17)
            .at(10, 0, 0, 0)
            .to_zoned(tz.clone())
            .expect("start");
        let end = Date::constant(2026, 11, 17)
            .at(10, 30, 0, 0)
            .to_zoned(tz)
            .expect("end");
        let draft = Event::draft(
            DRAFT.into(),
            "Sync".into(),
            start,
            end,
            false,
            exo_ui::CalendarColor::BLUE,
        );

        let browse = GridMode::Browse;
        assert!(browse.shows_hover_indicator());
        assert!(browse.allows_zone_settings());
        assert!(browse.selects_events());
        assert_eq!(browse.selected(), None);

        let details = GridMode::DetailsOpen { selected: "ev-1" };
        assert!(!details.shows_hover_indicator());
        assert!(details.allows_zone_settings());
        assert_eq!(details.selected(), Some("ev-1"));

        let editing = GridMode::EditingDraft {
            draft: Some(&draft),
            hidden: Some("ev-1"),
        };
        assert!(editing.shows_hover_indicator());
        assert!(!editing.allows_zone_settings());
        assert!(!editing.selects_events());
        assert_eq!(editing.hidden(), Some("ev-1"));
        assert!(editing.draft().is_some());

        let finding = GridMode::FindTime { selected: None };
        assert!(!finding.shows_hover_indicator());
        assert!(finding.is_find_time());

        let sharing = GridMode::ShareAvailability {
            minutes: 45,
            selected: None,
        };
        assert!(sharing.shows_hover_indicator());
        assert!(!sharing.allows_zone_settings());
        assert_eq!(sharing.sharing_minutes(), Some(45));
    }
}
