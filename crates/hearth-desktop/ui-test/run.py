#!/usr/bin/env python3
"""Runs the desktop UI's user flows in headless Chrome against a mock backend.

    python crates/hearth-desktop/ui-test/run.py [chrome-binary]
"""

import functools
import html
import http.server
import json
import os
import shutil
import subprocess
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
UI = os.path.join(HERE, "..", "ui")


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


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=UI))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/index.html"
    try:
        out = subprocess.run(
            [find_chrome(), "--headless=new", "--no-sandbox", "--disable-gpu", "--virtual-time-budget=60000", "--dump-dom", url],
            capture_output=True,
            timeout=120,
        ).stdout.decode("utf-8", "replace")
    finally:
        server.shutdown()

    start = out.find('data-results="')
    if start < 0:
        sys.exit("UI tests produced no results (did the page fail to load?)")
    raw = out[start + len('data-results="') :]
    results = json.loads(html.unescape(raw[: raw.index('"')]))
    failed = [r for r in results if not r["ok"]]
    for r in results:
        print(("PASS  " if r["ok"] else "FAIL  ") + r["name"] + ("" if r["ok"] else f": {r['error']}"))
    print(f"\n{len(results) - len(failed)}/{len(results)} UI flows passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
