use serde::{Deserialize, Serialize};

use crate::{ContextUsage, UnifiedTokenUsage};

/// Per-turn session facts collected at the gateway's single assistant-row
/// boundary.  The values are cumulative for the session, except `context`
/// which is the vendor-reported post-turn snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnStatus {
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    pub turn: u64,
    pub cost_usd: Option<f64>,
    pub tokens_total: Option<u64>,
}

/// Identity is catalog metadata rather than transcript data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusIdentity<'a> {
    pub slug: &'a str,
    pub sid: &'a str,
    pub vendor: &'a str,
    pub role: &'a str,
    pub title: Option<&'a str>,
}

/// Render the compact, shared status line used by text surfaces.
pub fn render_status_line(id: &StatusIdentity<'_>, st: &TurnStatus) -> String {
    let mut segments = vec![format!("→ {}/{}", id.slug, id.sid)];
    if !id.role.is_empty() {
        segments[0].push_str(&format!(" ({})", id.role));
    }
    let model = st.model.as_deref().filter(|model| !model.is_empty());
    match (id.vendor.is_empty(), model) {
        (false, Some(model)) => segments.push(format!("{} {model}", id.vendor)),
        (false, None) => segments.push(id.vendor.to_string()),
        (true, Some(model)) => segments.push(model.to_string()),
        (true, None) => {}
    }
    if let Some(context) = st.context.as_ref() {
        if let Some(pct) = context.pct() {
            let pct = pct.round() as u64;
            let warning = if pct >= 85 { "⚠" } else { "" };
            segments.push(format!("ctx {pct}%{warning}"));
        }
    }
    segments.push(format!("turn {}", st.turn));
    if let Some(cost) = st.cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0) {
        let rendered = if cost > 0.0 && cost < 0.005 {
            "<0.01".to_string()
        } else {
            format!("{cost:.2}")
        };
        segments.push(format!("${rendered}"));
    } else if let Some(tokens) = st.tokens_total {
        segments.push(format!("{:.1}k tok", tokens as f64 / 1000.0));
    }
    let mut line = segments.join(" · ");
    if let Some(title) = id.title.filter(|title| !title.is_empty()) {
        line.push_str(&format!(" 「{}」", truncate_title(title)));
    }
    line
}

fn truncate_title(title: &str) -> String {
    let mut chars = title.chars();
    let first: String = chars.by_ref().take(24).collect();
    if chars.next().is_some() {
        format!("{first}…")
    } else {
        first
    }
}

/// Build the transcript usage payload without making the mirror depend on
/// any gateway accounting details.
pub fn usage_value(usage: &UnifiedTokenUsage) -> serde_json::Value {
    serde_json::to_value(usage).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextSource;

    fn id<'a>() -> StatusIdentity<'a> {
        StatusIdentity {
            slug: "cct",
            sid: "s42",
            vendor: "codex",
            role: "reviewer",
            title: None,
        }
    }

    fn status() -> TurnStatus {
        TurnStatus {
            model: Some("gpt-5.3-codex".into()),
            context: Some(ContextUsage::known(19, 100, ContextSource::Reported)),
            turn: 7,
            cost_usd: Some(0.42),
            tokens_total: Some(12_300),
        }
    }

    #[test]
    fn renders_full_line() {
        assert_eq!(
            render_status_line(&id(), &status()),
            "→ cct/s42 (reviewer) · codex gpt-5.3-codex · ctx 19% · turn 7 · $0.42"
        );
    }

    #[test]
    fn omits_missing_segments_and_role() {
        let id = StatusIdentity { role: "", ..id() };
        let st = TurnStatus {
            model: None,
            context: Some(ContextUsage::window_only(100)),
            turn: 1,
            cost_usd: None,
            tokens_total: None,
        };
        assert_eq!(render_status_line(&id, &st), "→ cct/s42 · codex · turn 1");
    }

    #[test]
    fn omits_empty_vendor_and_model_segment() {
        let id = StatusIdentity {
            vendor: "",
            role: "",
            ..id()
        };
        let st = TurnStatus {
            model: None,
            context: None,
            turn: 1,
            cost_usd: None,
            tokens_total: None,
        };
        assert_eq!(render_status_line(&id, &st), "→ cct/s42 · turn 1");
    }

    #[test]
    fn warns_at_eighty_five_percent() {
        let st = TurnStatus {
            context: Some(ContextUsage::known(85, 100, ContextSource::Reported)),
            ..status()
        };
        assert!(render_status_line(&id(), &st).contains("ctx 85%⚠"));
    }

    #[test]
    fn renders_small_cost_and_token_fallback() {
        let small = TurnStatus {
            cost_usd: Some(0.001),
            ..status()
        };
        assert!(render_status_line(&id(), &small).contains("$<0.01"));
        let tok = TurnStatus {
            cost_usd: None,
            tokens_total: Some(12_345),
            ..status()
        };
        assert!(render_status_line(&id(), &tok).contains("12.3k tok"));
    }

    #[test]
    fn truncates_title_to_twenty_four_characters() {
        let id = StatusIdentity {
            title: Some("abcdefghijklmnopqrstuvwxyz"),
            ..id()
        };
        assert!(render_status_line(&id, &status()).ends_with("「abcdefghijklmnopqrstuvwx…」"));
    }
}
