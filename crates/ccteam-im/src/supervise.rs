//! Panic containment for the daemon's long-lived, single-instance tasks.
//!
//! A task of which there is exactly one — the gateway event consumer, the
//! inbound consumer, the delegation notifier, the body watcher, the
//! schedulers and sweepers — fails GLOBALLY when it dies: every session
//! loses what it does, permanently, and a panic message can be lost with it
//! (issue #210: the event consumer died while the log disk was full and no
//! reply was delivered again). Two shapes keep such a task alive:
//!
//! - [`contain_panic`]: a queue consumer runs each item's work through it, so
//!   one bad item costs that item and the loop keeps its state and receiver.
//! - [`supervise_loop`]: a loop with no state worth keeping across a panic
//!   (a periodic tick) is restarted after a short backoff.

use std::future::Future;
use std::time::Duration;

use futures::FutureExt;

/// How a panicked loop is restarted: after `initial`, doubling for each
/// panic in a row up to `max`, so a loop that panics on every pass neither
/// spins nor floods the log (#210 began with a full log disk). A run that
/// lasted `healthy` before panicking starts the doubling over.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    initial: Duration,
    max: Duration,
    healthy: Duration,
}

const RESTART_BACKOFF: Backoff = Backoff {
    initial: Duration::from_secs(1),
    max: Duration::from_secs(60),
    healthy: Duration::from_secs(300),
};

/// Run one item of `task`'s work; a panic is logged at error (with `item`
/// naming what was being processed) and swallowed. `None` = it panicked.
pub async fn contain_panic<F: Future>(task: &str, item: &str, work: F) -> Option<F::Output> {
    match std::panic::AssertUnwindSafe(work).catch_unwind().await {
        Ok(output) => Some(output),
        Err(panic) => {
            tracing::error!(
                task,
                item,
                panic = panic_message(panic.as_ref()),
                "ccteam-im: a long-lived task panicked on one item; item dropped, task continues"
            );
            None
        }
    }
}

/// Run the loop `make` builds until it returns, restarting it after a panic.
/// A loop that returns on its own is logged at error: every caller's loop is
/// meant to live as long as the daemon, and ends only by being aborted.
pub async fn supervise_loop<F, Fut>(task: &'static str, make: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    supervise_loop_with_backoff(task, RESTART_BACKOFF, make).await;
}

async fn supervise_loop_with_backoff<F, Fut>(task: &'static str, policy: Backoff, mut make: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut backoff = policy.initial;
    loop {
        let started = tokio::time::Instant::now();
        match std::panic::AssertUnwindSafe(make()).catch_unwind().await {
            Ok(()) => {
                tracing::error!(task, "ccteam-im: a long-lived task exited");
                return;
            }
            Err(panic) => {
                if started.elapsed() >= policy.healthy {
                    backoff = policy.initial;
                }
                tracing::error!(
                    task,
                    panic = panic_message(panic.as_ref()),
                    restart_in_ms = backoff.as_millis() as u64,
                    "ccteam-im: a long-lived task panicked; restarting it"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(policy.max);
            }
        }
    }
}

/// The text of a panic payload (`panic!` with a literal or a formatted
/// message), for the log line.
pub fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn contain_panic_returns_the_output_or_none() {
        assert_eq!(contain_panic("t", "i", async { 7 }).await, Some(7));
        let panicked: Option<()> = contain_panic("t", "i", async { panic!("boom") }).await;
        assert_eq!(panicked, None);
    }

    const FAST: Backoff = Backoff {
        initial: Duration::from_millis(1),
        max: Duration::from_millis(1),
        healthy: Duration::from_secs(300),
    };

    /// A loop that panics is run again (here: twice), and the supervisor
    /// returns once the loop itself returns.
    #[tokio::test]
    async fn supervise_loop_restarts_a_panicked_loop() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        supervise_loop_with_backoff("t", FAST, move || {
            let counted = Arc::clone(&counted);
            async move {
                if counted.fetch_add(1, Ordering::SeqCst) < 2 {
                    panic!("loop pass {}", counted.load(Ordering::SeqCst));
                }
            }
        })
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    /// The pauses between restarts of the production policy: doubling from
    /// 1s, capped at 60s, and back to 1s after a run that stayed up for the
    /// healthy window. Paused clock: the sleeps cost no wall time.
    #[tokio::test(start_paused = true)]
    async fn restart_backoff_doubles_to_a_cap_and_resets_after_a_healthy_run() {
        let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&starts);
        let healthy = RESTART_BACKOFF.healthy;
        supervise_loop_with_backoff("t", RESTART_BACKOFF, move || {
            let recorded = Arc::clone(&recorded);
            async move {
                let run = {
                    let mut starts = recorded.lock().unwrap();
                    starts.push(tokio::time::Instant::now());
                    starts.len()
                };
                match run {
                    // Run 9 stays up for the healthy window, then panics.
                    9 => {
                        tokio::time::sleep(healthy).await;
                        panic!("late")
                    }
                    11 => {}
                    _ => panic!("pass {run}"),
                }
            }
        })
        .await;
        let starts = starts.lock().unwrap();
        let gaps: Vec<u64> = starts
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).as_secs())
            .collect();
        let up = healthy.as_secs();
        assert_eq!(gaps, vec![1, 2, 4, 8, 16, 32, 60, 60, up + 1, 2]);
    }

    #[test]
    fn panic_message_reads_str_and_string_payloads() {
        let lit: Box<dyn std::any::Any + Send> = Box::new("lit");
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("owned"));
        let other: Box<dyn std::any::Any + Send> = Box::new(3u8);
        assert_eq!(panic_message(lit.as_ref()), "lit");
        assert_eq!(panic_message(owned.as_ref()), "owned");
        assert_eq!(panic_message(other.as_ref()), "<non-string panic payload>");
    }
}
