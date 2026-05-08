//! V0.2 M0.22 — `ccteam team {init,publish}` CLI handlers.
//!
//! `init` is intentionally lightweight: it stamps a single-phase
//! starter staging tree (`<staging>/.claude-plugin/plugin.json` +
//! `team.yaml` + `phases/01-intake.md` + `README.md`). The team author
//! then either edits the staging files directly or runs the meta-agent
//! `ccteam-team-author` skill, which interview-mode produces the same
//! tree with N phases instead of one.
//!
//! `publish` calls `ccteam_core::publish_team` for the local target.
//! The github target shells out to `gh repo create` + git push so
//! the user's gh CLI authentication is the trust anchor — the factory
//! does not embed any credentials.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use ccteam_core::{
    init_team_staging, publish_team, staging_dir_for, validate_staged_team, InitReport,
    PhaseScaffold, PluginAuthor, PluginManifest, PublishInput, PublishReport, PublishTarget,
    TeamInitInput, TeamSpec,
};

/// `ccteam team init` arguments parsed from clap.
#[derive(Debug, Clone)]
pub struct TeamInitArgs {
    pub name: String,
    pub description: String,
    pub author_name: String,
    pub author_email: Option<String>,
    pub version: Option<String>,
}

/// `ccteam team publish` arguments parsed from clap.
#[derive(Debug, Clone)]
pub struct TeamPublishArgs {
    pub name: String,
    pub target: PublishTargetArg,
    pub repo: Option<String>,
}

/// CLI surface for the publish target. Keeps the clap enum decoupled
/// from `ccteam_core::PublishTarget` so the core type can hold target-
/// specific data without leaking into clap derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PublishTargetArg {
    Local,
    Github,
}

/// Run `ccteam team init <name>`. Writes the staging tree under
/// `~/.config/ccteam/teams/<name>/` (or `$XDG_CONFIG_HOME/ccteam/...`).
/// Returns a human-readable report so the binary entry point can
/// print it.
pub fn run_team_init(args: &TeamInitArgs) -> Result<String> {
    let mut spec_yaml = format!("name: {}\n", args.name);
    if !args.description.is_empty() {
        spec_yaml.push_str(&format!("description: {}\n", args.description));
    }
    spec_yaml.push_str("phase_dir: phases\n");
    let spec = TeamSpec::parse(&spec_yaml)
        .with_context(|| format!("synthesize TeamSpec from `--name {}`", args.name))?;

    let manifest = PluginManifest {
        name: args.name.clone(),
        description: args.description.clone(),
        author: PluginAuthor {
            name: args.author_name.clone(),
            email: args.author_email.clone(),
        },
        version: args.version.clone(),
    };

    // Single starter phase. The team-author skill produces multi-phase
    // teams via N init invocations / direct edit; the CLI default
    // keeps the surface small and predictable.
    let phases = vec![PhaseScaffold {
        name: "intake",
        task_summary: "interview the user, write `.ccteam/spec.md`.",
        required_inputs: &[],
        required_outputs: &[".ccteam/spec.md"],
        auto_loop: true,
    }];

    let report: InitReport = init_team_staging(&TeamInitInput {
        spec: &spec,
        manifest: &manifest,
        phases: &phases,
        staging_root_override: None,
    })?;
    Ok(render_init_report(&report))
}

fn render_init_report(report: &InitReport) -> String {
    let mut out = String::new();
    out.push_str("ccteam team init\n\n");
    out.push_str(&format!("  staging dir   {}\n", report.staging_dir.display()));
    out.push_str(&format!(
        "  manifest      {}\n",
        report.manifest_path.display(),
    ));
    out.push_str(&format!(
        "  team yaml     {}\n",
        report.team_yaml_path.display(),
    ));
    out.push_str(&format!(
        "  phases ({})  ", report.phase_paths.len(),
    ));
    out.push('\n');
    for path in &report.phase_paths {
        out.push_str(&format!("                {}\n", path.display()));
    }
    out.push_str(&format!("  README        {}\n\n", report.readme_path.display()));
    out.push_str(
        "next:\n  \
         1. edit phases/*.md bodies to fill in domain detail.\n  \
         2. ccteam doctor --validate-team <name> — checks plugin manifest + team.yaml.\n  \
         3. ccteam team publish <name> --target local|github\n",
    );
    out
}

/// Run `ccteam team publish <name>`. For `--target github`, this
/// validates staging + repo coordinate, then shells out to `gh repo
/// create` + git plumbing. For `--target local`, it delegates to
/// `ccteam_core::publish_team` (which symlinks staging into the
/// ccteam-local marketplace).
pub fn run_team_publish(args: &TeamPublishArgs) -> Result<String> {
    let staging = staging_dir_for(&args.name, None);
    let findings = validate_staged_team(&staging)?;
    let mut out = String::new();
    out.push_str("ccteam team publish\n\n");
    for line in &findings {
        out.push_str(&format!("  {line}\n"));
    }
    if findings.iter().any(|l| l.starts_with("[FAIL]")) {
        bail!(
            "ccteam team publish: validation failed — fix the [FAIL] lines above and re-run.",
        );
    }
    match args.target {
        PublishTargetArg::Local => {
            let report = publish_team(&PublishInput {
                team_name: &args.name,
                target: PublishTarget::Local,
                staging_root_override: None,
                claude_dir_override: None,
            })?;
            out.push_str(&render_publish_report(&report));
        }
        PublishTargetArg::Github => {
            let repo = args.repo.as_deref().ok_or_else(|| {
                anyhow!(
                    "ccteam team publish --target github requires --repo <owner>/<name>",
                )
            })?;
            // Ensure the staging dir is otherwise OK before any network
            // side-effects. Then shell out.
            let _ = publish_team(&PublishInput {
                team_name: &args.name,
                target: PublishTarget::Github {
                    repo: repo.to_string(),
                },
                staging_root_override: None,
                claude_dir_override: None,
            })?;
            let url = github_publish_via_gh(&staging, repo)
                .with_context(|| format!("publish staged team to github repo `{repo}`"))?;
            out.push_str(&format!("  pushed → {url}\n"));
            out.push_str(&format!(
                "\nshare with: claude /plugin add {repo}\n",
            ));
        }
    }
    Ok(out)
}

fn render_publish_report(report: &PublishReport) -> String {
    let mut out = String::new();
    if let Some(link) = &report.local_link {
        out.push_str(&format!("  linked → {}\n", link.display()));
        out.push_str(
            "\nshare with: tell the user the staging dir path; on their machine they install via\n  \
             claude /plugin add <staging-path>\n  \
             or copy/git-clone the staging tree into their ccteam-local marketplace.\n",
        );
    }
    if let Some(url) = &report.github_url {
        out.push_str(&format!("  pushed → {url}\n"));
    }
    out
}

/// Shell out to `gh` + `git` to create a GitHub repo and push the
/// staging tree to it. Fail-loud (per task spec) when `gh` is missing
/// / unauthenticated — the caller should see a clear pointer to
/// `gh auth login`. Returns the resulting `https://github.com/<repo>`
/// URL.
fn github_publish_via_gh(staging: &Path, repo: &str) -> Result<String> {
    if !command_exists("gh") {
        bail!(
            "gh CLI not found on PATH — install https://cli.github.com/ then run `gh auth login`",
        );
    }
    let auth_status = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("invoke gh auth status")?;
    if !auth_status.status.success() {
        let stderr = String::from_utf8_lossy(&auth_status.stderr);
        bail!(
            "gh CLI is not authenticated; run `gh auth login` first.\n\
             gh stderr:\n{stderr}",
        );
    }

    // Ensure git history exists in the staging dir. The factory does
    // not init git itself (staging may live alongside other ccteam
    // metadata not part of the team plugin); we run `git init` here
    // idempotently.
    run_in(staging, "git", &["init", "-q"])?;
    run_in(staging, "git", &["add", "-A"])?;
    // `git commit` exits non-zero if the tree is clean; tolerate that.
    let _ = Command::new("git")
        .current_dir(staging)
        .args(["commit", "-q", "-m", "initial team plugin commit"])
        .status();

    // Create the repo. `--source .` would push the cwd; we keep
    // create + push separate so the failure mode (already exists,
    // permission denied) is easier to attribute.
    let create = Command::new("gh")
        .current_dir(staging)
        .args([
            "repo",
            "create",
            repo,
            "--public",
            "--source",
            ".",
            "--remote",
            "origin",
            "--push",
        ])
        .output()
        .context("invoke gh repo create")?;
    if !create.status.success() {
        let stderr = String::from_utf8_lossy(&create.stderr);
        bail!(
            "gh repo create failed.\nstderr:\n{stderr}",
        );
    }
    Ok(format!("https://github.com/{repo}"))
}

fn run_in(dir: &Path, cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd)
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("invoke {cmd} {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`{} {}` exited {}: {stderr}",
            cmd,
            args.join(" "),
            output.status,
        );
    }
    Ok(())
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[allow(dead_code)] // V0.3 hook — surfaced as a public helper for tests.
pub fn staging_path_for(name: &str) -> PathBuf {
    staging_dir_for(name, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_init_report_includes_staging_dir() {
        let report = InitReport {
            staging_dir: PathBuf::from("/tmp/x"),
            manifest_path: PathBuf::from("/tmp/x/.claude-plugin/plugin.json"),
            team_yaml_path: PathBuf::from("/tmp/x/team.yaml"),
            phase_paths: vec![PathBuf::from("/tmp/x/phases/01-intake.md")],
            readme_path: PathBuf::from("/tmp/x/README.md"),
        };
        let body = render_init_report(&report);
        assert!(body.contains("/tmp/x"));
        assert!(body.contains("01-intake.md"));
        assert!(body.contains("validate-team"));
    }

    #[test]
    fn render_publish_report_local_includes_link_and_share_hint() {
        let report = PublishReport {
            target: PublishTarget::Local,
            local_link: Some(PathBuf::from("/tmp/link")),
            github_url: None,
        };
        let body = render_publish_report(&report);
        assert!(body.contains("linked"));
        assert!(body.contains("/plugin add"));
    }

    #[test]
    fn render_publish_report_github_includes_url() {
        let report = PublishReport {
            target: PublishTarget::Github {
                repo: "alice/example".into(),
            },
            local_link: None,
            github_url: Some("https://github.com/alice/example".into()),
        };
        let body = render_publish_report(&report);
        assert!(body.contains("alice/example"));
        assert!(body.contains("https://"));
    }
}
