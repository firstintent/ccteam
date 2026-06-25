//! V0.2 M0.21 e2e: watchdog `scan` + `push_alert_to_meta_outbox`
//! exercise on a fully-laid-out project tree. Lives in `tests/` so it
//! runs in its own process — keeps any future env-touching tests
//! isolated from the lib's `#[cfg(test)] mod tests`.

use ccteam_core::{
    auto_loop::{self, AutoLoopState},
    push_watchdog_alert_to_meta_outbox, watchdog_scan, CcteamPaths, OutboxEventKind, OutboxMessage,
    OutboxPriority, ProjectState, WatchdogAlertKind, WatchdogConfig, WatchdogNotifyMode,
};
use tempfile::TempDir;

fn paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    }
}

fn write_state(p: &CcteamPaths, slug: &str, mutate: impl FnOnce(&mut ProjectState)) {
    let dir = p.project_ccteam_dir(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = ProjectState::initial(slug.to_string());
    mutate(&mut s);
    s.save(&p.project_state(slug)).unwrap();
}

#[cfg(unix)]
fn fake_daemon(p: &CcteamPaths) -> std::os::unix::net::UnixListener {
    let socket = ccteam_core::daemon_socket_path(p);
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::os::unix::net::UnixListener::bind(socket).unwrap()
}

#[test]
fn auto_loop_iteration_2_surfaces_alert_then_pushes_to_meta_outbox() {
    // Mirrors dev-plan §7 acceptance: "phase auto_loop cycle 第 2 次时,
    // meta-agent surface 一条用户可读通知".
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    let _daemon = fake_daemon(&p);

    let slug = "dev-bookmark";
    write_state(&p, slug, |s| {
        s.current_phase = "implement".into();
    });
    let alp = auto_loop::path_in(&p.project_dir(slug));
    let mut s = AutoLoopState::new(slug.into(), "fix tests".into(), 3, "PHASE_DONE".into());
    s.front.iteration = 2;
    auto_loop::write(&alp, &s).unwrap();

    let cfg = WatchdogConfig::default();
    let alerts = watchdog_scan(&p, &cfg).unwrap();
    let cycle: Vec<_> = alerts
        .iter()
        .filter(|a| a.kind == WatchdogAlertKind::AutoLoopCycle)
        .collect();
    assert_eq!(cycle.len(), 1);
    let alert = cycle[0];
    assert_eq!(alert.slug.as_deref(), Some(slug));
    assert!(alert.message.contains("2/3"));

    let path = push_watchdog_alert_to_meta_outbox(&p, alert).unwrap();
    let msg = OutboxMessage::load(&path).unwrap();
    // Cycle alerts are progress-priority (informational, not action-mandated).
    assert_eq!(msg.front.event_kind, OutboxEventKind::Progress);
    assert_eq!(msg.front.priority, OutboxPriority::Normal);
    assert!(msg.body.contains("auto_loop_cycle"));
    assert!(msg.body.contains(slug));
}

#[test]
fn quiet_mode_drops_cycle_alert_but_pushes_daemon_down_when_socket_unreachable() {
    // Mirrors dev-plan §7 acceptance: "用户改 watchdog.yaml notify_mode:
    // quiet → 通知不再 surface" — *except* the breaks-through-quiet
    // signals (cost / daemon down).
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    // Deliberately do NOT bind the MCP socket ⇒ daemon_down should fire.
    std::fs::create_dir_all(&p.projects_root).unwrap();

    let slug = "dev-bookmark";
    write_state(&p, slug, |s| s.current_phase = "implement".into());
    let alp = auto_loop::path_in(&p.project_dir(slug));
    let mut s = AutoLoopState::new(slug.into(), "fix".into(), 3, "PHASE_DONE".into());
    s.front.iteration = 3;
    auto_loop::write(&alp, &s).unwrap();

    let cfg = WatchdogConfig {
        notify_mode: WatchdogNotifyMode::Quiet,
        ..WatchdogConfig::default()
    };
    let alerts = watchdog_scan(&p, &cfg).unwrap();
    assert!(
        alerts
            .iter()
            .all(|a| a.kind != WatchdogAlertKind::AutoLoopCycle),
        "quiet mode must suppress auto_loop_cycle, got: {alerts:?}",
    );
    let daemon: Vec<_> = alerts
        .iter()
        .filter(|a| a.kind == WatchdogAlertKind::DaemonDown)
        .collect();
    assert_eq!(daemon.len(), 1, "daemon_down breaks through quiet");

    let path = push_watchdog_alert_to_meta_outbox(&p, daemon[0]).unwrap();
    let msg = OutboxMessage::load(&path).unwrap();
    assert_eq!(msg.front.event_kind, OutboxEventKind::Escalation);
    assert_eq!(msg.front.priority, OutboxPriority::High);
}

#[test]
fn watchdog_does_not_mutate_state_or_progress_jsonl() {
    // Sanity check the "translation only" red line: a full scan must
    // not touch state.json (mtime unchanged) or progress.jsonl
    // (file count unchanged).
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    let _daemon = fake_daemon(&p);
    std::fs::create_dir_all(p.root.join("state").join("progress")).unwrap();

    let slug = "dev-watch";
    write_state(&p, slug, |s| {
        // V0.4.6 F91 — `cost_used_usd` is deprecated. The watchdog scan
        // now reads cost from `cost_summary` (progress.jsonl + live
        // state.json), not from this field. This particular test only
        // verifies the watchdog doesn't *mutate* state.json mtime, so
        // we leave the assignment as a no-op marker for the
        // "translation only" red-line check.
        #[allow(deprecated)]
        {
            s.cost_used_usd = 100.0;
        }
        s.current_phase = "implement".into();
    });
    let state_path = p.project_state(slug);
    let mtime_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();
    let progress_dir = p.root.join("state").join("progress");
    let count_before = std::fs::read_dir(&progress_dir).unwrap().count();

    let cfg = WatchdogConfig {
        notify_on_phase_cost_usd: Some(50.0),
        ..WatchdogConfig::default()
    };
    let _ = watchdog_scan(&p, &cfg).unwrap();

    let mtime_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "scan must not touch state.json");
    let count_after = std::fs::read_dir(&progress_dir).unwrap().count();
    assert_eq!(
        count_before, count_after,
        "scan must not write to progress/",
    );
}
