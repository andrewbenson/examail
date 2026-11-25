//! A floating list popup anchored above or below a field: an [`egui::Area`] card with pixel-snapped
//! shadows or border, row hover tracking, press-to-pick (so a choice lands before focus leaves the
//! input), and optional wheel scrolling with [`Scrollbar`].

use egui::{
    Color32, Context, CornerRadius, CursorIcon, Id, Order, Painter, Rect, Sense, Stroke,
    StrokeKind, Vec2, pos2, vec2,
};

use crate::{BoxShadow, Scrollbar, paint_editor_tooltip, pixel_snap};

const MENU_SHADOWS: [BoxShadow; 2] = [
    BoxShadow::new([0.0, 2.0], 12.0, 0.0, Color32::from_black_alpha(13)),
    BoxShadow::new([0.0, 2.0], 4.0, 0.0, Color32::from_black_alpha(26)),
];

const ELEVATED_SHADOW: BoxShadow =
    BoxShadow::new([0.0, 2.0], 8.0, 0.0, Color32::from_black_alpha(26));

/// How an [`AnchoredList`]'s card and highlighted row are painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchoredCard {
    /// Menu and contact-list chrome: 2px corners and two soft shadows.
    Menu {
        background: Color32,
        selected: Color32,
    },
    /// Inline editor-assistant chrome: 4px corners and two soft shadows.
    Tooltip {
        background: Color32,
        selected: Color32,
    },
    /// Form suggestion and time-list chrome: 4px corners and `0 2px 8px rgba(0,0,0,.1)`.
    Elevated {
        background: Color32,
        selected: Color32,
    },
    /// Bordered card without a shadow: 4px corners and a 1px inside border.
    Bordered {
        background: Color32,
        border: Color32,
        selected: Color32,
    },
}

impl AnchoredCard {
    /// Menu chrome: 2px corners and two soft shadows.
    pub const fn menu(background: Color32, selected: Color32) -> Self {
        Self::Menu {
            background,
            selected,
        }
    }

    /// Inline tooltip chrome: 4px corners and two soft shadows.
    pub const fn tooltip(background: Color32, selected: Color32) -> Self {
        Self::Tooltip {
            background,
            selected,
        }
    }

    /// Elevated suggestion chrome: 4px corners and one 8px shadow.
    pub const fn elevated(background: Color32, selected: Color32) -> Self {
        Self::Elevated {
            background,
            selected,
        }
    }

    /// Bordered card chrome: 4px corners and a 1px inside border.
    pub const fn bordered(background: Color32, border: Color32, selected: Color32) -> Self {
        Self::Bordered {
            background,
            border,
            selected,
        }
    }

    /// The fill of a highlighted row.
    pub const fn selected(self) -> Color32 {
        match self {
            Self::Menu { selected, .. }
            | Self::Tooltip { selected, .. }
            | Self::Elevated { selected, .. }
            | Self::Bordered { selected, .. } => selected,
        }
    }

    pub(crate) fn paint(self, painter: &Painter, rect: Rect) -> Rect {
        let snapped = pixel_snap(rect);
        match self {
            Self::Menu { background, .. } => {
                let radius = CornerRadius::same(2);
                for shadow in MENU_SHADOWS {
                    shadow.paint(painter, snapped, radius);
                }
                painter.rect_filled(snapped, radius, background);
            }
            Self::Tooltip { background, .. } => {
                paint_editor_tooltip(painter, rect, background);
            }
            Self::Elevated { background, .. } => {
                let radius = CornerRadius::same(4);
                ELEVATED_SHADOW.paint(painter, snapped, radius);
                painter.rect_filled(snapped, radius, background);
            }
            Self::Bordered {
                background, border, ..
            } => {
                painter.rect(
                    snapped,
                    CornerRadius::same(4),
                    background,
                    Stroke::new(1.0, border),
                    StrokeKind::Inside,
                );
            }
        }
        snapped
    }
}

/// When pointer hover reports `hovered` on an [`AnchoredList`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnchoredHover {
    /// Only when the pointer moved this frame, so keyboard navigation isn't overridden by a
    /// stationary cursor.
    #[default]
    OnMove,
    /// Whenever the pointer rests over a selectable row.
    Always,
}

/// What the pointer did over an [`AnchoredList`] this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchoredListResponse {
    /// Selectable row under the pointer (according to [`AnchoredHover`]).
    pub hovered: Option<usize>,
    /// Selectable row the primary button went down on this frame.
    pub pressed: Option<usize>,
    /// Selectable row clicked (primary button pressed and released on the same row) this frame.
    pub clicked: Option<usize>,
    /// Whether the pointer is over this list's own layer (not occluded by a higher layer).
    pub contains_pointer: bool,
    /// The card's rectangle.
    pub rect: Rect,
}

impl Default for AnchoredListResponse {
    fn default() -> Self {
        Self {
            hovered: None,
            pressed: None,
            clicked: None,
            contains_pointer: false,
            rect: Rect::NOTHING,
        }
    }
}

/// A floating list popup rendered in an [`egui::Area`].
#[must_use]
pub struct AnchoredList<'a> {
    id: Id,
    rect: Rect,
    card: AnchoredCard,
    order: Order,
    pad_top: f32,
    pad_bottom: f32,
    inset_x: f32,
    highlighted: Option<usize>,
    hover_mode: AnchoredHover,
    clip_to_rows: bool,
    scroll: Option<&'a mut f32>,
    scrollbar: Option<(Id, &'a mut Scrollbar)>,
}

impl<'a> AnchoredList<'a> {
    /// A list popup at `rect` with `card` chrome.
    pub fn new(id: Id, rect: Rect, card: AnchoredCard) -> Self {
        Self {
            id,
            rect,
            card,
            order: Order::Tooltip,
            pad_top: 0.0,
            pad_bottom: 0.0,
            inset_x: 0.0,
            highlighted: None,
            hover_mode: AnchoredHover::OnMove,
            clip_to_rows: false,
            scroll: None,
            scrollbar: None,
        }
    }

    /// Draws in `order` rather than [`Order::Tooltip`].
    pub fn order(mut self, order: Order) -> Self {
        self.order = order;
        self
    }

    /// Top and bottom padding inside the card, before the first row and after the last.
    pub fn padding(mut self, top: f32, bottom: f32) -> Self {
        self.pad_top = top;
        self.pad_bottom = bottom;
        self
    }

    /// Equal top and bottom padding inside the card.
    pub fn uniform_padding(self, pad: f32) -> Self {
        self.padding(pad, pad)
    }

    /// Horizontal inset of each row's highlight and hit box inside the card (e.g. 1px inside a
    /// bordered card).
    pub fn inset_x(mut self, inset_x: f32) -> Self {
        self.inset_x = inset_x;
        self
    }

    /// The highlighted row, if any.
    pub fn highlighted(mut self, highlighted: Option<usize>) -> Self {
        self.highlighted = highlighted;
        self
    }

    /// Whether hover requires pointer motion or fires whenever the pointer is over a row.
    pub fn hover_mode(mut self, hover_mode: AnchoredHover) -> Self {
        self.hover_mode = hover_mode;
        self
    }

    /// Clips scrolled rows between `pad_top` and `pad_bottom` rather than to the full card.
    pub fn clip_rows(mut self) -> Self {
        self.clip_to_rows = true;
        self
    }

    /// Enables wheel scrolling over the card, updating `scroll`.
    pub fn scroll(mut self, scroll: &'a mut f32) -> Self {
        self.scroll = Some(scroll);
        self
    }

    /// Paints an overlay [`Scrollbar`] along the card's right edge when content overflows.
    pub fn scrollbar(mut self, id: Id, scrollbar: &'a mut Scrollbar) -> Self {
        self.scrollbar = Some((id, scrollbar));
        self
    }

    /// Paints `count` uniform-height rows of `row_height`.
    pub fn show_uniform(
        self,
        ctx: &Context,
        count: usize,
        row_height: f32,
        mut paint_row: impl FnMut(&Painter, Rect, usize),
    ) -> AnchoredListResponse {
        let heights = vec![row_height; count];
        self.show_variable(ctx, &heights, |_| true, &mut paint_row)
    }

    /// Paints variable-height rows, where `selectable(index)` controls whether a row highlights,
    /// shows a pointing-hand cursor, and responds to hover/press (e.g. false for section headers).
    pub fn show_variable(
        self,
        ctx: &Context,
        row_heights: &[f32],
        selectable: impl Fn(usize) -> bool,
        mut paint_row: impl FnMut(&Painter, Rect, usize),
    ) -> AnchoredListResponse {
        let rect = self.rect;
        let rows_sum: f32 = row_heights.iter().sum();
        let content = self.pad_top + rows_sum + self.pad_bottom;
        let mut response = AnchoredListResponse {
            hovered: None,
            pressed: None,
            clicked: None,
            contains_pointer: false,
            rect,
        };
        let (pointer, pressed, clicked, any_down, moving) = ctx.input(|input| {
            (
                input
                    .pointer
                    .interact_pos()
                    .or_else(|| input.pointer.hover_pos()),
                input.pointer.primary_pressed(),
                input.pointer.primary_clicked(),
                input.pointer.any_down(),
                input.pointer.is_moving() || input.pointer.delta() != Vec2::ZERO,
            )
        });
        let hover_active = match self.hover_mode {
            AnchoredHover::OnMove => moving,
            AnchoredHover::Always => true,
        };
        let selected_fill = self.card.selected();
        let Self {
            id,
            card,
            order,
            pad_top,
            pad_bottom,
            inset_x,
            highlighted,
            clip_to_rows,
            mut scroll,
            mut scrollbar,
            ..
        } = self;
        let press_id = id.with("exo_ui::anchored_press");
        let pressed_row = ctx.data(|data| data.get_temp::<usize>(press_id));

        egui::Area::new(id)
            .order(order)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |area| {
                let _ = card.paint(area.painter(), rect);
                let _ = area.allocate_rect(rect, Sense::click());
                let layer = area.layer_id();
                let over = !area.is_sizing_pass()
                    && pointer
                        .is_some_and(|at| rect.contains(at) && layer_unoccluded(ctx, layer, at));
                response.contains_pointer = over;
                let visible = if clip_to_rows {
                    Rect::from_min_max(
                        pos2(rect.left(), rect.top() + pad_top),
                        pos2(
                            rect.right(),
                            (rect.bottom() - pad_bottom).max(rect.top() + pad_top),
                        ),
                    )
                } else {
                    rect
                };
                let scroll_y = if let Some(offset) = scroll.as_deref_mut() {
                    let max_scroll = if clip_to_rows {
                        (rows_sum - visible.height()).max(0.0)
                    } else {
                        (content - rect.height()).max(0.0)
                    };
                    if over && max_scroll > 0.0 {
                        let wheel = area.input(|input| input.smooth_scroll_delta.y);
                        *offset = (*offset - wheel).clamp(0.0, max_scroll);
                    } else {
                        *offset = offset.clamp(0.0, max_scroll);
                    }
                    *offset
                } else {
                    0.0
                };

                let painter = if scroll.is_some() {
                    area.painter().with_clip_rect(pixel_snap(visible))
                } else {
                    area.painter().clone()
                };

                let row_left = rect.left() + inset_x;
                let row_width = (rect.width() - 2.0 * inset_x).max(0.0);
                let mut y = rect.top() + pad_top - scroll_y;
                let mut over_selectable = false;

                for (index, &height) in row_heights.iter().enumerate() {
                    let row = Rect::from_min_size(pos2(row_left, y), vec2(row_width, height));
                    y += height;
                    if scroll.is_some() && !row.intersects(visible) {
                        continue;
                    }
                    let can_pick = selectable(index);
                    if can_pick && highlighted == Some(index) {
                        painter.rect_filled(pixel_snap(row), 0.0, selected_fill);
                    }
                    paint_row(&painter, row, index);
                    let row_hovered =
                        over && pointer.is_some_and(|at| row.contains(at) && visible.contains(at));
                    if can_pick && row_hovered {
                        over_selectable = true;
                        if hover_active {
                            response.hovered = Some(index);
                        }
                        if pressed {
                            response.pressed = Some(index);
                        }
                        if clicked && pressed_row == Some(index) {
                            response.clicked = Some(index);
                        }
                    }
                }
                if over_selectable {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if let Some((bar_id, bar)) = scrollbar.as_mut() {
                    bar.show(area, *bar_id, rect, scroll_y, content);
                }
            });
        if pressed {
            ctx.data_mut(|data| match response.pressed {
                Some(index) => {
                    data.insert_temp(press_id, index);
                }
                None => data.remove::<usize>(press_id),
            });
        } else if !any_down {
            ctx.data_mut(|data| data.remove::<usize>(press_id));
        }
        response
    }
}

/// Whether `at` is not occluded by another interactive layer at or above `layer.order`.
///
/// Checking `top.order < layer.order` handles a newly opened `Area`'s first visible frame (when
/// `areas` still holds the previous sizing pass's `interactable = false`) while rejecting any
/// sibling or higher-order layer (modals, alerts, overlays) sitting on top of `layer`.
pub(crate) fn layer_unoccluded(ctx: &Context, layer: egui::LayerId, at: egui::Pos2) -> bool {
    match ctx.layer_id_at(at) {
        Some(top) => top == layer || top.order < layer.order,
        None => true,
    }
}

/// Clamps `scroll` so a row spanning `row_top..row_bottom` in content coordinates is visible inside
/// a `viewport_height`-tall list.
pub fn keep_row_in_view(scroll: f32, row_top: f32, row_bottom: f32, viewport_height: f32) -> f32 {
    let min_scroll = (row_bottom - viewport_height).max(0.0);
    let max_scroll = row_top.max(min_scroll);
    scroll.clamp(min_scroll, max_scroll)
}

/// Steps `current` by `delta` across `0..count`, wrapping around the ends when `wrap` is true and
/// clamping otherwise.
pub fn step_list_index(current: usize, count: usize, delta: isize, wrap: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if wrap {
        (current as isize + delta).rem_euclid(count as isize) as usize
    } else {
        (current as isize + delta).clamp(0, count as isize - 1) as usize
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, RawInput};

    use super::*;

    #[test]
    fn step_and_scroll_helpers_clamp_and_wrap() {
        assert_eq!(step_list_index(0, 4, -1, true), 3);
        assert_eq!(step_list_index(0, 4, -1, false), 0);
        assert_eq!(step_list_index(3, 4, 1, true), 0);
        assert_eq!(step_list_index(3, 4, 1, false), 3);

        assert_eq!(keep_row_in_view(0.0, 120.0, 150.0, 100.0), 50.0);
        assert_eq!(keep_row_in_view(80.0, 20.0, 50.0, 100.0), 20.0);
        assert_eq!(keep_row_in_view(30.0, 40.0, 70.0, 100.0), 30.0);
    }

    #[test]
    fn primary_press_picks_selectable_row_and_skips_headers() {
        let ctx = Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(120.0, 60.0));
        let card = AnchoredCard::elevated(Color32::WHITE, Color32::LIGHT_GRAY);
        // Prime pointer position first, then press on row 1 (y = 50) vs header row 0 (y = 30).
        let run = |at: egui::Pos2| {
            let mut out = AnchoredListResponse::default();
            ctx.run_ui(
                RawInput {
                    events: vec![
                        Event::PointerMoved(at),
                        Event::PointerButton {
                            pos: at,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| {
                    out = AnchoredList::new(Id::new("list"), rect, card).show_variable(
                        ui.ctx(),
                        &[30.0, 30.0],
                        |row| row != 0,
                        |_, _, _| {},
                    );
                },
            )
            .drop_without_applying_deltas();
            out
        };
        assert_eq!(run(pos2(40.0, 30.0)).pressed, None, "header row ignored");
        assert_eq!(run(pos2(40.0, 55.0)).pressed, Some(1), "item row picked");
    }

    #[test]
    fn occluded_list_ignores_pointer_hover_and_press() {
        let ctx = Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(120.0, 60.0));
        let card = AnchoredCard::elevated(Color32::WHITE, Color32::LIGHT_GRAY);
        let run = |events: Vec<Event>, show_cover: bool| {
            let mut out = AnchoredListResponse::default();
            ctx.run_ui(
                RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    out = AnchoredList::new(Id::new("behind-list"), rect, card)
                        .order(Order::Middle)
                        .hover_mode(AnchoredHover::Always)
                        .show_uniform(ui.ctx(), 2, 30.0, |_, _, _| {});
                    if show_cover {
                        egui::Area::new(Id::new("covering-modal"))
                            .order(Order::Foreground)
                            .fixed_pos(rect.min)
                            .constrain(false)
                            .show(ui.ctx(), |area| {
                                let _ = area.allocate_rect(rect, Sense::click());
                            });
                    }
                },
            )
            .drop_without_applying_deltas();
            out
        };

        // Prime both layers past their initial sizing pass so the covering modal is interactable.
        let at = pos2(40.0, 55.0);
        let _ = run(vec![Event::PointerMoved(at)], true);
        let _ = run(vec![Event::PointerMoved(at)], true);
        let covered = run(
            vec![
                Event::PointerMoved(pos2(42.0, 55.0)),
                Event::PointerButton {
                    pos: pos2(42.0, 55.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            true,
        );
        assert!(
            !covered.contains_pointer,
            "occluded list does not contain pointer"
        );
        assert_eq!(covered.hovered, None, "occluded list does not report hover");
        assert_eq!(covered.pressed, None, "occluded list does not report press");
        let _ = run(
            vec![Event::PointerButton {
                pos: pos2(42.0, 55.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );

        // Once the modal is gone, clicks and hover resume on the list.
        let _ = run(vec![Event::PointerMoved(at)], false);
        let pressed = run(
            vec![
                Event::PointerMoved(pos2(42.0, 55.0)),
                Event::PointerButton {
                    pos: pos2(42.0, 55.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            false,
        );
        assert!(pressed.contains_pointer);
        assert_eq!(pressed.hovered, Some(1));
        assert_eq!(pressed.pressed, Some(1));
        let released = run(
            vec![Event::PointerButton {
                pos: pos2(42.0, 55.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert_eq!(released.clicked, Some(1));
    }
}
