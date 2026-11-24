//! Open-thread lifecycle, conversation view host, and [`thread::Action`] routing on [`Mail`].

use egui::Rect;
use exo_commands::Hint;
use exo_store::{ListId, Mutation};

use crate::{
    Mail, accounts, attachment_files, body, compose, is_valid_tel, links, mailbox, remind,
    thread::{self, ThreadView},
    unsubscribe,
};

impl Mail {
    /// Opens thread `id` (the list cursor's), marking it read without a toast once its messages are
    /// in. The unread messages open expanded.
    pub(crate) fn open_thread(&mut self, id: String) {
        if self.snippet_unsaved() {
            self.ask_leaving_snippet(Box::new(move |mail| mail.open_thread(id)));
            return;
        }
        // A new message being written opens in its pane (its row is only a draft).
        let draft_only = self
            .inbox
            .threads()
            .iter()
            .any(|thread| thread.id == id && thread.draft_only);
        if draft_only && self.open_draft_thread(&id) {
            self.close_thread();
            return;
        }
        self.leave_pane();
        if self.thread.as_ref().is_some_and(|thread| thread.id() != id) {
            self.close_thread();
        }
        let unread = self
            .inbox
            .threads()
            .iter()
            .any(|thread| thread.id == id && thread.unread);
        self.search_opened_thread();
        self.close_snippet_view();
        self.account.open_thread(Some(&id));
        let context = self.view_context();
        let mut view = ThreadView::new(
            id,
            self.now.clone(),
            self.account.email().to_owned(),
            self.images.clone(),
        );
        view.sync_context(&context);
        if unread {
            view.set_read_on_open();
        }
        self.thread = Some(view);
    }

    /// Back to the list, its cursor where it was. The thread's bodies and their textures go.
    pub(crate) fn close_thread(&mut self) {
        self.pane.pin = None;
        if let Some(id) = self.thread.as_ref().map(|thread| thread.id().to_owned()) {
            self.compose_close_thread(&id);
        }
        if self.thread.take().is_some() {
            self.account.open_thread(None);
            self.bodies
                .set_thread(&[], self.account.email(), &self.account.loader());
        }
    }

    /// The next (1) or previous (-1) conversation from inside a thread:
    /// opens it, or goes back to the list past either end.
    pub(crate) fn next_conversation(&mut self, delta: isize) {
        let before = self.inbox.cursor().map(|thread| thread.id.clone());
        // Where the conversation was, if it has moved up the list since it opened.
        if let Some(anchored) = self.inbox.take_advance_anchor(delta, &[]) {
            self.inbox.point_at(&anchored);
            self.open_thread(anchored);
            return;
        }
        self.inbox.move_cursor(delta);
        match self.inbox.cursor().map(|thread| thread.id.clone()) {
            Some(id) if Some(&id) != before.as_ref() => {
                self.dismiss_command_nudge();
                self.open_thread(id);
            }
            _ => {
                self.close_thread();
                self.command_nudge_left_thread();
            }
        }
    }

    /// The open thread in the content pane, over the list, and what its clicks do.
    pub(super) fn show_thread(
        &mut self,
        ui: &mut egui::Ui,
        content: Rect,
        rem: f32,
        window_width: f32,
        events: &[body::Event],
        now: f64,
    ) {
        let ui_sender = self.ui_sender();
        let Some(thread) = self.thread.as_mut() else {
            return;
        };
        if let Some(detail) = self
            .snapshot
            .thread
            .clone()
            .filter(|detail| detail.id.as_str() == thread.id())
        {
            self.compose.set_thread_detail(thread, detail);
        }
        if thread.take_read_on_open() {
            self.account
                .apply(&[thread.id().to_owned()], Mutation::MarkRead, None);
        }
        self.bodies.set_images(
            self.account
                .settings()
                .remote_content_blocking
                .unwrap_or_default(),
        );
        if thread.detail().is_some() {
            // A message stored without its body stays a placeholder until it's in.
            let bodied: Vec<_> = thread
                .messages()
                .into_iter()
                .filter(|message| !self.snapshot.bodiless.contains(&message.id))
                .collect();
            self.bodies
                .set_thread(&bodied, self.account.email(), &self.account.loader());
        }
        for (id, attachment) in thread.invite_requests() {
            let ui = ui_sender.clone();
            self.account.loader().attachment(&attachment, move |bytes| {
                if let Ok(bytes) = bytes {
                    ui.send(move |mail| {
                        if let Some(thread) = &mut mail.thread {
                            thread.set_invite(&id, &bytes);
                        }
                    });
                }
            });
        }
        // The header's `trash` and `spam` chips show where the view shows Trash or Spam.
        let (trash, spam) = match self.search.as_ref().and_then(|search| search.query()) {
            Some(query) => {
                let query = exo_store::SearchQuery::parse(query);
                (query.shows_label("trash"), query.shows_label("spam"))
            }
            None => (self.list == ListId::Trash, self.list == ListId::Spam),
        };
        let chips = thread
            .detail()
            .map(|detail| {
                mailbox::Chips::new(&self.snapshot.labels).header_chips(
                    &detail.labels,
                    detail.muted,
                    trash,
                    spam,
                )
            })
            .unwrap_or_default();
        thread.set_chips(chips);
        let region = self.commands.region();
        let typing = self
            .compose
            .focused()
            .is_some_and(|open| open.thread.as_deref() == Some(thread.id()));
        let detail = thread.shared_detail();
        let mut inline = detail.as_deref().map(|detail| {
            self.compose.inline(
                detail,
                &self.snapshot.aliases,
                self.account.email(),
                &self.now,
                content.height(),
            )
        });
        self.attachment_files
            .update_thread(&self.account.loader(), thread);
        let actions = thread.show(
            ui,
            content,
            rem,
            window_width,
            &mut self.bodies,
            region,
            events,
            inline.as_mut(),
            typing,
        );
        for action in actions {
            self.handle_thread_action(ui, action, now);
        }
        self.open_links_hover();
    }

    fn handle_thread_action(&mut self, ui: &egui::Ui, action: thread::Action, now: f64) {
        match action {
            thread::Action::Back => {
                self.close_thread();
                self.command_nudge_left_thread();
            }
            // The previous and next conversation buttons, with their keys' hint.
            thread::Action::Navigate(delta) => {
                let (key, which) = if delta < 0 {
                    ("K", "previous")
                } else {
                    ("J", "next")
                };
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys([key])
                        .text(format!("to instantly open the {which} conversation.")),
                );
                self.next_conversation(delta);
            }
            thread::Action::ToggleDone => self.toggle_done(now),
            thread::Action::Remind => self.remind(remind::Action::Open { button: true }, now),
            thread::Action::CommandPalette => self.open_palette_button(),
            thread::Action::ShowLabel(name) => {
                self.close_thread();
                self.show_label(&name, now);
            }
            thread::Action::OpenUrl(url)
                if url
                    .get(..7)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:")) =>
            {
                self.compose_mailto(&url, ui.input(|input| input.modifiers.shift))
            }
            // Exo's own links open in a new tab, on the account they name.
            thread::Action::OpenUrl(url) if let Some(link) = links::parse(&url) => {
                self.request = Some(accounts::Request::Open(link))
            }
            thread::Action::OpenUrl(url) => self.open_url(&url),
            thread::Action::OpenUrlInBackground(url) => self.open_url_in_background(&url),
            thread::Action::ShowNewMessages => self.show_new_messages(),
            thread::Action::ExpandAllHint { expand } => {
                let then = if expand {
                    "to instantly expand all messages!"
                } else {
                    "to instantly collapse all messages!"
                };
                self.commands
                    .hint(Hint::new().text("Hit").keys(["shift", "O"]).text(then));
            }
            thread::Action::CopyHint { subject } => {
                let (lead, row) = if subject {
                    ("To copy the subject faster, hit", "→ Copy Subject!")
                } else {
                    ("To copy recipients faster, hit", "→ Copy Recipients!")
                };
                self.commands
                    .hint(Hint::new().text(lead).keys(["⌘", "K"]).text(row));
            }
            // A contact that a message shows, which the pane shows until the thread's focus moves.
            thread::Action::ShowContact(person) => self.pane.pin = Some((person, None)),
            thread::Action::ComposeTo(person, pop_out) => {
                let name = Some(person.name.clone()).filter(|name| !name.is_empty());
                let fields = compose::Fields {
                    to: vec![exo_store::Address {
                        name,
                        email: person.email.clone(),
                    }],
                    ..compose::Fields::default()
                };
                if pop_out {
                    self.compose_new_popped(fields)
                } else {
                    self.compose_new(fields, None)
                }
            }
            // The copy's confirmation, cut at 80 characters.
            thread::Action::Copied(text) => {
                ui.ctx().copy_text(text.clone());
                let shown: String = if text.chars().count() > 80 {
                    text.chars().take(80).chain(['…']).collect()
                } else {
                    text
                };
                self.commands
                    .hint(Hint::new().text(format!("\"{shown}\" copied to clipboard.")));
            }
            thread::Action::RespondToInvite(invite, response) => {
                self.calendar_respond_to_invite(invite, response, true)
            }
            thread::Action::ShowDay(date) => self.calendar_show_day(date),
            // Only strictly validated `tel:` URIs (`tel:` + optional `+` + 7..=15 ASCII
            // digits) reach the system URL handler.
            thread::Action::Dial(number) if is_valid_tel(&number) => {
                ui.ctx().open_url(egui::OpenUrl::new_tab(number))
            }
            thread::Action::Dial(_) => {}
            thread::Action::CreateEvent(date) => {
                let (title, guests) = self
                    .thread
                    .as_ref()
                    .map(ThreadView::event_prefill)
                    .unwrap_or_default();
                self.calendar_create_for_thread(title, guests, date);
            }
            // A message's Reply, Reply All and Forward buttons, with their keys' hint.
            thread::Action::Compose(compose::Action::Reply(reply)) => {
                let (key, what) = match reply {
                    exo_store::DraftAction::ReplyAll => ("enter", "to reply all instantly!"),
                    exo_store::DraftAction::Forward => ("F", "to forward instantly!"),
                    _ => ("R", "to reply instantly!"),
                };
                self.commands
                    .hint(Hint::new().text("Next time, hit").keys([key]).text(what));
                self.dismiss_command_nudge();
                self.compose_run(compose::Action::Reply(reply), now);
            }
            // A message expanded or collapsed by a click, with O's hint.
            thread::Action::QuoteHint { shown } => {
                let what = if shown {
                    "to instantly expand a message!"
                } else {
                    "to instantly collapse a message!"
                };
                self.commands
                    .hint(Hint::new().text("Next time, hit").keys(["O"]).text(what));
            }
            thread::Action::PopOutHint => self.commands.hint(
                Hint::new()
                    .text("Tip: hit")
                    .keys(["⌘", "shift", "P"])
                    .text("to pop out your draft!"),
            ),
            thread::Action::Compose(action) => self.compose_run(action, now),
            thread::Action::Unsubscribe => self.unsubscribe_run(unsubscribe::Action::Launch),
            thread::Action::HeaderHint { expanded } => {
                let then = if expanded {
                    "to instantly expand a message header!"
                } else {
                    "to instantly collapse a message header!"
                };
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["shift", "H"])
                        .text(then),
                );
            }
            thread::Action::Download { attachment, open } => {
                self.attachment_files
                    .open(&self.account.loader(), attachment, open)
            }
            thread::Action::Preview { attachments, start } => {
                self.preview_attachments(attachments, start)
            }
            thread::Action::DownloadAll { message } => self.download_all(&message),
            thread::Action::DragFile { attachment, at } => self.attachment_files.drag(
                &self.account.loader(),
                attachment_files::Part::Gmail(attachment),
                at,
                ui.ctx().zoom_factor(),
            ),
        }
    }

    /// The Show New Messages bar. The messages it held back show, and the conversation is marked
    /// read, without a toast, if they left it unread.
    fn show_new_messages(&mut self) {
        if self
            .thread
            .as_mut()
            .is_some_and(ThreadView::show_new_messages)
        {
            self.mark_shown_read();
        }
    }

    /// Marks the open conversation read, without a toast, if any of its messages is unread.
    pub(super) fn mark_shown_read(&mut self) {
        let Some(thread) = &self.thread else { return };
        if thread.detail().is_some_and(|detail| {
            detail
                .messages
                .iter()
                .any(exo_store::MessageDetail::is_unread)
        }) {
            let id = thread.id().to_owned();
            self.account
                .apply(std::slice::from_ref(&id), Mutation::MarkRead, None);
        }
    }
}
