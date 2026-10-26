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
    let key_at = payload
        .windows(TMLN_KEY.len())
        .position(|w| w == TMLN_KEY)?;
    // A kulcs után 4 bájt hossz, majd a 24 bájtos érték.
    let value_at = key_at + TMLN_KEY.len() + 4;
    let bytes = payload.get(value_at..value_at + 24)?;
    let micros_per_beat = u64::from_be_bytes(bytes[0..8].try_into().ok()?);
    // Az Ableton Link _asdp_v1 protokollban a beat_origin 64 bites előjeles
    // egész szám mikrobeatben (1_000_000 mikrobeat = 1 beat).
    let micro_beats = i64::from_be_bytes(bytes[8..16].try_into().ok()?);
    let beat_origin = micro_beats as f64 / 1_000_000.0;
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
    session_id: [u8; 8],
    last_packet: Option<Instant>,
    packets: u64,
    peers: std::collections::HashMap<[u8; 8], Instant>,
}

/// Ableton Link figyelő és peer-hirdető (háttérszálon).
pub struct LinkListener {
    state: Arc<Mutex<State>>,
    running: Arc<AtomicBool>,
    packets: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// Megosztott UDP socket nyitása SO_REUSEADDR + SO_REUSEPORT opciókkal, hogy
/// az Ableton Live és a Dada VJ egyszerre tudja használni a 20808-as portot.
#[cfg(unix)]
fn bind_shared_link_socket(port: u16) -> Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP);
        if fd < 0 {
            anyhow::bail!(
                "nem sikerült UDP socketet nyitni: {}",
                std::io::Error::last_os_error()
            );
        }
        let yes: libc::c_int = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &yes as *const _ as *const libc::c_void,
            std::mem::size_of_val(&yes) as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &yes as *const _ as *const libc::c_void,
            std::mem::size_of_val(&yes) as libc::socklen_t,
        );

        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        #[cfg(target_os = "macos")]
        {
            addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        }
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = port.to_be();
        addr.sin_addr.s_addr = u32::from(Ipv4Addr::UNSPECIFIED).to_be();

        if libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) != 0
        {
            let err = std::io::Error::last_os_error();
            libc::close(fd);
            anyhow::bail!("a Link port ({port}) nem foglalható: {err}");
        }
        Ok(UdpSocket::from_raw_fd(fd))
    }
}

#[cfg(not(unix))]
fn bind_shared_link_socket(port: u16) -> Result<UdpSocket> {
    UdpSocket::bind(("0.0.0.0", port))
        .with_context(|| format!("a Link port ({port}) nem foglalható"))
}

/// Az összes lokális IPv4 interfész (beleértve a 127.0.0.1 loopbacket is).
#[cfg(unix)]
fn local_ipv4_interfaces() -> Vec<Ipv4Addr> {
    let mut out = vec![Ipv4Addr::LOCALHOST];
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_unspecified() && !out.contains(&ip) {
                    out.push(ip);
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

#[cfg(not(unix))]
fn local_ipv4_interfaces() -> Vec<Ipv4Addr> {
    vec![Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED]
}

#[cfg(unix)]
fn set_multicast_interface(socket: &UdpSocket, iface: Ipv4Addr) {
    use std::os::fd::AsRawFd;
    let addr = libc::in_addr {
        s_addr: u32::from(iface).to_be(),
    };
    unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            &addr as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
        );
    }
}

#[cfg(not(unix))]
fn set_multicast_interface(_socket: &UdpSocket, _iface: Ipv4Addr) {}

fn parse_session_id(payload: &[u8]) -> Option<[u8; 8]> {
    if !payload.starts_with(LINK_MAGIC) {
        return None;
    }
    let key = b"sess";
    let key_at = payload.windows(key.len()).position(|w| w == key)?;
    let val_at = key_at + 4 + 4;
    let bytes: [u8; 8] = payload.get(val_at..val_at + 8)?.try_into().ok()?;
    Some(bytes)
}

fn parse_peer_ident(payload: &[u8]) -> Option<(u8, [u8; 8])> {
    if !payload.starts_with(LINK_MAGIC) || payload.len() < 20 {
        return None;
    }
    let msg_type = payload[8];
    let ident: [u8; 8] = payload.get(12..20)?.try_into().ok()?;
    Some((msg_type, ident))
}

fn build_peer_packet(
    msg_type: u8,
    ident: &[u8; 8],
    session_id: &[u8; 8],
    tl: Option<&Timeline>,
    local_ip: Ipv4Addr,
    port: u16,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    // _asdp_v1 fejléc (20 bájt)
    out.extend_from_slice(b"_asdp_v\x01");
    out.push(msg_type); // 1 = Alive, 2 = Response, 3 = ByeBye
    out.push(5); // TTL = 5 mp
    out.extend_from_slice(&0u16.to_be_bytes()); // group_id = 0
    out.extend_from_slice(ident);
    if msg_type != 3 {
        // sess TLV (8 bájt)
        out.extend_from_slice(b"sess");
        out.extend_from_slice(&8u32.to_be_bytes());
        out.extend_from_slice(session_id);
        if let Some(tl) = tl {
            // tmln TLV (24 bájt)
            let micro_beats = (tl.beat_origin * 1_000_000.0).round() as i64;
            out.extend_from_slice(TMLN_KEY);
            out.extend_from_slice(&24u32.to_be_bytes());
            out.extend_from_slice(&tl.micros_per_beat.to_be_bytes());
            out.extend_from_slice(&micro_beats.to_be_bytes());
            out.extend_from_slice(&tl.time_origin_us.to_be_bytes());
        }
        // mert TLV (6 bájt: IPv4 + port)
        out.extend_from_slice(b"mert");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&local_ip.octets());
        out.extend_from_slice(&port.to_be_bytes());
    }
    out
}

static INSTANCE_SEQ: AtomicU64 = AtomicU64::new(1);

impl LinkListener {
    pub fn new() -> Result<Self> {
        Self::with_port(LINK_PORT)
    }

    pub fn with_port(port: u16) -> Result<Self> {
        let socket = bind_shared_link_socket(port)?;
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .ok();
        let _ = socket.set_multicast_loop_v4(true);
        let _ = socket.set_multicast_ttl_v4(2);

        let ifaces = local_ipv4_interfaces();
        let mut joined = 0usize;
        for iface in &ifaces {
            if socket.join_multicast_v4(&LINK_MULTICAST, iface).is_ok() {
                joined += 1;
            }
        }
        if joined == 0 {
            if let Err(e) = socket.join_multicast_v4(&LINK_MULTICAST, &Ipv4Addr::UNSPECIFIED) {
                log::warn!("Link multicast csatlakozás sikertelen: {e}");
            }
        }

        let now_us = now_link_us();
        let pid = std::process::id() as u64;
        let seq = INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed);
        let seed = (now_us as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(seq.wrapping_mul(0xBF58_476D_1CE4_E5B9))
            ^ pid;
        let our_ident: [u8; 8] = seed.to_be_bytes();
        let initial_sess: [u8; 8] = (seed ^ 0x4441_4441_4C49_4E4B).to_be_bytes();

        let state = Arc::new(Mutex::new(State {
            timeline: None,
            session_id: initial_sess,
            last_packet: None,
            packets: 0,
            peers: std::collections::HashMap::new(),
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
                let mut last_announce = Instant::now()
                    .checked_sub(Duration::from_secs(2))
                    .unwrap_or_else(Instant::now);

                let broadcast = |msg_type: u8, sess: &[u8; 8], tl: Option<&Timeline>| {
                    for iface in &ifaces {
                        set_multicast_interface(&socket, *iface);
                        let pkt = build_peer_packet(msg_type, &our_ident, sess, tl, *iface, port);
                        let _ = socket.send_to(&pkt, (LINK_MULTICAST, port));
                    }
                };

                while run.load(Ordering::Relaxed) {
                    // Időszakos Alive hirdetés (minden ~900 ms-ban), hogy az Ableton Live
                    // lássa a Dada VJ-t és küldje az ütemtervet.
                    if last_announce.elapsed() >= Duration::from_millis(900) {
                        last_announce = Instant::now();
                        let (sess, tl) = if let Ok(mut s) = st.lock() {
                            s.peers
                                .retain(|_, seen| seen.elapsed() <= Duration::from_secs(6));
                            (s.session_id, s.timeline)
                        } else {
                            continue;
                        };
                        broadcast(1, &sess, tl.as_ref());
                    }

                    if let Ok((n, from)) = socket.recv_from(&mut buf) {
                        let data = &buf[..n];
                        let peer_info = parse_peer_ident(data);
                        if let Some((_, peer_id)) = peer_info {
                            if peer_id == our_ident {
                                continue;
                            }
                        }

                        if let Some(tl) = parse_timeline(data) {
                            if let Ok(mut s) = st.lock() {
                                s.timeline = Some(tl);
                                s.last_packet = Some(Instant::now());
                                s.packets += 1;
                            }
                            pk.fetch_add(1, Ordering::Relaxed);
                        }

                        if let Some((msg_type, peer_id)) = peer_info {
                            if msg_type == 3 {
                                if let Ok(mut s) = st.lock() {
                                    s.peers.remove(&peer_id);
                                }
                                continue;
                            }
                            if let Ok(mut s) = st.lock() {
                                s.peers.insert(peer_id, Instant::now());
                                if let Some(sess) = parse_session_id(data) {
                                    s.session_id = sess;
                                }
                            }
                            if msg_type == 1 {
                                if let Ok(s) = st.lock() {
                                    let resp = build_peer_packet(
                                        2,
                                        &our_ident,
                                        &s.session_id,
                                        s.timeline.as_ref(),
                                        Ipv4Addr::LOCALHOST,
                                        port,
                                    );
                                    let _ = socket.send_to(&resp, from);
                                    broadcast(2, &s.session_id, s.timeline.as_ref());
                                }
                            }
                        }
                    }
                }

                // Kilépéskor ByeBye (3) csomag küldése.
                if let Ok(s) = st.lock() {
                    broadcast(3, &s.session_id, None);
                }
            })?;
        Ok(Self {
            state,
            running,
            packets,
            worker: Some(worker),
        })
    }

    /// A legutóbbi ütemterv, ha az elmúlt 5 másodpercben jött csomag.
    pub fn timeline(&self) -> Option<Timeline> {
        let s = self.state.lock().ok()?;
        let last = s.last_packet?;
        if last.elapsed() > Duration::from_secs(5) {
            return None;
        }
        s.timeline
    }

    /// Él-e a Link session (az elmúlt 5 másodpercben jött csomag).
    pub fn is_active(&self) -> bool {
        self.timeline().is_some()
    }

    /// Aktív Link peerek száma.
    pub fn peers(&self) -> usize {
        self.state
            .lock()
            .ok()
            .map(|s| {
                s.peers
                    .values()
                    .filter(|seen| seen.elapsed() <= Duration::from_secs(6))
                    .count()
            })
            .unwrap_or(0)
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
    let micro_beats = (tl.beat_origin * 1_000_000.0).round() as i64;
    out.extend_from_slice(b"_asdp_v\x01");
    out.push(1); // Alive
    out.push(5); // TTL
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(b"testpeer");
    out.extend_from_slice(TMLN_KEY);
    out.extend_from_slice(&24u32.to_be_bytes());
    out.extend_from_slice(&tl.micros_per_beat.to_be_bytes());
    out.extend_from_slice(&micro_beats.to_be_bytes());
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
        assert!(
            (parsed.bpm() - 120.0).abs() < 0.001,
            "bpm = {}",
            parsed.bpm()
        );
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
        let test_port = LINK_PORT + 111;
        let listener = LinkListener::with_port(test_port).expect("figyelő indítás");
        let tl = Timeline {
            micros_per_beat: 416_666, // ~144 BPM
            beat_origin: 12.0,
            time_origin_us: 5_000_000,
        };
        let packet = build_timeline_packet(&tl);
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..20 {
            if let Err(e) = sock.send_to(&packet, ("127.0.0.1", test_port)) {
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(libc::EPERM)
                {
                    // Hálózat nélküli teszt-sandboxban a kimenő UDP tiltott;
                    // ilyenkor közvetlenül a csomagfeldolgozót ellenőrizzük.
                    assert_eq!(parse_timeline(&packet), Some(tl));
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
            if listener.is_active() {
                break;
            }
        }
        let got = listener.timeline().expect("nem érkezett Link csomag");
        assert_eq!(got, tl, "a vett ütemterv nem egyezik");
        assert!((got.bpm() - 144.0).abs() < 0.05, "bpm = {}", got.bpm());
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

    #[test]
    fn multiple_listeners_share_port() {
        let l1 = LinkListener::new().expect("első figyelő");
        let l2 = LinkListener::new().expect("második figyelő ugyanazon a porton (SO_REUSEPORT)");
        drop(l1);
        drop(l2);
    }
}
