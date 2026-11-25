//! Shared RSVP answer type, button-bar rendering, palette commands (`RSVP: Yes`, `RSVP: Maybe`,
//! `RSVP: No`), and `RSVP + Mark Done`.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Ui, pos2, vec2};
use exo_commands::{CONVERSATION, Command, Commands};
use exo_store::Mutation;
use exo_ui::{BoxShadow, Palette, TextStyle, Theme, motion};

use crate::{Mail, actionable, commands::Action};

/// An attendee's RSVP answer (`responseStatus` in Google Calendar, `PARTSTAT` in iCalendar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rsvp {
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

/// The three interactive RSVP choices in button order (`Yes`, `Maybe`, `No`).
pub const ANSWERS: [Rsvp; 3] = [Rsvp::Accepted, Rsvp::Tentative, Rsvp::Declined];

const SELECTED_ACCENT: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);
const FOCUS_GLOW: BoxShadow =
    BoxShadow::new([0.0, 0.0], 6.0, 1.0, Color32::from_rgb(0xae, 0xb1, 0xdd));
const SELECTED_FOCUS_GLOW: BoxShadow = BoxShadow::new([0.0, 0.0], 6.0, 0.0, SELECTED_ACCENT);
const FOCUS_HAZE: BoxShadow = BoxShadow::new(
    [0.0, 2.0],
    12.0,
    0.0,
    Color32::from_rgba_unmultiplied_const(0xae, 0xb1, 0xdd, 128),
);

impl Rsvp {
    /// Google Calendar's `responseStatus` string for this answer.
    #[must_use]
    pub const fn status(self) -> &'static str {
        match self {
            Self::NeedsAction => "needsAction",
            Self::Accepted => "accepted",
            Self::Tentative => "tentative",
            Self::Declined => "declined",
        }
    }

    /// Parses Google Calendar's `responseStatus`.
    #[must_use]
    pub fn parse(status: &str) -> Option<Self> {
        Some(match status {
            "needsAction" => Self::NeedsAction,
            "accepted" => Self::Accepted,
            "tentative" => Self::Tentative,
            "declined" => Self::Declined,
            _ => return None,
        })
    }

    /// Parses an optional Google Calendar `responseStatus`.
    #[must_use]
    pub fn parse_opt(status: Option<&str>) -> Option<Self> {
        status.and_then(Self::parse)
    }

    /// Parses an iCalendar `PARTSTAT` parameter value, defaulting to [`Self::NeedsAction`].
    #[must_use]
    pub fn parse_ics(partstat: Option<&str>) -> Self {
        match partstat {
            Some(s) if s.eq_ignore_ascii_case("ACCEPTED") => Self::Accepted,
            Some(s) if s.eq_ignore_ascii_case("TENTATIVE") => Self::Tentative,
            Some(s) if s.eq_ignore_ascii_case("DECLINED") => Self::Declined,
            _ => Self::NeedsAction,
        }
    }

    /// Human-readable toast word (`"Yes"`, `"Maybe"`, `"No"`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Accepted => "Yes",
            Self::Tentative => "Maybe",
            Self::Declined | Self::NeedsAction => "No",
        }
    }

    /// Uppercase button caption (`"YES"`, `"MAYBE"`, `"NO"`, `"ADD TO CALENDAR"`).
    #[must_use]
    pub const fn button_label(self) -> &'static str {
        match self {
            Self::Accepted => "YES",
            Self::Tentative => "MAYBE",
            Self::Declined => "NO",
            Self::NeedsAction => "ADD TO CALENDAR",
        }
    }
}

/// Geometry and animation options for a shared RSVP button or `Yes / Maybe / No` button row.
#[derive(Clone, Copy)]
pub(crate) struct RsvpButtonMetrics {
    pub font: TextStyle,
    pub height: f32,
    pub padding_x: f32,
    pub gap: f32,
    /// Offset from the button's `top()` to the text line's `top()`.
    pub text_top: f32,
    /// Whether hover/press transitions ease over 150ms (calendar details) vs instant + keyboard
    /// focus glow (thread invite card).
    pub animated: bool,
}

/// Paints a `Yes / Maybe / No` button strip starting at `origin` (top-left of the first button)
/// and returns the newly chosen [`Rsvp`] when a non-selected button is clicked.
pub(crate) fn show_rsvp_buttons(
    ui: &Ui,
    palette: &Palette,
    id_base: egui::Id,
    origin: Pos2,
    current: Option<Rsvp>,
    focused: Option<usize>,
    metrics: RsvpButtonMetrics,
) -> Option<Rsvp> {
    let mut x = origin.x;
    let mut chosen = None;
    for (index, answer) in ANSWERS.into_iter().enumerate() {
        let label = answer.button_label();
        let selected = current == Some(answer);
        let (button, clicked) = show_rsvp_button(
            ui,
            palette,
            id_base.with(label),
            pos2(x, origin.y),
            label,
            selected,
            focused == Some(index),
            metrics,
        );
        if clicked && !selected {
            chosen = Some(answer);
        }
        x = button.right() + metrics.gap;
    }
    chosen
}

/// Paints one RSVP button at `top_left` and returns `(rect, clicked)`.
#[expect(
    clippy::too_many_arguments,
    reason = "shared button primitive parameterized by card metrics and interaction state"
)]
pub(crate) fn show_rsvp_button(
    ui: &Ui,
    palette: &Palette,
    id: egui::Id,
    top_left: Pos2,
    label: &str,
    selected: bool,
    focused: bool,
    metrics: RsvpButtonMetrics,
) -> (Rect, bool) {
    let painter = ui.painter();
    let text_width = if metrics.animated {
        metrics.font.width(ui.ctx(), label)
    } else {
        metrics
            .font
            .single_line(painter, label, Color32::PLACEHOLDER, None)
            .size()
            .x
    };
    let button = Rect::from_min_size(
        top_left,
        vec2(text_width + 2.0 * metrics.padding_x, metrics.height),
    );
    let response = ui
        .interact(button, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let snapped = exo_ui::pixel_snap(button);
    let carbon = palette.theme == Theme::Carbon;
    let radius = CornerRadius::same(2);

    let ink = if metrics.animated {
        let ease = |on: bool, salt: &str| {
            motion::transition(
                ui.ctx(),
                response.id.with(salt),
                on,
                0.15,
                motion::CubicBezier::EASE,
            )
        };
        let active = ease(selected || response.is_pointer_button_down_on(), "selected");
        let lifted = ease(response.hovered(), "hovered") * (1.0 - active);
        if lifted > 0.0 {
            BoxShadow::new(
                [0.0, 0.0],
                8.0,
                0.0,
                Color32::from_black_alpha(if carbon { 128 } else { 51 }).gamma_multiply(lifted),
            )
            .paint(painter, snapped, radius);
        }
        if active > 0.0 {
            painter.rect_filled(
                snapped,
                radius,
                SELECTED_ACCENT.gamma_multiply(if carbon { 0.15 } else { 0.1 } * active),
            );
        }
        motion::mix(
            palette.ink(0.35, 0.45),
            SELECTED_ACCENT.gamma_multiply(0.8),
            active,
        )
    } else {
        let hovered = response.hovered();
        if focused {
            for shadow in [
                if selected {
                    SELECTED_FOCUS_GLOW
                } else {
                    FOCUS_GLOW
                },
                FOCUS_HAZE,
            ] {
                shadow.paint(painter, snapped, radius);
            }
        } else if hovered && !selected {
            BoxShadow::new(
                [0.0, 0.0],
                8.0,
                0.0,
                Color32::from_black_alpha(if carbon { 128 } else { 51 }),
            )
            .paint(painter, snapped, radius);
        }
        if hovered || selected || focused {
            painter.rect_filled(snapped, radius, palette.button_ghost_fill_active);
        }
        if selected {
            Color32::from_rgba_unmultiplied(0x54, 0xac, 0xdc, if carbon { 204 } else { 230 })
        } else if hovered || focused {
            palette.ink(0.35, 0.35)
        } else {
            palette.button_ghost_content_default
        }
    };

    metrics.font.paint_line(
        painter,
        pos2(
            button.left() + metrics.padding_x,
            button.top() + metrics.text_top,
        ),
        label,
        ink,
        None,
    );
    (button, response.clicked())
}

/// A palette row: its id, answer, title, aliases, operative word, icon and default score
/// (`scale: 3` each).
type Row = (
    &'static str,
    Rsvp,
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
    f64,
);

const ROWS: [Row; 3] = [
    (
        "RSVP_ACCEPTED",
        Rsvp::Accepted,
        "RSVP: Yes",
        &["Accept", "Going"],
        "Yes",
        "data/rsvp-accepted",
        20.0,
    ),
    (
        "RSVP_TENTATIVE",
        Rsvp::Tentative,
        "RSVP: Maybe",
        &["Tentative"],
        "Maybe",
        "data/rsvp-tentative",
        19.0,
    ),
    (
        "RSVP_DECLINED",
        Rsvp::Declined,
        "RSVP: No",
        &["Decline", "Not going"],
        "No",
        "data/rsvp-declined",
        18.0,
    ),
];

/// Registers the RSVP rows for one conversation with an invitation to answer, titled `… and
/// Mark Done` when an answer would mark it done.
pub(crate) fn register(commands: &mut Commands<Action>, marks_done: bool) {
    for (id, response, title, aliases, operative, icon, score) in ROWS {
        let title = if marks_done {
            format!("{title} and Mark Done")
        } else {
            title.to_owned()
        };
        commands.add(
            Command::new(id, title, Action::Rsvp(response))
                .aliases(aliases.iter().copied())
                .operative_word(operative)
                .icon(icon)
                .default_score(score)
                .scale(3.0)
                .regions(CONVERSATION),
        );
    }
}

impl Mail {
    /// Whether the palette's RSVP rows apply, and if so whether an answer marks the conversation
    /// done. They apply when the one actionable conversation is the open one and it has an
    /// invitation to answer.
    pub(crate) fn rsvp_rows(&self) -> Option<bool> {
        let thread = self.thread.as_ref()?;
        thread.conversation_invite()?;
        let [_] = actionable(Some(thread), &self.inbox)[..] else {
            return None;
        };
        Some(self.rsvp_marks_done())
    }

    /// Whether answering marks the actionable conversations done, which it does when
    /// `RSVP + Mark Done` is on and they aren't all done already.
    pub(crate) fn rsvp_marks_done(&self) -> bool {
        let threads = actionable(self.thread.as_ref(), &self.inbox);
        self.account.settings().rsvp_and_done
            && !threads.is_empty()
            && !threads.iter().all(|thread| thread.done)
    }

    /// A palette row, which answers the open conversation's invitation, even with the answer it
    /// already has.
    pub(crate) fn rsvp_command(&mut self, response: Rsvp) {
        let Some(invite) = self
            .thread
            .as_ref()
            .and_then(crate::thread::ThreadView::conversation_invite)
        else {
            return;
        };
        let mut answered = invite.clone();
        answered.answer(&self.calendar_invite_account(invite), response);
        self.calendar_respond_to_invite(answered, response, false);
    }

    /// The toast an answer gets, unless it marks done.
    pub(crate) fn rsvp_toast(&self, response: Rsvp) -> String {
        match (self.snapshot.connection.is_online(), response) {
            (false, _) => "RSVP will be sent when online.".to_owned(),
            (true, Rsvp::NeedsAction) => "Event added to calendar.".to_owned(),
            (true, response) => format!("Responded {}.", response.word()),
        }
    }

    /// RSVP + Mark Done's second half. The actionable conversations are marked done, with Undo and
    /// a toast that says the answer, and leave the list the way Auto Advance says.
    pub(crate) fn rsvp_mark_done(&mut self, response: Rsvp) {
        let text = match (self.snapshot.connection.is_online(), response) {
            (false, Rsvp::NeedsAction) => "Event will be added when online.".to_owned(),
            (false, _) => "RSVP will be sent when online.".to_owned(),
            (true, Rsvp::NeedsAction) => "Event added to calendar and Marked Done.".to_owned(),
            (true, response) => format!("Responded {} and Marked Done.", response.word()),
        };
        let ids: Vec<String> = actionable(self.thread.as_ref(), &self.inbox)
            .iter()
            .map(|thread| thread.id.clone())
            .collect();
        let now = self.ctx.input(|input| input.time);
        self.triage_threads(&ids, Mutation::MarkDone, Some(text), 0, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsvp_parses_google_and_ics_statuses_and_formats_labels() {
        for answer in [
            Rsvp::NeedsAction,
            Rsvp::Accepted,
            Rsvp::Tentative,
            Rsvp::Declined,
        ] {
            assert_eq!(Rsvp::parse(answer.status()), Some(answer));
            assert_eq!(Rsvp::parse_opt(Some(answer.status())), Some(answer));
        }
        assert_eq!(Rsvp::parse("unknown"), None);
        assert_eq!(Rsvp::parse_ics(Some("accepted")), Rsvp::Accepted);
        assert_eq!(Rsvp::parse_ics(Some("TENTATIVE")), Rsvp::Tentative);
        assert_eq!(Rsvp::parse_ics(Some("Declined")), Rsvp::Declined);
        assert_eq!(Rsvp::parse_ics(None), Rsvp::NeedsAction);
        assert_eq!(ANSWERS.map(Rsvp::word), ["Yes", "Maybe", "No"]);
        assert_eq!(ANSWERS.map(Rsvp::button_label), ["YES", "MAYBE", "NO"]);
    }
}
