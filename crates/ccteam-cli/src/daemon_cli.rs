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
/// and [`LauncherFlags::default`]. The DSH companion bind has no constant
/// of its own: it is always derived as `web port + 1`
/// ([`effective_dsh_web_bind`]), so a non-default web bind can never be
/// paired with a stale hardcoded companion port.
pub const DEFAULT_WEB_BIND: &str = "0.0.0.0:7331";
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

/// v0.10.5 D7 — the daemon's runtime flags as the launcher holds them.
/// Plain data (no clap) so `ccteam update`'s restart path can build one
/// without a parsed command line, and so [`daemon_run_argv`] is a pure
/// function the unit tests can pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherFlags {
    pub no_web: bool,
    pub no_imd: bool,
    pub web_bind: String,
    pub dsh_web_bind: Option<String>,
    pub web_no_auth: bool,
    pub web_token_file: Option<PathBuf>,
    pub no_clipboard: bool,
}

impl Default for LauncherFlags {
    fn default() -> Self {
        Self {
            no_web: false,
            no_imd: false,
            web_bind: DEFAULT_WEB_BIND.to_string(),
            dsh_web_bind: None,
            web_no_auth: false,
            web_token_file: None,
            no_clipboard: false,
        }
    }
}

/// argv (program excluded) for the detached daemon. v0.10.5 D7: the exec
/// target is the hidden `internal daemon-run`, NOT `start` — `start` is
/// now the launcher itself, so pointing the launcher at it would fork
/// bomb. Pure so the forwarding contract is unit-tested without spawning.
pub(crate) fn daemon_run_argv(flags: &LauncherFlags) -> Result<Vec<String>> {
    let dsh_web_bind = effective_dsh_web_bind(&flags.web_bind, flags.dsh_web_bind.as_deref())?;
    let mut args = vec![
        "internal".to_string(),
        "daemon-run".to_string(),
        "--web-bind".to_string(),
        flags.web_bind.clone(),
        "--dsh-web-bind".to_string(),
        dsh_web_bind,
    ];
    if flags.no_web {
        args.push("--no-web".to_string());
    }
    if flags.no_imd {
        args.push("--no-imd".to_string());
    }
    if flags.web_no_auth {
        args.push("--web-no-auth".to_string());
    }
    if let Some(path) = &flags.web_token_file {
        args.push("--web-token-file".to_string());
        args.push(path.display().to_string());
    }
    if flags.no_clipboard {
        args.push("--no-clipboard".to_string());
    }
    Ok(args)
}

fn start_spec(paths: &CcteamPaths, args: Vec<String>) -> Result<dcore::DaemonStartSpec> {
    Ok(dcore::DaemonStartSpec {
        program: spawn_program()?,
        args,
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
/// `web_bind` here must be what the daemon SERVES: with `--web-bind
/// 127.0.0.1:0` the requested string ends in `:0`, and printing a URL
/// with port 0 sends the operator to an address nothing listens on.
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

pub fn run_daemon_start(flags: &LauncherFlags, json: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // v0.10.5 — `home` rides on BOTH verdicts, not just `started`: a
    // plugin treats "I started it" and "it was already up" as the same
    // success and then has to answer one question — is this daemon MY
    // home's? Answering it on only one branch would leave the other
    // needing a second lookup to be safe.
    let home = canonical_home(&paths);
    takeover_pre_step();
    let _lock = match dcore::acquire_operation_lock(&paths) {
        Ok(lock) => lock,
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    };
    let spec = start_spec(&paths, daemon_run_argv(flags)?)?;
    match dcore::start_managed(&paths, &spec) {
        Ok(dcore::StartVerdict::Started { pid, version }) => {
            let v = version.clone().unwrap_or_else(|| "unknown".into());
            emit(
                json,
                serde_json::json!({
                    "status": "started",
                    "pid": pid,
                    "version": version,
                    "home": home,
                }),
                &format!(
                    "ccteam daemon started (pid {pid}, version {v}).\n{}",
                    // The daemon is ready by now, so ask it where it
                    // actually landed rather than echoing the request.
                    web_hint(
                        served_web_bind(&paths)
                            .as_deref()
                            .unwrap_or(&flags.web_bind)
                    )
                ),
            );
        }
        Ok(dcore::StartVerdict::AlreadyRunning { version, pid }) => {
            let v = version.clone().unwrap_or_else(|| "unknown".into());
            let who = pid
                .map(|p| format!("pid {p}"))
                .unwrap_or_else(|| "pid unknown — not started by the launcher".to_string());
            emit(
                json,
                serde_json::json!({
                    "status": "alreadyRunning",
                    "pid": pid,
                    "version": version,
                    "home": home,
                }),
                &format!("ccteam daemon already running ({who}, version {v}) in {home}."),
            );
        }
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    }
    Ok(())
}

/// How long we wait on the running daemon's `/health`. It is a loopback
/// GET against a process we already know is answering its MCP socket, so
/// a second is generous; the point of the cap is that `ccteam daemon
/// status` must stay fast when the answer is "not reachable".
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// Ask the RUNNING daemon who it is.
///
/// The identity source of truth is the live process, never the launcher's
/// recorded argv: `--web-bind 127.0.0.1:0` records `:0` and *serves*
/// `127.0.0.1:46303`, so reporting the record would report a request as
/// if it were a fact. `run/daemon-endpoint.json` supplies only the
/// address; `/health` supplies the answer.
///
/// `None` whenever the daemon cannot be reached that way (web disabled
/// with `--no-web`, a bind this host cannot dial, a build older than the
/// identity surface). Callers must then report *unknown*, never a guess.
pub(crate) fn live_health(paths: &CcteamPaths) -> Option<serde_json::Value> {
    let endpoint = dcore::read_endpoint(&dcore::endpoint_path(paths))?;
    let addr: std::net::SocketAddr = endpoint.web_bind.parse().ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    // An unspecified bind (0.0.0.0) is reachable on loopback; a specific
    // address is dialled as published.
    let host = if addr.ip().is_unspecified() {
        std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            addr.port(),
        )
    } else {
        addr
    };
    let body: serde_json::Value = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(HEALTH_TIMEOUT)
            .build()
            .ok()?;
        client
            .get(format!("http://{host}/health"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()
    })?;
    // A stale publication can outlive its daemon and point at whatever now
    // owns the port. Only an answer from OUR home is this daemon's.
    (body.get("home").and_then(|v| v.as_str()) == Some(canonical_home(paths).as_str()))
        .then_some(body)
}

/// The address the running daemon serves, for prose that would otherwise
/// echo the request. `None` = unknown; callers fall back to what was
/// asked for, which is at least honest about being the request.
fn served_web_bind(paths: &CcteamPaths) -> Option<String> {
    live_health(paths)?
        .get("web_bind")?
        .as_str()
        .map(str::to_string)
}

/// Canonical `$CCTEAM_HOME` as a string — the same resolution
/// `GET /health` reports, so a client comparing the two never sees a
/// symlinked path differ from a resolved one.
fn canonical_home(paths: &CcteamPaths) -> String {
    std::fs::canonicalize(&paths.root)
        .unwrap_or_else(|_| paths.root.clone())
        .display()
        .to_string()
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
                    "ccteam daemon stopped (pid {pid}). Agent sessions are NOT killed: idle ones \
                     exit on their own; a session mid-turn keeps working and the next `ccteam \
                     daemon start` picks it up by its body record (waits for it, then recovers \
                     its answer) — never a second process for the same session."
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
                 (daemon process only; agent session bodies are never touched — the next \
                 start finds them by their body records)"
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
    /// A replay restart was asked for but the running daemon's invocation
    /// could not be reconstructed. NOTHING was stopped.
    ReplayUnavailable { hint: String },
}

/// How a replay restart reconstructs the invocation to bring back, decided
/// BEFORE the daemon is stopped so a refusal costs nothing. Pure over its
/// two inputs so every rung is unit-tested without a daemon.
///
/// Rungs, in order of fidelity:
/// 1. the launcher's recorded argv — the only source carrying EVERY flag
///    (`--no-web`, `--no-imd`, `--web-token-file`, …);
/// 2. the binds the running daemon reports on `/health` — fewer flags,
///    but the addresses are the ones actually being served;
/// 3. refuse. Falling through to the compiled-in defaults is what moved a
///    daemon serving `127.0.0.1:17436` onto `0.0.0.0:7331` and killed
///    `/health` on the original port. "Restart" may never mean "restart
///    somewhere else".
pub(crate) fn plan_replay(
    recorded: Option<Vec<String>>,
    health: Option<&serde_json::Value>,
) -> std::result::Result<Vec<String>, String> {
    if let Some(args) = recorded {
        return Ok(args);
    }
    let web_bind = health
        .and_then(|h| h.get("web_bind"))
        .and_then(|v| v.as_str());
    if let Some(web_bind) = web_bind {
        // A daemon with the companion proxy disabled reports null; `off`
        // is how that is spelled on the command line.
        let dsh_web_bind = health
            .and_then(|h| h.get("dsh_web_bind"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "off".to_string());
        return daemon_run_argv(&LauncherFlags {
            web_bind: web_bind.to_string(),
            dsh_web_bind: Some(dsh_web_bind),
            ..LauncherFlags::default()
        })
        .map_err(|err| format!("{err:#}"));
    }
    Err(
        "the running daemon has no launcher record and does not report its bind on \
         `/health` (a build older than the identity surface, or one started with \
         `--no-web`), so a restart could only guess where to put it back. Restart it \
         yourself with the flags it is running: `ccteam stop && ccteam start --web-bind \
         <addr> [...]`, or name them on `ccteam daemon restart --web-bind <addr>`"
            .to_string(),
    )
}

/// Reusable restart core: acquire the operation lock, then stop (if
/// managed) + start under that ONE lock so no concurrent lifecycle op can
/// interleave. The CALLER runs any takeover pre-step first
/// (`daemon start/restart` and `ccteam update` all do). Shared by
/// [`run_daemon_restart`] and the `ccteam update` upgrade-restart contract
/// so the lock/stop/start logic lives in exactly one place.
///
/// `flags = None` means **replay the running invocation**: `ccteam update`
/// restarts a daemon it did not launch and must not move it anywhere. See
/// [`plan_replay`] for the rungs — and for why the missing rung is a
/// refusal rather than the compiled-in defaults.
pub(crate) fn restart_managed(
    paths: &CcteamPaths,
    flags: Option<&LauncherFlags>,
) -> Result<RestartOutcome> {
    let replay = match flags {
        Some(f) => daemon_run_argv(f)?,
        None => {
            // Decided while the daemon is still UP (it has to answer
            // `/health`) and before the lock, so a refusal stops nothing.
            let recorded = dcore::read_pid_record(&dcore::pidfile_path(paths)).and_then(|r| r.args);
            match plan_replay(recorded, live_health(paths).as_ref()) {
                Ok(args) => args,
                Err(hint) => return Ok(RestartOutcome::ReplayUnavailable { hint }),
            }
        }
    };
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
    let spec = start_spec(paths, replay)?;
    match dcore::start_managed(paths, &spec)? {
        dcore::StartVerdict::Started { pid, version } => Ok(if was_running {
            RestartOutcome::Restarted { pid, version }
        } else {
            RestartOutcome::Started { pid, version }
        }),
        dcore::StartVerdict::AlreadyRunning { version, .. } => {
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
        RestartOutcome::ReplayUnavailable { hint } => RestartCommandAction::Fail {
            code: "replayUnavailable",
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

pub fn run_daemon_restart(flags: &LauncherFlags, json: bool, if_managed: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // Restart is the verb `make install` runs on upgraded dev boxes, so
    // it carries the same takeover pre-step as start.
    takeover_pre_step();
    let outcome = match restart_managed(&paths, Some(flags)) {
        Ok(outcome) => outcome,
        Err(err) => fail(json, error_code(&err), &format!("{err:#}")),
    };
    match restart_command_action(outcome, if_managed, &flags.web_bind) {
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
    // v0.10.5 — the `GET /health` field set, sourced from the RUNNING
    // daemon. The launcher's recorded argv is NOT an identity: it records
    // what was requested, and `--web-bind 127.0.0.1:0` requests "any free
    // port" while the daemon serves a concrete one. The record is used for
    // exactly two things — `managed`, and the replay args a restart needs.
    //
    // What survives when `/health` is unreachable (`--no-web`, an old
    // build, a bind this host cannot dial) is only what is true BY
    // CONSTRUCTION: the home whose socket we just probed, the version that
    // socket answered with, and the record's pid when it matches a live
    // process. The bind/uptime/build fields go `null` — "unknown" is a
    // usable answer, a requested value dressed up as a served one is not.
    let health = live_health(&paths);
    let field = |name: &str| health.as_ref().and_then(|h| h.get(name)).cloned();
    let machine = serde_json::json!({
        "status": if report.ready { "ok" } else { "down" },
        "version": field("version").unwrap_or(serde_json::json!(report.running_version)),
        "build": field("build").unwrap_or(serde_json::Value::Null),
        "home": field("home").unwrap_or(serde_json::json!(canonical_home(&paths))),
        "pid": field("pid").unwrap_or(serde_json::json!(report
            .managed
            .then_some(pid)
            .flatten())),
        "web_bind": field("web_bind").unwrap_or(serde_json::Value::Null),
        "dsh_web_bind": field("dsh_web_bind").unwrap_or(serde_json::Value::Null),
        "uptime_secs": field("uptime_secs").unwrap_or(serde_json::Value::Null),
        "binary": ccteam_core::current_ccteam_bin()
            .ok()
            .map(|p| p.display().to_string()),
        "ready": report.ready,
        "managed": report.managed,
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
            "  managed: no   (a hand-run `internal daemon-run` or a self-supervised \
             instance; `ccteam start` always produces a managed one)\n",
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
    fn daemon_run_argv_targets_the_internal_entry_and_forwards_every_flag() {
        // The exec target must NOT be `start`: since v0.10.5 D7 `start` IS
        // the launcher, so pointing the launcher at it would fork bomb.
        let flags = LauncherFlags {
            no_web: true,
            no_imd: true,
            web_bind: "127.0.0.1:9000".to_string(),
            dsh_web_bind: Some("off".to_string()),
            web_no_auth: true,
            web_token_file: Some(PathBuf::from("/tmp/tok")),
            no_clipboard: true,
        };
        let argv = daemon_run_argv(&flags).unwrap();
        assert_eq!(
            &argv[..2],
            &["internal".to_string(), "daemon-run".to_string()]
        );
        for expected in [
            "--web-bind",
            "127.0.0.1:9000",
            "--dsh-web-bind",
            "off",
            "--no-web",
            "--no-imd",
            "--web-no-auth",
            "--web-token-file",
            "/tmp/tok",
            "--no-clipboard",
        ] {
            assert!(
                argv.iter().any(|a| a == expected),
                "missing {expected} in {argv:?}"
            );
        }
        assert!(!argv.contains(&"start".to_string()), "{argv:?}");
    }

    #[test]
    fn daemon_run_argv_omits_flags_that_are_off_and_derives_the_companion_bind() {
        let argv = daemon_run_argv(&LauncherFlags {
            web_bind: "127.0.0.1:7331".to_string(),
            ..LauncherFlags::default()
        })
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "internal",
                "daemon-run",
                "--web-bind",
                "127.0.0.1:7331",
                "--dsh-web-bind",
                "127.0.0.1:7332",
            ]
        );
        // The recorded argv is what a restart replays, so it must round
        // trip through the accessor `daemon status --json` reads.
        assert_eq!(
            ccteam_core::daemon::arg_value(&argv, "--web-bind"),
            Some("127.0.0.1:7331")
        );
    }

    #[test]
    fn plan_replay_prefers_the_record_then_the_served_binds_then_refuses() {
        let recorded: Vec<String> = [
            "internal",
            "daemon-run",
            "--web-bind",
            "127.0.0.1:9",
            "--no-imd",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let health = serde_json::json!({
            "web_bind": "127.0.0.1:46303",
            "dsh_web_bind": "127.0.0.1:46304",
        });

        // Rung 1 — the record carries every flag, so it wins even when
        // `/health` is also available.
        assert_eq!(
            plan_replay(Some(recorded.clone()), Some(&health)).unwrap(),
            recorded
        );

        // Rung 2 — no record: put the daemon back on the binds it is
        // ACTUALLY serving, not on the compiled-in defaults.
        let args = plan_replay(None, Some(&health)).unwrap();
        assert_eq!(
            ccteam_core::daemon::arg_value(&args, "--web-bind"),
            Some("127.0.0.1:46303")
        );
        assert_eq!(
            ccteam_core::daemon::arg_value(&args, "--dsh-web-bind"),
            Some("127.0.0.1:46304")
        );
        assert!(!args.iter().any(|a| a == DEFAULT_WEB_BIND), "{args:?}");

        // A daemon with the companion disabled reports null; `off` is how
        // that is spelled back on the command line.
        let no_companion = serde_json::json!({
            "web_bind": "127.0.0.1:46303",
            "dsh_web_bind": serde_json::Value::Null,
        });
        let args = plan_replay(None, Some(&no_companion)).unwrap();
        assert_eq!(
            ccteam_core::daemon::arg_value(&args, "--dsh-web-bind"),
            Some("off")
        );

        // Rung 3 — neither source knows where it is running. Restarting
        // onto the defaults would move a live daemon to another port and
        // kill /health on the original one, so REFUSE and say how to fix
        // it by hand.
        for health in [None, Some(&serde_json::json!({ "status": "ok" }))] {
            let err = plan_replay(None, health).expect_err("must refuse, not guess");
            assert!(err.contains("ccteam start --web-bind"), "{err}");
            assert!(err.contains("daemon restart --web-bind"), "{err}");
            assert!(
                !err.contains(DEFAULT_WEB_BIND),
                "must not suggest a guess: {err}"
            );
        }
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
