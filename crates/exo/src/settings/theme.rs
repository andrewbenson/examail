//! The Theme dialog has three radio buttons, then a line about the dark theme. Choosing one
//! applies it at once, and there's no Save. Snow and Carbon read `Light` and `Dark` here.

use egui::{Color32, Context, Rect, Ui, pos2};
use exo_store::Theme;
use exo_ui::{Dialog, DialogResponse, Family, Font, RadioGroup, TextStyle};

use super::Action;

/// The subheader's `padding-top: 20px` over its 12px/20px lines,
/// which sit 3px down (`top: 3px`).
const PADDING_TOP: f32 = 20.0;
const NUDGE: f32 = 3.0;
const LINE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
/// The footer's `padding: 17px 0 2px`, under the three lines. Like every dialog footer, it
/// sits another 3px down.
const FOOTER_PADDING: [f32; 2] = [17.0, 2.0];
const BODY_HEIGHT: f32 = PADDING_TOP + 3.0 * 20.0 + FOOTER_PADDING[0] + 20.0 + FOOTER_PADDING[1];
/// The footer's `rgba(0, 0, 0, .6)`.
const FOOTER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 153);
const FOOTER: &str =
    "The Dark theme can help reduce eyestrain and is best used in low light conditions.";

/// Draws the dialog over `area` with `current` checked. A click on a choice sets it.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    current: Theme,
) -> DialogResponse<Option<Action>> {
    let os = ctx.os();
    Dialog::new("theme", "Theme", BODY_HEIGHT)
        .show(ctx, area, open, |ui, body| picker(ui, body, current, os))
}

fn picker(
    ui: &mut Ui,
    body: Rect,
    current: Theme,
    os: egui::os::OperatingSystem,
) -> Option<Action> {
    let match_os = format!("Match {}", super::os_name(os));
    let choices = [
        ("Light", Theme::Snow),
        ("Dark", Theme::Carbon),
        (match_os.as_str(), Theme::Auto),
    ];
    let top = body.top() + NUDGE + PADDING_TOP;
    let chosen = RadioGroup::new("theme-choice", pos2(body.left(), top), &choices)
        .show(ui, current)
        .map(Action::SetTheme);
    let footer_top = top + 3.0 * LINE.line_height + NUDGE + FOOTER_PADDING[0];
    LINE.paint_line(
        ui.painter(),
        pos2(body.left(), footer_top),
        FOOTER,
        FOOTER_INK,
        None,
    );
    chosen
}
