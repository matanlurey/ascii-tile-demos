#!/usr/bin/env bash
# Builds every demo for every WASM variant and writes the gallery index that
# links to them.
#
# Convention over configuration: the demo list comes from
# `examples/examples/*.rs`, and each demo's title, blurb, and key bindings come
# from running the demo binary with ATD_PRINT_META set. Nothing here is a
# hand-maintained list, so adding a demo means adding one file (plus its
# [[example]] stanza) and nothing else.
#
# Usage: tools/build-wasm-gallery.sh [out-dir] [variant...]
#   out-dir defaults to dist/
#   variants default to "software gl"

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
examples_dir="$repo_root/examples"
templates_dir="$repo_root/web/templates"
out_dir="${1:-$repo_root/dist}"
shift || true

if [ "$#" -gt 0 ]; then
  variants=("$@")
else
  # The terminal (xterm.js) variant is built only on request: it duplicates the
  # look of the native terminal backend and roughly doubles gallery build time
  # for a view most visitors will not pick.
  variants=(software gl)
fi

label_for() {
  case "$1" in
    software) echo "Software" ;;
    gl) echo "WebGL2" ;;
    terminal) echo "Terminal" ;;
    *) echo "$1" ;;
  esac
}

mkdir -p "$out_dir"

# Metadata is harvested once, from a single native build of every demo, before
# any WASM work happens: a metadata run is a normal process launch, and the
# WASM artifacts cannot be executed here to ask them anything.
echo "== harvesting demo metadata ==" >&2
cargo build --manifest-path "$examples_dir/Cargo.toml" --examples --quiet

rows=""
count=0

for demo_path in "$examples_dir"/examples/*.rs; do
  demo="$(basename "$demo_path" .rs)"
  count=$((count + 1))
  echo "== $demo ==" >&2

  meta="$(ATD_PRINT_META=1 "$repo_root/target/debug/examples/$demo")"
  title="$(printf '%s' "$meta" | cut -f2)"
  blurb="$(printf '%s' "$meta" | cut -f3)"
  keys="$(printf '%s' "$meta" | cut -f4)"

  links=""
  for variant in "${variants[@]}"; do
    echo "-- $demo / $variant --" >&2
    "$repo_root/tools/build-wasm-demo.sh" "$demo" "$variant" "$out_dir/$demo/$variant"
    links="$links<a class=\"run\" href=\"./$demo/$variant/\">$(label_for "$variant")</a>"
  done

  rows="$rows<article class=\"card\">"
  # The thumbnail is written later by `gen-thumbnails`, so this links a file
  # that does not exist yet. That is fine for a static build, and the
  # `onerror` handler covers the demos the headless renderer cannot draw
  # faithfully, which deliberately ship no thumbnail at all.
  rows="$rows<a class=\"thumb\" href=\"./$demo/${variants[0]}/\">"
  rows="$rows<img src=\"./$demo/thumb.png\" alt=\"$title\" loading=\"lazy\" decoding=\"async\""
  rows="$rows onerror=\"this.closest('.thumb').classList.add('missing');this.remove()\">"
  rows="$rows</a>"
  rows="$rows<h2>$title</h2>"
  rows="$rows<p class=\"blurb\">$blurb</p>"
  [ -n "$keys" ] && rows="$rows<p class=\"keys\">$keys</p>"
  rows="$rows<div class=\"links\">$links</div>"
  rows="$rows</article>\n"
done

# Bash parameter expansion rather than `sed`: the accumulated rows are one
# block per demo joined by real newlines, and both GNU and BSD sed choke on a
# `s///` whose replacement spans that many embedded newlines. `${var//from/to}`
# is plain string substitution with no such limit, and it passes `&`, `/`, and
# newlines through verbatim.
template="$(cat "$templates_dir/index.html")"
rows_expanded="$(printf '%b' "$rows")"
page="${template//__CARDS__/$rows_expanded}"
page="${page//__COUNT__/$count}"
printf '%s\n' "$page" > "$out_dir/index.html"

echo "Wrote $out_dir/index.html and $count demo(s) x ${#variants[@]} variant(s)." >&2
