# 02 — Spike A2 mérések: videó-dekód és zero-copy GPU-út

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, 20 GPU mag, 24 GB, macOS 26.6.2*

## A kérdés

A projekt **kritikus kockázata** ez volt: hogyan kerül a videó frame a GPU-ra úgy, hogy
közben a CPU-t ne terhelje, és a zenei időhöz képest ne legyen késés?

**A válasz: a lánc működik, zero-copy, és elhanyagolható költségű.**

## A bizonyított út

```
.mp4/.mov  (H.264 / HEVC, hardveres)
    │  AVAssetReader + AVAssetReaderTrackOutput
    │  (pixel-formátum + Metal-kompatibilitás kérve)
    ▼
CVPixelBuffer  —  IOSurface-hátú, VideoToolbox dekódolta
    │  CVMetalTextureCacheCreateTextureFromImage
    ▼
MTLTexture     —  GPU-oldali kötés, nulla másolás
    │  wgpu-hal: metal::Device::texture_from_raw
    │  wgpu:     Device::create_texture_from_hal
    ▼
wgpu::Texture  —  ugyanaz a memória, amit a shader mintavételez
```

A CPU **nem látja a képpontokat**, és nincs GPU-oldali blit sem.

## Eredmények

### Dekód (VideoToolbox, BGRA kimenet)

| Klip | Felbontás | Névleges | Dekód | Valós idejű tényező |
|---|---|---|---|---|
| small.mov | 996×702 | 60 fps | **2554 fps** | 42.5× |
| hero.mp4 | 1920×1080 | 25 fps | **733 fps** | 29.3× |
| tahoe_4k.mov | 3840×2160 | **240 fps** | **283 fps** | 1.18× |

A 4K-s teszt szándékosan szélsőséges (4K **240 fps** HEVC = 8.3 Gpixel/s). 4K60-ra
átszámítva ez **4.7× valós idejű** tartalékot jelent.

### Dekód + zero-copy wrap (a teljes videó-út)

| Klip | Felbontás | frame-ek | idő | sebesség | wrap költség |
|---|---|---|---|---|---|
| hero.mp4 | 1920×1080 | 600 | 0.77 s | **784 fps** | **10.2 µs/frame** |
| tahoe_4k.mov | 3840×2160 | 300 | 1.16 s | **258 fps** | **18.6 µs/frame** |

A wrap (CVMetalTexture + wgpu textúra létrehozás) **10–19 µs**, azaz a 16.67 ms-os
60 Hz-es büdzsé **0.06–0.11%**-a.

### Következtetés a layer-büdzsére

1080p60-nál a dekód ~733 frame/s → **~12 darab 1080p60 klip** férne el egyszerre CPU-oldalon.
A 4 layeres tervhez tehát **bőséges** a tartalék, még a kompozitálással együtt is.

## Bizonyíték: valódi képpontok a GPU-textúrában

A `zerocopy_bench` visszaolvassa a 10. frame bal felső sarkát a létrehozott wgpu-textúrából:

- **hero.mp4** (1080p): `B=26 G=49 R=67 A=255`, `B=30 G=54 R=72 A=255` … → sötétkék kép, helyes BGRA sorrend
- **tahoe_4k.mov** (4K): `B=191 G=129 R=50 A=255` → narancsos háttérkép, helyes értékek
- alpha = 255 (a BGRA konverzió kitölti)

Ez azt igazolja, hogy nem üres/érvénytelen textúra keletkezik, hanem a dekódolt kép.

## Pixel-formátum: BGRA vs natív YUV

A VideoToolbox natív kimenete biplanáris YUV (`420v`). A 4K klippel mérve:

| Formátum | Dekód |
|---|---|
| BGRA (egy textúra) | 283 fps |
| 420v (natív, két sík) | 289 fps |

A különbség **~2%** — a hardveres konverzió gyakorlatilag ingyen van. Ezért a BGRA-val
indulunk (egyszerűbb, egy textúra), és a YUV-utat később, akkor vezetjük be, ha a
textúra-sávszélesség (feleannyi bájt/pixel) számítani kezd.

## Tanulságok / buktatók (a következő fejlesztőnek)

1. **`AVAssetReaderTrackOutput` `nil` outputSettings-szel tömörített sample-eket ad**, nem képet —
   `CVPixelBuffer`-hez pixel-formátumot kell kérni. Ezt a hibát a bench azonnal megfogta.
2. A **Metal-kompatibilitást kérni kell** (`kCVPixelBufferMetalCompatibilityKey`), különben
   nincs IOSurface-háttér → nincs zero-copy.
3. **objc2 0.6 nem derefel a szuperosztályra**: `copyNextSampleBuffer` az `AVAssetReaderOutput`-on
   van, ezért `Retained::into_super()` kell.
4. A `copyNextSampleBuffer` az **`objc2-core-media` feature** mögött van az objc2-av-foundation-ben.
5. A `CVMetalTextureGetTexture` objc2-metal típust ad, a wgpu-hal viszont metal-rs típust vár —
   a kettő között nyers pointer-cast kell. A +1 referenciát át kell adni (nem duplán felszabadítani).
6. A **GPU-textúra élettartama**: a becsomagolt textúra a CVPixelBufferhez kötődik, ezért a
   dekódolt frame-et (`CVMetalTexture` + pixel buffer) **addig életben kell tartani, amíg a GPU
   be nem fejezi** a frame renderelését. A bench 6 frame-es ringet tart.

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-media --bin decode_bench   -- .testmedia/hero.mp4 3 bgra
cargo run --release -p dada-media --bin zerocopy_bench -- .testmedia/hero.mp4 600
```

(A teszt-klipek a `.testmedia/` mappában vannak, ami nincs verziókövetve.)
