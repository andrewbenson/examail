//! Message bodies on screen, rendered by exo-mail's pipeline and Blitz view and painted as egui
//! textures. This module is the only place Exo talks to exo-mail.
//!
//! A [`exo_mail::BodyView`] isn't `Send` and laying out or painting one can take tens of
//! milliseconds, so every view lives on one worker thread. The UI sends it requests (render a
//! thread, lay a body out, paint a band of it, hit-test, select) and picks the answers up on a
//! later frame. Nothing here blocks a frame. Bodies are painted in horizontal bands so a tall
//! newsletter only costs the bands on screen, and every band is cached until the body's layout,
//! selection or images change. A band covers the body's whole width, so a body wider than its
//! column scrolls sideways without painting again. Animated images play in patches painted over
//! the bands, frame by frame, while they're on screen.

pub(crate) mod fetch;
mod index;
mod worker;

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::Duration,
};

use egui::{Color32, ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use exo_mail::Render;
use exo_store::{MessageDetail, RemoteContentBlocking};

pub(crate) use index::visible_texts;
use worker::{Job, Output, Worker};

use crate::account::Loader;

/// The height of a painted band, in CSS px.
pub(super) const BAND: f32 = 512.0;
/// Painted bands kept past each end of what's on screen, for scrolling back.
const KEPT_BANDS: u32 = 2;
/// Multi-click window and slop for double-/triple-click selection in a body.
const MULTI_CLICK_DELAY: f64 = 0.5;
const MULTI_CLICK_DISTANCE: f32 = 4.0;

/// Where a body sits on screen and which pointer interactions it accepts.
pub(crate) struct PointerArea<'a> {
    pub id: &'a str,
    /// Visible hit-testable rect of the body on screen.
    pub body: Rect,
    /// Top-left of the body's content in screen coordinates (may sit above `body.min` when
    /// scrolled).
    pub origin: Pos2,
    /// Pointer position to test against `body`.
    pub pointer: Option<Pos2>,
    /// Whether a secondary click opens the body's context menu.
    pub context_menu: bool,
}

/// Cross-frame pointer state for multi-click and drag selection in [`Bodies`].
#[derive(Clone, Debug, Default)]
pub(crate) struct PointerState {
    /// The last press in a body: when, where, and how many in a row (1..=3).
    clicks: Option<(f64, Pos2, u32)>,
    /// The message id currently being drag-selected, if any.
    selecting: Option<String>,
    /// Modifiers held on the last primary click in a body (e.g. ⌘ opens links in the background).
    pub(crate) click_modifiers: egui::Modifiers,
}

impl PointerState {
    /// Records a primary press at `time` and `at`, returning the resulting click count (`1..=3`).
    fn record_press(&mut self, time: f64, at: Pos2) -> u32 {
        let count = match self.clicks {
            Some((then, place, count))
                if time - then < MULTI_CLICK_DELAY && place.distance(at) < MULTI_CLICK_DISTANCE =>
            {
                (count + 1).min(3)
            }
            _ => 1,
        };
        self.clicks = Some((time, at, count));
        count
    }

    fn is_multi_click(&self) -> bool {
        self.clicks.is_some_and(|(.., count)| count > 1)
    }

    /// Resets transient multi-click and drag-selection state (e.g. when switching items).
    pub(crate) fn clear(&mut self) {
        self.clicks = None;
        self.selecting = None;
    }
}

/// What [`Bodies::handle_pointer`] observed this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PointerOutcome {
    /// The pointer hovered inside the body rect.
    pub hovered: bool,
    /// A new text selection started in the body (so any adjacent header selection should clear).
    pub started_selection: bool,
}

/// What the pipeline found in a body, for the parts of the thread view around it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The visible text's first 200 characters (a collapsed message's snippet).
    pub snippet: String,
    /// How many characters the visible text has (the new part's text as it shows).
    pub visible_chars: usize,
    /// There's a quoted part behind a `…`.
    pub has_quoted: bool,
    /// The new part, and the quoted part, look like marketing mail.
    pub unquoted_marketing: bool,
    pub quoted_marketing: bool,
    /// Read-receipt pixels were removed, so the header says "Email tracking blocked" unless the
    /// Images setting shows all images ([`Bodies::tracking_blocked`]).
    pub tracking_blocked: bool,
    /// Something in it loads from the network.
    pub remote_images: bool,
    /// The link an Unsubscribe would open when the headers have none.
    pub unsubscribe: Option<String>,
    /// The new part of the message as HTML, which copying the message serializes.
    pub unquoted_html: String,
    /// What hides behind the `…`, which prints when it's shown.
    pub quoted_html: String,
    /// The whole rendered (sanitized) document and its `<body>`'s attributes, which a reply
    /// quotes.
    pub html: String,
    pub body_attributes: exo_mail::BodyAttributes,
    /// Every link, in document order.
    pub links: Vec<exo_mail::Link>,
    /// One-click actions, in document order.
    pub actions: Vec<exo_mail::Action>,
    /// What a Google Calendar notification says about its event.
    pub calendar: exo_mail::CalendarData,
    /// The dates the new part mentions outside quotes, as written.
    pub unquoted_dates: Vec<String>,
    /// The new part proposes or settles a time (`are you free`, `works for me`…).
    pub proposes_time: bool,
}

/// How a body is shown. Changing any of it lays the body out again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// The column's width in CSS px.
    pub width: f32,
    /// Device pixels per CSS px.
    pub scale: f32,
    /// The app theme.
    pub theme: exo_ui::Theme,
    /// The root font size.
    pub rem: f32,
    /// The quoted part is expanded below the `…`.
    pub show_quoted: bool,
    /// The body is a notification that its invite card replaces, so nothing shows above the `…`
    /// and the new part is behind it.
    pub hide_original: bool,
    /// The fractional part of the body's x on screen, in CSS px (glyphs land on quarter device
    /// pixels of their absolute position).
    pub phase: f32,
    /// The card behind the body, painted under it (premultiplied RGBA).
    pub background: [u8; 4],
}

/// What's under the pointer in a body.
pub type Hit = exo_mail::Hit;

/// What a press in a body selects: a caret, a word or a paragraph (one, two or three clicks).
pub type Granularity = exo_mail::Granularity;

/// Something a body request came back with.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A click in message `id` landed on `hit`.
    Clicked { id: String, hit: Hit },
    /// The selected text, for the clipboard.
    Copied(String),
    /// A copy found no text selected.
    NothingSelected,
    /// What the messages of conversation `thread` hold, by message id, rendered for
    /// [`Bodies::render`].
    Rendered {
        thread: String,
        facts: Vec<(String, Facts)>,
    },
    /// The target of an in-message link in message `id` starts `top` CSS px down the body
    /// ([`Bodies::find_anchor`]).
    Anchored { id: String, top: f32 },
    /// A right-click at `at` on screen in message `id` ([`Bodies::context_menu`]). It carries
    /// what the click landed on, and the text selected then, with where the baseline of its
    /// first line starts on screen (roughly the top left of the selection's box, a line's ascent
    /// down).
    ContextMenu {
        id: String,
        at: Pos2,
        hit: exo_mail::ContextHit,
        selection: Option<(String, Pos2)>,
    },
    /// The selection changed to most of a message, or more than 500 characters of it.
    SelectedMuch,
}

/// The text selection, as the worker reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Selected {
    /// The message it's in.
    pub message: String,
    /// The selection as HTML, for a reply that quotes it.
    pub html: String,
    /// Its text, for ⌘E.
    pub text: String,
    /// Its bounding box in the body's CSS px.
    pub bounds: Rect,
}

/// The latest layout of a body.
#[derive(Clone)]
pub(super) struct Laid {
    pub height: f32,
    pub generation: u64,
    /// The column width it was laid out for, in CSS px.
    pub width: f32,
    /// How much wider than the column it is, when it scrolls sideways (else 0), in CSS px.
    pub overflow: f32,
    /// How much it's shrunk to fit the column (1 when it fits).
    pub zoom: f32,
    /// Where its animated images are.
    pub animations: Vec<[f32; 4]>,
}

#[derive(Default)]
struct Entry {
    facts: Option<Facts>,
    laid: Option<Laid>,
    /// The last look a layout was asked for, and whether it blocked remote content.
    asked: Option<(Look, bool)>,
    bands: HashMap<u32, (u64, TextureHandle)>,
    asked_bands: HashSet<(u64, u32)>,
    hover: Option<Hit>,
    /// Where the last hover was asked. A hover is asked again only once the pointer or the body
    /// moves.
    hovered_at: Option<Pos2>,
    /// How far a body wider than its column is scrolled sideways, in CSS px.
    scroll_x: f32,
    /// Animated images' latest frames, by the part of the body they cover.
    patches: HashMap<[u32; 2], (u64, [f32; 4], TextureHandle)>,
    animation: AnimationState,
    /// Find's matches in the body, in the body's CSS px, and the query they're for.
    found: Vec<Rect>,
    found_for: Option<String>,
}

/// Where a body's animations are at, on the UI side.
#[derive(Default)]
struct AnimationState {
    /// A request is out.
    asked: bool,
    /// When the next frame is due, on egui's clock.
    due: Option<f64>,
    /// The animated images on screen at the last request.
    shown: Vec<usize>,
}

struct WorkerStart {
    job_receiver: Receiver<Job>,
    output_sender: Sender<Output>,
    ctx: egui::Context,
}

/// The UI's handle on the body worker, and the textures it painted.
pub struct Bodies {
    jobs: Sender<Job>,
    outputs: Receiver<Output>,
    /// Held until the first job starts the worker thread, so idle instances spawn no thread.
    worker_start: std::sync::Mutex<Option<WorkerStart>>,
    /// Jobs sent and not finished yet.
    pending: Arc<AtomicUsize>,
    /// The messages last sent to the pipeline, in order.
    messages: Vec<String>,
    entries: HashMap<String, Entry>,
    /// The account's Images setting.
    images: RemoteContentBlocking,
    /// The messages whose images ⌘K Show Images showed, until the setting changes.
    images_shown: HashSet<String>,
    /// The text selection, if any.
    selection: Option<Selected>,
    /// What find searches the bodies for, as last sent to the worker. The next two are find's
    /// current match among the bodies' matches, as sent and as this frame's painting found it.
    searched: Option<String>,
    current_match: Option<(String, usize)>,
    painted_match: Option<(String, usize)>,
    /// How the pipeline departs from the usual. The attachment viewer's messages have no
    /// detected dates, and a read-only snippet isn't classified.
    options: exo_mail::RenderOptions,
}

impl Bodies {
    /// Creates a handle that spawns its worker thread on the first job, loading the shared email
    /// fonts and repainting `ctx` whenever it has something new.
    pub fn new(ctx: &egui::Context) -> Self {
        let (jobs, job_receiver) = channel();
        let (output_sender, outputs) = channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_start = std::sync::Mutex::new(Some(WorkerStart {
            job_receiver,
            output_sender,
            ctx: ctx.clone(),
        }));
        Self {
            jobs,
            outputs,
            worker_start,
            pending,
            messages: Vec::new(),
            entries: HashMap::new(),
            images: RemoteContentBlocking::default(),
            images_shown: HashSet::new(),
            selection: None,
            searched: None,
            current_match: None,
            painted_match: None,
            options: exo_mail::RenderOptions::default(),
        }
    }

    fn ensure_worker(&self) {
        let start = self
            .worker_start
            .lock()
            .expect("worker-start lock is never poisoned")
            .take();
        let Some(WorkerStart {
            job_receiver,
            output_sender,
            ctx,
        }) = start
        else {
            return;
        };
        let (wake, worker_pending) = (self.jobs.clone(), self.pending.clone());
        // Views aren't `Send`, so the worker is built on its own thread. A panic outside a job
        // restarts the worker, the way an account's background restarts (`recovery`).
        let spawned = std::thread::Builder::new()
            .name("exo-bodies".into())
            .spawn(move || {
                let restarts = crate::recovery::Restarts::new(ctx.clone());
                loop {
                    let worker = Worker::new(wake.clone(), worker_pending.clone());
                    let run = std::panic::AssertUnwindSafe(|| {
                        worker.run(&job_receiver, &output_sender, &ctx)
                    });
                    if std::panic::catch_unwind(run).is_ok() {
                        return;
                    }
                    let Some(wait) = restarts.failed() else {
                        return;
                    };
                    std::thread::sleep(wait);
                    worker_pending.fetch_add(1, Ordering::SeqCst);
                    if output_sender.send(Output::Restarted).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = spawned {
            log::error!("starting the body renderer: {error}");
        }
    }

    fn send(&self, job: Job) {
        self.ensure_worker();
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.jobs.send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Renders as the attachment viewer shows a message, cleaned and nothing more, with no dates
    /// marked and nothing behind a `…` ([`exo_mail::RenderOptions`]).
    #[must_use]
    pub fn cleaned_only(mut self) -> Self {
        self.options = exo_mail::RenderOptions {
            detect_dates: false,
            split_quoted: false,
            ..self.options
        };
        self
    }

    /// Renders bodies unclassified, as the read-only snippet view shows a snippet. Nothing is
    /// marketing and no colors are tagged for the restyle ([`exo_mail::RenderOptions::classify`]).
    #[must_use]
    pub fn unclassified(mut self) -> Self {
        self.options.classify = false;
        self
    }

    /// Nothing is in flight, so what's on screen is final. For screenshots and tests.
    pub fn is_idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }

    /// Renders `messages` (one thread's, oldest first) unless they're what's rendered already.
    /// `account` is the account's address, whose domain is the user's own, and `loader` fetches
    /// the messages' images. Messages that stay keep their layout and painted bands until the new
    /// render replaces them, so a reply arriving neither blanks the thread nor moves its scroll.
    pub fn set_thread(&mut self, messages: &[&MessageDetail], account: &str, loader: &Loader) {
        if messages
            .iter()
            .map(|message| &message.id)
            .eq(&self.messages)
        {
            return;
        }
        self.messages = messages.iter().map(|message| message.id.clone()).collect();
        self.entries.retain(|id, _| self.messages.contains(id));
        for entry in self.entries.values_mut() {
            entry.asked = None;
            entry.hover = None;
            entry.hovered_at = None;
        }
        let own_domains = account
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_lowercase())
            .into_iter()
            .collect();
        let read_receipts = self.images == RemoteContentBlocking::None;
        self.send(Job::Thread {
            messages: messages.iter().map(|message| (*message).clone()).collect(),
            own_domains,
            loader: loader.clone(),
            read_receipts,
            options: self.options,
        });
    }

    /// Follows the account's Images setting. Changing it forgets which messages' images were
    /// shown.
    pub fn set_images(&mut self, images: RemoteContentBlocking) {
        if images != self.images {
            self.images = images;
            self.images_shown.clear();
        }
    }

    /// Shows message `id`'s remote images.
    pub fn show_images(&mut self, id: &str) {
        self.images_shown.insert(id.to_owned());
    }

    /// Whether `id`'s remote content is blocked, as it is under `Block all remote images` until
    /// shown.
    fn blocks_images(&self, id: &str) -> bool {
        self.images == RemoteContentBlocking::All && !self.images_shown.contains(id)
    }

    /// Whether ⌘K Show Images would show anything in `id`.
    pub fn can_show_images(&self, id: &str) -> bool {
        self.blocks_images(id) && self.facts(id).is_some_and(|facts| facts.remote_images)
    }

    /// Whether `id`'s header says `Email tracking blocked`, because it had read receipts and only
    /// `Show all images` loads them.
    pub fn tracking_blocked(&self, id: &str) -> bool {
        self.images != RemoteContentBlocking::None
            && self.facts(id).is_some_and(|facts| facts.tracking_blocked)
    }

    /// What the pipeline found in message `id`, once it ran.
    pub fn facts(&self, id: &str) -> Option<&Facts> {
        self.entries.get(id)?.facts.as_ref()
    }

    /// Message `id`'s body as it prints (`exo_mail::printable_body`), with its quoted part when
    /// `show_quoted`, its remote content blocked and its shrinking to fit as on screen. `None`
    /// until it's rendered.
    pub fn printable(&self, id: &str, show_quoted: bool) -> Option<String> {
        let facts = self.facts(id)?;
        // The parts of the render that a body is composed from, which are all that printing reads.
        let render = Render {
            unquoted_html: facts.unquoted_html.clone(),
            quoted_html: facts.quoted_html.clone(),
            has_quoted_content: facts.has_quoted,
            body_attributes: facts.body_attributes.clone(),
            unquoted_is_marketing: facts.unquoted_marketing,
            quoted_is_marketing: facts.quoted_marketing,
            ..Render::default()
        };
        let zoom = self
            .entries
            .get(id)
            .and_then(|entry| entry.laid.as_ref())
            .map_or(1.0, |laid| laid.zoom);
        Some(exo_mail::printable_body(
            &render,
            show_quoted,
            self.blocks_images(id),
            zoom,
        ))
    }

    /// The height of `id`'s body when shown with `look`, asking for a layout when the look
    /// changed. Until one arrives it's the last layout's height, or `None` the first time.
    pub fn height(&mut self, id: &str, look: Look) -> Option<f32> {
        let block_remote = self.blocks_images(id);
        let entry = self.entries.entry(id.to_owned()).or_default();
        entry.facts.as_ref()?;
        if entry.asked != Some((look, block_remote)) {
            entry.asked = Some((look, block_remote));
            let job = Job::Layout {
                id: id.to_owned(),
                look,
                block_remote,
            };
            self.send(job);
        }
        self.entries.get(id)?.laid.as_ref().map(|laid| laid.height)
    }

    /// How much wider than its column `id`'s body is, in CSS px, which is how far it scrolls
    /// sideways.
    pub fn overflow(&self, id: &str) -> f32 {
        self.entries
            .get(id)
            .and_then(|entry| entry.laid.as_ref())
            .map_or(0.0, |laid| laid.overflow)
    }

    /// The column width `id`'s body was last laid out for, and its content's width there, in CSS
    /// px.
    pub fn laid_width(&self, id: &str) -> Option<(f32, f32)> {
        let laid = self.entries.get(id)?.laid.as_ref()?;
        Some((laid.width, laid.width + laid.overflow))
    }

    /// How far `id`'s body is scrolled sideways, in CSS px.
    pub fn scroll_x(&self, id: &str) -> f32 {
        self.entries.get(id).map_or(0.0, |entry| entry.scroll_x)
    }

    /// Scrolls `id`'s body sideways to `x` CSS px (kept within its overflow), rounded to a device
    /// pixel at `scale`.
    pub fn set_scroll_x(&mut self, id: &str, x: f32, scale: f32) {
        let overflow = self.overflow(id);
        if let Some(entry) = self.entries.get_mut(id) {
            entry.scroll_x = (x.clamp(0.0, overflow) * scale).round() / scale;
        }
    }

    /// Whether `id`'s body has been laid out at least once.
    pub fn height_known(&self, id: &str) -> bool {
        self.entries
            .get(id)
            .is_some_and(|entry| entry.laid.is_some())
    }

    /// Paints the part of `id`'s body inside `clip`, with its top left at `origin` less how far
    /// it's scrolled sideways. It asks for the bands that aren't painted yet, drops those far off
    /// screen, and plays the animated images on screen. `opacity` fades the whole body (a
    /// marketing card in dark mode is 90% opaque).
    pub fn paint(&mut self, painter: &Painter, id: &str, origin: Pos2, clip: Rect, opacity: f32) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        let Some(laid) = entry.laid.clone() else {
            return;
        };
        let ppp = painter.pixels_per_point();
        // The worker painted the fractional x into the band, so the texture sits on the whole px
        // below it. Vertically it sits on a device pixel (glyphs don't move vertically in
        // Chromium).
        let left = origin.x.floor() - entry.scroll_x;
        let top = (origin.y * ppp).round() / ppp;
        let shown = (clip.y_range().min - top)..(clip.y_range().max - top);
        let visible = visible_bands(shown.start, shown.end, laid.height);
        if visible.is_empty() {
            // Out of view (painted only so find can search the thread), so what's painted stays.
            return;
        }
        let kept = visible.start.saturating_sub(KEPT_BANDS)..visible.end.saturating_add(KEPT_BANDS);
        entry.bands.retain(|band, _| kept.contains(band));
        entry.asked_bands.retain(|(_, band)| kept.contains(band));
        let kept_y = (kept.start as f32 * BAND)..(kept.end as f32 * BAND);
        entry
            .patches
            .retain(|_, (_, [_, y, _, h], _)| *y < kept_y.end && *y + *h > kept_y.start);
        let alpha = (opacity * 255.0).round() as u8;
        let tint = Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha);
        let whole = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        // A band covers the content's whole width, as far as a texture can go.
        let max_texture_side = painter.ctx().input(|input| input.max_texture_side);
        let max_width = max_texture_side as f32 / ppp - 1.0;
        let width = (laid.width + laid.overflow).min(max_width);
        let mut jobs = Vec::new();
        for band in visible {
            let band_top = top + band as f32 * BAND;
            let stale = match entry.bands.get(&band) {
                Some((generation, texture)) => {
                    let [w, h] = texture.size();
                    let at = Rect::from_min_size(
                        pos2(left, band_top),
                        vec2(w as f32 / ppp, h as f32 / ppp),
                    );
                    painter
                        .with_clip_rect(clip)
                        .image(texture.id(), at, whole, tint);
                    *generation != laid.generation
                }
                None => true,
            };
            if stale && entry.asked_bands.insert((laid.generation, band)) {
                jobs.push(Job::Paint {
                    id: id.to_owned(),
                    generation: laid.generation,
                    band,
                    width,
                });
            }
        }
        for (generation, [x, y, ..], texture) in entry.patches.values() {
            if *generation == laid.generation {
                let [w, h] = texture.size();
                let at = Rect::from_min_size(
                    pos2(left + x, top + y),
                    vec2(w as f32 / ppp, h as f32 / ppp),
                );
                painter
                    .with_clip_rect(clip)
                    .image(texture.id(), at, whole, tint);
            }
        }
        // Animated images play while they show. The worker is asked for their next frames when
        // they're due, and again whenever another one scrolls into view.
        let on_screen: Vec<usize> = laid
            .animations
            .iter()
            .enumerate()
            .filter(|(_, [_, y, _, h])| *y < shown.end && y + h > shown.start)
            .map(|(at, _)| at)
            .collect();
        if !on_screen.is_empty() && !entry.animation.asked {
            let now = painter.ctx().input(|input| input.time);
            let due = entry.animation.due.is_some_and(|due| due <= now);
            if due || on_screen != entry.animation.shown {
                entry.animation.asked = true;
                entry.animation.shown = on_screen;
                jobs.push(Job::Animate {
                    id: id.to_owned(),
                    generation: laid.generation,
                    now: Duration::from_secs_f64(now),
                    visible: shown,
                    max_texture_side,
                });
            } else if let Some(due) = entry.animation.due {
                painter.ctx().request_repaint_after_secs((due - now) as f32);
            }
        }
        for job in jobs {
            self.send(job);
        }
    }

    /// Hit-tests `at` (CSS px from the body's top left) in `id`, unless that's what it last did
    /// and the body hasn't changed since. [`Bodies::hovered`] has the answer a frame or two later.
    pub fn hover(&mut self, id: &str, at: Pos2) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        let at = at + vec2(entry.scroll_x, 0.0);
        if entry.hovered_at.replace(at) != Some(at) {
            self.send(Job::Hover {
                id: id.to_owned(),
                at,
            });
        }
    }

    /// What was under the pointer at the last [`Bodies::hover`] of `id`.
    pub fn hovered(&self, id: &str) -> Option<&Hit> {
        self.entries.get(id)?.hover.as_ref()
    }

    /// Forgets the hover over `id` (the pointer left it).
    pub fn unhover(&mut self, id: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.hover = None;
            entry.hovered_at = None;
        }
    }

    /// Handles hover hit-testing, cursor icon, multi-click (single/double/triple), drag selection,
    /// optional right-click context menu, and link clicks over a painted body.
    pub(crate) fn handle_pointer(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        area: PointerArea<'_>,
        state: &mut PointerState,
        clickable_date: impl FnOnce(&str) -> bool,
    ) -> PointerOutcome {
        let local = |at: Pos2| (at - area.origin).to_pos2();
        let mut outcome = PointerOutcome::default();
        let hover = area.pointer.filter(|at| area.body.contains(*at));
        match hover {
            Some(at) if response.hovered() => {
                outcome.hovered = true;
                // Every frame, not only while the pointer moves: scrolling, a header unfolding or
                // a relayout moves the body under a still pointer.
                self.hover(area.id, local(at));
                if let Some(hit) = self.hovered(area.id) {
                    ui.ctx()
                        .set_cursor_icon(cursor_for_hit(hit, clickable_date));
                }
            }
            _ => self.unhover(area.id),
        }
        // A double or triple click selects the word or paragraph as the button goes down, as in
        // Chromium (and a fourth click in a row is another triple click, as macOS counts them).
        let (pressed, time) = ui.input(|input| (input.pointer.primary_pressed(), input.time));
        if pressed
            && response.hovered()
            && let Some(at) = hover
        {
            let count = state.record_press(time, at);
            let granularity = match count {
                1 => None,
                2 => Some(Granularity::Word),
                _ => Some(Granularity::Paragraph),
            };
            if let Some(granularity) = granularity {
                self.select_from(area.id, local(at), granularity);
                state.selecting = Some(area.id.to_owned());
                outcome.started_selection = true;
            }
        }
        let multi_click = state.is_multi_click();
        // A selection starts where the button went down, not where the drag was recognized. After
        // a double or triple click, the drag extends that word or paragraph.
        if response.drag_started() && !multi_click {
            state.selecting = None;
            if let Some(at) = ui
                .input(|input| input.pointer.press_origin())
                .filter(|at| area.body.contains(*at))
            {
                self.select_from(area.id, local(at), Granularity::Character);
                state.selecting = Some(area.id.to_owned());
                outcome.started_selection = true;
            }
        }
        if response.dragged()
            && state.selecting.as_deref() == Some(area.id)
            && let Some(at) = response.interact_pointer_pos()
        {
            self.select_to(area.id, local(at));
        }
        if area.context_menu
            && response.secondary_clicked()
            && let Some(at) = response
                .interact_pointer_pos()
                .filter(|at| area.body.contains(*at))
        {
            exo_ui::claim_right_click(ui.ctx());
            self.context_menu(area.id, local(at), at);
        }
        if response.clicked()
            && !multi_click
            && let Some(at) = response
                .interact_pointer_pos()
                .filter(|at| area.body.contains(*at))
        {
            self.clear_selection();
            // ⌘ on a link opens it in the background. What the click hit comes back later.
            state.click_modifiers = ui.input(|input| input.modifiers);
            self.click(area.id, local(at));
        }
        outcome
    }

    /// A click at `at` in `id`. [`Bodies::poll`] reports what it hit.
    pub fn click(&self, id: &str, at: Pos2) {
        self.send(Job::Click {
            id: id.to_owned(),
            at: self.scrolled(id, at),
        });
    }

    /// Starts a text selection at `at` in `id`, clearing any other. One click places a caret,
    /// and a double or triple click selects the word or paragraph there.
    pub fn select_from(&self, id: &str, at: Pos2, granularity: Granularity) {
        self.send(Job::SelectFrom {
            id: id.to_owned(),
            at: self.scrolled(id, at),
            granularity,
        });
    }

    /// Extends the selection to `at` in `id`.
    pub fn select_to(&self, id: &str, at: Pos2) {
        self.send(Job::SelectTo {
            id: id.to_owned(),
            at: self.scrolled(id, at),
        });
    }

    /// `at` (CSS px from the body's top left on screen) in the body's own coordinates.
    fn scrolled(&self, id: &str, at: Pos2) -> Pos2 {
        at + vec2(self.scroll_x(id), 0.0)
    }

    /// Clears the text selection.
    pub fn clear_selection(&self) {
        self.send(Job::ClearSelection);
    }

    /// Asks for the selected text. [`Bodies::poll`] reports it if there is any.
    pub fn copy(&self) {
        self.send(Job::Copy);
    }

    /// The text selected in message `id`, as HTML ([`exo_mail::BodyView::selected_html`]), for
    /// a reply to quote.
    pub fn selected_html(&self, id: &str) -> Option<&str> {
        self.selection
            .as_ref()
            .filter(|selected| selected.message == id)
            .map(|selected| selected.html.as_str())
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection
            .as_ref()
            .map(|selected| selected.text.as_str())
    }

    /// The message holding the text selection, and the selection's bounding box in its body's
    /// CSS px.
    pub fn selection_bounds(&self) -> Option<(&str, Rect)> {
        self.selection
            .as_ref()
            .map(|selected| (selected.message.as_str(), selected.bounds))
    }

    /// A right-click at `at` in `id`, `screen` on screen. [`Bodies::poll`] reports what it
    /// landed on.
    pub fn context_menu(&self, id: &str, at: Pos2, screen: Pos2) {
        self.send(Job::ContextMenu {
            id: id.to_owned(),
            at: self.scrolled(id, at),
            screen,
        });
    }

    /// Finds where in-message link `#name` points in message `id`. [`Bodies::poll`] reports it
    /// if it points anywhere.
    pub fn find_anchor(&self, id: &str, name: &str) {
        self.send(Job::Anchor {
            id: id.to_owned(),
            name: name.to_owned(),
        });
    }

    /// Renders the `messages` of conversation `thread` (oldest first) for what they hold (their
    /// links, their new parts), which [`Bodies::poll`] reports. `account` is as for
    /// [`Bodies::set_thread`]. The open thread's bodies stay as they are.
    pub fn render(&self, thread: &str, messages: &[&MessageDetail], account: &str) {
        let own_domains = account
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_lowercase())
            .into_iter()
            .collect();
        self.send(Job::Render {
            thread: thread.to_owned(),
            messages: messages.iter().map(|message| (*message).clone()).collect(),
            own_domains,
        });
    }

    /// Adds find's matches in `id`'s body, painted with its top left at `origin`, at this point in
    /// the page's order ([`exo_ui::find::add_matches`]), or says they're still to come while the
    /// worker hasn't searched it (or rendered it) yet.
    pub fn add_matches(&mut self, ctx: &egui::Context, id: &str, origin: Pos2) {
        let entry = self
            .entries
            .get(id)
            .filter(|entry| entry.found_for == self.searched);
        let Some(entry) = entry else {
            // With the worker idle nothing is coming (a message it has no render for).
            if !self.is_idle() {
                exo_ui::find::pending(ctx);
            }
            return;
        };
        let offset = origin.to_vec2() - vec2(entry.scroll_x, 0.0);
        let boxes: Vec<Rect> = entry
            .found
            .iter()
            .map(|found| found.translate(offset))
            .collect();
        if let Some(index) = exo_ui::find::add_matches(ctx, exo_ui::id(ctx, ("body", id)), &boxes) {
            self.painted_match = Some((id.to_owned(), index));
        }
    }

    /// Hands the worker what find searches for, and where its current match is, as the last
    /// frame left them. When find stops, its current match in a body becomes the selection.
    fn sync_find(&mut self, ctx: &egui::Context) {
        let painted = self.painted_match.take();
        let query = exo_ui::find::query(ctx);
        if query != self.searched {
            if let (None, Some((id, index))) = (&query, self.current_match.take()) {
                self.send(Job::SelectMatch { id, index });
            }
            for entry in self.entries.values_mut() {
                entry.found.clear();
            }
            self.searched.clone_from(&query);
            self.current_match = None;
            self.send(Job::Find(query));
        } else if painted != self.current_match {
            self.current_match.clone_from(&painted);
            self.send(Job::CurrentMatch(painted));
        }
    }

    /// Takes in what the worker finished, uploading painted bands as textures. Call once a frame,
    /// before painting.
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<Event> {
        self.sync_find(ctx);
        let mut events = Vec::new();
        while let Ok(output) = self.outputs.try_recv() {
            match output {
                Output::Rendered(facts) => {
                    for (id, facts) in facts {
                        self.entries.entry(id).or_default().facts = Some(facts);
                    }
                }
                Output::Laid { id, laid } => {
                    if let Some(entry) = self.entries.get_mut(&id) {
                        entry
                            .asked_bands
                            .retain(|(asked, _)| *asked == laid.generation);
                        entry.hovered_at = None;
                        entry
                            .patches
                            .retain(|_, (generation, ..)| *generation == laid.generation);
                        entry.animation.shown.clear();
                        entry.scroll_x = entry.scroll_x.min(laid.overflow);
                        entry.laid = Some(laid);
                    }
                }
                Output::Painted {
                    id,
                    generation,
                    band,
                    image,
                } => {
                    let Some(entry) = self.entries.get_mut(&id) else {
                        continue;
                    };
                    if entry
                        .laid
                        .as_ref()
                        .is_none_or(|laid| laid.generation != generation)
                    {
                        continue;
                    }
                    let color = ColorImage::from_rgba_premultiplied(
                        [image.width as usize, image.height as usize],
                        &image.rgba,
                    );
                    let texture = ctx.load_texture(
                        format!("body {id} {band}"),
                        color,
                        TextureOptions::NEAREST,
                    );
                    entry.bands.insert(band, (generation, texture));
                }
                Output::Animated {
                    id,
                    generation,
                    patches,
                    next,
                } => {
                    let Some(entry) = self.entries.get_mut(&id) else {
                        continue;
                    };
                    entry.animation.asked = false;
                    entry.animation.due = next.map(|next| next.as_secs_f64());
                    if entry
                        .laid
                        .as_ref()
                        .is_none_or(|laid| laid.generation != generation)
                    {
                        continue;
                    }
                    for (region, image) in patches {
                        let color = ColorImage::from_rgba_premultiplied(
                            [image.width as usize, image.height as usize],
                            &image.rgba,
                        );
                        let key = [region[0], region[1]].map(|edge| edge as u32);
                        entry.patches.retain(|existing, (_, old, _)| {
                            *existing == key
                                || old[0] >= region[0] + region[2]
                                || old[0] + old[2] <= region[0]
                                || old[1] >= region[1] + region[3]
                                || old[1] + old[3] <= region[1]
                        });
                        // A new frame goes into the texture the last one is in, rather than a new
                        // texture every frame.
                        match entry.patches.get_mut(&key) {
                            Some((patch_generation, patch_region, texture))
                                if texture.size() == color.size =>
                            {
                                texture.set(color, TextureOptions::NEAREST);
                                (*patch_generation, *patch_region) = (generation, region);
                            }
                            _ => {
                                let texture = ctx.load_texture(
                                    format!("body {id} frame {key:?}"),
                                    color,
                                    TextureOptions::NEAREST,
                                );
                                entry.patches.insert(key, (generation, region, texture));
                            }
                        }
                    }
                }
                Output::Hovered { id, hit } => {
                    if let Some(entry) = self.entries.get_mut(&id) {
                        entry.hover = hit;
                    }
                }
                Output::Clicked { id, hit } => {
                    events.extend(hit.map(|hit| Event::Clicked { id, hit }))
                }
                Output::Copied(text) => {
                    events.push(text.map_or(Event::NothingSelected, Event::Copied))
                }
                Output::Restarted => {
                    self.messages.clear();
                    self.entries.clear();
                    self.selection = None;
                    (self.searched, self.current_match) = (None, None);
                }
                Output::Conversation { thread, facts } => {
                    events.push(Event::Rendered { thread, facts })
                }
                Output::Selected(selection) => {
                    if selection != self.selection
                        && let Some(selected) = &selection
                        && let Some(visible) = self
                            .facts(&selected.message)
                            .map(|facts| facts.visible_chars)
                            .filter(|visible| *visible > 0)
                    {
                        let chars = selected.text.chars().count();
                        if chars > 500 || chars as f64 / visible as f64 > 0.8 {
                            events.push(Event::SelectedMuch);
                        }
                    }
                    self.selection = selection;
                }
                Output::Anchored { id, top } => {
                    events.extend(top.map(|top| Event::Anchored { id, top }))
                }
                Output::Found { id, query, boxes } => {
                    // An answer for an earlier query is outdated, because the worker searches
                    // again.
                    if query == self.searched {
                        let entry = self.entries.entry(id).or_default();
                        (entry.found, entry.found_for) = (boxes, query);
                    }
                }
                Output::ContextMenu {
                    id,
                    at,
                    screen,
                    hit,
                    selection,
                } => {
                    // A line's ascent down from the top of the selection's box, which is about
                    // where its baseline is.
                    let baseline = |bounds: Rect| {
                        screen + (bounds.min - at) + vec2(0.0, 0.75 * bounds.height().min(20.0))
                    };
                    events.push(Event::ContextMenu {
                        id,
                        at: screen,
                        hit,
                        selection: selection.map(|(text, bounds)| (text, baseline(bounds))),
                    });
                }
            }
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
        events
    }
}

/// The cursor Chromium's `cursor: auto` picks for `hit`: an I-beam over text, a hand over links,
/// quote toggles, inline images, and clickable dates.
fn cursor_for_hit(hit: &Hit, clickable_date: impl FnOnce(&str) -> bool) -> egui::CursorIcon {
    match hit {
        Hit::Text => egui::CursorIcon::Text,
        Hit::Link { .. } | Hit::QuoteToggle | Hit::InlineImage { .. } => {
            egui::CursorIcon::PointingHand
        }
        Hit::Date { text, .. } => {
            if clickable_date(text) {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::Text
            }
        }
    }
}

/// The bands of a body `height` px tall that show between `from` and `to` px below its top.
fn visible_bands(from: f32, to: f32, height: f32) -> Range<u32> {
    let count = (height / BAND).ceil().max(1.0);
    let band = |y: f32| y.clamp(0.0, count) as u32;
    band((from / BAND).floor())..band((to / BAND).ceil())
}

#[cfg(test)]
mod tests {
    use super::{worker::clip_animated_region, *};

    #[test]
    fn the_images_setting_decides_what_shows_and_changing_it_forgets_shown_messages() {
        let mut bodies = Bodies::new(&egui::Context::default());
        let facts = Facts {
            tracking_blocked: true,
            remote_images: true,
            ..Facts::default()
        };
        bodies.entries.entry("m".into()).or_default().facts = Some(facts);
        bodies.set_images(RemoteContentBlocking::None);
        assert!(
            !bodies.tracking_blocked("m") && !bodies.can_show_images("m"),
            "everything loads"
        );
        bodies.set_images(RemoteContentBlocking::ReduceTracking);
        assert!(
            bodies.tracking_blocked("m") && !bodies.can_show_images("m"),
            "the read receipt alone is blocked"
        );
        bodies.set_images(RemoteContentBlocking::All);
        assert!(
            bodies.tracking_blocked("m")
                && bodies.blocks_images("m")
                && bodies.can_show_images("m")
        );
        bodies.show_images("m");
        assert!(
            !bodies.blocks_images("m") && !bodies.can_show_images("m"),
            "shown"
        );
        assert!(
            bodies.tracking_blocked("m"),
            "its read receipt stays blocked"
        );
        bodies.set_images(RemoteContentBlocking::ReduceTracking);
        bodies.set_images(RemoteContentBlocking::All);
        assert!(
            bodies.blocks_images("m"),
            "a new setting forgets what was shown"
        );
    }

    #[test]
    fn the_index_text_leaves_out_quotes_and_signatures() {
        let source = |id: &str, html: &str, replying_to: Option<usize>| exo_store::TextSource {
            id: id.into(),
            body: exo_store::Body {
                html: Some(html.into()),
                text: None,
            },
            from_email: "ann@example.com".into(),
            from_name: "Ann Lee".into(),
            rfc822_id: format!("{id}@example.com"),
            attachment_names: Vec::new(),
            replying_to,
        };
        let first = "<div>Shall we try the boathouse on Friday? The walrus exhibit opens then.</div><div class=\"gmail_signature\">Ann Lee · Zanzibar Shipping</div>";
        let reply = format!(
            "<div>Noon works too.</div><div class=\"gmail_quote\"><div class=\"gmail_attr\">On Mon, Nov 16, 2026 at 9:00 AM Ann Lee &lt;ann@example.com&gt; wrote:</div><blockquote class=\"gmail_quote\">{first}</blockquote></div>"
        );
        let texts = visible_texts(&[source("a", first, None), source("b", &reply, Some(0))]);
        assert!(
            texts[0].contains("walrus") && !texts[0].contains("Zanzibar"),
            "{texts:?}"
        );
        assert!(
            texts[1].contains("Noon works too")
                && !texts[1].contains("walrus")
                && !texts[1].contains("Zanzibar"),
            "{texts:?}"
        );
        // Read alone, the reply keeps what it quotes.
        assert!(visible_texts(&[source("b", &reply, None)])[0].contains("walrus"));
    }

    /// A store on mock-servers with the app's visible text, synced.
    async fn indexed_store(
        server: &mock_servers::MockServer,
        dir: &std::path::Path,
    ) -> exo_store::Store {
        let options = exo_store::Options {
            quota: None,
            ..exo_store::Options::default()
        };
        let store = exo_store::Store::open_with(
            dir,
            "maya@brightmoor.co",
            server.endpoints(),
            gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into()),
            options,
        )
        .await
        .expect("opens");
        store.index_text_with(Arc::new(visible_texts));
        while store.sync().await.expect("syncs").backfilling {}
        store
    }

    #[test]
    fn search_finds_what_a_message_shows_and_not_what_it_quotes() {
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        runtime.block_on(async {
            let server = mock_servers::MockServer::start(mock_servers::Config::default()).await.expect("the mock starts");
            let dir = std::env::temp_dir().join(format!("exo-index-text-search-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let store = indexed_store(&server, &dir).await;
            let http = reqwest::Client::new();
            let deliver = server.url().join("/__mock/deliver").expect("a URL");
            // Ann asks, signing with her company. Ben answers above her message, which he quotes
            // whole.
            let first = "<div>Shall we try the boathouse on Friday? The walrus exhibit opens then.</div><div class=\"gmail_signature\">Ann Lee · Zanzibar Shipping</div>";
            let asked: serde_json::Value = http.post(deliver.clone()).json(&serde_json::json!({ "from": "Ann Lee <ann@example.com>", "subject": "Friday lunch", "html": first })).send().await.expect("delivers").json().await.expect("a message");
            let reply = format!("<div>Noon by the pergola works too.</div><div class=\"gmail_quote\"><div class=\"gmail_attr\">On Mon, Nov 16, 2026 at 9:00 AM Ann Lee &lt;ann@example.com&gt; wrote:</div><blockquote class=\"gmail_quote\">{first}</blockquote></div>");
            http.post(deliver).json(&serde_json::json!({ "from": "Ben Ortiz <ben@example.com>", "html": reply, "threadId": asked["threadId"] })).send().await.expect("delivers");
            while store.sync().await.expect("syncs").backfilling {}
            store.index_texts().await.expect("indexes");
            // The local index alone (offline). Online, Gmail's search adds what it finds.
            let found = async |query: &str| -> Vec<String> {
                let page = store.search(&mut exo_store::Search::new(query), 50, false, 15).await.expect("searches");
                page.results.into_iter().map(|result| result.thread.subject).collect()
            };
            assert_eq!(found("pergola").await, ["Friday lunch"], "the reply's own words");
            assert_eq!(found("walrus").await, ["Friday lunch"], "Ann's, which the reply quotes");
            assert!(found("zanzibar").await.is_empty(), "not her signature, there or quoted");
            drop(store);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// What it costs to work out the visible text for the search index
    /// (`Store::index_text_with`). It prints the pipeline's time per 100 conversations, first for
    /// the mock's mail as it is, then for 100 conversations of two messages (a reply quoting the
    /// first) at sizes of real mail.
    ///
    /// `cargo test --release -p exo --lib -- --ignored --nocapture index_text_cost`
    #[test]
    #[ignore = "a measurement"]
    fn index_text_cost() {
        type Spent = Arc<std::sync::Mutex<(std::time::Duration, usize, usize)>>;
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        runtime.block_on(async {
            let server = mock_servers::MockServer::start(mock_servers::Config::default()).await.expect("the mock starts");
            let dir = std::env::temp_dir().join(format!("exo-index-text-cost-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let options = exo_store::Options { quota: None, ..exo_store::Options::default() };
            let store = exo_store::Store::open_with(&dir, "maya@brightmoor.co", server.endpoints(), gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into()), options).await.expect("opens");
            let spent = Spent::default();
            let counted = spent.clone();
            store.index_text_with(Arc::new(move |sources: &[exo_store::TextSource]| {
                let clock = std::time::Instant::now();
                let texts = visible_texts(sources);
                let mut spent = counted.lock().expect("lock");
                *spent = (spent.0 + clock.elapsed(), spent.1 + 1, spent.2 + sources.len());
                texts
            }));
            // What the pipeline took for what's queued.
            let drain = async |label: &str| {
                store.index_texts().await.expect("indexes");
                let (pipeline, threads, messages) = std::mem::take(&mut *spent.lock().expect("lock"));
                let per = |time: std::time::Duration, n: usize| time / u32::try_from(n.max(1)).unwrap_or(1);
                println!("{label}: {threads} conversations ({messages} messages read): {pipeline:.0?}, {:.0?} per 100 conversations, {:.1?} a message", per(pipeline * 100, threads), per(pipeline, messages));
            };
            while store.sync().await.expect("syncs").backfilling {}
            for thread in store.list(&exo_store::ListId::All, None, 1000).await.expect("lists").threads {
                store.load_bodies(&thread.id).await.expect("fetches");
            }
            drain("the mock's mail").await;
            let http = reqwest::Client::new();
            let deliver = server.url().join("/__mock/deliver").expect("a URL");
            for (label, pad) in [("personal mail, 15 KB", 13_000), ("newsletters, 40 KB", 37_000)] {
                const CELL: &str = r#"<td style="padding:0 0 16px;font-family:-apple-system,Helvetica,Arial,sans-serif;font-size:16px;line-height:24px;color:#374151"></td>"#;
                let padding = format!("<div style=\"display:none\"><table><tr>{}</tr></table></div>", CELL.repeat(pad / CELL.len()));
                for n in 0..100 {
                    let first = format!("<div>Can we move the review of item {n} to Thursday? The deck is nearly done.</div>{padding}<div class=\"gmail_signature\">Ann Lee</div>");
                    let asked: serde_json::Value = http.post(deliver.clone()).json(&serde_json::json!({ "from": "Ann Lee <ann@example.com>", "subject": format!("Review {n}"), "html": first })).send().await.expect("delivers").json().await.expect("a message");
                    let reply = format!("<div>Thursday works.</div><div class=\"gmail_quote\"><div class=\"gmail_attr\">On Mon, Ann Lee wrote:</div><blockquote class=\"gmail_quote\">{first}</blockquote></div>");
                    http.post(deliver.clone()).json(&serde_json::json!({ "from": "Ben Ortiz <ben@example.com>", "html": reply, "threadId": asked["threadId"] })).send().await.expect("delivers");
                }
                while store.sync().await.expect("syncs").backfilling {}
                drain(label).await;
            }
            drop(store);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn paints_only_the_bands_on_screen() {
        assert_eq!(
            visible_bands(0.0, 300.0, 100.0),
            0..1,
            "a short body is one band"
        );
        assert_eq!(visible_bands(600.0, 1400.0, 5000.0), 1..3);
        assert_eq!(visible_bands(-200.0, -10.0, 5000.0), 0..0, "above the body");
        assert_eq!(
            visible_bands(9000.0, 9800.0, 5000.0),
            10..10,
            "below the body"
        );
        // A hostile `height: 1e11px` costs the bands on screen, not 200 million of them.
        assert!(visible_bands(5e10, 5e10 + 800.0, 1e11).len() <= 3);
    }

    #[test]
    fn clips_and_clamps_animated_regions_to_viewport_and_max_texture() {
        assert_eq!(
            clip_animated_region([10.2, 20.4, 32.0, 32.0], &(0.0..800.0), 600.0, 2.0, 8192),
            Some([10.0, 20.0, 33.0, 33.0]),
            "on-screen GIF snaps to whole CSS pixels"
        );
        assert_eq!(
            clip_animated_region(
                [0.0, 0.0, 2000.0, 50_000.0],
                &(1200.0..2000.0),
                640.0,
                2.0,
                8192
            ),
            Some([0.0, 1200.0, 640.0, 800.0]),
            "tall tiled background is clipped to visible y-range and body_width"
        );
        assert_eq!(
            clip_animated_region(
                [0.0, -300.0, 600.0, 500.0],
                &(-150.0..700.0),
                600.0,
                2.0,
                8192
            ),
            Some([0.0, 0.0, 600.0, 200.0]),
            "negative y and negative visible.start clamp at 0.0"
        );
        assert_eq!(
            clip_animated_region(
                [0.0, 0.0, 5000.0, 5000.0],
                &(0.0..5000.0),
                5000.0,
                2.0,
                4096
            ),
            Some([0.0, 0.0, 2047.0, 2047.0]),
            "pixel dimensions clamp to max_texture_side at retina scale"
        );
        assert_eq!(
            clip_animated_region([0.0, 900.0, 100.0, 100.0], &(0.0..800.0), 600.0, 2.0, 8192),
            None,
            "below viewport is skipped"
        );
        assert_eq!(
            clip_animated_region([650.0, 100.0, 50.0, 50.0], &(0.0..800.0), 600.0, 2.0, 8192),
            None,
            "past body_width is skipped"
        );
        assert_eq!(
            clip_animated_region([10.0, 10.0, 0.0, 50.0], &(0.0..800.0), 600.0, 2.0, 8192),
            None,
            "zero-width animation is skipped"
        );
    }

    #[test]
    fn pointer_state_tracks_multi_clicks_and_picks_cursors() {
        let mut state = PointerState::default();
        assert_eq!(state.record_press(1.0, pos2(10.0, 10.0)), 1);
        assert!(!state.is_multi_click());
        assert_eq!(state.record_press(1.2, pos2(11.0, 11.0)), 2);
        assert!(state.is_multi_click());
        assert_eq!(state.record_press(1.4, pos2(11.0, 12.0)), 3);
        assert_eq!(state.record_press(1.5, pos2(11.0, 12.0)), 3, "caps at 3");
        assert_eq!(
            state.record_press(2.5, pos2(11.0, 12.0)),
            1,
            "delay resets count"
        );
        assert_eq!(
            state.record_press(2.6, pos2(30.0, 12.0)),
            1,
            "distance resets count"
        );

        assert_eq!(
            cursor_for_hit(&Hit::Text, |_| false),
            egui::CursorIcon::Text
        );
        assert_eq!(
            cursor_for_hit(
                &Hit::Link {
                    href: "https://example.com".into()
                },
                |_| false
            ),
            egui::CursorIcon::PointingHand
        );
        assert_eq!(
            cursor_for_hit(&Hit::QuoteToggle, |_| false),
            egui::CursorIcon::PointingHand
        );
        assert_eq!(
            cursor_for_hit(
                &Hit::Date {
                    text: "tomorrow".into(),
                    bounds: [0.0; 4],
                },
                |_| true
            ),
            egui::CursorIcon::PointingHand
        );
        assert_eq!(
            cursor_for_hit(
                &Hit::Date {
                    text: "someday".into(),
                    bounds: [0.0; 4],
                },
                |_| false
            ),
            egui::CursorIcon::Text
        );
    }
}
