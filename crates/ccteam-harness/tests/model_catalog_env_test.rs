//! `model_catalog`'s environment-resolved seam (`record_vendor_models_best_effort`).
//!
//! Lives in an integration binary, not the lib target, because it mutates
//! process-wide `HOME`/`CCTEAM_HOME` — AGENTS.md §六: env-mutating tests get
//! their own process, since the lib target runs everything in ONE process at
//! full parallelism and a pinned `HOME` there is visible to every other test.
//! The lib module keeps the four `_in(root)`-injected tests, which need no env.

use std::ffi::OsString;

use ccteam_harness::model_catalog::{
    model_catalog_path_in, record_vendor_models_best_effort, CatalogModel,
};
use serial_test::serial;

const ENV_KEYS: &[&str] = &["HOME", "CCTEAM_HOME"];

/// RAII restore. Manual restore at the end of a test is not enough: a panic
/// mid-test would leak a tempdir-pinned `HOME` into everything that follows in
/// this binary.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn capture() -> Self {
        Self {
            saved: ENV_KEYS
                .iter()
                .copied()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter().rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn model(id: &str) -> CatalogModel {
    CatalogModel {
        id: id.to_string(),
        display_name: Some(format!("Display {id}")),
        efforts: vec!["low".to_string(), "high".to_string()],
    }
}

#[test]
#[serial]
fn env_resolution_prefers_ccteam_home_and_falls_back_to_home() {
    let _guard = EnvGuard::capture();
    let home = tempfile::tempdir().unwrap();
    let override_root = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("CCTEAM_HOME", override_root.path());
    }

    record_vendor_models_best_effort("grok", "ACP availableModels", vec![model("grok-4")]);
    assert!(model_catalog_path_in(override_root.path()).is_file());
    assert!(!model_catalog_path_in(&home.path().join(".ccteam")).exists());

    unsafe {
        std::env::remove_var("CCTEAM_HOME");
    }
    record_vendor_models_best_effort("opencode", "ACP configOptions", vec![model("open-model")]);
    assert!(model_catalog_path_in(&home.path().join(".ccteam")).is_file());
}
