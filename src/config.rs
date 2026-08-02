// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

/// Colour representation used for automatic copies (`copy_on_select`).
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorFormat {
    #[default]
    Hex,
    Rgb,
    Hsl,
}

#[derive(Debug, Default, Clone, CosmicConfigEntry, Eq, PartialEq, Serialize, Deserialize)]
#[version = 1]
pub struct Config {
    #[serde(default)]
    pub restore_token: Option<String>,

    /// Automatically copy the picked colour to the clipboard.
    #[serde(default)]
    pub copy_on_select: bool,

    /// Colour format used for automatic copies.
    #[serde(default)]
    pub default_color_format: ColorFormat,
}
