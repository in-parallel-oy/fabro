use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;

use crate::server::SharedState;

#[derive(Debug, Deserialize)]
pub struct AuthorizeParams {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub client_id: String,
    pub client_secret: String,
    pub code: String,
}

pub async fn authorize(
    State(state): State<SharedState>,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    let mut state = state.write().await;
    let redirect_url = if state.allow_authorize {
        let code = format!("oauth-code-{}", uuid::Uuid::new_v4());
        let subject = state.oauth_subject();
        state.oauth_codes.insert(
            code.clone(),
            crate::state::OauthCode {
                client_id: params.client_id.clone(),
                subject,
            },
        );
        format!(
            "{}?code={}&state={}",
            params.redirect_uri, code, params.state
        )
    } else {
        format!(
            "{}?error=access_denied&state={}",
            params.redirect_uri, params.state
        )
    };

    Redirect::to(&redirect_url).into_response()
}

pub async fn access_token(
    State(state): State<SharedState>,
    Form(form): Form<TokenRequest>,
) -> Response {
    let mut state = state.write().await;
    if form.client_id != state.oauth_client_id || form.client_secret != state.oauth_client_secret {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "invalid_client" })),
        )
            .into_response();
    }

    let Some(code) = state.oauth_codes.remove(&form.code) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    };
    if code.client_id != form.client_id {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    }

    let token = format!("gho_{}", uuid::Uuid::new_v4().simple());
    state.oauth_tokens.insert(token.clone(), code.subject);

    (
        StatusCode::OK,
        axum::Json(json!({
            "access_token": token,
            "token_type": "bearer",
            "scope": "read:user user:email"
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use reqwest::redirect::Policy;

    use crate::server::TestServer;
    use crate::state::AppState;

    fn no_redirect_client() -> fabro_http::HttpClient {
        fabro_http::HttpClientBuilder::new()
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn authorize_exchange_and_user_token_round_trip() {
        let state = AppState::new();
        let server = TestServer::start(state).await;
        let client = no_redirect_client();

        let authorize = client
            .get(format!(
                "{}/login/oauth/authorize?client_id=github-client-id&redirect_uri=http://127.0.0.1/callback&state=test-state",
                server.url()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(authorize.status(), 303);
        let location = authorize
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        assert!(location.contains("code="));
        assert!(location.contains("state=test-state"));
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .unwrap()
            .to_string();

        let token = client
            .post(format!("{}/login/oauth/access_token", server.url()))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id=github-client-id&client_secret=github-client-secret&code={code}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(token.status(), 200);
        let body: serde_json::Value = token.json().await.unwrap();
        assert_eq!(body["token_type"], "bearer");
        assert!(body["access_token"].as_str().is_some());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn token_exchange_rejects_wrong_client_secret() {
        let state = AppState::new();
        let server = TestServer::start(state).await;
        let client = no_redirect_client();

        let authorize = client
            .get(format!(
                "{}/login/oauth/authorize?client_id=github-client-id&redirect_uri=http://127.0.0.1/callback&state=test-state",
                server.url()
            ))
            .send()
            .await
            .unwrap();
        let location = authorize
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .unwrap();

        let token = client
            .post(format!("{}/login/oauth/access_token", server.url()))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id=github-client-id&client_secret=wrong-secret&code={code}"
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(token.status(), 400);
        let body: serde_json::Value = token.json().await.unwrap();
        assert_eq!(body["error"], "invalid_client");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn token_exchange_rejects_replayed_code() {
        let state = AppState::new();
        let server = TestServer::start(state).await;
        let client = no_redirect_client();

        let authorize = client
            .get(format!(
                "{}/login/oauth/authorize?client_id=github-client-id&redirect_uri=http://127.0.0.1/callback&state=test-state",
                server.url()
            ))
            .send()
            .await
            .unwrap();
        let location = authorize
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .unwrap()
            .to_string();

        let exchange = |code: &str| {
            client
                .post(format!("{}/login/oauth/access_token", server.url()))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(format!(
                    "client_id=github-client-id&client_secret=github-client-secret&code={code}"
                ))
        };

        let first = exchange(&code).send().await.unwrap();
        assert_eq!(first.status(), 200);

        let second = exchange(&code).send().await.unwrap();
        assert_eq!(second.status(), 400);
        let body: serde_json::Value = second.json().await.unwrap();
        assert_eq!(body["error"], "invalid_grant");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn authorize_can_deny_access() {
        let mut state = AppState::new();
        state.allow_authorize = false;
        let server = TestServer::start(state).await;
        let client = no_redirect_client();

        let authorize = client
            .get(format!(
                "{}/login/oauth/authorize?client_id=github-client-id&redirect_uri=http://127.0.0.1/callback&state=test-state",
                server.url()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(authorize.status(), 303);
        let location = authorize
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(location.contains("error=access_denied"));
        assert!(location.contains("state=test-state"));

        server.shutdown().await;
    }
}
