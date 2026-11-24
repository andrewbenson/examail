//! Text dragged across the window's edge, by the Mac's own drag (`NSDraggingSession`), which egui
//! never sees: text from another app over the window, and an editor's selection dragged out of
//! it. The app's drag bridge says at the start of the window's frame what is over the window and
//! what was let go on it ([`begin`]). Editors and fields say where they take text and what they
//! drag out as they draw, and the bridge reads it at the frame's end ([`end`]).
//!
//! Without a bridge (tests, a window off the main thread) nothing is ever over the window, and an
//! editor keeps a dragged selection inside itself ([`native`] is false).

use egui::{Color32, ColorImage, Context, Id, Pos2, Rect, Ui, text::CCursorRange};

use crate::TextStyle;

/// A drag over the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Over {
    /// The pointer, in the window.
    pub at: Pos2,
    /// The editor whose selection is dragged, when the drag is Exo's. When it's let go in the
    /// window, the text moves.
    pub source: Option<Id>,
}

/// Text let go over the window.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    /// Where, in the window.
    pub at: Pos2,
    /// Its HTML, when the drag has some.
    pub html: Option<String>,
    /// Its plain text.
    pub text: String,
    /// [`Over::source`]'s.
    pub source: Option<Id>,
    /// Whether ⌥ was held (or the drag allows no move), which makes it a copy.
    pub copy: bool,
}

/// An editor's selection dragged out of it, for the bridge to carry on as the Mac's drag.
#[derive(Clone, Debug)]
pub struct Outgoing {
    /// The editor.
    pub source: Id,
    /// The selection as HTML and as text, as a copy puts them on the clipboard.
    pub html: String,
    pub text: String,
    /// The dragged text's picture (premultiplied, at the window's pixels per point) and where it
    /// is in the window now.
    pub image: Option<(ColorImage, Rect)>,
}

/// What a frame's editors and fields left for the bridge.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Where text can be let go, in the window.
    pub targets: Vec<Rect>,
    /// A selection dragged out.
    pub outgoing: Option<Outgoing>,
    /// Who took the frame's drop.
    pub taken: Option<Id>,
}

/// The window's drag in this frame.
#[derive(Clone, Debug, Default)]
struct Frame {
    native: bool,
    over: Option<Over>,
    dropped: Option<Dropped>,
    /// The bridge's drag of an editor's selection is over: whose, and whether it moved.
    ended: Option<(Id, bool)>,
    report: Report,
}

fn frame_id(ctx: &Context) -> Id {
    crate::id(ctx, "exo_ui::text_drag")
}

fn with<R>(ctx: &Context, f: impl FnOnce(&mut Frame) -> R) -> R {
    let id = frame_id(ctx);
    ctx.data_mut(|data| f(data.get_temp_mut_or_default::<Frame>(id)))
}

/// The bridge, as the window's frame starts: whether a selection can be dragged out of it
/// (`native`), what's over it, what was let go on it, and the end of an editor's drag
/// (`ended`: its id, and whether the text moved to another place in this window).
pub fn begin(
    ctx: &Context,
    native: bool,
    over: Option<Over>,
    dropped: Option<Dropped>,
    ended: Option<(Id, bool)>,
) {
    with(ctx, |frame| {
        *frame = Frame {
            native,
            over,
            dropped,
            ended,
            report: Report::default(),
        }
    });
}

/// The bridge, as the window's frame ends: what its editors and fields left.
pub fn end(ctx: &Context) -> Report {
    with(ctx, |frame| std::mem::take(&mut frame.report))
}

/// Whether a selection dragged out of an editor goes on as the Mac's drag.
pub fn native(ctx: &Context) -> bool {
    with(ctx, |frame| frame.native)
}

/// The drag over the window, if any.
pub fn over(ctx: &Context) -> Option<Over> {
    with(ctx, |frame| frame.over)
}

/// Says text let go in `rect` this frame is taken.
pub fn accept(ctx: &Context, rect: Rect) {
    if rect.is_positive() {
        with(ctx, |frame| frame.report.targets.push(rect));
    }
}

/// The text let go in `rect`, for `taker` to put in. Once it's taken, nobody else gets it.
pub fn take_drop(ctx: &Context, rect: Rect, taker: Id) -> Option<Dropped> {
    with(ctx, |frame| {
        let dropped = frame.dropped.take_if(|dropped| rect.contains(dropped.at))?;
        frame.report.taken = Some(taker);
        Some(dropped)
    })
}

/// Hands the bridge a selection dragged out of an editor.
pub fn drag_out(ctx: &Context, outgoing: Outgoing) {
    with(ctx, |frame| frame.report.outgoing = Some(outgoing));
}

/// Whether the bridge's drag of `source`'s selection ended, and if so whether its text moved to
/// another place in the window (which takes it out of `source`).
pub fn ended(ctx: &Context, source: Id) -> Option<bool> {
    with(ctx, |frame| {
        frame
            .ended
            .take_if(|(id, _)| *id == source)
            .map(|(_, moved)| moved)
    })
}

/// A one-line field's part in a drag (an `<input>`'s, as Chromium has it) for field `id`, with its
/// `text` on `line` in `style`. While text is dragged over the line, a caret in `color` shows where
/// it would go. When it's let go there, the text goes in at that character with its line breaks
/// as spaces (`ReplaceEOLAndLimitLength`), selected, and the field is focused. Returns where the
/// dropped text is in `text`, in characters.
pub fn field(
    ui: &Ui,
    id: Id,
    text: &mut String,
    line: Rect,
    style: &TextStyle,
    color: Color32,
) -> Option<std::ops::Range<usize>> {
    let ctx = ui.ctx();
    let visible = line.intersect(ui.clip_rect());
    accept(ctx, visible);
    if let Some(over) = over(ctx).filter(|over| visible.contains(over.at)) {
        let index = crate::field_index_scrolled(ctx, id, text, line, style, over.at.x);
        let x = crate::field_x_scrolled(ctx, id, text, line, style, index);
        let pixels_per_point = ctx.pixels_per_point();
        let x = (x * pixels_per_point).round() / pixels_per_point;
        ui.painter().with_clip_rect(visible).rect_filled(
            Rect::from_x_y_ranges(x..=x + 1.0, line.y_range()),
            0.0,
            color,
        );
    }
    let dropped = take_drop(ctx, visible, id)?;
    let index = crate::field_index_scrolled(ctx, id, text, line, style, dropped.at.x);
    let inserted = dropped.text.replace("\r\n", " ").replace(['\r', '\n'], " ");
    let at = text
        .char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte);
    text.insert_str(at, &inserted);
    let range = index..index + inserted.chars().count();
    crate::focus_field(
        ctx,
        id,
        CCursorRange::two(
            egui::text::CCursor::new(range.start),
            egui::text::CCursor::new(range.end),
        ),
    );
    Some(range)
}

#[cfg(test)]
mod tests {
    use egui::{Context, Id, Rect, pos2, vec2};

    use super::*;

    #[test]
    fn a_drop_goes_to_the_first_taker_under_it_and_is_reported() {
        let ctx = Context::default();
        let dropped = Dropped {
            at: pos2(50.0, 50.0),
            html: None,
            text: "hi".into(),
            source: None,
            copy: false,
        };
        begin(&ctx, true, None, Some(dropped.clone()), None);
        let elsewhere = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0));
        let under = Rect::from_min_size(pos2(40.0, 40.0), vec2(20.0, 20.0));
        assert_eq!(take_drop(&ctx, elsewhere, Id::new("a")), None);
        assert_eq!(take_drop(&ctx, under, Id::new("b")), Some(dropped));
        assert_eq!(take_drop(&ctx, under, Id::new("c")), None, "taken once");
        accept(&ctx, under);
        let report = end(&ctx);
        assert_eq!(report.taken, Some(Id::new("b")));
        assert_eq!(report.targets, vec![under]);
        assert!(
            end(&ctx).targets.is_empty(),
            "a frame's report is read once"
        );
    }

    #[test]
    fn an_ended_drag_is_its_sources_only() {
        let ctx = Context::default();
        begin(&ctx, true, None, None, Some((Id::new("a"), true)));
        assert_eq!(ended(&ctx, Id::new("b")), None);
        assert_eq!(ended(&ctx, Id::new("a")), Some(true));
        assert_eq!(ended(&ctx, Id::new("a")), None);
    }

    #[test]
    fn scrolled_field_drop_uses_scrolled_index_and_clips_caret() {
        let ctx = Context::default();
        crate::install(&ctx).expect("system fonts");
        let style = TextStyle::new(crate::Font::new(crate::Family::Sans, 400), 14.0, 22.0);
        let id = Id::new("scrolled_drop_field");
        let line = Rect::from_min_size(pos2(40.0, 20.0), vec2(80.0, 22.0));
        let mut text = "prefix words before overflowing tail".to_owned();
        let len = text.chars().count();
        let caret_color = Color32::from_rgb(200, 30, 30);

        // Scroll field to its end.
        crate::focus_field(&ctx, id, CCursorRange::one(egui::text::CCursor::new(len)));
        for _ in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = crate::text_field(
                    ui,
                    id,
                    &mut text,
                    line,
                    &style,
                    Color32::BLACK,
                    Color32::LIGHT_BLUE,
                );
            })
            .drop_without_applying_deltas();
        }
        assert!(crate::field_scroll_offset(&ctx, id) > 40.0);

        // Drag over the middle of the visible line: caret clip stays within `line`.
        let hover_at = pos2(line.center().x, line.center().y);
        begin(
            &ctx,
            true,
            Some(Over {
                at: hover_at,
                source: None,
            }),
            None,
            None,
        );
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = field(ui, id, &mut text, line, &style, caret_color);
        });
        let shapes = std::mem::take(&mut output.shapes);
        output.drop_without_applying_deltas();
        let caret = shapes
            .iter()
            .find(|item| matches!(&item.shape, egui::Shape::Rect(r) if r.fill == caret_color))
            .expect("drop caret painted");
        assert!(
            caret.clip_rect.left() >= line.left() - 0.01
                && caret.clip_rect.right() <= line.right() + 0.01,
            "caret clipped to visible line: {:?}",
            caret.clip_rect
        );

        // Drop at the right edge of the visible scrolled line: inserts near the scrolled tail.
        let drop_at = pos2(line.right() - 2.0, line.center().y);
        begin(
            &ctx,
            true,
            None,
            Some(Dropped {
                at: drop_at,
                html: None,
                text: "!".into(),
                source: None,
                copy: false,
            }),
            None,
        );
        let mut inserted = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            inserted = field(ui, id, &mut text, line, &style, caret_color);
        })
        .drop_without_applying_deltas();
        let range = inserted.expect("drop accepted");
        assert!(
            range.start > len / 2,
            "scrolled drop inserted near tail ({range:?} in len {len}), got {text:?}"
        );
    }
}
