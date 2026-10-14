//! Dekód-teljesítmény mérés: AVFoundation/VideoToolbox → CVPixelBuffer.
//!
//! Használat: decode_bench <fájl> [ismétlés]

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| ".testmedia/small.mov".to_string());
    let repeats: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(1);
    let fmt_name = args.next().unwrap_or_else(|| "bgra".to_string());
    let pixel_format = match fmt_name.as_str() {
        "420v" | "native" => dada_media::macos::reader::PIXEL_FORMAT_420V,
        _ => dada_media::macos::reader::PIXEL_FORMAT_BGRA,
    };

    println!("Dekód-bench: {path} ({repeats}x, kért formátum: {fmt_name})");

    for run in 0..repeats {
        let mut reader = dada_media::VideoReader::open_with_pixel_format(
            std::path::Path::new(&path),
            pixel_format,
        )?;
        let first = reader.next_frame()?;
        let (w, h, fmt, fps_nominal) = match &first {
            Some(f) => (f.width, f.height, f.pixel_format, reader.nominal_fps),
            None => (reader.width(), reader.height(), 0, reader.nominal_fps),
        };
        println!(
            "  futás {}: {}x{} formátum={} névleges={:.2} fps",
            run + 1,
            w,
            h,
            dada_media::fourcc(fmt),
            fps_nominal
        );

        let mut count: u64 = if first.is_some() { 1 } else { 0 };
        let start = Instant::now();
        let mut last_pts = 0.0f64;
        while let Some(f) = reader.next_frame()? {
            last_pts = f.pts_secs;
            std::hint::black_box(&f);
            count += 1;
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "    {} frame, {:.2} s → {:.1} fps dekód | videó-idő: {:.2} s (valós idejű tényező: {:.1}x)",
            count,
            elapsed,
            count as f64 / elapsed,
            last_pts,
            last_pts / elapsed
        );
    }
    Ok(())
}
