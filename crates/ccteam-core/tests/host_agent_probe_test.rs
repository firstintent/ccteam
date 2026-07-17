//! v0.9.0 W3 (G9) — `ccteam_core::host_registry::probe_agents` env-mutating
//! coverage. Isolated integration test (own process) per CLAUDE.md §六:
//! `set_var`/`remove_var` on shared `CCTEAM_*_BIN` keys never belongs in a
//! lib `#[cfg(test)] mod tests` that shares a process with unrelated tests.

use ccteam_core::host_registry::{probe_agents, AGENT_PROBE_SPECS};

#[test]
fn probe_agents_covers_five_vendors_and_folds_missing_binary() {
    for spec in AGENT_PROBE_SPECS {
        std::env::set_var(spec.bin_env, "/nonexistent/ccteam-fake-zzz");
    }
    let agents = probe_agents();
    assert_eq!(agents.len(), 5);
    assert!(agents
        .iter()
        .all(|a| !a.installed && a.status == "not_installed" && a.version.is_none()));
    for spec in AGENT_PROBE_SPECS {
        std::env::remove_var(spec.bin_env);
    }
}
