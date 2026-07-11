#!/usr/bin/env python3
"""Wrapper to fix flatpak-cargo-generator with Cargo.lock v4 format."""
import sys
import json
import asyncio
import os
import tomllib

# Monkey-patch toml with tomllib before importing flatpak_cargo_generator
import types
toml_module = types.ModuleType('toml')
toml_module.load = lambda f: tomllib.load(f)
toml_module.dumps = lambda d: _toml_dumps(d)
sys.modules['toml'] = toml_module

def _toml_dumps(data):
    """Simple TOML dumps for the config output."""
    lines = []
    for key, val in data.items():
        if key == 'source' and isinstance(val, dict):
            for src_key, src_val in val.items():
                if isinstance(src_val, dict) and 'directory' in src_val:
                    lines.append(f'[source.{src_key!r}]')
                    lines.append(f'directory = {src_val["directory"]!r}')
                elif isinstance(src_val, dict) and 'replace-with' in src_val:
                    lines.append(f'[source.{src_key!r}]')
                    lines.append(f'replace-with = {src_val["replace-with"]!r}')
        else:
            lines.append(f'{key} = {json.dumps(val)}')
    return '\n'.join(lines) + '\n'

sys.path.insert(0, '/home/joel/.local/share/pipx/venvs/flatpak-cargo-generator/lib/python3.12/site-packages')

# Suppress async HTTP fetching - we don't need it for offline builds
import logging
logging.basicConfig(level=logging.INFO)

from flatpak_cargo_generator.script import generate_sources

async def main():
    with open('Cargo.lock', 'rb') as f:
        cargo_lock = tomllib.load(f)
    
    result = await generate_sources(cargo_lock, git_tarballs=True)
    
    with open('flatpak/cargo-sources.json', 'w') as f:
        json.dump(result, f, indent=4)
    print(f'Written {len(result)} sources')

asyncio.run(main())
