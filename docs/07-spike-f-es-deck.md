# 07 — Spike F és a Phase 1 magja: terhelés + clip-deck

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, 24 GB, macOS 26.6.2*

## Spike F — valós terhelés

A sandbox blokkolja a ps-t, ezért **a program méri magát** (getrusage): minden
másodpercben kiírja a saját CPU%-át (1 mag = 100%) és a maximum RSS-ét.
Ez amúgy is kell a jövőbeli budget-HUD-hoz.

| Mód | felbontás | fps | CPU | max RSS |
|---|---|---|---|---|
| videó, zero-copy dekód | 1920x1080 | 111 | **16–18%** | 94 MB |
| ISF shader (plasma) | 1920x1080 | 110 | **13–15%** | 87 MB |
| audio-vizuál + FFT + spektrogram | 1920x1080 | 115 | **16–25%** | 87 MB |
| web + MJPEG előnézet + shader | 1280x720 | 104 | **19–23%** | 90 MB |
| deck-autopilot (2 s-onként új klip) | 1280x720 | 108 | **38%** | 95 MB |

**Következtetés:** a VJ app önmaga **0.2–0.4 CPU magot** és **~90 MB RAM-ot**
használ 1080p60 mellett. Az Ableton Live-nak bőven marad hely. A legdrágább
eset a klipváltás (38%), mert ilyenkor új dekóder-session és fájlmegnyitás van —
ez később előre-töltéssel optimalizálható.

## Phase 1 mag: a clip-deck (dada-core)

A VJ-munkafolyamat modellje, GPU- és időmentesen (zenei időben dolgozik):

```
Deck (= layer)
 ├─ clips: Vec<Clip>          név, forrás (File/Shader/AudioVisual), in-point,
 │                            sebesség, opacity, blend, loop, súly
 ├─ active / queued           az éppen futó és az ütemezett clip
 ├─ autopilot                 Sequential | Shuffle | Weighted, N ütem, kvantálás
 ├─ fade_beats                crossfade hossza (0 = vágás)
 └─ fading_from / fade_progress

Mixer
 ├─ decks: Vec<Deck>
 └─ tick(now_beats, dt_beats) -> Vec<DeckEvent>
```

A kvantálás szabályai:
- **kézi indítás**: a *következő* rácsra megy (ha pont rácson állsz, a következőre),
- **autopilot**: a *esedékes* rácsvonalra (nem lök tovább),
- minden esemény zenei időben (negyed) van ütemezve, így a MIDI clock jittere
  nem befolyásolja a váltás pontosságát.

## Igazolt működés — valódi médián

```
clip-bank: 3 klip a(z) .testmedia mappából (autopilot: 4 ütem, Beat)
első klip: .testmedia/hero.mp4
clip váltás -> small.mov    (beat 4.00)
clip váltás -> tahoe_4k.mov (beat 8.00)
clip váltás -> hero.mp4     (beat 12.00)
clip váltás -> small.mov    (beat 16.00)
clip váltás -> tahoe_4k.mov (beat 20.00)
```

A váltások **pontosan a rácsvonalra** esnek (4.00, 8.00, 12.00, …), 108 fps
mellett. Ez a te munkafolyamatod: megy a clock, és x ütem után automatikusan
vált a layer.

## Egy hiba, amit a teszt talált meg

Az első megvalósítás az autopilot váltást is a **következő** rácsra tette
(next_grid_strict / ceil). A gyakorlatban ez egy ütemnyi csúszást okozott:
a váltások 5, 10, 15, 20 ütemre estek a 4, 8, 12 helyett — mert a tick
valamivel a rácsvonal *után* érkezik (4.03), és a ceil a következő vonalra lépett.

A javítás: az autopilot a **grid_floor**-t használja (az esedékes rácsvonalat),
míg a kézi indítás marad a next_grid_strict-en. A teszt (auto_advance_every_n_beats)
és az élő futás is ezt ellenőrzi.

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-core
cargo run --release -p dada-vj -- --deck .testmedia --autopilot 4
cargo run --release -p dada-vj -- --video .testmedia/hero.mp4 --size 1920x1080 --bench 7
```
