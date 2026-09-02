#!/usr/bin/env python3
"""Hermetic stand-in for `codex app-server --listen stdio://`.

Speaks just enough JSON-RPC (one JSON object per line on stdin/stdout) for
`CodexAppServerAdapter` to complete its `initialize` handshake and a
`thread/start` / `thread/resume`, and records its own lifecycle under
`$CCTEAM_FAKE_CODEX_STATE` so a test can prove HOW ccteam ended it:

  <state>/pid   — written at startup (this process's pid)
  <state>/term  — written when SIGTERM arrives (graceful stop), then exit 0

A real app-server holds every loaded thread's writer lock (an flock that dies
with the process) and owns a `codex-code-mode-host` helper child that only a
graceful shutdown cleans up — SIGKILL orphans it. This fake is therefore
used to assert that ccteam terminates its app-server with SIGTERM and waits
for the exit BEFORE dialing a replacement (GitHub #189).
"""

import json
import os
import signal
import sys


def state_dir():
    d = os.environ.get("CCTEAM_FAKE_CODEX_STATE")
    if d:
        os.makedirs(d, exist_ok=True)
    return d


def mark(name):
    d = state_dir()
    if d:
        with open(os.path.join(d, name), "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))


def on_term(_signum, _frame):
    mark("term")
    sys.exit(0)


def reply(req_id, result):
    sys.stdout.write(json.dumps({"id": req_id, "result": result}) + "\n")
    sys.stdout.flush()


def main():
    signal.signal(signal.SIGTERM, on_term)
    mark("pid")
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            continue
        req_id = msg.get("id")
        if req_id is None:
            continue  # notification (`initialized`) — nothing to answer
        method = msg.get("method", "")
        params = msg.get("params") or {}
        if method == "thread/start":
            reply(req_id, {"thread": {"id": "fake-thread-1"}, "model": "fake-model"})
        elif method == "thread/resume":
            reply(req_id, {"thread": {"id": params.get("threadId", "fake-thread-1")}})
        elif method == "turn/start":
            reply(req_id, {"turn": {"id": "fake-turn-1"}})
        elif method == "model/list":
            reply(req_id, {"models": []})
        else:
            reply(req_id, {})
    # stdin EOF: the real app-server exits here too.


if __name__ == "__main__":
    main()
