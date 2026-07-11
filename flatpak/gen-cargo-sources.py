#!/usr/bin/env python3
"""
Generate cargo-sources.json for flatpak using cargo metadata.
This replaces flatpak-cargo-generator which has issues with newer Cargo.lock format.
"""
import json
import subprocess
import sys
import os
import hashlib
import tomllib
from urllib.parse import urlparse, parse_qs

def get_cargo_metadata():
    """Get cargo metadata as JSON"""
    result = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "--no-deps"],
        capture_output=True, text=True, check=True
    )
    return json.loads(result.stdout)

def parse_git_source(source_str):
    """Parse git source string from Cargo.lock"""
    # Format: git+https://github.com/owner/repo?tag=v1.0#commit
    # or: git+https://github.com/owner/repo.git?rev=abc123#abc123
    if not source_str.startswith("git+"):
        return None
    
    source_str = source_str[4:]  # Remove "git+"
    
    # Split URL from commit hash
    if "#" in source_str:
        url_part, commit = source_str.rsplit("#", 1)
    else:
        url_part, commit = source_str, None
    
    # Parse URL
    parsed = urlparse(url_part)
    path = parsed.path.rstrip("/")
    if path.endswith(".git"):
        path = path[:-4]
    
    repo_url = f"{parsed.scheme}://{parsed.netloc}{path}"
    
    # Parse query parameters
    query = parse_qs(parsed.query)
    tag = query.get("tag", [None])[0]
    rev = query.get("rev", [None])[0]
    branch = query.get("branch", [None])[0]
    
    return {
        "repo_url": repo_url,
        "commit": commit,
        "tag": tag,
        "rev": rev,
        "branch": branch,
    }

def main():
    cargo_lock_path = "Cargo.lock"
    output_path = "flatpak/cargo-sources.json"
    
    # Load Cargo.lock
    with open(cargo_lock_path, "rb") as f:
        cargo_lock = tomllib.load(f)
    
    # Collect all unique sources from packages
    sources = {}  # key -> source info
    
    for pkg in cargo_lock.get("package", []):
        source = pkg.get("source", "")
        if not source:
            continue
        
        if source.startswith("git+"):
            info = parse_git_source(source)
            if info:
                key = f"git+{info['repo_url']}"
                if info.get("commit"):
                    key += f"#{info['commit']}"
                if info.get("tag"):
                    key += f"?tag={info['tag']}"
                elif info.get("rev"):
                    key += f"?rev={info['rev']}"
                elif info.get("branch"):
                    key += f"?branch={info['branch']}"
                
                if key not in sources:
                    sources[key] = info
        elif source == "registry+https://github.com/rust-lang/crates.io-index":
            # crates.io packages will be handled by flatpak-cargo-generator
            pass
    
    print(f"Found {len(sources)} unique git sources")
    
    # Generate sources list for flatpak
    flatpak_sources = []
    
    for key, info in sources.items():
        # Create a dest path based on repo name
        repo_name = info["repo_url"].split("/")[-1]
        if repo_name.endswith(".git"):
            repo_name = repo_name[:-4]
        
        # Use first 8 chars of commit for dest
        commit_short = info["commit"][:8] if info["commit"] else "unknown"
        dest = f"flatpak-cargo/git/{repo_name}-{commit_short}"
        
        source_entry = {
            "type": "git",
            "url": info["repo_url"],
            "commit": info["commit"],
            "dest": dest
        }
        
        if info.get("tag"):
            source_entry["tag"] = info["tag"]
        elif info.get("rev"):
            source_entry["rev"] = info["rev"]
        elif info.get("branch"):
            source_entry["branch"] = info["branch"]
        
        flatpak_sources.append(source_entry)
        print(f"  {info['repo_url']} @ {info['commit'][:12]} -> {dest}")
    
    # Now we need to also include the crates.io sources
    # Run flatpak-cargo-generator for those, or we can generate them
    # For now, let's run flatpak-cargo-generator to get the crates.io sources
    # and merge
    
    # Use cargo vendor to get crates.io sources with checksums
    print("Running cargo vendor to get crates.io sources...")
    result = subprocess.run(
        ["cargo", "vendor", "--versioned-dirs", "--manifest-path", "Cargo.toml"],
        capture_output=True, text=True, cwd="."
    )
    if result.returncode != 0:
        print(f"cargo vendor failed: {result.stderr}")
        # Try without versioned dirs
        result = subprocess.run(
            ["cargo", "vendor", "--manifest-path", "Cargo.toml"],
            capture_output=True, text=True, cwd="."
        )
        if result.returncode != 0:
            print(f"cargo vendor failed: {result.stderr}")
    
    # Read the vendor directory structure
    vendor_dir = "vendor"
    if os.path.exists(vendor_dir):
        for crate_dir in os.listdir(vendor_dir):
            crate_path = os.path.join(vendor_dir, crate_dir)
            if not os.path.isdir(crate_path):
                continue
            
            # Find .cargo-checksum.json
            checksum_file = os.path.join(crate_path, ".cargo-checksum.json")
            if os.path.exists(checksum_file):
                with open(checksum_file) as f:
                    checksums = json.load(f)
                
                package_checksum = checksums.get("package", "")
                if package_checksum:
                    # Find the .crate file
                    crate_files = [f for f in os.listdir(".") if f.startswith(crate_dir) and f.endswith(".crate")]
                    if not crate_files:
                        # Download URL
                        url = f"https://static.crates.io/crates/{crate_dir.split('-')[0]}/{crate_dir}.crate"
                    else:
                        url = f"https://static.crates.io/crates/{crate_dir.split('-')[0]}/{crate_files[0]}"
                    
                    flatpak_sources.append({
                        "type": "file",
                        "url": url,
                        "sha256": package_checksum,
                        "dest": f"cargo/vendor/{crate_dir}"
                    })
                    
                    # Add checksum file as inline
                    flatpak_sources.append({
                        "type": "inline",
                        "contents": json.dumps(checksums),
                        "dest": f"cargo/vendor/{crate_dir}",
                        "dest-filename": ".cargo-checksum.json"
                    })
    
    # Write output
    with open(output_path, "w") as f:
        json.dump(flatpak_sources, f, indent=4)
    
    print(f"Generated {output_path} with {len(flatpak_sources)} entries")

if __name__ == "__main__":
    main()