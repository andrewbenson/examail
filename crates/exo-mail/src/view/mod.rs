//! A message body laid out and painted with Blitz.
//!
//! [`BodyView`] builds one document per message that stands in for the shadow root a body renders
//! into: its stylesheet, the unquoted part with the original `<body>` attributes, the `…` toggle,
//! and the quoted part when it is shown. It then lays that out at the message column's width and
//! paints it into an RGBA buffer.

mod document;
mod find;
mod hit;
mod images;
mod selection;

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyrender::{AntiAliasing, ImageRenderer as _, PaintScene as _, recording::RenderCommand};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::NodeData;
use blitz_html::HtmlDocument;
use peniko::{
    Color, Fill,
    kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape as _},
};

pub(crate) use document::{BASE_URL, root_class, shadow_root};
pub use hit::{ContextHit, ContextImage, Granularity, Hit};
pub use images::Animated;

use self::{
    document::{MIN_WINDOW_WIDTH, build_document, viewport},
    hit::{TextPoint, in_layout_order},
    images::Animation,
};
use crate::{
    Fonts, Render,
    net::{Fetch, FetchProvider, MayAnimate},
};

/// The height of a too-wide body's horizontal scrollbar, which sits below its content because a
/// custom scrollbar takes up space (`.ShadowBody::-webkit-scrollbar { height: 11px }`).
pub const SCROLLBAR_HEIGHT: f32 = 11.0;

/// The tallest band painted at once, in device pixels (the rasterizer's sizes are 16-bit).
const MAX_BAND_ROWS: u32 = 8192;

/// How many times a layout resolves styles at most: once, and again for the `data:` images the
/// styles asked for (whose own styles could ask for more).
const MAX_RESOLVES: usize = 3;

/// Bodies up to this much wider than the column are shrunk to fit it…
const MAX_SHRINK: f32 = 1.25;
/// …plus this many px.
const SHRINK_AFFORDANCE: f32 = 2.0;

/// The two app themes, which decide the colors of personal mail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// `Theme-light` (also tagged `Theme-snow`).
    #[default]
    Light,
    /// `Theme-dark` (`Theme-carbon`). Marketing mail stays dark-on-white, on the white card the
    /// app draws behind it.
    Dark,
}

/// How a body is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewOptions {
    /// The app theme.
    pub theme: Theme,
    /// Whether the quoted part is expanded below the `…`.
    pub show_quoted: bool,
    /// The app's root font size in px (it scales with the window width). Senders' `rem` lengths
    /// and the `1.25rem` quote indent resolve against it.
    pub rem: f32,
    /// Nothing loads from the network (`Block all remote images`, until the message's images are
    /// shown). Remote images show as empty boxes, and CSS images, fonts and stylesheets stay
    /// unloaded. Inline attachments and `data:` images still show.
    pub block_remote_content: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            theme: Theme::Light,
            show_quoted: false,
            rem: 15.0,
            block_remote_content: false,
        }
    }
}

/// A part of the body to paint, and where it lands on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Region {
    /// The part's top, in CSS px from the top of the body.
    pub top: f32,
    /// Its height in CSS px.
    pub height: f32,
    /// Its left edge, in CSS px from the left of the body.
    pub left: f32,
    /// Its width in CSS px: the column's ([`BodyView::width`]), or the content's
    /// ([`BodyView::content_width`]) for a body that scrolls sideways.
    pub width: f32,
    /// The fractional part of the body's on-screen position, in CSS px. Chromium places glyphs
    /// at subpixel positions of their absolute coordinates, so a body drawn at x = 364.625 has
    /// to be painted 0.625 px to the right of a whole pixel to look the same.
    pub phase: [f32; 2],
}

/// Painted pixels: premultiplied RGBA8, `width × height` device pixels, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// One message body, ready to lay out, paint and hit-test.
///
/// Layout happens in CSS px and the result is scaled when painting, the way Chromium on macOS
/// does it. Line boxes and box edges snap to whole CSS px, not device pixels.
///
/// Options are fixed for a view, so to show or hide the quoted part, build a new one. A view isn't
/// `Send` (Stylo's font metrics aren't), so build, lay out and paint it on one thread, and send the
/// [`Image`]s elsewhere if painting shouldn't block the UI.
pub struct BodyView {
    pub(in crate::view) document: HtmlDocument,
    /// The `.ShadowHTML` element, whose width is the column's and whose height is the body's.
    pub(in crate::view) root: blitz_dom::NodeId,
    /// Its classes as composed, before [`BodyView::layout`] adds `at-1x`.
    root_class: String,
    theme: Theme,
    /// The message column's width, in CSS px.
    width: f32,
    scale: f32,
    /// How much a slightly-too-wide body is shrunk to fit the column (1 when it fits).
    pub(in crate::view) zoom: f32,
    /// [`Self::content_width`] as of the last layout.
    content_width: f32,
    /// Responses the fetch hook has delivered, and how many were laid out.
    pub(in crate::view) arrived: Option<Arc<AtomicUsize>>,
    /// How many `data:` URLs have been answered.
    answered_inline: Option<Arc<AtomicUsize>>,
    pub(in crate::view) applied: usize,
    /// Images that arrived and may be animated, not looked at yet.
    pub(in crate::view) may_animate: Option<MayAnimate>,
    pub(in crate::view) animations: Vec<Animation>,
    /// The inline roots the selection starts and ends in, which Blitz's ranges leave out when
    /// no text is selected there.
    pub(in crate::view) selection_ends: Option<(blitz_dom::NodeId, blitz_dom::NodeId)>,
    /// The word or paragraph a double or triple click selected, which a drag extends by whole
    /// words or paragraphs.
    pub(in crate::view) selected_unit: Option<(Granularity, Range<TextPoint>)>,
    /// What the last [`BodyView::find`] found, and which of it is the current match.
    pub(in crate::view) found: Vec<(blitz_dom::NodeId, Range<usize>)>,
    pub(in crate::view) current_match: Option<usize>,
}

impl BodyView {
    /// Builds the body for `render`. `fetch` loads what the message references (remote images,
    /// inline attachments, imported CSS). Without it, those stay blank.
    pub fn new(
        render: &Render,
        options: ViewOptions,
        fonts: &Fonts,
        fetch: Option<Arc<dyn Fetch>>,
    ) -> Self {
        let remote = !options.block_remote_content;
        let provider = fetch.map(|fetch| Arc::new(FetchProvider::new(fetch, remote)));
        let (document, root, root_class) = build_document(render, options, fonts, provider.clone());
        Self {
            document,
            root,
            root_class,
            theme: options.theme,
            width: 0.0,
            scale: 0.0,
            zoom: 1.0,
            content_width: 0.0,
            arrived: provider.as_ref().map(|provider| provider.arrived.clone()),
            answered_inline: provider
                .as_ref()
                .map(|provider| provider.answered_inline.clone()),
            applied: 0,
            may_animate: provider.map(|provider| provider.may_animate.clone()),
            animations: Vec::new(),
            selection_ends: None,
            selected_unit: None,
            found: Vec::new(),
            current_match: None,
        }
    }

    /// Lays the body out `width` CSS px wide for a display with `scale` device pixels per CSS px,
    /// and returns its height in CSS px.
    ///
    /// A body up to 25% wider than the column is shrunk to fit. Wider ones keep their size and
    /// overflow ([`Self::content_width`]). Every coordinate this view takes or returns is in the
    /// column's (shrunk) CSS px.
    pub fn layout(&mut self, width: f32, scale: f32) -> f32 {
        if width != self.width {
            self.width = width;
            self.zoom = 1.0;
            self.document
                .set_viewport(viewport(width.max(MIN_WINDOW_WIDTH), self.theme));
            self.set_document_width(width);
        }
        if scale != self.scale {
            // The body CSS's `@media (max-resolution: 1dppx)` rules. Blitz sees a 1× device
            // because layout happens in CSS px, so the view says which display it's for instead.
            let class = if scale <= 1.0 {
                format!("{} at-1x", self.root_class)
            } else {
                self.root_class.clone()
            };
            self.document
                .mutate()
                .set_attribute(self.root, blitz_dom::qual_name!("class"), &class);
            self.scale = scale;
        }
        self.resolve();
        self.shrink_to_fit();
        self.height()
    }

    /// The column's width in CSS px, as last laid out.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// How much the body is shrunk to fit its column, as last laid out (1 when it fits).
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Shrinks a body that is a little too wide for the column, and measures what's left wider
    /// than it. It shrinks once, so the body isn't rescaled after the images have loaded.
    pub(in crate::view) fn shrink_to_fit(&mut self) {
        let content = self.document_content_width();
        if self.zoom == 1.0
            && content > self.width
            && content <= self.width * MAX_SHRINK + SHRINK_AFFORDANCE
        {
            self.zoom = self.width / content;
            self.set_document_width(content);
            self.resolve();
        }
        self.content_width = self.document_content_width() * self.zoom;
    }

    fn set_document_width(&mut self, width: f32) {
        self.document
            .set_style_property(self.root, "--exo-width", &format!("{width}px"));
    }

    /// The content's width in the document's own CSS px: the right edge of the furthest box.
    fn document_content_width(&self) -> f32 {
        let mut right: f64 = 0.0;
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let Some(node) = self.document.get_node(id) else {
                continue;
            };
            if matches!(node.data, NodeData::Element(_))
                && let Some(rect) = self.document.get_client_bounding_rect(id)
            {
                right = right.max(rect.x + rect.width);
            }
            stack.extend(node.children.iter().copied());
        }
        (right as f32).max(self.width / self.zoom)
    }

    /// Applies loaded resources and lays out, then undoes Taffy's rounding. Chromium keeps
    /// fractional box positions (text lands at subpixel offsets) and snaps only what it paints,
    /// which [`BodyView::paint`] does.
    ///
    /// A `data:` image is answered as soon as it's asked for, and a style's (a CSS background's)
    /// is asked for while styles resolve, after the answers were taken: the view resolves again
    /// for those.
    pub(in crate::view) fn resolve(&mut self) {
        let answered = |view: &Self| {
            view.answered_inline
                .as_ref()
                .map_or(0, |answered| answered.load(Ordering::Acquire))
        };
        for _ in 0..MAX_RESOLVES {
            let before = answered(self);
            self.document.handle_messages();
            self.document.resolve(0.0);
            if answered(self) == before {
                break;
            }
        }
        self.adopt_animations();
        let ids: Vec<_> = self
            .document
            .tree()
            .iter()
            .filter(|(_, node)| {
                matches!(
                    node.data,
                    NodeData::Element(_) | NodeData::AnonymousBlock(_) | NodeData::Document(_)
                )
            })
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            if let Some(node) = self.document.get_node_mut(id) {
                *node.final_layout_mut() = *node.unrounded_layout();
            }
        }
    }

    /// An element's border box in the column's CSS px.
    pub(in crate::view) fn client_box(&self, node: blitz_dom::NodeId) -> Option<[f32; 4]> {
        let rect = self.document.get_client_bounding_rect(node)?;
        Some([rect.x, rect.y, rect.width, rect.height].map(|value| value as f32 * self.zoom))
    }

    /// The laid-out height in CSS px, with the horizontal scrollbar's when the body scrolls
    /// sideways.
    pub fn height(&self) -> f32 {
        let content = self
            .document
            .get_node(self.root)
            .map_or(0.0, |node| node.final_layout().size.height * self.zoom);
        content
            + if self.scrolls() {
                SCROLLBAR_HEIGHT
            } else {
                0.0
            }
    }

    /// The laid-out content width in CSS px. It's wider than the column when the message is too
    /// wide to shrink, and then the body scrolls sideways.
    pub fn content_width(&self) -> f32 {
        self.content_width
    }

    /// Whether the body is wider than its column, so it scrolls sideways (the two widths compared
    /// in whole px, as Chromium compares them).
    pub fn scrolls(&self) -> bool {
        self.content_width.round() > self.width.round()
    }

    /// Paints `region` of the last layout onto `background` (premultiplied RGBA, or `[0; 4]` for
    /// transparent, to composite over the message card).
    pub fn paint(&mut self, region: Region, background: [u8; 4]) -> Image {
        let width = device(region.width + region.phase[0], self.scale);
        let height = device(region.height, self.scale);
        let row = width as usize * 4;
        let mut rgba = vec![0; row * height as usize];
        // The rasterizer takes 16-bit sizes, so tall bodies are painted in bands.
        let mut painted = 0;
        while painted < height {
            let rows = (height - painted).min(MAX_BAND_ROWS);
            let band = &mut rgba[painted as usize * row..(painted + rows) as usize * row];
            let top = f64::from(region.top) + f64::from(painted) / f64::from(self.scale);
            let left = f64::from(region.left);
            self.paint_band([left, top], region.phase, width, rows, background, band);
            painted += rows;
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    /// Paints into `out` the `width` × `rows` device pixels whose top left is `[left, top]` CSS px
    /// into the body.
    fn paint_band(
        &mut self,
        [left, top]: [f64; 2],
        phase: [f32; 2],
        width: u32,
        rows: u32,
        background: [u8; 4],
        out: &mut [u8],
    ) {
        // Device pixels per CSS px of the document, which a shrunk body draws smaller.
        let scale = f64::from(self.scale * self.zoom);
        let [phase_x, phase_y] = phase.map(|p| f64::from(p * self.scale));
        let [left, top] = [left, top].map(|edge| edge / f64::from(self.zoom));
        // Blitz paints from a whole CSS px, and the fractional rest becomes a translation.
        let [first_column, first_row] = [left, top].map(|edge| edge.max(0.0).floor());
        let css_columns = (f64::from(width) / scale + left - first_column).ceil() as u32 + 1;
        let css_rows = (f64::from(rows) / scale + top - first_row).ceil() as u32 + 1;
        let mut scene = anyrender::Scene::new();
        // Painting from a point is scrolling the viewport to it (Blitz's paint offsets move the
        // content the other way).
        self.document.set_viewport_scroll(blitz_dom::Point {
            x: first_column,
            y: first_row,
        });
        blitz_paint::paint_scene(
            &mut scene,
            &mut self.document,
            1.0,
            css_columns,
            css_rows,
            0,
            0,
        );
        self.document
            .set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
        let transform = Affine::translate((
            phase_x - (left - first_column) * scale,
            phase_y - (top - first_row) * scale,
        )) * Affine::scale(scale);
        for command in &mut scene.commands {
            match_chromium(command, transform, scale);
        }
        let mut renderer = VelloCpuImageRenderer::new(width, rows);
        renderer.render(
            |painter| {
                if background != [0; 4] {
                    let [r, g, b, a] = background;
                    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(rows));
                    painter.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::from_rgba8(r, g, b, a),
                        None,
                        &bounds,
                        AntiAliasing::Enabled,
                    );
                }
                painter.append_scene(scene, transform);
            },
            out,
        );
    }
}

/// Adjusts one painted command to where Chromium on macOS would draw it, given the transform
/// that takes it to device pixels. Boxes (fills, strokes and clips that are plain rectangles)
/// snap to whole CSS px, and glyphs to quarter device pixels.
fn match_chromium(command: &mut RenderCommand, to_device: Affine, scale: f64) {
    match command {
        RenderCommand::GlyphRun(run) => match_chromium_glyphs(run, to_device),
        RenderCommand::Fill(fill) => snap_rect(&mut fill.shape, to_device * fill.transform, scale),
        RenderCommand::PushClipLayer(clip) => {
            snap_rect(&mut clip.clip, to_device * clip.transform, scale)
        }
        RenderCommand::PushLayer(layer) => {
            snap_rect(&mut layer.clip, to_device * layer.transform, scale)
        }
        RenderCommand::Stroke(stroke) => {
            if let Some(line) = date_line(stroke, to_device, scale) {
                *command = line;
            }
        }
        _ => {}
    }
}

/// Draws a glyph run the way Chromium (Skia over CoreText) does on macOS. The run is unhinted,
/// and each glyph's origin sits on the nearest quarter device pixel across and whole device pixel
/// down (Skia's subpixel positioning of horizontal text, `SkGlyphPositionRoundingSpec`). CoreText
/// doesn't hint, and `-webkit-font-smoothing: antialiased` stops it emboldening, which is the only
/// time Blitz turns hinting off.
///
/// On macOS, anyrender_vello_cpu instead moves an upright run's glyphs to CoreGraphics' grid,
/// which is Safari's: x floored to 1/2 device px at body sizes, and the baseline to the pixel
/// below. That put words up to ½ device px left of Chromium's. It leaves skewed runs alone, so
/// the run gets a skew no glyph can show (10⁻³⁰⁰ px per px of height), which Vello's glyph
/// rasterizer treats as none.
fn match_chromium_glyphs(run: &mut anyrender::recording::GlyphRunCommand, to_device: Affine) {
    run.hint = false;
    let transform = to_device * run.transform;
    let [x_scale, b, c, y_scale, ..] = transform.as_coeffs();
    if b != 0.0 || c != 0.0 || x_scale == 0.0 || y_scale == 0.0 {
        return;
    }
    for glyph in &mut run.glyphs {
        let origin = transform * Point::new(f64::from(glyph.x), f64::from(glyph.y));
        glyph.x += (((origin.x * 4.0).round() / 4.0 - origin.x) / x_scale) as f32;
        glyph.y += ((origin.y.round() - origin.y) / y_scale) as f32;
    }
    run.transform *= Affine::skew(UNSEEN_SKEW, 0.0);
}

/// See [`match_chromium_glyphs`].
const UNSEEN_SKEW: f64 = 1e-300;

/// Snaps an axis-aligned rectangle's edges to whole CSS px (`scale` device pixels) of its final
/// position, the way Chromium pixel-snaps box painting.
fn snap_rect(shape: &mut BezPath, full: Affine, scale: f64) {
    let [a, b, c, d, _, _] = full.as_coeffs();
    if b != 0.0 || c != 0.0 || a == 0.0 || d == 0.0 {
        return;
    }
    let Some(rect) = as_rect(shape) else {
        return;
    };
    let device = full.transform_rect_bbox(rect);
    let snap = |v: f64| (v / scale).round() * scale;
    let snapped = Rect::new(
        snap(device.x0),
        snap(device.y0),
        snap(device.x1),
        snap(device.y1),
    );
    *shape = full.inverse() * snapped.to_path(0.1);
}

/// The rectangle a path traces, if it is one (as `Rect::to_path` writes it).
fn as_rect(path: &BezPath) -> Option<Rect> {
    let mut points = Vec::with_capacity(5);
    for element in path.elements() {
        match element {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => points.push(*p),
            PathEl::ClosePath => {}
            _ => return None,
        }
    }
    if points.len() == 5 && points[4] == points[0] {
        points.pop();
    }
    let [p0, p1, p2, p3] = points.as_slice() else {
        return None;
    };
    let horizontal_first = p0.y == p1.y && p1.x == p2.x && p2.y == p3.y && p3.x == p0.x;
    let vertical_first = p0.x == p1.x && p1.y == p2.y && p2.x == p3.x && p3.y == p0.y;
    (horizontal_first || vertical_first).then(|| Rect::from_points(*p0, *p2))
}

/// The color `body.css` gives the line under detected dates.
const DATE_LINE_MARKER: [u8; 4] = [170, 170, 170, 254];

/// Turns the 1 CSS px underline `body.css` puts under detected dates into a hairline: `#aaa`, 0.5px
/// at the line's top, its ends snapped as a box's edges would be. (Blitz floors decoration
/// thickness to whole pixels of the scale it paints at, which is 1×.)
fn date_line(
    stroke: &anyrender::recording::StrokeCommand,
    to_device: Affine,
    scale: f64,
) -> Option<RenderCommand> {
    let anyrender::Paint::Solid(color) = stroke.brush else {
        return None;
    };
    if color.to_rgba8().to_u8_array() != DATE_LINE_MARKER {
        return None;
    }
    let full = to_device * stroke.transform;
    let bounds = full.transform_rect_bbox(stroke.shape.bounding_box());
    let top = bounds.y0 - stroke.style.width * scale / 2.0;
    let snap = |v: f64| (v / scale).round() * scale;
    let rect = Rect::new(snap(bounds.x0), top, snap(bounds.x1), top + scale / 2.0);
    Some(RenderCommand::Fill(anyrender::recording::FillCommand {
        fill: Fill::NonZero,
        transform: to_device.inverse(),
        brush: anyrender::Paint::Solid(Color::from_rgb8(170, 170, 170)),
        brush_transform: None,
        shape: rect.to_path(0.1),
        anti_aliasing: AntiAliasing::Enabled,
    }))
}

/// CSS px to whole device pixels.
fn device(css: f32, scale: f32) -> u32 {
    (css * scale).ceil().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_land_on_chromiums_grid() {
        let mut run = anyrender::recording::GlyphRunCommand {
            font_data: peniko::FontData::new(peniko::Blob::new(Arc::new(Vec::<u8>::new())), 0),
            font_size: 14.0,
            hint: true,
            normalized_coords: Vec::new(),
            embolden: Default::default(),
            style: Fill::NonZero.into(),
            brush: Color::BLACK.into(),
            brush_alpha: 1.0,
            transform: Affine::translate((10.0, 20.0)),
            glyph_transform: None,
            glyphs: vec![anyrender::Glyph {
                id: 1,
                x: 0.4,
                y: 0.3,
            }],
            anti_aliasing: AntiAliasing::Enabled,
        };
        let to_device = Affine::translate((0.6, 0.0)) * Affine::scale(2.0);
        match_chromium_glyphs(&mut run, to_device);
        let transform = to_device * run.transform;
        let glyph = &run.glyphs[0];
        let origin = transform * Point::new(f64::from(glyph.x), f64::from(glyph.y));
        // 2 × 10.4 + 0.6 = 21.4 is 21.5 for Skia (CoreGraphics' grid would make it 21), and the
        // baseline's 40.6 is 41.
        assert!(
            (origin.x - 21.5).abs() < 1e-4 && (origin.y - 41.0).abs() < 1e-4,
            "{origin:?}"
        );
        assert!(!run.hint);
        // anyrender_vello_cpu only regrids runs without a skew.
        assert_ne!(transform.as_coeffs()[2], 0.0);
    }
}
