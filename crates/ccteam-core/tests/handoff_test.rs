//! V0.6.0 F115 — handoff doc mechanism integration tests.

use std::fs;

use ccteam_core::handoff::{
    handoff_path, handoffs_dir, list_handoffs, read_concat, write_handoff, WriteHandoffOptions,
    HANDOFFS_DIRNAME, HANDOFF_TEMPLATE,
};
use tempfile::TempDir;

fn opts(
    dir: &std::path::Path,
    slug: &str,
    stage: u32,
    role: &str,
    body: &str,
) -> WriteHandoffOptions {
    WriteHandoffOptions {
        project_dir: dir.to_path_buf(),
        workflow_slug: slug.into(),
        stage_num: stage,
        role: role.into(),
        content: body.into(),
    }
}

#[test]
fn write_handoff_is_atomic_and_overwrites() {
    let td = TempDir::new().unwrap();
    let body1 = "first version\n";
    let body2 = "second version\n";

    let p1 = write_handoff(&opts(td.path(), "demo", 1, "explorer", body1)).expect("first write");
    assert!(p1.exists(), "file should exist after first write");
    assert_eq!(fs::read_to_string(&p1).unwrap(), body1);

    // No leftover `.tmp.<pid>` siblings.
    let parent = p1.parent().unwrap();
    let leftovers: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no .tmp.<pid> leftovers; saw {leftovers:?}"
    );

    let p2 = write_handoff(&opts(td.path(), "demo", 1, "explorer", body2)).expect("overwrite");
    assert_eq!(p1, p2, "same (slug, stage, role) → same path");
    assert_eq!(
        fs::read_to_string(&p2).unwrap(),
        body2,
        "second write should overwrite"
    );
}

#[test]
fn list_handoffs_sorted_by_stage_then_role() {
    let td = TempDir::new().unwrap();
    // Write out-of-order on purpose.
    write_handoff(&opts(td.path(), "demo", 3, "fixer", "x")).unwrap();
    write_handoff(&opts(td.path(), "demo", 1, "fixer", "x")).unwrap();
    write_handoff(&opts(td.path(), "demo", 2, "explorer", "x")).unwrap();
    write_handoff(&opts(td.path(), "demo", 1, "explorer", "x")).unwrap();

    let listed = list_handoffs(td.path(), "demo").unwrap();
    let names: Vec<String> = listed
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "stage-1-explorer.md",
            "stage-1-fixer.md",
            "stage-2-explorer.md",
            "stage-3-fixer.md",
        ]
    );
}

#[test]
fn read_concat_returns_last_n_in_chronological_order() {
    let td = TempDir::new().unwrap();
    for n in 1..=5 {
        write_handoff(&opts(
            td.path(),
            "demo",
            n,
            "explorer",
            &format!("body-{n}\n"),
        ))
        .unwrap();
    }

    let blob = read_concat(td.path(), "demo", 3).unwrap();
    // Should contain stages 3, 4, 5 — ascending — and NOT 1 / 2.
    assert!(blob.contains("body-3"));
    assert!(blob.contains("body-4"));
    assert!(blob.contains("body-5"));
    assert!(!blob.contains("body-1"));
    assert!(!blob.contains("body-2"));

    // Chronological: stage 3 marker comes BEFORE stage 5 marker.
    let pos_3 = blob.find("stage-3-explorer.md").expect("marker 3");
    let pos_5 = blob.find("stage-5-explorer.md").expect("marker 5");
    assert!(pos_3 < pos_5, "ascending order: 3 before 5");
}

#[test]
fn read_concat_graceful_when_no_handoffs() {
    let td = TempDir::new().unwrap();
    // No handoffs written yet → graceful empty string.
    let blob = read_concat(td.path(), "demo", 3).unwrap();
    assert_eq!(blob, "");

    // last_n == 0 short-circuit.
    write_handoff(&opts(td.path(), "demo", 1, "explorer", "x")).unwrap();
    let blob = read_concat(td.path(), "demo", 0).unwrap();
    assert_eq!(blob, "");
}

#[test]
fn path_escape_blocks_traversal_and_special_chars() {
    let td = TempDir::new().unwrap();
    // workflow_slug with traversal attempts + special chars.
    let p = handoff_path(td.path(), "../../etc", 1, "explorer");
    let canonical = handoffs_dir(td.path(), "demo");
    let _ = canonical; // (only used to confirm dir layout test below)

    // The written file must stay under <td>/.ccteam/handoffs/.
    let written = write_handoff(&opts(
        td.path(),
        "../../etc/passwd",
        1,
        "explorer",
        "body\n",
    ))
    .expect("write should succeed with sanitized slug");
    let expected_root = td.path().join(".ccteam").join(HANDOFFS_DIRNAME);
    assert!(
        written.starts_with(&expected_root),
        "sanitized path must stay under handoffs root; got {written:?}, root {expected_root:?}"
    );
    // Also: handoff_path itself should produce a sanitized path.
    assert!(
        p.starts_with(&expected_root),
        "handoff_path must sanitize too; got {p:?}"
    );

    // Role with special chars also sanitized.
    let p2 = handoff_path(td.path(), "demo", 1, "evil/role:..");
    assert!(p2.starts_with(&expected_root));
    let fname = p2.file_name().unwrap().to_string_lossy().to_string();
    assert!(!fname.contains('/'), "filename should not contain '/'");
    assert!(!fname.contains(':'), "filename should not contain ':'");
}

#[test]
fn multiple_roles_same_stage_get_separate_files() {
    let td = TempDir::new().unwrap();
    write_handoff(&opts(td.path(), "demo", 1, "explorer", "explorer body\n")).unwrap();
    write_handoff(&opts(td.path(), "demo", 1, "fixer", "fixer body\n")).unwrap();
    write_handoff(&opts(td.path(), "demo", 1, "reviewer", "reviewer body\n")).unwrap();

    let listed = list_handoffs(td.path(), "demo").unwrap();
    assert_eq!(listed.len(), 3, "three files for three roles in stage 1");

    let blob = read_concat(td.path(), "demo", 10).unwrap();
    assert!(blob.contains("explorer body"));
    assert!(blob.contains("fixer body"));
    assert!(blob.contains("reviewer body"));
}

#[test]
fn template_has_expected_sections() {
    // Cheap sanity check on the canonical template — agents grep for
    // these section headers to author their own handoff.
    for section in &[
        "**Decided**",
        "**Rejected**",
        "**Risks**",
        "**Files changed**",
        "**Remaining**",
    ] {
        assert!(
            HANDOFF_TEMPLATE.contains(section),
            "template missing section {section}"
        );
    }
    // Stage / role placeholders for downstream `.replace()` callers.
    assert!(HANDOFF_TEMPLATE.contains("{{stage_num}}"));
    assert!(HANDOFF_TEMPLATE.contains("{{role}}"));
}
