//! Klipek előnézeti képe (thumbnail): az első frame lekicsinyítve, JPEG-ben.

use anyhow::{Context, Result};
use std::path::Path;

use crate::macos::reader::VideoReader;
use crate::macos::writer::VideoWriter;

/// Egy videó első frame-jéből készített előnézet (JPEG bájtok).
///
/// CPU-oldali út: a CVPixelBufferből kiolvassuk a BGRA sorokat, box-szűrővel
/// kicsinyítünk, közben RGB-re váltunk, és JPEG-be kódoljuk. Ez egyszer fut
/// klippenként, ezért nem kell GPU-út.
pub fn video_thumbnail(path: &Path, out_w: u32, out_h: u32, quality: u8) -> Result<Vec<u8>> {
    let mut reader = VideoReader::open(path)?;
    let frame = reader
        .next_frame()?
        .with_context(|| format!("nincs frame: {}", path.display()))?;
    let (src_w, src_h) = (frame.width, frame.height);
    if src_w == 0 || src_h == 0 {
        anyhow::bail!("üres videó: {}", path.display());
    }

    let stride = (src_w * 4) as usize;
    let mut bgra = vec![0u8; stride * src_h as usize];
    VideoWriter::fill(&frame.pixel_buffer, |row, y| {
        let start = y * stride;
        let n = row.len().min(stride);
        if start + n <= bgra.len() {
            bgra[start..start + n].copy_from_slice(&row[..n]);
        }
    })?;

    let out_w = out_w.max(1);
    let out_h = out_h.max(1);
    let mut rgb = vec![0u8; (out_w * out_h * 3) as usize];
    for oy in 0..out_h {
        let y0 = oy * src_h / out_h;
        let y1 = ((oy + 1) * src_h / out_h).max(y0 + 1).min(src_h);
        for ox in 0..out_w {
            let x0 = ox * src_w / out_w;
            let x1 = ((ox + 1) * src_w / out_w).max(x0 + 1).min(src_w);
            let (mut b, mut g, mut r, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * src_w + x) as usize * 4;
                    if i + 2 < bgra.len() {
                        b += bgra[i] as u32;
                        g += bgra[i + 1] as u32;
                        r += bgra[i + 2] as u32;
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            let o = ((oy * out_w + ox) * 3) as usize;
            rgb[o] = (r / n) as u8;
            rgb[o + 1] = (g / n) as u8;
            rgb[o + 2] = (b / n) as u8;
        }
    }

    let mut jpeg = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut jpeg, quality);
    enc.encode(&rgb, out_w as u16, out_h as u16, jpeg_encoder::ColorType::Rgb)
        .context("JPEG kódolás")?;
    Ok(jpeg)
}
