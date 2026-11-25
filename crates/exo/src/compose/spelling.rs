//! Spelling and the right-click menu in a draft. macOS's spellchecker checks each draft's body,
//! which is corrected and completed as the account's autocorrect and autocomplete settings say.
//! The subject, and a snippet's name, are checked the way Chromium checks an `<input>`. A
//! right-click in any of them shows the shell's editable menu, with its spelling suggestions
//! first.

use exo_editor::{Named, People};
use exo_store::Address;

use crate::{
    Mail, desktop,
    format::names,
    spelling::{self, Done},
};

/// `address` named the way autocomplete takes it, with the first and last names from the
/// contact's display name. `None` without a display name.
fn named(address: &Address) -> Option<Named> {
    let full = address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(&address.email))?;
    let name = names::reverse_by_comma(full);
    let first = names::first_name(&name, &address.email);
    let last = name
        .split_once(first.as_str())
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(&name)
        .to_owned();
    Some(Named {
        first,
        last,
        full: full.to_owned(),
    })
}

/// The people a draft is from and to, for autocomplete.
fn people(record: &exo_store::DraftRecord) -> People {
    let all = |addresses: &[Address]| addresses.iter().map(named).collect();
    People {
        from: record.from.as_ref().and_then(named),
        to: all(&record.to),
        cc: all(&record.cc),
        bcc: all(&record.bcc),
    }
}

use super::draft::PeopleSource;

fn people_matches(
    cached: &Option<PeopleSource>,
    from: &Option<Address>,
    to: &[Address],
    cc: &[Address],
    bcc: &[Address],
) -> bool {
    cached
        .as_ref()
        .is_some_and(|(cached_from, cached_to, cached_cc, cached_bcc)| {
            cached_from == from
                && cached_to.as_slice() == to
                && cached_cc.as_slice() == cc
                && cached_bcc.as_slice() == bcc
        })
}

/// The words of `names` that a spellchecker may not know, which autocorrect leaves alone.
fn name_words<'a>(names: impl Iterator<Item = &'a str>) -> impl Iterator<Item = String> {
    names
        .flat_map(|name| name.split(|ch: char| !ch.is_alphabetic() && ch != '\''))
        .filter(|word| word.chars().count() > 1)
        .map(str::to_owned)
}

impl Mail {
    /// The words in the names of the people in the address book, which autocorrect leaves alone
    /// in every editor.
    pub(crate) fn contact_names(&self) -> Vec<String> {
        let book = self.compose.book.as_ref().map(|(contacts, _)| contacts);
        name_words(
            book.into_iter()
                .flat_map(|book| book.iter().filter_map(|contact| contact.name.as_deref())),
        )
        .collect()
    }

    /// Gives each open draft's body its spellchecker, greeting languages, recipient names, and
    /// cached `People`.
    pub(crate) fn compose_assist_spelling(&mut self) {
        let (autocorrect, autocomplete) =
            (self.autocorrect(), self.account.settings().autocomplete());
        let book = self
            .compose
            .book
            .as_ref()
            .map(|(contacts, _)| contacts.clone());
        let draft_names = |record: &exo_store::DraftRecord| -> Vec<String> {
            let people = book
                .iter()
                .flat_map(|book| book.iter().map(|contact| contact.name.as_deref()));
            let recipients = [&record.to, &record.cc, &record.bcc]
                .into_iter()
                .flatten()
                .map(|address| address.name.as_deref());
            name_words(people.chain(recipients).flatten()).collect()
        };
        let from = book
            .as_ref()
            .map(|book| std::sync::Arc::as_ptr(book) as usize);
        for open in &mut self.compose.drafts {
            let had_spelling = open.editor.has_spelling();
            let record = &open.record;
            let names_from = &mut open.body.names_from;
            spelling::assist(
                &mut open.editor,
                &*self.spelling,
                autocorrect,
                autocomplete,
                || {
                    *names_from = Some(from);
                    draft_names(record)
                },
            );
            if !had_spelling && open.editor.has_spelling() {
                open.editor
                    .set_greeting_languages(desktop::spelling::greeting_languages());
            }
            let people_changed = !people_matches(
                &open.body.people_source,
                &open.record.from,
                &open.record.to,
                &open.record.cc,
                &open.record.bcc,
            );
            if people_changed {
                open.editor.set_people(people(&open.record));
                open.body.people_source = Some((
                    open.record.from.clone(),
                    open.record.to.clone(),
                    open.record.cc.clone(),
                    open.record.bcc.clone(),
                ));
            }
            // Refresh the names autocorrect leaves alone once the address book or recipients change.
            if autocorrect && (open.body.names_from != Some(from) || people_changed) {
                open.body.names_from = Some(from);
                open.editor.set_names(draft_names(&open.record));
            }
        }
    }

    pub(crate) fn compose_body_editor_mut(&mut self) -> Option<&mut exo_editor::Editor> {
        self.compose.focused_mut().map(|open| &mut open.editor)
    }

    /// Applies `choice` from a right-click `menu` in draft `id`'s body editor.
    pub(crate) fn compose_editor_choice(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        menu: &exo_editor::ContextMenu,
        choice: crate::page_menu::Choice,
    ) {
        let now = ctx.input(|input| input.time);
        let Some(open) = self.compose.get_mut(id) else {
            return;
        };
        match spelling::editor_choice(ctx, &mut open.editor, menu, choice) {
            Done::Edited => open.touch(now),
            Done::Open(url) => self.open_chosen(&url),
            Done::Nothing => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_cache_detects_recipient_changes() {
        let mut to = Vec::new();
        assert!(!people_matches(&None, &None, &to, &[], &[]));
        let cached = Some((None, to.clone(), Vec::new(), Vec::new()));
        assert!(people_matches(&cached, &None, &to, &[], &[]));
        to.push(Address {
            name: Some("Priya Raman".into()),
            email: "priya@brightmoor.co".into(),
        });
        assert!(!people_matches(&cached, &None, &to, &[], &[]));
    }

    #[test]
    fn people_are_named_by_first_and_last_name() {
        let address = |name: Option<&str>, email: &str| Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        };
        let priya = named(&address(Some("Raman, Priya"), "priya@brightmoor.co")).expect("named");
        assert_eq!(
            (
                priya.first.as_str(),
                priya.last.as_str(),
                priya.full.as_str()
            ),
            ("Priya", "Raman", "Raman, Priya")
        );
        let cher = named(&address(Some("Cher"), "cher@example.com")).expect("named");
        assert_eq!(cher.last, "Cher", "no more to the name: all of it");
        assert_eq!(named(&address(None, "tom@brightmoor.co")), None);
        assert_eq!(
            named(&address(Some("tom@brightmoor.co"), "tom@brightmoor.co")),
            None,
            "an address for a name is none"
        );
    }
}
