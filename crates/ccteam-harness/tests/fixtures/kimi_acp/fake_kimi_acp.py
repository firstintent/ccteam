#!/usr/bin/env python3
"""Fake `kimi acp` for hermetic ccteam harness tests (kimi 0.26.0 wire).

Speaks JSON-RPC 2.0 over stdin/stdout. Supports:
  initialize → notifications/initialized → session/new|resume|load →
  session/prompt → session/set_model → session/cancel
  inbound session/request_permission (client auto-allows on skip)

Kimi wire traits mirrored from `references/kimi-code` (protocol reference
only — never vendored):
  - initialize reply carries agentInfo {name: "Kimi Code CLI", version}
  - session id is a ULID
  - model catalog arrives as `configOptions` (select id "model"); a
    `thought_level` "thinking" toggle rides along for thinking models
  - session/resume does NOT replay history; session/load does
  - the prompt stream emits agent_thought_chunk + agent_message_chunk and
    the response carries ONLY stopReason — no usage/cost on the ACP wire
"""
from __future__ import annotations

import json
import os
import sys
import uuid

SESSION_ID = "01JYQX7A9D2E3F4G5H6J7K8M9N"
KNOWN_MODELS = {
    "kimi-k2-0905-preview": "Kimi K2",
    "kimi-k2-thinking": "Kimi K2 Thinking",
}
MODEL = "kimi-k2-0905-preview"


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notif(method: str, params: dict) -> None:
    emit({"jsonrpc": "2.0", "method": method, "params": params})


def reply(req_id, result) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "result": result})


def err(req_id, code: int, message: str) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})


def config_options(current_model=None):
    current_model = current_model or MODEL
    return [
        {
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": current_model,
            "options": [
                {"value": mid, "name": name} for mid, name in KNOWN_MODELS.items()
            ],
        },
        {
            "type": "select",
            "id": "thinking",
            "name": "Thinking",
            "category": "thought_level",
            "currentValue": "off",
            "options": [
                {"value": "off", "name": "off"},
                {"value": "on", "name": "on"},
            ],
        },
        {
            "type": "select",
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "currentValue": "default",
            "options": [{"value": "default", "name": "default"}],
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
                    {"name": "compact", "description": "Compact context"},
                    {"name": "model", "description": "Switch model"},
                ],
            },
        },
    )


def main() -> None:
    # argv: fake_kimi_acp.py acp
    if "acp" not in sys.argv:
        sys.stderr.write("fake_kimi_acp: expected acp mode\n")
        sys.exit(2)

    session_id = SESSION_ID
    current_model = MODEL
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

        # Record the mcpServers count per session/* method to
        # $CCTEAM_ACP_MCP_DUMP so a test can assert resume/load carry the
        # ccteam tool face.
        if method and method.startswith("session/"):
            dump = os.environ.get("CCTEAM_ACP_MCP_DUMP")
            if dump:
                with open(dump, "a") as f:
                    f.write(f"{method}\t{len(params.get('mcpServers') or [])}\n")

        # Inbound responses from the client (permission allow/decline) — no
        # method; just tolerate and continue.
        if method is None and req_id is not None and ("result" in msg or "error" in msg):
            continue

        if method == "initialize":
            reply(
                req_id,
                {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "mcpCapabilities": {"http": True, "sse": False},
                        "promptCapabilities": {
                            "embeddedContext": True,
                            "image": True,
                        },
                    },
                    "authMethods": ["oauth"],
                    "agentInfo": {"name": "Kimi Code CLI", "version": "0.26.0"},
                },
            )
            continue

        if method == "notifications/initialized":
            # Notification from client — no response.
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
                {
                    "sessionId": session_id,
                    "configOptions": config_options(current_model),
                },
            )
            available_commands_notif(session_id)
            continue

        if method == "session/resume":
            sid = params.get("sessionId") or SESSION_ID
            session_id = sid
            reply(req_id, {"configOptions": config_options(current_model)})
            available_commands_notif(session_id)
            # No history replay on resume.
            continue

        if method == "session/load":
            sid = params.get("sessionId") or SESSION_ID
            session_id = sid
            # Replay history BEFORE the response (best-effort drop client-side).
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
            reply(req_id, {"configOptions": config_options(current_model)})
            available_commands_notif(session_id)
            continue

        if method == "session/set_model":
            mid = (params.get("modelId") or "").strip()
            if mid not in KNOWN_MODELS:
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": -32602,
                            "message": "Invalid params",
                            "data": f"unknown model: {mid}",
                        },
                    }
                )
                continue
            current_model = mid
            # kimi acks with an empty result, then emits a fresh configOptions
            # snapshot as config_option_update.
            reply(req_id, {})
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "config_option_update",
                        "configOptions": config_options(current_model),
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

            # One inbound permission request on the first prompt; the client
            # (skip → auto-allow, hitl → decline) replies asynchronously.
            if request_perm_once:
                request_perm_once = False
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": 900001,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {"toolCallId": "tc1", "title": "bash"},
                            "options": [
                                {
                                    "optionId": "approve_once",
                                    "name": "Approve once",
                                    "kind": "allow_once",
                                },
                                {
                                    "optionId": "approve_always",
                                    "name": "Approve always",
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
            # Kimi's ACP wire carries NO usage/cost — stopReason only.
            reply(req_id, {"stopReason": "end_turn"})
            continue

        if method == "session/cancel":
            if req_id is not None:
                reply(req_id, {})
            continue

        if req_id is not None:
            err(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
