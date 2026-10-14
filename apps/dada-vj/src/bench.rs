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
use crate::Args;
use anyhow::Result;
use std::time::Instant;

const BATCH: u32 = 600;
const WARMUP: u32 = 120;

fn batch(renderer: &mut Renderer, frames: u32, layers: u32) -> f64 {
    let start = Instant::now();
    for i in 0..frames {
        let t = i as f32 * 0.016;
        let _ = renderer.render(t, t * 2.0, layers);
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
