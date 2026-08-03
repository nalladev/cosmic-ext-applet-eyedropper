#!/usr/bin/env bash
set -euo pipefail

# System dependencies required to build cosmic-ext-applet-eyedropper.
# These are the -dev packages needed by the Rust crate's C dependencies
# (libcosmic, pipewire, smithay-client-toolkit, etc.)

sudo apt-get update
sudo apt-get install -y \
    libxkbcommon-dev \
    libwayland-dev \
    libegl1-mesa-dev \
    libgles2-mesa-dev \
    libdbus-1-dev \
    libpipewire-0.3-dev \
    libpulse-dev \
    libfontconfig-dev \
    libfreetype6-dev \
    libinput-dev \
    libudev-dev
