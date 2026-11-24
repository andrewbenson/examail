//! Wide mail. An expanded message whose body is wider than the column can shrink to fit widens
//! the messages column from the back gutter to 2rem from the pane's right edge. The column then
//! scrolls sideways as one, by the wheel or by ←/→, each card as wide as what it holds, with a
//! hairline down each edge that has more beyond it.

use egui::{Rect, Ui, pos2};

use super::{Layout, SANDBOX_MAX_WIDTH, ThreadView};
use crate::body::Bodies;

/// A body is wide past this width, more than the 25% a body shrinks by to fit the column, plus
/// 2px.
const WIDE_CONTENT: f32 = SANDBOX_MAX_WIDTH * 1.25 + 2.0;
/// How far ← and → scroll the column, at once.
const STEP: f32 = 175.0;
/// The widened column's side borders.
const BORDER: f32 = 1.0;
/// The back gutter's part of the column's left, besides its `4rem` (`calc(40px + 4rem)`).
const GUTTER: f32 = 40.0;

/// The widened column's cards.
#[derive(Clone, Copy, Debug)]
pub(super) struct Column {
    /// The width the collapsed rows span, which is the column's, or the widest card's when that's
    /// wider (`min-width: min-content`).
    rows: f32,
    /// How far the cards are scrolled sideways, and how far they can be.
    scroll: f32,
    max_scroll: f32,
}

impl Layout {
    /// Widens the messages column for a wide body `widest` px wide, its cards scrolled `scroll` px
    /// sideways (kept in range). `pane` is the content pane (whose width is `100cqw`), and
    /// `thread_right` is the thread pane's right edge. The column is
    /// `calc(100cqw - calc(40px + 4rem) - 2rem)` wide and ends 2rem inside the thread pane
    /// (`align-self: flex-end; margin-right: 2rem`), and the cards' column has `padding: 0 2rem`
    /// inside its borders.
    pub(super) fn widen(&mut self, pane: Rect, thread_right: f32, widest: f32, scroll: f32) {
        let rem = self.rem;
        let right = thread_right - 2.0 * rem;
        let width = pane.width() - (GUTTER + 4.0 * rem) - 2.0 * rem;
        self.column = Rect::from_x_y_ranges(right - width..=right, self.column.y_range());
        let viewport = width - 2.0 * BORDER;
        let rows = (viewport - 4.0 * rem).max(widest + 4.0 * rem);
        // Whole px, as Chromium rounds a scroll range, so the end keeps the bodies' phase too.
        let max_scroll = (rows + 4.0 * rem - viewport).round().max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        // An expanded card is `calc(640px + 4rem)` wide, with its body 640px.
        let left = self.column.left() + BORDER + 2.0 * rem - scroll;
        self.card = Rect::from_min_max(
            pos2(left, self.card.top()),
            pos2(left + SANDBOX_MAX_WIDTH + 4.0 * rem, self.card.top()),
        );
        self.inner = Rect::from_min_max(
            pos2(left + 2.0 * rem, self.inner.top()),
            pos2(left + 2.0 * rem + SANDBOX_MAX_WIDTH, self.inner.top()),
        );
        self.wide = Some(Column {
            rows,
            scroll,
            max_scroll,
        });
    }

    /// Whether the column is widened.
    pub(super) fn is_wide(&self) -> bool {
        self.wide.is_some()
    }

    /// How far the widened column is scrolled sideways, and how far it can be (0 and 0 when it
    /// isn't widened).
    pub(super) fn scroll_x(&self) -> (f32, f32) {
        self.wide
            .map_or((0.0, 0.0), |column| (column.scroll, column.max_scroll))
    }

    /// The back gutter's clickable width, which is the gutter's width plus the gap to the
    /// messages' table, less 20px. A widened column's table starts left of the gutter's right
    /// edge, which makes that gap negative.
    pub(super) fn gutter_clickable(&self) -> f32 {
        let table = if self.is_wide() {
            self.column.left() + BORDER
        } else {
            self.card.left()
        };
        self.action_pane.width() + table - self.action_pane.right() - 20.0
    }

    /// The part of the back gutter that takes the pointer. That's the clickable area from the
    /// gutter's left, as wide as [`Self::gutter_clickable`] (past the gutter, to 20px short of the
    /// messages' table), or beside a widened column only its back button
    /// (`pointer-events: none`, the button's `auto`).
    pub(super) fn gutter_target(&self) -> Rect {
        if self.is_wide() {
            return super::header::back_button_rect(self);
        }
        Rect::from_min_size(
            self.action_pane.min,
            egui::vec2(self.gutter_clickable().max(0.0), self.action_pane.height()),
        )
    }

    /// The part of the column that shows the messages, inside the widened column's borders.
    pub(super) fn viewport(&self, top: f32, bottom: f32) -> Rect {
        let inset = if self.is_wide() { BORDER } else { 0.0 };
        Rect::from_min_max(
            pos2(self.column.left() + inset, top),
            pos2(self.column.right() - inset, bottom),
        )
    }

    /// This layout with a card whose content is `inner` px wide, at the same left.
    fn sized(&self, inner: f32) -> Self {
        let mut sized = self.clone();
        sized.card.set_width(inner + 4.0 * self.rem);
        sized.inner.set_width(inner);
        sized
    }

    /// The hairlines down the widened column's edges that have more of it beyond them, from `top`
    /// to `bottom`. They're `rgba(0, 0, 0, .085)` (Carbon's `hsla(0, 0%, 100%, .085)`), on device
    /// pixels as Chromium snaps borders. The right one shows from the start, since there's more to
    /// the right then too.
    pub(super) fn paint_edges(&self, ui: &Ui, top: f32, bottom: f32) {
        let Some(column) = self.wide else { return };
        let ppp = ui.ctx().pixels_per_point();
        let snap = |x: f32| (x * ppp).round() / ppp;
        let color = exo_ui::palette(ui.ctx()).ink(0.085, 0.085);
        let edges = [
            (column.scroll > 0.0, self.column.left()),
            (
                column.scroll < column.max_scroll,
                self.column.right() - BORDER,
            ),
        ];
        for (shown, left) in edges {
            if shown {
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(snap(left), top), pos2(snap(left + BORDER), bottom)),
                    0.0,
                    color,
                );
            }
        }
    }
}

impl ThreadView {
    /// Notes the expanded messages whose bodies, laid out in the column, turn out too wide for it.
    /// From then on each is laid out as wide as its content was.
    pub(super) fn find_wide(&mut self, bodies: &Bodies) {
        for item in self.items.iter().filter(|item| item.expanded) {
            if let Some((width, content)) = bodies.laid_width(&item.id)
                && width <= SANDBOX_MAX_WIDTH
                && content > WIDE_CONTENT
            {
                self.scroll_state
                    .wide
                    .entry(item.id.clone())
                    .or_insert(content);
            }
        }
    }

    /// The widest of the expanded wide bodies, which widen the column.
    pub(super) fn widest(&self) -> Option<f32> {
        self.items
            .iter()
            .filter(|item| item.expanded)
            .filter_map(|item| self.scroll_state.wide.get(&item.id).copied())
            .reduce(f32::max)
    }

    /// The layout message `items[at]` is drawn in. In a widened column a collapsed row spans the
    /// cards' column, and an expanded card is `calc(640px + 4rem)` wide, or as wide as its wide
    /// body and its padding (`min-width: min-content`).
    pub(super) fn item_layout(&self, layout: &Layout, at: usize) -> Layout {
        let Some(column) = layout.wide else {
            return layout.clone();
        };
        let item = &self.items[at];
        match self.scroll_state.wide.get(&item.id) {
            Some(width) if item.expanded => layout.sized(*width),
            _ if item.expanded => layout.clone(),
            _ => layout.sized(column.rows - 4.0 * layout.rem),
        }
    }

    /// A layout whose card spans the cards' column (its content box), for markers
    /// (`Reminder returned`, a trashed message) and anything else kept within the table.
    pub(super) fn spanning(layout: &Layout) -> Layout {
        match layout.wide {
            Some(column) => layout.sized(column.rows - 4.0 * layout.rem),
            None => layout.clone(),
        }
    }

    /// Scrolls the widened column `direction` steps sideways. Returns whether it moved. A column
    /// that isn't widened never does.
    pub fn scroll_sideways(&mut self, direction: isize) -> bool {
        let to = (self.scroll_state.scroll_x + direction as f32 * STEP)
            .clamp(0.0, self.scroll_state.max_scroll_x);
        let moved = to != self.scroll_state.scroll_x;
        self.scroll_state.scroll_x = to;
        moved
    }
}

#[cfg(test)]
mod tests {
    use egui::vec2;

    use super::*;

    #[test]
    fn a_wide_body_widens_the_column_to_the_gutter() {
        // At 1440×900, the pane runs from 40 to 1087.64, the thread pane ends there, and the
        // body is 1200px wide. `rem` is 15px.
        let pane = Rect::from_min_size(pos2(40.0, 41.0), vec2(1047.64, 859.0));
        let mut layout = Layout::new(pane, 15.0, 1440.0, false);
        layout.widen(pane, pane.right(), 1200.0, 0.0);
        assert_eq!(
            (layout.column.left(), layout.column.right()),
            (140.0, 1057.64)
        );
        assert_eq!(
            (layout.card.left(), layout.card.width()),
            (171.0, 700.0),
            "an expanded card: 640px and its padding"
        );
        let (scroll, max) = layout.scroll_x();
        assert_eq!(scroll, 0.0);
        assert_eq!(
            max, 404.0,
            "the cards' 1320px less the 916px inside the borders"
        );
        assert_eq!(layout.sized(1200.0).card.width(), 1260.0);
        // The gutter's clickable area ends 20px short of the table, which here starts inside the
        // widened column's border. That's 81px, or 274.63px unwidened.
        assert!(
            (layout.gutter_clickable() - 81.0).abs() < 0.01,
            "{}",
            layout.gutter_clickable()
        );
        let unwidened = Layout::new(pane, 15.0, 1440.0, false);
        assert!((unwidened.gutter_clickable() - 274.63).abs() < 0.01);
        let target = unwidened.gutter_target();
        assert!(
            (target.right() - unwidened.action_pane.right() - 33.0).abs() < 0.1,
            "33px past the gutter: {target:?}"
        );

        layout.widen(pane, pane.right(), 1200.0, 500.0);
        assert_eq!(
            layout.card.left(),
            171.0 - 404.0,
            "scrolled as far as it goes"
        );
    }
}
