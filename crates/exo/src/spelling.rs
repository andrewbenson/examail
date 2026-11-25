//! What compose's body, the spellchecked fields (the subject, a snippet's name, the Out of Office
//! subject and dates, an event's title, a template's sender picker) and the other editors (About,
//! the Out of Office message) share of spelling: per-field [`exo_editor::FieldSpelling`] state,
//! `"Learned …"` hints, and the shell's editable menu for a right-click in any input or editor.
//!
//! The other editors check spelling as a body does, autocorrecting while autocorrect is on and
//! spellchecking otherwise. The one-line fields are spellchecked either way
//! ([`text_field`]).

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use exo_commands::Hint;
use exo_editor::{ContextMenu, Editor, EditorResponse, FieldSpelling, Spelling};

use crate::{
    Mail,
    desktop::context_menu,
    page_menu::{self, Choice, Editable, Target},
};

/// Bound on cached single-line [`FieldSpelling`] entries before unseen fields are pruned.
const MAX_CACHED_FIELDS: usize = 32;

static NEXT_SPELLING_KEY: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CONTEXTS: RefCell<HashMap<u64, SpellingContext>> = RefCell::new(HashMap::new());
}

/// Owns a [`Mail`] page's [`SpellingContext`] slot and drops its spellcheckers when the page drops.
pub(crate) struct SpellingHandle(u64);

impl SpellingHandle {
    pub(crate) fn new() -> Self {
        Self(NEXT_SPELLING_KEY.fetch_add(1, Ordering::Relaxed))
    }
}

impl Drop for SpellingHandle {
    fn drop(&mut self) {
        let _ = CONTEXTS.try_with(|contexts| {
            contexts.borrow_mut().remove(&self.0);
        });
    }
}

/// Which rich-text [`Editor`] reported a right-click menu.
#[derive(Clone)]
pub(crate) enum EditorTarget {
    Compose(String),
    CalendarAbout,
    OutOfOfficeMessage,
}

/// Per-page spellcheck and editable-menu state shared across widgets via [`egui::Context`].
struct SpellingContext {
    make: Rc<dyn Fn() -> Option<Box<dyn Spelling>>>,
    generation: u64,
    fields: HashMap<egui::Id, (u64, FieldSpelling)>,
    seen: HashSet<egui::Id>,
    pending_field_menu: Option<(egui::Id, ContextMenu)>,
    chosen_field: Option<(egui::Id, ContextMenu, Choice)>,
    pending_editor_menu: Option<(EditorTarget, ContextMenu)>,
    learned: Vec<String>,
}

fn key_id(ctx: &egui::Context) -> egui::Id {
    exo_ui::id(ctx, "exo::spelling_key")
}

fn active_key(ctx: &egui::Context) -> Option<u64> {
    let id = key_id(ctx);
    ctx.data(|data| data.get_temp::<u64>(id))
}

/// Shows [`exo_ui::text_field`] `id` with shared [`FieldSpelling`] and right-click menu handling.
/// Falls back to [`exo_ui::text_field`] when `ui` has no active [`Mail`] spelling context (such as
/// isolated widget tests).
pub(crate) fn text_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    line: egui::Rect,
    style: &exo_ui::TextStyle,
    color: egui::Color32,
    selection: egui::Color32,
) -> egui::Response {
    let ctx = ui.ctx().clone();
    let Some(key) = active_key(&ctx) else {
        return exo_ui::text_field(ui, id, text, line, style, color, selection);
    };
    let mut entry = CONTEXTS.with(|contexts| {
        let mut contexts = contexts.borrow_mut();
        let state = contexts.get_mut(&key)?;
        state.seen.insert(id);
        let generation = state.generation;
        let make = state.make.clone();
        let chosen = state.chosen_field.take_if(|(target, _, _)| *target == id);
        let (entry_gen, mut field) = state
            .fields
            .remove(&id)
            .unwrap_or_else(|| (generation, FieldSpelling::default()));
        if entry_gen != generation {
            field = FieldSpelling::default();
        }
        check_field(&mut field, &*make);
        if let Some((_, menu, choice)) = chosen {
            let _ = field_choice(&ctx, &mut field, text, &menu, choice);
        }
        Some((generation, field))
    });
    let Some((generation, ref mut field)) = entry else {
        return exo_ui::text_field(ui, id, text, line, style, color, selection);
    };
    let response = field.show(ui, id, text, line, style, color, selection);
    let menu = field.take_menu();
    if menu.is_some() {
        ctx.request_repaint();
    }
    CONTEXTS.with(|contexts| {
        if let Some(state) = contexts.borrow_mut().get_mut(&key)
            && let Some((_, field)) = entry.take()
        {
            state.fields.insert(id, (generation, field));
            if let Some(menu) = menu {
                state.pending_field_menu = Some((id, menu));
            }
        }
    });
    response
}

/// Records any learned word or right-click menu from `edited` for [`Mail::spelling_frame`].
pub(crate) fn note_editor(ctx: &egui::Context, target: EditorTarget, edited: &EditorResponse) {
    if edited.learned.is_none() && edited.context_menu.is_none() {
        return;
    }
    let Some(key) = active_key(ctx) else {
        return;
    };
    if edited.context_menu.is_some() {
        ctx.request_repaint();
    }
    CONTEXTS.with(|contexts| {
        if let Some(state) = contexts.borrow_mut().get_mut(&key) {
            if let Some(word) = &edited.learned {
                state.learned.push(word.clone());
            }
            if let Some(menu) = &edited.context_menu {
                state.pending_editor_menu = Some((target, menu.clone()));
            }
        }
    });
}

/// What a choice in an editable menu left to do.
pub(crate) enum Done {
    Nothing,
    /// The text changed.
    Edited,
    /// A link to open ([`Mail::open_chosen`]).
    Open(String),
}

impl Mail {
    /// The shell's editable menu for a right-click `menu` reported, and the item chosen in it.
    pub(crate) fn editable_menu(&mut self, menu: &ContextMenu) -> Option<Choice> {
        let misspelling = menu.misspelling.as_ref();
        let target = Target {
            link: menu.link.clone(),
            image: menu.image.as_ref().map(|image| image.src.clone()),
            editable: Some(Editable {
                misspelled: misspelling.map(|word| word.word.clone()),
                suggestions: misspelling
                    .map(|word| word.guesses.clone())
                    .unwrap_or_default(),
            }),
            selection: menu.selection.clone(),
        };
        let entries = page_menu::entries(&target);
        let items: Vec<context_menu::MenuItem> =
            entries.iter().map(|(item, _)| item.clone()).collect();
        (self.context_menu.borrow_mut())(&items, menu.at)
            .and_then(|index| entries.into_iter().nth(index))
            .and_then(|(_, choice)| choice)
    }

    /// Runs once per frame before anything draws: syncs spellcheckers on open editors and handles
    /// any right-click editable menu reported last frame across editors, spellchecked fields, and
    /// plain fields.
    pub(crate) fn spelling_frame(&mut self, ctx: &egui::Context) {
        let key = self.spelling_handle.0;
        let id = key_id(ctx);
        ctx.data_mut(|data| data.insert_temp(id, key));
        let make = self.spelling.clone();
        let generation = self.spelling_generation;
        let (editor_menu, field_menu) = CONTEXTS.with(|contexts| {
            let mut contexts = contexts.borrow_mut();
            let state = contexts.entry(key).or_insert_with(|| SpellingContext {
                make: make.clone(),
                generation,
                fields: HashMap::new(),
                seen: HashSet::new(),
                pending_field_menu: None,
                chosen_field: None,
                pending_editor_menu: None,
                learned: Vec::new(),
            });
            state.make = make;
            state.generation = generation;
            let seen = std::mem::take(&mut state.seen);
            if state.fields.len() > MAX_CACHED_FIELDS {
                let chosen = state.chosen_field.as_ref().map(|(id, _, _)| *id);
                state
                    .fields
                    .retain(|id, _| seen.contains(id) || Some(*id) == chosen);
            }
            (
                state.pending_editor_menu.take(),
                state.pending_field_menu.take(),
            )
        });
        self.drain_spelling_learned();
        self.compose_assist_spelling();
        self.assist_editor_spelling(Self::calendar_about_editor_mut);
        self.assist_editor_spelling(Self::out_of_office_editor_mut);

        for link_editor in [
            Self::compose_body_editor_mut,
            Self::calendar_about_editor_mut,
            Self::out_of_office_editor_mut,
        ] {
            if self.link_input_menu(ctx, link_editor) {
                return;
            }
        }
        if let Some((target, menu)) = editor_menu {
            let Some(choice) = self.editable_menu(&menu) else {
                return;
            };
            match target {
                EditorTarget::Compose(id) => self.compose_editor_choice(ctx, &id, &menu, choice),
                EditorTarget::CalendarAbout => self.apply_editor_choice(
                    ctx,
                    &menu,
                    choice,
                    Self::calendar_about_editor_mut,
                    Self::calendar_sync_about,
                ),
                EditorTarget::OutOfOfficeMessage => self.apply_editor_choice(
                    ctx,
                    &menu,
                    choice,
                    Self::out_of_office_editor_mut,
                    Self::out_of_office_sync_message,
                ),
            }
            return;
        }
        if let Some((id, menu)) = field_menu {
            let Some(choice) = self.editable_menu(&menu) else {
                return;
            };
            match choice {
                Choice::Correct(_) | Choice::AddToDictionary(_) | Choice::IgnoreSpelling(_) => {
                    CONTEXTS.with(|contexts| {
                        if let Some(state) = contexts.borrow_mut().get_mut(&key) {
                            state.chosen_field = Some((id, menu, choice));
                        }
                    });
                }
                choice => {
                    if let Done::Open(url) = shared_choice(ctx, &menu, choice) {
                        self.open_chosen(&url);
                    }
                }
            }
            return;
        }
        let Some(field) = exo_ui::take_field_menu(ctx, None) else {
            return;
        };
        let menu = ContextMenu {
            at: field.at,
            link: None,
            misspelling: None,
            selection: field.selected,
            selection_baseline: field.baseline,
            text_size: field.text_size,
            image: None,
        };
        let Some(choice) = self.editable_menu(&menu) else {
            return;
        };
        if let Done::Open(url) = shared_choice(ctx, &menu, choice) {
            self.open_chosen(&url);
        }
    }

    /// Shows `"Learned \"{word}\""` hints for any words learned from autocorrect pills this frame.
    pub(crate) fn drain_spelling_learned(&mut self) {
        let key = self.spelling_handle.0;
        let learned = CONTEXTS.with(|contexts| {
            contexts
                .borrow_mut()
                .get_mut(&key)
                .map(|state| std::mem::take(&mut state.learned))
                .unwrap_or_default()
        });
        for word in learned {
            self.commands
                .hint(Hint::new().text(format!("Learned \"{word}\"")));
        }
    }

    /// Handles a right-click last frame in the link input of the editor that `editor` finds. It
    /// shows the shell's editable menu, its spelling first, and does what's chosen to the input.
    /// Returns whether there was one.
    pub(crate) fn link_input_menu(
        &mut self,
        ctx: &egui::Context,
        editor: fn(&mut Mail) -> Option<&mut Editor>,
    ) -> bool {
        let Some(menu) = editor(self).and_then(|editor| editor.link_spelling_mut().take_menu())
        else {
            return false;
        };
        let Some(choice) = self.editable_menu(&menu) else {
            return true;
        };
        let done = editor(self)
            .and_then(Editor::link_field)
            .map_or(Done::Nothing, |(spelling, text)| {
                field_choice(ctx, spelling, text, &menu, choice)
            });
        if let Done::Open(url) = done {
            self.open_chosen(&url);
        }
        true
    }

    /// Gives the editor returned by `editor` its spellchecker (if missing, with contact names)
    /// and syncs autocorrect and autocomplete.
    fn assist_editor_spelling(&mut self, editor: fn(&mut Self) -> Option<&mut Editor>) {
        let (autocorrect, autocomplete) =
            (self.autocorrect(), self.account.settings().autocomplete());
        let needs_names = editor(self).is_some_and(|editor| !editor.has_spelling());
        let names = if needs_names {
            self.contact_names()
        } else {
            Vec::new()
        };
        let make = self.spelling.clone();
        let Some(editor) = editor(self) else {
            return;
        };
        assist(editor, &*make, autocorrect, autocomplete, || names);
    }

    /// Applies a right-click menu `choice` to the editor returned by `editor`, runs `sync` so its
    /// owner reflects any edit, and opens any chosen link.
    fn apply_editor_choice(
        &mut self,
        ctx: &egui::Context,
        menu: &ContextMenu,
        choice: Choice,
        editor: fn(&mut Self) -> Option<&mut Editor>,
        sync: fn(&mut Self),
    ) {
        let Some(done) = editor(self).map(|editor| editor_choice(ctx, editor, menu, choice)) else {
            return;
        };
        sync(self);
        if let Done::Open(url) = done {
            self.open_chosen(&url);
        }
    }

    /// Opens what a menu chose to open, a `mailto:` in a new draft and anything else in the
    /// browser.
    pub(crate) fn open_chosen(&mut self, url: &str) {
        if exo_mail::strip_ascii_prefix(url, "mailto:").is_some() {
            self.compose_mailto(url, false);
        } else {
            self.open_url(url);
        }
    }
}

/// Gives `editor` a spellchecker made by `make` if it has none, with `names` (made only then)
/// left alone by autocorrect, and turns its autocorrect and autocomplete as the account has them.
pub(crate) fn assist(
    editor: &mut Editor,
    make: &dyn Fn() -> Option<Box<dyn Spelling>>,
    autocorrect: bool,
    autocomplete: bool,
    names: impl FnOnce() -> Vec<String>,
) {
    if !editor.has_spelling()
        && let Some(engine) = make()
    {
        editor.set_spelling(engine);
        editor.set_names(names());
    }
    editor.set_autocorrect(autocorrect);
    editor.set_autocomplete(autocomplete);
    check_field(editor.link_spelling_mut(), make);
}

/// Gives `field` a spellchecker made by `make` if it has none.
pub(crate) fn check_field(field: &mut FieldSpelling, make: &dyn Fn() -> Option<Box<dyn Spelling>>) {
    if !field.has_engine()
        && let Some(engine) = make()
    {
        field.set_engine(engine);
    }
}

/// Does `choice`, from the menu of a right-click `menu` in `editor`.
pub(crate) fn editor_choice(
    ctx: &egui::Context,
    editor: &mut Editor,
    menu: &ContextMenu,
    choice: Choice,
) -> Done {
    match choice {
        Choice::Correct(correction) => {
            let Some(misspelling) = &menu.misspelling else {
                return Done::Nothing;
            };
            editor.correct(misspelling, &correction);
            return Done::Edited;
        }
        Choice::AddToDictionary(word) => editor.learn_spelling(&word),
        Choice::IgnoreSpelling(word) => editor.ignore_spelling(&word),
        choice => return shared_choice(ctx, menu, choice),
    }
    Done::Nothing
}

/// Does `choice`, from the menu of a right-click `menu` in a field editing `text`.
pub(crate) fn field_choice(
    ctx: &egui::Context,
    field: &mut FieldSpelling,
    text: &mut String,
    menu: &ContextMenu,
    choice: Choice,
) -> Done {
    match choice {
        Choice::Correct(correction) => {
            let Some(misspelling) = &menu.misspelling else {
                return Done::Nothing;
            };
            field.correct(ctx, text, misspelling, &correction);
            return Done::Edited;
        }
        Choice::AddToDictionary(word) => field.learn(&word, text),
        Choice::IgnoreSpelling(word) => field.ignore(&word, text),
        choice => return shared_choice(ctx, menu, choice),
    }
    Done::Nothing
}

/// The items an editor and a field do alike: what the keys would do for the widget with the
/// keyboard, Look Up, links to open, and an image's.
fn shared_choice(ctx: &egui::Context, menu: &ContextMenu, choice: Choice) -> Done {
    match choice {
        Choice::LookUp => {
            context_menu::look_up(&menu.selection, menu.selection_baseline, menu.text_size)
        }
        Choice::Cut => ctx.input_mut(|input| input.events.push(egui::Event::Cut)),
        Choice::Copy => ctx.input_mut(|input| input.events.push(egui::Event::Copy)),
        Choice::Paste => {
            let modifiers = egui::Modifiers::MAC_CMD.plus(egui::Modifiers::COMMAND);
            // A paste of the clipboard's text. With none, copied attachments take an empty Paste
            // event (as Edit ▸ Paste does); otherwise the ⌘V key pastes an image or file.
            let event = if let Some(text) = crate::clipboard::read_text(ctx) {
                egui::Event::Paste(text.replace("\r\n", "\n"))
            } else if crate::clipboard::read_attachments(ctx).is_some() {
                egui::Event::Paste(String::new())
            } else {
                egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }
            };
            ctx.input_mut(|input| input.events.push(event));
        }
        Choice::CopyText(text) => ctx.copy_text(text),
        Choice::CopyImage => {
            if let Some(image) = menu.image.as_ref().and_then(|image| decoded(&image.file)) {
                crate::clipboard::write_image(ctx, &image);
            }
        }
        Choice::OpenLink(url) | Choice::Search(url) => return Done::Open(url),
        // The spelling items are each widget's.
        Choice::Correct(_) | Choice::AddToDictionary(_) | Choice::IgnoreSpelling(_) => {}
    }
    Done::Nothing
}

/// An image file's picture at its own size (the first frame of an animation), as the clipboard
/// takes it.
fn decoded(file: &[u8]) -> Option<exo_mail::Image> {
    let rgba = image::load_from_memory(file).ok()?.into_rgba8();
    Some(exo_mail::Image {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn empty_menu() -> ContextMenu {
        ContextMenu {
            at: egui::Pos2::ZERO,
            link: None,
            misspelling: None,
            selection: String::new(),
            selection_baseline: egui::Pos2::ZERO,
            text_size: 13.0,
            image: None,
        }
    }

    #[test]
    fn paste_choice_emits_empty_paste_for_copied_attachments_and_cmd_v_otherwise() {
        let ctx = egui::Context::default();
        crate::clipboard::capture(&ctx);

        // Empty clipboard with no attachments -> ⌘V key event.
        shared_choice(&ctx, &empty_menu(), Choice::Paste);
        let events = ctx.input_mut(|input| std::mem::take(&mut input.events));
        assert!(matches!(
            events.as_slice(),
            [egui::Event::Key {
                key: egui::Key::V,
                pressed: true,
                ..
            }]
        ));

        // Clipboard holding copied attachments (and no text) -> Event::Paste("").
        crate::clipboard::capture_attachments(&ctx, r#"{"account":"a@x.com","files":[]}"#);
        shared_choice(&ctx, &empty_menu(), Choice::Paste);
        let events = ctx.input_mut(|input| std::mem::take(&mut input.events));
        assert_eq!(events, vec![egui::Event::Paste(String::new())]);

        // Plain text replaces copied attachments and normalizes CRLF.
        crate::clipboard::write(&ctx, "", "line 1\r\nline 2");
        shared_choice(&ctx, &empty_menu(), Choice::Paste);
        let events = ctx.input_mut(|input| std::mem::take(&mut input.events));
        assert_eq!(events, vec![egui::Event::Paste("line 1\nline 2".into())]);
    }

    #[test]
    fn open_chosen_matches_mailto_case_insensitively() {
        let ctx = egui::Context::default();
        let _ = exo_ui::install(&ctx);
        let dir = std::env::temp_dir().join(format!(
            "exo-spelling-mailto-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints =
            gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
        let account = crate::account::Account::open(
            &dir,
            crate::account::Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints,
                tokens: Arc::new(gmail::StaticToken("dev".into())),
            },
            ctx.clone(),
        )
        .expect("account opens");
        let mut mail = Mail::new(&ctx, &crate::assets_dir(), account, crate::Clock::system());
        mail.open_chosen("MAILTO:alice@example.com?subject=Hi");
        assert_eq!(
            mail.compose.focus_summary(),
            Some((vec!["alice@example.com".into()], "Hi".into()))
        );
        mail.discard();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
