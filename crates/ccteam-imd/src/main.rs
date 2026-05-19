//! `ccteam-imd` binary entry — thin clap shim over the library.

use std::path::PathBuf;

use anyhow::Result;
use ccteam_core::harness::AgentVendor;
use ccteam_imd::{
    daemon::{run_daemon, DaemonArgs},
    imd_heartbeat_path, list_bots, register_bot, unregister_bot, wait_for_health, HealthResult,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ccteam-imd",
    version,
    about = "V0.6.0 Wave 2 — IM-bot daemon (Telegram / Slack / Discord) + per-bot tmux supervisor"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (foreground only; systemd / launchd handle
    /// backgrounding via the bundled `systemd/ccteam-imd.service`
    /// unit).
    Run {
        /// Override the credentials file location (default
        /// `~/.ccteam/im/credentials.json`).
        #[arg(long, value_name = "PATH", env = "CCTEAM_IMD_CREDENTIALS")]
        credentials: Option<PathBuf>,
        /// Override the registry directory (default
        /// `~/.ccteam/imd/registry/`).
        #[arg(long, value_name = "PATH", env = "CCTEAM_IMD_REGISTRY")]
        registry: Option<PathBuf>,
        /// Supervisor poll interval in seconds (default 5).
        #[arg(long, default_value_t = 5)]
        tick_seconds: u64,
        /// Run for at most N seconds, then exit cleanly. `0` = run
        /// forever (the production default). Used by hermetic tests to
        /// keep the daemon from hanging the harness.
        #[arg(long, default_value_t = 0)]
        max_seconds: u64,
        /// Reserved for forward-compat with `ccteam start`'s flag set;
        /// `--foreground` is currently the only supported mode.
        #[arg(long, default_value_t = true)]
        foreground: bool,
    },
    /// Register a bot (creator skill calls this).
    Register {
        /// `workflow.yaml`'s `name` (project slug).
        #[arg(long)]
        slug: String,
        /// Role name within the workflow.
        #[arg(long)]
        role: String,
        /// Harness vendor: `claude` or `codex`.
        #[arg(long, value_parser = parse_vendor)]
        vendor: AgentVendor,
        /// IM platform: `telegram` | `slack` | `discord` | `mock`.
        #[arg(long)]
        platform: String,
        /// Platform-specific chat id (telegram chat_id, slack channel
        /// id, discord channel id).
        #[arg(long)]
        chat_id: String,
    },
    /// Unregister a bot (idempotent — missing file = success).
    Unregister {
        /// Project slug.
        #[arg(long)]
        slug: String,
        /// Role name.
        #[arg(long)]
        role: String,
    },
    /// Print the daemon status (heartbeat freshness + registered bots).
    Status,
    /// V0.6.1 F119 — block until the daemon publishes a *fresh*
    /// heartbeat (mtime ≥ the moment this command started), or the
    /// timeout elapses.
    ///
    /// `exit 0` = ready; `exit 1` = timed out. Scripted callers like
    /// `scripts/host-probe/run-probes.sh` use this to wait after
    /// spawning the daemon before exercising mode-3 scenarios.
    Health {
        /// Maximum seconds to wait for a fresh heartbeat (default 30).
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
        /// Poll interval in milliseconds between filesystem checks
        /// (default 200ms; tests may shorten).
        #[arg(long, default_value_t = 200)]
        poll_ms: u64,
    },
}

fn parse_vendor(s: &str) -> Result<AgentVendor, String> {
    match s {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        other => Err(format!("unknown vendor `{other}` (want claude|codex)")),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            credentials,
            registry,
            tick_seconds,
            max_seconds,
            foreground: _,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(run_daemon(DaemonArgs {
                credentials,
                registry,
                tick: std::time::Duration::from_secs(tick_seconds.max(1)),
                max_runtime: if max_seconds == 0 {
                    None
                } else {
                    Some(std::time::Duration::from_secs(max_seconds))
                },
                adapter_factory: None,
            }))?;
        }
        Command::Register {
            slug,
            role,
            vendor,
            platform,
            chat_id,
        } => {
            let path = register_bot(&slug, &role, vendor, &platform, &chat_id)?;
            println!("registered: {}", path.display());
        }
        Command::Unregister { slug, role } => {
            unregister_bot(&slug, &role)?;
            println!("unregistered {slug}/{role}");
        }
        Command::Status => {
            let hb = imd_heartbeat_path();
            let age = std::fs::metadata(&hb)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs());
            match age {
                Some(secs) if secs < 60 => println!("daemon: alive (heartbeat {secs}s ago)"),
                Some(secs) => println!("daemon: STALE (heartbeat {secs}s ago)"),
                None => println!("daemon: not running (no heartbeat at {})", hb.display()),
            }
            for reg in list_bots()? {
                println!(
                    "  {}/{} → {} (chat_id={})",
                    reg.workflow_slug, reg.role, reg.im_platform, reg.im_chat_id
                );
            }
        }
        Command::Health {
            timeout_seconds,
            poll_ms,
        } => {
            let started = std::time::SystemTime::now();
            let result = wait_for_health(
                started,
                std::time::Duration::from_secs(timeout_seconds),
                std::time::Duration::from_millis(poll_ms),
            );
            match result {
                HealthResult::Ready => {
                    let hb = imd_heartbeat_path();
                    println!("daemon: ready (heartbeat at {})", hb.display());
                }
                HealthResult::Timeout => {
                    let hb = imd_heartbeat_path();
                    eprintln!(
                        "daemon: NOT READY after {timeout_seconds}s (no fresh heartbeat at {})",
                        hb.display()
                    );
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}
