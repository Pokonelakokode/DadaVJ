# 05 — Spike D mérések: MIDI (clock, note/CC, envelope)

*Dátum: 2026-10-14 · Mérve: MacBook Pro (Mac16,7), Apple M4 Pro, macOS 26.6.2*

## Az architektúra

A MIDI-részt három, egymástól függetlenül tesztelhető egységre bontottam:

```
midir (CoreMIDI) ──► byte-puffer ──► MidiEngine::feed()
                                       │  1. MidiParser: byte -> üzenet (running status, SysEx,
                                       │     real-time üzenetek beékelve)
                                       │  2. ClockPll (dada-core): 24 PPQN -> BPM + fázis
                                       │  3. Transport: beats, Start/Stop/Continue, SPP
                                       │  4. Mapping-ek: note/CC -> Envelope
                                       ▼
                              envelope-érték (0..1) -> bármely paraméter
```

A hardveres réteg (midir) csak **byte-okat gyűjt**; a feldolgozás a feed metódusban
történik, ami így hardver nélkül, determinisztikusan tesztelhető.

## Mért eredmények (end-to-end, szintetikus MIDI byte-okkal)

```
MIDI-CHECK — szintetikus clock @ 128 BPM + kick minden negyeden
  clock tickek: 306 · üzenetek: 333 · bezárt: true
  detektált BPM: 127.99 (várt: 128) · transport: 12.78 negyed 6.00 s alatt
  envelope: csúcs 0.85 · lecsengés után 0.00 · kick-ek: 13
  különböző színminta: 14
  OK: MIDI clock -> BPM -> envelope -> shader paraméter -> pixel.
```

- **A clock PLL 0.01 BPM pontossággal zárt be** ±1.5 ms jitter mellett.
- A transport 12.78 negyedet haladt 6 másodperc alatt 128 BPM-en (elvárt: 12.8).
- Az envelope minden kicknél felfutott (0.85), és 220 ms alatt lecseng (0.00).
- A kimeneti pixelek **14 különböző értéket** vettek fel: a MIDI tényleg eljut a képernyőig.

## Valódi hardver ezen a gépen

```
MIDI-bemenetek: IAC Driver Synthesia Out, Scarlett 18i8 USB
megnyitva 2 MIDI-forrás
```

Ez pontosan a kívánt felállás:

1. **Ableton Live -> IAC Driver -> Dada VJ.** A Live-ból küldhetsz clock-ot és
   note-okat a virtuális IAC buszon; a Dada VJ ugyanazt a jelet kapja, amit a
   dobgép/kontroller küld. Nem kell hardver.
2. **Scarlett 18i8 USB** — a Focusrite interfész MIDI-bemenete közvetlenül fogadja
   a külső eszközöket (dobmodul, kontroller), akár az Abletonnal egyszerre.

Több forrás egyszerre működik: minden port külön azonosítót kap, és a mapping
forrásonként külön köthető (port 0 kickje és port 1 kickje nem ütközik).

## A tipikus VJ-eset, ami így már működik

```
note 36 (kick) ──► EnvelopeSpec { attack: 0 ms, hold: 0 ms, decay: 220 ms,
                                  curve: Exponential, retrigger: true }
                ──► shader.param.0 (0..1)
```

Ez a "ráütök a kickkel, és azonnal átvált, majd lecseng" viselkedés. Az envelope
bármely paraméterre köthető; a mapping célja jelenleg név szerinti
(pl. shader.param.0), a modulációs mátrix (Phase 1) ezt általánosítja.

## Tesztek (18 db, mind zöld)

| Csoport | Mit bizonyít |
|---|---|
| Parser (8) | note on/off, velocity=0 = note off, running status, clock beékelve note-ok közé, CC/SPP, transport üzenetek, SysEx kihagyása, szétdarabolt üzenetek |
| Envelope (6) | azonnali attack -> lecsengés, exponenciális vs lineáris, attack ramp, note-specifikus trigger, CC-küszöb, retrigger=false |
| Engine (4) | BPM bezárás, Start/Stop/Continue + SPP, note -> envelope, több forrás függetlensége |

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-midi
cargo run --release -p dada-vj -- --midi-check
cargo run --release -p dada-vj -- --midi                              # portok listázása + megnyitás
cargo run --release -p dada-vj -- --shader shaders/plasma.isf --midi-visual
```

## Következő lépés

Az Ableton **Link** szinkron még nincs bekötve (a crate GPL-3.0, és a MIDI clock
egyelőre mindent tud, amit kell). Ha később kell, a link-rs crate egy vékony
wrapperrel illeszthető a ClockPll helyére.
