//! What a see-through card shows of the window behind it, blurred (CSS's `backdrop-filter:
//! blur()`). egui can't read back what it has painted, so the screen that paints a photo behind
//! the window says what's there, and each card blurs its own part of it.
//!
//! As a browser does, a card blurs only what's behind itself, mirrored at its edges, so nothing
//! from beyond them bleeds in. The blur is made at a third of its radius to a texel, which bilinear
//! sampling draws smoothly, and kept while the card and what's behind it stay put.

use std::sync::Arc;

use egui::{Color32, ColorImage, Context, Id, Mesh, Painter, Rect, Shape, TextureHandle, pos2};

/// Texels in a blur's radius.
const TEXELS_PER_RADIUS: f32 = 3.0;

/// What's behind a region of the window, as colors in a grid.
type Sample = dyn Fn(Rect, [usize; 2]) -> Option<Vec<[f32; 3]>> + Send + Sync;

/// What's behind the window's see-through cards.
#[derive(Clone)]
pub struct Behind {
    /// Tells one backdrop from another, so a card's blur is made again only when it changes.
    key: u64,
    sample: Arc<Sample>,
}

impl Behind {
    /// What's behind, which `key` identifies. `sample(region, [columns, rows])` gives the mean
    /// color (sRGB, 0 to 255) of each cell of `region` cut into that grid, row by row, or None
    /// while there's nothing to show.
    pub fn new(
        key: u64,
        sample: impl Fn(Rect, [usize; 2]) -> Option<Vec<[f32; 3]>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            key,
            sample: Arc::new(sample),
        }
    }

    /// Paints what's behind `card` blurred by `radius` points, inside it.
    pub fn paint(&self, painter: &Painter, card: Rect, radius: f32) {
        let ctx = painter.ctx();
        let Some(texture) = self.blurred(ctx, card, radius) else {
            return;
        };
        let mut mesh = Mesh::with_texture(texture.id());
        mesh.add_rect_with_uv(
            card,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.add(Shape::mesh(mesh));
    }

    /// The blur of what's behind `card`, made now unless it was last pass.
    fn blurred(&self, ctx: &Context, card: Rect, radius: f32) -> Option<TextureHandle> {
        let id = crate::id(ctx, "exo_ui::behind");
        let pass = ctx.cumulative_pass_nr();
        let key = (
            self.key,
            [card.min.x, card.min.y, card.max.x, card.max.y, radius].map(f32::to_bits),
        );
        let mut made: Vec<Blurred> = ctx.data_mut(|data| data.remove_temp(id).unwrap_or_default());
        // A blur goes once its card hasn't been painted for a pass.
        made.retain(|blurred| blurred.used + 1 >= pass);
        let texture = if let Some(blurred) = made.iter_mut().find(|blurred| blurred.key == key) {
            blurred.used = pass;
            Some(blurred.texture.clone())
        } else {
            self.blur(card, radius).map(|image| {
                let texture = ctx.load_texture("behind", image, egui::TextureOptions::LINEAR);
                made.push(Blurred {
                    key,
                    texture: texture.clone(),
                    used: pass,
                });
                texture
            })
        };
        ctx.data_mut(|data| data.insert_temp(id, made));
        texture
    }

    /// What's behind `card` at a third of `radius` to a texel, blurred by `radius`.
    fn blur(&self, card: Rect, radius: f32) -> Option<ColorImage> {
        let texel = radius / TEXELS_PER_RADIUS;
        let columns = ((card.width() / texel).ceil() as usize).max(1);
        let rows = ((card.height() / texel).ceil() as usize).max(1);
        let mut cells = (self.sample)(card, [columns, rows])?;
        if cells.len() != columns * rows {
            return None;
        }
        // The cells are a little under `texel` across, to fit the card.
        let sigma_x = radius * columns as f32 / card.width();
        let sigma_y = radius * rows as f32 / card.height();
        blur_rows(&mut cells, columns, rows, sigma_x);
        transpose(&mut cells, columns, rows);
        blur_rows(&mut cells, rows, columns, sigma_y);
        transpose(&mut cells, rows, columns);
        let bytes: Vec<u8> = cells
            .iter()
            .flat_map(|cell| cell.map(|channel| channel.round().clamp(0.0, 255.0) as u8))
            .collect();
        Some(ColorImage::from_rgb([columns, rows], &bytes))
    }
}

/// A card's blur, and the pass it was last painted in.
#[derive(Clone)]
struct Blurred {
    key: (u64, [u32; 5]),
    texture: TextureHandle,
    used: u64,
}

/// Blurs each of `rows` rows of `columns` cells with a Gaussian of `sigma` cells, mirroring the
/// row at its ends.
fn blur_rows(cells: &mut [[f32; 3]], columns: usize, rows: usize, sigma: f32) {
    let reach = (3.0 * sigma).ceil() as isize;
    let weights: Vec<f32> = (-reach..=reach)
        .map(|offset| (-(offset * offset) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let mut row = vec![[0.0; 3]; columns];
    for y in 0..rows {
        row.copy_from_slice(&cells[y * columns..(y + 1) * columns]);
        for x in 0..columns {
            let mut sum = [0.0; 3];
            for (offset, weight) in (-reach..=reach).zip(&weights) {
                let cell = row[mirror(x as isize + offset, columns)];
                for (sum, channel) in sum.iter_mut().zip(cell) {
                    *sum += weight * channel;
                }
            }
            cells[y * columns + x] = sum.map(|sum| sum / total);
        }
    }
}

/// Index `i` of a row `length` long that repeats mirrored past its ends (`… c b a | a b c | c b
/// a …`).
fn mirror(i: isize, length: usize) -> usize {
    let period = 2 * length as isize;
    let i = i.rem_euclid(period) as usize;
    if i < length {
        i
    } else {
        period as usize - 1 - i
    }
}

/// Turns `rows` rows of `columns` cells into `columns` rows of `rows` cells.
fn transpose(cells: &mut Vec<[f32; 3]>, columns: usize, rows: usize) {
    let mut turned = Vec::with_capacity(cells.len());
    for x in 0..columns {
        for y in 0..rows {
            turned.push(cells[y * columns + x]);
        }
    }
    *cells = turned;
}

/// The toasts' stack (toasts, file notifications, popups) over a photo shows through.
impl Behind {
    /// Says what's behind the toasts' stack this frame: something it shows through (a photo),
    /// or nothing, which leaves its cards opaque.
    pub fn set_for_toasts(ctx: &Context, behind: Option<Self>) {
        let id = toasts_id(ctx);
        ctx.data_mut(|data| data.insert_temp(id, behind));
    }

    /// What's behind the toasts' stack this frame, if it shows through.
    pub fn for_toasts(ctx: &Context) -> Option<Self> {
        let id = toasts_id(ctx);
        ctx.data(|data| data.get_temp::<Option<Self>>(id)).flatten()
    }
}

fn toasts_id(ctx: &Context) -> Id {
    crate::id(ctx, "exo_ui::behind_toasts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_mirror_at_their_ends() {
        assert_eq!(
            (-4..8).map(|i| mirror(i, 3)).collect::<Vec<_>>(),
            [2, 2, 1, 0, 0, 1, 2, 2, 1, 0, 0, 1]
        );
    }

    #[test]
    fn a_blur_keeps_a_flat_backdrop_flat_and_mirrors_an_edge() {
        let mut flat = vec![[100.0; 3]; 12];
        blur_rows(&mut flat, 4, 3, 2.0);
        assert!(flat.iter().flatten().all(|c| (c - 100.0).abs() < 1e-3));
        // A step at the row's start: what's past the end mirrors what's inside, so the edge
        // keeps its own color rather than fading to black.
        let mut step = vec![
            [255.0; 3], [255.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
        ];
        blur_rows(&mut step, 6, 1, 1.0);
        assert!(step[0][0] > 200.0, "{step:?}");
        assert!(step[5][0] < 5.0, "{step:?}");
    }

    #[test]
    fn toasts_backdrop_is_scoped_per_viewport() {
        let ctx = Context::default();
        let main_vp = egui::ViewportId::ROOT;
        let child_vp = egui::ViewportId::from_hash_of("popout-compose");

        ctx.run_ui(
            egui::RawInput {
                viewport_id: main_vp,
                ..Default::default()
            },
            |ui| {
                Behind::set_for_toasts(ui.ctx(), Some(Behind::new(7, |_, _| None)));
                assert_eq!(Behind::for_toasts(ui.ctx()).map(|b| b.key), Some(7));
            },
        )
        .drop_without_applying_deltas();

        ctx.run_ui(
            egui::RawInput {
                viewport_id: child_vp,
                viewports: [(child_vp, egui::ViewportInfo::default())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
            |ui| {
                assert!(
                    Behind::for_toasts(ui.ctx()).is_none(),
                    "child viewport must not inherit main viewport's toast backdrop"
                );
                Behind::set_for_toasts(ui.ctx(), Some(Behind::new(42, |_, _| None)));
                assert_eq!(Behind::for_toasts(ui.ctx()).map(|b| b.key), Some(42));
            },
        )
        .drop_without_applying_deltas();

        ctx.run_ui(
            egui::RawInput {
                viewport_id: main_vp,
                ..Default::default()
            },
            |ui| {
                assert_eq!(
                    Behind::for_toasts(ui.ctx()).map(|b| b.key),
                    Some(7),
                    "main viewport retains its own toast backdrop"
                );
            },
        )
        .drop_without_applying_deltas();
    }
}
