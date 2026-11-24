//! Signing in through the Microsoft identity platform's authorization code flow: PKCE, the
//! loopback redirect, ID tokens, rotating refresh tokens, and consent errors.

mod graph;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use graph::{Graph, assert_error};
use mock_servers::{CONSUMER_TENANT, TENANT};
use serde_json::{Value, json};

const CLIENT: &str = "00001111-aaaa-2222-bbbb-3333cccc4444";
const REDIRECT: &str = "http://localhost:53682/callback";
const VERIFIER: &str = "eXo-mock-verifier-that-is-long-enough-for-pkce-0123456789";
const SCOPES: &str = "openid profile email offline_access Mail.ReadWrite User.Read";

fn challenge() -> String {
    gmail::oauth::s256_challenge(VERIFIER)
}

/// Starts sign-in at `tenant`, returning the redirect's query.
async fn authorize(graph: &Graph, tenant: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let challenge = challenge();
    let mut params = vec![
        ("client_id", CLIENT),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT),
        ("response_mode", "query"),
        ("scope", SCOPES),
        ("state", "s-42"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    params.extend_from_slice(extra);
    let response = graph
        .http
        .get(graph.url(&format!("/{tenant}/oauth2/v2.0/authorize")))
        .query(&params)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 302);
    let location = response.headers()["location"].to_str().unwrap();
    assert!(location.starts_with(REDIRECT), "{location}");
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn param<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

async fn token(graph: &Graph, tenant: &str, form: &[(&str, &str)]) -> (u16, Value) {
    let response = graph
        .http
        .post(graph.url(&format!("/{tenant}/oauth2/v2.0/token")))
        .form(form)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

async fn redeem(graph: &Graph, tenant: &str, code: &str) -> (u16, Value) {
    token(
        graph,
        tenant,
        &[
            ("client_id", CLIENT),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
        ],
    )
    .await
}

fn claims(jwt: &str) -> Value {
    let payload = jwt.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

async fn me(graph: &Graph, token: &str) -> (u16, Value) {
    let response = graph
        .http
        .get(graph.url("/v1.0/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[tokio::test]
async fn a_work_account_signs_in_with_pkce() {
    let graph = Graph::start().await;
    let query = authorize(&graph, "common", &[("nonce", "n-1")]).await;
    assert_eq!(param(&query, "state"), Some("s-42"));
    let code = param(&query, "code").expect("a code").to_owned();

    let (status, tokens) = redeem(&graph, "common", &code).await;
    assert_eq!(status, 200, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3599);
    assert!(
        tokens["scope"]
            .as_str()
            .unwrap()
            .contains("https://graph.microsoft.com/Mail.ReadWrite")
    );
    let id = claims(tokens["id_token"].as_str().unwrap());
    assert_eq!(id["tid"], TENANT);
    assert_eq!(
        id["iss"],
        format!("https://login.microsoftonline.com/{TENANT}/v2.0")
    );
    assert_eq!(id["aud"], CLIENT);
    assert_eq!(id["ver"], "2.0");
    assert_eq!(id["nonce"], "n-1");
    assert_eq!(id["preferred_username"], "maya@brightmoor.co");
    let (status, profile) = me(&graph, tokens["access_token"].as_str().unwrap()).await;
    assert_eq!(status, 200, "{profile}");
    assert_eq!(
        profile["id"], id["oid"],
        "the ID token's oid is the user's id"
    );
    assert_eq!(profile["mail"], "maya@brightmoor.co");

    let (status, reused) = redeem(&graph, "common", &code).await;
    assert_eq!(
        (status, reused["error"].as_str()),
        (400, Some("invalid_grant"))
    );
    assert_eq!(reused["error_codes"], json!([54005]));
    assert!(
        reused["error_description"]
            .as_str()
            .unwrap()
            .starts_with("AADSTS54005: ")
    );
    assert!(
        reused["trace_id"].is_string()
            && reused["correlation_id"].is_string()
            && reused["timestamp"].is_string()
    );

    let fresh = authorize(&graph, "organizations", &[]).await;
    let (status, wrong) = token(
        &graph,
        "organizations",
        &[
            ("client_id", CLIENT),
            ("grant_type", "authorization_code"),
            ("code", param(&fresh, "code").unwrap()),
            ("redirect_uri", REDIRECT),
            ("code_verifier", "not-the-verifier"),
        ],
    )
    .await;
    assert_eq!(
        (status, wrong["error_codes"].clone()),
        (400, json!([501481]))
    );
}

#[tokio::test]
async fn refresh_tokens_rotate() {
    let graph = Graph::start().await;
    let query = authorize(&graph, "common", &[]).await;
    let (_, first) = redeem(&graph, "common", param(&query, "code").unwrap()).await;
    let old = first["refresh_token"].as_str().unwrap().to_owned();
    let refresh = |refresh_token: String| {
        let graph = &graph;
        async move {
            token(
                graph,
                "common",
                &[
                    ("client_id", CLIENT),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh_token),
                    ("scope", SCOPES),
                ],
            )
            .await
        }
    };
    let (status, second) = refresh(old.clone()).await;
    assert_eq!(status, 200, "{second}");
    let new = second["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(new, old, "every refresh returns a new refresh token");
    assert_ne!(second["access_token"], first["access_token"]);
    assert_eq!(
        me(&graph, second["access_token"].as_str().unwrap()).await.0,
        200
    );
    let (status, _) = refresh(old).await;
    assert_eq!(status, 200, "the old one isn't revoked by its use");
    let (status, third) = refresh(new).await;
    assert_eq!(status, 200);
    assert!(third["id_token"].is_string());

    graph
        .admin(
            "/__mock/faults",
            json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS700082" }),
        )
        .await;
    let (status, expired) = refresh(third["refresh_token"].as_str().unwrap().to_owned()).await;
    assert_eq!(
        (status, expired["error"].as_str()),
        (400, Some("invalid_grant"))
    );
    assert_eq!(expired["error_codes"], json!([700082]));

    let (status, unknown) = refresh("M.C0_nope".to_owned()).await;
    assert_eq!(
        (status, unknown["error"].as_str()),
        (400, Some("invalid_grant"))
    );

    // Access tokens expire with the mock's clock.
    let access = third["access_token"].as_str().unwrap();
    graph
        .admin("/__mock/clock", json!({ "now": "2026-11-17T16:30:00Z" }))
        .await;
    let response = graph
        .http
        .get(graph.url("/v1.0/me"))
        .bearer_auth(access)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "InvalidAuthenticationToken");
    assert_eq!(
        body["error"]["message"],
        "Lifetime validation failed, the token is expired."
    );
}

#[tokio::test]
async fn personal_accounts_sign_into_the_consumer_tenant() {
    let graph = Graph::start().await;
    for (tenant, hint) in [
        ("consumers", None),
        ("common", Some("maya.okafor@outlook.com")),
    ] {
        let extra: Vec<(&str, &str)> = hint
            .map(|hint| vec![("login_hint", hint)])
            .unwrap_or_default();
        let query = authorize(&graph, tenant, &extra).await;
        let (status, tokens) = redeem(&graph, tenant, param(&query, "code").unwrap()).await;
        assert_eq!(status, 200, "{tokens}");
        let id = claims(tokens["id_token"].as_str().unwrap());
        assert_eq!(id["tid"], CONSUMER_TENANT, "{tenant}");
        let access = tokens["access_token"].as_str().unwrap();
        assert!(
            access.starts_with("EwB"),
            "personal accounts' access tokens are opaque"
        );
        let (status, profile) = me(&graph, access).await;
        assert_eq!(status, 200);
        assert_eq!(profile["id"].as_str().unwrap().len(), 16, "{profile}");
        let users = graph
            .http
            .get(graph.url("/v1.0/users"))
            .bearer_auth(access)
            .send()
            .await
            .unwrap();
        assert_eq!(users.status(), 403, "personal accounts have no directory");
    }
}

#[tokio::test]
async fn consent_errors_come_back_on_the_redirect() {
    let graph = Graph::start().await;
    for (code, error) in [
        ("AADSTS65004", "access_denied"),
        ("AADSTS65001", "consent_required"),
        ("AADSTS90094", "access_denied"),
    ] {
        graph
            .admin(
                "/__mock/faults",
                json!({ "path": "/common/oauth2/v2.0/authorize", "status": 302, "reason": code }),
            )
            .await;
        let query = authorize(&graph, "common", &[]).await;
        assert_eq!(param(&query, "error"), Some(error), "{code}");
        assert!(
            param(&query, "error_description")
                .unwrap()
                .starts_with(&format!("{code}: "))
        );
        assert_eq!(param(&query, "state"), Some("s-42"));
        assert_eq!(param(&query, "code"), None);
    }
    let query = authorize(&graph, "common", &[]).await;
    assert!(param(&query, "code").is_some(), "faults fire once");

    let page = graph
        .http
        .get(graph.url("/common/oauth2/v2.0/authorize"))
        .query(&[
            ("client_id", CLIENT),
            ("response_type", "code"),
            ("redirect_uri", "https://example.com/cb"),
            ("scope", SCOPES),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        page.status(),
        400,
        "a redirect that isn't the app's isn't followed"
    );
    assert!(page.text().await.unwrap().contains("AADSTS50011"));
}

#[tokio::test]
async fn api_requests_need_a_token() {
    let graph = Graph::start().await;
    let none = graph
        .http
        .get(graph.url("/v1.0/me/messages"))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 401);
    let body: Value = none.json().await.unwrap();
    assert_eq!(body["error"]["message"], "Access token is empty.");
    let mut bogus = Graph::start().await;
    bogus.token = "eyJ0eXAi.nope.sig".to_owned();
    assert_error(
        &bogus.get("/v1.0/me").await,
        401,
        "InvalidAuthenticationToken",
    );
}
