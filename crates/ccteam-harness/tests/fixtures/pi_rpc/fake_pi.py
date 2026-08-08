#!/usr/bin/env python3
import json
import os
import pathlib
import sys
import urllib.request


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def response(command, success=True, data=None, error=None):
    value = {
        "type": "response",
        "id": command.get("id"),
        "command": command.get("type", "unknown"),
        "success": success,
    }
    if data is not None:
        value["data"] = data
    if error is not None:
        value["error"] = error
    emit(value)


def usage(input_tokens=10, output_tokens=4, cost=0.01, reasoning=2):
    return {
        "input": input_tokens,
        "output": output_tokens,
        "cacheRead": 2,
        "cacheWrite": 1,
        "reasoning": reasoning,
        "totalTokens": input_tokens + output_tokens + 3,
        "cost": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "total": cost,
        },
    }


def assistant(stop_reason, text, *, cost=0.01, tool=False, error=None, model=None):
    content = []
    if text:
        content.append({"type": "text", "text": text})
    if tool:
        content.append(
            {
                "type": "toolCall",
                "id": "call-1",
                "name": "bash",
                "arguments": {"command": "true"},
            }
        )
    message = {
        "role": "assistant",
        "content": content,
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": model or state["model"],
        "responseModel": model or state["model"],
        "usage": usage(cost=cost),
        "stopReason": stop_reason,
        "timestamp": 1,
    }
    if error:
        message["errorMessage"] = error
    emit({"type": "message_end", "message": message})


def save():
    pathlib.Path(session_file).write_text(json.dumps(state), encoding="utf-8")


def mcp_call(method, params, request_id=None):
    payload = {"jsonrpc": "2.0", "method": method, "params": params}
    if request_id is not None:
        payload["id"] = request_id
    request = urllib.request.Request(
        os.environ["CCTEAM_MCP_HTTP_URL"],
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {os.environ['CCTEAM_MCP_BEARER']}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=2) as response_value:
        body = response_value.read()
    if request_id is None or not body:
        return None
    decoded = json.loads(body)
    if "error" in decoded:
        raise RuntimeError(decoded["error"].get("message", "MCP error"))
    return decoded.get("result")


args = sys.argv[1:]
if args == ["--version"]:
    print(os.environ.get("CCTEAM_PI_FAKE_VERSION", "0.83.0"))
    raise SystemExit(0)

if "--mode" not in args or args[args.index("--mode") + 1] != "rpc":
    print("fake Pi requires --mode rpc", file=sys.stderr)
    raise SystemExit(2)
if "--session" in args and "--session-id" in args:
    print("--session-id cannot be combined with --session", file=sys.stderr)
    raise SystemExit(2)
if "--no-context-files" in args:
    print("forbidden --no-context-files", file=sys.stderr)
    raise SystemExit(2)

session_dir = pathlib.Path(os.environ["CCTEAM_PI_FAKE_SESSION_DIR"])
session_dir.mkdir(parents=True, exist_ok=True)
if "--session" in args:
    session_file = args[args.index("--session") + 1]
    state = json.loads(pathlib.Path(session_file).read_text(encoding="utf-8"))
    resumed = True
else:
    session_id = args[args.index("--session-id") + 1]
    session_file = str((session_dir / f"{session_id}.jsonl").resolve())
    state = {
        "session_id": session_id,
        "model": "claude-sonnet-4-20250514",
        "thinking": "medium",
        "history": [],
        "context_null": False,
        "title": None,
    }
    resumed = False

if "--model" in args:
    requested = args[args.index("--model") + 1]
    provider, model_id = requested.split("/", 1)
    state["provider"] = provider
    state["model"] = "clamped-model" if model_id == "force-clamp" else model_id
else:
    state.setdefault("provider", "anthropic")
if "--thinking" in args:
    requested = args[args.index("--thinking") + 1]
    state["thinking"] = "low" if requested == "force-clamp" else requested
save()

prompt_path = None
prompt_body = None
if "--system-prompt" in args:
    prompt_path = args[args.index("--system-prompt") + 1]
    prompt_body = pathlib.Path(prompt_path).read_text(encoding="utf-8")
bad_handshake = prompt_body is not None and "FAIL ROLE" in prompt_body

bridge_path = None
bridge_body = None
if "-e" in args:
    bridge_path = args[args.index("-e") + 1]
    bridge_body = pathlib.Path(bridge_path).read_text(encoding="utf-8")

log_path = pathlib.Path(os.environ["CCTEAM_PI_FAKE_LOG"])
log_path.parent.mkdir(parents=True, exist_ok=True)
with log_path.open("a", encoding="utf-8") as log:
    log.write(
        json.dumps(
            {
                "args": args,
                "cwd": os.getcwd(),
                "session_id": state["session_id"],
                "resumed": resumed,
                "prompt_path": prompt_path,
                "prompt_body": prompt_body,
                "bridge_path": bridge_path,
                "bridge_body": bridge_body,
                "no_extensions": "--no-extensions" in args,
                "env_keys": sorted(
                    key
                    for key in os.environ
                    if key
                    in {
                        "CCTEAM_CHAT_SID",
                        "CCTEAM_PERMISSION_MODE",
                        "CCTEAM_MCP_BEARER",
                        "CCTEAM_MCP_HTTP_URL",
                    }
                ),
            },
            ensure_ascii=False,
        )
        + "\n"
    )

if bridge_path is not None:
    try:
        if os.environ.get("CCTEAM_MCP_HTTP_URL"):
            mcp_call(
                "initialize",
                {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "ccteam-pi-bridge", "version": "1"},
                },
                "pi-1",
            )
            mcp_call("notifications/initialized", {}, None)
            listed = mcp_call("tools/list", {}, "pi-2")
            tool_names = [f"ccteam_{tool['name']}" for tool in listed["tools"]]
        else:
            tool_names = [
                "ccteam_status",
                "ccteam_grok_claude_codex_kimi",
                "ccteam_chat_send_file",
                "ccteam_session_spawn",
                "ccteam_session_dispatch",
                "ccteam_session_collect",
                "ccteam_session_list",
                "ccteam_session_stop",
            ]
        emit(
            {
                "type": "extension_ui_request",
                "id": "bridge-ready",
                "method": "setStatus",
                "statusKey": "ccteam.bridge",
                "statusText": f"ready:{','.join(tool_names)}",
            }
        )
    except Exception as error:
        emit(
            {
                "type": "extension_error",
                "extensionPath": bridge_path,
                "event": "session_start",
                "error": f"ccteam bridge unavailable: {error}",
            }
        )


def log_event(value):
    with log_path.open("a", encoding="utf-8") as log:
        log.write(json.dumps(value, ensure_ascii=False) + "\n")


def wait_for_ui_response(request_id):
    for nested_raw in sys.stdin:
        nested = json.loads(nested_raw)
        if nested.get("type") == "extension_ui_response" and nested.get("id") == request_id:
            log_event({"ui_response": nested})
            return nested
        response(nested, success=False, error="fake Pi expected extension_ui_response")
    return {"type": "extension_ui_response", "id": request_id, "cancelled": True}


pending = None
for raw in sys.stdin:
    command = json.loads(raw)
    kind = command.get("type")
    if kind == "get_state":
        response(
            command,
            data={
                "model": {
                    "id": state["model"],
                    "name": state["model"],
                    "provider": state["provider"],
                    "reasoning": True,
                    "thinkingLevelMap": {},
                    "contextWindow": 200000,
                },
                "thinkingLevel": state["thinking"],
                "isStreaming": pending is not None,
                "isCompacting": False,
                "steeringMode": "one-at-a-time",
                "followUpMode": "one-at-a-time",
                "sessionFile": session_file,
                "sessionId": "wrong-session" if bad_handshake else state["session_id"],
                "autoCompactionEnabled": True,
                "messageCount": len(state["history"]),
                "pendingMessageCount": 0,
            },
        )
    elif kind == "get_available_models":
        response(
            command,
            data={
                "models": [
                    {
                        "id": "claude-sonnet-4-20250514",
                        "name": "Claude Sonnet",
                        "provider": "anthropic",
                        "reasoning": True,
                        "thinkingLevelMap": {},
                        "contextWindow": 200000,
                    },
                    {
                        "id": "claude-haiku-4-5",
                        "name": "Claude Haiku",
                        "provider": "anthropic",
                        "reasoning": False,
                        "contextWindow": 200000,
                    },
                    {
                        "id": "claude-opus-4-6",
                        "name": "Claude Opus",
                        "provider": "anthropic",
                        "reasoning": True,
                        "thinkingLevelMap": {"xhigh": "xhigh", "max": None},
                        "contextWindow": 200000,
                    },
                    {
                        "id": "gpt-5.6",
                        "name": "GPT 5.6",
                        "provider": "openai",
                        "reasoning": True,
                        "thinkingLevelMap": {"minimal": None, "xhigh": None, "max": "max"},
                        "contextWindow": 200000,
                    },
                ]
            },
        )
    elif kind == "get_available_thinking_levels":
        response(command, data={"levels": ["off", "minimal", "low", "medium", "high"]})
    elif kind == "get_session_stats":
        tokens = None if state.get("context_null") else 12345
        response(
            command,
            data={
                "sessionFile": session_file,
                "sessionId": state["session_id"],
                "userMessages": len(state["history"]),
                "assistantMessages": len(state["history"]),
                "toolCalls": 0,
                "toolResults": 0,
                "totalMessages": len(state["history"]) * 2,
                "tokens": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "total": 2},
                "cost": 0.01,
                "contextUsage": {"tokens": tokens, "contextWindow": 200000, "percent": None},
            },
        )
    elif kind == "set_model":
        state["provider"] = command["provider"]
        state["model"] = "clamped-model" if command["modelId"] == "force-clamp" else command["modelId"]
        save()
        response(
            command,
            data={
                "id": state["model"],
                "name": state["model"],
                "provider": state["provider"],
                "reasoning": True,
                "thinkingLevelMap": {},
                "contextWindow": 200000,
            },
        )
    elif kind == "set_thinking_level":
        state["thinking"] = "low" if command["level"] == "force-clamp" else command["level"]
        save()
        response(command)
    elif kind == "set_session_name":
        state["title"] = command["name"]
        save()
        response(command)
    elif kind == "get_commands":
        response(command, data={"commands": [{"name": "fake", "source": "extension"}]})
    elif kind == "compact":
        response(
            command,
            data={
                "summary": "summary",
                "firstKeptEntryId": "entry",
                "tokensBefore": 10,
                "estimatedTokensAfter": 3,
                "usage": usage(3, 2, 0.03, 1),
                "details": {},
            },
        )
    elif kind == "prompt":
        message = command["message"]
        state["history"].append(message)
        if message == "context-null":
            state["context_null"] = True
        save()
        response(command)
        emit({"type": "agent_start"})
        if message == "bridge-tools":
            mcp_call("tools/call", {"name": "status", "arguments": {}}, "pi-tool-status")
            mcp_call("tools/call", {"name": "session_list", "arguments": {}}, "pi-tool-list")
            assistant("stop", "bridge-tools-ok")
            emit({"type": "agent_settled"})
            continue
        if message == "skip-tool":
            assistant("stop", "skip-no-prompt")
            emit({"type": "agent_settled"})
            continue
        if message.startswith("hitl:"):
            _, tool_name, decision = message.split(":", 2)
            envelope = {
                "toolCallId": f"call-{tool_name}-{decision}",
                "toolName": tool_name,
                "input": {"testDecision": decision, "command": "echo safe"},
            }
            if decision == "oversize":
                envelope["input"]["blob"] = "x" * (65 * 1024)
            request_id = f"permission-{tool_name}-{decision}"
            emit(
                {
                    "type": "extension_ui_request",
                    "id": request_id,
                    "method": "confirm",
                    "title": "__ccteam_permission_v1__",
                    "message": json.dumps(envelope, separators=(",", ":")),
                    "timeout": 30 if decision == "timeout" else 1000,
                }
            )
            ui_response = wait_for_ui_response(request_id)
            confirmed = ui_response.get("confirmed") is True
            emit(
                {
                    "type": "tool_execution_start",
                    "toolCallId": envelope["toolCallId"],
                    "toolName": tool_name,
                    "args": envelope["input"],
                }
            )
            emit(
                {
                    "type": "tool_execution_end",
                    "toolCallId": envelope["toolCallId"],
                    "toolName": tool_name,
                    "result": {"content": []},
                    "isError": not confirmed,
                }
            )
            answer = "approved" if confirmed else "continued-after-deny"
            assistant("stop", f"{answer}:{tool_name}")
            emit({"type": "agent_settled"})
            continue
        if message == "dialogs":
            values = []
            dialogs = [
                {"id": "dialog-select", "method": "select", "title": "Pick", "options": ["A", "B"]},
                {"id": "dialog-confirm", "method": "confirm", "title": "Confirm", "message": "Proceed?"},
                {"id": "dialog-input", "method": "input", "title": "Input", "placeholder": "value"},
                {"id": "dialog-editor", "method": "editor", "title": "Editor", "prefill": "draft"},
            ]
            for dialog in dialogs:
                emit({"type": "extension_ui_request", **dialog})
                values.append(wait_for_ui_response(dialog["id"]))
            assistant("stop", json.dumps(values, separators=(",", ":")))
            emit({"type": "agent_settled"})
            continue
        if message == "dialog-hang":
            emit(
                {
                    "type": "extension_ui_request",
                    "id": "dialog-hang",
                    "method": "input",
                    "title": "Hang",
                    "placeholder": "wait",
                }
            )
            wait_for_ui_response("dialog-hang")
            assistant("stop", "dialog-cancelled")
            emit({"type": "agent_settled"})
            continue
        if message == "wait-steer":
            pending = "steer"
            continue
        if message == "wait-abort":
            pending = "abort"
            continue
        if message == "multi":
            emit({"type": "turn_end", "message": {}, "toolResults": []})
            emit({"type": "turn_end", "message": {}, "toolResults": []})
            emit({"type": "turn_end", "message": {}, "toolResults": []})
            assistant("stop", "multi-final")
            emit({"type": "agent_end", "messages": [], "willRetry": False})
        elif message == "retry":
            assistant("error", "retrying", error="overloaded", cost=0.02)
            emit({"type": "agent_end", "messages": [], "willRetry": True})
            emit({"type": "auto_retry_start", "attempt": 1, "maxAttempts": 3, "delayMs": 1, "errorMessage": "overloaded"})
            emit({"type": "auto_retry_end", "success": True, "attempt": 1})
            emit({"type": "agent_start"})
            assistant("stop", "retry-final", cost=0.03)
        elif message == "tool-preamble":
            assistant("toolUse", "I will run a tool", tool=True)
        elif message == "usage":
            assistant("toolUse", "tool preamble", tool=True, cost=0.10)
            emit(
                {
                    "type": "message_end",
                    "message": {
                        "role": "toolResult",
                        "toolCallId": "call-1",
                        "toolName": "bash",
                        "content": [{"type": "text", "text": "ok"}],
                        "usage": usage(5, 2, 0.20, 1),
                        "isError": False,
                        "timestamp": 1,
                    },
                }
            )
            emit(
                {
                    "type": "compaction_end",
                    "reason": "threshold",
                    "result": {"usage": usage(7, 3, 0.30, 1)},
                    "aborted": False,
                    "willRetry": True,
                }
            )
            assistant("stop", "usage-final", cost=0.40)
        elif message == "length":
            assistant("length", "partial")
        elif message == "error":
            assistant("error", "partial", error="provider exploded")
        elif message == "aborted":
            assistant("aborted", "partial", error="user abort")
        elif message == "unknown":
            assistant("future-stop", "partial")
        elif message == "no-terminal":
            pass
        else:
            assistant("stop", f"answer:{message}")
        emit({"type": "agent_settled"})
    elif kind == "steer":
        response(command)
        if pending != "steer":
            continue
        state["history"].append(command["message"])
        save()
        assistant("stop", f"steered:{command['message']}")
        emit({"type": "agent_settled"})
        pending = None
    elif kind == "abort":
        response(command)
        if pending == "abort":
            assistant("aborted", "", error="aborted by user")
            emit({"type": "agent_settled"})
            pending = None
    else:
        response(command, success=False, error=f"Unknown command: {kind}")
