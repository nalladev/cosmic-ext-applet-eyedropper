name := 'cosmic-ext-applet-eyedropper'
appid := 'io.github.nalladev.CosmicExtAppletEyedropper'
repo-url := 'https://github.com/nalladev/cosmic-ext-applet-eyedropper.git'

rootdir := ''
prefix := '/usr'

# Installation paths
base-dir := absolute_path(clean(rootdir / prefix))
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
appdata-dst := base-dir / 'share' / 'appdata' / appid + '.metainfo.xml'
bin-dst := base-dir / 'bin' / name
desktop-dst := base-dir / 'share' / 'applications' / appid + '.desktop'
icon-dst := base-dir / 'share' / 'icons' / 'hicolor' / 'scalable' / 'apps' / appid + '-symbolic.svg'


# Default recipe which runs `just build-release`
default: build-release

# Runs `cargo clean`
clean:
    cargo clean

# Removes vendored dependencies
clean-vendor:
    rm -rf .cargo vendor vendor.tar

# `cargo clean` and removes vendored dependencies
clean-dist: clean clean-vendor

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compiles release profile with vendored dependencies
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Formats the codebase
fmt *args:
    cargo fmt {{args}}

# Runs a cargo type check
check *args:
    cargo check {{args}}

# Runs clippy lints (same flags as CI)
lint *args:
    cargo clippy --all --all-targets --all-features {{args}} -- -D warnings -W clippy::pedantic

# Runs clippy lints with JSON message format
lint-json: (lint '--message-format=json')

# Run the application for testing purposes
run *args:
    env RUST_BACKTRACE=full cargo run --release {{args}}

# Installs files
install:
    install -Dm0755 {{ cargo-target-dir / 'release' / name }} {{bin-dst}}
    install -Dm0644 resources/app.desktop {{desktop-dst}}
    install -Dm0644 resources/app.metainfo.xml {{appdata-dst}}
    install -Dm0644 resources/icon.svg {{icon-dst}}

# Uninstalls installed files
uninstall:
    rm {{bin-dst}} {{desktop-dst}} {{icon-dst}}

# Compiles and packages a .deb with the release profile
build-deb: build-release
    command -v cargo-deb || cargo install cargo-deb
    cargo deb

# Installs the locally-built .deb
install-deb:
    apt install --reinstall ./target/debian/*.deb

# Compiles and packages an .rpm with the release profile
build-rpm: build-release
    command -v cargo-generate-rpm || cargo install cargo-generate-rpm
    strip -s {{ cargo-target-dir / 'release' / name }}
    cargo generate-rpm

# Installs the locally-built .rpm
install-rpm:
    dnf install ./target/generate-rpm/*.rpm

# Vendor dependencies locally into vendor.tar
vendor:
    mkdir -p .cargo
    cargo vendor --sync Cargo.toml | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    echo >> .cargo/config.toml
    tar pcf vendor.tar .cargo vendor
    rm -rf .cargo vendor

# Extracts vendored dependencies
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

# Regenerate flatpak cargo sources only if Cargo.lock changed
vendor-flatpak:
    #!/usr/bin/env bash
    set -euo pipefail
    OUT="flatpak/cargo-sources.json"
    if [ ! -f "$OUT" ] || [ Cargo.lock -nt "$OUT" ]; then
        echo "Regenerating $OUT ..."
        python3 flatpak/flatpak-cargo-generator.py -o "$OUT" Cargo.lock
    else
        echo "$OUT is up to date"
    fi

# Build and install flatpak
flatpak-install: vendor-flatpak
    flatpak-builder --user --install --force-clean build-dir \
        flatpak/io.github.nalladev.CosmicExtAppletEyedropper.json

# Replace the local test build with the production (Flathub) copy.
# No-op when the production copy is already installed.
flatpak-restore:
    #!/usr/bin/env bash
    set -euo pipefail
    APP="io.github.nalladev.CosmicExtAppletEyedropper"
    if flatpak info --show-origin "$APP" 2>/dev/null | grep -qx "flathub"; then
        echo "production copy already installed — nothing to do"
        exit 0
    fi
    flatpak uninstall --user -y "$APP" || true
    flatpak install --user -y flathub "$APP"
    echo "restored $APP from flathub"

# Bump cargo version, add the AppStream release entry, commit, and tag
# Usage: just tag 1.2.0 "Release notes here" or just tag v1.2.0 "Release notes here"
tag version message='':
    # Normalize version: strip leading 'v' if present
    norm_version=`bash -c 'v="{{version}}"; echo "${v#v}"'` && \
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "'"$norm_version"'"/' '{}' \; -exec git add '{}' \; && \
    cargo check && \
    python3 scripts/update-metainfo-release.py "$norm_version" "{{message}}" "{{repo-url}}" && \
    git add resources/app.metainfo.xml && \
    git add Cargo.lock && \
    git commit -m 'release: '"$norm_version" && \
    bash -c 'if [ -n "{{message}}" ]; then git tag -a v'"$norm_version"' -m "{{message}}"; else git tag -a v'"$norm_version"' -m "Release '"$norm_version"'"; fi'

# Regenerate cargo sources and sync the manifest + sources into a checkout of
# pop-os/cosmic-flatpak (the canonical Flatpak home for COSMIC applets).
# Usage: just sync-flatpak /path/to/cosmic-flatpak
sync-flatpak dir: vendor-flatpak
    #!/usr/bin/env bash
    set -euo pipefail
    DEST="{{dir}}/app/{{appid}}"
    if [ ! -d "$DEST" ]; then
        echo "error: $DEST not found — clone pop-os/cosmic-flatpak first"
        exit 1
    fi
    VERSION="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
    python3 scripts/sync-cosmic-flatpak.py "$DEST" "{{appid}}" "{{repo-url}}" "v$VERSION"
    cp flatpak/cargo-sources.json "$DEST/cargo-sources.json"
    echo "Synced to $DEST with tag v$VERSION."
    echo "Next: cd into the cosmic-flatpak checkout, review with git diff, commit, and open a PR."