#!/usr/bin/env python3
"""Runs the desktop UI's user flows in headless Chrome against a mock backend.

    python crates/hearth-desktop/ui-test/run.py [chrome-binary]
"""

import functools
import http.server
import json
import os
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
UI = os.path.join(HERE, "..", "ui")
CSP = json.load(open(os.path.join(HERE, "..", "tauri.conf.json")))["app"]["security"]["csp"]


def find_chrome():
    if len(sys.argv) > 1:
        return sys.argv[1]
    for name in ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"):
        path = shutil.which(name)
        if path:
            return path
    sys.exit("no Chrome/Chromium found; pass its path as an argument")


class Handler(http.server.SimpleHTTPRequestHandler):
    """Serves ui/, with the mock backend injected ahead of app.js."""

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html"):
            page = open(os.path.join(UI, "index.html"), encoding="utf-8").read()
            page = page.replace('<script src="app.js"', '<script src="/__test/mock.js"></script>\n    <script src="app.js"', 1)
            # Enforce the app's real content security policy, so a blocked image or video fails here too.
            page = page.replace("<head>", f'<head>\n    <meta http-equiv="Content-Security-Policy" content="{CSP}">', 1)
            body = page.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path == "/__test/mock.js":
            body = open(os.path.join(HERE, "mock.js"), "rb").read()
            self.send_response(200)
            self.send_header("Content-Type", "text/javascript")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *args):
        pass


class Chrome:
    """Headless Chrome driven over the DevTools protocol on a pipe (fds 3 and 4).

    Real time, not --virtual-time-budget: virtual time races ahead of work done
    on other threads (like video encoding), which made encoder tests flaky.
    """

    def __init__(self, binary, url):
        self.to_chrome_r, self.to_chrome_w = os.pipe()
        self.from_chrome_r, self.from_chrome_w = os.pipe()
        self.proc = subprocess.Popen(
            [
                binary, "--headless=new", "--no-sandbox", "--disable-gpu", "--remote-debugging-pipe", "--no-first-run",
                # Keep the page "foreground": throttled timers and paused video callbacks stall media tests.
                "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows",
                url,
            ],
            close_fds=False,  # Python would otherwise close fds 3 and 4 after the dup2 below
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            preexec_fn=self._wire_fds,
        )
        os.close(self.to_chrome_r)
        os.close(self.from_chrome_w)
        self.reader = os.fdopen(self.from_chrome_r, "rb")
        self.next_id = 0
        self.buf = b""

    def _wire_fds(self):
        # Chrome reads commands on fd 3 and writes replies on fd 4.
        os.dup2(self.to_chrome_r, 3)
        os.dup2(self.from_chrome_w, 4)

    def send(self, method, params=None, session=None):
        self.next_id += 1
        msg = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        os.write(self.to_chrome_w, json.dumps(msg).encode() + b"\0")
        while True:
            reply = self._read()
            if reply.get("id") == self.next_id:
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply.get("result", {})

    def _read(self):
        while b"\0" not in self.buf:
            chunk = self.reader.read1(65536)
            if not chunk:
                raise RuntimeError("Chrome exited")
            self.buf += chunk
        raw, self.buf = self.buf.split(b"\0", 1)
        return json.loads(raw)

    def close(self):
        self.proc.kill()
        self.proc.wait()


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=UI))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/index.html"
    chrome = Chrome(find_chrome(), url)
    progress = ""
    try:
        target = None
        for _ in range(100):
            pages = [t for t in chrome.send("Target.getTargets")["targetInfos"] if t["type"] == "page" and t["url"].startswith(url.rsplit("/", 1)[0])]
            if pages:
                target = pages[0]["targetId"]
                break
            time.sleep(0.1)
        if not target:
            sys.exit("Chrome never opened the test page")
        session = chrome.send("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
        chrome.send("Emulation.setFocusEmulationEnabled", {"enabled": True}, session)
        deadline = time.time() + 240
        results = None
        while time.time() < deadline:
            expr = "JSON.stringify([document.body && document.body.dataset.results || '', document.body && document.body.dataset.progress || ''])"
            r = chrome.send("Runtime.evaluate", {"expression": expr, "returnByValue": True}, session)
            value, progress = json.loads(r.get("result", {}).get("value") or '["", ""]')
            if value:
                results = json.loads(value)
                break
            time.sleep(0.5)
    finally:
        chrome.close()
        server.shutdown()

    if results is None:
        sys.exit(f"UI tests produced no results within 4 minutes; last progress: {progress or 'none (page never booted)'}")
    failed = [r for r in results if not r["ok"]]
    for r in results:
        print(("PASS  " if r["ok"] else "FAIL  ") + r["name"] + ("" if r["ok"] else f": {r['error']}"))
    print(f"\n{len(results) - len(failed)}/{len(results)} UI flows passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
