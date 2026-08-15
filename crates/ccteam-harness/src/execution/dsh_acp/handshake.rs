//! DSH-specific ACP handshake and cold-resume ladder.

use std::path::Path;

use serde_json::{json, Value};

use crate::execution::acp::{pluck_model_info, pluck_session_id, AcpTransport, ModelInfo};
use crate::HarnessError;

pub const DEFAULT_DSH_PROVIDER: &str = "deepseek-official";
pub const DEFAULT_DSH_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshAgentOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl DshAgentOptions {
    pub fn new(model: Option<&str>) -> Self {
        let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self {
                provider: None,
                model: None,
            };
        };
        if let Some((provider, model)) = model.split_once('/') {
            let provider = provider.trim();
            let model = model.trim();
            if !provider.is_empty() && !model.is_empty() {
                return Self {
                    provider: Some(provider.to_string()),
                    model: Some(model.to_string()),
                };
            }
        }
        Self {
            provider: None,
            model: Some(model.to_string()),
        }
    }

    fn to_json(&self) -> Option<Value> {
        if self.provider.is_none() && self.model.is_none() {
            return None;
        }
        let mut out = serde_json::Map::new();
        if let Some(provider) = &self.provider {
            out.insert("provider".to_string(), json!(provider));
        }
        if let Some(model) = &self.model {
            out.insert("model".to_string(), json!(model));
        }
        Some(Value::Object(out))
    }

    pub fn requested_model_display(&self) -> Option<String> {
        match (&self.provider, &self.model) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (None, Some(model)) => Some(model.clone()),
            _ => None,
        }
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
    let mut params = json!({ "cwd": cwd.to_string_lossy() });
    if let Some(options) = agent_options.to_json() {
        params["agentOptions"] = options;
    }
    let result = transport
        .call("session/new", params)
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
    let mut params = json!({
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
    });
    if let Some(options) = agent_options.to_json() {
        params["agentOptions"] = options;
    }
    let result = transport
        .call("session/load", params)
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
    fn default_agent_options_are_omitted_for_plugin_default_selection() {
        let opts = DshAgentOptions::new(None);
        assert_eq!(opts.to_json(), None);
        assert_eq!(opts.requested_model_display(), None);

        let opts = DshAgentOptions::new(Some("deepseek-v4-pro"));
        assert_eq!(opts.to_json(), Some(json!({"model":"deepseek-v4-pro"})));
        assert_eq!(
            opts.requested_model_display(),
            Some("deepseek-v4-pro".to_string())
        );

        let opts = DshAgentOptions::new(Some("aliyun/deepseek-v4-pro"));
        assert_eq!(
            opts.to_json(),
            Some(json!({"provider":"aliyun","model":"deepseek-v4-pro"}))
        );
        assert_eq!(
            opts.requested_model_display(),
            Some("aliyun/deepseek-v4-pro".to_string())
        );
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
