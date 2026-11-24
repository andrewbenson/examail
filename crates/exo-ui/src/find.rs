//! Find in page (⌘F): the text on screen, searched the way Chromium's find-in-page searches a page.
//!
//! While a search is on ([`search`]), each run of text [`crate::paint_text`] paints is searched
//! as it's painted, and its matches are highlighted under it as Chromium highlights them: yellow,
//! the current match orange, the text in them black. Screens register nothing for that. What a
//! screen paints outside the page (the shell's tab strip) it paints inside [`unsearchable`]. What
//! it paints some other way (a message body) adds its matches at that point with [`add_matches`],
//! or says with [`pending`] that they're still to come.
//!
//! While [`searching`], a screen that scrolls also paints what's out of view, as a page keeps it
//! in its DOM, inside a [`Scope`]. It scrolls by what [`reveal`] says when a search moves to a
//! match out of view, as Chromium scrolls it into view.
//!
//! Matches count in the order they're painted, which is the page's order, except where a screen
//! paints out of it. There, a [`Slot`] keeps a place for what's painted later. Each frame's
//! matches are settled at the start of the next ([`State::roll`]), so the count and the current
//! match lag the screen by a frame.

use std::{ops::Range, sync::Arc};

use egui::{
    Color32, Context, Galley, Id, Painter, Pos2, Rangef, Rect, Vec2, mutex::Mutex, pos2, vec2,
};

use crate::TextStyle;

/// A match's highlight (Chromium's `PlatformTextSearchHighlightColor`, inactive).
pub const MATCH: Color32 = Color32::from_rgb(255, 255, 0);
/// The current match's highlight (active).
pub const CURRENT: Color32 = Color32::from_rgb(255, 150, 50);
/// The text in a highlight (`PlatformTextSearchColor`).
pub const MATCH_TEXT: Color32 = Color32::BLACK;

/// Searches for `query` from now on, or stops searching (`None`, or empty). A new query starts
/// from the current match, keeping it if it still matches there (Chromium's
/// `kStartInSelection`).
pub fn search(ctx: &Context, query: Option<&str>) {
    let raw = query.filter(|query| !query.is_empty());
    let query = raw.map(fold_query);
    let changed = with(ctx, |state| {
        if state.query == query {
            return false;
        }
        if query.is_none() {
            state.stop();
        } else {
            state.query = query;
            state.raw = raw.map(str::to_owned);
            state.request = Some(Request::Restart);
            // What's been painted this frame was searched for the old query.
            state.stale = true;
        }
        true
    });
    if changed {
        ctx.request_repaint();
    }
}

/// Moves to the next match (or the previous), wrapping around, and scrolls it into view. With
/// none current, it moves to the first (or the last).
pub fn step(ctx: &Context, forward: bool) {
    with(ctx, |state| {
        if state.query.is_some() {
            state.request = Some(Request::Step(forward));
        }
    });
    ctx.request_repaint();
}

/// Whether a search is on, in which case screens that scroll paint what's out of view.
pub fn searching(ctx: &Context) -> bool {
    with(ctx, |state| state.query.is_some())
}

/// What's searched for, if anything, so text Exo doesn't paint itself can be searched with
/// [`matches`].
pub fn query(ctx: &Context) -> Option<String> {
    with(ctx, |state| state.raw.clone())
}

/// The current match's place among the matches (from 1, or 0 with none) and their number, once
/// the search has looked at a frame. This is what Chromium's find in page reports.
pub fn count(ctx: &Context) -> Option<(usize, usize)> {
    with(ctx, |state| state.query.as_ref().and(state.count))
}

/// Runs `paint` without searching what it paints, for text outside the page (the tab strip).
pub fn unsearchable<R>(ctx: &Context, paint: impl FnOnce() -> R) -> R {
    with(ctx, |state| state.unsearchable += 1);
    let painted = paint();
    with(ctx, |state| {
        state.unsearchable = state.unsearchable.saturating_sub(1)
    });
    painted
}

/// A place in the page's order, kept for what's painted later (a header painted over what's
/// under it). See [`in_slot`].
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct Slot(u32);

/// Keeps the place in the page's order at this point of the frame.
pub fn slot(ctx: &Context) -> Slot {
    with(ctx, |state| {
        let slot = state.next_section + 1;
        state.next_section += 2;
        state.section = state.next_section;
        Slot(slot)
    })
}

/// Runs `paint` with what it paints counted at `slot`.
pub fn in_slot<R>(ctx: &Context, slot: Slot, paint: impl FnOnce() -> R) -> R {
    let outside = with(ctx, |state| std::mem::replace(&mut state.section, slot.0));
    let painted = paint();
    with(ctx, |state| state.section = outside);
    painted
}

/// Adds matches Exo doesn't paint as text (in a message body, which `source` names) at this
/// point in the page's order. `boxes` are the matches on screen, in their own order. Returns which
/// of them is the current match.
pub fn add_matches(ctx: &Context, source: Id, boxes: &[Rect]) -> Option<usize> {
    with(ctx, |state| {
        if state.query.is_none() || state.unsearchable > 0 {
            return None;
        }
        let order = state.next_order();
        let scope = state.scopes.last().copied();
        let mut current = None;
        for (index, rect) in boxes.iter().enumerate() {
            let key = Key {
                scope: scope.map(|scope| scope.id),
                source: source.value(),
                index,
            };
            if state.current_key == Some(key) {
                current = Some(index);
            }
            state.found.push(Found {
                key,
                rect: *rect,
                scope,
                order: (order.0, order.1, index),
            });
        }
        current
    })
}

/// Says that what's painted at this point has matches still to come (a message body still being
/// searched). A new search waits for them before choosing its first match, so it starts where
/// the page's first match is rather than at whichever arrives first.
pub fn pending(ctx: &Context) {
    with(ctx, |state| {
        state.pending |= state.query.is_some() && state.unsearchable == 0
    });
}

/// How far (right and down, in points) to scroll `scope` before painting it, to bring the
/// current match into view the way Chromium does (`ScrollAlignment::CenterIfNeeded` both ways):
/// along each axis, not at all if it's in view, to the nearest edge if it's partly out, centred
/// if it's out. `Some` once, when a search has just moved to a match in it.
pub fn reveal(ctx: &Context, scope: Id) -> Option<Vec2> {
    let reveal = with(ctx, |state| {
        state.reveal.take_if(|reveal| reveal.scope == scope)
    })?;
    let into_view = |rect: Rangef, viewport: Rangef| {
        if rect.min >= viewport.min && rect.max <= viewport.max {
            0.0
        } else if rect.max <= viewport.min || rect.min >= viewport.max {
            rect.center() - viewport.center()
        } else if rect.min < viewport.min {
            rect.min - viewport.min
        } else {
            rect.max - viewport.max
        }
    };
    let (rect, viewport) = (reveal.rect, reveal.viewport);
    let delta = vec2(
        into_view(rect.x_range(), viewport.x_range()),
        into_view(rect.y_range(), viewport.y_range()),
    );
    (delta != Vec2::ZERO).then_some(delta)
}

/// What a scrolling screen paints, while it paints it. Matches in it are the screen's to scroll
/// to, and order by where they are in it. Closes when dropped.
#[must_use]
pub struct Scope {
    ctx: Context,
    open: bool,
}

impl Scope {
    /// Opens scope `id`, whose `viewport` on screen shows its content scrolled `offset` points
    /// down.
    pub fn begin(ctx: &Context, id: Id, viewport: Rect, offset: f32) -> Self {
        let open = with(ctx, |state| {
            let open = state.query.is_some();
            if open {
                state.scopes.push(ScopeFrame {
                    id,
                    viewport,
                    offset,
                });
            }
            open
        });
        Self {
            ctx: ctx.clone(),
            open,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.open {
            with(&self.ctx, |state| state.scopes.pop());
        }
    }
}

/// The byte ranges of `text` that match `query`, left to right without overlapping, compared
/// as Chromium's find compares text: ignoring case, diacritics and compatibility forms (`ﬁ` is
/// `fi`), reading `ß` as `ss`, `æ` as `ae`, `ø` as `o` and the like, a no-break space as a space.
/// A match starts and ends on whole characters.
pub fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = fold_query(query);
    found_in(text, &query)
        .into_iter()
        .map(|range| range.bytes)
        .collect()
}

/// A match of a folded query in a text.
struct Matched {
    bytes: Range<usize>,
}

fn fold_query(query: &str) -> Vec<char> {
    fold(query).into_iter().map(|folded| folded.ch).collect()
}

fn found_in(text: &str, query: &[char]) -> Vec<Matched> {
    if query.is_empty() {
        return Vec::new();
    }
    let folded = fold(text);
    let mut found = Vec::new();
    let mut at = 0;
    // Whether a match may start (or end) between `folded[at - 1]` and `folded[at]`: not inside
    // a character that folds to several.
    let boundary =
        |at: usize| at == 0 || at == folded.len() || folded[at - 1].char != folded[at].char;
    while at + query.len() <= folded.len() {
        if boundary(at)
            && boundary(at + query.len())
            && folded[at..at + query.len()]
                .iter()
                .map(|folded| folded.ch)
                .eq(query.iter().copied())
        {
            let (first, last) = (&folded[at], &folded[at + query.len() - 1]);
            found.push(Matched {
                bytes: first.bytes.start..last.bytes.end,
            });
            at += query.len();
        } else {
            at += 1;
        }
    }
    found
}

/// One character of folded text, and the character of the text it comes from.
struct Folded {
    ch: char,
    /// That character's index and bytes.
    char: usize,
    bytes: Range<usize>,
}

/// `text` without case, diacritics or compatibility forms (each character decomposed for
/// compatibility, its combining marks dropped, lowercased), the letters no decomposition gives
/// read as ICU's root collation reads them at primary strength ([`primary`]), and a no-break space
/// as a space.
fn fold(text: &str) -> Vec<Folded> {
    let mut folded = Vec::with_capacity(text.len());
    for (index, (at, ch)) in text.char_indices().enumerate() {
        let bytes = at..at + ch.len_utf8();
        let mut push = |ch: char| {
            folded.push(Folded {
                ch,
                char: index,
                bytes: bytes.clone(),
            })
        };
        match ch {
            '\u{a0}' => push(' '),
            // A letter of its own, not `и` with a breve.
            'й' | 'Й' => push('й'),
            ch => unicode_normalization::char::decompose_compatible(ch, |part| {
                if unicode_normalization::char::is_combining_mark(part) {
                    return;
                }
                for lower in part.to_lowercase() {
                    match primary(lower) {
                        Some(letters) => letters.chars().for_each(&mut push),
                        None => push(lower),
                    }
                }
            }),
        }
    }
    folded
}

/// What ICU's root collation, which Chromium's find compares with, reads a lowercase letter as at
/// primary strength when no decomposition says so: `ß` is `ss`, `æ` is `ae`, a stroked letter is
/// its letter, a final sigma is a sigma, and a middle dot (Catalan's `l·l`) is nothing.
fn primary(ch: char) -> Option<&'static str> {
    Some(match ch {
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'ø' => "o",
        'đ' | 'ð' => "d",
        'ħ' => "h",
        'ł' => "l",
        'ς' => "σ",
        '·' => "",
        _ => return None,
    })
}

/// Searches a run of text [`crate::paint_text`] is about to paint at `origin`, and paints its
/// matches' highlights under it, the text in them black.
pub(crate) fn text(painter: &Painter, origin: Pos2, galley: &mut Arc<Galley>, style: &TextStyle) {
    if !painter.is_visible()
        || galley
            .job
            .sections
            .iter()
            .all(|section| section.format.color.a() == 0)
    {
        return;
    }
    let found = register(painter, origin, galley, style, false);
    if found.is_empty() {
        return;
    }
    let galley = Arc::make_mut(galley);
    for (pieces, current) in found {
        for (row, glyphs, rect) in pieces {
            if rect.width() > 0.0 {
                painter.rect_filled(rect, 0.0, if current { CURRENT } else { MATCH });
            }
            recolor(&mut galley.rows[row], glyphs, MATCH_TEXT);
        }
    }
}

/// Searches what's typed in a field, which egui's text edit paints: `galley`, painted at
/// `origin`. Returns the matches' highlights, for the field to paint under its text. Its layout
/// makes the text in them black ([`darken_matches`]).
pub(crate) fn field(
    painter: &Painter,
    origin: Pos2,
    galley: &Galley,
    style: &TextStyle,
) -> Vec<egui::Shape> {
    if !painter.is_visible() {
        return Vec::new();
    }
    register(painter, origin, galley, style, true)
        .into_iter()
        .flat_map(|(pieces, current)| {
            pieces
                .into_iter()
                .filter(|(_, _, rect)| rect.width() > 0.0)
                .map(move |(_, _, rect)| {
                    egui::Shape::rect_filled(rect, 0.0, if current { CURRENT } else { MATCH })
                })
        })
        .collect()
}

/// Paints the matches in a field's freshly laid out `galley` black, as [`field`] highlights them.
pub(crate) fn darken_matches(ctx: &Context, galley: &mut Arc<Galley>) {
    let Some(query) = with(ctx, |state| {
        state.query.clone().filter(|_| state.unsearchable == 0)
    }) else {
        return;
    };
    let found = found_in(galley.text(), &query);
    if found.is_empty() {
        return;
    }
    let pieces: Vec<_> = found
        .iter()
        .flat_map(|matched| pieces(galley, &matched.bytes, Pos2::ZERO, 0.0, 0.0, &|x| x))
        .collect();
    let galley = Arc::make_mut(galley);
    for (row, glyphs, _) in pieces {
        recolor(&mut galley.rows[row], glyphs, MATCH_TEXT);
    }
}

/// A match's highlight, a box per row with the row and the glyphs in it.
type Pieces = Vec<(usize, Range<usize>, Rect)>;

/// Searches `galley`, painted at `origin`, counting its matches in the page's order. Returns
/// each match's highlight and whether it's the current match. A field's text (`in_field`) is
/// told apart from the same text painted in the same place (the search bar's underlay).
fn register(
    painter: &Painter,
    origin: Pos2,
    galley: &Galley,
    style: &TextStyle,
    in_field: bool,
) -> Vec<(Pieces, bool)> {
    let ctx = painter.ctx();
    let Some((query, order, scope)) = with(ctx, |state| {
        let query = state.query.clone().filter(|_| state.unsearchable == 0)?;
        Some((query, state.next_order(), state.scopes.last().copied()))
    }) else {
        return Vec::new();
    };
    let found = found_in(galley.text(), &query);
    if found.is_empty() {
        return Vec::new();
    }
    let metrics = crate::metrics(ctx, style.font.face().0);
    let ascent = (metrics.ascent * style.size).round();
    let descent = (metrics.descent * style.size).round();
    let ppp = painter.pixels_per_point();
    let snap = |value: f32| (value * ppp).round() / ppp;
    // Where the run is in what scrolls, which doesn't change as it scrolls.
    let content = origin + egui::vec2(0.0, scope.map_or(0.0, |scope| scope.offset));
    let source = Id::new((
        galley.text(),
        content.x.round() as i32,
        content.y.round() as i32,
        in_field,
    ))
    .value();
    let pieces: Vec<Pieces> = found
        .iter()
        .map(|matched| pieces(galley, &matched.bytes, origin, ascent, descent, &snap))
        .collect();
    with(ctx, |state| {
        pieces
            .into_iter()
            .enumerate()
            .map(|(index, pieces)| {
                let key = Key {
                    scope: scope.map(|scope| scope.id),
                    source,
                    index,
                };
                let rect = pieces
                    .iter()
                    .map(|(_, _, rect)| *rect)
                    .reduce(Rect::union)
                    .unwrap_or_else(|| Rect::from_min_size(origin, egui::Vec2::ZERO));
                state.found.push(Found {
                    key,
                    rect,
                    scope,
                    order: (order.0, order.1, index),
                });
                (pieces, state.current_key == Some(key))
            })
            .collect()
    })
}

/// The highlight of UTF-8 byte range `bytes` of `galley` (painted at `origin`), as a box per row
/// over the font's content area (`ascent` above the baseline, `descent` below, as Chromium bounds
/// a text fragment), with the row and the glyphs in it. A match in text cut off by an ellipsis is
/// a box as wide as nothing where it was cut.
///
/// Matching operates on `Glyph::cluster` UTF-8 byte offsets and visual glyph clusters rather than
/// raw `char`-to-glyph index equality so OpenType ligatures (`fi`, `fl`, `ffi`) and multi-codepoint
/// emoji (`👨‍👩‍👧‍👦`, `👍🏽`, `1️⃣`) recolor and highlight accurately.
fn pieces(
    galley: &Galley,
    bytes: &Range<usize>,
    origin: Pos2,
    ascent: f32,
    descent: f32,
    snap: &impl Fn(f32) -> f32,
) -> Pieces {
    let text = galley.text();
    let mut pieces = Vec::new();
    let mut row_byte_start = 0;
    let last = galley.rows.len().saturating_sub(1);

    for (index, placed) in galley.rows.iter().enumerate() {
        let elided_here = galley.elided
            && index == last
            && galley.job.wrap.overflow_character.is_some()
            && !placed.glyphs.is_empty();
        let shown = &placed.glyphs[..placed.glyphs.len() - usize::from(elided_here)];
        if let Some(first_glyph) = shown.first() {
            row_byte_start = (first_glyph.cluster as usize).max(row_byte_start);
        }

        let row_byte_end = if index < last {
            if placed.ends_with_newline {
                text[row_byte_start..]
                    .find('\n')
                    .map_or(text.len(), |rel| row_byte_start + rel)
            } else {
                galley.rows[index + 1]
                    .glyphs
                    .first()
                    .map_or(text.len(), |next| next.cluster as usize)
            }
        } else if galley.elided {
            if shown.is_empty() {
                row_byte_start
            } else {
                let last_cluster = shown.last().map_or(row_byte_start, |g| g.cluster as usize);
                let ellipsis_cluster = if elided_here {
                    placed.glyphs.last().map_or(0, |g| g.cluster as usize)
                } else {
                    0
                };
                if ellipsis_cluster > last_cluster {
                    ellipsis_cluster.min(text.len())
                } else {
                    cluster_text_end(text, last_cluster)
                }
            }
        } else {
            text.len()
        };

        let (from_byte, to_byte) = (bytes.start.max(row_byte_start), bytes.end.min(row_byte_end));
        let baseline = |glyph_idx: usize| shown.get(glyph_idx).map_or(0.0, |glyph| glyph.pos.y);

        if from_byte < to_byte && !shown.is_empty() {
            let mut glyph_range: Option<Range<usize>> = None;
            let mut x_min = f32::INFINITY;
            let mut x_max = f32::NEG_INFINITY;

            let mut g_start = 0;
            while g_start < shown.len() {
                let mut g_end = g_start + 1;
                while g_end < shown.len() {
                    let prev = &shown[g_end - 1];
                    let curr = &shown[g_end];
                    let same_cluster = curr.cluster == prev.cluster;
                    let ligature_continuation = shown[g_start].advance_width > 0.0
                        && curr.advance_width == 0.0
                        && curr.uv_rect.is_nothing()
                        && !matches!(curr.chr, '\u{200B}' | '\u{200C}' | '\u{FEFF}');
                    if same_cluster || ligature_continuation {
                        g_end += 1;
                    } else {
                        break;
                    }
                }

                let c_start = (shown[g_start].cluster as usize).clamp(row_byte_start, row_byte_end);
                let c_end = if g_end < shown.len() {
                    (shown[g_end].cluster as usize).clamp(c_start, row_byte_end)
                } else {
                    row_byte_end.max(c_start)
                };

                if c_start < to_byte && from_byte < c_end {
                    glyph_range = Some(match glyph_range {
                        Some(prev) => prev.start.min(g_start)..prev.end.max(g_end),
                        None => g_start..g_end,
                    });
                    let cluster_left = shown[g_start].pos.x;
                    let cluster_width: f32 =
                        shown[g_start..g_end].iter().map(|g| g.advance_width).sum();
                    let (sub_left, sub_right) = if from_byte <= c_start && to_byte >= c_end {
                        (cluster_left, cluster_left + cluster_width)
                    } else {
                        subcluster_x(
                            &text[c_start..c_end],
                            from_byte.saturating_sub(c_start)..to_byte.min(c_end) - c_start,
                            cluster_left,
                            cluster_width,
                        )
                    };
                    x_min = x_min.min(sub_left);
                    x_max = x_max.max(sub_right);
                }

                g_start = g_end;
            }

            if let Some(glyphs) = glyph_range
                && x_min.is_finite()
                && x_max.is_finite()
            {
                let first_glyph = glyphs.start;
                let y = origin.y + placed.pos.y + baseline(first_glyph);
                let rect = Rect::from_min_max(
                    pos2(snap(origin.x + placed.pos.x + x_min), snap(y - ascent)),
                    pos2(snap(origin.x + placed.pos.x + x_max), snap(y + descent)),
                );
                pieces.push((index, glyphs, rect));
            }
        } else if index == last && galley.elided && bytes.start >= row_byte_end && pieces.is_empty()
        {
            let end_x = shown.last().map_or(0.0, |g| g.pos.x + g.advance_width);
            let x = origin.x + placed.pos.x + end_x;
            let y = origin.y + placed.pos.y + baseline(0);
            pieces.push((
                index,
                0..0,
                Rect::from_min_max(pos2(x, y - ascent), pos2(x, y + descent)),
            ));
        }

        row_byte_start = row_byte_end + usize::from(placed.ends_with_newline);
    }
    pieces
}

fn cluster_text_end(text: &str, cluster_start: usize) -> usize {
    let Some(tail) = text.get(cluster_start..) else {
        return text.len();
    };
    if !tail.is_ascii()
        && let Some(fastframe_emoji::Piece::Emoji(cluster)) =
            fastframe_emoji::pieces(tail).into_iter().next()
    {
        return cluster_start + cluster.len();
    }
    let mut chars = tail.char_indices();
    let Some((_, first)) = chars.next() else {
        return cluster_start;
    };
    let mut len = first.len_utf8();
    for (rel, ch) in chars {
        if unicode_normalization::char::is_combining_mark(ch)
            || matches!(ch, '\u{FE00}'..='\u{FE0F}' | '\u{20E3}')
        {
            len = rel + ch.len_utf8();
        } else {
            break;
        }
    }
    cluster_start + len
}

fn subcluster_x(
    cluster_str: &str,
    sub_bytes: Range<usize>,
    cluster_left: f32,
    cluster_width: f32,
) -> (f32, f32) {
    let mut slots: Vec<(Range<usize>, usize)> = Vec::new();
    let mut total_slots = 0usize;
    for (rel, ch) in cluster_str.char_indices() {
        let range = rel..rel + ch.len_utf8();
        let is_attacher = unicode_normalization::char::is_combining_mark(ch)
            || matches!(ch, '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{20E3}');
        if is_attacher && total_slots > 0 {
            slots.push((range, total_slots - 1));
        } else {
            slots.push((range, total_slots));
            total_slots += 1;
        }
    }
    if total_slots <= 1 {
        return (cluster_left, cluster_left + cluster_width);
    }
    let mut min_slot = total_slots;
    let mut max_slot = 0usize;
    for (range, slot) in slots {
        if range.start < sub_bytes.end && sub_bytes.start < range.end {
            min_slot = min_slot.min(slot);
            max_slot = max_slot.max(slot + 1);
        }
    }
    if min_slot >= max_slot {
        return (cluster_left, cluster_left + cluster_width);
    }
    let step = cluster_width / total_slots as f32;
    (
        cluster_left + min_slot as f32 * step,
        cluster_left + max_slot as f32 * step,
    )
}

/// Paints glyphs `glyphs` of `placed` in `color`.
fn recolor(placed: &mut egui::epaint::text::PlacedRow, glyphs: Range<usize>, color: Color32) {
    if glyphs.is_empty() {
        return;
    }
    let row = Arc::make_mut(&mut placed.row);
    for index in glyphs {
        let glyph = &row.glyphs[index];
        // A glyph egui draws nothing for has no quad, and its `first_vertex` is the next glyph's.
        let first = glyph.first_vertex as usize;
        if glyph.uv_rect.is_nothing()
            || glyph.uv_rect.size.x <= 0.0
            || !row.visuals.glyph_vertex_range.contains(&first)
        {
            continue;
        }
        for vertex in &mut row.visuals.mesh.vertices[first..first + 4] {
            vertex.color = color;
        }
    }
}

/// What identifies a match from one frame to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    scope: Option<Id>,
    /// The run of text (its text and where it is) or the body it's in.
    source: u64,
    /// Which of its matches.
    index: usize,
}

/// A match painted this frame.
#[derive(Clone, Copy, Debug)]
struct Found {
    key: Key,
    rect: Rect,
    scope: Option<ScopeFrame>,
    /// Where it is in the page: its section ([`Slot`]), its run, its place in the run.
    order: (u32, u32, usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScopeFrame {
    id: Id,
    viewport: Rect,
    offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// A new query: the first match from the current one on.
    Restart,
    Step(bool),
}

/// A scroll to make, for the current match.
#[derive(Clone, Copy, Debug)]
struct Reveal {
    scope: Id,
    rect: Rect,
    viewport: Rect,
}

#[derive(Default)]
struct State {
    /// The search, folded, and as it was asked.
    query: Option<Vec<char>>,
    raw: Option<String>,
    /// The frame `found` is from.
    pass: u64,
    found: Vec<Found>,
    /// Whether this frame's matches are for an older query.
    stale: bool,
    /// Whether some of this frame's matches are still to come ([`pending`]).
    pending: bool,
    /// The last frame's matches, in order, and which is current.
    settled: Vec<Found>,
    current: Option<usize>,
    current_key: Option<Key>,
    count: Option<(usize, usize)>,
    request: Option<Request>,
    reveal: Option<Reveal>,
    scopes: Vec<ScopeFrame>,
    unsearchable: usize,
    /// The section text is painted in, the last one handed out, and the runs searched.
    section: u32,
    next_section: u32,
    runs: u32,
}

impl State {
    /// Stops searching, forgetting the matches.
    fn stop(&mut self) {
        (self.query, self.raw) = (None, None);
        self.found.clear();
        self.settled.clear();
        (self.current, self.current_key, self.count) = (None, None, None);
        (self.request, self.reveal, self.stale, self.pending) = (None, None, false, false);
    }

    /// Where the next run is in the page's order.
    fn next_order(&mut self) -> (u32, u32) {
        self.runs += 1;
        (self.section, self.runs)
    }

    /// Starts frame `pass`, settling the last frame's matches.
    fn roll(&mut self, pass: u64) -> bool {
        if pass == self.pass {
            return false;
        }
        self.pass = pass;
        let found = std::mem::take(&mut self.found);
        let stale = std::mem::take(&mut self.stale);
        let pending = std::mem::take(&mut self.pending);
        (self.section, self.next_section, self.runs) = (0, 0, 0);
        self.scopes.clear();
        self.unsearchable = 0;
        if self.query.is_none() {
            return false;
        }
        if stale {
            // This frame searches for the new query, and the next one settles it.
            return true;
        }
        if pending && self.request == Some(Request::Restart) {
            // A new query waits for all of the page's matches (what's still to come repaints).
            return false;
        }
        let before = (self.current_key, self.count);
        self.settle(found);
        before != (self.current_key, self.count)
    }

    fn settle(&mut self, mut found: Vec<Found>) {
        found.sort_by_key(|found| found.order);
        let total = found.len();
        let kept = self
            .current_key
            .and_then(|key| found.iter().position(|found| found.key == key));
        let held = kept.or_else(|| {
            self.current
                .map(|current| current.min(total.saturating_sub(1)))
        });
        let request = self.request.take();
        let current = if total == 0 {
            None
        } else {
            match request {
                Some(Request::Step(forward)) => Some(match held {
                    Some(at) if forward => (at + 1) % total,
                    Some(at) => (at + total - 1) % total,
                    None if forward => 0,
                    None => total - 1,
                }),
                Some(Request::Restart) => {
                    let from = self
                        .current
                        .and_then(|current| self.settled.get(current))
                        .map(|was| was.order);
                    Some(
                        from.and_then(|from| found.iter().position(|found| found.order >= from))
                            .unwrap_or(0),
                    )
                }
                None => held,
            }
        };
        self.current = current;
        self.current_key = current.map(|at| found[at].key);
        self.count = Some((current.map_or(0, |at| at + 1), total));
        if request.is_some()
            && let Some(at) = current
            && let Some(scope) = found[at].scope
        {
            self.reveal = Some(Reveal {
                scope: scope.id,
                rect: found[at].rect,
                viewport: scope.viewport,
            });
        }
        self.settled = found;
    }
}

/// Runs `f` on the search's state, starting a new frame's first.
fn with<R>(ctx: &Context, f: impl FnOnce(&mut State) -> R) -> R {
    let id = crate::id(ctx, "exo_ui::find");
    let state = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<State>>>(id)
            .clone()
    });
    let mut state = state.lock();
    if state.roll(ctx.cumulative_pass_nr()) {
        ctx.request_repaint();
    }
    f(&mut state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ignore_case_and_diacritics() {
        assert_eq!(
            matches("Café au lait, CAFE noir", "cafe"),
            vec![0..5, 15..19]
        );
        assert_eq!(matches("Résumé", "resume"), vec![0..8]);
        assert_eq!(matches("naïve", "NAÏVE"), vec![0..6]);
        assert_eq!(
            matches("a\u{a0}b", "a b"),
            vec![0..4],
            "a no-break space is a space"
        );
    }

    #[test]
    fn matches_read_letters_as_collation_does_and_compatibility_forms_as_their_letters() {
        assert_eq!(matches("Straße, STRASSE", "strasse"), vec![0..7, 9..16]);
        assert_eq!(matches("Strasse", "straße"), vec![0..7]);
        assert_eq!(
            matches("Straße", "stras"),
            Vec::<Range<usize>>::new(),
            "not half of a character"
        );
        assert_eq!(matches("ﬁnal", "fin"), vec![0..4], "a ligature");
        assert_eq!(matches("Encyclopædia", "encyclopaedia"), vec![0..13]);
        assert_eq!(matches("Søren Łukasz", "soren lukasz"), vec![0..14]);
        assert_eq!(
            matches("col·legi", "collegi"),
            vec![0..9],
            "a middle dot is nothing"
        );
        assert_eq!(
            matches("йод", "иод"),
            Vec::<Range<usize>>::new(),
            "`й` isn't `и` with a breve"
        );
        assert_eq!(matches("ＥＸＯ", "exo"), vec![0..9], "full-width letters");
    }

    #[test]
    fn matches_dont_overlap() {
        assert_eq!(matches("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(matches("abc", ""), Vec::<Range<usize>>::new());
        assert_eq!(matches("abc", "abcd"), Vec::<Range<usize>>::new());
    }

    fn found(order: u32, source: u64) -> Found {
        let key = Key {
            scope: None,
            source,
            index: 0,
        };
        Found {
            key,
            rect: Rect::from_min_size(pos2(0.0, order as f32 * 20.0), egui::vec2(10.0, 10.0)),
            scope: None,
            order: (0, order, 0),
        }
    }

    fn settled(state: &mut State, found: Vec<Found>) -> Option<(usize, usize)> {
        state.settle(found);
        state.count
    }

    #[test]
    fn a_search_starts_at_the_first_match_and_steps_around() {
        let mut state = State {
            query: Some(vec!['a']),
            request: Some(Request::Restart),
            ..State::default()
        };
        let page = || vec![found(1, 1), found(2, 2), found(3, 3)];
        assert_eq!(settled(&mut state, page()), Some((1, 3)));
        state.request = Some(Request::Step(true));
        assert_eq!(settled(&mut state, page()), Some((2, 3)));
        state.request = Some(Request::Step(false));
        assert_eq!(settled(&mut state, page()), Some((1, 3)));
        state.request = Some(Request::Step(false));
        assert_eq!(
            settled(&mut state, page()),
            Some((3, 3)),
            "back from the first wraps to the last"
        );
        state.request = Some(Request::Step(true));
        assert_eq!(
            settled(&mut state, page()),
            Some((1, 3)),
            "on from the last wraps to the first"
        );
    }

    #[test]
    fn the_current_match_follows_its_text_as_others_come_and_go() {
        let mut state = State {
            query: Some(vec!['a']),
            request: Some(Request::Step(true)),
            ..State::default()
        };
        settled(&mut state, vec![found(1, 1), found(2, 2)]);
        state.request = Some(Request::Step(true));
        assert_eq!(
            settled(&mut state, vec![found(1, 1), found(2, 2)]),
            Some((2, 2))
        );
        // A match painted before it appears (a row scrolled within reach).
        assert_eq!(
            settled(&mut state, vec![found(0, 9), found(1, 1), found(2, 2)]),
            Some((3, 3))
        );
        assert!(state.reveal.is_none(), "nothing moved the search");
    }

    #[test]
    fn a_new_query_keeps_to_the_current_match_or_moves_on_from_it() {
        let mut state = State {
            query: Some(vec!['a']),
            request: Some(Request::Restart),
            ..State::default()
        };
        settled(&mut state, vec![found(1, 1), found(2, 2), found(3, 3)]);
        state.request = Some(Request::Step(true));
        assert_eq!(
            settled(&mut state, vec![found(1, 1), found(2, 2), found(3, 3)]),
            Some((2, 3))
        );
        // The longer query no longer matches the second run, but does the first and third.
        state.request = Some(Request::Restart);
        assert_eq!(
            settled(&mut state, vec![found(1, 11), found(3, 13)]),
            Some((2, 2))
        );
        state.request = Some(Request::Restart);
        assert_eq!(
            settled(&mut state, vec![found(1, 21)]),
            Some((1, 1)),
            "past the last, the first"
        );
        state.request = Some(Request::Restart);
        assert_eq!(settled(&mut state, vec![]), Some((0, 0)));
    }

    #[test]
    fn a_new_query_waits_for_matches_still_to_come() {
        let mut state = State {
            query: Some(vec!['a']),
            request: Some(Request::Restart),
            ..State::default()
        };
        // A body above the second run is still being searched.
        (state.found, state.pending) = (vec![found(2, 2)], true);
        state.roll(1);
        assert_eq!(
            (state.count, state.request),
            (None, Some(Request::Restart)),
            "not settled yet"
        );
        state.found = vec![found(1, 1), found(2, 2)];
        state.roll(2);
        assert_eq!(state.count, Some((1, 2)), "from the page's first match");
    }

    #[test]
    fn painted_text_is_searched_and_highlighted_under_it() {
        use egui::{Shape, epaint::TextShape};
        let ctx = Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let style = TextStyle::new(crate::Font::new(crate::Family::Sans, 400), 14.0, 20.0);
        let frame = |ctx: &Context| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                style.paint_line(
                    ui.painter(),
                    pos2(10.0, 10.0),
                    "One agenda, two agendas",
                    Color32::WHITE,
                    None,
                );
                unsearchable(ui.ctx(), || {
                    style.paint_line(
                        ui.painter(),
                        pos2(10.0, 40.0),
                        "agenda",
                        Color32::WHITE,
                        None,
                    )
                });
            });
            let shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            shapes
                .into_iter()
                .map(|clipped| clipped.shape)
                .collect::<Vec<Shape>>()
        };
        search(&ctx, Some("AGENDA"));
        // One frame searched for the new query and another settled it, and the next paints it.
        frame(&ctx);
        frame(&ctx);
        let shapes = frame(&ctx);
        assert_eq!(
            count(&ctx),
            Some((1, 2)),
            "the unsearchable run isn't counted"
        );
        let fills: Vec<Color32> = shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Rect(rect) = shape {
                    Some(rect.fill)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(fills, [CURRENT, MATCH], "the first is current");
        let text: Vec<&TextShape> = shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Text(text) = shape {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();
        let colors: Vec<Color32> = text[0].galley.rows[0]
            .visuals
            .mesh
            .vertices
            .iter()
            .map(|vertex| vertex.color)
            .collect();
        assert!(
            colors.contains(&MATCH_TEXT) && colors.contains(&Color32::WHITE),
            "the matches' glyphs are black, the rest as painted"
        );
        step(&ctx, true);
        frame(&ctx);
        let shapes = frame(&ctx);
        let fills: Vec<Color32> = shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Rect(rect) = shape {
                    Some(rect.fill)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            (fills.as_slice(), count(&ctx)),
            ([MATCH, CURRENT].as_slice(), Some((2, 2)))
        );
        search(&ctx, None);
        assert!(
            frame(&ctx)
                .iter()
                .all(|shape| !matches!(shape, Shape::Rect(_))),
            "no search, no highlights"
        );
    }

    #[test]
    fn reveal_scrolls_as_chromium_scrolls_a_match_into_view() {
        let ctx = Context::default();
        let viewport = Rect::from_min_max(pos2(0.0, 100.0), pos2(100.0, 500.0));
        let scope = Id::new("list");
        let reveal = |left: f32, top: f32| {
            with(&ctx, |state| {
                state.reveal = Some(Reveal {
                    scope,
                    rect: Rect::from_min_size(pos2(left, top), vec2(10.0, 20.0)),
                    viewport,
                })
            });
            super::reveal(&ctx, scope)
        };
        assert_eq!(reveal(0.0, 200.0), None, "in view");
        assert_eq!(
            reveal(0.0, 490.0),
            Some(vec2(0.0, 10.0)),
            "partly below: to the bottom edge"
        );
        assert_eq!(
            reveal(0.0, 90.0),
            Some(vec2(0.0, -10.0)),
            "partly above: to the top edge"
        );
        assert_eq!(
            reveal(0.0, 1000.0),
            Some(vec2(0.0, 1010.0 - 300.0)),
            "out of view: centred"
        );
        assert_eq!(
            reveal(95.0, 200.0),
            Some(vec2(5.0, 0.0)),
            "partly right: to the right edge"
        );
        assert_eq!(
            reveal(400.0, 1000.0),
            Some(vec2(405.0 - 50.0, 1010.0 - 300.0)),
            "out of view both ways"
        );
        assert_eq!(super::reveal(&ctx, scope), None, "once");
    }

    #[test]
    fn ligatures_and_multi_codepoint_emoji_highlight_and_recolor_accurately() {
        use egui::{Shape, epaint::TextShape};
        let ctx = Context::default();
        crate::install(&ctx).expect("the system fonts load");
        // Keep ligatures enabled so `"final"` shapes `"fi"` into one ligature glyph.
        let style = TextStyle::new(crate::Font::new(crate::Family::Sans, 400), 16.0, 24.0);

        let run_query = |line_text: &str, needle: &str| {
            search(&ctx, Some(needle));
            let mut last_shapes = Vec::new();
            for _ in 0..3 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    style.paint_line(
                        ui.painter(),
                        pos2(10.0, 10.0),
                        line_text,
                        Color32::WHITE,
                        None,
                    );
                });
                last_shapes = std::mem::take(&mut output.shapes)
                    .into_iter()
                    .map(|clipped| clipped.shape)
                    .collect();
                output.drop_without_applying_deltas();
            }
            last_shapes
        };

        // Partial ligature match `"in"` inside `"final"`: highlight box must have wider width than
        // `'n'` alone and must recolor the `"fi"` ligature quad to `MATCH_TEXT`.
        let shapes_in = run_query("final", "in");
        let rect_in = shapes_in
            .iter()
            .find_map(|shape| match shape {
                Shape::Rect(rect) => Some(rect.rect),
                _ => None,
            })
            .expect("highlight rect for 'in' in 'final'");
        let shapes_n = run_query("final", "n");
        let rect_n = shapes_n
            .iter()
            .find_map(|shape| match shape {
                Shape::Rect(rect) => Some(rect.rect),
                _ => None,
            })
            .expect("highlight rect for 'n' in 'final'");
        assert!(
            rect_in.left() < rect_n.left() - 1.0 && (rect_in.right() - rect_n.right()).abs() < 0.5,
            "matching 'in' in 'final' includes 'i' inside the 'fi' ligature ({rect_in:?} vs {rect_n:?})"
        );
        let text_shape = shapes_in
            .iter()
            .find_map(|shape| match shape {
                Shape::Text(text) => Some(text),
                _ => None,
            })
            .expect("text shape");
        assert_eq!(
            text_shape.galley.rows[0].visuals.mesh.vertices[0].color, MATCH_TEXT,
            "the 'fi' ligature quad at vertex 0 is recolored to MATCH_TEXT"
        );

        // Multi-codepoint ZWJ family (`👨‍👩‍👧‍👦`, 7 chars) and skin-tone emoji (`👍🏽`, 2 chars)
        // followed by `"final"` must neither shift subsequent highlights nor collapse emoji boxes.
        let emoji_line = "👨‍👩‍👧‍👦 👍🏽 final";
        let shapes_family = run_query(emoji_line, "👨‍👩‍👧‍👦");
        let rect_family = shapes_family
            .iter()
            .find_map(|shape| match shape {
                Shape::Rect(rect) => Some(rect.rect),
                _ => None,
            })
            .expect("highlight rect for family emoji");
        assert!(
            (rect_family.width() - 20.0).abs() < 1.0,
            "family emoji highlight spans its 20px cell, got {}",
            rect_family.width()
        );

        let shapes_after = run_query(emoji_line, "final");
        let rect_after = shapes_after
            .iter()
            .find_map(|shape| match shape {
                Shape::Rect(rect) => Some(rect.rect),
                _ => None,
            })
            .expect("highlight rect for 'final' after multi-codepoint emoji");
        let text_after: &TextShape = shapes_after
            .iter()
            .find_map(|shape| match shape {
                Shape::Text(text) => Some(text),
                _ => None,
            })
            .expect("text shape");
        let row = &text_after.galley.rows[0];
        let expected_left = 10.0
            + row
                .glyphs
                .iter()
                .find(|g| g.chr == 'f')
                .expect("'f' glyph")
                .pos
                .x;
        let expected_right = 10.0 + row.size.x;
        assert!(
            (rect_after.left() - expected_left).abs() <= 1.0
                && (rect_after.right() - expected_right).abs() <= 1.0,
            "highlight after multi-codepoint emoji aligns with 'final' ({rect_after:?}, expected {expected_left}..{expected_right})"
        );
        search(&ctx, None);
    }
}
