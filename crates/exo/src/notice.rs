//! Alerts that only tell you something, with one button: ⌘K's `Version Info`, which ⌘C copies,
//! `Update Required` for a deep link Exo doesn't know, and the alerts of ⌘K's `Check for Updates`
//! (also the `upgrade` deep link's), which say how the updater's check goes
//! ([`crate::update`]). Enter, ⌘Enter, Space and Escape close them, and nothing else gets
//! through while one is up. Also ⌘K's `Open Source Attribution`, which opens Exo's notices.

use egui::{Color32, Rect, pos2};
use exo_commands::{Command, Commands, Hint, Region};
use exo_ui::{ALERT_TEXT, AlertRun};

use crate::commands;
use crate::update::{self, Mode};

/// `- Version: …`: Exo's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The copy hint under the text: `rgba(0, 0, 0, .5)` in both themes, on the action row's line.
const SUBTEXT_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 128);
const SUBTEXT_BELOW_TEXT: f32 = 16.0;
/// The text of `Update Required`, with its `<strong>`s marked.
const UPDATE_REQUIRED: &[AlertRun<'static>] = &[
    AlertRun::Text("To see this feature, hit "),
    AlertRun::Strong("Cmd+K"),
    AlertRun::Text(" (Mac) or "),
    AlertRun::Strong("Ctrl+K"),
    AlertRun::Text(" (Windows) → "),
    AlertRun::Strong("Check for Updates"),
    AlertRun::Text(" to get on the latest version of Examail."),
];
/// Where an alert's keys work.
const ALERT: &[Region] = &[Region::Alert];

/// Registers the standard shortcuts for a selectable [`Region::Alert`]: `Enter`, `⌘Enter` and
/// `Space` confirm the selected button, `Escape` runs `escape`, `→` and `Tab` step right (`-1`),
/// and `←` and `⇧Tab` step left (`+1`).
pub(crate) fn register_alert_keys<A: Clone + 'static>(
    commands: &mut Commands<A>,
    confirm: A,
    escape: A,
    select: impl Fn(isize) -> A,
) {
    commands.add(
        Command::shortcut("ALERT_CONFIRM", confirm)
            .keys(["enter", "mod+enter", "space"])
            .regions(ALERT),
    );
    commands.add(
        Command::shortcut("ALERT_ESCAPE", escape)
            .key("esc")
            .regions(ALERT),
    );
    commands.add(
        Command::shortcut("ALERT_NEXT", select(-1))
            .keys(["right", "tab"])
            .regions(ALERT),
    );
    commands.add(
        Command::shortcut("ALERT_PREVIOUS", select(1))
            .keys(["left", "shift+tab"])
            .regions(ALERT),
    );
}

/// Registers `Enter`, `⌘Enter`, `Space` and `Escape` for a single-action [`Region::Alert`].
pub(crate) fn register_alert_dismiss<A: Clone + 'static>(commands: &mut Commands<A>, dismiss: A) {
    commands.add(
        Command::shortcut("ALERT_OK", dismiss)
            .keys(["enter", "mod+enter", "space", "esc"])
            .regions(ALERT),
    );
}

/// An alert on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// ⌘K's `Version Info`.
    VersionInfo,
    /// A deep link to something Exo doesn't have.
    UpdateRequired,
    /// How ⌘K's `Check for Updates` goes.
    Update(update::Alert),
}

/// Something about these alerts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// ⌘K's `Version Info`.
    VersionInfo,
    /// ⌘K's `Open Source Attribution`, which opens Exo's third-party notices.
    OpenSourceAttribution,
    /// ⌘K's `Check for Updates`, and the `upgrade` deep link.
    CheckForUpdates,
    /// `OK`, Enter, Space or Escape.
    Dismiss,
    /// ⌘C on `Version Info`.
    Copy,
    /// ⌘K's `Turn On Phone Notifications`: Examail Cloud notifies a new private ntfy topic.
    PhoneNotificationsOn,
    /// ⌘K's `Turn Off Phone Notifications`.
    PhoneNotificationsOff,
    /// ⌘K's `Create Remote MCP Token`: copies the command that adds Examail Cloud's MCP server
    /// to Claude Code.
    McpTokenCreate,
    /// ⌘K's `Revoke Remote MCP Tokens`.
    McpTokensRevoke,
}

/// The ntfy server phone notifications go through.
const NTFY: &str = "https://ntfy.sh";

impl Notice {
    fn title(self) -> &'static str {
        match self {
            Self::VersionInfo => "Version Info",
            Self::UpdateRequired => "Update Required",
            Self::Update(_) => update::ALERT_TITLE,
        }
    }

    /// Its button.
    fn action(self) -> &'static str {
        match self {
            Self::VersionInfo | Self::UpdateRequired => "OK",
            Self::Update(alert) => alert.action(),
        }
    }

    /// Its text, a line per `\n`.
    pub fn text(self) -> String {
        match self {
            // The version alone.
            Self::VersionInfo => format!("- Version: {VERSION}"),
            Self::UpdateRequired => UPDATE_REQUIRED.iter().map(AlertRun::text).collect(),
            Self::Update(alert) => alert.text(),
        }
    }
}

/// Registers `Version Info`, `Open Source Attribution` and, when Exo updates itself,
/// `Check for Updates`, and the open alert's keys.
pub(crate) fn register(
    commands: &mut Commands<commands::Action>,
    open: Option<Notice>,
    updates: bool,
    phone_notifications: Option<bool>,
) {
    let act = commands::Action::Notice;
    // Only with Examail Cloud on, which sends them.
    match phone_notifications {
        Some(false) => commands.add(
            Command::new(
                "PHONE_NOTIFICATIONS_ON",
                "Turn On Phone Notifications",
                act(Action::PhoneNotificationsOn),
            )
            .aliases(["ntfy", "Push Notifications"])
            .default_score(0.0)
            .icon("data/icon"),
        ),
        Some(true) => commands.add(
            Command::new(
                "PHONE_NOTIFICATIONS_OFF",
                "Turn Off Phone Notifications",
                act(Action::PhoneNotificationsOff),
            )
            .aliases(["ntfy", "Push Notifications"])
            .default_score(0.0)
            .icon("data/icon"),
        ),
        None => {}
    }
    if phone_notifications.is_some() {
        commands.add(
            Command::new(
                "MCP_TOKEN_CREATE",
                "Create Remote MCP Token",
                act(Action::McpTokenCreate),
            )
            .aliases(["MCP", "Claude", "AI Agent"])
            .default_score(0.0)
            .icon("data/icon"),
        );
        commands.add(
            Command::new(
                "MCP_TOKENS_REVOKE",
                "Revoke Remote MCP Tokens",
                act(Action::McpTokensRevoke),
            )
            .aliases(["MCP", "Claude", "AI Agent"])
            .default_score(0.0)
            .icon("data/icon"),
        );
    }
    commands.add(
        Command::new("VERSION", "Version Info", act(Action::VersionInfo))
            .default_score(0.0)
            .icon("data/icon"),
    );
    commands.add(
        Command::new(
            "OPEN_SOURCE_LINK",
            "Open Source Attribution",
            act(Action::OpenSourceAttribution),
        )
        .default_score(0.0)
        .icon("data/open-links-link"),
    );
    if updates {
        commands.add(
            Command::new("UPGRADE", "Check for Updates", act(Action::CheckForUpdates))
                .aliases(["Update Examail", "Upgrade Version"])
                .operative_word("update")
                .default_score(0.9)
                .icon("data/icon"),
        );
    }
    if let Some(open) = open {
        register_alert_dismiss(commands, act(Action::Dismiss));
        if open == Notice::VersionInfo {
            commands.add(
                Command::shortcut("ALERT_COPY", act(Action::Copy))
                    .key("mod+c")
                    .regions(ALERT),
            );
        }
    }
}

/// Where Exo's third-party notices are, which is in `Exo.app`'s resources (`cargo xtask build`), or
/// else in the checkout.
fn notices() -> std::path::PathBuf {
    crate::bundled_resource("THIRD_PARTY_NOTICES.md")
        .filter(|file| file.is_file())
        .unwrap_or_else(|| {
            std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../THIRD_PARTY_NOTICES.md"
            ))
        })
}

impl crate::Mail {
    /// Carries out `action`.
    pub(crate) fn notice_run(&mut self, action: Action) {
        match action {
            Action::VersionInfo => self.notice = Some(Notice::VersionInfo),
            // The notices are a file in the app.
            Action::OpenSourceAttribution => self.ctx.open_url(egui::OpenUrl::new_tab(format!(
                "file://{}",
                notices().display()
            ))),
            // The check's alert takes the place of any other.
            Action::CheckForUpdates => {
                self.notice = None;
                if let Some(updates) = &self.updates {
                    updates.check(Mode::Alerts);
                }
            }
            Action::Dismiss => match self.notice() {
                Some(Notice::Update(_)) => {
                    if let Some(updates) = &self.updates {
                        updates.dismiss();
                    }
                }
                _ => self.notice = None,
            },
            Action::PhoneNotificationsOn => {
                let mut bytes = [0u8; 16];
                if getrandom::fill(&mut bytes).is_err() {
                    return;
                }
                let topic: String = std::iter::once("examail-".to_owned())
                    .chain(bytes.iter().map(|byte| format!("{byte:02x}")))
                    .collect();
                self.account.update_settings(|settings| {
                    settings.ntfy = Some(exo_store::Ntfy {
                        server: NTFY.to_owned(),
                        topic: topic.clone(),
                    });
                });
                self.ctx.copy_text(format!("{NTFY}/{topic}"));
                self.commands.hint(Hint::new().text(
                    "Phone notifications on. Subscribe to the copied topic in the ntfy app.",
                ));
            }
            Action::PhoneNotificationsOff => {
                self.account
                    .update_settings(|settings| settings.ntfy = None);
                self.commands
                    .hint(Hint::new().text("Phone notifications off."));
            }
            Action::McpTokenCreate => {
                self.account.mint_mcp_token(false);
                self.commands
                    .hint(Hint::new().text("Creating a remote MCP token…"));
            }
            Action::McpTokensRevoke => {
                self.account.mint_mcp_token(true);
                self.commands
                    .hint(Hint::new().text("Revoking remote MCP tokens…"));
            }
            Action::Copy => {
                if let Some(notice) = self.notice {
                    self.ctx.copy_text(notice.text());
                    self.commands
                        .hint(Hint::new().text("Alert copied to clipboard."));
                }
            }
        }
    }

    /// Examail Cloud's answer to `Create Remote MCP Token` or `Revoke Remote MCP Tokens`.
    pub(crate) fn mcp_token(&mut self, answer: Result<Option<(String, String)>, String>) {
        let text = match answer {
            Ok(Some((token, url))) => {
                self.ctx.copy_text(format!(
                    "claude mcp add --transport http examail {url} --header \"Authorization: Bearer {token}\""
                ));
                "Copied a command that adds Examail to Claude Code. Keep it secret: it reads your mail.".to_owned()
            }
            Ok(None) => "Remote MCP tokens revoked.".to_owned(),
            Err(why) => format!("Examail Cloud: {why}"),
        };
        self.commands.hint(Hint::new().text(text));
    }

    /// The alert on screen, if any, with the updater's check's alert over any other.
    pub fn notice(&self) -> Option<Notice> {
        self.updates
            .as_ref()
            .and_then(update::Updates::alert)
            .map(Notice::Update)
            .or(self.notice)
    }

    /// Checks for updates with `updates` (`None` when Exo doesn't update itself, and ⌘K has no
    /// `Check for Updates`).
    pub fn set_updates(&mut self, updates: Option<update::Updates>) {
        self.updates = updates;
    }

    /// Draws the alert, if one is up, over `page` dimmed.
    pub(crate) fn show_notice(&mut self, ui: &mut egui::Ui, page: Rect) {
        let Some(notice) = self.notice() else { return };
        let ctx = ui.ctx().clone();
        let text = notice.text();
        let alert = exo_ui::Alert {
            title: notice.title(),
            text: &text,
            actions: &[notice.action()],
            selected: 0,
        };
        let clicked = match notice {
            // The copy hint, on the action row's line at the text's left.
            Notice::VersionInfo => {
                let runs = [AlertRun::Text(&text)];
                let (clicked, _) = alert.show_runs_modal_with(
                    &ctx,
                    "notice-alert",
                    page,
                    &runs,
                    0.0,
                    |ui, extra| {
                        let subtext = format!(
                            "Press {} C to copy",
                            exo_ui::keycap_label("mod", ui.ctx().os())
                        );
                        ALERT_TEXT.paint_line(
                            ui.painter(),
                            pos2(extra.left(), extra.top() + SUBTEXT_BELOW_TEXT),
                            &subtext,
                            SUBTEXT_INK,
                            None,
                        );
                    },
                );
                clicked.is_some()
            }
            Notice::UpdateRequired => alert
                .show_runs_modal(&ctx, "notice-alert", page, UPDATE_REQUIRED)
                .0
                .is_some(),
            Notice::Update(_) => alert.show_modal(&ctx, "notice-alert", page).0.is_some(),
        };
        if clicked {
            self.notice_run(Action::Dismiss);
        }
    }
}
