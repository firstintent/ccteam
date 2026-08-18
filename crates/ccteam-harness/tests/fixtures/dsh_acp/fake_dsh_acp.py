#!/usr/bin/env python3
"""Fake DSH runtime ACP peer for hermetic ccteam tests.

Models the ccteam-owned Cordis plugin as it lives inside a real `dsh web`
runtime (v0.10.3): it LISTENS on a unix socket and serves every accepted
connection as an isolated ACP peer with its own session map, exactly like
`DshSocketTransport` -> `DshAcpServer`. One connection = one ccteam hire.

    initialize -> notifications/initialized -> session/new|load ->
    session/prompt -> session/cancel

Credentials are never read from the environment: they arrive per session in
`params._meta.ccteam` (sid / bearer / mcpUrl / approvalMode) and are echoed into
the dump so tests can assert on them. `mcpServers` on a session method is still
refused — ccteam's tool face is the daemon's MCP endpoint, not an ACP param.

Usage: fake_dsh_acp.py <socket-path>. Prints one readiness line on stdout, then
serves until killed. Env knobs:

    CCTEAM_DSH_ACP_DUMP      append one JSON record per request/connection event
    CCTEAM_DSH_AGENT_NAME    override `agentInfo.name` (default ccteam-dsh-client)
    CCTEAM_DSH_AGENT_VERSION override `agentInfo.version` (default 0.10.3-alpha.0)
    CCTEAM_DSH_LOAD_FAIL     `1` => every session/load fails
"""
from __future__ import annotations

import json
import os
import socket
import sys
import threading
import uuid

DEFAULT_AGENT_NAME = "ccteam-dsh-client"
DEFAULT_AGENT_VERSION = "0.10.3-alpha.0"
DEFAULT_MODEL = "deepseek-v4-flash"
WINDOW = 131072

DUMP_LOCK = threading.Lock()


def dump(record: dict) -> None:
    path = os.environ.get("CCTEAM_DSH_ACP_DUMP")
    if not path:
        return
    with DUMP_LOCK:
        with open(path, "a") as f:
            f.write(json.dumps(record, separators=(",", ":")) + "\n")


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


def ccteam_meta(params: dict) -> dict | None:
    meta = params.get("_meta")
    if not isinstance(meta, dict):
        return None
    ccteam = meta.get("ccteam")
    return ccteam if isinstance(ccteam, dict) else None


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
    return {"sessionId": session_id, "models": models_block(model)}


def text_prompt(params: dict) -> str:
    text = ""
    for block in params.get("prompt") or []:
        if isinstance(block, dict) and block.get("type") == "text":
            text += block.get("text") or ""
    return text


class Peer:
    """One accepted connection: its own sessions, like the real plugin."""

    def __init__(self, conn: socket.socket, conn_id: int) -> None:
        self.conn = conn
        self.conn_id = conn_id
        self.stream = conn.makefile("rwb")
        self.session_id = "dsh-fake-session"
        self.model = DEFAULT_MODEL
        self.lock = threading.Lock()

    def emit(self, obj: dict) -> None:
        with self.lock:
            self.stream.write(json.dumps(obj, separators=(",", ":")).encode() + b"\n")
            self.stream.flush()

    def notif(self, method: str, params: dict) -> None:
        self.emit({"jsonrpc": "2.0", "method": method, "params": params})

    def reply(self, req_id, result) -> None:
        self.emit({"jsonrpc": "2.0", "id": req_id, "result": result})

    def err(self, req_id, code, message: str, data=None) -> None:
        payload = {"code": code, "message": message}
        if data is not None:
            payload["data"] = data
        self.emit({"jsonrpc": "2.0", "id": req_id, "error": payload})

    def serve(self) -> None:
        dump({"conn": self.conn_id, "method": "connection/opened", "params": {}})
        try:
            for raw in self.stream:
                line = raw.decode(errors="replace").strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    continue
                self.handle(msg)
        finally:
            dump({"conn": self.conn_id, "method": "connection/closed", "params": {}})
            try:
                self.stream.close()
            finally:
                self.conn.close()

    def handle(self, msg: dict) -> None:
        method = msg.get("method")
        req_id = msg.get("id")
        params = msg.get("params") or {}

        if method and method.startswith("session/"):
            dump({"conn": self.conn_id, "method": method, "params": params})
            if params.get("mcpServers"):
                self.err(
                    req_id, -32602, "DSH ccteam transport must not receive ACP mcpServers"
                )
                return

        if method is None and req_id is not None and ("result" in msg or "error" in msg):
            return

        if method == "initialize":
            self.reply(
                req_id,
                {
                    "protocolVersion": "0.4",
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {
                            "embeddedContext": True,
                            "image": False,
                        },
                    },
                    "agentInfo": {
                        "name": os.environ.get(
                            "CCTEAM_DSH_AGENT_NAME", DEFAULT_AGENT_NAME
                        ),
                        "version": os.environ.get(
                            "CCTEAM_DSH_AGENT_VERSION", DEFAULT_AGENT_VERSION
                        ),
                    },
                    "authMethods": [],
                },
            )
            return

        if method == "notifications/initialized":
            return

        if method == "session/new":
            if not isinstance(params.get("cwd"), str) or not params["cwd"].strip():
                self.err(req_id, -32602, "session/new requires cwd")
                return
            picked = agent_options(params)
            if picked is None:
                self.err(req_id, "MISSING_CREDENTIAL", "agentOptions provider/model required")
                return
            provider, model = picked
            if provider == "unknown" or model == "unknown":
                self.err(req_id, "UNKNOWN_MODEL", f"unknown model: {provider}/{model}")
                return
            self.model = model
            self.session_id = f"dsh-fake-{uuid.uuid4()}"
            self.reply(req_id, session_result(self.session_id, self.model))
            return

        if method == "session/load":
            sid = params.get("sessionId") or ""
            if os.environ.get("CCTEAM_DSH_LOAD_FAIL") == "1" or sid.startswith("missing"):
                self.err(
                    req_id,
                    -32000,
                    f'session "{sid}" not found',
                    {"message": f'session "{sid}" not found', "name": "Error"},
                )
                return
            picked = agent_options(params)
            if picked is None:
                self.err(req_id, "MISSING_CREDENTIAL", "agentOptions provider/model required")
                return
            _, model = picked
            self.model = model
            self.session_id = sid
            self.reply(req_id, session_result(self.session_id, self.model))
            return

        if method == "session/prompt":
            text = text_prompt(params)
            answer = f"echo:{text}" if text else "echo:"
            mid = len(answer) // 2 or 1
            for chunk in [answer[:mid], answer[mid:]]:
                self.notif(
                    "session/update",
                    {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": chunk},
                        },
                    },
                )
            self.reply(
                req_id,
                {
                    "stopReason": "end_turn",
                    "_meta": {
                        "inputTokens": 12,
                        "outputTokens": 5,
                        "totalTokens": 17,
                        "modelId": self.model,
                    },
                },
            )
            return

        if method == "session/cancel":
            self.notif(
                "session/update",
                {
                    "sessionId": self.session_id,
                    "update": {
                        "sessionUpdate": "turn/end",
                        "kind": "aborted",
                        "reason": {"kind": "user"},
                    },
                },
            )
            if req_id is not None:
                self.reply(req_id, {})
            return

        if req_id is not None:
            self.err(req_id, -32601, f"Method not found: {method}")


def main() -> None:
    if len(sys.argv) < 2:
        sys.stderr.write("fake_dsh_acp: usage: fake_dsh_acp.py <socket-path>\n")
        sys.exit(2)
    path = sys.argv[1]
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen(16)
    print(f"fake dsh acp listening on {path}", flush=True)

    conn_id = 0
    while True:
        try:
            conn, _ = server.accept()
        except OSError:
            break
        conn_id += 1
        threading.Thread(target=Peer(conn, conn_id).serve, daemon=True).start()


if __name__ == "__main__":
    main()
