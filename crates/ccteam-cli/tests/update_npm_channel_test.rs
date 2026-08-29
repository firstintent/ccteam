//! v0.10.5 PLUG-1 — `ccteam update --channel npm --binary <path>`.
//!
//! The npm channel does not download anything: the package manager
//! already put `@ccteam/engine-<os>-<cpu>/bin/ccteam` on disk, so ccteam
//! validates that file, installs it through install.sh's ladder, and then
//! runs the standalone channel's restart contract. These tests cover the
//! parts that touch the filesystem — validation, the ladder destination,
//! and the two "somebody else owns this" refusals — with `--no-restart`
//! so no daemon is involved.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    ccteam_home: PathBuf,
    install_dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let ccteam_home = home.join(".ccteam");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(home.join("projects")).unwrap();
        Self {
            _tmp: tmp,
            home,
            ccteam_home,
            install_dir,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(ccteam_bin())
            .args(args)
            .env("HOME", &self.home)
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", self.home.join("projects"))
            .env("CCTEAM_INSTALL_DIR", &self.install_dir)
            .env("RUST_LOG", "warn")
            .output()
            .expect("run ccteam update")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.run(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "`ccteam {}` did not print one JSON line ({err}).\n--- stdout ---\n{stdout}\n\
                 --- stderr ---\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn dest(&self) -> PathBuf {
        self.install_dir.join("ccteam")
    }
}

/// The version `ccteam --version` reports, as the update path parses it.
fn own_version() -> String {
    let out = Command::new(ccteam_bin())
        .arg("--version")
        .output()
        .expect("ccteam --version");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|t| t.trim_start_matches('v').to_string())
        .find(|t| t.contains('.') && t.starts_with(|c: char| c.is_ascii_digit()))
        .expect("parse own version")
}

#[test]
#[cfg(unix)]
fn npm_channel_installs_the_supplied_binary_and_verifies_its_version() {
    let sb = Sandbox::new();
    let verdict = sb.json(&[
        "update",
        "--channel",
        "npm",
        "--binary",
        ccteam_bin(),
        "--no-restart",
        "--json",
    ]);
    assert_eq!(verdict["status"], "binarySwapped", "{verdict}");
    assert_eq!(verdict["channel"], "npm", "{verdict}");
    assert_eq!(
        verdict["version"],
        own_version(),
        "the reported version must come from the SOURCE binary's --version, \
         not from whatever compiled the updater: {verdict}"
    );
    assert_eq!(
        verdict["binary"].as_str(),
        Some(sb.dest().display().to_string().as_str()),
        "install goes to install.sh's ladder location: {verdict}"
    );

    let dest = sb.dest();
    assert!(dest.is_file(), "{} must exist", dest.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "installed binary must be mode 755");
    }
    assert!(
        !sb.install_dir.join(".ccteam.new").exists(),
        "the atomic-swap temp file must not survive"
    );

    // The install marker is how `install_channel::detect` answers next
    // time; without it the channel would be re-guessed from the path.
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.ccteam_home.join("install-channel")).expect("marker written"),
    )
    .expect("marker is JSON");
    assert_eq!(marker["channel"], "npm", "{marker}");
    assert_eq!(marker["bin"], dest.display().to_string(), "{marker}");
}

#[test]
#[cfg(unix)]
fn npm_channel_refuses_to_overwrite_a_symlinked_install() {
    // A symlink belongs to whatever created it (a package manager, a
    // dotfiles repo); clobbering it leaves two rival ccteam binaries and
    // an update that appears to do nothing.
    let sb = Sandbox::new();
    let real = sb.home.join("elsewhere-ccteam");
    std::fs::write(&real, b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink(&real, sb.dest()).unwrap();

    let out = sb.run(&[
        "update",
        "--channel",
        "npm",
        "--binary",
        ccteam_bin(),
        "--no-restart",
        "--json",
    ]);
    assert!(!out.status.success(), "a refusal must exit non-zero");
    let verdict: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json verdict");
    assert_eq!(verdict["status"], "error", "{verdict}");
    assert_eq!(verdict["code"], "destIsSymlink", "{verdict}");
    assert!(
        std::fs::symlink_metadata(sb.dest())
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink must be left exactly as it was"
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "#!/bin/sh\n",
        "its target must not be written through"
    );
}

#[test]
#[cfg(unix)]
fn npm_channel_rejects_a_binary_that_is_not_a_usable_ccteam() {
    let sb = Sandbox::new();

    // Not executable.
    let plain = sb.home.join("not-exec");
    std::fs::write(&plain, b"x").unwrap();
    let out = sb.run(&[
        "update",
        "--channel",
        "npm",
        "--binary",
        plain.to_str().unwrap(),
        "--no-restart",
        "--json",
    ]);
    assert!(!out.status.success());
    let verdict: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(verdict["code"], "binaryNotExecutable", "{verdict}");

    // Executable, but does not answer `--version` like ccteam.
    let bogus = sb.home.join("bogus");
    std::fs::write(&bogus, b"#!/bin/sh\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bogus, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = sb.run(&[
        "update",
        "--channel",
        "npm",
        "--binary",
        bogus.to_str().unwrap(),
        "--no-restart",
        "--json",
    ]);
    assert!(!out.status.success());
    let verdict: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(verdict["code"], "binaryVersionUnreadable", "{verdict}");

    // Nothing was installed by either refusal.
    assert!(
        !Path::new(&sb.dest()).exists(),
        "a rejected --binary must leave the destination untouched"
    );
}

#[test]
#[cfg(unix)]
fn binary_flag_is_refused_on_channels_that_download_their_own() {
    let sb = Sandbox::new();
    let out = sb.run(&[
        "update",
        "--channel",
        "standalone",
        "--binary",
        ccteam_bin(),
        "--no-restart",
        "--json",
    ]);
    assert!(!out.status.success());
    let verdict: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(verdict["code"], "binaryNotSupported", "{verdict}");
    assert!(
        !sb.dest().exists(),
        "nothing may be installed on the refused path"
    );
}
