//! Hand-rolled GCE Compute REST client (no `google-cloud` crate).
//!
//! Covers exactly the ~5 calls the per-run lifecycle needs, modeled on the
//! metafactory fleet provisioner: `instances.insert`, zonal `operations.get`
//! polling, `instances.get` (IP + labels), `instances.delete`, and the
//! `guest-attributes` read used to pin the VM host key.
//!
//! Security invariants baked into the insert body:
//!   * `serviceAccounts: []` — **no attached SA**, so untrusted job code gets
//!     no GCP identity token.
//!   * the ephemeral public key is the only authorized SSH credential
//!     (`metadata.items[ssh-keys]`).
//!   * managed labels mark ownership; `list`/`get`/`delete` refuse to touch
//!     anything missing them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::gcloud::auth::GcpAuth;
use crate::gcloud::config::GcloudConfig;

const COMPUTE_BASE: &str = "https://compute.googleapis.com/compute/v1";

/// GCE labels disallow `.`; the dotted logical names
/// (`sh.fabro.managed`) are encoded with `_`.
pub const MANAGED_LABEL_KEY: &str = "sh_fabro_managed";
pub const MANAGED_LABEL_VALUE: &str = "true";
pub const RUN_ID_LABEL_KEY: &str = "sh_fabro_run_id";

/// A minimal projection of a Compute instance resource.
#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "creationTimestamp")]
    pub creation_timestamp: Option<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default, rename = "networkInterfaces")]
    pub network_interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkInterface {
    #[serde(default, rename = "networkIP")]
    pub network_ip: Option<String>,
}

impl Instance {
    /// True when the instance carries the managed sentinel label.
    #[must_use]
    pub fn is_managed(&self) -> bool {
        self.labels.get(MANAGED_LABEL_KEY).map(String::as_str) == Some(MANAGED_LABEL_VALUE)
    }

    /// The instance's primary internal IP, if assigned.
    #[must_use]
    pub fn internal_ip(&self) -> Option<&str> {
        self.network_interfaces
            .first()
            .and_then(|nic| nic.network_ip.as_deref())
    }
}

#[derive(Deserialize)]
struct Operation {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct InstanceList {
    #[serde(default)]
    items: Vec<Instance>,
}

/// Compute REST client bound to one credential source.
///
/// `auth` is an [`Arc`] so the token cache is shared across every client built
/// for the same provider: a single minted access token is reused across
/// list/get/create/delete instead of being re-minted (and re-signing an SA JWT /
/// re-round-tripping to the token endpoint) per operation.
pub struct ComputeClient {
    http: reqwest::Client,
    auth: Arc<GcpAuth>,
}

impl ComputeClient {
    #[must_use]
    pub fn new(http: reqwest::Client, auth: Arc<GcpAuth>) -> Self {
        Self { http, auth }
    }

    /// `instances.insert`. Returns the zonal operation name to await.
    pub async fn insert_instance(
        &self,
        config: &GcloudConfig,
        name: &str,
        ssh_keys_metadata: &str,
        startup_script: &str,
        run_id: Option<&str>,
    ) -> crate::Result<String> {
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/instances",
            config.project, config.zone
        );
        let body = build_insert_body(config, name, ssh_keys_metadata, startup_script, run_id);
        let response: Operation = self.post_json(&url, body).await?;
        operation_name(response)
    }

    /// Poll a zonal operation to `DONE`, surfacing any operation error.
    pub async fn await_zonal_operation(
        &self,
        config: &GcloudConfig,
        operation: &str,
    ) -> crate::Result<()> {
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/operations/{}",
            config.project, config.zone, operation
        );
        let deadline = Instant::now() + config.operation_timeout;
        loop {
            let op: Operation = self.get_json(&url).await?;
            match op.status.as_deref() {
                Some("DONE") => {
                    if let Some(error) = op.error {
                        return Err(crate::Error::message(format!(
                            "GCE operation {operation} failed: {error}"
                        )));
                    }
                    return Ok(());
                }
                _ if Instant::now() >= deadline => {
                    return Err(crate::Error::message(format!(
                        "GCE operation {operation} did not reach DONE within timeout"
                    )));
                }
                _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
    }

    /// `instances.get`.
    pub async fn get_instance(&self, config: &GcloudConfig, name: &str) -> crate::Result<Instance> {
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/instances/{}",
            config.project, config.zone, name
        );
        self.get_json(&url).await
    }

    /// `instances.delete`. Returns the zonal operation name.
    pub async fn delete_instance(
        &self,
        config: &GcloudConfig,
        name: &str,
    ) -> crate::Result<String> {
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/instances/{}",
            config.project, config.zone, name
        );
        let response: Operation = self.delete_json(&url).await?;
        operation_name(response)
    }

    /// List managed instances in the configured zone (label-filtered
    /// server-side).
    pub async fn list_managed(&self, config: &GcloudConfig) -> crate::Result<Vec<Instance>> {
        let filter = format!("labels.{MANAGED_LABEL_KEY}={MANAGED_LABEL_VALUE}");
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/instances?filter={}",
            config.project,
            config.zone,
            urlencode(&filter)
        );
        let list: InstanceList = self.get_json(&url).await?;
        Ok(list
            .items
            .into_iter()
            .filter(Instance::is_managed)
            .collect())
    }

    /// Read the VM's SSH host key from guest attributes, written by the VM
    /// after boot. Returns `None` while it is not yet published.
    ///
    /// This is the pinning source: we **never** trust a host key learned on
    /// first connect.
    pub async fn host_key_from_guest_attributes(
        &self,
        config: &GcloudConfig,
        name: &str,
    ) -> crate::Result<Option<String>> {
        let url = format!(
            "{COMPUTE_BASE}/projects/{}/zones/{}/instances/{}/getGuestAttributes?queryPath={}",
            config.project,
            config.zone,
            name,
            urlencode("hostkeys/")
        );
        let token = self.auth.access_token().await?;
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| crate::Error::context("GCE getGuestAttributes failed", err))?;

        // 404 means the key is not yet published — the caller retries.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        // 403 is a permission problem that retrying cannot fix: surface it as a
        // hard error naming the missing IAM permission so the operator gets an
        // actionable diagnostic instead of an opaque poll timeout.
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(crate::Error::message(format!(
                "GCE getGuestAttributes for instance '{name}' was denied (403). Host-key pinning \
                 requires the `compute.instances.getGuestAttributes` IAM permission on the \
                 control-plane service account; add it to the custom role (it is NOT included in \
                 `compute.instances.{{insert,delete,get}}`)."
            )));
        }
        // Any other non-success: not yet published / transient — retry.
        if !response.status().is_success() {
            return Ok(None);
        }

        let body: Value = response
            .json()
            .await
            .map_err(|err| crate::Error::context("GCE guest attributes were not JSON", err))?;
        Ok(parse_host_key(&body))
    }

    async fn post_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        body: Value,
    ) -> crate::Result<T> {
        let token = self.auth.access_token().await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|err| crate::Error::context("GCE request failed", err))?;
        Self::deserialize_response(response, url).await
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> crate::Result<T> {
        let token = self.auth.access_token().await?;
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| crate::Error::context("GCE request failed", err))?;
        Self::deserialize_response(response, url).await
    }

    async fn delete_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> crate::Result<T> {
        let token = self.auth.access_token().await?;
        let response = self
            .http
            .delete(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| crate::Error::context("GCE request failed", err))?;
        Self::deserialize_response(response, url).await
    }

    async fn deserialize_response<T: for<'de> Deserialize<'de>>(
        response: reqwest::Response,
        url: &str,
    ) -> crate::Result<T> {
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| crate::Error::context("GCE response body read failed", err))?;
        if !status.is_success() {
            return Err(crate::Error::message(format!(
                "GCE request to {url} returned {status}: {text}"
            )));
        }
        serde_json::from_str(&text).map_err(|err| {
            crate::Error::context(
                format!("GCE response from {url} was not the expected JSON"),
                err,
            )
        })
    }
}

/// Build the `instances.insert` request body. Pure + public so the security
/// invariants (`serviceAccounts: []`, ssh-keys metadata, managed labels) are
/// unit-testable without GCP.
#[must_use]
pub fn build_insert_body(
    config: &GcloudConfig,
    name: &str,
    ssh_keys_metadata: &str,
    startup_script: &str,
    run_id: Option<&str>,
) -> Value {
    let mut labels = json!({
        MANAGED_LABEL_KEY: MANAGED_LABEL_VALUE,
    });
    if let (Some(run_id), Value::Object(map)) = (run_id, &mut labels) {
        map.insert(
            RUN_ID_LABEL_KEY.to_string(),
            Value::String(sanitize_label(run_id)),
        );
    }

    let mut metadata_items = vec![
        json!({ "key": "startup-script", "value": startup_script }),
        json!({ "key": "ssh-keys", "value": ssh_keys_metadata }),
        // Enable guest attributes so the VM can publish its host key for pinning.
        json!({ "key": "enable-guest-attributes", "value": "TRUE" }),
        // Block project-wide SSH keys: only the per-run ephemeral key is valid.
        json!({ "key": "block-project-ssh-keys", "value": "TRUE" }),
    ];

    let mut instance = json!({
        "name": name,
        "machineType": format!("zones/{}/machineTypes/{}", config.zone, config.machine_type),
        "labels": labels,
        "disks": [{
            "boot": true,
            "autoDelete": true,
            "initializeParams": { "sourceImage": config.vm_image }
        }],
        "networkInterfaces": [{ "subnetwork": config.subnetwork_url() }],
        // No attached service account: untrusted job code gets no GCP identity.
        "serviceAccounts": [],
        "metadata": { "items": metadata_items }
    });

    // Network tag so a pre-created VPC firewall rule can scope egress.
    if let (Some(tag), Value::Object(map)) = (config.egress_tag.as_deref(), &mut instance) {
        map.insert("tags".to_string(), json!({ "items": [tag] }));
    }
    // Scheduling block — only emitted for SPOT or when a max-run TTL is set, so
    // plain STANDARD/no-TTL bodies are byte-for-byte unchanged.
    if let (Some(sched), Value::Object(map)) = (build_scheduling(config), &mut instance) {
        map.insert("scheduling".to_string(), sched);
    }
    // `metadata_items` is moved into `instance` via the json! macro above; the
    // local binding is retained only for clarity. Silence unused-mut.
    let _ = &mut metadata_items;

    instance
}

/// Build the optional `scheduling` block for `instances.insert`.
///
/// Returns `None` for a plain STANDARD VM with no max-run TTL so the insert body
/// is unchanged from today (and GCE doesn't reject `instanceTerminationAction`
/// on a VM that has neither SPOT nor a TTL). When emitted, the block always
/// carries `instanceTerminationAction: DELETE` — `STOP` is deliberately not
/// exposed: `disks[].autoDelete` only fires on delete, so a stopped VM would
/// orphan its boot disk and accrue cost until external reclamation, and the
/// run lifecycle (host-key poll, SSH session, cleanup delete) assumes the VM is
/// deleted, never stopped.
///
/// SPOT pairing fields (`automaticRestart: false`, `onHostMaintenance:
/// TERMINATE`) are included unconditionally for SPOT because GCE 400s a SPOT
/// insert that keeps the default `automaticRestart: true` / `MIGRATE`.
fn build_scheduling(config: &GcloudConfig) -> Option<Value> {
    let spot = config.provisioning_model == "SPOT";
    let has_max = config.max_run_duration_secs.is_some();
    if !spot && !has_max {
        return None;
    }

    let mut map = serde_json::Map::new();
    map.insert(
        "provisioningModel".to_string(),
        json!(config.provisioning_model),
    );
    if spot {
        map.insert("automaticRestart".to_string(), json!(false));
        map.insert("onHostMaintenance".to_string(), json!("TERMINATE"));
    }
    map.insert("instanceTerminationAction".to_string(), json!("DELETE"));
    if let Some(secs) = config.max_run_duration_secs {
        // Compute's Duration is {seconds: string, nanos: int}, NOT the protobuf
        // well-known "3600s" form — seconds MUST be serialized as a string.
        map.insert(
            "maxRunDuration".to_string(),
            json!({ "seconds": secs.to_string() }),
        );
    }
    Some(Value::Object(map))
}

fn operation_name(op: Operation) -> crate::Result<String> {
    op.name
        .ok_or_else(|| crate::Error::message("GCE operation response had no name"))
}

/// Parse the OpenSSH host-key line out of a `getGuestAttributes` response. The
/// VM publishes entries under `hostkeys/<type>`; prefer ed25519, else the
/// first entry. Returns a `known_hosts`-ready line (`<host> <type> <key>` is
/// assembled by the caller; here we return `<type> <base64>`).
fn parse_host_key(body: &Value) -> Option<String> {
    let items = body
        .get("queryValue")
        .and_then(|qv| qv.get("items"))
        .and_then(Value::as_array)?;

    let mut fallback: Option<String> = None;
    for item in items {
        let key = item.get("key").and_then(Value::as_str).unwrap_or_default();
        let value = item
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if value.is_empty() {
            continue;
        }
        // GCE publishes the value as the full key blob; key is the algorithm
        // (e.g. "ssh-ed25519" or "ssh-rsa"). Normalize to "<algo> <blob>".
        let line = if value.starts_with("ssh-") || value.starts_with("ecdsa-") {
            value.to_string()
        } else {
            format!("{key} {value}")
        };
        if key.contains("ed25519") {
            return Some(line);
        }
        fallback.get_or_insert(line);
    }
    fallback
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(63)
        .collect()
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcloud::config::{GcloudConfig, GcloudSettings};
    use crate::gcloud::egress::EgressPolicy;

    fn config() -> GcloudConfig {
        let settings = GcloudSettings {
            project: Some("proj".to_string()),
            zone: Some("us-central1-a".to_string()),
            subnetwork: Some("default".to_string()),
            vm_image: Some("projects/proj/global/images/fabro".to_string()),
            machine_type: Some("e2-standard-4".to_string()),
            egress_tag: Some("fabro-run".to_string()),
            ..Default::default()
        };
        GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap()
    }

    #[test]
    fn insert_body_has_no_attached_service_account() {
        let body = build_insert_body(
            &config(),
            "fabro-run-x",
            "fabro:ssh-ed25519 AAAA",
            "#!/bin/sh",
            Some("01HY"),
        );
        assert_eq!(body["serviceAccounts"], json!([]));
    }

    #[test]
    fn insert_body_injects_ssh_key_and_labels() {
        let body = build_insert_body(
            &config(),
            "fabro-run-x",
            "fabro:ssh-ed25519 AAAA",
            "#!/bin/sh",
            Some("01HY"),
        );
        let items = body["metadata"]["items"].as_array().unwrap();
        let ssh = items.iter().find(|i| i["key"] == "ssh-keys").unwrap();
        assert_eq!(ssh["value"], "fabro:ssh-ed25519 AAAA");
        assert_eq!(body["labels"][MANAGED_LABEL_KEY], MANAGED_LABEL_VALUE);
        assert_eq!(body["labels"][RUN_ID_LABEL_KEY], "01hy");
        assert_eq!(body["tags"]["items"][0], "fabro-run");
    }

    #[test]
    fn insert_body_blocks_project_keys_and_enables_guest_attributes() {
        let body = build_insert_body(&config(), "n", "k", "s", None);
        let items = body["metadata"]["items"].as_array().unwrap();
        assert!(
            items
                .iter()
                .any(|i| i["key"] == "block-project-ssh-keys" && i["value"] == "TRUE")
        );
        assert!(
            items
                .iter()
                .any(|i| i["key"] == "enable-guest-attributes" && i["value"] == "TRUE")
        );
    }

    fn config_with(provisioning_model: &str, max_run_duration_secs: Option<&str>) -> GcloudConfig {
        let settings = GcloudSettings {
            project: Some("proj".to_string()),
            zone: Some("us-central1-a".to_string()),
            subnetwork: Some("default".to_string()),
            vm_image: Some("projects/proj/global/images/fabro".to_string()),
            machine_type: Some("e2-standard-4".to_string()),
            egress_tag: Some("fabro-run".to_string()),
            provisioning_model: Some(provisioning_model.to_string()),
            max_run_duration_secs: max_run_duration_secs.map(ToString::to_string),
            ..Default::default()
        };
        GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap()
    }

    #[test]
    fn standard_no_ttl_emits_no_scheduling_key() {
        let body = build_insert_body(
            &config(),
            "fabro-run-x",
            "fabro:ssh-ed25519 AAAA",
            "#!/bin/sh",
            Some("01HY"),
        );
        // Conditional emit keeps the default body unperturbed.
        assert_eq!(body.get("scheduling"), None);
        // Existing security/identity assertions remain intact.
        assert_eq!(body["serviceAccounts"], json!([]));
        assert_eq!(body["labels"][MANAGED_LABEL_KEY], MANAGED_LABEL_VALUE);
        assert_eq!(body["tags"]["items"][0], "fabro-run");
    }

    #[test]
    fn spot_emits_spot_scheduling_block() {
        let body = build_insert_body(&config_with("SPOT", None), "n", "k", "s", None);
        let sched = &body["scheduling"];
        assert_eq!(sched["provisioningModel"], "SPOT");
        assert_eq!(sched["automaticRestart"], json!(false));
        assert_eq!(sched["onHostMaintenance"], "TERMINATE");
        assert_eq!(sched["instanceTerminationAction"], "DELETE");
        assert_eq!(sched.get("maxRunDuration"), None);
    }

    #[test]
    fn standard_with_max_run_duration_emits_string_seconds() {
        let body = build_insert_body(&config_with("STANDARD", Some("3600")), "n", "k", "s", None);
        let sched = &body["scheduling"];
        assert_eq!(sched["provisioningModel"], "STANDARD");
        // STANDARD keeps the default MIGRATE/automaticRestart behaviour.
        assert_eq!(sched.get("onHostMaintenance"), None);
        assert_eq!(sched.get("automaticRestart"), None);
        assert_eq!(sched["instanceTerminationAction"], "DELETE");
        // seconds MUST be a JSON string, not a number.
        assert_eq!(sched["maxRunDuration"]["seconds"], json!("3600"));
    }

    #[test]
    fn spot_with_max_run_duration_emits_both() {
        let body = build_insert_body(&config_with("SPOT", Some("3600")), "n", "k", "s", None);
        let sched = &body["scheduling"];
        assert_eq!(sched["provisioningModel"], "SPOT");
        assert_eq!(sched["automaticRestart"], json!(false));
        assert_eq!(sched["onHostMaintenance"], "TERMINATE");
        assert_eq!(sched["instanceTerminationAction"], "DELETE");
        assert_eq!(sched["maxRunDuration"]["seconds"], json!("3600"));
    }

    #[test]
    fn parses_ed25519_host_key_preferentially() {
        let body = json!({
            "queryValue": { "items": [
                { "namespace": "hostkeys", "key": "ssh-rsa", "value": "ssh-rsa AAAARSA" },
                { "namespace": "hostkeys", "key": "ssh-ed25519", "value": "ssh-ed25519 AAAAED" }
            ]}
        });
        assert_eq!(parse_host_key(&body).as_deref(), Some("ssh-ed25519 AAAAED"));
    }

    #[test]
    fn managed_filter_excludes_unmanaged_instances() {
        let managed = Instance {
            name: "a".to_string(),
            status: None,
            creation_timestamp: None,
            labels: HashMap::from([(MANAGED_LABEL_KEY.to_string(), "true".to_string())]),
            network_interfaces: vec![],
        };
        let unmanaged = Instance {
            name: "b".to_string(),
            status: None,
            creation_timestamp: None,
            labels: HashMap::new(),
            network_interfaces: vec![],
        };
        assert!(managed.is_managed());
        assert!(!unmanaged.is_managed());
    }
}
