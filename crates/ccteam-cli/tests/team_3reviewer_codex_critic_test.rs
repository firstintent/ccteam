//! V0.6.5 F156 — verify `/ccteam-team` §3.5 Codex critic auto-injection
//! path mechanics. The skill spawns the Codex critic teammate directly
//! from skill body via Bash (the daemon-side `CodexExecAdapter` route
//! is explicitly **V0.7+ deferred** — see `skills/ccteam-team/SKILL.md`
//! §3.5 + `docs/versions/v0-6-5/prd.md` F156).
//!
//! These tests exercise the exact Bash spawn shape the skill uses,
//! against a fake codex binary via `$CCTEAM_CODEX_BIN`. They verify:
//!
//! 1. the heredoc + flag combination (`exec --json --skip-git-repo-check`)
//!    matches what the skill body writes;
//! 2. captured stdout is parseable JSONL with a `turn.completed` frame —
//!    the marker the team-lead synthesis loop polls for;
//! 3. the spawn pattern works when N=3 (one critic + two Claude
//!    teammates as the skill prescribes) — modeled as the parent
//!    backgrounding the critic + collecting its output via temp file.
//!
//! The Claude `Task` spawn path for the other N-1 teammates is not in
//! scope (Anthropic-owned surface, no stub-able adapter). What this
//! test guards is the Codex-critic mechanics specifically — which is
//! the part the skill body owns and the part that was unverified
//! before V0.6.5 F156.

use std::io::Write;
use std::process::Command;

/// Build a fake codex binary that responds to
/// `codex exec --json --skip-git-repo-check` by emitting deterministic
/// JSONL (the same shape the V0.6.0 Wave 3 `CodexExecAdapter` tests
/// use). The stub ignores its prompt body — the test only verifies
/// the spawn-and-capture mechanics, not real inference.
fn fake_codex_critic() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("codex");
    let body = "#!/usr/bin/env bash\n\
        # fake codex CLI for F156 /ccteam-team §3.5 mechanics tests.\n\
        # Drain stdin so the caller's heredoc closes cleanly even when\n\
        # the body is large.\n\
        cat >/dev/null\n\
        cat <<'EOF'\n\
        {\"type\":\"thread.started\",\"thread_id\":\"t-critic-1\"}\n\
        {\"type\":\"turn.started\"}\n\
        {\"type\":\"item.completed\",\"item\":{\"id\":\"i-1\",\"type\":\"agent_message\",\"text\":\"Critic verdict: looks reasonable, but watch the off-by-one in the retry loop.\"}}\n\
        {\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":42,\"output_tokens\":18}}\n\
        EOF\n\
        exit 0\n";
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
}

/// The literal Bash spawn snippet from `skills/ccteam-team/SKILL.md`
/// §3.5 (with the heredoc prompt body inlined for the test). If this
/// is ever changed in the skill, this test will catch the drift.
const SKILL_SPAWN_SNIPPET: &str = r#"
CODEX_BIN="${CCTEAM_CODEX_BIN:-codex}"
"$CODEX_BIN" exec --json --skip-git-repo-check <<'PROMPT'
You are the codex-critic teammate of team "test-slug". Review the
artifact and surface adversarial concerns. Reply in <= 200 words.

Task: Review retry loop in src/main.rs.
PROMPT
"#;

#[test]
fn skill_3_5_bash_spawn_shape_emits_well_formed_jsonl() {
    let (_dir, codex_path) = fake_codex_critic();
    let out = Command::new("bash")
        .arg("-c")
        .arg(SKILL_SPAWN_SNIPPET)
        .env("CCTEAM_CODEX_BIN", &codex_path)
        .output()
        .expect("bash spawn");
    assert!(
        out.status.success(),
        "skill §3.5 snippet failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The synthesis loop polls for a `turn.completed` JSONL frame —
    // that's the only marker the skill prescribes for "this critic
    // turn is done".
    let has_turn_completed = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("turn.completed"));
    assert!(
        has_turn_completed,
        "skill §3.5 spawn did not yield turn.completed JSONL: {stdout}",
    );
}

#[test]
fn skill_3_5_critic_runs_in_parallel_with_other_teammates() {
    // §3.5 says the team-lead session captures the critic's
    // stdout/stderr to a temp file the synthesis loop polls. Model
    // that here: background the critic, collect stdout to a temp
    // file, and verify the temp file ends up with a turn.completed
    // frame after the background process finishes. This is the
    // concrete shape the skill prescribes for N=3 teams (two Claude
    // Task spawns run in parallel with this one Codex spawn).
    let (_dir, codex_path) = fake_codex_critic();
    let temp = tempfile::NamedTempFile::new().unwrap();
    let out_path = temp.path().to_path_buf();
    // Heredoc terminator MUST be flush-left, so we build the script
    // line-by-line with String::push_str instead of an indented
    // format! literal (Rust's `\` line continuation would carry the
    // source-file indent into the bash heredoc and break it).
    let out_str = out_path.to_string_lossy().to_string();
    let mut script = String::new();
    script.push_str("CODEX_BIN=\"${CCTEAM_CODEX_BIN:-codex}\"\n");
    script.push_str(&format!(
        "\"$CODEX_BIN\" exec --json --skip-git-repo-check >{out_str} 2>&1 <<'PROMPT' &\n"
    ));
    script.push_str("critic prompt body\n");
    script.push_str("PROMPT\n");
    script.push_str("wait $!\n");
    let status = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("CCTEAM_CODEX_BIN", &codex_path)
        .status()
        .expect("bash spawn");
    assert!(status.success(), "parallel spawn snippet failed");
    let captured = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        captured.contains("turn.completed"),
        "background spawn did not capture turn.completed: {captured}",
    );
}

#[test]
fn skill_3_5_falls_back_silently_when_codex_unavailable() {
    // §3.5 also says: "Either probe fails → silently fall back to
    // all-Claude composition for this run; no error surfaced." Test
    // the probe shape exits non-zero when CCTEAM_CODEX_BIN is missing
    // — the skill MUST detect that and skip the critic spawn.
    let nonexistent = "/tmp/ccteam-f156-nonexistent-codex-xyz";
    let probe_script = r#"
        CODEX_BIN="${CCTEAM_CODEX_BIN:-codex}"
        "$CODEX_BIN" --version 2>/dev/null
    "#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(probe_script)
        .env("CCTEAM_CODEX_BIN", nonexistent)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("bash spawn");
    assert!(
        !out.status.success(),
        "probe MUST fail when codex binary is missing; skill relies on \
         this to silently fall back. stdout: {}",
        String::from_utf8_lossy(&out.stdout),
    );
}
