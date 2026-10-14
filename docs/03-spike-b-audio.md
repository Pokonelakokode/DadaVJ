# 03 — Spike B mérések: audio-elemzés és audio-textúrák

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, 24 GB, macOS 26.6.2*

## A kérdés

A hangból legyen vizuál (spektrogram, hullámforma, spektrum), és közben a gép
ne terhelődjön túl — hiszen ugyanezen a gépen fut az Ableton Live is.

**A válasz: az elemzés ~1250×-es valós idejű tartalékkal fut, a GPU-oldali
költség pedig elhanyagolható.**

## A lánc

```
cpal bemenet (48 kHz)  →  lock-free jellegű átadás  →  Analyzer
                                                        │  2048 pontos FFT, 512-es hop
                                                        │  Hann-ablak
                                                        ├─► spektrum (log, dB→0..1)
                                                        ├─► 6 sáv + envelope follower
                                                        ├─► spektrális fluxus → onset
                                                        └─► spektrogram-gyűrű
                                                              │
                          ┌───────────────────────────────────┘
                          ▼
      GPU-textúrák (R32Float, textúraLoad-dal olvasva):
        * waveform   1024 x 1     (legutóbbi 1024 minta, -1..1)
        * spectrum    512 x 1     (log frekvencia-tengely, 0..1)
        * spectrogram 256 x 512   (frekvencia x idő, gördülő)
                          │
                          ▼
              fs_audio shader: spektrogram + hullámforma + spektrum-sávok
```

## Mért eredmények

| Mérés | Eredmény |
|---|---|
| **Elemzés teljesítmény** | 10 s hang → **8 ms** számítás = **1256× valós idő** (937 FFT) |
| Egy FFT költsége | ~8.5 µs (2048 pont, M4 Pro) |
| Frame-enkénti feldolgozás | 33 µs (ebben benne van a **szintetikus** jel generálása is) |
| Valós bemenet | MacBook Pro Microphone @ 48000 Hz, 118 fps render mellett |
| Sávok | mind a 6 sáv 0.9+ értéket ér el a tesztsignalra |
| Onset-detektálás | 30 ütés 6 másodperc alatt (120 BPM szintetikus dobgép) |

A 60 Hz-es büdzséhez képest az elemzés **0.1% alatti** — a hangfeldolgozás
gyakorlatilag ingyen van.

## Validáció

A `--audio-check` mód headless renderel és pixel-mintákat olvas vissza:

```
AUDIO-CHECK — szintetikus forrás @ 48000 Hz
  FFT futások: 560 · onsetek: 30
  max sávenergiák (sub/bass/low_mid/mid/high_mid/high): 1.00 1.00 0.99 0.98 0.97 0.97
  különböző színminta: 3
  ELEMZÉS TELJESÍTMÉNY: 10 s hang → 0.008 s számítás (1256.2x valós idő, 937 FFT)
  OK: a hang az FFT-n és a shaderen keresztül megjelenik.
```

Az unit-tesztek (6 db) a jel-alapú helyességet ellenőrzik:
szinusz-csúcs a várt binben, csend → nulla, 50 Hz → sub/bass sáv, spektrogram-wrap.

## Két tervezési döntés, ami számít

1. **A szintetikus forrás nem luxus.** A macOS mikrofonengedély (TCC) csak
   aláírt app-bundle-lel megbízható; egy CLI-ből indítva csendet is kaphatunk.
   Ezért van szintetikus forrás, ami **determinisztikusan** és **valós időben**
   (a render fps-étől függetlenül) generál jelet: így a teljes lánc hardver és
   engedély nélkül tesztelhető, és a mérések reprodukálhatók.
2. **A spektrogram soronként megy fel a GPU-ra.** Az első verzió minden
   frame-ben a teljes 512x256-os tömböt (512 KB) másolta — ez ~25 µs/frame volt
   feleslegesen. A textúra 256 (frekvencia) x 512 (idő) elrendezésével egy új
   elemzési szelet egy **sor** (1 KB), ami 256-bájtosan igazított, így közvetlenül
   írható. Ez ~1 KB/hop a korábbi 512 KB/frame helyett.

## ISF-kompatibilitás (a következő fázis)

Az ISF szabvány két beépített audio inputot definiál (`audio` és `audioFFT`,
512x2). Ezeket pontosan ugyanígy fel tudjuk tölteni a saját waveform/spektrum
adatainkkal, így a meglévő audio-reaktív ISF shaderek **módosítás nélkül**
működni fognak. Ugyanez igaz a Shadertoy-konvencióra (iChannel-en érkező
512x2-es audio textúra: FFT + waveform).

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --audio-check
cargo run --release -p dada-vj -- --audio-visual            # szintetikus forrás
cargo run --release -p dada-vj -- --audio-visual --audio default   # valódi bemenet
cargo test -p dada-audio
```
