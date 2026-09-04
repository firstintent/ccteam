//! v0.9.0 W2 (F2/F7) — durable delegation watch: the crash-safe record of a
//! parent's interest in a child's completion.
//!
//! Lives at `<project>/.ccteam/chat/<child_sid>/delegation.json`, written with
//! the same `atomic_write_durable` (tmp+fsync+rename) discipline as `meta.json`
//! so a power-loss can never leave a half-written watch. There is at most ONE
//! watch per child (a later dispatch to the same child overwrites/updates it —
//! "one child has one pending parent" per the steer semantics), and it lives
//! only until the dispatched task's turn boundary, which spends it (the file is
//! removed). The gateway
//! keeps an in-memory mirror for the hot path; this file is the SoT that a
//! daemon-restart reconcile reads to deliver any completion notifications that
//! were missed while the daemon was down (at-least-once, deduped by
//! `notified_turns`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;

/// When a delegation watch wakes the parent. The unit of the notification
/// contract is the TASK (a vendor turn), not each mirrored assistant message —
/// a chatty child (codex narrates checkpoints as separate messages inside one
/// turn) must not flood the parent's context. In every mode the watch covers
/// exactly ONE task: the turn boundary that ends the dispatched task also
/// spends the watch (v0.10.1), so a child that keeps living its own life after
/// the task — an IM root, a session someone else drives — never keeps feeding
/// the dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyMode {
    /// Notify once, at the vendor turn boundary — the child finished the
    /// dispatched task and went idle — with the LONGER excerpt (interim
    /// narration stays in the ledger either way).
    Final,
    /// Same boundary as [`Self::Final`], a SHORTER excerpt — the default. The
    /// wake-up point is a property of the task; how much of the answer rides
    /// along is the parent's context budget, so the two are separate axes and
    /// only the cap differs here. Brief by default because the parent is the
    /// scarcest context in a team (issue #194: a planner paid 2000 chars ×
    /// twenty children for reports it only needed the verdict line of); the
    /// whole answer is always one exact `agent_read` call away, and a parent
    /// that wants it pushed asks for `final`.
    #[default]
    Brief,
    /// Never notify — ledger-only, the parent polls `agent_read`.
    Off,
}

/// The wire refusal for the retired `all` mode.
///
/// It promised a notification per mirrored assistant message; since the
/// notification unit became the TASK (v0.9.5) the notifier has skipped every
/// non-boundary signal unconditionally, so `all` was `final` wearing another
/// name — a decision trap that cost callers a choice and bought them nothing.
/// Removed rather than kept as an alias (pre-1.0: no compat shims).
const NOTIFY_ALL_REMOVED: &str = "notify `all` was removed; use final";

impl NotifyMode {
    /// Stable lowercase wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyMode::Final => "final",
            NotifyMode::Brief => "brief",
            NotifyMode::Off => "off",
        }
    }

    /// Parse a WIRE value: `"final"|"brief"|"off"` — plus the boolean form
    /// (`true` → `Final`, `false` → `Off`) so existing callers keep working.
    /// The retired `"all"` is a readable error here; only the state-file
    /// [`Deserialize`] impl still accepts it (as `Final`, which is what it
    /// always did).
    pub fn parse_value(v: &serde_json::Value) -> Result<Self, String> {
        match v {
            serde_json::Value::Bool(true) => Ok(NotifyMode::Final),
            serde_json::Value::Bool(false) => Ok(NotifyMode::Off),
            serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "final" | "true" => Ok(NotifyMode::Final),
                "brief" => Ok(NotifyMode::Brief),
                "off" | "false" | "none" => Ok(NotifyMode::Off),
                "all" => Err(NOTIFY_ALL_REMOVED.to_string()),
                other => Err(format!(
                    "invalid notify mode `{other}` (expected `final` | `brief` | `off`)"
                )),
            },
            other => Err(format!(
                "invalid notify value {other} (expected `final` | `brief` | `off` or a boolean)"
            )),
        }
    }
}

impl Serialize for NotifyMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NotifyMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        // State-file robustness, NOT an API alias: a `delegation.json` written
        // before `all` was retired must still load, or a daemon restart would
        // drop that child's completion notification entirely. It reads as the
        // `final` the notifier has been treating it as since v0.9.5.
        if v.as_str()
            .is_some_and(|raw| raw.trim().eq_ignore_ascii_case("all"))
        {
            tracing::warn!("delegation watch carries retired notify `all`; reading it as `final`");
            return Ok(NotifyMode::Final);
        }
        NotifyMode::parse_value(&v).map_err(serde::de::Error::custom)
    }
}

/// One child's completion watch — who to notify, and what has already been
/// notified (dedup key = `(child_sid, turn_id)`). Lifetime = ONE dispatched
/// task: armed by a dispatch, spent by that task's turn boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationWatch {
    /// The sid of the session to notify when a watched turn completes (the
    /// dispatcher's principal — usually, but not necessarily, the spawner).
    pub parent_sid: String,
    /// When completion delivers a notification turn to `parent_sid` — see
    /// [`NotifyMode`]. Deserializes the pre-v0.9.5 boolean form too.
    pub notify: NotifyMode,
    /// Optional short label carried into the notification / visualization
    /// (ledger-only — NEVER concatenated into any dispatched prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The turn id of the most recent dispatch (diagnostic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_turn: Option<String>,
    /// ISO-8601 dispatch time (diagnostic).
    pub dispatched_at: String,
    /// Completed child turns already notified — the at-least-once dedup set. A
    /// reconcile after a daemon restart delivers only the completed turns NOT
    /// already in here.
    #[serde(default)]
    pub notified_turns: Vec<String>,
}

impl DelegationWatch {
    /// A fresh watch armed by a dispatch (no turns notified yet).
    pub fn armed(
        parent_sid: impl Into<String>,
        notify: NotifyMode,
        title: Option<String>,
        dispatched_turn: Option<String>,
    ) -> Self {
        Self {
            parent_sid: parent_sid.into(),
            notify,
            title,
            dispatched_turn,
            dispatched_at: chrono::Utc::now().to_rfc3339(),
            notified_turns: Vec::new(),
        }
    }
}

/// `<project>/.ccteam/chat/<child_sid>/delegation.json`.
pub fn delegation_path(project_dir: &Path, child_sid: &str) -> PathBuf {
    chat_dir(project_dir, child_sid).join("delegation.json")
}

/// Read the watch for `child_sid` (best-effort: missing / unparseable → `None`).
pub fn read_delegation_watch(project_dir: &Path, child_sid: &str) -> Option<DelegationWatch> {
    let raw = std::fs::read_to_string(delegation_path(project_dir, child_sid)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Durably write the watch for `child_sid` (tmp+fsync+rename — same discipline
/// as `meta.json`).
pub fn write_delegation_watch(
    project_dir: &Path,
    child_sid: &str,
    watch: &DelegationWatch,
) -> Result<()> {
    let path = delegation_path(project_dir, child_sid);
    std::fs::create_dir_all(path.parent().expect("path has parent"))?;
    atomic_write_durable(&path, serde_json::to_string_pretty(watch)?.as_bytes())
}

/// Drop the watch for `child_sid` (best-effort — used when the parent session
/// no longer exists so the watch can never fire).
pub fn remove_delegation_watch(project_dir: &Path, child_sid: &str) {
    let _ = std::fs::remove_file(delegation_path(project_dir, child_sid));
}

/// Scan `<project>/.ccteam/chat/*/delegation.json` and return every parseable
/// `(child_sid, watch)` pair (child_sid = the chat dir name). Used by the
/// daemon-startup reconcile.
pub fn scan_delegation_watches(project_dir: &Path) -> Vec<(String, DelegationWatch)> {
    let chat_base = project_dir.join(".ccteam").join("chat");
    let Ok(entries) = std::fs::read_dir(&chat_base) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let Some(child_sid) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = entry.path().join("delegation.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(watch) = serde_json::from_str::<DelegationWatch>(&raw) {
            out.push((child_sid, watch));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let watch = DelegationWatch::armed(
            "s1",
            NotifyMode::Final,
            Some("research".into()),
            Some("s2-3".into()),
        );
        write_delegation_watch(tmp.path(), "s2", &watch).unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").expect("watch reads back");
        assert_eq!(back.parent_sid, "s1");
        assert_eq!(back.notify, NotifyMode::Final);
        assert_eq!(back.title.as_deref(), Some("research"));
        assert!(back.notified_turns.is_empty());
    }

    #[test]
    fn notify_mode_wire_forms() {
        // String forms round-trip; the pre-v0.9.5 boolean form still parses.
        for (raw, want) in [
            ("\"final\"", NotifyMode::Final),
            ("\"brief\"", NotifyMode::Brief),
            ("\"off\"", NotifyMode::Off),
            ("true", NotifyMode::Final),
            ("false", NotifyMode::Off),
        ] {
            let got: NotifyMode = serde_json::from_str(raw).unwrap();
            assert_eq!(got, want, "parsing {raw}");
        }
        assert_eq!(
            serde_json::to_string(&NotifyMode::Brief).unwrap(),
            "\"brief\""
        );
        assert!(serde_json::from_str::<NotifyMode>("\"sometimes\"").is_err());
        assert!(serde_json::from_str::<NotifyMode>("3").is_err());
    }

    /// The retired `all` is refused on the WIRE with the way out spelled…
    #[test]
    fn notify_all_is_refused_on_the_wire() {
        let err = NotifyMode::parse_value(&serde_json::json!("all")).unwrap_err();
        assert_eq!(err, "notify `all` was removed; use final");
        // …and it is not a value this server will ever emit again.
        for mode in [NotifyMode::Final, NotifyMode::Brief, NotifyMode::Off] {
            assert_ne!(mode.as_str(), "all");
        }
    }

    /// …but a watch persisted before the removal still LOADS: dropping it would
    /// silently lose that child's completion notification across a restart.
    #[test]
    fn persisted_all_watch_degrades_to_final() {
        let tmp = TempDir::new().unwrap();
        let path = delegation_path(tmp.path(), "s2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"parent_sid":"s1","notify":"all","dispatched_at":"2026-01-01T00:00:00Z","notified_turns":[]}"#,
        )
        .unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").expect("retired mode still loads");
        assert_eq!(back.notify, NotifyMode::Final);
    }

    #[test]
    fn overwrite_updates_watch() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", NotifyMode::Final, None, None),
        )
        .unwrap();
        // A later dispatch from a different parent overwrites (one pending parent).
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s9", NotifyMode::Off, None, None),
        )
        .unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").unwrap();
        assert_eq!(back.parent_sid, "s9");
        assert_eq!(back.notify, NotifyMode::Off);
    }

    #[test]
    fn scan_finds_all_watches() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", NotifyMode::Final, None, None),
        )
        .unwrap();
        write_delegation_watch(
            tmp.path(),
            "s3",
            &DelegationWatch::armed("s1", NotifyMode::Final, None, None),
        )
        .unwrap();
        let mut found = scan_delegation_watches(tmp.path());
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "s2");
        assert_eq!(found[1].0, "s3");
    }

    #[test]
    fn read_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        assert!(read_delegation_watch(tmp.path(), "nope").is_none());
    }

    #[test]
    fn remove_drops_watch() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", NotifyMode::Final, None, None),
        )
        .unwrap();
        remove_delegation_watch(tmp.path(), "s2");
        assert!(read_delegation_watch(tmp.path(), "s2").is_none());
    }

    #[test]
    fn pre_v095_boolean_watch_still_parses() {
        // An on-disk delegation.json written before NotifyMode existed.
        let tmp = TempDir::new().unwrap();
        let path = delegation_path(tmp.path(), "s2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"parent_sid":"s1","notify":true,"dispatched_at":"2026-01-01T00:00:00Z","notified_turns":["s2-1"]}"#,
        )
        .unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").expect("boolean watch parses");
        assert_eq!(back.notify, NotifyMode::Final);
        assert_eq!(back.notified_turns, vec!["s2-1".to_string()]);
    }
}
