//! [`SandboxProvider`] surface for the gcloud (GCE-per-run) backend.
//!
//! Mirrors the Daytona provider's shape: list/get/delete filter strictly on
//! the managed label and **refuse to touch unmanaged resources**; create builds
//! a [`GcloudSandbox`], initializes it (insert VM → pin host key → SSH → clone)
//! and projects the live instance into a [`SandboxInfo`].

use async_trait::async_trait;
use fabro_types::{
    SandboxInfo, SandboxNetwork, SandboxProviderKind, SandboxResources, SandboxState,
    SandboxTimestamps,
};

use super::{SandboxCreateSpec, SandboxProvider};
use crate::gcloud::compute::{ComputeClient, Instance, RUN_ID_LABEL_KEY};
use crate::gcloud::config::{GcloudConfig, GcloudSettings};
use crate::Sandbox;
use crate::gcloud::{GcloudSandbox, auth::GcpAuth};

/// GCE-per-run provider. Stateless beyond the resolved operator settings and a
/// shared HTTP client.
#[derive(Clone)]
pub struct GcloudSandboxProvider {
    settings: GcloudSettings,
    http:     reqwest::Client,
}

impl GcloudSandboxProvider {
    #[must_use]
    pub fn new(settings: GcloudSettings, http: reqwest::Client) -> Self {
        Self { settings, http }
    }

    fn compute(&self) -> ComputeClient {
        let auth = GcpAuth::new(self.http.clone(), self.settings.sa_key_json.clone());
        ComputeClient::new(self.http.clone(), auth)
    }

    /// Resolve a config for read-only operations (list/get/delete), which only
    /// need substrate identity, not a per-run egress policy.
    fn read_config(&self) -> crate::Result<GcloudConfig> {
        GcloudConfig::resolve(&self.settings, crate::gcloud::EgressPolicy::AllowAll)
    }
}

#[async_trait]
impl SandboxProvider for GcloudSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Gcloud
    }

    async fn list(&self) -> crate::Result<Vec<SandboxInfo>> {
        let config = self.read_config()?;
        let instances = self.compute().list_managed(&config).await?;
        Ok(instances
            .iter()
            .map(|instance| instance_to_info(instance, &config))
            .collect())
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let config = self.read_config()?;
        match self.compute().get_instance(&config, id).await {
            Ok(instance) if instance.is_managed() => Ok(Some(instance_to_info(&instance, &config))),
            Ok(_) => Ok(None),
            Err(err) if is_not_found(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        let SandboxCreateSpec::Gcloud {
            config,
            run_id,
            clone_origin_url,
            clone_branch,
        } = spec
        else {
            return Err(crate::Error::message(
                "gcloud sandbox provider can only create gcloud sandboxes",
            ));
        };

        let read_config = (*config).clone();
        let sandbox = GcloudSandbox::new(
            *config,
            self.http.clone(),
            self.settings.sa_key_json.clone(),
            run_id,
            clone_origin_url,
            clone_branch,
        )?;
        sandbox.initialize().await?;

        let name = sandbox
            .instance_name()
            .ok_or_else(|| crate::Error::message("gcloud sandbox initialized without an instance name"))?;
        let instance = self.compute().get_instance(&read_config, name).await?;
        Ok(instance_to_info(&instance, &read_config))
    }

    async fn delete(&self, id: &str) -> crate::Result<()> {
        let config = self.read_config()?;
        let compute = self.compute();
        let instance = match compute.get_instance(&config, id).await {
            Ok(instance) => instance,
            Err(err) if is_not_found(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        if !instance.is_managed() {
            return Err(crate::Error::message(format!(
                "Refusing to delete GCE instance '{id}' because it is missing the managed label"
            )));
        }
        let operation = compute.delete_instance(&config, id).await?;
        compute.await_zonal_operation(&config, &operation).await
    }
}

fn is_not_found(err: &crate::Error) -> bool {
    err.to_string().contains("returned 404")
}

fn instance_to_info(instance: &Instance, config: &GcloudConfig) -> SandboxInfo {
    let mut labels: std::collections::BTreeMap<String, String> =
        instance.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let display_name = labels
        .get(RUN_ID_LABEL_KEY)
        .cloned()
        .map(|run_id| format!("run {run_id}"));
    labels.entry("zone".to_string()).or_insert_with(|| config.zone.clone());

    SandboxInfo {
        provider:          SandboxProviderKind::Gcloud,
        id:                instance.name.clone(),
        display_name,
        state:             map_state(instance.status.as_deref()),
        native_state:      instance.status.clone(),
        image:             Some(config.vm_image.clone()),
        snapshot:          None,
        region:            Some(config.region()),
        web_url:           None,
        working_directory: Some(config.working_dir.clone()),
        resources:         SandboxResources::default(),
        network:           SandboxNetwork::unknown(),
        labels,
        timestamps:        SandboxTimestamps::default(),
    }
}

fn map_state(status: Option<&str>) -> SandboxState {
    match status {
        Some("PROVISIONING" | "STAGING") => SandboxState::Provisioning,
        Some("RUNNING") => SandboxState::Running,
        Some("STOPPING") => SandboxState::Stopping,
        Some("TERMINATED" | "STOPPED" | "SUSPENDED") => SandboxState::Stopped,
        _ => SandboxState::Unknown,
    }
}
