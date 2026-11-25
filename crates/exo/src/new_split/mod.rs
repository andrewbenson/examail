//! Making split inboxes from ⌘K.
//!
//! `New Split Inbox` and `Add to Split Inbox` offer definitions drawn from the search on screen
//! and the conversations actions apply to: their senders, the senders' domains, everyone they
//! were sent to, a lone conversation's subject (through `Edit Subject`) and their labels. Then
//! `New Split Inbox` asks for a name, and `Add to Split Inbox` for the split to add it to. The
//! conversation under the cursor (or the open one) can also offer `New Split: Google Docs`, a
//! split for a known service, or a split for its mailing list. The last two go through the New
//! Split dialog ([`dialog`]).
//!
//! Rows don't carry who a conversation was sent to, so while the palette is open the
//! conversations it could act on are read from the store, without their bodies.

pub(crate) mod dialog;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use exo_commands::{Command, Region, SubPalette};
use exo_store::{
    Contact, Definitions, Label, LabelIndex, ListId, MessageDetail, Settings, SplitChanges,
    SplitInbox, SplitLibrary, SplitQuery, ThreadDetail, ThreadId,
};

use crate::{Mail, commands, mailbox, settings};

/// The palette rows' icon.
const ICON: &str = "data/split-inbox";
/// The sub-palettes' types.
const CREATE: &str = "CreateSplit";
const ADD: &str = "AddToSplit";
const NAME: &str = "SplitInbox";
/// The most conversations a split is made from.
const MOST: usize = 10;
/// Google Drive's senders besides `@docs.google.com`.
const DRIVE_SENDERS: [&str; 2] = [
    "drive-shares-dm-noreply@google.com",
    "drive-shares-noreply@google.com",
];
/// The definition of `New Split: Google Docs`.
const DOCS_QUERY: &str =
    "from:docs.google.com OR drive-shares-dm-noreply@google.com OR drive-shares-noreply@google.com";
/// The senders with splits of their own: each split's definition (`from:` the sender when `None`)
/// and name.
const LINEAR: &str = "from:notifications@mail.linear.app OR from:notifications@linear.app";
type Service = (
    &'static str,
    &'static [(Option<&'static str>, &'static str)],
);
const SERVICES: [Service; 4] = [
    (
        "notifications@github.com",
        &[
            (None, "GitHub Notifications"),
            (Some("to:mention@noreply.github.com"), "GitHub Mentions"),
        ],
    ),
    (
        "notifications@linear.app",
        &[(Some(LINEAR), "Linear Notifications")],
    ),
    (
        "notifications@mail.linear.app",
        &[(Some(LINEAR), "Linear Notifications")],
    ),
    ("notify@mail.notion.so", &[(None, "Notion Notifications")]),
];

/// What a split command asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `New Split Inbox` (`adding` false) or `Add to Split Inbox`, which lists the definitions.
    Open { adding: bool },
    /// A definition was picked.
    Define {
        adding: bool,
        definition: Definition,
    },
    /// The `Subject: “{subject}”` row in Edit Subject.
    Subject { adding: bool, subject: String },
    /// `Create Split Inbox “{name}”` (or `Go to “{name}” Split Inbox`).
    Create { name: String, query: String },
    /// The picked definition goes into split `split`.
    AddTo {
        split: String,
        definition: Definition,
    },
    /// `New Split Inbox from scratch`, which opens the New Split Inbox form.
    FromScratch,
    /// `New Split: Google Docs`.
    GoogleDocs,
    /// `New Split Inbox: {list or service}`, which opens the New Split dialog for this definition.
    OpenDialog(String),
    /// Something in the New Split dialog.
    Dialog(dialog::Event),
    /// `Please try another name`, which does nothing.
    Nothing,
}

/// A split definition the palette offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    /// The definition, as a search.
    pub query: String,
    /// What it's drawn from.
    pub kind: Kind,
    /// The senders or domains, which a VIP, News or Team split takes as its own (`items`).
    pub items: Vec<String>,
}

/// What a [`Definition`] is drawn from (`kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Search,
    Sender,
    Domain,
    Recipient,
    Subject,
    Label,
}

/// The split commands' state between frames.
#[derive(Default)]
pub(crate) struct NewSplit {
    /// Conversations read for the palette, by id (`None` for one the store doesn't have), and
    /// those being read.
    read: HashMap<String, Option<Arc<ThreadDetail>>>,
    reading: HashSet<String>,
    /// Definitions to offer once their conversations are read, and whether they're for adding.
    waiting: Option<bool>,
    /// The New Split dialog, while it's open.
    pub(crate) dialog: Option<dialog::State>,
}

/// The non-draft messages of a conversation, oldest first.
fn messages(detail: &ThreadDetail) -> impl DoubleEndedIterator<Item = &MessageDetail> {
    detail.messages.iter().filter(|message| !message.is_draft())
}

/// Puts each of `contacts` once into `into`, by address.
fn push_unique<'a>(into: &mut Vec<&'a Contact>, contacts: impl IntoIterator<Item = &'a Contact>) {
    for contact in contacts {
        if !into.iter().any(|known| known.email == contact.email) {
            into.push(contact);
        }
    }
}

/// `New Split Inbox with sender: …` or `Add sender to Split Inbox: …`, with an `s` on `what` when
/// `count` is more than one. `items` comes already joined.
fn title(adding: bool, what: &str, count: usize, items: &str) -> String {
    let plural = if count > 1 { "s" } else { "" };
    if adding {
        format!("Add {what}{plural} to Split Inbox: {items}")
    } else {
        format!("New Split Inbox with {what}{plural}: {items}")
    }
}

/// The definitions offered for `conversations`, after the search's (`search`, when it can define a
/// split). They are the senders (the focused message's, else each conversation's last), their
/// domains except webmail ones, everyone on To and Cc, a lone subject, then the labels one by one
/// and together.
pub(crate) fn definitions(
    adding: bool,
    search: Option<&str>,
    conversations: &[&ThreadDetail],
    focused: Option<&str>,
    labels: &[Label],
) -> Vec<(String, Definition)> {
    let definition =
        |query: String, kind: Kind, items: Vec<String>| Definition { query, kind, items };
    let mut rows = Vec::new();
    if let Some(search) = search {
        let row = if adding {
            format!("Add search to Split Inbox: {search}")
        } else {
            format!("New Split Inbox with search: {search}")
        };
        rows.push((row, definition(search.to_owned(), Kind::Search, Vec::new())));
    }
    let (mut senders, mut recipients): (Vec<&Contact>, Vec<&Contact>) = Default::default();
    let (mut domains, mut subjects, mut shown): (Vec<String>, Vec<&str>, Vec<&Label>) =
        Default::default();
    for detail in conversations {
        // The focused message's sender, you included.
        let focused = focused
            .filter(|_| conversations.len() == 1)
            .and_then(|id| detail.messages.iter().find(|message| message.id == id));
        let sender = match focused {
            Some(message) => message.from.as_ref(),
            None => messages(detail)
                .next_back()
                .and_then(|message| message.from.as_ref()),
        };
        if let Some(sender) = sender {
            push_unique(&mut senders, [sender]);
            let domain = sender
                .email
                .rsplit_once('@')
                .map_or("", |(_, domain)| domain.trim());
            if !domain.is_empty()
                && !crate::format::names::is_popular_domain(&domain.to_lowercase())
                && !domains
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(domain))
            {
                domains.push(domain.to_owned());
            }
        }
        for message in messages(detail) {
            push_unique(&mut recipients, message.to.iter().chain(&message.cc));
        }
        subjects.push(
            messages(detail)
                .next()
                .map_or("", |message| message.subject.as_str()),
        );
        // The visible labels other than Auto Labels: the user's labels, Trash and Spam.
        for label in labels
            .iter()
            .filter(|label| detail.labels.contains(&label.id))
        {
            let visible = matches!(label.id.as_str(), "TRASH" | "SPAM")
                || label.kind == exo_store::LabelKind::User
                    && mailbox::chip_name(&label.name).is_some()
                    && !mailbox::is_auto_label(&label.name);
            if visible && !shown.iter().any(|known| known.id == label.id) {
                shown.push(label);
            }
        }
    }
    if !senders.is_empty() {
        let emails: Vec<String> = senders.iter().map(|sender| sender.email.clone()).collect();
        let query = emails
            .iter()
            .map(|email| format!("from:{email}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        rows.push((
            title(adding, "sender", emails.len(), &emails.join(", ")),
            definition(query, Kind::Sender, emails),
        ));
    }
    if !domains.is_empty() {
        let query = domains
            .iter()
            .map(|domain| format!("from:{domain}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        rows.push((
            title(adding, "domain", domains.len(), &domains.join(", ")),
            definition(query, Kind::Domain, domains),
        ));
    }
    if !recipients.is_empty() {
        let emails: Vec<&str> = recipients
            .iter()
            .map(|recipient| recipient.email.as_str())
            .collect();
        let query = emails
            .iter()
            .map(|email| format!("to:{email}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        rows.push((
            title(adding, "recipient", emails.len(), &emails.join(", ")),
            definition(query, Kind::Recipient, Vec::new()),
        ));
    }
    if let [subject] = subjects
        .iter()
        .filter(|subject| subject.chars().count() > 1)
        .collect::<Vec<_>>()[..]
    {
        let row = if adding {
            format!("Add subject to Split Inbox: {subject}")
        } else {
            format!("New Split Inbox with subject: {subject}")
        };
        rows.push((
            row,
            definition((*subject).to_owned(), Kind::Subject, Vec::new()),
        ));
    }
    shown.sort_by(|a, b| mailbox::compare_label_names(&label_name(a), &label_name(b)));
    let label_row = |labels: &[&Label]| {
        let names: Vec<String> = labels.iter().map(|label| label_name(label)).collect();
        let query = names
            .iter()
            .map(|name| format!("label:\"{}\"", name.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR ");
        (
            title(adding, "label", names.len(), &names.join(" OR ")),
            definition(query, Kind::Label, Vec::new()),
        )
    };
    if shown.len() > 1 {
        rows.extend(shown.iter().map(|label| label_row(&[label])));
    }
    if !shown.is_empty() {
        rows.push(label_row(&shown));
    }
    rows
}

/// What a label is called. That's its Gmail name, except for Trash and Spam, which take the app's
/// names for them.
fn label_name(label: &Label) -> String {
    crate::triage_copy::label_name(&label.id, std::slice::from_ref(label)).to_owned()
}

/// A definition with brackets outside quotes, which a definition can't be added to.
fn is_complex(query: &str) -> bool {
    let mut quoted = false;
    for c in query.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => return true,
            _ => {}
        }
    }
    false
}

/// Whether a template's senders can be changed (`properties.senders.isEditable`), or `None` for a
/// split without a template.
fn editable_template(split: &SplitInbox, library: Option<&SplitLibrary>) -> Option<bool> {
    let id = split.template_id.as_deref()?;
    Some(
        library
            .and_then(|library| library.templates.get(id))
            .and_then(|template| template.properties.senders.as_ref())
            .is_some_and(|senders| senders.is_editable),
    )
}

/// Whether split `split` can take any definition.
fn can_take(split: &SplitInbox, library: Option<&SplitLibrary>) -> bool {
    editable_template(split, library).unwrap_or_else(|| {
        matches!(split.kind.as_deref(), Some("vip" | "team" | "news"))
            || !split.is_default() && !is_complex(split.query())
    })
}

/// Whether split `split` can take `kind`, once picked.
fn takes(split: &SplitInbox, kind: Kind, library: Option<&SplitLibrary>) -> bool {
    match split.kind.as_deref() {
        Some("calendar" | "shared") => false,
        Some("team") => kind == Kind::Domain,
        Some("vip" | "news") => matches!(kind, Kind::Sender | Kind::Domain),
        _ if is_complex(split.query()) => false,
        _ => editable_template(split, library).unwrap_or(true),
    }
}

/// `split` with `definition` added. A built-in VIP, News or Team split takes the senders or domains
/// as its own, and any other split becomes `{its definition} OR {the new one}`.
fn with_definition(
    split: &SplitInbox,
    definition: &Definition,
    definitions: Definitions<'_>,
) -> SplitInbox {
    let query = if split.is_default() {
        let mut items: Vec<String> = Vec::new();
        for item in settings::split_template::query_items(split.kind.as_deref(), split.query())
            .into_iter()
            .chain(definition.items.iter().cloned())
        {
            let trimmed = item.trim();
            if !trimmed.is_empty() && !items.iter().any(|kept| kept.eq_ignore_ascii_case(trimmed)) {
                items.push(trimmed.to_owned());
            }
        }
        let from = items
            .iter()
            .map(|item| format!("from:{item}"))
            .collect::<Vec<_>>();
        match split.kind.as_deref() {
            Some("team") if from.len() > 1 => format!("({}) to:me", from.join(" OR ")),
            Some("team") if !from.is_empty() => format!("{} to:me", from.join(" OR ")),
            _ => from.join(" OR "),
        }
    } else {
        format!("{} OR {}", split.query(), definition.query)
    };
    split.update(
        &SplitChanges {
            query: Some(query),
            ..SplitChanges::default()
        },
        definitions,
    )
}

/// The mailing list `message` came through. It comes from the `X-BeenThere` headers, the last
/// first, then from the `list {address};` in `Mailing-list`.
fn mailing_list(message: &MessageDetail) -> Option<String> {
    let header = |name: &'static str| {
        message
            .headers
            .iter()
            .filter(move |header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    };
    let been_there = header("X-BeenThere")
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        });
    let listed = header("Mailing-list")
        .filter_map(|value| {
            Some(
                value
                    .split_once("list ")?
                    .1
                    .split_once(';')?
                    .0
                    .trim()
                    .to_owned(),
            )
        })
        .filter(|address| crate::senders::is_plain_address(address));
    been_there.chain(listed).find(|address| !address.is_empty())
}

/// The palette's rows for `message`, which is the conversation's latest message from someone else
/// (or the focused one).
fn special_rows(message: &MessageDetail, settings: &Settings) -> Vec<Command<commands::Action>> {
    let act = |action: Action| commands::Action::NewSplit(action);
    let sender = message.from.as_ref().map_or("", |from| from.email.as_str());
    if sender.ends_with("@docs.google.com") || DRIVE_SENDERS.contains(&sender) {
        if settings
            .enabled_splits()
            .any(|split| split.query() == DOCS_QUERY)
        {
            return Vec::new();
        }
        return vec![
            Command::new(
                "GOOGLE_DOCS_SPLIT",
                "New Split: Google Docs",
                act(Action::GoogleDocs),
            )
            .aliases(["Split Inbox"])
            .default_score(1.15)
            .icon(ICON)
            .query_only()
            .regions(crate::splits::PALETTE),
        ];
    }
    if let Some((service, splits)) = SERVICES.iter().find(|(service, _)| *service == sender) {
        return splits
            .iter()
            .map(|(query, name)| {
                let query = query.map_or_else(|| format!("from:{service}"), str::to_owned);
                Command::new(
                    format!("{name}_SPLIT_COMMAND"),
                    format!("New Split Inbox: {name}"),
                    act(Action::OpenDialog(query)),
                )
                .aliases(["Create Split", "Split Inbox"])
                .default_score(1.15)
                .icon(ICON)
                .query_only()
                .regions(crate::splits::PALETTE)
            })
            .collect();
    }
    let list = mailing_list(message).filter(|list| {
        message
            .labels
            .iter()
            .any(|label| label == "CATEGORY_FORUMS")
            && message
                .to
                .iter()
                .chain(&message.cc)
                .any(|recipient| recipient.email == *list)
    });
    list.map(|list| {
        Command::new(
            "SPLIT_INBOX_ON_MAILING_LIST",
            format!("New Split Inbox: {list}"),
            act(Action::OpenDialog(list.clone())),
        )
        .aliases(["Create Split", "Split Inbox"])
        .default_score(0.8)
        .icon(ICON)
        .query_only()
        .regions(crate::splits::PALETTE)
    })
    .into_iter()
    .collect()
}

impl Mail {
    /// The conversations a split can be made from (drafts aside), by id, when there are no more
    /// than ten.
    fn split_sources(&self) -> Vec<String> {
        if let Some(thread) = &self.thread {
            return vec![thread.id().to_owned()];
        }
        let threads: Vec<String> = crate::actionable(self.thread.as_ref(), &self.inbox)
            .iter()
            .filter(|thread| !thread.draft_only)
            .map(|thread| thread.id.clone())
            .collect();
        if threads.len() > MOST {
            Vec::new()
        } else {
            threads
        }
    }

    /// The search on screen, when it can define a split.
    fn split_search(&self) -> Option<String> {
        let query = self.search.as_ref()?.query()?.trim().to_owned();
        (!query.is_empty()
            && SplitQuery::parse(&query)
                .invalidity(&self.label_index())
                .is_none())
        .then_some(query)
    }

    fn label_index(&self) -> LabelIndex {
        LabelIndex::new(
            [],
            self.snapshot
                .labels
                .iter()
                .map(|label| (label.id.as_str(), label.name.as_str())),
        )
    }

    /// Whether a split can be made, from a search that can define one or from one to ten
    /// conversations.
    fn can_create_split(&self) -> bool {
        self.split_search().is_some() || !self.split_sources().is_empty()
    }

    /// Reads the conversations the palette's split rows need while it's open, and forgets them
    /// once it's closed. Call every frame.
    pub(crate) fn new_split_frame(&mut self) {
        if self.commands.region() != Region::Command && self.new_split.waiting.is_none() {
            self.new_split.read.clear();
            self.new_split.reading.clear();
            return;
        }
        let mut wanted = self.split_sources();
        wanted.extend(
            self.thread
                .as_ref()
                .map(|thread| thread.id().to_owned())
                .or_else(|| self.inbox.cursor().map(|thread| thread.id.clone())),
        );
        for id in wanted {
            if self.new_split.read.contains_key(&id) || !self.new_split.reading.insert(id.clone()) {
                continue;
            }
            let ui = self.ui_sender();
            self.account.loader().run(
                move |store| async move {
                    let detail = store
                        .thread_headers(&ThreadId::new(id.clone()))
                        .await
                        .inspect_err(|error| log::error!("reading thread {id}: {error}"))
                        .ok()
                        .flatten();
                    (id, detail.map(Arc::new))
                },
                move |(id, detail)| {
                    ui.send(move |mail| mail.new_split_thread_read(id, detail));
                },
            );
        }
        self.maybe_open_definitions();
    }

    fn new_split_thread_read(&mut self, id: String, detail: Option<Arc<ThreadDetail>>) {
        if self.new_split.reading.remove(&id) {
            self.new_split.read.insert(id, detail);
        }
        self.maybe_open_definitions();
    }

    fn maybe_open_definitions(&mut self) {
        if let Some(adding) = self.new_split.waiting
            && self
                .split_sources()
                .iter()
                .all(|id| self.new_split.read.contains_key(id))
        {
            self.new_split.waiting = None;
            self.open_definitions(adding);
        }
    }

    /// Registers `New Split Inbox`, `Add to Split Inbox` and the cursor's conversation's own
    /// rows, once something is typed.
    pub(crate) fn register_new_split(&mut self) {
        let act = |action: Action| commands::Action::NewSplit(action);
        if self.new_split_dialog_open() {
            // Enter saves whatever has the keyboard, and Escape closes the dialog.
            const DIALOG: &[Region] = &[Region::Dialog];
            self.commands.add(
                Command::shortcut(
                    "CREATE_SPLIT_DIALOG_ENTER",
                    act(Action::Dialog(dialog::Event::Save)),
                )
                .key("enter")
                .regions(DIALOG),
            );
            self.commands.add(
                Command::shortcut(
                    "CREATE_SPLIT_DIALOG_ESCAPE",
                    act(Action::Dialog(dialog::Event::Close)),
                )
                .key("esc")
                .regions(DIALOG),
            );
        }
        if self.compose.pane().is_some() || self.commands.region() != Region::Command {
            return;
        }
        let settings = self.account.settings();
        let library = self.settings.split_library();
        let mut rows = Vec::new();
        if self.can_create_split() {
            if settings
                .enabled_splits()
                .any(|split| can_take(split, library))
            {
                rows.push(
                    Command::new(
                        "ADD_TO_SPLIT_INBOX",
                        "Add to Split Inbox",
                        act(Action::Open { adding: true }),
                    )
                    .default_score(1.1),
                );
            }
            rows.push(
                Command::new(
                    "CREATE_SPLIT_FROM_THREADS",
                    "New Split Inbox",
                    act(Action::Open { adding: false }),
                )
                .aliases(["Create Split"])
                .default_score(1.09),
            );
        }
        let rows = rows.into_iter().map(|row| {
            row.icon(ICON)
                .multiple()
                .query_only()
                .regions(crate::splits::PALETTE)
        });
        let special = self
            .special_message()
            .map(|message| special_rows(&message, settings))
            .unwrap_or_default();
        for row in rows.chain(special) {
            self.commands.add(row);
        }
    }

    /// The message the palette's special rows are for. In a thread it's the focused message when
    /// it's someone else's, and otherwise the conversation's latest message from someone else.
    fn special_message(&self) -> Option<MessageDetail> {
        let id = self
            .thread
            .as_ref()
            .map(|thread| thread.id().to_owned())
            .or_else(|| self.inbox.cursor().map(|thread| thread.id.clone()))?;
        let detail = self.new_split.read.get(&id)?.as_ref()?;
        let focused = self
            .thread
            .as_ref()
            .and_then(|thread| thread.focused())
            .and_then(|focused| detail.messages.iter().find(|message| message.id == focused))
            .filter(|message| !message.from.as_ref().is_some_and(|from| from.is_me));
        focused
            .or_else(|| {
                messages(detail)
                    .rev()
                    .find(|message| !message.from.as_ref().is_some_and(|from| from.is_me))
            })
            .cloned()
    }

    /// Carries out a split command's `action`.
    pub(crate) fn new_split_run(&mut self, action: Action, now: f64) {
        match action {
            Action::Open { adding } => {
                self.new_split.waiting = Some(adding);
                self.new_split_frame();
            }
            Action::Define { adding, definition } if definition.kind == Kind::Subject => {
                // The subject, as typed, becomes `subject:"…"`.
                let palette = SubPalette::new(
                    if adding { ADD } else { CREATE },
                    "Edit Subject",
                    ICON,
                    Vec::new(),
                )
                .initial(definition.query)
                .stage()
                .footer(move |subject| {
                    (!subject.is_empty()).then(|| {
                        Command::new(
                            "SUBJECT",
                            format!("Subject: “{subject}”"),
                            commands::Action::NewSplit(Action::Subject {
                                adding,
                                subject: subject.to_owned(),
                            }),
                        )
                    })
                });
                self.commands.open_sub_palette(palette);
            }
            Action::Define { adding, definition } => self.defined(adding, definition),
            Action::Subject { adding, subject } => self.defined(
                adding,
                Definition {
                    query: crate::subject_query(&subject),
                    kind: Kind::Subject,
                    items: Vec::new(),
                },
            ),
            Action::Create { name, query } => {
                self.inbox.clear_selection();
                self.create_split(&name, &query, now);
            }
            Action::AddTo { split, definition } => self.add_to_split(&split, &definition),
            Action::FromScratch => self.run(
                commands::Action::Settings(settings::Action::Split(settings::SplitEvent::New(
                    settings::SplitBack::Close,
                ))),
                now,
            ),
            Action::GoogleDocs => {
                let docs = self
                    .account
                    .settings()
                    .enabled_splits()
                    .any(|split| split.has_same_name("Docs"));
                self.create_split(if docs { "Google Docs" } else { "Docs" }, DOCS_QUERY, now);
            }
            Action::OpenDialog(query) => self.new_split.dialog = Some(dialog::State::new(query)),
            Action::Dialog(event) => self.new_split_dialog(event, now),
            Action::Nothing => {}
        }
    }

    /// Opens the definitions for the conversations read. When creating, the list ends in
    /// `New Split Inbox from scratch`.
    fn open_definitions(&mut self, adding: bool) {
        let sources = self.split_sources();
        let conversations: Vec<&ThreadDetail> = sources
            .iter()
            .filter_map(|id| self.new_split.read.get(id)?.as_deref())
            .collect();
        let focused = self.thread.as_ref().and_then(|thread| thread.focused());
        let found = definitions(
            adding,
            self.split_search().as_deref(),
            &conversations,
            focused,
            &self.snapshot.labels,
        );
        let mut rows: Vec<Command<commands::Action>> = found
            .into_iter()
            .enumerate()
            .map(|(at, (title, definition))| {
                Command::new(
                    format!("DEFINITION_{at}"),
                    title,
                    commands::Action::NewSplit(Action::Define { adding, definition }),
                )
            })
            .collect();
        if !adding {
            rows.push(Command::new(
                "NEW_FROM_SCRATCH",
                "New Split Inbox from scratch",
                commands::Action::NewSplit(Action::FromScratch),
            ));
        }
        let palette = if adding {
            SubPalette::new(ADD, "Add to Split Inbox", ICON, rows)
        } else {
            SubPalette::new(CREATE, "New Split Inbox", ICON, rows)
        };
        self.commands.open_sub_palette(palette);
    }

    /// A definition was picked. Once it's valid (senders and domains always are), this asks for
    /// the name to create it under, or for the split to add it to.
    fn defined(&mut self, adding: bool, definition: Definition) {
        if !matches!(definition.kind, Kind::Sender | Kind::Domain)
            && !self.valid_definition(&definition.query)
        {
            return;
        }
        if adding {
            self.open_add_to(definition);
        } else {
            self.open_naming(definition.query, String::new());
        }
    }

    /// Whether `query` can define a split. When it can't, a message says why.
    fn valid_definition(&mut self, query: &str) -> bool {
        match SplitQuery::parse(query).invalidity(&self.label_index()) {
            Some(invalid) => {
                self.pickers.inform("Sorry!", invalid.text);
                false
            }
            None => true,
        }
    }

    /// The naming step, with `name` typed so far. Its footer creates the split, goes to one just
    /// like it, or refuses a built-in split's name.
    fn open_naming(&mut self, query: String, name: String) {
        let splits: Vec<SplitInbox> = self.account.settings().splits().cloned().collect();
        let palette = SubPalette::new(NAME, "Split Inbox", ICON, Vec::new())
            .placeholder("Name")
            .initial(name)
            .footer(move |typed| {
                if typed.is_empty() {
                    return None;
                }
                // A split with the same name and an equivalent definition.
                let same = splits
                    .iter()
                    .find(|split| split.has_same_name(typed) && split.has_equivalent_query(&query));
                if exo_store::is_reserved_name(typed)
                    || exo_store::is_calendar_name(typed) && same.is_none()
                {
                    return Some(Command::new(
                        "TRY_ANOTHER_NAME",
                        "Please try another name",
                        commands::Action::NewSplit(Action::Nothing),
                    ));
                }
                let title = same.map_or_else(
                    || format!("Create Split Inbox “{typed}”"),
                    |split| format!("Go to “{}” Split Inbox", split.name()),
                );
                Some(Command::new(
                    "CREATE_SPLIT",
                    title,
                    commands::Action::NewSplit(Action::Create {
                        name: typed.to_owned(),
                        query: query.clone(),
                    }),
                ))
            });
        self.commands.open_sub_palette(palette);
    }

    /// Picks the split to add `definition` to, from the enabled splits that can take it.
    fn open_add_to(&mut self, definition: Definition) {
        let library = self.settings.split_library();
        let rows: Vec<Command<commands::Action>> = self
            .account
            .settings()
            .enabled_splits()
            .filter(|split| takes(split, definition.kind, library))
            .map(|split| {
                Command::new(
                    split.id.clone(),
                    split.name().to_owned(),
                    commands::Action::NewSplit(Action::AddTo {
                        split: split.id.clone(),
                        definition: definition.clone(),
                    }),
                )
            })
            .collect();
        self.commands
            .open_sub_palette(SubPalette::new(ADD, "Add to Split Inbox", ICON, rows));
    }

    /// Creates a split from the palette or the New Split dialog, and shows it. When a split of the
    /// same name exists, this shows that split (turned on if it was off) if its definition is the
    /// same, and otherwise asks whether to update it.
    pub(crate) fn create_split(&mut self, name: &str, query: &str, now: f64) {
        let query = query.trim();
        let existing = self
            .account
            .settings()
            .splits()
            .find(|split| split.has_same_name(name))
            .cloned();
        let wrap = commands::Action::Settings;
        let shown = match existing {
            Some(existing) if !existing.has_equivalent_query(query) => {
                self.pickers.ask_update_split(&existing, query);
                None
            }
            Some(existing) if existing.is_disabled => match settings::toggle_split(
                &mut self.account,
                &mut self.commands,
                &wrap,
                &existing,
                SplitChanges::default(),
                &self.list,
            ) {
                Ok(show) => show,
                Err(()) => {
                    self.pickers.split_too_long(self.settings.dialog_open());
                    None
                }
            },
            Some(existing) => Some(ListId::Split(existing.id)),
            None => {
                let (labels, me) = settings::split_definitions(&self.account);
                let split = SplitInbox::new(
                    name,
                    query,
                    None,
                    None,
                    Definitions {
                        labels: &labels,
                        me: &me,
                    },
                );
                if settings::add_split(&mut self.account, &mut self.commands, &wrap, &split) {
                    Some(ListId::Split(split.id))
                } else {
                    self.pickers.split_too_long(self.settings.dialog_open());
                    None
                }
            }
        };
        if let Some(list) = shown {
            self.run(commands::Action::Go(list), now);
        }
    }

    /// Yes in `Update existing Split Inbox?`. Split `id` takes definition `query` and shows.
    pub(crate) fn update_existing_split(&mut self, id: &str, query: &str, now: f64) {
        let Some(old) = self.account.settings().split(id).cloned() else {
            return;
        };
        let (labels, me) = settings::split_definitions(&self.account);
        let new = old.update(
            &SplitChanges {
                query: Some(query.to_owned()),
                ..SplitChanges::default()
            },
            Definitions {
                labels: &labels,
                me: &me,
            },
        );
        if settings::replace_split(
            &mut self.account,
            &mut self.commands,
            &commands::Action::Settings,
            &old,
            &new,
            false,
        ) {
            self.run(commands::Action::Go(ListId::Split(new.id)), now);
        } else {
            self.pickers.split_too_long(self.settings.dialog_open());
        }
    }

    /// No (or Escape) in `Update existing Split Inbox?`, which goes back to naming the split.
    pub(crate) fn rename_split(&mut self, name: String, query: String) {
        self.open_naming(query, name);
    }

    /// Adds `definition` to split `id`, and clears the selection.
    fn add_to_split(&mut self, id: &str, definition: &Definition) {
        let Some(old) = self.account.settings().split(id).cloned() else {
            return;
        };
        let (labels, me) = settings::split_definitions(&self.account);
        let new = with_definition(
            &old,
            definition,
            Definitions {
                labels: &labels,
                me: &me,
            },
        );
        if settings::replace_split(
            &mut self.account,
            &mut self.commands,
            &commands::Action::Settings,
            &old,
            &new,
            true,
        ) {
            self.inbox.clear_selection();
        } else {
            self.pickers.split_too_long(self.settings.dialog_open());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::{Body, LabelKind};

    fn contact(email: &str) -> Contact {
        Contact {
            name: None,
            email: email.into(),
            is_me: false,
        }
    }

    fn message(id: &str, from: &str, to: &[&str], subject: &str) -> MessageDetail {
        MessageDetail {
            id: id.into(),
            labels: Vec::new(),
            date: "2026-11-16T14:05:00Z".parse().expect("valid time"),
            from: Some(contact(from)),
            to: to.iter().map(|email| contact(email)).collect(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: subject.into(),
            snippet: String::new(),
            rfc822_id: None,
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: Body::default(),
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        }
    }

    fn thread(id: &str, labels: &[&str], messages: Vec<MessageDetail>) -> ThreadDetail {
        ThreadDetail {
            id: ThreadId::new(id),
            subject: messages[0].subject.clone(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            muted: false,
            reminder: None,
            messages,
        }
    }

    fn label(id: &str, name: &str) -> Label {
        Label {
            id: id.into(),
            name: name.into(),
            kind: LabelKind::User,
            text_color: None,
            background_color: None,
            hidden: false,
        }
    }

    fn titles(rows: &[(String, Definition)]) -> Vec<&str> {
        rows.iter().map(|(title, _)| title.as_str()).collect()
    }

    #[test]
    fn one_conversation_offers_its_sender_domain_recipients_subject_and_labels() {
        let detail = thread(
            "t1",
            &["INBOX", "Label_1", "Label_2"],
            vec![
                message(
                    "m1",
                    "rachel@calloway.co",
                    &["maya@brightmoor.co"],
                    "Q4 volumes",
                ),
                message(
                    "m2",
                    "dev@calloway.co",
                    &["maya@brightmoor.co", "ops@calloway.co"],
                    "Re: Q4 volumes",
                ),
            ],
        );
        let labels = [label("Label_1", "clients"), label("Label_2", "Atlas")];
        let rows = definitions(false, None, &[&detail], None, &labels);
        assert_eq!(
            titles(&rows),
            [
                "New Split Inbox with sender: dev@calloway.co",
                "New Split Inbox with domain: calloway.co",
                "New Split Inbox with recipients: maya@brightmoor.co, ops@calloway.co",
                "New Split Inbox with subject: Q4 volumes",
                "New Split Inbox with label: Atlas",
                "New Split Inbox with label: clients",
                "New Split Inbox with labels: Atlas OR clients",
            ]
        );
        assert_eq!(
            rows[0].1,
            Definition {
                query: "from:dev@calloway.co".into(),
                kind: Kind::Sender,
                items: vec!["dev@calloway.co".into()]
            }
        );
        assert_eq!(
            rows[2].1.query,
            "to:maya@brightmoor.co OR to:ops@calloway.co"
        );
        assert_eq!(rows[6].1.query, "label:\"Atlas\" OR label:\"clients\"");
        // A focused message's sender instead, and the adding titles.
        let rows = definitions(
            true,
            Some("from:calloway.co"),
            &[&detail],
            Some("m1"),
            &labels,
        );
        assert_eq!(
            titles(&rows)[..2],
            [
                "Add search to Split Inbox: from:calloway.co",
                "Add sender to Split Inbox: rachel@calloway.co"
            ]
        );
    }

    #[test]
    fn several_conversations_join_their_senders_and_drop_webmail_and_subjects() {
        let a = thread(
            "a",
            &[],
            vec![message(
                "m1",
                "tom@gmail.com",
                &["maya@brightmoor.co"],
                "One",
            )],
        );
        let b = thread(
            "b",
            &[],
            vec![message(
                "m2",
                "jess@harbor.io",
                &["maya@brightmoor.co"],
                "Two",
            )],
        );
        let rows = definitions(false, None, &[&a, &b], None, &[]);
        assert_eq!(
            titles(&rows),
            [
                "New Split Inbox with senders: tom@gmail.com, jess@harbor.io",
                "New Split Inbox with domain: harbor.io",
                "New Split Inbox with recipient: maya@brightmoor.co"
            ]
        );
        assert_eq!(rows[0].1.query, "from:tom@gmail.com OR from:jess@harbor.io");
    }

    fn split(name: &str, query: &str, kind: Option<&str>) -> SplitInbox {
        SplitInbox::new(
            name,
            query,
            kind,
            None,
            Definitions {
                labels: &[],
                me: &[],
            },
        )
    }

    #[test]
    fn splits_take_what_their_kind_allows() {
        assert!(takes(
            &split("Atlas", "from:atlas.dev", None),
            Kind::Recipient,
            None
        ));
        assert!(
            !takes(
                &split("Atlas", "(from:atlas.dev) to:me", None),
                Kind::Sender,
                None
            ),
            "a complex definition"
        );
        assert!(
            takes(
                &split("Atlas", "subject:\"(draft)\"", None),
                Kind::Sender,
                None
            ),
            "brackets in quotes"
        );
        assert!(
            takes(&split("VIP", "", Some("vip")), Kind::Domain, None)
                && !takes(&split("VIP", "", Some("vip")), Kind::Label, None)
        );
        assert!(
            takes(&split("Team", "", Some("team")), Kind::Domain, None)
                && !takes(&split("Team", "", Some("team")), Kind::Sender, None)
        );
        assert!(!takes(
            &split("Calendar", exo_store::CALENDAR_QUERY, Some("calendar")),
            Kind::Sender,
            None
        ));
    }

    #[test]
    fn adding_appends_to_a_definition_or_a_built_in_splits_senders() {
        let definitions = Definitions {
            labels: &[],
            me: &[],
        };
        let sender = Definition {
            query: "from:dev@calloway.co".into(),
            kind: Kind::Sender,
            items: vec!["dev@calloway.co".into()],
        };
        assert_eq!(
            with_definition(
                &split("Atlas", "from:atlas.dev", None),
                &sender,
                definitions
            )
            .query(),
            "from:atlas.dev OR from:dev@calloway.co"
        );
        assert_eq!(
            with_definition(
                &split("VIP", "from:a@b.co", Some("vip")),
                &sender,
                definitions
            )
            .query(),
            "from:a@b.co OR from:dev@calloway.co"
        );
        let domain = Definition {
            query: "from:calloway.co".into(),
            kind: Kind::Domain,
            items: vec!["calloway.co".into()],
        };
        assert_eq!(
            with_definition(
                &split("Team", "from:brightmoor.co to:me", Some("team")),
                &domain,
                definitions
            )
            .query(),
            "(from:brightmoor.co OR from:calloway.co) to:me"
        );
    }

    #[test]
    fn a_service_or_a_mailing_list_offers_its_own_split() {
        let settings = Settings::default();
        let titles = |message: &MessageDetail| {
            special_rows(message, &settings)
                .iter()
                .map(|row| row.title().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(&message(
                "m",
                "notifications@github.com",
                &["maya@brightmoor.co"],
                "PR"
            )),
            [
                "New Split Inbox: GitHub Notifications",
                "New Split Inbox: GitHub Mentions"
            ]
        );
        assert_eq!(
            titles(&message(
                "m",
                "comments-noreply@docs.google.com",
                &[],
                "Doc"
            )),
            ["New Split: Google Docs"]
        );
        let mut list = message(
            "m",
            "someone@lists.example",
            &["rust-users@lists.example"],
            "Hi",
        );
        list.headers = vec![exo_store::Header {
            name: "X-BeenThere".into(),
            value: "rust-users@lists.example".into(),
        }];
        assert!(titles(&list).is_empty(), "only for forums mail");
        list.labels = vec!["CATEGORY_FORUMS".into()];
        assert_eq!(titles(&list), ["New Split Inbox: rust-users@lists.example"]);
    }

    #[test]
    fn definitions_skips_empty_domain_and_with_definition_deduplicates_items() {
        let mut msg = message("m", "no-at-sign", &["maya@brightmoor.co"], "Hi");
        let detail = ThreadDetail {
            id: ThreadId::new("t-no-at"),
            subject: "Hi".into(),
            labels: vec!["INBOX".into()],
            muted: false,
            reminder: None,
            messages: vec![msg.clone()],
        };
        let rows = definitions(false, None, &[&detail], None, &[]);
        assert!(
            !rows.iter().any(|(title, _)| title.contains("with domain:")),
            "at-less sender must not offer empty domain: {rows:?}"
        );

        msg.from = Some(Contact {
            name: None,
            email: "trailing@".into(),
            is_me: false,
        });
        let trailing = ThreadDetail {
            messages: vec![msg],
            ..detail
        };
        let rows = definitions(false, None, &[&trailing], None, &[]);
        assert!(
            !rows.iter().any(|(title, _)| title.contains("with domain:")),
            "trailing-@ sender must not offer empty domain: {rows:?}"
        );

        let defs = Definitions {
            labels: &[],
            me: &[],
        };
        let dup = Definition {
            query: "from:priya@brightmoor.co OR from:new@brightmoor.co".into(),
            kind: Kind::Sender,
            items: vec![
                "PRIYA@brightmoor.co".into(),
                "new@brightmoor.co".into(),
                "NEW@BRIGHTMOOR.CO".into(),
            ],
        };
        assert_eq!(
            with_definition(
                &split("VIP", "from:priya@brightmoor.co", Some("vip")),
                &dup,
                defs
            )
            .query(),
            "from:priya@brightmoor.co OR from:new@brightmoor.co"
        );
    }

    #[test]
    fn subject_action_escapes_embedded_quotes_via_subject_query() {
        assert_eq!(
            crate::subject_query("15\" MacBook OR in:trash"),
            "subject:\"15\" subject:\"MacBook OR in:trash\""
        );
    }
}
