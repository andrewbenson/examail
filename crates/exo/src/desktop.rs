//! Exo as a Mac app. Closing the window hides it and Exo keeps syncing, until the Dock icon,
//! another launch or a click on a notification brings it back, and ⌘Q quits. The window opens where
//! it was left (minimized into the Dock when macOS opens Exo at login), the Dock badge counts the
//! list on screen, and one Exo runs at a time, so a later launch hands its `mailto:` link to the
//! running one. [`menu`] builds the menu bar. Quitting asks first ([`confirm_quit`]).

#[cfg(target_os = "macos")]
pub mod applications;
pub mod context_menu;
pub mod drag;
mod instance;
#[cfg(target_os = "macos")]
mod macos;
pub mod menu;
mod opening;
mod placement;
mod quit;
pub(crate) mod spelling;
#[cfg(target_os = "macos")]
mod text_input;
pub(crate) mod titles;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use egui::{Key, Modifiers, ViewportCommand};

use crate::notifications::Click;

pub use instance::{Unclaimed, claim, launch_url};
pub use opening::Opening;
pub use placement::Placement;
pub use quit::QuitWarning;

/// Asks before Exo quits for ⌘Q, the app menu's `Quit Exo` and the Dock's Quit while `warning` is
/// on. Ticking the alert's `Do not ask again` and choosing `Quit` turns `warning` off. Exo doesn't
/// ask when macOS ends the session or when Exo quits by itself. Call on the main thread once the
/// menu bar is up (in eframe's app creator).
pub fn confirm_quit(warning: QuitWarning) {
    #[cfg(target_os = "macos")]
    macos::confirm_quit_from(warning);
    #[cfg(not(target_os = "macos"))]
    drop(warning);
}

/// Opens web link `url` without bringing the browser to the front (`NSWorkspace` with `activates`
/// off). Returns whether it could, which it can only on a Mac and from the main thread.
pub(crate) fn open_in_background(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    return macos::open_in_background(url);
    #[cfg(not(target_os = "macos"))]
    {
        drop(url);
        false
    }
}

/// Shows a modal alert with `message`, `detail` and `buttons` (the default first), with Exo in
/// front, and waits for a button to be chosen. Returns the chosen button's index, from 0, or `None`
/// where there's no Mac to ask. The main thread shows the alert, so call this from another thread.
pub(crate) fn ask(message: &str, detail: &str, buttons: &[&str]) -> Option<usize> {
    #[cfg(target_os = "macos")]
    return macos::ask(message, detail, buttons);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (message, detail, buttons);
        None
    }
}

/// Quits Exo without asking, once the main thread gets to it.
pub(crate) fn quit() {
    #[cfg(target_os = "macos")]
    macos::terminate_soon();
}

/// Opens Exo without taking the focus from the app in front, as it does after restarting for an
/// update.
pub fn without_activating(builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>) {
    #[cfg(target_os = "macos")]
    winit::platform::macos::EventLoopBuilderExtMacOS::with_activate_ignoring_other_apps(
        builder, false,
    );
    #[cfg(not(target_os = "macos"))]
    let _ = builder;
}

/// Whether macOS's accent colour is Graphite (`AppleAccentColor` is -1), which greys the
/// strip's traffic lights.
pub(crate) fn graphite_accent() -> bool {
    #[cfg(target_os = "macos")]
    return macos::graphite_accent();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Whether this Mac's Escape key is on its Touch Bar (the 2016–2019 MacBook Pros), the Macs that
/// the Touch Bar tip is for.
pub(crate) fn escape_on_touch_bar() -> bool {
    static ON_TOUCH_BAR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON_TOUCH_BAR.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let model = macos::model();
        #[cfg(not(target_os = "macos"))]
        let model: Option<String> = None;
        model.is_some_and(|model| crate::touch_bar_tip::escape_on_touch_bar(&model))
    })
}

/// What another launch or macOS asks of the running Exo.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// The window, in front.
    Show,
    /// A link to open, in front.
    Open(String),
    /// A clicked notification. What it's about opens in front.
    Clicked(Click),
    /// The Help menu's `Clear Cache and Restart`, confirmed.
    ClearCache,
    /// View ▸ `Reload`.
    Reload,
    /// Edit ▸ `Enable Autocorrect`.
    ToggleAutocorrect,
}

/// What a menu item or macOS hands to whatever has the keyboard, on top of the window's own events.
#[derive(Debug, PartialEq, Eq)]
enum Input {
    /// The keystroke of a clicked menu item.
    Keystroke(menu::Keystroke),
    /// A paste keystroke whose caller already verified the system pasteboard holds no plain text,
    /// avoiding a second synchronous clipboard read.
    KeystrokeWithoutText(menu::Keystroke),
    /// Text macOS inserted with no key behind it (Emoji & Symbols, dictation).
    Text(String),
}

/// Hands the running Exo what other launches and macOS ask of it: the window (a click on the Dock
/// icon, Exo opened again), a link (a `mailto:` one), or what a clicked notification is about.
/// Cheap to clone, and usable from any thread. A request made before the window exists waits for
/// the window.
#[derive(Clone, Default)]
pub struct Launcher(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    requests: Vec<Request>,
    input: Vec<Input>,
    /// The window's context, woken for each request, or `None` until the window exists.
    ctx: Option<egui::Context>,
    /// Whether autocorrect is on for the account on screen, which decides whether Edit ▸ `Enable
    /// Autocorrect` shows checked. `None` when no account's mail is showing.
    autocorrect: Option<bool>,
    /// The updater behind the app menu's item and the notification for a ready update.
    updates: Option<crate::update::Updates>,
}

impl Launcher {
    /// Brings the window back and to the front, as a click on the Dock icon does and another launch
    /// with nothing to open does.
    pub fn show(&self) {
        self.push(Request::Show);
    }

    /// Opens `url`. The window comes to the front and, for a `mailto:` link, a new message to the
    /// link's address opens in the account on screen (once there's one, off the sign-in screen).
    /// Links that macOS hands Exo (`kAEGetURL`) come in here, as do those that another launch
    /// passes on.
    pub fn open_url(&self, url: &str) {
        self.push(Request::Open(url.to_owned()));
    }

    /// A notification was clicked. The window comes to the front, shown and focused, and `click`
    /// opens in the notification's account.
    pub fn clicked(&self, click: Click) {
        self.push(Request::Clicked(click));
    }

    /// Clears every account's cache and starts over, as the Help menu's `Clear Cache and Restart`
    /// does once confirmed.
    pub fn clear_cache(&self) {
        self.push(Request::ClearCache);
    }

    /// Starts the account on screen over, as View ▸ `Reload` does.
    pub fn reload(&self) {
        self.push(Request::Reload);
    }

    /// Turns autocorrect the other way for every account, as Edit ▸ `Enable Autocorrect` does.
    pub fn toggle_autocorrect(&self) {
        self.push(Request::ToggleAutocorrect);
    }

    /// Whether Edit ▸ `Enable Autocorrect` shows checked (`None` when there's no account to say).
    pub fn autocorrect(&self) -> Option<bool> {
        self.lock().autocorrect
    }

    /// Updates Exo with `updates`, which the app menu's item and the notification of a ready update
    /// go to. Set it before the event loop starts, because starting the event loop builds the menu
    /// bar.
    pub fn set_updates(&self, updates: crate::update::Updates) {
        self.lock().updates = Some(updates);
    }

    /// The updater, if Exo updates itself.
    pub(crate) fn updates(&self) -> Option<crate::update::Updates> {
        self.lock().updates.clone()
    }

    /// Installs the update ready, as its notification's `Install and Restart` does.
    pub fn install_update(&self) {
        if let Some(updates) = self.updates() {
            updates.install();
        }
    }

    /// Wakes the window for a frame, once there's one.
    pub(crate) fn repaint(&self) {
        if let Some(ctx) = &self.lock().ctx {
            ctx.request_repaint();
        }
    }

    /// Hands `keystroke` to whatever has the keyboard, as a click on its menu item does.
    pub fn keystroke(&self, keystroke: menu::Keystroke) {
        self.give(Input::Keystroke(keystroke));
    }

    /// Hands `keystroke` to whatever has the keyboard when the pasteboard is already known to hold
    /// no text.
    #[cfg(target_os = "macos")]
    pub(super) fn keystroke_without_text(&self, keystroke: menu::Keystroke) {
        self.give(Input::KeystrokeWithoutText(keystroke));
    }

    /// Hands `text` to whatever has the keyboard as if typed, as macOS's Emoji & Symbols does.
    pub fn text(&self, text: String) {
        self.give(Input::Text(text));
    }

    fn push(&self, request: Request) {
        let mut pending = self.lock();
        pending.requests.push(request);
        if let Some(ctx) = &pending.ctx {
            ctx.request_repaint();
        }
    }

    fn give(&self, input: Input) {
        let mut pending = self.lock();
        pending.input.push(input);
        if let Some(ctx) = &pending.ctx {
            ctx.request_repaint();
        }
    }

    /// Wakes `ctx` for each request from now on, and wakes it now if requests are already waiting.
    fn attach(&self, ctx: &egui::Context) {
        let mut pending = self.lock();
        if !pending.requests.is_empty() || !pending.input.is_empty() {
            ctx.request_repaint();
        }
        pending.ctx = Some(ctx.clone());
    }

    fn take(&self) -> Vec<Request> {
        std::mem::take(&mut self.lock().requests)
    }

    fn take_input(&self) -> Vec<Input> {
        std::mem::take(&mut self.lock().input)
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The window's life as a Mac app (see the module docs), which [`App`](crate::App) drives.
pub struct Desktop {
    launcher: Launcher,
    placement: Placement,
    /// Saves the placement off the UI thread.
    files: fastframe_file::Writer,
    /// Whether the window was closed. A closed window is hidden, and Exo runs on.
    hidden: bool,
    /// Whether closing took the window out of full screen first. The window hides once it's out.
    leaving_full_screen: bool,
    /// Says when the window is out of full screen. `None` off the main thread (in tests).
    #[cfg(target_os = "macos")]
    full_screen_exits: Option<macos::FullScreenExits>,
    /// Links asked for, to open on the screen showing.
    links: Vec<String>,
    /// Notifications clicked, to open in their accounts.
    clicks: Vec<Click>,
    /// The Dock's badge, as last set.
    badge: String,
    /// Set when the window opened minimized, until it's back on screen.
    docked: Option<Docked>,
    /// The Help menu's `Clear Cache and Restart` was confirmed, for the app to do.
    clear_cache: bool,
    /// View ▸ `Reload` was chosen, for the app to do.
    reload: bool,
    /// Edit ▸ `Enable Autocorrect` was chosen.
    toggle_autocorrect: bool,
    /// The modifiers held before a clicked item's keystroke set its own, to restore on the frame
    /// after.
    held: Option<Modifiers>,
}

/// A window opened minimized ([`Opening::Minimized`]), until it's back on screen.
struct Docked {
    /// Whether its first frame has minimized it.
    minimized: bool,
    /// Whether it goes back to full screen, as it was left.
    full_screen: bool,
}

impl Desktop {
    /// The window of `ctx`, opened at `placement` the way `opening` says, and shown again for what
    /// `launcher` hands over. Make it in eframe's app creator, before the window's first frame,
    /// because Exo has finished launching there and only from then on can it hear a click on the
    /// Dock icon.
    pub fn new(
        ctx: &egui::Context,
        launcher: Launcher,
        placement: Placement,
        opening: Opening,
    ) -> Self {
        launcher.attach(ctx);
        #[cfg(target_os = "macos")]
        macos::on_reopen(launcher.clone());
        menu::window_ready();
        #[cfg(target_os = "macos")]
        text_input::listen(launcher.clone());
        let docked = (opening == Opening::Minimized).then(|| Docked {
            minimized: false,
            full_screen: placement.full_screen(),
        });
        // eframe orders a new window in for its first frame, and that frame minimizes it. With no
        // animation for either, the window never reaches the screen and goes straight to the Dock,
        // as a window never shown does.
        #[cfg(target_os = "macos")]
        if docked.is_some() {
            macos::set_animations(false);
        }
        Self {
            launcher,
            placement,
            files: fastframe_file::Writer::new(),
            hidden: false,
            leaving_full_screen: false,
            #[cfg(target_os = "macos")]
            full_screen_exits: macos::FullScreenExits::listen(ctx),
            links: Vec::new(),
            clicks: Vec::new(),
            badge: String::new(),
            docked,
            clear_cache: false,
            reload: false,
            toggle_autocorrect: false,
            held: None,
        }
    }

    /// Adds what the menu items and macOS handed to whatever has the keyboard since the last frame
    /// to this frame's input, as egui-winit would: a clicked item's keystroke, with its modifiers
    /// held for that frame only, and text that macOS inserted. eframe calls this before each frame
    /// (`raw_input_hook`).
    pub(crate) fn input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(held) = self.held.take() {
            // First, so it comes before any change egui-winit reports since.
            raw.events.insert(0, egui::Event::ModifiersChanged(held));
        }
        // A drag out of the window (a card's or a selection's) ended in AppKit, which took the
        // button's release, so add the release here.
        #[cfg(target_os = "macos")]
        if drag::take_release()
            && let Some(pos) = ctx.input(|input| input.pointer.latest_pos())
        {
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        for input in self.launcher.take_input() {
            match input {
                Input::Keystroke(keystroke) | Input::KeystrokeWithoutText(keystroke) => {
                    let read_text = matches!(input, Input::Keystroke(_));
                    let (modifiers, events) = keystroke_events(
                        keystroke,
                        || {
                            read_text
                                .then(|| {
                                    arboard::Clipboard::new()
                                        .and_then(|mut clipboard| clipboard.get_text())
                                        .ok()
                                })
                                .flatten()
                        },
                        crate::clipboard::holds_attachments,
                    );
                    self.held
                        .get_or_insert_with(|| ctx.input(|input| input.modifiers));
                    raw.events.push(egui::Event::ModifiersChanged(modifiers));
                    raw.events.extend(events);
                    ctx.request_repaint();
                }
                Input::Text(text) => raw.events.push(egui::Event::Text(text)),
            }
        }
    }

    /// Closes, shows and places the window, before the frame draws. eframe calls this part of a
    /// frame for a hidden or minimized window too, so Exo sees a close or a Dock click whatever
    /// state the window is in. `last_tab` says whether the window has one tab left, which ⌘W closes
    /// along with the window. Returns whether the window hid.
    pub(crate) fn logic(&mut self, ctx: &egui::Context, last_tab: bool) -> bool {
        let docked = self.dock(ctx);
        let hidden = self.hidden;
        let close = ctx.input_mut(|input| {
            // ⇧⌘W closes the window (`Close Window`) and keeps its tabs. ⌘W closes the last tab,
            // and the window with it (`Close Tab`). With more tabs open, ⌘W belongs to the tabs.
            let window =
                !hidden && input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::W);
            let tab = !hidden && last_tab && input.consume_key(Modifiers::COMMAND, Key::W);
            window || tab || input.viewport().close_requested()
        });
        let hide = if close {
            // The last window hides rather than closes, and Exo runs on. It leaves full screen
            // first, or its Space would stay behind, empty.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if self.leaving_full_screen {
                // Already on its way out.
                false
            } else if in_full_screen(ctx) {
                // Only the exit that this asks for counts.
                self.left_full_screen(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                self.leaving_full_screen = true;
                false
            } else {
                true
            }
        } else {
            self.leaving_full_screen && self.left_full_screen(ctx)
        };
        if hide {
            self.hidden = true;
            self.leaving_full_screen = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        for request in self.launcher.take() {
            match request {
                Request::Show => {}
                Request::Open(url) => self.links.push(url),
                Request::Clicked(click) => self.clicks.push(click),
                Request::ClearCache => self.clear_cache = true,
                // What's on screen starts over where it is, without showing the window.
                Request::Reload => {
                    self.reload = true;
                    continue;
                }
                Request::ToggleAutocorrect => {
                    self.toggle_autocorrect = true;
                    continue;
                }
            }
            self.show(ctx);
        }
        // Don't save the placement while the window animates out of full screen. Once it's out, its
        // full screen is over, hidden or not. Nor while it's in the Dock, which says nothing about
        // where it goes back to.
        if !self.leaving_full_screen && !docked && self.placement.remember(ctx) {
            self.files.write(
                self.placement.path(),
                self.placement.contents(),
                fastframe_file::Access::Default,
            );
        }
        hide
    }

    /// A window opened minimized goes to the Dock on its first frame, just after eframe orders it
    /// in. Once it's on screen again (its window in the Dock clicked, Exo's icon clicked, Exo
    /// opened again), it goes back to full screen if it was left there. Returns whether it was in
    /// the Dock.
    fn dock(&mut self, ctx: &egui::Context) -> bool {
        let Some(docked) = &mut self.docked else {
            return false;
        };
        if !docked.minimized {
            docked.minimized = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        } else if on_screen(ctx) {
            // On the Mac, egui hears that the window is minimized only from Exo (it reads
            // `minimized` when the window is made), and paints nothing while it is.
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            if docked.full_screen {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
            }
            #[cfg(target_os = "macos")]
            macos::set_animations(true);
            self.docked = None;
        }
        true
    }

    /// Whether the window is hidden, because it was closed and nothing has brought it back.
    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// Whether the Help menu's `Clear Cache and Restart` was confirmed since the last call.
    pub(crate) fn take_clear_cache(&mut self) -> bool {
        std::mem::take(&mut self.clear_cache)
    }

    /// Whether View ▸ `Reload` was chosen since the last call.
    pub(crate) fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.reload)
    }

    /// Whether Edit ▸ `Enable Autocorrect` was chosen since the last call.
    pub(crate) fn take_toggle_autocorrect(&mut self) -> bool {
        std::mem::take(&mut self.toggle_autocorrect)
    }

    /// Checks Edit ▸ `Enable Autocorrect` as `on` says for the account on screen (`None` when
    /// there's no account).
    pub(crate) fn set_autocorrect(&self, on: Option<bool>) {
        self.launcher.lock().autocorrect = on;
    }

    /// The links asked for since the last call, to open on the screen showing.
    pub(crate) fn take_links(&mut self) -> Vec<String> {
        std::mem::take(&mut self.links)
    }

    /// The notifications clicked since the last call.
    pub(crate) fn take_clicks(&mut self) -> Vec<Click> {
        std::mem::take(&mut self.clicks)
    }

    /// Something kept panicking (`recovery`). An alert says so, then Exo quits without asking
    /// first.
    pub(crate) fn crashed_repeatedly(&self, gave_up: crate::recovery::GaveUp) {
        #[cfg(target_os = "macos")]
        macos::crashed_repeatedly(gave_up);
        #[cfg(not(target_os = "macos"))]
        let _ = gave_up;
    }

    /// Shows `label` on the Dock icon, or no badge when `label` is empty.
    pub(crate) fn set_badge(&mut self, label: &str) {
        if self.badge == label {
            return;
        }
        label.clone_into(&mut self.badge);
        #[cfg(target_os = "macos")]
        macos::set_badge(label);
    }

    /// Whether the window has finished leaving full screen since the last call, as AppKit reports
    /// once the animation is over, or else as egui has it.
    fn left_full_screen(&self, ctx: &egui::Context) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(exits) = &self.full_screen_exits {
            return exits.take();
        }
        ctx.input(|input| input.viewport().fullscreen != Some(true))
    }

    /// Brings the window back: shown, out of the Dock if it was minimized, and in front.
    fn show(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.leaving_full_screen = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // A placement still being written is saved before Exo quits.
        self.files.flush(fastframe_file::FLUSH_TIMEOUT);
    }
}

/// Whether the window is on screen, shown and out of the Dock, as AppKit has it on the Mac, or else
/// as egui does.
fn on_screen(ctx: &egui::Context) -> bool {
    let reported = ctx.input(|input| input.viewport().occluded == Some(false));
    #[cfg(target_os = "macos")]
    return macos::on_screen().unwrap_or(reported);
    #[cfg(not(target_os = "macos"))]
    reported
}

/// The modifiers and egui events for a menu or system `keystroke`. When pasting with no clipboard
/// text while copied attachments are held, replaces the fallback `Event::Key` pair with
/// `Event::Paste(String::new())` so `compose_paste_attachments` handles the paste.
fn keystroke_events(
    keystroke: menu::Keystroke,
    clipboard: impl FnOnce() -> Option<String>,
    holds_attachments: impl FnOnce() -> bool,
) -> (Modifiers, Vec<egui::Event>) {
    let (modifiers, mut events) = keystroke.input(clipboard);
    if matches!(
        keystroke,
        menu::Keystroke::Paste | menu::Keystroke::PasteAndMatchStyle
    ) && !events
        .iter()
        .any(|event| matches!(event, egui::Event::Paste(_)))
        && holds_attachments()
    {
        events = vec![egui::Event::Paste(String::new())];
    }
    (modifiers, events)
}

/// Whether the window is in full screen, as AppKit has it on the Mac, or else as egui does.
fn in_full_screen(ctx: &egui::Context) -> bool {
    let reported = ctx.input(|input| input.viewport().fullscreen == Some(true));
    #[cfg(target_os = "macos")]
    return macos::in_full_screen().unwrap_or(reported);
    #[cfg(not(target_os = "macos"))]
    reported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_keystroke_emits_paste_event_when_clipboard_holds_attachments_without_text() {
        for keystroke in [menu::Keystroke::Paste, menu::Keystroke::PasteAndMatchStyle] {
            for text in [None, Some(String::new())] {
                let (_, events) = keystroke_events(keystroke, || text, || true);
                assert_eq!(
                    events,
                    vec![egui::Event::Paste(String::new())],
                    "{keystroke:?}"
                );
            }
        }

        // Without attachments held (e.g. a screenshot image), the fallback Key events stay so
        // compose handles image paste.
        let (_, events) = keystroke_events(menu::Keystroke::Paste, || None, || false);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, egui::Event::Key { key: Key::V, .. }))
        );

        // With non-empty clipboard text, the text paste event is preserved as-is.
        let (_, events) =
            keystroke_events(menu::Keystroke::Paste, || Some("hello".into()), || true);
        assert_eq!(events, vec![egui::Event::Paste("hello".into())]);

        // Non-paste keystrokes are unaffected even when attachments are held.
        let (_, events) = keystroke_events(menu::Keystroke::Copy, || None, || true);
        assert_eq!(events, vec![egui::Event::Copy]);
    }
}
