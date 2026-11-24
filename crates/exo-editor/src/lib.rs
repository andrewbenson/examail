//! The compose body editor, an HTML editing model with its egui widget.
//!
//! # The model
//!
//! A [`Document`] is the body's DOM flattened into leaf blocks (`div`, `li`, …), each with the
//! containers around it (lists, quotes, indents, wrapper `div`s) and inline runs carrying the
//! elements they sit in. Editing commands on [`Editor`] work on that structure, quirks included.
//! Enter in a quote breaks out into three empty lines, ⌘] in a quote adds a level outside it, and
//! lists nest as sibling lists with `aria-level`. [`Editor::html`] is the draft body that's saved,
//! and [`outgoing::html`] is what's sent.
//!
//! # Wiring it into compose
//!
//! The app owns an [`Editor`] per draft and draws it with [`Editor::ui`]. Keys go through
//! `exo-commands` first, so the formatting shortcuts are commands the compose region registers,
//! each calling an editor method. [`FormatCommand`] lists them with their keys and help-sheet
//! titles:
//!
//! ```ignore
//! for command in exo_editor::FormatCommand::ALL {
//!     if command.applies(&editor) {
//!         commands.add(Command::shortcut(command.id(), Action::Format(command)).keys(command.keys()).regions(&[Region::ComposeForm]));
//!     }
//! }
//! // …and when the action runs:
//! Action::Format(command) => command.run(&mut editor),
//! ```
//!
//! Everything else (typing, arrows, Enter, Backspace, ⌘Z, ⌘A, clipboard, IME) the widget
//! handles itself, as a browser does inside a `contenteditable`.
//!
//! The inline triggers (`:` emoji, `;` snippets, `+`/`@`/`-` mentions) are reported, not
//! resolved. [`EditorResponse::trigger`] has the kind, the query and the anchor rect. The app
//! shows its dropdown and, while the dropdown is open, registers ↑ ↓ Enter Tab Esc so they reach
//! the dropdown instead of the editor. Picking an item calls [`Editor::replace`].
//!
//! Dates in the text are detected the same way as in message bodies. [`EditorResponse::date`] is
//! the date the caret is on, whose day the app shows, and [`Editor::shift_date`] writes what the
//! date says a day earlier or later.
//!
//! The app's spellchecker, a [`Spelling`] handed over with [`Editor::set_spelling`], checks the
//! body. With autocorrect off ([`Editor::set_autocorrect`]), misspellings are marked as Chromium
//! marks them. With autocorrect on, words are corrected or marked as they're finished, and the
//! suggestion pill is the editor's own. Autocomplete's ghost text ([`Editor::set_autocomplete`])
//! comes from the same spellchecker's completions, and Tab takes it ([`Editor::wants_tab`]).
//!
//! The app loads images. [`Editor::image_requests`] says which images the body shows without
//! pixels, and [`Editor::set_image`] hands each its [`ImageLoad`]. Insert Image calls
//! [`Editor::insert_image`] with the Content-ID the app keeps the file by. A paste's `data:`
//! images come back from [`Editor::take_uploads`] for the app to keep the same way.

mod autocorrect;
mod blocks;
mod clipboard;
mod commands;
mod dates;
mod editor;
mod field;
mod grapheme;
pub mod html;
mod image;
mod model;
pub mod outgoing;
mod spelling;
mod toolbar;
mod trigger;
mod undo;
mod url_form;
mod view;

pub use autocorrect::{GREETING_LANGUAGES, MarkedWord, Named, People, SuggestionKind, Suggestions};
pub use clipboard::{Clipboard, MemoryClipboard, SystemClipboard};
pub use commands::FormatCommand;
pub use dates::ActiveDate;
pub use editor::{Color, Editor, Fit, Format, LinkAction, Settings, Upload, unicode_replacement};
pub use field::FieldSpelling;
pub use image::{ImageLoad, ImageSource, Picture, uuid_v4};
pub use model::{
    Block, Container, ContainerId, ContainerKind, Content, Document, Element, Piece, Pos, Run,
    Selection,
};
pub use spelling::{Misspelling, Spelling};
pub use toolbar::ToolbarAction;
pub use trigger::{Trigger, TriggerKind, find as find_trigger, is_trigger_char};
pub use undo::UndoMark;
pub use view::{BodyText, ContextMenu, EditorResponse, EditorStyle, KeyEffect, MenuImage};
