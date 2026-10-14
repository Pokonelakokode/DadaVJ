//! Link teszt-adó: szabályos tmln csomagokat küld a Link portra.
//!
//! Használat: link_sender [bpm] [másodperc]

use dada_link::{build_timeline_packet, now_link_us, Timeline, LINK_PORT};

fn main() {
    let bpm: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(128.0);
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let micros = (60_000_000.0 / bpm).round() as u64;
    let now = now_link_us();
    let tl = Timeline {
        micros_per_beat: micros,
        beat_origin: 0.0,
        time_origin_us: now,
    };
    let packet = build_timeline_packet(&tl);
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("UDP foglalás");
    println!("LINK-ADÓ: {bpm} BPM, {secs} s, {LINK_PORT} port, időeredet {now} us");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut sent = 0u32;
    while std::time::Instant::now() < end {
        if sock.send_to(&packet, ("127.0.0.1", LINK_PORT)).is_ok() {
            sent += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    println!("LINK-ADÓ: kész, {sent} csomag");
}
