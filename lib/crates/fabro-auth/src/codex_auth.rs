use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use crate::credential::OAuthCredential;
use crate::refresh::refresh_oauth_credential;

/// How close to its `exp` an `id_token` may be before we treat it as
/// needing a refresh. Mirrors the access token's 5-minute buffer in
/// `OAuthCredential::needs_refresh`.
const ID_TOKEN_REFRESH_BUFFER_MINUTES: i64 = 5;

/// Materialised codex `auth.json` plus an optional refreshed credential the
/// caller must persist back to the vault.
pub struct CodexAuthMaterial {
    /// Serialised `auth.json` body to write into `$CODEX_HOME`.
    pub auth_json: String,
    /// `Some` only when [`codex_auth_json`] refreshed the credential. The
    /// caller owns writing this back to the vault so the rotated refresh
    /// token is not lost (the file we write into the sandbox is discarded
    /// with the run).
    pub refreshed: Option<OAuthCredential>,
}

/// Build a codex `auth.json` (ChatGPT subscription auth) from a vaulted OAuth
/// credential, without burning the vault's single-use refresh token on every
/// run.
///
/// Unlike `claude-agent-acp`, which reads `CLAUDE_CODE_OAUTH_TOKEN` straight
/// from the environment, `codex-acp` only loads ChatGPT auth from
/// `$CODEX_HOME/auth.json`, which needs a parseable `id_token` JWT.
///
/// Codex itself refreshes lazily — only when the access-token JWT has expired
/// or `last_refresh` is older than its 8-day interval — and persists rotations
/// back to its own file. We mirror that:
///
/// - If the stored access token is still valid and we hold an `id_token` that
///   is itself still fresh, serialise the stored tokens verbatim and refresh
///   nothing. No rotation, so the vault's refresh token stays alive for the
///   next run.
/// - Otherwise refresh once and hand the refreshed credential back so the
///   caller can persist it to the vault (the single, write-back-aware rotation
///   path). Note the `id_token` is short-lived (≈1h) even when the access token
///   lives far longer, so a fresh access token alone is not enough — a lapsed
///   `id_token` is rejected by codex at `auth.json` load, so we refresh on its
///   expiry too rather than writing a dead one.
///
/// The written file deliberately carries **no usable refresh token**: with a
/// fresh access token codex won't try to refresh, and without a refresh token
/// it *can't* rotate and silently invalidate the vault copy from inside the
/// throwaway sandbox.
pub async fn codex_auth_json(credential: &OAuthCredential) -> anyhow::Result<CodexAuthMaterial> {
    if !credential.needs_refresh() && id_token_fresh(credential.tokens.id_token.as_deref()) {
        let id_token = credential
            .tokens
            .id_token
            .as_deref()
            .expect("id_token present + fresh checked above");
        return Ok(CodexAuthMaterial {
            auth_json: build_auth_json(credential, id_token),
            refreshed: None,
        });
    }

    let refreshed = refresh_oauth_credential(credential).await?;
    let id_token = refreshed
        .tokens
        .id_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("codex token refresh returned no id_token"))?;
    let auth_json = build_auth_json(&refreshed, id_token);
    Ok(CodexAuthMaterial {
        auth_json,
        refreshed: Some(refreshed),
    })
}

/// Serialise the ChatGPT `auth.json` body. `refresh_token` is intentionally
/// blank so codex cannot rotate the vault's token from the sandbox;
/// `last_refresh` is set to now so codex's time-based refresh trigger stays
/// dormant and it falls back solely to access-token expiry (which we guarantee
/// is not imminent).
fn build_auth_json(credential: &OAuthCredential, id_token: &str) -> String {
    let auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": serde_json::Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": credential.tokens.access_token,
            "refresh_token": "",
            "account_id": credential.account_id,
        },
        "last_refresh": Utc::now().to_rfc3339(),
    });
    auth.to_string()
}

/// True when an `id_token` is present and its `exp` is more than the
/// refresh buffer away. A missing, unparseable, or `exp`-less token is
/// treated as not fresh so the caller refreshes and mints a new one.
fn id_token_fresh(id_token: Option<&str>) -> bool {
    let Some(token) = id_token else {
        return false;
    };
    match jwt_exp(token) {
        Some(exp) => exp > Utc::now() + Duration::minutes(ID_TOKEN_REFRESH_BUFFER_MINUTES),
        None => false,
    }
}

/// Decode the `exp` claim of a JWT without verifying its signature (the
/// token came from the vault, not an untrusted edge).
fn jwt_exp(token: &str) -> Option<DateTime<Utc>> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;

    #[derive(serde::Deserialize)]
    struct Claims {
        exp: Option<i64>,
    }

    let claims: Claims = serde_json::from_slice(&payload).ok()?;
    DateTime::from_timestamp(claims.exp?, 0)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::Value;

    use super::*;
    use crate::credential::{OAuthConfig, OAuthTokens};

    fn credential(expires_at: chrono::DateTime<Utc>, id_token: Option<&str>) -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token: "stored-access".to_string(),
                refresh_token: Some("stored-refresh".to_string()),
                expires_at,
                id_token: id_token.map(str::to_string),
            },
            config:     OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: None,
                use_pkce:     true,
            },
            account_id: Some("acct_123".to_string()),
        }
    }

    fn id_token_with_exp(exp: DateTime<Utc>) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{}}}"#, exp.timestamp()));
        format!("{header}.{payload}.signature")
    }

    #[tokio::test]
    async fn reuses_fresh_tokens_without_refreshing() {
        let fresh_id = id_token_with_exp(Utc::now() + Duration::hours(1));
        let cred = credential(Utc::now() + Duration::hours(1), Some(&fresh_id));
        let material = codex_auth_json(&cred).await.unwrap();

        // Fresh access + fresh id_token => no network refresh, nothing to persist back.
        assert!(material.refreshed.is_none());

        let json: Value = serde_json::from_str(&material.auth_json).unwrap();
        assert_eq!(json["tokens"]["access_token"], "stored-access");
        assert_eq!(json["tokens"]["id_token"], fresh_id);
        assert_eq!(json["tokens"]["account_id"], "acct_123");
        // No usable refresh token reaches the sandbox => codex can't rotate it.
        assert_eq!(json["tokens"]["refresh_token"], "");
        assert_eq!(json["auth_mode"], "chatgpt");
        assert!(json["last_refresh"].is_string());
    }

    #[test]
    fn id_token_fresh_requires_a_present_unexpired_token() {
        assert!(id_token_fresh(Some(&id_token_with_exp(
            Utc::now() + Duration::hours(1)
        ))));
        // Expired id_token => not fresh, even though the access token may be.
        assert!(!id_token_fresh(Some(&id_token_with_exp(
            Utc::now() - Duration::minutes(1)
        ))));
        // Within the refresh buffer => not fresh.
        assert!(!id_token_fresh(Some(&id_token_with_exp(
            Utc::now() + Duration::minutes(1)
        ))));
        // Missing or unparseable => not fresh.
        assert!(!id_token_fresh(None));
        assert!(!id_token_fresh(Some("not-a-jwt")));
    }
}
