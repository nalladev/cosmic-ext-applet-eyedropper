// SPDX-License-Identifier: MPL-2.0

//! D-Bus activation support.
//!
//! Serves the standard `org.freedesktop.DbusActivation` interface on the
//! session bus so that a second invocation of the applet (for example the
//! `--pick` command-line option bound to a keyboard shortcut) can ask the
//! already-running instance to enter colour-picker mode, instead of starting
//! a second applet.
//!
//! The interface is served at the conventional path
//! `/io/github/nalladev/CosmicExtAppletEyedropper` under the app's well-known
//! name.  If another instance already owns the name (or the session bus is
//! unavailable, e.g. in a sandbox without `--own-name`), this subscription
//! simply stays idle and the applet behaves normally.

use std::any::TypeId;
use std::collections::HashMap;

use crate::app::AppModel;
use cosmic::Application;
use cosmic::iced::Subscription;
use cosmic::iced::futures::channel::mpsc::{Receiver, Sender};
use cosmic::iced::futures::{SinkExt, StreamExt};
use zbus::interface;
use zbus::zvariant::Value;

/// Messages forwarded from the D-Bus activation interface to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// The `pick` action was requested (`--pick` forwarded from a second
    /// invocation of the applet).
    Pick,
}

/// Object server implementing `org.freedesktop.DbusActivation`.
#[derive(Default)]
struct DbusActivation {
    tx: Option<Sender<Activation>>,
}

impl DbusActivation {
    /// Detach the message sender used by the interface handlers.
    fn rx(&mut self) -> Receiver<Activation> {
        let (tx, rx) = cosmic::iced::futures::channel::mpsc::channel(8);
        self.tx = Some(tx);
        rx
    }
}

// The `async` handlers without awaits are deliberate no-ops; unused
// parameters are discarded explicitly to satisfy zbus' generated dispatch.
#[allow(clippy::unused_async)]
#[interface(name = "org.freedesktop.DbusActivation")]
impl DbusActivation {
    async fn activate(&mut self, platform_data: HashMap<&str, Value<'_>>) {
        // The applet has no dedicated "activate" behaviour beyond its popup,
        // so plain activation is a no-op.
        let _ = platform_data;
    }

    async fn open(&mut self, uris: Vec<&str>, platform_data: HashMap<&str, Value<'_>>) {
        // Opening URIs is not supported by the eyedropper.
        let _ = (uris, platform_data);
    }

    async fn activate_action(
        &mut self,
        action: &str,
        parameter: Vec<&str>,
        platform_data: HashMap<&str, Value<'_>>,
    ) {
        if action == "pick"
            && let Some(tx) = &mut self.tx
        {
            let _ = tx.send(Activation::Pick).await;
        }
        // Parameter data (activation tokens etc.) is not used by the applet.
        let _ = (parameter, platform_data);
    }
}

/// Subscribe to activation requests on the session bus.
///
/// The stream claims the app's well-known name on the session bus and serves
/// the `org.freedesktop.DbusActivation` interface at the conventional path
/// derived from the app ID.  Activation requests are forwarded to the
/// application as [`Activation`] messages.
pub fn subscription() -> Subscription<Activation> {
    Subscription::run_with(TypeId::of::<DbusActivation>(), |_| {
        cosmic::iced::stream::channel(10, move |mut output: Sender<Activation>| async move {
            let mut activation = DbusActivation::default();
            let mut rx = activation.rx();

            if let Ok(builder) = zbus::connection::Builder::session() {
                let path: String = format!("/{}", AppModel::APP_ID.replace('.', "/"));
                if let Ok(conn) = builder.build().await {
                    // Serve the interface, then try to claim the well-known
                    // name.  If the name is already owned by another instance
                    // we are the duplicate and stay idle.
                    if conn.object_server().at(path, activation).await == Ok(true)
                        && conn.request_name(AppModel::APP_ID).await.is_ok()
                    {
                        while let Some(msg) = rx.next().await {
                            if output.send(msg).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }

            // Keep the subscription alive for the applet's lifetime.
            loop {
                cosmic::iced::futures::pending!();
            }
        })
    })
}
