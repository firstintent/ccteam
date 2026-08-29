//! Enrollment credentials: **which identity** a hand-started vendor session
//! speaks for — and deliberately nothing about what it may do.
//!
//! A vendor's global MCP config is a static file shared by every process that
//! vendor ever starts. Anything durable written into it is therefore a SHARED
//! identity, which is why the admin web token must not live there: two `codex`
//! processes started an hour apart in different repos would authenticate as the
//! same caller, so neither can be a delegation parent and neither has a project
//! of its own. The static file gets an enrollment credential instead — a
//! pointer to *whose* it is — and the per-process identity is issued by the
//! daemon at `initialize` time (see the `Mcp-Session-Id` binding in
//! `ccteam-web`'s `/mcp` route).
//!
//! Wire form: `ccteam-enroll:<id>:<secret>`. The `id` is ops-visible (it names
//! the record in `~/.ccteam/secrets/enroll/<id>.json` and appears in logs); the
//! `secret` is the part that is verified, constant-time.
//!
//! Scope decides what the resulting session may address:
//!
//! - [`EnrollScope::User`] — "this machine's user". Written into the five
//!   vendor global configs at daemon start so a hand-started agent works with
//!   no setup. It names no project, so a caller holding it must name one.
//! - [`EnrollScope::Project`] — issued by the web console's copy button for one
//!   project. An external agent pastes it and is bound to that workspace, which
//!   is what makes the copy-paste flow safe to hand out.
//!
//! HONEST SCOPE: the secret is stored in plaintext under mode 0600, exactly
//! like the web token, because the trust model here is a single OS uid — any
//! process running as this user can already read the vendor config the token
//! was pasted into. What this buys is not isolation but the removal of a
//! machine-wide *shared* identity: revoking one credential does not disturb the
//! others, and a leaked project-scoped one reaches one workspace. Real
//! isolation needs a per-user OS account or sandbox. Primitives leaf: no team
//! names, no LLM, no policy decisions — those live in the caller.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::paths::CcteamPaths;

/// Bearer prefix that marks an enrollment credential on the wire.
pub const ENROLL_BEARER_PREFIX: &str = "ccteam-enroll:";

/// What an enrollment credential is allowed to address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrollScope {
    /// Every project its owner can see; the caller must name one per call.
    User,
    /// Exactly one project, named here. The caller cannot address another.
    Project { slug: String },
}

impl EnrollScope {
    /// The project this credential pins, if it pins one.
    pub fn project(&self) -> Option<&str> {
        match self {
            EnrollScope::User => None,
            EnrollScope::Project { slug } => Some(slug.as_str()),
        }
    }
}

/// One enrollment credential as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollCredential {
    /// Ops-visible record id (16 hex chars). Safe to log.
    pub id: String,
    /// The verified half. NEVER log this.
    pub secret: String,
    pub scope: EnrollScope,
    /// ccteam identity this credential speaks for (`user:web-api`,
    /// `user:<tenant>`) — the owner every session it creates inherits.
    pub owner: String,
    /// Free-text label for the console listing ("rob's laptop", "ci runner").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl EnrollCredential {
    /// The `Authorization: Bearer <…>` value a vendor config carries.
    pub fn bearer(&self) -> String {
        format!("{ENROLL_BEARER_PREFIX}{}:{}", self.id, self.secret)
    }
}

/// Parse `ccteam-enroll:<id>:<secret>` → `(id, secret)`. `None` for any other
/// bearer family, so the caller can fall through to its next credential type.
pub fn parse_enroll_bearer(token: &str) -> Option<(String, String)> {
    let rest = token.strip_prefix(ENROLL_BEARER_PREFIX)?;
    let (id, secret) = rest.split_once(':')?;
    if id.is_empty() || secret.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((id.to_string(), secret.to_string()))
}

/// `<root>/secrets/enroll` — one JSON file per credential.
pub fn enroll_dir_in(root: &Path) -> PathBuf {
    root.join("secrets").join("enroll")
}

fn enroll_path_in(root: &Path, id: &str) -> PathBuf {
    enroll_dir_in(root).join(format!("{id}.json"))
}

fn write_private(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
    }
    Ok(())
}

/// Mint + persist a fresh credential. The returned value is the only time the
/// secret is handed back in full; callers that need it later re-read the file.
pub fn mint_in(
    root: &Path,
    scope: EnrollScope,
    owner: &str,
    label: Option<String>,
) -> Result<EnrollCredential> {
    // Two independent draws: the id is a public handle, the secret is not, so
    // one must never be derivable from the other.
    let cred = EnrollCredential {
        id: crate::session_secret::mint().chars().take(16).collect(),
        secret: crate::session_secret::mint(),
        scope,
        owner: owner.to_string(),
        label,
        created_at: Utc::now(),
    };
    let body = serde_json::to_string_pretty(&cred)?;
    write_private(&enroll_path_in(root, &cred.id), &body)?;
    Ok(cred)
}

/// Read one credential by id. `None` when absent or unparseable — a corrupt
/// record must fail closed, never authenticate.
pub fn load_in(root: &Path, id: &str) -> Option<EnrollCredential> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let body = std::fs::read_to_string(enroll_path_in(root, id)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Every credential on this machine, newest first. For the console listing and
/// `doctor`; the secret rides along, so callers rendering this MUST redact it.
pub fn list_in(root: &Path) -> Vec<EnrollCredential> {
    let mut out: Vec<EnrollCredential> = match std::fs::read_dir(enroll_dir_in(root)) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|body| serde_json::from_str::<EnrollCredential>(&body).ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by_key(|c| std::cmp::Reverse(c.created_at));
    out
}

/// Resolve a presented bearer to its record. Constant-time secret compare; an
/// unknown id and a wrong secret are indistinguishable to the caller.
pub fn verify_in(root: &Path, presented: &str) -> Option<EnrollCredential> {
    let (id, secret) = parse_enroll_bearer(presented)?;
    let cred = load_in(root, &id)?;
    if cred.secret.is_empty() || !crate::session_secret::ct_eq(&cred.secret, &secret) {
        return None;
    }
    Some(cred)
}

/// Delete a credential. Every process holding it fails closed on its next
/// request; nothing else is disturbed.
pub fn revoke_in(root: &Path, id: &str) -> Result<bool> {
    let path = enroll_path_in(root, id);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

/// Label the machine-user credential carries. It is part of that record's
/// IDENTITY, not decoration: see [`ensure_in`].
pub const MACHINE_LABEL: &str = "this machine";

/// What [`ensure_in`] resolved to.
pub struct Ensured {
    pub credential: EnrollCredential,
    /// `true` only when this call minted the record. The secret is knowable
    /// exactly then, so this is what tells a caller whether it may hand a
    /// bearer back; a reused record can only be identified by [`EnrollCredential::id`].
    pub created: bool,
}

/// Get-or-mint, keyed by **(owner, scope, label)** — the idempotent face of
/// [`mint_in`], shared by every caller that wants *the* credential for a
/// purpose rather than *another* one.
///
/// The key includes the LABEL because a label is how a caller names its own
/// slot ("this machine", "dsh-plugin:web"), and dropping it made the lookup
/// return whichever user-scoped record happened to be newest: one console mint
/// and the next daemon start would rewrite five vendor configs with a
/// different bearer. Owner is in the key because a credential speaks for one
/// identity, and scope because a project-pinned record must never satisfy a
/// request for an unpinned one (it reaches less than the caller asked for).
///
/// `rotate` is the escape hatch for a caller that LOST its secret: the record
/// is unreadable by then (only the id is public), so the only way back is a
/// fresh mint. The replacement is minted BEFORE the old one is revoked, so a
/// failure mid-way leaves the caller with a working credential rather than
/// none.
pub fn ensure_in(
    root: &Path,
    scope: EnrollScope,
    owner: &str,
    label: Option<&str>,
    rotate: bool,
) -> Result<Ensured> {
    let existing = list_in(root)
        .into_iter()
        .find(|c| c.owner == owner && c.scope == scope && c.label.as_deref() == label);
    match existing {
        Some(cred) if !rotate => Ok(Ensured {
            credential: cred,
            created: false,
        }),
        existing => {
            let cred = mint_in(root, scope, owner, label.map(str::to_string))?;
            if let Some(old) = existing {
                revoke_in(root, &old.id)?;
            }
            Ok(Ensured {
                credential: cred,
                created: true,
            })
        }
    }
}

/// The machine-user credential, minted once and reused — this is what daemon
/// start writes into the vendor global configs, so it must be stable across
/// restarts or every restart would rewrite five config files with a new value.
/// One slot, named by [`MACHINE_LABEL`]; see [`ensure_in`] for why the name is
/// part of the key.
pub fn ensure_user_credential_in(root: &Path, owner: &str) -> Result<EnrollCredential> {
    Ok(ensure_in(root, EnrollScope::User, owner, Some(MACHINE_LABEL), false)?.credential)
}

/// Convenience wrappers for callers that already hold [`CcteamPaths`].
pub fn ensure_user_credential(paths: &CcteamPaths, owner: &str) -> Result<EnrollCredential> {
    ensure_user_credential_in(&paths.root, owner)
}

/// See [`verify_in`].
pub fn verify(paths: &CcteamPaths, presented: &str) -> Option<EnrollCredential> {
    verify_in(&paths.root, presented)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("home");
        (tmp, root)
    }

    #[test]
    fn a_minted_credential_verifies_by_its_own_bearer() {
        let (_tmp, root) = root();
        let cred = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        let back = verify_in(&root, &cred.bearer()).expect("own bearer verifies");
        assert_eq!(back.id, cred.id);
        assert_eq!(back.owner, "user:web-api");
        assert_eq!(back.scope, EnrollScope::User);
    }

    #[test]
    fn a_wrong_secret_or_unknown_id_never_verifies() {
        let (_tmp, root) = root();
        let cred = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        let forged = format!(
            "{ENROLL_BEARER_PREFIX}{}:{}",
            cred.id,
            crate::session_secret::mint()
        );
        assert!(verify_in(&root, &forged).is_none(), "wrong secret");
        let unknown = format!("{ENROLL_BEARER_PREFIX}deadbeefdeadbeef:{}", cred.secret);
        assert!(verify_in(&root, &unknown).is_none(), "unknown id");
        // Other bearer families must fall through, not error.
        assert!(verify_in(&root, "ccteam:abc123").is_none());
        assert!(verify_in(&root, "ccteam-sid:s1:sek").is_none());
    }

    /// The wire form is the contract five vendor configs are written against.
    #[test]
    fn bearer_round_trips_through_the_parser() {
        let (_tmp, root) = root();
        let cred = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        let (id, secret) = parse_enroll_bearer(&cred.bearer()).expect("parses");
        assert_eq!(id, cred.id);
        assert_eq!(secret, cred.secret);
        assert!(parse_enroll_bearer("ccteam-enroll:").is_none());
        assert!(parse_enroll_bearer("ccteam-enroll:onlyid").is_none());
        assert!(parse_enroll_bearer("ccteam-enroll:not-hex:sek").is_none());
        assert!(parse_enroll_bearer("ccteam-enroll::sek").is_none());
    }

    /// A project-scoped credential is the copy-button's product: it pins the
    /// workspace so the pasting agent cannot address another one.
    #[test]
    fn project_scope_pins_one_workspace() {
        let (_tmp, root) = root();
        let cred = mint_in(
            &root,
            EnrollScope::Project {
                slug: "alpha".to_string(),
            },
            "user:web-api",
            Some("external reviewer".to_string()),
        )
        .unwrap();
        assert_eq!(cred.scope.project(), Some("alpha"));
        let back = verify_in(&root, &cred.bearer()).unwrap();
        assert_eq!(back.scope.project(), Some("alpha"));
        assert_eq!(back.label.as_deref(), Some("external reviewer"));
        assert_eq!(EnrollScope::User.project(), None);
    }

    /// Daemon start writes the user credential into five vendor configs, so it
    /// has to be the SAME value every restart.
    #[test]
    fn ensure_user_credential_is_idempotent_per_owner() {
        let (_tmp, root) = root();
        let first = ensure_user_credential_in(&root, "user:web-api").unwrap();
        let second = ensure_user_credential_in(&root, "user:web-api").unwrap();
        assert_eq!(first.id, second.id, "must not mint a second machine token");
        assert_eq!(first.secret, second.secret);

        // A different owner is a different credential, and a project-scoped
        // record never satisfies the user-scoped lookup.
        let other = ensure_user_credential_in(&root, "user:u42").unwrap();
        assert_ne!(other.id, first.id);
        assert_eq!(list_in(&root).len(), 2);
    }

    /// The get-or-mint face: same (owner, scope, label) → the same record, and
    /// no second file on disk. This is what a client that boots repeatedly
    /// (a plugin, a CI runner) needs, and it is the same function the machine
    /// credential goes through.
    #[test]
    fn ensure_is_idempotent_per_owner_scope_and_label() {
        let (_tmp, root) = root();
        let first = ensure_in(
            &root,
            EnrollScope::User,
            "user:web-api",
            Some("dsh-plugin:web"),
            false,
        )
        .unwrap();
        assert!(first.created, "nothing existed yet");
        let again = ensure_in(
            &root,
            EnrollScope::User,
            "user:web-api",
            Some("dsh-plugin:web"),
            false,
        )
        .unwrap();
        assert!(!again.created, "must not mint a second credential");
        assert_eq!(first.credential.id, again.credential.id);
        assert_eq!(list_in(&root).len(), 1);

        // Every part of the key separates: another identity, another scope,
        // another label are each a different slot.
        for other in [
            ensure_in(
                &root,
                EnrollScope::User,
                "user:u42",
                Some("dsh-plugin:web"),
                false,
            )
            .unwrap(),
            ensure_in(
                &root,
                EnrollScope::Project {
                    slug: "alpha".to_string(),
                },
                "user:web-api",
                Some("dsh-plugin:web"),
                false,
            )
            .unwrap(),
            ensure_in(&root, EnrollScope::User, "user:web-api", Some("ci"), false).unwrap(),
        ] {
            assert!(other.created);
            assert_ne!(other.credential.id, first.credential.id);
        }
        assert_eq!(list_in(&root).len(), 4);
        assert!(verify_in(&root, &first.credential.bearer()).is_some());
    }

    /// A caller that lost its secret cannot read it back (only the id is
    /// public), so `rotate` is the only way home: a fresh record under the same
    /// key, and the old bearer dead so a stale holder fails closed.
    #[test]
    fn ensure_rotate_replaces_the_record_and_revokes_the_old() {
        let (_tmp, root) = root();
        let first = ensure_in(&root, EnrollScope::User, "user:web-api", Some("ci"), false)
            .unwrap()
            .credential;
        let rotated =
            ensure_in(&root, EnrollScope::User, "user:web-api", Some("ci"), true).unwrap();
        assert!(rotated.created, "rotate always mints");
        assert_ne!(rotated.credential.id, first.id);
        assert!(
            verify_in(&root, &first.bearer()).is_none(),
            "the rotated-away bearer must stop verifying"
        );
        assert!(verify_in(&root, &rotated.credential.bearer()).is_some());
        assert_eq!(
            list_in(&root).len(),
            1,
            "rotate replaces, it does not pile up"
        );

        // And the rotated record is what the next plain ensure resolves to.
        let after = ensure_in(&root, EnrollScope::User, "user:web-api", Some("ci"), false).unwrap();
        assert!(!after.created);
        assert_eq!(after.credential.id, rotated.credential.id);
    }

    /// The machine credential is one SLOT, not "the newest user-scoped record":
    /// a console mint (or a plugin ensure) must not become what daemon start
    /// writes into five vendor configs.
    #[test]
    fn the_machine_credential_is_not_shadowed_by_a_newer_labelled_one() {
        let (_tmp, root) = root();
        let machine = ensure_user_credential_in(&root, "user:web-api").unwrap();
        assert_eq!(machine.label.as_deref(), Some(MACHINE_LABEL));
        let newer = mint_in(
            &root,
            EnrollScope::User,
            "user:web-api",
            Some("rob's laptop".to_string()),
        )
        .unwrap();
        assert_eq!(
            list_in(&root).first().map(|c| c.id.clone()),
            Some(newer.id.clone()),
            "the console mint really is the newest record"
        );
        assert_eq!(
            ensure_user_credential_in(&root, "user:web-api").unwrap().id,
            machine.id,
            "daemon start must keep writing the SAME bearer"
        );
    }

    #[test]
    fn revoke_kills_one_credential_and_leaves_the_rest() {
        let (_tmp, root) = root();
        let a = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        let b = mint_in(
            &root,
            EnrollScope::Project {
                slug: "alpha".to_string(),
            },
            "user:web-api",
            None,
        )
        .unwrap();
        assert!(revoke_in(&root, &a.id).unwrap());
        assert!(
            !revoke_in(&root, &a.id).unwrap(),
            "second revoke is a no-op"
        );
        assert!(verify_in(&root, &a.bearer()).is_none());
        assert!(
            verify_in(&root, &b.bearer()).is_some(),
            "revoking one must not disturb another"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stored_credentials_are_private_files_in_a_private_dir() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, root) = root();
        let cred = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        let file = enroll_dir_in(&root).join(format!("{}.json", cred.id));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file must be 0600, got {mode:o}");
        let dir_mode = std::fs::metadata(enroll_dir_in(&root))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "enroll dir must be 0700, got {dir_mode:o}");
    }

    #[test]
    fn a_corrupt_record_fails_closed() {
        let (_tmp, root) = root();
        let cred = mint_in(&root, EnrollScope::User, "user:web-api", None).unwrap();
        write_private(&enroll_path_in(&root, &cred.id), "{ not json at all").unwrap();
        assert!(load_in(&root, &cred.id).is_none());
        assert!(verify_in(&root, &cred.bearer()).is_none());
        assert!(list_in(&root).is_empty(), "unparseable records are skipped");
    }
}
