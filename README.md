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

The `retroglyph-*` dependencies are currently pinned to a git revision rather
than a crates.io version: the demos track APIs that have landed on retroglyph's
`main` but are not published yet. Nothing is a path dependency, so the repo still
clones and builds standalone; the pin goes back to a version requirement once the
next retroglyph release goes out. The pin is currently `e878716b`. What the
previous revision changed, and what tripped this repo up on the way, is written
up in [retroglyph#538](https://github.com/crates-lurey-io/retroglyph/issues/538);
`e878716b` needed no code change to compile, but it retired two workarounds (see
"Things worth knowing") and shipped one crash
([retroglyph#567](https://github.com/crates-lurey-io/retroglyph/issues/567)),
which is why the windowed backends are unusable at this pin until that lands.

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

The first seventeen are each about one rendering technique, drawn full-bleed.
The rest are about what those techniques add up to: a whole game interface, at
the resolution a game would actually use.

| # | Demo | Technique |
| --- | --- | --- |
| 18 | `panel_chrome` | A three-column roguelike interface: framed panels, gauges, a colored log |
| 19 | `hex_command` | A hex drawn as a multi-cell blob, with coordinate rulers and a command menu |
| 20 | `realm_map` | Painted 4X tiles with a live movement-path preview and a turn boundary |
| 21 | `deck_plan` | A ship deck as a labelled blueprint, with a typed command line |
| 22 | `overworld_quest` | Dithered cliff terraces, an entity layer, and a legend derived from what is on screen |
| 23 | `iso_tactics` | Depth-sorted walls with height, occlusion cutaway, and a HUD drawn in map space |
| 24 | `torchlit_crypt` | Colored additive lighting, and why it is not the same question as field of view |
| 25 | `flag_war` | Territory as colored ASCII trigrams, where flags pull population rather than order it |
| 26 | `hexcrawl` | A hand-drawn referee map: terrain that ignores the hex grid inked over it |

The last batch is about playing on a phone. Each one is adapted from a specific
turn-based game, drawn at a scale where an entity spans many cells, and built so
every control can be hit with a thumb.

| # | Demo | Adapted from | Technique |
| --- | --- | --- | --- |
| 27 | `rhythm_crypt` | Crypt of the NecroDancer | A beat track driving a dungeon of chunky tiles, played with a thumb D-pad or a swipe |
| 28 | `spire_deck` | Slay the Spire | A fanned card hand, telegraphed enemy intent, and tap-to-target versus drag-to-play |
| 29 | `ship_breach` | FTL | A ship cross-section where power is a fixed pool of pips you move between rooms |
| 30 | `fleet_command` | Crying Suns | A node star map and a lane battle, which become a tab switcher on a phone |
| 31 | `dice_tactics` | Slice & Dice | Die faces drawn large enough to read as dice, then assigned to targets |
| 32 | `loop_track` | Loop Hero | A hero who walks a loop you build around him, with legal slots lit before you commit |
| 33 | `onebit_quest` | OneBit Battle | Three desktop columns collapsing to one panel and a bottom tab bar |
| 34 | `ice_breach` | Netrunner, Monster Train | Cyberpunk intrusion up three vertical server lanes against a rising trace |
| 35 | `stealth_grid` | Invisible Inc | Wall-clipped vision cones, and why a dangerous move costs two taps |
| 36 | `court_reigns` | Reigns | One card, swiped left or right, previewing its consequences before you let go |

The batch after that takes the same approach to the interfaces of strategy and
party RPGs, where the screen is mostly chrome and the chrome is the point. Each
one is built around the single element that makes its source recognizable.

| # | Demo | Adapted from | Technique |
| --- | --- | --- | --- |
| 37 | `faith_war` | Dominions 5 | Armies drawn as formations, so strength is read as area rather than a number |
| 38 | `hex_general` | Fantasy General | A framed hex map whose bottom panels forecast the losses on both sides |
| 39 | `company_road` | Battle Brothers | Overland travel under a circle of vision, with time you can pause and speed up |
| 40 | `shard_realm` | Eador: Genesis | A gilded hero panel: portrait, stat grid, quest, and item slots, all live |
| 41 | `riven_route` | Vagrus | A weighted node web where a route is costed in supplies and days before you take it |
| 42 | `paper_dungeon` | Book of Demons | A papercraft crypt travelled on rails, with cards for an inventory |
| 43 | `bone_lord` | Iratus | An inked dungeon plan on parchment above a bench of four ranked squads |
| 44 | `dusk_field` | Battle for Wesnoth | The dense sidebar, and terrain defence read through a day/night cycle |
| 45 | `night_walk` | Traveller's Hymn | A bestiary that unredacts itself, in a world too dark to see across |
| 46 | `party_pause` | Baldur's Gate II | Real-time-with-pause, and the portrait column that makes it legible |

The last batch reaches further back, into 4X games, roguelikes, and board
wargames. Twelve candidates were screenshotted and seven cut for duplicating a
demo already here, which is why this table is shorter than the ones above.

| # | Demo | Adapted from | Technique |
| --- | --- | --- | --- |
| 47 | `hollow_talk` | Zorbus | Speech balloons with pointer tails, anchored in the dungeon rather than a log |
| 48 | `twin_planes` | Master of Magic | One set of coordinates, two overlaid worlds, and a toggle between them |
| 49 | `planet_fall` | Alpha Centauri | Isometric elevation under faction borders, with a three-way budget to spend |
| 50 | `veiled_hand` | Shadow of the Forbidden Gods | A hidden villain's task accruing per turn against two rising doom meters |
| 51 | `star_console` | Star Wars Rebellion | Draggable, closable, stacking windows inside a terminal |
| 52 | `quiet_march` | Divine Right | Region names set in spaced caps across a hex map drawn as cartography |
| 53 | `iron_colossus` | Ogre | Numbered counters against one giant unit whose parts are shot off in turn |

The final batch fills gaps rather than chasing genres. Every demo above is
drawn top-down, isometric, or on hexes, so the first entry here exists to add
a projection the gallery did not have at all.

| # | Demo | Adapted from | Technique |
| --- | --- | --- | --- |
| 54 | `walled_dawn` | Kingdom: Two Crowns | A side elevation with parallax, where night arrives on a timer either way |
| 55 | `warband_sheet` | Mordheim | A printed roster form carried entirely by typography and column rhythm |
| 56 | `open_terms` | Crusader Kings | One opinion total, itemised into signed modifiers you can act on |
| 57 | `dealt_dungeon` | Hand of Fate | The map and the deck are the same objects, dealt face-down |
| 58 | `carved_lair` | Dungeon Keeper | Terrain excavated at runtime, with invaders repathing around the cut |
| 59 | `city_works` | Civilization II | Small readouts ringing one large view, over a build queue |
| 66 | `saints_road` | Darklands | A pull-down menu bar with keyboard mnemonics, disabled items, and a flipping dropdown |
| 65 | `domesday_shire` | Conqueror AD 1086 | An isometric manor beside a minimap with a viewport rectangle derived from the camera |
| 64 | `edict_scales` | Tyranny | Two independent standing meters growing from a shared centre, plus a compositional spell builder |
| 63 | `grift_parley` | Griftlands | A negotiation board of linked argument entities, not a hand of cards |
| 62 | `quartered_arms` | Ultima Ratio Regum | Procedural heraldry as a glyph mosaic, reroll-guaranteed distinct per nation |
| 60 | `tyrant_age` | Lands of Achra | A wrapped, word-wise rich-text run: one sentence, six colors |
| 61 | `light_years` | Frontier: Elite II | Parallel projection of a 3D star volume, read by its Z-stalks |

Every demo animates on its own and responds to keys and mouse. `Q` or `Escape`
quits; `R` rerolls the world; arrows or WASD pan; drag pans. Per-demo keys are
listed in each demo's status bar and on the gallery page.

Demos 18 and up declare their own design grid through `Demo::GRID`, because a
three-column interface has a width below which it is no longer showing its
layout, only showing that it ran out of room. That is a design target rather
than a requirement: they still lay out from the live viewport, and the snapshot
tests deliberately run every one of them at 80x24, which is narrower than any
of them asks for.

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
knowledge of any demo and 211 unit tests.

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
| `path` | Least-cost routing over weighted terrain, and how far this turn's budget reaches |
| `light` | Additive colored light pools, torch flicker, and tone mapping |

### Things worth knowing

These are the non-obvious constraints this repo ran into. Each is worked around
in `tilekit` or the harness, so demos do not have to think about them.

**Blend through `tilekit::palette::mix`, not `Color::lerp`.** Not because
`Color::lerp` is broken any more (it resolves non-`Rgb` inputs itself as of
[retroglyph#518](https://github.com/crates-lurey-io/retroglyph/pull/518); it
used to return its first argument unchanged, which made
`Color::lerp(Color::BLACK, x, t)` a silent no-op returning black, since
`Color::BLACK` is an ANSI color). Two reasons remain: `mix` clamps `t`, so an
unclamped animation parameter cannot extrapolate past an endpoint into a wrapped
channel, and it does not need `retroglyph-core`'s optional `color-space` feature
(renamed from `gem` in retroglyph#556), which `Color::lerp` is gated behind.

**`hexal::Hex::line_to` is not contiguous.** As of hexal 0.1.1 it returns lines
with two-step jumps and repeated hexes along the `q == r` diagonal, so anything
walking a line looking for the first blocker can step straight over it.
`tilekit::geom::hex_line` implements the cube-lerp algorithm properly and is
tested for contiguity in every direction.

**A glyph has to be in CP437 or in the tileset, or it draws as a solid block.**
This was the sharpest constraint in the repo and it shaped most of the newer
demos. Half of it is now gone; the half that remains is what still matters.

The embedded bitmap font is CP437. A character outside it does not fail to draw;
it draws as a filled rectangle, which in a map of dense terrain glyphs reads as
"some cells are unusually bright" rather than as a bug.

The way out is a tileset, which overrides the font for the glyphs its codepage
names. `tools/gen-tileset` draws the missing 326 glyphs procedurally (they are
pure geometry) into `examples/assets/blocks.png`, and the harness registers it
for every demo on both pixel backends. Regenerate with `just tileset`.

**The color half of this constraint is dead as of retroglyph e878716b.** A
tileset sheet can now declare itself a `SheetColor::Mask`, and a mask sheet's
pixels are tinted by the cell's resolved foreground exactly the way a font
glyph's are
([retroglyph#548](https://github.com/crates-lurey-io/retroglyph/pull/548),
applied by both pixel backends in
[#557](https://github.com/crates-lurey-io/retroglyph/pull/557)). `blocks.png` is
a white mask, so `launch::block_tileset` sets `SheetColor::Mask` and quadrant,
sextant, and braille glyphs take their color like everything else. Before that
they rendered white and only white, which meant `16_subcell_canvas` computed a
real two-color result per cell and threw it away in three of its four panels.
`examples/tests/glyphs.rs::tileset_subcell_glyphs_take_the_foreground_color`
pins it.

A second route also opened and is not taken yet:
[retroglyph#550](https://github.com/crates-lurey-io/retroglyph/pull/550) made
both pixel backends resolve glyphs through a `FontChain`, so a
`BitmapFont::with_charset` fallback font can supply these characters directly
and skip the sprite path. That would retire the PNG, the codepage file, and
`gen-tileset` together. The sheet already exists and already works, so it stays
a follow-up.

**A tappable control is 9x4 cells, and that is why these demos are drawn
large.** The browser build fills the viewport and `retroglyph-window` caps the
device pixel ratio at 1.5, so one 8x16 cell is 5.33 x 10.67 CSS px. Apple asks
for a 44 pt touch target and Material for 48 dp; against that cell, 44 pt is
8.25 columns by 4.12 rows. A one-cell control is therefore a quarter the linear
size of the smallest thing a finger can reliably hit, which is the entire
argument for drawing demos 27 and up at interface scale rather than one glyph
per unit. `ui::touch` derives the constants once and `Hotspots::push_tappable`
grows a small control to a legal hit region without redrawing it larger.

The same arithmetic says what grid a phone actually hands over, and it is not
the shape a terminal usually is: 73x79 cells in portrait against 158x36 in
landscape. The responsive range is not "narrow to wide" but *tall and narrow* to
*wide and short*, so `ui::touch::Shape` classifies by which axis is scarce
rather than by width, and a demo that branched on width alone would break one of
the two.

The practical rule every demo from 18 on was written against: anything that
carries information through color must be a CP437 glyph. That is why
`ui::panel::bar` builds gauges from `█` and `▌` for half-cell precision instead
of from the eighth blocks `▏▎▍▋▊▉` a modern terminal UI would reach for.
Half-cell precision in the right color beat eighth-cell precision in white,
because in these interfaces the color carries the threshold and the fill only
carries the magnitude.

With the mask sheet the rule is now weaker than the code assumes: a glyph only
has to be *drawable* (CP437 or in the tileset), not CP437 specifically. The
eighth blocks are still neither, so `ui::panel::bar` is unchanged and
`half_blocks_are_colorable_but_eighth_blocks_are_not` still passes; adding them
to the sheet's codepage is all that stands between the gallery and eighth-cell
gauges.

This was found the hard way. Seven of `tilekit::glyphs::marker`'s ten constants
had never rendered, and `Site::glyph_color` reaches five of them, so every
capital, fortress, ruin, mine, and port in the gallery was a solid block.
`examples/tests/glyphs.rs` now renders every glyph constant twice at two
different foreground colors and asserts the result differs from the fallback
block *and* between the two colors; it also scans every demo's source for glyph
literals that cannot be drawn, because most glyphs here are inline in one demo
rather than shared, and two demos independently picked `▸` before that scan
existed.

**A sprite bigger than one cell needs a span, declared per draw call.** A
tileset sheet says nothing about how many cells its sprites occupy, and it
cannot: two sprites from one sheet can legitimately cover different footprints.
The span is where that is declared. `Surface::put_span_uniform(pos, size,
anchor, fill, style)` covers the common case: `anchor` is the glyph a pixel
backend looks up in its sprite cache and draws once across the whole footprint,
and `fill` is the text fallback the covered cells carry, which cell backends
print and pixel backends skip. `17_tileset_sprites` uses it for its 16x16
sprites over 2x1 cells of the 8x16 font grid. `put_span(pos, rows, style)` takes
explicit per-row strings for the rarer case where the fallback text is not
uniform.

**A sprite's `fg` is not a tint, but `Surface::with_tint` is.** Both pixel
backends composite an `Art` sheet's pixels verbatim, so `fg` does not shade them
and `bg` shows only through transparent ones. What changed at e878716b is that a
sprite can now be modulated on purpose: `Surface::with_tint(Tint::multiply(..))`
darkens and `Tint::mix(..)` blends toward a color, and both pixel backends apply
it ([retroglyph#545](https://github.com/crates-lurey-io/retroglyph/pull/545),
[#546](https://github.com/crates-lurey-io/retroglyph/pull/546),
[#557](https://github.com/crates-lurey-io/retroglyph/pull/557)).
`17_tileset_sprites` still animates its water by swapping between two water
sprites, which is the right tool there because the two sprites have different
silhouettes, not merely different shading.

A tint reaches sprites only. Font glyphs are painted in the cell's own `Style`,
tinted or not, so a lighting pass over ASCII terrain still resolves per-cell
`fg`/`bg` itself, the way `24_torchlit_crypt`, `44_dusk_field`, and
`45_night_walk` do.

**The winit driver only redraws on demand unless you opt out.** By default
`about_to_wait` gates every redraw on something having happened (input, resize,
an injected event) and otherwise leaves `ControlFlow::Wait` set, so an app that
animates from `Frame::delta` advances only while you move the mouse. That suits
an idle terminal-style app and nothing in this gallery, so the harness builds its
window config with `WindowConfig::animated(&renderer, title, 60)`, which is
`fit` with `event_driven: false`: render every tick, capped at 60.

The frame-rate cap and the redraw trigger are independent controls and both
apply on wasm, so nothing extra is needed there. This used to require a
workaround: `target_fps` was compiled out of the wasm build entirely, and the
harness injected one event per frame through the event-loop proxy just to keep
`requestAnimationFrame` scheduling itself. Fixed upstream in
[retroglyph#520](https://github.com/crates-lurey-io/retroglyph/pull/520) and
[#418](https://github.com/crates-lurey-io/retroglyph/pull/418); the pump is gone.

**A driver presents the frame, so `Demo::tick` must not.** Every driver
(`run_blocking`, both windowed drivers) calls `Terminal::present` once `tick`
returns, skipping it if the app already presented. Demos here draw through
`Terminal::surface` and return; the callers with no driver behind them (the
headless renders, the snapshot helpers, the `wasm-terminal` FFI tick) present
around `tick` themselves.

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
3. Add it to `examples/tests/snapshots.rs` and `tools/gen-thumbnails`.
4. Add a row to the table above.

Two things are worth knowing before starting. Use only CP437 glyphs for
anything whose color carries meaning, for the reasons under "Things worth
knowing" above; the glyph test scans your source and will fail the build
otherwise. And lay out from `term.area()` rather than from `Demo::GRID`, which
is a design target rather than a promise: the snapshot tests run every demo at
80x24, well under what an interface-heavy demo asks for, and `assert_draws_a_map`
rejects a layout that responds by drawing almost nothing.

Thumbnails are rendered by `tools/gen-thumbnails`, which runs after the gallery
build and drops a `thumb.png` into each demo's directory. It uses the headless
software backend rather than screenshotting the built pages, so it needs no
browser and no GPU, and it must configure that backend exactly as
`run_software` does. Two parts of that bite: the block tileset (without it every
braille and quadrant glyph falls back to CP437's solid block) and `Demo::GRID`
(without it an interface demo renders below its own responsive threshold, so the
thumbnail is a picture of the fallback layout with the panels the demo exists to
show missing entirely).

The same pass doubles as an animation gate: it renders each demo's settled
frame, compares it against several later ones, and fails if nothing moved. A
demo that has stopped animating still screenshots perfectly, so nothing else
in CI would catch it.

The tool declares a real `software` feature, on by default, and that matters:
including a demo's source resolves its `#[cfg(feature = ...)]` against the
*tool's* features, so without it `Demo::configure_software` is cfg'd out, the
trait's do-nothing default is used, and a demo that registers a tileset renders
its sprite codepoints as the font's fallback glyph instead. That was the real
cause of `17_tileset_sprites` coming out striped, which used to be attributed to
the headless renderer and excluded the demo from the gallery entirely.

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
