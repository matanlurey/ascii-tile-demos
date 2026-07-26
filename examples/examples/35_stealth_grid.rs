//! 35: Stealth grid -- Invisible Inc's turn-vs-real-time split, on a character
//! grid.
//!
//! Invisible Inc's whole design rests on one asymmetry: your agents act in
//! discrete turns, but the guards and cameras that hunt them sweep the floor
//! continuously, so the map you planned a move against is not quite the map
//! you execute it on. This demo is that asymmetry with nothing else around
//! it: a generated facility, two agents with an action-point budget, and a
//! handful of sentinels whose vision cones are the only real threat.
//!
//! Techniques on show:
//!
//! - **Wall-clipped vision cones** ([`cone_cells`]): [`tilekit::fov::shadowcast`]
//!   answers "what can this sentinel see at all", and a facing-angle filter
//!   narrows that circle to a cone. Doing it in that order, rather than
//!   casting one ray per cone cell, is what makes a cone that a wall
//!   correctly clips instead of a cone that a wall's corner leaks past: the
//!   shadowcast has already solved occlusion once for the whole radius, so
//!   the angle test never has to reason about walls at all.
//! - **Two shadings for one number** ([`StealthGrid::draw_facility`]): every
//!   sentinel is evaluated twice, once at the current instant and once
//!   [`PREDICT_SECONDS`] into the future, using the same continuous motion
//!   function both times. "Seen now" and "will be seen next turn" are drawn
//!   with different saturation and a different ramp of
//!   [`tilekit::glyphs::SHADE`] rather than folded into one color, because
//!   they mean different things to act on: a tile seen now is a tile you are
//!   already standing in during someone's turn, a tile that will be seen next
//!   is a tile that is safe to pass through now but not to linger in.
//! - **A two-tap confirm on exactly the dangerous tiles**
//!   ([`StealthGrid::handle_tile_tap`]): [`Path::reach`]/[`tilekit::path::reachable`]
//!   computes every tile an agent can afford this turn, and each of those
//!   tiles is cross-referenced against the cone shading above. A tile outside
//!   every cone moves on the first tap, because there is nothing to protect
//!   the player from. A tile inside one requires the same tile tapped twice:
//!   the first tap is a preview, not a commitment, so a thumb that lands one
//!   cell off from where it meant to does not walk an agent into a guard's
//!   line of sight.
//! - **Unlimited undo, one commit point** ([`StealthGrid::undo`]): every move
//!   this turn is popped from a stack, in Into the Breach's model -- movement
//!   is free to reconsider until the turn ends, because the cost of a wrong
//!   move should be the time spent looking at the board, not the move itself.
//! - **[`ui::touch::Pointer`]/[`ui::touch::Hotspots`]**: the facility is
//!   tap-select-then-tap-target (dense boards hide their own targets under a
//!   finger), Undo and End Turn sit in opposite thumb-zone corners so a panic
//!   tap cannot hit both, and dragging anywhere on the map pans it.
//!
//! ```sh
//! cargo run --example 35_stealth_grid --features crossterm
//! cargo run --example 35_stealth_grid --features software
//! cargo run --example 35_stealth_grid --features gl
//! cargo run --example 35_stealth_grid  # headless, prints a few frames
//! ```

use core::f32::consts::PI;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Log, Span};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, mask4};
use tilekit::fov::shadowcast;
use tilekit::geom::Cell;
use tilekit::glyphs::{SHADE, ramp_glyph};
use tilekit::noise::Rng;
use tilekit::palette::{mix, rgb};
use tilekit::path::{self, Diagonals};

/// Facility width, in tiles. Kept small enough that a whole floor plan is
/// panned across in a handful of drags rather than dozens: the point on show
/// is the cones and the undo model, not a sprawling map.
const FACILITY_W: i32 = 12;
/// See [`FACILITY_W`].
const FACILITY_H: i32 = 8;

/// A facility tile's footprint on screen, in cells.
///
/// The brief's floor: every board tile is a multi-cell token, never a single
/// glyph, because a single glyph is not a legal touch target. 7x3 is the
/// smallest footprint that also leaves room for a name tag abbreviation next
/// to the unit glyph and an AP-cost digit in the corner without either
/// crowding the other off the tile.
const TILE_W: i32 = 7;
/// See [`TILE_W`].
const TILE_H: i32 = 3;

/// Vision radius, in tiles, for every sentinel. Fixed rather than per-type:
/// the interesting variable in this demo is the cone's *angle* and whether a
/// wall clips it, not how far any one guard can see.
const CONE_RADIUS: i32 = 6;
/// Half-angle of a vision cone, in radians. About 64 degrees of total field,
/// narrow enough that stepping to either side of a corridor visibly matters.
const CONE_HALF_ANGLE: f32 = 0.56;

/// How far into the future, in simulated seconds, the "will be seen next
/// turn" shading looks. Guards and cameras move continuously (see the module
/// docs), so there is no discrete "next turn" state to query; this constant
/// stands in for "about how much sentinel motion one player turn buys them",
/// and is what makes the predicted cone a genuine forecast rather than a
/// cosmetic dimmer copy of the current one.
const PREDICT_SECONDS: f32 = 2.6;

/// Alarm gained at the end of every turn.
const ALARM_PER_TURN: f32 = 9.0;
/// Alarm thresholds that escalate the facility: one more sentinel goes live
/// and every remaining sentinel moves/sweeps faster. Three stages is enough
/// to make the clock legible without turning the demo into a spreadsheet.
const ESCALATION: [f32; 3] = [25.0, 55.0, 85.0];

/// Starting action points per agent per turn.
const START_AP: i32 = 5;

/// A single facility tile's role.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TileKind {
    /// Outside the generated footprint: not drawn as floor, not enterable.
    Void,
    /// Open ground: walkable, sight passes through freely.
    Floor,
    /// Solid: blocks both movement and every sentinel's line of sight.
    Wall,
    /// A doorway: walkable exactly like floor, and -- unlike a real Invisible
    /// Inc door -- always open, so it does not block sight either. Modelling
    /// closed doors would need a state machine per door and buys this demo
    /// nothing it isn't already showing with walls; what a door adds here is
    /// purely the read "this is the threshold between two rooms".
    Door,
}

impl TileKind {
    const fn walkable(self) -> bool {
        matches!(self, Self::Floor | Self::Door)
    }

    const fn blocks_sight(self) -> bool {
        matches!(self, Self::Wall | Self::Void)
    }
}

/// What a tile is for, beyond its structural role.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Objective {
    None,
    Safe,
    Terminal,
    Exit,
}

/// The generated facility: a tile grid plus the three objective tiles cut
/// into it.
struct Facility {
    tiles: Vec<TileKind>,
    /// Precomputed wall glyph per tile, so drawing never recomputes the
    /// autotile mask per screen cell.
    wall_glyph: Vec<char>,
    objectives: Vec<(Cell, Objective)>,
    spawns: [Cell; 2],
}

impl Facility {
    const fn index(x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= FACILITY_W || y >= FACILITY_H {
            return None;
        }
        Some((y * FACILITY_W + x) as usize)
    }

    fn kind(&self, x: i32, y: i32) -> TileKind {
        Self::index(x, y).map_or(TileKind::Void, |i| self.tiles[i])
    }

    fn walkable(&self, cell: Cell) -> bool {
        self.kind(cell.x, cell.y).walkable()
    }

    fn blocks_sight(&self, x: i32, y: i32) -> bool {
        self.kind(x, y).blocks_sight()
    }

    fn objective_at(&self, cell: Cell) -> Objective {
        self.objectives
            .iter()
            .find(|(c, _)| *c == cell)
            .map_or(Objective::None, |(_, o)| *o)
    }

    /// Movement cost for [`tilekit::path`]: 1 to enter any walkable tile,
    /// impassable otherwise. Every tile costs the same; the interesting
    /// budget in this demo is AP spent on distance, not terrain difficulty.
    fn move_cost(&self, cell: Cell) -> u32 {
        if self.walkable(cell) {
            1
        } else {
            path::IMPASSABLE
        }
    }

    /// Generates a facility by BSP room splitting, in the same spirit as
    /// `21_deck_plan`'s deck: split until leaves are room-sized, carve a room
    /// inset inside each leaf, connect siblings with a straight corridor, then
    /// derive walls from floor adjacency so two rooms sharing an edge share
    /// one wall rather than acquiring two. Deterministic in `seed` alone, so
    /// the frame-doubling determinism test sees the same facility both times.
    fn generate(seed: u32) -> Self {
        let (tiles, wall_glyph) = Self::generate_tiles(seed);
        let (objectives, spawns) = Self::place_objectives(seed, &tiles);
        Self {
            tiles,
            wall_glyph,
            objectives,
            spawns,
        }
    }

    /// The tile grid and its precomputed wall glyphs. Split from
    /// [`generate`](Self::generate) purely to stay under the crate's
    /// function-length lint; the two halves are still one deterministic
    /// pipeline run from one seed.
    fn generate_tiles(seed: u32) -> (Vec<TileKind>, Vec<char>) {
        let mut rng = Rng::new(seed);
        let mut tiles = vec![TileKind::Void; (FACILITY_W * FACILITY_H) as usize];
        let mut leaves = Vec::new();
        split(
            (1, 1, FACILITY_W - 2, FACILITY_H - 2),
            &mut rng,
            &mut leaves,
        );

        let mut rooms = Vec::new();
        for &(lx, ly, lw, lh) in &leaves {
            if lw < 4 || lh < 3 {
                continue;
            }
            let rw = lw - 1;
            let rh = lh - 1;
            let rx = lx + rng.next_below((lw - rw).max(1) as u32) as i32;
            let ry = ly + rng.next_below((lh - rh).max(1) as u32) as i32;
            for y in ry..ry + rh {
                for x in rx..rx + rw {
                    if let Some(i) = Self::index(x, y) {
                        tiles[i] = TileKind::Floor;
                    }
                }
            }
            rooms.push((rx, ry, rw, rh));
        }

        // Connect each pair of rooms that sit close enough to share a
        // corridor; carving before deriving walls means the wall pass below
        // only ever has to enclose floor that already exists, never punch a
        // doorway through solid rock.
        let mut doors = Vec::new();
        for i in 0..rooms.len() {
            for j in (i + 1)..rooms.len() {
                if let Some(c) = corridor(rooms[i], rooms[j], &mut rng) {
                    for (x, y) in c.span {
                        if let Some(idx) = Self::index(x, y) {
                            tiles[idx] = TileKind::Floor;
                        }
                    }
                    doors.push(c.door_a);
                    doors.push(c.door_b);
                }
            }
        }

        for y in 0..FACILITY_H {
            for x in 0..FACILITY_W {
                let Some(i) = Self::index(x, y) else {
                    continue;
                };
                if tiles[i] == TileKind::Floor {
                    continue;
                }
                let touches_floor = [(0, -1), (1, 0), (0, 1), (-1, 0)].iter().any(|&(dx, dy)| {
                    Self::index(x + dx, y + dy).is_some_and(|j| tiles[j] == TileKind::Floor)
                });
                if touches_floor {
                    tiles[i] = TileKind::Wall;
                }
            }
        }
        for (x, y) in doors {
            if let Some(i) = Self::index(x, y) {
                tiles[i] = TileKind::Door;
            }
        }

        let wall_glyph = (0..tiles.len())
            .map(|i| {
                let (x, y) = (i as i32 % FACILITY_W, i as i32 / FACILITY_W);
                let connects = |kx: i32, ky: i32| {
                    matches!(
                        Self::index(kx, ky).map(|j| tiles[j]),
                        Some(TileKind::Wall | TileKind::Door)
                    )
                };
                let mask = mask4([
                    connects(x, y - 1),
                    connects(x + 1, y),
                    connects(x, y + 1),
                    connects(x - 1, y),
                ]);
                BOX_SINGLE[(mask & 0x0F) as usize]
            })
            .collect();

        (tiles, wall_glyph)
    }

    /// Objective and spawn tiles, placed on distinct room centers chosen
    /// deterministically from the seeded RNG so two renders of one seed
    /// always agree on where everything is.
    fn place_objectives(seed: u32, tiles: &[TileKind]) -> (Vec<(Cell, Objective)>, [Cell; 2]) {
        let mut rng = Rng::new(seed ^ 0x5eed);
        let mut rooms = Vec::new();
        for y in 0..FACILITY_H {
            for x in 0..FACILITY_W {
                if tiles[(y * FACILITY_W + x) as usize] == TileKind::Floor {
                    rooms.push(Cell::new(x, y));
                }
            }
        }
        // Thin the raw floor-tile list down to well-separated candidate
        // centers rather than every floor cell, so objectives do not cluster
        // inside one large room.
        let mut centers: Vec<Cell> = rooms.into_iter().step_by(7).collect();
        if centers.is_empty() {
            centers.push(Cell::new(1, 1));
        }
        // A stable order the RNG then shuffles by swapping, rather than a
        // `HashMap`-backed shuffle: determinism here is load-bearing, not
        // incidental.
        for i in (1..centers.len()).rev() {
            let j = rng.next_below((i + 1) as u32) as usize;
            centers.swap(i, j);
        }

        let mut objectives = Vec::new();
        let mut spawns = [centers[0]; 2];
        let slots = [Objective::Safe, Objective::Terminal, Objective::Exit];
        for (slot, &center) in slots.iter().zip(centers.iter()) {
            objectives.push((center, *slot));
        }
        for (i, spawn) in spawns.iter_mut().enumerate() {
            *spawn = centers[(slots.len() + i).min(centers.len() - 1)];
        }

        (objectives, spawns)
    }
}

/// Recursively splits `(x, y, w, h)` into BSP leaves at least 6x5, the
/// smallest size that still fits a room with a one-tile margin plus the
/// eventual wall ring.
fn split(area: (i32, i32, i32, i32), rng: &mut Rng, leaves: &mut Vec<(i32, i32, i32, i32)>) {
    const MIN: i32 = 6;
    let (x, y, w, h) = area;
    let can_w = w > MIN * 2;
    let can_h = h > MIN * 2;
    if !can_w && !can_h {
        leaves.push(area);
        return;
    }
    let split_w = if can_w && can_h { w > h } else { can_w };
    if split_w {
        let at = MIN + rng.next_below((w - MIN * 2).max(1) as u32) as i32;
        split((x, y, at, h), rng, leaves);
        split((x + at, y, w - at, h), rng, leaves);
    } else {
        let at = MIN + rng.next_below((h - MIN * 2).max(1) as u32) as i32;
        split((x, y, w, at), rng, leaves);
        split((x, y + at, w, h - at), rng, leaves);
    }
}

/// A generated corridor: its floor span plus the two door tiles where it
/// crosses each room's own wall ring.
struct Corridor {
    span: Vec<(i32, i32)>,
    door_a: (i32, i32),
    door_b: (i32, i32),
}

/// Finds a straight corridor between two cardinally-adjacent rooms, if any.
fn corridor(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32), rng: &mut Rng) -> Option<Corridor> {
    const MAX_GAP: i32 = 4;
    let (al, at, aw, ah) = a;
    let (bl, bt, bw, bh) = b;
    let (ar, ab) = (al + aw, at + ah);
    let (br, bb) = (bl + bw, bt + bh);

    let vertical_overlap = at.max(bt)..ab.min(bb);
    if !vertical_overlap.is_empty() {
        let (lo, hi) = if ar <= bl {
            (ar, bl)
        } else if br <= al {
            (br, al)
        } else {
            (0, 0)
        };
        if hi > lo && hi - lo <= MAX_GAP {
            let y0 = vertical_overlap.start;
            let y1 = vertical_overlap.end - 1;
            let y = y0 + rng.next_below((y1 - y0 + 1).max(1) as u32) as i32;
            let span = (lo..hi).map(|x| (x, y)).collect();
            return Some(Corridor {
                span,
                door_a: (lo, y),
                door_b: (hi - 1, y),
            });
        }
    }
    let horizontal_overlap = al.max(bl)..ar.min(br);
    if !horizontal_overlap.is_empty() {
        let (lo, hi) = if ab <= bt {
            (ab, bt)
        } else if bb <= at {
            (bb, at)
        } else {
            (0, 0)
        };
        if hi > lo && hi - lo <= MAX_GAP {
            let x0 = horizontal_overlap.start;
            let x1 = horizontal_overlap.end - 1;
            let x = x0 + rng.next_below((x1 - x0 + 1).max(1) as u32) as i32;
            let span = (lo..hi).map(|y| (x, y)).collect();
            return Some(Corridor {
                span,
                door_a: (x, lo),
                door_b: (x, hi - 1),
            });
        }
    }
    None
}

/// A guard's fixed patrol, or a camera's fixed post.
enum Route {
    /// Cycles through these waypoints in order, one tile per leg.
    Patrol(Vec<Cell>),
    /// Stands at one tile and sweeps its facing back and forth.
    Fixed(Cell),
}

/// A guard or a camera: the two vary only in whether they walk.
struct Sentinel {
    route: Route,
    /// Tiles per second along a patrol leg.
    speed: f32,
    /// Facing at the midpoint of a camera's sweep, in radians.
    sweep_center: f32,
    /// Half-width of a camera's sweep, in radians.
    sweep_amplitude: f32,
    /// Seconds per full back-and-forth sweep cycle.
    sweep_period: f32,
    /// Escalation stage (see [`ESCALATION`]) at which this sentinel joins
    /// the floor. `0` means active from the start.
    active_at_stage: usize,
}

impl Sentinel {
    /// Position and facing at simulated time `t`, with `speed_mult` applied
    /// (escalation makes every active sentinel faster, not just the new
    /// ones). The same function computes both "now" (`t = time`) and "next
    /// turn" (`t = time + PREDICT_SECONDS`), which is what keeps the two
    /// cones honest reflections of one continuous motion rather than two
    /// independently-tuned effects.
    fn state_at(&self, t: f32, speed_mult: f32) -> (Cell, f32) {
        match &self.route {
            Route::Fixed(cell) => {
                let period = (self.sweep_period / speed_mult).max(0.05);
                let phase = (t / period) * core::f32::consts::TAU;
                let facing = self.sweep_amplitude.mul_add(phase.sin(), self.sweep_center);
                (*cell, facing)
            }
            Route::Patrol(route) if route.len() >= 2 => {
                let leg_seconds = (1.0 / (self.speed * speed_mult).max(0.01)).max(0.05);
                let progress = (t / leg_seconds).rem_euclid(route.len() as f32);
                let idx = progress.floor() as usize % route.len();
                let next = (idx + 1) % route.len();
                let frac = progress.fract();
                let from = route[idx];
                let to = route[next];
                // Snapping the reported cell at the segment midpoint (rather
                // than interpolating a fractional position that a tile grid
                // has no notion of) gives one discrete visibility change per
                // leg, timed to when the walking token is roughly overhead.
                let cell = if frac < 0.5 { from } else { to };
                let facing = f32::atan2((to.y - from.y) as f32, (to.x - from.x) as f32);
                (cell, facing)
            }
            Route::Patrol(route) => (route.first().copied().unwrap_or_default(), 0.0),
        }
    }
}

/// Every tile a sentinel can see at time `t`, paired with a `0.0..=1.0`
/// strength that falls off with distance from the sentinel.
///
/// [`shadowcast`] does the hard part: it already refuses to report anything
/// behind a wall, symmetrically and without per-ray special-casing. Cutting
/// that circle down to a forward-facing cone is then a single angle
/// comparison per visible tile -- cheap precisely because occlusion was
/// solved once for the whole radius rather than once per cone cell, which is
/// what a naive "cast a ray per cell inside the cone" approach would have to
/// redo.
fn cone_cells(facility: &Facility, origin: Cell, facing: f32, radius: i32) -> Vec<(Cell, f32)> {
    let mut out = Vec::new();
    shadowcast(
        origin.x,
        origin.y,
        radius,
        |x, y| facility.blocks_sight(x, y),
        |x, y| {
            if x == origin.x && y == origin.y {
                return;
            }
            let (dx, dy) = ((x - origin.x) as f32, (y - origin.y) as f32);
            let dist = dx.hypot(dy);
            if dist > radius as f32 {
                return;
            }
            let angle = dy.atan2(dx);
            if angle_diff(angle, facing).abs() <= CONE_HALF_ANGLE {
                let strength = (1.0 - dist / radius as f32).clamp(0.0, 1.0);
                out.push((Cell::new(x, y), strength));
            }
        },
    );
    out
}

/// Smallest signed difference between two angles, in `-PI..=PI`.
fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(core::f32::consts::TAU);
    if d > PI {
        d - core::f32::consts::TAU
    } else {
        d
    }
}

/// One of the two player-controlled agents.
struct Agent {
    name: &'static str,
    pos: Cell,
    ap: i32,
}

/// One committed move this turn, kept so [`StealthGrid::undo`] can reverse it.
struct MoveRecord {
    agent: usize,
    prev_pos: Cell,
    prev_ap: i32,
}

/// What a tap on the facility map means, resolved by [`StealthGrid::handle_tile_tap`].
#[derive(Clone, Copy)]
struct TileHit(Cell);

/// A bottom-of-screen control.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Undo,
    EndTurn,
}

/// Demo state: the generated facility, its sentinels, two agents, the
/// undo/pending-move bookkeeping their moves need, and the alarm clock.
pub struct StealthGrid {
    facility: Facility,
    seed: u32,
    sentinels: Vec<Sentinel>,
    agents: [Agent; 2],
    selected: usize,
    /// A dangerous tile awaiting its confirming second tap; see the module
    /// docs for why only dangerous tiles need one.
    pending: Option<(usize, Cell)>,
    undo_stack: Vec<MoveRecord>,
    alarm: f32,
    stage: usize,
    turn: u32,
    time: f32,
    scroll: (i32, i32),
    pointer: Pointer,
    tile_hotspots: Hotspots<TileHit>,
    control_hotspots: Hotspots<Control>,
    log: Log,
    fps: FpsMeter,
}

impl Default for StealthGrid {
    fn default() -> Self {
        let seed = 35;
        let facility = Facility::generate(seed);

        let sentinels = vec![
            Sentinel {
                route: Route::Patrol(vec![
                    Cell::new(3, 2),
                    Cell::new(6, 2),
                    Cell::new(6, 4),
                    Cell::new(3, 4),
                ]),
                speed: 0.6,
                sweep_center: 0.0,
                sweep_amplitude: 0.0,
                sweep_period: 1.0,
                active_at_stage: 0,
            },
            Sentinel {
                route: Route::Fixed(Cell::new(9, 5)),
                speed: 0.0,
                sweep_center: -core::f32::consts::FRAC_PI_2,
                sweep_amplitude: 0.9,
                sweep_period: 6.0,
                active_at_stage: 0,
            },
            Sentinel {
                route: Route::Patrol(vec![Cell::new(2, 6), Cell::new(2, 3), Cell::new(5, 3)]),
                speed: 0.5,
                sweep_center: 0.0,
                sweep_amplitude: 0.0,
                sweep_period: 1.0,
                active_at_stage: 1,
            },
            Sentinel {
                route: Route::Fixed(Cell::new(4, 6)),
                speed: 0.0,
                sweep_center: PI,
                sweep_amplitude: 0.7,
                sweep_period: 4.5,
                active_at_stage: 2,
            },
        ];

        let agents = [
            Agent {
                name: "Kai",
                pos: facility.spawns[0],
                ap: START_AP,
            },
            Agent {
                name: "Nia",
                pos: facility.spawns[1],
                ap: START_AP,
            },
        ];

        let mut log = Log::new(48);
        log.push("Infiltration begins. Two agents, one alarm clock.", ui::FG);
        log.push(
            "Tap an agent, then a tile. Two taps confirm a dangerous one.",
            ui::DIM,
        );

        Self {
            facility,
            seed,
            sentinels,
            agents,
            selected: 0,
            pending: None,
            undo_stack: Vec::new(),
            alarm: 0.0,
            stage: 0,
            turn: 1,
            time: 0.0,
            scroll: (-4, -4),
            pointer: Pointer::new(),
            tile_hotspots: Hotspots::new(),
            control_hotspots: Hotspots::new(),
            log,
            fps: FpsMeter::new(),
        }
    }
}

impl StealthGrid {
    fn speed_mult(&self) -> f32 {
        (self.stage as f32).mul_add(0.35, 1.0)
    }

    fn active_sentinels(&self) -> impl Iterator<Item = &Sentinel> {
        self.sentinels
            .iter()
            .filter(move |s| s.active_at_stage <= self.stage)
    }

    /// Cone strength per tile at `t`, as a dense `FACILITY_W * FACILITY_H`
    /// array rather than a set. Dense and additive (`max` across sentinels)
    /// so overlapping cones combine deterministically regardless of
    /// iteration order -- the alternative, a hash set of visible cells, would
    /// make "which sentinel lit this tile" depend on insertion order, which
    /// is exactly the kind of nondeterminism the gallery's frame-doubling
    /// test exists to catch.
    fn cone_field(&self, t: f32) -> Vec<f32> {
        let mut field = vec![0.0f32; (FACILITY_W * FACILITY_H) as usize];
        let mult = self.speed_mult();
        for sentinel in self.active_sentinels() {
            let (origin, facing) = sentinel.state_at(t, mult);
            for (cell, strength) in cone_cells(&self.facility, origin, facing, CONE_RADIUS) {
                if let Some(i) = Facility::index(cell.x, cell.y) {
                    field[i] = field[i].max(strength);
                }
            }
        }
        field
    }

    fn reachable_field(&self, agent: usize) -> Vec<u32> {
        let start = self.agents[agent].pos;
        path::reachable(
            start,
            FACILITY_W,
            FACILITY_H,
            Diagonals::Never,
            self.agents[agent].ap.max(0) as u32,
            |c| self.facility.move_cost(c),
        )
    }

    fn agent_at(&self, cell: Cell) -> Option<usize> {
        self.agents.iter().position(|a| a.pos == cell)
    }

    fn commit_move(&mut self, agent: usize, target: Cell, cost: u32) {
        let prev_pos = self.agents[agent].pos;
        let prev_ap = self.agents[agent].ap;
        self.undo_stack.push(MoveRecord {
            agent,
            prev_pos,
            prev_ap,
        });
        self.agents[agent].pos = target;
        self.agents[agent].ap -= cost as i32;

        match self.facility.objective_at(target) {
            Objective::Safe => self.log.push(
                format!("{} cracks the safe.", self.agents[agent].name),
                ui::ACCENT,
            ),
            Objective::Terminal => self.log.push(
                format!("{} hacks the terminal.", self.agents[agent].name),
                ui::ACCENT,
            ),
            Objective::Exit => self.log.push(
                format!("{} reaches the exit.", self.agents[agent].name),
                rgb(120, 196, 158),
            ),
            Objective::None => {}
        }
    }

    fn undo(&mut self) {
        let Some(record) = self.undo_stack.pop() else {
            return;
        };
        self.agents[record.agent].pos = record.prev_pos;
        self.agents[record.agent].ap = record.prev_ap;
        self.pending = None;
        self.log.push("Move undone.", ui::DIM);
    }

    fn end_turn(&mut self) {
        for agent in &mut self.agents {
            agent.ap = START_AP;
        }
        self.undo_stack.clear();
        self.pending = None;
        self.turn += 1;
        self.alarm = (self.alarm + ALARM_PER_TURN).min(100.0);
        let new_stage = ESCALATION.iter().filter(|&&t| self.alarm >= t).count();
        if new_stage > self.stage {
            self.stage = new_stage;
            self.log.push(
                format!(
                    "Alarm escalates -- stage {}. More eyes on the floor.",
                    self.stage
                ),
                rgb(216, 108, 84),
            );
        }
        self.log
            .push(format!("Turn {} begins.", self.turn), ui::DIM);
    }

    fn is_dangerous(cell: Cell, now: &[f32], next: &[f32]) -> bool {
        Facility::index(cell.x, cell.y).is_some_and(|i| now[i] > 0.0 || next[i] > 0.0)
    }

    fn handle_tile_tap(&mut self, cell: Cell, now: &[f32], next: &[f32]) {
        if let Some(idx) = self.agent_at(cell) {
            self.selected = idx;
            self.pending = None;
            return;
        }
        let field = self.reachable_field(self.selected);
        let Some(i) = Facility::index(cell.x, cell.y) else {
            return;
        };
        let cost = field[i];
        if cost == path::IMPASSABLE || cell == self.agents[self.selected].pos {
            self.pending = None;
            return;
        }

        let dangerous = Self::is_dangerous(cell, now, next);
        if dangerous && self.pending != Some((self.selected, cell)) {
            // First tap on a tile inside a cone previews it and stops there;
            // see the module docs for why only this case needs a second tap.
            self.pending = Some((self.selected, cell));
            return;
        }
        self.commit_move(self.selected, cell, cost);
        self.pending = None;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.selected = (self.selected + 1) % self.agents.len(),
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                let (dx, dy) = match code {
                    KeyCode::Up => (0, -1),
                    KeyCode::Down => (0, 1),
                    KeyCode::Left => (-1, 0),
                    _ => (1, 0),
                };
                let pos = self.agents[self.selected].pos;
                self.pending = Some((self.selected, pos.offset(dx, dy)));
            }
            KeyCode::Enter => {
                if let Some((idx, target)) = self.pending {
                    let field = self.reachable_field(idx);
                    if let Some(i) = Facility::index(target.x, target.y) {
                        let cost = field[i];
                        if cost != path::IMPASSABLE {
                            self.commit_move(idx, target, cost);
                        }
                    }
                    self.pending = None;
                }
            }
            KeyCode::Char('u' | 'U') => self.undo(),
            KeyCode::Char('e' | 'E') => self.end_turn(),
            _ => {}
        }
    }

    /// Applies drag-to-pan, and returns any tap so the caller can resolve it
    /// against whichever hotspot set was registered this frame.
    fn handle_pointer(&mut self) -> Gesture {
        let gesture = self.pointer.take();
        if gesture.delta != (0, 0) {
            self.scroll.0 -= gesture.delta.0;
            self.scroll.1 -= gesture.delta.1;
        }
        gesture
    }

    fn map_screen_origin(&self, tx: i32, ty: i32, area: Rect) -> (i32, i32) {
        (
            i32::from(area.left()) + tx * TILE_W - self.scroll.0,
            i32::from(area.top()) + ty * TILE_H - self.scroll.1,
        )
    }

    fn put(surface: &mut Surface<'_>, area: Rect, x: i32, y: i32, glyph: char, style: Style) {
        if x < i32::from(area.left())
            || y < i32::from(area.top())
            || x >= i32::from(area.right())
            || y >= i32::from(area.bottom())
        {
            return;
        }
        surface.put((x as u16, y as u16), glyph, style);
    }

    fn draw_facility(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < TILE_W as u16 || area.height() == 0 {
            return;
        }
        self.tile_hotspots.clear();
        let now = self.cone_field(self.time);
        let next = self.cone_field(self.time + PREDICT_SECONDS);
        let selected_field = self.reachable_field(self.selected);

        for ty in 0..FACILITY_H {
            for tx in 0..FACILITY_W {
                let (ox, oy) = self.map_screen_origin(tx, ty, area);
                if ox + TILE_W <= i32::from(area.left())
                    || ox >= i32::from(area.right())
                    || oy + TILE_H <= i32::from(area.top())
                    || oy >= i32::from(area.bottom())
                {
                    continue;
                }

                if ox >= i32::from(area.left())
                    && oy >= i32::from(area.top())
                    && ox + TILE_W <= i32::from(area.right())
                    && oy + TILE_H <= i32::from(area.bottom())
                {
                    self.tile_hotspots.push(
                        Rect::new(ox as u16, oy as u16, TILE_W as u16, TILE_H as u16),
                        TileHit(Cell::new(tx, ty)),
                    );
                }

                self.draw_tile(surface, area, tx, ty, ox, oy, &now, &next, &selected_field);
            }
        }

        self.draw_sentinel_tokens(surface, area);
        self.draw_agent_tokens(surface, area);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_tile(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tx: i32,
        ty: i32,
        ox: i32,
        oy: i32,
        now: &[f32],
        next: &[f32],
        selected_field: &[u32],
    ) {
        let kind = self.facility.kind(tx, ty);
        let idx = Facility::index(tx, ty);

        let base_bg = match kind {
            TileKind::Void => rgb(9, 10, 15),
            TileKind::Floor => rgb(20, 26, 30),
            TileKind::Door => rgb(30, 26, 16),
            TileKind::Wall => rgb(12, 13, 18),
        };

        // Cone shading: "seen now" gets the saturated red half of the ramp,
        // "will be seen next turn" gets a dimmer amber half, so the two never
        // collapse into "some shade of danger" -- the whole point of the
        // distinction is that they call for different responses.
        let (strength, danger_color) = idx.map_or((0.0, base_bg), |i| {
            if now[i] > 0.0 {
                (now[i], rgb(198, 62, 58))
            } else if next[i] > 0.0 {
                (next[i] * 0.7, rgb(196, 132, 64))
            } else {
                (0.0, base_bg)
            }
        });

        let tinted_bg = if strength > 0.0 && kind.walkable() {
            mix(base_bg, danger_color, strength.mul_add(0.45, 0.25))
        } else {
            base_bg
        };

        let reachable = idx.is_some_and(|i| selected_field[i] != path::IMPASSABLE);
        let cost = idx.map_or(path::IMPASSABLE, |i| selected_field[i]);
        let is_start = Cell::new(tx, ty) == self.agents[self.selected].pos;
        let dangerous_reach = reachable && strength > 0.0;
        let border_color = if !reachable || is_start {
            None
        } else if dangerous_reach {
            Some(rgb(224, 96, 90))
        } else {
            Some(rgb(108, 196, 168))
        };

        self.fill_tile_block(
            surface,
            area,
            tx,
            ty,
            ox,
            oy,
            kind,
            strength,
            danger_color,
            tinted_bg,
            border_color,
        );
        Self::draw_reach_frame(surface, area, ox, oy, border_color, tinted_bg, cost);
        self.draw_objective_marker(surface, area, tx, ty, ox, oy, tinted_bg);
    }

    /// Fills one tile's block with its base texture: solid wall glyph, floor
    /// with an optional cone-shade glyph, or blank. Split out of
    /// [`draw_tile`](Self::draw_tile) to stay under the crate's
    /// function-length lint.
    #[allow(clippy::too_many_arguments)]
    fn fill_tile_block(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tx: i32,
        ty: i32,
        ox: i32,
        oy: i32,
        kind: TileKind,
        strength: f32,
        danger_color: Color,
        tinted_bg: Color,
        border_color: Option<Color>,
    ) {
        for ly in 0..TILE_H {
            for lx in 0..TILE_W {
                let (x, y) = (ox + lx, oy + ly);
                let on_border = border_color.is_some()
                    && (lx == 0 || ly == 0 || lx == TILE_W - 1 || ly == TILE_H - 1);
                let style = border_color.filter(|_| on_border).map_or_else(
                    || Style::new().fg(tinted_bg).bg(tinted_bg),
                    |bc| Style::new().fg(bc).bg(tinted_bg),
                );
                let glyph = match kind {
                    TileKind::Wall => self
                        .facility
                        .wall_glyph
                        .get(Facility::index(tx, ty).unwrap_or(0))
                        .copied()
                        .unwrap_or('#'),
                    TileKind::Door => '+',
                    TileKind::Floor if strength > 0.0 => ramp_glyph(&SHADE, strength),
                    _ => ' ',
                };
                let fg = match kind {
                    TileKind::Wall => rgb(150, 156, 168),
                    TileKind::Door => rgb(226, 190, 110),
                    _ if strength > 0.0 => mix(danger_color, rgb(255, 255, 255), 0.15),
                    _ => tinted_bg,
                };
                let style = if on_border { style } else { style.fg(fg) };
                Self::put(surface, area, x, y, glyph, style);
            }
        }
    }

    /// Draws the reachable-tile frame corners and its AP-cost digit, if the
    /// tile is reachable this turn. Split out of
    /// [`draw_tile`](Self::draw_tile); see [`fill_tile_block`](Self::fill_tile_block).
    fn draw_reach_frame(
        surface: &mut Surface<'_>,
        area: Rect,
        ox: i32,
        oy: i32,
        border_color: Option<Color>,
        tinted_bg: Color,
        cost: u32,
    ) {
        let Some(bc) = border_color else { return };
        let mask_style = Style::new().fg(bc).bg(tinted_bg);
        Self::put(surface, area, ox, oy, '\u{250c}', mask_style);
        Self::put(surface, area, ox + TILE_W - 1, oy, '\u{2510}', mask_style);
        Self::put(surface, area, ox, oy + TILE_H - 1, '\u{2514}', mask_style);
        Self::put(
            surface,
            area,
            ox + TILE_W - 1,
            oy + TILE_H - 1,
            '\u{2518}',
            mask_style,
        );
        if cost != path::IMPASSABLE {
            let text = format!("{cost}");
            Self::put(
                surface,
                area,
                ox + TILE_W - 1 - text.len() as i32,
                oy,
                text.chars().next().unwrap_or('0'),
                mask_style,
            );
        }
    }

    /// Draws the safe/terminal/exit marker for one tile, if it carries one.
    /// Split out of [`draw_tile`](Self::draw_tile); see
    /// [`fill_tile_block`](Self::fill_tile_block).
    #[allow(clippy::too_many_arguments)]
    fn draw_objective_marker(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tx: i32,
        ty: i32,
        ox: i32,
        oy: i32,
        tinted_bg: Color,
    ) {
        match self.facility.objective_at(Cell::new(tx, ty)) {
            Objective::Safe => Self::put(
                surface,
                area,
                ox + 1,
                oy + 1,
                'S',
                Style::new().fg(rgb(246, 196, 96)).bg(tinted_bg),
            ),
            Objective::Terminal => Self::put(
                surface,
                area,
                ox + 1,
                oy + 1,
                'T',
                Style::new().fg(rgb(120, 196, 226)).bg(tinted_bg),
            ),
            Objective::Exit => Self::put(
                surface,
                area,
                ox + 1,
                oy + 1,
                'X',
                Style::new().fg(rgb(120, 226, 158)).bg(tinted_bg),
            ),
            Objective::None => {}
        }
    }

    fn draw_sentinel_tokens(&self, surface: &mut Surface<'_>, area: Rect) {
        let mult = self.speed_mult();
        for sentinel in self.active_sentinels() {
            let (cell, facing) = sentinel.state_at(self.time, mult);
            let (ox, oy) = self.map_screen_origin(cell.x, cell.y, area);
            let glyph = matches!(sentinel.route, Route::Fixed(_))
                .then_some('C')
                .unwrap_or('G');
            let arrow = facing_arrow(facing);
            let style = Style::new().fg(rgb(20, 12, 12)).bg(rgb(216, 108, 84));
            Self::put(surface, area, ox + 2, oy + 1, glyph, style);
            Self::put(
                surface,
                area,
                ox + 3,
                oy + 1,
                arrow,
                Style::new().fg(rgb(216, 108, 84)).bg(Color::Default),
            );
        }
    }

    fn draw_agent_tokens(&self, surface: &mut Surface<'_>, area: Rect) {
        for (i, agent) in self.agents.iter().enumerate() {
            let (ox, oy) = self.map_screen_origin(agent.pos.x, agent.pos.y, area);
            let selected = i == self.selected;
            let accent = if selected {
                rgb(246, 196, 96)
            } else {
                rgb(120, 170, 196)
            };
            let style = Style::new().fg(rgb(12, 14, 18)).bg(accent);
            let digit = char::from(b'1' + i as u8);
            Self::put(surface, area, ox + 1, oy + 1, digit, style);
            let tag = &agent.name[..agent.name.len().min(3)];
            let tag_style = Style::new().fg(accent).bg(Color::Default);
            for (j, c) in tag.chars().enumerate() {
                Self::put(surface, area, ox + 3 + j as i32, oy + 1, c, tag_style);
            }

            if let Some((pending_agent, target)) = self.pending
                && pending_agent == i
            {
                let (px, py) = self.map_screen_origin(target.x, target.y, area);
                let pulse = if (self.time * 3.0).fract() < 0.5 {
                    '!'
                } else {
                    '?'
                };
                Self::put(
                    surface,
                    area,
                    px + TILE_W / 2,
                    py,
                    pulse,
                    Style::new().fg(rgb(255, 220, 90)).bg(rgb(60, 20, 20)),
                );
            }
        }
    }

    fn draw_alarm(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Alarm")
            .badge(&format!("turn {}", self.turn))
            .draw(surface, area);
        if inner.height() == 0 || inner.width() < 10 {
            return;
        }
        let bar_w = inner.width().saturating_sub(2);
        panel::bar(
            surface,
            (inner.left(), inner.top()),
            bar_w,
            self.alarm / 100.0,
            panel::threshold(1.0 - self.alarm / 100.0),
            rgb(30, 30, 36),
        );

        let next = ESCALATION.iter().find(|&&t| t > self.alarm);
        let text = next.map_or_else(
            || "Fully escalated.".to_string(),
            |t| format!("Next escalation at {t:.0} (now {:.0}).", self.alarm),
        );
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_status(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Agents").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        for (i, agent) in self.agents.iter().enumerate() {
            if i as u16 >= inner.height() {
                break;
            }
            let selected = i == self.selected;
            let marker = if selected { '>' } else { ' ' };
            panel::spans(
                surface,
                (inner.left(), inner.top() + i as u16),
                inner.width(),
                &[
                    Span::new(&format!("{marker} "), ui::ACCENT),
                    Span::keyword(agent.name),
                    Span::plain(&format!("  AP {}/{START_AP}", agent.ap)),
                ],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Log").draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    /// Draws Undo (bottom-left) and End Turn (bottom-right), each grown to a
    /// legal touch target and placed at opposite corners of the thumb zone on
    /// purpose: a startled tap lands somewhere in the middle of the screen,
    /// not in either corner, so keeping an irreversible action (End Turn) and
    /// its escape hatch (Undo) maximally far apart is what stops one mis-tap
    /// from chaining into the other.
    fn draw_controls(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.control_hotspots.clear();
        if area.height() == 0 || area.width() < touch::TAP_W * 2 {
            return;
        }
        let undo_rect = touch::tappable(
            Rect::new(
                area.left(),
                area.top(),
                touch::TAP_W,
                touch::TAP_H.min(area.height()),
            ),
            area,
        );
        let end_rect = touch::tappable(
            Rect::new(
                area.right().saturating_sub(touch::TAP_W),
                area.top(),
                touch::TAP_W,
                touch::TAP_H.min(area.height()),
            ),
            area,
        );

        let undo_enabled = !self.undo_stack.is_empty();
        draw_button(
            surface,
            undo_rect,
            "UNDO",
            if undo_enabled {
                rgb(120, 170, 196)
            } else {
                ui::DIM
            },
        );
        draw_button(surface, end_rect, "END TURN", rgb(226, 184, 90));

        self.control_hotspots.push(undo_rect, Control::Undo);
        self.control_hotspots.push(end_rect, Control::EndTurn);
    }

    fn status_line(&self) -> String {
        format!(
            "seed {}  stage {}  alarm {:.0}  agent {}",
            self.seed, self.stage, self.alarm, self.agents[self.selected].name
        )
    }
}

fn draw_button(surface: &mut Surface<'_>, rect: Rect, label: &str, accent: Color) {
    let inner = panel::Panel::new()
        .border(panel::Border::Double)
        .frame(accent)
        .draw(surface, rect);
    if inner.width() == 0 || inner.height() == 0 {
        return;
    }
    let y = inner.top() + inner.height() / 2;
    let x = inner.left() + inner.width().saturating_sub(label.len() as u16) / 2;
    surface.print((x, y), label, Style::new().fg(accent).bg(panel::PANEL_BG));
}

/// The eight-way arrow for a facing angle, restricted to CP437's four
/// cardinal arrows and diagonal slashes -- the same restricted set
/// [`tilekit::path::arrow`] uses, for the same reason: CP437 has no
/// box-drawing diagonal.
fn facing_arrow(angle: f32) -> char {
    let deg = angle.to_degrees().rem_euclid(360.0);
    match ((deg + 22.5) / 45.0) as u32 % 8 {
        0 => '\u{2192}',
        1 | 5 => '\\',
        2 => '\u{2193}',
        4 => '\u{2190}',
        6 => '\u{2191}',
        _ => '/',
    }
}

impl Demo for StealthGrid {
    const NAME: &'static str = "35_stealth_grid";
    const TITLE: &'static str = "35 Stealth Grid";
    const BLURB: &'static str =
        "Invisible Inc vision cones: real-time guards, turn-based agents, one undo stack.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "cycle agent"),
            ("arrows", "preview a move"),
            ("Enter", "confirm move"),
            ("U", "undo move"),
            ("E", "end turn"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.handle_pointer();

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        let control_h = touch::TAP_H.min(content.height());
        let (map_and_side, controls_area) = panel::split_bottom(content, control_h);
        let (alarm_area, rest) = panel::split_top(map_and_side, 3.min(map_and_side.height()));

        let (map_area, side_area) = if shape.stacks() {
            // Portrait: the sidebar is a thin status strip under the map
            // rather than a column beside it, since rows (not columns) are
            // the scarce axis here.
            let side_h = 6.min(rest.height());
            let (map_rect, side_rect) = panel::split_bottom(rest, side_h);
            (map_rect, side_rect)
        } else {
            let side_w = 30.min(rest.width().saturating_sub(30));
            let (map_rect, side_rect) = panel::split_right(rest, side_w);
            (map_rect, side_rect)
        };

        self.draw_alarm(&mut surface, alarm_area);
        self.draw_facility(&mut surface, map_area);

        if shape.stacks() {
            self.draw_status(&mut surface, side_area);
        } else {
            let (status_rect, log_rect) = panel::split_top(side_area, 5.min(side_area.height()));
            self.draw_status(&mut surface, status_rect);
            self.draw_log(&mut surface, log_rect);
        }

        self.draw_controls(&mut surface, controls_area);

        // Resolve this frame's tap against whichever hotspot set it landed
        // in: controls first, since Undo/End Turn sit outside the map and
        // must never be shadowed by a tile hotspot underneath them.
        if let Some(pos) = gesture.tap {
            if let Some(&control) = self.control_hotspots.hit(pos) {
                match control {
                    Control::Undo => self.undo(),
                    Control::EndTurn => self.end_turn(),
                }
            } else if let Some(&TileHit(cell)) = self.tile_hotspots.hit(pos) {
                let now = self.cone_field(self.time);
                let next = self.cone_field(self.time + PREDICT_SECONDS);
                self.handle_tile_tap(cell, &now, &next);
            }
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(StealthGrid);
