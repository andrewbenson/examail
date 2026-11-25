//! Hosting open drafts on inline conversation cards, the new-message pane, printouts, and modal
//! compose alerts.

use exo_store::{Alias, MessageDetail, ThreadDetail};
use jiff::Timestamp;

use super::{
    Action, AlertThen, Compose, Open, SNIPPET_NAME_ROOM, Surface, contacts, form, reminder,
    snippet_editor, undo_alert,
};

impl Compose {
    /// The drafts open in `thread` as they print, each with the message it hangs under. One in
    /// the new-message pane or popped out isn't in the conversation.
    pub fn printouts(
        &self,
        thread: &ThreadDetail,
        aliases: &[Alias],
        me: &str,
    ) -> Vec<(String, crate::print::Draft)> {
        let shown: Vec<&MessageDetail> = thread
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .collect();
        let elsewhere = [self.pane.as_ref().map(|(id, _)| id.as_str()), self.popped()];
        self.drafts
            .iter()
            .filter(|open| {
                open.thread.as_deref() == Some(thread.id.as_str())
                    && !elsewhere.contains(&Some(open.id.as_str()))
            })
            .filter_map(|open| Some((anchor(open, &shown)?, form::printout(open, aliases, me))))
            .collect()
    }

    /// The drafts of `thread`, each under the message it answers (or under the last message when
    /// that one isn't shown), for [`crate::thread::ThreadView`] to make room for.
    pub fn inline<'a>(
        &'a mut self,
        thread: &'a ThreadDetail,
        aliases: &'a [Alias],
        me: &'a str,
        now: &'a jiff::Zoned,
        page: f32,
    ) -> Inline<'a> {
        let pane = self.pane.as_ref().map(|(id, _)| id.as_str());
        let thread_people = self
            .thread_people
            .as_ref()
            .filter(|(id, _)| *id == thread.id.as_str())
            .map(|(_, index)| index);
        Inline {
            drafts: &mut self.drafts,
            focus: &mut self.focus,
            pressed_inside: &mut self.pressed_inside,
            pane,
            popped: self.popped.as_deref(),
            alert: self.alert.is_some() || self.palette_open,
            thread,
            aliases,
            me,
            book: self.book.as_ref().map(|(_, index)| index.as_ref()),
            thread_people,
            now,
            send_and_done: self.send_and_done,
            availability_tip: self.availability_tip,
            max_height: page - form::REPLY_MAX_BELOW_PAGE,
        }
    }
}

/// The drafts of one thread, as the thread view lays them out.
pub(crate) struct Inline<'a> {
    drafts: &'a mut Vec<Open>,
    focus: &'a mut Option<String>,
    pressed_inside: &'a mut bool,
    pane: Option<&'a str>,
    /// The popped-out draft, of this thread or another.
    popped: Option<&'a str>,
    /// An alert, or the command palette, holds the keyboard.
    alert: bool,
    thread: &'a ThreadDetail,
    aliases: &'a [Alias],
    me: &'a str,
    book: Option<&'a contacts::Index>,
    thread_people: Option<&'a contacts::Index>,
    now: &'a jiff::Zoned,
    send_and_done: bool,
    availability_tip: bool,
    /// The inline forms' maximum height.
    max_height: f32,
}

impl<'a> Inline<'a> {
    /// Whether a draft is popped out, for the thread to make room for it.
    pub fn popped_out(&self) -> bool {
        self.popped.is_some()
    }

    fn shown(&self) -> Vec<&MessageDetail> {
        self.thread
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .collect()
    }

    /// Whether the wheel at `pointer`, moving the content `delta` down, scrolls a form's body or
    /// header rather than the thread ([`form::BodyScroll::takes_wheel`]).
    pub fn takes_wheel(&self, pointer: egui::Pos2, delta: f32) -> bool {
        self.drafts.iter().any(|open| {
            open.thread.as_deref() == Some(self.thread.id.as_str())
                && [&open.body.scroll, &open.header.scroll]
                    .iter()
                    .any(|scroll| scroll.takes_wheel(pointer, delta))
        })
    }

    /// The messages that have a draft under them.
    pub fn messages(&self) -> Vec<String> {
        let shown = self.shown();
        self.drafts
            .iter()
            .filter(|open| {
                open.thread.as_deref() == Some(self.thread.id.as_str())
                    && self.pane != Some(open.id.as_str())
            })
            .filter_map(|open| anchor(open, &shown))
            .collect()
    }

    /// The form context for the draft at `at`, in a thread whose message column spans `column`,
    /// in the message card `card`. It borrows nothing of `self`.
    fn context(
        &self,
        at: usize,
        column: egui::Rangef,
        card: Option<egui::Rect>,
    ) -> form::Context<'a> {
        form::Context {
            aliases: self.aliases,
            me: self.me,
            surface: Surface::Inline,
            focused: !self.alert && self.focus.as_deref() == Some(self.drafts[at].id.as_str()),
            book: self.book,
            thread_people: self.thread_people,
            pin: column,
            drop: card,
            now: self.now,
            thread_reminder: set_reminder(self.thread.reminder.as_ref()),
            send_and_done: self.send_and_done,
            max_height: self.max_height,
            availability_tip: self.availability_tip,
        }
    }

    fn draft_under(&self, message: &str) -> Option<usize> {
        let shown = self.shown();
        let thread = self.thread.id.as_str();
        self.drafts.iter().position(|open| {
            open.thread.as_deref() == Some(thread)
                && self.pane != Some(open.id.as_str())
                && anchor(open, &shown).as_deref() == Some(message)
        })
    }

    /// Whether the draft under `message` shows its form there rather than popped out.
    pub fn has_form(&self, message: &str) -> bool {
        self.draft_under(message)
            .is_some_and(|at| self.popped != Some(self.drafts[at].id.as_str()))
    }

    /// The height of the draft under `message` (separator included) at `width`.
    pub fn height(&mut self, ui: &egui::Ui, message: &str, width: f32) -> f32 {
        let Some(at) = self.draft_under(message) else {
            return 0.0;
        };
        if self.popped == Some(self.drafts[at].id.as_str()) {
            return form::SEPARATOR + form::PLACEHOLDER;
        }
        let cx = self.context(at, egui::Rangef::EVERYTHING, None);
        form::SEPARATOR + form::height(ui, &mut self.drafts[at], width, &cx)
    }

    /// Draws the draft under `message` in `form` (the top of its separator, and its width) inside
    /// the message's `card`, in a message column spanning `column`. Returns what the draft asked
    /// for.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        message: &str,
        form: egui::Rect,
        card: egui::Rect,
        column: egui::Rangef,
    ) -> Vec<Action> {
        let Some(at) = self.draft_under(message) else {
            return Vec::new();
        };
        let (left, top, width) = (form.left(), form.top(), form.width());
        form::separator(ui, left, top, width);
        let cx = self.context(at, column, Some(card));
        if self.popped == Some(self.drafts[at].id.as_str()) {
            return form::placeholder_header(
                ui,
                &self.drafts[at],
                left,
                top + form::SEPARATOR,
                width,
                &cx,
            );
        }
        let rect = egui::Rect::from_min_size(
            egui::pos2(left, top + form::SEPARATOR),
            egui::vec2(width, 0.0),
        );
        let response = form::show(ui, &mut self.drafts[at], rect, &cx);
        if response.clicked_inside {
            *self.pressed_inside = true;
            *self.focus = Some(self.drafts[at].id.clone());
        }
        response.actions
    }
}

/// A thread's reminder as the Remind button shows it: when, and whether it's `regardless`, while
/// it's set.
pub(super) fn set_reminder(
    reminder: Option<&exo_store::ReminderStatus>,
) -> Option<(Timestamp, bool)> {
    match reminder? {
        exo_store::ReminderStatus::Set { at, keep_on_reply } => Some((*at, *keep_on_reply)),
        _ => None,
    }
}

/// The message a draft hangs under: the one it answers, else the last.
fn anchor(open: &Open, shown: &[&MessageDetail]) -> Option<String> {
    let answers = open
        .record
        .in_reply_to
        .as_deref()
        .filter(|id| shown.iter().any(|message| message.id == *id));
    answers
        .map(str::to_owned)
        .or_else(|| shown.last().map(|message| message.id.clone()))
}

impl crate::Mail {
    /// The text of the alert compose is showing, if any. For tests.
    pub fn compose_alert(&self) -> Option<&str> {
        self.compose.alert.as_ref().map(|alert| alert.text.as_str())
    }

    /// Draws the alert compose is showing, if any.
    pub(crate) fn show_compose_alert(&mut self, ui: &mut egui::Ui, page: egui::Rect) {
        let Some(alert) = &self.compose.alert else {
            return;
        };
        let title = alert.title.unwrap_or_default();
        let shown = exo_ui::Alert {
            title,
            text: &alert.text,
            actions: &alert.actions,
            selected: alert.selected,
        };
        let (clicked, _) = exo_ui::Alert::modal(ui.ctx(), "compose-alert", page, |ui| {
            if alert.key {
                shown.show_with_key(
                    ui,
                    page,
                    reminder::HINT_BEFORE,
                    reminder::HINT_KEY,
                    reminder::HINT_AFTER,
                )
            } else if matches!(alert.then, AlertThen::UndoSend(_)) {
                shown.show_with_confirm_key(
                    ui,
                    page,
                    undo_alert::BEFORE,
                    undo_alert::KEY,
                    undo_alert::AFTER,
                )
            } else {
                shown.show(ui, page)
            }
        });
        if let Some(index) = clicked {
            self.alert_action(index);
        }
    }

    /// Leaves the new-message pane for where it was opened from, saving or dropping its draft.
    pub(super) fn leave_pane_to_origin(&mut self) {
        if self.leaving_snippet_asks() {
            return;
        }
        let origin = self
            .compose
            .pane
            .as_ref()
            .and_then(|(_, from)| from.clone());
        self.leave_pane();
        if let Some(thread) = origin {
            self.open_thread(thread);
        }
    }

    /// Draws the new-message pane in `rect` (the content pane).
    pub(crate) fn show_compose_pane(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        rem: f32,
        window_width: f32,
        now: f64,
    ) {
        let Some(id) = self.compose.pane().map(str::to_owned) else {
            return;
        };
        let aliases = self.snapshot.aliases.clone();
        let me = self.account.email().to_owned();
        let focused = self.compose.focus.as_deref() == Some(id.as_str())
            && self.compose.alert.is_none()
            && !self.compose.palette_open;
        let Some(open) = self.compose.drafts.iter_mut().find(|open| open.id == id) else {
            self.compose.pane = None;
            return;
        };
        let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
        let popped_out = self.compose.popped.is_some();
        let now_zoned = &self.now;
        let send_and_done = self.compose.send_and_done;
        let mut cx = form::Context {
            aliases: &aliases,
            me: &me,
            surface: Surface::Pane,
            focused,
            book,
            thread_people: None,
            pin: egui::Rangef::EVERYTHING,
            drop: None,
            now: now_zoned,
            thread_reminder: None,
            send_and_done,
            max_height: rect.height() - form::MAX_BELOW_PAGE,
            availability_tip: self.compose.availability_tip,
        };
        let width = crate::thread::content_width(rect, rem, window_width, popped_out);
        let height = form::height(ui, open, width, &cx);
        // A snippet's pane is titled for the snippet, with its name above the card.
        let (title, above) = if open.is_snippet() {
            (snippet_editor::pane_title(open), SNIPPET_NAME_ROOM)
        } else {
            ("New Message", 0.0)
        };
        let mut clicked = false;
        // The pop-out hint shows for a message started from a conversation, until its
        // recipients or anything else in it change.
        let pop_out_hint = self
            .compose
            .pane
            .as_ref()
            .is_some_and(|(_, from)| from.is_some())
            && !open.is_snippet()
            && open.is_unmodified();
        let pane = crate::thread::PaneCard {
            title,
            height,
            above,
            pop_out_hint,
            read_only: false,
        };
        let actions = crate::thread::new_message_pane(
            ui,
            rect,
            rem,
            window_width,
            popped_out,
            &pane,
            |ui, content, column, card| {
                cx.pin = column;
                cx.drop = Some(card);
                let response = form::show(ui, open, content, &cx);
                clicked = response.clicked_inside;
                response.actions
            },
        );
        if clicked {
            self.compose.pressed_inside = true;
            self.compose.focus = Some(id);
        }
        for action in actions {
            match action {
                crate::thread::Action::Back => self.leave_pane_to_origin(),
                // A snippet's arrows go to the next snippet, read-only.
                crate::thread::Action::Navigate(delta) if self.pane_snippet().is_some() => {
                    let step = move |mail: &mut crate::Mail| {
                        mail.close_snippet();
                        mail.snippet_step(delta);
                    };
                    if self.snippet_unsaved() {
                        self.ask_leaving_snippet(Box::new(step))
                    } else {
                        step(self)
                    }
                }
                crate::thread::Action::Navigate(delta) => {
                    self.leave_pane();
                    self.inbox.move_cursor(delta);
                    if let Some(next) = self.inbox.cursor().map(|thread| thread.id.clone()) {
                        self.open_thread(next);
                    }
                }
                crate::thread::Action::Compose(action) => self.compose_run(action, now),
                _ => {}
            }
        }
    }
}
