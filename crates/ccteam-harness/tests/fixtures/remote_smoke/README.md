# v0.9.0 real two-process remote smoke (manual, reverse-connection)

Not run in CI (needs a real `claude` binary + a real network hop). This is the
runbook the automated e2e tests stand in for on every commit
(`claude_stream_json_remote_test.rs` for the adapter path over the hub,
`ccteam-web/tests/satellite_ws_test.rs` for the real-socket reverse chain);
run this by hand once per wave / before a real two-machine deploy.

Reverse-connection reminder: the satellite exposes **no port** — it dials the
daemon's `:7331`. Only the daemon needs a reachable address.

## Single-machine version (two local processes, real `claude`)

```sh
# Terminal A — the "main daemon" side: just needs a host registry + a join token.
export CCTEAM_HOME=/tmp/ccteam-main
ccteam init /tmp/ccteam-main-project   # any project; note the slug, e.g. "demo"
ccteam start &                          # main daemon (web + IM + MCP)
TOKEN=$(curl -s -X POST localhost:7331/api/v1/hosts/join-token \
  -H "Authorization: Bearer ccteam:$(cat /tmp/ccteam-main/secrets/web-token)" \
  -d '{"label":"local-smoke"}' | jq -r .token)

# Terminal B — the "satellite" side: a SEPARATE ccteam home, same project slug.
export CCTEAM_HOME=/tmp/ccteam-sat
ccteam init /tmp/ccteam-sat-project --slug demo   # SAME slug as above
ccteam start --web-bind 127.0.0.1:7341 &          # unified process; different web port on one box
ccteam host join --daemon http://127.0.0.1:7331 --token "$TOKEN"
# The running `ccteam start` picks the join up within 30s and dials out
# (watch for "control channel connected" in its log).

# Terminal A again — spawn a session on the satellite and talk to it:
curl -s -X POST localhost:7331/api/v1/projects/demo/sessions \
  -H "Authorization: Bearer ccteam:$(cat /tmp/ccteam-main/secrets/web-token)" \
  -d '{"vendor":"claude","host":"<the host id ccteam host join printed>"}'
# → POST .../sessions/{sid}/turn a real prompt, watch the answer land in
#   <main-daemon-home>/... turns.jsonl (SoT stays on the MAIN side — F3.3).
```

## Two real machines

Same steps, with `--daemon http://<daemon-lan-ip>:7331`. Nothing to expose or
advertise on the satellite side; NAT/firewall in front of the satellite is fine.

## What to eyeball

- The control channel connects and reports (`RUST_LOG=ccteam_web=debug`
  on the satellite logs connect/report; the daemon logs
  "host channel connected" and the hosts page flips to online).
- `exec_open` → dial-back pairing on spawn (satellite logs
  "exec_open — dialing back").
- The answer lands in the MAIN daemon's turns.jsonl / web chat — not the
  satellite's (SoT stays main-side).
- Kill the satellite `ccteam start` mid-turn → the main-side chat gets a
  readable failure; restart it → control channel redials (backoff), and the
  NEXT message auto-reconnects (`--resume` on the satellite, same
  deterministic uuid).
- Stop the satellite process entirely, restart the main daemon → the
  session stays `stopped` with a readable "host offline / no live control
  channel" error, never silently respawns locally (G10).
- Pull the network cable / drop the route mid-idle → both sides tear the
  half-open link down within ~75s (keepalive contract) and the satellite
  redials when the route returns.
- HITL: spawn `--permission-mode hitl` (or the `hitl` protocol flag) on
  the remote session; the approval prompt must reach the SAME IM/web
  approval UI a local hitl session uses.
