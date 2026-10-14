# 08 — Több layer egyszerre: blend módok és opacity

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## A modell

A renderer mostantól **N réteget kompozitál egy render passban**, alulról felfelé:

```
  Layer 0 (alul)   videó / deck        Normal   opacity 1.00
  Layer 1          ISF shader          Add      opacity 0.45
  Layer 2 (felül)  audio-vizuál        Screen   opacity 0.70
  ─────────────────────────────────────────────────────────
  egyetlen render pass, Clear után LoadOp::Load, rétegenként egy draw
```

A blend mód a **pipeline állapota**, ezért minden shaderből 4 variáns készül
(Normal, Add, Multiply, Screen). A rétegek a saját pipeline-jukkal rajzolódnak.

| Mód | Blend (fix-funkciós) |
|---|---|
| Normal | src=SrcAlpha, dst=OneMinusSrcAlpha |
| Add | src=SrcAlpha, dst=One |
| Multiply | src=Dst, dst=Zero |
| Screen | src=OneMinusDst, dst=One |

## Opacity — egységesen a shader alpha-jában

A fix-funkciós blendnél az alpha hordozza az átlátszóságot (src=SrcAlpha), ezért
minden réteg a **saját shaderében** skálázza az alpha-t:

| Réteg | Hogyan kapja az opacity-t |
|---|---|
| videó | külön uniform (binding 2), a fragment alpha = opacity |
| audio | a meglévő uniform resolution.z mezője |
| ISF | a generált WGSL visszatérésébe injektálva (inject_layer_opacity) |

Az ISF-injektálás azért trükkös, mert a shader a saját színét írja. A naga
kimenete viszont kiszámítható alakú (return FragmentOutput(...)), ezt a sort
cseréljük ki úgy, hogy az alpha az _isf_frame.z (layer-opacity) értékkel
skálázódjon — így **minden ISF shader automatikusan támogatja az opacity-t**.

## Tervezési döntés: a LayerRef tulajdonol

A réteglista összeállítása a hívó dolga, ezért a LayerRef a pipeline és a
bind group **klónját** (Arc-handle) tartja, nem referenciát. Így a hívó
szabadon fűzheti össze a listát anélkül, hogy a renderert kölcsönözné, és a
renderer nem kényszerít felhasználói mintát (nincs video/audio paraméter).

## Mért eredmény — három réteg egyszerre

```
videó (1080p, zero-copy) + ISF plasma (Add 0.45) + audio-spektrogram (Screen 0.70)
  frame-ek: 747 · átlag 9.38 ms · 106.6 fps
  CPU 15.8–24.7% · max RSS 97 MB
```

Összehasonlítás: egyetlen réteg 1080p-n 13–18% CPU és 87–94 MB. A három réteg
tehát ~5–8 százalékponttal több CPU-t használ — a kompozitálás és a plusz
shaderek költsége elhanyagolható.

A különálló regressziós tesztek (videó, ISF, audio) továbbra is zöldek, és
mind a három az **új** réteg-úton megy át.

## Korlátok

| Hiányzik | Miért | Terv |
|---|---|---|
| Lighten / Difference | fix-funkciós blendeléssel nem kifejezhető | Phase 2: kompozitáló shader-pass (két textúra) |
| opacity Multiply módban | a Dst faktor nem skálázható | Phase 2, ugyanazzal a pass-szal |
| több videó-réteg egyszerre | egy videó params puffer | Phase 1 következő lépése: N deck |

## Reprodukálás

```bash
source scripts/env.sh
# három réteg egyszerre
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --shader shaders/plasma.isf --audio-visual
# külön regressziós ellenőrzések
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --video-check
cargo run --release -p dada-vj -- --shader shaders/plasma.isf --shader-check
cargo run --release -p dada-vj -- --audio-check
```
