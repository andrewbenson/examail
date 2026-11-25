//! Colour emoji in text, placed where Chromium puts them.
//!
//! egui draws glyph outlines only, and Apple Color Emoji is all bitmaps. So the font is loaded
//! as a fallback of every family purely for its advances, and draws nothing. [`fit_advances`]
//! then gives each emoji the room Chromium gives it, and [`paint`] paints each emoji cluster's
//! picture (fastframe-emoji, the platform's own artwork) in the font's cell at the start of that
//! room. The cell is one em wide and ascent plus descent tall, on the line's baseline, which is
//! the box CoreText draws the emoji in. An emoji takes its text color's alpha, as Chromium's do,
//! so one in faded text is faded too. Pictures are drawn at the cell's size in device pixels and
//! kept per context.

use std::{collections::HashMap, ops::Range, sync::Arc};

use egui::{
    Color32, ColorImage, FontData, FontDefinitions, FontFamily, Galley, Id, Painter, Pos2, Rect,
    TextureHandle, TextureOptions,
    epaint::text::{ByteIndex, Glyph, LayoutSection},
    pos2,
    text::LayoutJob,
    vec2,
};
use skrifa::{FontRef, MetadataProvider as _, instance::LocationRef, instance::Size};

use crate::text::{shift_glyph, update_bounds};

/// macOS's emoji font, which Chromium falls back to for emoji.
const SYSTEM_EMOJI: &str = "/System/Library/Fonts/Apple Color Emoji.ttc";
/// egui lays each emoji out with the font's advance (1em) times this, before [`fit_advances`]
/// corrects it. This is Chromium's advance at the 12px and 16px of most UI text, so the few
/// galleys laid out without [`crate::layout`] are close.
const ADVANCE_SCALE: f32 = 1.25;
/// The egui font name it's registered under.
pub(crate) const FONT_NAME: &str = "AppleColorEmoji";

/// The emoji font's cell, in ems.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cell {
    ascent: f32,
    descent: f32,
    /// A single emoji's advance (U+1F600's), which every emoji cluster is laid out with.
    advance: f32,
}

/// Maps the system emoji font for egui and measures its cell. `None` where there is none (off
/// macOS), and emoji stay egui's replacement glyph.
pub(crate) fn load() -> Option<(FontData, Cell)> {
    // The font is mapped, so of its 190 MB only the tables in use are paged in.
    let bytes = crate::fonts::mapped(SYSTEM_EMOJI)?;
    let font = FontRef::from_index(bytes, 0).ok()?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let units = f32::from(metrics.units_per_em);
    let grinning = font.charmap().map('\u{1F600}')?;
    let advance = font
        .glyph_metrics(Size::unscaled(), LocationRef::default())
        .advance_width(grinning)?;
    let cell = Cell {
        ascent: metrics.ascent / units,
        descent: -metrics.descent / units,
        advance: advance / units,
    };
    let mut data = FontData::from_static(bytes);
    data.tweak.hinting = Some(false);
    // Only the advance matters, because the font draws nothing.
    data.tweak.scale = ADVANCE_SCALE;
    Some((data, cell))
}

fn cell_id() -> Id {
    Id::new("exo_ui::emoji_cell")
}

fn pictures_id() -> Id {
    Id::new("exo_ui::emoji_pictures")
}

/// Pictures uploaded to a context, by cluster and height in device pixels; `None` when no font
/// draws the cluster.
type Pictures = Arc<std::sync::Mutex<HashMap<(String, u32), Option<TextureHandle>>>>;

/// Remembers `cell` for [`paint`].
pub(crate) fn install(ctx: &egui::Context, cell: Cell) {
    ctx.data_mut(|data| data.insert_temp(cell_id(), cell));
}

/// The emoji-first egui font family, containing only `AppleColorEmoji` so emoji base codepoints
/// (`'1'` in `"1️⃣"`, `'❤'` in `"❤️"`) never draw monochrome text outlines underneath [`paint`].
pub(crate) fn family() -> FontFamily {
    FontFamily::Name(FONT_NAME.into())
}

/// Registers [`family`] in `definitions`.
pub(crate) fn register_family(definitions: &mut FontDefinitions) {
    definitions
        .families
        .insert(family(), vec![FONT_NAME.to_owned()]);
}

/// Splits `job`'s sections on [`fastframe_emoji::pieces`] so each [`fastframe_emoji::Piece::Emoji`]
/// cluster is shaped in [`family`] at the section's exact CSS font size and line height.
pub(crate) fn split_sections(ctx: &egui::Context, mut job: LayoutJob) -> LayoutJob {
    if job.text.is_ascii() || ctx.data(|data| data.get_temp::<Cell>(cell_id())).is_none() {
        return job;
    }
    let emoji_family = family();
    let has_emoji = job.sections.iter().any(|section| {
        let slice = &job.text[section.byte_range.start.0..section.byte_range.end.0];
        !slice.is_ascii()
            && section.format.font_id.family != emoji_family
            && fastframe_emoji::pieces(slice)
                .iter()
                .any(|piece| matches!(piece, fastframe_emoji::Piece::Emoji(_)))
    });
    if !has_emoji {
        return job;
    }
    let base_ptr = job.text.as_ptr() as usize;
    for section in std::mem::take(&mut job.sections) {
        let slice = &job.text[section.byte_range.start.0..section.byte_range.end.0];
        if slice.is_ascii() || section.format.font_id.family == emoji_family {
            job.sections.push(section);
            continue;
        }
        let pieces = fastframe_emoji::pieces(slice);
        if !pieces
            .iter()
            .any(|piece| matches!(piece, fastframe_emoji::Piece::Emoji(_)))
        {
            job.sections.push(section);
            continue;
        }
        let line_height = section
            .format
            .line_height
            .or_else(|| Some(ctx.fonts_mut(|fonts| fonts.row_height(&section.format.font_id))));
        let mut emoji_format = section.format.clone();
        emoji_format.font_id.family = emoji_family.clone();
        emoji_format.line_height = line_height;
        emoji_format.italics = false;
        emoji_format.coords = Default::default();
        let mut leading_space = section.leading_space;
        for piece in pieces {
            let (part, format) = match piece {
                fastframe_emoji::Piece::Text("") => continue,
                fastframe_emoji::Piece::Text(part) => (part, section.format.clone()),
                fastframe_emoji::Piece::Emoji(part) => (part, emoji_format.clone()),
            };
            let start = part.as_ptr() as usize - base_ptr;
            job.sections.push(LayoutSection {
                leading_space: std::mem::take(&mut leading_space),
                byte_range: ByteIndex(start)..ByteIndex(start + part.len()),
                format,
            });
        }
    }
    job
}

fn section_at(sections: &[LayoutSection], byte_offset: usize) -> Option<&LayoutSection> {
    let idx = sections.partition_point(|section| section.byte_range.end.0 <= byte_offset);
    sections
        .get(idx)
        .filter(|section| section.byte_range.start.0 <= byte_offset)
}

/// The advance Chromium lays an Apple Color Emoji cluster out with at `size` px. It's a whole
/// number of px, wider than the font's 1em up to 24px (CoreText's optical advances): 1.25em
/// rounded up to 16px, then `(size + 25) / 2` rounded down, which meets 1em at 24px, then 1em
/// rounded. This was measured in Chromium from 8px to 48px every 0.05px (in single precision, as
/// Chromium rounds 12.4px's 15.5 down).
fn advance(size: f32) -> f32 {
    (1.25 * size)
        .round()
        .min(((size + 25.0) / 2.0).floor())
        .max(size.round())
}

/// Gives each emoji in `galley` the advance Chromium gives it ([`advance`]) instead of egui's
/// (1.25em). What follows on its row moves by the difference, on whole device pixels.
pub(crate) fn fit_advances(ctx: &egui::Context, galley: &mut Arc<Galley>) {
    let job = galley.job.clone();
    let clusters = clusters(&job.text);
    let Some(cell) = ctx
        .data(|data| data.get_temp::<Cell>(cell_id()))
        .filter(|_| !clusters.is_empty())
    else {
        return;
    };
    let pixels_per_point = ctx.pixels_per_point();
    let glyph_correction = |glyph: &Glyph, seen_start: &mut Option<usize>| -> Option<f32> {
        let (range, _) = cluster_containing(&clusters, glyph.cluster as usize)?;
        let target = if *seen_start == Some(range.start) {
            0.0
        } else {
            *seen_start = Some(range.start);
            let (size, letter_spacing) = section_at(&job.sections, glyph.cluster as usize).map_or(
                (glyph.advance_width / (cell.advance * ADVANCE_SCALE), 0.0),
                |section| {
                    (
                        section.format.font_id.size,
                        section.format.extra_letter_spacing,
                    )
                },
            );
            advance(size) + letter_spacing
        };
        Some(((target - glyph.advance_width) * pixels_per_point).round() / pixels_per_point)
    };
    if galley.rows.iter().all(|placed| {
        let mut seen = None;
        placed.row.glyphs.iter().all(|glyph| {
            let in_emoji = cluster_containing(&clusters, glyph.cluster as usize).is_some();
            (!in_emoji || glyph.uv_rect.is_nothing())
                && glyph_correction(glyph, &mut seen).is_none_or(|c| c == 0.0)
        })
    }) {
        return;
    }
    let galley = Arc::make_mut(galley);
    for placed in &mut galley.rows {
        let mut check_seen = None;
        if placed.glyphs.iter().all(|glyph| {
            let in_emoji = cluster_containing(&clusters, glyph.cluster as usize).is_some();
            (!in_emoji || glyph.uv_rect.is_nothing())
                && glyph_correction(glyph, &mut check_seen).is_none_or(|c| c == 0.0)
        }) {
            continue;
        }
        let row = Arc::make_mut(&mut placed.row);
        let mut offset = 0.0;
        let mut seen = None;
        for index in 0..row.glyphs.len() {
            if cluster_containing(&clusters, row.glyphs[index].cluster as usize).is_some()
                && !row.glyphs[index].uv_rect.is_nothing()
            {
                let first = row.glyphs[index].first_vertex as usize;
                if row.visuals.glyph_vertex_range.contains(&first)
                    && let Some(quad) = row.visuals.mesh.vertices.get_mut(first..first + 4)
                {
                    for vertex in quad {
                        vertex.pos = Pos2::ZERO;
                        vertex.uv = Pos2::ZERO;
                        vertex.color = Color32::TRANSPARENT;
                    }
                }
                row.glyphs[index].uv_rect = Default::default();
            }
            shift_glyph(row, index, vec2(offset, 0.0));
            if let Some(c) = glyph_correction(&row.glyphs[index], &mut seen) {
                row.glyphs[index].advance_width = (row.glyphs[index].advance_width + c).max(0.0);
                offset += c;
            }
        }
        if row.visuals.glyph_vertex_range.start == 0
            && row.glyphs.iter().all(|glyph| glyph.uv_rect.is_nothing())
        {
            row.visuals.mesh.clear();
            row.visuals.glyph_vertex_range = 0..0;
        }
        row.size.x += offset;
        row.visuals.mesh_bounds = row.visuals.mesh.calc_bounds();
    }
    update_bounds(galley);
}

/// The emoji clusters of `text` (each cluster's UTF-8 byte range, and the cluster).
fn clusters(text: &str) -> Vec<(Range<usize>, &str)> {
    // Emoji start at U+00A9, so ASCII text has none, and most text is ASCII.
    if text.is_ascii() {
        return Vec::new();
    }
    fastframe_emoji::pieces(text)
        .into_iter()
        .filter_map(|piece| match piece {
            fastframe_emoji::Piece::Emoji(cluster) => {
                let start = cluster.as_ptr() as usize - text.as_ptr() as usize;
                Some((start..start + cluster.len(), cluster))
            }
            fastframe_emoji::Piece::Text(_) => None,
        })
        .collect()
}

/// The cluster containing byte offset `byte`, if it's one of `clusters`.
fn cluster_containing<'a>(
    clusters: &'a [(Range<usize>, &'a str)],
    byte: usize,
) -> Option<&'a (Range<usize>, &'a str)> {
    let idx = clusters.partition_point(|(range, _)| range.end <= byte);
    clusters.get(idx).filter(|(range, _)| range.start <= byte)
}

/// Paints the pictures of the emoji in `galley`, with its top left at `origin` and its glyphs on
/// their line's baseline ([`crate::align_baselines`]), each at its text color's alpha.
pub(crate) fn paint(painter: &Painter, origin: Pos2, galley: &Galley) {
    let clusters = clusters(&galley.job.text);
    if clusters.is_empty() {
        return;
    }
    let Some(cell) = painter.ctx().data(|data| data.get_temp::<Cell>(cell_id())) else {
        return;
    };
    for row in &galley.rows {
        let mut painted_start = None;
        for glyph in row.glyphs.iter().filter(|glyph| glyph.advance_width > 0.0) {
            let Some((range, cluster)) = cluster_containing(&clusters, glyph.cluster as usize)
            else {
                continue;
            };
            if painted_start == Some(range.start) {
                continue;
            }
            painted_start = Some(range.start);
            let section = section_at(&galley.job.sections, glyph.cluster as usize);
            let em = section.map_or(
                glyph.advance_width / (cell.advance * ADVANCE_SCALE),
                |section| section.format.font_id.size,
            );
            let baseline = origin.y + row.pos.y + glyph.pos.y;
            let rect = Rect::from_min_size(
                pos2(
                    origin.x + row.pos.x + glyph.pos.x,
                    baseline - cell.ascent * em,
                ),
                vec2(cell.advance * em, (cell.ascent + cell.descent) * em),
            );
            let alpha = section
                .map(|section| section.format.color)
                .filter(|color| *color != Color32::PLACEHOLDER)
                .map_or(u8::MAX, |color| color.a());
            draw(painter, cluster, rect, Color32::from_white_alpha(alpha));
        }
    }
}

/// Paints `cluster`'s picture filling `rect`, drawing it first if this context hasn't yet. This is
/// for an emoji as an image (an `<img>` of one), not a glyph in a run of text.
pub fn picture(painter: &Painter, cluster: &str, rect: Rect) {
    draw(painter, cluster, rect, Color32::WHITE);
}

/// [`picture`], multiplied by `tint`.
fn draw(painter: &Painter, cluster: &str, rect: Rect, tint: Color32) {
    let ctx = painter.ctx();
    let height = (rect.height() * painter.pixels_per_point())
        .round()
        .max(1.0) as u32;
    let pictures = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Pictures>(pictures_id())
            .clone()
    });
    let mut pictures = pictures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let texture = pictures
        .entry((cluster.to_owned(), height))
        .or_insert_with(|| {
            // A millisecond or two, once per emoji and size.
            let picture = fastframe_emoji::get().render(cluster, height)?;
            let image = ColorImage::from_rgba_premultiplied(picture.size, &picture.rgba);
            Some(ctx.load_texture(
                format!("emoji-{cluster}-{height}"),
                image,
                TextureOptions::LINEAR,
            ))
        });
    if let Some(texture) = texture {
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            tint,
        );
    }
}
