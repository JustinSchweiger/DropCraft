// Symbole als Pixelbilder, ohne Abhängigkeiten. Wird auch von build.rs per
// include! eingebunden, um das Programmsymbol der .exe zu erzeugen.

/// Bei jeder Änderung am Logo erhöhen: Die .ico-Dateien in %APPDATA% bekommen
/// dann neue Namen, sonst zeigen Explorer und Benachrichtigungen das alte
/// Symbol aus ihrem Cache weiter.
pub const LOGO_REV: u32 = 2;

/// Das Logo (Tropfen mit Sync-Pfeilen auf blauer Kachel) als RGBA-Bild,
/// im Code gezeichnet. Ab 48 px mit Pfeilen, darunter vereinfacht.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    logo(size, size >= 48, false)
}

/// Vereinfachtes Logo (ohne Pfeile, größerer Tropfen) für die Taskleiste.
pub fn logo_simple_rgba(size: u32) -> Vec<u8> {
    logo(size, false, false)
}

/// Randlos und eckig, für Symbole, die das System selbst abrundet (iOS, Web-App).
pub fn logo_full_bleed_rgba(size: u32) -> Vec<u8> {
    logo(size, size >= 48, true)
}

type Rgb = [f32; 3];

fn hex(v: u32) -> Rgb {
    [((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Abstand (Einheiten) -> Deckung mit 1 Pixel Kantenglättung.
fn cover(d: f32, px: f32) -> f32 {
    (0.5 - d / px).clamp(0.0, 1.0)
}

fn sd_round_rect(x: f32, y: f32, c: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let qx = (x - c.0).abs() - half.0 + r;
    let qy = (y - c.1).abs() - half.1 + r;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Tropfen: Kreis `r1` um `c`, Spitze als Kreis `r2` im Abstand `h` darüber.
fn sd_drop(x: f32, y: f32, c: (f32, f32), r1: f32, r2: f32, h: f32) -> f32 {
    let (px, py) = ((x - c.0).abs(), c.1 - y);
    let b = (r1 - r2) / h;
    let a = (1.0 - b * b).sqrt();
    let k = -b * px + a * py;
    if k < 0.0 {
        px.hypot(py) - r1
    } else if k > a * h {
        px.hypot(py - h) - r2
    } else {
        a * px + b * py - r1
    }
}

/// Kreisbogen von Winkel `a0` bis `a1` (im Uhrzeigersinn, Bildschirm), runde Enden.
fn sd_arc(x: f32, y: f32, c: (f32, f32), r: f32, a0: f32, a1: f32, thick: f32) -> f32 {
    use std::f32::consts::TAU;
    let span = (a1 - a0).rem_euclid(TAU);
    let rel = ((y - c.1).atan2(x - c.0) - a0).rem_euclid(TAU);
    if rel <= span {
        ((x - c.0).hypot(y - c.1) - r).abs() - thick / 2.0
    } else {
        let end = |a: f32| (x - c.0 - r * a.cos()).hypot(y - c.1 - r * a.sin()) - thick / 2.0;
        end(a0).min(end(a1))
    }
}

fn sd_triangle(x: f32, y: f32, p: [(f32, f32); 3]) -> f32 {
    let e = [(p[1].0 - p[0].0, p[1].1 - p[0].1), (p[2].0 - p[1].0, p[2].1 - p[1].1), (p[0].0 - p[2].0, p[0].1 - p[2].1)];
    let s = (e[0].0 * e[2].1 - e[0].1 * e[2].0).signum();
    let (mut dist, mut side) = (f32::MAX, f32::MAX);
    for i in 0..3 {
        let v = (x - p[i].0, y - p[i].1);
        let t = ((v.0 * e[i].0 + v.1 * e[i].1) / (e[i].0 * e[i].0 + e[i].1 * e[i].1)).clamp(0.0, 1.0);
        let q = (v.0 - e[i].0 * t, v.1 - e[i].1 * t);
        dist = dist.min(q.0 * q.0 + q.1 * q.1);
        side = side.min(s * (v.0 * e[i].1 - v.1 * e[i].0));
    }
    -dist.sqrt() * side.signum()
}

fn logo(size: u32, detail: bool, full_bleed: bool) -> Vec<u8> {
    let s = size as f32;
    let px = 1.0 / s;
    let (bg_top, bg_bottom, beam, rim_col) = (hex(0x1747b8), hex(0x071538), hex(0x3fd4ff), hex(0x5aa8ff));
    let (drop_top, drop_bottom, violet, glow_col, white) = (hex(0x5ff4ff), hex(0x2f7bff), hex(0x8a5cff), hex(0x29d9ff), [1.0; 3]);
    let margin = if full_bleed || size <= 32 { 0.0 } else { 0.03 };
    let (c, r1, r2, h) = if detail { ((0.5, 0.6), 0.255, 0.03, 0.43) } else { ((0.5, 0.6), 0.3, 0.04, 0.5) };
    // Sync-Pfeile: zwei Bögen mit Spitze, je 130°.
    let arrow_c = (c.0, c.1 + 0.005);
    let (arrow_r, arrow_t) = (0.125, 0.045);
    let arrows: Vec<(f32, f32, [(f32, f32); 3])> = [0.0f32, std::f32::consts::PI]
        .iter()
        .map(|base| {
            let (a0, a1) = (base + 200f32.to_radians(), base + 330f32.to_radians());
            let e = (arrow_c.0 + arrow_r * a1.cos(), arrow_c.1 + arrow_r * a1.sin());
            let (t, n) = ((-a1.sin(), a1.cos()), (a1.cos(), a1.sin()));
            let (len, w) = (0.0675, 0.062);
            (a0, a1, [(e.0 + t.0 * len, e.1 + t.1 * len), (e.0 + n.0 * w, e.1 + n.1 * w), (e.0 - n.0 * w, e.1 - n.1 * w)])
        })
        .collect();

    let mut out = vec![0u8; (size * size * 4) as usize];
    for iy in 0..size {
        for ix in 0..size {
            let (x, y) = ((ix as f32 + 0.5) / s, (iy as f32 + 0.5) / s);
            // Kachel: Verlauf von oben links nach unten rechts, Lichtschein oben rechts.
            let tile = sd_round_rect(x, y, (0.5, 0.5), (0.5 - margin, 0.5 - margin), (0.5 - margin) * 0.44);
            let tile_a = if full_bleed { 1.0 } else { cover(tile, px) };
            let mut col = mix(bg_top, bg_bottom, ((x * 0.35 + y) / 1.35).clamp(0.0, 1.0));
            let glow = (-((x - 0.95).powi(2) + (y - 0.02).powi(2)) / 0.09).exp();
            col = mix(col, beam, glow * 0.55);
            if detail && !full_bleed {
                col = mix(col, rim_col, cover((tile + 0.012).abs() - 0.006, px) * 0.4);
            }
            // Tropfen mit Schein, Verlauf Cyan -> Blau, unten links ins Violette.
            let d = sd_drop(x, y, c, r1, r2, h);
            if detail && d > 0.0 {
                col = mix(col, glow_col, (-d / 0.05).exp() * 0.35);
            }
            let dt = ((y - (c.1 - h)) / (h + r1)).clamp(0.0, 1.0);
            let dx = ((x - (c.0 - r1)) / (2.0 * r1)).clamp(0.0, 1.0);
            let drop_col = mix(mix(drop_top, drop_bottom, dt), violet, dt * (1.0 - dx) * 0.55);
            let drop_a = cover(d, px);
            col = mix(col, drop_col, drop_a);
            if detail {
                let mut w = 0.0f32;
                for (a0, a1, tri) in &arrows {
                    w = w.max(cover(sd_arc(x, y, arrow_c, arrow_r, *a0, *a1, arrow_t), px));
                    w = w.max(cover(sd_triangle(x, y, *tri), px));
                }
                col = mix(col, white, w * drop_a);
            } else {
                let gloss = sd_drop(x, y, (c.0 - 0.085, c.1 - 0.05), 0.085, 0.012, 0.22);
                col = mix(col, white, cover(gloss, px) * drop_a * 0.7);
            }
            let i = ((iy * size + ix) * 4) as usize;
            for k in 0..3 {
                out[i + k] = (col[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[i + 3] = (tile_a.max(drop_a) * 255.0 + 0.5) as u8;
        }
    }
    out
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


/// Minimaler PNG-Encoder (unkomprimiert), für Symbole unter macOS.
pub fn png_bytes(rgba: &[u8], size: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    // Rohdaten: je Zeile Filterbyte 0 + RGBA
    let mut raw = Vec::with_capacity(((size * 4 + 1) * size) as usize);
    for y in 0..size as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * size as usize * 4..(y + 1) * size as usize * 4]);
    }
    // zlib mit „stored“-Blöcken
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Schreibt ein macOS-Iconset (für `iconutil`).
pub fn export_iconset(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for base in [16u32, 32, 128, 256, 512] {
        std::fs::write(dir.join(format!("icon_{base}x{base}.png")), png_bytes(&logo_rgba(base), base))?;
        let d = base * 2;
        std::fs::write(dir.join(format!("icon_{base}x{base}@2x.png")), png_bytes(&logo_rgba(d), d))?;
    }
    Ok(())
}
