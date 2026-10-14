//! Beépített web-szerver: vezérlő UI, állapot, parancsok, MJPEG előnézet.
//!
//! A szerver külön szálon fut; a render-szál csak a megosztott állapotot
//! frissíti (legutóbbi JPEG + JSON), és a parancsokat olvassa ki. Az engine
//! UI nélkül is teljes értékűen fut: ha nincs kliens, nincs költség.

use anyhow::{anyhow, Result};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A UI-ból érkező parancsok.
#[derive(Debug, Clone)]
pub enum Command {
    SetParam { index: usize, value: f32 },
    /// MIDI note szimulálása (ugyanazon az úton megy, mint a hardveres).
    Trigger { note: u8, velocity: u8 },
    /// Klip indítása a deck bankjából (kvantáltan).
    Launch { deck: usize, clip: usize },
    /// Autopilot beállítása.
    SetAutopilot {
        deck: usize,
        mode: String,
        every_beats: f64,
    },
    /// Deck opacity/blend beállítása.
    SetDeck {
        deck: usize,
        opacity: Option<f32>,
        blend: Option<String>,
    },
    /// Modulációs útvonal hozzáadása.
    ModAdd {
        source: String,
        target: String,
        amount: f32,
    },
    /// Modulációs útvonal törlése.
    ModRemove { index: usize },
    /// A jelenlegi felállás mentése projektfájlba.
    SaveProject { path: Option<String> },
}

pub struct WebShared {
    jpeg: Mutex<Vec<u8>>,
    state: Mutex<String>,
    commands: Mutex<Vec<Command>>,
    seq: AtomicU64,
    pub jpegs_sent: AtomicU64,
}

impl WebShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            jpeg: Mutex::new(Vec::new()),
            state: Mutex::new("{}".to_string()),
            commands: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            jpegs_sent: AtomicU64::new(0),
        })
    }

    pub fn push_jpeg(&self, data: Vec<u8>) {
        if let Ok(mut j) = self.jpeg.lock() {
            *j = data;
        }
        self.seq.fetch_add(1, Ordering::Release);
    }

    pub fn set_state(&self, json: String) {
        if let Ok(mut s) = self.state.lock() {
            *s = json;
        }
    }

    pub fn take_commands(&self) -> Vec<Command> {
        self.commands
            .lock()
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default()
    }

    fn push_command(&self, cmd: Command) {
        if let Ok(mut c) = self.commands.lock() {
            if c.len() < 256 {
                c.push(cmd);
            }
        }
    }

    fn snapshot_jpeg(&self) -> (Vec<u8>, u64) {
        (
            self.jpeg.lock().map(|j| j.clone()).unwrap_or_default(),
            self.seq.load(Ordering::Acquire),
        )
    }
}

/// A web-szerver indítása háttérszálon.
pub fn spawn(port: u16, shared: Arc<WebShared>) -> Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow!("a web-szerver nem indítható a {port} porton: {e}"))?;
    std::thread::Builder::new()
        .name("dada-web".into())
        .spawn(move || {
            log::info!("vezérlő UI: http://127.0.0.1:{port}/");
            // Kérésenként külön szál: a hosszú életű MJPEG stream nem blokkolhatja
            // a UI állapot-kéréseit (a tiny_http alapból sorban szolgálna ki).
            for request in server.incoming_requests() {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("dada-web-req".into())
                    .spawn(move || {
                        if let Err(e) = handle(request, &shared) {
                            log::debug!("web kérés hiba: {e}");
                        }
                    })
                    .ok();
            }
        })?;
    Ok(())
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn handle(mut request: tiny_http::Request, shared: &Arc<WebShared>) -> Result<()> {
    let url = request.url().to_string();
    let method = request.method().clone();
    match (method, url.as_str()) {
        (tiny_http::Method::Get, "/") => {
            let html = include_str!("../web/index.html");
            request.respond(
                tiny_http::Response::from_string(html).with_header(header(
                    "Content-Type",
                    "text/html; charset=utf-8",
                )),
            )?;
        }
        (tiny_http::Method::Get, "/api/state") => {
            let json = shared
                .state
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "{}".into());
            request.respond(
                tiny_http::Response::from_string(json)
                    .with_header(header("Content-Type", "application/json")),
            )?;
        }
        (tiny_http::Method::Post, "/api/command") => {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let mut ok = false;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                match v.get("cmd").and_then(|c| c.as_str()) {
                    Some("set_param") => {
                        let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let value = v.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                        shared.push_command(Command::SetParam { index, value });
                        ok = true;
                    }
                    Some("trigger") => {
                        let note = v.get("note").and_then(|n| n.as_u64()).unwrap_or(36) as u8;
                        let velocity =
                            v.get("velocity").and_then(|n| n.as_u64()).unwrap_or(100) as u8;
                        shared.push_command(Command::Trigger { note, velocity });
                        ok = true;
                    }
                    Some("launch") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let clip = v.get("clip").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::Launch { deck, clip });
                        ok = true;
                    }
                    Some("autopilot") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let mode = v
                            .get("mode")
                            .and_then(|m| m.as_str())
                            .unwrap_or("off")
                            .to_string();
                        let every_beats =
                            v.get("every_beats").and_then(|n| n.as_f64()).unwrap_or(16.0);
                        shared.push_command(Command::SetAutopilot {
                            deck,
                            mode,
                            every_beats,
                        });
                        ok = true;
                    }
                    Some("deck") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let opacity = v.get("opacity").and_then(|n| n.as_f64()).map(|f| f as f32);
                        let blend = v
                            .get("blend")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                        shared.push_command(Command::SetDeck {
                            deck,
                            opacity,
                            blend,
                        });
                        ok = true;
                    }
                    Some("mod_add") => {
                        let source = v
                            .get("source")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        let target = v
                            .get("target")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        let amount = v.get("amount").and_then(|n| n.as_f64()).unwrap_or(1.0) as f32;
                        shared.push_command(Command::ModAdd {
                            source,
                            target,
                            amount,
                        });
                        ok = true;
                    }
                    Some("mod_remove") => {
                        let index = v.get("index").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::ModRemove { index });
                        ok = true;
                    }
                    Some("save_project") => {
                        let path = v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string());
                        shared.push_command(Command::SaveProject { path });
                        ok = true;
                    }
                    _ => {}
                }
            }
            let reply = if ok { "{\"ok\":true}" } else { "{\"ok\":false}" };
            request.respond(
                tiny_http::Response::from_string(reply)
                    .with_header(header("Content-Type", "application/json")),
            )?;
        }
        (tiny_http::Method::Get, "/preview.mjpg") => {
            let stream = MjpegStream {
                shared: Arc::clone(shared),
                last_seq: 0,
                pending: Vec::new(),
            };
            let response = tiny_http::Response::new(
                tiny_http::StatusCode(200),
                vec![header(
                    "Content-Type",
                    "multipart/x-mixed-replace; boundary=frame",
                )],
                stream,
                None,
                None,
            );
            request.respond(response)?;
        }
        _ => {
            request.respond(tiny_http::Response::from_string("404").with_status_code(404))?;
        }
    }
    Ok(())
}

/// Végtelen MJPEG stream: mindig a legfrissebb JPEG-et küldi.
struct MjpegStream {
    shared: Arc<WebShared>,
    last_seq: u64,
    pending: Vec<u8>,
}

impl Read for MjpegStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            // Megvárjuk a következő frame-et (max ~2 s), hogy ne pörögjön feleslegesen.
            let mut waited = 0;
            loop {
                let (jpeg, seq) = self.shared.snapshot_jpeg();
                if seq != self.last_seq && !jpeg.is_empty() {
                    self.last_seq = seq;
                    self.shared.jpegs_sent.fetch_add(1, Ordering::Relaxed);
                    let head = format!(
                        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        jpeg.len()
                    );
                    self.pending.extend_from_slice(head.as_bytes());
                    self.pending.extend_from_slice(&jpeg);
                    self.pending.extend_from_slice(b"\r\n");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
                waited += 5;
                if waited > 2000 {
                    // Keep-alive: üres multipart chunk.
                    self.pending.extend_from_slice(b"\r\n");
                    break;
                }
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}
