//! Typed ACP-only credential env channel (GOAL B prevention seam).
//!
//! The per-run `acp_credentials` injection authenticates the ACP agent
//! (`CLAUDE_CODE_OAUTH_TOKEN` for Claude; `OPENAI_API_KEY` +
//! `CHATGPT_ACCOUNT_ID` for Codex) **without** the host's `~/.claude` or
//! `~/.codex`. Those secrets must reach the ACP child process *only* — never the
//! shared sandbox `base_env` that API-backend shell tools and `docker exec`
//! descendants inherit.
//!
//! [`AcpEnv`] is a newtype deliberately distinct from the
//! `HashMap<String, String>` used for `base_env`. Routing credentials into
//! `base_env` therefore requires an explicit, greppable [`AcpEnv::into_inner`]
//! — a deliberate act, not a silent map merge. The single sanctioned sink is
//! [`super::acp::AgentAcpBackend::resolve_launch_env`], which merges these vars
//! into the dedicated ACP launch env after `ToolEnvProvider::resolve`.
//!
//! A future implementer adding the create_run → `ManagedRun` → backend wiring
//! must pass the parsed `acp_credentials` through this type. If they instead
//! reach for `base_env`, the type mismatch surfaces at the call site rather than
//! leaking tokens to every exec descendant.

use std::collections::HashMap;

/// ACP-only credential environment. Distinct from `base_env` by construction.
///
/// `Debug` is **hand-rolled to redact values**: this wraps live credential
/// material (`CLAUDE_CODE_OAUTH_TOKEN`, etc.), and the whole point of the type
/// is that the secret never reaches a log/inspect line. A derived `Debug` would
/// print every token verbatim, so it is replaced with a key-only rendering.
#[derive(Clone, Default)]
pub struct AcpEnv(HashMap<String, String>);

impl std::fmt::Debug for AcpEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Expose which credential keys are present (useful for debugging) but
        // never their values.
        f.debug_struct("AcpEnv")
            .field("keys", &self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl AcpEnv {
    /// Wrap a set of credential env vars destined for the ACP child only.
    #[must_use]
    pub fn new(vars: HashMap<String, String>) -> Self {
        Self(vars)
    }

    /// True when no credentials are carried.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Merge these credentials into a resolved ACP launch env. This is the only
    /// sanctioned consumer; the target must be the ACP-only launch env, never
    /// the shared `base_env`.
    pub(crate) fn apply_to(&self, target: &mut HashMap<String, String>) {
        for (key, value) in &self.0 {
            target.insert(key.clone(), value.clone());
        }
    }

    /// Explicit, greppable escape hatch back to a plain map. Do **not** use this
    /// to populate `base_env` — that defeats the whole point of the type.
    #[must_use]
    pub fn into_inner(self) -> HashMap<String, String> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_by_default() {
        assert!(AcpEnv::default().is_empty());
    }

    #[test]
    fn debug_redacts_credential_values() {
        let creds = AcpEnv::new(HashMap::from([(
            "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
            "sk-ant-oat01-secret".to_string(),
        )]));
        let rendered = format!("{creds:?}");
        // The key may appear (helpful for debugging) but never the token value.
        assert!(!rendered.contains("sk-ant-oat01-secret"));
        assert!(rendered.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn apply_to_merges_over_resolved_env() {
        let creds = AcpEnv::new(HashMap::from([(
            "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
            "sk-ant-oat01-secret".to_string(),
        )]));
        let mut launch_env = HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
        creds.apply_to(&mut launch_env);
        assert_eq!(
            launch_env
                .get("CLAUDE_CODE_OAUTH_TOKEN")
                .map(String::as_str),
            Some("sk-ant-oat01-secret")
        );
        assert_eq!(launch_env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }
}
