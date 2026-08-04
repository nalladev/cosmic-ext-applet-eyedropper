#!/usr/bin/env python3
"""Switch the Flatpak manifest between release and local-build form.

The committed manifest always points at the latest release tag
(``"type": "git"`` + ``"tag": "vX.Y.Z"``). Local builds need it to point at
the working tree instead, so ``just flatpak-install`` converts it with
``to-dir`` before building and back with ``to-git`` afterwards.

``just release`` uses ``to-git`` to bump the tag before tagging, so the
change lands in the release commit.

Usage:
  flatpak-manifest.py to-dir [MANIFEST]
  flatpak-manifest.py to-git VERSION REPO_URL [MANIFEST]
"""

import json
import sys

MANIFEST = "flatpak/io.github.nalladev.CosmicExtAppletEyedropper.json"
DIR_SOURCE = {"type": "dir", "path": ".."}


def main() -> None:
    args = sys.argv[1:]
    if not args:
        sys.exit("usage: flatpak-manifest.py to-dir|to-git VERSION REPO_URL [MANIFEST]")

    cmd = args[0]
    if cmd == "to-dir":
        if len(args) > 2:
            sys.exit("usage: flatpak-manifest.py to-dir [MANIFEST]")
        manifest = args[1] if len(args) == 2 else MANIFEST
        source = DIR_SOURCE
        label = "dir"
    elif cmd == "to-git":
        if len(args) not in (3, 4):
            sys.exit("usage: flatpak-manifest.py to-git VERSION REPO_URL [MANIFEST]")
        version, repo_url = args[1].lstrip("v"), args[2]
        manifest = args[3] if len(args) == 4 else MANIFEST
        source = {"type": "git", "url": repo_url, "tag": "v" + version}
        label = f"git tag v{version}"
    else:
        sys.exit(f"unknown command: {cmd}")

    with open(manifest, encoding="utf-8") as f:
        data = json.load(f)
    data["modules"][0]["sources"][0] = source
    with open(manifest, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    print(f"{manifest}: source -> {label}")


if __name__ == "__main__":
    main()
