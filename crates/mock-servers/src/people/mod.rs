//! People v1 state: the user's profile, contacts, "other contacts" and the Workspace directory.

pub mod routes;

use std::collections::HashMap;

use gmail::people::{Person, PersonMetadata};
use serde::Deserialize;

/// Google's generated avatar, which every person without a photo gets.
pub const DEFAULT_PHOTO: &str = "https://lh3.googleusercontent.com/a/default-user=s100";

/// Everyone the People API knows about.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub me: Person,
    pub contacts: Vec<Person>,
    pub other_contacts: Vec<Person>,
    pub directory: Vec<Person>,
    /// Bumped on changes, and sync tokens carry it.
    pub revision: u64,
    /// The revision each person last changed at, by resource name. The fixtures' have none.
    pub changed: HashMap<String, u64>,
    /// People taken off a list, with the revision they went at.
    pub deleted: Vec<(u64, Kind, Person)>,
    /// Tokens from before this revision are expired.
    pub floor: u64,
}

/// One of the People API's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Contacts,
    OtherContacts,
    Directory,
}

impl Directory {
    /// The people on `kind`.
    pub fn list(&self, kind: Kind) -> &[Person] {
        match kind {
            Kind::Contacts => &self.contacts,
            Kind::OtherContacts => &self.other_contacts,
            Kind::Directory => &self.directory,
        }
    }

    fn list_mut(&mut self, kind: Kind) -> &mut Vec<Person> {
        match kind {
            Kind::Contacts => &mut self.contacts,
            Kind::OtherContacts => &mut self.other_contacts,
            Kind::Directory => &mut self.directory,
        }
    }

    /// Adds `person` to `kind`, or replaces the one with its resource name.
    pub fn put(&mut self, kind: Kind, person: Person) {
        self.revision += 1;
        let resource = person.resource_name.clone().unwrap_or_default();
        self.changed.insert(resource.clone(), self.revision);
        let list = self.list_mut(kind);
        match list
            .iter_mut()
            .find(|known| known.resource_name.as_deref() == Some(&resource))
        {
            Some(known) => *known = person,
            None => list.push(person),
        }
    }

    /// Takes whoever has `email` off `kind`.
    pub fn remove(&mut self, kind: Kind, email: &str) {
        self.revision += 1;
        let revision = self.revision;
        let has = |person: &Person| {
            person
                .email_addresses
                .iter()
                .any(|address| address.value.as_deref() == Some(email))
        };
        let list = self.list_mut(kind);
        let gone: Vec<Person> = list.iter().filter(|person| has(person)).cloned().collect();
        list.retain(|person| !has(person));
        for person in gone {
            let tombstone = Person {
                resource_name: person.resource_name,
                metadata: Some(PersonMetadata {
                    deleted: Some(true),
                    ..PersonMetadata::default()
                }),
                ..Person::default()
            };
            self.deleted.push((revision, kind, tombstone));
        }
    }

    /// Expires every sync token given so far, as Google does after a week or so.
    pub fn expire(&mut self) {
        self.revision += 1;
        self.floor = self.revision;
    }

    /// What changed on `kind` since `revision`: who was added or changed, and who was deleted.
    pub fn changes_since(&self, kind: Kind, revision: u64) -> Vec<Person> {
        let changed = self.list(kind).iter().filter(|person| {
            person
                .resource_name
                .as_ref()
                .and_then(|resource| self.changed.get(resource))
                .is_some_and(|&at| at > revision)
        });
        let deleted = self
            .deleted
            .iter()
            .filter(|(at, list, _)| *at > revision && *list == kind)
            .map(|(_, _, person)| person);
        changed.chain(deleted).cloned().collect()
    }
}
