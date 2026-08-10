//! `Mcp-Session-Id` bindings — one identity per hand-started vendor PROCESS.
//!
//! An enrollment credential says whose a vendor config is
//! ([`ccteam_core::enroll`]); it cannot say which process is calling, because
//! every process that vendor starts reads the same file. The MCP Streamable
//! HTTP transport already solves exactly this: the server may answer
//! `initialize` with an `Mcp-Session-Id`, and a conforming client echoes it on
//! every subsequent request. Real-machine verified against all five vendors
//! (claude / codex / grok / opencode / kimi — each echoes it on
//! `notifications/initialized`, the SSE GET, `tools/list` and `tools/call`;
//! codex and grok also send a closing `DELETE`).
//!
//! So the daemon issues one binding per `initialize` and keeps the identity on
//! the server side:
//!
//! ```text
//! initialize + enroll bearer  ->  new binding (+ Mcp-Session-Id)
//! tools/call + enroll bearer + Mcp-Session-Id  ->  that binding's identity
//! DELETE / expiry  ->  binding gone; the next call must initialize again
//! ```
//!
//! Two rules make the id safe to hand back to a client:
//!
//! 1. **The id is not a credential.** Every request must ALSO carry the enroll
//!    bearer, and [`NativeBindings::resolve`] only answers when the binding was
//!    opened by that same credential. A leaked id replays as nothing.
//! 2. **Nothing is inferred.** The project comes from the credential's scope or
//!    an explicit later bind — never from a client-supplied path, the peer
//!    address, or "the most recent project".
//!
//! Deliberately in-memory: a binding maps to a live client process, and a
//! daemon restart already ended every one of those conversations. A stale id
//! resolves to nothing and the client re-initializes, which is the transport's
//! own recovery path.

use std::collections::BTreeMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};

/// One hand-started vendor process, as ccteam sees it.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeBinding {
    /// Server-issued opaque id, echoed by the client on every request.
    pub mcp_session_id: String,
    /// Enrollment record that opened this binding — the only credential that
    /// may use it.
    pub enroll_id: String,
    /// ccteam identity the credential speaks for; sessions inherit it.
    pub owner: String,
    /// Project this client is bound to. `None` = the credential named none and
    /// the client has not picked one yet.
    pub project: Option<String>,
    /// The ledger node representing this client, once created. It is what makes
    /// the client a delegation PARENT rather than an anonymous caller.
    pub sid: Option<String>,
    /// The node's per-session secret — the server side of its identity.
    ///
    /// It is minted here and NEVER sent to the client: the client authenticates
    /// with its enrollment bearer plus the id, and the daemon speaks for it
    /// internally with this pair. That is what lets an enrolled client flow
    /// through the existing managed-session principal gate unchanged. Non-empty
    /// exactly when [`Self::sid`] is `Some` (both are set by
    /// [`NativeBindings::attach_session`]).
    principal_secret: String,
    /// `clientInfo` from `initialize` (`name/version`), for the console listing.
    pub client: String,
    /// When `initialize` issued this binding.
    pub created_at: DateTime<Utc>,
    /// Last request that resolved it — the liveness the idle sweep reads.
    pub last_seen_at: DateTime<Utc>,
}

impl NativeBinding {
    /// The `(sid, secret)` principal this client authenticates as, or `None`
    /// while it has no ledger node. One accessor so no caller can pair a sid with
    /// an empty secret and get a silently unauthenticated call.
    pub fn principal(&self) -> Option<(&str, &str)> {
        let sid = self.sid.as_deref()?;
        if sid.is_empty() || self.principal_secret.is_empty() {
            return None;
        }
        Some((sid, self.principal_secret.as_str()))
    }
}

/// Hand-written so the node secret cannot be printed by a `?binding` in some
/// future log line — the one property this type promises is that the secret stays
/// server-side, and a derived `Debug` would quietly break it.
impl std::fmt::Debug for NativeBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBinding")
            .field("mcp_session_id", &self.mcp_session_id)
            .field("enroll_id", &self.enroll_id)
            .field("owner", &self.owner)
            .field("project", &self.project)
            .field("sid", &self.sid)
            .field(
                "principal_secret",
                &if self.principal_secret.is_empty() {
                    "<none>"
                } else {
                    "<redacted>"
                },
            )
            .field("client", &self.client)
            .field("created_at", &self.created_at)
            .field("last_seen_at", &self.last_seen_at)
            .finish()
    }
}

/// The registry behind `Mcp-Session-Id`.
///
/// Its own lock, for the same reason [`crate::principals`] has one: credential
/// resolution happens inside a vendor's `initialize` handshake, and anything
/// that made it wait on the gateway mutex would deadlock against the very spawn
/// it is trying to serve.
#[derive(Debug, Default)]
pub struct NativeBindings {
    inner: RwLock<BTreeMap<String, NativeBinding>>,
}

impl NativeBindings {
    /// An empty registry — one per gateway, shared with the `/mcp` front door.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a binding for a fresh `initialize`. Returns the issued id.
    ///
    /// `project` is the credential's pinned scope (`None` for a user-scoped
    /// credential). Two concurrent processes holding the SAME credential get
    /// two ids, which is the whole point.
    pub fn open(
        &self,
        enroll_id: &str,
        owner: &str,
        project: Option<String>,
        client: &str,
    ) -> String {
        let now = Utc::now();
        // 128 bits from the same CSPRNG the session secrets use. Prefixed so an
        // id is recognisable in a log without being confusable with a bearer.
        let id = format!("ms_{}", ccteam_core::session_secret::mint());
        let binding = NativeBinding {
            mcp_session_id: id.clone(),
            enroll_id: enroll_id.to_string(),
            owner: owner.to_string(),
            project,
            sid: None,
            principal_secret: String::new(),
            client: client.to_string(),
            created_at: now,
            last_seen_at: now,
        };
        if let Ok(mut map) = self.inner.write() {
            map.insert(id.clone(), binding);
        }
        id
    }

    /// Resolve `(Mcp-Session-Id, enroll_id)` to the bound identity, refreshing
    /// liveness. `None` when the id is unknown OR was opened by a different
    /// credential — the caller answers both the same way, so an id cannot be
    /// probed for existence with the wrong credential.
    pub fn resolve(&self, mcp_session_id: &str, enroll_id: &str) -> Option<NativeBinding> {
        if mcp_session_id.is_empty() || enroll_id.is_empty() {
            return None;
        }
        let mut map = self.inner.write().ok()?;
        let binding = map.get_mut(mcp_session_id)?;
        if binding.enroll_id != enroll_id {
            return None;
        }
        binding.last_seen_at = Utc::now();
        Some(binding.clone())
    }

    /// Pin the project for a binding that started unbound. Refuses to move an
    /// already-bound client to a different workspace: one MCP session is one
    /// workspace for its whole life, so a mid-conversation switch cannot smuggle
    /// a caller into somebody else's project.
    pub fn bind_project(&self, mcp_session_id: &str, slug: &str) -> Result<(), String> {
        let mut map = self
            .inner
            .write()
            .map_err(|_| "binding registry poisoned".to_string())?;
        let binding = map
            .get_mut(mcp_session_id)
            .ok_or_else(|| "unknown Mcp-Session-Id".to_string())?;
        match binding.project.as_deref() {
            None => {
                binding.project = Some(slug.to_string());
                Ok(())
            }
            Some(existing) if existing == slug => Ok(()),
            Some(existing) => Err(format!(
                "this MCP session is bound to project `{existing}`; open a new session to work in `{slug}`"
            )),
        }
    }

    /// Attach the ledger node created for this client, together with the secret
    /// the daemon authenticates as that node with. Both at once: a sid without
    /// its secret is a node nothing can speak for
    /// ([`NativeBinding::principal`]).
    ///
    /// Attaches ONCE, and returns the principal actually in force. A binding that
    /// already has a node keeps it: two tool calls a client fired in parallel can
    /// both see a nodeless binding and both mint one, and overwriting would leave
    /// the loser's node parenting children the binding no longer points at. The
    /// caller compares the returned sid with the one it minted and retires its own
    /// when it lost. `None` = the id is gone (closed, or swept mid-flight).
    pub fn attach_session(
        &self,
        mcp_session_id: &str,
        sid: &str,
        secret: &str,
    ) -> Option<(String, String)> {
        let mut map = self.inner.write().ok()?;
        let binding = map.get_mut(mcp_session_id)?;
        if binding.principal().is_none() {
            binding.sid = Some(sid.to_string());
            binding.principal_secret = secret.to_string();
        }
        binding
            .principal()
            .map(|(sid, secret)| (sid.to_string(), secret.to_string()))
    }

    /// End a binding (client `DELETE`, or a sweep). Returns the node sid it
    /// held, so the caller can close that out too.
    pub fn close(&self, mcp_session_id: &str) -> Option<String> {
        let mut map = self.inner.write().ok()?;
        map.remove(mcp_session_id).and_then(|b| b.sid)
    }

    /// Every live binding, newest first — for the console and `status`.
    pub fn list(&self) -> Vec<NativeBinding> {
        let mut out: Vec<NativeBinding> = self
            .inner
            .read()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|b| std::cmp::Reverse(b.last_seen_at));
        out
    }

    /// Drop bindings idle longer than `max_idle`, returning their node sids.
    ///
    /// A hand-started agent exits without warning far more often than it sends
    /// `DELETE` (only codex and grok were observed sending one), so idle
    /// eviction is the primary reaper, not the exception.
    pub fn sweep_idle(&self, max_idle: chrono::Duration) -> Vec<String> {
        let cutoff = Utc::now() - max_idle;
        let mut closed = Vec::new();
        if let Ok(mut map) = self.inner.write() {
            let stale: Vec<String> = map
                .values()
                .filter(|b| b.last_seen_at < cutoff)
                .map(|b| b.mcp_session_id.clone())
                .collect();
            for id in stale {
                if let Some(binding) = map.remove(&id) {
                    if let Some(sid) = binding.sid {
                        closed.push(sid);
                    }
                }
            }
        }
        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_credential_two_processes_get_two_identities() {
        // The defect this whole mechanism exists to fix.
        let reg = NativeBindings::new();
        let a = reg.open("e1", "user:web-api", None, "codex/0.144");
        let b = reg.open("e1", "user:web-api", None, "codex/0.144");
        assert_ne!(a, b, "same credential, different processes, different ids");
        assert!(a.starts_with("ms_"));
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn an_id_is_not_a_credential() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", Some("alpha".into()), "grok/0.2");
        assert!(
            reg.resolve(&id, "e2").is_none(),
            "another credential must not ride a leaked id"
        );
        assert!(reg.resolve("ms_nope", "e1").is_none(), "unknown id");
        assert!(reg.resolve("", "e1").is_none());
        assert!(reg.resolve(&id, "").is_none());
        let bound = reg.resolve(&id, "e1").expect("its own credential resolves");
        assert_eq!(bound.project.as_deref(), Some("alpha"));
        assert_eq!(bound.owner, "user:web-api");
    }

    #[test]
    fn a_bound_session_cannot_be_moved_to_another_project() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", None, "claude/2.1");
        assert!(reg.bind_project(&id, "alpha").is_ok());
        assert!(
            reg.bind_project(&id, "alpha").is_ok(),
            "same slug is a no-op"
        );
        let err = reg.bind_project(&id, "beta").unwrap_err();
        assert!(err.contains("bound to project `alpha`"), "{err}");
        assert!(err.contains("beta"), "{err}");
        assert_eq!(
            reg.resolve(&id, "e1").unwrap().project.as_deref(),
            Some("alpha")
        );
        assert!(reg.bind_project("ms_unknown", "alpha").is_err());
    }

    #[test]
    fn the_node_sid_travels_with_the_binding_and_comes_back_on_close() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", Some("alpha".into()), "kimi/1");
        let before = reg.resolve(&id, "e1").unwrap();
        assert!(before.sid.is_none());
        assert!(
            before.principal().is_none(),
            "no node yet ⇒ nothing to authenticate as"
        );
        assert_eq!(
            reg.attach_session(&id, "s42", "sek"),
            Some(("s42".to_string(), "sek".to_string())),
            "the attach reports the principal in force"
        );
        let bound = reg.resolve(&id, "e1").unwrap();
        assert_eq!(bound.sid.as_deref(), Some("s42"));
        assert_eq!(bound.principal(), Some(("s42", "sek")));
        assert_eq!(reg.close(&id).as_deref(), Some("s42"));
        assert!(reg.resolve(&id, "e1").is_none(), "closed binding is gone");
        assert!(reg.close(&id).is_none(), "second close is a no-op");
    }

    /// Two tool calls a client fired in parallel can both find a nodeless binding
    /// and both mint a node. Whichever attaches first IS the client's identity —
    /// overwriting would leave the other node parenting children the binding no
    /// longer points at, which is the rootless-child defect all over again.
    #[test]
    fn a_node_attaches_once_so_a_parallel_caller_learns_which_one_won() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", None, "claude/2.1");
        assert_eq!(
            reg.attach_session(&id, "s10", "sek-a").unwrap().0,
            "s10",
            "the first attach wins"
        );
        let lost = reg.attach_session(&id, "s11", "sek-b").unwrap();
        assert_eq!(
            lost,
            ("s10".to_string(), "sek-a".to_string()),
            "the loser is TOLD which principal is in force, so it can retire its own"
        );
        assert_eq!(reg.resolve(&id, "e1").unwrap().sid.as_deref(), Some("s10"));
        assert!(
            reg.attach_session("ms_unknown", "s12", "sek").is_none(),
            "a binding that vanished mid-flight attaches nothing"
        );
    }

    /// The node secret is the one thing here that must never be handed out, and
    /// `{:?}` is the easiest way to leak it by accident.
    #[test]
    fn a_debug_dump_never_carries_the_node_secret() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", Some("alpha".into()), "codex/0.144");
        let _ = reg.attach_session(&id, "s42", "topsecretvalue");
        let dump = format!("{:?}", reg.resolve(&id, "e1").unwrap());
        assert!(!dump.contains("topsecretvalue"), "secret leaked: {dump}");
        assert!(dump.contains("<redacted>"), "{dump}");
        assert!(dump.contains("s42"), "the sid IS safe to log: {dump}");
    }

    #[test]
    fn idle_bindings_are_swept_and_report_their_nodes() {
        let reg = NativeBindings::new();
        let live = reg.open("e1", "user:web-api", None, "codex/0.144");
        let stale = reg.open("e1", "user:web-api", None, "codex/0.144");
        let _ = reg.attach_session(&stale, "s7", "sek");
        // Age the stale one past the cutoff.
        if let Ok(mut map) = reg.inner.write() {
            map.get_mut(&stale).unwrap().last_seen_at = Utc::now() - chrono::Duration::hours(2);
        }
        let closed = reg.sweep_idle(chrono::Duration::minutes(30));
        assert_eq!(closed, vec!["s7".to_string()]);
        assert!(reg.resolve(&stale, "e1").is_none());
        assert!(
            reg.resolve(&live, "e1").is_some(),
            "a live binding must survive the sweep"
        );
    }

    #[test]
    fn resolving_refreshes_liveness_so_a_busy_client_is_never_swept() {
        let reg = NativeBindings::new();
        let id = reg.open("e1", "user:web-api", None, "opencode/1.17");
        if let Ok(mut map) = reg.inner.write() {
            map.get_mut(&id).unwrap().last_seen_at = Utc::now() - chrono::Duration::hours(2);
        }
        reg.resolve(&id, "e1").expect("still resolvable");
        assert!(
            reg.sweep_idle(chrono::Duration::minutes(30)).is_empty(),
            "the resolve just now must count as activity"
        );
    }
}
