//! DSH-specific ACP handshake and cold-resume ladder.
//!
//! One `dsh web` runtime serves many ccteam hires plus the human at the DSH UI,
//! so ccteam's identity for a hire travels per session in `_meta.ccteam` — never
//! in the runtime's environment, which belongs to nobody in particular.

use std::path::Path;

use serde_json::{json, Value};

use crate::execution::acp::{pluck_model_info, pluck_session_id, AcpTransport, ModelInfo};
use crate::execution::mcp_config::SessionMcpEndpoint;
use crate::{HarnessError, PermissionMode};

pub const DEFAULT_DSH_PROVIDER: &str = "deepseek-official";
pub const DEFAULT_DSH_MODEL: &str = "deepseek-v4-flash";

/// Floor for the embedded `@ccteam/ccteam-client` plugin: the first version that
/// serves ACP on `transportSocket` and reads `_meta.ccteam`. An older plugin
/// answers `initialize` perfectly well and then ignores every credential, so
/// the gate has to be here rather than in a session method.
pub const MIN_DSH_CLIENT_VERSION: &str = "0.10.3";

/// Per-session ccteam identity for `_meta.ccteam` — the plugin's
/// `CcteamSessionMeta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcteamSessionMeta {
    pub sid: String,
    pub bearer: String,
    pub mcp_url: String,
    pub approval_mode: &'static str,
    /// DSH agent-preset id for `session/new` ONLY (`standard` | `code` |
    /// `minimal` | `cordis`). Never sent on `session/load`: the stored preset
    /// in the vendor's own session data is authoritative on resume.
    pub agent_preset: Option<String>,
}

impl CcteamSessionMeta {
    pub fn new(sid: &str, mcp: &SessionMcpEndpoint, permission_mode: PermissionMode) -> Self {
        Self {
            sid: sid.to_string(),
            bearer: mcp.bearer().to_string(),
            mcp_url: mcp.url().to_string(),
            // The same mapping the deleted approval-mode child env carried.
            approval_mode: match permission_mode {
                PermissionMode::Hitl => "hitl",
                PermissionMode::Skip => "skip",
            },
            agent_preset: None,
        }
    }

    /// The `session/new` variant carrying the resolved DSH agent preset.
    pub fn with_agent_preset(mut self, agent_preset: String) -> Self {
        self.agent_preset = Some(agent_preset);
        self
    }

    fn to_json(&self) -> Value {
        let mut ccteam = json!({
            "sid": self.sid,
            "bearer": self.bearer,
            "mcpUrl": self.mcp_url,
            "approvalMode": self.approval_mode,
        });
        if let Some(preset) = &self.agent_preset {
            ccteam["agentPreset"] = json!(preset);
        }
        json!({ "ccteam": ccteam })
    }
}

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
    meta: &CcteamSessionMeta,
) -> Result<(String, ModelInfo), HarnessError> {
    let mut params = json!({ "cwd": cwd.to_string_lossy(), "_meta": meta.to_json() });
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
    meta: &CcteamSessionMeta,
) -> Result<ModelInfo, HarnessError> {
    let mut params = json!({
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
        "_meta": meta.to_json(),
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

/// What the user has to do when the peer on the socket is not a plugin ccteam
/// can drive. Named once so every arm below says the same thing.
const REGISTER_REMEDY: &str = "register or update it from the ccteam Hosts page \
     (or `dsh plugin add @ccteam/ccteam-client`) and restart your DSH web instance";

fn assert_ccteam_plugin(init: &Value) -> Result<(), HarnessError> {
    let name = init
        .pointer("/agentInfo/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if name == "deepseek-harness-acp" {
        return Err(HarnessError::SpawnFailed(format!(
            "the DSH ACP peer is the official `deepseek-harness-acp` demo, not ccteam's Cordis \
             plugin: {REGISTER_REMEDY}"
        )));
    }
    if !name.starts_with("ccteam-dsh") {
        return Err(HarnessError::SpawnFailed(format!(
            "the DSH ACP peer identified as `{name}`, not ccteam's `@ccteam/ccteam-client` plugin: \
             {REGISTER_REMEDY}"
        )));
    }
    let raw_version = init
        .pointer("/agentInfo/version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let floor = parse_client_version(MIN_DSH_CLIENT_VERSION).expect("pinned plugin floor parses");
    match parse_client_version(raw_version) {
        Some(version) if version >= floor => {}
        _ => {
            return Err(HarnessError::SpawnFailed(format!(
                "the DSH ccteam plugin reports version `{raw_version}` but ccteam needs \
                 >= {MIN_DSH_CLIENT_VERSION} (per-session `_meta.ccteam` identity): \
                 {REGISTER_REMEDY}"
            )));
        }
    }
    let load_session = init
        .pointer("/agentCapabilities/loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !load_session {
        return Err(HarnessError::SpawnFailed(format!(
            "the DSH ccteam plugin did not advertise agentCapabilities.loadSession=true: \
             {REGISTER_REMEDY}"
        )));
    }
    Ok(())
}

/// Leading `x.y.z` of a plugin version, pre-release tag ignored: the embedded
/// asset ships as `0.10.3-alpha.0` and must satisfy a `0.10.3` floor.
fn parse_client_version(raw: &str) -> Option<(u64, u64, u64)> {
    let core = raw
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
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

    fn init(version: &str) -> Value {
        json!({
            "agentInfo": {"name": "ccteam-dsh-client", "version": version},
            "agentCapabilities": {"loadSession": true},
        })
    }

    #[test]
    fn official_demo_peer_is_rejected() {
        let err = assert_ccteam_plugin(&json!({"agentInfo":{"name":"deepseek-harness-acp"}}))
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("official `deepseek-harness-acp` demo"));
        assert!(err.to_string().contains("dsh plugin add"));
    }

    #[test]
    fn missing_load_session_capability_is_rejected() {
        let err = assert_ccteam_plugin(&json!({
            "agentInfo": {"name": "ccteam-dsh-client", "version": MIN_DSH_CLIENT_VERSION},
        }))
        .unwrap_err();
        assert!(err.to_string().contains("loadSession=true"));
    }

    /// A pre-socket plugin answers `initialize` happily and then silently drops
    /// every `_meta.ccteam` credential, so the floor is enforced here.
    #[test]
    fn plugin_below_the_socket_floor_is_rejected_with_a_remedy() {
        assert!(assert_ccteam_plugin(&init("0.10.3-alpha.0")).is_ok());
        assert!(assert_ccteam_plugin(&init("0.10.3")).is_ok());
        assert!(assert_ccteam_plugin(&init("0.11.0")).is_ok());
        assert!(assert_ccteam_plugin(&init("1.0.0")).is_ok());

        for old in ["0.10.2", "0.9.15", "0.10.2-alpha.9", "", "not-a-version"] {
            let err = assert_ccteam_plugin(&init(old)).unwrap_err();
            assert!(
                err.to_string().contains(MIN_DSH_CLIENT_VERSION)
                    && err.to_string().contains("dsh plugin add"),
                "`{old}` must be refused with the register/update remedy: {err}"
            );
        }
    }

    #[test]
    fn session_meta_carries_the_per_session_principal_flat_under_ccteam() {
        let mcp = SessionMcpEndpoint::at("http://127.0.0.1:7331/mcp", "s7", "sekret").unwrap();
        let meta = CcteamSessionMeta::new("s7", &mcp, PermissionMode::Hitl);
        assert_eq!(
            meta.to_json(),
            json!({"ccteam": {
                "sid": "s7",
                "bearer": "ccteam-sid:s7:sekret",
                "mcpUrl": "http://127.0.0.1:7331/mcp",
                "approvalMode": "hitl",
            }})
        );
        assert_eq!(
            CcteamSessionMeta::new("s7", &mcp, PermissionMode::Skip).approval_mode,
            "skip"
        );
    }
}
