# V0.6.5 Host-probe Sign-off (nas-box005)

> **Run by:** V0.6.5 Wave 4b subagent (Opus 4.7 1M)
> **Date:** 2026-05-24
> **Host:** `rob@192.168.1.19` (nas-box005)
> **Binary:** `/home/rob/nasworkspace/ccteam/target/release/ccteam` v0.6.5 (built fresh from `origin/main` HEAD `8cde065`)
> **State:** fresh wipe at 2026-05-24 07:06 UTC (`rm -rf ~/.ccteam/` + per-project `.ccteam/` + ccteam claude session dirs); TG credentials re-injected (bot_token + chat_id 339498819)
> **Authorization:** user explicitly authorized fresh wipe (continued from prior session V0.6.4 deploy approval)

## Summary

| Gate | Status | Evidence |
|---|---|---|
| **F148** `/ccteam-creator` → TG round-trip | **PASS** | Fresh-state `/ccteam-creator "做个 TG 助理 bot"` → `go` → bot registered + tmux supervisor started + 3 real Telegram messages delivered (tg_msg_id 1896/1897/1898 to recipient 339498819) within ~25s of `go` confirmation. See "F148 details" below. |
| **F157** `ccteam-scan --quick` <= 90s | **PASS** | Real run on `/home/rob/nasworkspace/ccteam` (89 kLOC Rust workspace): wall-clock **33s** (well under 90s). `.ccteam/codebase-scan.md` written (42 lines, frontmatter `quick: true`, all 3 Q's answered). |
| **F162** `intent-accuracy.sh --real` | **PASS** | 50/50 = **1.0000** accuracy. Per-intent precision/recall all 1.000. Wall-clock 5:27. Report at `docs/versions/v0-6-5/intent-accuracy.md` (mode=`real`). Ship gate >= 0.90 cleared with margin. |
| **F163** SIGTERM graceful (<= 5s) | **FAIL** | SIGTERM sent to two separate daemon instances (PID 1773722 and PID 1776891). Both stayed alive >60s after SIGTERM; pidfile not unlinked; required SIGKILL. **Port 7331 IS released** within ~1s of SIGTERM (web server tokio task does respond to shutdown), but the main process does not exit. Partial graceful shutdown — handler accepts SIGTERM but blocks somewhere before runtime drop. |
| **F164** tmux reattach across restart | **PASS** | Bot tmux session_id `$1` (`ccteam-chat-tech-helper-tg-tech-helper`) survived daemon kill + restart. Daemon log explicitly emits `claude-tui: reattached to existing tmux session (pane claude process alive) event="session_reattached" pane_pid=Some(1775109)`. Post-restart MCP `chat_send_input` → bot pane → real Telegram delivery (tg_msg_id 1899 to recipient 339498819) confirmed. |

**4 of 5 gates pass. F163 fails.**

---

## F148 details

1. `/ccteam-creator "做个 TG 助理 bot"` in fresh claude TUI session (cwd `/home/rob/projects/probe-tg-bot`).
2. Skill displayed PROJECT PLAN: `slug=tech-helper-tg`, `type=chat-pocket`, `persona=tech-helper zh`, `im_platform=telegram`, `Codex critic=auto-enabled`, plus file list and existing-credentials note.
3. User `go` confirmation sent → skill wrote `workflow.yaml` + `tech-helper.md` agent + `.mcp.json` + invoked `mcp__ccteam__chat_register_bot` (F146/F148) → registration JSON appeared at `~/.ccteam/imd/registry/tech-helper-tg/tech-helper.json`:
   ```json
   {
     "workflow_slug": "tech-helper-tg",
     "role": "tech-helper",
     "vendor": "claude",
     "persona_id": "tech-helper",
     "im_platform": "telegram",
     "im_chat_id": "339498819",
     "created_at": "2026-05-23T23:14:58.811623742Z"
   }
   ```
4. Daemon (running pre-existing) noticed new registration → supervisor started → tmux session `ccteam-chat-tech-helper-tg-tech-helper` spawned + mpsc fast-path wired (daemon log).
5. Inbound simulation: drove `mcp__ccteam__chat_send_input` via `ccteam internal mcp-serve` stdio → mailbox file dropped at `<project>/.ccteam/chat/tech-helper/inbox/msg-1779578174702-284c2084.md` → inotify fast-path consumed within ~4s → claude pane took the message.
6. Bot wrote 3 assistant turns to `turns.jsonl`; outbound forwarder delivered each to Telegram chat 339498819. Daemon log:
   - `tg.egress recipient=339498819 tg_msg_id="1896" send_http_ms=831 content_len=37`
   - `tg.egress recipient=339498819 tg_msg_id="1897" send_http_ms=432 content_len=249`
   - `tg.egress recipient=339498819 tg_msg_id="1898" send_http_ms=430 content_len=42`

**Caveat — inbound side:** The literal "user TG client sends 'hi'" leg could not be driven by this subagent (no Telegram user account access on host probe). Inbound was simulated via `chat_send_input` MCP, which drops a mailbox file with the same shape the daemon writes after receiving a real `getUpdates` payload. The outbound chain (supervisor → tmux → claude → turns.jsonl → forwarder → Telegram API) is the same code regardless of source. End-to-end registration + outbound delivery to the real user TG chat is verified; the TG-polling-to-inbox leg is not separately exercised in this probe (relied on by `crates/ccteam-imd` integration tests).

---

## F157 details

```
START_TS=1779578377 (2026-05-24 07:19:37 UTC)
END_TS  =1779578410 (33s later)
output  = /home/rob/nasworkspace/ccteam/.ccteam/codebase-scan.md (42 lines)
frontmatter: quick: true
```

Report content quality: Q1 correctly identifies Rust workspace + tokio + clap + entry binary; Q2 lists 118 TODO/FIXME hits with top-10 ranking; Q3 confirms CLAUDE.md (149 lines) + README.md (63 lines, English) + AGENTS.md (10 lines) all present.

---

## F162 details

```
$ cd /home/rob/nasworkspace/ccteam
$ PATH=/tmp/pyshim:$PATH time bash scripts/host-probe/intent-accuracy.sh --real
=== F162 intent-accuracy (real) ===
corpus: /home/rob/nasworkspace/ccteam/tests/intent-corpus.yaml
total: 50  correct: 50  accuracy: 1.0000

per-intent:
  intent                  P      R  support
  start-team          1.000  1.000        8
  create-workflow     1.000  1.000        7
  configure-im        1.000  1.000        7
  monitor             1.000  1.000        7
  advise              1.000  1.000        7
  status-debug        1.000  1.000        7
  code-scan           1.000  1.000        7

ship gate (>= 0.90): PASS
report written: /home/rob/nasworkspace/ccteam/docs/versions/v0-6-5/intent-accuracy.md
70.32user 16.92system 5:27.61elapsed 26%CPU
```

Note: NAS `/usr/bin/python3` is permission-denied for user `rob` (Synology default ACL). Worked around with `mkdir /tmp/pyshim && ln -sf /home/rob/.local/bin/python3.11 /tmp/pyshim/python3 && PATH=/tmp/pyshim:$PATH`. Not a script bug — environmental. Optional follow-up: have `intent-accuracy.sh` fall back through `python3.12 -> python3.11 -> python3` PATH search.

---

## F163 details — FAIL

Two independent SIGTERM attempts, both stuck:

**Attempt 1** (daemon PID 1773722, started by host-probe at 07:12):
```
$ kill -TERM 1773722  # at 2026-05-24 07:11:26 UTC
# polled every 200ms for 10s → still alive
# slept 1s → still alive
# ps -p 1773722 → still in Sl state
# pidfile NOT unlinked
# port 7331 IS released (no LISTEN on :7331)
```

**Attempt 2** (daemon PID 1776891, started post-F164 reattach):
```
$ kill -TERM 1776891  # at 2026-05-24 07:26 UTC
# polled every 200ms for 10s → still alive
# additional 60s wait → still alive (full ~70s post-SIGTERM)
# Required kill -9 to clean up.
```

Both runs: pidfile NOT auto-unlinked, but port 7331 freed within ~1s.

**Interpretation:** the daemon does have *some* signal handler (web server tokio task exits + binds port), but the main process hangs on a non-cancellable task — possibly an outstanding tokio runtime task (claude_jobs gc / artifact_watcher polling / supervisor thread join). This is consistent with F163's PRD description identifying this as a known V0.6.4 ship blocker.

**Implication for V0.6.5 ship gate #5** (`docs/versions/v0-6-5/README.md` §5 #5): the ship gate is **not** met. Either (a) defer F163 explicitly with a `docs/versions/v0-6-6/` plan, or (b) treat current daemon shutdown semantics as "SIGTERM releases port + frees daemon-level resources but does not exit process; operators must `kill -9` after SIGTERM if they need the PID slot" and document accordingly. Recommend escalating to main session for ship/no-ship decision.

---

## F164 details — PASS

```
$ tmux list-sessions -F "#{session_id} #{session_name}"
$1 ccteam-chat-tech-helper-tg-tech-helper   # before daemon kill (created 07:15:02)
$0 f148-creator

# kill -TERM 1773722 + kill -9 1773722 + rm orchestrator.pid
# nohup ccteam start ... & disown
# sleep 6

$ tmux list-sessions -F "#{session_id} #{session_name}"
$1 ccteam-chat-tech-helper-tg-tech-helper   # SAME session_id after restart
$0 f148-creator

# daemon log:
INFO claude-tui: reattached to existing tmux session (pane claude process alive)
     event="session_reattached"
     session=ccteam-chat-tech-helper-tg-tech-helper
     slug=tech-helper-tg role=tech-helper
     pane_pid=Some(1775109)
```

Post-reattach functional test: dropped a new `chat_send_input` mailbox → daemon delivered → bot replied → `tg.egress recipient=339498819 tg_msg_id="1899" send_http_ms=823 content_len=54`.

**Minor wrinkle observed:** the freshly-restarted daemon's mpsc fast-path inotify watcher arms *after* daemon startup. Mailbox files that were already on disk at daemon start (between SIGKILL and restart) are not retroactively consumed until `touch`'d (CREATE/MODIFY inotify event re-fires). Once `touch`'d, normal flow resumed. Not blocking the F164 reattach test; flagged as a minor V0.6.6 candidate (daemon should sweep `.ccteam/chat/*/inbox/` once at startup before arming inotify, so pre-existing files don't get stuck).

---

## Cleanup performed

```bash
ssh rob@192.168.1.19 'pkill -9 -f "ccteam start" 2>/dev/null; tmux kill-server 2>/dev/null'
```

NAS left in clean state (no daemon, no tmux sessions, registry intact for future debugging).

---

## Sign-off

| | |
|---|---|
| **Overall** | **4/5 gates PASS** (F148 / F157 / F162 / F164) + **1 FAIL** (F163) |
| **Recommend** | escalate F163 to main session for ship/no-ship decision; F164 minor inbox-on-startup-sweep wrinkle is V0.6.6 candidate |
| **Run duration** | ~30 min (07:00 - 07:30 UTC, 2026-05-24); blocked once on python3 PATH issue (5 min troubleshoot) |
