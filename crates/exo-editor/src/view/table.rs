//! The tables the editor keeps whole (a pasted table, a table-built signature, a revealed
//! quote's newsletter). exo-mail's Blitz body renderer draws each one in the block that holds it,
//! as the thread view draws a message. A table's stylesheet is the editor's (the message body's),
//! and its colours are tagged as the editor's content is ([`exo_mail::render_fragment`]).
//!
//! A table's images load as the body's do. Each is one of [`crate::Editor::image_requests`],
//! answered with [`crate::Editor::set_image`], whose file the table's document decodes off the UI
//! thread. The document asks for its `<img>`s by their place in the table
//! (`cid:exo-table-image-{n}`), so what it asks for maps back to the `<img>` whatever Blitz makes
//! of the URL. What its CSS or a `background` asks for loads by URL, as a remote or inline image.
//! Remote images that the Images setting blocks show as empty boxes, because the table is built
//! again with them blocked. A tall table is painted in bands, only where it's on screen.

use std::{
    collections::HashMap,
    hash::{Hash as _, Hasher as _},
    sync::{Arc, Mutex, mpsc},
};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2, vec2};
use exo_mail::{BodyView, Done, Fetch, Region, ViewOptions};
use exo_ui::Theme;

use crate::{
    html,
    image::{ImageSource, Pictures, State},
    model::Element,
};

/// The prefix a table's document loads an image by, followed by its place in [`Table::images`].
const IMAGE_URL: &str = "cid:exo-table-image-";

/// Which table a block holds, by its markup.
pub(crate) fn key(html: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hasher);
    hasher.finish()
}

/// The height of a painted band, in CSS px.
const BAND: f32 = 1024.0;

/// What a table's document has asked to load and not yet had. An `<img>` is asked for by its place
/// in [`Table::images`], and anything else (CSS, `background`) by the image its URL names.
#[derive(Default)]
struct Requests(Mutex<Vec<(Asked, Done)>>);

enum Asked {
    Image(usize),
    Url(ImageSource),
}

impl Fetch for Requests {
    fn fetch(&self, url: &str, done: Done) {
        let asked = match url
            .strip_prefix(IMAGE_URL)
            .and_then(|index| index.parse().ok())
        {
            Some(index) => Asked::Image(index),
            None => match ImageSource::of(&Element::with("img", &[("src", url)])) {
                Some(source @ (ImageSource::Remote(_) | ImageSource::Cid(_))) => Asked::Url(source),
                _ => return,
            },
        };
        if let Ok(mut pending) = self.0.lock() {
            pending.push((asked, done));
        }
    }
}

/// What a band was painted at: the table's generation, and the bits of the width, phase and scale.
type Painted = (u64, u32, u32, u32);

/// One table, laid out and painted.
struct Table {
    view: BodyView,
    requests: Arc<Requests>,
    /// Its `<img>`s' sources and elements, in document order, then the images its CSS asked for
    /// (with an empty `<img>`).
    images: Vec<(ImageSource, Element)>,
    theme: Theme,
    /// Whether it was built with its remote images blocked.
    blocked: bool,
    /// The width and scale it was last laid out at, and its height there.
    laid_out: Option<(f32, f32, f32)>,
    /// Bumped when what it shows changes (a layout, an image arriving).
    generation: u64,
    /// Its painted bands by index.
    bands: HashMap<u32, (TextureHandle, Painted)>,
    /// Whether the editor's latest layout laid it out.
    used: bool,
}

impl Table {
    fn new(html: &str, theme: Theme, blocked: bool) -> Self {
        let mut images = Vec::new();
        let markup = html::map_images(html, |image| {
            let Some(source) = ImageSource::of(image) else {
                return;
            };
            images.push((source.clone(), image.clone()));
            // A blocked image keeps its URL, for the view to draw it blocked, and a `data:` image
            // loads itself.
            match source {
                ImageSource::Remote(_) if blocked => {}
                ImageSource::Data(_) => {}
                ImageSource::Cid(_) | ImageSource::Remote(_) => {
                    image.set_attr("src", &format!("{IMAGE_URL}{}", images.len() - 1));
                    image.attrs.retain(|(name, _)| name != "srcset");
                }
            }
        });
        let requests = Arc::new(Requests::default());
        let options = ViewOptions {
            theme: match theme {
                Theme::Snow => exo_mail::Theme::Light,
                Theme::Carbon => exo_mail::Theme::Dark,
            },
            block_remote_content: blocked,
            ..ViewOptions::default()
        };
        let render = exo_mail::render_fragment(&markup);
        let fonts = exo_mail::Fonts::shared().clone();
        let view = BodyView::new(
            &render,
            options,
            &fonts,
            Some(requests.clone() as Arc<dyn Fetch>),
        );
        Self {
            view,
            requests,
            images,
            theme,
            blocked,
            laid_out: None,
            generation: 0,
            bands: HashMap::new(),
            used: true,
        }
    }

    /// Whether the Images setting blocks one of its images, as last answered.
    fn any_blocked(&self, pictures: &Pictures) -> bool {
        self.images
            .iter()
            .any(|(source, _)| matches!(pictures.get(&source.key()), Some(State::Blocked)))
    }
}

/// A batch of ready table images handed to the reusable decode worker thread.
type DecodeBatch = (Vec<(Done, Arc<[u8]>)>, egui::Context);

/// Every kept table an editor shows, by [`key`].
#[derive(Default)]
pub(crate) struct Tables {
    tables: HashMap<u64, Table>,
    /// Bumped whenever a table is built or what one shows changes, so the layout looks again.
    pub(crate) revision: u64,
    /// Single reusable worker thread for decoding table images off the UI thread; lazily spawned
    /// on first use and cleanly exited when dropped.
    worker: Option<mpsc::Sender<DecodeBatch>>,
}

impl Tables {
    /// The [`key`] of the table `html` makes, and its box in a block `width` wide at `scale` in
    /// `theme`. The table is built and laid out as needed.
    pub(crate) fn lay_out(
        &mut self,
        html: &str,
        width: f32,
        scale: f32,
        theme: Theme,
        pictures: &Pictures,
    ) -> (u64, Vec2) {
        let key = key(html);
        let stale = self.tables.get(&key).is_none_or(|table| {
            table.theme != theme || table.blocked != table.any_blocked(pictures)
        });
        if stale {
            let blocked = self
                .tables
                .get(&key)
                .is_some_and(|table| table.any_blocked(pictures));
            self.tables.insert(key, Table::new(html, theme, blocked));
            self.revision += 1;
        }
        let table = self.tables.get_mut(&key).expect("built above");
        table.used = true;
        let height = match table.laid_out {
            Some((w, s, height)) if w == width && s == scale => height,
            _ => {
                let height = table.view.layout(width, scale);
                table.laid_out = Some((width, scale, height));
                table.generation += 1;
                height
            }
        };
        (key, vec2(width, height))
    }

    /// Marks every table unused, before a layout.
    pub(crate) fn begin_layout(&mut self) {
        for table in self.tables.values_mut() {
            table.used = false;
        }
    }

    /// Forgets the tables the latest layout didn't show.
    pub(crate) fn end_layout(&mut self) {
        self.tables.retain(|_, table| table.used);
    }

    /// Every image the tables show, for the app to load.
    pub(crate) fn images(&self) -> impl Iterator<Item = &(ImageSource, Element)> {
        self.tables.values().flat_map(|table| table.images.iter())
    }

    /// Hands the tables' documents the images that have loaded (decoded on a reusable worker
    /// thread, which repaints `ctx` once done), and applies what they decoded. Returns whether a
    /// table changed.
    pub(crate) fn update(&mut self, pictures: &Pictures, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let mut ready = Vec::new();
        for table in self.tables.values_mut() {
            if let Ok(mut pending) = table.requests.0.lock() {
                for (asked, done) in std::mem::take(&mut *pending) {
                    let source = match &asked {
                        Asked::Image(index) => {
                            table.images.get(*index).map(|(source, _)| source.clone())
                        }
                        Asked::Url(source) => {
                            if !table.images.iter().any(|(known, _)| known == source) {
                                // A new image for the app to load.
                                table.images.push((source.clone(), Element::new("img")));
                                changed = true;
                            }
                            Some(source.clone())
                        }
                    };
                    match source.and_then(|source| pictures.get(&source.key())) {
                        Some(State::Loaded { file, .. }) => ready.push((done, file.clone())),
                        None | Some(State::Pending) => pending.push((asked, done)),
                        // A blocked image (the table is built again) or a broken one never loads.
                        Some(State::Blocked | State::Broken) => {}
                    }
                }
            }
            if table.view.poll() {
                table.laid_out = table
                    .laid_out
                    .map(|(width, scale, _)| (width, scale, table.view.height()));
                table.generation += 1;
                changed = true;
            }
        }
        if !ready.is_empty() {
            self.decode((ready, ctx.clone()));
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Routes ready table images onto the single reusable decode worker thread.
    fn decode(&mut self, batch: DecodeBatch) {
        let batch = match &self.worker {
            Some(tx) => match tx.send(batch) {
                Ok(()) => return,
                Err(mpsc::SendError(batch)) => {
                    self.worker = None;
                    batch
                }
            },
            None => batch,
        };
        let (tx, rx) = mpsc::channel::<DecodeBatch>();
        match std::thread::Builder::new()
            .name("exo-editor-table-images".into())
            .spawn(move || {
                while let Ok((ready, ctx)) = rx.recv() {
                    for (done, file) in ready {
                        done.finish(file.to_vec());
                    }
                    ctx.request_repaint();
                }
            }) {
            Ok(_) => {
                if tx.send(batch).is_ok() {
                    self.worker = Some(tx);
                }
            }
            Err(error) => log::warn!("decoding a table's images: {error}"),
        }
    }

    /// Paints the bands of table `key` that `shown` reaches and that aren't up to date. The table
    /// is `width` wide, `phase` px right of the whole pixel its textures go on, and `shown` is in
    /// CSS px from its top. Bands further than a band from `shown` are dropped.
    pub(crate) fn paint(
        &mut self,
        key: u64,
        width: f32,
        phase: f32,
        scale: f32,
        shown: std::ops::Range<f32>,
        ctx: &egui::Context,
    ) {
        let Some(table) = self.tables.get_mut(&key) else {
            return;
        };
        let Some((_, _, height)) = table.laid_out else {
            return;
        };
        if shown.end <= 0.0 || shown.start >= height || shown.start >= shown.end || height <= 0.0 {
            table.bands.clear();
            return;
        }
        let painted = (
            table.generation,
            width.to_bits(),
            phase.to_bits(),
            scale.to_bits(),
        );
        let band = |y: f32| (y.max(0.0) / BAND) as u32;
        let visible = band(shown.start)..band(shown.end.min(height - 0.5)) + 1;
        table
            .bands
            .retain(|index, _| *index + 1 >= visible.start && *index <= visible.end);
        for index in visible.filter(|index| (*index as f32) * BAND < height) {
            if table
                .bands
                .get(&index)
                .is_some_and(|(_, at)| *at == painted)
            {
                continue;
            }
            let top = index as f32 * BAND;
            let image = table.view.paint(
                Region {
                    top,
                    height: BAND.min(height - top),
                    left: 0.0,
                    width,
                    phase: [phase, 0.0],
                },
                [0; 4],
            );
            let pixels = ColorImage::from_rgba_premultiplied(
                [image.width as usize, image.height as usize],
                &image.rgba,
            );
            match table.bands.get_mut(&index) {
                Some((texture, at)) => {
                    texture.set(pixels, TextureOptions::NEAREST);
                    *at = painted;
                }
                None => {
                    let texture = ctx.load_texture(
                        format!("exo_editor::table {key:x} {index}"),
                        pixels,
                        TextureOptions::NEAREST,
                    );
                    table.bands.insert(index, (texture, painted));
                }
            }
        }
    }

    /// The `<img>` at `at` (CSS px from its top left) in table `key`, once its picture has loaded,
    /// with the `href` of the link around the point.
    pub(crate) fn image_at(
        &self,
        key: u64,
        at: egui::Pos2,
        pictures: &Pictures,
    ) -> Option<(crate::view::MenuImage, Option<String>)> {
        let table = self.tables.get(&key)?;
        let hit = table.view.context_hit(at.x, at.y);
        let shown = hit.image?.src;
        // The document knows its images by their place in the table, but for a `data:` image,
        // which loads itself.
        let source = match shown.strip_prefix(IMAGE_URL) {
            Some(place) => table.images.get(place.parse::<usize>().ok()?)?.0.clone(),
            None => table
                .images
                .iter()
                .find(|(source, _)| source.key() == shown)?
                .0
                .clone(),
        };
        let src = source.key();
        let file = pictures.file(&src)?;
        Some((crate::view::MenuImage { src, file }, hit.link))
    }

    /// The painted bands of table `key`, as each one's top (CSS px from the table's) and texture.
    pub(crate) fn bands(&self, key: u64) -> impl Iterator<Item = (f32, &TextureHandle)> {
        self.tables.get(&key).into_iter().flat_map(|table| {
            table
                .bands
                .iter()
                .map(|(index, (texture, _))| (*index as f32 * BAND, texture))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_screen_table_skips_rasterization_and_clears_cached_bands() {
        let ctx = egui::Context::default();
        let mut tables = Tables::default();
        let (key, size) = tables.lay_out(
            "<table><tbody><tr><td>cell</td></tr></tbody></table>",
            300.0,
            1.0,
            Theme::Snow,
            &Pictures::default(),
        );
        assert!(size.y > 0.0);

        // Table below the viewport (`shown.end <= 0.0`) must not rasterize band 0.
        tables.paint(key, size.x, 0.0, 1.0, -500.0..-100.0, &ctx);
        assert_eq!(tables.bands(key).count(), 0);
        tables.paint(key, size.x, 0.0, 1.0, -500.0..0.0, &ctx);
        assert_eq!(tables.bands(key).count(), 0);

        // Visible table rasterizes band 0.
        tables.paint(key, size.x, 0.0, 1.0, 0.0..size.y, &ctx);
        assert_eq!(tables.bands(key).count(), 1);

        // Scrolling back so the table is below or above the viewport clears cached bands.
        tables.paint(key, size.x, 0.0, 1.0, -200.0..-10.0, &ctx);
        assert_eq!(tables.bands(key).count(), 0);

        tables.paint(key, size.x, 0.0, 1.0, 0.0..size.y, &ctx);
        assert_eq!(tables.bands(key).count(), 1);
        tables.paint(key, size.x, 0.0, 1.0, size.y..size.y + 200.0, &ctx);
        assert_eq!(tables.bands(key).count(), 0);
    }
}
