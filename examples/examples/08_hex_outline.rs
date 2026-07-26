//! 08: Hex outline -- a visible drawn honeycomb, Endless Legend / Civ VI
//! style, instead of a colored fill.
//!
//! [`07_hex_tiles`] draws a hex as a beveled block of color; this demo draws
//! the hex's actual boundary as glyphs, so the honeycomb itself is the
//! visible structure rather than an implied grid between colored blobs. Each
//! hex draws its own complete outline every frame. That sounds like it should
//! double every shared edge, but it does not: two adjacent hexes agree
//! exactly on which screen cells their shared edge occupies and what glyph
//! belongs there (the geometry is the same lattice from either side), so the
//! second hex to draw a seam simply repaints the same glyph in the same color
//! over itself. The one thing that would break this is two hexes disagreeing
//! about a seam's color, which is why the outline color here is a function of
//! *screen position*, not of either hex's own fill -- a boundary is one
//! object, not two overlapping opinions about where it is.
//!
//! Territory borders are drawn as a second pass over the base grid, along
//! whichever edges have a neighbor in a different province. Same idempotence
//! argument applies: both hexes on a border agree it exists and agree what
//! color to paint it.
//!
//! Techniques on show:
//!
//! - **Exact hex tessellation** ([`tilekit::geom::HexLayout::tile_to_cell`]):
//!   a pointy-top hex on this module's cell pitch decomposes into a tapered
//!   top row, a full-width middle row, and a tapered bottom row, with
//!   consecutive tile rows sharing their taper rows -- see the layout's own
//!   doc comment for why that sharing is what makes the tiling exact. See
//!   Amit Patel on [hex geometry](https://www.redblobgames.com/grids/hexagons/#basics).
//! - **hexal-driven neighbor lookup** ([`tilekit::geom::HexLayout::neighbors`]):
//!   province-border detection is "does my neighbor in direction D belong to a
//!   different province", answered once per direction with no offset-space
//!   arithmetic.
//! - **Alternate edge glyph sets**: ASCII slashes, single-line box-drawing,
//!   and heavy box-drawing over the same geometry, so the legibility/weight
//!   tradeoff between them is visible on the same map.
//!
//! ```sh
//! cargo run --example 08_hex_outline --features crossterm
//! cargo run --example 08_hex_outline --features software
//! cargo run --example 08_hex_outline --features gl
//! cargo run --example 08_hex_outline  # headless, prints a few frames
//! ```

use std::collections::HashMap;

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::geom::{Cell, HexLayout, Tile};
use tilekit::palette::{self, mix};
use tilekit::world::{Biome, World};

/// World size in cells, aggregated at [`HEX_CELLS`] scale.
const WORLD_W: i32 = 220;
/// See [`WORLD_W`].
const WORLD_H: i32 = 150;

/// World cells aggregated per hex, along each axis of its bounding box.
const HEX_CELLS: i32 = 5;

/// Which glyph set draws hex edges. `E` cycles through these; the point is
/// making the legibility-versus-weight tradeoff between them visible on the
/// same map rather than asserted in prose.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EdgeStyle {
    /// `/ \ _` -- the classic hand-drawn-map look, universal in any font.
    Ascii,
    /// Unicode box-drawing diagonals and a light horizontal rule. Crisper
    /// than ASCII; needs a font with box-drawing coverage, which is nearly
    /// every terminal and UI font shipped today.
    Box,
    /// The same shapes in heavier strokes, so a style change alone (no color
    /// change) can carry emphasis -- this is what the province-border pass
    /// borrows for its own heavier line.
    Heavy,
}

impl EdgeStyle {
    const fn next(self) -> Self {
        match self {
            Self::Ascii => Self::Box,
            Self::Box => Self::Heavy,
            Self::Heavy => Self::Ascii,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::Box => "box",
            Self::Heavy => "heavy",
        }
    }

    /// `(rising-diagonal, falling-diagonal, horizontal)` glyphs: `/`-like,
    /// `\`-like, and `-`-like respectively.
    const fn glyphs(self) -> (char, char, char) {
        match self {
            Self::Ascii => ('/', '\\', '_'),
            Self::Box => ('\u{2571}', '\u{2572}', '\u{2500}'),
            Self::Heavy => ('\u{2571}', '\u{2572}', '\u{2501}'),
        }
    }
}

/// One aggregated hex: dominant biome and majority province.
#[derive(Clone, Copy)]
struct HexTile {
    biome: Biome,
    province: usize,
}

/// Aggregates a `HEX_CELLS`-square block of world cells by majority vote,
/// independently for biome and province -- they are unrelated questions, and
/// combining them into one vote would credit a cell's biome to the wrong
/// tally whenever the two happen to disagree in ranking within the block.
fn aggregate(world: &World, origin_x: i32, origin_y: i32) -> HexTile {
    let mut biome_votes: HashMap<Biome, u32> = HashMap::new();
    let mut province_votes: HashMap<usize, u32> = HashMap::new();
    for dy in 0..HEX_CELLS {
        for dx in 0..HEX_CELLS {
            let (x, y) = (origin_x + dx, origin_y + dy);
            *biome_votes.entry(world.biome_at(x, y)).or_insert(0) += 1;
            *province_votes.entry(world.province_at(x, y)).or_insert(0) += 1;
        }
    }
    // Ties break on the value's own ordering, not on iteration order.
    // `HashMap` iteration is randomized per process, so `max_by_key(count)`
    // alone silently returns a different winner run to run wherever two
    // candidates tie, which makes the whole map non-reproducible from its
    // seed.
    HexTile {
        biome: biome_votes
            .into_iter()
            .max_by_key(|&(biome, count)| (count, core::cmp::Reverse(biome)))
            .map_or(Biome::Ocean, |(b, _)| b),
        province: province_votes
            .into_iter()
            .max_by_key(|&(province, count)| (count, core::cmp::Reverse(province)))
            .map_or(0, |(p, _)| p),
    }
}

/// State: world, camera, edge style, and the selected hex.
pub struct HexOutline {
    world: World,
    layout: HexLayout,
    origin: Cell,
    selected: Tile,
    style: EdgeStyle,
    show_borders: bool,
    time: f32,
    fps: FpsMeter,
}

impl Default for HexOutline {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 5);
        let layout = HexLayout::POINTY_LARGE;
        let (sx, sy) = world.start_position();
        let selected = layout.cell_to_tile(Cell::new(sx, sy));
        let mut demo = Self {
            world,
            layout,
            origin: Cell::new(0, 0),
            selected,
            style: EdgeStyle::Box,
            show_borders: true,
            time: 0.0,
            fps: FpsMeter::new(),
        };
        demo.center_on(selected);
        demo
    }
}

impl HexOutline {
    fn center_on(&mut self, tile: Tile) {
        let center = self.layout.center_cell(tile);
        self.origin = Cell::new(
            center.x - i32::from(GRID_COLS) / 2,
            center.y - i32::from(GRID_ROWS) / 2,
        );
    }

    fn tile_at(&self, tile: Tile) -> HexTile {
        let cell = self.layout.tile_to_cell(tile);
        let origin_x = cell.x.div_euclid(self.layout.pitch_x) * HEX_CELLS;
        let origin_y = cell.y.div_euclid(self.layout.pitch_y) * HEX_CELLS;
        aggregate(&self.world, origin_x, origin_y)
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
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
                Event::Key(key) if key.is_down() => match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => {
                        self.origin = self.origin.offset(0, -6);
                    }
                    KeyCode::Down | KeyCode::Char('s' | 'S') => {
                        self.origin = self.origin.offset(0, 6);
                    }
                    KeyCode::Left | KeyCode::Char('a' | 'A') => {
                        self.origin = self.origin.offset(-6, 0);
                    }
                    KeyCode::Right | KeyCode::Char('d' | 'D') => {
                        self.origin = self.origin.offset(6, 0);
                    }
                    KeyCode::Char('e' | 'E') => self.style = self.style.next(),
                    KeyCode::Char('b' | 'B') => self.show_borders = !self.show_borders,
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                },
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: retroglyph_core::Pos) {
        if kind == MouseEventKind::Down(MouseButton::Left) {
            let screen = Cell::new(i32::from(pos.x), i32::from(pos.y) - 1);
            let world_cell = screen.offset(self.origin.x, self.origin.y);
            self.selected = self.layout.cell_to_tile(world_cell);
        }
    }

    /// Draws one hex's fill and its complete boundary outline.
    ///
    /// Layout: [`HexLayout::POINTY_LARGE`] is 12 cells wide by 4 tall, with a
    /// quarter-width taper (3 cells) on the top and bottom rows. That gives
    /// four screen rows of *shape*, top to bottom: a rising taper (NW edge on
    /// the left half, NE edge on the right half), the wide middle band twice
    /// (W/E vertical edges), and a falling taper (SW/SE). The two middle rows
    /// are both full width with vertical edges at the far left and right
    /// column; drawing the taper on rows 0 and 3 and verticals through rows
    /// 1..3 produces a closed hexagonal boundary with no gaps.
    fn draw_hex(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        sx: i32,
        sy: i32,
        data: &HexTile,
        emphasis: f32,
    ) {
        let (w, h) = (self.layout.pitch_x, self.layout.pitch_y);
        let taper = w / 4;
        let fill = mix(data.biome.color(), ui::BG, 0.55);
        let fill = if emphasis > 0.0 {
            mix(fill, palette::rgb(255, 236, 170), emphasis)
        } else {
            fill
        };

        for dy in 0..h {
            let (lo, hi) = row_span(dy, h, taper, w);
            for dx in lo..hi {
                put_clipped(surface, area, sx + dx, sy + dy, ' ', Style::new().bg(fill));
            }
        }

        let glyph_fg = if matches!(
            data.biome,
            Biome::Ocean | Biome::Sea | Biome::Lake | Biome::Peak | Biome::Ice
        ) {
            palette::rgb(230, 236, 244)
        } else {
            palette::rgb(24, 26, 32)
        };
        put_clipped(
            surface,
            area,
            sx + w / 2,
            sy + h / 2,
            data.biome.glyph(),
            Style::new().fg(glyph_fg).bg(fill),
        );

        let (up, down, horiz) = self.style.glyphs();
        // The lattice is drawn *lighter* than the terrain it sits on, not
        // darker. A darker line disappears into the dark end of the biome
        // palette (ocean, taiga, jungle), which is precisely where the grid is
        // most needed and where this demo's whole subject matter lives. A
        // light line reads against every biome because the fills are already
        // pulled halfway toward the page background.
        let line_color = mix(fill, palette::WHITE, 0.42);
        let edge = Style::new().fg(line_color).bg(fill);

        // Top taper: rises from the west corner (row 0, col `taper`) up to
        // the NW peak, then the mirrored fall on the right half to the NE
        // peak. A pointy-top hex's top is a shallow peak, not a flat edge, so
        // both diagonals meet at one cell in the middle of the top row.
        for dx in 0..taper {
            put_clipped(surface, area, sx + taper - 1 - dx, sy, up, edge);
            put_clipped(surface, area, sx + w - taper + dx, sy, down, edge);
        }
        // Bottom taper mirrors the top with the diagonals swapped, since a
        // falling-left edge on top is a rising-left edge on the bottom.
        for dx in 0..taper {
            put_clipped(surface, area, sx + taper - 1 - dx, sy + h - 1, down, edge);
            put_clipped(surface, area, sx + w - taper + dx, sy + h - 1, up, edge);
        }
        // Vertical west/east edges through the two middle rows.
        for dy in 1..h - 1 {
            put_clipped(surface, area, sx, sy + dy, '|', edge);
            put_clipped(surface, area, sx + w - 1, sy + dy, '|', edge);
        }
        // A short horizontal cap where the taper meets the middle band on
        // each side, closing the hexagon's shoulder rather than leaving a
        // one-cell notch between the diagonal and the vertical.
        put_clipped(surface, area, sx, sy, horiz, edge);
        put_clipped(surface, area, sx + w - 1, sy, horiz, edge);
        put_clipped(surface, area, sx, sy + h - 1, horiz, edge);
        put_clipped(surface, area, sx + w - 1, sy + h - 1, horiz, edge);
    }

    /// Draws heavier edges over [`draw_hex`]'s base grid wherever a neighbor
    /// in that direction belongs to a different province.
    fn draw_province_borders(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        sx: i32,
        sy: i32,
        tile: Tile,
        data: &HexTile,
    ) {
        let (w, h) = (self.layout.pitch_x, self.layout.pitch_y);
        let taper = w / 4;
        let border = palette::rgb(250, 214, 120);
        let heavy = Style::new().fg(border).bg(border);

        // hexal::Direction::ALL order: E, NE, NW, W, SW, SE.
        let neighbors = self.layout.neighbors(tile);
        let differs = |i: usize| self.tile_at(neighbors[i]).province != data.province;

        if differs(3) {
            for dy in 1..h - 1 {
                put_clipped(surface, area, sx, sy + dy, '\u{2588}', heavy);
            }
        }
        if differs(0) {
            for dy in 1..h - 1 {
                put_clipped(surface, area, sx + w - 1, sy + dy, '\u{2588}', heavy);
            }
        }
        if differs(2) {
            for dx in 0..taper {
                put_clipped(surface, area, sx + taper - 1 - dx, sy, '\u{2588}', heavy);
            }
        }
        if differs(1) {
            for dx in 0..taper {
                put_clipped(surface, area, sx + w - taper + dx, sy, '\u{2588}', heavy);
            }
        }
        if differs(4) {
            for dx in 0..taper {
                put_clipped(
                    surface,
                    area,
                    sx + taper - 1 - dx,
                    sy + h - 1,
                    '\u{2588}',
                    heavy,
                );
            }
        }
        if differs(5) {
            for dx in 0..taper {
                put_clipped(
                    surface,
                    area,
                    sx + w - taper + dx,
                    sy + h - 1,
                    '\u{2588}',
                    heavy,
                );
            }
        }
    }

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let (w, h) = (self.layout.pitch_x, self.layout.pitch_y);
        let margin_cols = i32::from(area.width()) / w + 2;
        let margin_rows = i32::from(area.height()) / h + 2;
        let top_left_tile = self.layout.cell_to_tile(self.origin);
        let pulse = (self.time * 3.0).sin().mul_add(0.5, 0.5).max(0.35);

        // Two passes: every hex's base fill and outline first, then every
        // border overlay. A single combined pass would let one hex's plain
        // outline draw *after* its neighbor's province-border overlay and
        // silently erase it.
        let mut visible = Vec::new();
        for dr in -margin_rows..margin_rows {
            for dc in -margin_cols..margin_cols {
                let tile = Tile::new(top_left_tile.col + dc, top_left_tile.row + dr);
                let cell = self.layout.tile_to_cell(tile);
                let (sx, sy) = (
                    i32::from(area.left()) + cell.x - self.origin.x,
                    i32::from(area.top()) + cell.y - self.origin.y,
                );
                if sx + w < i32::from(area.left())
                    || sx > i32::from(area.right())
                    || sy + h < i32::from(area.top())
                    || sy > i32::from(area.bottom())
                {
                    continue;
                }
                visible.push((tile, sx, sy));
            }
        }

        for &(tile, sx, sy) in &visible {
            let data = self.tile_at(tile);
            let emphasis = if tile == self.selected {
                0.55 * pulse
            } else {
                0.0
            };
            self.draw_hex(surface, area, sx, sy, &data, emphasis);
        }
        if self.show_borders {
            for &(tile, sx, sy) in &visible {
                let data = self.tile_at(tile);
                self.draw_province_borders(surface, area, sx, sy, tile, &data);
            }
        }
    }

    fn status(&self) -> String {
        let data = self.tile_at(self.selected);
        format!(
            "({}, {})  {}  province {}  edges: {}  borders {}  seed {}",
            self.selected.col,
            self.selected.row,
            data.biome.name(),
            data.province,
            self.style.label(),
            if self.show_borders { "on" } else { "off" },
            self.world.seed(),
        )
    }
}

/// The `(lo, hi)` column range of hex-interior cells in screen row `dy` of an
/// `h`-tall, `w`-wide, `taper`-tapered pointy-top hex footprint.
const fn row_span(dy: i32, h: i32, taper: i32, w: i32) -> (i32, i32) {
    if dy == 0 || dy == h - 1 {
        (taper, w - taper)
    } else {
        (1, w - 1)
    }
}

/// [`Terminal::put_styled`] with clipping to `area`.
fn put_clipped(surface: &mut Surface<'_>, area: Rect, x: i32, y: i32, glyph: char, style: Style) {
    if x >= i32::from(area.left())
        && x < i32::from(area.right())
        && y >= i32::from(area.top())
        && y < i32::from(area.bottom())
    {
        surface.put((x as u16, y as u16), glyph, style);
    }
}

impl Demo for HexOutline {
    const NAME: &'static str = "08_hex_outline";
    const TITLE: &'static str = "08 Hex outline";
    const BLURB: &'static str = "A drawn honeycomb with province borders, Endless Legend style.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("click", "select"),
            ("E", "edge style"),
            ("B", "borders"),
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

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_map(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(HexOutline);
