//! 07: Hex tiles -- Age of Wonders / Civ-style hex strategy tiles, in both
//! orientations.
//!
//! A strategic hex layer over an aggregated biome map: each hex is a filled,
//! beveled face colored by the dominant terrain underneath it, with a
//! centered glyph and a settlement marker. `T` swaps between pointy-top
//! (odd-r offset) and flat-top (odd-q offset) without changing anything about
//! the underlying world, which is the point -- orientation is a rendering
//! choice, not a data model choice.
//!
//! Every coordinate question here (distance, neighbors, rings, picking) is
//! answered through [`tilekit::geom::HexLayout`], which itself defers to
//! `hexal`'s axial coordinates rather than hand-rolled offset arithmetic. The
//! status bar deliberately prints both the axial `(q, r)` and the offset
//! `(col, row)` for the hovered hex, because the fact that both describe the
//! same hex -- and why algorithms prefer the former while storage prefers the
//! latter -- is the single most important idea in hex grids.
//!
//! Techniques on show:
//!
//! - **Axial vs. offset coordinates** ([`tilekit::geom::HexLayout::to_hex`]):
//!   `hexal`'s `(q, r)` is what every hex algorithm below actually runs on;
//!   offset `(col, row)` is only for addressing a rectangular map. See Amit
//!   Patel, [Coordinate systems](https://www.redblobgames.com/grids/hexagons/#coordinates).
//! - **Pointy-top vs. flat-top** ([`tilekit::geom::HexOrientation`]): the same
//!   world, drawn on two different lattices, with picking, rings, and
//!   neighbors all still correct in either. See Red Blob Games,
//!   [Hex geometry](https://www.redblobgames.com/grids/hexagons/#basics).
//! - **Aspect-corrected picking** ([`tilekit::geom::HexLayout::cell_to_tile`]):
//!   exact at hex centers and stable near edges, unlike naive nearest-center
//!   search in raw character-cell space.
//! - **Hex rings and spirals** ([`tilekit::geom::hex_ring`],
//!   [`tilekit::geom::hex_spiral`]): the selection halo and the movement-range
//!   overlay are both built from these, the same primitives a real 4X game
//!   uses for zone-of-control and unit range.
//!
//! ```sh
//! cargo run --example 07_hex_tiles --features crossterm
//! cargo run --example 07_hex_tiles --features software
//! cargo run --example 07_hex_tiles --features gl
//! cargo run --example 07_hex_tiles  # headless, prints a few frames
//! ```

use std::collections::HashMap;

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Terminal};

use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::geom::{Cell, HexLayout, Tile, hex_ring, hex_spiral};
use tilekit::palette::{self, mix, scale};
use tilekit::world::{Biome, Site, World};

/// World size in cells, aggregated into hexes at [`HEX_CELLS`] scale. Large
/// enough that a hex map still has real geography to show, not just a
/// handful of tiles.
const WORLD_W: i32 = 260;
/// See [`WORLD_W`].
const WORLD_H: i32 = 170;

/// How many world cells one hex tile aggregates along each axis of its
/// bounding box, independent of orientation. A strategic layer is coarser
/// than the terrain layer by design: 4x4 world cells collapse into one hex,
/// the same "board game counter" abstraction [`tilekit::geom::SquareLayout`]
/// gives the square demos.
const HEX_CELLS: i32 = 4;

/// One aggregated hex tile's summary of the world cells underneath it.
#[derive(Clone, Copy)]
struct HexTile {
    biome: Biome,
    /// Whether any underlying cell has a river, for a connector dot.
    river: bool,
    /// Whether any underlying cell has a road.
    road: bool,
    /// The best (highest-tier) settlement under this hex, if any.
    site: Option<Site>,
}

/// Aggregates a `HEX_CELLS`-square block of world cells into one summary.
///
/// Majority biome vote rather than sampling the center cell: sampling would
/// make a hex's terrain flicker between neighbors as the aggregation grid
/// shifts, while a vote is stable and also happens to be how real strategic
/// layers are built (a province "is" whatever most of it is).
fn aggregate(world: &World, origin_x: i32, origin_y: i32) -> HexTile {
    let mut counts: HashMap<Biome, u32> = HashMap::new();
    let (mut river, mut road) = (false, false);
    let mut site: Option<Site> = None;

    for dy in 0..HEX_CELLS {
        for dx in 0..HEX_CELLS {
            let (x, y) = (origin_x + dx, origin_y + dy);
            let biome = world.biome_at(x, y);
            *counts.entry(biome).or_insert(0) += 1;
            river |= world.river_at(x, y);
            road |= world.road_at(x, y);
            if let Some(landmark) = world.landmark_at(x, y) {
                // Capital beats city beats town beats everything else, so a
                // hex touching both a village and the capital shows the
                // capital -- the more strategically important marker wins.
                let rank = |s: Site| match s {
                    Site::Capital => 3,
                    Site::City => 2,
                    Site::Town => 1,
                    _ => 0,
                };
                if site.is_none_or(|current| rank(landmark.site) > rank(current)) {
                    site = Some(landmark.site);
                }
            }
        }
    }

    // Ties break on the biome's own ordering, not on iteration order.
    // `HashMap` iteration is randomized per process, so `max_by_key(count)`
    // alone silently returns a different winner run to run wherever two
    // biomes tie, which makes the whole map non-reproducible from its seed.
    let biome = counts
        .into_iter()
        .max_by_key(|&(biome, count)| (count, core::cmp::Reverse(biome)))
        .map_or(Biome::Ocean, |(b, _)| b);
    HexTile {
        biome,
        river,
        road,
        site,
    }
}

/// State: the world, the active hex layout, the selection, and animation.
pub struct HexTiles {
    world: World,
    /// Current orientation. `T` toggles between the two `HexLayout` constants.
    layout: HexLayout,
    /// Camera position, in cells, top-left of the viewport.
    origin: Cell,
    /// The selected hex, in offset tile coordinates for the active layout.
    selected: Tile,
    /// Movement-range radius, adjustable with +/-.
    range: i32,
    time: f32,
    fps: FpsMeter,
    /// Cache of aggregated hexes, since aggregation reads `HEX_CELLS^2` world
    /// cells per hex and every hex on screen is re-read every frame.
    cache: HashMap<(i32, i32, i32), HexTile>,
}

impl Default for HexTiles {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 11);
        let (sx, sy) = world.start_position();
        let layout = HexLayout::POINTY;
        let selected = layout.cell_to_tile(Cell::new(sx, sy));
        let mut demo = Self {
            world,
            layout,
            origin: Cell::new(0, 0),
            selected,
            range: 3,
            time: 0.0,
            fps: FpsMeter::new(),
            cache: HashMap::new(),
        };
        demo.center_on(selected);
        demo
    }
}

impl HexTiles {
    /// Aggregated tile at `tile`, memoized per `(orientation-tag, col, row)`
    /// since re-aggregating on every hover would touch 16 world cells per
    /// query for no visible benefit.
    fn tile_at(&mut self, tile: Tile) -> HexTile {
        let key = (self.orientation_tag(), tile.col, tile.row);
        if let Some(&cached) = self.cache.get(&key) {
            return cached;
        }
        // The hex's own bounding-box origin, scaled into world-cell space:
        // this is an approximation (hexes are not rectangles) but a
        // consistent one, and it is what "one hex equals one strategic
        // block" means in a character grid.
        let cell = self.layout.tile_to_cell(tile);
        let origin_x = cell.x.div_euclid(self.layout.pitch_x) * HEX_CELLS;
        let origin_y = cell.y.div_euclid(self.layout.pitch_y.max(1)) * HEX_CELLS;
        let summary = aggregate(&self.world, origin_x, origin_y);
        self.cache.insert(key, summary);
        summary
    }

    const fn orientation_tag(&self) -> i32 {
        match self.layout.orientation {
            tilekit::geom::HexOrientation::Pointy => 0,
            tilekit::geom::HexOrientation::Flat => 1,
        }
    }

    /// Swaps orientation, remapping the selection to the equivalent hex under
    /// the new layout so the cursor does not jump when you press `T`.
    fn toggle_orientation(&mut self) {
        let hex = self.layout.to_hex(self.selected);
        self.layout = match self.layout.orientation {
            tilekit::geom::HexOrientation::Pointy => HexLayout::FLAT,
            tilekit::geom::HexOrientation::Flat => HexLayout::POINTY,
        };
        self.selected = self.layout.from_hex(hex);
        self.center_on(self.selected);
        self.cache.clear();
    }

    fn center_on(&mut self, tile: Tile) {
        let center = self.layout.center_cell(tile);
        self.origin = Cell::new(
            center.x - i32::from(GRID_COLS) / 2,
            center.y - i32::from(GRID_ROWS) / 2,
        );
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        self.cache.clear();
        let (sx, sy) = self.world.start_position();
        self.selected = self.layout.cell_to_tile(Cell::new(sx, sy));
        self.center_on(self.selected);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let step = 6;
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => {
                            self.origin = self.origin.offset(0, -step);
                        }
                        KeyCode::Down | KeyCode::Char('s' | 'S') => {
                            self.origin = self.origin.offset(0, step);
                        }
                        KeyCode::Left | KeyCode::Char('a' | 'A') => {
                            self.origin = self.origin.offset(-step, 0);
                        }
                        KeyCode::Right | KeyCode::Char('d' | 'D') => {
                            self.origin = self.origin.offset(step, 0);
                        }
                        KeyCode::Char('t' | 'T') => self.toggle_orientation(),
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Char('=' | '+') => self.range = (self.range + 1).min(8),
                        KeyCode::Char('-' | '_') => self.range = (self.range - 1).max(0),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: retroglyph_core::Pos) {
        let screen = Cell::new(i32::from(pos.x), i32::from(pos.y) - 1);
        match kind {
            // A plain click selects. Dragging is deliberately not wired to
            // pan here (unlike 01_terrain_cells): with a hex grid, drag-pan
            // and click-select fighting over the same left button makes it
            // too easy to nudge the map while trying to select a hex right at
            // its edge. Keyboard panning stays exact.
            MouseEventKind::Down(MouseButton::Left) => {
                let world_cell = screen.offset(self.origin.x, self.origin.y);
                self.selected = self.layout.cell_to_tile(world_cell);
            }
            MouseEventKind::ScrollUp => self.origin = self.origin.offset(0, -3),
            MouseEventKind::ScrollDown => self.origin = self.origin.offset(0, 3),
            _ => {}
        }
    }

    /// Base fill color for a hex face, before bevel and selection tinting.
    fn face_color(tile: HexTile) -> Color {
        let mut color = tile.biome.color();
        if tile.biome.is_water() {
            color = mix(color, palette::WHITE, 0.08);
        }
        mix(color, ui::BG, 0.28)
    }

    /// Draws one hex's footprint into `area`, clipped, at world-cell offset
    /// `(sx, sy)` (the hex's top-left bounding-box corner in screen space).
    #[allow(clippy::too_many_arguments)]
    fn draw_hex<B: Backend>(
        &self,
        term: &mut Terminal<B>,
        area: Rect,
        sx: i32,
        sy: i32,
        tile: Tile,
        data: HexTile,
        highlight: f32,
        pulse: f32,
    ) {
        let mut face = Self::face_color(data);
        if highlight > 0.0 {
            face = mix(face, palette::rgb(255, 236, 170), highlight * pulse);
        }

        let (pitch_x, pitch_y) = (self.layout.pitch_x, self.layout.pitch_y);
        let is_pointy = matches!(
            self.layout.orientation,
            tilekit::geom::HexOrientation::Pointy
        );

        // Footprint. A pointy hex is drawn one row *taller* than its own row
        // pitch: a quarter-width taper row, a full-width middle, and another
        // taper. Consecutive hex rows then share their taper rows, and the
        // odd-row stagger offsets the two sets of taper cells so that together
        // they cover the shared row exactly once. That sharing is what makes
        // the honeycomb tessellate.
        //
        // Drawing only `pitch_y` rows instead (so that both of them end up
        // tapered) leaves half of every row unpainted, which reads as a grid
        // of separated tiles with black gutters rather than as a honeycomb.
        //
        // Flat hexes taper on the *column* axis instead, which the column
        // stagger already accounts for, so their footprint is the plain
        // rectangle their pitch describes.
        let rows = if is_pointy { pitch_y + 1 } else { pitch_y };
        for dy in 0..rows {
            let taper = if is_pointy && (dy == 0 || dy == rows - 1) {
                pitch_x / 4
            } else {
                0
            };
            for dx in taper..(pitch_x - taper) {
                let (px, py) = (sx + dx, sy + dy);
                let bg = bevel(face, dx - taper, dy, pitch_x - 2 * taper, rows);
                put_clipped(term, area, px, py, ' ', Style::new().bg(bg));
            }
        }

        // Connectors: a dot toward each neighbor sharing a river/road, so a
        // network reads as a continuous line through hex centers rather than
        // stubs at arbitrary points on the boundary.
        let center_x = sx + pitch_x / 2;
        let center_y = sy + pitch_y / 2;
        if data.river || data.road {
            for neighbor in self.layout.neighbors(tile) {
                let their = self.tile_at_readonly(neighbor);
                let ncenter = self.layout.center_cell(neighbor);
                let mycenter = self.layout.center_cell(tile);
                let (dx, dy) = (
                    (ncenter.x - mycenter.x).signum(),
                    (ncenter.y - mycenter.y).signum(),
                );
                if data.river && their.river {
                    let style = Style::new().fg(palette::rgb(120, 182, 235)).bg(face);
                    put_clipped(term, area, center_x + dx * 2, center_y + dy, '~', style);
                }
                if data.road && their.road {
                    let style = Style::new().fg(palette::rgb(214, 196, 156)).bg(face);
                    put_clipped(term, area, center_x + dx * 3, center_y - dy, '.', style);
                }
            }
        }

        let glyph_style = Style::new().fg(data.biome.glyph_fg()).bg(face);
        put_clipped(
            term,
            area,
            center_x,
            center_y,
            data.biome.glyph(),
            glyph_style,
        );

        if let Some(site) = data.site {
            let (marker, marker_color) = site.glyph_color();
            let style = Style::new().fg(marker_color).bg(face);
            put_clipped(term, area, center_x + 1, center_y, marker, style);
        }
    }

    /// Non-mutating lookup for use inside `draw_hex`, which only has `&self`.
    /// Falls back to a direct (uncached) aggregation on a cache miss, which is
    /// rare (only the outermost ring of drawn hexes) and cheap enough to not
    /// warrant threading `&mut self` through drawing.
    fn tile_at_readonly(&self, tile: Tile) -> HexTile {
        let key = (self.orientation_tag(), tile.col, tile.row);
        self.cache.get(&key).copied().unwrap_or_else(|| {
            let cell = self.layout.tile_to_cell(tile);
            let origin_x = cell.x.div_euclid(self.layout.pitch_x) * HEX_CELLS;
            let origin_y = cell.y.div_euclid(self.layout.pitch_y.max(1)) * HEX_CELLS;
            aggregate(&self.world, origin_x, origin_y)
        })
    }

    fn draw_map<B: Backend>(&mut self, term: &mut Terminal<B>, area: Rect) {
        let (pitch_x, pitch_y) = (self.layout.pitch_x, self.layout.pitch_y);
        let margin_cols = i32::from(area.width()) / pitch_x + 2;
        let margin_rows = i32::from(area.height()) / pitch_y + 2;

        let top_left_tile = self.layout.cell_to_tile(self.origin);
        let selected = self.selected;
        let pulse = (self.time * 3.2).sin().mul_add(0.5, 0.5);
        let ring = hex_ring(self.layout, selected, 1);
        let range_hexes = hex_spiral(self.layout, selected, self.range);

        for dr in -margin_rows..margin_rows {
            for dc in -margin_cols..margin_cols {
                let tile = Tile::new(top_left_tile.col + dc, top_left_tile.row + dr);
                let data = self.tile_at(tile);

                let cell = self.layout.tile_to_cell(tile);
                let (sx, sy) = (
                    i32::from(area.left()) + cell.x - self.origin.x,
                    i32::from(area.top()) + cell.y - self.origin.y,
                );
                if sx + pitch_x < i32::from(area.left())
                    || sx > i32::from(area.right())
                    || sy + pitch_y < i32::from(area.top())
                    || sy > i32::from(area.bottom())
                {
                    continue;
                }

                let highlight = if tile == selected {
                    0.65
                } else if ring.contains(&tile) {
                    0.30
                } else if range_hexes.contains(&tile) {
                    0.12
                } else {
                    0.0
                };
                self.draw_hex(term, area, sx, sy, tile, data, highlight, pulse.max(0.35));
            }
        }
    }

    fn status(&mut self) -> String {
        let hex = self.layout.to_hex(self.selected);
        let data = self.tile_at(self.selected);
        let orientation = match self.layout.orientation {
            tilekit::geom::HexOrientation::Pointy => "pointy-top (odd-r)",
            tilekit::geom::HexOrientation::Flat => "flat-top (odd-q)",
        };
        format!(
            "axial (q{}, r{})  offset ({}, {})  {}  range {}  {orientation}  seed {}",
            hex.q,
            hex.r,
            self.selected.col,
            self.selected.row,
            data.biome.name(),
            self.range,
            self.world.seed(),
        )
    }
}

/// Darkens or lightens a face color based on where in the footprint a cell
/// sits, so a solid fill reads as a raised, beveled tile instead of a flat
/// rectangle. Same northwest-light convention as the terrain hillshade.
fn bevel(face: Color, dx: i32, dy: i32, w: i32, h: i32) -> Color {
    if dy == 0 {
        scale(face, 1.22)
    } else if dy == h - 1 {
        scale(face, 0.68)
    } else if dx == 0 {
        scale(face, 1.08)
    } else if dx == w - 1 {
        scale(face, 0.86)
    } else {
        face
    }
}

/// [`Terminal::put_styled`] with clipping to `area`, since hexes at the map
/// edge legitimately hang partly outside the viewport.
fn put_clipped<B: Backend>(
    term: &mut Terminal<B>,
    area: Rect,
    x: i32,
    y: i32,
    glyph: char,
    style: Style,
) {
    if x >= i32::from(area.left())
        && x < i32::from(area.right())
        && y >= i32::from(area.top())
        && y < i32::from(area.bottom())
    {
        term.put_styled(x as u16, y as u16, glyph, style);
    }
}

/// Foreground color to draw a biome's glyph in, against its own bevelled
/// face: a plain white/black split by whether the face reads as light or
/// dark overall, so the glyph never washes out.
trait GlyphFg {
    fn glyph_fg(self) -> Color;
}

impl GlyphFg for Biome {
    fn glyph_fg(self) -> Color {
        if matches!(self, Self::Ice | Self::Peak | Self::Tundra | Self::Coast) {
            palette::rgb(40, 40, 48)
        } else {
            palette::rgb(240, 240, 236)
        }
    }
}

impl Demo for HexTiles {
    const NAME: &'static str = "07_hex_tiles";
    const TITLE: &'static str = "07 Hex tiles";
    const BLURB: &'static str =
        "Strategic hexes in both orientations, via hexal's axial coordinates.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("click", "select"),
            ("T", "pointy/flat"),
            ("+/-", "range"),
            ("R", "reroll"),
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

ascii_tile_demos::demo_main!(HexTiles);
