//! Pointer hit-testing, clicks, selection dragging, keyboard shortcuts and clipboard events.

use egui::{Event, Id, ImeEvent, Key, Modifiers, Pos2, Rect, Response, Ui, Vec2, pos2, vec2};

use super::{ContextMenu, EditorResponse, KeyEffect, MenuImage, drag, navigation::Words};
use crate::{
    clipboard::{Clipboard, SystemClipboard},
    editor::Editor,
    html,
    image::{ImageSource, Look},
    model::{Piece, Pos, Selection},
};

/// How a drag extends the selection, by the click that started it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Character,
    Word,
    Paragraph,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Drag {
    origin: Selection,
    granularity: Granularity,
}

impl Editor {
    /// The document position under `point` (screen), for an editor at `origin`.
    pub(super) fn hit(&self, origin: Pos2, point: Pos2) -> Pos {
        let Some(layout) = &self.view.layout else {
            return Pos::default();
        };
        let local = point - origin;
        let index = layout
            .blocks
            .iter()
            .position(|view| local.y < view.top + view.height)
            .unwrap_or(layout.blocks.len() - 1);
        let view = &layout.blocks[index];
        if view.rule {
            return Pos::new(index, 0);
        }
        let x = local.x - view.left + view.phase;
        let y = (local.y - view.top).clamp(0.0, view.height - 1.0);
        // The galley's rows are a plain line apart, so find the line box `y` is in.
        let line = view.text.line();
        let row = view
            .lines
            .iter()
            .rposition(|line| y >= line.top)
            .unwrap_or(0);
        let cursor = view
            .galley
            .cursor_from_pos(vec2(x, row as f32 * line + line / 2.0));
        Pos::new(index, view.document_offset(cursor.index.0))
    }

    /// The image under `point` that can be focused, if any.
    fn image_under(&self, origin: Pos2, point: Pos2) -> Option<Pos> {
        let layout = self.view.layout.as_ref()?;
        layout.blocks.iter().enumerate().find_map(|(index, view)| {
            let local = point - origin - vec2(view.left, view.top);
            view.images
                .iter()
                .find(|placed| {
                    placed.look == Look::Picture && placed.rect.contains(local.to_pos2())
                })
                .map(|placed| Pos::new(index, placed.offset))
        })
    }

    pub(super) fn pointer(&mut self, ui: &Ui, response: &Response, rect: Rect, id: Id) {
        if response.secondary_clicked()
            && let Some(point) = response.interact_pointer_pos()
        {
            exo_ui::claim_right_click(ui.ctx());
            self.right_click(ui, rect.min, point);
            response.request_focus();
            return;
        }
        if self.view.text_drag.is_some() {
            self.drag_text(ui, rect, id);
            return;
        }
        let (pressed, pointer, shift, time) = ui.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.interact_pos(),
                input.modifiers.shift,
                input.time,
            )
        });
        if response.hovered()
            && pressed
            && let Some(point) = pointer
            && let Some(at) = self.image_under(rect.min, point)
        {
            // A click on an image focuses it.
            response.request_focus();
            self.view.drag = None;
            self.view.preferred_x = None;
            if self.focus_image(at) {
                return;
            }
        }
        if response.hovered()
            && pressed
            && let Some(point) = pointer
        {
            response.request_focus();
            let count = match self.view.clicks {
                Some((at, place, count)) if time - at < 0.5 && place.distance(point) < 4.0 => {
                    count % 3 + 1
                }
                _ => 1,
            };
            self.view.clicks = Some((time, point, count));
            let pos = self.hit(rect.min, point);
            // A press on the selection may drag it (`FrameSelection::Contains`).
            if count == 1
                && !shift
                && !self.selection.is_collapsed()
                && self.selection.start() <= pos
                && pos <= self.selection.end()
            {
                self.view.text_drag = Some(drag::TextDrag::new(point));
                self.view.drag = None;
                return;
            }
            let granularity = match count {
                2 => Granularity::Word,
                3 => Granularity::Paragraph,
                _ => Granularity::Character,
            };
            let origin = match granularity {
                Granularity::Character if shift => Selection::new(self.selection.anchor, pos),
                Granularity::Character => Selection::caret(pos),
                Granularity::Word => self.word_at(pos),
                Granularity::Paragraph => Selection::new(
                    Pos::new(pos.block, 0),
                    Pos::new(pos.block, self.doc.blocks[pos.block].len()),
                ),
            };
            self.view.drag = Some(Drag {
                origin,
                granularity,
            });
            self.view.preferred_x = None;
            self.close_toolbar();
            self.set_selection(origin);
            self.view.word_selection = (granularity == Granularity::Word).then_some(self.selection);
        }
        if let Some(drag) = self.view.drag {
            if response.dragged()
                && let Some(point) = pointer
            {
                let pos = self.hit(rect.min, point);
                let span = match drag.granularity {
                    Granularity::Character => Selection::caret(pos),
                    Granularity::Word => self.word_at(pos),
                    Granularity::Paragraph => Selection::new(
                        Pos::new(pos.block, 0),
                        Pos::new(pos.block, self.doc.blocks[pos.block].len()),
                    ),
                };
                let selection = if span.end() <= drag.origin.start() {
                    Selection::new(drag.origin.end(), span.start())
                } else {
                    Selection::new(drag.origin.start(), span.end().max(drag.origin.end()))
                };
                self.set_selection(selection);
                if drag.granularity == Granularity::Word {
                    self.view.word_selection = Some(self.selection);
                }
                ui.scroll_to_rect(
                    Rect::from_center_size(point, Vec2::splat(self.view.body.line())),
                    None,
                );
            }
            if ui.input(|input| input.pointer.primary_released()) {
                self.view.drag = None;
            }
        }
    }

    /// A right-click at `point`, for an editor at `origin`. A click outside the selection selects
    /// the word under it, or puts the caret there off a word, as a Mac's does
    /// (`SelectClosestWordOrLinkFromMouseEvent`). Then the app shows the menu.
    fn right_click(&mut self, ui: &Ui, origin: Pos2, point: Pos2) {
        let pos = self.hit(origin, point);
        // The character under the pointer is the one before the caret position when the pointer
        // is left of that position.
        let before = pos.offset > 0 && point.x < self.char_rect(origin, pos).min.x;
        let under = Pos::new(pos.block, pos.offset - usize::from(before));
        let over_selection = self.selection.start() <= under
            && under < self.selection.end()
            && under.offset < self.doc.blocks[under.block].len();
        if !over_selection {
            let words = Words::of(&self.doc.blocks[under.block]);
            let on_word = words
                .clusters
                .iter()
                .position(|(from, to)| *from <= under.offset && under.offset < *to)
                .is_some_and(|at| words.is_word(at));
            self.close_toolbar();
            self.set_selection(if on_word {
                self.word_at(under)
            } else {
                Selection::caret(pos)
            });
        }
        let image = self.image_hit(origin, point);
        let selection = self.selection;
        let misspelling = self.misspelling_at(selection.start()).filter(|word| {
            Pos::new(word.block, word.range.start) == selection.start()
                && Pos::new(word.block, word.range.end) == selection.end()
        });
        let block = &self.doc.blocks[selection.start().block];
        let link = block
            .slice(selection.start().offset..selection.start().offset + 1)
            .pop()
            .and_then(|run| run.marks.into_iter().find(|mark| mark.is("a")))
            .and_then(|link| link.attr("href").map(str::to_owned));
        let text = self
            .view
            .layout
            .as_ref()
            .and_then(|layout| layout.blocks.get(selection.start().block))
            .map_or(self.view.body, |view| view.text);
        let (line, shift) = self.line_at(origin, selection.start());
        let selection_baseline = pos2(
            line.min.x,
            line.min.y + shift + text.style.baseline(ui.ctx()),
        );
        let (image, link) = match image {
            Some((image, around)) => (Some(image), around),
            None => (None, link),
        };
        self.view.context_menu = Some(ContextMenu {
            at: point,
            link,
            misspelling,
            selection: self.selected_text(),
            selection_baseline,
            text_size: text.style.size,
            image,
        });
    }

    /// The loaded image under `point`, for an editor at `origin`, with the `href` of the link
    /// around it: an inline image, or an `<img>` in a kept table.
    fn image_hit(&self, origin: Pos2, point: Pos2) -> Option<(MenuImage, Option<String>)> {
        let layout = self.view.layout.as_ref()?;
        let (index, view, placed) =
            layout.blocks.iter().enumerate().find_map(|(index, view)| {
                let local = (point - origin - vec2(view.left, view.top)).to_pos2();
                view.images
                    .iter()
                    .find(|placed| placed.rect.contains(local))
                    .map(|placed| (index, view, placed))
            })?;
        if let Look::Table(key) = placed.look {
            let at = point - origin - vec2(view.left, view.top) - placed.rect.min.to_vec2();
            return self
                .view
                .tables
                .image_at(key, at.to_pos2(), &self.view.pictures);
        }
        let at = Pos::new(index, placed.offset);
        let src = ImageSource::of(self.image_element(at)?)?.key();
        let file = self.view.pictures.file(&src)?;
        let link = self.doc.blocks[index]
            .slice(at.offset..at.offset + 1)
            .pop()
            .and_then(|run| run.marks.into_iter().find(|mark| mark.is("a")))
            .and_then(|link| link.attr("href").map(str::to_owned));
        Some((MenuImage { src, file }, link))
    }

    pub(super) fn keyboard(&mut self, ui: &Ui, out: &mut EditorResponse) {
        let events = ui.input(|input| input.events.clone());
        for event in events {
            match event {
                Event::Text(text) if self.view.preedit.is_none() => {
                    self.view.preferred_x = None;
                    self.type_text(&text);
                }
                Event::Ime(ImeEvent::Preedit { text, .. }) => {
                    self.view.preedit = (!text.is_empty()).then_some(text);
                    self.view.caret_moved();
                }
                Event::Ime(ImeEvent::Commit(text)) => {
                    self.view.preedit = None;
                    self.type_text(&text);
                }
                Event::Ime(ImeEvent::DeleteSurrounding {
                    before_chars,
                    after_chars,
                }) => {
                    let caret = self.selection.focus;
                    let len = self.doc.blocks[caret.block].len();
                    let start = Pos::new(caret.block, caret.offset.saturating_sub(before_chars));
                    let end = Pos::new(
                        caret.block,
                        caret.offset.saturating_add(after_chars).min(len),
                    );
                    self.set_selection(Selection::new(start, end));
                    self.delete_back_to(start);
                }
                Event::Copy => self.copy(false),
                Event::Cut => self.copy(true),
                Event::Paste(text) => {
                    let plain = ui.input(|input| input.modifiers.shift);
                    let html = if plain {
                        None
                    } else {
                        self.clipboard().read_html()
                    };
                    match html {
                        Some(html) => self.paste_html(&html),
                        None => self.paste_text(&text),
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => match self.press(key, modifiers) {
                    Some(KeyEffect::Undone(marks)) => out.undone.extend(marks),
                    Some(KeyEffect::Redone(marks)) => out.redone.extend(marks),
                    Some(KeyEffect::Escaped) => out.escaped = true,
                    Some(KeyEffect::PastEnd) => out.past_end = true,
                    None => {}
                },
                _ => {}
            }
        }
    }

    fn clipboard(&mut self) -> &mut Box<dyn Clipboard> {
        self.view
            .clipboard
            .get_or_insert_with(|| Box::new(SystemClipboard::default()))
    }

    /// ⌘C / ⌘X: the selection as HTML (bare list items in their lists) and as text.
    fn copy(&mut self, cut: bool) {
        if self.selection.is_collapsed() {
            return;
        }
        let markup = self.selection_markup();
        let text = self.selected_text();
        self.clipboard().write(&markup, &text);
        if cut {
            self.cut();
        }
    }

    /// The selection as a copy writes it in HTML, with bare list items in their lists.
    pub(crate) fn selection_markup(&self) -> String {
        let blocks = self
            .doc
            .fragment(self.selection.start(), self.selection.end());
        let mut out = String::new();
        if blocks.len() == 1 && blocks[0].path.is_empty() {
            html::write_runs(blocks[0].runs(), false, &mut out);
        } else {
            html::write_blocks(&blocks, &mut out);
        }
        out
    }

    /// Handles a key press as the widget does when it has focus. A keystroke script (or a test)
    /// drives the editor with it. Text keys go through [`Editor::type_text`].
    pub fn press(&mut self, key: Key, modifiers: Modifiers) -> Option<KeyEffect> {
        // A modifier pressed on its own (egui reports Shift, ⌘ and the rest as keys) does
        // nothing, as in a browser. It mustn't make the editor forget the caret's column before
        // Shift+↓.
        if matches!(
            key,
            Key::ShiftLeft
                | Key::ShiftRight
                | Key::ControlLeft
                | Key::ControlRight
                | Key::AltLeft
                | Key::AltRight
                | Key::SuperLeft
                | Key::SuperRight
        ) {
            return None;
        }
        if modifiers.is_none() && self.pill_key(key) {
            return None;
        }
        if self.ghost.is_none() && self.image_key(key, modifiers) {
            self.view.preferred_x = None;
            return None;
        }
        let past_end = matches!(key, Key::ArrowDown | Key::ArrowRight)
            && !modifiers.shift
            && (key == Key::ArrowDown || self.selection.is_collapsed())
            && self.ghost.is_none()
            && self.trigger.is_none()
            && self.nothing_after();
        let mac = modifiers.mac_cmd || modifiers.command;
        let caret = self.selection.focus;
        let vertical = matches!(key, Key::ArrowUp | Key::ArrowDown);
        if !vertical {
            self.view.preferred_x = None;
        }
        match key {
            Key::Enter if !mac => self.enter(modifiers.shift),
            Key::Backspace if modifiers.alt && self.selection.is_collapsed() => {
                self.delete_back_to(self.word_start(caret))
            }
            Key::Backspace if mac && self.selection.is_collapsed() => {
                self.delete_back_to(self.line_start(caret))
            }
            Key::Backspace => {
                self.backspace();
                self.deleted_for_assistance();
            }
            Key::Delete if modifiers.alt && self.selection.is_collapsed() => {
                self.delete_forward_to(self.word_end(caret))
            }
            Key::Delete => {
                self.delete_forward();
                self.recheck_edited_word();
            }
            Key::A if mac => self.select_all(),
            Key::Z if mac && modifiers.shift => return Some(KeyEffect::Redone(self.redo())),
            Key::Z if mac => return Some(KeyEffect::Undone(self.undo())),
            Key::Y if mac => return Some(KeyEffect::Redone(self.redo())),
            Key::ArrowRight if self.ghost.is_some() && !modifiers.shift => {
                self.accept_ghost_text();
            }
            Key::Tab if self.ghost.is_some() => {
                self.accept_ghost_text();
            }
            Key::Escape => {
                if self.trigger.is_some() {
                    self.dismiss_trigger();
                } else if self.ghost.is_some() {
                    self.ghost = None;
                } else if self.view.toolbar.is_open()
                    || !self.selection.is_collapsed() && self.view.toolbar.visible
                {
                    self.cancel_toolbar();
                    self.set_selection(Selection::caret(self.selection.focus));
                } else if self.view.toolbar.popover {
                    self.view.toolbar.popover = false;
                    self.view.toolbar.popover_closed = Some((self.selection, self.revision));
                } else {
                    return Some(KeyEffect::Escaped);
                }
            }
            Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::Home
            | Key::End => self.move_caret(key, modifiers),
            _ => {}
        }
        past_end.then_some(KeyEffect::PastEnd)
    }

    /// Whether nothing shows after the selection's end: no text, image or atom, and at most
    /// one line break.
    fn nothing_after(&self) -> bool {
        let end = self.selection.end();
        let mut breaks = 0;
        for (index, block) in self.doc.blocks.iter().enumerate().skip(end.block) {
            if !block.is_inline() {
                return false;
            }
            let from = if index == end.block { end.offset } else { 0 };
            for run in block.slice(from..block.len()) {
                match run.piece {
                    Piece::Break => breaks += 1,
                    _ => return false,
                }
            }
            breaks += usize::from(block.trailing_break);
        }
        breaks <= 1
    }
}
