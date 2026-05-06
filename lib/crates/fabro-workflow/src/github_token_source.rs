use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use fabro_github::{GitHubAppCredentials, InstallationToken};
use tokio::sync::Mutex;
use tracing::warn;

const REFRESH_THRESHOLD: Duration = Duration::from_mins(15);

#[async_trait::async_trait]
pub trait IatMinter: Send + Sync {
    async fn mint(&self) -> anyhow::Result<InstallationToken>;
}

pub struct AppIatMinter {
    creds:       GitHubAppCredentials,
    http:        fabro_http::HttpClient,
    owner:       String,
    repo:        String,
    api_base:    String,
    install_url: Option<String>,
    permissions: serde_json::Value,
}

impl AppIatMinter {
    #[must_use]
    pub fn new(
        creds: GitHubAppCredentials,
        http: fabro_http::HttpClient,
        owner: String,
        repo: String,
        api_base: String,
        install_url: Option<String>,
        permissions: serde_json::Value,
    ) -> Self {
        Self {
            creds,
            http,
            owner,
            repo,
            api_base,
            install_url,
            permissions,
        }
    }
}

#[async_trait::async_trait]
impl IatMinter for AppIatMinter {
    async fn mint(&self) -> anyhow::Result<InstallationToken> {
        self.creds
            .mint_installation_token(
                &self.http,
                &self.owner,
                &self.repo,
                &self.api_base,
                self.permissions.clone(),
                self.install_url.as_deref(),
            )
            .await
    }
}

pub struct GitHubTokenSource {
    state: SourceState,
}

enum SourceState {
    Pat(String),
    StaticIat(InstallationToken),
    Mintable {
        minter: Arc<dyn IatMinter>,
        cache:  Mutex<Option<InstallationToken>>,
    },
}

impl GitHubTokenSource {
    #[must_use]
    pub fn pat(token: String) -> Self {
        Self {
            state: SourceState::Pat(token),
        }
    }

    #[must_use]
    pub fn static_iat(token: InstallationToken) -> Self {
        Self {
            state: SourceState::StaticIat(token),
        }
    }

    #[must_use]
    pub fn mintable(minter: Arc<dyn IatMinter>) -> Self {
        Self {
            state: SourceState::Mintable {
                minter,
                cache: Mutex::new(None),
            },
        }
    }

    #[must_use]
    pub fn is_refreshable(&self) -> bool {
        matches!(self.state, SourceState::Mintable { .. })
    }

    pub async fn current_token(&self) -> anyhow::Result<String> {
        match &self.state {
            SourceState::Pat(token) => Ok(token.clone()),
            SourceState::StaticIat(token) => token.valid_token().map(str::to_owned),
            SourceState::Mintable { minter, cache } => {
                let mut cache = cache.lock().await;
                let cached_is_fresh = cache
                    .as_ref()
                    .is_some_and(|token| !token.near_expiry(REFRESH_THRESHOLD));

                if !cached_is_fresh {
                    match minter.mint().await {
                        Ok(token) => *cache = Some(token),
                        Err(err) => {
                            if let Some(token) = cache.as_ref() {
                                if let Ok(value) = token.valid_token() {
                                    warn!(
                                        error = %err,
                                        "GitHub installation token refresh failed; using cached token"
                                    );
                                    return Ok(value.to_owned());
                                }
                            }
                            return Err(err)
                                .context("failed to mint GitHub installation access token");
                        }
                    }
                }

                let token = cache
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("mintable token source has no cached token"))?;
                token.valid_token().map(str::to_owned)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::anyhow;

    use super::*;

    enum MintAction {
        Token(&'static str, chrono::DateTime<chrono::Utc>),
        Error(&'static str),
    }

    struct MockMinter {
        calls:  AtomicUsize,
        script: Mutex<VecDeque<MintAction>>,
    }

    impl MockMinter {
        fn new(script: Vec<MintAction>) -> Self {
            Self {
                calls:  AtomicUsize::new(0),
                script: Mutex::new(script.into()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl IatMinter for MockMinter {
        async fn mint(&self) -> anyhow::Result<InstallationToken> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.script.lock().await.pop_front().expect("mint script") {
                MintAction::Token(token, expires_at) => Ok(InstallationToken {
                    token: token.to_string(),
                    expires_at,
                }),
                MintAction::Error(message) => Err(anyhow!(message)),
            }
        }
    }

    #[tokio::test]
    async fn pat_returns_same_token_without_minting() {
        let source = GitHubTokenSource::pat("ghp_pat".to_string());

        assert_eq!(source.current_token().await.unwrap(), "ghp_pat");
        assert_eq!(source.current_token().await.unwrap(), "ghp_pat");
        assert!(!source.is_refreshable());
    }

    #[tokio::test]
    async fn static_iat_returns_valid_token_and_rejects_expired_token() {
        let valid = GitHubTokenSource::static_iat(InstallationToken {
            token:      "ghs_valid".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        });
        assert_eq!(valid.current_token().await.unwrap(), "ghs_valid");
        assert!(!valid.is_refreshable());

        let expired = GitHubTokenSource::static_iat(InstallationToken {
            token:      "ghs_expired".to_string(),
            expires_at: chrono::Utc::now() - chrono::Duration::seconds(1),
        });
        assert!(expired.current_token().await.is_err());
    }

    #[tokio::test]
    async fn mintable_reuses_cached_token_until_refresh_threshold() {
        let minter = Arc::new(MockMinter::new(vec![MintAction::Token(
            "ghs_cached",
            chrono::Utc::now() + chrono::Duration::minutes(30),
        )]));
        let source = GitHubTokenSource::mintable(minter.clone());

        assert!(source.is_refreshable());
        assert_eq!(source.current_token().await.unwrap(), "ghs_cached");
        assert_eq!(source.current_token().await.unwrap(), "ghs_cached");
        assert_eq!(minter.calls(), 1);
    }

    #[tokio::test]
    async fn mintable_refreshes_cached_token_near_expiry() {
        let minter = Arc::new(MockMinter::new(vec![
            MintAction::Token(
                "ghs_first",
                chrono::Utc::now() + chrono::Duration::minutes(10),
            ),
            MintAction::Token(
                "ghs_second",
                chrono::Utc::now() + chrono::Duration::minutes(30),
            ),
        ]));
        let source = GitHubTokenSource::mintable(minter.clone());

        assert_eq!(source.current_token().await.unwrap(), "ghs_first");
        assert_eq!(source.current_token().await.unwrap(), "ghs_second");
        assert_eq!(minter.calls(), 2);
    }

    #[tokio::test]
    async fn mintable_uses_valid_cached_token_when_refresh_fails() {
        let minter = Arc::new(MockMinter::new(vec![
            MintAction::Token(
                "ghs_cached",
                chrono::Utc::now() + chrono::Duration::minutes(10),
            ),
            MintAction::Error("mint failed"),
        ]));
        let source = GitHubTokenSource::mintable(minter.clone());

        assert_eq!(source.current_token().await.unwrap(), "ghs_cached");
        assert_eq!(source.current_token().await.unwrap(), "ghs_cached");
        assert_eq!(minter.calls(), 2);
    }

    #[tokio::test]
    async fn mintable_errors_when_no_cached_token_can_cover_mint_failure() {
        let minter = Arc::new(MockMinter::new(vec![MintAction::Error("mint failed")]));
        let source = GitHubTokenSource::mintable(minter);

        let err = format!("{:#}", source.current_token().await.unwrap_err());
        assert!(err.contains("mint failed"), "got: {err}");
    }
}
