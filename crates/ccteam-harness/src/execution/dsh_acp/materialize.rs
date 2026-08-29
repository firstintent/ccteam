//! Put ccteam's DSH plugin into a DSH profile — the ccteam-owned tenant
//! profile, or (merge-only) the operator's own `~/.dsh/profiles/web`.
//!
//! ONE plugin ships embedded and every profile ccteam writes gets it (see
//! [`CCTEAM_PLUGINS`]): `@ccteam/ccteam-ui` carries all three faces — the
//! ccteam tools a DSH agent calls, the ACP transport ccteam hires DSH
//! sessions through, and the ccteam workbench in the DSH web console. The
//! table stays a table: adding a second plugin means one row in it, not a new
//! code path.
//!
//! The embedded `assets/*.tgz` are checked-in assets: run
//! `dsh-plugins/pack-assets.sh` to rebuild them from `dsh-plugins/<name>/` and commit
//! the result with the Rust change. This mirrors the checked-in Pi bridge
//! asset: Rust builds must not require npm or node.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_yaml::Mapping;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::HarnessError;

const UI_TGZ: &[u8] = include_bytes!("assets/ccteam-ui.tgz");
pub const WEB_PROFILE: &str = "ccteam-web";
const DSH_BASE_BUNDLE: &str = "@deepseek-ai/dsh-base";
const DSH_WEB_APP_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
const CCTEAM_UI_BUNDLE: &str = "@ccteam/ccteam-ui";
const EMPTY_PATCH_YAML: &str = "[]\n";
const CCTEAM_SCOPE: &str = "@ccteam";
const CCTEAM_UI_ROW_ID: &str = "ccteam-ui";
const PATCH_FILE: &str = "cordis.patch.yml";

/// One embedded ccteam plugin package.
#[derive(Debug, Clone, Copy)]
struct PluginAsset {
    /// npm name, as it appears in the profile's `dsh.profile.bundles`.
    bundle: &'static str,
    /// Directory under the profile's `node_modules/@ccteam/`.
    package: &'static str,
    /// Cordis loader entry id — the one its OWN `cordis.patch.yml` inserts, so
    /// ccteam only ever overrides it (see [`merged_profile_patch_yaml`]).
    row_id: &'static str,
    /// Extraction-cache namespace under `<ccteam_home>/runtime/dsh/`. Separate
    /// per plugin so one plugin's sha never invalidates another's cache.
    cache_ns: &'static str,
    /// The one `config` key without which the plugin is inert: a bundle alone
    /// installs files, this key is what makes it a WORKING registration (see
    /// [`ccteam_plugins_registered_in_profile`]).
    required_config_key: &'static str,
    tgz: &'static [u8],
}

const UI_PLUGIN: PluginAsset = PluginAsset {
    bundle: CCTEAM_UI_BUNDLE,
    package: "ccteam-ui",
    row_id: CCTEAM_UI_ROW_ID,
    cache_ns: "ui",
    // No URL, nothing for any of the three faces to talk to.
    required_config_key: "daemonUrl",
    tgz: UI_TGZ,
};

/// Every ccteam plugin, installed into every profile ccteam writes.
///
/// Materialization is per-plugin but the SET is not a per-call-site choice: a
/// tenant home and the operator's merge-only registration install the same
/// plugins, so a new plugin reaches both by adding a row here.
const CCTEAM_PLUGINS: [PluginAsset; 1] = [UI_PLUGIN];

#[derive(Debug, Clone)]
pub struct MaterializedDshProfile {
    /// Extraction cache of each plugin ccteam materialized, in
    /// [`CCTEAM_PLUGINS`] order. A plugin the operator installed themselves is
    /// absent: ccteam extracted nothing for it (see [`OperatorInstall`]).
    pub cache_dirs: Vec<PathBuf>,
    pub profile_dir: PathBuf,
    /// `true` when at least one plugin cache had to be re-extracted.
    pub cache_rebuilt: bool,
    /// Plugins this profile already carried from the operator's own
    /// `dsh plugin add` — ccteam wrote their config row and nothing else.
    pub operator_installed: Vec<OperatorInstall>,
}

/// One ccteam plugin a merge-only profile already carries from the operator's
/// OWN `dsh plugin add`.
///
/// ccteam materializes exactly one shape: the bare bundle name in
/// `dsh.profile.bundles`, a `node_modules/@ccteam/<pkg>` symlink into
/// `<ccteam_home>/runtime/dsh/`, and never a line in pnpm's dependency table.
/// So a dependency entry, or a package directory that is not that symlink, can
/// only be the operator's. Materializing over it would swap their chosen copy
/// for ccteam's embedded one and leave the profile carrying the same plugin
/// TWICE — the shape Cordis aborts the whole boot for (`duplicate loader entry
/// id`). ccteam therefore installs nothing and writes only the config override
/// row that arms it (credentials, `daemonUrl`, transport socket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorInstall {
    /// npm name, as it appears in `dsh.profile.bundles`.
    pub bundle: &'static str,
    /// `version` from the installed package's own manifest, when readable — a
    /// dependency pnpm has not installed yet reads as `None`.
    pub version: Option<String>,
}

/// One thing ccteam found in a DSH profile it does NOT own and did not touch.
///
/// Every finding is a REPORT: `doctor` and the Hosts surface print it, the
/// operator's own `dsh plugin` command fixes it. Repairing an install ccteam
/// did not make is exactly what this module stopped doing — that is what
/// leaves the same plugin id in a profile twice and aborts the whole Cordis
/// boot (`duplicate loader entry id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshPluginFinding {
    /// npm name, as it appears in `dsh.profile.bundles`.
    pub bundle: String,
    pub kind: DshPluginFindingKind,
    /// The operator's next step. Built here so `doctor` and the Hosts panel
    /// can never word the same fix differently.
    pub remedy: String,
}

/// What ccteam found about one `@ccteam/*` entry someone else owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DshPluginFindingKind {
    /// Their install is at a version this build does not embed.
    VersionMismatch { installed: String, embedded: String },
    /// Their package is there but carries no readable `version`, so the
    /// mismatch check could not run — advisory. Not knowing is not drift, but
    /// staying silent would read as "aligned".
    VersionUnknown,
    /// pnpm's dependency table declares it and no package directory exists:
    /// the operator's own broken install, which ccteam does not take over.
    MissingOnDisk,
    /// The same bundle id is listed more than once and the rows are theirs.
    /// Cordis aborts the boot on a duplicate loader entry id; ccteam collapses
    /// only the rows it wrote itself.
    DuplicateBundleId { count: usize },
    /// More than one override row carries a ccteam loader id and they are not
    /// all ccteam's own shape — so ccteam wrote none of them rather than
    /// silently arm "the first".
    DuplicatePatchRow { row_id: &'static str, count: usize },
}

impl DshPluginFinding {
    /// Machine-readable code — the same token on every surface.
    pub fn code(&self) -> &'static str {
        match self.kind {
            DshPluginFindingKind::VersionMismatch { .. } => "plugin_version_mismatch",
            DshPluginFindingKind::VersionUnknown => "plugin_version_unknown",
            DshPluginFindingKind::MissingOnDisk => "plugin_missing_on_disk",
            DshPluginFindingKind::DuplicateBundleId { .. } => "duplicate_bundle_id",
            DshPluginFindingKind::DuplicatePatchRow { .. } => "duplicate_patch_row",
        }
    }

    /// The one wording both surfaces print, so `doctor` and the Hosts panel
    /// can never word the same finding differently.
    pub fn report(&self) -> String {
        let payload = match &self.kind {
            DshPluginFindingKind::VersionMismatch {
                installed,
                embedded,
            } => format!("installed={installed}, embedded={embedded}"),
            DshPluginFindingKind::VersionUnknown | DshPluginFindingKind::MissingOnDisk => {
                format!("id={}", self.bundle)
            }
            DshPluginFindingKind::DuplicateBundleId { count } => {
                format!("id={}, count={count}", self.bundle)
            }
            DshPluginFindingKind::DuplicatePatchRow { row_id, count } => {
                format!("id={row_id}, count={count}")
            }
        };
        format!("{} {}{{{payload}}}", self.bundle, self.code())
    }
}

/// What this call does about one plugin in one profile.
enum PluginPlan {
    /// ccteam installs it: extract on a cache miss, link it, list its bundle.
    /// Carries the extraction cache the profile links to.
    Materialize(PathBuf),
    /// The operator installed it themselves: touch none of its files and add
    /// no bundle row — only its config row (see [`OperatorInstall`], which the
    /// caller reports).
    KeepOperatorInstall,
}

/// Register ccteam's plugins into a profile ccteam does NOT own — the
/// operator's real `~/.dsh/profiles/<name>` (gate ①).
///
/// Strictly additive: the profile's `package.json` keeps every key it already
/// has (only our bundles are appended to `dsh.profile.bundles`), and the
/// profile's `cordis.patch.yml` keeps every row it already has (only ccteam's
/// own rows are written — and, since 2026-08-28, the panel row may carry the
/// operator's OWN REST token, so the patch file is kept private, 0600).
/// Unparseable JSON/YAML is an error, never a clobber.
pub fn register_ccteam_plugins_into_profile(
    ccteam_root: &Path,
    dsh_home: &Path,
    profile: &str,
    config: DshPluginConfig<'_>,
) -> Result<MaterializedDshProfile, HarnessError> {
    materialize_profile_in(
        ccteam_root,
        dsh_home,
        ProfileSpec {
            name: profile,
            vendor_bundles: &[],
            config,
            manifest: ManifestPolicy::MergeOnly,
        },
    )
}

/// Read-only, best-effort: are ccteam's plugins already registered in this
/// profile? True only when BOTH halves of gate ①'s writes are present for
/// EVERY plugin — the bundle in `dsh.profile.bundles` AND a configured
/// override row (a bundle alone installs files but leaves the plugin inert,
/// which is not a working registration). Any missing or unparseable file reads
/// as `false`; this never writes.
///
/// "Configured" is per plugin — each row of [`CCTEAM_PLUGINS`] names the one
/// config key it cannot work without. The panel's `restToken` is deliberately
/// NOT required: a resolver failure leaves the panel asking for one, which is
/// still a working registration.
pub fn ccteam_plugins_registered_in_profile(dsh_home: &Path, profile: &str) -> bool {
    let profile_dir = profile_dir_of(dsh_home, profile);
    let bundles = profile_bundles(&profile_dir);
    if !CCTEAM_PLUGINS
        .iter()
        .all(|plugin| bundles.iter().any(|b| b == plugin.bundle))
    {
        return false;
    }
    let rows = read_patch_rows(&profile_dir);
    CCTEAM_PLUGINS.iter().all(|plugin| {
        rows.iter().any(|row| {
            row.get("id").and_then(serde_yaml::Value::as_str) == Some(plugin.row_id)
                && row.get("insert").is_none()
                && row
                    .get("config")
                    .and_then(|c| c.get(plugin.required_config_key))
                    .and_then(serde_yaml::Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        })
    })
}

pub fn materialize_profile_in(
    ccteam_root: &Path,
    dsh_home: &Path,
    spec: ProfileSpec<'_>,
) -> Result<MaterializedDshProfile, HarnessError> {
    // Decide BEFORE extracting anything: a plugin the operator installed
    // themselves needs no cache, no link and no bundle row — extracting one
    // would only spend disk on a copy nothing may point at. The same snapshot
    // gates every branch below that would modify or delete something (see
    // [`ScopeOwnership`]), taken once, before this call writes anything.
    let profile_dir = profile_dir_of(dsh_home, spec.name);
    let ownership = ScopeOwnership::snapshot(&profile_dir, ccteam_root, spec.manifest);
    let mut plans = Vec::with_capacity(CCTEAM_PLUGINS.len());
    let mut cache_dirs = Vec::with_capacity(CCTEAM_PLUGINS.len());
    let mut operator_installed = Vec::new();
    let mut cache_rebuilt = false;
    for plugin in CCTEAM_PLUGINS.iter() {
        match ownership.operator_install_of(plugin) {
            Some(install) => {
                operator_installed.push(install);
                plans.push(PluginPlan::KeepOperatorInstall);
            }
            None => {
                let (cache_dir, rebuilt) = ensure_plugin_cache_dir(ccteam_root, plugin)?;
                cache_rebuilt |= rebuilt;
                cache_dirs.push(cache_dir.clone());
                plans.push(PluginPlan::Materialize(cache_dir));
            }
        }
    }
    materialize_profile_files(&profile_dir, &plans, &spec, &ownership)?;

    Ok(MaterializedDshProfile {
        cache_dirs,
        profile_dir,
        cache_rebuilt,
        operator_installed,
    })
}

fn profile_dir_of(dsh_home: &Path, profile: &str) -> PathBuf {
    dsh_home.join("profiles").join(profile)
}

/// Where ccteam extracts every plugin it materializes. A `node_modules` entry
/// pointing in here is one ccteam wrote; anything else is someone else's.
fn ccteam_plugin_cache_root(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join("runtime").join("dsh")
}

/// Who owns each `@ccteam/*` entry in one profile — the ONE gate every branch
/// that would modify or delete something asks first.
///
/// # The ownership rule (binding)
///
/// A bundle row is the USER's when
///
/// * the profile's `package.json` declares it in a pnpm dependency table
///   (`dependencies` / `devDependencies` / `optionalDependencies`), **or**
/// * its package directory exists and is not a symlink into
///   `<ccteam_home>/runtime/dsh/`.
///
/// User-owned rows, directories and patch rows are never modified or deleted —
/// only reported (see [`DshPluginFinding`]). A row with neither a dependency
/// line nor a package directory is ccteam's own: re-materializing it is
/// self-healing, not a clobber, which is how a new embedded sha still reaches
/// a profile an older ccteam linked.
///
/// Both halves matter, because ccteam materializes exactly one shape — the
/// bare bundle name in `dsh.profile.bundles`, a `node_modules/@ccteam/<pkg>`
/// symlink into `<ccteam_home>/runtime/dsh/`, and never a line in pnpm's
/// dependency table. So a dependency entry, or a package directory that is not
/// that symlink, can only be someone else's. Installing over one would swap
/// their chosen copy for ccteam's embedded one and leave the profile carrying
/// the same plugin TWICE — the shape Cordis aborts the whole boot for
/// (`duplicate loader entry id`).
///
/// [`ManifestPolicy::Owned`] (a ccteam-managed tenant home) answers "ccteam"
/// for everything: ccteam names, writes and owns that home end to end, so
/// there is no third party to defer to and the tenant path keeps its semantics
/// exactly.
///
/// A snapshot, not a live view: taken before the caller writes anything, so
/// ccteam's own writes can never turn an entry into "someone else's" halfway
/// through a call.
struct ScopeOwnership {
    /// `node_modules/@ccteam/` in this profile.
    scope_dir: PathBuf,
    /// Bundle names pnpm's dependency tables declare.
    declared: BTreeSet<String>,
    /// Package names present under [`Self::scope_dir`], mapped to whether the
    /// entry is ccteam's own symlink into its cache root.
    scope_dirs: BTreeMap<String, bool>,
    /// A home ccteam owns end to end has no third party in it.
    ccteam_owns_home: bool,
}

/// Who wrote one `@ccteam/*` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleOwner {
    /// ccteam's own: safe to re-materialize, collapse or prune.
    Ccteam,
    /// Someone else's: report it, never touch it.
    User,
}

impl ScopeOwnership {
    fn snapshot(profile_dir: &Path, ccteam_root: &Path, manifest: ManifestPolicy) -> Self {
        let scope_dir = profile_dir.join("node_modules").join(CCTEAM_SCOPE);
        let cache_root = ccteam_plugin_cache_root(ccteam_root);
        let mut scope_dirs = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(&scope_dir) {
            for entry in entries.flatten() {
                let ours =
                    fs::read_link(entry.path()).is_ok_and(|target| target.starts_with(&cache_root));
                scope_dirs.insert(entry.file_name().to_string_lossy().into_owned(), ours);
            }
        }
        Self {
            scope_dir,
            declared: declared_dependencies(profile_dir),
            scope_dirs,
            ccteam_owns_home: manifest == ManifestPolicy::Owned,
        }
    }

    /// The rule, in one place. Anything outside the `@ccteam/` scope answers
    /// [`BundleOwner::User`]: ccteam never writes there, so it never removes
    /// anything there either.
    fn owner_of(&self, bundle: &str) -> BundleOwner {
        if self.ccteam_owns_home {
            return BundleOwner::Ccteam;
        }
        let Some(package) = package_of_bundle(bundle) else {
            return BundleOwner::User;
        };
        if self.declared.contains(bundle) {
            return BundleOwner::User;
        }
        match self.scope_dirs.get(package) {
            // A directory that is not ccteam's link into its own cache: a real
            // pnpm install, or a link pointing somewhere else.
            Some(false) => BundleOwner::User,
            Some(true) | None => BundleOwner::Ccteam,
        }
    }

    fn is_ours(&self, bundle: &str) -> bool {
        self.owner_of(bundle) == BundleOwner::Ccteam
    }

    /// A stale `@ccteam/*` entry ccteam may prune: one this build's table no
    /// longer knows AND one ccteam wrote itself.
    fn is_prunable_stale(&self, name: Option<&str>) -> bool {
        name.is_some_and(|name| is_stale_ccteam_bundle(Some(name)) && self.is_ours(name))
    }

    fn package_dir_of(&self, bundle: &str) -> PathBuf {
        self.scope_dir
            .join(package_of_bundle(bundle).unwrap_or(bundle))
    }

    /// Is a package directory (or link, however broken) there at all? A
    /// declared bundle with none is the operator's own half-finished install.
    fn package_dir_present(&self, bundle: &str) -> bool {
        package_of_bundle(bundle).is_some_and(|package| self.scope_dirs.contains_key(package))
    }

    /// The operator's own copy of one plugin in this profile, or `None` when
    /// the only copy is ccteam's (or there is none).
    fn operator_install_of(&self, plugin: &PluginAsset) -> Option<OperatorInstall> {
        (self.owner_of(plugin.bundle) == BundleOwner::User).then(|| OperatorInstall {
            bundle: plugin.bundle,
            version: installed_package_version(&self.package_dir_of(plugin.bundle)),
        })
    }
}

/// The package directory name one `@ccteam/*` bundle installs into, or `None`
/// for a bundle outside ccteam's scope.
fn package_of_bundle(bundle: &str) -> Option<&str> {
    bundle.strip_prefix(CCTEAM_SCOPE)?.strip_prefix('/')
}

/// Bundles the profile's own dependency tables declare. Read-only,
/// best-effort; ccteam never writes any of these three keys.
fn declared_dependencies(profile_dir: &Path) -> BTreeSet<String> {
    let Some(manifest) = fs::read_to_string(profile_dir.join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return BTreeSet::new();
    };
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .filter_map(|table| manifest.get(table).and_then(serde_json::Value::as_object))
        .flat_map(|deps| deps.keys().cloned())
        .collect()
}

/// The bundles this profile lists, in order. Read-only, best-effort: an
/// unreadable or unparseable manifest lists none.
fn profile_bundles(profile_dir: &Path) -> Vec<String> {
    fs::read_to_string(profile_dir.join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|manifest| {
            Some(
                manifest
                    .get("dsh")?
                    .get("profile")?
                    .get("bundles")?
                    .as_array()?
                    .iter()
                    .filter_map(|b| b.as_str().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn installed_package_version(package_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(package_dir.join("package.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

/// `version` of the plugin ccteam embeds, read out of the tarball itself so it
/// can never drift from the bytes that get installed.
fn embedded_plugin_version(plugin: &PluginAsset) -> Option<String> {
    use std::io::Read as _;
    let mut archive = Archive::new(GzDecoder::new(plugin.tgz));
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?.into_owned();
        if strip_npm_package_prefix(&path).as_deref() != Some(Path::new("package.json")) {
            continue;
        }
        let mut raw = String::new();
        entry.read_to_string(&mut raw).ok()?;
        return serde_json::from_str::<serde_json::Value>(&raw)
            .ok()?
            .get("version")?
            .as_str()
            .map(str::to_string);
    }
    None
}

/// Read-only, best-effort: what this profile carries that is the operator's
/// own — and that ccteam therefore left exactly as it found it.
///
/// ccteam does not repair an install it did not make, so drift, duplicates and
/// half-finished installs are REPORTED (`doctor`, the Hosts surface) and left
/// for the operator's own `dsh plugin ...` command to fix. Anything missing or
/// unreadable answers "no finding": not knowing is not a finding. Never
/// writes.
pub fn ccteam_plugin_findings(
    ccteam_root: &Path,
    dsh_home: &Path,
    profile: &str,
) -> Vec<DshPluginFinding> {
    let profile_dir = profile_dir_of(dsh_home, profile);
    let ownership = ScopeOwnership::snapshot(&profile_dir, ccteam_root, ManifestPolicy::MergeOnly);
    let mut findings = Vec::new();
    let finding = |bundle: String, kind: DshPluginFindingKind| DshPluginFinding {
        remedy: plugin_finding_remedy(&profile_dir, profile, &bundle, &kind),
        bundle,
        kind,
    };

    // Duplicates first: a profile that cannot boot outranks a version digit.
    // ccteam collapses the duplicate rows it wrote itself, so one that
    // survives a registration is one of theirs.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for bundle in profile_bundles(&profile_dir) {
        if package_of_bundle(&bundle).is_some() {
            *counts.entry(bundle).or_default() += 1;
        }
    }
    findings.extend(counts.into_iter().filter_map(|(bundle, count)| {
        (count > 1 && !ownership.is_ours(&bundle))
            .then(|| finding(bundle, DshPluginFindingKind::DuplicateBundleId { count }))
    }));

    let rows = read_patch_rows(&profile_dir);
    for plugin in CCTEAM_PLUGINS.iter() {
        // Override rows ccteam did not write, sharing a loader id with the row
        // it does write: it armed none of them rather than pick one.
        let carrying = override_row_indices(&rows, plugin);
        if carrying.len() > 1
            && !carrying
                .iter()
                .all(|index| is_ccteam_patch_row(&rows[*index], plugin))
        {
            findings.push(finding(
                plugin.bundle.to_string(),
                DshPluginFindingKind::DuplicatePatchRow {
                    row_id: plugin.row_id,
                    count: carrying.len(),
                },
            ));
        }

        if ownership.is_ours(plugin.bundle) {
            continue;
        }
        let kind = if !ownership.package_dir_present(plugin.bundle) {
            DshPluginFindingKind::MissingOnDisk
        } else {
            match installed_package_version(&ownership.package_dir_of(plugin.bundle)) {
                None => DshPluginFindingKind::VersionUnknown,
                Some(installed) => match embedded_plugin_version(plugin) {
                    Some(embedded) if installed != embedded => {
                        DshPluginFindingKind::VersionMismatch {
                            installed,
                            embedded,
                        }
                    }
                    _ => continue,
                },
            }
        };
        findings.push(finding(plugin.bundle.to_string(), kind));
    }
    findings
}

/// The operator's next step for one finding — one wording, both surfaces.
fn plugin_finding_remedy(
    profile_dir: &Path,
    profile: &str,
    bundle: &str,
    kind: &DshPluginFindingKind,
) -> String {
    let dir = profile_dir.display();
    match kind {
        DshPluginFindingKind::VersionMismatch { .. } => format!(
            "your own install in {dir}, left untouched; update it with \
             `dsh plugin --profile {profile} update {bundle}`"
        ),
        DshPluginFindingKind::VersionUnknown => format!(
            "your own install in {dir} carries no readable `version`, so the version check did \
             not run; reinstall it with `dsh plugin --profile {profile} add {bundle}` if that \
             copy is broken"
        ),
        DshPluginFindingKind::MissingOnDisk => format!(
            "declared in {dir}/package.json but nothing is installed; ccteam did not take your \
             install over — finish it with `dsh plugin --profile {profile} add {bundle}`"
        ),
        DshPluginFindingKind::DuplicateBundleId { .. } => format!(
            "listed more than once in {dir}/package.json (`dsh.profile.bundles`) — Cordis aborts \
             the whole boot on a duplicate loader entry id; ccteam touched none of your rows, so \
             remove the extra one (`dsh plugin --profile {profile} remove {bundle}`, then add it \
             once)"
        ),
        DshPluginFindingKind::DuplicatePatchRow { .. } => format!(
            "more than one override row carries this loader id in {dir}/{PATCH_FILE}; ccteam \
             wrote none of them rather than arm one and leave the other — keep the row you want \
             and delete the rest"
        ),
    }
}

/// This profile's patch rows. Read-only, best-effort: a missing or
/// unparseable file has none.
fn read_patch_rows(profile_dir: &Path) -> Vec<serde_yaml::Value> {
    fs::read_to_string(profile_dir.join(PATCH_FILE))
        .ok()
        .and_then(|raw| serde_yaml::from_str::<serde_yaml::Value>(&raw).ok())
        .and_then(|patch| patch.as_sequence().cloned())
        .unwrap_or_default()
}

/// The `ccteam-ui` row's own plugin config. Reaches `apply(ctx, config)`
/// verbatim, so these keys are FLAT (see [`merged_profile_patch_yaml`]).
///
/// One row, one struct: the plugin has three faces and this is what arms each
/// of them. `restToken` and `enrollment` are both credentials and both this
/// identity's own — the panel reads the team as the human, the tools call the
/// daemon as this DSH process (owner decision 2026-08-28: the operator's own
/// admin web token goes into the operator's own profile; pasting a token is
/// for a hand-started `dsh web` only). The file is written 0600 and neither
/// value is ever logged or reported. `defaultProject` is deliberately absent
/// — ccteam has no per-identity default worth pinning, so the panel asks.
#[derive(Debug, Clone, Copy, Default)]
pub struct DshPluginConfig<'a> {
    pub daemon_url: Option<&'a str>,
    /// MCP enrollment credential (`ccteam-enroll:<id>:<secret>`) for the tool
    /// face. `None` on the operator branch: that one stays the human's to paste.
    pub enrollment: Option<&'a str>,
    /// Unix socket the plugin serves ACP on. Empty/absent = no transport — the
    /// plugin arms its listener on this key alone.
    pub transport_socket: Option<&'a str>,
    /// Full wire form the REST API accepts (`ccteam:<hex>`), for the workbench.
    pub rest_token: Option<&'a str>,
}

impl DshPluginConfig<'_> {
    fn is_empty(&self) -> bool {
        self.daemon_url.is_none()
            && self.enrollment.is_none()
            && self.transport_socket.is_none()
            && self.rest_token.is_none()
    }

    fn entries(&self) -> [(&'static str, Option<&str>); 4] {
        [
            ("daemonUrl", self.daemon_url),
            ("enrollment", self.enrollment),
            ("transportSocket", self.transport_socket),
            ("restToken", self.rest_token),
        ]
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
    /// Vendor bundles this profile needs. ccteam's own bundles are NOT listed
    /// here — [`CCTEAM_PLUGINS`] is always installed, so a new ccteam plugin
    /// never needs a second edit at the call sites.
    pub vendor_bundles: &'static [&'static str],
    pub config: DshPluginConfig<'a>,
    pub manifest: ManifestPolicy,
}

impl<'a> ProfileSpec<'a> {
    /// The ccteam-owned `dsh web` profile in a managed tenant home.
    pub fn web(config: DshPluginConfig<'a>) -> Self {
        Self {
            name: WEB_PROFILE,
            vendor_bundles: &[DSH_BASE_BUNDLE, DSH_WEB_APP_BUNDLE],
            config,
            manifest: ManifestPolicy::Owned,
        }
    }
}

pub fn ui_tgz_sha256() -> String {
    format!("{:x}", Sha256::digest(UI_TGZ))
}

/// `<ccteam_home>/runtime/dsh/<ns>/<sha>/` for one plugin, extracting it on a
/// cache miss. Returns the cache dir and whether this call rebuilt it.
fn ensure_plugin_cache_dir(
    ccteam_root: &Path,
    plugin: &PluginAsset,
) -> Result<(PathBuf, bool), HarnessError> {
    let cache_base = ccteam_root
        .join("runtime")
        .join("dsh")
        .join(plugin.cache_ns);
    fs::create_dir_all(&cache_base).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH plugin cache {}: {e}",
            cache_base.display()
        ))
    })?;
    set_private_dir(&cache_base)?;

    let hash = format!("{:x}", Sha256::digest(plugin.tgz));
    let cache_dir = cache_base.join(&hash);
    let rebuilt = ensure_plugin_cache(&cache_base, &cache_dir, &hash, plugin)?;
    Ok((cache_dir, rebuilt))
}

fn ensure_plugin_cache(
    cache_base: &Path,
    cache_dir: &Path,
    hash: &str,
    plugin: &PluginAsset,
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
            "create temp DSH plugin cache {}: {e}",
            tmp.display()
        ))
    })?;

    let result = extract_plugin_tgz(&tmp, plugin)
        .and_then(|_| {
            if cache_looks_usable(&tmp) {
                Ok(())
            } else {
                Err(HarnessError::SpawnFailed(format!(
                    "embedded archive for {} did not produce a package root",
                    plugin.bundle
                )))
            }
        })
        .and_then(|_| {
            fs::rename(&tmp, cache_dir).or_else(|e| {
                if cache_looks_usable(cache_dir) {
                    let _ = fs::remove_dir_all(&tmp);
                    Ok(())
                } else {
                    Err(HarnessError::SpawnFailed(format!(
                        "publish DSH plugin cache {} -> {}: {e}",
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

fn extract_plugin_tgz(dst: &Path, plugin: &PluginAsset) -> Result<(), HarnessError> {
    let reader = GzDecoder::new(plugin.tgz);
    let mut archive = Archive::new(reader);
    let bundle = plugin.bundle;
    let entries = archive
        .entries()
        .map_err(|e| HarnessError::SpawnFailed(format!("read embedded {bundle} archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            HarnessError::SpawnFailed(format!("read embedded {bundle} archive entry: {e}"))
        })?;
        let raw_path = entry.path().map_err(|e| {
            HarnessError::SpawnFailed(format!("read embedded {bundle} archive path: {e}"))
        })?;
        let Some(rel) = strip_npm_package_prefix(&raw_path) else {
            continue;
        };
        let out = dst.join(rel);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&out).map_err(|e| {
                HarnessError::SpawnFailed(format!("create DSH plugin dir {}: {e}", out.display()))
            })?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                HarnessError::SpawnFailed(format!(
                    "create DSH plugin parent {}: {e}",
                    parent.display()
                ))
            })?;
        }
        entry.unpack(&out).map_err(|e| {
            HarnessError::SpawnFailed(format!("unpack DSH plugin file {}: {e}", out.display()))
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
    profile_dir: &Path,
    plans: &[PluginPlan],
    spec: &ProfileSpec<'_>,
    ownership: &ScopeOwnership,
) -> Result<(), HarnessError> {
    fs::create_dir_all(profile_dir).map_err(|e| {
        HarnessError::SpawnFailed(format!("create DSH profile {}: {e}", profile_dir.display()))
    })?;

    let package_json = merged_profile_package_json(profile_dir, plans, spec, ownership)?;
    write_document_if_changed(
        &profile_dir.join("package.json"),
        &package_json,
        DocumentKind::Json,
    )?;
    let patch_path = profile_dir.join(PATCH_FILE);
    if let Some(patch_yaml) = merged_profile_patch_yaml(&patch_path, spec, ownership)? {
        write_document_if_changed(&patch_path, &patch_yaml, DocumentKind::Yaml)?;
    }
    // The patch may carry this identity's credentials (`restToken`, and a
    // tenant's `enrollment`): private to the OS user, whatever the umask made
    // it — including a file ccteam wrote before it carried any.
    if patch_path.is_file() {
        set_private_file(&patch_path)?;
    }

    let scope_dir = profile_dir.join("node_modules").join(CCTEAM_SCOPE);
    fs::create_dir_all(&scope_dir).map_err(|e| {
        HarnessError::SpawnFailed(format!(
            "create DSH profile package scope {}: {e}",
            scope_dir.display()
        ))
    })?;
    for (plugin, plan) in CCTEAM_PLUGINS.iter().zip(plans) {
        // An operator-installed plugin keeps every file pnpm put there: the
        // link is theirs, and replacing it would install ccteam's copy of the
        // same plugin id alongside their own.
        if let PluginPlan::Materialize(cache_dir) = plan {
            ensure_symlink(&scope_dir.join(plugin.package), cache_dir)?;
        }
    }
    // A link left behind by a former plugin name points at a cache nobody
    // maintains — but only the entries ccteam wrote are ccteam's to remove
    // (see [`ScopeOwnership`]): a `@ccteam/*` package the operator installed
    // themselves survives here byte-for-byte and is reported instead.
    if let Ok(entries) = fs::read_dir(&scope_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if CCTEAM_PLUGINS
                .iter()
                .any(|plugin| OsStr::new(plugin.package) == name.as_os_str())
            {
                continue;
            }
            let bundle = format!("{CCTEAM_SCOPE}/{}", name.to_string_lossy());
            if ownership.is_ours(&bundle) {
                remove_existing(&entry.path())?;
            }
        }
    }
    Ok(())
}

/// A bundle name under ccteam's npm scope that is NOT one of [`CCTEAM_PLUGINS`].
///
/// A bundle, patch row, or scope link under `@ccteam/` that this table no
/// longer knows is a former ccteam plugin (renamed or retired). Left in place
/// it resolves to a package nobody maintains — or to nothing — and its own
/// patch layer keeps inserting a row id the successor package also inserts,
/// which aborts the whole Cordis boot (`duplicate loader entry id`).
///
/// Stale is only half the question: ccteam prunes an entry only when it also
/// WROTE it (see [`ScopeOwnership::is_prunable_stale`]). A `@ccteam/*` package
/// the operator installed themselves is theirs whatever this table knows —
/// nothing outside ccteam's own writes is ever removed.
fn is_stale_ccteam_bundle(name: Option<&str>) -> bool {
    name.is_some_and(|name| {
        package_of_bundle(name).is_some()
            && !CCTEAM_PLUGINS.iter().any(|plugin| plugin.bundle == name)
    })
}

fn merged_profile_package_json(
    profile_dir: &Path,
    plans: &[PluginPlan],
    spec: &ProfileSpec<'_>,
    ownership: &ScopeOwnership,
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
    // profile whose bundles are only ccteam's cannot boot: the
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
    // One pass over ccteam's own scope: drop the rows for retired ccteam
    // plugins, and collapse a bundle id ccteam listed twice to one row. Both
    // are gated on ownership (see [`ScopeOwnership`]) — a `@ccteam/*` row the
    // operator wrote survives even when it is stale or duplicated, and the
    // scan reports it (`duplicate_bundle_id`) instead. Rows outside the scope
    // are never inspected.
    let mut kept: BTreeSet<String> = BTreeSet::new();
    bundles.retain(|value| {
        let Some(name) = value.as_str() else {
            return true;
        };
        if package_of_bundle(name).is_none() {
            return true;
        }
        let ours = ownership.is_ours(name);
        if ours && is_stale_ccteam_bundle(Some(name)) {
            return false;
        }
        // A repeat of an id already kept: ccteam's own collapses, theirs stays
        // exactly as they left it.
        kept.insert(name.to_string()) || !ours
    });
    // Only the plugins ccteam installs get a bundle row. An operator-installed
    // one is already a layer by `dsh plugin`'s own reconciliation — and if
    // that reconciliation dropped it, adding it back is ccteam overruling DSH
    // about the operator's own package.
    let ours = CCTEAM_PLUGINS
        .iter()
        .zip(plans)
        .filter(|(_, plan)| matches!(plan, PluginPlan::Materialize(_)))
        .map(|(plugin, _)| plugin.bundle);
    for required in scaffold
        .iter()
        .copied()
        .chain(spec.vendor_bundles.iter().copied())
        .chain(ours)
    {
        if !bundles.iter().any(|v| v.as_str() == Some(required)) {
            bundles.push(serde_json::Value::String(required.to_string()));
        }
    }

    serde_json::to_string_pretty(&value)
        .map(|mut body| {
            body.push('\n');
            body
        })
        .map_err(|e| HarnessError::SpawnFailed(format!("serialize DSH profile package: {e}")))
}

/// Upsert ONLY ccteam's own rows into the profile's patch list, preserving
/// every other row byte-for-byte in meaning. `Ok(None)` = nothing to write.
///
/// An OVERRIDE patch, never an `insert` one. Each ccteam bundle is in the
/// profile's `dsh.profile.bundles`, and each bundle's own patch layer
/// (`dsh-plugins/<name>/cordis.patch.yml`) inserts its row — inserting it a second
/// time here makes Cordis abort the whole boot with `duplicate loader entry
/// id: <row>`. That shipped once (v0.10.0) and killed every tenant instance
/// before readiness; it holds for the operator's own profile just as much, so
/// this writer has no `insert` arm at all.
///
/// dsh-app-boot's patch semantics (applyPatches): a patch carrying `insert`
/// inserts entries, while a patch with an `id` and NO `insert` looks the
/// existing entry up and copies its remaining keys onto it as overrides.
/// `name` is checked against the target and the patch is skipped on a
/// mismatch, so passing it keeps this honest rather than silently patching
/// some other plugin's row.
///
/// One override row per loader id, always: ccteam collapses the duplicates it
/// wrote itself (see [`is_ccteam_patch_row`]) onto the first and updates that
/// one. When a row ccteam did NOT write shares the id, it updates none of them
/// — arming one and leaving the other is the silent half of the bug — and the
/// scan reports `duplicate_patch_row` for the operator to resolve. Pruning is
/// gated the same way as everywhere else in this module: a stale row naming a
/// `@ccteam/*` package the operator installed is theirs and stays.
///
/// `config` is the row's own plugin config — it reaches `apply(ctx, config)`
/// verbatim and is the `base` layer of the plugin's settings namespace, so its
/// keys are FLAT (`daemonUrl` / `enrollment` / `transportSocket` / `restToken`).
/// Nesting them under the namespace name would leave `config.transportSocket`
/// undefined and the instance would come up with no ACP transport at all.
fn merged_profile_patch_yaml(
    path: &Path,
    spec: &ProfileSpec<'_>,
    ownership: &ScopeOwnership,
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

    // Each plugin whose config this call carries. A plugin with nothing to
    // configure is left alone entirely — its own bundle patch layer already
    // inserted the row, and an empty override would say nothing.
    let mut ours: Vec<(&PluginAsset, serde_yaml::Mapping)> = Vec::new();
    if !spec.config.is_empty() {
        ours.push((&UI_PLUGIN, flat_config(spec.config.entries())));
    }
    // Rows naming a former ccteam package ccteam itself wrote are pruned
    // whatever their id; every other row survives byte-for-byte.
    let before = existing.len();
    let mut rows: Vec<serde_yaml::Value> = existing
        .into_iter()
        .filter(|row| {
            !ownership.is_prunable_stale(row.get("name").and_then(serde_yaml::Value::as_str))
        })
        .collect();
    let pruned = rows.len() != before;

    if ours.is_empty() && !pruned {
        // No config of ours to install and nothing stale: leave an existing
        // file exactly as it is, and only create the empty list when the
        // profile has none.
        return Ok((!path.exists()).then(|| EMPTY_PATCH_YAML.to_string()));
    }

    for (plugin, config) in ours {
        let carrying = override_row_indices(&rows, plugin);
        if carrying.len() > 1
            && !carrying
                .iter()
                .all(|index| is_ccteam_patch_row(&rows[*index], plugin))
        {
            // A row ccteam did not write shares this loader id. Updating "the
            // first" would arm one copy and leave the other; the scan reports
            // it instead (`duplicate_patch_row`).
            continue;
        }
        // Duplicates ccteam wrote collapse onto the first — the invariant is
        // one override row per loader id. Removing from the back keeps the
        // earlier indices (and `carrying[0]`) valid.
        for index in carrying.iter().skip(1).rev() {
            rows.remove(*index);
        }
        match carrying.first() {
            Some(&index) => {
                let row = rows[index].as_mapping_mut().ok_or_else(|| {
                    HarnessError::SpawnFailed(format!(
                        "DSH profile patch {} row `{}` must be a mapping",
                        path.display(),
                        plugin.row_id
                    ))
                })?;
                row.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(plugin.bundle.to_string()),
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
                    serde_yaml::Value::String(plugin.row_id.to_string()),
                );
                row.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(plugin.bundle.to_string()),
                );
                row.insert(
                    serde_yaml::Value::String("config".to_string()),
                    serde_yaml::Value::Mapping(config),
                );
                rows.push(serde_yaml::Value::Mapping(row));
            }
        }
    }

    serde_yaml::to_string(&serde_yaml::Value::Sequence(rows))
        .map(Some)
        .map_err(|e| HarnessError::SpawnFailed(format!("serialize DSH profile patch: {e}")))
}

/// Every row that overrides this plugin's loader entry: its `id`, and no
/// `insert` (an insert row is a different statement — it ADDS the entry — and
/// ccteam never writes one).
fn override_row_indices(rows: &[serde_yaml::Value], plugin: &PluginAsset) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            row.get("id").and_then(serde_yaml::Value::as_str) == Some(plugin.row_id)
                && row.get("insert").is_none()
        })
        .map(|(index, _)| index)
        .collect()
}

/// Is this override row one ccteam wrote? ccteam writes exactly three keys
/// (`id`, `name`, `config`) naming its own bundle, so a row carrying any other
/// key — or naming another package — is the operator's, whatever id it holds.
/// Shape is the only evidence available: the row for a plugin the operator
/// installed is still written by ccteam (that config row is what arms their
/// copy), so "whose bundle is it" cannot answer this one.
fn is_ccteam_patch_row(row: &serde_yaml::Value, plugin: &PluginAsset) -> bool {
    let Some(mapping) = row.as_mapping() else {
        return false;
    };
    mapping
        .keys()
        .all(|key| matches!(key.as_str(), Some("id" | "name" | "config")))
        && row
            .get("name")
            .is_none_or(|name| name.as_str() == Some(plugin.bundle))
}

/// A row's `config` mapping: present keys only, FLAT, in declaration order.
fn flat_config<'a>(entries: impl IntoIterator<Item = (&'static str, Option<&'a str>)>) -> Mapping {
    let mut config = Mapping::new();
    for (key, value) in entries {
        if let Some(value) = value {
            config.insert(
                serde_yaml::Value::String(key.to_string()),
                serde_yaml::Value::String(value.to_string()),
            );
        }
    }
    config
}

/// Which parser answers "do these two files SAY the same thing?".
#[derive(Clone, Copy)]
enum DocumentKind {
    Json,
    Yaml,
}

impl DocumentKind {
    /// Same document? A side this build cannot parse answers `false`: an
    /// unreadable file is not evidence that ccteam's rows are already in it.
    fn same_document(self, a: &str, b: &str) -> bool {
        match self {
            DocumentKind::Json => match (
                serde_json::from_str::<serde_json::Value>(a),
                serde_json::from_str::<serde_json::Value>(b),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            },
            DocumentKind::Yaml => match (
                serde_yaml::from_str::<serde_yaml::Value>(a),
                serde_yaml::from_str::<serde_yaml::Value>(b),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            },
        }
    }
}

/// Write `body` to `path` only when it SAYS something the file does not
/// already say — compared as a parsed document, never as bytes.
///
/// Re-registration must cost ZERO writes: the daemon registers on every start
/// and every `register-mcp` call registers again. ccteam rebuilds both profile
/// files from a parsed model, and serializing normalizes formatting the
/// operator never asked to change (`serde_json` sorts object keys;
/// `serde_yaml` renders a flow mapping as a block one and prefers single
/// quotes), so a byte comparison calls a file "changed" that says exactly what
/// it said before — and rewrites the operator's own profile on every run.
///
/// Honest about what the FIRST ccteam write costs: it does normalize that
/// file's formatting, and `serde_yaml` drops YAML comments (it parses to a
/// value, not to a syntax tree that could carry them). That is a one-time cost
/// on a file ccteam legitimately edits — a normalized file compares equal on
/// every later run, so nothing here re-normalizes what it already normalized,
/// and comments the operator adds after that survive as long as the document
/// keeps saying the same thing. A comment-preserving round-trip needs a
/// CST-level YAML parser, and no crate in this workspace's graph has one
/// (`serde_yaml` 0.9 is the only YAML in it); pulling in a new dependency to
/// keep comments through a re-formatting write is not a trade this makes.
fn write_document_if_changed(
    path: &Path,
    body: &str,
    kind: DocumentKind,
) -> Result<(), HarnessError> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == body || kind.same_document(&existing, body) {
            return Ok(());
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            HarnessError::SpawnFailed(format!("create parent {}: {e}", parent.display()))
        })?;
    }
    replace_file(path, body.as_bytes())
}

/// Put `bytes` at `path` in one step: sibling temp file, then rename over the
/// target. A concurrent reader — DSH's own boot, or `dsh plugin`'s pnpm — sees
/// either the old file or the new one, never the truncated middle of an
/// in-place write.
///
/// The temp file takes the destination's current mode when there is one, so
/// the patch file (it carries this identity's credentials) stays 0600 across
/// the swap rather than being world-readable for the window between the rename
/// and the chmod behind it. No fsync: these are re-derivable registration
/// files, not the durability-critical state
/// [`crate::execution::fs_atomic::atomic_write_durable`] exists for — losing
/// one to a power cut costs a re-registration, which the next daemon start
/// does anyway.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), HarnessError> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        now_nanos()
    ));
    let swap = (|| -> std::io::Result<()> {
        fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(path) {
                let mode = meta.permissions().mode() & 0o7777;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
            }
        }
        fs::rename(&tmp, path)
    })();
    swap.map_err(|e| {
        let _ = fs::remove_file(&tmp);
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

fn set_private_file(path: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Already private: chmod-ing it again is a change to a file a
        // re-registration is supposed to leave entirely alone.
        if fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o7777 == 0o600) {
            return Ok(());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| {
            HarnessError::SpawnFailed(format!("chmod 0600 {}: {e}", path.display()))
        })?;
    }
    Ok(())
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
    const REST_TOKEN: &str = "ccteam:0123456789abcdef0123456789abcdef";

    fn plain_web() -> ProfileSpec<'static> {
        ProfileSpec::web(DshPluginConfig::default())
    }

    fn wired_web() -> ProfileSpec<'static> {
        ProfileSpec::web(DshPluginConfig {
            daemon_url: Some("http://127.0.0.1:7331"),
            enrollment: Some("ccteam-enroll:abc:secret"),
            transport_socket: Some(SOCKET),
            rest_token: Some(REST_TOKEN),
        })
    }

    /// The operator's merge-only registration as the runtime issues it: URL,
    /// transport socket and the operator's own REST token — and no enrollment,
    /// which stays theirs to paste.
    fn operator_register(
        root: &Path,
        dsh_home: &Path,
        socket: &str,
    ) -> Result<MaterializedDshProfile, HarnessError> {
        register_ccteam_plugins_into_profile(
            root,
            dsh_home,
            "web",
            DshPluginConfig {
                daemon_url: Some("http://127.0.0.1:7331"),
                transport_socket: Some(socket),
                rest_token: Some(REST_TOKEN),
                ..DshPluginConfig::default()
            },
        )
    }

    fn plugin_link(profile_dir: &Path, plugin: &PluginAsset) -> PathBuf {
        profile_dir
            .join("node_modules")
            .join(CCTEAM_SCOPE)
            .join(plugin.package)
    }

    /// The cache this call extracted for one plugin, found by its namespace —
    /// a plugin ccteam did not materialize has no entry at all.
    fn cache_of(out: &MaterializedDshProfile, plugin: &PluginAsset) -> PathBuf {
        out.cache_dirs
            .iter()
            .find(|dir| dir.parent().and_then(Path::file_name) == Some(OsStr::new(plugin.cache_ns)))
            .expect("plugin was materialized")
            .clone()
    }

    /// A profile carrying the operator's OWN `dsh plugin add` of the plugin:
    /// pnpm's dependency line, the bundle already reconciled into the layer
    /// list, and a real package directory (not one of ccteam's symlinks).
    fn operator_installed_profile(dsh_home: &Path, version: &str) -> PathBuf {
        let profile_dir = dsh_home.join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "private": true,
                "dependencies": {CCTEAM_UI_BUNDLE: "^0.10.4", "@user/other": "1.0.0"},
                "dsh": {"profile": {"bundles": [
                    "@deepseek-ai/dsh-base",
                    "@deepseek-ai/dsh-web-app",
                    CCTEAM_UI_BUNDLE
                ]}}
            })
            .to_string(),
        )
        .unwrap();
        let package_dir = profile_dir
            .join("node_modules")
            .join(CCTEAM_SCOPE)
            .join(UI_PLUGIN.package);
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("package.json"),
            serde_json::json!({"name": CCTEAM_UI_BUNDLE, "version": version}).to_string(),
        )
        .unwrap();
        fs::write(package_dir.join("marker.txt"), b"pnpm put me here").unwrap();
        profile_dir
    }

    fn bundle_entries(profile_dir: &Path) -> Vec<String> {
        read_package(profile_dir)["dsh"]["profile"]["bundles"]
            .as_array()
            .expect("bundles is an array")
            .iter()
            .map(|v| v.as_str().expect("bundle is a string").to_string())
            .collect()
    }

    fn read_patch(profile_dir: &Path) -> serde_yaml::Value {
        let raw = fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap();
        serde_yaml::from_str(&raw).unwrap()
    }

    fn read_package(profile_dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(profile_dir.join("package.json")).unwrap()).unwrap()
    }

    fn row_with_id(patch: &serde_yaml::Value, id: &str) -> serde_yaml::Value {
        patch
            .as_sequence()
            .expect("patch is a sequence")
            .iter()
            .find(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("row {id} present in {patch:?}"))
            .clone()
    }

    fn ccteam_row(patch: &serde_yaml::Value) -> serde_yaml::Value {
        row_with_id(patch, CCTEAM_UI_ROW_ID)
    }

    #[test]
    fn cache_miss_then_hit_uses_sha_directory() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(first.cache_rebuilt);
        let cache = cache_of(&first, &UI_PLUGIN);
        assert_eq!(
            cache.file_name().unwrap().to_string_lossy(),
            ui_tgz_sha256()
        );
        assert!(cache.join("package.json").is_file());

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(!second.cache_rebuilt);
        assert_eq!(second.cache_dirs, first.cache_dirs);
    }

    /// Each plugin caches under its OWN sha, in its own namespace: bumping one
    /// plugin must not invalidate (or, worse, collide with) another's cache.
    #[test]
    fn every_plugin_extracts_into_its_own_sha_namespace() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert_eq!(out.cache_dirs.len(), CCTEAM_PLUGINS.len());
        let caches: std::collections::BTreeSet<&PathBuf> = out.cache_dirs.iter().collect();
        assert_eq!(caches.len(), CCTEAM_PLUGINS.len(), "one cache per plugin");
        let ui = cache_of(&out, &UI_PLUGIN);
        assert_eq!(
            ui,
            root.path()
                .join("runtime")
                .join("dsh")
                .join("ui")
                .join(ui_tgz_sha256())
        );
        assert!(
            ui.join("package.json").is_file(),
            "the ui plugin is extracted, not just named"
        );
        assert_eq!(
            fs::read_to_string(ui.join("package.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|manifest| manifest["name"].as_str().map(str::to_string)),
            Some(CCTEAM_UI_BUNDLE.to_string()),
            "the team cache holds the team package"
        );
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
                "@ccteam/ccteam-ui"
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
        //      `@ccteam/ccteam-ui`, whose own patch layer inserts the
        //      `ccteam-ui` row; inserting it again makes Cordis abort the
        //      boot with `duplicate loader entry id`.
        //   2. A `config` nested under the settings-namespace name. The row's
        //      config reaches `apply(ctx, config)` verbatim, so the keys must
        //      be flat — nested, `config.transportSocket` is undefined and the
        //      instance serves no ACP socket at all.
        //   3. A missing `transportSocket`: the tool surface still works, so
        //      only a hire (never a boot log) would notice.
        let patch: serde_yaml::Value = serde_yaml::from_str(&patch_raw).unwrap();
        let rows = patch.as_sequence().expect("patch is a sequence");
        assert_eq!(rows.len(), 1, "one row per configured plugin: {patch_raw}");
        let row = ccteam_row(&patch);
        assert!(
            row.get("insert").is_none(),
            "must OVERRIDE the bundle-inserted row, never insert a duplicate: {patch_raw}"
        );
        assert_eq!(row["id"], serde_yaml::Value::String("ccteam-ui".into()));
        assert_eq!(
            row["name"],
            serde_yaml::Value::String("@ccteam/ccteam-ui".into()),
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
            config.get("ccteam-ui").is_none(),
            "config keys are flat, not nested under the namespace: {patch_raw}"
        );
    }

    /// The SAME row, from the workbench's side: the panel is useless without
    /// the identity's own REST bearer, and that bearer belongs ONLY in this
    /// file. One plugin means the tool face's keys and the panel's keys share
    /// a row, so both halves of it are asserted.
    #[test]
    fn team_row_carries_the_flat_daemon_url_and_rest_token() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();
        let patch_raw = fs::read_to_string(out.profile_dir.join("cordis.patch.yml")).unwrap();
        let patch: serde_yaml::Value = serde_yaml::from_str(&patch_raw).unwrap();
        let row = row_with_id(&patch, CCTEAM_UI_ROW_ID);
        assert!(
            row.get("insert").is_none(),
            "`@ccteam/ccteam-ui`'s own bundle patch already inserted this id; a \
             second insert aborts the Cordis boot: {patch_raw}"
        );
        assert_eq!(
            row["name"],
            serde_yaml::Value::String(CCTEAM_UI_BUNDLE.into()),
            "name guards against patching some other plugin's row"
        );
        let config = row["config"].as_mapping().expect("flat plugin config");
        assert_eq!(
            config["daemonUrl"],
            serde_yaml::Value::String("http://127.0.0.1:7331".into())
        );
        assert_eq!(
            config["restToken"],
            serde_yaml::Value::String(REST_TOKEN.into()),
            "the wire form the REST API accepts, prefix included"
        );
        assert!(
            config.get("ccteam-ui").is_none(),
            "config keys are flat, not nested under the namespace: {patch_raw}"
        );
        assert!(
            config.get("defaultProject").is_none(),
            "ccteam pins no default project — the panel asks: {patch_raw}"
        );

        // The credential is in the identity's own profile and nowhere else.
        assert!(
            !fs::read_to_string(out.profile_dir.join("package.json"))
                .unwrap()
                .contains(REST_TOKEN),
            "the manifest is not a place for credentials"
        );
    }

    /// Zero user steps means the panel is installed AND wired on the first
    /// start, and stays exactly once-installed on every later one.
    #[test]
    fn repeated_tenant_materialization_keeps_one_team_row_and_one_bundle() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();
        let second = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();
        assert_eq!(second.profile_dir, first.profile_dir);

        let patch = read_patch(&second.profile_dir);
        let rows = patch.as_sequence().unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.get("id").and_then(serde_yaml::Value::as_str)
                    == Some(CCTEAM_UI_ROW_ID))
                .count(),
            1,
            "exactly one team row after re-materializing: {patch:?}"
        );
        let package = read_package(&second.profile_dir);
        let bundles = package["dsh"]["profile"]["bundles"].as_array().unwrap();
        assert_eq!(
            bundles
                .iter()
                .filter(|b| b.as_str() == Some(CCTEAM_UI_BUNDLE))
                .count(),
            1,
            "exactly one team bundle entry: {bundles:?}"
        );
        assert!(
            second
                .profile_dir
                .join("node_modules")
                .join(CCTEAM_SCOPE)
                .join(UI_PLUGIN.package)
                .join("package.json")
                .is_file(),
            "the panel package is materialized into the profile"
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
                "@ccteam/ccteam-ui"
            ])
        );
    }

    /// Registering into a profile that does not exist yet must scaffold the
    /// vendor's own web bundles first — a manifest listing only
    /// `@ccteam/ccteam-ui` cannot boot (the plugin waits forever for the host
    /// services the vendor bundles provide; real-machine v0.10.3 DoD caught
    /// `dsh web` dying before readiness on a fresh operator home).
    #[test]
    fn registering_into_a_missing_profile_scaffolds_the_vendor_web_bundles() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        let package_json = read_package(&profile_dir);
        assert_eq!(
            package_json["dsh"]["profile"]["bundles"],
            serde_json::json!([
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "@ccteam/ccteam-ui"
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
            !ccteam_plugins_registered_in_profile(dsh_home.path(), "web"),
            "no profile at all -> not registered"
        );

        // Bundle installed by hand (`dsh plugin add`) but no config row.
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "dsh": { "profile": { "bundles": [CCTEAM_UI_BUNDLE] } }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(profile_dir.join("cordis.patch.yml"), "[]\n").unwrap();
        assert!(
            !ccteam_plugins_registered_in_profile(dsh_home.path(), "web"),
            "bundle without a configured row -> not registered"
        );

        // A row whose config is missing the key the plugin cannot work without
        // is not a registration either: files installed, plugin inert.
        fs::write(
            profile_dir.join("cordis.patch.yml"),
            format!("- id: ccteam-ui\n  config:\n    transportSocket: {SOCKET}\n"),
        )
        .unwrap();
        assert!(
            !ccteam_plugins_registered_in_profile(dsh_home.path(), "web"),
            "a row without the required config key -> not registered"
        );

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
        assert!(
            ccteam_plugins_registered_in_profile(dsh_home.path(), "web"),
            "after gate (1) registration -> registered"
        );
        assert!(
            !ccteam_plugins_registered_in_profile(dsh_home.path(), "headless"),
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

        let out = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
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
                "@ccteam/ccteam-ui"
            ])
        );

        // cordis.patch.yml: the user's row is untouched, ours are appended as
        // overrides (no `insert`); enrollment is not injected into the
        // operator's home (their own Settings owns it), while the panel row
        // carries the operator's own REST token and the file goes private.
        let patch = read_patch(&profile_dir);
        let rows = patch.as_sequence().unwrap();
        assert_eq!(
            rows.len(),
            1 + CCTEAM_PLUGINS.len(),
            "the user's row plus one per ccteam plugin"
        );
        assert_eq!(
            rows[0],
            serde_yaml::from_str::<serde_yaml::Value>(user_patch)
                .unwrap()
                .as_sequence()
                .unwrap()[0],
            "the user's own patch row must survive unchanged"
        );
        let ours = ccteam_row(&patch);
        assert!(
            ours.get("insert").is_none(),
            "ccteam's row is an override here too: {ours:?}"
        );
        let config = ours["config"].as_mapping().unwrap();
        assert_eq!(
            config["transportSocket"],
            serde_yaml::Value::String(SOCKET.into())
        );
        assert_eq!(
            config["daemonUrl"],
            serde_yaml::Value::String("http://127.0.0.1:7331".into()),
            "every face is pointed at this daemon"
        );
        assert_eq!(
            config["restToken"],
            serde_yaml::Value::String(REST_TOKEN.into()),
            "the operator's own REST token rides ccteam's row (owner \
             decision 2026-08-28: pasting is for a hand-started dsh web only)"
        );
        assert!(
            config.get("enrollment").is_none(),
            "the operator's enrollment is theirs to set: {config:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(profile_dir.join(PATCH_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "a patch carrying a credential is private"
            );
        }

        for plugin in CCTEAM_PLUGINS.iter() {
            assert!(
                profile_dir
                    .join("node_modules")
                    .join(CCTEAM_SCOPE)
                    .join(plugin.package)
                    .join("lib/index.js")
                    .is_file(),
                "{} is materialized into the operator profile",
                plugin.bundle
            );
        }
    }

    #[test]
    fn operator_registration_is_idempotent_and_reconfigurable() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let register =
            |socket: &str| operator_register(root.path(), dsh_home.path(), socket).unwrap();

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
        assert_eq!(patch.as_sequence().unwrap().len(), CCTEAM_PLUGINS.len());
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

        let err = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap_err();
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
        let link = plugin_link(&first.profile_dir, &UI_PLUGIN);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), cache_of(&second, &UI_PLUGIN));
    }

    #[test]
    fn node_modules_entry_is_symlink_to_cache() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        for plugin in CCTEAM_PLUGINS.iter() {
            let link = plugin_link(&out.profile_dir, plugin);
            let meta = fs::symlink_metadata(&link).unwrap();
            assert!(
                meta.file_type().is_symlink(),
                "{} is linked, not copied",
                plugin.bundle
            );
            assert_eq!(fs::read_link(&link).unwrap(), cache_of(&out, plugin));
            assert!(link.join("package.json").is_file());
        }
        assert!(plugin_link(&out.profile_dir, &UI_PLUGIN)
            .join("lib")
            .join("index.js")
            .is_file());
        assert!(
            plugin_link(&out.profile_dir, &UI_PLUGIN)
                .join("lib")
                .join("client.js")
                .is_file(),
            "the browser half ships in the same package as the host half"
        );
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
                "@ccteam/ccteam-ui"
            ])
        );
        assert_eq!(
            read_patch(&first.profile_dir).as_sequence().unwrap().len(),
            CCTEAM_PLUGINS.len()
        );
        assert_eq!(
            fs::read_link(plugin_link(&first.profile_dir, &UI_PLUGIN)).unwrap(),
            cache_of(&first, &UI_PLUGIN)
        );
    }

    #[test]
    fn empty_cache_directory_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let cache = cache_of(&first, &UI_PLUGIN);
        fs::remove_dir_all(&cache).unwrap();
        fs::create_dir_all(&cache).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert!(second.cache_rebuilt);
        assert!(cache.join("package.json").is_file());
        assert!(cache.join("lib").join("index.js").is_file());
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
        for plugin in CCTEAM_PLUGINS.iter() {
            let package_json: serde_json::Value = serde_json::from_slice(
                &fs::read(cache_of(&out, plugin).join("package.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(package_json["name"], plugin.bundle);
            assert_eq!(
                package_json["dsh"]["bundle"]["patch"],
                serde_json::json!("./cordis.patch.yml"),
                "{} must ship the bundle patch that inserts its loader row",
                plugin.bundle
            );
        }
    }

    #[test]
    fn link_is_replaced_if_it_points_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let wrong_target = tempfile::tempdir().unwrap();

        let first = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        let link = plugin_link(&first.profile_dir, &UI_PLUGIN);
        remove_existing(&link).unwrap();
        ensure_symlink(&link, wrong_target.path()).unwrap();

        let second = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), cache_of(&second, &UI_PLUGIN));
    }

    #[test]
    fn read_embedded_tgz_bytes_without_consuming_build_tools() {
        for plugin in CCTEAM_PLUGINS.iter() {
            let mut decoder = GzDecoder::new(plugin.tgz);
            let mut decoded = Vec::new();
            decoder.read_to_end(&mut decoded).unwrap();
            assert!(!decoded.is_empty(), "{} archive is empty", plugin.bundle);
        }
    }

    /// `npm pack` folds `bundledDependencies` in from `node_modules`, so a
    /// tarball packed without installing first extracts fine and then fails at
    /// runtime on `import Schema from '@deepseek-ai/schemastery'`. Assert the
    /// dependency is actually inside each embedded archive
    /// (`dsh-plugins/pack-assets.sh` refuses to publish one that is not).
    #[test]
    fn embedded_archives_carry_their_bundled_runtime_dependency() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), plain_web()).unwrap();
        for plugin in CCTEAM_PLUGINS.iter() {
            assert!(
                cache_of(&out, plugin)
                    .join("node_modules")
                    .join("@deepseek-ai")
                    .join("schemastery")
                    .join("package.json")
                    .is_file(),
                "{} must bundle its runtime dependency: the profile links this \
                 cache dir straight into node_modules, so nothing else can \
                 resolve it",
                plugin.bundle
            );
        }
    }

    /// Renaming or retiring a ccteam plugin must not strand its old name in a
    /// profile: a bundle, patch row, or scope link ccteam ITSELF wrote that
    /// this table no longer knows is pruned — a stale bundle's own patch layer
    /// would otherwise re-insert a row id the new package also inserts, and
    /// Cordis aborts the boot on the duplicate. What ccteam wrote is what the
    /// ownership gate says it wrote (a link into its own cache, with no pnpm
    /// dependency line); everything else survives byte-for-byte.
    #[test]
    fn stale_ccteam_scoped_entries_are_pruned_from_a_merge_only_profile() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        let scope_dir = profile_dir.join("node_modules").join(CCTEAM_SCOPE);
        fs::create_dir_all(&scope_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "dependencies": {"@user/my-plugin": "1.0.0"},
                "dsh": {"profile": {"bundles": [
                    "@deepseek-ai/dsh-base",
                    "@ccteam/dsh-retired",
                    "@user/my-plugin",
                    "@ccteam/ccteam-retired"
                ]}}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            profile_dir.join(PATCH_FILE),
            "- id: ccteam-retired\n  name: '@ccteam/dsh-retired'\n  config:\n    daemonUrl: http://old\n\
             - id: ccteam-retired\n  name: '@ccteam/ccteam-retired'\n  config:\n    daemonUrl: http://old\n\
             - id: my-plugin\n  name: '@user/my-plugin'\n  config:\n    keepMe: true\n",
        )
        .unwrap();
        // Links ccteam itself wrote: into its own extraction cache, under
        // namespaces this build no longer ships.
        let stale_target = ccteam_plugin_cache_root(root.path()).join("retired");
        fs::create_dir_all(&stale_target).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&stale_target, scope_dir.join("dsh-retired")).unwrap();
            std::os::unix::fs::symlink(&stale_target, scope_dir.join("ccteam-retired")).unwrap();
        }

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let package = read_package(&profile_dir);
        assert_eq!(
            package["dsh"]["profile"]["bundles"],
            serde_json::json!(["@deepseek-ai/dsh-base", "@user/my-plugin", CCTEAM_UI_BUNDLE]),
            "stale ccteam bundles go, the user's bundle stays, ours are appended"
        );
        assert_eq!(
            package["dependencies"]["@user/my-plugin"],
            serde_json::json!("1.0.0"),
            "pnpm's dependency table is not ours to edit"
        );

        let patch = read_patch(&profile_dir);
        let names: Vec<&str> = patch
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|row| row.get("name").and_then(serde_yaml::Value::as_str))
            .collect();
        assert_eq!(
            names,
            vec!["@user/my-plugin", CCTEAM_UI_BUNDLE],
            "rows naming former ccteam packages are pruned, whatever their id: {patch:?}"
        );
        let user_row = row_with_id(&patch, "my-plugin");
        assert_eq!(user_row["config"]["keepMe"], serde_yaml::Value::Bool(true));
        let ours = ccteam_row(&patch);
        assert_eq!(
            ours["config"]["transportSocket"],
            serde_yaml::Value::String(SOCKET.into()),
            "our row is re-issued from the live spec, not the stale row"
        );

        let mut links: Vec<String> = fs::read_dir(&scope_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        links.sort();
        assert_eq!(
            links,
            vec![UI_PLUGIN.package.to_string()],
            "the scope dir holds exactly the table's packages"
        );
    }

    /// The operator ran `dsh plugin add @ccteam/ccteam-ui` themselves. ccteam
    /// must add NOTHING that would make the profile carry that plugin twice —
    /// no second bundle row, no link of its own, not even an extraction — and
    /// still arm the copy that is there with this daemon's config row.
    #[test]
    fn an_operator_installed_plugin_is_configured_never_materialized() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = operator_installed_profile(dsh_home.path(), "0.10.4-alpha.0");

        let out = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            out.operator_installed,
            vec![OperatorInstall {
                bundle: CCTEAM_UI_BUNDLE,
                version: Some("0.10.4-alpha.0".to_string()),
            }],
            "the operator's install is recognized as theirs"
        );
        assert!(
            out.cache_dirs.is_empty() && !out.cache_rebuilt,
            "nothing to extract when the profile already carries the plugin"
        );
        assert!(
            !root.path().join("runtime").join("dsh").join("ui").exists(),
            "the embedded tarball is not unpacked for a plugin ccteam does not install"
        );

        assert_eq!(
            bundle_entries(&profile_dir),
            vec![
                "@deepseek-ai/dsh-base".to_string(),
                "@deepseek-ai/dsh-web-app".to_string(),
                CCTEAM_UI_BUNDLE.to_string(),
            ],
            "exactly one row for the plugin: a second is `duplicate loader entry id`"
        );
        assert_eq!(
            read_package(&profile_dir)["dependencies"][CCTEAM_UI_BUNDLE],
            serde_json::json!("^0.10.4"),
            "pnpm's dependency table is not ours to edit"
        );

        let package_dir = plugin_link(&profile_dir, &UI_PLUGIN);
        assert!(
            !fs::symlink_metadata(&package_dir)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the operator's own package directory is left in place"
        );
        assert!(
            package_dir.join("marker.txt").is_file(),
            "no file of theirs is replaced"
        );

        // The one thing ccteam does write: the row that arms their copy.
        let ours = ccteam_row(&read_patch(&profile_dir));
        assert!(ours.get("insert").is_none(), "an override, never an insert");
        assert_eq!(
            ours["name"],
            serde_yaml::Value::String(CCTEAM_UI_BUNDLE.into()),
            "the row names the plugin it patches"
        );
        let config = ours["config"]
            .as_mapping()
            .expect("flat plugin config")
            .clone();
        assert_eq!(
            config["daemonUrl"],
            serde_yaml::Value::String("http://127.0.0.1:7331".into())
        );
        assert_eq!(
            config["transportSocket"],
            serde_yaml::Value::String(SOCKET.into())
        );
        assert_eq!(
            config["restToken"],
            serde_yaml::Value::String(REST_TOKEN.into()),
            "credentials still reach the operator's own install"
        );
        assert!(
            config.keys().all(|key| key.as_str() != Some("ccteam-ui")),
            "config stays flat — a namespace nests it out of reach: {config:?}"
        );
    }

    /// A version the operator pinned that is not the one ccteam embeds is a
    /// report, not a repair: `doctor` and the Hosts surface print it and the
    /// files stay exactly as pnpm left them.
    #[test]
    fn a_version_mismatch_is_reported_and_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = operator_installed_profile(dsh_home.path(), "0.0.1-theirs");

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let embedded = embedded_plugin_version(&UI_PLUGIN).expect("the tarball carries a version");
        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        assert_eq!(findings.len(), 1, "one finding, no more: {findings:?}");
        assert_eq!(findings[0].bundle, CCTEAM_UI_BUNDLE);
        assert_eq!(
            findings[0].kind,
            DshPluginFindingKind::VersionMismatch {
                installed: "0.0.1-theirs".to_string(),
                embedded: embedded.clone(),
            }
        );
        assert_eq!(
            findings[0].report(),
            format!(
                "{CCTEAM_UI_BUNDLE} plugin_version_mismatch{{installed=0.0.1-theirs, embedded={embedded}}}"
            ),
            "one wording for both surfaces"
        );
        assert!(
            findings[0]
                .remedy
                .contains("`dsh plugin --profile web update @ccteam/ccteam-ui`"),
            "the remedy is theirs to run: {}",
            findings[0].remedy
        );
        assert_eq!(
            installed_package_version(&plugin_link(&profile_dir, &UI_PLUGIN)),
            Some("0.0.1-theirs".to_string()),
            "the operator's version is still the installed one"
        );
    }

    /// The same version is no finding, and neither is a profile ccteam
    /// materialized itself — that one matches the embedded copy by
    /// construction.
    #[test]
    fn matching_and_ccteam_owned_installs_report_no_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let embedded = embedded_plugin_version(&UI_PLUGIN).expect("the tarball carries a version");
        operator_installed_profile(dsh_home.path(), &embedded);
        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
        assert!(
            ccteam_plugin_findings(root.path(), dsh_home.path(), "web").is_empty(),
            "same version, nothing to report"
        );

        let fresh_root = tempfile::tempdir().unwrap();
        let fresh_home = tempfile::tempdir().unwrap();
        operator_register(fresh_root.path(), fresh_home.path(), SOCKET).unwrap();
        assert!(
            ccteam_plugin_findings(fresh_root.path(), fresh_home.path(), "web").is_empty(),
            "ccteam's own materialization is the embedded copy"
        );
        assert!(
            ccteam_plugin_findings(fresh_root.path(), fresh_home.path(), "no-such").is_empty(),
            "an absent profile is not a finding"
        );
    }

    /// ccteam keeps upgrading the copy it installed itself: its own link is
    /// not an "operator install", so a new embedded sha still re-points it.
    #[test]
    fn ccteams_own_link_is_still_refreshed_on_a_merge_only_profile() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let first = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
        let profile_dir = first.profile_dir.clone();
        assert_eq!(first.operator_installed, vec![]);

        // A stand-in for the next ccteam version's cache: the link points into
        // ccteam's runtime dir but at a sha this build no longer ships.
        let old_cache = ccteam_plugin_cache_root(root.path())
            .join(UI_PLUGIN.cache_ns)
            .join("0000000000000000000000000000000000000000000000000000000000000000");
        fs::create_dir_all(&old_cache).unwrap();
        fs::write(old_cache.join("package.json"), b"{\"version\":\"0.0.0\"}").unwrap();
        let link = plugin_link(&profile_dir, &UI_PLUGIN);
        remove_existing(&link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&old_cache, &link).unwrap();

        let again = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();
        assert_eq!(
            again.operator_installed,
            vec![],
            "a link into ccteam's own cache is ccteam's, whatever its sha"
        );
        assert_eq!(
            fs::read_link(&link).unwrap(),
            cache_of(&again, &UI_PLUGIN),
            "it is re-pointed at the cache this build ships"
        );
    }

    /// A tenant home ccteam owns end to end keeps its semantics: there is no
    /// third party to defer to there, so a dependency line someone dropped in
    /// changes nothing.
    #[test]
    fn an_owned_profile_materializes_even_with_a_dependency_line() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join(WEB_PROFILE);
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "dependencies": {CCTEAM_UI_BUNDLE: "^0.10.4"},
                "dsh": {"profile": {"bundles": [CCTEAM_UI_BUNDLE]}}
            })
            .to_string(),
        )
        .unwrap();

        let out = materialize_profile_in(root.path(), dsh_home.path(), wired_web()).unwrap();

        assert_eq!(out.operator_installed, vec![], "Owned defers to nobody");
        assert_eq!(
            fs::read_link(plugin_link(&out.profile_dir, &UI_PLUGIN)).unwrap(),
            cache_of(&out, &UI_PLUGIN),
            "the managed home still links ccteam's own cache"
        );
        assert_eq!(
            bundle_entries(&out.profile_dir)
                .iter()
                .filter(|bundle| bundle.as_str() == CCTEAM_UI_BUNDLE)
                .count(),
            1,
            "and still lists the bundle exactly once"
        );
        assert!(
            ccteam_plugin_findings(root.path(), dsh_home.path(), WEB_PROFILE).is_empty(),
            "a managed home has no operator install to drift from"
        );
    }

    /// A patch with nothing of ours to configure and nothing stale is left
    /// byte-for-byte alone; one stale row is enough to rewrite it.
    #[test]
    fn a_stale_row_alone_is_enough_to_rewrite_the_patch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PATCH_FILE);
        let spec = ProfileSpec {
            name: "web",
            vendor_bundles: &[],
            config: DshPluginConfig::default(),
            manifest: ManifestPolicy::MergeOnly,
        };
        // No profile files at all: nothing declared, no scope dir, so a
        // `@ccteam/*` row can only be one ccteam wrote.
        let ownership = ScopeOwnership::snapshot(dir.path(), dir.path(), ManifestPolicy::MergeOnly);
        fs::write(&path, "- id: keep\n  name: '@user/keep'\n").unwrap();
        assert_eq!(
            merged_profile_patch_yaml(&path, &spec, &ownership).unwrap(),
            None,
            "nothing ours, nothing stale: untouched"
        );
        fs::write(
            &path,
            "- id: keep\n  name: '@user/keep'\n- id: old\n  name: '@ccteam/ccteam-retired'\n",
        )
        .unwrap();
        let rewritten = merged_profile_patch_yaml(&path, &spec, &ownership)
            .unwrap()
            .expect("a stale row forces a rewrite");
        let rows: serde_yaml::Value = serde_yaml::from_str(&rewritten).unwrap();
        let names: Vec<&str> = rows
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|row| row.get("name").and_then(serde_yaml::Value::as_str))
            .collect();
        assert_eq!(names, vec!["@user/keep"]);
    }
    /// A real package directory pnpm put in a profile — never one of ccteam's
    /// symlinks, so the ownership gate reads it as the operator's.
    fn user_package(profile_dir: &Path, package: &str, manifest: serde_json::Value) -> PathBuf {
        let dir = profile_dir
            .join("node_modules")
            .join(CCTEAM_SCOPE)
            .join(package);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), manifest.to_string()).unwrap();
        fs::write(dir.join("marker.txt"), b"pnpm put me here").unwrap();
        dir
    }

    /// Every file of a directory, by name and bytes: "byte-for-byte" is the
    /// whole claim about someone else's install.
    fn tree_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    fn rows_with_id(patch: &serde_yaml::Value, id: &str) -> usize {
        patch
            .as_sequence()
            .expect("patch is a sequence")
            .iter()
            .filter(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some(id))
            .count()
    }

    /// The ownership gate from the PRUNER's side: a `@ccteam/*` package the
    /// operator installed is theirs, whatever this build's plugin table knows
    /// about the name — including an older copy of ccteam's own plugin. Their
    /// bundle rows, package directories, dependency lines and patch rows all
    /// survive a registration byte-for-byte; the drift is reported, not
    /// repaired.
    #[test]
    fn user_owned_ccteam_scoped_packages_survive_a_registration() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "private": true,
                "dependencies": {"@ccteam/whatever": "^2.0.0", CCTEAM_UI_BUNDLE: "^0.9.0"},
                "dsh": {"profile": {"bundles": [
                    "@deepseek-ai/dsh-base",
                    "@ccteam/whatever",
                    CCTEAM_UI_BUNDLE
                ]}}
            })
            .to_string(),
        )
        .unwrap();
        let theirs = user_package(
            &profile_dir,
            "whatever",
            serde_json::json!({"name": "@ccteam/whatever", "version": "2.0.0"}),
        );
        let older_ui = user_package(
            &profile_dir,
            UI_PLUGIN.package,
            serde_json::json!({"name": CCTEAM_UI_BUNDLE, "version": "0.9.0-theirs"}),
        );
        fs::write(
            profile_dir.join(PATCH_FILE),
            "- id: whatever\n  name: '@ccteam/whatever'\n  config:\n    keepMe: true\n",
        )
        .unwrap();
        let before_theirs = tree_bytes(&theirs);
        let before_ui = tree_bytes(&older_ui);

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            bundle_entries(&profile_dir),
            vec![
                "@deepseek-ai/dsh-base".to_string(),
                "@ccteam/whatever".to_string(),
                CCTEAM_UI_BUNDLE.to_string(),
            ],
            "a bundle row of theirs is neither pruned nor duplicated"
        );
        assert_eq!(
            read_package(&profile_dir)["dependencies"],
            serde_json::json!({"@ccteam/whatever": "^2.0.0", CCTEAM_UI_BUNDLE: "^0.9.0"}),
            "pnpm's dependency table is not ours to edit"
        );
        assert_eq!(
            tree_bytes(&theirs),
            before_theirs,
            "their package is theirs"
        );
        assert_eq!(
            tree_bytes(&older_ui),
            before_ui,
            "an older copy of ccteam's own plugin, installed by them, is still theirs"
        );
        let patch = read_patch(&profile_dir);
        assert_eq!(
            row_with_id(&patch, "whatever")["config"]["keepMe"],
            serde_yaml::Value::Bool(true),
            "their patch row survives whatever this build's table knows: {patch:?}"
        );
        assert!(
            !root.path().join("runtime").join("dsh").join("ui").exists(),
            "nothing of ours is extracted next to their install"
        );

        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        assert_eq!(
            findings.len(),
            1,
            "the drift is reported once: {findings:?}"
        );
        assert!(matches!(
            findings[0].kind,
            DshPluginFindingKind::VersionMismatch { .. }
        ));
    }

    /// Registering the same thing twice must cost NOTHING on disk. The daemon
    /// registers on every start and every `register-mcp` call registers again;
    /// a profile whose files are rewritten each time is a profile whose mtime
    /// lies to every tool that watches it — and whose operator sees ccteam
    /// touching their file for no reason.
    #[test]
    fn re_registration_is_a_no_op_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "private": true,
                "dependencies": {"@ccteam/whatever": "^2.0.0"},
                "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base", "@ccteam/whatever"]}}
            })
            .to_string(),
        )
        .unwrap();
        // Their row, in formatting ccteam's serializer does not reproduce: a
        // flow mapping and double quotes.
        fs::write(
            profile_dir.join(PATCH_FILE),
            "- config: { keep: true }\n  name: \"@ccteam/whatever\"\n  id: whatever\n",
        )
        .unwrap();
        let theirs = user_package(
            &profile_dir,
            "whatever",
            serde_json::json!({"name": "@ccteam/whatever", "version": "2.0.0"}),
        );
        let before_theirs = tree_bytes(&theirs);

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        // What survives their row's re-formatting is its MEANING.
        assert_eq!(
            row_with_id(&read_patch(&profile_dir), "whatever")["config"]["keep"],
            serde_yaml::Value::Bool(true),
            "their row still says what they wrote"
        );
        let package_path = profile_dir.join("package.json");
        let patch_path = profile_dir.join(PATCH_FILE);
        let package_bytes = fs::read(&package_path).unwrap();
        let patch_bytes = fs::read(&patch_path).unwrap();
        let package_stamp = fs::metadata(&package_path).unwrap().modified().unwrap();
        let patch_stamp = fs::metadata(&patch_path).unwrap().modified().unwrap();

        // Far enough apart that a rewrite would move the mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            fs::read(&package_path).unwrap(),
            package_bytes,
            "the second registration rewrote package.json"
        );
        assert_eq!(
            fs::metadata(&package_path).unwrap().modified().unwrap(),
            package_stamp,
            "package.json was written again — same bytes, new mtime"
        );
        assert_eq!(
            fs::read(&patch_path).unwrap(),
            patch_bytes,
            "the second registration rewrote the patch file"
        );
        assert_eq!(
            fs::metadata(&patch_path).unwrap().modified().unwrap(),
            patch_stamp,
            "the patch file was written again — same bytes, new mtime"
        );
        assert_eq!(
            tree_bytes(&theirs),
            before_theirs,
            "their package is still theirs"
        );
        assert!(
            !fs::read_dir(&profile_dir)
                .unwrap()
                .flatten()
                .any(|entry| { entry.file_name().to_string_lossy().ends_with(".tmp") }),
            "a swap left its temp file behind"
        );
    }

    /// The operator formatted their profile their own way — a comment above
    /// their row, a manifest they compacted — and it says exactly what ccteam
    /// would write. Re-registration asks that question by MEANING, so their
    /// formatting and their comment stay; a byte comparison would flatten both
    /// on every daemon start, and `serde_yaml` would take the comment with it.
    #[test]
    fn a_profile_the_operator_reformatted_is_not_rewritten() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = operator_installed_profile(dsh_home.path(), "0.10.4-alpha.0");

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let package_path = profile_dir.join("package.json");
        let patch_path = profile_dir.join(PATCH_FILE);
        let compacted = format!(
            "{}\n",
            serde_json::to_string(&read_package(&profile_dir)).unwrap()
        );
        let annotated = format!(
            "# these rows are ccteam's; this file is mine.\n{}\n",
            fs::read_to_string(&patch_path).unwrap()
        );
        assert_ne!(
            compacted.as_bytes(),
            fs::read(&package_path).unwrap(),
            "the fixture must not already be ccteam's own formatting"
        );
        fs::write(&package_path, &compacted).unwrap();
        fs::write(&patch_path, &annotated).unwrap();
        let package_stamp = fs::metadata(&package_path).unwrap().modified().unwrap();
        let patch_stamp = fs::metadata(&patch_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            fs::read_to_string(&package_path).unwrap(),
            compacted,
            "their manifest formatting is not ccteam's to normalize twice"
        );
        assert_eq!(
            fs::read_to_string(&patch_path).unwrap(),
            annotated,
            "their comment survives a registration that had nothing to change"
        );
        assert_eq!(
            fs::metadata(&package_path).unwrap().modified().unwrap(),
            package_stamp
        );
        assert_eq!(
            fs::metadata(&patch_path).unwrap().modified().unwrap(),
            patch_stamp
        );
    }

    /// A bundle id listed twice aborts the whole Cordis boot. Rows ccteam
    /// wrote itself collapse to one — self-healing, since re-materializing
    /// what ccteam owns is always ccteam's call.
    #[test]
    fn a_duplicate_bundle_row_ccteam_owns_collapses_to_one() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "dsh": {"profile": {"bundles": [
                    "@deepseek-ai/dsh-base",
                    CCTEAM_UI_BUNDLE,
                    CCTEAM_UI_BUNDLE
                ]}}
            })
            .to_string(),
        )
        .unwrap();

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            bundle_entries(&profile_dir),
            vec![
                "@deepseek-ai/dsh-base".to_string(),
                CCTEAM_UI_BUNDLE.to_string()
            ],
            "ccteam's own duplicate collapses to a single row"
        );
        assert!(
            ccteam_plugin_findings(root.path(), dsh_home.path(), "web").is_empty(),
            "nothing left to report once ccteam has cleaned up after itself"
        );
    }

    /// The same id twice in a profile the operator installed into: removing a
    /// row they wrote is not ccteam's call, so both stay and the scan reports
    /// the duplicate with the command that fixes it.
    #[test]
    fn a_duplicate_bundle_row_of_theirs_is_reported_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = operator_installed_profile(dsh_home.path(), "0.10.4-alpha.0");
        let mut manifest = read_package(&profile_dir);
        manifest["dsh"]["profile"]["bundles"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(CCTEAM_UI_BUNDLE));
        fs::write(profile_dir.join("package.json"), manifest.to_string()).unwrap();

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            bundle_entries(&profile_dir)
                .iter()
                .filter(|bundle| bundle.as_str() == CCTEAM_UI_BUNDLE)
                .count(),
            2,
            "both rows are theirs and both stay"
        );
        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        let duplicate = findings
            .iter()
            .find(|finding| finding.code() == "duplicate_bundle_id")
            .unwrap_or_else(|| panic!("the duplicate is reported: {findings:?}"));
        assert_eq!(
            duplicate.kind,
            DshPluginFindingKind::DuplicateBundleId { count: 2 }
        );
        assert_eq!(
            duplicate.report(),
            format!("{CCTEAM_UI_BUNDLE} duplicate_bundle_id{{id={CCTEAM_UI_BUNDLE}, count=2}}")
        );
        assert!(
            duplicate
                .remedy
                .contains("`dsh plugin --profile web remove @ccteam/ccteam-ui`"),
            "the remedy is theirs to run: {}",
            duplicate.remedy
        );
    }

    /// One override row per loader id: duplicates ccteam wrote itself collapse
    /// onto the first, and that one carries this call's config.
    #[test]
    fn duplicate_patch_rows_ccteam_wrote_collapse_to_one() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join(PATCH_FILE),
            "- id: ccteam-ui\n  name: '@ccteam/ccteam-ui'\n  config:\n    daemonUrl: http://old\n\
             - id: ccteam-ui\n  name: '@ccteam/ccteam-ui'\n  config:\n    daemonUrl: http://old\n",
        )
        .unwrap();

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let patch = read_patch(&profile_dir);
        assert_eq!(
            rows_with_id(&patch, CCTEAM_UI_ROW_ID),
            1,
            "one row per loader id: {patch:?}"
        );
        assert_eq!(
            ccteam_row(&patch)["config"]["transportSocket"],
            serde_yaml::Value::String(SOCKET.into()),
            "the surviving row is the one this call armed"
        );
        assert!(ccteam_plugin_findings(root.path(), dsh_home.path(), "web").is_empty());
    }

    /// A second override row for the same loader id that ccteam did NOT write:
    /// arming one and leaving the other is the silent half of the bug, so
    /// ccteam updates neither and reports instead.
    #[test]
    fn a_patch_row_ccteam_did_not_write_is_reported_never_silently_updated() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join(PATCH_FILE),
            "- id: ccteam-ui\n  name: '@ccteam/ccteam-ui'\n  config:\n    daemonUrl: http://old\n\
             - id: ccteam-ui\n  name: '@ccteam/ccteam-ui'\n  disabled: true\n  config:\n    daemonUrl: http://theirs\n",
        )
        .unwrap();

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let patch = read_patch(&profile_dir);
        assert_eq!(
            rows_with_id(&patch, CCTEAM_UI_ROW_ID),
            2,
            "a row of theirs is never removed: {patch:?}"
        );
        assert!(
            patch.as_sequence().unwrap().iter().all(|row| row
                .get("config")
                .and_then(|c| c.get("transportSocket"))
                .is_none()),
            "ccteam armed neither row rather than pick one: {patch:?}"
        );
        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        let duplicate = findings
            .iter()
            .find(|finding| finding.code() == "duplicate_patch_row")
            .unwrap_or_else(|| panic!("the ambiguity is reported: {findings:?}"));
        assert_eq!(
            duplicate.kind,
            DshPluginFindingKind::DuplicatePatchRow {
                row_id: CCTEAM_UI_ROW_ID,
                count: 2,
            }
        );
        assert_eq!(
            duplicate.report(),
            format!("{CCTEAM_UI_BUNDLE} duplicate_patch_row{{id={CCTEAM_UI_ROW_ID}, count=2}}")
        );
    }

    /// A dependency line with nothing installed is the operator's own
    /// half-finished install. ccteam neither takes it over nor drops a second
    /// copy next to it: it ensures the config row and reports the gap.
    #[test]
    fn a_declared_plugin_with_no_package_directory_is_reported_never_taken_over() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "dependencies": {CCTEAM_UI_BUNDLE: "^0.10.4"},
                "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base", CCTEAM_UI_BUNDLE]}}
            })
            .to_string(),
        )
        .unwrap();

        let out = operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        assert_eq!(
            out.operator_installed,
            vec![OperatorInstall {
                bundle: CCTEAM_UI_BUNDLE,
                version: None,
            }],
            "a declaration with nothing behind it is still their install"
        );
        assert!(
            out.cache_dirs.is_empty() && !out.cache_rebuilt,
            "nothing is extracted for an install ccteam does not make"
        );
        assert!(
            fs::symlink_metadata(plugin_link(&profile_dir, &UI_PLUGIN)).is_err(),
            "no link of ours lands where pnpm will put theirs"
        );
        assert_eq!(
            bundle_entries(&profile_dir),
            vec![
                "@deepseek-ai/dsh-base".to_string(),
                CCTEAM_UI_BUNDLE.to_string()
            ],
            "their row stays, and ccteam adds none"
        );
        assert_eq!(
            ccteam_row(&read_patch(&profile_dir))["config"]["daemonUrl"],
            serde_yaml::Value::String("http://127.0.0.1:7331".into()),
            "the config row is the one thing ccteam does write"
        );

        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, DshPluginFindingKind::MissingOnDisk);
        assert_eq!(
            findings[0].report(),
            format!("{CCTEAM_UI_BUNDLE} plugin_missing_on_disk{{id={CCTEAM_UI_BUNDLE}}}")
        );
        assert!(
            findings[0]
                .remedy
                .contains("`dsh plugin --profile web add @ccteam/ccteam-ui`"),
            "the remedy finishes THEIR install: {}",
            findings[0].remedy
        );
    }

    /// Their package with no readable `version`: the mismatch check cannot
    /// run, and staying silent would read as "aligned".
    #[test]
    fn a_user_install_without_a_version_is_reported_as_unknown() {
        let root = tempfile::tempdir().unwrap();
        let dsh_home = tempfile::tempdir().unwrap();
        let profile_dir = dsh_home.path().join("profiles").join("web");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("package.json"),
            serde_json::json!({
                "name": "dsh-web-profile",
                "dependencies": {CCTEAM_UI_BUNDLE: "^0.10.4"},
                "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base", CCTEAM_UI_BUNDLE]}}
            })
            .to_string(),
        )
        .unwrap();
        user_package(
            &profile_dir,
            UI_PLUGIN.package,
            serde_json::json!({"name": CCTEAM_UI_BUNDLE}),
        );

        operator_register(root.path(), dsh_home.path(), SOCKET).unwrap();

        let findings = ccteam_plugin_findings(root.path(), dsh_home.path(), "web");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, DshPluginFindingKind::VersionUnknown);
        assert_eq!(
            findings[0].report(),
            format!("{CCTEAM_UI_BUNDLE} plugin_version_unknown{{id={CCTEAM_UI_BUNDLE}}}"),
            "the operator can see WHY the version check did not run"
        );
    }
}
