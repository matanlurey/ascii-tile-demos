# ascii-tile-demos -- task runner.
#
# `just` with no arguments lists every recipe.

_default:
    @just --list

# ── Running demos ───────────────────────────────────────────────────────────

# List every demo with its title and blurb.
list:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --manifest-path examples/Cargo.toml --examples --quiet
    for path in examples/examples/*.rs; do
        demo="$(basename "$path" .rs)"
        ATD_PRINT_META=1 "target/debug/examples/$demo" | awk -F'\t' '{printf "  %-22s %s\n", $1, $3}'
    done

# Run a demo in the terminal. Example: `just run 01_terrain_cells`
run demo:
    cargo run --manifest-path examples/Cargo.toml --example {{demo}} --features crossterm

# Run a demo in a native window, CPU rasterized.
run-sw demo:
    cargo run --manifest-path examples/Cargo.toml --example {{demo}} --features software

# Run a demo in a native window on OpenGL 3.3.
run-gl demo:
    cargo run --manifest-path examples/Cargo.toml --example {{demo}} --features gl

# Print a few frames of a demo to stdout via the headless backend.
run-headless demo frames="3":
    ATD_HEADLESS_FRAMES={{frames}} cargo run --manifest-path examples/Cargo.toml --example {{demo}}

# ── Web ─────────────────────────────────────────────────────────────────────

# Build the full WASM gallery into dist/.
wasm:
    ./tools/build-wasm-gallery.sh dist

# Build one demo for one variant into dist/. Example: `just wasm-demo 05_iso_diamond gl`
wasm-demo demo variant="software":
    ./tools/build-wasm-demo.sh {{demo}} {{variant}} dist/{{demo}}/{{variant}}

# Build the gallery and serve it at http://localhost:8080.
serve: wasm
    @echo "Serving dist/ at http://localhost:8080 (Ctrl-C to stop)"
    @(sleep 1 && (command -v xdg-open >/dev/null && xdg-open http://localhost:8080 || open http://localhost:8080 2>/dev/null || true)) &
    @cd dist && python3 -m http.server 8080

# Serve an already-built dist/ without rebuilding it.
serve-only:
    @cd dist && python3 -m http.server 8080

# ── Quality ─────────────────────────────────────────────────────────────────

# Everything CI would run.
ci: fmt-check lint test build-all

# Run every test.
test:
    cargo test --workspace --all-features

# Update insta snapshots that have changed on purpose.
test-accept:
    cargo insta accept --workspace

test-review:
    cargo insta review --workspace

# Clippy over every target and feature combination that matters.
#
# Backends are mutually exclusive at the `launch` level, so `--all-features`
# would only ever check the software arm and silently leave the gl, crossterm,
# and headless dispatch paths unlinted. Each has to be checked on its own.
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy --manifest-path examples/Cargo.toml --all-targets --features crossterm -- -D warnings
    cargo clippy --manifest-path examples/Cargo.toml --all-targets --features software -- -D warnings
    cargo clippy --manifest-path examples/Cargo.toml --all-targets --features gl -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# Build every demo against every backend, including wasm.
build-all:
    cargo build --workspace --all-targets
    cargo build --manifest-path examples/Cargo.toml --examples --features crossterm
    cargo build --manifest-path examples/Cargo.toml --examples --features software
    cargo build --manifest-path examples/Cargo.toml --examples --features gl
    cargo build --manifest-path examples/Cargo.toml --examples --features software --target wasm32-unknown-unknown
    cargo build --manifest-path examples/Cargo.toml --examples --features gl --target wasm32-unknown-unknown

# ── Assets ──────────────────────────────────────────────────────────────────

# Regenerate examples/assets/terrain.png. Deterministic: a no-op run must
# produce no diff.
tileset:
    cargo run -p gen-tileset

# ── Setup ───────────────────────────────────────────────────────────────────

# Install the toolchain pieces the wasm build needs.
setup:
    rustup target add wasm32-unknown-unknown
    @./tools/wasm-bindgen-shim.sh --version

# Remove build output and the locally installed wasm-bindgen.
clean:
    cargo clean
    rm -rf dist .bin .bin-cargo
