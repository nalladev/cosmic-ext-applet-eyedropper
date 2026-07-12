name := 'cosmic-ext-applet-eyedropper'
appid := 'io.github.nalladev.CosmicExtAppletEyedropper'

rootdir := ''
prefix := '/usr'

# Installation paths
base-dir := absolute_path(clean(rootdir / prefix))
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
appdata-dst := base-dir / 'share' / 'appdata' / appid + '.metainfo.xml'
bin-dst := base-dir / 'bin' / name
desktop-dst := base-dir / 'share' / 'applications' / appid + '.desktop'
icon-dst := base-dir / 'share' / 'icons' / 'hicolor' / 'scalable' / 'apps' / appid + '.svg'

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

# Runs clippy lints
lint *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

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

# Vendor dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor --sync Cargo.toml | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    echo >> .cargo/config.toml
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
        python3 flatpak-builder-tools/cargo/flatpak-cargo-generator.py -o "$OUT" Cargo.lock
    else
        echo "$OUT is up to date"
    fi

# Build flatpak (auto-regenerates cargo sources if needed)
flatpak-build: vendor-flatpak
    flatpak-builder --force-clean --user --repo=repo builddir \
        flatpak/io.github.nalladev.CosmicExtAppletEyedropper.flatpak.json

# Install (or replace) the flatpak from local repo
flatpak-install:
    flatpak update --user io.github.nalladev.CosmicExtAppletEyedropper

# Bump cargo version, create git commit, and create tag
tag version:
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "{{version}}"/' '{}' \; -exec git add '{}' \;
    cargo check
    cargo clean
    git add Cargo.lock
    git commit -m 'release: {{version}}'
    git tag -a {{version}} -m ''

