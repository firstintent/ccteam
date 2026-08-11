//! The registry behind `ccteam-sid:<sid>:<secret>` — who a managed session is
//! allowed to be, for as long as that secret is worth anything.
//!
//! **Why this is not just a field on the live session.** A session's principal
//! is minted with its sid, handed to the vendor as part of the spawn (an
//! `mcp.json`, an ACP `mcpServers[]`, a bridge env var), and USED by the vendor
//! before the spawn returns — OpenCode dials `/mcp` inside `session/new`, and
//! Pi's bridge blocks its whole `session_start` on `initialize` + `tools/list`.
//! Authority that only begins when the session lands in the gateway's live map
//! therefore arrives too late for the very handshake it was minted for:
//!
//! - OpenCode: 401 during `session/new` → its MCP client burns a 30s startup
//!   timeout → EVERY managed spawn cost half a minute.
//! - Pi: the bridge's `fetch` never returns, so `session_start` never ends, so
//!   the child never reads stdin, so the handshake times out at 30s.
//!
//! Keeping it separate buys a second, larger property: **verifying a principal
//! must not need the gateway lock.** Pi's deadlock was exactly that cycle —
//! `/new` held the gateway mutex across `start_thread`, and the bridge's `/mcp`
//! call needed the same mutex to check a secret. A credential check is a
//! read-only string comparison; giving it its own lock means no spawn path,
//! present or future, can deadlock against a vendor's tool-face handshake.
//!
//! Lifecycle mirrors the session's, one state ahead of it:
//!
//! ```text
//! plan_*    reserve(sid, secret)  → Spawning   // the vendor may DISCOVER
//! apply_*   promote(sid)          → Live       // …and now USE
//! failure / stop_session          → forget     // the secret dies with it
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;

/// How far along the session behind a principal is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalState {
    /// Minted, spawn in flight, not yet in the live map. The vendor is
    /// building its tool face right now.
    Spawning,
    /// The session is live and dispatchable.
    Live,
}

/// A verified principal: the server-side identity, never the caller's word for
/// it.
#[derive(Debug, Clone)]
pub struct PrincipalMatch {
    /// The session id, echoed back from the registry rather than trusted from
    /// the wire.
    pub sid: String,
    /// Project the session belongs to — the scope every `session_*` call is
    /// server-side clamped to.
    pub slug: String,
    /// Role label, for prompts and receipts.
    pub role: String,
    /// Delegation depth, for the A2A guardrails.
    pub depth: u32,
    /// Whether the session behind this principal is live yet.
    pub state: PrincipalState,
}

#[derive(Debug, Clone)]
struct Principal {
    secret: String,
    slug: String,
    role: String,
    depth: u32,
    state: PrincipalState,
}

/// The one place a `(sid, secret)` pair becomes an identity.
#[derive(Debug, Default)]
pub struct SessionPrincipals {
    inner: RwLock<BTreeMap<String, Principal>>,
    /// Sids whose principal has actually authenticated a request at least once.
    ///
    /// Minting a credential and handing it to a vendor does not prove the
    /// vendor USES it: a same-named MCP entry from the host's own global
    /// config can serve the session instead, and then everything still works
    /// — the session just silently speaks with somebody else's identity, so
    /// its children mount as roots and its project scope is not its own.
    /// Recording first use turns that into a fact the gateway can check
    /// instead of a failure only visible in the delegation graph days later.
    used: RwLock<BTreeSet<String>>,
}

impl SessionPrincipals {
    /// An empty registry — one per gateway, shared with every front door that
    /// has to verify a principal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint-time registration: the secret starts working NOW, at
    /// [`PrincipalState::Spawning`], because the vendor is about to use it.
    ///
    /// Idempotent per sid — a re-plan for the same sid (a `/role` switch mints
    /// a fresh secret for a session that already exists) REPLACES the record,
    /// so the superseded secret stops verifying immediately.
    pub fn reserve(&self, sid: &str, secret: &str, slug: &str, role: &str, depth: u32) {
        if sid.is_empty() || secret.is_empty() {
            return;
        }
        if let Ok(mut map) = self.inner.write() {
            map.insert(
                sid.to_string(),
                Principal {
                    secret: secret.to_string(),
                    slug: slug.to_string(),
                    role: role.to_string(),
                    depth,
                    state: PrincipalState::Spawning,
                },
            );
        }
    }

    /// The session is live: the same secret now carries full authority.
    /// `slug` / `role` are re-stamped from the applied session, which is the
    /// authority for them (a plan can be adjusted before it lands).
    pub fn promote(&self, sid: &str, secret: &str, slug: &str, role: &str, depth: u32) {
        if sid.is_empty() || secret.is_empty() {
            return;
        }
        if let Ok(mut map) = self.inner.write() {
            map.insert(
                sid.to_string(),
                Principal {
                    secret: secret.to_string(),
                    slug: slug.to_string(),
                    role: role.to_string(),
                    depth,
                    state: PrincipalState::Live,
                },
            );
        }
    }

    /// The session ended, or never began. The secret is worthless from here —
    /// called on spawn failure as well as on stop, so a failed spawn cannot
    /// leave a usable credential behind.
    pub fn forget(&self, sid: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.remove(sid);
        }
        if let Ok(mut used) = self.used.write() {
            used.remove(sid);
        }
    }

    /// Has this sid's principal ever authenticated a request?
    ///
    /// `false` after the session has had a chance to build its tool face means
    /// the credential ccteam minted for it is not the one it is calling with.
    pub fn was_used(&self, sid: &str) -> bool {
        self.used
            .read()
            .map(|used| used.contains(sid))
            .unwrap_or(false)
    }

    /// Record first use WITHOUT a wire verification — for the one caller that
    /// proves identity another way: a provenance attach (`/mcp` bound a native
    /// binding to this session because the connecting process is the session's
    /// own vendor child). The session's tool face reached it, which is exactly
    /// the fact [`Self::was_used`] exists to record; leaving it unset would
    /// fire the identity-degraded warning for a session whose identity was
    /// just repaired.
    pub fn mark_used(&self, sid: &str) {
        if sid.is_empty() {
            return;
        }
        if let Ok(mut used) = self.used.write() {
            used.insert(sid.to_string());
        }
    }

    /// The `(secret, slug)` the daemon needs to bind a native binding to this
    /// managed session when PROCESS PROVENANCE proves the caller is the
    /// session's own vendor process (`ccteam_harness::execution::vendor_pids`).
    ///
    /// The secret never crosses the wire on this path — it moves from one
    /// daemon-internal registry (here) to another (the binding), the same trip
    /// it makes when a scope-pinned enrollment mints a ledger node. `None` for
    /// an unknown or already-forgotten sid, which is what makes pid reuse
    /// harmless: a recycled pid can only attach to a session that is still
    /// alive to be attached to.
    pub fn credential_for_managed_attach(&self, sid: &str) -> Option<(String, String)> {
        let map = self.inner.read().ok()?;
        let principal = map.get(sid)?;
        if principal.secret.is_empty() {
            return None;
        }
        Some((principal.secret.clone(), principal.slug.clone()))
    }

    /// Resolve `(sid, secret)` to an identity. Constant-time secret compare;
    /// an unknown sid and a wrong secret are indistinguishable to the caller.
    ///
    /// A successful verification also records first use (see [`Self::used`]).
    pub fn verify(&self, sid: &str, presented_secret: &str) -> Option<PrincipalMatch> {
        if sid.is_empty() || presented_secret.is_empty() {
            return None;
        }
        let matched = {
            let map = self.inner.read().ok()?;
            let principal = map.get(sid)?;
            if principal.secret.is_empty()
                || !ccteam_core::session_secret::ct_eq(&principal.secret, presented_secret)
            {
                return None;
            }
            PrincipalMatch {
                sid: sid.to_string(),
                slug: principal.slug.clone(),
                role: principal.role.clone(),
                depth: principal.depth,
                state: principal.state,
            }
        };
        // Read-only on the hot path: only the FIRST verification per sid takes
        // the write lock.
        if !self.was_used(sid) {
            if let Ok(mut used) = self.used.write() {
                used.insert(sid.to_string());
            }
        }
        Some(matched)
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }
}

/// Whether a principal in this state may invoke a tool, as opposed to merely
/// discovering which tools exist.
///
/// A spawning session is not yet a session: it has no live thread, nothing can
/// be dispatched to it, and it must not be able to spawn children or stop
/// anybody. It only needs `initialize` + `tools/list` to finish building its
/// tool face — so that is all it gets, and the window where a secret exists
/// for a session that may never come to life is closed by construction rather
/// than by hoping the cleanup ran.
pub fn may_invoke_tools(state: PrincipalState) -> bool {
    matches!(state, PrincipalState::Live)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_principal_verifies_before_the_session_is_live() {
        // The whole point: the vendor dials `/mcp` DURING spawn.
        let reg = SessionPrincipals::new();
        reg.reserve("s1", "sek", "alpha", "cto", 0);
        let m = reg
            .verify("s1", "sek")
            .expect("reserved principal verifies");
        assert_eq!(m.state, PrincipalState::Spawning);
        assert_eq!(m.slug, "alpha");
        assert!(
            !may_invoke_tools(m.state),
            "a session that does not exist yet must not be able to act"
        );
    }

    #[test]
    fn promote_grants_tool_authority_and_forget_revokes_everything() {
        let reg = SessionPrincipals::new();
        reg.reserve("s1", "sek", "alpha", "cto", 0);
        reg.promote("s1", "sek", "alpha", "reviewer", 2);
        let m = reg.verify("s1", "sek").expect("live principal verifies");
        assert_eq!(m.state, PrincipalState::Live);
        assert_eq!(m.role, "reviewer", "apply is the authority for role");
        assert_eq!(m.depth, 2);
        assert!(may_invoke_tools(m.state));

        reg.forget("s1");
        assert!(
            reg.verify("s1", "sek").is_none(),
            "a stopped session's secret must die with it"
        );
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn a_failed_spawn_leaves_no_usable_credential() {
        let reg = SessionPrincipals::new();
        reg.reserve("s7", "sek", "alpha", "cto", 0);
        reg.forget("s7"); // what the spawn-failure path does
        assert!(reg.verify("s7", "sek").is_none());
    }

    #[test]
    fn wrong_secret_and_unknown_sid_are_both_rejected() {
        let reg = SessionPrincipals::new();
        reg.reserve("s1", "sek", "alpha", "cto", 0);
        assert!(reg.verify("s1", "nope").is_none());
        assert!(reg.verify("s2", "sek").is_none());
        assert!(reg.verify("s1", "").is_none());
        assert!(reg.verify("", "sek").is_none());
    }

    /// First use is what distinguishes "ccteam minted a credential" from "the
    /// session is actually calling with it" — the whole point of the flag.
    #[test]
    fn first_use_is_recorded_only_on_a_successful_verify() {
        let reg = SessionPrincipals::new();
        reg.promote("s1", "sek", "alpha", "cto", 0);
        assert!(!reg.was_used("s1"), "minted is not used");

        assert!(reg.verify("s1", "nope").is_none());
        assert!(!reg.was_used("s1"), "a REJECTED attempt is not use");

        assert!(reg.verify("s1", "sek").is_some());
        assert!(reg.was_used("s1"));

        // A sid that ends and is reused must not inherit the old verdict.
        reg.forget("s1");
        assert!(!reg.was_used("s1"));
    }

    /// A provenance attach proves the tool face reached the session without a
    /// wire verification — it must count as use, and it must be able to read
    /// the credential it re-binds.
    #[test]
    fn provenance_attach_reads_the_credential_and_counts_as_use() {
        let reg = SessionPrincipals::new();
        reg.promote("s5", "sek", "alpha", "", 0);
        assert_eq!(
            reg.credential_for_managed_attach("s5"),
            Some(("sek".to_string(), "alpha".to_string()))
        );
        assert!(!reg.was_used("s5"));
        reg.mark_used("s5");
        assert!(reg.was_used("s5"));

        // Forgotten (stopped) session: nothing to attach to — pid reuse
        // cannot resurrect authority.
        reg.forget("s5");
        assert!(reg.credential_for_managed_attach("s5").is_none());
        assert!(!reg.was_used("s5"), "use dies with the principal");
    }

    /// A `/role` switch mints a fresh secret for an existing sid. The old one
    /// must stop working the moment the new one is registered.
    #[test]
    fn re_reserving_a_sid_supersedes_the_previous_secret() {
        let reg = SessionPrincipals::new();
        reg.promote("s1", "old", "alpha", "cto", 0);
        reg.reserve("s1", "new", "alpha", "auditor", 0);
        assert!(reg.verify("s1", "old").is_none());
        assert!(reg.verify("s1", "new").is_some());
    }
}
