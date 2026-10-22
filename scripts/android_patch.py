#!/usr/bin/env python3
"""Adds the permissions Hearth needs to the Android project Tauri generates.

    python scripts/android_patch.py crates/hearth-desktop/gen/android

The webview turns camera/microphone requests into Android permission prompts,
but only for permissions the manifest declares. Fails loudly if the manifest
doesn't look the way we expect, rather than shipping an APK that can't call.
"""

import os
import sys

manifest = os.path.join(sys.argv[1], "app", "src", "main", "AndroidManifest.xml")
xml = open(manifest, encoding="utf-8").read()

PERMISSIONS = [
    "android.permission.INTERNET",
    "android.permission.CAMERA",
    "android.permission.RECORD_AUDIO",
    "android.permission.MODIFY_AUDIO_SETTINGS",
    "android.permission.POST_NOTIFICATIONS",
    "android.permission.ACCESS_NETWORK_STATE",
]
# Not every phone has a camera; don't hide the app from those in stores.
FEATURES = ["android.hardware.camera", "android.hardware.microphone"]

lines = []
for p in PERMISSIONS:
    if f'android:name="{p}"' not in xml:
        lines.append(f'    <uses-permission android:name="{p}" />')
for f in FEATURES:
    if f'android:name="{f}"' not in xml:
        lines.append(f'    <uses-feature android:name="{f}" android:required="false" />')

anchor = "<application"
if anchor not in xml:
    sys.exit(f"{manifest}: no <application> element; Tauri's template changed")
xml = xml.replace(anchor, "\n".join(lines) + "\n    " + anchor, 1) if lines else xml
open(manifest, "w", encoding="utf-8").write(xml)
print(f"patched {manifest}: {len(lines)} entries added")
