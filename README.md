# Dada VJ

Saját, Mac-re optimalizált VJ program. Részletes terv: [TERV.md](TERV.md).

**Állapot: Phase 0 ✅ · Phase 1 ✅ · Phase 2 (streamelés/polish) — nagyrészt kész.**
Huszonegy dokumentum a [docs/](docs/)-ban, 20 teszt-csoport zöld.

## Felépítés

```
apps/dada-vj        a futtatható program (renderer, deckek, web UI, felvétel/stream)
crates/dada-core    zenei óra (PLL), transport, kvantálás, deck/mixer, modulációs mátrix
crates/dada-media   AVFoundation dekód (zero-copy), AVAssetWriter felvétel,
                    VTCompressionSession encoder, thumbnail
crates/dada-audio   FFT-elemzés (spektrum, spektrogram, sávok), bemenet
crates/dada-midi    MIDI üzenetek, portok, clock PLL, mapping
crates/dada-fx      ISF motor (GLSL->WGSL), blend módok, képi bemenet
crates/dada-stream  RTMP transport (FLV/AVC csomagolás, háttérszálas publikáló)
crates/dada-link    Ableton Link (saját tmln-parser, mach-óra fázis)
scripts/            build/run segédszkriptek
```

## Fejlesztői környezet

A Rust toolchain **a projekten belül** él (.toolchain/), így nem kell
rendszerszintű telepítés:

```bash
source scripts/env.sh
cargo build --release
cargo test
```

## Amit tud

| Terület | Állapot |
|---|---|
| Render | 4 réteg 1080p-n, 59.7 fps vsync mellett, CPU 6% |
| Clip-bank | N deck, mappából, thumbnailekkel a UI-ban |
| Indítás | kvantált beat-indítás, autopilot (sequential/shuffle/weighted), kézi next külön sequential/random |
| Blend | Normal, Add, Multiply, Screen + réteg-opacity |
| Chroma key | deckenkénti alap (ArKaOS-stílus) + klippenkénti override (öröklés/egyedi/ki), választható kulcsszín + tolerance/softness/spill, a projektben mentve |
| Szinkron | MIDI clock (24 PPQN, PLL) és **Ableton Link** |
| Moduláció | note (ADSR envelope), CC, 6 audio-sáv, onset, LFO, clock-fázis -> bármely cél |
| Lejátszás | Loop / Once / Ping-Pong bounce, klipenkénti és deck-szintű BPM-hossz igazítás |
| Audio | FFT (spektrum, spektrogram, sávok) GPU-textúraként és modulációs forrásként |
| ISF | generátorok **és effektek** (képi bemenet a lentebbi rétegek), **színkulcs (chroma key)**, effekt célréteg (legfelső kompozit vagy deck-réteg cseréje) |
| Web UI | clip-rács, média könyvtár drag&drop, **rétegek drag&drop (Z-sorrend)**, shader célréteg + alpha mód, ADSR + node patch szerkesztő, **gamepad effekt-körbeváltás szettel és deckenkénti következő-klip akciókkal**, MJPEG előnézet |
| Felvétel | natív H.264 zero-copy, valós idejű MP4, opcionális AAC audióval (választható capture-forrás) |
| Stream | RTMP (YouTube-kompatibilis), **kép és hang (AAC-LC)**, háttérszálon, frame-eldobással |

## Használat

```bash
source scripts/env.sh
BIN=./target/release/dada-vj

# a klasszikus 4 sávos felállás (base + overlay + generatív + audio)
$BIN --deck "media/base:16" --deck "media/overlay:8:screen" --shader shaders/plasma.isf --audio-visual --serve

# Ableton Link szinkronnal (a Live-dal egy gépen pontos a fázis is)
$BIN --link --deck "media/base:16" --serve

# felvétel és stream
$BIN --video media/loop.mp4 --record felvetel.mp4
$BIN --video media/loop.mp4 --rtmp rtmp://a.rtmp.youtube.com/live2/KULCS

# projekt
$BIN --deck "media/base:16" --shader shaders/plasma.isf --save my-set.toml
$BIN --project my-set.toml

# ISF effekt a videóra (képi bemenet)
$BIN --video media/loop.mp4 --shader shaders/mirror.isf

# deckenkénti chroma key: zöld háttér kivétele a Deck kártyán (Chroma key + zöld kulcsszín)
$BIN --deck "media/hatter:16" --deck "media/zold-hatter:16" --serve

# alternatíva: ISF chroma_key effekt a célrétegen (Shader célréteg + Shader Alpha = shader)
$BIN --deck "media/hatter:16" --deck "media/zold-hatter:16" --shader shaders/chroma_key.isf --serve
```

Kapcsolók:

```
--deck MAPPA[:UTEM[:BLEND]]      clip-bank (többször ismételhető)
--video FAJL                    egyetlen klip, autopilot nélkül
--shader FAJL.isf               ISF generátor vagy effekt
--mod FORRAS->CEL[:MENNY[:DECAY]]   modulációs útvonal
--audio-visual [--audio DEV]    audio-elemzés és vizuál
--link                          Ableton Link szinkron
--record FAJL                   felvétel (MP4, H.264)
--rtmp URL                      RTMP stream
--serve [--port N]              webes vezérlő UI + előnézet
--project / --save FAJL.toml    projekt betöltése / mentése
--size WxH · --vsync/--no-vsync · --bench N
```

## Ellenőrzés

Minden funkciónak van önálló, futtatható ellenőrzése:

```bash
$BIN --record-check --record /tmp/v.mp4   # írás + visszaolvasás
$BIN --encode-check                        # hardveres H.264, avcC, keyframe
$BIN --mod-check                           # note/audio/LFO -> célok
$BIN --audio-check                         # FFT -> shader -> pixel
$BIN --shader shaders/plasma.isf --shader-check
$BIN --chroma-check                        # deckenkénti chroma key (GPU)
$BIN --video media/loop.mp4 --video-check
$BIN --video media/loop.mp4 --bounce-check # Ping-Pong reverse lejátszás
$BIN --midi-check                          # clock -> BPM -> envelope -> pixel
$BIN --thumb-check media/loop.mp4
$BIN --probe felvetel.mp4
$BIN --link-status                         # Link peer figyelése 6 s-ig

# élő RTMP a helyi fogadóval:
./target/release/rtmp_sink 19390 400 &
$BIN --video media/loop.mp4 --rtmp rtmp://127.0.0.1:19390/live/teszt
```

Mért eredmények (M4 Pro) — részletek a docs/-ban:

```
4 réteg, 1920x1080          59.0 fps · p50 16.67 ms · CPU 5.9% · RSS 98 MB
4 réteg + felvétel          59.1 fps · p50 16.65 ms · CPU 6.1%
2 réteg + ISF effekt        59.7 fps · p50 16.66 ms
videó-dekód (1080p)         733 fps (zero-copy textúra-csere 10.2 us/frame)
audio FFT (2048)            8.5 us/frame (~1256x valós idő)
H.264 encode (hardveres)    530 fps @ 640x360
AAC-LC encode               46 packet / 3 ms (288x valós idő), ASC [11,90]
felvétel                    151 frame / 4.10 s (valós idejű MP4, 5.9 MB)
RTMP (kép + hang)           190 videó-tag (7 keyframe) + 240 audió-tag, 0 hiba
Ableton Link                128.00 BPM pontosan, fázis-hiteles
klipváltás                  4-12 ms
thumbnail (240x135)         19-200 ms, 3.5-6 KB
```

## Elvek

1. **Zero-copy** a videó úton: VideoToolbox -> IOSurface -> Metal textúra, CPU-másolás nélkül.
2. **A zenei óra az úr**, nem a dekóder: kimaradt frame-et eldobunk, nem akadunk.
3. **Az engine UI nélkül is teljes értékű** — a böngésző sosem kritikus úton.
4. **Hardveres dekód/encode**, a CPU az Ableton Live-é.

## Ismert korlátok

| Korlát | Megjegyzés |
|---|---|
| Lighten/Difference blend | fix-funkciós blendeléssel nem kifejezhető; kompozitáló pass kell |
| projectM/MilkDrop | a C++ könyvtár cmake-et igényel; az ISF lefedi a generatív igényt |
| Syphon kimenet | nincs Syphon.framework a gépen; az RTMP és a felvétel megy |
| YouTube | a helyi RTMP fogadóval igazolt; valódi kulccsal egy próba kell |
| Felvétel ~40 fps 1080p-n | a VideoToolbox valós idejű visszanyomása; a fájl valós idejű |
