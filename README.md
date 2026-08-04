# Eyedropper

An eyedropper applet for the [COSMIC](https://system76.com/cosmic) desktop. Pick any colour from your screen and copy it as hex, RGB, or HSL.

![Screenshot](/resources/screenshot-1.png)

![Screenshot](/resources/screenshot-2.png)

## Features

- **Freeze mode** — click the applet, then click anywhere on screen to pick a colour
- **Magnifier preview** — a zoomed-in lens follows your cursor so you can see exactly which pixel you're selecting; scroll to zoom from 8× to 24×, rendered on the GPU with crisp nearest-neighbour pixels
- **Multiple formats** — copy the picked colour as hex (`#ff0000`), RGB (`rgb(255, 0, 0)`), or HSL (`hsl(0, 100%, 50%)`)
- **Auto-copy on select** — optional setting that copies the picked colour to the clipboard the moment you click, using a configurable default format (hex, RGB, or HSL)
- **Popup with colour history** — the panel popup shows your last selection with quick-copy buttons
- **Keyboard-shortcut friendly** — `cosmic-ext-applet-eyedropper --pick` starts colour-picker mode immediately; if the applet is already running, the request is forwarded to it over D-Bus, so you can bind the command to a shortcut and pick a colour in one step

## Installation

### From the COSMIC store

The applet is published in the COSMIC flatpak repository. Install it from the COSMIC Store, or:

```sh
flatpak remote-add --if-not-exists --user cosmic https://apt.pop-os.org/cosmic/cosmic.flatpakrepo
flatpak install --user cosmic io.github.nalladev.CosmicExtAppletEyedropper
```

### From a release

Download the `.deb`, `.rpm`, or tarball for your architecture from the [releases page](https://github.com/nalladev/cosmic-ext-applet-eyedropper/releases/latest).

```sh
# Debian/Ubuntu/Pop!_OS
sudo apt install ./cosmic-ext-applet-eyedropper_*.deb

# Fedora
sudo dnf install ./cosmic-ext-applet-eyedropper-*.rpm

# Tarball (installs to ~/.local, no root required)
tar -xzf cosmic-ext-applet-eyedropper-*.tar.gz
cd cosmic-ext-applet-eyedropper
./install.sh
```

Then restart the panel and add the applet:

```sh
pkill cosmic-panel
```

Open **Settings → Desktop → Panel → Applets** and enable **Eyedropper**.

### Flatpak

Requires `flatpak-builder` and the Freedesktop SDK:

```sh
# Install the Freedesktop SDK extension (if not already installed)
flatpak install --user org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08

# Build and install the Flatpak (from the working tree)
just flatpak-install
```

This auto-regenerates the Flatpak cargo sources when `Cargo.lock` changes.

### From source

```sh
git clone https://github.com/nalladev/cosmic-ext-applet-eyedropper
cd cosmic-ext-applet-eyedropper
just build-release
sudo just install
```

Then restart the panel (`pkill cosmic-panel`) and add the applet from Settings.

## Development

```sh
just build-release     # Release build
just build-debug       # Debug build
just run               # Run standalone for testing
sudo just install      # Install system-wide
just check             # Type-check (cargo check)
just lint              # Run clippy lints
RUST_LOG=debug just run  # Run with verbose debug logging
```

## Contributing

Contributions are welcome. Feel free to open issues or submit pull requests.

## License

[MPL-2.0](LICENSE)
