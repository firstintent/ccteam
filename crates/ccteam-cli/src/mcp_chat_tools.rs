//! `ccteam__chat_*` MCP tools.
//!
//! V0.6.5 F146 turned `chat_register_bot` / `chat_unregister_bot` /
//! `chat_list_bots` from V0.6.0 Wave 1 STUBs into real implementations
//! and **removed** the old `chat_lifecycle` multi-action stub (no
//! deprecated alias — V0.6.5 dev-plan §F146; CLAUDE.md §五 #4 forbids
//! backwards-compat shims pre-v1.0).
//!
//! V0.6.5 F147 lands the remaining 3 stubs as real implementations
//! against the same file-system control plane:
//!
//! - `chat_send_input` — writes an [`ccteam_imd::inbound::InboxEnvelope`]
//!   into the bot's mailbox dir (`<project>/.ccteam/chat/<role>/inbox/msg-<unix-ms>-<rand>.md`).
//!   The daemon's per-bot mpsc fast-path (or the safety-net
//!   `drain_inboxes` tick) picks it up and submits to the live tmux
//!   session via `BotSupervisor::handle_inbound`. We do NOT push to
//!   tmux directly — that would bypass the supervisor's draining /
//!   shutdown gates AND violate CLAUDE.md §三 "No prompt injection"
//!   (only the user-turn payload is forwarded; no system prompt).
//! - `chat_history` — tails `<project>/.ccteam/chat/<role>/turns.jsonl`,
//!   returns the last `n` rows. `include_user` flag toggles whether
//!   user-side prompts are included (default: assistant only).
//! - `chat_reset` — writes `<project>/.ccteam/chat/<role>/signals/reset.signal`.
//!   The supervisor's next `decide()` tick returns `ResetSession`, which
//!   archives `turns.jsonl` → `archive/turns-<unix-ms>.jsonl`, closes
//!   the active tmux session, force-resets the in-memory OutboundCursor
//!   (V0.6.4 Bug B防线) + clears the on-disk transcript cursor, then
//!   spawns a fresh session.
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

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use ccteam_core::agent_naming::pick_unused_bot_name;
use ccteam_core::execution::turns_mirror::{read_all_turns, TurnRecord};
use ccteam_core::harness::AgentVendor;
use ccteam_core::paths::CcteamPaths;
use ccteam_imd::inbound::{render_envelope, InboxEnvelope};
use ccteam_imd::{
    bot_running_status_in, chat_inbox_dir, chat_reset_signal_path, last_turn_at, list_bots_in,
    register_bot_checked_in, turns_jsonl_path, unregister_bot_in, RegisterOutcome,
};

/// Tool definitions for the chat group: 3 real + 3 stubs (total 6).
/// Merged into the top-level `tool_definitions()` in `mcp_serve.rs`.
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__chat_register_bot",
            "description": "Register a chat-mode bot. Writes `<ccteam_root>/imd/registry/<workflow_slug>/<role>.json` (non-clobbering — returns `ok:false, error:\"already_registered\"` if the file already exists; unregister first to re-bind). The daemon's registry watcher picks the new file up and spawns the tmux session. When `chat_handle` is omitted, the dispatcher auto-mints an unused scientist nickname from `ccteam_core::agent_naming::SCIENTIST_NAMES` (e.g. `curie`, `galileo`) so IM users get a friendly `@curie` handle instead of `@helper`. Pass `chat_handle` to pin a specific handle.",
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
                    "persona_id": { "type": "string", "description": "Optional stable persona id for display name / avatar." },
                    "chat_handle": { "type": "string", "description": "Optional IM mention this bot answers to (without leading `@`). Omit to auto-mint an unused scientist nickname. Letters, digits, `_`, `-` only." }
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
        json!({
            "name": "ccteam__chat_send_input",
            "description": "V0.6.5 F147 — drop a user-NL turn into a chat-mode bot's mailbox. Writes `<project>/.ccteam/chat/<role>/inbox/msg-<unix-ms>-<rand>.md` (router-shaped InboxEnvelope). The daemon's per-bot mpsc fast-path picks it up within ~ms (or the 60s safety-net `drain_inboxes` if the fast-path isn't wired yet) and submits to the live tmux session via `BotSupervisor::handle_inbound`. Does NOT inject system prompts — only the `content` body is forwarded as a user turn (CLAUDE.md §三 red line).",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "Workflow slug (matches the bot registered via chat_register_bot)." },
                    "role": { "type": "string", "description": "Bot role within the workflow." },
                    "content": { "type": "string", "description": "User NL / markdown body. Forwarded verbatim to the tmux session via submit_turn." },
                    "reply_to": { "type": "string", "description": "Optional originating message id (echoes back through turns.jsonl for round-trip correlation)." }
                },
                "required": ["workflow_slug", "role", "content"],
            }),
        }),
        json!({
            "name": "ccteam__chat_history",
            "description": "V0.6.5 F147 — tail `<project>/.ccteam/chat/<role>/turns.jsonl`. Returns the last `n` turns (default 20). By default only `assistant`-side rows are returned; pass `include_user: true` to interleave user-side prompts (useful for full transcript reconstruction).",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "Workflow slug." },
                    "role": { "type": "string", "description": "Bot role." },
                    "n": { "type": "integer", "description": "How many turns to return (default 20)." },
                    "include_user": { "type": "boolean", "description": "Include user-side prompts in the result (default false — assistant rows only)." }
                },
                "required": ["workflow_slug", "role"],
            }),
        }),
        json!({
            "name": "ccteam__chat_reset",
            "description": "V0.6.5 F147 — request a session reset for a chat-mode bot. Writes `<project>/.ccteam/chat/<role>/signals/reset.signal`; the daemon's next supervisor tick (≤5s default) reads it, archives `turns.jsonl` → `archive/turns-<unix-ms>.jsonl`, force-resets the outbound + transcript cursors to 0 (V0.6.4 Bug B防线 — prevents the new session's first burst from being dedup-dropped), closes the active tmux session, and spawns a fresh one. Returns immediately after writing the signal — poll `chat_list_bots` for `last_turn_at` to observe the reset taking effect.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "workflow_slug": { "type": "string", "description": "Workflow slug." },
                    "role": { "type": "string", "description": "Bot role." }
                },
                "required": ["workflow_slug", "role"],
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
        "ccteam__chat_send_input" => Ok(Some(dispatch_send_input_in(&paths.projects_root, args)?)),
        "ccteam__chat_history" => Ok(Some(dispatch_history_in(&paths.projects_root, args)?)),
        "ccteam__chat_reset" => Ok(Some(dispatch_reset_in(&paths.projects_root, args)?)),
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

    let outcome = register_bot_checked_in(
        &paths.root,
        &workflow_slug,
        &role,
        vendor,
        &im_platform,
        &im_chat_id,
        persona_id.as_deref(),
        chat_handle.as_deref(),
    )?;
    match outcome {
        RegisterOutcome::Registered(path) => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "path": path.display().to_string(),
            "workflow_slug": workflow_slug,
            "role": role,
            "chat_handle": chat_handle,
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
fn mint_unused_handle(ccteam_root: &Path) -> Result<String> {
    let existing = list_bots_in(ccteam_root, None).unwrap_or_default();
    let in_use: Vec<String> = existing
        .iter()
        .map(|b| b.chat_handle.clone().unwrap_or_else(|| b.role.clone()))
        .collect();
    Ok(pick_unused_bot_name(&in_use))
}

/// Caller-supplied handles share the slug validator rules so registry
/// filenames + router parse paths stay clean (alphanumeric / `_` / `-`).
fn validate_chat_handle(s: &str) -> Result<()> {
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
                "chat_handle": reg.chat_handle,
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

/// V0.6.5 F147 — `ccteam__chat_send_input` dispatcher (tempdir-aware
/// variant for tests). Production callers go through [`dispatch`] which
/// substitutes `paths.projects_root` for `projects_root`.
pub fn dispatch_send_input_in(projects_root: &Path, args: &Value) -> Result<String> {
    let workflow_slug = arg_str(args, "workflow_slug")?;
    validate_slug(&workflow_slug, "workflow_slug")?;
    let role = arg_str(args, "role")?;
    validate_slug(&role, "role")?;
    let content = arg_str(args, "content")?;
    if content.is_empty() {
        return Err(anyhow!("`content` must be non-empty"));
    }
    let reply_to = args
        .get("reply_to")
        .and_then(|v| v.as_str())
        .map(String::from);

    let inbox = chat_inbox_dir(projects_root, &workflow_slug, &role);
    fs::create_dir_all(&inbox).with_context(|| format!("mkdir -p {}", inbox.display()))?;

    // Compose a router-shaped envelope. We bypass the IM security
    // pipeline (the caller is the meta-agent host process — already
    // trusted local code), but we use the same `InboxEnvelope` shape so
    // the daemon's parse + handle path is one well-tested code path.
    //
    // Filename: `msg-<unix-ms>-<rand>.md`. The 8-hex `<rand>` collision
    // window inside one millisecond is 2^32 — well past any single-host
    // concurrent MCP burst. (F147 PRD §risks.)
    let unix_ms = chrono::Utc::now().timestamp_millis().max(0) as u128;
    let rand_hex = generate_rand_hex();
    let file_name = format!("msg-{unix_ms}-{rand_hex}.md");
    let path = inbox.join(&file_name);
    let cid = reply_to
        .clone()
        .unwrap_or_else(|| format!("mcp-{unix_ms}-{rand_hex}"));

    let envelope = InboxEnvelope {
        platform: "mcp".to_string(),
        sender: "mcp-host".to_string(),
        hop: 0,
        received_at: chrono::Utc::now(),
        // No IM reply target — the agent's reply flows back through
        // `turns.jsonl` and the caller can read it via `chat_history`.
        reply_target: String::new(),
        payload: content,
        message_id: cid.clone(),
    };
    let body = render_envelope(&envelope);
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "mailbox_path": path.display().to_string(),
        "cid": cid,
        "workflow_slug": workflow_slug,
        "role": role,
    }))?)
}

/// V0.6.5 F147 — `ccteam__chat_history` dispatcher (tempdir-aware
/// variant for tests).
pub fn dispatch_history_in(projects_root: &Path, args: &Value) -> Result<String> {
    let workflow_slug = arg_str(args, "workflow_slug")?;
    validate_slug(&workflow_slug, "workflow_slug")?;
    let role = arg_str(args, "role")?;
    validate_slug(&role, "role")?;
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| x as usize)
        .unwrap_or(20);
    let include_user = args
        .get("include_user")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let project_dir = projects_root.join(&workflow_slug);
    let turns_path = turns_jsonl_path(projects_root, &workflow_slug, &role);

    // Missing file = bot registered but no turn yet. Return empty list
    // rather than erroring so caller can treat "no history" uniformly.
    if !turns_path.exists() {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "workflow_slug": workflow_slug,
            "role": role,
            "turns_jsonl": turns_path.display().to_string(),
            "turns": Vec::<Value>::new(),
        }))?);
    }

    let all = read_all_turns(&project_dir, &role)
        .with_context(|| format!("read turns.jsonl for {workflow_slug}/{role}"))?;

    // Project each TurnRecord to the wire view. Optional `include_user`
    // flag interleaves a synthetic `role:"user"` row before the
    // assistant row when the underlying TurnRecord carries non-empty
    // user-side text. We keep chronological ordering (oldest first
    // within the returned slice) for caller predictability.
    let mut wire: Vec<Value> = Vec::new();
    for t in &all {
        if include_user && !t.user.is_empty() {
            wire.push(turn_to_value(t, "user", &t.user));
        }
        if !t.assistant.is_empty() {
            wire.push(turn_to_value(t, "assistant", &t.assistant));
        }
    }
    let start = wire.len().saturating_sub(n);
    let tail: Vec<Value> = wire.into_iter().skip(start).collect();

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "workflow_slug": workflow_slug,
        "role": role,
        "turns_jsonl": turns_path.display().to_string(),
        "turns": tail,
    }))?)
}

/// V0.6.5 F147 — `ccteam__chat_reset` dispatcher (tempdir-aware variant
/// for tests).
pub fn dispatch_reset_in(projects_root: &Path, args: &Value) -> Result<String> {
    let workflow_slug = arg_str(args, "workflow_slug")?;
    validate_slug(&workflow_slug, "workflow_slug")?;
    let role = arg_str(args, "role")?;
    validate_slug(&role, "role")?;

    let sig = chat_reset_signal_path(projects_root, &workflow_slug, &role);
    if let Some(parent) = sig.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    // Body = unix-ms so the supervisor can log when the reset was
    // requested vs when it was applied. Idempotent overwrite — if a
    // prior signal hasn't been consumed yet, we just refresh the ts.
    let unix_ms = chrono::Utc::now().timestamp_millis();
    fs::write(&sig, format!("{unix_ms}")).with_context(|| format!("write {}", sig.display()))?;

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "signal_path": sig.display().to_string(),
        "workflow_slug": workflow_slug,
        "role": role,
        "requested_at_unix_ms": unix_ms,
    }))?)
}

fn turn_to_value(t: &TurnRecord, side: &str, content: &str) -> Value {
    json!({
        "turn_id": t.turn_id,
        "ts": t.ts.to_rfc3339(),
        "vendor": t.vendor,
        "role": side,
        "bot_role": t.role,
        "content": content,
    })
}

/// V0.6.5 F147 — 8-hex random suffix for mailbox filenames. Uses the
/// stdlib `RandomState` hasher seeded from system entropy to avoid
/// pulling in an extra `rand` direct dep on ccteam-cli. Collision
/// surface: 2^32 inside a single millisecond, vastly more than any
/// realistic concurrent MCP burst.
fn generate_rand_hex() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u128);
    format!("{:08x}", (h.finish() & 0xFFFF_FFFF) as u32)
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
        // V0.6.5 F147 real tools (renamed from the V0.6.0 stub names —
        // `chat_session_reset` → `chat_reset`; `chat_show_turn_log` →
        // `chat_history`. CLAUDE.md §五 #4 forbids deprecated aliases
        // pre-v1.0).
        assert!(names.contains(&"ccteam__chat_send_input"));
        assert!(names.contains(&"ccteam__chat_history"));
        assert!(names.contains(&"ccteam__chat_reset"));
        // Removed in V0.6.5 F146 / F147 — no deprecated alias.
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
    fn send_input_writes_envelope_with_router_yaml_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let body = dispatch(
            &p,
            "ccteam__chat_send_input",
            &json!({
                "workflow_slug": "demo",
                "role": "helper",
                "content": "hello world",
            }),
        )
        .unwrap()
        .expect("matched our tool");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        let path = parsed["mailbox_path"].as_str().unwrap();
        let disk = std::fs::read_to_string(path).unwrap();
        // Envelope round-trips through inbound::parse_envelope (same
        // wire format the daemon's drain_inboxes / mpsc fast-path uses).
        let env = ccteam_imd::inbound::parse_envelope(&disk).unwrap();
        assert_eq!(env.payload, "hello world");
        assert_eq!(env.platform, "mcp");
        assert!(path.contains("demo/.ccteam/chat/helper/inbox/msg-"));
        assert!(path.ends_with(".md"));
    }

    #[test]
    fn reset_writes_signal_file_at_documented_path() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let body = dispatch(
            &p,
            "ccteam__chat_reset",
            &json!({ "workflow_slug": "demo", "role": "helper" }),
        )
        .unwrap()
        .expect("matched our tool");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        let sig = parsed["signal_path"].as_str().unwrap();
        assert!(std::path::Path::new(sig).exists());
        assert!(
            sig.ends_with("demo/.ccteam/chat/helper/signals/reset.signal"),
            "unexpected signal path: {sig}"
        );
    }

    #[test]
    fn history_returns_empty_when_turns_jsonl_missing() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let body = dispatch(
            &p,
            "ccteam__chat_history",
            &json!({ "workflow_slug": "demo", "role": "helper" }),
        )
        .unwrap()
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["turns"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn history_tails_assistant_rows_only_by_default() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        // Seed turns.jsonl with mixed user+assistant rows.
        let dir = p.projects_root.join("demo/.ccteam/chat/helper");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for i in 0..5 {
            body.push_str(&format!(
                "{{\"turn_id\":\"t{i}\",\"ts\":\"2026-05-24T00:00:00Z\",\"vendor\":\"claude\",\"role\":\"helper\",\"user\":\"u{i}\",\"assistant\":\"a{i}\"}}\n"
            ));
        }
        std::fs::write(dir.join("turns.jsonl"), body).unwrap();

        let body = dispatch(
            &p,
            "ccteam__chat_history",
            &json!({ "workflow_slug": "demo", "role": "helper", "n": 3 }),
        )
        .unwrap()
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        let turns = parsed["turns"].as_array().unwrap();
        // Only assistant rows (default include_user:false) → tail-3 = a2..a4.
        assert_eq!(turns.len(), 3);
        for t in turns {
            assert_eq!(t["role"], "assistant");
        }
        assert_eq!(turns[0]["content"], "a2");
        assert_eq!(turns[2]["content"], "a4");
    }

    #[test]
    fn history_include_user_interleaves_both_sides() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let dir = p.projects_root.join("demo/.ccteam/chat/helper");
        std::fs::create_dir_all(&dir).unwrap();
        let body = "{\"turn_id\":\"t0\",\"ts\":\"2026-05-24T00:00:00Z\",\"vendor\":\"claude\",\"role\":\"helper\",\"user\":\"u0\",\"assistant\":\"a0\"}\n";
        std::fs::write(dir.join("turns.jsonl"), body).unwrap();

        let body = dispatch(
            &p,
            "ccteam__chat_history",
            &json!({ "workflow_slug": "demo", "role": "helper", "include_user": true }),
        )
        .unwrap()
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        let turns = parsed["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[0]["content"], "u0");
        assert_eq!(turns[1]["role"], "assistant");
        assert_eq!(turns[1]["content"], "a0");
    }

    #[test]
    fn send_input_rejects_path_injection_slug() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let err = dispatch(
            &p,
            "ccteam__chat_send_input",
            &json!({
                "workflow_slug": "../escape",
                "role": "helper",
                "content": "x",
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("illegal"));
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
