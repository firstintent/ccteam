//! V0.6.6 F168 — regression gate against silent `// TODO` /
//! `// FIXME` / `// HACK` / `TODO(...)` markers in production Rust
//! sources.
//!
//! Invariant: every surviving marker must carry an explicit
//! `V0.<N>` deferred-with-justification tag (either inline or in the
//! immediately adjacent comment lines), so future grep sweeps trivially
//! distinguish "deferred-with-reason" from "forgotten WIP". F168
//! originally delivered six `TODO(V0.7-<anchor>)` tags; V0.6.8 retired
//! the `chat-handle` anchor (the AgentSpec / BotRegistration /
//! build_handle_map schema landed) so the count is now five.
//! Sister-finding sites owned by F173 / F169 / F170 cover the
//! remaining markers — see `docs/dev-coupling-audit.md` for the index.
//!
//! Allowed escape hatch: a marker line that mentions any
//! `V0.<N>` (N ≥ 7) token in its own line or in the ±3 line window
//! around it counts as "deferred-with-justification". Anything else
//! fails the test loud — adding a bare `// TODO` to ship code is a
//! pre-v1.0 anti-pattern per CLAUDE.md §五.

use std::path::PathBuf;

/// Production source roots scanned by the regression gate. Test
/// sources, host-probe fixtures, build-script panics, and template /
/// fixture string-literals are out of scope.
const SCAN_ROOTS: &[&str] = &[
    "crates/ccteam-core/src",
    "crates/ccteam-cli/src",
    "crates/ccteam-cost/src",
    "crates/ccteam-im/src",
    "crates/ccteam-web/src",
];

/// File paths (relative to workspace root) whose `TODO` content is a
/// known false positive — string fixtures, template body, or doc
/// example. F168 audit verified each entry is **not** a code-level
/// WIP marker.
const FILE_ALLOWLIST: &[&str] = &[
    // `HANDOFF_TEMPLATE` body contains "- TODO" bullets as user-facing
    // template text, not Rust comments.
    "crates/ccteam-core/src/handoff.rs",
    // `team.rs` carries `pattern: TODO` inside a test-fixture string
    // literal for hook pattern matching.
    "crates/ccteam-core/src/team.rs",
];

/// Specific `(rel_path, substring)` pairs owned by another V0.6.6
/// sister finding whose PR clears the site. F168 explicitly leaves
/// these untouched to avoid merge conflicts; the test exempts them so
/// F168 lands clean while the sister PRs are in flight. Each entry
/// must be removed once the owning finding merges.
const SISTER_FINDING_ALLOWLIST: &[(&str, &str)] = &[
    // F173 (`v066-w1-codex-critic-ledger`) clears this in the same
    // PR that swaps the Codex arm to `CodexExecAdapter`.
    (
        "crates/ccteam-im/src/daemon.rs",
        "TODO(wave-3 codex-exec-impl)",
    ),
];

#[test]
fn no_silent_todo_in_production_src() {
    let workspace_root = workspace_root();
    let mut offenders: Vec<String> = Vec::new();

    for root in SCAN_ROOTS {
        let root_path = workspace_root.join(root);
        if !root_path.exists() {
            // Crate moved or renamed — fail loud so the allowlist
            // doesn't silently bit-rot.
            panic!("scan root missing: {}", root_path.display());
        }
        walk_rs_files(&root_path, &mut |file_path: &PathBuf| {
            let rel = file_path
                .strip_prefix(&workspace_root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");
            if FILE_ALLOWLIST.iter().any(|allowed| rel == *allowed) {
                return;
            }
            let body = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(_) => return,
            };
            let lines: Vec<&str> = body.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !is_marker_line(line) {
                    continue;
                }
                if has_v0_deferred_anchor(&lines, i) {
                    continue;
                }
                if SISTER_FINDING_ALLOWLIST
                    .iter()
                    .any(|(p, needle)| rel == *p && line.contains(needle))
                {
                    continue;
                }
                offenders.push(format!("{}:{}: {}", rel, i + 1, line.trim()));
            }
        });
    }

    assert!(
        offenders.is_empty(),
        "silent TODO/FIXME/HACK markers found (must carry `V0.<N>` \
         deferred-with-justification tag — see docs/versions/v0-6-6/prd.md §F168):\n{}",
        offenders.join("\n")
    );
}

/// Cross-check that the surviving `TODO(V0.7-<anchor>)` tag set
/// matches expectations — guards against accidental tag removal or
/// duplication. F168 originally delivered six anchors; V0.6.8
/// retired `chat-handle` (the schema landed), so the count is now
/// five.
#[test]
fn f168_v07_deferred_tag_count_is_five() {
    let workspace_root = workspace_root();
    let mut hits: Vec<String> = Vec::new();
    for root in SCAN_ROOTS {
        let root_path = workspace_root.join(root);
        walk_rs_files(&root_path, &mut |file_path: &PathBuf| {
            let rel = file_path
                .strip_prefix(&workspace_root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");
            let body = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(_) => return,
            };
            for (i, line) in body.lines().enumerate() {
                if line.contains("TODO(V0.7-") {
                    hits.push(format!("{}:{}", rel, i + 1));
                }
            }
        });
    }
    assert_eq!(
        hits.len(),
        5,
        "V0.6.8 left exactly 5 V0.7-deferred TODO anchors (F168 minus \
         chat-handle, which V0.6.8 closed); found {}:\n{}",
        hits.len(),
        hits.join("\n")
    );
}

/// Detect `// TODO` / `// FIXME` / `// HACK` / `TODO(` markers as
/// rust comments (leading `//` or inside doc-comment `///` / `//!`).
fn is_marker_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !(trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")) {
        // Not a comment line — skip (TODO inside string literals is
        // not a comment marker).
        return false;
    }
    trimmed.contains("// TODO")
        || trimmed.contains("// FIXME")
        || trimmed.contains("// HACK")
        || trimmed.contains("TODO(")
        || trimmed.contains("FIXME(")
        || trimmed.contains("HACK(")
}

/// Search `lines[i]` and the ±3 line window for any `V0.<N>` token
/// (N ≥ 7) — that counts as deferred-with-justification.
fn has_v0_deferred_anchor(lines: &[&str], i: usize) -> bool {
    let lo = i.saturating_sub(3);
    let hi = (i + 4).min(lines.len());
    for line in lines.iter().take(hi).skip(lo) {
        if line_mentions_v0_future(line) {
            return true;
        }
    }
    false
}

fn line_mentions_v0_future(line: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = b"V0.";
    if bytes.len() < needle.len() + 1 {
        return false;
    }
    let mut idx = 0;
    while idx + needle.len() < bytes.len() {
        if &bytes[idx..idx + needle.len()] == needle {
            let after = bytes[idx + needle.len()];
            if after.is_ascii_digit() {
                let digit = (after - b'0') as u32;
                if digit >= 7 {
                    return true;
                }
            }
        }
        idx += 1;
    }
    false
}

fn walk_rs_files(root: &PathBuf, sink: &mut dyn FnMut(&PathBuf)) {
    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files(&path, sink);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            sink(&path);
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("ccteam-cli crate must sit two levels deep in workspace")
}
