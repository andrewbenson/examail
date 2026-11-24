//! The editor widget: lays the document out in Chromium's boxes with exo-ui's text, paints it,
//! and turns input into [`Editor`] commands.
//!
//! Layout follows the editor's CSS: every line is Exo Sans Email 14px in a 24px line box; lists
//! indent 40px and keep 16px above and below the outermost list (margins collapse as CSS collapses
//! them); quotes pad 1.25em with a 1px bar; indents are 40px.
//!
//! An inline image is laid out as one wide glyph, a space in its own section sized so its advance
//! is the image's width, so egui wraps lines around it as it wraps words. Its height then grows
//! its line box as Chromium's does. The image sits on the baseline, and the line is as tall as the
//! image above it plus the text's leading and descent below. A table the
//! editor keeps whole is such a glyph too, as wide as its block and as tall as Blitz lays the
//! table out ([`table`]).

mod dates;
mod drag;
mod input;
mod layout;
mod navigation;
mod paint;
mod pill;
mod resizer;
mod table;

use std::sync::Arc;

use egui::{Color32, EventFilter, Pos2, Rect, Response, Sense, Ui, vec2};
use exo_ui::{Family, Font, TextStyle, Theme};

use self::layout::{BlockLayout, Layout, layout_unit};
use crate::{
    clipboard::Clipboard,
    editor::Editor,
    image::Pictures,
    model::{Block, Pos, Selection},
    spelling::Misspelling,
    toolbar,
    trigger::Trigger,
    undo::UndoMark,
};

/// `hr { margin: 10px 0; border-top: 1px; border-bottom: 1px transparent }`.
pub(super) const RULE_HEIGHT: f32 = 22.0;

/// The size the body's text is set in, and its placeholder's opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyText {
    /// The text's style: its size and its line box.
    pub style: TextStyle,
    /// The font's content area in the line box (where the caret and a selection's text box
    /// are): its top, from the line's top, and its height.
    pub content_top: f32,
    /// See [`BodyText::content_top`].
    pub content_height: f32,
    /// The placeholder's opacity over the text colour (`::before`).
    pub placeholder_opacity: f32,
}

impl BodyText {
    /// Compose's: 14px in forced 24px lines, with the content area (the ascent and descent at
    /// 14px, 12 + 5) centred and the odd pixel of leading below (`y = line top + 3`, `height 17`),
    /// and the placeholder at `.45`.
    pub const COMPOSE: Self = Self {
        style: TextStyle::new(Font::new(Family::SansEmail, 400), 14.0, 24.0),
        content_top: 3.0,
        content_height: 17.0,
        placeholder_opacity: 0.45,
    };

    /// 12px in 20px lines, as the Out of Office Reply's message sets it, with the content area 2px
    /// down and 15px tall and the placeholder black at `.55`.
    pub const SMALL: Self = Self {
        style: TextStyle::new(Font::new(Family::SansEmail, 400), 12.0, 20.0),
        content_top: 2.0,
        content_height: 15.0,
        placeholder_opacity: 0.55,
    };

    /// A code block's (`pre`), from the email CSS's `pre { font-family: "Exo Mono Email";
    /// font-size: 14px; letter-spacing: -0.02em; line-height: 20px }`, with the content area 1px
    /// down and 17px tall.
    pub const CODE: Self = Self {
        style: TextStyle::new(Font::new(Family::MonoEmail, 400), 14.0, 20.0).letter_spacing(-0.28),
        content_top: 1.0,
        content_height: 17.0,
        placeholder_opacity: 0.45,
    };

    pub(super) fn line(&self) -> f32 {
        self.style.line_height
    }
}

impl Default for BodyText {
    fn default() -> Self {
        Self::COMPOSE
    }
}

/// How the widget looks and sizes itself, beyond the theme.
#[derive(Clone, Debug, Default)]
pub struct EditorStyle {
    /// Shown while the body is empty (`Say hello`, `Write a note`, …), or `None` for none.
    pub placeholder: Option<String>,
    /// The least height the widget takes.
    pub min_height: f32,
}

/// What happened in the editor this frame.
pub struct EditorResponse {
    /// The widget's response (focus, hover).
    pub response: Response,
    /// Whether the document changed.
    pub changed: bool,
    /// The inline trigger being typed, with its anchor on screen.
    pub trigger: Option<Trigger>,
    /// Where the caret is on screen.
    pub caret: Rect,
    /// Marks of changes ⌘Z or ⌘⇧Z undid or redid this frame ([`Editor::mark_undo`]).
    pub undone: Vec<UndoMark>,
    /// Marks of changes redone this frame.
    pub redone: Vec<UndoMark>,
    /// Escape the editor didn't need (no dropdown, ghost text or toolbar to close), for the compose
    /// form to handle.
    pub escaped: bool,
    /// ↓ or → at the very end of the body, where the compose form moves on to the quote's `…` when
    /// it shows one.
    pub past_end: bool,
    /// A toolbar button clicked this frame (for its hint).
    pub toolbar: Option<crate::ToolbarAction>,
    /// A link the link popover's URL was clicked to open.
    pub open_link: Option<String>,
    /// A right-click, for the app's menu.
    pub context_menu: Option<ContextMenu>,
    /// A word autocorrect's pill learned (`Learned "{word}"`).
    pub learned: Option<String>,
    /// The detected date the caret is on, while the editor has the keyboard. The app shows its day
    /// and moves it with ⌘⇧– and ⌘⇧= ([`Editor::shift_date`]).
    pub date: Option<crate::ActiveDate>,
    /// Whether text the Mac's drag let go on the editor went in ([`exo_ui::text_drag`]), so the
    /// editor takes the keyboard, as a click would give it.
    pub dropped: bool,
}

/// A right-click in the body and what's under it, which the editable context menu is made of.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenu {
    /// Where, on screen.
    pub at: Pos2,
    /// The `href` of the link clicked.
    pub link: Option<String>,
    /// The misspelled word clicked, with its corrections.
    pub misspelling: Option<Misspelling>,
    /// The selected text, once the click has selected the word under it.
    pub selection: String,
    /// Where the selection's first line of text starts on screen, at its baseline's left end, which
    /// Look Up's popover lines its own text up with.
    pub selection_baseline: Pos2,
    /// The selection's text size, for Look Up.
    pub text_size: f32,
    /// The image clicked, once it has loaded.
    pub image: Option<MenuImage>,
}

/// An image a right-click landed on: an inline one, or one in a kept table.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuImage {
    /// Where it's from: its URL, or `cid:` and the Content-ID of one of the draft's files.
    pub src: String,
    /// Its file.
    pub file: Arc<[u8]>,
}

/// What a key did beyond editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEffect {
    /// ⌘Z undid a change, with its marks.
    Undone(Vec<UndoMark>),
    /// ⌘⇧Z or ⌘Y redid one, with its marks.
    Redone(Vec<UndoMark>),
    /// Escape had nothing to close in the editor.
    Escaped,
    /// ↓ or → with nothing after the caret but an empty line.
    PastEnd,
}

/// What the view keeps between frames.
#[derive(Default)]
pub(crate) struct ViewState {
    pub(super) blink_reset: bool,
    pub(super) blink_start: f64,
    pub(super) layout: Option<Layout>,
    pub(super) drag: Option<input::Drag>,
    pub(super) clicks: Option<(f64, Pos2, u8)>,
    pub(super) preferred_x: Option<f32>,
    pub(crate) preedit: Option<String>,
    pub(crate) toolbar: toolbar::State,
    pub(super) clipboard: Option<Box<dyn Clipboard>>,
    pub(super) scroll_to_caret: bool,
    pub(crate) body: BodyText,
    /// The images' pixels.
    pub(crate) pictures: Pictures,
    /// The tables it keeps whole, drawn by Blitz.
    pub(crate) tables: table::Tables,
    /// The document, pictures and tables revisions the images were last asked for at.
    pub(crate) images_scanned: Option<(u64, u64, u64)>,
    /// A resize handle being dragged.
    pub(super) resize: Option<resizer::Drag>,
    /// A right-click this frame.
    pub(super) context_menu: Option<ContextMenu>,
    /// Since when the date tooltip has shown.
    pub(super) date_since: Option<f64>,
    /// A press on the selection, which may drag it.
    pub(super) text_drag: Option<drag::TextDrag>,
    /// Where text another drag has over the editor would go.
    pub(super) drag_over: Option<Pos>,
    /// The selection the pointer last made by words (a double click, and its drag), which is put
    /// in smartly when dropped.
    pub(super) word_selection: Option<Selection>,
}

impl ViewState {
    /// Lays everything out again next frame.
    pub(crate) fn forget_layout(&mut self) {
        self.layout = None;
    }

    /// The caret moved or the document changed: show the caret and restart its blink.
    pub(crate) fn caret_moved(&mut self) {
        self.blink_reset = true;
        self.scroll_to_caret = true;
    }
}

/// The theme's colours for user content.
pub(super) struct Colors {
    pub(super) text: Color32,
    /// A quote's text, in a `blockquote:not([style*="border:none"])`.
    pub(super) quote_text: Color32,
    pub(super) quote: Color32,
    pub(super) bar: Color32,
    pub(super) rule: Color32,
    pub(super) selection: Color32,
    /// The selection's tint over a selected image.
    pub(super) image_selection: Color32,
    pub(super) ghost: Color32,
}

impl Colors {
    pub(super) fn of(ctx: &egui::Context) -> Self {
        let palette = exo_ui::palette(ctx);
        match exo_ui::theme(ctx) {
            // The text is rgba(0,0,0,.9), the quote bar rgb(237,237,237), `::selection` #b4d5fe
            // and the prediction rgba(0,0,0,.45).
            Theme::Snow => Self {
                text: Color32::from_rgba_unmultiplied(0, 0, 0, 230),
                quote_text: Color32::from_rgba_unmultiplied(0, 0, 0, 153),
                quote: palette.text_base_subdued,
                bar: Color32::from_rgb(237, 237, 237),
                rule: Color32::from_rgb(0xed, 0xed, 0xed),
                selection: Color32::from_rgb(0xb4, 0xd5, 0xfe),
                image_selection: crate::image::selection_tint(Color32::from_rgb(0xb4, 0xd5, 0xfe)),
                ghost: Color32::from_rgba_unmultiplied(0, 0, 0, 115),
            },
            // The dark themes fall through to Classic carbon: hsla(0,0%,100%,.9), bar
            // rgb(95,97,101), `hr` #5f6165, selection rgba(106,134,173,.996).
            Theme::Carbon => Self {
                text: Color32::from_rgba_unmultiplied(255, 255, 255, 230),
                quote_text: Color32::from_rgba_unmultiplied(255, 255, 255, 153),
                quote: palette.text_base_subdued,
                bar: Color32::from_rgb(95, 97, 101),
                rule: Color32::from_rgb(0x5f, 0x61, 0x65),
                selection: Color32::from_rgba_unmultiplied(106, 134, 173, 254),
                // Carbon's selection over an image, `color-mix(…, transparent 50%)`, for every
                // image, because an opaque one would hide the image whole.
                image_selection: Color32::from_rgba_unmultiplied(106, 134, 173, 127),
                ghost: Color32::from_rgba_unmultiplied(255, 255, 255, 102),
            },
        }
    }
}

/// The colour a block's text is drawn in: a quote's at 60% (measured: `rgba(0, 0, 0, .6)`, as
/// in the email restyle), the reply's quoted part subdued.
pub(super) fn colors_for(block: &Block, colors: &Colors) -> Color32 {
    if block.in_quote() {
        colors.quote_text
    } else if block.path.iter().any(|container| {
        container
            .element
            .attr("class")
            .is_some_and(|class| class.split_whitespace().any(|class| class == "gmail_quote"))
    }) {
        colors.quote
    } else {
        colors.text
    }
}

impl Editor {
    /// Uses `clipboard` for copy, cut and paste instead of the system's.
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.view.clipboard = Some(clipboard);
    }

    /// Sets the body's text in `body` rather than compose's size.
    pub fn set_body_text(&mut self, body: BodyText) {
        self.view.body = body;
        self.view.layout = None;
    }

    /// Whether Escape has something to close in the editor (a trigger's dropdown, ghost text,
    /// the toolbar, the link popover). Otherwise Escape is the compose form's.
    pub fn wants_escape(&self) -> bool {
        self.trigger.is_some()
            || self.ghost.is_some()
            || self.pill_wants_escape()
            || self.view.toolbar.is_open()
            || (!self.selection.is_collapsed() && self.view.toolbar.visible)
            || self.view.toolbar.popover
            || self.view.text_drag.is_some_and(|drag| drag.dragging())
    }

    /// Whether Tab is the editor's now (ghost text to take, a suggestion chosen) rather than the
    /// form's.
    pub fn wants_tab(&self) -> bool {
        self.ghost.is_some() || self.pill_wants_tab()
    }

    /// The document's height at `width`, with its left edge at `left` on screen, for callers
    /// placing things below. It's what [`Self::ui`] will lay the document out to (the layout is
    /// kept for it).
    pub fn height(&mut self, ui: &Ui, width: f32, left: f32) -> f32 {
        let colors = Colors::of(ui.ctx());
        self.layout(ui, width, &colors, left).height
    }

    /// Draws the editor in the available width and handles its input.
    pub fn ui(&mut self, ui: &mut Ui, style: &EditorStyle) -> EditorResponse {
        let id = ui.id().with("exo_editor");
        let width = ui.available_width();
        let origin = ui.cursor().min;
        let colors = Colors::of(ui.ctx());
        let layout_height = self.layout(ui, width, &colors, origin.x).height;
        let height = layout_height.max(style.min_height);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let response = response.on_hover_cursor(egui::CursorIcon::Text);
        // A click on the toolbar or the link popover keeps the editor's focus, as a cancelled
        // `mousedown` does in a browser. Otherwise egui would surrender it to the click.
        let overlays = std::mem::take(&mut self.view.toolbar.rects);
        if !response.has_focus()
            && ui.memory(|memory| memory.had_focus_last_frame(response.id))
            && ui.input(|input| {
                input.pointer.any_click()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|point| overlays.iter().any(|overlay| overlay.contains(point)))
            })
        {
            response.request_focus();
        }
        let focused = response.has_focus();
        let revision = self.revision;
        let mut out = EditorResponse {
            response: response.clone(),
            changed: false,
            trigger: None,
            caret: Rect::NOTHING,
            undone: Vec::new(),
            redone: Vec::new(),
            escaped: false,
            past_end: false,
            toolbar: None,
            open_link: None,
            context_menu: None,
            learned: None,
            date: None,
            dropped: false,
        };
        self.pointer(ui, &response, rect, id);
        if self.drag_over(ui, rect, id) {
            response.request_focus();
            out.dropped = true;
        }
        out.context_menu = self.view.context_menu.take();
        if (response.has_focus() || focused)
            && !matches!(self.view.toolbar.mode, toolbar::Mode::Color { .. })
        {
            self.keyboard(ui, &mut out);
        } else if !(response.has_focus() || focused) {
            // A composition ends with focus, so its commit or cancel goes to whatever has focus
            // now.
            self.view.preedit = None;
        }
        // Input may have changed the document, so lay out again before painting.
        self.layout(ui, width, &colors, rect.min.x);
        if self.update_spelling(response.has_focus()) {
            ui.ctx().request_repaint();
        }
        let now = ui.input(|input| input.time);
        if std::mem::take(&mut self.view.blink_reset) {
            self.view.blink_start = now;
        }
        // Autocorrect's pause-dependent work, the caret having rested since the blink restarted.
        if let Some(wait) = self.assist(response.has_focus(), now - self.view.blink_start) {
            ui.ctx().request_repaint_after_secs(wait as f32);
        }
        // What autocorrect did is laid out before painting.
        self.layout(ui, width, &colors, rect.min.x);
        // Images on screen have textures, and show the frame that's due.
        let shown = self.images_on_screen(rect.min, ui.clip_rect());
        self.view
            .pictures
            .upload(ui.ctx(), |key| shown.contains(key));
        if let Some(wait) = self.view.pictures.animate(now, |key| shown.contains(key)) {
            ui.ctx().request_repaint_after_secs(wait as f32);
        }
        self.paint_tables(ui.ctx(), rect.min, ui.clip_rect());
        self.paint(ui, rect, &colors, style, response.has_focus(), now);
        self.paint_text_drag(ui, rect.min, colors.text);
        if response.has_focus() {
            self.resizer(ui, rect.min, id);
            // A resize changed the document, so what's laid out next frame is what the resize
            // painted.
            self.layout(ui, width, &colors, rect.min.x);
        }
        out.caret = self.caret_rect(rect.min, ui.pixels_per_point());
        // The toolbar hangs from the selected text's box (`getBoundingClientRect`).
        let mut text_boxes =
            self.text_boxes(rect.min, self.selection.start(), self.selection.end());
        text_boxes.extend(
            self.selected_images()
                .into_iter()
                .filter_map(|at| self.image_rect(rect.min, at)),
        );
        let selection = toolbar::anchor(&text_boxes);
        out.toolbar = self.show_toolbar(ui, selection, response.has_focus());
        // Without the toolbar, a caret in a link shows the link popover under the link.
        let link = if response.has_focus() && !self.view.toolbar.visible {
            self.popover_link()
        } else {
            None
        };
        self.view.toolbar.popover = link.is_some();
        if let Some((block, range, href)) = link {
            let anchor = toolbar::anchor(&self.text_boxes(
                rect.min,
                Pos::new(block, range.start),
                Pos::new(block, range.end),
            ));
            match self.link_popover(ui, anchor, rect.min.x, &href) {
                Some(toolbar::PopoverClick::Open(href)) => out.open_link = Some(href),
                Some(toolbar::PopoverClick::Remove) => {
                    out.toolbar = Some(crate::ToolbarAction::LinkRemoved)
                }
                None => {}
            }
        }
        if let Some(mut trigger) = self.trigger.clone() {
            trigger.anchor = self.char_rect(rect.min, trigger.start);
            out.trigger = Some(trigger);
        }
        // A date the caret is on has its tooltip, and hides the assistant's dropdowns,
        // autocorrect's pill among them.
        out.date = if response.has_focus() {
            self.active_date()
        } else {
            None
        };
        match &out.date {
            Some(date) => self.show_date_tooltip(ui, rect.min, date, now),
            None => self.view.date_since = None,
        }
        if out.date.is_none()
            && let Some(picked) = self.show_pill(ui, rect.min)
        {
            self.pick_suggestion(picked);
            self.layout(ui, width, &colors, rect.min.x);
        }
        out.learned = self.autocorrect.learned.take();
        if response.has_focus() {
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect,
                    cursor_rect: out.caret,
                    should_interrupt_composition: false,
                });
            });
            if std::mem::take(&mut self.view.scroll_to_caret) {
                ui.scroll_to_rect(out.caret.expand2(vec2(0.0, 4.0)), None);
            }
        }
        if response.has_focus() {
            // The keys egui leaves the editor next frame, kept by the widget's own id: Tab and the
            // arrows always (egui's focus never moves on with them, to autocorrect's pill, say),
            // and Escape while the editor has something to close with it.
            let escape = self.wants_escape();
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape,
                    },
                )
            });
        }
        out.changed = self.revision != revision;
        out
    }
}
