//! Dev tool: renders widget states to BMP files without the taskbar.
//! Usage:
//!   preview <out.bmp> [responsive|themes]
//!     responsive: the same player squeezed into less and less room
//!     themes:     dark/light, normal and muted + low battery
//!   preview <out_dir> frames <scale> <count> <seconds_per_frame>
//!     animation frames of the demo widget (bars moving), e.g. for a post image
use taskdeck::render::{compose, draw_bars, layout, Item, Rgb, View};

fn view(battery: u8, light: bool, muted: bool) -> View {
    View {
        mic: Some(muted),
        speaker: Some(muted),
        battery: Some(battery),
        light,
        hover: None::<Item>,
        media: Some(taskdeck::demo::media(true)),
        media_text_w: 120,
        ..Default::default()
    }
}

/// Renders the views stacked vertically over a solid taskbar-like background.
fn render(views: &[View], levels: &[f32], scale: f32, h: i32) -> (Vec<u8>, i32, i32) {
    let rows: Vec<_> = views
        .iter()
        .map(|v| {
            let l = layout(v, scale, h);
            let mut px = compose(v, &l, scale);
            if let Some(span) = l.span(Item::Bars) {
                let c = if v.light { Rgb(0x1A, 0x1A, 0x1A) } else { Rgb(0xFF, 0xFF, 0xFF) };
                draw_bars(&mut px, l.w, h, span, levels, scale, c);
            }
            (px, l, v.light)
        })
        .collect();
    let w = rows.iter().map(|r| r.1.w).max().unwrap();
    let total_h = h * rows.len() as i32;
    let mut img = vec![0u8; (w * total_h * 3) as usize];
    for (ri, (px, l, light)) in rows.iter().enumerate() {
        let bg: u32 = if *light { 0xEE } else { 0x20 };
        for y in 0..h {
            for x in 0..w {
                let p = if x < l.w { px[(y * l.w + x) as usize] } else { 0 };
                let a = p >> 24;
                let ch = |s: u32| (((p >> s) & 0xFF) + bg * (255 - a) / 255).min(255) as u8;
                let row = ri as i32 * h + y;
                let i = ((row * w + x) * 3) as usize;
                img[i] = ch(0);
                img[i + 1] = ch(8);
                img[i + 2] = ch(16);
            }
        }
    }
    (img, w, total_h)
}

/// Minimal 24-bit top-down BMP.
fn write_bmp(path: &str, img: &[u8], w: i32, h: i32) {
    let stride = ((w * 3 + 3) & !3) as usize;
    let data_len = stride * h as usize;
    let mut f = Vec::with_capacity(54 + data_len);
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&((54 + data_len) as u32).to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&(-h).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&24u16.to_le_bytes());
    f.extend_from_slice(&[0; 24]);
    for y in 0..h as usize {
        let row = &img[y * w as usize * 3..(y + 1) * w as usize * 3];
        f.extend_from_slice(row);
        f.extend(std::iter::repeat(0).take(stride - row.len()));
    }
    std::fs::write(path, f).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "preview.bmp".into());
    let mode = args.get(2).cloned().unwrap_or_else(|| "responsive".into());

    if mode == "frames" {
        let scale: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2.0);
        let count: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(30);
        let dt: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.07);
        let h = (48.0 * scale) as i32;
        std::fs::create_dir_all(&out).unwrap();
        let v = view(87, false, false);
        for i in 0..count {
            let levels = taskdeck::demo::levels_at(i as f32 * dt);
            let (img, w, th) = render(std::slice::from_ref(&v), &levels, scale, h);
            write_bmp(&format!("{out}/f{i:03}.bmp"), &img, w, th);
        }
        println!("wrote {count} frames to {out}");
        return;
    }

    let scale = 1.5;
    let h = (48.0 * scale) as i32;
    let views: Vec<View> = if mode == "themes" {
        vec![view(87, false, false), view(15, false, true), view(87, true, false), view(15, true, true)]
    } else {
        let full = view(87, false, false);
        [520, 420, 330, 270, 200]
            .iter()
            .map(|room| taskdeck::render::fit(&full, scale, h, Some((*room as f32 * scale) as i32)))
            .collect()
    };
    let levels = [0.55, 0.9, 0.7, 0.45, 0.8, 0.6, 0.35, 0.5, 0.3, 0.2];
    let (img, w, th) = render(&views, &levels, scale, h);
    write_bmp(&out, &img, w, th);
    println!("wrote {out}");
}
