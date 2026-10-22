#!/usr/bin/env python3
"""End-to-end test: real `hearth` processes talking over the real network.

Covers invites and chat, a blind storage node, voice (checked by analysing
recorded audio), screen-share frames (verified byte for byte), and offline
delivery through the node.

    python scripts/e2e.py target/release/hearth
"""

import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import wave

BIN = os.path.abspath(sys.argv[1])
WORK = tempfile.mkdtemp(prefix="hearth-e2e-")
failures = []


class Peer:
    def __init__(self, name, *args):
        self.name = name
        self.dir = os.path.join(WORK, name)
        self.lines = []
        self.lock = threading.Lock()
        self.proc = subprocess.Popen(
            [BIN, "--data-dir", self.dir, *args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={**os.environ, "HEARTH_NO_UPDATE_CHECK": "1"},
        )
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        for raw in self.proc.stdout:
            line = raw.decode("utf-8", "replace").rstrip()
            with self.lock:
                self.lines.append(line)

    def send(self, line):
        self.proc.stdin.write((line + "\n").encode())
        self.proc.stdin.flush()

    def wait_for(self, pattern, timeout=90, after=0):
        """Waits for a line matching `pattern` (from index `after`); returns the match."""
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                for line in self.lines[after:]:
                    m = rx.search(line)
                    if m:
                        return m
            if self.proc.poll() is not None:
                break
            time.sleep(0.2)
        with self.lock:
            tail = "\n    ".join(self.lines[-15:])
        raise AssertionError(f"{self.name}: no line matching {pattern!r} within {timeout}s. Last output:\n    {tail}")

    def mark(self):
        with self.lock:
            return len(self.lines)

    def quit(self):
        if self.proc.poll() is None:
            try:
                self.send("/quit")
                self.proc.wait(timeout=20)
            except Exception:
                self.proc.kill()


def peer_id(name):
    out = subprocess.run([BIN, "--data-dir", os.path.join(WORK, name), "id"], capture_output=True, check=True)
    return out.stdout.decode().strip()


def check(name, fn):
    t = time.time()
    try:
        fn()
        print(f"PASS  {name}  ({time.time() - t:.1f}s)", flush=True)
    except Exception as e:
        failures.append(name)
        print(f"FAIL  {name}: {e}", flush=True)


def tone_hz(path):
    """Returns (seconds of audible audio, estimated pitch in Hz)."""
    w = wave.open(path)
    n, rate = w.getnframes(), w.getframerate()
    x = struct.unpack("<%dh" % n, w.readframes(n))
    loud = []
    for i in range(0, n - 960, 960):
        win = x[i : i + 960]
        if math.sqrt(sum(s * s for s in win) / 960) > 1000:
            loud.extend(win)
    secs = len(loud) / rate
    crossings = sum(1 for a, b in zip(loud, loud[1:]) if (a < 0) != (b < 0))
    return secs, (crossings / 2 / secs if secs else 0)


def main():
    ids = {n: peer_id(n) for n in ("alice", "bob", "node")}
    node = Peer("node", "node")
    alice = Peer("alice", "chat", "--name", "alice", "--tone", "440", "--record", os.path.join(WORK, "alice.wav"), "--video-test")
    bob = Peer("bob", "chat", "--name", "bob", "--tone", "660", "--record", os.path.join(WORK, "bob.wav"), "--video-test")
    peers = [node, alice, bob]

    try:
        for p in peers:
            p.wait_for(r"^online\.")

        def server_and_invite():
            alice.send("/new Den")
            alice.wait_for(r"-> Den #general")
            alice.send(f"/host {ids['node']}")
            alice.wait_for(r"now stored on node")
            alice.send(f"/invite {ids['bob']}")
            bob.wait_for(r"invited you to Den")
            bob.send("/accept 0")
            bob.wait_for(r"server Den synced")

        def chat():
            alice.send("hello from alice")
            bob.wait_for(r"^<alice> hello from alice$")
            bob.send("hi alice")
            alice.wait_for(r"^<bob> hi alice$")

        def voice():
            a, b = alice.mark(), bob.mark()
            alice.send("/voice")
            bob.send("/voice")
            alice.wait_for(r"General: .*bob", after=a)
            bob.wait_for(r"General: .*alice", after=b)
            time.sleep(8)  # let audio flow; analysed after shutdown

        def screen_share():
            alice.send("/live")
            alice.wait_for(r"streaming synthetic video")
            time.sleep(1)
            bob.send("/watch alice")
            m = bob.wait_for(r"\[video\] 90 frames .* (\d+) corrupt", timeout=45)
            assert m.group(1) == "0", f"{m.group(1)} corrupt frames"
            alice.send("/unlive")

        def friends_and_dms():
            alice.send("/friend bob")
            bob.wait_for(r"alice sent you a friend request")
            bob.send("/friend alice")
            alice.wait_for(r"you and bob are friends")
            bob.wait_for(r"you and alice are friends")
            # The peer with the larger ID joins the DM the other one created.
            joiner = alice if ids["alice"] > ids["bob"] else bob
            other = "bob" if joiner is alice else "alice"
            joiner.wait_for(rf"dm with {other} ready")
            alice.send("/dm bob hello friend")
            bob.wait_for(r"^\[dm alice\] hello friend$")
            bob.send("/dm alice hey you")
            alice.wait_for(r"^\[dm bob\] hey you$")

        def reactions():
            alice.send("react to this")
            bob.wait_for(r"^<alice> react to this$")
            bob.send("/react 🔥")
            alice.wait_for(r"reactions on your message: 🔥1")

        payload = os.urandom(3 * 1024 * 1024 + 123)  # spans several encrypted chunks
        with open(os.path.join(WORK, "payload.bin"), "wb") as f:
            f.write(payload)

        def file_transfer():
            alice.send(f"/attach {os.path.join(WORK, 'payload.bin')} here's a file")
            bob.wait_for(r"^<alice> here's a file \[file payload\.bin")
            bob.send(f"/save {os.path.join(WORK, 'bob-got')}")
            bob.wait_for(r"^saved .*payload\.bin \(3145851 bytes\)$", timeout=120)
            got = open(os.path.join(WORK, "bob-got", "payload.bin"), "rb").read()
            assert got == payload, "downloaded file differs from the original"

        photo = os.urandom(512 * 1024)
        with open(os.path.join(WORK, "photo.jpg"), "wb") as f:
            f.write(photo)

        def offline_delivery():
            bob.quit()
            alice.send("posted while bob was away")
            alice.send(f"/attach {os.path.join(WORK, 'photo.jpg')} and a photo")
            time.sleep(6)  # the node stores the message and fetches the file
            alice.quit()
            back = Peer("bob", "chat", "--name", "bob")
            peers.append(back)
            back.wait_for(r"^<alice> posted while bob was away$", timeout=120)
            back.wait_for(r"^<alice> and a photo \[file photo\.jpg", timeout=60)
            back.send(f"/save {os.path.join(WORK, 'from-node')}")
            back.wait_for(r"^saved .*photo\.jpg", timeout=120)
            got = open(os.path.join(WORK, "from-node", "photo.jpg"), "rb").read()
            assert got == photo, "file from the node differs from the original"

        check("invite + server sync", server_and_invite)
        check("chat both ways", chat)
        check("voice channel presence", voice)
        check("screen-share frames, verified", screen_share)
        check("friend request, accept, and DMs both ways", friends_and_dms)
        check("reactions", reactions)
        check("3 MB file transfer, byte-verified", file_transfer)
        check("offline delivery of message and file via blind node", offline_delivery)
    finally:
        for p in peers:
            p.quit()
        node.proc.kill()

    def audio():
        a_secs, a_hz = tone_hz(os.path.join(WORK, "alice.wav"))
        b_secs, b_hz = tone_hz(os.path.join(WORK, "bob.wav"))
        print(f"      alice heard {a_secs:.1f}s at ~{a_hz:.0f} Hz; bob heard {b_secs:.1f}s at ~{b_hz:.0f} Hz")
        assert a_secs > 4 and abs(a_hz - 660) < 60, "alice should hear bob's 660 Hz tone"
        assert b_secs > 4 and abs(b_hz - 440) < 40, "bob should hear alice's 440 Hz tone"

    check("voice audio both ways (pitch analysis)", audio)

    if failures:
        print(f"\n{len(failures)} failed: {', '.join(failures)}\nlogs kept in {WORK}")
        for p in peers:
            print(f"\n--- {p.name} ---\n" + "\n".join(p.lines[-40:]))
        sys.exit(1)
    shutil.rmtree(WORK, ignore_errors=True)
    print("\nall end-to-end checks passed")


if __name__ == "__main__":
    main()
