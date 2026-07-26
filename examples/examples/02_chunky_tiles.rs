//! 02: Chunky tiles -- board-game counters, the Civilization / Age of Wonders look.
//!
//! Instead of one glyph per world cell, each strategic tile is an `8x4`
//! character-cell block ([`tilekit::geom::SquareLayout::CHUNKY`], chosen
//! because 8x4 cells reads as roughly square at the usual 1:2 monospace
//! aspect). A block of world cells is aggregated down to a single dominant
//! biome, drawn as a beveled counter with a centered glyph, a landmark marker
//! if one falls inside it, and road/river connector marks toward whichever
//! neighbouring tiles also carry a road or river -- the same "only draw a
//! connector where both sides agree" rule 4-bit autotiling uses, just applied
//! by hand at the tile's four edge midpoints instead of by a bitmask lookup.
//!
//! Techniques on show:
//!
//! - **World aggregation**: many world cells collapse to one strategic tile by
//!   majority-vote biome, the same level of abstraction a 4X game actually
//!   turns on (nobody plays Civilization at the terrain-cell level).
//! - **Beveled tile faces**: a lit top edge and shadowed bottom edge (the same
//!   northwest-light convention as [`tilekit::palette::hillshade_nw`]) turn a
//!   flat color fill into a tile that reads as a raised counter, the way
//!   Civilization's and Age of Wonders' terrain hexes/squares are rendered.
//! - **Edge connectors, not solid lines**: a road overlays the tile only at
//!   the midpoints facing a neighbour that also has a road, so a route reads
//!   as a continuous line crossing tile boundaries rather than a stub that
//!   dead-ends at every edge.
//!
//! See Amit Patel / Red Blob Games on
//! [depicting terrain](https://www.redblobgames.com/maps/terrain-from-noise/)
//! for the general aggregation idea, and `01_terrain_cells.rs` for the
//! per-cell view this demo is the strategic counterpart to.
//!
//! ```sh
//! cargo run --example 02_chunky_tiles --features crossterm
//! cargo run --example 02_chunky_tiles --features software
//! cargo run --example 02_chunky_tiles --features gl
//! cargo run --example 02_chunky_tiles  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{SquareLayout, Tile};
use tilekit::palette::{self, hillshade_nw, mix, scale};
use tilekit::world::{Biome, World};

/// World size in cells. Large enough that even at the small [`SquareLayout::MEDIUM`]
/// zoom the map does not run out of tiles at the edges of a wide terminal.
const WORLD_W: i32 = 220;
/// See [`WORLD_W`].
const WORLD_H: i32 = 150;

/// Same vertical-exaggeration constant `01_terrain_cells` uses; kept identical
/// so switching between the two demos doesn't also change how mountainous the
/// same seed looks.
const RELIEF: f32 = 55.0;

/// One aggregated strategic tile.
#[derive(Clone, Copy)]
struct StratTile {
    biome: Biome,
    /// Average elevation across the block, for the bevel's hillshade term.
    elevation: f32,
    has_river: bool,
    has_road: bool,
    /// A settlement/landmark marker to draw centered on the tile, if any.
    marker: Option<(char, Color)>,
}

/// The aggregated strategic map: one [`StratTile`] per `tile_w x tile_h`
/// block of world cells, rebuilt whenever the zoom level or seed changes.
struct StrategicMap {
    cols: i32,
    rows: i32,
    tiles: Vec<StratTile>,
}

impl StrategicMap {
    /// Aggregates `world` into blocks of `tile_w x tile_h` world cells.
    ///
    /// Biome is decided by majority vote over the block rather than by
    /// sampling its center cell: a tile straddling a coastline should read as
    /// whichever terrain actually covers more of it, not flip entirely based
    /// on one sample landing a cell either side of the shore.
    fn build(world: &World, tile_w: i32, tile_h: i32) -> Self {
        // `div_ceil` on a signed integer is still unstable; world width and
        // height and the tile block sizes are always positive here, so plain
        // integer division plus a remainder check is equivalent.
        let cols = world.width() / tile_w + i32::from(world.width() % tile_w != 0);
        let rows = world.height() / tile_h + i32::from(world.height() % tile_h != 0);
        let mut tiles = Vec::with_capacity((cols * rows) as usize);

        for ty in 0..rows {
            for tx in 0..cols {
                let (x0, y0) = (tx * tile_w, ty * tile_h);
                let mut counts: Vec<(Biome, u32)> = Vec::new();
                let mut elevation_sum = 0.0;
                let mut sampled = 0u32;
                let (mut has_river, mut has_road) = (false, false);
                let mut marker = None;

                for dy in 0..tile_h {
                    for dx in 0..tile_w {
                        let (x, y) = (x0 + dx, y0 + dy);
                        if !world.in_bounds(x, y) {
                            continue;
                        }
                        let biome = world.biome_at(x, y);
                        if let Some(slot) = counts.iter_mut().find(|(b, _)| *b == biome) {
                            slot.1 += 1;
                        } else {
                            counts.push((biome, 1));
                        }
                        elevation_sum += world.elevation_at(x, y);
                        sampled += 1;
                        has_river |= world.river_at(x, y);
                        has_road |= world.road_at(x, y);
                        if let Some(landmark) = world.landmark_at(x, y) {
                            marker = Some(landmark.site.glyph_color());
                        }
                    }
                }

                // Ties break on the biome's own ordering, not on iteration order.
                // `HashMap` iteration is randomized per process, so
                // `max_by_key(count)` alone silently returns a different
                // winner run to run wherever two biomes tie, which makes the
                // whole map non-reproducible from its seed.
                let biome = counts
                    .into_iter()
                    .max_by_key(|&(biome, n)| (n, core::cmp::Reverse(biome)))
                    .map_or(Biome::Ocean, |(b, _)| b);
                let elevation = if sampled > 0 {
                    elevation_sum / sampled as f32
                } else {
                    0.0
                };
                tiles.push(StratTile {
                    biome,
                    elevation,
                    has_river,
                    has_road,
                    marker,
                });
            }
        }

        Self { cols, rows, tiles }
    }

    fn get(&self, tx: i32, ty: i32) -> Option<StratTile> {
        if tx < 0 || ty < 0 || tx >= self.cols || ty >= self.rows {
            return None;
        }
        self.tiles.get((ty * self.cols + tx) as usize).copied()
    }
}

/// The two zoom levels this demo cycles between, wrapping
/// [`tilekit::geom::SquareLayout`]'s two chunky presets and the world-cell
/// block size each aggregates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zoom {
    /// [`SquareLayout::MEDIUM`], each tile a 6x6 world-cell block.
    Medium,
    /// [`SquareLayout::CHUNKY`], each tile a 10x10 world-cell block. The
    /// larger screen footprint gets a proportionally larger sample so the two
    /// zoom levels show comparably detailed terrain, not the same aggregation
    /// just stretched.
    Chunky,
}

impl Zoom {
    const fn layout(self) -> SquareLayout {
        match self {
            Self::Medium => SquareLayout::MEDIUM,
            Self::Chunky => SquareLayout::CHUNKY,
        }
    }

    const fn block(self) -> (i32, i32) {
        match self {
            Self::Medium => (6, 6),
            Self::Chunky => (10, 10),
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Medium => Self::Chunky,
            Self::Chunky => Self::Medium,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Medium => "medium (4x2)",
            Self::Chunky => "chunky (8x4)",
        }
    }
}

/// State: the world, its current strategic aggregation, and view/camera.
pub struct ChunkyTiles {
    world: World,
    map: StrategicMap,
    zoom: Zoom,
    /// Top-left visible tile, in tile coordinates (not cells: chunky tiles pan
    /// one tile at a time, since a partial tile at the edge would need a
    /// second bevel origin and isn't worth the complexity for a demo about
    /// the tile's own look).
    origin: Tile,
    /// Tile currently under the cursor, for the hover ring.
    selected: Tile,
    time: f32,
    fps: FpsMeter,
}

impl Default for ChunkyTiles {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 11);
        let zoom = Zoom::Chunky;
        let (bw, bh) = zoom.block();
        let map = StrategicMap::build(&world, bw, bh);
        let start = tile_of_world_pos(&map, bw, bh, world.start_position());
        Self {
            world,
            map,
            zoom,
            origin: Tile::new(0, 0),
            selected: start,
            time: 0.0,
            fps: FpsMeter::new(),
        }
    }
}

/// Which strategic tile a world position falls in, for centering the camera
/// on the capital at startup.
fn tile_of_world_pos(map: &StrategicMap, block_w: i32, block_h: i32, pos: (i32, i32)) -> Tile {
    let tx = (pos.0 / block_w).clamp(0, map.cols - 1);
    let ty = (pos.1 / block_h).clamp(0, map.rows - 1);
    Tile::new(tx, ty)
}

impl ChunkyTiles {
    fn rebuild(&mut self) {
        let (bw, bh) = self.zoom.block();
        self.map = StrategicMap::build(&self.world, bw, bh);
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        self.rebuild();
        let (bw, bh) = self.zoom.block();
        self.selected = tile_of_world_pos(&self.map, bw, bh, self.world.start_position());
        self.origin = Tile::new(
            (self.selected.col - 4).max(0),
            (self.selected.row - 4).max(0),
        );
    }

    fn toggle_zoom(&mut self) {
        self.zoom = self.zoom.next();
        self.rebuild();
    }

    fn pan(&mut self, dx: i32, dy: i32) {
        self.origin = Tile::new(
            (self.origin.col + dx).clamp(0, (self.map.cols - 1).max(0)),
            (self.origin.row + dy).clamp(0, (self.map.rows - 1).max(0)),
        );
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        // Read before `drain_events` takes its borrow: `Terminal::size` can't
        // be called again until the returned iterator is dropped.
        let content_top = i32::from(term.size().height >= 3);
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.pan(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.pan(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.pan(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.pan(1, 0),
                    KeyCode::Char('z' | 'Z') => self.toggle_zoom(),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                },
                Event::Mouse(mouse) => self.handle_mouse(content_top, mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, content_top: i32, kind: MouseEventKind, pos: retroglyph_core::Pos) {
        let layout = self.zoom.layout();
        let sx = i32::from(pos.x);
        let sy = i32::from(pos.y) - content_top;
        if sy < 0 {
            return;
        }
        let (tx, ty) = (
            self.origin.col + sx / layout.w,
            self.origin.row + sy / layout.h,
        );
        match kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                if tx >= 0 && ty >= 0 && tx < self.map.cols && ty < self.map.rows {
                    self.selected = Tile::new(tx, ty);
                }
            }
            MouseEventKind::ScrollUp => self.pan(0, -1),
            MouseEventKind::ScrollDown => self.pan(0, 1),
            _ => {}
        }
    }

    /// Draws one tile's beveled face plus its glyph, marker, and connectors.
    fn draw_tile<B: Backend>(
        &self,
        term: &mut Terminal<B>,
        area: Rect,
        tx: i32,
        ty: i32,
        sx: i32,
        sy: i32,
    ) {
        let Some(tile) = self.map.get(tx, ty) else {
            return;
        };
        let layout = self.zoom.layout();
        let selected = Tile::new(tx, ty) == self.selected;
        let neighbor_of_selected =
            (tx - self.selected.col).abs() + (ty - self.selected.row).abs() == 1;

        // Hillshade the same way 01_terrain_cells does, from the average
        // elevation across the block rather than a per-cell gradient: a
        // strategic tile is one flat counter, so it gets one shade value.
        let shade = if tile.biome.is_water() {
            let phase = self
                .time
                .mul_add(1.1, (tx as f32).mul_add(0.7, ty as f32 * 0.5));
            phase.sin().mul_add(0.06, 1.0)
        } else {
            hillshade_nw((tile.elevation - 0.5) * RELIEF * 0.02, 0.0).mul_add(0.5, 0.7)
        };
        let base = scale(tile.biome.color(), shade);

        for dy in 0..layout.h {
            for dx in 0..layout.w {
                let (cx, cy) = (sx + dx, sy + dy);
                if cx < 0
                    || cy < 0
                    || cx >= i32::from(area.width())
                    || cy >= i32::from(area.height())
                {
                    continue;
                }
                let mut face = bevel(base, dx, dy, layout);
                if selected {
                    face = mix(face, palette::rgb(255, 236, 170), 0.34);
                } else if neighbor_of_selected {
                    face = mix(face, palette::rgb(255, 236, 170), 0.10);
                }
                term.put_styled(
                    area.left() + cx as u16,
                    area.top() + cy as u16,
                    ' ',
                    Style::new().bg(face),
                );
            }
        }

        let center = (layout.w / 2, layout.h / 2);
        let glyph_fg = mix(tile.biome.color(), palette::WHITE, 0.55);
        put_glyph(
            term,
            area,
            sx,
            sy,
            center,
            tile.biome.glyph(),
            glyph_fg,
            base,
            layout,
        );

        if let Some((marker, color)) = tile.marker {
            put_glyph(
                term,
                area,
                sx,
                sy,
                (center.0 + 1, center.1),
                marker,
                color,
                base,
                layout,
            );
        } else if tile.has_road {
            let road_color = palette::rgb(214, 196, 156);
            put_glyph(
                term, area, sx, sy, center, '\u{00b7}', road_color, base, layout,
            );
        }

        // Connectors: only drawn toward a neighbour that also carries a road
        // or river, exactly the "both sides must agree" rule 4-bit
        // autotiling encodes in a bitmask -- here just spelled out by hand at
        // the tile's four edge midpoints, since one tile has only one glyph's
        // worth of decoration to spend on it.
        for (dcx, dcy, at) in [
            (0, -1, (layout.w / 2, 0)),
            (0, 1, (layout.w / 2, layout.h - 1)),
            (-1, 0, (0, layout.h / 2)),
            (1, 0, (layout.w - 1, layout.h / 2)),
        ] {
            let Some(neighbor) = self.map.get(tx + dcx, ty + dcy) else {
                continue;
            };
            if tile.has_river && neighbor.has_river {
                put_glyph(
                    term,
                    area,
                    sx,
                    sy,
                    at,
                    '~',
                    palette::rgb(120, 182, 235),
                    base,
                    layout,
                );
            } else if tile.has_road && neighbor.has_road {
                let road_color = palette::rgb(214, 196, 156);
                put_glyph(term, area, sx, sy, at, '\u{00b7}', road_color, base, layout);
            }
        }
    }

    fn draw_map<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let layout = self.zoom.layout();
        let visible_cols = i32::from(area.width()) / layout.w + 1;
        let visible_rows = i32::from(area.height()) / layout.h + 1;

        for row in 0..visible_rows {
            for col in 0..visible_cols {
                let (tx, ty) = (self.origin.col + col, self.origin.row + row);
                self.draw_tile(term, area, tx, ty, col * layout.w, row * layout.h);
            }
        }
    }

    fn status(&self) -> String {
        let Some(tile) = self.map.get(self.selected.col, self.selected.row) else {
            return format!("zoom {}  seed {}", self.zoom.label(), self.world.seed());
        };
        let mut parts = vec![
            format!("({}, {})", self.selected.col, self.selected.row),
            tile.biome.name().to_owned(),
        ];
        if tile.has_river {
            parts.push("river".to_owned());
        }
        if tile.has_road {
            parts.push("road".to_owned());
        }
        parts.push(format!("elev {:.0}%", tile.elevation * 100.0));
        parts.push(format!("zoom {}", self.zoom.label()));
        parts.push(format!("seed {}", self.world.seed()));
        parts.join("  ")
    }
}

/// Beveled face color for one cell of a chunky tile: lit top edge, shadowed
/// bottom, faint left/right relief. Same convention `19_overworld`-style
/// square-tile bevels use in `retroglyph`'s own example gallery, and the same
/// northwest-light direction as [`hillshade_nw`].
fn bevel(face: Color, dx: i32, dy: i32, layout: SquareLayout) -> Color {
    let mut c = face;
    if dy == 0 {
        c = mix(c, palette::WHITE, 0.16);
    } else if dy == layout.h - 1 {
        c = mix(c, palette::BLACK, 0.34);
    }
    if dx == 0 {
        c = mix(c, palette::WHITE, 0.08);
    } else if dx == layout.w - 1 {
        c = mix(c, palette::BLACK, 0.20);
    }
    c
}

/// Writes one glyph at a tile-relative offset, clipped to `area`.
#[allow(clippy::too_many_arguments)]
fn put_glyph<B: Backend>(
    term: &mut Terminal<B>,
    area: Rect,
    sx: i32,
    sy: i32,
    (dx, dy): (i32, i32),
    glyph: char,
    fg: Color,
    face: Color,
    layout: SquareLayout,
) {
    let (cx, cy) = (sx + dx, sy + dy);
    if cx < 0 || cy < 0 || cx >= i32::from(area.width()) || cy >= i32::from(area.height()) {
        return;
    }
    let bg = bevel(face, dx, dy, layout);
    term.put_styled(
        area.left() + cx as u16,
        area.top() + cy as u16,
        glyph,
        Style::new().fg(fg).bg(bg),
    );
}

impl Demo for ChunkyTiles {
    const NAME: &'static str = "02_chunky_tiles";
    const TITLE: &'static str = "02 Chunky tiles";
    const BLURB: &'static str =
        "Board-game counters: beveled 8x4 tiles aggregated from world cells.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("Z", "zoom"),
            ("R", "reroll"),
            ("click", "select"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        ui::fill(term, content, Style::new().bg(ui::BG));
        self.draw_map(term, content);
        ui::title_bar::<B, Self>(term, title);
        let text = self.status();
        ui::status_bar::<B, Self>(term, status, &text, &self.fps);

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(ChunkyTiles);
