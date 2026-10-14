# 06 — Spike E mérések: webes vezérlő UI és előnézet

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## Az architektúra

```
  render-szál (winit + wgpu)                web-szál (std::thread)
  ─────────────────────────                 ──────────────────────
  minden frame:                             tiny_http szerver
    ├─ MIDI tick                              ├─ GET /            -> beépített UI (HTML+JS)
    ├─ web-parancsok feldolgozása             ├─ GET /api/state   -> JSON állapot
    ├─ réteg renderelése                      ├─ POST /api/command-> parancs a sorba
    └─ ~10 fps:                               └─ GET /preview.mjpg-> MJPEG stream
         ├─ előnézet 640x360-ba renderelés
         ├─ GPU -> CPU readback              minden kérés KÜLÖN szálon
         ├─ JPEG kódolás (quality 72)        (a hosszú MJPEG stream nem
         └─ Arc<Mutex<Vec<u8>>>              blokkolja a state-pollozást)
```

A render-szál **soha nem blokkol** a hálózaton: csak a megosztott állapotot
frissíti és a parancsokat olvassa. Ha nincs kliens, a költség nulla (a
preview-capture csak akkor fut, ha a szerver be van kapcsolva).

## Az előnézet trükkje

A teljes 1080p-s kép visszaolvasása ~8 MB/frame lenne. Helyette a réteget
**egy 640x360-as textúrába rendereljük újra** (a shaderek felbontás-függetlenek),
majd azt olvassuk vissza: így a readback ~900 KB, a JPEG pedig ~10 KB.
A mérés szerint ez ~10 fps-en elhanyagolható.

## Igazolt működés (élő curl-teszt)

```
GET /api/state
{"beats":0.0,"bpm":120.0,"fps":95.9,"layer":"shader","midi_messages":0,
 "params":[{"max":20.0,"min":0.5,"name":"scale","value":5.0}, ...],
 "shader":"Dada plasma"}

GET / (UI)
<!doctype html><html lang="hu"> ... (a beépített vezérlő felület)

GET /preview.mjpg
--frame
Content-Type: image/jpeg
Content-Length: 10460
<ffd8 ffe0 0010 4a46 4946 ...>   <- valódi JPEG
 SOF0: 0x0168 = 360 magas, 0x0280 = 640 széles

POST /api/command  {"cmd":"set_param","index":0,"value":3.5}
{"ok":true}
-> a következő /api/state már  "scale": 3.5
```

**Konkurencia-teszt:** nyitott MJPEG stream mellett két egymást követő
/api/state kérés is azonnal válaszolt.

## A UI

A böngészőben (http://127.0.0.1:7777) egy sötét VJ-vezérlő jelenik meg:

- **élő előnézet** (MJPEG, 640x360, ~10 fps)
- **FPS / BPM / negyed / MIDI üzenetszám** (300 ms-onként frissül)
- **a betöltött shader paraméterei** automatikusan generált csúszkákkal
  (névre, min/max-ra a JSON fejlécből)
- **Kick / Snare / Hat gombok**, amik szimulált MIDI note-ot küldenek — ugyanazon
  az úton mennek, mint a hardveres MIDI (parser -> mapping -> envelope)

## Amit még nem tud (és a terv)

| Hiányzik | Miért | Terv |
|---|---|---|
| WebSocket | a 300 ms-os HTTP-pollozás a vezérléshez bőven elég | ha kell, később |
| LAN-elérés (tablet) | jelenleg 127.0.0.1-hez kötött | Phase 2: tokenes LAN mód |
| WebRTC előnézet | az MJPEG egyszerűbb és elég | Phase 2, ha a késleltetés számít |
| Több egyidejű preview kliens | kliensenként egy szál | rendben van néhány kliensig |

## Reprodukálás

```bash
source scripts/env.sh
cargo run --release -p dada-vj -- --shader shaders/plasma.isf --serve
# böngésző: http://127.0.0.1:7777/
# vagy parancssorból:
curl -s http://127.0.0.1:7777/api/state
curl -s -X POST -d '{"cmd":"trigger","note":36}' http://127.0.0.1:7777/api/command
```
