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

/// Resolved per-run egress policy.
///
/// Constructed on the run path by `gcloud_config_from_environment`, which maps
/// the run's `EnvironmentNetworkMode` onto these variants.
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

impl EgressPolicy {
    /// Render a host firewall fragment (bash) enforcing this policy on the VM,
    /// covering **both** address families: `iptables` (IPv4) *and* `ip6tables`
    /// (IPv6). Rendering only IPv4 would leave host egress wide open on an
    /// IPv6-enabled subnet, bypassing the in-VM defence-in-depth layer.
    ///
    /// The metadata-server drop is unconditional so untrusted job code can never
    /// reach `169.254.169.254`, regardless of policy. The GCE metadata server is
    /// IPv4-only, so there is no IPv6 counterpart to drop.
    #[must_use]
    pub fn iptables_script(&self) -> String {
        let mut lines = vec![
            "#!/usr/bin/env bash".to_string(),
            "set -euo pipefail".to_string(),
            "# Always drop egress to the GCE metadata server (no SA token leak).".to_string(),
            "# It is IPv4-only (169.254.169.254); there is no IPv6 equivalent.".to_string(),
            "iptables -A OUTPUT -d 169.254.169.254 -j DROP || true".to_string(),
        ];

        match self {
            Self::AllowAll => {}
            Self::Block => {
                for bin in FIREWALL_BINS {
                    push_baseline_accepts(&mut lines, bin);
                    lines.push(format!("{bin} -P OUTPUT DROP || true"));
                }
            }
            Self::AllowList(cidrs) => {
                for bin in FIREWALL_BINS {
                    push_baseline_accepts(&mut lines, bin);
                }
                for cidr in cidrs {
                    if !is_safe_cidr(cidr) {
                        continue;
                    }
                    // Route each CIDR to its address family: a v6 CIDR fed to
                    // `iptables` (or a v4 CIDR to `ip6tables`) is rejected, so
                    // without family routing the allow rule silently never takes
                    // effect and the traffic is dropped by the default policy.
                    let bin = if cidr.contains(':') {
                        "ip6tables"
                    } else {
                        "iptables"
                    };
                    lines.push(format!("{bin} -A OUTPUT -d {cidr} -j ACCEPT || true"));
                }
                for bin in FIREWALL_BINS {
                    lines.push(format!("{bin} -P OUTPUT DROP || true"));
                }
            }
        }

        lines.join("\n")
    }
}

/// The two firewall binaries we render rules for, one per IP family.
const FIREWALL_BINS: [&str; 2] = ["iptables", "ip6tables"];

/// Loopback + established/related accepts that must precede a default-drop so
/// the SSH control channel and local traffic survive the policy.
fn push_baseline_accepts(lines: &mut Vec<String>, bin: &str) {
    lines.push(format!("{bin} -A OUTPUT -o lo -j ACCEPT || true"));
    lines.push(format!(
        "{bin} -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT || true"
    ));
}

/// True when a CIDR is restricted to the iptables/ip6tables address charset, so
/// an operator/run-supplied value can't inject shell into the rendered script.
fn is_safe_cidr(cidr: &str) -> bool {
    cidr.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b':' || b == b'/')
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
        let script =
            EgressPolicy::AllowList(vec!["10.0.0.0/8; rm -rf /".to_string()]).iptables_script();
        assert!(!script.contains("rm -rf"));
    }

    #[test]
    fn block_renders_both_address_families() {
        let script = EgressPolicy::Block.iptables_script();
        // Without the ip6tables default-drop, IPv6 egress is unrestricted on an
        // IPv6-enabled subnet.
        assert!(script.contains("iptables -P OUTPUT DROP"));
        assert!(script.contains("ip6tables -P OUTPUT DROP"));
        assert!(script.contains("ip6tables -A OUTPUT -o lo -j ACCEPT"));
    }

    #[test]
    fn allow_list_routes_each_cidr_to_its_family() {
        let script =
            EgressPolicy::AllowList(vec!["10.0.0.0/8".to_string(), "2001:db8::/32".to_string()])
                .iptables_script();
        assert!(script.contains("iptables -A OUTPUT -d 10.0.0.0/8 -j ACCEPT"));
        assert!(script.contains("ip6tables -A OUTPUT -d 2001:db8::/32 -j ACCEPT"));
        // The v6 CIDR must NOT be handed to iptables (it would be rejected).
        assert!(!script.contains("iptables -A OUTPUT -d 2001:db8::/32"));
        // Both families default-drop.
        assert!(script.contains("iptables -P OUTPUT DROP"));
        assert!(script.contains("ip6tables -P OUTPUT DROP"));
    }
}
