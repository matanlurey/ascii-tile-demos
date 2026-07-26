#!/usr/bin/env bash
# Resolves a `wasm-bindgen` CLI whose version matches the `wasm-bindgen` crate
# this workspace actually links, installing it into .bin/ if it isn't there.
#
# The versions must match exactly. wasm-bindgen's generated JS glue and the
# symbols the Rust side exports are a private protocol between the crate and
# the CLI, and it changes between patch releases; a mismatch produces either a
# loud "schema version mismatch" or, worse, a module that loads and then
# misbehaves at runtime. Pinning only the crate (as Cargo.toml does) is half
# the job, since the CLI is not a cargo dependency at all.
#
# Usage: tools/wasm-bindgen-shim.sh <wasm-bindgen args...>

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="$repo_root/.bin"

# The version Cargo actually resolved, read from the lockfile rather than from
# Cargo.toml: the lockfile is what the build used, and it is the only place the
# answer is unambiguous.
want="$(
  awk '
    /^name = "wasm-bindgen"$/ { found = 1; next }
    found && /^version = / { gsub(/[",]/, "", $3); print $3; exit }
  ' "$repo_root/Cargo.lock"
)"

if [ -z "$want" ]; then
  echo "could not determine the wasm-bindgen version from Cargo.lock" >&2
  exit 1
fi

have=""
if [ -x "$bin_dir/wasm-bindgen" ]; then
  have="$("$bin_dir/wasm-bindgen" --version 2>/dev/null | awk '{print $2}')"
fi

if [ "$have" != "$want" ]; then
  echo "installing wasm-bindgen $want into .bin (found '${have:-none}')" >&2
  mkdir -p "$bin_dir"
  # `--root` keeps this out of ~/.cargo/bin, so a globally installed
  # wasm-bindgen at a different version stays untouched and this repo's
  # version is not a machine-wide side effect of building it.
  if command -v cargo-binstall >/dev/null 2>&1; then
    cargo binstall --no-confirm --root "$repo_root/.bin-cargo" "wasm-bindgen-cli@$want"
  else
    cargo install --locked --root "$repo_root/.bin-cargo" "wasm-bindgen-cli@$want"
  fi
  ln -sf "$repo_root/.bin-cargo/bin/wasm-bindgen" "$bin_dir/wasm-bindgen"
fi

exec "$bin_dir/wasm-bindgen" "$@"
