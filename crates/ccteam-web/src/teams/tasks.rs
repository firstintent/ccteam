//! V0.5.0 F96 — Agent Teams task list parser.
//!
//! `<claude_home>/tasks/<team>/*.json` — one file per task. The
//! Anthropic schema isn't formally documented in the V0.5.0 PRD beyond
//! the `status` state machine (pending / in_progress / completed)
//! and the F95 emit shape `{ team_name, task_id, title, assignee?,
//! dependencies[] }`. We accept a tolerant superset so live host
//! variation doesn't 500 the endpoint:
//!
//! - `title` OR `subject` for the headline.
//! - `owner` OR `assignee` for the assignee.
//! - `dependencies[]` OR `blockedBy[]` for the dependency list.
//! - `id` from the JSON, else filename stem.
//! - `status` defaults to `"pending"` when missing.
//!
//! Skipped: `.highwatermark`, `.lock`, anything not ending `.json`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::tasks_root;

/// One Kanban card. Shape is wire-stable; the SPA matches it 1:1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    /// "pending" | "in_progress" | "completed" | other host status —
    /// the SPA buckets unknown values under Pending.
    pub status: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// RFC3339 / ISO-8601 / epoch — pass-through verbatim so the SPA
    /// formats with its own locale.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// Per-status counts surfaced on the team summary endpoint.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct TaskCounts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl TaskCounts {
    pub fn from(tasks: &[TaskView]) -> Self {
        let mut c = TaskCounts::default();
        for t in tasks {
            match t.status.as_str() {
                "in_progress" => c.in_progress += 1,
                "completed" => c.completed += 1,
                _ => c.pending += 1,
            }
        }
        c
    }
}

/// Read every `<claude_home>/tasks/<team>/*.json`. Missing dir → empty.
pub fn load_tasks(claude_home: &Path, team: &str) -> Result<Vec<TaskView>> {
    let dir = tasks_root(claude_home, team);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Skip Anthropic's bookkeeping files (`.highwatermark`,
        // `.lock`) and anything not a `.json` file.
        if name.starts_with('.') {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let body = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "skip task: read failed");
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "skip task: parse failed");
                continue;
            }
        };
        let task = task_from_value(&value, &path);
        out.push(task);
    }
    // Stable-sort by creation time then id so the Kanban renders
    // deterministically (and tests don't flake on `fs::read_dir`
    // ordering).
    out.sort_by(|a, b| match a.created_at.cmp(&b.created_at) {
        std::cmp::Ordering::Equal => a.id.cmp(&b.id),
        other => other,
    });
    Ok(out)
}

fn task_from_value(v: &serde_json::Value, path: &Path) -> TaskView {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });
    let title = v
        .get("title")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("subject").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| id.clone());
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "pending".to_string());
    let assignee = v
        .get("assignee")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("owner").and_then(|x| x.as_str()))
        .map(|s| s.to_string());
    let description = v
        .get("description")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let dependencies = v
        .get("dependencies")
        .or_else(|| v.get("blockedBy"))
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let created_at = v
        .get("createdAt")
        .or_else(|| v.get("created_at"))
        .map(stringify_ts);
    let completed_at = v
        .get("completedAt")
        .or_else(|| v.get("completed_at"))
        .map(stringify_ts);
    TaskView {
        id,
        title,
        status,
        assignee,
        description,
        dependencies,
        created_at,
        completed_at,
    }
}

fn stringify_ts(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}
