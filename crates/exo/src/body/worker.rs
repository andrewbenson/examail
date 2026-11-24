//! Background worker thread hosting Blitz [`BodyView`] instances and the [`Job`] / [`Output`]
//! protocol connecting it to [`super::Bodies`].

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, Sender},
    },
    time::Duration,
};

use egui::{Pos2, Rect, pos2, vec2};
use exo_mail::{BodyView, Fonts, Region, Render, ViewOptions};
use exo_store::MessageDetail;

use super::{
    BAND, Facts, Granularity, Hit, Laid, Look, Selected, fetch,
    index::{facts, render_thread},
};
use crate::account::Loader;

pub(super) enum Job {
    /// A new thread. With `read_receipts` (`Show all images`), its messages load their read
    /// receipts when first shown.
    Thread {
        messages: Vec<MessageDetail>,
        own_domains: Vec<String>,
        loader: Loader,
        read_receipts: bool,
        options: exo_mail::RenderOptions,
    },
    /// Lay a body out, loading nothing remote when `block_remote`.
    Layout {
        id: String,
        look: Look,
        block_remote: bool,
    },
    /// Paint band `band`, `width` CSS px wide (the content's width, capped at the GPU's texture
    /// limit).
    Paint {
        id: String,
        generation: u64,
        band: u32,
        width: f32,
    },
    /// Play the animated images showing in `visible` up to `now`.
    Animate {
        id: String,
        generation: u64,
        now: Duration,
        visible: Range<f32>,
        max_texture_side: usize,
    },
    Hover {
        id: String,
        at: Pos2,
    },
    Click {
        id: String,
        at: Pos2,
    },
    SelectFrom {
        id: String,
        at: Pos2,
        granularity: Granularity,
    },
    SelectTo {
        id: String,
        at: Pos2,
    },
    ClearSelection,
    Copy,
    /// Find where `#name` points in message `id`.
    Anchor {
        id: String,
        name: String,
    },
    /// A right-click at `at` in message `id`, at `screen` on screen. Unless it's on the
    /// selection, it selects the word under it, or clears the selection when there's no text
    /// there.
    ContextMenu {
        id: String,
        at: Pos2,
        screen: Pos2,
    },
    /// Render another conversation's `messages`, leaving the open thread be.
    Render {
        thread: String,
        messages: Vec<MessageDetail>,
        own_domains: Vec<String>,
    },
    /// An image a body asked for arrived.
    Wake,
    /// Find (⌘F) searches every body for this, or stops.
    Find(Option<String>),
    /// Find's current match is now match `.1` in message `.0`, or there's none.
    CurrentMatch(Option<(String, usize)>),
    /// Find stopped, and its current match (match `index` in message `id`) becomes the selection.
    SelectMatch {
        id: String,
        index: usize,
    },
}

impl Job {
    /// Whether `later`, queued after this job, makes this one pointless.
    fn superseded_by(&self, later: &Self) -> bool {
        match (self, later) {
            // A later layout of the same body paints all of it again.
            (Self::Layout { id, .. } | Self::Paint { id, .. }, Self::Layout { id: later, .. })
            | (Self::Hover { id, .. }, Self::Hover { id: later, .. }) => id == later,
            // A new thread replaces everything about the old one.
            (
                Self::Layout { .. } | Self::Paint { .. } | Self::Hover { .. },
                Self::Thread { .. },
            ) => true,
            (Self::Find(_), Self::Find(_)) | (Self::CurrentMatch(_), Self::CurrentMatch(_)) => true,
            _ => false,
        }
    }

    /// The message the job is about, if it's about one.
    fn message(&self) -> Option<&str> {
        match self {
            Self::Layout { id, .. }
            | Self::Paint { id, .. }
            | Self::Animate { id, .. }
            | Self::Hover { id, .. }
            | Self::Click { id, .. }
            | Self::SelectFrom { id, .. }
            | Self::SelectTo { id, .. }
            | Self::Anchor { id, .. }
            | Self::SelectMatch { id, .. }
            | Self::ContextMenu { id, .. } => Some(id),
            Self::Thread { .. }
            | Self::ClearSelection
            | Self::Copy
            | Self::Render { .. }
            | Self::Wake
            | Self::Find(_)
            | Self::CurrentMatch(_) => None,
        }
    }
}

pub(super) enum Output {
    Rendered(Vec<(String, Facts)>),
    Laid {
        id: String,
        laid: Laid,
    },
    Painted {
        id: String,
        generation: u64,
        band: u32,
        image: exo_mail::Image,
    },
    /// New frames of animated images, painted over the bands. Each patch is a part of the body
    /// (`[left, top, width, height]`, whole CSS px) and its pixels.
    Animated {
        id: String,
        generation: u64,
        patches: Vec<([f32; 4], exo_mail::Image)>,
        next: Option<Duration>,
    },
    Hovered {
        id: String,
        hit: Option<Hit>,
    },
    Clicked {
        id: String,
        hit: Option<Hit>,
    },
    Copied(Option<String>),
    /// The worker panicked outside a job and started again without the views it had, so the
    /// thread has to be sent again.
    Restarted,
    Conversation {
        thread: String,
        facts: Vec<(String, Facts)>,
    },
    /// The selection changed, or went.
    Selected(Option<Selected>),
    Anchored {
        id: String,
        top: Option<f32>,
    },
    /// Find's matches for `query` in the body of message `id`, as boxes in the body's CSS px.
    Found {
        id: String,
        query: Option<String>,
        boxes: Vec<Rect>,
    },
    /// What a right-click hit, and the selection's text and box at that moment (in the body's CSS
    /// px).
    ContextMenu {
        id: String,
        at: Pos2,
        screen: Pos2,
        hit: exo_mail::ContextHit,
        selection: Option<(String, Rect)>,
    },
}

/// One message's view on the worker.
struct View {
    body: BodyView,
    look: Look,
    block_remote: bool,
    height: f32,
    generation: u64,
}

impl View {
    /// Its latest layout, for message `id`.
    fn laid(&self, id: &str) -> Output {
        let overflow = if self.body.scrolls() {
            self.body.content_width() - self.body.width()
        } else {
            0.0
        };
        let laid = Laid {
            height: self.height,
            generation: self.generation,
            width: self.body.width(),
            overflow,
            zoom: self.body.zoom(),
            animations: self.body.animations(),
        };
        Output::Laid {
            id: id.to_owned(),
            laid,
        }
    }
}

/// The worker thread's state: every render and view of the open thread.
pub(super) struct Worker {
    fonts: Fonts,
    renders: HashMap<String, Render>,
    views: HashMap<String, View>,
    fetch: Option<Arc<dyn exo_mail::Fetch>>,
    /// The thread's messages load their read receipts when first shown.
    read_receipts: bool,
    /// The messages that have loaded them since the thread was opened.
    receipts_loaded: HashSet<String>,
    /// The view holding the text selection.
    selected: Option<String>,
    /// What find searches for, and its current match (the message, and which of its matches).
    query: Option<String>,
    current_match: Option<(String, usize)>,
    generation: u64,
    /// Lets image fetches wake the worker.
    wake: Sender<Job>,
    pending: Arc<AtomicUsize>,
}

impl Worker {
    pub(super) fn new(wake: Sender<Job>, pending: Arc<AtomicUsize>) -> Self {
        Self {
            fonts: Fonts::load(),
            renders: HashMap::new(),
            views: HashMap::new(),
            fetch: None,
            read_receipts: false,
            receipts_loaded: HashSet::new(),
            selected: None,
            query: None,
            current_match: None,
            generation: 0,
            wake,
            pending,
        }
    }

    pub(super) fn run(
        mut self,
        jobs: &Receiver<Job>,
        outputs: &Sender<Output>,
        ctx: &egui::Context,
    ) {
        while let Ok(first) = jobs.recv() {
            // While a window resizes, layouts queue up faster than they run, so only the newest
            // layout (and hover) of each body in the queue is worth doing.
            let mut batch = vec![first];
            batch.extend(jobs.try_iter());
            let superseded: Vec<bool> = batch
                .iter()
                .enumerate()
                .map(|(at, job)| batch[at + 1..].iter().any(|later| job.superseded_by(later)))
                .collect();
            for (job, skip) in batch.into_iter().zip(superseded) {
                // A job answers with an output for each thing it changed. Each output is one unit
                // of `pending` on the UI side, so the job's own unit is swapped for them.
                let produced = if skip {
                    Vec::new()
                } else {
                    self.handle_caught(job)
                };
                self.pending.fetch_add(produced.len(), Ordering::SeqCst);
                for output in produced {
                    if outputs.send(output).is_err() {
                        return;
                    }
                }
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
            ctx.request_repaint();
        }
    }

    /// [`Self::handle`], surviving a panic in the body engine. The message the job was about
    /// loses its view and render (its body stays a placeholder), and the other bodies carry on.
    /// Otherwise one hostile body would leave every body blank until Exo restarts.
    fn handle_caught(&mut self, job: Job) -> Vec<Output> {
        let id = job.message().map(str::to_owned);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle(job))).unwrap_or_else(
            |_| {
                log::error!("the body renderer panicked on message {id:?}");
                if let Some(id) = id {
                    self.views.remove(&id);
                    self.renders.remove(&id);
                }
                Vec::new()
            },
        )
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn handle(&mut self, job: Job) -> Vec<Output> {
        match job {
            Job::Thread {
                messages,
                own_domains,
                loader,
                read_receipts,
                options,
            } => {
                self.views.clear();
                self.current_match = None;
                let had_selection = self.selected.take().is_some();
                self.read_receipts = read_receipts;
                self.receipts_loaded.clear();
                self.renders = render_thread(&messages, &own_domains, options);
                let inline = messages
                    .iter()
                    .flat_map(|message| message.attachments.iter().cloned())
                    .collect();
                self.fetch = Some(Arc::new(fetch::Fetcher::new(
                    loader,
                    inline,
                    self.wake.clone(),
                    self.pending.clone(),
                )));
                let facts = messages
                    .iter()
                    .filter_map(|message| {
                        Some((message.id.clone(), facts(self.renders.get(&message.id)?)))
                    })
                    .collect();
                let mut outputs = vec![Output::Rendered(facts)];
                outputs.extend(had_selection.then(|| self.selection()));
                outputs
            }
            Job::Layout {
                id,
                look,
                block_remote,
            } => {
                let selected = self.selected.clone();
                let mut outputs: Vec<Output> =
                    self.layout(&id, look, block_remote).into_iter().collect();
                outputs.extend(self.find(&id));
                // A new view has no selection.
                if selected.is_some() && self.selected.is_none() {
                    outputs.push(self.selection());
                }
                outputs
            }
            Job::Paint {
                id,
                generation,
                band,
                width,
            } => {
                let Some(view) = self
                    .views
                    .get_mut(&id)
                    .filter(|view| view.generation == generation)
                else {
                    return Vec::new();
                };
                let top = band as f32 * BAND;
                let region = Region {
                    top,
                    height: BAND.min(view.height - top).max(1.0),
                    left: 0.0,
                    width,
                    phase: [view.look.phase, 0.0],
                };
                let image = view.body.paint(region, view.look.background);
                vec![Output::Painted {
                    id,
                    generation,
                    band,
                    image,
                }]
            }
            Job::Animate {
                id,
                generation,
                now,
                visible,
                max_texture_side,
            } => {
                // Always answered, because the UI asks again only once this one is.
                let Some(view) = self.views.get_mut(&id) else {
                    return vec![Output::Animated {
                        id,
                        generation,
                        patches: Vec::new(),
                        next: None,
                    }];
                };
                let body_width = if view.body.scrolls() {
                    view.body.content_width()
                } else {
                    view.body.width()
                };
                let scale = view.look.scale;
                let animated = view.body.animate(now, visible.clone());
                let patches = if view.generation == generation {
                    animated
                        .changed
                        .into_iter()
                        .filter_map(|bounds| {
                            clip_animated_region(
                                bounds,
                                &visible,
                                body_width,
                                scale,
                                max_texture_side,
                            )
                        })
                        .map(|region| {
                            let [left, top, width, height] = region;
                            (
                                region,
                                view.body.paint(
                                    Region {
                                        top,
                                        height,
                                        left,
                                        width,
                                        phase: [view.look.phase, 0.0],
                                    },
                                    view.look.background,
                                ),
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                vec![Output::Animated {
                    id,
                    generation,
                    patches,
                    next: animated.next,
                }]
            }
            Job::Hover { id, at } => {
                let hit = self
                    .views
                    .get(&id)
                    .and_then(|view| view.body.hit(at.x, at.y));
                vec![Output::Hovered { id, hit }]
            }
            Job::Click { id, at } => {
                let hit = self
                    .views
                    .get(&id)
                    .and_then(|view| view.body.hit(at.x, at.y));
                vec![Output::Clicked { id, hit }]
            }
            Job::SelectFrom {
                id,
                at,
                granularity,
            } => {
                let mut changed = self.clear_selection();
                if let Some(view) = self.views.get_mut(&id) {
                    view.body.select_from(at.x, at.y, granularity);
                    self.selected = Some(id.clone());
                    changed.extend(self.repaint(&id));
                }
                changed.push(self.selection());
                changed
            }
            Job::SelectTo { id, at } => {
                let moved = self.selected.as_deref() == Some(&id)
                    && self
                        .views
                        .get_mut(&id)
                        .is_some_and(|view| view.body.select_to(at.x, at.y));
                if moved {
                    self.repaint(&id)
                        .into_iter()
                        .chain([self.selection()])
                        .collect()
                } else {
                    Vec::new()
                }
            }
            Job::ContextMenu { id, at, screen } => {
                let Some(view) = self.views.get(&id) else {
                    return Vec::new();
                };
                let hit = view.body.context_hit(at.x, at.y);
                let on_selection = self.selected.as_deref() == Some(&id)
                    && view
                        .body
                        .selection_bounds()
                        .is_some_and(|[left, top, width, height]| {
                            Rect::from_min_size(pos2(left, top), vec2(width, height)).contains(at)
                        });
                let mut out = Vec::new();
                if !on_selection {
                    // A Mac's right-click selects the word under it
                    // (`SelectClosestWordOrLinkFromMouseEvent`).
                    out = self.clear_selection();
                    if hit.text
                        && let Some(view) = self.views.get_mut(&id)
                    {
                        view.body.select_from(at.x, at.y, Granularity::Word);
                        self.selected = Some(id.clone());
                        out.extend(self.repaint(&id));
                    }
                    out.push(self.selection());
                }
                let selection = self
                    .selected
                    .as_ref()
                    .and_then(|selected| self.views.get(selected))
                    .and_then(|view| {
                        let text = view.body.selected_text().filter(|text| !text.is_empty())?;
                        let [left, top, width, height] = view.body.selection_bounds()?;
                        Some((
                            text,
                            Rect::from_min_size(pos2(left, top), vec2(width, height)),
                        ))
                    });
                out.push(Output::ContextMenu {
                    id,
                    at,
                    screen,
                    hit,
                    selection,
                });
                out
            }
            Job::ClearSelection => self.clear_selection(),
            Job::Copy => {
                let text = self
                    .selected
                    .as_ref()
                    .and_then(|id| self.views.get(id))
                    .and_then(|view| view.body.selected_text())
                    .filter(|text| !text.is_empty());
                vec![Output::Copied(text)]
            }
            Job::Anchor { id, name } => {
                let top = self
                    .views
                    .get(&id)
                    .and_then(|view| view.body.anchor_top(&name));
                vec![Output::Anchored { id, top }]
            }
            Job::Render {
                thread,
                messages,
                own_domains,
            } => {
                let renders =
                    render_thread(&messages, &own_domains, exo_mail::RenderOptions::default());
                let facts = messages
                    .iter()
                    .filter_map(|message| {
                        Some((message.id.clone(), facts(renders.get(&message.id)?)))
                    })
                    .collect();
                vec![Output::Conversation { thread, facts }]
            }
            Job::Wake => {
                let ids: Vec<String> = self.views.keys().cloned().collect();
                let mut changed = Vec::new();
                for id in ids {
                    let Some(view) = self.views.get_mut(&id) else {
                        continue;
                    };
                    if view.body.poll() {
                        view.height = view.body.height();
                        changed.extend(self.find(&id));
                        changed.extend(self.repaint(&id));
                    }
                }
                changed
            }
            Job::Find(query) => {
                self.query = query;
                self.current_match = None;
                let ids: Vec<String> = self.views.keys().cloned().collect();
                ids.iter()
                    .flat_map(|id| self.find(id).into_iter().chain(self.repaint(id)))
                    .collect()
            }
            Job::CurrentMatch(current) => {
                let before = std::mem::replace(&mut self.current_match, current.clone());
                let mut changed = Vec::new();
                for (id, index) in [
                    before.map(|(id, _)| (id, None)),
                    current.map(|(id, index)| (id, Some(index))),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(view) = self.views.get_mut(&id) {
                        view.body.set_current_match(index);
                        changed.extend(self.repaint(&id));
                    }
                }
                changed
            }
            Job::SelectMatch { id, index } => {
                let mut changed = self.clear_selection();
                if let Some(view) = self.views.get_mut(&id) {
                    view.body.select_match(index);
                    self.selected = Some(id.clone());
                    changed.extend(self.repaint(&id));
                }
                changed.push(self.selection());
                changed
            }
        }
    }

    /// Searches message `id`'s body for find's query (or takes its highlights away), keeping the
    /// current match if it's there. `None` without a view.
    fn find(&mut self, id: &str) -> Option<Output> {
        let view = self.views.get_mut(id)?;
        let boxes = match &self.query {
            Some(query) => view.body.find(|text| exo_ui::find::matches(text, query)),
            None => {
                view.body.clear_find();
                Vec::new()
            }
        };
        if let Some((_, index)) = self
            .current_match
            .as_ref()
            .filter(|(current, _)| current == id)
        {
            view.body.set_current_match(Some(*index));
        }
        let boxes = boxes
            .into_iter()
            .map(|[left, top, width, height]| {
                Rect::from_min_size(pos2(left, top), vec2(width, height))
            })
            .collect();
        Some(Output::Found {
            id: id.to_owned(),
            query: self.query.clone(),
            boxes,
        })
    }

    /// Lays message `id` out with `look`, building its view if the look (or `block_remote`)
    /// needs a new one. Under `Show all images`, a message's first view since its thread opened
    /// loads its read receipts.
    fn layout(&mut self, id: &str, look: Look, block_remote: bool) -> Option<Output> {
        let render = self.renders.get(id)?;
        let fonts = &self.fonts;
        let rebuild = self.views.get(id).is_none_or(|view| {
            (
                view.look.theme,
                view.look.rem,
                view.look.show_quoted,
                view.look.hide_original,
                view.block_remote,
            ) != (
                look.theme,
                look.rem,
                look.show_quoted,
                look.hide_original,
                block_remote,
            )
        });
        if rebuild {
            let options = ViewOptions {
                theme: match look.theme {
                    exo_ui::Theme::Snow => exo_mail::Theme::Light,
                    exo_ui::Theme::Carbon => exo_mail::Theme::Dark,
                },
                show_quoted: look.show_quoted,
                rem: look.rem,
                block_remote_content: block_remote,
            };
            let hidden = look.hide_original.then(|| hidden(render));
            let body = BodyView::new(
                hidden.as_ref().unwrap_or(render),
                options,
                fonts,
                self.fetch.clone(),
            );
            if self.selected.as_deref() == Some(id) {
                self.selected = None;
            }
            if self.read_receipts
                && self.receipts_loaded.insert(id.to_owned())
                && let Some(fetch) = &self.fetch
            {
                exo_mail::load_read_receipts(render, fetch.as_ref());
            }
            self.views.insert(
                id.to_owned(),
                View {
                    body,
                    look,
                    block_remote,
                    height: 0.0,
                    generation: 0,
                },
            );
        }
        let generation = self.next_generation();
        let view = self.views.get_mut(id)?;
        view.height = view.body.layout(look.width, look.scale);
        view.look = look;
        view.generation = generation;
        Some(view.laid(id))
    }

    /// Marks `id`'s painted bands stale (its selection or images changed).
    fn repaint(&mut self, id: &str) -> Option<Output> {
        let generation = self.next_generation();
        let view = self.views.get_mut(id)?;
        view.generation = generation;
        Some(view.laid(id))
    }

    fn clear_selection(&mut self) -> Vec<Output> {
        let Some(id) = self.selected.take() else {
            return Vec::new();
        };
        if let Some(view) = self.views.get_mut(&id) {
            view.body.clear_selection();
        }
        self.repaint(&id)
            .into_iter()
            .chain([self.selection()])
            .collect()
    }

    /// The selection as the UI keeps it.
    fn selection(&self) -> Output {
        let selected = self.selected.as_ref().and_then(|id| {
            let body = &self.views.get(id)?.body;
            let [left, top, width, height] = body.selection_bounds()?;
            Some(Selected {
                message: id.clone(),
                html: body.selected_html()?,
                text: body.selected_text().unwrap_or_default(),
                bounds: Rect::from_min_size(pos2(left, top), vec2(width, height)),
            })
        });
        Output::Selected(selected)
    }
}

/// The render of a body that its invite card replaces. It has no new part and no `<body>`
/// attributes, and the original new part, when it has anything, becomes the quoted part behind
/// the `…`.
fn hidden(render: &Render) -> Render {
    let has_body = !render.unquoted_html.trim().is_empty();
    Render {
        unquoted_html: String::new(),
        quoted_html: if has_body {
            render.unquoted_html.clone()
        } else {
            String::new()
        },
        has_quoted_content: has_body,
        body_attributes: exo_mail::BodyAttributes::default(),
        ..render.clone()
    }
}

/// Clips an animated box `[x, y, w, h]` to `[0.0, body_width]` horizontally and the `visible`
/// viewport y-range (`>= 0.0`), clamping CSS dimensions so device pixels at `scale` never exceed
/// `max_texture_side`.
pub(super) fn clip_animated_region(
    [x, y, w, h]: [f32; 4],
    visible: &Range<f32>,
    body_width: f32,
    scale: f32,
    max_texture_side: usize,
) -> Option<[f32; 4]> {
    if !(x.is_finite()
        && y.is_finite()
        && w.is_finite()
        && h.is_finite()
        && body_width.is_finite()
        && scale.is_finite())
        || w <= 0.0
        || h <= 0.0
        || body_width <= 0.0
        || scale <= 0.0
        || max_texture_side == 0
    {
        return None;
    }
    let x0 = x.max(0.0);
    let x1 = (x + w).min(body_width);
    let y0 = y.max(visible.start.max(0.0));
    let y1 = (y + h).min(visible.end);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let max_css = max_texture_side as f32 / scale - 1.0;
    if max_css <= 0.0 {
        return None;
    }
    let [left, top] = [x0.floor(), y0.floor()];
    let width = (x1.ceil() - left).min(body_width - left).min(max_css);
    let height = (y1.ceil() - top).min(max_css);
    (width > 0.0 && height > 0.0).then_some([left, top, width, height])
}
