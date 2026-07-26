# ascii-tile-demos

[![CI](https://github.com/matanlurey/ascii-tile-demos/actions/workflows/ci.yml/badge.svg)](https://github.com/matanlurey/ascii-tile-demos/actions/workflows/ci.yml)
[![Pages](https://github.com/matanlurey/ascii-tile-demos/actions/workflows/pages.yml/badge.svg)](https://github.com/matanlurey/ascii-tile-demos/actions/workflows/pages.yml)

A gallery of tiled overland map rendering techniques, drawn entirely in a grid
of styled characters.

**[Run them in your browser](https://matanlurey.github.io/ascii-tile-demos/)**

Seventeen demos covering square, isometric, and hex tiles across a range of
visual styles, from a Dwarf Fortress-style glyph map through Age of Wonders
board-game counters to pixel-accurate sub-cell rendering. Built on
[retroglyph](https://github.com/crates-lurey-io/retroglyph) and
[hexal](https://github.com/crates-lurey-io/hexal).

The same source runs unchanged on five backends: a real terminal, a native
window (CPU or GPU), a browser canvas (CPU or WebGL2), and a headless in-memory
grid used for tests.

## Quick start

```sh
just setup            # adds the wasm32 target and pins wasm-bindgen
just list             # every demo with its blurb
just run 01_terrain_cells      # in your terminal
just run-sw 05_iso_diamond     # native window, CPU rasterized
just run-gl 07_hex_tiles       # native window, OpenGL 3.3
just serve            # build the WASM gallery and open it
```

Without `just`:

```sh
cargo run --manifest-path examples/Cargo.toml --example 01_terrain_cells --features crossterm
```

## Layout

| Path | What it is |
| --- | --- |
| `crates/tilekit/` | Shared building blocks: noise, world generation, tile geometry, autotiling, palettes, glyph banks, field of view, camera |
| `examples/` | The demo gallery. `src/` is the harness, `examples/` is one file per demo |
| `tools/gen-tileset/` | Generates the sprite sheet used by `17_tileset_sprites` |
| `tools/gen-thumbnails/` | Renders the gallery thumbnails, and fails the build if a demo stopped animating |
| `tools/*.sh` | WASM build scripts |
| `web/templates/` | HTML templates for the published gallery |

## The demos

| # | Demo | Technique |
| --- | --- | --- |
| 01 | `terrain_cells` | One glyph per tile: Whittaker biomes, hillshading, animated water |
| 02 | `chunky_tiles` | 8x4 cell tiles with bevels and edge connectors: board-game counters |
| 03 | `dual_grid` | Dual-grid corner tiling, side by side with naive per-cell rendering |
| 04 | `autotile_gallery` | 4-bit, 8-bit/47-blob, dual-grid, and marching squares, compared |
| 05 | `iso_diamond` | 2:1 dimetric isometric with painter's-algorithm depth sorting |
| 06 | `iso_elevation` | Staggered isometric with elevation stacking and cliff faces |
| 07 | `hex_tiles` | Pointy-top odd-r and flat-top odd-q hexes, with range overlays |
| 08 | `hex_outline` | Drawn honeycomb edges and territory outlines |
| 09 | `hex_subcell` | Pixel-accurate hexes via a braille canvas, versus cell-snapped |
| 10 | `political` | Province fills, one-cell borders, capitals, diplomacy tints |
| 11 | `fog_of_war` | Shroud, remembered terrain, recursive shadowcasting |
| 12 | `relief` | Hypsometric ramps, rotatable hillshading, contour lines, dithering |
| 13 | `parchment` | Pen-and-ink cartography: hatching, coastlines, labels, cartouche |
| 14 | `seasons` | One world, six moods: day/night and seasonal tint passes |
| 15 | `minimal` | Flat color fields, no glyph detail: the modern vector-map look |
| 16 | `subcell_canvas` | Half-block, quadrant, sextant, and braille sub-cell rendering |
| 17 | `tileset_sprites` | A PNG sprite sheet in the same grid, with an ASCII fallback |

Every demo animates on its own and responds to keys and mouse. `Q` or `Escape`
quits; `R` rerolls the world; arrows or WASD pan; drag pans. Per-demo keys are
listed in each demo's status bar and on the gallery page.

## Backends

| Backend | Feature | Notes |
| --- | --- | --- |
| Terminal | `crossterm` | Real TTY, raw mode, mouse capture |
| Native window (CPU) | `software` | winit + softbuffer, embedded bitmap font |
| Native window (GPU) | `gl` | OpenGL 3.3 via glow |
| Browser canvas (CPU) | `software` + `wasm32` | Same code, Canvas2D |
| Browser canvas (GPU) | `gl` + `wasm32` | Same code, WebGL2 |
| Headless | none | In-memory grid; the fallback and the test backend |
| Browser terminal | `wasm-terminal` | xterm.js over pushed ANSI. Plumbed, not built by default |

PNG tilesets are a pixel-backend capability, so `17_tileset_sprites` runs its
ASCII path on the terminal and headless backends. Every other demo renders the
same on all of them.

## tilekit

The shared crate is where the reusable, testable parts live. It has no
knowledge of any demo and 177 unit tests.

| Module | Contents |
| --- | --- |
| `noise` | Value noise, fBm, domain warping, ridged noise, a seeded PRNG |
| `world` | Heightmap, climate, Whittaker biomes, rivers, roads, settlements, Voronoi provinces |
| `geom` | Square, isometric, staggered, and hex projections in both directions, plus hex lines, rings, and spirals |
| `autotile` | 4-bit and 8-bit bitmasks, the 47-tile blob set, dual-grid corner tiling, marching squares |
| `palette` | Color ramps, biome and faction palettes, day/night and seasonal tints, hillshading |
| `glyphs` | Box drawing, block elements, braille, shade ramps, dithering, four sub-cell canvases |
| `fov` | Recursive shadowcasting, hex field of view, fog-of-war state |
| `camera` | A pan/zoom viewport in tile space with sub-tile scrolling |

### Things worth knowing

These are the non-obvious constraints this repo ran into. Each is worked around
in `tilekit` or the harness, so demos do not have to think about them.

**`Color::lerp` is a trap.** `retroglyph_core::Color::lerp` silently returns its
first argument unchanged if either input is not an `Rgb` variant, and
`Color::BLACK` is an ANSI color. `Color::lerp(Color::BLACK, x, t)` is therefore
a no-op that returns black, with no error and no warning. Use
`tilekit::palette::mix`, which resolves both inputs first.

**`hexal::Hex::line_to` is not contiguous.** As of hexal 0.1.1 it returns lines
with two-step jumps and repeated hexes along the `q == r` diagonal, so anything
walking a line looking for the first blocker can step straight over it.
`tilekit::geom::hex_line` implements the cube-lerp algorithm properly and is
tested for contiguity in every direction.

**The embedded bitmap font is CP437, and that is not extensible.** Both
`BitmapFont::try_char_to_index` and every font in a `FallbackFontChain` route
chars through `unicode_to_cp437`, so a fallback font can only fill gaps *within*
CP437, never add characters to the repertoire. CP437 has the shade ramp and the
four half blocks but none of the other ten quadrants, no sextants, and no
braille; those all resolve to the solid-block fallback, so a braille canvas
renders as a rectangle of solid color on the pixel backends.

The way out is a tileset, which does override the font for the glyphs its
codepage names. `tools/gen-tileset` draws the missing 328 glyphs procedurally
(they are pure geometry) into `examples/assets/blocks.png`, and the harness
registers it for every demo on both pixel backends. Regenerate with
`just tileset`.

**The winit driver is event-driven unless you ask for a frame rate, and on
wasm asking is not enough.** With `target_fps: None`, `about_to_wait` leaves
`ControlFlow::Wait` set and only requests a redraw when something happened, so
an animated app advances only while you move the mouse. That default suits an
idle terminal-style app and nothing in this gallery, so the harness passes
`Some(60)`.

That fixes native. In the published `retroglyph-window` 0.3.1 the whole
`frame_interval` branch is behind `#[cfg(not(target_arch = "wasm32"))]`, so on
wasm `target_fps` is not ignored, it is compiled out, and the browser build
freezes after one frame just the same. The harness therefore also injects one
event per frame through the event-loop proxy, which sets `needs_redraw` and
keeps `requestAnimationFrame` scheduling itself.

Upstream already has the real fix (retroglyph's own examples animate in the
browser); their unreleased tree moves the cfg to wrap only the *sleep*, leaving
`request_redraw()` on the wasm path. The workaround comes out when a release
carries it. See [retroglyph#510](https://github.com/crates-lurey-io/retroglyph/issues/510).

**The windowed drivers do not resize the terminal.** They resize the backend's
surface and push an `Event::Resize`, but never call `Terminal::resize`, and the
backend's own `size()` keeps reporting the configured grid. An app that ignores
that event renders at its startup grid size forever, which on the browser build
(where the window opens small and then fills the viewport) shows up as a black
band down two sides. The harness applies the resize centrally in
`DemoApp::update` and requeues every event, so demos see input exactly as
before.

## Development

```sh
just ci          # fmt check, clippy, tests, and every backend build
just test        # tests only
just lint        # clippy across each backend feature separately
just tileset     # regenerate examples/assets/terrain.png
```

Snapshot tests render each demo headless and compare against
`examples/tests/snapshots/`. Use `just test-review` to review changes and
`just test-accept` to take them.

Clippy runs with `pedantic` and `nursery` denied. Backends are mutually
exclusive at the dispatch level, so `--all-features` would only ever lint one
of them; `just lint` checks each separately.

## Adding a demo

1. Write `examples/examples/NN_name.rs` implementing `Demo` and ending in
   `ascii_tile_demos::demo_main!(YourDemo);`.
2. Add its `[[example]]` stanza to `examples/Cargo.toml`.
3. Add it to `examples/tests/snapshots.rs`.
4. Add a row to the table above.

Thumbnails are rendered by `tools/gen-thumbnails`, which runs after the gallery
build and drops a `thumb.png` into each demo's directory. It uses the headless
software backend rather than screenshotting the built pages, so it needs no
browser and no GPU, and it must configure that backend exactly as
`run_software` does (the block tileset especially, or every braille and
quadrant glyph falls back to CP437's solid block).

The same pass doubles as an animation gate: it renders each demo's settled
frame, compares it against several later ones, and fails if nothing moved. A
demo that has stopped animating still screenshots perfectly, so nothing else
in CI would catch it.

One demo ships no thumbnail. `17_tileset_sprites` draws 16x16 sprites across
two 8x16 cells via the tileset's `spacing(2, 1)`; the surfaced renderer honors
that and the headless one blits only the first cell, so the map comes out
striped. Its card falls back to a placeholder rather than showing a picture
that suggests the demo is broken when the page itself is fine.

The gallery needs nothing else: `tools/build-wasm-gallery.sh` finds demos by
globbing `examples/examples/*.rs` and reads each one's title, blurb, and key
bindings by running it with `ATD_PRINT_META=1`, so there is no catalog to keep
in sync.

## Credits

Techniques are cited in each demo's source header. The main sources:

- [Red Blob Games](https://www.redblobgames.com/) for hex grids, noise-based
  terrain, and pathfinding
- [Boris the Brave](http://www.boristhebrave.com/) on Wang tiles and autotiling
- [Jess Hammer's dual-grid tilemap system](https://github.com/jess-hammer/dual-grid-tilemap-system-godot)
- [RogueBasin](https://www.roguebasin.com/) for recursive shadowcasting
- [Catlike Coding](https://catlikecoding.com/unity/tutorials/marching-squares/)
  on marching squares
- [Inigo Quilez](https://www.iquilezles.org/articles/warp/) on domain warping

## License

MIT.
