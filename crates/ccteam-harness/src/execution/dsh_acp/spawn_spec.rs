//! Pure-ish DSH child spawn construction plus the spawn-time guards that must
//! run before stdio opens.
//!
//! DSH is a `ManagedSessionBridge` vendor: the ccteam Cordis plugin reads its
//! MCP endpoint from child env, and the transport face is activated by the same
//! plugin. The bearer appears only in `CCTEAM_MCP_BEARER`.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use super::materialize::{materialize_managed_profile, materialize_web_profile, WEB_PROFILE};
use crate::execution::mcp_config::{project_bridge_child_env, SessionMcpEndpoint};
use crate::{ccteam_root_from_env, HarnessError, PermissionMode, SpawnCtx};

pub const DSH_BIN_ENV: &str = "CCTEAM_DSH_BIN";
pub const DSH_PROFILE: &str = "ccteam";
pub const DSH_WEB_PROFILE: &str = WEB_PROFILE;
pub const DSH_NATIVE_WEB_PROFILE: &str = "web";
pub const DSH_TRANSPORT_ENV: &str = "CCTEAM_DSH_TRANSPORT";
pub const DSH_APPROVAL_ENV: &str = "CCTEAM_DSH_APPROVAL";
pub const DSH_HOME_ENV: &str = "DSH_HOME";
pub const DSH_TELEMETRY_DISABLED_ENV: &str = "DSH_TELEMETRY_DISABLED";
pub const DSH_TELEMETRY_MODE_ENV: &str = "DSH_TELEMETRY_MODE";
pub const DSH_SYSTEM_PROMPT_ENV: &str = "DSH_SYSTEM_PROMPT";
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
pub const DEEPSEEK_BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

pub const MIN_DSH_VERSION: &str = "0.1.0-rc.6";

const USER_DSH_DIR: &str = ".dsh";
const CREDENTIALS_FILE: &str = ".credentials.yaml";
const SETTINGS_FILE: &str = "settings.yaml";

/// Resolve the `dsh` binary path: `CCTEAM_DSH_BIN` override, else `dsh` on
/// `PATH`, else a cached `npx` copy ([`resolve_dsh_default_bin`]). The same
/// resolver the status/hosts/doctor probe panels use (`ccteam-core` and
/// `ccteam-cli` both already depend on this crate — this is the one place
/// that can own it without inverting the dependency graph), so "installed"
/// and "spawns" never disagree.
pub fn dsh_bin() -> String {
    std::env::var(DSH_BIN_ENV).unwrap_or_else(|_| resolve_dsh_default_bin())
}

/// DSH's product CLI is commonly reached only through
/// `npx @deepseek-ai/dsh …` — DSH's own documented quickstart — which never
/// puts a binary on `PATH`: npx caches the resolved package under
/// `~/.npm/_npx/<hash>/node_modules/.bin/dsh` and runs it transiently, once,
/// from that cache directory. Plain-name `PATH` resolution correctly reports
/// "not found" in that state even though a perfectly usable copy sits on
/// disk — which reads to a user who already has `dsh web` running as "ccteam
/// is broken", not as the accurate "no CLI on PATH".
///
/// This is **read-only discovery of something the user already caused to
/// exist** by running `dsh` themselves at least once — no network call, no
/// package fetch, no execution. It does not relax "ccteam never installs a
/// CLI for you": nothing gets installed here, only found.
///
/// Returns the literal `"dsh"` (ordinary `PATH` resolution) when that
/// already works, so the common global-install case is untouched; only
/// falls back to a discovered absolute path when bare `"dsh"` is not on
/// `PATH`.
pub fn resolve_dsh_default_bin() -> String {
    let home = dirs::home_dir();
    let path_env = std::env::var_os("PATH");
    resolve_dsh_default_bin_in(path_env.as_deref(), home.as_deref())
}

/// Scan `~/.npm/_npx/*/node_modules/.bin/dsh` for a cached copy of DSH's
/// product CLI, picking the most recently modified match when several exist
/// (npx keeps one cache directory per exact version range ever requested).
pub fn find_cached_dsh_bin() -> Option<PathBuf> {
    find_cached_dsh_bin_under(&dirs::home_dir()?)
}

const DSH_LITERAL: &str = "dsh";

/// Parameterized core of [`resolve_dsh_default_bin`] — no global env/HOME
/// reads, so it is deterministically testable without mutating process
/// state (env-mutating tests are a repo-wide flake source; see AGENTS.md).
fn resolve_dsh_default_bin_in(path_env: Option<&std::ffi::OsStr>, home: Option<&Path>) -> String {
    if bin_on_path_in(path_env, DSH_LITERAL) {
        return DSH_LITERAL.to_string();
    }
    home.and_then(find_cached_dsh_bin_under)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| DSH_LITERAL.to_string())
}

fn bin_on_path_in(path_env: Option<&std::ffi::OsStr>, name: &str) -> bool {
    let Some(path) = path_env else {
        return false;
    };
    std::env::split_paths(path).any(|dir| is_executable_file(&dir.join(name)))
}

fn find_cached_dsh_bin_under(home: &Path) -> Option<PathBuf> {
    let npx_root = home.join(".npm").join("_npx");
    let entries = std::fs::read_dir(&npx_root).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let candidate = entry.path().join("node_modules").join(".bin").join("dsh");
            is_executable_file(&candidate)
                .then(|| std::fs::metadata(&candidate).ok()?.modified().ok())
                .flatten()
                .map(|modified| (modified, candidate))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone)]
pub struct DshSpawnSpec {
    pub bin: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub cwd: PathBuf,
    /// Retained vendor-memory root. `close_thread` must not remove it.
    pub dsh_home: PathBuf,
}

/// Build the child command for the selected W0 runner tier:
/// `dsh --profile ccteam`.
pub fn build_spawn_spec(
    ctx: &SpawnCtx,
    mcp: &SessionMcpEndpoint,
) -> Result<DshSpawnSpec, HarnessError> {
    let bin = dsh_bin();
    reject_demo_bin(&bin)?;
    let cwd = project_cwd(ctx)?;
    let dsh_home = dsh_home(ctx)?;
    std::fs::create_dir_all(&dsh_home).map_err(|e| {
        HarnessError::SpawnFailed(format!("create DSH_HOME {}: {e}", dsh_home.display()))
    })?;
    materialize_managed_profile(&dsh_home)?;
    mirror_dsh_credentials_if_needed(&dsh_home)?;

    let mut env = vec![
        (
            crate::execution::claude_common::CHAT_SID_ENV.to_string(),
            ctx.sid.clone(),
        ),
        (
            DSH_HOME_ENV.to_string(),
            dsh_home.to_string_lossy().into_owned(),
        ),
        (DSH_TRANSPORT_ENV.to_string(), "1".to_string()),
        (
            DSH_APPROVAL_ENV.to_string(),
            match ctx.permission_mode {
                PermissionMode::Hitl => "hitl",
                PermissionMode::Skip => "skip",
            }
            .to_string(),
        ),
        (DSH_TELEMETRY_DISABLED_ENV.to_string(), "1".to_string()),
        (DSH_TELEMETRY_MODE_ENV.to_string(), "DISABLED".to_string()),
    ];
    for key in [DEEPSEEK_API_KEY_ENV, DEEPSEEK_BASE_URL_ENV] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                env.push((key.to_string(), value));
            }
        }
    }
    env.extend(project_bridge_child_env(mcp));

    Ok(DshSpawnSpec {
        bin,
        args: vec!["--profile".to_string(), DSH_PROFILE.to_string()],
        env,
        env_remove: Vec::new(),
        cwd,
        dsh_home,
    })
}

#[derive(Debug, Clone)]
pub struct DshWebSpawnOptions<'a> {
    pub dsh_home: PathBuf,
    pub profile: &'a str,
    pub materialize_profile: bool,
    pub enrollment: Option<&'a str>,
    pub daemon_url: Option<&'a str>,
    pub scrub_provider_env: bool,
}

/// Build a `dsh web` child command for a ccteam-web companion instance.
///
/// Managed tenant instances use `profile = ccteam-web` and
/// `materialize_profile = true`; the operator fallback uses the vendor-native
/// `web` profile in the real `~/.dsh` so ccteam never writes profile/config
/// files into that vendor-owned home.
pub fn build_web_spawn_spec(options: DshWebSpawnOptions<'_>) -> Result<DshSpawnSpec, HarnessError> {
    let bin = dsh_bin();
    reject_demo_bin(&bin)?;
    std::fs::create_dir_all(&options.dsh_home).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH web home {}: {e}",
            options.dsh_home.display()
        ))
    })?;
    if options.materialize_profile {
        materialize_web_profile(&options.dsh_home, options.enrollment, options.daemon_url)?;
    }

    let mut env = vec![
        (
            DSH_HOME_ENV.to_string(),
            options.dsh_home.to_string_lossy().into_owned(),
        ),
        (DSH_TELEMETRY_DISABLED_ENV.to_string(), "1".to_string()),
        (DSH_TELEMETRY_MODE_ENV.to_string(), "DISABLED".to_string()),
    ];
    let env_remove = if options.scrub_provider_env {
        [DEEPSEEK_API_KEY_ENV, DEEPSEEK_BASE_URL_ENV]
            .into_iter()
            .map(str::to_string)
            .collect()
    } else {
        // Match native DSH behavior for the operator: a hand-set env key may
        // intentionally override files in the real ~/.dsh.
        for key in [DEEPSEEK_API_KEY_ENV, DEEPSEEK_BASE_URL_ENV] {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    env.push((key.to_string(), value));
                }
            }
        }
        Vec::new()
    };

    Ok(DshSpawnSpec {
        bin,
        args: vec![
            "--profile".to_string(),
            options.profile.to_string(),
            "--port".to_string(),
            "0".to_string(),
        ],
        env,
        env_remove,
        cwd: options.dsh_home.clone(),
        dsh_home: options.dsh_home,
    })
}

/// Remove the mirrored credential pair after a managed child stops while
/// keeping the surrounding `DSH_HOME` vendor memory intact.
pub fn purge_mirrored_credentials(dsh_home: &Path) {
    for name in [CREDENTIALS_FILE, SETTINGS_FILE] {
        let _ = std::fs::remove_file(dsh_home.join(name));
    }
}

fn project_cwd(ctx: &SpawnCtx) -> Result<PathBuf, HarnessError> {
    let path = if ctx.project_dir.as_os_str().is_empty() {
        &ctx.cwd
    } else {
        &ctx.project_dir
    };
    if path.as_os_str().is_empty() {
        return Err(HarnessError::SpawnFailed(
            "DSH spawn needs a project directory".into(),
        ));
    }
    if path.is_absolute() {
        Ok(path.clone())
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path))
            .map_err(|e| HarnessError::SpawnFailed(format!("resolve project cwd: {e}")))
    }
}

fn dsh_home(ctx: &SpawnCtx) -> Result<PathBuf, HarnessError> {
    let sid = ctx.sid.trim();
    if sid.is_empty() {
        return Err(HarnessError::SpawnFailed(
            "DSH sessions need a non-empty ccteam sid for isolated DSH_HOME".into(),
        ));
    }
    let root = ccteam_root_from_env().ok_or_else(|| {
        HarnessError::SpawnFailed(
            "cannot resolve CCTEAM_HOME/HOME for managed DSH_HOME".to_string(),
        )
    })?;
    Ok(root.join("runtime").join("dsh").join(sid))
}

fn mirror_dsh_credentials_if_needed(dsh_home: &Path) -> Result<(), HarnessError> {
    if std::env::var(DEEPSEEK_API_KEY_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }

    let home = dirs::home_dir().ok_or_else(|| {
        HarnessError::SpawnFailed(
            "DSH credentials unavailable: DEEPSEEK_API_KEY is not set and HOME is unknown; \
             set DEEPSEEK_API_KEY or run DSH login so ~/.dsh/.credentials.yaml exists"
                .to_string(),
        )
    })?;
    let user_dsh = home.join(USER_DSH_DIR);
    let credentials = user_dsh.join(CREDENTIALS_FILE);
    if !credentials.exists() {
        return Err(HarnessError::SpawnFailed(format!(
            "DSH credentials unavailable: {DEEPSEEK_API_KEY_ENV} is not set and {} does not exist. \
             Set {DEEPSEEK_API_KEY_ENV}, or copy/login credentials with DSH first so ccteam can mirror \
             ~/.dsh/{{{CREDENTIALS_FILE},{SETTINGS_FILE}}} into managed DSH_HOME.",
            credentials.display()
        )));
    }
    copy_user_dsh_file(&credentials, &dsh_home.join(CREDENTIALS_FILE))?;
    let settings = user_dsh.join(SETTINGS_FILE);
    if settings.exists() {
        copy_user_dsh_file(&settings, &dsh_home.join(SETTINGS_FILE))?;
    }
    Ok(())
}

fn copy_user_dsh_file(src: &Path, dst: &Path) -> Result<(), HarnessError> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| {
        HarnessError::SpawnFailed(format!("stat DSH credential source {}: {e}", src.display()))
    })?;
    if meta.file_type().is_symlink() {
        return Err(HarnessError::SpawnFailed(format!(
            "refusing to mirror symlinked DSH credential file {}",
            src.display()
        )));
    }
    if !meta.is_file() {
        return Err(HarnessError::SpawnFailed(format!(
            "DSH credential source is not a regular file: {}",
            src.display()
        )));
    }
    let bytes = std::fs::read(src).map_err(|e| {
        HarnessError::SpawnFailed(format!("read DSH credential source {}: {e}", src.display()))
    })?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| HarnessError::SpawnFailed(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(dst, bytes).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "write DSH credential mirror {}: {e}",
            dst.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dst)
            .map_err(|e| {
                HarnessError::SpawnFailed(format!(
                    "stat DSH credential mirror {}: {e}",
                    dst.display()
                ))
            })?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(dst, perms).map_err(|e| {
            HarnessError::SpawnFailed(format!(
                "chmod 0600 DSH credential mirror {}: {e}",
                dst.display()
            ))
        })?;
    }
    Ok(())
}

fn reject_demo_bin(bin: &str) -> Result<(), HarnessError> {
    let basename = Path::new(bin)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(bin);
    if matches!(basename, "deepseek-harness-acp" | "dsh-acp-demo") {
        return Err(HarnessError::SpawnFailed(format!(
            "refusing to spawn `{basename}`: that is the official DSH ACP demo, not ccteam's `dsh --profile ccteam` runner"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DshVersion {
    major: u64,
    minor: u64,
    patch: u64,
    rc: Option<u64>,
}

/// Spawn-time version gate. Refuses unknown or older DSH builds before the
/// adapter opens the managed stdio transport.
pub async fn verify_dsh_version(bin: &str) -> Result<(), HarnessError> {
    reject_demo_bin(bin)?;
    let output = Command::new(bin)
        .arg("--version")
        .output()
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("run `{bin} --version`: {e}")))?;
    if !output.status.success() {
        return Err(HarnessError::SpawnFailed(format!(
            "`{bin} --version` exited with {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let got = parse_dsh_version(&raw).ok_or_else(|| {
        HarnessError::SpawnFailed(format!(
            "cannot parse DSH version from `{}`; need >= {MIN_DSH_VERSION}",
            raw.trim()
        ))
    })?;
    let min = parse_dsh_version(MIN_DSH_VERSION).expect("pinned DSH floor parses");
    if !version_at_least(got, min) {
        return Err(HarnessError::SpawnFailed(format!(
            "DSH version {} is too old; need >= {MIN_DSH_VERSION}",
            raw.trim()
        )));
    }
    Ok(())
}

fn parse_dsh_version(raw: &str) -> Option<DshVersion> {
    for token in raw.split_whitespace() {
        let token = token.trim_start_matches('v');
        let (core, rc) = match token.split_once("-rc.") {
            Some((core, rc)) => match rc.parse::<u64>() {
                Ok(rc) => (core, Some(rc)),
                Err(_) => continue,
            },
            None => {
                if token.contains('-') {
                    continue;
                }
                (token, None)
            }
        };
        let mut parts = core.split('.');
        let Some(major) = parts.next().and_then(|p| p.parse().ok()) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(|p| p.parse().ok()) else {
            continue;
        };
        let Some(patch) = parts.next().and_then(|p| p.parse().ok()) else {
            continue;
        };
        if parts.next().is_some() {
            continue;
        }
        let version = DshVersion {
            major,
            minor,
            patch,
            rc,
        };
        return Some(version);
    }
    None
}

fn version_at_least(got: DshVersion, min: DshVersion) -> bool {
    let got_core = (got.major, got.minor, got.patch);
    let min_core = (min.major, min.minor, min.patch);
    if got_core != min_core {
        return got_core > min_core;
    }
    match (got.rc, min.rc) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(g), Some(m)) => g >= m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser_accepts_rc_floor_and_newer_stable() {
        let floor = parse_dsh_version(MIN_DSH_VERSION).unwrap();
        assert!(version_at_least(
            parse_dsh_version("0.1.0-rc.6").unwrap(),
            floor
        ));
        assert!(version_at_least(
            parse_dsh_version("dsh 0.1.0-rc.7").unwrap(),
            floor
        ));
        assert!(version_at_least(parse_dsh_version("0.1.0").unwrap(), floor));
        assert!(!version_at_least(
            parse_dsh_version("0.1.0-rc.5").unwrap(),
            floor
        ));
        assert!(!version_at_least(
            parse_dsh_version("0.0.9").unwrap(),
            floor
        ));
        assert!(parse_dsh_version("deepseek harness").is_none());
    }

    #[test]
    fn demo_binary_name_is_rejected() {
        let err = reject_demo_bin("/tmp/deepseek-harness-acp").unwrap_err();
        assert!(err.to_string().contains("official DSH ACP demo"));
    }

    fn make_executable(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\necho stub\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn default_bin_prefers_plain_path_resolution_when_dsh_is_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bindir");
        make_executable(&bin_dir.join("dsh"));
        let path_env = std::ffi::OsString::from(bin_dir.as_os_str());
        // No npx cache at all — this only proves PATH wins when it resolves,
        // not that the fallback is skipped (that's the next test).
        assert_eq!(
            resolve_dsh_default_bin_in(Some(&path_env), None),
            DSH_LITERAL
        );
    }

    #[test]
    fn default_bin_falls_back_to_cached_npx_copy_when_absent_from_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cached = home
            .join(".npm")
            .join("_npx")
            .join("somehash")
            .join("node_modules")
            .join(".bin")
            .join("dsh");
        make_executable(&cached);
        // Empty PATH: bare "dsh" cannot resolve there.
        let empty_path = std::ffi::OsString::from("");
        let resolved = resolve_dsh_default_bin_in(Some(&empty_path), Some(&home));
        assert_eq!(resolved, cached.to_string_lossy());
    }

    #[test]
    fn default_bin_is_literal_dsh_when_neither_path_nor_npx_cache_has_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home"); // no .npm/_npx under it at all
        let empty_path = std::ffi::OsString::from("");
        assert_eq!(
            resolve_dsh_default_bin_in(Some(&empty_path), Some(&home)),
            DSH_LITERAL
        );
    }

    #[test]
    fn cached_npx_lookup_picks_the_most_recently_modified_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let older = home
            .join(".npm")
            .join("_npx")
            .join("older-hash")
            .join("node_modules")
            .join(".bin")
            .join("dsh");
        let newer = home
            .join(".npm")
            .join("_npx")
            .join("newer-hash")
            .join("node_modules")
            .join(".bin")
            .join("dsh");
        make_executable(&older);
        std::thread::sleep(std::time::Duration::from_millis(10));
        make_executable(&newer);
        let found = find_cached_dsh_bin_under(&home).unwrap();
        assert_eq!(found, newer);
    }
}
