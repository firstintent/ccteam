//! Optional cswap launcher integration. Claude/cswap own credentials; this
//! module handles only account references, argv, and profile metadata (#229).

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{fs_atomic::atomic_write_durable, transcript_tail::anthropic_project_dir_in};

const RELAY_ENV: &str = "CCTEAM_CSWAP_LAUNCH";
const REAL_BIN_ENV: &str = "CCTEAM_CSWAP_CLAUDE";
const ORIGINAL_PATH_ENV: &str = "CCTEAM_CSWAP_PATH";
const MAX_DISCOVERY_BYTES: usize = 1024 * 1024;

/// Safe to return to any authenticated frontend. No credentials or profile paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CswapAccount {
    pub id: String,
    pub slot: String,
    /// cswap alias, else the email's local part. The domain never leaves
    /// the daemon (owner 2026-11-08: number + name + quota only).
    pub label: String,
    pub active: bool,
    /// Rolling 5-hour window, whole percent. Absent for plans without one
    /// (Max) and whenever cswap has no reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_pct: Option<u16>,
    /// Weekly window, whole percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_pct: Option<u16>,
    /// cswap's `usageStatus` when it is not `ok` (e.g. `relogin_required`).
    /// With percentages present they are cswap's last good reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_status: Option<String>,
}

/// Private session metadata. `id` is the account the session is bound to;
/// `config_dir` / `default_login` record the Claude profile a launch ran in.
///
/// The profile is cswap's decision at every launch, not part of the binding
/// (docs-local issue #240): cswap keeps ONE live credential copy per account,
/// so an account runs in its own session profile while another account is
/// the default login and on the default login itself once it becomes the
/// default. A session therefore follows its account across the two; history
/// is shared, so the transcript is readable from both.
///
/// `config_dir` is the literal path exported by cswap (its spelling also
/// selects the macOS Keychain service). Persisted `default_login` means the
/// session was created on the default login (the account picked was the
/// default one) and follows it from then on, account switches included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeAccountBinding {
    pub id: String,
    #[serde(default)]
    pub slot: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub config_dir: Option<PathBuf>,
    #[serde(default)]
    pub default_login: bool,
}

impl ClaudeAccountBinding {
    pub fn requested(id: String) -> Result<Self> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid cswap account reference; choose an account from the account list");
        }
        Ok(Self {
            id,
            slot: String::new(),
            label: String::new(),
            config_dir: None,
            default_login: false,
        })
    }

    /// Whether a process launched as `launched` (what the relay confirmed) is
    /// no longer in the profile cswap picks for this binding now that the
    /// default login is `default_login_id` (docs-local issue #240).
    ///
    /// Two shapes, both for a session pinned to an account (`self` is the
    /// persisted binding):
    /// - the process runs in the account's session profile and the account
    ///   has become the default login: two live copies of one credential,
    ///   which is what makes `cswap switch` warn and one copy's token go
    ///   stale once the server rotates it;
    /// - the process runs on the default login and that login now belongs to
    ///   another account: it is spending the wrong account's quota.
    ///
    /// Releasing the process fixes either: the next launch asks cswap again.
    /// An unknown default login (`None`) is never evidence of a move.
    pub fn launch_profile_moved(
        &self,
        launched: &ClaudeAccountBinding,
        default_login_id: Option<&str>,
    ) -> bool {
        if self.default_login || launched.config_dir.is_none() {
            return false;
        }
        default_login_id.is_some_and(|id| launched.default_login != (id == self.id))
    }

    pub fn display(&self) -> CswapAccount {
        CswapAccount {
            id: self.id.clone(),
            slot: self.slot.clone(),
            label: self.label.clone(),
            active: self.default_login,
            five_hour_pct: None,
            seven_day_pct: None,
            usage_status: None,
        }
    }
}

/// Whether a sid's Claude transcript stays readable after the session moves
/// to another account (#236). cswap's shared history links every profile's
/// `projects/` to `~/.claude/projects`; a transcript that exists only in one
/// account's private profile (written before sharing, or while cswap
/// deferred merging it) would be invisible elsewhere, and resuming there
/// would start the conversation over.
pub fn history_follows_switch(cwd: &Path, uuid: &str, from: Option<&ClaudeAccountBinding>) -> bool {
    dirs::home_dir().is_some_and(|home| {
        history_follows_switch_in(&home.join(".claude").join("projects"), cwd, uuid, from)
    })
}

/// [`history_follows_switch`] against an explicit shared projects root.
pub fn history_follows_switch_in(
    shared_projects: &Path,
    cwd: &Path,
    uuid: &str,
    from: Option<&ClaudeAccountBinding>,
) -> bool {
    let file = format!("{uuid}.jsonl");
    if anthropic_project_dir_in(shared_projects, cwd)
        .join(&file)
        .exists()
    {
        return true;
    }
    // Not shared: only safe when the current profile holds no history either.
    from.and_then(|binding| binding.config_dir.as_ref())
        .is_none_or(|dir| {
            !anthropic_project_dir_in(&dir.join("projects"), cwd)
                .join(&file)
                .exists()
        })
}

/// `#<slot> <name>` — how an account reads on every surface (no email domain).
pub fn display_name(account: &CswapAccount) -> String {
    format!("#{} {}", account.slot, account.label)
}

pub fn account_id(email: &str, organization: &str) -> String {
    let identity = serde_json::to_vec(&(email, organization)).expect("string pair serializes");
    format!("{:x}", Sha256::digest(identity))
}

/// The [`account_id`] of Claude's default login, or `None` when nobody is
/// logged in there or its record cannot be read.
///
/// Read from the default profile's own `oauthAccount` record, the file cswap
/// compares against when it decides between a session profile and the
/// default login. No credential is read, and no cswap call is made: `cswap
/// list` may fetch quota over the network, which a residency pass must not.
pub fn default_login_account_id() -> Option<String> {
    default_login_account_id_in(&dirs::home_dir()?)
}

/// [`default_login_account_id`] against an explicit home directory.
pub fn default_login_account_id_in(home: &Path) -> Option<String> {
    // Claude Code's own choice of global config: the legacy
    // `~/.claude/.config.json` when it holds anything, else `~/.claude.json`.
    // `CLAUDE_CONFIG_DIR` is deliberately ignored: it names a session profile.
    let legacy = home.join(".claude").join(".config.json");
    let path = match std::fs::metadata(&legacy) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => legacy,
        _ => home.join(".claude.json"),
    };
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let account = config.get("oauthAccount")?;
    let email = account["emailAddress"].as_str().filter(|e| !e.is_empty())?;
    Some(account_id(
        email,
        account["organizationUuid"].as_str().unwrap_or(""),
    ))
}

pub fn parse_accounts(bytes: &[u8]) -> Result<Vec<CswapAccount>> {
    if bytes.len() > MAX_DISCOVERY_BYTES {
        bail!("cswap account list is too large");
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("cswap did not return a JSON account list")?;
    if value["schemaVersion"].as_u64() != Some(1) {
        bail!("unsupported cswap account-list schema");
    }
    let rows = value["accounts"]
        .as_array()
        .context("cswap account list is missing accounts")?;
    let mut accounts = BTreeMap::new();
    for row in rows {
        // API-key accounts have no subscription quota and run does not support them.
        if row["usageStatus"].as_str() == Some("api_key") {
            continue;
        }
        let email = row["email"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("cswap account has no identity")?;
        let org = row["organizationUuid"].as_str().unwrap_or("");
        let slot = row["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("cswap account has no slot")?
            .to_string();
        let label = row["alias"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| email.split_once('@').map_or(email, |(name, _)| name))
            .to_string();
        // Live reading first; cswap keeps the last good one when a fetch fails.
        let usage = Some(&row["usage"])
            .filter(|u| u.is_object())
            .or_else(|| Some(&row["lastGoodUsage"]).filter(|u| u.is_object()));
        let pct = |window: &str| {
            usage
                .and_then(|u| u[window]["pct"].as_f64())
                .filter(|p| p.is_finite())
                .map(|p| p.clamp(0.0, 999.0).round() as u16)
        };
        let usage_status = row["usageStatus"]
            .as_str()
            .filter(|status| *status != "ok")
            .map(str::to_string);
        let id = account_id(email, org);
        if accounts
            .insert(
                id.clone(),
                CswapAccount {
                    id,
                    slot,
                    label,
                    active: row["active"].as_bool().unwrap_or(false),
                    five_hour_pct: pct("fiveHour"),
                    seven_day_pct: pct("sevenDay"),
                    usage_status,
                },
            )
            .is_some()
        {
            bail!("cswap contains duplicate account identities; repair its account list first");
        }
    }
    let mut accounts: Vec<_> = accounts.into_values().collect();
    accounts.sort_by_key(|account| account.slot.parse::<u64>().unwrap_or(u64::MAX));
    Ok(accounts)
}

pub fn cswap_bin() -> OsString {
    std::env::var_os("CCTEAM_CSWAP_BIN").unwrap_or_else(|| "cswap".into())
}

/// Called only by optional discovery or an explicitly selected account. Never
/// part of daemon readiness or a normal/default Claude spawn.
pub async fn discover_accounts() -> Result<Vec<CswapAccount>> {
    let help = cswap_output(&["run", "--help"], Duration::from_secs(5)).await?;
    let help = String::from_utf8_lossy(&help);
    if !help.contains("--share-history") {
        bail!("this cswap version lacks shared history; upgrade cswap or use default login");
    }
    // Newer cswap versions optionally infer setup-token quota using a tiny
    // model request. Discovery must not create inference outside a session.
    verify_cswap_settings(
        &cswap_output(&["config", "list", "--json"], Duration::from_secs(5)).await?,
    )?;
    parse_accounts(&cswap_output(&["list", "--json"], Duration::from_secs(20)).await?)
}

fn verify_cswap_settings(bytes: &[u8]) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("cswap settings discovery returned invalid JSON")?;
    if value["schemaVersion"].as_u64() != Some(1) {
        bail!("unsupported cswap settings schema");
    }
    let settings = value["settings"]
        .as_array()
        .context("cswap settings discovery has no settings")?;
    for setting in settings {
        if setting["key"].as_str() == Some("usage.headerProbe")
            && setting["value"].as_bool() != Some(false)
        {
            bail!("disable cswap usage.headerProbe before account discovery: ccteam does not run quota inference probes");
        }
    }
    Ok(())
}

async fn cswap_output(args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new(cswap_bin());
    command
        .args(args)
        .env_remove("CLAUDE_CONFIG_DIR")
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .context("cswap account discovery timed out; default login remains available")?
        .context("cswap is not installed or cannot run; install cswap to select an account")?;
    if !output.status.success() {
        bail!("cswap account discovery failed; check cswap list in the daemon user's environment");
    }
    Ok(output.stdout)
}

/// Resolve what a person typed (`2`, `#2`, an alias / name, or a stable id)
/// to one listed account (#236, IM `/account`). A name matching several
/// accounts resolves to none: the slot number is the unambiguous handle.
pub fn find_account<'a>(accounts: &'a [CswapAccount], reference: &str) -> Option<&'a CswapAccount> {
    let reference = reference.trim();
    let slot = reference.strip_prefix('#').unwrap_or(reference);
    if let Some(account) = accounts
        .iter()
        .find(|account| account.slot == slot || account.id == reference)
    {
        return Some(account);
    }
    let mut named = accounts
        .iter()
        .filter(|account| account.label.eq_ignore_ascii_case(reference));
    match (named.next(), named.next()) {
        (Some(account), None) => Some(account),
        _ => None,
    }
}

/// Discovery is explicit and isolated from readiness/status. Coalesce open
/// picker requests and briefly cache failures as well as successful results.
/// One instance serves both web and IM (the daemon hands the gateway's to
/// the web state), so they share one cache.
#[derive(Default)]
pub struct CswapAccountService {
    cache: tokio::sync::Mutex<Option<CachedAccounts>>,
    /// Test seam: answer with these accounts instead of running cswap.
    fixed: Option<Vec<CswapAccount>>,
}

struct CachedAccounts {
    captured: std::time::Instant,
    result: std::result::Result<Vec<CswapAccount>, String>,
}

impl CswapAccountService {
    /// A service that lists `accounts` without running cswap (tests).
    pub fn fixed(accounts: Vec<CswapAccount>) -> Self {
        Self {
            cache: Default::default(),
            fixed: Some(accounts),
        }
    }

    pub async fn accounts(&self) -> Result<Vec<CswapAccount>> {
        if let Some(accounts) = &self.fixed {
            return Ok(accounts.clone());
        }
        let mut cache = self.cache.lock().await;
        if let Some(cached) = &*cache {
            let ttl = if cached.result.is_ok() { 30 } else { 5 };
            if cached.captured.elapsed() < Duration::from_secs(ttl) {
                return cached.result.clone().map_err(anyhow::Error::msg);
            }
        }
        let result = discover_accounts()
            .await
            .map_err(|error| format!("{error:#}"));
        *cache = Some(CachedAccounts {
            captured: std::time::Instant::now(),
            result: result.clone(),
        });
        result.map_err(anyhow::Error::msg)
    }
}

/// The relay performs the final profile check and fresh/resume decision just
/// before exec. This avoids a second preparatory run and a credential race.
#[derive(Debug, Serialize, Deserialize)]
struct LaunchRequest {
    binding: ClaudeAccountBinding,
    uuid: String,
    cwd: PathBuf,
}

pub struct CswapLaunch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    directory: PathBuf,
}

impl Drop for CswapLaunch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl CswapLaunch {
    pub async fn prepare(
        binding: &ClaudeAccountBinding,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        chat_dir: &Path,
        uuid: &str,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("cswap accounts currently require Linux, WSL or macOS");
        let mut binding = binding.clone();
        if !binding.default_login {
            let account = discover_accounts()
                .await?
                .into_iter()
                .find(|a| a.id == binding.id)
                .context(
                    "selected cswap account no longer exists; choose an account for a new session",
                )?;
            binding.slot = account.slot;
            binding.label = account.label;
        }
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce)
            .map_err(|error| anyhow::anyhow!("cswap launch nonce: {error}"))?;
        let nonce = format!("{:x}", Sha256::digest(nonce));
        let directory = chat_dir.join(format!("cswap-launch-{nonce}"));
        let bin_dir = directory.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut launch = Self {
            argv: Vec::new(),
            env: env.to_vec(),
            directory,
        };
        let request = LaunchRequest {
            binding: binding.clone(),
            uuid: uuid.to_string(),
            cwd: cwd.to_path_buf(),
        };
        atomic_write_durable(
            &launch.directory.join("request.json"),
            &serde_json::to_vec(&request)?,
        )?;
        let relay = relay_bin()?;
        let quoted = relay.to_string_lossy().replace('\'', "'\\''");
        let script = format!("#!/bin/sh\nexec '{quoted}' internal claude-account-launch \"$@\"\n");
        let entry = bin_dir.join("claude");
        std::fs::write(&entry, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700))?;
        }
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&original_path));
        launch.env.extend([
            (
                "PATH".into(),
                std::env::join_paths(paths)?.to_string_lossy().into_owned(),
            ),
            (
                ORIGINAL_PATH_ENV.into(),
                original_path.to_string_lossy().into_owned(),
            ),
            (
                REAL_BIN_ENV.into(),
                argv.first().context("missing Claude binary")?.clone(),
            ),
            (
                RELAY_ENV.into(),
                launch.directory.to_string_lossy().into_owned(),
            ),
        ]);
        // A prior default fallback stays a default-profile session that follows
        // cswap auto; re-running its old slot would quietly pin it instead.
        if binding.default_login {
            launch.argv = vec![entry.to_string_lossy().into_owned()];
        } else {
            // cswap's shared history (its default since 1.0) links every
            // profile's `projects/` to `~/.claude/projects`, so a sid's
            // transcript is readable from any account and the session can
            // switch accounts by sid (#236). Credentials stay per profile.
            // Explicit, so a user-wide `run.shareHistory false` cannot strand
            // a switched session's history in one account.
            //
            // No `--require-session`: when the account is the default login
            // cswap runs it there instead of in its session profile, so the
            // account never has two live credential copies. Insisting on the
            // session profile made the sid unresumable for as long as its
            // account stayed the default (docs-local issue #240).
            launch.argv = vec![
                cswap_bin().to_string_lossy().into_owned(),
                "run".into(),
                binding.slot,
                "--share-history".into(),
                "--".into(),
            ];
        }
        launch.argv.extend_from_slice(&argv[1..]);
        Ok(launch)
    }

    /// Why the relay refused this launch, if it did.
    pub fn failure(&self) -> Option<String> {
        std::fs::read_to_string(self.directory.join(FAILURE_FILE))
            .ok()
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty())
    }

    pub fn binding(&self) -> Result<ClaudeAccountBinding> {
        serde_json::from_slice(&std::fs::read(self.directory.join("binding.json"))?)
            .context("cswap launch did not confirm its profile")
    }
}

/// The ccteam binary the launch entry script execs as the relay
/// (`CCTEAM_CSWAP_RELAY_BIN` overrides it for integration tests, whose
/// current exe is the test runner).
///
/// The relay only runs while the daemon is spawning the session, so it is
/// the daemon's own running image, `/proc/<pid>/exe` on Linux: the kernel
/// keeps that link executable after an upgrade unlinks the file, and the
/// relay reading the launch request is exactly the version that wrote it.
/// Raw `current_exe()` there names `<path> (deleted)` once the binary is
/// replaced, so every account launch died with a bare "child exited" until
/// the daemon restarted (docs-local issue #238). Unlike
/// `ccteam_core::current_ccteam_bin`, this path dies with the daemon: never
/// persist it.
fn relay_bin() -> Result<PathBuf> {
    if let Some(bin) = std::env::var_os("CCTEAM_CSWAP_RELAY_BIN") {
        return Ok(PathBuf::from(bin));
    }
    match process_image(std::process::id()) {
        Some(image) => Ok(image),
        None => Ok(std::env::current_exe()?),
    }
}

/// `/proc/<pid>/exe` where the platform exposes it.
fn process_image(pid: u32) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let image = PathBuf::from(format!("/proc/{pid}/exe"));
    image.exists().then_some(image)
}

/// Hidden CLI entry: do not initialize daemon state or consume stdin. cswap
/// may call `claude auth status` before it execs the actual stream-json child.
pub fn run_relay(args: &[OsString]) -> Result<()> {
    let directory =
        PathBuf::from(std::env::var_os(RELAY_ENV).context("cswap relay is not configured")?);
    let result = relay(&directory, args);
    // The daemon discards a launch's stderr (Claude's debug stream), so a
    // refusal leaves its reason where the launch can read it back and say
    // why the session did not start. Reasons never carry credentials.
    if let Err(error) = &result {
        let _ = std::fs::write(directory.join(FAILURE_FILE), format!("{error:#}"));
    }
    result
}

const FAILURE_FILE: &str = "failure.txt";

fn relay(directory: &Path, args: &[OsString]) -> Result<()> {
    let request: LaunchRequest =
        serde_json::from_slice(&std::fs::read(directory.join("request.json"))?)?;
    let real_bin = std::env::var_os(REAL_BIN_ENV).context("cswap relay has no Claude binary")?;
    let mut command = std::process::Command::new(&real_bin);
    command.env(
        "PATH",
        std::env::var_os(ORIGINAL_PATH_ENV).unwrap_or_default(),
    );
    command
        .env_remove(RELAY_ENV)
        .env_remove(REAL_BIN_ENV)
        .env_remove(ORIGINAL_PATH_ENV);
    let is_stream_json = args.iter().any(|arg| arg == "--input-format");
    if !is_stream_json {
        // The SDK initialize request can already be queued in inherited
        // stdin while cswap probes auth. Probes must never consume it.
        command.stdin(std::process::Stdio::null());
        return exec(command, args);
    }
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .context("Claude profile cannot be resolved")?;
    let default_login = std::env::var_os("CLAUDE_CONFIG_DIR").is_none();
    verify_subscription_settings(&config_dir, &request.cwd)?;
    let mut binding = request.binding;
    // Which profile cswap chose is not checked against the previous launch:
    // an account moves between its session profile and the default login as
    // the default login changes (docs-local issue #240). What a launch must
    // never do is run another account or start the conversation over, and
    // both are checked below against the profile actually in use.
    // A selected subscription must not be hijacked by ambient API credentials,
    // including cswap's default-account fast path which retains the full env.
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    ] {
        command.env_remove(key);
    }
    for key in [
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        if std::env::var_os(key)
            .is_some_and(|value| override_is_active(key, &value.to_string_lossy()))
        {
            bail!("selected cswap subscription conflicts with {key}; remove the provider override first");
        }
    }
    let mut probe = std::process::Command::new(&real_bin);
    probe.args(["auth", "status", "--json"]).env(
        "PATH",
        std::env::var_os(ORIGINAL_PATH_ENV).unwrap_or_default(),
    );
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            probe.env(key, value);
        } else {
            probe.env_remove(key);
        }
    }
    let status = bounded_auth_probe(probe)?;
    let identity: serde_json::Value = serde_json::from_slice(&status.stdout)
        .context("Claude auth status returned invalid JSON")?;
    if !status.status.success()
        || identity["loggedIn"].as_bool() != Some(true)
        || identity["authMethod"].as_str() != Some("claude.ai")
    {
        bail!("selected Claude profile is not logged into a subscription; use cswap to repair the login");
    }
    if !binding.default_login {
        let email = identity["email"]
            .as_str()
            .context("Claude auth status has no account identity")?;
        let org = identity["orgId"].as_str().unwrap_or("");
        if account_id(email, org) != binding.id {
            bail!("cswap account identity changed; refusing to use another account");
        }
    }
    binding.config_dir = Some(config_dir.clone());
    binding.default_login = default_login;
    let transcript = anthropic_project_dir_in(&config_dir.join("projects"), &request.cwd)
        .join(format!("{}.jsonl", request.uuid));
    let mut args = args.to_vec();
    let flag = args
        .iter()
        .position(|arg| arg == "--session-id" || arg == "--resume")
        .context("cswap relay has no session UUID argument")?;
    let has_transcript = transcript.exists();
    // The daemon asks for `--resume` only when it saw this sid's transcript.
    // Not finding it in the profile cswap prepared (history sharing deferred
    // while another Claude still used that profile's own history) must not
    // silently start the conversation over (#236).
    if args[flag] == "--resume" && !has_transcript {
        bail!(
            "this conversation's history is not visible in the selected account's profile yet \
             (cswap shares history once no other Claude uses that profile); the session and \
             its history were kept, retry later or switch back"
        );
    }
    args[flag] = if has_transcript {
        "--resume"
    } else {
        "--session-id"
    }
    .into();
    atomic_write_durable(
        &directory.join("binding.json"),
        &serde_json::to_vec(&binding)?,
    )?;
    exec(command, &args)
}

fn verify_subscription_settings(config_dir: &Path, cwd: &Path) -> Result<()> {
    for path in [
        config_dir.join("settings.json"),
        cwd.join(".claude/settings.json"),
        cwd.join(".claude/settings.local.json"),
    ] {
        let body = match std::fs::read(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => bail!("could not verify Claude settings for the selected subscription"),
        };
        let settings: serde_json::Value = serde_json::from_slice(&body)
            .context("selected subscription has invalid Claude settings")?;
        if settings
            .get("apiKeyHelper")
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
        {
            bail!(
                "selected subscription conflicts with apiKeyHelper; remove the auth helper first"
            );
        }
        if let Some(env) = settings["env"].as_object() {
            for key in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
                "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ] {
                if env.get(key).is_some_and(|value| match value {
                    serde_json::Value::Null => false,
                    serde_json::Value::String(value) => override_is_active(key, value),
                    serde_json::Value::Bool(false) => !is_cloud_switch(key),
                    _ => true,
                }) {
                    bail!("selected subscription conflicts with {key} in Claude settings");
                }
            }
        }
    }
    Ok(())
}

fn is_cloud_switch(key: &str) -> bool {
    matches!(
        key,
        "CLAUDE_CODE_USE_BEDROCK" | "CLAUDE_CODE_USE_VERTEX" | "CLAUDE_CODE_USE_FOUNDRY"
    )
}

fn override_is_active(key: &str, value: &str) -> bool {
    // "0" disables a boolean cloud switch, but is a real stdin descriptor
    // for FD authentication; an arbitrary URL/key is never a boolean flag.
    !value.is_empty() && (!is_cloud_switch(key) || !matches!(value, "0" | "false"))
}

fn bounded_auth_probe(mut command: std::process::Command) -> Result<std::process::Output> {
    use std::io::Read;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command
        .spawn()
        .context("could not verify selected Claude login")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Claude auth status timed out; the profile and its history were kept");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .context("missing auth status output")?
        .take(64 * 1024)
        .read_to_end(&mut stdout)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn exec(mut command: std::process::Command, args: &[OsString]) -> Result<()> {
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec()).context("could not execute Claude")
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #238: after an upgrade unlinks a running binary, its image still runs
    /// through `/proc/<pid>/exe`, while the `current_exe()` spelling
    /// (`<path> (deleted)`) does not. A copied `sleep` stands in for the
    /// daemon, so the test runner's own ELF is never touched.
    #[cfg(target_os = "linux")]
    #[test]
    fn process_image_runs_after_the_binary_is_deleted() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("daemon-standin");
        std::fs::copy("/bin/sleep", &bin).unwrap();
        let mut running = Command::new(&bin).arg("30").spawn().unwrap();
        // `spawn` can return a moment before the child's exec has replaced
        // its image: the kernel wakes a vfork parent while the child still
        // points at the image it was cloned from, so `/proc/<pid>/exe` names
        // this test binary until the exec lands. Wait for the stand-in.
        let exe = format!("/proc/{}/exe", running.id());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::fs::read_link(&exe).ok().as_deref() != Some(bin.as_path()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the stand-in never started"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(&bin).unwrap();

        let reported = std::fs::read_link(&exe).unwrap();
        let version = |path: &Path| {
            Command::new(path)
                .arg("--version")
                .stdout(Stdio::null())
                .status()
        };
        let deleted = version(&reported);
        let image = process_image(running.id()).expect("linux exposes /proc/<pid>/exe");
        let through_image = version(&image);
        let _ = running.kill();
        let _ = running.wait();

        assert!(
            reported.to_string_lossy().ends_with(" (deleted)"),
            "{}",
            reported.display()
        );
        assert!(deleted.is_err(), "the (deleted) spelling must not run");
        assert!(through_image.unwrap().success(), "{}", image.display());
    }

    /// #238: the launch entry script must exec the daemon's running image,
    /// not `current_exe()` — after an upgrade the latter is `<path>
    /// (deleted)` and the launch dies before Claude starts. A default-login
    /// binding prepares without running cswap discovery.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn launch_entry_execs_the_running_image() {
        if std::env::var_os("CCTEAM_CSWAP_RELAY_BIN").is_some() {
            return; // explicit override wins; nothing to assert here
        }
        let dir = tempfile::tempdir().unwrap();
        let binding = ClaudeAccountBinding {
            id: "0".repeat(64),
            slot: String::new(),
            label: String::new(),
            config_dir: None,
            default_login: true,
        };
        let argv = vec!["claude".to_string(), "--verbose".to_string()];
        let launch = CswapLaunch::prepare(
            &binding,
            &argv,
            &[],
            dir.path(),
            &dir.path().join("chat"),
            "uuid",
        )
        .await
        .unwrap();
        let script = std::fs::read_to_string(&launch.argv[0]).unwrap();
        let image = format!("/proc/{}/exe", std::process::id());
        assert!(
            script.contains(&format!("exec '{image}' internal claude-account-launch")),
            "{script}"
        );
        assert_eq!(&launch.argv[1..], &argv[1..]);
    }

    /// #240: the default login's identity is read the way cswap reads it, so
    /// "is this account the default login" is the same answer on both sides.
    #[test]
    fn default_login_identity_comes_from_the_default_profile_record() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert_eq!(default_login_account_id_in(home), None, "nobody logged in");
        let record = |email: &str, org: &str| {
            format!(r#"{{"oauthAccount":{{"emailAddress":"{email}","organizationUuid":"{org}"}}}}"#)
        };
        std::fs::write(home.join(".claude.json"), record("one@x", "org")).unwrap();
        assert_eq!(
            default_login_account_id_in(home),
            Some(account_id("one@x", "org"))
        );
        // Logged out (`/logout` drops the record) or unreadable: unknown.
        std::fs::write(home.join(".claude.json"), r#"{"numStartups":3}"#).unwrap();
        assert_eq!(default_login_account_id_in(home), None);
        std::fs::write(home.join(".claude.json"), "not json").unwrap();
        assert_eq!(default_login_account_id_in(home), None);
        // Claude Code prefers a non-empty legacy `.config.json`; an empty one
        // is not a login record.
        std::fs::write(home.join(".claude.json"), record("one@x", "org")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.config.json"), "").unwrap();
        assert_eq!(
            default_login_account_id_in(home),
            Some(account_id("one@x", "org"))
        );
        std::fs::write(home.join(".claude/.config.json"), record("two@x", "")).unwrap();
        assert_eq!(
            default_login_account_id_in(home),
            Some(account_id("two@x", ""))
        );
    }

    /// #240: a pinned session's process is in the wrong profile exactly when
    /// the default login has moved onto or off its account since it launched.
    #[test]
    fn a_pinned_process_is_stale_when_the_default_login_moves_across_its_account() {
        let (mine, other) = (account_id("one@x", ""), account_id("two@x", ""));
        let pinned = ClaudeAccountBinding::requested(mine.clone()).unwrap();
        let launched = |default_login: bool| ClaudeAccountBinding {
            config_dir: Some(PathBuf::from("/profile")),
            default_login,
            ..pinned.clone()
        };
        let in_session_profile = launched(false);
        let on_default_login = launched(true);
        // In its session profile: stale once its account is the default login.
        assert!(pinned.launch_profile_moved(&in_session_profile, Some(&mine)));
        assert!(!pinned.launch_profile_moved(&in_session_profile, Some(&other)));
        // On the default login: stale once that login is another account.
        assert!(pinned.launch_profile_moved(&on_default_login, Some(&other)));
        assert!(!pinned.launch_profile_moved(&on_default_login, Some(&mine)));
        // An unreadable default login is not evidence of a move.
        assert!(!pinned.launch_profile_moved(&in_session_profile, None));
        assert!(!pinned.launch_profile_moved(&on_default_login, None));
        // A launch nothing confirmed yet has no profile to be wrong about.
        assert!(!pinned.launch_profile_moved(&pinned, Some(&mine)));
        // A session created on the default login follows it, whoever it is.
        let follower = launched(true);
        assert!(!follower.launch_profile_moved(&on_default_login, Some(&other)));
    }

    #[test]
    fn account_references_are_identity_based_and_labels_drop_the_domain() {
        let body = br#"{"schemaVersion":1,"accounts":[{"number":2,"email":"alice@example.test","organizationUuid":"org","alias":"work","active":false}]}"#;
        let account = parse_accounts(body).unwrap().pop().unwrap();
        assert_eq!(account.id, account_id("alice@example.test", "org"));
        assert_eq!(account.label, "work");
        assert!(!serde_json::to_string(&account)
            .unwrap()
            .contains("example.test"));
        let unaliased = String::from_utf8(body.to_vec())
            .unwrap()
            .replace(",\"alias\":\"work\"", "");
        assert_eq!(
            parse_accounts(unaliased.as_bytes()).unwrap()[0].label,
            "alice"
        );
        let moved = String::from_utf8(body.to_vec())
            .unwrap()
            .replace("\"number\":2", "\"number\":7");
        assert_eq!(parse_accounts(moved.as_bytes()).unwrap()[0].id, account.id);
        assert_ne!(account.id, account_id("alice@example.test", "another-org"));
    }

    #[test]
    fn quota_windows_follow_the_plan_and_rows_sort_by_slot() {
        let body = br#"{"schemaVersion":1,"accounts":[
            {"number":10,"email":"stale@x","usageStatus":"unavailable","usage":null,"lastGoodUsage":{"fiveHour":{"pct":40.4},"sevenDay":{"pct":60}}},
            {"number":5,"email":"sddenter@x","usageStatus":"ok","usage":{"sevenDay":{"pct":100.0}}},
            {"number":1,"email":"cloud@x","usageStatus":"ok","usage":{"fiveHour":{"pct":10.6},"sevenDay":{"pct":100}}},
            {"number":6,"email":"gone@x","usageStatus":"relogin_required","usage":null}
        ]}"#;
        let accounts = parse_accounts(body).unwrap();
        let slots: Vec<_> = accounts.iter().map(|a| a.slot.as_str()).collect();
        assert_eq!(
            slots,
            ["1", "5", "6", "10"],
            "numeric slot order, not id order"
        );
        let cloud = &accounts[0];
        assert_eq!(
            (cloud.five_hour_pct, cloud.seven_day_pct),
            (Some(11), Some(100))
        );
        assert_eq!(cloud.usage_status, None);
        let max = &accounts[1];
        assert_eq!(max.label, "sddenter");
        assert_eq!(
            (max.five_hour_pct, max.seven_day_pct),
            (None, Some(100)),
            "a plan without a 5h window reports only the weekly one"
        );
        let gone = &accounts[2];
        assert_eq!((gone.five_hour_pct, gone.seven_day_pct), (None, None));
        assert_eq!(gone.usage_status.as_deref(), Some("relogin_required"));
        let stale = &accounts[3];
        assert_eq!(
            (stale.five_hour_pct, stale.seven_day_pct),
            (Some(40), Some(60))
        );
        assert_eq!(stale.usage_status.as_deref(), Some("unavailable"));
        let wire = serde_json::to_value(max).unwrap();
        assert!(wire.get("five_hour_pct").is_none());
    }

    #[test]
    fn discovery_fails_closed_on_unknown_shapes_or_duplicate_identities() {
        for body in [
            r#"{"schemaVersion":2,"accounts":[]}"#,
            r#"{"schemaVersion":1}"#,
            r#"{"schemaVersion":1,"accounts":[{"number":1,"email":"a@b"},{"number":2,"email":"a@b"}]}"#,
        ] {
            assert!(parse_accounts(body.as_bytes()).is_err());
        }
        assert!(ClaudeAccountBinding::requested("2".into()).is_err());
    }

    #[test]
    fn subscription_settings_cannot_override_authentication() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        assert!(verify_subscription_settings(&profile, &project).is_ok());
        for body in [
            r#"{"apiKeyHelper":"return-a-key"}"#,
            r#"{"env":{"ANTHROPIC_API_KEY":"must-not-win"}}"#,
            r#"{"env":{"CLAUDE_CODE_USE_VERTEX":"1"}}"#,
            r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR":"0"}}"#,
            r#"{"env":{"ANTHROPIC_BASE_URL":"false"}}"#,
        ] {
            std::fs::write(profile.join("settings.json"), body).unwrap();
            assert!(verify_subscription_settings(&profile, &project).is_err());
        }
        std::fs::write(
            profile.join("settings.json"),
            r#"{"env":{"CLAUDE_CODE_USE_VERTEX":"0"},"theme":"dark"}"#,
        )
        .unwrap();
        assert!(verify_subscription_settings(&profile, &project).is_ok());
    }

    #[test]
    fn typed_account_references_resolve_by_slot_name_or_id() {
        let account = |slot: &str, label: &str| CswapAccount {
            id: account_id(&format!("{label}@x"), ""),
            slot: slot.into(),
            label: label.into(),
            active: false,
            five_hour_pct: None,
            seven_day_pct: None,
            usage_status: None,
        };
        let accounts = [
            account("1", "cloud"),
            account("2", "Work"),
            account("3", "work"),
        ];
        assert_eq!(find_account(&accounts, "2").unwrap().slot, "2");
        assert_eq!(find_account(&accounts, " #1 ").unwrap().slot, "1");
        assert_eq!(find_account(&accounts, "CLOUD").unwrap().slot, "1");
        assert_eq!(find_account(&accounts, &accounts[2].id).unwrap().slot, "3");
        assert!(
            find_account(&accounts, "work").is_none(),
            "an ambiguous name picks nothing"
        );
        assert!(find_account(&accounts, "9").is_none());
    }

    #[test]
    fn history_follows_a_switch_only_from_shared_history() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let shared = tmp.path().join("home/.claude/projects");
        let private = tmp.path().join("cswap/sessions/3-a");
        let uuid = "11111111-2222-3333-4444-555555555555";
        let mut bound = ClaudeAccountBinding::requested("a".repeat(64)).unwrap();
        bound.config_dir = Some(private.clone());
        let write = |root: &Path| {
            let dir = anthropic_project_dir_in(root, &cwd);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{uuid}.jsonl")), "{}\n").unwrap();
        };
        // No transcript anywhere yet: nothing to lose.
        assert!(history_follows_switch_in(&shared, &cwd, uuid, Some(&bound)));
        assert!(history_follows_switch_in(&shared, &cwd, uuid, None));
        // Only in the private profile: another account would start over.
        write(&private.join("projects"));
        assert!(!history_follows_switch_in(
            &shared,
            &cwd,
            uuid,
            Some(&bound)
        ));
        // Once cswap shares it, every account reads the same transcript.
        write(&shared);
        assert!(history_follows_switch_in(&shared, &cwd, uuid, Some(&bound)));
    }

    #[test]
    fn discovery_never_enables_vendor_quota_inference() {
        assert!(verify_cswap_settings(br#"{"schemaVersion":1,"settings":[]}"#).is_ok());
        assert!(verify_cswap_settings(
            br#"{"schemaVersion":1,"settings":[{"key":"usage.headerProbe","value":false}]}"#
        )
        .is_ok());
        assert!(verify_cswap_settings(
            br#"{"schemaVersion":1,"settings":[{"key":"usage.headerProbe","value":true}]}"#
        )
        .is_err());
    }
}
