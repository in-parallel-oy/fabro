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

/// Default Fabro install/control port a provisioned VM listens on. Matches the
/// metafactory fleet default so a shared image needs no re-pin.
pub const DEFAULT_FABRO_PORT: u16 = 32_276;
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
    pub fabro_port:   Option<u16>,
    /// Network tag applied to the VM so a pre-created VPC firewall rule can
    /// scope egress. Paired with the host iptables drop in the startup script.
    pub egress_tag:   Option<String>,
    /// Service-account key JSON for the SA→JWT auth fallback. Held in memory
    /// only; never written to disk. Absent when workload identity is used.
    pub sa_key_json:  Option<String>,
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
            fabro_port:   lookup("FABRO_GCLOUD_FABRO_PORT").and_then(|v| v.parse().ok()),
            egress_tag:   lookup("FABRO_GCLOUD_EGRESS_TAG"),
            sa_key_json:  lookup("FABRO_GCLOUD_SA_KEY_JSON"),
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
    pub fabro_port:            u16,
    pub egress_tag:            Option<String>,
    pub egress:                EgressPolicy,
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
            fabro_port: settings.fabro_port.unwrap_or(DEFAULT_FABRO_PORT),
            egress_tag: settings.egress_tag.clone(),
            egress,
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
