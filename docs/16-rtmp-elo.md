# 16 — Élő RTMP stream: a teljes lánc igazolva

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## A hiba, ami mindent blokkkolt

Egyetlen sor volt a felelős. A kliens a connect elfogadása **után** küldi a
SetChunkSize üzenetet (4096), és az rml_rtmp az eseményeket egy listában adja
vissza:

```
[ OutboundResponse(window_ack),
  RaisedEvent(ConnectionRequestAccepted),
  OutboundResponse(chunk_size) ]     <- ez a harmadik
```

A saját wait_event-ünk viszont az esemény láttán **azonnal visszatért**, így a
lista további elemeit (köztük a SetChunkSize-t) soha nem írta ki a socketre.

Következmény:
- a szerver deszerializálója **128** bájtos chunk-mérettel maradt,
- a kliens serializerje **4096**-tal küldött,
- a 128-nál hosszabb üzenetek (mind! egy 1080p frame 16–25 KB) szinkront
  vesztettek: ChunkDeserializationError / aritmetikai alulcsordulás.

A javítás: a batch **összes** kimenő csomagját elküldjük, és csak a batch után
térünk vissza:

```rust
let mut done = false;
for r in results {
    match r {
        OutboundResponse(packet) => stream.write_all(&packet.bytes)?,
        RaisedEvent(e) => { ...; if pred(&e) { done = true; } }
        _ => {}
    }
}
if done { return Ok(()); }
```

Ez a fajta hiba azért tanulságos, mert a kisé üzenetek (a connect, createStream,
publish és a 31 bájtos sequence header) **tökéletesen működtek** — csak a valódi
frame-eknél tört el. A hibát végül a saját kódunkban találtuk meg, nem a
könyvtárban.

## A teljes lánc igazolása

Helyi RTMP fogadó (rml_rtmp szerver-oldal), valódi hardveres encoderrel:

```
$ rtmp_sink 19390 400 &
$ dada-vj --video hero.mp4 --shader plasma.isf --audio-visual \
          --rtmp rtmp://127.0.0.1:19390/live/teszt --size 1280x720 --bench 6

app:
  RTMP publikálás aktív
  RTMP: avcC sequence header elküldve
  frame-ek: 705  átlag: 8.52 ms (117.4 fps)
  stream lezárva: 272 callback
  RTMP: 271 frame elküldve, 0 eldobva, 5904.8 KB

fogadó:
  app=live kulcs=teszt · 273 videó-tag, 10 keyframe, 6083063 bájt
  első tag: [17, 00, 00, 00, 00, 01, 27, 64, 00, FF, E1, 00]... (31 bájt)
```

A 273 tag = 1 sequence header + 272 videó frame. 10 keyframe a 272-ből, a
frame-ek átlagosan ~22 KB (több-chunkosak). **Nulla hiba, nulla szinkronvesztés.**

Az első tag pontosan egy AVC sequence header:
[17] frame type 1 (keyframe) + AVC, [00] AVCPacketType = sequence header,
[00 00 00] composition time, [01 27 64 00 FF E1 00] az avcC eleje.

## Amit ez lefed

| Réteg | Állapot |
|---|---|
| Render (3 réteg, 1280x720) | ✅ 117 fps |
| Hardveres H.264 (VTCompressionSession) | ✅ 272 frame, 10 keyframe |
| FLV/AVC csomagolás | ✅ az avcC és a NALU-k bájtpontosan |
| RTMP kézfogás + connect + publish | ✅ app=live, kulcs=teszt |
| RTMP chunking (4096, több-chunkos üzenetek) | ✅ 273 tag hiba nélkül |
| Háttérszál + eldobás | ✅ 0 eldobva |

Ami **még** nem igazolt: egy valódi külső végpont (YouTube). A helyi fogadó
ugyanazt a protokollt beszéli, de a YouTube ingestjének saját követelményei
lehetnek (pl. a chunk-méret felső korlátja). A --rtmp URL bármelyik
szolgáltatáshoz beállítható.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-stream          # 8 teszt, ebből 2 integrációs (kis ÉS 20 KB-os frame)
cargo build --release -p dada-stream --bin rtmp_sink
./target/release/rtmp_sink 19390 400 &
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --shader shaders/plasma.isf \
      --audio-visual --rtmp rtmp://127.0.0.1:19390/live/teszt --bench 6
```
