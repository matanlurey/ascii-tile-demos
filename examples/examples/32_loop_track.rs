//! 32: Loop Track -- Loop Hero's premise on a character grid: an
//! uncontrollable hero walks an endless road while you build the world
//! around him.
//!
//! Every other demo in this gallery lets the player move something. This one
//! deliberately does not: the hero advances tile by tile along a closed loop
//! at a fixed pace driven by `frame.delta`, forever, with no key or tap that
//! touches him directly. That single constraint is what turns the demo into
//! a placement puzzle rather than a movement game -- the only lever the
//! player has is what the loop passes *through*, not where it goes or how
//! fast. Placing a card is the whole verb set.
//!
//! Techniques on show:
//!
//! - **A deterministic rectangular loop with jitter**
//!   ([`generate_path`]): the ring is a plain rectangle whose four sides each
//!   get a one-cell notch, bulged in or out by a value drawn from
//!   [`tilekit::noise::Rng`] seeded from a fixed constant. The notch is built
//!   from five waypoints per side and a straight-line walk between each pair,
//!   so the result is always a single simple polygon (no self-intersection
//!   is possible once the notch depth is small relative to the rectangle) and
//!   always exactly reproducible: same seed, same loop, every run and every
//!   test.
//! - **Autotiled road, at tile scale rather than cell scale**
//!   ([`LoopTrack::road_cell`] + [`tilekit::autotile::mask4`] +
//!   [`tilekit::autotile::BOX_SINGLE`]): each road tile looks at which of its
//!   four cardinal neighbours are also road and draws a connector glyph
//!   through its own centre, then extends a `─`/`│` run from that centre out
//!   to whichever edges the mask says connect. The effect is the same
//!   autotiled-wall trick `21_deck_plan.rs` uses for a single cell, scaled up
//!   to a 9x5 tile so the loop reads as one continuous road rather than a row
//!   of disconnected squares -- which is exactly the failure mode a
//!   multi-cell board risks if each tile is drawn in isolation.
//! - **Tile-enter combat, not per-frame combat** ([`LoopTrack::simulate`]):
//!   the hero's position is a continuous float, but a fight is triggered only
//!   on the frame its *floor* changes, i.e. once per tile crossing. That is
//!   what lets the hero appear to walk smoothly (the point of an idle-loop
//!   game) while combat still resolves as a single, discrete exchange with a
//!   result you can read, rather than a mush of tiny damage ticks.
//! - **Highlight-before-tap instead of hover-tooltip**
//!   ([`LoopTrack::legal_slots`] + [`LoopTrack::register_hotspots`]): a
//!   desktop build of this idea would let a dragged card hover over an
//!   illegal tile and grey it out on contact. Touch has no hover -- the
//!   first place a finger ever contacts the screen is where it commits -- so
//!   every legal destination for the selected card must already be lit
//!   *before* the tap happens. `legal_slots` recomputes the full legal set
//!   the instant a card is selected (or a placement changes the board), and
//!   only tiles in that set both draw a highlight and register a
//!   [`ascii_tile_demos::ui::touch::Hotspots`] entry, so what is glowing is
//!   always exactly what is tappable.
//! - **Placement rules with genuine adjacency dependencies**
//!   ([`LoopTrack::is_legal`]): three distinct rule shapes -- road-only
//!   (Rock), empty-ground-beside-the-road (Wheat Fields, Grove), and
//!   empty-ground-adjacent-to-another-tile (Spider Cocoon needs a Grove;
//!   Vampire Mansion needs the road for its first placement and then any
//!   existing Mansion for the rest) -- so a Cocoon is a real, load-bearing
//!   decision about where the Grove went, not cosmetic variety.
//! - **Multi-cell cards as the hand** ([`ascii_tile_demos::ui::card`]): five
//!   terrain cards plus an Undo action card, fanned along the bottom thumb
//!   zone. The card's cost badge is repurposed to show the placement-rule tag
//!   (`ROAD`/`GRND`/`ADJ`) rather than a resource cost, because that is the
//!   one fact a stubbed-down card must still carry: which slots it can even
//!   go in.
//! - **Camera panning with clamped drag** ([`LoopTrack::handle_pointer`]):
//!   the generated loop is comfortably larger than a phone viewport, so a
//!   drag over the map with no card selected pans the camera; the same drag
//!   started over a card instead becomes a drag-to-slot placement. Distance
//!   from the loop's own bounding box, not a magic constant, decides how far
//!   the camera is allowed to wander past the edge.
//!
//! ```sh
//! cargo run --example 32_loop_track --features crossterm
//! cargo run --example 32_loop_track --features software
//! cargo run --example 32_loop_track --features gl
//! cargo run --example 32_loop_track  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{self, Card, CardState};
use ascii_tile_demos::ui::panel::{self, Log, Span};
use ascii_tile_demos::ui::touch::{self, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, E, N, S, W, mask4};
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb, scale};

/// Width of the base rectangle the loop is generated from, in tile-slots.
///
/// Slots, not cells: [`TILE_W`]/[`TILE_H`] is the multi-cell footprint of one
/// tile, and the loop is laid out at that coarser resolution so every tile on
/// it -- road or terrain -- is guaranteed to meet the gallery's "one glyph is
/// never one interactive unit" rule without any extra scaling logic.
const RING_W: i32 = 14;
/// See [`RING_W`].
const RING_H: i32 = 8;

/// Cell width of one loop tile. [`touch::TAP_W`] exactly, so a legal tile's
/// own footprint is already a legal touch target and its hotspot never needs
/// to be grown past its neighbours -- growing it would make two adjacent
/// legal tiles overlap, which is the one thing
/// [`Hotspots::push_tappable`](touch::Hotspots::push_tappable) warns against.
const TILE_W: i32 = touch::TAP_W as i32;
/// Cell height of one loop tile. [`touch::TAP_H`] plus one row, which is what
/// buys the terrain tiles a bordered box with a glyph row *and* a label row
/// inside it -- exactly [`touch::TAP_H`] leaves no room for both.
const TILE_H: i32 = touch::TAP_H as i32 + 1;

/// How many loop-tiles the hero crosses per second. Chosen so a full circuit
/// of the generated loop (40-60 tiles, depending on how the jitter lands)
/// takes well under a minute: fast enough that a few loops complete during a
/// normal look at the demo, slow enough that a tile crossing is still a
/// legible event rather than a blur.
const HERO_SPEED: f32 = 1.1;

/// Passive hit-point regeneration per second. Small and constant rather than
/// tied to anything else, so the loop stays survivable indefinitely without
/// requiring the player to fill every Wheat Field perfectly.
const HERO_REGEN: f32 = 0.4;

/// How fast a floating damage number rises, in cells per second.
const FLOAT_RISE: f32 = 3.0;

/// Margin, in slots, added on every side of the generated loop's bounding
/// box so each road tile has room for at least one adjacent ground tile.
const MARGIN: i32 = 1;

/// Fixed generation seed. Never wall-clock time: the loop, and therefore the
/// entire game, must be bit-identical between two runs and between two
/// renders of the same run, which is exactly what the gallery's determinism
/// test checks.
const SEED: u32 = 20_240_814;

/// One terrain card in the hand. [`Rock`](Self::Rock) is placed directly on
/// the road; the rest are placed on empty ground and each spawns a monster
/// that attaches itself to the nearest road tile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CardKind {
    Wheat,
    Rock,
    Grove,
    Cocoon,
    Mansion,
}

/// Display text and the placement-rule tag for one [`CardKind`].
struct CardMeta {
    name: &'static str,
    /// Shown in the card's cost badge. Repurposed from "what it costs" to
    /// "where it can go" -- see the module docs on why that survives to the
    /// smallest card tier.
    tag: &'static str,
    rule: &'static str,
    accent: Color,
}

impl CardKind {
    const ALL: [Self; 5] = [
        Self::Wheat,
        Self::Rock,
        Self::Grove,
        Self::Cocoon,
        Self::Mansion,
    ];

    const fn meta(self) -> CardMeta {
        match self {
            Self::Wheat => CardMeta {
                name: "Wheat Fields",
                tag: "GRND",
                rule: "Empty ground beside the road. Draws a rat; steady loot.",
                accent: rgb(214, 178, 88),
            },
            Self::Rock => CardMeta {
                name: "Rock",
                tag: "ROAD",
                rule: "On the road itself. No foe; hardens the loot to come.",
                accent: rgb(150, 150, 150),
            },
            Self::Grove => CardMeta {
                name: "Grove",
                tag: "GRND",
                rule: "Empty ground beside the road. Wolves take shelter.",
                accent: rgb(90, 150, 80),
            },
            Self::Cocoon => CardMeta {
                name: "Spider Cocoon",
                tag: "ADJ",
                rule: "Empty ground touching a Grove. Breeds spiders.",
                accent: rgb(150, 120, 170),
            },
            Self::Mansion => CardMeta {
                name: "Vampire Mansion",
                tag: "ADJ",
                rule: "By the road, or beside another Mansion. Vampires hunt.",
                accent: rgb(150, 60, 80),
            },
        }
    }

    const fn terrain(self) -> Option<TerrainKind> {
        match self {
            Self::Rock => None,
            Self::Wheat => Some(TerrainKind::Wheat),
            Self::Grove => Some(TerrainKind::Grove),
            Self::Cocoon => Some(TerrainKind::Cocoon),
            Self::Mansion => Some(TerrainKind::Mansion),
        }
    }

    const fn monster(self) -> Option<MonsterKind> {
        match self {
            Self::Rock => None,
            Self::Wheat => Some(MonsterKind::Rat),
            Self::Grove => Some(MonsterKind::Wolf),
            Self::Cocoon => Some(MonsterKind::Spider),
            Self::Mansion => Some(MonsterKind::Vampire),
        }
    }
}

/// Terrain planted on empty ground. Each variant's glyph, colour, and label
/// letter are what [`terrain_cell`] draws inside a tile's border.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TerrainKind {
    Wheat,
    Grove,
    Cocoon,
    Mansion,
}

impl TerrainKind {
    const fn visual(self) -> (char, Color, char) {
        match self {
            Self::Wheat => ('\u{2261}', rgb(214, 178, 88), 'W'),
            Self::Grove => ('\u{2663}', rgb(90, 150, 80), 'G'),
            Self::Cocoon => ('\u{0398}', rgb(150, 120, 170), 'C'),
            Self::Mansion => ('\u{03A9}', rgb(150, 60, 80), 'M'),
        }
    }
}

/// A monster spawned by a terrain card, permanently attached to one road
/// tile until it is killed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MonsterKind {
    Rat,
    Wolf,
    Spider,
    Vampire,
}

impl MonsterKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Rat => "Rat",
            Self::Wolf => "Wolf",
            Self::Spider => "Spider",
            Self::Vampire => "Vampire",
        }
    }

    const fn glyph(self) -> char {
        match self {
            Self::Rat => 'r',
            Self::Wolf => 'w',
            Self::Spider => 's',
            Self::Vampire => 'V',
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Rat => rgb(170, 150, 110),
            Self::Wolf => rgb(190, 130, 90),
            Self::Spider => rgb(160, 100, 190),
            Self::Vampire => rgb(200, 50, 70),
        }
    }

    /// Base `(hp, attack, defence)` before the current loop's difficulty
    /// scale is applied. See [`LoopTrack::place`].
    const fn base_stats(self) -> (f32, f32, f32) {
        match self {
            Self::Rat => (8.0, 3.0, 0.0),
            Self::Wolf => (16.0, 5.0, 1.0),
            Self::Spider => (20.0, 6.0, 1.0),
            Self::Vampire => (34.0, 9.0, 3.0),
        }
    }
}

/// Loot awarded for a kill, cycling through a fixed six-slot sequence so the
/// equipment grid always shows the six most recent finds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EquipKind {
    Sword,
    Shield,
    Amulet,
    Boots,
    Ring,
    Cloak,
}

impl EquipKind {
    const ALL: [Self; 6] = [
        Self::Sword,
        Self::Shield,
        Self::Amulet,
        Self::Boots,
        Self::Ring,
        Self::Cloak,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Sword => "Sword",
            Self::Shield => "Shield",
            Self::Amulet => "Amulet",
            Self::Boots => "Boots",
            Self::Ring => "Ring",
            Self::Cloak => "Cloak",
        }
    }

    const fn glyph(self) -> char {
        match self {
            Self::Sword => '/',
            Self::Shield => ']',
            Self::Amulet => 'o',
            Self::Boots => 'L',
            Self::Ring => '0',
            Self::Cloak => '~',
        }
    }

    /// `(attack, defence, max-hp)` granted on pickup.
    const fn bonus(self) -> (f32, f32, f32) {
        match self {
            Self::Sword => (3.0, 0.0, 0.0),
            Self::Shield => (0.0, 2.0, 0.0),
            Self::Amulet => (0.0, 0.0, 10.0),
            Self::Boots => (1.0, 1.0, 0.0),
            Self::Ring => (0.0, 3.0, 0.0),
            Self::Cloak => (2.0, 0.0, 0.0),
        }
    }
}

/// What one tile-slot in the world grid holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotKind {
    /// Part of the walked loop. `rock` marks a Rock card placed on it.
    Road { rock: bool },
    /// Off the loop. `Some` once a terrain card has been planted.
    Ground(Option<TerrainKind>),
}

/// A monster on the board. Never removed from [`LoopTrack::monsters`], only
/// marked `alive = false`, so [`Monster`] indices stay stable for the undo
/// history to reference.
struct Monster {
    kind: MonsterKind,
    /// Index into [`LoopTrack::path`] of the road tile this monster guards.
    road_index: usize,
    hp: f32,
    max_hp: f32,
    atk: f32,
    def: f32,
    alive: bool,
}

/// A rising, fading damage number.
struct Floater {
    x: f32,
    y: f32,
    text: String,
    color: Color,
    age: f32,
    ttl: f32,
}

/// Enough state to revert exactly one placement: what the slot held before,
/// and which monster (if any) was spawned alongside it.
///
/// Only the *most recent* placement is kept, deliberately -- see the module
/// docs' note on Into the Breach's single-step-undo model, which this copies
/// rather than a full history stack. A stack would let the player replay the
/// whole game backwards, which is more mechanism than the "don't punish a
/// mis-tap" goal actually needs.
struct Placement {
    slot: (i32, i32),
    prev: SlotKind,
    monster_idx: Option<usize>,
}

/// What tapping or clicking one registered region means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    SelectCard(usize),
    PlaceAt(i32, i32),
    Undo,
}

/// Highlight/cursor state passed into the per-tile drawing helpers, bundled
/// into one value so those helpers stay under the arity clippy accepts
/// without losing readability at the call site.
#[derive(Clone, Copy)]
struct CellFlags {
    highlighted: bool,
    cursor: bool,
}

/// The full demo state: the generated loop, the board built on top of it, the
/// hero and his stats, the monsters and floaters, the hand, and the touch and
/// keyboard input machinery.
pub struct LoopTrack {
    grid_w: i32,
    grid_h: i32,
    slots: Vec<SlotKind>,
    /// The road, in board order, as a closed sequence of grid-space slot
    /// coordinates. `path[i]` and `path[(i + 1) % path.len()]` are always
    /// cardinally adjacent; see [`generate_path`].
    path: Vec<(i32, i32)>,

    /// The hero's position along [`Self::path`], as a continuous index: the
    /// integer part is the tile he is on or just left, the fractional part
    /// is how far into the next tile he has walked. Never controlled by
    /// input; see [`Self::simulate`].
    hero_pos: f32,
    hero_hp: f32,
    hero_max_hp: f32,
    hero_atk: f32,
    hero_def: f32,
    loop_count: u32,

    monsters: Vec<Monster>,
    floaters: Vec<Floater>,
    equipment: [Option<EquipKind>; 6],
    loot_count: usize,

    hand: [CardKind; 5],
    selected: Option<usize>,
    /// Every grid-space slot the selected card may legally go into this
    /// frame. Recomputed whenever the selection or the board changes; see
    /// the module docs on why this must exist *before* a tap, not after.
    legal: Vec<(i32, i32)>,
    /// Keyboard placement cursor, in grid-space slot coordinates.
    cursor: (i32, i32),
    last_placement: Option<Placement>,

    /// Camera offset, in world cells.
    scroll: (i32, i32),
    pointer: Pointer,
    hotspots: Hotspots<Action>,

    log: Log,
    time: f32,
    /// Shared pulse phase for the legal-slot highlight, `0.0..=1.0`. Driven
    /// by [`Self::time`], never wall-clock time, so it is exactly
    /// reproducible.
    pulse: f32,
    fps: FpsMeter,
}

impl Default for LoopTrack {
    fn default() -> Self {
        let (path, grid_w, grid_h, slots) = build_world(SEED);
        let mut log = Log::new(48);
        log.push("The hero sets out. The loop is yours to fill.", ui::FG);
        log.push("Tap a card, then tap a lit tile to place it.", ui::DIM);

        Self {
            grid_w,
            grid_h,
            slots,
            path,
            hero_pos: 0.0,
            hero_hp: 40.0,
            hero_max_hp: 40.0,
            hero_atk: 6.0,
            hero_def: 2.0,
            loop_count: 0,
            monsters: Vec::new(),
            floaters: Vec::new(),
            equipment: [None; 6],
            loot_count: 0,
            hand: CardKind::ALL,
            selected: None,
            legal: Vec::new(),
            cursor: (grid_w / 2, grid_h / 2),
            last_placement: None,
            scroll: (0, 0),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            log,
            time: 0.0,
            pulse: 0.0,
            fps: FpsMeter::new(),
        }
    }
}

/// Builds a closed rectangular loop with one jittered notch per side.
///
/// A plain rectangle would be a valid loop but a visually dead one, and a
/// fully randomised polygon risks self-intersection (a road that crosses
/// itself has no sensible "which way is forward"). This splits the
/// difference: the shape is a rectangle almost everywhere, and each side gets
/// exactly one small rectangular dent, bulged in or out by [`Rng::next_below`]
/// sampling `{-1, 0, 1}` slots. A dent that shallow can never reach far enough
/// to cross an opposite side or a neighbouring dent given [`RING_W`]/
/// [`RING_H`], so the result is guaranteed to stay a simple closed curve
/// without needing a general-purpose self-intersection check.
fn generate_path(seed: u32) -> Vec<(i32, i32)> {
    let mut rng = Rng::new(seed);
    let bump = |rng: &mut Rng| rng.next_below(3) as i32 - 1;
    let (w, h) = (RING_W, RING_H);
    let (mx, my) = (w / 2, h / 2);

    let mut pts = vec![(0, 0)];
    push_side(&mut pts, (w, 0), mx, 0, true, bump(&mut rng)); // top
    push_side(&mut pts, (w, h), my, w, false, bump(&mut rng)); // right
    push_side(&mut pts, (0, h), mx, h, true, bump(&mut rng)); // bottom
    push_side(&mut pts, (0, 0), my, 0, false, bump(&mut rng)); // left
    pts.pop(); // drop the point that duplicates the starting corner
    pts
}

/// Walks from the last point in `pts` to `to`, inserting a one-notch detour
/// around `mid` if `bump != 0`. `horizontal` says which axis the side nominally
/// runs along (top/bottom vs left/right), which decides whether the bump
/// offsets `y` (a horizontal side bulging up or down) or `x` (a vertical side
/// bulging left or right).
fn push_side(
    pts: &mut Vec<(i32, i32)>,
    to: (i32, i32),
    mid: i32,
    fixed: i32,
    horizontal: bool,
    bump: i32,
) {
    let from = *pts
        .last()
        .expect("the starting corner is always pushed first");
    if bump == 0 {
        walk(pts, from, to);
        return;
    }
    if horizontal {
        let y = fixed;
        let step = (to.0 - from.0).signum();
        let p1 = (mid - step, y);
        let p2 = (mid - step, y + bump);
        let p3 = (mid + step, y + bump);
        let p4 = (mid + step, y);
        walk(pts, from, p1);
        walk(pts, p1, p2);
        walk(pts, p2, p3);
        walk(pts, p3, p4);
        walk(pts, p4, to);
    } else {
        let x = fixed;
        let step = (to.1 - from.1).signum();
        let p1 = (x, mid - step);
        let p2 = (x + bump, mid - step);
        let p3 = (x + bump, mid + step);
        let p4 = (x, mid + step);
        walk(pts, from, p1);
        walk(pts, p1, p2);
        walk(pts, p2, p3);
        walk(pts, p3, p4);
        walk(pts, p4, to);
    }
}

/// Appends every cell from `from` (exclusive) to `to` (inclusive) along a
/// straight cardinal run. A no-op if `from == to`.
fn walk(pts: &mut Vec<(i32, i32)>, from: (i32, i32), to: (i32, i32)) {
    let (x0, y0) = from;
    let (x1, y1) = to;
    if x0 == x1 {
        let step = (y1 - y0).signum();
        let mut y = y0;
        while y != y1 {
            y += step;
            pts.push((x0, y));
        }
    } else if y0 == y1 {
        let step = (x1 - x0).signum();
        let mut x = x0;
        while x != x1 {
            x += step;
            pts.push((x, y0));
        }
    }
}

/// Generates the loop and the dense slot grid it sits on.
///
/// The grid is [`generate_path`]'s bounding box plus a one-slot margin on
/// every side, so every road tile has room for at least one adjacent ground
/// tile to place Wheat Fields or a Grove beside it. Storage is a plain
/// row-major `Vec`, not a map keyed by coordinate, specifically so nothing in
/// this file ever needs to iterate a hash map to draw the board -- see the
/// gallery's determinism rule.
fn build_world(seed: u32) -> (Vec<(i32, i32)>, i32, i32, Vec<SlotKind>) {
    let raw = generate_path(seed);
    let min_x = raw.iter().map(|p| p.0).min().unwrap_or(0);
    let max_x = raw.iter().map(|p| p.0).max().unwrap_or(0);
    let min_y = raw.iter().map(|p| p.1).min().unwrap_or(0);
    let max_y = raw.iter().map(|p| p.1).max().unwrap_or(0);
    let ox = min_x - MARGIN;
    let oy = min_y - MARGIN;
    let grid_w = max_x - min_x + 1 + MARGIN * 2;
    let grid_h = max_y - min_y + 1 + MARGIN * 2;
    let path: Vec<(i32, i32)> = raw.into_iter().map(|(x, y)| (x - ox, y - oy)).collect();

    let mut slots = vec![SlotKind::Ground(None); (grid_w * grid_h) as usize];
    for &(gx, gy) in &path {
        let i = (gy * grid_w + gx) as usize;
        slots[i] = SlotKind::Road { rock: false };
    }
    (path, grid_w, grid_h, slots)
}

/// The centre world-cell of slot `slot`.
const fn tile_center(slot: (i32, i32)) -> (i32, i32) {
    (slot.0 * TILE_W + TILE_W / 2, slot.1 * TILE_H + TILE_H / 2)
}

impl LoopTrack {
    const fn idx(&self, gx: i32, gy: i32) -> Option<usize> {
        if gx < 0 || gy < 0 || gx >= self.grid_w || gy >= self.grid_h {
            return None;
        }
        Some((gy * self.grid_w + gx) as usize)
    }

    fn slot_at(&self, gx: i32, gy: i32) -> Option<SlotKind> {
        self.idx(gx, gy).map(|i| self.slots[i])
    }

    fn is_road(&self, gx: i32, gy: i32) -> bool {
        matches!(self.slot_at(gx, gy), Some(SlotKind::Road { .. }))
    }

    fn is_terrain(&self, gx: i32, gy: i32, kind: TerrainKind) -> bool {
        matches!(self.slot_at(gx, gy), Some(SlotKind::Ground(Some(k))) if k == kind)
    }

    fn adjacent_road(&self, gx: i32, gy: i32) -> bool {
        [(0, -1), (1, 0), (0, 1), (-1, 0)]
            .iter()
            .any(|&(dx, dy)| self.is_road(gx + dx, gy + dy))
    }

    fn adjacent_terrain(&self, gx: i32, gy: i32, kind: TerrainKind) -> bool {
        [(0, -1), (1, 0), (0, 1), (-1, 0)]
            .iter()
            .any(|&(dx, dy)| self.is_terrain(gx + dx, gy + dy, kind))
    }

    /// Whether `card` may legally be placed at grid slot `(gx, gy)` right now.
    ///
    /// Three distinct rule shapes on purpose, so the hand is not five copies
    /// of the same decision: Rock only targets the road itself; Wheat Fields
    /// and Grove only target empty ground touching the road; Spider Cocoon
    /// only targets empty ground touching an existing Grove, which is what
    /// makes planting a Grove first a real, load-bearing choice rather than
    /// flavour. Vampire Mansion is legal beside the road (so the first one is
    /// always plantable) *or* beside an existing Mansion (so a nest can grow
    /// away from the road once seeded) -- one card, two ways in, deliberately
    /// mirroring how Loop Hero itself lets a building's family expand.
    fn is_legal(&self, card: CardKind, gx: i32, gy: i32) -> bool {
        match self.slot_at(gx, gy) {
            Some(SlotKind::Road { rock }) => card == CardKind::Rock && !rock,
            Some(SlotKind::Ground(occupied)) => {
                if occupied.is_some() {
                    return false;
                }
                match card {
                    CardKind::Rock => false,
                    CardKind::Wheat | CardKind::Grove => self.adjacent_road(gx, gy),
                    CardKind::Cocoon => self.adjacent_terrain(gx, gy, TerrainKind::Grove),
                    CardKind::Mansion => {
                        self.adjacent_road(gx, gy)
                            || self.adjacent_terrain(gx, gy, TerrainKind::Mansion)
                    }
                }
            }
            None => false,
        }
    }

    /// Every legal slot for `card`, scanned across the whole grid.
    ///
    /// The grid is at most a few hundred slots, so a full scan every time the
    /// selection changes is cheap enough to redo per frame rather than cache
    /// incrementally -- and redoing it in full is what keeps it trivially
    /// correct after an undo, which can turn an illegal slot legal again in
    /// ways that would be easy to miss with incremental bookkeeping.
    fn legal_slots(&self, card: CardKind) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for gy in 0..self.grid_h {
            for gx in 0..self.grid_w {
                if self.is_legal(card, gx, gy) {
                    out.push((gx, gy));
                }
            }
        }
        out
    }

    fn nearest_road_index(&self, slot: (i32, i32)) -> usize {
        self.path
            .iter()
            .enumerate()
            .min_by_key(|&(_, &(px, py))| (px - slot.0).abs() + (py - slot.1).abs())
            .map_or(0, |(i, _)| i)
    }

    /// Commits `card` at `slot`, recording enough state in
    /// [`Self::last_placement`] to undo it. Does not check legality; callers
    /// (`try_place`) are responsible for that, so this stays a pure "make it
    /// so" step.
    fn place(&mut self, card: CardKind, slot: (i32, i32)) {
        let Some(i) = self.idx(slot.0, slot.1) else {
            return;
        };
        let prev = self.slots[i];
        let mut monster_idx = None;

        if card == CardKind::Rock {
            if let SlotKind::Road { rock } = &mut self.slots[i] {
                *rock = true;
            }
            self.log.push("Rock settles into the road.", ui::DIM);
        } else {
            let terrain = card
                .terrain()
                .expect("every non-Rock card carries a terrain kind");
            self.slots[i] = SlotKind::Ground(Some(terrain));
            self.log
                .push(format!("{} takes root.", card.meta().name), ui::FG);

            if let Some(mk) = card.monster() {
                let road_idx = self.nearest_road_index(slot);
                // Difficulty rises with the loop count, not with how many
                // monsters already exist: a card planted on loop ten is
                // dangerous the moment it appears, matching Loop Hero's own
                // rule that the *world*, not the individual spawn, gets
                // harder over time.
                let scale = (self.loop_count as f32).mul_add(0.18, 1.0);
                let (hp, atk, def) = mk.base_stats();
                self.monsters.push(Monster {
                    kind: mk,
                    road_index: road_idx,
                    hp: hp * scale,
                    max_hp: hp * scale,
                    atk: atk * scale,
                    def,
                    alive: true,
                });
                monster_idx = Some(self.monsters.len() - 1);
                self.log
                    .push(format!("{} stirs nearby.", mk.name()), rgb(216, 140, 90));
            }
        }

        self.last_placement = Some(Placement {
            slot,
            prev,
            monster_idx,
        });
    }

    /// Places `card` at `slot` if the slot is currently in [`Self::legal`],
    /// then clears the selection so the next tap starts a fresh choice.
    fn try_place(&mut self, card_i: usize, slot: (i32, i32)) {
        if self.selected != Some(card_i) || !self.legal.contains(&slot) {
            return;
        }
        let card = self.hand[card_i];
        self.place(card, slot);
        self.selected = None;
        self.legal.clear();
    }

    /// Reverts [`Self::last_placement`], if any. The one and only undo step;
    /// see [`Placement`]'s docs for why it is not a stack.
    fn undo(&mut self) {
        let Some(p) = self.last_placement.take() else {
            self.log.push("Nothing to undo.", ui::DIM);
            return;
        };
        if let Some(i) = self.idx(p.slot.0, p.slot.1) {
            self.slots[i] = p.prev;
        }
        if let Some(mi) = p.monster_idx {
            // The spawned monster was always the last one pushed, so
            // dropping everything from that index on removes exactly it.
            self.monsters.truncate(mi);
        }
        self.log.push("Last placement undone.", ui::DIM);
    }

    fn activate(&mut self, action: Action) {
        match action {
            Action::SelectCard(i) => {
                self.selected = if self.selected == Some(i) {
                    None
                } else {
                    Some(i)
                };
            }
            Action::PlaceAt(gx, gy) => {
                if let Some(i) = self.selected {
                    self.try_place(i, (gx, gy));
                }
            }
            Action::Undo => self.undo(),
        }
    }

    fn move_cursor(&mut self, dx: i32, dy: i32) {
        self.cursor.0 = (self.cursor.0 + dx).clamp(0, self.grid_w - 1);
        self.cursor.1 = (self.cursor.1 + dy).clamp(0, self.grid_h - 1);
    }

    /// Keyboard parity for every touch interaction: digits select a card,
    /// arrows move the placement cursor, Enter commits at the cursor, U
    /// undoes. Nothing here ever touches the hero.
    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c @ '1'..='5') => {
                let i = (c as u8 - b'1') as usize;
                self.activate(Action::SelectCard(i));
            }
            KeyCode::Left => self.move_cursor(-1, 0),
            KeyCode::Right => self.move_cursor(1, 0),
            KeyCode::Up => self.move_cursor(0, -1),
            KeyCode::Down => self.move_cursor(0, 1),
            KeyCode::Enter => {
                if let Some(i) = self.selected {
                    self.try_place(i, self.cursor);
                }
            }
            KeyCode::Char('u' | 'U') => self.undo(),
            _ => {}
        }
    }

    /// Resolves this frame's tap, drag, and drop against the hotspots that
    /// were just registered for this frame's layout.
    ///
    /// A drag that started on a card is a drag-to-slot placement; a drag that
    /// started anywhere else on the map pans the camera. Both read from
    /// [`Pointer::press_origin`] captured *before* [`Pointer::take`], because
    /// `take` only hands back the current frame's edges and a drag's origin
    /// is not one of them -- it is a property of the whole gesture, which
    /// `Pointer` tracks separately for exactly this reason.
    fn handle_pointer(&mut self, map_area: Rect) {
        let origin = self.pointer.press_origin();
        let gesture = self.pointer.take();

        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.activate(action);
        }

        let started_on_card = origin.and_then(|o| self.hotspots.hit(o)).copied();
        if let Some(pos) = gesture.drop
            && let Some(Action::SelectCard(i)) = started_on_card
        {
            self.selected = Some(i);
            self.legal = self.legal_slots(self.hand[i]);
            if let Some(&Action::PlaceAt(gx, gy)) = self.hotspots.hit(pos) {
                self.try_place(i, (gx, gy));
            }
        }

        if gesture.drag.is_some() && !matches!(started_on_card, Some(Action::SelectCard(_))) {
            self.scroll.0 -= gesture.delta.0;
            self.scroll.1 -= gesture.delta.1;
        }

        self.clamp_scroll(map_area);
    }

    /// Keeps the camera from wandering far past the loop's own extent.
    ///
    /// `pad` (one tile) rather than zero, so the loop's outer edge does not
    /// slam into the panel border the instant it is reached -- a little
    /// breathing room past the true edge is what makes the clamp feel like a
    /// soft stop instead of a wall.
    fn clamp_scroll(&mut self, map_area: Rect) {
        let world_w = self.grid_w * TILE_W;
        let world_h = self.grid_h * TILE_H;
        let view_w = i32::from(map_area.width());
        let view_h = i32::from(map_area.height());
        let pad = TILE_W;
        self.scroll.0 = self
            .scroll
            .0
            .clamp(-pad, (world_w - view_w + pad).max(-pad));
        self.scroll.1 = self
            .scroll
            .1
            .clamp(-pad, (world_h - view_h + pad).max(-pad));
    }

    fn push_floater(&mut self, x: f32, y: f32, text: String, color: Color) {
        self.floaters.push(Floater {
            x,
            y,
            text,
            color,
            age: 0.0,
            ttl: 1.2,
        });
    }

    /// The hero's exact screen-space (well, world-cell-space) position,
    /// linearly interpolated between the two path tiles his continuous
    /// [`Self::hero_pos`] currently sits between. This is what makes the walk
    /// read as smooth motion even though tile-enter events (and therefore
    /// combat) only fire on integer crossings.
    fn hero_world_pos(&self) -> (f32, f32) {
        let len = self.path.len();
        if len == 0 {
            return (0.0, 0.0);
        }
        let pos = self.hero_pos.rem_euclid(len as f32);
        let i0 = pos.floor() as usize % len;
        let i1 = (i0 + 1) % len;
        let frac = pos.fract();
        let (cx0, cy0) = tile_center(self.path[i0]);
        let (cx1, cy1) = tile_center(self.path[i1]);
        let x = ((cx1 - cx0) as f32).mul_add(frac, cx0 as f32);
        let y = ((cy1 - cy0) as f32).mul_add(frac, cy0 as f32);
        (x, y)
    }

    /// Advances the hero, the loop counter, regeneration, and every floating
    /// number by `dt` world-seconds.
    ///
    /// The hero's own step is unconditional: no event, no input, and no
    /// combat outcome ever pauses it. That is the mechanical expression of
    /// "the hero is never directly controlled" -- if a fight could halt him,
    /// placing a card would be about managing a stop, and the game would
    /// collapse back into the tactical-combat genre this demo is deliberately
    /// not. Instead a fight just happens to land in his path and resolves
    /// instantly; see [`Self::enter_tile`].
    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        self.pulse = 0.5f32.mul_add((self.time * 3.0).sin(), 0.5);

        self.hero_hp = HERO_REGEN.mul_add(dt, self.hero_hp).min(self.hero_max_hp);

        for f in &mut self.floaters {
            f.age += dt;
            f.y = FLOAT_RISE.mul_add(-dt, f.y);
        }
        self.floaters.retain(|f| f.age < f.ttl);

        if self.path.is_empty() {
            return;
        }
        let len = self.path.len() as f32;
        let old_index = self.hero_pos.floor() as i64;
        let advanced = HERO_SPEED.mul_add(dt, self.hero_pos);
        if advanced >= len {
            self.loop_count += 1;
            self.log.push(
                format!(
                    "Loop {} begins. The road grows crueler.",
                    self.loop_count + 1
                ),
                ui::ACCENT,
            );
        }
        self.hero_pos = advanced.rem_euclid(len);
        let new_index = self.hero_pos.floor() as i64;
        if new_index != old_index {
            self.enter_tile(new_index as usize % self.path.len());
        }
    }

    /// Fires every fight tied to road tile `idx`. Called once per tile
    /// crossing, never per frame -- see [`Self::simulate`].
    fn enter_tile(&mut self, idx: usize) {
        for i in 0..self.monsters.len() {
            if self.monsters[i].alive && self.monsters[i].road_index == idx {
                self.resolve_combat(i);
            }
        }
    }

    /// One short, instant exchange: both sides hit once, damage numbers rise
    /// from both tiles, and the log records the result. A weak monster the
    /// hero survives stays alive for the next loop and gets fought again on
    /// the next crossing, so a card planted early keeps mattering for as long
    /// as its monster does.
    fn resolve_combat(&mut self, mi: usize) {
        let monster_name = self.monsters[mi].kind.name();
        let (mx, my) = tile_center(self.path[self.monsters[mi].road_index]);

        let dmg_out = (self.hero_atk - self.monsters[mi].def).max(1.0);
        self.monsters[mi].hp -= dmg_out;
        self.push_floater(
            mx as f32,
            my as f32 - 1.0,
            format!("-{dmg_out:.0}"),
            rgb(255, 210, 90),
        );

        let dmg_in = (self.monsters[mi].atk - self.hero_def).max(0.5);
        self.hero_hp -= dmg_in;
        let (hx, hy) = self.hero_world_pos();
        self.push_floater(hx, hy - 1.0, format!("-{dmg_in:.0}"), rgb(230, 90, 90));

        self.log.push(
            format!("Clash with {monster_name}: dealt {dmg_out:.0}, took {dmg_in:.0}."),
            ui::FG,
        );

        if self.monsters[mi].hp <= 0.0 {
            self.monsters[mi].alive = false;
            self.log.push(
                format!("{monster_name} falls. Loot recovered."),
                rgb(120, 196, 158),
            );
            self.award_loot();
        }
        if self.hero_hp <= 0.0 {
            self.hero_hp = self.hero_max_hp * 0.5;
            self.log.push(
                "The hero staggers, rallies, and presses on.",
                rgb(226, 90, 90),
            );
        }
    }

    fn award_loot(&mut self) {
        let kind = EquipKind::ALL[self.loot_count % EquipKind::ALL.len()];
        let slot = self.loot_count % self.equipment.len();
        self.equipment[slot] = Some(kind);
        self.loot_count += 1;

        let (atk, def, hp) = kind.bonus();
        self.hero_atk += atk;
        self.hero_def += def;
        self.hero_max_hp += hp;
        self.hero_hp = (self.hero_hp + hp).min(self.hero_max_hp);
        self.log
            .push(format!("Equipped {}.", kind.name()), ui::ACCENT);
    }

    /// Splits `content` into the map, the hand, and (space permitting) a
    /// chronicle sidebar and a stats panel.
    ///
    /// The hand always gets a band, at whatever card tier still fits, because
    /// it is the only control surface this demo has; the sidebar and stats
    /// panel are the first things dropped on a short or narrow viewport,
    /// because their information (loop count, hp, recent log lines) is
    /// already restated, more tersely, in the status bar's `left` text --
    /// see [`LoopTrack::status`]. Nothing here is hover-only, so dropping a
    /// panel never removes information the player has no other way to reach.
    fn layout(content: Rect, shape: Shape) -> (Rect, Rect, Option<Rect>, Option<Rect>) {
        let hand_h = match content.height() {
            h if h >= 40 => card::FULL_H,
            h if h >= 18 => card::COMPACT_H,
            _ => 3,
        };
        let (rest, hand_area) = panel::split_bottom(content, hand_h);

        match shape {
            Shape::Desktop => {
                let (main, side) = panel::split_right(rest, 34);
                let (stats, side) = panel::split_top(side, 9);
                (main, hand_area, Some(side), Some(stats))
            }
            Shape::Landscape if rest.width() >= 110 => {
                let (main, side) = panel::split_right(rest, 28);
                let (stats, side) = panel::split_top(side, 7);
                (main, hand_area, Some(side), Some(stats))
            }
            Shape::Landscape => (rest, hand_area, None, None),
            Shape::Portrait => {
                let stats_h = if rest.height() >= 14 { 6 } else { 0 };
                let (stats, map) = panel::split_top(rest, stats_h);
                (map, hand_area, None, (stats_h > 0).then_some(stats))
            }
        }
    }

    /// The clipped screen rect for slot `(gx, gy)`, or `None` if it is fully
    /// scrolled out of `inner`. Used both for registering hotspots and (were
    /// it needed) for anything else that must agree exactly with what
    /// [`Self::draw_field`] painted there.
    fn slot_screen_rect(&self, gx: i32, gy: i32, inner: Rect) -> Option<Rect> {
        let wx = gx * TILE_W - self.scroll.0;
        let wy = gy * TILE_H - self.scroll.1;
        let x0 = wx.max(0);
        let y0 = wy.max(0);
        let x1 = (wx + TILE_W).min(i32::from(inner.width()));
        let y1 = (wy + TILE_H).min(i32::from(inner.height()));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect::new(
            inner.left() + x0 as u16,
            inner.top() + y0 as u16,
            (x1 - x0) as u16,
            (y1 - y0) as u16,
        ))
    }

    /// `0..5` are the terrain cards; `5` is the Undo action card appended to
    /// the same fan, so Undo gets the same multi-cell, always-tappable
    /// treatment as everything else rather than a bespoke small button.
    const fn hand_len(&self) -> usize {
        self.hand.len() + 1
    }

    /// Draw order for the hand: the selected card (if any) goes last, so it
    /// paints on top of, and is hit-tested above, any neighbour it happens to
    /// overlap with once the fan starts overlapping cards. See
    /// [`card::fan`]'s own docs for why draw order and hit-test order must
    /// agree.
    fn hand_draw_order(&self) -> Vec<usize> {
        let n = self.hand_len();
        self.selected.map_or_else(
            || (0..n).collect(),
            |sel| {
                (0..n)
                    .filter(|&i| i != sel)
                    .chain(core::iter::once(sel))
                    .collect()
            },
        )
    }

    fn register_hotspots(&mut self, map_area: Rect, hand_area: Rect) {
        let rects = card::fan(hand_area, self.hand_len(), card::FULL_W);
        for (i, rect) in rects.iter().enumerate() {
            let action = if i < self.hand.len() {
                Action::SelectCard(i)
            } else {
                Action::Undo
            };
            self.hotspots.push_tappable(*rect, hand_area, action);
        }
        for &(gx, gy) in &self.legal {
            if let Some(rect) = self.slot_screen_rect(gx, gy, map_area) {
                self.hotspots.push(rect, Action::PlaceAt(gx, gy));
            }
        }
    }

    fn cell_style(&self, wx: i32, wy: i32) -> (char, Color, Color) {
        let gx = wx.div_euclid(TILE_W);
        let gy = wy.div_euclid(TILE_H);
        let local = (wx.rem_euclid(TILE_W), wy.rem_euclid(TILE_H));
        let Some(slot) = self.slot_at(gx, gy) else {
            return void_cell(wx, wy);
        };
        let flags = CellFlags {
            highlighted: self.legal.contains(&(gx, gy)),
            cursor: self.cursor == (gx, gy),
        };
        match slot {
            SlotKind::Road { rock } => self.road_cell(gx, gy, local, rock, flags),
            SlotKind::Ground(occupant) => self.ground_cell(gx, gy, local, occupant, flags),
        }
    }

    /// Draws one cell of a road tile: a stone floor, an autotiled connector
    /// spine through the centre (see the module docs), a rock overlay if one
    /// was placed, and the shared highlight/cursor tint.
    fn road_cell(
        &self,
        gx: i32,
        gy: i32,
        local: (i32, i32),
        rock: bool,
        flags: CellFlags,
    ) -> (char, Color, Color) {
        let (lx, ly) = local;
        let mask = mask4([
            self.is_road(gx, gy - 1),
            self.is_road(gx + 1, gy),
            self.is_road(gx, gy + 1),
            self.is_road(gx - 1, gy),
        ]);
        let bg = tile_bg(rgb(26, 23, 20), self.pulse, flags);

        if rock && lx == 1 && ly == 1 {
            return ('\u{25A0}', rgb(160, 160, 160), bg);
        }
        let (cx, cy) = (TILE_W / 2, TILE_H / 2);
        if lx == cx && ly == cy {
            return (BOX_SINGLE[(mask & 0x0F) as usize], rgb(214, 202, 180), bg);
        }
        if ly == cy && lx > cx && mask & E != 0 {
            return ('\u{2500}', rgb(184, 172, 150), bg);
        }
        if ly == cy && lx < cx && mask & W != 0 {
            return ('\u{2500}', rgb(184, 172, 150), bg);
        }
        if lx == cx && ly < cy && mask & N != 0 {
            return ('\u{2502}', rgb(184, 172, 150), bg);
        }
        if lx == cx && ly > cy && mask & S != 0 {
            return ('\u{2502}', rgb(184, 172, 150), bg);
        }
        if hash01(0xC0BB, gx * 97 + lx, gy * 97 + ly) < 0.15 {
            return ('.', rgb(90, 86, 78), bg);
        }
        (' ', rgb(0, 0, 0), bg)
    }

    /// Draws one cell of a ground tile: grass texture when empty, a bordered
    /// terrain box when occupied, and a legal-slot border ring when this tile
    /// is a placement target for the currently selected card.
    fn ground_cell(
        &self,
        gx: i32,
        gy: i32,
        local: (i32, i32),
        occupant: Option<TerrainKind>,
        flags: CellFlags,
    ) -> (char, Color, Color) {
        let (lx, ly) = local;
        let bg = tile_bg(rgb(13, 19, 13), self.pulse, flags);
        if let Some(terrain) = occupant {
            return terrain_cell(local, terrain, bg);
        }
        if flags.highlighted && (lx == 0 || lx == TILE_W - 1 || ly == 0 || ly == TILE_H - 1) {
            return ('\u{2591}', ui::ACCENT, bg);
        }
        if hash01(0x6A11, gx * 97 + lx, gy * 97 + ly) < 0.12 {
            return ('.', rgb(58, 88, 54), bg);
        }
        (' ', rgb(0, 0, 0), bg)
    }

    fn draw_field(&self, surface: &mut Surface<'_>, inner: Rect) {
        for sy in 0..inner.height() {
            for sx in 0..inner.width() {
                let wx = i32::from(sx) + self.scroll.0;
                let wy = i32::from(sy) + self.scroll.1;
                let (glyph, fg, bg) = self.cell_style(wx, wy);
                surface.put(
                    (inner.left() + sx, inner.top() + sy),
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
    }

    fn draw_actors(&self, surface: &mut Surface<'_>, inner: Rect) {
        for m in &self.monsters {
            if !m.alive {
                continue;
            }
            let (wx, wy) = tile_center(self.path[m.road_index]);
            // Fading the glyph toward the background as it loses health
            // gives a wounded monster a visible "hurt" state without needing
            // a dedicated hp bar wedged into a 9x5 tile.
            let hurt = 1.0 - (m.hp / m.max_hp).clamp(0.0, 1.0);
            let color = mix(m.kind.color(), rgb(60, 40, 40), hurt * 0.6);
            self.put_world(
                surface,
                inner,
                wx,
                wy,
                m.kind.glyph(),
                Style::new().fg(color).bg(rgb(22, 12, 12)),
            );
        }

        // The hero is drawn last so he is never hidden behind a monster on
        // his own tile -- the one actor that must always be visible, since he
        // is the only thing on the board the player cannot place or remove.
        let (hx, hy) = self.hero_world_pos();
        self.put_world(
            surface,
            inner,
            hx.round() as i32,
            hy.round() as i32,
            '\u{263B}',
            Style::new().fg(rgb(250, 230, 120)).bg(rgb(46, 34, 12)),
        );

        for f in &self.floaters {
            let alpha = (1.0 - f.age / f.ttl).clamp(0.0, 1.0);
            let color = mix(f.color, rgb(6, 6, 10), 1.0 - alpha);
            self.put_world_text(
                surface,
                inner,
                f.x.round() as i32,
                f.y.round() as i32,
                &f.text,
                color,
            );
        }
    }

    fn put_world(
        &self,
        surface: &mut Surface<'_>,
        inner: Rect,
        wx: i32,
        wy: i32,
        glyph: char,
        style: Style,
    ) {
        let sx = wx - self.scroll.0;
        let sy = wy - self.scroll.1;
        if sx < 0 || sy < 0 || sx >= i32::from(inner.width()) || sy >= i32::from(inner.height()) {
            return;
        }
        surface.put(
            (inner.left() + sx as u16, inner.top() + sy as u16),
            glyph,
            style,
        );
    }

    fn put_world_text(
        &self,
        surface: &mut Surface<'_>,
        inner: Rect,
        wx: i32,
        wy: i32,
        text: &str,
        color: Color,
    ) {
        let sx = wx - self.scroll.0;
        let sy = wy - self.scroll.1;
        if sx < 0 || sy < 0 || sx >= i32::from(inner.width()) || sy >= i32::from(inner.height()) {
            return;
        }
        surface.print(
            (inner.left() + sx as u16, inner.top() + sy as u16),
            text,
            Style::new().fg(color).bg(rgb(9, 10, 15)),
        );
    }

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("The Loop")
            .border(panel::Border::Double)
            .badge(&format!("loop {}", self.loop_count + 1))
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let mut clipped = surface.clip(inner);
        self.draw_field(&mut clipped, inner);
        self.draw_actors(&mut clipped, inner);
    }

    fn draw_hand(&self, surface: &mut Surface<'_>, area: Rect) {
        let rects = card::fan(area, self.hand_len(), card::FULL_W);
        for i in self.hand_draw_order() {
            let Some(rect) = rects.get(i) else { continue };
            if i < self.hand.len() {
                let meta = self.hand[i].meta();
                let state = if self.selected == Some(i) {
                    CardState::Selected
                } else {
                    CardState::Idle
                };
                Card::new(meta.name)
                    .cost(meta.tag)
                    .kind("Terrain")
                    .body(meta.rule)
                    .accent(meta.accent)
                    .state(state)
                    .draw(surface, *rect);
            } else {
                let state = if self.last_placement.is_none() {
                    CardState::Disabled
                } else {
                    CardState::Idle
                };
                Card::new("Undo")
                    .cost("U")
                    .kind("Action")
                    .body("Revert the last placement.")
                    .accent(rgb(198, 110, 96))
                    .state(state)
                    .draw(surface, *rect);
            }
        }
    }

    fn draw_stats(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Hero").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let frac = (self.hero_hp / self.hero_max_hp).clamp(0.0, 1.0);
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::dim("HP")],
            panel::PANEL_BG,
        );
        if inner.width() > 3 {
            panel::bar(
                surface,
                (inner.left() + 3, inner.top()),
                inner.width() - 3,
                frac,
                panel::threshold(frac),
                rgb(30, 30, 36),
            );
        }
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::plain(&format!(
                    "ATK {:.0}  DEF {:.0}  Loop {}",
                    self.hero_atk,
                    self.hero_def,
                    self.loop_count + 1
                ))],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 3 {
            self.draw_equipment(
                surface,
                Rect::new(
                    inner.left(),
                    inner.top() + 2,
                    inner.width(),
                    inner.height() - 2,
                ),
            );
        }
    }

    fn draw_equipment(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() < 2 {
            return;
        }
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width(),
            &[Span::dim("Gear:")],
            panel::PANEL_BG,
        );
        let mut x = area.left();
        let mut y = area.top() + 1;
        for slot in &self.equipment {
            if x + 4 > area.right() {
                x = area.left();
                y += 1;
            }
            if y >= area.bottom() {
                break;
            }
            let (ch, color) = slot.map_or(('-', ui::DIM), |k| (k.glyph(), rgb(230, 196, 110)));
            surface.print(
                (x, y),
                &format!("[{ch}]"),
                Style::new().fg(color).bg(panel::PANEL_BG),
            );
            x += 4;
        }
    }

    fn draw_sidebar(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Chronicle")
            .badge(&format!(
                "{} kills",
                self.monsters.iter().filter(|m| !m.alive).count()
            ))
            .draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    fn status(&self) -> String {
        format!(
            "HP {:.0}/{:.0}  ATK {:.0}  DEF {:.0}  Loop {}",
            self.hero_hp.max(0.0),
            self.hero_max_hp,
            self.hero_atk,
            self.hero_def,
            self.loop_count + 1
        )
    }
}

/// The shared highlight/cursor background tint every tile uses. Highlight
/// pulses gently via `pulse` (see [`LoopTrack::pulse`]) so a legal-slot
/// callout keeps drawing the eye even on an otherwise static frame; cursor
/// tint is a flat brighten so the keyboard cursor stays legible regardless of
/// the pulse phase.
fn tile_bg(base: Color, pulse: f32, flags: CellFlags) -> Color {
    let mut bg = base;
    if flags.highlighted {
        bg = mix(bg, ui::ACCENT, pulse.mul_add(0.15, 0.25));
    }
    if flags.cursor {
        bg = mix(bg, rgb(255, 255, 255), 0.4);
    }
    bg
}

/// Draws one cell of a terrain box: a single-line border, the terrain's own
/// glyph on the tile's centre row, and its label letter one row below --
/// which is what still reads clearly even at a glance, without needing a
/// hover to explain which terrain a small glyph was meant to be.
fn terrain_cell(local: (i32, i32), terrain: TerrainKind, bg: Color) -> (char, Color, Color) {
    let (lx, ly) = local;
    let (l, t, r, b) = (0, 0, TILE_W - 1, TILE_H - 1);
    let (glyph, color, label) = terrain.visual();
    let frame = scale(color, 0.75);
    if lx == l && ly == t {
        return ('\u{250C}', frame, bg);
    }
    if lx == r && ly == t {
        return ('\u{2510}', frame, bg);
    }
    if lx == l && ly == b {
        return ('\u{2514}', frame, bg);
    }
    if lx == r && ly == b {
        return ('\u{2518}', frame, bg);
    }
    if ly == t || ly == b {
        return ('\u{2500}', frame, bg);
    }
    if lx == l || lx == r {
        return ('\u{2502}', frame, bg);
    }
    let cx = TILE_W / 2;
    if lx == cx && ly == TILE_H / 2 {
        return (glyph, color, bg);
    }
    if lx == cx && ly == TILE_H / 2 + 1 {
        return (label, scale(color, 0.85), bg);
    }
    (' ', color, bg)
}

/// Background beyond the generated grid: near-black, with a sparse dim dot
/// texture keyed by [`hash01`] on the absolute world cell so it stays fixed
/// from frame to frame rather than reshuffling as the camera pans.
fn void_cell(wx: i32, wy: i32) -> (char, Color, Color) {
    let bg = rgb(6, 6, 10);
    if hash01(0x5741, wx, wy) < 0.03 {
        return ('.', rgb(46, 46, 58), bg);
    }
    (' ', rgb(0, 0, 0), bg)
}

impl Demo for LoopTrack {
    const NAME: &'static str = "32_loop_track";
    const TITLE: &'static str = "32 Loop Track";
    const BLURB: &'static str =
        "An uncontrollable hero walks an endless loop that you build around him.";
    const GRID: (u16, u16) = (150, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-5", "select card"),
            ("arrows", "move placement cursor"),
            ("Enter", "place at cursor"),
            ("U", "undo last placement"),
            ("tap/drag", "select, place, or pan"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        for event in term.drain_events() {
            self.pointer.feed(&event);
            match &event {
                Event::Close => return false,
                Event::Key(key) if key.is_down() => {
                    if ui::is_quit(&event) {
                        return false;
                    }
                    self.handle_key(key.code);
                }
                _ => {}
            }
        }

        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let shape = Shape::of(content);
        let (map_area, hand_area, side_area, stats_area) = Self::layout(content, shape);

        // Legal slots and hotspots are computed once, then recomputed after
        // input is handled, so a tap that changes the selection sees its own
        // effect highlighted the same frame rather than one frame late.
        self.legal = self
            .selected
            .map(|i| self.legal_slots(self.hand[i]))
            .unwrap_or_default();
        self.hotspots.clear();
        self.register_hotspots(map_area, hand_area);
        self.handle_pointer(map_area);
        self.legal = self
            .selected
            .map(|i| self.legal_slots(self.hand[i]))
            .unwrap_or_default();
        self.hotspots.clear();
        self.register_hotspots(map_area, hand_area);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.draw_map(&mut surface, map_area);
        if let Some(side) = side_area {
            self.draw_sidebar(&mut surface, side);
        }
        if let Some(stats) = stats_area {
            self.draw_stats(&mut surface, stats);
        }
        self.draw_hand(&mut surface, hand_area);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(LoopTrack);
