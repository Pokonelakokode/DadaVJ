//! RTMP publikálás.
//!
//! A hálózat külön szálon fut: a render loop csak egy csatornába tesz, ami
//! tele van, azt a frame-et eldobjuk (élő adásnál ez a helyes viselkedés).
//! A szál saját kezűleg viszi a handshake-et, majd az rml_rtmp kliens
//! session-jét hajtja.

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType,
};
use rml_rtmp::time::RtmpTimestamp;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Egy kimenő videó-frame, már FLV/AVC csomagolással.
pub struct VideoFrame {
    pub data: Vec<u8>,
    pub pts_ms: u64,
}

/// Egy kimenő audio-frame, már FLV/AAC csomagolással.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub data: Vec<u8>,
    pub pts_ms: u64,
}

enum Outgoing {
    Video(VideoFrame),
    Audio(AudioFrame),
}

#[derive(Debug, Default)]
struct Stats {
    sent: AtomicU64,
    audio_sent: AtomicU64,
    dropped: AtomicU64,
    bytes: AtomicU64,
    connected: AtomicBool,
    error: Mutex<Option<String>>,
}

/// Pillanatkép a stream állapotáról.
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub sent: u64,
    pub audio_sent: u64,
    pub dropped: u64,
    pub bytes: u64,
    pub connected: bool,
    pub error: Option<String>,
}

/// RTMP publikáló: háttérszál + korlátos csatorna.
pub struct RtmpPublisher {
    tx: Option<SyncSender<Outgoing>>,
    stats: Arc<Stats>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RtmpPublisher {
    /// Kapcsolódás és a publikálás megkezdése (a háttérszálon).
    pub fn start(url: &str) -> Result<Self> {
        let (tx, rx) = sync_channel::<Outgoing>(16);
        let stats = Arc::new(Stats::default());
        let stats2 = Arc::clone(&stats);
        let url = url.to_string();
        let thread = std::thread::Builder::new()
            .name("dada-rtmp".to_string())
            .spawn(move || run(&url, rx, &stats2))
            .context("az RTMP szál nem indítható")?;
        Ok(Self {
            tx: Some(tx),
            stats,
            thread: Some(thread),
        })
    }

    /// Egy frame beillesztése. Ha a hálózat lassabb, a frame **eldobódik**
    /// (élő adásnál nem érdemes várni).
    pub fn send(&self, frame: VideoFrame) {
        self.push(Outgoing::Video(frame));
    }

    /// Egy hang-frame beillesztése (FLV/AAC csomagolással).
    pub fn send_audio(&self, frame: AudioFrame) {
        self.push(Outgoing::Audio(frame));
    }

    fn push(&self, outgoing: Outgoing) {
        if let Some(tx) = &self.tx {
            match tx.try_send(outgoing) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.stats.dropped.fetch_add(1, Ordering::SeqCst);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }

    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            sent: self.stats.sent.load(Ordering::SeqCst),
            audio_sent: self.stats.audio_sent.load(Ordering::SeqCst),
            dropped: self.stats.dropped.load(Ordering::SeqCst),
            bytes: self.stats.bytes.load(Ordering::SeqCst),
            connected: self.stats.connected.load(Ordering::SeqCst),
            error: self.stats.error.lock().ok().and_then(|e| e.clone()),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.stats.connected.load(Ordering::SeqCst)
    }

    /// Leállítás: a csatorna lezárása, a szál megvárása.
    pub fn stop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RtmpPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// rtmp://hoszt[:port]/app/stream-kulcs  ->  (hoszt, port, app, kulcs)
fn parse_url(url: &str) -> Result<(String, u16, String, String)> {
    let rest = url
        .strip_prefix("rtmp://")
        .ok_or_else(|| anyhow!("csak rtmp:// URL támogatott: {url}"))?;
    let (hostport, path) = rest
        .split_once('/')
        .ok_or_else(|| anyhow!("hiányzik az app/stream az URL-ből: {url}"))?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(1935)),
        None => (hostport.to_string(), 1935),
    };
    let (app, key) = path
        .split_once('/')
        .ok_or_else(|| anyhow!("hiányzik a stream kulcs az URL-ből: {url}"))?;
    if host.is_empty() || app.is_empty() || key.is_empty() {
        bail!("hiányos RTMP URL: {url}");
    }
    Ok((host, port, app.to_string(), key.to_string()))
}

fn run(url: &str, rx: Receiver<Outgoing>, stats: &Stats) {
    if let Err(e) = run_inner(url, rx, stats) {
        log::error!("RTMP hiba: {e:#}");
        if let Ok(mut slot) = stats.error.lock() {
            *slot = Some(format!("{e:#}"));
        }
    }
    stats.connected.store(false, Ordering::SeqCst);
}

fn write_results(stream: &mut TcpStream, results: Vec<ClientSessionResult>) -> Result<()> {
    for r in results {
        if let ClientSessionResult::OutboundResponse(packet) = r {
            stream.write_all(&packet.bytes)?;
        }
    }
    Ok(())
}

fn run_inner(url: &str, rx: Receiver<Outgoing>, stats: &Stats) -> Result<()> {
    let (host, port, app, key) = parse_url(url)?;
    let mut stream = TcpStream::connect((host.as_str(), port))
        .with_context(|| format!("nem lehet kapcsolódni: {host}:{port}"))?;
    stream.set_nodelay(true).ok();
    log::info!("RTMP kapcsolat: {host}:{port}, app={app}");

    // 1. Kézfogás.
    let mut hs = Handshake::new(PeerType::Client);
    stream.write_all(&hs.generate_outbound_p0_and_p1()?)?;
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            bail!("a szerver lezárta a kapcsolatot a kézfogás közben");
        }
        match hs.process_bytes(&buf[..n])? {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes)?;
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes, ..
            } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes)?;
                }
                break;
            }
        }
    }

    // 2. Session: connect -> publish.
    // Megjegyzés: az rml_rtmp szerver-oldali deszerializálója (amit a saját
    // teszfogadónk használ) megbotlik a több-chunkos üzeneteken, ezért a
    // teszfogadó csak kicsi frame-eket tud ellenőrizni. A kliens-oldali
    // szerializálást az rml_rtmp saját tesztjei fedik.
    let (mut session, initial) = ClientSession::new(ClientSessionConfig::new())?;
    write_results(&mut stream, initial)?;
    let connect = session.request_connection(app)?;
    write_results(&mut stream, vec![connect])?;
    wait_event(
        &mut session,
        &mut stream,
        |e| matches!(e, ClientSessionEvent::ConnectionRequestAccepted),
        Duration::from_secs(8),
        "connect",
    )?;
    let publish = session.request_publishing(key, PublishRequestType::Live)?;
    write_results(&mut stream, vec![publish])?;
    wait_event(
        &mut session,
        &mut stream,
        |e| matches!(e, ClientSessionEvent::PublishRequestAccepted),
        Duration::from_secs(8),
        "publish",
    )?;
    stats.connected.store(true, Ordering::SeqCst);
    log::info!("RTMP publikálás aktív");

    // 3. Frame-ek küldése.
    stream.set_read_timeout(Some(Duration::from_millis(2)))?;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(outgoing) => {
                let (data, pts_ms, is_audio) = match outgoing {
                    Outgoing::Video(f) => (f.data, f.pts_ms, false),
                    Outgoing::Audio(f) => (f.data, f.pts_ms, true),
                };
                let ts = RtmpTimestamp::new((pts_ms % (1u64 << 32)) as u32);
                let result = if is_audio {
                    session.publish_audio_data(Bytes::from(data), ts, false)?
                } else {
                    session.publish_video_data(Bytes::from(data), ts, false)?
                };
                if let ClientSessionResult::OutboundResponse(packet) = result {
                    stream.write_all(&packet.bytes)?;
                    if is_audio {
                        stats.audio_sent.fetch_add(1, Ordering::SeqCst);
                    } else {
                        stats.sent.fetch_add(1, Ordering::SeqCst);
                    }
                    stats.bytes.fetch_add(packet.bytes.len() as u64, Ordering::SeqCst);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        drain_input(&mut session, &mut stream)?;
    }
    Ok(())
}

/// A várt esemény megvárása (közben a kimenő válaszokat elküldjük).
fn wait_event(
    session: &mut ClientSession,
    stream: &mut TcpStream,
    pred: impl Fn(&ClientSessionEvent) -> bool,
    timeout: Duration,
    what: &str,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut buf = vec![0u8; 8192];
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => bail!("a szerver lezárta a kapcsolatot ({what})"),
            Ok(n) => {
                let results = session.handle_input(&buf[..n])?;
                // FONTOS: a batch összes kimenő csomagját el kell küldeni.
                // A kliens a connect elfogadása UTÁN küldi a SetChunkSize-t,
                // ezért a korai visszatérés elhagyná (a szerver 128-on maradna,
                // a kliens 4096-on küldene -> szinkronvesztés).
                let mut done = false;
                for r in results {
                    match r {
                        ClientSessionResult::OutboundResponse(packet) => {
                            stream.write_all(&packet.bytes)?;
                        }
                        ClientSessionResult::RaisedEvent(e) => {
                            if let ClientSessionEvent::ConnectionRequestRejected { description } =
                                &e
                            {
                                bail!("az RTMP kapcsolatot elutasították: {description}");
                            }
                            if pred(&e) {
                                done = true;
                            }
                        }
                        _ => {}
                    }
                }
                if done {
                    log::debug!("RTMP: {what} kész, minden válasz elküldve");
                    return Ok(());
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("olvasási hiba"),
        }
    }
    bail!("időtúllépés a(z) {what} eseményre ({} s)", timeout.as_secs())
}

/// A bejövő üzenetek kiszolgálása (ack, ping), hogy a szerver ne zárjon le.
fn drain_input(session: &mut ClientSession, stream: &mut TcpStream) -> Result<()> {
    let mut buf = vec![0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => bail!("a szerver lezárta a kapcsolatot"),
            Ok(n) => {
                let results = session.handle_input(&buf[..n])?;
                write_results(stream, results)?;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(())
            }
            Err(e) => return Err(e).context("olvasási hiba"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let (h, p, app, key) =
            parse_url("rtmp://a.rtmp.youtube.com/live2/abcd-1234-efgh").unwrap();
        assert_eq!(h, "a.rtmp.youtube.com");
        assert_eq!(p, 1935);
        assert_eq!(app, "live2");
        assert_eq!(key, "abcd-1234-efgh");

        let (h2, p2, app2, _) = parse_url("rtmp://127.0.0.1:1936/test/stream").unwrap();
        assert_eq!(h2, "127.0.0.1");
        assert_eq!(p2, 1936);
        assert_eq!(app2, "test");
    }

    #[test]
    fn url_errors() {
        assert!(parse_url("http://x/y/z").is_err());
        assert!(parse_url("rtmp://host/app").is_err());
        assert!(parse_url("rtmp:///app/key").is_err());
    }
}
