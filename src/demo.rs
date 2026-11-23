//! Demo mode (`--demo`, `--demo-low`): the real widget on the real taskbar, but
//! fed with made-up data, for screenshots and recordings. Nothing personal (song,
//! cover art) is shown, and the mute buttons only change the picture, never the
//! real microphone or speakers.
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::render::{Cover, MediaView};
use crate::spectrum::BANDS;

#[derive(Clone, Copy, Debug)]
pub struct Demo {
    /// Mic and speakers start muted.
    pub muted: bool,
    pub battery: u8,
}

pub fn from_args() -> Option<Demo> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--demo-low") {
        Some(Demo { muted: true, battery: 15 })
    } else if args.iter().any(|a| a == "--demo") {
        Some(Demo { muted: false, battery: 87 })
    } else {
        None
    }
}

pub fn media(playing: bool) -> MediaView {
    static COVER: OnceLock<Arc<Cover>> = OnceLock::new();
    MediaView {
        title: "Midnight Drive".into(),
        artist: "TaskDeck Demo".into(),
        playing,
        cover: Some(COVER.get_or_init(|| Arc::new(cover())).clone()),
    }
}

/// A generated "album cover": purple-to-teal gradient with a vinyl-like disc.
fn cover() -> Cover {
    const S: i32 = 96;
    let mut px = Vec::with_capacity((S * S) as usize);
    for y in 0..S {
        for x in 0..S {
            let t = (x + y) as f32 / (2 * S) as f32;
            let mix = |a: f32, b: f32| a + (b - a) * t;
            let (mut r, mut g, mut b) = (mix(123.0, 0.0), mix(47.0, 201.0), mix(247.0, 167.0));
            let d = (((x - 48) * (x - 48) + (y - 48) * (y - 48)) as f32).sqrt();
            // Disc, grooves and label.
            let disc = (30.5 - d).clamp(0.0, 1.0);
            let groove = if d < 30.0 && d > 12.0 && (d as i32) % 4 == 0 { 0.12 } else { 0.0 };
            let label = (10.5 - d).clamp(0.0, 1.0);
            let shade = 1.0 - 0.75 * disc + groove;
            r *= shade;
            g *= shade;
            b *= shade;
            r += (255.0 - r) * label * 0.9;
            g += (255.0 - g) * label * 0.9;
            b += (255.0 - b) * label * 0.9;
            let hole = (2.5 - d).clamp(0.0, 1.0);
            r *= 1.0 - hole;
            g *= 1.0 - hole;
            b *= 1.0 - hole;
            px.push(0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32);
        }
    }
    Cover { size: S, px }
}

/// Lively, music-like bar levels from a few mixed sine waves.
pub fn levels() -> [f32; BANDS] {
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let beat = (0.5 + 0.5 * (t * 7.5).sin()).powi(3); // ~1.2 beats per second
    let mut out = [0.0; BANDS];
    for (i, v) in out.iter_mut().enumerate() {
        let f = i as f32;
        let base = 0.62 - f * 0.035;
        let wave = 0.22 * (t * (2.3 + f * 0.41) + f * 1.7).sin() + 0.12 * (t * (5.1 + f * 0.9)).sin();
        let kick = if i < 3 { 0.35 * beat } else { 0.1 * beat };
        *v = (base + wave + kick - 0.15).clamp(0.05, 1.0);
    }
    out
}
