//! V0.6.0 Wave 2 F114 — verifies the persona prefab library shipped
//! under `skills/ccteam-creator/personas/` is well-formed:
//!
//! - `manifest.toml` parses
//! - every persona id in the manifest has both `zh/role.md` and
//!   `en/role.md` files
//! - each role.md has YAML frontmatter (delimited by `---`)
//! - ≥7 personas exist (PRD F114 verification #4)

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // crates/ccteam-core → repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn personas_dir() -> PathBuf {
    repo_root().join("skills/ccteam-creator/personas")
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    persona: Vec<PersonaEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct PersonaEntry {
    id: String,
    label_en: String,
    label_zh: String,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    default_mode: String,
    #[serde(default)]
    codex_eligible: bool,
}

fn read_manifest() -> Manifest {
    let path = personas_dir().join("manifest.toml");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    toml::from_str(&body).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[test]
fn manifest_parses() {
    let m = read_manifest();
    assert!(!m.persona.is_empty());
}

#[test]
fn at_least_seven_personas() {
    let m = read_manifest();
    assert!(
        m.persona.len() >= 7,
        "PRD F114 requires ≥7 personas, got {}",
        m.persona.len()
    );
}

#[test]
fn every_persona_has_zh_and_en_role_files() {
    let m = read_manifest();
    let base = personas_dir();
    for p in &m.persona {
        let zh = base.join(&p.id).join("zh/role.md");
        let en = base.join(&p.id).join("en/role.md");
        assert!(zh.exists(), "missing {zh:?} for persona {}", p.id);
        assert!(en.exists(), "missing {en:?} for persona {}", p.id);
    }
}

#[test]
fn every_role_md_has_frontmatter() {
    let m = read_manifest();
    let base = personas_dir();
    for p in &m.persona {
        for lang in ["zh", "en"] {
            let path = base.join(&p.id).join(lang).join("role.md");
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                body.starts_with("---\n"),
                "role.md missing frontmatter delimiter: {path:?}"
            );
            // Closing `---` on its own line somewhere after the opener.
            let after_opener = &body[4..];
            assert!(
                after_opener.contains("\n---\n"),
                "role.md missing closing frontmatter delimiter: {path:?}"
            );
        }
    }
}

#[test]
fn manifest_default_modes_are_valid() {
    let m = read_manifest();
    let valid: std::collections::HashSet<&'static str> =
        ["in-proc", "bg", "chat-dm", "chat-group"].into_iter().collect();
    for p in &m.persona {
        assert!(
            valid.contains(p.default_mode.as_str()),
            "persona {} has invalid default_mode {:?}",
            p.id,
            p.default_mode
        );
        assert!(!p.label_en.is_empty(), "persona {} missing label_en", p.id);
        assert!(!p.label_zh.is_empty(), "persona {} missing label_zh", p.id);
        assert!(
            !p.description.is_empty(),
            "persona {} missing description",
            p.id
        );
        // touch the rest so dead_code lint doesn't fire
        let _ = (&p.tags, p.codex_eligible);
    }
}
