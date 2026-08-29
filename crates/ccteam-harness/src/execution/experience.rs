//! Per-delegation experience records — a **derived index**, not a new SoT.
//!
//! Lives at `<project>/.ccteam/experience.jsonl` (project-level, shared across
//! sids). Each line is one JSON object: either a terminal-turn summary
//! (`kind: "turn"`) or a human verdict (`kind: "verdict"` — written by a
//! future task; type + serde defined here).
//!
//! **Authority**: `turns.jsonl` + `progress.jsonl` remain the only state-of-
//! truth sources. This file is a rebuildable projection for self-evolution /
//! analytics. The live daemon's event pump is the sole online writer of
//! `kind: "turn"` rows; `ccteam internal experience rebuild <slug>`
//! regenerates them offline (disaster recovery).
//!
//! Append is atomic (POSIX `O_APPEND` + one `write_all`; record bodies fit
//! under PIPE_BUF). Reads tolerate half-flushed / corrupt lines.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ccteam_cost::UnifiedTokenUsage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Relative path of the project-level experience index.
const EXPERIENCE_REL: &str = ".ccteam/experience.jsonl";

/// One line in `experience.jsonl` — tagged by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // turn carries usage + skills map; line-oriented
pub enum ExperienceRecord {
    Turn(TurnExperience),
    Verdict(VerdictExperience),
}

/// Per-terminal-turn summary (live pump + rebuild both emit this shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnExperience {
    pub sid: String,
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    /// Vendor scalar (`"claude"` / `"codex"` / `"grok"`).
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Bot role name (empty string for roleless).
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UnifiedTokenUsage>,
    /// Deterministic per-turn cost; `None` when unpriceable (never a faked 0
    /// for an unknown model — same honesty contract as status cost rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// First 12 hex of sha256(`.claude/agents/<role>.md`) at spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_sha: Option<String>,
    /// Per-skill content digests at spawn (see [`skills_fingerprint`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_sha: Option<BTreeMap<String, String>>,
    pub signals: TurnSignals,
}

/// Lightweight turn signals (approximations — not a full telemetry SoT).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSignals {
    /// Delta of the session's `activity_events` counter across the turn
    /// (counts every pump event — assistant deltas, tool-use, progress —
    /// not a precise tool-call count). Rebuild fills 0 when unknown.
    pub tool_calls: u64,
    /// True when a user message was mirrored while a prior turn was still
    /// in flight (mid-turn steer). Always `false` after rebuild.
    pub steered: bool,
    /// Reserved for a future error-recovery detector; always `null` in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_recovered: Option<bool>,
}

/// Human accept/revise on a completed turn (schema only in this task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictExperience {
    pub sid: String,
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

/// Accept or request a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Revise,
}

/// Resolve `<project>/.ccteam/experience.jsonl`.
pub fn experience_jsonl_path(project_dir: &Path) -> PathBuf {
    project_dir.join(EXPERIENCE_REL)
}

/// Append `record` as one JSONL line. Creates parent dir + file when missing.
/// Returns the absolute path written for caller logging.
pub fn append_experience(project_dir: &Path, record: &ExperienceRecord) -> Result<PathBuf> {
    let path = experience_jsonl_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(record)? + "\n";
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(path)
}

/// Read every parseable record. Returns empty when the file is absent.
/// Corrupt / half-flushed / torn lines are skipped one line at a time
/// ([`super::fs_atomic::read_jsonl`]).
pub fn read_all_experience(project_dir: &Path) -> Result<Vec<ExperienceRecord>> {
    super::fs_atomic::read_jsonl(&experience_jsonl_path(project_dir))
}

// ── fingerprints ─────────────────────────────────────────────────────────────

/// First 12 hex of sha256 of `.claude/agents/<role>.md`.
/// `None` for roleless (empty role) or a missing file.
pub fn role_fingerprint(project_dir: &Path, role: &str) -> Option<String> {
    let role = role.trim();
    if role.is_empty() {
        return None;
    }
    let path = project_dir
        .join(".claude")
        .join("agents")
        .join(format!("{role}.md"));
    let bytes = fs::read(&path).ok()?;
    Some(short_sha256(&bytes))
}

/// Per-skill digests under `.claude/skills/<id>/`.
///
/// For each skill directory, digest = first 12 hex of sha256 over the sorted
/// lines `"<relpath>:<sha256(content)>"` of every regular file under that
/// skill (recursive, deterministic). Returns `None` when no skill directories
/// exist; `Some(map)` when at least one skill dir is present (values may
/// still hash empty dirs as the digest of zero lines).
pub fn skills_fingerprint(project_dir: &Path) -> Option<BTreeMap<String, String>> {
    let skills_root = project_dir.join(".claude").join("skills");
    let entries = fs::read_dir(&skills_root).ok()?;
    let mut map = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        map.insert(id, skill_dir_digest(&path));
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Digest of one skill directory: sha256 over sorted `"relpath:content_sha"` lines.
fn skill_dir_digest(skill_dir: &Path) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    collect_skill_files(skill_dir, skill_dir, &mut pairs);
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, content_sha) in &pairs {
        hasher.update(rel.as_bytes());
        hasher.update(b":");
        hasher.update(content_sha.as_bytes());
        hasher.update(b"\n");
    }
    hex12(hasher.finalize())
}

fn collect_skill_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_skill_files(root, &path, out);
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let relpath = rel.to_string_lossy().replace('\\', "/");
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            out.push((relpath, full_sha256_hex(&bytes)));
        }
    }
}

fn short_sha256(bytes: &[u8]) -> String {
    hex12(Sha256::digest(bytes))
}

fn full_sha256_hex(bytes: &[u8]) -> String {
    hex_full(Sha256::digest(bytes))
}

fn hex_full(digest: impl AsRef<[u8]>) -> String {
    let d = digest.as_ref();
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex12(digest: impl AsRef<[u8]>) -> String {
    hex_full(digest).chars().take(12).collect()
}

// ── rebuild (offline / disaster recovery) ────────────────────────────────────

/// Regenerate all `kind: "turn"` records from `chat/<sid>/turns.jsonl` +
/// `progress.jsonl` `chat_turn_completed` events (+ each sid's `meta.json` for
/// role / vendor / fingerprints), **preserving** existing `kind: "verdict"`
/// lines. Writes atomically (tmp + rename).
///
/// **Offline / disaster use only** — a live daemon may still be appending
/// concurrent turn rows; pre-v1.0 this is acceptable for recovery, not for
/// concurrent online rebuild.
///
/// Returns `(turns_written, verdicts_preserved)`.
pub fn rebuild_experience(
    project_dir: &Path,
    progress_path: Option<&Path>,
) -> Result<(usize, usize)> {
    // Preserve verdicts from the current file (if any).
    let existing = read_all_experience(project_dir).unwrap_or_default();
    let verdicts: Vec<ExperienceRecord> = existing
        .into_iter()
        .filter(|r| matches!(r, ExperienceRecord::Verdict(_)))
        .collect();
    let verdicts_preserved = verdicts.len();

    // Index progress chat_turn_completed by (sid, turn_id) for usage/model.
    let mut progress_by_key: BTreeMap<(String, String), serde_json::Value> = BTreeMap::new();
    if let Some(pp) = progress_path {
        if let Ok(events) = read_progress_events(pp) {
            for ev in events {
                if ev.get("event").and_then(|v| v.as_str()) != Some("chat_turn_completed") {
                    continue;
                }
                let Some(sid) = ev.get("sid").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(turn_id) = ev.get("turn_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                progress_by_key.insert((sid.to_string(), turn_id.to_string()), ev);
            }
        }
    }

    let mut turns: Vec<ExperienceRecord> = Vec::new();
    let chat_base = project_dir.join(".ccteam").join("chat");
    if let Ok(entries) = fs::read_dir(&chat_base) {
        let mut sids: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.is_dir() {
                    e.file_name().to_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        sids.sort();
        for sid in sids {
            let meta = super::session_meta::read_session_meta(project_dir, &sid).ok();
            let role = meta.as_ref().map(|m| m.role.clone()).unwrap_or_default();
            let vendor = meta
                .as_ref()
                .map(|m| vendor_label(m.vendor).to_string())
                .unwrap_or_else(|| "claude".to_string());
            let role_sha = meta.as_ref().and_then(|m| m.role_sha.clone()).or_else(|| {
                if role.is_empty() {
                    None
                } else {
                    role_fingerprint(project_dir, &role)
                }
            });
            let skills_sha = meta
                .as_ref()
                .and_then(|m| m.skills_sha.clone())
                .or_else(|| skills_fingerprint(project_dir));
            let cost_vendor = meta
                .as_ref()
                .map(|m| m.vendor.cost_vendor())
                .unwrap_or(ccteam_cost::Vendor::Claude);

            let Ok(turn_records) = super::turns_mirror::read_all_turns(project_dir, &sid) else {
                continue;
            };
            // turns.jsonl may have user-only + assistant-only rows for the same
            // turn_id; emit one experience row per distinct turn_id (prefer the
            // last row which usually carries the assistant side).
            let mut by_id: BTreeMap<String, super::turns_mirror::TurnRecord> = BTreeMap::new();
            for tr in turn_records {
                by_id.insert(tr.turn_id.clone(), tr);
            }
            for (turn_id, tr) in by_id {
                let progress = progress_by_key.get(&(sid.clone(), turn_id.clone()));
                let usage = progress
                    .and_then(|ev| ev.get("usage"))
                    .and_then(|u| serde_json::from_value::<UnifiedTokenUsage>(u.clone()).ok())
                    .or_else(|| {
                        if tr.usage.is_null() {
                            None
                        } else {
                            serde_json::from_value::<UnifiedTokenUsage>(tr.usage.clone()).ok()
                        }
                    });
                let model = progress
                    .and_then(|ev| ev.get("model"))
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                let cost_usd = usage.as_ref().and_then(|u| {
                    let m = model.as_deref().unwrap_or("");
                    ccteam_cost::resolve_turn_cost(u, cost_vendor, m)
                });
                let tool_calls = tr.tool_calls.len() as u64;
                turns.push(ExperienceRecord::Turn(TurnExperience {
                    sid: sid.clone(),
                    turn_id,
                    ts: tr.ts,
                    vendor: vendor.clone(),
                    model,
                    role: role.clone(),
                    usage,
                    cost_usd,
                    duration_ms: None,
                    role_sha: role_sha.clone(),
                    skills_sha: skills_sha.clone(),
                    signals: TurnSignals {
                        tool_calls,
                        steered: false,
                        error_recovered: None,
                    },
                }));
            }
        }
    }

    let turns_written = turns.len();
    // Stable order: turns (by sid, then ts) then preserved verdicts.
    turns.sort_by(|a, b| match (a, b) {
        (ExperienceRecord::Turn(x), ExperienceRecord::Turn(y)) => {
            x.sid.cmp(&y.sid).then_with(|| x.ts.cmp(&y.ts))
        }
        _ => std::cmp::Ordering::Equal,
    });

    let mut body = String::new();
    for rec in turns.iter().chain(verdicts.iter()) {
        body.push_str(&serde_json::to_string(rec)?);
        body.push('\n');
    }

    let path = experience_jsonl_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // Atomic replace (tmp + rename). Not fsync-durable — recovery index,
    // not a hard SoT (see module docs).
    let tmp = path.with_file_name("experience.jsonl.tmp");
    fs::write(&tmp, body.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    Ok((turns_written, verdicts_preserved))
}

fn vendor_label(v: crate::AgentVendor) -> &'static str {
    match v {
        crate::AgentVendor::Claude => "claude",
        crate::AgentVendor::Codex => "codex",
        crate::AgentVendor::Grok => "grok",
        crate::AgentVendor::Opencode => "opencode",
        crate::AgentVendor::Kimi => "kimi",
        crate::AgentVendor::Pi => "pi",
        crate::AgentVendor::Dsh => "dsh",
    }
}

fn read_progress_events(path: &Path) -> Result<Vec<serde_json::Value>> {
    super::fs_atomic::read_jsonl(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_turn(sid: &str, turn_id: &str) -> ExperienceRecord {
        ExperienceRecord::Turn(TurnExperience {
            sid: sid.into(),
            turn_id: turn_id.into(),
            ts: Utc::now(),
            vendor: "claude".into(),
            model: Some("claude-sonnet-4-6".into()),
            role: "cto".into(),
            usage: Some(UnifiedTokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            cost_usd: Some(0.001),
            duration_ms: Some(100),
            role_sha: Some("ab12cd34ef56".into()),
            skills_sha: None,
            signals: TurnSignals {
                tool_calls: 3,
                steered: false,
                error_recovered: None,
            },
        })
    }

    #[test]
    fn schema_round_trip_turn_and_verdict() {
        let turn = sample_turn("s1", "s1-1");
        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains(r#""kind":"turn""#));
        let back: ExperienceRecord = serde_json::from_str(&json).unwrap();
        match (&turn, &back) {
            (ExperienceRecord::Turn(a), ExperienceRecord::Turn(b)) => {
                assert_eq!(a.sid, b.sid);
                assert_eq!(a.turn_id, b.turn_id);
                assert_eq!(a.vendor, b.vendor);
                assert_eq!(a.role_sha, b.role_sha);
                assert_eq!(a.signals.tool_calls, b.signals.tool_calls);
            }
            _ => panic!("expected turn"),
        }

        let verdict = ExperienceRecord::Verdict(VerdictExperience {
            sid: "s1".into(),
            turn_id: "s1-1".into(),
            ts: Utc::now(),
            verdict: Verdict::Accept,
            feedback: Some("lgtm".into()),
        });
        let vjson = serde_json::to_string(&verdict).unwrap();
        assert!(vjson.contains(r#""kind":"verdict""#));
        assert!(vjson.contains(r#""verdict":"accept""#));
        let vback: ExperienceRecord = serde_json::from_str(&vjson).unwrap();
        match vback {
            ExperienceRecord::Verdict(v) => {
                assert!(matches!(v.verdict, Verdict::Accept));
                assert_eq!(v.feedback.as_deref(), Some("lgtm"));
            }
            _ => panic!("expected verdict"),
        }
    }

    #[test]
    fn append_and_read_tolerant_of_corrupt_line() {
        let tmp = TempDir::new().unwrap();
        let t1 = sample_turn("s1", "s1-1");
        append_experience(tmp.path(), &t1).unwrap();
        // Inject a corrupt line between good records.
        let path = experience_jsonl_path(tmp.path());
        let good = serde_json::to_string(&sample_turn("s1", "s1-2")).unwrap();
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str("{not-json\n");
        body.push_str(&good);
        body.push('\n');
        fs::write(&path, body).unwrap();

        let read = read_all_experience(tmp.path()).unwrap();
        assert_eq!(read.len(), 2);
        match &read[0] {
            ExperienceRecord::Turn(t) => assert_eq!(t.turn_id, "s1-1"),
            _ => panic!("expected turn"),
        }
        match &read[1] {
            ExperienceRecord::Turn(t) => assert_eq!(t.turn_id, "s1-2"),
            _ => panic!("expected turn"),
        }
    }

    #[test]
    fn read_missing_file_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read_all_experience(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn role_fingerprint_deterministic_and_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".claude").join("agents");
        fs::create_dir_all(&agents).unwrap();
        let role_path = agents.join("cto.md");
        fs::write(&role_path, b"you are cto v1").unwrap();
        let a = role_fingerprint(tmp.path(), "cto").unwrap();
        let b = role_fingerprint(tmp.path(), "cto").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        fs::write(&role_path, b"you are cto v2").unwrap();
        let c = role_fingerprint(tmp.path(), "cto").unwrap();
        assert_ne!(a, c);
        assert!(role_fingerprint(tmp.path(), "").is_none());
        assert!(role_fingerprint(tmp.path(), "missing").is_none());
    }

    #[test]
    fn skills_fingerprint_deterministic_and_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        assert!(skills_fingerprint(tmp.path()).is_none());

        let skill = tmp.path().join(".claude").join("skills").join("ci-watcher");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), b"watch ci").unwrap();
        let a = skills_fingerprint(tmp.path()).unwrap();
        let b = skills_fingerprint(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.get("ci-watcher").unwrap().len(), 12);

        fs::write(skill.join("SKILL.md"), b"watch ci harder").unwrap();
        let c = skills_fingerprint(tmp.path()).unwrap();
        assert_ne!(a.get("ci-watcher"), c.get("ci-watcher"));
    }

    #[test]
    fn rebuild_prefers_vendor_reported_turn_cost() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let now = Utc::now();
        let meta = super::super::session_meta::SessionMeta {
            mode: None,
            managed_by: Default::default(),
            stopped_at: None,
            sid: "s1".into(),
            slug: "demo".into(),
            vendor: crate::AgentVendor::Opencode,
            protocol: crate::SessionProtocol::Acp,
            role: String::new(),
            permission_mode: crate::PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: now.to_rfc3339(),
            last_active: now.to_rfc3339(),
            origin: super::super::session_meta::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 1,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        super::super::session_meta::write_session_meta(project, &meta).unwrap();
        super::super::turns_mirror::append_turn(
            project,
            "s1",
            &super::super::turns_mirror::TurnRecord {
                turn_id: "turn-1".into(),
                ts: now,
                vendor: "opencode".into(),
                role: String::new(),
                user: "hi".into(),
                assistant: "partial".into(),
                usage: serde_json::to_value(UnifiedTokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    reported_cost_usd: Some(0.73),
                    ..Default::default()
                })
                .unwrap(),
                status: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
                outcome: Some("failed".into()),
                error_kind: Some("max_tokens".into()),
                error: Some("output truncated".into()),
            },
        )
        .unwrap();

        assert_eq!(rebuild_experience(project, None).unwrap(), (1, 0));
        let record = read_all_experience(project).unwrap().remove(0);
        match record {
            ExperienceRecord::Turn(turn) => assert_eq!(turn.cost_usd, Some(0.73)),
            other => panic!("expected turn, got {other:?}"),
        }
    }

    #[test]
    fn rebuild_preserves_verdicts_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        // Seed meta + turns for s1.
        let now = Utc::now();
        let meta = super::super::session_meta::SessionMeta {
            mode: None,
            managed_by: Default::default(),
            stopped_at: None,
            sid: "s1".into(),
            slug: "demo".into(),
            vendor: crate::AgentVendor::Claude,
            protocol: crate::SessionProtocol::StreamJson,
            role: "cto".into(),
            permission_mode: crate::PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: now.to_rfc3339(),
            last_active: now.to_rfc3339(),
            origin: super::super::session_meta::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 1,
            cost_usd: None,
            tokens_total: None,
            role_sha: Some("deadbeef0001".into()),
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        super::super::session_meta::write_session_meta(project, &meta).unwrap();
        super::super::turns_mirror::append_turn(
            project,
            "s1",
            &super::super::turns_mirror::TurnRecord {
                turn_id: "s1-1".into(),
                ts: now,
                vendor: "claude".into(),
                role: "cto".into(),
                user: "hi".into(),
                assistant: "hello".into(),
                usage: serde_json::Value::Null,
                status: None,
                tool_calls: vec![],
                attachments: vec![],
                outcome: None,
                error_kind: None,
                error: None,
            },
        )
        .unwrap();

        // Manual verdict in the experience file.
        append_experience(
            project,
            &ExperienceRecord::Verdict(VerdictExperience {
                sid: "s1".into(),
                turn_id: "s1-1".into(),
                ts: now,
                verdict: Verdict::Revise,
                feedback: Some("try again".into()),
            }),
        )
        .unwrap();

        let (n1, v1) = rebuild_experience(project, None).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(v1, 1);
        let recs = read_all_experience(project).unwrap();
        assert_eq!(recs.len(), 2);
        assert!(matches!(&recs[0], ExperienceRecord::Turn(t) if t.turn_id == "s1-1"));
        assert!(matches!(
            &recs[1],
            ExperienceRecord::Verdict(v) if matches!(v.verdict, Verdict::Revise)
        ));

        // Second rebuild is idempotent (same shape).
        let (n2, v2) = rebuild_experience(project, None).unwrap();
        assert_eq!((n2, v2), (n1, v1));
        let recs2 = read_all_experience(project).unwrap();
        assert_eq!(recs2.len(), recs.len());
        match (&recs[0], &recs2[0]) {
            (ExperienceRecord::Turn(a), ExperienceRecord::Turn(b)) => {
                assert_eq!(a.sid, b.sid);
                assert_eq!(a.turn_id, b.turn_id);
                assert_eq!(a.role_sha, b.role_sha);
            }
            _ => panic!("expected turns"),
        }
    }
}
