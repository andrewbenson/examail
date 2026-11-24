//! The copy tooltip, a dark tooltip box 8px under its target, centred on it and kept in the
//! window. It follows the hover tooltips' timing, showing once hover intent has the pointer
//! settled on the target and going 150ms after the pointer has left the target, unless the pointer
//! is on the tooltip. Its text is at 70% (`top: -1px`, cut at 460px, underlined while hovered) and
//! the copy icon 8px after it at 66% (100% hovered). The text does what its caller says (copy, or
//! compose to the address), and the icon copies.

use std::sync::Arc;

use egui::{Color32, Context, Galley, Id, Order, Painter, Rect, Sense, Ui, Vec2, pos2, vec2};

use crate::{Family, Font, HoverTooltip, Placement, TextStyle, layout, motion, paint_text};

/// A tooltip's text: 12px/20px bold, `padding: 6px 7px 3px`, with the icon's row a pixel taller.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 20.0);
const PADDING_X: f32 = 7.0;
const PADDING_TOP: f32 = 6.0;
const HEIGHT: f32 = 30.0;
/// Between the target and the box.
const GAP: f32 = 8.0;
/// The text's `max-width`; the icon's `margin-left`.
const MAX_TEXT: f32 = 460.0;
const ICON_GAP: f32 = 8.0;
const ICON: Vec2 = Vec2::new(11.0, 14.0);
const COPY: &str = "legacy/DEPRECATED_clipboard";

/// A copy tooltip's target: where it is, what it shows and what its icon copies.
#[derive(Clone, Debug, PartialEq)]
pub struct CopyTip {
    pub target: Rect,
    pub shown: String,
    pub copied: String,
}

/// What was clicked in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyClick {
    /// The text (its caller's action, or copy without one).
    Text,
    /// The copy icon.
    Icon,
}

/// The tooltip's state between frames, built on [`HoverTooltip`].
#[derive(Clone, Debug, Default)]
pub struct CopyTooltip {
    hover: HoverTooltip<(String, String)>,
    tip: Option<CopyTip>,
}

impl CopyTooltip {
    /// Whether a tooltip is up.
    pub fn is_open(&self) -> bool {
        self.hover.is_open()
    }

    /// Outer box size for a copy tooltip showing `text`.
    pub fn box_size(ctx: &Context, text: &str) -> Vec2 {
        size_for_text_width(text_galley(ctx, text).size().x)
    }

    /// Paints the text (cut at 460px, underlined while hovered) and clipboard icon inside `rect`,
    /// returning whichever part was clicked.
    pub fn paint_content(
        ui: &Ui,
        painter: &Painter,
        id: Id,
        rect: Rect,
        text: &str,
    ) -> Option<CopyClick> {
        paint_content_galley(ui, painter, id, rect, text_galley(ui.ctx(), text))
    }

    /// Follows the pointer, with `hovered` the target under it this frame, if any. Shows the
    /// tooltip once the pointer has settled on a target, keeps it 150ms after the pointer has
    /// left the target and while the pointer is on the tooltip (or the 8px between it and the
    /// target), and paints it. Returns a click on the tooltip, with its target.
    pub fn show(
        &mut self,
        ui: &Ui,
        id: Id,
        hovered: Option<CopyTip>,
    ) -> Option<(CopyClick, CopyTip)> {
        let ctx = ui.ctx().clone();
        let key = hovered
            .as_ref()
            .map(|tip| (tip.shown.clone(), tip.copied.clone()));
        if let Some(tip) = &hovered
            && self.hover.active() == key.as_ref()
        {
            self.tip = Some(tip.clone());
        }
        let mut laid = self.tip.as_ref().map(|open| {
            let galley = text_galley(&ctx, &open.shown);
            let rect = rect_for_width(&ctx, open.target, galley.size().x);
            self.hover.set_box(rect, false);
            (open.shown.clone(), galley)
        });
        let Some((active_key, opacity)) = self.hover.step(&ctx, key.clone(), key, false) else {
            self.tip = None;
            return None;
        };
        if let Some(tip) =
            hovered.filter(|tip| (&tip.shown, &tip.copied) == (&active_key.0, &active_key.1))
        {
            self.tip = Some(tip);
        }
        let tip = self.tip.clone()?;
        let galley = match laid.take() {
            Some((shown, galley)) if shown == tip.shown => galley,
            _ => text_galley(&ctx, &tip.shown),
        };
        let rect = rect_for_width(&ctx, tip.target, galley.size().x);
        self.hover.set_box(rect, false);
        let mut clicked = None;
        egui::Area::new(id)
            .order(Order::Tooltip)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(&ctx, |area| {
                let _ = area.allocate_rect(rect, Sense::click());
                let mut painter = area.painter().clone();
                painter.multiply_opacity(opacity);
                crate::paint_tooltip_box(&painter, rect);
                clicked = paint_content_galley(area, &painter, id, rect, galley);
            });
        if clicked.is_some() {
            self.hover.close();
            self.tip = None;
        }
        clicked.map(|click| (click, tip))
    }
}

/// The text and icon inks: white at 70% / 60% in Snow, black in Carbon.
fn inks(ctx: &Context) -> (Color32, Color32) {
    if crate::theme(ctx) == crate::Theme::Carbon {
        (
            Color32::from_black_alpha(179),
            Color32::from_black_alpha(153),
        )
    } else {
        (
            Color32::from_white_alpha(179),
            Color32::from_white_alpha(153),
        )
    }
}

fn text_galley(ctx: &Context, text: &str) -> Arc<Galley> {
    let (text_ink, _) = inks(ctx);
    layout(ctx, TEXT.line_job(text, text_ink, Some(MAX_TEXT)))
}

fn size_for_text_width(text_width: f32) -> Vec2 {
    vec2(
        text_width.min(MAX_TEXT) + ICON_GAP + ICON.x + 2.0 * PADDING_X,
        HEIGHT,
    )
}

/// The box, centred 8px under its target, inside the window.
fn rect_for_width(ctx: &Context, target: Rect, text_width: f32) -> Rect {
    let size = size_for_text_width(text_width);
    let window = ctx.content_rect();
    Placement::Below {
        gap: GAP,
        shift: 0.0,
    }
    .place(target, size, window.x_range())
}

fn paint_content_galley(
    ui: &Ui,
    painter: &Painter,
    id: Id,
    rect: Rect,
    galley: Arc<Galley>,
) -> Option<CopyClick> {
    let ctx = ui.ctx();
    let (text_ink, icon_ink) = inks(ctx);
    let at = pos2(rect.left() + PADDING_X, rect.top() + PADDING_TOP - 1.0);
    let width = galley.size().x;
    let text_rect = Rect::from_min_size(at, vec2(width, TEXT.line_height));
    let text_hit = ui
        .interact(text_rect, id.with("text"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    paint_text(painter, at, galley, &TEXT);
    if text_hit.hovered() {
        let underline = Rect::from_min_size(
            pos2(at.x, at.y + TEXT.baseline(ctx) + 1.0),
            vec2(width, 1.0),
        );
        painter.rect_filled(crate::pixel_snap(underline), 0.0, text_ink);
    }
    // The icon: `top: 1px` on the line.
    let icon = Rect::from_min_size(
        pos2(text_rect.right() + ICON_GAP, rect.top() + PADDING_TOP + 1.0),
        ICON,
    );
    let icon_hit = ui
        .interact(icon, id.with("icon"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let opacity = motion::transition(
        ctx,
        id.with("icon-hover"),
        icon_hit.hovered(),
        0.15,
        motion::CubicBezier::EASE,
    );
    crate::paint_icon_on(
        painter,
        COPY,
        icon,
        icon_ink.gamma_multiply(0.66 + 0.34 * opacity),
    );
    if text_hit.clicked() {
        Some(CopyClick::Text)
    } else {
        icon_hit.clicked().then_some(CopyClick::Icon)
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, RawInput};

    use super::*;

    #[test]
    fn clicking_tooltip_padding_blocks_click_through_while_icon_clicks_still_work() {
        let ctx = Context::default();
        crate::install(&ctx).expect("system fonts");
        let mut tooltip = CopyTooltip::default();
        let target = Rect::from_min_size(pos2(100.0, 40.0), vec2(120.0, 20.0));
        let tip = CopyTip {
            target,
            shown: "alice@example.com".into(),
            copied: "alice@example.com".into(),
        };
        // Commit installed fonts on an initial frame before measuring the tooltip box.
        let mut box_rect = Rect::NOTHING;
        ctx.run_ui(RawInput::default(), |ui| {
            box_rect = rect_for_width(ui.ctx(), target, text_galley(ui.ctx(), &tip.shown).size().x);
        })
        .drop_without_applying_deltas();

        let run = |tooltip: &mut CopyTooltip,
                   time: f64,
                   hovered: Option<CopyTip>,
                   events: Vec<Event>|
         -> (Option<(CopyClick, CopyTip)>, bool) {
            let mut clicked = None;
            let mut behind_clicked = false;
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    behind_clicked = ui
                        .interact(box_rect, Id::new("behind-copy-tip"), Sense::click())
                        .clicked();
                    clicked = tooltip.show(ui, Id::new("copy-tip"), hovered.clone());
                },
            )
            .drop_without_applying_deltas();
            (clicked, behind_clicked)
        };

        // Settle hover on target so tooltip opens, and run one frame past its Area sizing pass.
        let on_target = target.center();
        let _ = run(
            &mut tooltip,
            0.0,
            None,
            vec![Event::PointerMoved(pos2(0.0, 0.0))],
        );
        let _ = run(
            &mut tooltip,
            1.0,
            Some(tip.clone()),
            vec![Event::PointerMoved(on_target)],
        );
        let _ = run(
            &mut tooltip,
            1.15,
            Some(tip.clone()),
            vec![Event::PointerMoved(on_target)],
        );
        let _ = run(
            &mut tooltip,
            1.17,
            Some(tip.clone()),
            vec![Event::PointerMoved(on_target)],
        );
        assert!(tooltip.is_open());

        // Click in the top-left padding of the tooltip box: must not click through to control behind.
        let padding = pos2(box_rect.left() + 2.0, box_rect.top() + 2.0);
        let _ = run(
            &mut tooltip,
            1.2,
            None,
            vec![
                Event::PointerMoved(padding),
                Event::PointerButton {
                    pos: padding,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        let (click, behind_clicked) = run(
            &mut tooltip,
            1.22,
            None,
            vec![Event::PointerButton {
                pos: padding,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(click, None);
        assert!(
            !behind_clicked,
            "clicking padding inside CopyTooltip must not click through"
        );
        assert!(tooltip.is_open());

        // Click on the copy icon: must report CopyClick::Icon and close the tooltip.
        let icon_center = pos2(
            box_rect.right() - PADDING_X - ICON.x / 2.0,
            box_rect.center().y,
        );
        let _ = run(
            &mut tooltip,
            1.25,
            None,
            vec![
                Event::PointerMoved(icon_center),
                Event::PointerButton {
                    pos: icon_center,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        let (click, behind_clicked) = run(
            &mut tooltip,
            1.27,
            None,
            vec![Event::PointerButton {
                pos: icon_center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(click.map(|(c, _)| c), Some(CopyClick::Icon));
        assert!(!behind_clicked);
        assert!(!tooltip.is_open());
    }
}
