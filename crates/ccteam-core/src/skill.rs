//! Legacy skill-directory cleanup.
//!
//! ccteam no longer ships any bundled skills: their former functions
//! (project creation, CLI/MCP control wrap, IM onboarding) now live in
//! MCP tools, the `cto` role, and the `ccteam config` command. The
//! generic skill-install machinery was removed once the shipped-skill
//! set became empty.
//!
//! What remains is the reverse-migration name list: `ccteam doctor`
//! still scans for stale `~/.claude/skills/<old>/` directories left by
//! earlier versions and cleans them up. See
//! [`crate::tool_surface::migrate_legacy_skill_dirs`].

/// Legacy skill directory names that `ccteam doctor` removes when found.
///
/// These are all skills earlier ccteam versions installed into
/// `~/.claude/skills/`. They are listed here so an upgrading user has
/// the stale directories cleaned up.
///
/// - `cct-*` are the pre-rename names.
/// - `ccteam-team-author` / `ccteam-project-creator` were folded into
///   `ccteam-creator`.
/// - `ccteam-control` / `ccteam-creator` / `ccteam-im-setup` were the
///   last bundled skills, removed once their functions moved to MCP
///   tools / the `cto` role / `ccteam config`.
pub const LEGACY_SKILL_NAMES: &[&str] = &[
    "cct-control",
    "cct-team-author",
    "cct-project-creator",
    "ccteam-team-author",
    "ccteam-project-creator",
    "ccteam-control",
    "ccteam-creator",
    "ccteam-im-setup",
];
