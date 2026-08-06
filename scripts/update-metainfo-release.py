#!/usr/bin/env python3
"""Insert a <release> entry into resources/app.metainfo.xml.

Invoked by ``just release`` so the AppStream release notes shown in app centres
(and required by Flathub) stay in sync with the git tag. Idempotent: if the
version is already present the file is left unchanged.

Usage: update-metainfo-release.py VERSION MESSAGE REPO_URL [METAINFO]

The MESSAGE becomes the changelog: pass one bullet per line to get a <ul>
list (app centres and the COSMIC store render these as bullets), or a single
line for a one-line entry.
"""

import datetime
import re
import sys
import xml.sax.saxutils as sax

METAINFO = "resources/app.metainfo.xml"


def clean_bullet(line: str) -> str:
    """Strip a leading bullet marker the user may have typed ('- ', '* ', '• ')."""
    text = line.strip()
    for marker in ("- ", "* ", "• "):
        if text.startswith(marker):
            text = text[len(marker) :].strip()
            break
    if text in ("-", "*", "•"):
        return ""
    return text


def version_key(version: str) -> list:
    """Numeric-aware sort key for plain x.y.z AppStream version strings."""
    key = []
    for part in re.split(r"([0-9]+|[^0-9]+)", version):
        if not part:
            continue
        if part.isdigit():
            key.append((0, int(part)))
        else:
            key.append((1, part.lower()))
    return key


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

    # Multi-line messages become a <ul> of <li> bullets (app centres and the
    # COSMIC store render these as a proper list); a single-line message stays
    # a plain <p>. Both are valid AppStream rich text.
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    repo = repo_url.removesuffix(".git")

    lines = []
    for line in message.splitlines():
        if not line.strip():
            continue
        bullet = clean_bullet(line)
        if bullet:
            lines.append(bullet)
    if not lines:
        lines = ["Release " + version]
    if len(lines) == 1:
        description = "      <p>" + sax.escape(lines[0]) + "</p>\n"
    else:
        items = "".join(
            "        <li>" + sax.escape(line) + "</li>\n" for line in lines
        )
        description = "      <ul>\n" + items + "      </ul>\n"

    release = (
        '  <release version="' + version + '" date="' + date + '">\n'
        "    <description>\n"
        + description
        + "    </description>\n"
        "    <url type=\"details\">" + repo + "/releases/tag/v" + version + "</url>\n"
        "  </release>\n"
    )

    if "<releases>" in text:
        # Keep the list sorted newest-first even when versions are not
        # released in strictly increasing order (e.g. a backported patch
        # release): insert before the first existing release that is older.
        existing = re.findall(r'<release version="([^"]+)"', text)
        if not existing:
            sys.exit("error: <releases> section found but no <release> inside")
        new_key = version_key(version)
        insert_before = None
        for i, ver in enumerate(existing):
            if version_key(ver) < new_key:
                insert_before = i
                break
        if insert_before is None:
            # New release is the oldest: append after the last </release>.
            close_idx = text.rfind("</releases>")
            text = text[:close_idx] + release + text[close_idx:]
        else:
            idx = list(re.finditer(r"<release ", text))[insert_before].start()
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
