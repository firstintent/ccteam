//! v0.8.8 B5 — 共享的「sid → per-session pane 名」解析。
//!
//! F1 之后【根本没有项目级 pane】:每个会话的 pane 是 per-sid 命名的 ——
//! claude 走 `ccteam-chat-{slug}-{sid}`([`ccteam_harness::chat_session_name`]),
//! codex 走 `ccteam-{slug}-{sid}`([`ccteam_harness::codex_chat_session_name`])。
//! 所以 web 终端(`pty_ws`)与只读快照(`pane_snapshot`)都必须经 live
//! gateway 的 [`Gateway::session_resolve`](ccteam_im::gateway::Gateway::session_resolve)
//! 把 `sid` 解析成 `{role, vendor, project, …}`,再按 vendor 构 pane 名 ——
//! 两处共用本模块的 helper,避免 vendor 分支两份漂移(B5 之前 `pty_ws` 与
//! `pane_snapshot` 各自退回项目级 `state.tmux_session`,F1 后那个名字根本不
//! 对应任何活 pane → subscribe 空/秒断)。
//!
//! 锁纪律:沿用 [`super::sessions_api::handle_session_history`] 的范式 ——
//! `session_resolve` 是同步(不持 `.await`)、只 clone 标量,所以在 gateway
//! guard 内调用、立即 drop guard;构 pane 名是纯字符串运算,在锁外做。
//!
//! 无 gateway(standalone「internal web」,[`AppState::gateway`] = `None`)→
//! [`PaneResolveError::NoGateway`](进而 503),与 session 资源 API 的
//! no-gateway 契约一致;sid 不在 gateway → [`PaneResolveError::Unknown`](404)。

use ccteam_im::gateway::SessionResolve;

use crate::state::AppState;

/// `sid → pane` 解析失败的两种缘由,调用方按需映射到 HTTP 状态码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneResolveError {
    /// 无 live gateway(standalone internal-web)。无会话表可查 → 503。
    NoGateway,
    /// gateway 在,但没有这个 sid 的会话 → 404。
    Unknown,
}

/// 按 vendor 构 per-session pane 名的【纯函数】(无 gateway 依赖,便于单测)。
///
/// - `claude` → [`ccteam_harness::chat_session_name`](`ccteam-chat-{slug}-{sid}`)
/// - 其余(含 `codex`)→ [`ccteam_harness::codex_chat_session_name`](`ccteam-{slug}-{sid}`)
///
/// vendor 串来自 [`SessionResolve::vendor`](gateway 的 `vendor_str`,小写
/// `"claude"`/`"codex"`)。未知 vendor 退到 codex 命名是最保守选择(codex
/// 名 = 不带 `-chat-` 段的项目前缀派生),但当前只有 claude/codex 两种 vendor。
pub(crate) fn pane_name_for_vendor(vendor: &str, slug: &str, sid: &str) -> String {
    match vendor.trim().to_ascii_lowercase().as_str() {
        "claude" => ccteam_harness::chat_session_name(slug, sid),
        // codex(及未知 vendor 兜底)走 codex 容器 pane 命名权威。
        _ => ccteam_harness::codex_chat_session_name(slug, sid),
    }
}

/// 经 live gateway 把 `sid` 解析成 `(pane_name, SessionResolve)`。
///
/// 成功时 pane 名按 `resolved.vendor` 分支(见 [`pane_name_for_vendor`]),slug
/// 用会话自己的 `resolved.project`(而非 URL path 里的 slug —— path slug 仅是
/// 寻址命名空间,真值以 gateway 会话记录为准,和 session 资源 API 一致)。
///
/// 锁只在 `session_resolve` 期间短暂持有(同步、无 `.await`),随即 drop。
pub(crate) async fn resolve_session_pane(
    app: &AppState,
    sid: &str,
) -> Result<(String, SessionResolve), PaneResolveError> {
    let Some(gw) = app.gateway.as_ref() else {
        return Err(PaneResolveError::NoGateway);
    };
    // 锁内 clone 标量、立即 drop guard(范式同 handle_session_history)。
    let resolved = {
        let guard = gw.lock().await;
        guard.session_resolve(sid)
    };
    let Some(resolved) = resolved else {
        return Err(PaneResolveError::Unknown);
    };
    let pane = pane_name_for_vendor(&resolved.vendor, &resolved.project, &resolved.sid);
    Ok((pane, resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    // v0.8.8 B5 — 纯名字解析单测:断言 helper 对 claude/codex 给出与各 vendor
    // pane 命名权威逐字节一致的名字(不需要真 gateway / 真 pane)。这是 BUG-6
    // 的核心:F1 后名字必须按 sid + vendor 走,不能退项目级。
    #[test]
    fn pane_name_for_vendor_claude_uses_chat_prefix() {
        // claude → ccteam-chat-{slug}-{sid}(与 chat_session_name 同源)。
        assert_eq!(
            pane_name_for_vendor("claude", "dev-proj", "s2"),
            ccteam_harness::chat_session_name("dev-proj", "s2"),
        );
        assert_eq!(
            pane_name_for_vendor("claude", "dev-proj", "s2"),
            "ccteam-chat-dev-proj-s2",
        );
        // 大小写不敏感(SessionResolve.vendor 已是小写,但 helper 自防御)。
        assert_eq!(
            pane_name_for_vendor("CLAUDE", "demo", "s1"),
            "ccteam-chat-demo-s1",
        );
    }

    #[test]
    fn pane_name_for_vendor_codex_uses_project_prefix() {
        // codex → ccteam-{slug}-{sid}(与 codex_chat_session_name 同源)。
        assert_eq!(
            pane_name_for_vendor("codex", "dev-proj", "s2"),
            ccteam_harness::codex_chat_session_name("dev-proj", "s2"),
        );
        assert_eq!(
            pane_name_for_vendor("codex", "dev-proj", "s2"),
            "ccteam-dev-proj-s2",
        );
    }

    #[test]
    fn pane_name_for_vendor_unknown_falls_back_to_codex_naming() {
        // 未知 vendor 兜底 = codex 命名(保守:不带 -chat- 段)。
        assert_eq!(
            pane_name_for_vendor("gemini", "demo", "s5"),
            ccteam_harness::codex_chat_session_name("demo", "s5"),
        );
    }
}
