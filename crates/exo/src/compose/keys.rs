//! Compose command and keyboard shortcut registration (`Compose::register`,
//! [`register_shortcuts`], [`register_compose_to`], and focused-form / inline-picker bindings).

use exo_commands::{Command, Commands, Region};
use exo_editor::FormatCommand;
use exo_store::{Address, Alias, DraftAction};

use super::{Action, AlertThen, Compose, Field, Open, assistant, chips, form, intro};
use crate::commands;

/// Where compose's shortcuts start a draft from.
pub(crate) struct Shortcuts {
    /// A thread is open (rather than the list).
    pub in_thread: bool,
    /// The focused message is collapsed (Enter expands it rather than replying).
    pub focus_collapsed: bool,
    /// There's a conversation to reply to (a highlighted or open one, not a draft-only one).
    pub can_reply: bool,
    /// Several conversations are selected (the list's reply keys don't apply).
    pub selecting: bool,
}

impl Compose {
    /// Registers compose's commands for this frame.
    pub fn register(
        &self,
        commands: &mut Commands<commands::Action>,
        aliases: &[Alias],
        team: &[String],
    ) {
        if self.pane.is_some() && self.focus.is_none() && self.alert.is_none() {
            // In the new-message pane without the keyboard, Escape leaves the pane.
            commands.add(
                Command::shortcut("BACK", commands::Action::Compose(Action::LeavePane))
                    .keys(["esc", "backspace"])
                    .regions(&[Region::ThreadMessages]),
            );
            commands.add(
                Command::shortcut(
                    "FOCUS_DRAFT",
                    commands::Action::Compose(Action::Focus(Field::Body)),
                )
                .keys(["enter", "tab"])
                .regions(&[Region::ThreadMessages]),
            );
        }
        if let Some(alert) = &self.alert {
            const ALERT: &[Region] = &[Region::Alert];
            let act = |action: Action| commands::Action::Compose(action);
            crate::notice::register_alert_keys(
                commands,
                act(Action::Alert(alert.selected)),
                act(Action::Alert(alert.escape)),
                |step| act(Action::AlertSelect(step)),
            );
            // `h` in the follow-up hint is `Set reminder`.
            if matches!(alert.then, AlertThen::FollowUp) {
                commands.add(
                    Command::shortcut("FOLLOW_UP_H", act(Action::Alert(0)))
                        .key("h")
                        .regions(ALERT),
                );
            }
            // `z` in the Undo Send alert undoes the send.
            if let AlertThen::UndoSend(tag) = alert.then {
                commands.add(
                    Command::shortcut("UNDO_SEND_Z", act(Action::UndoSend(tag)))
                        .key("z")
                        .regions(ALERT),
                );
            }
            return;
        }
        if self.popped.is_some() {
            // ⌘D cycles focus between the app and the pop-out, wherever either has the keyboard.
            commands.add(
                Command::shortcut(
                    "CYCLE_APP_FOCUS",
                    commands::Action::Compose(Action::CycleFocus),
                )
                .key("mod+d")
                .regions(&[
                    Region::ThreadList,
                    Region::ThreadMessages,
                    Region::ComposeForm,
                    Region::RecipientList,
                ]),
            );
        }
        if let Some(open) = self.focused() {
            let in_pane = self.pane() == Some(open.id.as_str());
            register_form(
                commands,
                open,
                aliases,
                self.popped.as_deref() == Some(open.id.as_str()),
                self.send_and_done,
                in_pane,
                self.backtick_escape,
            );
            if !open.is_snippet() {
                intro::register(commands);
            }
            assistant::register_use_emoji(commands);
            assistant::register_removals(commands, open, team);
            if self.missing_originals > 0 {
                let (title, alias) = if self.missing_originals == 1 {
                    ("Include Original Attachment", "Keep Attachment")
                } else {
                    ("Include Original Attachments", "Keep Attachments")
                };
                commands.add(
                    Command::new(
                        "INCLUDE_ORIGINAL_ATTACHMENTS",
                        title,
                        commands::Action::Compose(Action::IncludeOriginals),
                    )
                    .aliases([alias])
                    .default_score(1.79)
                    .regions(&[Region::ComposeForm, Region::RecipientList]),
                );
            }
        }
    }
}

/// Registers the shortcuts that start drafts: `C` and the palette's `Compose` everywhere, `R`,
/// `A`, `F` on a conversation, Enter on an expanded message, and `⌘⇧O/C/B/F/S` in a thread.
pub(crate) fn register_shortcuts(commands: &mut Commands<commands::Action>, cx: &Shortcuts) {
    let act = commands::Action::Compose;
    commands.add(
        Command::new("COMPOSE", "Compose", act(Action::New))
            .key("c")
            .key_code(["C"])
            .aliases(["New Email", "New Draft", "Write Email", "Send Email"])
            .default_score(1.75)
            .scale(1.2)
            .regions(exo_commands::LIST_AND_THREAD),
    );
    commands.add(
        Command::new("NOTE_TO_SELF", "Note to self", act(Action::NoteToSelf))
            .operative_word("note")
            .regions(exo_commands::LIST_AND_THREAD),
    );
    commands.add(
        Command::new(
            "POP_OUT_COMPOSE",
            "Pop Out Compose",
            act(Action::NewPoppedOut),
        )
        .key("shift+c")
        .key_code(["shift", "C"])
        .aliases(["Pop-out", "Popout"])
        .regions(exo_commands::LIST_AND_THREAD),
    );
    if !cx.can_reply || cx.selecting {
        return;
    }
    const CONVERSATION: &[Region] = exo_commands::CONVERSATION;
    commands.add(
        Command::new(
            "REPLY_ALL",
            "Reply All",
            act(Action::Reply(DraftAction::ReplyAll)),
        )
        .key("a")
        .key_code(["enter"])
        .default_score(3.0)
        .regions(CONVERSATION),
    );
    commands.add(
        Command::new("REPLY", "Reply", act(Action::Reply(DraftAction::Reply)))
            .key("r")
            .key_code(["R"])
            .default_score(2.0)
            .regions(CONVERSATION),
    );
    commands.add(
        Command::new(
            "FORWARD",
            "Forward",
            act(Action::Reply(DraftAction::Forward)),
        )
        .key("f")
        .key_code(["F"])
        .default_score(1.9)
        .regions(CONVERSATION),
    );
    let popped =
        |id: &'static str, title: &'static str, action: DraftAction, aliases: [&'static str; 2]| {
            Command::new(id, title, act(Action::ReplyPoppedOut(action)))
                .aliases(aliases)
                .default_score(1.8)
                .regions(CONVERSATION)
        };
    commands.add(
        popped(
            "REPLY_POP_OUT",
            "Reply, Pop Out Draft",
            DraftAction::Reply,
            ["Reply and Pop-out", "Reply and Popout"],
        )
        .key("shift+r")
        .key_code(["shift", "R"]),
    );
    commands.add(
        popped(
            "REPLY_ALL_POP_OUT",
            "Reply All, Pop Out Draft",
            DraftAction::ReplyAll,
            ["Reply All and Pop-out", "Reply All and Popout"],
        )
        .keys(["shift+enter", "shift+a"])
        .key_code(["shift", "enter"]),
    );
    commands.add(
        popped(
            "FORWARD_POP_OUT",
            "Forward, Pop Out Draft",
            DraftAction::Forward,
            ["Forward and Pop-out", "Forward and Popout"],
        )
        .key("shift+f")
        .key_code(["shift", "F"]),
    );
    intro::register_on_conversation(commands, cx.in_thread);
    if !cx.in_thread {
        return;
    }
    const MESSAGES: &[Region] = &[Region::ThreadMessages];
    if !cx.focus_collapsed {
        commands.add(
            Command::shortcut("REPLY_ALL_ENTER", act(Action::Reply(DraftAction::ReplyAll)))
                .key("enter")
                .regions(MESSAGES),
        );
    }
    let with = |id: &'static str,
                title: &'static str,
                field: Field,
                key: &'static str,
                code: &'static str| {
        Command::new(id, title, act(Action::ReplyAllWith(field)))
            .key(key)
            .key_code(["⌘", "shift", code])
            .default_score(1.8)
            .regions(MESSAGES)
    };
    commands.add(with(
        "REPLY_ALL_CHANGE_TO",
        "Reply All, Change To",
        Field::To,
        "mod+shift+o",
        "O",
    ));
    commands.add(
        with(
            "REPLY_ALL_CHANGE_CC",
            "Reply All, Change CC",
            Field::Cc,
            "mod+shift+c",
            "C",
        )
        .aliases(["Reply All, Add CC"]),
    );
    commands.add(
        with(
            "REPLY_ALL_CHANGE_BCC",
            "Reply All, Change BCC",
            Field::Bcc,
            "mod+shift+b",
            "B",
        )
        .aliases(["Reply All, Add BCC"]),
    );
    commands.add(
        with(
            "REPLY_ALL_CHANGE_FROM",
            "Reply All, Change From",
            Field::From,
            "mod+shift+f",
            "F",
        )
        .aliases(["Change Alias", "Change Aliases"]),
    );
    // ⌘⇧S starts a Reply All on its subject. The palette's row makes it a new message.
    commands.add(
        Command::new(
            "REPLY_ALL_CHANGE_SUBJECT",
            "Reply All, Change Subject",
            act(Action::ReplyAllEditSubject),
        )
        .key("mod+shift+s")
        .key_code(["⌘", "shift", "S"])
        .display_only()
        .aliases(["Edit Subject"])
        .default_score(1.8)
        .regions(MESSAGES),
    );
    commands.add(
        Command::shortcut(
            "REPLY_ALL_CHANGE_SUBJECT_KEY",
            act(Action::ReplyAllWith(Field::Subject)),
        )
        .key("mod+shift+s")
        .regions(MESSAGES),
    );
}

/// `Compose to {email}` for the person the contact pane shows, after `Compose`.
pub(crate) fn register_compose_to(
    commands: &mut Commands<commands::Action>,
    shown: &crate::format::names::Person,
) {
    let to = Address {
        name: Some(shown.name.clone()).filter(|name| !name.is_empty()),
        email: shown.email.clone(),
    };
    commands.add(
        Command::new(
            "COMPOSE_TO_EMAIL",
            format!("Compose to {}", shown.email),
            commands::Action::Compose(Action::ComposeTo(to)),
        )
        .follow("COMPOSE")
        .operative_word("compose")
        .icon("data/compose")
        .regions(exo_commands::LIST_AND_THREAD),
    );
}

/// Registers the `;` picker's keys while it has rows and isn't hidden by a date, then the keys
/// of the `:` and contact dropdown, in the body or the subject.
fn picker_keys(commands: &mut Commands<commands::Action>, open: &Open, backtick: bool) {
    const FIELDS: &[Region] = &[Region::ComposeForm];
    let act = commands::Action::Compose;
    let snippets = &open.popups.snippets;
    if snippets.trigger.is_some() && !snippets.rows.is_empty() && open.body.date.is_none() {
        commands.add(
            Command::shortcut("SNIPPET_DOWN", act(Action::SnippetMove(1)))
                .key("down")
                .regions(FIELDS),
        );
        commands.add(
            Command::shortcut("SNIPPET_UP", act(Action::SnippetMove(-1)))
                .key("up")
                .regions(FIELDS),
        );
        commands.add(
            Command::shortcut("SNIPPET_PICK", act(Action::SnippetPick(None)))
                .keys(["enter", "tab"])
                .regions(FIELDS),
        );
        commands.add(
            Command::shortcut("SNIPPET_CLOSE", act(Action::SnippetClose))
                .key("esc")
                .backtick_escape(backtick)
                .regions(FIELDS),
        );
    }
    assistant::register(commands, open, backtick);
}

/// Registers the focused draft's commands. With `Send + Mark Done` on (`send_and_done`),
/// `Send & Mark Done` takes ⌘ Enter too and `Send` goes, except in the new-message pane
/// (`in_pane`), which never marks done. `backtick` is Backtick as Escape. Without it, compose's
/// Escape leaves a backtick to be typed.
fn register_form(
    commands: &mut Commands<commands::Action>,
    open: &Open,
    aliases: &[Alias],
    popped_out: bool,
    send_and_done: bool,
    in_pane: bool,
    backtick: bool,
) {
    const FORM: &[Region] = &[Region::ComposeForm, Region::RecipientList];
    const LISTS: &[Region] = &[Region::RecipientList];
    const BODY_AND_FIELDS: &[Region] = &[Region::ComposeForm];
    let act = commands::Action::Compose;
    // A snippet's `Send` is `Save Snippet`. A snippet has no Send & Mark Done and doesn't pop
    // out.
    let snippet = open.is_snippet();
    let send_title = if snippet { "Save Snippet" } else { "Send" };
    let send_keys: &[&'static str] = if send_and_done {
        &["mod+enter", "mod+shift+enter"]
    } else {
        &["mod+enter"]
    };
    if !(send_and_done && !in_pane && !snippet) {
        commands.add(
            Command::new("SEND", send_title, act(Action::Send { mark_done: false }))
                .keys(send_keys.iter().copied())
                .key_code(["⌘", "enter"])
                .aliases(["Ship It"])
                .default_score(100.0)
                .regions(FORM),
        );
    }
    if !snippet && !(send_and_done && in_pane) {
        let (keys, key_code, score): (&[&'static str], &[&'static str], f64) = if send_and_done {
            (send_keys, &["⌘", "enter"], 101.0)
        } else {
            (&["mod+shift+enter"], &["⌘", "shift", "enter"], 92.0)
        };
        commands.add(
            Command::new(
                "SEND_AND_MARK_DONE",
                "Send & Mark Done",
                act(Action::Send { mark_done: true }),
            )
            .keys(keys.iter().copied())
            .key_code(key_code.iter().copied())
            .aliases(["Send and Mark Done", "Send + Mark Done"])
            .default_score(score)
            .regions(FORM),
        );
    }
    if !snippet {
        // `Remind Me` (its Snooze id), and `Remove Reminder` once the draft has one.
        commands.add(
            Command::new("SNOOZE", "Remind Me", act(Action::RemindMe))
                .key("mod+shift+h")
                .key_code(["⌘", "shift", "H"])
                .aliases(["Add Reminder", "Snooze", "Boomerang"])
                .default_score(98.0)
                .icon("data/snooze")
                .regions(FORM),
        );
        if open.record.reminder.is_some() {
            commands.add(
                Command::new(
                    "REMOVE_REMINDER",
                    "Remove Reminder",
                    commands::Action::Remind(crate::remind::Action::RemoveFromDraft {
                        draft: open.id.clone(),
                        manual: false,
                    }),
                )
                .default_score(95.0)
                .icon("data/snooze")
                .regions(FORM),
            );
        }
        commands.add(
            Command::new("SEND_LATER", "Send Later", act(Action::SendLater))
                .key("mod+shift+l")
                .key_code(["⌘", "shift", "L"])
                .aliases(["Schedule", "Delayed Send", "Delay Send"])
                .default_score(99.0)
                .icon("data/send")
                .regions(FORM),
        );
    }
    commands.add(
        Command::new("DISCARD_DRAFTS", "Discard Draft", act(Action::Discard))
            .keys(["mod+shift+,", "mod+shift+."])
            .key_code(["⌘", "shift", ","])
            .aliases(["Delete Draft", "Trash Draft", "Abandon Draft"])
            .default_score(95.0)
            .scale(1.1)
            .regions(FORM),
    );
    let (title, aliases_of_pop) = if popped_out {
        ("Pop In Draft", ["Pop-in", "Popin"])
    } else {
        ("Pop Out Draft", ["Pop-out", "Popout"])
    };
    if !snippet {
        commands.add(
            Command::new("POP_OUT_DRAFT", title, act(Action::Pop))
                .key("mod+shift+p")
                .key_code(["⌘", "shift", "P"])
                .aliases(aliases_of_pop)
                .regions(FORM),
        );
        let title = if popped_out {
            "Search"
        } else {
            "Pop Out Draft & Search"
        };
        commands.add(
            Command::new("SEARCH", title, act(Action::PopAndSearch))
                .key("mod+/")
                .key_code(["⌘", "/"])
                .default_score(1.6)
                .icon("data/search")
                .regions(FORM),
        );
    }
    if !snippet {
        commands.add(
            crate::calendar::share_availability_command(act(Action::AvailabilityMenu(true)))
                .key("mod+shift+a")
                .key_code(["⌘", "shift", "A"])
                .regions(FORM),
        );
    }
    commands.add(
        Command::new("ATTACH", "Attach", act(Action::Attach))
            .key("mod+shift+u")
            .key_code(["⌘", "shift", "U"])
            .aliases(["Add Attachment"])
            .default_score(96.0)
            .regions(FORM),
    );
    commands.add(
        Command::new("INSERT_IMAGE", "Insert Image", act(Action::InsertImage))
            .aliases(["Attach Image"])
            .default_score(90.0)
            .icon("data/insert-image")
            .regions(FORM),
    );
    let change = |id: &'static str,
                  title: &'static str,
                  field: Field,
                  key: &'static str,
                  code: &'static str| {
        Command::new(id, title, act(Action::Focus(field)))
            .key(key)
            .key_code(["⌘", "shift", code])
            .default_score(94.0)
            .regions(FORM)
    };
    commands.add(change(
        "CHANGE_TO",
        "Change To",
        Field::To,
        "mod+shift+o",
        "O",
    ));
    commands
        .add(change("CHANGE_CC", "Change CC", Field::Cc, "mod+shift+c", "C").aliases(["Add CC"]));
    commands.add(
        change("CHANGE_BCC", "Change BCC", Field::Bcc, "mod+shift+b", "B").aliases(["Add BCC"]),
    );
    if form::active_aliases(aliases) > 1 && !snippet {
        commands.add(
            change(
                "CHANGE_FROM",
                "Change From",
                Field::From,
                "mod+shift+f",
                "F",
            )
            .aliases(["Change Alias", "Change Aliases"]),
        );
    }
    if open.is_new_email() {
        commands.add(
            change(
                "CHANGE_SUBJECT",
                "Change Subject",
                Field::Subject,
                "mod+shift+s",
                "S",
            )
            .aliases(["Edit Subject"]),
        );
    } else {
        // On a reply, the palette's row makes the reply a new message, while ⌘⇧S only focuses
        // its subject.
        commands.add(
            Command::new("CHANGE_SUBJECT", "Change Subject", act(Action::EditSubject))
                .key("mod+shift+s")
                .key_code(["⌘", "shift", "S"])
                .display_only()
                .aliases(["Edit Subject"])
                .default_score(94.0)
                .regions(FORM),
        );
        commands.add(
            Command::shortcut("CHANGE_SUBJECT_KEY", act(Action::Focus(Field::Subject)))
                .key("mod+shift+s")
                .regions(FORM),
        );
    }
    let tabs = |commands: &mut Commands<commands::Action>| {
        commands.add(
            Command::shortcut("TAB", act(Action::Tab(1)))
                .key("tab")
                .regions(FORM),
        );
        commands.add(
            Command::shortcut("SHIFT_TAB", act(Action::Tab(-1)))
                .key("shift+tab")
                .regions(FORM),
        );
    };
    // Compose's own Escape ignores a backtick unless Backtick as Escape is on.
    let escape = |commands: &mut Commands<commands::Action>| {
        commands.add(
            Command::shortcut("ESCAPE", act(Action::Escape))
                .key("esc")
                .backtick_escape(backtick)
                .regions(FORM),
        )
    };
    // ⌘O is swallowed in compose (the editor's colour picker has it).
    match open.state.field {
        Field::SnippetName => {
            // Enter goes on to To with the recipients showing, else the body.
            let next = if open.state.header_expanded {
                Field::To
            } else {
                Field::Body
            };
            commands.add(
                Command::shortcut("SNIPPET_NAME_ENTER", act(Action::Focus(next)))
                    .key("enter")
                    .regions(BODY_AND_FIELDS),
            );
            escape(commands);
            tabs(commands);
        }
        Field::Body => {
            // An Escape the editor has no use for leaves the form (egui would take it as a blur).
            // The editor keeps a backtick for typing unless Backtick as Escape is on, and then it
            // types one for ⌥⇧`.
            if !open.editor.wants_escape() {
                commands.add(
                    Command::shortcut("ESCAPE", act(Action::Escape))
                        .key("esc")
                        .backtick_escape(backtick)
                        .regions(BODY_AND_FIELDS),
                );
            }
            if backtick {
                commands.add(
                    Command::shortcut("INSERT_BACKTICK", act(Action::TypeBacktick))
                        .key("alt+shift+`")
                        .regions(BODY_AND_FIELDS),
                );
            }
            // ⌘⇧- and ⌘⇧= move a date only when the caret is on one, and go through otherwise.
            if open.body.date.is_some() {
                commands.add(
                    Command::shortcut("PREVIOUS_DAY", act(Action::ShiftDate(-1)))
                        .key("mod+shift+-")
                        .regions(BODY_AND_FIELDS),
                );
                commands.add(
                    Command::shortcut("NEXT_DAY", act(Action::ShiftDate(1)))
                        .key("mod+shift+=")
                        .regions(BODY_AND_FIELDS),
                );
            }
            // The editor's formatting keys that apply where the caret is. Tab and ⇧Tab outside a
            // list move focus (to Send, or back).
            for command in FormatCommand::ALL
                .into_iter()
                .filter(|command| command.applies(&open.editor))
            {
                commands.add(
                    Command::shortcut(command.id(), act(Action::Format(command)))
                        .keys(command.keys().iter().copied())
                        .regions(BODY_AND_FIELDS),
                );
            }
            // Tab doesn't move focus in a list, nor while the editor takes it (ghost text, a
            // suggestion chosen in autocorrect's pill).
            if !open.editor.in_list() && !open.editor.wants_tab() {
                tabs(commands);
            }
            // Share Availability's menu.
            if let Some(menu) = &open.popups.availability {
                // Its one row stays highlighted.
                commands.add(
                    Command::shortcut("AVAILABILITY_MOVE", act(Action::AvailabilityMenu(true)))
                        .keys(["up", "down"])
                        .regions(BODY_AND_FIELDS),
                );
                // Enter picks only while a row shows. With none, it types a line.
                if menu.shows_row() {
                    commands.add(
                        Command::shortcut("AVAILABILITY_PICK", act(Action::AvailabilityPick))
                            .keys(["enter", "tab"])
                            .regions(BODY_AND_FIELDS),
                    );
                }
                commands.add(
                    Command::shortcut("AVAILABILITY_CLOSE", act(Action::AvailabilityMenu(false)))
                        .key("esc")
                        .backtick_escape(backtick)
                        .regions(BODY_AND_FIELDS),
                );
                // ← or Backspace at the query's start, and → at its end, close the menu there.
                let caret = open.editor.selection().focus;
                if caret == menu.start {
                    commands.add(
                        Command::shortcut(
                            "AVAILABILITY_LEAVE",
                            act(Action::AvailabilityMenu(false)),
                        )
                        .keys(["left", "backspace"])
                        .regions(BODY_AND_FIELDS),
                    );
                } else if caret == menu.end() {
                    commands.add(
                        Command::shortcut(
                            "AVAILABILITY_LEAVE",
                            act(Action::AvailabilityMenu(false)),
                        )
                        .key("right")
                        .regions(BODY_AND_FIELDS),
                    );
                }
            }
            picker_keys(commands, open, backtick);
        }
        Field::QuotingStart => {
            commands.add(
                Command::shortcut("REVEAL_QUOTE", act(Action::RevealQuote))
                    .keys(["space", "o", "enter"])
                    .regions(BODY_AND_FIELDS),
            );
            commands.add(
                Command::shortcut("DELETE_QUOTE", act(Action::DeleteQuote))
                    .key("backspace")
                    .regions(BODY_AND_FIELDS),
            );
            commands.add(
                Command::shortcut("QUOTE_BACK", act(Action::Focus(Field::Body)))
                    .keys(["up", "left", "down"])
                    .regions(BODY_AND_FIELDS),
            );
            commands.add(
                Command::shortcut("ESCAPE", act(Action::Focus(Field::Body)))
                    .key("esc")
                    .regions(BODY_AND_FIELDS),
            );
            tabs(commands);
        }
        Field::CollapsedHeader => {
            commands.add(
                Command::shortcut("EXPAND_HEADER", act(Action::Focus(Field::To)))
                    .key("enter")
                    .regions(BODY_AND_FIELDS),
            );
            escape(commands);
            tabs(commands);
        }
        Field::Subject => {
            // Enter in a new message's subject goes on to the body, and in a reply's it starts a
            // new message.
            let enter = if open.is_new_email() {
                Action::Focus(Field::Body)
            } else {
                Action::EditSubject
            };
            commands.add(
                Command::shortcut("SUBJECT_ENTER", act(enter))
                    .key("enter")
                    .regions(BODY_AND_FIELDS),
            );
            escape(commands);
            tabs(commands);
            // A new message's `:` and `;` pickers, whose keys go over those.
            picker_keys(commands, open, backtick);
        }
        Field::From if open.header.from_menu.is_some() => {
            // The list's keys. Enter or Tab takes the highlighted alias, and Escape only closes
            // the list.
            commands.add(
                Command::shortcut("FROM_DOWN", act(Action::FromMove(1)))
                    .keys(["down", "j", "n"])
                    .regions(FORM),
            );
            commands.add(
                Command::shortcut("FROM_UP", act(Action::FromMove(-1)))
                    .keys(["up", "k", "p"])
                    .regions(FORM),
            );
            commands.add(
                Command::shortcut("FROM_CHOOSE", act(Action::FromChoose(None)))
                    .keys(["enter", "tab", "shift+tab"])
                    .regions(FORM),
            );
            commands.add(
                Command::shortcut("ESCAPE", act(Action::FromClose))
                    .key("esc")
                    .regions(FORM),
            );
        }
        Field::From => {
            // ↓ or ↑ opens the list. Enter clicks the button, which opens it too.
            commands.add(
                Command::shortcut("FROM_OPEN", act(Action::FromToggle))
                    .keys(["down", "j", "n", "up", "k", "p", "enter"])
                    .regions(FORM),
            );
            escape(commands);
            tabs(commands);
        }
        Field::Send
        | Field::SendLater
        | Field::RemindMe
        | Field::Attach
        | Field::Discard
        | Field::ExpandRecipients
        | Field::PopAction
        | Field::PopClose => {
            // Enter on a focused control clicks it. A keyboard Send shows no hint.
            let click = match open.state.field {
                Field::Send => Action::Send { mark_done: false },
                Field::SendLater => Action::SendLater,
                Field::RemindMe => Action::RemindMe,
                Field::Attach => Action::Attach,
                Field::Discard => Action::Discard,
                Field::ExpandRecipients => Action::Focus(Field::Cc),
                Field::PopAction => Action::Pop,
                _ => Action::ClosePopout,
            };
            commands.add(
                Command::shortcut("CLICK", act(click))
                    .key("enter")
                    .regions(FORM),
            );
            escape(commands);
            tabs(commands);
        }
        Field::To | Field::Cc | Field::Bcc => {
            let list = open.state.field.list().expect("a recipient field");
            let input = open.input(list);
            // The recipient field's own Escape takes a backtick whatever the setting.
            commands.add(
                Command::shortcut("ESCAPE", act(Action::Escape))
                    .key("esc")
                    .regions(FORM),
            );
            tabs(commands);
            commands.add(
                Command::shortcut("COMMIT", act(Action::Commit))
                    .key("enter")
                    .regions(LISTS),
            );
            if input.suggesting() {
                // With suggestions, ↑/↓ (and ⌃J/N, ⌃K/P) move the highlight, Tab takes it and
                // stays, and Escape closes the suggestions.
                commands.add(
                    Command::shortcut("SUGGESTION_DOWN", act(Action::MoveSuggestion(1)))
                        .keys(["down", "ctralt+j", "ctralt+n"])
                        .regions(LISTS),
                );
                commands.add(
                    Command::shortcut("SUGGESTION_UP", act(Action::MoveSuggestion(-1)))
                        .keys(["up", "ctralt+k", "ctralt+p"])
                        .regions(LISTS),
                );
                commands.add(
                    Command::shortcut("SUGGESTION_TAB", act(Action::Commit))
                        .key("tab")
                        .regions(LISTS),
                );
                commands.add(
                    Command::shortcut("SUGGESTION_ESCAPE", act(Action::CloseSuggestions))
                        .key("esc")
                        .regions(LISTS),
                );
            }
            if input.text.contains('@') {
                commands.add(
                    Command::shortcut("COMMIT_PUNCTUATION", act(Action::Commit))
                        .keys([",", ";"])
                        .regions(LISTS),
                );
            }
            if input.text.is_empty() {
                commands.add(
                    Command::shortcut("CHIP_BACKSPACE", act(Action::Backspace))
                        .keys(["backspace", "del"])
                        .regions(LISTS),
                );
                commands.add(
                    Command::shortcut("CHIP_LEFT", act(Action::MoveSelection(-1)))
                        .key("left")
                        .regions(LISTS),
                );
                if !input.selected.is_empty() || input.editing.is_some() {
                    commands.add(
                        Command::shortcut("CHIP_RIGHT", act(Action::MoveSelection(1)))
                            .key("right")
                            .regions(LISTS),
                    );
                }
            } else if input.caret == Some(0) {
                // At the edge of the typed text, ← and → finish it.
                commands.add(
                    Command::shortcut("CHIP_LEFT", act(Action::MoveSelection(-1)))
                        .key("left")
                        .regions(LISTS),
                );
            } else if input.caret == Some(input.text.chars().count()) {
                commands.add(
                    Command::shortcut("CHIP_RIGHT", act(Action::MoveSelection(1)))
                        .key("right")
                        .regions(LISTS),
                );
            }
            chips::register(commands, open, list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{super::Alert, *};

    #[test]
    fn compose_alert_arrows_move_right_to_left_and_tab_wraps_from_zero() {
        let mut compose = Compose {
            alert: Some(Alert::new(
                Some("Save Snippet?"),
                "Save changes?",
                vec!["Save", "Cancel", "Delete"],
                "draft-1",
                AlertThen::SaveSnippet,
            )),
            ..Compose::default()
        };
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("system fonts load");
        let page = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let step_key = |compose: &mut Compose, key: egui::Key, modifiers: egui::Modifiers| {
            let mut commands = Commands::new();
            commands.set_region(Region::Alert);
            compose.register(&mut commands, &[], &[]);
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
            let mut actions = Vec::new();
            let mut out = ctx.run_ui(raw, |ui| {
                actions = commands.dispatch(ui.ctx());
            });
            out.textures_delta.clear();
            for action in actions {
                if let commands::Action::Compose(Action::AlertSelect(delta)) = action
                    && let Some(alert) = &mut compose.alert
                {
                    alert.selected = exo_ui::Alert::move_selection(
                        alert.selected,
                        alert.actions.len(),
                        delta,
                        true,
                    );
                }
            }
            compose.alert.as_ref().expect("alert open").selected
        };
        assert_eq!(
            step_key(&mut compose, egui::Key::ArrowLeft, egui::Modifiers::NONE),
            1,
            "ArrowLeft moves visually left from Save (0) to Cancel (1)"
        );
        assert_eq!(
            step_key(&mut compose, egui::Key::ArrowRight, egui::Modifiers::NONE),
            0,
            "ArrowRight moves visually right from Cancel (1) to Save (0)"
        );
        assert_eq!(
            step_key(&mut compose, egui::Key::Tab, egui::Modifiers::NONE),
            2,
            "Tab wraps from 0 to leftmost button (2)"
        );
    }
}
