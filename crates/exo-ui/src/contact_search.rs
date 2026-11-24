//! Single-line contact-autocomplete input and keyboard/popup coordinator backed by
//! [`ContactSelectorList`].
//!
//! Captures focus and `↑`/`↓`/`Enter`/`Tab` before egui's single-line `TextEdit` processes `Enter`
//! (which otherwise drops focus before key handling runs), updates the highlighted suggestion on
//! typing or hover, and picks on `primary_pressed()` before focus leaves the field.

use egui::{Color32, Context, Id, Key, Pos2, Rect, Ui, pos2};

use crate::{ContactRow, ContactSelectorList, StackedContactStyle, TextStyle, text_field};

/// Which commit keys accept the highlighted suggestion or submit the typed text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContactAcceptKeys {
    /// Both `Enter` and `Tab` accept (Calendar event form's `Who`, Share Availability's `Who`).
    #[default]
    EnterAndTab,
    /// Only `Enter` accepts (`Meet`, where `Tab` moves focus normally).
    EnterOnly,
}

/// What happened in a [`ContactSearchInput`] on this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactSearchOutcome {
    /// Suggestion `index` was chosen by keyboard or pointer press.
    Picked(usize),
    /// The accept key was pressed with non-empty text and no suggestion list to take from.
    SubmittedTyped { by_enter: bool },
    /// The accept key was pressed while the trimmed input was empty and no suggestions were open
    /// (e.g. `Meet`'s `Enter` -> `Find Time`, or `EventForm`'s `Enter` -> next field).
    SubmittedEmpty { by_enter: bool },
}

/// Key state captured *before* [`text_field`] runs for a frame.
#[derive(Clone, Copy, Debug)]
pub struct ContactKeySnapshot {
    pub had_focus: bool,
    pub up: bool,
    pub down: bool,
    pub enter: bool,
    pub tab: bool,
}

impl ContactKeySnapshot {
    /// Captures focus on `id` and unmodified navigation/commit keys from `ui` before drawing the
    /// `TextEdit`, consuming them while `id` holds focus so `TextEdit` does not jump the caret or
    /// drop focus and modified shortcuts (`⇧Tab`, `⌘Enter`, …) pass through untouched.
    pub fn capture(ui: &Ui, id: Id) -> Self {
        let had_focus = ui.memory(|memory| memory.has_focus(id));
        if !had_focus {
            return Self {
                had_focus: false,
                up: false,
                down: false,
                enter: false,
                tab: false,
            };
        }
        let (up, down, enter, tab) = ui.input_mut(|input| {
            if !input.modifiers.is_none() {
                return (false, false, false, false);
            }
            (
                consume_unmodified_key(input, Key::ArrowUp),
                consume_unmodified_key(input, Key::ArrowDown),
                consume_unmodified_key(input, Key::Enter),
                consume_unmodified_key(input, Key::Tab),
            )
        });
        Self {
            had_focus,
            up,
            down,
            enter,
            tab,
        }
    }

    /// Evaluates navigation and commit keys against `suggestion_count` and `text`, updating
    /// `selected` in `0..suggestion_count`.
    pub fn apply(
        self,
        text: &str,
        suggestion_count: usize,
        selected: &mut usize,
        accept: ContactAcceptKeys,
        submit_when_empty: bool,
    ) -> Option<ContactSearchOutcome> {
        if !self.had_focus {
            return None;
        }
        if !submit_when_empty && text.is_empty() && suggestion_count == 0 {
            return None;
        }
        if suggestion_count > 0 {
            if self.up {
                *selected = selected.saturating_sub(1);
            } else if self.down {
                *selected = (*selected + 1).min(suggestion_count - 1);
            }
        }
        let committed = match accept {
            ContactAcceptKeys::EnterAndTab => self.enter || self.tab,
            ContactAcceptKeys::EnterOnly => self.enter,
        };
        if !committed {
            return None;
        }
        if suggestion_count > 0 {
            Some(ContactSearchOutcome::Picked(
                (*selected).min(suggestion_count - 1),
            ))
        } else if text.trim().is_empty() {
            submit_when_empty.then_some(ContactSearchOutcome::SubmittedEmpty {
                by_enter: self.enter,
            })
        } else {
            Some(ContactSearchOutcome::SubmittedTyped {
                by_enter: self.enter,
            })
        }
    }
}

/// Response from drawing a [`ContactSearchInput`]'s text field.
pub struct ContactInputResponse {
    /// The underlying `TextEdit` response.
    pub response: egui::Response,
    /// Pre-edit key snapshot, ready to evaluate once suggestions for the current text are known.
    pub keys: ContactKeySnapshot,
    /// Whether `text` changed on this frame.
    pub changed: bool,
}

/// A single-line contact search text field with placeholder, focus/blur handoff, and suggestion
/// popup plumbing.
#[must_use]
pub struct ContactSearchInput<'a> {
    id: Id,
    line: Rect,
    style: &'a TextStyle,
    color: Color32,
    selection: Color32,
    placeholder: Option<(&'a str, Color32, Pos2)>,
    request_focus: bool,
    surrender_focus: bool,
}

impl<'a> ContactSearchInput<'a> {
    /// A single-line contact search input on `line` in `style` and `color`.
    pub fn new(
        id: Id,
        line: Rect,
        style: &'a TextStyle,
        color: Color32,
        selection: Color32,
    ) -> Self {
        Self {
            id,
            line,
            style,
            color,
            selection,
            placeholder: None,
            request_focus: false,
            surrender_focus: false,
        }
    }

    /// Paints `text` in `ink` at `line.min` while the input string is empty.
    pub fn placeholder(mut self, text: &'a str, ink: Color32) -> Self {
        self.placeholder = Some((text, ink, self.line.min));
        self
    }

    /// Paints `text` in `ink` at `at` while the input string is empty.
    pub fn placeholder_at(mut self, text: &'a str, ink: Color32, at: Pos2) -> Self {
        self.placeholder = Some((text, ink, at));
        self
    }

    /// Requests keyboard focus on this frame.
    pub fn request_focus(mut self, request: bool) -> Self {
        self.request_focus = request;
        self
    }

    /// Surrenders keyboard focus after drawing on this frame.
    pub fn surrender_focus(mut self, surrender: bool) -> Self {
        self.surrender_focus = surrender;
        self
    }

    /// Paints the placeholder (when empty) and single-line [`text_field`], capturing the pre-edit
    /// [`ContactKeySnapshot`] and resetting `*selected = 0` when `text` changes.
    pub fn show_input(
        self,
        ui: &mut Ui,
        text: &mut String,
        selected: &mut usize,
    ) -> ContactInputResponse {
        self.show_input_with(ui, text, selected, text_field)
    }

    /// Like [`Self::show_input`], using a custom field painter (such as a spellchecked text field).
    pub fn show_input_with(
        self,
        ui: &mut Ui,
        text: &mut String,
        selected: &mut usize,
        paint_field: impl FnOnce(
            &mut Ui,
            Id,
            &mut String,
            Rect,
            &TextStyle,
            Color32,
            Color32,
        ) -> egui::Response,
    ) -> ContactInputResponse {
        if self.request_focus {
            ui.memory_mut(|memory| memory.request_focus(self.id));
        }
        let keys = ContactKeySnapshot::capture(ui, self.id);
        if text.is_empty()
            && let Some((placeholder, ink, at)) = self.placeholder
        {
            self.style
                .paint_line(ui.painter(), pos2(at.x, at.y), placeholder, ink, None);
        }
        let before = text.clone();
        let response = paint_field(
            ui,
            self.id,
            text,
            self.line,
            self.style,
            self.color,
            self.selection,
        );
        if self.surrender_focus {
            response.surrender_focus();
        }
        if ui.memory(|memory| memory.has_focus(self.id)) {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        tab: !text.is_empty(),
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }
        let changed = *text != before;
        if changed {
            *selected = 0;
        }
        ContactInputResponse {
            response,
            keys,
            changed,
        }
    }
}

/// Evaluates `keys` and, when the input still has focus and `rows` is non-empty, paints a stacked
/// [`ContactSelectorList`] popup, updating `selected` on hover and returning any keyboard or
/// pointer outcome.
#[expect(
    clippy::too_many_arguments,
    reason = "coordinates input focus/keys with the anchored contact popup"
)]
pub fn show_stacked_contact_popup<'a>(
    ctx: &Context,
    popup_id: Id,
    has_focus: bool,
    keys: ContactKeySnapshot,
    text: &'a str,
    rows: &'a [ContactRow<'a>],
    selected: &mut usize,
    accept: ContactAcceptKeys,
    submit_when_empty: bool,
    at: Pos2,
    width: f32,
    style: StackedContactStyle,
) -> Option<ContactSearchOutcome> {
    let mut outcome = keys.apply(text, rows.len(), selected, accept, submit_when_empty);
    if has_focus && let Some(focused) = ctx.memory(|memory| memory.focused()) {
        let lock_tab =
            accept == ContactAcceptKeys::EnterAndTab && (!rows.is_empty() || !text.is_empty());
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                focused,
                egui::EventFilter {
                    tab: lock_tab,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        });
    }
    if has_focus && !rows.is_empty() {
        let current = (*selected).min(rows.len() - 1);
        let list = ContactSelectorList::stacked(rows, text, Some(current), style).show(
            ctx,
            popup_id,
            at,
            width,
            egui::Order::Tooltip,
        );
        if let Some(hovered) = list.hovered {
            *selected = hovered;
        }
        if let Some(pressed) = list.pressed {
            outcome = Some(ContactSearchOutcome::Picked(pressed));
        }
    }
    outcome
}

/// Consumes every pressed `key` event that carries no modifiers (strictly rejecting `Shift`,
/// `Command`, `Alt`, and `Ctrl`).
fn consume_unmodified_key(input: &mut egui::InputState, key: Key) -> bool {
    let mut consumed = false;
    input.events.retain(|event| {
        if matches!(
            event,
            egui::Event::Key {
                key: pressed_key,
                pressed: true,
                modifiers,
                ..
            } if *pressed_key == key && modifiers.is_none()
        ) {
            consumed = true;
            false
        } else {
            true
        }
    });
    consumed
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, RawInput, Sense, vec2};

    use crate::{Family, Font};

    use super::*;

    #[test]
    fn key_snapshot_supports_navigation_pick_typed_and_empty_submit() {
        let mut selected = 0;
        let down = ContactKeySnapshot {
            had_focus: true,
            up: false,
            down: true,
            enter: false,
            tab: false,
        };
        assert_eq!(
            down.apply(
                "al",
                3,
                &mut selected,
                ContactAcceptKeys::EnterAndTab,
                false
            ),
            None
        );
        assert_eq!(selected, 1);

        let tab = ContactKeySnapshot {
            had_focus: true,
            up: false,
            down: false,
            enter: false,
            tab: true,
        };
        assert_eq!(
            tab.apply(
                "al",
                3,
                &mut selected,
                ContactAcceptKeys::EnterAndTab,
                false
            ),
            Some(ContactSearchOutcome::Picked(1))
        );
        assert_eq!(
            tab.apply("al", 3, &mut selected, ContactAcceptKeys::EnterOnly, true),
            None,
            "EnterOnly ignores Tab"
        );

        let enter = ContactKeySnapshot {
            had_focus: true,
            up: false,
            down: false,
            enter: true,
            tab: false,
        };
        assert_eq!(
            enter.apply("   ", 0, &mut selected, ContactAcceptKeys::EnterOnly, true),
            Some(ContactSearchOutcome::SubmittedEmpty { by_enter: true }),
            "Meet preserves Enter-when-empty -> SubmittedEmpty"
        );
        assert_eq!(
            enter.apply(
                "alice@example.com",
                0,
                &mut selected,
                ContactAcceptKeys::EnterAndTab,
                false,
            ),
            Some(ContactSearchOutcome::SubmittedTyped { by_enter: true })
        );
    }

    #[test]
    fn capture_ignores_modified_keys_and_consumes_unmodified_when_focused() {
        let ctx = Context::default();
        crate::install(&ctx).expect("system fonts");
        let id = Id::new("contact-search-capture");
        let style = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 20.0);
        let line = Rect::from_min_size(pos2(20.0, 20.0), vec2(200.0, 20.0));
        let mut text = "alice".to_owned();
        let mut selected = 0;

        ctx.memory_mut(|memory| memory.request_focus(id));

        let step = |text: &mut String,
                    selected: &mut usize,
                    key: Key,
                    modifiers: Modifiers|
         -> (ContactKeySnapshot, bool, bool) {
            let mut snap = None;
            let mut key_still_present = false;
            let mut still_focused = false;
            ctx.run_ui(
                RawInput {
                    events: vec![Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |ui| {
                    let out = ContactSearchInput::new(
                        id,
                        line,
                        &style,
                        Color32::BLACK,
                        Color32::LIGHT_BLUE,
                    )
                    .show_input(ui, text, selected);
                    snap = Some(out.keys);
                    still_focused = out.response.has_focus();
                    key_still_present = ui.input(|input| input.key_pressed(key));
                },
            )
            .drop_without_applying_deltas();
            (snap.expect("captured"), key_still_present, still_focused)
        };

        // Prime focus and filter.
        ctx.run_ui(RawInput::default(), |ui| {
            let _ = ContactSearchInput::new(id, line, &style, Color32::BLACK, Color32::LIGHT_BLUE)
                .show_input(ui, &mut text, &mut selected);
        })
        .drop_without_applying_deltas();

        // ⇧Tab and ⌘Enter are ignored and left unconsumed.
        let (shift_tab, tab_left, _) = step(&mut text, &mut selected, Key::Tab, Modifiers::SHIFT);
        assert!(shift_tab.had_focus);
        assert!(!shift_tab.tab, "Shift+Tab must not trigger Tab accept");
        assert!(tab_left, "Shift+Tab must remain unconsumed");

        let (cmd_enter, enter_left, _) =
            step(&mut text, &mut selected, Key::Enter, Modifiers::COMMAND);
        assert!(cmd_enter.had_focus);
        assert!(!cmd_enter.enter, "Cmd+Enter must not trigger Enter accept");
        assert!(enter_left, "Cmd+Enter must remain unconsumed");

        // Unmodified ArrowDown and Enter are captured AND consumed without dropping focus.
        let (down, down_left, focused_after_down) =
            step(&mut text, &mut selected, Key::ArrowDown, Modifiers::NONE);
        assert!(down.down);
        assert!(!down_left, "unmodified ArrowDown consumed");
        assert!(focused_after_down, "focus stays locked on ArrowDown");

        let (enter, enter_left, focused_after_enter) =
            step(&mut text, &mut selected, Key::Enter, Modifiers::NONE);
        assert!(enter.enter);
        assert!(!enter_left, "unmodified Enter consumed");
        assert!(
            focused_after_enter,
            "TextEdit keeps focus when Enter consumed"
        );

        // Unmodified Tab while non-empty stays on input thanks to focus_lock_filter.
        let mut sibling_focused = false;
        let mut tab_snap = None;
        ctx.run_ui(
            RawInput {
                events: vec![Event::Key {
                    key: Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                let out =
                    ContactSearchInput::new(id, line, &style, Color32::BLACK, Color32::LIGHT_BLUE)
                        .show_input(ui, &mut text, &mut selected);
                tab_snap = Some(out.keys);
                sibling_focused = ui
                    .interact(
                        Rect::from_min_size(pos2(20.0, 60.0), vec2(80.0, 20.0)),
                        Id::new("sibling-btn"),
                        Sense::click(),
                    )
                    .has_focus();
            },
        )
        .drop_without_applying_deltas();
        assert!(tab_snap.expect("snap").tab);
        assert!(
            !sibling_focused,
            "focus lock keeps Tab on non-empty ContactSearchInput"
        );
    }
}
