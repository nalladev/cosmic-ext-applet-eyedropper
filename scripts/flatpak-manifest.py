#!/usr/bin/env python3
"""Switch the Flatpak manifest between release and local-build form.

The committed manifest always points at the latest release tag
(``"type": "git"`` + ``"tag": "vX.Y.Z"``). Local builds need it to point at
the working tree instead, so ``just flatpak-install`` writes a throwaway copy
with ``to-dir`` and builds from that, leaving the committed manifest
untouched.

``just release`` uses ``to-git`` to bump the tag in place before tagging, so
the change lands in the release commit.

The copy written by ``to-dir`` must live in the ``flatpak/`` directory: the
manifest references ``cargo-sources.json`` and the dir source path relative
to its own location.

Usage:
  flatpak-manifest.py to-dir OUTPUT
  flatpak-manifest.py to-git VERSION REPO_URL [MANIFEST]
"""

import argparse
import json

MANIFEST = "flatpak/io.github.nalladev.CosmicExtAppletEyedropper.json"
DIR_SOURCE = {"type": "dir", "path": ".."}


def load(manifest: str) -> dict:
    with open(manifest, encoding="utf-8") as f:
        return json.load(f)


def write(manifest: str, data: dict) -> None:
    with open(manifest, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    to_dir = sub.add_parser("to-dir", help="write a local-build copy with a dir source")
    to_dir.add_argument("output", help="path for the local-build manifest copy")

    to_git = sub.add_parser("to-git", help="point the manifest at a release tag (in place)")
    to_git.add_argument("version")
    to_git.add_argument("repo_url")
    to_git.add_argument("manifest", nargs="?", default=MANIFEST)

    args = parser.parse_args()

    if args.cmd == "to-dir":
        data = load(MANIFEST)
        data["modules"][0]["sources"][0] = DIR_SOURCE
        write(args.output, data)
        print(f"{args.output}: source -> dir")
    else:
        version = args.version.lstrip("v")
        data = load(args.manifest)
        data["modules"][0]["sources"][0] = {
            "type": "git",
            "url": args.repo_url,
            "tag": "v" + version,
        }
        write(args.manifest, data)
        print(f"{args.manifest}: source -> git tag v{version}")


if __name__ == "__main__":
    main()
