//! Squad teammate roster block for chat-squad personas.
//!
//! When `ccteam-creator` installs a persona body into
//! `<project>/.claude/agents/<role>.md` (Phase 5.4), it appends a
//! "Squad teammates" block listing the other bots in the same workflow
//! so each bot reads its squad roster on every session start. Without
//! this awareness, a bot in a chat-squad replies to "@dev can you
//! help here" by trying to spawn a Task subagent named `dev` instead of
//! `@`-mentioning the real `@dev` bot (a sibling long-running tmux
//! session reachable via the daemon-internal mpsc cross-bot channel).
//!
//! The render layer is pure data → string so the LLM-driven skill can
//! mirror the exact byte-for-byte output in its `Write` call, and the
//! Rust side stays unit-testable.

/// One teammate entry rendered into the roster block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeammateInfo {
    /// Effective IM handle (the bot's `chat_handle`, falling back to
    /// `role` when unset). No leading `@`.
    pub handle: String,
    /// Workflow role (e.g. `"helper"`, `"critic"`).
    pub role: String,
    /// Human-readable persona label (e.g. `"explorer"`,
    /// `"architect"`). Empty string OK — the renderer skips the label
    /// suffix when blank.
    pub persona_label: String,
}

/// Render the Chinese-language squad roster block. Returns the empty
/// string when `teammates` is empty (single-bot pocket preset — no
/// block to append).
///
/// `self_handle` is documented only via exclusion: callers must
/// pre-filter `teammates` to remove the calling bot's own entry; the
/// renderer does **not** drop entries matching `self_handle`. This
/// keeps the helper's contract small and lets the caller decide
/// whether handle comparison is case-sensitive (it is, in practice;
/// the registry's collision-suffix scheme guarantees uniqueness).
pub fn render_squad_roster_zh(
    workflow_slug: &str,
    self_handle: &str,
    teammates: &[TeammateInfo],
) -> String {
    if teammates.is_empty() {
        return String::new();
    }
    let _ = self_handle; // see doc-comment — filtering is the caller's job.
    let mut out = String::new();
    out.push_str("\n---\n\n");
    out.push_str("## 你的队友 (Squad teammates)\n\n");
    out.push_str(&format!(
        "你是 `{}` chat-squad 工作流中的一员。其他兄弟 bot 在同一个 IM 群里,你可以 `@<handle>` 直接发起 bot-to-bot 协作(hop_limit=3,防循环):\n\n",
        workflow_slug
    ));
    for t in teammates {
        out.push_str(&format!(
            "- **@{}** — {}{}\n",
            t.handle,
            t.role,
            render_label_suffix_zh(&t.persona_label),
        ));
    }
    out.push('\n');
    out.push_str(
        "@ 他们能让消息直接进对方 inbox(不走 IM 端 round-trip)。\
         **他们不是你 spawn 出来的 Task subagent,而是独立长跑的兄弟进程** — \
         别试图调 Task 工具去\"扮演\" @<handle>,直接 @ 它就好。\n\n",
    );
    out.push_str("只在需要队友视角时 @,3 行 bug 不需要拉团队会议。\n");
    out
}

/// English-language sibling of [`render_squad_roster_zh`]. Same
/// contract: empty `teammates` returns empty string; `self_handle` is
/// the caller's filter responsibility.
pub fn render_squad_roster_en(
    workflow_slug: &str,
    self_handle: &str,
    teammates: &[TeammateInfo],
) -> String {
    if teammates.is_empty() {
        return String::new();
    }
    let _ = self_handle;
    let mut out = String::new();
    out.push_str("\n---\n\n");
    out.push_str("## Squad teammates\n\n");
    out.push_str(&format!(
        "You are one of several bots in the `{}` chat-squad workflow. \
         Your siblings sit in the same IM room; `@<handle>` reaches \
         them directly for bot-to-bot collaboration (hop_limit=3, \
         cycle-safe):\n\n",
        workflow_slug
    ));
    for t in teammates {
        out.push_str(&format!(
            "- **@{}** — {}{}\n",
            t.handle,
            t.role,
            render_label_suffix_en(&t.persona_label),
        ));
    }
    out.push('\n');
    out.push_str(
        "`@`-mentioning a sibling routes the message into their inbox \
         directly (no IM-side round-trip). **Siblings are NOT Task \
         subagents you spawn** — they are independent long-running \
         processes. Do not call the Task tool to \"play\" `@<handle>`; \
         just `@` them.\n\n",
    );
    out.push_str("Only `@` a teammate when you actually need their perspective. A 3-line bug fix does not need a team meeting.\n");
    out
}

fn render_label_suffix_zh(label: &str) -> String {
    if label.trim().is_empty() {
        String::new()
    } else {
        format!(" ({})", label)
    }
}

fn render_label_suffix_en(label: &str) -> String {
    if label.trim().is_empty() {
        String::new()
    } else {
        format!(" ({})", label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(handle: &str, role: &str, persona_label: &str) -> TeammateInfo {
        TeammateInfo {
            handle: handle.into(),
            role: role.into(),
            persona_label: persona_label.into(),
        }
    }

    #[test]
    fn zh_empty_teammates_returns_empty_string() {
        let out = render_squad_roster_zh("solo", "alice", &[]);
        assert!(
            out.is_empty(),
            "single-bot pocket preset must not render the block"
        );
    }

    #[test]
    fn en_empty_teammates_returns_empty_string() {
        let out = render_squad_roster_en("solo", "alice", &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn zh_lists_each_teammate_with_handle_role_label() {
        let mates = vec![
            t("galileo", "explorer", "代码探索"),
            t("newton", "architect", "高层设计"),
        ];
        let out = render_squad_roster_zh("reno", "cx", &mates);
        assert!(
            out.starts_with("\n---\n"),
            "block must begin with separator"
        );
        assert!(out.contains("`reno`"));
        assert!(out.contains("@galileo"));
        assert!(out.contains("explorer"));
        assert!(out.contains("代码探索"));
        assert!(out.contains("@newton"));
        assert!(out.contains("architect"));
        assert!(out.contains("高层设计"));
        // Routing guidance must be present.
        assert!(out.contains("hop_limit=3"));
        assert!(out.contains("Task subagent"));
    }

    #[test]
    fn en_lists_each_teammate_with_handle_role_label() {
        let mates = vec![
            t("galileo", "explorer", "code exploration"),
            t("newton", "architect", "high-level design"),
        ];
        let out = render_squad_roster_en("reno", "cx", &mates);
        assert!(out.starts_with("\n---\n"));
        assert!(out.contains("`reno`"));
        assert!(out.contains("@galileo"));
        assert!(out.contains("explorer"));
        assert!(out.contains("code exploration"));
        assert!(out.contains("@newton"));
        assert!(out.contains("architect"));
        assert!(out.contains("hop_limit=3"));
        assert!(out.contains("Task subagents"));
    }

    #[test]
    fn label_suffix_omitted_when_persona_label_blank() {
        let mates = vec![t("galileo", "explorer", "")];
        let out_zh = render_squad_roster_zh("reno", "cx", &mates);
        assert!(out_zh.contains("- **@galileo** — explorer\n"));
        assert!(!out_zh.contains("- **@galileo** — explorer ("));
        let out_en = render_squad_roster_en("reno", "cx", &mates);
        assert!(out_en.contains("- **@galileo** — explorer\n"));
    }

    #[test]
    fn caller_is_responsible_for_self_exclusion() {
        // Renderer does NOT filter `self_handle` out — that's the
        // caller's job. This test pins the contract so the skill knows
        // not to rely on the renderer for the filter.
        let mates = vec![
            t("cx", "critic", "code-critic"),
            t("galileo", "explorer", "tech-helper"),
        ];
        let out = render_squad_roster_zh("reno", "cx", &mates);
        assert!(out.contains("@cx"), "renderer must NOT drop self entries");
        assert!(out.contains("@galileo"));
    }

    #[test]
    fn three_bot_squad_lists_other_two() {
        // Caller-side simulation: a 3-bot squad rendering "cx"'s
        // roster should pass exactly the other two as `teammates`.
        let all = [
            ("cx", "critic", "code-critic"),
            ("galileo", "explorer", "tech-helper"),
            ("newton", "architect", "project-lead"),
        ];
        let self_handle = "cx";
        let mates: Vec<_> = all
            .iter()
            .filter(|(h, _, _)| *h != self_handle)
            .map(|(h, r, l)| t(h, r, l))
            .collect();
        let out = render_squad_roster_zh("reno", self_handle, &mates);
        assert!(
            !out.contains("@cx"),
            "self entry must be filtered by caller"
        );
        assert!(out.contains("@galileo"));
        assert!(out.contains("@newton"));
    }

    #[test]
    fn no_leading_at_in_handle_input() {
        // The renderer prepends `@` itself — callers must pass clean
        // handles (no leading `@`). This mirrors `BotRegistration::
        // effective_handle()` which never emits the `@`.
        let mates = vec![t("galileo", "explorer", "")];
        let out = render_squad_roster_zh("reno", "cx", &mates);
        assert!(out.contains("@galileo"));
        assert!(!out.contains("@@galileo"));
    }
}
