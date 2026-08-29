//! CLI-ENVTEST-1 — a guard test that keeps `#[cfg(test)]` code in this crate
//! out of the process environment.
//!
//! `ccteam-cli` ships a single binary and no lib target, so EVERY
//! `#[cfg(test)]` module under `src/` compiles into one test binary and runs
//! in one process. A test that `set_var`s `HOME` / `CCTEAM_HOME` /
//! `CCTEAM_MUX_BACKEND` and restores it afterwards therefore mutates shared
//! state under every sibling test that resolves a root from the environment —
//! the loser writes its files into a stranger's tempdir. That is not a
//! hypothetical: it turned CI's deterministic-baseline job red twice in one
//! day on commits that only touched a backlog file
//! (`web_chat_newproject_scaffolds_registers_and_cd_works`, reading a
//! `config.yaml` some other test's restore had moved out from under it).
//!
//! The rule (AGENTS.md §六) is: take the root/backend as an argument
//! (`_in(root)` / an injected backend), and if a case genuinely needs the
//! environment, move it to `crates/ccteam-cli/tests/*.rs`, where Cargo gives
//! it its own process. This test enforces that rule mechanically so the next
//! `set_var` never has to be diagnosed as a flake again.
//!
//! Scope note: only `#[cfg(test)]` regions are scanned. Production code in
//! this crate legitimately sets env (`main.rs` pins `CCTEAM_HOME` and
//! `RMUX_SDK_DAEMON_BINARY` for child processes) — that is a process-startup
//! decision, not a test racing its siblings.

use std::path::{Path, PathBuf};

/// Byte range of one `#[cfg(test)]` item body, `[start, end)`.
type Region = (usize, usize);

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
                if b == b'"' && src[i + 1..].starts_with(&"#".repeat(raw_hashes)) {
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
                } else if (b == b'r' || b == b'b')
                    && src[i..].starts_with('r')
                    && src[i + 1..].starts_with('#')
                {
                    let hashes = src[i + 1..].bytes().take_while(|c| *c == b'#').count();
                    if src[i + 1 + hashes..].starts_with('"') {
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
                } else if src[i..].starts_with("#[cfg(test)]") {
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

/// Every `set_var` / `remove_var` call site that sits inside a
/// `#[cfg(test)]` region, as `path:line`.
fn env_mutations_in_test_regions(src_dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    for file in rust_sources_under(src_dir) {
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
                let shown = file
                    .strip_prefix(src_dir.parent().and_then(Path::parent).unwrap_or(src_dir))
                    .unwrap_or(&file);
                hits.push(format!("{}:{line}", shown.display()));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// The guard itself. See the module docs for why this crate cannot
    /// tolerate env mutation in `#[cfg(test)]` code.
    #[test]
    fn no_cfg_test_code_in_this_crate_mutates_the_process_environment() {
        let hits = env_mutations_in_test_regions(&src_dir());
        assert!(
            hits.is_empty(),
            "`#[cfg(test)]` code in ccteam-cli must not set/remove env vars — \
             this crate has no lib target, so all of src/ shares ONE test \
             process and the mutation races every sibling test that resolves \
             a root from env (CLI-ENVTEST-1). Take the root/backend as an \
             argument, or move the case to crates/ccteam-cli/tests/*.rs where \
             it gets its own process. Offenders: {hits:?}"
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
            env_mutations_in_test_regions(&src).is_empty(),
            "production set_var, and #[cfg(test)] inside a comment or string, must not trip it"
        );

        std::fs::write(
            src.join("nested").join("dirty.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn bad() {\n        \
             std::env::set_var(\"CCTEAM_HOME\", \"/tmp/x\");\n    }\n}\n",
        )
        .unwrap();
        let hits = env_mutations_in_test_regions(&src);
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
