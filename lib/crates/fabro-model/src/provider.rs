use serde::{Deserialize, Serialize};

use crate::adapter::AdapterKind;
use crate::catalog::CatalogProvider;
use crate::ids::ProviderId;

/// A user-facing LLM provider from the catalog.
///
/// The public projection of [`CatalogProvider`]. It deliberately omits
/// internal-only fields (`auth`, `extra_headers`, `billing_policy`,
/// `agent_profile`) so credential material never reaches the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    pub id:                   ProviderId,
    pub display_name:         String,
    pub adapter:              AdapterKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url:             Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_url:          Option<String>,
    pub priority:             i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases:              Vec<String>,
    /// Number of catalog models for this provider. Stamped by the handler.
    pub model_count:          u32,
    /// Catalog default model ID for this provider, if any. Stamped by the
    /// handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model:        Option<String>,
    /// True if the server has credential material configured for this provider
    /// when the response is produced. Always `false` in static catalog data;
    /// stamped by `GET /providers` per request.
    #[serde(default)]
    pub configured:           bool,
    /// Suggested vault secret name for configuring this provider, derived
    /// from the first vault credential in the catalog. `None` when the
    /// provider has no vault credential (e.g. Ollama, env-only providers).
    /// Used by the web UI to prefill the create-secret form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_secret_name: Option<String>,
}

impl Provider {
    #[must_use]
    pub fn from_catalog(
        provider: &CatalogProvider,
        model_count: u32,
        default_model: Option<String>,
        configured: bool,
    ) -> Self {
        Self {
            id: provider.id.clone(),
            display_name: provider.display_name.clone(),
            adapter: provider.adapter,
            base_url: provider.base_url.clone(),
            api_key_url: provider.api_key_url.clone(),
            priority: provider.priority,
            aliases: provider.aliases.clone(),
            model_count,
            default_model,
            configured,
            expected_secret_name: provider.vault_secret_name().map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Provider;
    use crate::catalog::Catalog;
    use crate::ids::ProviderId;

    #[test]
    fn from_catalog_provider_copies_static_fields_and_supplied_runtime_fields() {
        let catalog = Catalog::builtin();
        let anthropic = catalog
            .provider(&ProviderId::anthropic())
            .expect("builtin catalog must define anthropic");

        let provider =
            Provider::from_catalog(anthropic, 7, Some("claude-opus-4-7".to_string()), true);

        assert_eq!(provider.id, ProviderId::anthropic());
        assert_eq!(provider.display_name, anthropic.display_name);
        assert_eq!(provider.adapter, anthropic.adapter);
        assert_eq!(provider.priority, anthropic.priority);
        assert_eq!(provider.model_count, 7);
        assert_eq!(provider.default_model.as_deref(), Some("claude-opus-4-7"));
        assert!(provider.configured);
        assert_eq!(
            provider.expected_secret_name.as_deref(),
            Some("ANTHROPIC_API_KEY"),
        );
    }
}
