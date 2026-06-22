//! v0.8.18 档1 — tenant registry for per-user web identities.
//!
//! 档0 had one shared web operator (`web-api`) behind a single web token.
//! 档1 (web-first, owner-directed) splits that into **per-user tenants**: each
//! tenant has its own web token (the `?token=ccteam:<hex>` value) and a stable
//! identity id used as the owner of the sessions it creates, so the web REST
//! surface can filter each user down to their own. A tenant may additionally
//! be **linked to an IM chat** (`"channel:chat_id"`) so the same person's web
//! and Telegram/Lark share ONE identity.
//!
//! Stored at `~/.ccteam/tenants.json` (admin-managed via `POST /api/v1/users`).
//! Created on the WEB (the CLI deliberately has no `ccteam user` command — the
//! runtime write surface lives on web/IM/REST; the CLI stays bootstrap-only).
//!
//! HONEST SCOPE: same as the rest of multi-user 档0/档1 — under one OS uid this
//! is soft (UX) isolation, NOT a security boundary; a per-user token only
//! scopes the UX, it doesn't sandbox the agents (same-uid can read files /
//! `/proc/<pid>/environ`). Real isolation = per-user OS user / sandbox (later).

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session_secret;

/// One web tenant (a per-user identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tenant {
    /// Stable identity id (`u<8hex>`). Used as the `chat_id` of the synthetic
    /// `web` [`owner`] ChatKey for sessions this tenant creates, and the key
    /// per-user filtering matches on. Never changes.
    pub id: String,
    /// Human display handle (e.g. `alice`). Display-only; not an identity key.
    pub handle: String,
    /// Per-user web token (lowercase hex, no `ccteam:` prefix) — the value in
    /// the tenant's personal link `?token=ccteam:<web_token>`.
    pub web_token: String,
    /// Optional IM chat (`"channel:chat_id"`) linked to this identity, so the
    /// same person's web + IM resolve to ONE identity. `None` until linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_chat: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The tenant registry, persisted as `~/.ccteam/tenants.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TenantRegistry {
    #[serde(default)]
    pub tenants: Vec<Tenant>,
}

impl TenantRegistry {
    /// Load from `path`. A missing / unreadable / unparseable file is an empty
    /// registry (best-effort — never an error, so the web layer always starts).
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomically persist to `path` (serialize → write `<path>.tmp` → rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self).context("serialize TenantRegistry")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut tmp_os = path.as_os_str().to_owned();
        tmp_os.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp_os);
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Mint a new tenant: a fresh id (`u<8hex>`, collision-checked) + a per-user
    /// web token. The handle is trimmed; an empty handle falls back to the id.
    pub fn add(&mut self, handle: &str) -> Tenant {
        let id = self.fresh_id();
        let handle = {
            let h = handle.trim();
            if h.is_empty() {
                id.clone()
            } else {
                h.to_string()
            }
        };
        let tenant = Tenant {
            id,
            handle,
            web_token: session_secret::mint(),
            linked_chat: None,
            created_at: Utc::now(),
        };
        self.tenants.push(tenant.clone());
        tenant
    }

    /// Resolve a tenant by its web token (constant-time compare).
    pub fn by_token(&self, token: &str) -> Option<&Tenant> {
        self.tenants
            .iter()
            .find(|t| session_secret::ct_eq(&t.web_token, token))
    }

    /// Resolve a tenant by a linked IM chat (`"channel:chat_id"`).
    pub fn by_chat(&self, chat: &str) -> Option<&Tenant> {
        self.tenants
            .iter()
            .find(|t| t.linked_chat.as_deref() == Some(chat))
    }

    /// Resolve a tenant by its identity id.
    pub fn by_id(&self, id: &str) -> Option<&Tenant> {
        self.tenants.iter().find(|t| t.id == id)
    }

    /// Remove a tenant by id. Returns whether one was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.tenants.len();
        self.tenants.retain(|t| t.id != id);
        self.tenants.len() != before
    }

    /// Link an IM chat (`"channel:chat_id"`) to a tenant. Clears the link from
    /// any OTHER tenant first (a chat maps to one identity). Returns whether the
    /// target tenant exists.
    pub fn link_chat(&mut self, id: &str, chat: &str) -> bool {
        if !self.tenants.iter().any(|t| t.id == id) {
            return false;
        }
        for t in &mut self.tenants {
            if t.linked_chat.as_deref() == Some(chat) {
                t.linked_chat = None;
            }
            if t.id == id {
                t.linked_chat = Some(chat.to_string());
            }
        }
        true
    }

    pub fn list(&self) -> &[Tenant] {
        &self.tenants
    }

    /// A fresh `u<8hex>` id not already in use.
    fn fresh_id(&self) -> String {
        loop {
            let id = format!("u{}", &session_secret::mint()[..8]);
            if !self.tenants.iter().any(|t| t.id == id) {
                return id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_mints_unique_id_and_token() {
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        let b = reg.add("bob");
        assert_eq!(a.handle, "alice");
        assert_ne!(a.id, b.id, "ids are unique");
        assert_ne!(a.web_token, b.web_token, "tokens are unique");
        assert!(a.id.starts_with('u') && a.id.len() == 9);
        assert_eq!(a.web_token.len(), 32);
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn add_empty_handle_falls_back_to_id() {
        let mut reg = TenantRegistry::default();
        let t = reg.add("   ");
        assert_eq!(t.handle, t.id);
    }

    #[test]
    fn by_token_and_by_id_resolve() {
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        assert_eq!(reg.by_token(&a.web_token).map(|t| &t.id), Some(&a.id));
        assert_eq!(reg.by_id(&a.id).map(|t| &t.handle), Some(&a.handle));
        assert!(reg.by_token("nope").is_none());
        assert!(reg.by_id("uffffffff").is_none());
    }

    #[test]
    fn link_chat_is_exclusive_and_resolvable() {
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        let b = reg.add("bob");
        assert!(reg.link_chat(&a.id, "telegram:111"));
        assert_eq!(reg.by_chat("telegram:111").map(|t| &t.id), Some(&a.id));
        // Re-linking the same chat to bob moves it off alice (one chat → one id).
        assert!(reg.link_chat(&b.id, "telegram:111"));
        assert_eq!(reg.by_chat("telegram:111").map(|t| &t.id), Some(&b.id));
        assert!(reg.by_id(&a.id).unwrap().linked_chat.is_none());
        // Unknown tenant → false.
        assert!(!reg.link_chat("ughost", "telegram:222"));
    }

    #[test]
    fn remove_drops_the_tenant() {
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        assert!(reg.remove(&a.id));
        assert!(!reg.remove(&a.id), "second remove is a no-op");
        assert!(reg.list().is_empty());
    }

    #[test]
    fn save_load_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tenants.json");
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        reg.link_chat(&a.id, "telegram:111");
        reg.save(&path).unwrap();

        let back = TenantRegistry::load(&path);
        assert_eq!(back.tenants, reg.tenants);
        // Missing file → empty registry, not an error.
        assert!(TenantRegistry::load(&tmp.path().join("absent.json"))
            .tenants
            .is_empty());
    }
}
