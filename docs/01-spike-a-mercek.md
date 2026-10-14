# 01 — Spike A mérések: render és kompozitálás

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, 20 GPU mag, 24 GB, macOS 26.6.2, Metal 4*

## Mit bizonyít ez a spike?

1. A **Rust + wgpu 24 + winit 0.30** toolchain működik ezen a gépen (Metal backend).
2. A **frame-pacing** rendben van: vsync mellett 60.0 fps, p50 16.64–16.68 ms.
3. A **kompozitálás** (N alpha-blendelt, teljes képernyős shader-réteg) költsége ismert — ez adja
   a layer-büdzsé valós számát.

## Módszertan

A mérést a `dada-vj --raw-bench` végzi **ablak és vsync nélkül** (headless, offscreen textúrába),
így az eredmény nem függ a monitor frissítésétől és az ablakkezelőtől.

- Nagy batcheket adunk be (600 frame), egyetlen szinkronizálással a végén → a GPU-sor telítődik,
  a mérés átviteli teljesítményt ad, nem latenciát.
- A rétegek költsége a **clear-only** és az **N-layer** batch különbsége; a clear-only referenciát
  a mérés előtt és után is vesszük, és a kisebbet használjuk (konzervatív becslés).
- A rétegek **instanced, alpha-blendelt** draw-k, ahol a shader az `instance_index`-szel
  rétegenként más képet rajzol. Ez azért fontos, mert:
  * az azonos draw-kat egy TBDR driver összevonhatja (hamis „ingyen van" eredmény),
  * az alpha blend valódi olvasás-módosítás-írás, nem elhagyható.
- A mérő shader szándékosan nem triviális (HSV-konverzió, 64 sávos spektrum-számítás, rács, villanás) —
  egy valódi VJ-effekt alsó korlátja, nem egy üres fill.

## Eredmények

| Felbontás | clear-only | N layer | frame-idő | rétegenként | 60 Hz büdzsé |
|---|---|---|---|---|---|
| 1920×1080 | 0.04 ms | 4 | **0.28 ms** | 0.059 ms | 1.7% |
| 1920×1080 | 0.04 ms | 8 | **0.52 ms** | 0.061 ms | 3.1% |
| 3840×2160 | 0.04 ms | 4 | **0.98 ms** | 0.235 ms | 5.9% |

Vagyis a 16.67 ms-os büdzséből **1080p-n ~270**, 4K-n **~70** ilyen teljes képernyős réteg férne bele.

### Vsync-es ablakos mérés (1920×1080 fizikai)

| Konfiguráció | p50 | átlag | fps |
|---|---|---|---|
| 1 layer, vsync | 16.64 ms | 16.95 ms | 59.0 |
| 4 layer, vsync | 16.66 ms | 16.55 ms | 60.4 |

A 4 layer nem lassítja a frame-pacinget — a GPU-költség eltűnik a vsync árnyékában.

## Következtetések a tervhez

1. **A kompozitálás nem szűk keresztmetszet.** A büdzsét a **videó-dekód** és a **drága shaderek**
   fogják kitölteni. A teljesítmény-terv helyes: a 4 layer önmagában elhanyagolható.
2. **A 1080p belső render + 4K kimenet upscale** jó döntés: 4K belső render 4 layer esetén is csak
   ~1 ms (5.9%), de a videó-dekód és a textúrasávszélesség 4K-n négyszereződik — ott érdemes spórolni.
3. **Az instanced draw-k használhatók a valódi kompozitorhoz** is (egy draw hívás, N layer).
4. A **shader-költség** lesz a következő mérendő dolog (Spike C: ISF).

## Technikai megjegyzés: GPU timestamp a wgpu 24-en

Szerettünk volna GPU-oldali időmérést (`TIMESTAMP_QUERY`), de a wgpu 24 Metal backendjén:

- az **encoder-szintű** `write_timestamp` mindkét mintát ugyanarra a pontra teszi (a Metal
  counter sample buffert a backend a blit encoder végén rögzíti) → 0 ms,
- a **pass-en belüli** `timestamp_writes` feature-t a backend Apple GPU-n nem hirdeti
  (`INSIDE_WGPU_PASSES = ON_RENDER_ENCODER | ON_COMPUTE_ENCODER`, és Apple GPU-n nincs
  dispatch-boundary mintavétel).

Ezért a batch-különbség módszerét használjuk, ami a *valódi* frame-költséget méri (CPU-beküldés +
GPU-végrehajtás), és nem függ driver-kvirkktől. A `gpu_timing_available()` API megmaradt, hogy
későbbi wgpu-verzióban vagy más platformon (Vulkan/DX12) kihasználhassuk.

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --raw-bench 600 --size 1920x1080 --layers 4
cargo run --release -p dada-vj -- --bench 5 --layers 4        # ablakos, vsync-es
```
