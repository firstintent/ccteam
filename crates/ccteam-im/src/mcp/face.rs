//! Per-caller MCP tool face — what `initialize` / `tools/list` answer.
//!
//! The tool surface used to be one global list: every session, however deep in
//! a delegation tree and whatever it was hired for, paid the full schema +
//! instructions bill on its very first turn. That bill is the "ambient tax" —
//! it is charged once per session process, before the session has done
//! anything, and a leaf worker can never spend it.
//!
//! So the face is composed for the CALLER instead:
//!
//! | caller | tools |
//! |---|---|
//! | admin / user / no gateway | all of them |
//! | session, `tool_face` full, below the depth cap | status + beacon + agent + agent_read + agent_stop |
//! | session at the depth cap, or spawned `tools:"read"` | agent_read |
//! | session spawned `tools:"none"` | nothing |
//! | enrolled client with no project yet | all of them (it still has to name a workspace) |
//!
//! plus `chat_send_file` whenever the caller actually has a chat to answer
//! into. `CCTEAM_DISABLE_TOOLS` is applied last, on top of whatever the face
//! resolved to.
//!
//! **This is a listing decision, not a permission one.** `tools/call` gates are
//! untouched: a hidden tool that is hard-called still runs through the same
//! principal / ownership / driveability checks it always did, and is refused
//! exactly where it already would be. The face saves tokens; it does not carry
//! authority.

use ccteam_core::CcteamPaths;
use serde_json::Value;

use super::dispatch::{GatewayHandle, McpCaller};
use super::groups;
use super::protocol::{self, STATUS_BEACON_TOOL_NAME};

/// The identity facts `initialize.instructions` may state (declarative only —
/// ccteam writes down who you are and where you work, never how to behave).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceIdentity {
    /// A session ccteam knows by principal.
    Session {
        /// The caller's own sid.
        sid: String,
        /// The project it works in.
        slug: String,
        /// It sits at `delegation.max_depth` and cannot hire.
        depth_capped: bool,
        /// It was spawned with `tools:"none"`.
        no_tools: bool,
    },
    /// A hand-started client whose enrollment binding has no project yet.
    EnrolledUnbound {
        /// Project slugs its credential's owner can reach.
        reachable: Vec<String>,
    },
}

/// The resolved tool face for one caller, for one process lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFace {
    /// Wire tool names, in registration order.
    pub tools: Vec<&'static str>,
    /// The face carries `agent` — the orchestration paragraph applies.
    pub orchestrates: bool,
    /// The caller has a chat to send a file back to.
    pub chat_capable: bool,
    /// Identity facts for the instructions, when ccteam knows them.
    pub identity: Option<FaceIdentity>,
}

/// The full face, in registration order. Also the answer for every caller
/// ccteam cannot narrow (admin, tenant, daemonless protocol tests).
const FULL_TOOLS: &[&str] = &[
    "status",
    STATUS_BEACON_TOOL_NAME,
    "chat_send_file",
    "agent",
    "agent_read",
    "agent_stop",
];

/// The orchestrating session face: everything except `chat_send_file`, which
/// is appended only when the caller has a chat.
const ORCHESTRATOR_TOOLS: &[&str] = &[
    "status",
    STATUS_BEACON_TOOL_NAME,
    "agent",
    "agent_read",
    "agent_stop",
];

/// A leaf's face: it can see the team, and that is all.
const READ_TOOLS: &[&str] = &["agent_read"];

impl Default for ToolFace {
    fn default() -> Self {
        Self::full()
    }
}

impl ToolFace {
    /// Every tool, no identity — the answer for a caller ccteam cannot narrow.
    pub fn full() -> Self {
        Self {
            tools: FULL_TOOLS.to_vec(),
            orchestrates: true,
            chat_capable: true,
            identity: None,
        }
    }

    /// True when `name` is listed on this face.
    pub fn lists(&self, name: &str) -> bool {
        self.tools.contains(&name)
    }

    /// Drop every tool whose group `CCTEAM_DISABLE_TOOLS` names. Applied last,
    /// on top of whatever the per-caller rules resolved to, so an operator
    /// toggle never silently re-adds a tool the face withheld.
    fn apply_disabled_groups(&mut self) {
        let disabled = groups::disabled_groups_from_env();
        if disabled.is_empty() {
            return;
        }
        self.tools
            .retain(|name| match groups::group_for_tool(name) {
                Some(group) => !disabled.contains(&group),
                None => true,
            });
        self.orchestrates = self.lists("agent");
        self.chat_capable = self.chat_capable && self.lists("chat_send_file");
    }
}

/// Compose the face for one `initialize` / `tools/list`.
///
/// Reads the caller's principal + its session `meta.json`; every miss degrades
/// to the full face (a caller ccteam cannot place is not a caller it may
/// silently starve of tools — the `tools/call` gates still refuse whatever it
/// is not allowed to do).
pub async fn resolve_tool_face(
    req: &Value,
    gateway: Option<&GatewayHandle>,
    caller: &McpCaller,
    _paths: &CcteamPaths,
) -> ToolFace {
    let mut face = resolve_uncapped(req, gateway, caller).await;
    face.apply_disabled_groups();
    face
}

async fn resolve_uncapped(
    req: &Value,
    gateway: Option<&GatewayHandle>,
    caller: &McpCaller,
) -> ToolFace {
    // Admin / tenant front doors and the daemonless protocol core are people
    // (or tests), not sessions: nothing to narrow by.
    let (McpCaller::Ambient, Some(gateway)) = (caller, gateway) else {
        return ToolFace::full();
    };

    let arg = |name: &str| {
        req.pointer(&format!("/params/arguments/{name}"))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let sid = arg("_caller_sid");
    let secret = arg("_caller_secret");

    let ctx = {
        let gw = gateway.lock().await;
        gw.verify_session_principal(sid, secret)
    };
    let Some(ctx) = ctx else {
        // An enrolled binding that has not named a project yet: the route
        // hands over the slugs it may name so the instructions can list them.
        if let Some(reachable) = enroll_reachable(req) {
            return ToolFace {
                tools: FULL_TOOLS.to_vec(),
                orchestrates: true,
                // MCP is client-dial-in for a hand-started agent: ccteam holds
                // no chat of its own to answer into.
                chat_capable: false,
                identity: Some(FaceIdentity::EnrolledUnbound { reachable }),
            };
        }
        // The transport should have answered 401 already; a face is not the
        // place to invent a refusal.
        return ToolFace::full();
    };

    let context = {
        let gw = gateway.lock().await;
        gw.session_face_context(&ctx.sid)
    };
    let Some(context) = context else {
        return ToolFace::full();
    };
    let meta =
        ccteam_harness::execution::session_meta::read_session_meta(&context.project_dir, &ctx.sid)
            .ok();
    session_face(
        &ctx.sid,
        &ctx.slug,
        FaceFacts {
            requested: meta.as_ref().and_then(|meta| meta.tool_face.clone()),
            depth: ctx.depth,
            max_depth: context.max_depth,
            is_root: meta
                .as_ref()
                .map(|meta| meta.parent_sid.is_none())
                .unwrap_or(true),
            has_reply_target: context.has_reply_target,
        },
    )
}

/// Everything the face DECISION depends on, so the rules are one pure
/// function instead of five conditionals buried in an async resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FaceFacts {
    /// `meta.tool_face`: `None` = full, `"read"` / `"none"` narrow it.
    pub requested: Option<String>,
    /// The caller's own delegation depth (root = 0).
    pub depth: u32,
    /// `delegation.max_depth` — a session AT the cap cannot hire.
    pub max_depth: u32,
    /// It has no delegation parent: its own human is at the other end.
    pub is_root: bool,
    /// A chat is currently wired to it.
    pub has_reply_target: bool,
}

/// The face rules, in one place.
pub(crate) fn session_face(sid: &str, slug: &str, facts: FaceFacts) -> ToolFace {
    let requested = facts.requested.as_deref().unwrap_or("");
    let no_tools = requested == "none";
    // A session at the depth cap cannot hire, so listing `agent` would be a
    // schema it can only be refused by. Same face as an explicit `read`.
    let depth_capped = requested.is_empty() && facts.depth >= facts.max_depth;
    let can_orchestrate = requested.is_empty() && !depth_capped;
    // A root session always answers its own human; a child only has a chat if
    // one is actually wired to it right now.
    let chat_capable = !no_tools && (facts.is_root || facts.has_reply_target);

    let mut tools: Vec<&'static str> = if no_tools {
        Vec::new()
    } else if can_orchestrate {
        ORCHESTRATOR_TOOLS.to_vec()
    } else {
        READ_TOOLS.to_vec()
    };
    if chat_capable {
        tools.push("chat_send_file");
    }
    ToolFace {
        orchestrates: tools.contains(&"agent"),
        chat_capable,
        identity: Some(FaceIdentity::Session {
            sid: sid.to_string(),
            slug: slug.to_string(),
            depth_capped,
            no_tools,
        }),
        tools,
    }
}

/// `_enroll_reachable`, injected by `POST /mcp` for a binding that holds no
/// principal yet. Absent for every other caller.
fn enroll_reachable(req: &Value) -> Option<Vec<String>> {
    let raw = req.pointer("/params/arguments/_enroll_reachable")?;
    let list = raw.as_array()?;
    Some(
        list.iter()
            .filter_map(|slug| slug.as_str().map(str::to_string))
            .collect(),
    )
}

/// The `initialize` / `tools/list` answer for `face`, so the transport layer
/// never rebuilds either shape itself.
pub fn tools_for(face: &ToolFace) -> Vec<Value> {
    protocol::tool_definitions()
        .into_iter()
        .filter(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| face.lists(name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_face_lists_every_registered_tool() {
        let face = ToolFace::full();
        assert_eq!(tools_for(&face).len(), protocol::tool_definitions().len());
        assert!(face.orchestrates);
        assert!(face.chat_capable);
    }

    #[test]
    fn tools_for_preserves_registration_order_and_drops_the_rest() {
        let face = ToolFace {
            tools: vec!["agent_read"],
            orchestrates: false,
            chat_capable: false,
            identity: None,
        };
        let tools = tools_for(&face);
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["agent_read"]);
    }

    fn facts(requested: Option<&str>, depth: u32, is_root: bool, chat: bool) -> FaceFacts {
        FaceFacts {
            requested: requested.map(str::to_string),
            depth,
            max_depth: 2,
            is_root,
            has_reply_target: chat,
        }
    }

    fn names(face: &ToolFace) -> Vec<&'static str> {
        face.tools.clone()
    }

    /// An orchestrator below the depth cap sees the hiring surface; the
    /// discovery tools come first so a truncating host keeps them.
    #[test]
    fn an_orchestrator_below_the_cap_gets_the_hiring_face() {
        let face = session_face("s1", "alpha", facts(None, 0, true, true));
        assert_eq!(
            names(&face),
            vec![
                "status",
                STATUS_BEACON_TOOL_NAME,
                "agent",
                "agent_read",
                "agent_stop",
                "chat_send_file",
            ]
        );
        assert!(face.orchestrates);
        assert!(face.chat_capable);
    }

    /// AT the cap the session cannot hire, so `agent` would be a schema it can
    /// only be refused by. Same face as an explicit `tools:"read"`.
    #[test]
    fn the_depth_cap_and_an_explicit_read_face_agree() {
        let capped = session_face("s7", "alpha", facts(None, 2, false, false));
        let asked = session_face("s7", "alpha", facts(Some("read"), 0, false, false));
        assert_eq!(names(&capped), vec!["agent_read"]);
        assert_eq!(names(&asked), vec!["agent_read"]);
        assert!(!capped.orchestrates && !asked.orchestrates);
        // …but only the CAPPED one says why: an explicit `read` was the
        // parent's choice, not a limit the child ran into.
        assert_eq!(
            capped.identity,
            Some(FaceIdentity::Session {
                sid: "s7".into(),
                slug: "alpha".into(),
                depth_capped: true,
                no_tools: false,
            })
        );
        assert_eq!(
            asked.identity,
            Some(FaceIdentity::Session {
                sid: "s7".into(),
                slug: "alpha".into(),
                depth_capped: false,
                no_tools: false,
            })
        );
    }

    #[test]
    fn tools_none_lists_nothing_at_all_not_even_a_chat() {
        let face = session_face("s9", "alpha", facts(Some("none"), 0, true, true));
        assert!(names(&face).is_empty());
        assert!(!face.orchestrates);
        assert!(!face.chat_capable);
    }

    /// `chat_send_file` follows the chat, not the rank: a root always has one,
    /// a child only while something is wired to it.
    #[test]
    fn chat_send_file_follows_an_actual_chat() {
        assert!(session_face("s1", "alpha", facts(None, 0, true, false)).chat_capable);
        assert!(session_face("s2", "alpha", facts(None, 1, false, true)).chat_capable);
        let orphan = session_face("s3", "alpha", facts(None, 1, false, false));
        assert!(!orphan.chat_capable);
        assert!(!orphan.lists("chat_send_file"));
    }

    #[test]
    fn enroll_reachable_reads_the_injected_list_only() {
        let req = serde_json::json!({
            "params": { "arguments": { "_enroll_reachable": ["a", "b"] } }
        });
        assert_eq!(
            enroll_reachable(&req),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(enroll_reachable(&serde_json::json!({})), None);
    }
}
