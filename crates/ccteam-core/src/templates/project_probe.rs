//! V0.6.6 F167 — lightweight project-type probe for `/ccteam-creator`
//! sensible defaults.
//!
//! The probe inspects a repo root via **file-existence checks only**
//! (no source parsing) and emits:
//!
//! - [`ProjectKind`] — Monorepo / SingleRepo / DocsOnly / ScriptsOnly / Empty
//! - [`Language`] top-3 (Rust / TypeScript / Python / Go / Java / Other)
//! - [`ProjectProbe::has_tests`] — at least one well-known test dir exists
//! - [`ProjectProbe::probable_scope`] — top-3 source subtrees the
//!   `ccteam-creator` skill should pre-populate into the rendered
//!   `workflow.yaml::agents.<role>.scope` field
//!
//! Scope cap is `3` deliberately: the ccteam repo itself probes to
//! 10+ Rust crates and a naive "all members" answer would blow the
//! V0.6.2 F140 blast-radius guarantee back open. Top-3 by descending
//! source line-count, ties broken alphabetically.
//!
//! **Non-goals (V0.7 epic):**
//!
//! - Cross-language adapter (the probe surfaces languages but does not
//!   pick per-language templates).
//! - Per-role auto-generated personas (full template library).
//! - LLM-assisted role inference.
//!
//! The probe is **pure heuristic** — when in doubt it under-reports
//! (e.g. returns `SingleRepo` rather than `Monorepo` for ambiguous
//! cases) so the skill's PROJECT PLAN never silently widens the scope
//! beyond what the user expected.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Coarse project shape, derived from manifest-file presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    /// `Cargo.toml` with `workspace.members`, `package.json` with
    /// `workspaces`, `pnpm-workspace.yaml`, or `go.work` present.
    Monorepo,
    /// Single top-level manifest (`Cargo.toml` without workspace,
    /// single `package.json`, `pyproject.toml`, `go.mod` alone, etc.)
    SingleRepo,
    /// Markdown / docs only — no source dir, no manifest.
    DocsOnly,
    /// Scripts (`.sh` / `.py` standalone) with no manifest / no
    /// proper src tree.
    ScriptsOnly,
    /// Empty (or unrecognized) directory.
    Empty,
}

impl ProjectKind {
    /// Wire name matching the JSON `--json` output.
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Monorepo => "monorepo",
            ProjectKind::SingleRepo => "single-repo",
            ProjectKind::DocsOnly => "docs-only",
            ProjectKind::ScriptsOnly => "scripts-only",
            ProjectKind::Empty => "empty",
        }
    }
}

/// Detected language tags — top-3 by source-file count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    Other,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Java => "java",
            Language::Other => "other",
        }
    }
}

/// The probe result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProbe {
    pub kind: ProjectKind,
    pub languages: Vec<Language>,
    pub has_tests: bool,
    /// Suggested `scope:` paths — relative to `repo_root`, top-3 by
    /// descending source LOC. Always non-empty for `Monorepo` /
    /// `SingleRepo`; `["docs"]` for `DocsOnly`; empty for `Empty` /
    /// `ScriptsOnly`.
    pub probable_scope: Vec<PathBuf>,
}

/// Probe a repo root. **Infallible by design** — every error path
/// degrades to a less-specific category (Empty when the dir is
/// unreadable, SingleRepo when monorepo detection trips midway). The
/// skill is expected to honor the user's explicit override; this is
/// only the "sensible default" seed.
pub fn probe(repo_root: &Path) -> ProjectProbe {
    // Step 1: detect monorepo first (strongest signal).
    if let Some(scope) = detect_monorepo(repo_root) {
        let (langs, has_tests) = detect_languages_and_tests(repo_root);
        return ProjectProbe {
            kind: ProjectKind::Monorepo,
            languages: langs,
            has_tests,
            probable_scope: scope,
        };
    }

    // Step 2: detect single-repo (any one manifest).
    if is_single_repo(repo_root) {
        let (langs, has_tests) = detect_languages_and_tests(repo_root);
        let scope = single_repo_scope(repo_root);
        return ProjectProbe {
            kind: ProjectKind::SingleRepo,
            languages: langs,
            has_tests,
            probable_scope: scope,
        };
    }

    // Step 3: docs-only.
    if is_docs_only(repo_root) {
        return ProjectProbe {
            kind: ProjectKind::DocsOnly,
            languages: Vec::new(),
            has_tests: false,
            probable_scope: docs_only_scope(repo_root),
        };
    }

    // Step 4: scripts-only.
    if is_scripts_only(repo_root) {
        let (langs, _) = detect_languages_and_tests(repo_root);
        return ProjectProbe {
            kind: ProjectKind::ScriptsOnly,
            languages: langs,
            has_tests: false,
            probable_scope: Vec::new(),
        };
    }

    // Step 5: empty / unrecognized.
    ProjectProbe {
        kind: ProjectKind::Empty,
        languages: Vec::new(),
        has_tests: false,
        probable_scope: Vec::new(),
    }
}

// -- monorepo detection --------------------------------------------------

/// Returns `Some(scope_paths)` when the root looks like a monorepo,
/// `None` otherwise. Scope is top-3 first-party member dirs.
fn detect_monorepo(repo_root: &Path) -> Option<Vec<PathBuf>> {
    // Cargo workspace.
    if let Some(scope) = cargo_workspace_members(repo_root) {
        return Some(scope);
    }
    // pnpm workspace.
    if repo_root.join("pnpm-workspace.yaml").is_file()
        || repo_root.join("pnpm-workspace.yml").is_file()
    {
        let scope = ts_monorepo_members(repo_root);
        if !scope.is_empty() {
            return Some(scope);
        }
    }
    // npm/yarn workspaces — coarse: package.json contains `"workspaces"`.
    let pkg = repo_root.join("package.json");
    if pkg.is_file() {
        if let Ok(s) = fs::read_to_string(&pkg) {
            if s.contains("\"workspaces\"") {
                let scope = ts_monorepo_members(repo_root);
                if !scope.is_empty() {
                    return Some(scope);
                }
            }
        }
    }
    // Go workspace.
    if repo_root.join("go.work").is_file() {
        let scope = go_workspace_members(repo_root);
        if !scope.is_empty() {
            return Some(scope);
        }
    }
    None
}

/// Parse `Cargo.toml` for `[workspace] members = [...]` and return the
/// top-3 by descending LOC (under `<member>/src/`). Falls back to
/// alphabetical when LOC counts tie.
fn cargo_workspace_members(repo_root: &Path) -> Option<Vec<PathBuf>> {
    let cargo = repo_root.join("Cargo.toml");
    if !cargo.is_file() {
        return None;
    }
    let body = fs::read_to_string(&cargo).ok()?;
    // Coarse parse — we don't need a full TOML reader to detect "this is
    // a workspace". Look for `[workspace]` section AND members entries.
    if !body.contains("[workspace]") && !body.contains("[workspace.package]") {
        return None;
    }
    let members = parse_cargo_workspace_members(&body);
    // If the manifest declares `[workspace]` but glob members didn't
    // expand to anything (e.g. `members = ["crates/*"]`), fall back to
    // scanning `crates/` for `Cargo.toml`-bearing subdirs.
    let mut resolved = resolve_member_paths(repo_root, &members);
    if resolved.is_empty() {
        // Standard ccteam layout: `crates/<member>/Cargo.toml`.
        for sub in ["crates", "packages", "apps", "libs"] {
            let dir = repo_root.join(sub);
            if dir.is_dir() {
                if let Ok(rd) = fs::read_dir(&dir) {
                    for ent in rd.flatten() {
                        let p = ent.path();
                        if p.is_dir() && p.join("Cargo.toml").is_file() {
                            resolved.push(p);
                        }
                    }
                }
            }
        }
    }
    if resolved.is_empty() {
        return None;
    }
    // Rank by descending source LOC under `<member>/src/`.
    let mut ranked: Vec<(PathBuf, usize)> = resolved
        .into_iter()
        .map(|p| {
            let loc = scan_loc(&p.join("src"));
            (p, loc)
        })
        .collect();
    // Secondary key: alphabetical.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<PathBuf> = ranked
        .into_iter()
        .take(3)
        .map(|(p, _)| relative_to(repo_root, &p))
        .collect();
    Some(top)
}

/// Coarse extract of the `members = [...]` list inside `[workspace]`.
/// Tolerates inline / multi-line arrays. Returns raw strings (might
/// include globs like `"crates/*"` which we resolve later).
fn parse_cargo_workspace_members(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_workspace = false;
    let mut in_members = false;
    let mut buf = String::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_workspace = line == "[workspace]";
            in_members = false;
            buf.clear();
            continue;
        }
        if !in_workspace {
            continue;
        }
        if in_members {
            buf.push_str(line);
            if line.contains(']') {
                in_members = false;
                extract_quoted(&buf, &mut out);
                buf.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("members") {
            // `members = [...]` or `members = [` (multi-line).
            let rest = rest.trim_start().trim_start_matches('=').trim();
            if rest.contains(']') {
                extract_quoted(rest, &mut out);
            } else {
                in_members = true;
                buf.push_str(rest);
            }
        }
    }
    out
}

fn extract_quoted(s: &str, out: &mut Vec<String>) {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut word = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                word.push(c2);
            }
            if !word.is_empty() {
                out.push(word);
            }
        }
    }
}

fn resolve_member_paths(repo_root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pat in patterns {
        if pat.ends_with("/*") {
            let dir = repo_root.join(pat.trim_end_matches("/*"));
            if dir.is_dir() {
                if let Ok(rd) = fs::read_dir(&dir) {
                    for ent in rd.flatten() {
                        let p = ent.path();
                        if p.is_dir() && p.join("Cargo.toml").is_file() {
                            out.push(p);
                        }
                    }
                }
            }
        } else {
            let p = repo_root.join(pat);
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out
}

/// Top-3 first-party packages in a TS monorepo. Scans `packages/` and
/// `apps/`, ranks by `.ts` / `.tsx` LOC.
fn ts_monorepo_members(repo_root: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for sub in ["packages", "apps", "libs"] {
        let dir = repo_root.join(sub);
        if dir.is_dir() {
            if let Ok(rd) = fs::read_dir(&dir) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.is_dir() && p.join("package.json").is_file() {
                        candidates.push(p);
                    }
                }
            }
        }
    }
    let mut ranked: Vec<(PathBuf, usize)> = candidates
        .into_iter()
        .map(|p| {
            let loc = scan_loc(&p.join("src"));
            (p, loc)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(3)
        .map(|(p, _)| relative_to(repo_root, &p))
        .collect()
}

/// Top-3 Go module dirs in a `go.work` workspace.
fn go_workspace_members(repo_root: &Path) -> Vec<PathBuf> {
    let body = match fs::read_to_string(repo_root.join("go.work")) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut members: Vec<PathBuf> = Vec::new();
    let mut in_use = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("use (") {
            in_use = true;
            continue;
        }
        if in_use {
            if line == ")" {
                in_use = false;
                continue;
            }
            if !line.is_empty() && !line.starts_with("//") {
                members.push(repo_root.join(line));
            }
        } else if let Some(rest) = line.strip_prefix("use ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                members.push(repo_root.join(rest));
            }
        }
    }
    let mut ranked: Vec<(PathBuf, usize)> = members
        .into_iter()
        .filter(|p| p.is_dir())
        .map(|p| {
            let loc = scan_loc(&p);
            (p, loc)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(3)
        .map(|(p, _)| relative_to(repo_root, &p))
        .collect()
}

// -- single-repo detection ----------------------------------------------

fn is_single_repo(repo_root: &Path) -> bool {
    repo_root.join("Cargo.toml").is_file()
        || repo_root.join("package.json").is_file()
        || repo_root.join("pyproject.toml").is_file()
        || repo_root.join("setup.py").is_file()
        || repo_root.join("requirements.txt").is_file()
        || repo_root.join("go.mod").is_file()
        || repo_root.join("pom.xml").is_file()
        || repo_root.join("build.gradle").is_file()
        || repo_root.join("build.gradle.kts").is_file()
}

/// `["src", "tests"]` style default — only emit subdirs that actually
/// exist on disk so the skill never writes a `scope:` pointing to a
/// missing path. Falls back to `["src"]` if `tests/` is absent, or
/// `[]` if neither exists (rare — most single-repo languages ship a
/// `src/`).
fn single_repo_scope(repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for cand in ["src", "lib", "tests"] {
        if repo_root.join(cand).is_dir() {
            out.push(PathBuf::from(cand));
        }
    }
    out
}

// -- docs-only / scripts-only -------------------------------------------

fn is_docs_only(repo_root: &Path) -> bool {
    // Has `.md` files AND no source dir AND no manifest.
    let mut has_md = false;
    let mut has_source_signal = false;
    if let Ok(rd) = fs::read_dir(repo_root) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let n = name.to_string_lossy().to_string();
            if n.starts_with('.') {
                continue;
            }
            if n.ends_with(".md") {
                has_md = true;
            }
            if n == "src" || n == "lib" || n == "crates" || n == "packages" {
                has_source_signal = true;
            }
        }
    }
    let has_docs_dir = repo_root.join("docs").is_dir();
    (has_md || has_docs_dir) && !has_source_signal
}

fn docs_only_scope(repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if repo_root.join("docs").is_dir() {
        out.push(PathBuf::from("docs"));
    }
    out
}

fn is_scripts_only(repo_root: &Path) -> bool {
    let mut has_script = false;
    if let Ok(rd) = fs::read_dir(repo_root) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let n = name.to_string_lossy().to_string();
            if n.starts_with('.') {
                continue;
            }
            if n.ends_with(".sh") || (n.ends_with(".py") && n != "setup.py") {
                has_script = true;
            }
        }
    }
    has_script
}

// -- language detection -------------------------------------------------

fn detect_languages_and_tests(repo_root: &Path) -> (Vec<Language>, bool) {
    // Coarse: peek at first 200 entries to detect predominant extensions.
    // We do *not* recursively count files — `walkdir` is overkill for a
    // skill bootstrap hint.
    let mut count_rs = 0usize;
    let mut count_ts = 0usize;
    let mut count_js = 0usize;
    let mut count_py = 0usize;
    let mut count_go = 0usize;
    let mut count_java = 0usize;
    let mut has_tests = false;
    let mut roots_to_scan: Vec<PathBuf> = vec![repo_root.to_path_buf()];
    for sub in [
        "src", "crates", "packages", "lib", "tests", "test", "app", "apps", "internal", "cmd",
        "libs",
    ] {
        let p = repo_root.join(sub);
        if p.is_dir() {
            roots_to_scan.push(p.clone());
            // For monorepo subdirs (crates/ packages/ apps/ libs/),
            // also peek into each member's `src/` so the language
            // tally reflects what lives in the wired-up workspace.
            if matches!(sub, "crates" | "packages" | "apps" | "libs") {
                if let Ok(rd) = fs::read_dir(&p) {
                    for ent in rd.flatten().take(50) {
                        let member = ent.path();
                        if member.is_dir() {
                            roots_to_scan.push(member.clone());
                            let member_src = member.join("src");
                            if member_src.is_dir() {
                                roots_to_scan.push(member_src);
                            }
                        }
                    }
                }
            }
        }
    }
    for r in roots_to_scan {
        if let Ok(rd) = fs::read_dir(&r) {
            for ent in rd.flatten().take(200) {
                let name = ent.file_name();
                let n = name.to_string_lossy().to_string();
                if n == "tests" || n == "test" || n == "__tests__" {
                    has_tests = true;
                }
                if let Some(ext) = std::path::Path::new(&n).extension() {
                    match ext.to_string_lossy().as_ref() {
                        "rs" => count_rs += 1,
                        "ts" | "tsx" => count_ts += 1,
                        "js" | "mjs" | "cjs" => count_js += 1,
                        "py" => count_py += 1,
                        "go" => count_go += 1,
                        "java" | "kt" => count_java += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    // Manifest-implied language signals — covers projects whose top-
    // level dir has manifest-only and source under nested folders.
    if repo_root.join("Cargo.toml").is_file() {
        count_rs += 1;
    }
    if repo_root.join("go.mod").is_file() || repo_root.join("go.work").is_file() {
        count_go += 1;
    }
    if repo_root.join("tsconfig.json").is_file() {
        count_ts += 1;
    }
    if repo_root.join("pyproject.toml").is_file() || repo_root.join("setup.py").is_file() {
        count_py += 1;
    }
    if repo_root.join("pom.xml").is_file()
        || repo_root.join("build.gradle").is_file()
        || repo_root.join("build.gradle.kts").is_file()
    {
        count_java += 1;
    }

    let mut ranked: Vec<(Language, usize)> = vec![
        (Language::Rust, count_rs),
        (Language::TypeScript, count_ts),
        (Language::JavaScript, count_js),
        (Language::Python, count_py),
        (Language::Go, count_go),
        (Language::Java, count_java),
    ];
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let langs: Vec<Language> = ranked
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(l, _)| l)
        .take(3)
        .collect();
    (langs, has_tests)
}

// -- LOC scanner (shallow, capped) --------------------------------------

/// Tally bytes (rounded to a coarse "lines" proxy) under `dir`, capped
/// at the first 500 entries per directory and 2 levels deep. Cheap
/// enough for an interactive `ccteam probe-project --json` even on
/// 10k-file monorepos.
fn scan_loc(dir: &Path) -> usize {
    fn walk(dir: &Path, depth: usize, acc: &mut usize) {
        if depth > 2 || *acc > 1_000_000 {
            return;
        }
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for ent in rd.flatten().take(500) {
            let p = ent.path();
            if let Ok(md) = ent.metadata() {
                if md.is_file() {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if matches!(
                        ext.as_str(),
                        "rs" | "ts" | "tsx" | "js" | "mjs" | "cjs" | "py" | "go" | "java" | "kt"
                    ) {
                        *acc = acc.saturating_add(md.len() as usize / 40);
                    }
                } else if md.is_dir() {
                    let name = ent.file_name();
                    let n = name.to_string_lossy().to_string();
                    if n == "target" || n == "node_modules" || n == ".git" || n.starts_with('.') {
                        continue;
                    }
                    walk(&p, depth + 1, acc);
                }
            }
        }
    }
    let mut acc = 0usize;
    walk(dir, 0, &mut acc);
    acc
}

fn relative_to(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map(PathBuf::from)
        .unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn empty_dir_probes_to_empty() {
        let td = tempdir().unwrap();
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Empty);
        assert!(p.languages.is_empty());
        assert!(p.probable_scope.is_empty());
    }

    #[test]
    fn docs_only_dir() {
        let td = tempdir().unwrap();
        touch(&td.path().join("README.md"));
        touch(&td.path().join("CONTRIBUTING.md"));
        fs::create_dir_all(td.path().join("docs")).unwrap();
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::DocsOnly);
        assert_eq!(p.probable_scope, vec![PathBuf::from("docs")]);
    }

    #[test]
    fn scripts_only_dir() {
        let td = tempdir().unwrap();
        touch(&td.path().join("deploy.sh"));
        touch(&td.path().join("cleanup.py"));
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::ScriptsOnly);
        assert!(p.probable_scope.is_empty());
    }

    #[test]
    fn single_repo_rust() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        );
        fs::create_dir_all(td.path().join("src")).unwrap();
        touch(&td.path().join("src/main.rs"));
        fs::create_dir_all(td.path().join("tests")).unwrap();
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::SingleRepo);
        assert!(p.languages.contains(&Language::Rust));
        assert_eq!(
            p.probable_scope,
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
        assert!(p.has_tests);
    }

    #[test]
    fn single_repo_python() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\n",
        );
        fs::create_dir_all(td.path().join("src")).unwrap();
        touch(&td.path().join("src/foo.py"));
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::SingleRepo);
        assert!(p.languages.contains(&Language::Python));
        assert_eq!(p.probable_scope, vec![PathBuf::from("src")]);
    }

    #[test]
    fn monorepo_rust_workspace_glob() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        );
        for c in ["alpha", "beta", "gamma"] {
            write(
                &td.path().join(format!("crates/{c}/Cargo.toml")),
                "[package]\nname = \"x\"\nversion = \"0.1\"\n",
            );
            // Synthesize LOC: alpha=largest, beta=mid, gamma=smallest.
            let body = match c {
                "alpha" => "a".repeat(8000),
                "beta" => "a".repeat(4000),
                _ => "a".repeat(1000),
            };
            write(&td.path().join(format!("crates/{c}/src/lib.rs")), &body);
        }
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Monorepo);
        assert!(p.languages.contains(&Language::Rust));
        // Top-3 by LOC, alpha first.
        assert_eq!(p.probable_scope.len(), 3);
        assert_eq!(p.probable_scope[0], PathBuf::from("crates/alpha"));
    }

    #[test]
    fn monorepo_rust_workspace_explicit_members() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"foo\", \"bar\"]\n",
        );
        for c in ["foo", "bar"] {
            write(
                &td.path().join(format!("{c}/Cargo.toml")),
                "[package]\nname = \"x\"\nversion = \"0.1\"\n",
            );
            write(
                &td.path().join(format!("{c}/src/lib.rs")),
                &"a".repeat(2000),
            );
        }
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Monorepo);
        assert!(p.probable_scope.iter().any(|p| p == &PathBuf::from("foo")));
        assert!(p.probable_scope.iter().any(|p| p == &PathBuf::from("bar")));
    }

    #[test]
    fn monorepo_caps_at_three_members() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        );
        for c in ["c1", "c2", "c3", "c4", "c5"] {
            write(
                &td.path().join(format!("crates/{c}/Cargo.toml")),
                "[package]\nname = \"x\"\nversion = \"0.1\"\n",
            );
            write(
                &td.path().join(format!("crates/{c}/src/lib.rs")),
                "fn x(){}",
            );
        }
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Monorepo);
        assert_eq!(p.probable_scope.len(), 3, "scope must cap at top-3");
    }

    #[test]
    fn monorepo_pnpm_workspace() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        );
        for c in ["web", "api"] {
            write(
                &td.path().join(format!("packages/{c}/package.json")),
                "{\"name\":\"x\"}",
            );
            write(
                &td.path().join(format!("packages/{c}/src/index.ts")),
                "export {}",
            );
        }
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Monorepo);
        assert!(p.languages.contains(&Language::TypeScript));
        assert!(p.probable_scope.iter().any(|p| p.starts_with("packages/")));
    }

    #[test]
    fn monorepo_go_workspace() {
        let td = tempdir().unwrap();
        write(
            &td.path().join("go.work"),
            "go 1.21\n\nuse (\n  ./svc-a\n  ./svc-b\n)\n",
        );
        for c in ["svc-a", "svc-b"] {
            write(
                &td.path().join(format!("{c}/go.mod")),
                &format!("module example.com/{c}\n"),
            );
            write(&td.path().join(format!("{c}/main.go")), "package main");
        }
        let p = probe(td.path());
        assert_eq!(p.kind, ProjectKind::Monorepo);
        assert!(p.languages.contains(&Language::Go));
    }

    #[test]
    fn probe_kind_as_str_matches_wire() {
        assert_eq!(ProjectKind::Monorepo.as_str(), "monorepo");
        assert_eq!(ProjectKind::SingleRepo.as_str(), "single-repo");
        assert_eq!(ProjectKind::DocsOnly.as_str(), "docs-only");
        assert_eq!(ProjectKind::ScriptsOnly.as_str(), "scripts-only");
        assert_eq!(ProjectKind::Empty.as_str(), "empty");
    }

    #[test]
    fn language_as_str_matches_wire() {
        assert_eq!(Language::Rust.as_str(), "rust");
        assert_eq!(Language::TypeScript.as_str(), "typescript");
        assert_eq!(Language::Go.as_str(), "go");
    }
}
