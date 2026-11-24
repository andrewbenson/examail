//! Document layout in CSS-style block and line boxes, with margin collapsing, list markers, quote
//! bars, inline images and Blitz-rendered kept tables.

use std::sync::Arc;

use egui::{
    Color32, Galley, Rect, Stroke, TextFormat, Ui, pos2,
    text::{CharIndex, LayoutJob, TextWrapping},
    vec2,
};
use exo_ui::{Font, TextStyle, Theme};

use super::{BodyText, Colors, RULE_HEIGHT, colors_for, table};
use crate::{
    editor::{Color, Editor},
    html,
    image::{ImageBox, ImageSource, Look, Pictures},
    model::{Block, ContainerKind, Content, Document, Piece, Pos},
};

/// `ol, ul { margin: 16px 0 }`, which nested lists don't have.
const LIST_MARGIN: f32 = 16.0;
/// The UA's `padding-inline-start` for lists.
const LIST_INDENT: f32 = 40.0;
/// `blockquote { padding: 0 0 0 1.25em !important }` at 14px.
const QUOTE_INDENT: f32 = 17.5;
/// `margin:0 0 0 40px` of an indent.
const INDENT: f32 = 40.0;
/// Links in user content (`a[href] { color: #54acdc !important }`).
const LINK: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);
/// Exo Sans Email's x-height (OS/2 `sxHeight` 1028 of 2000), which a blocked image's
/// `vertical-align: middle` centres on.
const X_HEIGHT: f32 = 0.514;
/// `pre { margin: 1em 0 }` (the UA's) at the code's 14px.
const CODE_MARGIN: f32 = 14.0;

/// The document laid out at one width.
pub(crate) struct Layout {
    key: LayoutKey,
    pub(crate) blocks: Vec<BlockLayout>,
    pub(crate) height: f32,
    pub(super) bars: Vec<Rect>,
    /// A space's width, which a selection paints past the end of each line it continues from.
    pub(super) space: f32,
    /// The width it was laid out at.
    pub(super) width: f32,
    /// Where egui puts the body face's baseline in a row.
    pub(super) baseline: f32,
}

/// What a layout was made from.
#[derive(Clone, Debug, Default, PartialEq)]
struct LayoutKey {
    revision: u64,
    /// The width at its sub-pixel phase, and the scale.
    size: (u32, u32),
    theme: Theme,
    ghost: Option<String>,
    preedit: Option<String>,
    /// Where the caret is, which matters only while text is spliced in at it.
    caret: Pos,
    pictures: u64,
    tables: u64,
}

/// One block's place and text.
pub(crate) struct BlockLayout {
    /// The top of its first line box, from the editor's top.
    pub(crate) top: f32,
    /// Where its text starts, from the editor's left.
    pub(crate) left: f32,
    pub(crate) height: f32,
    /// The laid-out text (with ghost or composition text spliced in at `insert`).
    pub(crate) galley: Arc<Galley>,
    /// The sub-pixel phase the text was laid out at (`exo_ui::paint_job`).
    pub(super) phase: f32,
    /// `(offset, length)` of text spliced in that isn't in the document.
    pub(super) insert: Option<(usize, usize)>,
    pub(super) marker: Option<Marker>,
    pub(super) rule: bool,
    /// Underlines and strike-throughs: galley character ranges and their colour.
    pub(super) decorations: Vec<Decoration>,
    /// For wrapped text, each row laid out on its own at the phase, to paint (the wrapped
    /// galley's later rows were rasterized without it).
    pub(super) rows: Vec<Arc<Galley>>,
    /// Each galley row's line box.
    pub(super) lines: Vec<LineBox>,
    /// Its images.
    pub(super) images: Vec<Placed>,
    /// What the text above was laid out from (`text_key`), to reuse it next time.
    text_key: u64,
    /// Its text's style: the body's, or a code block's.
    pub(super) text: BodyText,
    /// The dates detected in it ([`crate::dates`]), as character ranges of the block.
    pub(crate) dates: Vec<std::ops::Range<usize>>,
}

/// One line box of a block: where it is below the block's top, how tall, and how far its text
/// sits below where a line without images would put it (an image taller than the text raises
/// the line's baseline).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineBox {
    pub(super) top: f32,
    pub(super) height: f32,
    pub(super) shift: f32,
}

/// An image in a block's layout.
#[derive(Clone, Debug)]
pub(super) struct Placed {
    /// Its position in the block.
    pub(super) offset: usize,
    /// Its content box, from the block's text origin (`left`, `top`).
    pub(super) rect: Rect,
    /// The column it takes in its line box (its padding box across, the line box down), where a
    /// selection paints its tint rather than the text's highlight.
    pub(super) column: Rect,
    /// Its pixels' key ([`ImageSource::key`]).
    pub(super) key: Option<String>,
    pub(super) look: Look,
}

/// A `text-decoration` line over a run of characters (the run of the `u` or `s` element).
#[derive(Clone, Debug)]
pub(super) struct Decoration {
    pub(super) chars: std::ops::Range<usize>,
    pub(super) kind: DecorationKind,
    pub(super) color: Color32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecorationKind {
    Underline,
    LineThrough,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Marker {
    Disc,
    Circle,
    Square,
    Text(String),
}

impl BlockLayout {
    /// The galley row at galley `y` (its rows are `line` apart).
    pub(super) fn row_of(&self, y: f32, line: f32) -> usize {
        ((y / line).floor().max(0.0) as usize).min(self.lines.len() - 1)
    }

    /// The galley's character index for document offset `offset`.
    pub(super) fn galley_index(&self, offset: usize) -> usize {
        match self.insert {
            Some((at, len)) if offset > at => offset + len,
            Some((at, len)) if offset == at => at + len,
            _ => offset,
        }
    }

    /// The document offset for galley index `index`.
    pub(super) fn document_offset(&self, index: usize) -> usize {
        match self.insert {
            Some((at, len)) if index >= at + len => index - len,
            Some((at, _)) if index > at => at,
            _ => index,
        }
    }

    /// Each galley row intersecting `chars` (a galley character range), with its line box and its
    /// horizontal span `(x0, x1)` relative to the galley's origin. Uses row-local column offsets
    /// so a span starting on a soft-wrapped continuation row starts at that row's left edge rather
    /// than the previous row's end.
    pub(super) fn row_spans(
        &self,
        chars: std::ops::Range<usize>,
    ) -> impl Iterator<Item = (LineBox, f32, f32)> + '_ {
        let mut start = 0;
        self.galley
            .rows
            .iter()
            .enumerate()
            .filter_map(move |(row_index, row)| {
                let count = row.char_count_excluding_newline().0;
                let row_start = start;
                start += row.char_count_including_newline().0;
                let (from, to) = (chars.start.max(row_start), chars.end.min(row_start + count));
                if from >= to {
                    return None;
                }
                let line = *self.lines.get(row_index).or_else(|| self.lines.last())?;
                let x0 = row.pos.x + row.x_offset(CharIndex(from - row_start));
                let x1 = row.pos.x + row.x_offset(CharIndex(to - row_start));
                Some((line, x0, x1))
            })
    }
}

impl Editor {
    /// Lays the document out (cached until it, the width, the theme or the caret's splice
    /// changes) and returns it.
    pub(super) fn layout(&mut self, ui: &Ui, width: f32, colors: &Colors, left: f32) -> &Layout {
        self.view.tables.update(&self.view.pictures, ui.ctx());
        let pixels_per_point = ui.pixels_per_point();
        let phase = {
            let x = left * pixels_per_point;
            (x - x.floor()) / pixels_per_point
        };
        let key = LayoutKey {
            revision: self.revision,
            // Fonts are rasterized per scale, so a new one lays everything out again.
            size: (
                (width * 64.0) as u32 ^ ((phase * 64.0) as u32) << 24,
                pixels_per_point.to_bits(),
            ),
            theme: exo_ui::theme(ui.ctx()),
            ghost: self.ghost.clone(),
            preedit: self.view.preedit.clone(),
            caret: self.selection.focus,
            pictures: self.view.pictures.revision,
            tables: self.view.tables.revision,
        };
        let stale = self.view.layout.as_ref().is_none_or(|layout| {
            let spliced = key.ghost.is_some() || key.preedit.is_some();
            layout.key
                != LayoutKey {
                    caret: if spliced { key.caret } else { layout.key.caret },
                    ..key.clone()
                }
        });
        if stale {
            // Blocks laid out at this width, scale and theme keep their text, so an edit lays
            // out only the blocks it changed.
            let previous = self
                .view
                .layout
                .take()
                .filter(|layout| layout.key.size == key.size && layout.key.theme == key.theme);
            let tables = self.view.tables.revision;
            let layout = lay_out(
                ui,
                &self.doc,
                width,
                phase,
                colors,
                &self.view.body,
                self.splice(),
                previous.as_ref().map_or(&[], |layout| &layout.blocks),
                &self.view.pictures,
                &mut self.view.tables,
                !self.settings.skip_dates,
            );
            // A table built has images for the app to load, which it asks for next frame.
            if self.view.tables.revision != tables {
                ui.ctx().request_repaint();
            }
            self.view.layout = Some(Layout {
                key: LayoutKey {
                    tables: self.view.tables.revision,
                    ..key
                },
                ..layout
            });
        }
        self.view.layout.as_ref().expect("laid out above")
    }

    /// Text spliced in at the caret that isn't in the document: the IME composition or the
    /// ghost text.
    fn splice(&self) -> Option<(Pos, String, bool)> {
        let caret = self.selection.focus;
        if let Some(preedit) = self.view.preedit.clone().filter(|text| !text.is_empty()) {
            return Some((caret, preedit, true));
        }
        self.ghost.clone().map(|ghost| (caret, ghost, false))
    }
}

/// The hash of what a block's text layout depends on.
fn text_key(
    block: &Block,
    color: Color32,
    width: f32,
    splice: Option<&(Pos, String, bool)>,
    images: &[ImageBox],
) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        &block.content,
        block.in_code(),
        color,
        width.to_bits(),
        splice.map(|(pos, text, composing)| (pos.offset, text, composing)),
    )
        .hash(&mut hasher);
    for image in images {
        image.hash(&mut hasher);
    }
    hasher.finish()
}

/// The text's vertical metrics an image's line box is built from.
#[derive(Clone, Copy)]
struct LineMetrics {
    /// The baseline below a plain line box's top.
    baseline: f32,
    x_height: f32,
}

/// What a block's text is laid out with: its style and what's measured from its face.
struct Face {
    text: BodyText,
    metrics: LineMetrics,
    /// A space's advance per px of font size, which sizes an image's glyph.
    space_em: f32,
    /// Where egui puts the face's baseline in a row, which is where every face's glyphs go.
    baseline: f32,
}

impl Face {
    fn of(ui: &Ui, text: BodyText) -> Self {
        let first_glyph = |job: LayoutJob| {
            ui.fonts_mut(|fonts| fonts.layout_job(job))
                .rows
                .first()
                .and_then(|row| row.glyphs.first())
                .copied()
        };
        let format = text.style.format(Color32::WHITE);
        let space = TextFormat {
            font_id: egui::FontId::new(100.0, format.font_id.family.clone()),
            ..Default::default()
        };
        Self {
            metrics: LineMetrics {
                baseline: text.style.baseline(ui.ctx()),
                x_height: X_HEIGHT * text.style.size,
            },
            space_em: first_glyph(LayoutJob::single_section(" ".to_owned(), space))
                .map_or(0.25, |glyph| glyph.advance_width / 100.0),
            baseline: first_glyph(LayoutJob::single_section("x".to_owned(), format))
                .map_or(0.0, |glyph| glyph.pos.y),
            text,
        }
    }
}

/// The `margin-left` a code block's `style` gives it (`20px` for one made in the editor).
fn code_indent(element: &crate::model::Element) -> f32 {
    element
        .attr("style")
        .and_then(|style| {
            exo_mail::dom::Style::parse(style)
                .get("margin-left")
                .trim_end_matches("px")
                .trim()
                .parse::<f32>()
                .ok()
        })
        .unwrap_or(0.0)
}

/// The boxes of a block's images, in order.
fn image_boxes(block: &Block, pictures: &Pictures, container: f32) -> Vec<ImageBox> {
    block
        .runs()
        .iter()
        .filter_map(|run| match &run.piece {
            Piece::Image(image) => Some(ImageBox::of(
                image,
                ImageSource::of(image).and_then(|source| pictures.get(&source.key())),
                container,
            )),
            _ => None,
        })
        .collect()
}

/// Each galley row's line box, grown for its images (CSS 2.1 section 10.8). It spans the strut's
/// line box and each image's box from the baseline, as tall as both reach.
fn line_boxes(
    galley: &Galley,
    images: &[(usize, ImageBox)],
    line: f32,
    metrics: LineMetrics,
) -> Vec<LineBox> {
    let mut out = Vec::with_capacity(galley.rows.len());
    let (mut start, mut top) = (0, 0.0);
    for row in &galley.rows {
        let end = start + row.char_count_including_newline().0;
        // A `display: block` image alone on its row is a block box, with no strut and no line
        // around it.
        let alone = row.char_count_excluding_newline().0 == 1
            && images
                .iter()
                .any(|(index, image)| *index == start && image.block);
        let (mut above, mut below) = if alone {
            (0.0, 0.0)
        } else {
            (metrics.baseline, line - metrics.baseline)
        };
        for (_, image) in images
            .iter()
            .filter(|(index, _)| (start..end).contains(index))
        {
            let (image_top, image_bottom) = image.extent(metrics.x_height);
            above = above.max(-image_top);
            below = below.max(image_bottom);
        }
        out.push(LineBox {
            top,
            height: above + below,
            shift: above - metrics.baseline,
        });
        top += above + below;
        start = end;
    }
    if out.is_empty() {
        out.push(LineBox {
            top: 0.0,
            height: line,
            shift: 0.0,
        });
    }
    out
}

/// Where each image lands, which is where the pen is at its glyph, on its line's baseline. The pen
/// is the sum of the advances before the glyph in its row (each row is painted from the text's
/// origin). egui puts a glyph on the whole pixel left of the pen, which would put an image up to a
/// pixel left of where Chromium draws it.
fn place_images(
    block: &Block,
    galley: &Galley,
    glyphs: &[(usize, usize)],
    boxes: &[ImageBox],
    lines: &[LineBox],
    metrics: LineMetrics,
) -> Vec<Placed> {
    // The key of each image's pixels. A kept table's key is its own.
    let keys: Vec<Option<String>> = match &block.content {
        Content::Raw { .. } => vec![None],
        _ => block
            .runs()
            .iter()
            .filter_map(|run| match &run.piece {
                Piece::Image(image) => Some(ImageSource::of(image).map(|source| source.key())),
                _ => None,
            })
            .collect(),
    };
    keys.into_iter()
        .zip(glyphs.iter().zip(boxes))
        .map(|(key, (&(index, offset), image))| {
            let mut row_start = 0;
            let row = galley.rows.iter().position(|row| {
                let end = row_start + row.char_count_including_newline().0;
                let here = index < end;
                if !here {
                    row_start = end;
                }
                here
            });
            let row = row
                .unwrap_or(galley.rows.len().saturating_sub(1))
                .min(lines.len() - 1);
            let pen = layout_unit(galley.rows.get(row).map_or(0.0, |placed| {
                placed
                    .glyphs
                    .iter()
                    .take(index.saturating_sub(row_start))
                    .map(|glyph| glyph.advance_width)
                    .sum()
            }));
            let baseline = lines[row].top + lines[row].shift + metrics.baseline;
            let (image_top, _) = image.extent(metrics.x_height);
            Placed {
                offset,
                rect: Rect::from_min_size(
                    pos2(pen + image.padding, baseline + image_top),
                    image.size,
                ),
                column: Rect::from_min_size(
                    pos2(pen, lines[row].top),
                    vec2(image.advance(), lines[row].height),
                ),
                key,
                look: image.look,
            }
        })
        .collect()
}

/// `px` as Chromium's layout holds a length, to the nearest 1/64 px (`LayoutUnit`), which puts
/// what follows text (an image, a button's edge) where Chromium snaps it.
pub(super) fn layout_unit(px: f32) -> f32 {
    (px * 64.0).round() / 64.0
}

/// Lays `doc` out, taking each block's text from `previous` when nothing it depends on changed.
#[expect(
    clippy::too_many_arguments,
    reason = "the layout's inputs, each its own"
)]
fn lay_out(
    ui: &Ui,
    doc: &Document,
    width: f32,
    phase: f32,
    colors: &Colors,
    body: &BodyText,
    splice: Option<(Pos, String, bool)>,
    previous: &[BlockLayout],
    pictures: &Pictures,
    tables: &mut table::Tables,
    detect_dates: bool,
) -> Layout {
    let cached: std::collections::HashMap<u64, &BlockLayout> =
        previous.iter().map(|view| (view.text_key, view)).collect();
    let (theme, scale) = (exo_ui::theme(ui.ctx()), ui.pixels_per_point());
    tables.begin_layout();
    let faces = [Face::of(ui, *body), Face::of(ui, BodyText::CODE)];
    let mut blocks = Vec::with_capacity(doc.blocks.len());
    let mut y = 0.0;
    // Margins pending above the next block (collapsed as CSS does: the largest wins).
    let mut margin: f32 = 0.0;
    let mut bars: Vec<Rect> = Vec::new();
    // Quotes whose bar is still running: `(depth, top, left)`.
    let mut open_bars: Vec<(usize, f32, f32)> = Vec::new();
    let mut ordinals: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (index, block) in doc.blocks.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| &doc.blocks[i]);
        let shared = previous.map_or(0, |previous| {
            previous
                .path
                .iter()
                .zip(&block.path)
                .take_while(|(a, b)| a.id == b.id)
                .count()
        });
        // Containers closing after the previous block, then those opening before this one.
        if let Some(previous) = previous {
            for container in &previous.path[shared..] {
                margin = margin.max(container_margin(previous, container));
            }
            // Quotes that end here draw their bar down to the end of the previous block.
            let (closed, still_open): (Vec<_>, Vec<_>) = open_bars
                .into_iter()
                .partition(|(depth, _, _)| *depth >= shared);
            open_bars = still_open;
            bars.extend(
                closed
                    .into_iter()
                    .map(|(_, top, left)| Rect::from_min_max(pos2(left, top), pos2(left + 1.0, y))),
            );
        }
        for container in &block.path[shared..] {
            margin = margin.max(container_margin(block, container));
        }
        if previous.is_some_and(|previous| previous.element.is("p") && previous.path == block.path)
            && block.element.is("p")
        {
            margin = margin.max(16.0);
        }
        if block.element.is("pre") || previous.is_some_and(|previous| previous.element.is("pre")) {
            margin = margin.max(CODE_MARGIN);
        }
        y += margin;
        margin = 0.0;
        let mut left = 0.0;
        for (depth, container) in block.path.iter().enumerate() {
            match container.element.container_kind() {
                ContainerKind::List { .. } => left += LIST_INDENT,
                ContainerKind::Quote => {
                    if depth >= shared {
                        open_bars.push((depth, y, left));
                    }
                    left += QUOTE_INDENT;
                }
                ContainerKind::Indent => left += INDENT,
                ContainerKind::Code => left += code_indent(&container.element),
                ContainerKind::Other => {}
            }
        }
        if block.element.is("pre") {
            left += code_indent(&block.element);
        }
        let face = &faces[usize::from(block.in_code())];
        let text_width = (width - left).max(1.0);
        let color = colors_for(block, colors);
        let splice_here = splice.as_ref().filter(|(pos, _, _)| pos.block == index);
        // A kept table is one block-level atom, drawn by Blitz, as tall as its table.
        let boxes = match &block.content {
            Content::Raw { html, .. } => {
                let (key, size) = tables.lay_out(
                    &html::in_containers(&block.path, html),
                    text_width,
                    scale,
                    theme,
                    pictures,
                );
                vec![ImageBox {
                    size,
                    padding: 0.0,
                    look: Look::Table(key),
                    block: true,
                }]
            }
            _ => image_boxes(block, pictures, text_width),
        };
        let key = text_key(block, color, text_width, splice_here, &boxes);
        let (galley, insert, decorations, row_galleys, lines, images, dates) = match cached
            .get(&key)
        {
            Some(view) => (
                view.galley.clone(),
                view.insert,
                view.decorations.clone(),
                view.rows.clone(),
                view.lines.clone(),
                view.images.clone(),
                view.dates.clone(),
            ),
            None => {
                let dates = if detect_dates {
                    crate::dates::detect(block)
                } else {
                    Vec::new()
                };
                let BlockJob {
                    job,
                    insert,
                    decorations,
                    images: glyphs,
                } = block_job(
                    block,
                    color,
                    colors,
                    &face.text,
                    text_width,
                    phase,
                    splice_here,
                    &boxes,
                    face.space_em,
                    &dates,
                );
                let mut galley = exo_ui::layout(ui.ctx(), job.clone());
                align_rows(&mut galley, face.baseline, phase);
                let rows = if galley.rows.len() > 1 {
                    row_jobs(&job, &galley, phase, &face.text)
                        .into_iter()
                        .map(|row| {
                            let mut row = exo_ui::layout(ui.ctx(), row);
                            align_rows(&mut row, face.baseline, 0.0);
                            row
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let sized: Vec<(usize, ImageBox)> = glyphs
                    .iter()
                    .map(|(index, _)| *index)
                    .zip(boxes.iter().copied())
                    .collect();
                let lines = line_boxes(&galley, &sized, face.text.line(), face.metrics);
                let images = place_images(block, &galley, &glyphs, &boxes, &lines, face.metrics);
                (galley, insert, decorations, rows, lines, images, dates)
            }
        };
        let rule = matches!(block.content, Content::Rule);
        let height = if rule {
            RULE_HEIGHT
        } else {
            lines.iter().map(|line| line.height).sum()
        };
        let marker = list_marker(block, &mut ordinals);
        blocks.push(BlockLayout {
            top: y,
            left,
            height,
            galley,
            phase,
            insert,
            marker,
            rule,
            decorations,
            rows: row_galleys,
            lines,
            images,
            text_key: key,
            text: face.text,
            dates,
        });
        y += height;
    }
    // Containers still open at the end close here.
    if let Some(last) = doc.blocks.last() {
        for container in &last.path {
            margin = margin.max(container_margin(last, container));
        }
        if last.element.is("pre") {
            margin = margin.max(CODE_MARGIN);
        }
    }
    bars.extend(
        open_bars
            .into_iter()
            .map(|(_, top, left)| Rect::from_min_max(pos2(left, top), pos2(left + 1.0, y))),
    );
    tables.end_layout();
    let space = ui
        .fonts_mut(|fonts| {
            fonts.layout_job(LayoutJob::single_section(
                " ".to_owned(),
                body.style.format(colors.text),
            ))
        })
        .size()
        .x;
    Layout {
        key: LayoutKey::default(),
        blocks,
        height: y + margin,
        bars,
        space,
        width,
        baseline: faces[0].baseline,
    }
}

/// Puts every glyph on the body's baseline, where egui would hang an italic run higher than the
/// text around it (`exo_ui::align_baselines`), and every wrapped row at the text's sub-pixel
/// phase. Only the first row carries the phase `exo_ui::paint_job` lays text out at, so later
/// rows would otherwise start half a pixel off.
pub(super) fn align_rows(galley: &mut Arc<Galley>, baseline: f32, phase: f32) {
    exo_ui::align_baselines(galley, baseline);
    if galley.rows.len() > 1 && phase != 0.0 {
        for placed in Arc::make_mut(galley).rows.iter_mut().skip(1) {
            placed.pos.x += phase;
        }
    }
}

/// One unwrapped job per row of `galley` (laid out from `job`), each starting at `phase`.
fn row_jobs(job: &LayoutJob, galley: &Galley, phase: f32, body: &BodyText) -> Vec<LayoutJob> {
    let bytes: Vec<usize> = job
        .text
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(job.text.len()))
        .collect();
    let mut start = 0;
    let mut out = Vec::with_capacity(galley.rows.len());
    for row in &galley.rows {
        let end = start + row.char_count_excluding_newline().0;
        let range = bytes[start.min(bytes.len() - 1)]..bytes[end.min(bytes.len() - 1)];
        let mut row_job = LayoutJob {
            wrap: TextWrapping::no_max_width(),
            ..Default::default()
        };
        for (index, section) in job.sections.iter().enumerate() {
            let (from, to) = (
                section.byte_range.start.0.max(range.start),
                section.byte_range.end.0.min(range.end),
            );
            if from >= to {
                continue;
            }
            // An image's room (its section's leading space) stays where it starts.
            let own = if from == section.byte_range.start.0 && index > 0 {
                section.leading_space
            } else {
                0.0
            };
            let leading = if row_job.sections.is_empty() {
                phase + own
            } else {
                own
            };
            row_job.append(&job.text[from..to], leading, section.format.clone());
        }
        if row_job.sections.is_empty() {
            row_job.append("", phase, body.style.format(Color32::TRANSPARENT));
        }
        out.push(row_job);
        start += row.char_count_including_newline().0;
    }
    out
}

/// A container's vertical margin, which is 16px for a list not inside another list, or a code
/// block's.
fn container_margin(block: &Block, container: &crate::model::Container) -> f32 {
    match container.element.container_kind() {
        ContainerKind::Code => return CODE_MARGIN,
        ContainerKind::List { .. } => {}
        _ => return 0.0,
    }
    let depth = block
        .path
        .iter()
        .position(|c| c.id == container.id)
        .unwrap_or(0);
    let nested = block.path[..depth]
        .iter()
        .any(|c| matches!(c.element.container_kind(), ContainerKind::List { .. }));
    if nested { 0.0 } else { LIST_MARGIN }
}

/// A list item's marker, from its `list-style-type` or else by nesting (`ul ul` circle, deeper
/// square). Numbers count the items before it in its list.
fn list_marker(
    block: &Block,
    ordinals: &mut std::collections::HashMap<u32, u32>,
) -> Option<Marker> {
    if !block.is_list_item() {
        return None;
    }
    let (_, list) = block.list()?;
    let ordered = matches!(
        list.element.container_kind(),
        ContainerKind::List { ordered: true }
    );
    let style = block
        .element
        .attr("style")
        .map(exo_mail::dom::Style::parse)
        .and_then(|style| {
            let value = style.get("list-style-type");
            (!value.is_empty()).then(|| value.to_owned())
        });
    let count = ordinals.entry(list.id).or_insert_with(|| {
        list.element
            .attr("start")
            .and_then(|start| start.parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1)
    });
    *count = block
        .element
        .attr("value")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| count.saturating_add(1));
    let depth = block.list_depth();
    let style = style.unwrap_or_else(|| {
        if ordered {
            "decimal".to_owned()
        } else {
            ["disc", "circle", "square"][(depth.max(1) - 1).min(2)].to_owned()
        }
    });
    Some(match style.as_str() {
        "disc" => Marker::Disc,
        "circle" => Marker::Circle,
        "square" => Marker::Square,
        "lower-alpha" => Marker::Text(format!("{}.", alpha(*count))),
        // CSS Counter Styles bounds `lower-roman` to 1..=3999, falling back to decimal outside it.
        "lower-roman" if (1..=3999).contains(count) => Marker::Text(format!("{}.", roman(*count))),
        _ => Marker::Text(format!("{count}.")),
    })
}

fn alpha(mut n: u32) -> String {
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(char::from(b'a' + (n % 26) as u8));
        n /= 26;
    }
    out.iter().rev().collect()
}

fn roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, symbol) in TABLE {
        while n >= value {
            out.push_str(symbol);
            n -= value;
        }
    }
    out
}

/// A block's text as an egui job.
struct BlockJob {
    job: LayoutJob,
    /// `(offset, length)` of the splice.
    insert: Option<(usize, usize)>,
    decorations: Vec<Decoration>,
    /// Each image's glyph: its galley index and its offset in the block.
    images: Vec<(usize, usize)>,
}

/// One block's text as an egui job, with `splice` (IME or ghost text) inserted at its offset. Each
/// image (`images`, in order) becomes a transparent space whose advance is the image's width (a
/// space is `space_em` px of font size wide), in a line box of the text's. A line doesn't break
/// inside one of its `dates` (`white-space: nowrap`).
#[expect(
    clippy::too_many_arguments,
    reason = "the layout's inputs, each its own"
)]
fn block_job(
    block: &Block,
    color: Color32,
    colors: &Colors,
    body: &BodyText,
    width: f32,
    phase: f32,
    splice: Option<&(Pos, String, bool)>,
    images: &[ImageBox],
    space_em: f32,
    dates: &[std::ops::Range<usize>],
) -> BlockJob {
    let mut job = LayoutJob {
        wrap: TextWrapping::wrap_at_width(width + phase),
        ..Default::default()
    };
    let mut insert = None;
    let runs = match &block.content {
        Content::Inline(runs) => runs.clone(),
        Content::Raw { .. } => vec![crate::model::Run {
            piece: Piece::Image(crate::model::Element::new("table")),
            marks: Vec::new(),
        }],
        Content::Rule => Vec::new(),
    };
    // Spaces a browser would collapse are `&nbsp;`s, which egui mustn't break a line at. A
    // code block's spaces are all plain.
    let mut spaces = if block.in_code() {
        vec![false; runs.iter().map(|run| run.piece.len()).sum()]
    } else {
        html::space_kinds(&runs)
    };
    for at in dates.iter().flat_map(Clone::clone) {
        if let Some(space) = spaces.get_mut(at) {
            *space = true;
        }
    }
    let mut position = 0;
    // The first section starts at the sub-pixel phase (`exo_ui::paint_job`).
    let push = |job: &mut LayoutJob, text: &str, format: TextFormat| {
        let leading = if job.sections.is_empty() { phase } else { 0.0 };
        job.append(text, leading, format);
    };
    let splice_at = splice.map(|(pos, _, _)| pos.offset);
    let mut spliced = false;
    let mut decorations: Vec<Decoration> = Vec::new();
    let mut boxes = images.iter();
    let mut placed = Vec::new();
    let emit_splice = |job: &mut LayoutJob,
                       position: usize,
                       insert: &mut Option<(usize, usize)>,
                       spliced: &mut bool| {
        if let Some((_, text, composing)) = splice
            && splice_at == Some(position)
            && !*spliced
        {
            *spliced = true;
            let mut format = body
                .style
                .format(if *composing { color } else { colors.ghost });
            if *composing {
                format.underline = Stroke::new(1.0, color);
            }
            *insert = Some((position, text.chars().count()));
            push(job, text, format);
        }
    };
    for run in &runs {
        let (format, lines) = run_format(&run.marks, color, body);
        let before = job.text.chars().count();
        match &run.piece {
            Piece::Text(text) => {
                let mut chunk = String::new();
                for ch in text.chars() {
                    if splice_at == Some(position) && !spliced && !chunk.is_empty() {
                        push(&mut job, &chunk, format.clone());
                        chunk.clear();
                    }
                    emit_splice(&mut job, position, &mut insert, &mut spliced);
                    chunk.push(if ch == ' ' && spaces[position] {
                        '\u{A0}'
                    } else {
                        ch
                    });
                    position += 1;
                }
                if !chunk.is_empty() {
                    push(&mut job, &chunk, format.clone());
                }
            }
            Piece::Break => {
                emit_splice(&mut job, position, &mut insert, &mut spliced);
                push(&mut job, "\n", format.clone());
                position += 1;
            }
            Piece::Image(_) => {
                emit_splice(&mut job, position, &mut insert, &mut spliced);
                let advance = boxes.next().map_or(0.0, ImageBox::advance);
                let mut glyph = body.style.format(Color32::TRANSPARENT);
                glyph.font_id.size = (advance / space_em).max(0.01);
                glyph.line_height = Some(body.line());
                placed.push((job.text.chars().count(), position));
                push(&mut job, " ", glyph);
                position += 1;
                // Chromium draws no text decoration across an image.
                continue;
            }
        }
        // The run's decorations cover what it added (a splice inside it too, as the browser
        // composes inside the element).
        let galley_chars = job.text.chars().count();
        for (kind, line_color) in lines {
            match decorations.last_mut() {
                Some(last)
                    if last.kind == kind
                        && last.color == line_color
                        && last.chars.end == before =>
                {
                    last.chars.end = galley_chars
                }
                _ => decorations.push(Decoration {
                    chars: before..galley_chars,
                    kind,
                    color: line_color,
                }),
            }
        }
    }
    emit_splice(&mut job, position, &mut insert, &mut spliced);
    if job.sections.is_empty() {
        // An empty line still has a line box.
        job.append("", phase, body.style.format(color));
    }
    BlockJob {
        job,
        insert,
        decorations,
        images: placed,
    }
}

/// The egui format for text inside `marks`, and the decoration lines over it. A line takes the
/// colour of the element that draws it (`<s><span class="colour">` strikes in the text colour).
fn run_format(
    marks: &[crate::model::Element],
    base: Color32,
    body: &BodyText,
) -> (TextFormat, Vec<(DecorationKind, Color32)>) {
    let mut bold = false;
    let mut italic = false;
    let mut color = base;
    let mut lines = Vec::new();
    for mark in marks {
        let parsed = mark.attr("style").map(exo_mail::dom::Style::parse);
        if mark.is("b") {
            bold = true;
        }
        if mark.is("i") {
            italic = true;
        }
        if let Some(style) = &parsed {
            if let Some(weight) = css_bold(style.get("font-weight")) {
                bold = weight;
            }
            if let Some(slant) = css_italic(style.get("font-style")) {
                italic = slant;
            }
        }
        if mark.is("span")
            && mark
                .attr("class")
                .is_some_and(|class| class.split_whitespace().any(|class| class == "colour"))
        {
            color = match Color::of(mark) {
                Some(Color::None) => base,
                _ => parsed.as_ref().and_then(css_color).unwrap_or(color),
            };
        } else if let Some(value) = parsed.as_ref().and_then(css_color) {
            color = value;
        }
        // The `sh-` spelling comes in senders' signatures and in drafts begun elsewhere.
        if mark.is("a")
            && mark.attr("href").is_some()
            && !mark.attr("class").is_some_and(|class| {
                class.contains("exo-preserve-color") || class.contains("sh-preserve-color")
            })
        {
            color = LINK;
        }
        if mark.is("u")
            && !lines
                .iter()
                .any(|(kind, _)| *kind == DecorationKind::Underline)
        {
            lines.push((DecorationKind::Underline, color));
        }
        if mark.is("s")
            && !lines
                .iter()
                .any(|(kind, _)| *kind == DecorationKind::LineThrough)
        {
            lines.push((DecorationKind::LineThrough, color));
        }
    }
    let mut font = Font::new(body.style.font.family, if bold { 700 } else { 400 });
    if italic {
        font = font.italic();
    }
    let style = TextStyle { font, ..body.style };
    (style.format(color), lines)
}

/// Whether a parsed `font-weight` declaration is bold (`bold`, `bolder`, or numeric >= 600),
/// explicitly regular, or unrecognized (`None`).
fn css_bold(value: &str) -> Option<bool> {
    if value.is_empty() {
        return None;
    }
    if value.eq_ignore_ascii_case("bold") || value.eq_ignore_ascii_case("bolder") {
        Some(true)
    } else if value.eq_ignore_ascii_case("normal") || value.eq_ignore_ascii_case("lighter") {
        Some(false)
    } else {
        value.parse::<u16>().ok().map(|weight| weight >= 600)
    }
}

/// Whether a parsed `font-style` declaration is italic/oblique, explicitly `normal`, or
/// unrecognized (`None`).
fn css_italic(value: &str) -> Option<bool> {
    if value.is_empty() {
        return None;
    }
    if value.eq_ignore_ascii_case("italic")
        || value.eq_ignore_ascii_case("oblique")
        || value
            .split_once(char::is_whitespace)
            .is_some_and(|(head, _)| head.eq_ignore_ascii_case("oblique"))
    {
        Some(true)
    } else if value.eq_ignore_ascii_case("normal") {
        Some(false)
    } else {
        None
    }
}

/// The `color` of a parsed `style` attribute.
fn css_color(style: &exo_mail::dom::Style) -> Option<Color32> {
    let value = style.get("color");
    if value.is_empty() {
        return None;
    }
    let [r, g, b, a] = exo_mail::css_color(value)?;
    Some(Color32::from_rgba_unmultiplied(r, g, b, a))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::{Container, Element};

    #[test]
    fn lower_roman_falls_back_to_decimal_and_counter_saturates() {
        let list = Container {
            id: 1,
            element: Element::new("ol"),
        };
        let item = |value: Option<&str>| {
            let mut element = Element::with("li", &[("style", "list-style-type: lower-roman")]);
            if let Some(value) = value {
                element.set_attr("value", value);
            }
            Block {
                path: vec![list.clone()],
                element,
                content: Content::Inline(Vec::new()),
                trailing_break: false,
            }
        };
        let mut ordinals = HashMap::new();
        assert_eq!(
            list_marker(&item(Some("3999")), &mut ordinals),
            Some(Marker::Text("mmmcmxcix.".to_owned()))
        );
        assert_eq!(
            list_marker(&item(Some("4000")), &mut ordinals),
            Some(Marker::Text("4000.".to_owned()))
        );
        assert_eq!(
            list_marker(&item(Some("0")), &mut ordinals),
            Some(Marker::Text("0.".to_owned()))
        );
        assert_eq!(
            list_marker(&item(Some("4294967295")), &mut ordinals),
            Some(Marker::Text("4294967295.".to_owned()))
        );
        // The following item without `value` saturates instead of overflowing `u32`.
        assert_eq!(
            list_marker(&item(None), &mut ordinals),
            Some(Marker::Text("4294967295.".to_owned()))
        );
    }

    #[test]
    fn run_format_parses_spaced_and_case_insensitive_css_styles() {
        let base = Color32::BLACK;
        let body = BodyText::COMPOSE;
        let plain = run_format(&[], base, &body).0;
        let bold = run_format(&[Element::new("b")], base, &body).0;
        let italic = run_format(&[Element::new("i")], base, &body).0;

        for style in [
            "font-weight: 700",
            "font-weight:700",
            "font-weight:bold",
            "FONT-WEIGHT: BOLD",
            "font-weight: bolder",
            "font-weight: 600 !important",
        ] {
            let mark = Element::with("span", &[("style", style)]);
            let format = run_format(&[mark], base, &body).0;
            assert_eq!(format.font_id, bold.font_id, "bold font for `{style}`");
            assert_eq!(format.coords, bold.coords, "bold coords for `{style}`");
        }

        let medium = Element::with("span", &[("style", "font-weight: 500")]);
        assert_eq!(run_format(&[medium], base, &body).0.coords, plain.coords);

        for style in [
            "font-style: italic",
            "font-style:italic",
            "FONT-STYLE: ITALIC",
            "font-style: oblique",
            "font-style: oblique 12deg",
        ] {
            let mark = Element::with("span", &[("style", style)]);
            let format = run_format(&[mark], base, &body).0;
            assert_eq!(format.font_id, italic.font_id, "italic font for `{style}`");
            assert_eq!(format.italics, italic.italics, "italic flag for `{style}`");
        }

        let combined = Element::with(
            "span",
            &[(
                "style",
                "color: rgb(222, 84, 84); font-weight: 700; font-style: italic",
            )],
        );
        let format = run_format(&[combined], base, &body).0;
        assert_eq!(format.color, Color32::from_rgb(222, 84, 84));
        assert_eq!(format.coords, bold.coords);
        assert_eq!(format.italics, italic.italics);
    }

    #[test]
    fn row_spans_stay_ordered_on_soft_wrapped_continuation_rows() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        let text = "alpha beta gamma delta epsilon";
        let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::default());
        job.wrap = TextWrapping::wrap_at_width(70.0);
        let mut galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        assert!(
            galley.rows.len() >= 2 && !galley.rows[0].ends_with_newline,
            "expected a soft-wrapped multi-row galley, got {} rows",
            galley.rows.len()
        );
        let row1_start = galley.rows[0].char_count_including_newline().0;
        let row1_len = galley.rows[1].char_count_excluding_newline().0;
        assert!(row1_len >= 2);
        // Confirm why `pos_from_cursor` was broken here: the boundary index maps to row 0's end.
        assert!(
            galley
                .pos_from_cursor(egui::text::CCursor::new(row1_start))
                .min
                .x
                > 0.0
        );
        let total = text.chars().count();
        let make_view = |galley: Arc<Galley>, phase: f32| {
            let lines = (0..galley.rows.len())
                .map(|index| LineBox {
                    top: index as f32 * 24.0,
                    height: 24.0,
                    shift: 0.0,
                })
                .collect();
            BlockLayout {
                top: 0.0,
                left: 0.0,
                height: galley.rows.len() as f32 * 24.0,
                galley,
                phase,
                insert: None,
                marker: None,
                rule: false,
                decorations: Vec::new(),
                rows: Vec::new(),
                lines,
                images: Vec::new(),
                text_key: 0,
                text: BodyText::COMPOSE,
                dates: Vec::new(),
            }
        };

        let view = make_view(galley.clone(), 0.0);
        // Span wrapping from row 0 onto row 1.
        let wrapped: Vec<_> = view.row_spans(0..total).collect();
        assert!(wrapped.len() >= 2);
        let (line1, x0, x1) = wrapped[1];
        assert_eq!(line1.top, 24.0);
        assert_eq!(x0, 0.0);
        assert!(x0 < x1, "expected x0 < x1 on wrapped row, got {x0}..{x1}");

        // Span starting at column 0 of the soft-wrapped continuation row.
        let continuation: Vec<_> = view.row_spans(row1_start..row1_start + row1_len).collect();
        assert_eq!(continuation.len(), 1);
        assert_eq!(continuation[0].1, 0.0);
        assert!(
            continuation[0].1 < continuation[0].2,
            "expected x0 < x1 at continuation row start, got {:?}",
            continuation[0]
        );

        // With sub-pixel phase applied to continuation rows, `x0` tracks the row's phase.
        for placed in Arc::make_mut(&mut galley).rows.iter_mut().skip(1) {
            placed.pos.x += 0.5;
        }
        let phased = make_view(galley, 0.5);
        let continuation: Vec<_> = phased
            .row_spans(row1_start..row1_start + row1_len)
            .collect();
        assert_eq!(continuation.len(), 1);
        assert_eq!(continuation[0].1, 0.5);
        assert!(continuation[0].1 < continuation[0].2);
    }
}
