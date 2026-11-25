//! Scroll positioning, keyboard paging/stepping, and scroll animations in the open thread.

use std::collections::HashMap;

use egui::{Rect, Ui};
use exo_ui::motion::CubicBezier;

use super::{
    Action, Layout, MESSAGES_MARGIN_TOP, MESSAGES_PADDING_TOP, Placed, STEP, ThreadView, header,
    highlight, message, overflow,
};
use crate::body::Bodies;

/// Four steps: where a message lands below the top when scrolled to, and how far ↑/↓ scroll
/// within one.
const IDEAL_TOP: f32 = 4.0 * STEP;
/// 16px: how far a message may poke out before ↑/↓ or N/P reposition.
const SLACK: f32 = 16.0;
/// The keyboard's scroll animation, an estimate.
const SCROLL_DURATION: f32 = 0.2;
/// Opening a draft scrolls to it over 500ms.
const DRAFT_SCROLL_DURATION: f32 = 0.5;
/// While typing, a draft's bottom stays this far above the messages'.
const DRAFT_MARGIN_BOTTOM: f32 = 28.0;

/// An animated scroll.
#[derive(Clone, Copy, Debug)]
pub(super) struct ScrollAnimation {
    pub(super) from: f32,
    pub(super) to: f32,
    pub(super) started: f64,
    pub(super) duration: f32,
}

/// Scroll offsets, wide-column widths, animation, viewport/content geometry, layout placement,
/// scrollbars, and focus highlight.
#[derive(Default)]
pub(super) struct ScrollState {
    /// Vertical scroll offset, in px.
    pub(super) scroll: f32,
    /// Horizontal scroll offset and limit in a widened column, in px.
    pub(super) scroll_x: f32,
    pub(super) max_scroll_x: f32,
    /// Expanded messages whose bodies are wider than the standard column, mapped to their width.
    pub(super) wide: HashMap<String, f32>,
    pub(super) animation: Option<ScrollAnimation>,
    /// The first layout scrolls to a new thread's starting position.
    pub(super) positioned: bool,
    /// A message to bring into view on the next frame.
    pub(super) reveal: Option<(String, isize)>,
    /// The message whose draft to bring into view.
    pub(super) reveal_draft: Option<String>,
    /// Whether the draft being typed in showed when last looked at.
    pub(super) draft_was_in_view: bool,
    /// Where the bottom of the draft being typed in was last frame, so the thread follows it only
    /// as it grows. `-∞` while no draft has the keyboard.
    pub(super) draft_bottom: f32,
    /// Scroll to the end once everything is measured (a reply was just sent).
    pub(super) reveal_end: bool,
    /// The message column's visible height and the content's, from the last frame.
    pub(super) viewport: f32,
    pub(super) content: f32,
    /// Each item's top in the content and its height, from the last frame.
    pub(super) tops: Vec<(f32, f32)>,
    /// The column's entries as last laid out, and where its content ends.
    pub(super) entries: Vec<Placed>,
    pub(super) end: f32,
    /// The wheel gesture under way, and the body it scrolls sideways if any.
    pub(super) gesture: overflow::Gesture,
    /// The messages' vertical and horizontal scrollbars.
    pub(super) scrollbar: exo_ui::Scrollbar,
    pub(super) scrollbar_x: exo_ui::Scrollbar,
    /// The bar that slides to the focused message.
    pub(super) highlight: highlight::Highlight,
}

impl ThreadView {
    /// How far the messages are scrolled, in px.
    pub fn scroll(&self) -> f32 {
        self.scroll_state.scroll
    }

    /// ↓ / ↑: scrolls within the focused message, and past its edge moves to the next or previous
    /// message. A press that isn't held (`repeat`) moves past the first message to the header,
    /// and past the last to the Show New Messages bar.
    pub fn arrow(&mut self, delta: isize, repeat: bool) {
        let Some(at) = self.focus_index() else { return };
        let Some(&(top, height)) = self.scroll_state.tops.get(at) else {
            return;
        };
        let (view_top, view_bottom) = (
            self.scroll_state.scroll,
            self.scroll_state.scroll + self.scroll_state.viewport,
        );
        let target = if delta > 0 && top + height > view_bottom {
            Some(self.scroll_state.scroll + IDEAL_TOP)
        } else if delta < 0 && top < view_top {
            Some(self.scroll_state.scroll - IDEAL_TOP)
        } else {
            None
        };
        match target {
            Some(to) if self.clamp_scroll(to) != self.scroll_state.scroll => self.animate_to(to),
            _ if !repeat && self.arrow_out(delta, at) => {}
            _ => self.next_message(delta),
        }
    }

    /// Space / ⇧Space: scrolls a page down or up, then focuses the message nearest the middle.
    pub fn page(&mut self, direction: isize) {
        let viewport = self.scroll_state.viewport;
        let page = viewport - MESSAGES_PADDING_TOP - SLACK;
        let to = self.clamp_scroll(self.scroll_state.scroll + direction as f32 * page);
        self.animate_to(to);
        let middle = to + viewport / 2.0;
        let central = self
            .scroll_state
            .tops
            .iter()
            .position(|&(top, height)| top + height + 16.0 > middle);
        let at = match (direction, central) {
            _ if to <= 0.0 => 0,
            _ if to >= self.max_scroll() => self.items.len().saturating_sub(1),
            (_, Some(at)) => at,
            (_, None) => self.items.len().saturating_sub(1),
        };
        if let Some(item) = self.items.get(at) {
            self.focus = Some(item.id.clone());
        }
    }

    /// `g g` / ⌘↑ and ⇧G / ⌘↓: focuses the first or last message and scrolls to the top or
    /// bottom.
    pub fn jump(&mut self, to_end: bool) {
        let at = if to_end {
            self.items.len().saturating_sub(1)
        } else {
            0
        };
        if let Some(item) = self.items.get(at) {
            self.focus = Some(item.id.clone());
        }
        let to = if to_end { self.max_scroll() } else { 0.0 };
        self.animate_to(to);
    }

    pub(super) fn max_scroll(&self) -> f32 {
        (self.scroll_state.content - self.scroll_state.viewport).max(0.0)
    }

    pub(super) fn clamp_scroll(&self, to: f32) -> f32 {
        to.clamp(0.0, self.max_scroll())
    }

    pub(super) fn animate_to(&mut self, to: f32) {
        self.animate_over(to, SCROLL_DURATION);
    }

    pub(super) fn animate_over(&mut self, to: f32, duration: f32) {
        let to = self.clamp_scroll(to);
        self.scroll_state.animation = Some(ScrollAnimation {
            from: self.scroll_state.scroll,
            to,
            started: f64::NAN,
            duration,
        });
    }

    /// Focuses message `id` and scrolls its new draft into view on the next frame.
    pub fn reveal_draft(&mut self, id: &str) {
        self.focus = Some(id.to_owned());
        self.scroll_state.reveal_draft = Some(id.to_owned());
    }

    /// Focuses message `id`, a reply just sent that the next copy of the thread has, and scrolls
    /// to the end once it's measured.
    pub fn reveal_sent(&mut self, id: &str) {
        self.focus = Some(id.to_owned());
        self.scroll_state.reveal_end = true;
        // Shown at once, not behind the Show New Messages bar.
        self.action.admitted.insert(id.to_owned());
    }

    /// Where a newly opened thread scrolls: past all but four of the collapsed messages above the
    /// first expanded one. When the only expanded message is the last, the scroll puts that
    /// message `IDEAL_TOP` below the top instead.
    pub(super) fn initial_scroll(&self, collapsed_scroll: f32) -> f32 {
        let first_expanded = self.items.iter().position(|item| item.expanded);
        let target = match first_expanded {
            Some(at) if at + 1 == self.items.len() => self.scroll_state.tops[at].0 - IDEAL_TOP,
            _ => collapsed_scroll,
        };
        self.clamp_scroll(target)
    }

    /// Scrolls to what an in-message link pointed at, `top` CSS px down message `id`'s body, so
    /// that it starts just under the scrolled header.
    pub(super) fn scroll_to_anchor(
        &mut self,
        ui: &Ui,
        layout: &Layout,
        id: &str,
        top: f32,
        bodies: &Bodies,
    ) {
        let Some(at) = self.items.iter().position(|item| item.id == id) else {
            return;
        };
        let Some(message) = self.message(id) else {
            return;
        };
        let body = message::body_offset(self.items[at].header_expanded, message);
        let invite = self.invite_block(ui, id, self.item_layout(layout, at).inner.width(), bodies);
        // The messages start under the header's bottom padding. Once scrolled, the header ends
        // `SCROLLED_INSET` above its bottom.
        let under_header = -MESSAGES_MARGIN_TOP - header::SCROLLED_INSET;
        self.scroll_state.animation = None;
        self.scroll_state.scroll =
            self.clamp_scroll(self.scroll_state.tops[at].0 + body + invite + top - under_header);
    }

    /// Brings a draft into view. A card that fits is scrolled up to its top. Otherwise the draft's
    /// bottom is brought into view.
    pub(super) fn scroll_to_draft(&mut self, id: &str) {
        let Some(at) = self.items.iter().position(|item| item.id == id) else {
            return;
        };
        let Some(&(top, height)) = self.scroll_state.tops.get(at) else {
            return;
        };
        let viewport = self.scroll_state.viewport;
        let fits = height + MESSAGES_PADDING_TOP <= viewport;
        let to = if fits && top < self.scroll_state.scroll + MESSAGES_PADDING_TOP {
            top - MESSAGES_PADDING_TOP
        } else if top + height > self.scroll_state.scroll + viewport - DRAFT_MARGIN_BOTTOM {
            if fits {
                top - MESSAGES_PADDING_TOP
            } else {
                top + height - viewport + DRAFT_MARGIN_BOTTOM
            }
        } else {
            return;
        };
        self.animate_over(to, DRAFT_SCROLL_DURATION);
    }

    /// Keeps the focused draft's bottom 28px above the messages' bottom.
    /// Scrolls so the bottom of the draft being typed in stays in view as it grows. Only as it
    /// grows: the wheel can still scroll the thread up to read what the reply answers.
    pub(super) fn keep_draft_in_view(&mut self, drafts: &HashMap<String, f32>) {
        let viewport = self.scroll_state.viewport;
        let bottom = self
            .items
            .iter()
            .zip(&self.scroll_state.tops)
            .filter(|(item, _)| drafts.get(&item.id).is_some_and(|height| *height > 0.0))
            .map(|(_, (top, height))| top + height)
            .fold(f32::NEG_INFINITY, f32::max);
        let grew = bottom > self.scroll_state.draft_bottom + 0.5;
        self.scroll_state.draft_bottom = bottom;
        if grew
            && bottom.is_finite()
            && bottom > self.scroll_state.scroll + viewport - DRAFT_MARGIN_BOTTOM
        {
            self.scroll_state.scroll = self.clamp_scroll(bottom - viewport + DRAFT_MARGIN_BOTTOM);
        }
    }

    /// Whether a draft (its form, at the bottom of its message's card) shows in the viewport.
    pub(super) fn draft_in_view(&self, drafts: &HashMap<String, f32>) -> bool {
        let (view_top, view_bottom) = (
            self.scroll_state.scroll,
            self.scroll_state.scroll + self.scroll_state.viewport,
        );
        self.items
            .iter()
            .zip(&self.scroll_state.tops)
            .any(|(item, (top, height))| {
                drafts.get(&item.id).is_some_and(|draft| {
                    *draft > 0.0 && top + height - draft < view_bottom && top + height > view_top
                })
            })
    }

    /// N moving onto a message just below the fold scrolls a step. A message that's out of view
    /// scrolls to `IDEAL_TOP` below the top.
    pub(super) fn scroll_to_message(&mut self, id: &str, direction: isize) {
        let Some(at) = self.items.iter().position(|item| item.id == id) else {
            return;
        };
        let Some(&(top, height)) = self.scroll_state.tops.get(at) else {
            return;
        };
        let (view_top, view_bottom) = (
            self.scroll_state.scroll,
            self.scroll_state.scroll + self.scroll_state.viewport,
        );
        if direction > 0 && top > view_bottom - IDEAL_TOP / 2.0 && top < view_bottom + IDEAL_TOP {
            self.animate_to(self.scroll_state.scroll + IDEAL_TOP);
        } else if top + SLACK > view_bottom || top + height - SLACK < view_top {
            self.animate_to(top - IDEAL_TOP);
        }
    }

    /// Applies initial positioning, pending reveals, draft tracking, animations, wheel input, and
    /// device-pixel snapping to `self.scroll_state.scroll`.
    #[expect(
        clippy::too_many_arguments,
        reason = "advances one frame's scroll state from layout and input"
    )]
    pub(super) fn settle_scroll(
        &mut self,
        ui: &Ui,
        layout: &Layout,
        messages: Rect,
        collapsed_scroll: f32,
        anchored: Option<(String, f32)>,
        reveal: Option<egui::Vec2>,
        wheel_y: f32,
        typing: bool,
        bodies: &Bodies,
        drafts: Option<&crate::compose::Inline<'_>>,
        draft_heights: &HashMap<String, f32>,
        actions: &mut Vec<Action>,
        now: f64,
        scale: f32,
    ) {
        let measured = self
            .items
            .iter()
            .all(|item| !item.expanded || bodies.height_known(&item.id));
        if !self.scroll_state.positioned && measured {
            self.scroll_state.positioned = true;
            self.scroll_state.scroll = self.initial_scroll(collapsed_scroll);
        }
        if let Some((id, direction)) = self.scroll_state.reveal.take() {
            self.scroll_to_message(&id, direction);
        }
        if let Some(id) = self.scroll_state.reveal_draft.take().filter(|_| measured) {
            self.scroll_to_draft(&id);
        }
        if self.scroll_state.reveal_end && measured {
            self.scroll_state.reveal_end = false;
            self.animate_to(self.max_scroll());
        }
        if let Some((id, top)) = anchored {
            self.scroll_to_anchor(ui, layout, &id, top, bodies);
        }
        if !typing {
            self.scroll_state.draft_bottom = f32::NEG_INFINITY;
        } else if self.scroll_state.animation.is_none() {
            self.keep_draft_in_view(draft_heights);
        }
        // When the draft being typed in scrolls away, suggest popping it out.
        if typing {
            let in_view = self.draft_in_view(draft_heights);
            if self.scroll_state.draft_was_in_view && !in_view {
                actions.push(Action::PopOutHint);
            }
            self.scroll_state.draft_was_in_view = in_view;
        }
        if let Some(animation) = &mut self.scroll_state.animation {
            if animation.started.is_nan() {
                animation.started = now;
            }
            let progress = ((now - animation.started) as f32 / animation.duration).min(1.0);
            self.scroll_state.scroll =
                animation.from + (animation.to - animation.from) * CubicBezier::EASE.at(progress);
            if progress >= 1.0 {
                self.scroll_state.animation = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        if ui.rect_contains_pointer(messages) {
            let pointer = ui.input(|input| input.pointer.hover_pos());
            // A reply at its max height scrolls its own body first.
            let form = pointer
                .zip(drafts)
                .is_some_and(|(at, drafts)| drafts.takes_wheel(at, wheel_y));
            if wheel_y != 0.0 && !form {
                self.scroll_state.animation = None;
                self.scroll_state.scroll -= wheel_y;
            }
        }
        if let Some(delta) = reveal.map(|reveal| reveal.y).filter(|delta| *delta != 0.0) {
            self.scroll_state.animation = None;
            self.scroll_state.scroll += delta;
        }
        self.scroll_state.scroll = self.clamp_scroll(self.scroll_state.scroll);
        // Scroll offsets land on device pixels, as Chromium's do.
        self.scroll_state.scroll = (self.scroll_state.scroll * scale).round() / scale;
    }
}
