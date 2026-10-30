//! Client bindings for panoptes_window_v1, generated from the desk's copy
//! of the Cage fork's protocol file. checks.window-protocol fails when the
//! copy and the fork's file differ.

#![allow(missing_debug_implementations, clippy::all)]

use wayland_client;

pub mod __interfaces {
    wayland_scanner::generate_interfaces!("protocols/panoptes-window-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_client_code!("protocols/panoptes-window-v1.xml");
