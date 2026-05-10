//! V0.3 M5.3 — decision-file candidate scan.
//!
//! Lists `~/projects/<slug>/.ccteam/decision-*.md` paths so the
//! project-detail template can render an `<select>` of valid
//! decision-file destinations for `inject_decision`. **Read-only** —
//! the scan never creates or mutates anything; it just enumerates.
//!
//! The MCP `tool_inject_decision` historically wrote to one of a
//! small enum of decision-kind paths (PRD §6.2.2 example). For the
//! web layer we keep things mechanical: anything matching the
//! `decision-*.md` glob already on disk is a valid target. Empty list
//! → the form's `<select>` shows a placeholder + is disabled.

use std::path::PathBuf;

use ccteam_core::CcteamPaths;

/// Scan the project's `.ccteam/` directory for decision-file
/// candidates. Returns absolute paths sorted lexicographically (which,
/// because filenames embed timestamps in `decision-<utc>.md` form, is
/// also chronological).
pub fn scan_candidates(paths: &CcteamPaths, slug: &str) -> Vec<PathBuf> {
    let dir = paths.project_ccteam_dir(slug);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.starts_with("decision-") && n.ends_with(".md"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_paths(root: &std::path::Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        assert!(scan_candidates(&paths, "demo").is_empty());
    }

    #[test]
    fn returns_only_decision_files_sorted() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let dir = paths.project_ccteam_dir("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("decision-2026-01.md"), "x").unwrap();
        std::fs::write(dir.join("decision-2026-03.md"), "y").unwrap();
        std::fs::write(dir.join("decision-2026-02.md"), "z").unwrap();
        std::fs::write(dir.join("state.json"), "{}").unwrap();
        std::fs::write(dir.join("notdecision.md"), "n").unwrap();

        let out = scan_candidates(&paths, "demo");
        assert_eq!(out.len(), 3);
        let names: Vec<String> = out
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "decision-2026-01.md".to_string(),
                "decision-2026-02.md".to_string(),
                "decision-2026-03.md".to_string(),
            ]
        );
    }
}
