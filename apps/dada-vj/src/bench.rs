//! Headless teljesítmény-harness.
//!
//! Ablak és vsync nélkül méri a **frame valós költségét** (CPU-beküldés +
//! GPU-végrehajtás) 1080p-n, N alpha-blendelt teljes képernyős réteggel.
//!
//! Módszer: nagy batcheket adunk be, egyetlen szinkronizálással a végén, így a
//! GPU-sor telítődik. A rétegek költségét a *clear-only* batch és az N-layer
//! batch különbsége adja — így a fix overhead (present, szinkron) kiesik.
//!
//! GPU timestamp query-t is próbálunk, de a wgpu 24 Metal backend Apple GPU-n
//! ugyanarra a pontra mintavételezi a két mintát (0 ms), ezért az csak
//! diagnosztika: a mérvadó szám a batch-különbség.

use crate::renderer::Renderer;
use crate::video::VideoLayer;
use crate::Args;
use anyhow::{bail, Result};
use std::path::Path;
use std::time::{Duration, Instant};

/// Automatikus validáció: a shader tényleg a dekódolt videót mintavételezi-e?
///
/// Offscreen renderel, és a kép közepéről olvas vissza pixeleket több
/// időpontban. Ha a színek változnak (és nem feketék), a lánc él.
pub async fn video_check(args: &Args) -> Result<()> {
    let Some(path) = &args.video else {
        bail!("a --video-check-hez kell --video <fájl>");
    };
    let mut renderer = Renderer::new_offscreen(1920, 1080, wgpu::TextureFormat::Bgra8UnormSrgb).await?;
    let mut video = VideoLayer::open(renderer.device(), Path::new(path), 1.0)?;
    println!(
        "VIDEO-CHECK — {path} ({}x{}, {:.2} fps névleges)",
        video.size.0,
        video.size.1,
        video.nominal_fps()
    );

    let start = Instant::now();
    let mut samples: Vec<(u32, [u8; 3])> = Vec::new();
    let frames = 240;
    for i in 0..frames {
        // Szimulált 60 fps-es óra, hogy a videó valóban előre haladjon.
        let now = start + Duration::from_secs_f64(i as f64 / 60.0);
        let view = video.advance(renderer.device(), now)?;
        renderer.render(0.0, 0.0, 1, view)?;
        if i % 24 == 0 {
            if let Some(px) = renderer.read_back_center(4, 4) {
                let mut sum = [0u32; 3];
                let mut n = 0u32;
                for chunk in px.chunks_exact(4) {
                    // BGRA
                    sum[0] += chunk[2] as u32;
                    sum[1] += chunk[1] as u32;
                    sum[2] += chunk[0] as u32;
                    n += 1;
                }
                if n > 0 {
                    samples.push((
                        i,
                        [
                            (sum[0] / n) as u8,
                            (sum[1] / n) as u8,
                            (sum[2] / n) as u8,
                        ],
                    ));
                }
            }
        }
    }
    renderer.wait_idle();
    println!(
        "  dekódolt: {} frame · megjelenített: {} · utolsó PTS: {:.2} s",
        video.frames_decoded, video.frames_shown, video.last_pts
    );
    println!("  középső pixel (RGB) a kimeneten:");
    for (i, c) in &samples {
        println!("    frame {i:>3}: R={:>3} G={:>3} B={:>3}", c[0], c[1], c[2]);
    }
    let distinct = {
        let mut set = std::collections::BTreeSet::new();
        for (_, c) in &samples {
            set.insert(*c);
        }
        set.len()
    };
    println!("  különböző színminta: {distinct}");
    if distinct < 2 {
        bail!("a kimenet nem változik — a videó nem jut el a shaderig");
    }
    println!("  OK: a videó a shaderen keresztül jelenik meg.");
    Ok(())
}

const BATCH: u32 = 600;
const WARMUP: u32 = 120;

fn batch(renderer: &mut Renderer, frames: u32, layers: u32) -> f64 {
    let start = Instant::now();
    for i in 0..frames {
        let t = i as f32 * 0.016;
        let _ = renderer.render(t, t * 2.0, layers, None);
    }
    renderer.wait_idle();
    start.elapsed().as_secs_f64()
}

pub async fn run(args: &Args) -> Result<()> {
    let layers = args.layers;
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;

    let mut renderer = Renderer::new_offscreen(args.width, args.height, format).await?;
    println!(
        "NYERS BENCH — {}x{} · GPU: {}",
        args.width,
        args.height,
        renderer.adapter_name()
    );

    // Bemelegítés (shader-fordítás, textúrapool, órajel-felcsavarás).
    let _ = batch(&mut renderer, WARMUP, layers.max(1));

    // A clear-only referenciat a meres elott es utan is merjuk, a kisebbet vesszuk.
    let clear_before = batch(&mut renderer, BATCH, 0) * 1000.0 / BATCH as f64;
    println!(
        "  clear-only:  {clear_before:.3} ms/frame  ({:.0} fps)",
        1000.0 / clear_before
    );
    if layers == 0 {
        return Ok(());
    }

    let n_s = batch(&mut renderer, BATCH, layers);
    let clear_after = batch(&mut renderer, BATCH, 0) * 1000.0 / BATCH as f64;
    let clear_ms = clear_before.min(clear_after);
    let n_ms = n_s * 1000.0 / BATCH as f64;
    let per_layer = (n_ms - clear_ms) / layers as f64;
    let budget = 1000.0 / 60.0;

    println!("  {layers} layer:     {n_ms:.3} ms/frame  ({:.0} fps)", 1000.0 / n_ms);
    println!("  rétegenként: {per_layer:.3} ms");
    println!(
        "  60 Hz büdzsé: {:.1}% ({n_ms:.2} / {budget:.2} ms)",
        100.0 * n_ms / budget
    );
    println!(
        "  ~{} ilyen réteg férne bele 60 fps-nél",
        (budget / per_layer).floor()
    );
    Ok(())
}
