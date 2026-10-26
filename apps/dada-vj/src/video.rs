//! Videó-réteg: dekódolás + zero-copy GPU textúra + időzített lejátszás.
//!
//! A frame-eket a zenei óra helyett egyelőre a monoton falórához igazítjuk
//! (a MIDI clock/Link bekötése után ez beat-alapú lesz). A dekódolt frame-eket
//! ring-pufferben tartjuk, hogy a GPU biztonságosan befejezhesse a renderelést.

use anyhow::{Context, Result};
use dada_media::macos::reader::PIXEL_FORMAT_BGRA;
use dada_media::{Frame, MetalTextureCache, VideoReader, WrappedFrame};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Hány dekódolt frame maradjon életben (GPU-biztonság).
const RING: usize = 8;

pub struct VideoLayer {
    path: PathBuf,
    reader: VideoReader,
    cache: MetalTextureCache,
    ring: VecDeque<WrappedFrame>,
    current_view: Option<wgpu::TextureView>,
    pending: Option<Frame>,
    /// A réteg opacity-ja (16 bájtos uniform, binding 2).
    params_buf: wgpu::Buffer,
    local_start: Instant,
    pub speed: f64,
    pub in_point: f64,
    pub loop_mode: dada_core::LoopMode,
    pub finished: bool,
    pub frames_shown: u64,
    pub frames_decoded: u64,
    pub last_pts: f64,
    pub size: (u32, u32),
    /// A videó teljes hossza másodpercben (BPM-hez igazított hosszhoz).
    duration: f64,
    /// +1 = forward, -1 = reverse (PingPong / bounce).
    direction: f64,
    /// Bounce reverse közben dekódolt, időrendben növekvő frame-ek.
    reverse_frames: VecDeque<Frame>,
    /// Reverse horgony: innen indul a cél-PTS csökkenése.
    reverse_anchor_now: Instant,
    reverse_anchor_pts: f64,
}

/// Bounce reverse szelet hossza másodpercben. A dekóder csak előrefelé tud
/// olvasni, ezért rövid időablakokat dekódolunk előre, és azokat játszuk vissza.
const REVERSE_WINDOW: f64 = 0.20;
/// Ennyivel a szelet eleje előtt prefetcheljük a következő (korábbi) szeletet.
const REVERSE_PREFETCH: f64 = 0.08;

impl VideoLayer {
    pub fn open(device: &wgpu::Device, path: &Path, speed: f64) -> Result<Self> {
        let reader = VideoReader::open_with_pixel_format(path, PIXEL_FORMAT_BGRA)
            .with_context(|| format!("videó megnyitása: {}", path.display()))?;
        let cache = MetalTextureCache::new(device)?;
        let size = (reader.width(), reader.height());
        let duration = reader.duration;
        // 32 bájt: opacity (0..4) + deckenkénti chroma key mezők (4..32).
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-layer-params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            path: path.to_path_buf(),
            reader,
            cache,
            ring: VecDeque::with_capacity(RING),
            current_view: None,
            pending: None,
            params_buf,
            local_start: Instant::now(),
            speed: speed.clamp(0.05, 8.0),
            in_point: 0.0,
            loop_mode: dada_core::LoopMode::Loop,
            finished: false,
            frames_shown: 0,
            frames_decoded: 0,
            last_pts: 0.0,
            size,
            duration,
            direction: 1.0,
            reverse_frames: VecDeque::new(),
            reverse_anchor_now: Instant::now(),
            reverse_anchor_pts: 0.0,
        })
    }

    pub fn nominal_fps(&self) -> f32 {
        self.reader.nominal_fps
    }

    /// A videó hossza másodpercben.
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Éppen visszafelé (bounce) játszik-e.
    pub fn is_reversing(&self) -> bool {
        self.direction < 0.0
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lejátszási sebesség ugrásmentes módosítása.
    pub fn set_speed(&mut self, speed: f64, now: Instant) {
        let speed = speed.clamp(0.05, 8.0);
        if (self.speed - speed).abs() > 1e-4 {
            if self.direction < 0.0 {
                // Reverse közben a csökkenő cél-PTS horgonyát állítjuk át.
                self.reverse_anchor_pts = self.last_pts;
                self.reverse_anchor_now = now;
            } else {
                let current_pts = (self.last_pts - self.in_point).max(0.0);
                let offset = std::time::Duration::from_secs_f64(current_pts / speed);
                self.local_start = now.checked_sub(offset).unwrap_or(now);
            }
            self.speed = speed;
        }
    }

    /// Lejátszási mód váltása. Normál forward lejátszás közben nem indítja
    /// újra a klipet; reverse-ből vagy lejárt Once klipből viszont tiszta
    /// forward állapotba hozza.
    pub fn set_loop_mode(&mut self, mode: dada_core::LoopMode, now: Instant) -> Result<()> {
        if self.loop_mode == mode {
            return Ok(());
        }
        let was_finished = self.finished;
        let was_reverse = self.direction < 0.0;
        self.loop_mode = mode;
        self.direction = 1.0;
        self.reverse_frames.clear();
        if was_reverse || (was_finished && mode != dada_core::LoopMode::Once) {
            self.finished = false;
            self.restart(now)?;
        } else if mode != dada_core::LoopMode::Once {
            self.finished = false;
        }
        Ok(())
    }

    /// Kezdőpont (in_point) váltása. Ha tényleg változik, a klip az új
    /// kezdőponttól indul újra, így nem marad a régi loop-ban.
    pub fn set_in_point(&mut self, in_point: f64, now: Instant) -> Result<()> {
        let in_point = in_point.max(0.0);
        if (self.in_point - in_point).abs() < 1e-9 {
            return Ok(());
        }
        self.in_point = in_point;
        self.restart(now)
    }

    /// A klip újraindítása a kezdőponttól (in_point).
    pub fn restart(&mut self, now: Instant) -> Result<()> {
        self.reader = VideoReader::open_with_pixel_format(&self.path, PIXEL_FORMAT_BGRA)?;
        self.local_start = now;
        self.direction = 1.0;
        self.reverse_frames.clear();
        self.reverse_anchor_now = now;
        self.reverse_anchor_pts = 0.0;
        self.finished = false;
        self.pending = None;
        self.seek_to_in_point()?;
        Ok(())
    }

    fn seek_to_in_point(&mut self) -> Result<()> {
        let mut last_frame = None;
        while let Some(f) = self.reader.next_frame()? {
            self.frames_decoded += 1;
            if f.pts_secs >= self.in_point {
                self.pending = Some(f);
                return Ok(());
            }
            last_frame = Some(f);
        }
        if let Some(f) = last_frame {
            self.in_point = 0.0;
            self.pending = Some(f);
        } else {
            self.finished = true;
        }
        Ok(())
    }

    /// A réteg opacity-puffere (a bind group binding 2-höz).
    pub fn params_buffer(&self) -> &wgpu::Buffer {
        &self.params_buf
    }

    /// A réteg átlátszóságának beállítása (csak a paraméter-puffer első 4 bájtja,
    /// így a chroma key mezőket nem írja felül).
    pub fn set_opacity(&self, queue: &wgpu::Queue, opacity: f32) {
        queue.write_buffer(&self.params_buf, 0, bytemuck::cast_slice(&[opacity]));
    }

    /// Deck-szintű chroma key beállítása (színkivétel az adott videó-rétegen).
    ///
    /// A szín sRGB-ben értendő (a UI színválasztója ilyet ad); a shader a
    /// kimenetét sRGB-be kódolva hasonlítja össze vele.
    pub fn set_chroma(
        &self,
        queue: &wgpu::Queue,
        enabled: bool,
        color: [f32; 3],
        tolerance: f32,
        softness: f32,
        spill: f32,
    ) {
        // A puffer layoutja: 0 = opacity, 4 = key_enabled, 8 = tolerance,
        // 12 = softness, 16/20/24 = key_color (vec3), 28 = spill.
        let data: [f32; 7] = [
            if enabled { 1.0 } else { 0.0 },
            tolerance.clamp(0.0, 1.0),
            softness.clamp(0.0, 0.5),
            color[0].clamp(0.0, 1.0),
            color[1].clamp(0.0, 1.0),
            color[2].clamp(0.0, 1.0),
            spill.clamp(0.0, 1.0),
        ];
        queue.write_buffer(&self.params_buf, 4, bytemuck::cast_slice(&data));
    }

    /// A következő megjelenítendő frame előállítása az eltelt időhöz igazítva.
    ///
    /// Addig dekódol, amíg a frame PTS-e meg nem haladja a célidőt; így a videó
    /// valós idejű marad akkor is, ha a render 60 fps-en fut és a forrás 25 fps.
    pub fn advance(
        &mut self,
        device: &wgpu::Device,
        now: Instant,
    ) -> Result<Option<wgpu::TextureView>> {
        if self.finished {
            return Ok(self.current_view.clone());
        }
        if self.direction < 0.0 {
            return self.advance_reverse(device, now);
        }
        if self.pending.is_none() {
            self.fetch_next(now)?;
        }
        if self.direction < 0.0 {
            return self.advance_reverse(device, now);
        }
        const MAX_DECODE_PER_TICK: usize = 4;
        let mut steps = 0usize;
        while let Some(frame) = self.pending.take() {
            let target = self.in_point + (now - self.local_start).as_secs_f64() * self.speed;
            if frame.pts_secs <= target {
                if steps >= MAX_DECODE_PER_TICK {
                    let current_pts = (frame.pts_secs - self.in_point).max(0.0);
                    let offset =
                        std::time::Duration::from_secs_f64(current_pts / self.speed.max(0.05));
                    self.local_start = now.checked_sub(offset).unwrap_or(now);
                    self.pending = Some(frame);
                    break;
                }
                steps += 1;
                let wrapped =
                    self.cache
                        .wrap_bgra(device, &frame.pixel_buffer, frame.width, frame.height)?;
                self.current_view = Some(wrapped.view());
                self.ring.push_back(wrapped);
                while self.ring.len() > RING {
                    self.ring.pop_front();
                }
                self.frames_shown += 1;
                self.last_pts = frame.pts_secs;
                self.fetch_next(now)?;
                if self.direction < 0.0 {
                    break;
                }
            } else {
                self.pending = Some(frame);
                break;
            }
        }
        if self.direction < 0.0 {
            return self.advance_reverse(device, now);
        }
        // Tulajdonolt handle-t adunk vissza (Clone), hogy a hívó ne maradjon
        // kölcsönzésben, és több réteget is össze tudjon fűzni.
        Ok(self.current_view.clone())
    }

    /// Bounce (PingPong) reverse szakasz: a dekóder csak előrefelé olvas, ezért
    /// rövid időablakokat dekódolunk, és a frame-eket csökkenő PTS szerint
    /// jelenítjük meg. A következő (korábbi) ablakot előre prefetcheljük.
    fn advance_reverse(
        &mut self,
        device: &wgpu::Device,
        now: Instant,
    ) -> Result<Option<wgpu::TextureView>> {
        let target = self.reverse_anchor_pts
            - (now - self.reverse_anchor_now).as_secs_f64() * self.speed;
        if target <= self.in_point + 0.001 {
            self.reset_forward(now)?;
            return self.advance(device, now);
        }
        if self.reverse_frames.is_empty() {
            if !self.load_reverse_chunk(self.reverse_anchor_pts)? {
                self.reset_forward(now)?;
                return self.advance(device, now);
            }
        }
        let front_pts = self
            .reverse_frames
            .front()
            .map(|f| f.pts_secs)
            .unwrap_or(self.in_point);
        if target < front_pts + REVERSE_PREFETCH && front_pts > self.in_point + 0.01 {
            let _ = self.load_reverse_chunk(front_pts)?;
        }

        let mut chosen = self.reverse_frames.len() - 1;
        for (i, frame) in self.reverse_frames.iter().enumerate() {
            if frame.pts_secs >= target {
                chosen = i;
                break;
            }
        }
        let (pts, width, height) = {
            let frame = &self.reverse_frames[chosen];
            (frame.pts_secs, frame.width, frame.height)
        };
        if (self.last_pts - pts).abs() > 1e-9 {
            let wrapped = {
                let frame = &self.reverse_frames[chosen];
                self.cache
                    .wrap_bgra(device, &frame.pixel_buffer, frame.width, frame.height)?
            };
            self.current_view = Some(wrapped.view());
            self.ring.push_back(wrapped);
            while self.ring.len() > RING {
                self.ring.pop_front();
            }
            self.frames_shown += 1;
            self.last_pts = pts;
            let _ = (width, height);
        }

        // Az már lejátszott (magasabb PTS-ű) frame-eket kidobjuk, hogy a
        // reverse buffer ne hízzon meg.
        while self.reverse_frames.len() > 1 {
            let drop = self
                .reverse_frames
                .back()
                .map(|f| f.pts_secs > target + 0.5)
                .unwrap_or(false);
            if drop {
                self.reverse_frames.pop_back();
            } else {
                break;
            }
        }
        Ok(self.current_view.clone())
    }

    fn fetch_next(&mut self, now: Instant) -> Result<()> {
        match self.reader.next_frame()? {
            Some(f) => {
                self.frames_decoded += 1;
                self.pending = Some(f);
            }
            None => {
                match self.loop_mode {
                    dada_core::LoopMode::Once => {
                        self.finished = true;
                        self.pending = None;
                    }
                    dada_core::LoopMode::PingPong => {
                        // A végére értünk: jöhet a visszafelé játszás.
                        self.pending = None;
                        self.direction = -1.0;
                        self.reverse_anchor_now = now;
                        self.reverse_anchor_pts = self.last_pts.max(self.in_point);
                        self.reverse_frames.clear();
                        if !self.load_reverse_chunk(self.reverse_anchor_pts)? {
                            self.reset_forward(now)?;
                        }
                    }
                    dada_core::LoopMode::Loop => {
                        // A klip végére értünk: újraindítjuk az elejéről, és az
                        // időzítést is újraigazítjuk, hogy a loop ne csússzon el.
                        self.reset_forward(now)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn reset_forward(&mut self, now: Instant) -> Result<()> {
        self.reader = VideoReader::open_with_pixel_format(&self.path, PIXEL_FORMAT_BGRA)?;
        self.local_start = now;
        self.direction = 1.0;
        self.reverse_frames.clear();
        self.pending = None;
        self.finished = false;
        self.seek_to_in_point()?;
        Ok(())
    }

    /// Egy [end-REVERSE_WINDOW, end] időablak dekódolása, időrendben növekvő
    /// frame-listába, a már meglévő buffer elé fűzve.
    fn load_reverse_chunk(&mut self, end_pts: f64) -> Result<bool> {
        let end = end_pts.min(self.duration.max(end_pts));
        let start = (end - REVERSE_WINDOW).max(self.in_point);
        if end - start < 0.01 {
            return Ok(false);
        }
        let mut reader = VideoReader::open_range(
            &self.path,
            PIXEL_FORMAT_BGRA,
            start,
            end - start + 0.02,
        )?;
        let mut frames: Vec<Frame> = Vec::new();
        while let Some(f) = reader.next_frame()? {
            self.frames_decoded += 1;
            if f.pts_secs > end + 0.03 {
                break;
            }
            if f.pts_secs >= self.in_point {
                frames.push(f);
            }
        }
        frames.sort_by(|a, b| {
            a.pts_secs
                .partial_cmp(&b.pts_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let existing_front = self
            .reverse_frames
            .front()
            .map(|f| f.pts_secs)
            .unwrap_or(f64::INFINITY);
        let mut added = 0usize;
        for f in frames.into_iter().rev() {
            if f.pts_secs < existing_front - 1e-6 {
                self.reverse_frames.push_front(f);
                added += 1;
            }
        }
        Ok(added > 0)
    }
}
