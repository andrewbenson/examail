//! Message triage, expansion/navigation commands, `ViewContext` synchronization, and pointer/hover
//! interactions on [`ThreadView`].

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use egui::{Pos2, Rect, Ui};
use exo_commands::Region;
use exo_store::ThreadDetail;
use jiff::Zoned;

use super::{
    Action, Item, Layout, SELECTED_CONTACTS, SELECTED_SUBJECT, ThreadView, focus, footer, header,
    layout::open_file, message, person_of, selection, tooltips,
};
use crate::{
    ViewContext,
    body::{Bodies, Hit, PointerArea, PointerState},
    format::names::Person,
};

/// Triage, keyboard focus zone, header, environment, and arrival state for the open conversation.
pub(super) struct ActionState {
    /// Which part of the thread view currently holds keyboard focus.
    pub(super) zone: focus::Zone,
    /// Tab index of the last focused header control (defaults to 1).
    pub(super) header_tab: u8,
    /// Header status line (pending bulk action, one-click action, or unsubscribe).
    pub(super) status: Option<header::Status>,
    /// Label chips drawn in the header.
    pub(super) chips: Vec<String>,
    /// Whether messages arrived that are held behind the Show New Messages bar.
    pub(super) has_new: bool,
    /// A message to focus once the thread has it (from a notification).
    pub(super) wanted: Option<String>,
    /// Messages to show as soon as a copy of the thread has them: replies sent from here.
    pub(super) admitted: HashSet<String>,
    /// The thread opened unread. It's marked read once its first copy is in, so the messages that
    /// were unread open expanded.
    pub(super) read_on_open: bool,
    /// The bulk action the conversation waits on, as its header's status says it.
    pub(super) pending: Option<String>,
    /// Your domains, whose mailing lists aren't unsubscribed from.
    pub(super) own_domains: Vec<String>,
    /// The view includes Trash (the Trash list, or a search), so trashed messages show as any
    /// other.
    pub(super) trash_shown: bool,
    /// The view is Spam.
    pub(super) spam_view: bool,
    /// When the account first signed in to Exo on this Mac, if known.
    pub(super) joined: Option<jiff::Timestamp>,
    /// The hint under the last message: the intro hint under the only one, else the calendar
    /// tip.
    pub(super) hint: Option<footer::Hint>,
    /// Clock time used for relative dates and new-account hints.
    pub(super) now: Zoned,
    /// Whether Gmail can be reached.
    pub(super) online: bool,
    /// Whether headers show full sender names (`showSenderFullNames`).
    pub(super) full_names: bool,
}

impl ActionState {
    pub(super) fn new(now: Zoned) -> Self {
        Self {
            zone: focus::Zone::Messages,
            header_tab: 1,
            status: None,
            chips: Vec::new(),
            has_new: false,
            wanted: None,
            admitted: HashSet::new(),
            read_on_open: false,
            pending: None,
            own_domains: Vec::new(),
            trash_shown: false,
            spam_view: false,
            joined: None,
            hint: None,
            now,
            online: true,
            full_names: false,
        }
    }
}

/// Body pointer/selection/hover-intent tracking and header participant hover/tooltip state.
#[derive(Default)]
pub(super) struct HoverState {
    /// Multi-click, drag selection, and click-modifier tracking for bodies.
    pub(super) pointer: PointerState,
    /// Clicks in a row on headers that expanded, or collapsed, a message. The third suggests ⇧O.
    pub(super) clicks_in_a_row: (u8, u8),
    /// The message whose body is currently under the pointer.
    pub(super) hovered_body: Option<String>,
    /// Where each expanded body was drawn this frame.
    pub(super) body_origins: HashMap<String, Pos2>,
    /// Hover intent for a `mailto:` link in a body.
    pub(super) mailto_intent: exo_ui::HoverIntent<String>,
    /// Hover intent for a detected date in a body, and its active `Create Event` tooltip.
    pub(super) date_intent: exo_ui::HoverIntent<(String, String)>,
    pub(super) date_tip: Option<selection::DateTip>,
    /// A hovered participant or `mailto:` contact settled on this frame, emitted as `ShowContact`.
    pub(super) contact: Option<Person>,
    /// The participants' names the headers drew this frame, the name the pointer is settling
    /// on, and the header's tooltips.
    pub(super) named: message::Named,
    pub(super) name_intent: exo_ui::HoverIntent<exo_store::Contact>,
    pub(super) tooltips: tooltips::Tooltips,
    /// The selection in the header's text (the bodies keep their own).
    pub(super) selection: exo_ui::TextSelection,
}

impl ThreadView {
    /// Synchronizes the shared per-frame [`ViewContext`] onto this conversation view.
    pub(crate) fn sync_context(&mut self, context: &ViewContext) {
        let trash_changed = self.action.trash_shown != context.trash_shown;
        self.action.now = context.now.clone();
        self.action.online = context.online;
        self.action.full_names = context.full_names;
        self.action.joined = context.joined;
        self.action.pending = context.pending.get(&self.id).cloned();
        self.action.own_domains.clone_from(&context.own_domains);
        self.action.trash_shown = context.trash_shown;
        self.action.spam_view = context.spam_view;
        self.invite_state
            .calendar_accounts
            .clone_from(&context.calendar_accounts);
        self.invite_state.signed_out.clone_from(&context.signed_out);
        if trash_changed && self.detail.is_some() {
            self.update_items(false);
        }
    }

    /// The header's controls, in tab order.
    pub(super) fn header_controls(&self) -> Vec<header::Control> {
        header::controls(self.action.status.as_ref(), !self.action.spam_view)
    }

    /// Says the conversation was unread when it opened. [`Self::take_read_on_open`] says when to
    /// mark it read.
    pub fn set_read_on_open(&mut self) {
        self.action.read_on_open = true;
    }

    /// Whether to mark the conversation read now: once, after its first copy has laid out its
    /// messages.
    pub fn take_read_on_open(&mut self) -> bool {
        self.detail.is_some() && std::mem::take(&mut self.action.read_on_open)
    }

    /// The one-click actions of messages `ids`.
    fn actions_of(&self, ids: &[&str], bodies: &Bodies) -> Vec<exo_mail::Action> {
        let Some(detail) = &self.detail else {
            return Vec::new();
        };
        crate::email_actions::of(detail, ids, &self.action.own_domains, |id| {
            bodies.facts(id).map(|facts| facts.actions.clone())
        })
    }

    /// The conversation's last one-click action, which the header's status line and the
    /// palette offer.
    pub fn conversation_action(&self, bodies: &Bodies) -> Option<exo_mail::Action> {
        let ids: Vec<&str> = self
            .detail
            .as_ref()?
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        self.actions_of(&ids, bodies).pop()
    }

    /// The focused message's one-click actions, which ⌘O lists first.
    pub fn focused_actions(&self, bodies: &Bodies) -> Vec<exo_mail::Action> {
        self.focus
            .as_deref()
            .map(|id| self.actions_of(&[id], bodies))
            .unwrap_or_default()
    }

    /// Takes the latest copy of the thread. The first copy shows every message, with the unread
    /// ones expanded, and always the last that isn't in Trash. Later copies keep what the user
    /// expanded, and hold messages that arrived back behind the Show New Messages bar, except
    /// failed or scheduled sends and replies sent from here.
    pub fn set_detail(&mut self, detail: Arc<ThreadDetail>) {
        if self
            .detail
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &detail))
        {
            return;
        }
        let first = self.detail.is_none();
        self.detail = Some(detail);
        self.update_items(first);
        if let Some(wanted) = self.action.wanted.take() {
            self.focus_message(&wanted);
        }
    }

    /// Lists the thread's messages, adding the ones it doesn't show yet when `add_new`.
    pub(super) fn update_items(&mut self, add_new: bool) {
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let mut items = Vec::new();
        self.action.has_new = false;
        for message in Self::shown(&detail) {
            let trashed = message.has_label("TRASH") && !self.action.trash_shown;
            let item = match self.items.iter().find(|item| item.id == message.id) {
                Some(item) => Item {
                    trashed,
                    ..item.clone()
                },
                None if add_new
                    || message.is_draft()
                    || self.action.admitted.remove(&message.id) =>
                {
                    Item {
                        id: message.id.clone(),
                        expanded: !trashed && message.is_unread(),
                        header_expanded: false,
                        show_quoted: false,
                        trashed,
                    }
                }
                None => {
                    self.action.has_new = true;
                    continue;
                }
            };
            items.push(item);
        }
        if let Some(last) = items.iter_mut().rev().find(|item| !item.trashed) {
            last.expanded = true;
        }
        if self
            .focus
            .as_ref()
            .is_none_or(|focus| !items.iter().any(|item| &item.id == focus))
        {
            self.focus = items
                .iter()
                .find(|item| item.expanded)
                .map(|item| item.id.clone());
        }
        self.items = items;
        if !self.action.has_new && self.action.zone == focus::Zone::NewMessages {
            self.action.zone = focus::Zone::Messages;
        }
    }

    /// Whether messages arrived that the Show New Messages bar holds back.
    pub fn has_new_messages(&self) -> bool {
        self.action.has_new
    }

    /// Shows the messages the bar held back and focuses the first of
    /// them. Returns whether there were any.
    pub fn show_new_messages(&mut self) -> bool {
        if !self.action.has_new {
            return false;
        }
        let known: HashSet<String> = self.items.iter().map(|item| item.id.clone()).collect();
        self.update_items(true);
        self.action.zone = focus::Zone::Messages;
        if let Some(at) = self.items.iter().position(|item| !known.contains(&item.id)) {
            self.items[at].expanded = true;
            self.focus_at(at, 1);
        }
        true
    }

    /// ⇧N: shows the new messages, or else focuses the last message.
    pub fn show_new_or_last(&mut self) -> bool {
        if self.show_new_messages() {
            return true;
        }
        let last = self.items.len().saturating_sub(1);
        self.focus_at(last, 1);
        false
    }

    /// Focuses message `id`, expanded and in view, now or once a copy of the thread has it.
    pub fn focus_message(&mut self, id: &str) {
        // A message the Show New Messages bar holds back shows with the rest.
        if self.action.has_new
            && !self.items.iter().any(|item| item.id == id)
            && self.message(id).is_some()
        {
            self.update_items(true);
        }
        match self.items.iter().position(|item| item.id == id) {
            Some(at) => {
                self.items[at].expanded = true;
                self.focus_at(at, 1);
            }
            None => self.action.wanted = Some(id.to_owned()),
        }
    }

    /// Message `from` is now `to` (Gmail's copy of a sent reply took its place). It keeps its
    /// expansion and the focus.
    pub fn rename_message(&mut self, from: &str, to: &str) {
        for item in self.items.iter_mut().filter(|item| item.id == from) {
            to.clone_into(&mut item.id);
        }
        if self.focus.as_deref() == Some(from) {
            self.focus = Some(to.to_owned());
        }
    }

    /// Whether message `index` can collapse: it isn't the last message that
    /// isn't in Trash. A trashed one always can, back into its marker.
    pub(super) fn collapsible(&self, index: usize) -> bool {
        self.items
            .iter()
            .rposition(|item| !item.trashed)
            .is_none_or(|last| index != last)
    }

    /// Escape: collapses the focused message if it can, and says whether it did.
    pub fn collapse_current(&mut self) -> bool {
        match self.focus_index() {
            Some(at) if self.items[at].expanded && self.collapsible(at) => {
                self.items[at].expanded = false;
                self.items[at].show_quoted = false;
                true
            }
            _ => false,
        }
    }

    /// Whether the focused message is collapsed (Enter expands it).
    pub fn focus_is_collapsed(&self) -> bool {
        self.focus_index()
            .is_some_and(|at| !self.items[at].expanded)
    }

    /// Enter on a collapsed message: expands it.
    pub fn expand_current(&mut self) {
        if let Some(at) = self.focus_index() {
            self.items[at].expanded = true;
        }
    }

    /// O: cycles the focused message through collapsed → expanded → quoted text shown →
    /// collapsed. A message that can't collapse only toggles its quoted text.
    pub fn toggle_current(&mut self, bodies: &Bodies) {
        let Some(at) = self.focus_index() else { return };
        let has_quoted = bodies
            .facts(&self.items[at].id)
            .is_some_and(|facts| facts.has_quoted);
        let collapsible = self.collapsible(at);
        let item = &mut self.items[at];
        if !item.expanded {
            item.expanded = true;
        } else if has_quoted && !item.show_quoted {
            item.show_quoted = true;
        } else if collapsible {
            item.expanded = false;
            item.show_quoted = false;
        } else {
            item.show_quoted = false;
        }
    }

    /// ⇧O: expands every message, or collapses every one that can when all are expanded.
    pub fn toggle_all(&mut self) {
        let expand = self.items.iter().any(|item| !item.expanded);
        for at in 0..self.items.len() {
            let expanded = expand || !self.collapsible(at);
            let item = &mut self.items[at];
            item.expanded = expanded;
            if !item.expanded {
                item.show_quoted = false;
            }
        }
    }

    /// ⇧H: shows or hides the focused message's full header.
    pub fn toggle_header(&mut self) {
        if let Some(at) = self.focus_index() {
            self.items[at].header_expanded = !self.items[at].header_expanded;
        }
    }

    /// N / P: focuses the next (1) or previous (-1) message and scrolls to it.
    pub fn next_message(&mut self, delta: isize) {
        let Some(at) = self.focus_index() else { return };
        let to = self.step(at, delta);
        self.focus_at(to, delta);
    }

    /// The message `delta` (±1) from `at` that the cursor can land on, which is a trashed one
    /// only if it's expanded. Past the end it's the last message, and past the start it's `at`.
    pub(super) fn step(&self, at: usize, delta: isize) -> usize {
        let lands = |item: &Item| !(item.trashed && !item.expanded);
        let mut to = at;
        loop {
            match to
                .checked_add_signed(delta)
                .filter(|to| *to < self.items.len())
            {
                Some(next) if lands(&self.items[next]) => return next,
                Some(next) => to = next,
                None if delta > 0 => {
                    return self
                        .items
                        .len()
                        .checked_sub(1)
                        .filter(|last| lands(&self.items[*last]))
                        .unwrap_or(at);
                }
                None => return at,
            }
        }
    }

    pub(super) fn focus_at(&mut self, at: usize, direction: isize) {
        if let Some(item) = self.items.get(at) {
            self.focus = Some(item.id.clone());
            self.scroll_state.reveal = Some((item.id.clone(), direction));
        }
    }

    /// The message whose body is under the pointer.
    pub fn hovered_body(&self) -> Option<&str> {
        self.hover.hovered_body.as_deref()
    }

    /// The link under the pointer, for the link preview.
    pub fn hovered_link(&self, bodies: &Bodies) -> Option<String> {
        match bodies.hovered(self.hover.hovered_body.as_deref()?)? {
            Hit::Link { href } => Some(href.clone()),
            _ => None,
        }
    }

    /// The text selected in the header, if any, for ⌘C.
    pub fn selected_text(&self) -> Option<String> {
        self.hover.selection.text()
    }

    /// A right-click at `at`, which on the header's selectable text selects the word under it,
    /// unless it's on the selection.
    pub(crate) fn select_word_at(&mut self, at: egui::Pos2) {
        self.hover.selection.select_word_at(at);
    }

    /// Where the header's selection starts on screen, on its baseline, and its text's size.
    pub(crate) fn selection_start(&self) -> Option<(egui::Pos2, f32)> {
        self.hover.selection.start()
    }

    /// A press and drag in the header's text selects it and clears the bodies' selection. Letting
    /// go on the subject with text selected suggests Copy Subject. Letting go with the selection
    /// in a full header's addresses suggests Copy Recipients, while the messages have the
    /// keyboard.
    pub(super) fn select_header_text(
        &mut self,
        ui: &Ui,
        bodies: &Bodies,
        region: Region,
        actions: &mut Vec<Action>,
    ) {
        let (started, released) = self
            .hover
            .selection
            .interact(ui, ui.id().with("header-text"));
        if started {
            bodies.clear_selection();
        }
        let Some(released) = released else { return };
        let subject = released.on_one_piece
            && released.anchor == SELECTED_SUBJECT
            && released.focus == SELECTED_SUBJECT;
        if subject {
            actions.push(Action::CopyHint { subject: true });
        } else if region == Region::ThreadMessages
            && (released.anchor == SELECTED_CONTACTS || released.focus == SELECTED_CONTACTS)
        {
            actions.push(Action::CopyHint { subject: false });
        }
    }

    /// The participants' and the subject's tooltips, and the contact of a name the pointer
    /// settles on.
    pub(super) fn header_tooltips(
        &mut self,
        ui: &Ui,
        layout: &Layout,
        detail: &ThreadDetail,
        messages: Rect,
        actions: &mut Vec<Action>,
    ) {
        // Names scrolled under the header aren't there to hover.
        let named: Vec<(Rect, exo_store::Contact)> = self
            .hover
            .named
            .iter()
            .filter_map(|(rect, contact)| {
                Some((rect.intersect(messages), contact.clone()))
                    .filter(|(rect, _)| rect.is_positive())
            })
            .collect();
        let pointer = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|at| ui.ctx().layer_id_at(*at) == Some(ui.layer_id()));
        let over = named
            .iter()
            .find(|(rect, _)| pointer.is_some_and(|at| rect.contains(at)))
            .map(|(_, contact)| contact.clone());
        if let Some(contact) = self.hover.name_intent.update(ui.ctx(), over) {
            self.hover.contact = Some(person_of(contact));
        }
        let subject = header::subject_rect(
            ui,
            layout,
            detail,
            &self.action.chips,
            !self.action.spam_view,
        )
        .map(|rect| (rect, header::title(detail)));
        match self
            .hover
            .tooltips
            .show(ui, &named, subject, layout.column.x_range())
        {
            Some(tooltips::Clicked::ComposeTo(person, pop_out)) => {
                actions.push(Action::ComposeTo(person, pop_out))
            }
            Some(tooltips::Clicked::Copied(text)) => actions.push(Action::Copied(text)),
            None => {}
        }
    }

    /// Counts a click on a header that expanded (or folded) a message. The third in a row each
    /// way suggests ⇧O.
    pub(super) fn count_toggle(&mut self, expanded: bool, actions: &mut Vec<Action>) {
        let (expands, collapses) = &mut self.hover.clicks_in_a_row;
        let (counted, other) = if expanded {
            (expands, collapses)
        } else {
            (collapses, expands)
        };
        *other = 0;
        *counted = counted.saturating_add(1);
        if *counted >= 3 {
            actions.push(Action::ExpandAllHint { expand: expanded });
        }
    }

    /// The pointer over an expanded body (`body`, on screen): hover hit-tests and the cursor
    /// they pick, clicks, drag selections.
    pub(super) fn body_pointer(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        id: &str,
        body: Rect,
        pointer: Option<Pos2>,
        bodies: &mut Bodies,
    ) {
        let mut pointer_state = std::mem::take(&mut self.hover.pointer);
        let outcome = bodies.handle_pointer(
            ui,
            response,
            PointerArea {
                id,
                body,
                origin: body.min,
                pointer,
                context_menu: true,
            },
            &mut pointer_state,
            |text| self.date_on(id, text).is_some(),
        );
        self.hover.pointer = pointer_state;
        if outcome.hovered {
            self.hover.hovered_body = Some(id.to_owned());
        }
        if outcome.started_selection {
            self.hover.selection.clear();
        }
    }

    /// Where message `id`'s body was drawn this frame.
    pub(super) fn body_origin(&self, id: &str) -> Option<Pos2> {
        self.hover.body_origins.get(id).copied()
    }

    /// What a click in a body landed on.
    pub(super) fn clicked(
        &mut self,
        id: &str,
        hit: &Hit,
        bodies: &Bodies,
        actions: &mut Vec<Action>,
    ) {
        match hit {
            // `#id` scrolls to the anchor. `mailto:` opens compose (the app's), and ⌘ opens a
            // web link in the background.
            Hit::Link { href } if href.starts_with('#') => bodies.find_anchor(id, &href[1..]),
            Hit::Link { href } if self.hover.pointer.click_modifiers.command => {
                actions.push(Action::OpenUrlInBackground(href.clone()))
            }
            Hit::Link { href } => actions.push(Action::OpenUrl(href.clone())),
            Hit::Text => {}
            Hit::Date { text, .. } => {
                if let Some(date) = self.date_on(id, text) {
                    actions.push(Action::CreateEvent(date));
                }
            }
            Hit::QuoteToggle => {
                if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
                    item.show_quoted = !item.show_quoted;
                    actions.push(Action::QuoteHint {
                        shown: item.show_quoted,
                    });
                }
            }
            Hit::InlineImage { cid } => {
                // The viewer over all the message's images and messages.
                let message = self.message(id);
                let attachment = message.and_then(|message| {
                    message
                        .attachments
                        .iter()
                        .find(|attachment| attachment.content_id.as_deref() == Some(cid))
                        .cloned()
                });
                if let (Some(message), Some(attachment)) = (message, attachment) {
                    actions.push(open_file(message.attachments.iter(), attachment, true));
                }
            }
        }
    }
}
