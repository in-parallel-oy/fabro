//! Per-run ACP credential injection: transport parsing + engine conversion.
//!
//! GOAL B core seam. A create-run request may carry a top-level sibling key
//! `acp_credentials` alongside the run manifest. It authenticates the ACP agent
//! (Claude or Codex) from material supplied *at run-create time* — no host
//! `~/.claude` / `~/.codex` is ever consulted.
//!
//! The secret is split off at the transport edge ([`split_acp_credentials`])
//! **before** the manifest is deserialized or persisted, deserialized into the
//! hand-written [`InjectedAcpCredentials`] (deliberately *not* a field of the
//! codegen `RunManifest`), and converted into the engine-specific
//! [`AcpCredentials`] that the workflow threads to the ACP backend. The
//! conversion never re-serializes the secret into the manifest bytes, the run
//! record, or any serde error rendered from the request.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::acp_env::AcpEnv;

/// Which ACP agent the injected credentials authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpEngine {
    /// `claude-agent-acp` — authenticates via `CLAUDE_CODE_OAUTH_TOKEN`.
    Claude,
    /// `codex-acp` — authenticates via `$CODEX_HOME/auth.json`.
    Codex,
}

/// Hand-written transport struct for the `acp_credentials` sibling key.
///
/// Intentionally separate from the codegen `RunManifest`: it is removed from
/// the request body before the manifest is parsed, so the secret never enters
/// `submitted_manifest_bytes`, the persisted run, or a serde error rendered
/// from the manifest.
/// `Debug` is hand-rolled to redact `env` values: this struct carries the live
/// credential material, and the surrounding seam goes to lengths to keep secrets
/// out of logs/errors. A derived `Debug` would print every token verbatim, so it
/// renders only the engine + the credential key names.
#[derive(Clone, Serialize, Deserialize)]
pub struct InjectedAcpCredentials {
    /// The ACP agent the `env` material authenticates.
    pub engine: AcpEngine,
    /// Credential env vars. For Claude this carries `CLAUDE_CODE_OAUTH_TOKEN`
    /// (plus any extra keys); for Codex it carries `OPENAI_API_KEY` (the OAuth
    /// access token), `CHATGPT_ACCOUNT_ID`, and `CODEX_ID_TOKEN` (a parseable
    /// `id_token` JWT — **required**, because `codex-acp` rejects an `auth.json`
    /// whose `id_token` is null or unparseable; see [`codex_auth_json_from_env`]).
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl InjectedAcpCredentials {
    /// Validate the credential block at the transport edge, before persistence.
    ///
    /// Claude needs nothing beyond what the env carries. Codex **requires** a
    /// parseable `CODEX_ID_TOKEN`: `codex-acp` only loads ChatGPT auth from
    /// `$CODEX_HOME/auth.json` and rejects a null/unparseable `id_token` at load,
    /// so a Codex block missing it would persist as a run that can never
    /// authenticate. Fail-closed here so the caller returns a 400.
    ///
    /// # Errors
    /// Returns [`MalformedAcpCredentials`] for a Codex block whose env lacks a
    /// JWT-shaped `CODEX_ID_TOKEN`.
    pub fn validate(&self) -> Result<(), MalformedAcpCredentials> {
        match self.engine {
            AcpEngine::Claude => Ok(()),
            AcpEngine::Codex => codex_id_token(&self.env).map(|_| ()),
        }
    }
}

impl std::fmt::Debug for InjectedAcpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectedAcpCredentials")
            .field("engine", &self.engine)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Engine-specific, workflow-side form of the injected credentials. Built from
/// [`InjectedAcpCredentials`] once, then threaded to the ACP backend. `Clone`
/// because the backend factory closure may be invoked per stage.
///
/// `Debug` is hand-rolled to redact the payload: both `ClaudeEnv` (token map)
/// and `CodexAuthJson` (a materialized `auth.json` containing access + id
/// tokens) carry live secrets, so the rendering names only the active variant.
#[derive(Clone, Default)]
pub enum AcpCredentials {
    /// No per-run credentials were injected.
    #[default]
    None,
    /// Claude: credential env vars merged into the ACP launch env **only**
    /// (never the shared `base_env`).
    ClaudeEnv(AcpEnv),
    /// Codex: a materialized `auth.json` body to write into `$CODEX_HOME`
    /// inside the sandbox for the duration of the ACP turn.
    CodexAuthJson(String),
}

impl std::fmt::Debug for AcpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match self {
            Self::None => "None",
            Self::ClaudeEnv(_) => "ClaudeEnv(<redacted>)",
            Self::CodexAuthJson(_) => "CodexAuthJson(<redacted>)",
        };
        write!(f, "AcpCredentials::{variant}")
    }
}

impl TryFrom<InjectedAcpCredentials> for AcpCredentials {
    type Error = MalformedAcpCredentials;

    fn try_from(injected: InjectedAcpCredentials) -> Result<Self, Self::Error> {
        Ok(match injected.engine {
            AcpEngine::Claude => Self::ClaudeEnv(AcpEnv::new(injected.env)),
            AcpEngine::Codex => Self::CodexAuthJson(codex_auth_json_from_env(&injected.env)?),
        })
    }
}

/// Extract the required, JWT-shaped `CODEX_ID_TOKEN` from injected env.
///
/// `codex-acp` rejects an `auth.json` whose `id_token` is null or unparseable,
/// so the injector **must** supply one. We accept any token with the structural
/// shape of a JWT (three non-empty dot-separated segments) without verifying its
/// signature or expiry — freshness is the injector's responsibility (it refreshes
/// the `id_token` on its own expiry before sending it).
///
/// # Errors
/// Returns [`MalformedAcpCredentials`] when `CODEX_ID_TOKEN` is missing or not
/// JWT-shaped.
fn codex_id_token(env: &HashMap<String, String>) -> Result<&str, MalformedAcpCredentials> {
    let token = env
        .get("CODEX_ID_TOKEN")
        .map(String::as_str)
        .unwrap_or_default();
    if id_token_is_jwt_shaped(token) {
        Ok(token)
    } else {
        Err(MalformedAcpCredentials)
    }
}

/// True when `token` has the structural shape of a JWT: three non-empty
/// `.`-separated segments. A signature/expiry check is deliberately out of scope
/// — this only rejects the null/empty/garbage cases codex would refuse at load.
fn id_token_is_jwt_shaped(token: &str) -> bool {
    let mut parts = token.split('.');
    let shaped = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c)) if !a.is_empty() && !b.is_empty() && !c.is_empty()
    );
    shaped && parts.next().is_none()
}

/// Build a codex `auth.json` (ChatGPT subscription auth) from injected env.
///
/// Mirrors the vault path's file shape but sources the credential material from
/// the per-run injection: `OPENAI_API_KEY` is the OAuth access token,
/// `CHATGPT_ACCOUNT_ID` the account id, and `CODEX_ID_TOKEN` the (required)
/// `id_token` JWT. The `refresh_token` is **blank** so codex cannot rotate (and
/// strand) the upstream credential from inside the throwaway sandbox, and there
/// is **no vault write-back**. `last_refresh` is stamped now so codex's
/// time-based refresh trigger stays dormant.
///
/// # Errors
/// Returns [`MalformedAcpCredentials`] when `CODEX_ID_TOKEN` is absent or not
/// JWT-shaped — fail-closed, mirroring the vault path's hard requirement, rather
/// than materializing a null `id_token` codex would reject.
pub fn codex_auth_json_from_env(
    env: &HashMap<String, String>,
) -> Result<String, MalformedAcpCredentials> {
    let id_token = codex_id_token(env)?;
    let access_token = env
        .get("OPENAI_API_KEY")
        .map(String::as_str)
        .unwrap_or_default();
    let account_id = env.get("CHATGPT_ACCOUNT_ID").map(String::as_str);
    let auth = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": serde_json::Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": "",
            "account_id": account_id,
        },
        "last_refresh": chrono::Utc::now().to_rfc3339(),
    });
    Ok(auth.to_string())
}

/// Failure splitting the `acp_credentials` block off a create-run body.
///
/// Carries no detail by design: the credential material must never reach a log
/// line, an HTTP body, or an `inspect`-ed error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedAcpCredentials;

impl std::fmt::Display for MalformedAcpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid acp_credentials")
    }
}

impl std::error::Error for MalformedAcpCredentials {}

/// Split a top-level `acp_credentials` sibling key out of a create-run request
/// body **at the transport edge**.
///
/// Returns the remainder bytes (the body with `acp_credentials` removed,
/// re-serialized) plus the parsed credentials when present. The remainder — not
/// the raw body — is what the caller persists as `submitted_manifest_bytes`, so
/// the secret never lands in the manifest blob.
///
/// Fail-closed: a present-but-malformed `acp_credentials` returns
/// [`MalformedAcpCredentials`] so the caller can reject (400) rather than
/// persist a partial secret or silently drop auth. A body that is not a JSON
/// object is returned unchanged with no credentials (manifest-shape validation
/// is the caller's job).
///
/// # Errors
/// Returns [`MalformedAcpCredentials`] when the `acp_credentials` value is
/// present but cannot be parsed, fails [`InjectedAcpCredentials::validate`]
/// (e.g. a Codex block missing its required `CODEX_ID_TOKEN`), or the remainder
/// cannot be re-serialized.
pub fn split_acp_credentials(
    body: &[u8],
) -> Result<(Vec<u8>, Option<InjectedAcpCredentials>), MalformedAcpCredentials> {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_slice::<serde_json::Value>(body)
    else {
        return Ok((body.to_vec(), None));
    };
    let Some(raw) = map.remove("acp_credentials") else {
        return Ok((body.to_vec(), None));
    };
    // Discard the serde error: it can echo credential bytes back to the caller.
    let injected: InjectedAcpCredentials =
        serde_json::from_value(raw).map_err(|_| MalformedAcpCredentials)?;
    // Reject at the edge (400) rather than persisting a run that can never
    // authenticate — e.g. a Codex block missing its required id_token.
    injected.validate()?;
    let remainder = serde_json::to_vec(&serde_json::Value::Object(map))
        .map_err(|_| MalformedAcpCredentials)?;
    Ok((remainder, Some(injected)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_TOKEN: &str = "sk-ant-oat01-SECRET-do-not-leak";
    const CODEX_ACCESS: &str = "codex-access-SECRET";
    // Structurally JWT-shaped (three non-empty dot-separated segments). The
    // value is opaque to the seam — only its shape is checked.
    const CODEX_ID_TOKEN: &str = "header.payload.signature";

    fn codex_env() -> HashMap<String, String> {
        HashMap::from([
            ("OPENAI_API_KEY".to_string(), CODEX_ACCESS.to_string()),
            ("CHATGPT_ACCOUNT_ID".to_string(), "acct_xyz".to_string()),
            ("CODEX_ID_TOKEN".to_string(), CODEX_ID_TOKEN.to_string()),
        ])
    }

    fn claude_body() -> Vec<u8> {
        serde_json::json!({
            "target": {"path": "."},
            "acp_credentials": {
                "engine": "claude",
                "env": {"CLAUDE_CODE_OAUTH_TOKEN": CLAUDE_TOKEN},
            },
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn split_removes_credentials_and_returns_clean_remainder() {
        let (remainder, injected) = split_acp_credentials(&claude_body()).unwrap();
        let injected = injected.expect("credentials present");
        assert_eq!(injected.engine, AcpEngine::Claude);
        assert_eq!(
            injected.env.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
            Some(CLAUDE_TOKEN)
        );

        // The remainder must not carry the secret or the sibling key.
        let remainder_str = String::from_utf8(remainder.clone()).unwrap();
        assert!(!remainder_str.contains(CLAUDE_TOKEN));
        assert!(!remainder_str.contains("acp_credentials"));
        let value: serde_json::Value = serde_json::from_slice(&remainder).unwrap();
        assert!(value.get("acp_credentials").is_none());
        assert_eq!(value["target"]["path"], ".");
    }

    #[test]
    fn split_passes_through_when_no_credentials() {
        let body = br#"{"target":{"path":"."}}"#;
        let (remainder, injected) = split_acp_credentials(body).unwrap();
        assert!(injected.is_none());
        assert_eq!(remainder, body.to_vec());
    }

    #[test]
    fn split_passes_through_non_object_body() {
        let body = b"[1,2,3]";
        let (remainder, injected) = split_acp_credentials(body).unwrap();
        assert!(injected.is_none());
        assert_eq!(remainder, body.to_vec());
    }

    #[test]
    fn split_fails_closed_on_malformed_credentials() {
        let body = br#"{"acp_credentials":{"engine":"bogus","env":{}}}"#;
        let err = split_acp_credentials(body).unwrap_err();
        // The error message must not echo the request content.
        assert_eq!(err.to_string(), "invalid acp_credentials");

        let missing_engine = br#"{"acp_credentials":{"env":{}}}"#;
        assert!(split_acp_credentials(missing_engine).is_err());
    }

    #[test]
    fn codex_conversion_builds_auth_json_with_blank_refresh() {
        let injected = InjectedAcpCredentials {
            engine: AcpEngine::Codex,
            env:    codex_env(),
        };
        let AcpCredentials::CodexAuthJson(json) = AcpCredentials::try_from(injected).unwrap() else {
            panic!("codex engine should convert to auth.json");
        };
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["auth_mode"], "chatgpt");
        assert_eq!(value["tokens"]["access_token"], CODEX_ACCESS);
        assert_eq!(value["tokens"]["account_id"], "acct_xyz");
        assert_eq!(value["tokens"]["refresh_token"], "");
        // The materialized auth.json must carry the injected id_token, never a
        // null codex would reject at load.
        assert_eq!(value["tokens"]["id_token"], CODEX_ID_TOKEN);
    }

    #[test]
    fn codex_conversion_fails_closed_without_id_token() {
        // Access token + account id but no CODEX_ID_TOKEN: codex would reject the
        // resulting auth.json, so the conversion must fail rather than write null.
        let injected = InjectedAcpCredentials {
            engine: AcpEngine::Codex,
            env:    HashMap::from([
                ("OPENAI_API_KEY".to_string(), CODEX_ACCESS.to_string()),
                ("CHATGPT_ACCOUNT_ID".to_string(), "acct_xyz".to_string()),
            ]),
        };
        assert!(injected.validate().is_err());
        assert!(AcpCredentials::try_from(injected).is_err());
    }

    #[test]
    fn codex_conversion_rejects_non_jwt_id_token() {
        let injected = InjectedAcpCredentials {
            engine: AcpEngine::Codex,
            env:    HashMap::from([(
                "CODEX_ID_TOKEN".to_string(),
                "not-a-jwt".to_string(),
            )]),
        };
        assert!(AcpCredentials::try_from(injected).is_err());
    }

    #[test]
    fn split_fails_closed_on_codex_without_id_token() {
        let body = serde_json::json!({
            "target": {"path": "."},
            "acp_credentials": {
                "engine": "codex",
                "env": {"OPENAI_API_KEY": CODEX_ACCESS, "CHATGPT_ACCOUNT_ID": "acct_xyz"},
            },
        })
        .to_string()
        .into_bytes();
        assert!(split_acp_credentials(&body).is_err());
    }

    #[test]
    fn debug_redacts_injected_and_converted_secrets() {
        let injected = InjectedAcpCredentials {
            engine: AcpEngine::Codex,
            env:    codex_env(),
        };
        // InjectedAcpCredentials Debug shows engine + key names, never values.
        let injected_dbg = format!("{injected:?}");
        assert!(!injected_dbg.contains(CODEX_ACCESS));
        assert!(!injected_dbg.contains(CODEX_ID_TOKEN));
        assert!(injected_dbg.contains("CODEX_ID_TOKEN")); // key name is fine

        // The converted Codex auth.json (access + id tokens inside) must not
        // surface through Debug either.
        let creds = AcpCredentials::try_from(injected).unwrap();
        let creds_dbg = format!("{creds:?}");
        assert!(!creds_dbg.contains(CODEX_ACCESS));
        assert!(!creds_dbg.contains(CODEX_ID_TOKEN));
        assert!(creds_dbg.contains("redacted"));
    }

    #[test]
    fn claude_conversion_carries_env() {
        let injected = InjectedAcpCredentials {
            engine: AcpEngine::Claude,
            env:    HashMap::from([(
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                CLAUDE_TOKEN.to_string(),
            )]),
        };
        let AcpCredentials::ClaudeEnv(env) = AcpCredentials::try_from(injected).unwrap() else {
            panic!("claude engine should convert to env channel");
        };
        let mut target = HashMap::new();
        env.apply_to(&mut target);
        assert_eq!(
            target.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
            Some(CLAUDE_TOKEN)
        );
    }
}
