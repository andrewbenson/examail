use exo_commands::{Command, Commands, Region};
use exo_editor::FormatCommand;

use super::{
    Action,
    form::{Field, Meeting, Mode},
    week::Week,
};
use crate::commands::Action as AppAction;

/// Where the calendar's keys work from the mail views.
const MAIL: &[Region] = &[
    Region::ThreadList,
    Region::ThreadMessagesHeader,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ActionPane,
    Region::ThreadListHeader,
];
/// Where the week view's keys work.
const WEEK: &[Region] = &[Region::SeeYourWeek, Region::SeeYourWeekSidebar];
/// Where the event form's keys work.
const FORM: &[Region] = &[Region::CalendarCreate];
/// Where an alert's keys work.
const ALERT: &[Region] = &[Region::Alert];

/// A calendar colour picker's rows: five colours to a row, of 25.
pub(crate) const PICKER_ROW: usize = 5;

/// Where a calendar colour picker has the keyboard.
pub(crate) const PICKER: &[Region] = &[Region::CalendarColorPicker];

/// The event form's state, for its palette rows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FormState {
    create: bool,
    all_day: bool,
    meet: bool,
    /// The event's own meeting link has the keyboard.
    link_focused: bool,
    /// While About has the keyboard, the formatting keys that apply to it, by their place in
    /// [`FormatCommand::ALL`], and whether the editor takes Escape and Tab.
    about: Option<(u16, bool, bool)>,
}

/// What the calendar registers besides its standing keys.
#[derive(Clone, Copy, Debug)]
pub(crate) struct State {
    form: Option<FormState>,
    /// An alert or dialog: how many actions, and the selected one.
    prompt: Option<(usize, usize)>,
    /// Calendar Settings is up.
    options: bool,
    /// The open thread's invite card: whether it has the keyboard, or can take it.
    invite: Option<bool>,
    /// The week view's Meet: whether it has guests, and whether Find Time is on.
    meet: Option<(bool, bool)>,
    /// The week view is up, and whether it's sharing availability.
    share: Option<bool>,
    dragging: bool,
    /// A calendar's colour picker is open.
    picking: bool,
    /// The week shows its weekends.
    weekends: bool,
}

impl crate::Mail {
    /// What the calendar registers this frame besides its standing keys.
    pub(crate) fn calendar_state(&self) -> State {
        State {
            form: self.calendar.form.as_ref().map(|form| {
                let about = form.about_focused().then(|| {
                    let editor = form.about();
                    let formats = FormatCommand::ALL
                        .iter()
                        .enumerate()
                        .filter(|(_, command)| command.applies(editor))
                        .fold(0u16, |mask, (at, _)| mask | 1 << at);
                    (formats, editor.wants_escape(), editor.wants_tab())
                });
                FormState {
                    create: form.mode == Mode::Create,
                    all_day: form.values.all_day,
                    meet: form.values.meeting == Meeting::NewMeet,
                    link_focused: form.meeting_link_focused(),
                    about,
                }
            }),
            prompt: self.calendar_prompt_actions(),
            options: self.calendar.options_open,
            invite: self.thread.as_ref().and_then(|thread| {
                if thread.invite_focused() {
                    Some(true)
                } else {
                    thread.can_focus_invite().then_some(false)
                }
            }),
            meet: self
                .calendar
                .week
                .as_ref()
                .filter(|week| week.share.is_none())
                .map(|week| (!week.meet.guests.is_empty(), week.meet.finding)),
            share: self.calendar.week.as_ref().map(|week| week.share.is_some()),
            dragging: self.calendar.grid.dragging()
                || self.calendar.form_grid.dragging()
                || self.calendar.week.as_ref().is_some_and(Week::dragging),
            picking: self
                .calendar
                .week
                .as_ref()
                .is_some_and(|week| week.picker.is_some()),
            weekends: self.account.settings().calendar.show_weekends,
        }
    }
}

/// ⌘D while the right pane's form is open, registered after compose's so it wins over the
/// pop-out's own.
pub(crate) fn register_focus_cycle(commands: &mut Commands<AppAction>) {
    const SECTIONS: &[Region] = &[
        Region::ThreadList,
        Region::ThreadListHeader,
        Region::ThreadMessagesHeader,
        Region::ThreadMessages,
        Region::ThreadMessagesNewMessages,
        Region::ThreadMessageCalendarInvite,
        Region::ActionPane,
        Region::SearchBar,
        Region::ComposeForm,
        Region::PopOutComposeForm,
        Region::RecipientList,
        Region::CalendarCreate,
    ];
    commands.add(
        Command::shortcut("CYCLE_APP_FOCUS", AppAction::Calendar(Action::CycleFocus))
            .key("mod+d")
            .regions(SECTIONS),
    );
}

/// A colour picker's keys while it has the keyboard in `regions` (every picker's: the
/// calendars', Notifications' and the Accounts dialog's). The arrows and Tab repeat while held,
/// but ⌘O / ⌘⇧O and ⌘ with an arrow don't.
pub(crate) fn register_picker_keys<A: Clone + 'static>(
    commands: &mut Commands<A>,
    regions: &'static [Region],
    action: impl Fn(exo_ui::PickerKey) -> A,
) {
    use exo_ui::PickerKey;
    let held = [
        (
            "CALENDAR_COLOR_PICKER_PREVIOUS",
            PickerKey::Previous,
            &["left", "shift+tab"][..],
        ),
        (
            "CALENDAR_COLOR_PICKER_NEXT",
            PickerKey::Next,
            &["right", "tab"],
        ),
        ("CALENDAR_COLOR_PICKER_UP", PickerKey::Up, &["up"]),
        ("CALENDAR_COLOR_PICKER_DOWN", PickerKey::Down, &["down"]),
    ];
    for (id, key, keys) in held {
        commands.add(
            Command::shortcut(id, action(key))
                .keys(keys.iter().copied())
                .repeat(action(key))
                .regions(regions),
        );
    }
    let pressed = [
        (
            "CALENDAR_COLOR_PICKER_CYCLE_BACK",
            PickerKey::Previous,
            &["mod+shift+o"][..],
        ),
        ("CALENDAR_COLOR_PICKER_CYCLE", PickerKey::Next, &["mod+o"]),
        (
            "CALENDAR_COLOR_PICKER_FIRST",
            PickerKey::First,
            &["mod+left", "mod+up"],
        ),
        (
            "CALENDAR_COLOR_PICKER_LAST",
            PickerKey::Last,
            &["mod+right", "mod+down"],
        ),
        ("CALENDAR_COLOR_PICKER_ESCAPE", PickerKey::Escape, &["esc"]),
        (
            "CALENDAR_COLOR_PICKER_ENTER",
            PickerKey::Enter,
            &["enter", "space"],
        ),
    ];
    for (id, key, keys) in pressed {
        commands.add(
            Command::shortcut(id, action(key))
                .keys(keys.iter().copied())
                .regions(regions),
        );
    }
}

/// Registers the calendar's keys and palette rows for this frame.
pub(crate) fn register(
    commands: &mut Commands<AppAction>,
    day_open: bool,
    week_open: bool,
    show_declined: bool,
    compose: bool,
    details: Option<bool>,
    state: State,
) {
    let action = AppAction::Calendar;
    if state.share == Some(true) {
        register_share(commands);
        if details.is_some() {
            commands.add(
                Command::shortcut("EVENT_DETAILS_CLOSE", action(Action::DetailsEscape)).key("esc"),
            );
        }
        return;
    }
    if week_open {
        register_week(commands, details.is_some());
    }
    // `Share Availability`: the list's and a conversation's row, and the week's, with its key,
    // while it's doing nothing else.
    if state.share == Some(false)
        && state.form.is_none()
        && !state.meet.is_some_and(|(_, finding)| finding)
    {
        commands.add(
            share_availability_command(action(Action::ShareAvailability))
                .key("mod+shift+a")
                .key_code(["⌘", "shift", "a"])
                .regions(WEEK),
        );
    } else if state.share.is_none() {
        commands.add(share_availability_command(action(Action::ShareAvailability)).regions(MAIL));
    }
    // `b` / `shift+b`: a new event's form (the instant event needs AI, which Exo leaves out).
    commands.add(
        Command::shortcut("CALENDAR_CREATE_EVENT", action(Action::Create))
            .keys(["b", "shift+b"])
            .regions(MAIL),
    );
    if state.form.is_none() {
        commands.add(
            Command::new("CREATE_EVENT", "Create Empty Event", action(Action::Create))
                .aliases([
                    "Empty Event",
                    "Create Empty Calendar Event",
                    "New Empty Event",
                    "Add Empty Event",
                    "Create Meeting",
                    "Send Invitation",
                ])
                .operative_word("event")
                .key_code(["B"])
                .default_score(1.3)
                .scale(0.99)
                .icon("data/calendar"),
        );
    }
    if let Some(form) = state.form {
        register_form(commands, form);
    }
    commands.add(
        Command::new(
            "CALENDAR_SETTINGS",
            "Calendar Settings",
            action(Action::OpenSettings),
        )
        .aliases([
            "Multiple Calendars",
            "Add Calendar",
            "Add Calendar Account",
            "Timezone Settings",
            "Meeting Link Settings",
            "Zoom Links",
            "Google Meet Links",
            "Conference Links",
            "Conference Calls",
            "Video Calls",
            "Event Settings",
        ])
        .icon("data/calendar"),
    );
    if state.options {
        commands.add(
            Command::shortcut("CALENDAR_SETTINGS_CLOSE", action(Action::CloseSettings))
                .key("esc")
                .regions(&[Region::Dialog]),
        );
    }
    if let Some((count, selected)) = state.prompt {
        crate::notice::register_alert_keys(
            commands,
            action(Action::Prompt(selected)),
            action(Action::Prompt(count - 1)),
            |step| action(Action::PromptSelect(step)),
        );
        commands.add(
            Command::shortcut("CALENDAR_ALERT_UP", action(Action::PromptChoose(-1)))
                .key("up")
                .regions(ALERT),
        );
        commands.add(
            Command::shortcut("CALENDAR_ALERT_DOWN", action(Action::PromptChoose(1)))
                .key("down")
                .regions(ALERT),
        );
    }
    match state.invite {
        // With no draft to go to, an invite card takes the keyboard.
        Some(false) => commands.add(
            Command::shortcut("FOCUS_CALENDAR_INVITE", action(Action::InviteFocus))
                .key("tab")
                .regions(&[Region::ThreadMessages]),
        ),
        // The invite card's own keys.
        Some(true) => {
            const INVITE: &[Region] = &[Region::ThreadMessageCalendarInvite];
            commands.add(
                Command::shortcut("CALENDAR_INVITE_SUBMIT", action(Action::InviteSubmit))
                    .keys(["enter", "space"])
                    .regions(INVITE),
            );
            commands.add(
                Command::shortcut("CALENDAR_INVITE_ESCAPE", action(Action::InviteBlur))
                    .key("esc")
                    .regions(INVITE),
            );
            commands.add(
                Command::shortcut("CALENDAR_INVITE_NEXT", action(Action::InviteMove(1)))
                    .keys(["right", "down", "ctralt+j", "ctralt+n", "tab"])
                    .regions(INVITE),
            );
            commands.add(
                Command::shortcut("CALENDAR_INVITE_PREVIOUS", action(Action::InviteMove(-1)))
                    .keys(["left", "up", "ctralt+k", "ctralt+p"])
                    .regions(INVITE),
            );
        }
        None => {}
    }
    if let Some((guests, finding)) = state.meet.filter(|_| state.form.is_none()) {
        commands.add(
            Command::shortcut("MEET_WITH_FOCUS", action(Action::MeetFocus))
                .key("m")
                .regions(WEEK),
        );
        commands.add(
            Command::shortcut("MEET_WITH_INPUT_ESCAPE", action(Action::MeetBlur))
                .key("esc")
                .regions(&[Region::SeeYourWeekSidebarMeetWithInput]),
        );
        commands.add(
            Command::new("MEET_WITH", "Meet With", action(Action::MeetFocus))
                .key_code(["M"])
                .icon("data/calendar"),
        );
        if finding {
            // Over the week's own Escape.
            commands.add(
                Command::shortcut("FIND_TIME_ESCAPE", action(Action::CancelFindTime))
                    .key("esc")
                    .regions(WEEK),
            );
            commands.add(
                Command::new(
                    "FIND_TIME",
                    "Cancel Find Time",
                    action(Action::CancelFindTime),
                )
                .icon("data/calendar"),
            );
        } else if guests {
            commands.add(
                Command::new("FIND_TIME", "Find Time", action(Action::FindTime))
                    .icon("data/calendar"),
            );
        }
    }
    if state.picking {
        register_picker_keys(commands, PICKER, |key| action(Action::PickerKey(key)));
    }
    if state.dragging {
        // A drag's Escape, over everything else.
        commands
            .add(Command::shortcut("CALENDAR_DRAG_CANCEL", action(Action::CancelDrag)).key("esc"));
    }
    commands.add(
        Command::shortcut("OPEN_WEEK", action(Action::OpenWeek))
            .key("2")
            .regions(MAIL),
    );
    if !week_open {
        commands.add(
            Command::new("SEE_YOUR_WEEK", "Open Week", action(Action::OpenWeek))
                .aliases([
                    "Open Calendar",
                    "Open Weekly Calendar",
                    "Week View",
                    "See My Week",
                    "See Your Week",
                    "Open Calendar Week",
                ])
                .key_code(if compose { Vec::new() } else { vec!["2"] })
                .scale(1.0)
                .follow("CALENDAR_DAY")
                .icon("data/calendar"),
        );
    }
    commands.add(
        Command::shortcut("PREVIOUS_DAY", action(Action::Step(-1)))
            .key("-")
            .regions(MAIL),
    );
    commands.add(
        Command::shortcut("NEXT_DAY", action(Action::Step(1)))
            .keys(["=", "plus"])
            .regions(MAIL),
    );
    commands.add(
        Command::shortcut("CYCLE_RIGHT_PANE", action(Action::Cycle))
            .key("0")
            .regions(MAIL),
    );
    commands.add(
        Command::shortcut("CALENDAR_TODAY", action(Action::Today))
            .key("t")
            .regions(MAIL),
    );
    // Anywhere, composing included (`mod+shift+` on a Mac).
    commands.add(
        Command::shortcut("PREVIOUS_DAY_ANYWHERE", action(Action::Step(-1))).key("mod+shift+-"),
    );
    commands
        .add(Command::shortcut("NEXT_DAY_ANYWHERE", action(Action::Step(1))).key("mod+shift+="));
    commands.add(
        Command::shortcut("CYCLE_RIGHT_PANE_ANYWHERE", action(Action::Cycle)).key("mod+shift+0"),
    );
    let zero: &[&str] = if compose {
        &["⌘", "shift", "0"]
    } else {
        &["0"]
    };
    if day_open || week_open {
        commands.add(
            Command::new(
                "CHANGE_CALENDAR_DAY",
                "Jump to Date",
                action(Action::OpenJumpToDate),
            )
            .aliases(["Go to Date", "Change day"])
            .icon("data/calendar"),
        );
        commands.add(
            Command::new("CLOSE_CALENDAR", "Close Calendar", action(Action::Close))
                .key_code(if day_open { zero.to_vec() } else { vec!["1"] })
                .default_score(1.35)
                .icon("data/calendar"),
        );
    }
    if !day_open && !week_open {
        commands.add(
            Command::new("CALENDAR_DAY", "Open Day", action(Action::OpenDay))
                .aliases([
                    "Open Daily Calendar",
                    "Day View",
                    "See My Day",
                    "See Your Day",
                ])
                .key_code(zero.iter().copied())
                .default_score(1.35)
                .scale(0.5)
                .icon("data/calendar"),
        );
    }
    // `Show Weekends` / `Hide Weekends`, in the week view.
    if week_open {
        let title = if state.weekends {
            "Hide Weekends"
        } else {
            "Show Weekends"
        };
        commands.add(
            Command::new(
                "TOGGLE_CALENDAR_SHOW_WEEKENDS",
                title,
                action(Action::ToggleWeekends),
            )
            .aliases(["Show Weekends", "Hide Weekends"])
            .exclude_alias_from_title(["Show Weekends", "Hide Weekends"])
            .operative_word("weekends")
            .scale(1.01)
            .icon("data/calendar"),
        );
    }
    let title = if show_declined {
        "Hide Declined Events"
    } else {
        "Show Declined Events"
    };
    commands.add(
        Command::new(
            "TOGGLE_SHOW_DECLINED_EVENTS",
            title,
            action(Action::ToggleDeclined),
        )
        .aliases(["Show Declined Events", "Hide Declined Events"])
        .exclude_alias_from_title(["Show Declined Events", "Hide Declined Events"])
        .operative_word("declined")
        .scale(1.0)
        .follow("SEE_YOUR_WEEK")
        .icon("data/calendar"),
    );
    // The event details' own keys, registered last to win. Escape closes the alert, the menu or
    // the details, and Enter is the alert's OK.
    if let Some(alert) = details {
        commands.add(
            Command::shortcut("EVENT_DETAILS_CLOSE", action(Action::DetailsEscape)).key("esc"),
        );
        if alert {
            commands.add(
                Command::shortcut("EVENT_DETAILS_CONFIRM", action(Action::DetailsConfirm))
                    .key("enter"),
            );
        }
    }
}

/// The form's keys and palette rows.
fn register_form(commands: &mut Commands<AppAction>, form: FormState) {
    let action = AppAction::Calendar;
    commands.add(
        Command::shortcut("CALENDAR_CREATE_SAVE_KEY", action(Action::Save))
            .key("mod+enter")
            .regions(FORM),
    );
    if !form.about.is_some_and(|(_, escape, _)| escape) {
        commands.add(
            Command::shortcut("CALENDAR_CREATE_ESCAPE", action(Action::FormEscape))
                .key("esc")
                .regions(FORM),
        );
    }
    // About's formatting keys, as compose's. Outside a list, Tab and ⇧Tab leave About (the editor
    // keeps them otherwise, and keeps Tab while it has ghost text or a chosen suggestion).
    let applies = |command: FormatCommand| {
        form.about.is_some_and(|(formats, _, _)| {
            FormatCommand::ALL
                .iter()
                .position(|each| *each == command)
                .is_some_and(|at| formats & 1 << at != 0)
        })
    };
    for command in FormatCommand::ALL
        .into_iter()
        .filter(|command| applies(*command))
    {
        commands.add(
            Command::shortcut(command.id(), action(Action::FormFormat(command)))
                .keys(command.keys().iter().copied())
                .regions(FORM),
        );
    }
    if form.about.is_some() && !applies(FormatCommand::IndentList) {
        if form.create && !form.about.is_some_and(|(_, _, tab)| tab) {
            commands.add(
                Command::shortcut(
                    "CALENDAR_CREATE_ABOUT_TAB",
                    action(Action::FormFocus(Field::Calendar)),
                )
                .key("tab")
                .regions(FORM),
            );
        }
        commands.add(
            Command::shortcut(
                "CALENDAR_CREATE_ABOUT_SHIFT_TAB",
                action(Action::FormFocus(if form.create {
                    Field::Location
                } else {
                    Field::Guests
                })),
            )
            .key("shift+tab")
            .regions(FORM),
        );
    }
    if form.link_focused {
        commands.add(
            Command::shortcut("CALENDAR_CREATE_COPY_LINK", action(Action::FormCopyLink))
                .key("mod+c")
                .regions(FORM),
        );
    }
    commands.add(
        Command::shortcut("CALENDAR_CREATE_DISCARD", action(Action::Discard))
            .keys(["mod+shift+,", "mod+shift+."])
            .regions(FORM),
    );
    commands.add(
        Command::shortcut("CALENDAR_CREATE_NEXT", action(Action::FormStep(1)))
            .key("mod+shift+=")
            .regions(FORM),
    );
    commands.add(
        Command::shortcut("CALENDAR_CREATE_PREVIOUS", action(Action::FormStep(-1)))
            .key("mod+shift+-")
            .regions(FORM),
    );
    commands.add(
        Command::shortcut("CALENDAR_CREATE_TODAY", action(Action::FormToday))
            .key("mod+shift+0")
            .regions(FORM),
    );
    let rows: [(&str, &str, &[&str], Field, &str); 5] = [
        (
            "CALENDAR_CREATE_LOCATION",
            "Edit Location",
            &["Edit Place", "Edit Where"],
            Field::Location,
            "data/location",
        ),
        (
            "CALENDAR_CREATE_DESCRIPTION",
            "Edit Description",
            &["Edit About", "Edit Agenda"],
            Field::About,
            "data/description",
        ),
        (
            "CALENDAR_CREATE_ATTENDEES",
            "Edit Guests",
            &["Edit Who", "Add Guest", "Invite", "Edit Attendees"],
            Field::Guests,
            "data/edit",
        ),
        (
            "CALENDAR_CREATE_WHEN",
            "Edit Date & Time",
            &["Edit Date", "Edit Time", "Edit When"],
            Field::When,
            "data/edit",
        ),
        (
            "CALENDAR_CREATE_TITLE",
            "Edit Title",
            &["Edit Name"],
            Field::Title,
            "data/edit",
        ),
    ];
    for (id, title, aliases, field, icon) in rows {
        commands.add(
            Command::new(id, title, action(Action::FormFocus(field)))
                .aliases(aliases.iter().copied())
                .default_score(20.0)
                .icon(icon),
        );
    }
    if form.create {
        commands.add(
            Command::new(
                "CALENDAR_CREATE_CALENDAR",
                "Change Calendar",
                action(Action::FormFocus(Field::Calendar)),
            )
            .aliases(["Change Account", "Edit Calendar"])
            .default_score(20.0)
            .icon("data/edit"),
        );
    }
    commands.add(
        Command::new("CALENDAR_CREATE_SAVE", "Save Event", action(Action::Save))
            .aliases(["Create Event"])
            .key_code(["⌘", "Enter"])
            .default_score(20.0)
            .scale(5.0)
            .icon("data/done"),
    );
    let discard = if form.create {
        "Discard Event"
    } else {
        "Discard Changes"
    };
    commands.add(
        Command::new("CALENDAR_CREATE_CANCEL", discard, action(Action::Discard))
            .aliases(["Discard", "Close", "Cancel"])
            .key_code(["⌘", "shift", ","])
            .default_score(20.0)
            .scale(5.0)
            .icon("data/discard"),
    );
    let all_day = if form.all_day {
        "Remove all day"
    } else {
        "Make all day"
    };
    commands.add(
        Command::new("EDIT_CALENDAR_ALL_DAY", all_day, action(Action::FormAllDay))
            .aliases([
                "Make all day",
                "Remove all day",
                "Make full day",
                "Remove full day",
                "Toggle all day",
            ])
            .exclude_alias_from_title(["Make all day", "Remove all day"])
            .default_score(20.0)
            .icon("data/calendar"),
    );
    let verb = if form.meet { "Remove" } else { "Add" };
    commands.add(
        Command::new(
            "CALENDAR_CREATE_CONFERENCE_MEET",
            format!("{verb} Google Meet Link"),
            action(Action::FormMeet),
        )
        .aliases(
            [
                "Google Meet",
                "Google Video",
                "Google Hangouts",
                "G.co",
                "GMeet",
            ]
            .map(|name| format!("{verb} {name}")),
        )
        .icon("data/g-suite"),
    );
    // What only Google Calendar's own editor does: it opens with the form's event.
    if form.create {
        let external: [(&str, &str, &[&str]); 11] = [
            ("EDIT_CALENDAR_EVENT", "Edit event in Google Calendar", &[]),
            ("EDIT_CALENDAR_PERMISSIONS", "Change Guest Permissions", &[]),
            (
                "EDIT_CALENDAR_VISIBILITY",
                "Change Visibility",
                &["Change Event Privacy"],
            ),
            ("EDIT_CALENDAR_BUSY", "Change Busy/Free", &[]),
            ("EDIT_CALENDAR_COLOR", "Change Calendar Color", &[]),
            (
                "EDIT_CALENDAR_NOTIFICATIONS",
                "Add Notification",
                &["Add Reminder"],
            ),
            ("EDIT_CALENDAR_CONFERENCE", "Add Conference Room", &[]),
            ("EDIT_CALENDAR_RECURRING", "Make Recurring", &[]),
            ("EDIT_CALENDAR_MORE", "More Calendar Actions", &[]),
            ("EDIT_CALENDAR_TZ", "Change Time Zone", &[]),
            ("EDIT_CALENDAR_REPEAT", "Repeat Event", &[]),
        ];
        for (id, title, aliases) in external {
            commands.add(
                Command::new(id, title, action(Action::FormExternal))
                    .aliases(aliases.iter().copied())
                    .default_score(0.0)
                    .icon("data/calendar"),
            );
        }
    }
}

/// `Share Availability`'s palette row.
pub(crate) fn share_availability_command(action: AppAction) -> Command<AppAction> {
    Command::new("SHARE_AVAILABILITY", "Share Availability", action)
        .aliases([
            "Share Time",
            "Share Time Slots",
            "When Am I Free",
            "Find Times",
            "Suggest Times",
            "Propose Times",
            "Schedule",
            "Insert Booking Link",
            "Insert Booking Page",
            "Insert Calendar Link",
            "Insert Scheduling Link",
            "Insert Meeting Link",
            "Insert Meeting Times",
            "Insert Free Times",
            "Insert Availability",
            "Insert One-Time Link",
            "Insert One-Off Link",
            "Insert Single-Use Link",
            "Insert Recurring Link",
            "Calendly",
        ])
        .exclude_alias_from_title(["Calendly", "Find Times", "Schedule"])
        .default_score(1.4)
        .icon("data/upgrade-calendar")
}

/// Share Availability's keys: the week's, Enter and ⌘Enter to insert, Escape to cancel, and in
/// its fields ⌘Enter, ⌘⇧= / ⌘⇧– and Escape.
fn register_share(commands: &mut Commands<AppAction>) {
    const PAGE: &[Region] = &[Region::ShareAvailability];
    const FIELDS: &[Region] = &[
        Region::ShareAvailabilitySidebar,
        Region::ShareAvailabilitySidebarAttendees,
        Region::ShareAvailabilitySidebarTimezoneInput,
    ];
    const ALL: &[Region] = &[
        Region::ShareAvailability,
        Region::ShareAvailabilitySidebar,
        Region::ShareAvailabilitySidebarAttendees,
        Region::ShareAvailabilitySidebarTimezoneInput,
    ];
    let action = AppAction::Calendar;
    let next = action(Action::WeekStep(1));
    let previous = action(Action::WeekStep(-1));
    commands.add(
        Command::shortcut("SHARE_NEXT", next.clone())
            .keys(["=", "shift+=", "plus", "j", "n", "right"])
            .repeat(next.clone())
            .regions(PAGE),
    );
    commands.add(
        Command::shortcut("SHARE_PREVIOUS", previous.clone())
            .keys(["-", "shift+-", "k", "p", "left"])
            .repeat(previous.clone())
            .regions(PAGE),
    );
    commands.add(
        Command::shortcut("SHARE_NEXT_ANYWHERE", next.clone())
            .key("mod+shift+=")
            .repeat(next)
            .regions(ALL),
    );
    commands.add(
        Command::shortcut("SHARE_PREVIOUS_ANYWHERE", previous.clone())
            .key("mod+shift+-")
            .repeat(previous)
            .regions(ALL),
    );
    commands.add(
        Command::shortcut("SHARE_TODAY", action(Action::WeekToday))
            .keys(["space", "t"])
            .regions(PAGE),
    );
    commands.add(
        Command::shortcut("SHARE_CLOSE", action(Action::ShareCancel))
            .keys(["0", "mod+shift+0", "1", "esc"])
            .regions(PAGE),
    );
    commands.add(
        Command::shortcut("SHARE_INSERT", action(Action::ShareInsert))
            .keys(["enter", "mod+enter"])
            .regions(PAGE),
    );
    commands.add(
        Command::shortcut("SHARE_INSERT_FIELD", action(Action::ShareInsert))
            .key("mod+enter")
            .regions(FIELDS),
    );
    commands.add(
        Command::shortcut("SHARE_ESCAPE", action(Action::ShareEscape))
            .key("esc")
            .regions(FIELDS),
    );
}

/// The week view's keys. `b` waits while the details are up.
fn register_week(commands: &mut Commands<AppAction>, details: bool) {
    let action = AppAction::Calendar;
    if !details {
        commands.add(
            Command::shortcut("WEEK_CREATE_EVENT", action(Action::Create))
                .keys(["b", "shift+b"])
                .regions(WEEK),
        );
    }
    let next = action(Action::WeekStep(1));
    let previous = action(Action::WeekStep(-1));
    commands.add(
        Command::shortcut("WEEK_NEXT", next.clone())
            .keys(["=", "shift+=", "mod+shift+=", "plus", "j", "n", "right"])
            .repeat(next)
            .regions(WEEK),
    );
    commands.add(
        Command::shortcut("WEEK_PREVIOUS", previous.clone())
            .keys(["-", "shift+-", "mod+shift+-", "k", "p", "left"])
            .repeat(previous)
            .regions(WEEK),
    );
    commands.add(
        Command::shortcut("WEEK_TODAY", action(Action::WeekToday))
            .keys(["space", "t"])
            .regions(WEEK),
    );
    commands.add(
        Command::shortcut("WEEK_CLOSE", action(Action::CloseWeek))
            .keys(["0", "mod+shift+0", "1", "esc"])
            .regions(WEEK),
    );
}
