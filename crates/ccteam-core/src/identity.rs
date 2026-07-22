//! Shared ownership policy for authenticated web identities.
//!
//! The web REST surface and daemon-side MCP ingress both authorize projects
//! from the same persisted `ProjectState.owner` tag. Keep the pure policy here
//! so neither layer has to depend on the other.

/// Synthetic identity id used by the shared admin web console.
pub const ADMIN_WEB_ID: &str = "web-api";

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
        .and_then(|owner| owner.strip_prefix("user:"))
        .is_some_and(|id| id != ADMIN_WEB_ID)
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
}
