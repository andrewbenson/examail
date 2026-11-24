use std::collections::BTreeMap;

use exo_commands::{Hint, Toast};
use jiff::Zoned;

use super::{Context, Rsvp, Source, provider, save::Prompt};
use crate::thread::invite::{Invite, Response};

impl crate::Mail {
    /// When the right pane follows something new, a day view opened by anything but a command
    /// closes, and once the new thread's invite is read, the day view opens on its date. A
    /// command's day view stays put. In the list the thread's invite is read from the store, while
    /// an open thread reads its own.
    pub(super) fn calendar_follow_context(&mut self, now: &Zoned) {
        // The settings menu holds the right pane until it goes.
        if self.settings.visible() {
            return;
        }
        let thread = self.thread.as_ref();
        let context = Context {
            thread: thread
                .map(|thread| thread.id().to_owned())
                .or_else(|| self.pane_thread.clone()),
            message: thread.and_then(|thread| thread.focused().map(str::to_owned)),
        };
        if context != self.calendar.context {
            self.calendar.context = context.clone();
            self.calendar.invite = None;
            // A day a command opened stays, and invites don't take it over.
            let commanded = self
                .calendar
                .day
                .is_some_and(|day| day.source == Source::Command);
            self.calendar.invite_shown = commanded;
            if commanded {
                return;
            }
            self.calendar.day = None;
            let listed = self
                .inbox
                .threads()
                .iter()
                .find(|row| Some(&row.id) == context.thread.as_ref());
            if thread.is_none()
                && let Some(row) =
                    listed.filter(|row| row.attachment != crate::mailbox::Attachments::None)
            {
                self.account.calendar().load_invite(&row.id);
            }
        }
        if self.calendar.invite_shown {
            return;
        }
        let invite = match &self.thread {
            Some(thread) => thread.invite().cloned(),
            None => {
                let data = self.account.calendar().data();
                data.invite
                    .as_ref()
                    .filter(|(id, _)| Some(id) == self.calendar.context.thread.as_ref())
                    .and_then(|(_, ics)| {
                        crate::thread::invite::parse(&String::from_utf8_lossy(ics))
                    })
            }
        };
        let Some(invite) = invite else { return };
        self.calendar.invite_shown = true;
        let (start, end) = invite.zoned(now.time_zone());
        let all_day = invite.all_day;
        self.calendar.invite = Some(invite);
        if self.calendar_open_day(start.date(), Source::Other) && !all_day {
            self.calendar.grid.reveal(end.hour());
        }
    }

    /// The signed-out calendar account whose `Add Calendar` takes the day view's place, when the
    /// open conversation's invite is to it, I could answer it, and it hasn't been answered.
    pub(super) fn calendar_takeover(&self) -> Option<String> {
        let invite = self.calendar.invite.as_ref()?;
        let email = self.calendar_invite_needs_sign_in(invite)?;
        (invite.can_respond(&email)
            && !self
                .account
                .settings()
                .calendar_invite_hint
                .get(&email)
                .copied()
                .unwrap_or(false))
        .then_some(email)
    }

    /// Queues my answer to the open event (to the series when `all`), and says so.
    pub(super) fn calendar_respond(&mut self, answer: Rsvp, all: bool) {
        let Some(key) = self
            .calendar
            .details
            .as_ref()
            .map(|details| details.key.clone())
        else {
            return;
        };
        let now = self.now.clone();
        let Some((event, source, _)) = self.calendar_detailed(&key, &now) else {
            return;
        };
        let status = match answer {
            Rsvp::Accepted => "accepted",
            Rsvp::Tentative => "tentative",
            Rsvp::Declined => "declined",
            Rsvp::NeedsAction => return,
        };
        if event.id.is_none() {
            let calendar = self.calendar.invite.as_ref().map_or_else(
                || self.account.email().to_owned(),
                |invite| self.calendar_invite_account(invite),
            );
            let thread = self.thread.as_ref().map(|thread| thread.id().to_owned());
            let links = source
                .ical_uid
                .as_deref()
                .and_then(|uid| self.calendar_invite_links(uid));
            self.account
                .calendar()
                .queue(exo_store::CalendarChange::RespondToInvite {
                    calendar: calendar.clone(),
                    invite: source,
                    status: status.to_owned(),
                    thread,
                    links,
                });
            if let Some(invite) = &mut self.calendar.invite {
                invite.answer(&calendar, answer);
            }
            self.commands.toast(Toast::new(self.rsvp_toast(answer)));
            return;
        }
        self.account
            .calendar()
            .queue(exo_store::CalendarChange::Respond {
                calendar: event.calendar_id.clone(),
                event: source,
                status: status.to_owned(),
                all,
            });
        let text = if self.snapshot.connection.is_online() {
            let scope = if all { "all events" } else { "this event" };
            format!("Responded {} to {scope}.", answer.word())
        } else {
            "RSVP will be sent when online.".to_owned()
        };
        self.commands.toast(Toast::new(text));
    }

    /// The links of the mail with invite `uid`, in the open conversation, when the invitation is
    /// from a sender Calendar doesn't know (it carries a link that marks them as known).
    fn calendar_invite_links(&self, uid: &str) -> Option<exo_store::InviteLinks> {
        let message = self.thread.as_ref()?.invite_message(uid)?;
        let calendar = &self.bodies.facts(message)?.calendar;
        Some(exo_store::InviteLinks {
            mark_sender_known: calendar.mark_sender_known.clone()?,
            accepted: calendar.rsvp_links.accepted.clone(),
            declined: calendar.rsvp_links.declined.clone(),
            tentative: calendar.rsvp_links.tentative.clone(),
        })
    }

    /// An invite card's answer, given to the calendar's event with its UID, which is added from
    /// the invite if need be. `invite` has my answer recorded. `mouse` says it came from a click,
    /// which earns the hint that `tab` gets there. With `RSVP + Mark Done` on, the conversation is
    /// marked done too, with that toast.
    pub(crate) fn calendar_respond_to_invite(
        &mut self,
        invite: Invite,
        response: Response,
        mouse: bool,
    ) {
        // To a signed-out calendar account, which signs in first: the `RSVP` alert, or
        // `You're Offline` while offline.
        if let Some(email) = self.calendar_invite_needs_sign_in(&invite) {
            if mouse {
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["tab"])
                        .text("to RSVP instantly!"),
                );
            }
            if self.snapshot.connection.is_online() {
                self.calendar.prompt = Some(Prompt::SignIn {
                    email,
                    invite: Box::new(invite),
                    response,
                    provider: provider::ProviderPicker::default(),
                    selected: 0,
                });
            } else {
                self.pickers.inform(
                    "You're Offline",
                    "Please go online to respond to this invitation.".to_owned(),
                );
            }
            return;
        }
        let mark_done = self.rsvp_marks_done();
        // The card shows this answer now, not the calendar's from before it.
        self.calendar.answers.1.insert(invite.uid.clone(), response);
        // To the calendar of the account it invites.
        let calendar = self.calendar_invite_account(&invite);
        let thread = self.thread.as_ref().map(|thread| thread.id().to_owned());
        self.account
            .calendar()
            .queue(exo_store::CalendarChange::RespondToInvite {
                calendar,
                invite: invite_event(&invite),
                status: response.status().to_owned(),
                thread,
                links: self.calendar_invite_links(&invite.uid),
            });
        if self
            .calendar
            .invite
            .as_ref()
            .is_some_and(|shown| shown.uid == invite.uid)
        {
            self.calendar.invite = Some(invite);
        }
        if mark_done {
            self.rsvp_mark_done(response);
        } else {
            self.commands.toast(Toast::new(self.rsvp_toast(response)));
        }
        // After a mouse RSVP, a hint that `tab` gets there.
        if mouse {
            let then = if response == Response::NeedsAction {
                "to add to your calendar instantly!"
            } else {
                "to RSVP instantly!"
            };
            self.commands
                .hint(Hint::new().text("Next time, hit").keys(["tab"]).text(then));
        }
    }

    /// The open thread's invite cards show my answer as given here since it opened, else as the
    /// calendar has it (the `.ics` has the answer from when it was sent).
    pub(super) fn calendar_read_answers(&mut self) {
        let Some(thread) = &self.thread else {
            self.calendar.answers = (String::new(), BTreeMap::new());
            return;
        };
        if self.calendar.answers.0 != thread.id() {
            self.calendar.answers = (thread.id().to_owned(), BTreeMap::new());
        }
        let uids = thread.invite_uids();
        if uids.is_empty() {
            return;
        }
        let data = self.account.calendar().data();
        let me = self.account.email().to_owned();
        for uid in uids {
            let calendar = || {
                let stored = data
                    .events
                    .iter()
                    .find(|stored| stored.event.ical_uid.as_deref() == Some(uid.as_str()))?;
                let mine = stored.event.attendees.iter().find(|attendee| {
                    attendee.is_self == Some(true)
                        || attendee
                            .email
                            .as_deref()
                            .is_some_and(|email| email.eq_ignore_ascii_case(&me))
                })?;
                Response::parse(mine.response_status.as_deref()?)
            };
            let Some(answer) = self.calendar.answers.1.get(&uid).copied().or_else(calendar) else {
                continue;
            };
            if let Some(thread) = &mut self.thread {
                thread.show_calendar_answer(&uid, answer);
            }
        }
    }
}

/// An invite's event as Calendar would have it: what the event details show of an invite the
/// calendar doesn't have, and what's imported to answer it.
pub(crate) fn invite_event(invite: &Invite) -> gmail::calendar::Event {
    use gmail::calendar::{Event as ApiEvent, EventAttendee, EventDateTime, EventPerson};
    let (start, end) = if invite.all_day
        && let Some((first, last)) = invite.dates
    {
        (
            EventDateTime {
                date: Some(first.to_string()),
                ..EventDateTime::default()
            },
            EventDateTime {
                date: Some(last.to_string()),
                ..EventDateTime::default()
            },
        )
    } else {
        let at = |time: jiff::Timestamp| EventDateTime {
            date_time: Some(time.to_string()),
            ..EventDateTime::default()
        };
        (at(invite.start), at(invite.end))
    };
    let organizer = invite.organizer.as_ref().map(|(name, email)| EventPerson {
        email: Some(email.clone()),
        display_name: (!name.is_empty()).then(|| name.clone()),
        ..EventPerson::default()
    });
    ApiEvent {
        ical_uid: Some(invite.uid.clone()),
        summary: (!invite.summary.is_empty()).then(|| invite.summary.clone()),
        description: (!invite.description.is_empty()).then(|| invite.description.clone()),
        location: (!invite.location.is_empty()).then(|| invite.location.clone()),
        start: Some(start),
        end: Some(end),
        attendees: invite
            .attendees
            .iter()
            .map(|(name, email, response)| EventAttendee {
                email: Some(email.clone()),
                display_name: (!name.is_empty()).then(|| name.clone()),
                organizer: invite
                    .organizer
                    .as_ref()
                    .is_some_and(|(_, organizer)| organizer.eq_ignore_ascii_case(email))
                    .then_some(true),
                response_status: Some(response.status().to_owned()),
                ..EventAttendee::default()
            })
            .collect(),
        organizer,
        ..ApiEvent::default()
    }
}

#[cfg(test)]
mod tests {
    use exo_ui::CalendarColor;
    use jiff::civil::Date;

    use super::*;
    use crate::calendar::event::Event;

    #[test]
    fn invite_event_emits_date_fields_for_all_day_invites() {
        let all_day_ics = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:offsite-2026@brightmoor.co\r\n",
            "DTSTART;VALUE=DATE:20261207\r\n",
            "DTEND;VALUE=DATE:20261209\r\n",
            "SUMMARY:Offsite\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let parsed = crate::thread::invite::parse(all_day_ics).expect("parses");
        let api = invite_event(&parsed);
        assert_eq!(
            api.start.as_ref().and_then(|s| s.date.as_deref()),
            Some("2026-12-07")
        );
        assert_eq!(
            api.start.as_ref().and_then(|s| s.date_time.as_deref()),
            None
        );
        assert_eq!(
            api.end.as_ref().and_then(|e| e.date.as_deref()),
            Some("2026-12-09")
        );
        assert_eq!(api.end.as_ref().and_then(|e| e.date_time.as_deref()), None);

        let tz = jiff::tz::TimeZone::get("America/Los_Angeles").expect("zone");
        let from_inv = Event::from_invite(&parsed, CalendarColor::BLUE, "me", &tz);
        assert!(from_inv.all_day && from_inv.in_all_day_row());
        assert_eq!(from_inv.start.date(), Date::constant(2026, 12, 7));
        assert_eq!(from_inv.end.date(), Date::constant(2026, 12, 9));
    }
}
