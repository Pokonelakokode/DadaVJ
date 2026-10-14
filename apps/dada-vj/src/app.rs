//! Az alkalmazás életciklusa és a frame-ciklus.

use crate::audio_vis::AudioVisual;
use crate::hud::FrameStats;
use crate::renderer::{LayerRef, Renderer};
use crate::video::VideoLayer;
use crate::Args;
use dada_audio::AudioEngine;
use dada_fx::Effect;
use dada_midi::{EnvelopeSpec, Mapping, MidiEngine, Trigger};
use std::time::{Duration, SystemTime};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

pub struct App {
    args: Args,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    video: Option<VideoLayer>,
    audio: Option<(AudioEngine, AudioVisual)>,
    shader: Option<Effect>,
    midi: Option<MidiEngine>,
    shader_mtime: Option<SystemTime>,
    last_shader_check: Instant,
    stats: FrameStats,
    started: Instant,
    last_frame: Instant,
    summary_printed: bool,
}

impl App {
    pub fn new(args: Args) -> Self {
        let now = Instant::now();
        Self {
            args,
            window: None,
            renderer: None,
            video: None,
            audio: None,
            shader: None,
            midi: None,
            shader_mtime: None,
            last_shader_check: Instant::now(),
            stats: FrameStats::new(3600),
            started: now,
            last_frame: now,
            summary_printed: false,
        }
    }

    fn render_one_frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = now;
        let t = now.duration_since(self.started).as_secs_f32();

        // A zenei pozíció: MIDI clock-ból, ha van és be van zárva; különben belső 120 BPM.
        let beat = match self.midi.as_ref() {
            Some(engine) if engine.is_locked() => engine.transport.beats_at(t as f64) as f32,
            _ => t * 2.0,
        };

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let device = renderer.device().clone();
        let now = Instant::now();
        let video_view = match self.video.as_mut() {
            Some(video) => match video.advance(&device, now) {
                Ok(view) => view,
                Err(e) => {
                    log::error!("videó hiba: {e:#}");
                    None
                }
            },
            None => None,
        };
        // MIDI: hardveres byte-ok átvétele, clock, envelope-ok léptetése.
        if let Some(midi) = self.midi.as_mut() {
            midi.tick(t as f64, dt_ms as f32 / 1000.0);
            let _ = midi.take_events();
        }

        // A látható réteg kiválasztása: shader > audio > videó.
        let (w, h) = renderer.size();
        let mut layer: Option<LayerRef> = None;
        if self.shader.is_some() {
            // Hot-reload: ha a fájl megváltozott, újrafordítjuk.
            if self.last_shader_check.elapsed() > Duration::from_millis(800) {
                self.last_shader_check = Instant::now();
                let mtime = self
                    .shader
                    .as_ref()
                    .and_then(|e| std::fs::metadata(&e.shader.path).ok())
                    .and_then(|m| m.modified().ok());
                if mtime.is_some() && mtime != self.shader_mtime {
                    let effect = self.shader.as_mut().unwrap();
                    let path = effect.shader.path.clone();
                    match effect.reload(&device) {
                        Ok(()) => {
                            log::info!("shader újratöltve: {}", path.display());
                            self.shader_mtime = mtime;
                        }
                        Err(e) => log::error!("shader újratöltés hiba: {e:#}"),
                    }
                }
            }
            let effect = self.shader.as_mut().unwrap();

            // MIDI moduláció: note -> envelope -> shader paraméter.
            if let Some(midi) = self.midi.as_ref() {
                for mapping in &midi.mappings {
                    let Some(idx) = mapping
                        .target
                        .strip_prefix("shader.param.")
                        .and_then(|s| s.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    if let Some(p) = effect.shader.params.get(idx) {
                        let v = mapping.value();
                        let val = p.default[0] + v * (p.max - p.default[0]);
                        if idx < effect.values.len() {
                            effect.values[idx] = [val, p.default[1], p.default[2], p.default[3]];
                        }
                    }
                }
            }
            effect.update(renderer.queue(), [w as f32, h as f32], t, dt_ms as f32 / 1000.0);
            layer = Some(LayerRef {
                pipeline: effect.pipeline(),
                bind_group: effect.bind_group(),
            });
        } else if let Some((engine, vis)) = self.audio.as_mut() {
            let snapshot = engine.process();
            vis.update(renderer.queue(), &snapshot, t, w, h);
            layer = Some(LayerRef {
                pipeline: vis.pipeline(),
                bind_group: vis.bind_group(),
            });
        }
        if let Some(window) = self.window.as_ref() {
            match renderer.render(t, beat, self.args.layers, video_view, layer) {
                Ok(()) => {
                    // Csak a vsync-hez igazított, tényleges frame-eket mérjük.
                    self.stats.push(dt_ms as f32);
                    if let Some(report) = self.stats.take_report() {
                        println!("{}", report.line());
                    }
                }
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    renderer.reconfigure();
                }
                Err(wgpu::SurfaceError::OutOfMemory) => {
                    log::error!("GPU memória elfogyott");
                    event_loop.exit();
                    return;
                }
                Err(e) => log::warn!("surface hiba: {e}"),
            }
            if let Some(secs) = self.args.bench_secs {
                if t as f64 >= secs {
                    self.print_summary();
                    event_loop.exit();
                    return;
                }
            }
            window.request_redraw();
        }
    }

    fn print_summary(&mut self) {
        if self.summary_printed {
            return;
        }
        self.summary_printed = true;
        let elapsed = self.started.elapsed().as_secs_f64();
        println!("\n{}", "─".repeat(64));
        println!("BENCH ÖSSZEGZÉS — {}x{} vsync={} futás={:.2}s", self.args.width, self.args.height, self.args.vsync, elapsed);
        if let Some(s) = self.stats.summary() {
            println!(
                "frame-ek: {}  átlag: {:.2} ms ({:.1} fps)",
                s.frames, s.avg_ms, s.fps
            );
            println!(
                "p50: {:.2} ms  p95: {:.2} ms  p99: {:.2} ms  max: {:.2} ms",
                s.p50_ms, s.p95_ms, s.p99_ms, s.max_ms
            );
            let budget = 1000.0 / 60.0;
            println!(
                "60 Hz-es büdzsé 1.5x-e felett (>{:.2} ms): {} frame ({:.2}%)",
                budget * 1.5,
                s.over_budget,
                100.0 * s.over_budget as f64 / s.frames.max(1) as f64
            );
        }
        println!("{}", "─".repeat(64));
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("Dada VJ — Spike A")
            .with_inner_size(winit::dpi::PhysicalSize::new(self.args.width, self.args.height));
        if self.args.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("nem sikerült ablakot létrehozni"),
        );
        log::info!("ablak létrehozva: {:?}", window.inner_size());
        let renderer = pollster::block_on(Renderer::new_surface(window.clone(), self.args.vsync))
            .expect("nem sikerült a renderert inicializálni");
        if let Some(path) = self.args.video.clone() {
            match VideoLayer::open(renderer.device(), std::path::Path::new(&path), 1.0) {
                Ok(layer) => {
                    log::info!(
                        "videó-réteg: {} ({}x{}, {:.2} fps névleges)",
                        path,
                        layer.size.0,
                        layer.size.1,
                        layer.nominal_fps()
                    );
                    self.video = Some(layer);
                }
                Err(e) => log::error!("a videó nem nyitható meg: {e:#}"),
            }
        }
        if self.args.audio_visual {
            let dev = self.args.audio_device.as_deref();
            let engine = match dev {
                Some(dev) => {
                    let dev = if dev == "default" { None } else { Some(dev) };
                    match AudioEngine::input(48000.0, dev) {
                    Ok(e) => e,
                        Err(err) => {
                            log::error!(
                                "audio bemenet nem nyitható meg ({err:#}), szintetikus forrás"
                            );
                            AudioEngine::synthetic(48000.0)
                        }
                    }
                }
                None => AudioEngine::synthetic(48000.0),
            };
            let (w, h) = renderer.size();
            let vis = AudioVisual::new(renderer.device(), renderer.format(), w, h);
            log::info!(
                "audio-vizuál bekapcsolva: {:?} @ {} Hz",
                engine.kind(),
                engine.sample_rate()
            );
            self.audio = Some((engine, vis));
        }
        if let Some(path) = self.args.shader.clone() {
            match dada_fx::IsfShader::load(std::path::Path::new(&path)) {
                Ok(shader) => {
                    log::info!(
                        "ISF shader: {} — {} ({} paraméter{})",
                        path,
                        shader.metadata.description,
                        shader.params.len(),
                        if shader.skipped.is_empty() {
                            String::new()
                        } else {
                            format!(", kihagyva: {}", shader.skipped.join(", "))
                        }
                    );
                    match Effect::new(renderer.device(), renderer.format(), shader) {
                        Ok(effect) => {
                            self.shader_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                            self.shader = Some(effect);
                        }
                        Err(e) => log::error!("a shader pipeline nem építhető: {e:#}"),
                    }
                }
                Err(e) => log::error!("a shader nem tölthető be: {e:#}"),
            }
        }
        if self.args.midi {
            match MidiEngine::list_ports() {
                Ok(ports) => {
                    if ports.is_empty() {
                        log::warn!("nincs elérhető MIDI-bemenet");
                    } else {
                        log::info!(
                            "MIDI-bemenetek: {}",
                            ports
                                .iter()
                                .map(|p| p.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }
                Err(e) => log::warn!("MIDI-portok listázása sikertelen: {e:#}"),
            }
            let mut engine = MidiEngine::new();
            match engine.open_all() {
                Ok(names) => log::info!("megnyitva {} MIDI-forrás", names.len()),
                Err(e) => log::error!("MIDI megnyitás hiba: {e:#}"),
            }
            if self.args.midi_visual {
                // A kick (36) -> az első shader-paraméter, 220 ms lecsengéssel.
                let spec = EnvelopeSpec {
                    attack_ms: 0.0,
                    hold_ms: 0.0,
                    decay_ms: 220.0,
                    curve: dada_midi::Curve::Exponential,
                    retrigger: true,
                };
                engine.mappings.push(
                    Mapping::new(
                        0,
                        Trigger::Note {
                            note: 36,
                            channel: None,
                        },
                        "shader.param.0",
                        1.0,
                    )
                    .with_spec(spec),
                );
                log::info!("MIDI-moduláció: note 36 -> shader.param.0 (220 ms lecsengés)");
            }
            self.midi = Some(engine);
        }
        self.renderer = Some(renderer);
        self.window = Some(window.clone());
        self.started = Instant::now();
        self.last_frame = self.started;
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render_one_frame(event_loop),
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.print_summary();
    }
}
