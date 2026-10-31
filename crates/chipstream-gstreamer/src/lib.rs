use gst::glib;
use gstreamer as gst;

mod ndsif_transport;
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
