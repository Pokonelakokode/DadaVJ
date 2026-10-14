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

## Elvek

1. **Zero-copy** a videó úton: VideoToolbox → IOSurface → Metal textúra, CPU-másolás nélkül.
2. **A zenei óra az úr**, nem a dekóder: kimaradt frame-et eldobunk, nem akadunk.
3. **Az engine UI nélkül is teljes értékű** — a böngésző sosem kritikus úton.
4. **Hardveres dekód/encode**, a CPU az Ableton Live-é.
