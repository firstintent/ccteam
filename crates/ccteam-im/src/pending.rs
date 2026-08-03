//! Pending interaction registry (v0.8.5 D3/D4/D6).
//!
//! A session can be "waiting for the user to make a choice". This holds
//! that state, keyed by a gateway-composed `(chat, session)` string,
//! single-flight: a new prompt for the same key evicts the old one. Two
//! origins:
//!
//! - [`InteractionOrigin::Directive`] — the answer re-enters
//!   `handle_directive(original directive + choice)` (adapter `NeedsChoice`).
//! - [`InteractionOrigin::External`] — the answer is sent back over a
//!   oneshot to a waiting party (the D6 `AskUserQuestion` hook blocked on
//!   the mcp.sock). Type-only in W1; W2 wires the ingress.
//!
//! **Own lock**: the daemon holds this behind its own `Arc<Mutex<..>>`,
//! separate from the gateway's lock, so a long (600s-class) External
//! await never holds a gateway lock (arch-refactor §7-1 lock discipline).
//! Keyed by `String` (not the gateway's private `ChatKey`) so this module
//! stays decoupled from the gateway internals.

use std::collections::HashMap;
use std::time::Instant;

use ccteam_harness::{ChoicePrompt, ChoiceSelection, Directive};

/// Who is waiting on the answer + how to deliver it.
pub enum InteractionOrigin {
    /// Adapter `NeedsChoice`: re-enter `handle_directive` with the choice.
    Directive {
        /// Session whose adapter produced the prompt; the re-entry target.
        session_id: String,
        /// The original directive, replayed with `choice` set.
        directive: Directive,
    },
    /// Hook / future approval: deliver over a oneshot to the waiting task
    /// (mcp.sock handler). Wired in W2 (D6); type-only in W1.
    External {
        /// One-shot back to the waiting party (the blocked hook task).
        reply: tokio::sync::oneshot::Sender<ChoiceSelection>,
    },
}

/// One outstanding choice.
pub struct PendingInteraction {
    /// The prompt shown to the user (carries the option list + token).
    pub prompt: ChoicePrompt,
    /// Who is waiting + how to deliver the answer.
    pub origin: InteractionOrigin,
    /// When this prompt lapses (TTL); past this it is denied / dropped.
    pub expires_at: Instant,
    /// v0.8.22 P1 (review §3.1-3) — the gateway session sid (`s{n}`) this
    /// prompt belongs to, when the caller knows it. `None` by default
    /// (`register` never sets it, to avoid a signature break for the many
    /// existing callers); the HITL approval flow
    /// ([`crate::hitl::ask_permission`] / `ccteam-cli`'s
    /// `execute_permission_ask`) calls [`PendingInteractions::tag_sid`]
    /// right after `register` so a web SSE reconnect can re-seed a still-
    /// outstanding approval for that sid (see
    /// [`PendingInteractions::pending_for_sid`]).
    pub sid: Option<String>,
}

/// Registry of outstanding choices, single-flight per key.
#[derive(Default)]
pub struct PendingInteractions {
    map: HashMap<String, PendingInteraction>,
}

impl PendingInteractions {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a prompt for `key`, evicting + returning any prior pending
    /// for the same key (single-flight). The caller resolves the evicted
    /// one per its origin (External → deny-with-reason; Directive → drop).
    pub fn register(
        &mut self,
        key: String,
        prompt: ChoicePrompt,
        origin: InteractionOrigin,
        expires_at: Instant,
    ) -> Option<PendingInteraction> {
        self.map.insert(
            key,
            PendingInteraction {
                prompt,
                origin,
                expires_at,
                sid: None,
            },
        )
    }

    /// Tag an already-[`register`](Self::register)ed pending with the
    /// gateway sid it belongs to (v0.8.22 P1). Best-effort: a `key` that
    /// isn't (or is no longer) registered is a silent no-op — the caller
    /// just loses the reseed-on-reconnect affordance for that one prompt,
    /// never a hard failure.
    pub fn tag_sid(&mut self, key: &str, sid: String) {
        if let Some(p) = self.map.get_mut(key) {
            p.sid = Some(sid);
        }
    }

    /// Find the (still outstanding) pending interaction tagged with `sid`
    /// (v0.8.22 P1 — SSE reconnect reseed, review §3.1-3): a fresh page
    /// load or a reconnect during an outstanding HITL approval must render
    /// it again, not just live-stream events going forward. `None` when
    /// nothing pending is tagged with this sid (either nothing pending, or
    /// a prompt whose flow never calls [`Self::tag_sid`] — e.g. a Directive
    /// `/model`-style choice, out of scope for this fix). Scans linearly;
    /// the registry is small (one prompt per blocked turn in practice).
    pub fn pending_for_sid(&self, sid: &str) -> Option<&PendingInteraction> {
        self.map.values().find(|p| p.sid.as_deref() == Some(sid))
    }

    /// Peek the prompt for `key` without removing it (idx→id reverse
    /// lookup needs the option list before committing to take).
    pub fn prompt_for(&self, key: &str) -> Option<&ChoicePrompt> {
        self.map.get(key).map(|p| &p.prompt)
    }

    /// Is there an outstanding prompt for this key?
    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Take (remove) the pending for `key` iff its prompt token matches
    /// `token` — a click on an expired/replaced prompt must not resolve
    /// the current one.
    pub fn take_matching(&mut self, key: &str, token: &str) -> Option<PendingInteraction> {
        match self.map.get(key) {
            Some(p) if p.prompt.token == token => self.map.remove(key),
            _ => None,
        }
    }

    /// Take (remove) the pending for `key` regardless of token (a numeric
    /// short-reply targets the single current prompt, which carries the
    /// token implicitly).
    pub fn take(&mut self, key: &str) -> Option<PendingInteraction> {
        self.map.remove(key)
    }

    /// Take (remove) the pending whose prompt `token` matches, scanning all
    /// keys (v0.8.5 D6). The D6 ingress is token-only: a hook-minted
    /// `interaction/ask` prompt is registered under the token as its own key,
    /// and the inbound callback `"{token}:{idx}"` resolves it globally —
    /// gateway callbacks are unified onto this path so a click resolves by
    /// token regardless of which (chat, session) the registration key encoded.
    pub fn take_by_token(&mut self, token: &str) -> Option<PendingInteraction> {
        let key = self
            .map
            .iter()
            .find(|(_, p)| p.prompt.token == token)
            .map(|(k, _)| k.clone())?;
        self.map.remove(&key)
    }

    /// Peek the prompt whose `token` matches, scanning all keys (v0.8.5 D6).
    /// The idx→id reverse lookup needs the option list before committing to
    /// [`Self::take_by_token`].
    pub fn prompt_by_token(&self, token: &str) -> Option<&ChoicePrompt> {
        self.map
            .values()
            .find(|p| p.prompt.token == token)
            .map(|p| &p.prompt)
    }

    /// Take the outstanding free-text dialog for a ccteam sid. Empty options
    /// distinguish input/editor prompts from button choices; routing is by the
    /// ccteam sid tag, never a vendor-native session id.
    pub fn take_free_text_for_sid(&mut self, sid: &str) -> Option<PendingInteraction> {
        let key = self
            .map
            .iter()
            .find(|(_, pending)| {
                pending.sid.as_deref() == Some(sid) && pending.prompt.options.is_empty()
            })
            .map(|(key, _)| key.clone())?;
        self.map.remove(&key)
    }

    /// Remove every outstanding interaction for one ccteam sid. Dropping an
    /// External origin closes its oneshot, making teardown fail closed instead
    /// of leaving an adapter child blocked forever.
    pub fn drain_sid(&mut self, sid: &str) -> Vec<PendingInteraction> {
        let keys = self
            .map
            .iter()
            .filter(|(_, pending)| pending.sid.as_deref() == Some(sid))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| self.map.remove(&key))
            .collect()
    }

    /// Remove + return every entry whose `expires_at <= now`. Returned so
    /// External origins can be denied-with-reason rather than silently
    /// dropped.
    pub fn drain_expired(&mut self, now: Instant) -> Vec<PendingInteraction> {
        let expired: Vec<String> = self
            .map
            .iter()
            .filter(|(_, p)| p.expires_at <= now)
            .map(|(k, _)| k.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|k| self.map.remove(&k))
            .collect()
    }

    /// Count of outstanding prompts (test/observability).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no prompts are outstanding.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};
    use std::time::Duration;

    fn prompt(token: &str) -> ChoicePrompt {
        ChoicePrompt {
            token: token.to_string(),
            title: "pick".to_string(),
            options: vec![ChoiceOption {
                id: "a".into(),
                label: "A".into(),
            }],
            multi: false,
        }
    }

    fn directive_origin() -> InteractionOrigin {
        InteractionOrigin::Directive {
            session_id: "s1".to_string(),
            directive: Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        }
    }

    #[test]
    fn take_matching_enforces_token() {
        let mut p = PendingInteractions::new();
        let exp = Instant::now() + Duration::from_secs(60);
        assert!(p
            .register("k".into(), prompt("t1"), directive_origin(), exp)
            .is_none());
        assert!(p.has("k"));
        // A click on a stale token must not resolve the current prompt.
        assert!(p.take_matching("k", "nope").is_none());
        assert!(p.has("k"));
        // The matching token takes it.
        assert!(p.take_matching("k", "t1").is_some());
        assert!(!p.has("k"));
    }

    #[test]
    fn register_is_single_flight() {
        let mut p = PendingInteractions::new();
        let exp = Instant::now() + Duration::from_secs(60);
        p.register("k".into(), prompt("old"), directive_origin(), exp);
        let evicted = p.register("k".into(), prompt("new"), directive_origin(), exp);
        assert!(evicted.is_some(), "second register evicts the first");
        assert_eq!(p.len(), 1);
        assert_eq!(p.prompt_for("k").unwrap().token, "new");
    }

    #[test]
    fn drain_expired_returns_lapsed_only() {
        let mut p = PendingInteractions::new();
        let now = Instant::now();
        p.register(
            "fresh".into(),
            prompt("f"),
            directive_origin(),
            now + Duration::from_secs(60),
        );
        p.register(
            "stale".into(),
            prompt("s"),
            directive_origin(),
            now - Duration::from_secs(1),
        );
        let drained = p.drain_expired(now);
        assert_eq!(drained.len(), 1);
        assert!(p.has("fresh"));
        assert!(!p.has("stale"));
    }

    #[test]
    fn external_origin_delivers_over_oneshot() {
        let mut p = PendingInteractions::new();
        let (tx, mut rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        let exp = Instant::now() + Duration::from_secs(60);
        p.register(
            "k".into(),
            prompt("t"),
            InteractionOrigin::External { reply: tx },
            exp,
        );
        let taken = p.take("k").expect("present");
        match taken.origin {
            InteractionOrigin::External { reply } => reply
                .send(ChoiceSelection {
                    token: "t".into(),
                    ids: vec!["a".into()],
                    free_text: None,
                })
                .expect("receiver alive"),
            _ => panic!("expected External origin"),
        }
        assert_eq!(rx.try_recv().unwrap().ids, vec!["a".to_string()]);
    }

    /// v0.8.22 P1 (review §3.1-3) — `register` never tags a sid (no
    /// signature break for existing callers); `tag_sid` is the opt-in step
    /// the HITL approval flow takes right after registering.
    #[test]
    fn register_leaves_sid_unset_until_tagged() {
        let mut p = PendingInteractions::new();
        let exp = Instant::now() + Duration::from_secs(60);
        p.register("k".into(), prompt("t1"), directive_origin(), exp);
        assert!(p.pending_for_sid("s1").is_none());
        p.tag_sid("k", "s1".to_string());
        assert!(p.pending_for_sid("s1").is_some());
    }

    /// `pending_for_sid` finds a tagged prompt regardless of what its
    /// registry key looks like (a token, a composite chat key, ...) — it
    /// scans by the `sid` field, not the key.
    #[test]
    fn pending_for_sid_finds_the_tagged_entry() {
        let mut p = PendingInteractions::new();
        let exp = Instant::now() + Duration::from_secs(60);
        p.register("ptok1".into(), prompt("ptok1"), directive_origin(), exp);
        p.register("ptok2".into(), prompt("ptok2"), directive_origin(), exp);
        p.tag_sid("ptok2", "s7".to_string());

        assert!(
            p.pending_for_sid("s1").is_none(),
            "untagged entries don't match"
        );
        let found = p.pending_for_sid("s7").expect("tagged entry found");
        assert_eq!(found.prompt.token, "ptok2");
    }

    /// A `tag_sid` for a key that was never registered (or already taken) is
    /// a harmless no-op — never panics.
    #[test]
    fn tag_sid_on_missing_key_is_a_noop() {
        let mut p = PendingInteractions::new();
        p.tag_sid("nope", "s1".to_string());
        assert!(p.pending_for_sid("s1").is_none());
    }

    #[test]
    fn pi_free_text_and_teardown_are_sid_scoped() {
        let mut p = PendingInteractions::new();
        let exp = Instant::now() + Duration::from_secs(60);
        let mut text_prompt = prompt("text");
        text_prompt.options.clear();
        p.register("text".into(), text_prompt, directive_origin(), exp);
        p.tag_sid("text", "s1".to_string());
        p.register("choice".into(), prompt("choice"), directive_origin(), exp);
        p.tag_sid("choice", "s1".to_string());
        p.register("other".into(), prompt("other"), directive_origin(), exp);
        p.tag_sid("other", "s2".to_string());

        assert_eq!(p.take_free_text_for_sid("s1").unwrap().prompt.token, "text");
        assert!(p.take_free_text_for_sid("s1").is_none());
        assert_eq!(p.drain_sid("s1").len(), 1);
        assert_eq!(p.len(), 1, "s2 remains registered");
    }
}
