//! Bare `ccteam doctor` readiness checkup.
//!
//! A bare invocation reports one consolidated row per vendor plus ccteam and
//! project readiness advisories. It exits 1 only when the required Claude Code
//! binary is missing; warnings are informational because daemon startup can
//! self-heal vendor MCP registration.
//!
//! Every probe here is read-only: doctor never starts or changes the daemon and
//! never writes vendor configuration. Registration writers are `ccteam config`,
//! the web `POST .../register-mcp` endpoint, and daemon-start auto-registration.
//! `--verify-mcp` remains a separate dev/CI invariant handled by
//! `main::run_doctor` before this module is called.

use std::io::IsTerminal;
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
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Skip => 1,
            Self::Warn => 2,
            Self::Fail => 3,
        }
    }

    fn worst(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
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
        debug_assert!(name.chars().count() <= 9, "doctor row name is too wide");
        Self {
            status,
            name,
            detail: detail.into(),
        }
    }
}

#[derive(Default)]
struct Counts {
    pass: usize,
    warn: usize,
    fail: usize,
    skip: usize,
}

impl Counts {
    fn add(&mut self, status: CheckStatus) {
        match status {
            CheckStatus::Pass => self.pass += 1,
            CheckStatus::Warn => self.warn += 1,
            CheckStatus::Fail => self.fail += 1,
            CheckStatus::Skip => self.skip += 1,
        }
    }

    fn summary(&self) -> String {
        let mut parts = vec![format!("{} pass", self.pass)];
        if self.warn > 0 {
            parts.push(format!("{} warn", self.warn));
        }
        if self.fail > 0 {
            parts.push(format!("{} fail", self.fail));
        }
        if self.skip > 0 {
            parts.push(format!("{} skip", self.skip));
        }
        parts.join(", ")
    }
}

struct ReportRow {
    line: CheckLine,
    suppress_when_pass: bool,
}

impl ReportRow {
    fn visible(line: CheckLine) -> Self {
        Self {
            line,
            suppress_when_pass: false,
        }
    }

    fn advisory(line: CheckLine) -> Self {
        Self {
            line,
            suppress_when_pass: true,
        }
    }

    fn is_visible(&self) -> bool {
        !(self.suppress_when_pass && self.line.status == CheckStatus::Pass)
    }
}

/// Fully probed readiness state. Rendering this value is pure: it performs no
/// environment, filesystem, process, or daemon access.
struct ReadinessReport {
    agents: Vec<ReportRow>,
    ccteam: Vec<ReportRow>,
    projects: Vec<ReportRow>,
    daemon_healthy: bool,
}

/// Run the full readiness checkup and render it. Returns `(report, any_fail)`;
/// the caller exits 1 iff `any_fail`.
pub fn run_readiness_checkup(paths: &CcteamPaths) -> (String, bool) {
    let report = gather_readiness(paths);
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    render_readiness(&report, color)
}

fn gather_readiness(paths: &CcteamPaths) -> ReadinessReport {
    let agents = vec![
        ReportRow::visible(check_agent(
            "claude",
            ccteam_core::CLAUDE_BIN_ENV,
            "claude",
            true,
            check_vendor_auth_claude,
        )),
        ReportRow::visible(check_agent(
            "codex",
            ccteam_core::CODEX_BIN_ENV,
            "codex",
            false,
            check_vendor_auth_codex,
        )),
        ReportRow::visible(check_agent(
            "grok",
            ccteam_core::GROK_BIN_ENV,
            "grok",
            false,
            check_vendor_auth_grok,
        )),
        ReportRow::visible(check_agent(
            "opencode",
            ccteam_core::OPENCODE_BIN_ENV,
            "opencode",
            false,
            check_vendor_auth_opencode,
        )),
        ReportRow::visible(check_agent(
            "kimi",
            ccteam_core::KIMI_BIN_ENV,
            "kimi",
            false,
            check_vendor_auth_kimi,
        )),
    ];

    let daemon_probe = ccteam_core::daemon::probe_daemon(paths);
    let daemon_healthy = daemon_probe.ready;
    let daemon_version = daemon_probe.version;
    let daemon = if daemon_probe.ready {
        CheckLine::new(
            CheckStatus::Pass,
            "daemon",
            daemon_version
                .clone()
                .map(|version| format!("running (v{version})"))
                .unwrap_or_else(|| "running".to_string()),
        )
    } else {
        CheckLine::new(CheckStatus::Warn, "daemon", "not running")
    };
    let version = check_version(paths, daemon_healthy.then_some(daemon_version).flatten());
    let host_skew = check_host_skew(paths);
    let mut ccteam = vec![
        ReportRow::visible(daemon),
        ReportRow::advisory(check_legacy_service()),
        ReportRow::visible(version),
    ];
    if host_skew.is_empty() {
        ccteam.push(ReportRow::advisory(CheckLine::new(
            CheckStatus::Pass,
            "hosts",
            "fleet versions aligned",
        )));
    } else {
        ccteam.extend(host_skew.into_iter().map(ReportRow::visible));
    }
    ccteam.push(ReportRow::visible(check_pricing()));
    ccteam.push(ReportRow::visible(check_home_layout(paths)));

    ReadinessReport {
        agents,
        ccteam,
        projects: vec![ReportRow::advisory(check_project_skill_faces(paths))],
        daemon_healthy,
    }
}

fn render_readiness(report: &ReadinessReport, color: bool) -> (String, bool) {
    let mut counts = Counts::default();
    let mut out = String::from("ccteam doctor — readiness checkup\n\n");

    render_section(&mut out, "agents", &report.agents, color, &mut counts);
    render_section(&mut out, "ccteam", &report.ccteam, color, &mut counts);
    render_section(&mut out, "projects", &report.projects, color, &mut counts);

    let any_fail = counts.fail > 0;
    out.push_str("summary: ");
    out.push_str(&counts.summary());
    if any_fail {
        out.push_str(" — NOT READY (fix the FAIL lines above)\n");
    } else {
        out.push_str(" — READY (WARN is informational, not blocking)\n");
    }
    if !report.daemon_healthy {
        out.push_str("\ndaemon not running — start it:  ccteam daemon start\n");
    }
    (out, any_fail)
}

fn render_section(
    out: &mut String,
    title: &str,
    rows: &[ReportRow],
    color: bool,
    counts: &mut Counts,
) {
    for row in rows {
        counts.add(row.line.status);
    }
    if !rows.iter().any(ReportRow::is_visible) {
        return;
    }
    out.push_str(title);
    out.push('\n');
    for row in rows.iter().filter(|row| row.is_visible()) {
        let token = status_token(row.line.status, color);
        out.push_str(&format!(
            "  {token} {:<10}{}\n",
            row.line.name, row.line.detail
        ));
    }
    out.push('\n');
}

fn status_token(status: CheckStatus, color: bool) -> String {
    let plain = format!("[{}]", status.label());
    if !color {
        return plain;
    }
    let ansi = match status {
        CheckStatus::Pass => "32",
        CheckStatus::Warn => "33",
        CheckStatus::Fail => "31",
        CheckStatus::Skip => "2",
    };
    format!("\x1b[{ansi}m{plain}\x1b[0m")
}

struct BinaryCheck {
    installed: bool,
    status: CheckStatus,
    detail: String,
}

struct AuthCheck {
    ok: Option<bool>,
    login_hint: &'static str,
}

struct McpCheck {
    status: CheckStatus,
    detail: String,
}

fn check_agent(
    name: &'static str,
    env_var: &str,
    default_bin: &str,
    required: bool,
    auth_check: fn() -> AuthCheck,
) -> CheckLine {
    let binary = probe_binary(env_var, default_bin, required);
    if !binary.installed {
        return CheckLine::new(binary.status, name, binary.detail);
    }

    let auth = auth_check();
    let mcp = check_vendor_mcp(name);
    let mut status = binary.status.worst(mcp.status);
    let mut details = vec![binary.detail];
    match auth.ok {
        Some(true) => details.push("auth ok".to_string()),
        Some(false) => {
            status = status.worst(CheckStatus::Warn);
            details.push(format!("auth missing — {}", auth.login_hint));
        }
        None => {}
    }
    details.push(mcp.detail);
    CheckLine::new(status, name, details.join(" · "))
}

/// Shared `<bin> --version` probe. Doctor executes only this read-only version
/// command; daemon-start's registration gate uses a separate no-exec resolver.
fn probe_binary(env_var: &str, default_bin: &str, required: bool) -> BinaryCheck {
    let bin = std::env::var(env_var).unwrap_or_else(|_| default_bin.to_string());
    match Command::new(&bin).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let detail = if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                format!("resolved `{bin}`")
            };
            BinaryCheck {
                installed: true,
                status: CheckStatus::Pass,
                detail,
            }
        }
        Ok(out) => {
            let reason = out
                .status
                .code()
                .map(|code| format!("exit {code}"))
                .unwrap_or_else(|| "terminated by signal".to_string());
            BinaryCheck {
                installed: false,
                status: if required {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Warn
                },
                detail: format!(
                    "`{bin} --version` failed ({reason}) — reinstall or point {env_var} at a working binary"
                ),
            }
        }
        Err(_) => {
            let detail = if required {
                format!("not installed — install the Claude Code CLI or point {env_var} at it")
            } else {
                format!("not installed (optional) — install it or point {env_var} at it")
            };
            BinaryCheck {
                installed: false,
                status: if required {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Warn
                },
                detail,
            }
        }
    }
}

fn check_vendor_auth_claude() -> AuthCheck {
    let home = std::env::var("CLAUDE_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    let ok = home.as_ref().is_some_and(|h| {
        h.join(".credentials.json").exists() || h.join("credentials.json").exists()
    }) || std::env::var("ANTHROPIC_API_KEY").is_ok();
    AuthCheck {
        ok: Some(ok),
        login_hint: "run `claude auth login` or set ANTHROPIC_API_KEY",
    }
}

// Daemon-start MCP auto-registration creates the vendor config files, so those
// files alone must never impersonate a successful vendor login.
fn check_vendor_auth_codex() -> AuthCheck {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")));
    let ok = home.as_ref().is_some_and(|h| h.join("auth.json").exists())
        || std::env::var("OPENAI_API_KEY").is_ok();
    AuthCheck {
        ok: Some(ok),
        login_hint: "run `codex login` or set OPENAI_API_KEY",
    }
}

fn check_vendor_auth_grok() -> AuthCheck {
    let ok = std::env::var("XAI_API_KEY").is_ok()
        || dirs::home_dir().is_some_and(|home| {
            home.join(".config/grok").exists()
                || dir_contains_entry_other_than(&home.join(".grok"), "config.toml")
        });
    AuthCheck {
        ok: ok.then_some(true),
        login_hint: "",
    }
}

fn check_vendor_auth_opencode() -> AuthCheck {
    let ok = std::env::var("OPENAI_API_KEY").is_ok()
        || dirs::home_dir().is_some_and(|home| {
            home.join(".local/share/opencode").exists()
                || home.join(".opencode").exists()
                || dir_contains_entry_other_than(&home.join(".config/opencode"), "opencode.json")
        });
    AuthCheck {
        ok: ok.then_some(true),
        login_hint: "",
    }
}

fn dir_contains_entry_other_than(dir: &std::path::Path, excluded: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name() != std::ffi::OsStr::new(excluded))
    })
}

fn check_vendor_auth_kimi() -> AuthCheck {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")));
    let ok = std::env::var("MOONSHOT_API_KEY").is_ok()
        || kimi_home
            .as_ref()
            .is_some_and(|h| h.join("credentials").exists() || h.join("oauth").exists());
    AuthCheck {
        ok: Some(ok),
        login_hint: "run `kimi login` or set MOONSHOT_API_KEY",
    }
}

fn check_vendor_mcp(vendor: &str) -> McpCheck {
    let resolved = match vendor {
        "claude" => ccteam_core::projects::resolve_claude_json_path().map(|path| {
            let registered = ccteam_core::mcp_register::claude_mcp_registered(&path);
            (path, registered)
        }),
        "codex" => ccteam_core::mcp_register::resolve_codex_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::codex_mcp_registered(&path);
            (path, registered)
        }),
        "grok" => ccteam_core::mcp_register::resolve_grok_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::grok_mcp_registered(&path);
            (path, registered)
        }),
        "opencode" => ccteam_core::mcp_register::resolve_opencode_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::opencode_mcp_registered(&path);
            (path, registered)
        }),
        "kimi" => ccteam_core::mcp_register::resolve_kimi_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::kimi_mcp_registered(&path);
            (path, registered)
        }),
        _ => unreachable!("doctor vendor list is closed"),
    };

    match resolved {
        Ok((_, true)) => McpCheck {
            status: CheckStatus::Pass,
            detail: "MCP registered".to_string(),
        },
        Ok((_, false)) => McpCheck {
            status: CheckStatus::Warn,
            detail: "MCP not registered — auto-registers at `ccteam daemon start` (or `ccteam config mcp`)".to_string(),
        },
        Err(err) => McpCheck {
            status: CheckStatus::Warn,
            detail: format!("MCP state unknown ({err})"),
        },
    }
}

/// Residual legacy systemd/launchd unit detection. The clean result is hidden;
/// detected units keep the existing migration text.
fn check_legacy_service() -> CheckLine {
    let paths = crate::legacy_takeover::LegacyServicePaths::from_env();
    match crate::legacy_takeover::detect_legacy_unit(&paths) {
        None => CheckLine::new(
            CheckStatus::Pass,
            "legacy",
            "no legacy service unit",
        ),
        Some((path, true)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy",
            format!(
                "legacy installer-written ccteam unit at {} — systemd/launchd management is retired; migrate with `ccteam daemon start` (auto-takeover: stops + removes the unit, restarts detached)",
                path.display()
            ),
        ),
        Some((path, false)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy",
            format!(
                "service unit at {} was not written by the ccteam installer — ccteam will not manage or delete it; its instance counts as \"not managed\" for `ccteam daemon stop`. Remove it manually if you want ccteam self-management",
                path.display()
            ),
        ),
    }
}

fn check_version(paths: &CcteamPaths, daemon_version: Option<String>) -> CheckLine {
    let channel = ccteam_core::install_channel::detect(paths);
    let binary_version = env!("CARGO_PKG_VERSION");
    let cache = ccteam_core::version_check::cached_latest(paths).unwrap_or_default();
    let update_available = ccteam_core::version_check::update_available(&cache, binary_version);
    let mut status = CheckStatus::Pass;
    let latest = match update_available {
        Some(latest) => {
            status = CheckStatus::Warn;
            format!("update available → {latest}: run `ccteam update`")
        }
        None if cache.latest_version.is_some() => "up to date".to_string(),
        None => "latest not checked yet".to_string(),
    };
    let mut detail = format!("{binary_version} ({}) · {latest}", channel.as_str());
    if let Some(version) = daemon_version.filter(|version| version != binary_version) {
        status = CheckStatus::Warn;
        detail.push_str(&format!(
            " · daemon runs {version} — restart: `ccteam daemon restart`"
        ));
    }
    CheckLine::new(status, "version", detail)
}

fn check_host_skew(paths: &CcteamPaths) -> Vec<CheckLine> {
    crate::update::fleet_version_skew(paths, env!("CARGO_PKG_VERSION"))
        .into_iter()
        .map(|detail| CheckLine::new(CheckStatus::Warn, "hosts", detail))
        .collect()
}

fn check_pricing() -> CheckLine {
    const WARN_DAYS: i64 = 180;
    let today = chrono::Utc::now().date_naive();
    let mut worst: Option<i64> = None;
    for &vendor in Vendor::ALL {
        let raw = ccteam_core::pricing_schema_version_for(vendor);
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            let age = (today - date).num_days();
            if worst.is_none_or(|current| age > current) {
                worst = Some(age);
            }
        }
    }
    match worst {
        Some(age) if age > WARN_DAYS => CheckLine::new(
            CheckStatus::Warn,
            "pricing",
            format!("rate sheet {age}d old (>{WARN_DAYS}d) — upgrade ccteam for current pricing"),
        ),
        Some(age) => CheckLine::new(
            CheckStatus::Pass,
            "pricing",
            format!("rate sheet {age}d old (fresh)"),
        ),
        None => CheckLine::new(
            CheckStatus::Skip,
            "pricing",
            "could not parse embedded schema_version",
        ),
    }
}

fn check_home_layout(paths: &CcteamPaths) -> CheckLine {
    if !paths.root.exists() {
        return CheckLine::new(
            CheckStatus::Skip,
            "home",
            format!(
                "{} does not exist yet (fresh install)",
                paths.root.display()
            ),
        );
    }
    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return CheckLine::new(
            CheckStatus::Skip,
            "home",
            format!("could not read {}", paths.root.display()),
        );
    };
    let mut unexpected = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !ccteam_core::canonical_home_dirs().contains(&name) {
            unexpected.push(name.to_string());
        }
    }
    if unexpected.is_empty() {
        CheckLine::new(
            CheckStatus::Pass,
            "home",
            format!("{} matches the canonical layout", paths.root.display()),
        )
    } else {
        unexpected.sort();
        CheckLine::new(
            CheckStatus::Warn,
            "home",
            format!(
                "{} unexpected dir(s) under {} (orchestrator-era leftovers, safe to `rm -rf`): {}",
                unexpected.len(),
                paths.root.display(),
                unexpected.join(", ")
            ),
        )
    }
}

/// Advisory-only scan for registered projects that still use a real legacy
/// `.claude/skills` directory. A clean result is counted but hidden.
fn check_project_skill_faces(paths: &CcteamPaths) -> CheckLine {
    let config = match ccteam_core::load_ccteam_config(&paths.root) {
        Ok(config) => config,
        Err(err) => {
            return CheckLine::new(
                CheckStatus::Skip,
                "skills",
                format!("could not read registered projects: {err}"),
            );
        }
    };
    let mut legacy = Vec::new();
    for project in config.projects {
        let face = project.path.join(".claude/skills");
        if matches!(
            std::fs::symlink_metadata(&face),
            Ok(metadata) if metadata.file_type().is_dir()
        ) {
            legacy.push(project.slug);
        }
    }
    if legacy.is_empty() {
        CheckLine::new(CheckStatus::Pass, "skills", "no legacy project skill dirs")
    } else {
        legacy.sort();
        CheckLine::new(
            CheckStatus::Warn,
            "skills",
            format!(
                "legacy .claude/skills dir in: {} — migrate: `ccteam skill migrate-project --project <slug>`",
                legacy.join(", ")
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_daemon(daemon_healthy: bool) -> ReadinessReport {
        ReadinessReport {
            agents: vec![ReportRow::visible(CheckLine::new(
                CheckStatus::Pass,
                "claude",
                "test",
            ))],
            ccteam: vec![ReportRow::visible(CheckLine::new(
                if daemon_healthy {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warn
                },
                "daemon",
                if daemon_healthy {
                    "running"
                } else {
                    "not running"
                },
            ))],
            projects: Vec::new(),
            daemon_healthy,
        }
    }

    #[test]
    fn renderer_healthy_daemon_ends_on_summary_without_start_hint() {
        let (output, any_fail) = render_readiness(&report_with_daemon(true), false);
        assert!(!any_fail);
        assert!(!output.contains("daemon not running — start it"));
        assert!(output
            .lines()
            .rfind(|line| !line.is_empty())
            .is_some_and(|line| line.starts_with("summary:")));
    }

    #[test]
    fn renderer_unhealthy_daemon_ends_on_start_hint() {
        let (output, any_fail) = render_readiness(&report_with_daemon(false), false);
        assert!(!any_fail);
        assert_eq!(
            output.lines().rfind(|line| !line.is_empty()),
            Some("daemon not running — start it:  ccteam daemon start")
        );
    }

    #[test]
    fn renderer_counts_a_suppressed_clean_advisory() {
        let report = ReadinessReport {
            agents: Vec::new(),
            ccteam: vec![ReportRow::advisory(CheckLine::new(
                CheckStatus::Pass,
                "legacy",
                "clean but hidden",
            ))],
            projects: Vec::new(),
            daemon_healthy: true,
        };
        let (output, any_fail) = render_readiness(&report, false);
        assert!(!any_fail);
        assert!(output.contains("summary: 1 pass — READY"));
        assert!(!output.contains("clean but hidden"));
    }
}
