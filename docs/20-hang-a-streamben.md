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

## Ami még hátra van

Az app bekötése: a bemeneti mono minták megjegyzése az elemzés mellett,
sztereóvá duplázás, AAC kódolás, és a publikálóhoz küldés időbélyeggel
(a videó és a hang PTS-e ugyanabból az órából). Ez a következő kör.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-media -p dada-stream
cargo run --release -p dada-vj -- --aac-check
```
