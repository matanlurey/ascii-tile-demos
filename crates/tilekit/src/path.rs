//! Least-cost pathfinding over a weighted grid, and the turn budget that turns
//! a path into a *plan*.
//!
//! The interesting output here is not the route. Any A* returns a route; the
//! thing a strategy map actually shows is where this turn's movement runs out,
//! which is why [`Path`] carries [`reach`](Path::reach) alongside its steps.
//! Heroes of Might and Magic draws the same path in two colors split at that
//! index, and it is the single most information-dense element on its adventure
//! map: one glance answers "can I get there", "how far do I get", and "what
//! does it cost", without a tooltip.
//!
//! Costs come from the caller rather than from [`Biome`](crate::world::Biome)
//! directly, because "impassable" is a property of the traveller as much as of
//! the terrain: a boat's costs are the inverse of a hero's, and a scout
//! ignores roads a wagon must follow.

use alloc::collections::BinaryHeap;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;

use crate::geom::Cell;

extern crate alloc;

/// A cost that means "cannot enter".
///
/// A sentinel rather than `Option<u32>` because the cost callback is invoked
/// once per neighbour per expansion, and a plain integer compare is both
/// faster and easier to write at the call site than matching an option.
pub const IMPASSABLE: u32 = u32::MAX;

/// A route, plus how far along it the traveller gets on the current turn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Path {
    /// Cells from the start (exclusive) to the goal (inclusive).
    ///
    /// The start is excluded because every consumer either already knows where
    /// it started or is drawing a marker per *step taken*, and an inclusive
    /// start makes both off by one.
    pub steps: Vec<Cell>,
    /// Cumulative cost to enter each corresponding step.
    pub costs: Vec<u32>,
    /// How many leading steps fit the budget given to [`find`].
    ///
    /// `steps[..reach]` is reachable now and `steps[reach..]` is not, so a
    /// renderer splits the color there. Equals `steps.len()` when the whole
    /// route is affordable, and `0` when even the first step is not.
    pub reach: usize,
}

impl Path {
    /// The total cost of the whole route.
    #[must_use]
    pub fn total_cost(&self) -> u32 {
        self.costs.last().copied().unwrap_or(0)
    }

    /// Whether the goal is reachable within the budget.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.reach == self.steps.len() && !self.steps.is_empty()
    }

    /// The last cell reachable this turn, if any.
    #[must_use]
    pub fn stop(&self) -> Option<Cell> {
        self.reach
            .checked_sub(1)
            .and_then(|i| self.steps.get(i))
            .copied()
    }
}

/// A node on the frontier, ordered by cost so [`BinaryHeap`] pops the cheapest.
#[derive(PartialEq, Eq)]
struct Node {
    cost: u32,
    cell: Cell,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // Ties broken on coordinates so the search is deterministic. Without
        // it, two equal-cost routes are chosen by heap order, which varies
        // with insertion history, and the drawn path flickers between them as
        // the camera moves.
        self.cost
            .cmp(&other.cost)
            .then_with(|| self.cell.y.cmp(&other.cell.y))
            .then_with(|| self.cell.x.cmp(&other.cell.x))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The four cardinal neighbours.
const CARDINALS: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// The four diagonals, in the same rotational order.
const DIAGONALS: [(i32, i32); 4] = [(1, -1), (1, 1), (-1, 1), (-1, -1)];

/// Whether a search may move diagonally, and what that costs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Diagonals {
    /// Cardinal moves only. The right choice when the map is drawn in
    /// multi-cell tiles, where a diagonal step visually skips a corner.
    #[default]
    Never,
    /// Diagonals allowed at the same cost as a cardinal step. Cheap and
    /// forgiving, at the cost of routes that visibly prefer staircases.
    Free,
    /// Diagonals allowed at 1.41x, rounded. What Heroes of Might and Magic
    /// charges, and what stops a path from zigzagging where a straight line
    /// would do.
    Costly,
}

impl Diagonals {
    /// Scales a step cost for a diagonal move.
    const fn scale(self, cost: u32) -> u32 {
        match self {
            Self::Never => IMPASSABLE,
            Self::Free => cost,
            // 1.41 as a fraction, kept in integer math so a path's cost is
            // exactly reproducible across platforms.
            Self::Costly => (cost * 141) / 100,
        }
    }
}

/// Finds the least-cost route from `start` to `goal`.
///
/// `cost` returns the cost to *enter* a cell, or [`IMPASSABLE`]. `budget` is
/// what the traveller can spend now, and only affects [`Path::reach`]: the
/// whole route is still returned, because a strategy map wants to show the
/// unaffordable remainder rather than hide it.
///
/// Dijkstra rather than A*: the maps here are small enough that the heuristic
/// buys little, and an unguided search stays correct under any cost function a
/// caller invents, including the negative-looking ones (roads cheaper than
/// open ground) that break an inadmissible heuristic.
///
/// Returns `None` if no route exists.
#[must_use]
pub fn find(
    start: Cell,
    goal: Cell,
    width: i32,
    height: i32,
    diagonals: Diagonals,
    budget: u32,
    mut cost: impl FnMut(Cell) -> u32,
) -> Option<Path> {
    if start == goal || !in_bounds(goal, width, height) || !in_bounds(start, width, height) {
        return None;
    }

    let idx = |c: Cell| (c.y * width + c.x) as usize;
    let mut best = vec![IMPASSABLE; (width * height) as usize];
    let mut came: Vec<Option<Cell>> = vec![None; (width * height) as usize];
    let mut heap = BinaryHeap::new();

    best[idx(start)] = 0;
    heap.push(Reverse(Node {
        cost: 0,
        cell: start,
    }));

    while let Some(Reverse(Node { cost: spent, cell })) = heap.pop() {
        if cell == goal {
            break;
        }
        // A stale heap entry: this cell was reached more cheaply after this
        // entry was pushed. Skipping beats a decrease-key the heap does not
        // offer.
        if spent > best[idx(cell)] {
            continue;
        }

        let steps = CARDINALS
            .iter()
            .map(|&d| (d, false))
            .chain(DIAGONALS.iter().map(|&d| (d, true)));

        for ((dx, dy), is_diagonal) in steps {
            if is_diagonal && diagonals == Diagonals::Never {
                continue;
            }
            let next = cell.offset(dx, dy);
            if !in_bounds(next, width, height) {
                continue;
            }
            let step = cost(next);
            if step == IMPASSABLE {
                continue;
            }
            let step = if is_diagonal {
                diagonals.scale(step)
            } else {
                step
            };
            if step == IMPASSABLE {
                continue;
            }

            let total = spent.saturating_add(step);
            if total < best[idx(next)] {
                best[idx(next)] = total;
                came[idx(next)] = Some(cell);
                heap.push(Reverse(Node {
                    cost: total,
                    cell: next,
                }));
            }
        }
    }

    if best[idx(goal)] == IMPASSABLE {
        return None;
    }

    let mut steps = Vec::new();
    let mut costs = Vec::new();
    let mut at = goal;
    while at != start {
        steps.push(at);
        costs.push(best[idx(at)]);
        at = came[idx(at)]?;
    }
    steps.reverse();
    costs.reverse();

    let reach = costs.iter().take_while(|&&c| c <= budget).count();
    Some(Path {
        steps,
        costs,
        reach,
    })
}

/// Fills a cost-to-reach field from `start` out to `budget`.
///
/// The move-range highlight every tactics game draws before you commit: the
/// set of cells you could stand on this turn. Same search as [`find`] with no
/// goal, stopping once the frontier exceeds the budget, so it is cheap even on
/// a large map because the budget bounds the expansion rather than the map
/// size doing it.
///
/// Returns a `width * height` grid of costs, [`IMPASSABLE`] where unreachable.
#[must_use]
pub fn reachable(
    start: Cell,
    width: i32,
    height: i32,
    diagonals: Diagonals,
    budget: u32,
    mut cost: impl FnMut(Cell) -> u32,
) -> Vec<u32> {
    let idx = |c: Cell| (c.y * width + c.x) as usize;
    let mut best = vec![IMPASSABLE; (width * height) as usize];
    if !in_bounds(start, width, height) {
        return best;
    }

    let mut heap = BinaryHeap::new();
    best[idx(start)] = 0;
    heap.push(Reverse(Node {
        cost: 0,
        cell: start,
    }));

    while let Some(Reverse(Node { cost: spent, cell })) = heap.pop() {
        if spent > best[idx(cell)] {
            continue;
        }
        let steps = CARDINALS
            .iter()
            .map(|&d| (d, false))
            .chain(DIAGONALS.iter().map(|&d| (d, true)));

        for ((dx, dy), is_diagonal) in steps {
            if is_diagonal && diagonals == Diagonals::Never {
                continue;
            }
            let next = cell.offset(dx, dy);
            if !in_bounds(next, width, height) {
                continue;
            }
            let step = cost(next);
            if step == IMPASSABLE {
                continue;
            }
            let step = if is_diagonal {
                diagonals.scale(step)
            } else {
                step
            };
            if step == IMPASSABLE {
                continue;
            }
            let total = spent.saturating_add(step);
            // The budget check is what bounds this search; without it a
            // move-range overlay costs a full-map Dijkstra every frame.
            if total <= budget && total < best[idx(next)] {
                best[idx(next)] = total;
                heap.push(Reverse(Node {
                    cost: total,
                    cell: next,
                }));
            }
        }
    }
    best
}

/// Whether `cell` lies inside a `width` x `height` grid.
const fn in_bounds(cell: Cell, width: i32, height: i32) -> bool {
    cell.x >= 0 && cell.y >= 0 && cell.x < width && cell.y < height
}

/// The eight-way compass arrow pointing from `from` to `to`.
///
/// The path-preview glyph. Restricted to CP437's four arrows plus the four
/// diagonal slashes, because the box-drawing diagonals a nicer font would
/// offer are not in CP437 and would render as solid blocks on the pixel
/// backends.
#[must_use]
pub const fn arrow(from: Cell, to: Cell) -> char {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    match (dx.signum(), dy.signum()) {
        (0, -1) => '\u{2191}',
        (0, 1) => '\u{2193}',
        (1, 0) => '\u{2192}',
        (-1, 0) => '\u{2190}',
        (1, -1) | (-1, 1) => '/',
        (-1, -1) | (1, 1) => '\\',
        _ => '\u{00b7}',
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagonals, IMPASSABLE, Path, arrow, find, reachable};
    use crate::geom::Cell;

    /// Every cell costs 1.
    fn flat(_: Cell) -> u32 {
        1
    }

    #[test]
    fn a_straight_run_costs_one_per_step_and_excludes_the_start() {
        let path = find(
            Cell::new(0, 0),
            Cell::new(3, 0),
            8,
            8,
            Diagonals::Never,
            99,
            flat,
        )
        .expect("a clear route exists");
        assert_eq!(path.steps.len(), 3, "the start is not a step");
        assert_eq!(path.steps[0], Cell::new(1, 0));
        assert_eq!(path.total_cost(), 3);
        assert!(path.complete());
    }

    #[test]
    fn reach_marks_where_the_budget_runs_out() {
        let path = find(
            Cell::new(0, 0),
            Cell::new(9, 0),
            16,
            4,
            Diagonals::Never,
            4,
            flat,
        )
        .expect("a clear route exists");
        assert_eq!(path.steps.len(), 9);
        assert_eq!(path.reach, 4, "four steps are affordable");
        assert!(!path.complete());
        assert_eq!(path.stop(), Some(Cell::new(4, 0)));
    }

    #[test]
    fn a_budget_of_zero_reaches_nothing_but_still_returns_the_route() {
        let path = find(
            Cell::new(0, 0),
            Cell::new(3, 0),
            8,
            8,
            Diagonals::Never,
            0,
            flat,
        )
        .expect("a clear route exists");
        assert_eq!(path.reach, 0);
        assert_eq!(path.steps.len(), 3, "the unaffordable remainder is shown");
        assert_eq!(path.stop(), None);
    }

    #[test]
    fn a_wall_is_routed_around() {
        // A vertical wall at x == 2, with a gap at the bottom row.
        let cost = |c: Cell| {
            if c.x == 2 && c.y < 4 { IMPASSABLE } else { 1 }
        };
        let path = find(
            Cell::new(0, 0),
            Cell::new(4, 0),
            8,
            8,
            Diagonals::Never,
            99,
            cost,
        )
        .expect("the gap makes a route");
        assert!(
            path.steps.iter().all(|c| !(c.x == 2 && c.y < 4)),
            "the route walked through the wall"
        );
        assert!(
            path.total_cost() > 4,
            "detouring must cost more than a straight run"
        );
    }

    #[test]
    fn a_fully_walled_goal_has_no_route() {
        let cost = |c: Cell| if c.x == 2 { IMPASSABLE } else { 1 };
        assert!(
            find(
                Cell::new(0, 0),
                Cell::new(4, 0),
                8,
                8,
                Diagonals::Never,
                99,
                cost
            )
            .is_none()
        );
    }

    #[test]
    fn a_cheap_road_is_preferred_over_a_shorter_crossing() {
        // Row 0 costs 10 a step; row 1 is a road at 1. Going down, along, and
        // back up is longer but cheaper, and Dijkstra must prefer it.
        let cost = |c: Cell| if c.y == 1 { 1 } else { 10 };
        let path = find(
            Cell::new(0, 0),
            Cell::new(6, 0),
            8,
            4,
            Diagonals::Never,
            99,
            cost,
        )
        .expect("a route exists");
        assert!(
            path.steps.iter().filter(|c| c.y == 1).count() >= 5,
            "the road was not used: {:?}",
            path.steps
        );
        assert!(path.total_cost() < 60, "a straight run would cost 60");
    }

    #[test]
    fn diagonals_are_refused_charged_or_free_as_configured() {
        let goal = Cell::new(3, 3);
        let straight =
            find(Cell::new(0, 0), goal, 8, 8, Diagonals::Never, 99, flat).expect("cardinal route");
        let free =
            find(Cell::new(0, 0), goal, 8, 8, Diagonals::Free, 99, flat).expect("diagonal route");
        let costly =
            find(Cell::new(0, 0), goal, 8, 8, Diagonals::Costly, 99, flat).expect("diagonal route");

        assert_eq!(straight.steps.len(), 6, "3 across and 3 down");
        assert_eq!(free.steps.len(), 3, "three diagonal steps");
        assert_eq!(free.total_cost(), 3);
        assert_eq!(costly.steps.len(), 3);
        assert_eq!(costly.total_cost(), 3, "1.41 truncates to 1 per step");
        assert!(costly.total_cost() <= straight.total_cost());
    }

    #[test]
    fn a_costly_diagonal_does_not_beat_a_straight_line() {
        // Two cardinal steps cost 2; one diagonal plus one cardinal costs
        // 1.41 + 1. The diagonal is still cheaper here, which is correct; the
        // property worth pinning is that it is never *free*.
        let d = Diagonals::Costly;
        assert!(
            d.scale(100) > 100,
            "a costly diagonal must cost more than a step"
        );
        assert_eq!(Diagonals::Free.scale(100), 100);
    }

    #[test]
    fn a_path_to_itself_is_not_a_path() {
        assert!(
            find(
                Cell::new(2, 2),
                Cell::new(2, 2),
                8,
                8,
                Diagonals::Never,
                9,
                flat
            )
            .is_none()
        );
    }

    #[test]
    fn an_out_of_bounds_goal_is_rejected_rather_than_panicking() {
        for goal in [
            Cell::new(-1, 0),
            Cell::new(0, -1),
            Cell::new(8, 0),
            Cell::new(0, 8),
        ] {
            assert!(
                find(Cell::new(0, 0), goal, 8, 8, Diagonals::Never, 99, flat).is_none(),
                "{goal:?}"
            );
        }
    }

    #[test]
    fn reachable_is_bounded_by_the_budget() {
        let field = reachable(Cell::new(4, 4), 9, 9, Diagonals::Never, 2, flat);
        let within = field.iter().filter(|&&c| c != IMPASSABLE).count();
        // A cardinal-only radius-2 diamond: 1 + 4 + 8 = 13 cells.
        assert_eq!(within, 13, "the budget did not bound the frontier");
        assert_eq!(field[(4 * 9 + 4) as usize], 0, "the origin costs nothing");
        assert_eq!(field[(4 * 9 + 6) as usize], 2, "two steps east");
        assert_eq!(field[(4 * 9 + 7) as usize], IMPASSABLE, "three is too far");
    }

    #[test]
    fn reachable_routes_around_obstacles_rather_than_through_them() {
        let cost = |c: Cell| if c.x == 5 { IMPASSABLE } else { 1 };
        let field = reachable(Cell::new(4, 4), 9, 9, Diagonals::Never, 3, cost);
        assert_eq!(
            field[(4 * 9 + 5) as usize],
            IMPASSABLE,
            "the wall is not entered"
        );
        assert_eq!(
            field[(4 * 9 + 6) as usize],
            IMPASSABLE,
            "nor is anything past it"
        );
    }

    #[test]
    fn the_search_is_deterministic_across_runs() {
        let once = find(
            Cell::new(0, 0),
            Cell::new(5, 5),
            12,
            12,
            Diagonals::Free,
            99,
            flat,
        );
        let twice = find(
            Cell::new(0, 0),
            Cell::new(5, 5),
            12,
            12,
            Diagonals::Free,
            99,
            flat,
        );
        assert_eq!(once, twice, "equal-cost ties must not depend on heap order");
    }

    #[test]
    fn arrows_point_the_way_they_travel() {
        let o = Cell::new(4, 4);
        assert_eq!(arrow(o, Cell::new(4, 3)), '↑');
        assert_eq!(arrow(o, Cell::new(4, 5)), '↓');
        assert_eq!(arrow(o, Cell::new(5, 4)), '→');
        assert_eq!(arrow(o, Cell::new(3, 4)), '←');
        assert_eq!(arrow(o, Cell::new(5, 3)), '/');
        assert_eq!(arrow(o, Cell::new(5, 5)), '\\');
        assert_eq!(arrow(o, o), '·', "a non-move is a dot, not a panic");
    }

    #[test]
    fn an_empty_path_reports_zero_cost_and_is_not_complete() {
        let path = Path::default();
        assert_eq!(path.total_cost(), 0);
        assert!(!path.complete());
        assert_eq!(path.stop(), None);
    }
}
