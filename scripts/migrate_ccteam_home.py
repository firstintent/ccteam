#!/usr/bin/env python3
"""v0.8.20 — relocate an old ``~/.ccteam`` layout to the new grouped one.

The Rust side is a clean break (no compat logic); this one-time, idempotent
script moves an existing home in place so you keep your config, tenants, IM
credentials, and tokens.

    OLD (flat)                       NEW (grouped)
    ~/.ccteam/web-token          ->  ~/.ccteam/secrets/web-token
    ~/.ccteam/im/credentials.json -> ~/.ccteam/secrets/im-credentials.json
    ~/.ccteam/tenants.json       ->  ~/.ccteam/secrets/users/<id>.json  (one per tenant)
    ~/.ccteam/hub-cache/         ->  ~/.ccteam/cache/hub/
    ~/.ccteam/imd/               ->  ~/.ccteam/state/im/
    ~/.ccteam/progress/          ->  ~/.ccteam/state/progress/
    ~/.ccteam/harness/           ->  ~/.ccteam/state/harness/
    ~/.ccteam/pty/               ->  ~/.ccteam/state/pty/
    (kept: config.yaml, hooks/, run/, state/{pid})
    (deleted: phases/ templates/ inbox/ control/ teams-progress.jsonl config.yaml.bak)

Usage:
    python3 scripts/migrate_ccteam_home.py [HOME]      # default ~/.ccteam
    python3 scripts/migrate_ccteam_home.py --dry-run [HOME]

Run `ccteam stop` first. Re-running is safe (already-moved items are skipped).
"""

import json
import os
import shutil
import sys
from pathlib import Path

DRY = "--dry-run" in sys.argv
ARGS = [a for a in sys.argv[1:] if not a.startswith("-")]
HOME = Path(ARGS[0]).expanduser() if ARGS else Path.home() / ".ccteam"

# (old relative path, new relative path) — a directory or a single file.
MOVES = [
    ("web-token", "secrets/web-token"),
    ("im/credentials.json", "secrets/im-credentials.json"),
    ("hub-cache", "cache/hub"),
    ("imd", "state/im"),
    ("progress", "state/progress"),
    ("harness", "state/harness"),
    ("pty", "state/pty"),
]
# Dead orchestrator-/teams-era leftovers (removed, not moved).
DELETE = ["phases", "templates", "inbox", "control", "teams-progress.jsonl",
          "config.yaml.bak", "im"]


def log(msg):
    print(("[dry-run] " if DRY else "") + msg)


def move(old_rel, new_rel):
    old, new = HOME / old_rel, HOME / new_rel
    if not old.exists():
        return
    if new.exists():
        log(f"skip {old_rel} → {new_rel} (target exists)")
        return
    log(f"move {old_rel} → {new_rel}")
    if DRY:
        return
    new.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(old), str(new))


def split_tenants():
    """tenants.json (one array) → secrets/users/<id>.json (one 0600 file each)."""
    src = HOME / "tenants.json"
    if not src.exists():
        return
    users = HOME / "secrets" / "users"
    try:
        doc = json.loads(src.read_text())
    except Exception as e:  # noqa: BLE001
        log(f"WARN could not parse {src}: {e}; leaving it in place")
        return
    tenants = doc.get("tenants", []) if isinstance(doc, dict) else []
    log(f"split tenants.json → {len(tenants)} file(s) under secrets/users/")
    if DRY:
        return
    users.mkdir(parents=True, exist_ok=True)
    os.chmod(users, 0o700)
    for t in tenants:
        tid = t.get("id")
        if not tid:
            continue
        out = users / f"{tid}.json"
        out.write_text(json.dumps(t, indent=2, ensure_ascii=False))
        os.chmod(out, 0o600)
    src.unlink()


def harden_secrets():
    sec = HOME / "secrets"
    if not sec.exists() or DRY:
        return
    os.chmod(sec, 0o700)
    for name in ("web-token", "im-credentials.json"):
        p = sec / name
        if p.exists():
            os.chmod(p, 0o600)


def main():
    if not HOME.exists():
        log(f"no ccteam home at {HOME}; nothing to do")
        return
    log(f"migrating {HOME}")
    for old_rel, new_rel in MOVES:
        move(old_rel, new_rel)
    split_tenants()
    harden_secrets()
    for name in DELETE:
        p = HOME / name
        if p.exists():
            log(f"delete {name} (dead leftover)")
            if not DRY:
                (shutil.rmtree if p.is_dir() else os.unlink)(str(p))
    log("done." + ("" if DRY else "  Start: `ccteam start`."))


if __name__ == "__main__":
    main()
