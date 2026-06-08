//! D2 guard: production tmux shell-outs must stay inside the backend layer.

use std::path::{Path, PathBuf};

#[test]
fn tmux_command_shellouts_stay_in_backend_whitelist() {
    let root = workspace_root();
    let mut offenders = Vec::new();
    visit_rs_files(&root.join("crates"), &mut |path| {
        if allowed_tmux_shellout_file(&root, path) {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        if text.contains("Command::new(\"tmux\")")
            || text.contains("std::process::Command::new(\"tmux\")")
        {
            offenders.push(
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
    });

    assert!(
        offenders.is_empty(),
        "tmux shell-outs must route through the mux backend whitelist; offenders: {offenders:#?}"
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under <workspace>/crates/ccteam-harness")
        .to_path_buf()
}

fn visit_rs_files(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, f);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            f(&path);
        }
    }
}

fn allowed_tmux_shellout_file(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel = rel.to_string_lossy().replace('\\', "/");
    rel.contains("/tests/")
        || rel == "crates/ccteam-harness/src/tmux_ops.rs"
        || rel.starts_with("crates/ccteam-harness/src/tmux_backend/")
        || rel == "crates/ccteam-core/src/tmux.rs"
}
