//! The contact list: the list of contacts under a recipient field or a contact picker, each row an
//! avatar, a name and an address with the typed text darker in each, the highlighted row filled.
//! Compose's From list is drawn in its box too.

use std::ops::Range;

use egui::{Color32, CursorIcon, Id, Painter, Pos2, Rect, Sense, Vec2, pos2, vec2};

use crate::{
    AnchoredCard, KeyTooltip, Palette, Placement, TextStyle, Theme, paint_job, pixel_snap,
};

/// A row's remove button: a 42px cell at the row's right, its `button` 32px tall and padded 16px a
/// side around a 10px cross.
const REMOVE_WIDTH: f32 = 42.0;
const REMOVE_HEIGHT: f32 = 32.0;
const REMOVE_INSET: f32 = 16.0;
const REMOVE_ICON: f32 = 10.0;

/// The list's colors. Lists forced into Snow (the contact pickers') keep Snow's in Carbon.
#[derive(Clone, Copy, Debug)]
pub struct ContactListColors {
    /// The list's background.
    pub background: Color32,
    /// The highlighted row.
    pub selected: Color32,
    /// A name, and the typed text in it.
    pub name: (Color32, Color32),
    /// An address, and the typed text in it.
    pub email: (Color32, Color32),
    /// A row that can't be picked.
    pub disabled: Color32,
}

impl ContactListColors {
    /// The list's colors in `palette`'s theme, or always Snow's when `force_snow`.
    pub fn new(palette: &Palette, force_snow: bool) -> Self {
        // `hsl(220, 45%, 97%)`.
        let selected = Color32::from_rgb(0xf4, 0xf6, 0xfb);
        let black = |opacity: f32| Color32::from_black_alpha((opacity * 255.0).round() as u8);
        if force_snow || palette.theme == Theme::Snow {
            // A list forced into Snow has Snow's colors in Carbon too.
            return Self {
                background: Color32::WHITE,
                selected,
                name: (black(0.5), black(0.85)),
                email: (
                    black(0.45),
                    if force_snow { black(0.85) } else { black(0.75) },
                ),
                disabled: black(0.4),
            };
        }
        Self {
            background: palette.background_base_default,
            selected: Color32::from_rgb(0x46, 0x4b, 0x53),
            name: (palette.ink(0.5, 0.5), Color32::WHITE),
            email: (palette.ink(0.45, 0.45), palette.ink(0.75, 0.75)),
            disabled: palette.ink(0.4, 0.4),
        }
    }
}

/// The list's box: the background, 2px corners, and
/// `0 2px 12px 0 rgba(0,0,0,.05), 0 2px 4px 0 rgba(0,0,0,.1)`. Returns it pixel-snapped.
pub fn paint_contact_list_box(painter: &Painter, rect: Rect, background: Color32) -> Rect {
    AnchoredCard::menu(background, Color32::TRANSPARENT).paint(painter, rect)
}

/// Lowercases `text` while recording `(lower_byte, original_byte)` at every character boundary,
/// so matches on the lowercased string map back to exact UTF-8 boundaries in `text` even when
/// characters expand (`'İ'` 2 → 3 bytes) or shrink (`'ẞ'` 3 → 2 bytes).
fn lowercased_with_boundaries(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut lower = String::with_capacity(text.len());
    let mut boundaries = Vec::with_capacity(text.len() + 1);
    for (orig_idx, ch) in text.char_indices() {
        boundaries.push((lower.len(), orig_idx));
        for lower_ch in ch.to_lowercase() {
            lower.push(lower_ch);
        }
    }
    boundaries.push((lower.len(), text.len()));
    (lower, boundaries)
}

fn map_lower_range(boundaries: &[(usize, usize)], range: Range<usize>) -> Range<usize> {
    let start = match boundaries.binary_search_by_key(&range.start, |&(low, _)| low) {
        Ok(idx) => boundaries[idx].1,
        Err(idx) => boundaries[idx.saturating_sub(1)].1,
    };
    if range.end == range.start {
        return start..start;
    }
    let end = match boundaries.binary_search_by_key(&range.end, |&(low, _)| low) {
        Ok(idx) => boundaries[idx].1,
        Err(idx) => boundaries[idx].1,
    };
    start..end
}

/// First case-insensitive match of `query` in `text`, as a UTF-8 byte range into `text`.
pub fn find_case_insensitive(text: &str, query: &str) -> Option<Range<usize>> {
    if query.is_empty() {
        return Some(0..0);
    }
    let needle = query.to_lowercase();
    let (lower, boundaries) = lowercased_with_boundaries(text);
    let low_start = lower.find(&needle)?;
    Some(map_lower_range(
        &boundaries,
        low_start..low_start + needle.len(),
    ))
}

/// Last case-insensitive match of `query` in `text`, as a UTF-8 byte range into `text`.
pub fn rfind_case_insensitive(text: &str, query: &str) -> Option<Range<usize>> {
    if query.is_empty() {
        return Some(text.len()..text.len());
    }
    let needle = query.to_lowercase();
    let (lower, boundaries) = lowercased_with_boundaries(text);
    let low_start = lower.rfind(&needle)?;
    Some(map_lower_range(
        &boundaries,
        low_start..low_start + needle.len(),
    ))
}

fn matched_job(
    text: &str,
    query: &str,
    style: &TextStyle,
    rest: Color32,
    matched: Color32,
    width: f32,
) -> egui::text::LayoutJob {
    let query = query.trim();
    let found = (!query.is_empty())
        .then(|| find_case_insensitive(text, query))
        .flatten();
    let mut job = egui::text::LayoutJob::default();
    match found {
        Some(range) => {
            if range.start > 0 {
                job.append(&text[..range.start], 0.0, style.format(rest));
            }
            job.append(&text[range.clone()], 0.0, style.format(matched));
            if range.end < text.len() {
                job.append(&text[range.end..], 0.0, style.format(rest));
            }
        }
        None => job.append(text, 0.0, style.format(rest)),
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    crate::text::unkerned(job)
}

/// Paints `text` on the line at `at`, cut at `width`, with its first case-insensitive match of
/// `query` in `matched` and the rest in `rest`, unkerned (`font-kerning: none`). Returns its width.
#[expect(
    clippy::too_many_arguments,
    reason = "one cell's text and its two colors"
)]
pub fn paint_matched(
    painter: &Painter,
    at: Pos2,
    text: &str,
    query: &str,
    style: &TextStyle,
    rest: Color32,
    matched: Color32,
    width: f32,
) -> f32 {
    paint_job(
        painter,
        at,
        matched_job(text, query, style, rest, matched, width),
        style,
    )
}

/// A row's avatar: a 20px circle with the initial in black, or nothing in its cell (a domain's).
#[derive(Clone, Debug)]
pub struct RowAvatar {
    /// The letter, upper-cased.
    pub initial: String,
    /// The circle's color.
    pub color: Color32,
}

/// One row.
#[derive(Clone, Debug)]
pub struct ContactRow<'a> {
    /// What the name cell shows: the name, or the address when there's none.
    pub name: &'a str,
    /// The address cell, empty for a row without one.
    pub email: &'a str,
    /// Whether it's already added, which subdues it and keeps the pointer from picking it.
    pub disabled: bool,
    /// For a list with avatars: the avatar, or `None` for an empty cell.
    pub avatar: Option<RowAvatar>,
}

/// Where a row's cells sit, from the row's left: the avatar's left (lists with avatars), and each
/// text's left and width (the cell less its padding).
#[derive(Clone, Copy, Debug)]
pub struct ContactColumns {
    /// The 20px avatar's left, in a list with avatars.
    pub avatar: Option<f32>,
    /// The name's left and width.
    pub name: (f32, f32),
    /// The address's left and width.
    pub email: (f32, f32),
}

/// The list's metrics.
#[derive(Clone, Copy, Debug)]
pub struct ContactListStyle {
    /// The rows' text.
    pub text: TextStyle,
    /// A row's height.
    pub row: f32,
    /// The list's `padding` over the first row and under the last.
    pub padding: f32,
    /// Where the cells sit.
    pub columns: ContactColumns,
    /// Keep Snow's colors in Carbon.
    pub force_snow: bool,
}

/// Metrics and colors for a two-line stacked contact suggestion list (name over email, used by
/// Calendar event form, Share Availability, and Meet).
#[derive(Clone, Copy, Debug)]
pub struct StackedContactStyle {
    /// Card background, border/shadow, and highlighted row fill.
    pub card: crate::AnchoredCard,
    /// Height of each contact row.
    pub row: f32,
    /// Padding above the first row.
    pub pad_top: f32,
    /// Padding below the last row.
    pub pad_bottom: f32,
    /// Horizontal text padding from the row's left and right edges.
    pub pad_x: f32,
    /// Horizontal inset of each row's highlight and hit rect inside the card.
    pub inset_x: f32,
    /// Text style for the name line.
    pub name_style: TextStyle,
    /// Vertical offset of the name line from `row.top()`.
    pub name_top: f32,
    /// `(rest, matched)` inks for the name line.
    pub name_inks: (Color32, Color32),
    /// Text style for the email line.
    pub email_style: TextStyle,
    /// Vertical offset of the email line from `row.top()`.
    pub email_top: f32,
    /// `(rest, matched)` inks for the email line.
    pub email_inks: (Color32, Color32),
    /// Whether `query` matches are darkened in the name and email lines.
    pub match_query: bool,
    /// Whether hover requires pointer movement or fires whenever the pointer is over a row.
    pub hover_mode: crate::AnchoredHover,
}

#[derive(Clone, Copy)]
enum LayoutKind {
    Columns(ContactListStyle),
    Stacked(StackedContactStyle),
}

/// What the pointer did over the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContactListResponse {
    /// The row under a moving pointer.
    pub hovered: Option<usize>,
    /// The row a button went down on (a press picks).
    pub pressed: Option<usize>,
    /// The row whose `Remove Suggestion` was clicked.
    pub removed: Option<usize>,
}

/// A contact list of `rows` for `query`, with `highlight` filled.
pub struct ContactSelectorList<'a> {
    rows: &'a [ContactRow<'a>],
    query: &'a str,
    highlight: Option<usize>,
    layout: LayoutKind,
    removable: &'a [bool],
    all_matched_row: Option<usize>,
}

impl<'a> ContactSelectorList<'a> {
    /// The list, with `highlight` the row filled, if any.
    pub fn new(
        rows: &'a [ContactRow<'a>],
        query: &'a str,
        highlight: Option<usize>,
        style: ContactListStyle,
    ) -> Self {
        Self {
            rows,
            query,
            highlight,
            layout: LayoutKind::Columns(style),
            removable: &[],
            all_matched_row: None,
        }
    }

    /// A two-line stacked contact list (name over email) drawn through [`crate::AnchoredList`].
    pub fn stacked(
        rows: &'a [ContactRow<'a>],
        query: &'a str,
        highlight: Option<usize>,
        style: StackedContactStyle,
    ) -> Self {
        Self {
            rows,
            query,
            highlight,
            layout: LayoutKind::Stacked(style),
            removable: &[],
            all_matched_row: None,
        }
    }

    /// Measures auto-sized name and email columns for `rows` (with `cell_padding` on each side of
    /// a cell and `min_name_width` when at least one row has a non-empty name), returning the
    /// configured [`ContactSelectorList`] and its total width.
    #[expect(
        clippy::too_many_arguments,
        reason = "measures and builds a two-column contact list in one step"
    )]
    pub fn auto_columns(
        ctx: &egui::Context,
        rows: &'a [ContactRow<'a>],
        query: &'a str,
        highlight: Option<usize>,
        text: TextStyle,
        row: f32,
        padding: f32,
        cell_padding: f32,
        min_name_width: f32,
    ) -> (Self, f32) {
        let [name_width, email_width] =
            measure_contact_columns(ctx, rows, &text, cell_padding, min_name_width);
        let name = if name_width > 0.0 {
            (cell_padding, name_width - 2.0 * cell_padding)
        } else {
            (0.0, 0.0)
        };
        let style = ContactListStyle {
            text,
            row,
            padding,
            columns: ContactColumns {
                avatar: None,
                name,
                email: (name_width + cell_padding, email_width - 2.0 * cell_padding),
            },
            force_snow: false,
        };
        (
            Self::new(rows, query, highlight, style),
            name_width + email_width,
        )
    }

    /// Marks row `row` as an all-highlighted summary row (such as `"Everybody in this
    /// conversation"`), painting its name all in the matched ink and its email plain in the rest
    /// ink.
    pub fn all_matched_row(mut self, row: Option<usize>) -> Self {
        self.all_matched_row = row;
        self
    }

    /// Which rows offer `Remove Suggestion` while highlighted and hovered (not the thread's own
    /// participants).
    pub fn removable(mut self, removable: &'a [bool]) -> Self {
        self.removable = removable;
        self
    }

    /// The box's height for the rows.
    pub fn height(&self) -> f32 {
        match self.layout {
            LayoutKind::Columns(style) => self.rows.len() as f32 * style.row + 2.0 * style.padding,
            LayoutKind::Stacked(style) => {
                style.pad_top + self.rows.len() as f32 * style.row + style.pad_bottom
            }
        }
    }

    /// Draws the list as a box of `width` at `at`, over everything in `order`.
    pub fn show(
        &self,
        ctx: &egui::Context,
        id: Id,
        at: Pos2,
        width: f32,
        order: egui::Order,
    ) -> ContactListResponse {
        match self.layout {
            LayoutKind::Columns(style) => self.show_columns(ctx, id, at, width, order, style),
            LayoutKind::Stacked(style) => self.show_stacked(ctx, id, at, width, order, style),
        }
    }

    fn show_stacked(
        &self,
        ctx: &egui::Context,
        id: Id,
        at: Pos2,
        width: f32,
        order: egui::Order,
        style: StackedContactStyle,
    ) -> ContactListResponse {
        let rect = Rect::from_min_size(at, vec2(width, self.height()));
        let out = crate::AnchoredList::new(id, rect, style.card)
            .order(order)
            .padding(style.pad_top, style.pad_bottom)
            .inset_x(style.inset_x)
            .highlighted(self.highlight)
            .hover_mode(style.hover_mode)
            .show_variable(
                ctx,
                &vec![style.row; self.rows.len()],
                |index| !self.rows[index].disabled,
                |painter, row, index| {
                    let contact = &self.rows[index];
                    let room = (row.width() - 2.0 * style.pad_x).max(0.0);
                    let name_at = pos2(row.left() + style.pad_x, row.top() + style.name_top);
                    let email_at = pos2(row.left() + style.pad_x, row.top() + style.email_top);
                    if self.all_matched_row == Some(index) || !style.match_query {
                        let name_ink = if self.all_matched_row == Some(index) {
                            style.name_inks.1
                        } else {
                            style.name_inks.0
                        };
                        style.name_style.paint_line(
                            painter,
                            name_at,
                            contact.name,
                            name_ink,
                            Some(room),
                        );
                        if !contact.email.is_empty() {
                            style.email_style.paint_line(
                                painter,
                                email_at,
                                contact.email,
                                style.email_inks.0,
                                Some(room),
                            );
                        }
                    } else {
                        paint_matched(
                            painter,
                            name_at,
                            contact.name,
                            self.query,
                            &style.name_style,
                            style.name_inks.0,
                            style.name_inks.1,
                            room,
                        );
                        if !contact.email.is_empty() {
                            paint_matched(
                                painter,
                                email_at,
                                contact.email,
                                self.query,
                                &style.email_style,
                                style.email_inks.0,
                                style.email_inks.1,
                                room,
                            );
                        }
                    }
                },
            );
        ContactListResponse {
            hovered: out.hovered,
            pressed: out.pressed,
            removed: None,
        }
    }

    fn show_columns(
        &self,
        ctx: &egui::Context,
        id: Id,
        at: Pos2,
        width: f32,
        order: egui::Order,
        style: ContactListStyle,
    ) -> ContactListResponse {
        let rect = Rect::from_min_size(at, vec2(width, self.height()));
        let colors = ContactListColors::new(crate::palette(ctx), style.force_snow);
        let (hover, pressed, moving) = ctx.input(|input| {
            (
                input
                    .pointer
                    .interact_pos()
                    .or_else(|| input.pointer.hover_pos()),
                input.pointer.primary_pressed(),
                input.pointer.is_moving() || input.pointer.delta() != Vec2::ZERO,
            )
        });
        let mut response = ContactListResponse::default();
        egui::Area::new(id)
            .order(order)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |area| {
                let painter = area.painter().clone();
                paint_contact_list_box(&painter, rect, colors.background);
                let _ = area.allocate_rect(rect, Sense::click());
                let layer = area.layer_id();
                let over = !area.is_sizing_pass()
                    && hover.is_some_and(|at| {
                        rect.contains(at) && crate::anchored_list::layer_unoccluded(ctx, layer, at)
                    });
                let text = &style.text;
                let columns = style.columns;
                let mut over_selectable = false;
                for (index, contact) in self.rows.iter().enumerate() {
                    let row = Rect::from_min_size(
                        pos2(
                            rect.left(),
                            rect.top() + style.padding + index as f32 * style.row,
                        ),
                        vec2(width, style.row),
                    );
                    if self.highlight == Some(index) {
                        painter.rect_filled(pixel_snap(row), 0.0, colors.selected);
                    }
                    if let (Some(left), Some(avatar)) = (columns.avatar, &contact.avatar) {
                        let circle = Rect::from_center_size(
                            pos2(row.left() + left + 10.0, row.center().y),
                            vec2(20.0, 20.0),
                        );
                        // The initial: 12px, `.355em` down, in black.
                        let initial = TextStyle::new(text.font, 12.0, 16.0);
                        crate::paint_initial_avatar(
                            &painter,
                            circle,
                            avatar.color,
                            &avatar.initial,
                            &initial,
                            Color32::BLACK,
                            0.355 * 12.0,
                        );
                    }
                    // `padding-top: 4px` over the 28px line.
                    let line = row.top() + 4.0;
                    let (name, email) = if contact.disabled {
                        (
                            (colors.disabled, colors.disabled),
                            (colors.disabled, colors.disabled),
                        )
                    } else {
                        (colors.name, colors.email)
                    };
                    paint_matched(
                        &painter,
                        pos2(row.left() + columns.name.0, line),
                        contact.name,
                        self.query,
                        text,
                        name.0,
                        name.1,
                        columns.name.1,
                    );
                    if !contact.email.is_empty() {
                        paint_matched(
                            &painter,
                            pos2(row.left() + columns.email.0, line),
                            contact.email,
                            self.query,
                            text,
                            email.0,
                            email.1,
                            columns.email.1,
                        );
                    }
                    let row_hovered = over && hover.is_some_and(|at| row.contains(at));
                    let removing = row_hovered
                        && self.highlight == Some(index)
                        && self.removable.get(index).copied().unwrap_or(false);
                    let mut over_remove = false;
                    if removing {
                        let cell = Rect::from_min_size(
                            pos2(row.right() - REMOVE_WIDTH, row.top()),
                            vec2(REMOVE_WIDTH, REMOVE_HEIGHT),
                        );
                        let button = area
                            .interact(cell, id.with(("remove", index)), Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand);
                        over_remove = button.hovered();
                        crate::paint_fade_in(&painter, cell, colors.selected);
                        // The cross: 30% ink, 50% while hovered, over 150ms.
                        let hovered = crate::motion::transition(
                            ctx,
                            id.with(("remove-hover", index)),
                            over_remove,
                            0.15,
                            crate::motion::CubicBezier::EASE,
                        );
                        let alpha = 0.3 + 0.2 * hovered;
                        let ink = if style.force_snow || crate::palette(ctx).theme == Theme::Snow {
                            Color32::from_black_alpha((alpha * 255.0).round() as u8)
                        } else {
                            Color32::from_white_alpha((alpha * 255.0).round() as u8)
                        };
                        let icon = Rect::from_min_size(
                            pos2(
                                cell.left() + REMOVE_INSET,
                                cell.center().y - REMOVE_ICON / 2.0,
                            ),
                            vec2(REMOVE_ICON, REMOVE_ICON),
                        );
                        crate::paint_icon_on(&painter, "legacy/close", icon, ink);
                        // Right of the button, past the tooltip's 8px padding.
                        KeyTooltip {
                            text: "Remove Suggestion",
                            keys: &[],
                        }
                        .show_at(area, &button, Placement::Right(8.0));
                        if button.clicked() {
                            response.removed = Some(index);
                        }
                    }
                    if row_hovered {
                        if !contact.disabled {
                            over_selectable = true;
                        }
                        if moving {
                            response.hovered = Some(index);
                        }
                        if pressed && !contact.disabled && !over_remove {
                            response.pressed = Some(index);
                        }
                    }
                }
                if over_selectable {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
            });
        response
    }
}

/// Measures the unkerned `[name_width, email_width]` (including `2 * cell_padding` per non-empty
/// column) for `rows`.
pub fn measure_contact_columns(
    ctx: &egui::Context,
    rows: &[ContactRow<'_>],
    style: &TextStyle,
    cell_padding: f32,
    min_name_width: f32,
) -> [f32; 2] {
    let name_width = if rows.iter().all(|row| row.name.is_empty()) {
        0.0
    } else {
        (rows
            .iter()
            .map(|row| style.width_unkerned(ctx, row.name))
            .fold(0.0, f32::max)
            + 2.0 * cell_padding)
            .max(min_name_width)
    };
    let email_width = rows
        .iter()
        .map(|row| style.width_unkerned(ctx, row.email))
        .fold(0.0, f32::max)
        + 2.0 * cell_padding;
    [name_width, email_width]
}

#[cfg(test)]
mod tests {
    use crate::{Family, Font};

    use super::*;

    fn runs<'a>(text: &'a str, query: &str) -> Vec<(&'a str, bool)> {
        let rest = Color32::from_gray(120);
        let bold = Color32::BLACK;
        let style = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 20.0);
        let job = matched_job(text, query, &style, rest, bold, 400.0);
        let mut out: Vec<(Range<usize>, bool)> = Vec::new();
        for section in &job.sections {
            let range = section.byte_range.start.0..section.byte_range.end.0;
            let is_bold = section.format.color == bold;
            match out.last_mut() {
                Some((prev, prev_bold)) if *prev_bold == is_bold && prev.end == range.start => {
                    prev.end = range.end;
                }
                _ => out.push((range, is_bold)),
            }
        }
        out.into_iter()
            .map(|(range, is_bold)| (&text[range], is_bold))
            .collect()
    }

    #[test]
    fn case_insensitive_matching_maps_byte_expanding_and_shrinking_letters() {
        // `'İ'` (U+0130, 2 UTF-8 bytes) lowercases to `"i\u{0307}"` (3 UTF-8 bytes).
        let dotted = "İbrahim <ibrahim@example.com>";
        assert_eq!(
            runs(dotted, "ibrahim"),
            [
                ("İbrahim <", false),
                ("ibrahim", true),
                ("@example.com>", false)
            ]
        );
        assert_eq!(runs("İzmir", "İz"), [("İz", true), ("mir", false)]);
        assert_eq!(runs("İzmir", "i"), [("İ", true), ("zmir", false)]);

        // `'ẞ'` (U+1E9E, 3 UTF-8 bytes) lowercases to `'ß'` (2 UTF-8 bytes).
        assert_eq!(
            runs("GROẞE STRAẞE <info@strasse.de>", "ß"),
            [
                ("GRO", false),
                ("ẞ", true),
                ("E STRAẞE <info@strasse.de>", false)
            ]
        );
        assert_eq!(
            runs("GROẞE STRAẞE <info@strasse.de>", "info"),
            [
                ("GROẞE STRAẞE <", false),
                ("info", true),
                ("@strasse.de>", false)
            ]
        );
        assert_eq!(
            rfind_case_insensitive("GROẞE STRAẞE", "ß"),
            Some("GROẞE STRA".len().."GROẞE STRAẞ".len())
        );
    }

    #[test]
    fn columns_list_respects_layer_occlusion_and_sets_pointing_hand() {
        let ctx = egui::Context::default();
        crate::install(&ctx).expect("system fonts");
        let rows = [
            ContactRow {
                name: "Alice",
                email: "alice@example.com",
                disabled: false,
                avatar: None,
            },
            ContactRow {
                name: "Bob",
                email: "bob@example.com",
                disabled: true,
                avatar: None,
            },
        ];
        let style = ContactListStyle {
            text: TextStyle::new(Font::new(Family::Sans, 400), 13.0, 28.0),
            row: 32.0,
            padding: 8.0,
            columns: ContactColumns {
                avatar: None,
                name: (12.0, 80.0),
                email: (100.0, 140.0),
            },
            force_snow: false,
        };
        let at = pos2(20.0, 20.0);
        let width = 260.0;
        let run = |events: Vec<egui::Event>, occluded: bool| -> (ContactListResponse, CursorIcon) {
            let mut out = ContactListResponse::default();
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let list = ContactSelectorList::new(&rows, "", Some(0), style);
                    out = list.show(
                        ui.ctx(),
                        Id::new("cols-list"),
                        at,
                        width,
                        egui::Order::Middle,
                    );
                    if occluded {
                        let cover = Rect::from_min_size(at, vec2(width, list.height()));
                        egui::Area::new(Id::new("cols-cover"))
                            .order(egui::Order::Foreground)
                            .fixed_pos(cover.min)
                            .constrain(false)
                            .show(ui.ctx(), |area| {
                                let _ = area.allocate_rect(cover, Sense::click());
                            });
                    }
                },
            );
            let cursor = output.platform_output.cursor_icon;
            output.drop_without_applying_deltas();
            (out, cursor)
        };

        let row0 = pos2(60.0, at.y + 8.0 + 16.0);
        let row1 = pos2(60.0, at.y + 8.0 + 32.0 + 16.0);
        // Prime occluded layers past their initial sizing pass so the covering modal is interactable.
        let _ = run(vec![egui::Event::PointerMoved(row0)], true);
        let _ = run(vec![egui::Event::PointerMoved(row0)], true);
        let (occluded_out, occluded_cursor) = run(
            vec![
                egui::Event::PointerMoved(pos2(row0.x + 2.0, row0.y)),
                egui::Event::PointerButton {
                    pos: pos2(row0.x + 2.0, row0.y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            true,
        );
        assert_eq!(occluded_out.hovered, None);
        assert_eq!(occluded_out.pressed, None);
        assert_ne!(occluded_cursor, CursorIcon::PointingHand);
        let _ = run(
            vec![egui::Event::PointerButton {
                pos: pos2(row0.x + 2.0, row0.y),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );

        // Uncover and hover selectable row 0 -> PointingHand and press works.
        let _ = run(vec![egui::Event::PointerMoved(row0)], false);
        let (active_out, active_cursor) = run(
            vec![
                egui::Event::PointerMoved(pos2(row0.x + 2.0, row0.y)),
                egui::Event::PointerButton {
                    pos: pos2(row0.x + 2.0, row0.y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            false,
        );
        assert_eq!(active_out.hovered, Some(0));
        assert_eq!(active_out.pressed, Some(0));
        assert_eq!(active_cursor, CursorIcon::PointingHand);

        // Disabled row 1 does not show PointingHand or pick on press.
        let (disabled_out, disabled_cursor) = run(
            vec![
                egui::Event::PointerMoved(row1),
                egui::Event::PointerButton {
                    pos: row1,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            false,
        );
        assert_eq!(disabled_out.pressed, None);
        assert_ne!(disabled_cursor, CursorIcon::PointingHand);
    }
}
