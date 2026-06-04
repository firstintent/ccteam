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
            },
        )
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
}
