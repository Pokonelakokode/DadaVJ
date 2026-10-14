# 17 — ISF képi bemenet (effekt shaderek)

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## A naga korlát és a megkerülése

A naga GLSL-frontendje **nem támogatja az önálló samplert** — sem sima uniform
sampler2D, sem layout(binding=1) uniform sampler2D formában:

```
NotImplemented("variable qualifier")   (a sampler2D típusnál)
```

A megoldás: a samplert **stub-függvénnyé** alakítjuk a GLSL-ben, lefordítjuk,
majd a generált WGSL-ben a stub törzsét valódi textúra-mintavételre cseréljük.

```
GLSL (fordítás előtt)            WGSL (fordítás után)
uniform sampler2D inputImage;    @group(0) @binding(1) var isf_tex_0: texture_2d<f32>;
texture(inputImage, uv)          textureSample(isf_tex_0, isf_samp_0, uv)
  -> isf_image_0(uv)               fn isf_image_0(uv: vec2<f32>) -> vec4<f32> {
vec4 isf_image_0(vec2 uv) {              return textureSample(...);
  return vec4(0.0, 0.0, 0.0, 1.0); }
}
```

A névfeloldás robusztus: a naga néha átnevezi a függvényt (pl. isf_image_0_),
ezért a cserénél a **tényleges** nevet olvassuk ki a WGSL-ből, és a hívási
helyek változatlanok maradnak.

## A bindingek

| binding | mi |
|---|---|
| 0 | ISF paraméterek (std140 uniform blokk) |
| 1 / 2 | 1. kép textúrája / samplere |
| 3 / 4 | 2. kép textúrája / samplere |

A bind group layoutot az Effect a shader képszáma alapján építi, és ha nincs
bekötött bemenet, egy 1x1-es tartalék textúrát használ (így a shader mindig
renderelhető).

## Az app: a lentebbi rétegek a bemenet

A renderer kapott egy offscreen **input target**-et. Az ISF réteg előtt a már
összeállt rétegek (videó, audio) bele renderelődnek, és az a textúra lesz a
shader képi bemenete:

```
videó réteg(ek) --> offscreen input target --+
                                             |
                    ISF effekt (inputImage) --+--> Add blend a fő kimenetre
```

Ez pontosan a felhasználó által kért **"D: effekt layer"**: a lentebbi kép
effektje, a saját blend módjával.

## Igazolt működés — vizuálisan

Új minta-shader: shaders/mirror.isf (tükrözés + lebegés, inputImage-dzsel).

Élő futás, --video hero.mp4 --shader shaders/mirror.isf; az MJPEG előnézetből
kimentett képkockán **jól látszik a tükrözés a jobb oldalon** — a shader tehát
a videó-réteget mintavételezi.

```
INFO app] ISF shader: shaders/mirror.isf — Tükrözés + lebegés (2 paraméter,
         kihagyva: inputImage (image))
frame-ek: 239  átlag: 16.79 ms (59.6 fps)
```

## Amit ez megnyit

- **ISF effekt shaderek** (blur, mirror, chromatic aberration, feedback),
- a **Lighten / Difference** blend módok alapja (a lentebbi kép textúraként
  elérhető a shaderben),
- a **projectM/MilkDrop** későbbi bekötésének ugyanaz az input-útvonala lesz.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-fx          # 9 teszt, ebből 2 a képi bemenetre
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --shader shaders/mirror.isf --serve
# böngésző: http://127.0.0.1:7777/
```
