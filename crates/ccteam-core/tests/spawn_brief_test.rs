//! V0.6.0 F115 — `render_spawn_brief` integration tests.

use std::path::PathBuf;

use ccteam_core::handoff::{write_handoff, WriteHandoffOptions};
use ccteam_core::spawn_brief::{render_spawn_brief, SpawnContext};
use tempfile::TempDir;

fn ctx(td: &std::path::Path, slug: &str, role: &str) -> SpawnContext {
    SpawnContext::new(PathBuf::from(td), slug.to_string(), role.to_string())
}

#[test]
fn replaces_include_prev_handoffs_with_recent_docs() {
    let td = TempDir::new().unwrap();
    write_handoff(&WriteHandoffOptions {
        project_dir: td.path().to_path_buf(),
        workflow_slug: "demo".into(),
        stage_num: 1,
        role: "explorer".into(),
        content: "explorer-decision-text\n".into(),
    })
    .unwrap();
    write_handoff(&WriteHandoffOptions {
        project_dir: td.path().to_path_buf(),
        workflow_slug: "demo".into(),
        stage_num: 2,
        role: "fixer".into(),
        content: "fixer-decision-text\n".into(),
    })
    .unwrap();

    let template = "<kicker>\n{{include_prev_handoffs}}\n</kicker>";
    let rendered = render_spawn_brief(template, &ctx(td.path(), "demo", "reviewer")).unwrap();

    assert!(rendered.contains("explorer-decision-text"));
    assert!(rendered.contains("fixer-decision-text"));
    assert!(
        !rendered.contains("{{include_prev_handoffs}}"),
        "token must be expanded"
    );
}

#[test]
fn empty_token_replacement_when_no_handoffs() {
    let td = TempDir::new().unwrap();
    let template = "before {{include_prev_handoffs}} after";
    let rendered = render_spawn_brief(template, &ctx(td.path(), "demo", "explorer")).unwrap();
    // Token gone, no panic, no error.
    assert_eq!(rendered, "before  after");
}

#[test]
fn multiple_tokens_render_in_one_pass() {
    let td = TempDir::new().unwrap();
    let mut c = ctx(td.path(), "demo-slug", "explorer");
    c.stage_num = Some(7);

    let template = "slug={{workflow_slug}} role={{role}} stage={{stage_num}} END";
    let rendered = render_spawn_brief(template, &c).unwrap();
    assert_eq!(rendered, "slug=demo-slug role=explorer stage=7 END");
}

#[test]
fn handoffs_token_coexists_with_identity_tokens() {
    let td = TempDir::new().unwrap();
    write_handoff(&WriteHandoffOptions {
        project_dir: td.path().to_path_buf(),
        workflow_slug: "demo".into(),
        stage_num: 1,
        role: "explorer".into(),
        content: "previous-decision\n".into(),
    })
    .unwrap();

    let mut c = ctx(td.path(), "demo", "fixer");
    c.stage_num = Some(2);
    let template = "ROLE={{role}} | STAGE={{stage_num}}\n---\n{{include_prev_handoffs}}";
    let rendered = render_spawn_brief(template, &c).unwrap();

    assert!(rendered.starts_with("ROLE=fixer | STAGE=2\n---\n"));
    assert!(rendered.contains("previous-decision"));
}

#[test]
fn hot_path_no_tokens_returns_input_unchanged() {
    let td = TempDir::new().unwrap();
    let template = "raw kicker prompt, no template syntax";
    let rendered = render_spawn_brief(template, &ctx(td.path(), "demo", "explorer")).unwrap();
    assert_eq!(rendered, template);
}

#[test]
fn unknown_tokens_passthrough() {
    let td = TempDir::new().unwrap();
    let template = "{{unknown_field}} stays put; {{role}} expands";
    let rendered = render_spawn_brief(template, &ctx(td.path(), "demo", "explorer")).unwrap();
    assert_eq!(rendered, "{{unknown_field}} stays put; explorer expands");
}
