# Dada VJ

Saját, Mac-re optimalizált VJ program. A részletes terv: [TERV.md](TERV.md).

**Állapot: Phase 0 (spike-ok) — fejlesztés alatt.**

## Felépítés

```
apps/dada-vj        a futtatható program
crates/dada-core    zenei idő, transport, MIDI clock PLL, projekt-modell
crates/...          további modulok a spike-ok során
scripts/            build/run segédszkriptek
toolchain/          (nem verziókövetett) a projektbe telepített Rust toolchain
```

## Fejlesztői környezet

A Rust toolchain **a projekten belül** él (`.toolchain/`), hogy ne kelljen
rendszerszintű telepítés. Minden shell-ben előbb:

```bash
source scripts/env.sh
```

Utána:

```bash
cargo build --release
cargo test
cargo run -p dada-vj -- --help
```

## Teljesítmény-mérés

A `--raw-bench` ablak és vsync nélkül méri a frame valós költségét (lásd: [docs/01-spike-a-mercek.md](docs/01-spike-a-mercek.md)):

```bash
cargo run --release -p dada-vj -- --raw-bench 600 --size 1920x1080 --layers 4
```

Első mérési eredmény (M4 Pro): 1080p-n egy teljes képernyős, alpha-blendelt shader-réteg ~**0.06 ms**,
a 4 layeres teljes kép ~**0.28 ms** — a 16.67 ms-os büdzsé **1.7%-a**. A kompozitálás tehát nem
szűk keresztmetszet; a büdzsét a videó-dekód és a drága shaderek töltik ki.

## Elvek

1. **Zero-copy** a videó úton: VideoToolbox → IOSurface → Metal textúra, CPU-másolás nélkül.
2. **A zenei óra az úr**, nem a dekóder: kimaradt frame-et eldobunk, nem akadunk.
3. **Az engine UI nélkül is teljes értékű** — a böngésző sosem kritikus úton.
4. **Hardveres dekód/encode**, a CPU az Ableton Live-é.
