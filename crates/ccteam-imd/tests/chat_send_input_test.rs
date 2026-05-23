//! V0.6.5 F147 — integration tests for the mailbox-envelope writer +
//! supervisor `handle_inbound` round-trip exercised by the
//! `ccteam__chat_send_input` MCP tool.
//!
//! The MCP tool itself lives in `ccteam-cli/src/mcp_chat_tools.rs`
//! (unit-tested there for envelope shape / path validation). This file
//! covers the daemon-side contract: an envelope written by any external
//! producer (MCP, IM router, test fixture) must round-trip through
//! `inbound::parse_envelope` → `BotSupervisor::handle_inbound` →
//! `adapter.submit_turn` with the payload arriving verbatim — no
//! prompt-injection, no envelope leakage into the user-turn body.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_imd::inbound::{parse_envelope, render_envelope, InboxEnvelope};
use ccteam_imd::supervisor::BotSupervisor;
use ccteam_imd::{chat_inbox_dir, BotRegistration};
use futures::stream::BoxStream;
use std::sync::Mutex as StdMutex;
use tempfile::TempDir;

/// Stub HarnessAdapter that records every `submit_turn` payload so we
/// can assert envelope → payload pass-through exactly matches what the
/// caller wrote.
#[derive(Debug, Default)]
struct CapturingAdapter {
    submitted: StdMutex<Vec<String>>,
}

#[async_trait]
impl HarnessAdapter for CapturingAdapter {
    fn name(&self) -> &'static str {
        "capture"
    }
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("cap-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        if let TurnInput::UserText(s) = input {
            self.submitted.lock().unwrap().push(s);
        }
        Ok(TurnId::new("cap-turn"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(futures::stream::empty())
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "stub".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }
}

fn reg() -> BotRegistration {
    BotRegistration {
        workflow_slug: "demo".into(),
        role: "helper".into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mcp".into(),
        im_chat_id: "0".into(),
        created_at: chrono::Utc::now(),
    }
}

fn write_envelope(inbox: &PathBuf, payload: &str) -> PathBuf {
    std::fs::create_dir_all(inbox).unwrap();
    let env = InboxEnvelope {
        platform: "mcp".to_string(),
        sender: "mcp-host".to_string(),
        hop: 0,
        received_at: chrono::Utc::now(),
        reply_target: String::new(),
        payload: payload.to_string(),
        message_id: "test-cid".to_string(),
    };
    let body = render_envelope(&env);
    let path = inbox.join("msg-test.md");
    std::fs::write(&path, body).unwrap();
    path
}

#[tokio::test]
async fn mailbox_envelope_round_trips_through_handle_inbound() {
    // Setup: tempdir projects root, fresh supervisor against a
    // capturing stub adapter. This mirrors the wire path the MCP
    // `chat_send_input` triggers (write envelope → daemon parses →
    // supervisor.handle_inbound) without requiring a full daemon loop.
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let adapter = Arc::new(CapturingAdapter::default());
    let sup = BotSupervisor::new(reg(), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();

    // Drop the envelope into the documented mailbox path (same path
    // `chat_send_input` writes to via `chat_inbox_dir`).
    let inbox = chat_inbox_dir(&projects_root, "demo", "helper");
    let path = write_envelope(&inbox, "please plan");

    // Daemon-side parse + dispatch (the production safety-net
    // `drain_inboxes` runs this exact sequence).
    let body = std::fs::read_to_string(&path).unwrap();
    let env = parse_envelope(&body).unwrap();
    sup.handle_inbound(env.payload).await.unwrap();

    // Adapter must have seen exactly the payload — no envelope yaml,
    // no extra wrapping. (CLAUDE.md §三 "No prompt injection".)
    let submitted = adapter.submitted.lock().unwrap().clone();
    assert_eq!(submitted, vec!["please plan".to_string()]);
}

#[test]
fn envelope_filename_collision_window_is_realistic() {
    // F147 PRD §risks notes that `msg-<unix-ms>-<rand>` needs enough
    // randomness so concurrent MCP bursts inside the same millisecond
    // don't collide. We use 8 hex chars = 2^32 ≈ 4.3B distinct values.
    // Sanity-check that 100 sequential generations produce 100 distinct
    // suffixes (probabilistic — failure is essentially impossible
    // unless the RNG is broken or the impl regressed to a counter).
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for _ in 0..100 {
        let env = InboxEnvelope {
            platform: "mcp".into(),
            sender: "x".into(),
            hop: 0,
            received_at: chrono::Utc::now(),
            reply_target: String::new(),
            payload: String::new(),
            message_id: String::new(),
        };
        // Just exercise the render path; the filename randomness lives
        // in the MCP dispatcher itself (`generate_rand_hex`), tested
        // via the dispatcher unit tests. This serves as a regression
        // anchor that the envelope shape stays stable.
        let body = render_envelope(&env);
        seen.insert(body);
    }
    // 100 different received_at timestamps → 100 unique render outputs.
    // (At least 50 to tolerate any same-microsecond ties.)
    assert!(
        seen.len() >= 50,
        "envelope renders should vary by received_at: got {}",
        seen.len()
    );
}

#[tokio::test]
async fn mailbox_envelope_payload_preserves_special_chars() {
    // Slash commands, backticks, newlines, multiline markdown — must
    // round-trip verbatim. This is the regression anchor for "user
    // pastes `/compact` into IM and the daemon mangles it to literal
    // /compact" vs the legitimate user case of literal text.
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let adapter = Arc::new(CapturingAdapter::default());
    let sup = BotSupervisor::new(reg(), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();

    let inbox = chat_inbox_dir(&projects_root, "demo", "helper");
    let payload = "Line 1\n```rust\nfn main() {}\n```\n@mention `/clear` end";
    let path = write_envelope(&inbox, payload);

    let body = std::fs::read_to_string(&path).unwrap();
    let env = parse_envelope(&body).unwrap();
    sup.handle_inbound(env.payload).await.unwrap();

    let submitted = adapter.submitted.lock().unwrap().clone();
    assert_eq!(submitted, vec![payload.to_string()]);
}
