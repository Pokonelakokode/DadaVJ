# 13 — Felvétel: natív H.264, zero-copy

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## Az útvonal

Nincs ffmpeg, nincs külső encoder: az AVFoundation + VideoToolbox **hardveres**
H.264-ét használjuk, és a kompozit **közvetlenül** a felvevő pufferébe megy:

```
AVAssetWriterInputPixelBufferAdaptor
  -> CVPixelBuffer (IOSurface-hátú, BGRA, a felvevő pool-jából)
    -> CVMetalTextureCache -> MTLTexture        (GPU-oldali kötés, nem másol)
      -> wgpu textúra (RENDER_ATTACHMENT-tel)
        -> a kompozit bele renderelve (ugyanaz a draw-út, mint a képernyőre)
          -> appendPixelBuffer:withPresentationTime:  (VideoToolbox encode)
```

CPU-oldali képmásolás **nincs**: a pixel buffer egy IOSurface, amit a GPU
közvetlenül ír. Ugyanaz az útvonal, mint lejátszásnál, csak fordított irányban.

## Parancssor

```bash
# felvétel a kompozitról (a kimenet méretében)
dada-vj --video media/loop.mp4 --shader shaders/plasma.isf --audio-visual --record felvetel.mp4

# egy fájl vizsgálata
dada-vj --probe felvetel.mp4
-> PROBE felvetel.mp4 — 1920x1080, 256 frame, 6.22 s, 8.6 MB

# az encoder önálló validációja (CPU-töltött minta, írás + visszaolvasás)
dada-vj --record-check --record teszt.mp4
```

## Igazolt működés

**1. Az encoder önmagában** (CPU-töltött minta, 640x360):

```
RECORD-CHECK — 640x360 @ 60 fps, 90 frame -> rec-test.mp4
  encode: 107 ms (840 fps) · fájl: 7.1 KB
  visszaolvasva: 90 frame, 640x360
  háttér-pixel (600,20): B=193 G=45 R=14 A=255  (elvárt: B~200 G~50 R~10)
  a mozgó sáv 10 frame-en haladt át (első: Some(66))
  OK: a felvétel írható és visszaolvasható, a színek és a mozgás rendben.
```

A BGRA-sorrend helyes (a H.264 YUV-kör után is), és a mozgó minta pontosan a
várt frame-eken jelenik meg.

**2. Élő felvétel, 3 réteg, 1080p:**

```
$ dada-vj --video hero.mp4 --shader plasma.isf --audio-visual --record live-1080p.mp4 --size 1920x1080 --bench 6
INFO app] felvétel: live-1080p.mp4 (1920x1080 @ 60 fps, 12 Mbit/s)
frame-ek: 621  átlag: 9.67 ms (103.4 fps)   <- a fő render loop
erőforrás: CPU 8.5% · max RSS 100 MB
INFO app] felvétel lezárva: 255 frame

$ dada-vj --probe live-1080p.mp4
PROBE live-1080p.mp4 — 1920x1080, 256 frame, 6.22 s (PTS 0.000..6.217), 8.6 MB
```

A fájl hossza **6.22 s**, a futás 6 s volt -> a felvétel valós idejű. A fő loop
103 fps-en maradt, a felvétel CPU-igénye elhanyagolható.

## Amit a teszt talált meg (mind valódi hiba volt)

| Hiba | Tünet | Javítás |
|---|---|---|
| ProfileLevel = avc1.640028 | NSException: csak névvel ellátott konstansok | H264_High_AutoLevel |
| is_ready nélkül append | NSException: readyForMoreMediaData = NO | várakozás (max 500 ms) |
| meglévő fájl | startWriting -> Cannot Save | a fájl törlése induláskor |
| textúra-használat | validation: nincs RENDER_ATTACHMENT | wrap_bgra_with_usage |
| PTS a frame-számból | a videó 1.4x gyorsabban játszódott | valós idejű PTS |
| ExpectedSourceFrameRate | NSException: nem támogatott avc1-hez | elhagyva |

## Korlát

A **rögzítési arány ~42 fps** 1080p-n (nem 60), mert a VideoToolbox valós idejű
módban visszanyomja a hívást (az isReadyForMoreMediaData várakozik). A
wall-clock PTS miatt a fájl **valós idejű** és változó frame-rátájú, tehát
lejátszáskor nem gyorsul fel — csak kevesebb frame van benne.

Terv a javításra:
- offline mód (expectsMediaDataInRealTime = false) + háttérszál,
- vagy VTCompressionSession közvetlenül (nagyobb kontroll a queue felett).

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --record-check --record teszt.mp4
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --record live.mp4 --bench 6
cargo run --release -p dada-vj -- --probe live.mp4
```
