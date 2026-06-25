#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxServiceDiscoverySource {
    Ss,
    Procfs,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxServiceListMeta {
    pub source: SandboxServiceDiscoverySource,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxService {
    pub port: u16,
    pub addresses: Vec<String>,
    pub processes: Vec<String>,
    pub preview_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxServiceListResponse {
    pub data: Vec<SandboxService>,
    pub meta: SandboxServiceListMeta,
}
