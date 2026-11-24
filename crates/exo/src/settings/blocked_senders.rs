//! The Blocked Senders dialog lists the addresses and domains the account blocks, each with an
//! Unblock behind an `Unblock` alert. Unblocking takes the sender off the list and deletes the
//! Gmail filter its block made. What the filter trashed stays in Trash.

use egui::{Context, CornerRadius, Rect, Sense, Ui, pos2};
use exo_commands::{Command, Commands, Region};
use exo_ui::{
    Alert, Dialog, Family, Font, TextStyle, dialog_highlight, paint_dialog_footer, palette,
};

/// The subheader: 12px/20px, 24px down (and `top: 3px`).
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const SUBHEADER_TOP: f32 = 27.0;
/// A sender and its Unblock: 12px/20px bold, 7px into a 32px row.
const SENDER: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 20.0);
const ROW: f32 = 32.0;
const ROW_TEXT: f32 = 7.0;
/// The list's top: 21px under the subheader's last line, so 68px down under a one-line subheader
/// and 88px under a two-line one.
const LIST_GAP: f32 = 21.0;
/// The focused row's fill, 16px into the dialog's padding, 4px corners.
const HIGHLIGHT_INSET: f32 = 16.0;
/// The footer's line sits 22px under the list, or 19px under the subheader when there's no list.
/// The body ends 18px below its top.
const FOOTER_GAP: f32 = 22.0;
const EMPTY_FOOTER_GAP: f32 = 19.0;
const FOOTER_BELOW: f32 = 18.0;
/// `Unblock`: `#000` at `opacity: .5` on the focused row (`:focus`), hidden on the others.
const UNBLOCK_FOCUSED: f32 = 0.5;

/// The dialog's row focus and its alert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    focus: usize,
    /// `Unblock` is being asked about this sender, with this action selected.
    alert: Option<(String, usize)>,
}

/// Something done in the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A row was clicked, which asks about unblocking it.
    Row(usize),
    /// ↓/J/Tab (1) or ↑/K/⇧Tab (-1).
    Move(isize),
    /// Enter, which acts on the focused row.
    Activate,
    /// The alert's action, by index (`Unblock` first).
    AlertAction(usize),
    /// Enter or Space in the alert, which runs its selected action.
    AlertConfirm,
    /// ← / → in the alert.
    AlertCycle(isize),
    /// Escape or a click outside the alert.
    AlertDismiss,
}

impl State {
    /// Whether an alert is up, in which case it has the keyboard.
    pub fn alert_open(&self) -> bool {
        self.alert.is_some()
    }

    /// Back to the first row, as the dialog opens.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Registers the dialog's keyboard bindings in `Region::Dialog` (and its alert's keys).
    pub fn register_keys<A: Clone + 'static>(
        &self,
        commands: &mut Commands<A>,
        wrap: &impl Fn(super::Action) -> A,
    ) {
        const DIALOG: &[Region] = &[Region::Dialog];
        let blocked = |id: &'static str, event: Event, keys: &[&'static str]| {
            Command::shortcut(id, wrap(super::Action::Blocked(event)))
                .keys(keys.iter().copied())
                .regions(DIALOG)
        };
        commands.add(blocked(
            "BLOCKED_DOWN",
            Event::Move(1),
            &["down", "j", "tab", "ctralt+j", "ctralt+n"],
        ));
        commands.add(blocked(
            "BLOCKED_UP",
            Event::Move(-1),
            &["up", "k", "shift+tab", "ctralt+k", "ctralt+p"],
        ));
        commands.add(blocked("BLOCKED_ENTER", Event::Activate, &["enter"]));
        crate::notice::register_alert_keys(
            commands,
            wrap(super::Action::Blocked(Event::AlertConfirm)),
            wrap(super::Action::Blocked(Event::AlertDismiss)),
            |step| wrap(super::Action::Blocked(Event::AlertCycle(step))),
        );
    }

    /// Applies `event` over `senders`, and returns the sender to unblock once the alert says so.
    pub fn handle(&mut self, event: Event, senders: &[&str]) -> Option<String> {
        match event {
            Event::Move(delta) if !senders.is_empty() => {
                self.focus =
                    (self.focus as isize + delta).rem_euclid(senders.len() as isize) as usize
            }
            Event::Move(_) => {}
            Event::Activate => return self.handle(Event::Row(self.focus), senders),
            Event::Row(row) => {
                if let Some(sender) = senders.get(row) {
                    self.focus = row;
                    self.alert = Some(((*sender).to_owned(), 0));
                }
            }
            Event::AlertAction(index) => {
                let (sender, _) = self.alert.take()?;
                return (index == 0).then_some(sender);
            }
            Event::AlertConfirm => {
                let (_, selected) = self.alert.as_ref()?;
                return self.handle(Event::AlertAction(*selected), senders);
            }
            Event::AlertCycle(step) => {
                if let Some((_, selected)) = &mut self.alert {
                    *selected = Alert::move_selection(*selected, 2, step, true);
                }
            }
            Event::AlertDismiss => self.alert = None,
        }
        None
    }

    /// Keeps the focus on a row after one goes.
    pub fn clamp(&mut self, rows: usize) {
        self.focus = self.focus.min(rows.saturating_sub(1));
    }
}

/// The blocked senders the dialog lists. They're the setting's, in order, each once whatever its
/// case.
pub fn senders(blocked: &[String]) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    blocked
        .iter()
        .filter(|sender| seen.insert(sender.to_lowercase()))
        .map(String::as_str)
        .collect()
}

/// Draws the dialog over `area` while `open`, listing `senders`, and its alert.
pub fn show(
    ctx: &Context,
    area: Rect,
    open: bool,
    state: &State,
    senders: &[&str],
) -> exo_ui::DialogResponse<Option<Event>> {
    let subheader = subheader(senders);
    let lines = TEXT.wrap(ctx, subheader, exo_ui::DIALOG_WIDTH - 64.0).len();
    let list_top = SUBHEADER_TOP + lines as f32 * TEXT.line_height + LIST_GAP;
    // With no list, the footer sits 19px under the subheader.
    let footer = if senders.is_empty() {
        list_top - LIST_GAP + EMPTY_FOOTER_GAP
    } else {
        list_top + senders.len() as f32 * ROW + FOOTER_GAP
    };
    let mut response = Dialog::new("blocked-senders", "Blocked Senders", footer + FOOTER_BELOW)
        .show(ctx, area, open, |ui, body| {
            draw(ui, body, state, senders, list_top, footer)
        });
    if let (Some(event), Some((sender, selected))) = (&mut response.inner, &state.alert) {
        let text = format!("Remove the block on {sender}?");
        let (clicked, backdrop) = (Alert {
            title: "Unblock",
            text: &text,
            actions: &["Unblock", "Cancel"],
            selected: *selected,
        })
        .show_modal(ctx, "blocked-senders-alert", area);
        *event = clicked
            .map(Event::AlertAction)
            .or_else(|| backdrop.then_some(Event::AlertDismiss))
            .or(event.take());
        response.close = false;
    }
    response
}

/// `There are no blocked senders.`, or what blocking does.
fn subheader(senders: &[&str]) -> &'static str {
    if senders.is_empty() {
        "There are no blocked senders."
    } else {
        "All mail from the senders below will be moved to Trash. Messages in Trash for more than 30 days will be deleted automatically."
    }
}

fn draw(
    ui: &mut Ui,
    body: Rect,
    state: &State,
    senders: &[&str],
    list_top: f32,
    footer: f32,
) -> Option<Event> {
    let tokens = palette(ui.ctx());
    let painter = ui.painter().clone();
    for (i, line) in TEXT
        .wrap(ui.ctx(), subheader(senders), body.width())
        .iter()
        .enumerate()
    {
        // The subheader: `rgba(0, 0, 0, .85)`.
        TEXT.paint_line(
            &painter,
            pos2(
                body.left(),
                body.top() + SUBHEADER_TOP + i as f32 * TEXT.line_height,
            ),
            line,
            tokens.ink(0.85, 0.85),
            None,
        );
    }

    let mut event = None;
    for (i, sender) in senders.iter().enumerate() {
        let top = body.top() + list_top + i as f32 * ROW;
        let rect = Rect::from_min_max(
            pos2(body.left() - HIGHLIGHT_INSET, top),
            pos2(body.right() + HIGHLIGHT_INSET, top + ROW),
        );
        let response = ui
            .interact(
                rect,
                exo_ui::id(ui.ctx(), ("blocked-sender", i)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        // Hovering a row moves the focus to it.
        if response.hovered() && state.focus != i && state.alert.is_none() {
            event = Some(Event::Move(i as isize - state.focus as isize));
        }
        if response.clicked() {
            event = Some(Event::Row(i));
        }
        let focused = state.focus == i;
        if focused {
            painter.rect_filled(rect, CornerRadius::same(4), dialog_highlight(ui.ctx()));
        }
        // The sender: `rgba(0, 0, 0, .85)`.
        SENDER.paint_line(
            &painter,
            pos2(body.left(), top + ROW_TEXT),
            sender,
            tokens.ink(0.85, 0.85),
            Some(body.width() - 60.0),
        );
        if focused {
            let width = SENDER.width(ui.ctx(), "Unblock");
            SENDER.paint_line(
                &painter,
                pos2(body.right() - width, top + ROW_TEXT),
                "Unblock",
                tokens.ink(UNBLOCK_FOCUSED, UNBLOCK_FOCUSED),
                None,
            );
        }
    }

    // `To block a sender, open a message from them, hit ⌘ K → Block.`
    paint_dialog_footer(
        ui,
        body.left(),
        body.top() + footer,
        "To block a sender, open a message from them, hit",
        exo_ui::KEYCAP_GAP,
        1.0,
        " → Block.",
    );
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_show_once_whatever_their_case() {
        let blocked = [
            "spam@x.com".to_owned(),
            "Zed@y.com".to_owned(),
            "SPAM@x.com".to_owned(),
        ];
        assert_eq!(senders(&blocked), ["spam@x.com", "Zed@y.com"]);
    }

    #[test]
    fn unblocking_asks_first() {
        let senders = ["a@x.com", "b@y.com"];
        let mut state = State::default();
        state.handle(Event::Move(-1), &senders);
        assert_eq!(
            state.handle(Event::Activate, &senders),
            None,
            "the alert first"
        );
        assert!(state.alert_open());
        assert_eq!(
            state.handle(Event::AlertAction(1), &senders),
            None,
            "Cancel"
        );
        state.handle(Event::Row(0), &senders);
        assert_eq!(
            state.handle(Event::AlertAction(0), &senders).as_deref(),
            Some("a@x.com")
        );
    }

    #[test]
    fn tab_and_arrows_wrap_between_unblock_and_cancel() {
        let senders = ["a@x.com"];
        let mut state = State::default();
        state.handle(Event::Activate, &senders);
        // Tab (-1) from 0 wraps to Cancel (1), and Enter confirms Cancel rather than Unblock.
        state.handle(Event::AlertCycle(-1), &senders);
        assert_eq!(state.handle(Event::AlertConfirm, &senders), None);
        assert!(!state.alert_open());
        // ArrowLeft (+1) selects Cancel, ArrowRight (-1) moves back to Unblock.
        state.handle(Event::Activate, &senders);
        state.handle(Event::AlertCycle(1), &senders);
        state.handle(Event::AlertCycle(-1), &senders);
        assert_eq!(
            state.handle(Event::AlertConfirm, &senders).as_deref(),
            Some("a@x.com")
        );
    }
}
