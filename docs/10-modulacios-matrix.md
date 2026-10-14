# 10 — Modulációs mátrix

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## A modell

Egyetlen rendszer köti össze a forrásokat a célokkal:

```
FORRÁSOK                          CÉLOK
  note:N  (envelope-pal)            deck.opacity.N     (hozzáadódik a bázishoz)
  cc:N                              deck.next.N        (trigger: következő klip)
  audio.sub|bass|low_mid|           shader.param.N     (0..1 -> min..max)
        mid|high_mid|high           audio.opacity
  audio.rms                         master.opacity     (1 - érték skála)
  audio.onset  (trigger)
  lfo:ÜTEM  (tempó-szinkron)
  clock.phase
```

Minden útvonal egy **envelope-ot** is visz: attack / hold / decay (exp vagy lin),
retrigger kapcsolóval. Az envelope mostantól egy helyen él (dada-core), a MIDI
és a mátrix ugyanazt használja.

## Parancssor

```bash
--mod "FORRAS->CEL[:MENNYISEG[:DECAY_MS]]"     (többször ismételhető)
```

Példák — a te munkafolyamatodra:

```bash
# kick -> a base layer opacity-ja felugrik, majd 180 ms alatt lecseng
--mod "note:36->deck.opacity.0:1.0:180"

# snare -> a második deck a következő klipre lép (kvantáltan)
--mod "note:38->deck.next.1"

# a basszus vezérli a generatív shader zoom-paraméterét
--mod "audio.bass->shader.param.0"

# CC -> shader paraméter
--mod "cc:1->shader.param.1"

# tempó-szinkron LFO a master opacity-re (4 ütemes ciklus, fél erősséggel)
--mod "lfo:4->master.opacity:0.5"

# audio onset -> deck 0 következő klip
--mod "audio.onset->deck.next.0"
```

## Igazolt működés (headless)

A MOD-CHECK végigmegy a három fontos forrás-típuson:

```
MOD-CHECK — 4 útvonal, audio-forrás: true
  frame   kick-op   shader   master   kick-ek
      0   1.00      0.35     0.50     1
     40   0.17      0.57     0.93     2
     60   1.00      0.64     1.00     3
     120   1.00     0.67     0.50     5
     180   1.00     0.40     0.00     7
  csúcs deck-opacity: 1.00 · lecsengés után: 0.00 · trigger-ek: 6
  OK: MIDI note, audio-sáv és LFO is eljut a céljáig.
```

- **kick -> deck-opacity**: minden ütésnél 1.00, majd 0.00 (8 kick),
- **audio-bass -> shader param**: 0.17..0.70, követi a pulzáló basszust,
- **LFO (8 ütem) -> master opacity**: 0.50 -> 1.00 -> 0.50 -> 0.00, teljes ciklus,
- **audio onset -> deck.next**: 6 trigger.

## Élőben is

```
mod: Lfo { period_beats: 4.0 } -> MasterOpacity (mennyiség 0.50)
mod: AudioBand(1) -> DeckOpacity(0) (mennyiség 0.30)
118.8 fps | CPU 19.2% | RSS 94 MB

mod: Note { note: 36 } -> DeckOpacity(0) (mennyiség 1.00, decay 180 ms)
mod: Cc { cc: 1 } -> ShaderParam(0)
deck A: 2 klip, autopilot 8 ütem, blend Normal
deck 0 váltás -> tahoe.mov (beat 8.00)
```

## Egy finomság, amit a teszt talált meg

Az első megvalósítás a trigger frame-jében **azonnal lecsengésnek** indította az
envelope-ot: egy 200 ms-os decay-nél 100 ms-os frame-mel a kick értéke 0.5-re
jött ki 1.0 helyett. VJ-nél ez rossz: a kicknek **azonnal** kell ütnie. A javítás:
a trigger frame-jében a csúcsértéket adjuk ki, és csak a következő frame-től
számol a lecsengés.

## Korlátok

| Hiányzik | Terv |
|---|---|
| simítás/görbe állítása útvonalanként a parancssorból | a projektfájl (Phase 2) viszi |
| több cél egy forrásból | több útvonal ugyanazzal a forrással — már megy |
| master.opacity szemantika: 1 - érték | dokumentálva; LFO-nál természetes |
| web UI-ból szerkeszthető mátrix | Phase 1 következő lépése (clip-grid + mátrix) |

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-core
cargo run --release -p dada-vj -- --mod-check
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --mod "lfo:4->master.opacity:0.5"
cargo run --release -p dada-vj -- --deck ".testmedia/base:8" --mod "audio.onset->deck.next.0"
```
