//! The toast: the bottom-left card that reports an action, with its actions (`Undo`) and a close
//! button.
//!
//! The card is 48px tall at `left: calc(3.5rem + 40px - 1.75rem + 6px)`, with square corners, a
//! 3px accent bar inset on the left (`box-shadow: inset 3px 0 0`), 12px/16px text padded `1.75rem`
//! either side, bold uppercase actions padded `1rem`, and a 10px × at 30% opacity. The accent's
//! `::before` glow is on a box with no height, so it never shows.
//!
//! Over a photo ([`Behind::for_toasts`]) the card is see-through: white at .2 over the photo
//! blurred 12.5px, with white text.

use std::sync::Arc;

use egui::{
    Area, Color32, Context, CornerRadius, Galley, Id, LayerId, Order, Painter, Pos2, Rect,
    Response, Sense, Ui, Vec2,
    text::{LayoutJob, TextWrapping},
};

use crate::{
    Behind, BoxShadow, Family, Font, KeyTooltip, TextStyle, layout, motion, paint_icon, paint_text,
    palette, pixel_snap, rem,
};

const HEIGHT: f32 = 48.0;
/// `inset 3px 0 0 <accent>`.
const BAR: f32 = 3.0;
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const ACTION: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
/// Text and actions are vertically centred 16px line boxes nudged down 1px (`top: 1px`).
const LINE_TOP: f32 = (HEIGHT - 16.0) / 2.0 + 1.0;
/// The text's widest.
const MAX_TEXT_WIDTH: f32 = 500.0;
const CLOSE_SIZE: f32 = 10.0;
/// The close button. The icon's left edge is the actions' right edge, followed by its 10px padding
/// and 8px margin.
const CLOSE_TRAILING: f32 = CLOSE_SIZE + 10.0 + 8.0;
/// The separator between actions, shown on hover: 1px wide, 10px margins.
const SEPARATOR: f32 = 21.0;
/// A hidden action's expander opens to 131px on hover, over 200ms.
const EXPANDER: f32 = 131.0;
/// The hover shadows ease over .4s on this curve.
const SHADOW_CURVE: motion::CubicBezier = motion::CubicBezier::new(0.25, 0.46, 0.45, 0.94);
/// The default stack offset from the bottom.
pub const TOAST_BOTTOM: f32 = 32.0;
/// A see-through card's blur of what's behind it, and its white.
const BLUR: f32 = 12.5;
const SEE_THROUGH: Color32 = Color32::from_rgba_premultiplied(51, 51, 51, 51);
/// White at .75, a see-through card's text and actions.
const SEE_THROUGH_TEXT: Color32 = Color32::from_rgba_premultiplied(191, 191, 191, 191);

/// The toast's accent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    /// Information, the default.
    #[default]
    Info,
    /// Something failed.
    Error,
    /// A send.
    Send,
    /// Mark Done.
    MarkDone,
}

/// One action on a toast.
#[derive(Clone, Copy, Debug)]
pub struct ToastActionView<'a> {
    /// The label (`Undo`).
    pub name: &'a str,
    /// Its shortcut's keycaps, shown in a tooltip on hover (`["z"]`), or empty for none.
    pub keys: &'a [&'a str],
    /// The tooltip's title instead of the name (`Send message right now`).
    pub tooltip: Option<&'a str>,
    /// Shown only while the toast is hovered (`Instant Send ⚡`), sliding out of an expander.
    pub hidden: bool,
    /// Greyed and inert (a file notification's `Show` before the file is saved).
    pub disabled: bool,
}

/// A line of text to paint, shaped once, and its exact width.
struct Line {
    galley: Arc<Galley>,
    width: f32,
}

impl Line {
    fn new(ctx: &Context, job: LayoutJob) -> Self {
        let galley = layout(ctx, job);
        let width = galley.size().x;
        Self { galley, width }
    }
}

struct Layout {
    card: Rect,
    /// What the card shows through, if it's see-through.
    behind: Option<Behind>,
    text: Line,
    /// The percentage after the text, and the room it takes.
    percentage: Option<(Line, f32)>,
    actions: Vec<Line>,
    separators: f32,
    /// How far the hidden actions' expander is open.
    expander: f32,
    status: f32,
    padding: f32,
}

/// What happened to a toast this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToastResponse {
    /// The pointer is over the toast.
    pub hovered: bool,
    /// An action was clicked (its index).
    pub clicked: Option<usize>,
    /// The × was clicked.
    pub closed: bool,
    /// The text was clicked, on a toast whose text takes clicks.
    pub text_clicked: bool,
}

/// A toast to paint.
#[derive(Clone, Copy, Debug)]
pub struct ToastView<'a> {
    /// The message (`Marked as Done.`).
    pub text: &'a str,
    /// The accent.
    pub kind: ToastKind,
    /// Its actions, left to right.
    pub actions: &'a [ToastActionView<'a>],
    /// The lines between actions show without hovering.
    pub divided: bool,
    /// How much of a file has downloaded, after its name.
    pub percentage: Option<u32>,
    /// The text takes clicks, turning blue under the pointer (a saved file's notification previews
    /// it).
    pub clickable: bool,
}

/// The percentage: `min-width: 30px; margin-left: 4px`.
const PERCENTAGE_MIN: f32 = 30.0;
const PERCENTAGE_GAP: f32 = 4.0;
/// Clickable text under the pointer, in both themes.
const CLICKABLE_HOVER: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);

/// The toasts' stack's cards are each an area of this order.
const STACK: Order = Order::Foreground;

impl ToastView<'_> {
    /// The area a card of the toasts' stack is shown in.
    pub fn area(id: Id) -> Area {
        Area::new(id).order(STACK)
    }

    /// The layer a card of the toasts' stack shown with `id` is painted on.
    pub fn layer(id: Id) -> LayerId {
        LayerId::new(STACK, id)
    }

    /// Keeps the toasts' stack's `cards` (their [`ToastView::layer`]s) in this order, back to
    /// front, however they came up, and with `over_the_rest` over the other areas of their order
    /// (the dialogs), which leaves alone an area raised over them (the command palette). egui
    /// raises an area when it shows, so a popup raised after the toast, or a dialog opened while
    /// it shows, would otherwise cover it. Call it each frame, once they're painted: the first
    /// card out of place goes back where it belongs, which puts a newly raised one right the
    /// frame it shows.
    pub fn stack(ctx: &Context, cards: &[LayerId], over_the_rest: bool) {
        if over_the_rest {
            // Raised together, they keep their order among themselves.
            for &card in cards {
                ctx.move_to_top(card);
            }
        }
        let order: Vec<LayerId> = ctx.memory(|memory| {
            memory
                .layer_ids()
                .filter(|layer| cards.contains(layer))
                .collect()
        });
        if order.len() != cards.len() {
            return;
        }
        match cards.iter().zip(&order).position(|(card, is)| card != is) {
            // The back card goes under the rest (egui keeps a layer's sublayers in their order,
            // just over it).
            Some(0) => {
                for &card in &cards[1..] {
                    ctx.set_sublayer(cards[0], card);
                }
            }
            Some(at) => ctx.set_sublayer(cards[at - 1], cards[at]),
            None => {}
        }
    }

    /// Paints the toast at the bottom left of `area` (the window below the tab bar), its bottom
    /// edge `bottom` px above the area's ([`TOAST_BOTTOM`] normally), its card at `opacity` (it
    /// fades out by fading the card).
    pub fn show(
        &self,
        ctx: &Context,
        id: Id,
        area: Rect,
        bottom: f32,
        opacity: f32,
    ) -> ToastResponse {
        let palette = palette(ctx);
        let rem = rem(area.width(), false);
        // `left: calc(3.5rem + 40px - 1.75rem + 6px)`, with 44px instead of 3.5rem at ≤1152px.
        let inset = if area.width() <= 1152.0 {
            44.0
        } else {
            3.5 * rem
        };
        let left = area.left() + inset + 40.0 - 1.75 * rem + 6.0;
        let top = area.bottom() - bottom - HEIGHT;
        let behind = Behind::for_toasts(ctx);
        let see_through = behind.is_some();

        let text = {
            // The text: `rgba(0,0,0,.7)`, `hsla(0,0%,100%,.7)` in Carbon.
            let ink = match palette.theme {
                _ if see_through => SEE_THROUGH_TEXT,
                crate::Theme::Snow => Color32::from_black_alpha(179),
                crate::Theme::Carbon => Color32::from_white_alpha(179),
            };
            let mut job = LayoutJob::single_section(self.text.to_owned(), TEXT.format(ink));
            job.wrap = TextWrapping::truncate_at_width(MAX_TEXT_WIDTH);
            Line::new(ctx, job)
        };
        let percentage = self.percentage.map(|percent| {
            // `rgba(0,0,0,.3)`, `hsla(0,0%,100%,.3)` in Carbon.
            let ink = if see_through {
                SEE_THROUGH_TEXT
            } else {
                palette.ink(0.3, 0.3)
            };
            let line = Line::new(
                ctx,
                LayoutJob::single_section(format!("{percent}%"), TEXT.format(ink)),
            );
            let room = PERCENTAGE_GAP + line.width.max(PERCENTAGE_MIN);
            (line, room)
        });
        let action_color = match self.kind {
            _ if see_through => SEE_THROUGH_TEXT,
            ToastKind::Error => Color32::from_rgb(0xef, 0x8e, 0x8e),
            _ => palette.link_text_primary,
        };
        // Actions are `text-transform: uppercase`.
        let actions: Vec<_> = self
            .actions
            .iter()
            // A disabled action: black or white at .3, white at .4 when see-through.
            .map(|action| {
                let color = match (action.disabled, see_through) {
                    (true, true) => Color32::from_white_alpha(102),
                    (true, false) => palette.ink(0.3, 0.3),
                    (false, _) => action_color,
                };
                Line::new(
                    ctx,
                    LayoutJob::single_section(action.name.to_uppercase(), ACTION.format(color)),
                )
            })
            .collect();

        // The separators appear while hovered, which widens the card, so hover is last frame's.
        let hover_id = id.with("hovered");
        let was_hovered = ctx
            .data(|data| data.get_temp::<bool>(hover_id))
            .unwrap_or(false);
        let separators = motion::transition(
            ctx,
            id.with("separators"),
            was_hovered || self.divided,
            0.15,
            motion::CubicBezier::EASE_IN,
        );
        let expander = motion::transition(
            ctx,
            id.with("expander"),
            was_hovered,
            0.2,
            motion::CubicBezier::EASE,
        );
        let shown = self.actions.iter().filter(|action| !action.hidden).count();
        let actions_width: f32 = actions
            .iter()
            .zip(self.actions)
            .map(|(line, action)| {
                if action.hidden {
                    expanded_width(line.width, expander)
                } else {
                    line.width
                }
            })
            .sum::<f32>()
            + SEPARATOR * separators * shown.saturating_sub(1) as f32;
        let status = 1.75 * rem;
        // The actions' box (padded `1rem`) is only there when there are actions.
        let padding = if self.actions.is_empty() { 0.0 } else { rem };
        let width = 2.0 * status
            + text.width
            + percentage.as_ref().map_or(0.0, |(_, room)| *room)
            + 2.0 * padding
            + actions_width
            + CLOSE_TRAILING;
        let card = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, HEIGHT));

        Self::area(id)
            .fixed_pos(card.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(card, Sense::hover());
                // `contains_pointer`, because the actions on top of the card take `hovered`.
                let hovered = response.contains_pointer();
                ctx.data_mut(|data| data.insert_temp(hover_id, hovered));
                let mut painter = ui.painter().clone();
                painter.multiply_opacity(opacity);
                self.paint(
                    ui,
                    &painter,
                    id,
                    Layout {
                        card,
                        behind,
                        text,
                        percentage,
                        actions,
                        separators,
                        expander,
                        status,
                        padding,
                    },
                    hovered,
                )
            })
            .inner
    }

    fn paint(
        &self,
        ui: &Ui,
        painter: &Painter,
        id: Id,
        layout: Layout,
        hovered: bool,
    ) -> ToastResponse {
        let ctx = ui.ctx();
        let palette = palette(ctx);
        let Layout {
            card,
            behind,
            text,
            percentage,
            actions,
            separators,
            expander,
            status,
            padding,
        } = layout;
        let top = card.top();
        let shadow = motion::transition(ctx, id.with("shadow"), hovered, 0.4, SHADOW_CURVE);
        // Boxes snap to whole pixels; text keeps its fractional position.
        let snapped = pixel_snap(card);
        // Info and error toasts carry an extra faint shadow.
        let faint = matches!(self.kind, ToastKind::Info | ToastKind::Error);
        paint_shadow(painter, snapped, shadow, faint, behind.is_some());
        // The card: `#fff`, `#464b53` in Carbon, or see-through.
        if let Some(behind) = &behind {
            behind.paint(painter, snapped, BLUR);
            painter.rect_filled(snapped, 0.0, SEE_THROUGH);
        } else {
            let background = match palette.theme {
                crate::Theme::Snow => Color32::WHITE,
                crate::Theme::Carbon => Color32::from_rgb(0x46, 0x4b, 0x53),
            };
            painter.rect_filled(snapped, 0.0, background);
        }
        let accent = match self.kind {
            ToastKind::Info => palette.link_border_primary,
            ToastKind::Error => palette.border_critical_default,
            ToastKind::Send => Color32::from_rgb(0xac, 0x88, 0xe6),
            ToastKind::MarkDone => palette.border_success_default,
        };
        painter.rect_filled(
            Rect::from_min_size(snapped.min, Vec2::new(BAR, HEIGHT)),
            0.0,
            accent,
        );

        let mut result = ToastResponse {
            hovered,
            ..ToastResponse::default()
        };
        let mut x = card.left() + status;
        let mut text_galley = text.galley;
        if self.clickable {
            // The text's box, its padding included, takes the click.
            let room = percentage.as_ref().map_or(0.0, |(_, room)| *room);
            let area = Rect::from_min_size(
                card.min,
                Vec2::new(2.0 * status + text.width + room, HEIGHT),
            );
            let response = ui
                .interact(area, id.with("status"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            result.text_clicked = response.clicked();
            // `transition: color 150ms`, on the text alone.
            let text_box =
                Rect::from_min_size(Pos2::new(x, top + LINE_TOP), Vec2::new(text.width, 16.0));
            let over = response
                .hover_pos()
                .is_some_and(|pos| text_box.contains(pos));
            let blue = motion::transition(
                ctx,
                id.with("text_hover"),
                over,
                0.15,
                motion::CubicBezier::EASE,
            );
            if blue > 0.0
                && let Some(base) = text_galley.job.sections.first().map(|s| s.format.color)
            {
                text_galley = crate::text::recolor_galley(
                    &text_galley,
                    motion::mix(base, CLICKABLE_HOVER, blue),
                );
            }
        }
        paint_text(painter, Pos2::new(x, top + LINE_TOP), text_galley, &TEXT);
        x += text.width;
        if let Some((line, room)) = percentage {
            paint_text(
                painter,
                Pos2::new(x + PERCENTAGE_GAP, top + LINE_TOP),
                line.galley,
                &TEXT,
            );
            x += room;
        }
        x += status + padding;
        // The separators: 15% black, 15% white in Carbon.
        let (ink, separator) = match palette.theme {
            crate::Theme::Snow => (Color32::BLACK, Color32::from_black_alpha(38)),
            crate::Theme::Carbon => (Color32::WHITE, Color32::from_white_alpha(38)),
        };
        for (i, (line, action)) in actions.into_iter().zip(self.actions).enumerate() {
            if action.hidden {
                // The expander clips the separator and action to its animated width. The action
                // fades in with it (`opacity 200ms`).
                let width = expanded_width(line.width, expander);
                if width <= 0.0 {
                    continue;
                }
                let clip = Rect::from_min_size(Pos2::new(x, card.top()), Vec2::new(width, HEIGHT));
                let mut clipped = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
                paint_separator(&clipped, x, top, 1.0, separator);
                let at = x + SEPARATOR;
                clipped.multiply_opacity(expander);
                let hit = Rect::from_min_size(
                    Pos2::new(at, top + LINE_TOP - 18.0),
                    Vec2::new(line.width, 52.0),
                )
                .intersect(clip);
                let action_response = ui.interact(hit, id.with(("action", i)), Sense::click());
                if action_response.clicked() {
                    result.clicked = Some(i);
                }
                paint_text(
                    &clipped,
                    Pos2::new(at, top + LINE_TOP),
                    line.galley,
                    &ACTION,
                );
                show_action_tooltip(ui, &action_response, action);
                x += width;
                continue;
            }
            if i > 0 {
                paint_separator(painter, x, top, separators, separator);
                x += SEPARATOR * separators;
            }
            // An action is padded `18px 0`, a 52px box around the 16px line.
            let hit = Rect::from_min_size(
                Pos2::new(x, top + LINE_TOP - 18.0),
                Vec2::new(line.width, 52.0),
            );
            let action_response = ui.interact(
                hit,
                id.with(("action", i)),
                if action.disabled {
                    Sense::hover()
                } else {
                    Sense::click()
                },
            );
            if action_response.clicked() {
                result.clicked = Some(i);
            }
            paint_text(painter, Pos2::new(x, top + LINE_TOP), line.galley, &ACTION);
            show_action_tooltip(ui, &action_response, action);
            x += line.width;
        }
        x += padding;

        // The close button: padding 10px around the icon.
        let icon = Rect::from_min_size(
            Pos2::new(x, top + (HEIGHT - CLOSE_SIZE) / 2.0),
            Vec2::splat(CLOSE_SIZE),
        );
        let close = ui.interact(icon.expand(10.0), id.with("close"), Sense::click());
        let close_opacity = 0.3
            + 0.2
                * motion::transition(
                    ctx,
                    id.with("close_hover"),
                    close.hovered() || behind.is_some(),
                    0.15,
                    motion::CubicBezier::EASE,
                );
        // The × is black (white in Carbon) at 30%, 50% hovered. Its 12px SVG is drawn at 10px.
        // See-through, it's white at 50%.
        let ink = if behind.is_some() {
            Color32::WHITE
        } else {
            ink
        };
        paint_icon(
            ui,
            "legacy/close",
            icon,
            ink.gamma_multiply(close_opacity * painter.opacity()),
        );
        result.closed = close.clicked();
        result
    }
}

/// `0 6px 30px 2px rgba(0,0,0,.2)` (after `0 6px 30px 2px rgba(0,0,0,.04)` when `faint`),
/// easing to `0 8px 30px 2px rgba(0,0,0,.1), 0 10px 30px 2px rgba(0,0,0,.3)` on hover
/// (cross-faded, as an approximation of CSS's shadow interpolation). Under a see-through card
/// it's only outside the card, as CSS clips a box-shadow.
fn paint_shadow(painter: &Painter, card: Rect, hover: f32, faint: bool, see_through: bool) {
    let black = |alpha: f32| Color32::from_black_alpha((alpha * 255.0).round() as u8);
    let paint = |shadow: BoxShadow| {
        if see_through {
            shadow.paint_outside(painter, card);
        } else {
            shadow.paint(painter, card, CornerRadius::ZERO);
        }
    };
    if hover < 1.0 {
        if faint {
            paint(BoxShadow::new(
                [0.0, 6.0],
                30.0,
                2.0,
                black(0.04 * (1.0 - hover)),
            ));
        }
        paint(BoxShadow::new(
            [0.0, 6.0],
            30.0,
            2.0,
            black(0.2 * (1.0 - hover)),
        ));
    }
    if hover > 0.0 {
        paint(BoxShadow::new([0.0, 8.0], 30.0, 2.0, black(0.1 * hover)));
        paint(BoxShadow::new([0.0, 10.0], 30.0, 2.0, black(0.3 * hover)));
    }
}

fn paint_separator(painter: &Painter, x: f32, top: f32, progress: f32, color: Color32) {
    if progress <= 0.0 {
        return;
    }
    let rect = Rect::from_min_size(
        Pos2::new(x + 10.0 * progress, top + 16.5),
        Vec2::new(progress, 15.0),
    );
    painter.rect_filled(rect, 0.0, color.gamma_multiply(progress));
}

/// A hidden action's width in its expander: its separator and label, capped at the expander's
/// animated `max-width`.
fn expanded_width(label: f32, expander: f32) -> f32 {
    (SEPARATOR + label).min(EXPANDER * expander)
}

fn show_action_tooltip(ui: &Ui, response: &Response, action: &ToastActionView<'_>) {
    if action.keys.is_empty() {
        return;
    }
    if let Some(title) = action.tooltip {
        KeyTooltip {
            text: title,
            keys: action.keys,
        }
        .show_above(ui, response, 10.0);
        return;
    }
    // The tooltip title-cases the action's name.
    let mut name = String::with_capacity(action.name.len());
    let mut word_start = true;
    for c in action.name.chars() {
        if word_start {
            name.extend(c.to_uppercase());
        } else {
            name.push(c);
        }
        word_start = c == ' ';
    }
    // Attached to the action's 52px box with a 10px offset into it.
    KeyTooltip {
        text: &name,
        keys: action.keys,
    }
    .show_above(ui, response, 10.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows the stack's cards with these ids for a frame (and the areas `others`, after), then
    /// keeps them in `order`.
    fn frame(ctx: &Context, shown: &[Id], others: &[Id], order: &[Id], over_the_rest: bool) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for &id in shown.iter().chain(others) {
                ToastView::area(id).show(ui.ctx(), |ui| ui.allocate_space(Vec2::splat(10.0)));
            }
            let cards: Vec<LayerId> = order.iter().map(|&id| ToastView::layer(id)).collect();
            ToastView::stack(ui.ctx(), &cards, over_the_rest);
        });
        output.textures_delta.clear();
    }

    /// Where each of `ids` is, back to front.
    fn depths(ctx: &Context, ids: &[Id]) -> Vec<usize> {
        let order: Vec<LayerId> = ctx.memory(|memory| memory.layer_ids().collect());
        ids.iter()
            .map(|&id| {
                order
                    .iter()
                    .position(|layer| *layer == ToastView::layer(id))
                    .expect("shown")
            })
            .collect()
    }

    #[test]
    fn the_toast_stays_over_cards_raised_after_it() {
        let ctx = Context::default();
        let (toast, popup, file) = (Id::new("toast"), Id::new("popup"), Id::new("file"));
        frame(&ctx, &[toast], &[], &[toast], true);
        // A popup comes up after the toast, which egui raises over it.
        frame(&ctx, &[toast, popup], &[], &[popup, toast], true);
        let [toast_at, popup_at] = depths(&ctx, &[toast, popup])[..] else {
            unreachable!()
        };
        assert!(toast_at > popup_at, "the toast is over the popup");
        // Then a file notification: over the popup, under the toast.
        frame(
            &ctx,
            &[toast, popup, file],
            &[],
            &[popup, file, toast],
            true,
        );
        let depths = depths(&ctx, &[popup, file, toast]);
        assert!(depths.is_sorted(), "{depths:?}");
    }

    #[test]
    fn the_stack_goes_over_a_dialog_unless_told_to_leave_what_is_over_it() {
        let ctx = Context::default();
        let (toast, dialog) = (Id::new("toast"), Id::new("dialog"));
        frame(&ctx, &[toast], &[], &[toast], true);
        // A dialog opens while the toast shows.
        frame(&ctx, &[toast], &[dialog], &[toast], true);
        frame(&ctx, &[toast], &[dialog], &[toast], true);
        let [toast_at, dialog_at] = depths(&ctx, &[toast, dialog])[..] else {
            unreachable!()
        };
        assert!(toast_at > dialog_at, "the toast is over the dialog");
        // The palette opens over it, and the toast stays under.
        let palette = Id::new("palette");
        frame(&ctx, &[toast], &[dialog, palette], &[toast], false);
        frame(&ctx, &[toast], &[dialog, palette], &[toast], false);
        let [toast_at, palette_at] = depths(&ctx, &[toast, palette])[..] else {
            unreachable!()
        };
        assert!(palette_at > toast_at, "the palette is over the toast");
    }
}
