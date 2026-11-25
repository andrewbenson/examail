//! Data model types for the event form.

use std::collections::BTreeMap;

use egui::Id;
use exo_store::Address;
use exo_ui::CalendarColor;
use gmail::calendar::Event as ApiEvent;
use jiff::{Zoned, civil::Weekday};

use crate::{
    calendar::{guests::Marker, places::Place},
    compose::contacts::Index,
};

/// Creating or editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Create,
    Edit,
}

/// A part of the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Title,
    Guests,
    When,
    /// A new event's rule.
    Repeat,
    Meeting,
    Location,
    About,
    Calendar,
}

impl Field {
    /// The fields under the title, in the mode's order.
    pub(super) fn rows(mode: Mode) -> &'static [Field] {
        match mode {
            Mode::Create => &[
                Field::Guests,
                Field::When,
                Field::Repeat,
                Field::Meeting,
                Field::Location,
                Field::About,
                Field::Calendar,
            ],
            Mode::Edit => &[
                Field::When,
                Field::Meeting,
                Field::Location,
                Field::Guests,
                Field::About,
            ],
        }
    }

    /// How far under the row's top its icon starts. That's its label's nudge (Who's and the
    /// meeting link's `top: 1px`, About's `top: 3px`, the calendar's `top: 2px`), with the icon at
    /// the label's top, except Meet's, which is 3px lower in its own label.
    pub(super) fn icon_top(self) -> f32 {
        match self {
            Field::Guests => 1.0,
            Field::Meeting => 1.0 + 3.0,
            Field::About => 3.0,
            Field::Calendar => 2.0,
            Field::Title | Field::When | Field::Repeat | Field::Location => 0.0,
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            // Repeat's label is empty (`aria-hidden`), keeping the column.
            Field::Title | Field::Repeat => "",
            Field::Guests => "legacy/DEPRECATED_guest",
            Field::When => "legacy/DEPRECATED_snooze",
            Field::Meeting => "legacy/DEPRECATED_conference",
            Field::Location => "legacy/DEPRECATED_locationPin",
            Field::About => "legacy/DEPRECATED_description",
            Field::Calendar => "legacy/DEPRECATED_calendar",
        }
    }

    pub(super) fn id(self, ctx: &egui::Context) -> Id {
        exo_ui::id(ctx, ("calendar-form-field", self))
    }

    /// How far the row sits up into the gap above it (Repeat's `margin-top: -8px`, under When).
    pub(super) fn pulled_up(self) -> f32 {
        if self == Field::Repeat { 8.0 } else { 0.0 }
    }
}

/// A guest on the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guest {
    pub email: String,
    pub name: Option<String>,
    pub optional: bool,
    /// Their answer, as Calendar has it.
    pub response: Option<String>,
    /// The guest is the event's calendar (me), which isn't shown but is kept when saving.
    pub me: bool,
    pub organizer: bool,
}

/// The meeting link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Meeting {
    None,
    /// A Google Meet link, made when the event is saved.
    NewMeet,
    /// The event's own link.
    Existing(String),
}

/// What the form edits.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub title: String,
    /// Start and end, once picked.
    pub when: Option<(Zoned, Zoned)>,
    pub all_day: bool,
    pub guests: Vec<Guest>,
    pub location: String,
    pub about: String,
    pub meeting: Meeting,
    /// The calendar it goes on.
    pub calendar: String,
    /// A new event's rule. With none, it doesn't repeat.
    pub repeat: Option<exo_store::Recurrence>,
}

/// The event being edited.
#[derive(Clone, Debug)]
pub struct Edited {
    /// Its key on the calendar ([`crate::calendar::event::Event::key`]).
    pub key: String,
    /// Calendar's copy (an instance, for a recurring event).
    pub event: ApiEvent,
    pub color: CalendarColor,
}

/// A click on a copy tooltip. A guest's text composes to them (popped out with ⇧), the meeting
/// link's opens it, and the icon copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TipClick {
    Compose(Address, bool),
    Open(String),
    Copy(String),
}

/// What the form asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormAction {
    Save,
    Cancel,
}

/// A calendar the event can go on.
pub struct CalendarOption {
    /// The calendar account it's listed under.
    pub account: String,
    pub id: String,
    pub name: String,
    pub color: CalendarColor,
}

/// What the form shows besides itself.
pub struct Shown<'a> {
    pub calendars: &'a [CalendarOption],
    pub contacts: Option<&'a Index>,
    /// The account.
    pub me: &'a str,
    pub now: &'a Zoned,
    /// The day weeks start on, for the date pickers.
    pub week_start: Weekday,
    /// The people of the conversation open behind the form, me left out.
    pub conversation: &'a [Address],
    /// The places found for what's typed in Where.
    pub places: &'a [Place],
    /// Whether Exo is online, which a new Meet link waits for.
    pub online: bool,
    /// Each guest's marker and busy status (`(busy)`/`(OOO)`), by address.
    pub chips: &'a BTreeMap<String, (Marker, Option<&'static str>)>,
}
