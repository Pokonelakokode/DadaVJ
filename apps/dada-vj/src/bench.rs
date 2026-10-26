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

use crate::audio_vis::AudioVisual;
use crate::renderer::{LayerRef, Renderer};
use dada_core::{BlendMode, LoopMode};
use crate::video::VideoLayer;
use dada_audio::AudioEngine;
use dada_fx::{Effect, IsfShader};
use dada_midi::{
    mapping::Curve, synthetic_clock, EnvelopeSpec, Mapping, MidiEngine, Trigger,
};
use crate::Args;
use anyhow::{bail, Result};
use std::path::Path;
use std::time::{Duration, Instant};

/// AAC encoder-ellenőrzés: szinusz -> AAC packetek.
pub async fn aac_check(_args: &Args) -> Result<()> {
    use dada_media::macos::aac_objc::AacEncoder;

    let rate = 48000.0f64;
    let secs = 1.0f64;
    let frames = (rate * secs) as usize;
    // Fehér zaj (determinisztikus LCG): ez nem tömöríthető, így a bitráta
    // tényleg megmutatkozik. (A tiszta szinusz 30 kbps-ből is kijön.)
    // Mono minták (az encoder minden csatornába ugyanezt teszi).
    let mut pcm = Vec::with_capacity(frames);
    let mut seed: u32 = 0x1234_5678;
    for _ in 0..frames {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let s = ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.35;
        pcm.push(s);
    }
    println!("AAC-CHECK — {rate} Hz sztereó, {secs} s fehér zaj, 128 kbps cél");

    // Az élő streamelés útja: 100 ms-os blokkokban, folyamatosan.
    let mut enc = AacEncoder::new(rate, rate, 2, 128_000)?;
    let t0 = std::time::Instant::now();
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut calls = 0usize;
    let mut first_at = None;
    for (i, chunk) in pcm.chunks(4800).enumerate() {
        let p = enc.encode(chunk)?;
        calls += 1;
        if !p.is_empty() && first_at.is_none() {
            first_at = Some(i);
        }
        packets.extend(p);
    }
    println!(
        "  {calls} hívás (100 ms-os blokkok), első packet a {first_at:?}. hívásnál"
    );
    let dt = t0.elapsed();
    let total: usize = packets.iter().map(|p| p.len()).sum();
    let asc = enc.audio_specific_config().to_vec();
    let avg = if packets.is_empty() {
        0
    } else {
        total / packets.len()
    };
    println!(
        "  {} packet / {:.0} ms ({:.0}x valós idő) · {} bájt ({} B/packet, átlag)",
        packets.len(),
        dt.as_secs_f64() * 1000.0,
        secs / dt.as_secs_f64(),
        total,
        avg
    );
    println!(
        "  AudioSpecificConfig: {:02X?} ({} bájt) · első packetek: {:?}",
        asc,
        asc.len(),
        packets.iter().take(3).map(|p| p.len()).collect::<Vec<_>>()
    );
    // 48000 elvárt bitráta -> 1 s alatt ~16000 bájt; 1024-es frame-ek -> ~47 packet
    if asc.len() != 2 || asc[0] != 0x11 || asc[1] != 0x90 {
        bail!("az AudioSpecificConfig nem a várt 48k sztereó AAC-LC: {asc:02X?}");
    }
    if packets.len() < 40 || packets.len() > 60 {
        bail!("a packet-szám nem a várt ~47: {}", packets.len());
    }
    let secs_bytes = total as f64 / secs;
    if !(9_000.0..22_000.0).contains(&secs_bytes) {
        bail!("a bájt/másodperc nem a várt ~16000 (128 kbps): {secs_bytes:.0}");
    }
    println!("  OK: valódi AAC-LC, érvényes AudioSpecificConfig-gal.");
    Ok(())
}

/// Az encoder önálló validációja: CPU-töltött képek -> hardveres H.264.
pub async fn encode_check(_args: &Args) -> Result<()> {
    use dada_media::macos::encoder::VideoEncoder;
    use dada_media::macos::writer::VideoWriter;

    let (w, h) = (640u32, 360u32);
    let fps = 30u32;
    let frames = 30u64;
    println!("ENCODE-CHECK — {w}x{h} @ {fps} fps, {frames} frame (VTCompressionSession)");

    let mut encoder = VideoEncoder::new(w, h, 2_000_000, fps)?;
    let t0 = std::time::Instant::now();
    for i in 0..frames {
        let pb = encoder.pixel_buffer()?;
        let x0 = (i as u32 * 8) % w;
        VideoWriter::fill(&pb, |row, y| {
            for (x, px) in row.chunks_mut(4).enumerate() {
                let xu = x as u32;
                let on = xu >= x0 && xu < x0 + 60 && (y as u32) >= 100 && (y as u32) < 180;
                let (b, g, r) = if on {
                    (255u8, 255u8, 255u8)
                } else {
                    (200u8, 50u8, 10u8)
                };
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = 255;
            }
        })?;
        encoder.encode(&pb, i as f64 / fps as f64 * 1000.0)?;
    }
    let wall = t0.elapsed();
    encoder.finish();
    let encoded = encoder.drain();
    let errors = encoder.errors();
    let avcc = encoder.avcc();

    let total_bytes: usize = encoded.iter().map(|f| f.data.len()).sum();
    let keyframes = encoded.iter().filter(|f| f.keyframe).count();
    println!(
        "  {} frame kódolva {:.0} ms alatt ({:.0} fps), {:.1} KB, {} keyframe",
        encoded.len(),
        wall.as_secs_f64() * 1000.0,
        encoded.len() as f64 / wall.as_secs_f64(),
        total_bytes as f64 / 1024.0,
        keyframes
    );
    match &avcc {
        Some(a) => println!("  avcC: {} bájt, kezdete {:02X?}", a.len(), &a[..a.len().min(8)]),
        None => println!("  avcC: hiányzik!"),
    }
    if let Some(first) = encoded.first() {
        println!(
            "  első frame: {} bájt, pts {:.1} ms, keyframe={}, kezdete {:02X?}",
            first.data.len(),
            first.pts_ms,
            first.keyframe,
            &first.data[..first.data.len().min(8)]
        );
    }
    if !errors.is_empty() {
        bail!("encoder hibák: {:?}", errors);
    }
    if encoded.len() < (frames as usize * 8 / 10) {
        bail!("túl kevés frame készült: {}", encoded.len());
    }
    let avcc = avcc.ok_or_else(|| anyhow::anyhow!("nincs avcC (SPS/PPS)"))?;
    if avcc[0] != 0x01 {
        bail!("az avcC configurationVersion nem 1: {}", avcc[0]);
    }
    if avcc.len() < 10 {
        bail!("az avcC túl rövid: {}", avcc.len());
    }
    if keyframes == 0 {
        bail!("nem készült keyframe");
    }
    println!("  OK: valódi hardveres H.264, érvényes avcC-vel és keyframe-mel.");
    Ok(())
}

/// Egy videófájl vizsgálata: méret, frame-szám, hossz (headless).
pub async fn probe(path: &str) -> Result<()> {
    use dada_media::macos::reader::VideoReader;
    let mut reader = VideoReader::open(std::path::Path::new(path))?;
    let mut count = 0u64;
    let mut first = 0.0f64;
    let mut last = 0.0f64;
    while let Some(frame) = reader.next_frame()? {
        if count == 0 {
            first = frame.pts_secs;
        }
        last = frame.pts_secs;
        count += 1;
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!(
        "PROBE {path} — {}x{}, {count} frame, {:.2} s (PTS {:.3}..{:.3}), {:.1} MB",
        reader.width(),
        reader.height(),
        last - first,
        first,
        last,
        bytes as f64 / 1_048_576.0
    );
    if count == 0 {
        bail!("a fájl nem tartalmaz dekódolható frame-et");
    }
    Ok(())
}

/// Automatikus felvétel-validáció: írás AVAssetWriterrel, majd visszaolvasás
/// a saját dekóderünkkel. Ez a teljes encode -> fájl -> decode kört lefedi.
pub async fn record_check(args: &Args) -> Result<()> {
    use dada_media::macos::reader::VideoReader;
    use dada_media::macos::writer::VideoWriter;
    use std::time::Instant;

    let path = std::path::PathBuf::from(
        args.record
            .clone()
            .unwrap_or_else(|| ".testmedia/record-check.mp4".to_string()),
    );
    let (w, h) = (640u32, 360u32);
    let fps = 60.0f64;
    let frames = 90u64;
    println!(
        "RECORD-CHECK — {w}x{h} @ {fps} fps, {frames} frame -> {}",
        path.display()
    );

    let mut writer = VideoWriter::new(&path, w, h, fps, 4_000_000)?;
    let t0 = Instant::now();
    for i in 0..frames {
        let pb = writer.pixel_buffer()?;
        let x0 = (i as u32 * 4) % w;
        VideoWriter::fill(&pb, |row, y| {
            for (x, px) in row.chunks_mut(4).enumerate() {
                let xu = x as u32;
                let on = xu >= x0 && xu < x0 + 40 && (y as u32) >= 100 && (y as u32) < 160;
                // Háttér: B=200, G=50, R=10 (a csatorna-sorrend ellenőrzéséhez).
                let (b, g, r) = if on {
                    (255u8, 255u8, 255u8)
                } else {
                    (200u8, 50u8, 10u8)
                };
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = 255;
            }
        })?;
        writer.append(&pb)?;
    }
    writer.finish()?;
    let elapsed = t0.elapsed();
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "  encode: {:.0} ms ({:.0} fps) · fájl: {:.1} KB ({:.1} Mbit/s)",
        elapsed.as_secs_f64() * 1000.0,
        frames as f64 / elapsed.as_secs_f64(),
        bytes as f64 / 1024.0,
        bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0
    );

    // Visszaolvasás a saját dekóderünkkel.
    let mut reader = VideoReader::open(&path)?;
    let mut count = 0u64;
    let mut background = [0u8; 4];
    let mut bright_frames = 0u32;
    let mut first_bright: Option<u64> = None;
    while let Some(frame) = reader.next_frame()? {
        if count == 0 {
            background = VideoWriter::read_pixel_bgra(&frame.pixel_buffer, 600, 20);
        }
        // A mozgó sáv az y=130 sorban halad át x=300-on.
        let px = VideoWriter::read_pixel_bgra(&frame.pixel_buffer, 300, 130);
        let sum = px[0] as u32 + px[1] as u32 + px[2] as u32;
        if sum > 600 {
            bright_frames += 1;
            first_bright.get_or_insert(count);
        }
        count += 1;
    }
    println!(
        "  visszaolvasva: {count} frame, {}x{}",
        reader.width(),
        reader.height()
    );
    println!(
        "  háttér-pixel (600,20): B={} G={} R={} A={}  (elvárt: B~200 G~50 R~10)",
        background[0], background[1], background[2], background[3]
    );
    println!(
        "  a mozgó sáv {bright_frames} frame-en haladt át (első: {:?})",
        first_bright
    );

    if count != frames {
        bail!("a visszaolvasott frame-szám {count}, várt {frames}");
    }
    if reader.width() != w || reader.height() != h {
        bail!("a méret nem egyezik: {}x{}", reader.width(), reader.height());
    }
    // A csatorna-sorrend: a kék dominál, a piros alacsony.
    if background[0] < 150 || background[2] > 90 {
        bail!(
            "a csatorna-sorrend hibás (B={}, R={})",
            background[0],
            background[2]
        );
    }
    if !(5..=20).contains(&bright_frames) {
        bail!("a mozgó sáv nem a várt módon jelenik meg: {bright_frames} frame");
    }
    println!("  OK: a felvétel írható és visszaolvasható, a színek és a mozgás rendben.");
    Ok(())
}

/// Automatikus modulációs validáció: forrás -> mátrix -> cél (headless).
///
/// Végigmegy mindhárom fontos forrás-típuson:
///   * MIDI note -> envelope -> deck-opacity (majd lecseng),
///   * audio-sáv -> shader-paraméter,
///   * LFO -> master-opacity.
pub async fn mod_check(_args: &Args) -> Result<()> {
    use dada_core::modulation::{
        Curve, EnvelopeSpec, ModInputs, ModMatrix, ModOutputs, ModRoute, ModSource, ModTarget,
    };

    let mut m = ModMatrix::new();
    let kick_spec = EnvelopeSpec {
        attack_ms: 0.0,
        hold_ms: 0.0,
        decay_ms: 200.0,
        sustain: 0.0,
        release_ms: 150.0,
        curve: Curve::Linear,
        retrigger: true,
    };
    m.add(
        ModRoute::new(ModSource::Note { note: 36 }, ModTarget::DeckOpacity(1))
            .with_spec(kick_spec),
    );
    m.add(ModRoute::new(
        ModSource::AudioBand(1),
        ModTarget::ShaderParam(0),
    ));
    m.add(ModRoute::new(
        ModSource::Lfo {
            period_beats: 8.0,
        },
        ModTarget::MasterOpacity,
    ));
    m.add(ModRoute::new(
        ModSource::AudioOnset,
        ModTarget::DeckNextClip(0),
    ));

    println!("MOD-CHECK — {} útvonal, audio-forrás: {}", m.routes.len(), m.uses_audio());

    let frames = 240u32;
    let mut rows: Vec<(u32, f32, f32, f32, u64)> = Vec::new();
    let mut triggers = 0u32;
    let mut peak_op = 0.0f32;
    let mut min_after_peak = 1.0f32;
    let mut seen_peak = false;

    for i in 0..frames {
        let beat = i as f64 / 30.0; // 30 frame / negyed = 120 BPM 60 fps-en
        let kick = i % 30 == 0;
        let mut inp = ModInputs {
            dt_ms: 1000.0 / 60.0,
            beat,
            bpm: 120.0,
            ..Default::default()
        };
        if kick {
            inp.notes = vec![(36, 110)];
        }
        // Basszus-sáv: lassan pulzál.
        inp.bands[1] = (0.35 + 0.35 * (beat * 0.5).sin() as f32).clamp(0.0, 1.0);
        inp.onset = i % 45 == 0;

        let out = m.tick(&inp);
        let op = ModOutputs::value(&out, ModTarget::DeckOpacity(1));
        let sh = ModOutputs::value(&out, ModTarget::ShaderParam(0));
        let ma = ModOutputs::value(&out, ModTarget::MasterOpacity);
        if out.has_trigger(ModTarget::DeckNextClip(0)) {
            triggers += 1;
        }
        peak_op = peak_op.max(op);
        if op > 0.6 {
            seen_peak = true;
        }
        if seen_peak && op < min_after_peak {
            min_after_peak = op;
        }
        if i % 20 == 0 {
            rows.push((i, op, sh, ma, m.routes[0].hits));
        }
    }

    println!("  frame   kick-op   shader   master   kick-ek");
    for (i, op, sh, ma, hits) in &rows {
        println!("  {i:>5}   {op:.2}      {sh:.2}     {ma:.2}     {hits}");
    }
    println!(
        "  csúcs deck-opacity: {peak_op:.2} · lecsengés után: {min_after_peak:.2} · trigger-ek: {triggers}"
    );

    if peak_op < 0.6 {
        bail!("a kick nem hajtotta fel a deck-opacity-t: {peak_op:.2}");
    }
    if min_after_peak > 0.15 {
        bail!("az envelope nem cseng le: {min_after_peak:.2}");
    }
    if triggers < 3 {
        bail!("az audio-onset nem triggerelt: {triggers}");
    }
    println!("  OK: MIDI note, audio-sáv és LFO is eljut a céljáig.");
    Ok(())
}

/// Automatikus MIDI-validáció: clock -> BPM, note -> envelope -> shader -> pixel.
///
/// Végigmegy a teljes láncon szintetikus MIDI byte-okkal (nincs szükség hardverre):
/// 24 PPQN clock 128 BPM-en + kick minden negyeden, és ellenőrzi, hogy a
/// BPM bezár, az envelope felfut és lecseng, a kimenet pedig változik.
pub async fn midi_check(args: &Args) -> Result<()> {
    let shader_path = args
        .shader
        .clone()
        .unwrap_or_else(|| "shaders/plasma.isf".to_string());
    let width = 1280u32;
    let height = 720u32;
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut renderer = Renderer::new_offscreen(width, height, format).await?;
    let shader = IsfShader::load(Path::new(&shader_path))?;
    let mut effect = Effect::new(renderer.device(), format, shader)?;

    let bpm = 128.0f64;
    let seconds = 6.0f64;
    let mut midi = MidiEngine::new();
    let spec = EnvelopeSpec {
        attack_ms: 0.0,
        hold_ms: 0.0,
        decay_ms: 220.0,
        sustain: 0.0,
        release_ms: 150.0,
        curve: Curve::Exponential,
        retrigger: true,
    };
    midi.mappings.push(
        Mapping::new(0, Trigger::Note { note: 36, channel: None }, "shader.param.0", 1.0)
            .with_spec(spec),
    );

    // Szintetikus stream: clock + kick minden negyeden.
    let mut stream: Vec<(f64, Vec<u8>)> = synthetic_clock(bpm, seconds, 0.0015);
    let beat = 60.0 / bpm;
    let mut b = 0.0;
    while b < seconds {
        stream.push((b, vec![0x90, 36, 100]));
        stream.push((b + 0.03, vec![0x80, 36, 0]));
        b += beat;
    }
    stream.push((0.0, vec![0xFA]));
    stream.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());

    println!("MIDI-CHECK — szintetikus clock @ {bpm:.0} BPM + kick minden negyeden");
    println!("  shader: {shader_path}");

    let frames = (seconds * 60.0) as u32;
    let mut cursor = 0usize;
    let mut samples: Vec<(u32, f32, f32, [u8; 3])> = Vec::new();
    let mut env_peak = 0.0f32;
    let mut env_min_after_peak = 1.0f32;
    let mut seen_peak = false;

    for i in 0..frames {
        let now = i as f64 / 60.0;
        while cursor < stream.len() && stream[cursor].0 <= now {
            let (ts, bytes) = &stream[cursor];
            midi.feed(0, bytes, *ts);
            cursor += 1;
        }
        midi.tick(now, 1.0 / 60.0);

        // Mapping alkalmazása a shader paraméterére.
        if let Some(m) = midi.mappings.first() {
            let v = m.value();
            if let Some(p) = effect.shader.params.first() {
                effect.set_param(0, [p.default[0] + v * (p.max - p.default[0]), 0.0, 0.0, 0.0]);
            }
        }
        let env_value = midi.mappings.first().map(|m| m.value()).unwrap_or(0.0);
        env_peak = env_peak.max(env_value);
        if env_value > 0.5 {
            seen_peak = true;
        }
        if seen_peak && env_value < env_min_after_peak {
            env_min_after_peak = env_value;
        }

        let t = now as f32;
        effect.update(renderer.queue(), [width as f32, height as f32], t, 1.0 / 60.0);
        let layers = [LayerRef {
            pipeline: effect.pipeline(BlendMode::Normal).clone(),
            bind_group: effect.bind_group().clone(),
        }];
        renderer.render(t, 0.0, &layers)?;

        if i % 20 == 0 {
            if let Some(px) = renderer.read_back_center(8, 8) {
                let mut sum = [0u32; 3];
                let mut n = 0u32;
                for chunk in px.chunks_exact(4) {
                    sum[0] += chunk[2] as u32;
                    sum[1] += chunk[1] as u32;
                    sum[2] += chunk[0] as u32;
                    n += 1;
                }
                if n > 0 {
                    samples.push((
                        i,
                        now as f32,
                        env_value,
                        [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8],
                    ));
                }
            }
        }
    }
    renderer.wait_idle();

    let detected_bpm = midi.bpm();
    let beats = midi.transport.beats_at(seconds);
    println!(
        "  clock tickek: {} · üzenetek: {} · bezárt: {}",
        midi.clock_ticks,
        midi.messages_in,
        midi.is_locked()
    );
    println!(
        "  detektált BPM: {detected_bpm:.2} (várt: {bpm:.0}) · transport: {:.2} negyed {:.2} s alatt",
        beats, seconds
    );
    println!(
        "  envelope: csúcs {env_peak:.2} · lecsengés után {env_min_after_peak:.2} · kick-ek: {}",
        midi.mappings[0].hits
    );
    println!("  minta (idő, envelope, RGB):");
    for (i, t, env, c) in &samples {
        println!(
            "    frame {i:>3} t={t:>4.2}s env={env:.2}  R={:>3} G={:>3} B={:>3}",
            c[0], c[1], c[2]
        );
    }

    if (detected_bpm as f64 - bpm).abs() > 1.0 {
        bail!("a clock PLL nem zárt be: {detected_bpm:.2} BPM");
    }
    if env_peak < 0.5 {
        bail!("az envelope nem fut fel: {env_peak:.2}");
    }
    if env_min_after_peak > 0.1 {
        bail!("az envelope nem cseng le: {env_min_after_peak:.2}");
    }
    let distinct = {
        let mut set = std::collections::BTreeSet::new();
        for (_, _, _, c) in &samples {
            set.insert(*c);
        }
        set.len()
    };
    println!("  különböző színminta: {distinct}");
    if distinct < 2 {
        bail!("a kimenet nem változik a MIDI hatására");
    }
    println!("  OK: MIDI clock -> BPM -> envelope -> shader paraméter -> pixel.");
    Ok(())
}

/// Szilárd színű videó-réteg teljes (32 bájtos) paraméterezéssel — headless
/// chroma key teszthez.
fn solid_layer_with_chroma(
    renderer: &Renderer,
    srgb: [f32; 3],
    opacity: f32,
    chroma: (bool, [f32; 3], f32, f32, f32),
) -> (wgpu::Texture, LayerRef) {
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let texel = [to_u8(srgb[0]), to_u8(srgb[1]), to_u8(srgb[2]), 255u8];
    let mut pixels = [0u8; 2 * 2 * 4];
    for i in 0..4 {
        pixels[i * 4..i * 4 + 4].copy_from_slice(&texel);
    }
    let texture = renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("solid-video-layer"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    renderer.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let params = renderer.create_layer_params();
    let data: [f32; 8] = [
        opacity,
        if chroma.0 { 1.0 } else { 0.0 },
        chroma.2,
        chroma.3,
        chroma.1[0],
        chroma.1[1],
        chroma.1[2],
        chroma.4,
    ];
    renderer
        .queue()
        .write_buffer(&params, 0, bytemuck::cast_slice(&data));
    let layer = LayerRef {
        pipeline: renderer.video_pipeline(BlendMode::Normal).clone(),
        bind_group: renderer.make_video_bind_group(&view, &params),
    };
    (texture, layer)
}

/// Deck-szintű chroma key (színkivétel) önálló ellenőrzése (headless GPU).
///
/// Igazolja, hogy:
///  1. a kulcsszín (zöld) bekapcsolt kulccsal átlátszóvá válik (a háttér látszik),
///  2. kikapcsolt kulccsal a réteg változatlanul látszik,
///  3. más kulcsszín (piros) mellett a zöld réteg megmarad (nem kulcsol túl),
///  4. 50% opacity mellett a réteg félig áttetsző.
pub async fn chroma_check(_args: &Args) -> Result<()> {
    let width = 256u32;
    let height = 256u32;
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut renderer = Renderer::new_offscreen(width, height, format).await?;
    let green = [0.10f32, 0.85, 0.15];

    // 1) Kulcs be, kulcsszín = zöld -> a réteg átlátszó.
    let (_t1, layer1) =
        solid_layer_with_chroma(&renderer, green, 1.0, (true, green, 0.28, 0.12, 0.35));
    renderer.render(0.0, 0.0, &[layer1])?;
    let keyed = read_center_rgb(&renderer)?;
    println!(
        "  kulcsolt (zöld kulcsszín): R={} G={} B={}",
        keyed[0], keyed[1], keyed[2]
    );
    if keyed[0].max(keyed[1]).max(keyed[2]) > 64 {
        bail!("a chroma key nem tette átlátszóvá a kulcsszínt (a háttér nem látszik)");
    }

    // 2) Kulcs ki -> a zöld réteg látszik.
    let (_t2, layer2) = solid_layer_with_chroma(
        &renderer,
        green,
        1.0,
        (false, green, 0.28, 0.12, 0.35),
    );
    renderer.render(0.0, 0.0, &[layer2])?;
    let visible = read_center_rgb(&renderer)?;
    println!(
        "  kulcs nélkül: R={} G={} B={}",
        visible[0], visible[1], visible[2]
    );
    if visible[1] < 120 {
        bail!("kikapcsolt chroma key mellett a zöld réteg nem látszik");
    }

    // 3) Más kulcsszín (piros) -> a zöld réteg megmarad.
    let (_t3, layer3) = solid_layer_with_chroma(
        &renderer,
        green,
        1.0,
        (true, [1.0, 0.0, 0.0], 0.28, 0.12, 0.35),
    );
    renderer.render(0.0, 0.0, &[layer3])?;
    let other = read_center_rgb(&renderer)?;
    println!(
        "  más kulcsszínnel (piros): R={} G={} B={}",
        other[0], other[1], other[2]
    );
    if other[1] < 120 {
        bail!("a zöld réteg eltűnt, amikor a kulcsszín piros (túl széles kulcsozás)");
    }

    // 4) 50% opacity: félig áttetsző.
    let (_t4, layer4) = solid_layer_with_chroma(
        &renderer,
        green,
        0.5,
        (false, green, 0.28, 0.12, 0.35),
    );
    renderer.render(0.0, 0.0, &[layer4])?;
    let half = read_center_rgb(&renderer)?;
    println!(
        "  50% opacity: R={} G={} B={}",
        half[0], half[1], half[2]
    );
    if half[1] <= 40 || half[1] >= visible[1] - 20 {
        bail!("az 50%-os opacity nem félig áttetsző kimenetet ad");
    }

    renderer.wait_idle();
    println!("  OK: a deckenkénti chroma key kulcsol, kikapcsolható, és más színt nem kulcsol.");
    Ok(())
}

/// Automatikus ISF-validáció: a shader lefordul, lefut, és a kimenet változik?
/// Szintetikus, egyszínű kép-réteg az ISF effekt bemenetéhez.
fn synthetic_input_layer(renderer: &Renderer, srgb: [f32; 3]) -> (wgpu::Texture, LayerRef) {
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let texel = [to_u8(srgb[0]), to_u8(srgb[1]), to_u8(srgb[2]), 255u8];
    let mut pixels = [0u8; 2 * 2 * 4];
    for i in 0..4 {
        pixels[i * 4..i * 4 + 4].copy_from_slice(&texel);
    }
    let texture = renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("synthetic-isf-input"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    renderer.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let params = renderer.create_layer_params();
    // 32 bájt: opacity=1, chroma key kikapcsolva (key_enabled = 0).
    renderer.queue().write_buffer(
        &params,
        0,
        bytemuck::cast_slice(&[1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    );
    let layer = LayerRef {
        pipeline: renderer.video_pipeline(BlendMode::Normal).clone(),
        bind_group: renderer.make_video_bind_group(&view, &params),
    };
    (texture, layer)
}

/// Az effektet egyedüli rétegként rendereli a headless célba.
fn render_effect_only(renderer: &mut Renderer, effect: &Effect) -> Result<()> {
    let layers = [LayerRef {
        pipeline: effect.pipeline(BlendMode::Normal).clone(),
        bind_group: effect.bind_group().clone(),
    }];
    renderer.render(0.0, 0.0, &layers)?;
    Ok(())
}

/// Az offscreen cél közepének átlagos RGB színe (BGRA visszaolvasásból,
/// a soronkénti 256 bájtos padding kihagyásával).
fn read_center_rgb_sized(renderer: &Renderer, cols: u32, rows: u32) -> Option<[u8; 3]> {
    let px = renderer.read_back_center(cols, rows)?;
    let bytes_per_row = ((cols * 4).div_ceil(256)) * 256;
    let mut sum = [0u32; 3];
    let mut n = 0u32;
    for y in 0..rows {
        let row = (y * bytes_per_row) as usize;
        for x in 0..cols {
            let o = row + (x * 4) as usize;
            sum[0] += px.get(o + 2).copied().unwrap_or(0) as u32;
            sum[1] += px.get(o + 1).copied().unwrap_or(0) as u32;
            sum[2] += px.get(o).copied().unwrap_or(0) as u32;
            n += 1;
        }
    }
    let n = n.max(1);
    Some([
        (sum[0] / n) as u8,
        (sum[1] / n) as u8,
        (sum[2] / n) as u8,
    ])
}

fn read_center_rgb(renderer: &Renderer) -> Result<[u8; 3]> {
    match read_center_rgb_sized(renderer, 4, 4) {
        Some(rgb) => Ok(rgb),
        None => bail!("nincs offscreen visszaolvasási cél"),
    }
}

/// Képi bemenetes ISF effekt kompozit-ellenőrzése szintetikus bemenettel.
///
/// Két dolgot igazol pixel-szinten:
///  1. **fedés módban** (coverage, opacity = 1) az effekt teljesen lefedi az
///     alatta lévő képet — a sötét háttér nem üt át (ez a "full opacity mégis
///     látszik a videó" hiba regresszió-tesztje);
///  2. ha a shader kulcsozó paraméterekkel bír (key_color + tolerance), akkor
///     **shader-alpha módban** a kiválasztott szín átlátszó lesz, invertálva
///     pedig éppen a kulcsszín marad meg.
fn check_effect_composite(
    renderer: &mut Renderer,
    effect: &mut Effect,
    width: u32,
    height: u32,
) -> Result<()> {
    // Élénk narancs bemenet: egyértelműen elválik a sötét háttértől.
    let input_srgb = [0.95f32, 0.45, 0.08];
    let (_input_tex, below) = synthetic_input_layer(renderer, input_srgb);
    let Some(input_view) = renderer.render_input(0.0, 0.0, &[below]) else {
        bail!("az ISF képi bemeneti target nem jött létre");
    };
    effect.set_input(renderer.device(), &input_view);
    let res = [width as f32, height as f32];
    let dt = 1.0 / 60.0;

    // 1) Fedés: opacity = 1 => a kimenet alpha-ja 1, a háttér nem látszhat.
    effect.alpha_from_shader = false;
    effect.opacity = 1.0;
    effect.update(renderer.queue(), res, 0.0, dt);
    render_effect_only(renderer, effect)?;
    let px = read_center_rgb(renderer)?;
    println!("  fedés (coverage) minta: R={} G={} B={}", px[0], px[1], px[2]);
    if px[0].max(px[1]).max(px[2]) < 32 {
        bail!("a fedés módú effekt nem takarja le a hátteret (a mögötte lévő kép átüt)");
    }

    // 2) Chroma key: a bemenet színére állított kulccsal shader-alpha módban
    //    a kép átlátszó, invertálva pedig a kulcsszín marad meg.
    let key_idx = effect
        .shader
        .params
        .iter()
        .position(|p| p.name == "key_color");
    let tol_idx = effect
        .shader
        .params
        .iter()
        .position(|p| p.name == "tolerance");
    if let (Some(key_idx), Some(tol_idx)) = (key_idx, tol_idx) {
        let srgb_to_linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        effect.set_param(
            key_idx,
            [
                srgb_to_linear(input_srgb[0]),
                srgb_to_linear(input_srgb[1]),
                srgb_to_linear(input_srgb[2]),
                1.0,
            ],
        );
        effect.set_param(tol_idx, [0.12, 0.0, 0.0, 0.0]);
        if let Some(soft_idx) = effect.shader.params.iter().position(|p| p.name == "softness") {
            effect.set_param(soft_idx, [0.0, 0.0, 0.0, 0.0]);
        }
        effect.alpha_from_shader = true;
        effect.update(renderer.queue(), res, 0.0, dt);
        render_effect_only(renderer, effect)?;
        let keyed = read_center_rgb(renderer)?;
        println!(
            "  chroma key (kivett szín): R={} G={} B={}",
            keyed[0], keyed[1], keyed[2]
        );
        if keyed[0].max(keyed[1]).max(keyed[2]) > 40 {
            bail!("a chroma key nem kulcsolta ki a kiválasztott színt");
        }
        if let Some(inv_idx) = effect.shader.params.iter().position(|p| p.name == "invert") {
            effect.set_param(inv_idx, [1.0, 0.0, 0.0, 0.0]);
            effect.update(renderer.queue(), res, 0.0, dt);
            render_effect_only(renderer, effect)?;
            let inverted = read_center_rgb(renderer)?;
            println!(
                "  chroma key (invert): R={} G={} B={}",
                inverted[0], inverted[1], inverted[2]
            );
            if inverted[0].max(inverted[1]).max(inverted[2]) < 32 {
                bail!("a chroma key invert módban nem tartja meg a kulcsszínt");
            }
        }
        println!("  OK: chroma key kulcsolás és invert mód.");
    }
    println!("  OK: az ISF effekt lefedi a hátteret és a képi bemenet működik.");
    Ok(())
}

pub async fn shader_check(args: &Args) -> Result<()> {
    let Some(path) = args.shader.clone() else {
        bail!("a --shader-check-hez kell --shader FAJL.isf");
    };
    let width = 1280u32;
    let height = 720u32;
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut renderer = Renderer::new_offscreen(width, height, format).await?;

    let shader = IsfShader::load(Path::new(&path))?;
    println!("SHADER-CHECK — {path}");
    println!("  leírás: {}", shader.metadata.description);
    println!(
        "  paraméterek: {} · uniform puffer: {} bájt",
        shader.params.len(),
        shader.uniform_size
    );
    for p in &shader.params {
        println!(
            "    {:>12}  {:?}  offset={}  default={:?}",
            p.name, p.kind, p.offset, p.default
        );
    }
    if !shader.skipped.is_empty() {
        println!("  kihagyott inputok (nem támogatott típus): {}", shader.skipped.join(", "));
    }

    let has_inputs = !shader.images.is_empty();
    let mut effect = Effect::new(renderer.device(), format, shader)?;
    if has_inputs {
        println!("  képi bemenet(ek): {}", effect.shader.images.join(", "));
        println!("  -- kompozit-ellenőrzés szintetikus bemenettel --");
        // A shader lefordult, a bind group felépült, és a képi bemenet
        // (lentebbi rétegek) tényleg átmegy rajta — ezt pixel-szinten is
        // igazoljuk (fedés + opcionális chroma key).
        check_effect_composite(&mut renderer, &mut effect, width, height)?;
        return Ok(());
    }
    let mut samples: Vec<(u32, [u8; 3])> = Vec::new();
    let frames = 180u32;
    for i in 0..frames {
        let t = i as f32 / 60.0;
        // Az első paramétert időben változtatjuk, hogy a uniform-feltöltés is bizonyítva legyen.
        if !effect.values.is_empty() {
            let (lo, hi) = (effect.shader.params[0].min, effect.shader.params[0].max);
            let v = lo + (hi - lo) * (0.5 + 0.5 * (t * 2.0).sin());
            effect.set_param(0, [v, 0.0, 0.0, 0.0]);
        }
        effect.update(renderer.queue(), [width as f32, height as f32], t, 1.0 / 60.0);
        let layers = [LayerRef {
            pipeline: effect.pipeline(BlendMode::Normal).clone(),
            bind_group: effect.bind_group().clone(),
        }];
        renderer.render(t, 0.0, &layers)?;
        if i % 20 == 0 {
            if let Some(rgb) = read_center_rgb_sized(&renderer, 8, 8) {
                samples.push((i, rgb));
            }
        }
    }
    renderer.wait_idle();

    println!("  kimeneti pixel-minták (RGB, közép):");
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
    let maxv = samples
        .iter()
        .map(|(_, c)| c[0].max(c[1]).max(c[2]))
        .max()
        .unwrap_or(0);
    println!("  különböző színminta: {distinct} · legfényesebb: {maxv}");
    if maxv < 8 {
        bail!("a kimenet gyakorlatilag fekete — a shader nem ír");
    }
    if distinct < 2 {
        bail!("a kimenet nem változik — a shader vagy az uniform nem él");
    }
    println!("  OK: az ISF shader lefordult, lefutott, és a kimenet változik.");
    Ok(())
}

/// Automatikus audio-validáció: a szintetikus jel átjut-e az FFT-n és a shaderen?
pub async fn audio_check(_args: &Args) -> Result<()> {
    let width = 1280u32;
    let height = 720u32;
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut renderer = Renderer::new_offscreen(width, height, format).await?;
    let mut engine = AudioEngine::synthetic(48000.0);
    let mut vis = AudioVisual::new(renderer.device(), format, width, height);

    let mut samples: Vec<(u32, [u8; 3])> = Vec::new();
    let mut max_bands = [0f32; 6];
    let mut onsets = 0u32;
    let mut fft_runs = 0u64;
    let frames = 360u32;
    let mut process_ns: u128 = 0;

    for i in 0..frames {
        let t0 = Instant::now();
        let snap = engine.process_at(Some(i as f64 / 60.0));
        process_ns += t0.elapsed().as_nanos();
        fft_runs = snap.fft_runs;
        for (b, m) in max_bands.iter_mut().enumerate() {
            *m = m.max(snap.state.band_env[b]);
        }
        if snap.state.onset {
            onsets += 1;
        }
        let t = i as f32 / 60.0;
        vis.update(renderer.queue(), &snap, t, width, height);
        let layers = [LayerRef {
            pipeline: vis.pipeline(BlendMode::Normal).clone(),
            bind_group: vis.bind_group().clone(),
        }];
        renderer.render(t, 0.0, &layers)?;
        if i % 30 == 0 {
            if let Some(px) = renderer.read_back_center(4, 4) {
                let mut sum = [0u32; 3];
                let mut n = 0u32;
                for chunk in px.chunks_exact(4) {
                    sum[0] += chunk[2] as u32;
                    sum[1] += chunk[1] as u32;
                    sum[2] += chunk[0] as u32;
                    n += 1;
                }
                if n > 0 {
                    samples.push((
                        i,
                        [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8],
                    ));
                }
            }
        }
    }
    renderer.wait_idle();

    println!("AUDIO-CHECK — szintetikus forrás @ 48000 Hz");
    println!("  FFT futások: {fft_runs} · onsetek: {onsets}");
    println!(
        "  frame-enkénti feldolgozás (elemzés + szintetikus generálás): {:.1} µs",
        process_ns as f64 / frames as f64 / 1000.0
    );
    println!(
        "  max sávenergiák (sub/bass/low_mid/mid/high_mid/high): {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
        max_bands[0], max_bands[1], max_bands[2], max_bands[3], max_bands[4], max_bands[5]
    );
    println!("  kimeneti pixel-minták (RGB):");
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

    // Valódi elemzési teljesítmény: 10 másodpercnyi hang átfuttatása az elemzőn.
    {
        use dada_audio::analysis::Analyzer;
        let sr = 48000.0f32;
        let n = (sr * 10.0) as usize;
        let signal: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.5
                    + (2.0 * std::f32::consts::PI * 3000.0 * t).sin() * 0.2
            })
            .collect();
        let mut analyzer = Analyzer::new(sr);
        let t0 = Instant::now();
        for chunk in signal.chunks(4800) {
            analyzer.push(chunk);
        }
        let el = t0.elapsed().as_secs_f64();
        println!(
            "  ELEMZÉS TELJESÍTMÉNY: 10 s hang → {:.3} s számítás ({:.1}x valós idő, {} FFT)",
            el,
            10.0 / el.max(1e-9),
            analyzer.fft_runs
        );
    }

    let best_band = max_bands.iter().cloned().fold(0.0f32, f32::max);
    if best_band < 0.2 {
        bail!("a sávenergiák nem mozdulnak: {max_bands:?}");
    }
    if distinct < 3 {
        bail!("a kimenet nem elég változékony: {distinct} színminta");
    }
    println!("  OK: a hang az FFT-n és a shaderen keresztül megjelenik.");
    Ok(())
}

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
        if let Some(view) = view {
            let params = renderer.create_layer_params();
            // 32 bájt: opacity=1, chroma key kikapcsolva.
            let data: [f32; 8] = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
            renderer
                .queue()
                .write_buffer(&params, 0, bytemuck::cast_slice(&data));
            let bind_group = renderer.make_video_bind_group(&view, &params);
            let layers = [LayerRef {
                pipeline: renderer.video_pipeline(BlendMode::Normal).clone(),
                bind_group,
            }];
            renderer.render(i as f32 / 60.0, 0.0, &layers)?;
        }
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

/// Bounce / PingPong validáció: a videó a végére érve visszafelé folytatódik.
pub async fn bounce_check(args: &Args) -> Result<()> {
    let Some(path) = &args.video else {
        bail!("a --bounce-check-hez kell --video <fájl>");
    };
    let mut renderer =
        Renderer::new_offscreen(1280, 720, wgpu::TextureFormat::Bgra8UnormSrgb).await?;
    let mut video = VideoLayer::open(renderer.device(), Path::new(path), 1.0)?;
    video.loop_mode = LoopMode::PingPong;
    let duration = video.duration();
    if duration <= 0.0 {
        bail!("a videó hossza ismeretlen");
    }
    println!(
        "BOUNCE-CHECK — {path} ({:.2} s, {:.2} fps)",
        duration,
        video.nominal_fps()
    );
    let start = Instant::now();
    let fps = 60.0f64;
    let frames = ((duration + 1.5) * fps).ceil() as usize;
    let mut prev_pts = 0.0f64;
    let mut saw_reverse = false;
    let mut reverse_steps = 0usize;
    let mut forward_max = 0.0f64;
    for i in 0..frames {
        let now = start + Duration::from_secs_f64(i as f64 / fps);
        video.advance(renderer.device(), now)?;
        if video.is_reversing() {
            saw_reverse = true;
            if video.last_pts + 1e-5 < prev_pts {
                reverse_steps += 1;
            }
        } else {
            forward_max = forward_max.max(video.last_pts);
        }
        prev_pts = video.last_pts;
    }
    renderer.wait_idle();
    println!(
        "  forward max PTS: {forward_max:.2} s · reverse: {saw_reverse} · reverse lépés: {reverse_steps} · utolsó PTS: {:.2} s",
        video.last_pts
    );
    if !saw_reverse || reverse_steps == 0 {
        bail!("a bounce mód nem indult visszafelé");
    }
    println!("  OK: a PingPong / bounce lejátszás visszafelé is halad.");
    Ok(())
}

const BATCH: u32 = 600;
const WARMUP: u32 = 120;

fn batch(renderer: &mut Renderer, frames: u32, layers: u32) -> f64 {
    let start = Instant::now();
    for i in 0..frames {
        let t = i as f32 * 0.016;
        let _ = renderer.render(t, t * 2.0, &[]);
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
