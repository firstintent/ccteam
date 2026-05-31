//! `ccteam__advise_*` MCP tools.
//!
//! V0.6.0 Wave 1 (F112 §A) registered the schemas as STUBs returning
//! `NotImplemented`. V0.6.5 F152 / F153 swap both dispatchers in for
//! the real implementations defined in
//! [`ccteam_core::advise`].
//!
//! - `ccteam__advise_vote` — Claude + Codex one-shot fan-out + verdict
//!   synthesis (third Claude call). Codex-unavailable paths still
//!   return `ok:true` with `codex_status:"unavailable"` + the verdict
//!   prose explicitly noting unavailability (`ccteam-advise` skill
//!   "Red lines" §3 enforcement point).
//! - `ccteam__advise_parallel` — N raw answers, no synthesis. Vendors
//!   round-robin when `vendors.len() < n`.
//!
//! Budget enforcement: each call charges
//! [`ccteam_core::advise::APPROX_COST_PER_CALL_USD`] per vendor that
//! actually returns. The pre-call ledger sum is checked against the
//! caller-supplied `max_cost_usd` (default
//! [`ccteam_core::advise::DEFAULT_ADVISE_BUDGET_USD_24H`]) — over-cap
//! returns `ok:false, error:"budget_exceeded"` without spawning any
//! advisor subprocess.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::advise;
use ccteam_core::paths::CcteamPaths;
use ccteam_core::AgentVendor;

/// Tool definitions for the 2 advise tools. Merged into the top-level
/// `tool_definitions()` in `mcp_serve.rs`.
pub fn advise_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__advise_vote",
            "description": "V0.6.5 F152 — ask Claude AND Codex the same hard question in parallel, then synthesise a 3-5 sentence verdict via a third Claude call (agreement / disagreement / suggested approach). Use for second-opinion / cross-vendor sanity checks. When `codex` is not on PATH the result still returns `ok:true` with `codex_status:\"unavailable\"` and the verdict explicitly says \"Codex unavailable: <reason>\". Charges ~0.005 USD per vendor call (Claude + Codex + synth) against the rolling 24h cap in `<ccteam_root>/cost-budget.json`; default cap 0.50 USD/24h, override via `max_cost_usd`.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The hard question to put to both vendors."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional context block prepended to each vendor's prompt."
                    },
                    "codex_timeout_secs": {
                        "type": "integer",
                        "description": "Per-vendor wall clock (default 60). Codex slot returns `status:timeout` if exceeded; Claude slot runs to completion (no kill — Claude `-p` is bounded by the model's own response cap)."
                    },
                    "max_cost_usd": {
                        "type": "number",
                        "description": "Rolling 24h budget cap (default 0.50). Exceeded → `ok:false, error:\"budget_exceeded\"` without spawning any advisor."
                    }
                },
                "required": ["question"],
            }),
        }),
        json!({
            "name": "ccteam__advise_parallel",
            "description": "V0.6.5 F153 — fan one prompt to N parallel advisor sessions (2-8) and return all individual answers without vote synthesis. Use when you want raw N-of-N rather than a single combined verdict. `vendors` rotates round-robin when `vendors.len() < n`; passing `vendors.len() > n` returns invalid input. Same per-call budget charging + ledger as `advise_vote` (no synth charge — N calls, not N+1). Codex-unavailable slots return `status:\"unavailable\"`; the array still has N rows so the caller can render placeholders.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Prompt to fan out." },
                    "context": { "type": "string", "description": "Optional context block prepended to each vendor's prompt." },
                    "n": { "type": "integer", "description": "Number of advisor slots (2-8)." },
                    "vendors": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["claude", "codex"] },
                        "description": "Vendor pool to round-robin into N slots. Default ['claude','codex']."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Per-vendor wall clock (default 60)."
                    },
                    "max_cost_usd": {
                        "type": "number",
                        "description": "Rolling 24h budget cap (default 0.50)."
                    }
                },
                "required": ["question", "n"],
            }),
        }),
    ]
}

/// Dispatch a `ccteam__advise_*` tool. Returns `Ok(None)` for tools
/// that aren't ours so the caller falls through to the next
/// dispatcher.
pub async fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    match name {
        "ccteam__advise_vote" => Ok(Some(dispatch_vote(&paths.root, args).await?)),
        "ccteam__advise_parallel" => Ok(Some(dispatch_parallel(&paths.root, args).await?)),
        _ => Ok(None),
    }
}

/// V0.6.5 F152 — `ccteam__advise_vote` dispatcher.
pub async fn dispatch_vote(ccteam_root: &std::path::Path, args: &Value) -> Result<String> {
    let question = arg_str(args, "question")?;
    let context = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(String::from);
    let codex_timeout_secs = args.get("codex_timeout_secs").and_then(|v| v.as_u64());
    let max_cost_usd = args.get("max_cost_usd").and_then(|v| v.as_f64());

    let result = advise::advise_vote(
        ccteam_root,
        &question,
        context.as_deref(),
        codex_timeout_secs,
        max_cost_usd,
    )
    .await;
    match result {
        Ok(r) => Ok(serde_json::to_string_pretty(&json!({
            "ok": r.ok,
            "question": r.question,
            "verdict": r.verdict,
            "claude_answer": r.claude_answer,
            "codex_answer": r.codex_answer,
            "codex_status": r.codex_status,
            "agreement": r.agreement,
            "budget": {
                "advise_today_usd": r.budget.advise_today_usd,
                "cap_usd": r.budget.cap_usd,
            },
        }))?),
        Err(advise::AdviseError::BudgetExceeded { spent_usd, cap_usd }) => {
            Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "error": "budget_exceeded",
                "spent_usd": spent_usd,
                "cap_usd": cap_usd,
            }))?)
        }
        Err(advise::AdviseError::InvalidInput(msg)) => Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "error": "invalid_input",
            "detail": msg,
        }))?),
        Err(other) => Err(anyhow!("advise_vote failed: {other}")),
    }
}

/// V0.6.5 F153 — `ccteam__advise_parallel` dispatcher.
pub async fn dispatch_parallel(ccteam_root: &std::path::Path, args: &Value) -> Result<String> {
    let question = arg_str(args, "question")?;
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing required integer arg `n`"))? as usize;
    let context = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(String::from);
    let vendors = parse_vendors(args)?;
    let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64());
    let max_cost_usd = args.get("max_cost_usd").and_then(|v| v.as_f64());

    let result = advise::advise_parallel(
        ccteam_root,
        &question,
        context.as_deref(),
        n,
        &vendors,
        timeout_secs,
        max_cost_usd,
    )
    .await;
    match result {
        Ok(r) => {
            let answers: Vec<Value> = r
                .answers
                .iter()
                .map(|a| {
                    json!({
                        "vendor": a.vendor,
                        "answer": a.answer,
                        "status": a.status,
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&json!({
                "ok": r.ok,
                "question": r.question,
                "answers": answers,
                "budget": {
                    "advise_today_usd": r.budget.advise_today_usd,
                    "cap_usd": r.budget.cap_usd,
                },
            }))?)
        }
        Err(advise::AdviseError::BudgetExceeded { spent_usd, cap_usd }) => {
            Ok(serde_json::to_string_pretty(&json!({
                "ok": false,
                "error": "budget_exceeded",
                "spent_usd": spent_usd,
                "cap_usd": cap_usd,
            }))?)
        }
        Err(advise::AdviseError::InvalidInput(msg)) => Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "error": "invalid_input",
            "detail": msg,
        }))?),
        Err(other) => Err(anyhow!("advise_parallel failed: {other}")),
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("missing required string arg `{key}`"))
}

/// Parse the optional `vendors: ["claude","codex"]` array. Default
/// `[Claude, Codex]` when missing or empty.
fn parse_vendors(args: &Value) -> Result<Vec<AgentVendor>> {
    let Some(arr) = args.get("vendors").and_then(|v| v.as_array()) else {
        return Ok(vec![AgentVendor::Claude, AgentVendor::Codex]);
    };
    if arr.is_empty() {
        return Ok(vec![AgentVendor::Claude, AgentVendor::Codex]);
    }
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let raw = v
            .as_str()
            .ok_or_else(|| anyhow!("`vendors` array entries must be strings"))?;
        match raw.to_lowercase().as_str() {
            "claude" => out.push(AgentVendor::Claude),
            "codex" => out.push(AgentVendor::Codex),
            other => {
                return Err(anyhow!(
                    "invalid vendor `{other}`: expected `claude` or `codex`"
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("ccteam-root"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn two_advise_tools_registered() {
        let tools = advise_tool_definitions();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__advise_vote"));
        assert!(names.contains(&"ccteam__advise_parallel"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_returns_none_for_foreign_tools() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        assert!(dispatch(&p, "ccteam__chat_send_input", &json!({}))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_vote_reports_budget_exceeded_without_spawning() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-root");
        std::fs::create_dir_all(&root).unwrap();
        // Seed the ledger past the cap so the dispatcher refuses
        // BEFORE attempting any vendor spawn (so this test is
        // hermetic — no `claude` binary on PATH needed).
        for _ in 0..30 {
            ccteam_core::advise::append_budget_sample(&root, AgentVendor::Claude, 0.10).unwrap();
        }
        let body = dispatch_vote(
            &root,
            &json!({ "question": "Should I pick A or B?", "max_cost_usd": 0.5 }),
        )
        .await
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "budget_exceeded");
        let spent = parsed["spent_usd"].as_f64().unwrap();
        let cap = parsed["cap_usd"].as_f64().unwrap();
        assert!(spent >= cap, "spent {spent} < cap {cap}");
        assert!((cap - 0.5).abs() < 1e-9);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_vote_rejects_missing_question() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-root");
        let err = dispatch_vote(&root, &json!({})).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("missing required string arg `question`"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_parallel_rejects_n_out_of_range() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-root");
        std::fs::create_dir_all(&root).unwrap();
        let body = dispatch_parallel(&root, &json!({ "question": "Q", "n": 1 }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "invalid_input");
        assert!(parsed["detail"]
            .as_str()
            .unwrap()
            .contains("`n` must be in 2..=8"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_parallel_rejects_vendors_len_greater_than_n() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-root");
        std::fs::create_dir_all(&root).unwrap();
        let body = dispatch_parallel(
            &root,
            &json!({ "question": "Q", "n": 2, "vendors": ["claude","codex","claude"] }),
        )
        .await
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "invalid_input");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_parallel_reports_budget_exceeded() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-root");
        std::fs::create_dir_all(&root).unwrap();
        for _ in 0..30 {
            ccteam_core::advise::append_budget_sample(&root, AgentVendor::Claude, 0.10).unwrap();
        }
        let body = dispatch_parallel(
            &root,
            &json!({ "question": "Q", "n": 2, "max_cost_usd": 0.5 }),
        )
        .await
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "budget_exceeded");
    }

    #[test]
    fn parse_vendors_default_is_both() {
        let v = parse_vendors(&json!({})).unwrap();
        assert_eq!(v, vec![AgentVendor::Claude, AgentVendor::Codex]);
    }

    #[test]
    fn parse_vendors_empty_array_falls_back_to_default() {
        let v = parse_vendors(&json!({ "vendors": [] })).unwrap();
        assert_eq!(v, vec![AgentVendor::Claude, AgentVendor::Codex]);
    }

    #[test]
    fn parse_vendors_lowercases_input() {
        let v = parse_vendors(&json!({ "vendors": ["Claude", "CODEX"] })).unwrap();
        assert_eq!(v, vec![AgentVendor::Claude, AgentVendor::Codex]);
    }

    #[test]
    fn parse_vendors_rejects_unknown() {
        let err = parse_vendors(&json!({ "vendors": ["gpt"] })).unwrap_err();
        assert!(err.to_string().contains("invalid vendor"));
    }

    #[test]
    fn all_advise_tools_carry_advise_prefix() {
        for t in advise_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__advise_"),
                "advise tool name must start with ccteam__advise_: {n}"
            );
        }
    }
}
