//! The Images dialog, which picks the remote images that message bodies load. It has a line of
//! explanation, three radio buttons, then how to show a message's blocked images. Choosing one
//! applies it at once, and there's no Save.

use egui::{Color32, Context, Rect, Ui, pos2};
use exo_store::RemoteContentBlocking;
use exo_ui::{Dialog, DialogResponse, Family, Font, RadioGroup, TextStyle, paint_dialog_footer};

use super::Action;

/// The subheader: 12px/20px lines, `padding-top: 24px`, 3px down (`top: 3px`).
const PADDING_TOP: f32 = 24.0;
const NUDGE: f32 = 3.0;
const LINE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
/// The radio rows follow the explanation and a `<br>`.
const ROWS_TOP: f32 = 2.0 * 20.0;
/// The footer, inside the subheader: `padding-top: 20px` and another 3px down, around the
/// keycaps' 21px line.
const FOOTER_PADDING: f32 = 20.0;
const FOOTER_LINE: f32 = 21.0;
const BODY_HEIGHT: f32 = PADDING_TOP + ROWS_TOP + 3.0 * 20.0 + FOOTER_PADDING + FOOTER_LINE;
/// The subheader's `rgba(0, 0, 0, .85)`.
const LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const SUBHEADER: &str = "Choose which images to display.";
/// The radio rows, in order.
const CHOICES: [(&str, RemoteContentBlocking); 3] = [
    ("Show all images", RemoteContentBlocking::None),
    (
        "Block all known tracking pixels",
        RemoteContentBlocking::ReduceTracking,
    ),
    ("Block all remote images", RemoteContentBlocking::All),
];

/// Draws the dialog over `area` with `current` checked. A click on another choice sets it.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    current: RemoteContentBlocking,
) -> DialogResponse<Option<Action>> {
    Dialog::new("images", "Images", BODY_HEIGHT)
        .show(ctx, area, open, |ui, body| picker(ui, body, current))
}

fn picker(ui: &mut Ui, body: Rect, current: RemoteContentBlocking) -> Option<Action> {
    let top = body.top() + NUDGE + PADDING_TOP;
    LINE.paint_line(
        ui.painter(),
        pos2(body.left(), top),
        SUBHEADER,
        LABEL_INK,
        None,
    );
    let chosen = RadioGroup::new("images-choice", pos2(body.left(), top + ROWS_TOP), &CHOICES)
        .show(ui, current)
        .map(Action::SetImages);
    // `To show blocked images in a message, hit ⌘ K → Show Images.`
    let footer_top = top + ROWS_TOP + 3.0 * LINE.line_height + NUDGE + FOOTER_PADDING;
    paint_dialog_footer(
        ui,
        body.left(),
        footer_top,
        "To show blocked images in a message, hit",
        exo_ui::KEYCAP_GAP,
        0.0,
        " → Show Images.",
    );
    chosen
}
