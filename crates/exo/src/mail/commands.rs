//! Focus-region priority resolution and per-frame command registration on [`Mail`].

use exo_commands::Region;
use exo_store::ListId;

use super::copy_print::copy_rows;
use crate::{
    Mail, accounts, achievements, actionable, attachment_files, backdrop, bulk, calendar,
    commands::{self, Action},
    compose, filter, first_run, google_apps, links, notice, open_links, out_of_office, raw_source,
    rsvp, search, settings, snippets, splits,
    thread::ThreadView,
};

impl Mail {
    /// Resolves which focus region currently owns keyboard input, in strict top-to-bottom overlay
    /// priority order.
    pub(super) fn resolve_region(&self) -> Region {
        // The pickers' alert first, because signing another account out asks over the Accounts
        // dialog.
        if let Some(region) = self.first_run_region() {
            region
        } else if let Some(region) = self.pickers.region() {
            region
        } else if self.previewing() {
            Region::AttachmentPreview
        } else if self.touch_bar_tip.is_some() || self.notice().is_some() {
            Region::Alert
        } else if let Some(region) = self.out_of_office.region() {
            region
        } else if self.settings.dialog_open() {
            self.settings.dialog_region()
        } else if self.raw_source_open_now() {
            Region::ThreadSourceViewer
        } else if let Some(region) = self.calendar_region() {
            region
        } else if self.bulk_dialogs.is_open()
            || self.new_split_dialog_open()
            || self.achievements.dialog().is_some()
        {
            Region::Dialog
        } else if let Some(region) = self.compose.region() {
            region
        } else if self.folders.is_open() {
            Region::LabelSwitcher
        } else if let Some(thread) = &self.thread {
            thread.region()
        } else if self.snippets.folder.viewing().is_some() && self.search.is_none() {
            Region::ReadOnlySnippet
        } else if self
            .search
            .as_ref()
            .is_some_and(search::Search::bar_focused)
        {
            Region::SearchBar
        } else if self.inbox.header_focused() {
            Region::ThreadListHeader
        } else {
            Region::ThreadList
        }
    }

    /// Registers this frame's commands across settings, navigation, thread/list actions, compose,
    /// snippets, calendar, and overlays. Registration order is significant for tie-breaking.
    pub(super) fn register_commands(&mut self, ctx: &egui::Context, region: Region) {
        self.register_settings_commands(ctx);
        let in_folder =
            self.snippets.folder.is_open() && self.thread.is_none() && self.search.is_none();
        let actionable_count = self.register_mail_and_navigation_commands(region, in_folder);
        self.register_calendar_compose_and_snippet_commands(in_folder, actionable_count);
        self.register_overlay_commands(ctx, in_folder);
    }

    fn register_settings_commands(&mut self, ctx: &egui::Context) {
        let email_notifications = self
            .account
            .settings()
            .email_notifications(self.account.device_settings());
        settings::register(
            &mut self.commands,
            self.account.settings().theme,
            email_notifications,
            ctx.os(),
            Action::Settings,
        );
        settings::register_triage(
            &mut self.commands,
            self.account.settings(),
            Action::Settings,
        );
        self.settings.autocorrect = self.autocorrect();
        settings::register_typing(
            &mut self.commands,
            self.settings.autocorrect,
            self.account.settings().autocomplete(),
            Action::Settings,
        );
        self.commands
            .set_send_and_done(self.account.settings().send_and_done);
    }

    /// Registers core list, thread, RSVP, search, filter, splits, and folder commands.
    /// Returns `(actionable_len, single_non_draft)` needed by downstream compose registration.
    fn register_mail_and_navigation_commands(
        &mut self,
        region: Region,
        in_folder: bool,
    ) -> (usize, bool) {
        // A new message's pane has no conversation to act on.
        let actionable = if self.compose.pane().is_some() || in_folder {
            Vec::new()
        } else {
            actionable(self.thread.as_ref(), &self.inbox)
        };
        let actionable_len = actionable.len();
        let single_non_draft = actionable_len == 1 && !actionable[0].draft_only;
        self.commands.set_multiple_selected(actionable_len > 1);
        let copy = copy_rows(self.thread.as_ref(), &actionable);
        let (sender, own_domains) = if self.compose.pane().is_some() {
            (None, Vec::new())
        } else {
            (self.actionable_sender(), self.own_domains())
        };
        let account_settings = self.account.settings();
        commands::register(
            &mut self.commands,
            &commands::Context {
                list: &self.list,
                threads: &actionable,
                labels: &self.snapshot.labels,
                copy,
                home: splits::home(account_settings),
                other: splits::other(account_settings),
                unified: account_settings.unified_inbox,
                sender: sender.as_ref(),
                own_domains: &own_domains,
                exact: self.thread.is_some() || self.inbox.is_exact(),
            },
        );
        if let Some(marks_done) = self.rsvp_rows().filter(|_| self.compose.pane().is_none()) {
            rsvp::register(&mut self.commands, marks_done);
        }
        search::register(
            &mut self.commands,
            self.search.as_ref().filter(|_| self.thread.is_none()),
            true,
        );
        filter::register(
            &mut self.commands,
            self.thread.is_none() && self.compose.pane().is_none(),
        );
        let visible_splits = splits::tabs(account_settings, &self.list, &self.snapshot.counts)
            .iter()
            .filter(|tab| !tab.hidden && matches!(tab.list, ListId::Split(_)))
            .count();
        splits::register(&mut self.commands, account_settings, visible_splits);
        if self.folders.is_open() {
            commands::register_folders(&mut self.commands);
        }
        // A draft's images, under `Block all remote images`, until shown.
        let draft_images = self.account.settings().remote_content_blocking
            == Some(exo_store::RemoteContentBlocking::All)
            && self.compose_can_show_images();
        if let Some(thread) = &self.thread {
            commands::register_thread(
                &mut self.commands,
                &commands::ThreadContext {
                    focus_collapsed: thread.focus_is_collapsed(),
                    new_messages_focused: thread.new_messages_focused(),
                    has_selection: thread.selected_text().is_some()
                        || self.bodies.selected_text().is_some(),
                    // The messages had the keyboard before the palette took it.
                    show_images: draft_images
                        || region == Region::ThreadMessages
                            && thread
                                .focused()
                                .is_some_and(|id| self.bodies.can_show_images(id)),
                },
            );
        } else if draft_images {
            commands::register_show_images(&mut self.commands);
        }
        (actionable_len, single_non_draft)
    }

    fn register_calendar_compose_and_snippet_commands(
        &mut self,
        in_folder: bool,
        (actionable_len, single_non_draft): (usize, bool),
    ) {
        let _ = actionable_len;
        let account_settings = self.account.settings();
        let (day_open, week_open) = (
            self.calendar_day().is_some(),
            self.calendar_week().is_some(),
        );
        let details = self.calendar_details();
        let calendar_state = self.calendar_state();
        calendar::register(
            &mut self.commands,
            day_open,
            week_open,
            account_settings.calendar.show_declined_events,
            self.compose.focused().is_some(),
            details,
            calendar_state,
        );
        compose::register_shortcuts(
            &mut self.commands,
            &compose::Shortcuts {
                in_thread: self.thread.is_some(),
                focus_collapsed: self
                    .thread
                    .as_ref()
                    .is_some_and(ThreadView::focus_is_collapsed),
                can_reply: self.compose.pane().is_none() && single_non_draft,
                selecting: self.inbox.is_selecting(),
            },
        );
        let team = compose::team_domains(
            self.account.settings(),
            &self.snapshot.aliases,
            self.account.email(),
        );
        self.compose
            .register(&mut self.commands, &self.snapshot.aliases, &team);
        if self.calendar_pane_form() {
            calendar::register_focus_cycle(&mut self.commands);
        }
        if let Some(shown) = &self.pane.shown {
            compose::register_compose_to(&mut self.commands, shown);
        }
        let sender = if self.compose.pane().is_some() {
            None
        } else {
            self.actionable_sender()
        };
        if let Some(sender) = &sender {
            compose::auto_bcc::register_exclude(&mut self.commands, sender);
        }
        if self.compose.pane().is_none() && !in_folder {
            let (threads, exact) = (
                self.actionable_drafts().1,
                self.thread.is_some() || self.inbox.is_exact(),
            );
            compose::discard::register(&mut self.commands, threads, exact);
        }
        // After the list's, so the Snippets folder's keys win there.
        let focused_draft = self.compose.focused();
        snippets::register(
            &mut self.commands,
            &snippets::Context {
                draft_focused: focused_draft.is_some(),
                typing: focused_draft.is_some_and(compose::Open::takes_text),
                snippet: focused_draft.is_some_and(compose::Open::is_snippet),
                message_focused: self
                    .thread
                    .as_ref()
                    .is_some_and(|thread| thread.focused().is_some()),
                folder: in_folder,
                editing: self.compose.pane().is_some(),
                viewing: in_folder
                    && self.compose.pane().is_none()
                    && self.snippets.folder.viewing().is_some(),
                snippets: self.account.settings().snippets.len(),
            },
        );
    }

    fn register_overlay_commands(&mut self, ctx: &egui::Context, in_folder: bool) {
        let actionable = if self.compose.pane().is_some() || in_folder {
            Vec::new()
        } else {
            actionable(self.thread.as_ref(), &self.inbox)
        };
        self.pickers.register(&mut self.commands);
        accounts::register(&mut self.commands, ctx.os());
        let notice = self.notice();
        let phone_notifications = crate::cloud::status(self.account.email())
            .map(|_| self.account.settings().ntfy.is_some());
        notice::register(
            &mut self.commands,
            notice,
            self.updates.is_some(),
            phone_notifications,
        );
        first_run::register(&mut self.commands, &self.first_run);
        google_apps::register(
            &mut self.commands,
            self.thread.as_ref().and_then(google_apps::target),
        );
        links::register(
            &mut self.commands,
            self.thread.is_some(),
            actionable.len() == 1,
        );
        let action = self
            .thread
            .as_ref()
            .filter(|_| self.compose.pane().is_none())
            .and_then(|thread| thread.conversation_action(&self.bodies));
        open_links::register(&mut self.commands, &actionable, action.as_ref());
        let (source_thread, source_open) = (self.raw_source_thread(), self.raw_source_open_now());
        raw_source::register(&mut self.commands, source_thread.as_deref(), source_open);
        if self.previewing() {
            attachment_files::viewer::register(&mut self.commands);
        }
        bulk::register(&mut self.commands, &self.bulk_dialogs);
        achievements::register(&mut self.commands, self.achievements.dialog().is_some());
        backdrop::register(
            &mut self.commands,
            self.inbox.at_inbox_zero(),
            self.account.settings().reduce_motion,
        );
        self.register_new_split();
        out_of_office::register(&mut self.commands, &self.out_of_office);
        self.settings
            .register_escape(&mut self.commands, Action::Settings);
    }
}

#[cfg(test)]
mod tests {
    use exo_commands::Region;

    use crate::{Clock, Mail, account, assets_dir, commands::Action, notice, settings};

    fn test_mail(tag: &str) -> (Mail, std::path::PathBuf) {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!(
            "exo-mail-region-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints =
            gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
        let acct = account::Account::open(
            &dir,
            account::Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints,
                tokens: std::sync::Arc::new(gmail::StaticToken("dev".into())),
            },
            ctx.clone(),
        )
        .expect("account opens");
        let mail = Mail::new(&ctx, &assets_dir(), acct, Clock::system());
        (mail, dir)
    }

    #[test]
    fn resolve_region_respects_overlay_priority_order() {
        let (mut mail, dir) = test_mail("priority");
        // Dismiss any first-run state so baseline starts in the thread list.
        mail.welcome = None;
        mail.first_run = crate::first_run::State::default();
        assert_eq!(mail.resolve_region(), Region::ThreadList);

        // Header focus takes precedence over ThreadList.
        mail.inbox.focus_header();
        assert_eq!(mail.resolve_region(), Region::ThreadListHeader);

        // Open thread takes precedence over list header focus.
        mail.open_thread("t1".into());
        assert_eq!(mail.resolve_region(), Region::ThreadMessages);

        // Folder drawer (LabelSwitcher) takes precedence over an open thread.
        mail.run(Action::ToggleFolders, 0.0);
        assert_eq!(mail.resolve_region(), Region::LabelSwitcher);

        // Settings dialog takes precedence over LabelSwitcher.
        mail.run(
            Action::Settings(settings::Action::Open(settings::Dialog::Theme)),
            0.0,
        );
        assert_eq!(mail.resolve_region(), mail.settings.dialog_region());

        // Notice alert takes precedence over a settings dialog.
        mail.notice = Some(notice::Notice::VersionInfo);
        assert_eq!(mail.resolve_region(), Region::Alert);

        // First-run setup takes top priority over notices.
        mail.open_account_setup();
        assert_eq!(mail.resolve_region(), Region::AccountSetup);

        mail.discard();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
