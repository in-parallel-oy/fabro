use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray, VariantNames};

/// Selects how Fabro provides credentials to an ACP child process.
///
/// `Fabro` resolves credentials through `fabro-auth` and injects them into
/// the child env (API key + optional CLI login command). `Host` skips that
/// path entirely so the spawned ACP agent uses whatever subscription session
/// is already authenticated on the host (e.g. `~/.codex/auth.json` from
/// `codex login`, or `~/.claude/credentials.json` from `claude login`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
    VariantNames,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AcpAuthMode {
    #[default]
    Fabro,
    Host,
}

impl AcpAuthMode {
    #[must_use]
    pub fn expected_values() -> String {
        <Self as VariantNames>::VARIANTS.join(", ")
    }
}
