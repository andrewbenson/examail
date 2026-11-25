//! The Downloads dialog, which picks where attachments download to, app-wide
//! ([`crate::downloads`]). It has a line of explanation, then `Download attachments to:` and a
//! dropdown of `Downloads`, `Desktop`, the folder chosen before (by its path) and `Other…`, which
//! asks for a folder. A choice applies at once, and there's no Save. The dropdown has the keyboard
//! from the start.

use std::path::{Path, PathBuf};

use egui::{Color32, Context, Rect, Ui, pos2};
use exo_commands::{Command, Commands, Region};
use exo_ui::{
    Dialog, DialogResponse, DialogSelect, DialogSelectEvent, DialogSelectState, Family, Font,
    TextStyle,
};

/// The subheader: 12px/20px, `padding-top: 24px`, 3px down, `margin-bottom: 16px`.
const LINE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const SUBHEADER_TOP: f32 = 27.0;
/// The dropdown's row (`padding: 4px 0`) sits 60px down, with its label 3px down its line. The
/// dropdown's value is 1px past the label's `margin-right: 4px` and 2px lower.
const LABEL_TOP: f32 = 67.0;
const VALUE_LEFT: f32 = 5.0;
const VALUE_TOP: f32 = 69.0;
const BODY_HEIGHT: f32 = 88.0;
/// The subheader's `rgba(0, 0, 0, .85)`, the label's `.7`.
const SUBHEADER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 179);
const SUBHEADER: &str = "Choose where to download attachments.";
const LABEL: &str = "Download attachments to:";

/// A row of the dropdown (`{ name, id }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// `Downloads`.
    Downloads,
    /// `Desktop`.
    Desktop,
    /// The folder chosen before, by its path (`current`).
    Current(PathBuf),
    /// `Other…`, which opens a folder chooser (`pick`).
    Other,
}

impl Choice {
    fn name(&self) -> String {
        match self {
            Self::Downloads => "Downloads".into(),
            Self::Desktop => "Desktop".into(),
            Self::Current(path) => path.to_string_lossy().into_owned(),
            Self::Other => "Other…".into(),
        }
    }
}

/// The rows for the folder that downloads go to (`chosen`, `None` for the Downloads folder). That
/// folder's row comes first and the rest by name, as dialog dropdowns sort them.
pub fn choices(
    chosen: Option<&Path>,
    downloads: Option<&Path>,
    desktop: Option<&Path>,
) -> Vec<Choice> {
    let mut choices = vec![Choice::Downloads, Choice::Desktop];
    let selected = match chosen {
        None => Choice::Downloads,
        Some(folder) if Some(folder) == downloads => Choice::Downloads,
        Some(folder) if Some(folder) == desktop => Choice::Desktop,
        Some(folder) => {
            choices.push(Choice::Current(folder.to_owned()));
            Choice::Current(folder.to_owned())
        }
    };
    choices.push(Choice::Other);
    // The selected one first, then by name.
    choices.sort_by_cached_key(|choice| {
        format!(
            "{}{}",
            if *choice == selected { 'a' } else { 'b' },
            choice.name()
        )
    });
    choices
}

/// Something done in the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Open or close the list (a click on the dropdown, Enter or an arrow on it).
    ToggleMenu,
    /// ↓ (1) or ↑ (-1) in the open list.
    MenuMove(isize),
    /// Escape with the list open.
    MenuClose,
    /// The pointer on a row of the list.
    Highlight(usize),
    /// Enter or Tab on the open list, which picks its highlighted row.
    MenuPick,
    /// A row, clicked or picked.
    Choose(Choice),
}

/// The open list's highlighted row, between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    menu: DialogSelectState,
}

impl State {
    /// Whether the list is open, so that Escape closes it rather than the dialog.
    pub fn menu_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Registers the dialog's keyboard bindings in `Region::Dialog` (plus its alert's dismiss
    /// bindings while `alert` is showing).
    pub fn register_keys<A: Clone + 'static>(
        &self,
        commands: &mut Commands<A>,
        alert: bool,
        wrap: &impl Fn(super::Action) -> A,
    ) {
        const DIALOG: &[Region] = &[Region::Dialog];
        let key = |id: &'static str, event: Event, keys: &[&'static str]| {
            Command::shortcut(id, wrap(super::Action::Downloads(event)))
                .keys(keys.iter().copied())
                .regions(DIALOG)
        };
        if self.menu_open() {
            commands.add(key(
                "DOWNLOADS_MENU_DOWN",
                Event::MenuMove(1),
                &["down", "j", "n"],
            ));
            commands.add(key(
                "DOWNLOADS_MENU_UP",
                Event::MenuMove(-1),
                &["up", "k", "p"],
            ));
            commands.add(key(
                "DOWNLOADS_MENU_PICK",
                Event::MenuPick,
                &["enter", "tab"],
            ));
            commands.add(key("DOWNLOADS_MENU_ESCAPE", Event::MenuClose, &["esc"]));
        } else {
            commands.add(key(
                "DOWNLOADS_MENU_OPEN",
                Event::ToggleMenu,
                &["enter", "space", "down", "j", "n", "up", "k", "p"],
            ));
        }
        if alert {
            crate::notice::register_alert_dismiss(
                commands,
                wrap(super::Action::DismissDownloadsAlert),
            );
        }
    }

    /// Applies `event` to the list, whose rows are `choices`, and returns the row chosen.
    pub fn handle(&mut self, event: Event, choices: &[Choice]) -> Option<Choice> {
        match event {
            Event::ToggleMenu => self.menu.toggle(0),
            Event::MenuMove(step) => self.menu.step(step, choices.len(), 0, true, false),
            Event::MenuClose => self.menu.close(),
            Event::Highlight(row) => self.menu.open_at(row),
            Event::MenuPick => return self.menu.confirm().and_then(|at| choices.get(at).cloned()),
            Event::Choose(choice) => {
                self.menu.close();
                return Some(choice);
            }
        }
        None
    }
}

/// Draws the dialog over `area` with `choices` (the first being the folder downloads go to), and
/// returns what was done.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    state: &State,
    choices: &[Choice],
) -> DialogResponse<Option<Event>> {
    Dialog::new("downloads", "Downloads", BODY_HEIGHT)
        .show(ctx, area, open, |ui, body| draw(ui, body, state, choices))
}

fn draw(ui: &mut Ui, body: Rect, state: &State, choices: &[Choice]) -> Option<Event> {
    let painter = ui.painter().clone();
    let (left, top) = (body.left(), body.top());
    LINE.paint_line(
        &painter,
        pos2(left, top + SUBHEADER_TOP),
        SUBHEADER,
        SUBHEADER_INK,
        None,
    );
    let label = LINE.paint_line(
        &painter,
        pos2(left, top + LABEL_TOP),
        LABEL,
        LABEL_INK,
        None,
    );
    let names: Vec<String> = choices.iter().map(Choice::name).collect();
    let items: Vec<&str> = names.iter().map(String::as_str).collect();
    let selected = items.first().copied().unwrap_or("Downloads");
    DialogSelect::new(
        exo_ui::id(ui.ctx(), "downloads-location"),
        pos2(left + label + VALUE_LEFT, top + VALUE_TOP),
        selected,
        &items,
        state.menu.highlighted(),
    )
    .show(ui)
    .map(|ev| match ev {
        DialogSelectEvent::Toggle => Event::ToggleMenu,
        DialogSelectEvent::Highlight(row) => Event::Highlight(row),
        DialogSelectEvent::Pick(row) => Event::Choose(choices[row].clone()),
        DialogSelectEvent::Close => Event::MenuClose,
    })
}

/// `Could not change download folder` over the dialog in `area`. Returns whether it was
/// dismissed (`OK`, or a click beside it).
pub fn alert(ctx: &Context, area: Rect) -> bool {
    let (clicked, backdrop) = (exo_ui::Alert {
        title: "Could not change download folder",
        text: "Your download folder could not be changed. Please try again.",
        actions: &["OK"],
        selected: 0,
    })
    .show_modal(ctx, "downloads-alert", area);
    clicked.is_some() || backdrop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_in_use_comes_first_and_the_rest_by_name() {
        let (downloads, desktop) = (
            Path::new("/Users/maya/Downloads"),
            Path::new("/Users/maya/Desktop"),
        );
        let names = |chosen: Option<&Path>| {
            choices(chosen, Some(downloads), Some(desktop))
                .iter()
                .map(Choice::name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(None), ["Downloads", "Desktop", "Other…"]);
        assert_eq!(names(Some(downloads)), ["Downloads", "Desktop", "Other…"]);
        assert_eq!(names(Some(desktop)), ["Desktop", "Downloads", "Other…"]);
        assert_eq!(
            names(Some(Path::new("/Volumes/Work/Inbox"))),
            ["/Volumes/Work/Inbox", "Desktop", "Downloads", "Other…"]
        );
    }

    #[test]
    fn the_keyboard_opens_moves_through_and_picks_from_the_list() {
        let rows = choices(None, None, None);
        let mut state = State::default();
        assert_eq!(state.handle(Event::ToggleMenu, &rows), None);
        assert_eq!(state.menu.highlighted(), Some(0), "on the folder in use");
        state.handle(Event::MenuMove(-1), &rows);
        assert_eq!(state.menu.highlighted(), Some(2), "wraps");
        assert_eq!(state.handle(Event::MenuPick, &rows), Some(Choice::Other));
        assert!(!state.menu_open());
        state.handle(Event::ToggleMenu, &rows);
        assert_eq!(state.handle(Event::MenuClose, &rows), None);
        assert!(!state.menu_open());
    }
}
