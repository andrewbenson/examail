//! The New and Edit Split Inbox form: state, validation, save/toggle transitions, and rendering.

use egui::{Rect, Sense, Ui, pos2, vec2};
use exo_commands::Commands;
use exo_store::{
    Definitions, ListId, Settings, SplitChanges, SplitInbox, SplitQuery, clean_whitespace,
    name_error,
};
use exo_ui::{
    ActionKind, DIALOG_ACTION_HEIGHT, DialogAction, DialogInput, Family, Font, TextStyle,
    paint_checkbox, palette,
};

use super::alert::AlertKind;
use super::mutations::{add, definitions, replace, toggle};
use super::{
    BODY, BODY_BOLD, Back, CONTENT_WIDTH, Event, NUDGE, Outcome, SMALL, Screen, SplitDialogs,
};
use crate::account::Account;
use crate::settings::Action;

/// The New/Edit Split Inbox form's state.
#[derive(Debug, Default)]
pub(super) struct Form {
    /// The split being edited, by id, or `None` for a new one.
    pub(super) editing: Option<String>,
    /// Where closing goes.
    pub(super) back: Back,
    pub(super) name: String,
    pub(super) query: String,
    pub(super) leave: bool,
    pub(super) hide: bool,
    /// The name's inline error.
    pub(super) name_error: Option<&'static str>,
    /// Whether the query is marked wrong since its alert.
    pub(super) query_error: bool,
    /// The field to focus on the next frame.
    pub(super) focus: Option<Field>,
    /// Whether Edit Split Inbox just opened, so the name has the keyboard with nothing selected
    /// and the caret after the name.
    pub(super) opening: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Name,
    Query,
}

impl Form {
    /// A blank New Split Inbox form focused on its name field.
    pub(super) fn new(back: Back) -> Self {
        Self {
            back,
            focus: Some(Field::Name),
            ..Self::default()
        }
    }

    /// An Edit Split Inbox form seeded with `split`'s fields, going back to the list on close.
    pub(super) fn edit(split: &SplitInbox) -> Self {
        Self {
            editing: Some(split.id.clone()),
            back: Back::List,
            name: split.name().to_owned(),
            query: split.query().trim().to_owned(),
            leave: split.leave_threads_in_important_other,
            hide: split.hides_when_empty(),
            focus: Some(Field::Name),
            opening: true,
            ..Self::default()
        }
    }

    /// What saving the form changes in `old`, given its cleaned name
    /// and definition.
    fn changes(&self, old: &SplitInbox, name: String, query: String) -> SplitChanges {
        SplitChanges {
            name: Some(name),
            query: (clean_whitespace(old.query()) != query).then_some(query),
            leave_threads_in_important_other: Some(self.leave),
            hide_split_when_empty: Some(self.hide),
            ..SplitChanges::default()
        }
    }

    /// Whether the form differs from what's saved (for a new one, whether anything's typed).
    pub(super) fn has_changes(&self, settings: &Settings) -> bool {
        let (name, query) = (clean_whitespace(&self.name), clean_whitespace(&self.query));
        match self.editing.as_ref().and_then(|id| settings.split(id)) {
            None if self.editing.is_some() => false,
            None => !name.is_empty() || !query.is_empty(),
            Some(split) => {
                name != clean_whitespace(split.name())
                    || query != clean_whitespace(split.query())
                    || self.leave != split.leave_threads_in_important_other
                    || self.hide != split.hides_when_empty()
            }
        }
    }

    /// Whether the name or the definition is missing, which disables Save.
    fn incomplete(&self) -> bool {
        clean_whitespace(&self.name).is_empty() || clean_whitespace(&self.query).is_empty()
    }

    /// The form's body height: its fields, help, checkboxes and buttons.
    pub(super) fn height(&self) -> f32 {
        let error = if self.name_error.is_some() { 36.0 } else { 0.0 };
        // Two fields (56 + 20), the help (16 + 80), the checkboxes (16 + 48), and the buttons
        // (44 + 28 + 7).
        2.0 * 76.0 + error + 96.0 + 64.0 + 79.0
    }

    /// Draws the form in `body` and returns what was clicked.
    pub(super) fn show(&mut self, ui: &mut Ui, body: Rect, settings: &Settings) -> Option<Event> {
        let ctx = ui.ctx().clone();
        let tokens = palette(&ctx);
        let left = body.left();
        let mut event = None;
        let input = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
        let opening = std::mem::take(&mut self.opening);
        let mut y = body.top();
        let field = |ui: &mut Ui,
                     y: &mut f32,
                     label: &str,
                     which: Field,
                     focus: &mut Option<Field>,
                     text: &mut String,
                     error: bool,
                     placeholder: &str| {
            // The subheader: `padding-top: 24px`, 12px/20px 600 at `rgba(0, 0, 0, .85)`,
            // `margin-bottom: 12px`.
            BODY_BOLD.paint_line(
                ui.painter(),
                pos2(left, *y + 24.0 + NUDGE),
                label,
                tokens.ink(0.85, 0.85),
                None,
            );
            *y += 24.0 + 20.0 + 12.0;
            let line = Rect::from_min_size(pos2(left, *y + NUDGE), vec2(CONTENT_WIDTH, 20.0));
            let id = exo_ui::id(&ctx, ("split-inbox-field", label));
            let before = text.clone();
            let response = DialogInput::new(input, placeholder)
                .error(error)
                .show(ui, id, text, line);
            if focus.take_if(|wanted| *wanted == which).is_some() {
                // Focus selects the name (`select()`), and puts the caret after the query.
                let count = text.chars().count();
                let from = if which == Field::Name && !opening {
                    0
                } else {
                    count
                };
                exo_ui::focus_field(
                    ui.ctx(),
                    id,
                    egui::text::CCursorRange::two(
                        egui::text::CCursor::new(from),
                        egui::text::CCursor::new(count),
                    ),
                );
            }
            *y += 20.0;
            (response, *text != before)
        };
        let (_, name_changed) = field(
            ui,
            &mut y,
            "Name",
            Field::Name,
            &mut self.focus,
            &mut self.name,
            self.name_error.is_some(),
            "Enter a Split Inbox name",
        );
        if name_changed {
            self.name_error = None;
        }
        if let Some(error) = self.name_error {
            // The error: `margin-top: 16px`, 12px/20px critical.
            BODY.paint_line(
                ui.painter(),
                pos2(left, y + 16.0 + NUDGE),
                error,
                tokens.text_critical_default,
                None,
            );
            y += 36.0;
        }
        let (_, query_changed) = field(
            ui,
            &mut y,
            "Definition",
            Field::Query,
            &mut self.focus,
            &mut self.query,
            self.query_error,
            "Enter a search query that every conversation in this Split Inbox must match",
        );
        if query_changed {
            self.query_error = false;
        }

        // The help: `margin-top: 16px`, its paragraph and 20px-indented list at
        // `rgba(0, 0, 0, .4)`.
        y += 16.0;
        let painter = ui.painter().clone();
        let help = tokens.ink(0.4, 0.4);
        BODY.paint_line(
            &painter,
            pos2(left, y + NUDGE),
            "Write the search query based on the sender, recipients, subject line, or label. For example:",
            help,
            None,
        );
        for (i, example) in [
            "from:alice@example.com OR to:feedback@example.com",
            "subject:\"survey results\"",
            "label:\"Team Updates\"",
        ]
        .into_iter()
        .enumerate()
        {
            let line_top = y + 20.0 * (i as f32 + 1.0) + NUDGE;
            // Chromium's `disc` marker at 12px: 4px across, 8px in, mid-line.
            painter.circle_filled(pos2(left + 8.0, line_top + 10.0), 2.0, help);
            BODY.paint_line(&painter, pos2(left + 20.0, line_top), example, help, None);
        }
        y += 80.0;

        // The checkboxes: `margin-top: 16px`, each `margin-top: 8px` and 16px tall.
        y += 16.0;
        let leave_label = if settings.unified_inbox {
            "Also show conversations that match this Split Inbox in Inbox"
        } else {
            "Also show conversations that match this Split Inbox in Important or Other"
        };
        for (label, checked, toggle) in [
            (leave_label, self.leave, Event::ToggleLeave),
            (
                "Hide this Split Inbox when empty",
                self.hide,
                Event::ToggleHide,
            ),
        ] {
            y += 8.0;
            let mark = pos2(left, y + 1.0);
            let label_x = left + 14.0 + 12.0;
            let width = SMALL.width(&ctx, label);
            let hit = Rect::from_min_max(mark, pos2(label_x + width, y + 17.0));
            if ui
                .interact(
                    hit,
                    exo_ui::id(ui.ctx(), ("split-inbox-check", label)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                event = Some(toggle);
            }
            paint_checkbox(&painter, mark, checked, tokens);
            // The checkbox's label: `rgba(0, 0, 0, .55)`.
            SMALL.paint_line(
                &painter,
                pos2(label_x, y + 1.0),
                label,
                tokens.ink(0.55, 0.55),
                None,
            );
            y += 16.0;
        }

        // The buttons have `margin-top: 44px`, right-aligned and nudged down 3px, with the
        // tertiary one at the left edge, 12px out. A split that's on turns off rather than being
        // deleted, and one that's off turns on rather than being saved.
        let top = y + 44.0 + NUDGE;
        let disabled = self
            .editing
            .as_ref()
            .and_then(|id| settings.split(id))
            .map(|split| split.is_disabled);
        let (primary, on_primary) = match disabled {
            None => ("Create", Event::Save),
            Some(true) => ("Turn On Split Inbox", Event::Toggle),
            Some(false) => ("Save", Event::Save),
        };
        let primary = DialogAction::new(primary, ActionKind::Primary).enabled(!self.incomplete());
        let cancel = DialogAction::new("Cancel", ActionKind::Plain);
        let mut right = body.right() + 13.0;
        for (id, action, clicked) in [
            ("primary", primary, on_primary),
            ("cancel", cancel, Event::Close),
        ] {
            let width = action.width(&ctx);
            let rect =
                Rect::from_min_size(pos2(right - width, top), vec2(width, DIALOG_ACTION_HEIGHT));
            if action
                .show(ui, exo_ui::id(ui.ctx(), ("split-inbox-action", id)), rect)
                .clicked()
            {
                event = Some(clicked);
            }
            right = rect.left() - 8.0;
        }
        if let Some(disabled) = disabled {
            let (label, clicked) = if disabled {
                ("Delete Split Inbox", Event::Delete)
            } else {
                ("Turn Off Split Inbox", Event::Toggle)
            };
            let tertiary = DialogAction::new(label, ActionKind::Plain);
            let rect = Rect::from_min_size(
                pos2(left - 12.0, top),
                vec2(tertiary.width(&ctx), DIALOG_ACTION_HEIGHT),
            );
            if tertiary
                .show(
                    ui,
                    exo_ui::id(ui.ctx(), ("split-inbox-action", "tertiary")),
                    rect,
                )
                .clicked()
            {
                event = Some(clicked);
            }
        }
        event
    }
}

impl SplitDialogs {
    /// Validates the form's name and definition, showing what's wrong, and
    /// returns them cleaned.
    fn validate(&mut self, account: &Account) -> Option<(String, String)> {
        let Screen::Form(form) = &mut self.screen else {
            return None;
        };
        let settings = account.settings();
        let others: Vec<SplitInbox> = settings
            .splits()
            .filter(|split| Some(&split.id) != form.editing.as_ref())
            .cloned()
            .collect();
        let name = clean_whitespace(&form.name);
        let query = clean_whitespace(&form.query);
        // A built-in split keeps its own name (Calendar) without the check, since a template's
        // settings don't check names either.
        let builtin_name = form
            .editing
            .as_ref()
            .and_then(|id| settings.split(id))
            .is_some_and(|split| split.is_default() && split.has_same_name(&name));
        form.name_error = if builtin_name {
            None
        } else {
            name_error(&name, &others)
        };
        if form.name_error.is_some() {
            form.focus = Some(Field::Name);
        }
        let (labels, _) = definitions(account);
        let index = exo_store::LabelIndex::new(
            [],
            labels
                .iter()
                .map(|label| (label.id.as_str(), label.name.as_str())),
        );
        if let Some(invalid) = SplitQuery::parse(&query).invalidity(&index) {
            self.alert = Some((AlertKind::Invalid(invalid.text), 0));
            return None;
        }
        (form.name_error.is_none() && !name.is_empty() && !query.is_empty())
            .then_some((name, query))
    }

    /// Validates and saves the form.
    pub(super) fn save<A: Clone + 'static>(
        &mut self,
        account: &mut Account,
        commands: &mut Commands<A>,
        wrap: &impl Fn(Action) -> A,
        current: &ListId,
    ) -> Outcome {
        let Some((name, query)) = self.validate(account) else {
            return Outcome::default();
        };
        let Screen::Form(form) = &self.screen else {
            return Outcome::default();
        };
        let settings = account.settings().clone();
        let (labels, me) = definitions(account);
        let definitions = Definitions {
            labels: &labels,
            me: &me,
        };
        match form.editing.clone() {
            Some(id) => {
                let Some(old) = settings.split(&id) else {
                    return Outcome::default();
                };
                let new = old.update(&form.changes(old, name, query), definitions);
                if !replace(account, commands, wrap, old, &new, true) {
                    self.alert = Some((AlertKind::TooLong, 0));
                    return Outcome::default();
                }
                self.screen = Screen::List;
                // A redefined split has a new id, so keep showing it by that.
                let show = (*current == ListId::Split(id)).then(|| ListId::Split(new.id.clone()));
                Outcome { show, close: false }
            }
            None => {
                let mut split = SplitInbox::new(&name, &query, None, None, definitions);
                split.leave_threads_in_important_other = form.leave;
                split.hide_split_when_empty = Some(form.hide);
                if !add(account, commands, wrap, &split) {
                    self.alert = Some((AlertKind::TooLong, 0));
                    return Outcome::default();
                }
                self.created_hidden = form.hide.then(|| split.id.clone());
                let tab = self.tab;
                self.reset();
                self.tab = tab;
                Outcome {
                    show: Some(ListId::Split(split.id)),
                    close: true,
                }
            }
        }
    }

    /// `Turn On Split Inbox` / `Turn Off Split Inbox` in Edit Split Inbox. It toggles the split
    /// with the form's changes, then closes the dialog when the split turned on, or else goes back
    /// to the list.
    pub(super) fn toggle_edited<A: Clone + 'static>(
        &mut self,
        account: &mut Account,
        commands: &mut Commands<A>,
        wrap: &impl Fn(Action) -> A,
        current: &ListId,
    ) -> Outcome {
        let Screen::Form(Form {
            editing: Some(id), ..
        }) = &self.screen
        else {
            return Outcome::default();
        };
        let Some(split) = account.settings().split(id).cloned() else {
            return Outcome::default();
        };
        let changes = if matches!(&self.screen, Screen::Form(form) if form.has_changes(account.settings()))
        {
            let Some((name, query)) = self.validate(account) else {
                return Outcome::default();
            };
            let Screen::Form(form) = &self.screen else {
                return Outcome::default();
            };
            form.changes(&split, name, query)
        } else {
            SplitChanges::default()
        };
        let Ok(show) = toggle(account, commands, wrap, &split, changes, current) else {
            self.alert = Some((AlertKind::TooLong, 0));
            return Outcome::default();
        };
        if split.is_disabled {
            let tab = self.tab;
            self.reset();
            self.tab = tab;
            Outcome { show, close: true }
        } else {
            self.screen = Screen::List;
            Outcome { show, close: false }
        }
    }
}
