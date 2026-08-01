//! Hermetic coverage for daemon-start vendor MCP auto-registration.
//!
//! Every home/config/bin variable is pinned under one temp directory. The
//! command must never inspect or modify the developer's real vendor configs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
    ccteam_home: PathBuf,
    claude_home: PathBuf,
    claude_json: PathBuf,
    codex_home: PathBuf,
    kimi_home: PathBuf,
    xdg_home: PathBuf,
    empty_path: PathBuf,
    bins: std::collections::BTreeMap<&'static str, PathBuf>,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("sandbox");
        let home = root.join("home");
        let ccteam_home = root.join("ccteam-home");
        let claude_home = root.join("claude-config").join(".claude");
        let claude_json = claude_home.parent().unwrap().join(".claude.json");
        let codex_home = root.join("codex-home");
        let kimi_home = root.join("kimi-home");
        let xdg_home = root.join("xdg-config");
        let empty_path = root.join("empty-path");
        for dir in [
            &home,
            &ccteam_home,
            &claude_home,
            &codex_home,
            &kimi_home,
            &xdg_home,
            &empty_path,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }

        let bin_dir = root.join("bins");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let mut bins = std::collections::BTreeMap::new();
        for vendor in ["claude", "codex", "grok"] {
            let path = bin_dir.join(vendor);
            write_fake_bin(&path);
            bins.insert(vendor, path);
        }
        // OpenCode exercises the alternate gate: no binary, but an existing
        // config footprint. Kimi has neither and must stay skipped.
        bins.insert("opencode", bin_dir.join("missing-opencode"));
        // Kimi's override deliberately points at a directory. A directory is
        // not a runnable binary and must not trigger config creation.
        bins.insert("kimi", kimi_home.clone());

        Self {
            _tmp: tmp,
            root,
            home,
            ccteam_home,
            claude_home,
            claude_json,
            codex_home,
            kimi_home,
            xdg_home,
            empty_path,
            bins,
        }
    }

    fn grok_config(&self) -> PathBuf {
        self.home.join(".grok/config.toml")
    }

    fn opencode_config(&self) -> PathBuf {
        self.xdg_home.join("opencode/opencode.json")
    }

    fn kimi_config(&self) -> PathBuf {
        self.kimi_home.join("mcp.json")
    }

    fn seed_siblings(&self) {
        std::fs::write(
            &self.claude_json,
            r#"{"mcpServers":{"sibling":{"command":"sibling"}}}"#,
        )
        .unwrap();
        std::fs::write(
            self.codex_home.join("config.toml"),
            "[mcp_servers.sibling]\ncommand = \"sibling\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(self.grok_config().parent().unwrap()).unwrap();
        std::fs::write(
            self.grok_config(),
            "[mcp_servers.sibling]\ncommand = \"sibling\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(self.opencode_config().parent().unwrap()).unwrap();
        std::fs::write(
            self.opencode_config(),
            r#"{"mcp":{"sibling":{"type":"local","command":["sibling"]}}}"#,
        )
        .unwrap();
    }

    /// What an EXISTING user's configs look like before upgrading to the
    /// HTTP-only registration: ccteam's own entry is a stdio `mcp-serve`
    /// child, pointing at whatever binary path was current back then.
    fn seed_legacy_stdio_entries(&self) {
        std::fs::write(
            &self.claude_json,
            r#"{"userID":"keep-me","mcpServers":{"sibling":{"command":"sibling"},"ccteam":{"command":"/old/path/ccteam","args":["internal","mcp-serve"],"env":{}}}}"#,
        )
        .unwrap();
        std::fs::write(
            self.codex_home.join("config.toml"),
            "[mcp_servers.ccteam]\ncommand = \"/old/path/ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(self.grok_config().parent().unwrap()).unwrap();
        std::fs::write(
            self.grok_config(),
            "[mcp_servers.ccteam]\ncommand = \"/old/path/ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(self.opencode_config().parent().unwrap()).unwrap();
        std::fs::write(
            self.opencode_config(),
            r#"{"mcp":{"ccteam":{"type":"local","command":["/old/path/ccteam","internal","mcp-serve"]}}}"#,
        )
        .unwrap();
    }

    fn run(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ccteam"))
            .args(["internal", "register-mcp", "--json"])
            .env("HOME", &self.home)
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CLAUDE_CONFIG_HOME", &self.claude_home)
            .env("CODEX_HOME", &self.codex_home)
            .env("KIMI_CODE_HOME", &self.kimi_home)
            .env("XDG_CONFIG_HOME", &self.xdg_home)
            .env("PATH", &self.empty_path)
            .env("CCTEAM_CLAUDE_BIN", &self.bins["claude"])
            .env("CCTEAM_CODEX_BIN", &self.bins["codex"])
            .env("CCTEAM_GROK_BIN", &self.bins["grok"])
            .env("CCTEAM_OPENCODE_BIN", &self.bins["opencode"])
            .env("CCTEAM_KIMI_BIN", &self.bins["kimi"])
            .output()
            .expect("run internal register-mcp")
    }
}

fn write_fake_bin(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

fn parse_output(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "register-mcp failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

#[test]
fn auto_registration_is_gated_idempotent_and_merge_preserving() {
    let sb = Sandbox::new();
    sb.seed_siblings();

    let first = parse_output(&sb.run());
    let results = first["results"].as_array().unwrap();
    assert_eq!(results.len(), 5);
    for vendor in ["claude", "codex", "grok", "opencode"] {
        let row = results.iter().find(|row| row["vendor"] == vendor).unwrap();
        assert_eq!(row["status"], "registered");
        let path = Path::new(row["path"].as_str().unwrap());
        assert!(
            path.starts_with(&sb.root),
            "registration escaped sandbox: {}",
            path.display()
        );
    }
    let kimi = results.iter().find(|row| row["vendor"] == "kimi").unwrap();
    assert_eq!(kimi["status"], "skipped");
    assert!(
        !sb.kimi_config().exists(),
        "absent vendor must not gain a config footprint"
    );

    let claude: Value =
        serde_json::from_str(&std::fs::read_to_string(&sb.claude_json).unwrap()).unwrap();
    assert_eq!(claude["mcpServers"]["sibling"]["command"], "sibling");
    // One transport for all five vendors: HTTP + admin bearer, no stdio child.
    assert_eq!(claude["mcpServers"]["ccteam"]["type"], "http");
    assert!(claude["mcpServers"]["ccteam"]["url"].is_string());
    assert!(claude["mcpServers"]["ccteam"]
        .get("command")
        .is_none_or(Value::is_null));

    let codex: toml::Table =
        toml::from_str(&std::fs::read_to_string(sb.codex_home.join("config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        codex["mcp_servers"]["sibling"]["command"].as_str(),
        Some("sibling")
    );
    assert!(codex["mcp_servers"]["ccteam"]["url"].is_str());

    let grok: toml::Table =
        toml::from_str(&std::fs::read_to_string(sb.grok_config()).unwrap()).unwrap();
    assert_eq!(
        grok["mcp_servers"]["sibling"]["command"].as_str(),
        Some("sibling")
    );
    assert!(grok["mcp_servers"]["ccteam"]["url"].is_str());

    let opencode: Value =
        serde_json::from_str(&std::fs::read_to_string(sb.opencode_config()).unwrap()).unwrap();
    assert_eq!(opencode["mcp"]["sibling"]["type"], "local");
    assert_eq!(opencode["mcp"]["ccteam"]["type"], "remote");

    let second = parse_output(&sb.run());
    assert!(second["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "registered" || row["status"] == "skipped"));
    let claude_after: Value =
        serde_json::from_str(&std::fs::read_to_string(&sb.claude_json).unwrap()).unwrap();
    assert_eq!(claude_after["mcpServers"]["sibling"]["command"], "sibling");
    assert!(sb.ccteam_home.starts_with(&sb.root));
}

/// The upgrade path for EXISTING users: nobody edits `~/.claude.json` by hand.
/// Daemon start (`ccteam start` / `daemon restart` / the restart `ccteam update`
/// performs) runs this same auto-registration unconditionally, so a legacy stdio
/// `mcp-serve` entry is rewritten to HTTP in place — for every vendor, not just
/// Claude — while the user's own sibling servers and unrelated keys survive.
#[test]
fn upgrade_rewrites_legacy_stdio_entries_to_http_for_every_vendor() {
    let sb = Sandbox::new();
    sb.seed_legacy_stdio_entries();

    parse_output(&sb.run());

    let claude: Value =
        serde_json::from_str(&std::fs::read_to_string(&sb.claude_json).unwrap()).unwrap();
    let entry = &claude["mcpServers"]["ccteam"];
    assert_eq!(entry["type"], "http", "claude entry must become HTTP");
    assert!(entry["url"].is_string());
    assert!(
        entry["headers"]["Authorization"]
            .as_str()
            .is_some_and(|v| v.starts_with("Bearer ccteam:")),
        "claude entry must carry the admin bearer: {entry}"
    );
    // The stdio child is gone — no leftover key can combine with `url`.
    for stale in ["command", "args", "env"] {
        assert!(
            entry.get(stale).is_none(),
            "legacy `{stale}` survived the rewrite: {entry}"
        );
    }
    // Merge, never clobber: the user's other server and unrelated keys stay.
    assert_eq!(claude["mcpServers"]["sibling"]["command"], "sibling");
    assert_eq!(claude["userID"], "keep-me");

    let codex: toml::Table =
        toml::from_str(&std::fs::read_to_string(sb.codex_home.join("config.toml")).unwrap())
            .unwrap();
    let codex_entry = codex["mcp_servers"]["ccteam"].as_table().unwrap();
    assert!(codex_entry.contains_key("url"));
    assert!(!codex_entry.contains_key("command"));

    let grok: toml::Table =
        toml::from_str(&std::fs::read_to_string(sb.grok_config()).unwrap()).unwrap();
    let grok_entry = grok["mcp_servers"]["ccteam"].as_table().unwrap();
    assert!(grok_entry.contains_key("url"));
    assert!(!grok_entry.contains_key("command"));

    let opencode: Value =
        serde_json::from_str(&std::fs::read_to_string(sb.opencode_config()).unwrap()).unwrap();
    assert_eq!(opencode["mcp"]["ccteam"]["type"], "remote");
    assert!(opencode["mcp"]["ccteam"].get("command").is_none());
}
