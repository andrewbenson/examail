//! Calendar invite card state, RSVP keyboard navigation, and calendar prefill helpers on
//! [`ThreadView`].

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use egui::Ui;
use exo_store::{Attachment, MessageDetail};

use super::{Action, Layout, ThreadView, invite};
use crate::body::Bodies;

/// Invite-card fetch tracking, parsed invites, keyboard focus, calendar accounts, and card
/// interaction state.
pub(super) struct InviteState {
    pub(super) asked: HashSet<String>,
    /// Parsed calendar invitations, keyed by message id.
    pub(super) invites: HashMap<String, invite::Invite>,
    /// The focused RSVP button on the focused message's invite card, if any.
    pub(super) focus: Option<usize>,
    /// Where the invite photos are (`assets/images`).
    pub(super) images: PathBuf,
    /// The account's other calendar accounts, who may be the invited one
    /// ([`invite::Invite::calendar_account`]), and those of them Google signed out, which sign
    /// in before they answer.
    pub(super) calendar_accounts: Vec<String>,
    pub(super) signed_out: Vec<String>,
    /// The invite cards whose About shows every line (`Show Less`).
    pub(super) about_open: HashSet<String>,
    /// The invite's When the pointer is settling on, by message.
    pub(super) when_intent: exo_ui::HoverIntent<String>,
}

impl InviteState {
    pub(super) fn new(images: PathBuf) -> Self {
        Self {
            asked: HashSet::new(),
            invites: HashMap::new(),
            focus: None,
            images,
            calendar_accounts: Vec::new(),
            signed_out: Vec::new(),
            about_open: HashSet::new(),
            when_intent: exo_ui::HoverIntent::default(),
        }
    }
}

impl ThreadView {
    /// The `.ics` attachments to fetch for invite cards, by message. Each message is looked at
    /// once.
    pub fn invite_requests(&mut self) -> Vec<(String, Attachment)> {
        let Some(detail) = self.detail.clone() else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        for message in Self::shown(&detail) {
            if self.invite_state.asked.contains(&message.id) {
                continue;
            }
            self.invite_state.asked.insert(message.id.clone());
            requests
                .extend(invite::attachment(message).map(|ics| (message.id.clone(), ics.clone())));
        }
        requests
    }

    /// Takes the `.ics` of message `id`.
    pub fn set_invite(&mut self, id: &str, ics: &[u8]) {
        if let Some(parsed) = invite::parse(&String::from_utf8_lossy(ics)) {
            self.invite_state.invites.insert(id.to_owned(), parsed);
        }
    }

    /// What message `id`'s invite card is drawn with. `calendar` is the notification's facts.
    pub(super) fn invite_context<'a>(
        &'a self,
        id: &str,
        calendar: Option<&'a exo_mail::CalendarData>,
    ) -> Option<invite::Context<'a>> {
        let me = self
            .invite_state
            .invites
            .get(id)
            .map_or(self.me.as_str(), |parsed| {
                parsed.calendar_account(&self.me, &self.invite_state.calendar_accounts)
            });
        Some(invite::Context {
            me,
            user: &self.me,
            now: &self.action.now,
            message: self.message(id)?,
            calendar,
            about_open: self.invite_state.about_open.contains(id),
        })
    }

    /// The height an invite card, and the guest's note above it, add above message `id`'s body.
    pub(super) fn invite_block(&self, ui: &Ui, id: &str, width: f32, bodies: &Bodies) -> f32 {
        let calendar = bodies.facts(id).map(|facts| &facts.calendar);
        let (Some(parsed), Some(context)) = (
            self.invite_state.invites.get(id),
            self.invite_context(id, calendar),
        ) else {
            return 0.0;
        };
        let note = invite::note(parsed, calendar)
            .map_or(0.0, |note| invite::note_height(ui.ctx(), &note, width));
        note + invite::height(ui, parsed, &context, width) + invite::MARGIN_BOTTOM
    }

    /// Whether message `id`'s body is the notification its invite card stands in for.
    pub(super) fn hides_body(&self, id: &str, bodies: &Bodies) -> bool {
        let (Some(parsed), Some(message)) = (self.invite_state.invites.get(id), self.message(id))
        else {
            return false;
        };
        invite::hides_body(
            parsed,
            message,
            bodies.facts(id).is_some_and(|facts| facts.calendar.gcal),
        )
    }

    /// The focused message's invite, if it's expanded and I can answer it or add it to my
    /// calendar.
    fn focusable_invite(&self) -> Option<&invite::Invite> {
        let focused = self.focus.as_ref()?;
        let item = self
            .items
            .iter()
            .find(|item| &item.id == focused)
            .filter(|item| item.expanded)?;
        self.invite_state.invites.get(&item.id).filter(|parsed| {
            parsed.can_respond(self.invite_me(parsed))
                || parsed.can_add_to_calendar(self.invite_me(parsed))
        })
    }

    /// Who I am on `parsed` ([`invite::Invite::calendar_account`]).
    pub(super) fn invite_me<'a>(&'a self, parsed: &'a invite::Invite) -> &'a str {
        parsed.calendar_account(&self.me, &self.invite_state.calendar_accounts)
    }

    /// The account's other calendar accounts, any of whom an invite may be to, and those of
    /// them signed out.
    pub fn set_calendar_accounts(&mut self, accounts: Vec<String>, signed_out: Vec<String>) {
        self.invite_state.calendar_accounts = accounts;
        self.invite_state.signed_out = signed_out;
    }

    /// Whether `parsed` is to a signed-out calendar account, whose answer waits for it to sign
    /// in. Meanwhile the card doesn't show the answer as given.
    pub(super) fn needs_sign_in(&self, parsed: &invite::Invite) -> bool {
        self.invite_state
            .signed_out
            .iter()
            .any(|email| email == self.invite_me(parsed))
    }

    /// `tab` in the messages: moves the keyboard to the focused message's invite card, on its
    /// first button. Returns whether there was one.
    pub fn focus_invite(&mut self) -> bool {
        let found = self.focusable_invite().is_some();
        if found {
            self.invite_state.focus = Some(0);
        }
        found
    }

    /// Whether `tab` would take the keyboard to an invite card.
    pub fn can_focus_invite(&self) -> bool {
        self.invite_state.focus.is_none() && self.focusable_invite().is_some()
    }

    /// Whether the invite card has the keyboard.
    pub fn invite_focused(&self) -> bool {
        self.invite_state.focus.is_some() && self.focusable_invite().is_some()
    }

    /// → / ↓ / tab and ← / ↑ in the card: moves to the next or previous button, wrapping round.
    /// `Add To Calendar` is alone, so the keyboard stays on it.
    pub fn move_invite_focus(&mut self, delta: isize) {
        let answerable = self
            .focusable_invite()
            .is_some_and(|parsed| parsed.can_respond(self.invite_me(parsed)));
        if let Some(focus) = self.invite_state.focus.as_mut().filter(|_| answerable) {
            *focus = (*focus as isize + delta).rem_euclid(invite::ANSWERS.len() as isize) as usize;
        }
    }

    /// Escape in the card: gives the keyboard back to the messages.
    pub fn blur_invite(&mut self) {
        self.invite_state.focus = None;
    }

    /// Enter or space in the card: answers with the focused button, unless that's the current
    /// answer. Returns the invite and the answer. The invite has the answer recorded, unless its
    /// calendar account has to sign in first.
    pub fn submit_invite(&mut self) -> Option<(invite::Invite, invite::Response)> {
        let focus = self.invite_state.focus?;
        let id = self.focus.clone()?;
        let parsed = self.focusable_invite()?;
        let me = self.invite_me(parsed).to_owned();
        let response = if parsed.can_respond(&me) {
            invite::ANSWERS[focus]
        } else {
            invite::Response::NeedsAction
        };
        if parsed.answer_of(&me) == Some(response) {
            return None;
        }
        if self.needs_sign_in(parsed) {
            return Some((parsed.clone(), response));
        }
        let mut answered = parsed.clone();
        answered.answer(&me, response);
        self.invite_state.invites.insert(id, answered.clone());
        Some((answered, response))
    }

    /// Shows the calendar's answer to the invite with UID `uid` on its cards, rather than the
    /// `.ics`'s answer. It's what was answered before, from here or elsewhere.
    pub fn show_calendar_answer(&mut self, uid: &str, response: invite::Response) {
        for parsed in self
            .invite_state
            .invites
            .values_mut()
            .filter(|parsed| parsed.uid == uid)
        {
            let me = parsed
                .calendar_account(&self.me, &self.invite_state.calendar_accounts)
                .to_owned();
            if parsed.answer_of(&me) != Some(response) {
                parsed.answer(&me, response);
            }
        }
    }

    /// The conversation's invitation you can answer: the
    /// invite of the message whose id is the thread's, else of its first message.
    pub fn conversation_invite(&self) -> Option<&invite::Invite> {
        let first = self.detail.as_ref()?.messages.first()?;
        self.invite_state
            .invites
            .get(&self.id)
            .or_else(|| self.invite_state.invites.get(&first.id))
            .filter(|parsed| parsed.can_respond(self.invite_me(parsed)))
    }

    /// A new event for this conversation: its subject, and everyone on its messages but me, once
    /// each.
    pub fn event_prefill(&self) -> (String, Vec<(Option<String>, String)>) {
        let Some(detail) = &self.detail else {
            return Default::default();
        };
        let mut guests: Vec<(Option<String>, String)> = Vec::new();
        for contact in detail
            .messages
            .iter()
            .flat_map(|message| message.from.iter().chain(&message.to).chain(&message.cc))
        {
            if !contact.is_me
                && !contact.email.eq_ignore_ascii_case(&self.me)
                && !guests
                    .iter()
                    .any(|(_, email)| email.eq_ignore_ascii_case(&contact.email))
            {
                guests.push((contact.name.clone(), contact.email.clone()));
            }
        }
        (detail.subject.clone(), guests)
    }

    /// The message whose invite has UID `uid`.
    pub fn invite_message(&self, uid: &str) -> Option<&str> {
        self.invite_state
            .invites
            .iter()
            .find(|(_, parsed)| parsed.uid == uid)
            .map(|(id, _)| id.as_str())
    }

    /// The UIDs of the thread's invites.
    pub fn invite_uids(&self) -> Vec<String> {
        self.invite_state
            .invites
            .values()
            .map(|parsed| parsed.uid.clone())
            .collect()
    }

    /// The invite the right pane's calendar shows: the focused message's, or with none focused,
    /// that of the first message that has one.
    pub fn invite(&self) -> Option<&invite::Invite> {
        match &self.focus {
            Some(focused) => self.invite_state.invites.get(focused),
            None => self
                .detail
                .as_ref()?
                .messages
                .iter()
                .find_map(|message| self.invite_state.invites.get(&message.id)),
        }
    }

    /// Paints an expanded message's invite card (if any), handles its clicks, and returns the
    /// hovered `When` start timestamp if the pointer is on it.
    pub(super) fn paint_invite_card(
        &mut self,
        ui: &mut Ui,
        layout: &Layout,
        message: &MessageDetail,
        content_top: f32,
        bodies: &Bodies,
        actions: &mut Vec<Action>,
    ) -> Option<(String, jiff::Timestamp)> {
        let item_id = message.id.as_str();
        let parsed = self.invite_state.invites.get(item_id)?.clone();
        let id = ui.id().with(("invite", item_id));
        let focused = self
            .invite_state
            .focus
            .filter(|_| self.focus.as_deref() == Some(item_id));
        let calendar = bodies.facts(item_id).map(|facts| &facts.calendar);
        let context = invite::Context {
            me: parsed.calendar_account(&self.me, &self.invite_state.calendar_accounts),
            user: &self.me,
            now: &self.action.now,
            message,
            calendar,
            about_open: self.invite_state.about_open.contains(item_id),
        };
        let mut card_top = content_top;
        if let Some(note) = invite::note(&parsed, calendar) {
            actions.extend(
                invite::show_note(
                    ui,
                    layout.inner.left(),
                    card_top,
                    layout.inner.width(),
                    &note,
                    id,
                )
                .map(Action::OpenUrl),
            );
            card_top += invite::note_height(ui.ctx(), &note, layout.inner.width());
        }
        let shown = invite::show(
            ui,
            layout.inner.left(),
            card_top,
            layout.inner.width(),
            &parsed,
            &context,
            &self.invite_state.images,
            id,
            focused,
        );
        let on_when = shown.on_when.then(|| (item_id.to_owned(), parsed.start));
        self.hover.named.extend(shown.names);
        match shown.clicked {
            Some(invite::CardAction::Dial(tel)) => actions.push(Action::Dial(tel)),
            Some(invite::CardAction::Open(url)) => actions.push(Action::OpenUrl(url)),
            Some(invite::CardAction::Respond(response)) if self.needs_sign_in(&parsed) => {
                actions.push(Action::RespondToInvite(parsed, response))
            }
            Some(invite::CardAction::Respond(response)) => {
                let mut answered = parsed.clone();
                answered.answer(
                    parsed.calendar_account(&self.me, &self.invite_state.calendar_accounts),
                    response,
                );
                actions.push(Action::RespondToInvite(answered.clone(), response));
                self.invite_state
                    .invites
                    .insert(item_id.to_owned(), answered);
            }
            Some(invite::CardAction::ToggleAbout) => {
                let open = &mut self.invite_state.about_open;
                if !open.remove(item_id) {
                    open.insert(item_id.to_owned());
                }
            }
            None => {}
        }
        on_when
    }
}
