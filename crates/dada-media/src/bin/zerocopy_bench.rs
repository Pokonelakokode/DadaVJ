//! Zero-copy igazoló benchmark.
//!
//! Végigmegy egy videón: dekódol (VideoToolbox) → CVMetalTexture → wgpu textúra,
//! és méri a lépések költségét. Néhány frame-nél visszaolvassa a képpontokat,
//! hogy bizonyíthatóan valódi kép kerül a GPU-textúrába.

use anyhow::Result;
use std::collections::VecDeque;
use std::time::Instant;

use dada_media::macos::metal::MetalTextureCache;
use dada_media::macos::reader::PIXEL_FORMAT_BGRA;
use dada_media::VideoReader;

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| ".testmedia/hero.mp4".to_string());
    let frames: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(600);

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("nem találtam Metal adaptert"))?;
    let info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("zerocopy-bench"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        )
        .await?;

    let cache = MetalTextureCache::new(&device)?;
    let mut reader = VideoReader::open_with_pixel_format(std::path::Path::new(&path), PIXEL_FORMAT_BGRA)?;

    println!("ZERO-COPY BENCH — {path}");
    println!("  GPU: {} ({:?})", info.name, info.backend);

    // GPU-biztonság: a legutóbbi frame-ek textúráit életben tartjuk, amíg a GPU
    // be nem fejezi a használatukat.
    let mut alive: VecDeque<dada_media::WrappedFrame> = VecDeque::with_capacity(8);

    let start = Instant::now();
    let mut count: u32 = 0;
    let mut wrap_ns: u128 = 0;
    let mut first_dims = (0u32, 0u32);
    let mut sample_pixels: Option<Vec<u8>> = None;

    while count < frames {
        let Some(frame) = reader.next_frame()? else {
            break;
        };
        if count == 0 {
            first_dims = (frame.width, frame.height);
        }

        let t0 = Instant::now();
        let wrapped =
            cache.wrap_bgra(&device, &frame.pixel_buffer, frame.width, frame.height)?;
        wrap_ns += t0.elapsed().as_nanos();

        if count == 10 {
            sample_pixels =
                read_back_pixels(&device, &queue, &wrapped.texture, frame.width, frame.height);
        }

        alive.push_back(wrapped);
        while alive.len() > 6 {
            alive.pop_front();
        }
        count += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();

    println!(
        "  {} frame ({}x{}), {:.2} s → {:.1} fps (dekód + zero-copy wrap)",
        count,
        first_dims.0,
        first_dims.1,
        elapsed,
        count as f64 / elapsed
    );
    println!(
        "  wrap költsége: {:.1} µs/frame",
        wrap_ns as f64 / count.max(1) as f64 / 1000.0
    );

    match sample_pixels {
        Some(px) if px.len() >= 16 => {
            println!("  pixel-minta (BGRA, a 10. frame bal felső sarkából):");
            for i in 0..4 {
                let o = i * 4;
                println!(
                    "    px{}: B={} G={} R={} A={}",
                    i, px[o], px[o + 1], px[o + 2], px[o + 3]
                );
            }
            let nonzero = px.iter().filter(|b| **b != 0).count();
            println!("    nem-nulla bájtok: {nonzero}/{}", px.len());
        }
        Some(_) => println!("  pixel-visszaolvasás: túl rövid"),
        None => println!("  pixel-visszaolvasás: nem sikerült"),
    }

    Ok(())
}

/// Néhány képpont visszaolvasása a textúrából (bizonyíték arra, hogy valódi kép van benne).
fn read_back_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    const ROWS: u32 = 4;
    const COLS: u32 = 4;
    let bytes_per_row = ((COLS * 4 + 255) / 256) * 256;
    let size = (bytes_per_row * ROWS) as u64;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(ROWS),
            },
        },
        wgpu::Extent3d {
            width: COLS.min(width),
            height: ROWS.min(height),
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Some(data)
}
