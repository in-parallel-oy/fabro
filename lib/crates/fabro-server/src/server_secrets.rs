use std::collections::HashMap;
use std::path::Path;

use fabro_auth::ResolveError;
use fabro_config::envfile;
use fabro_llm::client::{Client, ProviderRegistrationIssue};
use fabro_model::ProviderId;

#[expect(
    clippy::disallowed_methods,
    reason = "ServerSecrets snapshots process env once at startup by design."
)]
pub fn process_env_snapshot() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct ServerSecrets {
    env_entries:  HashMap<String, String>,
    file_entries: HashMap<String, String>,
}

impl ServerSecrets {
    pub(crate) fn load(
        path: impl AsRef<Path>,
        env_entries: HashMap<String, String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            env_entries,
            file_entries: envfile::read_env_file(path.as_ref())?,
        })
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.env_entries
            .get(name)
            .cloned()
            .or_else(|| self.file_entries.get(name).cloned())
    }

    /// Expose file-loaded entries (from `~/.fabro/storage/server.env`) to
    /// the process env, so callers of `std::env::var` — notably
    /// `InterpString::resolve` in `runtime_docker_config` for sandbox
    /// env_vars like `OBAN_PRO_LICENSE_KEY = "{{ env.OBAN_PRO_LICENSE_KEY }}"`
    /// — can find them. Process-env entries (set by operator launch env)
    /// already take precedence and are left untouched.
    ///
    /// Call once at server startup before any worker spawns, so the
    /// `set_var` calls happen single-threaded.
    #[expect(
        clippy::disallowed_methods,
        reason = "Bridging server.env to process env is the documented startup behavior."
    )]
    #[allow(
        unsafe_code,
        reason = "set_var requires unsafe; called single-threaded at startup"
    )]
    pub(crate) fn expose_file_entries_to_process_env(&self) {
        for (key, value) in &self.file_entries {
            if std::env::var(key).is_err() {
                // SAFETY: documented to be called once at startup, before
                // any worker threads spawn. set_var's thread-safety
                // contract is satisfied in that startup window.
                unsafe {
                    std::env::set_var(key, value);
                }
            }
        }
    }
}

impl std::fmt::Debug for ServerSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSecrets")
            .field("env_entries", &self.env_entries.keys().collect::<Vec<_>>())
            .field(
                "file_entries",
                &self.file_entries.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

pub(crate) struct LlmClientResult {
    pub client:              Client,
    pub auth_issues:         Vec<(ProviderId, ResolveError)>,
    pub registration_issues: Vec<ProviderRegistrationIssue>,
}

impl LlmClientResult {
    pub(crate) fn provider_ids(&self) -> Vec<ProviderId> {
        self.client
            .provider_names()
            .into_iter()
            .map(ProviderId::new)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::envfile;

    use super::ServerSecrets;

    #[test]
    fn server_secrets_snapshot_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("server.env");
        envfile::write_env_file(
            &env_path,
            &HashMap::from([
                ("SESSION_SECRET".to_string(), "file-value".to_string()),
                (
                    "GITHUB_APP_CLIENT_SECRET".to_string(),
                    "file-client".to_string(),
                ),
            ]),
        )
        .unwrap();

        let secrets = ServerSecrets::load(
            env_path,
            HashMap::from([("SESSION_SECRET".to_string(), "env-value".to_string())]),
        )
        .unwrap();

        assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("env-value"));
        assert_eq!(
            secrets.get("GITHUB_APP_CLIENT_SECRET").as_deref(),
            Some("file-client")
        );
    }
}
