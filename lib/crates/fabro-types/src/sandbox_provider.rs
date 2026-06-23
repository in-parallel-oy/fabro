use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};

use crate::settings::run::RunMode;

/// Sandbox provider discriminator for agent tool operations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Display, EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum SandboxProviderKind {
    /// Run tools on the local host.
    #[default]
    Local,
    /// Run tools inside a Docker container.
    Docker,
    /// Run tools inside a Daytona cloud sandbox.
    Daytona,
    /// Run tools inside an ephemeral GCE VM provisioned per run.
    ///
    /// This variant is intentionally **not** `#[cfg(feature = "gcloud")]`-gated,
    /// even though the heavy gcloud *implementation* (and its `openssh` /
    /// `ssh-key` / `jsonwebtoken` deps) is feature-gated inside `fabro-sandbox`.
    /// Gating the discriminant here was evaluated and rejected:
    ///
    ///   * `SandboxProviderKind` is a dependency-free unit enum in the
    ///     foundational `fabro-types` crate; the variant costs nothing to compile
    ///     unconditionally (no transitive deps ride on it).
    ///   * Gating it would force a new `gcloud` Cargo feature on `fabro-types`
    ///     **and** every downstream crate that matches on it (`fabro-sandbox`,
    ///     `fabro-server`, `fabro-install`, `fabro-workflow`), each needing the
    ///     feature plumbed through to compile its arm — more upstream-merge
    ///     surface, not less.
    ///   * It does not shrink the merge-conflict footprint either: the
    ///     `Gcloud => ...` arms still exist textually at the same call sites; a
    ///     `#[cfg]` attribute only *adds* a line per arm.
    ///   * `is_clone_based` matches the variant inside a `matches!` or-pattern
    ///     (`Self::Docker | Self::Daytona | Self::Gcloud`), which cannot be
    ///     cfg-gated per-alternative without splitting the pattern.
    ///
    /// Net: gating is strictly more code and complexity for zero build or
    /// conflict-surface benefit, so the variant stays un-gated by design.
    Gcloud,
}

impl SandboxProviderKind {
    /// True only for Local. Used by dry-run to force local execution.
    /// NOT the same as "runs on the host" (Docker is host-adjacent but not
    /// dry-run compatible).
    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// True for providers that clone repository sources into their workspace.
    #[must_use]
    pub fn is_clone_based(&self) -> bool {
        matches!(self, Self::Docker | Self::Daytona | Self::Gcloud)
    }

    /// Coerce non-local providers to `Local` under dry-run; otherwise
    /// unchanged.
    #[must_use]
    pub fn effective_for(self, mode: RunMode) -> Self {
        if mode == RunMode::DryRun && !self.is_local() {
            Self::Local
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SandboxProviderKind;

    #[test]
    fn sandbox_provider_default_is_local() {
        assert_eq!(SandboxProviderKind::default(), SandboxProviderKind::Local);
    }

    #[test]
    fn sandbox_provider_from_str() {
        assert_eq!(
            "local".parse::<SandboxProviderKind>().unwrap(),
            SandboxProviderKind::Local
        );
        assert_eq!(
            "docker".parse::<SandboxProviderKind>().unwrap(),
            SandboxProviderKind::Docker
        );
        assert_eq!(
            "daytona".parse::<SandboxProviderKind>().unwrap(),
            SandboxProviderKind::Daytona
        );
        assert_eq!(
            "LOCAL".parse::<SandboxProviderKind>().unwrap(),
            SandboxProviderKind::Local
        );
        assert!("invalid".parse::<SandboxProviderKind>().is_err());
    }

    #[test]
    fn sandbox_provider_display() {
        assert_eq!(SandboxProviderKind::Local.to_string(), "local");
        assert_eq!(SandboxProviderKind::Docker.to_string(), "docker");
        assert_eq!(SandboxProviderKind::Daytona.to_string(), "daytona");
    }
}
