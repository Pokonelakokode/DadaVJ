# 11 — Web UI: clip-grid és vezérlés

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## Mit tud a vezérlőfelület

A http://127.0.0.1:7777/ oldalon (tabletről is elérhető a LAN-on, ha majd
engedjük) mostantól:

- **élő előnézet** (MJPEG),
- **FPS / BPM / negyed / MIDI** számlálók,
- **Kick / Snare / Hat** gombok (szimulált MIDI note-ok, ugyanazon az úton),
- **deck-enként egy clip-rács**: a kockákra kattintva a klip a következő
  zenei rácsra indul (a futó klip narancssárga, a várakozó kék),
- **autopilot vezérlés** deck-enként (off / sequential / shuffle / weighted + ütem),
- **opacity csúszka és blend választó** deck-enként (normal / add / screen / multiply),
- **modulációs útvonalak listája** (forrás, cél, pillanatnyi érték) törléssel,
  és egy egyszerű hozzáadó sor (a CLI-vel azonos szintaxissal),
- **shader paraméter-csúszkák** automatikusan.

## Az API

```
GET  /                 a UI
GET  /api/state        teljes állapot (deckek, klipek, modok, paraméterek)
POST /api/command      {"cmd":"launch","deck":0,"clip":2}
                       {"cmd":"autopilot","deck":1,"mode":"off","every_beats":16}
                       {"cmd":"deck","deck":0,"opacity":0.4,"blend":"add"}
                       {"cmd":"trigger","note":36}
                       {"cmd":"set_param","index":0,"value":3.5}
                       {"cmd":"mod_add","source":"note:36","target":"deck.opacity.1","amount":0.8}
                       {"cmd":"mod_remove","index":0}
GET  /preview.mjpg     MJPEG stream
```

## Igazolt működés (élő API-teszt)

```
1) induló állapot: decks=2 layer=deck mods=0
   [A active=0 queued=null ap=sequential/16 op=1 blend=normal clips=2]
   [B active=0 queued=null ap=sequential/8  op=0.85 blend=screen clips=1]

2) launch deck 0 klip 1        -> {"ok":true}
   app log: UI indítás: deck 0 klip 1 -> 3.00 negyed
            deck 0 váltás -> tahoe.mov (beat 3.00)

3) autopilot off (deck 1)      -> {"ok":true}
   app log: UI autopilot: deck 1 = off (16 ütem)

4) deck opacity 0.4 + add      -> {"ok":true}
   5 s múlva: [A ... op=0.4 blend=add]

5) mod_add note:36 -> deck.opacity.1:0.8   -> {"ok":true}
6) mod_remove 0                            -> {"ok":true}
   app log: UI mod hozzáadva / UI mod törölve
```

Vagyis a klip-indítás **kvantáltan** megy (a 3.00 negyedre ütemezve, nem azonnal),
és az UI-ból minden olyan állítás elérhető, ami eddig csak parancssorból.

## Ami még hiányzik

| Hiányzik | Terv |
|---|---|
| klip-thumbnailek | Phase 2: a médiatár előnézeti képe (a dekóder első frame-je) |
| drag & drop a bankok között | Phase 2 |
| projektfájl mentés/betöltés az UI-ból | Phase 2 |
| LAN-elérés tokennel | Phase 2 |

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --deck ".testmedia/base:16" --deck ".testmedia/overlay:8:screen" --serve
# böngésző: http://127.0.0.1:7777/
```
