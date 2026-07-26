//! A panning, zooming viewport over a tile map.
//!
//! `retroglyph`'s own [`Camera`](retroglyph_core::Camera) is a viewport over a
//! grid of *cells*, one world cell per screen cell. That is the right
//! abstraction for a roguelike and the wrong one here, because every demo in
//! this gallery draws tiles that are several cells across, at a zoom level the
//! user can change.
//!
//! So this camera works in *tiles* and hands back the tile range to draw plus
//! the cell offset to draw it at, leaving the actual projection to
//! [`geom`](crate::geom). Sub-tile scroll offsets are kept, which is what
//! makes panning smooth instead of jumping a whole tile at a time.

use crate::geom::Cell;

/// A viewport over a tile map, positioned by the cell its top-left corner
/// shows.
///
/// Position is stored in cells rather than tiles precisely so a pan can stop
/// part way through a tile. Storing it in tiles would quantize scrolling to
/// the tile grid, which at [`SquareLayout::CHUNKY`](crate::geom::SquareLayout::CHUNKY)
/// size means the map lurching eight columns at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileCamera {
    /// Cell coordinate shown at the viewport's top-left corner.
    origin: Cell,
    /// Viewport size, in cells.
    view_w: i32,
    view_h: i32,
    /// Map size, in cells. Zero means unbounded (no clamping).
    world_w: i32,
    world_h: i32,
}

impl TileCamera {
    /// A camera over a `world_w` x `world_h` cell map, showing `view_w` x
    /// `view_h` cells, positioned at the origin.
    #[must_use]
    pub const fn new(view_w: i32, view_h: i32, world_w: i32, world_h: i32) -> Self {
        Self {
            origin: Cell::new(0, 0),
            view_w: if view_w > 0 { view_w } else { 1 },
            view_h: if view_h > 0 { view_h } else { 1 },
            world_w: if world_w > 0 { world_w } else { 0 },
            world_h: if world_h > 0 { world_h } else { 0 },
        }
    }

    /// An unbounded camera, for layouts whose extent isn't a simple rectangle
    /// (an isometric diamond, most obviously).
    #[must_use]
    pub const fn unbounded(view_w: i32, view_h: i32) -> Self {
        Self::new(view_w, view_h, 0, 0)
    }

    /// The cell at the viewport's top-left corner.
    #[must_use]
    pub const fn origin(&self) -> Cell {
        self.origin
    }

    /// Viewport size in cells.
    #[must_use]
    pub const fn viewport(&self) -> (i32, i32) {
        (self.view_w, self.view_h)
    }

    /// Resizes the viewport, re-clamping the origin so a shrink can't leave
    /// the camera pointing past the map edge.
    pub const fn set_viewport(&mut self, view_w: i32, view_h: i32) {
        self.view_w = if view_w > 0 { view_w } else { 1 };
        self.view_h = if view_h > 0 { view_h } else { 1 };
        self.clamp();
    }

    /// Replaces the world extent, re-clamping the origin.
    pub const fn set_world(&mut self, world_w: i32, world_h: i32) {
        self.world_w = if world_w > 0 { world_w } else { 0 };
        self.world_h = if world_h > 0 { world_h } else { 0 };
        self.clamp();
    }

    /// Moves the viewport by `(dx, dy)` cells.
    pub const fn pan(&mut self, dx: i32, dy: i32) {
        self.origin = Cell::new(self.origin.x + dx, self.origin.y + dy);
        self.clamp();
    }

    /// Jumps so that `cell` is at the viewport's center.
    pub const fn center_on(&mut self, cell: Cell) {
        self.origin = Cell::new(cell.x - self.view_w / 2, cell.y - self.view_h / 2);
        self.clamp();
    }

    /// Clamps the origin so the viewport stays over the map.
    ///
    /// A viewport larger than the map is centered on it rather than pinned to
    /// a corner, because the alternative (clamping to 0) leaves all the empty
    /// space on one side, which looks like a bug even though it is one.
    const fn clamp(&mut self) {
        if self.world_w > 0 {
            let max = self.world_w - self.view_w;
            self.origin.x = if max < 0 {
                max / 2
            } else {
                clamp_i32(self.origin.x, 0, max)
            };
        }
        if self.world_h > 0 {
            let max = self.world_h - self.view_h;
            self.origin.y = if max < 0 {
                max / 2
            } else {
                clamp_i32(self.origin.y, 0, max)
            };
        }
    }

    /// Converts a world cell to a viewport-relative cell.
    ///
    /// Always succeeds, including for cells outside the viewport, which is
    /// what lets a renderer draw a tile that straddles the edge and clip per
    /// cell rather than dropping the whole tile.
    #[must_use]
    pub const fn world_to_screen(&self, cell: Cell) -> Cell {
        Cell::new(cell.x - self.origin.x, cell.y - self.origin.y)
    }

    /// Converts a viewport-relative cell back to a world cell.
    #[must_use]
    pub const fn screen_to_world(&self, cell: Cell) -> Cell {
        Cell::new(cell.x + self.origin.x, cell.y + self.origin.y)
    }

    /// Whether a viewport-relative cell is actually on screen.
    #[must_use]
    pub const fn on_screen(&self, cell: Cell) -> bool {
        cell.x >= 0 && cell.y >= 0 && cell.x < self.view_w && cell.y < self.view_h
    }

    /// The inclusive world-cell bounds the viewport currently shows, as
    /// `(left, top, right, bottom)`.
    #[must_use]
    pub const fn visible_cells(&self) -> (i32, i32, i32, i32) {
        (
            self.origin.x,
            self.origin.y,
            self.origin.x + self.view_w - 1,
            self.origin.y + self.view_h - 1,
        )
    }

    /// The half-open tile range to draw, as `(col_start, row_start, col_end,
    /// row_end)`, given a tile size in cells.
    ///
    /// Padded by one tile on every side so tiles straddling the viewport edge
    /// are still drawn (and then clipped per cell). Without that margin, a
    /// tile scrolling in from the left pops into existence only once its
    /// top-left corner crosses the edge, which is the single most obvious
    /// scrolling artifact there is.
    #[must_use]
    pub const fn visible_tiles(&self, tile_w: i32, tile_h: i32) -> (i32, i32, i32, i32) {
        if tile_w <= 0 || tile_h <= 0 {
            return (0, 0, 0, 0);
        }
        let col_start = self.origin.x.div_euclid(tile_w) - 1;
        let row_start = self.origin.y.div_euclid(tile_h) - 1;
        let col_end = (self.origin.x + self.view_w).div_euclid(tile_w) + 2;
        let row_end = (self.origin.y + self.view_h).div_euclid(tile_h) + 2;
        (col_start, row_start, col_end, row_end)
    }
}

/// `i32::clamp` is not `const`, and every clamp in this module runs inside one.
const fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::TileCamera;
    use crate::geom::Cell;

    #[test]
    fn a_new_camera_starts_at_the_origin() {
        let cam = TileCamera::new(40, 20, 200, 100);
        assert_eq!(cam.origin(), Cell::new(0, 0));
        assert_eq!(cam.viewport(), (40, 20));
        assert_eq!(cam.visible_cells(), (0, 0, 39, 19));
    }

    #[test]
    fn panning_clamps_to_the_map_edges() {
        let mut cam = TileCamera::new(40, 20, 200, 100);
        cam.pan(-50, -50);
        assert_eq!(cam.origin(), Cell::new(0, 0), "clamped at the near edge");
        cam.pan(1000, 1000);
        assert_eq!(
            cam.origin(),
            Cell::new(160, 80),
            "clamped at the far edge, leaving a full viewport of map"
        );
    }

    #[test]
    fn an_unbounded_camera_never_clamps() {
        let mut cam = TileCamera::unbounded(40, 20);
        cam.pan(-500, -500);
        assert_eq!(cam.origin(), Cell::new(-500, -500));
        cam.pan(9000, 9000);
        assert_eq!(cam.origin(), Cell::new(8500, 8500));
    }

    #[test]
    fn centering_puts_the_target_in_the_middle() {
        let mut cam = TileCamera::new(40, 20, 400, 200);
        cam.center_on(Cell::new(200, 100));
        assert_eq!(cam.origin(), Cell::new(180, 90));
        let screen = cam.world_to_screen(Cell::new(200, 100));
        assert_eq!(screen, Cell::new(20, 10), "target lands mid-viewport");
    }

    #[test]
    fn centering_near_an_edge_still_clamps() {
        let mut cam = TileCamera::new(40, 20, 200, 100);
        cam.center_on(Cell::new(0, 0));
        assert_eq!(cam.origin(), Cell::new(0, 0));
        cam.center_on(Cell::new(199, 99));
        assert_eq!(cam.origin(), Cell::new(160, 80));
    }

    #[test]
    fn a_viewport_larger_than_the_map_centers_the_map() {
        // Pinning to the corner instead would push all the empty space to one
        // side, which reads as a layout bug.
        let mut cam = TileCamera::new(100, 50, 40, 20);
        cam.pan(0, 0);
        assert_eq!(cam.origin(), Cell::new(-30, -15));
        let (left, top, right, bottom) = cam.visible_cells();
        assert_eq!(left.midpoint(right), 19, "map is horizontally centered");
        assert_eq!(top.midpoint(bottom), 9, "map is vertically centered");
    }

    #[test]
    fn screen_and_world_conversions_round_trip() {
        let mut cam = TileCamera::new(40, 20, 400, 200);
        cam.center_on(Cell::new(123, 77));
        for (x, y) in [(0, 0), (39, 19), (-5, -5), (100, 100)] {
            let cell = Cell::new(x, y);
            assert_eq!(cam.world_to_screen(cam.screen_to_world(cell)), cell);
            assert_eq!(cam.screen_to_world(cam.world_to_screen(cell)), cell);
        }
    }

    #[test]
    fn on_screen_bounds_the_viewport_exactly() {
        let cam = TileCamera::new(40, 20, 400, 200);
        assert!(cam.on_screen(Cell::new(0, 0)));
        assert!(cam.on_screen(Cell::new(39, 19)));
        assert!(!cam.on_screen(Cell::new(40, 19)), "one past the right edge");
        assert!(!cam.on_screen(Cell::new(39, 20)), "one past the bottom");
        assert!(!cam.on_screen(Cell::new(-1, 0)));
    }

    #[test]
    fn resizing_the_viewport_reclamps_the_origin() {
        let mut cam = TileCamera::new(40, 20, 200, 100);
        cam.pan(1000, 1000);
        assert_eq!(cam.origin(), Cell::new(160, 80));
        cam.set_viewport(80, 40);
        assert_eq!(cam.origin(), Cell::new(120, 60), "shrunk back into bounds");
    }

    #[test]
    fn changing_the_world_reclamps_the_origin() {
        let mut cam = TileCamera::new(40, 20, 400, 200);
        cam.pan(300, 150);
        assert_eq!(cam.origin(), Cell::new(300, 150));
        cam.set_world(200, 100);
        assert_eq!(cam.origin(), Cell::new(160, 80));
    }

    #[test]
    fn degenerate_sizes_do_not_produce_a_zero_viewport() {
        let cam = TileCamera::new(0, -5, 100, 100);
        assert_eq!(cam.viewport(), (1, 1), "viewport is never empty");
    }

    #[test]
    fn visible_tiles_covers_the_viewport_with_a_margin() {
        let mut cam = TileCamera::unbounded(40, 20);
        cam.pan(100, 50);
        let (c0, r0, c1, r1) = cam.visible_tiles(8, 4);
        // The viewport spans cells x 100..140, y 50..70, i.e. tiles
        // 12..17 and 12..17. The range must strictly contain those.
        assert!(c0 < 12, "left margin missing (got {c0})");
        assert!(c1 > 17, "right margin missing (got {c1})");
        assert!(r0 < 12, "top margin missing (got {r0})");
        assert!(r1 > 17, "bottom margin missing (got {r1})");
    }

    #[test]
    fn visible_tiles_covers_every_tile_that_touches_the_viewport() {
        // The property that actually matters: no tile overlapping the
        // viewport may be omitted, or it will pop in as you scroll.
        let mut cam = TileCamera::unbounded(37, 17);
        for (px, py) in [(0, 0), (3, 1), (-11, -7), (129, 63)] {
            cam.pan(px, py);
            let (c0, r0, c1, r1) = cam.visible_tiles(8, 4);
            let (ox, oy) = (cam.origin().x, cam.origin().y);
            for y in oy..oy + 17 {
                for x in ox..ox + 37 {
                    let (col, row) = (x.div_euclid(8), y.div_euclid(4));
                    assert!(
                        (c0..c1).contains(&col) && (r0..r1).contains(&row),
                        "tile ({col}, {row}) for cell ({x}, {y}) is outside {:?}",
                        (c0, r0, c1, r1)
                    );
                }
            }
            cam.pan(-px, -py);
        }
    }

    #[test]
    fn visible_tiles_rejects_a_degenerate_tile_size() {
        let cam = TileCamera::unbounded(40, 20);
        assert_eq!(cam.visible_tiles(0, 4), (0, 0, 0, 0));
        assert_eq!(cam.visible_tiles(8, -1), (0, 0, 0, 0));
    }
}
