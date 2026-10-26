//! MIDI-motor: portok, clock-dekód, események, mappingek.
//!
//! A hardveres rész (midir) csak nyers byte-okat gyűjt egy pufferbe; a
//! feldolgozás a feed metódusban történik, ami így hardver nélkül,
//! determinisztikusan tesztelhető.

use anyhow::Result;
use dada_core::{ClockPll, Transport, PPQN};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::mapping::Mapping;
use crate::message::{MidiMessage, MidiParser};

/// Egy MIDI-bemenet leírása.
#[derive(Debug, Clone)]
pub struct MidiPortInfo {
    pub index: usize,
    pub name: String,
}

/// Egy beérkezett üzenet forrással és időbélyeggel.
#[derive(Debug, Clone)]
pub struct Event {
    pub source: usize,
    pub message: MidiMessage,
    pub time_secs: f64,
}

#[derive(Default)]
struct Incoming {
    packets: Mutex<Vec<(usize, u64, Vec<u8>)>>,
    dropped: Mutex<u64>,
}

/// A MIDI-motor.
pub struct MidiEngine {
    parser: MidiParser,
    pub clock: ClockPll,
    pub transport: Transport,
    pub sources: Vec<String>,
    /// Ha `Some`, csak a felsorolt nevű portokról fogadunk üzenetet; ha `None`, mindről.
    pub enabled_ports: Option<Vec<String>>,
    pub events: Vec<Event>,
    pub mappings: Vec<Mapping>,
    pub messages_in: u64,
    pub clock_ticks: u64,
    /// Utoljára beérkezett nem-clock MIDI esemény: (port_név, üzenet, sorszám).
    pub last_event: Option<(String, MidiMessage, u64)>,
    last_event_seq: u64,
    last_clock_tick: f64,
    last_bpm_applied: f32,
    incoming: Arc<Incoming>,
    connections: Vec<midir::MidiInputConnection<()>>,
    pub start: Instant,
}

impl Default for MidiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiEngine {
    pub fn new() -> Self {
        Self {
            parser: MidiParser::new(),
            clock: ClockPll::new(96, 120.0),
            transport: Transport::new(120.0),
            sources: Vec::new(),
            enabled_ports: None,
            events: Vec::new(),
            mappings: Vec::new(),
            messages_in: 0,
            clock_ticks: 0,
            last_event: None,
            last_event_seq: 0,
            last_clock_tick: 0.0,
            last_bpm_applied: 120.0,
            incoming: Arc::new(Incoming::default()),
            connections: Vec::new(),
            start: Instant::now(),
        }
    }

    /// Elérhető MIDI-bemenetek felsorolása (csatlakozás nélkül).
    pub fn list_ports() -> Result<Vec<MidiPortInfo>> {
        let midi_in = midir::MidiInput::new("dada-vj")?;
        let ports = midi_in.ports();
        Ok(ports
            .iter()
            .enumerate()
            .map(|(index, p)| MidiPortInfo {
                index,
                name: midi_in
                    .port_name(p)
                    .unwrap_or_else(|_| format!("port {index}")),
            })
            .collect())
    }

    /// Csatlakozás az összes elérhető bemenethez (több forrás egyszerre).
    pub fn open_all(&mut self) -> Result<Vec<String>> {
        self.connections.clear();
        self.sources.clear();
        // A midir MidiInput-ja egy kapcsolatot kezel (connect self-et fogyaszt),
        // ezért portonként külön példányt hozunk létre.
        let port_count = midir::MidiInput::new("dada-vj")?.ports().len();
        let mut names = Vec::new();
        for index in 0..port_count {
            let midi_in = midir::MidiInput::new("dada-vj")?;
            let ports = midi_in.ports();
            let Some(port) = ports.get(index) else {
                continue;
            };
            let name = midi_in
                .port_name(port)
                .unwrap_or_else(|_| format!("port {index}"));
            let source_idx = names.len();
            let incoming = Arc::clone(&self.incoming);
            match midi_in.connect(
                port,
                "dada-vj-in",
                move |timestamp_us, bytes, _| {
                    if let Ok(mut q) = incoming.packets.lock() {
                        if q.len() < 8192 {
                            q.push((source_idx, timestamp_us, bytes.to_vec()));
                        } else if let Ok(mut d) = incoming.dropped.lock() {
                            *d += 1;
                        }
                    }
                },
                (),
            ) {
                Ok(conn) => {
                    self.connections.push(conn);
                    names.push(name);
                }
                Err(e) => log::warn!("nem sikerült csatlakozni a(z) {name} porchoz: {e:?}"),
            }
        }
        self.sources = names.clone();
        Ok(names)
    }

    pub fn is_port_enabled(&self, source: usize) -> bool {
        let Some(allowed) = &self.enabled_ports else {
            return true;
        };
        let Some(name) = self.sources.get(source) else {
            return true;
        };
        allowed.iter().any(|p| p == name)
    }

    pub fn dropped_packets(&self) -> u64 {
        self.incoming.dropped.lock().map(|d| *d).unwrap_or(0)
    }

    /// Frame-enként hívandó: átveszi a hardveres byte-okat, majd lépteti az időt.
    pub fn tick(&mut self, now_secs: f64, dt_secs: f32) {
        let packets: Vec<(usize, u64, Vec<u8>)> = match self.incoming.packets.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        for (source, _ts, bytes) in packets {
            if !self.is_port_enabled(source) {
                continue;
            }
            // A CoreMIDI időbélyeg helyett a monoton óránkat használjuk: a PLL
            // így is pontos, mert a tickek sűrűn érkeznek.
            self.feed(source, &bytes, now_secs);
        }
        for m in self.mappings.iter_mut() {
            m.env.advance(dt_secs * 1000.0, &m.spec);
        }
    }

    /// Nyers MIDI byte-ok feldolgozása adott forrásból és időpontban.
    pub fn feed(&mut self, source: usize, bytes: &[u8], time_secs: f64) {
        let messages = self.parser.parse(bytes);
        for message in messages {
            self.messages_in += 1;
            if matches!(
                message,
                MidiMessage::NoteOn { velocity, .. } if velocity > 0
            ) || matches!(message, MidiMessage::ControlChange { .. })
            {
                self.last_event_seq += 1;
                let port_name = self
                    .sources
                    .get(source)
                    .cloned()
                    .unwrap_or_else(|| format!("port {source}"));
                self.last_event = Some((port_name, message, self.last_event_seq));
            }
            self.handle(source, message, time_secs);
            self.events.push(Event {
                source,
                message,
                time_secs,
            });
        }
    }

    fn handle(&mut self, _source: usize, message: MidiMessage, time_secs: f64) {
        match message {
            MidiMessage::Clock => {
                self.clock_ticks += 1;
                self.last_clock_tick += 1.0;
                self.clock.on_tick(self.last_clock_tick, time_secs);
                let bpm = self.clock.tempo_bpm() as f32;
                // Csak érdemi változásnál horgonyozzuk újra a transportot.
                if (bpm - self.last_bpm_applied).abs() > 0.02 {
                    self.transport.set_bpm(bpm as f64, time_secs);
                    self.last_bpm_applied = bpm;
                }
            }
            MidiMessage::Start => {
                self.clock.reset(120.0);
                self.last_clock_tick = 0.0;
                self.transport.seek(0.0, time_secs);
                self.transport.play(time_secs);
            }
            MidiMessage::Continue => self.transport.play(time_secs),
            MidiMessage::Stop => self.transport.stop(time_secs),
            MidiMessage::SongPosition(spp) => {
                // 1 MIDI beat = 6 clock = 1/16 kotta = 0.25 negyed.
                self.transport.seek(spp as f64 * 0.25, time_secs);
            }
            _ => {}
        }
        for m in self.mappings.iter_mut() {
            m.process(_source, &message);
        }
    }

    /// A zenei pozíció most.
    pub fn beats_now(&self) -> f64 {
        self.transport.beats_at(self.start.elapsed().as_secs_f64())
    }

    pub fn bpm(&self) -> f32 {
        self.clock.tempo_bpm() as f32
    }

    pub fn is_locked(&self) -> bool {
        self.clock.is_locked()
    }

    /// Események kivétele (a hívó dolgozza fel őket).
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}

/// Segéd: 24 PPQN clock stream szintetizálása teszthez.
///
/// Visszaadja a byte-csomagokat időbélyeggel (másodperc).
pub fn synthetic_clock(bpm: f64, seconds: f64, jitter: f64) -> Vec<(f64, Vec<u8>)> {
    let mut out = Vec::new();
    let spt = 60.0 / bpm / PPQN as f64;
    let n = (seconds / spt) as usize;
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    for i in 0..n {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let r = ((seed >> 33) as f64 / (1u64 << 31) as f64) - 1.0;
        out.push(((i as f64 + 1.0) * spt + r * jitter, vec![0xF8]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{EnvelopeSpec, Trigger};

    #[test]
    fn locks_to_clock_tempo() {
        let mut e = MidiEngine::new();
        for (t, bytes) in synthetic_clock(128.0, 8.0, 0.002) {
            e.feed(0, &bytes, t);
        }
        assert!(e.is_locked());
        assert!((e.bpm() - 128.0).abs() < 0.5, "bpm = {}", e.bpm());
        assert!(e.clock_ticks > 400);
    }

    #[test]
    fn start_stop_continue_transport() {
        let mut e = MidiEngine::new();
        e.feed(0, &[0xFA], 0.0);
        assert!(e.transport.is_playing());
        for (t, b) in synthetic_clock(120.0, 1.0, 0.0) {
            e.feed(0, &b, t);
        }
        let beats = e.transport.beats_at(0.5);
        assert!((0.0..=1.01).contains(&beats), "beats = {beats}");
        e.feed(0, &[0xFC], 1.0);
        assert!(!e.transport.is_playing());
        let stopped = e.transport.beats_at(2.0);
        e.feed(0, &[0xFB], 2.0);
        assert!(e.transport.is_playing());
        assert!(e.transport.beats_at(2.0) >= stopped);
    }

    #[test]
    fn note_triggers_mapped_envelope() {
        let mut e = MidiEngine::new();
        let spec = EnvelopeSpec {
            attack_ms: 0.0,
            hold_ms: 0.0,
            decay_ms: 100.0,
            sustain: 0.0,
            release_ms: 150.0,
            curve: crate::mapping::Curve::Linear,
            retrigger: true,
        };
        e.mappings.push(
            Mapping::new(
                0,
                Trigger::Note {
                    note: 36,
                    channel: None,
                },
                "invert",
                1.0,
            )
            .with_spec(spec),
        );
        e.feed(0, &[0x90, 36, 100], 0.0);
        assert!(e.mappings[0].value() > 0.9);
        e.tick(0.05, 0.05);
        let mid = e.mappings[0].value();
        assert!(mid > 0.3 && mid < 0.7, "mid = {mid}");
        e.tick(0.05, 0.05);
        assert!(e.mappings[0].value() < 0.05);
        assert_eq!(e.mappings[0].hits, 1);
    }

    #[test]
    fn multiple_sources_are_independent() {
        let mut e = MidiEngine::new();
        e.mappings.push(Mapping::new(
            0,
            Trigger::Note {
                note: 36,
                channel: None,
            },
            "a",
            1.0,
        ));
        e.mappings.push(Mapping::new(
            1,
            Trigger::Note {
                note: 38,
                channel: None,
            },
            "b",
            1.0,
        ));
        e.feed(1, &[0x90, 38, 100], 0.0);
        assert_eq!(e.mappings[0].hits, 0);
        assert_eq!(e.mappings[1].hits, 1);
        e.feed(0, &[0x90, 36, 100], 0.1);
        assert_eq!(e.mappings[0].hits, 1);
        assert_eq!(e.messages_in, 2);
    }
}
