# Dada VJ — terv v2 (a döntéseid után)

*Frissítve: 2026-10-14 · Célmachina: MacBook Pro (Mac16,7), Apple M4 Pro, 20 GPU mag, 24 GB, macOS 26.6.2*

> **v2 változások a v1-hez képest:** web UI (nem egui) · nincs VST/AU host, csak audio-elemzés ·
> a backend **streamel** (előnézet a UI-nak + opcionálisan YouTube) · ISF-first generatív motor, MilkDrop/projectM később ·
> 1080p60 a fő target (4K opcionális) · lemezhely-kockázat törölve (külső SSD).
> Köszönöm a válaszokat — **a felállás így egyszerűbb és gyorsabb, mint az eredeti terv.**

---

## 0. A legfontosabb kérdés: „a LEGHATÉKONYABB legyen"

**A nyelv nem lesz a szűk keresztmetszet — az architektúra az.** Ez a jó hír.

Ezen a munkaterhelésen (videó dekódolás + GPU kompozitálás + audio FFT) a nehéz munkát a **médiamotor**,
a **GPU** és a **CoreAudio/CoreMIDI** végzi, nem a nyelv. A C++ és a Rust között mérhető különbség a valóságban
1–3% körül van, ami 1080p60-nál nem számít. Ami számít:

| Ami tényleg a teljesítményt adja | Nem pedig |
|---|---|
| Zero-copy (VideoToolbox → IOSurface → Metal textúra, nincs CPU-másolás) | a nyelv megválasztása |
| Hardveres dekód (media engine) és hardveres H.264 encode a streamhez | kézi SIMD optimalizálás |
| Nulla allokáció a frame-útban, textúrapool | „okos" adatszerkezetek |
| Korlátos dekóder-sorok + frame-drop policy (a zenei óra az úr) | több szál mindenre |
| 1080p belső render, inaktív layer kihagyása | 4K mindenáron |

Ezt a Rust × wgpu/Metal pontosan tudja, ugyanolyan jól, mint egy C++/Metal megoldás — közben viszont nem lesz
memóriahiba, nincs GC-pausa, és egy kódbázisból lesz Metal / Vulkan / DX12.

**És ami neked fontos: nem kell Rustot tanulnod ahhoz, hogy új vizuálokat csinálj.** Három bővítési felület lesz,
amiből kettőhöz nulla Rust kell:

| Felület | Nyelv | Mire jó |
|---|---|---|
| **ISF / Shadertoy shaderek** | GLSL | új effektek, generatív vizuálok, audio-reaktív képek — hot-reload, azonnal |
| **Web UI** | TypeScript / React | saját vezérlőfelület, saját gombok, saját logika a UI-ban |
| **WASM plugin** (később) | Rust / C / AssemblyScript / JS | egyedi viselkedés (pl. „melyik clip jöjjön" szabályok) homokozva |

A Rust marad a motorban; te a shaderekkel és a UI-jal játszol. Ha később bele akarsz tanulni, a kódbázis
moduláris és kicsi lesz — de nem ez a belépési feltétel.

---

## 1. A hardver és az Ableton-együttélés

| | |
|---|---|
| Chip | Apple M4 Pro, 14 CPU mag (10P + 4E), **20 GPU mag**, Metal 4 |
| Memória | 24 GB unified, ~273 GB/s |
| Médiamotor | H.264 / HEVC / ProRes / ProRes RAW **hardveres encode + decode**, AV1 decode |
| Kimenet | 1080p60 a fő target (a 4K TV-n upscale-elve is tökéletes); 4K opcionális később |
| Lemez | külső SSD — nem kockázat többé |

**Fontos: a Live és a Dada VJ ugyanazon a gépen fut egyszerre.** Ez tervezési követelmény, nem mellékes:

- A VJ app **hardveres dekódot és hardveres encodet** használ → a CPU-t szinte nem terheli.
- 1080p60, 3–4 layer: a 20 magos GPU-nak ez tervezetten **15–30%** közötti terhelése, a CPU-n 1–2 mag.
- Az audio-elemzés (FFT + sávok + onset) **egy mag 5%-a alatt** van.
- A streamhez a H.264 encodert a **médiamotor** adja (VideoToolbox), nem a CPU.
- A render-szál QoS-t „utility"-re tesszük, nem versenyzünk a Live audio-szálával; nincs busy-wait.
- Lesz egy **költségvetés-HUD** (GPU-ms, CPU-ms, kimaradt frame, hőfok), és automatikus visszafogás, ha szűkül a keret.

Összességében: **Live 12 + ez a VJ app 1080p60-ban kényelmesen elfér egyszerre.** 4K-ban is menne, de nem érdemes
(miközben zenéltek, a tartalék GPU/CPU a biztosíték, nem a luxus).

---

## 2. Amit kértél → hogyan lesz meg

| Kért | Megoldás |
|---|---|
| A lehető leghatékonyabb ezen a Macen | Rust + `wgpu`/Metal, zero-copy videó út, hardveres dekód/encode, nulla allokáció a frame-útban |
| Hordozható más platformra | ugyanaz a kód Metal / Vulkan / DX12-re; a macOS-specifikus részek (VideoToolbox, Syphon) trait mögé rejtve |
| Moduláris, elterjedt pluginokkal | **ISF** (VJ-ipari shader szabvány) + Shadertoy import + `.milk` (később) + Syphon/Spout/NDI + MIDI/OSC/WebSocket |
| MIDI clock | 24 PPQN + PLL simítás, Start/Continue/Stop/SPP, több forrás fallbackkel, **Ableton Link** (nálad adja magát) |
| Több MIDI forrás, note + CC | portonként külön forrás-azonosító, forrásonkénti routing, learn mód |
| Hatékony videóformátumok | ProRes (hardveres, alpha 4444) + HAP (GPU-dekód) + all-intra H.264/HEVC (kicsi, hardveres) + kép/generatív |
| Beat-alapú indítás, kézi felülírással | per-layer kvantálás + **autopilot** (N ütem után vált) + kézi trigger bármikor |
| 3–4 layer | 4 layer alapból (2–8 konfigurálható) |
| Effekt layer dob-MIDI-vel, envelope lecsengéssel | **modulációs mátrix** + AR/ADSR envelope generátor (attack 0 ms, decay állítható) |
| MilkDrop / generatív | **ISF + Shadertoy motor elsőként** (ez a modernebb), projectM/MilkDrop a 2. fázisban |
| **Audio input mint vizuál** | **audio → GPU textúra** szabvány (waveform + spektrum + **spektrogram**), amit bármely shader fogyaszthat; emellett audio-sávok mint modulációs források |
| Ne ölje meg a gépet | lásd az 1. pontot + költségvetés-HUD + automatikus visszafogás |

---

## 3. Architektúra (v2)

```
        ┌─────────────────────────── Dada VJ engine (Rust, native, UI nélkül is fut) ───────────────────────────┐
        │                                                                                                        │
  MIDI ─┤ dada-midi: clock PLL · note/CC routing · learn                                                         │
  Link ─┤                                                                                                        │
 Audio ─┤ dada-audio: FFT · sávok · onset · envelope follower                                                     │
        │        └──────────────► AUDIO TEXTÚRA (waveform / spektrum / spektrogram) ──┐                            │
        │                                                                             │                            │
        │ dada-core: Transport (beats) · ütemező · Cue/Scene · MODULÁCIÓS MÁTRIX      │                            │
        │                                     │                                       │                            │
        │ dada-render (wgpu/Metal): Layer A..D ─ FX-lánc ─ kompozit ─ master          │                            │
        │        ▲                      ▲                                             │                            │
        │   dada-media             dada-gen: ISF/Shadertoy motor  ◄────────────────────┘                            │
        │   (VideoToolbox,         (+ később projectM/MilkDrop)                                                     │
        │    HAP, FFmpeg, kép)                                                                                       │
        │                                     │                                                                       │
        │                          ┌──────────┴─────────── OUTPUT SINKS ──────────────┐                              │
        │                          ▼                    ▼                    ▼         ▼                              │
        │                   Projektor/ablak      Előnézet stream      RTMP → YouTube   Felvétel (mp4)                 │
        │                   1080p60 fullscreen   (MJPEG/WebRTC)       (H.264 HW enc)   (opcionális)                  │
        └────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                                              ▲
                                              │  HTTP + WebSocket (vezérlés)  ·  MJPEG/WebRTC (előnézet)
                                              ▼
                              ┌──────────────────────────────────────────┐
                              │  Web UI (Vite + TypeScript)              │
                              │  clip grid · mixer · mapping · médiatár   │
                              │  fut böngészőben, tableten, telefonon     │
                              └──────────────────────────────────────────┘
```

**Alapszabályok:**

1. **Az engine önálló.** Ha a böngésző bezárul, lefagy vagy újratölt, a show **zavartalanul megy tovább** (MIDI-vel vezérelve is).
2. **A UI sosem a kritikus úton van.** A vezérlés WebSocketen megy, a videó külön csatornán (előnézet).
3. **Minden kimenet egy `OutputSink`.** Ugyanaz a kész frame megy a projektorra, az előnézetbe, a YouTube-ra és a felvételbe — egy render, több fogyasztó.
4. **Minden esemény zenei időben** (beat) van ütemezve, a render-szál fordítja frame-re.

---

## 4. Audio: a jelből vizuál (ez most a fő funkció)

Ez a te 2-es pontod, ezért első osztályú állampolgár lesz, nem melléktermék.

### 4.1 Honnan jön a hang?

Három lehetőség, fontossági sorrendben:

| # | Forrás | Mit kapsz | Megjegyzés |
|---|---|---|---|
| 1 | **Ableton Live master kimenete** virtuális loopback eszközön (BlackHole — ingyenes, nyílt forrású; vagy Loopback; vagy a Focusrite Scarlett 4. gen **Loopback** funkciója) | pontosan azt, amit a közönség hall: a kész mixet, effektekkel, limiterrel | ⭐ ez a javaslat — a vizuál a *zenére* reagál, nem a nyers DI jelre |
| 2 | Ugyanaz a bemenet, amit a Live is használ (a Focusrite bemenete) | a nyers jelet (FX előtt) | egyszerű, de a vizuál korábban „látja" a hangot, mint a fül |
| 3 | Mikrofon a szobából | a szoba hangját | csak vésztartalék |

A Live és a VJ app **ugyanazt az eszközt is megnyithatja** macOS-en (a CoreAudio megosztott), de a legjobb élmény
az 1-es: Ableton fő kimenete menjen a Focusrite-ra **és** a BlackHole-ba (aggregate/multi-output), a VJ app a BlackHole-t hallgatja.
Így a vizuál és a stream audio is pontosan az, ami megszólal.

### 4.2 Audio → textúra (a „spektrogram" igény megoldása)

Az elemzés eredménye nem csak számokban megy tovább, hanem **GPU textúrákban**, amit bármely shader olvashat:

| Textúra | Tartalom | Kik használják |
|---|---|---|
| `audio` 512×2 | 0. sor: hullámforma · 1. sor: spektrum | **ISF szabvány** — a meglévő audio-reaktív ISF shaderek azonnal működnek |
| `audioFFT` 512×2 | FFT + simított csúcsérték | ISF szabvány |
| `spectrogram` 512×256 | **gördülő spektrogram-előzmény** (FFT-hoponként egy oszlop, ~5–10 s előzmény) | a saját és a te shadereid |
| `waveform` 1024×1 | nagy felbontású, triggerelt hullámforma (oscilloszkóp) | oscilloscope shaderek |

Mindez **minden frame-ben frissül, feltöltési költsége elhanyagolható** (néhány tíz KB/frame).
Emellett ugyanezek az értékek **modulációs forrásként** is elérhetők (bass → zoom, high onset → RGB split, stb.).

**Amit első nap leteszek az asztalra (beépített shaderek):**
spektrogram (színskálával, log frekvenciatengellyel) · oscilloscope · spektrum-sávok + hullámforma · körkörös spektrum ·
és egy „reaktív háttér", ami a spektrogramot használja textúraként. Ezek ISF fájlok — bármikor írhatsz mellé sajátot.

### 4.3 Amit az Ableton közben csinál

Nem nyúlunk hozzá: a Live felvesz, a VJ app csak **hallgat** (elemzés + opcionálisan a stream hangja).
Nincs VST/AU host, nincs audio-út a vizuál felé — ez **heteket spórol** és nem is hiányzik.

---

## 5. Generatív motor: mi a „jobb, mint a MilkDrop"

Rövid válasz: **az ISF + Shadertoy-kompatibilis natív motor**, MilkDrop-élménnyel körítve.

| Motor | Miért igen / miért nem |
|---|---|
| **ISF 2.0 (Interactive Shader Format)** ⭐ | A VJ-ipar de-facto shader szabványa (VDMX, Resolume, Magic, CoGe, TouchDesigner). JSON fejléc + GLSL. Több ezer ingyenes shader. Natív, gyors, hot-reload. **Ez lesz az első.** |
| **Shadertoy-kompatibilitás** ⭐ | Több tízezer shader; az `iTime/iResolution/iChannel/iMouse` konvenciót és az audio textúrákat (512×2: FFT + waveform) támogatjuk → a shaderek nagy része konvertálás nélkül megy. |
| **MilkDrop (projectM)** | A retro preset-könyvtár és az auto-morph élmény miatt később bekerül — de OpenGL-es (macOS-en deprecated) híd kell hozzá, ezért a 2. fázis. |
| Butterchurn (JS MilkDrop) | Böngészőben fut, headless WebGL — lassabb, késleltetett. Csak akkor, ha a MilkDrop-kompatibilitás később is fontos. |
| Hydra / live coding | Ötlet: a web UI-ban sketchpad, ami GLSL-t exportál → natívan fut az ISF motorban. Opcionális, szép extra. |
| Saját generátorok | feedback trail, részecske, tunnel, Voronoi, rács — ezek amúgy is ISF shaderek lesznek. |

**A MilkDrop „varázsa" nem a formátum, hanem a viselkedés** — ezt lemásoljuk egy feature-ként:
preset-playlist + **beat-szinkron automatikus presetváltás** (2/4/8/16 ütem), lágy crossfade vagy morph,
per-preset audio-érzékenység („auto-wiring": bass/mid/high → a preset szabad paraméterei), és egy „következő preset" előnézet a UI-ban.
Ezt ISF shaderekkel ugyanúgy megkapod, csak modernebb és gyorsabb anyagból.

---

## 6. Kimenet és streamelés

Egy render → több sink. A `OutputSink` trait mögött mind ugyanazt a kész frame-et kapja:

| Sink | Mire jó | Mikor |
|---|---|---|
| **Ablak / projektor** | fullscreen a 4K TV-n (1080p60), vagy ablakos | most |
| **Előnézet stream** | a web UI-ban látod, mi megy ki — **ez a te „backend streamel egy URL-en" ötleted** | most |
| **YouTube RTMP(S)** | élő adás; H.264-et a **médiamotor** kódol, az ffmpeg csak muxol | 2. fázis |
| **Felvétel (mp4/mov)** | utólagos vágáshoz | 2. fázis (opcionális) |
| **Syphon / NDI** | OBS-be, Resolume-ba, TouchDesignerbe, másik gépre | 2. fázis |

**Előnézet — a legfontosabb részlet:** a UI-ba nem mehet nyers frame. A terv:

- **MJPEG over HTTP** (induláshoz): 720p, 15–30 fps, hardveres JPEG; a `<img>`-ben azonnal megy, minden böngésző támogatja,
  a localhoston a sávszélesség nem szűk. Késleltetés ~50–150 ms. Ez bőven elég a monitorozáshoz.
- **WebRTC** (később, ha kell): 720p60, <100 ms, de nagyobb stack — csak ha tényleg kell.
- A UI-ban állítható: felbontás, fps, ki/be. Ha a gép szűkös, a preview lekapcsol, a show megy tovább.

**YouTube:** `VideoToolbox H.264 hardveres encode → ffmpeg mux → RTMPS`. Az ffmpeg csak csomagol, nem kódol,
tehát a CPU-terhelés minimális. **⚠️ A streamhez hang is kell** (a YouTube nem csak videót vár): a Live master
(BlackHole/Loopback) kerül a stream hangcsatornájára, AAC-ben, szintszabályozással.
Alternatíva, ha ez macerás: **Syphon → OBS → YouTube** (az OBS ingyenes, és mindent tud, amit egy streamhez kell).

---

## 7. Web UI

- **Stack:** Vite + TypeScript (+ React vagy Svelte), Tailwind. A backend **beágyazva szolgálja ki** a statikus fájlokat,
  így egy bináris az egész; böngészőben nyílik: `http://127.0.0.1:7777`.
- **Vezérlés:** WebSocket, kompakt JSON üzenetek (patch-szemantika: csak a változás megy). A pad-nyomás **azonnal** érződik a UI-ban,
  a tényleges váltás a következő ütemre esik (a UI mutatja: „queued → beat 4").
- **Amit a UI tud:** clip grid (thumbnailekkel), layer-mixer (opacity, blend, FX-lánc), médiatár/böngésző,
  modulációs mátrix (drag & drop „MIDI learn" gombbal), autopilot/időzítés beállítások, preset-playlist,
  előnézet + VU-méterek + spektrogram, HUD (GPU/CPU/frame/hőfok), stream-vezérlés, project mentés/betöltés.
- **Tablet/telefon:** ugyanaz a UI elérhető a LAN-on (tokennel védve) — élőben ez óriási kényelmi plusz.
- **Biztonság:** alapból csak 127.0.0.1; LAN mód külön kapcsoló + token; a YouTube stream key a **macOS Keychainben**,
  a UI soha nem kapja vissza.
- **Robusztusság:** az engine UI nélkül is teljes értékű. Kell egy **natív vészgomb** (globális hotkey + MIDI „panic" note +
  blackout/freeze), ami akkor is működik, ha a böngésző épp halott.

---

## 8. Adatmodell — layer, clip, bank, autopilot (változatlan)

- **Layer** (2–8, alapból 4): saját clip-bank, blend, opacity, transform, FX-lánc, autopilot.
- **Clip**: média + in/out + loop + sebesség + opacity + FX. Lehet fájl, kép, generatív preset (ISF), később MilkDrop/Syphon/kamera.
- **Bank**: mappa vagy válogatott lista, sorrendezhető, súlyozható.
- **Quantize**: off / 1/16 … 32 bar. Indítási mód: trigger, gate, toggle, retrigger, loop-sync.
- **Autopilot**: „N ütem után következő" — sorrendes / shuffle / súlyozott véletlen.
- **Cue/Scene**: oszlop, amit egy gombnyomásra több layer vált.

### A te felállásod, konkrétan

| Layer | Tartalom | Blend | Automatizmus | Vezérlés |
|---|---|---|---|---|
| **A · Base** | technó loop-ok | Normal 100% | autopilot 16 ütem | MIDI note/CC bármikor |
| **B · Overlay** | rövid snittek, alpha | Screen 65% | autopilot 8 ütem (shuffle) | CC → opacity |
| **C · Generative** | ISF presetek (spektrogram, tunnel, feedback) | Add 40% | 8–32 ütem / preset, beat-szinkron morph | audio bass → zoom, onset → presetváltás |
| **D · Punch FX** | csak FX (nincs saját videó) | Normal | — | kick(36) → invert 150 ms decay · snare → pixelate 200 ms · CC1 → hue |

---

## 9. Teljesítmény és renderelés

```
fájl → VideoToolbox dekóder → CVPixelBuffer (IOSurface) → CVMetalTextureCache → MTLTexture (wgpu)
                                                                                  │
                              layer FX-lánc (render-to-texture, ping-pong)  ◄──────┘
                                                                                  │
                        kompozitálás → master FX → ┬─► projektor/ablak (1080p60)
                                                   ├─► H.264 HW encode → előnézet / RTMP / felvétel
                                                   └─► Syphon/NDI
```

- **Nincs CPU-readback** a videó útvonalon — ez a legfontosabb szabály.
- HAP: Snappy dekompresszió compute shaderben, egyenesen BC textúrába.
- Textúrapool + perzisztens staging bufferek; a frame-út nem allokál.
- **Frame-drop policy**: a zenei óra az úr; ha a dekóder lemarad, kimarad egy frame, nem akadunk.
- **Adaptív minőség**: költségvetés túllépésnél előnézet-felbontás csökkentés → FX-felbontás csökkentés → layer skip.
- **1080p60 a belső render** (a 4K TV upscale-el), 4K opcionális kapcsoló.
- **Termika**: élő szett előtt 30 perces soak-teszt; HUD-figyelmeztetés hőfokra; automatikus FX-visszafogás.
- **Energia**: App Nap és elalvás tiltása show közben (különben a gép „elalszik" a szünetben).

**Ami biztosan nem gond:** 1080p60-nál 4 layer ~75 MB/frame mozgatás → ~4,5 GB/s a 273 GB/s-ból. Bőven van tartalék.

---

## 10. Plugin-stratégia

1. **ISF shader** (effekt + generátor) — fő felület, hot-reload, paraméter-reflexió. ⭐
2. **Shadertoy shaderek** — konverziós réteggel.
3. **.milk preset** — projectM-en át, 2. fázis.
4. **App↔app videó**: Syphon (macOS) / Spout (Windows) / NDI — be és ki.
5. **Vezérlés**: MIDI, OSC, WebSocket API (a web UI ugyanezt használja — bármilyen saját kliens csatlakozhat).
6. **Natív plugin C ABI** — mélyebb integrációhoz, ritka esetben.
7. **WASM plugin** (`wasmtime`) — homokozott user-logika, 3. fázis.
8. ~~VST3/AU host~~ — **kihúzva** (nem kell, mert nincs audio-effekt igény).

---

## 11. Fázisterv (v2)

| Fázis | Tartalom | Becslés |
|---|---|---|
| **0 · Spike-ok** | (a) VideoToolbox→Metal zero-copy 1080p60 H.264 all-intra / HEVC / ProRes, mérés; (b) **audio→textúra + spektrogram shader**; (c) **ISF motor**: betöltés, param-reflexió, hot-reload, 5–10 valódi isf.video shader + Shadertoy-konverzió; (d) MIDI clock PLL + note/CC + envelope (kick→invert); (e) **web UI váz**: engine + WS + MJPEG előnézet + pad-grid böngészőből; (f) 4-layer 1080p60 kompozit benchmark, **Ableton Live párhuzamosan futva** | **5–8 nap** |
| **1 · MVP — élő szett** | 4 layer, clip-bankok, kvantált indítás, autopilot, MIDI learn, MIDI clock/Link, envelope-vezérelt FX, audio-textúrák + spektrogram, ISF motor, web UI (grid, mixer, mapping, médiatár), projekt mentés/betöltés, előnézet stream, panic/blackout | **2–3 hét** |
| **2 · Streamelés + polish** | YouTube RTMP sink hanggal, felvétel, Syphon ki, **projectM/MilkDrop**, ingest/transcode pipeline, modulációs mátrix UI + LFO-k, scene-ek, thumbnailek, WebRTC előnézet, tablet UI | **2–4 hét** |
| **3 · Opcionális** | több kimenet / edge blend, OSC/DMX/Art-Net, WASM SDK, 10-bit szín, NDI, cue-timeline, undo, .app csomagolás + aláírás, Windows/Linux build | 4–8 hét |

**Élőben használható: ~4–5 hét. Streamelős, kiforrott v1: ~2 hónap.**

---

## 12. Kockázatok (frissítve)

| # | Kockázat | Súly | Kezelés |
|---|---|---|---|
| 1 | A böngésző/UI kiesik a show közben | alacsony | az engine önálló, natív panic gomb + MIDI panic; a UI sosem kritikus úton |
| 2 | Előnézet-stream CPU/GPU költsége | alacsony | hardveres encode, 720p15–30 MJPEG, kikapcsolható, adaptív |
| 3 | Audio-forrás kiválasztása (pre/post FX) | közepes | BlackHole/Loopback/Focusrite loopback; a UI-ban váltható |
| 4 | ISF/GLSL→MSL fordítási hiányosságok | közepes | glslang→SPIR-V→naga→MSL, teszt-shader készlet, hibás shader nem állítja meg a show-t |
| 5 | projectM/OpenGL életképessége (2. fázis) | alacsony | `VideoSource` trait + IOSurface-izoláció; ha kiesik, marad az ISF |
| 6 | Termika hosszú szett közben | közepes | soak-teszt, HUD, automatikus visszafogás |
| 7 | Több MIDI clock forrás / kiesés | alacsony | prioritás + fallback belső clockra |
| 8 | YouTube-késleltetés/beállítások | alacsony | ffmpeg mux + RTMPS; alternatíva Syphon→OBS |

---

## 13. Ami még nyitott

1. **Mehet-e a Phase 0 indítása?** (Első lépés: `rustup` + Homebrew + ffmpeg telepítés, projektváz, majd az első mérés.)
2. **Stream: beépítve vagy OBS-szel?** Nekem mindkettő oké; a beépített szebb, az OBS pár óra alatt kész és mindent tud.
   Javaslat: **Syphon-ki már az 1. fázisban** (így azonnal streamelhetsz OBS-szel), a beépített RTMP a 2. fázisban.
3. **Fő kimenet most:** csak a böngészős előnézet + stream, vagy kell a fullscreen projektor-ablak is a TV-n?
4. **Ableton clock:** Link vagy MIDI clock? (Link a kényelmesebb; MIDI clock akkor kell, ha hardveres dobgép is lesz.)
