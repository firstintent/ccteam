use serde::{Deserialize, Serialize};

/// Vendor discriminator for core data and user-facing wire payloads.
///
/// Execution adapters still use `ccteam_harness::AgentVendor` until the
/// harness trait can depend on core primitives without a cargo cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentVendor {
    Claude,
    Codex,
}

impl AgentVendor {
    pub fn cost_vendor(self) -> ccteam_cost::Vendor {
        match self {
            AgentVendor::Claude => ccteam_cost::Vendor::Claude,
            AgentVendor::Codex => ccteam_cost::Vendor::Codex,
        }
    }
}
