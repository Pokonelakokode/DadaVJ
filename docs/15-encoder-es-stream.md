# 15 — Encoder és az élő stream összekötése

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## A hardveres encoder (raw FFI)

Az objc2-nek **nincs** VideoToolbox bindingje (csak audio-toolbox és
media-toolbox), az objc2-core-media 0.3-ból pedig hiányzik a
CMSampleBufferGetDataBuffer, ezért a szükséges C API-kat közvetlenül
deklaráltuk (VideoToolbox + CoreMedia + CoreFoundation framework).

```
VTCompressionSessionCreate        (BGRA + Metal-kompatibilis forrás-pufferek)
  -> VTCompressionSessionEncodeFrame
    -> callback: CMSampleBuffer
      -> CMBlockBufferCopyDataBytes         (length-prefixed NALU-k)
      -> CMVideoFormatDescriptionGetH264ParameterSetAtIndex  (SPS/PPS -> avcC)
      -> CMSampleBufferGetSampleAttachmentsArray             (keyframe?)
```

A pixel puffereket az encoder **saját pool-ja** adja
(VTCompressionSessionGetPixelBufferPool), tehát a kompozit ugyanúgy
renderelhető bele, mint a felvételnél — CPU-másolás nélkül.

## Igazolt működés

```
$ dada-vj --encode-check
ENCODE-CHECK — 640x360 @ 30 fps, 30 frame (VTCompressionSession)
  30 frame kódolva 64 ms alatt (468 fps), 2.3 KB, 2 keyframe
  avcC: 26 bájt, kezdete [01, 27, 64, 00, FF, E1, 00, 0B]
  első frame: 256 bájt, pts 0.0 ms, keyframe=true, kezdete [00, 00, 00, 3B, 06, 05, 32, 47]
  OK: valódi hardveres H.264, érvényes avcC-vel és keyframe-mel.
```

- az avcC valódi AVCDecoderConfigurationRecord (version 1, High profil, SPS),
- a frame-ek **4 bájtos hossz-prefixes** NALU-k — pontosan az, amit az FLV vár,
- 30 frame-ből 2 keyframe.

## Az élő lánc

A --rtmp be van kötve: a render loop a kompozitot az encoder pool-jából vett
pixel bufferbe rendereli, kódolja, FLV-be csomagolja és a háttérszálas
publikálóval küldi.

Élő teszt a saját helyi RTMP fogadóval (rml_rtmp szerver-oldal):

```
$ rtmp_sink 19370 200 &
$ dada-vj --video hero.mp4 --shader plasma.isf --audio-visual --rtmp rtmp://127.0.0.1:19370/live/teszt --size 1280x720 --bench 5

app log:
  RTMP kapcsolat: 127.0.0.1:19370, app=live
  RTMP publikálás aktív
  RTMP: avcC sequence header elküldve
  stream lezárva: 1 frame (210 rögzítési ciklus, 210 callback)
  RTMP: 210 frame elküldve, 0 eldobva, 5007.8 KB

fogadó:
  app=live kulcs=teszt · 1 videó-tag
  első tag: [17, 00, 00, 00, 00, 01, 27, 64, 00, FF, E1, 00]...
```

Vagyis a **kapcsolat, a publish és az FLV sequence header bizonyítottan
megérkezik** (a 31 bájtos tag pontosan egy AVC sequence header).

## A talált korlát (őszintén)

A fogadó a **több-chunkos üzeneteknél** elszáll:

```
  handle_input hiba: ChunkDeserializationError(NoPreviousChunkOnStream { csid: 42 })
  ...
  panicked at rml_rtmp chunk_io/deserializer.rs:371:
  attempt to subtract with overflow
```

A deserializer.rs 371. sora (a payload hosszának kivonása) alulcsordul, és a
parser elveszti a szinkront. Ez az **rml_rtmp szerver-oldali** deszerializálójában
van (amit a saját tesztfogadónk használ); a kliens-oldali szerializálást az
rml_rtmp saját unit-tesztjei fedik
(message_split_when_payload_exceeds_max_chunk_size).

Egy 1080p-s H.264 frame ~16–25 KB, ami 4096-os chunk-mérettel 4–7 chunk. A saját
fogadónk ezt nem tudja ellenőrizni, ezért a **teljes lánc YouTube-ig nem
igazolható helyben**.

Amit kipróbáltam:
- chunk_size = 60000: a kapcsolat nem jött létre (a teszt elakadt),
- a nagy frame-es teszt ezért ignore-olt, dokumentált okkal.

## Továbblépés (a következő kör)

1. **Valódi RTMP végpont tesztje** (a YouTube egy nem publikus stream kulccsal) —
   ez dönti el, hogy a hiba csak az rml_rtmp szerverében van-e.
2. Ha a kliens sem jó: **saját RTMP chunk-író** (a handshake + connect/publish
   + chunking ~300 sor; az FLV oldalt már mi írjuk).
3. Vagy az rml_rtmp vendoring + foltozás.

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --encode-check
cargo build --release -p dada-stream --bin rtmp_sink
./target/release/rtmp_sink 19370 50 &
cargo run --release -p dada-vj -- --shader shaders/plasma.isf --rtmp rtmp://127.0.0.1:19370/live/teszt --bench 3
```
