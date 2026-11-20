//! Draws the widget into a premultiplied-ARGB pixel buffer (for UpdateLayeredWindow).
//! Glyphs come from Segoe Fluent Icons, so everything stays sharp at any DPI.
//!
//! `compose` draws the static part (only on state/hover changes); the spectrum
//! bars are drawn on top of a copy of it every animation frame by `draw_bars`.
use std::sync::Arc;

use windows::core::HSTRING;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Mic = 0,
    Speaker = 1,
    Battery = 2,
    Media = 3,
    Previous = 4,
    PlayPause = 5,
    Next = 6,
    Bars = 7,
}

impl Item {
    pub fn from_id(id: usize) -> Option<Item> {
        use Item::*;
        [Mic, Speaker, Battery, Media, Previous, PlayPause, Next, Bars].into_iter().find(|i| *i as usize == id)
    }
}

/// Square premultiplied BGRA image (cover art).
#[derive(Clone, PartialEq, Debug)]
pub struct Cover {
    pub size: i32,
    pub px: Vec<u32>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct MediaView {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub cover: Option<Arc<Cover>>,
}

#[derive(Clone, PartialEq, Default, Debug)]
pub struct View {
    pub mic: Option<bool>,
    pub speaker: Option<bool>,
    pub battery: Option<u8>,
    pub media: Option<MediaView>,
    /// Width of the song/artist text column, in 100%-scale pixels.
    pub media_text_w: i32,
    /// Compact steps used when the taskbar is crowded (see `fit`).
    pub hide_media_text: bool,
    pub hide_bars: bool,
    pub hide_nav: bool,
    pub light: bool,
    pub hover: Option<Item>,
}

#[derive(Clone, PartialEq, Default, Debug)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    /// (item, x start, x end)
    pub items: Vec<(Item, i32, i32)>,
}

impl Layout {
    pub fn span(&self, item: Item) -> Option<(i32, i32)> {
        self.items.iter().find(|i| i.0 == item).map(|i| (i.1, i.2))
    }
}

const GLYPH_MIC: &str = "\u{E720}";
const GLYPH_VOLUME: &str = "\u{E767}";
const GLYPH_MUTE: &str = "\u{E74F}";
const GLYPH_PREV: &str = "\u{E892}";
const GLYPH_NEXT: &str = "\u{E893}";
const GLYPH_PLAY: &str = "\u{E768}";
const GLYPH_PAUSE: &str = "\u{E769}";
const GLYPH_MUSIC: &str = "\u{E8D6}";

#[derive(Clone, Copy)]
pub struct Rgb(pub u32, pub u32, pub u32);
const RED: Rgb = Rgb(0xE8, 0x4A, 0x4A);
pub const SPOTIFY_GREEN: Rgb = Rgb(0x1E, 0xD7, 0x60);

/// Width of one icon button (100% scale). A 16 px glyph leaves 8 px on each side.
const SLOT: f32 = 32.0;
/// Inner padding of a slot around its content: (SLOT - 16) / 2.
const SLOT_PAD: f32 = 8.0;
const BARS_W: f32 = 44.0;
/// Cover area: 4 + 32 + 4. With text: + 4 gap + text width.
const COVER_W: f32 = 40.0;
const COVER_TEXT_GAP: f32 = 4.0;
/// Below this the song text is unreadable, so it is hidden instead.
const MIN_TEXT_W: f32 = 64.0;
/// Extra space between the player group and the mute group.
const GROUP_GAP: f32 = 8.0;
/// Battery outline width (100% scale).
const BATTERY_BODY_W: f32 = 21.0;

/// Width of the "87%" text (bold 12 px Segoe UI: ~7 px per digit + ~10 px for "%").
fn battery_text_w(pct: u8) -> f32 {
    7.0 * pct.min(100).to_string().len() as f32 + 10.5
}

/// Text + gap + outline + tip, with slot padding on both sides.
fn battery_w(pct: u8) -> f32 {
    6.0 + battery_text_w(pct) + 4.0 + BATTERY_BODY_W + 2.5 + SLOT_PAD
}

fn sc(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}

/// Shrinks the view until it is at most `room` pixels wide, in this order:
/// shorter song text, cover only (no text), no audio bars, no back/next,
/// and finally no player at all. Mute buttons and battery always stay.
pub fn fit(view: &View, scale: f32, h: i32, room: Option<i32>) -> View {
    let mut v = view.clone();
    let Some(room) = room else { return v };
    let width = |v: &View| layout(v, scale, h).w;
    if v.media.is_none() || width(&v) <= room {
        return v;
    }
    let mut cover_only = v.clone();
    cover_only.hide_media_text = true;
    let spare = (room - width(&cover_only)) as f32 / scale - COVER_TEXT_GAP;
    if spare >= MIN_TEXT_W {
        v.media_text_w = spare.floor() as i32;
        return v;
    }
    v = cover_only;
    for step in 0..3 {
        if width(&v) <= room {
            return v;
        }
        match step {
            0 => v.hide_bars = true,
            1 => v.hide_nav = true,
            _ => v.media = None,
        }
    }
    v
}

pub fn layout(view: &View, scale: f32, h: i32) -> Layout {
    let pad = sc(2.0, scale);
    let mut x = pad;
    let mut items = Vec::new();
    let mut push = |it: Item, w: i32, x: &mut i32| {
        items.push((it, *x, *x + w));
        *x += w;
    };
    // Every icon gets the same slot, so the gaps between glyphs are equal.
    let slot = sc(SLOT, scale);
    if view.media.is_some() {
        let text_w = if view.media_text_w > 0 { view.media_text_w } else { 120 };
        let media_w = if view.hide_media_text { COVER_W } else { COVER_W + COVER_TEXT_GAP + text_w as f32 };
        push(Item::Media, sc(media_w, scale), &mut x);
        if !view.hide_nav {
            push(Item::Previous, slot, &mut x);
        }
        push(Item::PlayPause, slot, &mut x);
        if !view.hide_nav {
            push(Item::Next, slot, &mut x);
        }
        if !view.hide_bars {
            push(Item::Bars, sc(BARS_W, scale), &mut x);
        }
        x += sc(GROUP_GAP, scale);
    }
    push(Item::Mic, slot, &mut x);
    push(Item::Speaker, slot, &mut x);
    if view.battery.is_some() {
        push(Item::Battery, sc(battery_w(view.battery.unwrap_or(0)), scale), &mut x);
    }
    Layout { w: x + pad, h, items }
}

/// Offscreen GDI surface used to rasterize one text run at a time (white on black = coverage).
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

const CENTER: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOPREFIX.0);
const LEFT: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOPREFIX.0 | DT_END_ELLIPSIS.0);

impl Surface {
    unsafe fn new(w: i32, h: i32) -> Self {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).expect("CreateDIBSection");
        ReleaseDC(None, screen);
        let old = SelectObject(dc, bmp);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        SetBkMode(dc, TRANSPARENT);
        Surface { dc, bmp, old, bits: bits as *mut u32, len: (w * h) as usize }
    }

    unsafe fn text(&self, text: &str, font: HFONT, mut rc: RECT, fmt: DRAW_TEXT_FORMAT) -> &[u32] {
        std::slice::from_raw_parts_mut(self.bits, self.len).fill(0);
        let prev = SelectObject(self.dc, font);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(self.dc, &mut wide, &mut rc, fmt);
        SelectObject(self.dc, prev);
        let _ = GdiFlush();
        std::slice::from_raw_parts(self.bits, self.len)
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

unsafe fn font(face: &str, px: i32, weight: i32) -> HFONT {
    CreateFontW(
        -px, 0, 0, 0, weight, 0, 0, 0,
        DEFAULT_CHARSET.0 as u32,
        OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        ANTIALIASED_QUALITY.0 as u32,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        &HSTRING::from(face),
    )
}

/// Source-over blend of a premultiplied pixel (a, r, g, b) onto dst.
#[inline]
fn over(dst: u32, a: u32, r: u32, g: u32, b: u32) -> u32 {
    let inv = 255 - a;
    let ch = |shift: u32, s: u32| (s + ((dst >> shift) & 0xFF) * inv / 255).min(255);
    (ch(24, a) << 24) | (ch(16, r) << 16) | (ch(8, g) << 8) | ch(0, b)
}

#[inline]
fn paint(out: &mut [u32], i: usize, c: Rgb, a: u32) {
    if a > 0 {
        out[i] = over(out[i], a, c.0 * a / 255, c.1 * a / 255, c.2 * a / 255);
    }
}

fn blend_mask(out: &mut [u32], mask: &[u32], c: Rgb, alpha: u32) {
    for (i, &m) in mask.iter().enumerate() {
        let cov = ((m >> 16) & 0xFF).max((m >> 8) & 0xFF).max(m & 0xFF);
        paint(out, i, c, cov * alpha / 255);
    }
}

/// Anti-aliased coverage of a rounded rectangle at pixel (x, y).
fn round_cov(x: i32, y: i32, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32) -> f32 {
    let fx = x as f32 + 0.5;
    let fy = y as f32 + 0.5;
    let dx = (x0 + r - fx).max(fx - (x1 - r)).max(0.0);
    let dy = (y0 + r - fy).max(fy - (y1 - r)).max(0.0);
    let edge = (fx - x0).min(x1 - fx).min(fy - y0).min(y1 - fy) + 0.5;
    (r + 0.5 - (dx * dx + dy * dy).sqrt()).min(edge).clamp(0.0, 1.0)
}

fn round_rect(out: &mut [u32], w: i32, h: i32, rect: (f32, f32, f32, f32), r: f32, c: Rgb, alpha: u32) {
    for y in (rect.1.floor() as i32).max(0)..(rect.3.ceil() as i32).min(h) {
        for x in (rect.0.floor() as i32).max(0)..(rect.2.ceil() as i32).min(w) {
            let a = (alpha as f32 * round_cov(x, y, rect, r)) as u32;
            paint(out, (y * w + x) as usize, c, a);
        }
    }
}

/// Diagonal "muted" slash centred at (cx, cy).
fn slash(out: &mut [u32], w: i32, h: i32, cx: f32, cy: f32, half: f32, thick: f32, c: Rgb) {
    let r = (half + thick).ceil() as i32;
    for y in (cy as i32 - r).max(0)..(cy as i32 + r).min(h) {
        for x in (cx as i32 - r).max(0)..(cx as i32 + r).min(w) {
            let px = x as f32 + 0.5 - cx;
            let py = y as f32 + 0.5 - cy;
            let along = (px + py) / std::f32::consts::SQRT_2;
            if along.abs() > half {
                continue;
            }
            let d = (px - py).abs() / std::f32::consts::SQRT_2;
            let cov = (thick / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            paint(out, (y * w + x) as usize, c, (255.0 * cov) as u32);
        }
    }
}

/// Cover art scaled (bilinear) into a rounded square.
fn draw_cover(out: &mut [u32], w: i32, h: i32, cover: &Cover, x0: i32, y0: i32, size: i32, radius: f32) {
    let rect = (x0 as f32, y0 as f32, (x0 + size) as f32, (y0 + size) as f32);
    let s = cover.size;
    let sample = |sx: i32, sy: i32| cover.px[(sy.clamp(0, s - 1) * s + sx.clamp(0, s - 1)) as usize];
    for y in y0.max(0)..(y0 + size).min(h) {
        for x in x0.max(0)..(x0 + size).min(w) {
            let cov = round_cov(x, y, rect, radius);
            if cov <= 0.0 {
                continue;
            }
            let u = ((x - x0) as f32 + 0.5) * s as f32 / size as f32 - 0.5;
            let v = ((y - y0) as f32 + 0.5) * s as f32 / size as f32 - 0.5;
            let (ux, vy) = (u.floor() as i32, v.floor() as i32);
            let (fu, fv) = (u - ux as f32, v - vy as f32);
            let (p00, p10, p01, p11) = (sample(ux, vy), sample(ux + 1, vy), sample(ux, vy + 1), sample(ux + 1, vy + 1));
            let ch = |shift: u32| {
                let g = |p: u32| ((p >> shift) & 0xFF) as f32;
                let top = g(p00) * (1.0 - fu) + g(p10) * fu;
                let bot = g(p01) * (1.0 - fu) + g(p11) * fu;
                ((top * (1.0 - fv) + bot * fv) * cov) as u32
            };
            let a = ch(24);
            let i = (y * w + x) as usize;
            if a > 0 {
                out[i] = over(out[i], a, ch(16), ch(8), ch(0));
            }
        }
    }
}

/// "87%" in bold followed by a small battery outline (with tip) filled to the level,
/// like a phone status bar. Drawn inside the Battery slot starting at `x`.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_battery(out: &mut [u32], w: i32, h: i32, s: &Surface, f: HFONT, x: i32, pct: u8, scale: f32, fg: Rgb) {
    let low = pct <= 20;
    let text_color = if low { RED } else { fg };

    // Percentage, right-aligned against the battery.
    let text_l = x + sc(6.0, scale);
    let text_r = text_l + sc(battery_text_w(pct), scale);
    let rc = RECT { left: text_l, top: 0, right: text_r, bottom: h };
    let right = DRAW_TEXT_FORMAT(DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOPREFIX.0);
    blend_mask(out, s.text(&format!("{pct}%"), f, rc, right), text_color, 255);

    // Body.
    let bw = BATTERY_BODY_W * scale;
    let bh = 11.0 * scale;
    let stroke = (1.2 * scale).max(1.0);
    let r = 2.6 * scale;
    let x0 = (text_r + sc(4.0, scale)) as f32;
    let y0 = ((h as f32 - bh) / 2.0).round();
    let body = (x0, y0, x0 + bw, y0 + bh);
    let inner = (x0 + stroke, y0 + stroke, x0 + bw - stroke, y0 + bh - stroke);
    for py in (y0.floor() as i32).max(0)..((y0 + bh).ceil() as i32).min(h) {
        for px in (x0.floor() as i32).max(0)..((x0 + bw).ceil() as i32).min(w) {
            let cov = (round_cov(px, py, body, r) - round_cov(px, py, inner, (r - stroke).max(0.5))).max(0.0);
            paint(out, (py * w + px) as usize, fg, (cov * 255.0) as u32);
        }
    }
    // Tip.
    let tip = (x0 + bw + 0.6 * scale, y0 + bh * 0.3, x0 + bw + 2.2 * scale, y0 + bh * 0.7);
    round_rect(out, w, h, tip, 0.8 * scale, fg, 255);

    // Fill, inset from the outline: green when healthy, icon colour mid, red low.
    let gap = 1.3 * scale;
    let area = (inner.0 + gap, inner.1 + gap, inner.2 - gap, inner.3 - gap);
    let level = (pct.min(100) as f32 / 100.0).max(0.08); // always show a sliver
    let fill = (area.0, area.1, area.0 + (area.2 - area.0) * level, area.3);
    let color = match pct {
        0..=20 => RED,
        21..=50 => fg,
        _ => Rgb(0x34, 0xC7, 0x59),
    };
    round_rect(out, w, h, fill, 1.0 * scale, color, 255);
}
/// Spectrum bars inside [x0, x1). `levels` are 0..1.
pub fn draw_bars(out: &mut [u32], w: i32, h: i32, (x0, x1): (i32, i32), levels: &[f32], scale: f32, c: Rgb) {
    let n = levels.len().max(1) as f32;
    let pad = SLOT_PAD * scale;
    let slot = ((x1 - x0) as f32 - 2.0 * pad) / n;
    let bar_w = (slot * 0.62).max(1.0);
    let max_h = 22.0 * scale;
    let min_h = 2.0 * scale;
    let cy = h as f32 / 2.0;
    for (i, &lv) in levels.iter().enumerate() {
        let bh = min_h + (max_h - min_h) * lv.clamp(0.0, 1.0);
        let bx = x0 as f32 + pad + i as f32 * slot + (slot - bar_w) / 2.0;
        round_rect(out, w, h, (bx, cy - bh / 2.0, bx + bar_w, cy + bh / 2.0), (bar_w / 2.0).min(1.5 * scale), c, 235);
    }
}

pub fn compose(view: &View, l: &Layout, scale: f32) -> Vec<u32> {
    let (w, h) = (l.w, l.h);
    // Alpha 1 everywhere: invisible, but still receives mouse clicks.
    let mut out = vec![0x0100_0000u32; (w.max(0) * h.max(0)) as usize];
    if w <= 0 || h <= 0 {
        return out;
    }
    let fg = if view.light { Rgb(0x1A, 0x1A, 0x1A) } else { Rgb(0xFF, 0xFF, 0xFF) };
    let (hover_c, hover_a) = if view.light { (Rgb(0, 0, 0), 0x12) } else { (Rgb(0xFF, 0xFF, 0xFF), 0x1C) };
    let hover_h = sc(40.0, scale).min(h);
    let hy = (h - hover_h) / 2;
    unsafe {
        let s = Surface::new(w, h);
        let icon = font("Segoe Fluent Icons", sc(16.0, scale), 400);
        let icon_small = font("Segoe Fluent Icons", sc(13.0, scale), 400);
        let text = font("Segoe UI", sc(13.0, scale), 600);
        let title_f = font("Segoe UI", sc(12.5, scale), 600);
        let artist_f = font("Segoe UI", sc(11.5, scale), 400);
        let battery_f = font("Segoe UI", sc(12.0, scale), 700);
        for &(it, x0, x1) in &l.items {
            if view.hover == Some(it) && it != Item::Bars {
                let r = (
                    (x0 + sc(2.0, scale)) as f32,
                    hy as f32,
                    (x1 - sc(2.0, scale)) as f32,
                    (hy + hover_h) as f32,
                );
                round_rect(&mut out, w, h, r, 4.0 * scale, hover_c, hover_a);
            }
            let rc = RECT { left: x0, top: 0, right: x1, bottom: h };
            match it {
                Item::Mic | Item::Speaker => {
                    let state = if it == Item::Mic { view.mic } else { view.speaker };
                    let muted = state == Some(true);
                    let (color, alpha) = match state {
                        Some(true) => (RED, 255),
                        Some(false) => (fg, 255),
                        None => (fg, 110),
                    };
                    let glyph = match (it, muted) {
                        (Item::Speaker, true) => GLYPH_MUTE,
                        (Item::Speaker, false) => GLYPH_VOLUME,
                        _ => GLYPH_MIC,
                    };
                    blend_mask(&mut out, s.text(glyph, icon, rc, CENTER), color, alpha);
                    if muted && it == Item::Mic {
                        let cx = (x0 + x1) as f32 / 2.0;
                        slash(&mut out, w, h, cx, h as f32 / 2.0, 8.0 * scale, 1.6 * scale, RED);
                    }
                }
                Item::Battery => {
                    let pct = view.battery.unwrap_or(0);
                    draw_battery(&mut out, w, h, &s, battery_f, x0, pct, scale, fg);
                }
                Item::Media => {
                    let Some(m) = &view.media else { continue };
                    let size = sc(32.0, scale);
                    let cx0 = x0 + sc(4.0, scale);
                    let cy0 = (h - size) / 2;
                    match &m.cover {
                        Some(c) => draw_cover(&mut out, w, h, c, cx0, cy0, size, 4.0 * scale),
                        None => {
                            let r = (cx0 as f32, cy0 as f32, (cx0 + size) as f32, (cy0 + size) as f32);
                            round_rect(&mut out, w, h, r, 4.0 * scale, fg, 0x22);
                            let g = RECT { left: cx0, top: cy0, right: cx0 + size, bottom: cy0 + size };
                            blend_mask(&mut out, s.text(GLYPH_MUSIC, icon, g, CENTER), fg, 200);
                        }
                    }
                    if view.hide_media_text {
                        continue;
                    }
                    let tx = cx0 + size + sc(8.0, scale);
                    let mid = h / 2;
                    let title = if m.title.is_empty() { "Spotify" } else { &m.title };
                    let tr = RECT { left: tx, top: mid - sc(17.0, scale), right: x1 - sc(2.0, scale), bottom: mid };
                    blend_mask(&mut out, s.text(title, title_f, tr, LEFT), fg, 255);
                    let ar = RECT { left: tx, top: mid, right: x1 - sc(2.0, scale), bottom: mid + sc(16.0, scale) };
                    blend_mask(&mut out, s.text(&m.artist, artist_f, ar, LEFT), fg, 175);
                }
                Item::Previous | Item::Next | Item::PlayPause => {
                    let playing = view.media.as_ref().is_some_and(|m| m.playing);
                    let (glyph, f) = match it {
                        Item::Previous => (GLYPH_PREV, icon_small),
                        Item::Next => (GLYPH_NEXT, icon_small),
                        _ => (if playing { GLYPH_PAUSE } else { GLYPH_PLAY }, icon),
                    };
                    blend_mask(&mut out, s.text(glyph, f, rc, CENTER), fg, 255);
                }
                Item::Bars => {}
            }
        }
        for f in [icon, icon_small, text, title_f, artist_f, battery_f] {
            let _ = DeleteObject(f);
        }
    }
    out
}
