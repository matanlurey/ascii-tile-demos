//! Tile geometry: square, isometric, and hex projections between map
//! coordinates and character-cell coordinates, in both directions.
//!
//! Every projection here is a pair of functions, `tile -> cell` for drawing
//! and `cell -> tile` for picking, plus the bookkeeping that makes them exact
//! inverses. Getting picking right is what separates a demo you can *use* from
//! a demo you can only look at, and it is also the part that is easy to get
//! subtly wrong: a `floor` where a `round` belongs shifts every hit by half a
//! tile, which nobody notices until they click near an edge.
//!
//! ## The character-cell aspect problem
//!
//! Terminal cells are not square. A typical monospace cell is about twice as
//! tall as it is wide, so a tile that is `N` cells wide and `N` cells tall
//! renders as a tall rectangle, not a square. Every layout constant in this
//! module is therefore chosen in *cells*, with the 1:2 aspect already
//! accounted for: [`SquareLayout::CHUNKY`] is 8x4 because 8 cells wide by 4
//! cells tall is visually square, and the isometric layouts are 4x1 and 8x2
//! rather than the 2:1 pixel ratio the textbooks give.
//!
//! ## References
//!
//! - Hex coordinates, layout, rounding: Amit Patel, [Hexagonal Grids](https://www.redblobgames.com/grids/hexagons/)
//! - Isometric projection and picking: [Isometric grid math](https://gamedevmath.com/isometric-grid/)

use hexal::{EvenQ, Hex, HexI, OddQ, OddR, OffsetHex};

/// A point in screen-cell space, signed so that tiles partly off the top or
/// left edge of the viewport still have a well-defined position.
///
/// Signed is not a detail: with `u16` cells, a tile scrolled half off the left
/// edge has to be either clamped (wrong: it would render squashed against the
/// edge) or skipped (wrong: it would pop in and out). Computing in `i32` and
/// clipping at the last moment is what makes smooth scrolling possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    /// Column, in character cells.
    pub x: i32,
    /// Row, in character cells.
    pub y: i32,
}

impl Cell {
    /// A cell at `(x, y)`.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Translates by `(dx, dy)`.
    #[must_use]
    pub const fn offset(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy)
    }
}

/// An integer tile coordinate on the map.
///
/// For square and isometric layouts this is a straightforward `(col, row)`.
/// For hexes it is offset coordinates, which [`HexLayout`] converts to and
/// from `hexal`'s axial [`Hex`] as needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub struct Tile {
    /// Column.
    pub col: i32,
    /// Row.
    pub row: i32,
}

impl Tile {
    /// A tile at `(col, row)`.
    #[must_use]
    pub const fn new(col: i32, row: i32) -> Self {
        Self { col, row }
    }
}

// ── Square ──────────────────────────────────────────────────────────────────

/// An axis-aligned grid of `w` x `h` cell tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SquareLayout {
    /// Tile width, in character cells.
    pub w: i32,
    /// Tile height, in character cells.
    pub h: i32,
}

impl SquareLayout {
    /// One tile per character cell: the classic terminal roguelike map.
    pub const FINE: Self = Self { w: 1, h: 1 };
    /// 8x4 cells per tile, which is roughly square on screen at the usual 1:2
    /// character aspect. Big enough for a terrain glyph, a unit marker, and an
    /// edge bevel: the "board game counter" tile of Civ or Age of Wonders.
    pub const CHUNKY: Self = Self { w: 8, h: 4 };
    /// 4x2 cells: half of [`CHUNKY`](Self::CHUNKY), for a zoomed-out view that
    /// still has room for one glyph plus a border.
    pub const MEDIUM: Self = Self { w: 4, h: 2 };

    /// A layout with the given tile size in cells.
    ///
    /// # Panics
    ///
    /// Panics if either dimension is not positive; a zero-size tile would make
    /// [`cell_to_tile`](Self::cell_to_tile) divide by zero and every tile land
    /// on top of every other.
    #[must_use]
    pub const fn new(w: i32, h: i32) -> Self {
        assert!(w > 0 && h > 0, "tile size must be positive");
        Self { w, h }
    }

    /// Top-left cell of `tile`, relative to the tile grid's origin.
    #[must_use]
    pub const fn tile_to_cell(self, tile: Tile) -> Cell {
        Cell::new(tile.col * self.w, tile.row * self.h)
    }

    /// The tile containing `cell`.
    ///
    /// Uses Euclidean division, so cells at negative coordinates (above or
    /// left of the origin) map to the tile that visually contains them rather
    /// than rounding toward zero and folding two tile rows into one.
    #[must_use]
    pub const fn cell_to_tile(self, cell: Cell) -> Tile {
        Tile::new(cell.x.div_euclid(self.w), cell.y.div_euclid(self.h))
    }

    /// Position of `cell` within its tile, as `(dx, dy)` in `0..w`, `0..h`.
    ///
    /// This is what a renderer switches on to decide whether a given cell is
    /// tile interior, a bevel edge, or a corner.
    #[must_use]
    pub const fn cell_within(self, cell: Cell) -> (i32, i32) {
        (cell.x.rem_euclid(self.w), cell.y.rem_euclid(self.h))
    }

    /// How many whole tiles fit in a `cols` x `rows` viewport.
    #[must_use]
    pub const fn tiles_visible(self, cols: u16, rows: u16) -> (i32, i32) {
        (cols as i32 / self.w, rows as i32 / self.h)
    }
}

// ── Isometric ───────────────────────────────────────────────────────────────

/// A diamond (rotated-square) isometric grid.
///
/// Tiles are diamonds `2 * half_w` cells wide and `2 * half_h` cells tall,
/// laid out so that moving one tile along `+col` goes right-and-down on screen
/// and one tile along `+row` goes left-and-down. Half-extents rather than full
/// width and height because every formula in the projection uses the halves,
/// and storing them directly keeps the odd/even parity exact instead of
/// re-deriving it with a division that may truncate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoLayout {
    /// Half the diamond's width, in cells.
    pub half_w: i32,
    /// Half the diamond's height, in cells.
    pub half_h: i32,
}

impl IsoLayout {
    /// 8x2 cells per diamond: the standard, readable isometric tile. Wide
    /// enough for a terrain glyph plus a decoration on the diamond's spine.
    pub const STANDARD: Self = Self {
        half_w: 4,
        half_h: 1,
    };
    /// 4x2 cells per diamond, for dense maps. One glyph per tile at most.
    pub const SMALL: Self = Self {
        half_w: 2,
        half_h: 1,
    };
    /// 16x4 cells per diamond: a chunky, close-up view with room for a full
    /// face, a bevel, and an elevation skirt.
    pub const LARGE: Self = Self {
        half_w: 8,
        half_h: 2,
    };

    /// A layout with the given half-extents.
    ///
    /// # Panics
    ///
    /// Panics if either half-extent is not positive.
    #[must_use]
    pub const fn new(half_w: i32, half_h: i32) -> Self {
        assert!(half_w > 0 && half_h > 0, "half extents must be positive");
        Self { half_w, half_h }
    }

    /// Full diamond width in cells.
    #[must_use]
    pub const fn width(self) -> i32 {
        self.half_w * 2
    }

    /// Full diamond height in cells.
    #[must_use]
    pub const fn height(self) -> i32 {
        self.half_h * 2
    }

    /// Center cell of `tile`, before any elevation offset.
    ///
    /// The standard dimetric transform: `x = (col - row) * half_w`,
    /// `y = (col + row) * half_h`. The difference term rotates the grid 45
    /// degrees; the sum term is also the tile's depth, which is why
    /// [`depth`](Self::depth) is just `col + row`.
    #[must_use]
    pub const fn tile_to_cell(self, tile: Tile) -> Cell {
        Cell::new(
            (tile.col - tile.row) * self.half_w,
            (tile.col + tile.row) * self.half_h,
        )
    }

    /// Center cell of `tile` raised by `elevation` levels.
    ///
    /// Raising a tile on screen is the entire elevation illusion: nothing
    /// about the tile changes except that it is drawn `elevation *
    /// cells_per_level` rows higher, and the painter's-algorithm ordering from
    /// [`depth`](Self::depth) makes the tile in front of it overlap its base,
    /// which the eye reads as height.
    #[must_use]
    pub const fn tile_to_cell_elevated(self, tile: Tile, elevation: i32, per_level: i32) -> Cell {
        let base = self.tile_to_cell(tile);
        Cell::new(base.x, base.y - elevation * per_level)
    }

    /// Painter's-algorithm sort key: draw tiles in ascending order.
    ///
    /// `col + row` is constant along each screen row of diamonds and increases
    /// toward the viewer, so ascending order draws back-to-front and lets near
    /// tiles overlap far ones. Elevation deliberately does *not* enter the key:
    /// a tall tile still belongs to its own map position in the draw order, and
    /// mixing height into the sort is the classic way to make a mountain
    /// occlude something that is actually in front of it.
    ///
    /// See Brendan Sechter on [draw order](https://sgeos.github.io/games/graphics/projection/2026/04/30/draw_order_y_sort_z_sort_and_painters_algorithm.html).
    #[must_use]
    pub const fn depth(tile: Tile) -> i32 {
        tile.col + tile.row
    }

    /// The tile whose diamond contains `cell`.
    ///
    /// Inverts [`tile_to_cell`](Self::tile_to_cell). The division is done in
    /// doubled integer space and then floored, rather than in floating point,
    /// so picking is exact at every diamond edge instead of depending on which
    /// side of a rounding boundary a float lands.
    ///
    /// Derivation: from `x = (c - r) * hw` and `y = (c + r) * hh`,
    /// `c = (x/hw + y/hh) / 2` and `r = (y/hh - x/hw) / 2`. Multiplying through
    /// by `2 * hw * hh` gives `c = (x*hh + y*hw) / (2*hw*hh)`, all integers.
    #[must_use]
    pub const fn cell_to_tile(self, cell: Cell) -> Tile {
        let denom = 2 * self.half_w * self.half_h;
        let col_num = cell.x * self.half_h + cell.y * self.half_w;
        let row_num = cell.y * self.half_w - cell.x * self.half_h;
        Tile::new(col_num.div_euclid(denom), row_num.div_euclid(denom))
    }

    /// Half-width of the diamond at vertical offset `dy` from its center, or
    /// `None` if that row is entirely outside the diamond.
    ///
    /// The diamond is the set of points satisfying
    /// `|dx| / half_w + |dy| / half_h <= 1`; solving for `|dx|` gives
    /// `half_w * (half_h - |dy|) / half_h`. Renderers walk `dy` over the
    /// tile's height and fill this many cells either side of the spine, so a
    /// tile is drawn with no per-cell inside test at all.
    ///
    /// The tip rows (`|dy| == half_h`) return `Some(0)`, meaning one cell on
    /// the spine. Consecutive diamond rows share those tip rows, which is
    /// what makes the layout tile the plane with no gaps: tile `(c, r)`'s
    /// bottom tip row is also tile `(c, r+1)`'s and `(c+1, r+1)`'s top tip
    /// row, and the three tips are horizontally disjoint.
    #[must_use]
    pub const fn span_at(self, dy: i32) -> Option<i32> {
        let from_center = if dy < 0 { -dy } else { dy };
        if from_center > self.half_h {
            return None;
        }
        Some(self.half_w * (self.half_h - from_center) / self.half_h)
    }

    /// Whether `(dx, dy)` relative to a tile's center is inside its diamond.
    #[must_use]
    pub const fn contains(self, dx: i32, dy: i32) -> bool {
        let adx = if dx < 0 { -dx } else { dx };
        match self.span_at(dy) {
            Some(span) => adx <= span,
            None => false,
        }
    }
}

/// A staggered ("2.5D") isometric grid: rows of diamonds where odd rows are
/// shifted half a tile right, filling a rectangular map area.
///
/// The tradeoff versus [`IsoLayout`]: a diamond layout gives clean coordinate
/// math but a diamond-shaped map, wasting the screen's corners. Staggering
/// keeps a rectangular map at the cost of a parity term in every formula. For
/// an overland strategy map that fills the window, rectangular usually wins.
///
/// See [Tiled's staggered renderer](https://github.com/mapeditor/tiled/blob/master/src/libtiled/staggeredrenderer.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaggeredLayout {
    /// Tile width, in cells.
    pub w: i32,
    /// Vertical pitch between rows, in cells. Half the tile's visual height,
    /// since staggered rows interlock.
    pub row_h: i32,
}

impl StaggeredLayout {
    /// 8-wide tiles on a 2-cell row pitch.
    pub const STANDARD: Self = Self { w: 8, row_h: 2 };
    /// 4-wide tiles on a 1-cell row pitch, for dense maps.
    pub const SMALL: Self = Self { w: 4, row_h: 1 };

    /// A layout with the given tile width and row pitch.
    ///
    /// # Panics
    ///
    /// Panics if either dimension is not positive, or if `w` is odd: a
    /// staggered row shifts by exactly `w / 2`, and an odd width would make
    /// that shift asymmetric, so alternate rows would not line up.
    #[must_use]
    pub const fn new(w: i32, row_h: i32) -> Self {
        assert!(w > 0 && row_h > 0, "tile size must be positive");
        assert!(w % 2 == 0, "staggered tile width must be even");
        Self { w, row_h }
    }

    /// Horizontal shift of `row`: half a tile on odd rows, zero on even.
    ///
    /// `rem_euclid` rather than `%` so rows above the origin stagger
    /// consistently instead of flipping parity at zero.
    #[must_use]
    pub const fn stagger(self, row: i32) -> i32 {
        if row.rem_euclid(2) == 1 {
            self.w / 2
        } else {
            0
        }
    }

    /// Top-left cell of `tile`.
    #[must_use]
    pub const fn tile_to_cell(self, tile: Tile) -> Cell {
        Cell::new(
            tile.col * self.w + self.stagger(tile.row),
            tile.row * self.row_h,
        )
    }

    /// The tile containing `cell`.
    ///
    /// Rows are unambiguous (each cell row belongs to exactly one tile row),
    /// so only the column needs the stagger backed out.
    #[must_use]
    pub const fn cell_to_tile(self, cell: Cell) -> Tile {
        let row = cell.y.div_euclid(self.row_h);
        let col = (cell.x - self.stagger(row)).div_euclid(self.w);
        Tile::new(col, row)
    }
}

// ── Hex ─────────────────────────────────────────────────────────────────────

/// Hex orientation.
///
/// The choice is not just cosmetic in a character grid: pointy-top hexes tile
/// with a *horizontal* row pitch that matches how text flows, so they pack
/// tightly and read well; flat-top hexes stagger *columns* instead, which
/// costs vertical space but gives each hex a flat top and bottom edge that
/// box-drawing characters can render crisply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexOrientation {
    /// Point up. Rows run horizontally, odd rows shifted right (`odd-r`).
    Pointy,
    /// Flat edge up. Columns run vertically, odd columns shifted down
    /// (`odd-q`).
    Flat,
}

/// A hex grid laid out in character cells.
///
/// Layout is in *cells*, not the abstract "size" of Red Blob Games' formulas,
/// because a character grid cannot render a hex at an arbitrary radius: the
/// hex has to snap to whole cells or its edges alias into a staircase that
/// changes shape from row to row. So this stores the exact cell pitch and
/// derives everything from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexLayout {
    /// Which way up the hexes sit.
    pub orientation: HexOrientation,
    /// Horizontal distance between adjacent hex centers in the same row
    /// (pointy) or between columns (flat), in cells.
    pub pitch_x: i32,
    /// Vertical distance between hex rows (pointy) or between adjacent hexes
    /// in a column (flat), in cells.
    pub pitch_y: i32,
}

impl HexLayout {
    /// Pointy-top hexes on an 8x2 cell pitch: the densest layout where a hex
    /// still has a distinct interior. Each hex is three cell rows tall (a
    /// taper row, a full-width middle row, another taper) and consecutive rows
    /// share their taper rows, which is why the pitch is 2 rather than 3.
    pub const POINTY: Self = Self {
        orientation: HexOrientation::Pointy,
        pitch_x: 8,
        pitch_y: 2,
    };
    /// Flat-top hexes on a 6x4 cell pitch. The column stagger is half of
    /// `pitch_y`.
    pub const FLAT: Self = Self {
        orientation: HexOrientation::Flat,
        pitch_x: 6,
        pitch_y: 4,
    };
    /// Pointy-top hexes at double [`POINTY`](Self::POINTY)'s scale, with room
    /// for a drawn outline rather than just a colored fill.
    pub const POINTY_LARGE: Self = Self {
        orientation: HexOrientation::Pointy,
        pitch_x: 12,
        pitch_y: 4,
    };

    /// A layout with the given orientation and cell pitch.
    ///
    /// # Panics
    ///
    /// Panics if either pitch is not positive, or if the staggered axis'
    /// pitch is odd (the stagger is exactly half of it, and an odd pitch would
    /// make alternate rows or columns misalign by a cell).
    #[must_use]
    pub const fn new(orientation: HexOrientation, pitch_x: i32, pitch_y: i32) -> Self {
        assert!(pitch_x > 0 && pitch_y > 0, "pitch must be positive");
        match orientation {
            HexOrientation::Pointy => assert!(pitch_x % 2 == 0, "pointy pitch_x must be even"),
            HexOrientation::Flat => assert!(pitch_y % 2 == 0, "flat pitch_y must be even"),
        }
        Self {
            orientation,
            pitch_x,
            pitch_y,
        }
    }

    /// Stagger offset for a given offset coordinate.
    ///
    /// Pointy: odd *rows* shift right by half [`pitch_x`](Self::pitch_x).
    /// Flat: odd *columns* shift down by half [`pitch_y`](Self::pitch_y).
    #[must_use]
    pub const fn stagger(self, tile: Tile) -> (i32, i32) {
        match self.orientation {
            HexOrientation::Pointy => {
                if tile.row.rem_euclid(2) == 1 {
                    (self.pitch_x / 2, 0)
                } else {
                    (0, 0)
                }
            }
            HexOrientation::Flat => {
                if tile.col.rem_euclid(2) == 1 {
                    (0, self.pitch_y / 2)
                } else {
                    (0, 0)
                }
            }
        }
    }

    /// Top-left cell of `tile`'s bounding box.
    #[must_use]
    pub const fn tile_to_cell(self, tile: Tile) -> Cell {
        let (sx, sy) = self.stagger(tile);
        Cell::new(tile.col * self.pitch_x + sx, tile.row * self.pitch_y + sy)
    }

    /// The hex containing `cell`.
    ///
    /// Two-stage: snap to the nearest lattice point of the staggered grid,
    /// then correct across the diagonal edges, because a hex's bounding box
    /// overlaps its neighbours' at the corners. Without the correction, clicks
    /// in a hex's slanted corner would land on the wrong hex, which is exactly
    /// where users click when they aim for an edge.
    #[must_use]
    pub fn cell_to_tile(self, cell: Cell) -> Tile {
        match self.orientation {
            HexOrientation::Pointy => self.cell_to_tile_pointy(cell),
            HexOrientation::Flat => self.cell_to_tile_flat(cell),
        }
    }

    fn cell_to_tile_pointy(self, cell: Cell) -> Tile {
        // Candidate rows: the two whose bands the cell could fall in. Row
        // bands are `pitch_y` tall but hexes are taller than their pitch (they
        // interlock), so the true owner is one of two candidates and we pick
        // by center distance, corrected for the 1:2 character aspect.
        let band = cell.y.div_euclid(self.pitch_y);
        let mut best = Tile::new(0, 0);
        let mut best_d = i64::MAX;
        for row in [band - 1, band, band + 1] {
            let shift = if row.rem_euclid(2) == 1 {
                self.pitch_x / 2
            } else {
                0
            };
            let col = (cell.x - shift + self.pitch_x / 2).div_euclid(self.pitch_x);
            for col in [col - 1, col, col + 1] {
                let candidate = Tile::new(col, row);
                let center = self.center_cell(candidate);
                let d = aspect_distance_sq(cell, center);
                if d < best_d {
                    best_d = d;
                    best = candidate;
                }
            }
        }
        best
    }

    fn cell_to_tile_flat(self, cell: Cell) -> Tile {
        let band = cell.x.div_euclid(self.pitch_x);
        let mut best = Tile::new(0, 0);
        let mut best_d = i64::MAX;
        for col in [band - 1, band, band + 1] {
            let shift = if col.rem_euclid(2) == 1 {
                self.pitch_y / 2
            } else {
                0
            };
            let row = (cell.y - shift + self.pitch_y / 2).div_euclid(self.pitch_y);
            for row in [row - 1, row, row + 1] {
                let candidate = Tile::new(col, row);
                let center = self.center_cell(candidate);
                let d = aspect_distance_sq(cell, center);
                if d < best_d {
                    best_d = d;
                    best = candidate;
                }
            }
        }
        best
    }

    /// Center cell of `tile`.
    #[must_use]
    pub const fn center_cell(self, tile: Tile) -> Cell {
        let origin = self.tile_to_cell(tile);
        Cell::new(origin.x + self.pitch_x / 2, origin.y + self.pitch_y / 2)
    }

    /// Converts an offset coordinate to `hexal`'s axial representation, using
    /// the offset scheme that matches this layout's orientation.
    ///
    /// This is the bridge to every hex *algorithm*: distance, rings, lines and
    /// neighbours are all clean in axial coordinates and all miserable in
    /// offset coordinates, so the rule is store and draw in offset, reason in
    /// axial. See Red Blob Games, [Coordinate systems](https://www.redblobgames.com/grids/hexagons/#coordinates).
    #[must_use]
    pub fn to_hex(self, tile: Tile) -> HexI {
        match self.orientation {
            HexOrientation::Pointy => OffsetHex::<i32, OddR>::new(tile.col, tile.row).to_hex(),
            HexOrientation::Flat => OffsetHex::<i32, OddQ>::new(tile.col, tile.row).to_hex(),
        }
    }

    /// Converts an axial coordinate back to this layout's offset scheme.
    #[must_use]
    pub fn from_hex(self, hex: HexI) -> Tile {
        match self.orientation {
            HexOrientation::Pointy => {
                let o = hex.to_offset::<OddR>();
                Tile::new(o.col, o.row)
            }
            HexOrientation::Flat => {
                let o = hex.to_offset::<OddQ>();
                Tile::new(o.col, o.row)
            }
        }
    }

    /// Hex distance between two offset tiles, in steps.
    #[must_use]
    pub fn distance(self, a: Tile, b: Tile) -> i32 {
        self.to_hex(a).distance(self.to_hex(b))
    }

    /// The six neighbours of `tile`, in `hexal`'s direction order.
    #[must_use]
    pub fn neighbors(self, tile: Tile) -> [Tile; 6] {
        let hex = self.to_hex(tile);
        let mut out = [Tile::new(0, 0); 6];
        for (slot, dir) in out.iter_mut().zip(hexal::Direction::ALL) {
            *slot = self.from_hex(hex.neighbor(dir));
        }
        out
    }
}

/// Squared distance between two cells with the horizontal axis scaled up 2x,
/// approximating the 1:2 aspect of a character cell.
///
/// Without the correction, "nearest center" picking in a character grid is
/// biased: a cell two columns away is visually about as far as a cell one row
/// away, but naive Euclidean distance calls the row neighbour twice as close,
/// so clicks near a horizontal edge snap to the wrong hex.
const fn aspect_distance_sq(a: Cell, b: Cell) -> i64 {
    let dx = (a.x - b.x) as i64;
    let dy = ((a.y - b.y) * 2) as i64;
    dx * dx + dy * dy
}

/// Converts an offset tile to axial using the `even-q` scheme, for the one
/// case `hexal`'s `OddQ`/`OddR` don't cover directly.
///
/// Exposed because a map authored against a different tool's convention (Tiled
/// defaults to `even` stagger) would otherwise be off by one row of shifts,
/// which looks like a correct map that is subtly wrong at every other row.
#[must_use]
pub fn even_q_to_hex(tile: Tile) -> HexI {
    OffsetHex::<i32, EvenQ>::new(tile.col, tile.row).to_hex()
}

/// The axial-coordinate hex ring at `radius` around `center`, as offset tiles.
///
/// Returns an empty vector for a negative radius, and a single-element vector
/// for radius 0 (the ring of radius 0 is the center itself).
#[must_use]
pub fn hex_ring(layout: HexLayout, center: Tile, radius: i32) -> Vec<Tile> {
    if radius < 0 {
        return Vec::new();
    }
    if radius == 0 {
        return vec![center];
    }
    let hex = layout.to_hex(center);
    hex.ring(radius).map(|h| layout.from_hex(h)).collect()
}

/// Every hex within `radius` of `center`, center first, then outward by ring.
#[must_use]
pub fn hex_spiral(layout: HexLayout, center: Tile, radius: i32) -> Vec<Tile> {
    let mut out = vec![center];
    for r in 1..=radius.max(0) {
        out.extend(hex_ring(layout, center, r));
    }
    out
}

/// The hex line from `a` to `b` inclusive, as offset tiles.
///
/// Guarantees every consecutive pair is exactly one step apart and that the
/// line has `distance(a, b) + 1` entries with no repeats. That guarantee is
/// the whole reason this exists rather than calling `hexal`'s `Hex::line_to`:
/// as of hexal 0.1.1 that method returns non-contiguous lines along the `q ==
/// r` diagonal (`hex(0,0).line_to(hex(2,2))` yields `(0,0), (1,1), (1,1),
/// (2,2), (2,2)` -- a two-step jump plus repeats), which silently breaks
/// anything that walks a line looking for the first blocker, since the
/// blocker can be skipped over entirely.
///
/// Implements Red Blob Games' [line drawing](https://www.redblobgames.com/grids/hexagons/#line-drawing):
/// linearly interpolate in cube space and round each sample. The endpoints are
/// nudged by a small epsilon first, which matters more than it looks: a line
/// running exactly along a hex edge hits an unbroken run of ties, and without
/// the nudge those ties resolve inconsistently and the line zig-zags across
/// the boundary instead of committing to one side.
#[must_use]
pub fn hex_line(layout: HexLayout, from: Tile, to: Tile) -> Vec<Tile> {
    let (hex_from, hex_to) = (layout.to_hex(from), layout.to_hex(to));
    let steps = hex_from.distance(hex_to);
    if steps <= 0 {
        return vec![from];
    }

    // Nudging q and r by the same epsilon shifts the derived s axis by -2e-6,
    // which keeps q + r + s == 0 exactly; nudging one axis alone would push
    // every sample off the cube plane.
    let (from_q, from_r) = (hex_from.q as f32 + NUDGE, hex_from.r as f32 + NUDGE);
    let (to_q, to_r) = (hex_to.q as f32 + NUDGE, hex_to.r as f32 + NUDGE);

    let mut out = Vec::with_capacity(steps as usize + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let q = (to_q - from_q).mul_add(t, from_q);
        let r = (to_r - from_r).mul_add(t, from_r);
        out.push(layout.from_hex(hex_round(q, r)));
    }
    out
}

/// Tie-breaking offset for [`hex_line`]'s cube interpolation. Small enough
/// never to change which hex a sample lands in, large enough to resolve an
/// exact edge-running tie consistently in one direction.
const NUDGE: f32 = 1e-6;

/// Rounds a fractional axial coordinate to the nearest hex.
///
/// The cube-rounding algorithm: round all three cube coordinates, then discard
/// and recompute whichever moved furthest, restoring the `q + r + s = 0`
/// constraint. Rounding each axis independently would produce coordinates that
/// violate the constraint and land outside any hex.
///
/// See Red Blob Games, [Rounding to nearest hex](https://www.redblobgames.com/grids/hexagons/#rounding).
#[must_use]
pub fn hex_round(q: f32, r: f32) -> HexI {
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());

    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    Hex::new(rq as i32, rr as i32)
}

#[cfg(test)]
mod tests {
    use super::{
        Cell, HexLayout, HexOrientation, IsoLayout, SquareLayout, StaggeredLayout, Tile,
        even_q_to_hex, hex_line, hex_ring, hex_round, hex_spiral,
    };

    // ── Square ──────────────────────────────────────────────────────────────

    #[test]
    fn square_round_trips_every_tile() {
        for layout in [
            SquareLayout::FINE,
            SquareLayout::MEDIUM,
            SquareLayout::CHUNKY,
        ] {
            for col in -5..5 {
                for row in -5..5 {
                    let tile = Tile::new(col, row);
                    let cell = layout.tile_to_cell(tile);
                    assert_eq!(layout.cell_to_tile(cell), tile, "{layout:?} {tile:?}");
                }
            }
        }
    }

    #[test]
    fn square_maps_every_cell_of_a_tile_back_to_it() {
        let layout = SquareLayout::CHUNKY;
        let tile = Tile::new(2, -3);
        let origin = layout.tile_to_cell(tile);
        for dy in 0..layout.h {
            for dx in 0..layout.w {
                let cell = origin.offset(dx, dy);
                assert_eq!(layout.cell_to_tile(cell), tile, "at ({dx}, {dy})");
                assert_eq!(layout.cell_within(cell), (dx, dy));
            }
        }
    }

    #[test]
    fn square_handles_negative_cells_without_folding_rows() {
        let layout = SquareLayout::new(8, 4);
        assert_eq!(layout.cell_to_tile(Cell::new(-1, -1)), Tile::new(-1, -1));
        assert_eq!(layout.cell_to_tile(Cell::new(-8, -4)), Tile::new(-1, -1));
        assert_eq!(layout.cell_to_tile(Cell::new(-9, -5)), Tile::new(-2, -2));
    }

    #[test]
    #[should_panic(expected = "tile size must be positive")]
    fn square_rejects_a_zero_size_tile() {
        let _ = SquareLayout::new(0, 4);
    }

    // ── Isometric ───────────────────────────────────────────────────────────

    #[test]
    fn iso_round_trips_tile_centers() {
        for layout in [IsoLayout::SMALL, IsoLayout::STANDARD, IsoLayout::LARGE] {
            for col in -6..6 {
                for row in -6..6 {
                    let tile = Tile::new(col, row);
                    let cell = layout.tile_to_cell(tile);
                    assert_eq!(layout.cell_to_tile(cell), tile, "{layout:?} {tile:?}");
                }
            }
        }
    }

    #[test]
    fn iso_depth_orders_back_to_front() {
        // A tile further from the camera (smaller col+row) must sort first.
        assert!(IsoLayout::depth(Tile::new(0, 0)) < IsoLayout::depth(Tile::new(1, 0)));
        assert!(IsoLayout::depth(Tile::new(1, 0)) < IsoLayout::depth(Tile::new(1, 1)));
        // Tiles on the same screen row tie, which is correct: they cannot
        // overlap, so their relative order is arbitrary.
        assert_eq!(
            IsoLayout::depth(Tile::new(2, 0)),
            IsoLayout::depth(Tile::new(0, 2))
        );
    }

    #[test]
    fn iso_neighbors_land_where_the_projection_says() {
        let layout = IsoLayout::STANDARD;
        let origin = layout.tile_to_cell(Tile::new(0, 0));
        // +col goes right and down, +row goes left and down.
        let east = layout.tile_to_cell(Tile::new(1, 0));
        assert_eq!(east.x - origin.x, layout.half_w);
        assert_eq!(east.y - origin.y, layout.half_h);
        let south = layout.tile_to_cell(Tile::new(0, 1));
        assert_eq!(south.x - origin.x, -layout.half_w);
        assert_eq!(south.y - origin.y, layout.half_h);
    }

    #[test]
    fn iso_elevation_only_moves_tiles_up() {
        let layout = IsoLayout::STANDARD;
        let flat = layout.tile_to_cell(Tile::new(3, 4));
        let high = layout.tile_to_cell_elevated(Tile::new(3, 4), 5, 2);
        assert_eq!(high.x, flat.x);
        assert_eq!(high.y, flat.y - 10);
    }

    #[test]
    fn iso_diamond_tapers_to_its_points() {
        for layout in [IsoLayout::SMALL, IsoLayout::STANDARD, IsoLayout::LARGE] {
            assert_eq!(layout.span_at(0), Some(layout.half_w), "widest at center");
            assert_eq!(
                layout.span_at(layout.half_h),
                Some(0),
                "one cell at the tip"
            );
            assert_eq!(layout.span_at(layout.half_h + 1), None, "past the tip");
            // Symmetric about the center row, and monotonically narrowing.
            for dy in 0..=layout.half_h {
                assert_eq!(layout.span_at(dy), layout.span_at(-dy));
                if dy > 0 {
                    assert!(layout.span_at(dy) < layout.span_at(dy - 1), "dy {dy}");
                }
            }
        }
    }

    #[test]
    fn iso_contains_agrees_with_span() {
        let layout = IsoLayout::LARGE;
        assert!(layout.contains(0, 0));
        assert!(layout.contains(layout.half_w, 0));
        assert!(!layout.contains(layout.half_w + 1, 0));
        assert!(layout.contains(0, layout.half_h), "the tip cell is inside");
        assert!(!layout.contains(1, layout.half_h), "but only the tip cell");
        assert!(!layout.contains(0, layout.half_h + 1));
    }

    #[test]
    fn iso_diamonds_tile_the_plane_without_gaps_or_overlap() {
        // Every cell in a window must be claimed by exactly one tile's
        // interior, or by tip cells that belong to different tiles. This is
        // the property that makes a diamond map look solid rather than
        // pinstriped.
        let layout = IsoLayout::STANDARD;
        for y in -6..6 {
            for x in -12..12 {
                let cell = Cell::new(x, y);
                let tile = layout.cell_to_tile(cell);
                let center = layout.tile_to_cell(tile);
                let (dx, dy) = (cell.x - center.x, cell.y - center.y);
                assert!(
                    dx.abs() <= layout.half_w && dy.abs() <= layout.half_h,
                    "cell {cell:?} resolved to distant tile {tile:?}"
                );
            }
        }
    }

    // ── Staggered ───────────────────────────────────────────────────────────

    #[test]
    fn staggered_round_trips_and_shifts_odd_rows() {
        let layout = StaggeredLayout::STANDARD;
        for col in -4..4 {
            for row in -4..4 {
                let tile = Tile::new(col, row);
                assert_eq!(layout.cell_to_tile(layout.tile_to_cell(tile)), tile);
            }
        }
        assert_eq!(layout.stagger(0), 0);
        assert_eq!(layout.stagger(1), layout.w / 2);
        assert_eq!(layout.stagger(-1), layout.w / 2, "parity holds below zero");
        assert_eq!(layout.stagger(2), 0);
    }

    #[test]
    #[should_panic(expected = "staggered tile width must be even")]
    fn staggered_rejects_odd_widths() {
        let _ = StaggeredLayout::new(7, 2);
    }

    // ── Hex ─────────────────────────────────────────────────────────────────

    #[test]
    fn hex_round_trips_offset_and_axial() {
        for layout in [HexLayout::POINTY, HexLayout::FLAT, HexLayout::POINTY_LARGE] {
            for col in -6..6 {
                for row in -6..6 {
                    let tile = Tile::new(col, row);
                    let back = layout.from_hex(layout.to_hex(tile));
                    assert_eq!(back, tile, "{layout:?} {tile:?}");
                }
            }
        }
    }

    #[test]
    fn hex_picking_finds_the_tile_under_its_own_center() {
        for layout in [HexLayout::POINTY, HexLayout::FLAT, HexLayout::POINTY_LARGE] {
            for col in -4..4 {
                for row in -4..4 {
                    let tile = Tile::new(col, row);
                    let center = layout.center_cell(tile);
                    assert_eq!(layout.cell_to_tile(center), tile, "{layout:?} {tile:?}");
                }
            }
        }
    }

    #[test]
    fn hex_picking_is_stable_near_centers() {
        // Every cell within a small radius of a hex's center must resolve to
        // that hex; this is the property that makes clicking feel right.
        let layout = HexLayout::POINTY_LARGE;
        let tile = Tile::new(2, 3);
        let center = layout.center_cell(tile);
        for dy in -1..=1 {
            for dx in -2..=2 {
                let probe = center.offset(dx, dy);
                assert_eq!(layout.cell_to_tile(probe), tile, "offset ({dx}, {dy})");
            }
        }
    }

    #[test]
    fn hex_neighbors_are_all_distance_one() {
        for layout in [HexLayout::POINTY, HexLayout::FLAT] {
            let tile = Tile::new(3, 4);
            let neighbors = layout.neighbors(tile);
            for n in neighbors {
                assert_eq!(layout.distance(tile, n), 1, "{layout:?} {n:?}");
            }
            // Six distinct neighbours, none of them the tile itself.
            let mut sorted = neighbors;
            sorted.sort_unstable();
            for pair in sorted.windows(2) {
                assert_ne!(pair[0], pair[1], "duplicate neighbour");
            }
            assert!(!neighbors.contains(&tile));
        }
    }

    #[test]
    fn hex_distance_is_a_metric() {
        let layout = HexLayout::POINTY;
        let (a, b, c) = (Tile::new(0, 0), Tile::new(3, -2), Tile::new(-1, 4));
        assert_eq!(layout.distance(a, a), 0);
        assert_eq!(layout.distance(a, b), layout.distance(b, a));
        assert!(layout.distance(a, c) <= layout.distance(a, b) + layout.distance(b, c));
    }

    #[test]
    fn hex_ring_has_the_right_size_and_distance() {
        let layout = HexLayout::POINTY;
        let center = Tile::new(2, 2);
        assert_eq!(hex_ring(layout, center, 0), vec![center]);
        assert!(hex_ring(layout, center, -1).is_empty());
        for radius in 1..5 {
            let ring = hex_ring(layout, center, radius);
            assert_eq!(ring.len(), 6 * radius as usize, "radius {radius}");
            for tile in ring {
                assert_eq!(layout.distance(center, tile), radius);
            }
        }
    }

    #[test]
    fn hex_spiral_covers_every_hex_in_range_exactly_once() {
        let layout = HexLayout::POINTY;
        let center = Tile::new(0, 0);
        let spiral = hex_spiral(layout, center, 3);
        // 1 + 6*(1+2+3) = 37
        assert_eq!(spiral.len(), 37);
        let mut sorted = spiral.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), spiral.len(), "spiral repeated a hex");
        assert!(spiral.iter().all(|&t| layout.distance(center, t) <= 3));
    }

    #[test]
    fn hex_round_satisfies_the_cube_constraint() {
        for i in 0..40 {
            let (q, r) = (
                (i as f32).mul_add(0.37, -7.0),
                (i as f32).mul_add(-0.21, 3.0),
            );
            let hex = hex_round(q, r);
            assert_eq!(hex.q + hex.r + hex.s(), 0, "q={q} r={r}");
        }
    }

    #[test]
    fn hex_line_is_contiguous_in_every_direction() {
        // The property hexal's own line_to violates, and the one FOV depends
        // on: consecutive steps must be adjacent, or a wall can be skipped.
        for layout in [HexLayout::POINTY, HexLayout::FLAT] {
            for col in -5..=5 {
                for row in -5..=5 {
                    let (a, b) = (Tile::new(0, 0), Tile::new(col, row));
                    let line = hex_line(layout, a, b);
                    for pair in line.windows(2) {
                        assert_eq!(
                            layout.distance(pair[0], pair[1]),
                            1,
                            "{layout:?}: line to {b:?} jumps {:?} -> {:?}",
                            pair[0],
                            pair[1]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn hex_line_has_exactly_distance_plus_one_distinct_steps() {
        let layout = HexLayout::POINTY;
        for col in -5..=5 {
            for row in -5..=5 {
                let (a, b) = (Tile::new(0, 0), Tile::new(col, row));
                let line = hex_line(layout, a, b);
                let expected = layout.distance(a, b) as usize + 1;
                assert_eq!(line.len(), expected, "line to {b:?}");

                let mut sorted = line.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(sorted.len(), line.len(), "line to {b:?} repeats a hex");
            }
        }
    }

    #[test]
    fn hex_line_starts_and_ends_where_asked() {
        let layout = HexLayout::POINTY;
        let (a, b) = (Tile::new(2, -3), Tile::new(-4, 5));
        let line = hex_line(layout, a, b);
        assert_eq!(line.first(), Some(&a));
        assert_eq!(line.last(), Some(&b));
        // A degenerate line is just the point itself.
        assert_eq!(hex_line(layout, a, a), vec![a]);
    }

    #[test]
    fn hex_line_passes_through_every_intervening_ring() {
        // Directly guards the FOV use case: a line to a distant hex must touch
        // every ring in between, so a blocker on any of them stops it.
        let layout = HexLayout::POINTY;
        let center = Tile::new(0, 0);
        for col in -4..=4 {
            for row in -4..=4 {
                let target = Tile::new(col, row);
                let line = hex_line(layout, center, target);
                for radius in 1..layout.distance(center, target) {
                    assert!(
                        line.iter().any(|&t| layout.distance(center, t) == radius),
                        "line to {target:?} skips ring {radius}"
                    );
                }
            }
        }
    }

    #[test]
    fn hex_round_is_exact_on_integers() {
        for q in -4..4 {
            for r in -4..4 {
                let hex = hex_round(q as f32, r as f32);
                assert_eq!((hex.q, hex.r), (q, r));
            }
        }
    }

    #[test]
    fn even_q_differs_from_odd_q_on_odd_columns() {
        let odd_layout = HexLayout::FLAT;
        // Even columns agree between the two schemes; odd ones must not, or
        // the two conventions would be the same thing.
        assert_eq!(
            even_q_to_hex(Tile::new(2, 3)),
            odd_layout.to_hex(Tile::new(2, 3))
        );
        assert_ne!(
            even_q_to_hex(Tile::new(1, 3)),
            odd_layout.to_hex(Tile::new(1, 3))
        );
    }

    #[test]
    fn pointy_and_flat_stagger_different_axes() {
        assert_eq!(HexLayout::POINTY.stagger(Tile::new(0, 1)).1, 0);
        assert!(HexLayout::POINTY.stagger(Tile::new(0, 1)).0 > 0);
        assert_eq!(HexLayout::FLAT.stagger(Tile::new(1, 0)).0, 0);
        assert!(HexLayout::FLAT.stagger(Tile::new(1, 0)).1 > 0);
    }

    #[test]
    #[should_panic(expected = "pointy pitch_x must be even")]
    fn hex_rejects_an_odd_pointy_pitch() {
        let _ = HexLayout::new(HexOrientation::Pointy, 7, 2);
    }
}
