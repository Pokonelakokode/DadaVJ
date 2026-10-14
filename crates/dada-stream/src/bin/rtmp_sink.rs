//! Helyi RTMP fogadó (teszthez): kiírja, mit kap.
//!
//! Használat: rtmp_sink [port] [max_tagek]

use dada_stream::sink::serve_once;

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(19350);
    let max_tags: u64 = std::env::args()
        .nth(2)
        .and_then(|p| p.parse().ok())
        .unwrap_or(200);
    println!("RTMP fogadó a {port} porton (max {max_tags} videó-tag)");
    match serve_once(port, max_tags) {
        Ok(s) => {
            println!(
                "app={} kulcs={} · {} videó-tag, {} keyframe, {} audió-tag, {} bájt",
                s.app_name, s.stream_key, s.video_tags, s.keyframes, s.audio_tags, s.bytes
            );
            if !s.first_tag.is_empty() {
                println!(
                    "első tag: {:02X?}... ({} bájt)",
                    &s.first_tag[..s.first_tag.len().min(12)],
                    s.first_tag.len()
                );
            }
        }
        Err(e) => {
            eprintln!("fogadó hiba: {e:#}");
            std::process::exit(1);
        }
    }
}
