//! Out of Office Reply's alerts: offline, Gmail not answering, a save that didn't take, changes to
//! discard, a pasted image, and the nudge to set a reply for out-of-office days on the calendar.

use egui::{Color32, Context, Rect, Sense, pos2, vec2};
use exo_store::OutOfOfficeDays;
use exo_ui::{ALERT_PARAGRAPH_GAP, ALERT_TEXT, AlertClick, AlertRun, PARAGRAPH};
use jiff::tz::TimeZone;

/// The opt-out row has `margin: 20px 0 24px` and `rgba(0, 0, 0, .55)`, with its label 8px after
/// the 14px box and 1px up. The row is 21px tall.
const OPT_OUT_ABOVE: f32 = 20.0;
const OPT_OUT_HEIGHT: f32 = 21.0;
/// Its `margin-bottom: 24px` swallows the text's own 8px.
const OPT_OUT_BELOW: f32 = 24.0 - ALERT_PARAGRAPH_GAP;
const OPT_OUT_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 140);
const OPT_OUT_TEXT_LEFT: f32 = 22.0;
/// `Don't show this again`.
const OPT_OUT: &str = "Don't show this again";

/// An alert showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// The dialog was asked for offline.
    Offline,
    /// Gmail couldn't be read when the dialog opened. This alert closes the dialog too.
    SyncFailed,
    /// Gmail didn't take the save.
    SaveFailed,
    /// Closing a changed form.
    Discard,
    /// A file was pasted in the message, with `email` for the Gmail settings link.
    PastedFile {
        /// The account's address.
        email: String,
    },
    /// Out-of-office days are coming up, and whether `Don't show this again` is checked.
    Nudge {
        /// The days.
        days: OutOfOfficeDays,
        /// The box is checked.
        opt_out: bool,
        /// They come from an event just made in Exo, which the alert doesn't date.
        created: bool,
    },
}

/// One of an alert's text runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Text,
    Bold,
    Link,
}

impl Alert {
    /// The title.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Offline => "You are offline",
            Self::SyncFailed | Self::PastedFile { .. } => "Sorry!",
            Self::SaveFailed => "Changes not saved",
            Self::Discard => "Discard changes?",
            Self::Nudge { .. } => "Set Out of Office Reply?",
        }
    }

    /// The actions, right to left (the sync failure's is lowercase).
    pub fn actions(&self) -> &'static [&'static str] {
        match self {
            Self::Offline | Self::SaveFailed => &["OK"],
            Self::SyncFailed => &["ok"],
            Self::PastedFile { .. } => &["Ok"],
            Self::Discard => &["Discard", "Cancel"],
            Self::Nudge { .. } => &["Set reply", "No thanks"],
        }
    }

    /// The text as plain text, with `<p>`s on lines of their own. For tests.
    pub fn text(&self, tz: &TimeZone) -> String {
        self.paragraphs(tz)
            .iter()
            .map(|runs| {
                runs.iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn paragraphs(&self, tz: &TimeZone) -> Vec<Vec<(String, Run)>> {
        let text = |text: &str| vec![(text.to_owned(), Run::Text)];
        match self {
            Self::Offline => vec![text(
                "You cannot change your Out of Office Reply settings when offline. Please check your internet connection and try again.",
            )],
            Self::SyncFailed => vec![text(
                "We were unable to sync your Out of Office Reply settings with your email provider. Please check your connection and try again.",
            )],
            Self::SaveFailed => vec![text(
                "Sorry! We were unable to sync your Out of Office Reply settings with your email provider. Please try again later.",
            )],
            Self::Discard => vec![text("Your settings will not be saved.")],
            Self::PastedFile { .. } => vec![vec![
                ("To add an image, go to ".into(), Run::Text),
                ("Gmail settings".into(), Run::Link),
                (" and update your Vacation Responder.".into(), Run::Text),
            ]],
            Self::Nudge { created: true, .. } => vec![text(
                "Would you like to send automatic out of office replies while you are away?",
            )],
            Self::Nudge { days, .. } => {
                // The weekday, month and day, in English: `Monday, June 2`.
                let date = |at: jiff::Timestamp| {
                    at.to_zoned(tz.clone()).strftime("%A, %B %-d").to_string()
                };
                let (start, end) = (
                    days.start.to_zoned(tz.clone()).date(),
                    days.end.to_zoned(tz.clone()).date(),
                );
                let when = if start == end {
                    vec![
                        (
                            "It looks like you may be out of office on ".into(),
                            Run::Text,
                        ),
                        (format!("{}.", date(days.start)), Run::Bold),
                    ]
                } else {
                    vec![
                        (
                            "It looks like you may be out of office from ".into(),
                            Run::Text,
                        ),
                        (date(days.start), Run::Bold),
                        (" to ".into(), Run::Text),
                        (format!("{}.", date(days.end)), Run::Bold),
                    ]
                };
                vec![
                    when,
                    text(
                        "Would you like to send automatic out of office replies while you are away?",
                    ),
                ]
            }
        }
    }

    /// The Gmail settings link of a pasted file's alert.
    pub fn link(&self) -> Option<String> {
        match self {
            Self::PastedFile { email } => Some(crate::settings::gmail_settings_url(email)),
            _ => None,
        }
    }

    /// Paints the alert with `selected` filled over `page`. Returns the action clicked, and
    /// whether the link was.
    pub fn show_modal(
        &mut self,
        ctx: &Context,
        page: Rect,
        selected: usize,
        tz: &TimeZone,
    ) -> (Option<usize>, bool) {
        let paragraphs = self.paragraphs(tz);
        let separator = PARAGRAPH.to_string();
        let mut runs = Vec::new();
        for (index, paragraph) in paragraphs.iter().enumerate() {
            if index > 0 {
                runs.push(AlertRun::Text(&separator));
            }
            for (text, kind) in paragraph {
                runs.push(match kind {
                    Run::Text => AlertRun::Text(text),
                    Run::Bold => AlertRun::Strong(text),
                    Run::Link => AlertRun::Link(text),
                });
            }
        }
        let extra_height = if matches!(self, Self::Nudge { .. }) {
            OPT_OUT_ABOVE + OPT_OUT_HEIGHT + OPT_OUT_BELOW
        } else {
            0.0
        };
        let alert = exo_ui::Alert {
            title: self.title(),
            text: "",
            actions: self.actions(),
            selected,
        };
        let mut toggled = false;
        let current_opt_out = match self {
            Self::Nudge { opt_out, .. } => Some(*opt_out),
            _ => None,
        };
        let (clicked, _) = alert.show_runs_modal_with(
            ctx,
            "out-of-office-alert",
            page,
            &runs,
            extra_height,
            |ui, extra| {
                let Some(checked) = current_opt_out else {
                    return;
                };
                let row = Rect::from_min_size(
                    pos2(extra.left(), extra.top() + OPT_OUT_ABOVE),
                    vec2(extra.width(), OPT_OUT_HEIGHT),
                );
                exo_ui::paint_checkbox(
                    ui.painter(),
                    row.min,
                    checked,
                    exo_ui::Theme::Snow.palette(),
                );
                let label_width = ALERT_TEXT.paint_line(
                    ui.painter(),
                    pos2(row.left() + OPT_OUT_TEXT_LEFT, row.top() - 1.0),
                    OPT_OUT,
                    OPT_OUT_INK,
                    None,
                );
                let hit = Rect::from_min_size(
                    row.min,
                    vec2(OPT_OUT_TEXT_LEFT + label_width, OPT_OUT_HEIGHT - 4.0),
                );
                toggled = ui
                    .interact(hit, ui.id().with("out-of-office-opt-out"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
            },
        );
        if toggled && let Self::Nudge { opt_out, .. } = self {
            *opt_out = !*opt_out;
        }
        match clicked {
            Some(AlertClick::Action(index)) => (Some(index), false),
            Some(AlertClick::Link(_)) => (None, true),
            None => (None, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;

    fn days(start: &str, end: &str) -> OutOfOfficeDays {
        OutOfOfficeDays {
            start: start.parse::<Timestamp>().expect("a time"),
            end: end.parse::<Timestamp>().expect("a time"),
            ids: vec!["ooo".into()],
        }
    }

    #[test]
    fn the_nudge_names_the_days() {
        let tz = TimeZone::get("America/New_York").expect("a zone");
        let one = Alert::Nudge {
            days: days("2026-11-19T05:00:00Z", "2026-11-20T04:59:59.999Z"),
            opt_out: false,
            created: false,
        };
        assert_eq!(
            one.text(&tz),
            "It looks like you may be out of office on Thursday, November 19.\nWould you like to send automatic out of office replies while you are away?"
        );
        let two = Alert::Nudge {
            days: days("2026-11-19T05:00:00Z", "2026-11-21T04:59:59.999Z"),
            opt_out: false,
            created: false,
        };
        let created = Alert::Nudge {
            days: days("2026-11-19T05:00:00Z", "2026-11-21T04:59:59.999Z"),
            opt_out: false,
            created: true,
        };
        assert_eq!(
            created.text(&tz),
            "Would you like to send automatic out of office replies while you are away?"
        );
        assert_eq!(
            two.text(&tz).lines().next(),
            Some(
                "It looks like you may be out of office from Thursday, November 19 to Friday, November 20."
            )
        );
        assert_eq!(two.actions(), ["Set reply", "No thanks"]);
    }

    #[test]
    fn a_pasted_file_links_to_gmails_settings() {
        let alert = Alert::PastedFile {
            email: "maya@brightmoor.co".into(),
        };
        assert_eq!(
            alert.text(&TimeZone::UTC),
            "To add an image, go to Gmail settings and update your Vacation Responder."
        );
        assert_eq!(
            alert.link().as_deref(),
            Some("https://mail.google.com/?shxr=0&authuser=maya%40brightmoor.co#settings/general")
        );
        let plus = Alert::PastedFile {
            email: "alice+work@example.com".into(),
        };
        assert_eq!(
            plus.link().as_deref(),
            Some(
                "https://mail.google.com/?shxr=0&authuser=alice%2Bwork%40example.com#settings/general"
            )
        );
    }
}
