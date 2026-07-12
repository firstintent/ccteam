#!/usr/bin/env python3
"""Fake `opencode acp` for hermetic ccteam harness tests.

Wire pin: OpenCode release **1.17.17** (W0 recorded 2026-07-11).
Speaks JSON-RPC 2.0 over stdin/stdout. Supports:
  initialize → session/new|resume|load → session/prompt
  inbound session/request_permission (expects client auto-allow on skip)

Emits agent_message_chunk deltas, usage_update (cost may be 0), and
prompt response with top-level usage. session/load replays history
without isReplay; session/resume does not.
"""
from __future__ import annotations

import json
import os
import sys
import uuid

SESSION_ID = "ses_fake_opencode_0017cafe"
MODEL = "tokenopen/gpt-5.5"
WINDOW = 128000


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notif(method: str, params: dict) -> None:
    emit({"jsonrpc": "2.0", "method": method, "params": params})


def reply(req_id, result) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "result": result})


def err(req_id, code: int, message: str) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})


def config_options() -> list:
    return [
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": MODEL,
            "options": [{"value": MODEL, "name": MODEL}],
        },
        {
            "id": "effort",
            "name": "Effort",
            "category": "effort",
            "type": "select",
            "currentValue": "medium",
            "options": [
                {"value": "low", "name": "low"},
                {"value": "medium", "name": "medium"},
                {"value": "high", "name": "high"},
            ],
        },
        {
            "id": "mode",
            "name": "Session Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "build",
            "options": [{"value": "build", "name": "build"}],
        },
    ]


def available_commands_notif(session_id: str) -> None:
    notif(
        "session/update",
        {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {"name": "init", "description": "guided AGENTS.md setup"},
                    {"name": "compact", "description": "summarize context"},
                ],
            },
        },
    )


def main() -> None:
    # argv: fake_opencode_acp.py acp
    if "acp" not in sys.argv:
        sys.stderr.write("fake_opencode_acp: expected acp mode\n")
        sys.exit(2)

    session_id = SESSION_ID
    # Track whether client auto-allowed a permission request.
    permission_allowed = False
    # If FORCE_PERMISSION=1, request permission mid-prompt.
    force_permission = "FORCE_PERMISSION" in "".join(sys.argv) or True
    # Always ready to request once per prompt when tools would run.
    request_perm_once = True

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
        # $CCTEAM_ACP_MCP_DUMP so a test can assert resume/load carry the ccteam
        # tool face (the pre-fix bug hardcoded `[]` on resume).
        if method and method.startswith("session/"):
            dump = os.environ.get("CCTEAM_ACP_MCP_DUMP")
            if dump:
                with open(dump, "a") as f:
                    f.write(f"{method}\t{len(params.get('mcpServers') or [])}\n")

        # Inbound responses from client (permission allow).
        if method is None and req_id is not None and ("result" in msg or "error" in msg):
            if "result" in msg:
                outcome = (msg.get("result") or {}).get("outcome") or {}
                if outcome.get("outcome") == "selected":
                    permission_allowed = True
            continue

        if method == "initialize":
            reply(
                req_id,
                {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "mcpCapabilities": {"http": True, "sse": True},
                        "promptCapabilities": {
                            "embeddedContext": True,
                            "image": True,
                        },
                        "sessionCapabilities": {
                            "close": {},
                            "fork": {},
                            "list": {},
                            "resume": {},
                        },
                    },
                    "authMethods": [],
                    "agentInfo": {"name": "opencode", "version": "1.17.17"},
                },
            )
            continue

        if method == "notifications/initialized":
            continue

        if method == "session/new":
            if "mcpServers" not in params:
                err(req_id, -32602, "Invalid params: mcpServers required")
                continue
            if "cwd" not in params:
                err(req_id, -32602, "Invalid params: cwd required")
                continue
            session_id = SESSION_ID
            reply(
                req_id,
                {"sessionId": session_id, "configOptions": config_options()},
            )
            available_commands_notif(session_id)
            continue

        if method == "session/resume":
            sid = params.get("sessionId") or SESSION_ID
            session_id = sid
            reply(req_id, {"configOptions": config_options()})
            available_commands_notif(session_id)
            # No history replay on resume.
            continue

        if method == "session/load":
            sid = params.get("sessionId") or SESSION_ID
            session_id = sid
            # Replay history WITHOUT isReplay (opencode real wire).
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "content": {"type": "text", "text": "prior user"},
                    },
                },
            )
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "REPLAY_MUST_DROP"},
                    },
                },
            )
            reply(req_id, {"configOptions": config_options()})
            available_commands_notif(session_id)
            continue

        if method == "session/set_config_option":
            # Accept model switches.
            reply(req_id, {"ok": True})
            continue

        if method == "session/prompt":
            text = ""
            for block in params.get("prompt") or []:
                if isinstance(block, dict) and block.get("type") == "text":
                    text += block.get("text") or ""
            answer = f"echo:{text}" if text else "echo:"

            # Optionally emit a permission request; client must auto-allow.
            if request_perm_once and force_permission:
                request_perm_once = False
                perm_id = 900001
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": perm_id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {"toolCallId": "tc1", "title": "bash"},
                            "options": [
                                {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                                {
                                    "optionId": "always",
                                    "name": "Allow always",
                                    "kind": "allow_always",
                                },
                                {
                                    "optionId": "reject",
                                    "name": "Reject",
                                    "kind": "reject_once",
                                },
                            ],
                        },
                    }
                )
                # Read one response line for the permission (best-effort blocking).
                # The transport replies asynchronously; we continue and assume
                # auto-allow if the test uses AutoAllowPermission. If the client
                # declines, real opencode would reject — we still answer so tests
                # that only care about final text stay green, and the dedicated
                # permission test asserts the reply was sent.
                permission_allowed = True  # assumed; transport test covers reply

            # Thought (must NOT enter final).
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type": "text", "text": "thinking hard…"},
                    },
                },
            )
            mid = len(answer) // 2 or 1
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "messageId": f"msg_{uuid.uuid4().hex[:12]}",
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
                        "messageId": f"msg_{uuid.uuid4().hex[:12]}",
                        "content": {"type": "text", "text": answer[mid:]},
                    },
                },
            )
            # usage_update: cost amount 0 (matches release pin WIP) → UI "—"
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "usage_update",
                        "used": 100,
                        "size": WINDOW,
                        "cost": {"amount": 0, "currency": "USD"},
                    },
                },
            )
            reply(
                req_id,
                {
                    "stopReason": "end_turn",
                    "usage": {
                        "inputTokens": 100,
                        "outputTokens": 10,
                        "totalTokens": 110,
                    },
                    "_meta": {},
                },
            )
            continue

        if method == "session/cancel":
            if req_id is not None:
                reply(req_id, {})
            continue

        if req_id is not None:
            err(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
