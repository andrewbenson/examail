//! The day's photo behind Inbox Zero and the full-screen alerts. The photos are open landscape
//! photos (`assets/inbox-zero`), one a day, each with its own scrims and placement
//! (`photos.json`).

use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use egui::{Color32, Mesh, Painter, Rect, Shape, Vec2, pos2, vec2};
use jiff::Zoned;
use serde::Deserialize;

use exo_commands::{Command, Commands, Region};

use crate::photo::Photo;

/// What the palette does with the photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `Download Inbox Zero Image`, which saves the photo to the download folder.
    Download,
    /// `Inbox Zero: Disable/Enable Animated Images`, which toggles the Reduce Motion setting.
    ToggleAnimatedImages,
}

/// Registers `Download Inbox Zero Image` at Inbox Zero (`zero`), in the list and its header,
/// and `Inbox Zero: Disable Animated Images` (`Enable` once `reduced`) everywhere.
pub(crate) fn register(
    commands: &mut Commands<crate::commands::Action>,
    zero: bool,
    reduced: bool,
) {
    let act = crate::commands::Action::Backdrop;
    if zero {
        const LIST: &[Region] = &[Region::ThreadList, Region::ThreadListHeader];
        commands.add(
            Command::new(
                "DOWNLOAD_INBOX_ZERO",
                "Download Inbox Zero Image",
                act(Action::Download),
            )
            .regions(LIST)
            .icon("data/download"),
        );
    }
    let title = if reduced {
        "Inbox Zero: Enable Animated Images"
    } else {
        "Inbox Zero: Disable Animated Images"
    };
    commands.add(
        Command::new(
            "TOGGLE_REDUCED_MOTION",
            title,
            act(Action::ToggleAnimatedImages),
        )
        .aliases([
            "Reduce Motion",
            "Allow Motion",
            "Disable Motion",
            "Enable Motion",
        ])
        .default_score(0.0)
        .icon("data/settings"),
    );
}

/// One photo of the rotation, as `photos.json` lists it.
#[derive(Debug, Deserialize)]
struct Entry {
    file: String,
    /// The top scrim's opacity in percent, instead of the default scrims.
    top_scrim: Option<f32>,
    /// The bottom scrim's, likewise.
    bottom_scrim: Option<f32>,
    /// Where the photo sits, as CSS's `object-position` takes it.
    position: Option<String>,
}

static PHOTOS: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/inbox-zero/photos.json"))
        .expect("photos.json is well-formed")
});

/// The day's photo, which changes once a day at 2 AM local time and is the same for everyone in
/// a time zone.
fn index(now: &Zoned) -> usize {
    let minutes =
        now.timestamp().as_second().div_euclid(60) + i64::from(now.offset().seconds()) / 60;
    let day = (minutes - 120).div_euclid(60 * 24) + 160;
    day.rem_euclid(PHOTOS.len() as i64) as usize
}

/// Where the photos are, which is `Contents/Resources/inbox-zero` in `Exo.app` (`cargo
/// xtask build`), or else `assets/inbox-zero` in the checkout.
fn photos_dir() -> PathBuf {
    crate::bundled_resource("inbox-zero")
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/inbox-zero"
            ))
        })
}

/// The backdrop: the day's photo, decoding or decoded.
pub struct Backdrop {
    entry: &'static Entry,
    photo: Photo,
    /// The day it's the photo for ([`index`]).
    day: usize,
}

impl Backdrop {
    /// Starts decoding the photo for `now`.
    pub fn new(ctx: &egui::Context, now: &Zoned) -> Self {
        let day = index(now);
        Self::of(ctx, &PHOTOS[day], day)
    }

    /// The photo named `file` until the day turns, for a screenshot of a given photo.
    pub fn named(ctx: &egui::Context, file: &str, now: &Zoned) -> Option<Self> {
        PHOTOS
            .iter()
            .find(|entry| entry.file == file)
            .map(|entry| Self::of(ctx, entry, index(now)))
    }

    fn of(ctx: &egui::Context, entry: &'static Entry, day: usize) -> Self {
        Self {
            entry,
            photo: Photo::load_blurred(ctx, photos_dir().join(&entry.file)),
            day,
        }
    }

    /// Moves on to `now`'s photo once the day has turned (at 2 AM).
    pub fn refresh(&mut self, ctx: &egui::Context, now: &Zoned) {
        if index(now) != self.day {
            *self = Self::new(ctx, now);
        }
    }

    /// Blocks until the photo is decoded, for a screen that shows only once it can draw it.
    pub fn wait(&mut self) {
        self.photo.wait();
    }

    /// Whether the photo is decoded. For screenshots and tests.
    pub fn is_loaded(&self) -> bool {
        self.photo.is_loaded()
    }

    /// The photo's file, for `Download Inbox Zero Image`.
    pub fn path(&self) -> PathBuf {
        photos_dir().join(&self.entry.file)
    }

    /// The photo's file name.
    pub fn file(&self) -> &Path {
        Path::new(&self.entry.file)
    }

    /// Paints the photo over `window`, covering it (`object-fit: cover`) where its position puts
    /// it.
    pub fn paint_photo(&self, painter: &Painter, window: Rect) {
        let at = position(self.entry.position.as_deref());
        self.photo.paint(painter, window, |free| free * at);
    }

    /// Paints Inbox Zero's backdrop over `window`, which is the photo and its scrims, the bottom
    /// one reaching further while the settings menu is open. With the menu open, the photo is
    /// blurred under black at .5 (`background: rgba(0,0,0,.5); backdrop-filter: blur(8px)`, at
    /// once).
    pub fn paint(&self, painter: &Painter, window: Rect, settings_open: bool) {
        if settings_open {
            let at = position(self.entry.position.as_deref());
            self.photo.paint_blurred(painter, window, |free| free * at);
        } else {
            self.paint_photo(painter, window);
        }
        for (edge, stops) in self.scrims(settings_open) {
            scrim(painter, window, edge, &stops);
        }
        if settings_open {
            painter.rect_filled(window, 0.0, black(SETTINGS_SHADE));
        }
    }

    /// What the see-through cards over the backdrop show through: the backdrop over `window`, as
    /// [`Backdrop::paint`] paints it, under the popups' black at `.2 * dim`. Under the settings
    /// menu the cards blur the sharp photo, which their own blur all but covers.
    pub fn behind(&self, window: Rect, settings_open: bool, dim: f32) -> exo_ui::Behind {
        let photo = self.photo.share();
        let at = position(self.entry.position.as_deref());
        let scrims = self.scrims(settings_open);
        let shade = (1.0 - if settings_open { SETTINGS_SHADE } else { 0.0 }) * (1.0 - 0.2 * dim);
        let mut key = std::hash::DefaultHasher::new();
        (self.entry.file.as_str(), self.photo.is_loaded()).hash(&mut key);
        [
            window.min.x,
            window.min.y,
            window.max.x,
            window.max.y,
            shade,
        ]
        .map(f32::to_bits)
        .hash(&mut key);
        exo_ui::Behind::new(key.finish(), move |region, [columns, rows]| {
            let mut cells = photo.sample(window, |free| free * at, region, [columns, rows])?;
            for (row, cells) in cells.chunks_mut(columns).enumerate() {
                let y = region.top() + region.height() * (row as f32 + 0.5) / rows as f32;
                let shade = scrims
                    .iter()
                    .map(|(edge, stops)| 1.0 - opacity_at(stops, edge.distance(window, y)))
                    .product::<f32>()
                    * shade;
                for channel in cells.iter_mut().flatten() {
                    *channel *= shade;
                }
            }
            Some(cells)
        })
    }

    /// The photo's scrims: the black's opacity at distances from the top and bottom edges,
    /// transparent past the last.
    fn scrims(&self, settings_open: bool) -> [(Edge, Vec<(f32, f32)>); 2] {
        match (self.entry.top_scrim, self.entry.bottom_scrim) {
            // `linear-gradient(to bottom, rgba(0,0,0,.5) 0, rgba(0,0,0,.05) 176px, transparent
            // 216px), linear-gradient(to top, rgba(0,0,0,.6) 0, rgba(0,0,0,.06) 176px, transparent
            // 216px)`.
            (None, None) => [
                (Edge::Top, vec![(0.0, 0.5), (176.0, 0.05), (216.0, 0.0)]),
                (Edge::Bottom, vec![(0.0, 0.6), (176.0, 0.06), (216.0, 0.0)]),
            ],
            // A photo's own scrims, 50% at the top and 60% at the bottom unless it says:
            // `rgba(0,0,0,{o}%) 0px, rgba(0,0,0,{o/10}%) 264px, transparent 324px`, the bottom one
            // to 636px with the settings open.
            (top, bottom) => [
                (Edge::Top, top.unwrap_or(50.0), 264.0),
                (
                    Edge::Bottom,
                    bottom.unwrap_or(60.0),
                    if settings_open { 636.0 } else { 264.0 },
                ),
            ]
            .map(|(edge, opacity, extent)| {
                let opacity = opacity / 100.0;
                (
                    edge,
                    vec![
                        (0.0, opacity),
                        (extent, opacity / 10.0),
                        (extent + 60.0, 0.0),
                    ],
                )
            }),
        }
    }
}

/// The settings menu's black over the photo.
const SETTINGS_SHADE: f32 = 0.5;

/// Black at `opacity`.
fn black(opacity: f32) -> Color32 {
    Color32::from_black_alpha((opacity * 255.0).round() as u8)
}

impl crate::Mail {
    /// Runs a palette command about the photo.
    pub(crate) fn backdrop_run(&mut self, action: Action) {
        match action {
            // Saved like an attachment, with its notification.
            Action::Download => match std::fs::read(self.backdrop.path()) {
                Ok(bytes) => self
                    .attachment_files
                    .save_file(self.backdrop.file().display().to_string(), bytes),
                Err(error) => log::warn!("reading the Inbox Zero photo: {error}"),
            },
            // Exo's photos are all stills, so the setting has nothing to stop yet. Its hint says
            // what it did.
            Action::ToggleAnimatedImages => {
                let reduce = !self.account.settings().reduce_motion;
                self.account
                    .update_settings(|settings| settings.reduce_motion = reduce);
                self.commands
                    .hint(exo_commands::Hint::new().text(if reduce {
                        "Animated Images Disabled"
                    } else {
                        "Animated Images Enabled"
                    }));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Edge {
    Top,
    Bottom,
}

impl Edge {
    /// How far `y` is from this edge of `window`.
    fn distance(self, window: Rect, y: f32) -> f32 {
        match self {
            Edge::Top => y - window.top(),
            Edge::Bottom => window.bottom() - y,
        }
    }
}

/// A black `linear-gradient` from `edge` into `window` through `stops` (distance from the edge,
/// opacity), transparent past the last. A mesh interpolates premultiplied colors linearly, as CSS
/// does.
fn scrim(painter: &Painter, window: Rect, edge: Edge, stops: &[(f32, f32)]) {
    let y = |distance: f32| match edge {
        Edge::Top => window.top() + distance,
        Edge::Bottom => window.bottom() - distance,
    };
    let mut mesh = Mesh::default();
    for (i, &(distance, opacity)) in stops.iter().enumerate() {
        let color = black(opacity);
        let (left, right) = (
            pos2(window.left(), y(distance)),
            pos2(window.right(), y(distance)),
        );
        for point in [left, right] {
            mesh.colored_vertex(point, color);
        }
        if i > 0 {
            let base = (2 * i - 2) as u32;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 2, base + 3);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// A scrim's opacity `distance` from its edge, between its `stops` as [`scrim`] paints them.
fn opacity_at(stops: &[(f32, f32)], distance: f32) -> f32 {
    let after = stops.partition_point(|&(at, _)| at <= distance);
    match (after.checked_sub(1).map(|i| stops[i]), stops.get(after)) {
        (Some((from, a)), Some(&(to, b))) => a + (b - a) * (distance - from) / (to - from),
        (Some(_), None) => 0.0,
        (None, Some(&(_, opacity))) => opacity,
        (None, None) => 0.0,
    }
}

/// Where `object-position` puts a covering image, as fractions of how much larger than the
/// window it is: `left X% top Y%` or `left X% bottom Y%`. Anything else CSS would throw out as
/// invalid (`left: 50% top 44%`, with its stray colon), leaving the default `50% 50%`.
fn position(css: Option<&str>) -> Vec2 {
    let center = vec2(0.5, 0.5);
    let Some(css) = css else { return center };
    let percent = |token: &str| {
        token
            .strip_suffix('%')?
            .parse::<f32>()
            .ok()
            .map(|value| value / 100.0)
    };
    match css.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["left", x, "top", y] => percent(x)
            .zip(percent(y))
            .map_or(center, |(x, y)| vec2(x, y)),
        ["left", x, "bottom", y] => percent(x)
            .zip(percent(y))
            .map_or(center, |(x, y)| vec2(x, 1.0 - y)),
        _ => center,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_photo_is_in_the_assets_with_its_placement_understood() {
        assert!((30..=50).contains(&PHOTOS.len()), "{} photos", PHOTOS.len());
        for entry in PHOTOS.iter() {
            assert!(
                photos_dir().join(&entry.file).is_file(),
                "{} is missing",
                entry.file
            );
            if let Some(css) = &entry.position {
                let understood = css.starts_with("left ") && css.split_whitespace().count() == 4;
                assert!(understood || css.contains(':'), "{css}");
            }
        }
    }

    #[test]
    fn the_photo_changes_at_two_in_the_morning_local_time() {
        let at = |time: &str| index(&time.parse::<Zoned>().expect("a time"));
        let today = at("2026-11-17T09:00:00-05:00[America/New_York]");
        assert_eq!(at("2026-11-17T02:00:00-05:00[America/New_York]"), today);
        assert_eq!(at("2026-11-18T01:59:00-05:00[America/New_York]"), today);
        assert_eq!(
            at("2026-11-18T02:00:00-05:00[America/New_York]"),
            (today + 1) % PHOTOS.len()
        );
        // The same local time elsewhere is the same photo.
        assert_eq!(at("2026-11-17T09:00:00+09:00[Asia/Tokyo]"), today);
    }

    #[test]
    fn a_scrims_opacity_runs_between_its_stops() {
        let stops = [(0.0, 0.5), (176.0, 0.05), (216.0, 0.0)];
        assert_eq!(opacity_at(&stops, 0.0), 0.5);
        assert!((opacity_at(&stops, 88.0) - 0.275).abs() < 1e-6);
        assert!((opacity_at(&stops, 196.0) - 0.025).abs() < 1e-6);
        assert_eq!(opacity_at(&stops, 216.0), 0.0);
        assert_eq!(opacity_at(&stops, 500.0), 0.0);
        // Past the window's edge, the first stop's.
        assert_eq!(opacity_at(&stops, -3.0), 0.5);
    }

    #[test]
    fn object_position_reads_as_css_does() {
        assert_eq!(position(None), vec2(0.5, 0.5));
        assert_eq!(position(Some("left 50% top 40%")), vec2(0.5, 0.4));
        assert_eq!(
            position(Some("left 52% bottom 41%")),
            vec2(0.52, 1.0 - 0.41)
        );
        assert_eq!(position(Some("left: 50% top 44%")), vec2(0.5, 0.5));
    }
}
