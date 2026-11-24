//! Modal alerts shown over the split inbox dialogs: definitions, copy, rendering, and action
//! handlers.

use egui::{Context, Rect};
use exo_commands::{Commands, Toast};
use exo_store::{ListId, Settings, SplitEntry, SplitInbox};
use exo_ui::{Alert, PARAGRAPH};

use super::form::{Field, Form};
use super::hide_empty;
use super::mutations::{TOO_LONG, delete, toggle_important_other};
use super::{Back, Event, Outcome, Screen, SplitDialogs};
use crate::account::Account;
use crate::settings::{Action, Dialog};
use crate::splits;

/// An alert over the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AlertKind {
    /// A definition that can't define a split (`Sorry!`).
    Invalid(String),
    /// `Discard changes?`.
    Discard,
    /// `Delete Split`, for the split being edited.
    Delete,
    /// `This makes your Split Inbox queries too long.`
    TooLong,
    /// `Almost there…`, to turn Important and Other off.
    TurnOffImportantOther,
    /// `Almost there…`, when a template's name is already a split's.
    NameTaken,
    /// `Delete Split`, from a template's settings.
    DeleteTemplateSplit,
    /// `Hide empty Split Inboxes for all Splits?`, from the nudge's `Try it out`.
    HideAll,
}

impl AlertKind {
    pub(super) fn title(&self) -> &'static str {
        match self {
            Self::Invalid(_) | Self::TooLong => "Sorry!",
            Self::Discard => "Discard changes?",
            Self::Delete | Self::DeleteTemplateSplit => "Delete Split",
            Self::TurnOffImportantOther | Self::NameTaken => "Almost there…",
            Self::HideAll => "Hide empty Split Inboxes for all Splits?",
        }
    }

    pub(super) fn actions(&self) -> &'static [&'static str] {
        match self {
            Self::Invalid(_) => &["OK"],
            Self::TooLong => &["Ok"],
            Self::Discard => &["Discard", "Cancel"],
            Self::Delete | Self::DeleteTemplateSplit => &["Delete", "Cancel"],
            Self::TurnOffImportantOther => &["Confirm", "Cancel"],
            Self::NameTaken => &["OK"],
            Self::HideAll => &["Turn on for all splits", "Cancel"],
        }
    }
}

impl SplitDialogs {
    pub(super) fn alert_action<A: Clone + 'static>(
        &mut self,
        index: usize,
        account: &mut Account,
        commands: &mut Commands<A>,
        wrap: &impl Fn(Action) -> A,
    ) -> Outcome {
        let Some((kind, _)) = self.alert.take() else {
            return Outcome::default();
        };
        match (kind, index) {
            (AlertKind::Invalid(_), _) => {
                if let Screen::Form(form) = &mut self.screen {
                    form.query_error = true;
                    if form.name_error.is_none() {
                        form.focus = Some(Field::Query);
                    }
                }
                Outcome::default()
            }
            (AlertKind::Discard, 0) => {
                let back = match &self.screen {
                    Screen::Form(form) => form.back,
                    Screen::Template(_, back) => *back,
                    _ => Back::List,
                };
                self.leave_form(back)
            }
            (AlertKind::DeleteTemplateSplit, 0) => {
                let Screen::Template(form, back) = &self.screen else {
                    return Outcome::default();
                };
                let back = *back;
                let Some(split) = form
                    .split
                    .as_ref()
                    .and_then(|id| account.settings().split(id))
                    .cloned()
                else {
                    return Outcome::default();
                };
                delete(account, commands, wrap, &split);
                let outcome = self.leave_form(back);
                Outcome {
                    show: Some(splits::home(account.settings())),
                    ..outcome
                }
            }
            (AlertKind::TurnOffImportantOther, 0) => {
                let show = Some(toggle_important_other(account, commands, wrap));
                // From the template's settings, it goes back where it came from.
                let back = match &self.screen {
                    Screen::Template(_, back) => self.leave_form(*back),
                    _ => Outcome::default(),
                };
                Outcome { show, ..back }
            }
            (AlertKind::HideAll, 0) => {
                // Hides every active split except Important (or Inbox), and Other, that doesn't
                // hide when empty yet. Undo shows those again.
                let settings = account.settings();
                let mut changed: Vec<ListId> = hide_empty::rows(settings)
                    .iter()
                    .map(SplitEntry::list)
                    .filter(|list| !hide_empty::hides_when_empty(settings, list))
                    .collect();
                if !settings.hides_other_when_empty() && !changed.contains(&ListId::Other) {
                    changed.push(ListId::Other);
                }
                account.update_settings(|settings| {
                    for list in &changed {
                        hide_empty::set_hides_when_empty(settings, list, true);
                    }
                });
                let undo = wrap(Action::Split(Event::UndoHideAll(changed)));
                commands.toast(
                    Toast::new("Split Inboxes updated.")
                        .divided()
                        .undo(undo)
                        .action(
                            "Settings",
                            None,
                            wrap(Action::Open(Dialog::HideEmptySplits)),
                        ),
                );
                Outcome::default()
            }
            (AlertKind::Delete, 0) => {
                let Screen::Form(Form {
                    editing: Some(id), ..
                }) = &self.screen
                else {
                    return Outcome::default();
                };
                let id = id.clone();
                let Some(split) = account.settings().split(&id).cloned() else {
                    return Outcome::default();
                };
                delete(account, commands, wrap, &split);
                self.screen = Screen::List;
                Outcome {
                    show: Some(splits::home(account.settings())),
                    close: false,
                }
            }
            _ => Outcome::default(),
        }
    }

    /// Draws the active alert modal over `area`, surrendering keyboard focus from any underlying
    /// form field first.
    pub(super) fn show_alert(
        &self,
        ctx: &Context,
        area: Rect,
        settings: &Settings,
        kind: &AlertKind,
        selected: usize,
    ) -> Option<Event> {
        // The alert takes the keyboard from the form's fields.
        ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
        });
        let text = match kind {
            AlertKind::Invalid(text) => text.clone(),
            AlertKind::Discard => "Your changes will not be saved.".to_owned(),
            AlertKind::TooLong => TOO_LONG.to_owned(),
            AlertKind::Delete => {
                let name = match &self.screen {
                    Screen::Form(form) => form
                        .editing
                        .as_ref()
                        .and_then(|id| settings.split(id))
                        .map_or("", SplitInbox::name),
                    _ => "",
                };
                format!(
                    "Would you like to delete the split {name}?{PARAGRAPH}Please note: you cannot undo this action."
                )
            }
            AlertKind::TurnOffImportantOther => format!(
                "Conversations from Important and Other will appear in Inbox instead.{PARAGRAPH}You can always turn Important and Other back on from the Split Inbox Library."
            ),
            AlertKind::NameTaken => "A Split Inbox with this name already exists. To create this one, please rename or remove the existing Split Inbox.".to_owned(),
            AlertKind::DeleteTemplateSplit => {
                let name = match &self.screen {
                    Screen::Template(form, _) => form
                        .split
                        .as_ref()
                        .and_then(|id| settings.split(id))
                        .map_or("", SplitInbox::name),
                    _ => "",
                };
                format!(
                    "Would you like to delete the split {name}?{PARAGRAPH}You can always turn this split back on from the Split Inbox Library."
                )
            }
            AlertKind::HideAll => {
                let home = if settings.unified_inbox {
                    "Inbox"
                } else {
                    "Important"
                };
                // A string with an empty line, not paragraphs.
                format!(
                    "This will hide empty Split Inboxes for all active splits except {home}.\n\nYou can update this anytime from the Split Inbox Settings."
                )
            }
        };
        let alert = Alert {
            title: kind.title(),
            text: &text,
            actions: kind.actions(),
            selected,
        };
        let (clicked, backdrop) = alert.show_modal(ctx, "split-inbox-alert", area);
        clicked
            .map(Event::AlertAction)
            .or_else(|| backdrop.then_some(Event::AlertDismiss))
    }
}
