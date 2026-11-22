// SPDX-License-Identifier: MPL-2.0

mod activation;
mod app;
mod config;
mod i18n;
pub mod picker;
pub mod widget;

use std::collections::HashMap;

use app::Flags;
use cosmic::Application;


/// Print usage information for the command-line interface.
fn print_help() {
    println!(
        "Usage: cosmic-ext-applet-eyedropper [OPTIONS]\n\
         \n\
         COSMIC eyedropper applet — pick colours from the screen.\n\
         \n\
         Options:\n\
         \x20 --pick   Start colour-picker mode immediately. If the applet is\n\
         \x20           already running, the request is forwarded to it over\n\
         \x20           D-Bus instead of starting a second instance.\n\
         \x20 -h, --help   Show this help and exit."
    );
}

/// Ask a running instance to activate the given action over D-Bus.
///
/// Returns `true` when the request was delivered to another instance.
#[allow(clippy::significant_drop_in_scrutinee)]
fn try_activate_existing_instance(action: &str) -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        log::warn!("[activation] no session bus available — starting a new instance");
        return false;
    };

    let path: String = format!("/{}", app::AppModel::APP_ID.replace('.', "/"));

    let Ok(proxy) = zbus::blocking::Proxy::new(
        &conn,
        app::AppModel::APP_ID,
        path,
        "org.freedesktop.DbusActivation",
    ) else {
        log::warn!("[activation] could not build D-Bus proxy — starting a new instance");
        return false;
    };

    let mut platform_data: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    if let Ok(token) = std::env::var("XDG_ACTIVATION_TOKEN") {
        platform_data.insert("activation-token", token.into());
    }
    if let Ok(startup_id) = std::env::var("DESKTOP_STARTUP_ID") {
        platform_data.insert("desktop-startup-id", startup_id.into());
    }

    match proxy.call_method("ActivateAction", &(action, Vec::<&str>::new(), platform_data)) {
        Ok(_) => {
            log::info!("[activation] forwarded {action:?} to the running instance");
            true
        }
        Err(err) => {
            log::info!("[activation] no running instance to activate ({err}) — starting a new one");
            false
        }
    }
}

fn main() -> cosmic::iced::Result {
    // Set up leveled logging (stderr → journald in production).  Users can
    // override with RUST_LOG, e.g. RUST_LOG=cosmic_ext_applet_eyedropper=debug.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("cosmic_ext_applet_eyedropper=info,wgpu=warn,cosmic=warn,iced=warn"),
    )
    .try_init();

    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();

    // Enable localizations to be applied.
    i18n::init(&requested_languages);

    // Parse command-line arguments (--pick / --help).
    let mut pick = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--pick" | "-p" => pick = true,
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => {
                log::error!("unknown argument: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    // If pick mode was requested and the applet is already running, hand the
    // request to the running instance and exit.  Otherwise start a fresh
    // applet that enters picker mode directly.
    if pick && try_activate_existing_instance("pick") {
        return Ok(());
    }

    // Starts the applet's event loop with the parsed flags.
    cosmic::applet::run::<app::AppModel>(Flags { pick })
}
