//! Integrációs teszt: egy helyi RTMP szerver fogadja a publikálót.
//!
//! Ez a teljes láncot igazolja: TCP kapcsolat, RTMP kézfogás, connect,
//! createStream, publish, majd a videó-tagek megérkezése.

use dada_stream::flv;
use dada_stream::{RtmpPublisher, VideoFrame};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn send(stream: &mut TcpStream, results: Vec<ServerSessionResult>) {
    for r in results {
        if let ServerSessionResult::OutboundResponse(packet) = r {
            let _ = stream.write_all(&packet.bytes);
        }
    }
}

/// A szerver oldal: kézfogás, majd connect/publish elfogadása és a videó gyűjtése.
fn serve(mut stream: TcpStream) -> Vec<Vec<u8>> {
    stream.set_nodelay(true).ok();
    let mut hs = Handshake::new(PeerType::Server);
    let mut buf = vec![0u8; 16384];
    let mut leftover: Vec<u8> = Vec::new();
    loop {
        let n = stream.read(&mut buf).expect("kézfogás olvasás");
        assert!(n > 0, "a kliens lezárta a kapcsolatot a kézfogás közben");
        match hs.process_bytes(&buf[..n]).expect("kézfogás feldolgozás") {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
                leftover.extend_from_slice(&remaining_bytes);
                break;
            }
        }
    }

    let (mut session, initial) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    send(&mut stream, initial);

    let mut video: Vec<Vec<u8>> = Vec::new();
    let mut feed = |session: &mut ServerSession,
                    stream: &mut TcpStream,
                    bytes: &[u8],
                    video: &mut Vec<Vec<u8>>| {
        let results = session.handle_input(bytes).unwrap();
        for r in results {
            match r {
                ServerSessionResult::OutboundResponse(packet) => {
                    let _ = stream.write_all(&packet.bytes);
                }
                ServerSessionResult::RaisedEvent(event) => match event {
                    ServerSessionEvent::ConnectionRequested { request_id, .. }
                    | ServerSessionEvent::PublishStreamRequested { request_id, .. }
                    | ServerSessionEvent::ReleaseStreamRequested { request_id, .. } => {
                        if let Ok(res) = session.accept_request(request_id) {
                            send(stream, res);
                        }
                    }
                    ServerSessionEvent::VideoDataReceived { data, .. } => {
                        video.push(data.to_vec());
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    };

    if !leftover.is_empty() {
        let l = leftover.clone();
        feed(&mut session, &mut stream, &l, &mut video);
    }

    while video.len() < 5 {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        feed(&mut session, &mut stream, &buf[..n], &mut video);
    }
    video
}

#[test]
fn publishes_video_to_a_local_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve(stream)
    });

    let url = format!("rtmp://127.0.0.1:{port}/live/test-key");
    let mut publisher = RtmpPublisher::start(&url).expect("a publikáló elindul");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !publisher.is_connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        publisher.is_connected(),
        "a publikáló nem kapcsolódott: {:?}",
        publisher.stats().error
    );

    let avcc = flv::avcc_from_sps_pps(&[0x67, 0x64, 0x28, 0xAA], &[0x68, 0xBB]);
    publisher.send(VideoFrame {
        data: flv::avc_sequence_header(&avcc),
        pts_ms: 0,
    });
    let nals = [0u8, 0, 0, 2, 0x65, 0x11];
    for i in 1..=4u64 {
        publisher.send(VideoFrame {
            data: flv::avc_frame(&nals, i == 1, 0),
            pts_ms: i * 33,
        });
    }

    let received = server.join().expect("a szerver szál elszállt");
    assert!(
        received.len() >= 5,
        "a szerver csak {} videó-csomagot kapott",
        received.len()
    );

    // Az első a sequence header.
    assert_eq!(received[0][0], 0x17, "sequence header frame típus");
    assert_eq!(received[0][1], 0x00, "AVCPacketType: sequence header");
    assert_eq!(&received[0][2..5], &[0, 0, 0], "composition time");
    assert_eq!(received[0][5], 0x01, "configurationVersion");
    // a tag: 5 bájt FLV fejléc + 5 bájt avcC fejléc, utána az SPS
    assert_eq!(
        &received[0][13..17],
        &[0x67, 0x64, 0x28, 0xAA],
        "SPS az avcC-ben"
    );

    // A második egy keyframe NALU.
    assert_eq!(received[1][0], 0x17, "keyframe + AVC");
    assert_eq!(received[1][1], 0x01, "AVCPacketType: NALU");
    assert_eq!(&received[1][5..], &nals, "a NALU-k változatlanul mennek át");

    // A többi interframe.
    assert_eq!(received[2][0], 0x27, "inter frame + AVC");

    let stats = publisher.stats();
    println!(
        "RTMP teszt: {} frame elküldve, {} bájt, {} eldobva",
        stats.sent, stats.bytes, stats.dropped
    );
    assert_eq!(stats.sent, 5, "a küldött frame-ek száma");
}

/// Nagy (20 KB) videó-frame — a több-chunkos üzenetek útja.
#[test]
#[ignore = "ismert rml_rtmp szerver-oldali deszerializalo hiba tobb-chunkos uzeneteknel"]
fn large_video_frames_round_trip() {
    use dada_stream::sink::serve_one;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve_one(stream, 12).expect("a fogadó hibázott")
    });

    let url = format!("rtmp://127.0.0.1:{port}/live/nagy");
    let mut publisher = RtmpPublisher::start(&url).expect("publikáló");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !publisher.is_connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(publisher.is_connected(), "nem kapcsolódott: {:?}", publisher.stats().error);

    // Egy 20 KB-os "NALU" (több chunk).
    let big = vec![0x42u8; 20_000];
    publisher.send(VideoFrame {
        data: flv::avc_frame(&big, true, 0),
        pts_ms: 0,
    });
    for i in 1..=10u64 {
        publisher.send(VideoFrame {
            data: flv::avc_frame(&big, false, 0),
            pts_ms: i * 33,
        });
    }

    let stats = server.join().expect("szerver szál");
    println!(
        "nagy frame teszt: {} tag, {} keyframe, {} bájt, első tag {} bájt",
        stats.video_tags,
        stats.keyframes,
        stats.bytes,
        stats.first_tag.len()
    );
    assert!(
        stats.video_tags >= 11,
        "csak {} tag érkezett meg a 11-ből",
        stats.video_tags
    );
}

