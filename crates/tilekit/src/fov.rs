//! Field of view and fog of war.
//!
//! Two things a strategy map needs and that look the same but aren't:
//!
//! - **Field of view** is a geometric query: from here, with these obstacles,
//!   what can be seen right now? Recomputed whenever a unit moves.
//! - **Fog of war** is accumulated memory: what has *ever* been seen, and is
//!   it visible at this instant? Persists across turns.
//!
//! [`shadowcast`] answers the first, [`FogMap`] tracks the second, and
//! [`hex_fov`] answers the first on a hex grid.

use crate::geom::{HexLayout, Tile};

/// What the player knows about a tile.
///
/// Ordered so `>=` comparisons work the way the words suggest: anything at
/// least [`Explored`](Visibility::Explored) has been seen at some point, so
/// the check for "may I draw terrain here" is a single comparison rather than
/// an enumeration of cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Visibility {
    /// Never seen. Draw nothing but shroud.
    #[default]
    Unknown,
    /// Seen before, not visible now. Draw remembered terrain, dimmed, with no
    /// units: this is the state that makes scouting matter, because what you
    /// remember can be out of date.
    Explored,
    /// Visible right now. Draw everything.
    Visible,
}

impl Visibility {
    /// Whether terrain should be drawn at all.
    #[must_use]
    pub const fn shows_terrain(self) -> bool {
        matches!(self, Self::Explored | Self::Visible)
    }

    /// Whether transient contents (units, animation) should be drawn.
    #[must_use]
    pub const fn shows_units(self) -> bool {
        matches!(self, Self::Visible)
    }
}

/// Per-tile visibility over a rectangular map.
#[derive(Debug, Clone)]
pub struct FogMap {
    width: usize,
    height: usize,
    tiles: Vec<Visibility>,
}

impl FogMap {
    /// A fully unexplored map.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let (width, height) = (width as usize, height as usize);
        Self {
            width,
            height,
            tiles: vec![Visibility::Unknown; width * height],
        }
    }

    /// Map size in tiles.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Visibility at `(x, y)`, or [`Unknown`](Visibility::Unknown) if out of
    /// bounds.
    ///
    /// Out-of-bounds reading as unknown rather than panicking is deliberate:
    /// renderers routinely sample a cell past the map edge while drawing a
    /// tile that straddles it, and "there is nothing there" is the correct
    /// answer for that query.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> Visibility {
        self.index(x, y)
            .map_or(Visibility::Unknown, |i| self.tiles[i])
    }

    /// Whether `(x, y)` is inside the map.
    #[must_use]
    pub const fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height
    }

    const fn index(&self, x: i32, y: i32) -> Option<usize> {
        if self.in_bounds(x, y) {
            Some(y as usize * self.width + x as usize)
        } else {
            None
        }
    }

    /// Demotes every currently-visible tile to explored.
    ///
    /// Call once before recomputing visibility for the turn. Splitting this
    /// from [`reveal`](Self::reveal) is what lets several units contribute to
    /// one turn's visibility: demote once, then reveal from each unit.
    pub fn begin_turn(&mut self) {
        for tile in &mut self.tiles {
            if *tile == Visibility::Visible {
                *tile = Visibility::Explored;
            }
        }
    }

    /// Marks `(x, y)` visible (and therefore explored, permanently).
    pub fn reveal(&mut self, x: i32, y: i32) {
        if let Some(i) = self.index(x, y) {
            self.tiles[i] = Visibility::Visible;
        }
    }

    /// Marks everything visible. For a "reveal map" toggle.
    pub fn reveal_all(&mut self) {
        self.tiles.fill(Visibility::Visible);
    }

    /// Resets everything to unexplored.
    pub fn reset(&mut self) {
        self.tiles.fill(Visibility::Unknown);
    }

    /// Fraction of the map ever explored, in `0.0..=1.0`.
    #[must_use]
    pub fn explored_fraction(&self) -> f32 {
        if self.tiles.is_empty() {
            return 0.0;
        }
        let seen = self.tiles.iter().filter(|v| v.shows_terrain()).count();
        seen as f32 / self.tiles.len() as f32
    }
}

/// One octant's basis change plus its two boundary-ownership flags.
#[derive(Clone, Copy)]
struct Octant {
    /// The `(xx, xy, yx, yy)` transform from octant-local to world offsets.
    transform: (i32, i32, i32, i32),
    /// Skip *reporting* the cells on the axis ray (`local dx == 0`); the
    /// neighbouring octant that shares this ray owns them.
    skip_axis: bool,
    /// Skip *reporting* the cells on the diagonal ray (`local dx == dy`).
    skip_diagonal: bool,
}

/// The eight octant transforms for recursive shadowcasting.
///
/// Shadowcasting is written once for a single octant and then applied eight
/// times through these transforms, rather than written out eight times with
/// the inequalities flipped. That is the whole trick: the algorithm looks
/// short because seven eighths of the work is a change of basis.
///
/// The complication is that adjacent octants *share* their boundary rays: the
/// four axes and the four diagonals each bound two octants, so a naive
/// implementation reports those cells twice. That is harmless when the caller
/// writes into a set, and quietly wrong when it accumulates (light levels,
/// visit counts, anything additive). The flags fix it by assigning each shared
/// ray to exactly one of its two octants:
///
/// | Ray | Shared by | Owner |
/// | --- | --- | --- |
/// | North axis | 0, 3 | 0 |
/// | West axis | 1, 6 | 1 |
/// | East axis | 2, 5 | 2 |
/// | South axis | 4, 7 | 4 |
/// | NW diagonal | 0, 1 | 0 |
/// | NE diagonal | 2, 3 | 2 |
/// | SE diagonal | 4, 5 | 4 |
/// | SW diagonal | 6, 7 | 6 |
///
/// Only *reporting* is suppressed; the scan still walks the ray so a wall
/// standing on it still casts its shadow.
const OCTANTS: [Octant; 8] = [
    Octant {
        transform: (1, 0, 0, 1),
        skip_axis: false,
        skip_diagonal: false,
    },
    Octant {
        transform: (0, 1, 1, 0),
        skip_axis: false,
        skip_diagonal: true,
    },
    Octant {
        transform: (0, -1, 1, 0),
        skip_axis: false,
        skip_diagonal: false,
    },
    Octant {
        transform: (-1, 0, 0, 1),
        skip_axis: true,
        skip_diagonal: true,
    },
    Octant {
        transform: (-1, 0, 0, -1),
        skip_axis: false,
        skip_diagonal: false,
    },
    Octant {
        transform: (0, -1, -1, 0),
        skip_axis: true,
        skip_diagonal: true,
    },
    Octant {
        transform: (0, 1, -1, 0),
        skip_axis: true,
        skip_diagonal: false,
    },
    Octant {
        transform: (1, 0, 0, -1),
        skip_axis: true,
        skip_diagonal: true,
    },
];

/// Recursive shadowcasting field of view on a square grid.
///
/// Calls `blocks(x, y)` to ask whether a tile blocks sight, and `visible(x, y)`
/// exactly once for each tile that can be seen (including the origin). Sight
/// is limited to `radius` tiles, measured as a circle.
///
/// Runs in time proportional to the visible *area* rather than the number of
/// ray casts, and produces symmetric, artifact-free results: no "pillars" of
/// light behind wall corners, no stair-stepped shadows. See [RogueBasin's
/// write-up](https://www.roguebasin.com/index.php/FOV_using_recursive_shadowcasting).
pub fn shadowcast<B, V>(origin_x: i32, origin_y: i32, radius: i32, mut blocks: B, mut visible: V)
where
    B: FnMut(i32, i32) -> bool,
    V: FnMut(i32, i32),
{
    visible(origin_x, origin_y);
    if radius <= 0 {
        return;
    }
    for octant in OCTANTS {
        cast_octant(
            origin_x,
            origin_y,
            radius,
            1,
            1.0,
            0.0,
            octant,
            &mut blocks,
            &mut visible,
        );
    }
}

/// One octant of [`shadowcast`], recursing whenever a run of blocking tiles
/// splits the visible arc in two.
#[allow(clippy::too_many_arguments)]
fn cast_octant<B, V>(
    ox: i32,
    oy: i32,
    radius: i32,
    row: i32,
    mut start_slope: f32,
    end_slope: f32,
    octant: Octant,
    blocks: &mut B,
    visible: &mut V,
) where
    B: FnMut(i32, i32) -> bool,
    V: FnMut(i32, i32),
{
    if start_slope < end_slope {
        return;
    }
    let (xx, xy, yx, yy) = octant.transform;
    let radius_sq = radius * radius;
    let mut blocked = false;
    let mut next_start = start_slope;

    for distance in row..=radius {
        if blocked {
            break;
        }
        let mut delta_x = -distance - 1;
        let delta_y = -distance;

        while delta_x <= 0 {
            delta_x += 1;
            let current_x = ox + delta_x * xx + delta_y * xy;
            let current_y = oy + delta_x * yx + delta_y * yy;

            // Slopes to this cell's leading and trailing edges. The half-cell
            // offsets are what make the result symmetric: without them, a wall
            // casts a shadow that is one cell narrower on one side.
            let left_slope = (delta_x as f32 - 0.5) / (delta_y as f32 + 0.5);
            let right_slope = (delta_x as f32 + 0.5) / (delta_y as f32 - 0.5);

            if right_slope > start_slope {
                continue;
            }
            if left_slope < end_slope {
                break;
            }

            let on_axis = delta_x == 0;
            let on_diagonal = delta_x == delta_y;
            let disowned = (on_axis && octant.skip_axis) || (on_diagonal && octant.skip_diagonal);
            let owned = !disowned;
            if owned && delta_x * delta_x + delta_y * delta_y <= radius_sq {
                visible(current_x, current_y);
            }

            if blocked {
                if blocks(current_x, current_y) {
                    next_start = right_slope;
                } else {
                    blocked = false;
                    start_slope = next_start;
                }
            } else if blocks(current_x, current_y) && distance < radius {
                // A new wall: everything past it in this scan is shadowed, so
                // recurse for the still-visible arc to its left and continue
                // this loop for the arc to its right.
                blocked = true;
                cast_octant(
                    ox,
                    oy,
                    radius,
                    distance + 1,
                    start_slope,
                    left_slope,
                    octant,
                    blocks,
                    visible,
                );
                next_start = right_slope;
            }
        }
    }
}

/// Field of view on a hex grid, by line-of-sight tracing.
///
/// Returns every hex within `radius` of `center` that has an unobstructed hex
/// line to it. Shadowcasting's octant decomposition has no clean hex analogue
/// (a hex grid has six directions, not eight, and its rows do not nest the
/// same way), so this traces a line per target instead: `O(r^3)` against
/// shadowcasting's `O(r^2)`, which is irrelevant at the radii a strategy game
/// actually uses.
///
/// See Red Blob Games on [hex field of view](https://www.redblobgames.com/grids/hexagons/#field-of-view).
pub fn hex_fov<B>(layout: HexLayout, center: Tile, radius: i32, mut blocks: B) -> Vec<Tile>
where
    B: FnMut(Tile) -> bool,
{
    let mut seen = vec![center];
    if radius <= 0 {
        return seen;
    }

    for target in crate::geom::hex_spiral(layout, center, radius) {
        if target == center {
            continue;
        }
        // `geom::hex_line`, not `hexal::Hex::line_to`: the latter is not
        // contiguous along the q == r diagonal, so a blocker sitting on that
        // diagonal gets stepped straight over and fails to block anything.
        let line = crate::geom::hex_line(layout, center, target);

        // Walk the line and stop at the first blocker. The blocker itself is
        // visible (you can see the wall, just not past it), which is what
        // makes a room's walls render instead of being an invisible boundary.
        let mut clear = true;
        for &step in line.iter().skip(1) {
            if step == target {
                break;
            }
            if blocks(step) {
                clear = false;
                break;
            }
        }
        if clear {
            seen.push(target);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::{FogMap, Visibility, hex_fov, shadowcast};
    use crate::geom::{HexLayout, Tile};
    use std::collections::HashSet;

    /// Runs shadowcasting over a small ASCII map and returns the visible set.
    /// `#` blocks, `@` is the origin.
    fn cast_map(map: &[&str], radius: i32) -> HashSet<(i32, i32)> {
        let grid: Vec<Vec<char>> = map.iter().map(|r| r.chars().collect()).collect();
        let mut origin = (0, 0);
        for (y, row) in grid.iter().enumerate() {
            for (x, &c) in row.iter().enumerate() {
                if c == '@' {
                    origin = (x as i32, y as i32);
                }
            }
        }
        let blocks = |x: i32, y: i32| {
            grid.get(y as usize)
                .and_then(|r| r.get(x as usize))
                .is_none_or(|&c| c == '#')
        };
        let mut seen = HashSet::new();
        shadowcast(origin.0, origin.1, radius, blocks, |x, y| {
            seen.insert((x, y));
        });
        seen
    }

    // ── Visibility ──────────────────────────────────────────────────────────

    #[test]
    fn visibility_states_are_ordered_by_how_much_they_reveal() {
        assert!(Visibility::Unknown < Visibility::Explored);
        assert!(Visibility::Explored < Visibility::Visible);
        assert!(!Visibility::Unknown.shows_terrain());
        assert!(Visibility::Explored.shows_terrain());
        assert!(Visibility::Visible.shows_terrain());
        assert!(!Visibility::Explored.shows_units(), "memory has no units");
        assert!(Visibility::Visible.shows_units());
    }

    // ── FogMap ──────────────────────────────────────────────────────────────

    #[test]
    fn a_new_fog_map_is_entirely_unknown() {
        let fog = FogMap::new(8, 5);
        assert_eq!(fog.size(), (8, 5));
        for y in 0..5 {
            for x in 0..8 {
                assert_eq!(fog.get(x, y), Visibility::Unknown);
            }
        }
        assert!((fog.explored_fraction() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn revealing_then_ending_the_turn_leaves_a_memory() {
        let mut fog = FogMap::new(4, 4);
        fog.reveal(1, 1);
        assert_eq!(fog.get(1, 1), Visibility::Visible);

        fog.begin_turn();
        assert_eq!(fog.get(1, 1), Visibility::Explored, "memory persists");
        assert_eq!(fog.get(2, 2), Visibility::Unknown, "and does not spread");

        // Explored never decays back to unknown, no matter how many turns pass.
        for _ in 0..10 {
            fog.begin_turn();
        }
        assert_eq!(fog.get(1, 1), Visibility::Explored);
    }

    #[test]
    fn out_of_bounds_access_is_unknown_and_writes_are_dropped() {
        let mut fog = FogMap::new(4, 4);
        for (x, y) in [(-1, 0), (0, -1), (4, 0), (0, 4), (99, 99)] {
            assert!(!fog.in_bounds(x, y));
            assert_eq!(fog.get(x, y), Visibility::Unknown);
            fog.reveal(x, y);
            assert_eq!(fog.get(x, y), Visibility::Unknown, "write leaked");
        }
        assert!((fog.explored_fraction() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn explored_fraction_tracks_revealed_tiles() {
        let mut fog = FogMap::new(4, 4);
        for x in 0..4 {
            fog.reveal(x, 0);
        }
        assert!((fog.explored_fraction() - 0.25).abs() < 1e-6);
        fog.reveal_all();
        assert!((fog.explored_fraction() - 1.0).abs() < 1e-6);
        fog.reset();
        assert!((fog.explored_fraction() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn a_zero_sized_map_does_not_divide_by_zero() {
        assert!((FogMap::new(0, 0).explored_fraction() - 0.0).abs() < f32::EPSILON);
    }

    // ── Shadowcasting ───────────────────────────────────────────────────────

    #[test]
    fn the_origin_is_always_visible_even_at_zero_radius() {
        let mut seen = Vec::new();
        shadowcast(3, 4, 0, |_, _| false, |x, y| seen.push((x, y)));
        assert_eq!(seen, vec![(3, 4)]);
    }

    #[test]
    fn open_ground_is_fully_visible_within_the_radius() {
        let seen = cast_map(
            &[
                ".......", ".......", ".......", "...@...", ".......", ".......", ".......",
            ],
            3,
        );
        // Every cell within Euclidean radius 3 of (3, 3) must be visible.
        for y in 0..7 {
            for x in 0..7 {
                let (dx, dy) = (x - 3, y - 3);
                if dx * dx + dy * dy <= 9 {
                    assert!(seen.contains(&(x, y)), "({x}, {y}) should be visible");
                }
            }
        }
    }

    #[test]
    fn nothing_outside_the_radius_is_ever_visible() {
        let seen = cast_map(&["..........", "....@.....", ".........."], 2);
        for &(x, y) in &seen {
            let (dx, dy) = (x - 4, y - 1);
            assert!(dx * dx + dy * dy <= 8, "({x}, {y}) is beyond the radius");
        }
    }

    #[test]
    fn a_wall_casts_a_shadow_behind_it() {
        //     0123456
        //  0  .......
        //  1  ...#...   wall directly north of the origin
        //  2  ...@...
        let seen = cast_map(&["-------", "...#...", "...@...", "......."], 4);
        assert!(seen.contains(&(3, 1)), "the wall itself is visible");
        assert!(!seen.contains(&(3, 0)), "directly behind the wall is dark");
    }

    #[test]
    fn a_sealed_room_shows_only_its_own_walls() {
        let seen = cast_map(&["#####", "#...#", "#.@.#", "#...#", "#####"], 8);
        // Every floor tile and every wall tile is visible; nothing beyond.
        for y in 0..5 {
            for x in 0..5 {
                assert!(seen.contains(&(x, y)), "({x}, {y}) inside the room");
            }
        }
        for &(x, y) in &seen {
            assert!(
                (0..5).contains(&x) && (0..5).contains(&y),
                "saw ({x}, {y}) outside the sealed room"
            );
        }
    }

    #[test]
    fn light_passes_through_a_doorway_but_not_the_wall_beside_it() {
        //  0  #######
        //  1  #.....#
        //  2  ###.###   a one-tile doorway
        //  3  ...@...
        let seen = cast_map(&["#######", "#.....#", "###.###", "...@..."], 6);
        assert!(seen.contains(&(3, 2)), "the doorway is visible");
        assert!(seen.contains(&(3, 1)), "and straight through it");
        assert!(
            !seen.contains(&(1, 1)),
            "but not far off-axis through a one-tile gap"
        );
    }

    #[test]
    fn visibility_is_symmetric_across_a_pillar() {
        // A single pillar should shadow a wedge, not produce scattered
        // asymmetric artifacts. Check the shadow is mirror-symmetric about
        // the origin-pillar axis.
        let seen = cast_map(
            &[
                ".........",
                ".........",
                ".........",
                "....#....",
                "....@....",
                ".........",
            ],
            4,
        );
        for dx in 1..=3 {
            assert_eq!(
                seen.contains(&(4 - dx, 2)),
                seen.contains(&(4 + dx, 2)),
                "shadow is asymmetric at dx = {dx}"
            );
        }
    }

    #[test]
    fn no_cell_is_reported_twice_in_open_ground() {
        // Duplicate reports would be harmless for a HashSet but would double
        // the work of any caller accumulating light levels.
        let mut counts = std::collections::HashMap::new();
        shadowcast(
            0,
            0,
            5,
            |_, _| false,
            |x, y| {
                *counts.entry((x, y)).or_insert(0) += 1;
            },
        );
        let dupes: Vec<_> = counts.iter().filter(|&(_, &n)| n > 1).collect();
        assert!(dupes.is_empty(), "reported twice: {dupes:?}");
    }

    // ── Hex FOV ─────────────────────────────────────────────────────────────

    #[test]
    fn hex_fov_sees_everything_on_open_ground() {
        let layout = HexLayout::POINTY;
        let center = Tile::new(0, 0);
        let seen = hex_fov(layout, center, 3, |_| false);
        // 1 + 6*(1+2+3) = 37 hexes within radius 3.
        assert_eq!(seen.len(), 37);
        assert!(seen.contains(&center));
    }

    #[test]
    fn hex_fov_at_zero_radius_sees_only_itself() {
        let seen = hex_fov(HexLayout::POINTY, Tile::new(2, 2), 0, |_| false);
        assert_eq!(seen, vec![Tile::new(2, 2)]);
    }

    #[test]
    fn hex_fov_never_looks_past_the_radius() {
        let layout = HexLayout::FLAT;
        let center = Tile::new(1, 1);
        for tile in hex_fov(layout, center, 4, |_| false) {
            assert!(layout.distance(center, tile) <= 4, "{tile:?} is too far");
        }
    }

    #[test]
    fn a_hex_blocker_hides_what_is_behind_it() {
        let layout = HexLayout::POINTY;
        let center = Tile::new(0, 0);
        let neighbors = layout.neighbors(center);
        let wall = neighbors[0];

        let open = hex_fov(layout, center, 3, |_| false);
        let blocked = hex_fov(layout, center, 3, |t| t == wall);

        assert!(blocked.contains(&wall), "the blocker itself stays visible");
        assert!(blocked.len() < open.len(), "the blocker hid nothing");
        // Everything still visible must be genuinely reachable.
        for tile in &blocked {
            assert!(layout.distance(center, *tile) <= 3);
        }
    }

    #[test]
    fn a_ring_of_blockers_confines_the_view() {
        let layout = HexLayout::POINTY;
        let center = Tile::new(0, 0);
        let wall: Vec<Tile> = crate::geom::hex_ring(layout, center, 1);
        let seen = hex_fov(layout, center, 4, |t| wall.contains(&t));
        // Only the center and its six walls should be visible.
        assert_eq!(seen.len(), 7, "saw beyond the ring: {seen:?}");
    }
}
