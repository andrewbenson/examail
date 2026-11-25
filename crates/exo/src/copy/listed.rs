//! ⌘C in the list copies the cursor's conversation the way ⌘C in the thread view does, without
//! opening it. A conversation of one message copies as that message, and a longer one as the
//! conversation. The bodies come from the store (sync usually has them already) and are rendered
//! off screen to find their new parts.

use std::sync::Arc;

use exo_commands::Hint;
use exo_store::{MessageDetail, ThreadDetail};

use crate::{Mail, body::Facts, clipboard, compose};

/// The copy under way.
#[derive(Default)]
pub(crate) struct Listed {
    /// The conversation being copied, and its messages once the store has read them.
    target: Option<(String, Option<Arc<ThreadDetail>>)>,
}

/// The hint after copying a message, with `more` messages to copy or without.
pub(crate) fn message_hint(more: bool) -> Hint {
    let hint = Hint::new()
        .text("Message copied to clipboard.")
        .text("Hit")
        .keys(["⌘", "V"])
        .text("to use as new draft.");
    if more {
        hint.text("Hit")
            .keys(["⌘", "C"])
            .text("again to copy conversation.")
    } else {
        hint
    }
}

/// The hint after copying a conversation.
pub(crate) fn conversation_hint() -> Hint {
    Hint::new().text("Conversation copied to clipboard.")
}

impl Mail {
    /// ⌘C in the list. Starts copying the cursor's conversation, unless several are selected or
    /// the conversation is a new message being written.
    pub(crate) fn copy_listed(&mut self) {
        if self.inbox.is_selecting() {
            return;
        }
        let Some(id) = self
            .inbox
            .cursor()
            .filter(|thread| !thread.draft_only)
            .map(|thread| thread.id.clone())
        else {
            return;
        };
        self.copy_listed.target = Some((id.clone(), None));
        let ui = self.ui_sender();
        self.account
            .loader()
            .read_thread_with_bodies(id, "to copy it", move |id, detail| {
                ui.send(move |mail| mail.copy_listed_read(&id, detail));
            });
    }

    /// Takes in the conversation the store read, and has its messages rendered.
    fn copy_listed_read(&mut self, id: &str, detail: Option<Arc<ThreadDetail>>) {
        let Some((target, waiting)) = self
            .copy_listed
            .target
            .as_mut()
            .filter(|(target, _)| target == id)
        else {
            return;
        };
        let Some(detail) = detail else {
            self.copy_listed.target = None;
            return;
        };
        let messages: Vec<&MessageDetail> = detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .collect();
        if messages.is_empty() {
            self.copy_listed.target = None;
            return;
        }
        self.bodies.render(target, &messages, self.account.email());
        *waiting = Some(detail);
    }

    /// The messages of conversation `id` were rendered. If it's the conversation being copied,
    /// its copy goes onto the clipboard.
    pub(crate) fn copy_listed_rendered(&mut self, id: &str, facts: &[(String, Facts)]) {
        let Some((_, Some(detail))) = self
            .copy_listed
            .target
            .take_if(|(target, detail)| target == id && detail.is_some())
        else {
            return;
        };
        let messages: Vec<(&MessageDetail, &str)> = detail
            .messages
            .iter()
            .filter(|message| !message.is_draft())
            .filter_map(|message| {
                Some((
                    message,
                    facts
                        .iter()
                        .find(|(rendered, _)| *rendered == message.id)?
                        .1
                        .unquoted_html
                        .as_str(),
                ))
            })
            .collect();
        match messages[..] {
            [] => {}
            [(message, html)] => {
                let contents = super::message(message, html, &self.now);
                clipboard::write(&self.ctx, &contents.html, &contents.text);
                self.commands.hint(message_hint(false));
                self.copied_message =
                    Some((contents.html, compose::Fields::of_message(message, html)));
            }
            _ => {
                let contents = super::conversation(&messages, &self.now);
                clipboard::write(&self.ctx, &contents.html, &contents.text);
                self.commands.hint(conversation_hint());
            }
        }
    }
}
