//! Remote and inline image loading, blocked-image placeholder sizing, and GIF/WebP animation
//! ticking.

use std::{
    collections::HashSet,
    ops::Range,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use blitz_dom::local_name;
use blitz_html::HtmlDocument;

use super::{BASE_URL, BodyView};
use crate::{animation::Player, remote};

/// What [`BodyView::animate`] changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animated {
    /// The boxes of the images that moved to a new frame (`[left, top, width, height]` in CSS px,
    /// like [`BodyView::animations`]). Paint them again.
    pub changed: Vec<[f32; 4]>,
    /// When the next frame of an image on screen is due, on the clock `animate` was given.
    pub next: Option<Duration>,
}

/// An animated image and where it shows (all in step, as in Chromium).
pub(super) struct Animation {
    url: String,
    player: Player,
    shown: Vec<Shown>,
}

/// Somewhere an image shows: an `<img>`, or one of an element's CSS background layers.
#[derive(Clone, Copy)]
enum Shown {
    Image(blitz_dom::NodeId),
    Background(blitz_dom::NodeId, usize),
}

impl Shown {
    fn node(self) -> blitz_dom::NodeId {
        match self {
            Self::Image(node) | Self::Background(node, _) => node,
        }
    }
}

impl BodyView {
    /// Starts playing the animated images that arrived, on the `<img>` elements and the CSS
    /// backgrounds showing them. Repeated URLs share one [`Player`] and refresh their targets.
    pub(super) fn adopt_animations(&mut self) {
        let arrived = match self.may_animate.as_ref().map(|pending| pending.lock()) {
            Some(Ok(mut pending)) => std::mem::take(&mut *pending),
            _ => Vec::new(),
        };
        if arrived.is_empty() && self.animations.is_empty() {
            return;
        }
        let base = url::Url::parse(BASE_URL).expect("the base URL parses");
        for animation in &mut self.animations {
            let shown = find_animation_targets(&self.document, &base, &animation.url);
            if !shown.is_empty() {
                animation.shown = shown;
            }
        }
        let mut seen = HashSet::new();
        for (url, bytes) in arrived {
            if !seen.insert(url.clone())
                || self.animations.iter().any(|animation| animation.url == url)
            {
                continue;
            }
            let shown = find_animation_targets(&self.document, &base, &url);
            if shown.is_empty() {
                continue;
            }
            if let Some(player) = Player::new(bytes) {
                self.animations.push(Animation { url, player, shown });
            }
        }
    }

    /// The boxes of the animated images (`[left, top, width, height]`, CSS px from the body's
    /// top left), as of the last layout. An animated background's is its element's border box.
    pub fn animations(&self) -> Vec<[f32; 4]> {
        self.animations
            .iter()
            .flat_map(|animation| animation.shown.iter())
            .filter_map(|shown| self.client_box(shown.node()))
            .collect()
    }

    /// Moves the animated images showing in `visible` (CSS px from the body's top) to their
    /// frames at `now` (any clock, as long as it's always the same one). The others wait until
    /// they show. Paint the [`Animated::changed`] boxes again.
    pub fn animate(&mut self, now: Duration, visible: Range<f32>) -> Animated {
        let mut animated = Animated::default();
        for at in 0..self.animations.len() {
            let boxes: Vec<[f32; 4]> = self.animations[at]
                .shown
                .iter()
                .filter_map(|shown| self.client_box(shown.node()))
                .collect();
            if !boxes
                .iter()
                .any(|[_, top, _, height]| *top < visible.end && top + height > visible.start)
            {
                continue;
            }
            let animation = &mut self.animations[at];
            if animation.player.advance(now) {
                let frame = animation.player.frame();
                let data = Arc::new(frame.as_raw().clone());
                let image = || {
                    blitz_dom::node::RasterImageData::new(
                        frame.width(),
                        frame.height(),
                        data.clone(),
                    )
                };
                for shown in &animation.shown {
                    let Some(element) = self
                        .document
                        .get_node_mut(shown.node())
                        .and_then(|node| node.element_data_mut())
                    else {
                        continue;
                    };
                    match *shown {
                        Shown::Image(_) => {
                            if let Some(raster) = element.raster_image_data_mut() {
                                *raster = image();
                            }
                        }
                        Shown::Background(_, layer) => {
                            if let Some(Some(background)) = element.background_images.get_mut(layer)
                            {
                                background.image = blitz_dom::node::ImageData::Raster(image());
                            }
                        }
                    }
                }
                animated.changed.extend(boxes);
            }
            let next = self.animations[at].player.next_due();
            animated.next = match (animated.next, next) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        animated
    }

    /// Applies resources that finished loading since the last call (see [`crate::Fetch`]) and lays
    /// out again. Returns whether anything changed, i.e. whether to paint again.
    pub fn poll(&mut self) -> bool {
        let arrived = self
            .arrived
            .as_ref()
            .map_or(0, |arrived| arrived.load(Ordering::Acquire));
        if arrived == self.applied {
            return false;
        }
        self.applied = arrived;
        self.resolve();
        self.shrink_to_fit();
        true
    }
}

/// Finds every `<img>` and CSS background layer in `document` showing `url`.
fn find_animation_targets(document: &HtmlDocument, base: &url::Url, url: &str) -> Vec<Shown> {
    let mut shown = Vec::new();
    for (id, node) in document.tree().iter() {
        let Some(element) = node.element_data() else {
            continue;
        };
        if element.name.local == local_name!("img")
            && element
                .attr(local_name!("src"))
                .and_then(|src| base.join(src).ok())
                .is_some_and(|src| src.as_str() == url)
        {
            shown.push(Shown::Image(id));
        }
        for (layer, background) in element.background_images.iter().enumerate() {
            if background
                .as_ref()
                .is_some_and(|background| background.url.as_str() == url)
            {
                shown.push(Shown::Background(id, layer));
            }
        }
    }
    shown
}

/// Marks the images that would load from the network (`data-exo-blocked`), for `body.css`'s
/// blocked-image box, and sizes each the way Chromium sizes the broken image
/// (`HTMLImageFallbackHelper::CustomStyleForAltText`). One with both dimension attributes and no
/// alt text keeps their size, and their aspect ratio, which Blitz doesn't map from the attributes.
/// One with alt text, or missing a dimension, is an inline box whose width and height are dropped,
/// so it's 16px square unless it isn't laid out inline.
pub(super) fn mark_blocked_images(document: &mut HtmlDocument) {
    let mut blocked = Vec::new();
    let mut blocked_imgs = Vec::new();
    for (id, node) in document.tree().iter() {
        let Some(element) = node.element_data() else {
            continue;
        };
        let local = &*element.name.local;
        if local == "img" {
            let attr = |name| element.attr(name).unwrap_or_default();
            if remote::is_remote(attr(local_name!("src")))
                || remote::srcset_is_remote(attr(local_name!("srcset")))
            {
                blocked.push(id);
                blocked_imgs.push(id);
            }
        } else if local.eq_ignore_ascii_case("image") || local.eq_ignore_ascii_case("feimage") {
            let remote_href = element.attrs().iter().any(|attr| {
                let is_href =
                    attr.name.local == local_name!("href") || &*attr.name.local == "xlink:href";
                is_href && remote::is_remote(&attr.value)
            });
            if remote_href {
                blocked.push(id);
            }
        }
    }
    if blocked.is_empty() {
        return;
    }
    let marker = blitz_dom::QualName::new(None, blitz_dom::ns!(), "data-exo-blocked".into());
    let mut mutator = document.mutate();
    for &id in &blocked {
        mutator.set_attribute(id, marker.clone(), "");
    }
    drop(mutator);
    if blocked_imgs.is_empty() {
        return;
    }
    document.resolve_stylist(0.0);
    let sized: Vec<_> = blocked_imgs
        .into_iter()
        .filter_map(|id| {
            let element = document.get_node(id)?.element_data()?;
            let attr = |name| element.attr(name).unwrap_or_default();
            let dimensions =
                dimension(attr(local_name!("width"))).zip(dimension(attr(local_name!("height"))));
            let alt = !attr(local_name!("alt")).is_empty();
            let inline = document.resolved_style_value(id, "display") == "inline";
            let size = match dimensions {
                Some((width, height)) if !alt || !inline => {
                    format!("aspect-ratio: {width} / {height}")
                }
                _ if inline => "width: auto !important; height: auto !important".to_owned(),
                _ => return None,
            };
            Some((id, format!("{}; {size}", attr(local_name!("style")))))
        })
        .collect();
    let mut mutator = document.mutate();
    for (id, style) in sized {
        mutator.set_attribute(id, blitz_dom::qual_name!("style"), &style);
    }
}

/// An image dimension attribute in px (the HTML rules for parsing dimension values), or `None`
/// for a percentage, zero or no number.
fn dimension(value: &str) -> Option<f32> {
    let value = value.trim_start();
    let end = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let number: f32 = value[..end].parse().ok()?;
    (number > 0.0 && !value[end..].starts_with('%')).then_some(number)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{Done, Fetch, Fonts, Message, ViewOptions, render};

    fn two_frame_gif() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder =
            gif::Encoder::new(&mut bytes, 2, 2, &[255, 0, 0, 0, 0, 255]).expect("gif header");
        encoder.set_repeat(gif::Repeat::Infinite).expect("repeat");
        for index in [0u8, 1] {
            let mut frame = gif::Frame::from_indexed_pixels(2, 2, vec![index; 4], None);
            frame.delay = 10;
            encoder.write_frame(&frame).expect("frame");
        }
        drop(encoder);
        bytes
    }

    #[derive(Default)]
    struct Deferred(Mutex<Vec<(String, Done)>>);

    impl Fetch for Deferred {
        fn fetch(&self, url: &str, done: Done) {
            self.0.lock().expect("lock").push((url.to_owned(), done));
        }
    }

    #[test]
    fn repeated_animated_urls_share_one_player_and_refresh_targets() {
        let gif_bytes = two_frame_gif();
        let url = "https://cdn.example/spin.gif";
        let message = Message {
            html: format!(
                "<img src=\"{url}\" width=\"10\" height=\"10\">\
                 <img src=\"{url}\" width=\"10\" height=\"10\">\
                 <img src=\"{url}\" width=\"10\" height=\"10\">\
                 <div style=\"width:10px;height:10px;background-image:url({url})\">x</div>"
            ),
            ..Message::default()
        };
        let rendered = render(&message, None, &[]);
        let fetch = Arc::new(Deferred::default());
        let mut view = BodyView::new(
            &rendered,
            ViewOptions::default(),
            &Fonts::load(),
            Some(fetch.clone()),
        );
        view.layout(400.0, 2.0);
        let pending: Vec<_> = std::mem::take(&mut *fetch.0.lock().expect("lock"));
        assert!(!pending.is_empty(), "at least one fetch should be issued");
        // Deliver the first response, then any additional responses on subsequent polls.
        let mut iter = pending.into_iter();
        let (_, first) = iter.next().expect("first fetch");
        first.finish(gif_bytes.clone());
        assert!(view.poll());
        assert_eq!(view.animations.len(), 1, "one Player for the URL");
        assert_eq!(view.animations().len(), 4, "3 <img> + 1 background target");

        for (_, done) in iter {
            done.finish(gif_bytes.clone());
            assert!(view.poll());
        }
        // Even if duplicate entries are pushed directly into `may_animate` in one batch,
        // `adopt_animations` deduplicates them without adding another `Player`.
        if let Some(may_animate) = &view.may_animate {
            let bytes: Arc<[u8]> = Arc::from(gif_bytes.as_slice());
            let mut queue = may_animate.lock().expect("lock");
            queue.push((url.to_owned(), bytes.clone()));
            queue.push((url.to_owned(), bytes));
        }
        view.adopt_animations();
        assert_eq!(
            view.animations.len(),
            1,
            "subsequent arrivals for the same URL must not create duplicate Players"
        );
        assert_eq!(view.animations().len(), 4);

        // Repeated `data:` URLs arriving in the same pass (img + background) also deduplicate.
        use base64::Engine as _;
        let data_url = format!(
            "data:image/gif;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&gif_bytes)
        );
        let inline = Message {
            html: format!(
                "<img src=\"{data_url}\" width=\"10\" height=\"10\">\
                 <img src=\"{data_url}\" width=\"10\" height=\"10\">\
                 <div style=\"width:10px;height:10px;background-image:url({data_url})\">x</div>"
            ),
            ..Message::default()
        };
        let mut inline_view = BodyView::new(
            &render(&inline, None, &[]),
            ViewOptions::default(),
            &Fonts::load(),
            Some(Arc::new(Deferred::default())),
        );
        inline_view.layout(400.0, 2.0);
        assert_eq!(inline_view.animations.len(), 1);
        assert_eq!(inline_view.animations().len(), 3);
        let first_tick = inline_view.animate(Duration::ZERO, 0.0..400.0);
        assert!(first_tick.next.is_some());
        let second_tick = inline_view.animate(Duration::from_millis(150), 0.0..400.0);
        assert_eq!(second_tick.changed.len(), 3);
    }
}
