//! Ledger nodes for hand-started vendor processes.
//!
//! A client that enrolled over `POST /mcp` gets a real sid, a real `meta.json`
//! and a place in the delegation tree — the same ledger a managed session uses,
//! marked [`ManagedBy::External`]. What it does NOT get is a row in the
//! gateway's live map, because that map is the set of sessions ccteam holds a
//! thread for. Keeping the two apart is what makes every driveable surface
//! (dispatch, steer, capacity eviction, budget, tool-face prime, event pump)
//! correct without checking anything: an external client simply is not in the
//! collection they iterate.
//!
//! The unification the ledger gives is the one that matters: one sid namespace,
//! one tree, one `meta.json` shape. Taking a client over later is then a real
//! transition and not a flag flip — stop the process, resume its vendor session
//! under management (the existing `import_external_session` machinery), reuse
//! the SAME sid, and flip `managed_by`. Identity, parent edge and history all
//! survive it.

use ccteam_harness::execution::session_meta::{ManagedBy, SessionMeta, SessionOrigin, TitleSource};
use ccteam_harness::{AgentVendor, PermissionMode, SessionProtocol};

/// Best-effort vendor from the MCP `clientInfo.name` a client sends at
/// `initialize`.
///
/// Display-only for an external node: nothing is spawned or resumed off this,
/// so an unrecognised client must not be forced into a wrong vendor — the
/// caller keeps the raw string as the node's title instead. Observed names on
/// this machine: `claude-code`, `codex`, `grok`, `opencode`, `kimi-code`,
/// `ccteam-dsh-client` / `dsh`.
pub fn vendor_from_client(client_name: &str) -> Option<AgentVendor> {
    let name = client_name.trim().to_ascii_lowercase();
    // Match on the leading token so `codex-cli` / `claude-code-sdk` still land.
    let head = name.split(['/', ' ', '_']).next().unwrap_or("");
    match head {
        h if h.starts_with("claude") => Some(AgentVendor::Claude),
        h if h.starts_with("codex") => Some(AgentVendor::Codex),
        h if h.starts_with("grok") => Some(AgentVendor::Grok),
        h if h.starts_with("opencode") => Some(AgentVendor::Opencode),
        h if h.starts_with("kimi") => Some(AgentVendor::Kimi),
        h if h.starts_with("pi") => Some(AgentVendor::Pi),
        h if h.starts_with("dsh") || name.starts_with("ccteam-dsh") => Some(AgentVendor::Dsh),
        _ => None,
    }
}

/// The `meta.json` for a freshly enrolled external client.
///
/// `vendor_uuid` is empty on purpose: ccteam does not know the client's native
/// session id, and inventing one would make a later takeover resume the wrong
/// conversation. The import flow fills it when the user hands the session over.
pub fn external_node_meta(
    sid: &str,
    slug: &str,
    owner: &str,
    client: &str,
    vendor: Option<AgentVendor>,
) -> SessionMeta {
    let now = chrono::Utc::now().to_rfc3339();
    let resolved_vendor = vendor.unwrap_or(AgentVendor::Claude);
    let protocol = match resolved_vendor {
        AgentVendor::Grok | AgentVendor::Opencode | AgentVendor::Kimi | AgentVendor::Dsh => {
            SessionProtocol::Acp
        }
        AgentVendor::Claude | AgentVendor::Codex | AgentVendor::Pi => SessionProtocol::StreamJson,
    };
    let mut meta = SessionMeta {
        tool_face: None,
        managed_by: ManagedBy::External,
        stopped_at: None,
        sid: sid.to_string(),
        slug: slug.to_string(),
        // Unrecognised clients are still real callers; Claude is the display
        // fallback and the raw name rides in the title so the listing never
        // silently misattributes one vendor's work to another.
        vendor: resolved_vendor,
        protocol,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: owner.to_string(),
        vendor_uuid: String::new(),
        model: None,
        observed_model: None,
        effort: None,
        mode: None,
        host: ccteam_core::LOCAL_HOST.to_string(),
        created_at: now.clone(),
        last_active: now,
        origin: SessionOrigin::Adopted,
        title: None,
        title_source: None,
        turn_count: 0,
        cost_usd: None,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: Some("mcp-enroll".to_string()),
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    };
    // The title is the honest label for a process ccteam did not start: the
    // client's own name/version, which is all it told us.
    let label = client.trim();
    if !label.is_empty() {
        ccteam_harness::execution::session_meta::apply_title(
            &mut meta,
            label.to_string(),
            TitleSource::Vendor,
        );
    }
    meta
}

/// Why a driveable surface refuses an external sid.
///
/// One message, one place: `agent` / `agent_stop` / `agent_read` must say what
/// this session IS rather than "not found", because the caller can see it in
/// the `agent_read` roster and would otherwise read a correct refusal as a bug.
pub fn not_driveable_error(tool: &str, sid: &str) -> String {
    format!(
        "{tool}: session {sid} is a hand-started agent that enrolled with ccteam \
         (external) — ccteam holds no thread for it, so it can be a delegation \
         parent but cannot be dispatched to, collected from or stopped. Its own \
         operator drives it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names_observed_on_a_real_machine_all_map() {
        // Exactly the `clientInfo.name` values the five vendors sent during the
        // Mcp-Session-Id probe.
        for (client, want) in [
            ("claude-code", AgentVendor::Claude),
            ("codex", AgentVendor::Codex),
            ("grok", AgentVendor::Grok),
            ("opencode", AgentVendor::Opencode),
            ("kimi-code", AgentVendor::Kimi),
            ("pi", AgentVendor::Pi),
            ("dsh", AgentVendor::Dsh),
        ] {
            assert_eq!(vendor_from_client(client), Some(want), "client {client}");
        }
        // Case and suffix tolerance, because a version bump must not reclassify
        // a client as unknown.
        assert_eq!(
            vendor_from_client("Claude-Code/2.1.226"),
            Some(AgentVendor::Claude)
        );
        assert_eq!(vendor_from_client("codex_cli"), Some(AgentVendor::Codex));
        assert_eq!(vendor_from_client("dsh/0.1.0"), Some(AgentVendor::Dsh));
        assert_eq!(vendor_from_client("dsh-client"), Some(AgentVendor::Dsh));
        assert_eq!(
            vendor_from_client("ccteam-dsh-client"),
            Some(AgentVendor::Dsh)
        );
    }

    #[test]
    fn an_unknown_client_is_not_forced_into_a_vendor() {
        assert_eq!(vendor_from_client("some-random-mcp-client"), None);
        assert_eq!(vendor_from_client(""), None);
        assert_eq!(vendor_from_client("   "), None);
    }

    #[test]
    fn an_external_node_is_a_ledger_row_that_cannot_be_driven() {
        let meta = external_node_meta(
            "s42",
            "alpha",
            "user:web-api",
            "codex/0.144.3",
            Some(AgentVendor::Codex),
        );
        assert_eq!(meta.sid, "s42");
        assert_eq!(meta.slug, "alpha");
        assert_eq!(meta.owner, "user:web-api");
        assert_eq!(meta.managed_by, ManagedBy::External);
        assert!(!meta.managed_by.is_driveable());
        assert_eq!(meta.origin, SessionOrigin::Adopted);
        assert_eq!(meta.trigger.as_deref(), Some("mcp-enroll"));
        // No native session id: a takeover must resume the RIGHT conversation
        // or none at all.
        assert!(meta.vendor_uuid.is_empty());
        // The label is what the client called itself.
        assert_eq!(meta.title.as_deref(), Some("codex/0.144.3"));
        // A fresh node is a root with no accounting of its own.
        assert!(meta.parent_sid.is_none());
        assert_eq!(meta.delegation_depth, 0);
        assert_eq!(meta.turn_count, 0);
        assert!(meta.cost_usd.is_none());
        assert!(meta.tokens_total.is_none());
    }

    #[test]
    fn an_unnamed_client_still_produces_a_valid_node() {
        let meta = external_node_meta("s7", "alpha", "user:u1", "  ", None);
        assert!(meta.title.is_none(), "no fabricated label");
        assert_eq!(meta.vendor, AgentVendor::Claude, "display fallback");
        assert_eq!(meta.managed_by, ManagedBy::External);
    }

    #[test]
    fn a_dsh_external_node_reports_acp_not_the_stream_json_fallback() {
        let meta = external_node_meta(
            "s99",
            "alpha",
            "user:web-api",
            "dsh/0.1.0",
            Some(AgentVendor::Dsh),
        );
        assert_eq!(meta.vendor, AgentVendor::Dsh);
        assert_eq!(meta.protocol, SessionProtocol::Acp);
    }

    #[test]
    fn the_refusal_says_what_the_session_is_not_that_it_is_missing() {
        let msg = not_driveable_error("agent", "s42");
        assert!(msg.contains("agent"));
        assert!(msg.contains("s42"));
        assert!(msg.contains("external"));
        assert!(
            msg.contains("delegation parent"),
            "must say what it CAN do: {msg}"
        );
        assert!(
            !msg.contains("not found"),
            "a visible session must never be reported as missing: {msg}"
        );
    }
}
