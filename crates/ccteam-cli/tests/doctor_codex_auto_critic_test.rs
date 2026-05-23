//! V0.6.5 F155 — `ccteam doctor --check-codex-auto-critic` end-to-end
//! tests. This flag is the deterministic gate the `ccteam-creator`
//! Phase 3.5 skill consults before injecting `executor: codex` into a
//! generated `workflow.yaml`. The gate must:
//!
//! - exit 0 when codex is present and a one-shot `codex exec --json`
//!   probe emits a well-formed `turn.completed` JSONL frame
//!   (`available: true` on stdout);
//! - exit 2 when the codex binary is missing / `--version` fails /
//!   `exec` errors out (`available: false`);
//! - exit 3 when `codex exec --json` exits 0 but the stdout doesn't
//!   carry a `turn.completed` event — the skill must NOT inject
//!   `executor: codex` until the install is fixed (`probe: malformed`).
//!
//! All tests inject a fake codex script via `CCTEAM_CODEX_BIN` so they
//! don't depend on the real codex CLI being installed on the host.

use serde_json::Value;
use std::io::Write;
use std::process::Command;

/// Build a fake codex script. `version_line` is what `<bin> --version`
/// echoes; `exec_emits` are the JSONL lines the script writes when
/// called as `<bin> exec --json --skip-git-repo-check <prompt>`. Pass
/// `None` for `exec_emits` to make the exec call exit 1 (simulates
/// auth failure / quota / etc).
fn fake_codex_script(
    version_line: &str,
    exec_emits: Option<&[&str]>,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let codex_path = dir.path().join("codex");
    let mut body = String::from("#!/usr/bin/env bash\n");
    body.push_str("# fake codex CLI for F155 doctor --check-codex-auto-critic tests\n");
    body.push_str("if [ \"$1\" = \"--version\" ]; then\n");
    body.push_str(&format!("  echo {version_line:?}\n"));
    body.push_str("  exit 0\n");
    body.push_str("fi\n");
    body.push_str("if [ \"$1\" = \"exec\" ]; then\n");
    match exec_emits {
        Some(lines) => {
            body.push_str("  cat <<'EOF'\n");
            for l in lines {
                body.push_str(l);
                body.push('\n');
            }
            body.push_str("EOF\n");
            body.push_str("  exit 0\n");
        }
        None => {
            body.push_str("  echo 'fake codex exec: auth failed' >&2\n");
            body.push_str("  exit 1\n");
        }
    }
    body.push_str("fi\n");
    body.push_str("echo \"fake codex: unknown arg $1\" >&2\n");
    body.push_str("exit 2\n");
    {
        let mut f = std::fs::File::create(&codex_path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&codex_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&codex_path, perms).unwrap();
    (dir, codex_path)
}

fn run_doctor(codex_bin: Option<&std::path::Path>) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let mut cmd = Command::new(bin);
    cmd.arg("doctor").arg("--check-codex-auto-critic");
    // env_clear too aggressive (would lose HOME etc); just override
    // CCTEAM_CODEX_BIN. If the caller passes None, remove the env so
    // the gate falls back to bare `codex` on PATH — but we also strip
    // PATH to `/usr/bin:/bin` so the bare lookup deterministically
    // fails on test hosts without a real codex install.
    match codex_bin {
        Some(p) => {
            cmd.env("CCTEAM_CODEX_BIN", p);
        }
        None => {
            cmd.env_remove("CCTEAM_CODEX_BIN");
            // Force a PATH that almost certainly has no `codex` binary
            // on a fresh CI host. (`/bin` may contain shell builtins
            // but never `codex`.)
            cmd.env("PATH", "/usr/bin:/bin");
        }
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// Extract the trailing JSON line from the stdout report — the
/// header (line 1+2) is for human operators; programmatic callers
/// parse the last non-empty line.
fn parse_json_tail(stdout: &str) -> Value {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("at least one stdout line");
    serde_json::from_str(line).expect("trailing line is JSON")
}

#[test]
fn exit_0_when_codex_emits_well_formed_turn_completed() {
    let (_dir, path) = fake_codex_script(
        "codex 0.131.0",
        Some(&[
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"i-1","type":"agent_message","text":"OK"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":4,"output_tokens":1}}"#,
        ]),
    );
    let (stdout, stderr, code) = run_doctor(Some(&path));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let v = parse_json_tail(&stdout);
    assert_eq!(v["available"], Value::Bool(true), "{v}");
    assert_eq!(v["probe"], Value::String("ok".into()), "{v}");
    assert_eq!(v["exit_code"], Value::Number(0.into()), "{v}");
    assert!(v["version"].as_str().unwrap().contains("0.131"), "{v}",);
}

#[test]
fn exit_2_when_codex_binary_missing() {
    // Point CCTEAM_CODEX_BIN at a non-existent path so spawn errors.
    let nonexistent = std::path::Path::new("/tmp/ccteam-f155-nonexistent-codex-binary-xyz");
    let (stdout, stderr, code) = run_doctor(Some(nonexistent));
    assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
    let v = parse_json_tail(&stdout);
    assert_eq!(v["available"], Value::Bool(false), "{v}");
    assert_eq!(v["exit_code"], Value::Number(2.into()), "{v}");
    assert!(v["reason"].as_str().unwrap().contains("spawn"), "{v}",);
}

#[test]
fn exit_2_when_codex_version_probe_fails() {
    // `--version` exits non-zero — simulate broken install.
    let dir = tempfile::tempdir().unwrap();
    let codex_path = dir.path().join("codex");
    {
        let mut f = std::fs::File::create(&codex_path).unwrap();
        f.write_all(
            b"#!/usr/bin/env bash\n\
              echo 'codex: license expired' >&2\n\
              exit 1\n",
        )
        .unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&codex_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&codex_path, perms).unwrap();
    let (stdout, _stderr, code) = run_doctor(Some(&codex_path));
    assert_eq!(code, 2, "stdout: {stdout}");
    let v = parse_json_tail(&stdout);
    assert_eq!(v["available"], Value::Bool(false), "{v}");
    assert!(v["reason"].as_str().unwrap().contains("--version"), "{v}",);
}

#[test]
fn exit_2_when_codex_exec_probe_fails() {
    // `--version` succeeds but `exec` exits 1 — typical for an
    // unauthenticated codex install.
    let (_dir, path) = fake_codex_script("codex 0.131.0", None);
    let (stdout, _stderr, code) = run_doctor(Some(&path));
    assert_eq!(code, 2, "stdout: {stdout}");
    let v = parse_json_tail(&stdout);
    assert_eq!(v["available"], Value::Bool(false), "{v}");
    assert!(v["reason"].as_str().unwrap().contains("codex exec"), "{v}",);
}

#[test]
fn exit_3_when_codex_exec_output_is_malformed() {
    // `exec` exits 0 but the stdout has no `turn.completed` event —
    // either output schema drifted (V0.6.3 F144 forward-compat
    // shielded the adapter, but the auto-critic gate needs a known-
    // good shape) or codex emitted only an error message. The gate
    // returns 3 so the skill silently falls back to `vendor: claude`
    // instead of injecting a broken `executor: codex` field.
    let (_dir, path) = fake_codex_script(
        "codex 0.131.0",
        Some(&[
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            r#"{"type":"turn.started"}"#,
            // No turn.completed — malformed stream.
        ]),
    );
    let (stdout, _stderr, code) = run_doctor(Some(&path));
    assert_eq!(code, 3, "stdout: {stdout}");
    let v = parse_json_tail(&stdout);
    assert_eq!(v["available"], Value::Bool(true), "{v}");
    assert_eq!(v["probe"], Value::String("malformed".into()), "{v}");
    assert_eq!(v["exit_code"], Value::Number(3.into()), "{v}");
}

#[test]
fn report_header_includes_binary_path_and_finding_marker() {
    let (_dir, path) = fake_codex_script(
        "codex 0.131.0",
        Some(&[r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#]),
    );
    let (stdout, _stderr, code) = run_doctor(Some(&path));
    assert_eq!(code, 0);
    assert!(
        stdout.contains("V0.6.5 F155"),
        "header must carry F155 marker for traceability: {stdout}",
    );
    assert!(
        stdout.contains(&path.to_string_lossy().to_string()),
        "header must include resolved codex binary path: {stdout}",
    );
}
