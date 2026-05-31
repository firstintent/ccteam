//! Router + NL-admin coverage (integration level).

use ccteam_im::nl_admin::{parse, AdminCmd};
use ccteam_im::router::{route, HandleMap, Route, MAX_HOPS};

fn make_map() -> HandleMap {
    let mut m = HandleMap::new();
    m.insert("lead", "dev-foo", "lead");
    m.insert("reviewer", "dev-foo", "reviewer");
    m
}

#[test]
fn routes_first_mention_only() {
    let r = route("@lead and also @reviewer please", &make_map(), 0);
    if let Route::Bot { role, payload, .. } = r {
        assert_eq!(role, "lead");
        assert!(payload.contains("@reviewer"));
    } else {
        panic!()
    }
}

#[test]
fn admin_handle_short_circuits_router() {
    let r = route("@ccteam pause dev-foo/lead", &make_map(), 0);
    match r {
        Route::Admin { verb_and_args } => assert_eq!(verb_and_args, "pause dev-foo/lead"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn admin_pause_round_trip() {
    let r = route("@ccteam pause dev-foo/lead", &make_map(), 0);
    if let Route::Admin { verb_and_args } = r {
        let cmd = parse(&verb_and_args);
        assert_eq!(
            cmd,
            AdminCmd::Pause {
                slug: "dev-foo".into(),
                role: Some("lead".into()),
            }
        );
    } else {
        panic!()
    }
}

#[test]
fn hop_budget_drops_at_max() {
    let r = route("@lead loop", &make_map(), MAX_HOPS);
    assert!(matches!(r, Route::Drop { .. }));
}

#[test]
fn unknown_handle_surfaces_typed_variant() {
    // F184 — router now emits a typed `UnknownHandle` variant so the
    // inbound pipeline can render a per-chat "available bots" reply
    // instead of silently dropping the message.
    let r = route("@phantom say hi", &make_map(), 0);
    match r {
        Route::UnknownHandle { handle } => assert_eq!(handle, "phantom"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_at_returns_drop() {
    let r = route("just chatting", &make_map(), 0);
    assert!(matches!(r, Route::Drop { .. }));
}

#[test]
fn admin_status_parses() {
    assert_eq!(parse("status"), AdminCmd::Status);
}
