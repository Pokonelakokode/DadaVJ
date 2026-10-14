# 04 — Spike C mérések: ISF shader motor

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## A döntési kísérlet

Az ISF (Interactive Shader Format) egy JSON fejléc + GLSL fragment shader — ez a
VJ-ipar de-facto shader-plugin formátuma (VDMX, Resolume, Magic, CoGe). A kérdés
az volt, hogyan jutunk el innen a Metal/WGSL-ig.

Megmértem a **naga** (a wgpu shader-fordítója) GLSL-frontendjét három szinten:

| GLSL változat | Eredmény |
|---|---|
| Szigorú Vulkan-stílus (explicit layout, uniform block) | OK, lefordul WGSL-re |
| Uniform block + önálló sampler2D | HIBA: NotImplemented("variable qualifier") |
| Nyers ISF (gl_FragColor, isf_FragNormCoord) | HIBA: UnknownVariable |

**A naga nem támogatja az önálló opaque uniformot** (sampler). Ez az ISF
képi bemenetű effektjeit kizárja. A glslang/shaderc alternatíva C++-os
build-láncot igényel (cmake), ami ezen a gépen nincs.

## A választott architektúra

**Saját WGSL-motor ISF-stílusú JSON fejléccel + GLSL->WGSL fordító a
generátor-részhalmazra.**

```
  .isf fájl
     │  1. JSON fejléc kiolvasása (/*{ ... }*/)
     │  2. paraméterek + std140 elrendezés számítása
     │  3. shader-törzs előfeldolgozása:
     │       gl_FragColor -> isf_FragColor
     │       RENDERSIZE/TIME/TIMEDELTA/FRAMEINDEX/PASSINDEX -> uniform mezők
     │       gl_FragCoord / isf_FragCoord -> normált koordináta * felbontás
     │  4. szigorú GLSL preambulum (location/binding, uniform block)
     ▼
  naga GLSL-frontend  ->  naga IR  ->  WGSL
     ▼
  wgpu render pipeline (a mi teljes képernyős vertex shaderünkkel)
```

Ez a **generátorok túlnyomó részét** lefedi: a legtöbb ISF generátor tiszta
matematika (sin, mix, smoothstep, fract, atan, length), képi bemenet nélkül.

## Igazolt működés

Három minta-shader (a shaders/ mappában), headless validációval
(--shader-check: offscreen render + pixel-visszaolvasás):

| Shader | Paraméterek | Uniform puffer | Kimenet |
|---|---|---|---|
| gradient.isf | speed, bands, tintA, tintB | 80 B | 5 különböző színminta, változik |
| bars.isf | count, spin, center, hue | 64 B | 5 különböző színminta, változik |
| plasma.isf | scale, speed, warp | 48 B | 6 különböző színminta, változik |

A std140/WGSL elrendezés helyessége a mérésekből látszik (float @32, float @36,
vec2 @40, vec4 @48) — és a shaderek **helyes képet** adnak, nem feketét.

### Hot-reload

Élő teszt: a shader-fájl módosítása futás közben, 2 másodpercnél:

```
INFO dada_vj::app] ISF shader: .testmedia/reload.isf — Dada gradiens (4 paraméter)
INFO dada_vj::app] shader újratöltve: .testmedia/reload.isf
(BENCH: 682 frame, átlag 8.81 ms, 113.5 fps)
```

A hot-reload a paraméterértékeket név szerint megőrzi, hibás shader esetén pedig
a régi pipeline marad (a show nem áll meg).

## Korlátok (őszintén)

| Nem támogatott még | Miért | Terv |
|---|---|---|
| Képi bemenet (inputImage, sampler2D) | a naga nem ismeri el az önálló samplert | Phase 2: saját sampler-plumbing + kézi fordítás, vagy glslang |
| Multipass / perzisztens bufferek | nincs hozzá pipeline | Phase 2 (visszacsatolás/trail effektekhez kell) |
| bool / long paraméter | GLSL-típus eltérés | könnyen pótolható (float + név szerinti csere) |
| Shadertoy shaderek | ugyanaz a sampler-korlát | konverter a Phase 2-ben |

A motor viszont **saját WGSL shadereket is fogad** (a JSON fejléc opcionális),
így a generatív rész azonnal bővíthető, és a korlátok nem blokkolják a munkát.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-fx
cargo run --release -p dada-vj -- --shader shaders/plasma.isf --shader-check
cargo run --release -p dada-vj -- --shader shaders/plasma.isf            # ablakban, hot-reload-dal
```
