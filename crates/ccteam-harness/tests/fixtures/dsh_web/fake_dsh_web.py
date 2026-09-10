#!/usr/bin/env python3
"""Fake `dsh web` server for hermetic ccteam runtime tests.

Models the three vendor behaviors ccteam's runtime supervision depends on, as
dsh 0.1.5 implements them (`packages/client/connection/src/browser-auth.ts`):

  1. it binds an ephemeral port and prints ONE readiness line carrying the
     launch token — `dsh web: http://127.0.0.1:<port>/?token=<tok>` — behind
     another `dsh web:` line that is not a URL at all;
  2. every request without an accepted cookie is answered 401, including the
     index — a live server saying "not you", never "not started";
  3. `GET /?token=<tok>` answers 303 and sets a cookie bound to the HOST
     AUTHORITY of that exchange, so a credential minted for one authority is
     worthless on another.

The cookie is deterministic (no timestamp) so the test can predict it: the
state file names the exact `name=value` the loopback authority mints, which is
what ccteam must end up holding.

Usage: fake_dsh_web.py [ignored dsh argv...]. Env knobs:

    CCTEAM_FAKE_DSH_STATE          write {port, token, cookie} JSON here first
    CCTEAM_FAKE_DSH_NO_AUTH=1      serve everything, print a token-less URL
                                   (an older dsh, or a future one dropping auth)
    CCTEAM_FAKE_DSH_READY_STDERR=1 print the readiness lines on stderr
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import secrets
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SIGNING_SECRET = secrets.token_bytes(32)
NO_AUTH = os.environ.get("CCTEAM_FAKE_DSH_NO_AUTH") == "1"
CHALLENGE = b"dsh web authentication required; reopen the URL printed by dsh web.\n"
PAGE = b"<!doctype html><html><head><title>dsh</title></head><body>dsh web fake</body></html>"


def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


TOKEN = b64url(secrets.token_bytes(32))


def cookie_name(authority: str) -> str:
    return "dsh-auth-" + b64url(hashlib.sha256(authority.encode()).digest())


def cookie_value(authority: str) -> str:
    body = b64url(authority.encode())
    signature = hmac.new(SIGNING_SECRET, body.encode(), hashlib.sha256).digest()
    return f"v1.{body}.{b64url(signature)}"


def cookie_for(authority: str) -> str:
    return f"{cookie_name(authority)}={cookie_value(authority)}"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args) -> None:  # keep the test output clean
        pass

    def do_GET(self) -> None:
        self.serve("GET")

    def do_POST(self) -> None:
        self.serve("POST")

    def serve(self, method: str) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        if NO_AUTH:
            self.page()
            return
        authority = self.headers.get("Host") or ""
        path, _, query = self.path.partition("?")
        tokens = urllib.parse.parse_qs(query).get("token") or []
        if tokens:
            if (method == "GET" and path == "/" and len(tokens) == 1
                    and tokens[0] == TOKEN and authority):
                self.exchange(authority)
            else:
                self.challenge()
            return
        if self.authenticated(authority):
            self.page()
        else:
            self.challenge()

    def authenticated(self, authority: str) -> bool:
        if not authority:
            return False
        name = cookie_name(authority)
        for segment in (self.headers.get("Cookie") or "").split(";"):
            key, sep, value = segment.partition("=")
            if sep and key.strip() == name:
                return hmac.compare_digest(value.strip(), cookie_value(authority))
        return False

    def exchange(self, authority: str) -> None:
        self.send_response(303)
        self.send_header("Location", "/")
        self.send_header("Cache-Control", "no-store")
        self.send_header(
            "Set-Cookie",
            f"{cookie_for(authority)}; Max-Age=2592000; Path=/; HttpOnly; SameSite=Strict",
        )
        self.send_header("Content-Length", "0")
        self.end_headers()

    def page(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

    def challenge(self) -> None:
        self.send_response(401)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(CHALLENGE)))
        self.end_headers()
        self.wfile.write(CHALLENGE)


def main() -> None:
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_port
    state = os.environ.get("CCTEAM_FAKE_DSH_STATE")
    if state:
        # Written BEFORE readiness: a test that saw the line can read this.
        with open(state, "w") as handle:
            json.dump(
                {
                    "port": port,
                    "token": TOKEN,
                    "cookie": cookie_for(f"127.0.0.1:{port}"),
                },
                handle,
            )
    stream = sys.stderr if os.environ.get("CCTEAM_FAKE_DSH_READY_STDERR") == "1" else sys.stdout
    url = f"http://127.0.0.1:{port}/" if NO_AUTH else f"http://127.0.0.1:{port}/?token={TOKEN}"
    lan = f"http://192.168.1.5:{port}/" if NO_AUTH else f"http://192.168.1.5:{port}/?token={TOKEN}"
    print("dsh web: opening the default browser; pass --no-open to disable",
          file=stream, flush=True)
    print(f"dsh web: {url} (LAN: {lan})", file=stream, flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
