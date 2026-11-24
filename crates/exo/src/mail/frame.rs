//! Per-frame rendering and input pipeline on [`Mail`]: prelude, content & chrome layout,
//! and overlay drawing.

use egui::{Rect, pos2};
use exo_commands::{Hint, Region, Toast};
use exo_store::ListId;
use exo_ui::motion::{self, CubicBezier};

use crate::{
    Mail, accounts, body, brands, bulk, calendar, chrome, commands::Action, folders, page_menu,
    people, quick_tips, settings, shell, status_bar, thread::ThreadView,
};

/// How high toasts sit at Inbox Zero, unless the streak animation hides the chrome.
const INBOX_ZERO_TOAST_BOTTOM: f32 = 68.0;
/// How high toasts sit over the select-all hint.
const SELECT_ALL_HINT_TOAST_BOTTOM: f32 = 72.0;
/// How high toasts sit over the status bar.
const STATUS_BAR_TOAST_BOTTOM: f32 = 64.0;

/// Window, page (below tab strip), and content-column rectangles for a single frame.
struct FrameRects {
    window: Rect,
    page: Rect,
    content: Rect,
}

impl Mail {
    /// Draws one frame and handles its input. Returns what it asks of the app, if anything.
    pub fn frame(&mut self, ui: &mut egui::Ui) -> Option<accounts::Request> {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|input| input.time);
        self.frame_prelude(&ctx, now);

        let palette = exo_ui::palette(&ctx);
        let window = ui.max_rect();
        ui.painter()
            .rect_filled(window, 0.0, palette.background_base_default);
        // At Inbox Zero the day's photo is behind everything, the tab strip too, and the right
        // pane is hidden. (The list and the rail don't fade.)
        self.backdrop.refresh(&ctx, &self.now);
        let zero = self.inbox.at_inbox_zero();
        let behind = zero.then(|| {
            let settings_open = self.settings.visible();
            self.backdrop.paint(ui.painter(), window, settings_open);
            let dim = self.paint_behind_app(&ctx, ui.painter(), window, now);
            self.backdrop.behind(window, settings_open, dim)
        });
        // The toasts' stack shows the photo through, unless the folder drawer has the keyboard.
        exo_ui::Behind::set_for_toasts(&ctx, behind.clone().filter(|_| !self.folders.is_open()));
        let popout_wheel = self.take_wheel_over_popout(&ctx);
        // A popped-out draft shrinks the root font and widens the content pane.
        let popped_out = self.compose.popped().is_some();
        let rem = exo_ui::rem(window.width(), popped_out);
        // The status bar takes the window's bottom 32px, and the app shrinks above it.
        let status_bar = self.status_bar();
        // Quick Tips shows only when no other bar does.
        let quick_tips = if status_bar.is_none() {
            self.quick_tips()
        } else {
            None
        };
        let bar_shown = status_bar.is_some() || quick_tips.is_some();
        let main = if bar_shown {
            Rect::from_min_max(
                window.min,
                pos2(window.right(), window.bottom() - exo_ui::MESSAGE_BAR_HEIGHT),
            )
        } else {
            window
        };
        let page = Rect::from_min_max(
            pos2(main.left(), main.top() + chrome::tab_bar_height(&ctx)),
            main.max,
        );
        let mut columns = shell::columns(page, rem, popped_out);
        // At Inbox Zero the list takes the right pane's room.
        if zero {
            columns.content.max.x = columns.right.max.x;
        }
        self.track_inbox_zero(&ctx, zero, now);

        self.show_rail(ui, columns.rail, zero, now);
        let events = self.poll_body_events(&ctx, now);
        self.sync_view_context();
        self.show_content_pane(ui, columns.content, rem, window.width(), &events, now);
        self.follow_cursor(&ctx);

        // Text inside keeps the unsnapped layout position.
        let pane = Rect::from_min_max(
            pos2(columns.right.left() + 1.0, columns.right.top()),
            columns.right.max,
        );
        self.show_right_pane(ui, columns.right, pane, rem, zero, now);
        self.show_footer(ui, &ctx, pane, rem, zero, now);

        if let Some(wheel) = popout_wheel {
            ctx.input_mut(|input| input.smooth_scroll_delta = wheel);
        }
        self.show_compose_popout(ui, columns.content, rem, window.width(), now);
        self.calendar_show_week(
            ui,
            Rect::from_min_max(pos2(columns.rail.right(), page.top()), page.max),
        );
        self.calendar_show_details(ui, main, page);
        self.calendar_show_form(ui, main);
        self.calendar_show_prompt(ui, page);

        let title = self.tab_title();
        let listed = self
            .accounts
            .iter()
            .find(|account| account.email == self.account.email());
        let account_color = accounts::color_name(self.account.email(), listed);
        // The welcome splash and account setup draw the strip over themselves.
        if self.welcome.is_none() && !self.account_setup_open() {
            let strip = if zero {
                chrome::Backdrop::Photo
            } else {
                chrome::Backdrop::Page
            };
            chrome::show(
                ui,
                chrome::Tab {
                    title: &title,
                    email: Some(self.account.email()),
                    login_screen: false,
                    account_color: Some(account_color),
                },
                strip,
            );
        }
        let hovered = self
            .thread
            .as_ref()
            .and_then(|thread| thread.hovered_link(&self.bodies));
        if crate::find::revealed(&ctx) {
            self.link_preview.hide(now);
        }
        self.link_preview
            .show(ui.painter(), page, hovered.as_deref(), now);

        self.show_folder_drawer(ui, page, rem, behind.as_ref(), now);
        self.show_bottom_bars(ui, window, status_bar.as_ref(), quick_tips);
        self.show_raw_source(&ctx, page);
        self.show_viewer(&ctx, page, now);

        self.update_toast_bottom(&ctx, zero, bar_shown, now);
        self.selection_hint();

        let region = self.resolve_region();
        self.commands.set_region(region);
        self.register_commands(&ctx, region);

        self.show_overlays(
            ui,
            &FrameRects {
                window,
                page,
                content: columns.content,
            },
            &title,
            account_color,
            now,
        );
        self.history_visit();
        self.request.take()
    }

    /// Runs the frame prelude: account catch-up, clock update, queued UI tasks, pending context
    /// menus, keyboard dispatch, theme/account binding, and per-frame subsystem ticks.
    fn frame_prelude(&mut self, ctx: &egui::Context, now: f64) {
        // What another tab on the account changed of its settings.
        self.account.catch_up();
        self.set_now(self.clock.now());
        self.drain_ui_tasks();
        if let Some(at) = self.row_menu.take() {
            self.show_row_menu(at, now);
        }
        // The shell's ⌘[ and ⌘], which the page sees too.
        self.history_keys(ctx);
        self.touch_bar_tip_keys(ctx);
        self.spelling_frame(ctx);
        self.show_message_menu(ctx);
        self.show_page_menu(ctx);
        // This frame's keys go to last frame's commands before anything draws, so a key a
        // command takes never reaches a widget.
        let actions = self.commands.dispatch(ctx);
        // The welcome splash takes the keys while it covers the window, and the first-run flow
        // keeps them to itself.
        if !self.welcome_covers() {
            let covered = self.first_run_covers();
            for action in actions {
                if !covered || matches!(action, Action::FirstRun(_)) {
                    self.run(action, now);
                }
            }
        }
        // ⌘E makes what's selected the text to find.
        if crate::find::wants_selection(ctx) {
            let selected = self
                .thread
                .as_ref()
                .and_then(ThreadView::selected_text)
                .or_else(|| self.bodies.selected_text().map(str::to_owned));
            crate::find::selected(ctx, selected);
        }
        // Opening the palette closes the folder drawer, and leaves the drafts without the
        // keyboard.
        if self.commands.region() == Region::Command {
            self.folders.close();
        }
        self.compose.palette_open = self.commands.region() == Region::Command;
        settings::apply_theme(
            ctx,
            self.first_run_theme()
                .unwrap_or(self.account.settings().theme),
        );
        brands::set_active_account(ctx, self.account.email());
        people::set_active_account(ctx, self.account.email());
        self.first_run_frame();
        self.sync_snapshot();
        self.out_of_office_frame(ctx);
        self.calendar_frame(ctx);
        self.compose_sync();
        self.compose_tick(now);
        self.compose_paste_events(ctx);
        self.compose_images(ctx);
        self.inbox.settle(now);
        self.report_events();
        self.attachment_files_poll(now);
    }

    /// Updates Inbox Zero streak tracking and emits the toast when a newly created hidden split
    /// first empties.
    fn track_inbox_zero(&mut self, ctx: &egui::Context, zero: bool, now: f64) {
        // A split just made hidden when empty says so when its list first empties.
        self.count_inbox_zero(ctx, zero, !self.was_zero, now);
        if zero
            && !self.was_zero
            && let Some(id) = self
                .hidden_split
                .take_if(|id| self.list == ListId::Split(id.clone()))
        {
            self.commands
                .toast(Toast::new("This Split Inbox is hidden when empty.").action(
                    "Settings",
                    None,
                    Action::Settings(settings::Action::Split(settings::SplitEvent::Edit(id))),
                ));
        }
        self.was_zero = zero;
    }

    /// Paints the left navigation rail (Mail, Calendar, and back area).
    fn show_rail(&mut self, ui: &mut egui::Ui, rail: Rect, zero: bool, now: f64) {
        // The rail's Calendar opens the week view, its Mail closes it, and the rest of it goes
        // back.
        let back = self.can_go_back();
        match shell::rail(ui, rail, zero, self.calendar_week().is_some(), back) {
            // Each switches only to the other app, with the hint at its key.
            Some(shell::RailAction::Calendar) if self.calendar_week().is_none() => {
                self.run(Action::Calendar(calendar::Action::OpenWeek), now);
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["2"])
                        .text("to go to Calendar instantly!"),
                );
            }
            Some(shell::RailAction::Mail) if self.calendar_week().is_some() => {
                self.run(Action::Calendar(calendar::Action::CloseWeek), now);
                self.commands.hint(
                    Hint::new()
                        .text("Next time, hit")
                        .keys(["1"])
                        .text("to go to Mail instantly!"),
                );
            }
            Some(shell::RailAction::Calendar | shell::RailAction::Mail) => {}
            Some(shell::RailAction::Back) => self.go_back(now),
            None => {}
        }
    }

    /// Polls rendered body events and handles clipboard copy, render notifications, and body
    /// context menu requests.
    fn poll_body_events(&mut self, ctx: &egui::Context, now: f64) -> Vec<body::Event> {
        let events = self.bodies.poll(ctx);
        for event in &events {
            match event {
                // A selection copies as plain text, with no hint.
                body::Event::Copied(text) => ctx.copy_text(text.clone()),
                body::Event::NothingSelected => self.copy_message_or_conversation(now),
                body::Event::SelectedMuch => self.commands.hint(
                    Hint::new()
                        .text("Want to copy this message? Deselect the text and just hit")
                        .keys(["⌘", "C"])
                        .text("!"),
                ),
                body::Event::Rendered { thread, facts } => {
                    self.open_links_rendered(thread, facts);
                    self.copy_listed_rendered(thread, facts);
                }
                body::Event::ContextMenu {
                    at, hit, selection, ..
                } => {
                    // Next frame, once the word it selected is on screen.
                    self.message_menu = Some(page_menu::Pending {
                        at: *at,
                        hit: hit.clone(),
                        selection: selection.clone(),
                    });
                    ctx.request_repaint();
                }
                body::Event::Clicked { .. } | body::Event::Anchored { .. } => {}
            }
        }
        events
    }

    /// Paints whichever primary surface currently occupies the content column.
    fn show_content_pane(
        &mut self,
        ui: &mut egui::Ui,
        content: Rect,
        rem: f32,
        window_width: f32,
        events: &[body::Event],
        now: f64,
    ) {
        if self.compose.pane().is_some() {
            self.show_compose_pane(ui, content, rem, window_width, now);
        } else if self.thread.is_some() {
            self.show_thread(ui, content, rem, window_width, events, now);
        } else if self.snippets.folder.viewing().is_some() && self.search.is_none() {
            self.show_snippet_view(ui, content, rem, window_width, now);
        } else if self.snippets.folder.is_open() && self.search.is_none() {
            self.show_snippets(ui, content, rem, window_width, now);
        } else {
            self.show_list(ui, content, rem, window_width, now);
        }
    }

    /// Paints the sync status line and the settings/calendar footer buttons.
    fn show_footer(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        pane: Rect,
        rem: f32,
        zero: bool,
        now: f64,
    ) {
        let bulk_actions = self.account.bulk_actions();
        self.count_cleared(&bulk_actions, now);
        self.footer_line = self.footer.line(
            self.snapshot.uploading,
            self.snapshot.connection.is_online(),
            bulk::status(&bulk_actions),
            self.account.sync_progress(),
            now,
        );
        self.footer.paint(ui, pane, rem, self.footer_line.clone());
        if bulk_actions.iter().any(|action| action.active) {
            // The count moves as Gmail answers.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // A new event's form covers the footer.
        let composing = matches!(
            self.commands.region(),
            Region::ComposeForm
                | Region::PopOutComposeForm
                | Region::RecipientList
                | Region::TextFormatterLinkInput
                | Region::TextFormatterColor
                | Region::SearchBar
        );
        // The streak animation hides the footer at once, and it comes back over 150ms (`ease`).
        let hidden = self.achievements.hides_chrome(now);
        let shown = motion::transition_in(
            ctx,
            exo_ui::id(ctx, "footer-shown"),
            !hidden,
            0.15,
            CubicBezier::EASE,
        );
        let footer = if self.calendar_pane_form() || hidden {
            None
        } else {
            let opacity = ui.opacity();
            ui.multiply_opacity(shown);
            let footer = shell::footer(
                ui,
                pane,
                rem,
                settings::badge(&self.account),
                self.settings.visible(),
                composing,
                zero,
            );
            ui.set_opacity(opacity);
            footer
        };
        match footer {
            Some(shell::FooterAction::Settings) => {
                self.run(Action::Settings(settings::Action::Toggle), now)
            }
            Some(shell::FooterAction::Calendar) => {
                self.run(Action::Calendar(calendar::Action::Footer), now)
            }
            None => {}
        }
    }

    /// Paints the folder drawer sliding out over the list.
    fn show_folder_drawer(
        &mut self,
        ui: &mut egui::Ui,
        page: Rect,
        rem: f32,
        behind: Option<&exo_ui::Behind>,
        now: f64,
    ) {
        let lines = self.folder_lines();
        let important_other = !self.account.settings().unified_inbox;
        let current = self
            .accounts
            .iter()
            .find(|account| account.email == self.account.email());
        let view = folders::View {
            email: self.account.email(),
            color: current.map_or_else(
                || accounts::default_color(self.account.email()),
                accounts::color,
            ),
            inbox: if important_other {
                self.snapshot.counts.get(&ListId::Important).copied()
            } else {
                self.snapshot.inbox
            },
            important_other,
            behind,
        };
        if let Some(action) = self.folders.show(ui, page, rem, &lines, &view) {
            self.folder_action(action, now);
        }
    }

    /// Paints the bottom status bar (offline / Out of Office) or Quick Tips bar.
    fn show_bottom_bars(
        &mut self,
        ui: &mut egui::Ui,
        window: Rect,
        status_bar: Option<&status_bar::StatusBar>,
        quick_tips: Option<quick_tips::Context>,
    ) {
        if let Some(bar) = status_bar {
            let shown = exo_ui::MessageBar {
                title: bar.title,
                description: &bar.runs,
                color: bar.color,
            };
            if let Some(url) = shown.show(ui, window) {
                self.open_url(&url);
            }
        }
        // Its cross turns Quick Tips off, saying how to get them back.
        if let Some(context) = quick_tips
            && (exo_ui::QuickTipsBar {
                parts: context.parts(),
            })
            .show(ui, window)
        {
            self.account
                .update_settings(|settings| settings.quick_tips.show = false);
            self.commands.hint(settings::quick_tips_off_hint());
        }
    }

    /// Computes and animates the vertical resting height of the toast stack.
    fn update_toast_bottom(&mut self, ctx: &egui::Context, zero: bool, bar_shown: bool, now: f64) {
        // At Inbox Zero the toast rises to 68px unless the streak animation hides the chrome,
        // moving there over 150ms (`ease`).
        let raised = zero && !self.achievements.hides_chrome(now);
        let toast_zero = motion::transition(
            ctx,
            exo_ui::id(ctx, "inbox-zero-toast"),
            raised,
            0.15,
            CubicBezier::EASE,
        );
        // Over the select-all hint, toasts sit at 72px. Hints stay where they are.
        let hint_shown = self.thread.is_none() && self.inbox.shows_select_all_hint();
        let toast_bottom = if hint_shown {
            SELECT_ALL_HINT_TOAST_BOTTOM
        } else {
            exo_ui::TOAST_BOTTOM
        };
        // Over the status bar they sit at 64px, over the select-all hint's height but not Inbox
        // Zero's.
        let layered = motion::transition(
            ctx,
            exo_ui::id(ctx, "status-bar-toast"),
            bar_shown,
            0.15,
            CubicBezier::EASE,
        );
        let toast_bottom = toast_bottom + (STATUS_BAR_TOAST_BOTTOM - toast_bottom) * layered;
        self.commands
            .set_toast_bottom(toast_bottom + (INBOX_ZERO_TOAST_BOTTOM - toast_bottom) * toast_zero);
    }

    /// Paints dialogs, alerts, command palette, toasts, first-run/welcome overlays, and handles
    /// page-level right-click capture.
    fn show_overlays(
        &mut self,
        ui: &mut egui::Ui,
        rects: &FrameRects,
        title: &str,
        account_color: &'static str,
        now: f64,
    ) {
        let ctx = ui.ctx().clone();
        if self.settings.dialog_open() {
            // For VIP's contact picker.
            self.index_address_book();
        }
        let calendars = if self.settings.dialog() == Some(settings::Dialog::Notifications) {
            self.calendar_list()
        } else {
            Vec::new()
        };
        if let Some(action) = self.settings.show_dialog(
            &ctx,
            rects.page,
            &self.account,
            &self.accounts,
            &calendars,
            self.compose.book(),
            &self.now,
        ) {
            self.run(Action::Settings(action), now);
        }
        self.show_bulk_dialog(&ctx, rects.page);
        self.show_new_split_dialog(&ctx, rects.page);
        self.show_out_of_office(ui, rects.page);
        self.compose_blur_on_press(&ctx, rects.content);
        self.show_compose_alert(ui, rects.page);
        self.show_picker_alert(ui, rects.page);
        self.show_touch_bar_tip(ui, rects.page);
        self.show_notice(ui, rects.page);
        self.open_links_frame();
        self.show_file_notifications(&ctx, rects.page, now);
        self.new_split_frame();
        self.drain_ui_tasks();
        self.command_nudge_frame(now);
        self.show_achievement_overlays(&ctx, rects.page, now);
        self.drain_spelling_learned();
        for action in self.commands.show(&ctx, rects.page) {
            self.run(action, now);
        }
        self.stack_toasts(&ctx);
        let email = self.account.email().to_owned();
        self.show_first_run(
            ui,
            rects.window,
            rects.page,
            &chrome::Tab {
                title,
                email: Some(&email),
                login_screen: false,
                account_color: Some(account_color),
            },
        );
        self.show_welcome(ui, rects.window, title, account_color);
        self.page_right_click(
            &ctx,
            Rect::from_min_max(
                pos2(
                    rects.window.left(),
                    rects.window.top() + chrome::tab_bar_height(&ctx),
                ),
                rects.window.max,
            ),
        );
    }
}
