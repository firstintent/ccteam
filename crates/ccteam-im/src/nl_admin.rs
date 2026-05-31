//! `@ccteam <NL admin>` command parser + executor.
//!
//! V0.6.1 F129: the daemon turns `@ccteam <verb> ...` mentions in
//! group chats into administrative actions (pause / resume / list /
//! cost / stop-everything) without burning a hop on the bot-to-bot
//! budget. The parser stays lexical — no LLM dispatch on the hot
//! path. The full free-form NL interpretation lands later via the
//! meta-agent `ccteam-control` skill; the daemon-side keyword path
//! covers the 5 acceptance verbs the user-manual §3.2 advertises.
//!
//! Dangerous commands (`stop everything` / `kill all`) two-phase via
//! [`AdminExecutor`]: the first message acks with a CONFIRM prompt,
//! and only a literal `CONFIRM` reply from the same `reply_target`
//! within the TTL actually fires the action.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::list_bots;
use crate::supervisor::{DRAIN_SIGNAL, SHUTDOWN_SIGNAL};
use crate::BotRegistration;

/// Parsed admin command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdminCmd {
    /// `@ccteam status` — print daemon + bot status.
    Status,
    /// `@ccteam list` / `@ccteam ls` — list every registered bot
    /// globally. The per-chat scoped form is [`AdminCmd::ListHere`].
    List,
    /// `@ccteam list bots` / `@ccteam bots` / `@ccteam who` — list the
    /// bots reachable from *this* chat (filtered by `(channel,
    /// chat_id)`). Mirrors the available-bot list the router-level
    /// unknown-handle reply renders so both surfaces stay consistent.
    ListHere,
    /// `@ccteam pause <slug>` (all roles) or `@ccteam pause
    /// <slug>/<role>` — write `signals/drain.signal`.
    Pause {
        /// Project slug.
        slug: String,
        /// Role name; `None` → pause every role under `slug`.
        role: Option<String>,
    },
    /// `@ccteam resume <slug>` / `<slug>/<role>` — remove drain signal.
    Resume {
        /// Project slug.
        slug: String,
        /// Role name; `None` → resume every role under `slug`.
        role: Option<String>,
    },
    /// `@ccteam stop <slug>` / `<slug>/<role>` — write
    /// `signals/shutdown.signal` for that bot (single-target stop).
    Stop {
        /// Project slug.
        slug: String,
        /// Role name; `None` → stop every role under `slug`.
        role: Option<String>,
    },
    /// `@ccteam cost today` / `@ccteam cost <slug>` — 24h cost summary.
    CostToday {
        /// Optional slug filter. `None` → aggregate across registry.
        slug: Option<String>,
    },
    /// `@ccteam stop everything` / `@ccteam kill all` — dangerous, two
    /// phase: prompts for `CONFIRM` before actually shutting every
    /// registered bot down.
    StopEverything,
    /// Literal `CONFIRM` (case-insensitive) — second leg of a
    /// dangerous command's two-phase flow.
    Confirm,
    /// `@ccteam help` / `@ccteam ?` — print help.
    Help,
    /// Unparsable input.
    Unknown {
        /// Original input for echoback.
        raw: String,
    },
}

/// Parse `<verb> [<args...>]`.
pub fn parse(input: &str) -> AdminCmd {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return AdminCmd::Help;
    }
    // Confirm literal — case-insensitive standalone token.
    if trimmed.eq_ignore_ascii_case("confirm") {
        return AdminCmd::Confirm;
    }
    let lower = trimmed.to_ascii_lowercase();

    // Multi-word verbs first (longest match wins).
    if lower == "list bots" || lower == "bots" || lower == "who" {
        return AdminCmd::ListHere;
    }
    if lower == "list" || lower == "ls" {
        return AdminCmd::List;
    }
    if lower == "stop everything" || lower == "kill all" || lower == "stop all" {
        return AdminCmd::StopEverything;
    }
    if lower == "cost today" || lower == "cost" {
        return AdminCmd::CostToday { slug: None };
    }
    if let Some(rest) = lower.strip_prefix("cost ") {
        let arg = rest.trim();
        // `cost today` already handled above; any other arg is a slug.
        if arg == "today" {
            return AdminCmd::CostToday { slug: None };
        }
        if !arg.is_empty() {
            return AdminCmd::CostToday {
                slug: Some(arg.to_string()),
            };
        }
    }

    let mut parts = trimmed.split_whitespace();
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();
    match verb.as_str() {
        "status" => AdminCmd::Status,
        "help" | "?" => AdminCmd::Help,
        "pause" | "resume" | "stop" => {
            let target = rest.first().copied().unwrap_or("");
            if target.is_empty() {
                return AdminCmd::Unknown { raw: input.into() };
            }
            let (slug, role) = match target.split_once('/') {
                Some((s, r)) if !s.is_empty() && !r.is_empty() => {
                    (s.to_string(), Some(r.to_string()))
                }
                Some(_) => return AdminCmd::Unknown { raw: input.into() },
                None => (target.to_string(), None),
            };
            match verb.as_str() {
                "pause" => AdminCmd::Pause { slug, role },
                "resume" => AdminCmd::Resume { slug, role },
                "stop" => AdminCmd::Stop { slug, role },
                _ => unreachable!(),
            }
        }
        _ => AdminCmd::Unknown { raw: input.into() },
    }
}

/// Side-effect classifier for the admin reply — lets tests assert
/// what the executor did without diffing free-form prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminSideEffect {
    /// No-op (status / list / help / unknown / cost summary).
    None,
    /// Wrote `drain.signal` for the listed `(slug, role)` pairs.
    Paused(Vec<(String, String)>),
    /// Removed `drain.signal` for the listed `(slug, role)` pairs.
    Resumed(Vec<(String, String)>),
    /// Wrote `shutdown.signal` for the listed `(slug, role)` pairs.
    Stopped(Vec<(String, String)>),
    /// Dangerous command pending — user must reply `CONFIRM`.
    ConfirmRequested {
        /// The command the executor will run on `CONFIRM`.
        pending: Box<AdminCmd>,
    },
    /// Confirm came in but no pending command (or expired).
    NothingToConfirm,
}

/// One executor reply — what to text back to the IM + what changed.
#[derive(Debug, Clone)]
pub struct AdminReply {
    /// Human-readable text to send through the channel.
    pub message: String,
    /// Side-effect classifier for assertions.
    pub side_effect: AdminSideEffect,
}

/// Pending-confirm TTL: a user has this long to reply `CONFIRM`
/// after a dangerous command before the prompt expires.
pub const CONFIRM_TTL: Duration = Duration::from_secs(60);

/// Per-(reply_target) pending dangerous command. Keyed by the IM
/// reply target so two groups can each have their own pending state
/// without crossing wires.
#[derive(Debug)]
struct Pending {
    cmd: AdminCmd,
    queued_at: Instant,
}

/// Admin-command executor. Holds the projects-root config (where bot
/// signal files live), the ccteam-root config (where the advise-budget
/// ledger lives — V0.6.6 F169), + the per-target pending-confirm map.
///
/// One instance per daemon. `Arc<AdminExecutor>` is cheap to clone
/// into per-message tasks.
pub struct AdminExecutor {
    projects_root: PathBuf,
    ccteam_root: PathBuf,
    pending: Mutex<HashMap<String, Pending>>,
    confirm_ttl: Duration,
}

impl AdminExecutor {
    /// Build an executor rooted at `projects_root`
    /// (production: `~/projects`; tests: a tempdir). The ccteam-root
    /// defaults to `~/.ccteam`; use [`Self::with_ccteam_root`] to
    /// override in tests.
    pub fn new(projects_root: impl Into<PathBuf>) -> Self {
        Self {
            projects_root: projects_root.into(),
            ccteam_root: crate::default_ccteam_root_public(),
            pending: Mutex::default(),
            confirm_ttl: CONFIRM_TTL,
        }
    }

    /// V0.6.6 F169 — override the ccteam-root used to read the
    /// `cost-budget.json` advise ledger. Production daemon stays on
    /// the home-derived default; tests inject a tempdir so each
    /// scenario has its own ledger state.
    #[doc(hidden)]
    pub fn with_ccteam_root(mut self, ccteam_root: impl Into<PathBuf>) -> Self {
        self.ccteam_root = ccteam_root.into();
        self
    }

    /// Test helper: override the confirm TTL.
    #[doc(hidden)]
    pub fn with_confirm_ttl(mut self, ttl: Duration) -> Self {
        self.confirm_ttl = ttl;
        self
    }

    /// Execute one parsed admin command on behalf of `reply_target`.
    /// Pure-async — the only IO is signal-file writes + registry
    /// reads. Never panics; unknown / unparsable commands return a
    /// help-ish reply with side-effect `None`.
    pub async fn execute(&self, cmd: AdminCmd, reply_target: &str) -> AdminReply {
        match cmd {
            AdminCmd::Status => AdminReply {
                message: "ccteam-im: daemon up.".into(),
                side_effect: AdminSideEffect::None,
            },
            AdminCmd::Help => AdminReply {
                message: help_text(),
                side_effect: AdminSideEffect::None,
            },
            AdminCmd::Unknown { raw } => AdminReply {
                message: format!("ccteam: don't recognize `{raw}`. Try `@ccteam help`."),
                side_effect: AdminSideEffect::None,
            },
            AdminCmd::List => self.list().await,
            AdminCmd::ListHere => self.list().await,
            AdminCmd::CostToday { slug } => self.cost_today(slug.as_deref()).await,
            AdminCmd::Pause { slug, role } => self.pause(&slug, role.as_deref()).await,
            AdminCmd::Resume { slug, role } => self.resume(&slug, role.as_deref()).await,
            AdminCmd::Stop { slug, role } => self.stop(&slug, role.as_deref()).await,
            AdminCmd::StopEverything => self.request_confirm(reply_target).await,
            AdminCmd::Confirm => self.consume_confirm(reply_target).await,
        }
    }

    /// Chat-context-aware variant of [`Self::execute`]. Today only the
    /// `list bots` / `bots` / `who` family of commands needs the
    /// `(channel, chat_id, bots)` slice — that surface renders the
    /// per-chat available-bot list shared with the unknown-handle
    /// reply. Everything else delegates straight to `execute`.
    pub async fn execute_for_chat(
        &self,
        cmd: AdminCmd,
        reply_target: &str,
        channel: &str,
        bots: &[BotRegistration],
    ) -> AdminReply {
        if matches!(cmd, AdminCmd::ListHere) {
            return self.list_here(channel, reply_target, bots);
        }
        self.execute(cmd, reply_target).await
    }

    fn list_here(&self, channel: &str, reply_target: &str, bots: &[BotRegistration]) -> AdminReply {
        let available = crate::router::available_handles_for_chat(bots, channel, reply_target);
        AdminReply {
            message: crate::router::format_available_bots_line(&available),
            side_effect: AdminSideEffect::None,
        }
    }

    async fn list(&self) -> AdminReply {
        let bots = list_bots().unwrap_or_default();
        if bots.is_empty() {
            return AdminReply {
                message: "ccteam: no bots registered.".into(),
                side_effect: AdminSideEffect::None,
            };
        }
        let mut lines = Vec::with_capacity(bots.len() + 1);
        lines.push(format!("ccteam bots ({})", bots.len()));
        for b in &bots {
            lines.push(format!(
                "  - {}/{} ({:?} on {})",
                b.workflow_slug, b.role, b.vendor, b.im_platform
            ));
        }
        AdminReply {
            message: lines.join("\n"),
            side_effect: AdminSideEffect::None,
        }
    }

    async fn cost_today(&self, slug_filter: Option<&str>) -> AdminReply {
        // V0.6.6 F169 — read the real `<ccteam_root>/cost-budget.json`
        // advise ledger (V0.6.5 F152 schema) so the IM `@ccteam cost
        // today` path surfaces the same USD numbers the
        // `/ccteam-control show-cost` CLI prints. The slug filter
        // stays advisory — the ledger is keyed on vendor + ts only
        // (per-slug attribution is a V0.7 ledger-schema bump) so we
        // report it back in the header for transparency.
        use ccteam_core::advise::{load_budget_ledger, sum_advise_today_by_vendor};
        use ccteam_core::harness::AgentVendor;
        use ccteam_core::DEFAULT_ADVISE_BUDGET_USD_24H;

        let ledger = load_budget_ledger(&self.ccteam_root).unwrap_or_default();
        // `+ 0.0` normalises negative-zero (Rust's `-0.0` formats as
        // `-0.0000` otherwise, which is ugly in the IM reply).
        let claude_24h = sum_advise_today_by_vendor(&ledger, AgentVendor::Claude) + 0.0;
        let codex_24h = sum_advise_today_by_vendor(&ledger, AgentVendor::Codex) + 0.0;
        let total = claude_24h + codex_24h;
        let cap = DEFAULT_ADVISE_BUDGET_USD_24H;
        let remaining = (cap - total).max(0.0);
        let near_cap = cap > 0.0 && total / cap >= 0.80;

        let bots = list_bots().unwrap_or_default();
        let filtered: Vec<&BotRegistration> = match slug_filter {
            Some(s) => bots.iter().filter(|b| b.workflow_slug == s).collect(),
            None => bots.iter().collect(),
        };
        let header = match slug_filter {
            Some(s) => format!("ccteam cost today — slug `{s}`"),
            None => "ccteam cost today".to_string(),
        };
        let warn_prefix = if near_cap {
            "⚠️ approaching daily budget cap\n"
        } else {
            ""
        };
        let slug_note = slug_filter.unwrap_or("none");
        let msg = format!(
            "{warn_prefix}{header}\n  \
             rolling 24h cost: Claude ${claude_24h:.4} + Codex ${codex_24h:.4} = total ${total:.4}\n  \
             cap: ${cap:.2}/24h · remaining: ${remaining:.4}\n  \
             active bots: {} (filter: {slug_note})\n  \
             full breakdown: `/ccteam-control show-cost`",
            filtered.len()
        );
        AdminReply {
            message: msg,
            side_effect: AdminSideEffect::None,
        }
    }

    async fn pause(&self, slug: &str, role: Option<&str>) -> AdminReply {
        let targets = self.targets(slug, role).await;
        if targets.is_empty() {
            return AdminReply {
                message: format!("ccteam: no bots match `{}`.", target_label(slug, role)),
                side_effect: AdminSideEffect::None,
            };
        }
        let mut applied = Vec::with_capacity(targets.len());
        let mut errs = Vec::new();
        for (s, r) in &targets {
            match self.write_signal(s, r, DRAIN_SIGNAL) {
                Ok(_) => applied.push((s.clone(), r.clone())),
                Err(err) => errs.push(format!("{s}/{r}: {err}")),
            }
        }
        let mut msg = format!("ccteam paused {} bot(s):", applied.len());
        for (s, r) in &applied {
            msg.push_str(&format!("\n  - {s}/{r}"));
        }
        if !errs.is_empty() {
            msg.push_str("\nerrors:");
            for e in &errs {
                msg.push_str(&format!("\n  - {e}"));
            }
        }
        AdminReply {
            message: msg,
            side_effect: AdminSideEffect::Paused(applied),
        }
    }

    async fn resume(&self, slug: &str, role: Option<&str>) -> AdminReply {
        let targets = self.targets(slug, role).await;
        if targets.is_empty() {
            return AdminReply {
                message: format!("ccteam: no bots match `{}`.", target_label(slug, role)),
                side_effect: AdminSideEffect::None,
            };
        }
        let mut applied = Vec::with_capacity(targets.len());
        for (s, r) in &targets {
            // Best-effort: missing signal file is fine (already not-paused).
            let _ = self.remove_signal(s, r, DRAIN_SIGNAL);
            applied.push((s.clone(), r.clone()));
        }
        let mut msg = format!("ccteam resumed {} bot(s):", applied.len());
        for (s, r) in &applied {
            msg.push_str(&format!("\n  - {s}/{r}"));
        }
        AdminReply {
            message: msg,
            side_effect: AdminSideEffect::Resumed(applied),
        }
    }

    async fn stop(&self, slug: &str, role: Option<&str>) -> AdminReply {
        let targets = self.targets(slug, role).await;
        if targets.is_empty() {
            return AdminReply {
                message: format!("ccteam: no bots match `{}`.", target_label(slug, role)),
                side_effect: AdminSideEffect::None,
            };
        }
        let mut applied = Vec::with_capacity(targets.len());
        let mut errs = Vec::new();
        for (s, r) in &targets {
            match self.write_signal(s, r, SHUTDOWN_SIGNAL) {
                Ok(_) => applied.push((s.clone(), r.clone())),
                Err(err) => errs.push(format!("{s}/{r}: {err}")),
            }
        }
        let mut msg = format!("ccteam stopped {} bot(s):", applied.len());
        for (s, r) in &applied {
            msg.push_str(&format!("\n  - {s}/{r}"));
        }
        if !errs.is_empty() {
            msg.push_str("\nerrors:");
            for e in &errs {
                msg.push_str(&format!("\n  - {e}"));
            }
        }
        AdminReply {
            message: msg,
            side_effect: AdminSideEffect::Stopped(applied),
        }
    }

    async fn request_confirm(&self, reply_target: &str) -> AdminReply {
        let pending = AdminCmd::StopEverything;
        let mut guard = self.pending.lock().await;
        guard.insert(
            reply_target.to_string(),
            Pending {
                cmd: pending.clone(),
                queued_at: Instant::now(),
            },
        );
        AdminReply {
            message: format!(
                "⚠️  ccteam: `stop everything` will shutdown every registered bot. \
                 Reply `CONFIRM` within {}s to proceed; anything else cancels.",
                self.confirm_ttl.as_secs()
            ),
            side_effect: AdminSideEffect::ConfirmRequested {
                pending: Box::new(pending),
            },
        }
    }

    async fn consume_confirm(&self, reply_target: &str) -> AdminReply {
        let pending_cmd = {
            let mut guard = self.pending.lock().await;
            match guard.remove(reply_target) {
                Some(p) if p.queued_at.elapsed() <= self.confirm_ttl => Some(p.cmd),
                _ => None,
            }
        };
        let Some(cmd) = pending_cmd else {
            return AdminReply {
                message: "ccteam: nothing pending to confirm.".into(),
                side_effect: AdminSideEffect::NothingToConfirm,
            };
        };
        match cmd {
            AdminCmd::StopEverything => self.stop_everything_now().await,
            // Defensive: only StopEverything is currently queued for
            // confirmation. Any other variant means a coding error
            // upstream — treat as nothing-to-confirm rather than
            // re-recursing.
            _ => AdminReply {
                message: "ccteam: nothing pending to confirm.".into(),
                side_effect: AdminSideEffect::NothingToConfirm,
            },
        }
    }

    async fn stop_everything_now(&self) -> AdminReply {
        let bots = list_bots().unwrap_or_default();
        if bots.is_empty() {
            return AdminReply {
                message: "ccteam: no bots registered — nothing to stop.".into(),
                side_effect: AdminSideEffect::Stopped(vec![]),
            };
        }
        let mut applied = Vec::with_capacity(bots.len());
        let mut errs = Vec::new();
        for b in &bots {
            match self.write_signal(&b.workflow_slug, &b.role, SHUTDOWN_SIGNAL) {
                Ok(_) => applied.push((b.workflow_slug.clone(), b.role.clone())),
                Err(err) => errs.push(format!("{}/{}: {err}", b.workflow_slug, b.role)),
            }
        }
        let mut msg = format!(
            "🛑 ccteam: shutdown signal written to {} bot(s).",
            applied.len()
        );
        for (s, r) in &applied {
            msg.push_str(&format!("\n  - {s}/{r}"));
        }
        if !errs.is_empty() {
            msg.push_str("\nerrors:");
            for e in &errs {
                msg.push_str(&format!("\n  - {e}"));
            }
        }
        AdminReply {
            message: msg,
            side_effect: AdminSideEffect::Stopped(applied),
        }
    }

    /// Resolve the `(slug, role)` targets from the registry. `role =
    /// None` → every role under `slug`.
    async fn targets(&self, slug: &str, role: Option<&str>) -> Vec<(String, String)> {
        let bots = list_bots().unwrap_or_default();
        bots.into_iter()
            .filter(|b| b.workflow_slug == slug)
            .filter(|b| role.is_none_or(|r| b.role == r))
            .map(|b| (b.workflow_slug, b.role))
            .collect()
    }

    fn signal_dir(&self, slug: &str, role: &str) -> PathBuf {
        self.projects_root
            .join(slug)
            .join(".ccteam")
            .join("chat")
            .join(role)
            .join("signals")
    }

    fn write_signal(&self, slug: &str, role: &str, name: &str) -> std::io::Result<()> {
        let dir = self.signal_dir(slug, role);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(name), "")
    }

    fn remove_signal(&self, slug: &str, role: &str, name: &str) -> std::io::Result<()> {
        let path = self.signal_dir(slug, role).join(name);
        if path.exists() {
            std::fs::remove_file(&path)
        } else {
            Ok(())
        }
    }

    /// Test helper: expose the projects root the executor was
    /// configured with.
    #[doc(hidden)]
    pub fn projects_root(&self) -> &Path {
        &self.projects_root
    }
}

fn target_label(slug: &str, role: Option<&str>) -> String {
    match role {
        Some(r) => format!("{slug}/{r}"),
        None => slug.to_string(),
    }
}

fn help_text() -> String {
    [
        "ccteam admin verbs:",
        "  pause <slug>[/<role>]    drain a bot (no new turns)",
        "  resume <slug>[/<role>]   un-drain",
        "  stop <slug>[/<role>]     shutdown a bot",
        "  list / ls                list every registered bot",
        "  list bots / bots / who   list bots reachable from this chat",
        "  cost today / cost <slug> 24h cost summary",
        "  stop everything          shutdown every bot (CONFIRM required)",
        "  status                   daemon liveness",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        assert_eq!(parse("status"), AdminCmd::Status);
        assert_eq!(parse("  STATUS "), AdminCmd::Status);
    }

    #[test]
    fn parses_pause_slug_only() {
        assert_eq!(
            parse("pause helper-bot"),
            AdminCmd::Pause {
                slug: "helper-bot".into(),
                role: None,
            }
        );
    }

    #[test]
    fn parses_pause_slug_role() {
        assert_eq!(
            parse("pause dev-foo/lead"),
            AdminCmd::Pause {
                slug: "dev-foo".into(),
                role: Some("lead".into()),
            }
        );
    }

    #[test]
    fn parses_resume_slug_only() {
        assert_eq!(
            parse("resume helper-bot"),
            AdminCmd::Resume {
                slug: "helper-bot".into(),
                role: None,
            }
        );
    }

    #[test]
    fn parses_list_aliases() {
        // Global registry dump.
        assert_eq!(parse("list"), AdminCmd::List);
        assert_eq!(parse("ls"), AdminCmd::List);
        // Per-chat scoped form (new 6th keyword family).
        assert_eq!(parse("list bots"), AdminCmd::ListHere);
        assert_eq!(parse("LIST BOTS"), AdminCmd::ListHere);
        assert_eq!(parse("bots"), AdminCmd::ListHere);
        assert_eq!(parse("who"), AdminCmd::ListHere);
    }

    #[test]
    fn parses_cost_today_and_slug() {
        assert_eq!(parse("cost today"), AdminCmd::CostToday { slug: None });
        assert_eq!(parse("cost"), AdminCmd::CostToday { slug: None });
        assert_eq!(
            parse("cost helper-bot"),
            AdminCmd::CostToday {
                slug: Some("helper-bot".into()),
            }
        );
    }

    #[test]
    fn parses_stop_everything_and_alias() {
        assert_eq!(parse("stop everything"), AdminCmd::StopEverything);
        assert_eq!(parse("STOP EVERYTHING"), AdminCmd::StopEverything);
        assert_eq!(parse("kill all"), AdminCmd::StopEverything);
        assert_eq!(parse("stop all"), AdminCmd::StopEverything);
    }

    #[test]
    fn parses_stop_single_bot() {
        assert_eq!(
            parse("stop helper-bot"),
            AdminCmd::Stop {
                slug: "helper-bot".into(),
                role: None,
            }
        );
        assert_eq!(
            parse("stop dev-foo/lead"),
            AdminCmd::Stop {
                slug: "dev-foo".into(),
                role: Some("lead".into()),
            }
        );
    }

    #[test]
    fn parses_confirm_case_insensitive() {
        assert_eq!(parse("CONFIRM"), AdminCmd::Confirm);
        assert_eq!(parse("confirm"), AdminCmd::Confirm);
        assert_eq!(parse("  Confirm  "), AdminCmd::Confirm);
    }

    #[test]
    fn empty_input_help() {
        assert_eq!(parse(""), AdminCmd::Help);
        assert_eq!(parse("help"), AdminCmd::Help);
        assert_eq!(parse("?"), AdminCmd::Help);
    }

    #[test]
    fn unknown_verb_returns_unknown() {
        assert!(matches!(
            parse("frobnicate the doohickey"),
            AdminCmd::Unknown { .. }
        ));
    }
}
