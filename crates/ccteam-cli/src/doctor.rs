//! v0.8.22 — bare `ccteam doctor` readiness checkup.
//!
//! Before this module existed, a bare `ccteam doctor` (no flags) only ran
//! the implicit pricing-staleness check (`commands::run_doctor`'s
//! `any_mode == false` branch) plus a daemon-health line, while the
//! genuinely useful "is my machine ready to run ccteam" checks (claude /
//! codex binaries, tmux, MCP registration) hid behind ~20 opt-in flags
//! that also cluttered `--help` (review: `docs/research/` v0.8.21 CLI
//! audit §二 item 3). This module gives bare `ccteam doctor` one
//! `[PASS]/[WARN]/[FAIL]/[SKIP]` line per readiness check + a summary
//! line, and reports exit code 1 iff any check is FAIL.
//!
//! Every probe here is read-only: it never starts/stops/restarts the
//! daemon (`ccteam_core::check_daemon_health` is a read-only socket
//! ping) and never writes to a vendor config (MCP registration is
//! read-only via `ccteam_core::mcp_register::{claude,codex}_mcp_registered`;
//! the writer is `ccteam config mcp`, which stays a separate explicit
//! step per the "ccteam writes nothing but its own MCP registration, and
//! only when asked" red line).
//!
//! The historical one-shot migration / repair flags (`--migrate-*`,
//! `--install-*`, `--reset-shipped-teams`, `--gc-claude-jobs`, ...) still
//! dispatch through `commands::run_doctor` completely unchanged — this
//! module only intercepts the *bare* invocation (see
//! [`is_bare_invocation`]). `--verify-mcp` (the MCP tool-surface / STUB
//! invariant self-check CLAUDE.md calls out by name) is a different
//! concern (dev/CI invariant, not end-user readiness) and is left alone.

use std::process::Command;

use ccteam_core::{CcteamPaths, Vendor};

/// Severity of one readiness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Skip => "SKIP",
        }
    }
}

/// One rendered checklist row.
struct CheckLine {
    status: CheckStatus,
    name: &'static str,
    detail: String,
}

impl CheckLine {
    fn new(status: CheckStatus, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            name,
            detail: detail.into(),
        }
    }
}

/// Mirrors `commands::run_doctor`'s own `any_mode` boolean (kept in sync
/// by hand — `commands.rs` is the SoT for `DoctorOptions`, this is a
/// read-only re-derivation so `ccteam-cli::main` can decide, BEFORE
/// calling into `commands::run_doctor`, whether to run the new readiness
/// checkup instead of the legacy dispatch). `verify_mcp` /
/// `check_codex_auto_critic` are included defensively even though
/// `main::run_doctor` already short-circuits on those before reaching
/// this check.
pub fn is_bare_invocation(opts: &crate::commands::DoctorOptions) -> bool {
    let any_mode = opts.tool_surface
        || opts.install_memory_bridge
        || opts.reset_shipped_teams
        || opts.validate_team.is_some()
        || opts.migrate_recommended_agents
        || opts.screenshot_smoke.is_some()
        || opts.migrate_v041_to_v042
        || opts.migrate_workflow_to_ccteam_dir
        || opts.gc_claude_jobs
        || opts.update_hooks
        || opts.check_pricing_version
        || opts.check_codex_version
        || opts.check_codex_auth
        || opts.check_codex_auto_critic
        || opts.check_cost_orphan
        || opts.install_hooks
        || opts.migrate_hook_commands
        || opts.verify_mcp;
    !any_mode
}

/// Run the full readiness checkup and render it. Returns `(report,
/// any_fail)`; the caller exits 1 iff `any_fail`.
pub fn run_readiness_checkup(paths: &CcteamPaths) -> (String, bool) {
    let claude = check_claude_binary();
    let codex = check_codex_binary();
    let grok = check_grok_binary();
    let opencode = check_opencode_binary();
    let codex_present = codex.status == CheckStatus::Pass;
    let tmux = check_tmux();
    let mcp_claude = check_mcp_claude();
    let mcp_codex = check_mcp_codex(codex_present);
    let daemon = check_daemon(paths);
    let pricing = check_pricing();
    let home = check_home_layout(paths);
    let auth_claude = check_vendor_auth_claude();
    let auth_codex = check_vendor_auth_codex();
    let auth_grok = check_vendor_auth_grok();
    let auth_opencode = check_vendor_auth_opencode();

    let checks = [
        claude,
        codex,
        grok,
        opencode,
        tmux,
        mcp_claude,
        mcp_codex,
        daemon,
        pricing,
        home,
        auth_claude,
        auth_codex,
        auth_grok,
        auth_opencode,
    ];

    let mut pass = 0usize;
    let mut warn = 0usize;
    let mut fail = 0usize;
    let mut skip = 0usize;

    let mut out = String::from("ccteam doctor: readiness checkup\n");
    out.push_str("=================================\n\n");
    for c in &checks {
        match c.status {
            CheckStatus::Pass => pass += 1,
            CheckStatus::Warn => warn += 1,
            CheckStatus::Fail => fail += 1,
            CheckStatus::Skip => skip += 1,
        }
        out.push_str(&format!(
            "[{:<4}] {:<16} {}\n",
            c.status.label(),
            format!("{}:", c.name),
            c.detail
        ));
    }

    let any_fail = fail > 0;
    out.push('\n');
    if any_fail {
        out.push_str(&format!(
            "summary: {pass} pass, {warn} warn, {fail} fail, {skip} skip — NOT READY \
             (fix the FAIL line(s) above; `ccteam config mcp` covers MCP registration).\n",
        ));
    } else {
        out.push_str(&format!(
            "summary: {pass} pass, {warn} warn, {fail} fail, {skip} skip — READY \
             (WARN/SKIP lines are informational, not blocking).\n",
        ));
    }
    (out, any_fail)
}

/// `claude` is ccteam's primary, always-required vendor (the default
/// `stream-json` protocol spawns it directly) — a missing binary is a
/// hard FAIL, not a WARN. Honors `CCTEAM_CLAUDE_BIN` exactly like every
/// spawn path (`ccteam_core::CLAUDE_BIN_ENV`).
fn check_claude_binary() -> CheckLine {
    probe_binary(
        "claude binary",
        ccteam_core::CLAUDE_BIN_ENV,
        "claude",
        CheckStatus::Fail,
        "install the Claude Code CLI, or point",
    )
}

/// codex is best-effort / optional (Claude is the only fully supported
/// vendor today) — a missing binary WARNs, it never fails the checkup.
fn check_codex_binary() -> CheckLine {
    probe_binary(
        "codex binary",
        ccteam_core::CODEX_BIN_ENV,
        "codex",
        CheckStatus::Warn,
        "install the Codex CLI (optional), or point",
    )
}

/// grok is optional (v0.8.23 third vendor) — missing binary WARNs only.
fn check_grok_binary() -> CheckLine {
    probe_binary(
        "grok binary",
        ccteam_core::GROK_BIN_ENV,
        "grok",
        CheckStatus::Warn,
        "install the Grok Build CLI (optional), or point",
    )
}

/// opencode is optional (v0.8.24 fourth vendor) — missing binary WARNs only.
fn check_opencode_binary() -> CheckLine {
    probe_binary(
        "opencode binary",
        ccteam_core::OPENCODE_BIN_ENV,
        "opencode",
        CheckStatus::Warn,
        "install the OpenCode CLI (optional), or point",
    )
}

/// Shared `<bin> --version` probe. `missing_status` lets the two callers
/// above pick FAIL (claude, required) vs WARN (codex, optional) for the
/// same "binary not resolvable" outcome.
fn probe_binary(
    name: &'static str,
    env_var: &str,
    default_bin: &str,
    missing_status: CheckStatus,
    fix_hint: &str,
) -> CheckLine {
    let bin = std::env::var(env_var).unwrap_or_else(|_| default_bin.to_string());
    match Command::new(&bin).arg("--version").output() {
        Ok(out) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let detail = if version.is_empty() {
                format!("resolved `{bin}`")
            } else {
                format!("{bin} — {version}")
            };
            CheckLine::new(CheckStatus::Pass, name, detail)
        }
        Err(err) => CheckLine::new(
            missing_status,
            name,
            format!("`{bin}` not resolvable ({err}) — {fix_hint} {env_var} at its path"),
        ),
    }
}

/// v0.8.24 F5 — per-vendor auth status at WARN level (missing credentials).
fn check_vendor_auth_claude() -> CheckLine {
    // Claude Code stores OAuth/API key under ~/.claude or CLAUDE_CONFIG_HOME.
    let home = std::env::var("CLAUDE_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    let ok = home
        .as_ref()
        .map(|h| {
            h.join(".credentials.json").exists()
                || h.join("credentials.json").exists()
                || std::env::var("ANTHROPIC_API_KEY").is_ok()
        })
        .unwrap_or_else(|| std::env::var("ANTHROPIC_API_KEY").is_ok());
    if ok {
        CheckLine::new(
            CheckStatus::Pass,
            "claude auth",
            "credentials present (file or ANTHROPIC_API_KEY)",
        )
    } else {
        CheckLine::new(
            CheckStatus::Warn,
            "claude auth",
            "no credentials found — run `claude auth login` or set ANTHROPIC_API_KEY",
        )
    }
}

fn check_vendor_auth_codex() -> CheckLine {
    let home = dirs::home_dir().map(|h| h.join(".codex"));
    let ok = home
        .as_ref()
        .map(|h| h.join("auth.json").exists() || h.join("config.toml").exists())
        .unwrap_or(false)
        || std::env::var("OPENAI_API_KEY").is_ok();
    if ok {
        CheckLine::new(
            CheckStatus::Pass,
            "codex auth",
            "credentials present (file or OPENAI_API_KEY)",
        )
    } else {
        CheckLine::new(
            CheckStatus::Warn,
            "codex auth",
            "no credentials found — run `codex login` or set OPENAI_API_KEY",
        )
    }
}

fn check_vendor_auth_grok() -> CheckLine {
    let ok = std::env::var("XAI_API_KEY").is_ok()
        || dirs::home_dir()
            .map(|h| h.join(".grok").exists() || h.join(".config/grok").exists())
            .unwrap_or(false);
    if ok {
        CheckLine::new(
            CheckStatus::Pass,
            "grok auth",
            "credentials present (env or config dir)",
        )
    } else {
        CheckLine::new(
            CheckStatus::Warn,
            "grok auth",
            "no credentials found — set XAI_API_KEY or log in via grok CLI",
        )
    }
}

fn check_vendor_auth_opencode() -> CheckLine {
    let ok = std::env::var("OPENAI_API_KEY").is_ok()
        || dirs::home_dir()
            .map(|h| {
                h.join(".local/share/opencode").exists()
                    || h.join(".config/opencode").exists()
                    || h.join(".opencode").exists()
            })
            .unwrap_or(false);
    if ok {
        CheckLine::new(
            CheckStatus::Pass,
            "opencode auth",
            "credentials present (provider config or env)",
        )
    } else {
        CheckLine::new(
            CheckStatus::Warn,
            "opencode auth",
            "no credentials found — run `opencode auth login` for a provider",
        )
    }
}

/// `tmux` is only needed for the `terminal` session protocol (the
/// bundled rmux backend works with no external tmux) — WARN, not FAIL.
fn check_tmux() -> CheckLine {
    match ccteam_core::tmux::tmux_version() {
        Some(version) => CheckLine::new(CheckStatus::Pass, "tmux", version),
        None => CheckLine::new(
            CheckStatus::Warn,
            "tmux",
            "not found on PATH — only needed for the `terminal` session protocol \
             (the default stream-json protocol and the bundled rmux backend don't need it)"
                .to_string(),
        ),
    }
}

/// Is the ccteam MCP server registered in Claude's `~/.claude.json`?
/// Read-only (`ccteam_core::mcp_register::claude_mcp_registered`); the
/// writer is the explicit `ccteam config mcp` step. Missing = FAIL:
/// without this, the `session_*` / chat / admin MCP tools the cto role
/// depends on are unreachable.
fn check_mcp_claude() -> CheckLine {
    match ccteam_core::projects::resolve_claude_json_path() {
        Ok(path) => {
            if ccteam_core::mcp_register::claude_mcp_registered(&path) {
                CheckLine::new(
                    CheckStatus::Pass,
                    "MCP (claude)",
                    format!("registered in {}", path.display()),
                )
            } else {
                CheckLine::new(
                    CheckStatus::Fail,
                    "MCP (claude)",
                    format!(
                        "ccteam MCP server not registered in {} — fix: `ccteam config mcp`",
                        path.display()
                    ),
                )
            }
        }
        Err(err) => CheckLine::new(
            CheckStatus::Warn,
            "MCP (claude)",
            format!("could not resolve ~/.claude.json: {err}"),
        ),
    }
}

/// Codex equivalent of [`check_mcp_claude`], but WARN (not FAIL) since
/// codex is optional; SKIPped outright when the codex binary itself
/// isn't present (nothing to register against).
fn check_mcp_codex(codex_present: bool) -> CheckLine {
    if !codex_present {
        return CheckLine::new(
            CheckStatus::Skip,
            "MCP (codex)",
            "codex binary not found — skipping registration check".to_string(),
        );
    }
    match ccteam_core::mcp_register::resolve_codex_config_path() {
        Ok(path) => {
            if ccteam_core::mcp_register::codex_mcp_registered(&path) {
                CheckLine::new(
                    CheckStatus::Pass,
                    "MCP (codex)",
                    format!("registered in {}", path.display()),
                )
            } else {
                CheckLine::new(
                    CheckStatus::Warn,
                    "MCP (codex)",
                    format!(
                        "ccteam MCP server not registered in {} — fix: `ccteam config mcp`",
                        path.display()
                    ),
                )
            }
        }
        Err(err) => CheckLine::new(
            CheckStatus::Warn,
            "MCP (codex)",
            format!("could not resolve codex config.toml: {err}"),
        ),
    }
}

/// Read-only daemon liveness ping — NEVER starts/stops/restarts the
/// daemon (CLAUDE.md red line). A down daemon WARNs (perfectly normal
/// before the first `ccteam start`), it doesn't fail the checkup.
fn check_daemon(paths: &CcteamPaths) -> CheckLine {
    let health = ccteam_core::check_daemon_health(paths);
    let status = if health.is_healthy() {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };
    CheckLine::new(status, "daemon", health.describe())
}

/// V0.5.0 F92's staleness threshold (>180 days) re-derived per vendor —
/// pure readout, always WARN-max (never fails the checkup). The
/// detailed per-vendor breakdown remains available via the (now hidden
/// but still functional) `ccteam doctor --check-pricing-version` flag.
fn check_pricing() -> CheckLine {
    const WARN_DAYS: i64 = 180;
    let today = chrono::Utc::now().date_naive();
    let mut worst: Option<i64> = None;
    for &vendor in Vendor::ALL {
        let raw = ccteam_core::pricing_schema_version_for(vendor);
        if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            let age = (today - d).num_days();
            if worst.is_none_or(|w| age > w) {
                worst = Some(age);
            }
        }
    }
    match worst {
        Some(age) if age > WARN_DAYS => CheckLine::new(
            CheckStatus::Warn,
            "pricing tables",
            format!(
                "embedded rate sheet is {age}d old (>{WARN_DAYS}d) — upgrade ccteam for \
                 current pricing (details: `ccteam doctor --check-pricing-version`)"
            ),
        ),
        Some(age) => CheckLine::new(
            CheckStatus::Pass,
            "pricing tables",
            format!("{age}d old (fresh)"),
        ),
        None => CheckLine::new(
            CheckStatus::Skip,
            "pricing tables",
            "could not parse embedded schema_version".to_string(),
        ),
    }
}

/// `~/.ccteam` home-layout drift, re-derived from
/// `ccteam_core::canonical_home_dirs()` (same manifest
/// `commands::render_home_drift_line` uses). SKIP when the home doesn't
/// exist yet (fresh install — nothing to compare); otherwise WARN on any
/// top-level dir the current architecture no longer writes.
fn check_home_layout(paths: &CcteamPaths) -> CheckLine {
    if !paths.root.exists() {
        return CheckLine::new(
            CheckStatus::Skip,
            "home layout",
            format!(
                "{} does not exist yet (fresh install)",
                paths.root.display()
            ),
        );
    }
    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return CheckLine::new(
            CheckStatus::Skip,
            "home layout",
            format!("could not read {}", paths.root.display()),
        );
    };
    let mut unexpected: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if ccteam_core::canonical_home_dirs().contains(&name) {
            continue;
        }
        unexpected.push(name.to_string());
    }
    if unexpected.is_empty() {
        CheckLine::new(
            CheckStatus::Pass,
            "home layout",
            format!("{} matches the canonical layout", paths.root.display()),
        )
    } else {
        unexpected.sort();
        CheckLine::new(
            CheckStatus::Warn,
            "home layout",
            format!(
                "{} unexpected dir(s) under {} (orchestrator-era leftovers, safe to `rm -rf`): {}",
                unexpected.len(),
                paths.root.display(),
                unexpected.join(", "),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_invocation_is_true_for_default_options() {
        assert!(is_bare_invocation(
            &crate::commands::DoctorOptions::default()
        ));
    }

    #[test]
    fn bare_invocation_is_false_when_any_flag_set() {
        let opts = crate::commands::DoctorOptions {
            check_pricing_version: true,
            ..Default::default()
        };
        assert!(!is_bare_invocation(&opts));

        let opts = crate::commands::DoctorOptions {
            verify_mcp: true,
            ..Default::default()
        };
        assert!(!is_bare_invocation(&opts));

        let opts = crate::commands::DoctorOptions {
            validate_team: Some("dev".to_string()),
            ..Default::default()
        };
        assert!(!is_bare_invocation(&opts));
    }

    // `run_readiness_checkup` itself is exercised end-to-end (with full
    // `CCTEAM_CLAUDE_BIN` / `CLAUDE_CONFIG_HOME` / `CCTEAM_HOME` / `HOME`
    // sandboxing) by `crates/ccteam-cli/tests/doctor_readiness_test.rs` —
    // deliberately NOT here: several of its probes (claude/codex/tmux
    // binaries, `~/.claude.json`, `~/.ccteam`) read ambient process env /
    // the real filesystem when not overridden, which a lib `#[cfg(test)]
    // mod tests` unit test (sharing the test binary's real env with every
    // other lib test) cannot safely sandbox. See CLAUDE.md §六.
}
