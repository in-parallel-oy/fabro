//! Egress policy for a gcloud run, mirroring the Daytona network model
//! (`Block | AllowAll | AllowList(cidrs)`).
//!
//! Enforcement is two-layered, exactly like the metafactory fleet:
//!   1. **VPC firewall** scoped by the VM's network tag (created out-of-band by
//!      the operator) — the authoritative, kernel-external control.
//!   2. **Host iptables** rendered into the startup script — defence in depth,
//!      and the only thing that can drop egress to the metadata server
//!      (`169.254.169.254`) without breaking the control channel.
//!
//! This type only *describes* the policy and renders the host-side iptables
//! fragment; it never opens a socket.

use fabro_types::{SandboxNetworkPolicy, SandboxNetworkPolicyMode};

/// Resolved per-run egress policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Drop all outbound traffic (except loopback + the established control
    /// channel).
    Block,
    /// No additional restriction beyond the always-on metadata-server drop.
    AllowAll,
    /// Permit outbound only to the listed CIDRs.
    AllowList(Vec<String>),
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self::AllowAll
    }
}

impl EgressPolicy {
    /// Map a run's resolved [`SandboxNetworkPolicy`] onto an egress policy.
    /// Unknown/essentials-only modes conservatively resolve to `AllowAll`
    /// (the metadata-server drop still applies).
    #[must_use]
    pub fn from_network_policy(policy: &SandboxNetworkPolicy) -> Self {
        match policy.mode() {
            SandboxNetworkPolicyMode::Blocked => Self::Block,
            SandboxNetworkPolicyMode::CidrAllowList => Self::AllowList(policy.cidrs().to_vec()),
            SandboxNetworkPolicyMode::Open
            | SandboxNetworkPolicyMode::EssentialsOnly
            | SandboxNetworkPolicyMode::Unknown => Self::AllowAll,
        }
    }

    /// Render an iptables fragment (bash) enforcing this policy on the VM. The
    /// metadata-server drop is unconditional so untrusted job code can never
    /// reach `169.254.169.254`, regardless of policy.
    #[must_use]
    pub fn iptables_script(&self) -> String {
        let mut lines = vec![
            "#!/usr/bin/env bash".to_string(),
            "set -euo pipefail".to_string(),
            "# Always drop egress to the GCE metadata server (no SA token leak).".to_string(),
            "iptables -A OUTPUT -d 169.254.169.254 -j DROP || true".to_string(),
        ];

        match self {
            Self::AllowAll => {}
            Self::Block => {
                lines.push("iptables -A OUTPUT -o lo -j ACCEPT || true".to_string());
                lines.push(
                    "iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT || true"
                        .to_string(),
                );
                lines.push("iptables -P OUTPUT DROP || true".to_string());
            }
            Self::AllowList(cidrs) => {
                lines.push("iptables -A OUTPUT -o lo -j ACCEPT || true".to_string());
                lines.push(
                    "iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT || true"
                        .to_string(),
                );
                for cidr in cidrs {
                    // The CIDR is operator/run supplied; restrict to the
                    // iptables address charset so it can't inject shell.
                    if cidr.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b':' || b == b'/') {
                        lines.push(format!("iptables -A OUTPUT -d {cidr} -j ACCEPT || true"));
                    }
                }
                lines.push("iptables -P OUTPUT DROP || true".to_string());
            }
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_all_only_drops_metadata() {
        let script = EgressPolicy::AllowAll.iptables_script();
        assert!(script.contains("169.254.169.254"));
        assert!(!script.contains("-P OUTPUT DROP"));
    }

    #[test]
    fn block_sets_default_drop() {
        let script = EgressPolicy::Block.iptables_script();
        assert!(script.contains("-P OUTPUT DROP"));
    }

    #[test]
    fn allow_list_rejects_injection() {
        let script = EgressPolicy::AllowList(vec!["10.0.0.0/8; rm -rf /".to_string()]).iptables_script();
        assert!(!script.contains("rm -rf"));
    }

    #[test]
    fn maps_blocked_network_policy() {
        let policy = SandboxNetworkPolicy::blocked();
        assert_eq!(EgressPolicy::from_network_policy(&policy), EgressPolicy::Block);
    }
}
