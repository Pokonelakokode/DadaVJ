# 18 — Ableton Link szinkron

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro*

## Miért nem a kész könyvtár

Két jelölt volt:

| crate | miért nem |
|---|---|
| ableton-link-rs | nem opcionális rodio (audiometronóm) -> cpal/alsa-sys ütközés, cmake sincs a C++ SDK-hoz |
| link-bpm-rs | csak BPM-et ad, beat-fázist nem |

A Link protokoll viszont egyszerű ahhoz, hogy magunk olvassuk: a multicast
csomagban van egy tmln bejegyzés:

```
_asdp_v ... tmln [4 bájt hossz] [24 bájt érték]
                                   |- tempo: i64 (mikroszekundum / beat)
                                   |- beat_origin: f64
                                   \- time_origin: i64 (mikroszekundum)
```

Ebből a beat bármely pillanatban:

```
beat(t) = beat_origin + (t - time_origin) / tempo
BPM     = 60_000_000 / tempo
```

## A kulcs-felismerés: az óradomain

A time_origin a hálózat steady clock ideje. macOS-en a libc++ steady_clock a
**mach_absolute_time**-ra épül, tehát ha a Link peer **ugyanazon a gépen** fut
(Ableton Live), akkor a fázis pontosan illeszkedik — nem kell hálózati
óraszinkron.

Ezért van a plausible_at ellenőrzés is: ha az időeredet más domainben lenne, a
számított beat hatalmas lenne, és ilyenkor a program a BPM-et használja, de a
fázist nem.

## Igazolt működés

A link_sender teszt-adó 128 BPM-es szabályos Link csomagokat küldött, a
dada-vj --link-status pedig figyelt:

```
$ ./target/release/link_sender 128 8 &
$ ./target/release/dada-vj --link-status
  csomagok: 18 · BPM 128.00 · beat 10.23 · hiteles: true
  csomagok: 20 · BPM 128.00 · beat 10.98 · hiteles: true
  csomagok: 22 · BPM 128.00 · beat 12.37 · hiteles: true
  csomagok: 26 · BPM 128.00 · beat 13.88 · hiteles: true
```

- a BPM **pontosan** 128.00,
- a beat folyamatosan halad (~0.7 beat / 300 ms = 128 BPM),
- a hiteles: true azt jelenti, hogy az **óradomain egyezik** — tehát Live-dal a
  fázis is pontos lesz.

A crate 7 tesztje zöld, köztük egy **valódi hálózati** teszt (UDP csomag küldése
és vétele a 20808 porton) és az óradomain monotonitás-ellenőrzése.

## Az app bekötése

A zenei pozíció forrása mostantól priorizált:

```
Ableton Link (ha él és hiteles)  >  MIDI clock (ha be van zárva)  >  belső 120 BPM
```

A --link kapcsoló indítja a figyelőt; a deckek autopilotja és a kvantálás
ugyanazt a beatet használja, mint eddig. A web UI állapotában megjelenik a link
és a link_packets mező.

## Korlátok

| Hiányzik | Miért |
|---|---|
| a tempó beállítása Link-en | passzív figyelő (VJ-hez ez a helyes: a Live a master) |
| start/stop követése | a tmln csak az ütemtervet hordozza, a transport nem |
| távoli gép óraszinkronja | azonos gépen pontos; hálózaton a fázis közelítő |

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-link
# két terminálban:
./target/release/link_sender 128 10
./target/release/dada-vj --link-status
# vagy Ableton Live-dal: Link bekapcsolása a Live-ban, majd
cargo run --release -p dada-vj -- --link --deck ".testmedia/base:16"
```
