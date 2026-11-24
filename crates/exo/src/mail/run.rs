//! Command dispatcher (`run`), list view host, folder drawer, navigation, and URL opening on
//! [`Mail`].

use egui::Rect;
use exo_commands::Hint;
use exo_store::{ListId, Mutation};

use crate::{
    Mail, accounts, achievements, bulk, calendar,
    commands::{self, Action},
    compose, desktop, filter, folders, format, inbox, mailbox, out_of_office, remind, row_menu,
    search, settings, snippets, splits,
    thread::{self, ThreadView},
    unsubscribe,
};

impl Mail {
    /// The right-click menu of the list's rows at `at`, for the conversations it acts on, and
    /// what's chosen in it. Exo's shortcuts are off while it's open, and AppKit's menu has the
    /// keyboard then anyway.
    pub(super) fn show_row_menu(&mut self, at: egui::Pos2, now: f64) {
        let threads = self.inbox.actionable();
        if threads.is_empty() || self.thread.is_some() {
            return;
        }
        let entries = row_menu::entries(&threads, self.inbox.is_exact(), &self.list);
        let items: Vec<desktop::context_menu::MenuItem> =
            entries.iter().map(|entry| entry.item.clone()).collect();
        let chosen = (self.context_menu.borrow_mut())(&items, at)
            .and_then(|index| entries.into_iter().nth(index))
            .and_then(|entry| entry.action);
        if let Some(action) = chosen {
            self.run(action, now);
        }
    }

    /// The folder drawer's lines for the account's labels.
    pub(super) fn folder_lines(&self) -> Vec<folders::Line> {
        folders::lines(
            &self.snapshot.labels,
            self.snapshot.muted,
            self.folders.all(),
        )
    }

    /// Carries out what the folder drawer asked for. The drawer closes, and a chosen list shows,
    /// with a hint at its keys when it was clicked.
    pub(super) fn folder_action(&mut self, action: folders::Action, now: f64) {
        self.folders.close();
        if action == folders::Action::SwitchAccounts {
            return self.accounts_run(accounts::Action::OpenSwitcher, now);
        }
        let folders::Action::Show(place, hint) = action else {
            return;
        };
        match hint {
            // The hint's parts have no edge spaces: each is laid out on its own.
            Some(folders::Hint::GoTo { key, name }) => {
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["G"])
                        .text("then")
                        .keys([key])
                        .text(format!("to go to {name} instantly!")),
                );
            }
            Some(folders::Hint::Label) => self.commands.hint(
                Hint::new()
                    .text("Next time, hit")
                    .keys(["⌘", "K"])
                    .text("and enter the label to go there faster!"),
            ),
            None => {}
        }
        match place {
            // The drawer's Inbox is home: Important while Important and Other are on.
            folders::Place::List(ListId::Inbox) => {
                self.run(Action::Go(splits::home(self.account.settings())), now)
            }
            folders::Place::List(list) => self.run(Action::Go(list), now),
            folders::Place::Snippets => self.go_to_snippets(),
        }
    }

    /// Carries out `action`.
    pub(crate) fn run(&mut self, action: Action, now: f64) {
        // Going anywhere closes the folder drawer, and leaves the Snippets folder and a snippet
        // open there, once `Save Snippet?` has asked.
        if matches!(action, Action::Go(_) | Action::GoToLabel) {
            if self.snippet_unsaved() {
                self.ask_leaving_snippet(Box::new(move |mail| mail.run(action, now)));
                return;
            }
            self.close_snippet();
            self.close_snippet_view();
            self.folders.close();
            self.snippets.folder.close();
        }
        match action {
            Action::ToggleFolders if self.folders.is_open() => self.folders.close(),
            Action::ToggleFolders => {
                let lines = self.folder_lines();
                let here = if self.snippets.folder.is_open() {
                    folders::Place::Snippets
                } else {
                    folders::Place::List(self.list.clone())
                };
                self.folders.open(&lines, &here);
            }
            Action::Folders(key) => {
                let lines = self.folder_lines();
                if let Some(action) = self.folders.key(key, &lines) {
                    self.folder_action(action, now);
                }
            }
            // ↑ on the first result gives the keyboard back to the search bar.
            Action::Up if self.search.is_some() && self.inbox.cursor_is_first() => {
                self.search_focus_bar(true)
            }
            Action::Up if self.inbox.cursor_is_first() => self.inbox.focus_header(),
            Action::Up => self.inbox.move_cursor(-1),
            Action::Move(delta) => self.inbox.move_cursor(delta),
            Action::Header(commands::HeaderKey::Down) => self.inbox.blur_header(),
            Action::Header(commands::HeaderKey::Move(step)) => {
                if let Some(action) = self.inbox.move_header_focus(step) {
                    self.run_header(action, now);
                }
            }
            Action::Header(key) => {
                if let Some(action) = self.inbox.header_key(key == commands::HeaderKey::Backspace) {
                    self.run_header(action, now);
                }
            }
            Action::Top => self.inbox.cursor_to_top(),
            Action::Bottom => self.inbox.cursor_to_bottom(),
            Action::Page(direction) => self.inbox.page(direction),
            Action::Open => {
                if let Some(id) = self.inbox.cursor().map(|thread| thread.id.clone()) {
                    self.open_thread(id);
                }
            }
            Action::ToggleSelected => self.inbox.toggle_selected(),
            Action::Extend(delta) => self.inbox.extend_selection(delta),
            Action::SelectAll => self.inbox.select_all(),
            Action::SelectAllFromHere => self.inbox.select_all_from_here(),
            Action::DeselectAll => self.inbox.clear_selection(),
            Action::Back { .. } if self.inbox.is_selecting() => self.inbox.clear_selection(),
            // In search, the results give the keyboard back to the bar.
            Action::Back { .. } if self.search.is_some() => self.search_focus_bar(true),
            // A filtered list loses its filter first.
            Action::Back { .. } if self.filter.is_some() => self.filter_set(None),
            // Going to a list doesn't start a back stack, so Escape in one goes home.
            Action::Back { escape: true } => {
                let home = splits::home(self.account.settings());
                if self.list != home {
                    self.run(Action::Go(home), now);
                }
            }
            Action::Back { escape: false } => self.history_back(),
            // Mark Unread from a thread goes back to the list.
            Action::Apply(Mutation::MarkUnread) if self.thread.is_some() => {
                self.triage(Mutation::MarkUnread, now);
                self.close_thread();
            }
            Action::Apply(mutation) => self.triage(mutation, now),
            Action::Pick(action) => self.pick(action, now),
            Action::Undo(token) => self.undo(token, now),
            // Going anywhere leaves search.
            Action::Go(list) if self.search.is_some() => {
                self.search_exit(now);
                self.run(Action::Go(list), now);
            }
            // From a thread, going to the list on screen just goes back to it.
            Action::Go(list) if list == self.list && self.thread.is_some() => self.close_thread(),
            Action::Go(list) if list == self.list => self.inbox.cursor_to_top(),
            Action::Go(list) => {
                self.close_thread();
                self.show(list);
            }
            Action::LeaveThread => self.close_thread(),
            Action::FromWeek(action) => {
                if self.calendar_form() != Some(true) {
                    self.run(Action::Calendar(calendar::Action::CloseWeek), now);
                    self.run(*action, now);
                }
            }
            Action::GoToLabel => self.commands.open_palette(Some("go to ")),
            Action::MuteHint => {
                let verb = if self.list == ListId::Muted {
                    "Want to unmute this conversation? Hit"
                } else {
                    "Want to mute this conversation? Hit"
                };
                self.commands
                    .hint(Hint::new().text(verb).keys(["shift", "M"]));
            }
            Action::ThreadBack => {
                if !self
                    .thread
                    .as_mut()
                    .is_some_and(ThreadView::collapse_current)
                {
                    self.close_thread();
                    self.command_nudge_left_thread();
                }
            }
            Action::NextConversation(delta) => self.next_conversation(delta),
            Action::ShowNewMessages => {
                if self
                    .thread
                    .as_mut()
                    .is_some_and(ThreadView::show_new_or_last)
                {
                    self.mark_shown_read();
                }
            }
            Action::ThreadHeader(key) => {
                let clicked = self
                    .thread
                    .as_mut()
                    .and_then(|thread| thread.header_key(key));
                match clicked {
                    Some(thread::Action::ToggleDone) => self.toggle_done(now),
                    Some(thread::Action::Remind) => {
                        self.remind(remind::Action::Open { button: true }, now)
                    }
                    Some(thread::Action::CommandPalette) => self.open_palette_button(),
                    Some(thread::Action::Unsubscribe) => {
                        self.unsubscribe_run(unsubscribe::Action::Launch)
                    }
                    _ => {}
                }
            }
            // Text selected in the thread's header copies as it is. Otherwise the bodies' selection
            // copies.
            Action::Copy => match self.thread.as_ref().and_then(ThreadView::selected_text) {
                Some(text) => self.ctx.copy_text(text),
                None => self.bodies.copy(),
            },
            // Show images: the focused draft's, else the focused message's.
            Action::ShowImages => {
                if !self.compose_show_images()
                    && let Some(id) = self.thread.as_ref().and_then(ThreadView::focused)
                {
                    self.bodies.show_images(id);
                }
            }
            Action::CopyListed => self.copy_listed(),
            Action::CopyMessage => self.copy_message(),
            Action::CopyConversation => self.copy_conversation(),
            Action::CopyRecipients => self.copy_recipients(),
            Action::CopyAttachments => self.copy_attachments(),
            Action::Preview(action) => self.viewer_run(action, now),
            Action::CopySubject => self.copy_subject(),
            Action::Print => self.print(),
            Action::SaveAsPdf => self.save_as_pdf(),
            Action::Unsubscribe(action) => self.unsubscribe_run(action),
            Action::Compose(action) => {
                // A reply's key ends the palette's nudge.
                if matches!(action, compose::Action::Reply(_)) {
                    self.dismiss_command_nudge();
                }
                self.compose_run(action, now);
            }
            Action::Snippets(action) => self.snippets_run(action, now),
            Action::Search(action) => self.search_run(action, now),
            Action::Filter(action) => self.filter_run(action, now),
            Action::Calendar(action) => self.calendar_run(action),
            Action::Bulk(action) => self.bulk_run(action),
            Action::Remind(action) => self.remind(action, now),
            Action::OutOfOffice(action) => self.out_of_office_run(action),
            Action::OpenLinks(action) => self.open_links_run(action),
            Action::NewSplit(action) => self.new_split_run(action, now),
            Action::RawSource(action) => self.raw_source_run(action),
            Action::GoogleApp(app) => {
                self.open_url(&app.url(self.account.email(), &compose::uuid_v4()))
            }
            Action::Links(action) => self.links_run(action),
            Action::Accounts(action) => self.accounts_run(action, now),
            Action::Rsvp(response) => self.rsvp_command(response),
            Action::Notice(action) => self.notice_run(action),
            Action::FirstRun(action) => self.first_run_run(action),
            Action::Achievements(achievements::Action::Show(view)) => self.show_achievements(view),
            Action::Achievements(achievements::Action::Close) => self.close_achievements(),
            Action::Backdrop(action) => self.backdrop_run(action),
            Action::Settings(action) => self.run_settings(action, now),
            Action::SplitTab(step) => {
                let tabs = splits::tabs(self.account.settings(), &self.list, &self.snapshot.counts);
                if let Some(list) = splits::step(&tabs, &self.list, step) {
                    self.folders.close();
                    self.run(Action::Go(list), now);
                }
            }
            Action::SplitTabHold(step) => {
                // In search, ⇧Tab goes back to the search bar and Tab does nothing.
                if self.search.is_some() {
                    if step < 0 {
                        self.search_focus_bar(true);
                    }
                    return;
                }
                let tabs = splits::tabs(self.account.settings(), &self.list, &self.snapshot.counts);
                // Outside the inbox views every step is to the first tab, however long Tab is held.
                let from = match &self.split_highlight {
                    Some(highlight) if splits::is_inbox_view(&self.list) => highlight.clone(),
                    _ => self.list.clone(),
                };
                self.split_highlight =
                    splits::step(&tabs, &from, step).or(self.split_highlight.take());
                self.folders.close();
                self.sync_snapshot();
            }
            Action::SplitTabRelease => {
                if let Some(list) = self.split_highlight.take() {
                    self.run(Action::Go(list), now);
                }
            }
            thread_action => self.run_thread_action(thread_action),
        }
    }

    fn run_settings(&mut self, action: settings::Action, now: f64) {
        match action {
            // The menu's `Out of Office Reply` is the palette's command.
            settings::Action::OutOfOffice => self.out_of_office_run(out_of_office::Action::Open),
            // Get Me To Zero and Bulk Actions are the app's dialogs, not the settings'.
            settings::Action::Triage(triage) => self.bulk_run(match triage {
                settings::Triage::GetMeToZero => bulk::Action::OpenGetMeToZero,
                settings::Triage::BulkActions => bulk::Action::OpenBulkActions,
            }),
            settings::Action::Achievements => self.show_achievements(achievements::View::Main),
            // The settings menu's `Sign Out` row is the palette's command.
            settings::Action::SignOut => self.sign_out(),
            settings::Action::Accounts(event) => self.account_settings_event(event),
            settings::Action::AccountPicker(key) => {
                if let Some(event) = self.settings.account_picker_key(key, &self.accounts) {
                    self.account_settings_event(event);
                }
            }
            settings::Action::CalendarSettings => {
                self.settings.hide();
                self.calendar_run(calendar::Action::OpenSettings);
            }
            // `Enable a calendar` closes the dialog and opens the week view.
            settings::Action::Notifications(settings::NotificationsEvent::EnableCalendar) => {
                self.run(Action::Settings(settings::Action::CloseDialog), now);
                if self.calendar_week().is_none() {
                    self.calendar_run(calendar::Action::OpenWeek);
                }
            }
            action => {
                let escape = action == settings::Action::Escape;
                let outcome = self.settings.run(
                    action,
                    &mut self.account,
                    &mut self.commands,
                    &self.list,
                    &self.now,
                    Action::Settings,
                );
                if let Some(id) = self.settings.take_created_hidden_split() {
                    self.hidden_split = Some(id);
                }
                if let Some(list) = outcome.show {
                    self.run(Action::Go(list), now);
                }
                if escape {
                    self.run(
                        if self.thread.is_some() {
                            Action::ThreadBack
                        } else {
                            Action::Back { escape: true }
                        },
                        now,
                    );
                }
                self.ctx.request_repaint();
            }
        }
    }

    fn run_thread_action(&mut self, action: Action) {
        let bodies = &self.bodies;
        let Some(thread) = self.thread.as_mut() else {
            return;
        };
        match action {
            Action::NextMessage(delta) => thread.next_message(delta),
            Action::Arrow(delta) => thread.arrow(delta, false),
            Action::ArrowHeld(delta) => thread.arrow(delta, true),
            Action::ThreadLeft => thread.left(false),
            Action::ThreadLeftHeld => thread.left(true),
            Action::ThreadRight => {
                thread.scroll_sideways(1);
            }
            Action::ThreadGutter(key) => thread.gutter_key(key),
            Action::ExpandMessage => thread.expand_current(),
            Action::ToggleMessage => thread.toggle_current(bodies),
            Action::ToggleAllMessages => thread.toggle_all(),
            Action::ToggleHeader => thread.toggle_header(),
            Action::PageThread(direction) => thread.page(direction),
            Action::JumpThread(to_end) => thread.jump(to_end),
            _ => {}
        }
    }

    /// Opens `url` in the system browser, if it's a web link.
    pub(crate) fn open_url(&self, url: &str) {
        if let Some(url) = format::urls::web(url) {
            self.ctx.open_url(egui::OpenUrl::new_tab(url));
        }
    }

    /// Opens `url` in the system browser without bringing it to the front, if it's a web link. This
    /// is what ⌘ does to a click on a link.
    pub(crate) fn open_url_in_background(&self, url: &str) {
        let Some(url) = format::urls::web(url) else {
            return;
        };
        if !desktop::open_in_background(&url) {
            self.ctx.open_url(egui::OpenUrl::new_tab(url));
        }
    }

    /// The thread list in the content pane, and what its clicks do.
    pub(super) fn show_list(
        &mut self,
        ui: &mut egui::Ui,
        content: Rect,
        rem: f32,
        window_width: f32,
        now: f64,
    ) {
        for action in self.inbox.show(ui, content, rem, window_width) {
            match action {
                inbox::Action::Open(id) => self.open_thread(id),
                inbox::Action::ToggleDone => self.toggle_done(now),
                inbox::Action::Remind => self.remind(remind::Action::Open { button: true }, now),
                inbox::Action::CommandPalette => self.open_palette_button(),
                inbox::Action::ShowLabel(name) => self.show_label(&name, now),
                inbox::Action::More if self.asked_more_at != Some(self.inbox.count()) => {
                    self.asked_more_at = Some(self.inbox.count());
                    self.account.more();
                }
                inbox::Action::Header(action) => self.run_header(action, now),
                inbox::Action::SelectHint { deselect } => {
                    let what = if deselect {
                        "to deselect conversations!"
                    } else {
                        "to select conversations!"
                    };
                    self.commands
                        .hint(Hint::new().text("Tip: you can hit").keys(["X"]).text(what));
                }
                inbox::Action::File { file, download } => self.open_result_file(file, !download),
                inbox::Action::FilesHint => self.commands.hint(
                    Hint::new()
                        .text("Hit")
                        .keys(["⌘", "O"])
                        .text("to open attachments instantly!")
                        .duration(2.0),
                ),
                inbox::Action::Achievements => {
                    self.show_achievements(achievements::View::InboxZero)
                }
                inbox::Action::Nudge(action) => {
                    // Either way it's done with, as of now.
                    let at = self.now.timestamp().to_string();
                    self.account.update_settings(|settings| {
                        settings.complete_nudge(inbox::HIDE_EMPTY_SPLIT, &at)
                    });
                    self.sync_snapshot();
                    // `Try it out` opens Split Inbox Settings, asking to hide every empty split.
                    if action == inbox::NudgeAction::Try {
                        self.run(
                            Action::Settings(settings::Action::Open(settings::Dialog::SplitInbox)),
                            now,
                        );
                        self.run(
                            Action::Settings(settings::Action::Split(
                                settings::SplitEvent::OfferHideAll,
                            )),
                            now,
                        );
                    }
                }
                inbox::Action::RowMenu(at) => {
                    // Next frame, once the row has the cursor on screen, since the menu waits for
                    // the user.
                    exo_ui::claim_right_click(&self.ctx);
                    self.row_menu = Some(at);
                    self.ctx.request_repaint();
                }
                inbox::Action::More => {}
            }
        }
        self.search_show(ui, content, rem, window_width, now);
    }

    /// What a click on the list's header (or Enter on what has its keyboard) does.
    fn run_header(&mut self, action: inbox::HeaderAction, now: f64) {
        match action {
            // The folders button, or the selected Inbox tab or view title.
            inbox::HeaderAction::Folders if !self.folders.is_open() => {
                self.run(Action::ToggleFolders, now)
            }
            inbox::HeaderAction::Tab(index) => self.show_tab(index, now),
            inbox::HeaderAction::AddSplit => {
                self.run(Action::Settings(settings::Action::NewSplit), now)
            }
            inbox::HeaderAction::SplitSettings => self.run(
                Action::Settings(settings::Action::Open(settings::Dialog::SplitInbox)),
                now,
            ),
            // The search icon, which shows the `/` hint as search opens.
            inbox::HeaderAction::Search => {
                if self.search.is_none() {
                    self.commands.hint(
                        Hint::new()
                            .text("Next time, hit")
                            .keys(["/"])
                            .text("to search instantly!"),
                    );
                }
                self.run(Action::Search(search::Action::Toggle), now);
            }
            inbox::HeaderAction::Filter => self.run(Action::Filter(filter::Action::Picker), now),
            inbox::HeaderAction::ClearFilter => {
                self.run(Action::Filter(filter::Action::Clear), now)
            }
            // The compose button, which leaves search and starts a new message.
            inbox::HeaderAction::Compose => {
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["C"])
                        .text("to compose instantly!"),
                );
                self.search_exit(now);
                self.compose_new(compose::Fields::default(), None);
            }
            _ => {}
        }
    }

    /// Whether there's somewhere for the rail's back area to go: out of the week view, a new
    /// message, a conversation, a snippet, search or the Snippets folder, or to the home list from
    /// another.
    pub(super) fn can_go_back(&self) -> bool {
        self.calendar_week().is_some()
            || self.compose.pane().is_some()
            || self.thread.is_some()
            || self.search.is_some()
            || self.snippets.folder.is_open()
            || self.list != splits::home(self.account.settings())
    }

    /// The rail's back area: back out of what's on screen, as its back gutter or Escape goes,
    /// without first collapsing or deselecting anything.
    pub(super) fn go_back(&mut self, now: f64) {
        if self.calendar_week().is_some() {
            self.run(Action::Calendar(calendar::Action::CloseWeek), now);
        } else if self.compose.pane().is_some() {
            self.run(Action::Compose(compose::Action::LeavePane), now);
        } else if self.thread.is_some() {
            self.close_thread();
            self.command_nudge_left_thread();
        } else if self.snippets.folder.viewing().is_some() && self.search.is_none() {
            self.snippets_run(snippets::Action::Back, now);
        } else if self.search.is_some() {
            self.search_exit(now);
        } else {
            self.run(Action::Go(splits::home(self.account.settings())), now);
        }
    }

    /// A click on split tab `index`, which shows its list. On the tab on screen, Important and
    /// Other go home and Inbox opens the folder drawer.
    fn show_tab(&mut self, index: usize, now: f64) {
        let tabs = splits::tabs(self.account.settings(), &self.list, &self.snapshot.counts);
        let Some(tab) = tabs.get(index) else { return };
        let action = match &tab.list {
            ListId::Inbox if self.list == ListId::Inbox => Action::ToggleFolders,
            list @ (ListId::Important | ListId::Other) if *list == self.list => {
                Action::Go(splits::home(self.account.settings()))
            }
            list => Action::Go(list.clone()),
        };
        self.folders.close();
        self.run(action, now);
    }

    /// A ⌘K button, which opens the palette and shows the hint at ⌘K.
    pub(super) fn open_palette_button(&mut self) {
        self.commands.hint(
            Hint::new()
                .text("Next time, hit")
                .keys(["⌘", "K"])
                .text("to instantly open Exo Command!"),
        );
        self.commands.open_palette(None);
    }

    /// A label chip, which shows that label's list.
    pub(super) fn show_label(&mut self, name: &str, now: f64) {
        let labels = &self.snapshot.labels;
        // An Auto Label's chip shows the Auto Label's name, not its Gmail label's.
        let label = labels.iter().find(|label| label.name == name).or_else(|| {
            labels
                .iter()
                .find(|label| mailbox::chip_name(&label.name).as_deref() == Some(name))
        });
        if let Some(label) = label {
            let list = ListId::Label(label.id.clone());
            self.run(Action::Go(list), now);
        }
    }
}
