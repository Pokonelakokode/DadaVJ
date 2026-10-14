//! Az alkalmazás életciklusa és a frame-ciklus.

use crate::audio_vis::AudioVisual;
use crate::hud::FrameStats;
use crate::renderer::{AudioLayerRef, Renderer};
use crate::video::VideoLayer;
use crate::Args;
use dada_audio::AudioEngine;
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

        // 120 BPM szintetikus beat, amíg a MIDI clock/Link be nem köt.
        let beat = t * 2.0;

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
        // Audio-elemzés + GPU-textúrák frissítése.
        let audio_layer = match self.audio.as_mut() {
            Some((engine, vis)) => {
                let snapshot = engine.process();
                let (w, h) = renderer.size();
                vis.update(renderer.queue(), &snapshot, t, w, h);
                Some(AudioLayerRef {
                    pipeline: vis.pipeline(),
                    bind_group: vis.bind_group(),
                })
            }
            None => None,
        };
        if let Some(window) = self.window.as_ref() {
            match renderer.render(t, beat, self.args.layers, video_view, audio_layer) {
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
