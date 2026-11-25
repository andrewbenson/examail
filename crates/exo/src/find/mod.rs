//! Find in page: the find field in the tab strip ([`bar`]), its keys, and the macOS find
//! pasteboard. What it finds is the page's text, which exo-ui searches ([`exo_ui::find`]).
//!
//! The keys work whatever has the keyboard: ⌘F opens the field with its text selected, ⌘G and
//! ⌘⇧G open it and move to the next or previous match, and, as in other Mac apps, ⌘E puts the
//! selected text in it ([`use_selection`]). While the field has the keyboard the page gets no
//! keys. Enter and ⇧Enter step through the matches, and Escape or ⌃[ closes the field.

mod bar;
mod pasteboard;

use std::sync::Arc;

use egui::{Context, Event, Id, Key, Modifiers, mutex::Mutex};

pub(crate) use bar::{layout, show};
pub use pasteboard::Pasteboard;

/// Edit ▸ Find… (⌘F) opens the find field with the keyboard and its text selected.
pub fn open(ctx: &Context) {
    command(ctx, Command::Open);
}

/// Edit ▸ Find Next (⌘G) opens the find field and moves to the next match.
pub fn next(ctx: &Context) {
    command(ctx, Command::Step(true));
}

/// Edit ▸ Find Previous (⌘⇧G) opens the find field and moves to the previous match.
pub fn previous(ctx: &Context) {
    command(ctx, Command::Step(false));
}

/// Edit ▸ Use Selection for Find (⌘E) makes the selected text the text to find, shared with other
/// apps. With the field open, Exo also searches for it.
pub fn use_selection(ctx: &Context) {
    command(ctx, Command::UseSelection);
}

/// Shares the text to find with other apps from now on, through `pasteboard`, the macOS find
/// pasteboard. What's typed goes there, and what's there when the window comes back to the front
/// becomes the text to find.
pub fn share_find_text(ctx: &Context, pasteboard: Pasteboard) {
    with(ctx, |find| find.pasteboard = Some(pasteboard));
}

/// What the find field is doing.
#[derive(Default)]
struct Find {
    /// Whether the field shows.
    open: bool,
    /// Whether the field has the keyboard.
    focused: bool,
    /// Whether to select the field's text on the next frame.
    select: bool,
    /// The text to find, which the field shows.
    query: String,
    /// Whether the text is being searched for. Typing, Enter or ⌘G start a search, and adopting
    /// the find pasteboard's text or closing the field stops it.
    searching: bool,
    /// What the menu and the keys asked for, not done yet.
    commands: Vec<Command>,
    /// The keys and text the field has this frame, taken from the page.
    events: Vec<Event>,
    /// Where else the text to find goes.
    pasteboard: Option<Pasteboard>,
    /// Whether the window was in front last frame.
    window_focused: bool,
    /// Who had the keyboard before the field took it, to give it back when the field closes.
    returns_focus_to: Option<Id>,
    /// Whether the field opened or took the keyboard again this frame.
    revealed: bool,
    /// Whether an IME preedit composition is currently active in the find field.
    composing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Open,
    Step(bool),
    UseSelection,
}

fn command(ctx: &Context, command: Command) {
    with(ctx, |find| find.commands.push(command));
    ctx.request_repaint();
}

fn with<R>(ctx: &Context, f: impl FnOnce(&mut Find) -> R) -> R {
    let id = exo_ui::id(ctx, "exo::find");
    let find = ctx.data_mut(|data| data.get_temp_mut_or_default::<Arc<Mutex<Find>>>(id).clone());
    let mut find = find.lock();
    f(&mut find)
}

/// The id of the field's text edit.
fn field_id(ctx: &egui::Context) -> Id {
    exo_ui::id(ctx, "exo::find::field")
}

/// Takes this frame's find keys, and every key while the field has the keyboard, before the
/// page sees them, and does what was asked. Call at the start of every frame, before anything
/// reads the keyboard. `available` is false on the sign-in screen, which has no find, so the field
/// closes there.
pub(crate) fn begin(ctx: &Context, available: bool) {
    let window_focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
    let regained = with(ctx, |find| {
        !std::mem::replace(&mut find.window_focused, window_focused) && window_focused
    });
    if regained {
        adopt_pasteboard(ctx);
    }
    let keys = take_shortcuts(ctx);
    with(ctx, |find| {
        find.commands.extend(keys);
        find.revealed = false;
    });
    let commands = with(ctx, |find| std::mem::take(&mut find.commands));
    for command in commands {
        match command {
            _ if !available => {}
            Command::Open => reveal(ctx),
            Command::Step(forward) => {
                // Search for the query, if any, from the current match.
                let query = with(ctx, |find| {
                    find.searching |= !find.query.is_empty();
                    find.searching.then(|| find.query.clone())
                });
                if let Some(query) = query {
                    exo_ui::find::search(ctx, Some(&query));
                    exo_ui::find::step(ctx, forward);
                }
                reveal(ctx);
            }
            Command::UseSelection => with(ctx, |find| find.commands.push(Command::UseSelection)),
        }
    }
    if !available && with(ctx, |find| find.open) {
        close(ctx);
    }
    if with(ctx, |find| find.focused) {
        take_field_keys(ctx);
    }
}

/// Whether ⌘F, ⌘G or ⌘⇧G opened the field or gave it the keyboard this frame, which hides the
/// page's link preview.
pub(crate) fn revealed(ctx: &Context) -> bool {
    with(ctx, |find| find.revealed)
}

/// Whether ⌘E asked for the selected text this frame. Hand it over with [`selected`].
pub(crate) fn wants_selection(ctx: &Context) -> bool {
    with(ctx, |find| find.commands.contains(&Command::UseSelection))
}

/// Answers ⌘E. `text`, the text selected on the page if any, becomes the text to find.
pub(crate) fn selected(ctx: &Context, text: Option<String>) {
    let open = with(ctx, |find| {
        find.commands
            .retain(|command| *command != Command::UseSelection);
        find.open
    });
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return;
    };
    set_query(ctx, text.clone(), open);
    share(ctx, &text);
}

/// Opens the field (if it isn't open) with the keyboard and its text selected. The page's link
/// preview goes.
fn reveal(ctx: &Context) {
    let field = field_id(ctx);
    let previous = ctx
        .memory(|memory| memory.focused())
        .filter(|id| *id != field);
    with(ctx, |find| {
        if !find.focused {
            find.returns_focus_to = previous;
        }
        (find.open, find.focused, find.select, find.revealed) = (true, true, true, true);
    });
}

/// Closes the field. The search stops, its highlights go, and the keyboard goes back to the page.
/// The text stays for next time.
fn close(ctx: &Context) {
    let returns = with(ctx, |find| {
        let returns = find.focused.then(|| find.returns_focus_to.take()).flatten();
        (find.open, find.focused, find.searching, find.composing) = (false, false, false, false);
        find.events.clear();
        returns
    });
    exo_ui::find::search(ctx, None);
    let field = field_id(ctx);
    ctx.memory_mut(|memory| {
        memory.surrender_focus(field);
        if let Some(id) = returns {
            memory.request_focus(id);
        }
    });
}

/// Makes `query` the text to find, and searches for it if `search`.
fn set_query(ctx: &Context, query: String, search: bool) {
    let searching = search && !query.is_empty();
    with(ctx, |find| {
        (find.query, find.searching) = (query.clone(), searching)
    });
    exo_ui::find::search(ctx, searching.then_some(query.as_str()));
}

/// Hands `query` to other apps.
fn share(ctx: &Context, query: &str) {
    let pasteboard = with(ctx, |find| find.pasteboard.clone());
    if let Some(pasteboard) = pasteboard {
        pasteboard.write(query);
    }
}

/// Called when the window comes back to the front. A text to find that another app left on the
/// find pasteboard replaces this one, without a search for it.
fn adopt_pasteboard(ctx: &Context) {
    let Some(pasteboard) = with(ctx, |find| find.pasteboard.clone()) else {
        return;
    };
    let text = pasteboard.read();
    if with(ctx, |find| find.query != text) {
        set_query(ctx, text, false);
    }
}

/// ⌘F, ⌘G, ⌘⇧G and ⌘E, taken out of this frame's input.
fn take_shortcuts(ctx: &Context) -> Vec<Command> {
    let mut commands = Vec::new();
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            let Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            let command = shortcut(*key, *modifiers);
            if *pressed {
                commands.extend(command);
            }
            command.is_none()
        });
    });
    commands
}

fn shortcut(key: Key, modifiers: Modifiers) -> Option<Command> {
    if !modifiers.command || modifiers.alt || modifiers.ctrl {
        return None;
    }
    match (key, modifiers.shift) {
        (Key::F, false) => Some(Command::Open),
        (Key::G, shift) => Some(Command::Step(!shift)),
        (Key::E, false) => Some(Command::UseSelection),
        _ => None,
    }
}

/// While the field has the keyboard, does the field's own keys here and keeps every other key it
/// types with for the field, out of the page's reach. Zooming still zooms.
fn take_field_keys(ctx: &Context) {
    let mut taken = Vec::new();
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            let keyboard = matches!(event, Event::Key { .. } | Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut | Event::Ime(_));
            let zoom = matches!(event, Event::Key { key: Key::Equals | Key::Plus | Key::Minus | Key::Num0, modifiers, .. } if modifiers.command);
            if keyboard && !zoom {
                taken.push(event.clone());
            }
            !keyboard || zoom
        });
    });
    let mut composing = with(ctx, |find| find.composing);
    let mut kept = Vec::new();
    let mut should_close = false;
    for event in taken {
        match event {
            Event::Ime(ref ime) => {
                #[expect(deprecated, reason = "egui retains ImeEvent::Disabled in Event::Ime")]
                match ime {
                    egui::ImeEvent::Preedit { text, .. } => {
                        composing = !text.is_empty() && text != "\n" && text != "\r";
                    }
                    egui::ImeEvent::Commit(_) | egui::ImeEvent::Disabled => {
                        composing = false;
                    }
                    _ => {}
                }
                kept.push(event);
            }
            Event::Key {
                key: Key::Enter,
                pressed: true,
                modifiers,
                ..
            } if !composing => {
                // Enter finds the next match, ⇧Enter the previous.
                let query = with(ctx, |find| find.query.clone());
                if !query.is_empty() {
                    set_query(ctx, query.clone(), true);
                    exo_ui::find::step(ctx, !modifiers.shift);
                    share(ctx, &query);
                }
            }
            Event::Key {
                key: Key::Escape,
                pressed: true,
                ..
            } if !composing => {
                should_close = true;
            }
            Event::Key {
                key: Key::OpenBracket,
                pressed: true,
                modifiers,
                ..
            } if !composing && modifiers.ctrl => {
                should_close = true;
            }
            // The field's own keys go no further, nor does Tab, which would move egui's focus.
            Event::Key { key: Key::Tab, .. } => {}
            Event::Key {
                key: Key::Enter | Key::Escape,
                ..
            } if !composing => {}
            event => kept.push(event),
        }
    }
    if should_close {
        close(ctx);
        return;
    }
    with(ctx, |find| {
        find.composing = composing;
        find.events.extend(kept);
    });
}

#[cfg(test)]
mod tests {
    use egui::{Color32, ImeEvent, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

    use super::*;

    #[test]
    fn keystrokes_typed_during_the_open_delay_are_buffered_and_inserted_once_open() {
        let ctx = Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");

        let run_frame = |time: f64, events: Vec<Event>| {
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    begin(ui.ctx(), true);
                    let (_, width) = layout(ui.ctx());
                    let rect = Rect::from_min_size(pos2(10.0, 4.0), vec2(width, 24.0));
                    show(ui, rect, 1.0, Color32::GRAY, |_, _, _| false);
                },
            )
            .drop_without_applying_deltas();
        };

        // Frame 0 (t = 1.000): ⌘F opens the find bar (`width` is 0 during the 25ms delay) and `'a'`
        // arrives immediately.
        run_frame(
            1.0,
            vec![
                Event::Key {
                    key: Key::F,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND,
                },
                Event::Text("a".into()),
            ],
        );
        // Frame 1 (t = 1.010, still inside the 25ms delay): `'b'` arrives.
        run_frame(1.01, vec![Event::Text("b".into())]);
        // Frame 2 (t = 1.020, still inside the 25ms delay): an idle frame with no keys must NOT
        // wipe out `'a'` and `'b'`.
        run_frame(1.02, Vec::new());

        // Frame 3 (t = 1.100, past the 25ms delay): the bar has positive width and receives `"ab"`.
        run_frame(1.1, Vec::new());
        assert_eq!(with(&ctx, |find| find.query.clone()), "ab");
    }

    #[test]
    fn ime_preedit_prevents_enter_and_escape_from_stepping_or_closing_find() {
        let ctx = Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");

        let run_frame = |time: f64, events: Vec<Event>| -> Vec<Event> {
            let mut remaining = Vec::new();
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    begin(ui.ctx(), true);
                    let (_, width) = layout(ui.ctx());
                    let rect = Rect::from_min_size(pos2(10.0, 4.0), vec2(width, 24.0));
                    show(ui, rect, 1.0, Color32::GRAY, |_, _, _| false);
                    remaining = ui.input(|input| input.events.clone());
                },
            )
            .drop_without_applying_deltas();
            remaining
        };

        open(&ctx);
        run_frame(1.0, Vec::new());
        run_frame(1.1, Vec::new());

        // Start IME preedit, then press Escape and Enter while composing: find must stay open!
        run_frame(
            1.2,
            vec![
                Event::Ime(ImeEvent::Preedit {
                    text: "にほん".into(),
                    active_range_chars: None,
                }),
                Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        assert!(with(&ctx, |find| find.open && find.composing));

        run_frame(
            1.25,
            vec![Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(
            with(&ctx, |find| find.open),
            "Escape during IME preedit cancels preedit instead of closing find"
        );

        // Commit ends IME composition, and also verify non-keyboard events survive `bar::show`'s
        // event swap intact.
        let pointer_event = Event::PointerButton {
            pos: Pos2::new(300.0, 300.0),
            button: PointerButton::Secondary,
            pressed: true,
            modifiers: Modifiers::NONE,
        };
        let remaining = run_frame(
            1.3,
            vec![
                Event::Ime(ImeEvent::Commit("日本".into())),
                pointer_event.clone(),
            ],
        );
        assert!(!with(&ctx, |find| find.composing));
        assert_eq!(with(&ctx, |find| find.query.clone()), "日本");
        assert!(
            remaining.contains(&pointer_event),
            "non-keyboard frame events are restored around text_field via mem::swap"
        );

        // Once composition has ended, Escape closes the find bar normally.
        run_frame(
            1.4,
            vec![Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(!with(&ctx, |find| find.open));
    }
}
