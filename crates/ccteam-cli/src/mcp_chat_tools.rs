//! `ccteam__chat_*` MCP tools.
//!
//! V0.6.5 F146 turned `chat_register_bot` / `chat_unregister_bot` /
//! `chat_list_bots` from V0.6.0 Wave 1 STUBs into real implementations
//! and **removed** the old `chat_lifecycle` multi-action stub (no
//! deprecated alias — V0.6.5 dev-plan §F146; CLAUDE.md §五 #4 forbids
//! backwards-compat shims pre-v1.0).
//!
//! The chat group is 4 tools: `chat_register_bot` / `chat_unregister_bot`
//! / `chat_list_bots` / `chat_send_file`. (`chat_send_input` and
//! `chat_history` were removed: both addressed a now-defunct role-keyed
//! control plane — `send_input` wrote a mailbox the deleted BotSupervisor
//! never drained, and `history` read a role-keyed `turns.jsonl` that the
//! session-keystone refactor stopped writing — so both were no-ops. No
//! deprecated alias per CLAUDE.md §五 #4.)
//!
//! Architecture:
//!
//! - The 3 lifecycle / list tools wrap `ccteam_im::{register_bot_checked_in,
//!   unregister_bot_in, list_bots_in}` (file-system control plane:
//!   registry JSON under `<ccteam_root>/state/im/registry/<slug>/<role>.json`).
//! - `running` status comes from a sidecar heartbeat file the daemon's
//!   per-bot supervisor touches every 5s; an MCP process (separate
//!   from the daemon) reads mtime to infer "alive within 30s".
//! - Vendor is normalized to lowercase (`"claude"` / `"codex"`) before
//!   serde-deserializing into `AgentVendor` — the daemon's
//!   `BotRegistration` deserialize trips on `"Claude"` (Bug A from
//!   the NAS deploy session).

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use ccteam_core::agent_naming::pick_unused_bot_name;
use ccteam_core::paths::CcteamPaths;
use ccteam_harness::AgentVendor;
use ccteam_im::{
    bot_running_status_in, last_turn_at, list_bots_in, register_bot_checked_in, unregister_bot_in,
    RegisterOutcome,
};

/// Tool definitions for the chat group (total 4): register / unregister
/// / list_bots / send_file. Merged into the top-level
/// `tool_definitions()` in `mcp_serve.rs`.
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__chat_register_bot",
            "description": "Register a chat-mode bot. Writes `<ccteam_root>/state/im/registry/<workflow_slug>/<role>.json` (non-clobbering — returns `ok:false, error:\"already_registered\"` if the file already exists; unregister first to re-bind). The daemon's registry watcher picks the new file up and spawns the tmux session. When `chat_handle` is omitted, the dispatcher auto-mints an unused scientist nickname from `ccteam_core::agent_naming::SCIENTIST_NAMES` (e.g. `curie`, `galileo`) so IM users get a friendly `@curie` handle instead of `@helper`. Pass `chat_handle` to pin a specific handle.",
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
                        "enum": ["telegram", "slack", "discord", "lark", "mock"],
                        "description": "IM platform binding."
                    },
                    "im_chat_id": { "type": "string", "description": "Platform-specific chat id (string)." },
                    "persona_id": { "type": "string", "description": "Optional stable persona id for display name / avatar." },
                    "chat_handle": { "type": "string", "description": "Optional IM mention this bot answers to (without leading `@`). Omit to auto-mint an unused scientist nickname. Letters, digits, `_`, `-` only." },
                    "project_dir": { "type": "string", "description": "Absolute path to the project directory hosting .ccteam/workflow.yaml. When omitted, defaults to the MCP server's current working directory (canonicalized). The daemon resolves the bot's working dir as `<project_dir>/.ccteam/chat/<role>/`. Use this when the project lives outside `~/projects/<slug>/` (NAS share, dir basename differs from workflow slug)." }
                },
                "required": ["workflow_slug", "role", "vendor", "im_platform", "im_chat_id"],
            }),
        }),
        json!({
            "name": "ccteam__chat_unregister_bot",
            "description": "V0.6.5 F146 — unregister a chat bot: deletes `<ccteam_root>/state/im/registry/<workflow_slug>/<role>.json` + the sidecar heartbeat. Idempotent: returns `ok:true, removed:false` when no registration exists. Daemon's registry watcher closes the corresponding tmux session.",
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
        json!({
            "name": "ccteam__chat_send_file",
            "description": "V0.8.4 P2b — send a file (image or document) from disk back to YOUR own bound chat (Telegram / Lark / web). Zero addressing params: your identity comes from the spawn-injected CCTEAM_CHAT_SLUG / CCTEAM_CHAT_ROLE env, and the daemon resolves your home chat from the registry. `path` must be on the daemon's filesystem (shared with you under tmux). `kind` is inferred from the extension when omitted (png/jpg/jpeg/gif/webp → photo, else document). To send a rendered screenshot, compose with `screenshot`: it returns a PNG path → pass that to chat_send_file. Delivery reuses the same outbound funnel as text replies (long-message split + durable ledger + failure echo).",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file on the daemon's filesystem." },
                    "caption": { "type": "string", "description": "Optional caption sent with the file." },
                    "kind": { "type": "string", "enum": ["photo", "document"], "description": "photo → sendPhoto (compressed image); document → sendDocument (any file). Inferred from the extension when omitted." }
                },
                "required": ["path"],
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
        _ => Ok(None),
    }
}

/// `ccteam__chat_register_bot` dispatcher.
///
/// When `chat_handle` is omitted the dispatcher walks the existing
/// registry, collects every effective handle currently in use (taking
/// per-bot `chat_handle` when set, otherwise `role`), and asks
/// `pick_unused_bot_name` for the first unused scientist nickname.
/// The minted handle is persisted into `BotRegistration.chat_handle`
/// so the daemon's `build_handle_map` resolves `@<minted>` →
/// `(slug, role)` immediately on the next registry-watcher tick.
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

    // Caller-supplied handle wins; absent → auto-mint from
    // SCIENTIST_NAMES across every effective handle already in use.
    let chat_handle = match args.get("chat_handle").and_then(|v| v.as_str()) {
        Some(h) if !h.is_empty() => {
            validate_chat_handle(h)?;
            Some(h.to_string())
        }
        _ => Some(mint_unused_handle(&paths.root)?),
    };

    // F185 — caller-supplied `project_dir` MUST be absolute. When
    // omitted, default to the MCP server's `current_dir` canonicalized
    // through the filesystem (resolves symlinks too — daemon stores the
    // post-canonical form so a moved symlink doesn't silently rebind).
    let project_dir_explicit = args
        .get("project_dir")
        .and_then(|v| v.as_str())
        .is_some_and(|p| !p.is_empty());
    let project_dir = match args.get("project_dir").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => {
            let path = std::path::PathBuf::from(p);
            if !path.is_absolute() {
                return Err(anyhow!(
                    "`project_dir` must be an absolute path (got `{}`)",
                    p
                ));
            }
            path
        }
        _ => std::env::current_dir().context("std::env::current_dir for default project_dir")?,
    };
    let project_dir = std::fs::canonicalize(&project_dir).with_context(|| {
        format!(
            "canonicalize project_dir `{}` (does the path exist?)",
            project_dir.display()
        )
    })?;

    // F197 — bootstrap `.ccteam/state.json` so the SessionStart hook's
    // `session_context_from_cwd` walk-up finds the project root. The
    // creator skill writes workflow.yaml / persona / registry but
    // historically skipped `bootstrap_project_at_dir`, leaving state.json
    // missing and every hook firing "not under any ccteam project".
    //
    // Gated on `project_dir_explicit` so unit tests (which omit
    // `project_dir` and fall back to the current_dir, i.e. the ccteam
    // source tree) don't pollute the repo with a stray `.ccteam/`. The
    // creator-skill SessionStart-bug scenario always supplies
    // `project_dir`, so the safety net still fires there.
    //
    // F198 — also lay down `<paths.root>/hooks/hook.sh` so the F139
    // dispatcher Claude Code hooks shell out to actually exists.
    // `install_hooks` is idempotent (`Unchanged` on a re-run), safe to
    // call on every register.
    if project_dir_explicit {
        let state_path = ccteam_core::CcteamPaths::project_state_in(&project_dir);
        if !state_path.exists() {
            let slug_for_bootstrap = workflow_slug.clone();
            ccteam_core::bootstrap_project_at_dir(
                paths,
                &project_dir,
                &slug_for_bootstrap,
                "",
                "chat",
            )
            .with_context(|| {
                format!(
                    "bootstrap_project_at_dir for {} (creator-flow state.json seed)",
                    project_dir.display()
                )
            })?;
        }
        ccteam_core::install_hooks(paths).context("install ~/.ccteam/hooks/hook.sh dispatcher")?;
    }

    let outcome = register_bot_checked_in(
        &paths.root,
        &workflow_slug,
        &role,
        vendor,
        &im_platform,
        &im_chat_id,
        persona_id.as_deref(),
        chat_handle.as_deref(),
        Some(project_dir.as_path()),
    )?;
    match outcome {
        RegisterOutcome::Registered(path) => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "path": path.display().to_string(),
            "workflow_slug": workflow_slug,
            "role": role,
            "chat_handle": chat_handle,
            "project_dir": project_dir.display().to_string(),
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

/// Pick the first unused scientist nickname across the entire registry.
///
/// "Effective handle" = `chat_handle.unwrap_or(role)` per bot — the same
/// resolution `build_handle_map` uses — so the auto-mint never picks a
/// name that would collide with an existing bot that fell back to its
/// role as the handle. The match is case-insensitive in
/// `pick_unused_bot_name` so registries that stored handles in mixed
/// case still claim the right slot.
///
/// V0.6.8 F202 — `pub(crate)` so the `ccteam admin register-bot` CLI
/// dispatcher in `commands.rs` can re-use the same auto-mint logic
/// without copy-paste.
pub(crate) fn mint_unused_handle(ccteam_root: &Path) -> Result<String> {
    let existing = list_bots_in(ccteam_root, None).unwrap_or_default();
    let in_use: Vec<String> = existing
        .iter()
        .map(|b| b.chat_handle.clone().unwrap_or_else(|| b.role.clone()))
        .collect();
    Ok(pick_unused_bot_name(&in_use))
}

/// Caller-supplied handles share the slug validator rules so registry
/// filenames + router parse paths stay clean (alphanumeric / `_` / `-`).
///
/// V0.6.8 F202 — `pub(crate)` so the `ccteam admin register-bot` CLI
/// dispatcher can re-use the same validator.
pub(crate) fn validate_chat_handle(s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(anyhow!("`chat_handle` must be non-empty"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(anyhow!(
            "`chat_handle` may contain only [a-zA-Z0-9_-]: `{s}`"
        ));
    }
    Ok(())
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
            let last = last_turn_at(&paths.projects_root, &reg).map(|dt| dt.to_rfc3339());
            // vendor is serialized via AgentVendor's `rename_all = "lowercase"`
            // — see ccteam_harness::AgentVendor — so the wire shape is
            // guaranteed `"claude"` / `"codex"` (Bug A防线).
            json!({
                "workflow_slug": reg.workflow_slug,
                "role": reg.role,
                "vendor": reg.vendor,
                "im_platform": reg.im_platform,
                "im_chat_id": reg.im_chat_id,
                "persona_id": reg.persona_id,
                "chat_handle": reg.chat_handle,
                "project_dir": reg.project_dir,
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

/// V0.6.8 F202 — `pub(crate)` so the `ccteam admin register-bot` CLI
/// dispatcher can re-use the same slug/role validator the MCP path
/// uses (alphanumerics + `-` + `_`). Distinct from
/// `ccteam_core::validate_slug_format`, which is stricter (lowercase
/// + digits + dashes only) and gates init-time project slugs.
pub(crate) fn validate_slug(s: &str, field: &str) -> Result<()> {
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
        "telegram" | "slack" | "discord" | "lark" | "mock" => Ok(()),
        other => Err(anyhow!(
            "invalid im_platform `{other}`: expected one of `telegram`, `slack`, `discord`, `lark`, `mock`"
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
    fn four_chat_tools_registered_with_correct_names() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        // V0.6.5 F146 real tools.
        assert!(names.contains(&"ccteam__chat_register_bot"));
        assert!(names.contains(&"ccteam__chat_unregister_bot"));
        assert!(names.contains(&"ccteam__chat_list_bots"));
        // V0.8.4 P2b — outbound file send.
        assert!(names.contains(&"ccteam__chat_send_file"));
        // Removed (no deprecated alias — CLAUDE.md §五 #4). `send_input`
        // and `history` addressed a now-defunct role-keyed control plane
        // (dead mailbox / never-written turns.jsonl) → dropped.
        assert!(!names.contains(&"ccteam__chat_send_input"));
        assert!(!names.contains(&"ccteam__chat_history"));
        assert!(!names.contains(&"ccteam__chat_reset"));
        assert!(!names.contains(&"ccteam__chat_lifecycle"));
        assert!(!names.contains(&"ccteam__chat_session_reset"));
        assert!(!names.contains(&"ccteam__chat_show_turn_log"));
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
    fn validate_im_platform_accepts_lark_and_known_platforms() {
        // Lark/Feishu is a first-class platform alongside the originals.
        for p in ["telegram", "slack", "discord", "lark", "mock"] {
            assert!(
                validate_im_platform(p).is_ok(),
                "`{p}` must be an accepted im_platform"
            );
        }
        // Unknown platforms still fail, and the error names the closed set
        // (now including `lark`).
        let err = validate_im_platform("matrix").unwrap_err().to_string();
        assert!(
            err.contains("matrix") && err.contains("lark"),
            "rejection must name the bad value + list `lark`; got: {err}"
        );
    }

    #[test]
    fn register_bot_schema_enum_lists_lark() {
        // The MCP register_bot tool's `im_platform` enum must advertise
        // `lark` so callers/clients see it as a legal binding.
        let tools = chat_tool_definitions();
        let reg = tools
            .iter()
            .find(|t| t["name"] == "ccteam__chat_register_bot")
            .expect("register_bot tool must exist");
        let variants = reg["inputSchema"]["properties"]["im_platform"]["enum"]
            .as_array()
            .expect("im_platform enum must be an array");
        let values: Vec<&str> = variants.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            values.contains(&"lark"),
            "register_bot im_platform enum must include `lark`; got: {values:?}"
        );
        // Sanity — the originals are still present (no accidental clobber).
        for p in ["telegram", "slack", "discord", "mock"] {
            assert!(values.contains(&p), "enum must still include `{p}`");
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
        let hb = ccteam_im::bot_heartbeat_path_in(&p.root, "demo", "helper");
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

    #[test]
    fn register_bot_auto_mints_scientist_nickname_when_chat_handle_absent() {
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
        let minted = parsed["chat_handle"]
            .as_str()
            .expect("dispatcher reports the minted handle in its reply");
        assert!(!minted.is_empty());
        // First-mint into an empty registry takes the head of
        // SCIENTIST_NAMES (Euclid).
        assert_eq!(minted, "Euclid");
        // On-disk row carries the same value so the daemon's
        // build_handle_map resolves @Euclid on the next tick.
        let path = parsed["path"].as_str().unwrap();
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(on_disk["chat_handle"], "Euclid");
    }

    #[test]
    fn register_bot_caller_supplied_chat_handle_overrides_auto_mint() {
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
                "chat_handle": "curie",
            }),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["chat_handle"], "curie");
        let path = parsed["path"].as_str().unwrap();
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(on_disk["chat_handle"], "curie");
    }

    #[test]
    fn register_bot_auto_mint_skips_handles_already_in_use() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        // First registration: claim Euclid via explicit handle.
        dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "a",
                "role": "lead",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "1",
                "chat_handle": "Euclid",
            }),
        )
        .unwrap();
        // Second registration with no chat_handle — mint should skip
        // Euclid (case-insensitive) and pick Archimedes.
        let body = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "b",
                "role": "lead",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "2",
            }),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["chat_handle"], "Archimedes");
    }

    #[test]
    fn register_bot_rejects_invalid_chat_handle_chars() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let err = dispatch_register_bot(
            &p,
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "vendor": "claude",
                "im_platform": "telegram",
                "im_chat_id": "42",
                "chat_handle": "bad/handle",
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("chat_handle"));
    }
}
