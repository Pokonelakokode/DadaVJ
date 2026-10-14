//! Dada VJ — belépési pont.
//!
//! Phase 0 / Spike A: wgpu (Metal) ablak, procedurális shader, frame-idő mérés,
//! valamint headless (ablak nélküli) nyers GPU-benchmark.

mod app;
mod audio_vis;
mod bench;
mod hud;
mod renderer;
mod video;

use anyhow::Result;
use winit::event_loop::{ControlFlow, EventLoop};

#[derive(Debug, Clone)]
pub struct Args {
    pub width: u32,
    pub height: u32,
    /// Ha meg van adva, a program N másodperc után kilép és összegzést ír.
    pub bench_secs: Option<f64>,
    pub vsync: bool,
    pub fullscreen: bool,
    /// Hány teljes képernyős draw hívás / frame (overdraw stressz a layer-büdzséhez).
    pub layers: u32,
    /// Headless nyers benchmark: ennyi frame-et mér ablak és vsync nélkül.
    pub raw_bench: Option<u32>,
    /// Videófájl, amit a videó-rétegben lejátszunk (zero-copy GPU úton).
    pub video: Option<String>,
    /// Automatikus videó-validáció (offscreen render + pixel-visszaolvasás).
    pub video_check: bool,
    /// Audio-vizuál réteg bekapcsolása (spektrogram + hullámforma + spektrum).
    pub audio_visual: bool,
    /// Valódi audio-bemenet (ha nincs megadva, szintetikus forrás fut).
    pub audio_device: Option<String>,
    /// Automatikus audio-validáció (szintetikus forrás + pixel-visszaolvasás).
    pub audio_check: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            bench_secs: None,
            vsync: true,
            fullscreen: false,
            layers: 1,
            raw_bench: None,
            video: None,
            video_check: false,
            audio_visual: false,
            audio_device: None,
            audio_check: false,
        }
    }
}

impl Args {
    pub fn parse() -> Result<Self> {
        let mut args = Args::default();
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--size" => {
                    let v = it.next().unwrap_or_default();
                    let (w, h) = v
                        .split_once(['x', 'X'])
                        .ok_or_else(|| anyhow::anyhow!("--size formátuma: 1920x1080"))?;
                    args.width = w.parse()?;
                    args.height = h.parse()?;
                }
                "--bench" => {
                    let v = it.next().unwrap_or_else(|| "10".into());
                    args.bench_secs = Some(v.trim_end_matches('s').parse()?);
                }
                "--raw-bench" => {
                    let v = it.next().unwrap_or_else(|| "300".into());
                    args.raw_bench = Some(v.parse()?);
                }
                "--layers" => {
                    let v = it.next().unwrap_or_else(|| "1".into());
                    args.layers = v.parse()?;
                }
                "--video" => args.video = it.next(),
                "--video-check" => args.video_check = true,
                "--audio-visual" => args.audio_visual = true,
                "--audio" => args.audio_device = it.next(),
                "--audio-check" => args.audio_check = true,
                "--no-vsync" => args.vsync = false,
                "--fullscreen" => args.fullscreen = true,
                "-h" | "--help" => {
                    println!(
                        "dada-vj {}\n\n  --size 1920x1080    ablak/render felbontás\n  --bench 10          N másodperc után kilép és összegez (ablakos)\n  --raw-bench 300     headless nyers GPU-benchmark N frame-re\n  --layers N          N teljes képernyős draw / frame (layer stressz)\n  --video FAJL        videó lejátszása a videó-rétegben (zero-copy)\n  --audio-visual      audio-vizuál réteg (spektrogram + hullámforma + spektrum)\n  --audio ESZKÖZ      valódi audio-bemenet (alapból: szintetikus forrás)\n  --audio-check       automatikus audio-validáció (headless)\n  --no-vsync          vsync kikapcsolása\n  --fullscreen        teljes képernyő",
                        env!("CARGO_PKG_VERSION")
                    );
                    std::process::exit(0);
                }
                other => eprintln!("ismeretlen kapcsoló: {other} (--help)"),
            }
        }
        Ok(args)
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse()?;

    // Headless audio-validáció.
    if args.audio_check {
        return pollster::block_on(bench::audio_check(&args));
    }

    // Headless videó-validáció.
    if args.video_check {
        return pollster::block_on(bench::video_check(&args));
    }

    // Headless mód: nem kell ablak, nem kell WindowServer.
    if args.raw_bench.is_some() {
        return pollster::block_on(bench::run(&args));
    }

    log::info!(
        "Dada VJ {} — {}x{} vsync={} layers={} bench={:?}",
        env!("CARGO_PKG_VERSION"),
        args.width,
        args.height,
        args.vsync,
        args.layers,
        args.bench_secs
    );

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut application = app::App::new(args);
    event_loop.run_app(&mut application)?;
    Ok(())
}
