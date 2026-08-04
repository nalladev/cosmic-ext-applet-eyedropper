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

# Clean everything build-related: cargo artifacts, vendored deps, flatpak build
# dirs + cache, python bytecode. Downside: next build and flatpak-install start
# cold (recompile / re-download). Don't run while a flatpak build is in progress
# (it removes the temp manifest and build dir being used).
clean-dist: clean clean-vendor
    rm -rf build-dir .flatpak-builder flatpak-repo flatpak/local-build.json
    rm -rf scripts/__pycache__

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

# Regenerate flatpak cargo sources only if Cargo.lock changed.
# The generator is cloned into ${TMPDIR:-/tmp} once; /tmp self-cleans on reboot.
vendor-flatpak:
    #!/usr/bin/env bash
    set -euo pipefail
    OUT="flatpak/cargo-sources.json"
    if [ ! -f "$OUT" ] || [ Cargo.lock -nt "$OUT" ]; then
        echo "Regenerating $OUT ..."
        CACHE="${TMPDIR:-/tmp}/flatpak-builder-tools"
        if [ ! -d "$CACHE/.git" ]; then
            git clone --quiet --depth 1 https://github.com/flatpak/flatpak-builder-tools.git "$CACHE"
        fi
        python3 "$CACHE/cargo/flatpak-cargo-generator.py" -o "$OUT" Cargo.lock
    else
        echo "$OUT is up to date"
    fi

# Build and install the local test build from a throwaway copy of the
# manifest (dir source); the committed manifest keeps its release tag.
flatpak-install: vendor-flatpak
    #!/usr/bin/env bash
    set -euo pipefail
    LOCAL="flatpak/local-build.json"
    python3 scripts/flatpak-manifest.py to-dir "$LOCAL"
    trap 'rm -f "$LOCAL"' EXIT
    flatpak-builder --user --install --force-clean build-dir "$LOCAL"
    trap - EXIT
    rm -f "$LOCAL"
    echo "Installed local test build — add the applet to the panel to test"
    echo "Replaced any existing applet (store version or local build)"

# Remove the local test build.
flatpak-uninstall:
    flatpak uninstall --user io.github.nalladev.CosmicExtAppletEyedropper
    echo "To get the production copy back, reinstall from the COSMIC store."

# Bump the version in Cargo.toml, run cargo check, commit everything staged, and tag.
# Any changes you staged beforehand (e.g. the AppStream entry via `just release`)
# are included in the commit automatically.
# Usage: just tag 1.2.0 "Release notes here" or just tag v1.2.0 "Release notes here"
tag version message='':
    # Normalize version: strip leading 'v' if present
    norm_version=`bash -c 'v="{{version}}"; echo "${v#v}"'` && \
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "'"$norm_version"'"/' '{}' \; -exec git add '{}' \; && \
    cargo check && \
    git add Cargo.lock && \
    git commit -m 'release: '"$norm_version" && \
    bash -c 'if [ -n "{{message}}" ]; then git tag -a v'"$norm_version"' -m "{{message}}"; else git tag -a v'"$norm_version"' -m "Release '"$norm_version"'"; fi'

# Update the AppStream entry and the Flatpak manifest tag, then run `just tag`
# (the staged changes land in the release commit) and push the new tag, which
# triggers the GitHub release workflow.
# Usage: just release 1.2.0 "Release notes here"
release version message='':
    # Normalize version: strip leading 'v' if present
    norm_version=`bash -c 'v="{{version}}"; echo "${v#v}"'` && \
    python3 scripts/update-metainfo-release.py "$norm_version" "{{message}}" "{{repo-url}}" && \
    python3 scripts/flatpak-manifest.py to-git "$norm_version" "{{repo-url}}" && \
    git add resources/app.metainfo.xml flatpak/io.github.nalladev.CosmicExtAppletEyedropper.json && \
    just tag "{{version}}" "{{message}}" && \
    git push origin v"$norm_version"