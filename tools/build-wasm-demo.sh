#!/usr/bin/env bash
# Builds one demo for one WASM backend variant, runs wasm-bindgen over it, and
# packages the result with the matching HTML template into a destination
# directory.
#
# Shared by tools/build-wasm-gallery.sh (which loops over every demo and
# variant for the published site) and by `just wasm-demo` (one demo at a time,
# served locally), so both produce byte-identical output and a local preview
# genuinely previews what gets deployed.
#
# Usage: tools/build-wasm-demo.sh <demo> <software|gl|terminal> <dest-dir>

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <demo> <software|gl|terminal> <dest-dir>" >&2
  exit 1
fi

demo="$1"
variant="$2"
dest="$3"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
examples_dir="$repo_root/examples"
templates_dir="$repo_root/web/templates"

case "$variant" in
  software) features=software; template=software.html ;;
  gl) features=gl; template=gl.html ;;
  terminal) features=wasm-terminal; template=terminal.html ;;
  *)
    echo "unknown variant: $variant (expected software, gl, or terminal)" >&2
    exit 1
    ;;
esac

# The wasm-release profile (see the workspace Cargo.toml) optimizes for size
# rather than speed, which roughly halves what a visitor downloads. `--profile`
# rather than `--release` also means the output lands in its own target
# directory, so a native release build and a wasm build don't invalidate each
# other's artifacts every time you switch.
cargo build \
  --manifest-path "$examples_dir/Cargo.toml" \
  --target wasm32-unknown-unknown \
  --profile wasm-release \
  --example "$demo" \
  --features "$features"

mkdir -p "$dest"
"$repo_root/tools/wasm-bindgen-shim.sh" \
  --target web \
  --out-dir "$dest" \
  --out-name "$demo" \
  --no-typescript \
  "$repo_root/target/wasm32-unknown-unknown/wasm-release/examples/$demo.wasm"

sed "s/__DEMO__/$demo/g" "$templates_dir/$template" > "$dest/index.html"
