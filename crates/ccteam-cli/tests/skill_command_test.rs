//! Deterministic end-to-end coverage for the user-level `ccteam skill` CLI.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SKILL_BODY: &str =
    "---\nname: helper-skill\ndescription: Helps with things\n---\nUse the helper.\n";
const AGENT_BODY: &str = "---\nname: helper-agent\n---\nYou are a helper.\n";

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn spawn_fake_hub() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let index = format!(
        r#"{{
          "version": 1,
          "name": "test-hub",
          "description": "test",
          "generated_at": "2026-07-24T00:00:00Z",
          "plugins": [
            {{
              "id": "helper-skill",
              "type": "skill",
              "name": "Helper Skill",
              "description": "Helps with things",
              "upstream": "{base}/skills/helper-skill/SKILL.md",
              "content_sha": "{}",
              "source": "ccteam",
              "license": "MIT",
              "tags": ["helper"]
            }},
            {{
              "id": "helper-agent",
              "type": "agent",
              "name": "Helper Agent",
              "description": "An agent, not a skill",
              "upstream": "{base}/agents/helper-agent.md",
              "content_sha": "{}",
              "source": "agency-agents",
              "license": "MIT",
              "tags": ["helper"]
            }}
          ]
        }}"#,
        sha256_hex(SKILL_BODY.as_bytes()),
        sha256_hex(AGENT_BODY.as_bytes())
    );
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut request = [0u8; 8192];
        let n = stream.read(&mut request).unwrap_or(0);
        let path = String::from_utf8_lossy(&request[..n])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let body = match path.as_str() {
            "/index.json" => Some(index.as_str()),
            "/skills/helper-skill/SKILL.md" => Some(SKILL_BODY),
            "/agents/helper-agent.md" => Some(AGENT_BODY),
            _ => None,
        };
        let response = match body {
            Some(body) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
            None => {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            }
        };
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    base
}

struct Sandbox {
    _tmp: TempDir,
    home: PathBuf,
    ccteam_home: PathBuf,
    projects_root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let ccteam_home = tmp.path().join("ccteam-home");
        let projects_root = tmp.path().join("projects");
        let repo = tmp.path().join("repo");
        for dir in [&home, &ccteam_home, &projects_root, &repo] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Self {
            _tmp: tmp,
            home,
            ccteam_home,
            projects_root,
            repo,
        }
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(ccteam_bin());
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", &self.projects_root);
        command
    }

    fn hub_cmd(&self, hub: &str) -> Command {
        let mut command = self.cmd();
        command.env("CCTEAM_HUB_BASE", hub);
        command
    }

    fn skills(&self) -> PathBuf {
        self.ccteam_home.join("skills")
    }
}

fn assert_success(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn skill_search_filters_catalog_and_role_add_refuses_skills() {
    let sandbox = Sandbox::new();
    let hub = spawn_fake_hub();
    let search = sandbox
        .hub_cmd(&hub)
        .args(["skill", "search", "helper"])
        .output()
        .unwrap();
    assert_success(&search, "skill search");
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert!(stdout.contains("helper-skill"));
    assert!(!stdout.contains("helper-agent"));
    assert!(stdout.contains("ccteam skill add"));

    let role_add = sandbox
        .hub_cmd(&hub)
        .args(["role", "add", "helper-skill"])
        .output()
        .unwrap();
    assert!(!role_add.status.success());
    assert!(
        String::from_utf8_lossy(&role_add.stderr).contains("use: ccteam skill add helper-skill")
    );

    let wrong_type = sandbox
        .hub_cmd(&hub)
        .args(["skill", "add", "helper-agent"])
        .output()
        .unwrap();
    assert!(!wrong_type.status.success());
    assert!(
        String::from_utf8_lossy(&wrong_type.stderr).contains("use: ccteam role add helper-agent")
    );
}

#[test]
fn skill_add_and_update_touch_only_the_library() {
    let sandbox = Sandbox::new();
    let hub = spawn_fake_hub();
    let add = sandbox
        .hub_cmd(&hub)
        .args(["skill", "add", "helper-skill"])
        .output()
        .unwrap();
    assert_success(&add, "skill add");
    let installed = sandbox.skills().join("helper-skill/SKILL.md");
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), SKILL_BODY);
    assert!(std::fs::read_dir(&sandbox.repo).unwrap().next().is_none());

    std::fs::write(&installed, "stale\n").unwrap();
    let update = sandbox
        .hub_cmd(&hub)
        .args(["skill", "update", "helper-skill"])
        .output()
        .unwrap();
    assert_success(&update, "skill update");
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), SKILL_BODY);

    std::fs::write(&installed, "stale again\n").unwrap();
    let update_all = sandbox
        .hub_cmd(&hub)
        .args(["skill", "update", "--all"])
        .output()
        .unwrap();
    assert_success(&update_all, "skill update --all");
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), SKILL_BODY);
    assert!(String::from_utf8_lossy(&update_all.stdout).contains("1 updated"));
    assert!(std::fs::read_dir(&sandbox.repo).unwrap().next().is_none());
}

#[test]
fn skill_ls_and_rm_handle_nested_skills_and_source_trees() {
    let sandbox = Sandbox::new();
    let nested = sandbox.skills().join("pack/tool/SKILL.md");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, "---\ndescription: nested\n---\nbody\n").unwrap();

    let list = sandbox
        .cmd()
        .args(["skill", "ls", "--json"])
        .output()
        .unwrap();
    assert_success(&list, "skill ls --json");
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value[0]["id"], "pack/tool");

    let remove = sandbox
        .cmd()
        .args(["skill", "rm", "pack/tool"])
        .output()
        .unwrap();
    assert_success(&remove, "skill rm nested skill");
    assert!(!nested.parent().unwrap().exists());

    let tree_skill = sandbox.skills().join("source-tree/child/SKILL.md");
    std::fs::create_dir_all(tree_skill.parent().unwrap()).unwrap();
    std::fs::write(&tree_skill, "body\n").unwrap();
    let refused = sandbox
        .cmd()
        .args(["skill", "rm", "source-tree"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("skill source rm"));
    let forced = sandbox
        .cmd()
        .args(["skill", "rm", "source-tree", "--force"])
        .output()
        .unwrap();
    assert_success(&forced, "skill rm tree --force");
    assert!(!sandbox.skills().join("source-tree").exists());
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output, &format!("git {}", args.join(" ")));
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", message]);
}

#[test]
fn skill_source_local_git_add_update_list_and_remove() {
    let sandbox = Sandbox::new();
    let upstream = sandbox._tmp.path().join("upstream");
    std::fs::create_dir_all(upstream.join("pack/tool")).unwrap();
    git(&upstream, &["init"]);
    git(&upstream, &["config", "user.email", "test@example.com"]);
    git(&upstream, &["config", "user.name", "Test User"]);
    std::fs::write(upstream.join("pack/tool/SKILL.md"), "version one\n").unwrap();
    commit_all(&upstream, "initial");

    let add = sandbox
        .cmd()
        .args(["skill", "source", "add", upstream.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&add, "skill source add local git");
    let cloned = sandbox.skills().join("upstream/pack/tool/SKILL.md");
    assert_eq!(std::fs::read_to_string(&cloned).unwrap(), "version one\n");
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sandbox.skills().join(".sources.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["upstream"]["kind"], "git");

    std::fs::write(upstream.join("pack/tool/SKILL.md"), "version two\n").unwrap();
    commit_all(&upstream, "update");
    let update = sandbox
        .cmd()
        .args(["skill", "source", "update", "upstream"])
        .output()
        .unwrap();
    assert_success(&update, "skill source update");
    assert_eq!(std::fs::read_to_string(&cloned).unwrap(), "version two\n");

    let list = sandbox
        .cmd()
        .args(["skill", "source", "ls", "--json"])
        .output()
        .unwrap();
    assert_success(&list, "skill source ls --json");
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(listed["upstream"]["kind"], "git");

    let remove = sandbox
        .cmd()
        .args(["skill", "source", "rm", "upstream"])
        .output()
        .unwrap();
    assert_success(&remove, "skill source rm");
    assert!(!sandbox.skills().join("upstream").exists());
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sandbox.skills().join(".sources.json")).unwrap())
            .unwrap();
    assert!(metadata.as_object().unwrap().is_empty());
}

#[test]
fn skill_source_plain_path_is_copied_once_and_self_managed() {
    let sandbox = Sandbox::new();
    let source = sandbox._tmp.path().join("plain-source");
    std::fs::create_dir_all(source.join("tool")).unwrap();
    std::fs::write(source.join("tool/SKILL.md"), "path source\n").unwrap();

    let add = sandbox
        .cmd()
        .args(["skill", "source", "add", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&add, "skill source add path");
    let update = sandbox
        .cmd()
        .args(["skill", "source", "update", "plain-source"])
        .output()
        .unwrap();
    assert_success(&update, "skill source update path");
    assert!(String::from_utf8_lossy(&update.stdout).contains("self-managed"));
}

#[cfg(unix)]
fn assert_project_skill_link(project: &Path) {
    let link = project.join(".claude/skills");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_link(link).unwrap(),
        PathBuf::from("../.agents/skills")
    );
}

#[cfg(unix)]
#[test]
fn skill_ensure_and_migrate_project_cover_all_face_states() {
    let sandbox = Sandbox::new();

    let absent = sandbox.projects_root.join("absent");
    std::fs::create_dir_all(&absent).unwrap();
    let ensure = sandbox
        .cmd()
        .args(["skill", "ensure-project", "--project", "absent"])
        .output()
        .unwrap();
    assert_success(&ensure, "ensure project absent face");
    assert!(absent.join(".agents/skills").is_dir());
    assert_project_skill_link(&absent);
    let again = sandbox
        .cmd()
        .args(["skill", "ensure-project", "--project", "absent"])
        .output()
        .unwrap();
    assert_success(&again, "ensure project correct symlink");

    let empty = sandbox.projects_root.join("empty");
    std::fs::create_dir_all(empty.join(".claude/skills")).unwrap();
    let replace = sandbox
        .cmd()
        .args(["skill", "ensure-project", "--project", "empty"])
        .output()
        .unwrap();
    assert_success(&replace, "ensure project empty legacy face");
    assert_project_skill_link(&empty);

    let legacy = sandbox.projects_root.join("legacy");
    let old_skill = legacy.join(".claude/skills/old-skill");
    std::fs::create_dir_all(&old_skill).unwrap();
    std::fs::write(old_skill.join("SKILL.md"), "legacy\n").unwrap();
    let refused = sandbox
        .cmd()
        .args(["skill", "ensure-project", "--project", "legacy"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("migrate-project"));
    assert!(old_skill.is_dir());

    let migrate = sandbox
        .cmd()
        .args(["skill", "migrate-project", "--project", "legacy"])
        .output()
        .unwrap();
    assert_success(&migrate, "migrate project skills");
    assert_eq!(
        std::fs::read_to_string(legacy.join(".agents/skills/old-skill/SKILL.md")).unwrap(),
        "legacy\n"
    );
    assert_project_skill_link(&legacy);
    assert!(std::fs::read_dir(sandbox.skills()).is_err());
}
