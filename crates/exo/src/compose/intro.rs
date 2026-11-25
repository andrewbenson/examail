//! Instant Intro. In a reply to an introduction, ⌘⇧I moves the introducer to Bcc and starts the
//! reply with the `customInstantIntro` line (`Thanks {first_name}! (to BCC)`) naming them. Each
//! press moves the next of the original To and Cc instead, and the line follows. After the last,
//! the recipients are as they were and the line goes, to come back on the next press.

use exo_commands::{Command, Commands, Hint, Region};
use exo_mail::dom::{self, NodeRef};
use exo_store::{Address, DraftAction, MessageDetail};

use super::{Action, Field, Open, auto_bcc, contacts};
use crate::commands;

/// The snippet's element and its name spans, as they go out in the sent mail.
const SNIPPET: &str = "Exo-Intro-Snippet";
const NAME: &str = "Exo-Intro-Name";
/// The same line's classes in a draft begun in another client, read as Exo's.
const FOREIGN_SNIPPET: &str = "Superhuman-Intro-Snippet";
const FOREIGN_NAME: &str = "Superhuman-Intro-Name";

fn is_name(node: &NodeRef) -> bool {
    dom::has_class(node, NAME) || dom::has_class(node, FOREIGN_NAME)
}

/// Where a draft's cycle is.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cycle {
    /// Which of the original To and Cc is in Bcc now.
    index: Option<usize>,
    /// The recipients as they were before the first press: To, Cc, Bcc.
    start: Option<[Vec<Address>; 3]>,
    /// The snippet taken out when the cycle came back round, to put back on the next press.
    last_snippet: Option<String>,
    /// The recipients as the cycle last left them. Any other change starts the cycle over.
    left: Option<[Vec<Address>; 3]>,
}

impl Cycle {
    /// Starts over if the recipients changed since the cycle last touched them.
    fn reset_if_changed(&mut self, now: &[Vec<Address>; 3]) {
        if self.left.as_ref().is_some_and(|left| left != now) {
            self.index = None;
            self.start = None;
            self.left = None;
        }
    }
}

fn recipients(open: &Open) -> [Vec<Address>; 3] {
    [
        open.record.to.clone(),
        open.record.cc.clone(),
        open.record.bcc.clone(),
    ]
}

/// One press. Returns whether it did anything (a draft needs two recipients), in which case Exo
/// hints `Not quite right? Hit ⌘ shift I again!`.
pub(crate) fn cycle(
    open: &mut Open,
    intro: &str,
    book: Option<&contacts::Index>,
    now: f64,
) -> bool {
    let current = recipients(open);
    open.delivery.intro.reset_if_changed(&current);
    let everyone = |open: &Open| open.recipients().count();
    if everyone(open) < 2 {
        return false;
    }
    // The next of the original To and Cc, or no one after the last.
    let cycle = &mut open.delivery.intro;
    match cycle.index {
        None => {
            cycle.index = Some(0);
            cycle.start = Some(current.clone());
        }
        Some(index) => cycle.index = Some(index + 1),
    }
    let [to, cc, bcc] = cycle.start.clone().unwrap_or_else(|| current.clone());
    if cycle
        .index
        .is_some_and(|index| index >= to.len() + cc.len())
    {
        cycle.index = None;
    }
    // That person moves to Bcc, and a To left empty takes the Cc (or failing that, the Bcc).
    let (mut to, mut cc, mut bcc) = (to, cc, bcc);
    let moved = match cycle.index {
        None => None,
        Some(index) if index < to.len() => {
            let person = to.remove(index);
            if to.is_empty() {
                to = std::mem::take(&mut cc);
                if to.is_empty() {
                    to = std::mem::take(&mut bcc);
                }
            }
            bcc.push(person.clone());
            Some(person)
        }
        Some(index) => {
            let person = cc.remove(index - to.len());
            bcc.push(person.clone());
            Some(person)
        }
    };
    let first_name = moved
        .as_ref()
        .and_then(|person| contacts::best_known_first_name(person, book));
    let index = open.delivery.intro.index;
    insert_snippet(open, first_name.as_deref(), index, intro);
    // The recipients and body change together (the recipient undo step belongs to
    // `chips::History`).
    open.record.to = to;
    open.record.cc = cc;
    open.record.bcc = bcc;
    open.delivery.intro.left = Some(recipients(open));
    auto_bcc::settle(open);
    open.touch(now);
    true
}

/// Puts the snippet naming `first_name` into an empty body. A snippet already there names them
/// instead, or goes when no one's moved. It's then kept aside for the next press, which puts it
/// back when it starts the cycle again. A body with anything else in it is left alone.
fn insert_snippet(open: &mut Open, first_name: Option<&str>, index: Option<usize>, intro: &str) {
    let document = dom::parse(&open.editor.html());
    let Some(body) = dom::body(&document) else {
        dom::drop_tree(document);
        return;
    };
    let mut snippet = find_snippet(&body);
    if snippet.is_none()
        && index == Some(0)
        && let Some(kept) = open.delivery.intro.last_snippet.take()
        && let Some(node) = fragment(&kept)
    {
        dom::prepend(&body, &node);
        snippet = Some(node);
    }
    match (snippet, first_name) {
        (Some(snippet), Some(name)) => set_names(&snippet, name),
        (Some(snippet), None) => {
            open.delivery.intro.last_snippet = Some(dom::outer_html(&snippet));
            dom::detach(&snippet);
            dom::drop_tree(snippet);
        }
        (None, _) if dom::text_content(&body).trim().is_empty() => {
            let line = exo_mail::escape_html_text(intro)
                .split("{first_name}")
                .collect::<Vec<_>>()
                .join(&format!("<span class='{NAME}'></span>"));
            if let Some(node) = fragment(&format!(
                "<div class=\"{SNIPPET}\">{line}<div><br></div></div>"
            )) {
                if let Some(name) = first_name {
                    set_names(&node, name);
                }
                dom::prepend(&body, &node);
            }
        }
        (None, _) => {
            dom::drop_tree(document);
            return;
        }
    }
    let updated = dom::inner_html(&body);
    dom::drop_tree(document);
    open.editor.modify_html(&updated);
}

/// The body's first `div`, if it's the snippet with a name in it.
fn find_snippet(body: &NodeRef) -> Option<NodeRef> {
    let first = dom::select_first(body, |node| dom::is(node, "div"))?;
    let class = dom::attr(&first, "class").unwrap_or_default();
    ((class.contains(SNIPPET) || class.contains(FOREIGN_SNIPPET))
        && dom::select_first(&first, is_name).is_some())
    .then_some(first)
}

/// Puts `name` in each of the snippet's name spans.
fn set_names(snippet: &NodeRef, name: &str) {
    let mut stack = vec![snippet.clone()];
    while let Some(node) = stack.pop() {
        if is_name(&node) {
            for child in dom::take_children(&node) {
                dom::drop_tree(child);
            }
            dom::set_children(&node, vec![dom::text_node(name)]);
        } else {
            stack.extend(dom::children(&node));
        }
    }
}

/// The element `html` parses to, detached from its temporary document.
fn fragment(html: &str) -> Option<NodeRef> {
    let mut nodes = dom::parse_fragment(html);
    let first = (!nodes.is_empty()).then(|| nodes.remove(0));
    for extra in nodes {
        dom::drop_tree(extra);
    }
    first
}

/// Registers `Instant Intro` (⌘⇧I) for the draft with the keyboard.
pub(super) fn register(commands: &mut Commands<commands::Action>) {
    commands
        .add(command(Action::InstantIntro).regions(&[Region::ComposeForm, Region::RecipientList]));
}

/// Registers `Instant Intro` on a conversation. In a thread it's for the focused message, and in
/// the list for the highlighted conversation.
pub(super) fn register_on_conversation(commands: &mut Commands<commands::Action>, in_thread: bool) {
    let regions: &[Region] = if in_thread {
        &[Region::ThreadMessages]
    } else {
        exo_commands::CONVERSATION
    };
    let command = command(Action::InstantIntroReply).regions(regions);
    commands.add(if in_thread {
        command
    } else {
        command.scale(1.01)
    });
}

fn command(action: Action) -> Command<commands::Action> {
    Command::new(
        "INSTANT_INTRO",
        "Instant Intro",
        commands::Action::Compose(action),
    )
    .key("mod+shift+i")
    .key_code(["⌘", "shift", "I"])
    .aliases(["To BCC", "Instant Introduction", "Move sender to BCC"])
    .icon("data/instant-intro")
}

/// The hint after a press: `Not quite right? Hit ⌘ shift I again!`.
fn try_again() -> Hint {
    Hint::new()
        .text("Not quite right? Hit")
        .keys(["⌘", "shift", "I"])
        .text("again!")
}

impl crate::Mail {
    /// ⌘⇧I in a draft takes the next step of its cycle.
    pub(super) fn instant_intro(&mut self, now: f64) {
        let intro = self.account.settings().instant_intro().to_owned();
        if let Some((open, book)) = self.compose.focused_with_book()
            && cycle(open, &intro, book, now)
        {
            self.commands.hint(try_again());
        }
    }

    /// ⌘⇧I on a conversation. The focused message's draft cycles, or a Reply All starts with the
    /// first step. A message with fewer than two recipients hints that Instant Intro is for
    /// introductions.
    pub(super) fn instant_intro_reply(&mut self, now: f64) {
        let focused = self.thread.as_ref().and_then(|view| {
            let detail = view.detail()?;
            let id = view.focused()?;
            detail
                .messages
                .iter()
                .find(|message| message.id == id)
                .cloned()
        });
        if let Some(message) = &focused
            && intro_recipients(message) < 2
        {
            self.commands.hint(
                Hint::new()
                    .text("Next time, hit")
                    .keys(["⌘", "shift", "I"])
                    .text("on an intro to instantly start a response."),
            );
        }
        let existing = focused.as_ref().and_then(|message| {
            self.compose
                .drafts
                .iter()
                .find(|open| open.record.in_reply_to.as_deref() == Some(message.id.as_str()))
                .map(|open| open.id.clone())
        });
        if let Some(id) = existing {
            self.compose.focus = Some(id);
            self.instant_intro(now);
            return;
        }
        self.compose_run(Action::Reply(DraftAction::ReplyAll), now);
        match &self.compose.pending {
            Some(pending) => self.compose.intro_pending = Some(pending.thread.clone()),
            None => self.instant_intro(now),
        }
    }

    /// The Reply All of an Instant Intro asked for before its thread loaded takes its first
    /// step once it exists.
    pub(super) fn start_pending_intro(&mut self, now: f64) {
        if self.compose.pending.is_some() {
            return;
        }
        let Some(thread) = self.compose.intro_pending.take() else {
            return;
        };
        if let Some(open) = self
            .compose
            .focused_mut()
            .filter(|open| open.thread.as_deref() == Some(thread.as_str()))
        {
            open.state.field = Field::Body;
            self.instant_intro(now);
        }
    }
}

/// How many people the message is to: its valid To, Cc and Bcc, each address once.
fn intro_recipients(message: &MessageDetail) -> usize {
    let mut seen: Vec<String> = Vec::new();
    for contact in message.to.iter().chain(&message.cc).chain(&message.bcc) {
        let email = contact.email.to_lowercase();
        if super::recipients::is_valid(&email) && !seen.contains(&email) {
            seen.push(email);
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use exo_store::DraftRecord;

    use super::*;

    fn person(name: &str, email: &str) -> Address {
        Address {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    fn reply(to: &[Address], cc: &[Address]) -> Open {
        let record = DraftRecord {
            action: DraftAction::ReplyAll,
            from: None,
            to: to.to_vec(),
            cc: cc.to_vec(),
            bcc: Vec::new(),
            subject: "Intro".into(),
            body: String::new(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: Some("m".into()),
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "x@y".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        Open::new("d".into(), Some("t".into()), record)
    }

    fn emails(list: &[Address]) -> Vec<&str> {
        list.iter().map(|address| address.email.as_str()).collect()
    }

    const INTRO: &str = "Thanks {first_name}! (to BCC)";

    #[test]
    fn each_press_moves_the_next_person_to_bcc_then_restores() {
        let (sam, bea, cy) = (
            person("Sam Lee", "sam@x.com"),
            person("Bea Ruiz", "bea@y.com"),
            person("Cy", "cy@z.com"),
        );
        let mut open = reply(std::slice::from_ref(&sam), &[bea.clone(), cy.clone()]);
        assert!(cycle(&mut open, INTRO, None, 0.0));
        // Sam was alone in To, so Cc moves up.
        assert_eq!(
            (
                emails(&open.record.to),
                emails(&open.record.cc),
                emails(&open.record.bcc)
            ),
            (vec!["bea@y.com", "cy@z.com"], vec![], vec!["sam@x.com"])
        );
        assert_eq!(
            open.editor.document().blocks[0].text(),
            "Thanks Sam! (to BCC)"
        );
        assert!(cycle(&mut open, INTRO, None, 0.0));
        assert_eq!(
            (
                emails(&open.record.to),
                emails(&open.record.cc),
                emails(&open.record.bcc)
            ),
            (vec!["sam@x.com"], vec!["cy@z.com"], vec!["bea@y.com"])
        );
        assert_eq!(
            open.editor.document().blocks[0].text(),
            "Thanks Bea! (to BCC)"
        );
        assert!(cycle(&mut open, INTRO, None, 0.0));
        assert_eq!(emails(&open.record.bcc), ["cy@z.com"]);
        assert!(
            cycle(&mut open, INTRO, None, 0.0),
            "after the last, back to the start"
        );
        assert_eq!(
            (
                emails(&open.record.to),
                emails(&open.record.cc),
                emails(&open.record.bcc)
            ),
            (vec!["sam@x.com"], vec!["bea@y.com", "cy@z.com"], vec![])
        );
        assert!(open.editor.is_empty(), "the line goes with them");
        assert!(cycle(&mut open, INTRO, None, 0.0));
        assert_eq!(
            open.editor.document().blocks[0].text(),
            "Thanks Sam! (to BCC)",
            "and comes back"
        );
        assert!(open.editor.html().starts_with("<div class=\"Exo-Intro-Snippet\">Thanks <span class=\"Exo-Intro-Name\">Sam</span>! (to BCC)<div><br></div></div>"), "{}", open.editor.html());
    }

    #[test]
    fn a_body_with_something_in_it_keeps_it_and_one_recipient_does_nothing() {
        let mut lonely = reply(&[person("Sam", "sam@x.com")], &[]);
        assert!(!cycle(&mut lonely, INTRO, None, 0.0));
        let mut written = reply(&[person("Sam", "sam@x.com")], &[person("Bea", "bea@y.com")]);
        written.editor.type_text("Hi both");
        let body = written.editor.html();
        assert!(cycle(&mut written, INTRO, None, 0.0));
        assert_eq!(emails(&written.record.bcc), ["sam@x.com"]);
        assert_eq!(written.editor.html(), body);
    }

    #[test]
    fn a_line_in_the_foreign_classes_is_renamed_too() {
        let mut open = reply(&[person("Sam", "sam@x.com")], &[person("Bea", "bea@y.com")]);
        open.editor.set_html("<div class=\"Superhuman-Intro-Snippet\">Thanks <span class='Superhuman-Intro-Name'>Dee</span>! (to BCC)<div><br></div></div><div><br></div>");
        assert!(cycle(&mut open, INTRO, None, 0.0));
        assert_eq!(
            open.editor.document().blocks[0].text(),
            "Thanks Sam! (to BCC)"
        );
    }

    #[test]
    fn a_custom_line_and_a_change_by_hand_start_over() {
        let mut open = reply(&[person("Sam", "sam@x.com")], &[person("Bea", "bea@y.com")]);
        assert!(cycle(&mut open, "Merci {first_name} & co", None, 0.0));
        assert_eq!(open.editor.document().blocks[0].text(), "Merci Sam & co");
        open.record.to.push(person("Dee", "dee@w.com"));
        assert!(cycle(&mut open, INTRO, None, 0.0));
        assert_eq!(
            emails(&open.record.bcc),
            ["sam@x.com", "bea@y.com"],
            "a new cycle from the recipients as they are"
        );
    }
}
