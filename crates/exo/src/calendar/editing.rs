//! Opening, hosting and wiring the event form and Calendar Settings dialog.
//!
//! Saving, deleting, series splitting, Undo and confirmation prompts live in [`super::save`], and
//! Google Calendar event construction and patching live in [`super::patch`].

use egui::{Rect, Ui, pos2, vec2};
use jiff::{ToSpan, Zoned};

use super::{
    event::Event,
    form::{
        self, CalendarOption, Edited, EventForm, Field, FormAction, Guest, Meeting, Mode, Values,
    },
    grid::{DRAFT, GridMode},
    patch::values_of,
    save::{Prompt, Then},
};

impl crate::Mail {
    /// The writable calendars a new event can go on, the primary first.
    pub(super) fn calendar_writable(&self) -> Vec<CalendarOption> {
        self.calendar_list()
            .into_iter()
            .filter(|calendar| calendar.writable)
            .map(|calendar| CalendarOption {
                account: calendar.account,
                id: calendar.id,
                name: calendar.name,
                color: calendar.color,
            })
            .collect()
    }

    /// A new event's form for `when` (in the week view when `week`), on the primary calendar.
    /// Over an open form that would lose something, asks first.
    pub(super) fn calendar_create(
        &mut self,
        when: Option<(Zoned, Zoned)>,
        all_day: bool,
        week: bool,
    ) {
        let form = EventForm::create(self.calendar_new_values(when, all_day), week);
        self.calendar_open_form(form);
    }

    /// A new event's values for `when`.
    fn calendar_new_values(&self, when: Option<(Zoned, Zoned)>, all_day: bool) -> Values {
        Values {
            title: String::new(),
            when,
            all_day,
            guests: Vec::new(),
            location: String::new(),
            about: String::new(),
            // A Meet link when Google Meet is the default meeting link.
            meeting: if self
                .account
                .settings()
                .meeting_links
                .default_meeting_link
                .as_deref()
                == Some("google")
            {
                Meeting::NewMeet
            } else {
                Meeting::None
            },
            calendar: self.account.email().to_owned(),
            repeat: None,
        }
    }

    /// A click on a date in a message opens the right pane's new event titled `title` (the
    /// conversation's subject) with `guests` (its people, as name and address), over the day
    /// `date` (the one the date's hover showed). When stays empty.
    pub(crate) fn calendar_create_for_thread(
        &mut self,
        title: String,
        guests: Vec<(Option<String>, String)>,
        date: jiff::civil::Date,
    ) {
        let mut values = self.calendar_new_values(None, false);
        values.title = title;
        values.guests = guests
            .into_iter()
            .map(|(name, email)| Guest {
                email,
                name,
                optional: false,
                response: None,
                me: false,
                organizer: false,
            })
            .collect();
        self.calendar_open_form(EventForm::create(values, false));
        if self
            .calendar
            .form
            .as_ref()
            .is_some_and(|form| form.mode == Mode::Create && !form.week)
        {
            self.calendar.form_day = Some(date);
        }
    }

    /// Opens `form`, asking first if the open one would lose something.
    pub(super) fn calendar_open_form(&mut self, form: EventForm) {
        self.calendar.details = None;
        if self
            .calendar
            .form
            .as_ref()
            .is_some_and(|open| !open.is_empty())
        {
            let title = if self
                .calendar
                .form
                .as_ref()
                .is_some_and(|open| open.mode == Mode::Edit)
            {
                "Discard changes?"
            } else {
                "Discard this event?"
            };
            self.calendar.prompt = Some(Prompt::Confirm {
                title,
                text: "Your changes will not be saved.",
                actions: vec!["Discard", "Cancel"],
                selected: 0,
                then: Then::Replace(Box::new(form)),
            });
            return;
        }
        if form.mode == Mode::Create && !form.week {
            self.calendar.form_day = form.values.when.as_ref().map(|(start, _)| start.date());
            self.calendar.form_grid.reset();
        }
        self.calendar.form = Some(form);
    }

    /// `b` (and `shift+b`): in the week view, a half hour from the next half hour on its day, and
    /// elsewhere the right pane's empty form.
    pub(super) fn calendar_new_event(&mut self, now: &Zoned) {
        let Some(active) = self.calendar_week() else {
            self.calendar_create(None, false, false);
            return;
        };
        let minutes = now.minute();
        let (mut hour, rounded) = (
            now.hour(),
            if minutes == 0 {
                0
            } else if minutes <= 30 {
                30
            } else {
                60
            },
        );
        let mut date = active;
        if rounded == 60 {
            hour += 1;
            if hour >= 24 {
                hour = 0;
                date = date.tomorrow().unwrap_or(date);
            }
        }
        let start = date
            .at(hour, (rounded % 60) as i8, 0, 0)
            .to_zoned(now.time_zone().clone())
            .ok();
        let when =
            start.and_then(|start| start.checked_add(30.minutes()).ok().map(|end| (start, end)));
        self.calendar_create(when, false, true);
    }

    /// Event `key`'s form, with `field` focused.
    pub(super) fn calendar_edit(&mut self, key: &str, field: Field, now: &Zoned) {
        let Some((event, source, _)) = self.calendar_detailed(key, now) else {
            return;
        };
        if !event.editable {
            return;
        }
        let me = self.calendar_account(&event.calendar_id);
        let values = values_of(&event, &source, &me);
        let edited = Edited {
            key: event.key.clone(),
            event: source,
            color: event.color,
        };
        let form = EventForm::edit(values, edited, self.calendar.week.is_some(), field);
        self.calendar_open_form(form);
    }

    /// A click or drag on empty grid makes a new event there. With the form open, it moves the
    /// draft there instead, keeping the draft's length for a click.
    pub(super) fn calendar_slot(
        &mut self,
        start: Zoned,
        end: Zoned,
        all_day: bool,
        dragged: bool,
        week: bool,
    ) {
        if let Some(form) = &mut self.calendar.form {
            let length = form
                .values
                .when
                .as_ref()
                .map(|(from, to)| to.timestamp().duration_since(from.timestamp()));
            let end = match length {
                Some(length) if !dragged && !all_day && !form.values.all_day => {
                    start.checked_add(length).unwrap_or(end)
                }
                _ => end,
            };
            form.values.when = Some((start, end));
            form.values.all_day = all_day;
            return;
        }
        self.calendar_create(Some((start, end)), all_day, week);
    }

    /// An event moved or resized in the grid becomes the draft's new time, or an edit saved at
    /// once (through the edit's dialogs, without the check for a time in the past).
    pub(super) fn calendar_moved(&mut self, key: &str, start: Zoned, end: Zoned, now: &Zoned) {
        if key == DRAFT {
            if let Some(form) = &mut self.calendar.form {
                form.values.when = Some((start, end));
            }
            return;
        }
        let Some((event, source, _)) = self.calendar_detailed(key, now) else {
            return;
        };
        if !event.editable {
            return;
        }
        let me = self.calendar_account(&event.calendar_id);
        let mut values = values_of(&event, &source, &me);
        let edited = Edited {
            key: event.key.clone(),
            event: source,
            color: event.color,
        };
        let mut form = EventForm::edit(
            values.clone(),
            edited,
            self.calendar.week.is_some(),
            Field::When,
        );
        values.when = Some((start, end));
        form.values = values;
        self.calendar.details = None;
        self.calendar_edit_dialogs(form, false);
    }

    /// Calendar Settings over `page`, while it's up or fading.
    pub(super) fn calendar_show_options(&mut self, ctx: &egui::Context, page: Rect) {
        use super::options::{OptionsAction, Setting};
        let settings = self.account.settings();
        let link = settings.meeting_links.default_meeting_link.clone();
        let calendar = &settings.calendar;
        let zone = calendar
            .additional_timezones
            .first()
            .map(|zone| zone.iana_id.clone());
        let options = [
            (Setting::Monday, calendar.week_starts_on == 1),
            (Setting::Weekends, calendar.show_weekends),
            (Setting::Declined, calendar.show_declined_events),
            (Setting::SecondaryZone, calendar.show_additional_timezones),
        ];
        // The mail account first, then the others by address.
        let email = self.account.email();
        let mut emails: Vec<&String> = settings
            .calendar_accounts
            .keys()
            .filter(|address| *address != email)
            .collect();
        emails.sort_by_key(|address| address.to_lowercase());
        let color = |address: &str| {
            self.accounts()
                .iter()
                .find(|account| account.email == address)
                .map_or_else(
                    || crate::accounts::default_color(address),
                    crate::accounts::color,
                )
        };
        let accounts: Vec<super::options::ShownAccount<'_>> = std::iter::once(email)
            .chain(emails.into_iter().map(String::as_str))
            .map(|address| super::options::ShownAccount {
                email: address,
                color: color(address),
                primary: address == email,
            })
            .collect();
        let shown = super::options::Shown {
            options,
            zone: zone.as_deref(),
            now: self.now.timestamp(),
            meeting_link: link.as_deref(),
            accounts: &accounts,
        };
        for action in super::options::show(
            ctx,
            page,
            self.calendar.options_open,
            &shown,
            &mut self.calendar.options_zone,
        ) {
            match action {
                OptionsAction::Toggle(setting) => self.account.update_settings(|settings| {
                    let calendar = &mut settings.calendar;
                    match setting {
                        Setting::Monday => {
                            calendar.week_starts_on = u8::from(calendar.week_starts_on != 1)
                        }
                        Setting::Weekends => calendar.show_weekends = !calendar.show_weekends,
                        Setting::Declined => {
                            calendar.show_declined_events = !calendar.show_declined_events
                        }
                        Setting::SecondaryZone => {
                            calendar.show_additional_timezones = !calendar.show_additional_timezones
                        }
                    }
                }),
                OptionsAction::SecondaryZone(iana) => self.account.update_settings(|settings| {
                    settings.calendar.additional_timezones = vec![exo_store::AdditionalTimezone {
                        iana_id: iana.clone(),
                    }]
                }),
                OptionsAction::MeetingLink(link) => self.account.update_settings(|settings| {
                    settings.meeting_links.default_meeting_link = link.clone()
                }),
                OptionsAction::Close => self.calendar.options_open = false,
                // Adding an account closes the dialog, opens the week view, and starts Google's
                // consent.
                OptionsAction::SignIn => {
                    self.calendar.options_open = false;
                    if self.calendar.week.is_none() {
                        self.calendar_run(super::Action::OpenWeek);
                    }
                    self.calendar_sign_in(None, super::accounts::SignInFor::List);
                }
                OptionsAction::Remove(email) => self.account.remove_calendar_account(&email),
                // Notification settings, in this dialog's place.
                OptionsAction::Notifications => {
                    self.calendar.options_open = false;
                    let now = ctx.input(|input| input.time);
                    self.run(
                        crate::commands::Action::Settings(crate::settings::Action::Open(
                            crate::settings::Dialog::Notifications,
                        )),
                        now,
                    );
                }
            }
        }
    }

    /// The form: the week view's modal beside the draft, the right pane's form over its day, or
    /// an edit's modal beside the event in the right pane's day.
    pub(crate) fn calendar_show_form(&mut self, ui: &mut Ui, window: Rect) {
        let Some(form) = &self.calendar.form else {
            return;
        };
        if !(form.week || form.mode == Mode::Edit)
            || (self.calendar.week.is_none() && self.calendar.day.is_none())
        {
            return;
        }
        let pane = self.calendar.week.is_none();
        let anchor = if pane {
            self.calendar.grid.draft_anchor
        } else {
            self.calendar
                .week
                .as_ref()
                .and_then(|week| week.draft_anchor())
        };
        let Some(anchor) = anchor else { return };
        let now = self.now.clone();
        let calendars = self.calendar_writable();
        let book = self.shared_book();
        let me = self.account.email().to_owned();
        let conversation = self.calendar_conversation();
        let places = self.calendar_places(ui.ctx());
        let chips = self.calendar_guest_chips();
        let shown = form::Shown {
            calendars: &calendars,
            contacts: book.as_deref(),
            me: &me,
            now: &now,
            week_start: self.calendar_week_start_day(),
            conversation: &conversation,
            places: &places,
            online: self.snapshot.connection.is_online(),
            chips: &chips,
        };
        let Some(form) = self.calendar.form.as_mut() else {
            return;
        };
        let height = form.content_height(false) + form::FOOTER_HEIGHT;
        let popup =
            self.calendar
                .form_placement
                .place(window, anchor, form::MODAL_WIDTH, height, pane);
        let mut clicked = None;
        egui::Area::new(exo_ui::id(ui.ctx(), "calendar-form"))
            .order(egui::Order::Foreground)
            .fixed_pos(popup.min)
            .constrain(false)
            .show(ui.ctx(), |area| {
                // The area covers the card, so the pointer over the card is over the area
                // (`layer_id_at`). The card's dropdowns ask that to stay open on a click inside.
                area.expand_to_include_rect(popup);
                let mut card = area.new_child(egui::UiBuilder::new().max_rect(popup));
                let snapped = exo_ui::pixel_snap(popup);
                super::popup::paint_card(card.painter(), snapped);
                let _ = card.interact(
                    popup,
                    exo_ui::id(card.ctx(), "calendar-form-card"),
                    egui::Sense::click_and_drag(),
                );
                let content = Rect::from_min_max(
                    popup.min,
                    pos2(popup.right(), popup.bottom() - form::FOOTER_HEIGHT),
                );
                form.show(&mut card, content, &shown, false);
                clicked = form.footer(
                    &card,
                    Rect::from_min_size(
                        pos2(popup.left(), content.bottom()),
                        vec2(popup.width(), form::FOOTER_HEIGHT),
                    ),
                    false,
                );
            });
        self.calendar_form_clicked(clicked, &now);
        self.calendar_tip_clicked(ui.ctx());
    }

    /// The right pane's new-event form: the form over a day of the calendar showing the draft,
    /// with Save and Cancel at the bottom. Returns whether it drew.
    pub(super) fn calendar_show_pane_form(&mut self, ui: &mut Ui, pane: Rect, rem: f32) -> bool {
        if !self.calendar_pane_form() {
            return false;
        }
        let now = self.now.clone();
        let calendars = self.calendar_writable();
        let book = self.shared_book();
        let me = self.account.email().to_owned();
        let conversation = self.calendar_conversation();
        let places = self.calendar_places(ui.ctx());
        let chips = self.calendar_guest_chips();
        let shown = form::Shown {
            calendars: &calendars,
            contacts: book.as_deref(),
            me: &me,
            now: &now,
            week_start: self.calendar_week_start_day(),
            conversation: &conversation,
            places: &places,
            online: self.snapshot.connection.is_online(),
            chips: &chips,
        };
        let draft = self.calendar_draft(&now);
        // The day follows When's highlighted suggestion.
        if let Some(preview) = self
            .calendar
            .form
            .as_ref()
            .and_then(EventForm::when_preview)
        {
            self.calendar.form_day = Some(preview.start.date());
        }
        let date = self
            .calendar
            .form_day
            .or(draft.as_ref().map(|draft| draft.start.date()))
            .unwrap_or(now.date());
        let events = self.calendar_events(&now, None);
        let days = [date];
        let (guests, expanded) = self.calendar_overlay(&now, true, &days);
        let Some(form) = self.calendar.form.as_mut() else {
            return false;
        };
        let content = form.content_height(true);
        let form_rect = Rect::from_min_max(pane.min, pos2(pane.right(), pane.top() + content));
        // The form covers the pane, the app's footer included, on `#fff`, Carbon `#18191a`.
        let fill = match exo_ui::theme(ui.ctx()) {
            exo_ui::Theme::Snow => egui::Color32::WHITE,
            exo_ui::Theme::Carbon => egui::Color32::from_rgb(0x18, 0x19, 0x1a),
        };
        ui.painter().rect_filled(pane, 0.0, fill);
        let footer = Rect::from_min_max(
            pos2(pane.left(), pane.bottom() - form::PANE_FOOTER_HEIGHT),
            pane.max,
        );
        // A press in the mail gives the mail the keyboard, and one in the pane takes it back.
        // What's over the page (the palette, a toast) doesn't count.
        let pressed = ui.input(|input| {
            input
                .pointer
                .primary_pressed()
                .then(|| input.pointer.press_origin())
                .flatten()
        });
        if let Some(at) = pressed.filter(|at| {
            ui.ctx().layer_id_at(*at).is_none_or(|layer| {
                layer == ui.layer_id() || layer.order == egui::Order::Background
            })
        }) {
            form.away = !pane.contains(at);
        }
        form.show(ui, form_rect, &shown, true);
        let clicked = form.footer(ui, footer, true);
        let hint = form::paint_away_hint(ui, pos2(form_rect.left(), form_rect.bottom()), form.away);
        // The day under the form, from the form's bottom (the hint's) to the footer's top. It's
        // 40px lower while When's list covers it (`margin-top: 64px`, over
        // `transition: margin-top .1s`).
        let covered = form.when_covers(form_rect.bottom() + hint + super::day::COMPACT_TOP);
        let spacing = 40.0
            * exo_ui::motion::transition(
                ui.ctx(),
                exo_ui::id(ui.ctx(), "calendar-form-when-spacing"),
                covered,
                0.1,
                exo_ui::motion::CubicBezier::EASE,
            );
        let day = Rect::from_min_max(
            pos2(pane.left(), form_rect.bottom() + hint + spacing),
            pos2(pane.right(), footer.top()),
        );
        let grid = super::grid::Grid {
            days: &days,
            events: &events,
            now: &now,
            tz: now.time_zone(),
            invite: None,
            mode: GridMode::EditingDraft {
                draft: draft.as_ref(),
                hidden: None,
            },
            guests: &guests,
            expanded: expanded.as_deref(),
            mine: true,
            zones: &[],
        };
        let compose = self.compose.focused().is_some();
        let actions = super::day::show_compact(
            ui,
            day,
            rem,
            date,
            &grid,
            &mut self.calendar.form_grid,
            compose,
        );
        for action in actions {
            match action {
                super::day::DayAction::Step(days) => {
                    let date = date.checked_add(days.days()).unwrap_or(date);
                    self.calendar.form_day = Some(date);
                }
                super::day::DayAction::Grid(super::grid::GridAction::Slot {
                    start,
                    end,
                    all_day,
                }) => self.calendar_slot(start, end, all_day, false, false),
                super::day::DayAction::Grid(super::grid::GridAction::Change {
                    key,
                    start,
                    end,
                }) => self.calendar_moved(&key, start, end, &now),
                super::day::DayAction::Grid(super::grid::GridAction::Expand(guest)) => {
                    self.calendar_expand(guest, true)
                }
                super::day::DayAction::Grid(_)
                | super::day::DayAction::Today
                | super::day::DayAction::Create => {}
            }
        }
        self.calendar_form_clicked(clicked, &now);
        self.calendar_tip_clicked(ui.ctx());
        true
    }

    /// A click on one of the form's copy tooltips, which starts a new message to a guest, opens
    /// the meeting link or copies.
    fn calendar_tip_clicked(&mut self, ctx: &egui::Context) {
        match self
            .calendar
            .form
            .as_mut()
            .and_then(|form| form.tip_clicked.take())
        {
            Some(form::TipClick::Compose(address, pop_out)) => {
                let fields = crate::compose::Fields {
                    to: vec![address],
                    ..crate::compose::Fields::default()
                };
                if pop_out {
                    self.compose_new_popped(fields)
                } else {
                    self.compose_new(fields, None)
                }
            }
            Some(form::TipClick::Open(link)) => self.open_url(&link),
            Some(form::TipClick::Copy(text)) => self.calendar_copy(ctx, text),
            None => {}
        }
    }

    /// Whether a new event's form fills the right pane.
    pub(crate) fn calendar_pane_form(&self) -> bool {
        self.calendar
            .form
            .as_ref()
            .is_some_and(|form| !form.week && form.mode == Mode::Create)
            && self.calendar.week.is_none()
    }

    fn calendar_form_clicked(&mut self, clicked: Option<FormAction>, now: &Zoned) {
        match clicked {
            Some(FormAction::Save) => self.calendar_save(now, false, false),
            Some(FormAction::Cancel) => self.calendar_discard(),
            None => {}
        }
    }

    /// The draft the form shows on the calendar.
    pub(super) fn calendar_draft(&self, now: &Zoned) -> Option<Event> {
        let form = self.calendar.form.as_ref()?;
        let (start, end, time_text) = form.draft_when(now)?;
        let color = match &form.edited {
            Some(edited) => edited.color,
            None => self
                .calendar_writable()
                .into_iter()
                .find(|calendar| calendar.id == form.values.calendar)
                .map_or(exo_ui::CalendarColor::BLUE, |calendar| calendar.color),
        };
        let title = if form.values.title.trim().is_empty() {
            "(No title)".to_owned()
        } else {
            form.values.title.clone()
        };
        Some(Event {
            time_text,
            ..Event::draft(
                DRAFT.to_owned(),
                title,
                start,
                end,
                form.values.all_day,
                color,
            )
        })
    }

    /// The people of the conversation open behind the form, each once and without me, for
    /// Who's `Everybody in this conversation` (senders, then Cc, then To; none while the week
    /// view is up).
    fn calendar_conversation(&self) -> Vec<exo_store::Address> {
        let Some(detail) = self
            .thread
            .as_ref()
            .and_then(|thread| thread.detail())
            .filter(|_| self.calendar.week.is_none())
        else {
            return Vec::new();
        };
        let me = self.account.email();
        let mut people: Vec<exo_store::Address> = Vec::new();
        for contact in detail
            .messages
            .iter()
            .flat_map(|message| message.from.iter().chain(&message.cc).chain(&message.to))
        {
            if contact.is_me || contact.email.eq_ignore_ascii_case(me) {
                continue;
            }
            // A later mention replaces the earlier one's name but keeps its place.
            match people
                .iter_mut()
                .find(|person| person.email == contact.email)
            {
                Some(person) => person.name = contact.name.clone(),
                None => people.push(exo_store::Address {
                    name: contact.name.clone(),
                    email: contact.email.clone(),
                }),
            }
        }
        people
    }

    /// The places for Where, asked for what's typed while Where has the keyboard and forgotten
    /// once it hasn't.
    fn calendar_places(&mut self, ctx: &egui::Context) -> Vec<super::places::Place> {
        match self
            .calendar
            .form
            .as_ref()
            .and_then(EventForm::location_query)
            .map(str::to_owned)
        {
            Some(text) => {
                self.calendar
                    .places
                    .ask(ctx, &text, self.snapshot.connection.is_online())
            }
            None => self.calendar.places.reset(),
        }
        self.calendar.places.found()
    }

    /// The external editor: Google Calendar's own new-event page with the form's title, times,
    /// guests, place and notes, as this account.
    pub(super) fn calendar_open_external(&mut self) {
        let Some(form) = &self.calendar.form else {
            return;
        };
        let values = &form.values;
        let Ok(mut link) =
            url::Url::parse("https://calendar.google.com/calendar/render?action=TEMPLATE")
        else {
            return;
        };
        let format = |at: &Zoned| at.strftime("%Y%m%dT%H%M%S").to_string();
        let dates = values
            .when
            .as_ref()
            .map(|(start, end)| format!("{}/{}", format(start), format(end)))
            .unwrap_or_default();
        let guests: Vec<&str> = values
            .guests
            .iter()
            .filter(|guest| !guest.me)
            .map(|guest| guest.email.as_str())
            .collect();
        link.query_pairs_mut()
            .append_pair("authuser", self.account.email())
            .append_pair("text", &values.title)
            .append_pair("dates", &dates)
            .append_pair("add", &guests.join(","))
            .append_pair("location", &values.location)
            .append_pair("details", &values.about)
            .append_pair("src", &values.calendar);
        self.open_url(link.as_str());
    }
}
