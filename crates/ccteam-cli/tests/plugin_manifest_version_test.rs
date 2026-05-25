//! V0.6.8 F201 — assert the plugin manifest JSON files + the workspace
//! `Cargo.toml` pin all agree on the current ccteam version.
//!
//! Drift between `Cargo.toml::workspace.package.version` and the four
//! `version` strings carried by the Claude + Codex plugin manifests is
//! invisible to `cargo build` / `cargo test` and was discovered (twice)
//! during V0.6.7 / V0.6.8 ship prep — the binary shipped one version
//! and the marketplace metadata advertised an older one. This test
//! turns that drift into a CI failure so the ship-gate catches it.
//!
//! Sources of truth (all four must equal `workspace.package.version`):
//!   1. `.claude-plugin/plugin.json::version`
//!   2. `.claude-plugin/marketplace.json::version`
//!   3. `.claude-plugin/marketplace.json::plugins[0].version`
//!   4. `.codex-plugin/plugin.json::version`
//!
//! When adding a new plugin manifest file, add it here too.

use std::path::{Path, PathBuf};

/// Walk up from this crate's manifest dir (`crates/ccteam-cli/`) to
/// the workspace root.
fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/ccteam-cli/ → workspace root is two levels up.
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("repo_root: CARGO_MANIFEST_DIR must have two parents")
        .to_path_buf()
}

/// Read `workspace.package.version` from the workspace `Cargo.toml`
/// without pulling in a TOML dep — the file is small and the field is
/// stable. We look for the first `version = "X.Y.Z"` after the
/// `[workspace.package]` header.
fn parse_workspace_version(repo_root: &Path) -> String {
    let cargo_toml = repo_root.join("Cargo.toml");
    let body =
        std::fs::read_to_string(&cargo_toml).expect("read workspace Cargo.toml for version probe");
    let after_header = body
        .split_once("[workspace.package]")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("Cargo.toml missing [workspace.package] section"));
    for line in after_header.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version") {
            // Expect `version = "X.Y.Z"`.
            let rest = rest.trim_start_matches([' ', '\t', '=']);
            let quoted = rest
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_end_matches(',');
            return quoted.to_string();
        }
        if trimmed.starts_with('[') {
            break;
        }
    }
    panic!("Cargo.toml::[workspace.package].version not found");
}

/// Read a single top-level `"version"` string from a JSON manifest.
fn parse_json_top_version(path: &Path) -> String {
    let body =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parse {} as JSON: {e}", path.display()));
    v.get("version")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| panic!("{} missing top-level `version` string", path.display()))
}

/// Read the nested `plugins[0].version` from the marketplace manifest.
fn parse_marketplace_plugin_version(path: &Path) -> String {
    let body =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parse {} as JSON: {e}", path.display()));
    v.get("plugins")
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p0| p0.get("version"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| panic!("{} missing `plugins[0].version` string", path.display()))
}

#[test]
fn plugin_manifests_match_workspace_version() {
    let root = repo_root();
    let workspace_version = parse_workspace_version(&root);

    let claude_plugin = parse_json_top_version(&root.join(".claude-plugin/plugin.json"));
    let codex_plugin = parse_json_top_version(&root.join(".codex-plugin/plugin.json"));
    let marketplace_top = parse_json_top_version(&root.join(".claude-plugin/marketplace.json"));
    let marketplace_plugin =
        parse_marketplace_plugin_version(&root.join(".claude-plugin/marketplace.json"));

    assert_eq!(
        claude_plugin, workspace_version,
        ".claude-plugin/plugin.json::version ({claude_plugin}) must match \
         workspace.package.version ({workspace_version})"
    );
    assert_eq!(
        codex_plugin, workspace_version,
        ".codex-plugin/plugin.json::version ({codex_plugin}) must match \
         workspace.package.version ({workspace_version})"
    );
    assert_eq!(
        marketplace_top, workspace_version,
        ".claude-plugin/marketplace.json::version ({marketplace_top}) must match \
         workspace.package.version ({workspace_version})"
    );
    assert_eq!(
        marketplace_plugin, workspace_version,
        ".claude-plugin/marketplace.json::plugins[0].version ({marketplace_plugin}) must \
         match workspace.package.version ({workspace_version})"
    );
}
