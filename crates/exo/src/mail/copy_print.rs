//! Clipboard copy operations and print/PDF export on [`Mail`].

use exo_commands::Hint;

use crate::{
    Mail, attachment_files, clipboard, commands, compose, copy, mailbox::ThreadSummary, print,
    subject_query, thread::ThreadView,
};

/// ⌘C again within this long copies the conversation, matching the copy hint's 10 s.
const COPY_AGAIN: f64 = 10.0;

/// Which of the palette's copy rows apply: in a thread, its conversation, focused message and
/// subject; in the list, the subject of the one conversation actions apply to.
pub(super) fn copy_rows(
    thread: Option<&ThreadView>,
    actionable: &[&ThreadSummary],
) -> commands::CopyRows {
    match thread {
        Some(thread) => commands::CopyRows {
            thread: true,
            message: thread.focused().is_some(),
            subject: thread
                .detail()
                .is_some_and(|detail| !detail.subject.is_empty()),
            attachments: thread
                .detail()
                .and_then(|detail| {
                    detail
                        .messages
                        .iter()
                        .find(|message| Some(message.id.as_str()) == thread.focused())
                })
                .is_some_and(|message| attachment_files::has_copied(&message.attachments)),
        },
        None => commands::CopyRows {
            subject: matches!(actionable, [one] if one.subject != "-"),
            ..commands::CopyRows::default()
        },
    }
}

impl Mail {
    /// ⌘C with nothing selected in a thread, which copies the focused message, or the conversation
    /// when it's ⌘C again on the same message while the first copy's hint could still show.
    pub(super) fn copy_message_or_conversation(&mut self, now: f64) {
        let Some(focused) = self
            .thread
            .as_ref()
            .and_then(ThreadView::focused)
            .map(str::to_owned)
        else {
            return;
        };
        let again = self
            .copied
            .take()
            .is_some_and(|(id, at)| id == focused && now - at < COPY_AGAIN);
        if again && self.copyable().len() > 1 {
            self.copy_conversation();
        } else {
            self.copy_message();
            self.copied = Some((focused, now));
        }
    }

    /// The open thread's messages that a copy includes, with their new parts. Drafts aren't
    /// included, and a body still being rendered can't be yet.
    pub(crate) fn copyable(&self) -> Vec<(&exo_store::MessageDetail, &str)> {
        let Some(detail) = self.thread.as_ref().and_then(ThreadView::detail) else {
            return Vec::new();
        };
        detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .filter_map(|message| {
                Some((
                    message,
                    self.bodies.facts(&message.id)?.unquoted_html.as_str(),
                ))
            })
            .collect()
    }

    /// Copies the focused message, with the hint to paste it.
    pub(super) fn copy_message(&mut self) {
        let messages = self.copyable();
        let focused = self.thread.as_ref().and_then(ThreadView::focused);
        let Some((message, html)) = messages
            .iter()
            .find(|(message, _)| Some(message.id.as_str()) == focused)
        else {
            return;
        };
        let contents = copy::message(message, html, &self.now);
        clipboard::write(&self.ctx, &contents.html, &contents.text);
        let copied = (
            contents.html.clone(),
            compose::Fields::of_message(message, html),
        );
        self.commands.hint(copy::message_hint(messages.len() > 1));
        self.copied_message = Some(copied);
    }

    /// Copies the open conversation.
    pub(super) fn copy_conversation(&mut self) {
        let messages = self.copyable();
        if messages.is_empty() {
            return;
        }
        let contents = copy::conversation(&messages, &self.now);
        clipboard::write(&self.ctx, &contents.html, &contents.text);
        self.commands.hint(copy::conversation_hint());
    }

    /// Copies the focused message's recipients.
    pub(super) fn copy_recipients(&mut self) {
        let Some(thread) = &self.thread else { return };
        let Some(message) = thread.detail().and_then(|detail| {
            detail
                .messages
                .iter()
                .find(|message| Some(message.id.as_str()) == thread.focused())
        }) else {
            return;
        };
        self.ctx.copy_text(copy::recipients(message));
        self.commands
            .hint(Hint::new().text("Recipients copied to clipboard."));
    }

    /// Copies the thread's subject.
    pub(super) fn copy_subject(&mut self) {
        let subject = match &self.thread {
            Some(thread) => thread.detail().map(|detail| detail.subject.clone()),
            None => self.inbox.cursor().map(|thread| thread.subject.clone()),
        };
        if let Some(subject) = subject {
            clipboard::write_with_search(&self.ctx, &subject, &subject_query(&subject));
            self.commands
                .hint(Hint::new().text("Subject copied to clipboard."));
        }
    }

    /// Prints the open conversation through the macOS print panel (`Print`, ⌘P, File ▸ Print).
    pub fn print(&mut self) {
        if let Some(printout) = self.printout() {
            print::conversation(printout, &self.account.loader());
        }
    }

    /// `Save as PDF`, which shows the save panel, then prints the open conversation into the file
    /// chosen.
    pub(super) fn save_as_pdf(&mut self) {
        if let Some(printout) = self.printout() {
            print::save_as_pdf(printout, &self.account.loader());
        }
    }

    /// Prints the open conversation into the PDF at `path` (`Save as PDF` without its panel, for
    /// screenshots and tests). `done` hears whether it was written.
    pub fn print_to_pdf(&mut self, path: std::path::PathBuf, done: print::Done) {
        match self.printout() {
            Some(printout) => print::print(
                printout,
                &self.account.loader(),
                print::Destination::Pdf(path),
                Some(done),
            ),
            None => done(false),
        }
    }

    /// The open conversation as it prints, with the drafts open in it. `None` before it loads.
    fn printout(&self) -> Option<print::Printout> {
        let thread = self.thread.as_ref()?;
        let drafts = thread
            .shared_detail()
            .map(|detail| {
                self.compose
                    .printouts(&detail, &self.snapshot.aliases, self.account.email())
            })
            .unwrap_or_default();
        thread.printout(&self.ctx, &self.bodies, self.print_title(), drafts)
    }

    /// The print job's title, from the open thread's subject.
    fn print_title(&self) -> String {
        let subject = self
            .thread
            .as_ref()
            .and_then(ThreadView::detail)
            .map(|detail| detail.subject.as_str())
            .unwrap_or_default();
        print::title(subject, self.accounts.len(), self.account.email())
    }
}
