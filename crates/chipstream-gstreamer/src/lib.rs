//! GStreamer plugin exposing chipstream playback as the `chipstreamsrc` element.
//!
//! The GStreamer `plugin_define!` macro generates [`plugin_register_static`]
//! for applications that register this plugin statically. Dynamic loading uses
//! the exported plugin descriptor instead.

#![doc = include_str!("../README.md")]

use gst::glib;
use gstreamer as gst;

mod source;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    source::register(plugin)
}

gst::plugin_define!(
    chipstream,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    env!("CARGO_PKG_VERSION"),
    "BSD-3-Clause",
    "chipstream-gstreamer",
    "chipstream",
    env!("CARGO_PKG_REPOSITORY"),
    "2026-10-30"
);
