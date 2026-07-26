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
//! Stored as per-user files `~/.ccteam/secrets/users/<id>.json` (v0.8.20 — one
//! 0600 file per tenant; admin-managed via `POST /api/v1/users`).
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

/// v0.8.20 F2 — a tenant's OWN Telegram bot (the per-user IM bot). Stored
/// plaintext in the tenant's `secrets/users/<id>.json` (0600, owner decision ②); the daemon runs
/// one `getUpdates` listener per tenant bot and routes its inbound to this
/// tenant's identity. Mirrors the global `ccteam_im::TelegramCreds` shape but
/// lives in core (where `Tenant` lives) — im maps it to its channel at spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantTelegram {
    /// The @BotFather bot token (distinct from every other bot — two bots
    /// sharing a token 409-conflict on `getUpdates`).
    pub bot_token: String,
    /// Chat IDs this tenant's bot accepts. Empty = accept any chat it's in.
    #[serde(default)]
    pub allowed_chat_ids: Vec<String>,
}

/// v0.8.20 F2 — a tenant's OWN Lark/Feishu app (the per-user IM bot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantLark {
    pub app_id: String,
    pub app_secret: String,
    /// `open_id`s allowed to drive the bot. Empty = closed (matches the
    /// channel-level semantics in `ccteam_im::LarkCreds`).
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    /// `true` → Feishu (CN); `false` → Lark intl. Defaults true (CN-first).
    #[serde(default = "default_true")]
    pub use_feishu: bool,
}

fn default_true() -> bool {
    true
}

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
    /// v0.8.20 F2 — this tenant's OWN Telegram bot (per-user IM). `None` until
    /// the tenant configures it (self-serve `PUT /api/v1/me/im`, or admin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TenantTelegram>,
    /// v0.8.20 F2 — this tenant's OWN Lark/Feishu app (per-user IM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lark: Option<TenantLark>,
    pub created_at: DateTime<Utc>,
}

/// The tenant registry — persisted as per-user `~/.ccteam/secrets/users/<id>.json` files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TenantRegistry {
    #[serde(default)]
    pub tenants: Vec<Tenant>,
}

impl TenantRegistry {
    /// v0.8.20 — load every tenant from `dir/<id>.json` (the per-user files under
    /// `~/.ccteam/secrets/users/`). A missing dir / unreadable / unparseable file
    /// is skipped (best-effort — never an error, so the web layer always starts);
    /// tenants are ordered by `created_at` for a stable list.
    pub fn load(dir: &Path) -> Self {
        let mut tenants = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(t) = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Tenant>(&b).ok())
                {
                    tenants.push(t);
                }
            }
        }
        tenants.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Self { tenants }
    }

    /// v0.8.20 — persist to `dir/<id>.json` (one 0600 file per tenant; the dir is
    /// 0700). Writes every current tenant atomically (tmp + rename) and DELETES
    /// any stale `<id>.json` whose tenant was removed, so `load → remove → save`
    /// drops the file. Each file holds the tenant's web token + IM bot creds.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(dir)?.permissions();
            perm.set_mode(0o700);
            let _ = std::fs::set_permissions(dir, perm);
        }
        // Delete files for tenants no longer present (handles removal).
        let keep: std::collections::HashSet<&str> =
            self.tenants.iter().map(|t| t.id.as_str()).collect();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !keep.contains(stem) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        // Write each tenant atomically + 0600 (it holds secret tokens).
        for t in &self.tenants {
            let path = dir.join(format!("{}.json", t.id));
            let json = serde_json::to_string_pretty(t).context("serialize Tenant")?;
            let tmp = dir.join(format!("{}.json.tmp", t.id));
            std::fs::write(&tmp, json.as_bytes())
                .with_context(|| format!("write {}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&tmp)?.permissions();
                perm.set_mode(0o600);
                std::fs::set_permissions(&tmp, perm)?;
            }
            std::fs::rename(&tmp, &path)
                .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
        }
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
            telegram: None,
            lark: None,
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

    /// Replace one tenant's web token and return the newly minted secret.
    /// Returns `None` when `id` is not present; callers persist the registry
    /// after a successful rotation.
    pub fn rotate_token(&mut self, id: &str) -> Option<String> {
        let tenant = self.tenants.iter_mut().find(|t| t.id == id)?;
        let token = session_secret::mint();
        tenant.web_token = token.clone();
        Some(token)
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

    /// v0.8.20 F2 — set (or clear with `None`) a tenant's OWN Telegram bot.
    /// Returns whether the tenant exists.
    pub fn set_telegram(&mut self, id: &str, telegram: Option<TenantTelegram>) -> bool {
        match self.tenants.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.telegram = telegram;
                true
            }
            None => false,
        }
    }

    /// v0.8.20 F2 — set (or clear with `None`) a tenant's OWN Lark/Feishu app.
    pub fn set_lark(&mut self, id: &str, lark: Option<TenantLark>) -> bool {
        match self.tenants.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.lark = lark;
                true
            }
            None => false,
        }
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
    fn rotate_token_replaces_only_the_requested_tenant() {
        let mut reg = TenantRegistry::default();
        let alice = reg.add("alice");
        let bob = reg.add("bob");

        let next = reg.rotate_token(&alice.id).expect("alice exists");
        assert_ne!(next, alice.web_token);
        assert_eq!(reg.by_id(&alice.id).unwrap().web_token, next);
        assert_eq!(reg.by_id(&bob.id).unwrap().web_token, bob.web_token);
        assert!(reg.rotate_token("ughost").is_none());
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
        let dir = tmp.path().join("users");
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        reg.link_chat(&a.id, "telegram:111");
        reg.save(&dir).unwrap();

        let back = TenantRegistry::load(&dir);
        assert_eq!(back.tenants, reg.tenants);
        // Missing dir → empty registry, not an error.
        assert!(TenantRegistry::load(&tmp.path().join("absent"))
            .tenants
            .is_empty());
        // load → remove → save drops the per-user file.
        let mut reg2 = TenantRegistry::load(&dir);
        assert!(reg2.remove(&a.id));
        reg2.save(&dir).unwrap();
        assert!(TenantRegistry::load(&dir).tenants.is_empty());
    }

    #[test]
    fn set_telegram_and_lark_round_trip() {
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        assert!(reg.set_telegram(
            &a.id,
            Some(TenantTelegram {
                bot_token: "123:abc".into(),
                allowed_chat_ids: vec!["42".into()],
            }),
        ));
        assert_eq!(
            reg.by_id(&a.id)
                .unwrap()
                .telegram
                .as_ref()
                .unwrap()
                .bot_token,
            "123:abc"
        );
        // Clearing with None.
        assert!(reg.set_telegram(&a.id, None));
        assert!(reg.by_id(&a.id).unwrap().telegram.is_none());
        // Lark side.
        assert!(reg.set_lark(
            &a.id,
            Some(TenantLark {
                app_id: "cli_x".into(),
                app_secret: "s".into(),
                allowed_user_ids: vec![],
                use_feishu: true,
            }),
        ));
        assert!(reg.by_id(&a.id).unwrap().lark.is_some());
        // Unknown tenant → false.
        assert!(!reg.set_telegram("ughost", None));
    }

    #[test]
    #[cfg(unix)]
    fn save_enforces_0600_for_secret_tokens() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("users");
        let mut reg = TenantRegistry::default();
        let a = reg.add("alice");
        reg.set_telegram(
            &a.id,
            Some(TenantTelegram {
                bot_token: "t".into(),
                allowed_chat_ids: vec![],
            }),
        );
        reg.save(&dir).unwrap();
        // The per-user file is 0600 (it holds bot tokens); the dir is 0700.
        let file = dir.join(format!("{}.json", a.id));
        let fmode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            fmode, 0o600,
            "per-user file holds bot tokens → must be 0600"
        );
        let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700, "users/ dir → 0700");
        // Per-tenant IM creds survive a round-trip.
        let back = TenantRegistry::load(&dir);
        assert_eq!(
            back.by_id(&a.id)
                .unwrap()
                .telegram
                .as_ref()
                .unwrap()
                .bot_token,
            "t"
        );
    }
}
