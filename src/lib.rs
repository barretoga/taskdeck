pub mod audio;
pub mod battery;
pub mod config;
pub mod diag;
pub mod media;
pub mod render;
pub mod spectrum;
pub mod voicemeeter;
pub mod widget;

/// Turns the `mic` setting into a comma-separated list of Voicemeeter strips.
/// "auto" picks the first hardware strip whose device looks like a microphone.
pub fn resolve_mic(vm: &voicemeeter::Voicemeeter, setting: &str) -> String {
    if !setting.eq_ignore_ascii_case("auto") {
        return setting.to_string();
    }
    const HINTS: [&str; 6] = ["mic", "micro", "c920", "webcam", "headset", "fone"];
    for i in 0..vm.hardware_strips() {
        if let Some(name) = vm.get_string(&format!("Strip[{i}].device.name")) {
            let lower = name.to_lowercase();
            if HINTS.iter().any(|h| lower.contains(h)) {
                return format!("Strip[{i}]");
            }
        }
    }
    "Strip[0]".into()
}

/// Splits "Strip[0], Bus[1]" into ["Strip[0]", "Bus[1]"].
pub fn targets(list: &str) -> Vec<String> {
    list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}
