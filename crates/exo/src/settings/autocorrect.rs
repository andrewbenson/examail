//! The Autocorrect dialog and the typing commands. The dialog has an intro and a `Use
//! Autocorrect` switch, without `Languages`, because macOS's spellchecker detects each
//! paragraph's language itself.

use egui::{Color32, Context, Rect, Ui, pos2, vec2};
use exo_commands::{Command, Commands};
use exo_ui::{
    DIALOG_SWITCH_ROW_HEIGHT, Dialog, DialogResponse, Family, Font, SwitchRow, TextStyle,
};

use super::{Action, Dialog as Which, Toggle};

/// The header's `margin-bottom: 24px`.
const HEADER_GAP: f32 = 24.0;
/// `h2`: 12px/20px, `top: 3px`, the first with `margin-bottom: 8px`.
const INTRO: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const NUDGE: f32 = 3.0;
const INTRO_GAP: f32 = 8.0;
/// The switch row: `padding: 4px 0; margin: 16px 0 15px`.
const SWITCH_ABOVE: f32 = 16.0;
const SWITCH_BELOW: f32 = 15.0;
/// The rule under the switch: 1px `rgba(0, 0, 0, .15)`, 16px above what follows.
const RULE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 38);
const SECTION_BELOW: f32 = 16.0;
/// The `h2`s' and the label's `rgba(0, 0, 0, .85)` (Carbon's dialog keeps it).
const INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const PARAGRAPHS: [&str; 2] = [
    "Autocorrect fixes errors as you go — extra characters, missing characters, transposed characters, missing punctuation, incorrect capitalization — to name just a few.",
    "Most importantly, you go faster. Autocorrect increases typing speed by 30–50%.",
];

/// Draws the dialog over `area` with the switch `on`. A click on its row turns autocorrect the
/// other way.
pub fn dialog(ctx: &Context, area: Rect, open: bool, on: bool) -> DialogResponse<Option<Action>> {
    let width = exo_ui::DIALOG_WIDTH - 2.0 * 32.0;
    let lines: Vec<Vec<String>> = PARAGRAPHS
        .iter()
        .map(|text| INTRO.wrap(ctx, text, width))
        .collect();
    let intro = lines.iter().map(Vec::len).sum::<usize>() as f32 * INTRO.line_height + INTRO_GAP;
    let body = HEADER_GAP
        + intro
        + SWITCH_ABOVE
        + DIALOG_SWITCH_ROW_HEIGHT
        + SWITCH_BELOW
        + 1.0
        + SECTION_BELOW;
    Dialog::new("autocorrect", "Autocorrect", body)
        .show(ctx, area, open, |ui, body| show(ui, body, &lines, on))
}

fn show(ui: &Ui, body: Rect, paragraphs: &[Vec<String>], on: bool) -> Option<Action> {
    let painter = ui.painter().clone();
    let mut y = body.top() + HEADER_GAP;
    for (at, lines) in paragraphs.iter().enumerate() {
        for line in lines {
            INTRO.paint_line(&painter, pos2(body.left(), y + NUDGE), line, INK, None);
            y += INTRO.line_height;
        }
        if at == 0 {
            y += INTRO_GAP;
        }
    }
    y += SWITCH_ABOVE;
    let response = SwitchRow::dialog(
        exo_ui::id(ui.ctx(), "autocorrect-use"),
        pos2(body.left(), y),
        body.width(),
        "Use Autocorrect",
        on,
    )
    .slider_id(exo_ui::id(ui.ctx(), "autocorrect-use-slider"))
    .show(ui);
    let rule = y + DIALOG_SWITCH_ROW_HEIGHT + SWITCH_BELOW;
    painter.rect_filled(
        Rect::from_min_size(pos2(body.left(), rule), vec2(body.width(), 1.0)),
        0.0,
        RULE,
    );
    response.clicked().then_some(Action::SetAutocorrect {
        on: !on,
        hint: false,
    })
}

/// Registers the typing commands for this frame, titled by whether `autocorrect` and
/// `autocomplete` are on: `Enable/Disable Autocorrect` (`Spellcheck`, `Auto-Correct`), `Enable/
/// Disable Autocomplete` and `Autocorrect Settings` (`Autocorrect Languages`).
pub fn register<A: Clone + 'static>(
    commands: &mut Commands<A>,
    autocorrect: bool,
    autocomplete: bool,
    wrap: impl Fn(Action) -> A,
) {
    let gear = |id: &'static str, title: &'static str, action: Action| {
        Command::new(id, title, wrap(action))
            .default_score(0.0)
            .icon("data/settings")
    };
    let title = if autocorrect {
        "Disable Autocorrect"
    } else {
        "Enable Autocorrect"
    };
    commands.add(
        gear(
            "TOGGLE_AUTOCORRECT",
            title,
            Action::SetAutocorrect {
                on: !autocorrect,
                hint: true,
            },
        )
        .aliases(["Spellcheck", "Auto-Correct"]),
    );
    let title = if autocomplete {
        "Disable Autocomplete"
    } else {
        "Enable Autocomplete"
    };
    commands.add(gear(
        "TOGGLE_AUTOCOMPLETE",
        title,
        Action::Flip {
            setting: Toggle::Autocomplete,
            palette: true,
        },
    ));
    commands.add(
        gear(
            "AUTOCORRECT_SETTINGS",
            "Autocorrect Settings",
            Action::Open(Which::Autocorrect),
        )
        .aliases(["Autocorrect Languages"]),
    );
}
