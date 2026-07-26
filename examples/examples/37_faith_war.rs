//! 37: Faith War -- armies as formations, not tokens, on a province map.
//!
//! Adapted from Dominions 5: Warriors of the Faith. Every other strategy map
//! in this gallery marks an army with a single glyph or a number badge; this
//! demo is built around the one thing Dominions draws differently. A stack on
//! its overland map is a little block of individual soldier sprites, so the
//! player reads an army's strength by the *area* the block covers, never by a
//! digit next to it. Forty spearmen are a 10x4 block; a scouting party is a
//! 3x2 smudge. That is the entire premise, and everything else here (the
//! province shapes, the commander roster, the move-order arrow) exists to put
//! that block on a map worth marching it across.
//!
//! Techniques on show:
//!
//! - **Armies drawn as formations** ([`Army::draw`]): a block whose width and
//!   height literally multiply out to the unit count, banded into a front
//!   rank of infantry, a back rank of archers, and (for a led army) one
//!   distinguished commander glyph under a banner. Strength is read by
//!   silhouette, matching the brief this demo exists to satisfy.
//! - **BSP provinces with boundary jitter** ([`generate_provinces`]): the
//!   world is split into leaf rectangles exactly as `21_deck_plan` splits
//!   rooms, but the leaves are provinces rather than chambers, and a second
//!   pass nibbles a few cells off each edge so the map reads as hand-drawn
//!   borders instead of a spreadsheet of rectangles -- while every leaf
//!   still keeps a guaranteed interior big enough for a block and a label.
//! - **Outward dominion pulse** ([`Province::dominion_glow`]): each owned
//!   province's tint brightens on a wave keyed to Euclidean distance from its
//!   capital, so a nation's color visibly radiates from its throne instead of
//!   sitting as a flat wash.
//! - **Tap-select-then-tap-target ordering** ([`FaithWar::handle_map_tap`]):
//!   select a commander card, tap an adjacent province, get a thick order
//!   arrow and a projected arrival; press End Turn and the army's block
//!   marches between the two centroids over the following few frames.
//! - **Two responsive chrome shapes** ([`ui::touch::Shape`]): commander cards
//!   run down the left edge with a command rail on the right on a wide
//!   viewport; on a portrait phone the cards become a horizontal scroller
//!   under the map and the rail becomes a bottom action row, while the map
//!   itself keeps its army blocks at full size and simply shows fewer
//!   provinces.
//!
//! ```sh
//! cargo run --example 37_faith_war --features crossterm
//! cargo run --example 37_faith_war --features software
//! cargo run --example 37_faith_war --features gl
//! cargo run --example 37_faith_war  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Log, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb, scale};

/// World width in cells. Wide enough that a phone viewport cannot show it all
/// at once, which is what exercises the "pan instead of shrink" requirement:
/// the army blocks stay full size and the map scrolls around them.
const WORLD_W: i32 = 116;
/// See [`WORLD_W`].
const WORLD_H: i32 = 52;

/// Smallest a BSP leaf may be before the split stops, in cells.
///
/// The brief asks for provinces at least ~14x6 so a block and a label both
/// fit. The boundary-jitter pass in [`generate_provinces`] nibbles up to two
/// cells off every edge, so the leaf itself has to be four cells larger on
/// each axis than that floor for the guarantee to survive jitter intact.
const MIN_LEAF_W: i32 = 18;
/// See [`MIN_LEAF_W`].
const MIN_LEAF_H: i32 = 10;

/// How many cells in from a leaf's edge the boundary-jitter pass may nibble.
const JITTER_BAND: i32 = 2;

/// Seconds a march animation takes to cross from one province's centroid to
/// another's, once End Turn commits a pending order.
const MARCH_SECONDS: f32 = 1.4;

/// A province's ground cover, which drives its base terrain glyph/color and
/// nothing else -- ownership and dominion tint are layered on top of this,
/// not folded into it, so the same terrain reads consistently under any
/// owner.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Plain,
    Forest,
    Hill,
    Marsh,
}

impl Terrain {
    const fn glyph(self) -> char {
        match self {
            Self::Plain => '.',
            Self::Forest => '\u{2663}', // club: reads as a small tree/shrub
            Self::Hill => '^',
            Self::Marsh => '~',
        }
    }

    const fn density(self) -> f32 {
        match self {
            Self::Plain => 0.18,
            Self::Forest => 0.75,
            Self::Hill => 0.45,
            Self::Marsh => 0.55,
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Plain => rgb(120, 140, 84),
            Self::Forest => rgb(58, 96, 58),
            Self::Hill => rgb(140, 122, 84),
            Self::Marsh => rgb(84, 108, 96),
        }
    }

    const fn from_index(i: u32) -> Self {
        match i % 4 {
            0 => Self::Plain,
            1 => Self::Forest,
            2 => Self::Hill,
            _ => Self::Marsh,
        }
    }
}

/// Who holds a province. Only two dominions are in play (the player's and one
/// rival), plus provinces nobody has converted yet -- Dominions calls that
/// last state a "no man's land", not a third faction, and it draws with no
/// tint at all rather than a third color competing for attention.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Player,
    Rival,
    Independent,
}

impl Owner {
    const fn dominion_color(self) -> Option<Color> {
        match self {
            Self::Player => Some(rgb(72, 196, 190)),
            Self::Rival => Some(rgb(196, 74, 74)),
            Self::Independent => None,
        }
    }
}

/// One province: its shape (via [`FaithWar::grid`]), its identity, and the
/// per-province numbers the top-left readout shows.
struct Province {
    name: String,
    terrain: Terrain,
    owner: Owner,
    capital: bool,
    /// Inclusive cell bounds, `(min_x, min_y, max_x, max_y)`.
    bbox: (i32, i32, i32, i32),
    centroid: (i32, i32),
    neighbors: Vec<usize>,
    population: u32,
    income: u32,
    supply: u32,
    unrest: u8,
    defence: u32,
}

impl Province {
    /// The dominion tint at `(x, y)`, or `None` over unowned ground.
    ///
    /// Brightens on a ring expanding from the capital rather than sitting
    /// flat, which is what makes a nation's color read as radiating *from*
    /// its throne instead of merely covering the map: the ring speed is slow
    /// enough that a still screenshot still shows a gradient, and the
    /// wavelength is wide enough that the whole province shows at most one
    /// bright band at a time.
    fn dominion_glow(&self, x: i32, y: i32, time: f32) -> Option<Color> {
        let base = self.owner.dominion_color()?;
        let (cx, cy) = self.centroid;
        let dx = f32::from((x - cx) as i16);
        let dy = f32::from((y - cy) as i16) * 2.0; // cells are ~2x taller than wide
        let dist = dx.hypot(dy);
        let wave = (dist.mul_add(-0.35, time * 1.6)).sin().mul_add(0.5, 0.5);
        Some(mix(base, rgb(255, 255, 255), wave * 0.18))
    }
}

/// The visible unit-count tiers an army can be drawn at.
///
/// Fixed tiers rather than a formula over an arbitrary head count: the brief
/// gives two exact examples (40 in a 10x4 block, a 3x2 scouting smudge) and
/// picking tiers that land on those numbers exactly is worth more than a
/// general formula that would only approximate them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArmyTier {
    Scout,
    Warband,
    Legion,
    Host,
}

impl ArmyTier {
    /// Block dimensions in cells, `(width, height)`.
    const fn dims(self) -> (u16, u16) {
        match self {
            Self::Scout => (3, 2),
            Self::Warband => (6, 3),
            Self::Legion => (8, 4),
            Self::Host => (10, 4),
        }
    }

    /// How many of the block's rows (counted from the top) are archers
    /// rather than infantry. The rest of the block is infantry.
    const fn archer_rows(self) -> u16 {
        match self {
            Self::Scout => 0,
            Self::Warband | Self::Legion | Self::Host => 1,
        }
    }

    fn head_count(self) -> u32 {
        let (w, h) = self.dims();
        u32::from(w) * u32::from(h)
    }

    const fn grow(self) -> Self {
        match self {
            Self::Scout => Self::Warband,
            Self::Warband => Self::Legion,
            Self::Legion | Self::Host => Self::Host,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Scout => "scouts",
            Self::Warband => "warband",
            Self::Legion => "legion",
            Self::Host => "host",
        }
    }
}

/// A commander's current standing order. Only [`Order::Move`] and
/// [`Order::Patrol`] commanders keep a field army; [`Order::Research`]
/// commanders sit in the capital's library instead, exactly as Dominions
/// splits its roster between the field and the lab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Patrol,
    Research,
    Move,
    Defend,
}

impl Order {
    const fn label(self) -> &'static str {
        match self {
            Self::Patrol => "Patrol",
            Self::Research => "Research",
            Self::Move => "Move",
            Self::Defend => "Defend",
        }
    }
}

/// A commander: the roster entry on the left edge, plus the army it leads,
/// if it leads one at all.
struct Commander {
    name: &'static str,
    sigil: char,
    order: Order,
    leadership: u8,
    /// Which province the commander (and its army, if any) currently sits in.
    province: usize,
    army: Option<ArmyTier>,
}

/// An in-flight move order that End Turn has committed: the army's block
/// slides from one province's centroid to another's over [`MARCH_SECONDS`],
/// after which the commander's `province` field updates for good.
///
/// The interpolation lives on the *sprite block*, never on a text field --
/// per the addendum, smoothing a label is what makes a UI look broken, but a
/// moving formation of glyphs is art, and art is exactly what idle animation
/// in this gallery is for.
struct March {
    commander: usize,
    from: usize,
    to: usize,
    t: f32,
}

/// Which right-rail / bottom-row command a tap landed on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Commander(usize),
    EndTurn,
    ArmySetup,
    Recruit,
    Research,
    Filters,
}

/// The two idle-animation clocks every army block reads from: which of the
/// banner's two states is showing, and whether the archer rank has stepped
/// down by its one "breathing" cell this beat. Grouped into their own type
/// rather than two more fields on [`FaithWar`], which is what keeps that
/// struct under the excessive-bools lint's threshold: these two are read
/// together everywhere they're read at all.
#[derive(Default)]
struct Anim {
    banner_phase: bool,
    breathe: bool,
}

/// State: the generated province map, the roster, pending and in-flight
/// orders, and the chrome's interaction plumbing.
pub struct FaithWar {
    grid: Vec<u8>,
    provinces: Vec<Province>,
    commanders: Vec<Commander>,
    selected: Option<usize>,
    /// A chosen but not yet committed move order: `(commander, target
    /// province)`. Drawn as the order arrow; cleared by End Turn (which
    /// starts [`Self::march`] instead) or by re-tapping the source province.
    pending: Option<(usize, usize)>,
    march: Option<March>,
    /// The province the top-left readout currently describes: whatever was
    /// last tapped on the map, defaulting to the capital.
    viewed: usize,
    turn: u32,
    treasury: i64,
    income: i64,
    /// Toggled by the Filters rail button: hides the terrain glyphs and
    /// dominion glow so only ownership borders and armies remain, the "just
    /// tell me who owns what and where the armies are" view.
    filters_bare: bool,
    setup_overlay: bool,
    camera: TileCamera,
    map_area: Rect,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    time: f32,
    anim: Anim,
    log: Log,
    fps: FpsMeter,
}

impl Default for FaithWar {
    fn default() -> Self {
        let seed = 37;
        let (grid, provinces) = generate_provinces(seed);
        let commanders = build_roster(&provinces);
        let capital = provinces.iter().position(|p| p.capital).unwrap_or(0);
        let mut camera = TileCamera::new(40, 20, WORLD_W, WORLD_H);
        let (cx, cy) = provinces[capital].centroid;
        camera.center_on(Cell::new(cx, cy));

        let mut log = Log::new(48);
        log.push("The dominion awakens.", ui::ACCENT);
        log.push(
            "Select a commander, then tap an adjacent province to order a march.",
            ui::DIM,
        );

        Self {
            grid,
            provinces,
            commanders,
            selected: None,
            pending: None,
            march: None,
            viewed: capital,
            turn: 1,
            treasury: 3641,
            income: 704,
            filters_bare: false,
            setup_overlay: false,
            camera,
            map_area: Rect::new(0, 0, 0, 0),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            time: 0.0,
            anim: Anim::default(),
            log,
            fps: FpsMeter::new(),
        }
    }
}

/// Recursively splits `area` into leaves at least [`MIN_LEAF_W`]x[`MIN_LEAF_H`],
/// alternating split axis by whichever dimension is more elongated -- the
/// same shape-preserving rule `21_deck_plan` uses for rooms, wanted here for
/// the same reason: a province should read as a region, not a sliver.
fn split(area: (i32, i32, i32, i32), rng: &mut Rng, leaves: &mut Vec<(i32, i32, i32, i32)>) {
    let (x0, y0, x1, y1) = area;
    let (w, h) = (x1 - x0, y1 - y0);
    let can_split_w = w > MIN_LEAF_W * 2;
    let can_split_h = h > MIN_LEAF_H * 2;
    if !can_split_w && !can_split_h {
        leaves.push(area);
        return;
    }

    let split_horizontally = if can_split_w && can_split_h {
        w > h
    } else {
        can_split_w
    };

    if split_horizontally {
        let span = w - MIN_LEAF_W * 2;
        let at = MIN_LEAF_W + rng.next_below(span.max(1) as u32) as i32;
        split((x0, y0, x0 + at, y1), rng, leaves);
        split((x0 + at, y0, x1, y1), rng, leaves);
    } else {
        let span = h - MIN_LEAF_H * 2;
        let at = MIN_LEAF_H + rng.next_below(span.max(1) as u32) as i32;
        split((x0, y0, x1, y0 + at), rng, leaves);
        split((x0, y0 + at, x1, y1), rng, leaves);
    }
}

/// Province names are composed from a stem and a suffix rather than drawn
/// from one flat pool. A flat pool of 16 names against a BSP split that can
/// produce up to ~30 leaves at this world size was guaranteed to repeat a
/// name across two provinces sooner or later (it did, visibly, on the very
/// first seed this demo shipped with) -- `Greywatch`, `Fenmoor`, and
/// `Highcairn` all appeared twice on screen at once. [`STEMS`] x [`SUFFIXES`]
/// gives 64 combinations, more than double the worst-case leaf count, and
/// [`province_name`] assigns each province index a distinct `(stem, suffix)`
/// pair by construction (one division and one remainder against a fixed
/// modulus), so two provinces can never collide without also being the same
/// index.
const STEMS: &[&str] = &[
    "Grey", "Fen", "High", "Dusk", "Ash", "Thorn", "Wolf", "Sable",
];
/// See [`STEMS`].
const SUFFIXES: &[&str] = &[
    "watch", "moor", "cairn", "reach", "dell", "field", "mere", "hollow",
];

/// Builds province index `i`'s name from a unique `(stem, suffix)` pair.
///
/// `i` ranges over `0..leaf_count`, and `leaf_count` never exceeds
/// `STEMS.len() * SUFFIXES.len()` at this world size (the BSP split's
/// worst case is ~30 leaves against 64 combinations here), so `i / M` and
/// `i % M` stay within bounds and never repeat a pair for a different `i`.
/// The stem index is additionally offset by a per-world hash so the pairing
/// doesn't read as a boring lexicographic sweep across the map, while
/// staying a pure function of `(seed, i)` -- no rejection sampling, no
/// mutable name registry, nothing that could depend on iteration order.
fn province_name(seed: u32, i: usize) -> String {
    let m = SUFFIXES.len();
    let shift = (hash01(seed ^ 0x4E41_4D45, 0, 0) * STEMS.len() as f32) as usize;
    let stem = STEMS[(i / m + shift) % STEMS.len()];
    let suffix = SUFFIXES[i % m];
    format!("{stem}{suffix}")
}

/// Builds the province map: a BSP split into leaves, a boundary-jitter pass
/// so the leaves read as hand-drawn regions rather than a grid of rectangles,
/// then per-province stats and adjacency derived from the finished grid.
fn generate_provinces(seed: u32) -> (Vec<u8>, Vec<Province>) {
    let mut rng = Rng::new(seed);
    let mut leaves = Vec::new();
    split((0, 0, WORLD_W, WORLD_H), &mut rng, &mut leaves);

    let grid = paint_leaves(&leaves);
    let grid = jitter_boundaries(&grid, seed);

    let count = leaves.len();
    let bbox = province_bboxes(&grid, count);
    let pairs = province_adjacency(&grid);

    // The capital is whichever leaf's centroid sits closest to the world
    // center: as good a "heartland" heuristic as any for a randomly split
    // map, and it keeps the player's holdings contiguous around it below.
    let capital = (0..count)
        .min_by_key(|&i| {
            let (x0, y0, x1, y1) = bbox[i];
            let cx = i32::midpoint(x0, x1);
            let cy = i32::midpoint(y0, y1);
            (cx - WORLD_W / 2).abs() + (cy - WORLD_H / 2).abs()
        })
        .unwrap_or(0);

    let mut provinces: Vec<Province> = (0..count)
        .map(|i| build_province(i, capital, bbox[i], &pairs, seed))
        .collect();

    // Border provinces adjacent to the rival get a heavier garrison feel via
    // defence; capital always reads as best-defended.
    if let Some(cap) = provinces.get_mut(capital) {
        cap.defence += 30;
    }

    (grid, provinces)
}

/// Stamps one province id per BSP leaf across the whole grid.
fn paint_leaves(leaves: &[(i32, i32, i32, i32)]) -> Vec<u8> {
    let idx = |x: i32, y: i32| (y * WORLD_W + x) as usize;
    let mut grid = vec![0u8; (WORLD_W * WORLD_H) as usize];
    for (i, &(x0, y0, x1, y1)) in leaves.iter().enumerate() {
        let id = i as u8;
        for y in y0..y1 {
            for x in x0..x1 {
                grid[idx(x, y)] = id;
            }
        }
    }
    grid
}

/// Nibbles up to [`JITTER_BAND`] cells off every leaf's edge into whichever
/// neighboring leaf touches it, so the finished map reads as hand-drawn
/// borders rather than a spreadsheet of rectangles. Confined to the edge
/// band, so the guaranteed interior (leaf inset by the band) never loses a
/// cell no matter how the coin lands, which is what keeps every province's
/// block-and-label floor intact after jitter runs.
fn jitter_boundaries(grid: &[u8], seed: u32) -> Vec<u8> {
    let idx = |x: i32, y: i32| (y * WORLD_W + x) as usize;
    let mut jittered = grid.to_owned();
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let here = grid[idx(x, y)];
            let near_edge = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| {
                let (nx, ny) = (x + dx * JITTER_BAND, y + dy * JITTER_BAND);
                nx < 0 || ny < 0 || nx >= WORLD_W || ny >= WORLD_H || grid[idx(nx, ny)] != here
            });
            if !near_edge || hash01(seed ^ 0xB16B_00B5, x, y) < 0.5 {
                continue;
            }
            for &(dx, dy) in &[(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= WORLD_W || ny >= WORLD_H {
                    continue;
                }
                let neighbor = grid[idx(nx, ny)];
                if neighbor != here && hash01(seed ^ 0x5EED_5EED, x, y) < 0.22 {
                    jittered[idx(x, y)] = neighbor;
                    break;
                }
            }
        }
    }
    jittered
}

/// Inclusive `(min_x, min_y, max_x, max_y)` bounds of every province's cells.
fn province_bboxes(grid: &[u8], count: usize) -> Vec<(i32, i32, i32, i32)> {
    let idx = |x: i32, y: i32| (y * WORLD_W + x) as usize;
    let mut bbox = vec![(i32::MAX, i32::MAX, i32::MIN, i32::MIN); count];
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let id = grid[idx(x, y)] as usize;
            let b = &mut bbox[id];
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        }
    }
    bbox
}

/// Every pair of provinces whose cells touch across the grid's north or west
/// edge (each boundary counted once, as `10_political` does for its border
/// pass), sorted and deduplicated so lookup order never depends on hash
/// iteration.
fn province_adjacency(grid: &[u8]) -> Vec<(u8, u8)> {
    let idx = |x: i32, y: i32| (y * WORLD_W + x) as usize;
    let mut pairs: Vec<(u8, u8)> = Vec::new();
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let here = grid[idx(x, y)];
            if x > 0 {
                let west = grid[idx(x - 1, y)];
                if west != here {
                    pairs.push((here.min(west), here.max(west)));
                }
            }
            if y > 0 {
                let north = grid[idx(x, y - 1)];
                if north != here {
                    pairs.push((here.min(north), here.max(north)));
                }
            }
        }
    }
    pairs.sort_unstable();
    pairs.dedup();
    pairs
}

/// Builds one province's identity and stats from its bounding box and its
/// entry in the shared adjacency list.
fn build_province(
    i: usize,
    capital: usize,
    bbox: (i32, i32, i32, i32),
    pairs: &[(u8, u8)],
    seed: u32,
) -> Province {
    let (x0, y0, x1, y1) = bbox;
    let centroid = (i32::midpoint(x0, x1), i32::midpoint(y0, y1));
    let neighbors: Vec<usize> = pairs
        .iter()
        .filter_map(|&(a, b)| {
            if usize::from(a) == i {
                Some(usize::from(b))
            } else if usize::from(b) == i {
                Some(usize::from(a))
            } else {
                None
            }
        })
        .collect();
    let roll = hash01(seed ^ 0xA5A5_1234, i as i32, 0);
    let owner = if i == capital {
        Owner::Player
    } else if roll < 0.3 {
        Owner::Rival
    } else if roll < 0.5 {
        Owner::Player
    } else {
        Owner::Independent
    };
    let area = ((x1 - x0) * (y1 - y0)).max(1) as u32;
    Province {
        name: province_name(seed, i),
        terrain: Terrain::from_index((hash01(seed ^ 0x007E_441E, i as i32, 1) * 4.0) as u32),
        owner,
        capital: i == capital,
        bbox: (x0, y0, x1 - 1, y1 - 1),
        centroid,
        neighbors,
        population: 4_000 + area * 40,
        income: 60 + area / 3,
        supply: 200 + area * 2,
        unrest: (hash01(seed ^ 0x9911, i as i32, 2) * 20.0) as u8,
        defence: 10 + u32::from(owner_defence_bonus(owner)),
    }
}

const fn owner_defence_bonus(owner: Owner) -> u8 {
    match owner {
        Owner::Player => 10,
        Owner::Rival => 8,
        Owner::Independent => 2,
    }
}

/// Builds the fixed five-commander roster the left column shows: two field
/// commanders on Patrol, two researchers, and the pretender itself carrying a
/// Move order and the largest army on the map.
fn build_roster(provinces: &[Province]) -> Vec<Commander> {
    let capital = provinces.iter().position(|p| p.capital).unwrap_or(0);
    let mut owned: Vec<usize> = provinces
        .iter()
        .enumerate()
        .filter(|(i, p)| *i != capital && p.owner == Owner::Player)
        .map(|(i, _)| i)
        .collect();
    owned.sort_unstable();

    let patrol_a = owned.first().copied().unwrap_or(capital);
    let patrol_b = owned.get(1).copied().unwrap_or(capital);

    vec![
        Commander {
            name: "Kestrel",
            sigil: '\u{2640}',
            order: Order::Patrol,
            leadership: 14,
            province: patrol_a,
            army: Some(ArmyTier::Scout),
        },
        Commander {
            name: "Varyn",
            sigil: '\u{2642}',
            order: Order::Defend,
            leadership: 16,
            province: patrol_b,
            army: Some(ArmyTier::Legion),
        },
        Commander {
            name: "Old Moro",
            sigil: '\u{263C}',
            order: Order::Research,
            leadership: 12,
            province: capital,
            army: None,
        },
        Commander {
            name: "Sister Ilse",
            sigil: '\u{2665}',
            order: Order::Research,
            leadership: 14,
            province: capital,
            army: None,
        },
        Commander {
            name: "Ashkindler",
            sigil: '\u{263B}',
            order: Order::Move,
            leadership: 14,
            province: capital,
            army: Some(ArmyTier::Host),
        },
    ]
}

impl FaithWar {
    /// Whether `to` is a province adjacent to `from` (or `from` itself).
    fn adjacent_or_same(&self, from: usize, to: usize) -> bool {
        from == to || self.provinces[from].neighbors.contains(&to)
    }

    /// Advances the march animation, committing the move once it finishes.
    fn simulate(&mut self, dt: f32) {
        self.anim.banner_phase = (self.time * 1.4).fract() < 0.5;
        self.anim.breathe = (self.time * 0.9).fract() < 0.5;

        let Some(march) = &mut self.march else {
            return;
        };
        march.t += dt / MARCH_SECONDS;
        if march.t >= 1.0 {
            let (commander, to) = (march.commander, march.to);
            if let Some(c) = self.commanders.get_mut(commander) {
                c.province = to;
            }
            let name = self.commanders[commander].name;
            let dest = &self.provinces[to].name;
            self.log
                .push(format!("{name} arrives at {dest}."), ui::ACCENT);
            self.march = None;
        }
    }

    /// Ends the turn: commits any pending move order into a march, advances
    /// the treasury, and ticks the turn counter. A second End Turn press
    /// while a march is already underway just banks income; Dominions itself
    /// would be resolving a dozen other armies' orders at the same moment,
    /// but this demo has exactly one to animate, so only one marches at a
    /// time.
    fn end_turn(&mut self) {
        self.treasury += self.income;
        self.turn += 1;
        if let Some((commander, target)) = self.pending.take() {
            let from = self.commanders[commander].province;
            self.march = Some(March {
                commander,
                from,
                to: target,
                t: 0.0,
            });
            let name = self.commanders[commander].name;
            let dest = &self.provinces[target].name;
            self.log.push(format!("{name} marches on {dest}."), ui::FG);
        } else {
            self.log
                .push(format!("Turn {} begins.", self.turn), ui::DIM);
        }
    }

    /// Handles a tap that landed on world cell `(wx, wy)`.
    ///
    /// Tap-select-then-tap-target: the map itself carries no drag-to-target
    /// interaction, because a dragged destination is exactly the case the
    /// touch guidance warns about -- a finger dragging an army icon would
    /// occlude the very province it is trying to land on.
    fn handle_map_tap(&mut self, wx: i32, wy: i32) {
        if wx < 0 || wy < 0 || wx >= WORLD_W || wy >= WORLD_H {
            return;
        }
        let id = self.grid[(wy * WORLD_W + wx) as usize] as usize;
        self.viewed = id;

        let Some(sel) = self.selected else {
            return;
        };
        let from = self.commanders[sel].province;
        if self.commanders[sel].army.is_none() {
            return; // a researcher has no army to march anywhere
        }
        if id == from {
            self.pending = None;
            return;
        }
        if self.adjacent_or_same(from, id) {
            self.pending = Some((sel, id));
            let name = self.commanders[sel].name;
            let dest = &self.provinces[id].name;
            self.log
                .push(format!("{name}: order set for {dest}."), ui::ACCENT);
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Commander(i) => {
                self.selected = Some(i);
                self.pending = None;
                self.viewed = self.commanders[i].province;
            }
            Action::EndTurn => self.end_turn(),
            Action::ArmySetup => self.setup_overlay = !self.setup_overlay,
            Action::Recruit => {
                if let Some(sel) = self.selected
                    && let Some(tier) = self.commanders[sel].army
                    && self.treasury >= 100
                {
                    self.treasury -= 100;
                    self.commanders[sel].army = Some(tier.grow());
                    let name = self.commanders[sel].name;
                    self.log
                        .push(format!("{name}'s ranks swell with recruits."), ui::FG);
                } else {
                    self.log
                        .push("Recruit: select a field commander first.", ui::DIM);
                }
            }
            Action::Research => {
                self.income += 4;
                self.log.push("Research: another rune deciphered.", ui::FG);
            }
            Action::Filters => self.filters_bare = !self.filters_bare,
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Tab => {
                        let n = self.commanders.len();
                        self.selected = Some(self.selected.map_or(0, |s| (s + 1) % n));
                        self.pending = None;
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => self.end_turn(),
                    KeyCode::Char('f' | 'F') => self.filters_bare = !self.filters_bare,
                    KeyCode::Char('r' | 'R') => self.handle_action(Action::Recruit),
                    KeyCode::Char('g' | 'G') => self.handle_action(Action::Research),
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.camera.pan(0, -2),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.camera.pan(0, 2),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.camera.pan(-2, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.camera.pan(2, 0),
                    _ => {}
                }
            }
            self.pointer.feed(&event);
        }
        true
    }

    /// Applies this frame's gesture against the hotspots and map area *as
    /// they were left by the previous frame's draw* -- the standard
    /// immediate-mode lag every demo in this gallery uses, since a hotspot
    /// cannot exist before the frame that draws it does.
    fn apply_gesture(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        if let Some(&action) = self.hotspots.hit(pos) {
            self.handle_action(action);
            return;
        }
        if self.map_area.contains(pos.x, pos.y) {
            let screen = Cell::new(
                i32::from(pos.x) - i32::from(self.map_area.left()),
                i32::from(pos.y) - i32::from(self.map_area.top()),
            );
            let world = self.camera.screen_to_world(screen);
            self.handle_map_tap(world.x, world.y);
        }
    }

    // ── drawing ──────────────────────────────────────────────────────────

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.map_area = area;
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        self.camera
            .set_viewport(i32::from(area.width()), i32::from(area.height()));
        let (left, top, right, bottom) = self.camera.visible_cells();

        for wy in top..=bottom {
            for wx in left..=right {
                if wx < 0 || wy < 0 || wx >= WORLD_W || wy >= WORLD_H {
                    continue;
                }
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                let (glyph, fg, bg) = self.render_cell(wx, wy);
                surface.put(at, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        self.draw_borders(surface, area);
        self.draw_labels(surface, area);
        self.draw_order_arrow(surface, area);
        self.draw_armies(surface, area);
    }

    /// Terrain glyph plus dominion wash for one world cell.
    fn render_cell(&self, x: i32, y: i32) -> (char, Color, Color) {
        let id = self.grid[(y * WORLD_W + x) as usize] as usize;
        let province = &self.provinces[id];
        let terrain = province.terrain;

        let show_glyph = !self.filters_bare && hash01(0x1357, x, y) < terrain.density();
        let mut fg = terrain.color();
        let mut bg = scale(terrain.color(), 0.35);

        if !self.filters_bare
            && let Some(tint) = province.dominion_glow(x, y, self.time)
        {
            fg = mix(fg, tint, 0.35);
            bg = mix(bg, tint, 0.55);
        } else if self.filters_bare {
            bg = scale(ui::BG, 1.0);
        }

        let glyph = if show_glyph { terrain.glyph() } else { ' ' };
        (glyph, fg, bg)
    }

    /// Border cells: a cell whose north or west neighbor is a different
    /// province, matching `10_political`'s single-count-per-edge rule.
    fn draw_borders(&self, surface: &mut Surface<'_>, area: Rect) {
        let (left, top, right, bottom) = self.camera.visible_cells();
        for wy in top.max(0)..=bottom.min(WORLD_H - 1) {
            for wx in left.max(0)..=right.min(WORLD_W - 1) {
                let here = self.grid[(wy * WORLD_W + wx) as usize];
                let west_diff = wx > 0 && self.grid[(wy * WORLD_W + wx - 1) as usize] != here;
                let north_diff = wy > 0 && self.grid[((wy - 1) * WORLD_W + wx) as usize] != here;
                if !west_diff && !north_diff {
                    continue;
                }
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                let (_, _, base) = self.render_cell(wx, wy);
                surface.put(at, '\u{00b7}', Style::new().fg(rgb(230, 214, 140)).bg(base));
            }
        }
    }

    fn draw_labels(&self, surface: &mut Surface<'_>, area: Rect) {
        for province in &self.provinces {
            let (x0, y0, _, _) = province.bbox;
            let screen = self.camera.world_to_screen(Cell::new(x0 + 1, y0));
            if !self.camera.on_screen(screen) {
                continue;
            }
            let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
            if at.0 + province.name.chars().count() as u16 > area.right() || at.1 >= area.bottom() {
                continue;
            }
            let color = if province.capital { ui::ACCENT } else { ui::FG };
            surface.print(
                at,
                &province.name,
                Style::new().fg(color).bg(scale(ui::BG, 1.0)),
            );
        }
    }

    /// Where a commander's army block currently sits, mid-march included: a
    /// linear blend between the source and target centroids, converted to
    /// its top-left screen cell so the block draws consistently through
    /// [`Self::army_block_origin`].
    fn army_world_pos(&self, commander: usize) -> (f32, f32) {
        if let Some(march) = &self.march
            && march.commander == commander
        {
            let (fx, fy) = self.provinces[march.from].centroid;
            let (tx, ty) = self.provinces[march.to].centroid;
            let t = march.t.clamp(0.0, 1.0);
            let (fx, fy) = (f32::from(fx as i16), f32::from(fy as i16));
            let (tx, ty) = (f32::from(tx as i16), f32::from(ty as i16));
            return ((tx - fx).mul_add(t, fx), (ty - fy).mul_add(t, fy));
        }
        let (cx, cy) = self.provinces[self.commanders[commander].province].centroid;
        (f32::from(cx as i16), f32::from(cy as i16))
    }

    fn draw_order_arrow(&self, surface: &mut Surface<'_>, area: Rect) {
        let Some((commander, target)) = self.pending else {
            return;
        };
        let (fx, fy) = self.army_world_pos(commander);
        let (tx, ty) = self.provinces[target].centroid;
        let (tx, ty) = (f32::from(tx as i16), f32::from(ty as i16));
        let steps: i32 = 24;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let wx = (tx - fx).mul_add(t, fx);
            let wy = (ty - fy).mul_add(t, fy);
            let screen = self
                .camera
                .world_to_screen(Cell::new(wx.round() as i32, wy.round() as i32));
            if !self.camera.on_screen(screen) {
                continue;
            }
            let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
            let glyph = if i == steps { '\u{25BA}' } else { '\u{2500}' };
            let (wxi, wyi) = (wx.round() as i32, wy.round() as i32);
            let (_, _, base) =
                self.render_cell(wxi.clamp(0, WORLD_W - 1), wyi.clamp(0, WORLD_H - 1));
            surface.put(at, glyph, Style::new().fg(rgb(220, 90, 70)).bg(base));
        }

        if self.setup_overlay {
            let dest = &self.provinces[target].name;
            let arrival = self.turn + 1;
            let text = format!("-> {dest} (turn {arrival})");
            let screen = self
                .camera
                .world_to_screen(Cell::new(tx.round() as i32, ty.round() as i32 - 3));
            if self.camera.on_screen(screen) {
                let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                if at.0 + text.chars().count() as u16 <= area.right() {
                    surface.print(
                        at,
                        &text,
                        Style::new().fg(ui::ACCENT).bg(scale(ui::BG, 1.0)),
                    );
                }
            }
        }
    }

    fn draw_armies(&self, surface: &mut Surface<'_>, area: Rect) {
        for (i, commander) in self.commanders.iter().enumerate() {
            let Some(tier) = commander.army else { continue };
            // A marching army is drawn once, from its interpolated position,
            // not twice (once at the source and once mid-flight).
            let pos = self.army_world_pos(i);
            let has_banner = matches!(commander.order, Order::Move | Order::Patrol);
            self.draw_army_block(
                surface,
                area,
                pos,
                tier,
                has_banner,
                Some(i) == self.selected,
            );
        }
    }

    /// Draws one army block centered on world position `(cx, cy)`: an
    /// optional banner-and-pole above it, then rows of unit glyphs banded
    /// infantry-then-archers, with the commander's own glyph replacing the
    /// centre cell of the archer row on a led army.
    fn draw_army_block(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        center: (f32, f32),
        tier: ArmyTier,
        has_banner: bool,
        selected: bool,
    ) {
        let (cx, cy) = center;
        let (bw, bh) = tier.dims();
        let origin_x = cx - f32::from(bw) / 2.0;
        // "Breathing": the whole block steps down by exactly one cell on a
        // slow two-state clock, rather than easing smoothly, so a still
        // frame always shows a crisp formation and a running one shows it
        // settle its weight -- matching the addendum's rule that idle motion
        // on anything text-adjacent should be a discrete step, not a fade.
        // Moving the banner, pole, and every rank together (rather than only
        // the back rank) is what keeps the ranks from ever drawing over one
        // another: a rigid shift never collides with itself.
        let bob = if self.anim.breathe { 1.0 } else { 0.0 };
        let origin_y = cy - f32::from(bh) / 2.0 + bob;
        let archer_rows = tier.archer_rows();
        let commander_col = bw / 2;

        if has_banner {
            // The banner glyph alternates between two fixed states on its own
            // clock -- a discrete step, not an eased fade, exactly what the
            // addendum asks for on anything that reads as text-adjacent. The
            // pole sits directly beneath it, one row above the block itself.
            let flag = if self.anim.banner_phase {
                '\u{2588}'
            } else {
                '\u{2593}'
            };
            let pole_bg = rgb(18, 16, 10);
            let pole_x = origin_x + f32::from(commander_col);
            self.put_world(
                surface,
                area,
                (pole_x, origin_y - 2.0),
                flag,
                rgb(230, 200, 90),
                Some(pole_bg),
            );
            self.put_world(
                surface,
                area,
                (pole_x, origin_y - 1.0),
                '\u{2502}',
                rgb(180, 160, 120),
                Some(pole_bg),
            );
        }

        for row in 0..bh {
            let is_archer_row = row < archer_rows;
            for col in 0..bw {
                let is_commander_cell = has_banner
                    && is_archer_row
                    && row == archer_rows.saturating_sub(1)
                    && col == commander_col;
                let (glyph, color) = if is_commander_cell {
                    ('\u{263B}', rgb(240, 210, 120))
                } else if is_archer_row {
                    ('\u{2191}', rgb(180, 176, 150))
                } else {
                    ('\u{2660}', rgb(196, 150, 96))
                };
                let selected_glow = if selected {
                    Some(rgb(48, 40, 18))
                } else {
                    None
                };
                let at = (origin_x + f32::from(col), origin_y + f32::from(row));
                self.put_world(surface, area, at, glyph, color, selected_glow);
            }
        }
    }

    /// Writes one glyph at a fractional world position, rounding to the
    /// nearest cell and clipping to the camera viewport and `area`. `tint`,
    /// if given, replaces the terrain background under the glyph (used for
    /// the selected army's block and the banner/pole), so a unit reads as a
    /// solid sprite rather than a colored letter fighting the map under it.
    fn put_world(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        pos: (f32, f32),
        glyph: char,
        fg: Color,
        tint: Option<Color>,
    ) {
        let (wx, wy) = pos;
        let world_x = wx.round() as i32;
        let world_y = wy.round() as i32;
        let screen = self.camera.world_to_screen(Cell::new(world_x, world_y));
        if !self.camera.on_screen(screen) {
            return;
        }
        let at = (area.left() + screen.x as u16, area.top() + screen.y as u16);
        if at.0 >= area.right() || at.1 >= area.bottom() {
            return;
        }
        let base = tint.unwrap_or_else(|| {
            let (_, _, bg) =
                self.render_cell(world_x.clamp(0, WORLD_W - 1), world_y.clamp(0, WORLD_H - 1));
            bg
        });
        surface.put(at, glyph, Style::new().fg(fg).bg(base));
    }
}

impl FaithWar {
    /// One-line status text for the bottom chrome bar.
    fn status_text(&self) -> String {
        let sel = self
            .selected
            .map_or_else(|| "none".to_owned(), |i| self.commanders[i].name.to_owned());
        format!(
            "turn {}  treasury {}  selected: {}",
            self.turn, self.treasury, sel
        )
    }

    /// Top band: the province readout on the left, the pretender/treasury
    /// banner beside it. Both are read-only status, which is why they sit at
    /// the top rather than the bottom -- the thumb zone is for actions.
    fn draw_top_band(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let readout_w = 36u16.min(area.width());
        let (readout, rest) = panel::split_left(area, readout_w);
        self.draw_province_readout(surface, readout);
        if rest.width() >= 30 {
            let banner_w = 42u16.min(rest.width());
            let (banner, _) = panel::split_left(rest, banner_w);
            self.draw_pretender_banner(surface, banner);
        }
    }

    fn draw_province_readout(&self, surface: &mut Surface<'_>, area: Rect) {
        let province = &self.provinces[self.viewed];
        let inner = Panel::new()
            .title(&province.name)
            .border(Border::Double)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let rows: [(&str, String); 5] = [
            ("Population", format!("{}", province.population)),
            ("Income", format!("{}", province.income)),
            ("Supply", format!("{}", province.supply)),
            ("Unrest", format!("{}", province.unrest)),
            ("Defence", format!("{}", province.defence)),
        ];
        for (i, (label, value)) in rows.iter().enumerate() {
            if i as u16 >= inner.height() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), inner.top() + i as u16),
                inner.width(),
                &[Span::dim(label), Span::plain(": "), Span::keyword(value)],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_pretender_banner(&self, surface: &mut Surface<'_>, area: Rect) {
        let pretender = self
            .commanders
            .iter()
            .find(|c| c.order == Order::Move)
            .map_or("the pretender", |c| c.name);
        let inner = Panel::new()
            .title(pretender)
            .border(Border::Double)
            .frame(ui::ACCENT)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::dim("Treasury "),
                Span::keyword(&format!("{}", self.treasury)),
                Span::plain("  "),
                Span::dim("Income "),
                Span::keyword(&format!("+{}", self.income)),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::dim(&format!("Turn {}", self.turn))],
                panel::PANEL_BG,
            );
        }
    }

    /// One commander card: name and sigil, order, leadership. Selection is
    /// carried by border weight and brightness (matching `card.rs`'s
    /// convention), which is what a touch interface needs since there is no
    /// hover state to lean on instead.
    fn draw_commander_card(&mut self, surface: &mut Surface<'_>, rect: Rect, i: usize) {
        let commander = &self.commanders[i];
        let selected = self.selected == Some(i);
        let inner = Panel::new()
            .border(if selected {
                Border::Double
            } else {
                Border::Single
            })
            .focused(selected)
            .draw(surface, rect);
        self.hotspots
            .push_tappable(rect, rect, Action::Commander(i));
        if inner.height() == 0 {
            return;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::new(&commander.sigil.to_string(), ui::ACCENT),
                Span::plain(" "),
                Span::keyword(commander.name),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            let order_color = match commander.order {
                Order::Move => ui::ACCENT,
                Order::Patrol => rgb(140, 190, 140),
                Order::Defend => rgb(160, 160, 210),
                Order::Research => ui::DIM,
            };
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::new(commander.order.label(), order_color)],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 2 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[Span::dim(&format!("Ldr {}", commander.leadership))],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 3 {
            let text = commander.army.map_or_else(
                || "no army".to_owned(),
                |tier| format!("{} x{}", tier.label(), tier.head_count()),
            );
            panel::spans(
                surface,
                (inner.left(), inner.top() + 3),
                inner.width(),
                &[Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
    }

    /// Vertical roster down the left edge, on a wide viewport.
    fn draw_commander_column(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 {
            return;
        }
        let card_h = 6u16.max(TAP_H);
        let n = self.commanders.len();
        for i in 0..n {
            let y = area.top() + i as u16 * card_h;
            if y >= area.bottom() {
                break;
            }
            let h = card_h.min(area.bottom() - y);
            self.draw_commander_card(surface, Rect::new(area.left(), y, area.width(), h), i);
        }
    }

    /// Horizontal scroller under the map, on a portrait phone. Deliberately
    /// no scroll offset: five cards at the minimum tappable width already
    /// fit a phone's 73-column portrait viewport (5 * 9 = 45), so a real
    /// scroll would add complexity a five-item roster never needs.
    fn draw_commander_scroller(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let n = self.commanders.len().max(1) as u16;
        let card_w = (area.width() / n).max(TAP_W);
        for i in 0..self.commanders.len() {
            let x = area.left() + i as u16 * card_w;
            if x >= area.right() {
                break;
            }
            let w = card_w.min(area.right() - x);
            self.draw_commander_card(surface, Rect::new(x, area.top(), w, area.height()), i);
        }
    }

    /// One rail/action button. `rect` is grown to a legal tap target inside
    /// `bounds` before being registered, so a narrow rail still hits reliably.
    fn draw_rail_button(
        &mut self,
        surface: &mut Surface<'_>,
        rect: Rect,
        bounds: Rect,
        label: &str,
        action: Action,
    ) {
        let inner = Panel::new().draw(surface, rect);
        self.hotspots.push_tappable(rect, bounds, action);
        if inner.height() == 0 {
            return;
        }
        let y = inner.top() + inner.height() / 2;
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::plain(label)],
            panel::PANEL_BG,
        );
    }

    /// Vertical command rail, on a wide viewport.
    fn draw_rail(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 {
            return;
        }
        let items: [(&str, Action); 5] = [
            ("End Turn", Action::EndTurn),
            ("Army Setup", Action::ArmySetup),
            ("Recruit", Action::Recruit),
            ("Research", Action::Research),
            ("Filters", Action::Filters),
        ];
        let button_h = TAP_H + 1;
        for (i, (label, action)) in items.into_iter().enumerate() {
            let y = area.top() + i as u16 * button_h;
            if y >= area.bottom() {
                break;
            }
            let h = button_h.min(area.bottom() - y);
            let rect = Rect::new(area.left(), y, area.width(), h);
            self.draw_rail_button(surface, rect, area, label, action);
        }
    }

    /// Bottom action row, on a portrait phone: the same five commands laid
    /// out side by side within thumb reach, per the mobile-first rule that
    /// primary actions live in the bottom third.
    fn draw_action_row(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let items: [(&str, Action); 5] = [
            ("End Turn", Action::EndTurn),
            ("Setup", Action::ArmySetup),
            ("Recruit", Action::Recruit),
            ("Research", Action::Research),
            ("Filters", Action::Filters),
        ];
        let n = items.len() as u16;
        let button_w = (area.width() / n).max(TAP_W.min(area.width()));
        for (i, (label, action)) in items.into_iter().enumerate() {
            let x = area.left() + i as u16 * button_w;
            if x >= area.right() {
                break;
            }
            let w = button_w.min(area.right() - x);
            let rect = Rect::new(x, area.top(), w, area.height());
            self.draw_rail_button(surface, rect, area, label, action);
        }
    }

    /// Layout for a wide viewport (landscape phone or desktop): commander
    /// column on the left, map in the middle, command rail on the right, a
    /// combined status/treasury band across the top.
    fn draw_wide(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let top_h = 5u16.min(content.height() / 3);
        let (top_band, rest) = panel::split_top(content, top_h);

        let cmd_w = 20u16.min(rest.width() / 4);
        let rail_w = 20u16.min(rest.width() / 4);
        let (cmd_col, rest2) = panel::split_left(rest, cmd_w);
        let (map_area, rail) = panel::split_right(rest2, rail_w);

        self.draw_map(surface, map_area);
        self.draw_commander_column(surface, cmd_col);
        self.draw_rail(surface, rail);
        self.draw_top_band(surface, top_band);
    }

    /// Layout for a portrait phone: the map keeps the top of the content
    /// area (and its army blocks stay full size; only the *pan* shrinks the
    /// number of visible provinces), the roster becomes a horizontal
    /// scroller, and the command rail becomes a bottom action row within
    /// thumb reach.
    fn draw_portrait(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let top_h = 4u16.min(content.height() / 5);
        let (top_band, rest) = panel::split_top(content, top_h);

        let action_h = (TAP_H + 2).min(rest.height() / 3);
        let (rest2, action_row) = panel::split_bottom(rest, action_h);
        let cards_h = 5u16.min(rest2.height() / 3);
        let (map_area, cards_row) = panel::split_bottom(rest2, cards_h);

        self.draw_map(surface, map_area);
        self.draw_commander_scroller(surface, cards_row);
        self.draw_action_row(surface, action_row);
        self.draw_top_band(surface, top_band);
    }
}

impl Demo for FaithWar {
    const NAME: &'static str = "37_faith_war";
    const TITLE: &'static str = "37 Faith War";
    const BLURB: &'static str = "Dominions province map: armies drawn as formations, not tokens.";
    const GRID: (u16, u16) = (170, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "cycle commander"),
            ("tap province", "view / order march"),
            ("Enter/Space", "end turn"),
            ("WASD/arrows", "pan map"),
            ("R", "recruit"),
            ("G", "research"),
            ("F", "toggle filters"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.apply_gesture();
        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.hotspots.clear();
        if Shape::of(content).stacks() {
            self.draw_portrait(&mut surface, content);
        } else {
            self.draw_wide(&mut surface, content);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(FaithWar);

#[cfg(test)]
mod tests {
    use super::{
        MIN_LEAF_H, MIN_LEAF_W, STEMS, SUFFIXES, WORLD_H, WORLD_W, generate_provinces,
        province_name,
    };

    /// Regression test for the bug where two provinces could share a name
    /// (`Greywatch`, `Fenmoor`, and `Highcairn` each appeared twice on a
    /// single generated map under the old 16-name flat pool). Checks both
    /// the low-level name builder directly against every index the BSP
    /// split can possibly produce, and the actual generated map end to end.
    #[test]
    fn every_generated_province_name_is_unique() {
        for seed in [1u32, 37, 12345, 999_999] {
            let (_, provinces) = generate_provinces(seed);
            let mut names: Vec<&str> = provinces.iter().map(|p| p.name.as_str()).collect();
            names.sort_unstable();
            let mut deduped = names.clone();
            deduped.dedup();
            assert_eq!(
                names.len(),
                deduped.len(),
                "seed {seed} produced a duplicate province name among {names:?}"
            );
        }
    }

    /// The theoretical maximum number of leaves [`split`](super::split) can
    /// produce for a `w`x`h` area: mirrors that function's own branching
    /// (split on the more elongated axis, recurse on both halves, stop once
    /// neither axis can legally split again), but only ever counts leaves --
    /// it does not need `Rng` because every branch is taken in the worst
    /// case regardless of which one an actual seed picks.
    fn max_leaves(w: i32, h: i32) -> usize {
        let can_split_w = w > MIN_LEAF_W * 2;
        let can_split_h = h > MIN_LEAF_H * 2;
        if !can_split_w && !can_split_h {
            return 1;
        }
        // The smallest legal split point is the branch that leaves the most
        // area for further recursion on both sides, so it is the split that
        // maximizes total leaf count -- the same reason a fixed split point
        // is enough to bound the worst case without walking every possible
        // split point.
        if can_split_w && (!can_split_h || w > h) {
            max_leaves(MIN_LEAF_W, h) + max_leaves(w - MIN_LEAF_W, h)
        } else {
            max_leaves(w, MIN_LEAF_H) + max_leaves(w, h - MIN_LEAF_H)
        }
    }

    /// The real trip wire: computes the worst-case leaf count from the
    /// actual live constants (not a hardcoded number) and checks it against
    /// the actual live name pool size. If `WORLD_W`/`WORLD_H`/the leaf
    /// minimums/the stem or suffix lists ever change, this fails instead of
    /// silently letting the flat-pool bug back in through the side door.
    #[test]
    fn name_pool_stays_larger_than_the_worst_case_leaf_count() {
        let worst_case = max_leaves(WORLD_W, WORLD_H);
        let pool = STEMS.len() * SUFFIXES.len();
        assert!(
            worst_case <= pool,
            "world/leaf-size constants can produce {worst_case} leaves, \
             but the name pool only has {pool} combinations"
        );
    }

    /// `province_name` must not collide across the full worst-case leaf
    /// count the live constants can produce, independent of which map
    /// actually gets generated -- this is what makes the guarantee
    /// "provably unique" rather than "unique on the seeds we happened to
    /// try".
    #[test]
    fn province_name_has_no_collisions_up_to_the_worst_case_leaf_count() {
        let worst_case_leaves = max_leaves(WORLD_W, WORLD_H);
        for seed in [0u32, 1, 37, 42, u32::MAX] {
            let mut names: Vec<String> = (0..worst_case_leaves)
                .map(|i| province_name(seed, i))
                .collect();
            names.sort_unstable();
            let mut deduped = names.clone();
            deduped.dedup();
            assert_eq!(
                names.len(),
                deduped.len(),
                "seed {seed} produced a duplicate among the worst-case name set"
            );
        }
    }
}
