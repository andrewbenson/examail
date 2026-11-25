use egui::{Rect, Ui, Vec2};
use jiff::{SignedDuration, ToSpan};

use super::{
    DAY_HEIGHT, DRAFT, Drag, FREE_SLOT, Frame, Grid, GridAction, GridMode, GridView, SLOT,
    days_span, draft_like, shift_span_days, span,
};
use crate::calendar::{event::Event, layout::HOUR_HEIGHT};

/// The most a block I can't change moves when dragged.
const RESTRICTED_DRAG: f32 = 11.0;
/// While dragging this close to the top or bottom, the grid scrolls a quarter hour every 15 ms.
const AUTO_SCROLL_EDGE: f32 = 20.0;
const AUTO_SCROLL_STEP: f32 = SLOT / 0.015;
/// A block's bottom edge, which resizes it (6px for exactly half an hour, 4px under it).
pub(super) const RESIZE_EDGE: f32 = 8.0;

/// How far a block I can't change follows the pointer along `axis`: half the way, at most 11px.
/// It's back in place when let go.
pub(super) fn jiggle(
    ui: &Ui,
    response: &egui::Response,
    event: &Event,
    axis: Vec2,
    actions: &mut Vec<GridAction>,
) -> Vec2 {
    if event.editable {
        return Vec2::ZERO;
    }
    if response.drag_started() {
        actions.push(GridAction::Restricted);
    }
    if !response.dragged() {
        return Vec2::ZERO;
    }
    let moved = ui
        .input(|input| {
            input
                .pointer
                .press_origin()
                .zip(input.pointer.interact_pos())
                .map(|(from, to)| to - from)
        })
        .unwrap_or_default();
    axis * (moved.dot(axis) / 2.0).clamp(-RESTRICTED_DRAG, RESTRICTED_DRAG)
}

impl GridView {
    /// The day column under `x` and the minutes from its midnight at `y` (unsnapped).
    pub(super) fn time_at(&self, frame: &Frame, grid: &Grid<'_>, at: egui::Pos2) -> (usize, f32) {
        let count = grid.days.len().max(1);
        let width = frame.columns.width() / count as f32;
        let day =
            (((at.x - frame.columns.left()) / width).floor().max(0.0) as usize).min(count - 1);
        let minutes = (at.y - (frame.columns.top() - self.scroll)) / DAY_HEIGHT * 1440.0;
        (day, minutes.clamp(0.0, 1440.0))
    }

    /// Clicks and drags on empty grid make a 30-minute event at the quarter hour clicked, or the
    /// range dragged out, and in the all-day row they make all-day events. With the details up, a
    /// click only closes them.
    pub(super) fn drag_background(
        &mut self,
        ui: &Ui,
        frame: &Frame,
        grid: &Grid<'_>,
        timed: &egui::Response,
        all_day: &egui::Response,
        actions: &mut Vec<GridAction>,
    ) {
        match grid.mode {
            GridMode::FindTime { .. } => return,
            GridMode::DetailsOpen { .. }
            | GridMode::ShareAvailability {
                selected: Some(_), ..
            } => {
                if timed.clicked()
                    || all_day.clicked()
                    || timed.drag_started()
                    || all_day.drag_started()
                {
                    actions.push(GridAction::Background);
                }
                return;
            }
            GridMode::Browse
            | GridMode::EditingDraft { .. }
            | GridMode::ShareAvailability { selected: None, .. } => {}
        }
        let sharing = grid.mode.sharing_minutes();
        let snap = |minutes: f32| ((minutes / 15.0).round() * 15.0) as i32;
        // Picking free times refuses a press before now.
        let past = |day: usize, from: i32| {
            sharing.is_some() && span(grid, day, from, from).is_some_and(|(at, _)| at < *grid.now)
        };
        if let Some(at) = timed.interact_pointer_pos() {
            let (day, minutes) = self.time_at(frame, grid, at);
            if timed.clicked() {
                let length = sharing.map_or(30, |minutes| minutes as i32);
                let from = snap(minutes).min(1440 - length);
                if past(day, snap(minutes)) {
                    actions.push(GridAction::Past);
                } else if let Some((start, end)) = span(grid, day, from, from + length) {
                    actions.push(GridAction::Slot {
                        start,
                        end,
                        all_day: false,
                    });
                }
            } else if timed.drag_started() {
                // Starts at the quarter pressed, 15 minutes long.
                let origin = ui.input(|input| input.pointer.press_origin()).unwrap_or(at);
                let (day, minutes) = self.time_at(frame, grid, origin);
                let from = snap(minutes).min(1440 - 15);
                if past(day, snap(minutes)) {
                    actions.push(GridAction::Past);
                } else {
                    self.drag = Some(Drag::New {
                        day,
                        from,
                        to: from + 15,
                    });
                }
            }
        }
        // Free times are picked in the hours, not the all-day row.
        if sharing.is_some() {
            return;
        }
        if let Some(at) = all_day.interact_pointer_pos() {
            let (day, _) = self.time_at(frame, grid, at);
            if all_day.clicked() {
                if let Some((start, end)) = days_span(grid, day, day) {
                    actions.push(GridAction::Slot {
                        start,
                        end,
                        all_day: true,
                    });
                }
            } else if all_day.drag_started() {
                self.drag = Some(Drag::NewAllDay { from: day, to: day });
            }
        }
    }

    /// While dragging: follows the pointer (scrolling the grid near its edges), and on release
    /// says what came of it.
    pub(super) fn follow_drag(
        &mut self,
        ui: &Ui,
        frame: &Frame,
        grid: &Grid<'_>,
        actions: &mut Vec<GridAction>,
    ) {
        let Some(drag) = self.drag.clone() else {
            self.scrolled_at = None;
            return;
        };
        let (pointer, released, time) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.any_released() || !input.pointer.any_down(),
                input.time,
            )
        });
        let Some(pointer) = pointer else {
            if released {
                self.drag = None;
            }
            self.scrolled_at = None;
            return;
        };
        if !matches!(drag, Drag::NewAllDay { .. }) {
            let direction = if pointer.y < frame.bodies.top() + AUTO_SCROLL_EDGE {
                -1.0
            } else if pointer.y > frame.bodies.bottom() - AUTO_SCROLL_EDGE {
                1.0
            } else {
                0.0
            };
            if direction != 0.0 {
                let elapsed = self.scrolled_at.map_or(0.0, |at| (time - at) as f32);
                self.scroll = (self.scroll + direction * AUTO_SCROLL_STEP * elapsed)
                    .clamp(0.0, (DAY_HEIGHT - frame.bodies.height()).max(0.0));
                self.scrolled_at = Some(time);
                ui.ctx().request_repaint();
            } else {
                self.scrolled_at = None;
            }
        }
        let snap = |minutes: f32| ((minutes / 15.0).round() * 15.0) as i32;
        let (day, minutes) = self.time_at(frame, grid, pointer);
        let updated = match drag {
            // While sharing, the pointer before now is ignored.
            Drag::New {
                day: first,
                from,
                to,
            } if grid.mode.sharing_minutes().is_some()
                && span(grid, first, snap(minutes).min(from), from)
                    .is_some_and(|(at, _)| at < *grid.now) =>
            {
                Drag::New {
                    day: first,
                    from,
                    to,
                }
            }
            Drag::New {
                day: first, from, ..
            } => Drag::New {
                day: first,
                from,
                to: snap(minutes),
            },
            Drag::NewAllDay { from, .. } => Drag::NewAllDay { from, to: day },
            Drag::Event {
                key,
                resize,
                span_row,
                grab_days,
                grab,
                start,
                end,
                original,
            } => {
                let length = end.timestamp().duration_since(start.timestamp());
                let (start, end) = if span_row {
                    grid.days
                        .get(day)
                        .copied()
                        .and_then(|target| shift_span_days(&original, target, grab_days))
                        .unwrap_or((start, end))
                } else if resize {
                    let at = span(grid, day, snap(minutes), snap(minutes)).map(|(at, _)| at);
                    let floor = start.checked_add(15.minutes()).ok();
                    let end = at
                        .zip(floor)
                        .map(|(at, floor)| if at < floor { floor } else { at })
                        .unwrap_or(end);
                    (start, end)
                } else {
                    let pointer_time = days_span(grid, day, day).and_then(|(midnight, _)| {
                        midnight
                            .checked_add(SignedDuration::from_secs_f64(f64::from(minutes) * 60.0))
                            .ok()
                    });
                    let moved = pointer_time
                        .and_then(|at| at.checked_sub(grab).ok())
                        .and_then(|at| {
                            let midnight = at.date().to_zoned(at.time_zone().clone()).ok()?;
                            let minutes = at
                                .timestamp()
                                .duration_since(midnight.timestamp())
                                .as_secs_f64()
                                / 60.0;
                            midnight
                                .checked_add(((minutes / 15.0).round() as i64 * 15).minutes())
                                .ok()
                        });
                    moved
                        .and_then(|start| start.checked_add(length).ok().map(|end| (start, end)))
                        .unwrap_or((start, end))
                };
                Drag::Event {
                    key,
                    resize,
                    span_row,
                    grab_days,
                    grab,
                    start,
                    end,
                    original,
                }
            }
        };
        self.drag = Some(updated.clone());
        if !released {
            return;
        }
        self.drag = None;
        self.scrolled_at = None;
        match updated {
            Drag::New { day, from, to } => {
                // Either way from where it started, a quarter hour at least.
                let (low, high) = if to < from {
                    (to, from + 15)
                } else {
                    (from, to.max(from + 15))
                };
                if let Some((start, end)) = span(grid, day, low, high.min(1440)) {
                    actions.push(GridAction::Slot {
                        start,
                        end,
                        all_day: false,
                    });
                }
            }
            Drag::NewAllDay { from, to } => {
                if let Some((start, end)) = days_span(grid, from.min(to), from.max(to)) {
                    actions.push(GridAction::Slot {
                        start,
                        end,
                        all_day: true,
                    });
                }
            }
            Drag::Event {
                key,
                start,
                end,
                original,
                ..
            } => {
                if (start.clone(), end.clone()) != original {
                    actions.push(GridAction::Change { key, start, end });
                }
            }
        }
    }

    /// The drag's event (the draft, or one being moved) as it's drawn now, if it's on screen.
    pub(super) fn dragged(&self, grid: &Grid<'_>) -> Option<Event> {
        match self.drag.as_ref()? {
            Drag::New { day, from, to } => {
                let (low, high) = if to < from {
                    (*to, from + 15)
                } else {
                    (*from, (*to).max(from + 15))
                };
                let (start, end) = span(grid, *day, low, high.min(1440))?;
                Some(draft_like(grid, start, end, false))
            }
            Drag::NewAllDay { from, to } => {
                let (start, end) = days_span(grid, *from.min(to), *from.max(to))?;
                Some(draft_like(grid, start, end, true))
            }
            Drag::Event {
                key, start, end, ..
            } => {
                let event = grid
                    .events
                    .iter()
                    .chain(grid.mode.draft())
                    .find(|event| event.key == *key)?;
                let span_row = event.all_day
                    || end.timestamp().duration_since(start.timestamp()).as_hours() >= 24;
                Some(Event {
                    span_row,
                    start: start.clone(),
                    end: end.clone(),
                    ..event.clone()
                })
            }
        }
    }

    /// Starts moving or resizing `event` when its block's drag starts. A double click opens its
    /// form.
    pub(super) fn take_event(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        event: &Event,
        rect: Rect,
        actions: &mut Vec<GridAction>,
    ) {
        if !event.editable || event.key.starts_with(FREE_SLOT) {
            return;
        }
        if response.double_clicked() && event.key != DRAFT && event.share.is_none() {
            actions.push(GridAction::Open(event.key.clone()));
        }
        if !response.drag_started() || self.drag.is_some() {
            return;
        }
        let Some(origin) = ui.input(|input| input.pointer.press_origin()) else {
            return;
        };
        let duration = event
            .end
            .timestamp()
            .duration_since(event.start.timestamp());
        let edge = if duration < SignedDuration::from_mins(30) {
            4.0
        } else if duration == SignedDuration::from_mins(30) {
            6.0
        } else {
            RESIZE_EDGE
        };
        let span_row = event.in_all_day_row();
        let resize = !span_row && origin.y > rect.bottom() - edge;
        // Where on the event it was taken: the time under the pointer after its start, or for an
        // all-day / day-spanning block, the civil day offset.
        let (grab_days, grab) = if span_row {
            let exclusive_end = if event.end.hour() == 0 && event.end.minute() == 0 {
                event.end.date()
            } else {
                event.end.date().tomorrow().unwrap_or(event.end.date())
            };
            let covered_days = event
                .start
                .date()
                .until(exclusive_end)
                .map_or(1, |span| i64::from(span.get_days()))
                .max(1);
            let width = rect.width() / (covered_days as f32);
            (
                ((origin.x - rect.left()) / width.max(1.0)).floor() as i64,
                SignedDuration::ZERO,
            )
        } else {
            (
                0,
                SignedDuration::from_secs_f64(
                    f64::from((origin.y - rect.top()) / HOUR_HEIGHT) * 3600.0,
                ),
            )
        };
        self.drag = Some(Drag::Event {
            key: event.key.clone(),
            resize,
            span_row,
            grab_days,
            grab,
            start: event.start.clone(),
            end: event.end.clone(),
            original: (event.start.clone(), event.end.clone()),
        });
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event as EguiEvent, Modifiers, PointerButton, RawInput, pos2, vec2};
    use jiff::{civil::Date, tz::TimeZone};

    use super::*;
    use crate::calendar::grid::Variant;

    fn test_frame() -> Frame {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 600.0));
        Frame {
            variant: Variant::Week,
            gutter: Rect::from_min_size(pos2(0.0, 0.0), vec2(64.0, 600.0)),
            headers: Rect::from_min_size(pos2(64.0, 0.0), vec2(336.0, 60.0)),
            all_day: Rect::from_min_size(pos2(64.0, 60.0), vec2(336.0, 30.0)),
            bodies: rect,
            columns: Rect::from_min_size(pos2(64.0, 90.0), vec2(336.0, DAY_HEIGHT)),
        }
    }

    fn run_follow(
        ctx: &egui::Context,
        input: RawInput,
        view: &mut GridView,
        actions: &mut Vec<GridAction>,
    ) {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let day = Date::constant(2026, 11, 18);
        let now = day.at(9, 0, 0, 0).to_zoned(tz.clone()).expect("now");
        let days = [day];
        let grid = Grid {
            days: &days,
            events: &[],
            now: &now,
            tz: &tz,
            invite: None,
            mode: GridMode::Browse,
            guests: &[],
            expanded: None,
            mine: true,
            zones: &[],
        };
        let frame = test_frame();
        let mut out = ctx.run_ui(input, |ui| {
            view.follow_drag(ui, &frame, &grid, actions);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn releasing_drag_outside_window_clears_drag_state() {
        let ctx = egui::Context::default();
        let mut view = GridView {
            drag: Some(Drag::New {
                day: 0,
                from: 9 * 60,
                to: 10 * 60,
            }),
            scrolled_at: Some(1.0),
            ..GridView::default()
        };
        let mut actions = Vec::new();

        // Pointer leaves window and button is not held (released outside window).
        run_follow(
            &ctx,
            RawInput {
                events: vec![EguiEvent::PointerGone],
                ..RawInput::default()
            },
            &mut view,
            &mut actions,
        );
        assert!(
            view.drag.is_none(),
            "drag cleared on release outside window"
        );
        assert!(view.scrolled_at.is_none(), "auto-scroll timestamp cleared");
        assert!(actions.is_empty(), "outside release cancels cleanly");
    }

    #[test]
    fn releasing_drag_inside_window_still_commits_slot() {
        let ctx = egui::Context::default();
        let mut view = GridView {
            drag: Some(Drag::New {
                day: 0,
                from: 9 * 60,
                to: 9 * 60 + 15,
            }),
            ..GridView::default()
        };
        let mut actions = Vec::new();
        let release_pos = pos2(120.0, 90.0 + 10.0 * HOUR_HEIGHT);

        run_follow(
            &ctx,
            RawInput {
                events: vec![
                    EguiEvent::PointerMoved(release_pos),
                    EguiEvent::PointerButton {
                        pos: release_pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..RawInput::default()
            },
            &mut view,
            &mut actions,
        );
        assert!(view.drag.is_some(), "still dragging while pressed");

        run_follow(
            &ctx,
            RawInput {
                events: vec![EguiEvent::PointerButton {
                    pos: release_pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                ..RawInput::default()
            },
            &mut view,
            &mut actions,
        );
        assert!(view.drag.is_none(), "drag cleared on release");
        assert_eq!(actions.len(), 1, "slot committed on inside release");
        assert!(matches!(
            actions[0],
            GridAction::Slot { all_day: false, .. }
        ));
    }
}
