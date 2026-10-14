//! Az alkalmazás életciklusa és a frame-ciklus.

use crate::audio_vis::AudioVisual;
use crate::hud::{FrameStats, ProcStats};
use crate::renderer::{LayerRef, Renderer};
use crate::video::VideoLayer;
use crate::web::{self, Command, WebShared};
use crate::Args;
use dada_audio::AudioEngine;
use dada_fx::Effect;
use dada_core::{
    Autopilot, AutopilotMode, BlendMode, Clip, ClipSource, Deck, DeckEvent, Mixer, ModInputs,
    ModMatrix, ModOutputs, ModRoute, ModSource, ModTarget, Quantize,
};
use dada_audio::AnalyzerSnapshot;
use dada_midi::MidiMessage;
use dada_midi::{EnvelopeSpec, Mapping, MidiEngine, Trigger};
use std::path::{Path, PathBuf};
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
    /// Deck-enként egy videó-réteg (a mixer.decks-cel párhuzamosan).
    videos: Vec<Option<VideoLayer>>,
    audio: Option<(AudioEngine, Option<AudioVisual>)>,
    audio_snapshot: AnalyzerSnapshot,
    matrix: ModMatrix,
    /// A betöltött projekt és a deck-konfigurációk (mentéshez és felépítéshez).
    project: crate::project::Project,
    /// Ableton Link figyelő (ha be van kapcsolva).
    link: Option<dada_link::LinkListener>,
    /// Felvétel: AVAssetWriter + a pixel bufferek Metal-csomagolója.
    recorder: Option<dada_media::macos::writer::VideoWriter>,
    /// RTMP stream: hardveres encoder + publikáló.
    encoder: Option<dada_media::macos::encoder::VideoEncoder>,
    publisher: Option<dada_stream::RtmpPublisher>,
    enc_cache: Option<dada_media::macos::metal::MetalTextureCache>,
    enc_start: Instant,
    enc_next: Instant,
    sent_avcc: bool,
    enc_fps: f64,
    enc_captures: u64,
    rec_cache: Option<dada_media::macos::metal::MetalTextureCache>,
    rec_next: Instant,
    rec_start: Instant,
    deck_configs: Vec<crate::project::DeckConfig>,
    mod_deck_opacity: Vec<f32>,
    mod_shader: Vec<(usize, f32)>,
    mod_audio: f32,
    mod_master: f32,
    mod_master_active: bool,
    shader: Option<Effect>,
    mixer: Option<Mixer>,
    deck_folder: Option<PathBuf>,
    midi: Option<MidiEngine>,
    shader_mtime: Option<SystemTime>,
    last_shader_check: Instant,
    web: Option<Arc<WebShared>>,
    /// Réteg-opacitások, ha több réteg van egyszerre.
    video_opacity: f32,
    shader_opacity: f32,
    audio_opacity: f32,
    last_preview: Instant,
    last_fps: f32,
    stats: FrameStats,
    proc_stats: ProcStats,
    started: Instant,
    last_frame: Instant,
    summary_printed: bool,
}

impl App {
    pub fn new(args: Args) -> Self {
        let mut args = args;
        let now = Instant::now();
        let project = load_project_into_args(&mut args);
        let deck_configs: Vec<crate::project::DeckConfig> = if !args.decks.is_empty() {
            args.decks
                .iter()
                .map(|s| crate::project::Project::parse_deck_spec(s, args.autopilot_beats).0)
                .collect()
        } else {
            project.decks.clone()
        };
        if args.decks.is_empty() && !project.decks.is_empty() {
            args.decks = project
                .decks
                .iter()
                .map(crate::project::Project::deck_to_spec)
                .collect();
        }
        Self {
            args,
            project,
            link: None,
            deck_configs,
            recorder: None,
            encoder: None,
            publisher: None,
            enc_cache: None,
            enc_start: now,
            enc_next: now,
            sent_avcc: false,
            enc_fps: 60.0,
            enc_captures: 0,
            rec_cache: None,
            rec_next: now,
            rec_start: now,
            window: None,
            renderer: None,
            videos: Vec::new(),
            audio: None,
            audio_snapshot: AnalyzerSnapshot::default(),
            matrix: ModMatrix::new(),
            mod_deck_opacity: Vec::new(),
            mod_shader: Vec::new(),
            mod_audio: 0.0,
            mod_master: 0.0,
            mod_master_active: false,
            shader: None,
            mixer: None,
            deck_folder: None,
            midi: None,
            shader_mtime: None,
            last_shader_check: Instant::now(),
            web: None,
            video_opacity: 1.0,
            shader_opacity: 0.45,
            audio_opacity: 0.7,
            last_preview: Instant::now(),
            last_fps: 0.0,
            stats: FrameStats::new(3600),
            proc_stats: ProcStats::new(),
            started: now,
            last_frame: now,
            summary_printed: false,
        }
    }

    /// A stream lezárása: a maradék frame-ek kiadása, majd a kapcsolat bontása.
    fn stop_streaming(&mut self) {
        if let Some(mut enc) = self.encoder.take() {
            enc.finish();
            let avcc = enc.avcc();
            let frames = enc.drain();
            if let Some(p) = self.publisher.as_ref() {
                if !self.sent_avcc {
                    if let Some(avcc) = &avcc {
                        p.send(dada_stream::VideoFrame {
                            data: dada_stream::flv::avc_sequence_header(avcc),
                            pts_ms: 0,
                        });
                        self.sent_avcc = true;
                    }
                }
                for frame in &frames {
                    p.send(dada_stream::VideoFrame {
                        data: dada_stream::flv::avc_frame(&frame.data, frame.keyframe, 0),
                        pts_ms: frame.pts_ms as u64,
                    });
                }
            }
            log::info!(
                "stream lezárva: {} frame ({} rögzítési ciklus, {} callback)",
                frames.len(),
                self.enc_captures,
                enc.callbacks()
            );
        }
        if let Some(mut p) = self.publisher.take() {
            let s = p.stats();
            p.stop();
            log::info!(
                "RTMP: {} frame elküldve, {} eldobva, {:.1} KB",
                s.sent,
                s.dropped,
                s.bytes as f64 / 1024.0
            );
        }
    }

    /// A felvétel lezárása (a fájl befejezése).
    fn finish_recording(&mut self) {
        if let Some(mut rec) = self.recorder.take() {
            let frames = rec.frames;
            match rec.finish() {
                Ok(()) => log::info!("felvétel lezárva: {frames} frame"),
                Err(e) => log::error!("a felvétel lezárása sikertelen: {e:#}"),
            }
        }
    }

    fn render_one_frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = now;
        let t = now.duration_since(self.started).as_secs_f32();

        // A zenei pozíció: Ableton Link > MIDI clock > belső 120 BPM.
        let link_now = self
            .link
            .as_ref()
            .and_then(|l| l.timeline())
            .map(|tl| (tl, dada_link::now_link_us()));
        let link_ok = link_now.map(|(tl, us)| tl.plausible_at(us)).unwrap_or(false);
        let beat = match (link_now, link_ok) {
            (Some((tl, us)), true) => tl.beat_at(us) as f32,
            _ => match self.midi.as_ref() {
                Some(engine) if engine.is_locked() => {
                    engine.transport.beats_at(t as f64) as f32
                }
                _ => t * 2.0,
            },
        };
        let clock_bpm: f64 = match (link_now, link_ok) {
            (Some((tl, _)), true) => tl.bpm(),
            _ => match self.midi.as_ref() {
                Some(engine) if engine.is_locked() => engine.bpm() as f64,
                _ => 120.0,
            },
        };

        // Deck/autopilot: kvantált clip-váltás (még a videó-réteg előtt).
        if self.mixer.is_some() {
            let device = self.renderer.as_ref().map(|r| r.device().clone());
            let bpm = clock_bpm;
            let dt_beats = (dt_ms / 1000.0) * bpm / 60.0;
            let events = self
                .mixer
                .as_mut()
                .map(|m| m.tick(beat as f64, dt_beats))
                .unwrap_or_default();
            for ev in events {
                if let DeckEvent::Launch {
                    deck, clip, at_beat, ..
                } = ev
                {
                    let next = self
                        .mixer
                        .as_ref()
                        .and_then(|m| m.decks.get(deck))
                        .and_then(|d| d.clips.get(clip))
                        .and_then(|c| match &c.source {
                            ClipSource::File(p) => Some((p.clone(), c.name.clone())),
                            _ => None,
                        });
                    if let (Some((path, name)), Some(device)) = (next, device.as_ref()) {
                        match VideoLayer::open(device, std::path::Path::new(&path), 1.0) {
                            Ok(video) => {
                                log::info!("deck {deck} váltás -> {name} (beat {at_beat:.2})");
                                if deck < self.videos.len() {
                                    self.videos[deck] = Some(video);
                                }
                            }
                            Err(e) => log::error!("a klip nem nyitható meg: {e:#}"),
                        }
                    }
                }
            }
        }

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let device = renderer.device().clone();
        let now = Instant::now();
        // Deck-enkénti videó-rétegek előkészítése (tulajdonolt view handle).
        let mut video_views: Vec<(usize, wgpu::TextureView)> = Vec::new();
        for (i, slot) in self.videos.iter_mut().enumerate() {
            if let Some(video) = slot.as_mut() {
                match video.advance(&device, now) {
                    Ok(Some(view)) => video_views.push((i, view)),
                    Ok(None) => {}
                    Err(e) => log::error!("videó hiba (deck {i}): {e:#}"),
                }
            }
        }
        // MIDI: hardveres byte-ok átvétele + események kigyűjtése.
        let mut midi_notes: Vec<(u8, u8)> = Vec::new();
        let mut midi_offs: Vec<u8> = Vec::new();
        let mut midi_ccs: Vec<(u8, u8)> = Vec::new();
        if let Some(midi) = self.midi.as_mut() {
            midi.tick(t as f64, dt_ms as f32 / 1000.0);
            for ev in midi.take_events() {
                match ev.message {
                    MidiMessage::NoteOn { note, velocity, .. } => midi_notes.push((note, velocity)),
                    MidiMessage::NoteOff { note, .. } => midi_offs.push(note),
                    MidiMessage::ControlChange { cc, value, .. } => midi_ccs.push((cc, value)),
                    _ => {}
                }
            }
        }

        // Audio feldolgozás (a modulációhoz és a vizuálhoz is).
        if let Some((engine, _)) = self.audio.as_mut() {
            self.audio_snapshot = engine.process();
        }

        // Modulációs mátrix.
        self.mod_shader.clear();
        self.mod_audio = 0.0;
        self.mod_master = 0.0;
        self.mod_master_active = false;
        for d in self.mod_deck_opacity.iter_mut() {
            *d = 0.0;
        }
        if !self.matrix.routes.is_empty() {
            let st = &self.audio_snapshot.state;
            let bpm = clock_bpm;
            let inputs = ModInputs {
                bands: [
                    st.bands[0], st.bands[1], st.bands[2], st.bands[3], st.bands[4], st.bands[5],
                ],
                rms: st.rms,
                onset: st.onset,
                notes: midi_notes,
                note_offs: midi_offs,
                ccs: midi_ccs,
                dt_ms: dt_ms as f32,
                beat: beat as f64,
                bpm,
            };
            let outputs = self.matrix.tick(&inputs);
            for (target, value) in &outputs.continuous {
                match target {
                    ModTarget::DeckOpacity(i) => {
                        while self.mod_deck_opacity.len() <= *i {
                            self.mod_deck_opacity.push(0.0);
                        }
                        if let Some(d) = self.mod_deck_opacity.get_mut(*i) {
                            *d = d.max(*value);
                        }
                    }
                    ModTarget::ShaderParam(i) => self.mod_shader.push((*i, *value)),
                    ModTarget::AudioOpacity => self.mod_audio = self.mod_audio.max(*value),
                    ModTarget::MasterOpacity => {
                        self.mod_master_active = true;
                        self.mod_master = self.mod_master.max(*value);
                    }
                    _ => {}
                }
            }
            for target in &outputs.triggers {
                if let ModTarget::DeckNextClip(deck) = target {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get(*deck) {
                            if !d.clips.is_empty() {
                                let next = d.active.map(|a| (a + 1) % d.clips.len()).unwrap_or(0);
                                let q = d.autopilot.quantize;
                                mixer.queue(*deck, next, beat as f64, q);
                                log::debug!("mod trigger: deck {deck} -> klip {next}");
                            }
                        }
                    }
                }
            }
        }

        // Web-parancsok alkalmazása (még a réteg kiválasztása előtt).
        if let Some(shared) = self.web.as_ref() {
            for cmd in shared.take_commands() {
                match cmd {
                    Command::SetParam { index, value } => {
                        if let Some(effect) = self.shader.as_mut() {
                            if let Some(p) = effect.shader.params.get(index) {
                                let mut v = p.default;
                                if let Some(cur) = effect.values.get(index) {
                                    v = *cur;
                                }
                                v[0] = value;
                                effect.set_param(index, v);
                            }
                        }
                    }
                    Command::Trigger { note, velocity } => {
                        if let Some(midi) = self.midi.as_mut() {
                            let now = t as f64;
                            midi.feed(0, &[0x90, note, velocity], now);
                            midi.feed(0, &[0x80, note, 0], now);
                        }
                    }
                    Command::Launch { deck, clip } => {
                        if let Some(mixer) = self.mixer.as_mut() {
                            let q = mixer
                                .decks
                                .get(deck)
                                .map(|d| d.autopilot.quantize)
                                .unwrap_or(Quantize::Beat);
                            if let Some(at) = mixer.queue(deck, clip, beat as f64, q) {
                                log::info!(
                                    "UI indítás: deck {deck} klip {clip} -> {at:.2} negyed"
                                );
                            }
                        }
                    }
                    Command::SetAutopilot {
                        deck,
                        mode,
                        every_beats,
                    } => {
                        if let Some(mixer) = self.mixer.as_mut() {
                            if let Some(d) = mixer.decks.get_mut(deck) {
                                d.autopilot.mode = match mode.as_str() {
                                    "sequential" => AutopilotMode::Sequential,
                                    "shuffle" => AutopilotMode::Shuffle,
                                    "weighted" => AutopilotMode::Weighted,
                                    _ => AutopilotMode::Off,
                                };
                                d.autopilot.every_beats = every_beats;
                                d.autopilot.next_change = None;
                                log::info!(
                                    "UI autopilot: deck {deck} = {mode} ({every_beats:.0} ütem)"
                                );
                            }
                        }
                    }
                    Command::SetDeck {
                        deck,
                        opacity,
                        blend,
                    } => {
                        if let Some(mixer) = self.mixer.as_mut() {
                            if let Some(d) = mixer.decks.get_mut(deck) {
                                if let Some(o) = opacity {
                                    d.opacity = o.clamp(0.0, 1.0);
                                }
                                if let Some(b) = blend {
                                    d.blend = match b.as_str() {
                                        "add" => BlendMode::Add,
                                        "screen" => BlendMode::Screen,
                                        "multiply" => BlendMode::Multiply,
                                        _ => BlendMode::Normal,
                                    };
                                }
                            }
                        }
                    }
                    Command::ModAdd {
                        source,
                        target,
                        amount,
                    } => {
                        let spec = format!("{source}->{target}:{amount}");
                        match parse_mod_route(&spec) {
                            Some(route) => {
                                self.matrix.add(route);
                                log::info!("UI mod hozzáadva: {spec}");
                            }
                            None => log::warn!("értelmezhetetlen UI mod: {spec}"),
                        }
                    }
                    Command::ModRemove { index } => {
                        if index < self.matrix.routes.len() {
                            self.matrix.routes.remove(index);
                            log::info!("UI mod törölve: #{index}");
                        }
                    }
                    Command::SaveProject { path } => {
                        let target = path
                            .or_else(|| self.args.project.clone())
                            .unwrap_or_else(|| "dada-project.toml".to_string());
                        let snapshot = project_snapshot(
                            &self.args,
                            &self.project,
                            self.mixer.as_ref(),
                            &self.matrix,
                            &self.deck_configs,
                            self.deck_folder.as_deref(),
                        );
                        match snapshot.save(Path::new(&target)) {
                            Ok(()) => log::info!("projekt mentve: {target}"),
                            Err(e) => log::error!("mentési hiba: {e:#}"),
                        }
                    }
                }
            }
        }

        // A réteglista összeállítása alulról felfelé: videó, ISF shader, audio.
        let (w, h) = renderer.size();
        let mut layers: Vec<LayerRef> = Vec::new();

        // Egyedüli rétegként teljes opacity, együtt a beállított értékek.
        let active_count = video_views.len()
            + self.shader.is_some() as usize
            + self.audio.is_some() as usize;
        let single = active_count <= 1;
        let video_opacity = if single { 1.0 } else { self.video_opacity };
        let shader_opacity = if single { 1.0 } else { self.shader_opacity };
        let audio_opacity = if single { 1.0 } else { self.audio_opacity };

        // 1. Videó-rétegek (deck-enként, saját blend/opacity).
        for (deck_index, view) in &video_views {
            let (blend, opacity) = self
                .mixer
                .as_ref()
                .and_then(|m| m.decks.get(*deck_index))
                .map(|d| (d.blend, d.opacity))
                .unwrap_or((BlendMode::Normal, video_opacity));
            let mod_delta = self
                .mod_deck_opacity
                .get(*deck_index)
                .copied()
                .unwrap_or(0.0);
            let master = master_scale(self.mod_master_active, self.mod_master);
            let opacity = ((opacity + mod_delta).min(1.0)) * master;
            if let Some(video) = self.videos.get(*deck_index).and_then(|s| s.as_ref()) {
                video.set_opacity(renderer.queue(), opacity);
                let bind_group = renderer.make_video_bind_group(view, video.params_buffer());
                layers.push(LayerRef {
                    pipeline: renderer.video_pipeline(blend).clone(),
                    bind_group,
                });
            }
        }

        // 2. ISF shader réteg — Add.
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
            // Modulációs mátrix -> shader paraméterek.
            for (idx, value) in &self.mod_shader {
                if let Some(p) = effect.shader.params.get(*idx) {
                    if *idx < effect.values.len() {
                        let val = p.default[0] + value * (p.max - p.default[0]);
                        effect.values[*idx] = [val, p.default[1], p.default[2], p.default[3]];
                    }
                }
            }
            // ISF képi bemenet: a lentebbi rétegek (videó/audio) a bemenete.
            if effect.has_inputs() {
                let below = layers.clone();
                if let Some(view) = renderer.render_input(t, beat, &below) {
                    effect.set_input(renderer.device(), &view);
                }
            }
            effect.opacity =
                shader_opacity * master_scale(self.mod_master_active, self.mod_master);
            effect.update(renderer.queue(), [w as f32, h as f32], t, dt_ms as f32 / 1000.0);
            layers.push(LayerRef {
                pipeline: effect.pipeline(BlendMode::Add).clone(),
                bind_group: effect.bind_group().clone(),
            });
        }

        // 3. Audio-vizuál réteg — Screen.
        if let Some((_, Some(vis))) = self.audio.as_mut() {
            vis.opacity = ((audio_opacity + self.mod_audio).min(1.0))
                * master_scale(self.mod_master_active, self.mod_master);
            vis.update(renderer.queue(), &self.audio_snapshot, t, w, h);
            layers.push(LayerRef {
                pipeline: vis.pipeline(BlendMode::Screen).clone(),
                bind_group: vis.bind_group().clone(),
            });
        }
        if let Some(window) = self.window.as_ref() {
            match renderer.render(t, beat, &layers) {
                Ok(()) => {
                    // Csak a vsync-hez igazított, tényleges frame-eket mérjük.
                    self.stats.push(dt_ms as f32);
                    if let Some(report) = self.stats.take_report() {
                        self.last_fps = report.fps as f32;
                        self.proc_stats.sample();
                        println!("{}", report.line_with_proc(&self.proc_stats));
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
                    self.finish_recording();
                    self.stop_streaming();
                    event_loop.exit();
                    return;
                }
            }
            // Felvétel: a kompozit a felvevő pixel bufferébe renderelve (zero-copy).
            if self.recorder.is_some() && self.rec_cache.is_some() {
                let now = Instant::now();
                if now >= self.rec_next {
                    let fps = self.recorder.as_ref().map(|r| r.fps).unwrap_or(60.0);
                    self.rec_next = now + Duration::from_secs_f64(1.0 / fps);
                    let buffer = self.recorder.as_ref().and_then(|r| r.pixel_buffer().ok());
                    if let Some(buffer) = buffer {
                        let (rw, rh) = self.recorder.as_ref().map(|r| r.size).unwrap_or((0, 0));
                        let wrapped = {
                            let device = renderer.device();
                            self.rec_cache
                                .as_ref()
                                .map(|c| {
                                    c.wrap_bgra_with_usage(
                                        device,
                                        &buffer,
                                        rw,
                                        rh,
                                        wgpu::TextureUsages::TEXTURE_BINDING
                                            | wgpu::TextureUsages::RENDER_ATTACHMENT
                                            | wgpu::TextureUsages::COPY_SRC,
                                    )
                                })
                        };
                        match wrapped {
                            Some(Ok(wrapped)) => {
                                let view = wrapped.view();
                                renderer.render_to(&view, (rw, rh), t, beat, &layers);
                                let pts_ms = self.rec_start.elapsed().as_secs_f64() * 1000.0;
                                if let Some(rec) = self.recorder.as_mut() {
                                    if let Err(e) = rec.append_at_ms(&buffer, pts_ms) {
                                        log::error!("felvétel-hiba: {e:#}");
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                log::error!("a felvétel-puffer nem csomagolható: {e:#}")
                            }
                            None => {}
                        }
                    }
                }
            }

            // Stream: hardveres encode, majd RTMP-re.
            if self.encoder.is_some() && self.publisher.is_some() && self.enc_cache.is_some() {
                let now = Instant::now();
                if now >= self.enc_next {
                    let fps = self.enc_fps;
                    self.enc_next = now + Duration::from_secs_f64(1.0 / fps);
                    if let Some(enc) = self.encoder.as_ref() {
                        match enc.pixel_buffer() {
                            Ok(buffer) => {
                                let (ew, eh) = enc.size;
                                let wrapped = {
                                    let device = renderer.device();
                                    self.enc_cache.as_ref().map(|c| {
                                        c.wrap_bgra_with_usage(
                                            device,
                                            &buffer,
                                            ew,
                                            eh,
                                            wgpu::TextureUsages::TEXTURE_BINDING
                                                | wgpu::TextureUsages::RENDER_ATTACHMENT
                                                | wgpu::TextureUsages::COPY_SRC,
                                        )
                                    })
                                };
                                match wrapped {
                                    Some(Ok(wrapped)) => {
                                        let view = wrapped.view();
                                        renderer.render_to(&view, (ew, eh), t, beat, &layers);
                                        let pts =
                                            self.enc_start.elapsed().as_secs_f64() * 1000.0;
                                        if let Err(e) = enc.encode(&buffer, pts) {
                                            log::error!("encode hiba: {e:#}");
                                        }
                                    }
                                    Some(Err(e)) => {
                                        log::error!("a stream-puffer nem csomagolható: {e:#}")
                                    }
                                    None => {}
                                }
                            }
                            Err(e) => log::error!("nincs encoder puffer: {e:#}"),
                        }
                        // A kész frame-ek továbbítása.
                        self.enc_captures += 1;
                        let avcc = enc.avcc();
                        for frame in enc.drain() {
                            if !self.sent_avcc {
                                if let Some(avcc) = &avcc {
                                    if let Some(p) = self.publisher.as_ref() {
                                        p.send(dada_stream::VideoFrame {
                                            data: dada_stream::flv::avc_sequence_header(avcc),
                                            pts_ms: 0,
                                        });
                                    }
                                    self.sent_avcc = true;
                                    log::info!("RTMP: avcC sequence header elküldve");
                                }
                            }
                            if let Some(p) = self.publisher.as_ref() {
                                p.send(dada_stream::VideoFrame {
                                    data: dada_stream::flv::avc_frame(
                                        &frame.data,
                                        frame.keyframe,
                                        0,
                                    ),
                                    pts_ms: frame.pts_ms as u64,
                                });
                            }
                        }
                    }
                }
            }

            window.request_redraw();
        }

        // Web: előnézet + állapot ~10 fps-en.
        if self.web.is_some() && self.last_preview.elapsed() > Duration::from_millis(100) {
            self.last_preview = Instant::now();
            if let Some(rgb) = renderer.capture_preview(&layers, t, beat) {
                let mut jpeg = Vec::new();
                let encoder = jpeg_encoder::Encoder::new(&mut jpeg, 72);
                if encoder
                    .encode(&rgb, 640, 360, jpeg_encoder::ColorType::Rgb)
                    .is_ok()
                {
                    if let Some(shared) = self.web.as_ref() {
                        shared.push_jpeg(jpeg);
                    }
                }
            }
            let (bpm, beats, midi_msgs) = match self.midi.as_ref() {
                Some(m) => (m.bpm(), m.transport.beats_at(t as f64), m.messages_in),
                None => (120.0f32, t as f64 * 2.0, 0u64),
            };
            let (shader_name, params) = match self.shader.as_ref() {
                Some(effect) => {
                    let list: Vec<serde_json::Value> = effect
                        .shader
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let v = effect.values.get(i).map(|v| v[0]).unwrap_or(p.default[0]);
                            serde_json::json!({
                                "name": p.name,
                                "value": v,
                                "min": p.min,
                                "max": p.max,
                            })
                        })
                        .collect();
                    (effect.shader.metadata.description.clone(), list)
                }
                None => (String::new(), Vec::new()),
            };
            let layer_name = if self.mixer.is_some() {
                "deck"
            } else if self.shader.is_some() {
                "shader"
            } else if self.audio.is_some() {
                "audio"
            } else {
                "teszt"
            };
            let decks_json: Vec<serde_json::Value> = match self.mixer.as_ref() {
                Some(mixer) => mixer
                    .decks
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let clips: Vec<serde_json::Value> = d
                            .clips
                            .iter()
                            .map(|c| serde_json::json!({ "name": c.name }))
                            .collect();
                        serde_json::json!({
                            "index": i,
                            "name": d.name,
                            "active": d.active,
                            "queued": d.queued.map(|q| q.clip),
                            "opacity": d.opacity,
                            "blend": blend_name(d.blend),
                            "has_video": self.videos.get(i).map(|v| v.is_some()).unwrap_or(false),
                            "autopilot": {
                                "mode": ap_name(d.autopilot.mode),
                                "every_beats": d.autopilot.every_beats,
                            },
                            "clips": clips,
                        })
                    })
                    .collect(),
                None => Vec::new(),
            };
            let mods_json: Vec<serde_json::Value> = self
                .matrix
                .routes
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "source": mod_source_name(r.source),
                        "target": mod_target_name(r.target),
                        "amount": r.amount,
                        "value": r.value,
                        "hits": r.env.triggers,
                    })
                })
                .collect();
            let state = serde_json::json!({
                "fps": self.last_fps,
                "bpm": bpm,
                "beats": beats,
                "midi_messages": midi_msgs,
                "layer": layer_name,
                "shader": shader_name,
                "params": params,
                "link": self.link.as_ref().map(|l| l.is_active()).unwrap_or(false),
                "link_packets": self.link.as_ref().map(|l| l.packets()).unwrap_or(0),
                "decks": decks_json,
                "mods": mods_json,
            });
            if let Some(shared) = self.web.as_ref() {
                shared.set_state(state.to_string());
            }
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
        self.proc_stats.sample();
        println!(
            "erőforrás: CPU {:5.1}% · max RSS {:.0} MB",
            self.proc_stats.cpu_percent, self.proc_stats.rss_mb
        );
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

fn blend_name(b: BlendMode) -> &'static str {
    match b {
        BlendMode::Normal => "normal",
        BlendMode::Add => "add",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        _ => "normal",
    }
}

fn ap_name(m: AutopilotMode) -> &'static str {
    match m {
        AutopilotMode::Off => "off",
        AutopilotMode::Sequential => "sequential",
        AutopilotMode::Shuffle => "shuffle",
        AutopilotMode::Weighted => "weighted",
    }
}

fn mod_source_name(s: ModSource) -> String {
    match s {
        ModSource::Note { note } => format!("note:{note}"),
        ModSource::Cc { cc } => format!("cc:{cc}"),
        ModSource::AudioBand(0) => "audio.sub".into(),
        ModSource::AudioBand(1) => "audio.bass".into(),
        ModSource::AudioBand(2) => "audio.low_mid".into(),
        ModSource::AudioBand(3) => "audio.mid".into(),
        ModSource::AudioBand(4) => "audio.high_mid".into(),
        ModSource::AudioBand(5) => "audio.high".into(),
        ModSource::AudioBand(i) => format!("audio.band{i}"),
        ModSource::AudioRms => "audio.rms".into(),
        ModSource::AudioOnset => "audio.onset".into(),
        ModSource::Lfo { period_beats } => format!("lfo:{period_beats}"),
        ModSource::ClockPhase => "clock.phase".into(),
    }
}

fn mod_target_name(t: ModTarget) -> String {
    match t {
        ModTarget::DeckOpacity(i) => format!("deck.opacity.{i}"),
        ModTarget::DeckNextClip(i) => format!("deck.next.{i}"),
        ModTarget::ShaderParam(i) => format!("shader.param.{i}"),
        ModTarget::AudioOpacity => "audio.opacity".into(),
        ModTarget::MasterOpacity => "master.opacity".into(),
    }
}

/// A master-opacity moduláció skálája (1.0, ha nincs ilyen útvonal).
fn master_scale(active: bool, value: f32) -> f32 {
    if active {
        (1.0 - value).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Egy modulációs útvonal értelmezése: FORRAS->CEL[:MENNY[:DECAY]]
fn parse_mod_route(spec: &str) -> Option<ModRoute> {
    let (src, dst_part) = spec.split_once("->")?;
    let mut parts = dst_part.split(':');
    let dst = parts.next()?;
    let amount: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let decay: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(150.0);

    let source = match src {
        s if s.starts_with("note:") => ModSource::Note {
            note: s[5..].parse().ok()?,
        },
        s if s.starts_with("cc:") => ModSource::Cc {
            cc: s[3..].parse().ok()?,
        },
        "audio.sub" => ModSource::AudioBand(0),
        "audio.bass" => ModSource::AudioBand(1),
        "audio.low_mid" => ModSource::AudioBand(2),
        "audio.mid" => ModSource::AudioBand(3),
        "audio.high_mid" => ModSource::AudioBand(4),
        "audio.high" => ModSource::AudioBand(5),
        "audio.rms" => ModSource::AudioRms,
        "audio.onset" => ModSource::AudioOnset,
        s if s.starts_with("lfo:") => ModSource::Lfo {
            period_beats: s[4..].parse().ok()?,
        },
        "clock.phase" => ModSource::ClockPhase,
        _ => return None,
    };
    let target = match dst {
        s if s.starts_with("deck.opacity.") => ModTarget::DeckOpacity(s[13..].parse().ok()?),
        s if s.starts_with("deck.next.") => ModTarget::DeckNextClip(s[10..].parse().ok()?),
        s if s.starts_with("shader.param.") => ModTarget::ShaderParam(s[13..].parse().ok()?),
        "audio.opacity" => ModTarget::AudioOpacity,
        "master.opacity" => ModTarget::MasterOpacity,
        _ => return None,
    };
    let mut route = ModRoute::new(source, target).with_amount(amount);
    route.spec.decay_ms = decay;
    Some(route)
}

/// A jelenlegi felállás projektként (mentéshez).
fn project_snapshot(args: &Args, project: &crate::project::Project, mixer: Option<&Mixer>, matrix: &ModMatrix, deck_configs: &[crate::project::DeckConfig], deck_folder: Option<&Path>) -> crate::project::Project {
    let mut p = crate::project::Project::from_cli(args, Some(project));
    if let Some(mixer) = mixer {
        p.decks = mixer
            .decks
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let folder = deck_configs
                    .get(i)
                    .map(|c| c.folder.clone())
                    .or_else(|| deck_folder.map(|f| f.display().to_string()))
                    .unwrap_or_default();
                crate::project::DeckConfig {
                    folder,
                    start_clip: d.active,
                    autopilot: Some(ap_name(d.autopilot.mode).to_string()),
                    every_beats: Some(d.autopilot.every_beats),
                    quantize: Some("beat".into()),
                    blend: Some(blend_name(d.blend).to_string()),
                    opacity: Some(d.opacity),
                    fade_beats: Some(d.fade_beats),
                }
            })
            .collect();
    }
    p.mods = matrix
        .routes
        .iter()
        .map(|r| crate::project::ModConfig {
            route: format!(
                "{}->{}:{}:{}",
                mod_source_name(r.source),
                mod_target_name(r.target),
                r.amount,
                r.spec.decay_ms
            ),
        })
        .collect();
    p
}

/// A projekt betöltése és a CLI-argumentumok kiegészítése vele.
fn load_project_into_args(args: &mut Args) -> crate::project::Project {
    let mut project = crate::project::Project::default();
    if let Some(path) = args.project.clone() {
        match crate::project::Project::load(Path::new(&path)) {
            Ok(p) => {
                log::info!(
                    "projekt betöltve: {path} ({} deck, {} mod)",
                    p.decks.len(),
                    p.mods.len()
                );
                project = p;
            }
            Err(e) => log::error!("a projekt nem tölthető be: {e:#}"),
        }
    }
    if !args.size_set {
      if let Some(sz) = &project.output.size {
        if let Some((w, h)) = sz.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                args.width = w;
                args.height = h;
            }
        }
      }
    }
    if !args.vsync_set {
        if let Some(v) = project.output.vsync {
            args.vsync = v;
        }
    }
    if !args.serve {
        if project.output.serve == Some(true) {
            args.serve = true;
        }
    }
    if !args.port_set {
        if let Some(p) = project.output.port {
            args.port = p;
        }
    }
    if args.shader.is_none() {
        args.shader = project.shader.clone();
    }
    if !args.audio_visual {
        if project.audio.enabled == Some(true) {
            args.audio_visual = true;
        }
    }
    if args.audio_device.is_none() {
        args.audio_device = project.audio.device.clone();
    }
    if args.mods.is_empty() {
        args.mods = project.mods.iter().map(|m| m.route.clone()).collect();
    }
    project
}

fn blend_from_name(name: Option<&str>) -> BlendMode {
    match name {
        Some("add") => BlendMode::Add,
        Some("screen") => BlendMode::Screen,
        Some("multiply") => BlendMode::Multiply,
        _ => BlendMode::Normal,
    }
}

fn autopilot_from_name(name: Option<&str>, beats: f64) -> AutopilotMode {
    match name {
        Some("sequential") => AutopilotMode::Sequential,
        Some("shuffle") => AutopilotMode::Shuffle,
        Some("weighted") => AutopilotMode::Weighted,
        Some("off") => AutopilotMode::Off,
        _ => {
            if beats > 0.0 {
                AutopilotMode::Sequential
            } else {
                AutopilotMode::Off
            }
        }
    }
}

fn quantize_from_name(name: Option<&str>) -> Quantize {
    match name {
        Some("bar") | Some("1bar") => Quantize::Bar,
        Some("off") => Quantize::Off,
        Some("1/16") => Quantize::Sixteenth,
        Some("1/8") => Quantize::Eighth,
        Some("4") => Quantize::FourBeats,
        Some("8") => Quantize::EightBeats,
        Some("16") => Quantize::SixteenBeats,
        _ => Quantize::Beat,
    }
}

/// Videófájlok felkutatása egy mappában.
fn scan_videos(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()),
                        Some(ref s) if matches!(s.as_str(), "mp4" | "mov" | "m4v" | "mkv")
                    )
                })
                .collect()
        })
        .unwrap_or_default()
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
        // Ableton Link figyelő.
        if self.args.link {
            match dada_link::LinkListener::new() {
                Ok(l) => {
                    log::info!(
                        "Ableton Link figyelő elindult ({} port) — a tempó és a beat a Link-ből jön, ha van peer",
                        dada_link::LINK_PORT
                    );
                    self.link = Some(l);
                }
                Err(e) => log::error!("a Link figyelő nem indítható: {e:#}"),
            }
        }

        // Modulációs mátrix felépítése.
        for spec in &self.args.mods {
            match parse_mod_route(spec) {
                Some(route) => {
                    log::info!(
                        "mod: {:?} -> {:?} (mennyiség {:.2}, decay {:.0} ms)",
                        route.source,
                        route.target,
                        route.amount,
                        route.spec.decay_ms
                    );
                    self.matrix.add(route);
                }
                None => log::error!("értelmezhetetlen mod útvonal: {spec}"),
            }
        }

        // Felvétel indítása (natív H.264; a kompozit közvetlenül a felvevő pufferébe megy).
        if let Some(path) = self.args.record.clone() {
            let (rw, rh) = renderer.size();
            let fps = 60.0f64;
            match dada_media::macos::writer::VideoWriter::new(
                Path::new(&path),
                rw,
                rh,
                fps,
                12_000_000,
            ) {
                Ok(writer) => match dada_media::macos::metal::MetalTextureCache::new(
                    renderer.device(),
                ) {
                    Ok(cache) => {
                        log::info!("felvétel: {path} ({rw}x{rh} @ {fps} fps, 12 Mbit/s)");
                        self.rec_cache = Some(cache);
                        self.recorder = Some(writer);
                        self.rec_start = Instant::now();
                    }
                    Err(e) => log::error!("a felvétel textúra-cache nem hozható létre: {e:#}"),
                },
                Err(e) => log::error!("a felvétel nem indítható: {e:#}"),
            }
        }

        // RTMP stream indítása (hardveres encoder + háttérszálas publikáló).
        if let Some(url) = self.args.rtmp.clone() {
            let (ew, eh) = renderer.size();
            let bitrate = 8_000_000u32;
            match dada_media::macos::encoder::VideoEncoder::new(ew, eh, bitrate, 60) {
                Ok(encoder) => match dada_stream::RtmpPublisher::start(&url) {
                    Ok(publisher) => match dada_media::macos::metal::MetalTextureCache::new(
                        renderer.device(),
                    ) {
                        Ok(cache) => {
                            log::info!(
                                "RTMP stream: {url} ({ew}x{eh}, {:.1} Mbit/s)",
                                bitrate as f64 / 1_000_000.0
                            );
                            self.encoder = Some(encoder);
                            self.publisher = Some(publisher);
                            self.enc_cache = Some(cache);
                            self.enc_start = Instant::now();
                            self.enc_next = Instant::now();
                        }
                        Err(e) => log::error!("a stream textúra-cache nem hozható létre: {e:#}"),
                    },
                    Err(e) => log::error!("az RTMP publikáló nem indul: {e:#}"),
                },
                Err(e) => log::error!("az encoder nem hozható létre: {e:#}"),
            }
        }

        let need_audio = self.args.audio_visual || self.matrix.uses_audio();
        if !need_audio {
            // nincs audio
        } else {
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
            let vis = if self.args.audio_visual {
                let (w, h) = renderer.size();
                Some(AudioVisual::new(renderer.device(), renderer.format(), w, h))
            } else {
                None
            };
            log::info!(
                "audio bekapcsolva: {:?} @ {} Hz (vizuál: {})",
                engine.kind(),
                engine.sample_rate(),
                if vis.is_some() { "igen" } else { "nem" }
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
        // Deckek felépítése: --video (egy klip) vagy --deck (mappa[:ütem[:blend]]).
        let mut specs: Vec<(PathBuf, Vec<PathBuf>, f64, BlendMode)> = Vec::new();
        if let Some(path) = self.args.video.clone() {
            let p = PathBuf::from(&path);
            let parent = p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            specs.push((parent, vec![p], 0.0, BlendMode::Normal));
        }
        for spec in &self.args.decks {
            let mut parts = spec.split(':');
            let folder = parts.next().unwrap_or(".").to_string();
            let beats: f64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(self.args.autopilot_beats);
            let blend = match parts.next() {
                Some("add") => BlendMode::Add,
                Some("screen") => BlendMode::Screen,
                Some("multiply") => BlendMode::Multiply,
                _ => BlendMode::Normal,
            };
            let path = PathBuf::from(&folder);
            let mut names = scan_videos(&path);
            names.sort();
            specs.push((path, names, beats, blend));
        }
        if !specs.is_empty() {
            let mut mixer = Mixer::new(specs.len());
            for (i, (folder, names, beats, blend)) in specs.iter().enumerate() {
                if names.is_empty() {
                    log::error!("nincs videó a mappában: {}", folder.display());
                    self.videos.push(None);
                    continue;
                }
                let clips: Vec<Clip> = names
                    .iter()
                    .map(|p| {
                        Clip::new(
                            p.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("?")
                                .to_string(),
                            ClipSource::File(p.display().to_string()),
                        )
                    })
                    .collect();
                let name = ((b'A' + i as u8) as char).to_string();
                let mut deck = Deck::new(name.clone(), clips);
                deck.blend = *blend;
                deck.opacity = if i == 0 { 1.0 } else { 0.85 };
                deck.fade_beats = 0.0;
                deck.autopilot = Autopilot {
                    mode: if *beats > 0.0 {
                        AutopilotMode::Sequential
                    } else {
                        AutopilotMode::Off
                    },
                    every_beats: *beats,
                    quantize: Quantize::Beat,
                    next_change: None,
                };
                match VideoLayer::open(renderer.device(), &names[0], 1.0) {
                    Ok(layer) => {
                        log::info!(
                            "deck {name}: {} klip, autopilot {}, blend {:?}, {}x{}",
                            names.len(),
                            if *beats > 0.0 {
                                format!("{beats:.0} ütem")
                            } else {
                                "ki".to_string()
                            },
                            blend,
                            layer.size.0,
                            layer.size.1
                        );
                        self.videos.push(Some(layer));
                        deck.active = Some(0);
                    }
                    Err(e) => {
                        log::error!("a klip nem nyitható meg: {e:#}");
                        self.videos.push(None);
                    }
                }
                mixer.decks[i] = deck;
            }
            self.mixer = Some(mixer);
            self.deck_folder = specs.first().map(|s| s.0.clone());
        }

        // A projektből származó extra deck-beállítások (opacity, autopilot mód, fade).
        if let Some(mixer) = self.mixer.as_mut() {
            for (i, cfg) in self.deck_configs.clone().iter().enumerate() {
                if let Some(d) = mixer.decks.get_mut(i) {
                    if let Some(op) = cfg.opacity {
                        d.opacity = op.clamp(0.0, 1.0);
                    }
                    if let Some(mode) = cfg.autopilot.as_deref() {
                        d.autopilot.mode = autopilot_from_name(Some(mode), d.autopilot.every_beats);
                    }
                    if let Some(f) = cfg.fade_beats {
                        d.fade_beats = f;
                    }
                }
            }
        }
        if self.args.serve {
            let shared = WebShared::new();
            match web::spawn(self.args.port, Arc::clone(&shared)) {
                Ok(()) => self.web = Some(shared),
                Err(e) => log::error!("{e:#}"),
            }
        }
        if self.args.midi || self.args.serve {
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
