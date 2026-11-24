//! Dragging the selection to move it, as Chromium drags and drops in a `contenteditable` on a Mac.
//! A press on the selection doesn't move the caret. Once the pointer moves more than 4px with the
//! button down, the press drags the selected text, with its picture following the pointer and a
//! caret where it would land. Let go there, and the text moves (or, with ⌥, is copied). Let go on
//! the selection itself, or press Escape, and nothing happens. A press let go without dragging
//! puts the caret there.
//!
//! Out of the editor, the Mac's own drag carries the text on ([`exo_ui::text_drag`]) with the same
//! picture, into a field or another editor of the window (a move), another app (a copy), or back.
//! Text dragged over the editor from elsewhere shows the caret where it would go, and goes in
//! there when let go.

use egui::{
    Color32, ColorImage, CornerRadius, CursorIcon, Id, Key, LayerId, Modifiers, Order, Painter,
    Pos2, Rect, TextureId, Ui, Vec2,
    epaint::{ClippedPrimitive, Primitive, Vertex},
    pos2,
};

use crate::{
    editor::Editor,
    model::{Pos, Selection},
};

/// How far the pointer moves with the button down before a press on the selection drags it
/// (`kDragThresholdX`, `kDragThresholdY`).
const THRESHOLD: f32 = 4.0;
/// The drag image's opacity (`kDragImageAlpha`).
const IMAGE_ALPHA: f32 = 0.75;

/// A press on the selection, and the drag it became.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextDrag {
    /// Where the button went down.
    press: Pos2,
    /// Whether the pointer has gone past the threshold, so the text is being dragged.
    dragging: bool,
    /// Where the text would land now, or nowhere when the pointer is off the editor or over the
    /// selection.
    drop: Option<Pos>,
    /// Whether the drag has left the editor, so the Mac's drag has it until the app says it's
    /// over.
    native: bool,
}

impl TextDrag {
    pub(super) fn new(press: Pos2) -> Self {
        Self {
            press,
            dragging: false,
            drop: None,
            native: false,
        }
    }

    /// Whether the selected text is being dragged inside the editor (Escape is the drag's).
    pub(crate) fn dragging(&self) -> bool {
        self.dragging && !self.native
    }
}

impl Editor {
    /// Follows a press on the selection while the button's down, for editor `id` at `rect`.
    pub(super) fn drag_text(&mut self, ui: &Ui, rect: Rect, id: Id) {
        let Some(mut drag) = self.view.text_drag.take() else {
            return;
        };
        let ctx = ui.ctx();
        if drag.native {
            match exo_ui::text_drag::ended(ctx, id) {
                // Moved to a field or another editor of the window, so the text goes from here.
                Some(true) => self.delete_dragged(),
                Some(false) => {}
                None => self.view.text_drag = Some(drag),
            }
            return;
        }
        let (down, point, copy) = ui.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.latest_pos(),
                input.modifiers.alt,
            )
        });
        if !drag.dragging
            && point.is_some_and(|at| {
                (at.x - drag.press.x).abs() > THRESHOLD || (at.y - drag.press.y).abs() > THRESHOLD
            })
        {
            drag.dragging = true;
        }
        if drag.dragging {
            if ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
                ui.ctx().request_repaint();
                return;
            }
            // Out of the editor, the Mac's drag carries the text on.
            let visible = rect.intersect(ui.clip_rect());
            if exo_ui::text_drag::native(ctx)
                && let Some(at) = point.filter(|at| !visible.contains(*at))
            {
                let image = self.drag_image(ui, rect.min, at - drag.press);
                exo_ui::text_drag::drag_out(
                    ctx,
                    exo_ui::text_drag::Outgoing {
                        source: id,
                        html: self.selection_markup(),
                        text: self.selected_text(),
                        image,
                    },
                );
                drag.native = true;
                drag.drop = None;
                self.view.text_drag = Some(drag);
                return;
            }
            // Not over the selection itself (`result.IsSelected()`).
            let (start, end) = (self.selection.start(), self.selection.end());
            drag.drop = point
                .filter(|at| rect.contains(*at))
                .map(|at| self.hit(rect.min, at))
                .filter(|pos| !(start <= *pos && *pos <= end));
            if let Some(at) = point {
                ui.scroll_to_rect(
                    Rect::from_center_size(at, Vec2::splat(self.view.body.line())),
                    None,
                );
            }
            ui.ctx().set_cursor_icon(if copy {
                CursorIcon::Copy
            } else {
                CursorIcon::Default
            });
        }
        if down {
            self.view.text_drag = Some(drag);
            return;
        }
        if !drag.dragging {
            self.close_toolbar();
            self.set_selection(Selection::caret(self.hit(rect.min, drag.press)));
        } else if let Some(at) = drag.drop {
            let smart = self.view.word_selection == Some(self.selection);
            self.drop_selection(at, copy, smart);
        }
    }

    /// Follows text another drag has over the editor (`id`, at `rect`). Shows the caret where the
    /// text would go, or none over the selection when the text is this editor's, and puts the text
    /// in there when it's let go. Returns whether it was.
    pub(super) fn drag_over(&mut self, ui: &Ui, rect: Rect, id: Id) -> bool {
        let ctx = ui.ctx();
        let visible = rect.intersect(ui.clip_rect());
        self.view.drag_over = None;
        if !ui.is_enabled() {
            return false;
        }
        exo_ui::text_drag::accept(ctx, visible);
        let (start, end) = (self.selection.start(), self.selection.end());
        if let Some(over) = exo_ui::text_drag::over(ctx).filter(|over| visible.contains(over.at)) {
            let at = self.hit(rect.min, over.at);
            self.view.drag_over =
                Some(at).filter(|at| over.source != Some(id) || !(start <= *at && *at <= end));
            ui.scroll_to_rect(
                Rect::from_center_size(over.at, Vec2::splat(self.view.body.line())),
                None,
            );
        }
        let Some(dropped) = exo_ui::text_drag::take_drop(ctx, visible, id) else {
            return false;
        };
        let at = self.hit(rect.min, dropped.at);
        self.close_toolbar();
        if dropped.source == Some(id) {
            let smart = self.view.word_selection == Some(self.selection);
            self.drop_selection(at, dropped.copy, smart);
        } else {
            self.drop_text(at, dropped.html.as_deref(), &dropped.text);
        }
        true
    }

    /// Paints the dragged text's picture under the pointer (the selected text without its
    /// highlight, moved with the pointer, at 75%) and the caret where it would land, for an editor
    /// at `origin`. The Mac's drag draws the picture once it's out of the editor.
    pub(super) fn paint_text_drag(&self, ui: &Ui, origin: Pos2, text: Color32) {
        let drag = self.view.text_drag.filter(|drag| drag.dragging);
        if let Some(at) = drag.and_then(|drag| drag.drop).or(self.view.drag_over) {
            ui.painter().rect_filled(
                self.caret_rect_at(origin, ui.pixels_per_point(), at),
                CornerRadius::ZERO,
                text,
            );
        }
        let Some(drag) = drag.filter(|drag| !drag.native) else {
            return;
        };
        let Some(point) = ui.input(|input| input.pointer.latest_pos()) else {
            return;
        };
        let mut painter = ui.ctx().layer_painter(LayerId::new(
            Order::Tooltip,
            ui.id().with("exo_editor_drag"),
        ));
        painter.multiply_opacity(IMAGE_ALPHA);
        self.paint_dragged(&painter, origin, point - drag.press);
    }

    /// Paints the selected text, moved by `delta`, for an editor at `origin`. Returns what it
    /// covers.
    fn paint_dragged(&self, painter: &Painter, origin: Pos2, delta: Vec2) -> Option<Rect> {
        let layout = self.view.layout.as_ref()?;
        let mut covered: Option<Rect> = None;
        for (highlight, _) in self.selection_rects(origin) {
            let clip = highlight.translate(delta).intersect(painter.clip_rect());
            covered = Some(covered.map_or(clip, |covered| covered.union(clip)));
            let painter = painter.with_clip_rect(clip);
            for view in layout.blocks.iter().filter(|view| {
                (origin.y + view.top..=origin.y + view.top + view.height)
                    .contains(&highlight.center().y)
            }) {
                let top_left = origin + Vec2::new(view.left, view.top) + delta;
                let x =
                    (top_left.x * painter.pixels_per_point()).floor() / painter.pixels_per_point();
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
            }
        }
        covered.filter(|covered| covered.is_positive())
    }

    /// The picture [`Self::paint_text_drag`] draws of what shows of the selection (as Chromium's
    /// `DragImageForSelection` is), for the Mac's drag. It's painted, then drawn into an image as
    /// the GPU would draw it, and comes with where it is in the window.
    fn drag_image(&self, ui: &Ui, origin: Pos2, delta: Vec2) -> Option<(ColorImage, Rect)> {
        let ctx = ui.ctx();
        let layer = LayerId::new(Order::Debug, exo_ui::id(ctx, "exo_editor_drag_image"));
        let mut painter = ctx
            .layer_painter(layer)
            .with_clip_rect(ui.clip_rect().translate(delta));
        painter.multiply_opacity(IMAGE_ALPHA);
        let covered = self.paint_dragged(&painter, origin, delta);
        // Taken out again, because it's only for the picture.
        let shapes = ctx
            .graphics_mut(|graphics| graphics.get_mut(layer).map(std::mem::take))
            .map(|list| list.all_entries().cloned().collect())
            .unwrap_or_default();
        let covered = covered?;
        let pixels_per_point = ctx.pixels_per_point();
        let primitives = ctx.tessellate(shapes, pixels_per_point);
        let atlas = ctx.fonts(|fonts| fonts.image());
        Some(rasterize(&primitives, &atlas, covered, pixels_per_point))
    }
}

/// Draws the font texture's triangles of `primitives` into a transparent picture of `bounds`, at
/// `pixels_per_point`, as egui's renderer would. Each pixel gets the premultiplied vertex colour
/// times the texel nearest its centre, blended over what's there, with a direct UV scan over
/// axis-aligned glyph and rect quads. Emoji and images, drawn from other textures, are left out.
/// Returns the picture and the rect it covers, on whole pixels.
fn rasterize(
    primitives: &[ClippedPrimitive],
    atlas: &ColorImage,
    bounds: Rect,
    pixels_per_point: f32,
) -> (ColorImage, Rect) {
    let (left, top) = (
        (bounds.min.x * pixels_per_point).floor(),
        (bounds.min.y * pixels_per_point).floor(),
    );
    let offset = Vec2::new(left, top);
    let (width, height) = (
        ((bounds.max.x * pixels_per_point).ceil() - left).max(1.0) as usize,
        ((bounds.max.y * pixels_per_point).ceil() - top).max(1.0) as usize,
    );
    let mut pixels = vec![Color32::TRANSPARENT; width * height];
    for primitive in primitives {
        let Primitive::Mesh(mesh) = &primitive.primitive else {
            continue;
        };
        if mesh.texture_id != TextureId::default() {
            continue;
        }
        let clip = Rect::from_min_max(
            primitive.clip_rect.min * pixels_per_point - offset,
            primitive.clip_rect.max * pixels_per_point - offset,
        );
        let mut index = 0;
        while index + 3 <= mesh.indices.len() {
            if index + 6 <= mesh.indices.len()
                && let Some(quad) = Quad::from_indices(
                    &mesh.vertices,
                    &mesh.indices[index..index + 6],
                    pixels_per_point,
                    offset,
                )
            {
                rasterize_quad(&quad, clip, atlas, &mut pixels, width, height);
                index += 6;
                continue;
            }
            let triangle = [
                mesh.indices[index],
                mesh.indices[index + 1],
                mesh.indices[index + 2],
            ];
            rasterize_triangle(
                &mesh.vertices,
                triangle,
                pixels_per_point,
                offset,
                clip,
                atlas,
                &mut pixels,
                width,
                height,
            );
            index += 3;
        }
    }
    let covers = Rect::from_min_size(
        pos2(left, top) / pixels_per_point,
        Vec2::new(width as f32, height as f32) / pixels_per_point,
    );
    (ColorImage::new([width, height], pixels), covers)
}

/// An axis-aligned textured rectangle of one vertex colour, as egui's text glyphs and rects emit.
struct Quad {
    rect: Rect,
    uv_min: Pos2,
    uv_max: Pos2,
    color: [f32; 4],
}

impl Quad {
    fn from_indices(
        vertices: &[Vertex],
        indices: &[u32],
        scale: f32,
        offset: Vec2,
    ) -> Option<Self> {
        let &[i0, i1, i2, i3, i4, i5] = indices else {
            return None;
        };
        let corners = if i3 == i0 && i4 == i2 {
            [i0, i1, i2, i5]
        } else if i3 == i0 && i5 == i1 {
            [i0, i4, i1, i2]
        } else if (i3 == i2 && i4 == i1) || (i3 == i1 && i5 == i2) {
            [i0, i1, if i3 == i2 { i5 } else { i4 }, i2]
        } else {
            return None;
        };
        let v = [
            vertices.get(corners[0] as usize)?,
            vertices.get(corners[1] as usize)?,
            vertices.get(corners[2] as usize)?,
            vertices.get(corners[3] as usize)?,
        ];
        if v[0].color != v[1].color || v[0].color != v[2].color || v[0].color != v[3].color {
            return None;
        }
        let min_corner = (0..4).min_by(|&a, &b| {
            (v[a].pos.x, v[a].pos.y)
                .partial_cmp(&(v[b].pos.x, v[b].pos.y))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let tl = v[min_corner];
        let br = v[(min_corner + 2) % 4];
        let (adj1, adj2) = (v[(min_corner + 1) % 4], v[(min_corner + 3) % 4]);
        let (tr, bl) = if (tl.pos.y - adj1.pos.y).abs() <= f32::EPSILON {
            (adj1, adj2)
        } else {
            (adj2, adj1)
        };
        let aligned = (tl.pos.y - tr.pos.y).abs() <= f32::EPSILON
            && (tr.pos.x - br.pos.x).abs() <= f32::EPSILON
            && (br.pos.y - bl.pos.y).abs() <= f32::EPSILON
            && (bl.pos.x - tl.pos.x).abs() <= f32::EPSILON
            && (tl.uv.y - tr.uv.y).abs() <= f32::EPSILON
            && (tr.uv.x - br.uv.x).abs() <= f32::EPSILON
            && (br.uv.y - bl.uv.y).abs() <= f32::EPSILON
            && (bl.uv.x - tl.uv.x).abs() <= f32::EPSILON;
        if !aligned {
            return None;
        }
        let rect = Rect::from_min_max(tl.pos * scale - offset, br.pos * scale - offset);
        if rect.width() < f32::EPSILON || rect.height() < f32::EPSILON {
            return None;
        }
        Some(Self {
            rect,
            uv_min: tl.uv,
            uv_max: br.uv,
            color: tl.color.to_array().map(f32::from),
        })
    }
}

fn rasterize_quad(
    quad: &Quad,
    clip: Rect,
    atlas: &ColorImage,
    pixels: &mut [Color32],
    width: usize,
    height: usize,
) {
    let [atlas_width, atlas_height] = atlas.size;
    let x0 = (quad.rect.min.x - 0.5)
        .ceil()
        .max(clip.min.x.floor())
        .max(0.0) as usize;
    let x1 = (quad.rect.max.x - 0.5)
        .ceil()
        .min(clip.max.x.ceil())
        .min(width as f32)
        .max(0.0) as usize;
    let y0 = (quad.rect.min.y - 0.5)
        .ceil()
        .max(clip.min.y.floor())
        .max(0.0) as usize;
    let y1 = (quad.rect.max.y - 0.5)
        .ceil()
        .min(clip.max.y.ceil())
        .min(height as f32)
        .max(0.0) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let du = (quad.uv_max.x - quad.uv_min.x) / quad.rect.width();
    let dv = (quad.uv_max.y - quad.uv_min.y) / quad.rect.height();
    for y in y0..y1 {
        let v = (quad.uv_min.y + (y as f32 + 0.5 - quad.rect.min.y) * dv).max(0.0);
        let texel_y = ((v * atlas_height as f32) as usize).min(atlas_height - 1);
        let atlas_row = &atlas.pixels[texel_y * atlas_width..(texel_y + 1) * atlas_width];
        let out_row = &mut pixels[y * width..(y + 1) * width];
        for (x, dst) in out_row.iter_mut().enumerate().take(x1).skip(x0) {
            let u = (quad.uv_min.x + (x as f32 + 0.5 - quad.rect.min.x) * du).max(0.0);
            let texel_x = ((u * atlas_width as f32) as usize).min(atlas_width - 1);
            *dst = blend_over(*dst, quad.color, atlas_row[texel_x]);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fallback rasterizer's mesh, clip and target buffer"
)]
fn rasterize_triangle(
    vertices: &[Vertex],
    triangle: [u32; 3],
    scale: f32,
    offset: Vec2,
    clip: Rect,
    atlas: &ColorImage,
    pixels: &mut [Color32],
    width: usize,
    height: usize,
) {
    let [atlas_width, atlas_height] = atlas.size;
    let [a, b, c] = triangle.map(|corner| &vertices[corner as usize]);
    let [pa, pb, pc] = [a, b, c].map(|vertex| vertex.pos * scale - offset);
    let area = (pb - pa).x * (pc - pa).y - (pb - pa).y * (pc - pa).x;
    if area.abs() < f32::EPSILON {
        return;
    }
    let x_range = (pa.x.min(pb.x).min(pc.x).max(clip.min.x).floor().max(0.0) as usize)
        ..(pa
            .x
            .max(pb.x)
            .max(pc.x)
            .min(clip.max.x)
            .ceil()
            .min(width as f32)
            .max(0.0) as usize);
    let y_range = (pa.y.min(pb.y).min(pc.y).max(clip.min.y).floor().max(0.0) as usize)
        ..(pa
            .y
            .max(pb.y)
            .max(pc.y)
            .min(clip.max.y)
            .ceil()
            .min(height as f32)
            .max(0.0) as usize);
    let colors = [a, b, c].map(|vertex| vertex.color.to_array());
    for y in y_range {
        for x in x_range.clone() {
            // Just off the centre, so a centre on the edge two triangles share (a square quad's
            // diagonal) is drawn once.
            let p = pos2(x as f32 + 0.5 + 1e-3, y as f32 + 0.5 + 2e-3);
            let edge = |from: Pos2, to: Pos2| {
                ((to - from).x * (p - from).y - (to - from).y * (p - from).x) / area
            };
            let (wa, wb, wc) = (edge(pb, pc), edge(pc, pa), edge(pa, pb));
            if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                continue;
            }
            let uv = a.uv.to_vec2() * wa + b.uv.to_vec2() * wb + c.uv.to_vec2() * wc;
            let texel_x = ((uv.x.max(0.0) * atlas_width as f32) as usize).min(atlas_width - 1);
            let texel_y = ((uv.y.max(0.0) * atlas_height as f32) as usize).min(atlas_height - 1);
            let vertex_color: [f32; 4] = std::array::from_fn(|channel| {
                f32::from(colors[0][channel]) * wa
                    + f32::from(colors[1][channel]) * wb
                    + f32::from(colors[2][channel]) * wc
            });
            let dst = &mut pixels[y * width + x];
            *dst = blend_over(
                *dst,
                vertex_color,
                atlas.pixels[texel_y * atlas_width + texel_x],
            );
        }
    }
}

fn blend_over(under: Color32, vertex: [f32; 4], texel: Color32) -> Color32 {
    let texel = texel.to_array();
    let source: [f32; 4] =
        std::array::from_fn(|channel| vertex[channel] * f32::from(texel[channel]) / 255.0);
    let under = under.to_array();
    let over: [u8; 4] = std::array::from_fn(|channel| {
        (source[channel] + f32::from(under[channel]) * (1.0 - source[3] / 255.0))
            .round()
            .clamp(0.0, 255.0) as u8
    });
    Color32::from_rgba_premultiplied(over[0], over[1], over[2], over[3])
}

#[cfg(test)]
mod tests {
    use egui::{Mesh, Rect, epaint::Vertex, pos2, vec2};

    use super::*;

    #[test]
    fn a_textured_quad_is_drawn_with_its_texels_times_its_colour() {
        // A 2×1 atlas: an opaque texel and a clear one.
        let atlas = ColorImage::new([2, 1], vec![Color32::WHITE, Color32::TRANSPARENT]);
        let mut mesh = Mesh::default();
        let red = Color32::from_rgba_premultiplied(128, 0, 0, 128);
        let corners = [
            (pos2(0.0, 0.0), pos2(0.0, 0.0)),
            (pos2(2.0, 0.0), pos2(0.5, 0.0)),
            (pos2(2.0, 2.0), pos2(0.5, 1.0)),
            (pos2(0.0, 2.0), pos2(0.0, 1.0)),
        ];
        mesh.vertices = corners
            .iter()
            .map(|&(pos, uv)| Vertex {
                pos,
                uv,
                color: red,
            })
            .collect();
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        let primitive = ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Mesh(mesh),
        };
        let (image, covers) = rasterize(
            &[primitive],
            &atlas,
            Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 2.0)),
            1.0,
        );
        assert_eq!(image.size, [4, 2]);
        assert_eq!(covers, Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 2.0)));
        assert_eq!(image.pixels[0], red, "the opaque texel times the colour");
        assert_eq!(image.pixels[3], Color32::TRANSPARENT, "outside the quad");
    }

    #[test]
    fn egui_rects_hit_the_quad_fast_path_and_non_axis_aligned_triangles_fall_back() {
        let mut mesh = Mesh::default();
        let blue = Color32::from_rgba_premultiplied(0, 0, 200, 200);
        mesh.add_rect_with_uv(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(2.0, 2.0)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            blue,
        );
        assert!(
            Quad::from_indices(&mesh.vertices, &mesh.indices, 1.0, Vec2::ZERO).is_some(),
            "egui's add_rect_with_uv indices take the quad fast path"
        );

        let atlas = ColorImage::new([1, 1], vec![Color32::WHITE]);
        let tri = Mesh {
            vertices: vec![
                Vertex {
                    pos: pos2(0.0, 0.0),
                    uv: pos2(0.0, 0.0),
                    color: blue,
                },
                Vertex {
                    pos: pos2(3.0, 0.0),
                    uv: pos2(0.0, 0.0),
                    color: blue,
                },
                Vertex {
                    pos: pos2(0.0, 3.0),
                    uv: pos2(0.0, 0.0),
                    color: blue,
                },
            ],
            indices: vec![0, 1, 2],
            ..Mesh::default()
        };
        let primitive = ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Mesh(tri),
        };
        let (image, _) = rasterize(
            &[primitive],
            &atlas,
            Rect::from_min_size(pos2(0.0, 0.0), vec2(3.0, 3.0)),
            1.0,
        );
        assert_eq!(image.pixels[0], blue, "inside the triangle");
        assert_eq!(
            image.pixels[8],
            Color32::TRANSPARENT,
            "outside its hypotenuse"
        );
    }
}
