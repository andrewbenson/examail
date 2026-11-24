//! Microsoft Graph v1.0 and the Microsoft identity platform's sign-in, serving the same mailbox
//! as Gmail: one fictional account that a client can reach as Gmail or as Outlook.
//!
//! Outlook's model differs from Gmail's, so the world is projected ([`model`]): each message sits
//! in one folder, chosen from its labels by precedence; `isRead` is the absence of UNREAD; the
//! follow-up flag is STARRED; user labels are categories; Focused Inbox follows Gmail's Primary
//! tab and IMPORTANT; Gmail's threads are conversations. What Outlook keeps and Gmail can't
//! (a completed flag, a moved message's new ID, user folders, rule names) lives in [`Outlook`].

mod batch;
mod calendar;
mod compose;
mod error;
mod event_input;
mod events;
mod filter;
mod folders;
mod http;
mod ics;
mod ids;
mod kql;
mod login;
mod messages;
mod model;
mod odata;
mod people;
mod project;
mod recurrence;
mod settings;
mod state;

use std::collections::HashMap;
use std::sync::{Arc, MutexGuard};

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::Uri;
use axum::http::request::Parts;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use tower::ServiceExt;

pub use login::{Account, CONSUMERS as CONSUMER_TENANT, TENANT};
pub use model::Folder;
pub use state::{Outlook, UserFolder};

use crate::world::{App, World};
use error::{GraphError, Result};
use odata::{Options, Prefer};
use project::View;

type Params = axum::extract::Path<HashMap<String, String>>;

/// Graph at `/v1.0/`, sign-in at `/{tenant}/oauth2/v2.0/`, and the upload sessions' URLs.
pub fn router(app: Arc<App>) -> Router {
    let api = layered(api(), &app).with_state(app.clone());
    let batch = layered(
        Router::new().route("/v1.0/$batch", post(batch::handle)),
        &app,
    )
    .with_state((app.clone(), api.clone()));
    Router::new()
        .route("/v1.0/{*rest}", any(dispatch))
        .route("/v1.0/", any(dispatch))
        .with_state((api, batch, app.clone()))
        .merge(
            Router::new()
                .route("/{tenant}/oauth2/v2.0/authorize", get(login::authorize))
                .route("/{tenant}/oauth2/v2.0/token", post(login::token))
                .route(
                    "/api/v2.0/{*rest}",
                    put(compose::upload).delete(compose::cancel_upload),
                )
                .with_state(app),
        )
}

/// Every Graph handler, behind sign-in, faults, the concurrency limit and Graph's response
/// conventions (outermost last).
fn layered<S: Clone + Send + Sync + 'static>(router: Router<S>, app: &Arc<App>) -> Router<S> {
    router
        .layer(from_fn_with_state(app.clone(), http::limit_concurrency))
        .layer(from_fn_with_state(app.clone(), http::inject_faults))
        .layer(from_fn_with_state(app.clone(), http::authenticate))
        .layer(from_fn_with_state(app.clone(), http::finish))
}

fn api() -> Router<Arc<App>> {
    let message = "/v1.0/me/messages/{id}";
    let in_folder = "/v1.0/me/mailFolders/{folder}/messages/{id}";
    let mut router = Router::new()
        .route("/v1.0/me", get(people::me))
        .route(
            "/v1.0/me/mailFolders",
            get(folders::list).post(folders::create),
        )
        .route("/v1.0/me/mailFolders/delta", get(folders::delta))
        .route(
            "/v1.0/me/mailFolders/{folder}",
            get(folders::get)
                .patch(folders::update)
                .delete(folders::delete),
        )
        .route(
            "/v1.0/me/mailFolders/{folder}/childFolders",
            get(folders::children).post(folders::create_child),
        )
        .route(
            "/v1.0/me/mailFolders/{folder}/messages",
            get(messages::list_in_folder),
        )
        .route(
            "/v1.0/me/mailFolders/{folder}/messages/delta",
            get(messages::delta),
        )
        .route(
            "/v1.0/me/mailFolders/{folder}/messageRules",
            get(settings::list_rules).post(settings::create_rule),
        )
        .route(
            "/v1.0/me/mailFolders/{folder}/messageRules/{id}",
            get(settings::get_rule)
                .patch(settings::update_rule)
                .delete(settings::delete_rule),
        )
        .route(
            "/v1.0/me/messages",
            get(messages::list).post(compose::create),
        )
        .route("/v1.0/me/sendMail", post(compose::send_mail))
        .route(
            "/v1.0/me/outlook/masterCategories",
            get(settings::list_categories).post(settings::create_category),
        )
        .route(
            "/v1.0/me/outlook/masterCategories/{id}",
            get(settings::get_category)
                .patch(settings::update_category)
                .delete(settings::delete_category),
        )
        .route(
            "/v1.0/me/mailboxSettings",
            get(settings::mailbox_settings).patch(settings::update_mailbox_settings),
        )
        .route(
            "/v1.0/me/mailboxSettings/{property}",
            get(settings::mailbox_setting),
        )
        .route(
            "/v1.0/me/inferenceClassification/overrides",
            get(settings::list_overrides).post(settings::create_override),
        )
        .route(
            "/v1.0/me/inferenceClassification/overrides/{id}",
            axum::routing::patch(settings::update_override).delete(settings::delete_override),
        )
        .route("/v1.0/me/people", get(people::people))
        .route("/v1.0/me/photo", get(people::my_photo))
        .route("/v1.0/me/photo/$value", get(people::my_photo_value))
        .route("/v1.0/me/contacts", get(people::contacts))
        .route("/v1.0/me/contacts/delta", get(people::contacts_delta))
        .route("/v1.0/me/contacts/{id}", get(people::contact))
        .route("/v1.0/me/contactFolders", get(people::contact_folders))
        .route(
            "/v1.0/me/contactFolders/{folder}",
            get(people::contact_folder),
        )
        .route(
            "/v1.0/me/contactFolders/{folder}/contacts",
            get(people::folder_contacts),
        )
        .route(
            "/v1.0/me/contactFolders/{folder}/contacts/delta",
            get(people::folder_contacts_delta),
        )
        .route("/v1.0/users", get(people::users))
        .route("/v1.0/users/delta", get(people::users_delta))
        .route("/v1.0/users/{id}", get(people::user))
        .route(
            "/v1.0/users/{id}/photo/$value",
            get(people::user_photo_value),
        )
        .route("/v1.0/users/{id}/{*rest}", any(people::other_mailbox))
        .route(
            "/v1.0/me/translateExchangeIds",
            post(messages::translate_ids),
        )
        .route("/v1.0/me/calendars", get(calendar::list_calendars))
        .route("/v1.0/me/calendar", get(calendar::get_calendar))
        .route("/v1.0/me/calendars/{calendar}", get(calendar::get_calendar));
    // A calendar's routes, for the default calendar and by ID; `/me/events` and
    // `/me/calendarView` are the default calendar's.
    for calendar in [
        "/v1.0/me",
        "/v1.0/me/calendar",
        "/v1.0/me/calendars/{calendar}",
    ] {
        let event = format!("{calendar}/events/{{id}}");
        router = router
            .route(
                &format!("{calendar}/events"),
                get(calendar::list).post(calendar::create),
            )
            .route(
                &event,
                get(calendar::get)
                    .patch(calendar::update)
                    .delete(calendar::delete),
            )
            .route(&format!("{event}/instances"), get(calendar::instances))
            .route(
                &format!("{calendar}/calendarView"),
                get(calendar::calendar_view),
            )
            .route(
                &format!("{calendar}/calendarView/delta"),
                get(calendar::view_delta),
            );
    }
    // A message's routes, under `/me/messages/{id}` and under its folder.
    for base in [message, in_folder] {
        router = router
            .route(
                base,
                get(messages::get)
                    .patch(messages::update)
                    .delete(messages::delete),
            )
            .route(&format!("{base}/$value"), get(messages::mime))
            .route(&format!("{base}/move"), post(messages::move_to))
            .route(&format!("{base}/copy"), post(messages::copy_to))
            .route(&format!("{base}/reply"), post(compose::reply))
            .route(&format!("{base}/replyAll"), post(compose::reply_all))
            .route(&format!("{base}/forward"), post(compose::forward))
            .route(
                &format!("{base}/permanentDelete"),
                post(messages::permanent_delete),
            )
            .route(&format!("{base}/send"), post(compose::send))
            .route(&format!("{base}/createReply"), post(compose::create_reply))
            .route(
                &format!("{base}/createReplyAll"),
                post(compose::create_reply_all),
            )
            .route(
                &format!("{base}/createForward"),
                post(compose::create_forward),
            )
            .route(
                &format!("{base}/attachments"),
                get(compose::list_attachments).post(compose::add_attachment),
            )
            .route(
                &format!("{base}/attachments/createUploadSession"),
                post(compose::create_upload_session),
            )
            .route(
                &format!("{base}/attachments/{{attachment}}"),
                get(compose::get_attachment).delete(compose::delete_attachment),
            )
            .route(
                &format!("{base}/attachments/{{attachment}}/$value"),
                get(compose::attachment_value),
            );
    }
    router
        .fallback(unknown_segment)
        .method_not_allowed_fallback(method_not_allowed)
}

/// Routes `/v1.0/` requests once their path is in canonical form: Graph's segments match in any
/// case, and keys may be given in parentheses (`mailFolders('inbox')`).
async fn dispatch(
    State((api, batch, app)): State<(Router, Router, Arc<App>)>,
    request: Request,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let target = own_mailbox(
        &app,
        &parts.headers,
        &canonical_target(&parts.uri.to_string()),
    );
    if let Ok(uri) = target.parse::<Uri>() {
        parts.uri = uri;
    }
    let is_batch = parts.uri.path() == "/v1.0/$batch";
    let request = Request::from_parts(parts, body);
    let router = if is_batch { batch } else { api };
    router.oneshot(request).await.into_response()
}

/// What follows `/users/{id}/` when it's a mailbox's: mail, its settings, contacts and people.
const MAILBOX: &[&str] = &[
    "mailFolders",
    "messages",
    "sendMail",
    "outlook",
    "mailboxSettings",
    "inferenceClassification",
    "contacts",
    "contactFolders",
    "people",
    "translateExchangeIds",
    "calendar",
    "calendars",
    "calendarView",
    "events",
];

/// `/users/{id or userPrincipalName}/…` for the signed-in user's own mailbox, as `/me/…`.
/// Another user's stays as it is, for [`people::other_mailbox`] to refuse.
pub fn own_mailbox(app: &App, headers: &axum::http::HeaderMap, target: &str) -> String {
    let Some(rest) = target.strip_prefix("/v1.0/users/") else {
        return target.to_owned();
    };
    let Some((user, tail)) = rest.split_once('/') else {
        return target.to_owned();
    };
    let segment = tail.split(['/', '?']).next().unwrap_or_default();
    if !MAILBOX.contains(&segment) {
        return target.to_owned();
    }
    let world = app.lock();
    let who = crate::oauth::bearer(headers).and_then(|token| login::who(&world, token).ok());
    match who {
        Some(who) if who.id == user || who.email.eq_ignore_ascii_case(&percent(user)) => {
            format!("/v1.0/me/{tail}")
        }
        _ => target.to_owned(),
    }
}

/// The names of Graph's path segments the mock serves, in their canonical case.
const SEGMENTS: &[&str] = &[
    "me",
    "users",
    "mailFolders",
    "childFolders",
    "messages",
    "messageRules",
    "attachments",
    "createUploadSession",
    "move",
    "send",
    "sendMail",
    "createReply",
    "createReplyAll",
    "createForward",
    "permanentDelete",
    "outlook",
    "masterCategories",
    "mailboxSettings",
    "inferenceClassification",
    "overrides",
    "people",
    "photo",
    "copy",
    "reply",
    "replyAll",
    "forward",
    "translateExchangeIds",
    "calendar",
    "calendars",
    "calendarView",
    "events",
    "instances",
    "contacts",
    "contactFolders",
    "delta",
    "$value",
    "$batch",
];

/// A request target (path and query) with its path in canonical form.
fn canonical_target(target: &str) -> String {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = canonical_path(path);
    if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    }
}

fn canonical_path(path: &str) -> String {
    let mut out = Vec::new();
    for segment in path.split('/') {
        let decoded = percent(segment);
        let (name, key) = match decoded.split_once("('") {
            Some((name, rest)) if rest.ends_with("')") => {
                (name.to_owned(), Some(rest[..rest.len() - 2].to_owned()))
            }
            _ => (decoded.trim_end_matches("()").to_owned(), None),
        };
        let name = SEGMENTS
            .iter()
            .find(|known| known.eq_ignore_ascii_case(&name))
            .map_or(name, |known| (*known).to_owned());
        out.push(encode_segment(&name));
        if let Some(key) = key {
            out.push(encode_segment(&key));
        }
    }
    out.join("/")
}

/// A path segment with its `%XX` escapes decoded (requiring two ASCII hex digits).
pub fn percent(segment: &str) -> String {
    if !segment.contains('%') {
        return segment.to_owned();
    }
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| segment.get(at + 1..at + 3))
            .flatten()
            .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| match ch {
            '%' => "%25".to_owned(),
            ' ' => "%20".to_owned(),
            '?' => "%3F".to_owned(),
            '#' => "%23".to_owned(),
            ch => ch.to_string(),
        })
        .collect()
}

async fn unknown_segment(uri: Uri) -> GraphError {
    let segment = uri
        .path()
        .trim_start_matches("/v1.0")
        .split('/')
        .rfind(|segment| !segment.is_empty())
        .unwrap_or_default()
        .to_owned();
    GraphError::bad_request(format!("Resource not found for the segment '{segment}'."))
}

async fn method_not_allowed() -> GraphError {
    GraphError::new(
        405,
        "Request_BadRequest",
        "Specified HTTP method is not allowed for the request target.",
    )
}

/// Who a Graph request signs in as, its preferences and its query options.
pub struct Call {
    pub app: Arc<App>,
    pub who: Account,
    pub prefer: Prefer,
    pub options: Options,
    /// The raw query, for paging links.
    pub query: String,
}

impl FromRequestParts<Arc<App>> for Call {
    type Rejection = GraphError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self> {
        let who = parts.extensions.get::<Account>().cloned().ok_or_else(|| {
            GraphError::new(401, "InvalidAuthenticationToken", "Access token is empty.")
        })?;
        let query = parts.uri.query().unwrap_or_default().to_owned();
        Ok(Self {
            app: app.clone(),
            who,
            prefer: Prefer::from_headers(&parts.headers),
            options: Options::parse(&query)?,
            query,
        })
    }
}

impl Call {
    /// The world, caught up with Gmail's changes since the last Graph request.
    pub fn world(&self) -> MutexGuard<'_, World> {
        locked(&self.app)
    }

    /// The world for a calendar request: the signed-in address's own calendars, as Google
    /// Calendar's API gives them to it, with Gmail's changes caught up.
    pub fn calendar_world(&self) -> crate::world::CalendarWorld<'_> {
        let identity = crate::oauth::Identity(Some(self.who.email.clone()));
        let mut world = self.app.lock_calendar(&identity);
        let now = world.clock.now();
        let world_ref = &mut *world;
        world_ref.graph.catch_up(&mut world_ref.mail, now);
        world
    }

    /// A view of `world` for this call's responses.
    pub fn view<'a>(&'a self, world: &'a World, origin: &'a url::Url) -> View<'a> {
        View {
            mail: &world.mail,
            outlook: &world.graph,
            origin,
            prefer: self.prefer.clone(),
            tz: &world.account.tz,
            calendars: &world.calendar,
            who: &self.who,
        }
    }
}

/// Locks the world and reads Gmail's history into Graph's change log.
pub fn locked(app: &App) -> MutexGuard<'_, World> {
    let mut world = app.lock();
    let now = world.clock.now();
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    world
}

/// Where the mock is served, which links point back to.
pub fn origin(world: &World) -> url::Url {
    world
        .config
        .origin
        .clone()
        .unwrap_or_else(|| url::Url::parse("http://127.0.0.1/").expect("a URL"))
}

/// A JSON body with Graph's status.
pub fn reply(status: u16, body: serde_json::Value) -> Response {
    let status = axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK);
    (status, axum::Json(body)).into_response()
}

/// A JSON body from parts, with Graph's status.
pub fn reply_map(status: u16, body: serde_json::Map<String, serde_json::Value>) -> Response {
    reply(status, serde_json::Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_canonical() {
        assert_eq!(
            canonical_path("/v1.0/me/mailfolders('AAMk-x=')/MESSAGES/delta()"),
            "/v1.0/me/mailFolders/AAMk-x=/messages/delta"
        );
        assert_eq!(
            canonical_path("/v1.0/me/mailFolders/Inbox"),
            "/v1.0/me/mailFolders/Inbox"
        );
        assert_eq!(
            canonical_path("/v1.0/me/messages/abc/$VALUE"),
            "/v1.0/me/messages/abc/$value"
        );
        assert_eq!(percent("%+a%-1%41"), "%+a%-1A");
    }
}
