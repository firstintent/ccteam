//! `dsh_acp::spawn_spec::dsh_bin` env-var resolution — integration (own
//! process) per the workspace rule against `set_var`/`remove_var` in lib
//! `#[cfg(test)]` modules.

use ccteam_harness::execution::dsh_acp::spawn_spec::dsh_bin;
use ccteam_harness::DSH_BIN_ENV;
use serial_test::serial;

#[test]
#[serial(dsh_bin_env)]
fn default_bin_is_dsh_absent_override() {
    // SAFETY: `#[serial]` on this key gives this test exclusive access to
    // `DSH_BIN_ENV` within the process.
    unsafe {
        std::env::remove_var(DSH_BIN_ENV);
    }
    assert_eq!(dsh_bin(), "dsh");
}

#[test]
#[serial(dsh_bin_env)]
fn env_override_wins() {
    // SAFETY: see above.
    unsafe {
        std::env::set_var(DSH_BIN_ENV, "/opt/dsh/bin/dsh");
    }
    assert_eq!(dsh_bin(), "/opt/dsh/bin/dsh");
    unsafe {
        std::env::remove_var(DSH_BIN_ENV);
    }
}
