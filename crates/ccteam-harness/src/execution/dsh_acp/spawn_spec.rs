//! DSH child bin resolution skeleton.
//!
//! v0.9.15 P1/P2 stub: `DshAcpAdapter::start_thread` never actually spawns a
//! child (it fails honestly — see `mod.rs`), so this module only pins the
//! env-override constant + default binary name the doctor / probe surfaces
//! (`AGENT_PROBE_SPECS`, `ccteam doctor`) need ahead of the real adapter
//! (P3, `dsh --profile ccteam` — tech-design.md §5.4.2, K5).
//!
//! Env-mutating coverage lives in `tests/dsh_acp_test.rs` (integration, own
//! process) — not here, per the workspace rule against `set_var`/`remove_var`
//! in lib `#[cfg(test)]` modules (cross-test env races).

pub const DSH_BIN_ENV: &str = "CCTEAM_DSH_BIN";

/// Resolve the `dsh` binary path: `CCTEAM_DSH_BIN` override, else `dsh` on
/// `PATH`. Mirrors `pi_rpc::spawn_spec::pi_bin`.
pub fn dsh_bin() -> String {
    std::env::var(DSH_BIN_ENV).unwrap_or_else(|_| "dsh".to_string())
}
