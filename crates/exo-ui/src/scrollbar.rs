//! The scrolling panes' overlay scrollbar, a thumb that shows while the pane scrolls or the pointer
//! is over its 11px track, and fades out .2s after a 1s pause.

use egui::{Color32, CornerRadius, Id, Rect, Ui, pos2, vec2};

use crate::motion::{self, CubicBezier};

/// The track is 11px along the pane's edge.
const TRACK: f32 = 11.0;

/// A pane's scrollbar, remembering when it last scrolled.
#[derive(Clone, Copy, Debug)]
pub struct Scrollbar {
    scrolled_at: f64,
    scrolled_to: f32,
}

impl Default for Scrollbar {
    fn default() -> Self {
        Self {
            scrolled_at: f64::NEG_INFINITY,
            scrolled_to: 0.0,
        }
    }
}

impl Scrollbar {
    /// Paints the thumb over the right edge of `viewport` for a pane scrolled to `scroll` of
    /// `content` px.
    pub fn show(&mut self, ui: &Ui, id: Id, viewport: Rect, scroll: f32, content: f32) {
        self.paint(ui, id, viewport, scroll, content, false);
    }

    /// Paints the thumb over the bottom edge of `viewport` for a pane scrolled sideways to
    /// `scroll` of `content` px.
    pub fn show_horizontal(&mut self, ui: &Ui, id: Id, viewport: Rect, scroll: f32, content: f32) {
        self.paint(ui, id, viewport, scroll, content, true);
    }

    fn paint(
        &mut self,
        ui: &Ui,
        id: Id,
        viewport: Rect,
        scroll: f32,
        content: f32,
        horizontal: bool,
    ) {
        let now = ui.input(|input| input.time);
        if scroll != self.scrolled_to {
            self.scrolled_to = scroll;
            self.scrolled_at = now;
        }
        let length = if horizontal {
            viewport.width()
        } else {
            viewport.height()
        };
        if content <= length {
            return;
        }
        let track = if horizontal {
            Rect::from_min_max(
                pos2(viewport.left(), viewport.bottom() - TRACK),
                viewport.max,
            )
        } else {
            Rect::from_min_max(pos2(viewport.right() - TRACK, viewport.top()), viewport.max)
        };
        let recently_scrolled = now < self.scrolled_at + 1.0;
        let shown = ui.rect_contains_pointer(track) || recently_scrolled;
        let opacity = motion::transition(
            ui.ctx(),
            id,
            shown,
            if shown { 0.0 } else { 0.2 },
            CubicBezier::LINEAR,
        );
        if opacity <= 0.0 {
            return;
        }
        if recently_scrolled {
            ui.ctx()
                .request_repaint_after_secs((self.scrolled_at + 1.0 - now) as f32);
        }
        // At least 25px long, inset 2px, black at .5 with `border-radius: 7px`.
        let thumb_length = (length * length / content).max(25.0);
        let start = scroll / (content - length) * (length - thumb_length);
        let thumb = if horizontal {
            Rect::from_min_size(
                pos2(track.left() + start, track.top()),
                vec2(thumb_length, TRACK),
            )
        } else {
            Rect::from_min_size(
                pos2(track.left(), track.top() + start),
                vec2(TRACK, thumb_length),
            )
        };
        ui.painter().rect_filled(
            thumb.shrink(2.0),
            CornerRadius::same(4),
            Color32::from_black_alpha((0.5 * opacity * 255.0).round() as u8),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use egui::{Context, Event, RawInput};

    use super::*;

    #[test]
    fn hovering_the_track_without_scrolling_schedules_no_timer_repaint() {
        let ctx = Context::default();
        let mut bar = Scrollbar::default();
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        // Move the pointer onto the 11px track and drain egui's immediate pointer-move follow-up
        // pass; once resting over the track, no timer repaint should be scheduled.
        for (time, events) in [
            (5.0, vec![Event::PointerMoved(pos2(195.0, 50.0))]),
            (5.016, Vec::new()),
        ] {
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    bar.show(ui, Id::new("bar"), viewport, 0.0, 400.0);
                },
            )
            .drop_without_applying_deltas();
        }

        let mut output = ctx.run_ui(
            RawInput {
                time: Some(5.1),
                ..Default::default()
            },
            |ui| {
                bar.show(ui, Id::new("bar"), viewport, 0.0, 400.0);
            },
        );
        let shapes = std::mem::take(&mut output.shapes);
        let repaint_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.drop_without_applying_deltas();
        assert!(!shapes.is_empty(), "the hovered thumb paints");
        assert_eq!(
            repaint_delay,
            Duration::MAX,
            "hovering an idle scrollbar schedules no timer repaint"
        );

        // Scrolling schedules a timer for the remaining 1s post-scroll hold once the pointer
        // event's immediate follow-up pass has settled.
        ctx.run_ui(
            RawInput {
                time: Some(6.0),
                events: vec![Event::PointerMoved(pos2(10.0, 50.0))],
                ..Default::default()
            },
            |ui| {
                bar.show(ui, Id::new("bar"), viewport, 40.0, 400.0);
            },
        )
        .drop_without_applying_deltas();
        ctx.run_ui(
            RawInput {
                time: Some(6.05),
                ..Default::default()
            },
            |ui| {
                bar.show(ui, Id::new("bar"), viewport, 40.0, 400.0);
            },
        )
        .drop_without_applying_deltas();
        let output = ctx.run_ui(
            RawInput {
                time: Some(6.1),
                ..Default::default()
            },
            |ui| {
                bar.show(ui, Id::new("bar"), viewport, 40.0, 400.0);
            },
        );
        let repaint_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.drop_without_applying_deltas();
        assert!(
            (repaint_delay.as_secs_f64() - 0.9).abs() < 0.02,
            "expected ~0.9s remaining hold timer, got {repaint_delay:?}"
        );
    }
}
