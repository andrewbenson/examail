//! Configuration setters, per-frame [`ViewContext`] assembly, and app/test query accessors on
//! [`Mail`].

use std::path::{Path, PathBuf};

use exo_store::ListId;
use jiff::Zoned;

use crate::{
    Mail, ViewContext,
    account::{self, Account},
    achievements, backdrop, desktop,
    inbox::Inbox,
    splits, status_bar,
    thread::ThreadView,
    welcome,
};

impl Mail {
    /// Keeps attachment previews in `folder` rather than the temp directory's cache, which every
    /// copy of Exo running shares. Tests give each app its own.
    pub fn set_preview_cache(&mut self, folder: PathBuf) {
        self.attachment_files.set_cache(folder);
    }

    /// Uses `library` as the split library, as this account sees it. Tests and
    /// screenshots give a made-up one in place of the copy in the assets.
    pub fn set_split_library(&mut self, library: Option<exo_store::SplitLibrary>) {
        let library = library.map(|library| {
            library.for_account(
                &splits::team_domain(self.account.email())
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        });
        self.settings.set_split_library(library);
    }

    /// Checks drafts' spelling with what `make` makes, one for each body, instead of macOS's
    /// spellchecker. Tests give a word list, and never change the user's dictionary. Autocorrect is
    /// off unless turned on in Exo, rather than following macOS.
    pub fn set_spelling(&mut self, make: impl Fn() -> Box<dyn exo_editor::Spelling> + 'static) {
        self.spelling = std::rc::Rc::new(move || Some(make()));
        self.spelling_generation += 1;
        self.corrects_automatically = || false;
    }

    /// Whether autocorrect is on for this account, as turned on or off in Exo, or else as macOS
    /// corrects spelling.
    pub fn autocorrect(&self) -> bool {
        self.account
            .settings()
            .autocorrect((self.corrects_automatically)())
    }

    /// Turns autocorrect on or off, as Edit ▸ `Enable Autocorrect` does for every account.
    pub fn set_autocorrect(&mut self, on: bool) {
        self.account
            .update_settings(|settings| settings.typing_assistance.enabled = Some(on));
    }

    /// Shows context menus with `show` instead of AppKit's, so tests can choose their items.
    pub fn set_context_menu(
        &mut self,
        show: impl FnMut(&[desktop::context_menu::MenuItem], egui::Pos2) -> Option<usize> + 'static,
    ) {
        self.context_menu = std::rc::Rc::new(std::cell::RefCell::new(Box::new(show)));
    }

    /// Shows `Warn Before Quitting` in the settings menu for `warning`, or no such row with `None`.
    pub fn set_quit_warning(&mut self, warning: Option<desktop::QuitWarning>) {
        self.settings.set_quit_warning(warning);
    }

    /// Takes the clock's time for this frame, which everything dated relative to now reads.
    pub(super) fn set_now(&mut self, now: Zoned) {
        if now.timestamp().as_second() / 60 != self.now.timestamp().as_second() / 60 {
            self.account.check_reminders();
        }
        self.now = now;
        self.sync_view_context();
    }

    /// Assembles the current per-frame view environment shared by the list and thread views.
    pub(super) fn view_context(&self) -> ViewContext {
        let spam_view = match self.search.as_ref().and_then(|search| search.query()) {
            Some(query) => exo_store::SearchQuery::parse(query).shows_label("spam"),
            None => self.list == ListId::Spam,
        };
        let email = self.account.email();
        let settings = self.account.settings();
        let calendar_accounts = settings
            .calendar_accounts
            .keys()
            .filter(|address| *address != email)
            .cloned()
            .collect();
        let signed_out = settings
            .calendar_accounts
            .iter()
            .filter(|(_, account)| !account.authed)
            .map(|(address, _)| address.clone())
            .collect();
        let popped = self.compose.popped();
        ViewContext {
            now: self.now.clone(),
            online: self.snapshot.connection.is_online(),
            full_names: settings.show_sender_full_names,
            joined: self.joined(),
            own_domains: self.own_domains(),
            calendar_accounts,
            signed_out,
            pending: self.pending_rows(&self.account.bulk_actions()),
            folders_open: self.folders.is_open(),
            popped_out: popped.is_some(),
            popout_focused: popped
                .is_some_and(|popped| self.compose.focused().is_some_and(|open| open.id == popped)),
            covered: self.popout_card(),
            trash_shown: self.list == ListId::Trash || self.search.is_some(),
            spam_view,
        }
    }

    /// Propagates the shared [`ViewContext`] to the thread list and any open conversation.
    pub(super) fn sync_view_context(&mut self) {
        let context = self.view_context();
        self.inbox.sync_context(&context);
        if let Some(thread) = &mut self.thread {
            thread.sync_context(&context);
        }
    }

    /// The account on screen.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Stops the account without sending what it holds, for signing out
    /// ([`Account::discard`]).
    pub(crate) fn discard(&mut self) {
        self.account.discard();
    }

    /// Where attachment previews are kept.
    pub(crate) fn preview_cache(&self) -> &Path {
        self.attachment_files.cache()
    }

    /// The account was switched away from, and stays open off screen as it was left. The last
    /// keystrokes of its open drafts are saved and go to Gmail now, not at their pace, and it asks
    /// for no more frames.
    pub(crate) fn leave_screen(&mut self) {
        self.save_drafts();
        self.account.write_drafts_now();
        self.account.set_on_screen(false);
    }

    /// The list on screen.
    pub fn list(&self) -> &ListId {
        &self.list
    }

    /// The thread list.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// The open thread, if any.
    pub fn thread(&self) -> Option<&ThreadView> {
        self.thread.as_ref()
    }

    /// The status bar at the bottom of the window, if one shows: the offline bar, else Out of
    /// Office Reply's, else Examail Cloud's.
    pub fn status_bar(&self) -> Option<status_bar::StatusBar> {
        status_bar::StatusBar::offline(&self.snapshot.connection, self.calendar_week().is_some())
            .or_else(|| {
                self.out_of_office()
                    .then(status_bar::StatusBar::out_of_office)
            })
            .or_else(|| {
                crate::cloud::status(self.account.email())
                    .and_then(|status| status_bar::StatusBar::cloud(&status))
            })
    }

    /// What the footer's sync line says, as last drawn.
    pub fn footer_line(&self) -> Option<&str> {
        self.footer_line.as_deref()
    }

    /// What the Dock badge shows: the list count, from the account's latest counts, which come
    /// in while the window is hidden too.
    pub fn badge(&self) -> Option<String> {
        self.list_count(&self.account.snapshot().counts)
    }

    /// The count the tab title and the Dock badge show: the number of conversations the store has
    /// in the list on screen, from `counts`, which counts the inbox views (the inbox and its
    /// splits). `None` in the Snippets folder and the week view, whose badge is the calendar's.
    pub(crate) fn list_count(&self, counts: &splits::Counts) -> Option<String> {
        let snippets = self.snippets.folder.is_open() && self.search.is_none();
        if snippets || self.calendar_week().is_some() {
            return None;
        }
        splits::count_label(counts.get(&self.list)?)
    }

    /// The draft with the keyboard, as its To addresses and subject (for tests).
    pub fn compose_focus(&self) -> Option<(Vec<String>, String)> {
        self.compose.focus_summary()
    }

    /// The body of the draft with the keyboard, as its editor writes it (for tests).
    pub fn compose_body(&self) -> Option<String> {
        self.compose.focused().map(|open| open.editor.html())
    }

    /// Where the caret of the body with the keyboard was last drawn (for tests).
    pub fn compose_caret(&self) -> Option<egui::Rect> {
        self.compose.focused().map(|open| open.caret)
    }

    /// How many drafts are open in the open thread (for tests).
    pub fn thread_drafts(&self) -> usize {
        self.thread
            .as_ref()
            .map_or(0, |thread| self.compose.open_in(thread.id()))
    }

    /// The Send later time of the draft with the keyboard (for tests).
    pub fn compose_send_later(&self) -> Option<jiff::Timestamp> {
        self.compose
            .focused()
            .and_then(|open| open.record.send_later.as_ref())
            .map(|later| later.at)
    }

    /// The account, to change (for tests).
    pub fn account_mut(&mut self) -> &mut Account {
        &mut self.account
    }

    /// The reminder of the draft with the keyboard (for tests).
    pub fn compose_reminder(&self) -> Option<exo_store::DraftReminder> {
        self.compose.focused().and_then(|open| open.record.reminder)
    }

    /// The hint tooltip showing, its words and keycaps one space apart. For tests.
    pub fn hint_text(&self) -> Option<String> {
        self.commands.hint_text()
    }

    /// Who called off the scheduled sends of the drafts open, by a reply (for tests).
    pub fn compose_unscheduled_by(&self) -> Vec<String> {
        self.compose.unscheduled_by()
    }

    /// The open sub-palette's prompt and rows, if one is open. For tests.
    pub fn sub_palette_rows(&self) -> Option<(String, Vec<String>)> {
        self.commands.sub_palette_rows()
    }

    /// The toast showing, if any. For tests.
    pub fn toast_text(&self) -> Option<&str> {
        self.commands.toast_text()
    }

    /// Whether the new-message pane is showing.
    pub fn compose_pane_open(&self) -> bool {
        self.compose.pane().is_some()
    }

    /// Whether the folder drawer is open.
    pub fn folders_open(&self) -> bool {
        self.folders.is_open()
    }

    /// Whether the Snippets folder is the list (for tests).
    pub fn snippets_folder_open(&self) -> bool {
        self.snippets.folder.is_open()
    }

    /// The snippet open read-only, if any (for tests).
    pub fn snippet_viewed(&self) -> Option<&str> {
        self.snippets.folder.viewing()
    }

    /// The Snippets folder's highlighted snippet, if any (for tests).
    pub fn snippet_cursor(&self) -> Option<&str> {
        self.snippets.folder.cursor()
    }

    /// Whether a draft is popped out.
    pub fn compose_popped_out(&self) -> bool {
        self.compose.popped().is_some()
    }

    /// The address of whom the contact pane showed last frame.
    pub fn contact_pane_email(&self) -> Option<&str> {
        self.pane.shown.as_ref().map(|person| person.email.as_str())
    }

    /// Whether the settings menu is in the right pane.
    pub fn settings_visible(&self) -> bool {
        self.settings.visible()
    }

    /// Whether a settings dialog is open.
    pub fn settings_dialog_open(&self) -> bool {
        self.settings.dialog_open()
    }

    /// Whether nothing is in flight: the account is idle and every body on screen is laid out and
    /// painted. For screenshots and tests.
    pub fn is_idle(&self) -> bool {
        self.account.is_idle()
            && self.ui_tasks.is_idle()
            && self.bodies.is_idle()
            && self.snippets.is_idle()
            && self.backdrop.is_loaded()
            && self.welcome.as_ref().is_none_or(welcome::Welcome::is_idle)
            && self.first_run_idle()
    }

    pub(crate) fn ui_sender(&self) -> account::UiSender<Self> {
        self.ui_tasks.sender(&self.ctx)
    }

    pub(crate) fn drain_ui_tasks(&mut self) {
        while let Some(task) = self.ui_tasks.pop() {
            task.run(self);
        }
    }

    /// The achievement popups' titles, oldest first. For tests.
    pub fn achievement_popups(&self) -> Vec<String> {
        self.achievements.popups()
    }

    /// The Achievements dialog's view, while it's open. For tests.
    pub fn achievements_dialog(&self) -> Option<achievements::View> {
        self.achievements.dialog()
    }

    /// Shows the photo `file` at Inbox Zero, whatever the day, for screenshots. False if there's
    /// no such photo.
    pub fn use_photo(&mut self, file: &str) -> bool {
        backdrop::Backdrop::named(&self.ctx, file, &self.now)
            .map(|backdrop| self.backdrop = backdrop)
            .is_some()
    }
}
