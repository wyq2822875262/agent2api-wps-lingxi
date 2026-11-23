#!/usr/bin/env python3
"""Run the real debug plugin over Agent2API RPC against a loopback HTTP fixture."""
import argparse
import base64
import collections
import http.server
import json
import os
import platform
import queue
import subprocess
import tempfile
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import urlsplit

ID = "wps-lingxi"
COOKIE = "wps_sid=fixture-only-do-not-use"
PROFILE = "/api/public/v1/account/profile"
PLANS = "/api/aioffice/v1/sessions/plans"
BALANCE = "/api/public/v1/credits/balance"
TASKS = "/api/public/v1/tasks"
CLAIM = TASKS + "/daily_check_in/claim"
SESSIONS = "/api/aigc/v3/assistant/sessions"


class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, payload, code=200):
        raw = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        state = self.server.state
        path = self.path
        if path.startswith("http://"):
            url = urlsplit(path)
            assert url.netloc == f"127.0.0.1:{self.server.server_port}" and not url.query
            path = url.path
            state.proxy_calls[path] += 1
        state.calls[path] += 1
        if self.headers.get("Cookie") != COOKIE:
            self.reply({"result": "UserUnauthorized"})
        elif path == PROFILE:
            if state.redirect:
                self.send_response(302)
                self.send_header("Location", "/never-follow")
                self.end_headers()
            else:
                self.reply({"data": {"id": 123, "name": "WPS fixture"}})
        elif path == PLANS:
            self.reply({"result": "ok", "data": {"models": [
                {"key": "text-model", "name": "Test Model", "max_context_tokens": 10000},
                {"key": "text-model", "name": "Duplicate"}, {"key": ""},
            ]}})
        elif path == BALANCE:
            self.reply({"data": state.balance})
        elif path == TASKS:
            tasks = [{"task_key": "unrelated", "status": "claimable"}]
            if state.task is not None:
                tasks.append({"task_key": "daily_check_in", "status": state.task,
                              "extra": {"consecutive_days": 3}})
            self.reply({"result": "ok", "data": {"tasks": tasks}})
        else:
            self.reply({}, 404)

    def do_POST(self):
        state = self.server.state
        path, _, query = self.path.partition("?")
        state.calls[path] += 1
        assert self.headers["Cookie"] == COOKIE
        raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if path == CLAIM:
            today = datetime.now(timezone(timedelta(hours=8))).date().isoformat()
            assert query == "date=" + today and not raw
            if state.claim == "unknown":
                self.reply({"error": COOKIE}, 500)
            else:
                self.reply({"result": "ok", "data": {"extra": {"today_reward": 100, "consecutive_days": 4}}})
        elif path == SESSIONS:
            assert json.loads(raw) == {}
            sid = str(state.calls[path])
            if state.stream == "invalid-session":
                sid = "../invalid?redirect="
            self.reply({"result": "ok", "data": {"session_id": sid}})
        elif path == f"{SESSIONS}/{state.calls[SESSIONS]}/completions":
            assert self.headers["X-Cc-Version"] == "1" and self.headers["X-Client-Type"] == "lingxi-web"
            assert self.headers["X-Cc-Entrance"] == "default"
            assert self.headers["Accept"] == "text/event-stream"
            body = json.loads(raw)
            assert body["model"] == "text-model"
            assert body["user_input"]["text"] == body["question"] == "System: brief\n\nUser: test"
            if state.stream == "quota":
                self.reply({"result": "InsufficientCredits", "detail": COOKIE})
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.end_headers()
            if state.stream == "error":
                chunks = [b'event: error\ndata: {"type":"error","data":{"result":"UserUnauthorized"}}\n\n']
            else:
                text = 'event: text\ndata: {"type":"text","data":"你好"}\n\n'.encode()
                split = text.index("你".encode()) + 1
                chunks = [b'event: reasoning\r\ndata: {"type":"reasoning","data":{"text":"thinking"}}\r\n\r\n',
                          text[:split], text[split:], b'event: text\ndata: {"type":"text","data":"\\n"}\n\n']
                if state.stream in {"normal", "late-error"}:
                    chunks.append(b'event: text_end\ndata: {"type":"text_end"}\n\n')
                    if state.stream == "normal":
                        chunks.append(b'event: finish\ndata: \n\n')
                    else:
                        chunks.append('event: error\ndata: {"type":"error","data":"智点不足"}\n\n'.encode())
            for chunk in chunks:
                self.wfile.write(chunk)
                self.wfile.flush()
        else:
            self.reply({}, 404)


class Host:
    def __init__(self, exe, target, base, data_dir, proxy="direct"):
        self.proc = subprocess.Popen([exe], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, bufsize=1,
                                     env={**os.environ, "WPS_LINGXI_TEST_BASE": base})
        self.frames = queue.Queue()
        self.calls = 0
        self.accounts = {}
        self.ledger = {}
        self.logs = []
        threading.Thread(target=self.read, daemon=True).start()
        self.init, _ = self.call("init", {"env": {"data_dir": data_dir, "host_version": "offline-test",
                                                 "target": target, "proxy": proxy, "debug_traffic": False}})

    def read(self):
        try:
            for line in self.proc.stdout:
                self.frames.put(json.loads(line))
        finally:
            self.frames.put(None)

    def write(self, frame):
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def call(self, method, params=None, error=False):
        self.calls += 1
        call_id = self.calls
        if params is None:
            params = {"provider": ID, "account": "fixture"}
        self.write({"id": call_id, "method": method, "params": params})
        events = []
        while True:
            f = self.frames.get(timeout=30)
            assert f is not None, "plugin exited"
            if "method" in f:
                p = f["params"]
                match f["method"]:
                    case "store.get": result = self.accounts[p["account"]]
                    case "store.list": result = list(self.accounts.values())
                    case "store.ledger_get": result = self.ledger.get((p["account"], p["kind"]))
                    case "store.ledger_put":
                        self.ledger[p["account"], p["kind"]] = p["value"]
                        result = None
                    case _: raise AssertionError(f)
                self.write({"id": f["id"], "result": result})
            elif f["id"] == call_id:
                if "event" in f:
                    events.append(f)
                    if f["event"] == "chunk":
                        self.calls += 1
                        n = len(base64.b64decode(f["data"])) + 2048
                        self.write({"id": self.calls, "method": "credit", "params": {"id": call_id, "n": n}})
                else:
                    assert ("error" in f) == error, f
                    if error:
                        assert COOKIE not in str(f["error"])
                    return f.get("error" if error else "result"), events
            elif f.get("event") == "log":
                self.logs.append(f)

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            self.proc.wait(timeout=5)


def verify(host, state):
    def status(error):
        prefix = "provider_http:"
        assert error["code"].startswith(prefix), error
        return json.loads(error["code"][len(prefix):])["status"]

    def call(method, params=None, error=False):
        return host.call(method, params, error)[0]

    def normalize(sid, error=False):
        return call("account.normalize", {"provider": ID, "source": {"kind": "manual", "payload": {
            "methodIndex": 0, "inputs": {"wps_sid": sid}}}}, error)

    provider, = host.init
    assert provider["id"] == ID
    for feature in ["stateful", "chat", "usage", "checkin", "model_refresh"]:
        assert provider["capabilities"][feature]
    assert not provider["capabilities"]["refresh"]
    assert provider["account_features"]["usage"] is True
    methods = call("admin.action", {"provider": ID, "account": "", "action": "auth.methods", "payload": {}})
    assert methods["methods"][0]["prompts"][0]["secret"]

    assert status(normalize("malformed; session=other", True)) == 400
    assert not state.calls[PROFILE]
    assert status(normalize("expired-fixture", True)) == 401
    state.redirect = True
    normalize(COOKIE, True)
    assert not state.calls["/never-follow"]
    state.redirect = False
    account = normalize(COOKIE)
    assert account["identity"] == ID + ":123"
    assert account["public"]["name"] == "WPS fixture" and "fixture-only-do-not-use" not in str(account["public"])
    host.accounts["fixture"] = {"id": "fixture", "provider": ID, **account}
    print("PASS registration, secret-only import, upstream identity, expiry, no redirects")

    assert call("models.list")["models"] == [{"id": "text-model", "name": "Test Model", "object": "model",
                                               "owned_by": ID, "contextWindow": 10000}]
    call("models.refresh", {"provider": ID, "account": "fixture", "force": True})
    for balance, expected in [({"total_balance": 0, "balance": 99}, 0),
                              ({"total_balance": 123}, 123),
                              ({"balance": "1375"}, 1375),
                              ({"total_balance": None, "balance": 18}, 18)]:
        state.balance = balance
        usage = call("usage.query")
        assert usage["available"] == usage["balance"] == expected
        assert usage["unit"] == "智点" and "windows" not in usage
    for balance in [{}, None, {"total_balance": -1}, {"total_balance": "NaN", "balance": 0}]:
        state.balance = balance
        assert status(call("usage.query", error=True)) == 502
    secret = host.accounts["fixture"]["secret"]
    original_sid = secret["wps_sid"]
    secret["wps_sid"] = "expired-fixture"
    err = call("usage.query", error=True)
    assert status(err) == 401 and "expired-fixture" not in str(err)
    secret["wps_sid"] = original_sid
    print("PASS model catalog/refresh, wallet balance, zero, invalid response, and expiry")

    assert call("checkin.claim")["outcome"] == "inactive"
    state.task = "claimed"
    assert call("checkin.claim")["outcome"] == "done" and not state.calls[CLAIM]
    state.task = "locked"
    assert call("checkin.claim")["outcome"] == "inactive" and not state.calls[CLAIM]
    state.task = "incomplete"
    report = call("checkin.claim")
    assert report["outcome"] == "claimed" and report["credit"] == "100 智点" and report["streak"] == 4
    assert call("checkin.claim")["outcome"] == "done" and state.calls[CLAIM] == 1
    host.ledger.clear()
    state.claim = "unknown"
    assert call("checkin.claim")["outcome"] == "failed"
    assert call("checkin.claim")["outcome"] == "failed" and state.calls[CLAIM] == 2
    state.task = "claimed"
    assert call("checkin.claim")["outcome"] == "done" and state.calls[CLAIM] == 2
    print("PASS check-in eligibility, Beijing date, reward, confirmed and uncertain deduplication")

    params = {"provider": ID, "account": "fixture", "proxy": None, "headers": {}, "wire_model": "text-model",
              "stream": True, "body": {"model": "client-alias", "messages": [
                  {"role": "system", "content": "brief"}, {"role": "user", "content": "test"}]}}

    def conversation(stream=True, error=False):
        result, frames = host.call("conversation", {**params, "stream": stream}, error)
        return result, frames, b"".join(base64.b64decode(f["data"]) for f in frames if f["event"] == "chunk")

    _, frames, raw = conversation()
    assert frames[0]["headers"]["content-type"] == "text/event-stream"
    chunks = [json.loads(line[6:]) for line in raw.splitlines() if line.startswith(b"data: {")]
    deltas = [chunk["choices"][0]["delta"] for chunk in chunks]
    assert deltas == [{"role": "assistant"}, {"reasoning_content": "thinking"}, {"content": "你好"}, {"content": "\n"}, {}], deltas
    assert all(chunk["model"] == "client-alias" for chunk in chunks)
    assert raw.endswith(b"data: [DONE]\n\n") and chunks[-1]["choices"][0]["finish_reason"] == "stop"
    _, frames, raw = conversation(stream=False)
    assert frames[0]["headers"]["content-type"] == "application/json"
    assert json.loads(raw)["choices"][0]["message"] == {"role": "assistant", "content": "你好\n", "reasoning_content": "thinking"}
    state.stream = "truncated"
    err, _, raw = conversation(error=True)
    assert status(err) == 502 and b"[DONE]" not in raw
    state.stream = "error"
    err, frames, _ = conversation(error=True)
    assert status(err) == 401 and not frames
    state.stream = "late-error"
    err, _, raw = conversation(error=True)
    assert status(err) == 402 and b"[DONE]" not in raw
    state.stream = "quota"
    err, frames, _ = conversation(error=True)
    assert status(err) == 402 and not frames
    state.stream = "invalid-session"
    assert status(conversation(error=True)[0]) == 502
    calls = state.calls[SESSIONS]
    params["body"]["tools"] = [{"type": "function"}]
    assert status(conversation(error=True)[0]) == 400 and state.calls[SESSIONS] == calls
    assert len([path for path in state.calls if path.endswith("/completions")]) == 6
    assert all(COOKIE not in str(f) for f in host.logs)
    print("PASS multi-turn text, independent upstream sessions, SSE/JSON, truncation, auth/quota errors, unsupported tools")


def verify_proxy(host, state, proxy):
    account = {"id": "fixture", "provider": ID, "secret": {"wps_sid": COOKIE.split("=", 1)[1]}, "public": {}}
    host.accounts["fixture"] = account
    state.balance = {"total_balance": 123}
    methods = {"usage.query": BALANCE, "models.list": PLANS, "models.refresh": PLANS}
    for setting, use_proxy in [
        ({"state": "unset"}, True),
        ({"state": "static", "config": {"source": "direct"}}, False),
        ({"state": "static", "config": {"url": proxy}}, True),
        ({"state": "static", "config": {"config": {"source": "custom", "address": proxy}}}, True),
        ({"state": "resolved", "proxy": {"protocol": "http", "host": "127.0.0.1",
                                          "port": urlsplit(proxy).port}}, True),
    ]:
        account["proxy"] = setting
        for method, path in methods.items():
            calls, proxy_calls = state.calls[path], state.proxy_calls[path]
            result, _ = host.call(method)
            assert state.calls[path] == calls + 1
            assert state.proxy_calls[path] == proxy_calls + int(use_proxy)
            if method == "usage.query":
                assert result["available"] == 123
            elif method == "models.list":
                assert result["models"][0]["id"] == "text-model"

    for setting in [
        {"state": "failed", "message": "proxy-fixture-private"},
        {"state": "static", "config": {"source": "pool", "url": proxy}},
        {"state": "static", "config": {"url": "file://proxy-fixture-private/invalid"}},
        {"state": "static", "config": {"url": "direct"}},
        {"state": "static", "config": {}},
    ]:
        account["proxy"] = setting
        for method, path in methods.items():
            calls = state.calls[path]
            error, _ = host.call(method, error=True)
            assert json.loads(error["code"].removeprefix("provider_http:"))["status"] == 503
            assert "proxy-fixture-private" not in str(error)
            assert state.calls[path] == calls
    print("PASS models and balance: inherited, explicit direct, legacy and resolved proxies; invalid proxies fail closed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--target", default="aarch64-apple-darwin" if platform.system() == "Darwin"
                        else "x86_64-unknown-linux-gnu")
    args = parser.parse_args()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    server.state = argparse.Namespace(calls=collections.Counter(), proxy_calls=collections.Counter(),
                                      balance={"total_balance": 0},
                                      task=None, claim="ok", stream="normal", redirect=False)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="wps-lingxi-test-") as tmp:
            base = f"http://127.0.0.1:{server.server_port}"
            host = Host(args.exe.resolve(), args.target, base, tmp)
            try:
                verify(host, server.state)
            finally:
                host.close()
            host = Host(args.exe.resolve(), args.target, base, tmp, proxy=base)
            try:
                verify_proxy(host, server.state, base)
            finally:
                host.close()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
