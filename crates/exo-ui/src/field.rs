//! A bare one-line text field, an egui `TextEdit` whose text sits where CSS puts an input's (the
//! palette's and the shortcuts sheet's search fields, dialog inputs). Find in page searches what's
//! typed in it, as it does the rest of the page.
//!
//! A right-click in one opens the shell's menu for editable text. As on a Mac, the click selects
//! the word under it (or keeps the selection it's on), and the field reports it
//! ([`take_field_menu`]) for the app to show the menu over.

use std::{collections::HashMap, ops::Range, sync::Arc};

use egui::{
    Color32, Context, Direction, Frame, Id, Layout, Margin, Pos2, Rect, Shape, Stroke, TextBuffer,
    TextEdit, Ui, UiBuilder, pos2,
    text::{CCursor, CCursorRange},
    text_edit::TextEditState,
    vec2,
};

use crate::{Font, TextStyle};

type Baselines = Arc<std::sync::Mutex<HashMap<(Font, u32, u32, u32), f32>>>;

fn baselines_id() -> Id {
    Id::new("exo_ui::field_baselines")
}

fn scroll_id(id: Id) -> Id {
    id.with("exo_ui::field_scroll")
}

fn cached_egui_baseline(ctx: &Context, style: &TextStyle, pixels_per_point: f32) -> f32 {
    let key = (
        style.font,
        style.size.to_bits(),
        style.line_height.to_bits(),
        pixels_per_point.to_bits(),
    );
    let cache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Baselines>(baselines_id())
            .clone()
    });
    if let Some(&baseline) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return baseline;
    }
    let probe = crate::text::layout(ctx, style.line_job("x", Color32::WHITE, None));
    let baseline = probe
        .rows
        .first()
        .and_then(|row| row.row.glyphs.first())
        .map_or(0.0, |glyph| glyph.pos.y);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, baseline);
    baseline
}

/// Edits `text` in a frameless field whose CSS line box (in `style`) is `line`, in `color` with
/// `selection` behind selected text. Like every input, it draws without ligatures
/// (`font-variant-ligatures: none`).
pub fn text_field(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    line: Rect,
    style: &TextStyle,
    color: Color32,
    selection: Color32,
) -> egui::Response {
    let style = &style.without_ligatures();
    let pixels_per_point = ui.ctx().pixels_per_point();
    // TextEdit puts a row's baseline where egui does, so shift it to where CSS puts it.
    let egui_baseline = cached_egui_baseline(ui.ctx(), style, pixels_per_point);
    // TextEdit paints its text from a whole pixel. Start the field there and the text at the
    // line's sub-pixel phase, as `paint_job` does, so the text lands where Chromium puts it.
    let phase = (line.left() * pixels_per_point).fract() / pixels_per_point;
    let rect = line.translate(vec2(-phase, style.baseline(ui.ctx()) - egui_baseline));
    let mut layouter = |ui: &Ui, text: &dyn TextBuffer, _wrap: f32| {
        let mut job = style.line_job(text.as_str(), color, None);
        if let Some(first) = job.sections.first_mut() {
            first.leading_space += phase;
        }
        let mut galley = crate::text::layout(ui.ctx(), job);
        crate::find::darken_matches(ui.ctx(), &mut galley);
        galley
    };
    let clipped = ui.painter().with_clip_rect(line.intersect(ui.clip_rect()));
    // Find's highlights go under the selection, in a slot filled once the edit has laid out.
    let found = clipped.add(Shape::Noop);
    // The selection is painted here, over the CSS line box (egui's would follow its own row, off
    // by the baseline shift), behind the text, clipped to `line`.
    let behind = clipped.add(Shape::Noop);
    // A right-click's press keeps the selection egui's caret move would lose.
    let ctx = ui.ctx().clone();
    let pressed_id = id.with("exo_ui::field_right_press");
    if ui.input(|input| input.pointer.secondary_pressed()) && ui.rect_contains_pointer(line) {
        let len = text.chars().count();
        let kept = TextEditState::load(&ctx, id)
            .and_then(|state| state.cursor.char_range())
            .filter(|_| ctx.memory(|memory| memory.has_focus(id)))
            .map(|range| clamp_range(range, len));
        ctx.data_mut(|data| data.insert_temp(pressed_id, kept));
    }
    // Chromium draws no caret while text is selected.
    let selecting = TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range())
        .is_some_and(|range| range.primary != range.secondary);
    let output = ui
        .scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::centered_and_justified(Direction::TopDown)),
            |ui| {
                let visuals = ui.visuals_mut();
                visuals.selection.bg_fill = Color32::TRANSPARENT;
                visuals.selection.stroke = Stroke::new(0.0, color);
                visuals.text_cursor.stroke = Stroke::new(
                    1.0,
                    if selecting {
                        Color32::TRANSPARENT
                    } else {
                        color
                    },
                );
                let edit = TextEdit::singleline(text)
                    .id(id)
                    .frame(Frame::NONE)
                    .margin(Margin::ZERO)
                    .desired_width(line.width())
                    .layouter(&mut layouter);
                edit.show(ui)
            },
        )
        .inner;
    let response = output.response.response;
    let scroll_offset = (rect.left() - output.galley_pos.x).max(0.0);
    ctx.data_mut(|data| data.insert_temp(scroll_id(id), scroll_offset));

    let highlights = crate::find::field(&clipped, output.galley_pos, &output.galley, style);
    clipped.set(found, Shape::Vec(highlights));
    crate::emoji::paint(&clipped, output.galley_pos, &output.galley);

    let pressed = ctx.data(|data| data.get_temp::<Option<CCursorRange>>(pressed_id));
    if let Some(pressed) = pressed {
        let (down, released) = ui.input(|input| {
            (
                input.pointer.secondary_down(),
                input.pointer.secondary_released(),
            )
        });
        if let Some(kept) = pressed.filter(|_| down || released) {
            set_selection(&ctx, id, clamp_range(kept, text.chars().count()));
        }
        if !down {
            ctx.data_mut(|data| data.remove::<Option<CCursorRange>>(pressed_id));
        }
        if response.secondary_clicked()
            && let Some(point) = response.interact_pointer_pos()
        {
            right_click(&ctx, id, text, line, style, point, pressed);
        }
    }
    let range = TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range())
        .filter(|range| range.primary != range.secondary && response.has_focus());
    if let Some(range) = range {
        let x = |index: usize| {
            if index == 0 {
                line.left() - scroll_offset
            } else {
                output.galley_pos.x + output.galley.pos_from_cursor(CCursor::new(index)).min.x
            }
        };
        let (from, to) = (
            range.primary.index.0.min(range.secondary.index.0),
            range.primary.index.0.max(range.secondary.index.0),
        );
        clipped.set(
            behind,
            Shape::rect_filled(
                Rect::from_x_y_ranges(x(from)..=x(to), line.y_range()),
                0.0,
                selection,
            ),
        );
    }
    response
}

/// The current horizontal scroll offset (in points) of single-line [`text_field`] `id`.
pub fn field_scroll_offset(ctx: &Context, id: Id) -> f32 {
    ctx.data(|data| data.get_temp::<f32>(scroll_id(id)))
        .unwrap_or(0.0)
}

/// Where character `index` of `text` starts on screen in a field [`text_field`] lays out on
/// `line` in `style` (unscrolled).
pub fn field_x(ctx: &Context, text: &str, line: Rect, style: &TextStyle, index: usize) -> f32 {
    line.left()
        + field_galley(ctx, text, style)
            .pos_from_cursor(egui::text::CCursor::new(index))
            .min
            .x
}

/// [`field_x`] shifted by the horizontal scroll offset of single-line [`text_field`] `id`.
pub fn field_x_scrolled(
    ctx: &Context,
    id: Id,
    text: &str,
    line: Rect,
    style: &TextStyle,
    index: usize,
) -> f32 {
    field_x(ctx, text, line, style, index) - field_scroll_offset(ctx, id)
}

/// The caret position nearest `x` on screen in a field [`text_field`] lays `text` out in
/// (unscrolled).
pub fn field_index(ctx: &Context, text: &str, line: Rect, style: &TextStyle, x: f32) -> usize {
    field_galley(ctx, text, style)
        .cursor_from_pos(vec2(x - line.left(), 0.0))
        .index
        .0
}

/// [`field_index`] accounting for the horizontal scroll offset of single-line [`text_field`] `id`.
pub fn field_index_scrolled(
    ctx: &Context,
    id: Id,
    text: &str,
    line: Rect,
    style: &TextStyle,
    x: f32,
) -> usize {
    field_index(ctx, text, line, style, x + field_scroll_offset(ctx, id))
}

fn field_galley(ctx: &Context, text: &str, style: &TextStyle) -> std::sync::Arc<egui::Galley> {
    crate::text::layout(
        ctx,
        style
            .without_ligatures()
            .line_job(text, Color32::PLACEHOLDER, None),
    )
}

/// A right-click in a field, for the shell's editable menu: where, and what it selected.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldMenu {
    /// The field.
    pub field: Id,
    /// Where, on screen.
    pub at: Pos2,
    /// The selection the click made or kept, as character offsets.
    pub selection: Range<usize>,
    /// The selected text.
    pub selected: String,
    /// Where its text starts on screen, on its baseline (Look Up's popover lines up with it).
    pub baseline: Pos2,
    /// The field's text size, for Look Up.
    pub text_size: f32,
}

/// The right-click a field reported since this was last asked (only `field`'s, when one is given).
pub fn take_field_menu(ctx: &Context, field: Option<Id>) -> Option<FieldMenu> {
    let key = crate::id(ctx, "exo_ui::field_menu");
    ctx.data_mut(|data| {
        let menu = data
            .get_temp::<FieldMenu>(key)
            .filter(|menu| field.is_none_or(|field| field == menu.field))?;
        data.remove::<FieldMenu>(key);
        Some(menu)
    })
}

/// Whether `ch` is part of a word.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '\'' | '’')
}

/// A right-click at `point` in field `id` (`SelectClosestWordOrLinkFromMouseEvent`). On the
/// selection its press found (`pressed`), it keeps that selection. Otherwise it selects the word
/// under it, or puts the caret there off a word. Then the field has the keyboard and the app shows
/// the menu.
fn right_click(
    ctx: &Context,
    id: Id,
    text: &str,
    line: Rect,
    style: &TextStyle,
    point: Pos2,
    pressed: Option<CCursorRange>,
) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let at = field_index_scrolled(ctx, id, text, line, style, point.x).min(len);
    // The character under the pointer is the one before the caret position when the pointer is
    // left of that position.
    let under = if at > 0 && point.x < field_x_scrolled(ctx, id, text, line, style, at) {
        at - 1
    } else {
        at
    };
    let selected = pressed.map(|range| {
        let a = range.primary.index.0.min(len);
        let b = range.secondary.index.0.min(len);
        a.min(b)..a.max(b)
    });
    let range = match selected {
        Some(range) if range.start <= under && under < range.end => range,
        _ if chars.get(under).copied().is_some_and(is_word_char) => {
            let start = (0..under)
                .rev()
                .take_while(|at| is_word_char(chars[*at]))
                .last()
                .unwrap_or(under);
            let end = (under..len)
                .take_while(|at| is_word_char(chars[*at]))
                .last()
                .map_or(under, |last| last + 1);
            start..end
        }
        _ => at..at,
    };
    set_selection(
        ctx,
        id,
        CCursorRange::two(CCursor::new(range.start), CCursor::new(range.end)),
    );
    ctx.memory_mut(|memory| memory.request_focus(id));
    crate::claim_right_click(ctx);
    let menu = FieldMenu {
        field: id,
        at: point,
        selected: chars[range.clone()].iter().collect(),
        baseline: pos2(
            field_x_scrolled(ctx, id, text, line, style, range.start)
                .clamp(line.left(), line.right()),
            line.top() + style.baseline(ctx),
        ),
        selection: range,
        text_size: style.size,
    };
    let key = crate::id(ctx, "exo_ui::field_menu");
    ctx.data_mut(|data| data.insert_temp(key, menu));
    ctx.request_repaint();
}

fn clamp_range(mut range: CCursorRange, max: usize) -> CCursorRange {
    range.primary.index.0 = range.primary.index.0.min(max);
    range.secondary.index.0 = range.secondary.index.0.min(max);
    range
}

fn set_selection(ctx: &Context, id: Id, range: CCursorRange) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(range));
    state.store(ctx, id);
}

/// Focuses the field `id` with `selection` of its text selected.
pub fn focus_field(ctx: &Context, id: Id, selection: CCursorRange) {
    ctx.memory_mut(|memory| memory.request_focus(id));
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(selection));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use egui::{Shape, epaint::TextShape, pos2};

    use super::*;
    use crate::{Family, Font};

    /// The pixel the tessellator draws a text shape's first glyph from. It rounds the shape's
    /// position to a whole pixel (`round_text_to_pixels`), and where the glyph sits inside that
    /// pixel is in its raster, the same for the same layout.
    fn first_glyph_x(text: &TextShape, pixels_per_point: f32) -> f32 {
        let row = &text.galley.rows[0];
        (text.pos.x * pixels_per_point).round() / pixels_per_point + row.pos.x + row.glyphs[0].pos.x
    }

    #[test]
    fn a_fields_text_lands_where_a_painted_lines_does() {
        let ctx = Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let style = TextStyle::new(Font::new(Family::Sans, 400), 16.0, 24.0).without_ligatures();
        ctx.set_pixels_per_point(2.0);
        // 3.5rem at 1280px wide, which is a quarter pixel past a whole one at 2×.
        let line = |top: f32| Rect::from_min_size(pos2(47.25, top), vec2(300.0, 24.0));
        let mut text = "has:attachment".to_owned();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            text_field(
                ui,
                Id::new("field"),
                &mut text,
                line(10.0),
                &style,
                Color32::BLACK,
                Color32::BLUE,
            );
            style.paint_line(
                ui.painter(),
                line(50.0).min,
                "has:attachment",
                Color32::BLACK,
                None,
            );
        });
        let shapes = std::mem::take(&mut output.shapes);
        output.drop_without_applying_deltas();
        let texts: Vec<&TextShape> = shapes
            .iter()
            .filter_map(|clipped| {
                if let Shape::Text(text) = &clipped.shape {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();
        let [field, painted] = texts[..] else {
            panic!("two lines of text, got {}", texts.len())
        };
        assert_eq!(first_glyph_x(field, 2.0), first_glyph_x(painted, 2.0));
    }

    #[test]
    fn overflowing_singleline_field_offsets_and_clips_selection_and_find_highlights() {
        let ctx = Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let style = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 22.0);
        let id = Id::new("scrolled_field");
        let line = Rect::from_min_size(pos2(40.0, 20.0), vec2(80.0, 22.0));
        let mut text = "prefix words before overflowing tail target_token".to_owned();
        let len = text.chars().count();
        let selection_color = Color32::from_rgb(40, 120, 220);

        // Select the entire overflowing string with primary cursor at the end so `TextEdit`
        // scrolls horizontally to reveal `target_token`.
        focus_field(
            &ctx,
            id,
            CCursorRange::two(CCursor::new(0), CCursor::new(len)),
        );
        crate::find::search(&ctx, Some("target_token"));

        let mut clipped_shapes = Vec::new();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                text_field(
                    ui,
                    id,
                    &mut text,
                    line,
                    &style,
                    Color32::BLACK,
                    selection_color,
                );
            });
            clipped_shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
        }

        let scroll = field_scroll_offset(&ctx, id);
        assert!(scroll > 50.0, "field scrolled horizontally ({scroll}px)");
        assert!(
            (field_x_scrolled(&ctx, id, &text, line, &style, len) - line.right()).abs() < 2.0,
            "scrolled end of text sits near the field's right edge"
        );

        let mut saw_selection = false;
        let mut saw_find = false;
        for item in &clipped_shapes {
            let check = |shape: &Shape, clip: Rect, saw_sel: &mut bool, saw_fnd: &mut bool| {
                if let Shape::Rect(rect) = shape {
                    if rect.fill == selection_color {
                        *saw_sel = true;
                        assert!(
                            clip.left() >= line.left() - 0.01
                                && clip.right() <= line.right() + 0.01,
                            "selection clip rect {clip:?} stays within line {line:?}"
                        );
                        assert!(
                            rect.rect.left() < line.left(),
                            "selection rect {rect:?} is shifted left by -scroll_offset"
                        );
                    }
                    if rect.fill == crate::find::CURRENT || rect.fill == crate::find::MATCH {
                        *saw_fnd = true;
                        assert!(
                            clip.left() >= line.left() - 0.01
                                && clip.right() <= line.right() + 0.01,
                            "find highlight clip rect {clip:?} stays within line {line:?}"
                        );
                        assert!(
                            rect.rect.right() <= line.right() + 1.0
                                && rect.rect.left() >= line.left() - 1.0,
                            "scrolled find highlight {rect:?} sits inside visible field {line:?}"
                        );
                    }
                }
            };
            match &item.shape {
                Shape::Vec(vec) => {
                    for sub in vec {
                        check(sub, item.clip_rect, &mut saw_selection, &mut saw_find);
                    }
                }
                other => check(other, item.clip_rect, &mut saw_selection, &mut saw_find),
            }
        }
        assert!(saw_selection, "selection rect painted");
        assert!(saw_find, "find highlight painted");
        crate::find::search(&ctx, None);
    }

    #[test]
    fn right_click_clamps_stale_selection_when_text_shortens() {
        let ctx = Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let style = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 22.0);
        let id = Id::new("shortened_field");
        let line = Rect::from_min_size(pos2(20.0, 20.0), vec2(240.0, 22.0));
        let mut text = "a very long initial string with selection".to_owned();
        let len = text.chars().count();
        let at = pos2(line.left() + 4.0, line.center().y);

        focus_field(
            &ctx,
            id,
            CCursorRange::two(CCursor::new(0), CCursor::new(len)),
        );
        let run = |text: &mut String, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let _ = text_field(
                        ui,
                        id,
                        text,
                        line,
                        &style,
                        Color32::BLACK,
                        Color32::LIGHT_BLUE,
                    );
                },
            )
            .drop_without_applying_deltas();
        };

        // Prime layout, then secondary-press while full long text is selected.
        run(&mut text, vec![egui::Event::PointerMoved(at)]);
        run(
            &mut text,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        // Shorten text before releasing the right button (kept range exceeds new len).
        text = "hi".to_owned();
        run(
            &mut text,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let menu = take_field_menu(&ctx, Some(id)).expect("field menu opened without panic");
        assert!(menu.selection.end <= text.chars().count());
        assert_eq!(menu.selected, "hi");

        // Also test stale TextEditState selection that already exceeds shortened text on press.
        focus_field(
            &ctx,
            id,
            CCursorRange::two(CCursor::new(5), CCursor::new(30)),
        );
        text = "ok".to_owned();
        run(
            &mut text,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        run(
            &mut text,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let menu = take_field_menu(&ctx, Some(id)).expect("second field menu opened cleanly");
        assert!(menu.selection.end <= text.chars().count());
    }
}
