#!/usr/bin/env python3
"""Fake `grok agent stdio` for hermetic ccteam harness tests (grok 0.2.93 wire).

Speaks JSON-RPC 2.0 over stdin/stdout. Supports:
  initialize → notifications/initialized → session/new|load → session/prompt
Emits agent_thought_chunk + agent_message_chunk, then prompt response with usage.
Also emits noise: _x.ai/* notifications, string-id skills-reload, isReplay on load.
"""
from __future__ import annotations

import json
import os
import sys
import uuid

SESSION_ID = "019f4547-0000-7000-8000-00000000cafe"
MODEL = "grok-4.5"
WINDOW = 500000


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notif(method: str, params: dict) -> None:
    emit({"jsonrpc": "2.0", "method": method, "params": params})


def reply(req_id, result) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "result": result})


def err(req_id, code: int, message: str) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})


def models_block() -> dict:
    return {
        "currentModelId": MODEL,
        "availableModels": [
            {
                "modelId": MODEL,
                "name": "Grok 4.5",
                "_meta": {
                    "totalContextTokens": WINDOW,
                    "reasoningEffort": "high",
                    "reasoningEfforts": ["high", "medium", "low"],
                },
            }
        ],
    }


def main() -> None:
    # argv: fake_grok_acp.py agent [--always-approve] [-m MODEL] stdio
    if "stdio" not in sys.argv:
        sys.stderr.write("fake_grok_acp: expected stdio mode\n")
        sys.exit(2)

    # Spontaneous string-id frame (skills-reload) — transport must tolerate.
    emit({"jsonrpc": "2.0", "id": "skills-reload", "result": {"ok": True}})

    session_id = SESSION_ID
    loaded = False

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = msg.get("method")
        req_id = msg.get("id")
        params = msg.get("params") or {}

        # v0.9.0 W1 (G2) — record the mcpServers count per session/* method to
        # $CCTEAM_ACP_MCP_DUMP so a test can assert session/new + session/load
        # carry the ccteam tool face (was hardcoded `[]`, dropping ctx.secret).
        if method and method.startswith("session/"):
            dump = os.environ.get("CCTEAM_ACP_MCP_DUMP")
            if dump:
                with open(dump, "a") as f:
                    f.write(f"{method}\t{len(params.get('mcpServers') or [])}\n")

        if method == "initialize":
            caps = params.get("clientCapabilities") or {}
            # Must be false — tests assert this on the request in Rust; fake just answers.
            _ = caps
            reply(
                req_id,
                {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {
                            "image": False,
                            "audio": False,
                            "embeddedContext": True,
                        },
                        "mcpCapabilities": {"http": True, "sse": True},
                    },
                    "authMethods": ["cached_token", "grok.com"],
                    "_meta": {
                        "grokShell": True,
                        "agentVersion": "0.2.93",
                    },
                },
            )
            # Noise after initialize.
            notif("_x.ai/settings/update", {"ok": True})
            continue

        if method == "notifications/initialized":
            # Notification from client — no response.
            continue

        if method == "session/new":
            if "mcpServers" not in params:
                err(req_id, -32602, "Invalid params: mcpServers required")
                continue
            session_id = SESSION_ID
            reply(req_id, {"sessionId": session_id, "models": models_block()})
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "available_commands_update",
                        "availableCommands": [
                            {"name": "compact", "description": "Compact context"},
                            {"name": "context", "description": "Show context"},
                        ],
                    },
                },
            )
            notif("_x.ai/models/update", {"models": [MODEL]})
            continue

        if method == "session/load":
            if "mcpServers" not in params:
                err(req_id, -32602, "Invalid params: mcpServers required")
                continue
            sid = params.get("sessionId") or SESSION_ID
            session_id = sid
            loaded = True
            reply(req_id, {"models": models_block()})
            # Replay history with isReplay — must be filtered by client.
            notif(
                "_x.ai/session/update",
                {
                    "_meta": {"isReplay": True},
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "REPLAY_MUST_DROP"},
                    },
                },
            )
            continue

        if method == "session/prompt":
            text = ""
            for block in params.get("prompt") or []:
                if isinstance(block, dict) and block.get("type") == "text":
                    text += block.get("text") or ""
            answer = f"echo:{text}" if text else "echo:"
            # Thought (must NOT enter final).
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type": "text", "text": "thinking hard…"},
                        "_meta": {"totalTokens": 1},
                    },
                },
            )
            # Answer chunks.
            mid = len(answer) // 2 or 1
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": answer[:mid]},
                    },
                },
            )
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": answer[mid:]},
                    },
                },
            )
            # Redundant turn_completed noise (not SoT).
            notif(
                "_x.ai/session_notification",
                {"type": "turn_completed", "stop_reason": "end_turn"},
            )
            reply(
                req_id,
                {
                    "stopReason": "end_turn",
                    "_meta": {
                        "inputTokens": 100,
                        "outputTokens": 10,
                        "cachedReadTokens": 20,
                        "reasoningTokens": 5,
                        "totalTokens": 110,
                        "modelId": MODEL,
                        "promptId": str(uuid.uuid4()),
                    },
                },
            )
            continue

        if method == "session/cancel":
            # Notification or request — no-op.
            if req_id is not None:
                reply(req_id, {})
            continue

        if req_id is not None:
            err(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
