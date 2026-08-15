//! DSH (DeepSeek Harness) ACP adapter — seventh vendor (`AgentVendor::Dsh`).
//!
//! v0.9.15 P1/P2 ships a **minimal stub**: every `HarnessAdapter` method
//! answers honestly (`start_thread` fails, `events` never yields, the tool
//! face always needs a respawn) so the enum-slam compiles end to end without
//! pretending DSH sessions work yet. The real handshake ladder
//! (`session/new` / `session/load`, K26 event mapping, cold resume) lands in
//! P3 — see `docs-local/versions/v0-9-15/tech-design.md` §5.4 / §5.7.

pub mod spawn_spec;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};

use crate::execution::acp::released_thread_status;
use crate::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, ToolSurfaceRebuild, TurnInput,
    TurnRouting, TurnSubmission,
};

pub use spawn_spec::{dsh_bin, DSH_BIN_ENV};

/// Adapter name — stable id for handles / logs / tests.
pub const DSH_ACP_ADAPTER_NAME: &str = "dsh-acp";

/// Why every stub method that cannot honestly do its job fails: named once
/// so the message stays identical across `start_thread` / `submit_turn_routed`
/// / `resume_thread` and a fix to one doesn't drift from the others.
const STUB_REASON: &str = "the dsh adapter is a v0.9.15 P1/P2 stub — the real \
    handshake lands in P3 (see docs-local/versions/v0-9-15/tech-design.md §5.4)";

/// Minimal DSH adapter stub. Holds no state — there is nothing to spawn,
/// track, or tear down yet.
#[derive(Debug, Default)]
pub struct DshAcpAdapter;

impl DshAcpAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HarnessAdapter for DshAcpAdapter {
    fn name(&self) -> &'static str {
        DSH_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Dsh
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        _ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::SpawnFailed(STUB_REASON.to_string()))
    }

    async fn submit_turn_routed(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: STUB_REASON.to_string(),
        })
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // No transport exists yet (start_thread always fails, so no live
        // handle should ever reach this) — a valid subscription that never
        // yields and never ends, consistent with `Rebuildable` below (an
        // attachment that "ends" would need a real transport to end).
        stream::pending().boxed()
    }

    fn event_attachment(&self) -> EventAttachment {
        EventAttachment::Rebuildable
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ToolSurfaceRebuild, HarnessError> {
        Ok(ToolSurfaceRebuild::RespawnRequired {
            reason: "dsh's plugin registers tools once at Cordis `apply()`; a respawn \
                     is lossless — `session/load` reattaches the vendor's own memory"
                .to_string(),
        })
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: STUB_REASON.to_string(),
        })
    }

    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        // Nothing was ever spawned — idempotent no-op.
        Ok(())
    }

    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Rejected {
            reason: format!("`{}` is not supported by the dsh adapter stub", d.name),
        })
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // No live handle ever exists yet — answer from whatever a persisted
        // snapshot has (honest "released" shape), same as the ACP vendors.
        Ok(released_thread_status(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionMode, TurnRouting};

    fn handle() -> ThreadHandle {
        ThreadHandle {
            vendor: AgentVendor::Dsh,
            mode: ExecutionMode::Chat,
            identity: "s1".to_string(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        }
    }

    #[test]
    fn name_and_vendor_are_dsh() {
        let a = DshAcpAdapter::new();
        assert_eq!(a.name(), DSH_ACP_ADAPTER_NAME);
        assert_eq!(a.vendor(), AgentVendor::Dsh);
    }

    #[tokio::test]
    async fn start_thread_fails_honestly() {
        let a = DshAcpAdapter::new();
        let spec = AgentSpecBrief {
            role: String::new(),
        };
        let ctx = SpawnCtx::default();
        let err = a.start_thread(&spec, &ctx).await.unwrap_err();
        assert!(matches!(err, HarnessError::SpawnFailed(_)));
        assert!(err.to_string().contains("v0.9.15 P1/P2 stub"));
    }

    #[tokio::test]
    async fn resume_thread_is_not_implemented() {
        let a = DshAcpAdapter::new();
        let err = a.resume_thread("some-vendor-uuid").await.unwrap_err();
        assert!(matches!(err, HarnessError::NotImplemented { .. }));
    }

    #[tokio::test]
    async fn submit_turn_routed_is_not_implemented() {
        let a = DshAcpAdapter::new();
        let err = a
            .submit_turn_routed(
                &handle(),
                TurnInput::UserText("hi".to_string()),
                TurnRouting::Inject,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::NotImplemented { .. }));
    }

    #[tokio::test]
    async fn events_stream_never_yields() {
        let a = DshAcpAdapter::new();
        let mut stream = a.events(&handle());
        let outcome =
            tokio::time::timeout(std::time::Duration::from_millis(20), stream.next()).await;
        assert!(
            outcome.is_err(),
            "a `pending()` stream must never yield an item"
        );
    }

    #[test]
    fn event_attachment_is_rebuildable() {
        let a = DshAcpAdapter::new();
        assert_eq!(a.event_attachment(), EventAttachment::Rebuildable);
    }

    #[tokio::test]
    async fn rebuild_tool_surface_always_needs_a_respawn() {
        let a = DshAcpAdapter::new();
        let outcome = a.rebuild_tool_surface(&handle()).await.unwrap();
        let ToolSurfaceRebuild::RespawnRequired { reason } = outcome;
        assert!(reason.contains("lossless"));
    }

    #[tokio::test]
    async fn close_thread_is_a_noop() {
        let a = DshAcpAdapter::new();
        a.close_thread(&handle()).await.unwrap();
    }

    #[tokio::test]
    async fn handle_directive_rejects_everything() {
        let a = DshAcpAdapter::new();
        let directive = Directive {
            name: "compact".to_string(),
            args: String::new(),
            choice: None,
        };
        let outcome = a.handle_directive(&handle(), directive).await.unwrap();
        assert!(matches!(outcome, DirectiveOutcome::Rejected { .. }));
    }

    #[tokio::test]
    async fn thread_status_answers_the_released_shape() {
        let a = DshAcpAdapter::new();
        let status = a.thread_status(&handle()).await.unwrap();
        assert_eq!(status, ThreadStatus::default());
    }
}
