//! The split inbox dialogs: Split Inbox Settings (its Active and Inactive tabs), New and Edit
//! Split Inbox, Hide Empty Split Inboxes, their alerts, and the actions behind them (creating,
//! replacing, turning on and off, reordering and deleting splits, and turning Important and Other
//! on and off).
//!
//! With the split library (`split_library`, `split_template`), Add Split Inbox opens the library
//! and a split from a template opens the template's settings. Without it, Add Split Inbox opens
//! New Split Inbox and a built-in split opens Edit Split Inbox. Important, Other and Inbox, whose
//! settings only the library describes, turn Important and Other off (after a confirmation) or on.

mod alert;
mod form;
mod hide_empty;
mod list;
mod mutations;
mod template;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use egui::{Context, Rect, vec2};
use exo_commands::Commands;
use exo_store::{ListId, Settings, SplitEntry, SplitLibrary};
use exo_ui::{Alert, DIALOG_WIDTH, Dialog, Family, Font, TextStyle};

pub(crate) use mutations::{TOO_LONG, add, definitions, replace, toggle};

use self::alert::AlertKind;
use self::form::Form;
use super::split_library::{self, Library, LibraryEvent};
use super::split_template::{TemplateEvent, TemplateForm};
use crate::account::Account;
use crate::{compose::contacts, contact_picker::PickerKey};

/// The dialogs' content width: the dialog's 550px less `padding: 20px 32px`.
pub(super) const CONTENT_WIDTH: f32 = DIALOG_WIDTH - 64.0;
/// Body text: 12px/20px, nudged down 3px.
pub(super) const BODY: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
pub(super) const BODY_BOLD: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 12.0, 20.0);
pub(super) const NUDGE: f32 = 3.0;
/// The Hide Empty link, checkbox labels and Hide Empty's rows: 12px/16px, nudged down 1px.
pub(super) const SMALL: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);

/// Split Inbox Settings' tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListTab {
    /// The splits shown as tabs, Important and Other (or Inbox) among them.
    #[default]
    Active,
    /// The splits that are off.
    Inactive,
}

/// Which split screen the dialog shows.
#[derive(Debug, Default)]
enum Screen {
    /// Split Inbox Settings.
    #[default]
    List,
    /// New or Edit Split Inbox.
    Form(Form),
    /// Hide Empty Split Inboxes.
    HideEmpty,
    /// The Split Inbox Library.
    Library(Library),
    /// A template's settings, for its split or a new one.
    Template(TemplateForm, Back),
}

/// Where a form or a template's settings go back to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Back {
    /// Split Inbox Settings.
    #[default]
    List,
    /// The library, as it was.
    Library,
    /// Nowhere, so the dialog closes.
    Close,
}

/// What the user did in a split dialog, or a key for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `Add Split Inbox`, or Enter on the list.
    AddSplit,
    /// The `Active` or `Inactive` tab.
    SelectTab(ListTab),
    /// A row of the list (or its gear), which opens the settings of the split, or of Important
    /// and Other.
    OpenRow(ListId),
    /// A toast's `Split Inbox Settings`, which edits split `id`.
    Edit(String),
    /// An Active row dragged from one index to another.
    Reorder(usize, usize),
    /// Edit Split Inbox's `Turn On Split Inbox` or `Turn Off Split Inbox`.
    Toggle,
    /// The `Hide Empty Split Inboxes` link.
    ShowHideEmpty,
    /// Save or Create, or Enter in the form.
    Save,
    /// `Delete Split Inbox`.
    Delete,
    /// Cancel, the back arrow, Escape or the backdrop, which close the dialog, asking first if
    /// anything changed.
    Close,
    /// The form's first checkbox.
    ToggleLeave,
    /// The form's second checkbox.
    ToggleHide,
    /// Hide Empty's row for a split, or Other.
    ToggleHideEmpty(ListId),
    /// Hide Empty's `Select all` / `Unselect all`.
    HideAll,
    /// The hide-empty nudge's `Try it out`, which asks to hide every empty split.
    OfferHideAll,
    /// The Undo on `Split Inboxes updated.`, which makes these show while empty again.
    UndoHideAll(Vec<ListId>),
    /// Something in the library.
    Library(LibraryEvent),
    /// Something in a template's settings.
    Template(TemplateEvent),
    /// Enter in a template's settings.
    TemplateEnter,
    /// The settings of template `id` (`template:<id>`), from a toast's `Split Inbox Settings` or
    /// the palette's `VIP Split Senders` and `News Split Senders`, going back where it says.
    /// Without the library, it opens New Split Inbox.
    OpenTemplate(String, Back),
    /// New Split Inbox, going `back` when closed, from the palette's `New Split Inbox from
    /// scratch`.
    New(Back),
    /// The alert's action, by index.
    AlertAction(usize),
    /// Enter or Space in the alert.
    AlertConfirm,
    /// Escape or a click outside the alert.
    AlertDismiss,
    /// The arrows or Tab in the alert, which move the selection.
    AlertCycle(isize),
}

/// What a split action asks of the app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Show this list.
    pub show: Option<ListId>,
    /// Close the dialog.
    pub close: bool,
}

/// The split dialogs' state.
#[derive(Debug, Default)]
pub struct SplitDialogs {
    screen: Screen,
    /// The list's tab, kept while the dialog moves between its screens.
    tab: ListTab,
    /// A click on the list took the keyboard from Add Split Inbox, which has it each time the
    /// list shows.
    list_blurred: bool,
    alert: Option<(AlertKind, usize)>,
    /// The list's scroll offset, and its scrollbar.
    scroll: f32,
    scrollbar: exo_ui::Scrollbar,
    /// A row being dragged: its index and how far the pointer has moved it.
    drag: Option<(usize, f32)>,
    /// The split library, when there is one.
    library: Option<Arc<SplitLibrary>>,
    /// The library as it was when a template was opened from it.
    library_state: Option<Library>,
    /// A split just made that's hidden when empty, which says so when it first empties.
    created_hidden: Option<String>,
}

impl SplitDialogs {
    /// Whether an alert is up, in which case it has the keyboard.
    pub fn alert_open(&self) -> bool {
        self.alert.is_some()
    }

    /// The split just made that's hidden when empty, if one was, once.
    pub fn take_created_hidden(&mut self) -> Option<String> {
        self.created_hidden.take()
    }

    /// Uses `library` for the library and templates' settings.
    pub fn set_library(&mut self, library: Option<SplitLibrary>) {
        self.library = library.map(Arc::new);
    }

    /// The split library, when Exo has a copy.
    pub fn library(&self) -> Option<&SplitLibrary> {
        self.library.as_deref()
    }

    /// The dialogs as they start, keeping the library.
    fn reset(&mut self) {
        *self = Self {
            library: self.library.take(),
            created_hidden: self.created_hidden.take(),
            ..Self::default()
        };
    }

    /// Opens Split Inbox Settings on its Active tab.
    pub fn open_list(&mut self) {
        self.reset();
    }

    /// Opens New Split Inbox. Closing it goes `back`.
    pub fn open_new(&mut self, back: Back) {
        self.alert = None;
        self.screen = Screen::Form(Form::new(back));
    }

    /// Opens Hide Empty Split Inboxes.
    pub fn open_hide_empty(&mut self) {
        self.reset();
        self.screen = Screen::HideEmpty;
    }

    /// Opens the library, or New Split Inbox without one. Either goes back to Split Inbox
    /// Settings.
    pub fn open_library(&mut self) {
        self.alert = None;
        if self.library.is_some() {
            self.screen = Screen::Library(self.library_state.take().unwrap_or_default());
        } else {
            self.open_new(Back::List);
        }
    }

    /// Opens the library with only the templates of the role named `role` (in any case), or as it
    /// starts when no role has that name.
    pub fn open_library_at_role(&mut self, role: &str) {
        self.open_library();
        let role = role.to_lowercase();
        let index = self.library.as_deref().and_then(|library| {
            library
                .roles()
                .iter()
                .position(|section| section.section.name.to_lowercase() == role)
        });
        if let (Screen::Library(state), Some(index)) = (&mut self.screen, index) {
            state.show_role(index);
        }
    }

    /// Opens the settings for template `id` (or a built-in kind's), going `back`. Returns false
    /// when there's no such template.
    fn open_template(&mut self, settings: &Settings, id: &str, back: Back) -> bool {
        let Some(template) = self
            .library
            .as_ref()
            .and_then(|library| library.template(id))
        else {
            return false;
        };
        self.alert = None;
        self.screen = Screen::Template(TemplateForm::new(template, settings), back);
        true
    }

    /// Opens the settings for split `id`, which are its template's, or else Edit Split Inbox. Both
    /// go back to the list.
    fn open_split(&mut self, settings: &Settings, id: &str) {
        let template = settings
            .split(id)
            .and_then(|split| {
                self.library
                    .as_ref()
                    .and_then(|library| library.template_for(split))
            })
            .map(|template| template.id.clone());
        if !template.is_some_and(|template| self.open_template(settings, &template, Back::List)) {
            self.open_edit(settings, id);
        }
    }

    /// Opens Edit Split Inbox for split `id` (`custom:<slug>`), back to the list on close.
    fn open_edit(&mut self, settings: &Settings, id: &str) {
        let Some(split) = settings.split(id) else {
            return;
        };
        self.alert = None;
        self.screen = Screen::Form(Form::edit(split));
    }

    /// Carries out `event`. `current` is the list on screen, and `wrap` makes app actions of
    /// settings actions, for toasts.
    pub fn register_keys<A: Clone + 'static>(
        &self,
        commands: &mut Commands<A>,
        wrap: &impl Fn(super::Action) -> A,
    ) {
        const DIALOG: &[exo_commands::Region] = &[exo_commands::Region::Dialog];
        let split = |id: &'static str, event: Event, keys: &[&'static str]| {
            exo_commands::Command::shortcut(id, wrap(super::Action::Split(event)))
                .keys(keys.iter().copied())
                .regions(DIALOG)
        };
        crate::notice::register_alert_keys(
            commands,
            wrap(super::Action::Split(Event::AlertConfirm)),
            wrap(super::Action::Split(Event::AlertDismiss)),
            |step| wrap(super::Action::Split(Event::AlertCycle(step))),
        );
        commands.add(split("SPLIT_DIALOG_ESCAPE", Event::Close, &["esc"]));
        if let Some(enter) = self.enter() {
            commands.add(split("SPLIT_DIALOG_ENTER", enter, &["enter"]));
        }
        if let Some(remove) = self.row_delete() {
            commands.add(split(
                "SPLIT_DIALOG_REMOVE_ROW",
                remove,
                &["backspace", "del"],
            ));
        }
        for (id, key, keys) in self.picker_keys() {
            commands.add(split(
                id,
                Event::Template(TemplateEvent::PickerKey(key)),
                keys,
            ));
        }
    }

    pub fn run<A: Clone + 'static>(
        &mut self,
        event: Event,
        account: &mut Account,
        commands: &mut Commands<A>,
        current: &ListId,
        wrap: &impl Fn(super::Action) -> A,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        match event {
            Event::AddSplit => self.open_library(),
            Event::New(back) => self.open_new(back),
            Event::SelectTab(tab) => self.tab = tab,
            Event::OpenRow(list) => {
                let settings = account.settings().clone();
                let template = if list == ListId::Inbox {
                    "splitTemplate_inbox"
                } else {
                    "splitTemplate_importantOther"
                };
                match list {
                    ListId::Split(id) => self.open_split(&settings, &id),
                    _ if self.open_template(&settings, template, Back::List) => {}
                    ListId::Important | ListId::Other if !settings.unified_inbox => {
                        self.alert = Some((AlertKind::TurnOffImportantOther, 0))
                    }
                    _ => {
                        outcome.show =
                            Some(mutations::toggle_important_other(account, commands, wrap))
                    }
                }
            }
            Event::Edit(id) => {
                let settings = account.settings().clone();
                self.open_split(&settings, &id);
            }
            // Without the template (no library), New Split Inbox.
            Event::OpenTemplate(id, back) => {
                let settings = account.settings().clone();
                if !self.open_template(&settings, &id, back) {
                    self.reset();
                    self.open_new(back);
                }
            }
            Event::Library(LibraryEvent::Open(id)) => {
                if let Screen::Library(state) = std::mem::take(&mut self.screen) {
                    self.library_state = Some(state);
                }
                let settings = account.settings().clone();
                self.open_template(&settings, &id, Back::Library);
            }
            Event::Library(LibraryEvent::BuildYourOwn) => {
                if let Screen::Library(state) = std::mem::take(&mut self.screen) {
                    self.library_state = Some(state);
                }
                self.open_new(Back::Library);
            }
            Event::Template(event) => {
                outcome = self.template_event(event, account, commands, current, wrap)
            }
            Event::TemplateEnter => {
                let enter = match (&self.screen, &self.library) {
                    (Screen::Template(form, _), Some(library)) => library
                        .templates
                        .get(&form.template)
                        .and_then(|template| form.enter(template)),
                    _ => None,
                };
                if let Some(event) = enter {
                    outcome = self.template_event(event, account, commands, current, wrap);
                }
            }
            Event::Reorder(from, to) => {
                let rows: Vec<String> = mutations::active_rows(account.settings())
                    .iter()
                    .map(|entry| entry.external_id().to_owned())
                    .collect();
                if let (Some(moved), Some(pushed)) = (rows.get(from), rows.get(to)) {
                    account.update_settings(|settings| settings.reorder_splits(moved, pushed));
                }
            }
            Event::Toggle => outcome = self.toggle_edited(account, commands, wrap, current),
            Event::ShowHideEmpty => self.open_hide_empty(),
            Event::Save => outcome = self.save(account, commands, wrap, current),
            Event::Delete => {
                if matches!(
                    self.screen,
                    Screen::Form(Form {
                        editing: Some(_),
                        ..
                    })
                ) {
                    self.alert = Some((AlertKind::Delete, 0));
                }
            }
            Event::Close => outcome = self.close(account),
            Event::ToggleLeave => {
                if let Screen::Form(form) = &mut self.screen {
                    form.leave = !form.leave;
                }
            }
            Event::ToggleHide => {
                if let Screen::Form(form) = &mut self.screen {
                    form.hide = !form.hide;
                }
            }
            Event::ToggleHideEmpty(list) => account.update_settings(|settings| {
                let hide = !hide_empty::hides_when_empty(settings, &list);
                hide_empty::set_hides_when_empty(settings, &list, hide);
            }),
            Event::HideAll => account.update_settings(|settings| {
                let rows: Vec<ListId> = hide_empty::rows(settings)
                    .iter()
                    .map(SplitEntry::list)
                    .collect();
                let hide = !rows
                    .iter()
                    .all(|list| hide_empty::hides_when_empty(settings, list));
                for list in rows {
                    hide_empty::set_hides_when_empty(settings, &list, hide);
                }
            }),
            Event::OfferHideAll => self.alert = Some((AlertKind::HideAll, 0)),
            Event::UndoHideAll(lists) => account.update_settings(|settings| {
                for list in &lists {
                    hide_empty::set_hides_when_empty(settings, list, false);
                }
            }),
            Event::AlertConfirm => {
                if let Some((_, selected)) = self.alert {
                    outcome = self.run(
                        Event::AlertAction(selected),
                        account,
                        commands,
                        current,
                        wrap,
                    );
                }
            }
            Event::AlertAction(index) => {
                outcome = self.alert_action(index, account, commands, wrap)
            }
            Event::AlertDismiss => self.alert = None,
            Event::AlertCycle(step) => {
                if let Some((kind, selected)) = &mut self.alert {
                    *selected = Alert::move_selection(*selected, kind.actions().len(), step, true);
                }
            }
        }
        outcome
    }

    /// Cancel, back, Escape or the backdrop, after the form's discard check.
    fn close(&mut self, account: &Account) -> Outcome {
        match &self.screen {
            Screen::Form(form) if form.has_changes(account.settings()) => {
                self.alert = Some((AlertKind::Discard, 0));
                Outcome::default()
            }
            Screen::Form(form) => self.leave_form(form.back),
            Screen::Template(form, back) => {
                let back = *back;
                let changed = self
                    .library
                    .as_ref()
                    .and_then(|library| library.templates.get(&form.template))
                    .is_some_and(|template| form.has_changes(template));
                if changed {
                    self.alert = Some((AlertKind::Discard, 0));
                    Outcome::default()
                } else {
                    self.leave_form(back)
                }
            }
            Screen::Library(_) => self.leave_form(Back::List),
            Screen::List | Screen::HideEmpty => Outcome {
                show: None,
                close: true,
            },
        }
    }

    /// Leaves the form or template for where it goes back to.
    fn leave_form(&mut self, back: Back) -> Outcome {
        self.alert = None;
        match back {
            Back::List => {
                self.screen = Screen::List;
                Outcome::default()
            }
            Back::Library => {
                self.screen = Screen::List;
                self.open_library();
                Outcome::default()
            }
            Back::Close => {
                self.screen = Screen::List;
                Outcome {
                    show: None,
                    close: true,
                }
            }
        }
    }

    /// What Backspace and Delete do, which is remove the template's sender row that has the
    /// keyboard.
    pub fn row_delete(&self) -> Option<Event> {
        match &self.screen {
            Screen::Template(form, _) => form
                .focused_row()
                .map(|row| Event::Template(TemplateEvent::Remove(row))),
            _ => None,
        }
    }

    /// The keys VIP's contact picker takes over from the dialog's while it has the keyboard.
    pub fn picker_keys(&self) -> Vec<(&'static str, PickerKey, &'static [&'static str])> {
        match &self.screen {
            Screen::Template(form, _) if form.picker.is_focused() => form.picker.keys(),
            _ => Vec::new(),
        }
    }

    /// What Enter does on the current screen: Add Split Inbox on the list,
    /// Save in the form.
    pub fn enter(&self) -> Option<Event> {
        match self.screen {
            Screen::List => Some(Event::AddSplit),
            Screen::Form(_) => Some(Event::Save),
            Screen::Template(..) => Some(Event::TemplateEnter),
            Screen::HideEmpty | Screen::Library(_) => None,
        }
    }

    /// Draws the dialog for the current screen over `area` while `open` (and while it fades out
    /// after), and its alert. Returns what the user did and whether the dialog's close control or
    /// backdrop was clicked. `None` once it has faded out.
    pub fn show(
        &mut self,
        ctx: &Context,
        area: Rect,
        open: bool,
        account: &Account,
        book: Option<&contacts::Index>,
    ) -> Option<(Option<Event>, bool)> {
        let settings = account.settings();
        let library = self.library.clone();
        let template = match (&self.screen, &library) {
            (Screen::Template(form, _), Some(library)) => library.templates.get(&form.template),
            _ => None,
        };
        let (header, back, body_height) = match &self.screen {
            Screen::List => (
                "Split Inbox Settings",
                false,
                list::height(ctx, settings, self.tab),
            ),
            Screen::Form(form) if form.editing.is_some() => {
                ("Edit Split Inbox", true, form.height())
            }
            Screen::Form(form) => ("New Split Inbox", true, form.height()),
            Screen::HideEmpty => (
                "Hide Empty Split Inboxes",
                false,
                hide_empty::height(settings),
            ),
            // These draw their own headers. The library has none, and a template's is over its
            // hero.
            Screen::Library(_) => ("", true, 0.0),
            Screen::Template(form, _) => (
                "",
                true,
                template.map_or(0.0, |template| form.body_height(ctx, template, settings)),
            ),
        };
        if !matches!(self.screen, Screen::List) {
            self.list_blurred = false;
        }
        let mut dialog = Dialog::new("split-inbox", header, body_height).top();
        if back {
            dialog = dialog.back();
        }
        if matches!(self.screen, Screen::Library(_)) {
            // The library: up to 1080px wide, `margin: 36px 48px`.
            dialog = dialog.fill(1080.0, vec2(48.0, 36.0));
        }
        // An alert's backdrop, a layer above, takes the pointer from the dialog under it.
        let alert_open = self.alert.is_some();
        let response = dialog.show(ctx, area, open, |ui, body| {
            match &mut self.screen {
                Screen::List => list::show(
                    ui,
                    body,
                    settings,
                    self.tab,
                    &mut self.list_blurred,
                    (&mut self.scroll, &mut self.scrollbar),
                    &mut self.drag,
                ),
                Screen::Form(form) => form.show(ui, body, settings),
                Screen::HideEmpty => hide_empty::show(ui, body, settings),
                Screen::Library(state) => {
                    let library = library.as_deref()?;
                    split_library::show(ui, body, library, state, &|template| {
                        template::in_use(settings, template)
                    })
                    .map(Event::Library)
                }
                Screen::Template(form, _) => {
                    let template = template?;
                    // The card around the body: `padding: 20px 32px` and the header's line.
                    let card = Rect::from_min_max(
                        body.min - vec2(32.0, 40.0),
                        body.max + vec2(32.0, 20.0),
                    );
                    let event = form
                        .show(ui, card, body, template, settings, book)
                        .map(Event::Template);
                    template::paint_header(ui, body, template);
                    event
                }
            }
        });
        let Some(mut event) = response.inner else {
            self.reset();
            return None;
        };
        let mut close = response.close && !alert_open;
        if let Some((kind, selected)) = &self.alert {
            event = self
                .show_alert(ctx, area, settings, kind, *selected)
                .or(event);
            close = false;
        }
        Some((event, close))
    }
}
