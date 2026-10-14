# 09 — N videó-deck egyszerre (a 4 sávos felállás)

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## Az architektúra

Eddig egy videó-réteg volt; mostantól **deck-enként egy saját dekóder és réteg**:

```
Mixer
 ├─ Deck A  ──► VideoLayer (saját AVAssetReader + Metal texture cache)
 │                ├─ params buffer (opacity)   ← saját
 │                └─ blend mód (Normal)        ← a deck-től
 ├─ Deck B  ──► VideoLayer
 │                └─ blend: Screen, opacity 0.85
 ├─ Deck C  ──► ISF shader (Add)
 └─ Deck D  ──► audio-vizuál (Screen)

minden deck saját: clip-bank, autopilot (N ütem), kvantálás, blend, opacity
minden frame: a deckek a saját zenei rácsukra váltanak, a rétegek alulról
felfelé kompozitálódnak egyetlen render passban.
```

A --video is egységesült: egy deck egyetlen klippel, autopilot nélkül (a klip
belül loopol). Így egyetlen kódút van minden videóhoz.

## Parancssor

```bash
# két videó-deck, külön autopilottal és blend módokkal
dada-vj --deck "media/base:16" --deck "media/overlay:8:screen"

# a teljes 4 sávos felállás (A base + B overlay + C generatív + D audio)
dada-vj --deck "media/base:16" --deck "media/overlay:8:screen" --shader shaders/plasma.isf --audio-visual

# egyetlen klip, autopilot nélkül
dada-vj --video media/loop.mp4
```

A --deck szintaxisa: MAPPA[:UTEM[:BLEND]], ahol a blend normal | add | screen |
multiply. Többször megadva több deck lesz.

## Igazolt működés

Élő futás, két deck (12 és 5 ütemes autopilottal), a web API-val is ellenőrizve:

```
deck A: 2 klip, autopilot 12 ütem, blend Normal, 1920x1080
deck B: 1 klip, autopilot 5 ütem,  blend Screen, 996x702

deck 1 váltás -> small.mov (beat 5.00)
deck 1 váltás -> small.mov (beat 10.00)
deck 0 váltás -> tahoe.mov (beat 12.00)
deck 1 váltás -> small.mov (beat 15.00)
deck 0 váltás -> hero.mp4  (beat 24.00)

web API közben:
{"active_clips":[0,0],"decks":2,"fps":94.4,"layer":"deck", ...}
{"active_clips":[1,0],"decks":2,"fps":105.6,"layer":"deck", ...}
```

Vagyis a 0. deck továbbváltott (0 -> 1), az 1. deck a saját 5 ütemes ütemén.
Mindkettő **a saját zenei rácsán**, egymástól függetlenül.

**Teljesítmény:** két 1080p-s dekódolt réteg egyszerre ~105–120 fps,
CPU ~18–22%, RSS 93 MB.

## Ez már a te felállásod

| Layer | Parancssor | Mi történik |
|---|---|---|
| A base | --deck "media/base:16" | 16 ütem után vált, Normal blend |
| B overlay | --deck "media/overlay:8:screen" | 8 ütem után vált, Screen blend, 85% |
| C generatív | --shader shaders/plasma.isf | ISF generátor, Add blend, 45% |
| D effekt/audio | --audio-visual vagy MIDI note-ok | audio-reaktív, Screen blend |

## Korlátok

| Hiányzik | Terv |
|---|---|
| Klipváltásnál új dekóder indul (~100 ms) | Phase 2: előre-töltés a háttérben |
| Lighten/Difference blend | Phase 2: kompozitáló pass |

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --deck ".testmedia/base:12" --deck ".testmedia/overlay:5:screen"
cargo run --release -p dada-vj -- --deck ".testmedia/base:12" --deck ".testmedia/overlay:5:screen" --serve
```
