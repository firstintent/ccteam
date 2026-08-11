//! Per-session `meta.json` — written at spawn, never deleted by `/stop`.
//!
//! Lives at `<project>/.ccteam/chat/<sid>/meta.json`. Persists all fields
//! needed to list and resume a session after it leaves the gateway live map
//! (stopped, daemon-restarted, or adopted from an external vendor session).
//!
//! v0.8.21 Wave-2 made this the SOLE session SoT: `gateway-state.json`'s
//! `sessions` vec is retired. The daemon now persists only routing
//! (`state/gateway/routing.json` — per-chat focus + the live-set) and the
//! monotonic sid counter (`state/sessions/next-sid`); on restart it cold-start
//! rebuilds the live map from these `meta.json` files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{AgentVendor, PermissionMode, SessionProtocol};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;

// ── origin tag ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// Created by ccteam (`start_session`).
    Ccteam,
    /// Adopted from an external vendor session (import flow).
    Adopted,
}

// ── title system (v0.8.22 P1) ────────────────────────────────────────────────

/// Which mechanism produced [`SessionMeta::title`]. Precedence (low → high):
/// `Auto` < `Vendor` < `User` — enforced by [`apply_title`] at WRITE time, so
/// an explicit rename is sticky (never later clobbered by the first-message
/// truncation or a vendor `ai-title`). `#[serde(default)]`-friendly: absent on
/// any meta.json predating this field (reads back as `None`, i.e. no title
/// yet — the caller falls back to `role`/`sid` display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// Rule-based truncation of the session's first user message.
    Auto,
    /// Extracted from the vendor's own transcript (`ai-title` / `custom-title`).
    Vendor,
    /// Explicit user rename (`/rename`, `PATCH /api/v1/sessions/{sid}`, or the
    /// SPA inline editor). Sticky: never overwritten by `Auto` or `Vendor`.
    User,
}

impl TitleSource {
    /// Precedence rank — higher wins. Used by [`apply_title`] to reject a
    /// lower-ranked write over an existing higher-ranked title.
    fn rank(self) -> u8 {
        match self {
            TitleSource::Auto => 0,
            TitleSource::Vendor => 1,
            TitleSource::User => 2,
        }
    }
}

/// Cap (in `char`s, not bytes — CJK-safe) for an auto-generated title.
const TITLE_MAX_CHARS: usize = 40;

/// Rule-based session title from a user's first message: collapse internal
/// whitespace/newlines to single spaces, trim, and cap at [`TITLE_MAX_CHARS`]
/// chars with a trailing ellipsis. **Pure, deterministic — no LLM call** (the
/// "no prompt injection" discipline extends to "no covert side-calls" for
/// something this cheap). Returns `None` for a blank/whitespace-only message
/// (nothing worth titling, e.g. a lone attachment).
pub fn truncate_title(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= TITLE_MAX_CHARS {
        return Some(chars.into_iter().collect());
    }
    let head: String = chars[..TITLE_MAX_CHARS].iter().collect();
    Some(format!("{head}…"))
}

/// Set `meta.title` to `candidate` under [`TitleSource`] precedence
/// (`Auto < Vendor < User`): a write is rejected iff a title already exists
/// AND `source` ranks lower than the title's current source — so a `User`
/// rename is never clobbered by a later `Auto`/`Vendor` write, and a `Vendor`
/// title survives a later `Auto` one. A blank `candidate` is always rejected
/// (never clears a title). Returns `true` iff the title was actually written
/// — callers that only persist on change (e.g. avoiding a redundant
/// `write_session_meta`) can skip the write on `false`.
pub fn apply_title(meta: &mut SessionMeta, candidate: String, source: TitleSource) -> bool {
    if candidate.trim().is_empty() {
        return false;
    }
    if meta.title.is_some() {
        let current_rank = meta.title_source.map(TitleSource::rank).unwrap_or(0);
        if source.rank() < current_rank {
            return false;
        }
    }
    meta.title = Some(candidate);
    meta.title_source = Some(source);
    true
}

// ── core struct ───────────────────────────────────────────────────────────────

/// Who runs the process behind a session.
///
/// The ledger is shared — one sid namespace, one `meta.json` shape, one
/// delegation tree — but only one of these kinds has a thread ccteam owns, and
/// every driveable surface has to be able to tell them apart:
///
/// | | [`Self::Ccteam`] | [`Self::External`] |
/// |---|---|---|
/// | thread | ccteam's | none — a hand-started process |
/// | dispatch / steer / stop | yes | no: refuse, don't pretend |
/// | capacity eviction, budget | applies | never (there is nothing to stop) |
/// | delegation parent, project, tree | yes | yes — this is the whole point |
///
/// An external node exists so that a hand-started agent's children mount under
/// it instead of as roots. Taking one over later is a real transition (stop the
/// process, resume its vendor session under management, flip this field), not a
/// flag that quietly changes meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBy {
    /// ccteam spawned it and owns its thread.
    #[default]
    Ccteam,
    /// A hand-started vendor process that enrolled over `POST /mcp`.
    External,
}

impl ManagedBy {
    /// Whether ccteam can send this session work.
    pub fn is_driveable(self) -> bool {
        matches!(self, ManagedBy::Ccteam)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub sid: String,
    pub slug: String,
    pub vendor: AgentVendor,
    pub protocol: SessionProtocol,
    pub role: String,
    pub permission_mode: PermissionMode,
    /// Canonical owner tag, e.g. `"user:web-api"` or `"telegram:123"`.
    pub owner: String,
    /// Anthropic session UUID (stream-json: deterministic FNV; TUI: from
    /// `active-session-id`; Codex: empty; adopted: the foreign uuid).
    pub vendor_uuid: String,
    /// Opaque model requested when the session was spawned. `None` means the
    /// vendor default was requested. It is advisory/display state only and is
    /// always passed back to the vendor verbatim on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The canonical model the VENDOR reported for this session's most recent
    /// completed turn (off its `chat_turn_completed` accounting, refreshed by
    /// the same per-turn meta write as `turn_count`/`cost_usd`). Display-only
    /// and NEVER replayed to the vendor: [`Self::model`] is what the user
    /// asked for, this is what actually ran — the fact that survives a stop,
    /// so an A2A child spawned on the vendor default still has a model to
    /// show after it leaves the live map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_model: Option<String>,
    /// Reasoning effort requested when the session was spawned, same contract
    /// as [`Self::model`]: opaque, `None` = vendor default, replayed verbatim
    /// on every re-spawn.
    ///
    /// It exists BECAUSE `model` did and this did not: a resume, a role
    /// switch, or a rebuild restored the model and silently reset the effort
    /// to the vendor default — an explicit pick surviving one axis but not the
    /// other, invisibly, which is the same failure the spawn surfaces just
    /// stopped committing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub host: String,
    pub created_at: String,
    /// Updated only on turn completion — not on every event.
    pub last_active: String,
    pub origin: SessionOrigin,
    /// v0.8.22 P1 — user-facing session title (session-title system). `None`
    /// until either the first user message is auto-titled or a vendor
    /// `ai-title`/`custom-title` is adopted — see [`TitleSource`] +
    /// [`apply_title`] for precedence. `#[serde(default)]` keeps every
    /// pre-existing meta.json parseable (reads back `None`).
    #[serde(default)]
    pub title: Option<String>,
    /// Which mechanism produced [`Self::title`]; `None` alongside a `None`
    /// title (nothing set yet). `#[serde(default)]` for the same reason.
    #[serde(default)]
    pub title_source: Option<TitleSource>,
    /// v0.8.22 P1 — number of turns recorded in this session's
    /// `turns.jsonl` (best-effort, refreshed on each completed turn).
    /// `#[serde(default)]` keeps older metas parseable (reads back `0`).
    #[serde(default)]
    pub turn_count: u64,
    /// v0.8.22 P1 — accrued cost (USD) for this session's priced turns —
    /// the same deterministic per-turn accounting `GET /api/v1/status`'s
    /// per-session cost row uses. `None` when no turn has priced yet
    /// (never a faked `0.0`). `#[serde(default)]` for old metas.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// v0.9.5 feedback fix — accrued RAW token count across this session's
    /// turns (every usage bucket summed). Unlike [`Self::cost_usd`] this
    /// accrues for vendors with no price table (codex/grok/opencode/kimi),
    /// so a non-claude session still shows an honest ledger number. `None`
    /// when no turn reported usage yet. `#[serde(default)]` for old metas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<u64>,
    /// v0.9 T5 — first 12 hex of sha256(`.claude/agents/<role>.md`)
    /// captured at (re)spawn. Snapshot semantics: mid-session role-file
    /// edits are intentionally NOT re-hashed. `None` for roleless /
    /// missing agent files. Legacy metas parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_sha: Option<String>,
    /// v0.9 T5 — per-skill content digests under `.claude/skills/` at
    /// (re)spawn (see [`super::experience::skills_fingerprint`]). Snapshot
    /// at spawn; mid-session skill edits not re-hashed. Legacy metas
    /// parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_sha: Option<BTreeMap<String, String>>,
    /// v0.8.24 F5 — which surface triggered session creation:
    /// `im` | `web` | `mcp` | `session_spawn`. Legacy metas parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// v0.9.0 W2 (F2) — delegation parent: the sid of the session whose
    /// principal spawned this one via `session_spawn`. `None` for a
    /// human-created (root) session. Legacy metas parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_sid: Option<String>,
    /// v0.9.0 W2 (F2) — audit label: the role of the spawning principal at
    /// delegation time (may differ from this session's own role). `None` for
    /// a root session. Legacy metas parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by_role: Option<String>,
    /// v0.9.0 W2 (F2/F5) — delegation depth: `0` for a root (human-created)
    /// session, `parent.delegation_depth + 1` for a delegated child. The
    /// `delegation.max_depth` guardrail caps this. Legacy metas parse as `0`.
    #[serde(default)]
    pub delegation_depth: u32,
    /// Who runs the process behind this session — see [`ManagedBy`]. Legacy
    /// metas parse as [`ManagedBy::Ccteam`], which is what they all are.
    #[serde(default)]
    pub managed_by: ManagedBy,
}

// ── path helpers ──────────────────────────────────────────────────────────────

pub fn session_meta_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join("meta.json")
}

// ── write / read ──────────────────────────────────────────────────────────────

/// Durably write `meta.json` to `.ccteam/chat/<sid>/`: tmp file + `fsync` +
/// rename + best-effort parent-dir `fsync` (see [`atomic_write_durable`]).
/// `meta.json` is the sole session SoT (v0.8.21 Wave-2), so a power-loss
/// rollback here would resurrect stale session state — worth the extra
/// fsync given this file is written only at spawn / turn-completion
/// frequency, not per-event.
pub fn write_session_meta(project_dir: &Path, meta: &SessionMeta) -> Result<()> {
    let path = session_meta_path(project_dir, &meta.sid);
    std::fs::create_dir_all(path.parent().expect("path has parent"))?;
    atomic_write_durable(&path, serde_json::to_string_pretty(meta)?.as_bytes())
}

pub fn read_session_meta(project_dir: &Path, sid: &str) -> Result<SessionMeta> {
    let path = session_meta_path(project_dir, sid);
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Update `last_active` to now. Best-effort: silently ignores missing file.
pub fn touch_last_active(project_dir: &Path, sid: &str) {
    if let Ok(mut meta) = read_session_meta(project_dir, sid) {
        meta.last_active = Utc::now().to_rfc3339();
        let _ = write_session_meta(project_dir, &meta);
    }
}

// ── discovery ─────────────────────────────────────────────────────────────────

/// Scan `<project_dir>/.ccteam/chat/*/meta.json` and return all parseable
/// metas, sorted by `last_active` descending.
pub fn list_session_metas(project_dir: &Path) -> Vec<SessionMeta> {
    let chat_base = project_dir.join(".ccteam").join("chat");
    let Ok(entries) = std::fs::read_dir(&chat_base) else {
        return vec![];
    };
    let mut out: Vec<SessionMeta> = entries
        .flatten()
        .filter_map(|e| {
            let meta_path = e.path().join("meta.json");
            let raw = std::fs::read_to_string(&meta_path).ok()?;
            serde_json::from_str(&raw).ok()
        })
        .collect();
    out.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    out
}

// ── external vendor discovery ─────────────────────────────────────────────────

/// A Claude session discovered from `~/.claude/projects/` that has no ccteam
/// sid yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalClaudeSession {
    pub vendor_uuid: String,
    /// Best-effort title extracted from the jsonl tail.
    pub title: String,
    pub last_active: String,
    /// The cwd this session was created in.
    pub cwd: String,
}

/// Discover Claude sessions under `~/.claude/projects/` whose recorded `cwd`
/// matches `project_cwd`, filtering out any uuid already tracked as a known
/// ccteam vendor_uuid (to avoid duplicating adopted sessions).
///
/// **Implementation note**: reads the *tail* of each jsonl (last 16 KiB) to
/// extract `cwd` and title metadata — same approach Claude's own resume picker
/// uses. Does NOT rely on `encode_project_cwd` path encoding.
pub fn discover_external_claude_sessions(
    project_cwd: &Path,
    known_uuids: &std::collections::HashSet<String>,
) -> Vec<ExternalClaudeSession> {
    let claude_projects_dir = match home_dir() {
        Some(h) => h.join(".claude").join("projects"),
        None => return vec![],
    };
    let Ok(project_dirs) = std::fs::read_dir(&claude_projects_dir) else {
        return vec![];
    };

    let cwd_str = project_cwd.to_string_lossy();
    let mut out = vec![];

    for dir_entry in project_dirs.flatten() {
        let dir_path = dir_entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            if file_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !looks_like_uuid(stem) {
                continue;
            }
            if known_uuids.contains(stem) {
                continue;
            }
            // Read tail to find cwd and title.
            let Some(tail) = read_tail(&file_path, 16 * 1024) else {
                continue;
            };
            let Some(session_cwd) = extract_cwd_from_jsonl(&tail) else {
                continue;
            };
            if session_cwd.trim_end_matches('/') != cwd_str.trim_end_matches('/') {
                continue;
            }
            // Skip subagent sessions (first line type == "agent-setting").
            if is_subagent_jsonl(&file_path) {
                continue;
            }
            let title = extract_title_from_jsonl(&tail).unwrap_or_default();
            let last_active = file_entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                })
                .map(|secs| {
                    use chrono::TimeZone;
                    Utc.timestamp_opt(secs as i64, 0)
                        .single()
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            out.push(ExternalClaudeSession {
                vendor_uuid: stem.to_string(),
                title,
                last_active,
                cwd: session_cwd,
            });
        }
    }
    out.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    out
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn looks_like_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Read the last `max_bytes` from a file.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    // Lossy: a 16 KiB tail cut can land mid-UTF-8 char (common with CJK
    // transcripts). The partial first line fails to parse and is skipped
    // anyway, so a replacement char there is harmless — but a strict
    // `from_utf8` here would discard the WHOLE tail and make the session
    // undiscoverable.
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Extract `cwd` from any JSON line in the tail that contains a `"cwd"` key.
fn extract_cwd_from_jsonl(tail: &str) -> Option<String> {
    for line in tail.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// Extract title from `"type":"custom-title"` or `"type":"ai-title"` lines.
fn extract_title_from_jsonl(tail: &str) -> Option<String> {
    let mut title: Option<String> = None;
    for line in tail.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "custom-title" {
                if let Some(ct) = v.get("customTitle").and_then(|c| c.as_str()) {
                    return Some(ct.to_string()); // custom title wins
                }
            } else if t == "ai-title" {
                if let Some(at) = v.get("aiTitle").and_then(|c| c.as_str()) {
                    title = Some(at.to_string());
                }
            }
        }
    }
    title
}

fn is_subagent_jsonl(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    if let Some(first_line) = raw.lines().next() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(first_line) {
            return v.get("type").and_then(|t| t.as_str()) == Some("agent-setting");
        }
    }
    false
}

#[cfg(test)]
mod title_tests {
    use super::*;

    fn blank_meta() -> SessionMeta {
        SessionMeta {
            managed_by: Default::default(),
            sid: "s1".into(),
            slug: "demo".into(),
            vendor: AgentVendor::Claude,
            protocol: SessionProtocol::StreamJson,
            role: "cto".into(),
            permission_mode: PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_active: "2026-01-01T00:00:00Z".into(),
            origin: SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        }
    }

    // ---- truncate_title ----------------------------------------------------

    #[test]
    fn truncate_title_short_message_passes_through_trimmed() {
        assert_eq!(
            truncate_title("  fix the login bug  "),
            Some("fix the login bug".to_string())
        );
    }

    #[test]
    fn truncate_title_collapses_internal_whitespace_and_newlines() {
        assert_eq!(
            truncate_title("fix\n\nthe   login\tbug"),
            Some("fix the login bug".to_string())
        );
    }

    #[test]
    fn truncate_title_blank_or_whitespace_only_is_none() {
        assert_eq!(truncate_title(""), None);
        assert_eq!(truncate_title("   \n\t  "), None);
    }

    #[test]
    fn truncate_title_caps_at_40_chars_with_ellipsis() {
        let long = "a".repeat(100);
        let got = truncate_title(&long).unwrap();
        // 40 kept chars + one ellipsis char.
        assert_eq!(got.chars().count(), TITLE_MAX_CHARS + 1);
        assert!(got.ends_with('…'));
        assert_eq!(
            got.chars().take(TITLE_MAX_CHARS).collect::<String>(),
            "a".repeat(TITLE_MAX_CHARS)
        );
    }

    #[test]
    fn truncate_title_is_cjk_safe_char_not_byte_capped() {
        // Each CJK char is >1 byte in UTF-8; the cap must count chars, so a
        // 100-char CJK string still yields exactly 40 kept chars + ellipsis,
        // never panicking on a mid-codepoint byte slice.
        let long = "会".repeat(100);
        let got = truncate_title(&long).unwrap();
        assert_eq!(got.chars().count(), TITLE_MAX_CHARS + 1);
    }

    #[test]
    fn truncate_title_exactly_at_cap_has_no_ellipsis() {
        let exact = "b".repeat(TITLE_MAX_CHARS);
        assert_eq!(truncate_title(&exact), Some(exact));
    }

    // ---- apply_title precedence / rename-stability -------------------------

    #[test]
    fn apply_title_sets_on_blank_meta() {
        let mut meta = blank_meta();
        assert!(apply_title(
            &mut meta,
            "first message".into(),
            TitleSource::Auto
        ));
        assert_eq!(meta.title.as_deref(), Some("first message"));
        assert_eq!(meta.title_source, Some(TitleSource::Auto));
    }

    #[test]
    fn apply_title_rejects_blank_candidate() {
        let mut meta = blank_meta();
        assert!(!apply_title(&mut meta, "   ".into(), TitleSource::Auto));
        assert!(meta.title.is_none());
    }

    #[test]
    fn apply_title_user_rename_is_never_overwritten_by_auto_or_vendor() {
        let mut meta = blank_meta();
        assert!(apply_title(
            &mut meta,
            "renamed by user".into(),
            TitleSource::User
        ));

        // A later first-message auto-title must NOT clobber the rename.
        assert!(!apply_title(
            &mut meta,
            "auto from message".into(),
            TitleSource::Auto
        ));
        assert_eq!(meta.title.as_deref(), Some("renamed by user"));

        // Nor may a vendor ai-title.
        assert!(!apply_title(
            &mut meta,
            "vendor ai-title".into(),
            TitleSource::Vendor
        ));
        assert_eq!(meta.title.as_deref(), Some("renamed by user"));
        assert_eq!(meta.title_source, Some(TitleSource::User));
    }

    #[test]
    fn apply_title_vendor_beats_auto_but_not_vice_versa() {
        let mut meta = blank_meta();
        assert!(apply_title(
            &mut meta,
            "auto title".into(),
            TitleSource::Auto
        ));
        assert!(apply_title(
            &mut meta,
            "vendor title".into(),
            TitleSource::Vendor
        ));
        assert_eq!(meta.title.as_deref(), Some("vendor title"));

        // A later Auto write (e.g. a stray call) must not downgrade Vendor.
        assert!(!apply_title(
            &mut meta,
            "later auto".into(),
            TitleSource::Auto
        ));
        assert_eq!(meta.title.as_deref(), Some("vendor title"));
    }

    #[test]
    fn apply_title_user_can_rename_again() {
        let mut meta = blank_meta();
        assert!(apply_title(
            &mut meta,
            "first rename".into(),
            TitleSource::User
        ));
        assert!(apply_title(
            &mut meta,
            "second rename".into(),
            TitleSource::User
        ));
        assert_eq!(meta.title.as_deref(), Some("second rename"));
    }

    #[test]
    fn session_meta_json_round_trips_without_title_fields_present() {
        // A pre-v0.8.22 meta.json has no title/title_source/turn_count/cost_usd
        // keys at all — `#[serde(default)]` must still parse it.
        let legacy = serde_json::json!({
            "sid": "s1",
            "slug": "demo",
            "vendor": "claude",
            "protocol": "stream-json",
            "role": "cto",
            "permission_mode": "skip",
            "owner": "user:web-api",
            "vendor_uuid": "",
            "host": "local",
            "created_at": "2026-01-01T00:00:00Z",
            "last_active": "2026-01-01T00:00:00Z",
            "origin": "ccteam",
        });
        let meta: SessionMeta = serde_json::from_value(legacy).expect("legacy meta.json parses");
        assert!(meta.title.is_none());
        assert!(meta.title_source.is_none());
        assert_eq!(meta.turn_count, 0);
        assert!(meta.cost_usd.is_none());
        // v0.9 T5 — pre-fingerprint metas must still load with None digests.
        assert!(meta.role_sha.is_none());
        assert!(meta.skills_sha.is_none());
        // v0.9.0 W2 — pre-delegation metas load with no parent + depth 0.
        assert!(meta.parent_sid.is_none());
        assert!(meta.spawned_by_role.is_none());
        assert_eq!(meta.delegation_depth, 0);
    }
}
