//! A snippet open in compose. It's the regular new-message form with a name, its header folded
//! until asked for, and `Done` instead of Send. A snippet is never a Gmail draft. Done saves it
//! to the settings (no autosave), and leaving with unsaved changes asks first.

use exo_commands::{Hint, Toast};
use exo_store::Snippet;
use jiff::Timestamp;

use super::{Action, Alert, AlertThen, Field, Fields, Open, factory};
use crate::commands;

/// Where leaving a snippet with unsaved changes was going, once `Save Snippet?` lets it.
pub(crate) type Leaving = Box<dyn FnOnce(&mut crate::Mail)>;

/// How many times saving a snippet shows the `To use a Snippet, hit ⌘ ;` hint.
const DONE_HINTS: u32 = 3;

impl crate::Mail {
    /// A new private snippet named `name`, in the pane with the name focused
    /// (`New Private Snippet`).
    pub(crate) fn create_snippet(&mut self, name: String) {
        let mut record =
            factory::compose(&self.compose_account(), Fields::default(), Timestamp::now());
        // `"use default"`, which means the From of the draft the snippet is used in.
        record.from = None;
        let mut open = Open::new(factory::new_id(), None, record);
        open.set_snippet(name, None, false);
        self.open_snippet_pane(open);
    }

    /// Opens saved snippet `id` for editing (`Edit Private Snippet`). Its header shows when the
    /// snippet has recipients or a subject.
    pub(crate) fn edit_snippet(&mut self, id: &str) {
        let Some(snippet) = self
            .account
            .settings()
            .snippets
            .iter()
            .find(|snippet| snippet.id == id)
            .cloned()
        else {
            return;
        };
        let mut record =
            factory::compose(&self.compose_account(), Fields::default(), Timestamp::now());
        record.from.clone_from(&snippet.from);
        record.to.clone_from(&snippet.to);
        record.cc.clone_from(&snippet.cc);
        record.bcc.clone_from(&snippet.bcc);
        record.subject.clone_from(&snippet.subject);
        record.body.clone_from(&snippet.body);
        record.attachments.clone_from(&snippet.attachments);
        let mut open = Open::new(snippet.id.clone(), None, record);
        let shown = !(snippet.to.is_empty() && snippet.cc.is_empty() && snippet.bcc.is_empty())
            || !snippet.subject.is_empty();
        // The saved copy holds the body as the editor reads it back, so an untouched snippet
        // counts as unchanged.
        let saved = Snippet {
            body: open.editor.html(),
            ..snippet.clone()
        };
        open.set_snippet(snippet.name.clone(), Some(saved), shown);
        self.open_snippet_pane(open);
    }

    fn open_snippet_pane(&mut self, open: Open) {
        if self.snippet_unsaved() {
            self.ask_leaving_snippet(Box::new(move |mail| mail.open_snippet_pane(open)));
            return;
        }
        self.leave_pane();
        self.close_thread();
        self.close_snippet_view();
        self.snippets.folder.open();
        self.compose.focus = Some(open.id.clone());
        self.compose.pane = Some((open.id.clone(), None));
        self.compose.drafts.push(open);
    }

    /// Done / ⌘Enter on a snippet runs the size and recipient checks, then saves the snippet and
    /// shows the folder again, with the `⌘ ;` hint the first three times.
    pub(super) fn save_snippet(&mut self) {
        let aliases = self.snapshot.aliases.clone();
        let now = self.ctx.input(|input| input.time);
        let Some(id) = self
            .compose
            .focus
            .clone()
            .or_else(|| self.compose.pane().map(str::to_owned))
        else {
            return;
        };
        let Some(at) = self
            .compose
            .drafts
            .iter()
            .position(|open| open.id == id && open.is_snippet())
        else {
            return;
        };
        let book = self.compose.book.as_ref().map(|(_, index)| index.as_ref());
        let open = &mut self.compose.drafts[at];
        super::form::commit_all(open, now, book);
        let html = super::outgoing::html(open, &aliases);
        if let Some(problem) = super::checks::check(
            open,
            &html,
            true,
            super::checks::Bypass::default(),
            jiff::Timestamp::now(),
        ) {
            self.compose.alert = Some(Alert::new(
                problem.title,
                problem.text,
                vec!["OK"],
                id,
                AlertThen::Ok { focus_from: false },
            ));
            return;
        }
        let Some(snippet) = open.to_snippet() else {
            return;
        };
        let at = Timestamp::now().to_string();
        let mut hint = false;
        self.account.update_settings(|settings| {
            let analytics = settings
                .snippets
                .iter()
                .find(|kept| kept.id == snippet.id)
                .map(|kept| kept.analytics.clone())
                .unwrap_or_default();
            let saved = Snippet {
                date: at.clone(),
                analytics,
                ..snippet
            };
            match settings
                .snippets
                .iter_mut()
                .find(|kept| kept.id == saved.id)
            {
                Some(kept) => *kept = saved,
                None => settings.snippets.push(saved),
            }
            settings.snippets_last_used.insert(id.clone(), at);
            if settings.use_snippet_hint_count < DONE_HINTS {
                settings.use_snippet_hint_count += 1;
                hint = true;
            }
        });
        self.close_snippet();
        if hint {
            self.commands.hint(
                Hint::new()
                    .text("To use a Snippet, hit")
                    .keys(["⌘", ";"])
                    .text("at any time!"),
            );
        }
    }

    /// `Create Snippet from Draft`: a new snippet with the focused draft's recipients, subject,
    /// body and files.
    pub(crate) fn snippet_from_draft(&mut self) {
        let Some(open) = self
            .compose
            .focused_mut()
            .filter(|open| open.persists_to_gmail())
        else {
            return;
        };
        open.sync_body();
        let record = open.record.clone();
        let snippet = Snippet {
            to: record.to,
            cc: record.cc,
            bcc: record.bcc,
            subject: record.subject,
            body: record.body,
            attachments: record.attachments,
            ..Snippet::default()
        };
        self.new_snippet_from(snippet);
    }

    /// `Create Snippet from Message`: a new snippet with the focused message's recipients,
    /// subject and unquoted body.
    pub(crate) fn snippet_from_message(&mut self) {
        let focused = self
            .thread
            .as_ref()
            .and_then(crate::thread::ThreadView::focused)
            .map(str::to_owned);
        let Some((message, html)) = self
            .copyable()
            .into_iter()
            .find(|(message, _)| Some(&message.id) == focused.as_ref())
        else {
            return;
        };
        let fields = Fields::of_message(message, html);
        let snippet = Snippet {
            to: fields.to,
            cc: fields.cc,
            bcc: fields.bcc,
            subject: fields.subject,
            body: fields.body,
            ..Snippet::default()
        };
        self.new_snippet_from(snippet);
    }

    /// A new, unsaved snippet holding `snippet`'s fields.
    fn new_snippet_from(&mut self, snippet: Snippet) {
        let mut record =
            factory::compose(&self.compose_account(), Fields::default(), Timestamp::now());
        record.from = None;
        record.to = snippet.to;
        record.cc = snippet.cc;
        record.bcc = snippet.bcc;
        record.subject = snippet.subject;
        record.body = snippet.body;
        record.attachments = snippet.attachments;
        let mut open = Open::new(factory::new_id(), None, record);
        let shown = open.recipients().next().is_some() || !open.record.subject.is_empty();
        open.set_snippet(String::new(), None, shown);
        self.open_snippet_pane(open);
    }

    /// ⌘; with no snippets opens the `Snippets` alert, which offers to create one.
    pub(crate) fn ask_create_snippet(&mut self) {
        let draft = self.compose.focus.clone().unwrap_or_default();
        // Two `<br>`s make a blank line, not a paragraph.
        let text = "Instantly insert phrases, paragraphs, or whole emails.\n\nType less, do more!";
        self.compose.alert = Some(Alert::new(
            Some("Snippets"),
            text,
            vec!["Create Snippet", "Cancel"],
            draft,
            AlertThen::CreateSnippet,
        ));
    }

    /// Applies `snippet` to the focused draft.
    pub(crate) fn compose_apply_snippet(&mut self, snippet: &Snippet, at_start: bool, now: f64) {
        let aliases = self.snapshot.aliases.clone();
        let Some((open, book)) = self.compose.focused_with_book() else {
            return;
        };
        super::snippets::apply(open, snippet, &aliases, book, at_start, now);
    }

    /// `Use Snippet Inline` types a `;`, which opens the picker. In a new message's subject it
    /// goes in at the caret ([`super::subject::use_snippet`]). Otherwise the body gets the
    /// keyboard, with its caret at the end unless the body already had the keyboard, and the `;`
    /// goes after a no-break space when the caret follows a word.
    pub(crate) fn snippet_inline(&mut self, now: f64) {
        let Some(open) = self.compose.focused_mut() else {
            return;
        };
        if open.state.field == Field::Subject && open.is_new_email() {
            super::subject::use_snippet(open);
            open.touch(now);
            return;
        }
        if open.state.field != Field::Body {
            open.state.field = Field::Body;
            let end = open.editor.document().end();
            open.editor.set_selection(exo_editor::Selection::caret(end));
        }
        let caret = open.editor.selection().focus;
        let before = open
            .editor
            .document()
            .blocks
            .get(caret.block)
            .map(|block| block.text().chars().take(caret.offset).last());
        let after_word = before.flatten().is_some_and(|ch| !ch.is_whitespace());
        open.editor
            .type_text(if after_word { "\u{a0};" } else { ";" });
        open.touch(now);
    }

    /// Keeps the `;` picker's rows for the focused draft.
    pub(super) fn inline_snippet_rows(&mut self) {
        let settings = self.account.settings();
        let Some(open) = self
            .compose
            .focus
            .clone()
            .and_then(|id| self.compose.get_mut(&id))
        else {
            return;
        };
        let Some(query) = open
            .popups
            .snippets
            .trigger
            .as_ref()
            .map(|trigger| trigger.query.clone())
        else {
            open.popups.snippets.rows.clear();
            open.popups.snippets.hint = false;
            return;
        };
        // A snippet being edited isn't offered in itself.
        let offered: Vec<Snippet> = settings
            .snippets
            .iter()
            .filter(|snippet| snippet.id != open.id)
            .cloned()
            .collect();
        let rows: Vec<super::draft::SnippetRow> =
            super::snippets::rank(&offered, &settings.snippets_last_used, &query)
                .into_iter()
                .map(|snippet| super::draft::SnippetRow {
                    id: snippet.id.clone(),
                    name: super::snippets::name(snippet).to_owned(),
                    preview: super::snippets::preview(snippet),
                })
                .collect();
        if rows != open.popups.snippets.rows {
            open.popups.snippets.pick = 0;
            open.popups.snippets.rows = rows;
        }
        open.popups.snippets.hint = query.is_empty() && settings.snippets.is_empty();
    }

    /// Enter, Tab or a press in the `;` picker removes the typed `;query` and puts the snippet in
    /// its place.
    pub(super) fn pick_inline_snippet(&mut self, index: Option<usize>, now: f64) {
        let Some(open) = self.compose.focused_mut() else {
            return;
        };
        let Some(row) = open
            .popups
            .snippets
            .rows
            .get(index.unwrap_or(open.popups.snippets.pick))
            .cloned()
        else {
            return;
        };
        if let Some(trigger) = open.popups.snippets.trigger.take() {
            if open.state.field == Field::Subject {
                super::subject::replace(open, &trigger, "");
            } else {
                open.editor.replace(trigger.start, trigger.end, "");
            }
        }
        open.popups.snippets.rows.clear();
        let Some(snippet) = self
            .account
            .settings()
            .snippets
            .iter()
            .find(|snippet| snippet.id == row.id)
            .cloned()
        else {
            return;
        };
        self.compose_apply_snippet(&snippet, false, now);
        self.account.update_settings(|settings| {
            settings
                .snippets_last_used
                .insert(snippet.id.clone(), Timestamp::now().to_string());
        });
    }

    /// Discard on a snippet, which asks first when there are unsaved changes (`Discard changes?`).
    pub(super) fn ask_discard_snippet(&mut self) {
        let Some(open) = self.compose.focused() else {
            return;
        };
        if open.is_snippet_dirty() {
            let draft = open.id.clone();
            self.compose.alert = Some(Alert::new(
                Some("Discard changes?"),
                "Your changes will not be saved.",
                vec!["Discard", "Cancel"],
                draft,
                AlertThen::DiscardSnippet,
            ));
        } else {
            self.discard_snippet();
        }
    }

    /// Discards a snippet, removing it from the folder, with `Snippet discarded.` and its Undo.
    pub(super) fn discard_snippet(&mut self) {
        let Some(id) = self
            .compose
            .focus
            .clone()
            .or_else(|| self.compose.pane().map(str::to_owned))
        else {
            return;
        };
        self.close_snippet();
        self.discard_saved_snippet(&id);
    }

    /// Removes snippet `id` from the settings, with `Snippet discarded.` and its Undo. A snippet
    /// that was never saved has nothing for Undo to bring back.
    pub(crate) fn discard_saved_snippet(&mut self, id: &str) {
        let Some(saved) = self
            .account
            .settings()
            .snippets
            .iter()
            .find(|snippet| snippet.id == id)
            .cloned()
        else {
            self.commands.toast(Toast::new("Snippet discarded."));
            return;
        };
        self.account
            .update_settings(|settings| settings.snippets.retain(|snippet| snippet.id != id));
        let tag = self.compose.tag();
        self.compose.discarded_snippet = Some((tag, saved));
        self.commands.toast(
            Toast::new("Snippet discarded.")
                .undo(commands::Action::Compose(Action::UndoDiscardSnippet(tag)))
                .tag(tag),
        );
    }

    /// The toast's Undo, which brings the discarded snippet back.
    pub(super) fn undo_discard_snippet(&mut self, tag: u64) {
        let Some((_, snippet)) = self
            .compose
            .discarded_snippet
            .take_if(|(kept, _)| *kept == tag)
        else {
            return;
        };
        self.account.update_settings(|settings| {
            if !settings.snippets.iter().any(|kept| kept.id == snippet.id) {
                settings.snippets.push(snippet);
            }
        });
    }

    /// Escape in a snippet closes an empty new one. Otherwise the form lets go of the keyboard.
    pub(super) fn escape_snippet(&mut self) {
        let Some(open) = self.compose.focused() else {
            return;
        };
        if open.is_new_snippet() && open.is_snippet_empty() {
            self.close_snippet();
        } else {
            self.compose.focus = None;
        }
    }

    /// Back from the snippet in the pane closes it, once `Save Snippet?`
    /// has asked about unsaved changes. Returns whether a snippet was there.
    pub(super) fn leaving_snippet_asks(&mut self) -> bool {
        if self.pane_snippet().is_none() {
            return false;
        }
        if self.snippet_unsaved() {
            self.ask_leaving_snippet(Box::new(|_| {}));
        } else {
            self.close_snippet();
        }
        true
    }

    /// Whether the snippet in the pane has changes that Done hasn't saved.
    pub(crate) fn snippet_unsaved(&self) -> bool {
        self.pane_snippet().is_some_and(Open::is_snippet_dirty)
    }

    /// The snippet open in the pane.
    pub(super) fn pane_snippet(&self) -> Option<&Open> {
        let id = self.compose.pane()?;
        self.compose
            .drafts
            .iter()
            .find(|open| open.id == id)
            .filter(|open| open.is_snippet())
    }

    /// Leaving the snippet in the pane with unsaved changes, for `then`. `Save Snippet?` asks
    /// first, and `then` runs after Save or Don't save (Delete for a snippet never saved), not
    /// after Cancel.
    pub(crate) fn ask_leaving_snippet(&mut self, then: Leaving) {
        let Some(open) = self.pane_snippet() else {
            return;
        };
        let never_saved = open.is_new_snippet();
        let actions = vec![
            "Save",
            "Cancel",
            if never_saved { "Delete" } else { "Don't save" },
        ];
        let mut alert = Alert::new(
            Some("Save Snippet?"),
            "Would you like to save changes to this Snippet? \n You cannot undo this action.",
            actions,
            open.id.clone(),
            AlertThen::SaveSnippet,
        );
        alert.escape = 1;
        self.compose.alert = Some(alert);
        self.snippets.leaving = Some(then);
    }

    /// `Save Snippet?`'s answer (`index` of Save, Cancel, Don't save) about snippet `id`.
    pub(super) fn answer_save_snippet(&mut self, index: usize, id: String) {
        let then = self.snippets.leaving.take();
        match index {
            // Save the snippet, which gets the keyboard first since a popped-out draft may have
            // it.
            0 => {
                self.compose.focus = Some(id.clone());
                self.save_snippet();
            }
            1 => {
                self.compose.focus = Some(id);
                return;
            }
            _ => self.close_snippet(),
        }
        // Saving stops at a check's alert, with the snippet still open.
        if self.compose.pane() != Some(id.as_str())
            && let Some(then) = then
        {
            then(self);
        }
    }

    /// Closes the snippet in the pane and drops unsaved changes. The folder shows.
    pub(crate) fn close_snippet(&mut self) {
        let Some((id, _)) = self.compose.pane.take_if(|(id, _)| {
            self.compose
                .drafts
                .iter()
                .any(|open| open.id == *id && open.is_snippet())
        }) else {
            return;
        };
        self.compose.drafts.retain(|open| open.id != id);
        if self.compose.focus.as_deref() == Some(id.as_str()) {
            self.compose.focus = None;
        }
    }
}

/// The pane's title for a snippet.
pub(crate) fn pane_title(open: &Open) -> &'static str {
    if open.is_saved_snippet() {
        "Edit Private Snippet"
    } else {
        "New Private Snippet"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::compose::draft::List;

    #[test]
    fn save_snippet_commits_uncommitted_recipient_inputs_before_checks() {
        let ctx = egui::Context::default();
        let _ = exo_ui::install(&ctx);
        let dir = std::env::temp_dir().join(format!(
            "exo-snippet-commit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints =
            gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
        let account = crate::account::Account::open(
            &dir,
            crate::account::Credentials {
                email: "maya@brightmoor.co".into(),
                endpoints,
                tokens: Arc::new(gmail::StaticToken("dev".into())),
            },
            ctx.clone(),
        )
        .expect("account opens");
        let mut mail =
            crate::Mail::new(&ctx, &crate::assets_dir(), account, crate::Clock::system());

        mail.create_snippet("Follow up".into());
        mail.compose
            .focused_mut()
            .expect("snippet open")
            .input_mut(List::To)
            .text = "priya@brightmoor.co".into();
        mail.save_snippet();

        let saved = mail
            .account
            .settings()
            .snippets
            .iter()
            .find(|snippet| snippet.name == "Follow up")
            .expect("snippet saved");
        assert_eq!(saved.to.len(), 1);
        assert_eq!(saved.to[0].email, "priya@brightmoor.co");

        // An invalid uncommitted recipient is committed and rejected by checks.
        mail.create_snippet("Bad".into());
        mail.compose
            .focused_mut()
            .expect("snippet open")
            .input_mut(List::To)
            .text = "not-an-email".into();
        mail.save_snippet();
        assert!(
            mail.compose.alert.is_some(),
            "invalid address triggers alert"
        );
        assert!(
            mail.account
                .settings()
                .snippets
                .iter()
                .all(|snippet| snippet.name != "Bad"),
            "invalid snippet was not saved"
        );

        mail.discard();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
