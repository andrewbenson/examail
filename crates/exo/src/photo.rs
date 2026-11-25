//! Full-window photos (the sign-in screen's, the full-screen alerts'), decoded off the UI thread
//! and painted the way CSS paints `background-size: cover`.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use egui::{Color32, Mesh, Painter, Rect, Shape, TextureHandle, Vec2, pos2, vec2};
use image::RgbImage;

/// The largest texture side every backend takes (egui assumes no more before the first frame).
/// The photos are bigger, so they're cut into tiles.
const MAX_SIDE: u32 = 2048;

/// A photo that is being decoded, or has been. A 4096px JPEG takes a moment (65ms in a release
/// build, two seconds in a debug one).
pub struct Photo {
    tiles: Arc<OnceLock<Option<Tiles>>>,
}

/// The photos decoded or being decoded, keyed by file, whether with a blurred copy, and the
/// context whose textures hold them (its texture manager's address, kept alive by the textures).
/// Every screen showing a photo shares it (each tab's page has the day's photo behind Inbox
/// Zero).
type Decoded = Vec<((PathBuf, bool, usize), Weak<OnceLock<Option<Tiles>>>)>;
static DECODED: Mutex<Decoded> = Mutex::new(Vec::new());

/// A photo as textures no bigger than [`MAX_SIDE`].
struct Tiles {
    size: Vec2,
    /// A small blurred copy, for a photo asked for with one.
    blurred: Option<TextureHandle>,
    /// A small copy in memory, for [`Photo::sample`], likewise.
    small: Option<Arc<RgbImage>>,
    /// Each tile owns a range of the photo's columns and rows, and its texture holds one more
    /// texel on each inner side, so sampling at the seam blends the same texels as one big
    /// texture would.
    tiles: Vec<Tile>,
}

/// Decoded pixel buffers shared across [`egui::Context`]s (such as compose pop-out windows and
/// concurrent harnesses) so the same 4096px JPEG is not decoded repeatedly.
struct Pixels {
    rgb: RgbImage,
    small: Option<Arc<RgbImage>>,
    blurred: Option<RgbImage>,
}

type PixelKey = (PathBuf, u64, Option<std::time::SystemTime>, bool);
type PixelSlot = Arc<OnceLock<Option<Arc<Pixels>>>>;
type PixelCache = Vec<(PixelKey, PixelSlot)>;
static PIXELS: Mutex<PixelCache> = Mutex::new(Vec::new());
const MAX_CACHED_PIXELS: usize = 4;

struct Tile {
    texture: TextureHandle,
    /// The texels it paints, in photo pixels.
    owned: Rect,
    /// The texels its texture holds.
    held: Rect,
}

impl Photo {
    /// Starts decoding the JPEG at `path` into textures of `ctx`, which repaints when it's done.
    pub fn load(ctx: &egui::Context, path: PathBuf) -> Self {
        Self::start(ctx, path, false)
    }

    /// [`Photo::load`], with a blurred copy too ([`Photo::paint_blurred`]).
    pub fn load_blurred(ctx: &egui::Context, path: PathBuf) -> Self {
        Self::start(ctx, path, true)
    }

    fn start(ctx: &egui::Context, path: PathBuf, blurred: bool) -> Self {
        let key = (
            path.clone(),
            blurred,
            Arc::as_ptr(&ctx.tex_manager()) as usize,
        );
        let mut decoded = DECODED
            .lock()
            .expect("the photo cache's lock is never poisoned");
        decoded.retain(|(_, tiles)| tiles.strong_count() > 0);
        if let Some(tiles) = decoded
            .iter()
            .find(|(known, _)| *known == key)
            .and_then(|(_, tiles)| tiles.upgrade())
        {
            return Self { tiles };
        }
        let tiles = Arc::new(OnceLock::new());
        decoded.push((key, Arc::downgrade(&tiles)));
        drop(decoded);
        let (done, ctx) = (tiles.clone(), ctx.clone());
        let decode_job = move || {
            // Set whatever happens, for the screens sharing it that wait ([`Photo::wait`]).
            let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                decode(&ctx, &path, blurred)
            }));
            let _ = done.set(decoded.ok().flatten());
            ctx.request_repaint();
        };
        let mut photo = Self { tiles };
        match crate::account::shared_runtime() {
            Ok(runtime) => {
                runtime.spawn_blocking(decode_job);
            }
            Err(error) => {
                log::error!("decoding a photo: {error}");
                photo.give_up();
            }
        }
        photo
    }

    /// Another handle on the same photo, for painting it from elsewhere.
    pub fn share(&self) -> Self {
        Self {
            tiles: self.tiles.clone(),
        }
    }

    /// Blocks until decoding is over, here or for another screen sharing the photo, for launch
    /// before the window is revealed and for headless screenshot harnesses.
    pub fn wait(&mut self) {
        self.tiles.wait();
    }

    fn give_up(&mut self) {
        let _ = self.tiles.set(None);
    }

    /// Whether decoding is over, whether or not it worked. For screenshots and tests.
    pub fn is_loaded(&self) -> bool {
        self.tiles.get().is_some()
    }

    /// Paints the photo scaled to cover `rect`, placed by `position`. Given how much wider and
    /// taller the scaled photo is than `rect`, `position` says how far left and up to shift it
    /// (CSS's `background-position`). Paints nothing until it's decoded.
    pub fn paint(&self, painter: &Painter, rect: Rect, position: impl FnOnce(Vec2) -> Vec2) {
        let Some(Some(photo)) = self.tiles.get() else {
            return;
        };
        let scale = (rect.width() / photo.size.x).max(rect.height() / photo.size.y);
        let origin = rect.min - position(photo.size * scale - rect.size());
        let on_screen = |texels: Rect| {
            Rect::from_min_max(
                origin + texels.min.to_vec2() * scale,
                origin + texels.max.to_vec2() * scale,
            )
        };
        for tile in &photo.tiles {
            let uv = Rect::from_min_max(
                ((tile.owned.min - tile.held.min) / tile.held.size()).to_pos2(),
                ((tile.owned.max - tile.held.min) / tile.held.size()).to_pos2(),
            );
            // A bare mesh, because a rect shape would feather its edges, which shows at the seams.
            let mut mesh = Mesh::with_texture(tile.texture.id());
            mesh.add_rect_with_uv(on_screen(tile.owned), uv, Color32::WHITE);
            painter.add(Shape::mesh(mesh));
        }
    }
}

/// The blurred copy's width, an eighth of a 2880px photo, which a window of half its width shows
/// at twice that size, so a blur of `BLUR` texels is CSS's `blur(8px)` there.
const BLURRED_WIDTH: u32 = 360;
const BLUR: f32 = 2.0;
/// The copy in memory's width, a quarter of a 2880px photo: two pixels to a point in a window of
/// half its width.
const SMALL_WIDTH: u32 = 720;

impl Photo {
    /// Paints the blurred copy as [`Photo::paint`] paints the photo (a photo loaded with
    /// [`Photo::load_blurred`]; nothing otherwise).
    pub fn paint_blurred(
        &self,
        painter: &Painter,
        rect: Rect,
        position: impl FnOnce(Vec2) -> Vec2,
    ) {
        let Some(Some(photo)) = self.tiles.get() else {
            return;
        };
        let Some(texture) = &photo.blurred else {
            return;
        };
        let scale = (rect.width() / photo.size.x).max(rect.height() / photo.size.y);
        let origin = rect.min - position(photo.size * scale - rect.size());
        let mut mesh = Mesh::with_texture(texture.id());
        mesh.add_rect_with_uv(
            Rect::from_min_size(origin, photo.size * scale),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter
            .with_clip_rect(rect.intersect(painter.clip_rect()))
            .add(Shape::mesh(mesh));
    }
}

impl Photo {
    /// The photo as [`Photo::paint`] paints it over `rect`, in `region` of the screen cut into
    /// `columns` × `rows` cells: each cell's mean color, row by row, from the copy in memory of a
    /// photo loaded with [`Photo::load_blurred`]. None for another photo, or before it's decoded.
    pub fn sample(
        &self,
        rect: Rect,
        position: impl FnOnce(Vec2) -> Vec2,
        region: Rect,
        [columns, rows]: [usize; 2],
    ) -> Option<Vec<[f32; 3]>> {
        let photo = self.tiles.get()?.as_ref()?;
        let small = photo.small.as_ref()?;
        let scale = (rect.width() / photo.size.x).max(rect.height() / photo.size.y);
        let origin = rect.min - position(photo.size * scale - rect.size());
        // Points to the copy's pixels.
        let per_point = small.width() as f32 / (photo.size.x * scale);
        // The pixels under `from..to` (in the copy's pixels), at least one.
        let span = |from: f32, to: f32, length: u32| {
            let start = (from.floor().max(0.0) as u32).min(length - 1);
            let end = (to.ceil().max(0.0) as u32).clamp(start + 1, length);
            start..end
        };
        let mut cells = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            let at = |i: usize| region.top() + region.height() * i as f32 / rows as f32;
            let ys = span(
                (at(row) - origin.y) * per_point,
                (at(row + 1) - origin.y) * per_point,
                small.height(),
            );
            for column in 0..columns {
                let at = |i: usize| region.left() + region.width() * i as f32 / columns as f32;
                let xs = span(
                    (at(column) - origin.x) * per_point,
                    (at(column + 1) - origin.x) * per_point,
                    small.width(),
                );
                let mut sum = [0.0; 3];
                for y in ys.clone() {
                    for x in xs.clone() {
                        for (sum, channel) in sum.iter_mut().zip(small.get_pixel(x, y).0) {
                            *sum += f32::from(channel);
                        }
                    }
                }
                let count = (ys.len() * xs.len()) as f32;
                cells.push(sum.map(|sum| sum / count));
            }
        }
        Some(cells)
    }
}

fn decode_pixels(path: &Path, blurred: bool) -> Option<Arc<Pixels>> {
    let meta = std::fs::metadata(path).ok()?;
    let key = (
        path.to_path_buf(),
        meta.len(),
        meta.modified().ok(),
        blurred,
    );
    let slot = {
        let mut cache = PIXELS
            .lock()
            .expect("the photo pixel cache's lock is never poisoned");
        if let Some(pos) = cache.iter().position(|(known, _)| *known == key) {
            let entry = cache.remove(pos);
            let slot = entry.1.clone();
            cache.push(entry);
            slot
        } else {
            if cache.len() >= MAX_CACHED_PIXELS {
                cache.remove(0);
            }
            let slot = Arc::new(OnceLock::new());
            cache.push((key, slot.clone()));
            slot
        }
    };
    slot.get_or_init(|| {
        let decoded = std::fs::read(path)
            .ok()
            .and_then(|bytes| image::load_from_memory(&bytes).ok())?;
        let small = blurred.then(|| {
            Arc::new(
                decoded
                    .thumbnail(
                        SMALL_WIDTH,
                        SMALL_WIDTH * decoded.height() / decoded.width().max(1),
                    )
                    .into_rgb8(),
            )
        });
        let blurred_rgb = blurred.then(|| {
            decoded
                .thumbnail(
                    BLURRED_WIDTH,
                    BLURRED_WIDTH * decoded.height() / decoded.width().max(1),
                )
                .blur(BLUR)
                .into_rgb8()
        });
        Some(Arc::new(Pixels {
            rgb: decoded.into_rgb8(),
            small,
            blurred: blurred_rgb,
        }))
    })
    .clone()
}

fn decode(ctx: &egui::Context, path: &Path, blurred: bool) -> Option<Tiles> {
    let Some(pixels_data) = decode_pixels(path, blurred) else {
        log::warn!("no photo at {}", path.display());
        return None;
    };
    let small = pixels_data.small.clone();
    let blurred = pixels_data.blurred.as_ref().map(|small| {
        let image = egui::ColorImage::from_rgb(
            [small.width() as usize, small.height() as usize],
            small.as_raw(),
        );
        ctx.load_texture(
            format!("{} blurred", path.display()),
            image,
            egui::TextureOptions::LINEAR,
        )
    });
    let rgb = &pixels_data.rgb;
    let (width, height) = rgb.dimensions();
    let (columns, rows) = (spans(width), spans(height));
    let mut tiles = Vec::new();
    for (x, columns) in columns.iter().enumerate() {
        for (y, rows) in rows.iter().enumerate() {
            let held = (
                columns.start.saturating_sub(1),
                rows.start.saturating_sub(1),
                (columns.end + 1).min(width),
                (rows.end + 1).min(height),
            );
            // Row by row, since a copy per row is far quicker than a generic crop in a debug build.
            let mut pixels =
                Vec::with_capacity(((held.2 - held.0) * (held.3 - held.1) * 3) as usize);
            for row in held.1..held.3 {
                let start = ((row * width + held.0) * 3) as usize;
                pixels.extend_from_slice(
                    &rgb.as_raw()[start..start + ((held.2 - held.0) * 3) as usize],
                );
            }
            let image = egui::ColorImage::from_rgb(
                [(held.2 - held.0) as usize, (held.3 - held.1) as usize],
                &pixels,
            );
            // A scaled-down photo is sampled bilinearly, without mipmaps.
            let texture = ctx.load_texture(
                format!("{} {x},{y}", path.display()),
                image,
                egui::TextureOptions::LINEAR,
            );
            let rect = |left: u32, top: u32, right: u32, bottom: u32| {
                Rect::from_min_max(
                    pos2(left as f32, top as f32),
                    pos2(right as f32, bottom as f32),
                )
            };
            tiles.push(Tile {
                texture,
                owned: rect(columns.start, rows.start, columns.end, rows.end),
                held: rect(held.0, held.1, held.2, held.3),
            });
        }
    }
    Some(Tiles {
        size: vec2(width as f32, height as f32),
        blurred,
        small,
        tiles,
    })
}

/// `length` pixels cut into even spans that fit a texture with a spare texel on each side.
fn spans(length: u32) -> Vec<Range<u32>> {
    let count = length.div_ceil(MAX_SIDE - 2);
    (0..count)
        .map(|i| length * i / count..length * (i + 1) / count)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_is_what_covers_its_cells() {
        // A 1440×960 photo, red on its left half and blue on its right, covering a 720×360
        // window: it's scaled by half and its middle third of rows shows.
        let path =
            std::env::temp_dir().join(format!("exo-photo-sample-{}.png", std::process::id()));
        image::RgbImage::from_fn(1440, 960, |x, _| {
            image::Rgb(if x < 720 { [255, 0, 0] } else { [0, 0, 255] })
        })
        .save(&path)
        .expect("saved");
        let ctx = egui::Context::default();
        let mut photo = Photo::load_blurred(&ctx, path.clone());
        photo.wait();
        let window = Rect::from_min_size(pos2(0.0, 0.0), vec2(720.0, 360.0));
        let centered = |free: Vec2| free * 0.5;
        let cells = photo
            .sample(window, centered, window, [4, 2])
            .expect("decoded");
        assert_eq!(cells.len(), 8);
        for row in cells.chunks(4) {
            assert_eq!(row[0], [255.0, 0.0, 0.0]);
            assert_eq!(row[1], [255.0, 0.0, 0.0]);
            assert_eq!(row[2], [0.0, 0.0, 255.0]);
            assert_eq!(row[3], [0.0, 0.0, 255.0]);
        }
        // A cell across the middle mixes the two.
        let middle = Rect::from_min_size(pos2(350.0, 100.0), vec2(20.0, 20.0));
        let mixed = photo
            .sample(window, centered, middle, [1, 1])
            .expect("decoded");
        assert!((mixed[0][0] - 127.5).abs() < 1.0, "{mixed:?}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn spans_fit_a_texture_with_their_neighbours_texels() {
        for length in [1, 2046, 2047, 3264, 4096, 4896] {
            let spans = spans(length);
            assert_eq!(spans.first().map(|span| span.start), Some(0));
            assert_eq!(spans.last().map(|span| span.end), Some(length));
            assert!(spans.windows(2).all(|pair| pair[0].end == pair[1].start));
            assert!(
                spans
                    .iter()
                    .all(|span| span.end - span.start + 2 <= MAX_SIDE),
                "{length}: {spans:?}"
            );
        }
        assert_eq!(spans(2046).len(), 1);
        assert_eq!(spans(4096).len(), 3);
    }
}
