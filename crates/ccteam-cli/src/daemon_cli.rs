//! v0.9.7 — `ccteam daemon <start|stop|restart|status|logs>` handlers.
//!
//! Thin CLI layer over `ccteam_core::daemon` (the lifecycle core): each
//! verb takes the operation lock (mutating verbs only), runs the legacy
//! systemd/launchd takeover pre-step where relevant, and owns the
//! machine contract:
//!
//! - `--json` → EXACTLY one line of JSON on stdout
//!   (`status ∈ started|alreadyRunning|stopped|notRunning|restarted|skippedNotManaged`,
//!   or `{"status":"error","code":…,"message":…}`); human prose goes to
//!   stderr.
//! - without `--json` → human prose on stdout.
//! - deterministic failures exit 1 after emitting the JSON/human error.

use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use ccteam_core::{daemon as dcore, CcteamPaths};

use crate::legacy_takeover;

/// Hidden test hook: override the program `daemon start` detaches
/// (default `canonicalize(current_exe())`). Same convention as
/// `CCTEAM_{CLAUDE,CODEX}_BIN`.
pub const DAEMON_BIN_ENV: &str = "CCTEAM_DAEMON_BIN";
/// Default embedded-web bind, shared by the daemon verbs' clap defaults
/// and `ccteam update`'s upgrade-restart (which has no `--web-bind` of its
/// own — same posture as `daemon restart`).
pub const DEFAULT_WEB_BIND: &str = "0.0.0.0:7331";
pub const DEFAULT_DSH_WEB_BIND: &str = "0.0.0.0:7332";
/// Hidden test hooks: shrink the ready-wait / stop-wait budgets so
/// failure-path integration tests don't burn the production timeouts.
const READY_TIMEOUT_ENV: &str = "CCTEAM_DAEMON_READY_TIMEOUT_MS";
const STOP_WAIT_ENV: &str = "CCTEAM_DAEMON_STOP_WAIT_MS";

fn env_duration_ms(key: &str, default: Duration) -> Duration {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// One JSON line on stdout (json mode) and/or prose. In `--json` mode
/// prose is demoted to stderr so stdout stays a single machine line.
fn emit(json: bool, machine: serde_json::Value, human: &str) {
    if json {
        println!("{machine}");
        if !human.is_empty() {
            eprintln!("{human}");
        }
    } else if !human.is_empty() {
        println!("{human}");
    }
}

/// Emit a deterministic failure per the machine contract and exit 1.
fn fail(json: bool, code: &str, message: &str) -> ! {
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": "error", "code": code, "message": message })
        );
    }
    eprintln!("ccteam daemon: {message}");
    std::process::exit(1);
}

fn error_code(err: &anyhow::Error) -> &'static str {
    err.downcast_ref::<dcore::LifecycleError>()
        .map(|e| e.code)
        .unwrap_or("error")
}

/// Resolve what to detach: `CCTEAM_DAEMON_BIN` override (tests) or the
/// on-disk current executable (so a symlinked launcher pins the real
/// binary for the daemon's lifetime). Routed through
/// `current_ccteam_bin()` so that when `ccteam update` swaps the binary
/// under the running updater, we detach the NEW file on disk rather than
/// the deleted inode `current_exe()` reports as `<path> (deleted)`.
fn spawn_program() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(DAEMON_BIN_ENV) {
        return Ok(PathBuf::from(p));
    }
    ccteam_core::current_ccteam_bin().context("resolve daemon binary to detach")
}

fn start_spec(
    paths: &CcteamPaths,
    web_bind: &str,
    dsh_web_bind: Option<&str>,
) -> Result<dcore::DaemonStartSpec> {
    let dsh_web_bind = effective_dsh_web_bind(web_bind, dsh_web_bind)?;
    Ok(dcore::DaemonStartSpec {
        program: spawn_program()?,
        args: vec![
            "start".to_string(),
            "--web-bind".to_string(),
            web_bind.to_string(),
            "--dsh-web-bind".to_string(),
            dsh_web_bind,
        ],
        log_path: dcore::daemon_log_path(paths),
        ready_timeout: env_duration_ms(READY_TIMEOUT_ENV, dcore::START_READY_TIMEOUT),
    })
}

fn effective_dsh_web_bind(web_bind: &str, dsh_web_bind: Option<&str>) -> Result<String> {
    match dsh_web_bind {
        Some(value) if value.eq_ignore_ascii_case("off") => Ok("off".to_string()),
        Some(value) => {
            let _: std::net::SocketAddr = value.parse().with_context(|| {
                format!("--dsh-web-bind {value} is not a valid socket address or `off`")
            })?;
            Ok(value.to_string())
        }
        None => {
            let web: std::net::SocketAddr = web_bind
                .parse()
                .with_context(|| format!("--web-bind {web_bind} is not a valid socket address"))?;
            let port = web.port().checked_add(1).context(
                "--dsh-web-bind default cannot be derived because --web-bind uses port 65535",
            )?;
            Ok(std::net::SocketAddr::new(web.ip(), port).to_string())
        }
    }
}

fn stop_tuning() -> dcore::StopTuning {
    dcore::StopTuning {
        term_wait: env_duration_ms(STOP_WAIT_ENV, dcore::STOP_TERM_WAIT),
        ..dcore::StopTuning::default()
    }
}

/// Run the legacy systemd/launchd takeover pre-step (idempotent; PRD
/// F4). Best-effort: a takeover hiccup is reported but never blocks the
/// start itself. All output → stderr (diagnostics, both modes).
///
/// `pub(crate)` so `ccteam update`'s upgrade-restart contract runs the
/// same takeover pre-step before it restarts (PRD F4 layer ③).
pub(crate) fn takeover_pre_step() {
    match legacy_takeover::run_takeover_from_env() {
        Ok(legacy_takeover::TakeoverOutcome::NothingToDo) => {}
        Ok(legacy_takeover::TakeoverOutcome::Migrated { unit, actions }) => {
            eprintln!(
                "ccteam daemon: migrated from systemd/launchd to ccteam self-managed \
                 (installer-written unit {} taken over):",
                unit.display()
            );
            for action in actions {
                eprintln!("  - {action}");
            }
        }
        Ok(legacy_takeover::TakeoverOutcome::ForeignUnitPresent { unit }) => {
            eprintln!(
                "ccteam daemon: found a service unit at {} that was NOT written by the ccteam \
                 installer — leaving it alone. ccteam treats an instance it supervises as \
                 \"not managed\"; remove the unit yourself if you want ccteam self-management.",
                unit.display()
            );
        }
        Err(err) => {
            eprintln!("ccteam daemon: legacy service takeover failed (continuing): {err:#}");
        }
    }
}

/// Human-facing pointer printed after a successful start.
fn web_hint(web_bind: &str) -> String {
    let port = web_bind.rsplit(':').next().unwrap_or("7331");
    let host = crate::first_lan_ipv4()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "localhost".to_string());
    format!(
        "web console: http://{host}:{port}/  (run `ccteam status` for the tokenized login link)\n\
         logs:        ccteam daemon logs -f"
    )
}

pub fn run_daemon_start(web_bind: &str, dsh_web_bind: Option<&str>, json: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    takeover_pre_step();
    let _lock = match dcore::acquire_operation_lock(&paths) {
        Ok(lock) => lock,
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    };
    let spec = start_spec(&paths, web_bind, dsh_web_bind)?;
    match dcore::start_managed(&paths, &spec) {
        Ok(dcore::StartVerdict::Started { pid, version }) => {
            let v = version.clone().unwrap_or_else(|| "unknown".into());
            emit(
                json,
                serde_json::json!({ "status": "started", "pid": pid, "version": version }),
                &format!(
                    "ccteam daemon started (pid {pid}, version {v}).\n{}",
                    web_hint(web_bind)
                ),
            );
        }
        Ok(dcore::StartVerdict::AlreadyRunning { version }) => {
            let v = version.clone().unwrap_or_else(|| "unknown".into());
            emit(
                json,
                serde_json::json!({ "status": "alreadyRunning", "version": version }),
                &format!("ccteam daemon already running (version {v})."),
            );
        }
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    }
    Ok(())
}

pub fn run_daemon_stop(force: bool, json: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let _lock = match dcore::acquire_operation_lock(&paths) {
        Ok(lock) => lock,
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    };
    match dcore::stop_managed_with(&paths, force, stop_tuning()) {
        Ok(dcore::StopVerdict::Stopped { pid }) => {
            emit(
                json,
                serde_json::json!({ "status": "stopped", "pid": pid }),
                &format!(
                    "ccteam daemon stopped (pid {pid}). Agent sessions are NOT killed — \
                     the next `ccteam daemon start` reattaches to them."
                ),
            );
        }
        Ok(dcore::StopVerdict::NotRunning) => {
            emit(
                json,
                serde_json::json!({ "status": "notRunning" }),
                "no managed ccteam daemon is running.",
            );
        }
        Ok(dcore::StopVerdict::RefusedNotManaged { hint }) => {
            fail(json, "notManaged", &hint);
        }
        Ok(dcore::StopVerdict::TimedOut { pid }) => {
            let extra = if force {
                "even SIGKILL did not reap it; inspect the process manually"
            } else {
                "retry with `ccteam daemon stop --force` to escalate to SIGKILL \
                 (daemon process only; agent sessions are never touched)"
            };
            fail(
                json,
                "stopTimeout",
                &format!("daemon pid {pid} is still alive after the stop wait; {extra}"),
            );
        }
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    }
    Ok(())
}

/// Outcome of the reusable managed-restart core ([`restart_managed`]).
/// Refusals / timeouts are verdicts (not panics) so both callers
/// (`daemon restart`, `ccteam update`) own their own exit-code / JSON
/// mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestartOutcome {
    /// Was running → SIGTERM'd → started again.
    Restarted { pid: u32, version: Option<String> },
    /// Nothing was running → freshly started.
    Started { pid: u32, version: Option<String> },
    /// A ready but not-managed instance holds the socket — refused.
    NotManaged { hint: String },
    /// The managed daemon did not exit within the stop wait.
    StopTimedOut { pid: u32 },
    /// After the stop, an unmanaged instance already serves the socket.
    AlreadyServing { version: Option<String> },
}

/// Reusable restart core: acquire the operation lock, then stop (if
/// managed) + start under that ONE lock so no concurrent lifecycle op can
/// interleave. The CALLER runs any takeover pre-step first
/// (`daemon start/restart` and `ccteam update` all do). Shared by
/// [`run_daemon_restart`] and the `ccteam update` upgrade-restart contract
/// so the lock/stop/start logic lives in exactly one place.
pub(crate) fn restart_managed(
    paths: &CcteamPaths,
    web_bind: &str,
    dsh_web_bind: Option<&str>,
) -> Result<RestartOutcome> {
    // ONE lock across stop + start.
    let _lock = dcore::acquire_operation_lock(paths)?;
    let was_running = match dcore::stop_managed_with(paths, false, stop_tuning())? {
        dcore::StopVerdict::Stopped { .. } => true,
        dcore::StopVerdict::NotRunning => false,
        dcore::StopVerdict::RefusedNotManaged { hint } => {
            return Ok(RestartOutcome::NotManaged { hint })
        }
        dcore::StopVerdict::TimedOut { pid } => return Ok(RestartOutcome::StopTimedOut { pid }),
    };
    let spec = start_spec(paths, web_bind, dsh_web_bind)?;
    match dcore::start_managed(paths, &spec)? {
        dcore::StartVerdict::Started { pid, version } => Ok(if was_running {
            RestartOutcome::Restarted { pid, version }
        } else {
            RestartOutcome::Started { pid, version }
        }),
        dcore::StartVerdict::AlreadyRunning { version } => {
            Ok(RestartOutcome::AlreadyServing { version })
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum RestartCommandAction {
    Emit {
        machine: serde_json::Value,
        human: String,
    },
    Fail {
        code: &'static str,
        message: String,
    },
}

/// Shared success rendering for `daemon restart` (status = `restarted`
/// when a daemon was running, `started` when none was).
fn restart_started_action(
    status: &str,
    pid: u32,
    version: Option<String>,
    web_bind: &str,
) -> RestartCommandAction {
    let v = version.clone().unwrap_or_else(|| "unknown".into());
    RestartCommandAction::Emit {
        machine: serde_json::json!({ "status": status, "pid": pid, "version": version }),
        human: format!(
            "ccteam daemon {status} (pid {pid}, version {v}).\n{}",
            web_hint(web_bind)
        ),
    }
}

fn restart_command_action(
    outcome: RestartOutcome,
    if_managed: bool,
    web_bind: &str,
) -> RestartCommandAction {
    match outcome {
        RestartOutcome::Restarted { pid, version } => {
            restart_started_action("restarted", pid, version, web_bind)
        }
        RestartOutcome::Started { pid, version } => {
            // Nothing was running before this restart — it was a plain start.
            restart_started_action("started", pid, version, web_bind)
        }
        RestartOutcome::AlreadyServing { version } => RestartCommandAction::Emit {
            machine: serde_json::json!({ "status": "alreadyRunning", "version": version }),
            human: "a daemon is already serving the socket (not spawned by this restart)."
                .to_string(),
        },
        RestartOutcome::NotManaged { hint } if if_managed => {
            let hint = format!(
                "{hint}; the newly installed binary is NOT live until you restart that daemon \
                 yourself"
            );
            RestartCommandAction::Emit {
                machine: serde_json::json!({
                    "status": "skippedNotManaged",
                    "hint": hint,
                }),
                human: format!("warning: {hint}"),
            }
        }
        RestartOutcome::NotManaged { hint } => RestartCommandAction::Fail {
            code: "notManaged",
            message: hint,
        },
        RestartOutcome::StopTimedOut { pid } => RestartCommandAction::Fail {
            code: "stopTimeout",
            message: format!(
                "daemon pid {pid} did not exit within the stop wait; restart aborted \
                 (`ccteam daemon stop --force` can escalate)"
            ),
        },
    }
}

pub fn run_daemon_restart(
    web_bind: &str,
    dsh_web_bind: Option<&str>,
    json: bool,
    if_managed: bool,
) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // Restart is the verb `make install` runs on upgraded dev boxes, so
    // it carries the same takeover pre-step as start.
    takeover_pre_step();
    let outcome = match restart_managed(&paths, web_bind, dsh_web_bind) {
        Ok(outcome) => outcome,
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    };
    match restart_command_action(outcome, if_managed, web_bind) {
        RestartCommandAction::Emit { machine, human } => emit(json, machine, &human),
        RestartCommandAction::Fail { code, message } => fail(json, code, &message),
    }
    Ok(())
}

pub fn run_daemon_status(json: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let report = dcore::daemon_status(&paths);
    let binary_version = env!("CARGO_PKG_VERSION");
    let pid = report.record.as_ref().map(|r| r.pid);
    let machine = serde_json::json!({
        "ready": report.ready,
        "managed": report.managed,
        "pid": report.managed.then_some(pid).flatten(),
        "runningVersion": report.running_version,
        "binaryVersion": binary_version,
        "socket": report.socket.display().to_string(),
    });

    let mut human = String::from("ccteam daemon status\n");
    human.push_str(&format!(
        "  ready:   {}  ({})\n",
        if report.ready { "yes" } else { "no" },
        report.socket.display()
    ));
    match (&report.record, report.managed) {
        (Some(r), true) => human.push_str(&format!(
            "  managed: yes  (pid {}, started {})\n",
            r.pid, r.started_at
        )),
        (Some(_), false) if report.ready => human.push_str(
            "  managed: no   (stale pid record; the serving instance was not started by \
             `ccteam daemon start`)\n",
        ),
        (Some(_), false) => human.push_str("  managed: no   (stale pid record)\n"),
        (None, _) if report.ready => human.push_str(
            "  managed: no   (foreground `ccteam start` or a self-supervised instance)\n",
        ),
        (None, _) => human.push_str("  managed: no\n"),
    }
    match &report.running_version {
        Some(v) if v == binary_version => {
            human.push_str(&format!(
                "  version: running {v} / binary {binary_version}\n"
            ));
        }
        Some(v) => {
            human.push_str(&format!(
                "  version: running {v} / binary {binary_version}  \
                 (RESTART NEEDED: `ccteam daemon restart` to load the new binary)\n"
            ));
        }
        None => {
            human.push_str(&format!(
                "  version: running -  / binary {binary_version}\n"
            ));
        }
    }
    if !report.ready {
        human.push_str("  hint:    start it with `ccteam daemon start`\n");
    }
    emit(json, machine, human.trim_end());
    Ok(())
}

pub fn run_daemon_logs(lines: usize, follow: bool, json: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let path = dcore::daemon_log_path(&paths);
    if follow && json {
        fail(json, "badArgs", "--json cannot be combined with --follow");
    }
    if !path.exists() {
        emit(
            json,
            serde_json::json!({ "path": path.display().to_string(), "lines": [] }),
            &format!(
                "no daemon log yet at {} (it appears on the first `ccteam daemon start`).",
                path.display()
            ),
        );
        return Ok(());
    }

    let tail = tail_lines(&path, lines)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "path": path.display().to_string(), "lines": tail })
        );
        return Ok(());
    }
    for line in &tail {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }

    // Follow: poll for appended bytes until the process is interrupted.
    let mut file =
        std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut offset = file.metadata().map(|m| m.len()).unwrap_or(0);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let len = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if len < offset {
            // Truncated/rotated externally — restart from the top.
            offset = 0;
        }
        if len > offset {
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = Vec::new();
            let reader = std::io::BufReader::new(&mut file);
            for line in reader.lines() {
                match line {
                    Ok(l) => buf.push(l),
                    Err(_) => break,
                }
            }
            for l in &buf {
                println!("{l}");
            }
            offset = len;
        }
    }
}

/// Last `n` lines of a file, reading only a bounded tail window (the
/// daemon log is unrotated and can grow large).
fn tail_lines(path: &std::path::Path, n: usize) -> Result<Vec<String>> {
    const WINDOW: u64 = 1024 * 1024;
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(WINDOW);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    // Drop a partial first line when the window cut mid-line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let keep = lines.len().saturating_sub(n);
    Ok(lines[keep..].iter().map(|s| s.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emitted(action: RestartCommandAction) -> (serde_json::Value, String) {
        match action {
            RestartCommandAction::Emit { machine, human } => (machine, human),
            other => panic!("expected successful emit, got {other:?}"),
        }
    }

    fn failed(action: RestartCommandAction) -> (&'static str, String) {
        match action {
            RestartCommandAction::Fail { code, message } => (code, message),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn restart_if_managed_skips_unmanaged_with_loud_drift_warning() {
        let original_hint = "the socket belongs to a foreground daemon";
        let (machine, human) = emitted(restart_command_action(
            RestartOutcome::NotManaged {
                hint: original_hint.to_string(),
            },
            true,
            DEFAULT_WEB_BIND,
        ));

        assert_eq!(machine["status"], "skippedNotManaged");
        let machine_hint = machine["hint"].as_str().expect("JSON hint");
        for rendered in [machine_hint, human.as_str()] {
            assert!(
                rendered.contains(original_hint),
                "missing original hint: {rendered}"
            );
            assert!(
                rendered.contains(
                    "the newly installed binary is NOT live until you restart that daemon yourself"
                ),
                "missing deploy-drift warning: {rendered}"
            );
        }
        assert!(
            human.starts_with("warning:"),
            "warning must be loud: {human}"
        );
    }

    #[test]
    fn restart_if_managed_preserves_restarted_and_started_successes() {
        for (outcome, expected_status, expected_pid, expected_version) in [
            (
                RestartOutcome::Restarted {
                    pid: 41,
                    version: Some("0.10.0".to_string()),
                },
                "restarted",
                41,
                "0.10.0",
            ),
            (
                RestartOutcome::Started {
                    pid: 42,
                    version: Some("0.10.1".to_string()),
                },
                "started",
                42,
                "0.10.1",
            ),
        ] {
            let (machine, human) = emitted(restart_command_action(outcome, true, "127.0.0.1:7331"));
            assert_eq!(machine["status"], expected_status);
            assert_eq!(machine["pid"], expected_pid);
            assert_eq!(machine["version"], expected_version);
            assert!(human.starts_with(&format!(
                "ccteam daemon {expected_status} (pid {expected_pid}, version {expected_version})."
            )));
            assert!(human.contains("web console:"));
            assert!(human.contains("logs:        ccteam daemon logs -f"));
        }
    }

    #[test]
    fn restart_if_managed_keeps_stop_timeout_fatal() {
        let (code, message) = failed(restart_command_action(
            RestartOutcome::StopTimedOut { pid: 99 },
            true,
            DEFAULT_WEB_BIND,
        ));

        assert_eq!(code, "stopTimeout");
        assert_eq!(
            message,
            "daemon pid 99 did not exit within the stop wait; restart aborted \
             (`ccteam daemon stop --force` can escalate)"
        );
    }

    #[test]
    fn restart_without_if_managed_keeps_unmanaged_failure_unchanged() {
        let hint = "existing unmanaged-daemon guidance";
        let (code, message) = failed(restart_command_action(
            RestartOutcome::NotManaged {
                hint: hint.to_string(),
            },
            false,
            DEFAULT_WEB_BIND,
        ));

        assert_eq!(code, "notManaged");
        assert_eq!(message, hint);
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("log");
        let body: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        std::fs::write(&path, body.join("\n")).unwrap();
        let tail = tail_lines(&path, 3).unwrap();
        assert_eq!(tail, vec!["line 98", "line 99", "line 100"]);
        // n larger than the file → the whole file.
        assert_eq!(tail_lines(&path, 1000).unwrap().len(), 100);
    }
}
