#!/usr/bin/env python3
"""Fix the cargo config in cargo-sources.json to use correct source IDs.

flatpak-cargo-generator's canonical_url() strips .git suffixes and query
parameters, causing collisions when the same git repo appears at multiple
revisions. This script regenerates the config with the exact source IDs
from Cargo.lock.
"""
import json
import sys
import tomllib
from urllib.parse import urlparse, parse_qs

def main():
    cargo_lock_path = sys.argv[1] if len(sys.argv) > 1 else "Cargo.lock"
    sources_path = sys.argv[2] if len(sys.argv) > 2 else "flatpak/cargo-sources.json"

    with open(cargo_lock_path, 'rb') as f:
        cargo_lock = tomllib.load(f)

    # Collect all unique git source IDs from Cargo.lock
    git_sources = {}
    for pkg in cargo_lock['package']:
        source = pkg.get('source', '')
        if not source.startswith('git+'):
            continue

        # Cargo source ID: strip git+ prefix and #commit suffix
        source_id = source[4:]
        if '#' in source_id:
            source_id = source_id[:source_id.rindex('#')]

        # Canonical URL for the git URL (strip .git, keep query params for source ID)
        u = urlparse(source_id)
        path = u.path.rstrip('/')
        if path.endswith('.git'):
            path = path[:-4]
        repo_url = f"{u.scheme}://{u.netloc}{path}"

        if source_id not in git_sources:
            qs = parse_qs(u.query) if u.query else {}
            rev = qs.get('rev', [None])[0]
            tag = qs.get('tag', [None])[0]
            branch = qs.get('branch', [None])[0]

            git_sources[source_id] = {
                'repo_url': repo_url,
                'rev': rev,
                'tag': tag,
                'branch': branch,
            }

    # Build the TOML config string
    lines = []
    lines.append('[source.vendored-sources]')
    lines.append('directory = "cargo/vendor"')
    lines.append('')
    lines.append('[source.crates-io]')
    lines.append('replace-with = "vendored-sources"')

    for source_id in sorted(git_sources.keys()):
        info = git_sources[source_id]
        lines.append('')
        lines.append(f'[source."{source_id}"]')
        lines.append(f'git = "{info["repo_url"]}"')
        lines.append('replace-with = "vendored-sources"')
        if info['rev']:
            lines.append(f'rev = "{info["rev"]}"')
        elif info['tag']:
            lines.append(f'tag = "{info["tag"]}"')
        elif info['branch']:
            lines.append(f'branch = "{info["branch"]}"')

    config_str = '\n'.join(lines) + '\n'

    # Update cargo-sources.json
    with open(sources_path) as f:
        sources = json.load(f)

    new_inline = {
        'type': 'inline',
        'contents': config_str,
        'dest': 'cargo',
        'dest-filename': 'config'
    }

    replaced = False
    for i, item in enumerate(sources):
        if (item.get('type') == 'inline' and
            item.get('dest-filename') == 'config' and
            item.get('dest') == 'cargo' and
            'source.vendored-sources' in str(item.get('contents', ''))):
            sources[i] = new_inline
            replaced = True
            break

    if not replaced:
        sources.append(new_inline)

    with open(sources_path, 'w') as f:
        json.dump(sources, f, indent=4)

    print(f"Fixed cargo config: {len(git_sources)} git sources, {len(sources)} total entries")

if __name__ == '__main__':
    main()
