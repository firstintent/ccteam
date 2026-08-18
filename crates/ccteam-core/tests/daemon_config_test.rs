use std::ffi::OsString;

use ccteam_core::{config, DAEMON_WORKERS_ENV};

struct EnvRestore {
    values: Vec<(&'static str, Option<OsString>)>,
}

impl EnvRestore {
    fn set(values: &[(&'static str, OsString)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self { values: previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.values.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[test]
fn daemon_workers_env_overrides_yaml() {
    let tmp = tempfile::TempDir::new().unwrap();
    let isolated_home = tmp.path().join("home");
    let ccteam_home = isolated_home.join(".ccteam");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    let _env = EnvRestore::set(&[
        ("HOME", isolated_home.into_os_string()),
        ("CCTEAM_HOME", ccteam_home.clone().into_os_string()),
        (DAEMON_WORKERS_ENV, OsString::from("6")),
    ]);
    std::fs::write(config::config_path(&ccteam_home), "daemon:\n  workers: 2\n").unwrap();

    let cfg = config::load(&ccteam_home).unwrap();
    assert_eq!(cfg.daemon.workers, 2, "YAML value still parses");
    assert_eq!(cfg.daemon.effective_workers().unwrap(), 6);
}
