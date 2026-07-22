//! Bare `ccteam doctor` readiness checkup.
//!
//! A bare `ccteam doctor` (no flags) reports the "is my machine ready to
//! run ccteam" checks — the five vendor binaries (claude / codex / grok /
//! opencode / kimi) and their auth, tmux, MCP registration, daemon
//! health, pricing staleness, and `~/.ccteam` home-layout drift — as one
//! `[PASS]/[WARN]/[FAIL]/[SKIP]` line per check plus a summary line, and
//! reports exit code 1 iff any check is FAIL. `main::run_doctor` calls
//! [`run_readiness_checkup`] for every invocation that is not
//! `--verify-mcp`.
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
//! `--install-*`, `--reset-shipped-teams`, `--gc-claude-jobs`, ...) were
//! removed outright (pre-v1.0 = no back-compat shims). `--verify-mcp`
//! (the MCP tool-surface / STUB invariant self-check CLAUDE.md calls out
//! by name) is a different concern (dev/CI invariant, not end-user
//! readiness) and short-circuits in `main::run_doctor` before this.

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

/// Run the full readiness checkup and render it. Returns `(report,
/// any_fail)`; the caller exits 1 iff `any_fail`.
pub fn run_readiness_checkup(paths: &CcteamPaths) -> (String, bool) {
    let claude = check_claude_binary();
    let codex = check_codex_binary();
    let grok = check_grok_binary();
    let opencode = check_opencode_binary();
    let kimi = check_kimi_binary();
    let codex_present = codex.status == CheckStatus::Pass;
    let tmux = check_tmux();
    let mcp_claude = check_mcp_claude();
    let mcp_codex = check_mcp_codex(codex_present);
    let mcp_kimi = check_mcp_kimi();
    let daemon = check_daemon(paths);
    let legacy_service = check_legacy_service();
    let updates = check_updates(paths);
    let pricing = check_pricing();
    let home = check_home_layout(paths);
    let auth_claude = check_vendor_auth_claude();
    let auth_codex = check_vendor_auth_codex();
    let auth_grok = check_vendor_auth_grok();
    let auth_opencode = check_vendor_auth_opencode();
    let auth_kimi = check_vendor_auth_kimi();

    let checks = [
        claude,
        codex,
        grok,
        opencode,
        kimi,
        tmux,
        mcp_claude,
        mcp_codex,
        mcp_kimi,
        daemon,
        legacy_service,
        updates,
        pricing,
        home,
        auth_claude,
        auth_codex,
        auth_grok,
        auth_opencode,
        auth_kimi,
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

/// kimi is optional (fifth vendor) — missing binary WARNs only.
fn check_kimi_binary() -> CheckLine {
    probe_binary(
        "kimi binary",
        ccteam_core::KIMI_BIN_ENV,
        "kimi",
        CheckStatus::Warn,
        "install the Kimi Code CLI (optional), or point",
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

fn check_vendor_auth_kimi() -> CheckLine {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")));
    let ok = std::env::var("MOONSHOT_API_KEY").is_ok()
        || kimi_home
            .as_ref()
            .map(|h| h.join("credentials").exists() || h.join("oauth").exists())
            .unwrap_or(false);
    if ok {
        CheckLine::new(
            CheckStatus::Pass,
            "kimi auth",
            "credentials present (oauth store or MOONSHOT_API_KEY)",
        )
    } else {
        CheckLine::new(
            CheckStatus::Warn,
            "kimi auth",
            "no credentials found — run `kimi login` or set MOONSHOT_API_KEY",
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

/// Kimi equivalent of [`check_mcp_codex`] (WARN, kimi is optional): the
/// global `$KIMI_CODE_HOME/mcp.json` entry lets a plain `kimi` main session
/// orchestrate.
fn check_mcp_kimi() -> CheckLine {
    match ccteam_core::mcp_register::resolve_kimi_config_path() {
        Ok(path) => {
            if ccteam_core::mcp_register::kimi_mcp_registered(&path) {
                CheckLine::new(
                    CheckStatus::Pass,
                    "MCP (kimi)",
                    format!("registered in {}", path.display()),
                )
            } else {
                CheckLine::new(
                    CheckStatus::Warn,
                    "MCP (kimi)",
                    format!(
                        "ccteam MCP server not registered in {} — fix: `ccteam config mcp`",
                        path.display()
                    ),
                )
            }
        }
        Err(err) => CheckLine::new(
            CheckStatus::Warn,
            "MCP (kimi)",
            format!("could not resolve kimi mcp.json: {err}"),
        ),
    }
}

/// Read-only daemon liveness ping — NEVER starts/stops/restarts the
/// daemon (CLAUDE.md red line). A down daemon WARNs (perfectly normal
/// before the first `ccteam daemon start`), it doesn't fail the checkup.
fn check_daemon(paths: &CcteamPaths) -> CheckLine {
    let health = ccteam_core::check_daemon_health(paths);
    let status = if health.is_healthy() {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };
    CheckLine::new(status, "daemon", health.describe())
}

/// v0.9.7 — residual legacy systemd/launchd unit detection (read-only;
/// the actual migration lives in `ccteam daemon start`'s takeover
/// pre-step, never in doctor). Installer-written unit → WARN with the
/// one-command migration; hand-written unit → WARN with guidance only
/// (ccteam never deletes it).
fn check_legacy_service() -> CheckLine {
    let paths = crate::legacy_takeover::LegacyServicePaths::from_env();
    match crate::legacy_takeover::detect_legacy_unit(&paths) {
        None => CheckLine::new(
            CheckStatus::Pass,
            "legacy service",
            "no legacy systemd/launchd ccteam unit (pid-detach self-management)",
        ),
        Some((path, true)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy service",
            format!(
                "legacy installer-written ccteam unit at {} — systemd/launchd management is \
                 retired; migrate with `ccteam daemon start` (auto-takeover: stops + removes \
                 the unit, restarts detached)",
                path.display()
            ),
        ),
        Some((path, false)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy service",
            format!(
                "service unit at {} was not written by the ccteam installer — ccteam will not \
                 manage or delete it; its instance counts as \"not managed\" for \
                 `ccteam daemon stop`. Remove it manually if you want ccteam self-management",
                path.display()
            ),
        ),
    }
}

/// v0.9.7 (PRD F3.5/F3.6) — install channel + version skew + fleet skew.
///
/// Reports: install channel · `current_exe` · on-disk binary version ·
/// running-daemon version (from the versioned probe) with a
/// restart-needed note if it lags the binary · cached latest release (with
/// `update available → …` or `up to date`) · one line per registered
/// satellite whose version differs (F3.6). WARN iff a newer ccteam is
/// available, the running daemon lags the binary, or a satellite is
/// skewed; otherwise PASS. **Cache-only** for the latest-version display —
/// the doctor never blocks on a network fetch (the ≥20h refresh is driven
/// by `ccteam status`); a missing cache / down daemon is informational,
/// never FAIL.
fn check_updates(paths: &CcteamPaths) -> CheckLine {
    let channel = ccteam_core::install_channel::detect(paths);
    let binary_version = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let probe = ccteam_core::daemon::probe_daemon(paths);
    let cache = ccteam_core::version_check::cached_latest(paths).unwrap_or_default();
    let update_avail = ccteam_core::version_check::update_available(&cache, binary_version);

    let mut warn = false;
    let mut parts: Vec<String> = vec![
        format!("channel {}", channel.as_str()),
        format!("exe {exe}"),
        format!("binary {binary_version}"),
    ];

    // Running daemon version vs the on-disk binary.
    if probe.ready {
        match &probe.version {
            Some(v) if v == binary_version => parts.push(format!("daemon {v}")),
            Some(v) => {
                warn = true;
                parts.push(format!(
                    "daemon {v} (RESTART NEEDED: `ccteam daemon restart` to load the new binary)"
                ));
            }
            None => parts.push("daemon running (version unknown)".to_string()),
        }
    } else {
        parts.push("daemon not running".to_string());
    }

    // Latest release, from the cache only (no network here).
    match &update_avail {
        Some(latest) => {
            warn = true;
            let action =
                if ccteam_core::install_channel::suggested_update_command(&channel).is_some() {
                    "run `ccteam update`".to_string()
                } else {
                    format!("reinstall from {}", ccteam_core::install_channel::REPO_URL)
                };
            parts.push(format!(
                "update available {binary_version} → {latest}: {action}"
            ));
        }
        None if cache.latest_version.is_some() => parts.push("up to date".to_string()),
        None => parts.push(
            "latest unknown (no cached check yet — `ccteam status` refreshes it)".to_string(),
        ),
    }

    // Fleet version skew (F3.6) — shared with `ccteam status`.
    for line in crate::update::fleet_version_skew(paths, binary_version) {
        warn = true;
        parts.push(line);
    }

    CheckLine::new(
        if warn {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        },
        "updates",
        parts.join("; "),
    )
}

/// V0.5.0 F92's staleness threshold (>180 days) re-derived per vendor —
/// pure readout, always WARN-max (never fails the checkup).
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
                 current pricing"
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
/// `ccteam_core::canonical_home_dirs()` (the init-time directory
/// manifest). SKIP when the home doesn't exist yet (fresh install —
/// nothing to compare); otherwise WARN on any top-level dir the current
/// architecture no longer writes.
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

// `run_readiness_checkup` (the only public entry here now that the bare
// invocation is decided inline in `main::run_doctor`) is exercised
// end-to-end (with full `CCTEAM_CLAUDE_BIN` / `CLAUDE_CONFIG_HOME` /
// `CCTEAM_HOME` / `HOME` sandboxing) by
// `crates/ccteam-cli/tests/doctor_readiness_test.rs` — deliberately NOT
// as a lib `#[cfg(test)] mod tests` here: several of its probes
// (claude/codex/tmux binaries, `~/.claude.json`, `~/.ccteam`) read
// ambient process env / the real filesystem when not overridden, which a
// lib unit test (sharing the test binary's real env with every other lib
// test) cannot safely sandbox. See CLAUDE.md §六.
