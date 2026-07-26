# CI notes

CI and GitHub Pages deployment are deliberately not wired up yet; this file
records what they will need so nothing has to be rediscovered.

## What a CI job has to do

```sh
rustup target add wasm32-unknown-unknown
just ci                    # fmt check, clippy per backend, tests, every build target
just wasm                  # writes dist/
```

`just lint` checks each backend feature separately on purpose. Backends are
mutually exclusive at the `launch` dispatch level, so `--all-features` compiles
only the software arm and silently leaves the gl, crossterm, and headless paths
unlinted.

## The wasm-bindgen version

`tools/wasm-bindgen-shim.sh` reads the exact `wasm-bindgen` version out of
`Cargo.lock` and installs a matching CLI into `.bin/` if one is not already
there. The crate and the CLI must match exactly: the generated JS glue and the
symbols the Rust side exports are a private protocol between them that changes
between patch releases, and a mismatch either fails loudly with a schema error
or, worse, loads and misbehaves at runtime.

CI should cache `.bin-cargo/` to avoid rebuilding the CLI on every run, or
install `cargo-binstall` first (the shim prefers it and it takes seconds
instead of minutes).

## Pages deployment

`dist/` is self-contained and relative-linked, so it can be uploaded as a Pages
artifact directly. Roughly 19 MB for 17 demos across 2 variants.

Nothing in the gallery build hardcodes a base URL; every link in
`web/templates/` is relative, so the site works from a project subpath
(`user.github.io/repo/`) as well as from a domain root.

## Build cost

A full `just wasm` from cold is about three minutes on an M-series laptop: 34
`cargo build` invocations (17 demos x 2 variants) that share almost all their
dependency compilation, plus 34 wasm-bindgen runs. Caching `target/` across
runs cuts it to well under a minute.
