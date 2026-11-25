//! The find field, a tab-strip item between the tabs and + that is 240px wide while open. It holds
//! the text field, the `{current}/{total}` count and a close button.

use egui::{
    Color32, CornerRadius, CursorIcon, Rect, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    text::CCursorRange, vec2,
};
use exo_ui::{
    Family, Font, TextStyle, Theme,
    motion::{self, CubicBezier},
};

use super::{field_id, set_query, share, with};

/// The field's text and count: 13px/22px in the system font.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::System, 400), 13.0, 22.0);
/// The field's width while open.
const WIDTH: f32 = 240.0;
/// A tab-strip item is 24px tall, inside which the field's 22px line sits under a 1px border.
const LINE: f32 = 22.0;
/// The close button (16px, the glyph in 4px of padding), with 6px before it.
const CLOSE: f32 = 16.0;
const CLOSE_MARGIN: f32 = 6.0;
/// The field's border while it has the keyboard, in Snow and Carbon.
const FOCUS_BORDER: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
/// The field's selected text: a blue at 20%.
const SELECTION: Color32 = Color32::from_rgba_unmultiplied_const(64, 113, 245, 51);

/// Where the field is in its transitions between collapsed and open, each with its own
/// duration, delay and curve, which differ by direction.
struct Geometry {
    margin: f32,
    width: f32,
    border: f32,
    padding: f32,
    /// The text field's and the count's opacity, and the close button's.
    contents: f32,
    close: f32,
}

fn geometry(ctx: &egui::Context) -> Geometry {
    let open = with(ctx, |find| find.open);
    let id = exo_ui::id(ctx, "exo::find::bar");
    // Each value takes its open value, its opening duration and delay, the same for closing, and
    // its curve.
    let value =
        |name: &str, on: f32, opening: (f32, f32), closing: (f32, f32), curve: CubicBezier| {
            let (duration, delay) = if open { opening } else { closing };
            motion::transition_value(
                ctx,
                id.with(name),
                if open { on } else { 0.0 },
                duration,
                delay,
                curve,
            )
        };
    // A step (`step-start` opening, `step-end` closing) is a change after its delay alone.
    const STEP: f32 = 0.001;
    Geometry {
        // `width 175ms linear 25ms` both ways.
        width: value(
            "width",
            WIDTH,
            (0.175, 0.025),
            (0.175, 0.025),
            CubicBezier::LINEAR,
        ),
        // `margin 25ms linear`, closing `25ms linear 200ms`.
        margin: value(
            "margin",
            8.0,
            (0.025, 0.0),
            (0.025, 0.2),
            CubicBezier::LINEAR,
        ),
        // `border-width 175ms step-start 25ms`, closing `step-end`.
        border: value(
            "border",
            1.0,
            (STEP, 0.025),
            (STEP, 0.2),
            CubicBezier::LINEAR,
        )
        .round(),
        // `padding 25ms linear`, closing `175ms linear 25ms`.
        padding: value(
            "padding",
            6.0,
            (0.025, 0.0),
            (0.175, 0.025),
            CubicBezier::LINEAR,
        ),
        // `opacity 175ms ease`, and the close button's own is `opacity 50ms`.
        contents: value(
            "contents",
            1.0,
            (0.175, 0.0),
            (0.175, 0.0),
            CubicBezier::EASE,
        ),
        close: value("close", 1.0, (0.05, 0.0), (0.05, 0.0), CubicBezier::EASE),
    }
}

/// The room the field takes in the strip this frame, in the strip's CSS px: its left margin and
/// its width.
pub(crate) fn layout(ctx: &egui::Context) -> (f32, f32) {
    let geometry = geometry(ctx);
    (geometry.margin, geometry.width)
}

/// Draws the field in `rect`, its border box, as wide as [`layout`] has it. `s` is the strip's
/// scale in points per CSS px (the strip doesn't zoom with the page), and `border` is a border
/// like the tabs'. `close` draws the close button (the tabs' ×) in its rect at an opacity and says
/// whether it was clicked.
pub(crate) fn show(
    ui: &mut Ui,
    rect: Rect,
    s: f32,
    border: Color32,
    close: impl FnOnce(&mut Ui, Rect, f32) -> bool,
) {
    let ctx = ui.ctx().clone();
    let field_id = field_id(&ctx);
    let geometry = geometry(&ctx);
    if rect.width() <= 0.0 {
        return;
    }
    let (edge, padding, close_size, line_height) = (
        geometry.border * s,
        geometry.padding * s,
        CLOSE * s,
        LINE * s,
    );
    let style = TextStyle::new(TEXT.font, TEXT.size * s, TEXT.line_height * s);
    let (focused, select, query) = with(&ctx, |find| {
        (
            find.focused,
            std::mem::take(&mut find.select),
            find.query.clone(),
        )
    });
    let carbon = exo_ui::theme(&ctx) == Theme::Carbon;
    // The text is black at .85, or `hsla(0,0%,100%,.9)` in Carbon. The count is black at .3, or
    // white at .5 in Carbon.
    let (text, count_color) = if carbon {
        (
            Color32::from_white_alpha(230),
            Color32::from_white_alpha(128),
        )
    } else {
        (
            Color32::from_black_alpha(217),
            Color32::from_black_alpha(77),
        )
    };

    let response = ui.interact(
        rect,
        exo_ui::id(ui.ctx(), "exo::find::box"),
        egui::Sense::click(),
    );
    // A pointing hand over the item, but the default cursor over the text field.
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if geometry.border > 0.0 {
        let stroke = if focused { FOCUS_BORDER } else { border };
        painter.rect_stroke(
            rect,
            CornerRadius::from(4.0 * s),
            Stroke::new(edge, stroke),
            StrokeKind::Inside,
        );
    }
    // A flex row in the content box (`padding-left` only), where the input grows into what the
    // count and the close button leave.
    let content = Rect::from_min_max(
        rect.min + vec2(edge + padding, edge),
        rect.max - vec2(edge, edge),
    );
    let shown = exo_ui::find::count(&ctx)
        .filter(|_| !query.is_empty() && with(&ctx, |find| find.searching));
    let count = shown
        .map(|(current, total)| format!("{current}/{total}"))
        .unwrap_or_default();
    let count_width = if count.is_empty() {
        0.0
    } else {
        style.width(&ctx, &count)
    };
    let count_right = content.right() - close_size - CLOSE_MARGIN * s;
    let line = Rect::from_min_size(
        content.min,
        vec2(
            (count_right - count_width - content.left()).max(0.0),
            line_height,
        ),
    );

    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(painter.clip_rect());
    child.multiply_opacity(geometry.contents);
    if !count.is_empty() {
        style.paint_line(
            child.painter(),
            pos2(count_right - count_width, content.bottom() - line_height),
            &count,
            count_color,
            None,
        );
    }
    if select {
        exo_ui::focus_field(
            &ctx,
            field_id,
            CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(query.chars().count()),
            ),
        );
    } else if focused {
        ctx.memory_mut(|memory| memory.request_focus(field_id));
    }
    // The field's keys were taken from the page at the start of the frame; swap them into
    // `input.events` strictly around `text_field` so non-keyboard frame events are restored intact.
    let mut events = with(&ctx, |find| std::mem::take(&mut find.events));
    ctx.input_mut(|input| std::mem::swap(&mut input.events, &mut events));
    let mut edited = query.clone();
    let field = exo_ui::text_field(
        &mut child,
        field_id,
        &mut edited,
        line,
        &style,
        text,
        SELECTION,
    );
    ctx.input_mut(|input| std::mem::swap(&mut input.events, &mut events));
    if field.hovered() {
        ctx.set_cursor_icon(CursorIcon::Default);
    }
    if edited != query {
        // Typing searches as it goes, and an empty field stops.
        set_query(&ctx, edited.clone(), true);
        share(&ctx, &edited);
    }

    // The close button, bottom-aligned, 3px up and 3px in from the right.
    let button = Rect::from_min_size(
        pos2(
            content.right() - close_size - 3.0 * s,
            content.bottom() - close_size - 3.0 * s,
        ),
        vec2(close_size, close_size),
    );
    let closed = close(ui, button, geometry.close);
    if closed {
        super::close(&ctx);
        return;
    }
    // A press in the field gives it the keyboard, and a press anywhere else on the window takes
    // the keyboard away (the tab bar's web contents lose focus to the page's).
    if ui.input(|input| input.pointer.primary_pressed()) {
        let inside = response.hovered() || field.hovered();
        let previous = ctx
            .memory(|memory| memory.focused())
            .filter(|id| *id != field_id);
        with(&ctx, |find| {
            if inside && find.open && !find.focused {
                find.returns_focus_to = previous;
            }
            find.focused = inside && find.open;
            if !find.focused {
                find.returns_focus_to = None;
                find.composing = false;
            }
        });
        if !inside {
            ctx.memory_mut(|memory| memory.surrender_focus(field_id));
        }
    }
}
