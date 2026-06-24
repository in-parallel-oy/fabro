use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray, VariantNames};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
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
pub enum AgentBackend {
    Api,
    Acp,
    Tmux, // ponytail: rebase anchor — tmux backend
}

impl AgentBackend {
    #[must_use]
    pub fn expected_values() -> String {
        <Self as VariantNames>::VARIANTS.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::AgentBackend;

    #[test]
    fn agent_backend_accepts_api_acp_and_tmux() {
        assert_eq!("api".parse::<AgentBackend>().unwrap(), AgentBackend::Api);
        assert_eq!("acp".parse::<AgentBackend>().unwrap(), AgentBackend::Acp);
        assert_eq!("tmux".parse::<AgentBackend>().unwrap(), AgentBackend::Tmux);
        assert!("cli".parse::<AgentBackend>().is_err());
        assert_eq!(AgentBackend::expected_values(), "api, acp, tmux");
    }
}
