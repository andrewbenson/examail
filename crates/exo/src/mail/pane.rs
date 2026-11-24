//! Right-pane contact tracking, cursor settle debounce, and right-pane panel rendering on [`Mail`].

use egui::Rect;
use exo_commands::Hint;

use crate::{
    Mail, SETTLED_HOVER, brands,
    commands::Action,
    compose,
    contact_pane::{self, ContactAction},
    format::names::Person,
    inbox::CursorSource,
    mailbox::ContactCard,
    search, settings, shortcuts_pane, snippets,
};

impl Mail {
    /// Keeps the contact pane on the cursor's thread: at once after a command, and once the
    /// cursor has stayed on a hovered row for [`SETTLED_HOVER`], however the pointer moves
    /// within the row.
    pub(super) fn follow_cursor(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|input| input.time);
        let cursor = self.inbox.cursor().map(|thread| thread.id.clone());
        if cursor != self.pane.cursor_since.0 {
            self.pane.cursor_since = (cursor.clone(), now);
        }
        if cursor == self.pane_thread {
            return;
        }
        let rested = now - self.pane.cursor_since.1;
        if self.inbox.cursor_source() == CursorSource::Command || rested >= SETTLED_HOVER {
            // Fetch its bodies, if they aren't stored, so opening it shows them at once.
            if let Some(id) = &cursor {
                self.account.prefetch(id);
            }
            self.pane_thread = cursor;
            // The right pane moving to another conversation hides the settings menu.
            self.settings.hide();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLED_HOVER - rested));
        }
    }

    /// Whom the contact pane shows: for a new message, its last recipient or you; a draft's
    /// selected chip or new recipient, until the focus moves; in a thread, the focused message's
    /// sender (the thread's contact when that's you), unless a Mail row opened it; in the list,
    /// the contact of the thread the cursor rests on.
    fn pane_person(&mut self) -> Option<Person> {
        // A new message: the recipient last added in the compose form, else you.
        if let Some(recipient) = self.compose.pane_recipient() {
            return Some(match recipient {
                Some(address) => Person::new(
                    address.name.clone().unwrap_or_default(),
                    address.email.clone(),
                ),
                None => {
                    let name = self
                        .snapshot
                        .aliases
                        .iter()
                        .find(|alias| alias.primary)
                        .and_then(|alias| alias.name.clone());
                    Person::new(name.unwrap_or_default(), self.account.email())
                }
            });
        }
        // A reply's or a popped-out draft's selected chip, or the recipient it just got, shows
        // until the focus moves (a thread's focused message, else the list's cursor), as a Mail
        // row's contact does. A chip without an address shows you.
        let focused = match &self.thread {
            Some(thread) => thread.focused().map(str::to_owned),
            None => self.pane_thread.clone(),
        };
        if let Some(chip) = self.compose.shown_recipient() {
            let person = if chip.email.is_empty() {
                Person::new(String::new(), self.account.email())
            } else {
                Person::new(chip.name.clone().unwrap_or_default(), chip.email.clone())
            };
            self.pane.pin = Some((person, focused.clone()));
        }
        match &mut self.pane.pin {
            Some((_, pinned)) if pinned.is_none() => *pinned = focused,
            Some((_, pinned)) if *pinned != focused => self.pane.pin = None,
            _ => {}
        }
        if let Some((person, _)) = &self.pane.pin {
            return Some(person.clone());
        }
        let listed = |id: &str| {
            self.inbox
                .threads()
                .iter()
                .find(|thread| thread.id == id)
                .and_then(|thread| thread.contact.clone())
        };
        let Some(thread) = &self.thread else {
            if let Some(person) = self.search_pane_person() {
                return person;
            }
            return listed(self.pane_thread.as_deref()?);
        };
        thread.focused_sender().or_else(|| listed(thread.id()))
    }

    /// The contact card for the pane, and whether the message on screen for that sender allows
    /// showing a cached brand logo. The account is told whenever the pane's person or conversation
    /// changes, and its next snapshot (a frame or two later) carries their conversations and DMARC
    /// verdict from local mail. Until then, the Mail section is empty rather than showing someone
    /// else's, and a list row's brand logo stays hidden until its message headers pass DMARC.
    fn pane_card(&mut self, shown: Option<&str>) -> Option<(ContactCard, bool)> {
        let person = self.pane_person()?;
        let key = person.email.to_lowercase();
        let is_me = key.eq_ignore_ascii_case(self.account.email());
        let target = (key.clone(), shown.map(str::to_owned));
        if !is_me && self.pane.contact.as_ref() != Some(&target) {
            self.account.show_contact_in(&key, shown);
            self.pane.contact = Some(target);
        }
        let recent = match &self.snapshot.contact {
            Some((email, threads)) if *email == key => threads.clone(),
            _ => Vec::new(),
        };
        let allow_brand = if is_me {
            false
        } else if let Some(open) = &self.thread {
            match open.detail() {
                Some(detail) => brands::allowed_in_thread(detail, open.focused(), &person.email)
                    .unwrap_or_else(|| {
                        !matches!(
                            &self.snapshot.contact_brand,
                            Some((email, _, Some(false))) if *email == key
                        )
                    }),
                None => false,
            }
        } else if self.compose.pane().is_some() {
            !matches!(
                &self.snapshot.contact_brand,
                Some((email, _, Some(false))) if *email == key
            )
        } else {
            matches!(
                &self.snapshot.contact_brand,
                Some((email, thread, Some(true) | None))
                    if *email == key && thread.as_deref() == shown
            )
        };
        Some((ContactCard { person, recent }, allow_brand))
    }

    /// Paints the right pane: its accent background, border, and whichever panel wins priority
    /// (settings menu, calendar day view, shortcut hints, snippets tips, contact card, or search
    /// tips).
    pub(super) fn show_right_pane(
        &mut self,
        ui: &mut egui::Ui,
        column: Rect,
        pane: Rect,
        rem: f32,
        zero: bool,
        now: f64,
    ) {
        if !zero {
            let palette = exo_ui::palette(ui.ctx());
            // The right pane: the accent background behind a 1px border, both painted from the
            // pane's snapped edge.
            let right = exo_ui::pixel_snap(column);
            ui.painter()
                .rect_filled(right, 0.0, palette.background_base_accent);
            ui.painter().vline(
                right.left() + 0.5,
                right.y_range(),
                egui::Stroke::new(1.0, palette.border_base_default),
            );
        }
        // The pane's body, first match wins: the multi-select shortcuts, the Go to…
        // panel while a `g` sequence is pending, then the contact.
        let panel = if self.inbox.is_selecting() {
            Some(shortcuts_pane::Panel::MultiSelect {
                select_all: self.inbox.is_select_all_mode(),
            })
        } else {
            self.commands
                .go_menu_visible()
                .then_some(shortcuts_pane::Panel::GoTo {
                    unified: self.account.settings().unified_inbox,
                })
        };
        match panel {
            // Then the settings menu, which at Inbox Zero sits in the footer instead.
            None if self.settings.visible() => {
                if let Some(action) =
                    self.settings
                        .show_menu(ui, pane, rem, zero, self.account.settings())
                {
                    self.run(Action::Settings(action), now);
                }
            }
            _ if zero => {}
            // The day view comes before the shortcut panels.
            _ if self.calendar_show_pane(ui, pane, rem) => {}
            Some(panel) => shortcuts_pane::show(&ui.painter_at(pane), pane, rem, panel),
            // The Snippets folder's tips, under the highlighted snippet when one is.
            None if self.snippets.folder.is_open()
                && self.thread.is_none()
                && self.search.is_none() =>
            {
                let highlighted = self
                    .snippets
                    .folder
                    .pane(ui.ctx(), now)
                    .map(str::to_owned)
                    .filter(|_| self.compose.pane().is_none());
                let snippet = highlighted.and_then(|id| {
                    self.account
                        .settings()
                        .snippets
                        .iter()
                        .find(|snippet| snippet.id == id)
                });
                snippets::show_pane(&ui.painter_at(pane), pane, rem, snippet, &self.now);
            }
            None => self.show_contact_pane(ui, pane, rem, zero, now),
        }
    }

    fn show_contact_pane(&mut self, ui: &mut egui::Ui, pane: Rect, rem: f32, zero: bool, now: f64) {
        // A new message's pane leaves out no conversation, because the draft is the current one.
        let shown = if self.compose.pane().is_some() {
            None
        } else {
            self.thread
                .as_ref()
                .map(|thread| thread.id().to_owned())
                .or_else(|| self.pane_thread.clone())
        };
        let card = self.pane_card(shown.as_deref());
        self.pane.shown = card.as_ref().map(|(card, _)| card.person.clone());
        if let Some((card, allow_brand)) = card {
            let is_me = card.person.email.eq_ignore_ascii_case(self.account.email());
            match contact_pane::show(
                ui,
                pane,
                rem,
                &card,
                contact_pane::PaneContext {
                    account: self.account.email(),
                    current: shown.as_deref(),
                    is_me,
                    allow_brand,
                    now: self.now.timestamp(),
                },
            ) {
                Some(ContactAction::OpenThread(id)) => {
                    self.open_thread(id);
                    self.pane.pin = Some((card.person, None));
                }
                Some(ContactAction::OpenDomain(domain)) => {
                    self.open_url(&format!("https://{domain}"))
                }
                // The copy button's confirmation.
                Some(ContactAction::Copied(text)) => self
                    .commands
                    .hint(Hint::new().text(format!("\"{text}\" copied to clipboard."))),
                Some(ContactAction::Compose(email)) => {
                    self.compose_new(compose::Fields::to(&email), None)
                }
                Some(ContactAction::Search(name)) => self.search_enter(Some(&name), now),
                None => {}
            }
        } else if self.search.is_some() && self.thread.is_none() {
            search::show_tips(ui.painter(), pane, rem);
        }
        if self.search.is_some() && !zero && search::tips_overflow(pane, rem) {
            settings::footer_shadow(ui, pane, rem, true);
        }
    }
}
