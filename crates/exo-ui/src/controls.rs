//! Shared settings and dialog controls: radio-button rows ([`RadioGroup`]), labeled toggle-switch
//! rows ([`SwitchRow`]), and drag-to-reorder lists ([`SortableList`]).

use egui::{Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::{
    BoxShadow, Family, Font, SLIDER_SIZE, SliderColors, TextStyle, motion, paint_radio,
    paint_slider,
};

const RADIO_DIAMETER: f32 = 12.0;
const RADIO_TOP: f32 = 3.0;
const RADIO_GAP: f32 = 7.0;
const RADIO_LINE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const RADIO_LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);

/// Height of a standard dialog switch row (`padding: 4px 0` around a 20px line).
pub const DIALOG_SWITCH_ROW_HEIGHT: f32 = 28.0;
const DIALOG_SWITCH_PADDING: f32 = 4.0;
const DIALOG_SWITCH_NUDGE: f32 = 3.0;
const DIALOG_SWITCH_STYLE: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 12.0, 20.0);
const DIALOG_SWITCH_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);

const DRAG_SHADOW: BoxShadow = BoxShadow::new([1.0, 1.0], 4.0, 0.0, Color32::from_black_alpha(51));
const DRAG_ACCENT: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// Paints a single radio-button line (`12px` radio `3px` down a `12px/20px` line, `7px` before
/// `label`) and returns its clickable response.
pub(crate) fn paint_radio_row(
    ui: &mut Ui,
    id: Id,
    top_left: Pos2,
    label: &str,
    selected: bool,
) -> egui::Response {
    paint_radio(
        ui.painter(),
        pos2(top_left.x, top_left.y + RADIO_TOP),
        selected,
    );
    RADIO_LINE.paint_line(
        ui.painter(),
        pos2(top_left.x + RADIO_DIAMETER + RADIO_GAP, top_left.y),
        label,
        RADIO_LABEL_INK,
        None,
    );
    let hit = Rect::from_min_size(
        top_left,
        vec2(
            RADIO_DIAMETER + RADIO_GAP + RADIO_LINE.width(ui.ctx(), label),
            RADIO_LINE.line_height,
        ),
    );
    ui.interact(hit, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A vertical stack of `12px/20px` radio rows (used by the Theme and Images dialogs).
#[must_use]
pub struct RadioGroup<'a, T> {
    id_source: &'a str,
    top_left: Pos2,
    choices: &'a [(&'a str, T)],
}

impl<'a, T: Copy + PartialEq> RadioGroup<'a, T> {
    /// A radio group starting at `top_left`.
    pub const fn new(id_source: &'a str, top_left: Pos2, choices: &'a [(&'a str, T)]) -> Self {
        Self {
            id_source,
            top_left,
            choices,
        }
    }

    /// Paints every choice, marking `current`, and returns the newly clicked value when a
    /// different choice is clicked.
    pub fn show(self, ui: &mut Ui, current: T) -> Option<T> {
        let mut chosen = None;
        for (index, &(label, value)) in self.choices.iter().enumerate() {
            let line_top = self.top_left.y + index as f32 * RADIO_LINE.line_height;
            let response = paint_radio_row(
                ui,
                ui.id().with((self.id_source, index)),
                pos2(self.top_left.x, line_top),
                label,
                value == current,
            );
            if response.clicked() && value != current {
                chosen = Some(value);
            }
        }
        chosen
    }
}

/// A clickable row pairing a left-aligned text label with a right-aligned [`paint_slider`] switch.
#[must_use]
pub struct SwitchRow<'a> {
    id: Id,
    slider_id: Id,
    rect: Rect,
    label: &'a str,
    style: TextStyle,
    ink: Color32,
    label_y_offset: f32,
    slider_y_offset: f32,
    on: bool,
    disabled: bool,
    colors: SliderColors,
}

impl<'a> SwitchRow<'a> {
    /// A custom switch row inside `rect`, with `label` at `rect.top() + label_y_offset` and the
    /// 30×16 switch at `rect.top() + slider_y_offset`.
    pub fn new(
        id: Id,
        rect: Rect,
        label: &'a str,
        style: TextStyle,
        ink: Color32,
        on: bool,
    ) -> Self {
        Self {
            id,
            slider_id: id.with("slider"),
            rect,
            label,
            style,
            ink,
            label_y_offset: 0.0,
            slider_y_offset: 0.0,
            on,
            disabled: false,
            colors: SliderColors::CLASSIC,
        }
    }

    /// A standard 28px dialog switch row (`12px/20px` semibold label at `+7px`, switch at `+4px`).
    pub fn dialog(id: Id, top_left: Pos2, width: f32, label: &'a str, on: bool) -> Self {
        let rect = Rect::from_min_size(top_left, vec2(width, DIALOG_SWITCH_ROW_HEIGHT));
        Self::new(id, rect, label, DIALOG_SWITCH_STYLE, DIALOG_SWITCH_INK, on).offsets(
            DIALOG_SWITCH_PADDING + DIALOG_SWITCH_NUDGE,
            DIALOG_SWITCH_PADDING,
        )
    }

    /// Overrides the animation id of the switch pill.
    pub fn slider_id(mut self, slider_id: Id) -> Self {
        self.slider_id = slider_id;
        self
    }

    /// Sets vertical offsets from `rect.top()` for the label's line box and the switch pill.
    pub fn offsets(mut self, label_y_offset: f32, slider_y_offset: f32) -> Self {
        self.label_y_offset = label_y_offset;
        self.slider_y_offset = slider_y_offset;
        self
    }

    /// Draws the switch at half opacity and suppresses pointer clicks when `disabled`.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Overrides the switch's track colors.
    pub fn colors(mut self, colors: SliderColors) -> Self {
        self.colors = colors;
        self
    }

    /// Paints the label and switch and returns the row's interaction [`egui::Response`].
    pub fn show(self, ui: &Ui) -> egui::Response {
        let sense = if self.disabled {
            Sense::hover()
        } else {
            Sense::click()
        };
        let response = ui.interact(self.rect, self.id, sense);
        let response = if self.disabled {
            response
        } else {
            response.on_hover_cursor(CursorIcon::PointingHand)
        };
        self.style.paint_line(
            ui.painter(),
            pos2(self.rect.left(), self.rect.top() + self.label_y_offset),
            self.label,
            self.ink,
            None,
        );
        let slider = Rect::from_min_size(
            pos2(
                self.rect.right() - SLIDER_SIZE.x,
                self.rect.top() + self.slider_y_offset,
            ),
            SLIDER_SIZE,
        );
        paint_slider(
            ui.painter(),
            slider,
            self.on,
            self.disabled,
            self.slider_id,
            self.colors,
        );
        response
    }
}

/// Geometry of a drag-to-reorder [`SortableList`].
#[derive(Clone, Copy, Debug)]
pub struct SortableRowMetrics {
    /// Top y of the first row.
    pub first_top: f32,
    /// Vertical distance from one row's top to the next (`row_height` when rows touch).
    pub row_step: f32,
    /// Height of a single row and of the floating dragged card.
    pub row_height: f32,
    /// Left x of the floating dragged card.
    pub card_left: f32,
    /// Width of the floating dragged card.
    pub card_width: f32,
}

/// Shared drag-to-reorder list controller used by Account Settings and Split Inbox.
///
/// Tracks pointer y-delta in `drag` (`Some((from_index, y_offset))`), animates non-dragged rows
/// making way over `150ms ease`, paints the lifted white card with its `3px` `#aeb1dd` left bar
/// and drop shadow, and returns `Some((from, to))` when the pointer releases on a new slot.
#[must_use]
pub struct SortableList {
    count: usize,
    metrics: SortableRowMetrics,
    sortable: bool,
    accent: Color32,
}

impl SortableList {
    /// A sortable list of `count` rows laid out by `metrics`.
    pub const fn new(count: usize, metrics: SortableRowMetrics) -> Self {
        Self {
            count,
            metrics,
            sortable: true,
            accent: DRAG_ACCENT,
        }
    }

    /// Enables or disables drag-reordering (e.g. inactive split tab).
    pub const fn sortable(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }

    /// Overrides the 3px left accent bar color on the lifted card.
    pub const fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    /// Draws the rows and lifted card.
    ///
    /// - `shift_id(index)` supplies the stable animation [`Id`] for row `index` making way.
    /// - `draw_row(ui, index, row_top, is_dragging, drag)` renders a stationary row (and may start
    ///   a drag by writing `*drag = Some((index, initial_delta_y))`).
    /// - `draw_dragged(ui, from_index, dragged_top)` paints the content inside the lifted card.
    pub fn show(
        self,
        ui: &mut Ui,
        drag: &mut Option<(usize, f32)>,
        shift_id: impl Fn(usize) -> Id,
        mut draw_row: impl FnMut(&mut Ui, usize, f32, bool, &mut Option<(usize, f32)>),
        draw_dragged: impl FnOnce(&mut Ui, usize, f32),
    ) -> Option<(usize, usize)> {
        if self.count == 0 {
            *drag = None;
            return None;
        }
        if !self.sortable {
            *drag = None;
        }
        if let Some((from, _)) = *drag
            && from >= self.count
        {
            *drag = None;
        }
        if let Some((_, offset)) = drag.as_mut() {
            *offset += ui.input(|input| input.pointer.delta().y);
        }

        let ctx = ui.ctx().clone();
        let last = self.count - 1;
        let step = self.metrics.row_step;
        let dragging = drag.filter(|_| self.sortable).map(|(from, offset)| {
            let to = ((from as f32 * step + offset) / step)
                .round()
                .clamp(0.0, last as f32) as usize;
            (from, offset, to)
        });

        for index in 0..self.count {
            let slot = match dragging {
                Some((from, _, to)) if index != from && from < index && index <= to => index - 1,
                Some((from, _, to)) if index != from && to <= index && index < from => index + 1,
                _ => index,
            };
            let shift = motion::transition_value(
                &ctx,
                shift_id(index),
                slot as f32 - index as f32,
                0.15,
                0.0,
                motion::CubicBezier::EASE,
            );
            if dragging.is_some_and(|(from, ..)| from == index) {
                continue;
            }
            let top = self.metrics.first_top + step * (index as f32 + shift);
            draw_row(ui, index, top, dragging.is_some(), drag);
        }

        let mut reordered = None;
        if let Some((from, offset, to)) = dragging {
            let top = self.metrics.first_top + step * from as f32 + offset;
            let rect = Rect::from_min_size(
                pos2(self.metrics.card_left, top),
                vec2(self.metrics.card_width, self.metrics.row_height),
            );
            let painter = ui.painter();
            DRAG_SHADOW.paint(painter, rect, CornerRadius::same(4));
            painter.rect_filled(rect, CornerRadius::same(4), Color32::WHITE);
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(3.0, self.metrics.row_height)),
                CornerRadius {
                    nw: 4,
                    sw: 4,
                    ne: 0,
                    se: 0,
                },
                self.accent,
            );
            draw_dragged(ui, from, top);
            if ui.input(|input| input.pointer.any_released()) {
                *drag = None;
                if to != from {
                    reordered = Some((from, to));
                }
            }
        }
        reordered
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, RawInput};

    use super::*;

    #[test]
    fn sortable_list_reorders_when_drag_releases_over_new_slot() {
        let ctx = Context::default();
        let metrics = SortableRowMetrics {
            first_top: 10.0,
            row_step: 40.0,
            row_height: 40.0,
            card_left: 0.0,
            card_width: 200.0,
        };
        let mut drag = Some((0, 75.0));
        let mut moved = None;
        ctx.run_ui(
            RawInput {
                events: vec![Event::PointerButton {
                    pos: pos2(50.0, 95.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                moved = SortableList::new(3, metrics).show(
                    ui,
                    &mut drag,
                    |i| Id::new(("shift", i)),
                    |_, _, _, _, _| {},
                    |_, _, _| {},
                );
            },
        )
        .drop_without_applying_deltas();
        assert_eq!(moved, Some((0, 2)));
        assert_eq!(drag, None);
    }

    #[test]
    fn sortable_list_animates_row_smoothly_when_returning_to_home_slot() {
        let ctx = Context::default();
        let metrics = SortableRowMetrics {
            first_top: 10.0,
            row_step: 40.0,
            row_height: 40.0,
            card_left: 0.0,
            card_width: 200.0,
        };
        let step = |time: f64, drag: &mut Option<(usize, f32)>| -> Option<f32> {
            let mut row1_top = None;
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    let _ = SortableList::new(3, metrics).show(
                        ui,
                        drag,
                        |i| Id::new(("smooth-shift", i)),
                        |_, index, top, _, _| {
                            if index == 1 {
                                row1_top = Some(top);
                            }
                        },
                        |_, _, _| {},
                    );
                },
            )
            .drop_without_applying_deltas();
            row1_top
        };

        // Home position at t = 0.0: row 1 sits at 10 + 40 = 50.
        let mut drag = None;
        assert_eq!(step(0.0, &mut drag), Some(50.0));

        // Drag row 0 past row 1 (offset 45 -> slot 1): row 1 shifts up to slot 0 (top = 10).
        drag = Some((0, 45.0));
        let _ = step(1.0, &mut drag);
        assert_eq!(step(1.2, &mut drag), Some(10.0));

        // Drag row 0 back to slot 0 (offset 0 -> slot 0): row 1 must return smoothly, not snap.
        drag = Some((0, 0.0));
        assert_eq!(
            step(2.0, &mut drag),
            Some(10.0),
            "at start of return, row stays at shifted top instead of snapping"
        );
        let mid = step(2.075, &mut drag).expect("row 1 drawn");
        assert!(
            mid > 10.0 && mid < 50.0,
            "mid-transition top {mid} smoothly interpolates between 10 and 50"
        );
        assert_eq!(
            step(2.2, &mut drag),
            Some(50.0),
            "after 150ms, row settles back at home slot"
        );
    }
}
