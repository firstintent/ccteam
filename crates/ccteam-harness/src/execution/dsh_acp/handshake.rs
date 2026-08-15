//! DSH-specific ACP handshake and cold-resume ladder.

use std::path::Path;

use serde_json::{json, Value};

use crate::execution::acp::{pluck_model_info, pluck_session_id, AcpTransport, ModelInfo};
use crate::HarnessError;

pub const DEFAULT_DSH_PROVIDER: &str = "deepseek-official";
pub const DEFAULT_DSH_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshAgentOptions {
    pub provider: String,
    pub model: String,
}

impl DshAgentOptions {
    pub fn new(model: Option<&str>) -> Self {
        Self {
            // DSH's agent-default-model is visible only inside the plugin. The
            // Rust adapter must still send a pair, so use dsh-base's static
            // default observed in W0 before user settings hot-load.
            provider: DEFAULT_DSH_PROVIDER.to_string(),
            model: model
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_DSH_MODEL)
                .to_string(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "provider": self.provider,
            "model": self.model,
        })
    }
}

pub async fn initialize(transport: &AcpTransport) -> Result<(), HarnessError> {
    let init = transport
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "ccteam",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("dsh initialize failed: {e}")))?;

    assert_ccteam_plugin(&init)?;
    let _ = transport
        .notify("notifications/initialized", Value::Null)
        .await;
    Ok(())
}

pub async fn session_new(
    transport: &AcpTransport,
    cwd: &Path,
    agent_options: &DshAgentOptions,
) -> Result<(String, ModelInfo), HarnessError> {
    let result = transport
        .call(
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                "agentOptions": agent_options.to_json()
            }),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("dsh session/new failed: {e}")))?;

    let session_id = pluck_session_id(&result)
        .ok_or_else(|| HarnessError::SpawnFailed("dsh session/new missing sessionId".into()))?;
    Ok((session_id, pluck_model_info(&result)))
}

pub async fn session_load(
    transport: &AcpTransport,
    cwd: &Path,
    session_id: &str,
    agent_options: &DshAgentOptions,
) -> Result<ModelInfo, HarnessError> {
    let result = transport
        .call(
            "session/load",
            json!({
                "sessionId": session_id,
                "cwd": cwd.to_string_lossy(),
                "agentOptions": agent_options.to_json()
            }),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("dsh session/load failed: {e}")))?;
    Ok(pluck_model_info(&result))
}

fn assert_ccteam_plugin(init: &Value) -> Result<(), HarnessError> {
    let name = init
        .pointer("/agentInfo/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if name == "deepseek-harness-acp" {
        return Err(HarnessError::SpawnFailed(
            "DSH ACP peer is the official `deepseek-harness-acp` demo, not ccteam's Cordis plugin; \
             check profile materialization for `dsh --profile ccteam`"
                .to_string(),
        ));
    }
    if !name.starts_with("ccteam-dsh") {
        return Err(HarnessError::SpawnFailed(format!(
            "DSH ACP peer identified as `{name}`; expected ccteam's `ccteam-dsh-client` plugin"
        )));
    }
    let load_session = init
        .pointer("/agentCapabilities/loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !load_session {
        return Err(HarnessError::SpawnFailed(
            "DSH ccteam plugin did not advertise agentCapabilities.loadSession=true".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agent_options_are_explicit() {
        let opts = DshAgentOptions::new(None);
        assert_eq!(opts.provider, "deepseek-official");
        assert_eq!(opts.model, "deepseek-v4-flash");
        let opts = DshAgentOptions::new(Some("deepseek-v4-pro"));
        assert_eq!(opts.provider, "deepseek-official");
        assert_eq!(opts.model, "deepseek-v4-pro");
    }

    #[test]
    fn official_demo_peer_is_rejected() {
        let err = assert_ccteam_plugin(&json!({"agentInfo":{"name":"deepseek-harness-acp"}}))
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("official `deepseek-harness-acp` demo"));
    }

    #[test]
    fn missing_load_session_capability_is_rejected() {
        let err =
            assert_ccteam_plugin(&json!({"agentInfo":{"name":"ccteam-dsh-client"}})).unwrap_err();
        assert!(err.to_string().contains("loadSession=true"));
    }
}
