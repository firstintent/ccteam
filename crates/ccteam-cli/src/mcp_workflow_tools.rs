//! V0.4.0 F65 — Meta-agent MCP workflow tools.
//!
//! Adds 7 new MCP tools on top of the M2.5 surface so the meta-agent
//! (the long-lived CC session a user attaches to) can drive a v0.4.0
//! workflow project end-to-end through natural language:
//!
//! - `ccteam__workflow_spawn_agent` — immediately dispatch one agent session
//! - `ccteam__workflow_stop_agent` — soft-stop a running session (file marker)
//! - `ccteam__workflow_observe_agents` — list running sessions + status
//! - `ccteam__workflow_signal` — pause / resume / btw / interrupt a session
//! - `ccteam__workflow_set_parallelism` — hot-tune per-role parallelism cap
//! - `ccteam__workflow_trigger_gate` — manually release a gate trigger
//! - `ccteam__workflow_get_artifact_summary` — count + latest per artifact dir
//!
//! ## Red lines (PRD v0-4-0 §F65)
//!
//! 1. The handlers are **thin file-system writers**: they never spawn
//!    processes directly. Every "action" is recorded as a marker file
//!    under `<project_dir>/.ccteam/<bucket>/` and the F66 thin
//!    orchestrator daemon picks it up on its next tick.
//! 2. **No persistent watch loops** in this module. `observe_agents`
//!    is a one-shot read; the meta-agent polls via repeated calls.
//! 3. `crates/ccteam-core` is not touched by this PR. Schemas reference
//!    [`ccteam_flow::WorkflowSpec`] read-only.
//!
//! ## F66 integration hooks
//!
//! Each handler that writes a marker file documents the consumer path
//! the F66 orchestrator is expected to wire (see inline `// F66:`
//! comments). Until F66 lands, every handler returns a deterministic
//! JSON envelope so the meta-agent can confirm the request landed.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Map, Value};

use ccteam_core::{actions, CcteamPaths, SendOptions};
use ccteam_flow::WorkflowSpec;

/// Helper: locate `<project_dir>/.ccteam/` and verify the project
/// exists. Mirrors `actions::send_to_session_with` shape so error
/// messages stay consistent ("no project / session named ..." vs
/// "missing required argument ...").
fn ensure_project(paths: &CcteamPaths, slug: &str) -> Result<std::path::PathBuf> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Err(anyhow!(
            "no project named `{}` (looked under {})",
            slug,
            project_dir.display(),
        ));
    }
    Ok(project_dir)
}

/// Load `workflow.yaml` and check the role exists. Returns the parsed
/// spec for callers that need to read further (`get_artifact_summary`
/// iterates `agents`; `spawn_agent` validates the role).
fn load_workflow_for(project_dir: &Path) -> Result<WorkflowSpec> {
    WorkflowSpec::load_for_project(project_dir)
        .with_context(|| format!("load workflow.yaml under {}", project_dir.display()))
}

fn validate_role_in_workflow(spec: &WorkflowSpec, role: &str) -> Result<()> {
    if !spec.agents.contains_key(role) {
        let mut available: Vec<&str> = spec.agents.keys().map(|s| s.as_str()).collect();
        available.sort();
        return Err(anyhow!(
            "role `{}` not declared in workflow.yaml (available roles: {})",
            role,
            available.join(", "),
        ));
    }
    Ok(())
}

/// Compact monotonic ID. Used for pending-session IDs and marker
/// filenames; the wall-clock millisecond + suffix is enough to keep
/// concurrent calls within the same second from colliding.
fn fresh_id(role: &str) -> String {
    let ts = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
    format!("{role}-{ts}")
}

fn arg_string(args: &Value, name: &str) -> Result<String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing required argument `{name}`"))
}

fn opt_string(args: &Value, name: &str) -> Option<String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// =============== Tool implementations ===============

/// `ccteam__workflow_spawn_agent`: enqueue a spawn request for role `role` in
/// `slug`. Validates the role exists in `workflow.yaml`; writes a
/// pending-spawn marker to `.ccteam/spawn_requests/<role>-<ts>.json`
/// so F66's tick picks it up.
///
/// F66 integration: orchestrator reads `.ccteam/spawn_requests/`,
/// spawns a HarnessAdapter session per file, deletes the marker on
/// successful spawn. Until F66 lands the marker accumulates; the
/// meta-agent can read it back via `observe_agents` and `ls`.
pub fn tool_spawn_agent(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let role = arg_string(args, "role")?;
    let project_dir = ensure_project(paths, &slug)?;
    let spec = load_workflow_for(&project_dir)?;
    validate_role_in_workflow(&spec, &role)?;

    let session_id = fresh_id(&role);
    let bucket = project_dir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket).with_context(|| format!("create {}", bucket.display()))?;
    let marker = bucket.join(format!("{session_id}.json"));

    // Carry through optional overrides verbatim — F66 will read whatever
    // shape this PR records.
    let overrides = args.get("overrides").cloned().unwrap_or(json!({}));
    let payload = json!({
        "session_id": session_id,
        "role": role,
        "requested_at": Utc::now().to_rfc3339(),
        "overrides": overrides,
    });
    std::fs::write(&marker, serde_json::to_string_pretty(&payload)?)
        .with_context(|| format!("write {}", marker.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "role": role,
        "session_id": session_id,
        "marker": marker.to_string_lossy(),
        "note": "F66 orchestrator consumes .ccteam/spawn_requests/ on next tick",
    }))?)
}

/// `ccteam__workflow_stop_agent`: write a soft-stop marker. F66's orchestrator
/// reads `.ccteam/stop_signal/<role>_<sid>` and tears down the
/// matching session (SessionHandle::shutdown). `session_id == None`
/// means "stop all sessions of this role" — the marker filename uses
/// `__all__` as the sid placeholder.
///
/// F66 integration: each tick scans `.ccteam/stop_signal/`, dispatches
/// shutdown per marker, deletes on success.
pub fn tool_stop_agent(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let role = arg_string(args, "role")?;
    let session_id = opt_string(args, "session_id");
    let project_dir = ensure_project(paths, &slug)?;

    let bucket = project_dir.join(".ccteam").join("stop_signal");
    std::fs::create_dir_all(&bucket).with_context(|| format!("create {}", bucket.display()))?;

    let sid_for_filename = session_id.clone().unwrap_or_else(|| "__all__".into());
    let marker = bucket.join(format!("{role}_{sid_for_filename}"));
    let body = json!({
        "role": role,
        "session_id": session_id,
        "requested_at": Utc::now().to_rfc3339(),
    });
    std::fs::write(&marker, serde_json::to_string_pretty(&body)?)
        .with_context(|| format!("write {}", marker.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "role": role,
        "session_id": session_id,
        "marker": marker.to_string_lossy(),
        "note": "F66 orchestrator drains .ccteam/stop_signal/ on next tick",
    }))?)
}

/// `ccteam__workflow_observe_agents`: one-shot read of the project's current
/// agent sessions. Source of truth is `state.json::sessions` (V0.3.1
/// F49 registry); per-harness state files provide cost / status when
/// available. Returns empty `agents` array when no sessions registered.
///
/// F66 integration: F66 grows `state.json::sessions` entries with
/// per-session role + status. This handler reads whatever is present;
/// nothing breaks if F66 hasn't extended the shape yet.
pub fn tool_observe_agents(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let _project_dir = ensure_project(paths, &slug)?;

    let state_path = paths.project_state(&slug);
    let mut agents: Vec<Value> = Vec::new();
    if state_path.exists() {
        let body = std::fs::read_to_string(&state_path)
            .with_context(|| format!("read {}", state_path.display()))?;
        if let Ok(state) = serde_json::from_str::<Value>(&body) {
            if let Some(sessions) = state.get("sessions").and_then(|v| v.as_object()) {
                for (sid, record) in sessions {
                    let harness = record
                        .get("harness")
                        .and_then(|v| v.as_str())
                        .unwrap_or("claude")
                        .to_string();
                    let tmux_session = record
                        .get("tmux_session")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let started_at = record
                        .get("started_at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let pid = record.get("pid").cloned().unwrap_or(Value::Null);
                    // Role is not yet recorded in V0.3.1 SessionRecord;
                    // F66 will add a `role` field. Until then surface
                    // the sid (which encodes role-<seq> per
                    // harness_sid_prefix) and let the meta-agent parse.
                    let role = record.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    agents.push(json!({
                        "session_id": sid,
                        "role": role,
                        "harness": harness,
                        "tmux_session": tmux_session,
                        "started_at": started_at,
                        "pid": pid,
                        // F66 will populate `status` / `cost_usd` by
                        // reading the per-session state file.
                        "status": "unknown",
                    }));
                }
            }
        }
    }

    Ok(serde_json::to_string_pretty(&json!({
        "slug": slug,
        "agents": agents,
    }))?)
}

/// `ccteam__workflow_signal`: send a control signal to a running agent.
///
/// - `pause` / `resume` / `interrupt` write a marker under
///   `.ccteam/signal/<role>_<sid>`; F66 reads + applies SIGSTOP /
///   SIGCONT / SIGINT to the harness pid.
/// - `btw` reuses the inbox path (`actions::send_to_session_with`)
///   so the existing inotify + send-keys delivery picks it up.
pub fn tool_signal(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let role = arg_string(args, "role")?;
    let signal = arg_string(args, "signal")?;
    let session_id = opt_string(args, "session_id");
    let message = opt_string(args, "message");
    let project_dir = ensure_project(paths, &slug)?;

    let signal_norm = signal.to_lowercase();
    match signal_norm.as_str() {
        "pause" | "resume" | "interrupt" => {
            let bucket = project_dir.join(".ccteam").join("signal");
            std::fs::create_dir_all(&bucket)
                .with_context(|| format!("create {}", bucket.display()))?;
            let sid_for_filename = session_id.clone().unwrap_or_else(|| "__all__".into());
            let marker = bucket.join(format!("{role}_{sid_for_filename}"));
            let body = json!({
                "role": role,
                "session_id": session_id,
                "signal": signal_norm,
                "requested_at": Utc::now().to_rfc3339(),
            });
            std::fs::write(&marker, serde_json::to_string_pretty(&body)?)
                .with_context(|| format!("write {}", marker.display()))?;
            Ok(serde_json::to_string_pretty(&json!({
                "ok": true,
                "slug": slug,
                "role": role,
                "session_id": session_id,
                "signal": signal_norm,
                "marker": marker.to_string_lossy(),
                "note": "F66 orchestrator applies SIGSTOP/SIGCONT/SIGINT to harness pid",
            }))?)
        }
        "btw" => {
            let body = message.ok_or_else(|| anyhow!("signal=btw requires `message` argument"))?;
            // For `btw` we want the message to flow through the same
            // inbox path that send_to_session uses so idle-aware
            // injection picks it up. Tag the source so retro /
            // silence-classifier can distinguish.
            let opts = SendOptions {
                source: "ccteam-mcp".into(),
                source_user: format!("meta-agent:signal:{role}"),
                content_type: "text".into(),
            };
            let formatted = format!(
                "**META-AGENT BTW for `{role}`{}**:\n\n{body}",
                session_id
                    .as_deref()
                    .map(|s| format!(" (session `{s}`)"))
                    .unwrap_or_default(),
            );
            let result = actions::send_to_session_with(paths, &slug, &formatted, &opts)?;
            Ok(serde_json::to_string_pretty(&json!({
                "ok": true,
                "slug": slug,
                "role": role,
                "session_id": session_id,
                "signal": "btw",
                "inbox_file": result.inbox_file,
            }))?)
        }
        other => Err(anyhow!(
            "unknown signal `{other}`; valid: pause | resume | btw | interrupt",
        )),
    }
}

/// `ccteam__workflow_set_parallelism`: hot-tune the per-role parallelism cap.
/// Writes a merge-friendly `.ccteam/workflow_overrides.json` with
/// shape `{"<role>": {"parallelism": N}}`. F66 reloads this file each
/// tick.
pub fn tool_set_parallelism(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let role = arg_string(args, "role")?;
    let parallelism = args
        .get("parallelism")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing required argument `parallelism` (1-50)"))?;
    if !(1..=50).contains(&parallelism) {
        return Err(anyhow!(
            "parallelism must be between 1 and 50 inclusive (got {parallelism})",
        ));
    }
    let project_dir = ensure_project(paths, &slug)?;
    let ccteam_dir = project_dir.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;
    let overrides_path = ccteam_dir.join("workflow_overrides.json");

    // Merge into any existing override file so successive calls for
    // different roles don't clobber one another.
    let mut root: Map<String, Value> = if overrides_path.exists() {
        let body = std::fs::read_to_string(&overrides_path)
            .with_context(|| format!("read {}", overrides_path.display()))?;
        match serde_json::from_str::<Value>(&body) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    } else {
        Map::new()
    };

    let role_entry = root
        .entry(role.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(m) = role_entry {
        m.insert("parallelism".into(), json!(parallelism));
    } else {
        *role_entry = json!({ "parallelism": parallelism });
    }
    let pretty = serde_json::to_string_pretty(&Value::Object(root))?;

    // Atomic write so a concurrent F66 reader never sees a torn file.
    let mut tmp = overrides_path.clone();
    tmp.set_extension("json.tmp");
    std::fs::write(&tmp, pretty.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &overrides_path)
        .with_context(|| format!("rename {} → {}", tmp.display(), overrides_path.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "role": role,
        "parallelism": parallelism,
        "overrides_file": overrides_path.to_string_lossy(),
    }))?)
}

/// `ccteam__workflow_trigger_gate`: write a release marker for the named gate
/// role. F66 reads `.ccteam/gate_override/<role>` next tick and spawns
/// the gate-trigger agent regardless of input artifact state when
/// `force == true`.
pub fn tool_trigger_gate(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let role = arg_string(args, "role")?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let project_dir = ensure_project(paths, &slug)?;

    let bucket = project_dir.join(".ccteam").join("gate_override");
    std::fs::create_dir_all(&bucket).with_context(|| format!("create {}", bucket.display()))?;
    let marker = bucket.join(&role);

    let body = json!({
        "role": role,
        "force": force,
        "requested_at": Utc::now().to_rfc3339(),
    });
    std::fs::write(&marker, serde_json::to_string_pretty(&body)?)
        .with_context(|| format!("write {}", marker.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "role": role,
        "force": force,
        "marker": marker.to_string_lossy(),
        "note": "F66 orchestrator releases gate on next tick",
    }))?)
}

/// `ccteam__workflow_get_artifact_summary`: stat-only summary of each artifact
/// directory declared in `workflow.yaml`. Returns `{artifacts: {}}`
/// when no agent declares an `input`/`output` dir.
///
/// Per-dir entries: `{count, latest, latest_mtime, size_bytes}`. Only
/// regular files at top level are counted (no recursive descent).
/// O(n) on inodes; never opens file contents.
pub fn tool_get_artifact_summary(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let project_dir = ensure_project(paths, &slug)?;

    // Collect declared dirs across all agents, deduped, preserving
    // YAML order for deterministic output.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(spec) = load_workflow_for(&project_dir) {
        for agent in spec.agents.values() {
            for rel in [&agent.input, &agent.output].into_iter().flatten() {
                if !dirs.contains(rel) {
                    dirs.push(rel.clone());
                }
            }
        }
    }

    let mut artifacts = Map::new();
    for rel in dirs {
        let abs = if rel.is_absolute() {
            rel.clone()
        } else {
            project_dir.join(&rel)
        };
        // Skip if the dir hasn't been created yet — F66 lazy-creates on
        // first spawn. Surface as `{count: 0, latest: null}` so the
        // meta-agent gets a uniform shape.
        if !abs.exists() {
            artifacts.insert(
                rel.display().to_string(),
                json!({
                    "count": 0,
                    "latest": Value::Null,
                    "latest_mtime": Value::Null,
                    "size_bytes": 0u64,
                    "exists": false,
                }),
            );
            continue;
        }
        let (count, latest_name, latest_mtime, size_bytes) = scan_dir(&abs)?;
        artifacts.insert(
            rel.display().to_string(),
            json!({
                "count": count,
                "latest": latest_name,
                "latest_mtime": latest_mtime,
                "size_bytes": size_bytes,
                "exists": true,
            }),
        );
    }

    Ok(serde_json::to_string_pretty(&json!({
        "slug": slug,
        "artifacts": Value::Object(artifacts),
    }))?)
}

/// Returns `(count, latest_filename, latest_mtime_rfc3339, total_size_bytes)`
/// for regular files at the top level of `dir`. Errors are propagated
/// so a permissions issue surfaces as a tools/call isError rather than
/// silent 0 counts.
fn scan_dir(dir: &Path) -> Result<(usize, Option<String>, Option<String>, u64)> {
    let mut count = 0usize;
    let mut size_bytes = 0u64;
    let mut latest: Option<(std::time::SystemTime, String)> = None;
    let read = std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?;
    for entry in read {
        let entry = entry?;
        let ft = entry.file_type()?;
        if !ft.is_file() {
            continue;
        }
        let meta = entry.metadata()?;
        count += 1;
        size_bytes += meta.len();
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let name = entry.file_name().to_string_lossy().into_owned();
        match &latest {
            Some((t, _)) if *t >= mtime => {}
            _ => latest = Some((mtime, name)),
        }
    }
    let (latest_name, latest_mtime) = match latest {
        Some((t, name)) => {
            let dt: chrono::DateTime<Utc> = t.into();
            (Some(name), Some(dt.to_rfc3339()))
        }
        None => (None, None),
    };
    Ok((count, latest_name, latest_mtime, size_bytes))
}

/// Tool definitions for the 7 F65 workflow tools. Merged into the
/// top-level `tool_definitions()` in `mcp_serve.rs`.
pub fn workflow_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__workflow_spawn_agent",
            "description": "V0.4.0 F65: enqueue a spawn request for a named agent role in a workflow project. Writes a marker under <project>/.ccteam/spawn_requests/ that the F66 orchestrator picks up on its next tick. Returns {ok, session_id, marker}. Validates role exists in workflow.yaml; errors when role is unknown.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "role": { "type": "string", "description": "Agent role name (must match workflow.yaml agents key)." },
                    "overrides": {
                        "type": "object",
                        "description": "Optional spawn overrides (carried verbatim to F66 dispatcher)."
                    }
                },
                "required": ["slug", "role"],
            }),
        }),
        json!({
            "name": "ccteam__workflow_stop_agent",
            "description": "V0.4.0 F65: soft-stop one or all running sessions of a role. Writes <project>/.ccteam/stop_signal/<role>_<sid>; F66 reads + tears down the matching session. session_id omitted = stop all sessions of this role.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "role": { "type": "string", "description": "Agent role to stop." },
                    "session_id": {
                        "type": "string",
                        "description": "Optional specific session id; omit to stop every session of this role."
                    }
                },
                "required": ["slug", "role"],
            }),
        }),
        json!({
            "name": "ccteam__workflow_observe_agents",
            "description": "V0.4.0 F65: one-shot snapshot of the project's currently registered agent sessions. Reads state.json::sessions (V0.3.1 F49 registry). Returns {agents: [{session_id, role, harness, tmux_session, started_at, pid, status}]}. Empty array when no sessions registered.",
            "inputSchema": object_schema_one("slug", "Project slug."),
        }),
        json!({
            "name": "ccteam__workflow_signal",
            "description": "V0.4.0 F65: send a control signal to a running agent. Signals: pause|resume|interrupt write marker files under .ccteam/signal/ for F66 to apply SIGSTOP/SIGCONT/SIGINT to the harness pid; btw routes through the inbox so idle-aware injection delivers the message.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "role": { "type": "string", "description": "Agent role." },
                    "session_id": {
                        "type": "string",
                        "description": "Optional specific session id; omit to target every session of this role."
                    },
                    "signal": {
                        "type": "string",
                        "enum": ["pause", "resume", "btw", "interrupt"],
                        "description": "Signal kind. `btw` requires `message`."
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional NL/markdown body; required when signal=btw."
                    }
                },
                "required": ["slug", "role", "signal"],
            }),
        }),
        json!({
            "name": "ccteam__workflow_set_parallelism",
            "description": "V0.4.0 F65: hot-tune a workflow agent role's parallelism cap. Atomically rewrites <project>/.ccteam/workflow_overrides.json; F66 reloads the file each tick. Range 1-50.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "role": { "type": "string", "description": "Agent role to retune." },
                    "parallelism": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "description": "New parallelism cap (1-50)."
                    }
                },
                "required": ["slug", "role", "parallelism"],
            }),
        }),
        json!({
            "name": "ccteam__workflow_trigger_gate",
            "description": "V0.4.0 F65: manually release a gate-triggered agent role. Writes <project>/.ccteam/gate_override/<role>; F66 spawns the gate agent on the next tick. `force=true` instructs F66 to skip its automatic input-satisfaction check.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "role": { "type": "string", "description": "Gate-triggered agent role to release." },
                    "force": {
                        "type": "boolean",
                        "description": "Skip the automatic input satisfaction check (default false)."
                    }
                },
                "required": ["slug", "role"],
            }),
        }),
        json!({
            "name": "ccteam__workflow_get_artifact_summary",
            "description": "V0.4.0 F65: stat-only summary of each artifact directory declared in workflow.yaml. Returns {artifacts: {<dir>: {count, latest, latest_mtime, size_bytes, exists}}}. Skips file contents (O(n) on inodes).",
            "inputSchema": object_schema_one("slug", "Project slug."),
        }),
    ]
}

fn object_schema_one(name: &str, desc: &str) -> Value {
    let mut p = Map::new();
    p.insert(
        name.into(),
        json!({ "type": "string", "description": desc }),
    );
    json!({
        "type": "object",
        "properties": Value::Object(p),
        "required": [name],
    })
}

/// Dispatch one of the F65 tools by name. Returns `Ok(None)` if the
/// tool is not one of the F65 set, so the caller can fall through to
/// the legacy M2.5 dispatch table.
pub fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    let body = match name {
        "ccteam__workflow_spawn_agent" => tool_spawn_agent(paths, args)?,
        "ccteam__workflow_stop_agent" => tool_stop_agent(paths, args)?,
        "ccteam__workflow_observe_agents" => tool_observe_agents(paths, args)?,
        "ccteam__workflow_signal" => tool_signal(paths, args)?,
        "ccteam__workflow_set_parallelism" => tool_set_parallelism(paths, args)?,
        "ccteam__workflow_trigger_gate" => tool_trigger_gate(paths, args)?,
        "ccteam__workflow_get_artifact_summary" => tool_get_artifact_summary(paths, args)?,
        _ => return Ok(None),
    };
    Ok(Some(body))
}

/// Names of tools that require a live daemon heartbeat (state-mutating
/// tools where a dead daemon means F66 will never pick up the marker).
/// Read-only tools (`observe_agents`, `get_artifact_summary`) stay
/// daemon-independent so the meta-agent can inspect a stopped project.
pub fn requires_daemon(name: &str) -> bool {
    matches!(
        name,
        "ccteam__workflow_spawn_agent"
            | "ccteam__workflow_stop_agent"
            | "ccteam__workflow_signal"
            | "ccteam__workflow_set_parallelism"
            | "ccteam__workflow_trigger_gate"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_paths() -> (tempfile::TempDir, CcteamPaths) {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.projects_root).unwrap();
        (tmp, paths)
    }

    fn write_workflow_yaml(project_dir: &Path, body: &str) {
        let dir = project_dir.join(".ccteam");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workflow.yaml"), body).unwrap();
    }

    fn ensure_project_skeleton(paths: &CcteamPaths, slug: &str) {
        let dir = paths.project_dir(slug);
        std::fs::create_dir_all(dir.join(".ccteam")).unwrap();
    }

    const DEMO_WF: &str = r#"
name: demo
agents:
  fixer:
    executor: claude
    trigger: watch:.ccteam/issues/
    parallelism: 3
    input: .ccteam/issues/
    output: .ccteam/fixes/
  shipper:
    executor: claude
    trigger: gate
    input: .ccteam/fixes/
"#;

    #[test]
    fn workflow_tool_definitions_returns_seven_object_schemas() {
        let defs = workflow_tool_definitions();
        assert_eq!(defs.len(), 7);
        for tool in &defs {
            assert!(tool["name"].as_str().unwrap().starts_with("ccteam__"));
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        let names: std::collections::BTreeSet<&str> =
            defs.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for required in [
            "ccteam__workflow_spawn_agent",
            "ccteam__workflow_stop_agent",
            "ccteam__workflow_observe_agents",
            "ccteam__workflow_signal",
            "ccteam__workflow_set_parallelism",
            "ccteam__workflow_trigger_gate",
            "ccteam__workflow_get_artifact_summary",
        ] {
            assert!(names.contains(required), "missing tool {required}");
        }
    }

    #[test]
    fn spawn_agent_writes_marker_for_known_role() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        write_workflow_yaml(&paths.project_dir(slug), DEMO_WF);
        let body = tool_spawn_agent(&paths, &json!({ "slug": slug, "role": "fixer" })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        let sid = v["session_id"].as_str().unwrap();
        assert!(sid.starts_with("fixer-"));
        let bucket = paths
            .project_dir(slug)
            .join(".ccteam")
            .join("spawn_requests");
        let count = std::fs::read_dir(&bucket).unwrap().count();
        assert_eq!(count, 1, "expected one spawn marker, got {count}");
    }

    #[test]
    fn spawn_agent_rejects_unknown_role() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        write_workflow_yaml(&paths.project_dir(slug), DEMO_WF);
        let err = tool_spawn_agent(&paths, &json!({ "slug": slug, "role": "nope" })).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nope"),
            "expected mention of bad role, got: {msg}"
        );
        // Available roles listed
        assert!(msg.contains("fixer"));
    }

    #[test]
    fn stop_agent_writes_role_only_marker_when_sid_omitted() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let body = tool_stop_agent(&paths, &json!({ "slug": slug, "role": "fixer" })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        let marker = paths
            .project_dir(slug)
            .join(".ccteam")
            .join("stop_signal")
            .join("fixer___all__");
        assert!(marker.exists(), "expected marker at {}", marker.display());
    }

    #[test]
    fn observe_agents_returns_empty_array_with_no_sessions() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let body = tool_observe_agents(&paths, &json!({ "slug": slug })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["agents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn set_parallelism_writes_override_file_and_merges_roles() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let _ = tool_set_parallelism(
            &paths,
            &json!({ "slug": slug, "role": "fixer", "parallelism": 5 }),
        )
        .unwrap();
        let _ = tool_set_parallelism(
            &paths,
            &json!({ "slug": slug, "role": "shipper", "parallelism": 2 }),
        )
        .unwrap();
        let f = paths
            .project_dir(slug)
            .join(".ccteam")
            .join("workflow_overrides.json");
        assert!(f.exists());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["fixer"]["parallelism"], 5);
        assert_eq!(v["shipper"]["parallelism"], 2);
    }

    #[test]
    fn set_parallelism_rejects_out_of_range_value() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let err = tool_set_parallelism(
            &paths,
            &json!({ "slug": slug, "role": "fixer", "parallelism": 9001 }),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("1 and 50"));
    }

    #[test]
    fn trigger_gate_writes_marker_with_force_flag() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let _ = tool_trigger_gate(
            &paths,
            &json!({ "slug": slug, "role": "shipper", "force": true }),
        )
        .unwrap();
        let marker = paths
            .project_dir(slug)
            .join(".ccteam")
            .join("gate_override")
            .join("shipper");
        assert!(marker.exists());
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(body["force"], true);
    }

    #[test]
    fn get_artifact_summary_empty_dirs_yield_zero_counts() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        write_workflow_yaml(&paths.project_dir(slug), DEMO_WF);
        let body = tool_get_artifact_summary(&paths, &json!({ "slug": slug })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        // Two distinct dirs declared: .ccteam/issues/ and .ccteam/fixes/
        // (shipper's input duplicates fixer's output, so dedup yields 2).
        let artifacts = v["artifacts"].as_object().unwrap();
        assert!(artifacts.contains_key(".ccteam/issues/"));
        assert!(artifacts.contains_key(".ccteam/fixes/"));
        for (_, summary) in artifacts {
            assert_eq!(summary["count"], 0);
            assert_eq!(summary["exists"], false);
        }
    }

    #[test]
    fn get_artifact_summary_counts_top_level_files() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        write_workflow_yaml(&paths.project_dir(slug), DEMO_WF);
        let issues = paths.project_dir(slug).join(".ccteam").join("issues");
        std::fs::create_dir_all(&issues).unwrap();
        std::fs::write(issues.join("a.md"), b"hello").unwrap();
        std::fs::write(issues.join("b.md"), b"world!!!").unwrap();
        let body = tool_get_artifact_summary(&paths, &json!({ "slug": slug })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let issues_summary = &v["artifacts"][".ccteam/issues/"];
        assert_eq!(issues_summary["count"], 2);
        assert_eq!(issues_summary["exists"], true);
        // total bytes = "hello".len() + "world!!!".len() = 5 + 8 = 13
        assert_eq!(issues_summary["size_bytes"], 13);
    }

    #[test]
    fn get_artifact_summary_no_workflow_returns_empty_artifacts() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        // No workflow.yaml written
        let body = tool_get_artifact_summary(&paths, &json!({ "slug": slug })).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["artifacts"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn signal_btw_routes_through_inbox_when_message_provided() {
        ccteam_core::disable_tool_surface_bootstrap_for_tests();
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        // signal=btw uses send_to_session under the hood; that requires
        // the project to be bootstrapped (so .ccteam/inbox/ resolves).
        ccteam_core::bootstrap_project(&paths, slug, "demo request", "dev").unwrap();
        let body = tool_signal(
            &paths,
            &json!({
                "slug": slug,
                "role": "fixer",
                "signal": "btw",
                "message": "hello there",
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        let inbox = paths.project_ccteam_dir(slug).join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let body = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
        assert!(body.contains("hello there"));
        assert!(body.contains("fixer"));
    }

    #[test]
    fn signal_pause_writes_marker_file() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let body = tool_signal(
            &paths,
            &json!({
                "slug": slug,
                "role": "fixer",
                "signal": "pause",
                "session_id": "fixer-abc",
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        let marker = paths
            .project_dir(slug)
            .join(".ccteam")
            .join("signal")
            .join("fixer_fixer-abc");
        assert!(marker.exists());
    }

    #[test]
    fn signal_btw_without_message_errors() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let err = tool_signal(
            &paths,
            &json!({ "slug": slug, "role": "fixer", "signal": "btw" }),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("requires `message`"));
    }

    #[test]
    fn signal_unknown_kind_errors() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        let err = tool_signal(
            &paths,
            &json!({ "slug": slug, "role": "fixer", "signal": "fly" }),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown signal"));
    }

    #[test]
    fn requires_daemon_only_for_mutating_tools() {
        for t in [
            "ccteam__workflow_spawn_agent",
            "ccteam__workflow_stop_agent",
            "ccteam__workflow_signal",
            "ccteam__workflow_set_parallelism",
            "ccteam__workflow_trigger_gate",
        ] {
            assert!(requires_daemon(t), "{t} should require daemon");
        }
        for t in [
            "ccteam__workflow_observe_agents",
            "ccteam__workflow_get_artifact_summary",
            "ccteam__admin_ls",
        ] {
            assert!(!requires_daemon(t), "{t} should not require daemon");
        }
    }

    // Smoke: dispatch table routes a known tool, returns None for an
    // unknown tool so the legacy table can claim it.
    #[test]
    fn dispatch_returns_some_for_workflow_tool_and_none_otherwise() {
        let (_tmp, paths) = tmp_paths();
        let slug = "demo";
        ensure_project_skeleton(&paths, slug);
        write_workflow_yaml(&paths.project_dir(slug), DEMO_WF);
        let out = dispatch(
            &paths,
            "ccteam__workflow_get_artifact_summary",
            &json!({ "slug": slug }),
        )
        .unwrap();
        assert!(out.is_some());
        let none = dispatch(&paths, "ccteam__admin_ls", &json!({})).unwrap();
        assert!(none.is_none());
    }
}
