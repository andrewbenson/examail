//! Painting the laid-out document: selection highlights, quote bars, rules, list markers, text
//! decorations, misspellings, inline images and kept tables, the placeholder, and the blinking
//! caret, together with the screen-space boxes of characters, lines, images and selections.

use egui::{
    Color32, CornerRadius, Pos2, Rect, Stroke, Ui, Vec2, pos2,
    text::{CCursor, CharIndex, LayoutJob},
    vec2,
};

use super::{
    BodyText, Colors, EditorStyle, RULE_HEIGHT, ViewState, colors_for, dates,
    layout::{BlockLayout, Decoration, DecorationKind, Marker, align_rows},
    pill, resizer,
};
use crate::{
    editor::Editor,
    image::{Look, State},
    model::{Content, Piece, Pos},
};

/// Chromium's caret: 1px wide.
const CARET_WIDTH: f32 = 1.0;

impl Editor {
    pub(super) fn paint(
        &self,
        ui: &Ui,
        rect: Rect,
        colors: &Colors,
        style: &EditorStyle,
        focused: bool,
        now: f64,
    ) {
        let Some(layout) = &self.view.layout else {
            return;
        };
        let body = &self.view.body;
        let painter = ui.painter_at(rect.expand(2.0));
        for bar in &layout.bars {
            painter.rect_filled(
                exo_ui::pixel_snap(bar.translate(rect.min.to_vec2())),
                CornerRadius::ZERO,
                colors.bar,
            );
        }
        // The selection goes under the text, and a focused image shows the resizer instead.
        let focused_image = self.focused_image();
        if !self.selection.is_collapsed() && focused_image.is_none() {
            for (highlight, _) in self.selection_rects(rect.min) {
                painter.rect_filled(highlight, CornerRadius::ZERO, colors.selection);
            }
        }
        if let Some(image) = focused_image.and_then(|at| self.image_rect(rect.min, at)) {
            resizer::paint_shadow(ui, image);
        }
        for (index, (block, view)) in self.doc.blocks.iter().zip(&layout.blocks).enumerate() {
            let top_left = rect.min + vec2(view.left, view.top);
            if view.rule {
                let line = Rect::from_min_size(
                    pos2(rect.min.x + view.left, top_left.y + 10.0),
                    vec2(rect.width() - view.left, 1.0),
                );
                painter.rect_filled(exo_ui::pixel_snap(line), CornerRadius::ZERO, colors.rule);
                continue;
            }
            if let Some(marker) = &view.marker {
                paint_marker(
                    &painter,
                    marker,
                    top_left,
                    colors_for(block, colors),
                    &view.text,
                );
            }
            let x = (top_left.x * painter.pixels_per_point()).floor() / painter.pixels_per_point();
            // Autocorrect's dots, under the text (`z-index: -1`).
            for (range, corrected) in self
                .dotted_words(index)
                .filter(|(range, _)| !range.is_empty())
            {
                let chars = view.galley_index(range.start)..view.galley_index(range.end - 1) + 1;
                pill::paint_annotation(
                    &painter,
                    view,
                    chars,
                    pos2(x, top_left.y),
                    &view.text,
                    corrected,
                );
            }
            if view.rows.is_empty() {
                exo_ui::paint_text(
                    &painter,
                    pos2(x, top_left.y + view.lines[0].shift),
                    view.galley.clone(),
                    &view.text.style,
                );
            }
            for (row, line) in view.rows.iter().zip(&view.lines) {
                exo_ui::paint_text(
                    &painter,
                    pos2(x, top_left.y + line.top + line.shift),
                    row.clone(),
                    &view.text.style,
                );
            }
            for decoration in &view.decorations {
                paint_decoration(&painter, view, decoration, pos2(x, top_left.y), &view.text);
            }
            for range in self.misspellings(index).filter(|range| !range.is_empty()) {
                let chars = view.galley_index(range.start)..view.galley_index(range.end - 1) + 1;
                paint_misspelling(&painter, view, chars, pos2(x, top_left.y), &view.text);
            }
            self.paint_images(
                &painter,
                view,
                index,
                top_left,
                colors,
                focused_image.is_none(),
            );
            for range in view.dates.iter().filter(|range| !range.is_empty()) {
                let chars = view.galley_index(range.start)..view.galley_index(range.end - 1) + 1;
                dates::paint_line(&painter, view, chars, pos2(x, top_left.y), &view.text);
            }
        }
        if self.doc.is_empty()
            && let Some(placeholder) = &style.placeholder
            && self.view.preedit.is_none()
        {
            // `::before { opacity: .45 }` over the text colour; laid out like the body so a
            // fallback glyph (`⌘`) sits on the same baseline.
            let color = colors.text.gamma_multiply(body.placeholder_opacity);
            let first = &layout.blocks[0];
            let mut job = LayoutJob::single_section(placeholder.clone(), body.style.format(color));
            job.sections[0].leading_space = first.phase;
            let mut galley = exo_ui::layout(ui.ctx(), job);
            align_rows(&mut galley, layout.baseline, 0.0);
            let left = rect.min.x + first.left;
            let x = (left * painter.pixels_per_point()).floor() / painter.pixels_per_point();
            exo_ui::paint_text(
                &painter,
                pos2(x, rect.min.y + first.top),
                galley,
                &body.style,
            );
        }
        if focused && self.selection.is_collapsed() {
            let cursor = &ui.visuals().text_cursor;
            let elapsed = now - self.view.blink_start;
            let period = f64::from(cursor.on_duration + cursor.off_duration);
            let visible = !cursor.blink
                || period <= 0.0
                || (elapsed % period) < f64::from(cursor.on_duration);
            if visible {
                painter.rect_filled(
                    self.caret_rect(rect.min, painter.pixels_per_point()),
                    CornerRadius::ZERO,
                    colors.text,
                );
            }
            if cursor.blink && period > 0.0 {
                let phase = elapsed % period;
                let next = if phase < f64::from(cursor.on_duration) {
                    f64::from(cursor.on_duration) - phase
                } else {
                    period - phase
                };
                ui.ctx().request_repaint_after_secs(next as f32);
            }
        }
    }

    /// The caret's box on screen, for an editor whose top left is `origin`. It's Chromium's caret
    /// rect, the font's content area in the line box (ascent + descent, 17px, 3px down), 1px wide
    /// on a whole device pixel. Beside an image with no text on either side, the caret is the side
    /// of the image's box, as tall as the whole line box.
    pub(super) fn caret_rect(&self, origin: Pos2, pixels_per_point: f32) -> Rect {
        self.caret_rect_at(origin, pixels_per_point, self.selection.focus)
    }

    /// [`Self::caret_rect`] for a caret at `pos`.
    pub(super) fn caret_rect_at(&self, origin: Pos2, pixels_per_point: f32, pos: Pos) -> Rect {
        let (rect, shift) = self.line_at(origin, pos);
        let x = (rect.min.x * pixels_per_point).round() / pixels_per_point;
        let view = self
            .view
            .layout
            .as_ref()
            .and_then(|layout| layout.blocks.get(pos.block));
        let text = view.map_or(self.view.body, |view| view.text);
        if view.is_some_and(|view| view.rule) || self.caret_on_image_side(pos) {
            return Rect::from_min_size(pos2(x, rect.min.y), vec2(CARET_WIDTH, rect.height()));
        }
        Rect::from_min_size(
            pos2(x, rect.min.y + shift + text.content_top),
            vec2(CARET_WIDTH, text.content_height),
        )
    }

    /// Whether the caret at `pos` is the side of an image's box (Chromium's caret beside an atomic
    /// inline), which it is with an image on one side and no text on either.
    fn caret_on_image_side(&self, pos: Pos) -> bool {
        let block = &self.doc.blocks[pos.block];
        // A kept table is a block-level atom, so the caret is the side of its box.
        if matches!(block.content, Content::Raw { .. }) {
            return true;
        }
        let before = pos
            .offset
            .checked_sub(1)
            .and_then(|offset| block.slice(offset..pos.offset).pop())
            .map(|run| run.piece);
        let after = block
            .slice(pos.offset..pos.offset + 1)
            .pop()
            .map(|run| run.piece);
        let text = |piece: &Option<Piece>| matches!(piece, Some(Piece::Text(_)));
        let image = |piece: &Option<Piece>| matches!(piece, Some(Piece::Image(_)));
        !text(&before) && !text(&after) && (image(&before) || image(&after))
    }

    /// The box of the line position `pos` is at, zero wide and a line box tall.
    pub(super) fn char_rect(&self, origin: Pos2, pos: Pos) -> Rect {
        self.line_at(origin, pos).0
    }

    /// The line box position `pos` is at (zero wide, at its x) and how far down its text is
    /// moved for its images.
    pub(super) fn line_at(&self, origin: Pos2, pos: Pos) -> (Rect, f32) {
        let Some(layout) = &self.view.layout else {
            return (Rect::NOTHING, 0.0);
        };
        let Some(view) = layout.blocks.get(pos.block) else {
            return (Rect::NOTHING, 0.0);
        };
        // The caret goes before ghost text, after a composition.
        let ghost = self.ghost.is_some()
            && self.view.preedit.is_none()
            && view.insert.is_some_and(|(at, _)| at == pos.offset);
        let index = if ghost {
            pos.offset
        } else {
            view.galley_index(pos.offset)
        };
        let cursor = view.galley.pos_from_cursor(CCursor::new(index));
        let x = origin.x + view.left + cursor.min.x - view.phase;
        if view.rule {
            return (
                Rect::from_min_size(pos2(x, origin.y + view.top), vec2(0.0, RULE_HEIGHT)),
                0.0,
            );
        }
        let line = view.lines[view.row_of(cursor.center().y, view.text.line())];
        (
            Rect::from_min_size(
                pos2(x, origin.y + view.top + line.top),
                vec2(0.0, line.height),
            ),
            line.shift,
        )
    }

    /// The box of the image at `at` on screen, for an editor at `origin`.
    pub(crate) fn image_rect(&self, origin: Pos2, at: Pos) -> Option<Rect> {
        let view = self.view.layout.as_ref()?.blocks.get(at.block)?;
        let placed = view
            .images
            .iter()
            .find(|placed| placed.offset == at.offset)?;
        Some(
            placed
                .rect
                .translate(origin.to_vec2() + vec2(view.left, view.top)),
        )
    }

    /// The keys of the images whose boxes are in `clip`, for an editor at `origin`.
    pub(super) fn images_on_screen(
        &self,
        origin: Pos2,
        clip: Rect,
    ) -> std::collections::HashSet<String> {
        let Some(layout) = &self.view.layout else {
            return Default::default();
        };
        layout
            .blocks
            .iter()
            .flat_map(|view| {
                view.images.iter().filter(move |placed| {
                    clip.intersects(
                        placed
                            .rect
                            .translate((origin + vec2(view.left, view.top)).to_vec2()),
                    )
                })
            })
            .filter_map(|placed| placed.key.clone())
            .collect()
    }

    /// Paints block `index`'s images (laid out in `view`, its text origin at `top_left`), and the
    /// selection's tint over those selected, if `tint`.
    fn paint_images(
        &self,
        painter: &egui::Painter,
        view: &BlockLayout,
        index: usize,
        top_left: Pos2,
        colors: &Colors,
        tint: bool,
    ) {
        let (start, end) = (self.selection.start(), self.selection.end());
        for placed in &view.images {
            let rect = exo_ui::pixel_snap(placed.rect.translate(top_left.to_vec2()));
            match placed.look {
                Look::Picture => {
                    let texture =
                        placed
                            .key
                            .as_deref()
                            .and_then(|key| match self.view.pictures.get(key) {
                                Some(State::Loaded {
                                    texture: Some(texture),
                                    ..
                                }) => Some(texture.id()),
                                _ => None,
                            });
                    if let Some(texture) = texture {
                        painter.image(
                            texture,
                            rect,
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                // Its texture holds the sub-pixel phase, on the whole pixel left of the text's
                // origin ([`Self::paint_tables`]).
                Look::Table(key) => {
                    let ppp = painter.pixels_per_point();
                    for (top, texture) in self.view.tables.bands(key) {
                        let at = pos2(
                            (top_left.x * ppp).floor() / ppp,
                            ((top_left.y + placed.rect.min.y + top) * ppp).round() / ppp,
                        );
                        let [width, height] = texture.size();
                        painter.image(
                            texture.id(),
                            Rect::from_min_size(at, vec2(width as f32 / ppp, height as f32 / ppp)),
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                // `img[src^="https://media-blocked"]::after`: white, a 1px `rgba(0, 0, 0, .2)`
                // border inside it.
                Look::Blocked => {
                    painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
                    painter.rect_stroke(
                        rect,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 51)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            // Chromium tints the column a selected image takes in its line box, over the image.
            let at = Pos::new(index, placed.offset);
            if tint && at >= start && at < end {
                let column = exo_ui::pixel_snap(placed.column.translate(top_left.to_vec2()));
                painter.rect_filled(column, CornerRadius::ZERO, colors.image_selection);
            }
        }
    }

    /// Paints the parts of the layout's kept tables inside `clip`, for an editor at `origin`,
    /// where their pictures are out of date.
    pub(super) fn paint_tables(&mut self, ctx: &egui::Context, origin: Pos2, clip: Rect) {
        let ViewState {
            layout: Some(layout),
            tables,
            ..
        } = &mut self.view
        else {
            return;
        };
        let ppp = ctx.pixels_per_point();
        for view in &layout.blocks {
            for placed in &view.images {
                let Look::Table(key) = placed.look else {
                    continue;
                };
                let left = origin.x + view.left;
                let top = origin.y + view.top + placed.rect.min.y;
                tables.paint(
                    key,
                    placed.rect.width(),
                    left - (left * ppp).floor() / ppp,
                    ppp,
                    (clip.top() - top)..(clip.bottom() - top),
                    ctx,
                );
            }
        }
    }

    /// The selection's highlight boxes. Each line gets the selected text's span in a full line
    /// box, plus a space's width past its end when the selection continues past it. Each box comes
    /// with how far its line's text is moved down for its images.
    pub(super) fn selection_rects(&self, origin: Pos2) -> Vec<(Rect, f32)> {
        self.range_rects(origin, self.selection.start(), self.selection.end())
    }

    /// The text's boxes from `start` to `end`, which are the font's content area in each line box
    /// (the body's 17px, 3px down).
    pub(super) fn text_boxes(&self, origin: Pos2, start: Pos, end: Pos) -> Vec<Rect> {
        let body = self.view.body;
        self.range_rects(origin, start, end)
            .into_iter()
            .map(|(line, shift)| {
                Rect::from_min_size(
                    line.min + vec2(0.0, shift + body.content_top),
                    vec2(line.width(), body.content_height),
                )
            })
            .collect()
    }

    /// [`Self::selection_rects`] for the range from `start` to `end`.
    fn range_rects(&self, origin: Pos2, start: Pos, end: Pos) -> Vec<(Rect, f32)> {
        let Some(layout) = &self.view.layout else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for index in start.block..=end.block {
            let view = &layout.blocks[index];
            let block = &self.doc.blocks[index];
            let from = if index == start.block {
                start.offset
            } else {
                0
            };
            let to = if index == end.block {
                end.offset
            } else {
                block.len()
            };
            let left = origin.x + view.left - view.phase;
            let top = origin.y + view.top;
            if view.rule {
                if from < to {
                    out.push((
                        Rect::from_min_size(
                            pos2(origin.x + view.left, top),
                            vec2(4.0, RULE_HEIGHT),
                        ),
                        0.0,
                    ));
                }
                continue;
            }
            let mut offset = 0;
            for (row_index, row) in view.galley.rows.iter().enumerate() {
                let count = row.char_count_including_newline().0;
                let (row_start, row_end) = (offset, offset + row.char_count_excluding_newline().0);
                offset += count;
                let (a, b) = (
                    view.document_offset(row_start),
                    view.document_offset(row_end),
                );
                let (s, e) = (from.max(a), to.min(b));
                // Past a hard line end, the selection paints the line break as a space. Past a
                // soft wrap, it paints nothing more.
                let last_row = row_index + 1 == view.galley.rows.len();
                let continues =
                    (to > b || index < end.block) && (last_row || row.ends_with_newline);
                if s > e || (s == e && !continues) {
                    continue;
                }
                let x0 = row.x_offset(CharIndex(view.galley_index(s) - row_start));
                let mut x1 = row.x_offset(CharIndex(view.galley_index(e) - row_start));
                if continues {
                    // Chromium highlights the line break as a space.
                    x1 += layout.space;
                }
                let line = view.lines[row_index];
                let y = top + line.top;
                // Chromium paints the whole line box inside the block, with its edges on whole CSS
                // pixels. An image's column gets the image's tint instead
                // ([`Self::paint_images`]).
                let right = (left + x1).min(origin.x + layout.width);
                let mut from_x = left + x0;
                for placed in view
                    .images
                    .iter()
                    .filter(|placed| (s..e).contains(&placed.offset))
                {
                    let column = placed.column.translate(vec2(origin.x + view.left, 0.0));
                    if column.min.x > from_x {
                        out.push((
                            exo_ui::pixel_snap(Rect::from_min_max(
                                pos2(from_x, y),
                                pos2(column.min.x, y + line.height),
                            )),
                            line.shift,
                        ));
                    }
                    from_x = from_x.max(column.max.x);
                }
                if right > from_x {
                    out.push((
                        exo_ui::pixel_snap(Rect::from_min_max(
                            pos2(from_x, y),
                            pos2(right, y + line.height),
                        )),
                        line.shift,
                    ));
                }
            }
        }
        out
    }
}

/// Paints a list marker in the first line box at `line_top_left` (the item's text start), using
/// Chromium's shapes for disc, circle and square, and text for numbers.
fn paint_marker(
    painter: &egui::Painter,
    marker: &Marker,
    line_top_left: Pos2,
    color: Color32,
    body: &BodyText,
) {
    let baseline = line_top_left.y + body.style.baseline(painter.ctx());
    match marker {
        Marker::Text(text) => {
            // `1. `: the marker's text and its trailing space end where the item's text starts.
            let galley = body
                .style
                .single_line(painter, &format!("{text} "), color, None);
            let width = galley.size().x;
            exo_ui::paint_text(
                painter,
                pos2(line_top_left.x - width, line_top_left.y),
                galley,
                &body.style,
            );
        }
        shape => {
            // As in `ListMarker::RelativeSymbolMarkerRect`, a (ascent·2/3 + 1)/2 = 4px bullet
            // whose box ends 11.375px before the text and sits 2px above the baseline at every
            // nesting level, on whole device pixels.
            let pixels = painter.pixels_per_point();
            let left = ((line_top_left.x - 15.375) * pixels).round() / pixels;
            let rect = Rect::from_min_size(pos2(left, baseline - 6.0), Vec2::splat(4.0));
            match shape {
                Marker::Disc => painter.circle_filled(rect.center(), 2.0, color),
                Marker::Circle => {
                    painter.circle_stroke(rect.center(), 2.0, Stroke::new(1.0, color))
                }
                _ => painter.rect_filled(rect, CornerRadius::ZERO, color),
            };
        }
    }
}

/// Paints an underline (1px, 1px under the baseline) or a strike-through (1px, 5px above it)
/// across `decoration`'s characters, one segment per row, as Chromium draws them for 14px
/// text (the post table's underline position −75, OS/2 strikeout 250, both 50 thick).
fn paint_decoration(
    painter: &egui::Painter,
    view: &BlockLayout,
    decoration: &Decoration,
    galley_origin: Pos2,
    body: &BodyText,
) {
    let baseline = body.style.baseline(painter.ctx());
    let offset = match decoration.kind {
        DecorationKind::Underline => 1.0,
        DecorationKind::LineThrough => -5.0,
    };
    for (line, x0, x1) in view.row_spans(decoration.chars.clone()) {
        let y = galley_origin.y + line.top + line.shift + baseline + offset;
        let rect = Rect::from_min_max(
            pos2(galley_origin.x + x0, y),
            pos2(galley_origin.x + x1, y + 1.0),
        );
        painter.rect_filled(rect, CornerRadius::ZERO, decoration.color);
    }
}

/// Chromium's misspelling marker on a Mac ([`exo_ui::paint_misspelling`]) under characters
/// `chars` of `view`, on each row they're on.
fn paint_misspelling(
    painter: &egui::Painter,
    view: &BlockLayout,
    chars: std::ops::Range<usize>,
    galley_origin: Pos2,
    body: &BodyText,
) {
    let top = exo_ui::misspelling_top(&body.style, painter.ctx());
    let ppp = painter.pixels_per_point();
    for (line, x0, x1) in view.row_spans(chars) {
        let (x0, x1) = (galley_origin.x + x0, galley_origin.x + x1);
        exo_ui::paint_misspelling(
            painter,
            x0..x1,
            galley_origin.y + line.top + line.shift + top,
            (x0 * ppp).floor() / ppp,
        );
    }
}
