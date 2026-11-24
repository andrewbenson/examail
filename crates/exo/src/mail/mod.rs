//! Submodules decomposing [`crate::Mail`] into focused responsibilities:
//! - [`accessors`]: configuration setters, [`crate::ViewContext`] sync, and state/test queries.
//! - [`commands`]: focus-region priority chain and per-frame command registration.
//! - [`copy_print`]: clipboard copy and print/PDF export for messages and conversations.
//! - [`frame`]: per-frame layout, chrome, and overlay rendering pipeline.
//! - [`pane`]: right-pane contact tracking, cursor settle, and panel rendering.
//! - [`run`]: command dispatch, list interactions, folder drawer, and back navigation.
//! - [`thread_host`]: open-thread lifecycle, rendering host, and thread action routing.
//! - [`triage`]: snapshot sync, list switching, triage mutations, departures, undo, and events.

mod accessors;
mod commands;
mod copy_print;
mod frame;
mod pane;
mod run;
mod thread_host;
mod triage;

use exo_store::ListId;

use crate::{account::UndoToken, format::names::Person};

/// What an undo puts back: the threads, and where the cursor was and on which list.
pub(crate) struct Undoable {
    pub(super) token: UndoToken,
    pub(super) list: ListId,
    pub(super) threads: Vec<String>,
    pub(super) cursor: Option<String>,
    /// Whether the action ran in the open thread, which undo reopens.
    pub(super) in_thread: bool,
    /// Whether it was a move, whose undo says so.
    pub(super) moved: bool,
}

/// A thread an action may take out of the list without the store having said so beforehand (a move,
/// removing labels). Its row slides out when the first snapshot showing the action drops it.
pub(crate) struct Departure {
    pub(super) thread: String,
    /// The action's [`crate::account::Account::apply`] number.
    pub(super) applied: u64,
    pub(super) list: ListId,
    /// Where the cursor goes from a row that leaves: 1 down, -1 up (`[` and `]`), 0 auto.
    pub(super) direction: isize,
}

/// Right-pane contact tracking and hover-settle state private to [`crate::Mail`].
#[derive(Default)]
pub(crate) struct PaneState {
    /// The address and conversation whose contact pane details the account was last asked for.
    pub(super) contact: Option<(String, Option<String>)>,
    /// The contact that a Mail row opened its thread with, or that a draft's chip showed, which the
    /// pane keeps showing until the focus moves. Also what had the focus then: the thread's focused
    /// message, or in the list the cursor's thread.
    pub(super) pin: Option<(Person, Option<String>)>,
    /// Whom the contact pane showed this frame, for `Compose to {email}`.
    pub(super) shown: Option<Person>,
    /// The cursor's thread and since when, for the contact pane's settle delay.
    pub(super) cursor_since: (Option<String>, f64),
}
