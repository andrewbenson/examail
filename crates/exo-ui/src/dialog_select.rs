//! A dialog's dropdown trigger and its floating [`Menu`]: paints the value and down caret, opens
//! or closes on click, closes on an outside pointer release, and handles keyboard navigation
//! (`↑`/`↓`/`J`/`K`/`Enter`/`Escape`).

use egui::{
    Color32, Context, CursorIcon, Id, Key, Modifiers, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2,
};

use crate::{
    DialogDropdown, Menu, hold_arrows, list_keys, paint_dialog_dropdown_with, palette,
    step_list_index,
};

/// Default offset from the dropdown button's top-left to its open menu (`-18px` left, `26px` down).
const DEFAULT_MENU_OFFSET: Vec2 = vec2(-18.0, 26.0);
/// Offset for an address list (`Signatures`), `23px` under the value (`25px` under the button's top).
const ADDRESS_MENU_OFFSET: Vec2 = vec2(-18.0, 25.0);

/// Which [`Menu`] layout a [`DialogSelect`] opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DialogMenuKind {
    /// Standard 32px dialog rows ([`Menu::show_in_dialog`]).
    #[default]
    Standard,
    /// Tall 36px dialog rows ([`Menu::show_tall_in_dialog`]).
    Tall,
    /// Fitted 12px-measured rows ([`Menu::show_fitted_in_dialog`]).
    Fitted,
    /// 28px contact-address rows ([`Menu::show_addresses_in_dialog`]).
    Addresses,
}

/// An interaction produced by a [`DialogSelect`] in a single frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogSelectEvent {
    /// The dropdown trigger was clicked or toggled.
    Toggle,
    /// Pointer hovered a different menu row.
    Highlight(usize),
    /// A menu row was clicked or chosen with `Enter`.
    Pick(usize),
    /// Pointer released outside the open menu and trigger, or `Escape` closed the menu.
    Close,
}

impl DialogSelectEvent {
    /// Applies `self` to an `Option<usize>` open-menu state (`Some(highlighted)` while open),
    /// opening on `initial` when toggled from closed, and returns the chosen row index if one was
    /// picked.
    pub fn apply(self, menu: &mut Option<usize>, initial: usize) -> Option<usize> {
        match self {
            Self::Toggle => {
                *menu = if menu.is_some() { None } else { Some(initial) };
                None
            }
            Self::Highlight(row) => {
                *menu = Some(row);
                None
            }
            Self::Pick(row) => {
                *menu = None;
                Some(row)
            }
            Self::Close => {
                *menu = None;
                None
            }
        }
    }
}

/// Reusable state for a dialog's dropdown menu (`None` when closed, `Some(highlighted)` when open).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialogSelectState {
    menu: Option<usize>,
}

impl DialogSelectState {
    /// Whether the dropdown's menu is currently open.
    pub const fn is_open(self) -> bool {
        self.menu.is_some()
    }

    /// The currently highlighted row when open.
    pub const fn highlighted(self) -> Option<usize> {
        self.menu
    }

    /// Opens the menu highlighting `row`.
    pub fn open_at(&mut self, row: usize) {
        self.menu = Some(row);
    }

    /// Closes the menu.
    pub fn close(&mut self) {
        self.menu = None;
    }

    /// Toggles the menu, highlighting `initial` when opening.
    pub fn toggle(&mut self, initial: usize) {
        self.menu = if self.menu.is_some() {
            None
        } else {
            Some(initial)
        };
    }

    /// Moves the highlight by `delta` across `count` items (opening on `initial` first when
    /// `open_if_closed` is true).
    pub fn step(
        &mut self,
        delta: isize,
        count: usize,
        initial: usize,
        wrap: bool,
        open_if_closed: bool,
    ) {
        if count == 0 {
            return;
        }
        match self.menu {
            Some(at) => self.menu = Some(step_list_index(at, count, delta, wrap)),
            None if open_if_closed => self.menu = Some(initial.min(count - 1)),
            None => {}
        }
    }

    /// Takes the highlighted row and closes the menu.
    pub fn confirm(&mut self) -> Option<usize> {
        self.menu.take()
    }

    /// Applies a [`DialogSelectEvent`], returning the picked row index when one was chosen.
    pub fn apply(&mut self, event: DialogSelectEvent, initial: usize) -> Option<usize> {
        event.apply(&mut self.menu, initial)
    }
}

/// A dialog's dropdown button and its open [`Menu`].
///
/// Takes `open: Option<usize>` by value and returns `Option<DialogSelectEvent>` so callers whose
/// draw functions borrow dialog state immutably can still render and emit events cleanly.
#[must_use]
pub struct DialogSelect<'a> {
    id: Id,
    value_top_left: Pos2,
    value: &'a str,
    items: &'a [&'a str],
    open: Option<usize>,
    current: usize,
    kind: DialogMenuKind,
    style: Option<DialogDropdown>,
    menu_offset: Option<Vec2>,
    focused: Option<bool>,
    clipped_underline: bool,
    wrap_keys: bool,
}

impl<'a> DialogSelect<'a> {
    /// A dialog dropdown drawing `value` at `value_top_left` and opening `items` when `open` is
    /// `Some(highlighted)`.
    pub fn new(
        id: Id,
        value_top_left: Pos2,
        value: &'a str,
        items: &'a [&'a str],
        open: Option<usize>,
    ) -> Self {
        Self {
            id,
            value_top_left,
            value,
            items,
            open,
            current: 0,
            kind: DialogMenuKind::Standard,
            style: None,
            menu_offset: None,
            focused: None,
            clipped_underline: false,
            wrap_keys: true,
        }
    }

    /// Which item `↑`/`↓` highlights first when opening a closed dropdown with the keyboard.
    pub fn current(mut self, current: usize) -> Self {
        self.current = current;
        self
    }

    /// Chooses the [`Menu`] variant (`Standard`, `Tall`, `Fitted`, `Addresses`).
    pub fn kind(mut self, kind: DialogMenuKind) -> Self {
        self.kind = kind;
        self
    }

    /// Custom value/caret colors and gap on the dropdown button.
    pub fn style(mut self, style: DialogDropdown) -> Self {
        self.style = Some(style);
        self
    }

    /// Custom offset from the button's top-left to the open menu's top-left.
    pub fn menu_offset(mut self, offset: Vec2) -> Self {
        self.menu_offset = Some(offset);
        self
    }

    /// Ties keyboard focus to an explicit dialog focus state (`Some(is_focused)`). When `None`
    /// (the default), clicking the trigger gives it egui focus so `↑`/`↓`/`J`/`K`/`Enter`/`Escape`
    /// work automatically.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = Some(focused);
        self
    }

    /// Paints the focused `0 2px #aeb1dd` underline clipped to the 19px `overflow: hidden` button
    /// box (as in Get Me To Zero, where `padding-top: 3px` pushes the underline below the clip).
    pub fn clipped_focus_underline(mut self, focused: bool) -> Self {
        self.focused = Some(focused);
        self.clipped_underline = true;
        self
    }

    /// Clamps `↑`/`↓` at the ends instead of wrapping around.
    pub fn clamp_keys(mut self) -> Self {
        self.wrap_keys = false;
        self
    }

    /// Paints the button (and open menu) and returns any [`DialogSelectEvent`] from pointer or
    /// keyboard input this frame.
    pub fn show(self, ui: &Ui) -> Option<DialogSelectEvent> {
        let ctx = ui.ctx();
        let tokens = palette(ctx);
        let style = self.style.unwrap_or(DialogDropdown {
            value: tokens.ink(0.7, 0.7),
            caret: tokens.ink(0.5, 0.5),
            gap: 10.0,
        });

        match self.focused {
            Some(true) if !ctx.memory(|memory| memory.has_focus(self.id)) => {
                ctx.memory_mut(|memory| memory.request_focus(self.id));
            }
            Some(false) if ctx.memory(|memory| memory.has_focus(self.id)) => {
                ctx.memory_mut(|memory| memory.surrender_focus(self.id));
            }
            _ => {}
        }

        let key_event = self.poll_keys(ctx);

        let button = paint_dialog_dropdown_with(ui, self.value_top_left, self.value, style);
        if self.clipped_underline && self.focused == Some(true) {
            let clip = Rect::from_min_size(
                pos2(self.value_top_left.x - 1.0, self.value_top_left.y - 4.0),
                vec2(button.width() + 1.0, 19.0),
            );
            ui.painter()
                .with_clip_rect(clip.intersect(ui.clip_rect()))
                .rect_filled(
                    Rect::from_min_size(
                        pos2(self.value_top_left.x, self.value_top_left.y + 16.0),
                        vec2(button.width(), 2.0),
                    ),
                    0.0,
                    Color32::from_rgb(0xae, 0xb1, 0xdd),
                );
        }

        let response = ui
            .interact(button, self.id, Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() && self.focused.is_none() {
            response.request_focus();
        }
        let mut event =
            key_event.or_else(|| response.clicked().then_some(DialogSelectEvent::Toggle));
        let will_be_open = match event {
            Some(DialogSelectEvent::Toggle) => self.open.is_none(),
            Some(DialogSelectEvent::Highlight(_)) => true,
            Some(DialogSelectEvent::Pick(_) | DialogSelectEvent::Close) => false,
            None => self.open.is_some(),
        };
        if ctx.memory(|memory| memory.has_focus(self.id)) {
            ctx.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: will_be_open,
                        ..egui::EventFilter::default()
                    },
                );
            });
        }

        if let Some(highlighted) = self.open.filter(|_| !self.items.is_empty()) {
            let highlighted = highlighted.min(self.items.len() - 1);
            let offset = self.menu_offset.unwrap_or(match self.kind {
                DialogMenuKind::Addresses => ADDRESS_MENU_OFFSET,
                DialogMenuKind::Standard | DialogMenuKind::Tall | DialogMenuKind::Fitted => {
                    DEFAULT_MENU_OFFSET
                }
            });
            let top_left = button.left_top() + offset;
            let list_id = self.id.with("list");
            let menu = Menu {
                items: self.items,
                highlighted,
            };
            let list = match self.kind {
                DialogMenuKind::Standard => menu.show_in_dialog(ui, list_id, top_left),
                DialogMenuKind::Tall => menu.show_tall_in_dialog(ui, list_id, top_left),
                DialogMenuKind::Fitted => menu.show_fitted_in_dialog(ui, list_id, top_left),
                DialogMenuKind::Addresses => menu.show_addresses_in_dialog(ui, list_id, top_left),
            };
            if let Some(clicked) = list.clicked {
                event = Some(DialogSelectEvent::Pick(clicked));
            } else if event.is_none()
                && let Some(hovered) = list.hovered.filter(|hovered| *hovered != highlighted)
            {
                event = Some(DialogSelectEvent::Highlight(hovered));
            } else if event.is_none() && list.outside_released(ui, response.hovered()) {
                event = Some(DialogSelectEvent::Close);
            }
        }

        event
    }

    fn poll_keys(&self, ctx: &Context) -> Option<DialogSelectEvent> {
        let count = self.items.len();
        if count == 0 || !ctx.memory(|memory| memory.has_focus(self.id)) {
            return None;
        }
        hold_arrows(ctx, self.id);
        if self.open.is_some() {
            let (j, k, esc) = ctx.input_mut(|input| {
                (
                    input.consume_key(Modifiers::NONE, Key::J),
                    input.consume_key(Modifiers::NONE, Key::K),
                    input.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if esc {
                return Some(DialogSelectEvent::Close);
            }
            if j || k {
                let delta = if j { 1 } else { -1 };
                let at = self.open.unwrap_or(self.current);
                return Some(DialogSelectEvent::Highlight(step_list_index(
                    at,
                    count,
                    delta,
                    self.wrap_keys,
                )));
            }
            if self.wrap_keys {
                let (up, down, enter) = ctx.input_mut(|input| {
                    (
                        input.consume_key(Modifiers::NONE, Key::ArrowUp),
                        input.consume_key(Modifiers::NONE, Key::ArrowDown),
                        input.consume_key(Modifiers::NONE, Key::Enter),
                    )
                });
                let at = self.open.unwrap_or(0).min(count - 1);
                if enter {
                    return Some(DialogSelectEvent::Pick(at));
                }
                if up || down {
                    let delta = if down { 1 } else { -1 };
                    return Some(DialogSelectEvent::Highlight(step_list_index(
                        at, count, delta, true,
                    )));
                }
                return None;
            }
        }
        let mut highlighted = self.open;
        if let Some(picked) = list_keys(ctx, self.id, &mut highlighted, count, self.current) {
            return Some(DialogSelectEvent::Pick(picked));
        }
        if highlighted != self.open
            && let Some(next) = highlighted
        {
            return Some(DialogSelectEvent::Highlight(next));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, RawInput};

    use super::*;

    #[test]
    fn state_steps_and_confirms_cleanly() {
        let mut state = DialogSelectState::default();
        assert!(!state.is_open());
        state.step(1, 3, 2, true, true);
        assert_eq!(state.highlighted(), Some(2), "opens on initial");
        state.step(1, 3, 2, true, true);
        assert_eq!(state.highlighted(), Some(0), "wraps around");
        assert_eq!(state.confirm(), Some(0));
        assert!(!state.is_open());
    }

    #[test]
    fn standalone_select_handles_arrows_jk_enter_and_escape() {
        let ctx = Context::default();
        crate::install(&ctx).expect("system fonts");
        let id = Id::new("test-dialog-select");
        ctx.memory_mut(|memory| memory.request_focus(id));
        let items = ["15 minutes", "5 minutes", "At start"];
        let mut state = DialogSelectState::default();

        let send = |key: Key, state: &mut DialogSelectState| -> Option<usize> {
            let mut picked = None;
            ctx.run_ui(
                RawInput {
                    events: vec![Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    if let Some(ev) = DialogSelect::new(
                        id,
                        pos2(20.0, 20.0),
                        items[1],
                        &items,
                        state.highlighted(),
                    )
                    .current(1)
                    .show(ui)
                    {
                        picked = state.apply(ev, 1);
                    }
                },
            )
            .drop_without_applying_deltas();
            picked
        };

        assert_eq!(send(Key::ArrowDown, &mut state), None);
        assert_eq!(state.highlighted(), Some(1), "ArrowDown opens on current");
        assert_eq!(send(Key::J, &mut state), None);
        assert_eq!(state.highlighted(), Some(2), "J steps down");
        assert_eq!(send(Key::J, &mut state), None);
        assert_eq!(state.highlighted(), Some(0), "J wraps to top");
        assert_eq!(send(Key::K, &mut state), None);
        assert_eq!(state.highlighted(), Some(2), "K wraps to bottom");
        assert_eq!(send(Key::Enter, &mut state), Some(2), "Enter picks row");
        assert!(!state.is_open());

        state.open_at(1);
        // Let one frame register the open menu's Escape focus lock filter, then press Escape.
        ctx.run_ui(RawInput::default(), |ui| {
            let _ = DialogSelect::new(id, pos2(20.0, 20.0), items[1], &items, state.highlighted())
                .current(1)
                .show(ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(send(Key::Escape, &mut state), None);
        assert!(!state.is_open(), "Escape closes menu");
    }

    #[test]
    fn stationary_hover_does_not_fight_keyboard_and_padding_release_keeps_menu_open() {
        let ctx = Context::default();
        crate::install(&ctx).expect("system fonts");
        let id = Id::new("test-dialog-select-hover");
        ctx.memory_mut(|memory| memory.request_focus(id));
        let items = ["15 minutes", "5 minutes", "At start"];
        let mut state = DialogSelectState::default();
        state.open_at(1);
        let value_at = pos2(60.0, 40.0);
        // Menu top-left is button.left_top() + DEFAULT_MENU_OFFSET = (60, 38) + (-18, 26) = (42, 64).
        let row0_pos = pos2(80.0, 64.0 + 11.0 + 12.0);
        let padding_pos = pos2(80.0, 64.0 + 4.0);

        let run = |events: Vec<Event>, state: &mut DialogSelectState| -> Option<usize> {
            let mut picked = None;
            ctx.run_ui(
                RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::Area::new(Id::new("dialog-select-host"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(pos2(0.0, 0.0))
                        .show(ui.ctx(), |dialog_ui| {
                            if let Some(ev) = DialogSelect::new(
                                id,
                                value_at,
                                items[1],
                                &items,
                                state.highlighted(),
                            )
                            .current(1)
                            .show(dialog_ui)
                            {
                                picked = state.apply(ev, 1);
                            }
                        });
                },
            )
            .drop_without_applying_deltas();
            picked
        };

        // Prime Area geometry past its sizing pass and establish an initial pointer position.
        let _ = run(vec![Event::PointerMoved(pos2(0.0, 0.0))], &mut state);
        let _ = run(vec![Event::PointerMoved(pos2(0.0, 0.0))], &mut state);
        ctx.memory_mut(|memory| memory.request_focus(id));
        let _ = run(vec![Event::PointerMoved(row0_pos)], &mut state);
        assert_eq!(
            state.highlighted(),
            Some(0),
            "moving pointer highlights row 0"
        );
        state.open_at(1);
        // Settle pointer velocity so the cursor rests stationary over row 0 while row 1 is highlighted.
        for _ in 0..8 {
            let _ = run(vec![], &mut state);
        }
        state.open_at(1);
        let _ = run(vec![], &mut state);
        assert_eq!(
            state.highlighted(),
            Some(1),
            "stationary pointer over row 0 must not pull highlight off row 1"
        );

        let key = |key: Key| {
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }]
        };
        assert_eq!(run(key(Key::ArrowDown), &mut state), None);
        assert_eq!(
            state.highlighted(),
            Some(2),
            "ArrowDown steps to row 2 despite stationary pointer on row 0"
        );
        assert_eq!(
            run(key(Key::Enter), &mut state),
            Some(2),
            "Enter picks row 2 despite stationary pointer on row 0"
        );
        assert!(!state.is_open());

        // Reopen and verify Escape closes even if pointer moves on the same frame.
        state.open_at(1);
        let _ = run(vec![], &mut state);
        let _ = run(
            vec![
                Event::PointerMoved(pos2(row0_pos.x + 2.0, row0_pos.y)),
                Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            &mut state,
        );
        assert!(
            !state.is_open(),
            "Escape is not swallowed by simultaneous pointer hover"
        );

        // Reopen and release pointer inside Menu's top padding (cross-layer): must stay open.
        state.open_at(1);
        let _ = run(vec![], &mut state);
        let _ = run(
            vec![
                Event::PointerMoved(padding_pos),
                Event::PointerButton {
                    pos: padding_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            &mut state,
        );
        let _ = run(
            vec![Event::PointerButton {
                pos: padding_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            &mut state,
        );
        assert!(
            state.is_open(),
            "releasing inside Menu padding must not close DialogSelect"
        );
    }
}
