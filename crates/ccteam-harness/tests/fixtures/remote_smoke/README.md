# v0.9.0 W3 real two-process remote smoke (manual, `CCTEAM_REAL_REMOTE=1`)

Not run in CI (needs a real `claude` binary + a real network hop). This is the
runbook the automated fake-satellite e2e tests (`claude_stream_json_remote_test.rs`)
stand in for on every commit; run this by hand once per wave / before a real
two-machine deploy.

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
ccteam host join --daemon http://127.0.0.1:7331 --token "$TOKEN" \
  --agent-url http://127.0.0.1:7332
ccteam host serve --bind 127.0.0.1:7332 &

# Terminal A again — spawn a session on the satellite and talk to it:
curl -s -X POST localhost:7331/api/v1/projects/demo/sessions \
  -H "Authorization: Bearer ccteam:$(cat /tmp/ccteam-main/secrets/web-token)" \
  -d '{"vendor":"claude","host":"<the host id ccteam host join printed>"}'
# → POST .../sessions/{sid}/turn a real prompt, watch the answer land in
#   <main-daemon-home>/... turns.jsonl (SoT stays on the MAIN side — F3.3).
```

## Two real machines

Same steps, with `--daemon`/`--agent-url` pointing at real LAN addresses
(not `127.0.0.1`) and `ccteam host serve --bind 0.0.0.0:7332 --advertise-url
http://<sat-lan-ip>:7332`.

## What to eyeball

- `ExecSpec` reaches the satellite (`RUST_LOG=ccteam_web=debug ccteam host serve`
  logs the exec-bridge session lifecycle).
- The answer lands in the MAIN daemon's turns.jsonl / web chat — not the
  satellite's (SoT stays main-side).
- Kill `ccteam host serve` mid-turn → the main-side chat gets a readable
  `TurnFailed`-style message; the NEXT message auto-reconnects
  (`--resume` on the satellite, same deterministic uuid).
- Stop the satellite process entirely, restart the main daemon → the
  session stays `stopped` with a readable "host offline" error, never
  silently respawns locally (G10).
- HITL: spawn `--permission-mode hitl` (or the `hitl` protocol flag) on
  the remote session; the approval prompt must reach the SAME IM/web
  approval UI a local hitl session uses.
