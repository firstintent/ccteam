//! Where one identity's DSH lives: the config-source ladder, the per-identity
//! DSH home, the ACP socket path, and the `dsh web` child command.
//!
//! DSH is a `ManagedSessionBridge` vendor and — since v0.10.3 — a SHARED one:
//! ccteam never spawns a DSH child per hire. Each identity has exactly one
//! `dsh web` runtime (owned by
//! [`crate::execution::dsh_runtime::DshRuntimeManager`]) whose embedded ccteam
//! Cordis plugin serves ACP on a unix socket; hires connect to it. Nothing
//! per-session enters through the environment any more — credentials travel in
//! ACP `_meta.ccteam`, one identity per session.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::materialize::{materialize_profile_in, DshPluginConfig, ProfileSpec, WEB_PROFILE};
use crate::{ccteam_root_from_env, HarnessError, SpawnCtx};

pub const DSH_BIN_ENV: &str = "CCTEAM_DSH_BIN";
pub const DSH_WEB_PROFILE: &str = WEB_PROFILE;
pub const DSH_NATIVE_WEB_PROFILE: &str = "web";
pub const DSH_HOME_ENV: &str = "DSH_HOME";
pub const DSH_TELEMETRY_DISABLED_ENV: &str = "DSH_TELEMETRY_DISABLED";
pub const DSH_TELEMETRY_MODE_ENV: &str = "DSH_TELEMETRY_MODE";
pub const DSH_SYSTEM_PROMPT_ENV: &str = "DSH_SYSTEM_PROMPT";
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
pub const DEEPSEEK_BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// TEST-ONLY override: connect straight to this ACP socket and skip the
/// runtime manager entirely. Mirrors the `CCTEAM_{CLAUDE,CODEX}_BIN` precedent
/// (hermetic fakes without a real vendor); production never sets it.
pub const DSH_SOCKET_ENV: &str = "CCTEAM_DSH_SOCKET";

const USER_DSH_DIR: &str = ".dsh";
const OPERATOR_SOCKET_SEGMENT: &str = "operator";
const CREDENTIALS_FILE: &str = ".credentials.yaml";
const SETTINGS_FILE: &str = "settings.yaml";
const SEED_MARKER_FILE: &str = ".ccteam-dsh-seed.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DshSeedMarker {
    credentials_sha256: String,
    settings_sha256: Option<String>,
    seeded_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DshConfigSource {
    Env,
    OperatorHome(PathBuf),
    TenantHome(PathBuf),
    None,
}

pub fn tenant_home_segment(id: &str) -> String {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return id.to_string();
    }
    let hash = Sha256::digest(id.as_bytes());
    let suffix = hash[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("tenant-{suffix}")
}

pub fn dsh_config_source(owner_tag: &str, ccteam_home: &Path) -> DshConfigSource {
    if env_provider_key_is_set() {
        return DshConfigSource::Env;
    }

    let Some(tenant_id) = tenant_id_from_owner_tag(owner_tag) else {
        return operator_config_source();
    };
    let tenant_home = managed_tenant_home(tenant_id, ccteam_home);
    if tenant_home.is_dir() && tenant_home.join(CREDENTIALS_FILE).is_file() {
        return DshConfigSource::TenantHome(tenant_home);
    }
    operator_config_source()
}

/// The DSH home of the identity behind `owner_tag` — the SINGLE source of
/// truth, shared by the runtime manager (which spawns `dsh web` there) and the
/// adapter (which connects to the runtime living there).
///
/// Same lineage as [`dsh_config_source`]: `user:<id>` is a tenant and gets a
/// ccteam-managed home; `user:web-api`, an empty id, and every non-`user:` tag
/// (IM chats) are the operator, who uses their own real `~/.dsh`. One person
/// therefore always resolves to one home no matter which front door they came
/// through.
pub fn identity_dsh_home(owner_tag: &str, ccteam_home: &Path) -> Result<PathBuf, HarnessError> {
    match tenant_id_from_owner_tag(owner_tag) {
        Some(tenant) => Ok(managed_tenant_home(tenant, ccteam_home)),
        None => operator_dsh_home(),
    }
}

/// [`identity_dsh_home`] keyed the way the runtime manager holds an identity
/// (`operator` flag + id) instead of by owner tag.
pub fn dsh_home_for_identity(
    operator: bool,
    id: &str,
    ccteam_home: &Path,
) -> Result<PathBuf, HarnessError> {
    if operator {
        operator_dsh_home()
    } else {
        Ok(managed_tenant_home(id, ccteam_home))
    }
}

/// The unix socket this identity's DSH runtime serves ACP on.
///
/// Deliberately under `<ccteam_home>/runtime/dsh/acp/` and NOT inside the DSH
/// home: the operator's home is vendor-owned, and Linux caps `sun_path` at
/// ~108 bytes — `~/.ccteam/runtime/dsh/acp/<segment>.sock` stays far below it
/// while a nested temp path would not (tests use [`DSH_SOCKET_ENV`]).
pub fn identity_socket_path(owner_tag: &str, ccteam_home: &Path) -> PathBuf {
    let segment = match tenant_id_from_owner_tag(owner_tag) {
        Some(tenant) => tenant_home_segment(tenant),
        None => OPERATOR_SOCKET_SEGMENT.to_string(),
    };
    socket_path_for_segment(&segment, ccteam_home)
}

/// [`identity_socket_path`] keyed like [`dsh_home_for_identity`].
pub fn socket_path_for_identity(operator: bool, id: &str, ccteam_home: &Path) -> PathBuf {
    let segment = if operator {
        OPERATOR_SOCKET_SEGMENT.to_string()
    } else {
        tenant_home_segment(id)
    };
    socket_path_for_segment(&segment, ccteam_home)
}

fn socket_path_for_segment(segment: &str, ccteam_home: &Path) -> PathBuf {
    ccteam_home
        .join("runtime")
        .join("dsh")
        .join("acp")
        .join(format!("{segment}.sock"))
}

/// Create the socket's parent directory 0700 before anything binds in it.
/// The plugin `mkdir -p`s it too, but only ccteam can promise the mode.
pub fn ensure_socket_dir(socket: &Path) -> Result<(), HarnessError> {
    let Some(parent) = socket.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH ACP socket dir {}: {e}",
            parent.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            HarnessError::SpawnFailed(format!("chmod 0700 {}: {e}", parent.display()))
        })?;
    }
    Ok(())
}

/// TEST-ONLY: [`DSH_SOCKET_ENV`], when set to a non-empty path.
pub fn dsh_socket_override() -> Option<PathBuf> {
    std::env::var(DSH_SOCKET_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn managed_tenant_home(tenant_id: &str, ccteam_home: &Path) -> PathBuf {
    ccteam_home
        .join("runtime")
        .join("dsh")
        .join("web")
        .join(tenant_home_segment(tenant_id))
}

fn operator_dsh_home() -> Result<PathBuf, HarnessError> {
    let home = dirs::home_dir().ok_or_else(|| {
        HarnessError::SpawnFailed(
            "cannot resolve HOME for the operator's own DSH home (~/.dsh)".to_string(),
        )
    })?;
    Ok(home.join(USER_DSH_DIR))
}

fn env_provider_key_is_set() -> bool {
    std::env::var(DEEPSEEK_API_KEY_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

pub(crate) fn tenant_id_from_owner_tag(owner_tag: &str) -> Option<&str> {
    match owner_tag.strip_prefix("user:") {
        Some("web-api" | "") | None => None,
        Some(id) => Some(id),
    }
}

fn operator_config_source() -> DshConfigSource {
    let Some(home) = dirs::home_dir() else {
        return DshConfigSource::None;
    };
    let user_dsh = home.join(USER_DSH_DIR);
    if user_dsh.join(CREDENTIALS_FILE).is_file() {
        DshConfigSource::OperatorHome(user_dsh)
    } else {
        DshConfigSource::None
    }
}

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
/// package fetch, no execution. Nothing gets installed here, only found;
/// installs are the admin's explicit one-click action (VENDOR-INSTALL-1,
/// `routes::vendor_install`), never a probe side effect.
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
    /// Retained vendor-memory root. Stopping a runtime must not remove it.
    pub dsh_home: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DshWebSpawnOptions<'a> {
    pub owner_tag: &'a str,
    pub ccteam_home: PathBuf,
    pub dsh_home: PathBuf,
    pub profile: &'a str,
    /// `true` = ccteam owns this whole profile (managed tenant home).
    /// `false` = the operator's own `~/.dsh`: ccteam merges ONLY its own
    /// `@ccteam/ccteam-ui` entries in (gate ①), never a whole profile.
    pub materialize_profile: bool,
    pub enrollment: Option<&'a str>,
    pub daemon_url: Option<&'a str>,
    /// ACP socket the embedded ccteam plugin serves on for this identity.
    pub transport_socket: Option<&'a Path>,
    /// This identity's own ccteam REST bearer (`ccteam:<hex>`), for the ccteam
    /// panel — on both branches (owner decision 2026-08-28: the operator's own
    /// admin web token goes into the operator's own profile; pasting a token
    /// is for a hand-started `dsh web` only). `None` = nothing written and the
    /// panel asks. Enrollment keeps the older line: `None` on the operator
    /// branch.
    pub rest_token: Option<&'a str>,
}

/// Build a `dsh web` child command for one identity's DSH runtime.
///
/// Managed tenant instances use `profile = ccteam-web` and
/// `materialize_profile = true`; the operator uses the vendor-native `web`
/// profile in the real `~/.dsh`, where ccteam registers only its own plugin
/// row (merge-only) so the human's profile keeps working untouched.
pub fn build_web_spawn_spec(options: DshWebSpawnOptions<'_>) -> Result<DshSpawnSpec, HarnessError> {
    let bin = dsh_bin();
    reject_demo_bin(&bin)?;
    std::fs::create_dir_all(&options.dsh_home).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH web home {}: {e}",
            options.dsh_home.display()
        ))
    })?;
    let socket = options
        .transport_socket
        .map(|path| path.to_string_lossy().into_owned());
    if let Some(path) = options.transport_socket {
        ensure_socket_dir(path)?;
    }
    // ccteam's one row: this daemon's URL for every face, the enrollment
    // credential and socket for the tool + transport faces, and — when the
    // runtime resolved one — this identity's REST bearer for the workbench
    // (see `DshWebSpawnOptions::rest_token`).
    let config = DshPluginConfig {
        daemon_url: options.daemon_url,
        enrollment: options.enrollment,
        transport_socket: socket.as_deref(),
        rest_token: options.rest_token,
    };
    if options.materialize_profile {
        seed_or_refresh_tenant_web_config_home(
            options.owner_tag,
            &options.ccteam_home,
            &options.dsh_home,
        )?;
        materialize_profile_in(
            &options.ccteam_home,
            &options.dsh_home,
            ProfileSpec::web(config),
        )?;
    } else {
        super::materialize::register_ccteam_plugins_into_profile(
            &options.ccteam_home,
            &options.dsh_home,
            options.profile,
            config,
        )?;
    }

    let mut env = vec![
        (
            DSH_HOME_ENV.to_string(),
            options.dsh_home.to_string_lossy().into_owned(),
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

    Ok(DshSpawnSpec {
        bin,
        args: vec![
            "--profile".to_string(),
            options.profile.to_string(),
            "--port".to_string(),
            "0".to_string(),
        ],
        env,
        env_remove: Vec::new(),
        cwd: options.dsh_home.clone(),
        dsh_home: options.dsh_home,
    })
}

fn seed_or_refresh_tenant_web_config_home(
    owner_tag: &str,
    ccteam_home: &Path,
    dsh_home: &Path,
) -> Result<(), HarnessError> {
    if tenant_id_from_owner_tag(owner_tag).is_none() {
        return Ok(());
    }

    let marker_path = dsh_home.join(SEED_MARKER_FILE);
    if marker_path.exists() {
        let mut marker = read_seed_marker(&marker_path)?;
        if let DshConfigSource::OperatorHome(source_home) = operator_config_source() {
            refresh_seeded_file(
                &source_home.join(CREDENTIALS_FILE),
                &dsh_home.join(CREDENTIALS_FILE),
                &mut marker.credentials_sha256,
            )?;
            refresh_seeded_optional_file(
                &source_home.join(SETTINGS_FILE),
                &dsh_home.join(SETTINGS_FILE),
                &mut marker.settings_sha256,
            )?;
            write_seed_marker(&marker_path, &marker)?;
        }
        return Ok(());
    }

    if dsh_home.join(CREDENTIALS_FILE).exists() || dsh_home.join(SETTINGS_FILE).exists() {
        return Ok(());
    }

    if let DshConfigSource::OperatorHome(source_home) = dsh_config_source(owner_tag, ccteam_home) {
        seed_tenant_web_config_home(&source_home, dsh_home, &marker_path)?;
    }
    Ok(())
}

fn seed_tenant_web_config_home(
    source_home: &Path,
    dsh_home: &Path,
    marker_path: &Path,
) -> Result<(), HarnessError> {
    let credentials = source_home.join(CREDENTIALS_FILE);
    if !credentials.exists() {
        return Ok(());
    }
    let credentials_hash = copy_user_dsh_file(&credentials, &dsh_home.join(CREDENTIALS_FILE))?;
    let settings = source_home.join(SETTINGS_FILE);
    let settings_sha256 = if settings.exists() {
        Some(copy_user_dsh_file(
            &settings,
            &dsh_home.join(SETTINGS_FILE),
        )?)
    } else {
        None
    };
    write_seed_marker(
        marker_path,
        &DshSeedMarker {
            credentials_sha256: credentials_hash,
            settings_sha256,
            seeded_at: Utc::now().to_rfc3339(),
        },
    )
}

fn refresh_seeded_file(
    source: &Path,
    target: &Path,
    marker_sha: &mut String,
) -> Result<(), HarnessError> {
    let Ok(current) = std::fs::read(target) else {
        return Ok(());
    };
    if sha256_hex(&current) != *marker_sha || !source.exists() {
        return Ok(());
    }
    *marker_sha = copy_user_dsh_file(source, target)?;
    Ok(())
}

fn refresh_seeded_optional_file(
    source: &Path,
    target: &Path,
    marker_sha: &mut Option<String>,
) -> Result<(), HarnessError> {
    match (marker_sha.as_ref(), std::fs::read(target)) {
        (Some(expected), Ok(current)) if sha256_hex(&current) == *expected && source.exists() => {
            *marker_sha = Some(copy_user_dsh_file(source, target)?);
        }
        (None, Err(_)) if source.exists() => {
            *marker_sha = Some(copy_user_dsh_file(source, target)?);
        }
        _ => {}
    }
    Ok(())
}

fn read_seed_marker(path: &Path) -> Result<DshSeedMarker, HarnessError> {
    let raw = std::fs::read(path).map_err(|e| {
        HarnessError::SpawnFailed(format!("read DSH seed marker {}: {e}", path.display()))
    })?;
    let marker = serde_json::from_slice(&raw).map_err(|e| {
        HarnessError::SpawnFailed(format!("parse DSH seed marker {}: {e}", path.display()))
    })?;
    Ok(marker)
}

fn write_seed_marker(path: &Path, marker: &DshSeedMarker) -> Result<(), HarnessError> {
    let bytes = serde_json::to_vec(marker)
        .map_err(|e| HarnessError::SpawnFailed(format!("serialize DSH seed marker: {e}")))?;
    std::fs::write(path, bytes).map_err(|e| {
        HarnessError::SpawnFailed(format!("write DSH seed marker {}: {e}", path.display()))
    })
}

pub(crate) fn project_cwd(ctx: &SpawnCtx) -> Result<PathBuf, HarnessError> {
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

pub(crate) fn ccteam_root() -> Result<PathBuf, HarnessError> {
    ccteam_root_from_env().ok_or_else(|| {
        HarnessError::SpawnFailed(
            "cannot resolve CCTEAM_HOME/HOME for managed DSH_HOME".to_string(),
        )
    })
}

fn copy_user_dsh_file(src: &Path, dst: &Path) -> Result<String, HarnessError> {
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
    let hash = sha256_hex(&bytes);
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
    Ok(hash)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn reject_demo_bin(bin: &str) -> Result<(), HarnessError> {
    let basename = Path::new(bin)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(bin);
    if matches!(basename, "deepseek-harness-acp" | "dsh-acp-demo") {
        return Err(HarnessError::SpawnFailed(format!(
            "refusing to spawn `{basename}`: that is the official DSH ACP demo, not the `dsh web` runtime ccteam connects to"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every env-mutating DSH spawn-spec test lives in
    // `tests/dsh_acp_test.rs` (AGENTS.md §五): the lib target is one process
    // at full parallelism, so a `HOME`/`DEEPSEEK_API_KEY` write here is
    // visible to every other lib test. What stays below is pure or injected.

    #[test]
    fn identity_socket_path_is_one_short_path_per_identity() {
        let ccteam_home = Path::new("/srv/ccteam-home");
        assert_eq!(
            identity_socket_path("user:alice", ccteam_home),
            ccteam_home.join("runtime/dsh/acp/alice.sock")
        );
        assert_eq!(
            identity_socket_path("user:alice", ccteam_home),
            socket_path_for_identity(false, "alice", ccteam_home)
        );
        for tag in ["user:web-api", "telegram:123", ""] {
            assert_eq!(
                identity_socket_path(tag, ccteam_home),
                ccteam_home.join("runtime/dsh/acp/operator.sock"),
                "`{tag}` shares the operator's single runtime and socket"
            );
        }
        assert_eq!(
            socket_path_for_identity(true, "admin", ccteam_home),
            ccteam_home.join("runtime/dsh/acp/operator.sock")
        );
        // Unsafe ids are hashed by `tenant_home_segment`, so the socket name
        // can never escape the acp directory.
        let odd = identity_socket_path("user:../../etc", ccteam_home);
        assert_eq!(odd.parent().unwrap(), ccteam_home.join("runtime/dsh/acp"));
        assert!(odd
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tenant-"));
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
