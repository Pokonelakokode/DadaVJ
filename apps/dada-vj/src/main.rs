//! Dada VJ — belépési pont.
//!
//! Phase 0 / Spike A: wgpu (Metal) ablak, procedurális shader, frame-idő mérés,
//! valamint headless (ablak nélküli) nyers GPU-benchmark.

mod app;
mod audio_vis;
mod bench;
mod project;
mod hud;
mod renderer;
mod video;
mod web;

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
    /// ISF shader (generátor/effekt) betöltése és renderelése.
    pub shader: Option<String>,
    /// Automatikus shader-validáció (offscreen render + pixel-visszaolvasás).
    pub shader_check: bool,
    /// MIDI-bemenetek megnyitása.
    pub midi: bool,
    /// MIDI note -> envelope -> shader paraméter (a kickre reagáló effekt).
    pub midi_visual: bool,
    /// Automatikus MIDI-validáció (szintetikus clock + note stream, headless).
    pub midi_check: bool,
    /// Videó-mappák mint clip-bankok: --deck MAPPA[:UTEM[:BLEND]]
    /// Többször megadva több deck (layer) lesz.
    pub decks: Vec<String>,
    /// Az autopilot váltás hossza ütemekben.
    pub autopilot_beats: f64,
    /// Modulációs útvonalak: FORRAS->CEL[:mennyiseg[:decay_ms]]
    pub mods: Vec<String>,
    /// Projektfájl betöltése (TOML).
    pub project: Option<String>,
    /// Az aktuális felállás mentése projektfájlba, majd kilépés.
    pub save: Option<String>,
    /// Explicit jelzők: a projekt csak azt tölti ki, amit a CLI nem adott meg.
    pub size_set: bool,
    pub vsync_set: bool,
    pub port_set: bool,
    /// Automatikus modulációs validáció (headless).
    pub mod_check: bool,
    /// Felvétel fájlba (MP4, H.264).
    pub record: Option<String>,
    /// Felvétel-validáció (headless): írás, majd visszaolvasás.
    pub record_check: bool,
    /// Egy videófájl vizsgálata (headless): méret, frame-szám, hossz.
    pub probe: Option<String>,
    /// RTMP stream cél-URL (pl. rtmp://a.rtmp.youtube.com/live2/KULCS).
    pub rtmp: Option<String>,
    /// Ableton Link szinkron bekapcsolása.
    pub link: bool,
    /// Link-állapot kiírása (headless diagnosztika).
    pub link_status: bool,
    /// Thumbnail készítése egy videóból (headless ellenőrzés).
    pub thumb_check: Option<String>,
    /// AAC encoder-ellenőrzés (headless).
    pub aac_check: bool,
    /// Az encoder önálló validációja (headless).
    pub encode_check: bool,
    /// Beépített web-szerver (vezérlő UI + MJPEG előnézet).
    pub serve: bool,
    /// A web-szerver portja.
    pub port: u16,
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
            shader: None,
            shader_check: false,
            midi: false,
            midi_visual: false,
            midi_check: false,
            decks: Vec::new(),
            autopilot_beats: 16.0,
            mods: Vec::new(),
            project: None,
            save: None,
            size_set: false,
            vsync_set: false,
            port_set: false,
            mod_check: false,
            record: None,
            record_check: false,
            probe: None,
            rtmp: None,
            link: false,
            link_status: false,
            thumb_check: None,
            aac_check: false,
            encode_check: false,
            serve: false,
            port: 7777,
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
                    args.size_set = true;
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
                "--shader" => args.shader = it.next(),
                "--shader-check" => args.shader_check = true,
                "--midi" => args.midi = true,
                "--midi-visual" => {
                    args.midi = true;
                    args.midi_visual = true;
                }
                "--midi-check" => args.midi_check = true,
                "--deck" => {
                    if let Some(v) = it.next() {
                        args.decks.push(v);
                    }
                }
                "--autopilot" => {
                    let v = it.next().unwrap_or_else(|| "16".into());
                    args.autopilot_beats = v.parse()?;
                }
                "--mod" => {
                    if let Some(v) = it.next() {
                        args.mods.push(v);
                    }
                }
                "--mod-check" => args.mod_check = true,
                "--record" => args.record = it.next(),
                "--record-check" => args.record_check = true,
                "--probe" => args.probe = it.next(),
                "--rtmp" => args.rtmp = it.next(),
                "--link" => args.link = true,
                "--link-status" => args.link_status = true,
                "--thumb-check" => args.thumb_check = it.next(),
                "--aac-check" => args.aac_check = true,
                "--encode-check" => args.encode_check = true,
                "--project" => args.project = it.next(),
                "--save" => args.save = it.next(),
                "--serve" => args.serve = true,
                "--port" => {
                    let v = it.next().unwrap_or_else(|| "7777".into());
                    args.port = v.parse()?;
                    args.serve = true;
                    args.port_set = true;
                }
                "--no-vsync" => {
                    args.vsync = false;
                    args.vsync_set = true;
                }
                "--fullscreen" => args.fullscreen = true,
                "-h" | "--help" => {
                    println!(
                        "dada-vj {}\n\n  --size 1920x1080    ablak/render felbontás\n  --bench 10          N másodperc után kilép és összegez (ablakos)\n  --raw-bench 300     headless nyers GPU-benchmark N frame-re\n  --layers N          N teljes képernyős draw / frame (layer stressz)\n  --video FAJL        videó lejátszása a videó-rétegben (zero-copy)\n  --audio-visual      audio-vizuál réteg (spektrogram + hullámforma + spektrum)\n  --audio ESZKÖZ      valódi audio-bemenet (alapból: szintetikus forrás)\n  --audio-check       automatikus audio-validáció (headless)\n  --shader FAJL.isf   ISF shader betöltése és renderelése (hot-reload)\n  --shader-check      automatikus shader-validáció (headless)\n  --midi              MIDI-bemenetek megnyitása (több forrás egyszerre)\n  --midi-visual       MIDI note -> envelope -> shader paraméter\n  --midi-check        automatikus MIDI-validáció (headless)\n  --deck MAPPA[:UTEM[:BLEND]]  videó-mappa mint clip-bank (többször ismételhető)\n  --autopilot N       autopilot váltás N ütem után (alap: 16)\n  --mod FORRAS->CEL[:MENNY[:DECAY]]  modulációs útvonal (többször ismételhető)\n      források: note:N, cc:N, audio.sub|bass|low_mid|mid|high_mid|high|rms|onset, lfo:UTEM, clock.phase\n      célok:    deck.opacity.N, deck.next.N, shader.param.N, audio.opacity, master.opacity\n  --mod-check         automatikus modulációs validáció (headless)\n  --rtmp URL          RTMP stream (pl. rtmp://a.rtmp.youtube.com/live2/KULCS)\n  --link              Ableton Link szinkron bekapcsolása\n  --link-status       Link-állapot kiírása (headless)\n  --encode-check      az encoder önálló validációja (headless)\n  --project FAJL.toml projekt betöltése\n  --save FAJL.toml    a felállás mentése és kilépés\n  --serve             webes vezérlő UI + MJPEG előnézet (127.0.0.1:7777)\n  --port N            a web-szerver portja\n  --no-vsync          vsync kikapcsolása\n  --fullscreen        teljes képernyő",
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

    // Projekt mentése és kilépés.
    if let Some(path) = args.save.clone() {
        let loaded = match &args.project {
            Some(p) => project::Project::load(std::path::Path::new(p)).ok(),
            None => None,
        };
        let p = project::Project::from_cli(&args, loaded.as_ref());
        p.save(std::path::Path::new(&path))?;
        println!("projekt mentve: {path}");
        println!("{}", p.to_toml()?);
        return Ok(());
    }

    // Egy videófájl vizsgálata.
    if let Some(path) = args.probe.clone() {
        return pollster::block_on(bench::probe(&path));
    }

    // AAC encoder-ellenőrzés.
    if args.aac_check {
        return pollster::block_on(bench::aac_check(&args));
    }

    // Thumbnail-ellenőrzés.
    if let Some(path) = args.thumb_check.clone() {
        let out = format!("{path}.thumb.jpg");
        let t0 = std::time::Instant::now();
        let jpeg = dada_media::macos::thumb::video_thumbnail(
            std::path::Path::new(&path),
            320,
            180,
            80,
        )?;
        std::fs::write(&out, &jpeg)?;
        println!(
            "THUMB — {} -> {} ({} bájt, {:.0} ms)",
            path,
            out,
            jpeg.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        return Ok(());
    }

    // Link-állapot (headless).
    if args.link_status {
        let listener = dada_link::LinkListener::new()?;
        println!("LINK — port {}, 6 másodpercig figyelek...", dada_link::LINK_PORT);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(300));
            if let Some(tl) = listener.timeline() {
                let now = dada_link::now_link_us();
                println!(
                    "  csomagok: {} · BPM {:.2} · beat {:.2} · hiteles: {}",
                    listener.packets(),
                    tl.bpm(),
                    tl.beat_at(now),
                    tl.plausible_at(now)
                );
            }
        }
        if listener.packets() == 0 {
            println!("  nincs Link peer (indítsd el az Ableton Live-ot, vagy egy másik Link klienst)");
        }
        return Ok(());
    }

    // Headless encoder-validáció.
    if args.encode_check {
        return pollster::block_on(bench::encode_check(&args));
    }

    // Headless felvétel-validáció.
    if args.record_check {
        return pollster::block_on(bench::record_check(&args));
    }

    // Headless moduláció-validáció.
    if args.mod_check {
        return pollster::block_on(bench::mod_check(&args));
    }

    // Headless MIDI-validáció.
    if args.midi_check {
        return pollster::block_on(bench::midi_check(&args));
    }

    // Headless shader-validáció.
    if args.shader_check {
        return pollster::block_on(bench::shader_check(&args));
    }

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
