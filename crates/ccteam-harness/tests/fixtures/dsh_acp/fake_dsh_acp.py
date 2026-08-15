#!/usr/bin/env python3
"""Fake `dsh --profile ccteam` ACP peer for hermetic ccteam tests.

Speaks JSON-RPC 2.0 over stdin/stdout. It models only the ccteam-owned DSH
Cordis plugin contract observed in W0:
  initialize -> notifications/initialized -> session/new|load ->
  session/prompt -> session/cancel

The fake deliberately accepts no non-empty `mcpServers` on session methods:
DSH receives ccteam's MCP endpoint through child env, not ACP params.
"""
from __future__ import annotations

import json
import os
import sys
import uuid

DEFAULT_PROVIDER = "deepseek-official"
DEFAULT_MODEL = "deepseek-v4-flash"
WINDOW = 131072


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notif(method: str, params: dict) -> None:
    emit({"jsonrpc": "2.0", "method": method, "params": params})


def reply(req_id, result) -> None:
    emit({"jsonrpc": "2.0", "id": req_id, "result": result})


def err(req_id, code, message: str, data=None) -> None:
    payload = {"code": code, "message": message}
    if data is not None:
        payload["data"] = data
    emit({"jsonrpc": "2.0", "id": req_id, "error": payload})


def dump_session(method: str, params: dict) -> None:
    dump = os.environ.get("CCTEAM_DSH_ACP_DUMP")
    if not dump:
        return
    with open(dump, "a") as f:
        f.write(json.dumps({"method": method, "params": params}, separators=(",", ":")) + "\n")


def dump_env() -> None:
    dump = os.environ.get("CCTEAM_DSH_ENV_DUMP")
    if not dump:
        return
    keys = [
        "CCTEAM_CHAT_SID",
        "DSH_HOME",
        "CCTEAM_DSH_TRANSPORT",
        "CCTEAM_DSH_APPROVAL",
        "DSH_TELEMETRY_DISABLED",
        "DSH_TELEMETRY_MODE",
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_BASE_URL",
        "CCTEAM_MCP_HTTP_URL",
        "CCTEAM_MCP_BEARER",
        "DSH_SYSTEM_PROMPT",
    ]
    with open(dump, "w") as f:
        json.dump(
            {
                "argv": sys.argv[1:],
                "env": {key: os.environ.get(key) for key in keys if key in os.environ},
            },
            f,
            separators=(",", ":"),
        )


def agent_options(params: dict) -> tuple[str, str] | None:
    options = params.get("agentOptions")
    if not isinstance(options, dict):
        return None
    provider = options.get("provider")
    model = options.get("model")
    if not isinstance(provider, str) or not isinstance(model, str):
        return None
    if not provider.strip() or not model.strip():
        return None
    return provider, model


def models_block(model: str) -> dict:
    return {
        "currentModelId": model,
        "availableModels": [
            {
                "modelId": model,
                "name": model,
                "_meta": {"totalContextTokens": WINDOW},
            }
        ],
    }


def session_result(session_id: str, model: str) -> dict:
    return {
        "sessionId": session_id,
        "models": models_block(model),
    }


def text_prompt(params: dict) -> str:
    text = ""
    for block in params.get("prompt") or []:
        if isinstance(block, dict) and block.get("type") == "text":
            text += block.get("text") or ""
    return text


def main() -> None:
    if "--version" in sys.argv:
        print("dsh 0.1.0-rc.6")
        return

    if "--profile" not in sys.argv or "ccteam" not in sys.argv:
        sys.stderr.write("fake_dsh_acp: expected --profile ccteam\n")
        sys.exit(2)

    dump_env()

    session_id = "dsh-fake-session"
    current_model = DEFAULT_MODEL

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

        if method and method.startswith("session/"):
            dump_session(method, params)
            if params.get("mcpServers"):
                err(req_id, -32602, "DSH ccteam transport must not receive ACP mcpServers")
                continue

        if method is None and req_id is not None and ("result" in msg or "error" in msg):
            continue

        if method == "initialize":
            reply(
                req_id,
                {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {
                            "embeddedContext": True,
                            "image": False,
                        },
                    },
                    "agentInfo": {
                        "name": os.environ.get("CCTEAM_DSH_AGENT_NAME", "ccteam-dsh-client"),
                        "version": "0.1.0-test",
                    },
                },
            )
            continue

        if method == "notifications/initialized":
            continue

        if method == "session/new":
            picked = agent_options(params)
            if picked is None:
                err(req_id, "MISSING_CREDENTIAL", "agentOptions provider/model required")
                continue
            provider, model = picked
            if provider == "unknown" or model == "unknown":
                err(req_id, "UNKNOWN_MODEL", f"unknown model: {provider}/{model}")
                continue
            current_model = model
            session_id = f"dsh-fake-{uuid.uuid4()}"
            reply(req_id, session_result(session_id, current_model))
            continue

        if method == "session/load":
            sid = params.get("sessionId") or ""
            if os.environ.get("CCTEAM_DSH_LOAD_FAIL") == "1" or sid.startswith("missing"):
                err(
                    req_id,
                    -32000,
                    f'session "{sid}" not found',
                    {"message": f'session "{sid}" not found', "name": "Error"},
                )
                continue
            picked = agent_options(params)
            if picked is None:
                err(req_id, "MISSING_CREDENTIAL", "agentOptions provider/model required")
                continue
            _, model = picked
            current_model = model
            session_id = sid
            reply(req_id, session_result(session_id, current_model))
            continue

        if method == "session/prompt":
            text = text_prompt(params)
            answer = f"echo:{text}" if text else "echo:"
            mid = len(answer) // 2 or 1
            for chunk in [answer[:mid], answer[mid:]]:
                notif(
                    "session/update",
                    {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": chunk},
                        },
                    },
                )
            reply(
                req_id,
                {
                    "stopReason": "end_turn",
                    "_meta": {
                        "inputTokens": 12,
                        "outputTokens": 5,
                        "totalTokens": 17,
                        "modelId": current_model,
                    },
                },
            )
            continue

        if method == "session/cancel":
            notif(
                "session/update",
                {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "turn/end",
                        "kind": "aborted",
                        "reason": {"kind": "user"},
                    },
                },
            )
            if req_id is not None:
                reply(req_id, {})
            continue

        if req_id is not None:
            err(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
