//! The Microsoft identity platform's authorization code flow at `/{tenant}/oauth2/v2.0/`:
//! consent (automatic), PKCE, the loopback redirect, refresh tokens that rotate on every use, and
//! ID tokens whose `tid` tells a work account from a personal one.
//!
//! Consent signs in the mailbox's owner at Brightmoor, or the address in `login_hint`. A
//! personal account (the `consumers` tenant, or a hint at outlook.com and the like) signs into
//! Microsoft's consumer tenant. Every account shares the one mailbox, as Google sign-ins do.
//!
//! Faults armed on these paths (`/__mock/faults` with `reason` an `AADSTS` code) fail the next
//! sign-in: on the authorize path, as an error on the redirect (consent declined or blocked); on
//! the token path, as a token error.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ids;
use crate::http::extract::Params;
use crate::world::{App, World};

/// Brightmoor's directory tenant.
pub const TENANT: &str = "5f2a7c1e-8d4b-4e9a-b3c6-2d81f0a9e7b4";
/// Microsoft's tenant for personal accounts.
pub const CONSUMERS: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

const LIFETIME_SECS: i64 = 3599;

/// Domains whose addresses are personal Microsoft accounts.
const CONSUMER_DOMAINS: &[&str] = &["outlook.com", "hotmail.com", "live.com", "msn.com"];

/// Who a token signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub email: String,
    pub name: String,
    /// The object ID, which Graph calls the user's `id`.
    pub id: String,
    pub tenant: String,
}

impl Account {
    pub fn personal(&self) -> bool {
        self.tenant == CONSUMERS
    }

    /// The account for `email`: the mailbox's owner, a colleague, or a personal account.
    pub fn of(world: &World, email: &str, personal: bool) -> Self {
        let owner = email.eq_ignore_ascii_case(&world.account.email);
        let domain = email
            .rsplit('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let personal = personal || CONSUMER_DOMAINS.contains(&domain.as_str());
        let name = if owner || personal {
            world.account.name()
        } else {
            world
                .people
                .directory
                .iter()
                .find(|person| {
                    person.email_addresses.iter().any(|address| {
                        address
                            .value
                            .as_deref()
                            .is_some_and(|v| v.eq_ignore_ascii_case(email))
                    })
                })
                .and_then(|person| person.names.first()?.display_name.clone())
                .unwrap_or_else(|| email.to_owned())
        };
        Self {
            id: if personal {
                // Personal accounts' IDs are 16 hex digits.
                ids::guid(email).replace('-', "")[..16].to_owned()
            } else {
                super::people::user_id(world, email)
            },
            email: email.to_owned(),
            name,
            tenant: if personal { CONSUMERS } else { TENANT }.to_owned(),
        }
    }

    /// The mailbox's owner at work.
    pub fn owner(world: &World) -> Self {
        Self::of(world, &world.account.email.clone(), false)
    }
}

/// Issued codes and tokens.
#[derive(Debug, Default)]
pub struct Sessions {
    codes: HashMap<String, Grant>,
    /// Codes already redeemed, which Microsoft refuses with their own error.
    redeemed: Vec<String>,
    /// Access token → (expiry in epoch ms, account).
    access: HashMap<String, (i64, Account)>,
    /// Refresh token → (client ID, scope, account). Using one mints another and leaves it
    /// working, as Microsoft's don't stop when used.
    refresh: HashMap<String, (String, String, Account)>,
    issued: u64,
}

#[derive(Debug, Clone)]
struct Grant {
    client_id: String,
    redirect_uri: String,
    scope: String,
    challenge: Option<(String, String)>,
    nonce: Option<String>,
    account: Account,
}

impl Sessions {
    fn next(&mut self) -> u64 {
        self.issued += 1;
        self.issued
    }
}

/// Who `token` signs in, or whether it's expired (`Err(true)`) or unknown (`Err(false)`). The
/// mock's dev token (`DEV_ACCESS_TOKEN`) signs in the owner, or with `:address` that address.
pub fn who(world: &World, token: &str) -> Result<Account, bool> {
    if let Some(rest) = token.strip_prefix(crate::oauth::DEV_ACCESS_TOKEN) {
        match rest.strip_prefix(':') {
            Some(email) if !email.is_empty() => return Ok(Account::of(world, email, false)),
            _ if rest.is_empty() => return Ok(Account::owner(world)),
            _ => {}
        }
    }
    match world.graph.sessions.access.get(token) {
        Some((expiry, account)) if *expiry > world.clock.now_ms() => Ok(account.clone()),
        Some(_) => Err(true),
        None => Err(false),
    }
}

/// Consent, approved at once: validates the request, then redirects back with a code (or with
/// the error an armed fault names).
pub async fn authorize(
    State(app): State<Arc<App>>,
    Path(tenant): Path<String>,
    method: Method,
    uri: Uri,
    params: Params,
) -> Response {
    let Some(tenant) = Tenant::parse(&tenant) else {
        return page(&format!("AADSTS90002: Tenant '{tenant}' not found."));
    };
    let (Some(client_id), Some(redirect_uri)) =
        (params.text("client_id"), params.text("redirect_uri"))
    else {
        return page(
            "AADSTS900144: The request body must contain the following parameter: 'client_id'.",
        );
    };
    let Ok(mut redirect) = url::Url::parse(redirect_uri) else {
        return page("AADSTS90102: 'redirect_uri' value must be a valid absolute URI.");
    };
    if !loopback(&redirect) {
        return page(&format!(
            "AADSTS50011: The redirect URI '{redirect_uri}' specified in the request does not match the redirect URIs configured for the application '{client_id}'."
        ));
    }
    let state = params.get("state").map(str::to_owned);
    let mut world = app.lock();
    let fault = crate::http::take_fault(&mut world, &method, &uri);
    let now = world.clock.now();
    let error = if let Some(fault) = fault {
        let code = fault
            .reason
            .clone()
            .unwrap_or_else(|| "AADSTS65004".to_owned());
        Some((
            redirect_error(&code),
            describe(&code, fault.message.as_deref(), now),
        ))
    } else if params.get("response_type") != Some("code") {
        Some((
            "unsupported_response_type",
            describe(
                "AADSTS70005",
                Some("response_type 'token' is not enabled for the application."),
                now,
            ),
        ))
    } else if params.text("scope").is_none() {
        Some((
            "invalid_request",
            describe(
                "AADSTS900144",
                Some("The request body must contain the following parameter: 'scope'."),
                now,
            ),
        ))
    } else {
        None
    };
    {
        let mut query = redirect.query_pairs_mut();
        match error {
            Some((error, description)) => {
                query
                    .append_pair("error", error)
                    .append_pair("error_description", &description);
                if error == "access_denied" {
                    query.append_pair("error_subcode", "cancel");
                }
            }
            None => {
                let email = params
                    .text("login_hint")
                    .map_or_else(|| tenant.default_email(&world), str::to_owned);
                let account = Account::of(&world, &email, tenant == Tenant::Consumers);
                let challenge = crate::oauth::pkce_challenge(&params);
                let sessions = &mut world.graph.sessions;
                let code = format!(
                    "M.C{}_BAY.2.U.{:08x}",
                    sessions.next(),
                    sessions.issued.wrapping_mul(0x9E37_79B9)
                );
                sessions.codes.insert(
                    code.clone(),
                    Grant {
                        client_id: client_id.to_owned(),
                        redirect_uri: redirect_uri.to_owned(),
                        scope: params.text("scope").unwrap_or_default().to_owned(),
                        challenge,
                        nonce: params.get("nonce").map(str::to_owned),
                        account,
                    },
                );
                query.append_pair("code", &code);
            }
        }
        if let Some(state) = &state {
            query.append_pair("state", state);
        }
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, redirect.to_string())],
    )
        .into_response()
}

/// The path's tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tenant {
    Common,
    Organizations,
    Consumers,
}

impl Tenant {
    fn parse(tenant: &str) -> Option<Self> {
        match tenant.to_ascii_lowercase().as_str() {
            "common" => Some(Self::Common),
            "organizations" | TENANT | "brightmoor.co" => Some(Self::Organizations),
            "consumers" | CONSUMERS => Some(Self::Consumers),
            _ => None,
        }
    }

    /// Who signs in without a `login_hint`.
    fn default_email(self, world: &World) -> String {
        match self {
            Self::Consumers => {
                let account = &world.account;
                format!("{}{}@outlook.com", account.given_name, account.family_name).to_lowercase()
            }
            _ => world.account.email.clone(),
        }
    }
}

/// A native app's redirect: `http://localhost` or `http://127.0.0.1` on any port, or a custom
/// scheme.
fn loopback(redirect: &url::Url) -> bool {
    match redirect.scheme() {
        "http" => matches!(
            redirect.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        ),
        "https" => false,
        _ => true,
    }
}

/// The `error` an `AADSTS` code comes back as on the redirect.
fn redirect_error(code: &str) -> &'static str {
    match code {
        "AADSTS65004" | "AADSTS90094" => "access_denied",
        "AADSTS65001" => "consent_required",
        "AADSTS50058" | "AADSTS50076" => "interaction_required",
        _ => "invalid_request",
    }
}

/// An `AADSTS` code's description, with the trace lines Microsoft appends.
fn describe(code: &str, message: Option<&str>, now: jiff::Timestamp) -> String {
    let message = message.unwrap_or(match code {
        "AADSTS65001" => "The user or administrator has not consented to use the application. Send an interactive authorization request for this user and resource.",
        "AADSTS65004" => "User declined to consent to access the app.",
        "AADSTS90094" => "Administrator consent is required.",
        "AADSTS70008" => "The provided authorization code or refresh token has expired due to inactivity. Send a new interactive authorization request for this user and resource.",
        "AADSTS700082" => "The refresh token has expired due to inactivity.",
        "AADSTS54005" => "OAuth2 Authorization code was already redeemed, please retry with a new valid code or use an existing refresh token.",
        "AADSTS501481" => "The Code_Verifier does not match the code_challenge supplied in the authorization request.",
        "AADSTS70000" => "The provided value for the 'code' parameter is not valid.",
        "AADSTS9002313" => "Invalid request. Request is malformed or invalid.",
        _ => "The request is not valid.",
    });
    let trace = ids::guid(&format!("trace {code} {now}"));
    let correlation = ids::guid(&format!("correlation {code} {now}"));
    format!(
        "{code}: {message}\r\nTrace ID: {trace}\r\nCorrelation ID: {correlation}\r\nTimestamp: {}",
        now.strftime("%Y-%m-%d %H:%M:%SZ")
    )
}

/// Microsoft's sign-in error page, for requests it can't redirect back.
fn page(detail: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!DOCTYPE html><title>Sign in to your account</title><h1>Sorry, but we’re having trouble signing you in.</h1><p>{detail}</p>"
        )),
    )
        .into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct TokenRequest {
    grant_type: Option<String>,
    client_id: Option<String>,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

/// Redeems a code, or a refresh token for a new pair.
pub async fn token(
    State(app): State<Arc<App>>,
    Path(_tenant): Path<String>,
    method: Method,
    uri: Uri,
    Form(request): Form<TokenRequest>,
) -> Response {
    let mut world = app.lock();
    let now = world.clock.now();
    if let Some(fault) = crate::http::take_fault(&mut world, &method, &uri) {
        let code = fault
            .reason
            .clone()
            .unwrap_or_else(|| "AADSTS70008".to_owned());
        let status = StatusCode::from_u16(fault.status).unwrap_or(StatusCode::BAD_REQUEST);
        return token_error(
            status,
            "invalid_grant",
            &code,
            fault.message.as_deref(),
            &world,
        );
    }
    let Some(client_id) = request.client_id.clone() else {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "AADSTS900144",
            Some("The request body must contain the following parameter: 'client_id'."),
            &world,
        );
    };
    let (scope, account, nonce) = match request.grant_type.as_deref() {
        Some("authorization_code") => {
            let code = request.code.clone().unwrap_or_default();
            let sessions = &mut world.graph.sessions;
            let Some(grant) = sessions.codes.remove(&code) else {
                let (error, aadsts) = if sessions.redeemed.contains(&code) {
                    ("invalid_grant", "AADSTS54005")
                } else {
                    ("invalid_grant", "AADSTS70000")
                };
                return token_error(StatusCode::BAD_REQUEST, error, aadsts, None, &world);
            };
            sessions.redeemed.push(code);
            if grant.client_id != client_id {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "AADSTS70000",
                    None,
                    &world,
                );
            }
            if request.redirect_uri.as_deref() != Some(grant.redirect_uri.as_str()) {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "AADSTS50011",
                    Some(
                        "The redirect URI specified in the request does not match the one the authorization code was issued for.",
                    ),
                    &world,
                );
            }
            if let Some((method, challenge)) = &grant.challenge {
                let verifier = request.code_verifier.clone().unwrap_or_default();
                if !crate::oauth::verify_pkce(method, challenge, &verifier) {
                    return token_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        "AADSTS501481",
                        None,
                        &world,
                    );
                }
            }
            (grant.scope, grant.account, grant.nonce)
        }
        Some("refresh_token") => {
            let token = request.refresh_token.clone().unwrap_or_default();
            let Some((owner, scope, account)) = world.graph.sessions.refresh.get(&token).cloned()
            else {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "AADSTS9002313",
                    None,
                    &world,
                );
            };
            if owner != client_id {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "AADSTS70000",
                    Some("The refresh token was issued to a different client."),
                    &world,
                );
            }
            (request.scope.clone().unwrap_or(scope), account, None)
        }
        other => {
            let message = format!(
                "The provided value '{}' for the input parameter 'grant_type' is not valid.",
                other.unwrap_or_default()
            );
            return token_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "AADSTS70003",
                Some(&message),
                &world,
            );
        }
    };
    let scopes: Vec<&str> = scope.split_whitespace().collect();
    let granted = granted_scopes(&scopes);
    let sessions = &mut world.graph.sessions;
    let access = if account.personal() {
        format!("EwBYBMl6BAAUmock{:08x}", sessions.next())
    } else {
        crate::oauth::encode_jwt(
            &json!({ "aud": "https://graph.microsoft.com", "tid": account.tenant, "oid": account.id, "upn": account.email, "scp": granted, "n": sessions.next() }),
        )
    };
    let expiry = now.as_millisecond() + LIFETIME_SECS * 1000;
    sessions
        .access
        .insert(access.clone(), (expiry, account.clone()));
    let mut body = json!({
        "token_type": "Bearer",
        "scope": granted,
        "expires_in": LIFETIME_SECS,
        "ext_expires_in": LIFETIME_SECS,
        "access_token": access,
    });
    if scopes.contains(&"offline_access") {
        let refresh = format!(
            "M.C{}_SN1.0.U.-Mock!{:08x}",
            sessions.next(),
            sessions.issued.wrapping_mul(0x85EB_CA6B)
        );
        sessions.refresh.insert(
            refresh.clone(),
            (client_id.clone(), scope.clone(), account.clone()),
        );
        body["refresh_token"] = refresh.into();
    }
    if scopes.contains(&"openid") {
        body["id_token"] = id_token(&account, &client_id, &scopes, nonce.as_deref(), now).into();
    }
    json_response(StatusCode::OK, &body)
}

/// The scopes a token carries: Graph's with their resource, then OpenID Connect's.
fn granted_scopes(scopes: &[&str]) -> String {
    let mut graph: Vec<String> = Vec::new();
    let mut oidc: Vec<&str> = Vec::new();
    for scope in scopes {
        match *scope {
            "openid" | "profile" | "email" => oidc.push(scope),
            "offline_access" => {}
            scope if scope.starts_with("https://") => graph.push(scope.to_owned()),
            scope => graph.push(format!("https://graph.microsoft.com/{scope}")),
        }
    }
    graph.extend(oidc.into_iter().map(str::to_owned));
    graph.join(" ")
}

/// An ID token with the v2.0 claims. The signature is fake.
fn id_token(
    account: &Account,
    client_id: &str,
    scopes: &[&str],
    nonce: Option<&str>,
    now: jiff::Timestamp,
) -> String {
    let at = now.as_second();
    let mut claims = json!({
        "aud": client_id,
        "iss": format!("https://login.microsoftonline.com/{}/v2.0", account.tenant),
        "iat": at,
        "nbf": at,
        "exp": at + 3600,
        "sub": ids::guid(&format!("{client_id} {}", account.id)).replace('-', ""),
        "tid": account.tenant,
        "uti": ids::guid(&format!("uti {at} {}", account.id)).replace('-', "")[..22].to_owned(),
        "ver": "2.0",
    });
    if scopes.contains(&"profile") {
        claims["name"] = account.name.clone().into();
        claims["preferred_username"] = account.email.clone().into();
        claims["oid"] = if account.personal() {
            ids::guid(&format!("msa {}", account.id)).into()
        } else {
            account.id.clone().into()
        };
    }
    if scopes.contains(&"email") {
        claims["email"] = account.email.clone().into();
    }
    if let Some(nonce) = nonce {
        claims["nonce"] = nonce.into();
    }
    crate::oauth::encode_jwt(&claims)
}

fn token_error(
    status: StatusCode,
    error: &str,
    code: &str,
    message: Option<&str>,
    world: &World,
) -> Response {
    let now = world.clock.now();
    let number: u64 = code
        .trim_start_matches("AADSTS")
        .parse()
        .unwrap_or_default();
    let description = describe(code, message, now);
    let trace = description
        .split("Trace ID: ")
        .nth(1)
        .and_then(|rest| rest.split("\r\n").next())
        .unwrap_or_default()
        .to_owned();
    let origin = super::origin(world);
    json_response(
        status,
        &json!({
            "error": error,
            "error_description": description,
            "error_codes": [number],
            "timestamp": now.strftime("%Y-%m-%d %H:%M:%SZ").to_string(),
            "trace_id": trace,
            "correlation_id": ids::guid(&format!("correlation {code} {now}")),
            "error_uri": format!("{origin}error?code={number}"),
        }),
    )
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        serde_json::to_string(body).expect("JSON values serialize"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_scopes_carry_their_resource() {
        assert_eq!(
            granted_scopes(&[
                "openid",
                "profile",
                "offline_access",
                "Mail.ReadWrite",
                "https://graph.microsoft.com/User.Read"
            ]),
            "https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/User.Read openid profile"
        );
    }

    #[test]
    fn only_native_redirects() {
        for (uri, ok) in [
            ("http://localhost:53682/callback", true),
            ("http://127.0.0.1:1/", true),
            ("msal00001111://auth", true),
            ("https://example.com/cb", false),
            ("http://example.com/cb", false),
        ] {
            assert_eq!(loopback(&url::Url::parse(uri).unwrap()), ok, "{uri}");
        }
    }
}
