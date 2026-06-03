//! V0.8.4 P1 (B1) — live progress folding for IM status messages.
//!
//! A turn's tool / reasoning activity is folded into **one** editable
//! "status" message (mirrors the official telegram plugin UX) rather than
//! one ping per step. Borrows claude-code's `GroupedToolUseMessage` /
//! `CollapsedReadSearchGroup` (group + count by category) and
//! `truncateForPreview` (phone-sized arg previews).
//!
//! Granularity = **per completed step**. The transcript only lands a row
//! when a step finishes; there is no sub-second token stream to follow,
//! so the fold reacts to whole [`ThreadEvent`]s, never tokens.
//!
//! The shape Claude emits (verified against `transcript_tail`): a tool is
//! `ItemStarted{ToolCall{name,args=input}}` then
//! `ItemCompleted{ToolCall{name,args=result}}` (same `item.id`);
//! reasoning is `ItemUpdated{Reasoning}`; the answer is
//! `ItemCompleted{AgentMessage}`. Codex additionally emits
//! `CommandExecution` / `FileChange` / `WebSearch` as `ItemCompleted`.
//! [`ProgressFold`] handles all of them and de-dups a tool's
//! start/complete pair by `item.id` so it counts once.

use std::collections::HashSet;

use ccteam_harness::{ThreadEvent, ThreadItemDetails};
use serde_json::Value;

/// Phone-sized cap (chars) for a tool's argument preview.
pub const PREVIEW_MAX: usize = 200;
/// How many recent steps to expand below the folded summary.
const MAX_DETAIL_LINES: usize = 2;
/// Hard cap on rendered status lines (summary + details).
const MAX_LINES: usize = 8;

/// A folded category: a stable emoji + short label that several raw tool
/// names collapse into (e.g. `Read`/`Grep`/`Glob` → `read`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Category {
    emoji: &'static str,
    label: &'static str,
}

const CAT_READ: Category = Category {
    emoji: "📖",
    label: "read",
};
const CAT_BASH: Category = Category {
    emoji: "🔧",
    label: "bash",
};
const CAT_EDIT: Category = Category {
    emoji: "✏️",
    label: "edit",
};
const CAT_WEB: Category = Category {
    emoji: "🔎",
    label: "web",
};
const CAT_TASK: Category = Category {
    emoji: "🤖",
    label: "task",
};
const CAT_TODO: Category = Category {
    emoji: "📝",
    label: "todo",
};

/// Map a raw tool name (Claude `ToolCall.name`, or a Codex tool) to a
/// folded [`Category`]. Unknown names fold under the wrench with the raw
/// (lowercased) name so nothing is silently dropped.
fn tool_category(name: &str) -> Category {
    match name {
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => CAT_READ,
        "Bash" | "BashOutput" | "KillBash" | "KillShell" => CAT_BASH,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => CAT_EDIT,
        "WebSearch" | "WebFetch" => CAT_WEB,
        "Task" => CAT_TASK,
        "TodoWrite" => CAT_TODO,
        _ => Category {
            emoji: "🔧",
            // Leak the raw name (it is `&str` with a non-static lifetime
            // in general, but tool names from the adapter live long
            // enough; we copy into the count label as an owned String, so
            // the &'static here is only the fallback emoji bucket).
            label: "tool",
        },
    }
}

/// Collapse whitespace + truncate to [`PREVIEW_MAX`] chars with an
/// ellipsis. Mirrors claude-code `truncateForPreview` — keeps a
/// `Write(5KB)` / multi-line `Bash(...)` from flooding the status line.
pub fn truncate_for_preview(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(PREVIEW_MAX - 1).collect();
    out.push('…');
    out
}

/// Pull a human-meaningful field out of a tool's JSON args for the
/// preview line (command / path / pattern / query), falling back to a
/// compact JSON rendering.
fn preview_args(args: &Value) -> String {
    let picked = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
    ]
    .iter()
    .find_map(|k| args.get(*k).and_then(Value::as_str));
    let raw = match picked {
        Some(s) => s.to_string(),
        None => match args {
            Value::Null => String::new(),
            other => other.to_string(),
        },
    };
    truncate_for_preview(&raw)
}

/// One folded count bucket, kept in first-seen order for stable render.
struct Bucket {
    emoji: String,
    label: String,
    count: usize,
}

/// Rolling fold of a single status epoch (one turn's progress). Feed it
/// [`ThreadEvent`]s with [`apply`](Self::apply); render the current
/// status text with [`render`](Self::render).
#[derive(Default)]
pub struct ProgressFold {
    buckets: Vec<Bucket>,
    seen_ids: HashSet<String>,
    /// Most recent step previews (newest last), capped at
    /// [`MAX_DETAIL_LINES`].
    recent: Vec<String>,
    thinking: bool,
    /// Codex streamed an `ItemUpdated{AgentMessage}` delta (drafting the
    /// reply) — shown as a head state, never sent as its own answer.
    drafting: bool,
    done: bool,
    tool_total: usize,
    file_total: usize,
}

impl ProgressFold {
    /// Fresh, empty fold for a new status epoch (one per turn).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything worth showing has accumulated (so the pump knows
    /// whether to bother sending / finalizing a status message).
    pub fn has_activity(&self) -> bool {
        !self.buckets.is_empty() || self.thinking || self.drafting
    }

    /// Whether the epoch has been finalized (renders the `✅ done` line).
    pub fn done(&self) -> bool {
        self.done
    }

    /// Mark the epoch finished (renders the `✅ done · …` summary).
    pub fn mark_done(&mut self) {
        self.done = true;
    }

    fn bump(&mut self, cat: Category, raw_name: &str) {
        // Unknown tools fold under the wrench but keep their own label.
        let label: String = if cat.label == "tool" {
            raw_name.to_lowercase()
        } else {
            cat.label.to_string()
        };
        if let Some(b) = self.buckets.iter_mut().find(|b| b.label == label) {
            b.count += 1;
        } else {
            self.buckets.push(Bucket {
                emoji: cat.emoji.to_string(),
                label,
                count: 1,
            });
        }
    }

    fn push_recent(&mut self, line: String) {
        self.recent.push(line);
        if self.recent.len() > MAX_DETAIL_LINES {
            self.recent.remove(0);
        }
    }

    /// Count a tool once (de-duped by `item.id`), bump its category, and
    /// record a preview line. Returns whether state changed.
    fn count_tool(&mut self, id: &str, cat: Category, raw_name: &str, preview: String) -> bool {
        if !self.seen_ids.insert(id.to_string()) {
            return false; // start/complete pair — already counted
        }
        self.bump(cat, raw_name);
        self.tool_total += 1;
        if cat == CAT_EDIT {
            self.file_total += 1;
        }
        self.push_recent(format!("{} {preview}", cat.emoji));
        true
    }

    /// Fold one event into the status. Returns `true` if the rendered
    /// status would change (the caller marks the status dirty). Answer
    /// (`ItemCompleted{AgentMessage}`) and lifecycle events are *not*
    /// progress and return `false` — the pump routes those separately.
    pub fn apply(&mut self, evt: &ThreadEvent) -> bool {
        match evt {
            ThreadEvent::ItemStarted { item } | ThreadEvent::ItemCompleted { item } => {
                match &item.details {
                    ThreadItemDetails::ToolCall { name, args } => {
                        let preview = format!("{name}({})", preview_args(args));
                        self.count_tool(&item.id, tool_category(name), name, preview)
                    }
                    ThreadItemDetails::CommandExecution { cmd, .. } => {
                        let preview = format!("$ {}", truncate_for_preview(cmd));
                        self.count_tool(&item.id, CAT_BASH, "bash", preview)
                    }
                    ThreadItemDetails::FileChange { path, kind } => {
                        let preview = format!("{kind} {}", path.display());
                        self.count_tool(&item.id, CAT_EDIT, "edit", preview)
                    }
                    ThreadItemDetails::WebSearch { query } => {
                        let preview = truncate_for_preview(query);
                        self.count_tool(&item.id, CAT_WEB, "web", preview)
                    }
                    ThreadItemDetails::Reasoning(_) => self.set_thinking(),
                    ThreadItemDetails::AgentMessage(_) | ThreadItemDetails::Error(_) => false,
                }
            }
            ThreadEvent::ItemUpdated { item } => match &item.details {
                ThreadItemDetails::Reasoning(_) => self.set_thinking(),
                // Codex streaming delta — drafting, not an answer.
                ThreadItemDetails::AgentMessage(_) => {
                    if self.drafting {
                        false
                    } else {
                        self.drafting = true;
                        true
                    }
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn set_thinking(&mut self) -> bool {
        if self.thinking {
            false
        } else {
            self.thinking = true;
            true
        }
    }

    fn counts_summary(&self) -> String {
        self.buckets
            .iter()
            .map(|b| format!("{} {} ×{}", b.emoji, b.label, b.count))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn head(&self) -> &'static str {
        if self.drafting {
            "✍️ drafting…"
        } else if self.buckets.is_empty() && self.thinking {
            "💭 thinking…"
        } else {
            "⏳ working…"
        }
    }

    /// Render the current status text (≤ [`MAX_LINES`] lines).
    pub fn render(&self) -> String {
        if self.done {
            return format!(
                "✅ done · {} tools · {} files",
                self.tool_total, self.file_total
            );
        }
        let mut header = self.head().to_string();
        let summary = self.counts_summary();
        if !summary.is_empty() {
            header.push_str(" · ");
            header.push_str(&summary);
        }
        let mut lines = vec![header];
        for d in &self.recent {
            lines.push(format!("  {d}"));
        }
        lines.truncate(MAX_LINES);
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{ThreadItem, ThreadItemDetails};
    use serde_json::json;

    fn started_tool(id: &str, name: &str, args: Value) -> ThreadEvent {
        ThreadEvent::ItemStarted {
            item: ThreadItem {
                id: id.to_string(),
                details: ThreadItemDetails::ToolCall {
                    name: name.to_string(),
                    args,
                },
            },
        }
    }

    fn completed_tool(id: &str, name: &str) -> ThreadEvent {
        ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: id.to_string(),
                details: ThreadItemDetails::ToolCall {
                    name: name.to_string(),
                    args: json!("result"),
                },
            },
        }
    }

    #[test]
    fn folds_and_counts_by_category() {
        let mut f = ProgressFold::new();
        assert!(f.apply(&started_tool("t1", "Read", json!({"file_path": "/a"}))));
        assert!(f.apply(&started_tool("t2", "Read", json!({"file_path": "/b"}))));
        assert!(f.apply(&started_tool("t3", "Bash", json!({"command": "ls"}))));
        let r = f.render();
        assert!(r.contains("📖 read ×2"), "got: {r}");
        assert!(r.contains("🔧 bash ×1"), "got: {r}");
        assert!(r.starts_with("⏳ working…"), "got: {r}");
    }

    #[test]
    fn dedups_tool_start_and_complete_by_id() {
        let mut f = ProgressFold::new();
        assert!(f.apply(&started_tool("t1", "Bash", json!({"command": "ls"}))));
        // The matching completion carries the same id → must NOT recount.
        assert!(!f.apply(&completed_tool("t1", "Bash")));
        assert!(f.render().contains("🔧 bash ×1"));
    }

    #[test]
    fn truncates_arg_preview_phone_sized() {
        let mut f = ProgressFold::new();
        let long = "x".repeat(500);
        f.apply(&started_tool("t1", "Bash", json!({ "command": long })));
        let r = f.render();
        // The arg preview is capped to PREVIEW_MAX chars; the whole line
        // adds only the `🔧 Bash(…)` wrapper + indent, so it stays far
        // below the untruncated 500.
        let detail = r.lines().last().unwrap();
        assert!(
            detail.chars().count() < PREVIEW_MAX + 32,
            "detail not truncated: {} chars",
            detail.chars().count()
        );
        assert!(detail.contains('…'));
    }

    #[test]
    fn reasoning_sets_thinking_without_counting() {
        let mut f = ProgressFold::new();
        let ev = ThreadEvent::ItemUpdated {
            item: ThreadItem {
                id: "r1".into(),
                details: ThreadItemDetails::Reasoning("hmm".into()),
            },
        };
        assert!(f.apply(&ev));
        assert!(!f.apply(&ev)); // second reasoning is a no-op
        assert!(f.render().starts_with("💭 thinking…"));
        assert!(f.has_activity());
    }

    #[test]
    fn codex_command_and_file_change_fold() {
        let mut f = ProgressFold::new();
        f.apply(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "c1".into(),
                details: ThreadItemDetails::CommandExecution {
                    cmd: "cargo build".into(),
                    status: "ok".into(),
                },
            },
        });
        f.apply(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "fc1".into(),
                details: ThreadItemDetails::FileChange {
                    path: "/src/lib.rs".into(),
                    kind: "modified".into(),
                },
            },
        });
        let r = f.render();
        assert!(r.contains("🔧 bash ×1"), "got: {r}");
        assert!(r.contains("✏️ edit ×1"), "got: {r}");
    }

    #[test]
    fn done_renders_summary_counts() {
        let mut f = ProgressFold::new();
        f.apply(&started_tool("t1", "Bash", json!({"command": "ls"})));
        f.apply(&started_tool("t2", "Edit", json!({"file_path": "/a"})));
        f.mark_done();
        assert_eq!(f.render(), "✅ done · 2 tools · 1 files");
    }

    #[test]
    fn answer_and_lifecycle_events_are_not_progress() {
        let mut f = ProgressFold::new();
        let answer = ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "a1".into(),
                details: ThreadItemDetails::AgentMessage("final".into()),
            },
        };
        assert!(!f.apply(&answer));
        assert!(!f.has_activity());
    }

    #[test]
    fn line_count_capped() {
        let mut f = ProgressFold::new();
        for i in 0..30 {
            f.apply(&started_tool(
                &format!("t{i}"),
                "Bash",
                json!({"command": "ls"}),
            ));
        }
        assert!(f.render().lines().count() <= MAX_LINES);
    }
}
