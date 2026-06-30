//! Per-session `meta.json` — written at spawn, never deleted by `/stop`.
//!
//! Lives at `<project>/.ccteam/chat/<sid>/meta.json`. Persists all fields
//! needed to list and resume a session after it leaves the gateway live map
//! (stopped, daemon-restarted, or adopted from an external vendor session).
//!
//! This is the v0.8.21 Wave-1 additive layer; gateway-state.json is still the
//! live cache. Wave-2 makes meta.json the sole SoT and retires the `sessions`
//! vec from gateway-state.json.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{AgentVendor, PermissionMode, SessionProtocol};

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

// ── core struct ───────────────────────────────────────────────────────────────

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
    pub host: String,
    pub created_at: String,
    /// Updated only on turn completion — not on every event.
    pub last_active: String,
    pub origin: SessionOrigin,
}

// ── path helpers ──────────────────────────────────────────────────────────────

pub fn session_meta_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join("meta.json")
}

// ── write / read ──────────────────────────────────────────────────────────────

/// Atomically write `meta.json` (tmp + rename) to `.ccteam/chat/<sid>/`.
pub fn write_session_meta(project_dir: &Path, meta: &SessionMeta) -> Result<()> {
    let path = session_meta_path(project_dir, &meta.sid);
    std::fs::create_dir_all(path.parent().expect("path has parent"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(meta)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
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
