//! Put `@ccteam/dsh-client` into a DSH profile — the ccteam-owned tenant
//! profile, or (merge-only) the operator's own `~/.dsh/profiles/web`.
//!
//! The embedded `assets/dsh-client.tgz` is a manual asset for now. When the
//! plugin changes, run `npm run build && npm pack` in `plugins/dsh-client/`,
//! replace this file with the resulting tarball, and commit it with the Rust
//! change. This mirrors the checked-in Pi bridge asset: Rust builds must not
//! require npm or node.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::HarnessError;

const CLIENT_TGZ: &[u8] = include_bytes!("assets/dsh-client.tgz");
pub const WEB_PROFILE: &str = "ccteam-web";
const DSH_BASE_BUNDLE: &str = "@deepseek-ai/dsh-base";
const DSH_WEB_APP_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
const CCTEAM_CLIENT_BUNDLE: &str = "@ccteam/dsh-client";
const EMPTY_PATCH_YAML: &str = "[]\n";
const CLIENT_SCOPE: &str = "@ccteam";
const CLIENT_PACKAGE: &str = "dsh-client";
const CCTEAM_CLIENT_ROW_ID: &str = "ccteam-client";
const PATCH_FILE: &str = "cordis.patch.yml";

#[derive(Debug, Clone)]
pub struct MaterializedDshProfile {
    pub cache_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub cache_rebuilt: bool,
}

/// Register `@ccteam/dsh-client` into a profile ccteam does NOT own — the
/// operator's real `~/.dsh/profiles/<name>` (gate ①).
///
/// Strictly additive: the profile's `package.json` keeps every key it already
/// has (only our bundle is appended to `dsh.profile.bundles`), and the
/// profile's `cordis.patch.yml` keeps every row it already has (only the
/// `ccteam-client` row is written). Unparseable JSON/YAML is an error, never a
/// clobber.
pub fn register_dsh_client_into_profile(
    ccteam_root: &Path,
    dsh_home: &Path,
    profile: &str,
    config: DshClientConfig<'_>,
) -> Result<MaterializedDshProfile, HarnessError> {
    materialize_profile_in(
        ccteam_root,
        dsh_home,
        ProfileSpec {
            name: profile,
            required_bundles: &[CCTEAM_CLIENT_BUNDLE],
            config,
            manifest: ManifestPolicy::MergeOnly,
        },
    )
}

/// Read-only, best-effort: is ccteam's client already registered in this
/// profile? True only when BOTH halves gate ① writes are present — our bundle
/// in `dsh.profile.bundles` AND a `ccteam-client` override row carrying a
/// `transportSocket` (the bundle alone installs files but leaves the plugin
/// with no ACP listener, which is not a working registration). Any missing or
/// unparseable file reads as `false`; this never writes.
pub fn dsh_client_registered_in_profile(dsh_home: &Path, profile: &str) -> bool {
    let profile_dir = dsh_home.join("profiles").join(profile);
    let bundled = fs::read_to_string(profile_dir.join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|manifest| {
            manifest
                .get("dsh")?
                .get("profile")?
                .get("bundles")?
                .as_array()
                .map(|bundles| {
                    bundles
                        .iter()
                        .any(|b| b.as_str() == Some(CCTEAM_CLIENT_BUNDLE))
                })
        })
        .unwrap_or(false);
    if !bundled {
        return false;
    }
    fs::read_to_string(profile_dir.join(PATCH_FILE))
        .ok()
        .and_then(|raw| serde_yaml::from_str::<serde_yaml::Value>(&raw).ok())
        .and_then(|patch| {
            let rows = patch.as_sequence()?.clone();
            Some(rows.iter().any(|row| {
                row.get("id").and_then(serde_yaml::Value::as_str) == Some(CCTEAM_CLIENT_ROW_ID)
                    && row.get("insert").is_none()
                    && row
                        .get("config")
                        .and_then(|c| c.get("transportSocket"))
                        .and_then(serde_yaml::Value::as_str)
                        .is_some_and(|socket| !socket.trim().is_empty())
            }))
        })
        .unwrap_or(false)
}

pub fn materialize_profile_in(
    ccteam_root: &Path,
    dsh_home: &Path,
    spec: ProfileSpec<'_>,
) -> Result<MaterializedDshProfile, HarnessError> {
    let cache_base = ccteam_root.join("runtime").join("dsh").join("client");
    fs::create_dir_all(&cache_base).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH client cache {}: {e}",
            cache_base.display()
        ))
    })?;
    set_private_dir(&cache_base)?;

    let hash = client_tgz_sha256();
    let cache_dir = cache_base.join(&hash);
    let cache_rebuilt = ensure_client_cache(&cache_base, &cache_dir, &hash)?;
    let profile_dir = materialize_profile_files(dsh_home, &cache_dir, &spec)?;

    Ok(MaterializedDshProfile {
        cache_dir,
        profile_dir,
        cache_rebuilt,
    })
}

/// The `ccteam-client` row's own plugin config. Reaches `apply(ctx, config)`
/// verbatim, so these keys are FLAT (see [`merged_profile_patch_yaml`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct DshClientConfig<'a> {
    pub enrollment: Option<&'a str>,
    pub daemon_url: Option<&'a str>,
    /// Unix socket the plugin serves ACP on. Empty/absent = tool surface only,
    /// no transport — the plugin activates its listener on this key alone.
    pub transport_socket: Option<&'a str>,
}

impl DshClientConfig<'_> {
    fn is_empty(&self) -> bool {
        self.enrollment.is_none() && self.daemon_url.is_none() && self.transport_socket.is_none()
    }
}

/// Who owns the profile's `package.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestPolicy {
    /// ccteam-managed home: ccteam names the profile and pins `private`.
    Owned,
    /// Someone else's profile (the operator's `~/.dsh`): touch nothing but
    /// `dsh.profile.bundles`, and only to append our own bundle.
    MergeOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileSpec<'a> {
    pub name: &'a str,
    pub required_bundles: &'static [&'static str],
    pub config: DshClientConfig<'a>,
    pub manifest: ManifestPolicy,
}

impl<'a> ProfileSpec<'a> {
    /// The ccteam-owned `dsh web` profile in a managed tenant home.
    pub fn web(config: DshClientConfig<'a>) -> Self {
        Self {
            name: WEB_PROFILE,
            required_bundles: &[DSH_BASE_BUNDLE, DSH_WEB_APP_BUNDLE, CCTEAM_CLIENT_BUNDLE],
            config,
            manifest: ManifestPolicy::Owned,
        }
    }
}

pub fn client_tgz_sha256() -> String {
    format!("{:x}", Sha256::digest(CLIENT_TGZ))
}

fn ensure_client_cache(
    cache_base: &Path,
    cache_dir: &Path,
    hash: &str,
) -> Result<bool, HarnessError> {
    if cache_looks_usable(cache_dir) {
        return Ok(false);
    }
    remove_existing(cache_dir)?;

    let tmp = cache_base.join(format!(
        ".{hash}-{}-{}.tmp",
        std::process::id(),
        now_nanos()
    ));
    remove_existing(&tmp)?;
    fs::create_dir_all(&tmp).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create temp DSH client cache {}: {e}",
            tmp.display()
        ))
    })?;

    let result = extract_client_tgz(&tmp)
        .and_then(|_| {
            if cache_looks_usable(&tmp) {
                Ok(())
            } else {
                Err(HarnessError::SpawnFailed(
                    "embedded DSH client archive did not produce a package root".into(),
                ))
            }
        })
        .and_then(|_| {
            fs::rename(&tmp, cache_dir).or_else(|e| {
                if cache_looks_usable(cache_dir) {
                    let _ = fs::remove_dir_all(&tmp);
                    Ok(())
                } else {
                    Err(HarnessError::SpawnFailed(format!(
                        "publish DSH client cache {} -> {}: {e}",
                        tmp.display(),
                        cache_dir.display()
                    )))
                }
            })
        });

    if let Err(err) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err);
    }
    set_private_dir(cache_dir)?;
    sync_dir(cache_base);
    Ok(true)
}

fn extract_client_tgz(dst: &Path) -> Result<(), HarnessError> {
    let reader = GzDecoder::new(CLIENT_TGZ);
    let mut archive = Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| HarnessError::SpawnFailed(format!("read embedded DSH client archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            HarnessError::SpawnFailed(format!("read embedded DSH client archive entry: {e}"))
        })?;
        let raw_path = entry.path().map_err(|e| {
            HarnessError::SpawnFailed(format!("read embedded DSH client archive path: {e}"))
        })?;
        let Some(rel) = strip_npm_package_prefix(&raw_path) else {
            continue;
        };
        let out = dst.join(rel);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&out).map_err(|e| {
                HarnessError::SpawnFailed(format!("create DSH client dir {}: {e}", out.display()))
            })?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                HarnessError::SpawnFailed(format!(
                    "create DSH client parent {}: {e}",
                    parent.display()
                ))
            })?;
        }
        entry.unpack(&out).map_err(|e| {
            HarnessError::SpawnFailed(format!("unpack DSH client file {}: {e}", out.display()))
        })?;
    }
    Ok(())
}

fn strip_npm_package_prefix(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    match components.next()? {
        Component::Normal(name) if name == OsStr::new("package") => {}
        _ => return None,
    }
    let mut out = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn materialize_profile_files(
    dsh_home: &Path,
    cache_dir: &Path,
    spec: &ProfileSpec<'_>,
) -> Result<PathBuf, HarnessError> {
    let profile_dir = dsh_home.join("profiles").join(spec.name);
    fs::create_dir_all(&profile_dir).map_err(|e| {
        HarnessError::SpawnFailed(format!("create DSH profile {}: {e}", profile_dir.display()))
    })?;

    let package_json = merged_profile_package_json(&profile_dir, spec)?;
    write_if_changed(&profile_dir.join("package.json"), package_json.as_bytes())?;
    let patch_path = profile_dir.join(PATCH_FILE);
    if let Some(patch_yaml) = merged_profile_patch_yaml(&patch_path, spec)? {
        write_if_changed(&patch_path, patch_yaml.as_bytes())?;
    }

    let scope_dir = profile_dir.join("node_modules").join(CLIENT_SCOPE);
    fs::create_dir_all(&scope_dir).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH profile package scope {}: {e}",
            scope_dir.display()
        ))
    })?;
    let link = scope_dir.join(CLIENT_PACKAGE);
    ensure_symlink(&link, cache_dir)?;
    Ok(profile_dir)
}

fn merged_profile_package_json(
    profile_dir: &Path,
    spec: &ProfileSpec<'_>,
) -> Result<String, HarnessError> {
    let path = profile_dir.join("package.json");
    let manifest_exists = path.exists();
    let mut value = if manifest_exists {
        let raw = fs::read_to_string(&path).map_err(|e| {
            HarnessError::SpawnFailed(format!("read DSH profile package {}: {e}", path.display()))
        })?;
        serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| {
            HarnessError::SpawnFailed(format!("parse DSH profile package {}: {e}", path.display()))
        })?
    } else {
        serde_json::json!({})
    };

    let obj = value.as_object_mut().ok_or_else(|| {
        HarnessError::SpawnFailed(format!(
            "DSH profile package {} must be a JSON object",
            path.display()
        ))
    })?;
    if spec.manifest == ManifestPolicy::Owned {
        obj.entry("name".to_string()).or_insert_with(|| {
            serde_json::Value::String(format!("ccteam-{name}-profile", name = spec.name))
        });
        obj.insert("private".to_string(), serde_json::Value::Bool(true));
    } else if !manifest_exists {
        // Nothing of the user's to preserve: give the file the minimum a
        // profile manifest needs.
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(format!("dsh-{name}-profile", name = spec.name)),
        );
        obj.insert("private".to_string(), serde_json::Value::Bool(true));
    }

    // Registering into a profile that does not exist yet CREATES it, and a
    // profile whose bundles are only `@ccteam/dsh-client` cannot boot: the
    // plugin waits forever for the host services (`agents`, `tools`, …) the
    // vendor bundles provide, and `dsh web` dies before readiness (real-machine
    // v0.10.3 DoD). Scaffold the vendor's own defaults first — exactly what a
    // first `dsh web` run would have written — and merge ours after. An
    // EXISTING manifest is never given vendor rows: merge-only means ccteam's
    // own entries only.
    let scaffold: &[&str] = if spec.manifest == ManifestPolicy::MergeOnly && !manifest_exists {
        if spec.name == super::spawn_spec::DSH_NATIVE_WEB_PROFILE {
            &[DSH_BASE_BUNDLE, DSH_WEB_APP_BUNDLE]
        } else {
            &[DSH_BASE_BUNDLE]
        }
    } else {
        &[]
    };

    let dsh = obj
        .entry("dsh".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !dsh.is_object() {
        *dsh = serde_json::json!({});
    }
    let profile = dsh
        .as_object_mut()
        .expect("dsh coerced to object")
        .entry("profile".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !profile.is_object() {
        *profile = serde_json::json!({});
    }
    let bundles = profile
        .as_object_mut()
        .expect("profile coerced to object")
        .entry("bundles".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !bundles.is_array() {
        *bundles = serde_json::json!([]);
    }
    let bundles = bundles.as_array_mut().expect("bundles coerced to array");
    for required in scaffold.iter().chain(spec.required_bundles) {
        if !bundles.iter().any(|v| v.as_str() == Some(required)) {
            bundles.push(serde_json::Value::String((*required).to_string()));
        }
    }

    serde_json::to_string_pretty(&value)
        .map(|mut body| {
            body.push('\n');
            body
        })
        .map_err(|e| HarnessError::SpawnFailed(format!("serialize DSH profile package: {e}")))
}

/// Upsert ONLY ccteam's own row into the profile's patch list, preserving
/// every other row byte-for-byte in meaning. `Ok(None)` = nothing to write.
///
/// An OVERRIDE patch, never an `insert` one. `@ccteam/dsh-client` is in the
/// profile's `dsh.profile.bundles`, and that bundle's own patch layer
/// (`plugins/dsh-client/cordis.patch.yml`) inserts the `ccteam-client` row —
/// inserting it a second time here makes Cordis abort the whole boot with
/// `duplicate loader entry id: ccteam-client`. That shipped once (v0.10.0) and
/// killed every tenant instance before readiness; it holds identically for the
/// operator's own profile, so this writer has no `insert` arm at all.
///
/// dsh-app-boot's patch semantics (applyPatches): a patch carrying `insert`
/// inserts entries, while a patch with an `id` and NO `insert` looks the
/// existing entry up and copies its remaining keys onto it as overrides.
/// `name` is checked against the target and the patch is skipped on a
/// mismatch, so passing it keeps this honest rather than silently patching
/// some other plugin's row.
///
/// `config` is the row's own plugin config — it reaches `apply(ctx, config)`
/// verbatim and is the `base` layer of the plugin's settings namespace, so its
/// keys are FLAT (`daemonUrl` / `enrollment` / `transportSocket`). Nesting them
/// under the namespace name would leave `config.transportSocket` undefined and
/// the instance would come up with no ACP transport at all.
fn merged_profile_patch_yaml(
    path: &Path,
    spec: &ProfileSpec<'_>,
) -> Result<Option<String>, HarnessError> {
    let existing = if path.exists() {
        let raw = fs::read_to_string(path).map_err(|e| {
            HarnessError::SpawnFailed(format!("read DSH profile patch {}: {e}", path.display()))
        })?;
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            match serde_yaml::from_str::<serde_yaml::Value>(&raw).map_err(|e| {
                HarnessError::SpawnFailed(format!(
                    "parse DSH profile patch {}: {e}",
                    path.display()
                ))
            })? {
                serde_yaml::Value::Sequence(rows) => rows,
                serde_yaml::Value::Null => Vec::new(),
                _ => {
                    return Err(HarnessError::SpawnFailed(format!(
                        "DSH profile patch {} must be a YAML sequence",
                        path.display()
                    )))
                }
            }
        }
    } else {
        Vec::new()
    };

    if spec.config.is_empty() {
        // No config of ours to install: leave an existing file exactly as it
        // is, and only create the empty list when the profile has none.
        return Ok((!path.exists()).then(|| EMPTY_PATCH_YAML.to_string()));
    }

    let mut config = serde_yaml::Mapping::new();
    for (key, value) in [
        ("enrollment", spec.config.enrollment),
        ("daemonUrl", spec.config.daemon_url),
        ("transportSocket", spec.config.transport_socket),
    ] {
        if let Some(value) = value {
            config.insert(
                serde_yaml::Value::String(key.to_string()),
                serde_yaml::Value::String(value.to_string()),
            );
        }
    }

    let mut rows = existing;
    let ours = rows.iter_mut().find(|row| {
        row.get("id").and_then(serde_yaml::Value::as_str) == Some(CCTEAM_CLIENT_ROW_ID)
            && row.get("insert").is_none()
    });
    match ours {
        Some(row) => {
            let row = row.as_mapping_mut().ok_or_else(|| {
                HarnessError::SpawnFailed(format!(
                    "DSH profile patch {} row `{CCTEAM_CLIENT_ROW_ID}` must be a mapping",
                    path.display()
                ))
            })?;
            row.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(CCTEAM_CLIENT_BUNDLE.to_string()),
            );
            row.insert(
                serde_yaml::Value::String("config".to_string()),
                serde_yaml::Value::Mapping(config),
            );
        }
        None => {
            let mut row = serde_yaml::Mapping::new();
            row.insert(
                serde_yaml::Value::String("id".to_string()),
                serde_yaml::Value::String(CCTEAM_CLIENT_ROW_ID.to_string()),
            );
            row.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(CCTEAM_CLIENT_BUNDLE.to_string()),
            );
            row.insert(
                serde_yaml::Value::String("config".to_string()),
                serde_yaml::Value::Mapping(config),
            );
            rows.push(serde_yaml::Value::Mapping(row));
        }
    }

    serde_yaml::to_string(&serde_yaml::Value::Sequence(rows))
        .map(Some)
        .map_err(|e| HarnessError::SpawnFailed(format!("serialize DSH profile patch: {e}")))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), HarnessError> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            HarnessError::SpawnFailed(format!("create parent {}: {e}", parent.display()))
        })?;
    }
    fs::write(path, bytes).map_err(|e| {
        HarnessError::SpawnFailed(format!("write DSH profile file {}: {e}", path.display()))
    })
}

fn ensure_symlink(link: &Path, target: &Path) -> Result<(), HarnessError> {
    if fs::symlink_metadata(link)
        .ok()
        .is_some_and(|meta| meta.file_type().is_symlink())
        && fs::read_link(link).is_ok_and(|existing| existing == target)
    {
        return Ok(());
    }
    remove_existing(link)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "symlink DSH client {} -> {}: {e}",
            link.display(),
            target.display()
        ))
    })?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "symlink DSH client {} -> {}: {e}",
            link.display(),
            target.display()
        ))
    })?;
    Ok(())
}

fn cache_looks_usable(path: &Path) -> bool {
    path.is_dir()
        && path.join("package.json").is_file()
        && fs::read_dir(path)
            .ok()
            .and_then(|mut entries| entries.next())
            .is_some()
}

fn remove_existing(path: &Path) -> Result<(), HarnessError> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|e| HarnessError::SpawnFailed(format!("remove {}: {e}", path.display())))
}

fn set_private_dir(path: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
            HarnessError::SpawnFailed(format!("chmod 0700 {}: {e}", path.display()))
        })?;
    }
    Ok(())
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const SOCKET: &str = "/srv/ccteam-home/runtime/dsh/acp/alice.sock";

    fn plain_web() -> ProfileSpec<'static> {
        ProfileSpec::web(DshClientConfig::default())
    }

    fn wired_web() -> ProfileSpec<'static> {
        ProfileSpec::web(DshClientConfig {
            enrollment: Some("ccteam-enroll:abc:secret"),
            daemon_url: Some("http://127.0.0.1:7331"),
            transport_socket: Some(SOCKET),
        })
    }

    fn read_patch(profile_dir: &Path) -> serde_yaml::Value {
        let raw = fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap();
        serde_yaml::from_str(&raw).unwrap()
    }

    fn read_package(profile_dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(profile_dir.join("package.json")).unwrap()).unwrap()
    }

    fn ccteam_row(patch: &serde_yaml::Value) -> serde_yaml::Value {
        patch
            .as_sequence()
            .expect("patch is a sequence")
            .iter()
            .find(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some("ccteam-client"))
            .expect("ccteam row present")
            .clone()
    }

    #[test]
    fn cache_miss_then_hit_uses_sha_directory() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(first.cache_rebuilt);
        assert_eq!(
            first.cache_dir.file_name().unwrap().to_string_lossy(),
            client_tgz_sha256()
        );
        assert!(first.cache_dir.join("package.json").is_file());

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(!second.cache_rebuilt);
        assert_eq!(second.cache_dir, first.cache_dir);
    }

    #[test]
    fn owned_web_profile_matches_dsh_bundle_shape() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let package_json = read_package(&out.profile_dir);
        assert_eq!(package_json["name"], "ccteam-ccteam-web-profile");
        assert_eq!(package_json["private"], true);
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ])
        );
        assert_eq!(
            fs::read_to_string(out.profile_dir.join("cordis.patch.yml")).unwrap(),
            EMPTY_PATCH_YAML,
            "no config of ours to install => an empty patch list"
        );
    }

    #[test]
    fn web_profile_row_carries_flat_config_and_never_a_duplicate_insert() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();
        let package_raw = fs::read_to_string(out.profile_dir.join("package.json")).unwrap();
        let patch_raw = fs::read_to_string(out.profile_dir.join("cordis.patch.yml")).unwrap();
        assert!(!package_raw.contains("host"));
        assert!(!patch_raw.contains("host:"));

        // Structure, not substrings. Three ways this file can be well-formed on
        // its face yet break the instance, all of which a `contains` assertion
        // sails straight past:
        //
        //   1. An `insert:` wrapper. The bundle list already pulls in
        //      `@ccteam/dsh-client`, whose own patch layer inserts the
        //      `ccteam-client` row; inserting it again makes Cordis abort the
        //      boot with `duplicate loader entry id`.
        //   2. A `config` nested under the settings-namespace name. The row's
        //      config reaches `apply(ctx, config)` verbatim, so the keys must
        //      be flat — nested, `config.transportSocket` is undefined and the
        //      instance serves no ACP socket at all.
        //   3. A missing `transportSocket`: the tool surface still works, so
        //      only a hire (never a boot log) would notice.
        let patch: serde_yaml::Value = serde_yaml::from_str(&patch_raw).unwrap();
        let rows = patch.as_sequence().expect("patch is a sequence");
        assert_eq!(rows.len(), 1, "one patch row: {patch_raw}");
        let row = &rows[0];
        assert!(
            row.get("insert").is_none(),
            "must OVERRIDE the bundle-inserted row, never insert a duplicate: {patch_raw}"
        );
        assert_eq!(row["id"], serde_yaml::Value::String("ccteam-client".into()));
        assert_eq!(
            row["name"],
            serde_yaml::Value::String("@ccteam/dsh-client".into()),
            "name guards against patching some other plugin's row"
        );
        let config = row["config"].as_mapping().expect("flat plugin config");
        assert_eq!(
            config["enrollment"],
            serde_yaml::Value::String("ccteam-enroll:abc:secret".into())
        );
        assert_eq!(
            config["daemonUrl"],
            serde_yaml::Value::String("http://127.0.0.1:7331".into())
        );
        assert_eq!(
            config["transportSocket"],
            serde_yaml::Value::String(SOCKET.into())
        );
        assert!(
            config.get("ccteam-client").is_none(),
            "config keys are flat, not nested under the namespace: {patch_raw}"
        );
    }

    #[test]
    fn merge_preserves_self_installed_profile_layer() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join(WEB_PROFILE);
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "tenant-profile",
                "private": false,
                "dependencies": {
                    "is-number": "7.0.0"
                },
                "dsh": {
                    "profile": {
                        "bundles": ["tenant-plugin"]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let package_json = read_package(&out.profile_dir);
        assert_eq!(package_json["name"], "tenant-profile");
        assert_eq!(package_json["private"], true);
        assert_eq!(package_json["dependencies"]["is-number"], "7.0.0");
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "tenant-plugin",
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ])
        );
    }

    /// Registering into a profile that does not exist yet must scaffold the
    /// vendor's own web bundles first — a manifest listing only
    /// `@ccteam/dsh-client` cannot boot (the plugin waits forever for the host
    /// services the vendor bundles provide; real-machine v0.10.3 DoD caught
    /// `dsh web` dying before readiness on a fresh operator home).
    #[test]
    fn registering_into_a_missing_profile_scaffolds_the_vendor_web_bundles() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        register_dsh_client_into_profile(
            root.path(),
            dsh_home.path(),
            "web",
            DshClientConfig {
                transport_socket: Some(SOCKET),
                daemon_url: Some("http://127.0.0.1:7331"),
                ..DshClientConfig::default()
            },
        )
        .unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        let package_json = read_package(&profile_dir);
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ]),
            "a scaffolded manifest must be bootable, vendor bundles first"
        );
    }

    /// The Hosts page shows "register the DSH plugin" until this reads true,
    /// so it must answer for a profile ccteam actually registered — and must
    /// NOT claim registration for a half-written one (bundle present but no
    /// configured row leaves the plugin with no ACP listener).
    #[test]
    fn registration_detection_needs_both_the_bundle_and_a_configured_row() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");

        assert!(
            !dsh_client_registered_in_profile(dsh_home.path(), "web"),
            "no profile at all -> not registered"
        );

        // Bundle installed by hand (`dsh plugin add`) but no config row.
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "dsh": { "profile": { "bundles": ["@ccteam/dsh-client"] } }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(profile_dir.join("cordis.patch.yml"), "[]\n").unwrap();
        assert!(
            !dsh_client_registered_in_profile(dsh_home.path(), "web"),
            "bundle without a configured transport row -> not registered"
        );

        register_dsh_client_into_profile(
            root.path(),
            dsh_home.path(),
            "web",
            DshClientConfig {
                transport_socket: Some(SOCKET),
                daemon_url: Some("http://127.0.0.1:7331"),
                ..DshClientConfig::default()
            },
        )
        .unwrap();
        assert!(
            dsh_client_registered_in_profile(dsh_home.path(), "web"),
            "after gate (1) registration -> registered"
        );
        assert!(
            !dsh_client_registered_in_profile(dsh_home.path(), "headless"),
            "registration is per profile"
        );
    }

    /// Gate (1): ccteam may write into the operator's REAL `~/.dsh` profile,
    /// and that permission is only defensible if it is strictly additive.
    #[test]
    fn operator_registration_only_adds_ccteam_entries() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        let user_package = serde_json::json!({
            "name": "dsh-web-profile",
            "version": "1.2.3",
            "dependencies": { "left-pad": "1.3.0" },
            "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"] } }
        })
        .to_string();
        fs::write(profile_dir.join("package.json"), &user_package).unwrap();
        let user_patch = "- id: my-own-plugin\n  config:\n    keepMe: true\n";
        fs::write(profile_dir.join("cordis.patch.yml"), user_patch).unwrap();

        let out = register_dsh_client_into_profile(
            root.path(),
            dsh_home.path(),
            "web",
            DshClientConfig {
                transport_socket: Some(SOCKET),
                daemon_url: Some("http://127.0.0.1:7331"),
                ..DshClientConfig::default()
            },
        )
        .unwrap();
        assert_eq!(out.profile_dir, profile_dir);

        // package.json: every user key survives, ONLY the bundle is appended.
        let package_json = read_package(&profile_dir);
        assert_eq!(package_json["name"], "dsh-web-profile");
        assert_eq!(package_json["version"], "1.2.3");
        assert_eq!(package_json["dependencies"]["left-pad"], "1.3.0");
        assert!(
            package_json.get("private").is_none(),
            "merge-only must not pin keys the user owns: {package_json}"
        );
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ])
        );

        // cordis.patch.yml: the user's row is untouched, ours is appended as an
        // override (no `insert`), and enrollment is NEVER injected into the
        // operator's home — their own Settings owns that.
        let patch = read_patch(&profile_dir);
        let rows = patch.as_sequence().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            serde_yaml::from_str::<serde_yaml::Value>(user_patch)
                .unwrap()
                .as_sequence()
                .unwrap()[0],
            "the user's own patch row must survive unchanged"
        );
        let ours = ccteam_row(&patch);
        assert!(ours.get("insert").is_none());
        let config = ours["config"].as_mapping().unwrap();
        assert_eq!(
            config["transportSocket"],
            serde_yaml::Value::String(SOCKET.into())
        );
        assert!(
            config.get("enrollment").is_none(),
            "the operator's enrollment is theirs to set: {config:?}"
        );
        assert!(profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE)
            .join("dist")
            .join("index.js")
            .is_file());
    }

    #[test]
    fn operator_registration_is_idempotent_and_reconfigurable() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let register = |socket: &str| {
            register_dsh_client_into_profile(
                root.path(),
                dsh_home.path(),
                "web",
                DshClientConfig {
                    transport_socket: Some(socket),
                    ..DshClientConfig::default()
                },
            )
            .unwrap()
        };

        let first = register(SOCKET);
        let package_after_first = fs::read(first.profile_dir.join("package.json")).unwrap();
        let patch_after_first = fs::read(first.profile_dir.join("cordis.patch.yml")).unwrap();

        let second = register(SOCKET);
        assert_eq!(second.profile_dir, first.profile_dir);
        assert_eq!(
            fs::read(second.profile_dir.join("package.json")).unwrap(),
            package_after_first,
            "re-registering must not grow the manifest"
        );
        assert_eq!(
            fs::read(second.profile_dir.join("cordis.patch.yml")).unwrap(),
            patch_after_first,
            "re-registering must not duplicate our patch row"
        );

        // A moved socket rewrites OUR row in place instead of appending a
        // second one — a duplicate id kills the whole Cordis boot.
        let third = register("/srv/other/acp/operator.sock");
        let patch = read_patch(&third.profile_dir);
        assert_eq!(patch.as_sequence().unwrap().len(), 1);
        assert_eq!(
            ccteam_row(&patch)["config"]["transportSocket"],
            serde_yaml::Value::String("/srv/other/acp/operator.sock".into())
        );
    }

    #[test]
    fn unparseable_operator_files_are_reported_not_clobbered() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        let broken_patch = "- id: [unclosed\n";
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({ "name": "dsh-web-profile" }).to_string(),
        )
        .unwrap();
        fs::write(profile_dir.join("cordis.patch.yml"), broken_patch).unwrap();

        let err = register_dsh_client_into_profile(
            root.path(),
            dsh_home.path(),
            "web",
            DshClientConfig {
                transport_socket: Some(SOCKET),
                ..DshClientConfig::default()
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("parse DSH profile patch"),
            "got {err}"
        );
        assert_eq!(
            fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap(),
            broken_patch
        );
    }

    #[test]
    fn web_profile_symlink_self_heals() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let wrong_target = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let link = first
            .profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), second.cache_dir);
    }

    #[test]
    fn node_modules_entry_is_symlink_to_cache() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let link = out
            .profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE);

        let meta = fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), out.cache_dir);
        assert!(link.join("dist").join("index.js").is_file());
    }

    #[test]
    fn rerunning_profile_materialization_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();
        let second = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();

        assert!(first.cache_rebuilt);
        assert!(!second.cache_rebuilt);
        assert_eq!(
            read_package(&first.profile_dir)["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ])
        );
        assert_eq!(
            read_patch(&first.profile_dir).as_sequence().unwrap().len(),
            1
        );
        assert_eq!(
            fs::read_link(
                first
                    .profile_dir
                    .join("node_modules")
                    .join(CLIENT_SCOPE)
                    .join(CLIENT_PACKAGE)
            )
            .unwrap(),
            first.cache_dir
        );
    }

    #[test]
    fn empty_cache_directory_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        fs::remove_dir_all(&first.cache_dir).unwrap();
        fs::create_dir_all(&first.cache_dir).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(second.cache_rebuilt);
        assert!(second.cache_dir.join("package.json").is_file());
        assert!(second.cache_dir.join("dist").join("index.js").is_file());
    }

    #[test]
    fn archive_paths_are_stripped_and_sanitized() {
        assert_eq!(
            strip_npm_package_prefix(Path::new("package/dist/index.js")).unwrap(),
            PathBuf::from("dist/index.js")
        );
        assert!(strip_npm_package_prefix(Path::new("not-package/index.js")).is_none());
        assert!(strip_npm_package_prefix(Path::new("package/../x")).is_none());
    }

    #[test]
    fn cache_contains_the_plugin_bundle_patch() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let package_json: serde_json::Value =
            serde_json::from_slice(&fs::read(out.cache_dir.join("package.json")).unwrap()).unwrap();
        assert_eq!(package_json["name"], "@ccteam/dsh-client");
        assert_eq!(
            package_json["dsh"]["bundle"]["patch"],
            serde_json::json!("./cordis.patch.yml")
        );
    }

    #[test]
    fn link_is_replaced_if_it_points_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let wrong_target = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let link = first
            .profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), second.cache_dir);
    }

    #[test]
    fn read_embedded_tgz_bytes_without_consuming_build_tools() {
        let mut decoder = GzDecoder::new(CLIENT_TGZ);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert!(!decoded.is_empty());
    }
}
