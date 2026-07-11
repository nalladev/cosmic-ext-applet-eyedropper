# Eyedropper

An eyedropper applet for the [COSMIC](https://system76.com/cosmic) desktop. Pick any colour from your screen and copy it as hex, RGB, or HSL.

![Screenshot](https://github.com/nalladev/cosmic-ext-applet-eyedropper/raw/main/resources/screenshot.png)

## Features

- **Freeze mode** — click the applet, then click anywhere on screen to pick a colour
- **Magnifier preview** — a zoomed-in view follows your cursor so you can see exactly which pixel you're selecting
- **Multiple formats** — copy the picked colour as hex (`#ff0000`), RGB (`rgb(255, 0, 0)`), or HSL (`hsl(0, 100%, 50%)`)
- **Popup with colour history** — the panel popup shows your last selection with quick-copy buttons

## Installation

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
just check             # Run clippy lints
```

## Contributing

Contributions are welcome. Feel free to open issues or submit pull requests.

## License

[MPL-2.0](LICENSE)
