//! Best-effort last-seen model catalogs captured from vendor handshakes.
//!
//! The cache is advisory only. Adapters write the vendor facts they already
//! receive; no spawn path reads this file and a cache failure never fails a
//! session. The on-disk shape is a transparent vendor-keyed map at
//! `~/.ccteam/model-catalog.json` (honouring `CCTEAM_HOME`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::adapter::ccteam_root_from_env;
use crate::execution::fs_atomic::atomic_write_durable;

/// One vendor-reported model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    /// Opaque model id passed to the vendor verbatim.
    pub id: String,
    /// Vendor display label when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Vendor-supported reasoning effort tokens when supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// Last successful observation for one vendor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorModelCatalog {
    /// RFC3339 timestamp recorded by ccteam at capture time.
    pub observed_at: String,
    /// Vendor protocol surface that supplied the catalog.
    pub source: String,
    /// Vendor models, kept in vendor order.
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// Per-vendor cache. Transparent serialization produces the owner-approved
/// `{vendor: {observed_at, source, models}}` top-level shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelCatalog(pub BTreeMap<String, VendorModelCatalog>);

static CATALOG_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Cache path under an injected ccteam root.
pub fn model_catalog_path_in(root: &Path) -> PathBuf {
    root.join("model-catalog.json")
}

/// Read an injected cache. Missing, unreadable, or corrupt files deliberately
/// degrade to an empty catalog: this cache must never obstruct a session.
pub fn load_model_catalog_in(root: &Path) -> ModelCatalog {
    std::fs::read(model_catalog_path_in(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Read the environment-resolved cache, or an empty catalog when no home can
/// be resolved.
pub fn load_model_catalog() -> ModelCatalog {
    ccteam_root_from_env()
        .as_deref()
        .map(load_model_catalog_in)
        .unwrap_or_default()
}

/// Atomically replace one vendor's last-seen entry under an injected root.
/// Empty captures are ignored so a transient vendor omission cannot erase a
/// useful prior observation.
pub fn record_vendor_models_in(
    root: &Path,
    vendor: &str,
    source: &str,
    models: Vec<CatalogModel>,
) -> Result<()> {
    let vendor = vendor.trim();
    if vendor.is_empty() || models.is_empty() {
        return Ok(());
    }
    let lock = CATALOG_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("model catalog write lock poisoned"))?;
    std::fs::create_dir_all(root)
        .with_context(|| format!("create ccteam root {}", root.display()))?;
    let mut catalog = load_model_catalog_in(root);
    catalog.0.insert(
        vendor.to_string(),
        VendorModelCatalog {
            observed_at: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            models,
        },
    );
    let bytes = serde_json::to_vec_pretty(&catalog).context("serialize model catalog")?;
    atomic_write_durable(&model_catalog_path_in(root), &bytes)
}

/// Environment-resolved adapter seam. Every error is intentionally swallowed:
/// catalog persistence is never part of the spawn/turn success contract.
pub fn record_vendor_models_best_effort(vendor: &str, source: &str, models: Vec<CatalogModel>) {
    if let Some(root) = ccteam_root_from_env() {
        let _ = record_vendor_models_in(&root, vendor, source, models);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn model(id: &str) -> CatalogModel {
        CatalogModel {
            id: id.to_string(),
            display_name: Some(format!("Display {id}")),
            efforts: vec!["low".to_string(), "high".to_string()],
        }
    }

    #[test]
    fn injected_cache_round_trips_vendors_without_clobbering() {
        let root = tempfile::tempdir().unwrap();
        record_vendor_models_in(
            root.path(),
            "claude",
            "initialize.models",
            vec![model("opus")],
        )
        .unwrap();
        record_vendor_models_in(root.path(), "codex", "model/list", vec![model("gpt-5")]).unwrap();

        let got = load_model_catalog_in(root.path());
        assert_eq!(got.0["claude"].models[0].id, "opus");
        assert_eq!(got.0["codex"].source, "model/list");
        assert!(!got.0["codex"].observed_at.is_empty());
        assert!(!model_catalog_path_in(root.path())
            .with_file_name("model-catalog.json.tmp")
            .exists());
    }

    #[test]
    fn corrupt_or_missing_cache_is_tolerated() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load_model_catalog_in(root.path()), ModelCatalog::default());
        std::fs::write(model_catalog_path_in(root.path()), b"{not-json").unwrap();
        assert_eq!(load_model_catalog_in(root.path()), ModelCatalog::default());
    }

    #[test]
    fn empty_capture_preserves_last_seen_entry() {
        let root = tempfile::tempdir().unwrap();
        record_vendor_models_in(
            root.path(),
            "kimi",
            "ACP availableModels",
            vec![model("k2")],
        )
        .unwrap();
        record_vendor_models_in(root.path(), "kimi", "ACP availableModels", Vec::new()).unwrap();
        assert_eq!(
            load_model_catalog_in(root.path()).0["kimi"].models[0].id,
            "k2"
        );
    }

    #[test]
    #[serial]
    fn env_resolution_prefers_ccteam_home_and_falls_back_to_home() {
        let old_home = std::env::var_os("HOME");
        let old_ccteam_home = std::env::var_os("CCTEAM_HOME");
        let home = tempfile::tempdir().unwrap();
        let override_root = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("CCTEAM_HOME", override_root.path());

        record_vendor_models_best_effort("grok", "ACP availableModels", vec![model("grok-4")]);
        assert!(model_catalog_path_in(override_root.path()).is_file());
        assert!(!model_catalog_path_in(&home.path().join(".ccteam")).exists());

        std::env::remove_var("CCTEAM_HOME");
        record_vendor_models_best_effort(
            "opencode",
            "ACP configOptions",
            vec![model("open-model")],
        );
        assert!(model_catalog_path_in(&home.path().join(".ccteam")).is_file());

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_ccteam_home {
            Some(value) => std::env::set_var("CCTEAM_HOME", value),
            None => std::env::remove_var("CCTEAM_HOME"),
        }
    }
}
