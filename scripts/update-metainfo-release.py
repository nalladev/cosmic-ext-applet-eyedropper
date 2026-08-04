#!/usr/bin/env python3
"""Insert a <release> entry into resources/app.metainfo.xml.

Invoked by ``just tag`` so the AppStream release notes shown in app centres
(and required by Flathub) stay in sync with the git tag. Idempotent: if the
version is already present the file is left unchanged.

Usage: update-metainfo-release.py VERSION MESSAGE REPO_URL [METAINFO]
"""

import datetime
import sys
import xml.sax.saxutils as sax

METAINFO = "resources/app.metainfo.xml"


def main() -> None:
    args = sys.argv[1:]
    if len(args) not in (3, 4):
        sys.exit("usage: update-metainfo-release.py VERSION MESSAGE REPO_URL [METAINFO]")

    version, message, repo_url = args[0], args[1], args[2]
    metainfo = args[3] if len(args) == 4 else METAINFO

    with open(metainfo, encoding="utf-8") as f:
        text = f.read()

    if f'version="{version}"' in text:
        print(f"release {version} already present — {metainfo} left unchanged")
        return

    # Collapse the message to a single line: predictable output for a
    # one-line changelog summary, and safe to embed in XML text.
    summary = " ".join(message.split()).strip() or f"Release {version}"
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    repo = repo_url.removesuffix(".git")

    release = (
        '  <release version="' + version + '" date="' + date + '">\n'
        '    <description>\n'
        '      <p>' + sax.escape(summary) + "</p>\n"
        "    </description>\n"
        "    <url type=\"details\">" + repo + "/releases/tag/v" + version + "</url>\n"
        "  </release>\n"
    )

    if "<releases>" in text:
        # Newest-first: insert before the first existing <release> line.
        idx = text.find("<release ")
        if idx == -1:
            sys.exit("error: <releases> section found but no <release> inside")
        line_start = text.rfind("\n", 0, idx) + 1
        text = text[:line_start] + release + text[line_start:]
    else:
        # No releases section yet: create one before the closing tag.
        if "</component>" not in text:
            sys.exit("error: </component> not found")
        block = "<releases>\n" + release + "</releases>\n"
        text = text.replace("</component>", block + "</component>", 1)

    with open(metainfo, "w", encoding="utf-8") as f:
        f.write(text)

    print(f"added release {version} ({date}) to {metainfo}")


if __name__ == "__main__":
    main()
