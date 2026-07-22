//! v0.9.7 — one-time takeover of legacy installer-written systemd /
//! launchd units (PRD F4).
//!
//! systemd / launchd management is retired: the pid-detach launcher
//! (`ccteam daemon start`) is the only daemon supervisor. Machines that
//! installed ccteam before v0.9.7 may still carry a service unit written
//! by the historical Makefile / install.sh templates; `daemon start`
//! (and `daemon restart`) run this takeover as an idempotent pre-step:
//!
//! - **installer fingerprint match** → `systemctl --user disable --now
//!   ccteam` (a `systemctl stop` does NOT trigger `Restart=always`, so
//!   no bounce) → remove the unit file → `systemctl --user
//!   daemon-reload` (macOS: `launchctl bootout` + rm plist) → print the
//!   action list.
//! - **hand-written unit** (fingerprint mismatch) → NEVER deleted;
//!   guidance only. Its foreground `ExecStart=… ccteam start` keeps
//!   working; ccteam's lifecycle commands treat that instance as "not
//!   managed".
//!
//! The migration logic lives ONLY here (Rust); install.sh / doctor only
//! detect and point at `ccteam daemon start` — no shell reimplementation
//! to drift.
//!
//! Everything is injectable (base paths + command runner) so tests never
//! touch the real `~/.config` or run real `systemctl`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// launchd label the historical Makefile / install.sh templates used.
pub const LAUNCHD_LABEL: &str = "com.firstintent.ccteam";

/// Where the historical installers put their units.
#[derive(Debug, Clone)]
pub struct LegacyServicePaths {
    /// `${XDG_CONFIG_HOME:-~/.config}/systemd/user/ccteam.service`
    pub systemd_unit: PathBuf,
    /// `~/Library/LaunchAgents/com.firstintent.ccteam.plist`
    pub launchd_plist: PathBuf,
}

impl LegacyServicePaths {
    /// Resolve from the live environment (`XDG_CONFIG_HOME` / `$HOME`).
    /// Reads env at call time so tests can pin both.
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| home.join(".config"));
        Self {
            systemd_unit: config.join("systemd").join("user").join("ccteam.service"),
            launchd_plist: home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
        }
    }
}

/// Minimal injectable process runner (so tests record instead of exec).
pub trait CommandRunner {
    /// Run `program args…`; `Ok(true)` = exit success, `Ok(false)` =
    /// non-zero exit, `Err` = could not spawn.
    fn run(&mut self, program: &str, args: &[&str]) -> Result<bool>;
}

/// Production runner: spawn the real process, swallow its output.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<bool> {
        let status = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .with_context(|| format!("spawn {program}"))?;
        Ok(status.success())
    }
}

/// What the takeover pre-step found / did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverOutcome {
    /// No legacy unit anywhere — the common (already-migrated) case.
    NothingToDo,
    /// Installer-written unit was disabled + removed; `actions` is the
    /// human-readable audit list to print.
    Migrated { unit: PathBuf, actions: Vec<String> },
    /// A unit exists but does not match the installer fingerprint —
    /// left strictly alone.
    ForeignUnitPresent { unit: PathBuf },
}

/// Detection result for doctor: `(unit path, installer_written)`.
pub fn detect_legacy_unit(paths: &LegacyServicePaths) -> Option<(PathBuf, bool)> {
    if let Ok(content) = std::fs::read_to_string(&paths.systemd_unit) {
        return Some((
            paths.systemd_unit.clone(),
            systemd_unit_matches_installer(&content),
        ));
    }
    if let Ok(content) = std::fs::read_to_string(&paths.launchd_plist) {
        return Some((
            paths.launchd_plist.clone(),
            launchd_plist_matches_installer(&content),
        ));
    }
    None
}

/// Fingerprint whitelist for the two historical systemd templates
/// (Makefile `CCTEAM_UNIT` and install.sh `start_systemd`): a
/// `Description=` containing "ccteam daemon" AND an `ExecStart=` whose
/// program basename is `ccteam` with first argument `start`. Anything
/// else is treated as hand-written.
pub fn systemd_unit_matches_installer(content: &str) -> bool {
    let mut description_ok = false;
    let mut exec_ok = false;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Description=") {
            if rest.contains("ccteam daemon") {
                description_ok = true;
            }
        }
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            let mut words = rest.split_whitespace();
            let program_is_ccteam = words
                .next()
                .map(|p| Path::new(p).file_name().and_then(|n| n.to_str()) == Some("ccteam"))
                .unwrap_or(false);
            let first_arg_is_start = words.next() == Some("start");
            if program_is_ccteam && first_arg_is_start {
                exec_ok = true;
            }
        }
    }
    description_ok && exec_ok
}

/// Fingerprint for the historical launchd templates (Makefile
/// `CCTEAM_PLIST` and install.sh `start_launchd`): the exact label plus
/// `ProgramArguments` = [`…/ccteam`, `start`, …]. plists have no
/// Description; the label + program shape is the whitelist.
pub fn launchd_plist_matches_installer(content: &str) -> bool {
    if !content.contains(&format!("<string>{LAUNCHD_LABEL}</string>")) {
        return false;
    }
    // Extract the <string> entries after ProgramArguments and check the
    // first two: program basename `ccteam`, then literal `start`.
    let Some(after) = content.split("ProgramArguments").nth(1) else {
        return false;
    };
    let mut strings = after.split("<string>").skip(1).map(|chunk| {
        chunk
            .split("</string>")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let program_is_ccteam = strings
        .next()
        .map(|p| Path::new(&p).file_name().and_then(|n| n.to_str()) == Some("ccteam"))
        .unwrap_or(false);
    let first_arg_is_start = strings.next().as_deref() == Some("start");
    program_is_ccteam && first_arg_is_start
}

/// Run the takeover with the live environment (real dirs + real
/// systemctl / launchctl).
pub fn run_takeover_from_env() -> Result<TakeoverOutcome> {
    run_takeover(&LegacyServicePaths::from_env(), &mut SystemRunner)
}

/// Idempotent takeover core. Order matters: **stop first** (`disable
/// --now` — never triggers `Restart=`), then remove the unit, then
/// reload, so the pid-detach start that follows finds a clean slate.
pub fn run_takeover(
    paths: &LegacyServicePaths,
    runner: &mut dyn CommandRunner,
) -> Result<TakeoverOutcome> {
    // systemd (Linux path). Checked first; existence of the file is the
    // trigger, so a macOS box simply never has one.
    if let Ok(content) = std::fs::read_to_string(&paths.systemd_unit) {
        if !systemd_unit_matches_installer(&content) {
            return Ok(TakeoverOutcome::ForeignUnitPresent {
                unit: paths.systemd_unit.clone(),
            });
        }
        let mut actions = Vec::new();
        match runner.run("systemctl", &["--user", "disable", "--now", "ccteam"]) {
            Ok(true) => actions.push("systemctl --user disable --now ccteam".to_string()),
            Ok(false) => actions.push(
                "systemctl --user disable --now ccteam (non-zero exit — unit may not have been \
                 active)"
                    .to_string(),
            ),
            Err(err) => actions.push(format!(
                "systemctl not runnable ({err}); unit file removed anyway"
            )),
        }
        std::fs::remove_file(&paths.systemd_unit)
            .with_context(|| format!("remove {}", paths.systemd_unit.display()))?;
        actions.push(format!("removed {}", paths.systemd_unit.display()));
        if matches!(
            runner.run("systemctl", &["--user", "daemon-reload"]),
            Ok(true)
        ) {
            actions.push("systemctl --user daemon-reload".to_string());
        }
        return Ok(TakeoverOutcome::Migrated {
            unit: paths.systemd_unit.clone(),
            actions,
        });
    }

    // launchd (macOS path).
    if let Ok(content) = std::fs::read_to_string(&paths.launchd_plist) {
        if !launchd_plist_matches_installer(&content) {
            return Ok(TakeoverOutcome::ForeignUnitPresent {
                unit: paths.launchd_plist.clone(),
            });
        }
        let mut actions = Vec::new();
        let service = format!("gui/{}/{LAUNCHD_LABEL}", uid());
        match runner.run("launchctl", &["bootout", &service]) {
            Ok(true) => actions.push(format!("launchctl bootout {service}")),
            Ok(false) => actions.push(format!(
                "launchctl bootout {service} (non-zero exit — agent may not have been loaded)"
            )),
            Err(err) => actions.push(format!(
                "launchctl not runnable ({err}); plist removed anyway"
            )),
        }
        std::fs::remove_file(&paths.launchd_plist)
            .with_context(|| format!("remove {}", paths.launchd_plist.display()))?;
        actions.push(format!("removed {}", paths.launchd_plist.display()));
        return Ok(TakeoverOutcome::Migrated {
            unit: paths.launchd_plist.clone(),
            actions,
        });
    }

    Ok(TakeoverOutcome::NothingToDo)
}

#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: getuid is always safe.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shapes the two historical installers wrote (Makefile
    /// `CCTEAM_UNIT` with expanded vars, and install.sh `start_systemd`).
    const MAKEFILE_UNIT: &str = "[Unit]\n\
        Description=ccteam daemon (IM gateway + web UI + MCP)\n\
        Documentation=https://github.com/firstintent/ccteam\n\
        StartLimitIntervalSec=300\n\
        StartLimitBurst=5\n\
        \n[Service]\n\
        Type=exec\n\
        ExecStart=/home/u/.local/bin/ccteam start --web-bind 0.0.0.0:7331\n\
        WorkingDirectory=/home/u\n\
        Environment=PATH=/home/u/.local/bin:/usr/bin:/bin\n\
        KillSignal=SIGTERM\n\
        TimeoutStopSec=40\n\
        Restart=always\n\
        RestartSec=2\n\
        \n[Install]\n\
        WantedBy=default.target\n";

    const INSTALL_SH_UNIT: &str = "[Unit]\n\
        Description=ccteam daemon (IM gateway + web console + MCP)\n\
        After=network-online.target\n\
        Wants=network-online.target\n\
        \n[Service]\n\
        Type=simple\n\
        ExecStart=/home/u/.local/bin/ccteam start\n\
        Environment=PATH=/usr/bin:/bin\n\
        Restart=on-failure\n\
        RestartSec=5\n\
        \n[Install]\n\
        WantedBy=default.target\n";

    const HAND_WRITTEN_UNIT: &str = "[Unit]\n\
        Description=my own ccteam wrapper\n\
        \n[Service]\n\
        ExecStart=/usr/local/bin/run-ccteam.sh\n\
        Restart=always\n\
        \n[Install]\n\
        WantedBy=default.target\n";

    fn installer_plist(bin: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>Label</key><string>com.firstintent.ccteam</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>{bin}</string>\n\t\t<string>start</string>\n\t</array>\n\
             \t<key>RunAtLoad</key><true/>\n\t<key>KeepAlive</key><true/>\n\
             </dict>\n</plist>\n"
        )
    }

    struct RecordingRunner {
        calls: Vec<String>,
        succeed: bool,
    }

    impl RecordingRunner {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                succeed: true,
            }
        }
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[&str]) -> Result<bool> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            Ok(self.succeed)
        }
    }

    fn fake_paths(tmp: &tempfile::TempDir) -> LegacyServicePaths {
        LegacyServicePaths {
            systemd_unit: tmp.path().join("config/systemd/user/ccteam.service"),
            launchd_plist: tmp
                .path()
                .join("Library/LaunchAgents/com.firstintent.ccteam.plist"),
        }
    }

    #[test]
    fn fingerprint_accepts_both_historical_templates() {
        assert!(systemd_unit_matches_installer(MAKEFILE_UNIT));
        assert!(systemd_unit_matches_installer(INSTALL_SH_UNIT));
        assert!(launchd_plist_matches_installer(&installer_plist(
            "/home/u/.local/bin/ccteam"
        )));
    }

    #[test]
    fn fingerprint_rejects_hand_written_units() {
        assert!(!systemd_unit_matches_installer(HAND_WRITTEN_UNIT));
        // ccteam basename but not `start` → not ours.
        assert!(!systemd_unit_matches_installer(
            "Description=ccteam daemon fork\nExecStart=/usr/bin/ccteam serve\n"
        ));
        // `start` but a different program → not ours.
        assert!(!systemd_unit_matches_installer(
            "Description=ccteam daemon fork\nExecStart=/usr/bin/other start\n"
        ));
        // Wrong label / wrong args on the plist side.
        assert!(!launchd_plist_matches_installer(&installer_plist(
            "/usr/local/bin/other"
        )));
        assert!(!launchd_plist_matches_installer(
            "<plist><dict><key>Label</key><string>com.example.foo</string></dict></plist>"
        ));
    }

    #[test]
    fn installer_unit_is_stopped_then_removed_then_reloaded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        std::fs::create_dir_all(paths.systemd_unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.systemd_unit, MAKEFILE_UNIT).unwrap();

        let mut runner = RecordingRunner::new();
        let outcome = run_takeover(&paths, &mut runner).unwrap();

        // A3 semantics: systemctl stop (disable --now) FIRST, unit
        // removed second, daemon-reload last — then the caller's
        // pid-detach start proceeds on a clean slate.
        assert_eq!(
            runner.calls,
            vec![
                "systemctl --user disable --now ccteam".to_string(),
                "systemctl --user daemon-reload".to_string(),
            ]
        );
        assert!(
            !paths.systemd_unit.exists(),
            "installer unit must be removed"
        );
        match outcome {
            TakeoverOutcome::Migrated { unit, actions } => {
                assert_eq!(unit, paths.systemd_unit);
                assert_eq!(actions.len(), 3, "disable + remove + reload: {actions:?}");
                assert!(actions[0].contains("disable --now"));
                assert!(actions[1].contains("removed"));
            }
            other => panic!("expected Migrated, got {other:?}"),
        }
    }

    #[test]
    fn hand_written_unit_is_never_touched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        std::fs::create_dir_all(paths.systemd_unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.systemd_unit, HAND_WRITTEN_UNIT).unwrap();

        let mut runner = RecordingRunner::new();
        let outcome = run_takeover(&paths, &mut runner).unwrap();

        assert_eq!(
            outcome,
            TakeoverOutcome::ForeignUnitPresent {
                unit: paths.systemd_unit.clone()
            }
        );
        assert!(
            runner.calls.is_empty(),
            "no systemctl may run for a foreign unit: {:?}",
            runner.calls
        );
        assert_eq!(
            std::fs::read_to_string(&paths.systemd_unit).unwrap(),
            HAND_WRITTEN_UNIT,
            "foreign unit content must be untouched"
        );
    }

    #[test]
    fn takeover_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        std::fs::create_dir_all(paths.systemd_unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.systemd_unit, INSTALL_SH_UNIT).unwrap();

        let mut runner = RecordingRunner::new();
        assert!(matches!(
            run_takeover(&paths, &mut runner).unwrap(),
            TakeoverOutcome::Migrated { .. }
        ));
        // Second run: nothing left to do, no commands.
        let calls_after_first = runner.calls.len();
        assert_eq!(
            run_takeover(&paths, &mut runner).unwrap(),
            TakeoverOutcome::NothingToDo
        );
        assert_eq!(runner.calls.len(), calls_after_first);
    }

    #[test]
    fn takeover_survives_systemctl_spawn_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        std::fs::create_dir_all(paths.systemd_unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.systemd_unit, INSTALL_SH_UNIT).unwrap();

        struct FailingRunner;
        impl CommandRunner for FailingRunner {
            fn run(&mut self, _program: &str, _args: &[&str]) -> Result<bool> {
                anyhow::bail!("no such binary")
            }
        }
        let outcome = run_takeover(&paths, &mut FailingRunner).unwrap();
        assert!(matches!(outcome, TakeoverOutcome::Migrated { .. }));
        assert!(
            !paths.systemd_unit.exists(),
            "unit removed even without systemctl"
        );
    }

    #[test]
    fn installer_plist_is_booted_out_and_removed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        std::fs::create_dir_all(paths.launchd_plist.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.launchd_plist,
            installer_plist("/usr/local/bin/ccteam"),
        )
        .unwrap();

        let mut runner = RecordingRunner::new();
        let outcome = run_takeover(&paths, &mut runner).unwrap();
        assert!(matches!(outcome, TakeoverOutcome::Migrated { .. }));
        assert_eq!(runner.calls.len(), 1);
        assert!(runner.calls[0].starts_with("launchctl bootout gui/"));
        assert!(runner.calls[0].ends_with("/com.firstintent.ccteam"));
        assert!(!paths.launchd_plist.exists());
    }

    #[test]
    fn detect_legacy_unit_classifies_installer_vs_foreign() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        assert_eq!(detect_legacy_unit(&paths), None);

        std::fs::create_dir_all(paths.systemd_unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.systemd_unit, MAKEFILE_UNIT).unwrap();
        assert_eq!(
            detect_legacy_unit(&paths),
            Some((paths.systemd_unit.clone(), true))
        );

        std::fs::write(&paths.systemd_unit, HAND_WRITTEN_UNIT).unwrap();
        assert_eq!(
            detect_legacy_unit(&paths),
            Some((paths.systemd_unit.clone(), false))
        );
    }
}
