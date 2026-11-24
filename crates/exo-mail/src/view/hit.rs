//! Hit-testing links, quote toggles, detected dates, inline images, anchors, and pointer text
//! selection.

use std::ops::Range;

use blitz_dom::local_name;

use super::{BodyView, Image};

/// What is under a point of the body.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// A link.
    Link {
        /// Its `href`: absolute after the pipeline, or `#id` for an anchor within the message. The
        /// pipeline only drops script URLs, so any other scheme can come through (`file:`, app URL
        /// schemes). Open only the ones you trust.
        href: String,
    },
    /// The `…` that shows or hides the quoted part.
    QuoteToggle,
    /// A detected date (`.sh-date`), which the app offers to make an event of.
    Date {
        /// What it says (`tomorrow`, `Wed Nov 25`), which [`crate::date_parts`] reads.
        text: String,
        /// Its border box, `[left, top, width, height]` in the column's CSS px.
        bounds: [f32; 4],
    },
    /// An inline attachment image (`src="cid:…"`), by content id.
    InlineImage {
        /// The attachment's `Content-ID`, without angle brackets.
        cid: String,
    },
    /// Text that isn't any of the above. Chromium's `cursor: auto` shows the I-beam over it, and
    /// the arrow where there's no hit at all.
    Text,
}

/// What a right-click lands on, for the context menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextHit {
    /// The `href` of the link around the point.
    pub link: Option<String>,
    /// The image under the point, once it has its pixels.
    pub image: Option<ContextImage>,
    /// The point is on text (not a link's or an image's).
    pub text: bool,
}

/// An image a right-click landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextImage {
    /// Its `src`.
    pub src: String,
    /// Its picture at its own size: straight (not premultiplied) RGBA8.
    pub pixels: Image,
}

/// What a mouse press selects, by how many clicks it ends (Chromium's click count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Granularity {
    /// One click: a caret, which a drag extends.
    #[default]
    Character,
    /// A double click: the word under the pointer, between word boundaries as UAX #29 defines
    /// them and ICU finds them for Chromium. A space, or a punctuation mark, is a word of its own.
    Word,
    /// A triple click: the paragraph under the pointer, the text of its block between line
    /// breaks (`<br>`, or newlines in preformatted text).
    Paragraph,
}

/// A place in the body's text: an inline root, and a byte offset into its laid-out text.
pub(super) type TextPoint = (blitz_dom::NodeId, usize);

impl BodyView {
    /// What is at `(x, y)` CSS px from the body's top-left corner: a link (anywhere in its box,
    /// padding included), the quote toggle, an inline image, other text, or nothing.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let hit = self.document.hit(x / self.zoom, y / self.zoom)?;
        let node = self.document.get_node(hit.node_id)?;
        // Parley matches a point above or below every line box to the first or last line, so a
        // "text" hit can lie in the empty part of its block (down a table cell holding an
        // `&nbsp;`, under a paragraph). That's a hit on the block, as in Chromium.
        let (node, is_text) = match node.inline_root_ancestor() {
            Some(root) if hit.is_text && !on_a_line(root, hit.y) => (root, false),
            _ => (node, hit.is_text),
        };
        let mut node = Some(node);
        while let Some(current) = node {
            if let Some(element) = current.element_data() {
                let name = &element.name.local;
                if *name == local_name!("a")
                    && let Some(href) = element.attr(local_name!("href"))
                {
                    return Some(Hit::Link {
                        href: href.to_owned(),
                    });
                }
                if *name == local_name!("img")
                    && let Some(cid) = element
                        .attr(local_name!("src"))
                        .and_then(|src| src.strip_prefix("cid:"))
                {
                    return Some(Hit::InlineImage {
                        cid: cid.to_owned(),
                    });
                }
                let has_class = |wanted: &str| {
                    element
                        .attr(local_name!("class"))
                        .is_some_and(|class| class.split_ascii_whitespace().any(|c| c == wanted))
                };
                if has_class("Exo-quoting-ellipsis") {
                    return Some(Hit::QuoteToggle);
                }
                if *name == local_name!("span")
                    && has_class("exo-date")
                    && let Some(bounds) = self.client_box(current.id)
                {
                    return Some(Hit::Date {
                        text: current.text_content(),
                        bounds,
                    });
                }
            }
            node = current
                .parent
                .and_then(|parent| self.document.get_node(parent));
        }
        is_text.then_some(Hit::Text)
    }

    /// What a right-click at `(x, y)` CSS px from the body's top-left corner lands on: the link
    /// around it, the image under it, or text.
    pub fn context_hit(&self, x: f32, y: f32) -> ContextHit {
        let mut out = ContextHit::default();
        let Some(hit) = self.document.hit(x / self.zoom, y / self.zoom) else {
            return out;
        };
        let mut node = self.document.get_node(hit.node_id);
        while let Some(current) = node {
            if let Some(element) = current.element_data() {
                let name = &element.name.local;
                if *name == local_name!("a") && out.link.is_none() {
                    out.link = element.attr(local_name!("href")).map(str::to_owned);
                }
                if *name == local_name!("img") && out.image.is_none() {
                    out.image = element.raster_image_data().map(|image| ContextImage {
                        src: element
                            .attr(local_name!("src"))
                            .unwrap_or_default()
                            .to_owned(),
                        pixels: Image {
                            width: image.width,
                            height: image.height,
                            rgba: image.data.as_ref().to_vec(),
                        },
                    });
                }
            }
            node = current
                .parent
                .and_then(|parent| self.document.get_node(parent));
        }
        out.text = hit.is_text && out.link.is_none();
        out
    }

    /// The top of what an in-message link `#name` points at, in the column's CSS px from the
    /// body's top: the first element, in document order, whose `id` or `name` is `name`
    /// (`querySelector("[id='…'], [name='…']")`). `None` if there's none.
    ///
    /// An empty inline element (the usual `<a name="…"></a>`) lays nothing out. It takes the top
    /// of the text on its block's first line instead, where Chromium puts it on its line (exactly
    /// so when it starts the block, as it does before a heading), or else the top of the nearest
    /// box around it.
    pub fn anchor_top(&self, name: &str) -> Option<f32> {
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let node = self.document.get_node(id)?;
            let named = node.element_data().is_some_and(|element| {
                [local_name!("id"), local_name!("name")]
                    .into_iter()
                    .any(|attribute| element.attr(attribute) == Some(name))
            });
            if named {
                if let Some([_, top, ..]) = self.client_box(id) {
                    return Some(top);
                }
                if let Some(top) = node
                    .inline_root_ancestor()
                    .and_then(|root| self.first_line_top(root))
                {
                    return Some(top);
                }
                return std::iter::successors(Some(node), |node| {
                    node.parent
                        .and_then(|parent| self.document.get_node(parent))
                })
                .find_map(|node| self.client_box(node.id))
                .map(|[_, top, ..]| top);
            }
            // Children in reverse, so the first comes off the stack first.
            stack.extend(node.children.iter().rev().copied());
        }
        None
    }

    /// The top of the text on inline root `root`'s first line (its content area, as Chromium
    /// places an inline box), in the column's CSS px.
    fn first_line_top(&self, root: &blitz_dom::Node) -> Option<f32> {
        let text = root.element_data()?.inline_layout_data.as_deref()?;
        let line = text
            .layout
            .lines()
            .next()?
            .metrics()
            .content_block_min_coord
            / text.layout.scale();
        let [_, top, ..] = self.client_box(root.id)?;
        let layout = root.unrounded_layout();
        Some(top + (layout.padding.top + layout.border.top + text.block_offset + line) * self.zoom)
    }

    /// Starts a text selection at `(x, y)` for a mouse press that ends `granularity`'s clicks. The
    /// selection is a caret, or the word or paragraph there. Returns whether there was text there.
    pub fn select_from(&mut self, x: f32, y: f32, granularity: Granularity) -> bool {
        let (x, y) = (x / self.zoom, y / self.zoom);
        let selected = match granularity {
            Granularity::Character => self.document.find_text_position(x, y).map(|at| at..at),
            Granularity::Word | Granularity::Paragraph => self.text_around(x, y, granularity),
        };
        self.selected_unit = selected
            .clone()
            .filter(|_| granularity != Granularity::Character)
            .map(|unit| (granularity, unit));
        match selected {
            Some(range) => {
                self.select(range.start, range.end);
                true
            }
            None => {
                self.clear_selection();
                false
            }
        }
    }

    pub(super) fn select(&mut self, anchor: TextPoint, focus: TextPoint) {
        self.document
            .set_text_selection(anchor.0, anchor.1, focus.0, focus.1);
        self.selection_ends = Some((anchor.0, focus.0));
    }

    /// The word or paragraph at `(x, y)` (the document's CSS px). As in Chromium
    /// (`SelectClosestWordFromHitTestResult`), it's the one after the caret the point puts down.
    /// So the right half of a word's last letter selects the space after it, and a point past a
    /// line's end selects its line break. A paragraph, or the break past a block's last line, runs
    /// to where the next block's text starts.
    fn text_around(&self, x: f32, y: f32, granularity: Granularity) -> Option<Range<TextPoint>> {
        let (root, at) = self.document.find_text_position(x, y)?;
        let text = self
            .document
            .get_node(root)?
            .element_data()?
            .inline_layout_data
            .as_deref()?;
        let len = text.text.len();
        let block_end = || {
            self.next_inline_root(root)
                .map_or((root, len), |next| (next, 0))
        };
        let range = match granularity {
            Granularity::Word => match parley::Cluster::from_byte_index(&text.layout, at) {
                Some(cluster) => {
                    let start = if cluster.is_word_boundary() {
                        Some(cluster)
                    } else {
                        cluster.previous_logical_word()
                    };
                    (root, start.map_or(0, |start| start.text_range().start))
                        ..(
                            root,
                            cluster
                                .next_logical_word()
                                .map_or(len, |end| end.text_range().start),
                        )
                }
                None => (root, at)..block_end(),
            },
            Granularity::Paragraph | Granularity::Character => {
                let start = text.text[..at].rfind('\n').map_or(0, |newline| newline + 1);
                let end = text.text[at..]
                    .find('\n')
                    .map(|newline| (root, at + newline + 1));
                (root, start)..end.unwrap_or_else(block_end)
            }
        };
        Some(range)
    }

    /// Extends the selection to `(x, y)` for a mouse drag. It extends to the character there or,
    /// after a double or triple click, to the far end of the word or paragraph there, keeping the
    /// one clicked (Chromium's `SelectionController`). Returns whether there was text there.
    pub fn select_to(&mut self, x: f32, y: f32) -> bool {
        let (x, y) = (x / self.zoom, y / self.zoom);
        if let Some((granularity, unit)) = self.selected_unit.clone() {
            let Some(around) = self.text_around(x, y, granularity) else {
                return false;
            };
            if self.precedes(around.start, unit.start) {
                self.select(unit.end, around.start);
            } else {
                self.select(unit.start, around.end);
            }
            return true;
        }
        if let (Some((anchor, _)), Some((focus, _))) =
            (self.selection_ends, self.document.find_text_position(x, y))
        {
            self.selection_ends = Some((anchor, focus));
        }
        self.document.extend_text_selection_to_point(x, y)
    }

    /// Whether `a` comes before `b` in the body's text.
    fn precedes(&self, a: TextPoint, b: TextPoint) -> bool {
        if a.0 == b.0 {
            return a.1 < b.1;
        }
        let document: &blitz_dom::BaseDocument = &self.document;
        let chain = |id| {
            let mut chain: Vec<_> =
                std::iter::successors(Some(id), |&id| document.get_node(id)?.parent).collect();
            chain.reverse();
            chain
        };
        let (a, b) = (chain(a.0), chain(b.0));
        let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
        match (
            a.get(common),
            b.get(common),
            common
                .checked_sub(1)
                .and_then(|parent| document.get_node(a[parent])),
        ) {
            (Some(a), Some(b), Some(parent)) => {
                let children = in_layout_order(parent);
                children.iter().position(|id| id == a) < children.iter().position(|id| id == b)
            }
            // One holds the other (an inline block in a line), so the outer one's text comes
            // first.
            _ => a.len() < b.len(),
        }
    }

    /// The first inline root after `root` in the body, if any.
    fn next_inline_root(&self, root: blitz_dom::NodeId) -> Option<blitz_dom::NodeId> {
        let document: &blitz_dom::BaseDocument = &self.document;
        fn first_in(
            document: &blitz_dom::BaseDocument,
            id: blitz_dom::NodeId,
        ) -> Option<blitz_dom::NodeId> {
            let node = document.get_node(id)?;
            if node
                .element_data()
                .is_some_and(|data| data.inline_layout_data.is_some())
            {
                return Some(id);
            }
            in_layout_order(node)
                .into_iter()
                .find_map(|child| first_in(document, child))
        }
        let mut current = root;
        while current != self.root {
            let parent = document.get_node(document.get_node(current)?.parent?)?;
            let siblings = in_layout_order(parent);
            let after = siblings.iter().skip_while(|&&id| id != current).skip(1);
            if let Some(next) = after.copied().find_map(|id| first_in(document, id)) {
                return Some(next);
            }
            current = parent.id;
        }
        None
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.document.clear_text_selection();
        self.selection_ends = None;
        self.selected_unit = None;
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        self.document.get_selected_text()
    }
}

/// `node`'s children as laid out (an anonymous block standing for the inline children it wraps).
pub(super) fn in_layout_order(node: &blitz_dom::Node) -> Vec<blitz_dom::NodeId> {
    node.layout_children
        .borrow()
        .as_ref()
        .map_or_else(|| node.children.to_vec(), |children| children.to_vec())
}

/// Whether `y` (CSS px from the top of `root`'s line boxes, as a text hit reports it) lies on one
/// of the inline root's lines.
fn on_a_line(root: &blitz_dom::Node, y: f32) -> bool {
    let Some(text) = root
        .element_data()
        .and_then(|data| data.inline_layout_data.as_deref())
    else {
        return false;
    };
    let y = y * text.layout.scale();
    text.layout.lines().any(|line| {
        let metrics = line.metrics();
        (metrics.block_min_coord..metrics.block_max_coord).contains(&y)
    })
}
