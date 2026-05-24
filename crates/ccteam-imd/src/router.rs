//! @mention parsing + bot-to-bot routing with hop limits.
//!
//! IM messages route to bots via `@<handle>` patterns. We also detect
//! `@ccteam` (the administrative meta-handle) which is dispatched to
//! [`crate::nl_admin`] instead of forwarded to a tmux session.
//!
//! Bot-to-bot routing: when one chat-bot session writes a turn whose
//! content contains `@<otherbot>`, the outbound tailer routes the
//! turn back through the inbound path as a synthetic message. To
//! prevent infinite loops, every routed message carries an integer
//! `hop` counter that the router increments and rejects beyond
//! [`MAX_HOPS`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::BotRegistration;

/// Maximum forwarding hops in a bot-to-bot chain. V0.6 picks 3 to
/// match the per-bot fix-loop budget; tuning belongs in workflow.yaml
/// later.
pub const MAX_HOPS: u8 = 3;

/// Administrative meta-handle. Messages starting with `@ccteam` are
/// dispatched to the NL-admin parser instead of a bot session.
pub const ADMIN_HANDLE: &str = "ccteam";

/// One routing decision returned by [`route`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    /// Send to bot identified by `(slug, role)` (resolved from handle).
    Bot {
        /// Workflow slug from registry mapping.
        slug: String,
        /// Role within the workflow.
        role: String,
        /// Stripped payload (mentions removed).
        payload: String,
    },
    /// Dispatch to the NL admin parser.
    Admin {
        /// The text after `@ccteam ` (admin verb + args).
        verb_and_args: String,
    },
    /// `@handle` was parsed but no bot in the registry answers to it.
    /// The inbound pipeline surfaces a helpful "available bots" reply
    /// instead of a silent drop.
    UnknownHandle {
        /// The handle the user typed (without the leading `@`).
        handle: String,
    },
    /// No mention found — drop the message (don't broadcast).
    Drop {
        /// Why we dropped (for logging).
        reason: String,
    },
}

/// Per-handle resolution input. The router needs to map `@<handle>`
/// to `(slug, role)`. The daemon builds this map from the registry +
/// workflow.yaml's optional `chat_handle` mapping.
#[derive(Debug, Clone)]
pub struct HandleMap {
    /// handle (without `@`) → (slug, role)
    entries: std::collections::BTreeMap<String, (String, String)>,
}

impl HandleMap {
    /// Empty resolver.
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
        }
    }

    /// Add one mapping.
    pub fn insert(&mut self, handle: &str, slug: &str, role: &str) {
        self.entries
            .insert(handle.to_string(), (slug.to_string(), role.to_string()));
    }

    /// Resolve.
    pub fn lookup(&self, handle: &str) -> Option<(String, String)> {
        self.entries.get(handle).cloned()
    }

    /// All known handles (for tab-completion / NL admin `list`).
    pub fn handles(&self) -> BTreeSet<String> {
        self.entries.keys().cloned().collect()
    }
}

impl Default for HandleMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the **first** `@handle` in the text. Returns the handle
/// (without `@`) plus the rest of the text with that mention stripped.
fn parse_first_mention(text: &str) -> Option<(String, String)> {
    let mut handle = String::new();
    let mut start = None;
    for (i, ch) in text.char_indices() {
        if ch == '@' {
            start = Some(i);
            continue;
        }
        if start.is_some() {
            // Handle chars: alphanum / `_` / `-`.
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                handle.push(ch);
            } else {
                break;
            }
        }
    }
    let start = start?;
    if handle.is_empty() {
        return None;
    }
    let end = start + 1 + handle.len();
    let mut stripped = String::with_capacity(text.len() - handle.len() - 1);
    stripped.push_str(&text[..start]);
    if end < text.len() {
        stripped.push_str(&text[end..]);
    }
    Some((handle, stripped.trim().to_string()))
}

/// Pre-check: has the hop budget been exceeded?
pub fn within_hop_budget(hop: u8) -> bool {
    hop < MAX_HOPS
}

/// Main router entry point.
pub fn route(text: &str, handles: &HandleMap, hop: u8) -> Route {
    if !within_hop_budget(hop) {
        return Route::Drop {
            reason: format!("hop {hop} ≥ MAX_HOPS={MAX_HOPS}"),
        };
    }
    let (handle, rest) = match parse_first_mention(text) {
        Some(parts) => parts,
        None => {
            return Route::Drop {
                reason: "no @mention".into(),
            }
        }
    };
    if handle == ADMIN_HANDLE {
        return Route::Admin {
            verb_and_args: rest,
        };
    }
    match handles.lookup(&handle) {
        Some((slug, role)) => Route::Bot {
            slug,
            role,
            payload: rest,
        },
        None => Route::UnknownHandle { handle },
    }
}

/// List the effective handles bound to a `(channel, chat_id)`, sorted
/// alphabetically and deduped. Mirrors the cross-slug collision rule
/// `build_handle_map` applies: a bot whose effective handle clashes
/// with another claimant gets the `__<slug>` suffix (using `_` so
/// `parse_first_mention`'s handle-char rules accept the full token;
/// `@` would terminate parsing at the second sigil).
///
/// Used by both the unknown-handle reply path in
/// [`crate::inbound::process_inbound_admin_aware`] and the
/// `@ccteam list bots` admin keyword so the two surfaces always agree
/// on what's reachable from a given chat.
pub fn available_handles_for_chat(
    bots: &[BotRegistration],
    channel: &str,
    chat_id: &str,
) -> Vec<String> {
    // Match build_handle_map's sort + collision policy on the *full*
    // registry so the bare-handle vs `__<slug>`-suffix decision matches
    // what the router actually sees. Filter to this chat after assigning
    // handles.
    let mut sorted: Vec<&BotRegistration> = bots.iter().collect();
    sorted.sort_by(|a, b| {
        a.workflow_slug
            .cmp(&b.workflow_slug)
            .then_with(|| a.role.cmp(&b.role))
    });

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut per_bot: Vec<(&BotRegistration, String)> = Vec::with_capacity(sorted.len());
    for b in sorted {
        let base = b.effective_handle().to_string();
        let handle = if claimed.contains(&base) {
            collision_suffix(&base, &b.workflow_slug)
        } else {
            base.clone()
        };
        claimed.insert(handle.clone());
        claimed.insert(base);
        per_bot.push((b, handle));
    }

    let mut out: Vec<String> = per_bot
        .into_iter()
        .filter(|(b, _)| b.im_platform == channel && b.im_chat_id == chat_id)
        .map(|(_, handle)| handle)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Build the cross-slug collision suffix for a handle. Uses `__`
/// (double underscore) as the separator so the result stays inside
/// `parse_first_mention`'s `[a-zA-Z0-9_-]` handle charset — `@` would
/// truncate parsing at the second sigil and route the suffixed handle
/// to UnknownHandle. Slug `-` characters are preserved verbatim.
pub fn collision_suffix(base: &str, slug: &str) -> String {
    format!("{base}__{slug}")
}

/// Render the user-facing "available bots in this chat" line shared by
/// the unknown-handle reply and the `@ccteam list bots` admin keyword.
/// Falls back to a no-bots message when `available` is empty.
pub fn format_available_bots_line(available: &[String]) -> String {
    if available.is_empty() {
        return "No bots registered in this chat.".to_string();
    }
    let mentions: Vec<String> = available.iter().map(|h| format!("@{h}")).collect();
    format!("Available bots in this chat: {}", mentions.join(" "))
}

/// Render the full unknown-handle reply.
pub fn format_unknown_handle_reply(handle: &str, available: &[String]) -> String {
    format!(
        "Unknown handle '@{handle}'. {}",
        format_available_bots_line(available)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(handle: &str, slug: &str, role: &str) -> HandleMap {
        let mut m = HandleMap::new();
        m.insert(handle, slug, role);
        m
    }

    #[test]
    fn routes_to_bot() {
        let map = map_with("lead", "dev-foo", "lead");
        let r = route("@lead please plan v0.7", &map, 0);
        assert_eq!(
            r,
            Route::Bot {
                slug: "dev-foo".into(),
                role: "lead".into(),
                payload: "please plan v0.7".into(),
            }
        );
    }

    #[test]
    fn routes_admin_handle() {
        let map = HandleMap::new();
        let r = route("@ccteam status", &map, 0);
        assert_eq!(
            r,
            Route::Admin {
                verb_and_args: "status".into()
            }
        );
    }

    #[test]
    fn drops_when_no_mention() {
        let r = route("hello world", &HandleMap::new(), 0);
        assert!(matches!(r, Route::Drop { .. }));
    }

    #[test]
    fn unknown_handle_surfaces_for_reply() {
        let r = route("@ghost reply", &HandleMap::new(), 0);
        match r {
            Route::UnknownHandle { handle } => assert_eq!(handle, "ghost"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn drops_when_hop_budget_exceeded() {
        let map = map_with("lead", "dev-foo", "lead");
        let r = route("@lead loop", &map, MAX_HOPS);
        assert!(matches!(r, Route::Drop { .. }));
    }

    #[test]
    fn parses_first_mention_only() {
        let (h, rest) = parse_first_mention("@a then @b").unwrap();
        assert_eq!(h, "a");
        assert!(rest.contains("@b"));
    }

    #[test]
    fn handles_alphanum_and_dash_underscore() {
        let (h, _) = parse_first_mention("@bot_1-foo hi").unwrap();
        assert_eq!(h, "bot_1-foo");
    }

    #[test]
    fn ignores_bare_at_symbol() {
        // `@ ` (no handle chars) returns None.
        assert!(parse_first_mention("@ hi").is_none());
    }
}
