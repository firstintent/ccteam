//! V0.2 M0.22 — env-mutating tests for the team factory's
//! `default_user_staging_dir` resolution. Mutates `XDG_CONFIG_HOME` so
//! it lives in its own integration test binary per CLAUDE.md §六
//! ("env-mutating tests放 crates/*/tests/*.rs").

use ccteam_core::{
    init_team_staging, publish_team, staging_dir_for, validate_staged_team, PhaseScaffold,
    PluginAuthor, PluginManifest, PublishInput, PublishTarget, TeamInitInput, TeamSpec,
};
use tempfile::TempDir;

fn sample_input<'a>(
    spec: &'a TeamSpec,
    manifest: &'a PluginManifest,
    phases: &'a [PhaseScaffold<'a>],
) -> TeamInitInput<'a> {
    TeamInitInput {
        spec,
        manifest,
        phases,
        staging_root_override: None,
    }
}

#[test]
fn xdg_config_home_drives_staging_dir_resolution() {
    // Set XDG_CONFIG_HOME to a tmp path; staging dir must resolve
    // beneath it. Pair with init/publish_local end-to-end so we trust
    // both code paths against the resolved root.
    let tmp = TempDir::new().unwrap();
    let xdg = tmp.path().join("xdg");
    std::env::set_var("XDG_CONFIG_HOME", &xdg);

    let staging = staging_dir_for("xdg-test-team", None);
    assert!(staging.starts_with(&xdg), "got: {}", staging.display());

    let spec = TeamSpec::parse("name: xdg-test-team\nphase_dir: phases\n").unwrap();
    let manifest = PluginManifest {
        name: "xdg-test-team".into(),
        description: "test".into(),
        author: PluginAuthor {
            name: "tester".into(),
            email: None,
        },
        version: None,
    };
    let phases = vec![PhaseScaffold {
        name: "intake",
        task_summary: "do the thing.",
        required_inputs: &[],
        required_outputs: &[".ccteam/spec.md"],
        auto_loop: true,
    }];
    let report = init_team_staging(&sample_input(&spec, &manifest, &phases)).unwrap();
    assert!(report.staging_dir.starts_with(&xdg));
    assert!(report.manifest_path.exists());

    // Validate the staged tree end-to-end.
    let findings = validate_staged_team(&staging).unwrap();
    assert!(findings.iter().any(|l| l.starts_with("[OK] plugin.json")));
    assert!(findings.iter().any(|l| l.starts_with("[OK] team.yaml")));

    // Publish to a faux ~/.claude — exercises the symlink path against
    // the XDG-resolved staging.
    let claude = tmp.path().join("claude");
    let publish = publish_team(&PublishInput {
        team_name: "xdg-test-team",
        target: PublishTarget::Local,
        staging_root_override: None,
        claude_dir_override: Some(&claude),
    })
    .unwrap();
    let link = publish.local_link.unwrap();
    let canonical = std::fs::canonicalize(&link).unwrap();
    assert_eq!(canonical, std::fs::canonicalize(&staging).unwrap());

    std::env::remove_var("XDG_CONFIG_HOME");
}
