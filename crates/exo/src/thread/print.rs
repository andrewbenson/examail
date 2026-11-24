//! What the open conversation prints ([`crate::print`]). Each message prints as the view shows it
//! now, with the same participants, dates, snippets and statuses, and so do the markers between
//! them.

use egui::{Color32, Context, LayerId};
use exo_store::{Contact, MessageDetail, ThreadDetail};

use super::{
    Entry, Item, ThreadView, attachments, invite, markers,
    message::{
        self, DATE, DATE_ICON, FROM, FROM_MIN_WIDTH, FROM_PADDING, SNIPPET, STATUS, State, Status,
    },
};
use crate::{
    body::Bodies,
    format::dates,
    print::{Conversation, Draft, File, Header, Message, Page, Printout, Row, When},
};

/// A collapsed row's snippet before it's cut to a page: its whole text, and the width its sender
/// and date take beside it.
struct Snippet {
    text: String,
    beside: f32,
}

/// `text`'s width in `style`, on one line.
fn width(painter: &egui::Painter, style: &exo_ui::TextStyle, text: &str) -> f32 {
    style
        .single_line(painter, text, Color32::PLACEHOLDER, None)
        .size()
        .x
}

impl ThreadView {
    /// The conversation as it prints, titled `title`, with `drafts` (each by the message it hangs
    /// under). `None` before it has loaded. `ctx` measures the collapsed rows' text, to cut their
    /// snippets where the screen would at a page's width.
    pub(crate) fn printout(
        &self,
        ctx: &Context,
        bodies: &Bodies,
        title: String,
        drafts: Vec<(String, Draft)>,
    ) -> Option<Printout> {
        let detail = self.detail.as_deref()?;
        let mut snippets = Vec::new();
        let messages: Vec<Message> = self
            .order(detail)
            .into_iter()
            .filter_map(|entry| {
                let at = match entry {
                    Entry::Message(at) => at,
                    Entry::Trashed(_) => {
                        return Some(Message::Marker {
                            text: markers::TRASHED.to_owned(),
                            link: Some(markers::SHOW.to_owned()),
                            when: None,
                        });
                    }
                    Entry::Reminder => {
                        let Some(exo_store::ReminderStatus::Returned { at, .. }) = detail.reminder
                        else {
                            return None;
                        };
                        return Some(Message::Marker {
                            text: markers::REMINDER.to_owned(),
                            link: None,
                            when: Some(dates::row(at, &self.action.now)),
                        });
                    }
                };
                let item = &self.items[at];
                let message = detail
                    .messages
                    .iter()
                    .find(|message| message.id == item.id)?;
                let state = State {
                    focused: false,
                    show_focus: false,
                    header_expanded: item.header_expanded,
                    collapsible: false,
                    composing: false,
                    offline: !self.action.online,
                    full_names: self.action.full_names,
                    reply_labels: false,
                };
                let draft = drafts
                    .iter()
                    .find(|(under, _)| *under == item.id)
                    .map(|(_, draft)| draft.clone());
                Some(if item.expanded {
                    self.print_expanded(item, message, detail, &state, bodies, draft)
                } else {
                    let (row, snippet) =
                        self.print_collapsed(ctx, item, message, detail, &state, bodies);
                    snippets.push(snippet);
                    row
                })
            })
            .collect();
        let inline = detail
            .messages
            .iter()
            .flat_map(|message| {
                message
                    .attachments
                    .iter()
                    .filter(|attachment| attachment.content_id.is_some())
                    .cloned()
            })
            .collect();
        let conversation = Conversation {
            title: title.clone(),
            subject: detail.subject.clone(),
            labels: self.action.chips.clone(),
            scrolled: self.scroll_state.scroll > 0.0,
            messages,
            inline,
            page: Page::default(),
        };
        let ctx = ctx.clone();
        Some(Printout::new(title, move |page| {
            let painter = ctx.layer_painter(LayerId::background());
            let mut conversation = conversation.clone();
            let collapsed = conversation
                .messages
                .iter_mut()
                .filter_map(|message| match message {
                    Message::Collapsed { snippet, .. } => Some(snippet),
                    _ => None,
                });
            for (cut, Snippet { text, beside }) in collapsed.zip(&snippets) {
                // The snippet: `flex: 1 1 0; margin-right: .5rem`.
                let available = (page.message_width() - beside - 0.5 * page.rem()).max(0.0);
                *cut = message::cut_snippet(text.clone(), available, |text| {
                    width(&painter, &SNIPPET, text) > available
                });
            }
            conversation.page = page;
            conversation
        }))
    }

    /// A collapsed row, its snippet left for [`Printout::on`] to cut to the page.
    fn print_collapsed(
        &self,
        ctx: &Context,
        item: &Item,
        message: &MessageDetail,
        detail: &ThreadDetail,
        state: &State,
        bodies: &Bodies,
    ) -> (Message, Snippet) {
        let painter = ctx.layer_painter(LayerId::background());
        let width = |style: &exo_ui::TextStyle, text: &str| width(&painter, style, text);
        let from = message::participants(message, detail, false, self.action.full_names);
        let (when, date_width) = match Status::of(message, state) {
            Some(status) => {
                let text = status.collapsed_title(&self.action.now);
                let measured = width(&STATUS, &text);
                (
                    When::Status {
                        text,
                        color: status.ink(),
                    },
                    measured,
                )
            }
            None => {
                let date = dates::row(message.date, &self.action.now);
                let measured = width(&DATE, &date) + DATE.letter_spacing;
                (When::Date(date), measured)
            }
        };
        // A failed, offline or scheduled send's status has no icon.
        let icon = match when {
            When::Date(_) => attachments::kind(message),
            When::Status { .. } => exo_store::Attachments::None,
        };
        let from_box = (width(&FROM, &from) + FROM_PADDING).max(FROM_MIN_WIDTH);
        let date_box = date_width
            + if icon == exo_store::Attachments::None {
                0.0
            } else {
                DATE_ICON
            };
        let snippet = Snippet {
            text: message::snippet_text(bodies.facts(&item.id)),
            beside: from_box + date_box,
        };
        let row = Message::Collapsed {
            from,
            snippet: String::new(),
            icon,
            when,
        };
        (row, snippet)
    }

    fn print_expanded(
        &self,
        item: &Item,
        message: &MessageDetail,
        detail: &ThreadDetail,
        state: &State,
        bodies: &Bodies,
        draft: Option<Draft>,
    ) -> Message {
        let status = Status::of(message, state);
        let header = if item.header_expanded {
            let rows = [
                ("From", message.from.as_slice()),
                ("To", &message.to),
                ("Cc", &message.cc),
                ("Bcc", &message.bcc),
            ]
            .into_iter()
            .filter(|(_, contacts)| !contacts.is_empty())
            .map(|(label, contacts)| Row {
                label,
                contacts: contacts.iter().map(named).collect(),
            })
            .collect();
            let when = match status {
                Some(status) => When::Status {
                    text: status.full_title(&self.action.now),
                    color: status.ink(),
                },
                None => When::Date(dates::expanded(message.date, &self.action.now)),
            };
            Header::Full {
                rows,
                when,
                chevron: !matches!(status, Some(Status::Failed(_))),
                tracking_blocked: bodies.tracking_blocked(&item.id),
            }
        } else {
            let when = match status {
                Some(status) => When::Status {
                    text: status.title(&self.action.now),
                    color: status.ink(),
                },
                None => When::Date(dates::row(message.date, &self.action.now)),
            };
            Header::Compact {
                participants: message::participants(message, detail, true, self.action.full_names),
                when,
            }
        };
        let files = attachments::listed(message)
            .map(|attachment| File::of(&attachment.filename, &attachment.mime_type))
            .collect();
        let calendar = bodies.facts(&item.id).map(|facts| &facts.calendar);
        let invite = self
            .invite_state
            .invites
            .get(&item.id)
            .zip(self.invite_context(&item.id, calendar))
            .map(|(parsed, context)| {
                invite::printout(parsed, &context, invite::note(parsed, calendar).as_deref())
            });
        // A notification the card stands in for doesn't print either.
        let body = if self.hides_body(&item.id, bodies) {
            None
        } else {
            bodies.printable(&item.id, item.show_quoted)
        };
        Message::Expanded {
            header,
            invite,
            body,
            files,
            draft,
        }
    }
}

/// A contact as the full header names it: its name, if it has one, and its address.
fn named(contact: &Contact) -> (Option<String>, String) {
    (
        contact.name.clone().filter(|name| !name.is_empty()),
        contact.email.clone(),
    )
}
