//! Snapshot synchronization, list switching, triage mutations, departures, undo, and background
//! event reporting on [`Mail`].

use std::sync::Arc;

use exo_commands::{Hint, Toast, ToastKind};
use exo_store::{ListId, Mutation};

use super::{Departure, Undoable};
use crate::{
    Mail,
    account::{Event, UndoToken},
    actionable,
    commands::Action,
    filter,
    inbox::{self, View},
    lists,
    mailbox::ThreadSummary,
    splits, triage_copy,
};

impl Mail {
    /// Takes the newest snapshot of the list on screen into the list view.
    pub(super) fn sync_snapshot(&mut self) {
        self.snapshot = self.account.snapshot();
        let current = self.snapshot.list == self.list
            && self.snapshot.filter == self.filter
            && self.search.is_none();
        if current
            && !self
                .rows_from
                .as_ref()
                .is_some_and(|taken| Arc::ptr_eq(taken, &self.snapshot))
        {
            self.inbox
                .set_reading(self.thread.is_some() && self.search.is_none());
            self.dismiss_departed();
            self.inbox.set_threads(self.snapshot.threads.clone());
            self.rows_from = Some(self.snapshot.clone());
        }
        let inbox = lists::is_inbox(&self.list);
        let settings = self.account.settings();
        let mut tabs = if inbox {
            splits::tabs(settings, &self.list, &self.snapshot.counts)
        } else {
            Vec::new()
        };
        splits::describe(&mut tabs, settings, self.settings.split_library());
        self.inbox.set_view(View {
            title: lists::name(&self.list, &self.snapshot.labels, settings.splits()).to_owned(),
            // While Tab is held, the tab it has reached.
            selected: tabs
                .iter()
                .position(|tab| tab.list == *self.split_highlight.as_ref().unwrap_or(&self.list))
                .unwrap_or_default(),
            tabs,
            // Examail adds splits from Split Inbox Settings (the header's gear), not a header button.
            add_split: false,
            inbox,
            more: current && self.snapshot.more,
            loading: !current || self.snapshot.loading,
            complete: current && self.snapshot.complete,
            archivable: self.list != ListId::Spam,
            filter: self.filter.map(filter::name),
            count: self.snapshot.count.filter(|_| current),
            search: None,
            unsegmented: self.list == ListId::Scheduled,
            // A split (or Other) that shows while empty, until the nudge is done with.
            nudge: !settings.nudge_completed(inbox::HIDE_EMPTY_SPLIT)
                && match &self.list {
                    ListId::Split(id) => settings
                        .split(id)
                        .is_some_and(|split| !split.hides_when_empty()),
                    ListId::Other => !settings.hides_other_when_empty(),
                    _ => false,
                },
            streak: settings.statistics.inbox_zero_streak_week_current,
            hide_chrome: self
                .achievements
                .hides_chrome(self.ctx.input(|input| input.time)),
            settings_open: self.settings.visible(),
        });
        self.search_sync();
    }

    /// Switches the list view to `list`.
    pub(crate) fn show(&mut self, list: ListId) {
        let filter = filter::for_list(&self.list, &list, self.filter, &self.last_filters);
        if filter != self.filter {
            self.filter = filter;
            self.account.filter(filter);
        }
        self.list = list.clone();
        self.rows_from = None;
        self.asked_more_at = None;
        self.inbox.clear();
        self.account.show(list);
    }

    /// Applies `mutation` to the actionable threads, with the toast for it.
    pub(crate) fn triage(&mut self, mutation: Mutation, now: f64) {
        self.dismiss_command_nudge();
        let ids: Vec<String> = actionable(self.thread.as_ref(), &self.inbox)
            .iter()
            .map(|thread| thread.id.clone())
            .collect();
        self.triage_threads(&ids, mutation, None, 0, now);
    }

    /// Applies `mutation` to the listed threads among `ids`. Their rows slide out if it takes them
    /// out of this list, and a toast says `text` (or what `mutation` did), with Undo. A thread the
    /// mutation takes out of the list gives way to the next conversation in `direction`, or in Auto
    /// Advance's direction ([`Self::dismiss_and_advance`]). Returns the toast's undo token, when
    /// there was something to apply.
    pub(crate) fn triage_threads(
        &mut self,
        ids: &[String],
        mutation: Mutation,
        text: Option<String>,
        direction: isize,
        now: f64,
    ) -> Option<UndoToken> {
        if self.bulk_instead(ids, &mutation) {
            return None;
        }
        let in_thread = self.thread.is_some();
        let threads: Vec<&ThreadSummary> = self
            .inbox
            .threads()
            .iter()
            .filter(|thread| ids.contains(&thread.id))
            .collect();
        if threads.is_empty() {
            return None;
        }
        let already = match mutation {
            Mutation::MarkDone if threads.iter().all(|thread| thread.done) => {
                Some("Already marked as Done.")
            }
            Mutation::MarkNotDone if threads.iter().all(|thread| !thread.done) => {
                Some("Already marked not Done.")
            }
            _ => None,
        };
        if let Some(text) = already {
            self.commands.toast(Toast::new(text));
            return None;
        }
        let ids: Vec<String> = threads.iter().map(|thread| thread.id.clone()).collect();
        // In Starred, starring and unstarring leave the rows where they are (the account holds
        // them), and marking read or unread doesn't take out an unstarred row it holds.
        let stays = self.search.is_none()
            && self.list == ListId::Starred
            && matches!(
                mutation,
                Mutation::Star | Mutation::Unstar | Mutation::MarkRead | Mutation::MarkUnread
            );
        // Snoozing takes a thread out of the inbox as marking it done does.
        let removes = |thread: &ThreadSummary| {
            thread.removed_by.contains(&mutation)
                || (matches!(mutation, Mutation::Remind { snooze: true, .. })
                    && thread.removed_by.contains(&Mutation::MarkDone))
        };
        let leaving: Vec<String> = threads
            .iter()
            .filter(|thread| !stays && removes(thread))
            .map(|thread| thread.id.clone())
            .collect();
        let cursor = self
            .thread
            .as_ref()
            .map(|thread| thread.id().to_owned())
            .or_else(|| self.inbox.cursor().map(|thread| thread.id.clone()));
        let token = self.account.undo_token();
        let applied = self.account.apply(&ids, mutation.clone(), Some(token));
        if matches!(mutation, Mutation::MarkDone) {
            self.count_done(ids.len(), now);
        }
        // The store predicts the common triage; anything else leaves once its snapshot says so.
        // Search results stay put.
        let unpredicted = ids
            .iter()
            .filter(|id| self.search.is_none() && !stays && !leaving.contains(id));
        self.departing.extend(unpredicted.map(|id| Departure {
            thread: id.clone(),
            applied,
            list: self.list.clone(),
            direction,
        }));
        let text = text.unwrap_or_else(|| triage_copy::toast(&mutation, &self.snapshot.labels));
        self.commands
            .toast(Toast::new(text).undo(Action::Undo(token)));
        self.undoable = Some(Undoable {
            token,
            list: self.list.clone(),
            threads: ids,
            cursor,
            in_thread,
            moved: matches!(mutation, Mutation::Move { .. }),
        });
        if !leaving.is_empty() {
            self.dismiss_and_advance(&leaving, direction, now);
        }
        Some(token)
    }

    /// Slides `leaving` out of the list and moves on. The cursor goes to the next conversation in
    /// `direction` (`[`, `]`), else the way Auto Advance goes (`desktopAutoAdvance`) from where the
    /// open conversation was before it moved up the list, or the other way past the end. An open
    /// thread among them gives way to that conversation, or to the list when Auto Advance is off,
    /// when there's no next conversation, or when there's none in `direction` (back in the list,
    /// the cursor goes down first).
    pub(crate) fn dismiss_and_advance(&mut self, leaving: &[String], direction: isize, now: f64) {
        let toward = if direction != 0 {
            direction
        } else {
            self.account.settings().desktop_auto_advance.direction()
        };
        let anchored = if direction == 0 && toward != 0 && !self.inbox.is_selecting() {
            self.inbox.take_advance_anchor(toward, leaving)
        } else {
            None
        };
        let went = self
            .inbox
            .dismiss_towards(leaving, if toward == 0 { 1 } else { toward }, now);
        if let Some(anchored) = &anchored {
            self.inbox.point_at(anchored);
        }
        let Some(open) = self
            .thread
            .as_ref()
            .map(|thread| thread.id().to_owned())
            .filter(|open| leaving.contains(open))
        else {
            return;
        };
        match self.inbox.cursor().map(|thread| thread.id.clone()) {
            Some(next) if toward != 0 && next != open && (direction == 0 || went) => {
                self.open_thread(next)
            }
            _ => self.close_thread(),
        }
    }

    /// Slides out the departing rows that the latest snapshot no longer lists, before the snapshot
    /// replaces the list's rows. When an action with a direction (`[`, `]`) leaves a row in place,
    /// the cursor moves instead.
    fn dismiss_departed(&mut self) {
        let applied = self.snapshot.applied;
        let (due, waiting): (Vec<Departure>, Vec<Departure>) = std::mem::take(&mut self.departing)
            .into_iter()
            .partition(|departure| departure.applied <= applied);
        self.departing = waiting;
        let now = self.ctx.input(|input| input.time);
        let listed = |id: &str| self.snapshot.threads.iter().any(|thread| thread.id == id);
        let due: Vec<Departure> = due
            .into_iter()
            .filter(|departure| departure.list == self.list)
            .collect();
        let gone: Vec<String> = due
            .iter()
            .filter(|departure| !listed(&departure.thread))
            .map(|departure| departure.thread.clone())
            .collect();
        let direction = due.first().map_or(0, |departure| departure.direction);
        if gone.is_empty() {
            if direction != 0 {
                self.step(direction);
            }
            return;
        }
        self.dismiss_and_advance(&gone, direction, now);
    }

    /// Undoes the mutation behind `token` and takes the user back to its threads.
    pub(super) fn undo(&mut self, token: UndoToken, now: f64) {
        self.account.undo(token);
        self.picker_undone(token);
        self.bulk_undone(token);
        self.unsubscribe_undone(token);
        let Some(undone) = self.undoable.take_if(|undoable| undoable.token == token) else {
            return;
        };
        if undone.list != self.list {
            self.show(undone.list);
        }
        self.inbox
            .restore(&undone.threads, undone.cursor.clone(), now);
        if undone.moved {
            self.commands.toast(Toast::new("Move undone."));
        }
        if undone.in_thread
            && let Some(id) = undone.cursor
            && self.thread.as_ref().is_none_or(|thread| thread.id() != id)
        {
            self.open_thread(id);
        }
    }

    /// Tells the user about what went wrong in the background.
    pub(super) fn report_events(&mut self) {
        let events: Vec<Event> = self.account.events();
        for event in events {
            match event {
                Event::Failed { mutation, undo } => {
                    if let Some(text) = triage_copy::failure(&mutation, undo) {
                        self.commands.toast(Toast::new(text).kind(ToastKind::Error));
                    }
                }
                event @ (Event::LabelCreated(_) | Event::LabelNotCreated { .. }) => {
                    let now = self.ctx.input(|input| input.time);
                    self.label_event(event, now);
                }
                Event::Unsent(unsent) => self.sign_out_unsent(unsent),
                Event::Thread { id, detail } => self.unsubscribe_thread_read(id, detail),
                Event::Unavailable { id, offline } => self.thread_unavailable(&id, offline),
                Event::McpToken(answer) => self.mcp_token(answer),
                event => self.compose_event(event),
            }
        }
    }

    /// A Done button, which marks the actionable threads done, or not done when they all are.
    pub(super) fn toggle_done(&mut self, now: f64) {
        let done = actionable(self.thread.as_ref(), &self.inbox)
            .iter()
            .all(|thread| thread.done);
        let hint = if done {
            Hint::new()
                .text("Next time, hit")
                .keys(["shift", "E"])
                .text("to Mark not Done!")
        } else {
            Hint::new()
                .text("Next time, hit")
                .keys(["E"])
                .text("to Mark Done instantly!")
        };
        self.commands.hint(hint);
        self.triage(
            if done {
                Mutation::MarkNotDone
            } else {
                Mutation::MarkDone
            },
            now,
        );
    }
}
