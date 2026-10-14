//! Ableton Link szinkron: a hálózaton érkező Link csomagokból olvassuk a
//! tempót és a beat-idővonalat.
//!
//! A Link UDP multicaston hirdeti az ütemtervet (tmln bejegyzés):
//!   tempo (i64, mikroszekundum / beat), beat_origin (f64), time_origin (i64)
//! Ebből a beat bármely időpontban: beat_origin + (t - time_origin) / tempo.
//!
//! A time_origin a hálózat steady clock ideje mikroszekundumban; macOS-en ez
//! a mach_absolute_time számlálóval azonos domain, ezért ha a Link peer
//! ugyanezen a gépen fut (Ableton Live), a fázis is pontosan illeszkedik.

use anyhow::{Context, Result};
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A Link UDP portja.
pub const LINK_PORT: u16 = 20808;
/// A Link multicast címe.
pub const LINK_MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 76, 78, 75);
const LINK_MAGIC: &[u8] = b"_asdp_v";
const TMLN_KEY: &[u8] = b"tmln";

/// A Link ütemterve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timeline {
    /// Mikroszekundum / beat.
    pub micros_per_beat: u64,
    /// A beat-érték a time_origin pillanatban.
    pub beat_origin: f64,
    /// Az időeredet (mikroszekundum, Link steady clock).
    pub time_origin_us: i64,
}

impl Timeline {
    /// A tempó BPM-ben.
    pub fn bpm(&self) -> f64 {
        if self.micros_per_beat == 0 {
            0.0
        } else {
            60_000_000.0 / self.micros_per_beat as f64
        }
    }

    /// A beat értéke egy adott (Link-azonos) időpillanatban.
    pub fn beat_at(&self, now_us: i64) -> f64 {
        if self.micros_per_beat == 0 {
            return 0.0;
        }
        self.beat_origin + (now_us - self.time_origin_us) as f64 / self.micros_per_beat as f64
    }

    /// Másodperc / beat.
    pub fn seconds_per_beat(&self) -> f64 {
        self.micros_per_beat as f64 / 1_000_000.0
    }

    /// Hihető-e az idővonal ezen a gépen? (Az óradomain ellenőrzése.)
    pub fn plausible_at(&self, now_us: i64) -> bool {
        let beat = self.beat_at(now_us);
        beat.is_finite() && beat.abs() < 1.0e8
    }
}

/// A tmln bejegyzés kiolvasása egy Link UDP csomagból.
pub fn parse_timeline(payload: &[u8]) -> Option<Timeline> {
    if !payload.starts_with(LINK_MAGIC) {
        return None;
    }
    let key_at = payload.windows(TMLN_KEY.len()).position(|w| w == TMLN_KEY)?;
    // A kulcs után 4 bájt hossz, majd a 24 bájtos érték.
    let value_at = key_at + TMLN_KEY.len() + 4;
    let bytes = payload.get(value_at..value_at + 24)?;
    let micros_per_beat = u64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let beat_origin = f64::from_be_bytes(bytes[8..16].try_into().ok()?);
    let time_origin_us = i64::from_be_bytes(bytes[16..24].try_into().ok()?);
    if micros_per_beat == 0 || !beat_origin.is_finite() {
        return None;
    }
    Some(Timeline {
        micros_per_beat,
        beat_origin,
        time_origin_us,
    })
}

/// Az aktuális idő a Link óradomainjében (mikroszekundum).
#[cfg(target_os = "macos")]
pub fn now_link_us() -> i64 {
    use std::sync::OnceLock;
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info: libc::mach_timebase_info_data_t = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::mach_timebase_info(&mut info) };
        if ret == 0 && info.denom != 0 {
            (info.numer as u64, info.denom as u64)
        } else {
            (1, 1)
        }
    });
    let ticks = unsafe { libc::mach_absolute_time() };
    let nanos = (ticks as u128) * (numer as u128) / (denom as u128);
    (nanos / 1000) as i64
}

#[cfg(not(target_os = "macos"))]
pub fn now_link_us() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

struct State {
    timeline: Option<Timeline>,
    last_packet: Option<Instant>,
    packets: u64,
}

/// Passzív Link figyelő (háttérszálon).
pub struct LinkListener {
    state: Arc<Mutex<State>>,
    running: Arc<AtomicBool>,
    packets: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LinkListener {
    pub fn new() -> Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", LINK_PORT))
            .with_context(|| format!("a Link port ({LINK_PORT}) nem foglalható"))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .ok();
        if let Err(e) = socket.join_multicast_v4(&LINK_MULTICAST, &Ipv4Addr::UNSPECIFIED) {
            log::warn!("Link multicast csatlakozás sikertelen: {e}");
        }
        let _ = socket.set_multicast_loop_v4(true);

        let state = Arc::new(Mutex::new(State {
            timeline: None,
            last_packet: None,
            packets: 0,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let packets = Arc::new(AtomicU64::new(0));
        let st = Arc::clone(&state);
        let run = Arc::clone(&running);
        let pk = Arc::clone(&packets);
        let worker = std::thread::Builder::new()
            .name("dada-link".to_string())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                while run.load(Ordering::Relaxed) {
                    if let Ok((n, _from)) = socket.recv_from(&mut buf) {
                        if let Some(tl) = parse_timeline(&buf[..n]) {
                            if let Ok(mut s) = st.lock() {
                                s.timeline = Some(tl);
                                s.last_packet = Some(Instant::now());
                                s.packets += 1;
                            }
                            pk.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })?;
        Ok(Self {
            state,
            running,
            packets,
            worker: Some(worker),
        })
    }

    /// A legutóbbi ütemterv, ha az elmúlt 3 másodpercben jött csomag.
    pub fn timeline(&self) -> Option<Timeline> {
        let s = self.state.lock().ok()?;
        let last = s.last_packet?;
        if last.elapsed() > Duration::from_secs(3) {
            return None;
        }
        s.timeline
    }

    /// Él-e a Link session (az elmúlt 3 másodpercben jött csomag).
    pub fn is_active(&self) -> bool {
        self.timeline().is_some()
    }

    /// Az eddig látott Link csomagok száma.
    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }
}

impl Drop for LinkListener {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Egy Link csomag összeállítása (teszthez).
pub fn build_timeline_packet(tl: &Timeline) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(LINK_MAGIC);
    out.extend_from_slice(TMLN_KEY);
    out.extend_from_slice(&24u32.to_be_bytes());
    out.extend_from_slice(&tl.micros_per_beat.to_be_bytes());
    out.extend_from_slice(&tl.beat_origin.to_be_bytes());
    out.extend_from_slice(&tl.time_origin_us.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timeline_from_packet() {
        let tl = Timeline {
            micros_per_beat: 500_000,
            beat_origin: 4.0,
            time_origin_us: 1_000_000,
        };
        let packet = build_timeline_packet(&tl);
        let parsed = parse_timeline(&packet).expect("értelmezés");
        assert_eq!(parsed, tl);
        assert!((parsed.bpm() - 120.0).abs() < 0.001, "bpm = {}", parsed.bpm());
    }

    #[test]
    fn beat_advances_with_time() {
        let tl = Timeline {
            micros_per_beat: 500_000,
            beat_origin: 8.0,
            time_origin_us: 10_000_000,
        };
        assert!((tl.beat_at(10_000_000) - 8.0).abs() < 1e-9);
        assert!((tl.beat_at(11_000_000) - 10.0).abs() < 1e-9);
        assert!((tl.beat_at(12_000_000) - 12.0).abs() < 1e-9);
        assert!((tl.seconds_per_beat() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_timeline(b"").is_none());
        assert!(parse_timeline(b"not a link packet at all").is_none());
        let mut p = build_timeline_packet(&Timeline {
            micros_per_beat: 0,
            beat_origin: 0.0,
            time_origin_us: 0,
        });
        assert!(parse_timeline(&p).is_none());
        p.truncate(10);
        assert!(parse_timeline(&p).is_none());
    }

    #[test]
    fn several_tempos() {
        for bpm in [90.0f64, 128.0, 174.0] {
            let micros = (60_000_000.0 / bpm).round() as u64;
            let tl = Timeline {
                micros_per_beat: micros,
                beat_origin: 0.0,
                time_origin_us: 0,
            };
            assert!((tl.bpm() - bpm).abs() < 0.01, "{} -> {}", bpm, tl.bpm());
        }
    }

    #[test]
    fn clock_domain_is_monotonic() {
        let a = now_link_us();
        std::thread::sleep(Duration::from_millis(20));
        let b = now_link_us();
        assert!(b > a, "az óra nem monoton: {a} -> {b}");
        let diff = (b - a) as f64 / 1000.0;
        assert!(
            (15.0..200.0).contains(&diff),
            "20 ms alatt {diff:.1} ms telt el"
        );
    }

    #[test]
    fn receives_a_link_packet_over_the_network() {
        let listener = LinkListener::new().expect("figyelő indítás");
        let tl = Timeline {
            micros_per_beat: 416_666, // ~144 BPM
            beat_origin: 12.0,
            time_origin_us: 5_000_000,
        };
        let packet = build_timeline_packet(&tl);
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..20 {
            let _ = sock.send_to(&packet, ("127.0.0.1", LINK_PORT));
            std::thread::sleep(Duration::from_millis(25));
            if listener.is_active() {
                break;
            }
        }
        let got = listener.timeline().expect("nem érkezett Link csomag");
        assert_eq!(got, tl, "a vett ütemterv nem egyezik");
        assert!(
            (got.bpm() - 144.0).abs() < 0.05,
            "bpm = {}",
            got.bpm()
        );
    }

    #[test]
    fn plausible_guard_rejects_wrong_domain() {
        let tl = Timeline {
            micros_per_beat: 500_000,
            beat_origin: 0.0,
            time_origin_us: 0,
        };
        assert!(!tl.plausible_at(1_700_000_000_000_000));
        assert!(tl.plausible_at(1_000_000));
    }
}
