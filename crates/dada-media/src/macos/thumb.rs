//! Klipek előnézeti képe (thumbnail): az első frame lekicsinyítve, JPEG-ben.

use anyhow::{bail, Context, Result};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use std::path::Path;

use crate::macos::reader::VideoReader;

/// Egy videó első frame-jéből készített előnézet (JPEG bájtok).
///
/// CPU-oldali út: a CVPixelBuffert csak olvasásra (ReadOnly) zároljuk, és
/// közvetlenül mintavételezzük a célfelbontásra (teljes képkocka-másolás nélkül),
/// majd RGB JPEG-be kódoljuk.
pub fn video_thumbnail(path: &Path, out_w: u32, out_h: u32, quality: u8) -> Result<Vec<u8>> {
    let mut reader = VideoReader::open(path)?;
    let frame = reader
        .next_frame()?
        .with_context(|| format!("nincs frame: {}", path.display()))?;
    let (src_w, src_h) = (frame.width, frame.height);
    if src_w == 0 || src_h == 0 {
        bail!("üres videó: {}", path.display());
    }

    let out_w = out_w.max(1);
    let out_h = out_h.max(1);
    let mut rgb = vec![0u8; (out_w * out_h * 3) as usize];

    unsafe {
        let pb = &frame.pixel_buffer;
        CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
        let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
        let bytes_per_row = CVPixelBufferGetBytesPerRow(pb);
        let buf_h = CVPixelBufferGetHeight(pb);
        if base.is_null() || bytes_per_row == 0 || buf_h == 0 {
            CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
            bail!("a pixel buffer nem olvasható: {}", path.display());
        }
        let total_len = bytes_per_row * buf_h;
        let slice = std::slice::from_raw_parts(base, total_len);
        let eff_h = (src_h as usize).min(buf_h).max(1);
        let eff_w = (src_w as usize).min(bytes_per_row / 4).max(1);

        for oy in 0..out_h as usize {
            let y0 = oy * eff_h / out_h as usize;
            let y1 = ((oy + 1) * eff_h / out_h as usize)
                .saturating_sub(1)
                .max(y0)
                .min(eff_h - 1);
            let ym = (y0 + y1) / 2;
            for ox in 0..out_w as usize {
                let x0 = ox * eff_w / out_w as usize;
                let x1 = ((ox + 1) * eff_w / out_w as usize)
                    .saturating_sub(1)
                    .max(x0)
                    .min(eff_w - 1);
                let xm = (x0 + x1) / 2;

                let samples = [(x0, y0), (xm, y0), (x0, ym), (xm, ym)];
                let (mut b, mut g, mut r) = (0u32, 0u32, 0u32);
                for (sx, sy) in samples {
                    let idx = sy * bytes_per_row + sx * 4;
                    b += slice[idx] as u32;
                    g += slice[idx + 1] as u32;
                    r += slice[idx + 2] as u32;
                }
                let o = (oy * out_w as usize + ox) * 3;
                rgb[o] = (r / 4) as u8;
                rgb[o + 1] = (g / 4) as u8;
                rgb[o + 2] = (b / 4) as u8;
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
    }

    let mut jpeg = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut jpeg, quality);
    enc.encode(
        &rgb,
        out_w as u16,
        out_h as u16,
        jpeg_encoder::ColorType::Rgb,
    )
    .context("JPEG kódolás")?;
    Ok(jpeg)
}
