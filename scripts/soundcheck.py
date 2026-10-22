#!/usr/bin/env python3
"""End-to-end sound check: does a call keep working while people come and go?

Three real `hearth` peers each send a different tone and record what they
hear. People join, leave, rejoin and turn cameras on mid-call; afterwards each
recording is cut into 20 ms windows and checked, phase by phase, for exactly
the tones that should be audible then.

    python scripts/soundcheck.py target/release/hearth [--soak SECONDS]

--soak adds a long steady phase at the end, for longevity runs.
"""

import argparse
import math
import os
import shutil
import struct
import sys
import time
import wave

parser = argparse.ArgumentParser()
parser.add_argument("bin")
parser.add_argument("--soak", type=float, default=0, help="extra seconds of steady call at the end")
args = parser.parse_args()
sys.argv[1:] = [args.bin]  # e2e reads the binary from argv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e  # noqa: E402  (reuses its Peer driver)

TONES = {"alice": 440, "bob": 660, "carol": 1000}
PHASE = 6.0  # seconds per phase
SETTLE = 1.5  # ignore this much at the start of each phase (joins, jitter buffer)
WIN = 960  # 20 ms at 48 kHz


def power(win, hz, rate=48000):
    """Goertzel: energy of one frequency in a window, normalised by window energy."""
    k = 2 * math.cos(2 * math.pi * hz / rate)
    s1 = s2 = 0.0
    for x in win:
        s1, s2 = x + k * s1 - s2, s1
    p = s1 * s1 + s2 * s2 - k * s1 * s2
    total = sum(x * x for x in win) or 1.0
    return p / (total * len(win) / 2)


def windows(path):
    w = wave.open(path)
    n = w.getnframes()
    x = struct.unpack("<%dh" % n, w.readframes(n))
    return [x[i : i + WIN] for i in range(0, n - WIN, WIN)]


def judge(name, path, phases):
    """phases: [(start_s, end_s, {expected tone names})] in this recording's time."""
    wins = windows(path)
    problems = []
    for start, end, expect in phases:
        lo, hi = int((start + SETTLE) * 50), int(end * 50)
        span = wins[lo:hi]
        if len(span) < 25:
            problems.append(f"{start:.0f}-{end:.0f}s: recording too short ({len(span)} windows)")
            continue
        for who, hz in TONES.items():
            if who == name:
                continue
            present = sum(1 for win in span if max(map(abs, win)) > 500 and power(win, hz) > 0.05)
            frac = present / len(span)
            want = who in expect
            if want and frac < 0.95:
                problems.append(f"{start:.0f}-{end:.0f}s: heard {who} in only {frac:.0%} of the time")
            if not want and frac > 0.05:
                problems.append(f"{start:.0f}-{end:.0f}s: still heard {who} ({frac:.0%}) after they left")
        print(f"      {name} {start:5.1f}-{end:5.1f}s expects {sorted(expect) or 'silence'}", flush=True)
    assert not problems, f"{name}: " + "; ".join(problems)


def main():
    work = e2e.WORK
    peers = {n: e2e.Peer(n, "chat", "--name", n, "--tone", str(hz), "--record", os.path.join(work, f"{n}.wav"), "--video-test") for n, hz in TONES.items()}
    alice, bob, carol = peers["alice"], peers["bob"], peers["carol"]
    ids = {n: e2e.peer_id(n) for n in TONES}
    joined = {}  # when each peer's current recording started
    timeline = []  # (time, who's in the call)

    def join(p):
        m = p.mark()
        p.send("/voice")
        p.wait_for(r"joined General", after=m)
        joined[p.name] = time.time()

    def leave(p):
        m = p.mark()
        p.send("/leave")
        p.wait_for(r"left voice", after=m)

    def phase(present):
        timeline.append((time.time(), set(present)))
        time.sleep(PHASE)

    try:
        for p in peers.values():
            p.wait_for(r"^online\.")
        alice.send("/new Den")
        alice.wait_for(r"-> Den #general")
        alice.send(f"/invite {ids['bob']}")
        bob.wait_for(r"invited you to Den")
        bob.send("/accept 0")
        bob.wait_for(r"server Den synced")
        # Carol joins the way a scanned QR code does: with an invite link.
        m = alice.mark()
        alice.send("/link")
        link = alice.wait_for(r"(hearth://join/\S+)", after=m).group(1)
        carol.send(f"/open {link}")
        carol.wait_for(r"invite code accepted")
        carol.wait_for(r"server Den synced")
        bad = link[:-4] + ("0000" if not link.endswith("0000") else "1111")
        carol.send(f"/open {bad}")  # already a member: refused locally
        carol.wait_for(r"already in that server")
        print("PASS  joined by invite link", flush=True)

        join(alice)
        join(bob)
        phase({"alice", "bob"})
        join(carol)
        phase({"alice", "bob", "carol"})
        alice.send("/cam")  # video shares the connection; audio must not suffer
        alice.wait_for(r"camera on")
        phase({"alice", "bob", "carol"})
        leave(carol)
        phase({"alice", "bob"})
        join(carol)  # rejoining restarts carol's packet numbering
        phase({"alice", "bob", "carol"})
        alice.send("/nocam")
        leave(bob)
        phase({"alice", "carol"})
        join(bob)
        phase({"alice", "bob", "carol"})
        if args.soak:
            timeline.append((time.time(), {"alice", "bob", "carol"}))
            time.sleep(args.soak)
        end = time.time()
        # Connectivity and quality as each peer measured it.
        for p in peers.values():
            m = p.mark()
            p.send("/stats")
            for _ in range(2):
                line = p.wait_for(r"ping .* got (\d+) lost (\d+) late (\d+)", after=m)
                got, lost, late = map(int, line.groups())
                print(f"      {p.name}: {line.string.strip()}", flush=True)
                assert got > 100, f"{p.name}: only {got} packets"
                assert lost + late <= 0.03 * got, f"{p.name}: {lost} lost, {late} late of {got}"
                m = p.lines.index(line.string, m) + 1
    finally:
        for p in peers.values():
            p.quit()

    def phases_for(name):
        out = []
        for i, (t, present) in enumerate(timeline):
            t_end = timeline[i + 1][0] if i + 1 < len(timeline) else end
            if name not in present or t < joined[name]:
                continue  # not in the call, or before this recording began
            out.append((t - joined[name], t_end - joined[name] - 0.3, present - {name}))
        return out

    failures = []
    for name in TONES:
        try:
            judge(name, os.path.join(work, f"{name}.wav"), phases_for(name))
            print(f"PASS  {name} heard the right people throughout", flush=True)
        except AssertionError as e:
            failures.append(name)
            print(f"FAIL  {e}", flush=True)
    if failures:
        print(f"\nrecordings kept in {work}")
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)
    print("\nsound check passed")


if __name__ == "__main__":
    main()
