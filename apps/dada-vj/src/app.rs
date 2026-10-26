//! Az alkalmazás életciklusa és a frame-ciklus.

use crate::audio_vis::AudioVisual;
use crate::hud::{FrameStats, ProcStats};
use crate::renderer::{LayerRef, MasterFxParams, Renderer};
use crate::video::VideoLayer;
use crate::web::{self, Command, WebShared};
use crate::Args;
use dada_audio::{AnalyzerSnapshot, AudioDeviceInfo, AudioEngine};
use dada_core::{
    Autopilot, AutopilotMode, BlendMode, ChannelModInput, ChromaMode, Clip, ClipSource, Deck,
    DeckEvent, LayerOrder, LoopMode, ManualNextMode, Mixer, ModInputs, ModMatrix, ModRoute, ModSource,
    ModTarget, Quantize, Transport,
};
use dada_fx::Effect;
use dada_midi::MidiMessage;
use dada_midi::{EnvelopeSpec, Mapping, MidiEngine, Trigger};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use std::time::{Duration, SystemTime};
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
    /// Crossfade alatt az előzőleg futó videó-réteg.
    fading_videos: Vec<Option<VideoLayer>>,
    audio: Option<(AudioEngine, Option<AudioVisual>)>,
    audio_snapshot: AnalyzerSnapshot,
    audio_selected_channels: Vec<usize>,
    /// Felvétel/stream hangforrás.
    capture_audio_enabled: bool,
    capture_audio_source: String,
    capture_audio_channel_spec: String,
    capture_audio: Option<AudioEngine>,
    capture_audio_snapshot: AnalyzerSnapshot,
    matrix: ModMatrix,
    /// A betöltött projekt és a deck-konfigurációk (mentéshez és felépítéshez).
    project: crate::project::Project,
    /// Ableton Link figyelő (ha be van kapcsolva).
    link: Option<dada_link::LinkListener>,
    /// Belső zenei óra (ha nincs Link vagy MIDI clock).
    transport: Transport,
    /// Felvétel: AVAssetWriter + a pixel bufferek Metal-csomagolója.
    recorder: Option<dada_media::macos::writer::VideoWriter>,
    rec_path: String,
    rec_bitrate_mbps: f64,
    /// RTMP stream: hardveres encoder + publikáló.
    encoder: Option<dada_media::macos::encoder::VideoEncoder>,
    stream_url: String,
    stream_bitrate_mbps: f64,
    /// Hang a streamhez: AAC encoder + mono gyűjtőpuffer.
    aac: Option<dada_media::macos::aac_objc::AacEncoder>,
    audio_pcm: Vec<f32>,
    audio_frames_written: u64,
    sent_audio_config: bool,
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
    mod_deck_speed: Vec<f32>,
    mod_shader: Vec<(usize, f32)>,
    mod_audio: f32,
    mod_video: f32,
    mod_shader_op: f32,
    mod_master: f32,
    mod_master_active: bool,
    mod_fx: MasterFxParams,
    master_fx: MasterFxParams,
    shader: Option<Effect>,
    mixer: Option<Mixer>,
    deck_folder: Option<PathBuf>,
    midi: Option<MidiEngine>,
    last_ccs: BTreeMap<u8, u8>,
    shader_mtime: Option<SystemTime>,
    last_shader_check: Instant,
    web: Option<Arc<WebShared>>,
    /// Master és réteg-opacitások / blend módok.
    master_opacity: f32,
    blackout: bool,
    video_opacity: f32,
    shader_opacity: f32,
    shader_blend: BlendMode,
    /// Az ISF effekt alpha-forrása: false = fedés (opacity), true = shader alpha.
    shader_alpha_from_shader: bool,
    /// Az ISF effekt cél rétege: None = legfelső kompozit, Some(N) = deck N rétege.
    shader_target: Option<usize>,
    /// A deck-rétegek Z-sorrendje alulról felfelé.
    layer_order: LayerOrder,
    audio_opacity: f32,
    audio_blend: BlendMode,
    /// Felfedezett erőforrások a UI legördülő listáihoz.
    available_shaders: Vec<String>,
    available_audio_devices: Vec<String>,
    available_audio_devices_detailed: Vec<AudioDeviceInfo>,
    available_midi_ports: Vec<String>,
    available_projects: Vec<String>,
    available_media: Vec<String>,
    last_preview: Instant,
    last_fps: f32,
    stats: FrameStats,
    proc_stats: ProcStats,
    started: Instant,
    last_frame: Instant,
    /// Az előző frame zenei pozíciója az idővonal-ugrások felismeréséhez.
    last_beat: Option<f64>,
    summary_printed: bool,
}

impl App {
    pub fn new(args: Args) -> Self {
        let mut args = args;
        let now = Instant::now();
        let project = load_project_into_args(&mut args);
        let deck_count = if !args.decks.is_empty() {
            args.decks.len()
        } else {
            project.decks.len()
        };
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
        let mut transport = Transport::new(120.0);
        transport.play(0.0);
        let rec_path = args
            .record
            .clone()
            .unwrap_or_else(|| "dada-record.mp4".to_string());
        let stream_url = args
            .rtmp
            .clone()
            .unwrap_or_else(|| "rtmp://127.0.0.1:1935/live/dada".to_string());
        let mut app = Self {
            args,
            project,
            link: None,
            transport,
            deck_configs,
            recorder: None,
            rec_path,
            rec_bitrate_mbps: 12.0,
            encoder: None,
            stream_url,
            stream_bitrate_mbps: 8.0,
            aac: None,
            audio_pcm: Vec::new(),
            audio_frames_written: 0,
            sent_audio_config: false,
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
            fading_videos: Vec::new(),
            audio: None,
            audio_snapshot: AnalyzerSnapshot::default(),
            audio_selected_channels: Vec::new(),
            capture_audio_enabled: true,
            capture_audio_source: "analysis".to_string(),
            capture_audio_channel_spec: "-1".to_string(),
            capture_audio: None,
            capture_audio_snapshot: AnalyzerSnapshot::default(),
            matrix: ModMatrix::new(),
            mod_deck_opacity: Vec::new(),
            mod_deck_speed: Vec::new(),
            mod_shader: Vec::new(),
            mod_audio: 0.0,
            mod_video: 0.0,
            mod_shader_op: 0.0,
            mod_master: 0.0,
            mod_master_active: false,
            mod_fx: MasterFxParams {
                saturation: 0.0,
                contrast: 0.0,
                ..MasterFxParams::default()
            },
            master_fx: MasterFxParams::default(),
            shader: None,
            mixer: None,
            deck_folder: None,
            midi: None,
            last_ccs: BTreeMap::new(),
            shader_mtime: None,
            last_shader_check: Instant::now(),
            web: None,
            master_opacity: 1.0,
            blackout: false,
            video_opacity: 1.0,
            shader_opacity: 1.0,
            shader_blend: BlendMode::Normal,
            shader_alpha_from_shader: false,
            shader_target: None,
            layer_order: LayerOrder::new(deck_count),
            audio_opacity: 1.0,
            audio_blend: BlendMode::Screen,
            available_shaders: Vec::new(),
            available_audio_devices: Vec::new(),
            available_audio_devices_detailed: Vec::new(),
            available_midi_ports: Vec::new(),
            available_projects: Vec::new(),
            available_media: Vec::new(),
            last_preview: Instant::now(),
            last_fps: 0.0,
            stats: FrameStats::new(3600),
            proc_stats: ProcStats::new(),
            started: now,
            last_frame: now,
            last_beat: None,
            summary_printed: false,
        };
        app.refresh_catalog();
        app
    }

    fn refresh_catalog(&mut self) {
        let mut shaders = Vec::new();
        if let Ok(rd) = std::fs::read_dir("shaders") {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("isf") {
                    shaders.push(p.display().to_string());
                }
            }
        }
        shaders.sort();
        self.available_shaders = shaders;

        let mut projects = Vec::new();
        if let Ok(rd) = std::fs::read_dir(".") {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                        if name != "Cargo.toml" {
                            projects.push(name.to_string());
                        }
                    }
                }
            }
        }
        projects.sort();
        self.available_projects = projects;

        let mut media = Vec::new();
        for dir in [".", "media", ".testmedia", "examples", "clips"] {
            let p = Path::new(dir);
            if !p.exists() {
                continue;
            }
            if !scan_videos(p).is_empty() && dir != "." {
                media.push(dir.to_string());
            }
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let sub = e.path();
                    if sub.is_dir() {
                        let name = sub.display().to_string();
                        if !name.starts_with("./.")
                            && !name.starts_with("./target")
                            && !scan_videos(&sub).is_empty()
                        {
                            media.push(name.trim_start_matches("./").to_string());
                        }
                    } else if matches!(
                        sub.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()),
                        Some(ref s) if matches!(s.as_str(), "mp4" | "mov" | "m4v" | "mkv")
                    ) && dir == "."
                    {
                        media.push(
                            sub.display()
                                .to_string()
                                .trim_start_matches("./")
                                .to_string(),
                        );
                    }
                }
            }
        }
        media.sort();
        media.dedup();
        self.available_media = media;

        self.available_audio_devices_detailed = AudioEngine::list_devices_detailed();
        self.available_audio_devices = self
            .available_audio_devices_detailed
            .iter()
            .map(|d| d.name.clone())
            .collect();
        self.available_midi_ports = MidiEngine::list_ports()
            .map(|ports| ports.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
    }

    /// A projektből jövő réteg-sorrend / effekt célréteg / alpha / blend
    /// visszaállítása a már felépített deck-lista alapján.
    fn apply_project_layout(&mut self, project: &crate::project::Project) {
        let deck_count = self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
        self.layer_order = LayerOrder::new(deck_count);
        if let Some(order) = project.layer_order.as_deref() {
            if !self.layer_order.set(order, deck_count) {
                log::warn!(
                    "a projekt layer_order érvénytelen ({} deck): {order:?}",
                    deck_count
                );
                self.layer_order.ensure(deck_count);
            }
        }
        self.shader_target = project.shader_target.filter(|t| *t < deck_count);
        self.shader_alpha_from_shader =
            matches!(project.shader_alpha.as_deref(), Some("shader"));
        if let Some(b) = project.shader_blend.as_deref() {
            self.shader_blend = blend_from_name(Some(b));
        }
        if let Some(effect) = self.shader.as_mut() {
            effect.alpha_from_shader = self.shader_alpha_from_shader;
        }
        log::info!(
            "projekt rétegek: {:?} · effekt cél: {:?} · alpha: {} · blend: {}",
            self.layer_order.as_slice(),
            self.shader_target,
            if self.shader_alpha_from_shader {
                "shader"
            } else {
                "coverage"
            },
            blend_name(self.shader_blend)
        );
    }

    fn sync_web_clips(&self) {
        let Some(shared) = self.web.as_ref() else {
            return;
        };
        let mut clips: Vec<((usize, usize), String)> = Vec::new();
        if let Some(mixer) = self.mixer.as_ref() {
            for (di, d) in mixer.decks.iter().enumerate() {
                for (ci, c) in d.clips.iter().enumerate() {
                    if let ClipSource::File(p) = &c.source {
                        clips.push(((di, ci), p.clone()));
                    }
                }
            }
        }
        shared.set_clips(clips);
    }

    fn ensure_audio(&mut self) {
        if self.audio.is_some() {
            return;
        }
        let dev = self.args.audio_device.as_deref();
        let mut engine = match dev {
            Some(dev) if dev != "synthetic" && !dev.is_empty() => {
                let d = if dev == "default" { None } else { Some(dev) };
                match AudioEngine::input_with_channels(
                    48000.0,
                    d,
                    self.audio_selected_channels.clone(),
                ) {
                    Ok(e) => e,
                    Err(err) => {
                        log::error!("audio bemenet nem nyitható meg ({err:#}), szintetikus forrás");
                        AudioEngine::synthetic(48000.0)
                    }
                }
            }
            _ => AudioEngine::synthetic(48000.0),
        };
        self.audio_snapshot = engine.process();
        let vis = if self.args.audio_visual {
            self.renderer.as_ref().map(|r| {
                let (w, h) = r.size();
                AudioVisual::new(r.device(), r.format(), w, h)
            })
        } else {
            None
        };
        self.audio = Some((engine, vis));
    }

    /// A felvétel/stream hangforrásának előkészítése.
    fn prepare_capture_audio(&mut self) {
        if !self.capture_audio_enabled {
            self.capture_audio = None;
            return;
        }
        if self.capture_audio_source == "analysis" {
            self.capture_audio = None;
            self.ensure_audio();
            return;
        }
        let channels = AudioEngine::parse_channel_spec(&self.capture_audio_channel_spec);
        let source = self.capture_audio_source.clone();
        let mut engine = if source == "synthetic" {
            AudioEngine::synthetic(48000.0)
        } else {
            let dev = if source == "default" {
                None
            } else {
                Some(source.as_str())
            };
            match AudioEngine::input_with_channels(48000.0, dev, channels) {
                Ok(e) => e,
                Err(e) => {
                    log::error!("a capture audio forrás nem nyitható meg: {e:#}");
                    self.capture_audio = None;
                    return;
                }
            }
        };
        self.capture_audio_snapshot = engine.process();
        let rate = engine.sample_rate();
        log::info!("capture audio: {:?} @ {rate} Hz", engine.kind());
        self.capture_audio = Some(engine);
    }

    fn capture_sample_rate(&self) -> f64 {
        if !self.capture_audio_enabled {
            return 0.0;
        }
        if self.capture_audio_source == "analysis" {
            self.audio
                .as_ref()
                .map(|(e, _)| e.sample_rate() as f64)
                .unwrap_or(0.0)
        } else {
            self.capture_audio
                .as_ref()
                .map(|e| e.sample_rate() as f64)
                .unwrap_or(0.0)
        }
    }

    fn capture_mono(&self) -> &[f32] {
        if !self.capture_audio_enabled {
            return &[];
        }
        if self.capture_audio_source == "analysis" {
            &self.audio_snapshot.mono
        } else {
            &self.capture_audio_snapshot.mono
        }
    }

    /// A stream AAC encoderét a kiválasztott capture-forrás mintarátájához
    /// igazítja. Új stream / forrásváltás esetén hívjuk.
    fn ensure_capture_aac(&mut self) {
        if self.publisher.is_none() || !self.capture_audio_enabled {
            self.aac = None;
            return;
        }
        let rate = self.capture_sample_rate();
        if rate <= 0.0 {
            self.aac = None;
            return;
        }
        match dada_media::macos::aac_objc::AacEncoder::new(rate, 48000.0, 2, 128_000) {
            Ok(enc) => {
                log::info!(
                    "stream audio: AAC-LC 48 kHz sztereó (capture {rate:.0} Hz), 128 kbps"
                );
                self.aac = Some(enc);
                self.audio_pcm.clear();
                self.sent_audio_config = false;
                self.audio_frames_written = 0;
            }
            Err(e) => log::error!("az AAC encoder nem hozható létre: {e:#}"),
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
        self.aac = None;
        self.enc_cache = None;
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
        self.rec_cache = None;
    }

    /// A snap_beats-tel megjelölt aktív klipek sebességét a pillanatnyi
    /// BPM-hez igazítja, hogy a klip hossza pontosan ennyi ütem legyen.
    fn apply_bpm_snap(&mut self, bpm: f64, now: Instant) {
        let Some(mixer) = self.mixer.as_mut() else {
            return;
        };
        for (i, deck) in mixer.decks.iter_mut().enumerate() {
            let deck_sync = deck.bpm_sync;
            let Some(active) = deck.active else {
                continue;
            };
            let Some(clip) = deck.clips.get_mut(active) else {
                continue;
            };
            let snap_beats = if deck_sync > 0.0 {
                deck_sync
            } else {
                clip.snap_beats
            };
            if snap_beats <= 0.0 {
                continue;
            }
            let Some(video) = self.videos.get_mut(i).and_then(|v| v.as_mut()) else {
                continue;
            };
            let target = Clip::snap_speed(video.duration(), clip.in_point, snap_beats, bpm);
            if (video.speed - target).abs() > 0.002 {
                video.set_speed(target, now);
                clip.speed = target;
            }
        }
    }

    fn render_one_frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = now;
        let t = now.duration_since(self.started).as_secs_f32();

        // A zenei pozíció: Ableton Link > MIDI clock > belső Transport.
        let link_now = self
            .link
            .as_ref()
            .and_then(|l| l.timeline())
            .map(|tl| (tl, dada_link::now_link_us()));
        let link_ok = link_now
            .map(|(tl, us)| tl.plausible_at(us))
            .unwrap_or(false);
        let (beat, clock_bpm, clock_source) = match (link_now, link_ok) {
            (Some((tl, us)), true) => (tl.beat_at(us) as f32, tl.bpm(), "link"),
            _ => match self.midi.as_ref() {
                Some(engine) if engine.is_locked() => (
                    engine.transport.beats_at(t as f64) as f32,
                    engine.bpm() as f64,
                    "midi",
                ),
                _ => (
                    self.transport.beats_at(t as f64) as f32,
                    self.transport.bpm(),
                    "internal",
                ),
            },
        };

        // Ha a zenei idővonal hirtelen elugrik (tap/beat reset, MIDI Start,
        // Link timeline-váltás), az autopilot és a queue időpontjait is
        // toljuk vele, különben a rotáció a régi beat-számra várna.
        let beat_now = beat as f64;
        if let Some(prev) = self.last_beat {
            let delta = beat_now - prev;
            // Visszafelé már kis ugrás is reset/újrahorgonyzás; előrefelé
            // csak nagyobb (pl. seek vagy Link-váltás), hogy egy lassabb
            // frame ne tolja el feleslegesen az autopilotot.
            if delta < -0.25 || delta > 4.0 {
                if let Some(mixer) = self.mixer.as_mut() {
                    mixer.shift_schedule(delta);
                }
                log::debug!("zenei idő ugrás: {delta:+.2} beat, ütemezés eltolva");
            }
        }
        self.last_beat = Some(beat_now);

        let Some(device) = self.renderer.as_ref().map(|r| r.device().clone()) else {
            return;
        };
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
                    MidiMessage::ControlChange { cc, value, .. } => {
                        self.last_ccs.insert(cc, value);
                        midi_ccs.push((cc, value));
                    }
                    _ => {}
                }
            }
        }

        // Audio feldolgozás (a modulációhoz, a vizuálhoz és a streamhez).
        if let Some((engine, _)) = self.audio.as_mut() {
            self.audio_snapshot = engine.process();
        }
        // A felvétel/stream külön hangforrása (ha nem az analízis forrást használjuk).
        if let Some(engine) = self.capture_audio.as_mut() {
            self.capture_audio_snapshot = engine.process();
        }

        // Hang a felvételhez és a streamhez: a kiválasztott capture-forrás
        // mono mintái. A felvételnél frame-enként adjuk át, hogy a videó
        // fps-étől függetlenül egyetlen minta se maradjon ki.
        let capture_needed = self.recorder.is_some()
            || (self.aac.is_some() && self.publisher.is_some());
        let capture_samples = if capture_needed {
            self.capture_mono().to_vec()
        } else {
            Vec::new()
        };
        if !capture_samples.is_empty() {
            if let Some(rec) = self.recorder.as_mut() {
                if let Err(e) = rec.append_audio(&capture_samples) {
                    log::error!("felvétel-audio hiba: {e:#}");
                }
            }
        }
        if self.aac.is_some() && self.publisher.is_some() && !capture_samples.is_empty() {
            self.audio_pcm.extend_from_slice(&capture_samples);
            let mut packets: Vec<Vec<u8>> = Vec::new();
            while self.audio_pcm.len() >= 4800 {
                let chunk: Vec<f32> = self.audio_pcm.drain(..4800).collect();
                if let Some(aac) = self.aac.as_mut() {
                    match aac.encode(&chunk) {
                        Ok(p) => packets.extend(p),
                        Err(e) => log::error!("AAC hiba: {e:#}"),
                    }
                }
            }
            if !packets.is_empty() {
                let config = self
                    .aac
                    .as_ref()
                    .map(|a| a.audio_specific_config().to_vec())
                    .unwrap_or_default();
                if let Some(p) = self.publisher.as_ref() {
                    if !self.sent_audio_config && !config.is_empty() {
                        p.send_audio(dada_stream::AudioFrame {
                            data: dada_stream::flv::aac_sequence_header(&config),
                            pts_ms: 0,
                        });
                        self.sent_audio_config = true;
                        log::info!("RTMP: AAC sequence header elküldve ({:02X?})", config);
                    }
                    for packet in &packets {
                        let pts = (self.audio_frames_written as f64 / 48000.0 * 1000.0) as u64;
                        self.audio_frames_written += 1024;
                        p.send_audio(dada_stream::AudioFrame {
                            data: dada_stream::flv::aac_frame(packet),
                            pts_ms: pts,
                        });
                    }
                }
            }
        }

        // Modulációs mátrix.
        self.mod_shader.clear();
        self.mod_audio = 0.0;
        self.mod_video = 0.0;
        self.mod_shader_op = 0.0;
        self.mod_master = 0.0;
        self.mod_master_active = false;
        self.mod_fx = MasterFxParams {
            saturation: 0.0,
            contrast: 0.0,
            ..MasterFxParams::default()
        };
        for d in self.mod_deck_opacity.iter_mut() {
            *d = 0.0;
        }
        for s in self.mod_deck_speed.iter_mut() {
            *s = 0.0;
        }
        if !self.matrix.routes.is_empty() {
            let st = &self.audio_snapshot.state;
            let bpm = clock_bpm;
            let channels: Vec<ChannelModInput> = self
                .audio_snapshot
                .channels
                .iter()
                .map(|c| ChannelModInput {
                    rms: c.rms,
                    low: c.low,
                    mid: c.mid,
                    high: c.high,
                    onset: c.onset,
                })
                .collect();
            let inputs = ModInputs {
                bands: [
                    st.bands[0],
                    st.bands[1],
                    st.bands[2],
                    st.bands[3],
                    st.bands[4],
                    st.bands[5],
                ],
                rms: st.rms,
                onset: st.onset,
                channels,
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
                            *d += *value;
                        }
                    }
                    ModTarget::DeckSpeed(i) => {
                        while self.mod_deck_speed.len() <= *i {
                            self.mod_deck_speed.push(0.0);
                        }
                        if let Some(s) = self.mod_deck_speed.get_mut(*i) {
                            *s += *value;
                        }
                    }
                    ModTarget::ShaderParam(i) => self.mod_shader.push((*i, *value)),
                    ModTarget::AudioOpacity => self.mod_audio += *value,
                    ModTarget::VideoOpacity => self.mod_video += *value,
                    ModTarget::ShaderOpacity => self.mod_shader_op += *value,
                    ModTarget::MasterOpacity => {
                        self.mod_master_active = true;
                        self.mod_master += *value;
                    }
                    ModTarget::ClockBpm => {
                        if (*value).abs() > 0.001 {
                            let new_bpm = (60.0 + (*value as f64) * 140.0).clamp(30.0, 300.0);
                            self.transport.set_bpm(new_bpm, t as f64);
                        }
                    }
                    ModTarget::FxFlash => self.mod_fx.flash += *value,
                    ModTarget::FxInvert => self.mod_fx.invert += *value,
                    ModTarget::FxHue => self.mod_fx.hue += *value,
                    ModTarget::FxSaturation => self.mod_fx.saturation += *value,
                    ModTarget::FxContrast => self.mod_fx.contrast += *value,
                    ModTarget::FxRgbSplit => self.mod_fx.rgb_split += *value,
                    _ => {}
                }
            }
            for target in &outputs.triggers {
                match target {
                    ModTarget::DeckNextClip(deck) => {
                        if let Some(mixer) = self.mixer.as_mut() {
                            if let Some(d) = mixer.decks.get(*deck) {
                                if !d.clips.is_empty() {
                                    let next =
                                        d.active.map(|a| (a + 1) % d.clips.len()).unwrap_or(0);
                                    let q = d.autopilot.quantize;
                                    mixer.queue(*deck, next, beat as f64, q);
                                    log::debug!("mod trigger: deck {deck} -> klip {next}");
                                }
                            }
                        }
                    }
                    ModTarget::DeckLaunchClip(deck, clip) => {
                        if let Some(mixer) = self.mixer.as_mut() {
                            let q = mixer
                                .decks
                                .get(*deck)
                                .map(|d| d.autopilot.quantize)
                                .unwrap_or(Quantize::Beat);
                            mixer.queue(*deck, *clip, beat as f64, q);
                            log::debug!("mod trigger: deck {deck} -> klip {clip}");
                        }
                    }
                    ModTarget::MasterBlackout => {
                        self.blackout = !self.blackout;
                        if let Some(mx) = self.mixer.as_mut() {
                            mx.blackout = self.blackout;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Web-parancsok alkalmazása (még a réteg kiválasztása előtt).
        let commands = self
            .web
            .as_ref()
            .map(|s| s.take_commands())
            .unwrap_or_default();
        for cmd in commands {
            match cmd {
                Command::SetParam {
                    index,
                    name,
                    value,
                    values,
                } => {
                    if let Some(effect) = self.shader.as_mut() {
                        let resolved_idx = name
                            .as_deref()
                            .and_then(|n| effect.shader.params.iter().position(|p| p.name == n))
                            .unwrap_or(index);
                        if let Some(p) = effect.shader.params.get(resolved_idx) {
                            let mut v = p.default;
                            if let Some(cur) = effect.values.get(resolved_idx) {
                                v = *cur;
                            }
                            if let Some(arr) = values {
                                v = arr;
                            } else if let Some(val) = value {
                                v[0] = val;
                            }
                            effect.set_param(resolved_idx, v);
                        }
                    }
                }
                Command::ResetParams => {
                    if let Some(effect) = self.shader.as_mut() {
                        for (i, p) in effect.shader.params.iter().enumerate() {
                            if let Some(v) = effect.values.get_mut(i) {
                                *v = p.default;
                            }
                        }
                    }
                }
                Command::SetShader {
                    path,
                    opacity,
                    blend,
                } => {
                    if let Some(o) = opacity {
                        self.shader_opacity = o.clamp(0.0, 1.0);
                    }
                    if let Some(b) = blend {
                        self.shader_blend = blend_from_name(Some(b.as_str()));
                    }
                    if let Some(p) = path {
                        let trimmed = p.trim();
                        if trimmed.is_empty() || trimmed == "none" {
                            self.shader = None;
                            self.args.shader = None;
                            log::info!("UI shader kikapcsolva");
                        } else if let Some(r) = self.renderer.as_ref() {
                            match dada_fx::IsfShader::load(Path::new(trimmed)) {
                                Ok(sh) => match Effect::new(r.device(), r.format(), sh) {
                                    Ok(mut effect) => {
                                        effect.alpha_from_shader =
                                            self.shader_alpha_from_shader;
                                        self.shader_mtime = std::fs::metadata(trimmed)
                                            .and_then(|m| m.modified())
                                            .ok();
                                        self.shader = Some(effect);
                                        self.args.shader = Some(trimmed.to_string());
                                        log::info!("UI shader betöltve: {trimmed}");
                                    }
                                    Err(e) => log::error!("shader pipeline hiba: {e:#}"),
                                },
                                Err(e) => log::error!("shader betöltési hiba ({trimmed}): {e:#}"),
                            }
                        }
                    }
                }
                Command::ReloadShader => {
                    if let Some(effect) = self.shader.as_mut() {
                        match effect.reload(&device) {
                            Ok(()) => log::info!("UI shader újratöltve"),
                            Err(e) => log::error!("shader újratöltés hiba: {e:#}"),
                        }
                    }
                }
                Command::SetMaster {
                    master_opacity,
                    blackout,
                    video_opacity,
                    shader_opacity,
                    shader_blend,
                    shader_alpha,
                    shader_target,
                    audio_opacity,
                    audio_blend,
                } => {
                    if let Some(m) = master_opacity {
                        self.master_opacity = m.clamp(0.0, 1.0);
                        if let Some(mx) = self.mixer.as_mut() {
                            mx.master_opacity = self.master_opacity;
                        }
                    }
                    if let Some(b) = blackout {
                        self.blackout = b;
                        if let Some(mx) = self.mixer.as_mut() {
                            mx.blackout = b;
                        }
                    }
                    if let Some(v) = video_opacity {
                        self.video_opacity = v.clamp(0.0, 1.0);
                    }
                    if let Some(s) = shader_opacity {
                        self.shader_opacity = s.clamp(0.0, 1.0);
                    }
                    if let Some(sb) = shader_blend {
                        self.shader_blend = blend_from_name(Some(sb.as_str()));
                    }
                    if let Some(alpha) = shader_alpha.as_deref() {
                        self.shader_alpha_from_shader = matches!(alpha, "shader");
                        if let Some(effect) = self.shader.as_mut() {
                            effect.alpha_from_shader = self.shader_alpha_from_shader;
                        }
                        log::info!(
                            "ISF alpha mód: {}",
                            if self.shader_alpha_from_shader {
                                "shader (kulcsozás)"
                            } else {
                                "coverage (opacity fedés)"
                            }
                        );
                    }
                    if let Some(target) = shader_target {
                        let deck_count =
                            self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
                        self.shader_target = if target >= 0 {
                            let idx = target as usize;
                            if idx < deck_count {
                                Some(idx)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        log::info!(
                            "ISF cél réteg: {}",
                            self.shader_target
                                .map(|i| format!("deck #{i}"))
                                .unwrap_or_else(|| "legfelső kompozit".to_string())
                        );
                    }
                    if let Some(a) = audio_opacity {
                        self.audio_opacity = a.clamp(0.0, 1.0);
                    }
                    if let Some(ab) = audio_blend {
                        self.audio_blend = blend_from_name(Some(ab.as_str()));
                    }
                }
                Command::SetFx {
                    flash,
                    invert,
                    hue,
                    saturation,
                    contrast,
                    rgb_split,
                    reset,
                } => {
                    if reset {
                        self.master_fx = MasterFxParams::default();
                    } else {
                        if let Some(f) = flash {
                            self.master_fx.flash = f.clamp(0.0, 1.0);
                        }
                        if let Some(inv) = invert {
                            self.master_fx.invert = inv.clamp(0.0, 1.0);
                        }
                        if let Some(h) = hue {
                            self.master_fx.hue = h.clamp(0.0, 1.0);
                        }
                        if let Some(sat) = saturation {
                            self.master_fx.saturation = sat.clamp(0.0, 3.0);
                        }
                        if let Some(con) = contrast {
                            self.master_fx.contrast = con.clamp(0.1, 4.0);
                        }
                        if let Some(rgb) = rgb_split {
                            self.master_fx.rgb_split = rgb.clamp(0.0, 0.15);
                        }
                    }
                }
                Command::SetClock {
                    bpm,
                    playing,
                    reset_beat,
                    link,
                    anchor_time,
                    anchor_beat,
                } => {
                    // A UI által küldött timestamp a tap/beat-reset valódi
                    // pillanata. Ha megérkezett, ahhoz horgonyozzuk a fázist,
                    // nem a parancs feldolgozásának (későbbi) idejéhez.
                    let anchor = anchor_time.map(|at| {
                        let at = at.clamp(0.0, t as f64);
                        (at, anchor_beat.unwrap_or(0.0))
                    });
                    if let Some(b) = bpm {
                        let at = anchor.map(|(at, _)| at).unwrap_or(t as f64);
                        self.transport.set_bpm(b, at);
                    }
                    if let Some(play) = playing {
                        if play {
                            self.transport.play(t as f64);
                        } else {
                            self.transport.stop(t as f64);
                        }
                    }
                    if let Some((at, beat)) = anchor {
                        self.transport.seek(beat, at);
                        if let Some(midi) = self.midi.as_mut() {
                            // A MIDI-clock transportját is ugyanarra a
                            // fázisra állítjuk (ha épp az hajtja a vizuált).
                            midi.transport.seek(beat, at);
                        }
                    } else if reset_beat {
                        self.transport.seek(0.0, t as f64);
                        if let Some(midi) = self.midi.as_mut() {
                            midi.transport.seek(0.0, t as f64);
                        }
                    }
                    if let Some(en) = link {
                        self.args.link = en;
                        if en && self.link.is_none() {
                            match dada_link::LinkListener::new() {
                                Ok(l) => {
                                    log::info!("UI Ableton Link bekapcsolva");
                                    self.link = Some(l);
                                }
                                Err(e) => log::error!("Link indítás hiba: {e:#}"),
                            }
                        } else if !en {
                            self.link = None;
                            log::info!("UI Ableton Link kikapcsolva");
                        }
                    }
                }
                Command::Trigger { note, velocity } => {
                    let midi = self.midi.get_or_insert_with(MidiEngine::new);
                    let now = t as f64;
                    midi.feed(0, &[0x90, note, velocity], now);
                    midi.feed(0, &[0x80, note, 0], now);
                }
                Command::MidiCc { cc, value } => {
                    self.last_ccs.insert(cc, value);
                    let midi = self.midi.get_or_insert_with(MidiEngine::new);
                    midi.feed(0, &[0xB0, cc & 0x7F, value & 0x7F], t as f64);
                }
                Command::SetMidi {
                    rescan,
                    midi_visual,
                    enabled_ports,
                    toggle_port,
                } => {
                    let prev_enabled = self.midi.as_ref().and_then(|m| m.enabled_ports.clone());
                    if let Some(mv) = midi_visual {
                        self.args.midi_visual = mv;
                        let midi = self.midi.get_or_insert_with(MidiEngine::new);
                        midi.mappings.clear();
                        // Ha bekapcsoljuk és még nincs note:36 moduláció a mátrixban,
                        // felvesszük szerkeszthető útvonalként a Modulációs Mátrixba!
                        if mv {
                            let has_kick_route = self
                                .matrix
                                .routes
                                .iter()
                                .any(|r| matches!(r.source, ModSource::Note { note: 36 }));
                            if !has_kick_route {
                                if let Some(route) =
                                    parse_mod_route("note:36->shader.param.0:0.65:220")
                                {
                                    self.matrix.add(route);
                                }
                            }
                        } else {
                            self.matrix.routes.retain(|r| {
                                !(matches!(r.source, ModSource::Note { note: 36 })
                                    && matches!(r.target, ModTarget::ShaderParam(0)))
                            });
                        }
                    }
                    if rescan {
                        self.available_midi_ports = MidiEngine::list_ports()
                            .map(|ports| ports.into_iter().map(|p| p.name).collect())
                            .unwrap_or_default();
                        let mut engine = MidiEngine::new();
                        engine.enabled_ports = prev_enabled;
                        if let Ok(names) = engine.open_all() {
                            log::info!("MIDI újracsatlakoztatva: {} port", names.len());
                        }
                        self.midi = Some(engine);
                    }
                    if let Some(ports) = enabled_ports {
                        let midi = self.midi.get_or_insert_with(MidiEngine::new);
                        midi.enabled_ports = Some(ports);
                    }
                    if let Some(port_name) = toggle_port {
                        let all_ports = self.available_midi_ports.clone();
                        let midi = self.midi.get_or_insert_with(MidiEngine::new);
                        let mut list = midi
                            .enabled_ports
                            .clone()
                            .unwrap_or_else(|| all_ports.clone());
                        if let Some(pos) = list.iter().position(|p| p == &port_name) {
                            list.remove(pos);
                        } else {
                            list.push(port_name);
                        }
                        midi.enabled_ports = if list.len() == all_ports.len() {
                            None
                        } else {
                            Some(list)
                        };
                    }
                }
                Command::SetCaptureAudio {
                    enabled,
                    source,
                    channel_spec,
                } => {
                    if self.recorder.is_some() || self.publisher.is_some() {
                        log::warn!(
                            "capture audio beállítás felvétel/stream közben nem módosítható"
                        );
                    } else {
                        if let Some(e) = enabled {
                            self.capture_audio_enabled = e;
                        }
                        if let Some(s) = source {
                            self.capture_audio_source = s;
                        }
                        if let Some(c) = channel_spec {
                            self.capture_audio_channel_spec = c;
                        }
                        self.prepare_capture_audio();
                        log::info!(
                            "capture audio: enabled={} source={} channels={}",
                            self.capture_audio_enabled,
                            self.capture_audio_source,
                            self.capture_audio_channel_spec
                        );
                    }
                }
                Command::SetAudio {
                    visual,
                    device: dev_opt,
                    channel,
                    channel_spec,
                    opacity,
                    blend,
                } => {
                    if let Some(o) = opacity {
                        self.audio_opacity = o.clamp(0.0, 1.0);
                    }
                    if let Some(b) = blend {
                        self.audio_blend = blend_from_name(Some(b.as_str()));
                    }
                    if let Some(spec) = channel_spec {
                        self.audio_selected_channels = AudioEngine::parse_channel_spec(&spec);
                        if self.audio.is_none() && dev_opt.is_none() {
                            self.ensure_audio();
                        }
                        if let Some((eng, _)) = self.audio.as_mut() {
                            eng.set_selected_channels(self.audio_selected_channels.clone());
                        }
                    } else if let Some(ch) = channel {
                        self.audio_selected_channels = if ch < 0 {
                            Vec::new()
                        } else {
                            vec![ch as usize]
                        };
                        if self.audio.is_none() && dev_opt.is_none() {
                            self.ensure_audio();
                        }
                        if let Some((eng, _)) = self.audio.as_mut() {
                            eng.set_selected_channels(self.audio_selected_channels.clone());
                        }
                    }
                    if let Some(dev) = dev_opt {
                        let mut new_engine = if dev == "synthetic" || dev.is_empty() {
                            self.args.audio_device = None;
                            let mut e = AudioEngine::synthetic(48000.0);
                            e.set_selected_channels(self.audio_selected_channels.clone());
                            e
                        } else {
                            self.args.audio_device = Some(dev.clone());
                            let name = if dev == "default" {
                                None
                            } else {
                                Some(dev.as_str())
                            };
                            match AudioEngine::input_with_channels(
                                48000.0,
                                name,
                                self.audio_selected_channels.clone(),
                            ) {
                                Ok(e) => e,
                                Err(err) => {
                                    log::error!(
                                        "audio eszköz hiba ({err:#}), visszaállás szintetikusra"
                                    );
                                    AudioEngine::synthetic(48000.0)
                                }
                            }
                        };
                        self.audio_snapshot = new_engine.process();
                        let cur_vis = self.audio.take().and_then(|(_, v)| v);
                        self.audio = Some((new_engine, cur_vis));
                    }
                    if let Some(vis_on) = visual {
                        self.args.audio_visual = vis_on;
                        if vis_on {
                            self.ensure_audio();
                            let has_vis = self
                                .audio
                                .as_ref()
                                .map(|(_, v)| v.is_some())
                                .unwrap_or(false);
                            if !has_vis {
                                let new_vis = self.renderer.as_ref().map(|r| {
                                    let (w, h) = r.size();
                                    AudioVisual::new(r.device(), r.format(), w, h)
                                });
                                if let Some((_, slot)) = self.audio.as_mut() {
                                    *slot = new_vis;
                                }
                            }
                        } else if let Some((_, slot)) = self.audio.as_mut() {
                            *slot = None;
                        }
                    }
                }
                Command::Launch {
                    deck,
                    clip,
                    immediate,
                } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if immediate {
                            if let Some(at) = mixer.trigger_now(deck, clip, beat as f64) {
                                log::info!(
                                    "UI azonnali indítás: deck {deck} klip {clip} (@ {at:.2})"
                                );
                            }
                        } else {
                            let q = mixer
                                .decks
                                .get(deck)
                                .map(|d| d.autopilot.quantize)
                                .unwrap_or(Quantize::Beat);
                            if let Some(at) = mixer.queue(deck, clip, beat as f64, q) {
                                log::info!("UI indítás: deck {deck} klip {clip} -> {at:.2} negyed");
                            }
                        }
                    }
                }
                Command::DeckNext { deck, immediate } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get_mut(deck) {
                            if !d.clips.is_empty() {
                                // A kézi „Következő klip” gomb a deck saját
                                // manual_next beállítását használja, nem az
                                // autopilot shuffle/sequential módját.
                                let next = d.pick_manual_next().unwrap_or(0);
                                let q = d.autopilot.quantize;
                                if immediate {
                                    mixer.trigger_now(deck, next, beat as f64);
                                } else {
                                    mixer.queue(deck, next, beat as f64, q);
                                }
                            }
                        }
                    }
                }
                Command::SetAutopilot {
                    deck,
                    mode,
                    every_beats,
                    quantize,
                } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get_mut(deck) {
                            if let Some(m) = mode {
                                d.autopilot.mode = match m.as_str() {
                                    "sequential" => AutopilotMode::Sequential,
                                    "shuffle" => AutopilotMode::Shuffle,
                                    "weighted" => AutopilotMode::Weighted,
                                    _ => AutopilotMode::Off,
                                };
                            }
                            if let Some(eb) = every_beats {
                                d.autopilot.every_beats = eb.max(0.25);
                            }
                            if let Some(q) = quantize {
                                d.autopilot.quantize = quantize_from_name(Some(q.as_str()));
                            }
                            d.autopilot.next_change = None;
                            log::info!(
                                "UI autopilot: deck {deck} = {:?} ({:.1} ütem, quant={})",
                                d.autopilot.mode,
                                d.autopilot.every_beats,
                                d.autopilot.quantize.as_str()
                            );
                        }
                    }
                }
                Command::SetLayerOrder { order } => {
                    let deck_count = self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
                    if self.layer_order.set(&order, deck_count) {
                        log::info!("réteg-sorrend (alulról felfelé): {:?}", self.layer_order.as_slice());
                    } else {
                        log::warn!(
                            "érvénytelen réteg-sorrend ({} deck): {order:?}",
                            deck_count
                        );
                    }
                }
                Command::SetDeck {
                    deck,
                    opacity,
                    blend,
                    fade_beats,
                    speed,
                    bpm_sync,
                    next_mode,
                    chroma_key,
                    chroma_color,
                    chroma_tolerance,
                    chroma_softness,
                    chroma_spill,
                    restart,
                } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get_mut(deck) {
                            if let Some(o) = opacity {
                                d.opacity = o.clamp(0.0, 1.0);
                            }
                            if let Some(b) = blend {
                                d.blend = blend_from_name(Some(b.as_str()));
                            }
                            if let Some(f) = fade_beats {
                                d.fade_beats = f.clamp(0.0, 64.0);
                            }
                            if let Some(bs) = bpm_sync {
                                d.bpm_sync = bs.clamp(0.0, 256.0);
                            }
                            if let Some(m) = next_mode.as_deref() {
                                d.manual_next = match m {
                                    "shuffle" | "random" => ManualNextMode::Shuffle,
                                    _ => ManualNextMode::Sequential,
                                };
                            }
                            if let Some(k) = chroma_key {
                                d.chroma_key = k;
                            }
                            if let Some(c) = chroma_color {
                                d.chroma_color = [
                                    c[0].clamp(0.0, 1.0),
                                    c[1].clamp(0.0, 1.0),
                                    c[2].clamp(0.0, 1.0),
                                ];
                            }
                            if let Some(t) = chroma_tolerance {
                                d.chroma_tolerance = t.clamp(0.0, 1.0);
                            }
                            if let Some(s) = chroma_softness {
                                d.chroma_softness = s.clamp(0.0, 0.5);
                            }
                            if let Some(sp) = chroma_spill {
                                d.chroma_spill = sp.clamp(0.0, 1.0);
                            }
                            if let (Some(sp), Some(ci)) = (speed, d.active) {
                                if let Some(c) = d.clips.get_mut(ci) {
                                    c.speed = sp.clamp(0.05, 8.0);
                                }
                            }
                        }
                    }
                    if let Some(video) = self.videos.get_mut(deck).and_then(|s| s.as_mut()) {
                        if let Some(sp) = speed {
                            video.set_speed(sp, Instant::now());
                        }
                        if restart {
                            let _ = video.restart(Instant::now());
                        }
                    }
                }
                Command::SetClip {
                    deck,
                    clip,
                    speed,
                    weight,
                    in_point,
                    loop_mode,
                    snap_beats,
                    chroma_mode,
                    chroma_color,
                    chroma_tolerance,
                    chroma_softness,
                    chroma_spill,
                } => {
                    let mut is_active = false;
                    let mut deck_sync = 0.0;
                    let snap = snap_beats.map(|s| s.max(0.0));
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get_mut(deck) {
                            is_active = d.active == Some(clip);
                            deck_sync = d.bpm_sync;
                            if let Some(c) = d.clips.get_mut(clip) {
                                if let Some(sp) = speed {
                                    c.speed = sp.clamp(0.05, 8.0);
                                }
                                if let Some(w) = weight {
                                    c.weight = w.clamp(0.0, 100.0);
                                }
                                if let Some(inp) = in_point {
                                    c.in_point = inp.max(0.0);
                                }
                                if let Some(lm) = loop_mode.as_deref() {
                                    c.loop_mode = loop_mode_from_name(Some(lm));
                                }
                                if let Some(sb) = snap {
                                    c.snap_beats = sb;
                                }
                                if let Some(m) = chroma_mode.as_deref() {
                                    c.chroma_mode = ChromaMode::from_name(Some(m));
                                }
                                if let Some(col) = chroma_color {
                                    c.chroma_color = [
                                        col[0].clamp(0.0, 1.0),
                                        col[1].clamp(0.0, 1.0),
                                        col[2].clamp(0.0, 1.0),
                                    ];
                                }
                                if let Some(t) = chroma_tolerance {
                                    c.chroma_tolerance = t.clamp(0.0, 1.0);
                                }
                                if let Some(s) = chroma_softness {
                                    c.chroma_softness = s.clamp(0.0, 0.5);
                                }
                                if let Some(sp) = chroma_spill {
                                    c.chroma_spill = sp.clamp(0.0, 1.0);
                                }
                            }
                        }
                    }
                    if is_active {
                        let mut snapped_speed = None;
                        if let Some(video) = self.videos.get_mut(deck).and_then(|s| s.as_mut()) {
                            if let Some(sp) = speed {
                                video.set_speed(sp, Instant::now());
                            }
                            if let Some(inp) = in_point {
                                let _ = video.set_in_point(inp, Instant::now());
                            }
                            if let Some(lm) = loop_mode.as_deref() {
                                let _ = video.set_loop_mode(
                                    loop_mode_from_name(Some(lm)),
                                    Instant::now(),
                                );
                            }
                            let effective_snap = if deck_sync > 0.0 {
                                deck_sync
                            } else {
                                snap.unwrap_or(0.0)
                            };
                            if effective_snap > 0.0 && (snap.is_some() || deck_sync > 0.0) {
                                let target = Clip::snap_speed(
                                    video.duration(),
                                    video.in_point,
                                    effective_snap,
                                    clock_bpm,
                                );
                                video.set_speed(target, Instant::now());
                                snapped_speed = Some(target);
                            }
                        }
                        if let Some(target) = snapped_speed {
                            if let Some(mixer) = self.mixer.as_mut() {
                                if let Some(c) = mixer
                                    .decks
                                    .get_mut(deck)
                                    .and_then(|d| d.clips.get_mut(clip))
                                {
                                    c.speed = target;
                                }
                            }
                        }
                    }
                }
                Command::AddDeck {
                    folder,
                    every_beats,
                    blend,
                } => {
                    let cleaned = folder.trim().trim_matches(|c| c == '"' || c == '\'').trim();
                    let expanded = if let Some(rest) = cleaned.strip_prefix("~/") {
                        if let Ok(home) = std::env::var("HOME") {
                            format!("{home}/{rest}")
                        } else {
                            cleaned.to_string()
                        }
                    } else {
                        cleaned.to_string()
                    };
                    let p = PathBuf::from(&expanded);
                    let mut names = if p.is_file() {
                        vec![p.clone()]
                    } else {
                        scan_videos(&p)
                    };
                    names.sort();
                    if names.is_empty() {
                        log::warn!("UI add_deck: nem található videó itt: {}", p.display());
                    } else {
                        let beats = every_beats.unwrap_or(self.args.autopilot_beats);
                        let bl = blend_from_name(blend.as_deref());
                        let clips: Vec<Clip> = names
                            .iter()
                            .map(|f| {
                                Clip::new(
                                    f.file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("?")
                                        .to_string(),
                                    ClipSource::File(f.display().to_string()),
                                )
                            })
                            .collect();
                        let idx = self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
                        let name = ((b'A' + (idx as u8).min(25)) as char).to_string();
                        let mut deck = Deck::new(name.clone(), clips);
                        deck.blend = bl;
                        deck.opacity = if idx == 0 { 1.0 } else { 0.85 };
                        deck.autopilot = Autopilot {
                            mode: if beats > 0.0 {
                                AutopilotMode::Sequential
                            } else {
                                AutopilotMode::Off
                            },
                            every_beats: beats.max(1.0),
                            quantize: Quantize::Beat,
                            next_change: None,
                        };
                        let video = match VideoLayer::open(&device, &names[0], 1.0) {
                            Ok(v) => {
                                deck.active = Some(0);
                                Some(v)
                            }
                            Err(e) => {
                                log::error!("új deck videó hiba: {e:#}");
                                None
                            }
                        };
                        self.videos.push(video);
                        self.fading_videos.push(None);
                        self.deck_configs.push(crate::project::DeckConfig {
                            folder: expanded.clone(),
                            clips: deck
                                .clips
                                .iter()
                                .map(|c| crate::project::ClipConfig {
                                    name: c.name.clone(),
                                    speed: Some(c.speed),
                                    in_point: Some(c.in_point),
                                    loop_mode: Some(
                                        loop_mode_name(c.loop_mode).to_string(),
                                    ),
                                    snap_beats: (c.snap_beats > 0.0).then_some(c.snap_beats),
                                    weight: Some(c.weight),
                                    chroma_mode: Some(c.chroma_mode.as_str().to_string()),
                                    chroma_color: Some(c.chroma_color),
                                    chroma_tolerance: Some(c.chroma_tolerance),
                                    chroma_softness: Some(c.chroma_softness),
                                    chroma_spill: Some(c.chroma_spill),
                                })
                                .collect(),
                            start_clip: deck.active,
                            autopilot: Some(ap_name(deck.autopilot.mode).to_string()),
                            every_beats: Some(deck.autopilot.every_beats),
                            quantize: Some(deck.autopilot.quantize.as_str().to_string()),
                            blend: Some(blend_name(deck.blend).to_string()),
                            opacity: Some(deck.opacity),
                            fade_beats: Some(deck.fade_beats),
                            bpm_sync: Some(deck.bpm_sync),
                            manual_next: Some(manual_next_name(deck.manual_next).to_string()),
                            chroma_key: Some(deck.chroma_key),
                            chroma_color: Some(deck.chroma_color),
                            chroma_tolerance: Some(deck.chroma_tolerance),
                            chroma_softness: Some(deck.chroma_softness),
                            chroma_spill: Some(deck.chroma_spill),
                        });
                        if let Some(mixer) = self.mixer.as_mut() {
                            mixer.decks.push(deck);
                        } else {
                            let mut mixer = Mixer::new(0);
                            mixer.decks.push(deck);
                            self.mixer = Some(mixer);
                        }
                        self.layer_order.push();
                        self.sync_web_clips();
                        log::info!("UI új deck hozzáadva: {name} ({expanded})");
                    }
                }
                Command::AddClips {
                    deck: target_deck,
                    paths,
                    every_beats,
                    blend,
                } => {
                    let mut names: Vec<PathBuf> = Vec::new();
                    for raw in &paths {
                        let cleaned = raw.trim().trim_matches(|c| c == '"' || c == '\'').trim();
                        let expanded = if let Some(rest) = cleaned.strip_prefix("~/") {
                            if let Ok(home) = std::env::var("HOME") {
                                format!("{home}/{rest}")
                            } else {
                                cleaned.to_string()
                            }
                        } else {
                            cleaned.to_string()
                        };
                        let p = PathBuf::from(&expanded);
                        if p.is_file() {
                            names.push(p);
                        } else {
                            let mut sub = scan_videos(&p);
                            sub.sort();
                            names.extend(sub);
                        }
                    }
                    if names.is_empty() {
                        log::warn!(
                            "UI add_clips: nem található videófájl a kiválasztott elemekben"
                        );
                    } else {
                        let new_clips: Vec<Clip> = names
                            .iter()
                            .map(|f| {
                                Clip::new(
                                    f.file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("?")
                                        .to_string(),
                                    ClipSource::File(f.display().to_string()),
                                )
                            })
                            .collect();
                        let can_append = target_deck
                            .and_then(|di| self.mixer.as_ref().map(|m| di < m.decks.len()))
                            .unwrap_or(false);
                        if let (Some(di), true) = (target_deck, can_append) {
                            let need_open = self.videos.get(di).and_then(|v| v.as_ref()).is_none();
                            if let Some(mixer) = self.mixer.as_mut() {
                                if let Some(d) = mixer.decks.get_mut(di) {
                                    d.clips.extend(new_clips);
                                    if d.autopilot.mode != AutopilotMode::Off
                                        && d.autopilot.next_change.is_none()
                                    {
                                        d.autopilot.next_change =
                                            Some(beat as f64 + d.autopilot.every_beats);
                                    }
                                    if need_open {
                                        if let Ok(v) = VideoLayer::open(&device, &names[0], 1.0) {
                                            while self.videos.len() <= di {
                                                self.videos.push(None);
                                            }
                                            self.videos[di] = Some(v);
                                            d.active = Some(0);
                                        }
                                    }
                                    log::info!(
                                        "UI klipek hozzáadva a(z) {} deckhez ({} új klip)",
                                        d.name,
                                        names.len()
                                    );
                                }
                            }
                            self.sync_web_clips();
                        } else {
                            let beats = every_beats.unwrap_or(self.args.autopilot_beats);
                            let bl = blend_from_name(blend.as_deref());
                            let idx = self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
                            let name = ((b'A' + (idx as u8).min(25)) as char).to_string();
                            let mut deck = Deck::new(name.clone(), new_clips);
                            deck.blend = bl;
                            deck.opacity = if idx == 0 { 1.0 } else { 0.85 };
                            deck.autopilot = Autopilot {
                                mode: if beats > 0.0 {
                                    AutopilotMode::Sequential
                                } else {
                                    AutopilotMode::Off
                                },
                                every_beats: beats.max(1.0),
                                quantize: Quantize::Beat,
                                next_change: None,
                            };
                            let video = match VideoLayer::open(&device, &names[0], 1.0) {
                                Ok(v) => {
                                    deck.active = Some(0);
                                    Some(v)
                                }
                                Err(e) => {
                                    log::error!("új deck videó hiba: {e:#}");
                                    None
                                }
                            };
                            self.videos.push(video);
                            self.fading_videos.push(None);
                            let folder_label = names[0]
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| names[0].display().to_string());
                            self.deck_configs.push(crate::project::DeckConfig {
                                folder: folder_label.clone(),
                                clips: deck
                                    .clips
                                    .iter()
                                    .map(|c| crate::project::ClipConfig {
                                        name: c.name.clone(),
                                        speed: Some(c.speed),
                                        in_point: Some(c.in_point),
                                        loop_mode: Some(
                                            loop_mode_name(c.loop_mode).to_string(),
                                        ),
                                        snap_beats: (c.snap_beats > 0.0)
                                            .then_some(c.snap_beats),
                                        weight: Some(c.weight),
                                        chroma_mode: Some(
                                            c.chroma_mode.as_str().to_string(),
                                        ),
                                        chroma_color: Some(c.chroma_color),
                                        chroma_tolerance: Some(c.chroma_tolerance),
                                        chroma_softness: Some(c.chroma_softness),
                                        chroma_spill: Some(c.chroma_spill),
                                    })
                                    .collect(),
                                start_clip: deck.active,
                                autopilot: Some(ap_name(deck.autopilot.mode).to_string()),
                                every_beats: Some(deck.autopilot.every_beats),
                                quantize: Some(deck.autopilot.quantize.as_str().to_string()),
                                blend: Some(blend_name(deck.blend).to_string()),
                                opacity: Some(deck.opacity),
                                fade_beats: Some(deck.fade_beats),
                                bpm_sync: Some(deck.bpm_sync),
                            manual_next: Some(manual_next_name(deck.manual_next).to_string()),
                            chroma_key: Some(deck.chroma_key),
                            chroma_color: Some(deck.chroma_color),
                            chroma_tolerance: Some(deck.chroma_tolerance),
                            chroma_softness: Some(deck.chroma_softness),
                            chroma_spill: Some(deck.chroma_spill),
                            });
                            if let Some(mixer) = self.mixer.as_mut() {
                                mixer.decks.push(deck);
                            } else {
                                let mut mixer = Mixer::new(0);
                                mixer.decks.push(deck);
                                self.mixer = Some(mixer);
                            }
                            self.sync_web_clips();
                            log::info!(
                                "UI új deck létrehozva kiválasztott klipekből: {name} ({} klip)",
                                names.len()
                            );
                        }
                    }
                }
                Command::RemoveClip { deck, clip } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if let Some(d) = mixer.decks.get_mut(deck) {
                            if clip < d.clips.len() {
                                d.clips.remove(clip);
                                if d.clips.is_empty() {
                                    d.active = None;
                                    if let Some(slot) = self.videos.get_mut(deck) {
                                        *slot = None;
                                    }
                                } else if d.active == Some(clip) {
                                    let next_idx = clip.min(d.clips.len() - 1);
                                    d.active = Some(next_idx);
                                    if let ClipSource::File(p) = &d.clips[next_idx].source {
                                        if let Ok(v) = VideoLayer::open(
                                            &device,
                                            Path::new(p),
                                            d.clips[next_idx].speed,
                                        ) {
                                            if let Some(slot) = self.videos.get_mut(deck) {
                                                *slot = Some(v);
                                            }
                                        }
                                    }
                                } else if let Some(act) = d.active {
                                    if act > clip {
                                        d.active = Some(act - 1);
                                    }
                                }
                                self.sync_web_clips();
                            }
                        }
                    }
                }
                Command::RemoveDeck { deck } => {
                    if let Some(mixer) = self.mixer.as_mut() {
                        if deck < mixer.decks.len() {
                            mixer.decks.remove(deck);
                            if deck < self.videos.len() {
                                self.videos.remove(deck);
                            }
                            if deck < self.fading_videos.len() {
                                self.fading_videos.remove(deck);
                            }
                            if deck < self.deck_configs.len() {
                                self.deck_configs.remove(deck);
                            }
                            self.layer_order.remove(deck);
                            self.shader_target = match self.shader_target {
                                Some(t) if t == deck => None,
                                Some(t) if t > deck => Some(t - 1),
                                other => other,
                            };
                            for (i, d) in mixer.decks.iter_mut().enumerate() {
                                d.name = ((b'A' + (i as u8).min(25)) as char).to_string();
                            }
                            self.sync_web_clips();
                            log::info!("UI deck eltávolítva: #{deck}");
                        }
                    }
                }
                Command::ModAdd {
                    source,
                    target,
                    amount,
                    attack_ms,
                    hold_ms,
                    decay_ms,
                    sustain,
                    release_ms,
                    curve,
                    retrigger,
                } => {
                    let spec = format!("{source}->{target}:{amount}");
                    match parse_mod_route(&spec) {
                        Some(mut route) => {
                            if let Some(a) = attack_ms {
                                route.spec.attack_ms = a.max(0.0);
                            }
                            if let Some(h) = hold_ms {
                                route.spec.hold_ms = h.max(0.0);
                            }
                            if let Some(d) = decay_ms {
                                route.spec.decay_ms = d.max(1.0);
                            }
                            if let Some(s) = sustain {
                                route.spec.sustain = s.clamp(0.0, 1.0);
                            }
                            if let Some(r) = release_ms {
                                route.spec.release_ms = r.max(1.0);
                            }
                            if let Some(c) = curve.as_deref() {
                                route.spec.curve = curve_from_name(Some(c));
                            }
                            if let Some(r) = retrigger {
                                route.spec.retrigger = r;
                            }
                            self.matrix.add(route);
                            if self.matrix.uses_audio() {
                                self.ensure_audio();
                            }
                            log::info!("UI mod hozzáadva: {spec}");
                        }
                        None => log::warn!("értelmezhetetlen UI mod: {spec}"),
                    }
                }
                Command::ModUpdate {
                    index,
                    source,
                    target,
                    amount,
                    attack_ms,
                    hold_ms,
                    decay_ms,
                    sustain,
                    release_ms,
                    curve,
                    retrigger,
                } => {
                    if let Some(r) = self.matrix.routes.get_mut(index) {
                        if let Some(src) = source.as_deref() {
                            // A forrás cseréje az ismert parseren át, hogy a
                            // trigger/continuous mód is helyesen frissüljön.
                            let probe = format!(
                                "{src}->{}:{}",
                                mod_target_name(r.target),
                                r.amount
                            );
                            if let Some(parsed) = parse_mod_route(&probe) {
                                r.source = parsed.source;
                                r.mode = parsed.mode;
                                r.value = 0.0;
                                r.env = Default::default();
                            }
                        }
                        if let Some(dst) = target.as_deref() {
                            let probe = format!(
                                "{}->{}:{}",
                                mod_source_name(r.source),
                                dst,
                                r.amount
                            );
                            if let Some(parsed) = parse_mod_route(&probe) {
                                r.target = parsed.target;
                            }
                        }
                        if let Some(a) = amount {
                            r.amount = a;
                        }
                        if let Some(at) = attack_ms {
                            r.spec.attack_ms = at.max(0.0);
                        }
                        if let Some(ho) = hold_ms {
                            r.spec.hold_ms = ho.max(0.0);
                        }
                        if let Some(de) = decay_ms {
                            r.spec.decay_ms = de.max(1.0);
                        }
                        if let Some(su) = sustain {
                            r.spec.sustain = su.clamp(0.0, 1.0);
                        }
                        if let Some(rel) = release_ms {
                            r.spec.release_ms = rel.max(1.0);
                        }
                        if let Some(cu) = curve.as_deref() {
                            r.spec.curve = curve_from_name(Some(cu));
                        }
                        if let Some(re) = retrigger {
                            r.spec.retrigger = re;
                        }
                    }
                }
                Command::ModRemove { index } => {
                    if index < self.matrix.routes.len() {
                        self.matrix.routes.remove(index);
                        log::info!("UI mod törölve: #{index}");
                    }
                }
                Command::ModClear => {
                    self.matrix.routes.clear();
                    log::info!("UI összes mod törölve");
                }
                Command::SetRecord {
                    active,
                    path,
                    fps,
                    bitrate_mbps,
                } => {
                    self.finish_recording();
                    if let Some(p) = path {
                        if !p.trim().is_empty() {
                            self.rec_path = p.trim().to_string();
                        }
                    }
                    if let Some(b) = bitrate_mbps {
                        self.rec_bitrate_mbps = b.clamp(0.5, 100.0);
                    }
                    if active {
                        let rec_fps = fps.unwrap_or(60.0).clamp(15.0, 120.0);
                        let (rw, rh) = self
                            .renderer
                            .as_ref()
                            .map(|r| r.size())
                            .unwrap_or((self.args.width, self.args.height));
                        let bps = (self.rec_bitrate_mbps * 1_000_000.0) as u32;
                        if self.capture_audio_enabled {
                            self.prepare_capture_audio();
                        }
                        let audio_rate = self.capture_sample_rate();
                        let writer_result = if audio_rate > 0.0 {
                            dada_media::macos::writer::VideoWriter::new_with_audio(
                                Path::new(&self.rec_path),
                                rw,
                                rh,
                                rec_fps,
                                bps,
                                audio_rate,
                                2,
                                128_000,
                            )
                        } else {
                            dada_media::macos::writer::VideoWriter::new(
                                Path::new(&self.rec_path),
                                rw,
                                rh,
                                rec_fps,
                                bps,
                            )
                        };
                        match writer_result {
                            Ok(writer) => {
                                match dada_media::macos::metal::MetalTextureCache::new(&device) {
                                    Ok(cache) => {
                                        log::info!(
                                            "UI felvétel indítva: {} ({rw}x{rh} @ {rec_fps} fps, {:.1} Mbit/s, audio: {})",
                                            self.rec_path,
                                            self.rec_bitrate_mbps,
                                            if audio_rate > 0.0 { "igen" } else { "nem" }
                                        );
                                        self.rec_cache = Some(cache);
                                        self.recorder = Some(writer);
                                        self.rec_start = Instant::now();
                                        self.rec_next = Instant::now();
                                    }
                                    Err(e) => log::error!("felvétel cache hiba: {e:#}"),
                                }
                            }
                            Err(e) => log::error!("felvétel indítás hiba: {e:#}"),
                        }
                    }
                }
                Command::SetStream {
                    active,
                    url,
                    fps,
                    bitrate_mbps,
                } => {
                    self.stop_streaming();
                    if let Some(u) = url {
                        if !u.trim().is_empty() {
                            self.stream_url = u.trim().to_string();
                        }
                    }
                    if let Some(b) = bitrate_mbps {
                        self.stream_bitrate_mbps = b.clamp(0.5, 50.0);
                    }
                    if let Some(f) = fps {
                        self.enc_fps = (f.clamp(15, 120)) as f64;
                    }
                    if active {
                        let (ew, eh) = self
                            .renderer
                            .as_ref()
                            .map(|r| r.size())
                            .unwrap_or((self.args.width, self.args.height));
                        let bps = (self.stream_bitrate_mbps * 1_000_000.0) as u32;
                        let stream_fps = self.enc_fps as u32;
                        match dada_media::macos::encoder::VideoEncoder::new(ew, eh, bps, stream_fps)
                        {
                            Ok(encoder) => {
                                match dada_stream::RtmpPublisher::start(&self.stream_url) {
                                    Ok(publisher) => {
                                        match dada_media::macos::metal::MetalTextureCache::new(
                                            &device,
                                        ) {
                                            Ok(cache) => {
                                                log::info!(
                                                "UI RTMP stream indítva: {} ({ew}x{eh} @ {stream_fps} fps, {:.1} Mbit/s)",
                                                self.stream_url,
                                                self.stream_bitrate_mbps
                                            );
                                                self.encoder = Some(encoder);
                                                self.publisher = Some(publisher);
                                                self.enc_cache = Some(cache);
                                                self.enc_start = Instant::now();
                                                self.enc_next = Instant::now();
                                                self.sent_avcc = false;
                                                self.sent_audio_config = false;
                                                self.enc_captures = 0;
                                                self.audio_pcm.clear();
                                                self.audio_frames_written = 0;
                                                self.prepare_capture_audio();
                                                self.ensure_capture_aac();
                                            }
                                            Err(e) => log::error!("stream cache hiba: {e:#}"),
                                        }
                                    }
                                    Err(e) => log::error!("RTMP csatlakozás hiba: {e:#}"),
                                }
                            }
                            Err(e) => log::error!("stream encoder hiba: {e:#}"),
                        }
                    }
                }
                Command::SetOutput {
                    width,
                    height,
                    vsync,
                    fullscreen,
                } => {
                    if let (Some(w), Some(h)) = (width, height) {
                        let w = w.clamp(320, 7680);
                        let h = h.clamp(240, 4320);
                        self.args.width = w;
                        self.args.height = h;
                        if let Some(win) = self.window.as_ref() {
                            let _ = win.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
                        }
                        if let Some(r) = self.renderer.as_mut() {
                            r.resize(w, h);
                        }
                    }
                    if let Some(vs) = vsync {
                        self.args.vsync = vs;
                        if let Some(r) = self.renderer.as_mut() {
                            r.set_vsync(vs);
                        }
                    }
                    if let Some(fs) = fullscreen {
                        self.args.fullscreen = fs;
                        if let Some(win) = self.window.as_ref() {
                            win.set_fullscreen(if fs {
                                Some(Fullscreen::Borderless(None))
                            } else {
                                None
                            });
                        }
                    }
                }
                Command::SaveProject { path } => {
                    let target = path
                        .filter(|p| !p.trim().is_empty())
                        .or_else(|| self.args.project.clone())
                        .unwrap_or_else(|| "dada-project.toml".to_string());
                    let snapshot = project_snapshot(
                        &self.args,
                        &self.project,
                        self.mixer.as_ref(),
                        &self.matrix,
                        &self.deck_configs,
                        self.deck_folder.as_deref(),
                        &self.layer_order,
                        self.shader_target,
                        self.shader_alpha_from_shader,
                        self.shader_blend,
                    );
                    match snapshot.save(Path::new(&target)) {
                        Ok(()) => {
                            self.args.project = Some(target.clone());
                            self.refresh_catalog();
                            log::info!("projekt mentve: {target}");
                        }
                        Err(e) => log::error!("mentési hiba: {e:#}"),
                    }
                }
                Command::LoadProject { path } => {
                    let target = path.trim();
                    match crate::project::Project::load(Path::new(target)) {
                        Ok(proj) => {
                            self.args.project = Some(target.to_string());
                            self.project = proj.clone();
                            // Shader
                            if let Some(sh_path) = &proj.shader {
                                if let Some(r) = self.renderer.as_ref() {
                                    if let Ok(sh) = dada_fx::IsfShader::load(Path::new(sh_path)) {
                                        if let Ok(eff) = Effect::new(r.device(), r.format(), sh) {
                                            self.shader_mtime = std::fs::metadata(sh_path)
                                                .and_then(|m| m.modified())
                                                .ok();
                                            self.shader = Some(eff);
                                            self.args.shader = Some(sh_path.clone());
                                        }
                                    }
                                }
                            } else {
                                self.shader = None;
                                self.args.shader = None;
                            }
                            // Decks
                            self.videos.clear();
                            self.fading_videos.clear();
                            self.deck_configs = proj.decks.clone();
                            if !proj.decks.is_empty() {
                                let mut mixer = Mixer::new(0);
                                for (i, cfg) in proj.decks.iter().enumerate() {
                                    let p = PathBuf::from(cfg.folder.trim());
                                    let mut names = scan_videos(&p);
                                    names.sort();
                                    let mut clips: Vec<Clip> = names
                                        .iter()
                                        .map(|f| {
                                            Clip::new(
                                                f.file_name()
                                                    .and_then(|s| s.to_str())
                                                    .unwrap_or("?")
                                                    .to_string(),
                                                ClipSource::File(f.display().to_string()),
                                            )
                                        })
                                        .collect();
                                    for clip in clips.iter_mut() {
                                        let Some(cc) = cfg
                                            .clips
                                            .iter()
                                            .find(|cc| cc.name == clip.name)
                                        else {
                                            continue;
                                        };
                                        if let Some(v) = cc.speed {
                                            clip.speed = v.clamp(0.05, 8.0);
                                        }
                                        if let Some(v) = cc.in_point {
                                            clip.in_point = v.max(0.0);
                                        }
                                        if cc.loop_mode.is_some() {
                                            clip.loop_mode = loop_mode_from_name(
                                                cc.loop_mode.as_deref(),
                                            );
                                        }
                                        clip.snap_beats = cc.snap_beats.unwrap_or(0.0).max(0.0);
                                        if let Some(v) = cc.weight {
                                            clip.weight = v.clamp(0.0, 100.0);
                                        }
                                        clip.chroma_mode =
                                            ChromaMode::from_name(cc.chroma_mode.as_deref());
                                        if let Some(c) = cc.chroma_color {
                                            clip.chroma_color = [
                                                c[0].clamp(0.0, 1.0),
                                                c[1].clamp(0.0, 1.0),
                                                c[2].clamp(0.0, 1.0),
                                            ];
                                        }
                                        if let Some(t) = cc.chroma_tolerance {
                                            clip.chroma_tolerance = t.clamp(0.0, 1.0);
                                        }
                                        if let Some(s) = cc.chroma_softness {
                                            clip.chroma_softness = s.clamp(0.0, 0.5);
                                        }
                                        if let Some(sp) = cc.chroma_spill {
                                            clip.chroma_spill = sp.clamp(0.0, 1.0);
                                        }
                                    }
                                    let name = ((b'A' + (i as u8).min(25)) as char).to_string();
                                    let mut deck = Deck::new(name, clips);
                                    deck.blend = blend_from_name(cfg.blend.as_deref());
                                    deck.opacity = cfg
                                        .opacity
                                        .unwrap_or(if i == 0 { 1.0 } else { 0.85 })
                                        .clamp(0.0, 1.0);
                                    deck.fade_beats = cfg.fade_beats.unwrap_or(0.0);
                                    deck.bpm_sync =
                                        cfg.bpm_sync.unwrap_or(0.0).clamp(0.0, 256.0);
                                    deck.manual_next = match cfg.manual_next.as_deref() {
                                        Some("shuffle") | Some("random") => {
                                            ManualNextMode::Shuffle
                                        }
                                        _ => ManualNextMode::Sequential,
                                    };
                                    deck.chroma_key = cfg.chroma_key.unwrap_or(false);
                                    let cc = cfg.chroma_color.unwrap_or([0.0, 1.0, 0.0]);
                                    deck.chroma_color = [
                                        cc[0].clamp(0.0, 1.0),
                                        cc[1].clamp(0.0, 1.0),
                                        cc[2].clamp(0.0, 1.0),
                                    ];
                                    deck.chroma_tolerance =
                                        cfg.chroma_tolerance.unwrap_or(0.28).clamp(0.0, 1.0);
                                    deck.chroma_softness =
                                        cfg.chroma_softness.unwrap_or(0.12).clamp(0.0, 0.5);
                                    deck.chroma_spill =
                                        cfg.chroma_spill.unwrap_or(0.35).clamp(0.0, 1.0);
                                    let eb = cfg.every_beats.unwrap_or(self.args.autopilot_beats);
                                    deck.autopilot = Autopilot {
                                        mode: autopilot_from_name(cfg.autopilot.as_deref(), eb),
                                        every_beats: eb.max(0.25),
                                        quantize: quantize_from_name(cfg.quantize.as_deref()),
                                        next_change: None,
                                    };
                                    let start_idx = cfg.start_clip.unwrap_or(0);
                                    let mut video = names
                                        .get(start_idx)
                                        .or_else(|| names.first())
                                        .and_then(|file| VideoLayer::open(&device, file, 1.0).ok());
                                    if let Some(v) = video.as_mut() {
                                        let act = start_idx.min(names.len().saturating_sub(1));
                                        if let Some(c) = deck.clips.get(act) {
                                            v.in_point = c.in_point;
                                            v.loop_mode = c.loop_mode;
                                            if c.snap_beats <= 0.0 {
                                                v.set_speed(c.speed, Instant::now());
                                            }
                                            if c.in_point > 0.0 {
                                                let _ = v.restart(Instant::now());
                                            }
                                        }
                                        deck.active = Some(act);
                                    }
                                    self.videos.push(video);
                                    self.fading_videos.push(None);
                                    mixer.decks.push(deck);
                                }
                                self.mixer = Some(mixer);
                            } else {
                                self.mixer = None;
                            }
                            self.sync_web_clips();
                            // Rétegek / effekt célréteg a betöltött projektből.
                            let layout = self.project.clone();
                            self.apply_project_layout(&layout);
                            // Mods
                            self.matrix.routes.clear();
                            for m in &proj.mods {
                                if let Some(route) = parse_mod_route(&m.route) {
                                    self.matrix.add(route);
                                }
                            }
                            if self.matrix.uses_audio() || proj.audio.enabled == Some(true) {
                                self.args.audio_visual = proj.audio.enabled.unwrap_or(false);
                                self.args.audio_device = proj.audio.device.clone();
                                self.ensure_audio();
                            }
                            log::info!("UI projekt betöltve: {target}");
                        }
                        Err(e) => log::error!("projekt betöltési hiba ({target}): {e:#}"),
                    }
                }
            }
        }

        // A BPM-hez rögzített klip-hosszakat minden frame-en kövessük
        // (manuális tempó, tap, MIDI clock és Ableton Link esetén is).
        self.apply_bpm_snap(clock_bpm, now);

        // Deck/autopilot: kvantált clip-váltás (a parancsok és moduláció után).
        if self.mixer.is_some() {
            let bpm = clock_bpm;
            let dt_beats = (dt_ms / 1000.0) * bpm / 60.0;
            let events = self
                .mixer
                .as_mut()
                .map(|m| m.tick(beat as f64, dt_beats))
                .unwrap_or_default();
            for ev in events {
                match ev {
                    DeckEvent::Launch {
                        deck,
                        clip,
                        at_beat,
                        ..
                    } => {
                        let next = self
                            .mixer
                            .as_ref()
                            .and_then(|m| m.decks.get(deck))
                            .and_then(|d| {
                                d.clips.get(clip).and_then(|c| match &c.source {
                                    ClipSource::File(p) => Some((
                                        p.clone(),
                                        c.name.clone(),
                                        c.speed,
                                        c.in_point,
                                        c.loop_mode,
                                        c.snap_beats,
                                        d.bpm_sync,
                                        d.fade_beats > 0.0,
                                    )),
                                    _ => None,
                                })
                            });
                        if let Some((
                            path,
                            name,
                            speed,
                            in_point,
                            loop_mode,
                            clip_snap,
                            deck_sync,
                            has_fade,
                        )) = next
                        {
                            let switch_start = Instant::now();
                            match VideoLayer::open(&device, std::path::Path::new(&path), speed) {
                                Ok(mut video) => {
                                    video.in_point = in_point;
                                    video.loop_mode = loop_mode;
                                    let snap_beats = if deck_sync > 0.0 {
                                        deck_sync
                                    } else {
                                        clip_snap
                                    };
                                    if snap_beats > 0.0 {
                                        let target = Clip::snap_speed(
                                            video.duration(),
                                            in_point,
                                            snap_beats,
                                            bpm,
                                        );
                                        video.set_speed(target, Instant::now());
                                    }
                                    if in_point > 0.0 {
                                        let _ = video.restart(Instant::now());
                                    }
                                    log::info!(
                                        "deck {deck} váltás -> {name} (beat {at_beat:.2}) [{:.1} ms]",
                                        switch_start.elapsed().as_secs_f64() * 1000.0
                                    );
                                    while self.fading_videos.len() <= deck {
                                        self.fading_videos.push(None);
                                    }
                                    while self.videos.len() <= deck {
                                        self.videos.push(None);
                                    }
                                    if has_fade {
                                        self.fading_videos[deck] = self.videos[deck].take();
                                    } else {
                                        self.fading_videos[deck] = None;
                                    }
                                    self.videos[deck] = Some(video);
                                }
                                Err(e) => log::error!("a klip nem nyitható meg: {e:#}"),
                            }
                        }
                    }
                    DeckEvent::FadeDone { deck } => {
                        if let Some(slot) = self.fading_videos.get_mut(deck) {
                            *slot = None;
                        }
                    }
                }
            }
        }

        let now = Instant::now();
        // Deck-enkénti videó-rétegek előkészítése (tulajdonolt view handle).
        let mut fading_views: Vec<(usize, wgpu::TextureView)> = Vec::new();
        for (i, slot) in self.fading_videos.iter_mut().enumerate() {
            if let Some(video) = slot.as_mut() {
                match video.advance(&device, now) {
                    Ok(Some(view)) => fading_views.push((i, view)),
                    Ok(None) => {}
                    Err(e) => log::error!("fading videó hiba (deck {i}): {e:#}"),
                }
            }
        }
        let mut video_views: Vec<(usize, wgpu::TextureView)> = Vec::new();
        for (i, slot) in self.videos.iter_mut().enumerate() {
            if let Some(video) = slot.as_mut() {
                if let Some(base_speed) = self
                    .mixer
                    .as_ref()
                    .and_then(|m| m.decks.get(i))
                    .and_then(|d| d.active.and_then(|ci| d.clips.get(ci)))
                    .map(|c| c.speed)
                {
                    let mod_sp = self.mod_deck_speed.get(i).copied().unwrap_or(0.0) as f64;
                    if mod_sp.abs() > 1e-5
                        || self
                            .matrix
                            .routes
                            .iter()
                            .any(|r| r.target == ModTarget::DeckSpeed(i))
                    {
                        video.speed = (base_speed + mod_sp * 2.0).clamp(0.05, 8.0);
                    }
                }
                match video.advance(&device, now) {
                    Ok(Some(view)) => video_views.push((i, view)),
                    Ok(None) => {}
                    Err(e) => log::error!("videó hiba (deck {i}): {e:#}"),
                }
            }
        }

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        // Master Post-FX: alapérték + modulációs offsetek összeadása.
        let effective_fx = crate::renderer::MasterFxParams {
            flash: (self.master_fx.flash + self.mod_fx.flash).clamp(0.0, 1.0),
            invert: (self.master_fx.invert + self.mod_fx.invert).clamp(0.0, 1.0),
            hue: (self.master_fx.hue + self.mod_fx.hue).rem_euclid(1.0),
            saturation: (self.master_fx.saturation + self.mod_fx.saturation).clamp(0.0, 4.0),
            contrast: (self.master_fx.contrast + self.mod_fx.contrast).clamp(0.1, 4.0),
            rgb_split: (self.master_fx.rgb_split + self.mod_fx.rgb_split).clamp(0.0, 0.15),
            _pad0: 0.0,
            _pad1: 0.0,
        };
        renderer.set_master_fx(effective_fx);

        // A réteglista összeállítása alulról felfelé: videó, ISF shader, audio.
        let (w, h) = renderer.size();
        let mut layers: Vec<LayerRef> = Vec::new();

        let video_opacity = (self.video_opacity + self.mod_video).clamp(0.0, 1.0);
        let shader_opacity = (self.shader_opacity + self.mod_shader_op).clamp(0.0, 1.0);
        let audio_opacity = self.audio_opacity;
        let master = if self.blackout {
            0.0
        } else {
            self.master_opacity * master_scale(self.mod_master_active, self.mod_master)
        };
        renderer.set_proc_opacity(video_opacity * master);

        // 1. Videó-rétegek a beállított Z-sorrendben (alulról felfelé).
        //    Deck-enként előkészítjük a LayerRef listát (crossfade alatt két
        //    layer), majd a sorrendben fűzzük a kompozithoz.
        let deck_count = self.mixer.as_ref().map(|m| m.decks.len()).unwrap_or(0);
        let mut deck_layers = build_deck_layers(
            renderer,
            self.mixer.as_ref(),
            &self.videos,
            &self.fading_videos,
            &self.mod_deck_opacity,
            &video_views,
            &fading_views,
            video_opacity,
            master,
            deck_count,
        );
        let mut order: Vec<usize> = self
            .layer_order
            .as_slice()
            .iter()
            .copied()
            .filter(|i| *i < deck_count)
            .collect();
        for i in 0..deck_count {
            if !order.contains(&i) {
                order.push(i);
            }
        }

        // 2. ISF shader réteg előkészítése (hot-reload, modulációk).
        let mut effect = self.shader.as_mut();
        if let Some(effect) = effect.as_mut() {
            // Hot-reload: ha a fájl megváltozott, újrafordítjuk.
            if self.last_shader_check.elapsed() > Duration::from_millis(800) {
                self.last_shader_check = Instant::now();
                let mtime = std::fs::metadata(&effect.shader.path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                if mtime.is_some() && mtime != self.shader_mtime {
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
            effect.clear_mod_offsets();

            // MIDI moduláció: note -> envelope -> shader paraméter (nem-destruktív offset).
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
                        let span = (p.max - p.min).max(1.0);
                        effect.add_mod_offset(idx, v * span);
                    }
                }
            }
            // Modulációs mátrix -> shader paraméterek (relatív offset a slider alapértékre).
            for (idx, value) in &self.mod_shader {
                if let Some(p) = effect.shader.params.get(*idx) {
                    let span = (p.max - p.min).max(1.0);
                    effect.add_mod_offset(*idx, *value * span);
                }
            }
            effect.alpha_from_shader = self.shader_alpha_from_shader;
            effect.opacity = shader_opacity * master;
        }

        // A cél réteg akkor érvényes, ha az effekt be van töltve és a deck
        // indexe létezik. Ha a decknek épp nincs képe (pl. induláskor, még
        // nincs dekódolt frame), az effekt nem ugrik a kompozit tetejére:
        // ilyenkor egyszerűen kimarad ebben a frame-ben.
        let target_deck = if effect.is_some() {
            self.shader_target.filter(|t| *t < deck_count)
        } else {
            None
        };
        let target_visible = target_deck
            .map(|t| {
                video_views.iter().any(|(d, _)| *d == t)
                    || fading_views.iter().any(|(d, _)| *d == t)
            })
            .unwrap_or(false);

        // 3. Videó-rétegek beillesztése; a cél deck helyére az ISF effekt lép.
        for deck_ix in order {
            if target_deck == Some(deck_ix) {
                if !target_visible {
                    continue;
                }
                let Some(effect) = effect.as_mut() else {
                    continue;
                };
                // A bemenet a cél deck "tiszta" képe (a deck-opacity csak a
                // kimeneten érvényesül, így nem sötétül duplán).
                if effect.has_inputs() {
                    let snapshot = build_deck_snapshot(
                        renderer,
                        self.mixer.as_ref(),
                        &self.videos,
                        &self.fading_videos,
                        &video_views,
                        &fading_views,
                        deck_ix,
                    );
                    if let Some(view) = renderer.render_input(t, beat, &snapshot) {
                        effect.set_input(renderer.device(), &view);
                    }
                }
                let deck_op = self
                    .mixer
                    .as_ref()
                    .and_then(|m| m.decks.get(deck_ix))
                    .map(|d| {
                        (d.opacity
                            + self.mod_deck_opacity.get(deck_ix).copied().unwrap_or(0.0))
                        .clamp(0.0, 1.0)
                    })
                    .unwrap_or(1.0);
                // A cél deck rétegét cseréli le, ezért a deck-opacity és a
                // globális videó-opacity is érvényesül rajta.
                effect.opacity = shader_opacity * master * video_opacity * deck_op;
                effect.update(
                    renderer.queue(),
                    [w as f32, h as f32],
                    t,
                    dt_ms as f32 / 1000.0,
                );
                layers.push(LayerRef {
                    pipeline: effect.pipeline(self.shader_blend).clone(),
                    bind_group: effect.bind_group().clone(),
                });
                // A cél deck nyers videó-rétegét az effekt kiváltja.
                continue;
            }
            layers.extend(std::mem::take(&mut deck_layers[deck_ix]));
        }

        // 4. ISF shader réteg legfelül (ha nincs cél rétege): a teljes kompozit a bemenet.
        if target_deck.is_none() {
            if let Some(effect) = effect.as_mut() {
                if effect.has_inputs() {
                    let below = layers.clone();
                    if let Some(view) = renderer.render_input(t, beat, &below) {
                        effect.set_input(renderer.device(), &view);
                    }
                }
                effect.update(
                    renderer.queue(),
                    [w as f32, h as f32],
                    t,
                    dt_ms as f32 / 1000.0,
                );
                layers.push(LayerRef {
                    pipeline: effect.pipeline(self.shader_blend).clone(),
                    bind_group: effect.bind_group().clone(),
                });
            }
        }

        // 5. Audio-vizuál réteg.
        if let Some((_, Some(vis))) = self.audio.as_mut() {
            vis.opacity = ((audio_opacity + self.mod_audio).clamp(0.0, 1.0)) * master;
            vis.update(renderer.queue(), &self.audio_snapshot, t, w, h);
            layers.push(LayerRef {
                pipeline: vis.pipeline(self.audio_blend).clone(),
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
                            self.rec_cache.as_ref().map(|c| {
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
                                        let pts = self.enc_start.elapsed().as_secs_f64() * 1000.0;
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
            let midi_msgs = self.midi.as_ref().map(|m| m.messages_in).unwrap_or(0);
            let (shader_name, shader_path, shader_meta, params) = match self.shader.as_ref() {
                Some(effect) => {
                    let list: Vec<serde_json::Value> = effect
                        .shader
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let vals = effect.values.get(i).copied().unwrap_or(p.default);
                            let mod_off = effect.mod_offsets.get(i).copied().unwrap_or(0.0);
                            let eff_val = match p.kind {
                                dada_fx::isf::ParamKind::Float => {
                                    (vals[0] + mod_off).clamp(p.min, p.max)
                                }
                                dada_fx::isf::ParamKind::Bool => {
                                    if vals[0] + mod_off >= 0.5 {
                                        1.0
                                    } else {
                                        0.0
                                    }
                                }
                                dada_fx::isf::ParamKind::Long => {
                                    (vals[0] + mod_off).round().clamp(p.min, p.max)
                                }
                                dada_fx::isf::ParamKind::Event => {
                                    if vals[0] + mod_off >= 0.5 {
                                        1.0
                                    } else {
                                        0.0
                                    }
                                }
                                _ => vals[0],
                            };
                            let kind_str = match p.kind {
                                dada_fx::isf::ParamKind::Float => "float",
                                dada_fx::isf::ParamKind::Bool => "bool",
                                dada_fx::isf::ParamKind::Long => "long",
                                dada_fx::isf::ParamKind::Event => "event",
                                dada_fx::isf::ParamKind::Point2D => "point2D",
                                dada_fx::isf::ParamKind::Color => "color",
                            };
                            serde_json::json!({
                                "name": p.name,
                                "kind": kind_str,
                                "value": vals[0],
                                "effective_value": eff_val,
                                "mod_offset": mod_off,
                                "values": vals,
                                "enum_values": p.values,
                                "default": p.default,
                                "min": p.min,
                                "max": p.max,
                                "labels": p.labels,
                            })
                        })
                        .collect();
                    let meta = serde_json::json!({
                        "credit": effect.shader.metadata.credit,
                        "categories": effect.shader.metadata.categories,
                        "has_inputs": effect.has_inputs(),
                        "frames": effect.frames,
                    });
                    (
                        effect.shader.metadata.description.clone(),
                        effect.shader.path.display().to_string(),
                        meta,
                        list,
                    )
                }
                None => (
                    String::new(),
                    String::new(),
                    serde_json::json!({}),
                    Vec::new(),
                ),
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
                            .enumerate()
                            .map(|(ci, c)| {
                                let path = match &c.source {
                                    ClipSource::File(p) => p.clone(),
                                    _ => String::new(),
                                };
                                serde_json::json!({
                                    "index": ci,
                                    "name": c.name,
                                    "path": path,
                                    "speed": c.speed,
                                    "weight": c.weight,
                                    "in_point": c.in_point,
                                    "loop_mode": loop_mode_name(c.loop_mode),
                                    "snap_beats": c.snap_beats,
                                    "chroma_mode": c.chroma_mode.as_str(),
                                    "chroma_color": c.chroma_color,
                                    "chroma_tolerance": c.chroma_tolerance,
                                    "chroma_softness": c.chroma_softness,
                                    "chroma_spill": c.chroma_spill,
                                })
                            })
                            .collect();
                        let vinfo = self.videos.get(i).and_then(|v| v.as_ref()).map(|v| {
                            serde_json::json!({
                                "width": v.size.0,
                                "height": v.size.1,
                                "fps": v.nominal_fps(),
                                "pts": v.last_pts,
                                "speed": v.speed,
                                "in_point": v.in_point,
                                "loop_mode": loop_mode_name(v.loop_mode),
                                "duration": v.duration(),
                                "frames_shown": v.frames_shown,
                            })
                        });
                        let folder = self
                            .deck_configs
                            .get(i)
                            .map(|c| c.folder.clone())
                            .unwrap_or_default();
                        serde_json::json!({
                            "index": i,
                            "z": self.layer_order.position(i).unwrap_or(i),
                            "name": d.name,
                            "folder": folder,
                            "active": d.active,
                            "current": d.active,
                            "queued": d.queued.map(|q| q.clip),
                            "queued_at_beat": d.queued.map(|q| q.at_beat),
                            "opacity": d.opacity,
                            "blend": blend_name(d.blend),
                            "fade_beats": d.fade_beats,
                            "bpm_sync": d.bpm_sync,
                            "manual_next": manual_next_name(d.manual_next),
                            "chroma_key": d.chroma_key,
                            "chroma_color": d.chroma_color,
                            "chroma_tolerance": d.chroma_tolerance,
                            "chroma_softness": d.chroma_softness,
                            "chroma_spill": d.chroma_spill,
                            "fade_progress": d.fade_progress(),
                            "launched_count": d.launched_count,
                            "has_video": vinfo.is_some(),
                            "video": vinfo,
                            "autopilot": {
                                "mode": ap_name(d.autopilot.mode),
                                "every_beats": d.autopilot.every_beats,
                                "quantize": d.autopilot.quantize.as_str(),
                                "next_change": d.autopilot.next_change,
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
                        "contribution": r.contribution(),
                        "hits": r.env.triggers,
                        "attack_ms": r.spec.attack_ms,
                        "hold_ms": r.spec.hold_ms,
                        "decay_ms": r.spec.decay_ms,
                        "sustain": r.spec.sustain,
                        "release_ms": r.spec.release_ms,
                        "curve": curve_name(r.spec.curve),
                        "retrigger": r.spec.retrigger,
                    })
                })
                .collect();
            let audio_json = {
                let st = &self.audio_snapshot.state;
                let (
                    enabled,
                    visual,
                    source,
                    sr,
                    err,
                    selected_ch,
                    selected_chs,
                    selected_spec,
                    num_ch,
                ) = match self.audio.as_ref() {
                    Some((eng, vis)) => {
                        let src = match eng.kind() {
                            dada_audio::SourceKind::Synthetic => "synthetic".to_string(),
                            dada_audio::SourceKind::Input { device } => {
                                device.clone().unwrap_or_else(|| "default".to_string())
                            }
                        };
                        (
                            true,
                            vis.is_some(),
                            src,
                            eng.sample_rate(),
                            eng.error(),
                            eng.selected_channel(),
                            eng.selected_channels().to_vec(),
                            eng.selected_channel_spec(),
                            self.audio_snapshot.num_channels.max(eng.num_channels()),
                        )
                    }
                    None => (
                        false,
                        false,
                        "synthetic".to_string(),
                        48000.0,
                        None,
                        self.audio_selected_channels.first().copied(),
                        self.audio_selected_channels.clone(),
                        AudioEngine::format_channel_spec(&self.audio_selected_channels),
                        0,
                    ),
                };
                let channels_json: Vec<serde_json::Value> = (0..num_ch
                    .min(dada_audio::MAX_CHANNELS))
                    .map(|ch| {
                        let cm = self
                            .audio_snapshot
                            .channels
                            .get(ch)
                            .copied()
                            .unwrap_or_default();
                        serde_json::json!({
                            "channel": ch + 1,
                            "index": ch,
                            "rms": cm.rms,
                            "peak": cm.peak,
                            "low": cm.low,
                            "mid": cm.mid,
                            "high": cm.high,
                            "onset": cm.onset,
                        })
                    })
                    .collect();
                let devices_detailed: Vec<serde_json::Value> = self
                    .available_audio_devices_detailed
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "name": d.name,
                            "channels": d.channels,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "enabled": enabled,
                    "visual": visual,
                    "source": source,
                    "selected_channel": selected_ch,
                    "selected_channels": selected_chs,
                    "selected_channel_spec": selected_spec,
                    "input_channels": num_ch,
                    "channels": channels_json,
                    "devices_detailed": devices_detailed,
                    "sample_rate": sr,
                    "capture_enabled": self.capture_audio_enabled,
                    "capture_source": self.capture_audio_source,
                    "capture_channel_spec": self.capture_audio_channel_spec,
                    "capture_sample_rate": if self.capture_audio_source == "analysis" {
                        self.audio
                            .as_ref()
                            .map(|(e, _)| e.sample_rate() as f64)
                            .unwrap_or(0.0)
                    } else {
                        self.capture_audio
                            .as_ref()
                            .map(|e| e.sample_rate() as f64)
                            .unwrap_or(0.0)
                    },
                    "capture_active": self.capture_audio.is_some()
                        || (self.capture_audio_source == "analysis" && self.audio.is_some()),
                    "recording": self.recorder.is_some(),
                    "streaming": self.publisher.is_some(),
                    "opacity": self.audio_opacity,
                    "blend": blend_name(self.audio_blend),
                    "rms": st.rms,
                    "peak": st.peak,
                    "flux": st.flux,
                    "onset": st.onset,
                    "bands": st.band_env,
                    "sub": st.band_env.get(0).copied().unwrap_or(0.0),
                    "bass": st.band_env.get(1).copied().unwrap_or(0.0),
                    "low_mid": st.band_env.get(2).copied().unwrap_or(0.0),
                    "mid": st.band_env.get(3).copied().unwrap_or(0.0),
                    "high_mid": st.band_env.get(4).copied().unwrap_or(0.0),
                    "high": st.band_env.get(5).copied().unwrap_or(0.0),
                    "error": err,
                })
            };
            let midi_json = match self.midi.as_ref() {
                Some(m) => {
                    let ccs: Vec<serde_json::Value> = self
                        .last_ccs
                        .iter()
                        .map(|(k, v)| serde_json::json!({"cc": k, "value": v}))
                        .collect();
                    let last_ev =
                        m.last_event
                            .as_ref()
                            .and_then(|(port_name, msg, seq)| match *msg {
                                dada_midi::MidiMessage::NoteOn {
                                    channel,
                                    note,
                                    velocity,
                                } => Some(serde_json::json!({
                                    "port_name": port_name,
                                    "kind": "note_on",
                                    "channel": channel,
                                    "number": note,
                                    "value": velocity,
                                    "mod_source": format!("note:{note}"),
                                    "seq": seq,
                                })),
                                dada_midi::MidiMessage::ControlChange { channel, cc, value } => {
                                    Some(serde_json::json!({
                                        "port_name": port_name,
                                        "kind": "cc",
                                        "channel": channel,
                                        "number": cc,
                                        "value": value,
                                        "mod_source": format!("cc:{cc}"),
                                        "seq": seq,
                                    }))
                                }
                                _ => None,
                            });
                    let enabled_list: Vec<String> = match &m.enabled_ports {
                        Some(set) => set.iter().cloned().collect(),
                        None => self.available_midi_ports.clone(),
                    };
                    serde_json::json!({
                        "active": true,
                        "sources": m.sources,
                        "ports": m.sources,
                        "available_ports": self.available_midi_ports,
                        "enabled_ports": enabled_list,
                        "all_ports_enabled": m.enabled_ports.is_none(),
                        "last_event": last_ev,
                        "messages": m.messages_in,
                        "messages_in": m.messages_in,
                        "clock_running": m.is_locked(),
                        "clock_locked": m.is_locked(),
                        "clock_bpm": m.bpm(),
                        "clock_ticks": m.clock_ticks,
                        "clock_err_ms": m.clock.last_error_ms(),
                        "clock_error_ms": m.clock.last_error_ms(),
                        "dropped": m.dropped_packets(),
                        "midi_visual": self.args.midi_visual,
                        "ccs": ccs,
                    })
                }
                None => serde_json::json!({
                    "active": false,
                    "sources": [],
                    "ports": [],
                    "available_ports": self.available_midi_ports,
                    "enabled_ports": self.available_midi_ports,
                    "all_ports_enabled": true,
                    "last_event": serde_json::Value::Null,
                    "messages": 0,
                    "messages_in": 0,
                    "clock_running": false,
                    "clock_locked": false,
                    "clock_bpm": 120.0,
                    "clock_ticks": 0,
                    "clock_err_ms": 0.0,
                    "clock_error_ms": 0.0,
                    "dropped": 0,
                    "midi_visual": self.args.midi_visual,
                    "ccs": [],
                }),
            };
            let record_json = serde_json::json!({
                "active": self.recorder.is_some(),
                "path": self.rec_path,
                "frames": self.recorder.as_ref().map(|r| r.frames).unwrap_or(0),
                "fps": self.recorder.as_ref().map(|r| r.fps).unwrap_or(60.0),
                "bitrate_mbps": self.rec_bitrate_mbps,
                "seconds": if self.recorder.is_some() { self.rec_start.elapsed().as_secs_f64() } else { 0.0 },
                "elapsed_secs": if self.recorder.is_some() { self.rec_start.elapsed().as_secs_f64() } else { 0.0 },
            });
            let stream_stats = self.publisher.as_ref().map(|p| p.stats());
            let stream_json = serde_json::json!({
                "active": self.publisher.is_some(),
                "url": self.stream_url,
                "fps": self.enc_fps,
                "bitrate_mbps": self.stream_bitrate_mbps,
                "captures": self.enc_captures,
                "sent": stream_stats.as_ref().map(|s| s.sent).unwrap_or(0),
                "dropped": stream_stats.as_ref().map(|s| s.dropped).unwrap_or(0),
                "bytes": stream_stats.as_ref().map(|s| s.bytes).unwrap_or(0),
                "kb": stream_stats.as_ref().map(|s| s.bytes / 1024).unwrap_or(0),
                "elapsed_secs": if self.publisher.is_some() { self.enc_start.elapsed().as_secs_f64() } else { 0.0 },
            });
            let perf_summary = self.stats.summary();
            let (rw, rh) = renderer.size();
            let system_json = serde_json::json!({
                "gpu": renderer.adapter_name(),
                "backend": "Metal",
                "width": rw,
                "height": rh,
                "vsync": self.args.vsync,
                "fullscreen": self.args.fullscreen,
                "cpu_percent": self.proc_stats.cpu_percent,
                "cpu_pct": self.proc_stats.cpu_percent,
                "rss_mb": self.proc_stats.rss_mb,
                "frame_avg_ms": perf_summary.map(|s| s.avg_ms).unwrap_or(0.0),
                "frame_p95_ms": perf_summary.map(|s| s.p95_ms).unwrap_or(0.0),
                "avg_ms": perf_summary.map(|s| s.avg_ms).unwrap_or(0.0),
                "p50_ms": perf_summary.map(|s| s.p50_ms).unwrap_or(0.0),
                "p95_ms": perf_summary.map(|s| s.p95_ms).unwrap_or(0.0),
                "p99_ms": perf_summary.map(|s| s.p99_ms).unwrap_or(0.0),
                "max_ms": perf_summary.map(|s| s.max_ms).unwrap_or(0.0),
                "over_budget": perf_summary.map(|s| s.over_budget).unwrap_or(0),
                "project": self.args.project.clone().unwrap_or_else(|| "dada-project.toml".to_string()),
            });
            let clock_json = serde_json::json!({
                "source": clock_source,
                "bpm": clock_bpm,
                "beats": beat,
                "playing": self.transport.is_playing(),
                "link_enabled": self.link.is_some(),
                "link_peers": self.link.as_ref().map(|l| l.peers()).unwrap_or(0),
                "link_packets": self.link.as_ref().map(|l| l.packets()).unwrap_or(0),
            });
            let fx_json = serde_json::json!({
                "flash": self.master_fx.flash,
                "invert": self.master_fx.invert,
                "hue": self.master_fx.hue,
                "saturation": self.master_fx.saturation,
                "contrast": self.master_fx.contrast,
                "rgb_split": self.master_fx.rgb_split,
                "effective": {
                    "flash": effective_fx.flash,
                    "invert": effective_fx.invert,
                    "hue": effective_fx.hue,
                    "saturation": effective_fx.saturation,
                    "contrast": effective_fx.contrast,
                    "rgb_split": effective_fx.rgb_split,
                }
            });
            let state = serde_json::json!({
                "fps": self.last_fps,
                "bpm": clock_bpm,
                "beats": beat,
                "clock": clock_json,
                "clock_source": clock_source,
                "transport_playing": self.transport.is_playing(),
                "manual_bpm": self.transport.bpm(),
                "midi_messages": midi_msgs,
                "layer": layer_name,
                "master": {
                    "opacity": self.master_opacity,
                    "blackout": self.blackout,
                    "video_opacity": self.video_opacity,
                    "shader_opacity": self.shader_opacity,
                    "shader_blend": blend_name(self.shader_blend),
                    "shader_alpha": if self.shader_alpha_from_shader {
                        "shader"
                    } else {
                        "coverage"
                    },
                    "shader_target": self.shader_target,
                    "audio_opacity": self.audio_opacity,
                    "audio_blend": blend_name(self.audio_blend),
                    "mod_master": if self.mod_master_active { self.mod_master } else { 0.0 },
                },
                "fx": fx_json,
                "layer_order": self.layer_order.as_slice(),
                "shader": shader_name,
                "shader_path": shader_path,
                "shader_meta": shader_meta,
                "params": params,
                "link": self.link.as_ref().map(|l| l.is_active()).unwrap_or(false),
                "link_enabled": self.link.is_some(),
                "link_peers": self.link.as_ref().map(|l| l.peers()).unwrap_or(0),
                "link_packets": self.link.as_ref().map(|l| l.packets()).unwrap_or(0),
                "decks": decks_json,
                "mods": mods_json,
                "audio": audio_json,
                "midi": midi_json,
                "record": record_json,
                "stream": stream_json,
                "system": system_json,
                "catalog": {
                    "shaders": self.available_shaders,
                    "audio_devices": self.available_audio_devices,
                    "midi_ports": self.available_midi_ports,
                    "projects": self.available_projects,
                    "media": self.available_media,
                },
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
        println!(
            "BENCH ÖSSZEGZÉS — {}x{} vsync={} futás={:.2}s",
            self.args.width, self.args.height, self.args.vsync, elapsed
        );
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

/// Egy deck adott klipjének effektív chroma beállításai (a videó-rétegre).
///
/// A deck szintű chroma az alapértelmezés; a klip chroma_mode-ja felülírhatja
/// (custom = saját kulcs, off = nincs kulcs ezen a klipen).
fn clip_chroma(mixer: Option<&Mixer>, deck: usize, clip: Option<usize>) -> (bool, [f32; 3], f32, f32, f32) {
    mixer
        .and_then(|m| m.decks.get(deck))
        .map(|d| d.chroma_for_clip(clip))
        .unwrap_or((false, [0.0, 1.0, 0.0], 0.28, 0.12, 0.35))
}

/// Deck-enkénti videó-rétegek (LayerRef) előkészítése a kompozithoz.
///
/// A deck saját blend/opacity-ja, chroma key-e és a crossfade súlya érvényesül;
/// a sorrendet a hívó adja (a LayerOrder szerint fűzi össze a listát).
#[allow(clippy::too_many_arguments)]
fn build_deck_layers(
    renderer: &Renderer,
    mixer: Option<&Mixer>,
    videos: &[Option<VideoLayer>],
    fading_videos: &[Option<VideoLayer>],
    mod_deck_opacity: &[f32],
    video_views: &[(usize, wgpu::TextureView)],
    fading_views: &[(usize, wgpu::TextureView)],
    video_opacity: f32,
    master: f32,
    deck_count: usize,
) -> Vec<Vec<LayerRef>> {
    let mut out: Vec<Vec<LayerRef>> = vec![Vec::new(); deck_count];
    for (deck_index, view) in video_views {
        let Some(slot) = out.get_mut(*deck_index) else {
            continue;
        };
        let (blend, opacity, fade_prog) = mixer
            .and_then(|m| m.decks.get(*deck_index))
            .map(|d| (d.blend, d.opacity, d.fade_progress()))
            .unwrap_or((BlendMode::Normal, 1.0, 1.0));
        let mod_delta = mod_deck_opacity.get(*deck_index).copied().unwrap_or(0.0);
        let base_op = ((opacity + mod_delta).clamp(0.0, 1.0)) * video_opacity * master;
        // A kifutó és a bejövő klip külön effektív chroma beállítást kaphat
        // (klipp-szintű felülírás), ezért a klipindexekhez kötjük.
        let (fading_clip, active_clip) = mixer
            .and_then(|m| m.decks.get(*deck_index))
            .map(|d| (d.fading_from, d.active))
            .unwrap_or((None, None));
        let fading_chroma = clip_chroma(mixer, *deck_index, fading_clip);
        let current_chroma = clip_chroma(mixer, *deck_index, active_clip);

        // Ha crossfade zajlik, az előző videót (1 - fade_prog) súllyal rajzoljuk alá.
        if fade_prog < 1.0 {
            if let Some((_, fview)) = fading_views.iter().find(|(di, _)| di == deck_index) {
                if let Some(fvideo) = fading_videos.get(*deck_index).and_then(|s| s.as_ref()) {
                    fvideo.set_chroma(
                        renderer.queue(),
                        fading_chroma.0,
                        fading_chroma.1,
                        fading_chroma.2,
                        fading_chroma.3,
                        fading_chroma.4,
                    );
                    fvideo.set_opacity(renderer.queue(), base_op * (1.0 - fade_prog));
                    let bg = renderer.make_video_bind_group(fview, fvideo.params_buffer());
                    slot.push(LayerRef {
                        pipeline: renderer.video_pipeline(blend).clone(),
                        bind_group: bg,
                    });
                }
            }
        }

        if let Some(video) = videos.get(*deck_index).and_then(|s| s.as_ref()) {
            video.set_chroma(
                renderer.queue(),
                current_chroma.0,
                current_chroma.1,
                current_chroma.2,
                current_chroma.3,
                current_chroma.4,
            );
            video.set_opacity(renderer.queue(), base_op * fade_prog);
            let bind_group = renderer.make_video_bind_group(view, video.params_buffer());
            slot.push(LayerRef {
                pipeline: renderer.video_pipeline(blend).clone(),
                bind_group,
            });
        }
    }
    out
}

/// Egy deck "tiszta" képe az ISF effekt képi bemenetéhez: Normal blend, a
/// crossfade súlyát megtartva, de a deck-opacity és a moduláció nélkül (az
/// csak a kimeneti rétegen érvényesül, így nem sötétül duplán).
fn build_deck_snapshot(
    renderer: &Renderer,
    mixer: Option<&Mixer>,
    videos: &[Option<VideoLayer>],
    fading_videos: &[Option<VideoLayer>],
    video_views: &[(usize, wgpu::TextureView)],
    fading_views: &[(usize, wgpu::TextureView)],
    deck_index: usize,
) -> Vec<LayerRef> {
    let (fade_prog, fading_clip, active_clip) = mixer
        .and_then(|m| m.decks.get(deck_index))
        .map(|d| (d.fade_progress(), d.fading_from, d.active))
        .unwrap_or((1.0, None, None));
    let fading_chroma = clip_chroma(mixer, deck_index, fading_clip);
    let current_chroma = clip_chroma(mixer, deck_index, active_clip);
    let mut out = Vec::new();
    if fade_prog < 1.0 {
        if let Some((_, fview)) = fading_views.iter().find(|(di, _)| *di == deck_index) {
            if let Some(fvideo) = fading_videos.get(deck_index).and_then(|s| s.as_ref()) {
                fvideo.set_chroma(
                    renderer.queue(),
                    fading_chroma.0,
                    fading_chroma.1,
                    fading_chroma.2,
                    fading_chroma.3,
                    fading_chroma.4,
                );
                fvideo.set_opacity(renderer.queue(), 1.0 - fade_prog);
                out.push(LayerRef {
                    pipeline: renderer.video_pipeline(BlendMode::Normal).clone(),
                    bind_group: renderer.make_video_bind_group(fview, fvideo.params_buffer()),
                });
            }
        }
    }
    if let Some((_, view)) = video_views.iter().find(|(di, _)| *di == deck_index) {
        if let Some(video) = videos.get(deck_index).and_then(|s| s.as_ref()) {
            video.set_chroma(
                renderer.queue(),
                current_chroma.0,
                current_chroma.1,
                current_chroma.2,
                current_chroma.3,
                current_chroma.4,
            );
            video.set_opacity(renderer.queue(), fade_prog);
            out.push(LayerRef {
                pipeline: renderer.video_pipeline(BlendMode::Normal).clone(),
                bind_group: renderer.make_video_bind_group(view, video.params_buffer()),
            });
        }
    }
    out
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

fn loop_mode_name(m: LoopMode) -> &'static str {
    match m {
        LoopMode::Loop => "loop",
        LoopMode::Once => "once",
        LoopMode::PingPong => "pingpong",
    }
}

fn loop_mode_from_name(s: Option<&str>) -> LoopMode {
    match s {
        Some("once") => LoopMode::Once,
        Some("pingpong") => LoopMode::PingPong,
        _ => LoopMode::Loop,
    }
}

fn curve_name(c: dada_core::modulation::Curve) -> &'static str {
    match c {
        dada_core::modulation::Curve::Linear => "linear",
        dada_core::modulation::Curve::Exponential => "exponential",
    }
}

fn curve_from_name(s: Option<&str>) -> dada_core::modulation::Curve {
    match s {
        Some("linear") => dada_core::modulation::Curve::Linear,
        _ => dada_core::modulation::Curve::Exponential,
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

fn manual_next_name(m: ManualNextMode) -> &'static str {
    match m {
        ManualNextMode::Sequential => "sequential",
        ManualNextMode::Shuffle => "shuffle",
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
        ModSource::AudioChannelRms(ch) => format!("audio.ch.{}.rms", ch + 1),
        ModSource::AudioChannelOnset(ch) => format!("audio.ch.{}.onset", ch + 1),
        ModSource::AudioChannelLow(ch) => format!("audio.ch.{}.low", ch + 1),
        ModSource::AudioChannelMid(ch) => format!("audio.ch.{}.mid", ch + 1),
        ModSource::AudioChannelHigh(ch) => format!("audio.ch.{}.high", ch + 1),
        ModSource::AudioPairRms(a, b) => format!("audio.ch.{}-{}.rms", a + 1, b + 1),
        ModSource::AudioPairOnset(a, b) => format!("audio.ch.{}-{}.onset", a + 1, b + 1),
        ModSource::AudioPairLow(a, b) => format!("audio.ch.{}-{}.low", a + 1, b + 1),
        ModSource::AudioPairMid(a, b) => format!("audio.ch.{}-{}.mid", a + 1, b + 1),
        ModSource::AudioPairHigh(a, b) => format!("audio.ch.{}-{}.high", a + 1, b + 1),
        ModSource::Lfo { period_beats } => format!("lfo:{period_beats}"),
        ModSource::ClockPhase => "clock.phase".into(),
    }
}

fn mod_target_name(t: ModTarget) -> String {
    match t {
        ModTarget::DeckOpacity(i) => format!("deck.opacity.{i}"),
        ModTarget::DeckSpeed(i) => format!("deck.speed.{i}"),
        ModTarget::DeckNextClip(i) => format!("deck.next.{i}"),
        ModTarget::DeckLaunchClip(deck, clip) => format!("deck.clip.{deck}.{clip}"),
        ModTarget::ShaderParam(i) => format!("shader.param.{i}"),
        ModTarget::ShaderOpacity => "shader.opacity".into(),
        ModTarget::VideoOpacity => "video.opacity".into(),
        ModTarget::AudioOpacity => "audio.opacity".into(),
        ModTarget::MasterOpacity => "master.opacity".into(),
        ModTarget::MasterBlackout => "master.blackout".into(),
        ModTarget::ClockBpm => "clock.bpm".into(),
        ModTarget::FxFlash => "fx.flash".into(),
        ModTarget::FxInvert => "fx.invert".into(),
        ModTarget::FxHue => "fx.hue".into(),
        ModTarget::FxSaturation => "fx.saturation".into(),
        ModTarget::FxContrast => "fx.contrast".into(),
        ModTarget::FxRgbSplit => "fx.rgb_split".into(),
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

/// Egy modulációs útvonal értelmezése: FORRAS->CEL[:MENNY[:DECAY[:ATTACK[:HOLD[:CURVE[:RETRIGGER]]]]]]
fn parse_mod_route(spec: &str) -> Option<ModRoute> {
    let (src, dst_part) = spec.split_once("->")?;
    let mut parts = dst_part.split(':');
    let dst = parts.next()?;
    let amount: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let decay: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(150.0);
    let attack: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let hold: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let curve = curve_from_name(parts.next());
    let retrigger: bool = parts.next().and_then(|s| s.parse().ok()).unwrap_or(true);
    let sustain: f32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let release_ms: f32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150.0);

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
        s if s.starts_with("audio.ch.") => {
            let rest = &s[9..];
            let (ch_str, feat) = rest.split_once('.')?;
            if let Some((a_str, b_str)) = ch_str.split_once('-') {
                let a_1based: usize = a_str.parse().ok()?;
                let b_1based: usize = b_str.parse().ok()?;
                let a = a_1based.checked_sub(1)?;
                let b = b_1based.checked_sub(1)?;
                if a >= dada_audio::MAX_CHANNELS || b >= dada_audio::MAX_CHANNELS {
                    return None;
                }
                match feat {
                    "rms" => ModSource::AudioPairRms(a, b),
                    "onset" => ModSource::AudioPairOnset(a, b),
                    "low" => ModSource::AudioPairLow(a, b),
                    "mid" => ModSource::AudioPairMid(a, b),
                    "high" => ModSource::AudioPairHigh(a, b),
                    _ => return None,
                }
            } else {
                let ch_1based: usize = ch_str.parse().ok()?;
                let ch = ch_1based.checked_sub(1)?;
                if ch >= dada_audio::MAX_CHANNELS {
                    return None;
                }
                match feat {
                    "rms" => ModSource::AudioChannelRms(ch),
                    "onset" => ModSource::AudioChannelOnset(ch),
                    "low" => ModSource::AudioChannelLow(ch),
                    "mid" => ModSource::AudioChannelMid(ch),
                    "high" => ModSource::AudioChannelHigh(ch),
                    _ => return None,
                }
            }
        }
        s if s.starts_with("lfo:") => ModSource::Lfo {
            period_beats: s[4..].parse().ok()?,
        },
        "clock.phase" => ModSource::ClockPhase,
        _ => return None,
    };
    let target = match dst {
        s if s.starts_with("deck.opacity.") => ModTarget::DeckOpacity(s[13..].parse().ok()?),
        s if s.starts_with("deck.speed.") => ModTarget::DeckSpeed(s[11..].parse().ok()?),
        s if s.starts_with("deck.next.") => ModTarget::DeckNextClip(s[10..].parse().ok()?),
        s if s.starts_with("deck.clip.") => {
            let rest = &s[10..];
            let (d_str, c_str) = rest.split_once('.')?;
            ModTarget::DeckLaunchClip(d_str.parse().ok()?, c_str.parse().ok()?)
        }
        s if s.starts_with("shader.param.") => ModTarget::ShaderParam(s[13..].parse().ok()?),
        "shader.opacity" => ModTarget::ShaderOpacity,
        "video.opacity" => ModTarget::VideoOpacity,
        "audio.opacity" => ModTarget::AudioOpacity,
        "master.opacity" => ModTarget::MasterOpacity,
        "master.blackout" => ModTarget::MasterBlackout,
        "clock.bpm" => ModTarget::ClockBpm,
        "fx.flash" => ModTarget::FxFlash,
        "fx.invert" => ModTarget::FxInvert,
        "fx.hue" => ModTarget::FxHue,
        "fx.saturation" => ModTarget::FxSaturation,
        "fx.contrast" => ModTarget::FxContrast,
        "fx.rgb_split" => ModTarget::FxRgbSplit,
        _ => return None,
    };
    let mut route = ModRoute::new(source, target).with_amount(amount);
    route.spec.decay_ms = decay;
    route.spec.attack_ms = attack;
    route.spec.hold_ms = hold;
    route.spec.sustain = sustain.clamp(0.0, 1.0);
    route.spec.release_ms = release_ms.max(1.0);
    route.spec.curve = curve;
    route.spec.retrigger = retrigger;
    Some(route)
}

/// A jelenlegi felállás projektként (mentéshez).
fn project_snapshot(
    args: &Args,
    project: &crate::project::Project,
    mixer: Option<&Mixer>,
    matrix: &ModMatrix,
    deck_configs: &[crate::project::DeckConfig],
    deck_folder: Option<&Path>,
    layer_order: &LayerOrder,
    shader_target: Option<usize>,
    shader_alpha_from_shader: bool,
    shader_blend: BlendMode,
) -> crate::project::Project {
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
                    clips: d
                        .clips
                        .iter()
                        .map(|c| crate::project::ClipConfig {
                            name: c.name.clone(),
                            speed: Some(c.speed),
                            in_point: Some(c.in_point),
                            loop_mode: Some(loop_mode_name(c.loop_mode).to_string()),
                            snap_beats: (c.snap_beats > 0.0).then_some(c.snap_beats),
                            weight: Some(c.weight),
                            chroma_mode: Some(c.chroma_mode.as_str().to_string()),
                            chroma_color: Some(c.chroma_color),
                            chroma_tolerance: Some(c.chroma_tolerance),
                            chroma_softness: Some(c.chroma_softness),
                            chroma_spill: Some(c.chroma_spill),
                        })
                        .collect(),
                    start_clip: d.active,
                    autopilot: Some(ap_name(d.autopilot.mode).to_string()),
                    every_beats: Some(d.autopilot.every_beats),
                    quantize: Some(d.autopilot.quantize.as_str().to_string()),
                    blend: Some(blend_name(d.blend).to_string()),
                    opacity: Some(d.opacity),
                    fade_beats: Some(d.fade_beats),
                    bpm_sync: Some(d.bpm_sync),
                    manual_next: Some(manual_next_name(d.manual_next).to_string()),
                    chroma_key: Some(d.chroma_key),
                    chroma_color: Some(d.chroma_color),
                    chroma_tolerance: Some(d.chroma_tolerance),
                    chroma_softness: Some(d.chroma_softness),
                    chroma_spill: Some(d.chroma_spill),
                }
            })
            .collect();
    }
    if !layer_order.is_empty() {
        p.layer_order = Some(layer_order.as_slice().to_vec());
    }
    p.shader_target = shader_target;
    p.shader_alpha = Some(if shader_alpha_from_shader {
        "shader"
    } else {
        "coverage"
    }
    .to_string());
    p.shader_blend = Some(blend_name(shader_blend).to_string());
    p.mods = matrix
        .routes
        .iter()
        .map(|r| {
            let base = format!(
                "{}->{}:{}:{}",
                mod_source_name(r.source),
                mod_target_name(r.target),
                r.amount,
                r.spec.decay_ms
            );
            let route = if r.spec.attack_ms == 0.0
                && r.spec.hold_ms == 0.0
                && r.spec.sustain == 0.0
                && (r.spec.release_ms - 150.0).abs() < 1e-3
                && r.spec.curve == dada_core::modulation::Curve::Exponential
                && r.spec.retrigger
            {
                base
            } else {
                format!(
                    "{base}:{}:{}:{}:{}:{}:{}",
                    r.spec.attack_ms,
                    r.spec.hold_ms,
                    curve_name(r.spec.curve),
                    r.spec.retrigger,
                    r.spec.sustain,
                    r.spec.release_ms
                )
            };
            crate::project::ModConfig { route }
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
        Some("off") => Quantize::Off,
        Some("1/16") => Quantize::Sixteenth,
        Some("1/8") => Quantize::Eighth,
        Some("1/4") => Quantize::Quarter,
        Some("1") | Some("beat") => Quantize::Beat,
        Some("2") => Quantize::TwoBeats,
        Some("4") => Quantize::FourBeats,
        Some("8") => Quantize::EightBeats,
        Some("16") => Quantize::SixteenBeats,
        Some("32") => Quantize::ThirtyTwoBeats,
        Some("bar") | Some("1bar") => Quantize::Bar,
        Some("2bar") => Quantize::TwoBars,
        Some("4bar") => Quantize::FourBars,
        Some("8bar") => Quantize::EightBars,
        _ => Quantize::Beat,
    }
}

/// Videófájlok felkutatása egy mappában.
fn scan_videos(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
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
            .with_inner_size(winit::dpi::PhysicalSize::new(
                self.args.width,
                self.args.height,
            ));
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

        // RTMP stream indítása (hardveres encoder + háttérszálas publikáló).
        if let Some(url) = self.args.rtmp.clone() {
            let (ew, eh) = renderer.size();
            let bitrate = 8_000_000u32;
            match dada_media::macos::encoder::VideoEncoder::new(ew, eh, bitrate, 60) {
                Ok(encoder) => match dada_stream::RtmpPublisher::start(&url) {
                    Ok(publisher) => {
                        match dada_media::macos::metal::MetalTextureCache::new(renderer.device()) {
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
                            Err(e) => {
                                log::error!("a stream textúra-cache nem hozható létre: {e:#}")
                            }
                        }
                    }
                    Err(e) => log::error!("az RTMP publikáló nem indul: {e:#}"),
                },
                Err(e) => log::error!("az encoder nem hozható létre: {e:#}"),
            }
        }

        // Hang kell, ha vizuál, moduláció, stream vagy audio-s felvétel kéri.
        let need_audio = self.args.audio_visual
            || self.matrix.uses_audio()
            || self.publisher.is_some()
            || (self.args.record.is_some() && self.capture_audio_enabled);
        if !need_audio {
            // nincs audio
        } else {
            let dev = self.args.audio_device.as_deref();
            let mut engine = match dev {
                Some(dev) => {
                    let dev = if dev == "default" { None } else { Some(dev) };
                    match AudioEngine::input_with_channels(
                        48000.0,
                        dev,
                        self.audio_selected_channels.clone(),
                    ) {
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
            self.audio_snapshot = engine.process();
            self.audio = Some((engine, vis));
            // A felvétel/stream külön hangforrásának előkészítése.
            self.prepare_capture_audio();
            if self.publisher.is_some() {
                self.ensure_capture_aac();
            }
        }
        // Felvétel indítása (natív H.264; opcionálisan AAC audióval).
        if let Some(path) = self.args.record.clone() {
            let (rw, rh) = renderer.size();
            let fps = 60.0f64;
            let audio_rate = self.capture_sample_rate();
            let writer = if audio_rate > 0.0 {
                dada_media::macos::writer::VideoWriter::new_with_audio(
                    Path::new(&path),
                    rw,
                    rh,
                    fps,
                    12_000_000,
                    audio_rate,
                    2,
                    128_000,
                )
            } else {
                dada_media::macos::writer::VideoWriter::new(
                    Path::new(&path),
                    rw,
                    rh,
                    fps,
                    12_000_000,
                )
            };
            match writer {
                Ok(writer) => {
                    match dada_media::macos::metal::MetalTextureCache::new(renderer.device()) {
                        Ok(cache) => {
                            log::info!(
                                "felvétel: {path} ({rw}x{rh} @ {fps} fps, 12 Mbit/s, audio: {})",
                                if audio_rate > 0.0 { "igen" } else { "nem" }
                            );
                            self.rec_cache = Some(cache);
                            self.recorder = Some(writer);
                            self.rec_start = Instant::now();
                            self.rec_next = Instant::now();
                        }
                        Err(e) => log::error!("a felvétel textúra-cache nem hozható létre: {e:#}"),
                    }
                }
                Err(e) => log::error!("a felvétel nem indítható: {e:#}"),
            }
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
                            self.shader_mtime =
                                std::fs::metadata(&path).and_then(|m| m.modified()).ok();
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
                    self.fading_videos.push(None);
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
                        self.fading_videos.push(None);
                        deck.active = Some(0);
                    }
                    Err(e) => {
                        log::error!("a klip nem nyitható meg: {e:#}");
                        self.videos.push(None);
                        self.fading_videos.push(None);
                    }
                }
                mixer.decks[i] = deck;
            }
            self.mixer = Some(mixer);
            self.deck_folder = specs.first().map(|s| s.0.clone());
        }

        // A projektből származó extra deck-beállítások (opacity, autopilot mód, quantize, fade).
        if let Some(mixer) = self.mixer.as_mut() {
            for (i, cfg) in self.deck_configs.clone().iter().enumerate() {
                if let Some(d) = mixer.decks.get_mut(i) {
                    if let Some(op) = cfg.opacity {
                        d.opacity = op.clamp(0.0, 1.0);
                    }
                    if let Some(mode) = cfg.autopilot.as_deref() {
                        d.autopilot.mode = autopilot_from_name(Some(mode), d.autopilot.every_beats);
                    }
                    if let Some(q) = cfg.quantize.as_deref() {
                        d.autopilot.quantize = quantize_from_name(Some(q));
                    }
                    if let Some(f) = cfg.fade_beats {
                        d.fade_beats = f;
                    }
                    d.bpm_sync = cfg.bpm_sync.unwrap_or(0.0).clamp(0.0, 256.0);
                    d.manual_next = match cfg.manual_next.as_deref() {
                        Some("shuffle") | Some("random") => ManualNextMode::Shuffle,
                        _ => ManualNextMode::Sequential,
                    };
                    d.chroma_key = cfg.chroma_key.unwrap_or(false);
                    let cc = cfg.chroma_color.unwrap_or([0.0, 1.0, 0.0]);
                    d.chroma_color = [
                        cc[0].clamp(0.0, 1.0),
                        cc[1].clamp(0.0, 1.0),
                        cc[2].clamp(0.0, 1.0),
                    ];
                    d.chroma_tolerance =
                        cfg.chroma_tolerance.unwrap_or(0.28).clamp(0.0, 1.0);
                    d.chroma_softness = cfg.chroma_softness.unwrap_or(0.12).clamp(0.0, 0.5);
                    d.chroma_spill = cfg.chroma_spill.unwrap_or(0.35).clamp(0.0, 1.0);
                    for clip in d.clips.iter_mut() {
                        let Some(cc) = cfg.clips.iter().find(|cc| cc.name == clip.name) else {
                            continue;
                        };
                        if let Some(v) = cc.speed {
                            clip.speed = v.clamp(0.05, 8.0);
                        }
                        if let Some(v) = cc.in_point {
                            clip.in_point = v.max(0.0);
                        }
                        if cc.loop_mode.is_some() {
                            clip.loop_mode = loop_mode_from_name(cc.loop_mode.as_deref());
                        }
                        clip.snap_beats = cc.snap_beats.unwrap_or(0.0).max(0.0);
                        if let Some(v) = cc.weight {
                            clip.weight = v.clamp(0.0, 100.0);
                        }
                        clip.chroma_mode = ChromaMode::from_name(cc.chroma_mode.as_deref());
                        if let Some(c) = cc.chroma_color {
                            clip.chroma_color = [
                                c[0].clamp(0.0, 1.0),
                                c[1].clamp(0.0, 1.0),
                                c[2].clamp(0.0, 1.0),
                            ];
                        }
                        if let Some(t) = cc.chroma_tolerance {
                            clip.chroma_tolerance = t.clamp(0.0, 1.0);
                        }
                        if let Some(s) = cc.chroma_softness {
                            clip.chroma_softness = s.clamp(0.0, 0.5);
                        }
                        if let Some(sp) = cc.chroma_spill {
                            clip.chroma_spill = sp.clamp(0.0, 1.0);
                        }
                    }
                    if let Some(active) = d.active {
                        if let Some(Some(v)) = self.videos.get_mut(i) {
                            if let Some(c) = d.clips.get(active) {
                                v.in_point = c.in_point;
                                v.loop_mode = c.loop_mode;
                                if c.snap_beats <= 0.0 {
                                    v.set_speed(c.speed, Instant::now());
                                }
                                if c.in_point > 0.0 {
                                    let _ = v.restart(Instant::now());
                                }
                            }
                        }
                    }
                }
            }
        }
        // Projekt: réteg-sorrend, ISF célréteg / alpha mód / blend visszaállítása.
        let layout = self.project.clone();
        self.apply_project_layout(&layout);
        // A klipek útvonalainak átadása a web-szervernek (thumbnailhez).
        if self.args.serve {
            let shared = WebShared::new();
            match web::spawn(self.args.port, Arc::clone(&shared)) {
                Ok(()) => self.web = Some(shared),
                Err(e) => log::error!("{e:#}"),
            }
        }
        self.sync_web_clips();

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
                let has_kick_route = self
                    .matrix
                    .routes
                    .iter()
                    .any(|r| matches!(r.source, ModSource::Note { note: 36 }));
                if !has_kick_route {
                    if let Some(route) = parse_mod_route("note:36->shader.param.0:0.65:220") {
                        self.matrix.add(route);
                        log::info!("MIDI-moduláció (mátrixban szerkeszthető): note:36 -> shader.param.0 (220 ms lecsengés)");
                    }
                }
            }
            self.midi = Some(engine);
        }
        self.renderer = Some(renderer);
        self.window = Some(window.clone());
        self.started = Instant::now();
        self.last_frame = self.started;
        if let Some(shared) = self.web.as_ref() {
            // A web API időbélyegei ugyanerről az origóról induljanak.
            shared.set_clock_origin(self.started);
        }
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
