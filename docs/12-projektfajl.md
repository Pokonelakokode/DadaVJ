# 12 — Projektfájl (mentés/betöltés)

*Dátum: 2026-10-14*

## A formátum

TOML, amely szándékosan a parancssori szintaxist tükrözi — így kézzel is
szerkeszthető, és ugyanazokat a parsereket használja, mint a CLI:

```toml
version = 1
shader = "shaders/plasma.isf"

[output]
size = "1280x720"
vsync = true
serve = true
port = 7777

[audio]
enabled = true
device = "default"

[[decks]]
folder = ".testmedia/base"
start_clip = 0
autopilot = "sequential"
every_beats = 16.0
quantize = "beat"
blend = "normal"
opacity = 0.33
fade_beats = 0.0

[[decks]]
folder = ".testmedia/overlay"
autopilot = "shuffle"
every_beats = 4.0
blend = "screen"
opacity = 0.85

[[mods]]
route = "note:36->deck.opacity.1:1:180"
```

Az autopilot mód: off | sequential | shuffle | weighted.
A quantize: off | 1/16 | 1/8 | beat | 4 | 8 | 16 | bar.
A blend: normal | add | screen | multiply.

## Használat

```bash
# a felállás mentése a CLI-ből, majd kilépés
dada-vj --deck ".testmedia/base:16" --deck ".testmedia/overlay:8:screen" --shader shaders/plasma.isf --save my-set.toml

# betöltés
dada-vj --project my-set.toml

# mentés a UI-ból: a fejléc Mentés gombja
```

## Az elsőbbség szabálya

A projekt **kitölti**, amit a parancssor nem adott meg. Explicit CLI-kapcsoló
mindig nyer. Ehhez a méret/vsync/port esetén külön jelző kell (mert ezeknek
van alapértékük) — ezt egy hiba után vezettem be: az első verzióban a projekt
serve = false értéke felülírta a parancssori --serve-et.

## Igazolt működés

Mentés a parancssorból:

```
$ dada-vj --deck ".testmedia/base:16" --deck ".testmedia/overlay:8:screen" --shader shaders/plasma.isf --mod "note:36->deck.opacity.1:1.0:180" --save dada-project.toml
projekt mentve: dada-project.toml
[[decks]] folder = ".testmedia/base"    every_beats = 16.0
[[decks]] folder = ".testmedia/overlay" every_beats = 8.0  blend = "screen"
[[mods]]  route = "note:36->deck.opacity.1:1.0:180"
```

Betöltés és a CLI felülírás:

```
$ dada-vj --project dada-project.toml --serve --port 7831
INFO app] projekt betöltve: dada-project.toml (2 deck, 1 mod)
INFO app] deck A: 2 klip, autopilot 16 ütem, blend Normal, 1920x1080
INFO app] deck B: 1 klip, autopilot 8 ütem, blend Screen, 996x702
GET /api/state -> decks: A(op=1.00, normal, ap=16)  B(op=0.85, screen, ap=8)
```

Mentés a UI-ból (élő állapot):

```
POST deck      {"deck":0,"opacity":0.33}
POST autopilot {"deck":1,"mode":"shuffle","every_beats":4}
POST save_project {"path":"dada-saved.toml"}
-> a fájlban: deck A opacity = 0.33, deck B autopilot = "shuffle", every_beats = 4.0
```

Vagyis a **futás közben beállított** értékek is mentődnek, nem csak amit a
parancssorban megadtál.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-vj
./target/release/dada-vj --project examples/demo-project.toml --serve
```
