//! GCP access-token acquisition for the gcloud provider.
//!
//! Two sources, in priority order:
//!   1. **Metadata workload identity** (preferred) — when the control plane
//!      runs on GCP, `GET 169.254.169.254/.../token` with the
//!      `Metadata-Flavor: Google` header yields a short-lived access token and
//!      no key ever exists.
//!   2. **Service-account JWT exchange** (fallback) — sign a JWT with the SA
//!      private key (RS256) and exchange it at `oauth2.googleapis.com/token`.
//!      The key is read from injected config and **never written to disk**.
//!
//! Tokens are cached until shortly before expiry.

use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

const METADATA_TOKEN_URL: &str = "http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/token";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// Refresh this far ahead of the reported expiry.
const EXPIRY_SKEW: Duration = Duration::from_secs(60);

/// Resolves and caches GCP access tokens.
pub struct GcpAuth {
    http:        reqwest::Client,
    sa_key_json: Option<String>,
    cached:      Mutex<Option<CachedToken>>,
}

#[derive(Clone)]
struct CachedToken {
    value:      String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in:   u64,
}

#[derive(Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key:  String,
    #[serde(default)]
    token_uri:    Option<String>,
}

#[derive(serde::Serialize)]
struct JwtClaims<'a> {
    iss:   &'a str,
    scope: &'a str,
    aud:   &'a str,
    iat:   u64,
    exp:   u64,
}

impl GcpAuth {
    #[must_use]
    pub fn new(http: reqwest::Client, sa_key_json: Option<String>) -> Self {
        Self {
            http,
            sa_key_json,
            cached: Mutex::new(None),
        }
    }

    /// Return a valid `cloud-platform` access token, fetching/refreshing as
    /// needed.
    pub async fn access_token(&self) -> crate::Result<String> {
        {
            let guard = self.cached.lock().await;
            if let Some(token) = guard.as_ref() {
                if token.expires_at > Instant::now() {
                    return Ok(token.value.clone());
                }
            }
        }

        let (value, ttl) = self.fetch_token().await?;
        let expires_at = Instant::now() + ttl.saturating_sub(EXPIRY_SKEW);
        *self.cached.lock().await = Some(CachedToken {
            value: value.clone(),
            expires_at,
        });
        Ok(value)
    }

    async fn fetch_token(&self) -> crate::Result<(String, Duration)> {
        // Prefer metadata-server workload identity; fall back to SA-JWT only
        // when an SA key is configured.
        match self.fetch_from_metadata().await {
            Ok(token) => Ok(token),
            Err(metadata_err) => match &self.sa_key_json {
                Some(key_json) => self.fetch_from_sa_jwt(key_json).await,
                None => Err(metadata_err),
            },
        }
    }

    async fn fetch_from_metadata(&self) -> crate::Result<(String, Duration)> {
        let response = self
            .http
            .get(METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|err| crate::Error::context("GCP metadata token request failed", err))?;

        if !response.status().is_success() {
            return Err(crate::Error::message(format!(
                "GCP metadata token request returned status {}",
                response.status()
            )));
        }

        let parsed: TokenResponse = response
            .json()
            .await
            .map_err(|err| crate::Error::context("GCP metadata token response was not JSON", err))?;
        Ok((parsed.access_token, Duration::from_secs(parsed.expires_in)))
    }

    async fn fetch_from_sa_jwt(&self, key_json: &str) -> crate::Result<(String, Duration)> {
        let key: ServiceAccountKey = serde_json::from_str(key_json)
            .map_err(|err| crate::Error::context("GCP SA key JSON is invalid", err))?;
        let token_uri = key.token_uri.as_deref().unwrap_or(DEFAULT_TOKEN_URI);

        let now = unix_now();
        let claims = JwtClaims {
            iss:   &key.client_email,
            scope: SCOPE,
            aud:   token_uri,
            iat:   now,
            exp:   now + 3600,
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(key.private_key.as_bytes())
            .map_err(|err| crate::Error::context("GCP SA private key is not valid RSA PEM", err))?;
        let assertion = jsonwebtoken::encode(&header, &claims, &encoding_key)
            .map_err(|err| crate::Error::context("Failed to sign GCP SA JWT", err))?;

        let response = self
            .http
            .post(token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &assertion),
            ])
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|err| crate::Error::context("GCP SA token exchange failed", err))?;

        if !response.status().is_success() {
            return Err(crate::Error::message(format!(
                "GCP SA token exchange returned status {}",
                response.status()
            )));
        }

        let parsed: TokenResponse = response
            .json()
            .await
            .map_err(|err| crate::Error::context("GCP SA token response was not JSON", err))?;
        Ok((parsed.access_token, Duration::from_secs(parsed.expires_in)))
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn errors_without_metadata_or_key() {
        // No metadata server reachable in test + no SA key → error, never a
        // silent empty token.
        let auth = GcpAuth::new(reqwest::Client::new(), None);
        assert!(auth.access_token().await.is_err());
    }
}
