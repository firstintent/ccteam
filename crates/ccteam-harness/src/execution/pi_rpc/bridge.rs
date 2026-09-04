//! Embedded Pi extension bridge and interaction contract.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ccteam_root_from_env, ApprovalIR, ApprovalRisk, ChoicePrompt, HarnessError};

pub const BRIDGE_STATUS_KEY: &str = "ccteam.bridge";
pub const PERMISSION_DIALOG_TITLE: &str = "__ccteam_permission_v1__";
pub const MAX_PERMISSION_ENVELOPE_BYTES: usize = 64 * 1024;

/// Every ccteam MCP tool a managed session may be served. The bridge itself
/// contains no copy of this list: it registers and reports what `tools/list`
/// actually returned. This adapter-side set turns an UNKNOWN name into a spawn
/// failure, and the web `/mcp` integration test locks it to the server's full
/// face.
///
/// It is a KNOWN set, not a required one: the daemon composes the face per
/// caller, so a depth-capped or `tools:"read"` child legitimately sees a subset
/// (and a `tools:"none"` child sees nothing at all). Demanding the full list
/// would make exactly the sessions the face exists to slim down fail to start.
pub const KNOWN_MCP_TOOL_NAMES: &[&str] = &[
    "status",
    "grok_claude_codex_kimi",
    "chat_send_file",
    "agent",
    "agent_read",
    "agent_stop",
];

const BRIDGE_SOURCE: &str = include_str!("ccteam_bridge.mjs");

pub fn bridge_source() -> &'static str {
    BRIDGE_SOURCE
}

pub fn materialize_bridge() -> Result<PathBuf, HarnessError> {
    let root = ccteam_root_from_env()
        .ok_or_else(|| HarnessError::SpawnFailed("cannot resolve CCTEAM_HOME".into()))?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let runtime_dir = root.join("runtime").join("pi");
    std::fs::create_dir_all(&runtime_dir)?;
    let hash = format!("{:x}", Sha256::digest(BRIDGE_SOURCE.as_bytes()));
    let path = runtime_dir.join(format!("ccteam-bridge-{hash}.mjs"));
    if path.exists() {
        let existing = std::fs::read(&path)?;
        if existing != BRIDGE_SOURCE.as_bytes() {
            return Err(HarnessError::Io(format!(
                "Pi bridge hash collision at {}",
                path.display()
            )));
        }
        set_private_permissions(&path)?;
        return Ok(path);
    }

    let tmp = runtime_dir.join(format!(".ccteam-bridge-{hash}-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(BRIDGE_SOURCE.as_bytes())?;
    file.sync_all()?;
    drop(file);
    match std::fs::rename(&tmp, &path) {
        Ok(()) => {}
        Err(error) if path.exists() => {
            let _ = std::fs::remove_file(&tmp);
            if std::fs::read(&path)? != BRIDGE_SOURCE.as_bytes() {
                return Err(HarnessError::Io(format!(
                    "Pi bridge materialization raced with different content: {error}"
                )));
            }
        }
        Err(error) => return Err(error.into()),
    }
    set_private_permissions(&path)?;
    if let Ok(dir) = std::fs::File::open(&runtime_dir) {
        let _ = dir.sync_all();
    }
    Ok(path)
}

fn set_private_permissions(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Gate the names the bridge reported at `session_start`: no duplicates, and
/// every name must be one ccteam actually ships. A SUBSET is valid (the daemon
/// serves a per-caller face) and so is the EMPTY set (`agent{tools:"none"}`).
pub fn validate_ready_names(names: &[String]) -> Result<(), String> {
    let actual: HashSet<&str> = names.iter().map(String::as_str).collect();
    if actual.len() != names.len() {
        return Err("Pi bridge reported duplicate tool names".to_string());
    }
    let known: HashSet<String> = KNOWN_MCP_TOOL_NAMES
        .iter()
        .map(|name| format!("ccteam_{name}"))
        .collect();
    let mut unknown: Vec<&str> = actual
        .iter()
        .filter(|name| !known.contains(**name))
        .copied()
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let mut known: Vec<_> = known.into_iter().collect();
        known.sort();
        return Err(format!(
            "Pi bridge reported unknown ccteam tools [{}]; known: [{}]",
            unknown.join(", "),
            known.join(", ")
        ));
    }
    Ok(())
}

pub fn parse_ready_status(
    status_key: &str,
    status_text: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    if status_key != BRIDGE_STATUS_KEY {
        return Ok(None);
    }
    let text =
        status_text.ok_or_else(|| "Pi bridge cleared status before becoming ready".to_string())?;
    let names = text
        .strip_prefix("ready:")
        .ok_or_else(|| format!("invalid Pi bridge status `{text}`"))?
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    validate_ready_names(&names)?;
    Ok(Some(names))
}

pub fn auto_allows_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read"
            | "grep"
            | "find"
            | "ls"
            | "ccteam_status"
            | "ccteam_grok_claude_codex_kimi"
            | "ccteam_agent_read"
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum PiApprovalDecision {
    Allow,
    Deny(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PiDialogKind {
    Select { options: Vec<String> },
    Confirm { message: String },
    Input { placeholder: Option<String> },
    Editor { prefill: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PiDialogRequest {
    pub request_id: String,
    pub title: String,
    pub kind: PiDialogKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PiDialogResponse {
    Value(String),
    Confirmed(bool),
    Cancelled,
}

#[async_trait]
pub trait PiInteractionResolver: Send + Sync {
    fn classify_tool_risk(&self, tool_name: &str, input: &Value) -> ApprovalRisk;

    async fn resolve_approval(&self, sid: &str, request: &ApprovalIR) -> PiApprovalDecision;

    async fn resolve_dialog(&self, sid: &str, request: &PiDialogRequest) -> PiDialogResponse;

    async fn cancel_sid(&self, _sid: &str) {}
}

pub fn dialog_prompt(token: String, request: &PiDialogRequest) -> ChoicePrompt {
    let options = match &request.kind {
        PiDialogKind::Select { options } => options
            .iter()
            .map(|value| crate::ChoiceOption {
                id: value.clone(),
                label: value.clone(),
            })
            .collect(),
        PiDialogKind::Confirm { .. } => vec![
            crate::ChoiceOption {
                id: "true".to_string(),
                label: "✅ Confirm".to_string(),
            },
            crate::ChoiceOption {
                id: "false".to_string(),
                label: "⛔ Cancel".to_string(),
            },
        ],
        PiDialogKind::Input { .. } | PiDialogKind::Editor { .. } => Vec::new(),
    };
    ChoicePrompt {
        token,
        title: request.title.clone(),
        options,
        multi: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_has_no_prompt_mutation_surface() {
        for forbidden in [
            "pi.on(\"before_agent_start\"",
            "pi.on(\"context\"",
            "systemPrompt",
            "promptGuidelines",
            "promptSnippet",
        ] {
            assert!(!bridge_source().contains(forbidden), "found {forbidden}");
        }
        for name in KNOWN_MCP_TOOL_NAMES {
            assert!(
                !bridge_source().contains(&format!("\"{name}\"")),
                "bridge must discover `{name}` from tools/list"
            );
        }
        assert!(!bridge_source().contains("\"properties\""));
    }

    #[test]
    fn ready_gate_accepts_any_subset_of_the_known_face_and_rejects_strangers() {
        let names = KNOWN_MCP_TOOL_NAMES
            .iter()
            .map(|name| format!("ccteam_{name}"))
            .collect::<Vec<_>>();
        validate_ready_names(&names).unwrap();
        // A per-caller face is a SUBSET — a depth-capped child gets one tool,
        // and a `tools:"none"` child gets none. Both are ready.
        validate_ready_names(&names[..1]).unwrap();
        validate_ready_names(&[]).unwrap();
        // A name ccteam does not ship is a wiring bug, not a slim face.
        assert!(validate_ready_names(&["ccteam_session_spawn".to_string()]).is_err());
        // Duplicates still fail.
        assert!(validate_ready_names(&[names[0].clone(), names[0].clone()]).is_err());
    }
}
