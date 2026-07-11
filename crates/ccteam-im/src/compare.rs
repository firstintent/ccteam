//! Multi-vendor `/compare` (v0.8.24 C2 / v0.9-W3).
//!
//! Spawns N **roleless** one-shot sessions (true sids), fans the same prompt,
//! aggregates answers with cost subtotals. Per-session answers are suppressed
//! from the IM stream via compare taps; one aggregated Answer is emitted.

use std::time::Duration;

use ccteam_harness::AgentVendor;
use serde::{Deserialize, Serialize};

/// Default wall-clock budget for a compare fan-out (PRD Q12).
pub const DEFAULT_COMPARE_TIMEOUT: Duration = Duration::from_secs(300);

/// One vendor's slot in a compare result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareSlot {
    /// Vendor token (`claude` / `codex` / `grok` / `opencode`).
    pub vendor: String,
    /// Gateway session id (`s{n}`) that produced this slot.
    pub sid: String,
    /// Assistant text when successful; empty on timeout/error.
    pub answer: String,
    /// Per-session cost USD when known (never faked `0.0`).
    pub cost_usd: Option<f64>,
    /// `ok` | `error` | `timeout`
    pub status: String,
    /// Error/timeout detail when not ok.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregated compare outcome (REST body + IM render source).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareResult {
    /// Shared group id written to each session's meta.
    pub compare_group: String,
    /// Original user prompt.
    pub prompt: String,
    /// Per-vendor outcomes (order may mix ok/error/timeout).
    pub slots: Vec<CompareSlot>,
    /// Sum of known slot costs (None if every slot lacked cost).
    pub cost_subtotal_usd: Option<f64>,
    /// Wall-clock timeout used for this fan-out.
    pub timeout_secs: u64,
}

/// In-flight answer delivered from the event pump to the compare coordinator.
#[derive(Debug, Clone)]
pub struct CompareAnswer {
    /// Session id that produced the answer.
    pub sid: String,
    /// Vendor that answered.
    pub vendor: AgentVendor,
    /// Assistant text.
    pub text: String,
    /// Cost if known.
    pub cost_usd: Option<f64>,
}

/// Render an IM-facing markdown card for a [`CompareResult`].
pub fn render_compare_markdown(result: &CompareResult) -> String {
    let q = truncate_chars(&result.prompt, 80);
    let mut out = format!("⚖️ compare — \"{q}\"\n");
    for slot in &result.slots {
        out.push_str(&format!(
            "\n—— {} · {} · {}\n",
            slot.vendor,
            slot.sid,
            status_label(slot)
        ));
        if slot.status == "ok" {
            let body = slot.answer.trim();
            if body.is_empty() {
                out.push_str("(empty answer)\n");
            } else {
                out.push_str(body);
                out.push('\n');
            }
        } else if let Some(err) = &slot.error {
            out.push_str(&format!("⚠ {err}\n"));
        } else {
            out.push_str("⚠ no answer\n");
        }
        if let Some(c) = slot.cost_usd {
            out.push_str(&format!("cost: ${c:.4}\n"));
        }
    }
    match result.cost_subtotal_usd {
        Some(s) => out.push_str(&format!("\nΣ cost ≈ ${s:.4}\n")),
        None => out.push_str("\nΣ cost ≈ —\n"),
    }
    out.push_str("Tip: `/use <sid>` to continue any answer.\n");
    out
}

fn status_label(slot: &CompareSlot) -> &'static str {
    match slot.status.as_str() {
        "ok" => "ok",
        "timeout" => "timeout",
        _ => "error",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..max].iter().collect();
    format!("{head}…")
}

/// Default vendor set for compare (all four; unavailable ones fail partial).
pub fn default_compare_vendors() -> Vec<AgentVendor> {
    vec![
        AgentVendor::Claude,
        AgentVendor::Codex,
        AgentVendor::Grok,
        AgentVendor::Opencode,
    ]
}

/// Parse optional `vendors` body tokens → AgentVendor list.
pub fn parse_compare_vendors(raw: &[String]) -> Result<Vec<AgentVendor>, String> {
    if raw.is_empty() {
        return Ok(default_compare_vendors());
    }
    let mut out = Vec::new();
    for r in raw {
        let v = match r.trim().to_ascii_lowercase().as_str() {
            "claude" => AgentVendor::Claude,
            "codex" => AgentVendor::Codex,
            "grok" => AgentVendor::Grok,
            "opencode" | "open-code" => AgentVendor::Opencode,
            other => {
                return Err(format!(
                    "unknown vendor `{other}` (claude|codex|grok|opencode)"
                ))
            }
        };
        if !out.contains(&v) {
            out.push(v);
        }
    }
    if out.is_empty() {
        return Err("vendors list empty after parse".into());
    }
    Ok(out)
}

/// Mint a short compare group id (not a security secret).
pub fn mint_compare_group() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("cmp-{:x}", nanos & 0xffff_ffff_ffff)
}

/// Protocol for a compare target (roleless, skip permissions).
pub fn protocol_for_vendor(v: AgentVendor) -> ccteam_harness::SessionProtocol {
    match v {
        AgentVendor::Grok | AgentVendor::Opencode => ccteam_harness::SessionProtocol::Acp,
        _ => ccteam_harness::SessionProtocol::StreamJson,
    }
}

/// Vendor wire label.
pub fn vendor_label(v: AgentVendor) -> &'static str {
    match v {
        AgentVendor::Claude => "claude",
        AgentVendor::Codex => "codex",
        AgentVendor::Grok => "grok",
        AgentVendor::Opencode => "opencode",
    }
}

/// Sum known costs; None if every slot lacked a number.
pub fn cost_subtotal(slots: &[CompareSlot]) -> Option<f64> {
    let mut sum = 0.0;
    let mut any = false;
    for s in slots {
        if let Some(c) = s.cost_usd {
            sum += c;
            any = true;
        }
    }
    any.then_some(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vendors_default_and_subset() {
        assert_eq!(parse_compare_vendors(&[]).unwrap().len(), 4);
        let v = parse_compare_vendors(&["claude".into(), "codex".into()]).unwrap();
        assert_eq!(v, vec![AgentVendor::Claude, AgentVendor::Codex]);
        assert!(parse_compare_vendors(&["nope".into()]).is_err());
    }

    #[test]
    fn render_includes_sid_and_subtotal() {
        let r = CompareResult {
            compare_group: "cmp-1".into(),
            prompt: "why flaky?".into(),
            slots: vec![
                CompareSlot {
                    vendor: "claude".into(),
                    sid: "s1".into(),
                    answer: "race".into(),
                    cost_usd: Some(0.01),
                    status: "ok".into(),
                    error: None,
                },
                CompareSlot {
                    vendor: "codex".into(),
                    sid: "s2".into(),
                    answer: String::new(),
                    cost_usd: None,
                    status: "timeout".into(),
                    error: Some("timed out".into()),
                },
            ],
            cost_subtotal_usd: Some(0.01),
            timeout_secs: 300,
        };
        let md = render_compare_markdown(&r);
        assert!(md.contains("s1"));
        assert!(md.contains("race"));
        assert!(md.contains("timeout"));
        assert!(md.contains("0.0100"));
    }
}
