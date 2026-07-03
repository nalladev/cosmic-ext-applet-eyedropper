# Colour Picker

A colour picker applet for the COSMIC™ desktop. Click the eyedropper icon in the panel, select a colour from anywhere on screen, and copy it as hex, RGB, or HSL.

## Screenshot

![Screenshot](https://github.com/nalladev/cosmic-applet-color-picker/raw/main/resources/screenshot.png)

*Coming soon — a proper screenshot of the applet in action.*

## Installation

### From source

```sh
git clone https://github.com/nalladev/cosmic-applet-color-picker
cd cosmic-applet-color-picker
cargo build --release
sudo just install
pkill cosmic-panel
```

Then right-click the panel → **Add Applet** → find **Colour Picker**.

### Dependencies

- Rust (edition 2024, MSRV 1.85+)
- [just](https://github.com/casey/just) (`sudo apt install just`)

## Development

```sh
# Build and run standalone (for testing capture/picker flow)
just run

# Build release
just

# Install locally
sudo just install

# Restart the panel to pick up changes
pkill cosmic-panel

# Check for warnings
just check
```

The applet can be run standalone with `just run` for quick testing. When run from the panel, install it first with `sudo just install` then restart the panel.

## License

MPL-2.0
