//! Regression tests for backend-aware `ccteam internal peek`.

use std::process::Command;

#[cfg(unix)]
#[test]
fn internal_peek_default_rmux_does_not_shell_out_to_path_tmux() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    let sentinel = tmp.path().join("tmux-called");
    let fake_tmux = fake_bin.join("tmux");
    std::fs::write(
        &fake_tmux,
        "#!/bin/sh\nprintf called > \"$CCTEAM_TMUX_SENTINEL\"\nexit 99\n",
    )
    .expect("write fake tmux");
    let mut perms = std::fs::metadata(&fake_tmux)
        .expect("fake tmux metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_tmux, perms).expect("chmod fake tmux");

    let joined_path = match std::env::var_os("PATH") {
        Some(path) => {
            let mut paths = vec![fake_bin.clone()];
            paths.extend(std::env::split_paths(&path));
            std::env::join_paths(paths).expect("join PATH")
        }
        None => fake_bin.into_os_string(),
    };

    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["internal", "peek", "missing"])
        .env("PATH", joined_path)
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .env("CCTEAM_MUX_BACKEND", "rmux")
        .env("CCTEAM_TMUX_SENTINEL", &sentinel)
        .output()
        .expect("spawn ccteam internal peek");

    assert!(
        !out.status.success(),
        "missing rmux session should fail, stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rmux"),
        "peek should fail through rmux, got stderr={stderr}"
    );
    assert!(
        !sentinel.exists(),
        "default rmux peek must not invoke a PATH tmux binary"
    );
}
