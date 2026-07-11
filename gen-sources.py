#!/usr/bin/env python3
"""Simple generator for flatpak cargo-sources.json from Cargo.lock."""
import sys
import json
import tomllib
from urllib.parse import urlparse, parse_qs

def parse_git_source(source):
    """Parse a git source string from Cargo.lock into (url, commit).
    
    Examples:
    - git+https://github.com/user/repo#abc123
    - git+https://github.com/user/repo.git?tag=v1.0#abc123
    - git+https://github.com/user/repo#branch=main
    """
    # Remove 'git+' prefix
    if not source.startswith('git+'):
        raise ValueError(f"Not a git source: {source}")
    git_url = source[4:]
    
    # Split into URL and fragment
    if '#' in git_url:
        url_part, _, fragment = git_url.partition('#')
    else:
        url_part, fragment = git_url, ''
    
    # Clean up URL
    url = url_part.rstrip('/')
    if url.endswith('.git'):
        url = url[:-4]
    
    # Parse query parameters from URL (if any)
    query_params = {}
    if '?' in url:
        base_url, _, query_string = url.partition('?')
        if query_string:
            query_params = parse_qs(query_string)
        url = base_url
    
    # Determine commit/revision
    commit = None
    
    # Check fragment for commit hash (most common in lockfiles)
    if fragment:
        # If fragment contains '=', it might be a branch/tag specification
        if '=' not in fragment and len(fragment) >= 7 and all(c in '0123456789abcdefABCDEF' for c in fragment):
            # Looks like a commit hash
            commit = fragment
        else:
            # Might be branch=, tag=, etc.
            if '=' in fragment:
                key, _, value = faction.partition('=')
                if key in ('rev', 'commit'):
                    commit = value
                elif key == 'tag':
                    # In an ideal world we'd resolve the tag, but in lockfile
                    # the fragment should already be the resolved commit
                    # So we'll use the fragment as-is for now
                    commit = fragment
                elif key == 'branch':
                    commit = fragment
            # If we couldn't parse it as key=value, use the whole fragment
            if not commit:
                commit = fragment
    
    # Check query parameters for explicit rev/tag/branch
    if not commit:
        if 'rev' in query_params:
            commit = query_params['rev'][0]
        elif 'tag' in query_params:
            # Similar to above - in lockfile, should already be resolved
            commit = query_params['tag'][0]
        elif 'branch' in query_params:
            commit = query_params['branch'][0]
    
    # Final fallback
    if not commit:
        commit = 'HEAD'
    
    return url, commit

def generate_sources(cargo_lock):
    """Generate flatpak sources from cargo lock data."""
    sources = []
    
    for pkg in cargo_lock['package']:
        name = pkg['name']
        version = pkg['version']
        source = pkg.get('source', '')
        
        if source.startswith('git+'):
            # Git repo dependency
            try:
                url, commit = parse_git_source(source)
            except Exception as e:
                print(f"WARNING: Failed to parse git source {source}: {e}", file=sys.stderr)
                continue
            
            # Generate directory name: repo-name-commithash
            repo_name = url.split('/')[-1]
            if repo_name.endswith('.git'):
                repo_name = repo_name[:-4]
            dir_name = f"{repo_name}-{commit[:7]}"
            
            sources.append({
                'type': 'git',
                'url': url,
                'commit': commit,
                'dest': f"flatpak-cargo/git/{dir_name}"
            })
        else:
            # crates.io or other registry
            checksum = pkg.get('checksum', '')
            if not checksum:
                # Skip packages without checksum (like local path deps)
                continue
            sources.append({
                'type': 'file',
                'url': f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                'sha256': checksum,
                'dest': f"cargo/vendor/{name}-{version}"
            })
            # Add the .cargo-checksum.json file
            sources.append({
                'type': 'file',
                'url': f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                'sha256': checksum,
                'dest': f"cargo/vendor/{name}-{version}",
                'dest-filename': ".cargo-checksum.json",
                'contents': json.dumps({"package": checksum, "files": {}})
            })
    
    # Add vendored sources config
    sources.append({
        'type': 'inline',
        'dest': 'cargo',
        'dest-filename': 'config',
        'contents': '''[source.vendored-sources]
directory = "cargo/vendor"

[source.crates-io]
replace-with = "vendored-sources"
'''
    })
    
    return sources

if __name__ == '__main__':
    with open('Cargo.lock', 'rb') as f:
        cargo_lock = tomllib.load(f)
    
    sources = generate_sources(cargo_lock)
    
    with open('flatpak/cargo-sources.json', 'w') as f:
        json.dump(sources, f, indent=4)
    
    print(f"Generated {len(sources)} source entries")