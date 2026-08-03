#!/usr/bin/env python3
"""Build `latest.json`, the manifest tauri-plugin-updater polls.

Reads the `.sig` files the build produced next to each updater artifact and
pairs them with the release download URLs. Run from the publish job, with the
artifacts already downloaded into `artifacts/`.

Deliberately strict: a missing signature or an unmapped platform is an error
rather than a quietly incomplete manifest, because the failure mode of a bad
manifest is silent — the app just never offers an update, and nobody notices for
weeks.
"""

import json
import os
import sys
from pathlib import Path

TAG = os.environ["TAG"]
REPO = os.environ["REPO"]
ARTIFACTS = Path("artifacts")

# Tauri asks for these target keys. The macOS build is universal, so one
# artifact serves both architectures.
PLATFORMS = {
    ".app.tar.gz": ["darwin-x86_64", "darwin-aarch64"],
    "-setup.exe": ["windows-x86_64"],
    ".AppImage": ["linux-x86_64"],
}


def main() -> int:
    signatures = {p.name[: -len(".sig")]: p.read_text().strip()
                  for p in ARTIFACTS.rglob("*.sig")}
    if not signatures:
        print("no .sig files found — was TAURI_SIGNING_PRIVATE_KEY set?", file=sys.stderr)
        return 1

    platforms = {}
    for name, signature in sorted(signatures.items()):
        targets = next((t for suffix, t in PLATFORMS.items() if name.endswith(suffix)), None)
        if targets is None:
            print(f"signed artifact with no platform mapping: {name}", file=sys.stderr)
            return 1
        url = f"https://github.com/{REPO}/releases/download/{TAG}/{name}"
        for target in targets:
            platforms[target] = {"signature": signature, "url": url}

    missing = {t for ts in PLATFORMS.values() for t in ts} - platforms.keys()
    if missing:
        print(f"no signed artifact for: {', '.join(sorted(missing))}", file=sys.stderr)
        return 1

    manifest = {
        "version": TAG.lstrip("v"),
        "notes": f"See https://github.com/{REPO}/releases/tag/{TAG}",
        "pub_date": os.environ.get("PUB_DATE") or __import__("datetime").datetime.now(
            __import__("datetime").timezone.utc
        ).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }
    Path("latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
