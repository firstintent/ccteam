//! `session_*` MCP tools (v0.8.7 W1 — cto scheduling, B-tier).
//!
//! Tool **definitions** and the daemon-side gate live in [`ccteam_im::mcp`].
//! This module owns the **stdio-side forwarder**: inject ambient caller
//! identity and ship the call over `mcp.sock` to the daemon.
//!
//! Two caller identities (v0.9.1):
//! - **ccteam-spawned session** — env carries the `(sid, secret)` principal;
//!   forwarded as `_caller_sid`/`_caller_secret` (daemon verifies, scopes to
//!   the caller's own project, applies delegation guardrails).
//! - **plain main session** — the user's daily-driver Claude/Codex that
//!   ccteam did NOT spawn: no principal env. We prove same-user identity by
//!   reading the admin web token (`~/.ccteam/secrets/web-token`, 0600) and
//!   forwarding it as `_caller_admin_token`; the daemon verifies it
//!   constant-time and serves the call with Admin semantics (root spawn, no
//!   parent, project = explicit `project` arg or the cwd-resolved slug).
//!   Without a readable token the call soft-degrades, fail-closed.

use anyhow::Result;
use serde_json::{json, Value};

use ccteam_core::paths::CcteamPaths;

pub use ccteam_im::mcp::is_session_tool;

/// Stdio-side dispatch for a `session_*` tool: inject the ambient
/// caller identity and forward to the daemon over `mcp.sock`. Returns
/// `Ok(None)` for tools that aren't ours so `call_tool` falls through.
///
/// The forwarded request reuses the EXACT shape the daemon's
/// `execute_session_tool` expects: a `tools/call` whose `arguments` carry the
/// original args plus `_caller_slug` / `_caller_role` / `_caller_secret`
/// overwritten from the env (caller-supplied values are ignored). The secret
/// is what the daemon authenticates; it only raises the bar (not a hard
/// boundary under a single-uid model — see the module-level honest scope in
/// `ccteam_im::mcp::dispatch`).
pub async fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    if !is_session_tool(name) {
        return Ok(None);
    }
    let content = forward_session_tool(paths, name, args).await?;
    // The forwarder returns the MCP `content` array (a single text block);
    // unwrap it to the body string the group-dispatch contract expects.
    let body = content
        .first()
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    Ok(Some(body))
}

/// Forward one `session_*` tool call to the daemon, injecting the ambient
/// identity. Surfaces a structured (non-fatal) message when we're not in a
/// ccteam chat session or the daemon is unreachable, matching
/// `forward_chat_send_file`'s graceful-degrade contract.
async fn forward_session_tool(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Vec<Value>> {
    let slug = std::env::var("CCTEAM_CHAT_SLUG").unwrap_or_default();
    let role = std::env::var("CCTEAM_CHAT_ROLE").unwrap_or_default();
    // v0.8.7 review-fix (R-M1) — the per-session secret minted at spawn. The
    // daemon authenticates this `(role, secret)` pair against its session map
    // instead of trusting the plaintext role. Absent for a session spawned
    // before this change (restored, no secret) → the daemon fails the check
    // closed, which is the intended fail-safe.
    let secret = std::env::var("CCTEAM_CHAT_SECRET").unwrap_or_default();
    // v0.8.8 F1 — the caller (cto) session's own ccteam sid. Forwarded so the
    // daemon can resolve the SPECIFIC caller session post-dedup (`(slug, role)`
    // is no longer unique). Empty for a pre-F1 / restored pane.
    let caller_sid = std::env::var("CCTEAM_CHAT_SID").unwrap_or_default();
    // v0.9.0 W1 (F1) — the caller identity is now the `(sid, secret)` PRINCIPAL,
    // not `(slug, role)`. Require the principal env (a roleless session has an
    // empty ROLE but still a valid sid+secret); slug/role ride along as
    // audit labels the daemon overwrites from the resolved CallerCtx.
    //
    // v0.9.1 — no principal ≠ no access: a plain main session (the user's
    // daily-driver Claude/Codex, not spawned by ccteam) falls back to the
    // same-user admin identity (see module docs). This is what makes "call
    // codex" work from any local session instead of only from ccteam-spawned
    // ones.
    if caller_sid.is_empty() || secret.is_empty() {
        return forward_as_local_admin(paths, name, args).await;
    }
    let mut fwd_args = args.clone();
    if let Some(obj) = fwd_args.as_object_mut() {
        // Overwrite (never trust) any caller-supplied identity. The secret is
        // the authenticated component; the slug/role are inputs the daemon
        // cross-checks against the secret's session.
        obj.insert("_caller_slug".to_string(), json!(slug));
        obj.insert("_caller_role".to_string(), json!(role));
        obj.insert("_caller_secret".to_string(), json!(secret));
        obj.insert("_caller_sid".to_string(), json!(caller_sid));
    }
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": fwd_args },
    });
    let socket = ccteam_core::daemon_socket_path(paths);
    match crate::mcp_serve::forward_to_socket(&socket, &req).await {
        Ok(resp) => crate::mcp_serve::forward_outcome(&resp),
        Err(err) => Ok(vec![json!({
            "type": "text",
            "text": format!("{name} failed: daemon mcp.sock unreachable ({err})"),
        })]),
    }
}

/// v0.9.1 — main-session fallback (no `(sid, secret)` principal in the env):
/// authenticate as the machine owner via the admin web token and forward with
/// Admin semantics. See the module docs for the trust argument.
async fn forward_as_local_admin(
    paths: &CcteamPaths,
    name: &str,
    args: &Value,
) -> Result<Vec<Value>> {
    let Some(token) = read_web_token(paths) else {
        return Ok(vec![json!({
            "type": "text",
            "text": format!(
                "{name}: not in a ccteam session (CCTEAM_CHAT_SID/SECRET unset) and no admin \
                 web token readable at {} — start the daemon once (`ccteam start`) so the token \
                 exists, then retry",
                paths.web_token_path().display()
            ),
        })]);
    };
    let mut fwd_args = args.clone();
    if let Some(obj) = fwd_args.as_object_mut() {
        // Never trust caller-supplied identity args on this path either.
        for k in [
            "_caller_sid",
            "_caller_secret",
            "_caller_role",
            "_caller_depth",
        ] {
            obj.remove(k);
        }
        obj.insert("_caller_admin_token".to_string(), json!(token));
        // Default the target project to the one the session is sitting in
        // (registry prefix-match on cwd) unless the model named one.
        let explicit = obj
            .get("project")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        if !explicit {
            if let Some(slug) = project_slug_for_cwd(paths) {
                obj.insert("_caller_slug".to_string(), json!(slug));
            }
        }
    }
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": fwd_args },
    });
    let socket = ccteam_core::daemon_socket_path(paths);
    match crate::mcp_serve::forward_to_socket(&socket, &req).await {
        Ok(resp) => crate::mcp_serve::forward_outcome(&resp),
        Err(err) => Ok(vec![json!({
            "type": "text",
            "text": format!("{name} failed: daemon mcp.sock unreachable ({err})"),
        })]),
    }
}

/// Read the admin web token (`~/.ccteam/secrets/web-token`). `None` when the
/// file is missing/empty — the caller then soft-degrades, fail-closed.
fn read_web_token(paths: &CcteamPaths) -> Option<String> {
    let token = std::fs::read_to_string(paths.web_token_path()).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Resolve the project slug for the current working directory: the registry
/// entry whose path is the LONGEST prefix of cwd (so a subdirectory of a
/// registered repo still resolves). `None` when cwd is outside every
/// registered project.
fn project_slug_for_cwd(paths: &CcteamPaths) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let cfg = ccteam_core::config::load(&paths.root).ok()?;
    slug_for_dir(&cwd, &cfg.projects)
}

/// Pure core of [`project_slug_for_cwd`] (unit-testable without env/cwd).
fn slug_for_dir(
    dir: &std::path::Path,
    projects: &[ccteam_core::config::ProjectEntry],
) -> Option<String> {
    projects
        .iter()
        .filter_map(|p| {
            let root = p.path.canonicalize().unwrap_or_else(|_| p.path.clone());
            dir.starts_with(&root)
                .then(|| (root.components().count(), p.slug.clone()))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, slug)| slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_im::mcp::{is_session_tool, session_tool_definitions, SESSION_TOOL_NAMES};

    #[test]
    fn five_session_tools_registered_with_correct_names() {
        let tools = session_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"session_spawn"));
        assert!(names.contains(&"session_dispatch"));
        assert!(names.contains(&"session_collect"));
        assert!(names.contains(&"session_list"));
        assert!(names.contains(&"session_stop"));
    }

    /// Tripwire for the cto scheduling surface.
    #[test]
    fn cto_scheduling_tools_present_in_canonical_set() {
        for needed in [
            "session_spawn",
            "session_dispatch",
            "session_collect",
            "session_list",
            "session_stop",
        ] {
            assert!(
                SESSION_TOOL_NAMES.contains(&needed),
                "the session_* scheduling tools depend on the `{needed}` tool (exposed \
                 to agents as `mcp__ccteam__{needed}`); if you removed or renamed \
                 it, update the daemon session handler — do not \
                 silently drop it"
            );
        }
    }

    #[test]
    fn definitions_match_the_name_constant() {
        let defs = session_tool_definitions();
        let mut def_names: Vec<&str> = defs.iter().map(|t| t["name"].as_str().unwrap()).collect();
        def_names.sort();
        let mut const_names: Vec<&str> = SESSION_TOOL_NAMES.to_vec();
        const_names.sort();
        assert_eq!(
            def_names, const_names,
            "SESSION_TOOL_NAMES must stay in lockstep with session_tool_definitions()"
        );
    }

    #[test]
    fn all_session_tools_carry_session_prefix() {
        for t in session_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("session_"),
                "session tool name must start with session_: {n}"
            );
        }
        for t in session_tool_definitions() {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn session_spawn_schema_carries_full_facet_set() {
        let spawn = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "session_spawn")
            .expect("session_spawn defined");
        let props = &spawn["inputSchema"]["properties"];
        // v0.9.0 W1 (G1) — vendor enum lists all FOUR harnesses.
        let vendors: Vec<&str> = props["vendor"]["enum"]
            .as_array()
            .expect("vendor enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vendors, vec!["claude", "codex", "grok", "opencode"]);
        // New facets present; protocol enum excludes terminal. v0.9.1 adds the
        // one-call spawn+dispatch trio (task / wait_seconds / notify).
        for key in [
            "model",
            "effort",
            "protocol",
            "host",
            "title",
            "task",
            "wait_seconds",
            "notify",
        ] {
            assert!(props[key].is_object(), "schema must carry `{key}`");
        }
        let protos: Vec<&str> = props["protocol"]["enum"]
            .as_array()
            .expect("protocol enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(protos, vec!["stream-json", "acp"]);
        // role is optional now → required is empty.
        let required = spawn["inputSchema"]["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "role is optional; required must be empty"
        );
    }

    #[test]
    fn is_session_tool_recognizes_group_and_rejects_others() {
        assert!(is_session_tool("session_spawn"));
        assert!(is_session_tool("session_stop"));
        assert!(!is_session_tool("ccteam__chat_register_bot"));
        assert!(!is_session_tool("ccteam__advise_vote"));
        assert!(!is_session_tool("ccteam__session_bogus"));
    }

    #[tokio::test]
    async fn dispatch_returns_none_for_foreign_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        assert!(dispatch(&paths, "ccteam__chat_register_bot", &json!({}))
            .await
            .unwrap()
            .is_none());
        assert!(dispatch(&paths, "ccteam__advise_vote", &json!({}))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn dispatch_without_ambient_identity_degrades_softly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let prev_sid = std::env::var("CCTEAM_CHAT_SID").ok();
        let prev_secret = std::env::var("CCTEAM_CHAT_SECRET").ok();
        std::env::remove_var("CCTEAM_CHAT_SID");
        std::env::remove_var("CCTEAM_CHAT_SECRET");
        let body = dispatch(&paths, "session_list", &json!({}))
            .await
            .unwrap()
            .expect("session tool matched");
        // No principal env AND no web token on this fresh root → the
        // local-admin fallback fails closed with actionable guidance.
        assert!(
            body.contains("not in a ccteam session") && body.contains("web token"),
            "expected fail-closed fallback message, got: {body}"
        );
        if let Some(v) = prev_sid {
            std::env::set_var("CCTEAM_CHAT_SID", v);
        }
        if let Some(v) = prev_secret {
            std::env::set_var("CCTEAM_CHAT_SECRET", v);
        }
    }

    // v0.9.1 — with a readable admin web token the principal-less call is
    // forwarded (Admin semantics); here the daemon socket is absent, so the
    // proof of promotion is that it got PAST identity to the transport error.
    #[tokio::test]
    async fn dispatch_without_principal_but_with_token_forwards_as_admin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("home");
        let paths = CcteamPaths {
            root: root.clone(),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets/web-token"), "tok-xyz\n").unwrap();
        let prev_sid = std::env::var("CCTEAM_CHAT_SID").ok();
        let prev_secret = std::env::var("CCTEAM_CHAT_SECRET").ok();
        std::env::remove_var("CCTEAM_CHAT_SID");
        std::env::remove_var("CCTEAM_CHAT_SECRET");
        let body = dispatch(&paths, "session_list", &json!({}))
            .await
            .unwrap()
            .expect("session tool matched");
        assert!(
            body.contains("mcp.sock unreachable"),
            "token fallback must reach the socket forward, got: {body}"
        );
        if let Some(v) = prev_sid {
            std::env::set_var("CCTEAM_CHAT_SID", v);
        }
        if let Some(v) = prev_secret {
            std::env::set_var("CCTEAM_CHAT_SECRET", v);
        }
    }

    #[test]
    fn slug_for_dir_picks_longest_registered_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outer = tmp.path().join("repo");
        let inner = outer.join("sub/crate-a");
        std::fs::create_dir_all(&inner).unwrap();
        let entry = |slug: &str, path: std::path::PathBuf| ccteam_core::config::ProjectEntry {
            slug: slug.into(),
            path,
            team: "dev".into(),
            installed_at: chrono::Utc::now(),
        };
        let projects = vec![
            entry("outer", outer.clone()),
            entry("inner", outer.join("sub")),
        ];
        let canon = inner.canonicalize().unwrap();
        assert_eq!(slug_for_dir(&canon, &projects).as_deref(), Some("inner"));
        let canon_outer = outer.canonicalize().unwrap();
        assert_eq!(
            slug_for_dir(&canon_outer, &projects).as_deref(),
            Some("outer")
        );
        assert_eq!(
            slug_for_dir(std::path::Path::new("/nowhere"), &projects),
            None
        );
    }
}
