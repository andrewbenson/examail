//! The thread list's commands, with their keys, regions, palette titles, aliases and scores,
//! registered every frame for what's on screen.
//!
//! Only what works end to end is registered, so there's no AI. Compose, search and the list filters
//! register their own (`compose::register_shortcuts`, `search::register`,
//! `filter::register`).

use exo_commands::{CONVERSATION, Command, Commands, Region};
use exo_store::{Label, ListId, Mutation};

use crate::{
    account::UndoToken,
    folders::Key as FolderKey,
    lists::{self, GoTo},
    mailbox::ThreadSummary,
    pickers::Action as Pick,
    remind::Action as Remind,
    thread::{GutterKey, HeaderKey as ThreadHeader},
};

/// Where list navigation and selection keys live.
const THREAD_LIST: &[Region] = &[Region::ThreadList];
/// Where `g` sequences work: the list and thread regions and the label switcher.
const GO_TO: &[Region] = &[
    Region::ThreadListHeader,
    Region::ThreadList,
    Region::ActionPane,
    Region::ThreadMessagesHeader,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ReadOnlySnippet,
    Region::LabelSwitcher,
];
/// Where `g` sequences also work, closing the week view first.
const WEEK_GO_TO: &[Region] = &[Region::SeeYourWeek, Region::SeeYourWeekSidebar];

/// Where the thread view's message keys live.
const THREAD_MESSAGES: &[Region] = &[Region::ThreadMessages];
/// Where the arrows, N and P, `g g`, ⇧G, Enter and ⌘C work: the messages and the Show New
/// Messages bar.
const MESSAGES_AND_NEW: &[Region] = &[Region::ThreadMessages, Region::ThreadMessagesNewMessages];
/// Where ⇧N works.
const THREAD_PANE: &[Region] = &[
    Region::ThreadMessagesHeader,
    Region::ActionPane,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ThreadMessagesNewMessages,
];
/// Where Escape leaves the thread.
const THREAD_BACK: &[Region] = &[
    Region::ThreadMessages,
    Region::ActionPane,
    Region::ReadOnlySnippet,
];
/// Where J/K move between conversations without leaving the thread (`ActionPane`).
const NEXT_CONVERSATION: &[Region] = &[
    Region::ActionPane,
    Region::ThreadMessagesHeader,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ReadOnlySnippet,
];

/// A key in the list's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderKey {
    /// ↓, J, Escape: back to the list.
    Down,
    /// ← and →: the previous or next item.
    Move(isize),
    /// Enter: what a click does.
    Enter,
    /// Backspace: the filter tab goes.
    Backspace,
}

/// What a command does, for the app to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Move the cursor this many rows.
    Move(isize),
    /// ↑ in the list: up a row, or from the first one into the header (the search bar while
    /// searching).
    Up,
    /// Something in the list's header with the keyboard.
    Header(HeaderKey),
    /// Jump to the first row.
    Top,
    /// Jump to the last row.
    Bottom,
    /// Page down (1) or up (-1).
    Page(isize),
    /// Open the cursor's thread.
    Open,
    /// Select or unselect the cursor's thread.
    ToggleSelected,
    /// Extend the selection while moving this many rows.
    Extend(isize),
    /// Select every thread.
    SelectAll,
    /// Select the cursor's thread and those below it.
    SelectAllFromHere,
    /// Clear the selection.
    DeselectAll,
    /// Escape or Backspace: clear the selection, or go back. Escape goes back to where the back
    /// stack began, which for a list is the home list. Backspace goes back to the previous list.
    Back {
        /// Escape rather than Backspace.
        escape: bool,
    },
    /// Apply a mutation to the actionable threads.
    Apply(Mutation),
    /// Something the Move and Label pickers do (`crate::pickers`).
    Pick(crate::pickers::Action),
    /// Undo the mutation behind this toast.
    Undo(UndoToken),
    /// Show a list.
    Go(ListId),
    /// Open the palette on `go to ` (`g l`).
    GoToLabel,
    /// Hint that Mute is ⇧M (`m`).
    MuteHint,
    /// Escape in a thread: collapse the focused message, or go back to the list.
    ThreadBack,
    /// `g u`: back to the list from a thread.
    LeaveThread,
    /// A `g` sequence in the week view: closes the week, then does its action. It does nothing
    /// while a new event is being made.
    FromWeek(Box<Action>),
    /// J / K in a thread: the next (1) or previous (-1) conversation.
    NextConversation(isize),
    /// N / P: the next (1) or previous (-1) message.
    NextMessage(isize),
    /// ↓ / ↑: scroll within the focused message, then move on.
    Arrow(isize),
    /// ↓ / ↑ held: as [`Self::Arrow`], without leaving the messages.
    ArrowHeld(isize),
    /// ← in the messages: scrolls a widened column left, else the back gutter takes the keyboard.
    ThreadLeft,
    /// ← held: scrolls a widened column left, never leaving the messages.
    ThreadLeftHeld,
    /// → in the messages, pressed or held: scrolls a widened column right.
    ThreadRight,
    /// ⇧N, or Enter on the bar: show the new messages, or else focus the last message.
    ShowNewMessages,
    /// A key in the thread's header.
    ThreadHeader(crate::thread::HeaderKey),
    /// → or Tab in the back gutter.
    ThreadGutter(crate::thread::GutterKey),
    /// Enter on a collapsed message: expand it.
    ExpandMessage,
    /// O: cycle the focused message through collapsed, expanded and quoted text shown.
    ToggleMessage,
    /// ⇧O: expand or collapse every message.
    ToggleAllMessages,
    /// ⇧H: show or hide the focused message's full header.
    ToggleHeader,
    /// Space / ⇧Space in a thread.
    PageThread(isize),
    /// The first (false) or last (true) message.
    JumpThread(bool),
    /// ⌘C: copy the selected text.
    Copy,
    /// ⌘C in the list: copy the cursor's conversation.
    CopyListed,
    /// Show the focused message's blocked remote images (`Show Images`).
    ShowImages,
    /// Something Unsubscribe does (`⌘U`, its picker).
    Unsubscribe(crate::unsubscribe::Action),
    /// Copy the focused message (`Copy Message Contents`).
    CopyMessage,
    /// Copy the open conversation (`Copy Conversation Contents`).
    CopyConversation,
    /// Copy the focused message's recipients (`Copy Recipients`).
    CopyRecipients,
    /// Copy the focused message's files, for pasting into a draft (`Copy Attachments`).
    CopyAttachments,
    /// Something the attachment viewer's keys do.
    Preview(crate::attachment_files::viewer::Action),
    /// Copy the thread's subject (`Copy Subject`).
    CopySubject,
    /// Print the open conversation (`Print`, ⌘P, File ▸ Print) through the macOS print panel.
    Print,
    /// Save the open conversation as a PDF (`Save as PDF`).
    SaveAsPdf,
    /// ← in the list: open or close the folder drawer.
    ToggleFolders,
    /// A key in the folder drawer.
    Folders(crate::folders::Key),
    /// A settings command, row or button.
    Settings(crate::settings::Action),
    /// Tab / ⇧Tab: the next (1) or previous (-1) split tab.
    SplitTab(isize),
    /// Tab or ⇧Tab held: the tab highlight moves that way, when pressed and on each repeat,
    /// without the list changing.
    SplitTabHold(isize),
    /// …let go: the highlighted tab's list shows.
    SplitTabRelease,
    /// Something compose does.
    Compose(crate::compose::Action),
    /// Something about snippets.
    Snippets(crate::snippets::Action),
    /// Something search does.
    Search(crate::search::Action),
    /// Something a list filter does.
    Filter(crate::filter::Action),
    /// Something the calendar does.
    Calendar(crate::calendar::Action),
    /// Something Get Me To Zero or Bulk Actions does.
    Bulk(crate::bulk::Action),
    /// Something Remind me does.
    Remind(crate::remind::Action),
    /// Something Out of Office Reply does.
    OutOfOffice(crate::out_of_office::Action),
    /// Open a Google app as the account (the palette's Google apps rows).
    GoogleApp(crate::google_apps::App),
    /// Copy a link to where the page is, or to its conversation (`crate::links`).
    Links(crate::links::Action),
    /// Something Open Links & Attachments does (`⌘O`, its list).
    OpenLinks(crate::open_links::Action),
    /// Making a split from the palette (`New Split Inbox`, `Add to Split Inbox`, …).
    NewSplit(crate::new_split::Action),
    /// Something the raw source viewer does (`|`).
    RawSource(crate::raw_source::Action),
    /// Something about the device's accounts: switching, adding, signing out.
    Accounts(crate::accounts::Action),
    /// A palette RSVP row: answer the open conversation's invitation (`crate::rsvp`).
    Rsvp(crate::thread::invite::Response),
    /// `Version Info`, `Open Source Attribution`, and their alert's keys (`crate::notice`).
    Notice(crate::notice::Action),
    /// Account setup's button and `Welcome to Exo`'s action (`crate::first_run`).
    FirstRun(crate::first_run::Action),
    /// Something the Achievements dialog does.
    Achievements(crate::achievements::Action),
    /// Something about Inbox Zero's photo.
    Backdrop(crate::backdrop::Action),
}

/// Declarative specification for a static shortcut or palette command.
struct CommandSpec {
    id: &'static str,
    title: Option<&'static str>,
    action: Action,
    repeat: Option<Action>,
    keys: &'static [&'static str],
    key_code: Option<&'static [&'static str]>,
    typing: Option<&'static str>,
    regions: Option<&'static [Region]>,
    aliases: &'static [&'static str],
    operative_word: Option<&'static str>,
    default_score: f64,
    scale: Option<f64>,
    follow: Option<&'static str>,
    multiple: bool,
    icon: Option<&'static str>,
}

const fn sc(
    id: &'static str,
    keys: &'static [&'static str],
    regions: &'static [Region],
    action: Action,
) -> CommandSpec {
    CommandSpec {
        id,
        title: None,
        action,
        repeat: None,
        keys,
        key_code: None,
        typing: None,
        regions: Some(regions),
        aliases: &[],
        operative_word: None,
        default_score: 0.0,
        scale: None,
        follow: None,
        multiple: false,
        icon: None,
    }
}

const fn sc_rep(
    id: &'static str,
    keys: &'static [&'static str],
    regions: &'static [Region],
    action: Action,
    repeat: Action,
) -> CommandSpec {
    CommandSpec {
        id,
        title: None,
        action,
        repeat: Some(repeat),
        keys,
        key_code: None,
        typing: None,
        regions: Some(regions),
        aliases: &[],
        operative_word: None,
        default_score: 0.0,
        scale: None,
        follow: None,
        multiple: false,
        icon: None,
    }
}

const fn row(id: &'static str, title: &'static str, action: Action) -> CommandSpec {
    CommandSpec {
        id,
        title: Some(title),
        action,
        repeat: None,
        keys: &[],
        key_code: None,
        typing: None,
        regions: None,
        aliases: &[],
        operative_word: None,
        default_score: 0.0,
        scale: None,
        follow: None,
        multiple: false,
        icon: None,
    }
}

const fn conv(
    id: &'static str,
    title: &'static str,
    icon: &'static str,
    score: f64,
    action: Action,
) -> CommandSpec {
    row(id, title, action)
        .icon(icon)
        .score(score)
        .multiple()
        .regions(CONVERSATION)
}

impl CommandSpec {
    const fn keys(mut self, keys: &'static [&'static str]) -> Self {
        self.keys = keys;
        self
    }
    const fn key_code(mut self, key_code: &'static [&'static str]) -> Self {
        self.key_code = Some(key_code);
        self
    }
    const fn when_typing(mut self, typing: &'static str) -> Self {
        self.typing = Some(typing);
        self
    }
    const fn regions(mut self, regions: &'static [Region]) -> Self {
        self.regions = Some(regions);
        self
    }
    const fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }
    const fn operative_word(mut self, word: &'static str) -> Self {
        self.operative_word = Some(word);
        self
    }
    const fn score(mut self, score: f64) -> Self {
        self.default_score = score;
        self
    }
    const fn scale(mut self, scale: f64) -> Self {
        self.scale = Some(scale);
        self
    }
    const fn follow(mut self, follow: &'static str) -> Self {
        self.follow = Some(follow);
        self
    }
    const fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }
    const fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    fn build(&self) -> Command<Action> {
        let mut cmd = match self.title {
            Some(title) => Command::new(self.id, title, self.action.clone()),
            None => Command::shortcut(self.id, self.action.clone()),
        };
        if !self.keys.is_empty() {
            cmd = cmd.keys(self.keys.iter().copied());
        }
        if let Some(tokens) = self.key_code {
            cmd = cmd.key_code(tokens.iter().copied());
        }
        if let Some(typing) = self.typing {
            cmd = cmd.when_typing(typing);
        }
        if let Some(regions) = self.regions {
            cmd = cmd.regions(regions);
        }
        if let Some(repeat) = &self.repeat {
            cmd = cmd.repeat(repeat.clone());
        }
        if !self.aliases.is_empty() {
            cmd = cmd.aliases(self.aliases.iter().copied());
        }
        if let Some(word) = self.operative_word {
            cmd = cmd.operative_word(word);
        }
        if self.default_score != 0.0 {
            cmd = cmd.default_score(self.default_score);
        }
        if let Some(scale) = self.scale {
            cmd = cmd.scale(scale);
        }
        if let Some(follow) = self.follow {
            cmd = cmd.follow(follow);
        }
        if self.multiple {
            cmd = cmd.multiple();
        }
        if let Some(icon) = self.icon {
            cmd = cmd.icon(icon);
        }
        cmd
    }
}

fn register_specs(commands: &mut Commands<Action>, specs: &[CommandSpec]) {
    for spec in specs {
        commands.add(spec.build());
    }
}

const THREAD_HDR: &[Region] = &[Region::ThreadMessagesHeader];
const GUTTER: &[Region] = &[Region::ActionPane];
const NEW_BAR: &[Region] = &[Region::ThreadMessagesNewMessages];
const LIST_HDR: &[Region] = &[Region::ThreadListHeader];
const LABEL_SW: &[Region] = &[Region::LabelSwitcher];
/// ⌘⇧H is compose's own in the reply pane.
const SHIFT_H_REGIONS: &[Region] = &[
    Region::ThreadList,
    Region::ThreadMessagesHeader,
    Region::ThreadMessages,
    Region::ThreadMessageCalendarInvite,
    Region::ThreadMessagesNewMessages,
];

const THREAD_HEADER_SPECS: &[CommandSpec] = &[
    sc(
        "HEADER_ESCAPE",
        &["esc"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Escape),
    ),
    sc(
        "HEADER_UP",
        &["up", "p", "ctralt+k", "ctralt+p"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Up),
    ),
    sc(
        "HEADER_DOWN",
        &["down", "n", "ctralt+j", "ctralt+n"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Down),
    ),
    sc(
        "HEADER_LEFT",
        &["left"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Step(-1)),
    ),
    sc(
        "HEADER_RIGHT",
        &["right"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Step(1)),
    ),
    sc(
        "HEADER_ENTER",
        &["enter"],
        THREAD_HDR,
        Action::ThreadHeader(ThreadHeader::Enter),
    ),
    // Enter goes back as the button does. ↑ and ↓ move between conversations.
    sc("ACTION_PANE_ENTER", &["enter"], GUTTER, Action::LeaveThread),
    sc(
        "ACTION_PANE_RIGHT",
        &["right", "tab"],
        GUTTER,
        Action::ThreadGutter(GutterKey::Right),
    ),
    sc(
        "ACTION_PANE_UP",
        &["up", "ctralt+k", "ctralt+p"],
        GUTTER,
        Action::NextConversation(-1),
    ),
    sc(
        "ACTION_PANE_DOWN",
        &["down", "ctralt+j", "ctralt+n"],
        GUTTER,
        Action::NextConversation(1),
    ),
];

const THREAD_TOP_SPECS: &[CommandSpec] = &[
    sc(
        "BACK",
        &["esc", "backspace"],
        THREAD_BACK,
        Action::ThreadBack,
    ),
    sc("GO_TO_LIST", &["g u"], GO_TO, Action::LeaveThread),
];

const THREAD_FOCUS_SPECS: &[CommandSpec] = &[
    sc(
        "NEXT_CONVERSATION",
        &["j"],
        NEXT_CONVERSATION,
        Action::NextConversation(1),
    ),
    sc(
        "PREVIOUS_CONVERSATION",
        &["k"],
        NEXT_CONVERSATION,
        Action::NextConversation(-1),
    ),
    sc_rep(
        "NEXT_MESSAGE",
        &["n", "ctralt+n"],
        MESSAGES_AND_NEW,
        Action::NextMessage(1),
        Action::NextMessage(1),
    ),
    sc_rep(
        "PREVIOUS_MESSAGE",
        &["p", "ctralt+p"],
        MESSAGES_AND_NEW,
        Action::NextMessage(-1),
        Action::NextMessage(-1),
    ),
    sc_rep(
        "FOCUS_DOWN",
        &["down", "ctralt+j"],
        MESSAGES_AND_NEW,
        Action::Arrow(1),
        Action::ArrowHeld(1),
    ),
    sc_rep(
        "FOCUS_UP",
        &["up", "ctralt+k"],
        MESSAGES_AND_NEW,
        Action::Arrow(-1),
        Action::ArrowHeld(-1),
    ),
    sc_rep(
        "FOCUS_LEFT",
        &["left"],
        MESSAGES_AND_NEW,
        Action::ThreadLeft,
        Action::ThreadLeftHeld,
    ),
    sc_rep(
        "FOCUS_RIGHT",
        &["right"],
        MESSAGES_AND_NEW,
        Action::ThreadRight,
        Action::ThreadRight,
    ),
];

const EXPAND_MESSAGE_SPEC: CommandSpec = sc(
    "EXPAND_MESSAGE",
    &["enter"],
    THREAD_MESSAGES,
    Action::ExpandMessage,
);
const SHOW_NEW_MESSAGES_ENTER_SPEC: CommandSpec = sc(
    "SHOW_NEW_MESSAGES_ENTER",
    &["enter"],
    NEW_BAR,
    Action::ShowNewMessages,
);

const THREAD_BODY_SPECS: &[CommandSpec] = &[
    row(
        "EXPAND_COLLAPSE_HEADER",
        "Expand / collapse Message header",
        Action::ToggleHeader,
    )
    .keys(&["shift+h"])
    .key_code(&["shift", "H"])
    .regions(THREAD_MESSAGES),
    row(
        "EXPAND_COLLAPSE_CURRENT",
        "Expand / collapse current Message",
        Action::ToggleMessage,
    )
    .keys(&["o"])
    .key_code(&["O"])
    .regions(THREAD_MESSAGES),
    row(
        "EXPAND_COLLAPSE_ALL",
        "Expand / collapse all Messages",
        Action::ToggleAllMessages,
    )
    .keys(&["shift+o"])
    .key_code(&["shift", "O"])
    .regions(THREAD_MESSAGES),
    sc_rep(
        "PAGE_DOWN",
        &["space", "pagedown"],
        THREAD_MESSAGES,
        Action::PageThread(1),
        Action::PageThread(1),
    ),
    sc_rep(
        "PAGE_UP",
        &["shift+space", "pageup"],
        THREAD_MESSAGES,
        Action::PageThread(-1),
        Action::PageThread(-1),
    ),
    sc(
        "JUMP_TO_TOP",
        &["mod+up", "home", "shift+p"],
        THREAD_MESSAGES,
        Action::JumpThread(false),
    ),
    sc(
        "JUMP_TO_TOP_SEQUENCE",
        &["g g"],
        MESSAGES_AND_NEW,
        Action::JumpThread(false),
    ),
    sc(
        "JUMP_TO_BOTTOM",
        &["mod+down", "end"],
        THREAD_MESSAGES,
        Action::JumpThread(true),
    ),
    sc(
        "JUMP_TO_BOTTOM_SHIFT",
        &["shift+g"],
        MESSAGES_AND_NEW,
        Action::JumpThread(true),
    ),
    sc(
        "SHOW_NEW_MESSAGES",
        &["shift+n"],
        THREAD_PANE,
        Action::ShowNewMessages,
    ),
];

/// Print and Save as PDF, in the thread view. ⌘P belongs in the File menu, but until Exo's menu
/// has Print, the key is the row's.
const THREAD_PRINT_SPECS: &[CommandSpec] = &[
    row("PRINT", "Print", Action::Print)
        .keys(&["mod+p"])
        .key_code(&["⌘", "P"])
        .score(1.45)
        .icon("data/print"),
    row("PRINT_TO_PDF", "Save as PDF", Action::SaveAsPdf)
        .aliases(&["Print to PDF", "Save to PDF", "Export to PDF"])
        .score(1.45)
        .icon("data/download"),
];

const SHOW_IMAGES_SPEC: CommandSpec = row("SHOW_IMAGES", "Show Images", Action::ShowImages)
    .aliases(&["Load Images", "See Images", "View Images"])
    .operative_word("show images")
    .follow("INSERT_IMAGE")
    .icon("data/insert-image");

/// What the thread view's commands depend on this frame.
pub struct ThreadContext {
    /// Whether the focused message is collapsed, so Enter expands it.
    pub focus_collapsed: bool,
    /// Whether the Show New Messages bar has the keyboard, so Enter shows the new messages.
    pub new_messages_focused: bool,
    /// Whether text is selected in the thread, so ⌘C copies it anywhere in the thread pane.
    pub has_selection: bool,
    /// Whether the focused message has remote images and they're blocked (`SHOW_IMAGES`).
    pub show_images: bool,
}

/// Registers the open thread's commands for this frame. Reply, forward and compose are compose's
/// (`compose::register_shortcuts`).
pub fn register_thread(commands: &mut Commands<Action>, cx: &ThreadContext) {
    register_specs(commands, THREAD_TOP_SPECS);
    commands.add(from_week("GO_TO_LIST", &["g u"], Action::LeaveThread));
    register_specs(commands, THREAD_FOCUS_SPECS);
    if cx.focus_collapsed {
        commands.add(EXPAND_MESSAGE_SPEC.build());
    }
    register_specs(commands, THREAD_BODY_SPECS);
    if cx.new_messages_focused {
        commands.add(SHOW_NEW_MESSAGES_ENTER_SPEC.build());
    }
    let copy_regions = if cx.has_selection {
        THREAD_PANE
    } else {
        MESSAGES_AND_NEW
    };
    commands.add(
        Command::shortcut("COPY", Action::Copy)
            .key("mod+c")
            .regions(copy_regions),
    );
    register_specs(commands, THREAD_HEADER_SPECS);
    register_specs(commands, THREAD_PRINT_SPECS);
    if cx.show_images {
        register_show_images(commands);
    }
}

/// Registers `Show Images`, which shows the remote images that the Images setting blocks in the
/// focused message or the focused draft.
pub fn register_show_images(commands: &mut Commands<Action>) {
    commands.add(SHOW_IMAGES_SPEC.build());
}

/// What the commands depend on this frame.
pub struct Context<'a> {
    /// The list on screen.
    pub list: &'a ListId,
    /// The threads that actions apply to, which are the selection or the cursor's.
    pub threads: &'a [&'a ThreadSummary],
    /// The account's labels.
    pub labels: &'a [Label],
    /// Which copy rows apply.
    pub copy: CopyRows,
    /// Where `g i` goes: home.
    pub home: ListId,
    /// Where `g o` goes: Other, or the Inbox.
    pub other: ListId,
    /// Whether Important and Other are off (`unifiedInbox`).
    pub unified: bool,
    /// The one actionable conversation's sender, if it isn't you.
    pub sender: Option<&'a crate::pickers::Sender>,
    /// Your addresses' domains.
    pub own_domains: &'a [String],
    /// Whether the selection is exactly the loaded rows. Remove all Labels is only offered then.
    pub exact: bool,
}

/// The palette's copy rows that apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyRows {
    /// Whether a thread is open, which offers Copy Conversation Contents.
    pub thread: bool,
    /// Whether a message in it has focus, which offers Copy Message Contents and Copy Recipients.
    pub message: bool,
    /// Whether the thread has a subject, which offers Copy Subject (in the list only once
    /// something is typed).
    pub subject: bool,
    /// Whether the focused message has files besides its inline images, which offers Copy
    /// Attachments.
    pub attachments: bool,
}

/// Registers the list's commands for this frame.
pub fn register(commands: &mut Commands<Action>, cx: &Context<'_>) {
    register_specs(commands, NAVIGATION_SPECS);
    if !cx.threads.is_empty() {
        conversation(commands, cx);
    }
    register_specs(commands, SELECTION_SPECS);
    go_to(commands, cx);
}

const NAVIGATION_SPECS: &[CommandSpec] = &[
    sc_rep(
        "NEXT",
        &["j", "down", "n", "ctralt+j", "ctralt+n"],
        THREAD_LIST,
        Action::Move(1),
        Action::Move(1),
    ),
    // K and P stay in the list. ↑ leaves it from the first row.
    sc_rep(
        "PREVIOUS",
        &["k", "p", "ctralt+k", "ctralt+p"],
        THREAD_LIST,
        Action::Move(-1),
        Action::Move(-1),
    ),
    sc_rep("PREVIOUS_UP", &["up"], THREAD_LIST, Action::Up, Action::Up),
    sc(
        "HEADER_DOWN",
        &["down", "j", "ctralt+j", "esc"],
        LIST_HDR,
        Action::Header(HeaderKey::Down),
    ),
    sc(
        "HEADER_ENTER",
        &["enter"],
        LIST_HDR,
        Action::Header(HeaderKey::Enter),
    ),
    sc(
        "HEADER_BACKSPACE",
        &["backspace"],
        LIST_HDR,
        Action::Header(HeaderKey::Backspace),
    ),
    sc_rep(
        "HEADER_LEFT",
        &["left"],
        LIST_HDR,
        Action::Header(HeaderKey::Move(-1)),
        Action::Header(HeaderKey::Move(-1)),
    ),
    sc_rep(
        "HEADER_RIGHT",
        &["right"],
        LIST_HDR,
        Action::Header(HeaderKey::Move(1)),
        Action::Header(HeaderKey::Move(1)),
    ),
    sc("OPEN", &["enter", "o"], THREAD_LIST, Action::Open),
    sc("COPY_LISTED", &["mod+c"], THREAD_LIST, Action::CopyListed),
    sc(
        "JUMP_TO_TOP",
        &["mod+up", "home", "g g"],
        THREAD_LIST,
        Action::Top,
    ),
    sc(
        "JUMP_TO_BOTTOM",
        &["mod+down", "end", "shift+g"],
        THREAD_LIST,
        Action::Bottom,
    ),
    sc_rep(
        "PAGE_DOWN",
        &["space", "pagedown"],
        THREAD_LIST,
        Action::Page(1),
        Action::Page(1),
    ),
    sc_rep(
        "PAGE_UP",
        &["shift+space", "pageup"],
        THREAD_LIST,
        Action::Page(-1),
        Action::Page(-1),
    ),
    sc("BACK", &["esc"], THREAD_LIST, Action::Back { escape: true }),
    sc(
        "BACK_BACKSPACE",
        &["backspace"],
        THREAD_LIST,
        Action::Back { escape: false },
    ),
    sc(
        "TOGGLE_LABEL_SWITCHER",
        &["left"],
        THREAD_LIST,
        Action::ToggleFolders,
    ),
];

const FOLDER_SPECS: &[CommandSpec] = &[
    // Held, ↑ stops at the first folder like K does.
    sc_rep(
        "UP",
        &["up"],
        LABEL_SW,
        Action::Folders(FolderKey::Up),
        Action::Folders(FolderKey::UpWithin),
    ),
    sc_rep(
        "UP_WITHIN",
        &["k", "ctralt+k", "p", "ctralt+p"],
        LABEL_SW,
        Action::Folders(FolderKey::UpWithin),
        Action::Folders(FolderKey::UpWithin),
    ),
    sc_rep(
        "DOWN",
        &["down", "j", "ctralt+j", "n", "ctralt+n"],
        LABEL_SW,
        Action::Folders(FolderKey::Down),
        Action::Folders(FolderKey::Down),
    ),
    sc(
        "OPEN",
        &["enter"],
        LABEL_SW,
        Action::Folders(FolderKey::Enter),
    ),
    sc(
        "CLOSE",
        &["esc", "mod+right"],
        LABEL_SW,
        Action::Folders(FolderKey::Close),
    ),
    sc(
        "RIGHT",
        &["right"],
        LABEL_SW,
        Action::Folders(FolderKey::Right),
    ),
    sc(
        "BOTTOM",
        &["mod+down", "end", "shift+j"],
        LABEL_SW,
        Action::Folders(FolderKey::Bottom),
    ),
    sc(
        "TOP",
        &["mod+up", "home", "shift+k"],
        LABEL_SW,
        Action::Folders(FolderKey::Top),
    ),
    sc_rep(
        "PAGE_DOWN",
        &["pagedown", "space"],
        LABEL_SW,
        Action::Folders(FolderKey::PageDown),
        Action::Folders(FolderKey::PageDown),
    ),
    sc_rep(
        "PAGE_UP",
        &["pageup", "shift+space"],
        LABEL_SW,
        Action::Folders(FolderKey::PageUp),
        Action::Folders(FolderKey::PageUp),
    ),
];

/// Registers the folder drawer's keys for this frame, while it's open.
pub fn register_folders(commands: &mut Commands<Action>) {
    register_specs(commands, FOLDER_SPECS);
}

const SELECTION_SPECS: &[CommandSpec] = &[
    sc("SELECT", &["x"], THREAD_LIST, Action::ToggleSelected),
    sc_rep(
        "EXTEND_DOWN",
        &["shift+j", "shift+down"],
        THREAD_LIST,
        Action::Extend(1),
        Action::Extend(1),
    ),
    sc_rep(
        "EXTEND_UP",
        &["shift+k", "shift+up"],
        THREAD_LIST,
        Action::Extend(-1),
        Action::Extend(-1),
    ),
    sc(
        "DESELECT_ALL",
        &["mod+alt+a"],
        THREAD_LIST,
        Action::DeselectAll,
    ),
    row(
        "SELECT_ALL_FROM_HERE",
        "Select All From Here",
        Action::SelectAllFromHere,
    )
    .keys(&["mod+a"])
    .key_code(&["⌘", "A"])
    .icon("data/select-all")
    .multiple()
    .regions(THREAD_LIST),
    row("SELECT_ALL", "Select All", Action::SelectAll)
        .keys(&["mod+shift+a"])
        .key_code(&["⌘", "shift", "A"])
        .follow("SELECT_ALL_FROM_HERE")
        .icon("data/select-all")
        .multiple()
        .regions(THREAD_LIST),
];

const STAR_PAIR: [CommandSpec; 2] = [
    conv(
        "STAR",
        "Star",
        "data/star",
        9.0,
        Action::Apply(Mutation::Star),
    )
    .keys(&["s"])
    .aliases(&["Flag"]),
    conv(
        "STAR",
        "Unstar",
        "data/star",
        9.0,
        Action::Apply(Mutation::Unstar),
    )
    .keys(&["s"])
    .aliases(&["Remove Flag"]),
];
const TRASH_PAIR: [CommandSpec; 2] = [
    conv(
        "TRASH",
        "Trash",
        "data/discard",
        6.0,
        Action::Apply(Mutation::Trash),
    )
    .keys(&["#", "del"])
    .aliases(&["Delete"]),
    conv(
        "TRASH",
        "Remove from Trash",
        "data/discard",
        6.0,
        Action::Apply(Mutation::Untrash),
    )
    .keys(&["#", "del"]),
];
/// On UK keyboards ⇧3 types `£`, and `#` is the key by Enter, or ⌥3 on a Mac.
const TRASH_UK_PAIR: [CommandSpec; 2] = [
    sc(
        "TRASH_UK",
        &["\\", "alt+3"],
        CONVERSATION,
        Action::Apply(Mutation::Trash),
    )
    .when_typing("#"),
    sc(
        "TRASH_UK",
        &["\\", "alt+3"],
        CONVERSATION,
        Action::Apply(Mutation::Untrash),
    )
    .when_typing("#"),
];
/// For a single sender, `!` opens the Mark Spam picker, which offers blocking. Otherwise it
/// reports spam directly (`crate::pickers`).
const SPAM_PAIR: [CommandSpec; 2] = [
    conv(
        "MARK_SPAM",
        "Mark Spam",
        "data/spam",
        5.0,
        Action::Pick(Pick::MarkSpam),
    )
    .keys(&["!"])
    .aliases(&["Mark Junk"]),
    conv(
        "MARK_SPAM",
        "Mark not Spam",
        "data/spam",
        5.0,
        Action::Apply(Mutation::NotSpam),
    )
    .keys(&["!"])
    .aliases(&["Mark not Junk"]),
];
const UNREAD_PAIR: [CommandSpec; 2] = [
    conv(
        "MARK_UNREAD",
        "Mark Unread",
        "data/mark-unread",
        4.5,
        Action::Apply(Mutation::MarkUnread),
    )
    .keys(&["u"])
    .aliases(&["Mark as Unread", "Toggle Read"]),
    conv(
        "MARK_UNREAD",
        "Mark Read",
        "data/mark-unread",
        4.0,
        Action::Apply(Mutation::MarkRead),
    )
    .keys(&["u"])
    .aliases(&["Mark as Read", "Toggle Read"]),
];
const MUTE_PAIR: [CommandSpec; 2] = [
    conv(
        "MUTE",
        "Mute",
        "data/mute",
        4.0,
        Action::Apply(Mutation::Mute),
    )
    .keys(&["shift+m"])
    .aliases(&["Ignore", "Unfollow"]),
    conv(
        "MUTE",
        "Unmute",
        "data/mute",
        4.0,
        Action::Apply(Mutation::Unmute),
    )
    .keys(&["shift+m"])
    .aliases(&["Stop Ignoring", "Follow"]),
];
const MUTE_INTENT_SPEC: CommandSpec = sc("MUTE_INTENT", &["m"], CONVERSATION, Action::MuteHint);
const DONE_KEY_SPECS: &[CommandSpec] = &[
    sc(
        "MARK_DONE_KEY",
        &["e"],
        CONVERSATION,
        Action::Apply(Mutation::MarkDone),
    ),
    sc(
        "MARK_NOT_DONE_KEY",
        &["shift+e"],
        CONVERSATION,
        Action::Apply(Mutation::MarkNotDone),
    ),
];
const DONE_PAIR: [CommandSpec; 2] = [
    conv(
        "MARK_DONE",
        "Mark Done",
        "data/done",
        11.0,
        Action::Apply(Mutation::MarkDone),
    )
    .key_code(&["E"])
    .aliases(&["Archive"]),
    conv(
        "MARK_DONE",
        "Mark not Done",
        "data/done",
        11.0,
        Action::Apply(Mutation::MarkNotDone),
    )
    .key_code(&["shift", "E"])
    .aliases(&["Unarchive", "Move to Inbox"]),
];

/// The actions on the actionable threads, in the palette's order (provider defaults, label
/// commands, then Mark Done).
fn conversation(commands: &mut Commands<Action>, cx: &Context<'_>) {
    let all = |test: fn(&ThreadSummary) -> bool| cx.threads.iter().all(|thread| test(thread));
    let trashed = usize::from(all(|thread| thread.has_label("TRASH")));
    let spam = usize::from(all(|thread| thread.has_label("SPAM")) || *cx.list == ListId::Spam);

    commands.add(STAR_PAIR[usize::from(all(|thread| thread.starred))].build());
    commands.add(TRASH_PAIR[trashed].build());
    commands.add(TRASH_UK_PAIR[trashed].build());
    commands.add(SPAM_PAIR[spam].build());
    commands.add(UNREAD_PAIR[usize::from(all(|thread| thread.unread))].build());
    commands.add(MUTE_PAIR[usize::from(all(|thread| thread.muted))].build());
    commands.add(MUTE_INTENT_SPEC.build());

    unsubscribe(commands, cx);
    block(commands, cx);
    important_or_other(commands, cx);
    copy(commands, cx.copy);
    labels(commands, cx.exact);
    reminders(commands, cx);

    // `e` marks done and `⇧E` not done anywhere but Spam. The palette row, which toggles, isn't
    // offered for a draft that isn't in a conversation yet, since that draft stays in the inbox.
    if *cx.list == ListId::Spam {
        return;
    }
    register_specs(commands, DONE_KEY_SPECS);
    if !cx.threads.iter().any(|thread| thread.draft_only) {
        commands.add(DONE_PAIR[usize::from(all(|thread| thread.done))].build());
    }
}

const UNSUBSCRIBE_SPEC: CommandSpec = row(
    "UNSUBSCRIBE",
    "Unsubscribe",
    Action::Unsubscribe(crate::unsubscribe::Action::Launch),
)
.keys(&["mod+u"])
.key_code(&["⌘", "U"])
.icon("data/unsubscribe")
.regions(CONVERSATION);

/// `Unsubscribe` (`⌘U`) for one conversation, in the list and in a thread. A Gmail account always
/// has it, and with nothing to unsubscribe from, it offers to block the sender.
fn unsubscribe(commands: &mut Commands<Action>, cx: &Context<'_>) {
    if let [thread] = cx.threads
        && !thread.draft_only
        && cx.exact
    {
        commands.add(UNSUBSCRIBE_SPEC.build());
    }
}

/// `Block Sender: {email}` and, unless it's a webmail domain or yours, `Block Domain: {domain}`
/// for one conversation with a sender.
fn block(commands: &mut Commands<Action>, cx: &Context<'_>) {
    let Some(sender) = cx.sender else { return };
    let block_row = |id: &'static str, title: String, from: &str, domain: bool| {
        let action = Action::Pick(Pick::Block {
            from: from.to_owned(),
            domain,
            spam: false,
        });
        Command::new(id, title, action)
            .icon("data/unsubscribe")
            .operative_word("block")
            .default_score(0.98)
            .regions(CONVERSATION)
    };
    commands.add(block_row(
        "BLOCK_SENDER",
        format!("Block Sender: {}", sender.email),
        &sender.email,
        false,
    ));
    if let Some(domain) = sender.domain.as_ref().filter(|domain| {
        !crate::format::names::is_popular_domain(domain) && !cx.own_domains.contains(domain)
    }) {
        commands.add(block_row(
            "BLOCK_DOMAIN",
            format!("Block Domain: {domain}"),
            domain,
            true,
        ));
    }
}

const MOVE_TO_IMPORTANT_SPEC: CommandSpec = row(
    "MOVE_TO_IMPORTANT",
    "Move to Important",
    Action::Pick(Pick::OpenImportantOrOther(false)),
)
.icon("data/move")
.aliases(&["Mark as Important"])
.operative_word("move")
.follow("MOVE")
.regions(CONVERSATION);
const MOVE_TO_OTHER_SPEC: CommandSpec = row(
    "MOVE_TO_OTHER",
    "Move to Other",
    Action::Pick(Pick::OpenImportantOrOther(true)),
)
.icon("data/move")
.aliases(&["Mark as not Important"])
.operative_word("move")
.follow("MOVE")
.regions(CONVERSATION);

/// `Move to Important` and `Move to Other` for one conversation while Important and Other are
/// on, each except where the list on screen is that one.
fn important_or_other(commands: &mut Commands<Action>, cx: &Context<'_>) {
    let [thread] = cx.threads else { return };
    if cx.unified || thread.draft_only {
        return;
    }
    if *cx.list != ListId::Important {
        commands.add(MOVE_TO_IMPORTANT_SPEC.build());
    }
    if *cx.list != ListId::Other {
        commands.add(MOVE_TO_OTHER_SPEC.build());
    }
}

const MOVE_SPEC: CommandSpec = conv(
    "MOVE",
    "Move",
    "data/move",
    8.0,
    Action::Pick(Pick::OpenMove),
)
.keys(&["v"])
.key_code(&["V"])
.aliases(&["Folder", "File"])
.operative_word("move");
const REMOVE_ALL_PAIR: [CommandSpec; 2] = [
    sc(
        "REMOVE_ALL_LABELS",
        &["shift+y"],
        CONVERSATION,
        Action::Pick(Pick::RemoveAll),
    ),
    conv(
        "REMOVE_ALL_LABELS",
        "Remove all Labels",
        "data/label",
        6.9,
        Action::Pick(Pick::RemoveAll),
    )
    .keys(&["shift+y"])
    .key_code(&["shift", "Y"])
    .aliases(&[
        "Remove all Labels and Mark Done",
        "Remove all Labels and Archive",
    ]),
];
const LABEL_TAIL_SPECS: &[CommandSpec] = &[
    conv(
        "LABEL",
        "Label",
        "data/label",
        7.0,
        Action::Pick(Pick::OpenLabel),
    )
    .keys(&["l"])
    .key_code(&["L"])
    .follow("CREATE_EVENT")
    .aliases(&[
        "Add Label",
        "Remove Label",
        "Create Label",
        "New Label",
        "Tag",
        "Create Tag",
        "Remove Tag",
    ]),
    conv(
        "REMOVE_FROM_LABEL",
        "Remove from Label",
        "data/label",
        7.0,
        Action::Pick(Pick::RemoveFromList(0)),
    )
    .keys(&["y"])
    .key_code(&["Y"])
    .aliases(&["Remove from List"]),
    // `]` is -1 and `[` is 1: the previous and the next conversation.
    sc(
        "REMOVE_FROM_LABEL_PREVIOUS",
        &["]"],
        CONVERSATION,
        Action::Pick(Pick::RemoveFromList(-1)),
    ),
    sc(
        "REMOVE_FROM_LABEL_NEXT",
        &["["],
        CONVERSATION,
        Action::Pick(Pick::RemoveFromList(1)),
    ),
];

/// Move, then the label commands, and the keys that take threads out of the list on screen.
/// Without an exact selection, `⇧Y` only says why it can't.
fn labels(commands: &mut Commands<Action>, exact: bool) {
    commands.add(MOVE_SPEC.build());
    commands.add(REMOVE_ALL_PAIR[usize::from(exact)].build());
    register_specs(commands, LABEL_TAIL_SPECS);
}

const SNOOZE_OPEN: Action = Action::Remind(Remind::Open { button: false });
const SNOOZE_SHIFT_SPEC: CommandSpec = sc(
    "SNOOZE_SHIFT",
    &["mod+shift+h"],
    SHIFT_H_REGIONS,
    SNOOZE_OPEN,
);
const SNOOZE_SHORTCUT_SPEC: CommandSpec = sc("SNOOZE", &["h"], CONVERSATION, SNOOZE_OPEN);
const SNOOZE_ROW_SPEC: CommandSpec = conv("SNOOZE", "Remind Me", "data/snooze", 10.0, SNOOZE_OPEN)
    .keys(&["h"])
    .key_code(&["H"])
    .scale(1.01)
    .aliases(&["Snooze", "Boomerang", "Add Reminder"]);
const REMOVE_REMINDER_PAIR: [CommandSpec; 2] = [
    conv(
        "REMOVE_REMINDER",
        "Remove Reminder",
        "data/snooze",
        6.5,
        Action::Remind(Remind::Remove {
            threads: None,
            to_inbox: true,
        }),
    )
    .scale(1.01),
    conv(
        "REMOVE_REMINDER",
        "Remove Reminders",
        "data/snooze",
        6.5,
        Action::Remind(Remind::Remove {
            threads: None,
            to_inbox: true,
        }),
    )
    .scale(1.01),
];

/// `Remind Me` and, for threads with one, `Remove Reminder(s)`. They're not for drafts being
/// written, and only for an exact selection. Without one, `H` says why it can't.
fn reminders(commands: &mut Commands<Action>, cx: &Context<'_>) {
    if cx.threads.iter().any(|thread| thread.draft_only) {
        return;
    }
    commands.add(SNOOZE_SHIFT_SPEC.build());
    if !cx.exact {
        commands.add(SNOOZE_SHORTCUT_SPEC.build());
        return;
    }
    commands.add(SNOOZE_ROW_SPEC.build());
    let reminded = cx
        .threads
        .iter()
        .filter(|thread| thread.has_reminder())
        .count();
    if reminded > 0 {
        commands.add(REMOVE_REMINDER_PAIR[usize::from(reminded > 1)].build());
    }
}

struct CopySpec {
    enabled: fn(CopyRows) -> bool,
    query_only_in_list: bool,
    spec: CommandSpec,
}

const COPY_SPECS: &[CopySpec] = &[
    // Both copy the message, and pasting it (⌘V) makes the new draft.
    CopySpec {
        enabled: |r| r.message,
        query_only_in_list: false,
        spec: row(
            "COPY_MESSAGE_TO_DRAFT",
            "Copy Message to new Draft",
            Action::CopyMessage,
        )
        .icon("data/copy")
        .score(4.3)
        .regions(CONVERSATION)
        .key_code(&["⌘", "C"]),
    },
    CopySpec {
        enabled: |r| r.message,
        query_only_in_list: false,
        spec: row("COPY_MESSAGE", "Copy Message Contents", Action::CopyMessage)
            .icon("data/copy")
            .score(4.2)
            .regions(CONVERSATION)
            .key_code(&["⌘", "C"]),
    },
    CopySpec {
        enabled: |r| r.thread,
        query_only_in_list: false,
        spec: row(
            "COPY_CONVERSATION",
            "Copy Conversation Contents",
            Action::CopyConversation,
        )
        .icon("data/copy")
        .score(4.1)
        .regions(CONVERSATION)
        .key_code(&["⌘", "C"]),
    },
    CopySpec {
        enabled: |r| r.message,
        query_only_in_list: false,
        spec: row("COPY_RECIPIENTS", "Copy Recipients", Action::CopyRecipients)
            .icon("data/copy")
            .score(4.05)
            .regions(CONVERSATION),
    },
    CopySpec {
        enabled: |r| r.attachments,
        query_only_in_list: false,
        spec: row(
            "COPY_ATTACHMENTS",
            "Copy Attachments",
            Action::CopyAttachments,
        )
        .icon("data/copy")
        .score(4.005)
        .regions(CONVERSATION),
    },
    CopySpec {
        enabled: |r| r.subject,
        query_only_in_list: true,
        spec: row("COPY_SUBJECT", "Copy Subject", Action::CopySubject)
            .icon("data/copy")
            .score(4.01)
            .regions(CONVERSATION),
    },
];

/// The copy rows, for one conversation at a time.
fn copy(commands: &mut Commands<Action>, rows: CopyRows) {
    for entry in COPY_SPECS {
        if (entry.enabled)(rows) {
            let command = entry.spec.build();
            commands.add(if entry.query_only_in_list && !rows.thread {
                command.query_only()
            } else {
                command
            });
        }
    }
}

/// `go to …` rows for every view and label, and the `g` sequences for system views.
fn go_to(commands: &mut Commands<Action>, cx: &Context<'_>) {
    let highlighted = !cx.threads.is_empty();
    let go = |go: &GoTo| {
        let mut command = Command::new(
            go.id,
            format!("go to {}", go.name),
            Action::Go(go.list.clone()),
        )
        .keys(go.keys.iter().copied())
        .key_code(go.key_code.iter().copied())
        .operative_word(go.operative_word)
        .default_score(1.0)
        .scale(0.6)
        .icon("data/go-to")
        .multiple()
        .regions(GO_TO);
        if let Some(follow) = go.follow {
            command = command.follow(follow);
        }
        // With a conversation highlighted, an empty palette lists its actions instead.
        if highlighted {
            command.query_only()
        } else {
            command
        }
    };
    for system in lists::system_go_tos(cx.unified) {
        commands.add(go(&system));
        if !system.keys.is_empty() {
            commands.add(from_week(system.id, system.keys, Action::Go(system.list)));
        }
    }
    for label in lists::listed_labels(cx.labels) {
        let command = Command::new(
            lists::slug(&label.name),
            format!("go to {}", label.name),
            Action::Go(ListId::Label(label.id.clone())),
        )
        .operative_word(label.name.clone())
        .default_score(1.0)
        .scale(0.5)
        .icon("data/go-to")
        .multiple()
        .regions(GO_TO);
        commands.add(if highlighted {
            command.query_only()
        } else {
            command
        });
    }
    for late in lists::late_go_tos() {
        commands.add(go(&late));
        if !late.keys.is_empty() {
            commands.add(from_week(late.id, late.keys, Action::Go(late.list)));
        }
    }
    // `g i` goes home and `g o` to Other. With the unified inbox, both go to the Inbox.
    commands.add(
        Command::shortcut("GO_HOME", Action::Go(cx.home.clone()))
            .key("g i")
            .regions(GO_TO),
    );
    commands.add(
        Command::shortcut("GO_TO_OTHER", Action::Go(cx.other.clone()))
            .key("g o")
            .regions(GO_TO),
    );
    commands.add(
        Command::shortcut("GO_TO_LABEL", Action::GoToLabel)
            .keys(["g l", "g f"])
            .regions(GO_TO),
    );
    commands.add(from_week("GO_HOME", &["g i"], Action::Go(cx.home.clone())));
    commands.add(from_week(
        "GO_TO_OTHER",
        &["g o"],
        Action::Go(cx.other.clone()),
    ));
    commands.add(from_week("GO_TO_LABEL", &["g l", "g f"], Action::GoToLabel));
}

/// The week view's twin of `g` sequence `id`: `keys` there close the week, then do `action`.
pub(crate) fn from_week(id: &str, keys: &[&'static str], action: Action) -> Command<Action> {
    Command::shortcut(
        format!("{id}_FROM_WEEK"),
        Action::FromWeek(Box::new(action)),
    )
    .keys(keys.iter().copied())
    .regions(WEEK_GO_TO)
}

#[cfg(test)]
mod tests {
    use exo_store::Attachments;
    use jiff::Timestamp;

    use super::*;

    fn sample_thread() -> ThreadSummary {
        ThreadSummary {
            id: "t1".into(),
            participants: Vec::new(),
            recipients: false,
            drafts: 0,
            draft_only: false,
            subject: "Quarterly Review".into(),
            snippet: String::new(),
            labels: Vec::new(),
            label_ids: Vec::new(),
            received: Timestamp::UNIX_EPOCH,
            unread: false,
            starred: false,
            done: false,
            muted: false,
            attachment: Attachments::None,
            contact: None,
            removed_by: Vec::new(),
            messages: 1,
            search: None,
            failure: None,
            sending: false,
            scheduled: None,
            reminder: None,
            files: Vec::new(),
        }
    }

    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("system fonts load");
        ctx
    }

    fn dispatch_key(
        ctx: &egui::Context,
        commands: &mut Commands<Action>,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Vec<Action> {
        let page = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut primed = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = commands.show(ui.ctx(), page);
        });
        primed.textures_delta.clear();

        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
        raw.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut actions = Vec::new();
        let mut out = ctx.run_ui(raw, |ui| {
            actions = commands.dispatch(ui.ctx());
        });
        out.textures_delta.clear();
        actions
    }

    #[test]
    fn list_and_conversation_tables_preserve_shortcuts_toggles_and_spam_gating() {
        let ctx = test_ctx();
        let mut thread = sample_thread();

        let run_in_list = |list: &ListId, thread: &ThreadSummary, key, modifiers| {
            let mut commands = Commands::new();
            commands.set_region(Region::ThreadList);
            let threads = [thread];
            register(
                &mut commands,
                &Context {
                    list,
                    threads: &threads,
                    labels: &[],
                    copy: CopyRows::default(),
                    home: ListId::Important,
                    other: ListId::Other,
                    unified: false,
                    sender: None,
                    own_domains: &[],
                    exact: true,
                },
            );
            dispatch_key(&ctx, &mut commands, key, modifiers)
        };

        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::J,
                egui::Modifiers::NONE
            ),
            vec![Action::Move(1)]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::S,
                egui::Modifiers::NONE
            ),
            vec![Action::Apply(Mutation::Star)]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::E,
                egui::Modifiers::NONE
            ),
            vec![Action::Apply(Mutation::MarkDone)]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::M,
                egui::Modifiers::NONE
            ),
            vec![Action::MuteHint]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::M,
                egui::Modifiers::SHIFT
            ),
            vec![Action::Apply(Mutation::Mute)]
        );

        thread.starred = true;
        thread.unread = true;
        thread.muted = true;
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::S,
                egui::Modifiers::NONE
            ),
            vec![Action::Apply(Mutation::Unstar)]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::U,
                egui::Modifiers::NONE
            ),
            vec![Action::Apply(Mutation::MarkRead)]
        );
        assert_eq!(
            run_in_list(
                &ListId::Important,
                &thread,
                egui::Key::M,
                egui::Modifiers::SHIFT
            ),
            vec![Action::Apply(Mutation::Unmute)]
        );

        assert!(
            run_in_list(&ListId::Spam, &thread, egui::Key::E, egui::Modifiers::NONE).is_empty(),
            "Spam list must not bind Mark Done"
        );
    }

    #[test]
    fn thread_and_folder_tables_dispatch_expected_actions_and_palette_ranking() {
        let ctx = test_ctx();
        let page = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let mut thread_commands = Commands::new();
        thread_commands.set_region(Region::ThreadMessages);
        register_thread(
            &mut thread_commands,
            &ThreadContext {
                focus_collapsed: true,
                new_messages_focused: false,
                has_selection: false,
                show_images: true,
            },
        );
        assert_eq!(
            dispatch_key(
                &ctx,
                &mut thread_commands,
                egui::Key::Enter,
                egui::Modifiers::NONE
            ),
            vec![Action::ExpandMessage]
        );

        let mut folder_commands = Commands::new();
        folder_commands.set_region(Region::LabelSwitcher);
        register_folders(&mut folder_commands);
        assert_eq!(
            dispatch_key(
                &ctx,
                &mut folder_commands,
                egui::Key::ArrowUp,
                egui::Modifiers::NONE
            ),
            vec![Action::Folders(crate::folders::Key::Up)]
        );

        let thread = sample_thread();
        let threads = [&thread];
        let mut palette_commands = Commands::new();
        palette_commands.set_region(Region::ThreadList);
        palette_commands.open_palette(Some("Snooze"));
        register(
            &mut palette_commands,
            &Context {
                list: &ListId::Important,
                threads: &threads,
                labels: &[],
                copy: CopyRows::default(),
                home: ListId::Important,
                other: ListId::Other,
                unified: false,
                sender: None,
                own_domains: &[],
                exact: true,
            },
        );
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut chosen = Vec::new();
        let mut out = ctx.run_ui(raw, |ui| {
            chosen = palette_commands.show(ui.ctx(), page);
        });
        out.textures_delta.clear();
        assert_eq!(
            chosen,
            vec![Action::Remind(crate::remind::Action::Open {
                button: false
            })]
        );
    }
}
