//! v0.9.7 — `ccteam update` (PRD F3.2/F3.3): channel-aware self-update.
//!
//! Dispatch by [`ccteam_core::install_channel::detect`]:
//! - **Standalone** → replay `install.sh` non-interactively (it owns the
//!   download + sha256 + atomic swap; ccteam embeds NO second downloader),
//!   then the **upgrade-restart contract**: if the daemon is ready, wait
//!   for in-flight turns to go idle (≤5 min cap, `--now` skips) and
//!   gracefully restart it onto the new binary, verifying the running
//!   version afterwards.
//! - **Source** → print `git pull && make install` (never compiles).
//! - **Npm/Bun/Pnpm** → print "not published yet" (V094).
//! - **Other** → error + `current_exe()` + repo URL.
//!
//! Adapted from openai/codex `tui/update_action.rs` (`UpdateAction` enum +
//! channel→command mapping), Apache-2.0 — see `LICENSES.md`. The
//! upgrade-restart contract (probe → in-flight drain → graceful restart →
//! version verify) is ccteam-specific (codex only prints "please restart").
//!
//! The channel→action mapping ([`plan`]) and the restart contract
//! ([`run_restart_contract`]) are pure/injectable so they unit-test
//! without running curl or a real daemon.

use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use ccteam_core::daemon as dcore;
use ccteam_core::install_channel::{self, InstallChannel};
use ccteam_core::version_check::{update_available, VersionCache};
use ccteam_core::CcteamPaths;

use crate::daemon_cli;

/// Wait cap for the in-flight drain before a restart (PRD F3.3: default 5
/// min, `--now` skips).
const IN_FLIGHT_WAIT_CAP: Duration = Duration::from_secs(5 * 60);
/// Progress-line cadence while draining.
const IN_FLIGHT_POLL: Duration = Duration::from_secs(15);

/// What `ccteam update` will do, decided BEFORE any side effect so the
/// channel→action mapping is unit-testable without running curl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// Standalone: replay install.sh, then honor `restart`.
    RunInstaller { restart: RestartPlan },
    /// Source: print `git pull && make install`, exit 0.
    SourceGuidance,
    /// Npm/Bun/Pnpm: print "not published yet", exit 0.
    NpmNotPublished {
        channel: InstallChannel,
        suggested: Option<String>,
    },
    /// Other: error + current_exe + repo URL, exit 1.
    UnknownChannel,
}

/// Post-swap restart posture for a standalone update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartPlan {
    /// `--no-restart`: swap the binary only.
    None,
    /// Probe the daemon and, if ready, drain + gracefully restart.
    Managed { now: bool },
}

/// Pure channel→action mapping (unit-tested; no side effects).
pub(crate) fn plan(channel: &InstallChannel, no_restart: bool, now: bool) -> UpdateAction {
    match channel {
        InstallChannel::Standalone => UpdateAction::RunInstaller {
            restart: if no_restart {
                RestartPlan::None
            } else {
                RestartPlan::Managed { now }
            },
        },
        InstallChannel::Source => UpdateAction::SourceGuidance,
        InstallChannel::Npm | InstallChannel::Bun | InstallChannel::Pnpm => {
            UpdateAction::NpmNotPublished {
                channel: channel.clone(),
                suggested: install_channel::suggested_update_command(channel),
            }
        }
        InstallChannel::Other => UpdateAction::UnknownChannel,
    }
}

/// Whether there is actually something newer to download, decided BEFORE any
/// network write / binary swap (pure → unit-testable without touching curl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateGate {
    /// `latest` is strictly newer than the running version → install it.
    Proceed { latest: String },
    /// Already on the newest published release → skip the download entirely.
    UpToDate { latest: String },
    /// The latest release couldn't be determined (offline / GitHub hiccup) →
    /// fail OPEN and install anyway, rather than stranding the user behind a
    /// flaky network.
    Unknown,
}

/// Pure version gate: compare the fetched `latest` release tag against the
/// running `current` version.
///
/// `ccteam update` is EXPLICIT user intent, so two deliberate differences from
/// the passive `ccteam status` check: the caller fetches fresh (never the 20h
/// [`ccteam_core::version_check::REFRESH_INTERVAL_HOURS`] cache), and the probe
/// cache carries no `dismissed_version` — dismissal only silences the passive
/// nag and must never block an explicit update. Version comparison itself is
/// delegated to the shared normalized comparator so a `v`-prefixed tag
/// and a bare version resolve identically everywhere.
pub(crate) fn gate(latest: Option<&str>, current: &str) -> UpdateGate {
    let Some(latest) = latest else {
        return UpdateGate::Unknown;
    };
    let probe = VersionCache {
        latest_version: Some(latest.to_string()),
        last_checked_at: None,
        dismissed_version: None,
    };
    if update_available(&probe, current).is_some() {
        UpdateGate::Proceed {
            latest: latest.to_string(),
        }
    } else {
        UpdateGate::UpToDate {
            latest: latest.to_string(),
        }
    }
}

/// Terminal state of the upgrade-restart contract (PRD F3.3), so the
/// state machine is unit-testable with injected probe/restart fns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestartContractStatus {
    /// The daemon wasn't running — nothing to restart (binary swapped only).
    DaemonDown,
    /// Restarted and the running version matches the new binary's version.
    Restarted { version: Option<String> },
    /// Restarted but the running version != the new binary's version — the
    /// daemon may not have come up on the new binary (WARN, not fatal).
    RestartedVersionSkew {
        running: Option<String>,
        expected: String,
    },
    /// A ready but not-managed instance holds the socket — restart refused.
    RestartRefused { hint: String },
    /// The restart itself failed (stop timeout / lock / spawn error).
    RestartError { message: String },
}

/// The upgrade-restart contract as a pure state machine over injected
/// daemon operations (production wires the real probe / drain / restart;
/// tests inject fakes). Steps: probe → (drain unless `skip_wait`) →
/// restart → re-probe + version verify.
pub(crate) fn run_restart_contract(
    expected_version: &str,
    skip_wait: bool,
    probe: impl Fn() -> dcore::DaemonProbe,
    wait_for_idle: impl FnOnce(),
    restart: impl FnOnce() -> Result<daemon_cli::RestartOutcome>,
) -> RestartContractStatus {
    if !probe().ready {
        return RestartContractStatus::DaemonDown;
    }
    if !skip_wait {
        wait_for_idle();
    }
    let outcome = match restart() {
        Ok(o) => o,
        Err(err) => {
            return RestartContractStatus::RestartError {
                message: format!("{err:#}"),
            }
        }
    };
    match outcome {
        daemon_cli::RestartOutcome::NotManaged { hint } => {
            RestartContractStatus::RestartRefused { hint }
        }
        daemon_cli::RestartOutcome::StopTimedOut { pid } => RestartContractStatus::RestartError {
            message: format!("daemon pid {pid} did not exit within the stop wait"),
        },
        daemon_cli::RestartOutcome::Restarted { .. }
        | daemon_cli::RestartOutcome::Started { .. }
        | daemon_cli::RestartOutcome::AlreadyServing { .. } => {
            // Authoritative running version comes from a fresh probe.
            let after = probe();
            if after.version.as_deref() == Some(expected_version) {
                RestartContractStatus::Restarted {
                    version: after.version,
                }
            } else {
                RestartContractStatus::RestartedVersionSkew {
                    running: after.version,
                    expected: expected_version.to_string(),
                }
            }
        }
    }
}

/// `ccteam update` entry point.
pub fn run_update(now: bool, no_restart: bool, json: bool, force: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let channel = install_channel::detect(&paths);
    match plan(&channel, no_restart, now) {
        UpdateAction::SourceGuidance => {
            emit(
                json,
                serde_json::json!({
                    "status": "guidance",
                    "channel": channel.as_str(),
                    "suggested": "git pull && make install",
                }),
                "ccteam was built from source; `ccteam update` will not compile it. Update with:\n  \
                 git pull && make install",
            );
            Ok(())
        }
        UpdateAction::NpmNotPublished { channel, suggested } => {
            let cmd = suggested.unwrap_or_default();
            emit(
                json,
                serde_json::json!({
                    "status": "guidance",
                    "channel": channel.as_str(),
                    "suggested": cmd,
                }),
                &format!(
                    "the {} channel is not published yet (tracked in V094). Once it ships, update \
                     with:\n  {}",
                    channel.as_str(),
                    cmd
                ),
            );
            Ok(())
        }
        UpdateAction::UnknownChannel => {
            let exe = std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            fail(
                json,
                "unknownChannel",
                &format!(
                    "cannot self-update: ccteam's install channel is unknown (running {exe}). \
                     Reinstall from {}",
                    install_channel::REPO_URL
                ),
            )
        }
        UpdateAction::RunInstaller { restart } => {
            // Only download when something newer actually exists. Explicit user
            // intent → fetch FRESH (not the 20h lazy cache `ccteam status`
            // uses); `--force` skips the check entirely so a corrupt install can
            // still be repaired in place. A failed fetch reads as `Unknown` and
            // falls through to the install (fail-open, today's behaviour).
            let current = env!("CARGO_PKG_VERSION");
            if !force {
                if let UpdateGate::UpToDate { latest } =
                    gate(fetch_latest_version().as_deref(), current)
                {
                    return emit_up_to_date(&paths, &channel, current, &latest, json);
                }
            }
            run_standalone_update(&paths, restart, json)
        }
    }
}

/// Report "nothing to download". If a daemon is still running a DIFFERENT
/// version, say so and point at the restart: the install path we just skipped
/// is what would otherwise have restarted it, so staying silent here would
/// quietly leave the daemon on the old binary.
fn emit_up_to_date(
    paths: &CcteamPaths,
    channel: &InstallChannel,
    current: &str,
    latest: &str,
    json: bool,
) -> Result<()> {
    let probe = dcore::probe_daemon(paths);
    let daemon_version = if probe.ready {
        probe.version.clone()
    } else {
        None
    };
    let stale_daemon = daemon_version.as_deref().is_some_and(|v| v != current);
    let mut human = format!(
        "already on the latest release ({current}) — nothing to download.\n  \
         reinstall anyway with `ccteam update --force`"
    );
    if let Some(running) = daemon_version.as_deref().filter(|_| stale_daemon) {
        human.push_str(&format!(
            "\n  note: the running daemon reports {running} — restart it to pick up \
             {current}: `ccteam daemon restart`"
        ));
    }
    emit(
        json,
        serde_json::json!({
            "status": "upToDate",
            "channel": channel.as_str(),
            "version": current,
            "latest": latest,
            "daemonVersion": daemon_version,
            "daemonRestartRequired": stale_daemon,
        }),
        &human,
    );
    Ok(())
}

/// Standalone path: replay install.sh, then the upgrade-restart contract.
fn run_standalone_update(paths: &CcteamPaths, restart: RestartPlan, json: bool) -> Result<()> {
    // install.sh chatter must NOT pollute stdout in `--json` mode (stdout
    // stays exactly one JSON line), so route the child's stdout to stderr
    // there; human mode inherits it.
    if json {
        eprintln!("ccteam update: replaying the installer...");
    } else {
        println!("ccteam update: replaying the installer (download + verify + atomic swap)...");
    }
    let status = run_installer(json).context("run the ccteam installer")?;
    if !status.success() {
        fail(
            json,
            "installerFailed",
            &format!(
                "the installer exited unsuccessfully ({status}); the existing binary is unchanged \
                 (install.sh swaps atomically) and the daemon was NOT restarted"
            ),
        );
    }

    // Binary swapped. The running `ccteam update` process is still the OLD
    // binary, so its compiled version is stale — ask the freshly-installed
    // binary on disk for the NEW version to verify the restart against.
    let expected =
        installed_binary_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    match restart {
        RestartPlan::None => {
            emit(
                json,
                serde_json::json!({
                    "status": "binarySwapped",
                    "channel": "standalone",
                    "reason": "noRestart",
                    "version": expected,
                }),
                &format!(
                    "binary updated to {expected}; run `ccteam daemon restart` to load it into the \
                     running daemon"
                ),
            );
            Ok(())
        }
        RestartPlan::Managed { now } => {
            // PRD F4 layer ③ — the restart carries the legacy-unit takeover
            // pre-step (a residual old unit also converges here).
            daemon_cli::takeover_pre_step();
            let status = run_restart_contract(
                &expected,
                now,
                || dcore::probe_daemon(paths),
                || wait_for_active_sessions_idle(paths),
                || {
                    daemon_cli::restart_managed(
                        paths,
                        daemon_cli::DEFAULT_WEB_BIND,
                        Some(daemon_cli::DEFAULT_DSH_WEB_BIND),
                    )
                },
            );
            emit_restart_contract(json, &expected, status)
        }
    }
}

/// Map the restart-contract state to the `--json` line / prose / exit code.
fn emit_restart_contract(json: bool, expected: &str, status: RestartContractStatus) -> Result<()> {
    match status {
        RestartContractStatus::DaemonDown => {
            emit(
                json,
                serde_json::json!({
                    "status": "binarySwapped",
                    "channel": "standalone",
                    "reason": "daemonDown",
                    "version": expected,
                }),
                &format!(
                    "binary updated to {expected}; the daemon is not running \
                     (`ccteam daemon start` to launch the new version)"
                ),
            );
            Ok(())
        }
        RestartContractStatus::Restarted { version } => {
            let v = version.clone().unwrap_or_else(|| "unknown".to_string());
            emit(
                json,
                serde_json::json!({
                    "status": "restarted",
                    "channel": "standalone",
                    "version": version,
                }),
                &format!(
                    "updated to {expected} and gracefully restarted the daemon (running version \
                     {v}). Agent sessions are not killed; a session still mid-turn finishes it \
                     and the new daemon picks it up by sid (never a second process)."
                ),
            );
            Ok(())
        }
        RestartContractStatus::RestartedVersionSkew {
            running,
            expected: exp,
        } => {
            let r = running.clone().unwrap_or_else(|| "unknown".to_string());
            // The binary IS swapped and the daemon DID restart — the version
            // mismatch is a warning, not a failure.
            emit(
                json,
                serde_json::json!({
                    "status": "restarted",
                    "channel": "standalone",
                    "version": running,
                    "versionSkew": true,
                    "expectedVersion": exp,
                }),
                &format!(
                    "WARNING: updated the binary to {exp} and restarted, but the running daemon \
                     reports version {r} — it may not have come up on the new binary. Check \
                     `ccteam daemon status` / `ccteam daemon logs`."
                ),
            );
            Ok(())
        }
        RestartContractStatus::RestartRefused { hint } => fail(
            json,
            "notManaged",
            &format!("binary updated, but the restart was refused: {hint}"),
        ),
        RestartContractStatus::RestartError { message } => fail(
            json,
            "restartFailed",
            &format!(
                "binary updated, but the restart failed: {message}. Restart manually with \
                 `ccteam daemon restart`."
            ),
        ),
    }
}

/// Replay `install.sh` (it does the download + sha256 + atomic mv). In
/// `--json` mode the child's stdout is redirected to OUR stderr so stdout
/// stays a single machine line; human mode inherits it.
fn run_installer(json: bool) -> Result<ExitStatus> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(install_channel::STANDALONE_INSTALL_PIPELINE)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    // Pin the installer to the binary we are REPLACING. install.sh's own ladder
    // would infer the destination from `command -v ccteam`, which answers a
    // different question — "what does a shell find first" — and would install
    // beside a shadowing copy instead of over this one, leaving two binaries and
    // an update that appears to do nothing. The running process knows where it
    // lives; that is the authoritative answer, so pass it explicitly.
    if let Some(dir) = ccteam_core::current_ccteam_bin()
        .ok()
        .as_deref()
        .and_then(std::path::Path::parent)
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        cmd.env("CCTEAM_INSTALL_DIR", dir);
    }
    if json {
        cmd.stdout(stderr_stdio());
    } else {
        cmd.stdout(Stdio::inherit());
    }
    cmd.status().context("spawn sh -c install.sh")
}

/// A [`Stdio`] that writes to the process's own stderr (used to keep
/// installer chatter off stdout in `--json` mode). Unix-only (the whole
/// daemon lifecycle is); falls back to `inherit` if the dup fails.
#[cfg(unix)]
fn stderr_stdio() -> Stdio {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    // SAFETY: dup(2) the stderr fd into a fresh owned fd; `Stdio::from(File)`
    // takes ownership and closes it after the spawn. A negative fd (dup
    // failure) falls back to inherit.
    let fd = unsafe { libc::dup(std::io::stderr().as_raw_fd()) };
    if fd < 0 {
        return Stdio::inherit();
    }
    unsafe { Stdio::from(std::fs::File::from_raw_fd(fd)) }
}

#[cfg(not(unix))]
fn stderr_stdio() -> Stdio {
    Stdio::inherit()
}

/// Ask the freshly-installed binary on disk for its version by running
/// `<current_exe> --version` (the running updater is the OLD binary, so
/// its compiled version is stale). `None` on any failure → caller falls
/// back to the updater's own compiled version.
fn installed_binary_version() -> Option<String> {
    // Route through `current_ccteam_bin()` (not raw `current_exe()`): the
    // atomic swap just replaced the running updater's inode, so
    // `current_exe()` reports `<path> (deleted)` and probing it would fail —
    // masking the real (new) version behind the updater's stale compiled one.
    let exe = ccteam_core::current_ccteam_bin().ok()?;
    let out = Command::new(&exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `ccteam --version` prints e.g. "ccteam 0.9.8 (<commit>)"; take the
    // first dotted-numeric token.
    text.lines().next().and_then(|line| {
        line.split_whitespace()
            .map(|t| t.trim_start_matches('v'))
            .find(|t| t.contains('.') && t.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(str::to_string)
    })
}

/// Block until no tracked session has an in-flight ("working") turn, up to
/// [`IN_FLIGHT_WAIT_CAP`], printing a progress line to stderr every
/// [`IN_FLIGHT_POLL`]. Reads the SAME file-backed progress truth
/// `ccteam status` uses (`tracked_chat_sessions` × `read_all_events` ×
/// `classify_progress_activity_for_sid`). At the cap it proceeds anyway
/// (a stream-json body finishes its turn unobserved and the new daemon picks
/// it up by its body record; an ACP/codex turn is interrupted and resumes by
/// sid).
fn wait_for_active_sessions_idle(paths: &CcteamPaths) {
    let deadline = Instant::now() + IN_FLIGHT_WAIT_CAP;
    loop {
        let active = active_session_count(paths);
        if active == 0 {
            return;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "ccteam update: proceeding after the {}-minute cap with {active} session(s) still \
                 active; a stream-json session keeps finishing its turn and the new daemon waits \
                 for it (one sid, one body); an ACP/codex turn is interrupted and resumes by sid",
                IN_FLIGHT_WAIT_CAP.as_secs() / 60
            );
            return;
        }
        eprintln!(
            "ccteam update: waiting for {active} active session(s) to go idle before restarting…"
        );
        std::thread::sleep(IN_FLIGHT_POLL);
    }
}

/// Count tracked sessions with an in-flight ("working") turn — a recent
/// non-idle progress event. Idle / stale / stuck do NOT count: only
/// "working" is a genuinely in-flight turn worth waiting on (stale/stuck
/// are wedged and would just burn the cap).
fn active_session_count(paths: &CcteamPaths) -> usize {
    use std::collections::HashMap;
    let tracked = ccteam_im::gateway::tracked_chat_sessions(&paths.root).unwrap_or_default();
    let now = chrono::Utc::now();
    let mut events_by_project: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut active = 0usize;
    for row in tracked {
        let events = events_by_project
            .entry(row.project.clone())
            .or_insert_with(|| {
                ccteam_core::progress::read_all_events(&paths.progress_jsonl(&row.project))
                    .unwrap_or_default()
            });
        let activity =
            ccteam_core::stall::classify_progress_activity_for_sid(events, &row.sid, 0, now);
        if activity.status.activity == "working" {
            active += 1;
        }
    }
    active
}

/// GitHub "latest release" fetcher for the lazy version check (PRD F3.4).
///
/// **STUBBED (returns `None`) in this wave.** This sandbox has no outbound
/// network, so the live path can't be tested here and must NOT be exercised
/// by ccteam's own tests. The lazy check is otherwise fully wired
/// (`ccteam status` calls it through `maybe_refresh_latest` on the ≥20h
/// gate); the orchestrator drops the real HTTP GET in here and its live
/// check exercises it. **Any failure MUST fold to `None`** (silent degrade
/// to the cached value) — never panic, never block a status/doctor render.
///
/// Reference implementation (mirrors `install.sh`'s redirect parse — no
/// GitHub API rate limit): a short-timeout HEAD of
/// `https://github.com/firstintent/ccteam/releases/latest`, then the
/// `/tag/<v>` segment of the `Location:` header (e.g. via
/// `curl -sI --max-time 3 <url>` or a short-timeout reqwest client).
pub(crate) fn fetch_latest_version() -> Option<String> {
    // Mirror install.sh's redirect parse (no GitHub API rate limit): a
    // short-timeout HEAD of releases/latest, then the `/tag/<v>` segment of
    // the `Location:` header. Shelling out to curl matches the installer's
    // own resolution exactly and sidesteps any blocking-in-async concern; it
    // is also already an update-path dependency. Any failure (curl absent, no
    // network, parse miss) folds to `None` — the caller keeps the cache.
    let out = std::process::Command::new("curl")
        .args([
            "-sI",
            "--max-time",
            "3",
            "https://github.com/firstintent/ccteam/releases/latest",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let headers = String::from_utf8_lossy(&out.stdout);
    for line in headers.lines() {
        // Header names are case-insensitive; the value keeps the tag's case.
        if line.len() >= 9 && line[..9].eq_ignore_ascii_case("location:") {
            let value = line[9..].trim();
            if let Some(idx) = value.rfind("/tag/") {
                let tag = value[idx + "/tag/".len()..].trim();
                if !tag.is_empty() {
                    return Some(tag.to_string());
                }
            }
        }
    }
    None
}

/// One line per registered satellite whose ccteam version differs from
/// `daemon_version`. Empty when the registry is empty/absent or every host
/// is aligned, so the common no-satellite case stays silent (no noise).
/// Shared by `ccteam status` and the `ccteam doctor` updates section so
/// the two never drift.
pub(crate) fn fleet_version_skew(paths: &CcteamPaths, daemon_version: &str) -> Vec<String> {
    let registry = ccteam_core::HostRegistry::load(&paths.host_registry_path()).unwrap_or_default();
    registry
        .list()
        .filter(|h| h.ccteam_version != daemon_version)
        .map(|h| {
            format!(
                "host {} runs {}, daemon runs {} — run `ccteam update` on that host",
                h.id, h.ccteam_version, daemon_version
            )
        })
        .collect()
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
    eprintln!("ccteam update: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The version gate is what stops `ccteam update` from re-downloading the
    /// installer when nothing newer has been published — the whole point of the
    /// pre-check. Pure, so it runs without touching the network.
    #[test]
    fn gate_skips_the_download_when_already_on_the_latest() {
        // Newer upstream → install it.
        assert_eq!(
            gate(Some("v0.9.8"), "0.9.7"),
            UpdateGate::Proceed {
                latest: "v0.9.8".into()
            }
        );
        // Same version, with and without the `v` prefix → nothing to download.
        assert_eq!(
            gate(Some("v0.9.7"), "0.9.7"),
            UpdateGate::UpToDate {
                latest: "v0.9.7".into()
            }
        );
        assert_eq!(
            gate(Some("0.9.7"), "0.9.7"),
            UpdateGate::UpToDate {
                latest: "0.9.7".into()
            }
        );
        // Running a build NEWER than the published release (local / rc build)
        // must never "update" backwards.
        assert_eq!(
            gate(Some("v0.9.7"), "0.10.0"),
            UpdateGate::UpToDate {
                latest: "v0.9.7".into()
            }
        );
        // Release info unreachable → fail OPEN, so a flaky network cannot
        // strand the user (preserves the pre-check-era behaviour).
        assert_eq!(gate(None, "0.9.7"), UpdateGate::Unknown);
    }

    #[test]
    fn plan_maps_channel_to_action() {
        assert_eq!(
            plan(&InstallChannel::Standalone, false, false),
            UpdateAction::RunInstaller {
                restart: RestartPlan::Managed { now: false }
            }
        );
        assert_eq!(
            plan(&InstallChannel::Standalone, false, true),
            UpdateAction::RunInstaller {
                restart: RestartPlan::Managed { now: true }
            }
        );
        // --no-restart wins over --now.
        assert_eq!(
            plan(&InstallChannel::Standalone, true, true),
            UpdateAction::RunInstaller {
                restart: RestartPlan::None
            }
        );
        assert_eq!(
            plan(&InstallChannel::Source, false, false),
            UpdateAction::SourceGuidance
        );
        assert!(matches!(
            plan(&InstallChannel::Npm, false, false),
            UpdateAction::NpmNotPublished {
                channel: InstallChannel::Npm,
                ..
            }
        ));
        assert!(matches!(
            plan(&InstallChannel::Bun, false, false),
            UpdateAction::NpmNotPublished { .. }
        ));
        assert_eq!(
            plan(&InstallChannel::Other, false, false),
            UpdateAction::UnknownChannel
        );
    }

    /// A probe fn that returns each supplied value in sequence (then
    /// repeats the last) so the contract's before/after probes differ.
    fn probe_seq(seq: Vec<dcore::DaemonProbe>) -> impl Fn() -> dcore::DaemonProbe {
        let calls = Cell::new(0usize);
        move || {
            let i = calls.get();
            calls.set(i + 1);
            seq.get(i)
                .or_else(|| seq.last())
                .cloned()
                .unwrap_or(dcore::DaemonProbe {
                    ready: false,
                    version: None,
                })
        }
    }

    fn ready(v: &str) -> dcore::DaemonProbe {
        dcore::DaemonProbe {
            ready: true,
            version: Some(v.to_string()),
        }
    }

    #[test]
    fn restart_contract_daemon_down_skips_wait_and_restart() {
        let restart_called = Cell::new(false);
        let wait_called = Cell::new(false);
        let status = run_restart_contract(
            "0.9.8",
            false,
            || dcore::DaemonProbe {
                ready: false,
                version: None,
            },
            || wait_called.set(true),
            || {
                restart_called.set(true);
                Ok(daemon_cli::RestartOutcome::Restarted {
                    pid: 1,
                    version: Some("0.9.8".into()),
                })
            },
        );
        assert_eq!(status, RestartContractStatus::DaemonDown);
        assert!(!restart_called.get(), "must not restart a down daemon");
        assert!(!wait_called.get(), "must not wait when daemon down");
    }

    #[test]
    fn restart_contract_verifies_matching_version() {
        let status = run_restart_contract(
            "0.9.8",
            true, // skip wait
            probe_seq(vec![ready("0.9.7"), ready("0.9.8")]),
            || {},
            || {
                Ok(daemon_cli::RestartOutcome::Restarted {
                    pid: 42,
                    version: Some("0.9.8".into()),
                })
            },
        );
        assert_eq!(
            status,
            RestartContractStatus::Restarted {
                version: Some("0.9.8".into())
            }
        );
    }

    #[test]
    fn restart_contract_warns_on_version_skew() {
        // Daemon came back up still reporting the OLD version.
        let status = run_restart_contract(
            "0.9.8",
            true,
            probe_seq(vec![ready("0.9.7"), ready("0.9.7")]),
            || {},
            || {
                Ok(daemon_cli::RestartOutcome::Restarted {
                    pid: 42,
                    version: Some("0.9.7".into()),
                })
            },
        );
        assert_eq!(
            status,
            RestartContractStatus::RestartedVersionSkew {
                running: Some("0.9.7".into()),
                expected: "0.9.8".into(),
            }
        );
    }

    #[test]
    fn restart_contract_maps_refusal_and_timeout() {
        let refused = run_restart_contract(
            "0.9.8",
            true,
            || ready("0.9.7"),
            || {},
            || {
                Ok(daemon_cli::RestartOutcome::NotManaged {
                    hint: "foreground instance".into(),
                })
            },
        );
        assert!(matches!(
            refused,
            RestartContractStatus::RestartRefused { .. }
        ));

        let timeout = run_restart_contract(
            "0.9.8",
            true,
            || ready("0.9.7"),
            || {},
            || Ok(daemon_cli::RestartOutcome::StopTimedOut { pid: 9 }),
        );
        assert!(matches!(
            timeout,
            RestartContractStatus::RestartError { .. }
        ));

        let errored = run_restart_contract(
            "0.9.8",
            true,
            || ready("0.9.7"),
            || {},
            || anyhow::bail!("lock busy"),
        );
        assert!(matches!(
            errored,
            RestartContractStatus::RestartError { .. }
        ));
    }

    #[test]
    fn fleet_version_skew_flags_only_differing_hosts() {
        use ccteam_core::host_registry::{HostRecord, HostRegistry};
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // Empty / absent registry → silent (no noise).
        assert!(fleet_version_skew(&paths, "0.9.7").is_empty());

        let rec = |id: &str, ver: &str| HostRecord {
            id: id.into(),
            hostname: id.into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: ver.into(),
            agent_token: "t".into(),
            last_heartbeat_unix: 0,
            agents: vec![],
            projects: vec![],
            joined_at: "2026-07-22T00:00:00Z".into(),
        };
        let mut reg = HostRegistry::default();
        reg.upsert(rec("old-sat", "0.9.6"));
        reg.upsert(rec("aligned", "0.9.7"));
        reg.save(&paths.host_registry_path()).unwrap();

        let lines = fleet_version_skew(&paths, "0.9.7");
        assert_eq!(
            lines.len(),
            1,
            "only the skewed host should show: {lines:?}"
        );
        assert!(lines[0].contains("old-sat"));
        assert!(lines[0].contains("0.9.6"));
        assert!(lines[0].contains("ccteam update"));
    }

    #[test]
    fn restart_contract_runs_wait_when_not_skipped() {
        let wait_called = Cell::new(false);
        let status = run_restart_contract(
            "0.9.8",
            false, // do NOT skip the wait
            probe_seq(vec![ready("0.9.8"), ready("0.9.8")]),
            || wait_called.set(true),
            || {
                Ok(daemon_cli::RestartOutcome::Restarted {
                    pid: 1,
                    version: Some("0.9.8".into()),
                })
            },
        );
        assert!(wait_called.get(), "wait must run when not skipped");
        assert!(matches!(status, RestartContractStatus::Restarted { .. }));
    }
}
