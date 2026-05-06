//! Phase DAG inferred from a topologically-ordered slice of
//! `PhaseTemplate`s. Replaces the M0 `M0_PHASE_DAG` / `FIRST_PHASE`
//! constants so ccteam-core can drive non-dev teams.
//!
//! Two edges per phase:
//!
//! - `next_on_done`: the phase to dispatch after `phase_done`. When
//!   omitted in YAML, defaults to the next phase in the topological
//!   list (file-order). Last phase's default is `None` — that's how
//!   the DAG terminal node is recognized.
//! - `next_on_escalate`: explicit revert target on `escalate`. When
//!   omitted, the project is marked terminally escalated. The M0.5.4
//!   `REVERT_TO_PHASE` ESCALATE grammar still routes via the event's
//!   `target_phase` field — that's hot-routed by the orchestrator
//!   and independent of this static fallback.

use std::collections::HashMap;

use anyhow::Result;

use crate::phases::PhaseTemplate;
use crate::state::ProjectState;

#[derive(Debug, Clone)]
pub struct Dag {
    /// First phase to dispatch on a fresh project (idle, current_phase
    /// empty). Derived from the input slice's order — i.e. the first
    /// template in topological filename order.
    entry: String,
    /// phase name → next phase on `phase_done`. `None` marks a DAG
    /// endpoint (terminal node).
    next_on_done: HashMap<String, Option<String>>,
    /// phase name → static revert target on `escalate`. `None` means
    /// "no automatic revert; project is terminally escalated".
    next_on_escalate: HashMap<String, Option<String>>,
}

impl Dag {
    /// Build a DAG from a topologically-ordered slice of phase
    /// templates (typically loaded by `Orchestrator::new` from
    /// `~/.ccteam/phases/` after sort-by-filename).
    ///
    /// Defaults: `phases[i].next_on_done = phases[i+1].name` unless
    /// the YAML overrides; the last phase's default is `None`. So the
    /// shipped dev pipeline gets a working DAG without any phase
    /// markdown declaring `next_on_done` — only forks need explicit
    /// edges.
    ///
    /// An empty template slice is allowed (matches the pre-M3.1
    /// behavior where `Orchestrator::new` succeeded with no phases on
    /// disk). The resulting DAG has an empty entry node and dispatches
    /// nothing — `decide_tick` short-circuits to NoOp until phases
    /// are written and the orchestrator restarts.
    pub fn from_templates(templates: &[PhaseTemplate]) -> Result<Self> {
        if templates.is_empty() {
            return Ok(Self {
                entry: String::new(),
                next_on_done: HashMap::new(),
                next_on_escalate: HashMap::new(),
            });
        }
        let mut next_on_done: HashMap<String, Option<String>> = HashMap::new();
        let mut next_on_escalate: HashMap<String, Option<String>> = HashMap::new();
        for (i, t) in templates.iter().enumerate() {
            let topo_default = templates.get(i + 1).map(|n| n.name.clone());
            let done = t.next_on_done.clone().or(topo_default);
            next_on_done.insert(t.name.clone(), done);
            next_on_escalate.insert(t.name.clone(), t.next_on_escalate.clone());
        }
        Ok(Self {
            entry: templates[0].name.clone(),
            next_on_done,
            next_on_escalate,
        })
    }

    /// True when the DAG has no phases (orchestrator started without a
    /// populated `~/.ccteam/phases/` dir). `decide_tick` returns NoOp
    /// in this state — there's nothing to dispatch.
    pub fn is_empty(&self) -> bool {
        self.entry.is_empty()
    }

    /// First phase a fresh project should dispatch.
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Phase to dispatch after `phase_done` from `name`. `None` when
    /// `name` is a DAG endpoint or unknown.
    pub fn next_on_done(&self, name: &str) -> Option<&str> {
        self.next_on_done.get(name).and_then(|o| o.as_deref())
    }

    /// Static revert target on `escalate` from `name`. `None` for
    /// "terminally escalate" (M0/M1 default for every phase).
    pub fn next_on_escalate(&self, name: &str) -> Option<&str> {
        self.next_on_escalate.get(name).and_then(|o| o.as_deref())
    }

    /// True when `name` is a DAG endpoint — declared without a
    /// `next_on_done` edge and last in topological order.
    pub fn is_terminal_phase(&self, name: &str) -> bool {
        matches!(self.next_on_done.get(name), Some(None))
    }

    /// True when the project has reached a terminal state: any
    /// history entry escalated **without a subsequent `resumed`
    /// entry**, or any passed history entry landed on a DAG endpoint.
    ///
    /// E2E 2026-05-06 F8: `ccteam resume` appends a `"resumed"` entry
    /// after an `"escalated"` one to lift the terminal flag — without
    /// it, the orchestrator's tick stayed `NoOp` forever and no
    /// further phases advanced even after the user manually
    /// re-injected the next phase. We pair escalated/resumed in
    /// history rather than mutating the original entry so the audit
    /// trail of past escalations stays intact.
    pub fn is_terminal_state(&self, state: &ProjectState) -> bool {
        let mut escalated_active = false;
        for h in &state.phase_history {
            match h.status.as_str() {
                "escalated" => escalated_active = true,
                "resumed" => escalated_active = false,
                "passed" if self.is_terminal_phase(&h.phase) => return true,
                _ => {}
            }
        }
        escalated_active
    }
}

/// Build a `Dag` from the shipped dev phase templates (`PHASE_TEMPLATES`).
/// Convenience for tests that exercise the orchestrator state machine
/// without bootstrapping a project on disk first. Production code
/// gets its DAG from `Orchestrator::new` after loading templates from
/// `~/.ccteam/phases/`.
pub fn dev_dag() -> Dag {
    let templates: Vec<PhaseTemplate> = crate::templates::PHASE_TEMPLATES
        .iter()
        .map(|(_, body)| {
            PhaseTemplate::parse(body)
                .expect("shipped phase template must parse")
        })
        .collect();
    Dag::from_templates(&templates).expect("shipped phase templates must form a valid DAG")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Parallelism;
    use crate::tool_surface::ToolsRequired;

    fn t(name: &str, next_on_done: Option<&str>) -> PhaseTemplate {
        PhaseTemplate {
            name: name.into(),
            required_inputs: Vec::new(),
            required_outputs: Vec::new(),
            soft_cost_warn_usd: None,
            stall_warn_minutes: None,
            parallelism: Parallelism::Solo,
            agent_team: Vec::new(),
            sub_skills: Vec::new(),
            tools_required: ToolsRequired::default(),
            hooks: Default::default(),
            auto_loop: false,
            auto_loop_max_iterations: 3,
            completion_signal: String::new(),
            next_on_done: next_on_done.map(String::from),
            next_on_escalate: None,
            decision_mode: crate::phases::DecisionMode::default(),
            max_clarify_rounds: 3,
            golden_rules: Vec::new(),
        }
    }

    #[test]
    fn topological_order_provides_default_edges() {
        let templates = vec![t("a", None), t("b", None), t("c", None)];
        let dag = Dag::from_templates(&templates).unwrap();
        assert_eq!(dag.entry(), "a");
        assert_eq!(dag.next_on_done("a"), Some("b"));
        assert_eq!(dag.next_on_done("b"), Some("c"));
        assert_eq!(dag.next_on_done("c"), None);
        assert!(dag.is_terminal_phase("c"));
        assert!(!dag.is_terminal_phase("a"));
    }

    #[test]
    fn explicit_next_on_done_overrides_topological_default() {
        // `a → c` skips `b`; `b` falls back to its topo neighbor `c`.
        let templates = vec![t("a", Some("c")), t("b", None), t("c", None)];
        let dag = Dag::from_templates(&templates).unwrap();
        assert_eq!(dag.next_on_done("a"), Some("c"));
        assert_eq!(dag.next_on_done("b"), Some("c"));
        assert_eq!(dag.next_on_done("c"), None);
    }

    #[test]
    fn empty_template_list_yields_inert_dag() {
        let dag = Dag::from_templates(&[]).unwrap();
        assert!(dag.is_empty());
        assert_eq!(dag.entry(), "");
        assert_eq!(dag.next_on_done("anything"), None);
    }

    #[test]
    fn unknown_phase_returns_none_not_panic() {
        let templates = vec![t("a", None)];
        let dag = Dag::from_templates(&templates).unwrap();
        assert_eq!(dag.next_on_done("ghost"), None);
        assert!(!dag.is_terminal_phase("ghost"));
    }
}
