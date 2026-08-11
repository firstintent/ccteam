//! ccteam's own MCP-server registration into the vendor configs.
//!
//! This is the **one** thing ccteam writes into a vendor's footprint
//! (red line: "ccteam executes/writes nothing else"; registering its own
//! MCP server is the single allowed write). `ccteam config`, ccteam-web's
//! `POST /api/v1/hosts/{host}/register-mcp`, and daemon-start
//! auto-registration all call these seams, so the merge logic lives in exactly
//! one place.
//!
//! All writers are **merge, never clobber + idempotent**: an existing
//! config keeps every other key / MCP server; only `ccteam` is set. The
//! `_into` seams take an explicit path so they are unit-testable against a
//! temp dir without touching the real `~/.claude.json` / `~/.codex`.
//!
//! **One transport for all five vendors: Streamable HTTP against the daemon's
//! `POST /mcp`, carrying an ENROLLMENT credential
//! ([`crate::enroll`]).** No registration spawns a stdio child: the `internal
//! mcp-serve` command that served that shape is deleted, and a legacy
//! `command`/`args` entry left in a user's config reads as NOT registered so
//! the repair pass replaces it. The shapes differ only in each vendor's config
//! dialect (Claude/Kimi `mcpServers` JSON, Codex/Grok `[mcp_servers]` TOML,
//! OpenCode `mcp` JSON) and in the header key (`headers` vs Codex's
//! `http_headers`).
//!
//! What the entry's bearer is FOR: a vendor's global config is one static file
//! shared by every process that vendor ever starts, so anything durable in it
//! is a machine-wide SHARED identity. It used to be the admin web token, which
//! made every hand-started `claude`/`codex`/`grok` the same caller — none could
//! be a delegation parent and none had a project of its own. The enrollment
//! credential says only WHOSE config this is and grants nothing by itself; the
//! per-process identity is issued by the daemon at `initialize` time. A legacy
//! `Bearer ccteam:<token>` entry therefore reads as NOT registered
//! ([`is_current_bearer`]) so the daemon's idempotent repair pass replaces it,
//! exactly as it does a legacy stdio entry. ccteam-managed sessions never rely
//! on this file at all: they override the same-named entry with their
//! per-session `ccteam-sid:<sid>:<secret>` principal (see
//! `ccteam_harness::execution::mcp_config`).
//!
//! The URL every writer registers comes from the ONE resolution chain in
//! `ccteam_harness::execution::mcp_config` (`resolve_mcp_http_url`) — the same
//! chain a managed session's own endpoint resolves through, so a non-default
//! `--web-bind` cannot leave global config and managed sessions disagreeing.
//!
//! Every config written here embeds a credential, so every writer chmods its
//! file to 0600.
//!
//! v0.8.18 柱1 — moved here from `ccteam-cli::mcp_serve` (which now
//! re-exports) so the web host page can register MCP without depending on
//! the CLI binary crate.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// The `Authorization` value every dialect writes. One home for the wire form
/// so five writers cannot drift from the one predicate that reads it back.
fn authorization_value(bearer: &str) -> String {
    format!("Bearer {bearer}")
}

/// Whether an existing entry's `Authorization` carries the CURRENT credential
/// family. Anything else — most importantly the legacy `Bearer ccteam:<admin
/// web token>` — reads as false, which makes the daemon's idempotent repair
/// pass rewrite the entry on next start. Same mechanism the legacy stdio
/// `command` entry uses, and the reason no migration code is needed.
fn is_current_bearer(value: &str) -> bool {
    value
        .strip_prefix("Bearer ")
        .and_then(crate::enroll::parse_enroll_bearer)
        .is_some()
}

/// Validate what every writer is about to persist, before any file is touched.
///
/// The bearer must be one [`is_current_bearer`] accepts, or the writer would
/// produce an entry its own predicate reads as NOT registered — and the daemon
/// would then rewrite that file on every single start, forever, with nothing in
/// the logs to explain the churn. Failing the call names the mistake instead.
fn checked_pair<'a>(mcp_http_url: &'a str, bearer: &'a str) -> Result<(&'a str, &'a str)> {
    let url = mcp_http_url.trim();
    if url.is_empty() {
        anyhow::bail!("ccteam MCP HTTP URL must not be empty");
    }
    let bearer = bearer.trim();
    if bearer.is_empty() {
        anyhow::bail!("ccteam MCP enrollment bearer must not be empty");
    }
    if !is_current_bearer(&authorization_value(bearer)) {
        anyhow::bail!(
            "ccteam MCP registration needs an enrollment bearer \
             ({}<id>:<secret>) — a vendor's global config is shared by every \
             process that vendor starts, so it must not carry the admin web token",
            crate::enroll::ENROLL_BEARER_PREFIX
        );
    }
    Ok((url, bearer))
}

/// Register the ccteam MCP server in Claude's `~/.claude.json` as an
/// `mcpServers.ccteam` Streamable HTTP entry (`type` + `url` + `headers` —
/// the shape `claude mcp add --transport http` writes, and the same one the
/// per-session `--mcp-config` file uses).
///
/// MERGE, never clobber + idempotent: every other top-level key and every
/// other `mcpServers.*` entry survives; only `ccteam` is set. A junk /
/// non-object root is tolerated (treated as "start fresh") rather than
/// failing the install.
///
/// The whole `ccteam` table is REPLACED so a legacy stdio `command` / `args` /
/// `env` cannot survive alongside `url` — Claude's config schema validates
/// per transport, and a mixed entry fails the server outright.
pub fn install_mcp_into(claude_json: &Path, mcp_http_url: &str, bearer: &str) -> Result<()> {
    let (url, bearer) = checked_pair(mcp_http_url, bearer)?;
    let mut root = if claude_json.exists() {
        let bytes = std::fs::read(claude_json)
            .with_context(|| format!("read {}", claude_json.display()))?;
        if bytes.is_empty() {
            serde_json::Map::new()
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            }
        }
    } else {
        serde_json::Map::new()
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match servers {
        Value::Object(m) => m,
        _ => {
            *servers = Value::Object(serde_json::Map::new());
            servers.as_object_mut().unwrap()
        }
    };
    map.insert(
        crate::CCTEAM_MCP_SERVER_KEY.into(),
        json!({
            "type": "http",
            "url": url,
            "headers": { "Authorization": authorization_value(bearer) },
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(claude_json, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(claude_json, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", claude_json.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in Codex's `config.toml` as a
/// `[mcp_servers.ccteam]` Streamable HTTP table.
///
/// MERGE, never clobber: every other top-level key and every other
/// `[mcp_servers.*]` entry is preserved; only `mcp_servers.ccteam` is
/// set/replaced. `bearer` is the complete credential value (this writer only
/// adds the `Bearer` scheme). Parent dir + file are created if absent.
/// Idempotent.
///
/// The global entry intentionally uses the same HTTP transport as Codex's
/// per-thread override. Codex 0.144.3 deep-merges those tables, so retaining a
/// legacy global `command` while a thread adds `url` creates an invalid mixed
/// transport and rejects `thread/start`.
pub fn install_codex_mcp_into(config_toml: &Path, mcp_http_url: &str, bearer: &str) -> Result<()> {
    let (url, bearer) = checked_pair(mcp_http_url, bearer)?;

    // Parse the existing config (or start empty). A non-table / unparseable
    // root is treated as "start fresh" rather than failing the install,
    // mirroring `install_mcp_into`'s tolerance for a junk `~/.claude.json`.
    let mut root: toml::Table = if config_toml.exists() {
        let body = std::fs::read_to_string(config_toml)
            .with_context(|| format!("read {}", config_toml.display()))?;
        if body.trim().is_empty() {
            toml::Table::new()
        } else {
            toml::from_str::<toml::Table>(&body)
                .with_context(|| format!("parse existing {}", config_toml.display()))?
        }
    } else {
        toml::Table::new()
    };

    // Ensure `[mcp_servers]` is a table, preserving any sibling servers.
    let servers_entry = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !servers_entry.is_table() {
        anyhow::bail!(
            "{}: `mcp_servers` exists but is not a TOML table",
            config_toml.display()
        );
    }
    let servers = servers_entry.as_table_mut().expect("checked above");

    // Replace the whole ccteam table so a prior stdio command/args/env cannot
    // survive and combine with `url` under Codex's deep config merge.
    let mut entry = toml::Table::new();
    entry.insert("url".to_string(), toml::Value::String(url.to_string()));
    let mut headers = toml::Table::new();
    headers.insert(
        "Authorization".to_string(),
        toml::Value::String(authorization_value(bearer)),
    );
    entry.insert("http_headers".to_string(), toml::Value::Table(headers));
    servers.insert(
        crate::CCTEAM_MCP_SERVER_KEY.to_string(),
        toml::Value::Table(entry),
    );

    let body = toml::to_string_pretty(&root).context("serialize codex config.toml")?;
    atomic_write(config_toml, body.as_bytes())?;
    // This table now carries a credential. Keep Codex's config private
    // even when the caller's umask would otherwise create a world-readable
    // replacement during the atomic rewrite.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_toml, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", config_toml.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in Grok CLI's `~/.grok/config.toml` as a
/// `[mcp_servers.ccteam]` HTTP entry — the same table `grok mcp add
/// --transport http` writes (`url` + `enabled` + `headers`; Grok's key is
/// `headers`, Codex's is `http_headers`).
///
/// MERGE, never clobber + idempotent, mirroring [`install_codex_mcp_into`].
/// The global entry carries the enrollment bearer, so any plain `grok` main
/// session can orchestrate (v0.9.3 vendor symmetry). ccteam-managed grok
/// sessions ALSO receive a same-named ACP-injected server with their
/// per-session principal; real-machine probe (grok 0.2.x, 2026-07-15): grok
/// connects both, dedups same-named tools, and `tools/call` lands on the
/// ACP-injected (session) server — so the delegation parent edge survives the
/// global entry.
pub fn install_grok_mcp_into(config_toml: &Path, mcp_http_url: &str, bearer: &str) -> Result<()> {
    let (url, bearer) = checked_pair(mcp_http_url, bearer)?;

    let mut root: toml::Table = if config_toml.exists() {
        let body = std::fs::read_to_string(config_toml)
            .with_context(|| format!("read {}", config_toml.display()))?;
        if body.trim().is_empty() {
            toml::Table::new()
        } else {
            toml::from_str::<toml::Table>(&body)
                .with_context(|| format!("parse existing {}", config_toml.display()))?
        }
    } else {
        toml::Table::new()
    };

    let servers_entry = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !servers_entry.is_table() {
        anyhow::bail!(
            "{}: `mcp_servers` exists but is not a TOML table",
            config_toml.display()
        );
    }
    let servers = servers_entry.as_table_mut().expect("checked above");

    let mut entry = toml::Table::new();
    entry.insert("url".to_string(), toml::Value::String(url.to_string()));
    entry.insert("enabled".to_string(), toml::Value::Boolean(true));
    let mut headers = toml::Table::new();
    headers.insert(
        "Authorization".to_string(),
        toml::Value::String(authorization_value(bearer)),
    );
    entry.insert("headers".to_string(), toml::Value::Table(headers));
    servers.insert(
        crate::CCTEAM_MCP_SERVER_KEY.to_string(),
        toml::Value::Table(entry),
    );

    let body = toml::to_string_pretty(&root).context("serialize grok config.toml")?;
    atomic_write(config_toml, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_toml, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", config_toml.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in OpenCode's global `opencode.json` under
/// the runtime `mcp.<name>` shape (OpenCode 1.18.x CLI loader:
/// `packages/opencode/src/config/config.ts` deep-merges raw JSON and the MCP
/// service consumes `mcp.<name>` records — the `mcp.servers` shape belongs to
/// the separate core-v2 path, not the shipped CLI).
///
/// MERGE, never clobber + idempotent: only `mcp.ccteam` is set; every other
/// key survives. Credential inside → 0600. ccteam-managed OpenCode sessions
/// override this by name at runtime (`MCP.add` replaces the same-named config
/// entry with the ACP-injected per-session principal).
pub fn install_opencode_mcp_into(
    opencode_json: &Path,
    mcp_http_url: &str,
    bearer: &str,
) -> Result<()> {
    let (url, bearer) = checked_pair(mcp_http_url, bearer)?;

    let mut root = if opencode_json.exists() {
        let bytes = std::fs::read(opencode_json)
            .with_context(|| format!("read {}", opencode_json.display()))?;
        if bytes.is_empty() {
            serde_json::Map::new()
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            }
        }
    } else {
        serde_json::Map::new()
    };

    let mcp = root
        .entry("mcp")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match mcp {
        Value::Object(m) => m,
        _ => {
            *mcp = Value::Object(serde_json::Map::new());
            mcp.as_object_mut().unwrap()
        }
    };
    map.insert(
        crate::CCTEAM_MCP_SERVER_KEY.into(),
        json!({
            "type": "remote",
            "url": url,
            "enabled": true,
            "headers": { "Authorization": authorization_value(bearer) },
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(opencode_json, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(opencode_json, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", opencode_json.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in Kimi Code CLI's global
/// `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code/mcp.json`) as an
/// `mcpServers.ccteam` HTTP entry — the shape `docs/en/customization/mcp.md`
/// documents (`url` + `headers`; an entry with `url` and no `transport` is
/// HTTP). NOTE: the file schema's `headers` is a **map** — do NOT copy the
/// ACP `mcpServers` parameter's name/value array shape here.
///
/// MERGE, never clobber + idempotent: only `mcpServers.ccteam` is set; every
/// other key survives. Credential inside → 0600. ccteam-managed kimi
/// sessions ALSO receive a same-named ACP-injected server with their
/// per-session principal (same dedup-by-name posture as grok/opencode).
pub fn install_kimi_mcp_into(mcp_json: &Path, mcp_http_url: &str, bearer: &str) -> Result<()> {
    let (url, bearer) = checked_pair(mcp_http_url, bearer)?;

    let mut root = if mcp_json.exists() {
        let bytes =
            std::fs::read(mcp_json).with_context(|| format!("read {}", mcp_json.display()))?;
        if bytes.is_empty() {
            serde_json::Map::new()
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            }
        }
    } else {
        serde_json::Map::new()
    };

    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match servers {
        Value::Object(m) => m,
        _ => {
            *servers = Value::Object(serde_json::Map::new());
            servers.as_object_mut().unwrap()
        }
    };
    map.insert(
        crate::CCTEAM_MCP_SERVER_KEY.into(),
        json!({
            "url": url,
            "headers": { "Authorization": authorization_value(bearer) },
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(mcp_json, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(mcp_json, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", mcp_json.display()))?;
    }
    Ok(())
}

/// Whether Kimi's `mcp.json` carries the ccteam HTTP MCP entry with a current
/// ([`is_current_bearer`]) credential. Best-effort; missing/junk file reads as
/// `false`.
pub fn kimi_mcp_registered(mcp_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(mcp_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(entry) = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|m| m.get(crate::CCTEAM_MCP_SERVER_KEY))
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty())
        && entry
            .pointer("/headers/Authorization")
            .and_then(Value::as_str)
            .is_some_and(is_current_bearer)
}

/// Resolve Kimi Code CLI's user MCP config: `$KIMI_CODE_HOME/mcp.json`
/// (KIMI_CODE_HOME → `~/.kimi-code` fallback), mirroring kimi's own session
/// dir resolution.
pub fn resolve_kimi_config_path() -> Result<PathBuf> {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")))
        .ok_or_else(|| anyhow!("cannot resolve KIMI_CODE_HOME or ~/.kimi-code (no home dir)"))?;
    Ok(kimi_home.join("mcp.json"))
}

/// Whether Grok's `config.toml` carries the current HTTP form of
/// `[mcp_servers.ccteam]` with a current ([`is_current_bearer`]) credential.
/// Best-effort; missing/junk file reads as `false`.
pub fn grok_mcp_registered(config_toml: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(config_toml) else {
        return false;
    };
    let Ok(root) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    let Some(entry) = root
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(crate::CCTEAM_MCP_SERVER_KEY))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(toml::Value::as_str)
        .is_some_and(|url| !url.trim().is_empty())
        && entry
            .get("headers")
            .and_then(toml::Value::as_table)
            .and_then(|headers| headers.get("Authorization"))
            .and_then(toml::Value::as_str)
            .is_some_and(is_current_bearer)
}

/// Whether OpenCode's global `opencode.json` carries the ccteam remote MCP
/// entry with a current ([`is_current_bearer`]) credential. Best-effort;
/// missing/junk file reads as `false`.
pub fn opencode_mcp_registered(opencode_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(opencode_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(entry) = root
        .get("mcp")
        .and_then(Value::as_object)
        .and_then(|m| m.get(crate::CCTEAM_MCP_SERVER_KEY))
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty())
        && entry
            .pointer("/headers/Authorization")
            .and_then(Value::as_str)
            .is_some_and(is_current_bearer)
}

/// Resolve Grok CLI's user config: `~/.grok/config.toml`.
pub fn resolve_grok_config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".grok").join("config.toml"))
        .ok_or_else(|| anyhow!("cannot resolve ~/.grok (no home dir)"))
}

/// Resolve OpenCode's global config: `$XDG_CONFIG_HOME/opencode/opencode.json`
/// (default `~/.config/opencode/opencode.json`) — the middle file of the
/// loader's `config.json` → `opencode.json` → `opencode.jsonc` merge chain.
pub fn resolve_opencode_config_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .ok_or_else(|| anyhow!("cannot resolve XDG_CONFIG_HOME or ~/.config (no home dir)"))?;
    Ok(base.join("opencode").join("opencode.json"))
}

/// Whether `~/.claude.json` carries the current HTTP form of
/// `mcpServers.ccteam` with a current ([`is_current_bearer`]) credential. A
/// legacy stdio entry — and equally a legacy admin-token bearer — deliberately
/// reads as `false` so doctor / host readiness asks the operator to rerun
/// `ccteam config mcp` and the daemon's auto-registration replaces the old
/// shape, instead of declaring a `mcp-serve`-child entry ready. Best-effort: a
/// missing / unreadable / junk file reads as `false`. Read-only — never writes.
pub fn claude_mcp_registered(claude_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(claude_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(entry) = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|m| m.get(crate::CCTEAM_MCP_SERVER_KEY))
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty())
        && entry
            .pointer("/headers/Authorization")
            .and_then(Value::as_str)
            .is_some_and(is_current_bearer)
        && entry.get("command").is_none()
}

/// Whether Codex's `config.toml` carries the current HTTP form of
/// `[mcp_servers.ccteam]` with a current ([`is_current_bearer`]) credential. A
/// legacy stdio entry — and equally a legacy admin-token bearer — deliberately
/// reads as `false` so doctor/host readiness asks the operator to rerun `ccteam
/// config mcp` and removes the `mcp-serve` child instead of declaring the old
/// shape ready. Best-effort: a missing / unreadable / junk file reads as `false`.
pub fn codex_mcp_registered(config_toml: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(config_toml) else {
        return false;
    };
    let Ok(root) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    let Some(entry) = root
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(crate::CCTEAM_MCP_SERVER_KEY))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(toml::Value::as_str)
        .is_some_and(|url| !url.trim().is_empty())
        && entry
            .get("http_headers")
            .and_then(toml::Value::as_table)
            .and_then(|headers| headers.get("Authorization"))
            .and_then(toml::Value::as_str)
            .is_some_and(is_current_bearer)
        && !entry.contains_key("command")
}

/// Resolve `$CODEX_HOME/config.toml` (CODEX_HOME → `~/.codex` fallback),
/// mirroring `ccteam_harness::execution::codex_app_server`'s resolution.
pub fn resolve_codex_config_path() -> Result<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or_else(|| anyhow!("cannot resolve CODEX_HOME or ~/.codex (no home dir)"))?;
    Ok(codex_home.join("config.toml"))
}

/// Atomic write via a sibling `.ccteam-mcp.tmp` + rename. Creates the
/// parent dir if absent.
fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".ccteam-mcp.tmp");
    let tmp = PathBuf::from(tmp_os);
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        }
        f.write_all(body)?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form production hands these writers: a machine-user
    /// ENROLLMENT bearer. The `_into` seams never mint one, so the tests state
    /// it literally rather than reaching into `~/.ccteam`.
    fn enroll(secret: &str) -> String {
        format!(
            "{}deadbeefdeadbeef:{secret}",
            crate::enroll::ENROLL_BEARER_PREFIX
        )
    }

    /// What every pre-enrollment config still carries: the machine-wide SHARED
    /// admin web token. Every predicate must read this as NOT registered.
    fn legacy_admin_bearer(token: &str) -> String {
        format!("Bearer ccteam:{token}")
    }

    #[test]
    fn install_mcp_into_writes_http_url_and_enroll_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        install_mcp_into(&claude_json, "http://127.0.0.1:7331/mcp", &enroll("s3cret")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["type"], "http");
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(
            entry["headers"]["Authorization"],
            format!("Bearer {}", enroll("s3cret"))
        );
        // No stdio child: the global entry never spawns `internal mcp-serve`.
        assert!(entry.get("command").is_none());
        assert!(entry.get("args").is_none());
    }

    /// The predicate is what decides whether the daemon repairs an entry, so
    /// it must accept exactly the family the writers produce.
    #[test]
    fn only_the_enrollment_family_reads_as_a_current_bearer() {
        assert!(is_current_bearer(&format!("Bearer {}", enroll("s"))));
        assert!(!is_current_bearer(&legacy_admin_bearer("deadbeef")));
        assert!(!is_current_bearer("Bearer ccteam-sid:s1:secret"));
        assert!(!is_current_bearer(&enroll("s")), "scheme is mandatory");
        assert!(!is_current_bearer("Bearer ccteam-enroll:not-hex:s"));
        assert!(!is_current_bearer(""));
    }

    #[test]
    fn install_mcp_into_preserves_other_top_level_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"projects":{"a":1},"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        install_mcp_into(&claude_json, "http://localhost:7331/mcp", &enroll("tok")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(v["projects"]["a"], 1);
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(
            v["mcpServers"]["ccteam"]["url"],
            "http://localhost:7331/mcp"
        );
    }

    #[test]
    fn install_mcp_into_replaces_legacy_stdio_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"mcpServers":{"ccteam":{"command":"/old/ccteam","args":["internal","mcp-serve"],"env":{"CCTEAM_CHAT_SID":"stale"}}}}"#,
        )
        .unwrap();
        assert!(
            !claude_mcp_registered(&claude_json),
            "legacy stdio shape must read as NOT registered so it gets repaired"
        );
        install_mcp_into(&claude_json, "http://localhost:7331/mcp", &enroll("fresh")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["url"], "http://localhost:7331/mcp");
        assert!(entry.get("command").is_none());
        assert!(entry.get("args").is_none());
        assert!(entry.get("env").is_none());
        assert!(claude_mcp_registered(&claude_json));
    }

    /// The upgrade path off the shared machine identity: an entry that still
    /// carries the admin web token reads as NOT registered, so the daemon's
    /// repair pass rewrites it. No migration code — replace in place.
    #[test]
    fn install_mcp_into_replaces_a_legacy_admin_token_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            json!({"mcpServers": {"ccteam": {
                "type": "http",
                "url": "http://localhost:7331/mcp",
                "headers": {"Authorization": legacy_admin_bearer("deadbeefcafe")},
            }}})
            .to_string(),
        )
        .unwrap();
        assert!(
            !claude_mcp_registered(&claude_json),
            "the shared admin-token entry must read as NOT registered"
        );
        install_mcp_into(&claude_json, "http://localhost:7331/mcp", &enroll("fresh")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["ccteam"]["headers"]["Authorization"],
            format!("Bearer {}", enroll("fresh"))
        );
        assert!(claude_mcp_registered(&claude_json));
    }

    #[test]
    fn claude_mcp_registered_detects_presence_and_absence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        assert!(!claude_mcp_registered(&claude_json), "missing file → false");
        std::fs::write(&claude_json, r#"{"mcpServers":{"other":{}}}"#).unwrap();
        assert!(!claude_mcp_registered(&claude_json), "no ccteam → false");
        install_mcp_into(&claude_json, "http://localhost/mcp", &enroll("t")).unwrap();
        assert!(claude_mcp_registered(&claude_json), "after install → true");
    }

    #[test]
    fn install_mcp_into_is_idempotent_and_rejects_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join("nested").join(".claude.json");
        install_mcp_into(&claude_json, "http://localhost:7331/mcp", &enroll("a")).unwrap();
        install_mcp_into(&claude_json, "http://localhost:7444/mcp", &enroll("b")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["ccteam"]["url"],
            "http://localhost:7444/mcp"
        );
        assert_eq!(
            v["mcpServers"]["ccteam"]["headers"]["Authorization"],
            format!("Bearer {}", enroll("b"))
        );
        let absent = tmp.path().join("absent.json");
        assert!(install_mcp_into(&absent, "", &enroll("t")).is_err());
        assert!(install_mcp_into(&absent, "http://x/mcp", " ").is_err());
        assert!(!absent.exists());
    }

    /// A writer must never persist a bearer its own predicate rejects: that
    /// entry would be repaired on every daemon start, forever, silently.
    #[test]
    fn writers_refuse_a_bearer_their_predicate_would_reject() {
        let tmp = tempfile::TempDir::new().unwrap();
        let url = "http://localhost:7331/mcp";
        let stale = "ccteam:deadbeefcafe";
        for (name, result) in [
            (
                "claude",
                install_mcp_into(&tmp.path().join("c.json"), url, stale),
            ),
            (
                "codex",
                install_codex_mcp_into(&tmp.path().join("cx.toml"), url, stale),
            ),
            (
                "grok",
                install_grok_mcp_into(&tmp.path().join("g.toml"), url, stale),
            ),
            (
                "opencode",
                install_opencode_mcp_into(&tmp.path().join("o.json"), url, stale),
            ),
            (
                "kimi",
                install_kimi_mcp_into(&tmp.path().join("k.json"), url, stale),
            ),
        ] {
            let err = result.expect_err(&format!("{name} accepted the admin-token form"));
            assert!(
                err.to_string()
                    .contains(crate::enroll::ENROLL_BEARER_PREFIX),
                "{name} error must name the expected family: {err}"
            );
        }
        // Nothing was written on the way out.
        for file in ["c.json", "cx.toml", "g.toml", "o.json", "k.json"] {
            assert!(!tmp.path().join(file).exists(), "{file} was created");
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_install_makes_token_bearing_config_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        install_mcp_into(&claude_json, "http://localhost/mcp", &enroll("s")).unwrap();
        assert_eq!(
            std::fs::metadata(&claude_json)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn install_codex_mcp_into_merges_preserving_other_servers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "model = \"o3\"\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        install_codex_mcp_into(&config_toml, "http://127.0.0.1:7331/mcp", &enroll("s3cret"))
            .unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(root["model"].as_str(), Some("o3"));
        assert_eq!(root["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://127.0.0.1:7331/mcp")
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["http_headers"]["Authorization"].as_str(),
            Some(format!("Bearer {}", enroll("s3cret")).as_str())
        );
        assert!(root["mcp_servers"]["ccteam"].get("command").is_none());
    }

    #[test]
    fn install_codex_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("nested").join(".codex").join("config.toml");
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", &enroll("a")).unwrap();
        assert!(!codex_mcp_registered(
            &config_toml.with_file_name("absent.toml")
        ));
        assert!(codex_mcp_registered(&config_toml), "after install → true");
        // Second install must not error or duplicate.
        install_codex_mcp_into(&config_toml, "http://localhost:7444/mcp", &enroll("b")).unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://localhost:7444/mcp"),
            "idempotent re-install replaces only the ccteam HTTP entry"
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["http_headers"]["Authorization"].as_str(),
            Some(format!("Bearer {}", enroll("b")).as_str())
        );
    }

    #[test]
    fn codex_mcp_registered_rejects_legacy_stdio_shape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[mcp_servers.ccteam]\ncommand = \"ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\n",
        )
        .unwrap();
        assert!(!codex_mcp_registered(&config_toml));
        install_codex_mcp_into(&config_toml, "http://localhost/mcp", &enroll("t")).unwrap();
        assert!(codex_mcp_registered(&config_toml));
    }

    /// Same replace-in-place upgrade as Claude's, in Codex's dialect (the
    /// header key differs: `http_headers`, not `headers`).
    #[test]
    fn codex_mcp_registered_rejects_a_legacy_admin_token_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            format!(
                "[mcp_servers.ccteam]\nurl = \"http://localhost:7331/mcp\"\n\
                 [mcp_servers.ccteam.http_headers]\nAuthorization = \"{}\"\n",
                legacy_admin_bearer("deadbeefcafe")
            ),
        )
        .unwrap();
        assert!(!codex_mcp_registered(&config_toml));
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", &enroll("fresh"))
            .unwrap();
        assert!(codex_mcp_registered(&config_toml));
    }

    #[test]
    fn install_codex_mcp_into_replaces_legacy_stdio_entry_atomically() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[mcp_servers.ccteam]\ncommand = \"/old/ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\nenv = { CCTEAM_CHAT_SID = \"stale\" }\n",
        )
        .unwrap();

        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", &enroll("fresh"))
            .unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        let entry = root["mcp_servers"]["ccteam"].as_table().unwrap();
        assert_eq!(
            entry.get("url").and_then(toml::Value::as_str),
            Some("http://localhost:7331/mcp")
        );
        assert!(!entry.contains_key("command"));
        assert!(!entry.contains_key("args"));
        assert!(!entry.contains_key("env"));
    }

    #[test]
    fn install_codex_mcp_into_rejects_missing_http_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        assert!(install_codex_mcp_into(&config_toml, "", &enroll("t")).is_err());
        assert!(install_codex_mcp_into(&config_toml, "http://localhost/mcp", " ").is_err());
        assert!(!config_toml.exists());
    }

    #[test]
    fn install_grok_mcp_into_writes_grok_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[cli]\nuse_leader = true\n[mcp_servers.other]\nurl = \"http://x/mcp\"\n",
        )
        .unwrap();
        install_grok_mcp_into(&config_toml, "http://127.0.0.1:7331/mcp", &enroll("tok")).unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(root["cli"]["use_leader"].as_bool(), Some(true));
        assert_eq!(
            root["mcp_servers"]["other"]["url"].as_str(),
            Some("http://x/mcp")
        );
        let entry = root["mcp_servers"]["ccteam"].as_table().unwrap();
        assert_eq!(entry["url"].as_str(), Some("http://127.0.0.1:7331/mcp"));
        assert_eq!(entry["enabled"].as_bool(), Some(true));
        // Grok's key is `headers` (Codex's is `http_headers`).
        assert_eq!(
            entry["headers"]["Authorization"].as_str(),
            Some(format!("Bearer {}", enroll("tok")).as_str())
        );
        assert!(!entry.contains_key("http_headers"));
        assert!(grok_mcp_registered(&config_toml));
        assert!(!grok_mcp_registered(&tmp.path().join("absent.toml")));
    }

    #[test]
    fn install_grok_mcp_into_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join(".grok").join("config.toml");
        install_grok_mcp_into(&config_toml, "http://localhost:7331/mcp", &enroll("a")).unwrap();
        install_grok_mcp_into(&config_toml, "http://localhost:7444/mcp", &enroll("b")).unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://localhost:7444/mcp")
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["headers"]["Authorization"].as_str(),
            Some(format!("Bearer {}", enroll("b")).as_str())
        );
    }

    #[test]
    fn install_opencode_mcp_into_writes_runtime_v1_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let opencode_json = tmp.path().join("opencode.json");
        std::fs::write(
            &opencode_json,
            r#"{"$schema":"https://opencode.ai/config.json","model":"x/y","mcp":{"other":{"type":"local","command":["x"]}}}"#,
        )
        .unwrap();
        install_opencode_mcp_into(&opencode_json, "http://127.0.0.1:7331/mcp", &enroll("tok"))
            .unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&opencode_json).unwrap()).unwrap();
        assert_eq!(v["model"], "x/y");
        assert_eq!(v["mcp"]["other"]["type"], "local");
        // Runtime v1 shape: `mcp.<name>` (NOT the core-v2 `mcp.servers.<name>`).
        let entry = &v["mcp"]["ccteam"];
        assert_eq!(entry["type"], "remote");
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(entry["enabled"], true);
        assert_eq!(
            entry["headers"]["Authorization"],
            format!("Bearer {}", enroll("tok"))
        );
        assert!(v["mcp"].get("servers").is_none());
        assert!(opencode_mcp_registered(&opencode_json));
        assert!(!opencode_mcp_registered(&tmp.path().join("absent.json")));
    }

    #[test]
    fn install_opencode_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let opencode_json = tmp
            .path()
            .join("nested")
            .join("opencode")
            .join("opencode.json");
        install_opencode_mcp_into(&opencode_json, "http://localhost:7331/mcp", &enroll("a"))
            .unwrap();
        install_opencode_mcp_into(&opencode_json, "http://localhost:7444/mcp", &enroll("b"))
            .unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&opencode_json).unwrap()).unwrap();
        assert_eq!(v["mcp"]["ccteam"]["url"], "http://localhost:7444/mcp");
        assert_eq!(
            v["mcp"]["ccteam"]["headers"]["Authorization"],
            format!("Bearer {}", enroll("b"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn grok_and_opencode_installs_make_token_bearing_configs_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let grok = tmp.path().join("config.toml");
        install_grok_mcp_into(&grok, "http://localhost/mcp", &enroll("s")).unwrap();
        assert_eq!(
            std::fs::metadata(&grok).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let oc = tmp.path().join("opencode.json");
        install_opencode_mcp_into(&oc, "http://localhost/mcp", &enroll("s")).unwrap();
        assert_eq!(
            std::fs::metadata(&oc).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn grok_and_opencode_installs_reject_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(install_grok_mcp_into(&tmp.path().join("g.toml"), "", &enroll("t")).is_err());
        assert!(install_grok_mcp_into(&tmp.path().join("g.toml"), "http://x/mcp", " ").is_err());
        assert!(install_opencode_mcp_into(&tmp.path().join("o.json"), "", &enroll("t")).is_err());
        assert!(
            install_opencode_mcp_into(&tmp.path().join("o.json"), "http://x/mcp", " ").is_err()
        );
    }

    #[test]
    fn install_kimi_mcp_into_writes_http_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"filesystem":{"command":"npx","args":["-y","fs"]}}}"#,
        )
        .unwrap();
        install_kimi_mcp_into(&mcp_json, "http://127.0.0.1:7331/mcp", &enroll("tok")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        // Sibling server survives (merge, never clobber).
        assert_eq!(v["mcpServers"]["filesystem"]["command"], "npx");
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        // File-schema headers are a MAP (never the ACP name/value array).
        assert_eq!(
            entry["headers"]["Authorization"],
            format!("Bearer {}", enroll("tok"))
        );
        assert!(entry.get("command").is_none());
        assert!(kimi_mcp_registered(&mcp_json));
        assert!(!kimi_mcp_registered(&tmp.path().join("absent.json")));
    }

    #[test]
    fn install_kimi_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("nested").join("kimi").join("mcp.json");
        install_kimi_mcp_into(&mcp_json, "http://localhost:7331/mcp", &enroll("a")).unwrap();
        install_kimi_mcp_into(&mcp_json, "http://localhost:7444/mcp", &enroll("b")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["ccteam"]["url"],
            "http://localhost:7444/mcp"
        );
        assert_eq!(
            v["mcpServers"]["ccteam"]["headers"]["Authorization"],
            format!("Bearer {}", enroll("b"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn kimi_install_makes_token_bearing_config_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("mcp.json");
        install_kimi_mcp_into(&mcp_json, "http://localhost/mcp", &enroll("s")).unwrap();
        assert_eq!(
            std::fs::metadata(&mcp_json).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn kimi_install_rejects_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(install_kimi_mcp_into(&tmp.path().join("k.json"), "", &enroll("t")).is_err());
        assert!(install_kimi_mcp_into(&tmp.path().join("k.json"), "http://x/mcp", " ").is_err());
    }

    /// The legacy admin-token entry must read as NOT registered in EVERY
    /// dialect, or an un-upgraded vendor keeps the shared machine identity
    /// while the others move on.
    #[test]
    fn every_dialect_rejects_the_legacy_admin_token_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let legacy = legacy_admin_bearer("deadbeefcafe");

        let grok = tmp.path().join("grok.toml");
        std::fs::write(
            &grok,
            format!(
                "[mcp_servers.ccteam]\nurl = \"http://x/mcp\"\nenabled = true\n\
                 [mcp_servers.ccteam.headers]\nAuthorization = \"{legacy}\"\n"
            ),
        )
        .unwrap();
        assert!(!grok_mcp_registered(&grok));
        install_grok_mcp_into(&grok, "http://x/mcp", &enroll("fresh")).unwrap();
        assert!(grok_mcp_registered(&grok));

        let opencode = tmp.path().join("opencode.json");
        std::fs::write(
            &opencode,
            json!({"mcp": {"ccteam": {
                "type": "remote",
                "url": "http://x/mcp",
                "headers": {"Authorization": legacy},
            }}})
            .to_string(),
        )
        .unwrap();
        assert!(!opencode_mcp_registered(&opencode));
        install_opencode_mcp_into(&opencode, "http://x/mcp", &enroll("fresh")).unwrap();
        assert!(opencode_mcp_registered(&opencode));

        let kimi = tmp.path().join("mcp.json");
        std::fs::write(
            &kimi,
            json!({"mcpServers": {"ccteam": {
                "url": "http://x/mcp",
                "headers": {"Authorization": legacy},
            }}})
            .to_string(),
        )
        .unwrap();
        assert!(!kimi_mcp_registered(&kimi));
        install_kimi_mcp_into(&kimi, "http://x/mcp", &enroll("fresh")).unwrap();
        assert!(kimi_mcp_registered(&kimi));
    }

    #[cfg(unix)]
    #[test]
    fn install_codex_mcp_into_makes_token_bearing_config_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        install_codex_mcp_into(&config_toml, "http://localhost/mcp", &enroll("s3cret")).unwrap();
        assert_eq!(
            std::fs::metadata(&config_toml)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
