//! Search: the local full-text index, looking a query up in it, and Gmail's search for what
//! isn't stored or isn't matched locally, with the two merged.
//!
//! Every value a user types reaches SQLite as a bound parameter, and reaches FTS5 only as a
//! bare alphanumeric term or a quoted string (`crate::query`). The SQL text is built from fixed
//! fragments alone.
//!
//! The index is `search_fts`, made by the migrations (`migrations/0001_baseline.sql`). It has
//! one row per thread, with a rowid derived from the thread id (`row_id`). `sort` is the date of
//! the thread's latest message. `messages` lists the messages whose text `content` holds (those
//! with a body stored) and the revisions of Exo's drafts it holds. So a label change doesn't
//! re-read bodies, but a body arriving or a draft being saved does.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::LazyLock;

use gmail::ListQuery;
use regex::Regex;
use toasty::Executor;
use toasty::stmt::Value;

use crate::db::{AttachmentRow, BodyRow, IN_LIST_CHUNK, LabelRow, ListCursor, Mailbox, query_rows};
use crate::drafts::EditableDraft;
use crate::error::Result;
use crate::ids::ListId;
use crate::ids::labels::{INBOX, SH_ALL, SH_IMPORTANT, SH_OTHER, TRASH};
use crate::model::ThreadSummary;
use crate::parse::unescape_html;
use crate::provider;
use crate::query::{FTS_ANCHOR, LabelIndex, Recheck, SearchQuery, tokenize_email};
use crate::quota::Lane;
use crate::split::{Me, important_definition};
use crate::store::{Store, load_attachments_by_thread, load_views, reindex};
use crate::view::{Router, ThreadView};

/// `snippet()`'s column numbers.
const SUBJECT_COLUMN: i64 = 3;
const CONTENT_COLUMN: i64 = 4;
/// The index's highlight markers and ellipsis, which are private-use characters no mail contains.
const MARK_START: char = '\u{e550}';
const MARK_END: char = '\u{e551}';
const ELLIPSIS: char = '\u{e553}';
/// What separates the parts of a column (`ARRAY_JOINER`), and drafts' text from the rest
/// (`DRAFT_JOINER`), so a phrase never spans two.
const ARRAY_JOINER: &str = " \u{f8f8} ";
const DRAFT_JOINER: &str = " \u{f8f5} ";
/// A message's text past this many characters isn't indexed.
const MAX_MESSAGE_TEXT: usize = 100_000;
/// A subject snippet's length, in tokens.
const SUBJECT_TOKENS: i64 = 64;
/// The size of Gmail's first page of matches, then of every page after it.
const FIRST_GMAIL_PAGE: u32 = 10;
const GMAIL_PAGE: u32 = 100;

/// A thread's row id, which is a stable 64-bit hash of its id (FNV-1a), so updating a thread's
/// row is a lookup by rowid.
fn row_id(thread: &str) -> i64 {
    let hash = gmail::mime::fnv1a(thread.as_bytes());
    i64::from_ne_bytes(hash.to_ne_bytes())
}

/// Drops `thread` from the index.
pub(crate) async fn unindex(exec: &mut dyn Executor, thread: &str) -> Result<()> {
    toasty::sql::statement("DELETE FROM search_fts WHERE rowid = ?1")
        .bind(row_id(thread))
        .exec(exec)
        .await?;
    Ok(())
}

/// Writes `view`'s row: its subject, every message's text, its people, its attachments' names,
/// its labels and lists (`list_ids`), mailing lists and Message-IDs. A thread that is only a
/// draft written elsewhere isn't searchable, because such drafts only show in Drafts.
pub(crate) async fn index(
    exec: &mut dyn Executor,
    view: &ThreadView,
    list_ids: &[&str],
    attachments: &[AttachmentRow],
) -> Result<()> {
    if view.messages.is_empty() {
        return unindex(exec, &view.id).await;
    }
    let row = row_id(&view.id);
    let ids: Vec<&str> = view.messages.iter().map(|m| m.row.id.as_str()).collect();
    // A draft being written belongs to compose, so its draft record's text stands in for its
    // message's text.
    let drafts = crate::drafts::editable(exec, &view.id).await?;
    let others: Vec<&str> = ids
        .iter()
        .filter(|id| {
            !drafts
                .iter()
                .any(|draft| draft.message_id.as_deref() == Some(**id))
        })
        .copied()
        .collect();
    let mut with_body = BTreeSet::new();
    for chunk in others.chunks(IN_LIST_CHUNK) {
        with_body.extend(
            BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
                .select(BodyRow::fields().message_id())
                .exec(exec)
                .await?,
        );
    }
    let bodied: Vec<&str> = others
        .into_iter()
        .filter(|id| with_body.contains(*id))
        .collect();
    let messages = bodied
        .iter()
        .map(|id| (*id).to_owned())
        .chain(
            drafts
                .iter()
                .map(|draft| format!("{}@{}", draft.id, draft.revision)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let stored = query_rows::<(String, String)>(
        exec,
        toasty::sql::query("SELECT messages, content FROM search_fts WHERE rowid = ?1").bind(row),
    )
    .await?;
    let content = match stored.into_iter().next() {
        Some((indexed, content)) if indexed == messages => content,
        _ => content(exec, view, &bodied, &drafts).await?,
    };
    let contacts = |field: fn(&crate::db::MessageRow) -> Vec<&Mailbox>| {
        let entries: BTreeSet<String> = view
            .messages
            .iter()
            .flat_map(|m| field(&m.row))
            .map(|mailbox| {
                let name = mailbox.name.as_deref().unwrap_or_default();
                collapse(&format!(
                    "{} {name} {}",
                    mailbox.email,
                    tokenize_email(&mailbox.email)
                ))
            })
            .collect();
        entries.into_iter().collect::<Vec<_>>().join(ARRAY_JOINER)
    };
    let delivered_to: Vec<Mailbox> = view
        .messages
        .iter()
        .flat_map(|m| {
            m.row
                .headers
                .iter()
                .filter(|header| header.name.eq_ignore_ascii_case("Delivered-To"))
        })
        .map(|header| Mailbox {
            name: None,
            email: header.value.trim().to_owned(),
        })
        .collect();
    let delivered = delivered_to
        .iter()
        .map(|mailbox| format!("{} {}", mailbox.email, tokenize_email(&mailbox.email)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(ARRAY_JOINER);
    let files: Vec<&str> = attachments
        .iter()
        .filter(|row| !row.inline && ids.contains(&row.message_id.as_str()))
        .map(|row| row.filename.as_str())
        .collect();
    let labels: BTreeSet<&str> = list_ids.iter().copied().chain(view.labels()).collect();
    let mut meta = vec![FTS_ANCHOR];
    if !files.is_empty() {
        meta.push("attachment");
    }
    let joined = |values: Vec<String>| {
        values
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(ARRAY_JOINER)
    };
    let sort = view
        .messages
        .iter()
        .map(|m| m.row.internal_date)
        .max()
        .unwrap_or_default();
    unindex(exec, &view.id).await?;
    toasty::sql::statement(
        r#"INSERT INTO search_fts (rowid, thread_id, sort, messages, subject, content, "from", "to", cc, bcc, replyto, deliveredto, attachments, labels, list, rfc822msgid, meta) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"#,
    )
    .bind(row)
    .bind(view.id.clone())
    .bind(sort)
    .bind(messages)
    .bind(collapse(&view.subject()))
    .bind(content)
    .bind(contacts(|row| row.sender.0.iter().collect()))
    .bind(contacts(|row| row.to.iter().collect()))
    .bind(contacts(|row| row.cc.iter().collect()))
    .bind(contacts(|row| row.bcc.iter().collect()))
    .bind(contacts(|row| row.reply_to.iter().collect()))
    .bind(delivered)
    .bind(collapse(&files.join(ARRAY_JOINER)))
    .bind(labels.into_iter().collect::<Vec<_>>().join(ARRAY_JOINER))
    .bind(joined(view.messages.iter().filter_map(|m| m.row.list_id.as_deref().map(collapse)).collect()))
    .bind(joined(view.messages.iter().filter_map(|m| m.row.rfc822_id.clone()).collect()))
    .bind(meta.join(" "))
    .exec(exec)
    .await?;
    Ok(())
}

/// The text of every message (`content`), received mail first, then drafts' text. The messages
/// in `ids` have their bodies stored. `drafts` are being written, and their text is as compose
/// last saved it.
async fn content(
    exec: &mut dyn Executor,
    view: &ThreadView,
    ids: &[&str],
    drafts: &[EditableDraft],
) -> Result<String> {
    let mut bodies: HashMap<String, BodyRow> = HashMap::new();
    for chunk in ids.chunks(IN_LIST_CHUNK) {
        for body in BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
            .exec(exec)
            .await?
        {
            bodies.insert(body.message_id.clone(), body);
        }
    }
    let bodies = &bodies;
    let text = |draft: bool| {
        view.messages
            .iter()
            .filter(move |m| m.is_draft() == draft)
            .filter_map(move |m| bodies.get(&m.row.id))
            .map(body_text)
    };
    let joined = |texts: Vec<String>| {
        texts
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(ARRAY_JOINER)
    };
    let drafts = text(true)
        .chain(
            drafts
                .iter()
                .map(|draft| whole_text(Some(&draft.body), None)),
        )
        .collect();
    Ok(format!(
        "{}{DRAFT_JOINER}{}",
        joined(text(false).collect()),
        joined(drafts)
    ))
}

/// What the index holds of a stored body. That's the text a reader sees, without the quoted
/// reply and the signature (`crate::text`), or the body's whole text until that's worked out.
fn body_text(body: &BodyRow) -> String {
    match &body.visible {
        Some(visible) => truncated(collapse(visible)),
        None => whole_text(body.html.as_deref(), body.text.as_deref()),
    }
}

/// A body's whole text, which is the HTML's text without its styles and scripts, or else the
/// plain-text part.
pub(crate) fn whole_text(html: Option<&str>, text: Option<&str>) -> String {
    static HIDDEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)<style\b[^>]*>.*?</style\s*>|<script\b[^>]*>.*?</script\s*>|<head\b[^>]*>.*?</head\s*>|<title\b[^>]*>.*?</title\s*>|<!--.*?-->",
        )
        .expect("valid regex")
    });
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("valid regex"));
    let text = match (html, text) {
        (Some(html), _) => unescape_html(&TAG.replace_all(&HIDDEN.replace_all(html, " "), " ")),
        (None, Some(text)) => text.to_owned(),
        (None, None) => String::new(),
    };
    truncated(collapse(&text))
}

/// `text` up to the [`MAX_MESSAGE_TEXT`] characters the index takes of a message.
fn truncated(text: String) -> String {
    match text.char_indices().nth(MAX_MESSAGE_TEXT) {
        Some((at, _)) => text[..at].to_owned(),
        None => text,
    }
}

/// `text` with each whitespace run collapsed to one space, trimmed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text with some of it in bold: a search result's subject or snippet with the words that
/// matched highlighted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Highlighted {
    /// The text.
    pub text: String,
    /// Byte ranges of `text` shown in bold, in order and apart.
    pub bold: Vec<Range<usize>>,
}

impl Highlighted {
    /// `marked` with the text between [`MARK_START`] and [`MARK_END`] in bold.
    fn from_marked(marked: &str) -> Self {
        let mut highlighted = Self::default();
        let mut start = None;
        for c in marked.chars() {
            match c {
                MARK_START => start = Some(highlighted.text.len()),
                MARK_END => {
                    if let Some(start) = start.take() {
                        highlighted.bold.push(start..highlighted.text.len());
                    }
                }
                c => highlighted.text.push(c),
            }
        }
        highlighted
    }

    /// Every whole-word occurrence of `terms` in bold, ignoring case.
    fn terms(text: &str, terms: &[String]) -> Self {
        let patterns = terms.iter().filter_map(|term| {
            Regex::new(&format!(r"(?i)(?-u:\b){}(?-u:\b)", regex::escape(term))).ok()
        });
        Self::bold_matches(text, patterns)
    }

    /// `text` with what positive `rechecks` match in bold, and nothing else, which redoes the
    /// index's highlights. `subject_only` rechecks only apply when `for_subject` is set.
    fn matching(text: &str, rechecks: &[Recheck], for_subject: bool) -> Self {
        Self::bold_matches(
            text,
            rechecks
                .iter()
                .filter(|recheck| !recheck.negated && (for_subject || !recheck.subject_only))
                .map(|recheck| recheck.regex.clone()),
        )
    }

    /// `text` with every match of `patterns` in bold, overlapping ones merged.
    fn bold_matches(text: &str, patterns: impl Iterator<Item = Regex>) -> Self {
        let mut bold: Vec<Range<usize>> = patterns
            .flat_map(|pattern| {
                pattern
                    .find_iter(text)
                    .map(|found| found.range())
                    .collect::<Vec<_>>()
            })
            .collect();
        bold.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in bold {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        Self {
            text: text.to_owned(),
            bold: merged,
        }
    }

    fn has_bold(&self) -> bool {
        !self.bold.is_empty()
    }
}

/// One conversation a search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Its row, dated by the result's sort time.
    pub thread: ThreadSummary,
    /// Its subject with the matched words in bold, when the query has words to match. Otherwise
    /// the row's subject shows.
    pub subject: Option<Highlighted>,
    /// The excerpt that matched, in place of the row's snippet.
    pub snippet: Option<Highlighted>,
}

/// What a search has found so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchPage {
    /// The conversations, newest first.
    pub results: Vec<SearchResult>,
    /// Whether the local index has more past the limit.
    pub more_local: bool,
    /// The label ids the query asks for, which the label chips on results leave out.
    pub labels: Vec<String>,
    /// The words to highlight in results' names, where a name or address matches.
    pub names: Vec<String>,
}

/// A message Gmail's search found (`messages.list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailHit {
    thread: String,
    message: String,
}

/// One page of Gmail's matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GmailPage {
    /// The matching messages, newest first.
    pub hits: Vec<GmailHit>,
    /// The next page, if Gmail has more.
    pub next: Option<String>,
}

/// A search in progress: the query, when it started, Gmail's matches so far, and each result's
/// sort time, subject and excerpt. Those three are whatever the result was first found with, so
/// finding it again keeps its sort time and changes nothing.
#[derive(Debug, Clone)]
pub struct Search {
    query: SearchQuery,
    started: i64,
    gmail: Vec<GmailHit>,
    next_page: Option<String>,
    gmail_done: bool,
    found: HashMap<String, Found>,
    /// Whether the search runs ahead of being asked for (instant search), so Gmail's first page
    /// waits behind what's on screen.
    preloaded: bool,
}

/// How a result was first found.
#[derive(Debug, Clone)]
struct Found {
    sort: i64,
    subject: Option<Highlighted>,
    snippet: Option<Highlighted>,
}

impl Search {
    /// A search for `raw`, starting now.
    pub fn new(raw: &str) -> Self {
        Self {
            query: SearchQuery::parse(raw),
            started: jiff::Timestamp::now().as_millisecond(),
            gmail: Vec::new(),
            next_page: None,
            gmail_done: false,
            found: HashMap::new(),
            preloaded: false,
        }
    }

    /// A search for `raw` run ahead of being asked for. Its first page of Gmail's matches goes in
    /// [`Lane::Show`], after what the user is waiting on.
    pub fn preload(raw: &str) -> Self {
        Self {
            preloaded: true,
            ..Self::new(raw)
        }
    }

    /// Marks the preloaded search as asked for. A first page it hasn't asked Gmail for yet then
    /// goes in the same lane as any other search's.
    pub fn ask(&mut self) {
        self.preloaded = false;
    }

    /// The query.
    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// Whether Gmail has no more matches to give.
    pub fn gmail_done(&self) -> bool {
        self.gmail_done
    }

    /// Where Gmail's next page starts (`None` before the first).
    pub fn next_page(&self) -> Option<&str> {
        self.next_page.as_deref()
    }

    /// How many matches Gmail has given.
    pub fn gmail_hits(&self) -> usize {
        self.gmail.len()
    }

    /// Adds a page of Gmail's matches.
    pub fn add_gmail(&mut self, page: GmailPage) {
        self.gmail_done = page.next.is_none();
        self.next_page = page.next;
        for hit in page.hits {
            if !self.gmail.contains(&hit) {
                self.gmail.push(hit);
            }
        }
    }
}

impl Store {
    /// The conversations `search` has found, newest first. That's up to `limit` from the local
    /// index, then the matches from Gmail that the index didn't find. When `online`, the local
    /// results only go back as far as All Mail is stored, since Gmail finds the older ones.
    /// `snippet_tokens` is how long an excerpt is, in words (10, 15, 26 or 35, depending on the
    /// window's width).
    ///
    /// # Errors
    /// On database failures.
    pub async fn search(
        &self,
        search: &mut Search,
        limit: usize,
        online: bool,
        snippet_tokens: i64,
    ) -> Result<SearchPage> {
        let mut db = self.inner.db.clone();
        let router = self.inner.router();
        let labels = label_index(&mut db).await?;
        let me = &router.me.addresses;
        let query = &search.query;
        let fts = query.fts(&labels, me);
        let mut sql = String::from(
            "SELECT thread_id, sort, snippet(search_fts, ?1, char(58704), char(58705), char(58707), ?2), snippet(search_fts, ?3, char(58704), char(58705), char(58707), ?4) FROM search_fts WHERE search_fts MATCH ?5",
        );
        let mut params: Vec<Value> = vec![
            SUBJECT_COLUMN.into(),
            SUBJECT_TOKENS.into(),
            CONTENT_COLUMN.into(),
            snippet_tokens.into(),
        ];
        for (at, (negated, expression)) in fts.matches.iter().enumerate() {
            params.push(expression.clone().into());
            if at > 0 {
                let not = if *negated { "NOT " } else { "" };
                sql.push_str(&format!(
                    " AND rowid {not}IN (SELECT rowid FROM search_fts WHERE search_fts MATCH ?{})",
                    params.len()
                ));
            }
        }
        for (before, date) in &fts.dates {
            params.push((*date).into());
            sql.push_str(&format!(
                " AND sort {} ?{}",
                if *before { "<" } else { ">=" },
                params.len()
            ));
        }
        // Online, local results stop where All Mail's backfill does, unless the query asks for
        // drafts or snippets.
        if online && !query.shows_label("draft") && !query.shows_label("snippets") {
            let all = ListCursor::filter_by_id(SH_ALL)
                .first()
                .exec(&mut db)
                .await?;
            let bound = match all {
                Some(cursor) if cursor.complete => None,
                Some(cursor) => Some(cursor.min_sort.unwrap_or(search.started)),
                None => Some(search.started),
            };
            if let Some(bound) = bound {
                params.push(bound.into());
                sql.push_str(&format!(" AND sort >= ?{}", params.len()));
            }
        }
        params.push(i64::try_from(limit + 1).unwrap_or(i64::MAX).into());
        sql.push_str(&format!(
            " ORDER BY sort DESC, rowid DESC LIMIT ?{}",
            params.len()
        ));
        let mut statement = toasty::sql::query(sql);
        for param in params {
            statement = statement.bind(param);
        }
        let rows =
            query_rows::<(String, Option<i64>, Option<String>, Option<String>)>(&mut db, statement)
                .await?;
        let mut found: Vec<(String, Option<Highlighted>, Option<Highlighted>)> = Vec::new();
        let highlightable = !query.highlightable_tokens().is_empty();
        for (thread, _, subject, snippet) in rows {
            let subject = subject
                .filter(|_| highlightable)
                .map(|subject| Highlighted::from_marked(&subject))
                .filter(Highlighted::has_bold);
            let snippet = snippet
                .filter(|_| highlightable)
                .and_then(|snippet| pick_snippet(&snippet));
            found.push((thread, subject, snippet));
        }
        let more_local = found.len() > limit;
        found.truncate(limit);

        let ids: Vec<String> = found
            .iter()
            .map(|(id, ..)| id.clone())
            .chain(search.gmail.iter().map(|hit| hit.thread.clone()))
            .collect();
        let mut views = load_views(&mut db, &ids).await?;
        let attachments_by_thread = load_attachments_by_thread(&mut db, &views).await?;
        let included = query.included_labels(&labels);
        let keeps_trash = query.shows_label("trash");
        let (rechecks, rehighlight) = query.rechecks();
        let list = if query.shows_label("sent") && included.len() == 1 {
            ListId::Sent
        } else {
            ListId::All
        };
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();
        for (id, subject, snippet) in found {
            // A thread that Gmail found too, and that is new to the index since the search
            // started, was stored by that Gmail search. It shows as Gmail found it, not as an
            // index hit.
            if !search.found.contains_key(&id) && search.gmail.iter().any(|hit| hit.thread == id) {
                continue;
            }
            let Some(view) = views.get_mut(&id) else {
                continue;
            };
            let attachments = attachments_by_thread
                .get(&id)
                .map_or(&[][..], Vec::as_slice);
            if query.checks_contacts()
                && query.is_match(view, attachments, &labels, me) == Some(false)
            {
                continue;
            }
            let mut subject = subject;
            let mut snippet = snippet.unwrap_or_else(|| Highlighted {
                text: latest_snippet(view),
                bold: Vec::new(),
            });
            if !rechecks.is_empty() {
                let searched = subject.as_ref().map_or("", |subject| subject.text.as_str());
                if !recheck(&mut db, view, &rechecks, &snippet.text, searched).await? {
                    continue;
                }
                if rehighlight {
                    snippet = Highlighted::matching(&snippet.text, &rechecks, false);
                    let target = subject
                        .as_ref()
                        .map(|subject| subject.text.clone())
                        .unwrap_or_else(|| view.subject());
                    subject = (!target.is_empty())
                        .then(|| Highlighted::matching(&target, &rechecks, true))
                        .filter(Highlighted::has_bold);
                } else if subject.is_none()
                    && rechecks
                        .iter()
                        .any(|recheck| recheck.subject_only && !recheck.negated)
                {
                    let target = view.subject();
                    subject = (!target.is_empty())
                        .then(|| Highlighted::matching(&target, &rechecks, true))
                        .filter(Highlighted::has_bold);
                }
            }
            let first = search.found.entry(id.clone()).or_insert_with(|| Found {
                sort: sort_for_search(view, search.started),
                subject,
                snippet: Some(snippet),
            });
            seen.insert(id.clone());
            results.push(SearchResult {
                thread: summary(
                    view,
                    &list,
                    first.sort,
                    &router,
                    attachments,
                    &included,
                    keeps_trash,
                ),
                subject: first.subject.clone(),
                snippet: first.snippet.clone(),
            });
        }
        let terms = query.highlightable_tokens();
        for hit in &search.gmail {
            if seen.contains(&hit.thread) {
                continue;
            }
            let Some(view) = views.get_mut(&hit.thread) else {
                continue;
            };
            let attachments = attachments_by_thread
                .get(&hit.thread)
                .map_or(&[][..], Vec::as_slice);
            if view.messages.is_empty() || query.excludes(view, attachments, &labels, me) {
                continue;
            }
            seen.insert(hit.thread.clone());
            let first = search.found.entry(hit.thread.clone()).or_insert_with(|| {
                let message = view
                    .messages
                    .iter()
                    .find(|m| m.row.id == hit.message)
                    .or_else(|| view.messages.first());
                Found {
                    sort: message.map_or_else(|| view.gmail_sort(), |m| m.row.internal_date),
                    subject: Some(Highlighted::terms(&view.subject(), &terms))
                        .filter(Highlighted::has_bold),
                    snippet: message.map(|m| {
                        Highlighted::terms(&collapse(&unescape_html(&m.row.snippet)), &terms)
                    }),
                }
            });
            results.push(SearchResult {
                thread: summary(
                    view,
                    &list,
                    first.sort,
                    &router,
                    attachments,
                    &included,
                    keeps_trash,
                ),
                subject: first.subject.clone(),
                snippet: first.snippet.clone(),
            });
        }
        results.sort_by(|a, b| {
            b.thread
                .date
                .cmp(&a.thread.date)
                .then_with(|| a.thread.id.cmp(&b.thread.id))
        });
        Ok(SearchPage {
            results,
            more_local,
            labels: included,
            names: query.highlightable_contact_tokens(),
        })
    }

    /// The next page of Gmail's matches for `search`, from `messages.list` with the query's Gmail
    /// form, ten at first and a hundred after. Conversations not stored yet are fetched for their
    /// rows and stored (and so indexed) on the way. Storing them doesn't tick
    /// [`changes`](Self::changes). Read the search again once the page is added to it, so that
    /// they show as Gmail found them rather than as index hits.
    ///
    /// # Errors
    /// [`Error::Offline`](crate::Error::Offline) and the like when Gmail can't be reached.
    pub async fn search_gmail(&self, search: &Search) -> Result<GmailPage> {
        let inner = &self.inner;
        inner.bootstrapped().await?;
        let mut db = inner.db.clone();
        let router = inner.router();
        let labels = label_index(&mut db).await?;
        let me = Me {
            addresses: &router.me.addresses,
            name: router.me.name.as_deref(),
        };
        let q = search
            .query
            .to_gmail(&labels, &router.me.addresses, &important_definition(&me));
        let query = ListQuery {
            q: Some(q),
            include_spam_trash: search.query.includes_spam_trash(),
            max_results: Some(if search.next_page.is_some() {
                GMAIL_PAGE
            } else {
                FIRST_GMAIL_PAGE
            }),
            page_token: search.next_page.clone(),
            ..ListQuery::default()
        };
        // The first page is what the user just asked for. Later ones fill in as they scroll,
        // and a search run ahead may never be asked for.
        let lane = if search.next_page.is_some() || search.preloaded {
            Lane::Show
        } else {
            Lane::Act
        };
        let page = inner
            .api
            .call(lane, crate::sync::ATTEMPTS, |gmail| gmail.messages(&query))
            .await?;
        let hits: Vec<GmailHit> = page
            .messages
            .iter()
            .filter_map(|message| {
                Some(GmailHit {
                    thread: message.thread_id.clone()?,
                    message: message.id.clone()?,
                })
            })
            .collect();
        let threads: Vec<(String, u64)> = hits
            .iter()
            .map(|hit| hit.thread.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| (id, 0))
            .collect();
        let fetched = inner.fetch_stale(lane, &threads).await?;
        if !fetched.is_empty() {
            let mut write = inner.begin(&mut db).await?;
            for rows in &fetched {
                if let Some(id) = provider::save_rows(write.exec(), rows).await? {
                    reindex(write.exec(), &id, &router).await?;
                }
            }
            write.commit().await?;
        }
        Ok(GmailPage {
            hits,
            next: page.next_page_token,
        })
    }
}

/// The labels a query can name: the account's and Exo's own lists.
pub(crate) async fn label_index(exec: &mut dyn Executor) -> Result<LabelIndex> {
    let rows = LabelRow::all().exec(exec).await?;
    Ok(LabelIndex::new(
        [],
        rows.iter().map(|row| (row.id.as_str(), row.name.as_str())),
    ))
}

/// Whether a local result passes the check of every one of its phrases and stemmed words. Each
/// positive recheck must appear in its subject as the index highlighted it (`searched`), its
/// subject, or (unless it's a `subject:`) its excerpt or a message's body as the index has it
/// (its visible text, once worked out). Each negated recheck must appear in none of them.
async fn recheck(
    exec: &mut dyn Executor,
    view: &ThreadView,
    rechecks: &[Recheck],
    excerpt: &str,
    searched: &str,
) -> Result<bool> {
    let subject = view.subject();
    let mut bodies: Option<Vec<String>> = None;
    for recheck in rechecks {
        let regex = &recheck.regex;
        let mut found = regex.is_match(searched) || regex.is_match(&subject);
        if !recheck.subject_only && !found {
            found = regex.is_match(excerpt);
            if !found {
                if bodies.is_none() {
                    let drafts = crate::drafts::editable(exec, &view.id).await?;
                    let ids: Vec<&str> = view
                        .messages
                        .iter()
                        .map(|m| m.row.id.as_str())
                        .filter(|id| {
                            !drafts
                                .iter()
                                .any(|draft| draft.message_id.as_deref() == Some(*id))
                        })
                        .collect();
                    let mut texts: Vec<String> = Vec::new();
                    for chunk in ids.chunks(IN_LIST_CHUNK) {
                        let rows = BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
                            .exec(exec)
                            .await?;
                        texts.extend(rows.iter().map(body_text));
                    }
                    texts.extend(
                        drafts
                            .iter()
                            .map(|draft| whole_text(Some(&draft.body), None)),
                    );
                    bodies = Some(texts);
                }
                found = bodies.iter().flatten().any(|body| regex.is_match(body));
            }
        }
        if found == recheck.negated {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Of the excerpt's pieces (split at ellipses and between messages), the one with the most
/// matches, if any matched.
fn pick_snippet(marked: &str) -> Option<Highlighted> {
    let mut best: Option<Highlighted> = None;
    for piece in marked.split([ELLIPSIS, '\u{f8f8}', '\u{f8f5}']) {
        let piece = Highlighted::from_marked(piece.trim());
        if piece.bold.len() > best.as_ref().map_or(0, |best| best.bold.len()) {
            best = Some(piece);
        }
    }
    best
}

/// A result's sort time, which is the date of the latest message at or before the search
/// started, drafts included.
fn sort_for_search(view: &ThreadView, started: i64) -> i64 {
    let dates = view.messages.iter().map(|m| m.row.internal_date);
    dates
        .clone()
        .filter(|date| *date <= started)
        .max()
        .or_else(|| dates.max())
        .unwrap_or_default()
}

/// The last message's snippet, whitespace collapsed.
fn latest_snippet(view: &ThreadView) -> String {
    view.messages
        .last()
        .map(|m| collapse(&unescape_html(&m.row.snippet)))
        .unwrap_or_default()
}

/// A result's row. It leaves the results when a triage takes away a label the query asks for,
/// or trashes the thread when the query doesn't ask for Trash.
fn summary(
    view: &mut ThreadView,
    list: &ListId,
    sort: i64,
    router: &Router,
    attachments: &[AttachmentRow],
    included: &[String],
    keeps_trash: bool,
) -> ThreadSummary {
    let mut row = view.summary(list, sort, router, attachments);
    row.removed_by = view.removed_by_with(router, attachments, None, |lists, labels| {
        (!keeps_trash && labels.contains(TRASH))
            || included.iter().any(|label| match label.as_str() {
                INBOX => !lists
                    .iter()
                    .any(|id| *id == SH_IMPORTANT || *id == SH_OTHER),
                label => !lists.contains(&label) && !labels.contains(label),
            })
    });
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafts::{Draft, DraftAction, DraftRecord};
    use crate::ids::ThreadId;

    #[tokio::test]
    async fn recheck_uses_stripped_html_and_editable_drafts_and_quotes_keyword_prefixes() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-search-recheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        let filler = "alpha ".repeat(80);
        let html = format!(
            "<style>.hiddenword {{ color: blue; }}</style><div>{filler}<b>crystal</b> &amp; clear</div>"
        );
        let msg = gmail::Message {
            id: Some("m1".into()),
            thread_id: Some("t1".into()),
            internal_date: Some(1_794_900_000_000),
            label_ids: vec![INBOX.into()],
            payload: Some(gmail::MessagePart {
                mime_type: Some("text/html".into()),
                headers: vec![
                    gmail::MessagePartHeader {
                        name: "Subject".into(),
                        value: "Deck".into(),
                    },
                    gmail::MessagePartHeader {
                        name: "From".into(),
                        value: "Priya <priya@brightmoor.co>".into(),
                    },
                ],
                body: Some(gmail::MessagePartBody {
                    data: Some(html.into_bytes()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            let parsed = crate::parse::parse(&msg, true).expect("parses");
            provider::save_message(write.exec(), parsed)
                .await
                .expect("saves");
            reindex(write.exec(), "t1", &store.inner.router())
                .await
                .expect("indexes");
            write.commit().await.expect("commits");
        }

        // `OR*` / `AND*` / `NOT*` / `NEAR*` (both free text and column values) don't crash FTS5.
        for q in [
            "OR*",
            "AND*",
            "NOT*",
            "NEAR*",
            "label:OR*",
            "label:AND*",
            "label:NOT*",
            "label:NEAR*",
        ] {
            let mut s = Search::new(q);
            store.search(&mut s, 20, false, 10).await.expect("fts5 ok");
        }

        // Raw HTML tags/styles don't break phrase matching, and entities (`&amp;`) are decoded.
        let mut hit = Search::new("\"crystal & clear\"");
        let page = store.search(&mut hit, 20, false, 10).await.expect("search");
        assert_eq!(page.results.len(), 1);

        // Editable draft text also passes `recheck`.
        store
            .save_draft(Draft {
                id: "draft00000000000001".into(),
                thread: Some(ThreadId::new("t1")),
                adopted: None,
                record: DraftRecord {
                    action: DraftAction::Reply,
                    from: None,
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "Re: Deck".into(),
                    body: format!("<div>{filler}velvet giraffe draft</div>"),
                    quoted_content: None,
                    quoted_inlined: false,
                    quote_signed: false,
                    in_reply_to: None,
                    in_reply_to_rfc822: None,
                    references: Vec::new(),
                    rfc822_id: "d1@exo".into(),
                    fingerprint: String::new(),
                    attachments: Vec::new(),
                    send_later: None,
                    reminder: None,
                },
                raw: String::new(),
                complete: true,
                sending: false,
            })
            .await
            .expect("saves draft");

        let mut draft_hit = Search::new("\"velvet giraffe\"");
        let page = store
            .search(&mut draft_hit, 20, false, 10)
            .await
            .expect("search");
        assert_eq!(page.results.len(), 1);

        // Multiple rechecks are ANDed: a failing positive or negated phrase must reject the thread.
        let mut both_missing = Search::new("\"crystal & clear\" \"not present\"");
        assert!(
            store
                .search(&mut both_missing, 20, false, 10)
                .await
                .expect("search")
                .results
                .is_empty(),
            "every positive recheck must pass"
        );
        // Stemmed mismatch on second phrase ("crystals" stems to "crystal" in FTS5, so FTS5
        // returns t1, and recheck must enforce AND across both phrases).
        let mut second_stemmed_mismatch = Search::new("\"crystal & clear\" \"velvets giraffe\"");
        assert!(
            store
                .search(&mut second_stemmed_mismatch, 20, false, 10)
                .await
                .expect("search")
                .results
                .is_empty(),
            "second recheck mismatch must reject even when first passes"
        );
        // Negated recheck must reject when found (using a phrase whose tokens aren't already
        // dropped by FTS5, e.g. `-subject:"Deck"` or `"crystal & clear" -"crystal"`).
        let mut negated_subject = Search::new("\"crystal & clear\" -subject:\"Deck\"");
        assert!(
            store
                .search(&mut negated_subject, 20, false, 10)
                .await
                .expect("search")
                .results
                .is_empty()
        );
        // Negated recheck that passes must NOT be bolded in highlights.
        let mut neg_other = Search::new("\"crystal & clear\" -subject:\"velvet\"");
        let page = store
            .search(&mut neg_other, 20, false, 10)
            .await
            .expect("search");
        assert_eq!(page.results.len(), 1);
        let snippet = page.results[0].snippet.as_ref().expect("snippet");
        let bold_words: Vec<&str> = snippet
            .bold
            .iter()
            .map(|r| &snippet.text[r.clone()])
            .collect();
        assert!(!bold_words.contains(&"velvet"));

        // Quoted `subject:"..."` uses an FTS5 subquery, so outer snippet() has no subject marks;
        // rehighlight must still highlight `view.subject()`.
        let msg2 = gmail::Message {
            id: Some("m2".into()),
            thread_id: Some("t2".into()),
            internal_date: Some(1_794_900_000_100),
            label_ids: vec![INBOX.into()],
            payload: Some(gmail::MessagePart {
                mime_type: Some("text/plain".into()),
                headers: vec![
                    gmail::MessagePartHeader {
                        name: "Subject".into(),
                        value: "Quarterly Q4 Plan Review".into(),
                    },
                    gmail::MessagePartHeader {
                        name: "From".into(),
                        value: "Priya <priya@brightmoor.co>".into(),
                    },
                    gmail::MessagePartHeader {
                        name: "Cc".into(),
                        value: "\"Ada Lovelace\" <ada@example.com>".into(),
                    },
                ],
                body: Some(gmail::MessagePartBody {
                    data: Some(b"Review notes for q4 plan".to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            let parsed = crate::parse::parse(&msg2, true).expect("parses");
            provider::save_message(write.exec(), parsed)
                .await
                .expect("saves");
            reindex(write.exec(), "t2", &store.inner.router())
                .await
                .expect("indexes");
            write.commit().await.expect("commits");
        }
        let mut subj_q = Search::new("subject:\"q4 plan\"");
        let page = store
            .search(&mut subj_q, 20, false, 10)
            .await
            .expect("search");
        assert_eq!(page.results.len(), 1);
        let subj = page.results[0]
            .subject
            .as_ref()
            .expect("quoted subject highlights view.subject()");
        let subj_bold: Vec<&str> = subj.bold.iter().map(|r| &subj.text[r.clone()]).collect();
        assert_eq!(subj_bold, ["Q4 Plan"]);
        // Quoted `to:` searches across to, cc, and bcc in FTS5.
        let mut cc_q = Search::new("to:\"Ada Lovelace\"");
        let page = store
            .search(&mut cc_q, 20, false, 10)
            .await
            .expect("search");
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].thread.id, ThreadId::new("t2"));

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whole_text_strips_matching_hidden_tags_without_eating_body_when_head_unclosed() {
        let html = "<head><title>Hidden Title</title><body><p>Visible &mdash; body</p><style>.x{color:red}</style><script>alert(1)</script><!-- comment --></body>";
        assert_eq!(
            whole_text(Some(html), None),
            "Visible \u{2014} body",
            "omitted </head> must not eat body up to </style>"
        );
    }
}
