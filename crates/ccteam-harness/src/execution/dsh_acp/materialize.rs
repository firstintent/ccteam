//! Materialize the managed DSH profile used by `dsh --profile ccteam`.
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

use crate::{ccteam_root_from_env, HarnessError};

const CLIENT_TGZ: &[u8] = include_bytes!("assets/dsh-client.tgz");
pub const MANAGED_PROFILE: &str = "ccteam";
pub const WEB_PROFILE: &str = "ccteam-web";
const DSH_BASE_BUNDLE: &str = "@deepseek-ai/dsh-base";
const DSH_WEB_APP_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
const CCTEAM_CLIENT_BUNDLE: &str = "@ccteam/dsh-client";
const EMPTY_PATCH_YAML: &str = "[]\n";
const CLIENT_SCOPE: &str = "@ccteam";
const CLIENT_PACKAGE: &str = "dsh-client";
const CCTEAM_CLIENT_ROW_ID: &str = "ccteam-client";

#[derive(Debug, Clone)]
pub struct MaterializedDshProfile {
    pub cache_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub cache_rebuilt: bool,
}

pub fn materialize_managed_profile(
    dsh_home: &Path,
) -> Result<MaterializedDshProfile, HarnessError> {
    materialize_profile(dsh_home, ProfileSpec::managed())
}

pub fn materialize_web_profile(
    dsh_home: &Path,
    enrollment: Option<&str>,
    daemon_url: Option<&str>,
) -> Result<MaterializedDshProfile, HarnessError> {
    materialize_profile(dsh_home, ProfileSpec::web(enrollment, daemon_url))
}

fn materialize_profile(
    dsh_home: &Path,
    spec: ProfileSpec<'_>,
) -> Result<MaterializedDshProfile, HarnessError> {
    let root = ccteam_root_from_env()
        .ok_or_else(|| HarnessError::SpawnFailed("cannot resolve CCTEAM_HOME".into()))?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    materialize_profile_in(&root, dsh_home, spec)
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

#[derive(Debug, Clone, Copy)]
pub struct ProfileSpec<'a> {
    pub name: &'static str,
    pub required_bundles: &'static [&'static str],
    pub enrollment: Option<&'a str>,
    pub daemon_url: Option<&'a str>,
}

impl<'a> ProfileSpec<'a> {
    pub fn managed() -> Self {
        Self {
            name: MANAGED_PROFILE,
            required_bundles: &[DSH_BASE_BUNDLE, CCTEAM_CLIENT_BUNDLE],
            enrollment: None,
            daemon_url: None,
        }
    }

    pub fn web(enrollment: Option<&'a str>, daemon_url: Option<&'a str>) -> Self {
        Self {
            name: WEB_PROFILE,
            required_bundles: &[DSH_BASE_BUNDLE, DSH_WEB_APP_BUNDLE, CCTEAM_CLIENT_BUNDLE],
            enrollment,
            daemon_url,
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
    let patch_yaml = profile_patch_yaml(spec)?;
    write_if_changed(&profile_dir.join("cordis.patch.yml"), patch_yaml.as_bytes())?;

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
    let mut value = if path.exists() {
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
    obj.entry("name".to_string()).or_insert_with(|| {
        let name = if spec.name == MANAGED_PROFILE {
            "ccteam-profile".to_string()
        } else {
            format!("ccteam-{name}-profile", name = spec.name)
        };
        serde_json::Value::String(name)
    });
    obj.insert("private".to_string(), serde_json::Value::Bool(true));

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
    for required in spec.required_bundles {
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

fn profile_patch_yaml(spec: &ProfileSpec<'_>) -> Result<String, HarnessError> {
    if spec.enrollment.is_none() && spec.daemon_url.is_none() {
        return Ok(EMPTY_PATCH_YAML.to_string());
    }
    let mut config = serde_yaml::Mapping::new();
    if let Some(enrollment) = spec.enrollment {
        config.insert(
            serde_yaml::Value::String("enrollment".to_string()),
            serde_yaml::Value::String(enrollment.to_string()),
        );
    }
    if let Some(daemon_url) = spec.daemon_url {
        config.insert(
            serde_yaml::Value::String("daemonUrl".to_string()),
            serde_yaml::Value::String(daemon_url.to_string()),
        );
    }

    // An OVERRIDE patch, not an `insert` one. `@ccteam/dsh-client` is already
    // listed in the profile's `dsh.profile.bundles`, and that bundle's own
    // patch layer inserts the `ccteam-client` row — inserting it a second time
    // here makes Cordis abort the whole boot with
    // `duplicate loader entry id: ccteam-client`.
    //
    // dsh-app-boot's patch semantics (applyPatches): a patch carrying `insert`
    // inserts entries, while a patch with an `id` and NO `insert` looks the
    // existing entry up and copies its remaining keys onto it as overrides.
    // `name` is checked against the target and the patch is skipped on a
    // mismatch, so passing it keeps this honest rather than silently patching
    // some other plugin's row.
    //
    // `config` is the row's own plugin config — it reaches `apply(ctx, config)`
    // verbatim and is the `base` layer of the plugin's settings namespace, so
    // its keys are FLAT (`daemonUrl` / `enrollment`). Nesting them under the
    // namespace name would leave `config.enrollment` undefined and the tenant
    // instance silently unenrolled.
    let patch = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping({
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
        row
    })]);
    serde_yaml::to_string(&patch)
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

    #[test]
    fn cache_miss_then_hit_uses_sha_directory() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        assert!(first.cache_rebuilt);
        assert_eq!(
            first.cache_dir.file_name().unwrap().to_string_lossy(),
            client_tgz_sha256()
        );
        assert!(first.cache_dir.join("package.json").is_file());

        let second =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        assert!(!second.cache_rebuilt);
        assert_eq!(second.cache_dir, first.cache_dir);
    }

    #[test]
    fn profile_files_match_dsh_bundle_shape() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        let package_json: serde_json::Value =
            serde_json::from_slice(&fs::read(out.profile_dir.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(package_json["name"], "ccteam-profile");
        assert_eq!(package_json["private"], true);
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!(["@deepseek-ai/dsh-base", "@ccteam/dsh-client"])
        );
        assert_eq!(
            fs::read_to_string(out.profile_dir.join("cordis.patch.yml")).unwrap(),
            EMPTY_PATCH_YAML
        );
    }

    #[test]
    fn web_profile_contains_web_app_bundle_and_no_host_override() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(
            root.path(),
            dsh_home.path(),
            ProfileSpec::web(
                Some("ccteam-enroll:abc:secret"),
                Some("http://127.0.0.1:7331"),
            ),
        )
        .unwrap();
        let package_raw = fs::read_to_string(out.profile_dir.join("package.json")).unwrap();
        let package_json: serde_json::Value = serde_json::from_str(&package_raw).unwrap();
        assert_eq!(package_json["name"], "ccteam-ccteam-web-profile");
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/dsh-client"
            ])
        );
        let patch_raw = fs::read_to_string(out.profile_dir.join("cordis.patch.yml")).unwrap();
        assert!(!package_raw.contains("host"));
        assert!(!patch_raw.contains("host:"));

        // Structure, not substrings. Two ways this file can be well-formed on
        // its face yet break the tenant instance, both of which a `contains`
        // assertion sails straight past:
        //
        //   1. An `insert:` wrapper. The bundle list above already pulls in
        //      `@ccteam/dsh-client`, whose own patch layer inserts the
        //      `ccteam-client` row; inserting it again makes Cordis abort the
        //      boot with `duplicate loader entry id` (the instance dies before
        //      readiness and the user just sees "DSH web exited").
        //   2. A `config` nested under the settings-namespace name. The row's
        //      config reaches `apply(ctx, config)` verbatim, so the keys must
        //      be flat — nested, `config.enrollment` is undefined and the
        //      tenant instance comes up silently unenrolled.
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

        let out =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::web(None, None))
                .unwrap();
        let package_json: serde_json::Value =
            serde_json::from_slice(&fs::read(out.profile_dir.join("package.json")).unwrap())
                .unwrap();
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

    #[test]
    fn web_profile_factory_has_no_mirrored_operator_credentials() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::web(None, None)).unwrap();

        assert!(!dsh_home.path().join(".credentials.yaml").exists());
        assert!(!dsh_home.path().join("settings.yaml").exists());
    }

    #[test]
    fn web_profile_symlink_self_heals() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let wrong_target = tempfile::tempdir().unwrap();

        let first =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::web(None, None))
                .unwrap();
        let link = first
            .profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::web(None, None))
                .unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), second.cache_dir);
    }

    #[test]
    fn node_modules_entry_is_symlink_to_cache() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
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

        let first =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        let second =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();

        assert!(first.cache_rebuilt);
        assert!(!second.cache_rebuilt);
        let package_json: serde_json::Value =
            serde_json::from_slice(&fs::read(first.profile_dir.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!(["@deepseek-ai/dsh-base", "@ccteam/dsh-client"])
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

        let first =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        fs::remove_dir_all(&first.cache_dir).unwrap();
        fs::create_dir_all(&first.cache_dir).unwrap();

        let second =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
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

        let out =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
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

        let first =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
        let link = first
            .profile_dir
            .join("node_modules")
            .join(CLIENT_SCOPE)
            .join(CLIENT_PACKAGE);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second =
            materialize_profile_in(root.path(), dsh_home.path(), ProfileSpec::managed()).unwrap();
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
