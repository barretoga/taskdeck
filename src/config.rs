//! `taskdeck.ini` next to the exe. Created with defaults on first run.
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Auto,
    Voicemeeter,
    Windows,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub mic: String,
    pub speakers: String,
    pub show_battery: bool,
    pub voicemeeter_dll: String,
    pub show_media: bool,
    /// Keys from `media::SOURCES`, e.g. ["spotify", "brave"].
    pub media_sources: Vec<String>,
    pub media_width: i32,
    pub show_bars: bool,
    /// None = same colour as the icons.
    pub bars_color: Option<(u8, u8, u8)>,
    pub mouse_receiver: (u16, u16),
}

impl Default for Config {
    fn default() -> Self {
        parse(DEFAULT)
    }
}

pub const DEFAULT: &str = r#"# TaskDeck settings. After editing, right-click the widget > Reload settings.

# Where the mute buttons act:
#   auto        = Voicemeeter when it is running, otherwise Windows' default devices
#   voicemeeter = always Voicemeeter
#   windows     = always Windows' default playback / recording device
mode = auto

# Voicemeeter strip(s) muted by the microphone button.
# "auto" picks the hardware input whose device name looks like a microphone.
# Otherwise: Strip[0], Strip[1], Strip[2] = hardware inputs 1-3 (Banana).
# Several are allowed, comma separated: Strip[0], Strip[1]
mic = auto

# Voicemeeter bus(es) muted by the speaker button.
# Bus[0] = A1, Bus[1] = A2, Bus[2] = A3, Bus[3] = B1, Bus[4] = B2 (Banana).
speakers = Bus[0]

# Show the mouse battery (2.4 GHz receiver below, or Bluetooth).
show_battery = true
# USB id of the mouse's 2.4 GHz receiver (Attack Shark X11 = 1D57:FA60).
mouse_receiver = 1D57:FA60

voicemeeter_dll = C:\Program Files (x86)\VB\Voicemeeter\VoicemeeterRemote64.dll

# Player (cover, title, artist/channel, back / play-pause / next, audio bars).
# Follows whatever is playing: Spotify or a browser (YouTube and other sites).
show_media = true
# Maximum width of the song/artist text, in pixels at 100% scale.
# It shrinks (and then hides) automatically when the taskbar gets crowded.
media_width = 120
# Animated bars from the player's own audio (only runs while music plays).
show_bars = true
# Bar colour: auto (same as the icons), green, or a hex colour like #FF8800.
bars_color = auto
# Apps the player follows. Remove any you don't want.
# Known: spotify, brave, chrome, edge, firefox, opera, vivaldi
media_sources = spotify, brave, chrome, edge, firefox, opera, vivaldi
"#;

pub fn path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("taskdeck.ini")))
        .unwrap_or_else(|| PathBuf::from("taskdeck.ini"))
}

pub fn load() -> Config {
    let p = path();
    // The app used to be called MouseBar: carry its settings over once.
    let legacy = p.with_file_name("mousebar.ini");
    if !p.exists() && legacy.exists() {
        if let Ok(text) = std::fs::read_to_string(&legacy) {
            if std::fs::write(&p, text.replacen("# MouseBar settings", "# TaskDeck settings", 1)).is_ok() {
                let _ = std::fs::remove_file(&legacy);
            }
        }
    }
    if !p.exists() {
        let _ = std::fs::write(&p, DEFAULT);
    } else if let Ok(existing) = std::fs::read_to_string(&p) {
        // Older settings file: append the sections added since.
        if !existing.contains("show_media") {
            if let Some(i) = DEFAULT.find("# Player (cover") {
                let _ = std::fs::write(&p, format!("{}\n{}", existing.trim_end(), &DEFAULT[i - 1..]));
            }
        } else if !existing.contains("media_sources") {
            // Spotify-only file: swap media_app/media_process for the source list.
            let kept: Vec<&str> = existing
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("media_app") && !t.starts_with("media_process") && !t.starts_with("# Which player to follow")
                })
                .collect();
            let block = DEFAULT.find("# Apps the player follows").map(|i| &DEFAULT[i..]).unwrap_or("");
            let text = kept
                .join("\n")
                .replace(
                    "# Spotify player (cover, song, artist, back / play-pause / next, audio bars).",
                    "# Player (cover, title, artist/channel, back / play-pause / next, audio bars).\n# Follows whatever is playing: Spotify or a browser (YouTube and other sites).",
                );
            let _ = std::fs::write(&p, format!("{}\n{}", text.trim_end(), block));
        }
    }
    std::fs::read_to_string(&p).map(|t| parse(&t)).unwrap_or_default()
}

pub fn parse(text: &str) -> Config {
    let mut c = Config {
        mode: Mode::Auto,
        mic: "auto".into(),
        speakers: "Bus[0]".into(),
        show_battery: true,
        voicemeeter_dll: r"C:\Program Files (x86)\VB\Voicemeeter\VoicemeeterRemote64.dll".into(),
        show_media: true,
        media_sources: ["spotify", "brave", "chrome", "edge", "firefox", "opera", "vivaldi"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        media_width: 120,
        show_bars: true,
        bars_color: None,
        mouse_receiver: (0x1D57, 0xFA60),
    };
    let yes = |v: &str| !matches!(v.to_lowercase().as_str(), "false" | "0" | "no" | "off");
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with(';') {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim().to_lowercase().as_str() {
            "mode" => {
                c.mode = match v.to_lowercase().as_str() {
                    "voicemeeter" => Mode::Voicemeeter,
                    "windows" => Mode::Windows,
                    _ => Mode::Auto,
                }
            }
            "mic" => c.mic = v,
            "speakers" => c.speakers = v,
            "show_battery" => c.show_battery = yes(&v),
            "voicemeeter_dll" => c.voicemeeter_dll = v,
            "show_media" => c.show_media = yes(&v),
            "show_bars" => c.show_bars = yes(&v),
            "mouse_receiver" => {
                if let Some((a, b)) = v.split_once(':') {
                    if let (Ok(vid), Ok(pid)) = (u16::from_str_radix(a.trim(), 16), u16::from_str_radix(b.trim(), 16)) {
                        c.mouse_receiver = (vid, pid);
                    }
                }
            }
            "bars_color" => {
                let v = v.to_lowercase();
                c.bars_color = match v.as_str() {
                    "green" => Some((0x1E, 0xD7, 0x60)),
                    s if s.len() == 7 && s.starts_with('#') => u32::from_str_radix(&s[1..], 16)
                        .ok()
                        .map(|n| ((n >> 16) as u8, (n >> 8) as u8, n as u8)),
                    _ => None,
                }
            }
            "media_sources" => {
                c.media_sources = v.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
            }
            "media_width" => c.media_width = v.parse().unwrap_or(120).clamp(60, 600),
            _ => {}
        }
    }
    c
}
