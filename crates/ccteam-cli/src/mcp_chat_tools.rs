//! `ccteam__chat_*` MCP tools.
//!
//! V0.6.5 F146 turned `chat_register_bot` / `chat_unregister_bot` /
//! `chat_list_bots` from V0.6.0 Wave 1 STUBs into real implementations
//! and **removed** the old `chat_lifecycle` multi-action stub (no
//! deprecated alias — V0.6.5 dev-plan §F146; CLAUDE.md §五 #4 forbids
//! backwards-compat shims pre-v1.0).
//!
//! The remaining 3 stubs (`chat_send_input`, `chat_session_reset`,
//! `chat_show_turn_log`) still return `NotImplemented` until F147
//! lands. The schema names + arg shapes here are the wire contract;
//! `docs/interfaces.md` §MCP table mirrors them.
//!
//! Architecture:
//!
//! - The 3 lifecycle / list tools wrap `ccteam_imd::{register_bot_checked_in,
//!   unregister_bot_in, list_bots_in}` (file-system control plane:
//!   registry JSON under `<ccteam_root>/imd/registry/<slug>/<role>.json`).
//! - `running` status comes from a sidecar heartbeat file the daemon's
//!   per-bot supervisor touches every 5s; an MCP process (separate
//!   from the daemon) reads mtime to infer "alive within 30s".
//! - Vendor is normalized to lowercase (`"claude"` / `"codex"`) before
//!   serde-deserializing into `AgentVendor` — the daemon's
//!   `BotRegistration` deserialize trips on `"Claude"` (Bug A from
//!   the NAS deploy session).

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::harness::AgentVendor;
use ccteam_core::paths::CcteamPaths;
use ccteam_imd::{
    bot_running_status_in, last_turn_at, list_bots_in, register_bot_checked_in, unregister_bot_in,
    RegisterOutcome,
};

/// Tool definitions for the chat group: 3 real + 3 stubs (total 6).
/// Merged into the top-level `tool_definitions()` in `mcp_serve.rs`.
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__chat_register_bot",
            "description": "V0.6.5 F146 — register a chat-mode bot. Writes `<ccteam_root>/imd/registry/<workflow_slug>/<role>.json` (non-clobbering — returns `ok:false, error:\"already_registered\"` if the file already exists; unregister first to re-bind). The daemon's registry watcher picks the new file up and spawns the tmux session.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "workflow.yaml name." },
                    "role": { "type": "string", "description": "Role within the workflow." },
                    "vendor": {
                        "type": "string",
                        "enum": ["claude", "codex"],
                        "description": "Harness vendor (lowercase — `claude` or `codex`)."
                    },
                    "im_platform": {
                        "type": "string",
                        "enum": ["telegram", "slack", "discord", "mock"],
                        "description": "IM platform binding."
                    },
                    "im_chat_id": { "type": "string", "description": "Platform-specific chat id (string)." },
                    "persona_id": { "type": "string", "description": "Optional stable persona id for display name / avatar." }
                },
                "required": ["workflow_slug", "role", "vendor", "im_platform", "im_chat_id"],
            }),
        }),
        json!({
            "name": "ccteam__chat_unregister_bot",
            "description": "V0.6.5 F146 — unregister a chat bot: deletes `<ccteam_root>/imd/registry/<workflow_slug>/<role>.json` + the sidecar heartbeat. Idempotent: returns `ok:true, removed:false` when no registration exists. Daemon's registry watcher closes the corresponding tmux session.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "workflow.yaml name." },
                    "role": { "type": "string", "description": "Role within the workflow." }
                },
                "required": ["workflow_slug", "role"],
            }),
        }),
        json!({
            "name": "ccteam__chat_list_bots",
            "description": "V0.6.5 F146 — enumerate registered chat bots, optionally filtered by `workflow_slug`. Each row carries `running` (true when the daemon's sidecar heartbeat is fresh within 30s) + `last_turn_at` (mtime of the ccteam-owned turns.jsonl, RFC3339 or null).",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "Optional filter — return only bots under this slug." }
                },
                "required": [],
            }),
        }),
        // V0.6.0 Wave 1 STUBs still pending real implementations in F147.
        json!({
            "name": "ccteam__chat_send_input",
            "description": "V0.6.0 Wave 2 (F108) STUB — send a user-NL turn to a chat-mode bot session. F147 lands the dispatch handler.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id (registered via chat_register_bot)." },
                    "input": { "type": "string", "description": "User NL / markdown body." }
                },
                "required": ["slug", "bot", "input"],
            }),
        }),
        json!({
            "name": "ccteam__chat_session_reset",
            "description": "V0.6.0 Wave 2 (F108) STUB — force-reset a single chat session. F147 lands the dispatch handler.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "preserve_history": {
                        "type": "boolean",
                        "description": "If true, copy current turns.jsonl into archived/ before reset. Default true."
                    }
                },
                "required": ["slug", "bot"],
            }),
        }),
        json!({
            "name": "ccteam__chat_show_turn_log",
            "description": "V0.6.0 Wave 2 (F108) STUB — return the last N turns from a bot's ccteam-owned <project>/.ccteam/chat/<bot>/turns.jsonl. F147 lands the dispatch handler.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "last_n": {
                        "type": "integer",
                        "description": "How many turns to return (default 20)."
                    }
                },
                "required": ["slug", "bot"],
            }),
        }),
    ]
}

/// Dispatch a `ccteam__chat_*` tool. Returns `Ok(None)` for tools that
/// aren't ours so the caller falls through to the next dispatcher.
pub fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    match name {
        "ccteam__chat_register_bot" => Ok(Some(dispatch_register_bot(paths, args)?)),
        "ccteam__chat_unregister_bot" => Ok(Some(dispatch_unregister_bot(paths, args)?)),
        "ccteam__chat_list_bots" => Ok(Some(dispatch_list_bots(paths, args)?)),
        "ccteam__chat_send_input" | "ccteam__chat_session_reset" | "ccteam__chat_show_turn_log" => {
            Ok(Some(not_implemented_body(name)?))
        }
        _ => Ok(None),
    }
}

fn not_implemented_body(name: &str) -> Result<String> {
    let body = json!({
        "ok": false,
        "error": "NotImplemented",
        "tool": name,
        "wave": "V0.6.5 F147",
        "note": "Stub — F147 lands the dispatch handler.",
    });
    Ok(serde_json::to_string_pretty(&body)?)
}

/// V0.6.5 F146 — `ccteam__chat_register_bot` dispatcher.
pub(crate) fn dispatch_register_bot(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let workflow_slug = arg_str(args, "workflow_slug")?;
    validate_slug(&workflow_slug, "workflow_slug")?;
    let role = arg_str(args, "role")?;
    validate_slug(&role, "role")?;
    let vendor = parse_vendor(args)?;
    let im_platform = arg_str(args, "im_platform")?;
    validate_im_platform(&im_platform)?;
    let im_chat_id = arg_str(args, "im_chat_id")?;
    if im_chat_id.is_empty() {
        return Err(anyhow!("`im_chat_id` must be non-empty"));
    }
    let persona_id = args
        .get("persona_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let outcome = register_bot_checked_in(
        &paths.root,
        &workflow_slug,
        &role,
        vendor,
        &im_platform,
        &im_chat_id,
        persona_id.as_deref(),
    )?;
    match outcome {
        RegisterOutcome::Registered(path) => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "path": path.display().to_string(),
            "workflow_slug": workflow_slug,
            "role": role,
        }))?),
        RegisterOutcome::AlreadyRegistered(path) => Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "error": "already_registered",
            "path": path.display().to_string(),
            "workflow_slug": workflow_slug,
            "role": role,
            "hint": "Unregister first with chat_unregister_bot, then re-register.",
        }))?),
    }
}

/// V0.6.5 F146 — `ccteam__chat_unregister_bot` dispatcher.
pub(crate) fn dispatch_unregister_bot(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let workflow_slug = arg_str(args, "workflow_slug")?;
    validate_slug(&workflow_slug, "workflow_slug")?;
    let role = arg_str(args, "role")?;
    validate_slug(&role, "role")?;
    let (removed, path) = unregister_bot_in(&paths.root, &workflow_slug, &role)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "removed": removed,
        "path": path.display().to_string(),
        "workflow_slug": workflow_slug,
        "role": role,
    }))?)
}

/// V0.6.5 F146 — `ccteam__chat_list_bots` dispatcher.
pub(crate) fn dispatch_list_bots(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let filter_slug = args
        .get("workflow_slug")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref s) = filter_slug {
        validate_slug(s, "workflow_slug")?;
    }
    let regs = list_bots_in(&paths.root, filter_slug.as_deref())?;
    let bots: Vec<Value> = regs
        .into_iter()
        .map(|reg| {
            let running = bot_running_status_in(&paths.root, &reg.workflow_slug, &reg.role);
            let last = last_turn_at(&paths.projects_root, &reg.workflow_slug, &reg.role)
                .map(|dt| dt.to_rfc3339());
            // vendor is serialized via AgentVendor's `rename_all = "lowercase"`
            // — see ccteam_core::harness::AgentVendor — so the wire shape is
            // guaranteed `"claude"` / `"codex"` (Bug A防线).
            json!({
                "workflow_slug": reg.workflow_slug,
                "role": reg.role,
                "vendor": reg.vendor,
                "im_platform": reg.im_platform,
                "im_chat_id": reg.im_chat_id,
                "persona_id": reg.persona_id,
                "created_at": reg.created_at.to_rfc3339(),
                "running": running,
                "last_turn_at": last,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "bots": bots,
    }))?)
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("missing required string arg `{key}`"))
}

/// V0.6.5 F146 — accept only the lowercase vendor strings the daemon's
/// `BotRegistration` deserialize understands (`#[serde(rename_all =
/// "lowercase")]` on `AgentVendor`). We **lowercase first** so a
/// misconfigured caller with `"Claude"` lands in the right enum
/// variant rather than tripping the daemon at registry-watcher time
/// (Bug A from NAS deploy).
fn parse_vendor(args: &Value) -> Result<AgentVendor> {
    let raw = arg_str(args, "vendor")?;
    let lower = raw.to_lowercase();
    match lower.as_str() {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        other => Err(anyhow!(
            "invalid vendor `{other}`: expected one of `claude`, `codex`"
        )),
    }
}

fn validate_slug(s: &str, field: &str) -> Result<()> {
    if s.is_empty() {
        return Err(anyhow!("`{field}` must be non-empty"));
    }
    // Path-injection guard — slug becomes a dir component.
    if s.contains('/') || s.contains('\\') || s == "." || s == ".." || s.starts_with('.') {
        return Err(anyhow!(
            "`{field}` contains illegal characters or path component: `{s}`"
        ));
    }
    // Conservative: alphanumeric + `-` + `_` only.
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!("`{field}` may contain only [a-zA-Z0-9_-]: `{s}`"));
    }
    Ok(())
}

fn validate_im_platform(p: &str) -> Result<()> {
    match p {
        "telegram" | "slack" | "discord" | "mock" => Ok(()),
        other => Err(anyhow!(
            "invalid im_platform `{other}`: expected one of `telegram`, `slack`, `discord`, `mock`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("ccteam-root"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn six_chat_tools_registered_with_correct_names() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        // V0.6.5 F146 real tools.
        assert!(names.contains(&"ccteam__chat_register_bot"));
        assert!(names.contains(&"ccteam__chat_unregister_bot"));
        assert!(names.contains(&"ccteam__chat_list_bots"));
        // F147-pending stubs.
        assert!(names.contains(&"ccteam__chat_send_input"));
        assert!(names.contains(&"ccteam__chat_session_reset"));
        assert!(names.contains(&"ccteam__chat_show_turn_log"));
        // Removed in V0.6.5 F146 — no deprecated alias.
        assert!(!names.contains(&"ccteam__chat_lifecycle"));
    }

    #[test]
    fn all_chat_tools_carry_chat_prefix() {
        for t in chat_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__chat_"),
                "chat tool name must start with ccteam__chat_: {n}"
            );
        }
    }

    #[test]
    fn dispatch_returns_none_for_foreign_tools() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        assert!(dispatch(&p, "ccteam__workflow_ls", &json!({}))
            .unwrap()
            .is_none());
        assert!(dispatch(&p, "ccteam__advise_vote", &json!({}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn dispatch_returns_not_implemented_for_pending_stubs() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let body = dispatch(&p, "ccteam__chat_send_input", &json!({}))
            .unwrap()
            .expect("matched our tool");
        assert!(body.contains("NotImplemented"));
        assert!(body.contains("F147"));
    }

    #[test]
    fn register_bot_writes_registry_with_lowercase_vendor() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let body = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        let path = parsed["path"].as_str().unwrap();
        let disk = std::fs::read_to_string(path).unwrap();
        let on_disk: Value = serde_json::from_str(&disk).unwrap();
        // Bug A防线 — vendor on disk MUST be lowercase.
        assert_eq!(on_disk["vendor"], "claude");
        assert_eq!(on_disk["workflow_slug"], "demo");
        assert_eq!(on_disk["role"], "helper");
        assert_eq!(on_disk["im_platform"], "telegram");
    }

    #[test]
    fn register_bot_lowercases_uppercase_vendor_input() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        // Caller sends "Claude" (PascalCase) — schema enum *says* lowercase
        // but the daemon's BotRegistration deserialize trips on this if
        // it sneaks through. We lowercase first so the registry JSON is
        // always canonical.
        let body = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper2",
                "vendor": "Claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        let path = parsed["path"].as_str().unwrap();
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(on_disk["vendor"], "claude");
    }

    #[test]
    fn register_bot_rejects_invalid_vendor() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let err = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "gpt",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid vendor"));
    }

    #[test]
    fn register_bot_idempotent_miss_on_duplicate() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let args = json!({
            "workflow_slug": "demo",
            "role": "helper",
            "vendor": "claude",
            "im_platform": "telegram",
            "im_chat_id": "42",
        });
        let first = dispatch_register_bot(&p, &args).unwrap();
        let first_parsed: Value = serde_json::from_str(&first).unwrap();
        let first_path = first_parsed["path"].as_str().unwrap().to_string();
        // Snapshot the bytes so we can prove the second call did NOT clobber.
        let original_bytes = std::fs::read(&first_path).unwrap();

        // Second call with a *different* chat_id — must NOT overwrite.
        let dup = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "99",
            }),
        )
        .unwrap();
        let dup_parsed: Value = serde_json::from_str(&dup).unwrap();
        assert_eq!(dup_parsed["ok"], false);
        assert_eq!(dup_parsed["error"], "already_registered");
        let after_bytes = std::fs::read(&first_path).unwrap();
        assert_eq!(
            original_bytes, after_bytes,
            "duplicate register must not clobber existing file"
        );
    }

    #[test]
    fn list_bots_filters_by_workflow_slug() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        // Seed two slugs.
        dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap();
        dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "other",
                "role": "lead",
                "vendor": "codex",
                "im_platform": "slack",
                "im_chat_id": "C123",
            }),
        )
        .unwrap();

        // No filter → 2 rows.
        let all = dispatch_list_bots(&p, &json!({})).unwrap();
        let all_parsed: Value = serde_json::from_str(&all).unwrap();
        assert_eq!(all_parsed["bots"].as_array().unwrap().len(), 2);

        // Filter to "demo" → 1 row.
        let filtered = dispatch_list_bots(&p, &json!({ "workflow_slug": "demo" })).unwrap();
        let filtered_parsed: Value = serde_json::from_str(&filtered).unwrap();
        let bots = filtered_parsed["bots"].as_array().unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0]["workflow_slug"], "demo");
        assert_eq!(bots[0]["role"], "helper");
        // Vendor must round-trip as lowercase on the wire.
        assert_eq!(bots[0]["vendor"], "claude");
        // No heartbeat seeded → running:false.
        assert_eq!(bots[0]["running"], false);

        // Filter to a non-existent slug → 0 rows.
        let none = dispatch_list_bots(&p, &json!({ "workflow_slug": "missing" })).unwrap();
        let none_parsed: Value = serde_json::from_str(&none).unwrap();
        assert_eq!(none_parsed["bots"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_bots_running_status_reflects_heartbeat_freshness() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap();
        // Touch heartbeat — running should flip to true.
        let hb = ccteam_imd::bot_heartbeat_path_in(&p.root, "demo", "helper");
        std::fs::create_dir_all(hb.parent().unwrap()).unwrap();
        std::fs::write(&hb, chrono::Utc::now().to_rfc3339()).unwrap();
        let body = dispatch_list_bots(&p, &json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["bots"][0]["running"], true);
    }

    #[test]
    fn unregister_bot_removes_file_then_idempotent_miss() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap();
        let first =
            dispatch_unregister_bot(&p, &json!({ "workflow_slug": "demo", "role": "helper" }))
                .unwrap();
        let first_parsed: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first_parsed["ok"], true);
        assert_eq!(first_parsed["removed"], true);
        assert!(
            !std::path::Path::new(first_parsed["path"].as_str().unwrap()).exists(),
            "file must be gone after first unregister"
        );

        let second =
            dispatch_unregister_bot(&p, &json!({ "workflow_slug": "demo", "role": "helper" }))
                .unwrap();
        let second_parsed: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second_parsed["ok"], true);
        // Idempotent miss.
        assert_eq!(second_parsed["removed"], false);
    }

    #[test]
    fn register_bot_rejects_path_injection_slug() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let err = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "../escape",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("illegal"));
    }
}
