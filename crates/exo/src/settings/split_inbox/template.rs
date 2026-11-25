//! Template-backed split settings transitions, active-template detection, and hero header
//! rendering.

use egui::{Rect, Ui, pos2};
use exo_commands::{Commands, Toast};
use exo_store::{
    Definitions, ListId, Me, Settings, SplitChanges, SplitEntry, SplitInbox, Template,
};
use exo_ui::{Family, Font, TextStyle, palette};

use super::alert::AlertKind;
use super::mutations::{self, definitions, toggle_important_other, too_long};
use super::{Back, Event, Outcome, Screen, SplitDialogs};
use crate::account::Account;
use crate::settings::Action;
use crate::settings::split_template::{Kind, TemplateAction, TemplateEvent};
use crate::splits;

impl SplitDialogs {
    /// Carries out something done in a template's settings.
    pub(super) fn template_event<A: Clone + 'static>(
        &mut self,
        event: TemplateEvent,
        account: &mut Account,
        commands: &mut Commands<A>,
        current: &ListId,
        wrap: &impl Fn(Action) -> A,
    ) -> Outcome {
        let Some(library) = self.library.clone() else {
            return Outcome::default();
        };
        let Screen::Template(form, _) = &mut self.screen else {
            return Outcome::default();
        };
        let Some(template) = library.templates.get(&form.template) else {
            return Outcome::default();
        };
        match event {
            TemplateEvent::Add => {
                form.add(template);
                form.focus = true;
            }
            TemplateEvent::Pick(sender) => form.pick(sender),
            TemplateEvent::PickerKey(key) => {
                if let Some(sender) = form.picker.key(key) {
                    form.pick(sender);
                }
            }
            TemplateEvent::Remove(index) => {
                if index < form.senders.len() {
                    form.senders.remove(index);
                }
                form.focus = true;
            }
            TemplateEvent::ToggleLeave => form.leave = !form.leave,
            TemplateEvent::ToggleHide => {
                form.hide = !form.hide;
                // Important • Other's setting applies at once.
                if form.kind == Kind::ImportantOther {
                    let hide = form.hide;
                    account.update_settings(|settings| settings.set_hide_other_when_empty(hide));
                }
            }
            TemplateEvent::Action(TemplateAction::Reset) => form.reset(template),
            TemplateEvent::Action(TemplateAction::Delete) => {
                self.alert = Some((AlertKind::DeleteTemplateSplit, 0))
            }
            TemplateEvent::Action(TemplateAction::Toggle) => {
                return self.template_save(template, true, account, commands, current, wrap);
            }
            TemplateEvent::Action(TemplateAction::Primary) => {
                return self.template_primary(template, account, commands, current, wrap);
            }
        }
        Outcome::default()
    }

    /// The primary button: saves the template's split, turns it on, or creates it.
    fn template_primary<A: Clone + 'static>(
        &mut self,
        template: &Template,
        account: &mut Account,
        commands: &mut Commands<A>,
        current: &ListId,
        wrap: &impl Fn(Action) -> A,
    ) -> Outcome {
        let Screen::Template(form, _) = &mut self.screen else {
            return Outcome::default();
        };
        if !form.commit_pending(template) {
            return Outcome::default();
        }
        let Screen::Template(form, _) = &self.screen else {
            return Outcome::default();
        };
        if let Some(split) = form
            .split
            .as_ref()
            .and_then(|id| account.settings().split(id))
        {
            let toggle = split.is_disabled;
            return self.template_save(template, toggle, account, commands, current, wrap);
        }
        if account
            .settings()
            .splits()
            .any(|split| split.has_same_name(&template.name))
        {
            self.alert = Some((AlertKind::NameTaken, 0));
            return Outcome::default();
        }
        // Creates the split with the template's name, definition, kind and id.
        let (labels, me) = definitions(account);
        let mut split = SplitInbox::new(
            &template.name,
            &template.query_with(&form.senders),
            template.kind.as_deref(),
            Some(&template.id),
            Definitions {
                labels: &labels,
                me: &me,
            },
        );
        split.leave_threads_in_important_other = form.leave;
        split.hide_split_when_empty = Some(form.hide);
        if too_long(
            account.settings(),
            None,
            &split,
            &Me {
                addresses: &me,
                name: None,
            },
        ) {
            self.alert = Some((AlertKind::TooLong, 0));
            return Outcome::default();
        }
        account.update_settings(|settings| {
            settings.add_split(split.clone());
            settings.engage_splits();
        });
        let settings = Event::OpenTemplate(template.id.clone(), Back::List);
        commands.toast(
            Toast::new(format!("Split Inbox turned on: {}", split.name())).action(
                "Split Inbox Settings",
                None,
                wrap(Action::Split(settings)),
            ),
        );
        self.created_hidden = form.hide.then(|| split.id.clone());
        self.reset();
        Outcome {
            show: Some(ListId::Split(split.id)),
            close: true,
        }
    }

    /// Saves the template's split, turning it on or off when `toggle`. Important • Other and Inbox
    /// turn Important and Other on or off.
    fn template_save<A: Clone + 'static>(
        &mut self,
        template: &Template,
        toggle: bool,
        account: &mut Account,
        commands: &mut Commands<A>,
        current: &ListId,
        wrap: &impl Fn(Action) -> A,
    ) -> Outcome {
        let Screen::Template(form, back) = &mut self.screen else {
            return Outcome::default();
        };
        let back = *back;
        match form.kind {
            Kind::ImportantOther if form.was_enabled => {
                self.alert = Some((AlertKind::TurnOffImportantOther, 0));
                return Outcome::default();
            }
            Kind::ImportantOther | Kind::Inbox => {
                let show = Some(toggle_important_other(account, commands, wrap));
                self.reset();
                return Outcome { show, close: true };
            }
            Kind::Split => {}
        }
        if !form.commit_pending(template) {
            return Outcome::default();
        }
        let Screen::Template(form, _) = &self.screen else {
            return Outcome::default();
        };
        let Some(old) = form
            .split
            .as_ref()
            .and_then(|id| account.settings().split(id))
            .cloned()
        else {
            return Outcome::default();
        };
        let editable = template
            .properties
            .senders
            .as_ref()
            .is_some_and(|senders| senders.is_editable);
        let mut changes = SplitChanges {
            leave_threads_in_important_other: Some(form.leave),
            hide_split_when_empty: Some(form.hide),
            ..SplitChanges::default()
        };
        if editable {
            changes.query = Some(template.query_with(&form.senders));
        }
        let changed = form.has_changes(template);
        if toggle && !changed {
            // Turns it on or off, which closes the dialog when the split turns on.
            let Ok(show) = mutations::toggle(
                account,
                commands,
                wrap,
                &old,
                SplitChanges::default(),
                current,
            ) else {
                self.alert = Some((AlertKind::TooLong, 0));
                return Outcome::default();
            };
            let outcome = if old.is_disabled {
                self.reset();
                Outcome {
                    show: None,
                    close: true,
                }
            } else {
                self.leave_form(back)
            };
            return Outcome { show, ..outcome };
        }
        if toggle {
            changes.is_disabled = Some(!old.is_disabled);
        }
        let (labels, me) = definitions(account);
        let mut new = old.update(
            &changes,
            Definitions {
                labels: &labels,
                me: &me,
            },
        );
        if new.kind != template.kind {
            new.kind.clone_from(&template.kind);
        }
        if too_long(
            account.settings(),
            Some(&old),
            &new,
            &Me {
                addresses: &me,
                name: None,
            },
        ) {
            self.alert = Some((AlertKind::TooLong, 0));
            return Outcome::default();
        }
        account.update_settings(|settings| {
            settings.replace_split(&old.id, new.clone());
            if !new.is_disabled {
                settings.engage_splits();
            }
        });
        // The toast for the template's changes.
        let text = if new.is_disabled != old.is_disabled {
            format!(
                "Split Inbox turned {}: {}",
                if new.is_disabled { "off" } else { "on" },
                new.name()
            )
        } else if new.is_default() {
            format!("{} Split updated.", new.name())
        } else {
            format!("Split '{}' updated.", new.name())
        };
        commands.toast(Toast::new(text).action(
            "Split Inbox Settings",
            None,
            wrap(Action::Split(Event::Edit(new.id.clone()))),
        ));
        if old.is_disabled && !new.is_disabled {
            self.reset();
            return Outcome {
                show: Some(ListId::Split(new.id)),
                close: true,
            };
        }
        let show = (*current == ListId::Split(old.id.clone())).then(|| {
            if new.is_disabled {
                splits::home(account.settings())
            } else {
                ListId::Split(new.id.clone())
            }
        });
        Outcome {
            show,
            ..self.leave_form(back)
        }
    }
}

/// Whether an enabled split (or Important and Other, or Inbox) uses `template`, in which case its
/// card shows a check.
pub(super) fn in_use(settings: &Settings, template: &Template) -> bool {
    settings
        .split_entries()
        .iter()
        .filter(|entry| !entry.is_disabled())
        .any(|entry| match entry {
            SplitEntry::System(system) => system.template_id() == template.id,
            SplitEntry::Split(split) => {
                split.template_id.as_deref() == Some(template.id.as_str())
                    || (template.kind.is_some() && split.kind == template.kind)
            }
        })
}

/// A template's header over its hero: its name (`rgba(0, 0, 0, .85)`), or `Important • Other`
/// with its dot at `.4`.
pub(super) fn paint_header(ui: &Ui, body: Rect, template: &Template) {
    const HEADER: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 16.0, 20.0);
    let tokens = palette(ui.ctx());
    let painter = ui.painter();
    let ink = tokens.ink(0.85, 0.85);
    let at = pos2(body.left(), body.top() - 20.0 + 2.0);
    if template.id == "splitTemplate_importantOther" {
        let x = at.x + HEADER.paint_line(painter, at, "Important ", ink, None);
        let x = x + HEADER.paint_line(painter, pos2(x, at.y), "•", tokens.ink(0.4, 0.4), None);
        HEADER.paint_line(painter, pos2(x, at.y), " Other", ink, None);
    } else {
        HEADER.paint_line(painter, at, &template.name, ink, Some(body.width() - 40.0));
    }
}
