//! v0.9.7 — install-channel detection + per-channel update command (PRD
//! F3.1).
//!
//! `ccteam update` has to know **how this binary was installed** before it
//! can update it. [`detect`] answers that with a fixed priority ladder
//! (env → marker file → path heuristic → `Other`), and
//! [`suggested_update_command`] maps a channel to the command that
//! refreshes it.
//!
//! Adapted from openai/codex `install-context` (`InstallMethod` enum +
//! `current()` detection priority), Apache-2.0 — see `LICENSES.md`. The
//! codex `CodexPackageLayout` / releases / resources machinery is dropped
//! (ccteam has no managed package tree); ccteam adds a marker-file layer
//! (`~/.ccteam/install-channel`, written by `install.sh` on a successful
//! standalone install) that codex has no equivalent of.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::CcteamPaths;

/// Marker file name under `~/.ccteam/` written by a successful standalone
/// `install.sh` run (JSON [`InstallMarker`]).
pub const INSTALL_CHANNEL_MARKER: &str = "install-channel";

/// npm-shim env vars (reserved for the V094 npm distribution; this
/// version only reads them so the enum + `ccteam update` arm exist).
pub const MANAGED_BY_NPM_ENV: &str = "CCTEAM_MANAGED_BY_NPM";
pub const MANAGED_BY_BUN_ENV: &str = "CCTEAM_MANAGED_BY_BUN";
pub const MANAGED_BY_PNPM_ENV: &str = "CCTEAM_MANAGED_BY_PNPM";

/// The exact non-interactive pipeline `ccteam update` replays for a
/// standalone install: `install.sh` itself does the download + sha256 +
/// atomic mv, so ccteam embeds NO second downloader (PRD "升级 = 再走安装
/// 管道"). `CCTEAM_POST_INSTALL=none` keeps install.sh from touching the
/// daemon — the upgrade-restart contract owns that.
pub const STANDALONE_INSTALL_PIPELINE: &str = "curl -fsSL \
     https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh \
     | CCTEAM_POST_INSTALL=none sh";

/// Project home page, printed when the channel can't be classified.
pub const REPO_URL: &str = "https://github.com/firstintent/ccteam";

/// How this ccteam binary was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallChannel {
    /// npm global install (V094; reserved).
    Npm,
    /// bun global install (V094; reserved).
    Bun,
    /// pnpm global install (V094; reserved).
    Pnpm,
    /// The prebuilt binary dropped into `~/.local/bin` by `install.sh`.
    Standalone,
    /// A `cargo`-built binary (a dev tree's `target/{debug,release}` or a
    /// `cargo install` into `~/.cargo/bin`).
    Source,
    /// Anything else — ccteam can't self-update it.
    Other,
}

impl InstallChannel {
    /// Stable lowercase token (`"standalone"`, `"source"`, …) for display.
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallChannel::Npm => "npm",
            InstallChannel::Bun => "bun",
            InstallChannel::Pnpm => "pnpm",
            InstallChannel::Standalone => "standalone",
            InstallChannel::Source => "source",
            InstallChannel::Other => "other",
        }
    }
}

/// JSON body of `~/.ccteam/install-channel`. `install.sh` writes it on a
/// successful standalone install; [`detect`] reads `channel` from it as
/// the second-priority signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallMarker {
    pub channel: InstallChannel,
    /// Release tag the marker was written for (e.g. `v0.9.7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// RFC3339 install time (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
}

/// Resolve `~/.ccteam/install-channel`.
pub fn install_channel_marker_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join(INSTALL_CHANNEL_MARKER)
}

/// Read + parse the marker. `None` for missing / unreadable / unparseable
/// (any of those simply means "no marker signal").
pub fn read_marker(paths: &CcteamPaths) -> Option<InstallMarker> {
    let body = std::fs::read_to_string(install_channel_marker_path(paths)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Atomically publish the marker (tmp + rename in the same dir).
pub fn write_marker(paths: &CcteamPaths, marker: &InstallMarker) -> Result<()> {
    let path = install_channel_marker_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(marker).context("serialize install marker")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    if let Err(err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("publish install marker {}", path.display()));
    }
    Ok(())
}

/// Detect the install channel of the running binary (production entry).
pub fn detect(paths: &CcteamPaths) -> InstallChannel {
    let exe = std::env::current_exe().unwrap_or_default();
    detect_with(&exe, |k| std::env::var(k).ok(), paths)
}

/// Testable core: classify against an injected executable path + env
/// lookup so unit tests never depend on the real `current_exe()` /
/// process env. Priority high→low: managed-by env → marker file → path
/// heuristic → `Other`.
pub fn detect_with(
    exe: &Path,
    env_lookup: impl Fn(&str) -> Option<String>,
    paths: &CcteamPaths,
) -> InstallChannel {
    // 1. npm-shim env (reserved for V094).
    if env_set(&env_lookup, MANAGED_BY_NPM_ENV) {
        return InstallChannel::Npm;
    }
    if env_set(&env_lookup, MANAGED_BY_BUN_ENV) {
        return InstallChannel::Bun;
    }
    if env_set(&env_lookup, MANAGED_BY_PNPM_ENV) {
        return InstallChannel::Pnpm;
    }
    // 2. marker file written by install.sh.
    if let Some(marker) = read_marker(paths) {
        return marker.channel;
    }
    // 3. path heuristic on the executable location.
    if let Some(channel) = channel_from_exe_path(exe) {
        return channel;
    }
    // 4. unknown.
    InstallChannel::Other
}

/// True iff `key` resolves to a non-empty value (an npm shim sets these to
/// e.g. `1`; presence is the signal).
fn env_set(env_lookup: &impl Fn(&str) -> Option<String>, key: &str) -> bool {
    env_lookup(key).is_some_and(|v| !v.trim().is_empty())
}

/// Classify by executable location:
/// - under `~/.local/bin` → [`InstallChannel::Standalone`] (install.sh drop);
/// - under `~/.cargo/bin` or a `target/{debug,release}` build tree →
///   [`InstallChannel::Source`];
/// - anything else → `None` (caller falls through to `Other`).
fn channel_from_exe_path(exe: &Path) -> Option<InstallChannel> {
    let parent = exe.parent()?;
    if parent.ends_with(".local/bin") {
        return Some(InstallChannel::Standalone);
    }
    if parent.ends_with(".cargo/bin") {
        return Some(InstallChannel::Source);
    }
    // Cargo build tree: `<…>/target/{debug,release}[/deps]/ccteam`.
    let mut dirs: Vec<&str> = exe
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    dirs.pop(); // drop the file name; look at directory components only
    if let Some(pos) = dirs.iter().position(|c| *c == "target") {
        if dirs
            .get(pos + 1)
            .is_some_and(|c| *c == "debug" || *c == "release")
        {
            return Some(InstallChannel::Source);
        }
    }
    None
}

/// The command that updates a given channel, for display in `ccteam
/// status` / `doctor` (and the guidance `ccteam update` prints for
/// channels it can't self-update). `None` for [`InstallChannel::Other`]
/// (ccteam has no way to refresh it).
pub fn suggested_update_command(channel: &InstallChannel) -> Option<String> {
    match channel {
        InstallChannel::Standalone => Some(STANDALONE_INSTALL_PIPELINE.to_string()),
        InstallChannel::Npm => Some("npm install -g @firstintent/ccteam@latest".to_string()),
        InstallChannel::Bun => Some("bun add -g @firstintent/ccteam@latest".to_string()),
        InstallChannel::Pnpm => Some("pnpm add -g @firstintent/ccteam@latest".to_string()),
        InstallChannel::Source => Some("git pull && make install".to_string()),
        InstallChannel::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn marker_roundtrips_through_atomic_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let marker = InstallMarker {
            channel: InstallChannel::Standalone,
            tag: Some("v0.9.7".to_string()),
            installed_at: Some("2026-07-22T00:00:00Z".to_string()),
        };
        write_marker(&p, &marker).unwrap();
        assert_eq!(read_marker(&p), Some(marker));
        assert!(!install_channel_marker_path(&p)
            .with_extension("tmp")
            .exists());
    }

    #[test]
    fn missing_or_corrupt_marker_reads_as_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        assert_eq!(read_marker(&p), None);
        std::fs::create_dir_all(&p.root).unwrap();
        std::fs::write(install_channel_marker_path(&p), b"not json").unwrap();
        assert_eq!(read_marker(&p), None);
    }

    #[test]
    fn env_beats_marker_beats_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        // A marker says Source; the path would say Standalone.
        write_marker(
            &p,
            &InstallMarker {
                channel: InstallChannel::Source,
                tag: None,
                installed_at: None,
            },
        )
        .unwrap();
        let standalone_exe = Path::new("/home/u/.local/bin/ccteam");

        // env wins over everything.
        let with_npm = |k: &str| (k == MANAGED_BY_NPM_ENV).then(|| "1".to_string());
        assert_eq!(
            detect_with(standalone_exe, with_npm, &p),
            InstallChannel::Npm
        );

        // no env → marker wins over the path heuristic.
        assert_eq!(
            detect_with(standalone_exe, |_| None, &p),
            InstallChannel::Source
        );

        // no env + no marker → path heuristic.
        std::fs::remove_file(install_channel_marker_path(&p)).unwrap();
        assert_eq!(
            detect_with(standalone_exe, |_| None, &p),
            InstallChannel::Standalone
        );
    }

    #[test]
    fn path_heuristic_classifies_local_bin_cargo_and_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp); // no marker present
        let cases = [
            ("/home/u/.local/bin/ccteam", InstallChannel::Standalone),
            ("/home/u/.cargo/bin/ccteam", InstallChannel::Source),
            (
                "/home/u/src/ccteam/target/debug/ccteam",
                InstallChannel::Source,
            ),
            (
                "/home/u/src/ccteam/target/release/ccteam",
                InstallChannel::Source,
            ),
            // Test binaries live under target/debug/deps/…
            (
                "/home/u/src/ccteam/target/release/deps/ccteam",
                InstallChannel::Source,
            ),
            // Unclassifiable → Other.
            ("/opt/weird/place/ccteam", InstallChannel::Other),
        ];
        for (exe, want) in cases {
            assert_eq!(detect_with(Path::new(exe), |_| None, &p), want, "exe {exe}");
        }
    }

    #[test]
    fn empty_exe_path_falls_through_to_other() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        assert_eq!(
            detect_with(Path::new(""), |_| None, &p),
            InstallChannel::Other
        );
    }

    #[test]
    fn suggested_command_per_channel() {
        assert_eq!(
            suggested_update_command(&InstallChannel::Standalone).as_deref(),
            Some(STANDALONE_INSTALL_PIPELINE)
        );
        assert_eq!(
            suggested_update_command(&InstallChannel::Source).as_deref(),
            Some("git pull && make install")
        );
        assert!(suggested_update_command(&InstallChannel::Npm)
            .unwrap()
            .contains("npm install -g"));
        assert!(suggested_update_command(&InstallChannel::Bun)
            .unwrap()
            .contains("bun add -g"));
        assert!(suggested_update_command(&InstallChannel::Pnpm)
            .unwrap()
            .contains("pnpm add -g"));
        assert_eq!(suggested_update_command(&InstallChannel::Other), None);
    }
}
