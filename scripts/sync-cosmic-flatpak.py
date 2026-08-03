#!/usr/bin/env python3
"""Sync the Flatpak manifest into a pop-os/cosmic-flatpak checkout.

The local manifest builds from a directory (type: dir) so it works for local
``just flatpak-build`` runs; the canonical cosmic-flatpak copy must build from
the released git tag instead. This rewrites the manifest source accordingly
and is invoked by ``just sync-flatpak``.

Usage: sync-cosmic-flatpak.py DEST APPID REPO_URL TAG
"""

import json
import sys


def main() -> None:
    dest, appid, repo_url, tag = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

    with open(f"flatpak/{appid}.json", encoding="utf-8") as f:
        manifest = json.load(f)

    manifest["modules"][0]["sources"][0] = {
        "type": "git",
        "url": repo_url,
        "tag": tag,
    }

    with open(f"{dest}/{appid}.json", "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()
