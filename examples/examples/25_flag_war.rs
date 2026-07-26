//! 25: Flag war -- a real-time territory game encoded entirely in colored
//! ASCII trigrams, after Andrey Nikolaev's `curseofwar`.
//!
//! `curseofwar` is worth studying because it refuses every convenience a
//! modern strategy map would reach for. There is no sprite, no icon font, no
//! per-unit rendering: every tile is exactly three characters plus the space
//! around it, and the entire game state (terrain, ownership, population
//! magnitude, whose flag stands where) is legible from that alone. This demo
//! reproduces the encoding faithfully rather than modernizing it, because the
//! encoding is the interesting part.
//!
//! Techniques on show:
//!
//! - **The staggered trigram grid.** Each tile is a 3-glyph string; tiles sit
//!   4 columns apart horizontally and 1 row apart vertically, with every row
//!   shifted 2 columns from the last (`x = col*4 + row*2`, `y = row`). That
//!   stagger is what gives the map its lens-shaped silhouette rather than a
//!   plain rectangle, and it falls out of drawing an offset hex grid with
//!   plain square math instead of [`tilekit::geom::HexLayout`]: `curseofwar`
//!   is not actually hexagonal, it only reads as roughly hex-packed because
//!   half of each row's horizontal gap is absorbed by the stagger.
//! - **Population density as a logarithmic glyph code.** A tile's crowd is
//!   drawn as one of ten fixed 3-character trigrams (`.`, `..`, `...`, ` : `,
//!   ...) rather than as a number, and the bands double roughly every step.
//!   A linear encoding would waste eight of ten codes on the low end (any
//!   crowd worth fighting over is already past 50) or need far more than ten
//!   codes to reach the high end; a log scale spends its limited alphabet
//!   where the games's decisions actually happen.
//! - **Color as the only ownership channel.** A tile's *glyph* says what it
//!   is (grassland, town, fortress, mine); its *color* says whose it is.
//!   Terrain that supports no owner (mountains, mines before they are
//!   captured) is drawn in a fixed neutral tone specifically so that a glance
//!   distinguishes "nobody's yet" from "somebody's, and here is who."
//! - **Flag-driven population flow**, which is the whole game rather than a
//!   decoration: every tick, each populated tile sends part of its people
//!   toward whichever neighbor is closer to a flag of the same faction. The
//!   "closer to a flag" field is a per-faction breadth-first distance map,
//!   recomputed only when that faction's flags change, so the flow itself is
//!   cheap even though the map beneath it updates every frame. Placing a flag
//!   is a wish, not a command: population still has to walk there, tile by
//!   tile, past whatever is in the way.
//! - **Two AI factions** place and relocate flags on their own timers, so a
//!   real (if simple) three-way war plays out with no input at all -- the
//!   thing the thumbnail generator's animation check demands, and also just
//!   the point of a real-time strategy demo.
//!
//! ```sh
//! cargo run --example 25_flag_war --features crossterm
//! cargo run --example 25_flag_war --features software
//! cargo run --example 25_flag_war --features gl
//! cargo run --example 25_flag_war  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::Rng;
use tilekit::palette::{self, faction, mix};

/// Map size in tiles. Odd so the lens shape has a true center row/column.
///
/// Chosen against [`PITCH_X`]/[`ROW_SHIFT`] so the map's full screen extent
/// (see [`FlagWar::map_extent`]) fills most of [`FlagWar::GRID`]'s content
/// area rather than a fraction of it: the trigram grid has a fixed pitch, so
/// unlike every other demo here, making the map bigger does not mean "more
/// detail", it means "physically wider (and taller) on screen", and a map
/// sized to fit a much smaller terminal leaves most of a wide window as empty
/// page background above and below the lens. An earlier 23x21 map filled its
/// width but left more than half of `GRID`'s 44-row content area black above
/// and below -- the mountain border floating in the middle of a mostly empty
/// window.
///
/// 28x27 keeps the same width-to-height ratio that produced a clean lens at
/// 23x21 (rather than independently maximizing width and height, which
/// stretches the silhouette toward a diamond: the vertical row stagger's
/// contribution to screen width grows with `MAP_H`, and once it dominates the
/// ellipse's own width the shape's corners sharpen into points instead of
/// staying a rounded lens). The result fills 137 of `GRID`'s 140 columns and
/// 27 of its 44 content rows -- still short of the full height, because
/// pushing height further at this fixed ratio would overshoot the width
/// first, but a large improvement over 21. [`FlagWar::map_offset`] still
/// scrolls to keep the cursor visible on any narrower terminal (an 80-column
/// one is well under the map's own width).
const MAP_W: i32 = 28;
/// See [`MAP_W`].
const MAP_H: i32 = 27;

/// Horizontal cell pitch between tile origins: `curseofwar`'s `POSX` stride.
/// Three cells for the trigram plus one cell of gap.
const PITCH_X: i32 = 4;
/// Extra horizontal shift applied per row, which produces the stagger.
///
/// Folded around the map's vertical center (see [`FlagWar::tile_origin`])
/// rather than applied monotonically top to bottom. A monotonic `y *
/// ROW_SHIFT` is what the naive reading of `curseofwar`'s `POSX = i*4 + j*2`
/// gives, and it is wrong: over a map tens of rows tall, that shear
/// accumulates far more than the tile grid's own width, so an elliptical mask
/// in tile-space renders on screen as a steep diagonal parallelogram rather
/// than the lens `curseofwar` actually displays. Folding the shift around the
/// center row instead makes rows above and below the middle lean toward each
/// other, which is what turns a hex-packed offset grid back into a convex
/// oval on screen.
const ROW_SHIFT: i32 = 2;
/// Vertical cell pitch between rows: one row of screen per row of map.
const PITCH_Y: i32 = 1;

/// Number of factions, including the player. Index 0 is always the player.
const FACTIONS: usize = 3;

/// Simulated seconds between AI flag-placement decisions, scaled by
/// [`FlagWar::speed`](FlagWar). Deliberately slower than the player can act,
/// so a human with the cursor still reads as faster and more decisive than
/// the AI.
const AI_DECISION_PERIOD: f32 = 3.2;

/// Delay before each AI faction's *first* decision, much shorter than
/// [`AI_DECISION_PERIOD`] itself.
///
/// Without a separate first delay, the map sits with zero flags anywhere for
/// up to a full [`AI_DECISION_PERIOD`] after startup -- during which nothing
/// visible happens at all: no flag to blink, no pull to migrate toward, no
/// density band close to ticking over. A demo is supposed to prove its own
/// technique is alive within the first second or two someone looks at it, not
/// after a multi-second lag the gallery's thumbnail generator also has no
/// patience for.
const AI_FIRST_DECISION_DELAY: f32 = 0.6;

/// Simulated seconds per population-flow step at 1x speed. `curseofwar` calls
/// this the game's real clock; everything else (growth, AI) is keyed off it.
const FLOW_PERIOD: f32 = 0.35;

/// How much of a tile's population moves toward a better neighbor each flow
/// step, as a fraction. Low enough that migration reads as a flood filling in
/// over several seconds rather than an instant teleport.
const FLOW_FRACTION: f32 = 0.28;

/// Starting population for each faction's seed village.
///
/// Large enough, combined with [`BASE_GROWTH`]'s compounding rate, that a
/// faction's home tile has already grown and begun spilling onto its
/// neighbors within the first simulated second -- the window the gallery's
/// thumbnail is taken in, and the same window anyone landing on the demo
/// actually looks at it. `curseofwar` itself starts a new settlement with a
/// double-digit population for the same reason: a village that opens at 1
/// person spends its first several seconds looking abandoned.
const SEED_POPULATION: f32 = 24.0;

/// Soft ceiling a single tile's per-faction population grows toward.
///
/// Just above the top density band ([`density_glyphs`]'s `401+` catch-all),
/// so a fortress-tier tile can visibly reach the densest glyph without ever
/// needing to reach further. [`Map::grow`]'s logistic term is what makes this
/// a ceiling rather than a target compounding sails past: percentage growth
/// with no limit is not a cosmetic simplification, it is wrong on any
/// timescale longer than the one it was tuned for. A tile compounding at even
/// a modest rate reaches five and six digits within a couple of simulated
/// minutes, at which point it dominates every flow comparison, breaks the
/// gold-income math built on `usize` tile counts times a small constant, and
/// eventually stops being representable as a sane `f32` at all. This demo has
/// no natural end state -- it runs until someone closes the window -- so
/// "looks fine for the first five minutes" is not good enough.
const POPULATION_CAP: f32 = 480.0;

/// Population growth per flow step on owned, non-water, non-mountain terrain,
/// as a fraction of the tile's own population, scaled by the settlement
/// bonus. Mirrors `curseofwar`'s village/town/fortress growth-rate
/// multipliers, which are also percentages of the tile's own crowd, not a
/// flat headcount: a fixed `+N` per step is what makes growth invisible once
/// population is above a handful, since the increment stops mattering long
/// before the density band it would need to cross does. A percentage keeps
/// pace with whatever is already there, the way an unchecked population
/// naturally does.
const BASE_GROWTH: f32 = 0.12;

/// Terrain a tile is built from. Doubles as `curseofwar`'s glyph vocabulary:
/// each variant maps to exactly one trigram in [`Tile::glyphs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terrain {
    /// Impassable, uninhabitable, never owned.
    Mountain,
    /// A resource deposit. Uninhabited until a faction's population reaches
    /// it, at which point it starts counting toward that faction's gold.
    Mine,
    /// Open, habitable ground. Population here is shown by density trigram
    /// rather than by a settlement glyph.
    Grass,
    /// A settlement tier. Higher tiers grow their population faster, which is
    /// `curseofwar`'s whole reason to build one instead of just occupying
    /// grass.
    Settlement(SettlementTier),
}

/// Settlement tiers, weakest to strongest. `curseofwar` cycles a build
/// through these with one key; this demo lets the player do the same, and the
/// AI upgrades its own settlements automatically once population justifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SettlementTier {
    Village,
    Town,
    Fortress,
}

impl SettlementTier {
    /// Population growth multiplier over [`BASE_GROWTH`]. `curseofwar` uses
    /// +10%/+20%/+30%; kept as multipliers here so the numbers stay in the
    /// same units as [`BASE_GROWTH`].
    const fn growth_multiplier(self) -> f32 {
        match self {
            Self::Village => 1.10,
            Self::Town => 1.20,
            Self::Fortress => 1.30,
        }
    }

    /// Population at which a settlement is worth upgrading. The AI (and a
    /// manual `R`/`V` press) use this to decide when to build the next tier.
    const fn upgrade_threshold(self) -> f32 {
        match self {
            Self::Village => 40.0,
            Self::Town => 120.0,
            Self::Fortress => f32::INFINITY,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Village => Self::Town,
            Self::Town | Self::Fortress => Self::Fortress,
        }
    }
}

/// One map tile: its terrain, who holds it, how many people live there, and
/// whether each faction has planted a flag on it.
#[derive(Debug, Clone)]
struct Tile {
    terrain: Terrain,
    /// The faction with the most population here, or `None` if uninhabited or
    /// uninhabitable. `curseofwar` derives ownership from population majority
    /// rather than storing it independently, which is what lets a tile change
    /// hands purely by migration with no explicit "capture" event.
    owner: Option<usize>,
    /// Population per faction. `curseofwar` sums every faction's population
    /// on a tile for the density glyph and separately tracks who has the
    /// most for ownership; both come from this array.
    population: [f32; FACTIONS],
    /// Which factions have a flag planted here.
    flags: [bool; FACTIONS],
}

impl Tile {
    const fn new(terrain: Terrain) -> Self {
        Self {
            terrain,
            owner: None,
            population: [0.0; FACTIONS],
            flags: [false; FACTIONS],
        }
    }

    /// Total population across every faction, the number the density trigram
    /// encodes.
    fn total_population(&self) -> f32 {
        self.population.iter().sum()
    }

    /// Recomputes [`owner`](Self::owner) from the population majority. Called
    /// after any population change; a tile with nobody on it has no owner,
    /// which is what lets territory revert to neutral once abandoned.
    fn recompute_owner(&mut self) {
        let mut best = (usize::MAX, 0.0f32);
        for (faction, &pop) in self.population.iter().enumerate() {
            if pop > best.1 {
                best = (faction, pop);
            }
        }
        self.owner = (best.1 > 0.5)
            .then_some(best.0)
            .filter(|_| best.0 != usize::MAX);
    }

    /// The three-character glyph string `curseofwar` would draw for this
    /// tile: terrain shape for a mountain, mine, or settlement; population
    /// density band for open grass.
    fn glyphs(&self) -> [char; 3] {
        match self.terrain {
            // A mountain range: `/\^`. Never owned, so this is always neutral.
            Terrain::Mountain => ['/', '\\', '^'],
            // An unclaimed or claimed gold mine: `/$\`. The `$` is the part
            // that reads as "resource" regardless of who, if anyone, holds it.
            Terrain::Mine => ['/', '$', '\\'],
            Terrain::Settlement(SettlementTier::Village) => [' ', 'n', ' '],
            Terrain::Settlement(SettlementTier::Town) => ['i', '=', 'i'],
            Terrain::Settlement(SettlementTier::Fortress) => ['W', '#', 'W'],
            Terrain::Grass => density_glyphs(self.total_population()),
        }
    }

    /// Whether population can grow or flow onto this tile at all.
    const fn habitable(&self) -> bool {
        !matches!(self.terrain, Terrain::Mountain)
    }
}

/// The population-density trigram for a total, per `curseofwar`'s own table.
///
/// Ten bands rather than a smooth ramp because the display is exactly three
/// characters: this is the richest vocabulary three cells of `.`/`:`/space can
/// spell without repeating a pattern. The bands roughly double (1-3, 4-6,
/// 7-12, 13-25, 26-50, ...) because population *worth distinguishing* also
/// roughly doubles: the difference between 2 and 4 people changes who wins a
/// border skirmish, but the difference between 300 and 320 does not, so a
/// linear scale would spend most of its ten codes on a range where nothing
/// strategic changes and have none left for the range where everything does.
///
/// Zero is `" - "`, not blank. `curseofwar` draws empty grassland as a dash,
/// specifically so uninhabited-but-habitable ground still reads as ground: a
/// blank trigram is indistinguishable from the gaps between tiles, so an
/// unpopulated map would render as nothing but the mountain border floating
/// on black, which is exactly the failure this glyph exists to prevent.
fn density_glyphs(total: f32) -> [char; 3] {
    let n = total.round() as i64;
    match n {
        ..=0 => [' ', '-', ' '],
        1..=3 => [' ', '.', ' '],
        4..=6 => [' ', '.', '.'],
        7..=12 => ['.', '.', '.'],
        13..=25 => [' ', ':', ' '],
        26..=50 => ['.', ':', ' '],
        51..=100 => [' ', ':', '.'],
        101..=200 => [' ', ':', ':'],
        201..=400 => ['.', ':', ':'],
        _ => [':', ':', ':'],
    }
}

/// Breadth-first distance, in tiles, to the nearest flag of one faction.
///
/// Recomputed only when that faction's flags actually change (tracked in
/// [`FlagWar::fields_dirty`](FlagWar)), not every frame: the flow step reads
/// it every tile every tick, but the flags themselves change only on a
/// handful of player and AI actions, so caching here is what keeps the whole
/// simulation cheap on a map this size.
struct FlagField {
    /// Distance per tile, `u16::MAX` for unreachable or too far to matter.
    distance: Vec<u16>,
}

/// Distances beyond this are treated as equally unattractive, so a flag does
/// not pull population from clear across the map through territory that will
/// never actually reach it in reasonable time.
const FLAG_HORIZON: u16 = 14;

impl FlagField {
    fn compute(map: &Map, faction: usize) -> Self {
        let mut distance = vec![u16::MAX; (MAP_W * MAP_H) as usize];
        let mut frontier = Vec::new();
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                if map.tile(x, y).flags[faction] {
                    let i = Map::index(x, y);
                    distance[i] = 0;
                    frontier.push((x, y));
                }
            }
        }
        let mut depth = 0u16;
        while !frontier.is_empty() && depth < FLAG_HORIZON {
            depth += 1;
            let mut next = Vec::new();
            for (x, y) in frontier {
                for (nx, ny) in Map::neighbors4(x, y) {
                    if !map.tile(nx, ny).habitable() {
                        continue;
                    }
                    let i = Map::index(nx, ny);
                    if distance[i] == u16::MAX {
                        distance[i] = depth;
                        next.push((nx, ny));
                    }
                }
            }
            frontier = next;
        }
        Self { distance }
    }

    /// The pull score at `(x, y)`: high near a flag, zero with no flag or
    /// beyond the horizon. Population flow compares this between neighbors.
    fn pull(&self, x: i32, y: i32) -> f32 {
        let d = self.distance[Map::index(x, y)];
        if d == u16::MAX {
            0.0
        } else {
            1.0 - f32::from(d) / f32::from(FLAG_HORIZON)
        }
    }
}

/// The map: a flat tile grid plus the four-neighbor adjacency the flow and
/// flag-field search share.
struct Map {
    tiles: Vec<Tile>,
    /// Each faction's starting village position, indexed by faction. Used
    /// only to give the cursor a sensible starting tile; the simulation
    /// itself reads ownership and population from `tiles`, never from this.
    villages: [(i32, i32); FACTIONS],
}

impl Map {
    const fn index(x: i32, y: i32) -> usize {
        (y * MAP_W + x) as usize
    }

    fn in_bounds(x: i32, y: i32) -> bool {
        (0..MAP_W).contains(&x) && (0..MAP_H).contains(&y)
    }

    fn tile(&self, x: i32, y: i32) -> &Tile {
        &self.tiles[Self::index(x, y)]
    }

    fn tile_mut(&mut self, x: i32, y: i32) -> &mut Tile {
        let i = Self::index(x, y);
        &mut self.tiles[i]
    }

    /// The four map-adjacent neighbors that exist on the map. Cardinal rather
    /// than the six a true hex grid would use: this map only *reads* as
    /// hex-packed thanks to the row stagger, but its underlying adjacency is
    /// the plain square grid the stagger is drawn over.
    fn neighbors4(x: i32, y: i32) -> Vec<(i32, i32)> {
        [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .map(|(dx, dy)| (x + dx, y + dy))
            .filter(|&(nx, ny)| Self::in_bounds(nx, ny))
            .collect()
    }

    /// Generates a lens-shaped map: an elliptical mask over noise-seeded
    /// terrain, echoing `curseofwar`'s own oval silhouette (its stagger
    /// naturally rounds off a rectangular tile array into that shape once
    /// every row is shifted by a different amount).
    fn generate(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let (cx, cy) = (f32::from(MAP_W as u16) / 2.0, f32::from(MAP_H as u16) / 2.0);
        let mut tiles = Vec::with_capacity((MAP_W * MAP_H) as usize);

        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let dx = (x as f32 - cx) / cx;
                let dy = (y as f32 - cy) / cy;
                let r = dx.hypot(dy);
                let terrain = if r > 1.0 {
                    // Outside the lens: mountains, so the map has a solid
                    // border rather than fraying into isolated grass islands.
                    Terrain::Mountain
                } else {
                    let roll = rng.next_f32();
                    if roll < 0.06 {
                        Terrain::Mountain
                    } else if roll < 0.10 {
                        Terrain::Mine
                    } else {
                        Terrain::Grass
                    }
                };
                tiles.push(Tile::new(terrain));
            }
        }

        let mut map = Self {
            tiles,
            villages: [(0, 0); FACTIONS],
        };
        map.villages = map.seed_factions(&mut rng);
        map
    }

    /// Places each faction's starting settlement, spread roughly evenly
    /// around the lens so no faction begins pinned against another. Returns
    /// each faction's village position, indexed by faction, so the caller can
    /// start the cursor somewhere the population readout already has real
    /// numbers to show instead of on whatever tile happens to sit at the geometric
    /// map center.
    ///
    /// Also plants each faction's first flag, one tile toward the map's
    /// center from its village, so population starts marching outward from
    /// frame one instead of waiting on the AI's first decision timer (or, for
    /// the player, on someone actually pressing Space). Flags are what pulls
    /// population here -- growth alone only compounds a tile's own crowd, it
    /// does not spread it to a neighbor -- so a faction with no flag anywhere
    /// sits on its starting tile indefinitely no matter how much it has
    /// grown, and a demo about territory that spends its first couple of
    /// seconds visibly not contesting any territory is not showing its own
    /// point.
    fn seed_factions(&mut self, rng: &mut Rng) -> [(i32, i32); FACTIONS] {
        let mut villages = [(0i32, 0i32); FACTIONS];
        for (faction, village) in villages.iter_mut().enumerate() {
            let angle =
                (faction as f32 / FACTIONS as f32).mul_add(core::f32::consts::TAU, rng.next_f32());
            let (cx, cy) = (MAP_W as f32 / 2.0, MAP_H as f32 / 2.0);
            let radius_x = cx * 0.55;
            let radius_y = cy * 0.55;
            let x = angle.cos().mul_add(radius_x, cx).round() as i32;
            let y = angle.sin().mul_add(radius_y, cy).round() as i32;
            let (x, y) = (x.clamp(1, MAP_W - 2), y.clamp(1, MAP_H - 2));

            let tile = self.tile_mut(x, y);
            tile.terrain = Terrain::Settlement(SettlementTier::Village);
            tile.population[faction] = SEED_POPULATION;
            tile.recompute_owner();

            // One step toward the center, so the flag lands on habitable
            // ground for every seed position this loop can produce (the
            // village itself is always inside the lens, and the direction
            // toward the center from anywhere inside a convex lens stays
            // inside it too).
            let (dx, dy) = (
                (cx - x as f32).signum() as i32,
                (cy - y as f32).signum() as i32,
            );
            let (fx, fy) = self.first_flag_target(x, y, dx, dy);
            self.tile_mut(fx, fy).flags[faction] = true;

            *village = (x, y);
        }
        villages
    }

    /// The tile a faction's opening flag lands on: one cardinal step from
    /// `(x, y)` toward `(dx, dy)`, falling back to `(x, y)`'s other cardinal
    /// neighbors, and finally to `(x, y)` itself, if the exact diagonal step
    /// is not on the map or not habitable (a mountain can still separate a
    /// village from the map's center in an unlucky roll). A flag on the
    /// village's own tile still works: [`FlagField::compute`] seeds its
    /// search from every flagged tile regardless of what is on it, so the
    /// pull simply radiates from the village itself rather than one step
    /// ahead of it.
    fn first_flag_target(&self, x: i32, y: i32, dx: i32, dy: i32) -> (i32, i32) {
        for (cx, cy) in [(x + dx, y), (x, y + dy)] {
            if Self::in_bounds(cx, cy) && self.tile(cx, cy).habitable() {
                return (cx, cy);
            }
        }
        (x, y)
    }

    /// One flow step: every habitable tile sends [`FLOW_FRACTION`] of each
    /// faction's population toward whichever habitable neighbor scores best
    /// for that faction, then grows in place.
    ///
    /// Scoring combines the destination's flag pull with a mild preference
    /// for a tile the faction already holds, so population does not scatter
    /// evenly toward a flag but tends to consolidate the path to it -- the
    /// behavior that makes a flag read as "reinforcements are heading here"
    /// rather than as an instant teleport.
    fn flow(&mut self, fields: &[FlagField; FACTIONS]) {
        let mut delta = vec![[0.0f32; FACTIONS]; self.tiles.len()];

        for y in 0..MAP_H {
            for x in 0..MAP_W {
                if !self.tile(x, y).habitable() {
                    continue;
                }
                let neighbors = Self::neighbors4(x, y);
                for faction in 0..FACTIONS {
                    let here_i = Self::index(x, y);
                    let population = self.tiles[here_i].population[faction];
                    if population < 0.5 {
                        continue;
                    }

                    let here_score = self.tile_score(x, y, faction, &fields[faction]);
                    let Some(&(bx, by)) = neighbors
                        .iter()
                        .filter(|&&(nx, ny)| self.tile(nx, ny).habitable())
                        .max_by(|&&a, &&b| {
                            let sa = self.tile_score(a.0, a.1, faction, &fields[faction]);
                            let sb = self.tile_score(b.0, b.1, faction, &fields[faction]);
                            sa.total_cmp(&sb)
                        })
                    else {
                        continue;
                    };
                    let best_score = self.tile_score(bx, by, faction, &fields[faction]);

                    // Only flow toward a strictly better tile. Without this, a
                    // faction with no flags anywhere would still slosh its
                    // population back and forth between equally-scored
                    // neighbors every tick, which reads as jitter rather than
                    // migration.
                    if best_score > here_score + 1e-4 {
                        let moved = population * FLOW_FRACTION;
                        delta[here_i][faction] -= moved;
                        delta[Self::index(bx, by)][faction] += moved;
                    }
                }
            }
        }

        for (tile, deltas) in self.tiles.iter_mut().zip(delta.iter()) {
            for (pop, d) in tile.population.iter_mut().zip(deltas.iter()) {
                *pop = (*pop + d).max(0.0);
            }
            Self::grow(tile);
            tile.recompute_owner();
        }
    }

    /// How attractive `(x, y)` is to `faction`: flag pull, plus a tie-breaking
    /// bonus for territory the faction already holds.
    ///
    /// The bonus has to stay well under one flag-field step
    /// (`1.0 / FLAG_HORIZON`, about 0.07): its only job is to stop population
    /// sloshing between two neighbors the field scores exactly equally, not to
    /// compete with the field's own gradient. An earlier version of this used
    /// 0.15, larger than a full step, and the result was that population
    /// never left a faction's own village at all -- the tile the population
    /// started on always scored higher than a neighbor one step closer to a
    /// flag, however far away that flag was, so a flag pulled nothing.
    fn tile_score(&self, x: i32, y: i32, faction: usize, field: &FlagField) -> f32 {
        let tile = self.tile(x, y);
        if !tile.habitable() {
            return f32::NEG_INFINITY;
        }
        let held_bonus = if tile.owner == Some(faction) {
            0.02
        } else {
            0.0
        };
        field.pull(x, y) + held_bonus
    }

    /// Grows a tile's population for each faction that already holds
    /// majority-share ground there, scaled by settlement tier.
    ///
    /// Logistic, not plain compounding: the step multiplies population by
    /// `1 + rate * (1 - pop / cap)`, so growth starts close to pure
    /// compounding while a tile is nearly empty (`pop / cap` is near zero,
    /// the parenthesized term is near 1) and tapers to nothing as `pop`
    /// approaches [`POPULATION_CAP`]. A tile's crowd still grows in
    /// proportion to itself while there is room, which is what makes a
    /// fortress at 200 gain far more people per step than a village at 5
    /// without a separate scale for each regime, but it can no longer
    /// compound forever: plain exponential growth reaches astronomical
    /// numbers within a couple of simulated minutes on a demo with no natural
    /// end state, at which point it breaks the flow comparison, the mine
    /// income math, and eventually `f32` itself.
    ///
    /// An associated function rather than a method: called from inside a loop
    /// that already holds `&mut self.tiles[i]`, so it must not also need
    /// `&self`.
    fn grow(tile: &mut Tile) {
        if !tile.habitable() {
            return;
        }
        let multiplier = match tile.terrain {
            Terrain::Settlement(tier) => tier.growth_multiplier(),
            _ => 1.0,
        };
        let rate = BASE_GROWTH * multiplier;
        for pop in &mut tile.population {
            if *pop > 0.1 {
                let room = (1.0 - *pop / POPULATION_CAP).max(0.0);
                *pop *= rate.mul_add(room, 1.0);
            }
        }
    }

    /// Total gold income: mines whose tile is owned contribute per capita.
    /// `curseofwar` prices buildings against a single gold pool; this mirrors
    /// that with a simple per-tick accrual from held mines.
    fn mine_income(&self, faction: usize) -> f32 {
        self.tiles
            .iter()
            .filter(|t| matches!(t.terrain, Terrain::Mine) && t.owner == Some(faction))
            .count() as f32
            * 2.0
    }
}

/// Build prices, escalating per tier the way `curseofwar`'s status line shows
/// "150, 300, 600".
const BUILD_PRICES: [u32; 3] = [150, 300, 600];

/// State: the map, the player's gold and cursor, per-faction flag fields, and
/// the AI's decision timers.
pub struct FlagWar {
    map: Map,
    seed: u32,
    cursor: (i32, i32),
    /// Player gold, spent on settlement upgrades.
    gold: u32,
    fields: [FlagField; FACTIONS],
    /// Whether faction `n`'s flags changed since `fields[n]` was computed.
    fields_dirty: [bool; FACTIONS],
    /// Simulated seconds until the next flow step.
    flow_timer: f32,
    /// Simulated seconds until each AI faction's next decision.
    ai_timer: [f32; FACTIONS],
    /// Speed multiplier: 0 is paused, 1 is normal, up to 4 is fast-forward.
    speed: f32,
    paused: bool,
    time: f32,
    fps: FpsMeter,
}

impl Default for FlagWar {
    fn default() -> Self {
        let seed = 7;
        let map = Map::generate(seed);
        let fields = core::array::from_fn(|f| FlagField::compute(&map, f));
        // The player's own village, not the geometric map center: a fresh
        // view should show the "Pop:" readout already reading real numbers
        // rather than the zeros of whatever generic tile happens to sit in
        // the middle of the lens.
        let cursor = map.villages[0];
        Self {
            map,
            seed,
            cursor,
            gold: 400,
            fields,
            fields_dirty: [false; FACTIONS],
            flow_timer: FLOW_PERIOD,
            ai_timer: [
                0.0, // faction 0 is the player; the AI loop skips index 0.
                AI_FIRST_DECISION_DELAY,
                AI_FIRST_DECISION_DELAY * 1.7,
            ],
            speed: 1.0,
            paused: false,
            time: 0.0,
            fps: FpsMeter::new(),
        }
    }
}

impl FlagWar {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.map = Map::generate(self.seed);
        self.fields = core::array::from_fn(|f| FlagField::compute(&self.map, f));
        self.fields_dirty = [false; FACTIONS];
        self.cursor = self.map.villages[0];
        self.gold = 400;
        self.ai_timer = [0.0, AI_FIRST_DECISION_DELAY, AI_FIRST_DECISION_DELAY * 1.7];
    }

    fn move_cursor(&mut self, dx: i32, dy: i32) {
        self.cursor.0 = (self.cursor.0 + dx).clamp(0, MAP_W - 1);
        self.cursor.1 = (self.cursor.1 + dy).clamp(0, MAP_H - 1);
    }

    /// Toggles the player's own flag at the cursor.
    fn toggle_flag(&mut self) {
        let (x, y) = self.cursor;
        let tile = self.map.tile_mut(x, y);
        if tile.habitable() {
            tile.flags[0] = !tile.flags[0];
            self.fields_dirty[0] = true;
        }
    }

    /// Clears every one of the player's flags.
    fn clear_all_flags(&mut self) {
        for tile in &mut self.map.tiles {
            tile.flags[0] = false;
        }
        self.fields_dirty[0] = true;
    }

    /// Clears roughly half the player's flags: a tactical retreat rather than
    /// a full reset, matching `curseofwar`'s `C` binding.
    fn clear_half_flags(&mut self) {
        let mut rng = Rng::new(self.seed ^ (self.time.to_bits()));
        for tile in &mut self.map.tiles {
            if tile.flags[0] && rng.next_f32() < 0.5 {
                tile.flags[0] = false;
            }
        }
        self.fields_dirty[0] = true;
    }

    /// Builds or upgrades a settlement at the cursor, if the player can
    /// afford it and the tile is eligible (grass they hold, or one of their
    /// own settlements below fortress tier).
    fn build(&mut self) {
        let (x, y) = self.cursor;
        let tier = {
            let tile = self.map.tile(x, y);
            if tile.owner != Some(0) {
                return;
            }
            match tile.terrain {
                Terrain::Grass => SettlementTier::Village,
                Terrain::Settlement(t) if t != SettlementTier::Fortress => t.next(),
                _ => return,
            }
        };
        let price = BUILD_PRICES[match tier {
            SettlementTier::Village => 0,
            SettlementTier::Town => 1,
            SettlementTier::Fortress => 2,
        }];
        if self.gold < price {
            return;
        }
        self.gold -= price;
        self.map.tile_mut(x, y).terrain = Terrain::Settlement(tier);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_cursor(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_cursor(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_cursor(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_cursor(1, 0),
                    KeyCode::Char(' ') => self.toggle_flag(),
                    KeyCode::Char('x' | 'X') => self.clear_all_flags(),
                    KeyCode::Char('c' | 'C') => self.clear_half_flags(),
                    KeyCode::Char('r' | 'R' | 'v' | 'V') => self.build(),
                    // `curseofwar` uses S for slow-down, but S is already
                    // WASD's "cursor down" here, so speed uses F/Z instead
                    // (Z sitting right next to the movement keys).
                    KeyCode::Char('f' | 'F') => self.speed = (self.speed + 0.5).min(4.0),
                    KeyCode::Char('z' | 'Z') => self.speed = (self.speed - 0.5).max(0.0),
                    KeyCode::Char('p' | 'P') => self.paused = !self.paused,
                    KeyCode::Char('n' | 'N') => self.reroll(),
                    _ => {}
                }
            }
        }
        true
    }

    /// Advances the AI factions: each faction, on its own timer, either
    /// plants a new flag near its strongest tile or upgrades a settlement,
    /// giving each one a slow but continuous push without any input.
    fn ai_tick(&mut self, dt: f32) {
        for faction in 1..FACTIONS {
            self.ai_timer[faction] -= dt;
            if self.ai_timer[faction] > 0.0 {
                continue;
            }
            self.ai_timer[faction] = AI_DECISION_PERIOD;
            self.ai_decide(faction);
        }
    }

    fn ai_decide(&mut self, faction: usize) {
        // Find this faction's strongest tile, then aim a flag at a habitable
        // neighbor of it that is not already flagged: a cheap "push the
        // frontier outward" heuristic rather than any real strategy, which is
        // enough to keep three factions visibly contesting the map.
        let mut best: Option<(i32, i32, f32)> = None;
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let tile = self.map.tile(x, y);
                let pop = tile.population[faction];
                if pop > best.map_or(0.0, |b| b.2) {
                    best = Some((x, y, pop));
                }
            }
        }
        let Some((bx, by, _)) = best else {
            return;
        };
        let mut rng = Rng::new(
            self.seed
                .wrapping_add(faction as u32)
                .wrapping_add(self.time.to_bits()),
        );
        let candidates = Map::neighbors4(bx, by);
        if let Some(&(tx, ty)) = rng.choose(&candidates) {
            let eligible = {
                let tile = self.map.tile(tx, ty);
                tile.habitable() && !tile.flags[faction]
            };
            if eligible {
                // At most a few flags per faction: unbounded flag placement
                // would flatten the flag field into "everywhere is near a
                // flag", which erases the whole mechanic this demo is about.
                // Computed before taking any mutable borrow so relocating a
                // flag below does not need two mutable borrows of `self.map`
                // alive at once.
                let existing: Vec<(i32, i32)> = self
                    .map
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.flags[faction])
                    .map(|(i, _)| (i as i32 % MAP_W, i as i32 / MAP_W))
                    .collect();

                if existing.len() >= 5 {
                    // Relocate rather than accumulate: drop the AI's oldest
                    // flag and place the new one, so the AI's front keeps
                    // moving instead of freezing once it hits the cap.
                    if let Some(&(ox, oy)) = existing.first() {
                        self.map.tile_mut(ox, oy).flags[faction] = false;
                    }
                }
                self.map.tile_mut(tx, ty).flags[faction] = true;
                self.fields_dirty[faction] = true;
            }
        }

        // Auto-upgrade: if this faction holds a settlement past its
        // threshold, build the next tier for free (the AI has no gold
        // economy of its own in this demo; it plays entirely through flags
        // and growth).
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let tile = self.map.tile_mut(x, y);
                if tile.owner != Some(faction) {
                    continue;
                }
                if let Terrain::Settlement(tier) = tile.terrain
                    && tier != SettlementTier::Fortress
                    && tile.population[faction] > tier.upgrade_threshold()
                {
                    tile.terrain = Terrain::Settlement(tier.next());
                }
            }
        }
    }

    /// Recomputes any faction's flag field that changed since the last tick.
    fn refresh_fields(&mut self) {
        for faction in 0..FACTIONS {
            if self.fields_dirty[faction] {
                self.fields[faction] = FlagField::compute(&self.map, faction);
                self.fields_dirty[faction] = false;
            }
        }
    }

    /// Screen-cell origin of tile `(x, y)`.
    ///
    /// The horizontal term folds the row shift around the map's vertical
    /// center; see [`ROW_SHIFT`] for why a monotonic `y * ROW_SHIFT` produces
    /// a parallelogram instead of `curseofwar`'s lens.
    const fn tile_origin(x: i32, y: i32) -> (i32, i32) {
        (x * PITCH_X + (y - MAP_H / 2).abs() * ROW_SHIFT, y * PITCH_Y)
    }

    /// The full map's footprint in screen cells.
    const fn map_extent() -> (i32, i32) {
        let (max_x, max_y) = Self::tile_origin(MAP_W - 1, MAP_H - 1);
        (max_x + 3, max_y + 1)
    }

    /// The offset [`draw_map`](Self::draw_map) should use for `area`.
    ///
    /// Two regimes, both because the trigram grid has a fixed pitch so
    /// `area`'s size relative to the map's is not under this demo's control
    /// the way it is for every other demo here:
    ///
    /// - `area` at least as big as the map (its [`GRID`](Demo::GRID) size, and
    ///   anything wider): center the map, so it does not stick to the
    ///   top-left corner of a much bigger viewport.
    /// - `area` smaller than the map (an 80-column terminal is narrower than
    ///   the map's own 115-cell width): scroll to keep the cursor on screen
    ///   rather than reflowing a grid whose entire point is a fixed pitch.
    fn map_offset(&self, area: Rect) -> (i32, i32) {
        let (extent_w, extent_h) = Self::map_extent();
        let (aw, ah) = (i32::from(area.width()), i32::from(area.height()));

        let x = if extent_w <= aw {
            (aw - extent_w) / 2
        } else {
            let (cursor_x, _) = Self::tile_origin(self.cursor.0, self.cursor.1);
            (aw / 2 - cursor_x).clamp(aw - extent_w, 0)
        };
        let y = if extent_h <= ah {
            (ah - extent_h) / 2
        } else {
            let (_, cursor_y) = Self::tile_origin(self.cursor.0, self.cursor.1);
            (ah / 2 - cursor_y).clamp(ah - extent_h, 0)
        };
        (x, y)
    }

    /// The neutral terrain tone: dim enough to read as "structure, not
    /// content", matching `curseofwar`'s undistinguished `COLOR_PAIR(4)` for
    /// unowned ground.
    const fn terrain_color(terrain: Terrain) -> Color {
        match terrain {
            Terrain::Mountain => palette::rgb(120, 116, 108),
            Terrain::Mine => palette::rgb(214, 186, 90),
            Terrain::Grass | Terrain::Settlement(_) => palette::rgb(90, 96, 84),
        }
    }

    /// Draws the map into `area`, shifted by `offset` (screen cells added to
    /// every tile's origin).
    ///
    /// `offset` rather than the more familiar "pan" naming because it has to
    /// go both ways: a map wider than `area` needs a negative offset to scroll
    /// toward whatever matters, but this map is usually *smaller* than the
    /// content area (the trigram grid has a fixed pitch, so a bigger map means
    /// a physically wider map, not a more detailed one -- see [`MAP_W`]), and
    /// a smaller map needs a positive offset to center it, or it sticks to the
    /// top-left corner like a document that never scrolled.
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect, offset: (i32, i32)) {
        let blink_on = (self.time * 2.4).fract() < 0.6;

        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let (ox, oy) = Self::tile_origin(x, y);
                let (sx, sy) = (ox + offset.0, oy + offset.1);
                if sx < -2
                    || sy < 0
                    || sx >= i32::from(area.width())
                    || sy >= i32::from(area.height())
                {
                    continue;
                }

                let tile = self.map.tile(x, y);
                let glyphs = tile.glyphs();
                let is_owned_glyph = matches!(tile.terrain, Terrain::Settlement(_));
                let color = if is_owned_glyph {
                    tile.owner
                        .map_or_else(|| Self::terrain_color(tile.terrain), faction)
                } else if matches!(tile.terrain, Terrain::Grass) {
                    tile.owner.map_or_else(
                        || palette::rgb(150, 150, 150),
                        |owner| mix(faction(owner), palette::WHITE, 0.15),
                    )
                } else {
                    Self::terrain_color(tile.terrain)
                };

                for (i, &ch) in glyphs.iter().enumerate() {
                    let cx = sx + i as i32;
                    if cx < 0 || cx >= i32::from(area.width()) {
                        continue;
                    }
                    let (px, py) = (area.left() + cx as u16, area.top() + sy as u16);
                    surface.put((px, py), ch, Style::new().fg(color).bg(ui::BG));
                }

                // The player's flag: a bold white P one cell right of the
                // trigram. Enemy flags: a colored x one cell left. Both blink
                // rather than staying solid, since a static marker on a
                // crowded map is easy to miss against the terrain glyphs.
                if blink_on {
                    if tile.flags[0] {
                        let (px, py) =
                            (area.left() + (sx + 3).max(0) as u16, area.top() + sy as u16);
                        if sx + 3 < i32::from(area.width()) {
                            surface.put((px, py), 'P', Style::new().fg(palette::WHITE).bg(ui::BG));
                        }
                    }
                    for enemy in 1..FACTIONS {
                        if tile.flags[enemy] && sx > 0 {
                            let (px, py) = (area.left() + (sx - 1) as u16, area.top() + sy as u16);
                            surface.put((px, py), 'x', Style::new().fg(faction(enemy)).bg(ui::BG));
                        }
                    }
                }

                if (x, y) == self.cursor {
                    let above = sy - 1;
                    let below = sy + 1;
                    if above >= 0 {
                        surface.put(
                            (area.left() + sx as u16, area.top() + above as u16),
                            '(',
                            Style::new().fg(ui::ACCENT).bg(ui::BG),
                        );
                    }
                    if below < i32::from(area.height()) {
                        surface.put(
                            (area.left() + sx as u16, area.top() + below as u16),
                            ')',
                            Style::new().fg(ui::ACCENT).bg(ui::BG),
                        );
                    }
                }
            }
        }
    }

    /// Population at the cursor's tile, one entry per faction, for the
    /// bottom readout.
    fn population_at_cursor(&self) -> [f32; FACTIONS] {
        let (x, y) = self.cursor;
        self.map.tile(x, y).population
    }

    fn status(&self) -> String {
        let speed_label = if self.paused {
            "Pause".to_string()
        } else {
            format!("{:.1}x", self.speed)
        };
        format!(
            "Gold: {}  Prices: {}, {}, {}  Speed: {speed_label}",
            self.gold, BUILD_PRICES[0], BUILD_PRICES[1], BUILD_PRICES[2]
        )
    }
}

impl Demo for FlagWar {
    const NAME: &'static str = "25_flag_war";
    const TITLE: &'static str = "25 Flag war";
    const BLURB: &'static str =
        "Territory as colored ASCII trigrams; flags pull population, not orders.";
    const GRID: (u16, u16) = (140, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "cursor"),
            ("Space", "flag"),
            ("X", "clear flags"),
            ("C", "clear half"),
            ("R/V", "build"),
            ("F/Z", "speed"),
            ("P", "pause"),
            ("N", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        if !self.paused && self.speed > 0.0 {
            let sim_dt = dt * self.speed;
            self.flow_timer -= sim_dt;
            self.ai_tick(sim_dt);
            self.refresh_fields();
            // Gold accrues continuously rather than only per flow step, so
            // the counter itself is a second, faster-ticking clock the player
            // can see move even while population is between flow steps.
            let income: f32 =
                (0..FACTIONS).map(|f| self.map.mine_income(f)).sum::<f32>() / FACTIONS as f32;
            self.gold = income.mul_add(sim_dt, f32::from(self.gold as u16)).round() as u32;

            if self.flow_timer <= 0.0 {
                self.flow_timer += FLOW_PERIOD;
                let fields = self.fields.clone_array();
                self.map.flow(&fields);
            }
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        // A dedicated row for the per-faction population readout, split off
        // the content area before the map is drawn: without this the map's
        // own drawing and the readout text would compete for the same row
        // whenever the map is tall enough to reach the bottom of the content
        // area, and the readout would either be overwritten or overwrite a
        // tile.
        let (map_area, readout_area) = if content.height() > 3 {
            panel::split_bottom(content, 1)
        } else {
            (
                content,
                Rect::new(content.left(), content.bottom(), content.width(), 0),
            )
        };

        let offset = self.map_offset(map_area);
        self.draw_map(&mut surface, map_area, offset);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);

        // A compact per-faction population readout at the cursor, mirroring
        // `curseofwar`'s "Population at the cursor" row.
        if readout_area.height() > 0 {
            let y = readout_area.top();
            let pops = self.population_at_cursor();
            let mut x = readout_area.left() + 1;
            let label = "Pop: ";
            surface.print((x, y), label, Style::new().fg(ui::DIM).bg(ui::BG));
            x += label.chars().count() as u16;
            for (f, pop) in pops.iter().enumerate() {
                let text = format!("{pop:>4.0} ");
                surface.print((x, y), &text, Style::new().fg(faction(f)).bg(ui::BG));
                x += text.chars().count() as u16;
            }
        }

        true
    }
}

/// A tiny helper so [`Demo::tick`] can hand a plain array of [`FlagField`]s to
/// [`Map::flow`] without fighting the borrow checker over `self.map` and
/// `self.fields` at the same time.
trait CloneArray {
    fn clone_array(&self) -> [FlagField; FACTIONS];
}

impl CloneArray for [FlagField; FACTIONS] {
    fn clone_array(&self) -> [FlagField; FACTIONS] {
        core::array::from_fn(|i| FlagField {
            distance: self[i].distance.clone(),
        })
    }
}

ascii_tile_demos::demo_main!(FlagWar);
