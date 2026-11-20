// Embeds an application manifest. Declaring Windows 8+ support is required for
// layered *child* windows (the widget lives inside the taskbar as one).
use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("TaskDeck").dpi_awareness(DpiAwareness::PerMonitorV2))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
