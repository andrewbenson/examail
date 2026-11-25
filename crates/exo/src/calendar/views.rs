use egui::{Rect, Ui};
use exo_commands::Toast;
use exo_ui::CalendarColor;
use jiff::{SignedDuration, Zoned, civil::Date};

use super::{
    Action, accounts, day,
    details::{Details, DetailsAction, MenuItem, Shown},
    event::{self, Event},
    form::Field,
    grid::{self, Grid, GridAction, GridMode},
    invite_event, provider, repeat, share,
    week::{Page, WeekAction},
};

impl crate::Mail {
    /// The right pane's day view, when it's up. Returns whether it drew.
    pub(crate) fn calendar_show_pane(&mut self, ui: &mut Ui, pane: Rect, rem: f32) -> bool {
        if self.calendar_show_pane_form(ui, pane, rem) {
            return true;
        }
        let Some(day) = self.calendar.day else {
            return false;
        };
        if let Some(email) = self.calendar_takeover() {
            match provider::show_takeover(ui, pane, rem, &email, &mut self.calendar.takeover) {
                // Calendar Settings opens over the sign-in.
                Some(provider::TakeoverAction::SignIn) => {
                    self.calendar.options_open = true;
                    self.calendar_sign_in(Some(&email), accounts::SignInFor::List);
                    self.account.update_settings(|settings| {
                        _ = settings.calendar_invite_hint.insert(email.clone(), true)
                    });
                }
                Some(provider::TakeoverAction::Dismiss) => {
                    self.account.update_settings(|settings| {
                        _ = settings.calendar_invite_hint.insert(email.clone(), true)
                    })
                }
                None => {}
            }
            return true;
        }
        let now = self.now.clone();
        let (guests, expanded) = self.calendar_overlay(&now, true, &[day.date]);
        let email = self.account.email();
        let invite_uid = self
            .calendar
            .invite
            .as_ref()
            .map(|invite| invite.uid.as_str());
        let mut events = self.calendar_events(&now, invite_uid);
        // The invite being read, if the calendar doesn't have it.
        if let Some(invite) = &self.calendar.invite
            && !events
                .iter()
                .any(|event| event.ical_uid.as_deref() == Some(invite.uid.as_str()))
        {
            let colors = event::colors(self.account.settings());
            let color = colors.get(email).copied().unwrap_or(CalendarColor::BLUE);
            let event = Event::from_invite(invite, color, email, now.time_zone());
            if event.on(day.date) && event.rsvp != Some(event::Rsvp::Declined) {
                events.push(event);
            }
        }
        let days = [day.date];
        let selected = self
            .calendar
            .details
            .as_ref()
            .map(|details| details.key.clone());
        let draft = self.calendar_draft(&now);
        let hidden = self
            .calendar
            .form
            .as_ref()
            .and_then(|form| form.edited.as_ref())
            .map(|edited| edited.key.clone());
        let mode = if self.calendar.form.is_some() {
            GridMode::EditingDraft {
                draft: draft.as_ref(),
                hidden: hidden.as_deref(),
            }
        } else if let Some(selected) = selected.as_deref() {
            GridMode::DetailsOpen { selected }
        } else {
            GridMode::Browse
        };
        let grid = Grid {
            days: &days,
            events: &events,
            now: &now,
            tz: now.time_zone(),
            invite: invite_uid,
            mode,
            guests: &guests,
            expanded: expanded.as_deref(),
            mine: true,
            zones: &[],
        };
        let compose = self.compose.focused().is_some();
        let actions = day::show(
            ui,
            pane,
            rem,
            day.date,
            &grid,
            &mut self.calendar.grid,
            compose,
        );
        // The day shows as loading until it's in what Calendar has been read for.
        if !self
            .account
            .calendar()
            .data()
            .window
            .as_ref()
            .is_some_and(|window| window.days.contains(&day.date))
        {
            day::paint_loading(ui, pane);
        }
        for action in actions {
            match action {
                day::DayAction::Step(days) => self.calendar_step(days),
                day::DayAction::Today => self.calendar_run(Action::Today),
                day::DayAction::Create => self.calendar_create(None, false, false),
                day::DayAction::Grid(action) => self.calendar_grid(action, true),
            }
        }
        true
    }

    /// What the pointer did in a grid: an event's details, a new event, an event moved or
    /// opened.
    fn calendar_grid(&mut self, action: GridAction, pane: bool) {
        let now = self.now.clone();
        match action {
            GridAction::Select { key, .. } if key.starts_with(grid::FREE_SLOT) => {
                self.calendar_pick_slot(&key, &now)
            }
            GridAction::Select { key, anchor } => {
                self.calendar.details = Some(Details::new(key, anchor, pane))
            }
            GridAction::Background => self.calendar.details = None,
            GridAction::Restricted => {
                // A drag closes the details first.
                self.calendar.details = None;
                self.commands
                    .toast(Toast::new("You do not have permission to edit this event."));
            }
            GridAction::Slot {
                start,
                end,
                all_day,
            } => {
                self.calendar.details = None;
                let dragged = end.timestamp().duration_since(start.timestamp())
                    != SignedDuration::from_mins(30);
                self.calendar_slot(start, end, all_day, dragged, !pane);
            }
            GridAction::Change { key, start, end } => {
                self.calendar.details = None;
                self.calendar_moved(&key, start, end, &now);
            }
            // Only sharing availability picks times.
            GridAction::Past | GridAction::Remove(_) => {}
            // Calendar Settings, at the secondary time zone.
            GridAction::ZoneSettings => self.calendar.options_open = true,
            GridAction::Expand(guest) => self.calendar_expand(guest, pane),
            GridAction::Open(key) => {
                // Not while editing, or with Meet's guests loaded.
                if self.calendar.form.is_none() {
                    self.calendar.details = None;
                    self.calendar_edit(&key, Field::Title, &now);
                }
            }
        }
    }

    /// The week view over `area` (the window right of the rail, under the tab strip), when it's
    /// up.
    pub(crate) fn calendar_show_week(&mut self, ui: &Ui, area: Rect) {
        let Some(active) = self.calendar_week() else {
            return;
        };
        let now = self.now.clone();
        let events = self.calendar_events(&now, None);
        let start = self.calendar_week_start(active);
        let days = self.calendar_week_days(active);
        let listed = self.calendar_list();
        let selected = self
            .calendar
            .details
            .as_ref()
            .map(|details| details.key.clone());
        let draft = self.calendar_draft(&now);
        let hidden = self
            .calendar
            .form
            .as_ref()
            .and_then(|form| form.edited.as_ref())
            .map(|edited| edited.key.clone());
        let slots = self.calendar_meet_view(&now);
        let (guests, expanded) = self.calendar_overlay(&now, false, &days);
        let markers = self.calendar_meet_markers();
        // Sharing availability, my events show while I'm listed, under the slots picked.
        let share = self
            .calendar
            .week
            .as_ref()
            .and_then(|week| week.share.as_ref());
        let sharing = share.map(|share| share.minutes);
        let shows_mine = share.is_none_or(share::Share::shows_mine);
        let events: Vec<Event> = match share {
            Some(share) => {
                let color = listed
                    .first()
                    .map_or(CalendarColor::BLUE, |calendar| calendar.color);
                let mine = if share.shows_mine() {
                    events
                } else {
                    Vec::new()
                };
                mine.into_iter().chain(share.events(color, &now)).collect()
            }
            None => events.into_iter().chain(slots).collect(),
        };
        let book = self.shared_book();
        let zones = self.calendar_secondary_zones();
        let mode = if self.calendar.form.is_some() {
            GridMode::EditingDraft {
                draft: draft.as_ref(),
                hidden: hidden.as_deref(),
            }
        } else if let Some(minutes) = sharing {
            GridMode::ShareAvailability {
                minutes,
                selected: selected.as_deref(),
            }
        } else if self
            .calendar
            .week
            .as_ref()
            .is_some_and(|week| week.meet.finding)
        {
            GridMode::FindTime {
                selected: selected.as_deref(),
            }
        } else if let Some(selected) = selected.as_deref() {
            GridMode::DetailsOpen { selected }
        } else {
            GridMode::Browse
        };
        let grid = Grid {
            days: &days,
            events: &events,
            now: &now,
            tz: now.time_zone(),
            invite: None,
            mode,
            guests: &guests,
            expanded: expanded.as_deref(),
            mine: shows_mine,
            zones: &zones,
        };
        let loading = self.account.calendar().data().loading
            && events
                .iter()
                .all(|event| !days.iter().any(|day| event.on(*day)));
        let monday = self.account.settings().calendar.week_starts_on == 1;
        // Not dismissed, nothing else using the week, and no dialog.
        let find_time_hint = !self
            .account
            .settings()
            .calendar
            .has_dismissed_find_time_enter_shortcut_tooltip
            && self.calendar.form.is_none()
            && sharing.is_none()
            && !self.settings.dialog_open();
        let accounts = self.calendar_accounts_listed();
        let folded = self
            .account
            .device_settings()
            .folded_calendar_accounts
            .clone();
        let page = Page {
            grid,
            week_start: start,
            monday,
            account: self.account.email(),
            accounts: &accounts,
            loading,
            contacts: book.as_deref(),
            markers: &markers,
            find_time_hint,
            folded: &folded,
        };
        let compose = self.compose.focused().is_some() || self.calendar.form.is_some();
        let Some(week) = self.calendar.week.as_mut() else {
            return;
        };
        for action in week.show(ui, area, &page, compose) {
            match action {
                WeekAction::Today => self.calendar_run(Action::WeekToday),
                WeekAction::Create => {
                    let now = self.now.clone();
                    self.calendar_new_event(&now);
                }
                WeekAction::Step(weeks) => self.calendar_run(Action::WeekStep(weeks)),
                // While the form is open, a day moves the draft there.
                WeekAction::Select(date) if self.calendar.form.is_some() => {
                    self.calendar_form_shift(|start| start.with().date(date).build().ok())
                }
                WeekAction::Select(date) => {
                    self.calendar.details = None;
                    if let Some(week) = &mut self.calendar.week {
                        week.active = date;
                    }
                }
                WeekAction::ToggleCalendar(id) => self.calendar_toggle(&id),
                WeekAction::Recolor(id, color) => self.calendar_recolor(&id, color),
                WeekAction::AddAccount => self.calendar_sign_in(None, accounts::SignInFor::List),
                WeekAction::SignInAgain(email) => {
                    self.calendar_sign_in(Some(&email), accounts::SignInFor::List)
                }
                WeekAction::RemoveAccount(email) => self.account.remove_calendar_account(&email),
                WeekAction::Fold(email) => self.account.update_device_settings(|device| {
                    if !device.folded_calendar_accounts.remove(&email) {
                        device.folded_calendar_accounts.insert(email);
                    }
                }),
                WeekAction::Grid(action) => {
                    if !self.calendar_share_grid(action.clone(), &now) {
                        self.calendar_grid(action, false);
                    }
                }
                WeekAction::Share(action) => self.calendar_share_action(action, &now),
                WeekAction::Meet(action) => self.calendar_meet(action, &now),
                // Its gear opens Calendar Settings directly.
                WeekAction::Settings => self.calendar.options_open = true,
            }
        }
    }

    /// The event details over `window`, when they're up, and the recurring-RSVP alert over
    /// `page`. Also reports the calendar's failed changes.
    pub(crate) fn calendar_show_details(&mut self, ui: &Ui, window: Rect, page: Rect) {
        for failure in self.account.calendar().failures() {
            self.calendar_failed(failure);
        }
        let Some(key) = self
            .calendar
            .details
            .as_ref()
            .map(|details| details.key.clone())
        else {
            return;
        };
        let now = self.now.clone();
        let found = (self.calendar.week.is_some() || self.calendar.day.is_some())
            .then(|| {
                self.calendar_detailed(&key, &now)
                    .map(|(event, source, can_respond)| (event, source, can_respond, false))
                    .or_else(|| {
                        self.calendar_guest_detailed(&key, &now)
                            .map(|(event, source)| (event, source, false, true))
                    })
            })
            .flatten();
        let Some((event, source, can_respond, guest)) = found else {
            self.calendar.details = None;
            return;
        };
        let email = self.account.email().to_owned();
        let calendar = self.calendar_name(&event, &source);
        let writable = self
            .account
            .calendar()
            .data()
            .calendars
            .iter()
            .flatten()
            .find(|calendar| calendar.entry.id.as_deref() == Some(event.calendar_id.as_str()))
            .is_some_and(|calendar| {
                matches!(
                    calendar.entry.access_role.as_deref(),
                    Some("owner" | "writer")
                )
            });
        let mut menu = Vec::new();
        if event.editable {
            menu.push(MenuItem::Edit);
        }
        // Deleting needs a calendar I can write to.
        if writable && event.id.is_some() {
            menu.push(MenuItem::Delete);
        }
        if source.attendees.iter().any(|attendee| {
            !attendee
                .email
                .as_deref()
                .is_some_and(|address| address.eq_ignore_ascii_case(&email))
                && attendee.is_self != Some(true)
        }) {
            menu.push(MenuItem::EmailGuests);
        }
        if source.html_link.is_some() && event.title != "Busy" && !guest {
            menu.push(MenuItem::OpenInCalendar);
        }
        // A guest's series has no rule Exo knows, so its line says `Repeats`.
        let rule = if guest {
            source
                .recurring_event_id
                .as_ref()
                .map(|_| repeat::details_line(None, event.start.date()))
        } else {
            self.calendar_rule_line(&event, &source)
        };
        let shown = Shown {
            event: &event,
            source: &source,
            me: &email,
            calendar: calendar.as_deref(),
            now: &now,
            can_respond,
            menu: &menu,
            editable: event.editable,
            rule,
            guest,
            online: self.snapshot.connection.is_online(),
        };
        let Some(details) = self.calendar.details.as_mut() else {
            return;
        };
        let (actions, popup) = details.show(ui, window, &shown);
        if details.dismissed(ui, popup) {
            self.calendar.details = None;
        }
        for action in actions {
            match action {
                DetailsAction::Respond { answer, all } => self.calendar_respond(answer, all),
                DetailsAction::Menu(MenuItem::EmailGuests) => {
                    self.calendar.details = None;
                    let to = source
                        .attendees
                        .iter()
                        .filter(|attendee| {
                            attendee.is_self != Some(true) && attendee.resource != Some(true)
                        })
                        .filter_map(|attendee| {
                            let address = attendee
                                .email
                                .clone()
                                .filter(|address| !address.eq_ignore_ascii_case(&email))?;
                            Some(exo_store::Address {
                                name: attendee.display_name.clone(),
                                email: address,
                            })
                        });
                    let fields = crate::compose::Fields {
                        to: to.collect(),
                        subject: source.summary.clone().unwrap_or_default(),
                        ..crate::compose::Fields::default()
                    };
                    self.compose_new(fields, Some(crate::compose::Field::Body));
                }
                DetailsAction::Menu(MenuItem::OpenInCalendar) => {
                    // The event's page, as this account.
                    if let Some(mut link) = source
                        .html_link
                        .as_deref()
                        .and_then(crate::format::urls::web)
                        .and_then(|link| url::Url::parse(&link).ok())
                    {
                        link.query_pairs_mut()
                            .append_pair("authuser", &email)
                            .append_pair("shxr", "0");
                        self.open_url(link.as_str());
                    }
                }
                DetailsAction::Open(url) => self.open_url(&url),
                DetailsAction::Menu(MenuItem::Edit) => self.calendar_edit(&key, Field::Title, &now),
                DetailsAction::Edit(field) => self.calendar_edit(&key, field, &now),
                DetailsAction::EditDate => {
                    self.calendar_edit(&key, Field::When, &now);
                    if let Some(form) = &mut self.calendar.form {
                        form.focus(Field::When);
                    }
                }
                DetailsAction::Copy(text) => self.calendar_copy(ui.ctx(), text),
                DetailsAction::Menu(MenuItem::Delete) => self.calendar_delete(&key, &now),
            }
        }
        if let Some(alert) = self
            .calendar
            .details
            .as_mut()
            .and_then(|details| details.alert.as_mut())
        {
            let title = format!("RSVP {} to recurring event", alert.answer.word());
            let mut choice = alert.choice;
            let (clicked, _) = exo_ui::Alert {
                title: &title,
                text: "",
                actions: &["OK", "Cancel"],
                selected: 0,
            }
            .show_options_modal(
                ui.ctx(),
                "calendar-rsvp-alert",
                page,
                &["This event", "All events"],
                &mut choice,
            );
            alert.choice = choice;
            match clicked {
                Some(0) => self.calendar_run(Action::DetailsConfirm),
                Some(_) => self.calendar_run(Action::DetailsEscape),
                None => {}
            }
        }
    }

    /// Event `key` as the calendar shows it, Calendar's event behind it, and whether I can
    /// answer it. An invite the calendar doesn't have stands in as its own event.
    pub(super) fn calendar_detailed(
        &self,
        key: &str,
        now: &Zoned,
    ) -> Option<(Event, gmail::calendar::Event, bool)> {
        let email = self.account.email();
        let invite_uid = self
            .calendar
            .invite
            .as_ref()
            .map(|invite| invite.uid.as_str());
        if let Some(event) = self
            .calendar_events(now, invite_uid)
            .into_iter()
            .find(|event| event.key == key)
        {
            let data = self.account.calendar().data();
            let source = data
                .events
                .iter()
                .find(|stored| {
                    stored.calendar_id == event.calendar_id && stored.event.id == event.id
                })?
                .event
                .clone();
            let writable = data
                .calendars
                .iter()
                .flatten()
                .find(|calendar| calendar.entry.id.as_deref() == Some(event.calendar_id.as_str()))
                .is_some_and(|calendar| {
                    matches!(
                        calendar.entry.access_role.as_deref(),
                        Some("owner" | "writer")
                    )
                });
            let can_respond = writable && event.rsvp.is_some();
            return Some((event, source, can_respond));
        }
        let invite = self.calendar.invite.as_ref()?;
        let colors = event::colors(self.account.settings());
        let event = Event::from_invite(
            invite,
            colors.get(email).copied().unwrap_or(CalendarColor::BLUE),
            email,
            now.time_zone(),
        );
        let can_respond = invite.request && event.rsvp.is_some();
        (event.key == key).then(|| (event, invite_event(invite), can_respond))
    }

    /// A series instance's rule line: a new series' own, else what Calendar says of the series,
    /// asked for the first time the details open on it.
    pub(super) fn calendar_rule_line(
        &self,
        event: &Event,
        source: &gmail::calendar::Event,
    ) -> Option<String> {
        let series = source.recurring_event_id.as_deref()?;
        let rules = if source.recurrence.is_empty() {
            let data = self.account.calendar().data();
            let known = data
                .series
                .get(&format!("{}/{series}", event.calendar_id))
                .cloned();
            if known.is_none() {
                self.account
                    .calendar()
                    .load_series(&event.calendar_id, series);
            }
            known.unwrap_or_default()
        } else {
            source.recurrence.clone()
        };
        // The instance's own start before any move.
        let tz = event.start.time_zone();
        let anchor = source
            .original_start_time
            .as_ref()
            .and_then(|time| match &time.date {
                Some(date) => date.parse::<Date>().ok(),
                None => time.timestamp(tz).map(|at| at.to_zoned(tz.clone()).date()),
            });
        let rule = exo_store::Recurrence::from_rules(&rules, tz);
        Some(repeat::details_line(
            rule.as_ref(),
            anchor.unwrap_or_else(|| event.start.date()),
        ))
    }

    /// The `Label` row of an event with its own colour (`via {calendar}`), or the `Calendar` row:
    /// the event's calendar, when more than one shows.
    fn calendar_name(&self, event: &Event, source: &gmail::calendar::Event) -> Option<String> {
        let data = self.account.calendar().data();
        let calendar = &data
            .calendars
            .iter()
            .flatten()
            .find(|calendar| calendar.entry.id.as_deref() == Some(event.calendar_id.as_str()))?
            .entry;
        let name = calendar
            .summary_override
            .clone()
            .or_else(|| calendar.summary.clone())?;
        if source.color_id.is_some() {
            return Some(format!("via {name}"));
        }
        (self
            .account
            .settings()
            .calendars_where(|setting| setting.show)
            .len()
            > 1)
        .then_some(name)
    }
}
