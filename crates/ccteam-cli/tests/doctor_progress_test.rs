use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

fn fake_vendor(path: &Path) -> PathBuf {
    let binary = path.join("fake-vendor.sh");
    std::fs::write(&binary, "#!/bin/sh\necho 'vendor 9.9.9'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
    }
    binary
}

fn doctor_command(root: &Path, repair: bool) -> Command {
    let home = root.join("home");
    let ccteam_home = root.join("ccteam-home");
    let vendor = fake_vendor(root);
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ccteam"));
    command.arg("doctor");
    if repair {
        command.arg("--repair-progress");
    }
    command
        .env("HOME", &home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", root.join("projects"))
        .env("CLAUDE_CONFIG_HOME", home.join(".claude"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("KIMI_CODE_HOME", home.join(".kimi-code"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("CCTEAM_CLAUDE_BIN", &vendor)
        .env("CCTEAM_CODEX_BIN", &vendor)
        .env("CCTEAM_GROK_BIN", &vendor)
        .env("CCTEAM_OPENCODE_BIN", &vendor)
        .env("CCTEAM_KIMI_BIN", &vendor)
        .env("CCTEAM_PI_BIN", &vendor)
        .env("CCTEAM_DSH_BIN", &vendor)
        .env("CCTEAM_PROGRESS_ROTATE_BYTES", "1024")
        .env("NO_COLOR", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("MOONSHOT_API_KEY")
        .env_remove("DEEPSEEK_API_KEY");
    command
}

fn write_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let progress = root.join("ccteam-home/state/progress");
    std::fs::create_dir_all(&progress).unwrap();
    let active = progress.join("demo.jsonl");
    let archive = progress.join("demo.1.jsonl");

    let mut active_file = std::fs::File::create(&active).unwrap();
    serde_json::to_writer(
        &mut active_file,
        &json!({"event": "flood_kind", "payload": "x".repeat(900)}),
    )
    .unwrap();
    active_file.write_all(b"\n").unwrap();
    serde_json::to_writer(
        &mut active_file,
        &json!({"event": "small_kind", "payload": "ok"}),
    )
    .unwrap();
    active_file.write_all(b"\n").unwrap();
    active_file
        .write_all("{\"event\":\"torn\",\"payload\":\"配置".as_bytes())
        .unwrap();
    active_file.write_all(&[0xff, 0xfe]).unwrap();
    active_file
        .write_all(b"{\"event\":\"lost_next\"}\n")
        .unwrap();

    std::fs::write(
        &archive,
        b"{\"event\":\"archive_kind\",\"cost_usd\":1.0}\n{broken archive}\n",
    )
    .unwrap();
    std::fs::write(
        progress.join("demo.checkpoint.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "rotation_sequence": 9,
            "event_count": 99,
            "corrupt_line_count": 0,
            "cost_total_usd": 9.0,
            "cost_total_by_vendor": {"claude": 9.0},
            "cost_total_by_sid": {"s1": 9.0},
            "coverage": {
                "byte_size": 1,
                "first_line_sha256": "stale"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    (active, archive)
}

fn backup_count(progress: &Path) -> usize {
    std::fs::read_dir(progress)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak-"))
        .count()
}

#[test]
fn doctor_reports_and_repairs_progress_damage_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let (active, archive) = write_fixture(temp.path());
    let progress = active.parent().unwrap();

    let first = doctor_command(temp.path(), false).output().unwrap();
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("progress\n"), "{stdout}");
    assert!(stdout.contains("SIZE WARNING"), "{stdout}");
    assert!(
        stdout.contains("active corrupt=1 first_offset="),
        "{stdout}"
    );
    assert!(
        stdout.contains("archive=") && stdout.contains("corrupt=1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("TOP KINDS BY BYTES: flood_kind="),
        "{stdout}"
    );
    assert!(stdout.contains("checkpoint=INCONSISTENT"), "{stdout}");
    assert!(
        stdout.contains("archive status=ORPHAN/uncovered"),
        "{stdout}"
    );

    let repaired = doctor_command(temp.path(), true).output().unwrap();
    let repaired_stdout = String::from_utf8_lossy(&repaired.stdout);
    assert!(
        repaired_stdout.contains("demo.jsonl: kept 2, dropped 1"),
        "{repaired_stdout}"
    );
    assert!(
        repaired_stdout.contains("demo.1.jsonl: kept 1, dropped 1"),
        "{repaired_stdout}"
    );
    assert!(
        repaired_stdout.contains("a torn line usually costs 2 records"),
        "{repaired_stdout}"
    );
    assert_eq!(backup_count(progress), 2);
    for path in [&active, &archive] {
        for line in std::fs::read(path).unwrap().split(|byte| *byte == b'\n') {
            if !line.is_empty() {
                serde_json::from_slice::<serde_json::Value>(line).unwrap();
            }
        }
    }

    let second = doctor_command(temp.path(), true).output().unwrap();
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.contains("no corrupt progress lines found; no journals changed"),
        "{second_stdout}"
    );
    assert_eq!(backup_count(progress), 2);

    std::fs::write(progress.join("demo.checkpoint.json"), b"{not-json").unwrap();
    let parse_error = doctor_command(temp.path(), false).output().unwrap();
    let parse_error_stdout = String::from_utf8_lossy(&parse_error.stdout);
    assert!(
        parse_error_stdout.contains("checkpoint=PARSE ERROR"),
        "{parse_error_stdout}"
    );

    let mut active_file = std::fs::OpenOptions::new()
        .append(true)
        .open(&active)
        .unwrap();
    active_file.write_all(b"{new-corruption}\n").unwrap();
    drop(active_file);
    let repaired_with_bad_checkpoint = doctor_command(temp.path(), true).output().unwrap();
    let repaired_with_bad_checkpoint_stdout =
        String::from_utf8_lossy(&repaired_with_bad_checkpoint.stdout);
    assert!(
        repaired_with_bad_checkpoint_stdout.contains("demo.jsonl: kept 2, dropped 1"),
        "{repaired_with_bad_checkpoint_stdout}"
    );
    assert_eq!(backup_count(progress), 3);
}
