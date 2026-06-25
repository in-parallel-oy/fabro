//! Convert resolved [`RunEnvironmentSettings`] into runtime sandbox configs.
//!
//! These mappings are consumed by both the workflow run-start path and the
//! server preflight path, so they live here next to their destination types.

use std::path::{Path, PathBuf};

#[cfg(feature = "daytona")]
use fabro_types::settings::run::DockerfileSource as ResolvedDockerfileSource;
use fabro_types::settings::run::{EnvironmentNetworkMode, RunEnvironmentSettings};

#[cfg(feature = "daytona")]
use crate::config::{
    DaytonaNetwork, DaytonaSnapshotSettings, DockerfileSource as SandboxDockerfileSource,
};
#[cfg(feature = "daytona")]
use crate::daytona::DaytonaConfig;
#[cfg(feature = "docker")]
use crate::docker::DockerSandboxOptions;
#[cfg(feature = "gcloud")]
use crate::gcloud::{EgressPolicy, GcloudConfig, GcloudSettings};

#[cfg(feature = "daytona")]
#[must_use]
pub fn daytona_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DaytonaConfig {
    DaytonaConfig {
        auto_stop_interval: settings
            .lifecycle
            .auto_stop
            .map(|duration| duration_to_minutes_i32(duration.as_std())),
        labels: (!settings.labels.is_empty()).then(|| settings.labels.clone()),
        snapshot: settings
            .image
            .dockerfile
            .as_ref()
            .map(|dockerfile| DaytonaSnapshotSettings {
                cpu: settings.resources.cpu,
                memory: settings
                    .resources
                    .memory
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                disk: settings
                    .resources
                    .disk
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                dockerfile: Some(match dockerfile {
                    ResolvedDockerfileSource::Inline(text) => {
                        SandboxDockerfileSource::Inline(text.clone())
                    }
                    ResolvedDockerfileSource::Path { path } => {
                        SandboxDockerfileSource::Path { path: path.clone() }
                    }
                }),
            }),
        network: Some(match settings.network.mode {
            EnvironmentNetworkMode::Block => DaytonaNetwork::Block,
            EnvironmentNetworkMode::AllowAll => DaytonaNetwork::AllowAll,
            EnvironmentNetworkMode::CidrAllowList => {
                DaytonaNetwork::AllowList(settings.network.allow.clone())
            }
        }),
        skip_clone,
    }
}

#[cfg(feature = "docker")]
#[must_use]
pub fn docker_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DockerSandboxOptions {
    let mut env_vars = settings
        .resolve_env(process_env_var)
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    env_vars.sort();
    let default_options = DockerSandboxOptions::default();

    // Fork-only: bind specs (`id:mount_path:ro|rw`) pass straight through to
    // bollard `HostConfig.binds`. Named-volume binds are auto-created by
    // Docker on first mount. Upstream dropped the `volumes` model; narayan
    // declares these under `[environments.*].binds`.
    let binds = settings.binds.clone();

    DockerSandboxOptions {
        image: settings
            .image
            .docker
            .clone()
            .unwrap_or(default_options.image),
        network_mode: match settings.network.mode {
            EnvironmentNetworkMode::Block => Some("none".to_string()),
            EnvironmentNetworkMode::AllowAll | EnvironmentNetworkMode::CidrAllowList => {
                default_options.network_mode
            }
        },
        memory_limit: settings
            .resources
            .memory
            .and_then(|size| i64::try_from(size.as_bytes()).ok()),
        cpu_quota: settings
            .resources
            .cpu
            .map(|cpu| i64::from(cpu).saturating_mul(100_000)),
        env_vars,
        binds,
        skip_clone,
        ..DockerSandboxOptions::default()
    }
}

/// Resolve a per-run [`GcloudConfig`] from the run's environment settings plus
/// the operator `FABRO_GCLOUD_*` env (via `lookup`). Returns the config and the
/// SA key JSON (a secret carried only in the transient `SandboxSpec`, never
/// persisted). Errors when a required substrate identifier is missing.
///
/// The egress policy is mapped from the environment network mode, mirroring the
/// Daytona network model.
#[cfg(feature = "gcloud")]
pub fn gcloud_config_from_environment(
    settings: &RunEnvironmentSettings,
    lookup: impl Fn(&str) -> Option<String>,
) -> crate::Result<(GcloudConfig, Option<String>)> {
    let operator = GcloudSettings::from_lookup(&lookup);
    let sa_key_json = operator.sa_key_json.clone();
    let egress = match settings.network.mode {
        EnvironmentNetworkMode::Block => EgressPolicy::Block,
        EnvironmentNetworkMode::AllowAll => EgressPolicy::AllowAll,
        EnvironmentNetworkMode::CidrAllowList => {
            EgressPolicy::AllowList(settings.network.allow.clone())
        }
    };
    let config = GcloudConfig::resolve(&operator, egress)?;
    Ok((config, sa_key_json))
}

pub fn local_working_directory_from_environment(
    settings: &RunEnvironmentSettings,
    source_directory: Option<&Path>,
) -> crate::Result<PathBuf> {
    if let Some(cwd) = settings.cwd.as_deref() {
        return Ok(PathBuf::from(cwd));
    }

    let Some(source_directory) = source_directory else {
        return Err(crate::Error::message(
            "local environment requires a server-side working directory; configure `environment.cwd = \"/absolute/path\"` on the selected local environment",
        ));
    };

    if source_directory.is_dir() {
        return Ok(source_directory.to_path_buf());
    }

    Err(crate::Error::message(format!(
        "local environment source_directory does not exist or is not a directory on this server: {}. Configure `environment.cwd = \"/absolute/path\"` on the selected local environment for remote client/server deployments.",
        source_directory.display()
    )))
}

#[cfg(feature = "docker")]
#[expect(
    clippy::disallowed_methods,
    reason = "Environment interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(feature = "daytona")]
fn duration_to_minutes_i32(duration: std::time::Duration) -> i32 {
    let minutes = duration.as_secs() / 60;
    i32::try_from(minutes).unwrap_or(i32::MAX)
}

#[cfg(feature = "daytona")]
fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use fabro_types::settings::run::{
        EnvironmentImageSettings, EnvironmentLifecycleSettings, EnvironmentNetworkSettings,
        EnvironmentProvider, EnvironmentResourcesSettings,
    };

    use super::*;

    fn run_environment(provider: EnvironmentProvider) -> RunEnvironmentSettings {
        RunEnvironmentSettings {
            id: "host".to_string(),
            provider,
            cwd: None,
            image: EnvironmentImageSettings::default(),
            resources: EnvironmentResourcesSettings::default(),
            network: EnvironmentNetworkSettings::default(),
            lifecycle: EnvironmentLifecycleSettings::default(),
            labels: HashMap::new(),
            env: HashMap::new(),
            binds: Vec::new(),
        }
    }

    #[cfg(feature = "docker")]
    #[test]
    fn docker_binds_pass_through_to_host_config() {
        let mut settings = run_environment(EnvironmentProvider::Docker);
        settings.binds = vec![
            "narayan-fabro-hex:/home/dev/.hex:rw".to_string(),
            "narayan-seed-build:/seed/_build:ro".to_string(),
        ];

        let options = docker_config_from_environment(&settings, false);

        assert_eq!(options.binds, settings.binds);
    }

    #[cfg(feature = "gcloud")]
    #[test]
    fn gcloud_config_maps_cidr_allow_list_and_carries_sa_key() {
        let mut settings = run_environment(EnvironmentProvider::Local);
        settings.network.mode = fabro_types::settings::run::EnvironmentNetworkMode::CidrAllowList;
        settings.network.allow = vec!["10.0.0.0/8".to_string()];

        let env = HashMap::from([
            ("FABRO_GCLOUD_PROJECT".to_string(), "proj".to_string()),
            ("FABRO_GCLOUD_ZONE".to_string(), "us-central1-a".to_string()),
            ("FABRO_GCLOUD_SUBNETWORK".to_string(), "default".to_string()),
            (
                "FABRO_GCLOUD_VM_IMAGE".to_string(),
                "projects/proj/global/images/fabro".to_string(),
            ),
            (
                "FABRO_GCLOUD_MACHINE_TYPE".to_string(),
                "e2-standard-4".to_string(),
            ),
            (
                "FABRO_GCLOUD_SA_KEY_JSON".to_string(),
                "{\"k\":1}".to_string(),
            ),
        ]);

        let (config, sa_key_json) =
            gcloud_config_from_environment(&settings, |key| env.get(key).cloned()).unwrap();

        assert_eq!(
            config.egress,
            EgressPolicy::AllowList(vec!["10.0.0.0/8".to_string()])
        );
        assert_eq!(sa_key_json.as_deref(), Some("{\"k\":1}"));
    }

    #[cfg(feature = "gcloud")]
    #[test]
    fn gcloud_config_errors_on_missing_substrate() {
        let settings = run_environment(EnvironmentProvider::Local);
        let err = gcloud_config_from_environment(&settings, |_| None).unwrap_err();
        assert!(err.to_string().contains("FABRO_GCLOUD_PROJECT"));
    }

    #[test]
    fn local_working_directory_prefers_environment_cwd() {
        let mut settings = run_environment(EnvironmentProvider::Local);
        settings.cwd = Some("/srv/fabro/workspaces/team-a".to_string());
        let missing_source = Path::new("/path/that/should/not/exist");

        let resolved = local_working_directory_from_environment(&settings, Some(missing_source))
            .expect("configured cwd should be accepted");

        assert_eq!(resolved, PathBuf::from("/srv/fabro/workspaces/team-a"));
        assert!(!missing_source.exists());
    }

    #[test]
    fn local_working_directory_uses_existing_source_directory_without_cwd() {
        let settings = run_environment(EnvironmentProvider::Local);
        let dir = tempfile::tempdir().unwrap();

        let resolved = local_working_directory_from_environment(&settings, Some(dir.path()))
            .expect("existing source directory should be accepted");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn local_working_directory_rejects_missing_source_directory_without_cwd() {
        let settings = run_environment(EnvironmentProvider::Local);
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("client-only");

        let err = local_working_directory_from_environment(&settings, Some(&missing))
            .expect_err("missing source directory without cwd should fail");

        let message = err.to_string();
        assert!(
            message.contains("environment.cwd") && message.contains("does not exist"),
            "unexpected error: {message}"
        );
        assert!(!missing.exists());
    }
}
