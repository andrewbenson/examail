//! The calendar: the day view in the right pane, the week view, their keys and palette rows, and
//! the calendar's data.

mod accounts;
mod availability;
mod block;
mod calendars;
mod day;
pub(crate) mod description;
mod details;
mod editing;
pub mod event;
mod form;
mod grid;
mod guests;
mod invites;
mod jump;
mod keys;
pub(crate) mod layout;
mod meet;
mod month;
mod options;
mod patch;
pub mod places;
mod popup;
mod provider;
mod repeat;
mod save;
mod share;
mod spelling;
mod sync;
mod timezones;
mod views;
mod week;
mod when;
mod when_edit;

pub(crate) use event::{Rsvp, my_attendance, unique_key as event_key};
pub(crate) use invites::invite_event;
pub(crate) use keys::{
    PICKER, PICKER_ROW, register, register_focus_cycle, register_picker_keys,
    share_availability_command,
};
pub(crate) use share::Target;
pub use sync::CalendarSync;
pub(crate) use week::ListedCalendar;

use std::collections::BTreeMap;

use exo_commands::{Hint, Region, Toast};
use exo_editor::FormatCommand;
use exo_store::AccountCalendar;
use jiff::{ToSpan, Zoned, civil::Date};

use self::{
    details::Details,
    event::Event,
    form::{EventForm, Field, Meeting, Mode},
    grid::GridView,
    popup::Placement,
    save::{Prompt, Undone},
    sync::Window,
    week::Week,
};
use crate::thread::invite::{Invite, Response};

/// Something the calendar does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A key in a calendar's colour picker.
    PickerKey(exo_ui::PickerKey),
    /// ⌘D with the right pane's form open, which moves the keyboard to the next section.
    CycleFocus,
    /// `-` / `=`, which move the day view to the day before (-1) or after (1) the one it shows.
    Step(i64),
    /// `t` or the day view's Today, which moves the day view to today unless an event is being
    /// made.
    Today,
    /// `0`: the day view, or the contact pane when it shows.
    Cycle,
    /// The footer's calendar button.
    Footer,
    /// `Open Day` in the palette.
    OpenDay,
    /// `Close Calendar`.
    Close,
    /// `Show Declined Events` / `Hide Declined Events`.
    ToggleDeclined,
    /// `Show Weekends` / `Hide Weekends`.
    ToggleWeekends,
    /// `See Message` on a refused answer's toast, which opens conversation `thread`.
    SeeMessage(String),
    /// `2`, `Open Week` or the rail's Calendar, which open the week view.
    OpenWeek,
    /// The week view one week back (-1) or on (1).
    WeekStep(i64),
    /// The week view on this week (`space`, `t`).
    WeekToday,
    /// Close the week view (`0`, `1`, Escape, the rail's Mail).
    CloseWeek,
    /// Escape with the event details up, which closes the recurring alert, else the menu, else the
    /// details.
    DetailsEscape,
    /// Enter with the recurring alert up, which is its OK.
    DetailsConfirm,
    /// `b`, `shift+b` or `Create Empty Event`, which open a new event's form.
    Create,
    /// ⌘Enter, `Save Event`.
    Save,
    /// ⌘⇧, (or ⌘⇧.) or `Discard Event`, which closes the form, asking first.
    Discard,
    /// Escape in the form, which closes an open dropdown first and is otherwise Discard.
    FormEscape,
    /// ⌘⇧= / ⌘⇧- with the form open, which move the draft a day (a week, in the week view) on or
    /// back.
    FormStep(i64),
    /// ⌘⇧0 with the form open, which moves the draft to today.
    FormToday,
    /// The palette's `Edit …` rows, which move the keyboard to a field.
    FormFocus(Field),
    /// `Make all day` / `Remove all day`.
    FormAllDay,
    /// A formatting key in About.
    FormFormat(FormatCommand),
    /// `Add Google Meet Link` / `Remove Google Meet Link`.
    FormMeet,
    /// ⌘C on the event's own meeting link, which copies it.
    FormCopyLink,
    /// `Edit event in Google Calendar` and the rest that open Google Calendar's editor.
    FormExternal,
    /// An alert's or dialog's action, by index.
    Prompt(usize),
    /// ← (1) / → (-1) and Tab (-1) / ⇧Tab (1) across an alert's buttons.
    PromptSelect(isize),
    /// ↑ (-1) / ↓ (1) in a dialog's options.
    PromptChoose(isize),
    /// Undo on a toast, which takes back the held change `token`.
    Undo(u64, Undone),
    /// Escape while dragging in the grid.
    CancelDrag,
    /// `tab` in the messages, which moves the keyboard to the focused message's invite card.
    InviteFocus,
    /// The invite card's next (1) or previous (-1) button.
    InviteMove(isize),
    /// Enter or space in the invite card, which gives its focused button's answer.
    InviteSubmit,
    /// `m` or `Meet With`, which move the keyboard to Meet's input.
    MeetFocus,
    /// Escape in Meet's input.
    MeetBlur,
    /// `Calendar Settings`, the week view's gear.
    OpenSettings,
    /// Escape in Calendar Settings, its ×, the backdrop.
    CloseSettings,
    /// `Find Time`.
    FindTime,
    /// `Cancel Find Time`, Escape while finding.
    CancelFindTime,
    /// `Show next availability`, which goes to the week with the first free slot.
    ShowAvailability(Date),
    /// Escape in the invite card, which goes back to the messages.
    InviteBlur,
    /// `Jump to Date`, which opens its palette.
    OpenJumpToDate,
    /// A Jump to Date row, which moves the week view, else the day view, to this day.
    JumpTo(Date),
    /// `Share Availability` from the list, a conversation or the week. The week picks free times
    /// for the conversation's draft or a Reply All, or for a new message.
    ShareAvailability,
    /// Enter, ⌘Enter or Insert, which put the times into the draft.
    ShareInsert,
    /// Cancel, `0`, `1`, ⌘⇧0, or Escape outside a field, which close the page.
    ShareCancel,
    /// Escape in a field, which clears its text or else closes the page.
    ShareEscape,
}

/// What opened the day view. A day view that a command opened stays as the cursor moves, and any
/// other goes when the right pane follows something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Command,
    Other,
}

/// The day the right pane shows.
#[derive(Clone, Copy, Debug)]
struct DayPane {
    date: Date,
    source: Source,
}

/// What the right pane follows: the open thread and its focused message, or the thread the
/// contact pane follows in the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Context {
    thread: Option<String>,
    message: Option<String>,
}

/// The calendar's UI state.
#[derive(Default)]
pub(crate) struct Calendar {
    day: Option<DayPane>,
    grid: GridView,
    /// Day changes with the chevrons or keys since the last hint.
    date_changes: u8,
    /// The window last asked of the data.
    window: Option<Window>,
    /// What the right pane followed last frame.
    context: Context,
    /// The invite of what it follows, once read, and whether the day view has opened for it.
    invite: Option<Invite>,
    invite_shown: bool,
    /// The week view, when it's up.
    week: Option<Week>,
    /// `0` from the day view has said, this session, that `2` opens the week.
    week_hint_shown: bool,
    /// The event details, when they're up.
    details: Option<Details>,
    /// The event form, while it's open.
    form: Option<EventForm>,
    /// Where the form's modal went (sticky, like the details').
    form_placement: Placement,
    /// The right pane's day under a new event's form, and its grid.
    form_day: Option<Date>,
    form_grid: GridView,
    /// An alert or dialog over the page.
    prompt: Option<Prompt>,
    /// New events held for Undo, by token, whose form opens again on Undo or a failure.
    created: BTreeMap<u64, form::Values>,
    /// The open thread, and my answers to its invites given since it opened, by UID.
    answers: (String, BTreeMap<String, Response>),
    /// Meet's last free/busy ask, the week it last saw, Find Time's first-slot search waiting on
    /// data, and the week's free slots shown.
    meet_asked: Option<(Vec<String>, jiff::Timestamp, jiff::Timestamp)>,
    meet_week: Option<Date>,
    find_pending: bool,
    slots: Vec<(Zoned, Zoned)>,
    /// Calendar Settings is up.
    options_open: bool,
    /// Calendar Settings' secondary time zone input.
    options_zone: timezones::ZoneInput,
    /// The Where field's suggestions.
    places: places::Places,
    /// A calendar account's sign-in in flight, and what it's for.
    signing_in: Option<accounts::SignInFor>,
    /// The day view's `Add Calendar` provider dropdown.
    takeover: provider::ProviderPicker,
}

impl crate::Mail {
    /// The day the right pane's calendar shows, if it does.
    pub fn calendar_day(&self) -> Option<Date> {
        self.calendar.day.map(|day| day.date)
    }

    /// The day the week view is around, if it's up.
    pub fn calendar_week(&self) -> Option<Date> {
        self.calendar.week.as_ref().map(|week| week.active)
    }

    /// Whether the event details are up, and if so whether their recurring alert is.
    pub fn calendar_details(&self) -> Option<bool> {
        self.calendar
            .details
            .as_ref()
            .map(|details| details.alert.is_some())
    }

    /// The calendar's prompt, form or week view has the keyboard while it's up, and an invite
    /// card when it's taken it.
    pub(crate) fn calendar_region(&self) -> Option<Region> {
        if self.calendar.prompt.is_some() {
            Some(Region::Alert)
        } else if self
            .calendar
            .week
            .as_ref()
            .is_some_and(|week| week.picker.is_some())
        {
            Some(Region::CalendarColorPicker)
        } else if self.calendar.options_open {
            Some(Region::Dialog)
        } else if self.calendar.form.as_ref().is_some_and(|form| !form.away) {
            Some(Region::CalendarCreate)
        } else if let Some(share) = self
            .calendar
            .week
            .as_ref()
            .and_then(|week| week.share.as_ref())
        {
            Some(match share.typing {
                Some(share::Input::Who) => Region::ShareAvailabilitySidebarAttendees,
                Some(share::Input::Zone) => Region::ShareAvailabilitySidebarTimezoneInput,
                None if share.control => Region::ShareAvailabilitySidebar,
                None => Region::ShareAvailability,
            })
        } else if let Some(week) = &self.calendar.week {
            Some(if week.meet.typing {
                Region::SeeYourWeekSidebarMeetWithInput
            } else {
                Region::SeeYourWeek
            })
        } else {
            self.thread
                .as_ref()
                .filter(|thread| thread.invite_focused())
                .map(|_| Region::ThreadMessageCalendarInvite)
        }
    }

    /// `CopyTooltip`'s copy: `text` on the clipboard, and its confirmation with the text cut at
    /// 80 characters.
    pub(super) fn calendar_copy(&mut self, ctx: &egui::Context, text: String) {
        ctx.copy_text(text.clone());
        let shown: String = if text.chars().count() > 80 {
            text.chars().take(80).chain(['…']).collect()
        } else {
            text
        };
        self.commands
            .hint(Hint::new().text(format!("\"{shown}\" copied to clipboard.")));
    }

    /// The title of the calendar's alert or dialog, if one is up.
    pub fn calendar_prompt(&self) -> Option<&'static str> {
        self.calendar.prompt.as_ref().map(Prompt::title)
    }

    /// The events the calendar shows on `date` (held changes included): their titles and
    /// start times (`9:00 AM`).
    pub fn calendar_events_on(&self, date: Date) -> Vec<(String, String)> {
        let now = self.now.clone();
        self.calendar_events(&now, None)
            .into_iter()
            .filter(|event| event.on(date))
            .map(|event| (event.title, event.start.strftime("%-I:%M %p").to_string()))
            .collect()
    }

    /// My answer on the open thread's invite card (`accepted`, `needsAction`, …), as it shows.
    pub fn invite_answer(&self) -> Option<&'static str> {
        let invite = self.thread.as_ref()?.invite()?;
        invite
            .answer_of(&self.calendar_invite_account(invite))
            .map(Response::status)
    }

    /// Calendar Settings is up (for tests).
    pub fn calendar_settings_open(&self) -> bool {
        self.calendar.options_open
    }

    /// The signed-out calendar account whose `Add Calendar` takes the day view's place, if it
    /// does (for tests).
    pub fn calendar_add_calendar(&self) -> Option<String> {
        self.calendar.day.and(self.calendar_takeover())
    }

    /// How many free slots Find Time shows.
    pub fn calendar_free_slots(&self) -> usize {
        if self
            .calendar
            .week
            .as_ref()
            .is_some_and(|week| week.meet.finding)
        {
            self.calendar.slots.len()
        } else {
            0
        }
    }

    /// The form's About, as the HTML it saves, and its Where.
    pub fn calendar_form_about(&self) -> Option<(String, String)> {
        let values = &self.calendar.form.as_ref()?.values;
        Some((values.about.clone(), values.location.clone()))
    }

    /// The open form's guests' chips: each address, the colour of their calendar's swatch (or
    /// `loading` / `empty`), and their busy status.
    pub fn calendar_form_chips(&self) -> Vec<(String, String, Option<&'static str>)> {
        self.calendar_guest_chips()
            .into_iter()
            .map(|(email, (marker, busy))| {
                let marker = match marker {
                    guests::Marker::Swatch(color) => color.name().to_owned(),
                    guests::Marker::Spinner => "loading".to_owned(),
                    guests::Marker::Empty => "empty".to_owned(),
                };
                (email, marker, busy)
            })
            .collect()
    }

    /// The guests' calendars overlaid on `day` in the week view or the right pane: each
    /// guest's address and their events' titles that day.
    pub fn calendar_guest_columns(&mut self, day: Date) -> Vec<(String, Vec<String>)> {
        let now = self.now.clone();
        let pane = self.calendar.week.is_none();
        let (guests, _) = self.calendar_overlay(&now, pane, &[day]);
        guests
            .into_iter()
            .map(|(email, events)| {
                (
                    email,
                    events
                        .into_iter()
                        .filter(|event| event.on(day))
                        .map(|event| event.title)
                        .collect(),
                )
            })
            .collect()
    }

    /// Whether the open form has given the keyboard to the mail (the right pane's, on a click
    /// there or ⌘D).
    pub fn calendar_form_away(&self) -> Option<bool> {
        self.calendar.form.as_ref().map(|form| form.away)
    }

    /// The open form's title and guests' addresses.
    pub fn calendar_form_values(&self) -> Option<(String, Vec<String>)> {
        let form = self.calendar.form.as_ref()?;
        Some((
            form.values.title.clone(),
            form.values
                .guests
                .iter()
                .map(|guest| guest.email.clone())
                .collect(),
        ))
    }

    /// Whether the open form asks for a new Meet link.
    pub fn calendar_form_new_meet(&self) -> Option<bool> {
        Some(self.calendar.form.as_ref()?.values.meeting == Meeting::NewMeet)
    }

    /// The open form's calendar.
    pub fn calendar_form_calendar(&self) -> Option<&str> {
        Some(&self.calendar.form.as_ref()?.values.calendar)
    }

    /// The open details' event: its title and its calendar (a guest's address for a guest's
    /// event).
    pub fn calendar_details_event(&self) -> Option<(String, String)> {
        let key = self.calendar.details.as_ref()?.key.clone();
        let event = self
            .calendar_detailed(&key, &self.now)
            .map(|(event, ..)| event)
            .or_else(|| {
                self.calendar_guest_detailed(&key, &self.now)
                    .map(|(event, _)| event)
            })?;
        Some((event.title, event.calendar_id))
    }

    /// The open details' line under When for a series (`Weekly on Monday`).
    pub fn calendar_details_rule(&self) -> Option<String> {
        let key = self.calendar.details.as_ref()?.key.clone();
        let (event, source, _) = self.calendar_detailed(&key, &self.now)?;
        self.calendar_rule_line(&event, &source)
    }

    /// The open form's Repeat as its trigger reads, and whether its Custom dialog is up.
    pub fn calendar_form_repeat(&self) -> Option<(String, bool)> {
        let form = self.calendar.form.as_ref()?;
        let anchor = form
            .values
            .when
            .as_ref()
            .map_or_else(|| self.now.date(), |(start, _)| start.date());
        Some((
            repeat::label(form.values.repeat.as_ref(), anchor),
            form.custom.is_some(),
        ))
    }

    /// Whether the event form is open, and if so whether it's a new event's.
    pub fn calendar_form(&self) -> Option<bool> {
        self.calendar
            .form
            .as_ref()
            .map(|form| form.mode == Mode::Create)
    }

    /// Opens the week view around `date`, closing the settings menu and the right pane's day.
    pub(crate) fn calendar_open_week(&mut self, date: Date) {
        self.settings.hide();
        self.calendar.day = None;
        match &mut self.calendar.week {
            Some(week) => week.go_to(date),
            None => self.calendar.week = Some(Week::new(date)),
        }
    }

    /// The day the account switched to opens the week view on. It's the middle of the week on
    /// screen, if the week view is up, so either account's first weekday finds it.
    pub(crate) fn calendar_week_to_keep(&self) -> Option<Date> {
        self.calendar_week()
            .map(|active| self.calendar_week_start(active) + 3.days())
    }

    /// Opens the week view on `day`'s week, kept from the account switched from, as the
    /// account's view starts.
    pub(crate) fn calendar_keep_week(&mut self, day: Date) {
        self.calendar_open_week(day);
    }

    /// A calendar notification's click opens the week view on `day`, with event `key`'s details up
    /// once the event shows and the grid scrolled to its end at `end_hour`.
    pub(crate) fn calendar_open_event(&mut self, key: String, day: Date, end_hour: i8) {
        self.calendar.details = None;
        self.calendar_open_week(day);
        if let Some(week) = &mut self.calendar.week {
            week.select(key, end_hour);
        }
    }

    /// Opens the day view on `date`, out of the settings menu's way. Returns whether it wasn't up
    /// (a new day view, whose grid scrolls to the morning again).
    fn calendar_open_day(&mut self, date: Date, source: Source) -> bool {
        self.settings.hide();
        let mounted = self.calendar.day.is_none();
        if mounted {
            self.calendar.grid.reset();
        }
        self.calendar.day = Some(DayPane { date, source });
        mounted
    }

    /// The day view on `date`, from the open thread (an invite's When, or a date in a message).
    pub(crate) fn calendar_show_day(&mut self, date: Date) {
        self.calendar_open_day(date, Source::Other);
    }

    /// Moves the day view by `days`, or opens it on today moved by `days`. Every third change
    /// hints at the week view.
    pub(super) fn calendar_step(&mut self, days: i64) {
        let today = self.now.clone().date();
        let from = self.calendar.day.map_or(today, |day| day.date);
        let date = from.checked_add(days.days()).unwrap_or(from);
        if self.compose.focused().is_some() {
        } else if self.calendar.date_changes >= 2 {
            self.commands.hint(
                Hint::new()
                    .text("Next time, hit")
                    .keys(["2"])
                    .text("to see your week!"),
            );
            self.calendar.date_changes = 0;
        } else {
            self.calendar.date_changes += 1;
        }
        self.calendar_open_day(date, Source::Other);
    }

    /// Carries out a calendar command.
    pub(crate) fn calendar_run(&mut self, action: Action) {
        let today = self.now.clone().date();
        // Changing the date, or the view, closes the details.
        let keeps_details = matches!(
            action,
            Action::ToggleDeclined
                | Action::DetailsEscape
                | Action::DetailsConfirm
                | Action::Prompt(_)
                | Action::PromptSelect(_)
                | Action::PromptChoose(_)
                | Action::Undo(..)
                | Action::CancelDrag
                | Action::InviteFocus
                | Action::InviteMove(_)
                | Action::InviteBlur
                | Action::InviteSubmit
                | Action::MeetFocus
                | Action::MeetBlur
                | Action::OpenJumpToDate
                | Action::ShareInsert
                | Action::ShareEscape
        );
        if !keeps_details {
            self.calendar.details = None;
        }
        match action {
            Action::Step(days) => self.calendar_step(days),
            Action::Today => {
                if self.calendar.form.is_none() {
                    self.settings.hide();
                    self.calendar_open_day(today, Source::Other);
                }
            }
            // With the day view up, `0` and the footer's button open the week on its day. The
            // first time `0` does, a hint says `2` does it now.
            Action::Cycle | Action::Footer if let Some(day) = self.calendar.day => {
                if action == Action::Cycle && !self.calendar.week_hint_shown {
                    self.calendar.week_hint_shown = true;
                    self.commands.hint(Hint::new().text("Please note: to make things faster, we're changing the calendar shortcut to").keys(["2"]).text("!"));
                }
                self.calendar_open_week(day.date);
            }
            Action::Cycle | Action::Footer => {
                self.calendar_open_day(today, Source::Other);
            }
            Action::OpenDay => {
                self.calendar_open_day(today, Source::Command);
            }
            // The week view closes, and the day view with it.
            Action::Close => {
                self.calendar.week = None;
                self.calendar.day = None;
            }
            Action::ToggleDeclined => self.account.update_settings(|settings| {
                settings.calendar.show_declined_events = !settings.calendar.show_declined_events
            }),
            Action::ToggleWeekends => self.account.update_settings(|settings| {
                settings.calendar.show_weekends = !settings.calendar.show_weekends
            }),
            Action::SeeMessage(thread) => self.open_thread(thread),
            Action::OpenWeek => {
                let date = self.calendar.day.map_or(today, |day| day.date);
                self.calendar_open_week(date);
            }
            Action::WeekStep(weeks) => {
                if let Some(week) = &mut self.calendar.week {
                    week.go_to(week.active + (7 * weeks).days());
                }
            }
            Action::WeekToday => {
                if let Some(week) = &mut self.calendar.week {
                    week.go_to(today);
                }
            }
            Action::CloseWeek => self.calendar.week = None,
            Action::OpenJumpToDate => {
                let now = self.now.clone();
                self.commands.open_sub_palette(jump::palette(now));
            }
            Action::JumpTo(date) => match &mut self.calendar.week {
                Some(week) => week.go_to(date),
                None => {
                    self.calendar_open_day(date, Source::Other);
                }
            },
            Action::DetailsEscape => match &mut self.calendar.details {
                Some(details) if details.alert.is_some() => details.alert = None,
                Some(details) if details.menu_open() => details.close_menu(),
                _ => self.calendar.details = None,
            },
            Action::DetailsConfirm => {
                if let Some(alert) = self
                    .calendar
                    .details
                    .as_mut()
                    .and_then(|details| details.alert.take())
                {
                    self.calendar_respond(alert.answer, alert.choice == 1);
                }
            }
            Action::Create => {
                let now = self.now.clone();
                self.calendar_new_event(&now);
            }
            Action::Save => match &mut self.calendar.form {
                Some(form) if form.custom.is_some() => form.finish_custom(),
                _ => {
                    let now = self.now.clone();
                    self.calendar_save(&now, false, false);
                }
            },
            Action::Discard => self.calendar_discard(),
            Action::FormEscape => match &mut self.calendar.form {
                Some(form) if form.custom.is_some() => form.escape_custom(),
                Some(form) if form.has_dropdown() => form.close_dropdown(),
                _ => self.calendar_discard(),
            },
            Action::FormStep(steps) => {
                let days = if self.calendar.week.is_some() {
                    7 * steps
                } else {
                    steps
                };
                self.calendar_form_shift(|start| start.checked_add(days.days()).ok());
            }
            Action::FormToday => {
                self.calendar_form_shift(|start| start.with().date(today).build().ok())
            }
            Action::FormFocus(field) => {
                if let Some(form) = &mut self.calendar.form {
                    form.focus(field);
                }
            }
            Action::FormAllDay => {
                if let Some(form) = &mut self.calendar.form {
                    form.toggle_all_day();
                }
            }
            Action::FormFormat(command) => {
                if let Some(form) = &mut self.calendar.form {
                    command.run(form.about_mut());
                }
            }
            Action::FormMeet => {
                if let Some(form) = &mut self.calendar.form {
                    form.values.meeting = if form.values.meeting == Meeting::None {
                        Meeting::NewMeet
                    } else {
                        Meeting::None
                    };
                }
            }
            Action::FormCopyLink => {
                let calendars = self.calendar_writable();
                if let Some(link) = self
                    .calendar
                    .form
                    .as_ref()
                    .and_then(|form| form.meeting_link(&calendars, self.account.email()))
                {
                    self.commands
                        .hint(Hint::new().text(format!("\"{link}\" copied to clipboard.")));
                    self.ctx.copy_text(link);
                }
            }
            Action::FormExternal => self.calendar_open_external(),
            Action::Prompt(index) => {
                let now = self.now.clone();
                self.calendar_prompt_action(index, &now);
            }
            Action::PromptSelect(delta) => self.calendar_prompt_select(delta),
            Action::PromptChoose(delta) => self.calendar_prompt_choose(delta),
            Action::Undo(token, undone) => self.calendar_undo(token, undone),
            Action::InviteFocus => {
                if let Some(thread) = &mut self.thread {
                    thread.focus_invite();
                }
            }
            Action::InviteMove(delta) => {
                if let Some(thread) = &mut self.thread {
                    thread.move_invite_focus(delta);
                }
            }
            Action::InviteBlur => {
                if let Some(thread) = &mut self.thread {
                    thread.blur_invite();
                }
            }
            Action::InviteSubmit => {
                if let Some((invite, response)) = self
                    .thread
                    .as_mut()
                    .and_then(crate::thread::ThreadView::submit_invite)
                {
                    self.calendar_respond_to_invite(invite, response, false);
                }
            }
            Action::MeetFocus => {
                if let Some(week) = &mut self.calendar.week {
                    week.meet.focus();
                }
            }
            // Calendar Settings opens the day view on today too.
            Action::OpenSettings => {
                if self.calendar.week.is_none() && self.calendar.day.is_none() {
                    self.calendar_open_day(today, Source::Other);
                }
                self.calendar.options_open = true;
            }
            Action::CloseSettings => self.calendar.options_open = false,
            Action::MeetBlur => {
                if let Some(week) = &mut self.calendar.week {
                    week.meet.blur();
                }
            }
            Action::FindTime => {
                let now = self.now.clone();
                self.calendar_meet(meet::MeetAction::Find, &now);
            }
            Action::CancelFindTime => {
                let now = self.now.clone();
                self.calendar_meet(meet::MeetAction::Cancel, &now);
            }
            Action::ShowAvailability(date) => {
                if let Some(week) = &mut self.calendar.week {
                    week.go_to(date);
                }
                self.commands.toast(Toast::new(format!(
                    "Showing availability from {} {}",
                    date.strftime("%b"),
                    date.day()
                )));
            }
            Action::ShareAvailability => {
                let now = self.now.clone();
                let target = self.calendar_share_target();
                self.calendar_share(target, &now);
            }
            Action::ShareInsert => {
                let now = self.now.clone();
                self.calendar_share_insert(&now);
            }
            Action::ShareCancel => self.calendar.week = None,
            Action::ShareEscape => {
                if self
                    .calendar
                    .week
                    .as_mut()
                    .and_then(|week| week.share.as_mut())
                    .is_some_and(share::Share::escape)
                {
                    self.calendar.week = None;
                }
            }
            Action::PickerKey(key) => self.calendar_picker_key(key),
            Action::CycleFocus => self.calendar_cycle_focus(),
            Action::CancelDrag => {
                self.calendar.grid.cancel_drag();
                self.calendar.form_grid.cancel_drag();
                if let Some(week) = &mut self.calendar.week {
                    week.cancel_drag();
                }
            }
        }
    }

    /// Moves the form's draft by `shift` of its start, keeping its length, and the calendar
    /// with it.
    pub(super) fn calendar_form_shift(&mut self, shift: impl Fn(&Zoned) -> Option<Zoned>) {
        let Some(form) = &mut self.calendar.form else {
            return;
        };
        let Some((start, end)) = form.values.when.clone() else {
            return;
        };
        let Some(moved) = shift(&start) else { return };
        let length = end.timestamp().duration_since(start.timestamp());
        let Ok(end) = moved.checked_add(length) else {
            return;
        };
        let date = moved.date();
        form.values.when = Some((moved, end));
        if let Some(week) = &mut self.calendar.week {
            week.go_to(date);
        } else if self.calendar.form_day.is_some() {
            self.calendar.form_day = Some(date);
        }
    }

    /// Keeps the calendar's data and the right pane in step with the app, once a frame: the right
    /// pane following the open thread's invite, the account's calendars switched on the first time
    /// they're listed, the days around the day view kept fresh, and the open thread's invite cards
    /// showing the calendar's answers.
    pub(crate) fn calendar_frame(&mut self, ctx: &egui::Context) {
        // A click anywhere takes the keyboard back from an invite card.
        if ctx.input(|input| input.pointer.any_pressed())
            && let Some(thread) = &mut self.thread
        {
            thread.blur_invite();
        }
        let now = self.now.clone();
        self.calendar_accounts_frame();
        self.calendar_follow_context(&now);
        self.calendar_read_answers();
        let data = self.account.calendar().data();
        if let Some(calendars) = &data.calendars {
            self.calendar_enable_by_default(calendars);
        }
        let center = self
            .calendar_week()
            .or(self.calendar_day())
            .unwrap_or(now.date());
        let window = self.calendar_window(center, &now);
        if self.calendar.window.as_ref() != Some(&window) {
            self.calendar.window = Some(window.clone());
            self.account.calendar().show(window);
        }
    }

    /// The days the week view around `active` shows: its week, without the weekend unless
    /// Calendar Settings shows it or the week is picking times to share.
    pub fn calendar_week_days(&self, active: Date) -> Vec<Date> {
        let start = self.calendar_week_start(active);
        let weekends = self.account.settings().calendar.show_weekends
            || self
                .calendar
                .week
                .as_ref()
                .is_some_and(|week| week.share.is_some());
        (0..7)
            .map(|offset| start + offset.days())
            .filter(|day| weekends || day.weekday().to_monday_one_offset() <= 5)
            .collect()
    }

    /// The week's secondary time zones, while Calendar Settings shows them.
    pub(super) fn calendar_secondary_zones(&self) -> Vec<jiff::tz::TimeZone> {
        let calendar = &self.account.settings().calendar;
        if !calendar.show_additional_timezones {
            return Vec::new();
        }
        calendar
            .additional_timezones
            .iter()
            .filter_map(|zone| jiff::tz::TimeZone::get(&zone.iana_id).ok())
            .collect()
    }

    /// The first day of `day`'s week, as Calendar Settings' `Start week on Monday` says.
    pub(super) fn calendar_week_start(&self, day: Date) -> Date {
        week_start(day, self.account.settings().calendar.week_starts_on == 1)
    }

    /// The weekday weeks start on, for the date pickers' months.
    pub(super) fn calendar_week_start_day(&self) -> jiff::civil::Weekday {
        if self.account.settings().calendar.week_starts_on == 1 {
            jiff::civil::Weekday::Monday
        } else {
            jiff::civil::Weekday::Sunday
        }
    }

    /// The days around `center` the data keeps: the week before its week to two weeks after.
    fn calendar_window(&self, center: Date, now: &Zoned) -> Window {
        let settings = self.account.settings();
        let week_start = self.calendar_week_start(center);
        let (shown, synced) = (
            settings.calendars_where(|calendar| calendar.show),
            settings.calendars_where(|calendar| calendar.sync),
        );
        Window {
            days: week_start - 7.days()..week_start + 14.days(),
            shown,
            synced,
            tz: now.time_zone().clone(),
        }
    }

    /// The events the data holds, as the calendar shows them: declined ones only when the
    /// settings show them, or when they're `invite` or the one whose details are open.
    pub(super) fn calendar_events(&self, now: &Zoned, invite: Option<&str>) -> Vec<Event> {
        let open = self
            .calendar
            .details
            .as_ref()
            .map(|details| details.key.as_str());
        let data = self.account.calendar().data();
        let settings = self.account.settings();
        let email = self.account.email();
        let colors = event::colors(settings);
        let lists: BTreeMap<&str, &AccountCalendar> = data
            .calendars
            .iter()
            .flatten()
            .filter_map(|calendar| Some((calendar.entry.id.as_deref()?, calendar)))
            .collect();
        data.events
            .iter()
            .filter_map(|stored| {
                // Me on another account's calendar is that account.
                let list = lists.get(stored.calendar_id.as_str()).copied();
                Event::new(
                    stored,
                    list.map(|calendar| &calendar.entry),
                    &colors,
                    list.map_or(email, |calendar| calendar.account.as_str()),
                    now.time_zone(),
                )
            })
            .filter(|event| {
                event.shown(settings.calendar.show_declined_events)
                    || (invite.is_some() && event.ical_uid.as_deref() == invite)
                    || open == Some(event.key.as_str())
            })
            .collect()
    }

    /// ⌘D with the right pane's form open. The keyboard goes from the mail to the form (the field
    /// it left), from the form to a popped-out draft if there is one or else back to the mail, and
    /// from the draft to the mail.
    fn calendar_cycle_focus(&mut self) {
        let popout = self.popout_focus();
        let ctx = self.ctx.clone();
        let Some(form) = self.calendar.form.as_mut() else {
            return;
        };
        match popout {
            Some(true) => self.focus_popout(false),
            _ if form.away => form.take_keyboard(),
            Some(false) => {
                form.give_up_keyboard(&ctx);
                self.focus_popout(true);
            }
            None => form.give_up_keyboard(&ctx),
        }
    }
}

/// The first day of `day`'s week, a Sunday or with `monday` a Monday.
fn week_start(day: Date, monday: bool) -> Date {
    let offset = if monday {
        day.weekday().to_monday_zero_offset()
    } else {
        day.weekday().to_sunday_zero_offset()
    };
    day - i64::from(offset).days()
}

/// The week from `first` as the tab's title names it: `Nov 16–22`, or `Nov 30–Dec 6` across two
/// months. It names the week the view shows, from whichever day it starts on
/// ([`crate::Mail::calendar_week_start`]).
pub(crate) fn week_title(first: Date) -> String {
    let last = first + 6.days();
    if first.month() == last.month() {
        format!("{} {}–{}", first.strftime("%b"), first.day(), last.day())
    } else {
        format!(
            "{} {}–{} {}",
            first.strftime("%b"),
            first.day(),
            last.strftime("%b"),
            last.day()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_starts_on_sunday_or_monday() {
        let tuesday = Date::constant(2026, 11, 17);
        assert_eq!(week_start(tuesday, false), Date::constant(2026, 11, 15));
        assert_eq!(week_start(tuesday, true), Date::constant(2026, 11, 16));
        assert_eq!(
            week_start(Date::constant(2026, 11, 15), true),
            Date::constant(2026, 11, 9),
            "a Sunday is its week's last day"
        );
    }

    #[test]
    fn the_tab_names_the_week_shown() {
        assert_eq!(week_title(Date::constant(2026, 11, 15)), "Nov 15–21");
        assert_eq!(
            week_title(Date::constant(2026, 11, 16)),
            "Nov 16–22",
            "from a Monday"
        );
        assert_eq!(
            week_title(Date::constant(2026, 11, 29)),
            "Nov 29–Dec 5",
            "across two months"
        );
        assert_eq!(
            week_title(Date::constant(2026, 12, 27)),
            "Dec 27–Jan 2",
            "and two years"
        );
    }
}
