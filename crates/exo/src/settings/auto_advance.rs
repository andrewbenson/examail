//! The Auto Advance dialog, which picks where triaging a conversation goes next. A choice applies
//! at once.
//!
//! The dropdown has the keyboard from the start and lists its choice first, then the others by
//! name. Metrics are at a 1280×800 window. In Carbon the card keeps Snow's colours.

use egui::{Color32, Context, Rect, Ui, pos2};
use exo_commands::{Command, Commands, Region};
use exo_store::AutoAdvance;
use exo_ui::{
    Dialog, DialogMenuKind, DialogResponse, DialogSelect, DialogSelectEvent, DialogSelectState,
    Family, Font, TextStyle,
};

/// The subheader and the dropdown's label: 12px/20px, 3px down their lines.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
/// How far below the body's top the subheader's line (`padding-top: 24px`), the label's line
/// (`margin-bottom: 16px`, the row's `padding: 4px 0`) and the dropdown's value sit. The body
/// ends with the row.
const SUBHEADER_TOP: f32 = 3.0 + 24.0;
const LABEL_TOP: f32 = 44.0 + 16.0 + 4.0 + 3.0;
const VALUE_TOP: f32 = 69.0;
const BODY_HEIGHT: f32 = 88.0;
/// The label's `margin-right: 4px`, and the value 1px into its button.
const VALUE_LEFT: f32 = 5.0;
/// The subheader is `rgba(0,0,0,.85)`, and the dropdown's label `.7`.
const SUBHEADER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 179);
const SUBHEADER: &str =
    "Choose where you go after triaging a conversation (e.g. Mark Done, Remind Me, Trash).";
const LABEL: &str = "After triaging a conversation:";

/// Each choice's name, in order.
const CHOICES: [(AutoAdvance, &str); 3] = [
    (AutoAdvance::Down, "go down and show the older conversation"),
    (AutoAdvance::Up, "go up and show the newer conversation"),
    (AutoAdvance::Off, "go back to the list of conversations"),
];

/// Something done in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A click on the dropdown, or Enter on it, which opens or closes the list.
    ToggleMenu,
    /// ↓ (1) or ↑ (-1), which opens the list or moves its highlight round.
    Move(isize),
    /// Enter or Tab in the open list, which picks its highlighted row.
    Enter,
    /// Escape, or a click elsewhere, with the list open, which closes it.
    MenuClose,
    /// The pointer on a row of the list.
    Highlight(usize),
    /// A choice.
    Choose(AutoAdvance),
}

/// The open list's highlighted row, between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    menu: DialogSelectState,
}

impl State {
    /// Whether the list is open, in which case its keys are the dialog's.
    pub fn menu_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Registers the dialog's keyboard bindings in `Region::Dialog`.
    pub fn register_keys<A: Clone + 'static>(
        &self,
        commands: &mut Commands<A>,
        wrap: &impl Fn(super::Action) -> A,
    ) {
        const DIALOG: &[Region] = &[Region::Dialog];
        let key = |id: &'static str, event: Event, keys: &[&'static str]| {
            Command::shortcut(id, wrap(super::Action::AutoAdvance(event)))
                .keys(keys.iter().copied())
                .regions(DIALOG)
        };
        commands.add(key(
            "AUTO_ADVANCE_DOWN",
            Event::Move(1),
            &["down", "j", "n"],
        ));
        commands.add(key("AUTO_ADVANCE_UP", Event::Move(-1), &["up", "k", "p"]));
        if self.menu_open() {
            commands.add(key("AUTO_ADVANCE_ENTER", Event::Enter, &["enter", "tab"]));
            commands.add(key("AUTO_ADVANCE_ESCAPE", Event::MenuClose, &["esc"]));
        } else {
            commands.add(key("AUTO_ADVANCE_ENTER", Event::ToggleMenu, &["enter"]));
        }
    }

    /// Applies `event` with `current` chosen, and returns a new choice to save.
    pub fn handle(&mut self, event: Event, current: AutoAdvance) -> Option<AutoAdvance> {
        match event {
            // The list opens on the choice, which it lists first.
            Event::ToggleMenu => self.menu.toggle(0),
            Event::Move(step) => self.menu.step(step, CHOICES.len(), 0, true, true),
            Event::Enter => {
                let at = self.menu.confirm()?;
                return Some(order(current)[at].0).filter(|chosen| *chosen != current);
            }
            Event::MenuClose => self.menu.close(),
            Event::Highlight(row) => self.menu.open_at(row),
            Event::Choose(chosen) => {
                self.menu.close();
                return (chosen != current).then_some(chosen);
            }
        }
        None
    }
}

/// The choices as the list shows them: `current` first, then the rest by name.
fn order(current: AutoAdvance) -> Vec<(AutoAdvance, &'static str)> {
    let mut choices = CHOICES.to_vec();
    choices.sort_by_key(|(choice, name)| (*choice != current, *name));
    choices
}

/// The name of `current`, the first choice's when it has none.
fn name(current: AutoAdvance) -> &'static str {
    CHOICES
        .iter()
        .find(|(choice, _)| *choice == current)
        .map_or(CHOICES[0].1, |(_, name)| name)
}

/// Draws the dialog over `area` while `open` (and as it fades), and returns what was done.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    state: &State,
    current: AutoAdvance,
) -> DialogResponse<Option<Event>> {
    Dialog::new("auto-advance", "Auto Advance", BODY_HEIGHT)
        .show(ctx, area, open, |ui, body| draw(ui, body, state, current))
}

fn draw(ui: &mut Ui, body: Rect, state: &State, current: AutoAdvance) -> Option<Event> {
    let painter = ui.painter().clone();
    let (left, top) = (body.left(), body.top());
    TEXT.paint_line(
        &painter,
        pos2(left, top + SUBHEADER_TOP),
        SUBHEADER,
        SUBHEADER_INK,
        None,
    );
    let label = TEXT.paint_line(
        &painter,
        pos2(left, top + LABEL_TOP),
        LABEL,
        LABEL_INK,
        None,
    );
    let choices = order(current);
    let names: Vec<&str> = choices.iter().map(|(_, name)| *name).collect();
    let select = DialogSelect::new(
        exo_ui::id(ui.ctx(), "auto-advance-dropdown"),
        pos2(left + label + VALUE_LEFT, top + VALUE_TOP),
        name(current),
        &names,
        state.menu.highlighted(),
    )
    .kind(DialogMenuKind::Fitted);
    select.show(ui).map(|ev| match ev {
        DialogSelectEvent::Toggle => Event::ToggleMenu,
        DialogSelectEvent::Highlight(row) => Event::Highlight(row),
        DialogSelectEvent::Pick(row) => Event::Choose(choices[row].0),
        DialogSelectEvent::Close => Event::MenuClose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_puts_the_choice_first_then_the_rest_by_name() {
        let names = |current| {
            order(current)
                .into_iter()
                .map(|(choice, _)| choice)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(AutoAdvance::Down),
            [AutoAdvance::Down, AutoAdvance::Off, AutoAdvance::Up]
        );
        assert_eq!(
            names(AutoAdvance::Up),
            [AutoAdvance::Up, AutoAdvance::Off, AutoAdvance::Down]
        );
        assert_eq!(
            names(AutoAdvance::Off),
            [AutoAdvance::Off, AutoAdvance::Down, AutoAdvance::Up]
        );
    }

    #[test]
    fn arrows_open_the_list_and_go_round_and_enter_chooses() {
        let mut state = State::default();
        assert_eq!(state.handle(Event::Move(1), AutoAdvance::Down), None);
        assert_eq!(
            state.menu.highlighted(),
            Some(0),
            "↓ opens the list on the choice"
        );
        state.handle(Event::Move(-1), AutoAdvance::Down);
        assert_eq!(state.menu.highlighted(), Some(2), "↑ wraps to the last row");
        assert_eq!(
            state.handle(Event::Enter, AutoAdvance::Down),
            Some(AutoAdvance::Up)
        );
        assert!(!state.menu_open());
        state.handle(Event::ToggleMenu, AutoAdvance::Down);
        assert_eq!(
            state.handle(Event::Enter, AutoAdvance::Down),
            None,
            "the choice again changes nothing"
        );
        assert_eq!(
            state.handle(Event::Choose(AutoAdvance::Off), AutoAdvance::Down),
            Some(AutoAdvance::Off)
        );
    }
}
