// Symbole als Pixelbilder, ohne Abhängigkeiten. Wird auch von build.rs per
// include! eingebunden, um das Programmsymbol der .exe zu erzeugen.

/// Das Würfel-Logo als RGBA-Bild, im Code gezeichnet.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let bg = [0x0b, 0x0e, 0x12, 0xff];
    let cyan = [0x19, 0xe3, 0xf0, 0xff];
    let r = s * 0.2;
    // Abgerundetes Quadrat als Hintergrund.
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (r - fx).max(fx - (s - r)).max(0.0);
            let dy = (r - fy).max(fy - (s - r)).max(0.0);
            if dx * dx + dy * dy <= r * r {
                let i = ((y * size + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&bg);
            }
        }
    }
    // Isometrischer Würfel.
    let c = s / 2.0;
    let h = s * 0.34;
    let w = h * 0.866;
    let top = (c, c - h);
    let ul = (c - w, c - h / 2.0);
    let ur = (c + w, c - h / 2.0);
    let mid = (c, c);
    let ll = (c - w, c + h / 2.0);
    let lr = (c + w, c + h / 2.0);
    let bot = (c, c + h);
    let t = (s / 16.0).max(1.2);
    for (a, b) in [(top, ul), (top, ur), (ul, ll), (ur, lr), (ll, bot), (lr, bot), (ul, mid), (ur, mid), (mid, bot)] {
        line(&mut px, size, a, b, t, cyan);
    }
    px
}

pub fn line(px: &mut [u8], size: u32, a: (f32, f32), b: (f32, f32), thick: f32, col: [u8; 4]) {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let tt = (((fx - a.0) * vx + (fy - a.1) * vy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (a.0 + tt * vx - fx, a.1 + tt * vy - fy);
            let d = (qx * qx + qy * qy).sqrt();
            let cover = (thick / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                for k in 0..3 {
                    px[i + k] = (px[i + k] as f32 * (1.0 - cover) + col[k] as f32 * cover) as u8;
                }
                px[i + 3] = px[i + 3].max((255.0 * cover) as u8);
            }
        }
    }
}

/// Disc mit weicher Kante.
pub fn disc(px: &mut [u8], size: u32, c: (f32, f32), r: f32, col: [u8; 4]) {
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - c.0, y as f32 + 0.5 - c.1);
            let cover = (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if cover > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                for k in 0..3 {
                    px[i + k] = (px[i + k] as f32 * (1.0 - cover) + col[k] as f32 * cover) as u8;
                }
                px[i + 3] = px[i + 3].max((255.0 * cover) as u8);
            }
        }
    }
}

/// Ordnerbild als RGBA.
pub fn folder_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let tab = [0x0f, 0xa8, 0xb4, 0xff];
    let body = [0x19, 0xe3, 0xf0, 0xff];
    let ink = [0x0b, 0x0e, 0x12, 0xff];
    let r = s * 0.06;
    // Reiter hinten links
    round_rect(&mut px, size, (s * 0.06, s * 0.14), (s * 0.46, s * 0.34), r, tab);
    // Rückwand
    round_rect(&mut px, size, (s * 0.06, s * 0.22), (s * 0.94, s * 0.86), r, tab);
    // Vorderseite
    round_rect(&mut px, size, (s * 0.06, s * 0.30), (s * 0.94, s * 0.86), r, body);
    // Würfel auf der Vorderseite
    let c = (s * 0.5, s * 0.58);
    let h = s * 0.19;
    let w = h * 0.866;
    let pt = |x: f32, y: f32| (c.0 + x, c.1 + y);
    let (top, ul, ur, mid, ll, lr, bot) = (pt(0.0, -h), pt(-w, -h / 2.0), pt(w, -h / 2.0), pt(0.0, 0.0), pt(-w, h / 2.0), pt(w, h / 2.0), pt(0.0, h));
    let t = (s / 22.0).max(1.0);
    for (a, b) in [(top, ul), (top, ur), (ul, ll), (ur, lr), (ll, bot), (lr, bot), (ul, mid), (ur, mid), (mid, bot)] {
        line(&mut px, size, a, b, t, ink);
    }
    px
}

fn round_rect(px: &mut [u8], size: u32, a: (f32, f32), b: (f32, f32), r: f32, col: [u8; 4]) {
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (a.0 + r - fx).max(fx - (b.0 - r)).max(0.0);
            let dy = (a.1 + r - fy).max(fy - (b.1 - r)).max(0.0);
            let inside_box = fx >= a.0 - 0.5 && fx <= b.0 + 0.5 && fy >= a.1 - 0.5 && fy <= b.1 + 0.5;
            if !inside_box {
                continue;
            }
            let d = (dx * dx + dy * dy).sqrt() - r;
            let edge = (a.0 - fx).max(fx - b.0).max(a.1 - fy).max(fy - b.1);
            let cover = (0.5 - d.max(edge)).clamp(0.0, 1.0);
            if cover > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                for k in 0..3 {
                    px[i + k] = (px[i + k] as f32 * (1.0 - cover) + col[k] as f32 * cover) as u8;
                }
                px[i + 3] = px[i + 3].max((255.0 * cover) as u8);
            }
        }
    }
}

/// Erzeugt eine .ico-Datei (BMP-Einträge, 32 Bit) mit mehreren Größen.
pub fn ico_bytes(sizes: &[u32], draw: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
    let mut images = Vec::new();
    for &sz in sizes {
        let rgba = draw(sz);
        let mut img = Vec::new();
        // BITMAPINFOHEADER; Höhe doppelt (Farbe + Maske)
        img.extend_from_slice(&40u32.to_le_bytes());
        img.extend_from_slice(&(sz as i32).to_le_bytes());
        img.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());
        let mask_row = sz.div_ceil(32) * 4;
        img.extend_from_slice(&(sz * sz * 4 + mask_row * sz).to_le_bytes());
        img.extend_from_slice(&[0u8; 16]);
        // Pixel von unten nach oben, BGRA
        for y in (0..sz).rev() {
            for x in 0..sz {
                let i = ((y * sz + x) * 4) as usize;
                img.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        // AND-Maske (leer, Alpha regelt die Transparenz)
        img.extend(std::iter::repeat(0u8).take((mask_row * sz) as usize));
        images.push((sz, img));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (sz, img) in &images {
        let b = if *sz >= 256 { 0u8 } else { *sz as u8 };
        out.extend_from_slice(&[b, b, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for (_, img) in images {
        out.extend_from_slice(&img);
    }
    out
}

