use chrono::Utc;
use serde_json::json;

use crate::credential::OAuthCredential;

/// Build the body of a codex `auth.json` (ChatGPT subscription auth) from a
/// vaulted OAuth credential.
///
/// Unlike `claude-agent-acp`, which reads `CLAUDE_CODE_OAUTH_TOKEN` straight
/// from the environment, `codex-acp` only loads ChatGPT auth from
/// `$CODEX_HOME/auth.json`. That file requires a parseable `id_token` JWT,
/// which the vaulted credential omits — the device-login flow keeps only the
/// account id extracted from it (see `strategies::codex_device`). We therefore
/// refresh the credential first: the refresh response carries a fresh
/// `id_token` (and access token), which doubles as a guarantee that the
/// materialised token isn't already stale at injection time.
pub async fn codex_auth_json(credential: &OAuthCredential) -> anyhow::Result<String> {
    let refresh_token = credential
        .tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("codex credential has no refresh token"))?;

    let refreshed = fabro_oauth::refresh_token(
        fabro_oauth::OAuthEndpoint {
            token_url: &credential.config.token_url,
            client_id: &credential.config.client_id,
        },
        refresh_token,
    )
    .await
    .map_err(anyhow::Error::msg)?;

    let id_token = refreshed
        .id_token
        .ok_or_else(|| anyhow::anyhow!("codex token refresh returned no id_token"))?;
    let access_token = refreshed.access_token;
    // Token endpoints may rotate the refresh token or omit it (keep ours then).
    let refresh_token = refreshed
        .refresh_token
        .unwrap_or_else(|| refresh_token.to_string());

    let auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": serde_json::Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": credential.account_id,
        },
        "last_refresh": Utc::now().to_rfc3339(),
    });

    Ok(serde_json::to_string(&auth)?)
}
