//! People: the signed-in user (`/me`), the directory (`/users` and its delta), saved contacts
//! (`/me/contacts`, its delta and the contact folders), relevant people (`/me/people`) and
//! profile photos, all from the same people as Google's People API serves.

use std::collections::HashMap;

use axum::extract::Path;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use gmail::people::Person;
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::filter::Expr;
use super::ids::{self, Id};
use super::messages::entity;
use super::{Call, odata, origin, reply, reply_map};
use crate::people::Kind;
use crate::world::World;

const USER_PROPERTIES: &[&str] = &[
    "id",
    "businessPhones",
    "displayName",
    "givenName",
    "jobTitle",
    "mail",
    "mobilePhone",
    "officeLocation",
    "preferredLanguage",
    "surname",
    "userPrincipalName",
];

/// The object ID of the directory user with `email`: from their People resource name, so a
/// deleted user's tombstone has it too.
pub fn user_id(world: &World, email: &str) -> String {
    let people = &world.people;
    let resource = std::iter::once(&people.me)
        .chain(&people.directory)
        .find(|person| email_of(person).is_some_and(|e| e.eq_ignore_ascii_case(email)))
        .and_then(|person| person.resource_name.clone())
        .unwrap_or_else(|| email.to_lowercase());
    ids::guid(&format!("user {resource}"))
}

fn email_of(person: &Person) -> Option<&str> {
    person.email_addresses.first()?.value.as_deref()
}

fn names(person: &Person) -> (String, Option<String>, Option<String>) {
    let name = person.names.first();
    let display = name
        .and_then(|n| n.display_name.clone())
        .or_else(|| email_of(person).map(str::to_owned))
        .unwrap_or_default();
    (
        display,
        name.and_then(|n| n.given_name.clone()),
        name.and_then(|n| n.family_name.clone()),
    )
}

fn phone(person: &Person) -> Option<(String, bool)> {
    let number = person.phone_numbers.first()?;
    let mobile = number
        .kind
        .as_deref()
        .or(number.formatted_type.as_deref())
        .is_some_and(|kind| kind.eq_ignore_ascii_case("mobile"));
    Some((number.value.clone()?, mobile))
}

/// A directory person as a Graph user.
fn user_json(world: &World, person: &Person) -> Map<String, Value> {
    let (display, given, surname) = names(person);
    let email = email_of(person).unwrap_or_default().to_owned();
    let organization = person.organizations.first();
    let mut json = Map::new();
    json.insert(
        "businessPhones".into(),
        phone(person)
            .map(|(n, _)| vec![n])
            .unwrap_or_default()
            .into(),
    );
    json.insert("displayName".into(), display.into());
    json.insert("givenName".into(), given.map_or(Value::Null, Value::from));
    json.insert(
        "jobTitle".into(),
        organization
            .and_then(|o| o.title.clone())
            .map_or(Value::Null, Value::from),
    );
    json.insert("mail".into(), email.clone().into());
    json.insert("mobilePhone".into(), Value::Null);
    json.insert("officeLocation".into(), Value::Null);
    json.insert("preferredLanguage".into(), "en-US".into());
    json.insert("surname".into(), surname.map_or(Value::Null, Value::from));
    json.insert("userPrincipalName".into(), email.clone().into());
    json.insert("id".into(), user_id(world, &email).into());
    json
}

/// The directory's users: the owner and the colleagues.
fn directory(world: &World) -> Vec<&Person> {
    std::iter::once(&world.people.me)
        .chain(&world.people.directory)
        .collect()
}

fn work_only(call: &Call) -> Result<()> {
    if call.who.personal() {
        return Err(GraphError::new(
            403,
            "Authorization_RequestDenied",
            "Insufficient privileges to complete the operation.",
        ));
    }
    Ok(())
}

/// `GET /me`.
pub async fn me(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let mut json = if call.who.personal() {
        let (given, surname) = (
            world.account.given_name.clone(),
            world.account.family_name.clone(),
        );
        let mut json = Map::new();
        json.insert("businessPhones".into(), Value::Array(Vec::new()));
        json.insert("displayName".into(), call.who.name.clone().into());
        json.insert("givenName".into(), given.into());
        json.insert("jobTitle".into(), Value::Null);
        json.insert("mail".into(), call.who.email.clone().into());
        json.insert("mobilePhone".into(), Value::Null);
        json.insert("officeLocation".into(), Value::Null);
        json.insert("preferredLanguage".into(), Value::Null);
        json.insert("surname".into(), surname.into());
        json.insert("userPrincipalName".into(), call.who.email.clone().into());
        json.insert("id".into(), call.who.id.clone().into());
        json
    } else {
        let person = directory(&world)
            .into_iter()
            .find(|person| {
                email_of(person).is_some_and(|e| e.eq_ignore_ascii_case(&call.who.email))
            })
            .cloned()
            .unwrap_or_else(|| world.people.me.clone());
        let mut json = user_json(&world, &person);
        json.insert("id".into(), call.who.id.clone().into());
        json
    };
    json = odata::select(
        json,
        call.options.select.as_deref(),
        USER_PROPERTIES,
        "user",
    )?;
    Ok(reply_map(
        200,
        entity(odata::context(&origin, "users/$entity"), json),
    ))
}

/// `GET /users`: the directory, filtered and paged.
pub async fn users(call: Call) -> Result<Response> {
    work_only(&call)?;
    let world = call.world();
    let origin = origin(&world);
    let filter = call
        .options
        .filter
        .as_deref()
        .map(Expr::parse)
        .transpose()?;
    let mut all = Vec::new();
    for person in directory(&world) {
        let json = user_json(&world, person);
        if filter
            .as_ref()
            .is_none_or(|filter| filter.matches(&Value::Object(json.clone())))
        {
            all.push(json);
        }
    }
    let total = all.len();
    let top = call.options.top.unwrap_or(100).clamp(1, 999);
    let skip = match call.options.skiptoken.as_deref() {
        Some(token) => {
            decode_offset(token).ok_or_else(|| GraphError::bad_request("Invalid $skiptoken."))?
        }
        None => 0,
    };
    let value = all
        .into_iter()
        .skip(skip)
        .take(top)
        .map(|json| {
            odata::select(
                json,
                call.options.select.as_deref(),
                USER_PROPERTIES,
                "user",
            )
            .map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let next = (skip + top < total).then(|| {
        let mut params: Vec<(String, String)> = url::form_urlencoded::parse(call.query.as_bytes())
            .into_owned()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("$skiptoken"))
            .collect();
        params.push(("$skiptoken".into(), encode_offset(skip + top)));
        odata::link(&origin, "users", &params)
    });
    let fragment = match &call.options.select {
        Some(select) => format!("users({})", select.join(",")),
        None => "users".to_owned(),
    };
    Ok(reply(
        200,
        odata::collection(
            odata::context(&origin, &fragment),
            value,
            call.options.count.then_some(total),
            next,
        ),
    ))
}

fn encode_offset(offset: usize) -> String {
    odata::encode_text_token(format!("offset {offset}"))
}

fn decode_offset(token: &str) -> Option<usize> {
    odata::decode_text_token(token)?
        .strip_prefix("offset ")?
        .parse()
        .ok()
}

/// `GET /users/{id or userPrincipalName}`.
pub async fn user(call: Call, Path(params): super::Params) -> Result<Response> {
    work_only(&call)?;
    let world = call.world();
    let origin = origin(&world);
    let person = find_user(&world, &params["id"])?;
    let json = odata::select(
        user_json(&world, person),
        call.options.select.as_deref(),
        USER_PROPERTIES,
        "user",
    )?;
    Ok(reply_map(
        200,
        entity(odata::context(&origin, "users/$entity"), json),
    ))
}

fn find_user<'a>(world: &'a World, id: &str) -> Result<&'a Person> {
    directory(world)
        .into_iter()
        .find(|person| {
            let email = email_of(person).unwrap_or_default();
            email.eq_ignore_ascii_case(id) || user_id(world, email) == id
        })
        .ok_or_else(|| GraphError::new(404, "Request_ResourceNotFound", format!("Resource '{id}' does not exist or one of its queried reference-property objects are not present.")))
}

/// A sync token for one of the People lists: changes after `since` (everything when `None`),
/// from entry `at`, with the round's `$select`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeopleToken {
    s: Option<u64>,
    t: u64,
    i: usize,
    q: Option<Vec<String>>,
}

/// Which People list a delta reads, and how Graph names it.
struct DeltaList<'a> {
    kind: Kind,
    path: &'a str,
    context: &'a str,
    removed_reason: &'a str,
    properties: &'a [&'a str],
    type_name: &'a str,
}

/// A delta over a People list: the list, then who was added, changed or removed since.
fn people_delta(
    call: &Call,
    world: &World,
    list: &DeltaList<'_>,
    render: impl Fn(&Person) -> Map<String, Value>,
    id_of: impl Fn(&Person) -> String,
) -> Result<Response> {
    let DeltaList {
        kind,
        path,
        context,
        removed_reason,
        properties,
        type_name,
    } = *list;
    let origin = origin(world);
    let people = &world.people;
    let latest = call.options.deltatoken.as_deref() == Some("latest");
    let token = match call
        .options
        .skiptoken
        .as_deref()
        .or(call.options.deltatoken.as_deref())
        .filter(|_| !latest)
    {
        Some(text) => {
            let mut token: PeopleToken =
                odata::decode_token(text).ok_or_else(GraphError::sync_state_not_found)?;
            if token.s.is_some_and(|since| since < people.floor) {
                return Err(GraphError::sync_state_not_found());
            }
            if call.options.deltatoken.is_some() {
                token.t = people.revision;
                token.i = 0;
            }
            token
        }
        None => {
            if let Some(select) = &call.options.select {
                odata::check_properties(select, properties, type_name)?;
            }
            PeopleToken {
                s: latest.then_some(people.revision),
                t: people.revision,
                i: 0,
                q: call.options.select.clone(),
            }
        }
    };
    let entries: Vec<Value> = if latest {
        Vec::new()
    } else {
        let changed: Vec<Person> = match token.s {
            None => match kind {
                Kind::Directory => directory(world).into_iter().cloned().collect(),
                kind => people.list(kind).to_vec(),
            },
            Some(since) => people.changes_since(kind, since),
        };
        changed
            .iter()
            .map(|person| {
                if person.metadata.as_ref().and_then(|m| m.deleted) == Some(true) {
                    Ok(json!({ "id": id_of(person), "@removed": { "reason": removed_reason } }))
                } else {
                    odata::select(render(person), token.q.as_deref(), properties, type_name)
                        .map(Value::Object)
                }
            })
            .collect::<Result<_>>()?
    };
    let size = call.prefer.max_page.unwrap_or(100).min(1000);
    let page: Vec<Value> = entries.iter().skip(token.i).take(size).cloned().collect();
    let mut body = Map::new();
    body.insert(
        "@odata.context".into(),
        odata::context(&origin, context).into(),
    );
    if token.i + size < entries.len() {
        let next = PeopleToken {
            i: token.i + size,
            ..token
        };
        body.insert(
            "@odata.nextLink".into(),
            odata::link(
                &origin,
                path,
                &[("$skiptoken".into(), odata::encode_token(&next))],
            )
            .into(),
        );
    } else {
        let done = PeopleToken {
            s: Some(token.t),
            i: 0,
            ..token
        };
        body.insert(
            "@odata.deltaLink".into(),
            odata::link(
                &origin,
                path,
                &[("$deltatoken".into(), odata::encode_token(&done))],
            )
            .into(),
        );
    }
    body.insert("value".into(), page.into());
    Ok(reply_map(200, body))
}

/// `GET /users/delta`. A user taken off the directory comes back as `@removed`, reason
/// `changed` (deleted, and restorable for a while).
pub async fn users_delta(call: Call) -> Result<Response> {
    work_only(&call)?;
    let world = call.world();
    let list = DeltaList {
        kind: Kind::Directory,
        path: "users/delta",
        context: "users",
        removed_reason: "changed",
        properties: USER_PROPERTIES,
        type_name: "user",
    };
    people_delta(
        &call,
        &world,
        &list,
        |person| user_json(&world, person),
        |person| {
            ids::guid(&format!(
                "user {}",
                person.resource_name.as_deref().unwrap_or_default()
            ))
        },
    )
}

/// The default Contacts folder's ID.
fn contacts_folder() -> String {
    ids::contact("folders/contacts")
}

const CONTACT_PROPERTIES: &[&str] = &[
    "id",
    "createdDateTime",
    "lastModifiedDateTime",
    "changeKey",
    "categories",
    "parentFolderId",
    "birthday",
    "fileAs",
    "displayName",
    "givenName",
    "initials",
    "middleName",
    "nickName",
    "surname",
    "title",
    "yomiGivenName",
    "yomiSurname",
    "yomiCompanyName",
    "generation",
    "imAddresses",
    "jobTitle",
    "companyName",
    "department",
    "officeLocation",
    "profession",
    "businessHomePage",
    "assistantName",
    "manager",
    "homePhones",
    "mobilePhone",
    "businessPhones",
    "spouseName",
    "personalNotes",
    "children",
    "emailAddresses",
    "homeAddress",
    "businessAddress",
    "otherAddress",
];

/// A saved contact as an Outlook contact.
fn contact_json(world: &World, person: &Person) -> Map<String, Value> {
    let (display, given, surname) = names(person);
    let resource = person.resource_name.clone().unwrap_or_default();
    let revision = world.people.changed.get(&resource).copied().unwrap_or(1);
    let at = world.config.now.timestamp();
    let organization = person.organizations.first();
    let text = |value: Option<String>| value.map_or(Value::Null, Value::from);
    let (business, mobile) = match phone(person) {
        Some((number, true)) => (Vec::new(), Some(number)),
        Some((number, false)) => (vec![number], None),
        None => (Vec::new(), None),
    };
    let mut json = Map::new();
    json.insert(
        "@odata.etag".into(),
        format!("W/\"{}\"", ids::change_key(1, revision)).into(),
    );
    json.insert("id".into(), ids::contact(&resource).into());
    json.insert("createdDateTime".into(), odata::datetime(at).into());
    json.insert("lastModifiedDateTime".into(), odata::datetime(at).into());
    json.insert("changeKey".into(), ids::change_key(1, revision).into());
    json.insert("categories".into(), Value::Array(Vec::new()));
    json.insert("parentFolderId".into(), contacts_folder().into());
    json.insert("birthday".into(), Value::Null);
    json.insert("fileAs".into(), "".into());
    json.insert("displayName".into(), display.into());
    json.insert("givenName".into(), text(given));
    json.insert("initials".into(), Value::Null);
    json.insert("middleName".into(), Value::Null);
    json.insert("nickName".into(), Value::Null);
    json.insert("surname".into(), text(surname));
    json.insert("title".into(), Value::Null);
    json.insert("yomiGivenName".into(), Value::Null);
    json.insert("yomiSurname".into(), Value::Null);
    json.insert("yomiCompanyName".into(), Value::Null);
    json.insert("generation".into(), Value::Null);
    json.insert("imAddresses".into(), Value::Array(Vec::new()));
    json.insert(
        "jobTitle".into(),
        text(
            organization
                .and_then(|o| o.title.clone())
                .filter(|t| !t.is_empty()),
        ),
    );
    json.insert(
        "companyName".into(),
        text(
            organization
                .and_then(|o| o.name.clone())
                .filter(|n| !n.is_empty()),
        ),
    );
    json.insert(
        "department".into(),
        text(organization.and_then(|o| o.department.clone())),
    );
    json.insert("officeLocation".into(), Value::Null);
    json.insert("profession".into(), Value::Null);
    json.insert("businessHomePage".into(), Value::Null);
    json.insert("assistantName".into(), Value::Null);
    json.insert("manager".into(), Value::Null);
    json.insert("homePhones".into(), Value::Array(Vec::new()));
    json.insert("mobilePhone".into(), text(mobile));
    json.insert("businessPhones".into(), business.into());
    json.insert("spouseName".into(), Value::Null);
    json.insert("personalNotes".into(), "".into());
    json.insert("children".into(), Value::Array(Vec::new()));
    json.insert(
        "emailAddresses".into(),
        person
            .email_addresses
            .iter()
            .filter_map(|address| Some(json!({ "name": address.display_name.clone().unwrap_or_else(|| address.value.clone().unwrap_or_default()), "address": address.value.clone()? })))
            .collect::<Vec<_>>()
            .into(),
    );
    for address in ["homeAddress", "businessAddress", "otherAddress"] {
        json.insert(address.into(), json!({}));
    }
    json
}

fn check_contacts_folder(params: &HashMap<String, String>) -> Result<()> {
    match params.get("folder") {
        Some(folder) if *folder != contacts_folder() => Err(GraphError::folder_not_found()),
        _ => Ok(()),
    }
}

/// `GET /me/contacts`.
pub async fn contacts(call: Call) -> Result<Response> {
    list_contacts(&call)
}

/// `GET /me/contactFolders/{id}/contacts`, for the default folder.
pub async fn folder_contacts(call: Call, Path(params): super::Params) -> Result<Response> {
    check_contacts_folder(&params)?;
    list_contacts(&call)
}

fn list_contacts(call: &Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let filter = call
        .options
        .filter
        .as_deref()
        .map(Expr::parse)
        .transpose()?;
    let all: Vec<Map<String, Value>> = world
        .people
        .contacts
        .iter()
        .map(|person| contact_json(&world, person))
        .filter(|json| {
            filter
                .as_ref()
                .is_none_or(|f| f.matches(&Value::Object(json.clone())))
        })
        .collect();
    let total = all.len();
    let top = call.options.top.unwrap_or(10).clamp(1, 1000);
    let skip = call.options.skip;
    let value = all
        .into_iter()
        .skip(skip)
        .take(top)
        .map(|json| {
            odata::select(
                json,
                call.options.select.as_deref(),
                CONTACT_PROPERTIES,
                "contact",
            )
            .map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let next = (skip + top < total).then(|| {
        odata::link(
            &origin,
            "me/contacts",
            &odata::next_page(&call.query, skip + top),
        )
    });
    Ok(reply(
        200,
        odata::collection(
            view.context("contacts"),
            value,
            call.options.count.then_some(total),
            next,
        ),
    ))
}

/// `GET /me/contacts/delta`. A contact taken off the list comes back as `@removed`.
pub async fn contacts_delta(call: Call) -> Result<Response> {
    delta_contacts(&call, "me/contacts/delta")
}

/// `GET /me/contactFolders/{id}/contacts/delta`, for the default folder.
pub async fn folder_contacts_delta(call: Call, Path(params): super::Params) -> Result<Response> {
    check_contacts_folder(&params)?;
    delta_contacts(
        &call,
        &format!("me/contactFolders/{}/contacts/delta", params["folder"]),
    )
}

fn delta_contacts(call: &Call, path: &str) -> Result<Response> {
    let world = call.world();
    let list = DeltaList {
        kind: Kind::Contacts,
        path,
        context: "Collection(contact)",
        removed_reason: "deleted",
        properties: CONTACT_PROPERTIES,
        type_name: "contact",
    };
    people_delta(
        call,
        &world,
        &list,
        |person| {
            let mut json = Map::new();
            json.insert("@odata.type".into(), "#microsoft.graph.contact".into());
            json.extend(contact_json(&world, person));
            json
        },
        |person| ids::contact(person.resource_name.as_deref().unwrap_or_default()),
    )
}

/// `GET /me/contacts/{id}`.
pub async fn contact(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let Some(Id::Contact(resource)) = ids::decode(&params["id"]) else {
        return Err(GraphError::malformed_id());
    };
    let person = world
        .people
        .contacts
        .iter()
        .find(|person| person.resource_name.as_deref() == Some(&resource))
        .ok_or_else(GraphError::not_found)?;
    let json = odata::select(
        contact_json(&world, person),
        call.options.select.as_deref(),
        CONTACT_PROPERTIES,
        "contact",
    )?;
    Ok(reply_map(
        200,
        entity(view.context("contacts/$entity"), json),
    ))
}

/// `GET /me/contactFolders`: the default Contacts folder's subfolders, of which there are none.
pub async fn contact_folders(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply(
        200,
        odata::collection(view.context("contactFolders"), Vec::new(), None, None),
    ))
}

/// `GET /me/contactFolders/{id}`: the default Contacts folder.
pub async fn contact_folder(call: Call, Path(params): super::Params) -> Result<Response> {
    check_contacts_folder(&params)?;
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let json = json!({
        "@odata.context": view.context("contactFolders/$entity"),
        "id": contacts_folder(),
        "displayName": "Contacts",
        "parentFolderId": ids::contact("folders/root"),
    });
    Ok(reply(200, json))
}

/// `GET /me/people`: everyone the user writes with, most written with first.
pub async fn people(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for message in world.mail.select_messages(&crate::mail::Selection {
        query: None,
        label_ids: Vec::new(),
        include_spam_trash: false,
    }) {
        let from = message
            .mime
            .payload
            .header("From")
            .map(|from| gmail::mime::Mailbox::parse(from).email.to_lowercase());
        for address in from
            .into_iter()
            .chain(message.mime.recipients.iter().cloned())
        {
            *counts.entry(address).or_default() += 1;
        }
    }
    let me = world.account.email.to_lowercase();
    let lists = [
        (Kind::Directory, "OrganizationUser"),
        (Kind::Contacts, "PersonalContact"),
        (Kind::OtherContacts, "ImplicitContact"),
    ];
    let mut everyone: Vec<(usize, Map<String, Value>)> = Vec::new();
    for (kind, subclass) in lists {
        for person in world.people.list(kind) {
            let Some(email) = email_of(person).map(str::to_lowercase) else {
                continue;
            };
            if email == me
                || everyone
                    .iter()
                    .any(|(_, json)| json["scoredEmailAddresses"][0]["address"] == email.as_str())
            {
                continue;
            }
            let count = counts.get(&email).copied().unwrap_or_default();
            everyone.push((count, person_json(person, &email, subclass, count)));
        }
    }
    everyone.sort_by(|(a, a_json), (b, b_json)| {
        b.cmp(a).then_with(|| {
            a_json["displayName"]
                .as_str()
                .cmp(&b_json["displayName"].as_str())
        })
    });
    let search = call
        .options
        .search
        .as_deref()
        .map(|text| crate::mail::mime::tokenize(text.trim_matches('"')));
    let matching: Vec<Map<String, Value>> = everyone
        .into_iter()
        .map(|(_, json)| json)
        .filter(|json| {
            search.as_ref().is_none_or(|words| {
                let haystack = crate::mail::mime::tokenize(&format!(
                    "{} {}",
                    json["displayName"].as_str().unwrap_or_default(),
                    json["scoredEmailAddresses"][0]["address"]
                        .as_str()
                        .unwrap_or_default()
                ));
                words.iter().all(|word| {
                    haystack
                        .iter()
                        .any(|token| token.starts_with(word.as_str()))
                })
            })
        })
        .collect();
    let top = call.options.top.unwrap_or(10).clamp(1, 1000);
    let skip = call.options.skip;
    let value = matching
        .iter()
        .skip(skip)
        .take(top)
        .map(|json| {
            odata::select(
                json.clone(),
                call.options.select.as_deref(),
                PERSON_PROPERTIES,
                "person",
            )
            .map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let next = (skip + top < matching.len()).then(|| {
        odata::link(
            &origin,
            "me/people",
            &odata::next_page(&call.query, skip + top),
        )
    });
    Ok(reply(
        200,
        odata::collection(view.context("people"), value, None, next),
    ))
}

const PERSON_PROPERTIES: &[&str] = &[
    "id",
    "displayName",
    "givenName",
    "surname",
    "birthday",
    "personNotes",
    "isFavorite",
    "jobTitle",
    "companyName",
    "yomiCompany",
    "department",
    "officeLocation",
    "profession",
    "userPrincipalName",
    "imAddress",
    "scoredEmailAddresses",
    "phones",
    "postalAddresses",
    "websites",
    "personType",
];

fn person_json(person: &Person, email: &str, subclass: &str, count: usize) -> Map<String, Value> {
    let (display, given, surname) = names(person);
    let organization = person.organizations.first();
    let text = |value: Option<String>| {
        value
            .filter(|v| !v.is_empty())
            .map_or(Value::Null, Value::from)
    };
    let work = subclass == "OrganizationUser";
    let mut json = Map::new();
    json.insert(
        "id".into(),
        ids::guid(&format!("person {email}")).to_uppercase().into(),
    );
    json.insert("displayName".into(), display.into());
    json.insert("givenName".into(), text(given));
    json.insert("surname".into(), text(surname));
    json.insert("birthday".into(), "".into());
    json.insert("personNotes".into(), "".into());
    json.insert("isFavorite".into(), false.into());
    json.insert(
        "jobTitle".into(),
        text(organization.and_then(|o| o.title.clone())),
    );
    json.insert(
        "companyName".into(),
        text(organization.and_then(|o| o.name.clone())),
    );
    json.insert("yomiCompany".into(), "".into());
    json.insert(
        "department".into(),
        text(organization.and_then(|o| o.department.clone())),
    );
    json.insert("officeLocation".into(), Value::Null);
    json.insert("profession".into(), "".into());
    json.insert(
        "userPrincipalName".into(),
        if work { email.into() } else { "".into() },
    );
    json.insert(
        "imAddress".into(),
        if work {
            format!("sip:{email}").into()
        } else {
            Value::Null
        },
    );
    json.insert(
        "scoredEmailAddresses".into(),
        json!([{ "address": email, "relevanceScore": count as f64, "selectionLikelihood": "notSpecified" }]),
    );
    json.insert(
        "phones".into(),
        phone(person)
            .map(|(number, mobile)| {
                vec![
                    json!({ "type": if mobile { "mobile" } else { "business" }, "number": number }),
                ]
            })
            .unwrap_or_default()
            .into(),
    );
    json.insert("postalAddresses".into(), Value::Array(Vec::new()));
    json.insert("websites".into(), Value::Array(Vec::new()));
    json.insert(
        "personType".into(),
        json!({ "class": "Person", "subclass": subclass }),
    );
    json
}

fn image_not_found() -> GraphError {
    GraphError::new(
        404,
        "ImageNotFound",
        "Exception of type 'Microsoft.Fast.Profile.Core.Exception.ImageNotFoundException' was thrown.",
    )
}

/// The seed of a portrait, when `email` has a photo of their own.
fn photo_seed(world: &World, email: &str) -> Option<u64> {
    if email.eq_ignore_ascii_case(&world.account.email) {
        return world.account.photo.is_some().then_some(0);
    }
    let known = world
        .people
        .directory
        .iter()
        .chain(&world.people.contacts)
        .any(|person| email_of(person).is_some_and(|e| e.eq_ignore_ascii_case(email)));
    (world.config.contact_photos && known).then(|| crate::fixtures::fnv(email).max(1))
}

const PHOTO_SIZE: u32 = 240;

/// `GET /me/photo`: the photo's metadata.
pub async fn my_photo(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    photo_seed(&world, &call.who.email).ok_or_else(image_not_found)?;
    Ok(reply(
        200,
        json!({
            "@odata.context": view.context("photo/$entity"),
            "@odata.mediaContentType": "image/png",
            "@odata.mediaEtag": format!("W/\"{}\"", ids::guid(&format!("photo {}", call.who.email))),
            "id": format!("{PHOTO_SIZE}X{PHOTO_SIZE}"),
            "height": PHOTO_SIZE,
            "width": PHOTO_SIZE,
        }),
    ))
}

/// `GET /me/photo/$value`.
pub async fn my_photo_value(call: Call) -> Result<Response> {
    let world = call.world();
    let seed = photo_seed(&world, &call.who.email).ok_or_else(image_not_found)?;
    Ok((
        [(header::CONTENT_TYPE, "image/png")],
        crate::fixtures::portrait_png(PHOTO_SIZE, seed),
    )
        .into_response())
}

/// `GET /users/{id}/photo/$value`.
pub async fn user_photo_value(call: Call, Path(params): super::Params) -> Result<Response> {
    work_only(&call)?;
    let world = call.world();
    let email = email_of(find_user(&world, &params["id"])?)
        .unwrap_or_default()
        .to_owned();
    let seed = photo_seed(&world, &email).ok_or_else(image_not_found)?;
    Ok((
        [(header::CONTENT_TYPE, "image/png")],
        crate::fixtures::portrait_png(PHOTO_SIZE, seed),
    )
        .into_response())
}

/// Another user's mailbox (`/users/{id}/messages`…), which a delegated token has no rights to:
/// 403 for a user the directory has, 404 for one it hasn't.
pub async fn other_mailbox(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let user = super::percent(&params["id"]);
    match find_user(&world, &user) {
        Ok(_) => Err(GraphError::access_denied()),
        Err(_) => Err(GraphError::new(
            404,
            "ErrorInvalidUser",
            format!("The requested user '{user}' is invalid."),
        )),
    }
}
