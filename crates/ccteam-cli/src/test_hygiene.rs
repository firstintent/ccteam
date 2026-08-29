//! CLI-ENVTEST-1 — a workspace-wide guard test that keeps `#[cfg(test)]` code
//! in EVERY `crates/*/src/**` out of the process environment.
//!
//! A crate's unit tests all share ONE test process (one per lib target, one
//! per bin target — and `ccteam-cli` has no lib at all, so its whole `src/`
//! is a single binary). A test that `set_var`s `HOME` / `CCTEAM_HOME` /
//! `CLAUDE_CONFIG_HOME` / `CCTEAM_MUX_BACKEND` and restores it afterwards
//! therefore mutates shared state under every sibling test in that process
//! that resolves a root from the environment — the loser writes its files
//! into a stranger's tempdir, or reads a root that has already been restored
//! out from under it. That is not a hypothetical: it turned CI's
//! deterministic-baseline job red twice in one day on commits that only
//! touched a backlog file (`web_chat_newproject_scaffolds_registers_and_cd_works`,
//! reading a `config.yaml` some other test's restore had moved).
//!
//! The rule (AGENTS.md §五) is: take the root as an argument (`_in(root)` /
//! an injected backend), and if a case genuinely exercises env RESOLUTION
//! itself, move it to that crate's `crates/<crate>/tests/*.rs`, where Cargo
//! gives it its own process. This test enforces that rule mechanically so the
//! next `set_var` never has to be diagnosed as a flake again.
//!
//! It lives in `ccteam-cli` because that is the workspace's top-level binary
//! crate and `--bins` puts it inside `make test-baseline`; it deliberately
//! scans the whole workspace rather than only its own crate — one fallback
//! missed usually has siblings (AGENTS.md §四 総纲「同形扫一遍」).
//!
//! Scope note: only `#[cfg(test)]` regions are scanned. Production code
//! legitimately sets env (`ccteam-cli/src/main.rs` pins `CCTEAM_HOME` and
//! `RMUX_SDK_DAEMON_BINARY` for child processes; `ccteam-core`'s
//! `disable_tool_surface_bootstrap_for_tests` is a monotone, never-restored
//! switch) — a process-startup decision is not a test racing its siblings.

use std::path::{Path, PathBuf};

/// Byte range of one `#[cfg(test)]` item body, `[start, end)`.
type Region = (usize, usize);

/// Identifier bytes, so a trailing `r` in `four` cannot be mistaken for the
/// start of a raw string literal.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Lexer state, so `#[cfg(test)]` inside a doc comment and `{` inside a
/// string literal cannot be mistaken for code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    LineComment,
    BlockComment,
    Str,
    Char,
    RawStr,
}

/// Byte ranges covered by a `#[cfg(test)]`-annotated item's braces, plus a
/// per-byte mask of "this byte is real code" (false inside comments and
/// string/char literals).
///
/// Deliberately a small hand-rolled scanner rather than a parser dependency:
/// it only has to answer "is this byte real code inside a test-only item?",
/// and it skips comments, string/char literals and raw strings so neither the
/// `#[cfg(test)]` marker nor the `set_var` hit can come from prose or from a
/// fixture string — this very file quotes both.
fn cfg_test_regions(src: &str) -> (Vec<Region>, Vec<bool>) {
    // Byte operations only: this workspace's sources are full of CJK comments
    // and `…`, and slicing `src` at a byte index that lands mid-character
    // panics (it did, the first time this guard was pointed at the workspace).
    let bytes = src.as_bytes();
    let mut mode = Mode::Code;
    let mut raw_hashes = 0usize;
    let mut depth: i64 = 0;
    // Depths at which a `#[cfg(test)]` item is waiting for its opening brace.
    let mut pending: Vec<usize> = Vec::new();
    // (start_byte, depth_outside_the_item) for regions currently open.
    let mut open: Vec<(usize, i64)> = Vec::new();
    let mut regions: Vec<Region> = Vec::new();
    let mut is_code = vec![false; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if mode == Mode::Code {
            is_code[i] = true;
        }
        match mode {
            Mode::LineComment => {
                if b == b'\n' {
                    mode = Mode::Code;
                }
                i += 1;
            }
            Mode::BlockComment => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    mode = Mode::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            Mode::Str => {
                if b == b'\\' {
                    i += 2;
                } else {
                    if b == b'"' {
                        mode = Mode::Code;
                    }
                    i += 1;
                }
            }
            Mode::Char => {
                if b == b'\\' {
                    i += 2;
                } else {
                    if b == b'\'' {
                        mode = Mode::Code;
                    }
                    i += 1;
                }
            }
            Mode::RawStr => {
                if b == b'"' && bytes[i + 1..].iter().take(raw_hashes).all(|c| *c == b'#') {
                    mode = Mode::Code;
                    i += 1 + raw_hashes;
                } else {
                    i += 1;
                }
            }
            Mode::Code => {
                if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    mode = Mode::LineComment;
                    i += 2;
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    mode = Mode::BlockComment;
                    i += 2;
                } else if b == b'"' {
                    mode = Mode::Str;
                    i += 1;
                } else if b == b'r' && !i.checked_sub(1).is_some_and(|p| is_ident_byte(bytes[p])) {
                    // `r"…"` / `r#"…"#` (and the `br` byte-string forms, whose
                    // `b` is just an identifier byte before this `r`).
                    let hashes = bytes[i + 1..].iter().take_while(|c| **c == b'#').count();
                    if bytes.get(i + 1 + hashes) == Some(&b'"') {
                        mode = Mode::RawStr;
                        raw_hashes = hashes;
                        i += 2 + hashes;
                    } else {
                        i += 1;
                    }
                } else if b == b'\'' && bytes.get(i + 2) == Some(&b'\'') {
                    // A char literal; a lifetime (`'a`) has no closing quote.
                    mode = Mode::Char;
                    i += 1;
                } else if bytes[i..].starts_with(b"#[cfg(test)]") {
                    pending.push(depth as usize);
                    i += "#[cfg(test)]".len();
                } else if b == b'{' {
                    if pending.last() == Some(&(depth as usize)) {
                        pending.pop();
                        open.push((i, depth));
                    }
                    depth += 1;
                    i += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if let Some(&(start, at)) = open.last() {
                        if at == depth {
                            open.pop();
                            regions.push((start, i + 1));
                        }
                    }
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }
    (regions, is_code)
}

fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Every `crates/<crate>/src` directory in the workspace.
fn workspace_crate_src_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(workspace_root.join("crates")) else {
        return out;
    };
    for entry in entries.flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            out.push(src);
        }
    }
    out.sort();
    out
}

/// Every `set_var` / `remove_var` call site under `scan_dir` that sits inside
/// a `#[cfg(test)]` region, reported as `path:line` relative to `display_root`.
fn env_mutations_in_test_regions(scan_dir: &Path, display_root: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    for file in rust_sources_under(scan_dir) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let (regions, is_code) = cfg_test_regions(&text);
        if regions.is_empty() {
            continue;
        }
        for needle in ["set_var", "remove_var"] {
            let mut from = 0usize;
            while let Some(rel) = text[from..].find(needle) {
                let at = from + rel;
                from = at + needle.len();
                if !is_code.get(at).copied().unwrap_or(false) {
                    continue;
                }
                if !regions.iter().any(|(s, e)| at >= *s && at < *e) {
                    continue;
                }
                let line = text[..at].lines().count();
                let shown = file.strip_prefix(display_root).unwrap_or(&file);
                hits.push(format!("{}:{line}", shown.display()));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `crates/ccteam-cli` → the workspace root two levels up.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/<crate> lives two levels under the workspace root")
            .to_path_buf()
    }

    /// The guard itself, over every crate in the workspace. See the module
    /// docs for why unit tests here cannot tolerate env mutation.
    #[test]
    fn no_cfg_test_code_in_the_workspace_mutates_the_process_environment() {
        let root = workspace_root();
        let dirs = workspace_crate_src_dirs(&root);
        assert!(
            dirs.len() >= 6,
            "the scan found almost no crates ({dirs:?}) — a guard that scans \
             nothing passes for the wrong reason"
        );
        let hits: Vec<String> = dirs
            .iter()
            .flat_map(|dir| env_mutations_in_test_regions(dir, &root))
            .collect();
        assert!(
            hits.is_empty(),
            "`#[cfg(test)]` code under crates/*/src must not set/remove env \
             vars — a crate's unit tests share ONE process, so the mutation \
             races every sibling test that resolves a root from env \
             (CLI-ENVTEST-1). Take the root as an argument (`_in(root)`), or \
             move the case to crates/<crate>/tests/<name>_env_test.rs where it \
             gets its own process. Offenders: {hits:?}"
        );
    }

    /// Teeth: the scanner really does find a mutation in a test region, and
    /// really does ignore one in production code, a comment, or a string —
    /// otherwise the guard above would pass by being blind.
    #[test]
    fn the_guard_finds_env_mutation_only_inside_cfg_test_regions() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();

        std::fs::write(
            src.join("clean.rs"),
            "fn main() { std::env::set_var(\"A\", \"1\"); }\n\
             // #[cfg(test)] set_var in a comment\n\
             const S: &str = \"#[cfg(test)] mod x { set_var }\";\n\
             #[cfg(test)]\nmod tests {\n    #[test]\n    fn ok() { assert!(true); }\n}\n",
        )
        .unwrap();
        assert!(
            env_mutations_in_test_regions(&src, dir.path()).is_empty(),
            "production set_var, and #[cfg(test)] inside a comment or string, must not trip it"
        );

        std::fs::write(
            src.join("nested").join("dirty.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn bad() {\n        \
             std::env::set_var(\"CCTEAM_HOME\", \"/tmp/x\");\n    }\n}\n",
        )
        .unwrap();
        let hits = env_mutations_in_test_regions(&src, dir.path());
        assert_eq!(
            hits.len(),
            1,
            "expected exactly the nested offender: {hits:?}"
        );
        assert!(
            hits[0].ends_with(":5"),
            "should point at the call line: {hits:?}"
        );
    }
}
