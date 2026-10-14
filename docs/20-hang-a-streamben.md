# 20 — Hang a streamben: AAC encoder

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## Miért kell

A terv Phase 2-je **"RTMP sink hanggal"** — a YouTube élő adáshoz hang kell. A
stream eddig néma volt.

## Az encoder (AudioToolbox, raw FFI)

Az objc2 AVAudioConverterje csak PCM -> PCM átalakítást tud szinkron
(convertToBuffer:fromBuffer:error:); a tömörített kimenethez blokk-alapú hívás
kellene. Ezért a **C AudioToolbox** utat választottuk — kiszámíthatóbb:

```
AudioConverterNew(in=Float32 interleaved PCM, out=AAC-LC)
  -> AudioConverterFillComplexBuffer   (callback adja a mintákat)
    -> AudioStreamPacketDescription[]  (packetenként a bájt-tartomány)
      -> a nyers AAC packetek
```

A callback a saját pufferünkből mutat be egy szeletet (nulla másolás a
converter felé), a kimenet pedig 64 KB-os scratch-be kerül, ahonnan a
packet-leírók alapján vágjuk ki a darabokat.

A bitráta az kAudioConverterEncodeBitRate property-vel állítható; az
AudioSpecificConfig-ot (a sequence headerhez) a converter magic cookie-jából
kérjük, és ha az nem elérhető, szabvány szerint építjük.

## Az FLV audio-tag

```
[0xAF]  hangformátum 10 (AAC) + 44 kHz + 16 bit + sztereó
[0x00]  AAC sequence header  -> mögötte az AudioSpecificConfig
[0x01]  nyers AAC frame       -> mögötte az AAC packet
```

(Az igazi mintavételi rátát az AudioSpecificConfig hordozza, ezért az FLV
fejléc mező mindig 44 kHz — ez szabványos.)

## Igazolt működés

```
$ dada-vj --aac-check
AAC-CHECK — 48000 Hz sztereó, 1 s fehér zaj, 128 kbps cél
  49 packet / 4 ms (276x valós idő) · 11878 bájt (242 B/packet, átlag)
  AudioSpecificConfig: [11, 90] (2 bájt) · első packetek: [6, 214, 237]
  OK: valódi AAC-LC, érvényes AudioSpecificConfig-gal.
```

- **49 packet** 1 másodpercre (1024 minta/packet -> ~47 elvárt),
- **11.9 KB** zajra (~95 kbps): a 128 kbps **cél**, nem fix ráta,
- az AudioSpecificConfig pontosan 0x11 0x90 (48 kHz sztereó AAC-LC),
- a kódolás **4 ms / másodperc** (276x valós idő) — elhanyagolható.

Egy tanulság a mérésből: **tiszta szinuszra csak 30 kbps** jött ki, mert az AAC
nagyon jól tömöríti. A bitráta helyes méréséhez fehér zaj kell (nem
tömöríthető) — ezért a check is zajt használ.

## Az RTMP útvonal

A publikáló csatornája mostantól Outgoing::{Video, Audio}, a küldés pedig vagy
publish_video_data, vagy publish_audio_data. A helyi fogadó (rtmp_sink)
számolja külön a videó- és audió-tageket.

FLV audio-teszt: a tagek bájtpontos elrendezése unit-teszttel igazolva
([0xAF, 0x00, ASC] és [0xAF, 0x01, data]).

## A C API csapdája (és a következő lépés)

Az élő bekötés közben kiderült, hogy a C AudioConverter **nem alkalmas
folyamatos streamelésre**:

1. Ha az input callback 0 packetet ad, a converter **lezárja a streamet** —
   utána soha többé nem kér bemenetet.
2. A converter a kért packeteken FELÜL ~2 packetet előre olvas, ezért ha
   pontosan annyit adunk, amennyi a kért packetekhez kell, a callback
   kifogy — és az 1. pont miatt az encoder meghal.

Amit kipróbáltam: perzisztens puffer, tartalék packet, 100-250 ms-os blokkok.
Az eredmény: 4 hívásból **14 packet** (a várt ~47 helyett) — a converter a
bemenet egy részét belső pufferben tartja, a kimenet elmarad. Élő streamhez
ez nem jó (a késleltetés nő).

**Egyetlen nagy hívással viszont tökéletes**: 1 másodperc zaj -> 48 packet,
3 ms (297x valós idő), 11.5 KB. Ezért a `--aac-check` mindkét utat méri és
megmutatja a különbséget.

A helyes API az **ObjC AVAudioConverter blokk-alapú** hívása
(convertToBuffer:error:withInputFromBlock:), mert a blokk tud
AVAudioConverterInputStatus_NoDataNow-t adni — az **nem** EOF. A következő
kör ezt ülteti át. Addig az app streamje **videó-only** (a könyvtári kód, az
FLV-tagek és az RTMP hang-út készen áll, és a helyi fogadó már számolja is az
audió-tageket).

## Az ObjC átirat első kísérlete (23. kör)

Az ObjC AVAudioConverter (blokk, NoDataNow) átirata **lefordult és lefutott**,
de a konstruktorban ObjC kivétel keletkezett:

¤¤¤
fatal runtime error: Rust cannot catch foreign exceptions, aborting
¤¤¤

A kivétel a `new()` négy ObjC hívásának valamelyikéből jön
(initWithCommonFormat, initWithSettings, initFromFormat:toFormat,
AVAudioCompressedBuffer). A pontos hely nem derült ki, mert a
diagnosztikai kiírások beillesztése közben a kód elromlott.

**Tanulság:** ezt lépésenként kell portolni, minden ObjC híváshoz egy-egy
kis `#[test]`-tel, így a kivétel pontos helye azonnal látszik. A C verzió
addig is a helyén marad (egyetlen nagy hívással tökéletesen működik).

## Ami még hátra van

1. Az ObjC AVAudioConverter (blokk, NoDataNow) az AAC encoderhez.
2. Az app bekötése: a mono minták (már benne vannak a snapshotban) ->
   sztereó -> AAC -> publikáló, a videóval azonos órából származó PTS-sel.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-media -p dada-stream
cargo run --release -p dada-vj -- --aac-check
```
