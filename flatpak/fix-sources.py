#!/usr/bin/env python3
import json
import sys

def main():
    sources_path = sys.argv[1] if len(sys.argv) > 1 else "flatpak/cargo-sources.json"
    
    with open(sources_path) as f:
        sources = json.load(f)
    
    seen = set()
    new_sources = []
    
    for item in sources:
        # Create a key for deduplication
        if item.get('type') == 'git':
            key = ('git', item.get('url'), item.get('commit'), item.get('dest'))
        elif item.get('type') == 'file':
            key = (
                'file',
                item.get('url'),
                item.get('sha256'),
                item.get('dest'),
                item.get('dest-filename')
            )
        elif item.get('type') == 'inline':
            key = (
                'inline',
                item.get('dest'),
                item.get('dest-filename'),
                item.get('contents')
            )
        else:
            key = tuple(sorted(item.items()))
        
        if key in seen:
            continue
        seen.add(key)
        
        # Fix inline type: remove url and sha256 fields
        if item.get('type') == 'inline':
            item = item.copy()
            item.pop('url', None)
            item.pop('sha256', None)
        
        new_sources.append(item)
    
    with open(sources_path, 'w') as f:
        json.dump(new_sources, f, indent=4)
    
    print(f"Fixed {len(sources)} -> {len(new_sources)} items")

if __name__ == '__main__':
    main()