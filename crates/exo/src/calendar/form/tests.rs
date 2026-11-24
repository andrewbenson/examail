use exo_ui::CalendarColor;
use gmail::calendar::Event as ApiEvent;
use jiff::{Zoned, tz::TimeZone};

use super::{
    overlays::{CalendarRow, calendar_highlight, calendar_label, calendar_rows, calendar_step},
    *,
};

fn at(text: &str) -> Zoned {
    text.parse::<jiff::civil::DateTime>()
        .expect("time")
        .to_zoned(TimeZone::get("America/New_York").expect("zone"))
        .expect("exists")
}

fn values() -> Values {
    Values {
        title: String::new(),
        when: None,
        all_day: false,
        guests: Vec::new(),
        location: String::new(),
        about: String::new(),
        meeting: Meeting::None,
        calendar: "me".into(),
        repeat: None,
    }
}

#[test]
fn the_day_makes_room_once_when_s_list_reaches_it_until_when_is_left_or_emptied() {
    let mut form = EventForm::create(values(), false);
    form.focused = Some(Field::When);
    form.when.text = "fri".into();
    form.when.list_bottom = Some(300.0);
    assert!(!form.when_covers(320.0), "10px short");
    form.when.list_bottom = Some(312.0);
    assert!(form.when_covers(320.0));
    form.when.list_bottom = Some(200.0);
    assert!(form.when_covers(360.0), "it stays while typing");
    form.focused = Some(Field::Title);
    assert!(!form.when_covers(360.0), "leaving When ends it");
    form.focused = Some(Field::When);
    form.when.text.clear();
    assert!(!form.when_covers(360.0));
}

#[test]
fn typed_guests_need_addresses() {
    let mut form = EventForm::create(values(), true);
    form.guest_input = "lena@brightmoor.co, Jordan <jordan@calloway.com>".into();
    assert!(form.commit_guests());
    assert_eq!(
        form.values
            .guests
            .iter()
            .map(|guest| guest.email.as_str())
            .collect::<Vec<_>>(),
        ["lena@brightmoor.co", "jordan@calloway.com"]
    );
    form.guest_input = "not an address".into();
    assert!(!form.commit_guests());
    assert!(form.guest_error);
    assert!(!form.is_empty(), "guests make it worth asking");
}

#[test]
fn an_edit_knows_what_changed() {
    let mut values = values();
    values.when = Some((at("2026-11-17T15:00"), at("2026-11-17T16:00")));
    let edited = Edited {
        key: "k".into(),
        event: ApiEvent::default(),
        color: CalendarColor::BLUE,
    };
    let mut form = EventForm::edit(values, edited, true, Field::Title);
    assert!(form.is_empty());
    form.values.title = "Design crit".into();
    form.toggle_all_day();
    assert_eq!(form.dirty(), [Field::Title, Field::When]);
    assert_eq!(
        form.values.when,
        Some((at("2026-11-17T00:00"), at("2026-11-18T00:00")))
    );
}

/// The writable calendars of the mail account and a second calendar account, as the form
/// lists them.
fn calendars() -> Vec<CalendarOption> {
    let option = |account: &str, id: &str, name: &str| CalendarOption {
        account: account.into(),
        id: id.into(),
        name: name.into(),
        color: CalendarColor::BLUE,
    };
    vec![
        option(
            "maya@brightmoor.co",
            "maya@brightmoor.co",
            "maya@brightmoor.co",
        ),
        option("maya@brightmoor.co", "team", "Brightmoor Team"),
        option(
            "maya.lin@example.com",
            "maya.lin@example.com",
            "maya.lin@example.com",
        ),
    ]
}

#[test]
fn a_meet_link_opens_as_the_events_calendar_account() {
    let form = |link: &str, calendar: &str| {
        EventForm::create(
            Values {
                meeting: Meeting::Existing(link.into()),
                calendar: calendar.into(),
                ..values()
            },
            true,
        )
    };
    let meet = "https://meet.google.com/abc-defg-hij";
    assert_eq!(
        form(meet, "maya.lin@example.com")
            .meeting_link(&calendars(), "maya@brightmoor.co")
            .as_deref(),
        Some("https://meet.google.com/abc-defg-hij?authuser=maya.lin%40example.com")
    );
    assert_eq!(
        form("https://zoom.us/j/1", "team")
            .meeting_link(&calendars(), "maya@brightmoor.co")
            .as_deref(),
        Some("https://zoom.us/j/1"),
        "only Meet's links take the account"
    );
    assert_eq!(
        EventForm::create(values(), true).meeting_link(&calendars(), "maya@brightmoor.co"),
        None
    );
}

fn listed(rows: &[CalendarRow<'_>]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            CalendarRow::Header(account) => format!("# {account}"),
            CalendarRow::Calendar(calendar) => calendar.name.clone(),
        })
        .collect()
}

#[test]
fn the_calendar_list_heads_each_account_once_there_are_two_and_ranks_what_is_typed() {
    let calendars = calendars();
    assert_eq!(
        listed(&calendar_rows(&calendars, "")),
        [
            "# maya@brightmoor.co",
            "maya@brightmoor.co",
            "Brightmoor Team",
            "# maya.lin@example.com",
            "maya.lin@example.com"
        ]
    );
    assert_eq!(
        listed(&calendar_rows(&calendars[..2], "")),
        ["maya@brightmoor.co", "Brightmoor Team"],
        "one account's: no headers"
    );
    assert_eq!(
        listed(&calendar_rows(&calendars, "team")),
        ["Brightmoor Team"]
    );
    // Best first, with each account's calendars together under it and the accounts in the
    // order their best calendars come.
    assert_eq!(
        listed(&calendar_rows(&calendars, "e")),
        [
            "# maya.lin@example.com",
            "maya.lin@example.com",
            "# maya@brightmoor.co",
            "Brightmoor Team"
        ]
    );
    assert_eq!(
        listed(&calendar_rows(&calendars, "bri")),
        ["Brightmoor Team", "maya@brightmoor.co"],
        "the better match first"
    );
    assert!(calendar_rows(&calendars, "zzz").is_empty());
}

#[test]
fn the_calendar_list_highlight_skips_headers_and_goes_round() {
    let calendars = calendars();
    let rows = calendar_rows(&calendars, "");
    assert_eq!(
        calendar_highlight(&rows, 0),
        1,
        "the first calendar, not its header"
    );
    assert_eq!(calendar_highlight(&rows, 9), 1);
    assert_eq!(calendar_highlight(&rows, 2), 2);
    assert_eq!(calendar_step(&rows, 2, false), 4, "past maya.lin's header");
    assert_eq!(
        calendar_step(&rows, 4, false),
        1,
        "round the end, past the first header"
    );
    assert_eq!(calendar_step(&rows, 1, true), 4);
}

#[test]
fn the_calendar_field_names_its_account_once_there_are_two() {
    let calendars = calendars();
    assert_eq!(
        calendar_label(&calendars, "team"),
        "Brightmoor Team · maya@brightmoor.co"
    );
    assert_eq!(
        calendar_label(&calendars, "maya.lin@example.com"),
        "maya.lin@example.com",
        "named as its account"
    );
    assert_eq!(calendar_label(&calendars[..2], "team"), "Brightmoor Team");
    assert_eq!(calendar_label(&calendars, "gone"), "gone");
}

#[test]
fn enter_on_focused_dropdown_opens_without_picking_on_the_same_frame() {
    let ctx = egui::Context::default();
    exo_ui::install(&ctx).expect("the system fonts load");
    let now = at("2026-11-17T09:00");
    let calendars = calendars();
    let chips = std::collections::BTreeMap::new();
    let shown = Shown {
        calendars: &calendars,
        contacts: None,
        me: "maya@brightmoor.co",
        now: &now,
        week_start: jiff::civil::Weekday::Sunday,
        conversation: &[],
        places: &[],
        online: true,
        chips: &chips,
    };
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 600.0));
    let step = |form: &mut EventForm, events: Vec<egui::Event>| {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(area),
                events,
                ..Default::default()
            },
            |ui| {
                form.show(ui, area, &shown, true);
            },
        );
        out.textures_delta.clear();
    };
    let press = |key| {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    };

    for (field, expected) in [
        (Field::Repeat, Dropdown::Repeat),
        (Field::Meeting, Dropdown::Meeting),
    ] {
        let mut form = EventForm::create(values(), false);
        form.focus = Some(field);
        step(&mut form, Vec::new());
        assert_eq!(form.focused, Some(field));

        step(&mut form, press(egui::Key::Enter));
        assert_eq!(
            form.dropdown,
            Some((expected, 0)),
            "Enter opens {expected:?} and keeps it open"
        );

        step(&mut form, press(egui::Key::Enter));
        assert_eq!(
            form.dropdown, None,
            "a second Enter picks the highlighted row and closes {expected:?}"
        );
    }
}
