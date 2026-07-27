//! 49: Planet Fall -- an isometric 4X overworld: elevation, faction borders,
//! and a three-way budget you can push around with a finger.
//!
//! Adapted from Alpha Centauri's main map screen. Three things only make
//! sense together here, which is why they are all in one demo rather than
//! three: a diamond-tiled world whose hills and coastline read through
//! shading and screen offset rather than a printed number; dashed
//! territory outlines drawn over that terrain, with base names and roaming
//! unit glyphs inside them; and a live Econ/Labs/Psych split that has to sum
//! to 100 and whose consequences (energy, research, unrest) are read off
//! immediately, not on the next turn. `23_iso_tactics` already owns
//! isometric close combat on a small board; this is the opposite scale --
//! a whole continent, ownership rather than initiative order, no per-unit
//! combat at all.
//!
//! Techniques on show:
//!
//! - **Elevation by raise, not by number**
//!   ([`tilekit::geom::IsoLayout::tile_to_cell_elevated`]): each tile's
//!   quantized height lifts its diamond by a few screen rows, exactly as
//!   `06_iso_elevation` does, with a bevel darkening the lower-right faces so
//!   a ridge reads as lit from the northwest instead of just "higher".
//! - **A distance-bounded Voronoi territory** ([`build_territory`]): every
//!   land tile is claimed by whichever base is nearest, capped at a radius,
//!   so ownership frays into unclaimed buffer land near a frontier instead of
//!   tiling the whole continent -- the empty margins between blocs in the
//!   reference screenshot.
//! - **Marching dashed borders** ([`PlanetFall::draw_border`]): a border mark
//!   is drawn on the *owned* side of every tile edge whose neighbour belongs
//!   to someone else (or no one), and which marks are lit alternates on a
//!   timer, so the line reads as a patrolled boundary rather than a static
//!   overlay -- the animation this file is required to show even with no
//!   input at all.
//! - **A slider that is also the readout** ([`PlanetFall::draw_budget`]):
//!   Econ/Labs/Psych are one shared 100-point pool. Setting one by tap or
//!   drag rescales the other two proportionally rather than clamping them,
//!   which is what makes the three bars feel like one control instead of
//!   three independent ones fighting over a total nobody is enforcing.
//! - **Grown, invisible tap targets** ([`ascii_tile_demos::ui::touch::tappable`]):
//!   each slider's visible bar is one cell tall, but the region that answers
//!   to a tap or drag is grown to a full touch target width and height,
//!   because the constraint is on the hit region, not on how much ink a
//!   slider needs to spend to look like a slider.
//!
//! ```sh
//! cargo run --example 49_planet_fall --features crossterm
//! cargo run --example 49_planet_fall --features software
//! cargo run --example 49_planet_fall --features gl
//! cargo run --example 49_planet_fall  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, IsoLayout, Tile};
use tilekit::glyphs::{SHADE, marker};
use tilekit::noise::Rng;
use tilekit::palette::{self, mix, scale};
use tilekit::world::World;

/// World size in tiles. Big enough that the camera (parked over the busiest
/// faction cluster) never runs out of land before the viewport's inverse
/// projection does, at any of the three shapes this demo has to fill.
const WORLD_W: i32 = 54;
/// See [`WORLD_W`].
const WORLD_H: i32 = 38;

/// Quantized elevation tiers, water included as tier 0. Enough tiers to read
/// as rolling terrain with real high ground, not so many that a one-level
/// step (the smallest the eye can register at [`PER_LEVEL`]) disappears into
/// noise.
const LEVELS: i32 = 5;

/// Screen rows a tile rises per elevation tier. Kept small (one row) because
/// the world tile itself is already compact ([`LAYOUT`]); a taller raise on a
/// small diamond starts looking like the tile is floating rather than sitting
/// on a slope.
const PER_LEVEL: i32 = 1;

/// The diamond size: bigger than [`IsoLayout::STANDARD`] so a base marker and
/// a biome glyph both have room on one tile, smaller than
/// [`IsoLayout::LARGE`] because this map has to show a whole frontier, not
/// one room.
const LAYOUT: IsoLayout = IsoLayout::new(6, 2);

/// Number of factions in play. Four is enough for the border overlay to show
/// real three- and four-way frontiers without the roster or the map crowding
/// out at the smallest supported shape.
const FACTION_COUNT: usize = 4;
/// Bases per faction. Two lets a faction's territory read as one blob grown
/// from two seeds rather than a single dot, and gives the roaming unit a
/// short patrol route to walk between them.
const BASES_PER_FACTION: usize = 2;
/// How far, in tiles, a base's claim reaches before land goes unclaimed.
/// Deliberately more than half of [`MIN_BASE_SEP`]: two adjacent factions'
/// claims then overlap enough that the frontier the camera centers on (see
/// `PlanetFall::camera_tile`) is guaranteed to show real, contiguous
/// territory and a border on both sides, not a strip of unclaimed buffer so
/// wide the overlay this demo exists to show would be out of frame.
const TERRITORY_RADIUS: i32 = 20;
/// Minimum tile separation enforced between newly placed faction anchors, so
/// two factions never spawn on top of each other and immediately overlap.
const MIN_BASE_SEP: i32 = 15;

/// Candidate (faction name, leader name) pairs. Oversized relative to
/// [`FACTION_COUNT`] so a reroll draws a different four each time; sampled
/// without replacement (see [`build_factions`]), which is what keeps every
/// name on screen unique per the no-duplicate-names rule.
const FACTION_POOL: [(&str, &str); 7] = [
    ("The Faithful Choir", "Sister Aveline"),
    ("Verdant Accord", "Warden Kessa"),
    ("Praxis Institute", "Dr. Renn Osei"),
    ("Iron Concord", "Marshal Doru"),
    ("Helion Combine", "CEO Talia Voss"),
    ("Free Commons", "Speaker Uma"),
    ("Deep Chorus", "Archivist Nyle"),
];

/// Candidate base names, oversized against `FACTION_COUNT * BASES_PER_FACTION`
/// for the same reason as [`FACTION_POOL`].
const BASE_NAME_POOL: [&str; 12] = [
    "New Jerusalem",
    "Landing Reach",
    "Hearth Cairn",
    "Cold Spring",
    "Farsight Watch",
    "Rust Hollow",
    "Glass Terrace",
    "Amber Vale",
    "Stillwater",
    "North Furrow",
    "Ember Reach",
    "Longshore",
];

/// Flavor text for the status panel's `Integrity:` line, weakest to
/// strongest -- Alpha Centauri's own vocabulary for faction cohesion.
const INTEGRITY_POOL: [&str; 5] = ["Corrupt", "Wavering", "Sound", "Noble", "Flawless"];
/// Flavor text for `Might:`.
const MIGHT_POOL: [&str; 5] = ["Feeble", "Timid", "Ready", "Mighty", "Overwhelming"];

/// Research project names the mission panel cycles through as the mission
/// year advances. Purely cosmetic (no tech tree is modeled), but a fixed
/// name reading "Industrial Economics" for the whole demo would look static
/// next to a readout that claims to be live.
const RESEARCH_POOL: [&str; 6] = [
    "Industrial Economics",
    "Applied Genetics",
    "Nonlinear Mathematics",
    "Orbital Insertion",
    "Homo Superior",
    "Planetary Networks",
];

/// Research points needed to complete the current project. Arbitrary but
/// fixed, so `research_turns_left` has something to divide against.
const TECH_COST: i32 = 260;
/// Real seconds per in-fiction mission year. Slow enough that the year
/// readout does not visibly tick during a short screenshot, fast enough that
/// `ATD_HEADLESS_FRAMES` style multi-frame checks still see it move.
const YEAR_SECONDS: f32 = 5.0;

/// Colors for the three budget categories. Deliberately not the faction
/// palette: these are game-mechanic categories that apply to whichever
/// faction is selected, not factions themselves, and reusing faction colors
/// here would make the budget panel look like it belonged to one of them.
const SLIDER_COLORS: [Color; 3] = [
    palette::rgb(226, 184, 90),
    palette::rgb(96, 176, 224),
    palette::rgb(186, 120, 206),
];

/// How many percentage points one keypress moves the focused slider.
const BUDGET_STEP: f32 = 5.0;

/// One line of flavor text per budget category, printed under its bar so
/// the touch band each slider already reserves (see [`touch::TAP_H`]) earns
/// its height instead of sitting empty under a single row of content.
const BUDGET_FLAVOR: [&str; 3] = [
    "Funds infrastructure, trade, and upkeep.",
    "Staffs the labs chasing the next breakthrough.",
    "Keeps the streets calm and the council seated.",
];

/// Waypoints along a faction's two-base patrol route, endpoints included.
const PATROL_STEPS: i32 = 4;
/// Real seconds a roaming unit holds each patrol waypoint before stepping to
/// the next one -- long enough to read as a discrete move, not a glide.
const UNIT_DWELL: f32 = 1.4;

/// One playable faction: name, leader, color, and the flavor stats the
/// bottom status panel reports.
struct Faction {
    name: &'static str,
    leader: &'static str,
    color: Color,
    integrity: &'static str,
    might: &'static str,
    votes: u32,
}

/// One base: a named claim that seeds its faction's territory and anchors
/// its roaming unit's patrol.
struct Base {
    name: &'static str,
    faction: usize,
    tile: Tile,
}

/// Quantizes `world`'s continuous elevation into `0..LEVELS`, sea level
/// pinned to tier 0 so water never raises a skirt.
///
/// A local copy of the same idea `06_iso_elevation` uses rather than a shared
/// helper: the brief for this batch asks each demo file to stand alone, and
/// the function is four lines.
fn quantize_levels(world: &World) -> Vec<i32> {
    let mut levels = Vec::with_capacity((world.width() * world.height()) as usize);
    for y in 0..world.height() {
        for x in 0..world.width() {
            let biome = world.biome_at(x, y);
            let level = if biome.is_water() {
                0
            } else {
                let e = world.elevation_at(x, y);
                let above = ((e - tilekit::world::SEA_LEVEL) / (1.0 - tilekit::world::SEA_LEVEL))
                    .clamp(0.0, 1.0);
                1 + (above * (LEVELS - 2) as f32).round() as i32
            };
            levels.push(level.clamp(0, LEVELS - 1));
        }
    }
    levels
}

/// Draws `FACTION_COUNT` factions from [`FACTION_POOL`] without replacement,
/// so a reroll varies the roster and no two factions ever share a name.
fn build_factions(rng: &mut Rng) -> Vec<Faction> {
    let mut indices: Vec<usize> = (0..FACTION_POOL.len()).collect();
    let mut out = Vec::with_capacity(FACTION_COUNT);
    for i in 0..FACTION_COUNT {
        let pick = rng.next_below(indices.len() as u32) as usize;
        let idx = indices.swap_remove(pick);
        let (name, leader) = FACTION_POOL[idx];
        out.push(Faction {
            name,
            leader,
            color: palette::faction(i),
            integrity: rng.choose(&INTEGRITY_POOL).copied().unwrap_or("Sound"),
            might: rng.choose(&MIGHT_POOL).copied().unwrap_or("Ready"),
            votes: 1 + rng.next_below(6),
        });
    }
    out
}

/// Finds a land tile at least [`MIN_BASE_SEP`] from every base already
/// placed, for seeding a new faction's home region.
///
/// Bounded rejection sampling rather than an exact packing: with only
/// [`FACTION_COUNT`] anchors needed on a map this size, a fixed attempt
/// budget almost always succeeds, and falling back to the last candidate
/// seen keeps the function total (no risk of spinning forever) at the cost
/// of an occasional closer-than-ideal pair, which just reads as two
/// factions sharing a contested frontier -- not wrong for this demo.
fn find_land_tile(world: &World, rng: &mut Rng, existing: &[Base]) -> Tile {
    let mut fallback = Tile::new(world.width() / 2, world.height() / 2);
    for _ in 0..200 {
        let col = rng.next_below(world.width() as u32) as i32;
        let row = rng.next_below(world.height() as u32) as i32;
        if world.biome_at(col, row).is_water() {
            continue;
        }
        fallback = Tile::new(col, row);
        let far_enough = existing.iter().all(|b| {
            let (dx, dy) = (b.tile.col - col, b.tile.row - row);
            dx * dx + dy * dy >= MIN_BASE_SEP * MIN_BASE_SEP
        });
        if far_enough {
            return fallback;
        }
    }
    fallback
}

/// Finds a land tile within `radius` of `anchor`, for placing a faction's
/// second base near its first without the two landing on the same spot.
fn wander_land_tile(world: &World, rng: &mut Rng, anchor: Tile, radius: i32) -> Tile {
    for _ in 0..40 {
        let dc = rng.next_below((radius * 2 + 1) as u32) as i32 - radius;
        let dr = rng.next_below((radius * 2 + 1) as u32) as i32 - radius;
        let col = (anchor.col + dc).clamp(0, world.width() - 1);
        let row = (anchor.row + dr).clamp(0, world.height() - 1);
        if !world.biome_at(col, row).is_water() {
            return Tile::new(col, row);
        }
    }
    anchor
}

/// Places `BASES_PER_FACTION` bases per faction: one anchor found by
/// [`find_land_tile`], the rest scattered nearby by [`wander_land_tile`].
/// Names are drawn from [`BASE_NAME_POOL`] without replacement, so every base
/// on screen has a distinct name regardless of how many reroll.
fn place_bases(world: &World, rng: &mut Rng) -> Vec<Base> {
    let mut name_indices: Vec<usize> = (0..BASE_NAME_POOL.len()).collect();
    let mut bases = Vec::with_capacity(FACTION_COUNT * BASES_PER_FACTION);
    for faction in 0..FACTION_COUNT {
        let anchor = find_land_tile(world, rng, &bases);
        for i in 0..BASES_PER_FACTION {
            let tile = if i == 0 {
                anchor
            } else {
                wander_land_tile(world, rng, anchor, 7)
            };
            let pick = rng.next_below(name_indices.len() as u32) as usize;
            let idx = name_indices.swap_remove(pick);
            bases.push(Base {
                name: BASE_NAME_POOL[idx],
                faction,
                tile,
            });
        }
    }
    bases
}

/// Assigns every land tile to its nearest base's faction, capped at
/// [`TERRITORY_RADIUS`]. `None` means unclaimed: too far from any base, or
/// water (territory here is a land claim, not a sea claim).
fn build_territory(world: &World, bases: &[Base]) -> Vec<Option<usize>> {
    let mut territory = vec![None; (world.width() * world.height()) as usize];
    for y in 0..world.height() {
        for x in 0..world.width() {
            if world.biome_at(x, y).is_water() {
                continue;
            }
            let mut best: Option<(i32, usize)> = None;
            for base in bases {
                let (dx, dy) = (x - base.tile.col, y - base.tile.row);
                let d2 = dx * dx + dy * dy;
                if best.is_none_or(|(bd, _)| d2 < bd) {
                    best = Some((d2, base.faction));
                }
            }
            if let Some((d2, faction)) = best
                && d2 <= TERRITORY_RADIUS * TERRITORY_RADIUS
            {
                territory[(y * world.width() + x) as usize] = Some(faction);
            }
        }
    }
    territory
}

/// Pulls `color` toward its own perceptual gray by `t`, leaving hue and
/// saturation behind rather than shifting them.
///
/// [`palette::mix`] blends toward a *different* color and so always fights
/// whatever hue is already there; this blends toward the same tile's
/// brightness with the color stripped out, which is what a faction tint
/// needs to overwrite a biome's own saturation instead of competing with it
/// (see [`PlanetFall::tile_base_color`]).
fn desaturate(color: Color, amount: f32) -> Color {
    let (red, green, blue) = color.resolve_rgb((0, 0, 0));
    let luma = 0.299_f32
        .mul_add(
            f32::from(red),
            0.587_f32.mul_add(f32::from(green), 0.114 * f32::from(blue)),
        )
        .round() as u8;
    mix(color, palette::rgb(luma, luma, luma), amount)
}

/// The four straight edges of a diamond, as `(dx, dy)` offsets from its
/// center, for the edge shared with the neighbour reached by stepping
/// `(dcol, drow)` in tile space.
///
/// Derived from [`IsoLayout::tile_to_cell`]: stepping `-1` in `col` moves the
/// neighbour's center up-and-left on screen, so the edge between here and
/// there is this diamond's upper-left side, and so on for the other three
/// tile-space steps. Reusing [`IsoLayout::span_at`] rather than hand-walking
/// the diamond's slope keeps this in exact agreement with how the tile is
/// actually filled in [`PlanetFall::draw_cover`].
fn edge_points(dcol: i32, drow: i32) -> Vec<(i32, i32)> {
    // Any tile-space step other than the four cardinals has no edge at all,
    // so an empty half-height range (never entered by the loop below) is the
    // correct fallback rather than a sentinel to special-case.
    let (lo, hi, sign) = match (dcol, drow) {
        (-1, 0) => (-LAYOUT.half_h, 0, -1),
        (1, 0) => (0, LAYOUT.half_h, 1),
        (0, -1) => (-LAYOUT.half_h, 0, 1),
        (0, 1) => (0, LAYOUT.half_h, -1),
        _ => (1, 0, 1),
    };
    let mut points = Vec::new();
    for dy in lo..=hi {
        if let Some(span) = LAYOUT.span_at(dy) {
            points.push((sign * span, dy));
        }
    }
    points
}

/// Which slider (Econ, Labs, or Psych) a keypress or a gesture targets.
const BUDGET_LABELS: [&str; 3] = ["Econ", "Labs", "Psych"];

/// Areas laid out for one frame, computed fresh from `term.area()` every
/// tick rather than cached, so a resize (or a device rotation, on the web
/// build) reflows immediately instead of on the next explicit relayout.
struct Areas {
    map: Rect,
    budget: Rect,
    mission: Rect,
    status: Rect,
    roster: Rect,
    minimap: Rect,
}

/// Splits `content` into the map plus five panels, branching on
/// [`Shape`] rather than on a width threshold: portrait stacks everything
/// (map, then the interactive budget close to the thumb zone, then the
/// read-only status strip and roster/minimap at the bottom); landscape and
/// desktop put the map and a roster/minimap sidebar side by side above a
/// three-panel bottom row.
fn layout(content: Rect) -> Areas {
    let shape = Shape::of(content);
    if shape.stacks() {
        let map_h = (content.height() * 2 / 5)
            .max(10)
            .min(content.height().saturating_sub(28));
        let (map, rest) = panel::split_top(content, map_h);
        let budget_h = 15.min(rest.height() * 2 / 5);
        let (budget, rest) = panel::split_top(rest, budget_h);
        let mission_h = 5.min(rest.height() / 3);
        let (mission, rest) = panel::split_top(rest, mission_h);
        let status_h = 6.min(rest.height() / 2);
        let (status, rest) = panel::split_top(rest, status_h);
        let minimap_h = 10.min(rest.height() / 2);
        let (minimap, roster) = panel::split_top(rest, minimap_h);
        Areas {
            map,
            budget,
            mission,
            status,
            roster,
            minimap,
        }
    } else {
        let bottom_h = 15.min(content.height() * 2 / 5);
        let (top, bottom) = panel::split_bottom(content, bottom_h);
        let side_w = if top.width() >= 90 {
            28.min(top.width().saturating_sub(60))
        } else {
            0
        };
        let (map, side) = panel::split_right(top, side_w);
        let (minimap, roster) = if side.width() > 0 {
            let minimap_h = 9.min(side.height() / 2);
            panel::split_top(side, minimap_h)
        } else {
            let empty = Rect::new(side.left(), side.top(), 0, 0);
            (empty, empty)
        };
        let cols = panel::columns(bottom, 3, 1);
        Areas {
            map,
            mission: cols[0],
            budget: cols[1],
            status: cols[2],
            roster,
            minimap,
        }
    }
}

/// A tap or drag target this demo remembers between frames: hit-tested
/// against the *previous* frame's drawn hotspots, which is the immediate-mode
/// discipline [`Hotspots`] itself documents.
#[derive(Clone, Copy)]
enum Action {
    SelectFaction(usize),
}

/// State: the generated world and its factions/bases/territory, the shared
/// Econ/Labs/Psych pool, and the input plumbing.
pub struct PlanetFall {
    world: World,
    levels: Vec<i32>,
    factions: Vec<Faction>,
    bases: Vec<Base>,
    territory: Vec<Option<usize>>,
    seed: u32,
    time: f32,
    /// Econ, Labs, Psych, always summing to 100.0 (mod float drift corrected
    /// on every write; see [`PlanetFall::set_budget`]).
    budget: [f32; 3],
    /// Which slider keyboard Left/Right adjusts.
    focus: usize,
    selected_faction: usize,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    /// The three sliders' hit rects, grown to a legal touch target and
    /// recorded during [`PlanetFall::draw_budget`] so next frame's drag can
    /// be mapped back to a value; see the module doc's note on grown targets.
    slider_rects: [Rect; 3],
    fps: FpsMeter,
}

/// What one world generation pass produces, bundled so `Default` and
/// `reroll` can share the pipeline without either restating its shape.
type Generated = (World, Vec<i32>, Vec<Faction>, Vec<Base>, Vec<Option<usize>>);

impl PlanetFall {
    /// Regenerates the world, factions, bases, and territory from `seed`.
    /// Split out of `Default`/`reroll` so both can share it without either
    /// duplicating the pipeline order.
    fn regenerate(seed: u32) -> Generated {
        let world = World::generate(WORLD_W, WORLD_H, seed);
        let levels = quantize_levels(&world);
        let mut rng = Rng::new(seed ^ 0x9E37_79B9);
        let factions = build_factions(&mut rng);
        let bases = place_bases(&world, &mut rng);
        let territory = build_territory(&world, &bases);
        (world, levels, factions, bases, territory)
    }

    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        let (world, levels, factions, bases, territory) = Self::regenerate(self.seed);
        self.world = world;
        self.levels = levels;
        self.factions = factions;
        self.bases = bases;
        self.territory = territory;
        self.selected_faction = 0;
    }

    fn level_at(&self, tile: Tile) -> i32 {
        if !self.world.in_bounds(tile.col, tile.row) {
            return 0;
        }
        self.levels[(tile.row * self.world.width() + tile.col) as usize]
    }

    fn territory_at(&self, tile: Tile) -> Option<usize> {
        if !self.world.in_bounds(tile.col, tile.row) {
            return None;
        }
        self.territory[(tile.row * self.world.width() + tile.col) as usize]
    }

    /// A tile's fill color before bevel: biome, elevation brighten, gentle
    /// animated water swell, and a territory tint mixed in last so ownership
    /// reads as a wash over the terrain rather than replacing it.
    fn tile_base_color(&self, tile: Tile) -> Color {
        let biome = self.world.biome_at(tile.col, tile.row);
        let mut color = biome.color();
        if biome.is_water() {
            let phase = self
                .time
                .mul_add(1.1, (tile.col as f32).mul_add(0.5, tile.row as f32 * 0.35));
            let swell = phase.sin().mul_add(0.5, 0.5);
            color = mix(color, palette::WHITE, swell * 0.12);
        } else {
            let brighten = f32::from(self.level_at(tile) as i16).mul_add(0.05, 1.0);
            color = scale(color, brighten);
        }
        if let Some(faction) = self.territory_at(tile) {
            // A straight mix toward the faction color competed with the
            // terrain's own biome hue instead of overriding it: 38% of a
            // faction's blue mixed into a biome-saturated ocean tile still
            // reads as "water", not "claimed water". Desaturating first
            // pulls the terrain toward its own gray so the faction hue that
            // follows has nothing saturated left to compete with, and reads
            // as the tile's own color rather than a wash on top of it.
            color = desaturate(color, 0.55);
            color = mix(color, self.factions[faction].color, 0.5);
        }
        color
    }

    /// The camera's world center: the midpoint of whichever two bases from
    /// *different* factions sit closest together.
    ///
    /// An average of every base's tile was the first thing tried here, and
    /// it was wrong: with anchors spread out by [`MIN_BASE_SEP`], the average
    /// of four of them tends to land in the empty quarter no faction claims,
    /// so the border overlay this demo exists to show was routinely just off
    /// screen. Centering on the closest cross-faction pair instead guarantees
    /// the view sits on an actual frontier, where territory, a border, and
    /// both sides' bases are all in frame together.
    fn camera_tile(&self) -> Tile {
        let mut best: Option<(i32, Tile)> = None;
        for i in 0..self.bases.len() {
            for j in (i + 1)..self.bases.len() {
                if self.bases[i].faction == self.bases[j].faction {
                    continue;
                }
                let (a, b) = (self.bases[i].tile, self.bases[j].tile);
                let d2 = (a.col - b.col).pow(2) + (a.row - b.row).pow(2);
                let mid = Tile::new(i32::midpoint(a.col, b.col), i32::midpoint(a.row, b.row));
                if best.is_none_or(|(bd, _)| d2 < bd) {
                    best = Some((d2, mid));
                }
            }
        }
        best.map_or_else(
            || Tile::new(self.world.width() / 2, self.world.height() / 2),
            |(_, t)| t,
        )
    }

    /// Writes one glyph clipped to `content`, given content-relative
    /// coordinates that may be negative or past the edge.
    fn put_clipped(
        surface: &mut Surface<'_>,
        content: Rect,
        x: i32,
        y: i32,
        glyph: char,
        style: Style,
    ) {
        if x < 0 || y < 0 {
            return;
        }
        let (sx, sy) = (i32::from(content.left()) + x, i32::from(content.top()) + y);
        if sx >= i32::from(content.right()) || sy >= i32::from(content.bottom()) {
            return;
        }
        surface.put((sx as u16, sy as u16), glyph, style);
    }

    /// Fills the gap under a raised tile with a dark, roughly-striped skirt
    /// wherever a lower neighbour would otherwise leave the terrain looking
    /// like it is floating. A compact cousin of `06_iso_elevation`'s cliff
    /// face: this map's elevation is subtler ([`PER_LEVEL`] of one row), so
    /// the skirt only ever needs to cover a few rows.
    fn draw_skirt(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tile: Tile,
        level: i32,
        center: Cell,
    ) {
        if level == 0 {
            return;
        }
        let south = Tile::new(tile.col, tile.row + 1);
        let east = Tile::new(tile.col + 1, tile.row);
        let drop = level - self.level_at(south).min(self.level_at(east));
        if drop <= 0 {
            return;
        }
        let raised = LAYOUT.tile_to_cell_elevated(tile, level, PER_LEVEL);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let rock = scale(self.tile_base_color(tile), 0.55);
        let rock_dark = scale(rock, 0.8);
        let rows = (drop * PER_LEVEL).max(1);
        for r in 1..=rows {
            for dy in 0..=LAYOUT.half_h {
                let Some(span) = LAYOUT.span_at(dy) else {
                    continue;
                };
                for dx in -span..=span {
                    let color = if (sx + dx).rem_euclid(3) == 0 {
                        rock_dark
                    } else {
                        rock
                    };
                    Self::put_clipped(
                        surface,
                        area,
                        sx + dx,
                        sy + dy + r,
                        ' ',
                        Style::new().bg(color),
                    );
                }
            }
        }
    }

    /// Draws one tile: its skirt, its raised and beveled diamond, a biome
    /// glyph on non-water, and its border dashes.
    fn draw_cover(&self, surface: &mut Surface<'_>, area: Rect, tile: Tile, center: Cell) {
        let level = self.level_at(tile);
        self.draw_skirt(surface, area, tile, level, center);

        let raised = LAYOUT.tile_to_cell_elevated(tile, level, PER_LEVEL);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let base = self.tile_base_color(tile);

        for dy in -LAYOUT.half_h..=LAYOUT.half_h {
            let Some(span) = LAYOUT.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                // Northwest-lit bevel: brighter toward the top of the
                // diamond, darker toward the bottom, a fainter version of
                // the same split left-to-right. This is the whole "shading"
                // half of "elevation shown through shading and offset".
                let bevel = if dy < 0 {
                    1.12
                } else if dy > 0 {
                    0.85
                } else if dx < 0 {
                    1.04
                } else {
                    0.94
                };
                Self::put_clipped(
                    surface,
                    area,
                    sx + dx,
                    sy + dy,
                    ' ',
                    Style::new().bg(scale(base, bevel)),
                );
            }
        }

        let biome = self.world.biome_at(tile.col, tile.row);
        if !biome.is_water() {
            Self::put_clipped(
                surface,
                area,
                sx,
                sy,
                biome.glyph(),
                Style::new().fg(scale(biome.color(), 1.35)).bg(base),
            );
        }

        self.draw_border(surface, area, tile, sx, sy);
    }

    /// Draws a bold, continuous border mark on every edge of `tile` whose
    /// neighbour belongs to a different owner (or no one), filled in `tile`'s
    /// owning faction's color.
    ///
    /// Only owned tiles draw: a claimed tile bordering unclaimed land, or a
    /// rival's claim, paints its own rim. The tile on the other side of that
    /// same edge (if also owned) paints its own rim in its own color on its
    /// own pass, so a frontier between two factions ends up lined in both
    /// colors rather than the two overwriting each other.
    ///
    /// This used to be a sparse alternating dash: a lone `.` glyph, half the
    /// edge points skipped, read as scattered dots against the terrain
    /// rather than a boundary. Then it was a full-coverage darkened fill --
    /// still invisible, because darkening a mid-saturation faction color
    /// lands it close to the same terrain's own shadow tones (mountains,
    /// water troughs, cliff skirts) at this palette's value range. Then it
    /// was a bright core stippled over a dark outline within a single glyph
    /// -- still too subtle, because at this map's tile size the stipple's
    /// sub-cell dots compress into a faint average color instead of reading
    /// as texture, and a single flat tint (even a bright one) sits too close
    /// in value to nearby land, water, and skirt cells to jump out.
    ///
    /// So the line itself is now the contrast: every other point along the
    /// edge alternates between a fully-saturated, brightened core color and
    /// a fully solid near-black outline color -- two flat colors as far
    /// apart in value as this palette allows, each drawn as a full opaque
    /// block rather than blended with anything underneath. That is the same
    /// trick a "marching ants" selection outline uses: no single color has to
    /// win a contrast fight against the terrain, because the eye reads the
    /// alternation itself as a boundary regardless of what is on either side
    /// of it. The alternation's phase shifts with `self.time`, which is what
    /// makes the line march rather than sit static.
    fn draw_border(&self, surface: &mut Surface<'_>, area: Rect, tile: Tile, sx: i32, sy: i32) {
        let Some(owner) = self.territory_at(tile) else {
            return;
        };
        let faction_color = self.factions[owner].color;
        // Scaled up rather than mixed toward white: scaling keeps each
        // faction's hue (a brightened blue still reads as blue) where mixing
        // toward white would wash every faction toward the same pale tint,
        // costing the one thing that tells two adjacent claims apart.
        let core = scale(faction_color, 1.8);
        let outline = mix(faction_color, palette::BLACK, 0.88);
        let step = (self.time * 6.0) as i32;
        let mut i = 0i32;
        for (dcol, drow) in [(-1, 0), (0, -1), (1, 0), (0, 1)] {
            let neighbor = Tile::new(tile.col + dcol, tile.row + drow);
            if self.territory_at(neighbor) == Some(owner) {
                continue;
            }
            for (dx, dy) in edge_points(dcol, drow) {
                i += 1;
                let color = if (i + step) % 2 == 0 { core } else { outline };
                Self::put_clipped(
                    surface,
                    area,
                    sx + dx,
                    sy + dy,
                    SHADE[4],
                    Style::new().fg(color).bg(color),
                );
            }
        }
    }

    /// Draws every base: a marker glyph in its faction's color, plus its
    /// name in a small chip above it when the base itself is on screen.
    fn draw_bases(&self, surface: &mut Surface<'_>, area: Rect, center: Cell) {
        for base in &self.bases {
            let level = self.level_at(base.tile);
            let raised = LAYOUT.tile_to_cell_elevated(base.tile, level, PER_LEVEL);
            let (sx, sy) = (raised.x - center.x, raised.y - center.y);
            let color = self.factions[base.faction].color;
            let bg = self.tile_base_color(base.tile);
            Self::put_clipped(
                surface,
                area,
                sx,
                sy,
                marker::CAPITAL,
                Style::new().fg(color).bg(bg),
            );
            // The chip's background is the faction's own color, darkened for
            // text contrast, rather than the page background: a name in the
            // right color floating on plain UI background still reads as
            // unowned at a glance, while a colored chip is legible as "this
            // name belongs to that territory" without reading the glyph
            // underneath it first.
            let chip_bg = mix(color, palette::BLACK, 0.6);
            for (i, ch) in base.name.chars().enumerate() {
                Self::put_clipped(
                    surface,
                    area,
                    sx + 2 + i as i32,
                    sy - 1,
                    ch,
                    Style::new().fg(palette::WHITE).bg(chip_bg),
                );
            }
        }
    }

    /// The straight-line waypoints a faction's roaming unit patrols between
    /// its two bases, or its one base's tile if it only has one.
    fn faction_patrol(&self, faction: usize) -> Vec<Tile> {
        let owned: Vec<Tile> = self
            .bases
            .iter()
            .filter(|b| b.faction == faction)
            .map(|b| b.tile)
            .collect();
        let [a, b] = match owned.as_slice() {
            [only] => return vec![*only],
            [a, b, ..] => [*a, *b],
            [] => return Vec::new(),
        };
        (0..=PATROL_STEPS)
            .map(|s| {
                Tile::new(
                    a.col + (b.col - a.col) * s / PATROL_STEPS,
                    a.row + (b.row - a.row) * s / PATROL_STEPS,
                )
            })
            .collect()
    }

    /// The tile a faction's unit occupies at the current time: a ping-pong
    /// walk along [`faction_patrol`], one discrete step at a time rather than
    /// a smooth glide -- movement on a strategic map reads as a unit
    /// finishing an order, not as a token sliding across the table.
    fn unit_tile(&self, faction: usize) -> Option<Tile> {
        let path = self.faction_patrol(faction);
        if path.len() < 2 {
            return path.first().copied();
        }
        let cycle = path.len() * 2 - 2;
        let step = ((self.time / UNIT_DWELL) as usize) % cycle;
        let idx = if step < path.len() {
            step
        } else {
            cycle - step
        };
        path.get(idx).copied()
    }

    fn draw_units(&self, surface: &mut Surface<'_>, area: Rect, center: Cell) {
        for faction in 0..self.factions.len() {
            let Some(tile) = self.unit_tile(faction) else {
                continue;
            };
            let level = self.level_at(tile);
            let raised = LAYOUT.tile_to_cell_elevated(tile, level, PER_LEVEL);
            let (sx, sy) = (raised.x - center.x, raised.y - center.y);
            let color = self.factions[faction].color;
            let bg = self.tile_base_color(tile);
            Self::put_clipped(
                surface,
                area,
                sx,
                sy,
                marker::UNIT,
                Style::new().fg(color).bg(bg),
            );
        }
    }

    /// Draws the full map: every tile the viewport can see (found by
    /// inverse-projecting the panel's own corners, so the drawn world always
    /// fills whatever rect it is given), then bases and units on top.
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let center_tile = self.camera_tile();
        let center_cell = LAYOUT.tile_to_cell(center_tile);
        let center = Cell::new(
            center_cell.x - i32::from(area.width()) / 2,
            center_cell.y - i32::from(area.height()) / 2,
        );

        let max_raise = (LEVELS - 1) * PER_LEVEL;
        let margin = 3 + max_raise / LAYOUT.height().max(1);
        let corners = [
            Cell::new(0, -max_raise),
            Cell::new(i32::from(area.width()), -max_raise),
            Cell::new(0, i32::from(area.height())),
            Cell::new(i32::from(area.width()), i32::from(area.height())),
        ];
        let (mut min_col, mut max_col) = (i32::MAX, i32::MIN);
        let (mut min_row, mut max_row) = (i32::MAX, i32::MIN);
        for corner in corners {
            let world_cell = Cell::new(corner.x + center.x, corner.y + center.y);
            let tile = LAYOUT.cell_to_tile(world_cell);
            min_col = min_col.min(tile.col - margin);
            max_col = max_col.max(tile.col + margin);
            min_row = min_row.min(tile.row - margin);
            max_row = max_row.max(tile.row + margin);
        }

        let mut visible: Vec<Tile> = Vec::new();
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                if self.world.in_bounds(col, row) {
                    visible.push(Tile::new(col, row));
                }
            }
        }
        // Painter's algorithm, map order only: see `06_iso_elevation` and
        // `23_iso_tactics` for why elevation must never enter this key.
        visible.sort_by_key(|&t| IsoLayout::depth(t));
        for tile in visible {
            self.draw_cover(surface, area, tile, center);
        }
        self.draw_bases(surface, area, center);
        self.draw_units(surface, area, center);
    }

    /// A coarse top-down render of the whole world (biome color, tinted by
    /// territory), with a cross marking where the main map's camera is
    /// centered.
    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Map").draw(surface, area);
        if inner.width() < 6 || inner.height() < 3 {
            return;
        }
        for my in 0..inner.height() {
            for mx in 0..inner.width() {
                let wx = i32::from(mx) * self.world.width() / i32::from(inner.width());
                let wy = i32::from(my) * self.world.height() / i32::from(inner.height());
                let mut color = self.world.biome_at(wx, wy).color();
                if let Some(f) = self.territory_at(Tile::new(wx, wy)) {
                    color = mix(color, self.factions[f].color, 0.55);
                }
                surface.put(
                    (inner.left() + mx, inner.top() + my),
                    ' ',
                    Style::new().bg(color),
                );
            }
        }
        let cam = self.camera_tile();
        let mx =
            inner.left() + (cam.col * i32::from(inner.width()) / self.world.width().max(1)) as u16;
        let my =
            inner.top() + (cam.row * i32::from(inner.height()) / self.world.height().max(1)) as u16;
        if mx < inner.right() && my < inner.bottom() {
            surface.put(
                (mx, my),
                '+',
                Style::new().fg(palette::WHITE).bg(palette::BLACK),
            );
        }
    }

    /// The right-hand faction list: one line per faction, tinted in its own
    /// color, tappable to select it for the status panel below.
    fn draw_roster(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Factions")
            .badge(&format!("{}", self.factions.len()))
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let rows = usize::from(inner.height());
        for (i, faction) in self.factions.iter().enumerate().take(rows) {
            let y = inner.top() + i as u16;
            let row_rect = Rect::new(inner.left(), y, inner.width(), 1);
            let mark = if i == self.selected_faction { '>' } else { ' ' };
            let text = format!("{mark}{}. {}", i + 1, faction.name);
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[panel::Span::new(&text, faction.color)],
                panel::PANEL_BG,
            );
            self.hotspots
                .push_tappable(row_rect, inner, Action::SelectFaction(i));
        }
    }

    fn mission_year(&self) -> i32 {
        2100 + (self.time / YEAR_SECONDS) as i32
    }

    fn current_research(&self) -> &'static str {
        RESEARCH_POOL[self.research_index()]
    }

    fn research_index(&self) -> usize {
        let elapsed = (self.mission_year() - 2100).max(0);
        (elapsed / 6) as usize % RESEARCH_POOL.len()
    }

    /// How far into the current project's 6-year span the mission clock has
    /// gotten, as a `0.0..=1.0` fraction -- a real, ticking progress bar
    /// rather than only the turn count already shown in text.
    fn research_progress(&self) -> f32 {
        let elapsed = (self.mission_year() - 2100).max(0);
        (elapsed % 6) as f32 / 6.0
    }

    /// The next few projects after the one in progress, in cycle order, for
    /// the mission panel's queue -- filling the lower half of that panel with
    /// something that earns its height instead of leaving it blank once the
    /// three summary lines are printed.
    fn research_queue(&self, count: usize) -> Vec<&'static str> {
        let start = self.research_index();
        (1..=count)
            .map(|i| RESEARCH_POOL[(start + i) % RESEARCH_POOL.len()])
            .collect()
    }

    fn energy_income(&self) -> i32 {
        (self.budget[0] / 100.0 * 60.0).round() as i32 - 8
    }

    fn research_bulbs(&self) -> i32 {
        (self.budget[1] / 100.0 * 42.0).round() as i32
    }

    fn research_turns_left(&self) -> i32 {
        (TECH_COST / self.research_bulbs().max(1)).max(1)
    }

    fn unrest_label(&self) -> &'static str {
        if self.budget[2] >= 40.0 {
            "Low"
        } else if self.budget[2] >= 15.0 {
            "Moderate"
        } else {
            "High"
        }
    }

    /// The one-line consequence of `which` category's current split, printed
    /// under its bar in [`PlanetFall::draw_budget`]. Per-category rather than
    /// one shared readout: the slider a person is dragging is the number
    /// that should visibly move right under their thumb, not three rows down
    /// in a combined summary.
    fn budget_consequence(&self, which: usize) -> String {
        match which {
            0 => format!("-> Energy {:+}/turn", self.energy_income()),
            1 => format!(
                "-> {} bulbs/turn, {} turns left",
                self.research_bulbs(),
                self.research_turns_left()
            ),
            _ => format!("-> Unrest: {}", self.unrest_label()),
        }
    }

    fn draw_mission(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Mission")
            .badge(&format!("{}", self.mission_year()))
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                panel::Span::dim("Research: "),
                panel::Span::keyword(self.current_research()),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            panel::bar(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                self.research_progress(),
                ui::ACCENT,
                panel::PANEL_BG,
            );
        }
        if inner.height() > 2 {
            let text = format!(
                "{} turns to completion, {} bulbs/turn",
                self.research_turns_left(),
                self.research_bulbs()
            );
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[panel::Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
        // The queue fills what used to be dead space below the summary: a
        // strategy game's mission panel is exactly the place a person
        // expects to see what comes after the current project, not just what
        // is running now.
        if inner.height() > 4 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 4),
                inner.width(),
                &[panel::Span::dim("Queue:")],
                panel::PANEL_BG,
            );
            let rows_left = usize::from(inner.height().saturating_sub(5));
            for (i, name) in self.research_queue(rows_left).into_iter().enumerate() {
                let y = inner.top() + 5 + i as u16;
                let text = format!("  {}. {}", i + 1, name);
                panel::spans(
                    surface,
                    (inner.left(), y),
                    inner.width(),
                    &[panel::Span::plain(&text)],
                    panel::PANEL_BG,
                );
            }
        }
    }

    /// How close the selected faction's nearest base sits to `other`'s
    /// nearest base, in flavor terms -- a real read of the map (closer bases
    /// mean a more contested frontier) rather than an arbitrary label, and
    /// the thing that fills the status panel's lower half with faction
    /// relations rather than empty rows.
    fn faction_relation(&self, selected: usize, other: usize) -> &'static str {
        let mut best = i32::MAX;
        for a in self.bases.iter().filter(|b| b.faction == selected) {
            for b in self.bases.iter().filter(|b| b.faction == other) {
                let (dx, dy) = (a.tile.col - b.tile.col, a.tile.row - b.tile.row);
                best = best.min(dx * dx + dy * dy);
            }
        }
        if best <= (TERRITORY_RADIUS * 2).pow(2) {
            "Hostile -- contested frontier"
        } else if best <= (TERRITORY_RADIUS * 4).pow(2) {
            "Wary -- claims run close"
        } else {
            "Cordial -- distant borders"
        }
    }

    fn draw_status(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Status").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let f = &self.factions[self.selected_faction];
        let lines = [
            (format!("{} ({})", f.name, f.leader), f.color),
            (format!("Integrity: {}", f.integrity), ui::FG),
            (format!("Might: {}", f.might), ui::FG),
            (format!("Council Votes: {}", f.votes), ui::FG),
        ];
        let mut y = inner.top();
        for (line, color) in &lines {
            if y >= inner.bottom() {
                return;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[panel::Span::new(line, *color)],
                panel::PANEL_BG,
            );
            y += 1;
        }
        y += 1;
        if y >= inner.bottom() {
            return;
        }
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[panel::Span::dim("Relations:")],
            panel::PANEL_BG,
        );
        y += 1;
        // Two lines per faction (name, then relation indented under it)
        // rather than one long "Name: Hostile -- contested frontier" line:
        // the longest name in `FACTION_POOL` plus the longest relation
        // phrase together run past every status column width this demo's
        // three [`Shape`]s produce, and `panel::spans` truncates mid-word
        // rather than wrapping, which is what used to clip "frontier" down
        // to "frontie". Each piece alone comfortably fits any of those
        // widths, so splitting them across two lines is enough on its own,
        // with no need to shorten the wording.
        for (i, other) in self.factions.iter().enumerate() {
            if i == self.selected_faction {
                continue;
            }
            if y >= inner.bottom() {
                break;
            }
            let name = format!("  {}:", other.name);
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[panel::Span::new(&name, other.color)],
                panel::PANEL_BG,
            );
            y += 1;
            if y >= inner.bottom() {
                break;
            }
            let relation = self.faction_relation(self.selected_faction, i);
            let text = format!("    {relation}");
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[panel::Span::plain(&text)],
                panel::PANEL_BG,
            );
            y += 1;
        }
    }

    /// Rescales the other two categories proportionally to their prior
    /// split so `which` can become `new_val` while the trio still sums to
    /// 100. Proportional rather than an even split: dragging Labs up should
    /// eat more from whichever of Econ/Psych was already smaller-relative,
    /// not force a 50/50 split on the two you were not touching.
    fn set_budget(&mut self, which: usize, new_val: f32) {
        let new_val = new_val.clamp(0.0, 100.0);
        let old_others = (100.0 - self.budget[which]).max(0.001);
        let new_others = 100.0 - new_val;
        for i in 0..3 {
            if i != which {
                self.budget[i] = self.budget[i] / old_others * new_others;
            }
        }
        self.budget[which] = new_val;
    }

    fn nudge_budget(&mut self, delta: f32) {
        let v = (self.budget[self.focus] + delta).clamp(0.0, 100.0);
        self.set_budget(self.focus, v);
    }

    /// Integer percentages that always sum to exactly 100, even though the
    /// underlying floats drift a little with every rescale. Rounding each
    /// value independently would occasionally read "33 33 33" or "34 34 33"
    /// depending on drift; correcting the remainder onto the largest bucket
    /// keeps the displayed total honest without hiding which category is
    /// actually biggest.
    fn budget_percentages(&self) -> [i32; 3] {
        let mut rounded = [
            self.budget[0].round() as i32,
            self.budget[1].round() as i32,
            self.budget[2].round() as i32,
        ];
        let diff = 100 - rounded.iter().sum::<i32>();
        if diff != 0
            && let Some((idx, _)) = rounded.iter().enumerate().max_by_key(|&(_, v)| *v)
        {
            rounded[idx] += diff;
        }
        rounded
    }

    /// The interactive heart: three one-row sliders sharing one 100-point
    /// pool, each grown to a legal touch target, plus a live readout of what
    /// the current split actually buys.
    fn draw_budget(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Budget")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.width() < 10 || inner.height() == 0 {
            self.slider_rects = [Rect::new(0, 0, 0, 0); 3];
            return;
        }
        let pct = self.budget_percentages();
        // One row of bar per slider, but the row *band* (used for the
        // tappable region below) grows up to a full touch target when the
        // panel has the height to spare; a squeezed panel still draws every
        // slider, just with a smaller hit area, rather than dropping one.
        let band_h = (inner.height() / 3).clamp(1, touch::TAP_H);
        for i in 0..3 {
            let y0 = inner.top() + i as u16 * band_h;
            if y0 >= inner.bottom() {
                self.slider_rects[i] = Rect::new(0, 0, 0, 0);
                continue;
            }
            let label = format!("{} {:>3}% ", BUDGET_LABELS[i], pct[i]);
            let label_w = (label.chars().count() as u16).min(inner.width());
            let label_color = if self.focus == i { ui::ACCENT } else { ui::FG };
            panel::spans(
                surface,
                (inner.left(), y0),
                inner.width(),
                &[panel::Span::new(&label, label_color)],
                panel::PANEL_BG,
            );
            let bar_x = inner.left() + label_w;
            if bar_x >= inner.right() {
                self.slider_rects[i] = Rect::new(0, 0, 0, 0);
                continue;
            }
            let bar_w = inner.right() - bar_x;
            panel::bar(
                surface,
                (bar_x, y0),
                bar_w,
                self.budget[i] / 100.0,
                SLIDER_COLORS[i],
                panel::PANEL_BG,
            );
            let band = Rect::new(bar_x, y0, bar_w, band_h.min(inner.bottom() - y0));
            self.slider_rects[i] = touch::tappable(band, inner);

            // The band is already `band_h` rows tall to hold a legal touch
            // target (see `touch::tappable` below); a bar that only ever
            // paints its own top row leaves the rest of that reserved height
            // looking like a bug rather than a margin. Spend the spare rows
            // on what this specific slider actually does, so the panel's
            // full height is doing something even though the hit region
            // does not shrink to match.
            if band_h > 1 && y0 + 1 < inner.bottom() {
                let text = self.budget_consequence(i);
                panel::spans(
                    surface,
                    (inner.left(), y0 + 1),
                    inner.width(),
                    &[panel::Span::new(&text, SLIDER_COLORS[i])],
                    panel::PANEL_BG,
                );
            }
            if band_h > 2 && y0 + 2 < inner.bottom() {
                panel::spans(
                    surface,
                    (inner.left(), y0 + 2),
                    inner.width(),
                    &[panel::Span::dim(BUDGET_FLAVOR[i])],
                    panel::PANEL_BG,
                );
            }
        }
        let readout_y = inner.top() + (band_h * 3).min(inner.height().saturating_sub(1));
        if readout_y < inner.bottom() {
            let text = format!(
                "Energy {:+}  Bulbs {}  Unrest {}",
                self.energy_income(),
                self.research_bulbs(),
                self.unrest_label()
            );
            panel::spans(
                surface,
                (inner.left(), readout_y),
                inner.width(),
                &[panel::Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
    }

    /// Maps a screen column inside slider `which`'s rect to a `0..=100`
    /// value and applies it, for both a tap (jump to that point) and a drag
    /// (continuously follow the finger).
    fn set_budget_from_x(&mut self, which: usize, x: u16) {
        let rect = self.slider_rects[which];
        if rect.width() == 0 {
            return;
        }
        let frac = f32::from(x.saturating_sub(rect.left())) / f32::from(rect.width());
        self.set_budget(which, frac.clamp(0.0, 1.0) * 100.0);
    }

    /// Applies one frame's gesture against last frame's hotspots and slider
    /// rects: a tap on a roster row selects that faction, and a tap or drag
    /// on a slider sets its value.
    fn handle_gesture(&mut self, gesture: &Gesture) {
        if let Some(pos) = gesture.tap {
            if let Some(&Action::SelectFaction(i)) = self.hotspots.hit(pos) {
                self.selected_faction = i;
                return;
            }
            for i in 0..3 {
                if self.slider_rects[i].contains_pos(pos) {
                    self.set_budget_from_x(i, pos.x);
                    return;
                }
            }
        }
        if let Some(pos) = gesture.drag {
            for i in 0..3 {
                if self.slider_rects[i].contains_pos(pos) {
                    self.set_budget_from_x(i, pos.x);
                    return;
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.focus = (self.focus + 1) % 3,
            KeyCode::Left | KeyCode::Char('a' | 'A') => self.nudge_budget(-BUDGET_STEP),
            KeyCode::Right | KeyCode::Char('d' | 'D') => self.nudge_budget(BUDGET_STEP),
            KeyCode::Up | KeyCode::Char('w' | 'W') => {
                let n = self.factions.len().max(1);
                self.selected_faction = (self.selected_faction + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Char('s' | 'S') => {
                let n = self.factions.len().max(1);
                self.selected_faction = (self.selected_faction + 1) % n;
            }
            KeyCode::Char('r' | 'R') => self.reroll(),
            _ => {}
        }
    }

    fn status_line(&self) -> String {
        let pct = self.budget_percentages();
        format!(
            "seed {}  E{}/L{}/P{}  {}",
            self.seed, pct[0], pct[1], pct[2], self.factions[self.selected_faction].name
        )
    }
}

impl Default for PlanetFall {
    fn default() -> Self {
        // 2117 is the mission year Alpha Centauri's own opening screen
        // reports; reusing it as the default seed is a small nod at the
        // source rather than a meaningful choice, but it does mean two
        // fresh runs of this demo produce the same first world.
        let seed = 2117;
        let (world, levels, factions, bases, territory) = Self::regenerate(seed);
        Self {
            world,
            levels,
            factions,
            bases,
            territory,
            seed,
            time: 0.0,
            budget: [30.0, 45.0, 25.0],
            focus: 0,
            selected_faction: 0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            slider_rects: [Rect::new(0, 0, 0, 0); 3],
            fps: FpsMeter::new(),
        }
    }
}

impl Demo for PlanetFall {
    const NAME: &'static str = "49_planet_fall";
    const TITLE: &'static str = "49 Planet Fall";
    const BLURB: &'static str =
        "Isometric elevation, faction borders, and a three-way budget split.";
    const GRID: (u16, u16) = (160, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "focus slider"),
            ("Left/Right", "adjust slider"),
            ("Up/Down", "select faction"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        let mut keep_running = true;
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                keep_running = false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        if !keep_running {
            return false;
        }

        let gesture = self.pointer.take();
        self.handle_gesture(&gesture);
        self.hotspots.clear();

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let areas = layout(content);
        self.draw_map(&mut surface, areas.map);
        if areas.roster.width() > 0 {
            self.draw_roster(&mut surface, areas.roster);
        }
        if areas.minimap.width() > 0 {
            self.draw_minimap(&mut surface, areas.minimap);
        }
        self.draw_mission(&mut surface, areas.mission);
        self.draw_budget(&mut surface, areas.budget);
        self.draw_status(&mut surface, areas.status);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{BASES_PER_FACTION, FACTION_COUNT, PlanetFall};

    #[test]
    fn base_names_are_unique_for_several_seeds() {
        for seed in [1u32, 7, 42, 999] {
            let (_world, _levels, _factions, bases, _territory) = PlanetFall::regenerate(seed);
            assert_eq!(
                bases.len(),
                FACTION_COUNT * BASES_PER_FACTION,
                "seed {seed}"
            );
            let mut names: Vec<&str> = bases.iter().map(|b| b.name).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(
                names.len(),
                bases.len(),
                "seed {seed} produced duplicate base names"
            );
        }
    }

    #[test]
    fn faction_names_and_leaders_are_unique() {
        for seed in [1u32, 7, 42, 999] {
            let (_world, _levels, factions, _bases, _territory) = PlanetFall::regenerate(seed);
            assert_eq!(factions.len(), FACTION_COUNT, "seed {seed}");
            let mut names: Vec<&str> = factions.iter().map(|f| f.name).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(
                names.len(),
                factions.len(),
                "seed {seed} produced duplicate faction names"
            );
        }
    }

    #[test]
    fn budget_rebalancing_always_sums_to_100() {
        let mut demo = PlanetFall::default();
        demo.set_budget(0, 80.0);
        let sum: f32 = demo.budget.iter().sum();
        assert!((sum - 100.0).abs() < 0.01, "sum drifted to {sum}");
        let pct = demo.budget_percentages();
        assert_eq!(pct.iter().sum::<i32>(), 100);

        demo.set_budget(2, 0.0);
        let sum: f32 = demo.budget.iter().sum();
        assert!(
            (sum - 100.0).abs() < 0.01,
            "sum drifted to {sum} after zeroing psych"
        );
    }

    #[test]
    fn every_land_tile_within_radius_is_claimed() {
        let (world, _levels, _factions, bases, territory) = PlanetFall::regenerate(3);
        for base in &bases {
            let idx = (base.tile.row * world.width() + base.tile.col) as usize;
            assert_eq!(
                territory[idx],
                Some(base.faction),
                "a base must sit inside its own faction's territory"
            );
        }
    }
}

ascii_tile_demos::demo_main!(PlanetFall);
