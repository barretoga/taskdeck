//! Dev tool: renders widget states to a BMP so the look can be checked without the taskbar.
use taskdeck::render::{compose, layout, Item, View};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "preview.bmp".into());
    let scale = 1.5;
    let h = (48.0 * scale) as i32;
    let v = |battery: u8, light: bool| View {
        mic: Some(false),
        speaker: Some(false),
        battery: Some(battery),
        light,
        hover: None::<Item>,
        ..Default::default()
    };
    let mut with_media = v(87, false);
    with_media.media = Some(taskdeck::render::MediaView { title: std::env::args().nth(2).unwrap_or("Song Within A Song".into()), artist: std::env::args().nth(3).unwrap_or("Camel".into()), playing: true, cover: None });
    with_media.media_text_w = 120;
    // Responsiveness: the same player squeezed into less and less room (100% scale px).
    let views: Vec<View> = [520, 420, 330, 270, 230, 200, 150]
        .iter()
        .map(|room| taskdeck::render::fit(&with_media, scale, h, Some((*room as f32 * scale) as i32)))
        .collect();
    let rows: Vec<_> = views
        .iter()
        .map(|v| {
            let l = layout(v, scale, h);
            (compose(v, &l, scale), l, v.light)
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
    // Minimal 24-bit top-down BMP.
    let stride = ((w * 3 + 3) & !3) as usize;
    let mut f = Vec::new();
    let data_len = stride * total_h as usize;
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&((54 + data_len) as u32).to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&(-total_h).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&24u16.to_le_bytes());
    f.extend_from_slice(&[0; 24]);
    for y in 0..total_h as usize {
        let row = &img[y * w as usize * 3..(y + 1) * w as usize * 3];
        f.extend_from_slice(row);
        f.extend(std::iter::repeat(0).take(stride - row.len()));
    }
    std::fs::write(&out, f).unwrap();
    println!("wrote {out}");
}

