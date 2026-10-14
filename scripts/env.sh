#!/usr/bin/env bash
# Dada VJ fejlesztői környezet.
# A Rust toolchain a projekten belül van, hogy ne kelljen rendszerszintű telepítés.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export DADA_ROOT="$ROOT"
export CARGO_HOME="$ROOT/.toolchain/cargo"
export RUSTUP_HOME="$ROOT/.toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
