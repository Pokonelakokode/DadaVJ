//! Helyi RTMP fogadó (teszthez és hibakereséshez).
//!
//! Nem teljes szerver: egyetlen klienst fogad, elfogadja a connect/publish
//! kéréseket, és továbbadja a beérkező videó-tageket.

use anyhow::{bail, Result};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// Amit a fogadó összegyűjtött.
#[derive(Debug, Default, Clone)]
pub struct SinkStats {
    pub app_name: String,
    pub stream_key: String,
    pub video_tags: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Az első videó-tag (a sequence header).
    pub first_tag: Vec<u8>,
}

fn write_results(stream: &mut TcpStream, results: Vec<ServerSessionResult>) {
    for r in results {
        if let ServerSessionResult::OutboundResponse(packet) = r {
            let _ = stream.write_all(&packet.bytes);
        }
    }
}

/// Egy kliens kiszolgálása. A megadott tagszám elérése után visszatér.
pub fn serve_one(stream: TcpStream, max_tags: u64) -> Result<SinkStats> {
    let mut stream = stream;
    stream.set_nodelay(true).ok();
    // Időtúllépés: a teszt ne akadhasson be, ha a kliens elhallgat.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .ok();
    let mut stats = SinkStats::default();
    let mut buf = vec![0u8; 65536];

    // Kézfogás.
    let mut hs = Handshake::new(PeerType::Server);
    let mut leftover: Vec<u8> = Vec::new();
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            bail!("a kliens lezárta a kapcsolatot a kézfogás közben");
        }
        match hs.process_bytes(&buf[..n])? {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes)?;
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes)?;
                }
                leftover.extend_from_slice(&remaining_bytes);
                break;
            }
        }
    }

    let (mut session, initial) = ServerSession::new(ServerSessionConfig::new())?;
    write_results(&mut stream, initial);

    let mut handle = |session: &mut ServerSession,
                      stream: &mut TcpStream,
                      bytes: &[u8],
                      stats: &mut SinkStats| {
        let results = match session.handle_input(bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("handle_input hiba ({} bájt után): {e:?}", bytes.len());
                return;
            }
        };
        for r in results {
            match r {
                ServerSessionResult::OutboundResponse(packet) => {
                    let _ = stream.write_all(&packet.bytes);
                }
                ServerSessionResult::RaisedEvent(event) => match event {
                    ServerSessionEvent::ConnectionRequested {
                        request_id,
                        app_name,
                    } => {
                        stats.app_name = app_name;
                        if let Ok(res) = session.accept_request(request_id) {
                            write_results(stream, res);
                        }
                    }
                    ServerSessionEvent::PublishStreamRequested {
                        request_id,
                        stream_key,
                        ..
                    } => {
                        stats.stream_key = stream_key;
                        if let Ok(res) = session.accept_request(request_id) {
                            write_results(stream, res);
                        }
                    }
                    ServerSessionEvent::ReleaseStreamRequested { request_id, .. } => {
                        if let Ok(res) = session.accept_request(request_id) {
                            write_results(stream, res);
                        }
                    }
                    ServerSessionEvent::VideoDataReceived { data, .. } => {
                        if stats.video_tags == 0 {
                            stats.first_tag = data.to_vec();
                        }
                        // frame type 1 = keyframe (a felső nibble),
                        // AVCPacketType 1 = NALU (nem sequence header).
                        if data.len() > 1
                            && data[1] == 0x01
                            && (data[0] & 0xF0) == 0x10
                        {
                            stats.keyframes += 1;
                        }
                        stats.video_tags += 1;
                        stats.bytes += data.len() as u64;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    };

    if !leftover.is_empty() {
        let l = leftover.clone();
        handle(&mut session, &mut stream, &l, &mut stats);
    }

    while stats.video_tags < max_tags {
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                eprintln!("a kliens lezárta ({} tag után)", stats.video_tags);
                break;
            }
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                eprintln!("olvasási időtúllépés ({} tag után)", stats.video_tags);
                break;
            }
            Err(e) => {
                eprintln!("olvasási hiba: {e:?}");
                break;
            }
        };
        handle(&mut session, &mut stream, &buf[..n], &mut stats);
        if std::env::var("DADA_SINK_VERBOSE").is_ok() {
            eprintln!("  {n} bájt -> {} tag", stats.video_tags);
        }
    }
    Ok(stats)
}

/// Egyetlen kapcsolat kiszolgálása egy adott porton.
pub fn serve_once(port: u16, max_tags: u64) -> Result<SinkStats> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (stream, _) = listener.accept()?;
    serve_one(stream, max_tags)
}
