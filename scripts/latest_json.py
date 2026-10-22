#!/usr/bin/env python3
"""Writes the Tauri updater manifest (latest.json) for a release.

    python scripts/latest_json.py v0.1.1 dist/ > dist/latest.json

Each platform points at its signed bundle in the release, with the bundle's
signature inlined. Fails if a platform's bundle or signature is missing, so a
release can never ship a manifest the updater would reject.
"""

import datetime
import glob
import json
import os
import sys

REPO = "GPTmadeit/hearth"

tag, dist = sys.argv[1], sys.argv[2]
version = tag.lstrip("v")

# updater platform key -> glob for the bundle it installs
PLATFORMS = {
    "windows-x86_64": f"Hearth_{version}_x64-setup.exe",
    "linux-x86_64": f"Hearth_{version}_amd64.AppImage",
}

platforms = {}
for key, pattern in PLATFORMS.items():
    matches = glob.glob(os.path.join(dist, pattern))
    if len(matches) != 1:
        sys.exit(f"{key}: expected exactly one {pattern} in {dist}, found {matches}")
    bundle = matches[0]
    sig = bundle + ".sig"
    if not os.path.exists(sig):
        sys.exit(f"{key}: missing signature {sig} (is TAURI_SIGNING_PRIVATE_KEY set?)")
    name = os.path.basename(bundle)
    platforms[key] = {
        "signature": open(sig).read().strip(),
        "url": f"https://github.com/{REPO}/releases/download/{tag}/{name}",
    }

json.dump(
    {
        "version": version,
        "notes": f"https://github.com/{REPO}/releases/tag/{tag}",
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    },
    sys.stdout,
    indent=2,
)
print()
