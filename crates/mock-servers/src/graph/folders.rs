//! `/me/mailFolders`: the well-known folders and user folders (each backed by a Gmail label),
//! their children, and the folder hierarchy's delta.

use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::ids::{self, Id};
use super::model::Folder;
use super::project::{FOLDER_PROPERTIES, View};
use super::state::UserFolder;
use super::{Call, odata, origin, reply, reply_map};

/// A folder named in a path: its ID or well-known name.
pub fn resolve(view: &View<'_>, name: &str) -> Result<Folder> {
    let folder = match Folder::well_known(name) {
        Some(folder) => folder,
        None => match ids::decode(name) {
            Some(Id::Folder(folder)) => folder,
            Some(_) | None => return Err(GraphError::malformed_id()),
        },
    };
    if view.has_folder(&folder) {
        Ok(folder)
    } else {
        Err(GraphError::folder_not_found())
    }
}

/// A folder named in a request body (`destinationId`, `moveToFolder`).
pub fn resolve_target(view: &View<'_>, name: &str) -> Result<Folder> {
    resolve(view, name).map_err(|error| {
        if error.status == 404 {
            error
        } else {
            GraphError::new(400, "ErrorInvalidIdMalformed", "Id is malformed.")
        }
    })
}

const MAX_PAGE: usize = 1000;

fn folder_list(
    call: &Call,
    view: &View<'_>,
    folders: Vec<Folder>,
    fragment: &str,
) -> Result<Response> {
    let hidden = call
        .options
        .custom("includeHiddenFolders")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let mut value = Vec::new();
    for folder in folders {
        let json = view.folder_json(&folder);
        if json["isHidden"] == Value::Bool(true) && !hidden {
            continue;
        }
        value.push(Value::Object(odata::select(
            json,
            call.options.select.as_deref(),
            FOLDER_PROPERTIES,
            "mailFolder",
        )?));
    }
    let total = value.len();
    let top = call.options.top.unwrap_or(10).clamp(1, MAX_PAGE);
    let skip = call.options.skip;
    let page: Vec<Value> = value.into_iter().skip(skip).take(top).collect();
    let next = (skip + top < total).then(|| {
        odata::link(
            view.origin,
            &format!("me/{fragment}"),
            &odata::next_page(&call.query, skip + top),
        )
    });
    Ok(reply(
        200,
        odata::collection(
            view.context(fragment),
            page,
            call.options.count.then_some(total),
            next,
        ),
    ))
}

/// `GET /me/mailFolders`: the top-level folders (Top of Information Store's children).
pub async fn list(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    folder_list(&call, &view, view.children(&Folder::Root), "mailFolders")
}

/// `GET /me/mailFolders/{id}/childFolders`.
pub async fn children(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let folder = resolve(&view, &params["folder"])?;
    let fragment = format!("mailFolders('{}')/childFolders", ids::folder(&folder));
    folder_list(&call, &view, view.children(&folder), &fragment)
}

/// `GET /me/mailFolders/{id}`.
pub async fn get(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let folder = resolve(&view, &params["folder"])?;
    let mut json = Map::new();
    json.insert(
        "@odata.context".into(),
        view.context("mailFolders/$entity").into(),
    );
    json.extend(odata::select(
        view.folder_json(&folder),
        call.options.select.as_deref(),
        FOLDER_PROPERTIES,
        "mailFolder",
    )?);
    Ok(reply_map(200, json))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewFolder {
    display_name: Option<String>,
    is_hidden: Option<bool>,
}

/// `POST /me/mailFolders`: a top-level user folder.
pub async fn create(call: Call, body: axum::body::Bytes) -> Result<Response> {
    make(call, Folder::Root, &body)
}

/// `POST /me/mailFolders/{id}/childFolders`.
pub async fn create_child(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let parent = {
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        resolve(&view, &params["folder"])?
    };
    make(call, parent, &body)
}

/// A new user folder under `parent`: a Gmail label named after its path (`Inbox/Invoices`),
/// hidden from Gmail's label list when the folder is hidden.
fn make(call: Call, parent: Folder, body: &[u8]) -> Result<Response> {
    let request: NewFolder = body_json(body, "mailFolder")?;
    let name = request
        .display_name
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| GraphError::invalid_request("The folder name is missing."))?;
    let mut world = call.world();
    let label_name = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        if view
            .children(&parent)
            .iter()
            .any(|sibling| view.folder_name(sibling).eq_ignore_ascii_case(name.trim()))
        {
            return Err(GraphError::new(
                409,
                "ErrorFolderExists",
                "A folder with the specified name already exists.",
            ));
        }
        match &parent {
            Folder::Root => name.trim().to_owned(),
            Folder::User(label) => format!(
                "{}/{}",
                world
                    .mail
                    .labels()
                    .iter()
                    .find(|l| l.id.as_deref() == Some(label))
                    .and_then(|l| l.name.clone())
                    .unwrap_or_default(),
                name.trim()
            ),
            known => format!(
                "{}/{}",
                known.display_name().unwrap_or_default(),
                name.trim()
            ),
        }
    };
    let hidden = request.is_hidden.unwrap_or(false);
    let label = world.mail.create_label(gmail::Label {
        name: Some(label_name),
        label_list_visibility: Some(if hidden {
            gmail::LabelListVisibility::LabelHide
        } else {
            gmail::LabelListVisibility::LabelShow
        }),
        ..gmail::Label::default()
    })?;
    let label = label.id.unwrap_or_default();
    world.graph.folders.push(UserFolder {
        label: label.clone(),
        parent,
        hidden,
    });
    world.graph.folder_changed(Folder::User(label.clone()));
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let mut json = Map::new();
    json.insert(
        "@odata.context".into(),
        view.context("mailFolders/$entity").into(),
    );
    json.extend(view.folder_json(&Folder::User(label)));
    Ok(reply_map(201, json))
}

/// `PATCH /me/mailFolders/{id}`: renames a user folder.
pub async fn update(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: NewFolder = body_json(&body, "mailFolder")?;
    let mut world = call.world();
    let folder = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        resolve(&view, &params["folder"])?
    };
    if request.is_hidden.is_some() {
        return Err(GraphError::invalid_request(
            "isHidden can only be set when the folder is created.",
        ));
    }
    let Folder::User(label) = &folder else {
        return Err(GraphError::new(
            403,
            "ErrorAccessDenied",
            "Access is denied. Check credentials and try again.",
        ));
    };
    if let Some(name) = request.display_name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(GraphError::invalid_request("The folder name is missing."));
        }
        let current = world
            .mail
            .labels()
            .iter()
            .find(|l| l.id.as_deref() == Some(label))
            .and_then(|l| l.name.clone())
            .unwrap_or_default();
        let renamed = match current.rsplit_once('/') {
            Some((path, _)) => format!("{path}/{trimmed}"),
            None => trimmed.to_owned(),
        };
        let child_renames: Vec<(String, String)> = if renamed == current {
            Vec::new()
        } else {
            let origin = origin(&world);
            let view = call.view(&world, &origin);
            let old_prefix = format!("{current}/");
            let new_prefix = format!("{renamed}/");
            descendants(&view, &folder)
                .into_iter()
                .filter_map(|child| match child {
                    Folder::User(child_label) => {
                        let child_name = world
                            .mail
                            .labels()
                            .iter()
                            .find(|l| l.id.as_deref() == Some(&child_label))
                            .and_then(|l| l.name.as_deref())?;
                        let suffix = child_name.strip_prefix(&old_prefix)?;
                        Some((child_label, format!("{new_prefix}{suffix}")))
                    }
                    _ => None,
                })
                .collect()
        };
        for (child_label, new_name) in &child_renames {
            let conflict = world.mail.labels().iter().any(|existing| {
                existing.id.as_deref() != Some(child_label.as_str())
                    && existing
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(new_name))
            });
            if conflict {
                return Err(GraphError::new(
                    409,
                    "ErrorFolderExists",
                    "A folder with the specified name already exists.",
                ));
            }
        }
        world.mail.update_label(
            label,
            gmail::Label {
                name: Some(renamed),
                ..gmail::Label::default()
            },
            true,
        )?;
        for (child_label, new_name) in child_renames {
            world.mail.update_label(
                &child_label,
                gmail::Label {
                    name: Some(new_name),
                    ..gmail::Label::default()
                },
                true,
            )?;
        }
        world.graph.folder_changed(folder.clone());
    }
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let mut json = Map::new();
    json.insert(
        "@odata.context".into(),
        view.context("mailFolders/$entity").into(),
    );
    json.extend(view.folder_json(&folder));
    Ok(reply_map(200, json))
}

/// `DELETE /me/mailFolders/{id}`: a user folder and its descendants go, and their messages with
/// them to Deleted Items.
pub async fn delete(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    let (folder, all_folders) = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let folder = resolve(&view, &params["folder"])?;
        let mut all = vec![folder.clone()];
        all.extend(descendants(&view, &folder));
        (folder, all)
    };
    let Folder::User(_) = folder else {
        return Err(GraphError::new(
            403,
            "ErrorDeleteDistinguishedFolder",
            "Distinguished folders cannot be deleted.",
        ));
    };
    let labels: Vec<String> = all_folders
        .iter()
        .filter_map(|f| match f {
            Folder::User(label) => Some(label.clone()),
            _ => None,
        })
        .collect();
    let inside: Vec<String> = world
        .graph
        .items
        .values()
        .filter(|item| all_folders.contains(&item.folder))
        .map(|item| item.gmail.clone())
        .collect();
    for gmail in inside {
        world
            .mail
            .apply_labels(&gmail, &["TRASH".into()], &["INBOX".into(), "SPAM".into()]);
    }
    for label in &labels {
        world.mail.delete_label(label)?;
    }
    world.graph.folders.retain(|f| !labels.contains(&f.label));
    for label in labels {
        world.graph.folder_changed(Folder::User(label));
    }
    let now = world.clock.now();
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

/// `GET /me/mailFolders/delta`: every folder below the root, then the ones created, renamed or
/// deleted since.
pub async fn delta(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let outlook = &world.graph;
    let all = descendants(&view, &Folder::Root);
    let token = call
        .options
        .deltatoken
        .as_deref()
        .or(call.options.skiptoken.as_deref());
    let (since, page_from) = match token {
        None => (None, 0),
        Some(token) => {
            let state: FolderToken =
                odata::decode_token(token).ok_or_else(GraphError::sync_state_not_found)?;
            if state.since.is_some_and(|since| since < outlook.floor) {
                return Err(GraphError::sync_state_not_found());
            }
            (state.since, state.at)
        }
    };
    let entries: Vec<Value> = match since {
        None => all
            .iter()
            .map(|folder| Value::Object(view.folder_json(folder)))
            .collect(),
        Some(since) => outlook
            .folder_changes(since, outlook.seq)
            .into_iter()
            .map(|folder| {
                if all.contains(&folder) {
                    Value::Object(view.folder_json(&folder))
                } else {
                    json!({ "id": ids::folder(&folder), "@removed": { "reason": "deleted" } })
                }
            })
            .collect(),
    };
    let size = call.prefer.max_page.unwrap_or(10).min(1000);
    let page: Vec<Value> = entries.iter().skip(page_from).take(size).cloned().collect();
    let more = page_from + size < entries.len();
    let mut body = Map::new();
    body.insert(
        "@odata.context".into(),
        odata::context(&origin, "Collection(mailFolder)").into(),
    );
    let (name, token) = if more {
        (
            "@odata.nextLink",
            (
                "$skiptoken",
                odata::encode_token(&FolderToken {
                    since,
                    at: page_from + size,
                }),
            ),
        )
    } else {
        (
            "@odata.deltaLink",
            (
                "$deltatoken",
                odata::encode_token(&FolderToken {
                    since: Some(outlook.seq),
                    at: 0,
                }),
            ),
        )
    };
    body.insert(
        name.into(),
        odata::link(
            &origin,
            "me/mailFolders/delta",
            &[(token.0.to_owned(), token.1)],
        )
        .into(),
    );
    body.insert("value".into(), page.into());
    Ok(reply_map(200, body))
}

/// Every folder below `folder`, depth first.
fn descendants(view: &View<'_>, folder: &Folder) -> Vec<Folder> {
    let mut out = Vec::new();
    for child in view.children(folder) {
        out.push(child.clone());
        out.extend(descendants(view, &child));
    }
    out
}

/// A folder delta's state: changes after `since` (all folders when `None`), from entry `at`.
#[derive(Serialize, Deserialize)]
struct FolderToken {
    since: Option<u64>,
    at: usize,
}

/// A JSON request body, refusing properties `kind` doesn't have.
pub fn body_json<T: serde::de::DeserializeOwned + Default>(body: &[u8], kind: &str) -> Result<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|error| {
        let text = error.to_string();
        match text
            .split('`')
            .nth(1)
            .filter(|_| text.starts_with("unknown field"))
        {
            Some(field) => GraphError::unknown_body_property(field, kind),
            None => GraphError::unreadable_body(text),
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    async fn call(
        router: &axum::Router,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = Request::builder().method(method).uri(uri).header(
            "Authorization",
            format!("Bearer {}", crate::DEV_ACCESS_TOKEN),
        );
        let payload = if let Some(json) = body {
            req = req.header("Content-Type", "application/json");
            Body::from(json.to_string())
        } else {
            Body::empty()
        };
        let response = router
            .clone()
            .oneshot(req.body(payload).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn top_zero_advances_folder_pagination() {
        let router = crate::router(&crate::Config::default());
        let (status, page) = call(&router, Method::GET, "/v1.0/me/mailFolders?$top=0", None).await;
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["value"].as_array().unwrap().len(), 1);
        let next = page["@odata.nextLink"].as_str().unwrap();
        assert!(next.contains("$skip=1"), "{next}");
    }

    #[tokio::test]
    async fn rename_and_delete_cascade_across_descendants() {
        let router = crate::router(&crate::Config::default());
        let (status, parent) = call(
            &router,
            Method::POST,
            "/v1.0/me/mailFolders",
            Some(json!({ "displayName": "OldParent" })),
        )
        .await;
        assert_eq!(status, 201, "{parent}");
        let parent_id = parent["id"].as_str().unwrap().to_owned();

        let (status, child) = call(
            &router,
            Method::POST,
            &format!("/v1.0/me/mailFolders/{parent_id}/childFolders"),
            Some(json!({ "displayName": "Child" })),
        )
        .await;
        assert_eq!(status, 201, "{child}");
        let child_id = child["id"].as_str().unwrap().to_owned();

        let (status, grand) = call(
            &router,
            Method::POST,
            &format!("/v1.0/me/mailFolders/{child_id}/childFolders"),
            Some(json!({ "displayName": "Grand" })),
        )
        .await;
        assert_eq!(status, 201, "{grand}");
        let grand_id = grand["id"].as_str().unwrap().to_owned();

        // Rename parent and confirm Gmail labels for child and grandchild pick up NewParent/.
        let (status, renamed) = call(
            &router,
            Method::PATCH,
            &format!("/v1.0/me/mailFolders/{parent_id}"),
            Some(json!({ "displayName": "NewParent" })),
        )
        .await;
        assert_eq!(status, 200, "{renamed}");
        let (status, labels) = call(&router, Method::GET, "/gmail/v1/users/me/labels", None).await;
        assert_eq!(status, 200, "{labels}");
        let label_names: Vec<&str> = labels["labels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["name"].as_str())
            .collect();
        assert!(label_names.contains(&"NewParent"), "{label_names:?}");
        assert!(label_names.contains(&"NewParent/Child"), "{label_names:?}");
        assert!(
            label_names.contains(&"NewParent/Child/Grand"),
            "{label_names:?}"
        );
        assert!(
            !label_names.iter().any(|n| n.starts_with("OldParent")),
            "{label_names:?}"
        );

        // Move a message into the grandchild folder, then delete the parent folder.
        let (status, inbox) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/inbox/messages?$top=1",
            None,
        )
        .await;
        assert_eq!(status, 200);
        let msg_id = inbox["value"][0]["id"].as_str().unwrap().to_owned();
        let (status, moved) = call(
            &router,
            Method::POST,
            &format!("/v1.0/me/messages/{msg_id}/move"),
            Some(json!({ "destinationId": grand_id })),
        )
        .await;
        assert_eq!(status, 201, "{moved}");
        let subject = moved["subject"].as_str().unwrap().to_owned();

        let mut delta_uri = "/v1.0/me/mailFolders/delta".to_owned();
        let delta_path = loop {
            let (status, page) = call(&router, Method::GET, &delta_uri, None).await;
            assert_eq!(status, 200, "{page}");
            if let Some(link) = page["@odata.deltaLink"].as_str() {
                break format!("/v1.0{}", link.split("/v1.0").nth(1).unwrap());
            }
            let next = page["@odata.nextLink"].as_str().unwrap();
            delta_uri = format!("/v1.0{}", next.split("/v1.0").nth(1).unwrap());
        };

        let (status, _) = call(
            &router,
            Method::DELETE,
            &format!("/v1.0/me/mailFolders/{parent_id}"),
            None,
        )
        .await;
        assert_eq!(status, 204);

        // Parent, child, and grandchild are all gone from Graph and Gmail.
        for id in [&parent_id, &child_id, &grand_id] {
            let (status, _) = call(
                &router,
                Method::GET,
                &format!("/v1.0/me/mailFolders/{id}"),
                None,
            )
            .await;
            assert_eq!(status, 404, "folder {id} should be deleted");
        }
        let (_, labels) = call(&router, Method::GET, "/gmail/v1/users/me/labels", None).await;
        let label_names: Vec<&str> = labels["labels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["name"].as_str())
            .collect();
        assert!(
            !label_names.iter().any(|n| n.starts_with("NewParent")),
            "{label_names:?}"
        );

        // Message that was inside the grandchild moved to Deleted Items.
        let (status, trashed) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/deleteditems/messages?$top=100",
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            trashed["value"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["subject"] == subject),
            "grandchild message should be in deleteditems"
        );

        // Folder delta reports parent, child, and grandchild as @removed.
        let (status, delta_after) = call(&router, Method::GET, &delta_path, None).await;
        assert_eq!(status, 200, "{delta_after}");
        let removed: Vec<&str> = delta_after["value"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry.get("@removed").is_some())
            .filter_map(|entry| entry["id"].as_str())
            .collect();
        for id in [&parent_id, &child_id, &grand_id] {
            assert!(
                removed.contains(&id.as_str()),
                "delta should report {id} removed: {removed:?}"
            );
        }
    }
}
