#!/usr/bin/env python3
"""Fake `agy acp` for hermetic ccteam harness tests (Antigravity CLI 1.5.18).

Speaks JSON-RPC 2.0 over stdin/stdout. Every shape below was captured from a
live `agy acp` on 2026-11-27 (`references/antigravity-cli` is the vendor's
README/changelog — protocol reference only, never vendored):

  - argv is `acp [--dangerously-skip-permissions] [--agent <name>]`; any
    other flag prints usage and exits 2 BEFORE answering `initialize`, the
    way the real binary does — so an invented flag kills the spawn in tests
    too, not only in production.
  - `initialize` advertises `_meta.antigravity.systemPrompt.append`. ccteam
    must never send it; every `session/*` params object is dumped to
    $CCTEAM_AGY_PARAMS_DUMP so a test can prove that.
  - `session/new|resume|load` return BOTH `models.availableModels`
    (composite `<model>-<level>` ids) and `configOptions` (`model` select of
    base ids, `thought_level` select scoped to the current model, `mode`).
  - `session/set_config_option` replies with the full snapshot and republishes
    it as `config_option_update`. The model axis accepts a composite id and
    answers with base model + implied level; a model with no thinking axis
    drops the `thought_level` entry; a level the model lacks is refused.
  - `session/set_mode` validates the id and emits `current_mode_update`.
  - Without `--dangerously-skip-permissions` a tool call sends
    `session/request_permission` (string id, options `allow`/`deny`) and
    waits; a decline fails that tool call and the turn ends `end_turn` with
    no agent text.
  - `usage_update{used,size}` precedes the `session/prompt` response, which
    carries `usage{inputTokens,outputTokens,cachedReadTokens,…}`.
  - A second `session/prompt` while one runs is agy's "steer": the FIRST
    request is answered at once with a bare `end_turn` and the stream carries
    on under the second. ccteam queues instead — a test that sees
    `steered:` in a reply has caught two prompts overlapping.
  - `session/resume` restores without replay; `session/load` replays
    user + agent chunks before its response; an unknown id fails both.
    Test hooks: a `gone-*` id fails both, a `loadonly-*` id fails resume only.
"""
from __future__ import annotations

import json
import os
import select
import sys

SESSION_ID = "349c21ed-18c4-48da-aca8-4d5b54e09a9e"
WINDOW = 1048576
TOKENS_PER_TURN = 9475

# base model id -> (display name, thinking levels, default level)
MODELS = {
    "gemini-4-neon": ("Gemini 4 Neon", ["low", "medium", "high", "xhigh"], "high"),
    "gemini-3.8-flash": ("Gemini 3.8 Flash", ["low", "medium", "high"], "high"),
    "gemini-pro-orchestrator": ("Orchestrator (Carbon)", [], None),
}
LEVEL_NAMES = {"low": "Low", "medium": "Medium", "high": "High", "xhigh": "Extra"}
MODES = {
    "default": "Default",
    "accept-edits": "Accept edits",
    "auto": "Auto",
    "plan": "Plan",
}
KNOWN_FLAGS = {"--dangerously-skip-permissions": 0, "--agent": 1}


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def reply(req_id, result) -> None:
    emit({"jsonrpc": "2.0", "result": result, "id": req_id})


def err(req_id, code: int, message: str) -> None:
    emit({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": req_id})


class Lines:
    """Line reader over fd 0 that can wait with a timeout (for the steer window)."""

    def __init__(self) -> None:
        self.buf = b""
        self.eof = False

    def next(self, timeout=None):
        while b"\n" not in self.buf and not self.eof:
            if timeout is not None:
                ready, _, _ = select.select([0], [], [], timeout)
                if not ready:
                    return None
            chunk = os.read(0, 65536)
            if not chunk:
                self.eof = True
                break
            self.buf += chunk
        if b"\n" not in self.buf:
            return None
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode("utf-8", "replace").strip()


def parse_argv(argv):
    if not argv or argv[0] != "acp":
        sys.stderr.write("fake_agy_acp: expected `acp` subcommand\n")
        sys.exit(2)
    opts = {"skip": False, "agent": ""}
    i = 1
    while i < len(argv):
        flag = argv[i]
        if flag not in KNOWN_FLAGS:
            # Real agy: usage dump + non-zero exit, pipes close unanswered.
            name = flag.lstrip("-")
            sys.stderr.write(f"Usage: agy acp [flags]\nError: flags provided but not defined: -{name}\n")
            sys.exit(2)
        if KNOWN_FLAGS[flag]:
            i += 1
            if i >= len(argv):
                sys.stderr.write(f"Error: flag needs an argument: {flag}\n")
                sys.exit(2)
            opts["agent"] = argv[i]
        else:
            opts["skip"] = True
        i += 1
    return opts


class Session:
    def __init__(self, opts) -> None:
        self.opts = opts
        self.session_id = SESSION_ID
        self.model = "gemini-4-neon"
        self.level = "high"
        self.mode = "default"
        self.used = 0
        self.history = []  # (user text, agent text)
        self.perm_seq = 0

    def notif(self, update: dict) -> None:
        emit(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": self.session_id, "update": update},
            }
        )

    def config_options(self):
        name, levels, _default = MODELS[self.model]
        opts = [
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": self.model,
                "options": [{"value": mid, "name": m[0]} for mid, m in MODELS.items()],
            }
        ]
        if levels:
            opts.append(
                {
                    "id": "thought_level",
                    "name": "Thinking Level",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": self.level,
                    "options": [{"value": lv, "name": LEVEL_NAMES[lv]} for lv in levels],
                }
            )
        opts.append(
            {
                "id": "mode",
                "name": "Mode",
                "category": "mode",
                "type": "select",
                "currentValue": self.mode,
                "options": [{"value": mid, "name": n} for mid, n in MODES.items()],
            }
        )
        return opts

    def session_result(self):
        available = []
        for mid, (name, levels, _d) in MODELS.items():
            if not levels:
                available.append({"modelId": mid, "name": name})
            for lv in levels:
                available.append({"modelId": f"{mid}-{lv}", "name": f"{name} ({LEVEL_NAMES[lv]})"})
        name, levels, _d = MODELS[self.model]
        current = f"{self.model}-{self.level}" if levels else self.model
        return {
            "sessionId": self.session_id,
            "models": {"availableModels": available, "currentModelId": current},
            "modes": {
                "currentModeId": self.mode,
                "availableModes": [{"id": mid, "name": n} for mid, n in MODES.items()],
            },
            "configOptions": self.config_options(),
            "_meta": {"antigravity": {"tier": {"name": "Google AI Pro", "id": "g1-pro-tier"}}},
        }

    def opening_notifs(self) -> None:
        self.notif(
            {
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {"name": "goal", "description": "Run until the specified goal is completely finished."},
                    {"name": "compact", "description": "Compact the conversation now."},
                    {"name": "plan", "description": "Plan carefully before executing a task."},
                ],
            }
        )
        self.notif({"sessionUpdate": "usage_update", "used": self.used, "size": WINDOW})

    def publish_config(self) -> None:
        self.notif({"sessionUpdate": "config_option_update", "configOptions": self.config_options()})

    def set_model(self, value: str):
        """Base or composite id. Returns an error message or None."""
        if value in MODELS:
            if value != self.model:
                self.model = value
                self.level = MODELS[value][2]
            return None
        for mid, (_name, levels, _d) in MODELS.items():
            for lv in levels:
                if value == f"{mid}-{lv}":
                    self.model, self.level = mid, lv
                    return None
        return f"Unknown model ID: {value}"

    def set_level(self, value: str):
        name, levels, _d = MODELS[self.model]
        if value not in levels:
            return f'Thinking level "{value}" is not offered for model "{name}"'
        self.level = value
        return None


def main() -> None:
    opts = parse_argv(sys.argv[1:])
    argv_dump = os.environ.get("CCTEAM_AGY_ARGV_DUMP")
    if argv_dump:
        with open(argv_dump, "a") as f:
            f.write(json.dumps(sys.argv[1:]) + "\n")

    lines = Lines()
    backlog = []  # messages read while waiting for something else
    s = Session(opts)

    def read_msg(timeout=None):
        """Next JSON message off stdin; None on timeout or EOF."""
        while True:
            line = lines.next(timeout)
            if line is None:
                return None
            if not line:
                continue
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                continue

    def next_msg():
        return backlog.pop(0) if backlog else read_msg()

    def wait_for(pred, timeout):
        """Read until `pred(msg)`; park everything else. None on timeout/EOF."""
        while True:
            msg = read_msg(timeout)
            if msg is None:
                return None
            if pred(msg):
                return msg
            backlog.append(msg)

    def prompt_text(params) -> str:
        text = ""
        for block in params.get("prompt") or []:
            if isinstance(block, dict) and block.get("type") == "text":
                text += block.get("text") or ""
        return text

    def finish(req_id, stop_reason="end_turn", with_usage=True):
        s.notif({"sessionUpdate": "usage_update", "used": s.used, "size": WINDOW})
        result = {"stopReason": stop_reason}
        if with_usage:
            result["usage"] = {
                "inputTokens": 2298,
                "outputTokens": 47,
                "cachedReadTokens": 7394,
                "cachedWriteTokens": 0,
                "thoughtTokens": 45,
                "totalTokens": 9739,
            }
        reply(req_id, result)

    def run_prompt(req_id, params):
        text = prompt_text(params)
        s.used += TOKENS_PER_TURN

        if text.strip() == "/compact":
            # Live agy runs its own compact command: a model call, a fresh
            # `usage_update`, and no agent text at all.
            s.used = 1200
            finish(req_id)
            return

        if "USE_TOOL" in text:
            call = {
                "toolCallId": "call_4346625752038185178",
                "name": "run_command",
                "kind": "execute",
                "title": "echo hello-from-agy > probe.txt",
                "rawInput": {"CommandLine": "echo hello-from-agy > probe.txt"},
            }
            s.notif({"sessionUpdate": "tool_call", "status": "in_progress", **call})
            allowed = True
            if not opts["skip"]:
                s.perm_seq += 1
                perm_id = str(s.perm_seq)
                s.notif({"sessionUpdate": "tool_call_update", "status": "pending", **call})
                emit(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": s.session_id,
                            "toolCall": {"sessionUpdate": "tool_call", "status": "pending", **call},
                            "options": [
                                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
                            ],
                        },
                        "id": perm_id,
                    }
                )
                answer = wait_for(lambda m: m.get("id") == perm_id and "method" not in m, 10)
                picked = ((answer or {}).get("result") or {}).get("outcome") or {}
                allowed = picked.get("outcome") == "selected" and picked.get("optionId") == "allow"
            if not allowed:
                # Live agy: the command does not run, no agent text follows.
                s.notif({"sessionUpdate": "tool_call_update", "status": "failed", **call})
                finish(req_id)
                return
            s.notif(
                {
                    "sessionUpdate": "tool_call_update",
                    "status": "completed",
                    "rawOutput": {"output": "Command exited with code 0.", "exit_code": 0},
                    **call,
                }
            )

        answer = f"echo:{text}"
        if opts["agent"]:
            answer = f"[{opts['agent']}] {answer}"
        mid = len(answer) // 2 or 1
        s.notif({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "thinking hard…"}})
        s.notif({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": answer[:mid]}})

        # agy's steer: a second prompt while this one streams ends THIS
        # request early and carries on under the new one.
        steer = wait_for(lambda m: m.get("method") == "session/prompt", 0.08)
        if steer is not None:
            reply(req_id, {"stopReason": "end_turn"})
            s.notif({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": answer[mid:]}})
            s.notif(
                {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": f"\n\nsteered:{prompt_text(steer.get('params') or {})}"},
                }
            )
            s.history.append((text, answer))
            finish(steer.get("id"))
            return

        s.notif({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": answer[mid:]}})
        s.history.append((text, answer))
        stop_reason = "end_turn"
        if "STOP:" in text:
            stop_reason = text.split("STOP:", 1)[1].split()[0].strip()
        finish(req_id, stop_reason)

    while True:
        msg = next_msg()
        if msg is None:
            break
        method = msg.get("method")
        req_id = msg.get("id")
        params = msg.get("params") or {}

        if method and method.startswith("session/"):
            dump = os.environ.get("CCTEAM_ACP_MCP_DUMP")
            if dump:
                with open(dump, "a") as f:
                    f.write(f"{method}\t{len(params.get('mcpServers') or [])}\n")
            pdump = os.environ.get("CCTEAM_AGY_PARAMS_DUMP")
            if pdump:
                with open(pdump, "a") as f:
                    f.write(json.dumps({"method": method, "params": params}) + "\n")

        # Stray responses from the client (e.g. a late permission answer).
        if method is None:
            continue

        if method == "initialize":
            reply(
                req_id,
                {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {"image": True, "audio": False, "embeddedContext": True},
                        "sessionCapabilities": {"resume": {}, "close": {}, "load": {}, "list": {}},
                        "_meta": {"antigravity": {"tier": True, "systemPrompt": {"append": True}}},
                    },
                    "agentInfo": {"name": "Antigravity ACP Server", "version": "1.5.18"},
                    "authMethods": [{"id": "oauth-personal", "name": "Log in with Google"}],
                },
            )
            continue

        if method == "notifications/initialized":
            if req_id is not None:
                err(req_id, -32601, f"Method not found: {method}")
            continue

        if method == "session/new":
            if "cwd" not in params or "mcpServers" not in params:
                err(req_id, -32602, "Invalid params: cwd and mcpServers required")
                continue
            reply(req_id, s.session_result())
            s.opening_notifs()
            continue

        if method in ("session/resume", "session/load"):
            sid = params.get("sessionId") or ""
            if sid.startswith("gone-"):
                if method == "session/resume":
                    err(req_id, -32603, f"Failed to read session {sid}; retry, or load it with session/load")
                else:
                    err(req_id, -32002, f"Session not found: {sid}")
                continue
            if sid.startswith("loadonly-") and method == "session/resume":
                # A session agy can only restore by replaying it.
                err(req_id, -32603, f"Failed to read session {sid}; retry, or load it with session/load")
                continue
            s.session_id = sid or SESSION_ID
            if method == "session/load":
                # History replay precedes the response.
                s.notif({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "earlier question"}})
                s.notif({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "REPLAY_MUST_DROP"}})
            reply(req_id, s.session_result())
            s.opening_notifs()
            continue

        if method == "session/set_model":
            problem = s.set_model((params.get("modelId") or "").strip())
            if problem:
                err(req_id, -32602, problem)
                continue
            reply(req_id, {})
            s.publish_config()
            continue

        if method == "session/set_config_option":
            config_id = (params.get("configId") or "").strip()
            value = (params.get("value") or "").strip()
            if config_id == "model":
                problem = s.set_model(value)
            elif config_id == "thought_level":
                problem = s.set_level(value)
            elif config_id == "mode":
                problem = None if value in MODES else f"Unknown mode: {value}"
                if not problem:
                    s.mode = value
            else:
                problem = f"Unknown config option: {config_id}"
            if problem:
                err(req_id, -32602, problem)
                continue
            reply(req_id, {"configOptions": s.config_options()})
            s.publish_config()
            continue

        if method == "session/set_mode":
            mode = (params.get("modeId") or "").strip()
            if mode not in MODES:
                err(req_id, -32602, f"Unknown mode: {mode}")
                continue
            s.mode = mode
            reply(req_id, {})
            s.notif({"sessionUpdate": "current_mode_update", "currentModeId": mode})
            s.publish_config()
            continue

        if method == "session/prompt":
            run_prompt(req_id, params)
            continue

        if method == "session/cancel":
            continue

        if method == "session/close":
            reply(req_id, {})
            continue

        if req_id is not None:
            err(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
