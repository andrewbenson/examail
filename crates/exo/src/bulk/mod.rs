//! Bulk actions. Triage on 50 or more selected conversations, or on a select-all that reaches
//! conversations not loaded yet, asks first (`This might take a while…`) and then runs in the
//! background ([`Jobs`]) instead of row by row. A bulk action's rows say `Pending:` until it's
//! done, the footer counts it, and the toast's Undo puts it all back. Get Me To Zero is one too,
//! with its own dialog, and the Bulk Actions dialog lists those.

mod actions_dialog;
mod get_me_to_zero;
mod jobs;

use std::collections::HashMap;

use egui::{Context, Rect};
use exo_commands::{Command, Commands, Hint, Region, Toast};
use exo_store::{
    BulkTarget, Filter, LabelIndex, ListCutoff, ListId, Mutation, Scope, SearchQuery, ThreadId,
};

pub use jobs::{Jobs, Request, Status, Summary};

use crate::{Mail, account::UndoToken, commands, inbox::SelectionHint};

/// Selections this large go through a bulk action even when every row is loaded.
const THRESHOLD: usize = 50;
/// Get Me To Zero's name as a bulk action, which the Bulk Actions dialog lists.
pub(crate) const GET_ME_TO_ZERO: &str = "Get Me To Zero";

/// A bulk action waiting for the user's OK, with the toast it starts with.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    request: Request,
    toast: String,
}

/// What the bulk dialogs and their commands ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Open Get Me To Zero.
    OpenGetMeToZero,
    /// Open Bulk Actions.
    OpenBulkActions,
    /// Something in Get Me To Zero.
    GetMeToZero(get_me_to_zero::Event),
    /// Something in Bulk Actions.
    BulkActions(actions_dialog::Event),
}

/// The bulk dialog on screen, or fading out.
#[derive(Debug, Default)]
pub(crate) struct Dialogs {
    shown: Option<Shown>,
    /// Whether it hasn't been closed.
    open: bool,
}

#[derive(Debug)]
enum Shown {
    GetMeToZero(get_me_to_zero::State),
    BulkActions(actions_dialog::State),
}

impl Dialogs {
    /// Whether a bulk dialog has the keyboard.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

/// Registers the palette's `Get Me To Zero` and `Bulk Actions`, and while a bulk dialog is open,
/// its keys.
pub(crate) fn register(commands: &mut Commands<commands::Action>, dialogs: &Dialogs) {
    let act = commands::Action::Bulk;
    commands.add(
        Command::new(
            "GET_ME_TO_ZERO",
            GET_ME_TO_ZERO,
            act(Action::OpenGetMeToZero),
        )
        .aliases([
            "Bulk Archive",
            "Bulk Mark Done",
            "Inbox Zero",
            "Clean up my inbox",
        ])
        .default_score(1.0)
        .icon("data/done"),
    );
    commands.add(
        Command::new(
            "BULK_ACTIONS_DIALOG",
            "Bulk Actions",
            act(Action::OpenBulkActions),
        )
        .aliases(["Settings", GET_ME_TO_ZERO])
        .operative_word("bulk")
        .default_score(0.98)
        .icon("data/settings")
        .multiple(),
    );
    const DIALOG: &[Region] = &[Region::Dialog];
    let key = |id: &'static str, action: Action, keys: &[&'static str]| {
        Command::shortcut(id, act(action))
            .keys(keys.iter().copied())
            .regions(DIALOG)
    };
    match (&dialogs.shown, dialogs.open) {
        (Some(Shown::GetMeToZero(_)), true) => {
            use get_me_to_zero::Event;
            let gmtz = Action::GetMeToZero;
            commands.add(key("GMTZ_ESCAPE", gmtz(Event::Escape), &["esc"]));
            commands.add(key(
                "GMTZ_ENTER",
                gmtz(Event::Activate),
                &["enter", "space"],
            ));
            commands.add(key("GMTZ_NEXT", gmtz(Event::Move(1)), &["tab", "down"]));
            commands.add(key(
                "GMTZ_PREVIOUS",
                gmtz(Event::Move(-1)),
                &["shift+tab", "up"],
            ));
        }
        (Some(Shown::BulkActions(_)), true) => {
            use actions_dialog::Event;
            let rows = Action::BulkActions;
            commands.add(key("BULK_ACTIONS_ESCAPE", rows(Event::Close), &["esc"]));
            commands.add(key("BULK_ACTIONS_ENTER", rows(Event::Activate), &["enter"]));
            commands.add(key(
                "BULK_ACTIONS_DOWN",
                rows(Event::Move(1)),
                &["down", "j", "tab", "ctralt+j", "ctralt+n"],
            ));
            commands.add(key(
                "BULK_ACTIONS_UP",
                rows(Event::Move(-1)),
                &["up", "k", "shift+tab", "ctralt+k", "ctralt+p"],
            ));
        }
        _ => {}
    }
}

impl Mail {
    /// Takes over triage of `ids` with `mutation` when it's a bulk action, which is when the
    /// list's selection has 50 rows or more or reaches rows not loaded. It asks first or, where the
    /// list can't be acted on in bulk, says why it can't be done. Where the list can't but every
    /// row is loaded, the triage is left to act row by row. Returns whether it took the triage.
    pub(crate) fn bulk_instead(&mut self, ids: &[String], mutation: &Mutation) -> bool {
        let Some(name) = crate::triage_copy::bulk_name(mutation) else {
            return false;
        };
        if self.thread.is_some() || !self.inbox.is_selecting() || self.inbox.selection() != ids {
            return false;
        }
        let exact = self.inbox.is_exact();
        if exact && ids.len() < THRESHOLD {
            return false;
        }
        if let Some(text) = self.bulk_refusal() {
            if !exact {
                self.pickers.inform("Sorry!", text);
            }
            return !exact;
        }
        let searched = self.search.as_ref().and_then(|search| search.query());
        let scope = searched.map_or_else(
            || Scope::List(self.list.clone()),
            |query| Scope::Search(query.to_owned()),
        );
        // A search's filters are tokens of its query.
        let filter = self.filter.filter(|_| self.search.is_none());
        let target = BulkTarget {
            list: self
                .inbox
                .cutoff()
                .filter(|_| !exact)
                .map(|before| ListCutoff {
                    scope,
                    filter,
                    keep: Vec::new(),
                    before,
                }),
            include: ids.iter().map(|id| ThreadId::new(id.clone())).collect(),
            exclude: self
                .inbox
                .unselected()
                .into_iter()
                .map(ThreadId::new)
                .collect(),
        };
        let toast = crate::triage_copy::bulk_toast(
            mutation,
            &self.list,
            &self.snapshot.labels,
            self.account.settings().splits(),
        );
        let request = Request {
            name,
            query: None,
            mutation: mutation.clone(),
            target,
            threads: ids.iter().cloned().collect(),
        };
        let what = if ids.len() >= THRESHOLD {
            "many conversations"
        } else {
            "conversations that are still loading"
        };
        let text = format!(
            "You are about to act on {what}, which may take some time. You can track progress in the bottom right of the screen."
        );
        self.pickers.confirm_bulk(
            "This might take a while…",
            text,
            None,
            vec!["OK", "Cancel"],
            Plan { request, toast },
        );
        true
    }

    /// Why the list or search on screen can't be acted on in bulk.
    fn bulk_refusal(&self) -> Option<String> {
        if let Some(query) = self.search.as_ref().and_then(|search| search.query()) {
            let labels = LabelIndex::new(
                [],
                self.snapshot
                    .labels
                    .iter()
                    .map(|label| (label.id.as_str(), label.name.as_str())),
            );
            return SearchQuery::parse(query).bulk_refusal(&labels);
        }
        Some(match self.filter {
            Some(Filter::Important) => "We can't complete this action on conversations filtered by important.\n\nPlease remove the filter and try again.",
            Some(Filter::NoReply) => "We can't complete this action on conversations filtered by no reply.\n\nPlease remove the filter and try again.",
            _ if self.list == ListId::Drafts => "We can't complete this action in the Drafts folder.\n\nPlease unselect these conversations and try again.",
            _ if self.list == ListId::Scheduled => "We can't complete this action in the Scheduled folder.\n\nPlease unselect these conversations and try again.",
            _ => return None,
        }.into())
    }

    /// Runs `plan` once the user said OK. The selection clears, and the toast's Undo puts back
    /// what it did.
    pub(crate) fn start_bulk(&mut self, plan: Plan) {
        // No Get Me To Zero opens by itself after an interrupted first run.
        if plan.request.name == GET_ME_TO_ZERO {
            self.account
                .update_device_settings(|device| device.gmtz_started = true);
        }
        let token = self.account.undo_token();
        self.account.bulk(token, plan.request);
        self.inbox.clear_selection();
        self.undoable = None;
        self.commands
            .toast(Toast::new(plan.toast).undo(commands::Action::Undo(token)));
    }

    /// The toast's Undo ran on `token`. Get Me To Zero says so (`Undoing Get Me To Zero.`).
    pub(crate) fn bulk_undone(&mut self, token: UndoToken) {
        if self
            .account
            .bulk_actions()
            .iter()
            .any(|action| action.token == token && action.name == GET_ME_TO_ZERO)
        {
            self.commands.toast(Toast::new("Undoing Get Me To Zero."));
        }
    }

    /// Opens Get Me To Zero as the first run ends in it. It welcomes the user to their inbox, and
    /// so does every Get Me To Zero until one has closed.
    pub fn open_first_get_me_to_zero(&mut self) {
        self.account
            .update_settings(|settings| settings.show_first_time_gmtz = true);
        self.bulk_run(Action::OpenGetMeToZero);
    }

    /// Closes the bulk dialog. A Get Me To Zero that welcomed the user leaves the keyboard on the
    /// first conversation, and the welcome is done with.
    fn close_bulk_dialog(&mut self) {
        self.bulk_dialogs.open = false;
        if let Some(Shown::GetMeToZero(state)) = &self.bulk_dialogs.shown
            && state.is_first_time()
        {
            self.inbox.cursor_to_top();
            self.account
                .update_settings(|settings| settings.show_first_time_gmtz = false);
        }
    }

    /// Carries out a bulk dialog's `action`.
    pub(crate) fn bulk_run(&mut self, action: Action) {
        match action {
            Action::OpenGetMeToZero => {
                // The welcome lasts until a Get Me To Zero that shows it closes.
                let state = if self.account.settings().show_first_time_gmtz {
                    get_me_to_zero::State::first_time()
                } else {
                    get_me_to_zero::State::default()
                };
                self.commands.close_palette();
                self.bulk_dialogs = Dialogs {
                    shown: Some(Shown::GetMeToZero(state)),
                    open: true,
                };
            }
            Action::OpenBulkActions => {
                self.commands.close_palette();
                self.bulk_dialogs = Dialogs {
                    shown: Some(Shown::BulkActions(actions_dialog::State::default())),
                    open: true,
                };
            }
            Action::GetMeToZero(event) => {
                let Some(Shown::GetMeToZero(state)) = &mut self.bulk_dialogs.shown else {
                    return;
                };
                match state.handle(event) {
                    Some(get_me_to_zero::Event::Confirm) => {
                        let state = state.clone();
                        self.close_bulk_dialog();
                        self.confirm_get_me_to_zero(&state);
                    }
                    // `Clean up your inbox anytime by hitting ⌘ K → Get Me To Zero!`
                    Some(get_me_to_zero::Event::DoLater) => {
                        self.close_bulk_dialog();
                        self.commands.hint(
                            Hint::new()
                                .text("Clean up your inbox anytime by hitting")
                                .keys(["⌘", "K"])
                                .text("→ Get Me To Zero!"),
                        );
                    }
                    Some(get_me_to_zero::Event::Close) => self.close_bulk_dialog(),
                    _ => {}
                }
            }
            Action::BulkActions(event) => {
                let rows = self.get_me_to_zero_actions();
                let Some(Shown::BulkActions(state)) = &mut self.bulk_dialogs.shown else {
                    return;
                };
                match state.handle(event, rows.len()) {
                    Some(actions_dialog::Event::Row(row)) => {
                        let Some(row) = rows.get(row) else { return };
                        // Cancel a waiting one, Undo one that ran.
                        match actions_dialog::action(row) {
                            ("Cancel", true) => self.pickers.confirm_undo(
                                "Cancel action",
                                format!("Would you like to cancel this {} action?", row.name),
                                vec!["Cancel", "Keep"],
                                row.token,
                            ),
                            (_, true) => self.pickers.confirm_undo(
                                "Undo action",
                                format!("Would you like to undo this {} action?", row.name),
                                vec!["Undo", "Cancel"],
                                row.token,
                            ),
                            _ => {}
                        }
                    }
                    Some(actions_dialog::Event::Close) => self.bulk_dialogs.open = false,
                    _ => {}
                }
            }
        }
    }

    /// Get Me To Zero's `Let's go!`. The dialog closes and an alert says it may take a while. The
    /// alert's `Got it` (or Escape) starts it, which marks Done every inbox conversation older
    /// than the age, except those kept.
    fn confirm_get_me_to_zero(&mut self, state: &get_me_to_zero::State) {
        let before = get_me_to_zero::RANGES[state.range].before(&self.now);
        let keep: Vec<Filter> = [
            (state.keep_unread, Filter::Unread),
            (state.keep_starred, Filter::Starred),
        ]
        .into_iter()
        .filter_map(|(kept, filter)| kept.then_some(filter))
        .collect();
        // Gmail's inbox label, which every inbox view and split is drawn from (`in:inbox`).
        let inbox = ListId::Label("INBOX".into());
        let target = BulkTarget {
            // `before` is exclusive here, since a message dated at the cutoff isn't older than it.
            list: Some(ListCutoff {
                scope: Scope::List(inbox),
                filter: None,
                keep,
                before: before - jiff::SignedDuration::from_millis(1),
            }),
            ..BulkTarget::default()
        };
        // The loaded rows it reaches, which show `Pending:` while it runs.
        let threads = self
            .inbox
            .threads()
            .iter()
            .filter(|thread| {
                !thread.done
                    && thread.received < before
                    && !(state.keep_unread && thread.unread)
                    && !(state.keep_starred && thread.starred)
            })
            .map(|thread| thread.id.clone())
            .collect();
        let query = state.display_query(&before.to_zoned(self.now.time_zone().clone()));
        let request = Request {
            name: GET_ME_TO_ZERO,
            query: Some(query),
            mutation: Mutation::MarkDone,
            target,
            threads,
        };
        // `You can track progress or undo this action with ⌘ K → Bulk Actions.`
        let text = format!(
            "Moving a lot of emails may take some time.{}You can track progress or undo this action \nwith",
            exo_ui::PARAGRAPH
        );
        let keys = crate::pickers::Keys {
            caps: &["⌘", "K"],
            after: "→ Bulk Actions.",
        };
        self.pickers.confirm_bulk(
            "You're on your way! 🎉",
            text,
            Some(keys),
            vec!["Got it"],
            Plan {
                request,
                toast: crate::triage_copy::bulk_toast(
                    &Mutation::MarkDone,
                    &self.list,
                    &self.snapshot.labels,
                    self.account.settings().splits(),
                ),
            },
        );
    }

    /// The Get Me To Zero actions of the last week, newest first, which are the Bulk Actions
    /// dialog's rows.
    fn get_me_to_zero_actions(&self) -> Vec<Summary> {
        self.account
            .bulk_actions()
            .into_iter()
            .rev()
            .filter(|action| action.name == GET_ME_TO_ZERO)
            .collect()
    }

    /// Draws the bulk dialog, if one shows, over `area` (the window below the tab strip).
    pub(crate) fn show_bulk_dialog(&mut self, ctx: &Context, area: Rect) {
        let open = self.bulk_dialogs.open;
        let (inner, close) = match &self.bulk_dialogs.shown {
            None => return,
            Some(Shown::GetMeToZero(state)) => {
                let response = get_me_to_zero::show(ctx, area, open, state);
                (
                    response.inner.map(|event| event.map(Action::GetMeToZero)),
                    response.close,
                )
            }
            Some(Shown::BulkActions(state)) => {
                let rows = self.get_me_to_zero_actions();
                let response = actions_dialog::show(ctx, area, open, state, &rows);
                (
                    response.inner.map(|event| event.map(Action::BulkActions)),
                    response.close,
                )
            }
        };
        match inner {
            None => self.bulk_dialogs.shown = None,
            Some(action) => {
                if close {
                    self.close_bulk_dialog();
                }
                if let Some(action) = action {
                    self.bulk_run(action);
                }
            }
        }
    }

    /// Shows the hint the selection asked for.
    pub(crate) fn selection_hint(&mut self) {
        let hint = match self.inbox.take_hint() {
            Some(SelectionHint::SelectAll) => Hint::new()
                .text("Want to select everything before this conversation? Hit")
                .keys(["⌘", "A"])
                .text("!"),
            Some(SelectionHint::ChangedYourMind) => Hint::new()
                .text("Changed your mind? Hit")
                .keys(["esc"])
                .text("to unselect all conversations."),
            None => return,
        };
        self.commands.hint(hint);
    }

    /// Refuses `⇧Y` on a select-all that reaches rows not loaded, because removing every label
    /// needs each row's labels. Returns whether it refused.
    pub(crate) fn refuse_unloaded(&mut self, what: &str) -> bool {
        if self.thread.is_some() || self.inbox.is_exact() {
            return false;
        }
        self.commands.hint(Hint::new().text(format!("You cannot {what} on conversations that have not loaded. Please clear your selection and try again.")));
        true
    }

    /// What each loaded row shows instead of its date while a bulk action waits or runs for it,
    /// such as `Pending: Mark Done`.
    pub(crate) fn pending_rows(&self, actions: &[Summary]) -> HashMap<String, String> {
        let mut rows = HashMap::new();
        for action in actions
            .iter()
            .filter(|action| matches!(action.status, Status::Pending | Status::Running))
        {
            for thread in action.threads.iter() {
                rows.entry(thread.clone())
                    .or_insert_with(|| format!("Pending: {}", action.name));
            }
        }
        rows
    }
}

/// The footer's status while a bulk action works, such as `Updating: 120 of 4,000`, with
/// `of many` until its threads are all found.
pub(crate) fn status(actions: &[Summary]) -> Option<String> {
    let action = actions.iter().find(|action| action.active)?;
    let done = crate::format::thousands(action.done.max(1));
    Some(if action.total == 0 {
        format!("Updating: {done} of many")
    } else {
        format!(
            "Updating: {done} of {}",
            crate::format::thousands(action.total)
        )
    })
}
