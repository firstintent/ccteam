//! LIVE smoke (network — `#[ignore]`, NOT in the deterministic baseline):
//! installs a real multi-file skill from the live ccteam-hub into a TempDir,
//! end-to-end through the real engine (load_catalog + install_plugin against
//! `raw.githubusercontent.com`). Proves the track-upstream chain works against
//! production: pointer index -> upstream fetch -> per-file sha gate -> dir
//! landing. Run explicitly:
//!   cargo test -p ccteam-im --test hub_live_smoke -- --ignored --nocapture

#![cfg(unix)]

use tempfile::TempDir;

#[tokio::test]
#[ignore = "network: hits the live ccteam-hub"]
async fn live_install_multi_file_skill() {
    let home = TempDir::new().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());
    let paths = ccteam_core::CcteamPaths::from_env().unwrap();

    // Pull the LIVE catalog (force refresh) and pick a real multi-file skill.
    let index = ccteam_im::hub::load_catalog(ccteam_core::HUB_RAW_BASE, &paths, true)
        .await
        .expect("fetch live ccteam-hub index.json");
    let plugin = index
        .plugins
        .iter()
        .find(|p| p.manifest.as_ref().map(|m| m.len() > 1).unwrap_or(false))
        .expect("the live index must contain a multi-file skill")
        .clone();
    let manifest = plugin.manifest.clone().unwrap();
    println!(
        "live multi-file skill `{}` -> {} files (upstream {})",
        plugin.id,
        manifest.len(),
        plugin.upstream
    );

    // Install through the real engine into a throwaway project.
    let proj = TempDir::new().unwrap();
    let res = ccteam_im::hub::install_plugin(proj.path(), &plugin, None, false)
        .await
        .expect("live multi-file install");
    let skill_dir = res.path.parent().expect("SKILL.md has a parent dir");

    // Every manifest file must have landed.
    for entry in &manifest {
        let p = skill_dir.join(&entry.relpath);
        assert!(p.is_file(), "manifest file did not land: {}", p.display());
    }
    println!(
        "OK: {} files landed under {}",
        manifest.len(),
        skill_dir.display()
    );

    // installed_status must now read back as Installed.
    assert_eq!(
        ccteam_im::hub::installed_status(proj.path(), &plugin),
        ccteam_im::hub::InstalledStatus::Installed,
        "freshly installed multi-file skill must read back Installed"
    );

    std::env::remove_var("CCTEAM_HOME");
}
