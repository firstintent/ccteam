//! Shared ownership policy for authenticated web identities.
//!
//! The web REST surface and daemon-side MCP ingress both authorize projects
//! from the same persisted `ProjectState.owner` tag. Keep the pure policy here
//! so neither layer has to depend on the other.

/// Synthetic identity id used by the shared admin web console.
pub const ADMIN_WEB_ID: &str = "web-api";

/// Prefix of the synthetic web-console owner namespace (`user:<id>`). Owners
/// outside it (`telegram:<chat_id>`, …) are IM-owned; `None` is unowned.
pub const WEB_OWNER_PREFIX: &str = "user:";

/// Return the persisted owner tag for resources created by a web identity.
pub fn owner_tag(user_id: &str, is_admin: bool) -> String {
    if is_admin {
        format!("user:{ADMIN_WEB_ID}")
    } else {
        format!("user:{user_id}")
    }
}

/// Return the web frontend chat id used as the gateway's reply-to seed.
pub fn web_chat_id(user_id: &str, is_admin: bool) -> String {
    if is_admin {
        ADMIN_WEB_ID.to_string()
    } else {
        user_id.to_string()
    }
}

/// Whether `owner` belongs to a per-user tenant rather than the shared admin
/// pool. Unowned and IM-owned resources are not tenant-owned.
pub fn is_tenant_owned(owner: Option<&str>) -> bool {
    owner
        .and_then(|owner| owner.strip_prefix(WEB_OWNER_PREFIX))
        .is_some_and(|id| id != ADMIN_WEB_ID)
}

/// Whether `owner` is a WEB-CONSOLE owner tag (`user:<id>`) rather than an
/// IM chat (`telegram:<chat_id>`, …) or unowned.
pub fn is_web_pool_owned(owner: &str) -> bool {
    owner.starts_with(WEB_OWNER_PREFIX)
}

/// Shared project/resource ownership policy.
///
/// A tenant sees only `user:<its-id>`. The admin sees its own shared pool plus
/// legacy/unowned and IM-owned resources, but not another tenant's private
/// resources.
pub fn can_see_owner(user_id: &str, is_admin: bool, owner: Option<&str>) -> bool {
    let own = owner_tag(user_id, is_admin);
    if owner == Some(own.as_str()) {
        return true;
    }
    is_admin && !is_tenant_owned(owner)
}

/// Shared SESSION-visibility policy — the IM/gateway twin of
/// [`can_see_owner`], expressed on the SAME ownership tags.
///
/// A frontend chat sees a session iff:
/// 1. it OWNS it — `owner == viewer_identity`, the chat-level isolation that
///    keeps two IM chats apart even on one bot, or
/// 2. the session lives in the WEB-CONSOLE pool (`user:<id>`) **and**
///    [`can_see_owner`] grants this identity that pool — i.e. only its OWN
///    console: the admin sees `user:web-api`, a tenant sees `user:<its-id>`,
///    and neither sees the other's.
///
/// Rule 2 used to be a blanket "any `user:*` owner is shared", which was
/// correct only while the web console had a single (admin) identity. With
/// per-user web tokens that blanket leaked EVERY tenant's sessions into the
/// admin's IM bot — and into every other tenant's console — so the pool leg now
/// routes through the same core policy the project ACL uses. An IM-owned
/// session (`telegram:<chat_id>`) is never pooled, which preserves both "IM
/// chats stay isolated from each other" and "the web console does not see IM
/// sessions".
///
/// `viewer_identity` is the viewer's canonical owner identity string
/// (`user:<id>` for a web console / per-tenant bot, `telegram:<chat_id>` for
/// the global bot); `(user_id, is_admin)` is that same viewer resolved to the
/// shared ACL identity.
pub fn can_see_session_owner(
    viewer_identity: &str,
    user_id: &str,
    is_admin: bool,
    owner: &str,
) -> bool {
    if owner == viewer_identity {
        return true;
    }
    is_web_pool_owned(owner) && can_see_owner(user_id, is_admin, Some(owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_and_web_chat_tags_are_stable() {
        assert_eq!(owner_tag("admin", true), "user:web-api");
        assert_eq!(web_chat_id("admin", true), "web-api");
        assert_eq!(owner_tag("ualice", false), "user:ualice");
        assert_eq!(web_chat_id("ualice", false), "ualice");
    }

    #[test]
    fn tenant_ownership_classification_excludes_admin_pool() {
        assert!(is_tenant_owned(Some("user:ualice")));
        assert!(!is_tenant_owned(Some("user:web-api")));
        assert!(!is_tenant_owned(Some("telegram:42")));
        assert!(!is_tenant_owned(None));
    }

    #[test]
    fn admin_and_tenant_visibility_matches_web_policy() {
        assert!(can_see_owner("admin", true, Some("user:web-api")));
        assert!(can_see_owner("admin", true, Some("telegram:42")));
        assert!(can_see_owner("admin", true, None));
        assert!(!can_see_owner("admin", true, Some("user:ualice")));

        assert!(can_see_owner("ualice", false, Some("user:ualice")));
        assert!(!can_see_owner("ualice", false, Some("user:ubob")));
        assert!(!can_see_owner("ualice", false, Some("user:web-api")));
        assert!(!can_see_owner("ualice", false, Some("telegram:42")));
        assert!(!can_see_owner("ualice", false, None));
    }

    /// The session rule = own ⊕ the web pool this identity may see. The
    /// regression it guards: a blanket "any `user:*` owner is shared" leaked
    /// every tenant's sessions to the admin's IM bot and to other tenants.
    #[test]
    fn session_visibility_is_own_plus_only_its_own_web_pool() {
        // The owner's global IM bot: its own chat + the admin web pool only.
        let admin_im = "telegram:339";
        assert!(can_see_session_owner(
            admin_im,
            ADMIN_WEB_ID,
            true,
            admin_im
        ));
        assert!(can_see_session_owner(
            admin_im,
            ADMIN_WEB_ID,
            true,
            "user:web-api"
        ));
        assert!(
            !can_see_session_owner(admin_im, ADMIN_WEB_ID, true, "user:ualice"),
            "the admin bot must NOT receive a tenant's sessions"
        );
        assert!(
            !can_see_session_owner(admin_im, ADMIN_WEB_ID, true, "telegram:999"),
            "another IM chat on the same bot stays isolated"
        );

        // A tenant (its web console AND its own IM bot are one identity).
        let tenant = "user:ualice";
        assert!(can_see_session_owner(tenant, "ualice", false, tenant));
        assert!(!can_see_session_owner(tenant, "ualice", false, "user:ubob"));
        assert!(!can_see_session_owner(
            tenant,
            "ualice",
            false,
            "user:web-api"
        ));
        assert!(!can_see_session_owner(
            tenant,
            "ualice",
            false,
            "telegram:339"
        ));

        // The admin web console: its own pool, never an IM chat's sessions.
        let admin_web = "user:web-api";
        assert!(can_see_session_owner(
            admin_web,
            ADMIN_WEB_ID,
            true,
            admin_web
        ));
        assert!(!can_see_session_owner(
            admin_web,
            ADMIN_WEB_ID,
            true,
            "telegram:339"
        ));
        assert!(!can_see_session_owner(
            admin_web,
            ADMIN_WEB_ID,
            true,
            "user:ualice"
        ));
    }
}
