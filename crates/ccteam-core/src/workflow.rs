//! V0.4.0 F63 — `workflow.yaml` schema + parser.
//!
//! ## Scope
//!
//! Pure data layer: parse `workflow.yaml` into a typed [`WorkflowSpec`],
//! validate structural rules, expose role → [`AgentSpec`] map preserving
//! YAML declaration order (so logs / fixtures / trigger graph build are
//! deterministic). **No IO side effects** beyond reading the file in
//! [`WorkflowSpec::load`].
//!
//! ## Red lines (PRD v0-4-0 §F63)
//!
//! 1. `workflow.yaml` MUST NOT contain `prompt:`, `system_prompt:`, or
//!    `messages:` fields. Prompts live in `.claude/agents/<role>.md`.
//!    Any future PR adding a prompt field here is a schema violation.
//! 2. This module is **team-name agnostic**: no string literals like
//!    `"dev"`, `"qa"`, `"ccteam"` outside test fixtures.
//! 3. Parser writes nothing to `progress.jsonl`. Pure parse + validate.
//!
//! ## Trigger forms (YAML scalar string)
//!
//! - `manual`            — explicit `ccteam trigger <role>` invocation.
//! - `schedule`          — periodic; V0.4.0 stub (meta-agent triggers
//!                         manually); V0.4.1 will wire `interval` cron.
//! - `gate`              — wait until `trigger_gate` MCP tool releases.
//! - `watch:<path>`      — inotify on artifact dir; new file → spawn.
//!
//! ## See also
//!
//! - `docs/v0-4-0/prd.md` §6.1 (full schema spec)
//! - `docs/v0-4-0/dev-plan.md` §5 (PR #4 task list)
//! - `docs/interfaces.md` §17 (workflow.yaml schema reference)

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

/// Full `workflow.yaml` document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WorkflowSpec {
    /// Workflow identifier (unique within a project).
    pub name: String,
    /// Optional human-readable description for the meta-agent / UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// V0.4.6 F82 — soft toggle. `false` makes the daemon skip rostering
    /// this workflow (and tear down a running loop on hot-reload). The
    /// project's `state.json`, `progress.jsonl`, and artifact dirs stay
    /// intact; flipping back to `true` resumes from where the last loop
    /// left off. Defaults to `true` for backwards compatibility with
    /// V0.4.5 workflow.yaml files that omit the field.
    ///
    /// Skipped on serialize when `true` (the default) so round-trips
    /// don't add boilerplate to hand-authored workflow.yaml files —
    /// only the opt-out form `enabled: false` shows up in the
    /// rendered YAML.
    #[serde(
        default = "default_enabled",
        skip_serializing_if = "is_enabled_default"
    )]
    pub enabled: bool,
    /// Role → agent spec. `IndexMap` preserves YAML declaration order so
    /// trigger graph build is deterministic across runs.
    pub agents: IndexMap<String, AgentSpec>,
}

/// Default for `WorkflowSpec::enabled` when the YAML omits the field.
/// `true` matches V0.4.5 behaviour (no field = run the workflow).
fn default_enabled() -> bool {
    true
}

/// Predicate paired with `default_enabled` so `enabled: true` (the
/// default) is omitted from serialized YAML — only the opt-out form
/// `enabled: false` is rendered.
fn is_enabled_default(v: &bool) -> bool {
    *v
}

/// Per-agent role configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentSpec {
    /// Harness binary that runs this role. Defaults to `Claude` when the
    /// YAML field is omitted — Codex is opt-in.
    #[serde(default)]
    pub executor: Executor,
    /// What triggers a new session of this role. See [`Trigger`].
    pub trigger: Trigger,
    /// Max concurrent sessions of this role. Only meaningful for
    /// `Trigger::Watch`; validate() rejects `> 1` for other triggers.
    /// `None` semantics = "single instance" (caller treats `None` as 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    /// Optional input artifact dir (relative to project root). Passed to
    /// the spawned harness via `CCTEAM_INPUT` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<PathBuf>,
    /// Optional output artifact dir (relative to project root). Passed
    /// via `CCTEAM_OUTPUT` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    /// Schedule interval (e.g. `"5m"`, `"1h"`) when
    /// `trigger == Trigger::Schedule`. Ignored otherwise. V0.4.0 keeps
    /// this as opaque string; cron / duration parsing lands in V0.4.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Optional per-session timeout (duration string). V0.4.0 carries
    /// the field for later watchdog consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// What to do when `timeout` elapses. Default `Escalate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<OnTimeout>,
}

/// Harness binary executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    /// Anthropic Claude Code CLI (default).
    #[default]
    Claude,
    /// OpenAI Codex CLI. Requires `CodexAdapter` (V0.3.1 stub; V0.4.0
    /// real impl per PRD §6.5).
    Codex,
}

/// What to do when an agent session exceeds its `timeout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnTimeout {
    /// Push a watchdog alert to meta-agent outbox; do not kill.
    Escalate,
    /// Tear down session and respawn with same inputs.
    Retry,
    /// Mark session abandoned; do not respawn.
    Skip,
}

/// What event spawns a new session of an agent role.
///
/// YAML scalar grammar:
///
/// ```yaml
/// trigger: manual            # explicit ccteam trigger <role>
/// trigger: schedule          # periodic (interval field + V0.4.1 cron)
/// trigger: gate              # wait for trigger_gate MCP call
/// trigger: watch:.ccteam/issues/  # inotify on path
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Meta-agent / user explicitly invokes `ccteam trigger <role>`.
    Manual,
    /// Periodic. V0.4.0 = meta-agent-only manual trigger placeholder;
    /// V0.4.1 wires actual scheduler reading `AgentSpec::interval`.
    Schedule,
    /// Wait for `trigger_gate` MCP call to release. Requires `input`
    /// dir so the agent has artifacts to consume on release.
    Gate,
    /// Watch the given path for new files; each new file spawns one
    /// session (subject to `parallelism`).
    Watch(PathBuf),
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        parse_trigger(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Trigger {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let rendered = match self {
            Trigger::Manual => "manual".to_string(),
            Trigger::Schedule => "schedule".to_string(),
            Trigger::Gate => "gate".to_string(),
            Trigger::Watch(path) => format!("watch:{}", path.display()),
        };
        s.serialize_str(&rendered)
    }
}

fn parse_trigger(raw: &str) -> Result<Trigger, String> {
    let trimmed = raw.trim();
    match trimmed {
        "manual" => Ok(Trigger::Manual),
        "schedule" => Ok(Trigger::Schedule),
        "gate" => Ok(Trigger::Gate),
        other => {
            if let Some(rest) = other.strip_prefix("watch:") {
                // Note: empty path (`watch:`) parses successfully here;
                // WorkflowSpec::validate() rejects it with
                // ValidationFailed so the error surfaces at the
                // workflow-level rather than as a raw parse error.
                Ok(Trigger::Watch(PathBuf::from(rest)))
            } else {
                Err(format!(
                    "unknown trigger `{other}`: expected one of \
                     `manual`, `schedule`, `gate`, or `watch:<path>`"
                ))
            }
        }
    }
}

/// Errors surfaced by [`WorkflowSpec::load`] / [`WorkflowSpec::validate`].
#[derive(thiserror::Error, Debug)]
pub enum WorkflowError {
    /// Neither `<project>/.ccteam/workflow.yaml` (canonical) nor
    /// `<project>/workflow.yaml` (legacy V0.4.0–V0.4.5 fallback) exists.
    #[error("workflow.yaml not found in {0:?}")]
    NotFound(PathBuf),
    /// Filesystem read failure (permissions, EIO, etc).
    #[error("workflow.yaml read failed: {0}")]
    ReadFailed(#[from] std::io::Error),
    /// YAML syntax error or unknown enum variant.
    #[error("workflow.yaml parse failed: {0}")]
    ParseFailed(#[from] serde_yaml::Error),
    /// Semantic validation failure (empty watch path, gate without
    /// input, illegal role name, etc.).
    #[error("workflow validation failed: {0}")]
    ValidationFailed(String),
}

impl WorkflowSpec {
    /// Parse and validate a workflow.yaml at an explicit path.
    pub fn load(path: &Path) -> Result<Self, WorkflowError> {
        let body = std::fs::read_to_string(path)?;
        let spec: WorkflowSpec = serde_yaml::from_str(&body)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Discovery: probe `<project_dir>/.ccteam/workflow.yaml` first
    /// (canonical V0.4.6+ location), then fall back to
    /// `<project_dir>/workflow.yaml` (V0.4.0–V0.4.5 legacy; removed in
    /// V0.5). Returns [`WorkflowError::NotFound`] when neither exists.
    pub fn load_for_project(project_dir: &Path) -> Result<Self, WorkflowError> {
        let nested = project_dir.join(".ccteam").join("workflow.yaml");
        if nested.exists() {
            return Self::load(&nested);
        }
        let direct = project_dir.join("workflow.yaml");
        if direct.exists() {
            return Self::load(&direct);
        }
        Err(WorkflowError::NotFound(project_dir.to_path_buf()))
    }

    /// Apply structural validation (PRD §6.1):
    ///
    /// 1. `Trigger::Watch(path)` — path must be non-empty.
    /// 2. `Trigger::Gate` — `input` must be set (otherwise the gate has
    ///    nothing to hand off on release).
    /// 3. Agent role name only `[a-z0-9_-]`, length ≥ 1 (must map to a
    ///    valid `.claude/agents/<role>.md` filename).
    /// 4. `parallelism > 1` only allowed with `Trigger::Watch`;
    ///    `schedule` / `gate` / `manual` are single-instance.
    /// 5. `agents` map must be non-empty.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.agents.is_empty() {
            return Err(WorkflowError::ValidationFailed(
                "workflow must declare at least one agent".to_string(),
            ));
        }
        for (role, spec) in &self.agents {
            validate_role_name(role)?;
            match &spec.trigger {
                Trigger::Watch(path) => {
                    if path.as_os_str().is_empty() {
                        return Err(WorkflowError::ValidationFailed(format!(
                            "agent `{role}`: trigger `watch:` requires a non-empty path"
                        )));
                    }
                }
                Trigger::Gate => {
                    if spec.input.is_none() {
                        return Err(WorkflowError::ValidationFailed(format!(
                            "agent `{role}`: trigger `gate` requires an `input` directory"
                        )));
                    }
                }
                Trigger::Manual | Trigger::Schedule => {}
            }
            if let Some(n) = spec.parallelism {
                if n > 1 && !matches!(spec.trigger, Trigger::Watch(_)) {
                    return Err(WorkflowError::ValidationFailed(format!(
                        "agent `{role}`: parallelism > 1 only valid with `watch:` trigger \
                         (schedule / gate / manual are single-instance)"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_role_name(role: &str) -> Result<(), WorkflowError> {
    if role.is_empty() {
        return Err(WorkflowError::ValidationFailed(
            "agent role name must be non-empty".to_string(),
        ));
    }
    for ch in role.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-';
        if !ok {
            return Err(WorkflowError::ValidationFailed(format!(
                "agent role `{role}`: character `{ch}` not allowed \
                 (only [a-z0-9_-] permitted)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trigger_manual() {
        assert_eq!(parse_trigger("manual").unwrap(), Trigger::Manual);
    }

    #[test]
    fn parse_trigger_schedule() {
        assert_eq!(parse_trigger("schedule").unwrap(), Trigger::Schedule);
    }

    #[test]
    fn parse_trigger_gate() {
        assert_eq!(parse_trigger("gate").unwrap(), Trigger::Gate);
    }

    #[test]
    fn parse_trigger_watch_with_path() {
        assert_eq!(
            parse_trigger("watch:.ccteam/issues/").unwrap(),
            Trigger::Watch(PathBuf::from(".ccteam/issues/"))
        );
    }

    #[test]
    fn parse_trigger_watch_empty_is_parsed_then_caught_by_validate() {
        // parse succeeds (empty PathBuf)…
        let parsed = parse_trigger("watch:").unwrap();
        assert_eq!(parsed, Trigger::Watch(PathBuf::new()));
        // …but validate() rejects it.
        let spec = WorkflowSpec {
            name: "x".into(),
            description: None,
            enabled: true,
            agents: {
                let mut m = IndexMap::new();
                m.insert(
                    "explorer".into(),
                    AgentSpec {
                        executor: Executor::Claude,
                        trigger: parsed,
                        parallelism: None,
                        input: None,
                        output: None,
                        interval: None,
                        timeout: None,
                        on_timeout: None,
                    },
                );
                m
            },
        };
        let err = spec.validate().unwrap_err();
        assert!(matches!(err, WorkflowError::ValidationFailed(_)));
    }

    #[test]
    fn parse_trigger_unknown_form_errors() {
        assert!(parse_trigger("foo").is_err());
        assert!(parse_trigger("cron:5m").is_err());
    }

    #[test]
    fn validate_role_name_accepts_kebab_snake_digits() {
        assert!(validate_role_name("explorer").is_ok());
        assert!(validate_role_name("fix-bot").is_ok());
        assert!(validate_role_name("agent_2").is_ok());
        assert!(validate_role_name("a").is_ok());
    }

    #[test]
    fn validate_role_name_rejects_upper_space_punct() {
        assert!(validate_role_name("Explorer").is_err());
        assert!(validate_role_name("my agent").is_err());
        assert!(validate_role_name("agent.x").is_err());
        assert!(validate_role_name("").is_err());
    }
}
