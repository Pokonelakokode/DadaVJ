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
    pub frames_shown: u64,
    pub frames_decoded: u64,
    pub last_pts: f64,
    pub size: (u32, u32),
}

impl VideoLayer {
    pub fn open(device: &wgpu::Device, path: &Path, speed: f64) -> Result<Self> {
        let reader = VideoReader::open_with_pixel_format(path, PIXEL_FORMAT_BGRA)
            .with_context(|| format!("videó megnyitása: {}", path.display()))?;
        let cache = MetalTextureCache::new(device)?;
        let size = (reader.width(), reader.height());
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-layer-params"),
            size: 16,
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
            speed,
            frames_shown: 0,
            frames_decoded: 0,
            last_pts: 0.0,
            size,
        })
    }

    pub fn nominal_fps(&self) -> f32 {
        self.reader.nominal_fps
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A réteg opacity-puffere (a bind group binding 2-höz).
    pub fn params_buffer(&self) -> &wgpu::Buffer {
        &self.params_buf
    }

    /// A réteg átlátszóságának beállítása.
    pub fn set_opacity(&self, queue: &wgpu::Queue, opacity: f32) {
        let data: [f32; 4] = [opacity, 0.0, 0.0, 0.0];
        queue.write_buffer(&self.params_buf, 0, bytemuck::cast_slice(&data));
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
        let target = (now - self.local_start).as_secs_f64() * self.speed;
        if self.pending.is_none() {
            self.fetch_next(now)?;
        }
        while let Some(frame) = self.pending.take() {
            if frame.pts_secs <= target {
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
            } else {
                self.pending = Some(frame);
                break;
            }
        }
        // Tulajdonolt handle-t adunk vissza (Clone), hogy a hívó ne maradjon
        // kölcsönzésben, és több réteget is össze tudjon fűzni.
        Ok(self.current_view.clone())
    }

    fn fetch_next(&mut self, now: Instant) -> Result<()> {
        match self.reader.next_frame()? {
            Some(f) => {
                self.frames_decoded += 1;
                self.pending = Some(f);
            }
            None => {
                // A klip végére értünk: újraindítjuk az elejéről, és az időzítést
                // is újraigazítjuk, hogy a loop ne csússzon el.
                self.reader = VideoReader::open_with_pixel_format(&self.path, PIXEL_FORMAT_BGRA)?;
                self.local_start = now;
                self.pending = self.reader.next_frame()?;
            }
        }
        Ok(())
    }
}
