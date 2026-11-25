//! Sub-palette dropdown: a choice beside a sub-palette's input (`if no reply` / `regardless`), its
//! cycle shortcut, button, and open menu.

use egui::{Color32, Context, Event, Id, Key, Order, Painter, Pos2, Rect, Sense, Ui, Vec2};
use exo_ui::{Family, Font, TextStyle, paint_icon, paint_text};

use crate::{
    Hint,
    keys::{Binding, Chord},
};

/// The dropdown's button and menu items: Exo Mono 14px, `-0.0225em`. The button sits in a 20px
/// line, and the menu's items in 28px ones.
const DROPDOWN: TextStyle =
    TextStyle::new(Font::new(Family::Mono, 400), 14.0, 20.0).letter_spacing(-0.315);
const DROPDOWN_ITEM: TextStyle =
    TextStyle::new(Font::new(Family::Mono, 400), 14.0, 28.0).letter_spacing(-0.315);
/// The dropdown's arrow: `margin-left: 6px` before the 10×6 `DEPRECATED_dropdown`.
const ARROW_GAP: f32 = 6.0;
const ARROW: Vec2 = Vec2::new(10.0, 6.0);
/// A menu item: 32px tall, `padding: 0 16px`.
const MENU_ITEM: f32 = 32.0;
const MENU_PADDING: f32 = 16.0;
const HEADER_COLOR: Color32 = Color32::from_rgb(0xfa, 0xfa, 0xfa);
const INPUT_BAR: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// Runtime state of a [`Dropdown`] inside an open sub-palette.
#[derive(Clone, Debug)]
pub struct DropdownState {
    /// Selected option index.
    pub choice: usize,
    /// Highlighted option index while the dropdown menu is open.
    pub menu: Option<usize>,
    /// Timestamp when the cycle shortcut last fired, flashing the button for 50ms.
    pub flashed: f64,
    /// Option explicitly picked from the open menu this frame.
    pub picked: Option<usize>,
}

impl Default for DropdownState {
    fn default() -> Self {
        Self::new(0)
    }
}

impl DropdownState {
    /// Initial state starting on option `choice` with the menu closed.
    pub const fn new(choice: usize) -> Self {
        Self {
            choice,
            menu: None,
            flashed: f64::NEG_INFINITY,
            picked: None,
        }
    }
}

/// A choice beside a sub-palette's input, such as Send later's `if no reply` / `regardless`. The
/// rows are made for what it says. A key cycles it (`cycle`, as Send later's ⌘⇧L does), and Tab,
/// or → at the end of the input, opens its menu.
#[derive(Clone, Debug)]
#[must_use]
pub struct Dropdown {
    options: Vec<&'static str>,
    selected: usize,
    cycle: &'static str,
    /// An option, and the hint it shows when it's picked from the open menu.
    hint: Option<(usize, Hint)>,
}

impl Dropdown {
    /// A dropdown with `options` in the menu's order, starting on `selected` and cycled by `cycle`
    /// (a binding as [`crate::Command::key`] takes it).
    pub fn new(options: Vec<&'static str>, selected: usize, cycle: &'static str) -> Self {
        Self {
            options,
            selected,
            cycle,
            hint: None,
        }
    }

    /// Shows `hint` when `option` is picked from the open menu, by a click or Enter, but not
    /// when the cycle key gets to it.
    pub fn hint_on(mut self, option: usize, hint: Hint) -> Self {
        self.hint = Some((option, hint));
        self
    }

    /// Initial selected option index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn hint(&self) -> Option<&(usize, Hint)> {
        self.hint.as_ref()
    }

    /// Handles the dropdown's keys before the palette's: its cycle key and, while its menu is
    /// open, the menu's. ↑ / `K` step upward, ↓ / `J` step downward, Enter picks, and Escape, Tab
    /// and ← close it. While the menu is open, nothing reaches the input.
    pub fn keys(&self, ctx: &Context, state: &mut DropdownState) {
        let os = ctx.os();
        let cycle =
            Binding::parse(self.cycle, os).and_then(|binding| binding.chords.first().copied());
        let count = self.options.len().max(1);
        let now = ctx.input(|input| input.time);
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                if state.menu.is_some() && matches!(event, Event::Text(_)) {
                    return false;
                }
                let Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let chord = Chord::from_event(*key, *modifiers);
                if Some(chord) == cycle {
                    // `cycle`: the next option, the menu's highlight with it, and a flash.
                    state.choice = (state.choice + 1) % count;
                    state.menu = state.menu.map(|_| state.choice);
                    state.flashed = now;
                    return false;
                }
                let Some(highlight) = state.menu else {
                    return true;
                };
                let plain = !chord.shift && !chord.ctrl && !chord.alt && !chord.meta;
                match chord.key {
                    Key::ArrowDown | Key::J if plain => {
                        state.menu = Some((highlight + 1) % count);
                    }
                    Key::ArrowUp | Key::K if plain => {
                        state.menu = Some((highlight + count - 1) % count);
                    }
                    Key::Enter if plain => {
                        state.choice = highlight;
                        state.menu = None;
                        state.picked = Some(highlight);
                    }
                    Key::Tab | Key::ArrowLeft => state.menu = None,
                    _ if chord.is_escape(os) => state.menu = None,
                    _ => {}
                }
                false
            });
        });
    }

    /// The dropdown's width: its button's (the option chosen, then the arrow), or with its menu
    /// open, the widest item's if wider.
    pub(crate) fn width(&self, ui: &Ui, state: &DropdownState) -> f32 {
        let ctx = ui.ctx();
        // Chromium's text box takes the last letter's (negative) spacing too.
        let text = |label: &str, style: &TextStyle| style.width(ctx, label) + style.letter_spacing;
        let button = text(
            self.options.get(state.choice).copied().unwrap_or_default(),
            &DROPDOWN,
        ) + ARROW_GAP
            + ARROW.x;
        let menu = if state.menu.is_some() {
            self.options
                .iter()
                .map(|option| text(option, &DROPDOWN_ITEM) + 2.0 * MENU_PADDING)
                .fold(0.0_f32, f32::max)
        } else {
            0.0
        };
        button.max(menu)
    }

    /// Paints the dropdown's button in `rect`.
    pub(crate) fn paint_button(
        &self,
        ui: &Ui,
        painter: &Painter,
        state: &mut DropdownState,
        rect: Rect,
        enabled: bool,
    ) {
        let ctx = ui.ctx();
        let id = exo_ui::id(ctx, "exo_commands::palette_dropdown");
        let label = self.options.get(state.choice).copied().unwrap_or_default();
        let temp_galley = DROPDOWN.single_line(painter, label, Color32::WHITE, None);
        let text_box = temp_galley.size().x + DROPDOWN.letter_spacing;
        let width = text_box + ARROW_GAP + ARROW.x;
        let line = Rect::from_min_size(
            Pos2::new(rect.right() - width, rect.top() + 12.0),
            Vec2::new(width, DROPDOWN.line_height),
        );
        let response = ui.interact(
            line,
            id,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if enabled && response.clicked() {
            state.menu = match state.menu {
                Some(_) => None,
                None => Some(state.choice),
            };
        }
        let open = state.menu.is_some();
        let flashing = ctx.input(|input| input.time) - state.flashed < 0.05;
        // `opacity: .5`, or 1 on hover, open or flashed. `transition: opacity 150ms ease`, with
        // none while flashing.
        let lit = enabled && (response.hovered() || open);
        let eased = exo_ui::motion::transition(
            ctx,
            id.with("opacity"),
            lit || flashing,
            0.15,
            exo_ui::motion::CubicBezier::EASE,
        );
        let opacity = if flashing { 1.0 } else { 0.5 + 0.5 * eased };
        if flashing {
            ctx.request_repaint();
        }
        let color = HEADER_COLOR.gamma_multiply(opacity);
        let galley = DROPDOWN.single_line(painter, label, color, None);
        paint_text(painter, line.min, galley, &DROPDOWN);
        // The arrow's 7px inline block sits on the baseline, the 10×6 glyph at its top.
        let baseline = line.top() + DROPDOWN.baseline(ctx);
        let arrow = Rect::from_min_size(
            Pos2::new(line.left() + text_box + ARROW_GAP, baseline - 7.0),
            ARROW,
        );
        paint_icon(
            ui,
            "legacy/DEPRECATED_dropdown",
            arrow,
            Color32::WHITE.gamma_multiply(opacity),
        );
        if open {
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(line.left(), line.bottom()), Vec2::new(width, 2.0)),
                0.0,
                INPUT_BAR,
            );
        }
    }

    /// Paints the dropdown's open menu under `rect`.
    pub(crate) fn paint_menu(
        &self,
        ui: &Ui,
        state: &mut DropdownState,
        highlight: usize,
        rect: Rect,
    ) {
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            Order::Tooltip,
            Id::new("exo_commands::palette_menu"),
        ));
        let top = rect.top() + 10.0 + DROPDOWN.line_height + 8.0;
        let menu = Rect::from_min_size(
            Pos2::new(rect.left(), top),
            Vec2::new(rect.width(), 16.0 + MENU_ITEM * self.options.len() as f32),
        );
        painter.rect_filled(menu, 0.0, Color32::WHITE);
        let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
        for (index, option) in self.options.iter().enumerate() {
            let item = Rect::from_min_size(
                Pos2::new(menu.left(), menu.top() + 8.0 + MENU_ITEM * index as f32),
                Vec2::new(menu.width(), MENU_ITEM),
            );
            let response = ui.interact(
                item,
                exo_ui::id(ui.ctx(), ("exo_commands::palette_menu_item", index)),
                Sense::click(),
            );
            if response.hovered() && pointer_moved {
                state.menu = Some(index);
            }
            if response.clicked() {
                state.choice = index;
                state.menu = None;
                state.picked = Some(index);
            }
            let highlighted = index == highlight;
            if highlighted {
                painter.rect_filled(item, 0.0, Color32::from_rgb(0xe4, 0xe4, 0xe4));
            }
            let color = Color32::from_black_alpha(if highlighted { 153 } else { 102 });
            let galley = DROPDOWN_ITEM.single_line(&painter, option, color, None);
            let width = galley.size().x;
            paint_text(
                &painter,
                Pos2::new(item.right() - MENU_PADDING - width, item.top() + 2.0),
                galley,
                &DROPDOWN_ITEM,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Modifiers, RawInput};

    use super::*;

    fn send_key(ctx: &Context, dropdown: &Dropdown, state: &mut DropdownState, key: Key) {
        let input = RawInput {
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            dropdown.keys(ui.ctx(), state);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn three_item_dropdown_steps_up_with_arrow_up_and_k_and_down_with_arrow_down_and_j() {
        let ctx = Context::default();
        let dropdown = Dropdown::new(
            vec!["if no reply", "regardless", "every week"],
            0,
            "mod+shift+l",
        );
        let mut state = DropdownState {
            menu: Some(0),
            ..DropdownState::new(0)
        };

        // ArrowUp steps upward: 0 -> 2 -> 1 -> 0.
        send_key(&ctx, &dropdown, &mut state, Key::ArrowUp);
        assert_eq!(state.menu, Some(2));
        send_key(&ctx, &dropdown, &mut state, Key::ArrowUp);
        assert_eq!(state.menu, Some(1));
        send_key(&ctx, &dropdown, &mut state, Key::ArrowUp);
        assert_eq!(state.menu, Some(0));

        // Plain `K` also steps upward: 0 -> 2 -> 1.
        send_key(&ctx, &dropdown, &mut state, Key::K);
        assert_eq!(state.menu, Some(2));
        send_key(&ctx, &dropdown, &mut state, Key::K);
        assert_eq!(state.menu, Some(1));

        // ArrowDown and `J` step downward: 1 -> 2 -> 0, and Enter picks.
        send_key(&ctx, &dropdown, &mut state, Key::ArrowDown);
        assert_eq!(state.menu, Some(2));
        send_key(&ctx, &dropdown, &mut state, Key::J);
        assert_eq!(state.menu, Some(0));
        send_key(&ctx, &dropdown, &mut state, Key::ArrowUp);
        assert_eq!(state.menu, Some(2));
        send_key(&ctx, &dropdown, &mut state, Key::Enter);
        assert_eq!((state.choice, state.menu, state.picked), (2, None, Some(2)));
    }
}
