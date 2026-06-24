//! Operator + per-run configuration for the gcloud (GCE-per-run) provider.
//!
//! [`GcloudSettings`] is the operator-supplied substrate identity (project,
//! zone, image, machine type, subnetwork) resolved once from environment
//! variables. [`GcloudConfig`] is the per-run snapshot handed to a single
//! [`crate::gcloud::GcloudSandbox`].
//!
//! Nothing here ever holds a credential — auth is resolved lazily by
//! [`crate::gcloud::auth`]. The created VM carries **no attached service
//! account**, mirroring the metafactory fleet provisioner: untrusted job code
//! inside the run must never receive a GCP identity token.

use std::time::Duration;

use crate::gcloud::egress::EgressPolicy;

/// Instance name prefix. The control-plane SA's IAM condition is expected to
/// be bound to `resource.name.startsWith("<prefix>")` to cap blast radius.
pub const DEFAULT_NAME_PREFIX: &str = "fabro-run-";
/// Linux user the ephemeral SSH key authorizes on the VM.
pub const DEFAULT_SSH_USER: &str = "fabro";
/// Working directory the repo is cloned into on the VM.
pub const DEFAULT_WORKING_DIR: &str = "/home/fabro/workspace";

/// Operator-resolved GCE substrate identity. Substrate identifiers may be
/// `None` in dev/test where the provider is never actually invoked; the
/// provider validates required fields at `create()` time, not construction.
#[derive(Debug, Clone, Default)]
pub struct GcloudSettings {
    pub project:      Option<String>,
    pub zone:         Option<String>,
    pub subnetwork:   Option<String>,
    pub vm_image:     Option<String>,
    pub machine_type: Option<String>,
    pub name_prefix:  Option<String>,
    pub ssh_user:     Option<String>,
    pub working_dir:  Option<String>,
    /// Network tag applied to the VM so a pre-created VPC firewall rule can
    /// scope egress. Paired with the host iptables drop in the startup script.
    pub egress_tag:   Option<String>,
    /// Service-account key JSON for the SA→JWT auth fallback. Held in memory
    /// only; never written to disk. Absent when workload identity is used.
    pub sa_key_json:  Option<String>,
    /// Raw `FABRO_GCLOUD_PROVISIONING_MODEL` (`STANDARD` | `SPOT`). Validated +
    /// defaulted in [`GcloudConfig::resolve`].
    pub provisioning_model:    Option<String>,
    /// Raw `FABRO_GCLOUD_MAX_RUN_DURATION_SECS` (optional positive integer).
    /// Parsed in [`GcloudConfig::resolve`]; sets a GCE-side hard TTL on the VM.
    pub max_run_duration_secs: Option<String>,
}

impl GcloudSettings {
    /// Resolve operator settings from environment via the supplied lookup.
    /// The lookup indirection keeps this testable without touching the real
    /// process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            project:      lookup("FABRO_GCLOUD_PROJECT"),
            zone:         lookup("FABRO_GCLOUD_ZONE"),
            subnetwork:   lookup("FABRO_GCLOUD_SUBNETWORK"),
            vm_image:     lookup("FABRO_GCLOUD_VM_IMAGE"),
            machine_type: lookup("FABRO_GCLOUD_MACHINE_TYPE"),
            name_prefix:  lookup("FABRO_GCLOUD_NAME_PREFIX"),
            ssh_user:     lookup("FABRO_GCLOUD_SSH_USER"),
            working_dir:  lookup("FABRO_GCLOUD_WORKING_DIR"),
            egress_tag:   lookup("FABRO_GCLOUD_EGRESS_TAG"),
            sa_key_json:  lookup("FABRO_GCLOUD_SA_KEY_JSON"),
            provisioning_model:    lookup("FABRO_GCLOUD_PROVISIONING_MODEL"),
            max_run_duration_secs: lookup("FABRO_GCLOUD_MAX_RUN_DURATION_SECS"),
        }
    }

    /// True when the minimum substrate identity needed to insert an instance
    /// is present.
    #[must_use]
    pub fn is_provisionable(&self) -> bool {
        self.project.is_some()
            && self.zone.is_some()
            && self.vm_image.is_some()
            && self.machine_type.is_some()
            && self.subnetwork.is_some()
    }
}

/// Per-run resolved configuration. Cloned into a single `GcloudSandbox`.
#[derive(Debug, Clone)]
pub struct GcloudConfig {
    pub project:               String,
    pub zone:                  String,
    pub subnetwork:            String,
    pub vm_image:              String,
    pub machine_type:          String,
    pub name_prefix:           String,
    pub ssh_user:              String,
    pub working_dir:           String,
    pub egress_tag:            Option<String>,
    pub egress:                EgressPolicy,
    /// Compute `scheduling.provisioningModel`: `STANDARD` (default) or `SPOT`.
    pub provisioning_model:    String,
    /// GCE-side hard TTL in seconds (`scheduling.maxRunDuration`). `None` leaves
    /// the VM running until explicit delete.
    pub max_run_duration_secs: Option<u64>,
    pub operation_timeout:     Duration,
    pub host_key_poll_timeout: Duration,
    pub ssh_connect_timeout:   Duration,
}

impl GcloudConfig {
    /// Build a per-run config from operator settings + the run's egress
    /// policy. Returns an error naming the first missing required field.
    pub fn resolve(settings: &GcloudSettings, egress: EgressPolicy) -> crate::Result<Self> {
        let require = |value: &Option<String>, name: &str| {
            value
                .clone()
                .ok_or_else(|| crate::Error::message(format!("gcloud provider: {name} is not configured")))
        };

        // Whitelist-validate an enum-style operator knob. Trims; a trimmed-empty
        // value (infra templating a var that resolves to "") is treated as unset
        // → default, never a hard error that would dead-end the provider.
        let enum_field = |value: &Option<String>, name: &str, allowed: &[&str], default: &str| -> crate::Result<String> {
            match value.as_deref().map(str::trim) {
                None | Some("") => Ok(default.to_string()),
                Some(raw) => {
                    let upper = raw.to_ascii_uppercase();
                    if allowed.contains(&upper.as_str()) {
                        Ok(upper)
                    } else {
                        Err(crate::Error::message(format!(
                            "gcloud provider: {name} must be one of {allowed:?}, got {raw:?}"
                        )))
                    }
                }
            }
        };

        let provisioning_model = enum_field(
            &settings.provisioning_model,
            "FABRO_GCLOUD_PROVISIONING_MODEL",
            &["STANDARD", "SPOT"],
            "STANDARD",
        )?;

        // Optional GCE-side hard TTL. Trimmed-empty → unset; otherwise a positive
        // integer. Errors name the offending var so a bad value fails fast at
        // resolve, not at API insert.
        let max_run_duration_secs = match settings.max_run_duration_secs.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => {
                let secs = raw.parse::<u64>().map_err(|_| {
                    crate::Error::message(format!(
                        "gcloud provider: FABRO_GCLOUD_MAX_RUN_DURATION_SECS must be a positive \
                         integer, got {raw:?}"
                    ))
                })?;
                if secs == 0 {
                    return Err(crate::Error::message(
                        "gcloud provider: FABRO_GCLOUD_MAX_RUN_DURATION_SECS must be greater than 0",
                    ));
                }
                Some(secs)
            }
        };

        // A restrictive egress policy relies on TWO layers (VPC firewall scoped
        // by the network tag + host iptables). With no `FABRO_GCLOUD_EGRESS_TAG`
        // the firewall layer is silently absent and only the in-VM iptables drop
        // applies — surface that so an operator doesn't believe they have
        // network isolation they don't actually have.
        if settings.egress_tag.is_none()
            && matches!(egress, EgressPolicy::Block | EgressPolicy::AllowList(_))
        {
            tracing::warn!(
                "gcloud provider: a restrictive egress policy is configured but \
                 FABRO_GCLOUD_EGRESS_TAG is unset — the VPC-firewall enforcement layer is \
                 absent; only the in-VM iptables defence applies"
            );
        }

        Ok(Self {
            project: require(&settings.project, "FABRO_GCLOUD_PROJECT")?,
            zone: require(&settings.zone, "FABRO_GCLOUD_ZONE")?,
            subnetwork: require(&settings.subnetwork, "FABRO_GCLOUD_SUBNETWORK")?,
            vm_image: require(&settings.vm_image, "FABRO_GCLOUD_VM_IMAGE")?,
            machine_type: require(&settings.machine_type, "FABRO_GCLOUD_MACHINE_TYPE")?,
            name_prefix: settings
                .name_prefix
                .clone()
                .unwrap_or_else(|| DEFAULT_NAME_PREFIX.to_string()),
            ssh_user: settings
                .ssh_user
                .clone()
                .unwrap_or_else(|| DEFAULT_SSH_USER.to_string()),
            working_dir: settings
                .working_dir
                .clone()
                .unwrap_or_else(|| DEFAULT_WORKING_DIR.to_string()),
            egress_tag: settings.egress_tag.clone(),
            egress,
            provisioning_model,
            max_run_duration_secs,
            operation_timeout: Duration::from_secs(180),
            host_key_poll_timeout: Duration::from_secs(180),
            ssh_connect_timeout: Duration::from_secs(120),
        })
    }

    /// The compute region derived from the zone (`us-central1-a` →
    /// `us-central1`).
    #[must_use]
    pub fn region(&self) -> String {
        self.zone
            .rsplit_once('-')
            .map_or_else(|| self.zone.clone(), |(region, _)| region.to_string())
    }

    /// Fully-qualified subnetwork URL fragment for `instances.insert`.
    #[must_use]
    pub fn subnetwork_url(&self) -> String {
        if self.subnetwork.contains('/') {
            self.subnetwork.clone()
        } else {
            format!(
                "projects/{}/regions/{}/subnetworks/{}",
                self.project,
                self.region(),
                self.subnetwork
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_reports_first_missing_field() {
        let settings = GcloudSettings::default();
        let err = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap_err();
        assert!(err.to_string().contains("FABRO_GCLOUD_PROJECT"));
    }

    #[test]
    fn region_is_derived_from_zone() {
        let settings = full_settings();
        let config = GcloudConfig::resolve(&settings, EgressPolicy::AllowAll).unwrap();
        assert_eq!(config.region(), "us-central1");
        assert_eq!(
            config.subnetwork_url(),
            "projects/proj/regions/us-central1/subnetworks/default"
        );
    }

    #[test]
    fn explicit_subnetwork_path_is_passed_through() {
        let mut settings = full_settings();
        settings.subnetwork = Some("projects/p/regions/r/subnetworks/s".to_string());
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        assert_eq!(config.subnetwork_url(), "projects/p/regions/r/subnetworks/s");
    }

    #[test]
    fn scheduling_knobs_default_to_standard_no_ttl() {
        let config = GcloudConfig::resolve(&full_settings(), EgressPolicy::AllowAll).unwrap();
        assert_eq!(config.provisioning_model, "STANDARD");
        assert_eq!(config.max_run_duration_secs, None);
    }

    #[test]
    fn provisioning_model_is_trimmed_and_uppercased() {
        let mut settings = full_settings();
        settings.provisioning_model = Some("  spot ".to_string());
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        assert_eq!(config.provisioning_model, "SPOT");
    }

    #[test]
    fn blank_provisioning_model_falls_back_to_default() {
        let mut settings = full_settings();
        settings.provisioning_model = Some("   ".to_string());
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        assert_eq!(config.provisioning_model, "STANDARD");
    }

    #[test]
    fn invalid_provisioning_model_errors_naming_the_var() {
        let mut settings = full_settings();
        settings.provisioning_model = Some("PREEMPTIBLE".to_string());
        let err = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap_err();
        assert!(err.to_string().contains("FABRO_GCLOUD_PROVISIONING_MODEL"));
    }

    #[test]
    fn max_run_duration_parses_positive_integer() {
        let mut settings = full_settings();
        settings.max_run_duration_secs = Some("3600".to_string());
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        assert_eq!(config.max_run_duration_secs, Some(3600));
    }

    #[test]
    fn blank_max_run_duration_is_unset() {
        let mut settings = full_settings();
        settings.max_run_duration_secs = Some("  ".to_string());
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        assert_eq!(config.max_run_duration_secs, None);
    }

    #[test]
    fn non_numeric_max_run_duration_errors_naming_the_var() {
        let mut settings = full_settings();
        settings.max_run_duration_secs = Some("soon".to_string());
        let err = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap_err();
        assert!(err.to_string().contains("FABRO_GCLOUD_MAX_RUN_DURATION_SECS"));
    }

    #[test]
    fn zero_max_run_duration_errors() {
        let mut settings = full_settings();
        settings.max_run_duration_secs = Some("0".to_string());
        let err = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap_err();
        assert!(err.to_string().contains("FABRO_GCLOUD_MAX_RUN_DURATION_SECS"));
        assert!(err.to_string().contains("greater than 0"));
    }

    fn full_settings() -> GcloudSettings {
        GcloudSettings {
            project:      Some("proj".to_string()),
            zone:         Some("us-central1-a".to_string()),
            subnetwork:   Some("default".to_string()),
            vm_image:     Some("projects/proj/global/images/fabro".to_string()),
            machine_type: Some("e2-standard-4".to_string()),
            ..Default::default()
        }
    }
}
