#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo test --locked -p yuge-hasi-devkit
cargo clippy --locked -p yuge-hasi-devkit --all-targets -- -D warnings
cargo build --locked --release -p yuge-hasi-devkit
