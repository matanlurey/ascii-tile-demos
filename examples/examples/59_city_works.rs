//! 59: City Works -- Civilization II's city screen: a dozen small readouts
//! ringing one large worked-tile view, all live functions of citizen
//! assignment.
//!
//! Every readout panel here (Food, Production, Trade, Pollution, Corruption,
//! Improvements) reads off the same underlying state as the map: which
//! citizen is standing on which tile, or working which specialist job. Move a
//! citizen off a wheat tile and the food readout drops, the granary
//! countdown slows, and -- because shields also moved -- the build queue's
//! turn estimate changes too. Nothing on this screen is typed in; it is all
//! [`CityWorks::totals`].
//!
//! Techniques on show:
//!
//! - **A worked-tile map as the interactive centre**
//!   ([`CityWorks::draw_map`]): the classic Civilization "fat cross" of 21
//!   tiles (city centre plus 20 workable rings, corners of the 5x5 excluded),
//!   each cell large enough to show both its terrain and, once a citizen is
//!   assigned, the food/shield/trade it yields. Tapping an unworked tile
//!   pulls a specialist onto it; tapping a worked tile sends its citizen back
//!   to being a specialist. Both directions go through one totals function,
//!   so the change is instant everywhere on screen.
//! - **A citizen strip with three specialist roles and three moods**
//!   ([`CityWorks::draw_citizens`]): each citizen is its own small tappable
//!   card, bordered in the colour of its mood (content baseline offset by
//!   luxury from trade and from entertainers), labelled with either the tile
//!   it works or the specialist role it holds. Tapping cycles a specialist's
//!   role, or frees a tile-worker back to being a specialist -- the map tap
//!   handles the other direction.
//! - **A self-paging readout grid** ([`CityWorks::draw_readouts`]): six
//!   boxed panels want more room than a phone-sized sidebar has. Rather than
//!   shrink them below legibility, the grid computes how many panels of
//!   minimum legible size actually fit and, if that is fewer than six, pages
//!   through the rest on a timer driven by [`retroglyph_core::Frame::delta`]
//!   -- everything is still shown, just not all at once.
//! - **A build queue with a real progress bar** ([`CityWorks::draw_build`]):
//!   turns remaining is `ceil(shields still needed / net shields per turn)`,
//!   not a made-up counter, and switching production (tap, or Left/Right with
//!   the panel focused) resets invested shields the way changing a build
//!   category does in the source game.
//! - **Turn-stepped simulation, not per-frame drift**: food and shields
//!   accumulate every frame, but growth, build completion, and every visible
//!   number only change once a turn boundary is crossed
//!   ([`CityWorks::advance_turns`]). A number on this screen either holds
//!   steady or steps to a new value; it never eases toward one.
//!
//! ```sh
//! cargo run --example 59_city_works --features crossterm
//! cargo run --example 59_city_works --features software
//! cargo run --example 59_city_works --features gl
//! cargo run --example 59_city_works  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Log, Panel, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::rgb;

// ── Tuning constants ────────────────────────────────────────────────────
//
// Every one of these is a rule the totals function below applies, not a
// display value, so changing one changes what the screen reports as well as
// what it means.

/// Citizens eat this much food each; the classic Civilization ration. Food
/// surplus is gross tile food minus `citizens.len() * FOOD_PER_CITIZEN`.
const FOOD_PER_CITIZEN: i32 = 2;
/// Fraction of gross shields lost to distance/inefficiency before a turn's
/// production counts toward the build queue.
const WASTE_RATE: f32 = 0.1;
/// Fraction of gross trade lost to corruption before the tax/luxury/science
/// split. Held constant rather than modelled on distance-from-capital: this
/// demo has exactly one city, so a distance term would always evaluate to the
/// same number and only add an unused knob.
const CORRUPTION_RATE: f32 = 0.15;
/// Share of post-corruption trade that becomes tax. Science takes the same
/// share; luxury is whatever remains, so the three always sum exactly to
/// trade net of corruption with no rounding leak.
const TAX_RATE: f32 = 0.4;
/// See [`TAX_RATE`].
const SCI_RATE: f32 = 0.4;
/// Flat output of one specialist citizen, in whichever resource their role
/// produces directly (bypassing the trade split entirely, as specialists do
/// in the source game).
const SPECIALIST_OUTPUT: i32 = 2;
/// Gross shields above this level start producing pollution. Below it a city
/// is assumed clean.
const POLLUTION_THRESHOLD: i32 = 10;
/// Shields-above-threshold needed for pollution to reach 100%.
const POLLUTION_SCALE: f32 = 18.0;
/// Citizens this content by default, before luxury conversions. The rest
/// start unhappy.
const CONTENT_BASE: i32 = 3;
/// Luxury needed to convert one unhappy citizen to content.
const LUX_PER_CONTENT: i32 = 2;
/// Additional luxury, on top of every unhappy-to-content conversion, needed
/// to lift one content citizen to happy.
const LUX_PER_HAPPY: i32 = 4;
/// Granary capacity at zero population; capacity grows with size so later
/// citizens take longer to arrive, matching the source game's curve.
const GRANARY_BASE: f32 = 8.0;
/// Granary capacity added per existing citizen. See [`GRANARY_BASE`].
const GRANARY_PER_CITIZEN: f32 = 3.0;
/// Simulated seconds per game turn. Long enough that a viewer watching the
/// demo idle sees the granary and build bars visibly creep before a turn
/// lands, which is what makes the eventual step read as a discrete event
/// rather than a glitch.
const TURN_SECONDS: f32 = 5.0;
/// Citizens the city starts with.
const INITIAL_SIZE: usize = 7;
/// Hard cap on population, so the citizen strip and the 20-tile ring never
/// have to reconcile more workers than there is room to employ or draw.
const MAX_SIZE: usize = 16;
/// Improvements the log keeps before the oldest scrolls off.
const IMPROVEMENT_LOG_CAP: usize = 8;
/// How long each page of the readout grid holds before advancing, when there
/// is not room to show all six panels at once. See [`CityWorks::draw_readouts`].
const PAGE_SECONDS: f32 = 3.5;

/// Terrain kinds present in the city's worked ring.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Ocean,
    Grassland,
    Plains,
    Forest,
    Hills,
    Desert,
    Mountains,
}

impl Terrain {
    /// The glyph drawn for a tile of this terrain when it is not worked.
    const fn glyph(self) -> char {
        match self {
            Self::Ocean => '~',
            Self::Grassland => '"',
            Self::Plains => '.',
            Self::Forest => '\u{2660}', // spade suit glyph, stands in for a tree
            Self::Hills => '\u{2229}',  // intersection glyph, reads as a hill hump
            Self::Desert => ',',
            Self::Mountains => '^',
        }
    }

    /// Four-character label for the map cell and the citizen strip.
    const fn label(self) -> &'static str {
        match self {
            Self::Ocean => "Ocen",
            Self::Grassland => "Gras",
            Self::Plains => "Plns",
            Self::Forest => "Frst",
            Self::Hills => "Hils",
            Self::Desert => "Dsrt",
            Self::Mountains => "Mtn",
        }
    }

    /// Base (food, shields, trade) yield before river/resource specials.
    const fn base_yield(self) -> (i32, i32, i32) {
        match self {
            Self::Ocean => (1, 0, 2),
            Self::Grassland => (2, 0, 0),
            Self::Plains => (1, 1, 0),
            Self::Forest | Self::Hills => (1, 2, 0),
            Self::Desert | Self::Mountains => (0, 1, 0),
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Ocean => rgb(50, 90, 140),
            Self::Grassland => rgb(96, 150, 76),
            Self::Plains => rgb(150, 140, 78),
            Self::Forest => rgb(58, 108, 62),
            Self::Hills => rgb(120, 104, 70),
            Self::Desert => rgb(176, 150, 96),
            Self::Mountains => rgb(120, 118, 122),
        }
    }
}

/// One tile in the worked ring: its terrain plus the final yield already
/// including river/resource bonuses, so drawing and totalling never
/// recompute the hash noise.
#[derive(Clone, Copy)]
struct Tile {
    terrain: Terrain,
    food: i32,
    shields: i32,
    trade: i32,
}

/// Offsets of the 20 workable tiles around the city centre: a 5x5 square
/// with its four corners removed, the "fat cross" every Civilization game
/// uses for a city's work radius. The centre itself, `(0, 0)`, is not in
/// this list -- it is worked automatically and is not a citizen's choice.
const RING_OFFSETS: [(i32, i32); 20] = [
    (-1, -2),
    (0, -2),
    (1, -2),
    (-2, -1),
    (-1, -1),
    (0, -1),
    (1, -1),
    (2, -1),
    (-2, 0),
    (-1, 0),
    (1, 0),
    (2, 0),
    (-2, 1),
    (-1, 1),
    (0, 1),
    (1, 1),
    (2, 1),
    (-1, 2),
    (0, 2),
    (1, 2),
];

/// Builds one deterministic tile from its ring offset. Terrain comes from a
/// hashed threshold ladder rather than a stored seed table so the whole ring
/// regenerates from nothing but `seed` and position -- the same trick
/// [`tilekit::noise`] uses everywhere else in the gallery, and it is what
/// lets [`CityWorks::reroll`] regenerate the world by changing one number.
fn generate_tile(seed: u32, dx: i32, dy: i32) -> Tile {
    let r = hash01(seed, dx, dy);
    let terrain = if r < 0.12 {
        Terrain::Ocean
    } else if r < 0.40 {
        Terrain::Grassland
    } else if r < 0.60 {
        Terrain::Plains
    } else if r < 0.75 {
        Terrain::Forest
    } else if r < 0.88 {
        Terrain::Hills
    } else if r < 0.95 {
        Terrain::Desert
    } else {
        Terrain::Mountains
    };

    let (mut food, mut shields, trade0) = terrain.base_yield();
    let mut trade = trade0;

    // A river bonus (+1 trade) never lands on ocean or mountain, and a
    // resource bonus (+1 shield on land, +1 food at sea) is drawn from an
    // independent hash so the two specials do not always coincide.
    let river = hash01(seed ^ 0x5241_5645, dx, dy) < 0.22;
    if river && !matches!(terrain, Terrain::Ocean | Terrain::Mountains) {
        trade += 1;
    }
    let resource = hash01(seed ^ 0x5245_534F, dx, dy) < 0.20;
    if resource {
        if terrain == Terrain::Ocean {
            food += 1;
        } else {
            shields += 1;
        }
    }

    Tile {
        terrain,
        food,
        shields,
        trade,
    }
}

/// Specialist jobs a citizen can hold instead of working a tile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Entertainer,
    Scientist,
    TaxCollector,
}

impl Role {
    /// The next role in the cycle a tap or Enter steps through.
    const fn next(self) -> Self {
        match self {
            Self::Entertainer => Self::Scientist,
            Self::Scientist => Self::TaxCollector,
            Self::TaxCollector => Self::Entertainer,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Entertainer => "Ent",
            Self::Scientist => "Sci",
            Self::TaxCollector => "Tax",
        }
    }

    /// A CP437 glyph standing in for the role's icon: a musical note for the
    /// entertainer, a Greek alpha for the scientist's formula, a dollar sign
    /// for the collector.
    const fn glyph(self) -> char {
        match self {
            Self::Entertainer => '\u{266A}',
            Self::Scientist => '\u{03B1}',
            Self::TaxCollector => '$',
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Entertainer => rgb(214, 140, 200),
            Self::Scientist => rgb(120, 176, 226),
            Self::TaxCollector => rgb(226, 196, 96),
        }
    }
}

/// What one citizen is doing: working a ring tile (by index into
/// [`RING_OFFSETS`]/`CityWorks::tiles`) or holding a specialist role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Assign {
    Worked(usize),
    Role(Role),
}

/// A city's citizen. Mood is not stored here -- it is derived fresh every
/// frame from the whole population's luxury output, so it can never drift
/// out of sync with the assignment that produces it.
#[derive(Clone, Copy)]
struct Citizen {
    assign: Assign,
}

/// A citizen's happiness state, purely a readout of [`Totals`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mood {
    Happy,
    Content,
    Unhappy,
}

impl Mood {
    const fn color(self) -> Color {
        match self {
            Self::Happy => rgb(120, 206, 120),
            Self::Content => rgb(170, 168, 180),
            Self::Unhappy => rgb(216, 96, 90),
        }
    }

    const fn glyph(self) -> char {
        match self {
            Self::Happy => '\u{263A}',   // smiley
            Self::Content => '\u{00B7}', // middle dot: quietly fine
            Self::Unhappy => 'x',
        }
    }
}

/// One entry in the production catalog: a name and a shield cost.
struct BuildItem {
    name: &'static str,
    cost: i32,
}

/// Fixed production catalog the build queue cycles through. A real game's
/// list is much longer and player-chosen; a small fixed ring is enough to
/// show a queue that has a "now" and a "next" without inventing a planner.
const CATALOG: [BuildItem; 8] = [
    BuildItem {
        name: "Warriors",
        cost: 10,
    },
    BuildItem {
        name: "Workers",
        cost: 16,
    },
    BuildItem {
        name: "Granary",
        cost: 32,
    },
    BuildItem {
        name: "Temple",
        cost: 24,
    },
    BuildItem {
        name: "Barracks",
        cost: 28,
    },
    BuildItem {
        name: "Library",
        cost: 48,
    },
    BuildItem {
        name: "Marketplace",
        cost: 40,
    },
    BuildItem {
        name: "Aqueduct",
        cost: 56,
    },
];

/// Which control has keyboard focus, for [`CityWorks::handle_key`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Map,
    Citizens,
    Build,
}

impl Focus {
    const fn next(self) -> Self {
        match self {
            Self::Map => Self::Citizens,
            Self::Citizens => Self::Build,
            Self::Build => Self::Map,
        }
    }
}

/// What tapping a hotspot means. Rebuilt every frame by
/// [`ascii_tile_demos::ui::touch::Hotspots`], so it only ever names something
/// that is actually on screen this frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Tile(usize),
    Citizen(usize),
    Build,
}

/// Every number the readout panels show, computed fresh from the current
/// citizen assignment. Nothing here is stored between frames except through
/// [`CityWorks::totals`] being called again -- that is the discipline the
/// brief asks for: move a citizen and every one of these fields is a
/// different number on the very next frame.
struct Totals {
    food_gross: i32,
    food_surplus: i32,
    shields_gross: i32,
    shields_net: i32,
    trade_gross: i32,
    corruption: i32,
    tax: i32,
    luxury: i32,
    science: i32,
    pollution_pct: i32,
    happy: i32,
    content: i32,
    unhappy: i32,
}

/// State for the City Works demo.
///
/// A generated ring of tiles, a population of citizens each assigned to a
/// tile or a specialist role, a build queue, and the turn clock that steps
/// growth and production forward.
pub struct CityWorks {
    seed: u32,
    tiles: [Tile; 20],
    citizens: Vec<Citizen>,
    granary: f32,
    shields_invested: f32,
    build_index: usize,
    completed: Log,
    turn: u32,
    turn_timer: f32,
    time: f32,
    focus: Focus,
    map_cursor: usize,
    citizen_cursor: usize,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl CityWorks {
    /// Builds a fresh city from `seed`: regenerates the ring, then assigns
    /// the first [`INITIAL_SIZE`] citizens to the best food tiles so the
    /// city opens with a sensible economy rather than twenty idle acres.
    fn generate(seed: u32) -> Self {
        let mut tiles = [Tile {
            terrain: Terrain::Grassland,
            food: 0,
            shields: 0,
            trade: 0,
        }; 20];
        for (i, &(dx, dy)) in RING_OFFSETS.iter().enumerate() {
            tiles[i] = generate_tile(seed, dx, dy);
        }

        // Rank tiles by food descending, index ascending as a tiebreak so
        // the initial assignment is deterministic rather than depending on
        // sort stability quirks.
        let mut order: Vec<usize> = (0..tiles.len()).collect();
        order.sort_by(|&a, &b| tiles[b].food.cmp(&tiles[a].food).then(a.cmp(&b)));

        let citizens = (0..INITIAL_SIZE)
            .map(|i| Citizen {
                assign: Assign::Worked(order[i]),
            })
            .collect();

        let mut completed = Log::new(IMPROVEMENT_LOG_CAP);
        // Short enough to survive truncation at the readout grid's minimum
        // panel width (20 interior columns) without clipping mid-word.
        completed.push("None built yet.", ui::DIM);

        Self {
            seed,
            tiles,
            citizens,
            granary: 0.0,
            shields_invested: 0.0,
            build_index: 0,
            completed,
            turn: 0,
            turn_timer: 0.0,
            time: 0.0,
            focus: Focus::Map,
            map_cursor: 0,
            citizen_cursor: 0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }

    fn reroll(&mut self) {
        let seed = self.seed.wrapping_add(0x9E37_79B9);
        *self = Self::generate(seed);
    }

    /// The city centre's own yield: always at least (2, 1, 1), the classic
    /// rule that a city never starves or stalls just from existing on bad
    /// terrain. Not part of `tiles`/`RING_OFFSETS` because it is never a
    /// citizen's choice to work or not work it.
    const fn center_yield() -> (i32, i32, i32) {
        (2, 1, 1)
    }

    /// Which citizen index (if any) is working ring tile `idx`.
    fn citizen_on_tile(&self, idx: usize) -> Option<usize> {
        self.citizens
            .iter()
            .position(|c| c.assign == Assign::Worked(idx))
    }

    /// The first citizen currently holding a specialist role, i.e. available
    /// to be sent to work an empty tile.
    fn first_specialist(&self) -> Option<usize> {
        self.citizens
            .iter()
            .position(|c| matches!(c.assign, Assign::Role(_)))
    }

    /// Every readout on screen, recomputed from the current assignment. See
    /// the [module docs](self) for why this being the single source of truth
    /// is the whole point of the demo.
    fn totals(&self) -> Totals {
        let (mut food, mut shields, mut trade) = Self::center_yield();
        let (mut lux_flat, mut sci_flat, mut tax_flat) = (0, 0, 0);

        for citizen in &self.citizens {
            match citizen.assign {
                Assign::Worked(idx) => {
                    let tile = self.tiles[idx];
                    food += tile.food;
                    shields += tile.shields;
                    trade += tile.trade;
                }
                Assign::Role(Role::Entertainer) => lux_flat += SPECIALIST_OUTPUT,
                Assign::Role(Role::Scientist) => sci_flat += SPECIALIST_OUTPUT,
                Assign::Role(Role::TaxCollector) => tax_flat += SPECIALIST_OUTPUT,
            }
        }

        let upkeep = self.citizens.len() as i32 * FOOD_PER_CITIZEN;
        let food_surplus = food - upkeep;

        let waste = (shields as f32 * WASTE_RATE).round() as i32;
        let shields_net = (shields - waste).max(0);

        let corruption = (trade as f32 * CORRUPTION_RATE).round() as i32;
        let trade_after_corruption = (trade - corruption).max(0);
        let tax = (trade_after_corruption as f32 * TAX_RATE).round() as i32 + tax_flat;
        let science = (trade_after_corruption as f32 * SCI_RATE).round() as i32 + sci_flat;
        // Luxury takes the remainder rather than its own rate, so the three
        // components always sum exactly to `trade_after_corruption` plus
        // their flat specialist bonuses -- no rounding leak to explain away.
        let base_tax = (trade_after_corruption as f32 * TAX_RATE).round() as i32;
        let base_sci = (trade_after_corruption as f32 * SCI_RATE).round() as i32;
        let luxury = (trade_after_corruption - base_tax - base_sci).max(0) + lux_flat;

        let pollution_pct = (100.0 * (shields - POLLUTION_THRESHOLD).max(0) as f32
            / POLLUTION_SCALE)
            .min(100.0) as i32;

        let size = self.citizens.len() as i32;
        let content_base = CONTENT_BASE.min(size);
        let unhappy_base = size - content_base;
        let to_content = (luxury / LUX_PER_CONTENT).min(unhappy_base);
        let leftover_luxury = luxury - to_content * LUX_PER_CONTENT;
        let content_after = content_base + to_content;
        let to_happy = (leftover_luxury / LUX_PER_HAPPY).min(content_after);
        let unhappy = unhappy_base - to_content;
        let content = content_after - to_happy;
        let happy = to_happy;

        Totals {
            food_gross: food,
            food_surplus,
            shields_gross: shields,
            shields_net,
            trade_gross: trade,
            corruption,
            tax,
            luxury,
            science,
            pollution_pct,
            happy,
            content,
            unhappy,
        }
    }

    /// Granary capacity at the current population. Grows with size so a
    /// bigger city takes proportionally longer to grow again, matching the
    /// source game's curve rather than a flat number that makes growth
    /// accelerate forever.
    fn granary_capacity(&self) -> f32 {
        GRANARY_PER_CITIZEN.mul_add(self.citizens.len() as f32, GRANARY_BASE)
    }

    /// Turns until the granary fills at the current surplus, or `None` if
    /// surplus is zero or negative (growth will never happen at this rate).
    fn turns_to_grow(&self, totals: &Totals) -> Option<i32> {
        if totals.food_surplus <= 0 {
            return None;
        }
        let remaining = (self.granary_capacity() - self.granary).max(0.0);
        Some((remaining / totals.food_surplus as f32).ceil() as i32)
    }

    const fn current_build(&self) -> &'static BuildItem {
        &CATALOG[self.build_index % CATALOG.len()]
    }

    /// Turns remaining on the current build at the current net shields, or
    /// `None` if production is zero (it will never finish at this rate).
    fn turns_to_build(&self, totals: &Totals) -> Option<i32> {
        if totals.shields_net <= 0 {
            return None;
        }
        let remaining = (self.current_build().cost as f32 - self.shields_invested).max(0.0);
        Some((remaining / totals.shields_net as f32).ceil() as i32)
    }

    /// Advances however many whole turns `dt` simulated seconds cover.
    /// Capped at a handful per call so a huge `dt` (e.g. a paused tab
    /// resuming) cannot spin through hundreds of turns in one frame; the
    /// timer simply keeps the remainder for next time.
    fn advance_turns(&mut self, dt: f32) {
        self.turn_timer += dt;
        let mut guard = 0;
        while self.turn_timer >= TURN_SECONDS && guard < 8 {
            self.turn_timer -= TURN_SECONDS;
            self.step_turn();
            guard += 1;
        }
    }

    fn step_turn(&mut self) {
        let totals = self.totals();
        self.granary = (self.granary + totals.food_surplus as f32).max(0.0);
        let cap = self.granary_capacity();
        if self.granary >= cap && self.citizens.len() < MAX_SIZE {
            self.granary -= cap;
            self.grow();
        }

        self.shields_invested += totals.shields_net as f32;
        let cost = self.current_build().cost as f32;
        if self.shields_invested >= cost {
            self.shields_invested -= cost;
            let built = self.current_build().name;
            self.completed.push(built, ui::ACCENT);
            self.build_index = (self.build_index + 1) % CATALOG.len();
        }

        self.turn += 1;
    }

    /// Adds one citizen. Sent to the best-food unworked tile if one exists,
    /// otherwise becomes an idle entertainer -- the same "assign somewhere
    /// sensible" rule the initial population uses in
    /// [`generate`](Self::generate).
    fn grow(&mut self) {
        let worked: Vec<bool> = (0..self.tiles.len())
            .map(|i| self.citizen_on_tile(i).is_some())
            .collect();
        let best = (0..self.tiles.len())
            .filter(|&i| !worked[i])
            .max_by_key(|&i| self.tiles[i].food);
        let assign = best.map_or(Assign::Role(Role::Entertainer), Assign::Worked);
        self.citizens.push(Citizen { assign });
    }

    /// Tapping/activating a ring tile: free its worker if it has one,
    /// otherwise pull the first available specialist onto it. A tile with no
    /// worker and no specialist available is a no-op -- every citizen is
    /// already doing something.
    fn tap_tile(&mut self, idx: usize) {
        if let Some(ci) = self.citizen_on_tile(idx) {
            self.citizens[ci].assign = Assign::Role(Role::Entertainer);
        } else if let Some(ci) = self.first_specialist() {
            self.citizens[ci].assign = Assign::Worked(idx);
        }
    }

    /// Tapping/activating a citizen: a tile-worker is freed to become an
    /// entertainer (the map tap is what sends them back to work); a
    /// specialist cycles to their next role.
    fn tap_citizen(&mut self, i: usize) {
        let Some(citizen) = self.citizens.get_mut(i) else {
            return;
        };
        citizen.assign = match citizen.assign {
            Assign::Worked(_) => Assign::Role(Role::Entertainer),
            Assign::Role(role) => Assign::Role(role.next()),
        };
    }

    /// Tapping/activating the build queue: switch to the next catalog item.
    /// Invested shields reset, matching the source game's penalty for
    /// changing what a city is building mid-project.
    const fn advance_build(&mut self) {
        self.build_index = (self.build_index + 1) % CATALOG.len();
        self.shields_invested = 0.0;
    }

    fn handle_gesture(&mut self, gesture: &Gesture) {
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            match action {
                Action::Tile(i) => self.tap_tile(i),
                Action::Citizen(i) => self.tap_citizen(i),
                Action::Build => self.advance_build(),
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_selection(-1),
            KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_selection(-5),
            KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_selection(5),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_selection(),
            KeyCode::Char('r' | 'R') => self.reroll(),
            _ => {}
        }
    }

    /// Moves whichever cursor the current focus owns by `delta`, wrapping.
    /// The map cursor steps through [`RING_OFFSETS`] in storage order rather
    /// than by screen geometry: it is not spatially exact, but it reaches
    /// every tile in a few presses, which is what keyboard parity requires.
    const fn move_selection(&mut self, delta: i32) {
        match self.focus {
            Focus::Map => {
                self.map_cursor = wrapping_add(self.map_cursor, delta, self.tiles.len());
            }
            Focus::Citizens => {
                if !self.citizens.is_empty() {
                    self.citizen_cursor =
                        wrapping_add(self.citizen_cursor, delta, self.citizens.len());
                }
            }
            Focus::Build => {
                if delta != 0 {
                    self.advance_build();
                }
            }
        }
    }

    fn activate_selection(&mut self) {
        match self.focus {
            Focus::Map => self.tap_tile(self.map_cursor),
            Focus::Citizens => self.tap_citizen(self.citizen_cursor),
            Focus::Build => self.advance_build(),
        }
    }

    fn status_line(&self) -> String {
        let totals = self.totals();
        format!(
            "turn {}  pop {}  food {:+}  shields {}  trade {}",
            self.turn,
            self.citizens.len(),
            totals.food_surplus,
            totals.shields_net,
            totals.trade_gross
        )
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    fn draw_header(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        panel::band(surface, area);
        let text = format!(
            "Rivenhall Falls -- Pop {}  Founded Turn 0  Turn {}",
            self.citizens.len(),
            self.turn
        );
        panel::spans(
            surface,
            (area.left() + 1, area.top()),
            area.width().saturating_sub(2),
            &[Span::keyword(&text)],
            ui::CHROME_BG,
        );
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title("City Radius")
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Map);
        let inner = panel.draw(surface, area);
        if inner.width() < 5 || inner.height() < 5 {
            return;
        }

        let cell_w = inner.width() / 5;
        let cell_h = inner.height() / 5;
        if cell_w == 0 || cell_h == 0 {
            return;
        }
        // Centre the 5x5 grid inside whatever room the panel has, rather
        // than pinning it to the top-left corner: on a wide desktop panel
        // this is the difference between the ring looking placed and
        // looking abandoned in a corner of a mostly black inset.
        let used_w = cell_w * 5;
        let used_h = cell_h * 5;
        let ox = inner.left() + (inner.width() - used_w) / 2;
        let oy = inner.top() + (inner.height() - used_h) / 2;

        for row in 0..5u16 {
            for col in 0..5u16 {
                let cell = Rect::new(ox + col * cell_w, oy + row * cell_h, cell_w, cell_h);
                if row == 2 && col == 2 {
                    Self::draw_center_cell(surface, cell);
                    continue;
                }
                let Some(idx) = RING_OFFSETS
                    .iter()
                    .position(|&(dx, dy)| ((dx + 2) as u16, (dy + 2) as u16) == (col, row))
                else {
                    continue; // one of the four excluded corners
                };
                self.draw_ring_cell(surface, cell, idx);
                if cell.width() >= 9 && cell.height() >= 4 {
                    self.hotspots.push(cell, Action::Tile(idx));
                } else {
                    self.hotspots.push_tappable(cell, inner, Action::Tile(idx));
                }
            }
        }
    }

    fn draw_center_cell(surface: &mut Surface<'_>, cell: Rect) {
        let (food, shields, trade) = Self::center_yield();
        let bg = rgb(46, 40, 20);
        surface.fill_rect(cell, ' ', Style::new().bg(bg));
        surface.print(
            (cell.left(), cell.top()),
            "CITY",
            Style::new().fg(ui::ACCENT).bg(bg),
        );
        if cell.height() > 1 {
            let yields = format!("{food}F{shields}S{trade}T");
            surface.print(
                (cell.left(), cell.top() + 1),
                &yields,
                Style::new().fg(ui::FG).bg(bg),
            );
        }
    }

    fn draw_ring_cell(&self, surface: &mut Surface<'_>, cell: Rect, idx: usize) {
        let tile = self.tiles[idx];
        let worked = self.citizen_on_tile(idx).is_some();
        let selected = self.focus == Focus::Map && self.map_cursor == idx;

        let bg = if selected {
            rgb(58, 50, 20)
        } else if worked {
            rgb(24, 34, 22)
        } else {
            rgb(14, 15, 20)
        };
        surface.fill_rect(cell, ' ', Style::new().bg(bg));

        let glyph_style = Style::new().fg(tile.terrain.color()).bg(bg);
        surface.put((cell.left(), cell.top()), tile.terrain.glyph(), glyph_style);
        let label_style = Style::new()
            .fg(if worked { ui::FG } else { ui::DIM })
            .bg(bg);
        let label_room = cell.width().saturating_sub(2) as usize;
        surface.print(
            (cell.left() + 2, cell.top()),
            retroglyph_widgets::truncate(tile.terrain.label(), label_room),
            label_style,
        );

        if cell.height() > 1 {
            let text = if worked {
                format!("{}F{}S{}T", tile.food, tile.shields, tile.trade)
            } else {
                "unworked".to_string()
            };
            surface.print(
                (cell.left(), cell.top() + 1),
                retroglyph_widgets::truncate(&text, cell.width() as usize),
                Style::new()
                    .fg(if worked { ui::ACCENT } else { ui::DIM })
                    .bg(bg),
            );
        }
        if selected && cell.height() > 2 {
            surface.print(
                (cell.left(), cell.top() + 2),
                "> tap to toggle",
                Style::new().fg(ui::ACCENT).bg(bg),
            );
        }
    }

    /// The mood of citizen `index`, from the same rank the totals function
    /// used to decide how many citizens overall are happy/content/unhappy:
    /// earliest indices are content-or-better, latest are unhappy-or-worse,
    /// which keeps the assignment stable frame to frame rather than
    /// reshuffling which specific citizen is unhappy every tick.
    const fn mood_of(index: usize, totals: &Totals) -> Mood {
        if (index as i32) < totals.happy {
            Mood::Happy
        } else if (index as i32) < totals.happy + totals.content {
            Mood::Content
        } else {
            Mood::Unhappy
        }
    }

    /// Rows the citizen grid needs to show every citizen, given a panel of
    /// `outer_width` columns (border included). Used to size the bottom band
    /// from actual content instead of a fixed guess, so the citizens panel
    /// never reserves rows it has nothing to draw into -- see the layout
    /// functions below.
    fn citizen_rows(&self, outer_width: u16) -> u16 {
        let inner_width = outer_width.saturating_sub(2);
        if inner_width < 9 || self.citizens.is_empty() {
            return 1;
        }
        let max_cols = (inner_width / 9).max(1);
        let cols = max_cols.min(self.citizens.len() as u16).max(1);
        (self.citizens.len() as u16).div_ceil(cols)
    }

    fn draw_citizens(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let unhappy = self.totals().unhappy;
        let badge = if unhappy > 0 {
            format!("{} ({unhappy}u)", self.citizens.len())
        } else {
            format!("{}", self.citizens.len())
        };
        let panel = Panel::new()
            .title("Citizens")
            .badge(&badge)
            .focused(self.focus == Focus::Citizens);
        let inner = panel.draw(surface, area);
        if inner.width() < 9 || inner.height() < 4 || self.citizens.is_empty() {
            return;
        }

        // Cap columns at the citizen count so a small population still
        // fills the row -- otherwise a wide desktop panel would leave the
        // unused columns as a large empty strip down its right side, one of
        // the defects the brief specifically calls out.
        let max_cols = (inner.width() / 9).max(1);
        let cols = max_cols.min(self.citizens.len() as u16).max(1);
        let rows_available = inner.height() / 4;
        let slots = (cols * rows_available) as usize;
        let shown = self.citizens.len().min(slots.max(1));

        let totals = self.totals();
        let cell_w = inner.width() / cols;

        for i in 0..shown {
            let col = (i as u16) % cols;
            let row = (i as u16) / cols;
            if row >= rows_available {
                break;
            }
            let cell = Rect::new(
                inner.left() + col * cell_w,
                inner.top() + row * 4,
                cell_w,
                4,
            );
            self.draw_citizen_card(surface, cell, i, &totals);
            self.hotspots.push_tappable(cell, inner, Action::Citizen(i));
        }

        let overflow = self.citizens.len().saturating_sub(shown);
        if overflow > 0 && rows_available > 0 {
            let y = inner.top() + (rows_available - 1) * 4;
            let text = format!("+{overflow} more citizens");
            surface.print(
                (inner.left(), y + 3),
                retroglyph_widgets::truncate(&text, inner.width() as usize),
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
        }
    }

    fn draw_citizen_card(
        &self,
        surface: &mut Surface<'_>,
        cell: Rect,
        index: usize,
        totals: &Totals,
    ) {
        let citizen = self.citizens[index];
        let mood = Self::mood_of(index, totals);
        let selected = self.focus == Focus::Citizens && self.citizen_cursor == index;

        let card = Panel::new()
            .frame(mood.color())
            .bg(if selected {
                rgb(40, 36, 18)
            } else {
                panel::PANEL_BG
            })
            .focused(selected);
        let inner = card.draw(surface, cell);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        let (label, sub) = match citizen.assign {
            Assign::Worked(idx) => {
                let tile = self.tiles[idx];
                (
                    format!("T{idx:02} {}", mood.glyph()),
                    format!("{}F{}S{}T", tile.food, tile.shields, tile.trade),
                )
            }
            Assign::Role(role) => (
                format!("{} {} {}", role.glyph(), role.label(), mood.glyph()),
                format!("{} +{SPECIALIST_OUTPUT}", mood.glyph()),
            ),
        };
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(&label, inner.width() as usize),
            Style::new().fg(ui::FG).bg(card_bg(selected)),
        );
        if inner.height() > 1 {
            let color = match citizen.assign {
                Assign::Worked(_) => ui::ACCENT,
                Assign::Role(role) => role.color(),
            };
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(&sub, inner.width() as usize),
                Style::new().fg(color).bg(card_bg(selected)),
            );
        }
    }

    fn draw_build(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title("Build Queue")
            .focused(self.focus == Focus::Build);
        let inner = panel.draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }
        self.hotspots.push_tappable(inner, area, Action::Build);

        let totals = self.totals();
        let build = self.current_build();
        let progress = (self.shields_invested / build.cost as f32).clamp(0.0, 1.0);
        let turns = self
            .turns_to_build(&totals)
            .map_or_else(|| "--".to_string(), |t| t.to_string());

        let turns_text = format!("  {turns}t");
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::keyword(build.name), Span::dim(&turns_text)],
            panel::PANEL_BG,
        );

        if inner.height() > 1 {
            let bar_w = inner.width().saturating_sub(1);
            panel::bar(
                surface,
                (inner.left(), inner.top() + 1),
                bar_w,
                progress,
                ui::ACCENT,
                rgb(30, 30, 36),
            );
        }
        if inner.height() > 2 {
            let cost_line = format!("{}/{} shields", self.shields_invested as i32, build.cost);
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[Span::dim(&cost_line)],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 3 {
            let next = &CATALOG[(self.build_index + 1) % CATALOG.len()];
            let text = format!("next: {}", next.name);
            panel::spans(
                surface,
                (inner.left(), inner.top() + 3),
                inner.width(),
                &[Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
    }

    /// The six readout panels, as (title, lines) pairs computed from the
    /// current totals. A plain data list rather than drawing directly, so
    /// [`draw_readouts`](Self::draw_readouts) can decide how many actually
    /// fit before it starts drawing any of them.
    fn readout_panels(&self, totals: &Totals) -> [(&'static str, Vec<String>); 6] {
        let bar_w = 10u16;
        let granary_frac = (self.granary / self.granary_capacity()).clamp(0.0, 1.0);
        let grow_line = self
            .turns_to_grow(totals)
            .map_or_else(|| "grows: never".to_string(), |t| format!("grows in {t}t"));

        [
            (
                "Food",
                vec![
                    format!("gross {} food", totals.food_gross),
                    format!("surplus {:+}/turn", totals.food_surplus),
                    format!("granary {:.0}%", granary_frac * 100.0),
                    grow_line,
                ],
            ),
            (
                "Production",
                vec![
                    format!("gross {} shields", totals.shields_gross),
                    format!("waste {}", totals.shields_gross - totals.shields_net),
                    format!("net {}/turn", totals.shields_net),
                ],
            ),
            (
                "Trade",
                vec![
                    format!("tax {}", totals.tax),
                    format!("lux {}", totals.luxury),
                    format!("sci {}", totals.science),
                ],
            ),
            (
                "Pollution",
                vec![
                    format!("{}%", totals.pollution_pct),
                    "-".repeat(
                        (bar_w as usize * totals.pollution_pct as usize / 100).min(bar_w as usize),
                    ),
                ],
            ),
            (
                "Corruption",
                vec![
                    format!("-{} trade", totals.corruption),
                    format!("of {} gross", totals.trade_gross),
                ],
            ),
            ("Improvements", Vec::new()),
        ]
    }

    /// Draws the readout panels into `area`, choosing between two layouts:
    /// a grid, when every panel fits at [`READOUT_MIN_W`]x[`READOUT_MIN_H`]
    /// or larger; otherwise a single page of however many do fit, cycling
    /// through the rest on [`PAGE_SECONDS`]. This is what keeps a phone
    /// sidebar from either shrinking six panels below legibility or hiding
    /// five of them permanently -- see the module docs.
    fn draw_readouts(&self, surface: &mut Surface<'_>, area: Rect) {
        const READOUT_MIN_W: u16 = 22;
        const READOUT_MIN_H: u16 = 6;
        if area.width() < READOUT_MIN_W || area.height() < READOUT_MIN_H {
            return;
        }

        let totals = self.totals();
        let panels = self.readout_panels(&totals);

        let cols = (area.width() / READOUT_MIN_W).clamp(1, 2);
        let rows_fit = (area.height() / READOUT_MIN_H).max(1);
        let slots = (cols * rows_fit) as usize;

        let (start, count) = if slots >= panels.len() {
            (0, panels.len())
        } else {
            let pages = panels.len().div_ceil(slots);
            let page = ((self.time / PAGE_SECONDS) as usize) % pages.max(1);
            let start = page * slots;
            (start, slots.min(panels.len() - start))
        };

        let rows = count.div_ceil(cols as usize) as u16;
        let cell_w = area.width() / cols;
        // Panels never grow past what their content needs (border plus up to
        // four lines): stretching them to fill a tall sidebar would leave
        // each one mostly blank interior, exactly the "large empty panel"
        // defect the brief calls out. The leftover height is spent instead
        // as quiet margin above and below the grid.
        let cell_h = (area.height() / rows.max(1)).min(READOUT_MIN_H + 2);
        let grid_h = cell_h * rows;
        let oy = area.top() + (area.height() - grid_h) / 2;

        for (slot, panel_idx) in (start..start + count).enumerate() {
            let col = (slot as u16) % cols;
            let row = (slot as u16) / cols;
            let cell = Rect::new(
                area.left() + col * cell_w,
                oy + row * cell_h,
                cell_w,
                cell_h,
            );
            self.draw_readout_panel(surface, cell, panels[panel_idx].0, &panels[panel_idx].1);
        }
    }

    fn draw_readout_panel(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        title: &str,
        lines: &[String],
    ) {
        let inner = Panel::new().title(title).draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        if title == "Improvements" {
            self.completed.draw(surface, inner, panel::PANEL_BG);
            return;
        }
        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height() {
                break;
            }
            surface.print(
                (inner.left(), inner.top() + i as u16),
                retroglyph_widgets::truncate(line, inner.width() as usize),
                Style::new().fg(ui::FG).bg(panel::PANEL_BG),
            );
        }
    }

    // ── Layout ───────────────────────────────────────────────────────────

    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(content);

        let (header, rest) = panel::split_top(content, 1);
        self.draw_header(surface, header);
        if rest.height() == 0 {
            return;
        }

        if shape.stacks() {
            self.layout_portrait(surface, rest);
        } else {
            self.layout_wide(surface, rest);
        }
    }

    /// Wide layouts (landscape phone and desktop): a bottom band for the
    /// citizen strip and build queue (the thumb zone), a right sidebar for
    /// the six readouts, and the map filling whatever is left -- which is
    /// most of the rect, since the sidebar and bottom band are both budgeted
    /// against the live rect rather than a fixed design size. See the
    /// "fill the viewport" rule in the round-3 brief addendum.
    fn layout_wide(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let build_w = 30u16.min(area.width() / 3).max(1);
        let citizens_w = area.width().saturating_sub(build_w);
        let rows = self.citizen_rows(citizens_w);
        // Build's own four lines (name, bar, cost, next) plus its border is
        // the other claim on the bottom band's height; the band takes
        // whichever of the two needs is larger.
        let bottom_h = (rows * 4 + 2).max(6).min(area.height() / 2);
        let (mid, bottom) = panel::split_bottom(area, bottom_h);

        let sidebar_w = (mid.width() * 3 / 10).clamp(20, 44).min(mid.width());
        let (map_area, sidebar) = panel::split_right(mid, sidebar_w);
        self.draw_map(surface, map_area);
        self.draw_readouts(surface, sidebar);

        let (citizens_area, build_area) = panel::split_right(bottom, build_w);
        self.draw_citizens(surface, citizens_area);
        self.draw_build(surface, build_area);
    }

    /// Portrait (tall phone): everything stacks. The map claims a fixed
    /// share of the middle band and the readouts claim the rest, so a very
    /// tall screen (lots of spare rows) gives proportionally more room to
    /// both rather than leaving either one pinned to a minimum while empty
    /// space grows below it.
    fn layout_portrait(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let rows = self.citizen_rows(area.width());
        let citizens_h = (rows * 4 + 2).max(6);
        let build_h = 6u16;
        let bottom_h = (citizens_h + build_h).min(area.height() / 2);
        let (mid, bottom) = panel::split_bottom(area, bottom_h);

        let map_h = (mid.height() * 9 / 20).max(20).min(mid.height());
        let (map_area, readout_area) = panel::split_top(mid, map_h);
        self.draw_map(surface, map_area);
        self.draw_readouts(surface, readout_area);

        let citizens_split = citizens_h.min(bottom.height());
        let (citizens_area, build_area) = panel::split_top(bottom, citizens_split);
        self.draw_citizens(surface, citizens_area);
        self.draw_build(surface, build_area);
    }
}

/// Adds `delta` to `value` modulo `len`, treating both as a ring rather than
/// clamping -- so repeatedly pressing Left from index 0 on a 20-tile ring
/// wraps to 19 instead of sticking, which is what makes the map cursor
/// reachable from any starting cell with a handful of keystrokes.
const fn wrapping_add(value: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i32;
    let v = value as i32;
    (((v + delta) % len + len) % len) as usize
}

const fn card_bg(selected: bool) -> Color {
    if selected {
        rgb(40, 36, 18)
    } else {
        panel::PANEL_BG
    }
}

impl Default for CityWorks {
    fn default() -> Self {
        Self::generate(1)
    }
}

impl Demo for CityWorks {
    const NAME: &'static str = "59_city_works";
    const TITLE: &'static str = "City Works";
    const BLURB: &'static str =
        "Civilization II: small readouts ringing one large city view and a build queue.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "cycle focus"),
            ("arrows", "move selection"),
            ("Enter/Space", "toggle tile / cycle role / build"),
            ("R", "reroll city"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        self.advance_turns(dt);

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

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.layout_and_draw(&mut surface, content);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Assign, CityWorks, RING_OFFSETS, Role};

    #[test]
    fn ring_offsets_cover_the_fat_cross_exactly() {
        assert_eq!(RING_OFFSETS.len(), 20);
        for &(dx, dy) in &RING_OFFSETS {
            assert!((-2..=2).contains(&dx) && (-2..=2).contains(&dy));
            assert!(!(dx.abs() == 2 && dy.abs() == 2), "corner not excluded");
            assert!((dx, dy) != (0, 0), "centre is not part of the ring");
        }
        let mut seen = RING_OFFSETS.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 20, "offsets must be unique");
    }

    #[test]
    fn unassigning_a_worked_tile_changes_food_or_production() {
        let mut demo = CityWorks::default();
        let before = demo.totals();
        let idx = demo
            .citizens
            .iter()
            .position(|c| matches!(c.assign, Assign::Worked(_)))
            .expect("the default city has at least one worked tile");
        demo.citizens[idx].assign = Assign::Role(Role::Entertainer);
        let after = demo.totals();
        assert!(
            before.food_surplus != after.food_surplus
                || before.shields_net != after.shields_net
                || before.trade_gross != after.trade_gross,
            "unassigning a worked tile must change at least one total"
        );
    }

    #[test]
    fn tapping_an_unworked_tile_pulls_a_specialist_onto_it() {
        let mut demo = CityWorks::default();
        // Free one citizen back to being a specialist first, so there is
        // someone available to reassign.
        demo.citizens[0].assign = Assign::Role(Role::Entertainer);
        let target = (0..demo.tiles.len())
            .find(|&i| demo.citizen_on_tile(i).is_none())
            .expect("some tile is unworked after freeing a citizen");
        demo.tap_tile(target);
        assert_eq!(demo.citizen_on_tile(target), Some(0));
    }

    #[test]
    fn tapping_a_specialist_cycles_through_every_role() {
        let mut demo = CityWorks::default();
        demo.citizens[0].assign = Assign::Role(Role::Entertainer);
        demo.tap_citizen(0);
        assert_eq!(demo.citizens[0].assign, Assign::Role(Role::Scientist));
        demo.tap_citizen(0);
        assert_eq!(demo.citizens[0].assign, Assign::Role(Role::TaxCollector));
        demo.tap_citizen(0);
        assert_eq!(demo.citizens[0].assign, Assign::Role(Role::Entertainer));
    }

    #[test]
    fn turns_to_grow_is_none_rather_than_dividing_by_zero_at_zero_surplus() {
        let mut demo = CityWorks::default();
        // Move every citizen to a specialist role: food surplus should drop
        // to (city centre food) minus upkeep, which for enough citizens
        // goes negative or zero.
        for citizen in &mut demo.citizens {
            citizen.assign = Assign::Role(Role::Entertainer);
        }
        let totals = demo.totals();
        if totals.food_surplus <= 0 {
            assert!(demo.turns_to_grow(&totals).is_none());
        }
    }

    #[test]
    fn advancing_many_turns_in_one_call_is_bounded() {
        let mut demo = CityWorks::default();
        // A huge delta must not hang or overflow; the turn counter should
        // advance by at most the internal guard, not by however many whole
        // TURN_SECONDS fit in the delta.
        demo.advance_turns(10_000.0);
        assert!(demo.turn <= 8);
    }
}

ascii_tile_demos::demo_main!(CityWorks);
