# Eyedropper

An eyedropper applet for the [COSMIC](https://system76.com/cosmic) desktop. Pick any colour from your screen and copy it as hex, RGB, or HSL.

![Magnifier in the colour-picking state](/resources/screenshot-1.png)

![The applet menu opened from the panel](/resources/screenshot-2.png)

## Features

- **Freeze mode** — click the applet, then click anywhere on screen to pick a colour
- **Magnifier preview** — a zoomed-in lens follows your cursor so you can see exactly which pixel you're selecting; scroll to zoom from 8× to 24×, rendered on the GPU with crisp nearest-neighbour pixels
- **Multiple formats** — copy the picked colour as hex (`#ff0000`), RGB (`rgb(255, 0, 0)`), or HSL (`hsl(0, 100%, 50%)`)
- **Auto-copy on select** — optional setting that copies the picked colour to the clipboard the moment you click, using a configurable default format (hex, RGB, or HSL)
- **Popup with colour history** — the panel popup shows your last selection with quick-copy buttons
- **Natural wheel zoom** — wheel forward zooms in, wheel backward zooms out
- **Keyboard-shortcut friendly** — `cosmic-ext-applet-eyedropper --pick` starts colour-picker mode immediately; if the applet is already running, the request is forwarded to it over D-Bus, so you can bind the command to a shortcut and pick a colour in one step

## Installing

Download the `.deb`, `.rpm`, or tarball from the [releases page](https://github.com/nalladev/cosmic-ext-applet-eyedropper/releases/latest), or install from the COSMIC Store.

Then restart the panel and add the applet:

```sh
pkill cosmic-panel
```

Open **Settings → Desktop → Panel → Applets** and enable **Eyedropper**.

## Building from source

Clone the repository and install with [just](https://github.com/casey/just):

```sh
git clone https://github.com/nalladev/cosmic-ext-applet-eyedropper
cd cosmic-ext-applet-eyedropper
just build-release
sudo just install
```

Then restart the panel and add the applet as above.

## Development

```sh
just build-release       # Release build
just build-debug         # Debug build
just run                 # Run standalone for testing
sudo just install        # Install system-wide
just check               # Type-check (cargo check)
just lint                # Run clippy lints
RUST_LOG=debug just run  # Run with verbose debug logging
just flatpak-install     # Build and install the Flatpak from the working tree
```

## Contributing

Contributions are welcome. Feel free to open issues or submit pull requests.

## License

[MPL-2.0](LICENSE)
