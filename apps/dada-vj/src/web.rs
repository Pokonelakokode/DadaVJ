//! Beépített web-szerver: vezérlő UI, állapot, parancsok, MJPEG előnézet.
//!
//! A szerver külön szálon fut; a render-szál csak a megosztott állapotot
//! frissíti (legutóbbi JPEG + JSON), és a parancsokat olvassa ki. Az engine
//! UI nélkül is teljes értékűen fut: ha nincs kliens, nincs költség.

use anyhow::{anyhow, Result};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A UI-ból érkező parancsok.
#[derive(Debug, Clone)]
pub enum Command {
    SetParam {
        index: usize,
        name: Option<String>,
        value: Option<f32>,
        values: Option<[f32; 4]>,
    },
    ResetParams,
    SetShader {
        path: Option<String>,
        opacity: Option<f32>,
        blend: Option<String>,
    },
    ReloadShader,
    SetMaster {
        master_opacity: Option<f32>,
        blackout: Option<bool>,
        video_opacity: Option<f32>,
        shader_opacity: Option<f32>,
        shader_blend: Option<String>,
        /// Az ISF effekt alpha-forrása: "coverage" (opacity fedés) vagy "shader".
        shader_alpha: Option<String>,
        /// Az ISF effekt cél rétege: -1 = legfelső kompozit, >= 0 = deck index.
        shader_target: Option<i64>,
        audio_opacity: Option<f32>,
        audio_blend: Option<String>,
    },
    SetClock {
        bpm: Option<f64>,
        playing: Option<bool>,
        reset_beat: bool,
        link: Option<bool>,
        /// A tap/beat-reset pillanata az engine monoton idején (másodperc).
        /// Ha megvan, a fázist ehhez a timestamphez horgonyozzuk.
        anchor_time: Option<f64>,
        /// Az anchor_time pillanatban érvényes ütem (tap esetén a tap sorszáma).
        anchor_beat: Option<f64>,
    },
    /// MIDI note szimulálása (ugyanazon az úton megy, mint a hardveres).
    Trigger {
        note: u8,
        velocity: u8,
    },
    /// Virtuális MIDI CC küldése.
    MidiCc {
        cc: u8,
        value: u8,
    },
    /// MIDI-motor beállítások (újracsatlakozás, port-szűrés, kick->shader mapping).
    SetMidi {
        rescan: bool,
        midi_visual: Option<bool>,
        enabled_ports: Option<Vec<String>>,
        toggle_port: Option<String>,
    },
    /// Felvétel/stream hangforrásának beállítása.
    SetCaptureAudio {
        enabled: Option<bool>,
        source: Option<String>,
        channel_spec: Option<String>,
    },
    /// Audio-motor és audio-vizuál réteg beállítása.
    SetAudio {
        visual: Option<bool>,
        device: Option<String>,
        /// -1 = Mix (minden csatorna), 0..31 = konkrét bemeneti csatorna.
        channel: Option<i32>,
        /// Sztereó pár vagy csatorna specifikáció (pl. "-1", "10", "10-11").
        channel_spec: Option<String>,
        opacity: Option<f32>,
        blend: Option<String>,
    },
    /// Beépített Master Post-FX (Color / Strobe / Flash / Invert / Hue / RGB Split).
    SetFx {
        flash: Option<f32>,
        invert: Option<f32>,
        hue: Option<f32>,
        saturation: Option<f32>,
        contrast: Option<f32>,
        rgb_split: Option<f32>,
        reset: bool,
    },
    /// Klip indítása a deck bankjából (kvantáltan vagy azonnal).
    Launch {
        deck: usize,
        clip: usize,
        immediate: bool,
    },
    /// Következő klip indítása a decken.
    DeckNext {
        deck: usize,
        immediate: bool,
    },
    /// Autopilot beállítása.
    SetAutopilot {
        deck: usize,
        mode: Option<String>,
        every_beats: Option<f64>,
        quantize: Option<String>,
    },
    /// A deck-rétegek Z-sorrendje (alulról felfelé, deck indexek).
    SetLayerOrder {
        order: Vec<usize>,
    },
    /// Deck opacity/blend/crossfade/speed beállítása.
    SetDeck {
        deck: usize,
        opacity: Option<f32>,
        blend: Option<String>,
        fade_beats: Option<f64>,
        speed: Option<f64>,
        /// Deck-szintű BPM sync ütemszám (0 = kikapcsolva).
        bpm_sync: Option<f64>,
        /// A kézi következő-klip gomb módja: "sequential" vagy "shuffle".
        next_mode: Option<String>,
        /// Deckenkénti chroma key (színkivétel).
        chroma_key: Option<bool>,
        chroma_color: Option<[f32; 3]>,
        chroma_tolerance: Option<f32>,
        chroma_softness: Option<f32>,
        chroma_spill: Option<f32>,
        restart: bool,
    },
    /// Klip-szintű beállítások (sebesség, súly, in_point, loop mód).
    SetClip {
        deck: usize,
        clip: usize,
        speed: Option<f64>,
        weight: Option<f32>,
        in_point: Option<f64>,
        loop_mode: Option<String>,
        /// 0 = kikapcsolva, > 0 = a klip hossza ennyi ütem legyen a BPM-hez igazítva.
        snap_beats: Option<f64>,
        /// Klipp-szintű chroma override módja: "inherit" | "custom" | "off".
        chroma_mode: Option<String>,
        chroma_color: Option<[f32; 3]>,
        chroma_tolerance: Option<f32>,
        chroma_softness: Option<f32>,
        chroma_spill: Option<f32>,
    },
    /// Új deck (mappa vagy videófájl) hozzáadása.
    AddDeck {
        folder: String,
        every_beats: Option<f64>,
        blend: Option<String>,
    },
    /// Egy vagy több konkrét videófájl / mappa hozzáadása meglévő vagy új deckhez.
    AddClips {
        deck: Option<usize>,
        paths: Vec<String>,
        every_beats: Option<f64>,
        blend: Option<String>,
    },
    /// Egyetlen klip eltávolítása egy deckből.
    RemoveClip {
        deck: usize,
        clip: usize,
    },
    /// Deck eltávolítása.
    RemoveDeck {
        deck: usize,
    },
    /// Modulációs útvonal hozzáadása.
    ModAdd {
        source: String,
        target: String,
        amount: f32,
        attack_ms: Option<f32>,
        hold_ms: Option<f32>,
        decay_ms: Option<f32>,
        sustain: Option<f32>,
        release_ms: Option<f32>,
        curve: Option<String>,
        retrigger: Option<bool>,
    },
    /// Meglévő modulációs útvonal módosítása.
    ModUpdate {
        index: usize,
        source: Option<String>,
        target: Option<String>,
        amount: Option<f32>,
        attack_ms: Option<f32>,
        hold_ms: Option<f32>,
        decay_ms: Option<f32>,
        sustain: Option<f32>,
        release_ms: Option<f32>,
        curve: Option<String>,
        retrigger: Option<bool>,
    },
    /// Modulációs útvonal törlése.
    ModRemove {
        index: usize,
    },
    /// Összes modulációs útvonal törlése.
    ModClear,
    /// MP4 felvétel indítása / leállítása.
    SetRecord {
        active: bool,
        path: Option<String>,
        fps: Option<f64>,
        bitrate_mbps: Option<f64>,
    },
    /// RTMP stream indítása / leállítása.
    SetStream {
        active: bool,
        url: Option<String>,
        fps: Option<u32>,
        bitrate_mbps: Option<f64>,
    },
    /// Kimeneti ablak / megjelenítés beállításai.
    SetOutput {
        width: Option<u32>,
        height: Option<u32>,
        vsync: Option<bool>,
        fullscreen: Option<bool>,
    },
    /// A jelenlegi felállás mentése projektfájlba.
    SaveProject {
        path: Option<String>,
    },
    /// Projektfájl betöltése futás közben.
    LoadProject {
        path: String,
    },
}

pub struct WebShared {
    jpeg: Mutex<Vec<u8>>,
    state: Mutex<String>,
    commands: Mutex<Vec<Command>>,
    /// (deck, clip) -> fájlútvonal, a thumbnail-generáláshoz.
    clips: Mutex<std::collections::HashMap<(usize, usize), String>>,
    /// Elkészült thumbnailek (fájlútvonal szerint cache-elve).
    thumbs: Mutex<std::collections::HashMap<String, Arc<Vec<u8>>>>,
    /// Egyszerre csak egy thumbnail-dekódolás fusson (VideoToolbox kímélése).
    thumb_gen_lock: Mutex<()>,
    seq: AtomicU64,
    /// A render/engine monoton órájának origója (az app indítási pillanata).
    clock_origin: Mutex<Instant>,
    pub jpegs_sent: AtomicU64,
}

impl WebShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            jpeg: Mutex::new(Vec::new()),
            state: Mutex::new("{}".to_string()),
            commands: Mutex::new(Vec::new()),
            clips: Mutex::new(std::collections::HashMap::new()),
            thumbs: Mutex::new(std::collections::HashMap::new()),
            thumb_gen_lock: Mutex::new(()),
            seq: AtomicU64::new(0),
            clock_origin: Mutex::new(Instant::now()),
            jpegs_sent: AtomicU64::new(0),
        })
    }

    /// Az app indítási pillanatának beállítása: innentől a web API és a
    /// render/engine ugyanazon a monoton idővonalon számol.
    pub fn set_clock_origin(&self, origin: Instant) {
        if let Ok(mut o) = self.clock_origin.lock() {
            *o = origin;
        }
    }

    /// Az engine idővonala másodpercben (a válasz pillanatában).
    pub fn now_secs(&self) -> f64 {
        self.clock_origin
            .lock()
            .map(|o| o.elapsed().as_secs_f64())
            .unwrap_or(0.0)
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

    /// A klipek útvonalainak beállítása (thumbnailhez).
    pub fn set_clips(&self, clips: Vec<((usize, usize), String)>) {
        let mut active_paths = std::collections::HashSet::new();
        if let Ok(mut c) = self.clips.lock() {
            c.clear();
            for (k, v) in clips {
                active_paths.insert(v.clone());
                c.insert(k, v);
            }
        }
        if let Ok(mut t) = self.thumbs.lock() {
            t.retain(|k, _| active_paths.contains(k));
        }
    }

    /// Egy klip thumbnailje (szükség esetén legenerálva, cache-elve).
    fn thumbnail(&self, deck: usize, clip: usize) -> Option<Arc<Vec<u8>>> {
        let path = self.clips.lock().ok()?.get(&(deck, clip)).cloned()?;
        if let Ok(t) = self.thumbs.lock() {
            if let Some(v) = t.get(&path) {
                return Some(Arc::clone(v));
            }
        }
        let _guard = self.thumb_gen_lock.lock().ok()?;
        if let Ok(t) = self.thumbs.lock() {
            if let Some(v) = t.get(&path) {
                return Some(Arc::clone(v));
            }
        }
        let t0 = std::time::Instant::now();
        match dada_media::macos::thumb::video_thumbnail(std::path::Path::new(&path), 240, 135, 75) {
            Ok(bytes) => {
                log::info!(
                    "thumbnail: deck {deck} klip {clip} ({} bájt, {:.1} ms)",
                    bytes.len(),
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                let arc = Arc::new(bytes);
                if let Ok(mut t) = self.thumbs.lock() {
                    t.insert(path, Arc::clone(&arc));
                }
                Some(arc)
            }
            Err(e) => {
                log::warn!("thumbnail hiba ({path}): {e:#}");
                None
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

fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.as_bytes().iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                let hex = [h, l];
                if let Ok(hex_str) = std::str::from_utf8(&hex) {
                    if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                        bytes.push(val);
                        continue;
                    }
                }
            }
            bytes.push(b'%');
        } else if b == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn browse_fs_json(raw_query_path: Option<&str>) -> String {
    let mut roots = Vec::new();

    // 1. Külső SSD-k és meghajtók a /Volumes alatt
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        let mut vols: Vec<(String, String)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None;
                }
                let path = e.path().to_string_lossy().to_string();
                Some((name, path))
            })
            .collect();
        vols.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path) in vols {
            roots.push(serde_json::json!({
                "name": format!("💾 {name}"),
                "path": path,
                "kind": "volume"
            }));
        }
    }

    // 2. Projekt mappa és helyi clips/
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        roots.push(serde_json::json!({
            "name": "📁 Projekt mappa",
            "path": cwd_str,
            "kind": "project"
        }));
        let clips_dir = cwd.join("clips");
        if clips_dir.is_dir() {
            roots.push(serde_json::json!({
                "name": "🎬 Helyi clips/",
                "path": clips_dir.to_string_lossy().to_string(),
                "kind": "clips"
            }));
        }
    }

    // 3. Felhasználói mappák (Home, Movies, Downloads, Desktop)
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::PathBuf::from(&home);
        roots.push(serde_json::json!({
            "name": "🏠 Saját mappa",
            "path": home,
            "kind": "home"
        }));
        for (label, sub) in [
            ("🎞️ Movies", "Movies"),
            ("⬇️ Downloads", "Downloads"),
            ("🖥️ Desktop", "Desktop"),
        ] {
            let p = home_path.join(sub);
            if p.is_dir() {
                roots.push(serde_json::json!({
                    "name": label,
                    "path": p.to_string_lossy().to_string(),
                    "kind": "user"
                }));
            }
        }
    }

    let default_dir = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("/Volumes"))
        .to_string_lossy()
        .to_string();
    let target_str = raw_query_path
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&default_dir);
    let target_path = std::path::PathBuf::from(target_str);
    let canonical = std::fs::canonicalize(&target_path).unwrap_or(target_path);
    let current = canonical.to_string_lossy().to_string();
    let parent = canonical.parent().map(|p| p.to_string_lossy().to_string());

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    if let Ok(rd) = std::fs::read_dir(&canonical) {
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.starts_with('.') {
                continue;
            }
            let p = entry.path();
            let p_str = p.to_string_lossy().to_string();
            if p.is_dir() {
                dirs.push((fname, p_str));
            } else if p.is_file() {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .unwrap_or_default();
                if matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "mkv" | "webm") {
                    let size_mb = entry
                        .metadata()
                        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                        .unwrap_or(0.0);
                    files.push((fname, p_str, size_mb));
                }
            }
        }
    }
    dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let dirs_json: Vec<_> = dirs
        .into_iter()
        .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
        .collect();
    let files_json: Vec<_> = files
        .into_iter()
        .map(|(name, path, size_mb)| {
            serde_json::json!({
                "name": name,
                "path": path,
                "size_mb": (size_mb * 10.0).round() / 10.0
            })
        })
        .collect();

    serde_json::json!({
        "current": current,
        "parent": parent,
        "roots": roots,
        "dirs": dirs_json,
        "files": files_json,
    })
    .to_string()
}

fn handle(mut request: tiny_http::Request, shared: &Arc<WebShared>) -> Result<()> {
    let url = request.url().to_string();
    let method = request.method().clone();
    // /api/fs?path=... — Fájl-, mappa- és külső SSD (/Volumes) böngésző
    if method == tiny_http::Method::Get && url.starts_with("/api/fs") {
        let mut query_path = None;
        if let Some((_, q)) = url.split_once('?') {
            for part in q.split('&') {
                if let Some((k, v)) = part.split_once('=') {
                    if k == "path" {
                        query_path = Some(url_decode(v));
                    }
                }
            }
        }
        let body = browse_fs_json(query_path.as_deref());
        return request
            .respond(
                tiny_http::Response::from_string(body)
                    .with_header(header("Content-Type", "application/json")),
            )
            .map_err(|e| anyhow!("{e}"));
    }

    // /thumb?deck=D&clip=C or /api/thumb/D/C
    if method == tiny_http::Method::Get
        && (url.starts_with("/thumb") || url.starts_with("/api/thumb"))
    {
        let (mut deck, mut clip) = (0usize, 0usize);
        if let Some(rest) = url.strip_prefix("/api/thumb/") {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() >= 2 {
                deck = parts[0].parse().unwrap_or(0);
                clip = parts[1].parse().unwrap_or(0);
            }
        } else if let Some((_, q)) = url.split_once('?') {
            for part in q.split('&') {
                if let Some((k, v)) = part.split_once('=') {
                    match k {
                        "deck" => deck = v.parse().unwrap_or(0),
                        "clip" => clip = v.parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
        }
        return match shared.thumbnail(deck, clip) {
            Some(bytes) => request
                .respond(
                    tiny_http::Response::from_data(bytes.as_ref().clone())
                        .with_header(header("Content-Type", "image/jpeg"))
                        .with_header(header("Cache-Control", "public, max-age=3600")),
                )
                .map_err(|e| anyhow!("{e}")),
            None => request
                .respond(tiny_http::Response::from_string("nincs thumbnail").with_status_code(404))
                .map_err(|e| anyhow!("{e}")),
        };
    }

    match (method, url.as_str()) {
        (tiny_http::Method::Get, "/") => {
            // Fejlesztés közben a lemezről olvassuk, hogy a UI szerkesztése
            // után ne kelljen újrabuildelni az egész binárist; ha a fájl nem
            // elérhető, a befordított változat a fallback.
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/index.html");
            let html = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| include_str!("../web/index.html").to_string());
            request.respond(
                tiny_http::Response::from_string(html)
                    .with_header(header("Content-Type", "text/html; charset=utf-8"))
                    .with_header(header(
                        "Cache-Control",
                        "no-store, no-cache, must-revalidate",
                    ))
                    .with_header(header("Pragma", "no-cache")),
            )?;
        }
        (tiny_http::Method::Get, "/api/state") => {
            let json = shared
                .state
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "{}".into());
            // A válasz pillanatában mért engine-idő: a UI ebből számolja a
            // böngésző performance.now() és az engine órája közti offsetet.
            let now = shared.now_secs();
            request.respond(
                tiny_http::Response::from_string(json)
                    .with_header(header("Content-Type", "application/json"))
                    .with_header(header("X-Dada-Time", &format!("{now:.6}"))),
            )?;
        }
        (tiny_http::Method::Post, "/api/command") => {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let mut ok = false;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                let str_opt = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string());
                let f32_opt = |k: &str| v.get(k).and_then(|x| x.as_f64()).map(|f| f as f32);
                let f64_opt = |k: &str| v.get(k).and_then(|x| x.as_f64());
                let bool_opt = |k: &str| v.get(k).and_then(|x| x.as_bool());

                match v.get("cmd").and_then(|c| c.as_str()) {
                    Some("set_param") => {
                        let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let name = str_opt("name");
                        let value = f32_opt("value");
                        let values = v.get("values").and_then(|arr| arr.as_array()).map(|a| {
                            let mut out = [0.0f32; 4];
                            for (i, x) in a.iter().take(4).enumerate() {
                                out[i] = x.as_f64().unwrap_or(0.0) as f32;
                            }
                            out
                        });
                        shared.push_command(Command::SetParam {
                            index,
                            name,
                            value,
                            values,
                        });
                        ok = true;
                    }
                    Some("reset_params") => {
                        shared.push_command(Command::ResetParams);
                        ok = true;
                    }
                    Some("set_shader") => {
                        shared.push_command(Command::SetShader {
                            path: str_opt("path"),
                            opacity: f32_opt("opacity"),
                            blend: str_opt("blend"),
                        });
                        ok = true;
                    }
                    Some("reload_shader") => {
                        shared.push_command(Command::ReloadShader);
                        ok = true;
                    }
                    Some("set_master") => {
                        shared.push_command(Command::SetMaster {
                            master_opacity: f32_opt("master_opacity")
                                .or_else(|| f32_opt("opacity")),
                            blackout: bool_opt("blackout"),
                            video_opacity: f32_opt("video_opacity"),
                            shader_opacity: f32_opt("shader_opacity"),
                            shader_blend: str_opt("shader_blend"),
                            shader_alpha: str_opt("shader_alpha"),
                            shader_target: v.get("shader_target").and_then(|x| x.as_i64()),
                            audio_opacity: f32_opt("audio_opacity"),
                            audio_blend: str_opt("audio_blend"),
                        });
                        ok = true;
                    }
                    Some("set_fx") => {
                        shared.push_command(Command::SetFx {
                            flash: f32_opt("flash"),
                            invert: f32_opt("invert"),
                            hue: f32_opt("hue"),
                            saturation: f32_opt("saturation"),
                            contrast: f32_opt("contrast"),
                            rgb_split: f32_opt("rgb_split"),
                            reset: bool_opt("reset").unwrap_or(false),
                        });
                        ok = true;
                    }
                    Some("set_clock") => {
                        shared.push_command(Command::SetClock {
                            bpm: f64_opt("bpm"),
                            playing: bool_opt("playing"),
                            reset_beat: bool_opt("reset_beat")
                                .or_else(|| bool_opt("reset_beats"))
                                .unwrap_or(false),
                            link: bool_opt("link"),
                            anchor_time: f64_opt("anchor_time").or_else(|| f64_opt("at")),
                            anchor_beat: f64_opt("anchor_beat").or_else(|| f64_opt("beat")),
                        });
                        ok = true;
                    }
                    Some("trigger") => {
                        let note = v.get("note").and_then(|n| n.as_u64()).unwrap_or(36) as u8;
                        let velocity =
                            v.get("velocity").and_then(|n| n.as_u64()).unwrap_or(100) as u8;
                        shared.push_command(Command::Trigger { note, velocity });
                        ok = true;
                    }
                    Some("midi_cc") => {
                        let cc = v.get("cc").and_then(|n| n.as_u64()).unwrap_or(1) as u8;
                        let value = v.get("value").and_then(|n| n.as_u64()).unwrap_or(0) as u8;
                        shared.push_command(Command::MidiCc { cc, value });
                        ok = true;
                    }
                    Some("set_midi") => {
                        let enabled_ports =
                            v.get("enabled_ports")
                                .and_then(|a| a.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                        .collect()
                                });
                        shared.push_command(Command::SetMidi {
                            rescan: bool_opt("rescan").unwrap_or(false),
                            midi_visual: bool_opt("midi_visual").or_else(|| bool_opt("visual")),
                            enabled_ports,
                            toggle_port: str_opt("toggle_port"),
                        });
                        ok = true;
                    }
                    Some("set_capture_audio") => {
                        shared.push_command(Command::SetCaptureAudio {
                            enabled: bool_opt("enabled"),
                            source: str_opt("source"),
                            channel_spec: str_opt("channel_spec")
                                .or_else(|| str_opt("channel")),
                        });
                        ok = true;
                    }
                    Some("set_audio") => {
                        let channel = v.get("channel").and_then(|n| n.as_i64()).map(|n| n as i32);
                        let channel_spec = str_opt("channel_spec")
                            .or_else(|| str_opt("channel"))
                            .or_else(|| channel.map(|c| c.to_string()));
                        shared.push_command(Command::SetAudio {
                            visual: bool_opt("visual"),
                            device: str_opt("device").or_else(|| str_opt("source")),
                            channel,
                            channel_spec,
                            opacity: f32_opt("opacity"),
                            blend: str_opt("blend"),
                        });
                        ok = true;
                    }
                    Some("launch") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let clip = v.get("clip").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let immediate = bool_opt("immediate").unwrap_or(false);
                        shared.push_command(Command::Launch {
                            deck,
                            clip,
                            immediate,
                        });
                        ok = true;
                    }
                    Some("deck_next") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let immediate = bool_opt("immediate").unwrap_or(false);
                        shared.push_command(Command::DeckNext { deck, immediate });
                        ok = true;
                    }
                    Some("autopilot") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::SetAutopilot {
                            deck,
                            mode: str_opt("mode"),
                            every_beats: f64_opt("every_beats"),
                            quantize: str_opt("quantize"),
                        });
                        ok = true;
                    }
                    Some("set_layer_order") => {
                        let order: Vec<usize> = v
                            .get("order")
                            .and_then(|a| a.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_u64().map(|n| n as usize))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !order.is_empty() {
                            shared.push_command(Command::SetLayerOrder { order });
                            ok = true;
                        }
                    }
                    Some("deck") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::SetDeck {
                            deck,
                            opacity: f32_opt("opacity"),
                            blend: str_opt("blend"),
                            fade_beats: f64_opt("fade_beats"),
                            speed: f64_opt("speed"),
                            bpm_sync: f64_opt("bpm_sync"),
                            next_mode: str_opt("next_mode"),
                            chroma_key: bool_opt("chroma_key"),
                            chroma_color: v
                                .get("chroma_color")
                                .and_then(|a| a.as_array())
                                .map(|arr| {
                                    let mut out = [0.0f32; 3];
                                    for (i, x) in arr.iter().take(3).enumerate() {
                                        out[i] = x.as_f64().unwrap_or(0.0) as f32;
                                    }
                                    out
                                }),
                            chroma_tolerance: f32_opt("chroma_tolerance"),
                            chroma_softness: f32_opt("chroma_softness"),
                            chroma_spill: f32_opt("chroma_spill"),
                            restart: bool_opt("restart").unwrap_or(false),
                        });
                        ok = true;
                    }
                    Some("set_clip") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let clip = v.get("clip").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::SetClip {
                            deck,
                            clip,
                            speed: f64_opt("speed"),
                            weight: f32_opt("weight"),
                            in_point: f64_opt("in_point"),
                            loop_mode: str_opt("loop_mode"),
                            snap_beats: f64_opt("snap_beats"),
                            chroma_mode: str_opt("chroma_mode"),
                            chroma_color: v
                                .get("chroma_color")
                                .and_then(|a| a.as_array())
                                .map(|arr| {
                                    let mut out = [0.0f32; 3];
                                    for (i, x) in arr.iter().take(3).enumerate() {
                                        out[i] = x.as_f64().unwrap_or(0.0) as f32;
                                    }
                                    out
                                }),
                            chroma_tolerance: f32_opt("chroma_tolerance"),
                            chroma_softness: f32_opt("chroma_softness"),
                            chroma_spill: f32_opt("chroma_spill"),
                        });
                        ok = true;
                    }
                    Some("add_deck") => {
                        if let Some(folder) = str_opt("folder").or_else(|| str_opt("path")) {
                            shared.push_command(Command::AddDeck {
                                folder,
                                every_beats: f64_opt("every_beats"),
                                blend: str_opt("blend"),
                            });
                            ok = true;
                        }
                    }
                    Some("add_clips") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).map(|n| n as usize);
                        let mut paths: Vec<String> = v
                            .get("paths")
                            .and_then(|a| a.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Some(single) = str_opt("path").or_else(|| str_opt("folder")) {
                            if !paths.contains(&single) {
                                paths.push(single);
                            }
                        }
                        if !paths.is_empty() {
                            shared.push_command(Command::AddClips {
                                deck,
                                paths,
                                every_beats: f64_opt("every_beats"),
                                blend: str_opt("blend"),
                            });
                            ok = true;
                        }
                    }
                    Some("remove_clip") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        let clip = v.get("clip").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::RemoveClip { deck, clip });
                        ok = true;
                    }
                    Some("remove_deck") => {
                        let deck = v.get("deck").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::RemoveDeck { deck });
                        ok = true;
                    }
                    Some("mod_add") => {
                        let source = str_opt("source").unwrap_or_default();
                        let target = str_opt("target").unwrap_or_default();
                        let amount = f32_opt("amount").unwrap_or(1.0);
                        shared.push_command(Command::ModAdd {
                            source,
                            target,
                            amount,
                            attack_ms: f32_opt("attack_ms"),
                            hold_ms: f32_opt("hold_ms"),
                            decay_ms: f32_opt("decay_ms"),
                            sustain: f32_opt("sustain"),
                            release_ms: f32_opt("release_ms"),
                            curve: str_opt("curve"),
                            retrigger: bool_opt("retrigger"),
                        });
                        ok = true;
                    }
                    Some("mod_update") => {
                        let index = v.get("index").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::ModUpdate {
                            index,
                            source: str_opt("source"),
                            target: str_opt("target"),
                            amount: f32_opt("amount"),
                            attack_ms: f32_opt("attack_ms"),
                            hold_ms: f32_opt("hold_ms"),
                            decay_ms: f32_opt("decay_ms"),
                            sustain: f32_opt("sustain"),
                            release_ms: f32_opt("release_ms"),
                            curve: str_opt("curve"),
                            retrigger: bool_opt("retrigger"),
                        });
                        ok = true;
                    }
                    Some("mod_remove") => {
                        let index = v.get("index").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        shared.push_command(Command::ModRemove { index });
                        ok = true;
                    }
                    Some("mod_clear") => {
                        shared.push_command(Command::ModClear);
                        ok = true;
                    }
                    Some("set_record") => {
                        let active = bool_opt("active")
                            .or_else(|| bool_opt("enabled"))
                            .unwrap_or(false);
                        let bitrate_mbps = f64_opt("bitrate_mbps")
                            .or_else(|| f64_opt("bitrate_kbps").map(|k| k / 1000.0));
                        shared.push_command(Command::SetRecord {
                            active,
                            path: str_opt("path"),
                            fps: f64_opt("fps"),
                            bitrate_mbps,
                        });
                        ok = true;
                    }
                    Some("set_stream") => {
                        let active = bool_opt("active")
                            .or_else(|| bool_opt("enabled"))
                            .unwrap_or(false);
                        let fps = v.get("fps").and_then(|n| n.as_u64()).map(|n| n as u32);
                        let bitrate_mbps = f64_opt("bitrate_mbps")
                            .or_else(|| f64_opt("bitrate_kbps").map(|k| k / 1000.0));
                        shared.push_command(Command::SetStream {
                            active,
                            url: str_opt("url"),
                            fps,
                            bitrate_mbps,
                        });
                        ok = true;
                    }
                    Some("set_output") => {
                        let width = v.get("width").and_then(|n| n.as_u64()).map(|n| n as u32);
                        let height = v.get("height").and_then(|n| n.as_u64()).map(|n| n as u32);
                        shared.push_command(Command::SetOutput {
                            width,
                            height,
                            vsync: bool_opt("vsync"),
                            fullscreen: bool_opt("fullscreen"),
                        });
                        ok = true;
                    }
                    Some("save_project") => {
                        shared.push_command(Command::SaveProject {
                            path: str_opt("path"),
                        });
                        ok = true;
                    }
                    Some("load_project") => {
                        if let Some(path) = str_opt("path") {
                            shared.push_command(Command::LoadProject { path });
                            ok = true;
                        }
                    }
                    _ => {}
                }
            }
            let reply = if ok {
                "{\"ok\":true}"
            } else {
                "{\"ok\":false}"
            };
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
