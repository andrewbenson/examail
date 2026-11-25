//! Exo: a local, Gmail-direct desktop email client.

pub mod account;
pub mod accounts;
pub mod achievements;
mod app;
mod attachment_files;
mod avatar_cache;
mod backdrop;
mod body;
mod brands;
mod bulk;
mod calendar;
mod chrome;
pub mod clipboard;
mod clock;
pub mod cloud;
mod command_nudge;
mod commands;
mod compose;
mod contact_pane;
mod contact_picker;
mod copy;
mod deep_links;
pub mod desktop;
pub mod downloads;
mod email_actions;
mod emoji;
mod filter;
pub mod find;
mod first_run;
mod folders;
mod footer_sync;
pub mod format;
mod google_apps;
mod history;
pub mod inbox;
mod link_preview;
pub mod links;
mod lists;
pub mod log_in;
pub mod logging;
mod mail;
pub mod mail_client;
pub mod mailbox;
pub mod mcp;
mod new_split;
pub mod notice;
pub mod notifications;
mod open_links;
pub mod out_of_office;
mod page_menu;
pub mod people;
mod photo;
mod pickers;
pub mod power;
pub mod print;
pub mod profile_pictures;
pub mod quick_tips;
mod raw_source;
mod reachability;
pub mod recovery;
mod remind;
mod remote_content;
mod row_menu;
mod rsvp;
mod search;
mod senders;
mod settings;
mod shell;
mod shortcuts_pane;
pub mod sign_in;
mod signed_out;
mod snippets;
mod spelling;
mod splits;
pub mod status_bar;
mod tabs;
pub mod teardown;
pub mod thread;
pub mod time_suggest;
mod touch_bar_tip;
mod triage_copy;
mod unavailable;
mod unsubscribe;
pub mod update;
mod welcome;
pub mod zoom;

pub use app::{App, LaunchError, Screen, Start};
pub use calendar::places;
pub use clock::Clock;
pub use log_in::LogIn;
pub use signed_out::SignedOut;
pub use teardown::Unopened;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use egui::Rect;
use exo_commands::Commands;
use exo_store::ListId;
use jiff::Zoned;

use crate::{
    account::{Account, Snapshot},
    body::Bodies,
    commands::Action,
    inbox::Inbox,
    mailbox::ThreadSummary,
    thread::ThreadView,
};

/// The contact pane follows a row that the pointer moved the cursor to once the cursor has stayed
/// on it this long, a trailing debounce on the row the pointer entered.
pub(crate) const SETTLED_HOVER: f64 = 0.1;

/// A resource in `Exo.app`'s `Contents/Resources/<subpath>` beside the running executable, when
/// present on disk. Callers check `is_dir` / `is_file` and fall back to the checkout path.
pub(crate) fn bundled_resource(subpath: &str) -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .parent()?
        .join("Resources")
        .join(subpath);
    path.exists().then_some(path)
}

/// Where Exo's images are (their `images` folder): `Exo.app`'s `Contents/Resources/assets`
/// (`cargo xtask build`), else the checkout's `assets`. (Exo's fonts and icons are compiled in.)
pub fn assets_dir() -> PathBuf {
    bundled_resource("assets")
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets")))
}

/// The window's options, at its place for a first launch. [`desktop::Placement`] moves it to where
/// it was.
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Examail")
            .with_inner_size([1280.0, 800.0])
            .with_position([22.0, 22.0])
            .with_min_inner_size([780.0, 300.0])
            .with_icon(egui::IconData::default())
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            // Exo hides the native traffic lights and draws its own.
            .with_titlebar_buttons_shown(false),
        ..Default::default()
    }
}

/// The threads an action applies to: in a thread, that thread alone, as long as it's still listed
/// (the list's selection only counts in the list); in the list, the selection, or else the cursor's
/// thread.
pub(crate) fn actionable<'a>(
    thread: Option<&ThreadView>,
    inbox: &'a Inbox,
) -> Vec<&'a ThreadSummary> {
    match thread {
        Some(open) => inbox
            .threads()
            .iter()
            .filter(|row| row.id == open.id())
            .collect(),
        None => inbox.actionable(),
    }
}

/// An account's mail: the shell, the thread list and the open thread.
pub struct Mail {
    account: Account,
    inbox: Inbox,
    commands: Commands<Action>,
    /// The list on screen.
    list: ListId,
    /// Where back and forward go (⌘[, ⌘], Backspace).
    history: history::History,
    /// The Touch Bar's tip, while it's up.
    touch_bar_tip: Option<touch_bar_tip::Tip>,
    /// The welcome splash, shown the account's first time on this Mac.
    welcome: Option<welcome::Welcome>,
    /// The first-run flow after it: account setup and what ends it.
    first_run: first_run::State,
    /// `Version Info` or `Update Required`, while it's up.
    notice: Option<notice::Notice>,
    /// The updater, whose check's alerts show here.
    updates: Option<update::Updates>,
    /// The palette's first-run nudge.
    command_nudge: command_nudge::State,
    /// The account's latest snapshot.
    snapshot: Arc<Snapshot>,
    /// The snapshot the list's rows came from, or `None` since the list was switched.
    rows_from: Option<Arc<Snapshot>>,
    /// How many rows the list had when it last asked for more. It asks again once they arrive.
    asked_more_at: Option<usize>,
    /// The newest toast's undo.
    undoable: Option<mail::Undoable>,
    /// Rows that may be on their way out.
    departing: Vec<mail::Departure>,
    /// The Move and Label pickers' alerts and label creation.
    pickers: pickers::Pickers,
    /// Get Me To Zero and Bulk Actions.
    bulk_dialogs: bulk::Dialogs,
    /// Unsubscribe's picker and what it's waiting for.
    unsubscribing: unsubscribe::State,
    /// Out of Office Reply's dialog, alerts and polls.
    out_of_office: out_of_office::OutOfOffice,
    /// Open Links & Attachments' list.
    open_links: open_links::OpenLinks,
    /// ⌘C in the list, while the conversation is read and rendered.
    copy_listed: copy::Listed,
    /// Making splits from the palette, and the New Split dialog.
    new_split: new_split::NewSplit,
    /// The raw source viewer.
    raw_source: raw_source::RawSource,
    /// The thread whose contact the right pane shows.
    pane_thread: Option<String>,
    /// Right-pane contact pinning, fetch tracking, and cursor hover-settle state.
    pane: mail::PaneState,
    /// The folder drawer (`LabelSwitcher`).
    folders: folders::Folders,
    /// A row's right-click menu to show next frame, at this point.
    row_menu: Option<egui::Pos2>,
    /// The tab Tab or ⇧Tab has reached while held, whose list shows when it's let go.
    split_highlight: Option<ListId>,
    /// A split just made that's hidden when empty, until its list first empties.
    hidden_split: Option<String>,
    /// Whether the list was at Inbox Zero last frame.
    was_zero: bool,
    /// The day's photo behind Inbox Zero.
    backdrop: backdrop::Backdrop,
    /// The achievements' popups, streak animation and dialog.
    achievements: achievements::State,
    /// Shows a context menu and returns the item chosen ([`desktop::context_menu::show`]). The
    /// account's other pages made from this one share it.
    context_menu: std::rc::Rc<std::cell::RefCell<Box<desktop::context_menu::Show>>>,
    /// A right-click in a message body, whose menu shows next frame.
    message_menu: Option<page_menu::Pending>,
    /// A right-click on the rest of the page, whose menu shows next frame.
    page_menu: Option<page_menu::PagePending>,
    /// Makes a draft body's spellchecker (macOS's, [`desktop::spelling::engine`]).
    spelling: std::rc::Rc<dyn Fn() -> Option<Box<dyn exo_editor::Spelling>>>,
    /// Bumped when [`Mail::set_spelling`] replaces `spelling`, so cached field engines reset.
    spelling_generation: u64,
    /// Owns this page's shared spellcheck and editable-menu slot.
    spelling_handle: spelling::SpellingHandle,
    /// Whether that spellchecker corrects spelling automatically, autocorrect's default
    /// ([`desktop::spelling::corrects_automatically`]).
    corrects_automatically: fn() -> bool,
    /// The message ⌘C last copied, and when. ⌘C on it again copies the conversation.
    copied: Option<(String, f64)>,
    /// The settings menu and dialogs.
    settings: settings::Settings,
    /// The open thread, drawn over the list.
    thread: Option<ThreadView>,
    /// Message bodies, rendered off the UI thread.
    bodies: Bodies,
    /// Completions from background tasks, drained on the UI thread.
    ui_tasks: account::UiTasks<Self>,
    /// Where the invite photos are.
    images: PathBuf,
    link_preview: link_preview::LinkPreview,
    /// Clicked attachments being fetched and previewed.
    attachment_files: attachment_files::AttachmentFiles,
    /// Drafts being written and sends being held.
    compose: compose::Compose,
    /// The Snippets folder, and the ⌘; picker's origin.
    snippets: snippets::Snippets,
    /// The HTML of the message ⌘C last copied, and its fields. Pasting it outside a text field
    /// makes a draft of it.
    copied_message: Option<(String, compose::Fields)>,
    /// Search mode, over the list on screen.
    search: Option<search::Search>,
    /// The search results the list's rows came from.
    search_rows_from: Option<Arc<account::SearchResults>>,
    /// The list's cursor when search opened, which it gets back after.
    search_cursor: Option<String>,
    /// The calendar's day view.
    calendar: calendar::Calendar,
    /// The filter narrowing the list.
    filter: Option<exo_store::Filter>,
    /// Each list's last filter, which it opens with again.
    last_filters: std::collections::HashMap<ListId, exo_store::Filter>,
    /// The device's accounts, in the user's order ([`Mail::set_accounts`]).
    accounts: Vec<exo_auth::Account>,
    /// Whether signing out is waiting for the account to count what Gmail doesn't have yet.
    signing_out: bool,
    /// What this frame asks of the app, returned by [`Mail::frame`].
    request: Option<accounts::Request>,
    /// The footer's sync line.
    footer: footer_sync::FooterSync,
    /// What the sync line says this frame.
    footer_line: Option<String>,
    clock: Clock,
    /// The clock's time this frame.
    now: Zoned,
    ctx: egui::Context,
}

/// Shared per-frame environment read by [`Inbox`] and [`ThreadView`].
#[derive(Clone, Debug)]
pub(crate) struct ViewContext {
    /// Clock time used for relative dates and date separators.
    pub now: Zoned,
    /// Whether Gmail can be reached.
    pub online: bool,
    /// Whether headers name senders in full (`showSenderFullNames`).
    pub full_names: bool,
    /// When the account first signed in on this Mac, if known.
    pub joined: Option<jiff::Timestamp>,
    /// Domains belonging to the user, excluded from mailing-list unsubscribe offers.
    pub own_domains: Vec<String>,
    /// Other connected calendar accounts, and those currently signed out.
    pub calendar_accounts: Vec<String>,
    pub signed_out: Vec<String>,
    /// Bulk-action status labels waiting or running per thread ID (`Pending: Mark Done`).
    pub pending: std::collections::HashMap<String, String>,
    /// Whether the folder drawer is open, taking keyboard focus off the list.
    pub folders_open: bool,
    /// Whether a draft is popped out, whether it holds keyboard focus, and its card bounds.
    pub popped_out: bool,
    pub popout_focused: bool,
    pub covered: Option<Rect>,
    /// Whether the active list or search includes Trash or is Spam.
    pub trash_shown: bool,
    pub spam_view: bool,
}

impl ViewContext {
    pub(crate) fn new(now: Zoned) -> Self {
        Self {
            now,
            online: true,
            full_names: false,
            joined: None,
            own_domains: Vec::new(),
            calendar_accounts: Vec::new(),
            signed_out: Vec::new(),
            pending: std::collections::HashMap::new(),
            folders_open: false,
            popped_out: false,
            popout_focused: false,
            covered: None,
            trash_shown: false,
            spam_view: false,
        }
    }
}

impl Drop for Mail {
    fn drop(&mut self) {
        // Before the account goes, which sends what it holds.
        self.save_drafts();
    }
}

impl Mail {
    /// `account`'s inbox in `ctx` (set up by [`App::new`]) with the images in `assets`, dated
    /// relative to `clock`.
    pub fn new(ctx: &egui::Context, assets: &Path, account: Account, clock: Clock) -> Self {
        let now = clock.now();
        account.set_clock(clock.clone());
        // The account's theme from the start, and for what may replace its mail before a frame.
        settings::apply_theme(ctx, account.settings().theme);
        let snapshot = account.snapshot();
        // The app opens on home: Important while Important and Other are on.
        let home = splits::home(account.settings());
        if home != ListId::Inbox {
            account.show(home.clone());
        }
        let backdrop = backdrop::Backdrop::new(ctx, &now);
        let ui_tasks = account::UiTasks::default();
        let attachment_files =
            attachment_files::AttachmentFiles::new(ctx, account.email(), ui_tasks.sender(ctx));
        let mut mail = Self {
            bodies: Bodies::new(ctx),
            thread: None,
            link_preview: link_preview::LinkPreview::default(),
            attachment_files,
            compose: compose::Compose::default(),
            snippets: snippets::Snippets::default(),
            copied_message: None,
            search: None,
            search_rows_from: None,
            search_cursor: None,
            calendar: calendar::Calendar::default(),
            filter: None,
            last_filters: std::collections::HashMap::new(),
            ui_tasks,
            images: assets.join("images"),
            now: now.clone(),
            ctx: ctx.clone(),
            account,
            inbox: Inbox::new(now),
            commands: Commands::new(),
            list: home,
            history: history::History::default(),
            touch_bar_tip: None,
            welcome: None,
            first_run: first_run::State::default(),
            notice: None,
            updates: None,
            command_nudge: command_nudge::State::default(),
            snapshot,
            rows_from: None,
            asked_more_at: None,
            undoable: None,
            departing: Vec::new(),
            pickers: pickers::Pickers::default(),
            bulk_dialogs: bulk::Dialogs::default(),
            unsubscribing: unsubscribe::State::default(),
            out_of_office: out_of_office::OutOfOffice::default(),
            open_links: open_links::OpenLinks::default(),
            copy_listed: copy::Listed::default(),
            new_split: new_split::NewSplit::default(),
            raw_source: raw_source::RawSource::default(),
            pane_thread: None,
            pane: mail::PaneState::default(),
            folders: folders::Folders::default(),
            row_menu: None,
            split_highlight: None,
            hidden_split: None,
            was_zero: false,
            backdrop,
            achievements: achievements::State::default(),
            context_menu: std::rc::Rc::new(std::cell::RefCell::new(Box::new(
                desktop::context_menu::show,
            ))),
            message_menu: None,
            page_menu: None,
            spelling: std::rc::Rc::new(desktop::spelling::engine),
            spelling_generation: 0,
            spelling_handle: spelling::SpellingHandle::new(),
            corrects_automatically: desktop::spelling::corrects_automatically,
            copied: None,
            settings: settings::Settings::new(ctx),
            accounts: Vec::new(),
            signing_out: false,
            request: None,
            footer: footer_sync::FooterSync::default(),
            footer_line: None,
            clock,
        };
        mail.set_split_library(splits::load_library());
        mail.resume_welcome();
        if mail.welcome.is_none() {
            mail.maybe_start_first_run();
        }
        // Until the app says otherwise, the device has this one account.
        mail.accounts = vec![exo_auth::Account {
            email: mail.account.email().to_owned(),
            ..exo_auth::Account::default()
        }];
        mail
    }

    /// Another page on the account, for another tab. It has a view of the account of its own, opens
    /// at home, and is set up as this one is (where previews go, the split library, the
    /// spellchecker and context menus, which tests give).
    pub(crate) fn another_page(&self, assets: &Path) -> Box<Self> {
        let mut page = Box::new(Self::new(
            &self.ctx,
            assets,
            self.account.view(),
            self.clock.clone(),
        ));
        page.attachment_files
            .set_cache(self.attachment_files.cache().to_owned());
        page.settings
            .set_split_library(self.settings.split_library().cloned());
        page.spelling = self.spelling.clone();
        page.spelling_generation = self.spelling_generation;
        page.corrects_automatically = self.corrects_automatically;
        page.context_menu = self.context_menu.clone();
        page.accounts = self.accounts.clone();
        page
    }
}

/// Whether `number` is a strict `tel:` URI (`tel:` followed by an optional `+` and 7..=15 ASCII
/// digits, nothing else).
pub(crate) fn is_valid_tel(number: &str) -> bool {
    let Some(rest) = number.strip_prefix("tel:") else {
        return false;
    };
    let digits = rest.strip_prefix('+').unwrap_or(rest);
    (7..=15).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// Search query for `subject` that keeps embedded double quotes from closing `subject:"…"` early
/// (`exo_store::SearchQuery` strips `"` inside tokens rather than reading `\"` escapes).
pub(crate) fn subject_query(subject: &str) -> String {
    if !subject.contains('"') {
        return format!("subject:\"{subject}\"");
    }
    let parts: Vec<String> = subject
        .split('"')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| format!("subject:\"{part}\""))
        .collect();
    if parts.is_empty() {
        "subject:\"\"".to_owned()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::{assets_dir, bundled_resource, is_valid_tel, subject_query};

    #[test]
    fn subject_query_keeps_quotes_inside_subject_clauses() {
        assert_eq!(
            subject_query("Quarterly review"),
            "subject:\"Quarterly review\""
        );
        assert_eq!(
            subject_query("15\" MacBook"),
            "subject:\"15\" subject:\"MacBook\""
        );
        assert_eq!(
            subject_query("Project \"Orion\" OR in:trash"),
            "subject:\"Project\" subject:\"Orion\" subject:\"OR in:trash\""
        );
        let parsed = exo_store::SearchQuery::parse(&subject_query("Project \"Orion\" OR in:trash"));
        assert!(!parsed.shows_label("trash"));
        assert_eq!(subject_query("\"\""), "subject:\"\"");
    }

    #[test]
    fn dial_validator_accepts_only_strict_tel_uris() {
        for valid in [
            "tel:5550102",
            "tel:+15550102030",
            "tel:123456789012345",
            "tel:+123456789012345",
        ] {
            assert!(is_valid_tel(valid), "expected valid: {valid}");
        }
        for invalid in [
            "",
            "tel:",
            "tel:+",
            "tel:123456",
            "tel:+123456",
            "tel:1234567890123456",
            "tel:+1234567890123456",
            "TEL:+15550102030",
            "tel:++15550102030",
            "tel:+1-555-010-2030",
            "tel:+1 555 010 2030",
            "tel:+15550102030;ext=123",
            "tel:+15550102030#1",
            "tel:+15550102030\n",
            " tel:+15550102030",
            "tel:+15550102030 ",
            "https://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert!(!is_valid_tel(invalid), "expected invalid: {invalid:?}");
        }
    }

    #[test]
    fn bundled_resource_falls_back_to_checkout_assets() {
        assert!(bundled_resource("nonexistent-resource-check").is_none());
        assert!(assets_dir().join("images").is_dir());
    }
}
