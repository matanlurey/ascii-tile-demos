//! 44: Dusk field -- Battle for Wesnoth's sidebar, and terrain defence as the
//! reason it exists.
//!
//! Every other hex demo in this gallery treats the map as the whole of the
//! interface. Wesnoth does not: its signature is a persistent vertical
//! sidebar that never goes away, stacked minimap-over-status-over-unit, and
//! the entire reason it is that dense is that combat in Wesnoth depends on a
//! number the map itself cannot show -- how much a unit's own terrain
//! protects it. A forest tile looks the same whether the archer standing in
//! it dodges half of everything or a third of everything; the only place
//! that number lives is the sidebar, so the sidebar has to carry it. This
//! demo reproduces that whole stack -- minimap, resource strip, hex info,
//! time-of-day, unit card, End Turn -- faithfully, and builds the map and its
//! mechanics around making that stack worth having.
//!
//! [`38_hex_general`](../38_hex_general) is the Fantasy General demo in this
//! same batch and owns the pre-attack odds *forecast* (friendly losses versus
//! enemy losses, shown on hover before a strike is committed). This demo does
//! not attack at all: Wesnoth's information model is reactive, not
//! predictive -- tap a hex, learn what it is; tap a unit, learn what it has --
//! and the two demos are deliberately not the same shape.
//!
//! Techniques on show:
//!
//! - **Per-cell hex ownership** ([`DuskField::draw_board`]), the same
//!   technique [`19_hex_command`](../19_hex_command) uses: every screen cell
//!   asks [`tilekit::geom::HexLayout::cell_to_tile`] which hex it belongs to,
//!   rather than a taper formula trying to get all six edges right at once.
//! - **Terrain defence as the sidebar's reason to exist** ([`Terrain::defence`],
//!   [`DuskField::draw_hex_info`]): the percentage is never drawn on the map
//!   itself, only in the panel, and only once a hex is tapped -- there is no
//!   hover on a touch device, so tapping is the *only* way to learn it, and
//!   the whole layout is built to make that one tap answer the question.
//! - **Zone of control** ([`DuskField::reachable`]): a relaxation over the
//!   tiny board (not a priority queue -- forty-two hexes make one unnecessary)
//!   that refuses to expand past any hex adjacent to an enemy, so the
//!   highlighted reachable set itself *is* the `ZoC` preview; there is nothing
//!   further to draw.
//! - **A continuous day/night dial driving a discrete combat rule**
//!   ([`DuskField::daylight`], [`TimeCategory`], [`alignment_modifier`]): the
//!   ambient tint and the sidebar's sun/moon dial sweep continuously so the
//!   demo never sits still, but the *schedule phase* that actually changes a
//!   lawful or chaotic unit's modifier only steps forward on End Turn --
//!   matching the round 2 rule that names and numbers must stay pinned to the
//!   grid rather than tweening.
//! - **Village capture and income** ([`DuskField::activate_tile`]): entering
//!   an unheld or enemy-held village flips its owner and its flag on the
//!   spot; income is recomputed from villages held the next time a side's
//!   turn begins, and both gold and income are pinned in the sidebar's
//!   resource strip.
//! - **Tap-select-then-tap-target movement** via [`ui::touch::Pointer`]: a
//!   friendly unit is selected by tapping it, its reachable hexes light up,
//!   and a second tap on a lit hex moves it -- the two-tap path the touch
//!   guidance recommends for a dense board where a dragged token would sit
//!   under the player's own finger.
//!
//! ```sh
//! cargo run --example 44_dusk_field --features crossterm
//! cargo run --example 44_dusk_field --features software
//! cargo run --example 44_dusk_field --features gl
//! cargo run --example 44_dusk_field  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, HexLayout, HexOrientation, Tile};
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb, scale};

/// Board width in hexes.
///
/// Small on purpose: Wesnoth's own scenarios run to dozens of hexes across,
/// but this demo has to survive at 80x24 with a third of its width spent on
/// the sidebar, so the board is sized to *fit whole* at the smallest shape
/// this gallery supports rather than to be panned. A board that must be
/// panned to be understood would compete with the sidebar for "what am I
/// looking at", and the sidebar is the point.
const COLS: i32 = 7;
/// See [`COLS`].
const ROWS: i32 = 6;

/// The hex layout every hex is drawn and picked on: pointy-top on an 8x4 cell
/// pitch. Small enough that the full 7x6 board (about 60x26 cells) fits
/// inside a landscape phone's content area after the sidebar is subtracted;
/// large enough that a unit token, its flag, and its health bar all have
/// their own cell inside one hex rather than overlapping.
const LAYOUT: HexLayout = HexLayout::new(HexOrientation::Pointy, 8, 4);

/// Turns the schedule counts toward, purely cosmetic (the cycle repeats
/// regardless); shown as "Turn N/MAX" the way a Wesnoth scenario shows a turn
/// limit.
const MAX_TURNS: u32 = 30;

/// How many world-seconds one full day/night sweep takes for the *ambient*
/// tint and sidebar dial (see [`DuskField::daylight`]). This is deliberately
/// decoupled from the schedule phase that actually changes combat: the dial
/// is decoration and is allowed to glide, but the phase name and the
/// alignment modifier are numbers a player reads and acts on, and round 2's
/// own lesson is that those must step, not tween.
const DAY_CYCLE_SECONDS: f32 = 40.0;

// ── Terrain ──────────────────────────────────────────────────────────────

/// One hex's terrain. Eight distinct kinds so the map reads as varied at a
/// glance, which matters here specifically because the sidebar's headline
/// number -- defence percentage -- is meaningless if every hex looks (and
/// defends) the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Terrain {
    Grass,
    Forest,
    Hills,
    Mountain,
    ShallowWater,
    Road,
    Village,
    Keep,
}

impl Terrain {
    const fn from_char(c: char) -> Self {
        match c {
            'F' => Self::Forest,
            'H' => Self::Hills,
            'M' => Self::Mountain,
            'W' => Self::ShallowWater,
            'R' => Self::Road,
            'V' => Self::Village,
            'K' => Self::Keep,
            _ => Self::Grass,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Grass => "Grassland",
            Self::Forest => "Forest",
            Self::Hills => "Hills",
            Self::Mountain => "Mountain",
            Self::ShallowWater => "Shallow Water",
            Self::Road => "Road",
            Self::Village => "Village",
            Self::Keep => "Castle Keep",
        }
    }

    /// Base fill color, before the day/night wash [`DuskField::tint`] applies.
    const fn base_color(self) -> Color {
        match self {
            Self::Grass => rgb(96, 128, 68),
            Self::Forest => rgb(44, 84, 50),
            Self::Hills => rgb(120, 104, 70),
            Self::Mountain => rgb(104, 96, 92),
            Self::ShallowWater => rgb(58, 96, 138),
            Self::Road => rgb(126, 108, 78),
            Self::Village => rgb(140, 114, 72),
            Self::Keep => rgb(90, 90, 100),
        }
    }

    /// Glyph drawn at each hex's own centre cell, under any unit or flag
    /// overlay. One glyph per hex rather than a scattered texture (the
    /// technique [`19_hex_command`](../19_hex_command) uses): these hexes are
    /// half that demo's size, and a scatter would not read at this scale.
    const fn glyph(self) -> char {
        match self {
            Self::Grass => '.',
            Self::Forest => '\u{2663}', // suit club, reads as a tree crown
            Self::Hills => '\u{2229}',  // intersection, a rounded rise
            Self::Mountain => '\u{25B2}',
            Self::ShallowWater => '\u{2248}',
            Self::Road => '-',
            Self::Village => '\u{25CB}', // small circle, a hut roof
            Self::Keep => '\u{25A0}',
        }
    }

    /// Base terrain defence percentage: how much of the time an attack against
    /// a unit standing here simply misses. This is the number the sidebar
    /// exists to surface -- see the module docs.
    const fn base_defence(self) -> i32 {
        match self {
            Self::Grass => 30,
            Self::Road | Self::ShallowWater => 20,
            Self::Forest | Self::Hills => 50,
            Self::Mountain | Self::Village | Self::Keep => 60,
        }
    }

    /// Movement points to enter this hex.
    const fn move_cost(self) -> i32 {
        match self {
            Self::Grass | Self::Road | Self::Village | Self::Keep => 1,
            Self::Forest | Self::Hills | Self::ShallowWater => 2,
            Self::Mountain => 3,
        }
    }
}

/// A hand-authored scenario rather than a noise field: at 42 hexes total,
/// noise would as likely produce an unreadable scramble as a legible little
/// battlefield, and legibility (a road linking two keeps, water and forest
/// as a natural midfield obstacle, two villages worth contesting) is the
/// entire point of the terrain layer here. One character per hex, row-major,
/// matching [`COLS`] characters per row.
const MAP: [&str; ROWS as usize] = [
    "KRGFFHM", "GRFFHHM", "GVRWHMM", "MMHWRVG", "MHHFRFG", "MFFGRGK",
];

const fn tile_index(tile: Tile) -> Option<usize> {
    if tile.col < 0 || tile.row < 0 || tile.col >= COLS || tile.row >= ROWS {
        return None;
    }
    Some((tile.row * COLS + tile.col) as usize)
}

// ── Units ────────────────────────────────────────────────────────────────

/// Which army a unit or village belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Player,
    Enemy,
}

impl Side {
    const fn color(self) -> Color {
        match self {
            Self::Player => rgb(94, 140, 214),
            Self::Enemy => rgb(206, 90, 78),
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Player => Self::Enemy,
            Self::Enemy => Self::Player,
        }
    }
}

/// A unit's ancestry, which is what [`Terrain::base_defence`] gets adjusted
/// by: the same forest hex is not equally safe for every unit type, which is
/// the nuance that makes the defence number worth checking per-unit rather
/// than reading once off the terrain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Race {
    Human,
    Elf,
    /// Gets a defence bonus on hills and mountains, per `defence_bonus`. No
    /// unit in this scenario's small roster is a dwarf, but the rule is kept
    /// (rather than deleted along with the variant) because the whole point
    /// of `defence_on` being per-race is that swapping a unit's race changes
    /// its number on the same hex, and a roster of one race per bonus would
    /// hide that this is a general rule, not a special case wired to Elves.
    #[allow(dead_code)]
    Dwarf,
    Orc,
    Bat,
}

impl Race {
    const fn name(self) -> &'static str {
        match self {
            Self::Human => "Human",
            Self::Elf => "Elf",
            Self::Dwarf => "Dwarf",
            Self::Orc => "Orc",
            Self::Bat => "Bat",
        }
    }

    /// Adjustment to add to [`Terrain::base_defence`] for this race standing
    /// on `terrain`, clamped by the caller to a legal percentage.
    const fn defence_bonus(self, terrain: Terrain) -> i32 {
        match (self, terrain) {
            (Self::Dwarf, Terrain::Hills | Terrain::Mountain) => 15,
            (Self::Elf, Terrain::Forest) | (Self::Bat, Terrain::Mountain) => 10,
            _ => 0,
        }
    }
}

/// How a unit's combat strength responds to the time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Alignment {
    Lawful,
    Neutral,
    Chaotic,
    /// Peaks at dawn/dusk, weaker the rest of the day and night. No unit in
    /// this scenario's small roster has it, but `alignment_modifier`
    /// implements the rule anyway since it is one match arm, not a feature.
    #[allow(dead_code)]
    Liminal,
}

impl Alignment {
    const fn name(self) -> &'static str {
        match self {
            Self::Lawful => "Lawful",
            Self::Neutral => "Neutral",
            Self::Chaotic => "Chaotic",
            Self::Liminal => "Liminal",
        }
    }
}

/// Coarse bucket a schedule phase falls into, which is all
/// [`alignment_modifier`] actually needs to know.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TimeCategory {
    Day,
    Night,
    Dim,
}

/// The combat percentage `alignment` gets under `category`, Wesnoth's own
/// rule: lawful units hit harder by day and weaker by night, chaotic units
/// the mirror image, liminal units peak at dawn/dusk and suffer the rest of
/// the day, neutral units are indifferent throughout.
const fn alignment_modifier(alignment: Alignment, category: TimeCategory) -> i32 {
    match (alignment, category) {
        (Alignment::Lawful, TimeCategory::Day)
        | (Alignment::Chaotic, TimeCategory::Night)
        | (Alignment::Liminal, TimeCategory::Dim) => 25,
        (Alignment::Lawful, TimeCategory::Night)
        | (Alignment::Chaotic, TimeCategory::Day)
        | (Alignment::Liminal, _) => -25,
        (Alignment::Lawful | Alignment::Chaotic, TimeCategory::Dim) | (Alignment::Neutral, _) => 0,
    }
}

/// One weapon a unit carries, shown in the sidebar as Wesnoth's own
/// damage-by-strikes notation: `5x2 mace, melee-impact` reads as "5 damage,
/// 2 strikes, a mace, dealing impact damage".
struct Attack {
    name: &'static str,
    damage: i32,
    strikes: i32,
    kind: &'static str,
}

impl Attack {
    fn notation(&self) -> String {
        format!(
            "{}x{} {}, {}",
            self.damage, self.strikes, self.name, self.kind
        )
    }
}

/// A unit standing on the board.
struct Unit {
    name: &'static str,
    kind: &'static str,
    race: Race,
    alignment: Alignment,
    side: Side,
    tile: Tile,
    hp: i32,
    hp_max: i32,
    xp: i32,
    xp_max: i32,
    mp: i32,
    mp_max: i32,
    traits: &'static [&'static str],
    attacks: &'static [Attack],
}

impl Unit {
    /// Defence percentage this unit gets standing on `terrain`: the terrain's
    /// own base value plus this unit's race bonus, clamped to a legal
    /// percentage. The number [`Terrain::base_defence`]'s doc comment says
    /// the sidebar exists to surface.
    fn defence_on(&self, terrain: Terrain) -> i32 {
        (terrain.base_defence() + self.race.defence_bonus(terrain)).clamp(0, 70)
    }
}

/// A capturable settlement: entering it (by either side) flips ownership and
/// its flag.
struct Village {
    tile: Tile,
    owner: Option<Side>,
}

// ── Tap targets ──────────────────────────────────────────────────────────

/// What a registered hotspot means. The board itself is not registered
/// through [`Hotspots`] -- a hex's true footprint is not a rectangle, so its
/// tap resolution goes through [`HexLayout::cell_to_tile`] directly, the same
/// query [`19_hex_command`](../19_hex_command) trusts for picking. Hotspots
/// carries the one genuinely rectangular control this demo has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    EndTurn,
}

// ── State ────────────────────────────────────────────────────────────────

/// Demo state: board, unit roster, turn/economy tracking, and input.
pub struct DuskField {
    terrain: Vec<Terrain>,
    villages: Vec<Village>,
    units: Vec<Unit>,
    turn: u32,
    /// Index into a fixed six-entry schedule; see [`Self::phase`].
    phase: usize,
    active_side: Side,
    gold: i32,
    income: i32,
    selected_unit: Option<usize>,
    reachable: Vec<Tile>,
    /// Last hex a tap or the keyboard cursor landed on, for the hex-info
    /// panel. Distinct from `selected_unit`: tapping empty terrain updates
    /// this without disturbing (or requiring) a unit selection.
    inspected: Tile,
    cursor: Tile,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

/// The six-entry time-of-day schedule: name, sun/moon glyph, and category.
/// A fixed array rather than a generated cycle because a schedule *is* fixed
/// data in Wesnoth (an author writes it once per scenario); this is one
/// scenario's worth.
const SCHEDULE: [(&str, char, TimeCategory); 6] = [
    ("Dawn", '\u{263C}', TimeCategory::Dim),
    ("Morning", '\u{263C}', TimeCategory::Day),
    ("Afternoon", '\u{263C}', TimeCategory::Day),
    ("Dusk", '\u{263C}', TimeCategory::Dim),
    ("First Watch", '\u{25CB}', TimeCategory::Night),
    ("Second Watch", '\u{25CB}', TimeCategory::Night),
];

impl Default for DuskField {
    // The unit roster below is scenario data, not logic: six units with a
    // handful of fields each reads as one long literal no matter how it is
    // split, and splitting it across helper functions would just move the
    // line count without making any of it easier to follow.
    #[allow(clippy::too_many_lines)]
    fn default() -> Self {
        let terrain = MAP
            .iter()
            .flat_map(|row| row.chars())
            .map(Terrain::from_char)
            .collect();

        let villages = vec![
            Village {
                tile: Tile::new(1, 2),
                owner: None,
            },
            Village {
                tile: Tile::new(5, 3),
                owner: None,
            },
        ];

        let units = vec![
            Unit {
                name: "Konrad",
                kind: "Human Fighter",
                race: Race::Human,
                alignment: Alignment::Lawful,
                side: Side::Player,
                tile: Tile::new(0, 1),
                hp: 32,
                hp_max: 32,
                xp: 12,
                xp_max: 32,
                mp: 5,
                mp_max: 5,
                traits: &["strong", "resilient"],
                attacks: &[Attack {
                    name: "sword",
                    damage: 8,
                    strikes: 4,
                    kind: "melee-blade",
                }],
            },
            Unit {
                name: "Eryssa",
                kind: "Elvish Archer",
                race: Race::Elf,
                alignment: Alignment::Neutral,
                side: Side::Player,
                tile: Tile::new(2, 1),
                hp: 24,
                hp_max: 24,
                xp: 4,
                xp_max: 32,
                mp: 5,
                mp_max: 5,
                traits: &["quick"],
                attacks: &[
                    Attack {
                        name: "short sword",
                        damage: 5,
                        strikes: 3,
                        kind: "melee-blade",
                    },
                    Attack {
                        name: "bow",
                        damage: 5,
                        strikes: 4,
                        kind: "ranged-pierce",
                    },
                ],
            },
            Unit {
                name: "Delfador",
                kind: "Human Mage",
                race: Race::Human,
                alignment: Alignment::Neutral,
                side: Side::Player,
                tile: Tile::new(2, 0),
                hp: 20,
                hp_max: 20,
                xp: 30,
                xp_max: 32,
                mp: 4,
                mp_max: 4,
                traits: &["intelligent"],
                attacks: &[
                    Attack {
                        name: "staff",
                        damage: 5,
                        strikes: 2,
                        kind: "melee-impact",
                    },
                    Attack {
                        name: "missile",
                        damage: 4,
                        strikes: 3,
                        kind: "ranged-arcane",
                    },
                ],
            },
            Unit {
                name: "Grukk",
                kind: "Orcish Grunt",
                race: Race::Orc,
                alignment: Alignment::Chaotic,
                side: Side::Enemy,
                tile: Tile::new(6, 4),
                hp: 38,
                hp_max: 38,
                xp: 0,
                xp_max: 32,
                mp: 5,
                mp_max: 5,
                traits: &["strong"],
                attacks: &[Attack {
                    name: "mace",
                    damage: 5,
                    strikes: 2,
                    kind: "melee-impact",
                }],
            },
            Unit {
                name: "Vrask",
                kind: "Vampire Bat",
                race: Race::Bat,
                alignment: Alignment::Chaotic,
                side: Side::Enemy,
                tile: Tile::new(4, 3),
                hp: 18,
                hp_max: 18,
                xp: 8,
                xp_max: 24,
                mp: 8,
                mp_max: 8,
                traits: &["quick"],
                attacks: &[Attack {
                    name: "fangs",
                    damage: 4,
                    strikes: 2,
                    kind: "melee-blade",
                }],
            },
            Unit {
                name: "Karg",
                kind: "Orcish Archer",
                race: Race::Orc,
                alignment: Alignment::Chaotic,
                side: Side::Enemy,
                tile: Tile::new(5, 5),
                hp: 22,
                hp_max: 22,
                xp: 2,
                xp_max: 32,
                mp: 5,
                mp_max: 5,
                traits: &["resilient"],
                attacks: &[
                    Attack {
                        name: "crude knife",
                        damage: 3,
                        strikes: 2,
                        kind: "melee-blade",
                    },
                    Attack {
                        name: "bow",
                        damage: 4,
                        strikes: 3,
                        kind: "ranged-pierce",
                    },
                ],
            },
        ];

        Self {
            terrain,
            villages,
            units,
            turn: 1,
            phase: 0,
            active_side: Side::Player,
            gold: 60,
            income: 2,
            selected_unit: None,
            reachable: Vec::new(),
            inspected: Tile::new(0, 1),
            cursor: Tile::new(0, 1),
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl DuskField {
    fn terrain_at(&self, tile: Tile) -> Terrain {
        tile_index(tile).map_or(Terrain::Grass, |i| self.terrain[i])
    }

    fn unit_at(&self, tile: Tile) -> Option<usize> {
        self.units.iter().position(|u| u.tile == tile)
    }

    fn village_at(&self, tile: Tile) -> Option<usize> {
        self.villages.iter().position(|v| v.tile == tile)
    }

    /// Whether `tile` is inside `side`'s enemy's zone of control: adjacent to
    /// at least one living enemy unit. A unit that steps into a `ZoC` hex has
    /// spent its whole turn doing so -- see [`Self::reachable`].
    fn in_enemy_zoc(&self, tile: Tile, side: Side) -> bool {
        LAYOUT
            .neighbors(tile)
            .into_iter()
            .any(|n| self.unit_at(n).is_some_and(|i| self.units[i].side != side))
    }

    /// Every hex `unit_idx` can reach this turn, zone of control included.
    ///
    /// A relaxation over the whole board rather than a priority-queue
    /// Dijkstra: the board is 42 hexes, so the asymptotic difference is
    /// noise, and a flat "keep relaxing until nothing improves" pass is far
    /// less code to get right than a heap-based shortest path, for a board
    /// this is never going to be asked to scale past.
    ///
    /// The `ZoC` rule is the one line that makes this Wesnoth's movement
    /// instead of generic pathfinding: a tile inside an enemy zone of control
    /// (other than the unit's own starting tile) never expands to its
    /// neighbours, so remaining movement past it is simply unreachable. That
    /// is the entire rule, and it needs no separate "preview" overlay: the
    /// reachable set already stops exactly where Wesnoth's would.
    fn reachable(&self, unit_idx: usize) -> Vec<Tile> {
        let unit = &self.units[unit_idx];
        let n = (COLS * ROWS) as usize;
        let mut best = vec![-1i32; n];
        let Some(start) = tile_index(unit.tile) else {
            return Vec::new();
        };
        best[start] = unit.mp;

        let mut changed = true;
        while changed {
            changed = false;
            for row in 0..ROWS {
                for col in 0..COLS {
                    let tile = Tile::new(col, row);
                    let Some(i) = tile_index(tile) else { continue };
                    if best[i] <= 0 {
                        continue;
                    }
                    if tile != unit.tile && self.in_enemy_zoc(tile, unit.side) {
                        continue;
                    }
                    for neighbor in LAYOUT.neighbors(tile) {
                        let Some(ni) = tile_index(neighbor) else {
                            continue;
                        };
                        if self.unit_at(neighbor).is_some() {
                            continue; // no stacking, no passing through
                        }
                        let cost = self.terrain[ni].move_cost();
                        let left = best[i] - cost;
                        if left >= 0 && left > best[ni] {
                            best[ni] = left;
                            changed = true;
                        }
                    }
                }
            }
        }

        (0..n)
            .filter(|&i| i != start && best[i] >= 0)
            .map(|i| Tile::new(i as i32 % COLS, i as i32 / COLS))
            .collect()
    }

    /// Handles a tap or Enter-key activation of `tile`: select the unit
    /// there, move the selected unit onto it if it is a reachable hex, or
    /// just record it as the inspected hex for the terrain panel.
    fn activate_tile(&mut self, tile: Tile) {
        self.inspected = tile;
        self.cursor = tile;

        if let Some(idx) = self.unit_at(tile) {
            self.selected_unit = Some(idx);
            self.reachable = if self.units[idx].side == self.active_side && self.units[idx].mp > 0 {
                self.reachable(idx)
            } else {
                Vec::new()
            };
            return;
        }

        if let Some(idx) = self.selected_unit
            && self.units[idx].side == self.active_side
            && self.reachable.contains(&tile)
        {
            let cost_paid = self.units[idx].mp - self.remaining_mp_at(idx, tile);
            self.units[idx].mp -= cost_paid.max(0);
            self.units[idx].tile = tile;
            if let Some(v) = self.village_at(tile) {
                self.villages[v].owner = Some(self.active_side);
            }
            self.reachable = if self.units[idx].mp > 0 {
                self.reachable(idx)
            } else {
                Vec::new()
            };
            return;
        }

        self.selected_unit = None;
        self.reachable.clear();
    }

    /// Movement points `unit_idx` would have left after arriving at `tile`,
    /// recomputed from [`Self::reachable`]'s own relaxation rather than
    /// stored, since the reachable set is the only place that number is
    /// already known to be correct.
    fn remaining_mp_at(&self, unit_idx: usize, tile: Tile) -> i32 {
        let unit = &self.units[unit_idx];
        let n = (COLS * ROWS) as usize;
        let mut best = vec![-1i32; n];
        let Some(start) = tile_index(unit.tile) else {
            return 0;
        };
        best[start] = unit.mp;
        let mut changed = true;
        while changed {
            changed = false;
            for row in 0..ROWS {
                for col in 0..COLS {
                    let t = Tile::new(col, row);
                    let Some(i) = tile_index(t) else { continue };
                    if best[i] <= 0 || (t != unit.tile && self.in_enemy_zoc(t, unit.side)) {
                        continue;
                    }
                    for neighbor in LAYOUT.neighbors(t) {
                        let Some(ni) = tile_index(neighbor) else {
                            continue;
                        };
                        if self.unit_at(neighbor).is_some() {
                            continue;
                        }
                        let cost = self.terrain[ni].move_cost();
                        let left = best[i] - cost;
                        if left >= 0 && left > best[ni] {
                            best[ni] = left;
                            changed = true;
                        }
                    }
                }
            }
        }
        tile_index(tile).map_or(0, |i| best[i].max(0))
    }

    /// Ends the active side's turn: advances the schedule, refills the side
    /// that becomes active, and -- if that side is the player -- collects
    /// income from held villages.
    fn end_turn(&mut self) {
        self.phase = (self.phase + 1) % SCHEDULE.len();
        if self.phase == 0 {
            self.turn = (self.turn % MAX_TURNS) + 1;
        }
        self.active_side = self.active_side.other();
        for unit in &mut self.units {
            if unit.side == self.active_side {
                unit.mp = unit.mp_max;
            }
        }
        if self.active_side == Side::Player {
            self.income = 1 + self
                .villages
                .iter()
                .filter(|v| v.owner == Some(Side::Player))
                .count() as i32;
            self.gold += self.income;
        }
        self.selected_unit = None;
        self.reachable.clear();
    }

    /// Continuous day/night level in `-1.0` (deepest night) to `1.0`
    /// (brightest noon), used only for the ambient tint and the sidebar's
    /// sweeping dial. See [`DAY_CYCLE_SECONDS`] for why this runs on its own
    /// clock rather than the discrete schedule phase.
    fn daylight(&self) -> f32 {
        (self.time / DAY_CYCLE_SECONDS * core::f32::consts::TAU).sin()
    }

    /// Mixes `color` toward a warm day wash or a cool night wash by the
    /// current [`Self::daylight`] level, applied to every hex and to the
    /// minimap alike so the two never disagree about what time it looks like.
    fn tint(&self, color: Color) -> Color {
        let d = self.daylight();
        if d >= 0.0 {
            mix(color, rgb(255, 214, 140), d * 0.22)
        } else {
            mix(color, rgb(18, 22, 48), -d * 0.42)
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = event
                && key.is_down()
            {
                let (mut dc, mut dr) = (0, 0);
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => dr = -1,
                    KeyCode::Down | KeyCode::Char('s' | 'S') => dr = 1,
                    KeyCode::Left | KeyCode::Char('a' | 'A') => dc = -1,
                    KeyCode::Right | KeyCode::Char('d' | 'D') => dc = 1,
                    KeyCode::Enter => {
                        let cursor = self.cursor;
                        self.activate_tile(cursor);
                    }
                    KeyCode::Char('e' | 'E') => self.end_turn(),
                    _ => {}
                }
                if dc != 0 || dr != 0 {
                    let next = Tile::new(self.cursor.col + dc, self.cursor.row + dr);
                    if tile_index(next).is_some() {
                        self.cursor = next;
                    }
                }
            }
        }
        true
    }

    /// The world-cell origin that centres the board inside `area`.
    fn map_origin(area: Rect) -> Cell {
        let top_left = LAYOUT.tile_to_cell(Tile::new(0, 0));
        let far = LAYOUT.tile_to_cell(Tile::new(COLS - 1, ROWS - 1));
        let map_w = far.x + LAYOUT.pitch_x - top_left.x;
        let map_h = far.y + LAYOUT.pitch_y - top_left.y;
        Cell::new(
            top_left.x - (i32::from(area.width()) - map_w) / 2,
            top_left.y - (i32::from(area.height()) - map_h) / 2,
        )
    }

    fn to_screen(area: Rect, origin: Cell, wx: i32, wy: i32) -> Option<(u16, u16)> {
        let (dx, dy) = (wx - origin.x, wy - origin.y);
        if dx < 0 || dy < 0 || dx >= i32::from(area.width()) || dy >= i32::from(area.height()) {
            return None;
        }
        Some((area.left() + dx as u16, area.top() + dy as u16))
    }

    /// Draws the board: per-cell hex ownership and fill, then unit and
    /// village overlays on top.
    fn draw_board(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 2 || area.height() < 2 {
            return;
        }
        let origin = Self::map_origin(area);

        for sy in area.top()..area.bottom() {
            for sx in area.left()..area.right() {
                let wx = origin.x + i32::from(sx - area.left());
                let wy = origin.y + i32::from(sy - area.top());
                let tile = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                let Some(_) = tile_index(tile) else { continue };
                self.paint_cell(surface, (sx, sy), tile, wx, wy);
            }
        }

        for i in 0..self.villages.len() {
            self.draw_flag(surface, area, origin, i);
        }
        for i in 0..self.units.len() {
            self.draw_unit(surface, area, origin, i);
        }
        self.draw_cursor(surface, area, origin);
    }

    fn paint_cell(
        &self,
        surface: &mut Surface<'_>,
        (sx, sy): (u16, u16),
        tile: Tile,
        wx: i32,
        wy: i32,
    ) {
        let terrain = self.terrain_at(tile);
        let mut bg = self.tint(terrain.base_color());

        let is_reachable = self.reachable.contains(&tile);
        let is_selected = self
            .selected_unit
            .is_some_and(|i| self.units[i].tile == tile);
        let in_zoc = self
            .selected_unit
            .is_some_and(|i| self.in_enemy_zoc(tile, self.units[i].side));
        if is_selected {
            bg = mix(bg, rgb(255, 246, 200), 0.35);
        } else if is_reachable {
            // Reachable hexes still inside an enemy zone of control get a
            // ruddier highlight than the rest of the reachable set: this is
            // the "shown when previewing a move" requirement for zone of
            // control, done by tinting the boundary the movement relaxation
            // already stopped at rather than drawing a separate overlay.
            bg = mix(
                bg,
                if in_zoc {
                    rgb(214, 110, 90)
                } else {
                    rgb(140, 200, 240)
                },
                0.3,
            );
        }

        let center = LAYOUT.center_cell(tile);
        let north = LAYOUT.cell_to_tile(Cell::new(wx, wy - 1));
        let west = LAYOUT.cell_to_tile(Cell::new(wx - 1, wy));
        if (north != tile && tile_index(north).is_some())
            || (west != tile && tile_index(west).is_some())
        {
            surface.put((sx, sy), '\u{00b7}', Style::new().fg(scale(bg, 0.5)).bg(bg));
            return;
        }

        let glyph = if wx == center.x && wy == center.y {
            terrain.glyph()
        } else {
            ' '
        };
        // Shallow water shimmers: a per-cell hashed phase drifting under
        // elapsed time, mixed into brightness only (never the glyph), so the
        // ripple reads as light on moving water rather than as a flicker on
        // top of decorative art -- the same continuous-color, stable-glyph
        // split [`21_deck_plan`](../21_deck_plan)'s starfield uses.
        let fg = if terrain == Terrain::ShallowWater {
            let phase = hash01(0x5741, wx, wy) * core::f32::consts::TAU;
            let glint = 0.5f32.mul_add((self.time.mul_add(1.1, phase)).sin(), 0.5);
            mix(bg, rgb(220, 236, 250), 0.15 + glint * 0.35)
        } else {
            mix(bg, rgb(240, 240, 236), 0.4)
        };
        surface.put((sx, sy), glyph, Style::new().fg(fg).bg(bg));
    }

    /// Draws a village's flag: a pole plus a banner that alternates between
    /// two triangle glyphs on a coarse timer, the fluttering Wesnoth's own
    /// village flags animate with. Undefended villages show no flag, since a
    /// flag is a claim and an unheld village has not been claimed by anyone.
    fn draw_flag(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell, index: usize) {
        let village = &self.villages[index];
        let Some(owner) = village.owner else { return };
        let center = LAYOUT.center_cell(village.tile);
        let Some((sx, sy)) = Self::to_screen(area, origin, center.x - 1, center.y - 1) else {
            return;
        };
        let bg = self.tint(Terrain::Village.base_color());
        let flutter = self
            .time
            .mul_add(1.6, hash01(0x1357, village.tile.col, village.tile.row))
            .fract()
            < 0.5;
        let banner = if flutter { '\u{25BA}' } else { '\u{25C4}' };
        surface.put((sx, sy), '|', Style::new().fg(rgb(200, 196, 190)).bg(bg));
        if sx + 1 < area.right() {
            surface.put((sx + 1, sy), banner, Style::new().fg(owner.color()).bg(bg));
        }
    }

    fn draw_unit(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell, index: usize) {
        let unit = &self.units[index];
        let center = LAYOUT.center_cell(unit.tile);
        let base_bg = self.tint(self.terrain_at(unit.tile).base_color());
        let selected = self.selected_unit == Some(index);
        let base = mix(
            scale(unit.side.color(), 0.5),
            base_bg,
            if selected { 0.1 } else { 0.35 },
        );

        // The colored base ellipse: a 3-wide band at the unit's own row, so
        // the token reads as standing on a plinth rather than floating on
        // the terrain glyph.
        for dx in -1..=1i32 {
            if let Some((sx, sy)) = Self::to_screen(area, origin, center.x + dx, center.y) {
                surface.put((sx, sy), ' ', Style::new().bg(base));
            }
        }
        if let Some((sx, sy)) = Self::to_screen(area, origin, center.x, center.y) {
            let initial = unit.name.chars().next().unwrap_or('?');
            surface.put((sx, sy), initial, Style::new().fg(rgb(12, 12, 16)).bg(base));
        }
        // Health bar directly beneath, half-cell precision via the shared
        // gauge widget so a wounded unit is legible without reading a number.
        if let Some((sx, sy)) = Self::to_screen(area, origin, center.x - 1, center.y + 1) {
            let t = unit.hp as f32 / unit.hp_max.max(1) as f32;
            panel::bar(
                surface,
                (sx, sy),
                3,
                t,
                panel::threshold(t),
                scale(base_bg, 0.6),
            );
        }
    }

    fn draw_cursor(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell) {
        let center = LAYOUT.center_cell(self.cursor);
        let bg = self.tint(self.terrain_at(self.cursor).base_color());
        let style = Style::new().fg(rgb(255, 246, 200)).bg(bg);
        if let Some((sx, sy)) = Self::to_screen(area, origin, center.x - 2, center.y) {
            surface.put((sx, sy), '[', style);
        }
        if let Some((sx, sy)) = Self::to_screen(area, origin, center.x + 2, center.y) {
            surface.put((sx, sy), ']', style);
        }
    }

    // ── Sidebar ──────────────────────────────────────────────────────────

    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Map").draw(surface, area);
        if inner.width() < COLS as u16 || inner.height() < ROWS as u16 {
            return;
        }
        for row in 0..ROWS {
            for col in 0..COLS {
                let terrain = self.terrain[(row * COLS + col) as usize];
                let color = self.tint(terrain.base_color());
                // '\u{25CF}' (a solid disc) is not in CP437 and would render
                // as a colorless block on the pixel backends, so the player
                // marker uses the open bullet `\u{2022}` instead; paired with
                // `x` for the enemy, the two stay easy to tell apart.
                let glyph = self.unit_at(Tile::new(col, row)).map_or(' ', |i| {
                    if self.units[i].side == Side::Player {
                        '\u{2022}'
                    } else {
                        'x'
                    }
                });
                surface.put(
                    (inner.left() + col as u16, inner.top() + row as u16),
                    glyph,
                    Style::new().fg(rgb(20, 20, 24)).bg(color),
                );
            }
        }
    }

    /// The icon row: Wesnoth's own is a strip of small tool icons; this
    /// scenario has no menu to put there, so the strip carries the resource
    /// figures the brief asks the sidebar to show instead -- gold, income,
    /// and villages held, each a short badge rather than a full panel of its
    /// own, in keeping with how compressed this one row is meant to be.
    fn draw_resources(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let held = self
            .villages
            .iter()
            .filter(|v| v.owner == Some(Side::Player))
            .count();
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::keyword(&format!("{}g", self.gold)),
                Span::plain("  "),
                Span::new(&format!("+{}/turn", self.income), rgb(140, 200, 140)),
                Span::plain("  "),
                Span::dim(&format!("{held}/{} villages", self.villages.len())),
            ],
            panel::PANEL_BG,
        );
    }

    fn draw_hex_info(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Hex")
            .badge(&tile_label(self.inspected))
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let terrain = self.terrain_at(self.inspected);
        let mut y = inner.top();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::keyword(terrain.name())],
            panel::PANEL_BG,
        );
        y += 1;
        if y < inner.bottom() {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(&format!("Move cost: {}", terrain.move_cost()))],
                panel::PANEL_BG,
            );
            y += 1;
        }
        if y < inner.bottom() {
            // This is the line the whole demo is built to make legible: the
            // selected unit's defence on the hex that was actually tapped,
            // which is the only place in Wesnoth's own interface this number
            // ever appears.
            let text = self.selected_unit.map_or_else(
                || "Defence: -- (select a unit)".to_string(),
                |i| format!("Defence: {}%", self.units[i].defence_on(terrain)),
            );
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::plain(&text)],
                panel::PANEL_BG,
            );
        }
    }

    const fn phase(&self) -> (&'static str, char, TimeCategory) {
        SCHEDULE[self.phase]
    }

    fn draw_time(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Time").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let (name, icon, _) = self.phase();
        let icon_color = if self.daylight() >= 0.0 {
            rgb(250, 214, 120)
        } else {
            rgb(150, 170, 220)
        };
        surface.put(
            (inner.left(), inner.top()),
            icon,
            Style::new().fg(icon_color).bg(panel::PANEL_BG),
        );
        panel::spans(
            surface,
            (inner.left() + 2, inner.top()),
            inner.width().saturating_sub(2),
            &[Span::plain(name)],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::dim(&format!("Turn {}/{MAX_TURNS}", self.turn))],
                panel::PANEL_BG,
            );
        }
        // A continuously sweeping dial: a marker sliding across a fixed
        // track by [`Self::daylight`], decoration rather than a number, so it
        // is allowed to glide where the phase name above it is not.
        if inner.height() > 3 && inner.width() > 6 {
            let track_y = inner.top() + 3;
            let w = inner.width();
            for x in 0..w {
                surface.put(
                    (inner.left() + x, track_y),
                    '\u{2500}',
                    Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
                );
            }
            let t = self.daylight().mul_add(0.5, 0.5);
            let mx = ((f32::from(w) - 1.0) * t).round() as u16;
            surface.put(
                (inner.left() + mx, track_y),
                if self.daylight() >= 0.0 {
                    '\u{263C}'
                } else {
                    '\u{25CB}'
                },
                Style::new().fg(icon_color).bg(panel::PANEL_BG),
            );
        }
    }

    /// Draws a small framed multi-line "portrait": a boxed initial in the
    /// unit's side color, standing in for the illustrated bust Wesnoth's own
    /// sidebar shows. See the round 2 addendum on finding a structural
    /// equivalent for illustration rather than attempting to fake it.
    fn draw_portrait(surface: &mut Surface<'_>, at: (u16, u16), unit: &Unit) {
        let (x, y) = at;
        let color = unit.side.color();
        let style = Style::new().fg(color).bg(panel::PANEL_BG);
        surface.print((x, y), "\u{250C}\u{2500}\u{2500}\u{2500}\u{2510}", style);
        surface.put((x, y + 1), '\u{2502}', style);
        surface.put(
            (x + 2, y + 1),
            unit.name.chars().next().unwrap_or('?'),
            style,
        );
        surface.put((x + 4, y + 1), '\u{2502}', style);
        surface.print(
            (x, y + 2),
            "\u{2514}\u{2500}\u{2500}\u{2500}\u{2518}",
            style,
        );
    }

    /// The dense unit card: portrait, HP/XP/MP, defence on its own hex, name,
    /// type, alignment (with the live combat modifier), race, traits, and
    /// (if there is room) its attack list. `show_attacks` is what
    /// [`Self::draw_sidebar`] turns off first under a squeezed shape, per
    /// the brief's own ordering.
    // The unit card draws nine independent fields (portrait, name, kind,
    // alignment, three bars, defence/race, traits) each behind its own
    // "is there room" check; that guard-per-field shape is what the touch
    // guidance's graceful-degradation requirement actually looks like in
    // code, and breaking it into sub-functions would scatter the one thing
    // worth reading in one place: the order fields drop in as space runs out.
    #[allow(clippy::too_many_lines)]
    fn draw_unit_panel(&self, surface: &mut Surface<'_>, area: Rect, show_attacks: bool) {
        let inner = panel::Panel::new()
            .title("Unit")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let Some(idx) = self.selected_unit else {
            if inner.height() > 1 {
                panel::spans(
                    surface,
                    (inner.left(), inner.top()),
                    inner.width(),
                    &[Span::dim("Tap a unit to inspect it.")],
                    panel::PANEL_BG,
                );
            }
            return;
        };
        let unit = &self.units[idx];
        let has_portrait = inner.width() >= 20 && inner.height() >= 4;
        let text_x = if has_portrait {
            Self::draw_portrait(surface, (inner.left(), inner.top()), unit);
            inner.left() + 6
        } else {
            inner.left()
        };
        let text_w = inner.right().saturating_sub(text_x);

        let mut y = inner.top();
        panel::spans(
            surface,
            (text_x, y),
            text_w,
            &[Span::keyword(unit.name)],
            panel::PANEL_BG,
        );
        y += 1;
        if y < inner.bottom() {
            panel::spans(
                surface,
                (text_x, y),
                text_w,
                &[Span::plain(unit.kind)],
                panel::PANEL_BG,
            );
            y += 1;
        }
        let (_, _, category) = self.phase();
        let modifier = alignment_modifier(unit.alignment, category);
        if y < inner.bottom() {
            let (sign, modifier_color) = match modifier.cmp(&0) {
                core::cmp::Ordering::Greater => ("+", rgb(140, 210, 140)),
                core::cmp::Ordering::Less => ("", rgb(214, 120, 110)),
                core::cmp::Ordering::Equal => ("\u{b1}", ui::DIM),
            };
            panel::spans(
                surface,
                (text_x, y),
                text_w,
                &[
                    Span::plain(unit.alignment.name()),
                    Span::plain(" "),
                    Span::new(&format!("({sign}{modifier}%)"), modifier_color),
                ],
                panel::PANEL_BG,
            );
            y += 1;
        }

        y = y.max(inner.top() + 3); // clear the portrait block before text below it
        if y < inner.bottom() {
            let t = unit.hp as f32 / unit.hp_max.max(1) as f32;
            draw_stat(
                surface,
                (inner.left(), y),
                inner.width(),
                "HP",
                unit.hp,
                unit.hp_max,
                panel::threshold(t),
            );
            y += 1;
        }
        if y < inner.bottom() {
            draw_stat(
                surface,
                (inner.left(), y),
                inner.width(),
                "XP",
                unit.xp,
                unit.xp_max,
                rgb(150, 170, 220),
            );
            y += 1;
        }
        if y < inner.bottom() {
            draw_stat(
                surface,
                (inner.left(), y),
                inner.width(),
                "MP",
                unit.mp,
                unit.mp_max,
                rgb(200, 190, 120),
            );
            y += 1;
        }
        if y < inner.bottom() {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    Span::dim("Def here: "),
                    Span::keyword(&format!("{}%", unit.defence_on(self.terrain_at(unit.tile)))),
                    Span::plain("  "),
                    Span::dim(unit.race.name()),
                ],
                panel::PANEL_BG,
            );
            y += 1;
        }
        if y < inner.bottom() && !unit.traits.is_empty() {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(&format!("Traits: {}", unit.traits.join(", ")))],
                panel::PANEL_BG,
            );
            y += 1;
        }
        if show_attacks {
            for attack in unit.attacks {
                if y >= inner.bottom() {
                    break;
                }
                panel::spans(
                    surface,
                    (inner.left(), y),
                    inner.width(),
                    &[Span::plain(&attack.notation())],
                    panel::PANEL_BG,
                );
                y += 1;
            }
        }
    }

    /// The End Turn button, pinned to the bottom of `area` (the thumb zone on
    /// a phone) and grown to at least [`ui::touch::TAP_W`] x
    /// [`ui::touch::TAP_H`] so it stays hittable at any panel width.
    fn draw_end_turn(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let grown = ui::touch::tappable(area, area);
        self.hotspots.push_tappable(grown, area, Action::EndTurn);
        let side_color = self.active_side.color();
        surface.fill_rect(area, ' ', Style::new().bg(scale(side_color, 0.35)));
        let label = format!("End Turn ({:?})", self.active_side);
        let x = area.left() + (area.width().saturating_sub(label.len() as u16)) / 2;
        let y = area.top() + area.height() / 2;
        surface.print(
            (x.max(area.left()), y),
            &label,
            Style::new().fg(ui::FG).bg(scale(side_color, 0.35)),
        );
    }

    /// Lays out and draws the whole sidebar, budgeting height smallest-first
    /// (minimap and resources are cheap and fixed, End Turn is pinned, and
    /// whatever remains splits between hex info, time, and the unit card),
    /// dropping the attack list first when the unit card cannot fit it --
    /// the ordering the brief calls out explicitly.
    fn draw_sidebar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let minimap_h = (ROWS as u16 + 2).min(area.height());
        let resources_h = 3u16.min(area.height().saturating_sub(minimap_h));
        let end_turn_h =
            ui::touch::TAP_H.min(area.height().saturating_sub(minimap_h + resources_h));
        let hex_h = 5u16;
        let time_h = 5u16;

        let (minimap_area, rest) = panel::split_top(area, minimap_h);
        let (resources_area, rest) = panel::split_top(rest, resources_h);
        let (rest, end_turn_area) = panel::split_bottom(rest, end_turn_h);
        let (hex_area, rest) = panel::split_top(rest, hex_h.min(rest.height()));
        let (time_area, unit_area) = panel::split_top(rest, time_h.min(rest.height()));

        self.draw_minimap(surface, minimap_area);
        self.draw_resources(surface, resources_area);
        self.draw_hex_info(surface, hex_area);
        self.draw_time(surface, time_area);
        // Attacks need at least 8 rows of unit card (portrait, three bars,
        // defence, traits, plus one attack line) to show anything; below
        // that they are the first thing this panel drops.
        let show_attacks = unit_area.height() >= 8;
        self.draw_unit_panel(surface, unit_area, show_attacks);
        self.draw_end_turn(surface, end_turn_area);
    }

    /// Portrait layout: sidebar sections move under the map as two dense
    /// columns (minimap/resources/hex-info on the left, time/unit on the
    /// right), with End Turn as a full-width band at the very bottom, inside
    /// a phone's thumb reach.
    fn draw_sidebar_portrait(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let end_turn_h = ui::touch::TAP_H.min(area.height());
        let (rest, end_turn_area) = panel::split_bottom(area, end_turn_h);
        let cols = panel::columns(rest, 2, 0);
        let (left, right) = (cols[0], cols[1]);

        let minimap_h = (ROWS as u16 + 2).min(left.height());
        let (minimap_area, left_rest) = panel::split_top(left, minimap_h);
        let resources_h = 3u16.min(left_rest.height());
        let (resources_area, left_rest) = panel::split_top(left_rest, resources_h);
        self.draw_minimap(surface, minimap_area);
        self.draw_resources(surface, resources_area);
        self.draw_hex_info(surface, left_rest);

        let time_h = 5u16.min(right.height());
        let (time_area, unit_area) = panel::split_top(right, time_h);
        self.draw_time(surface, time_area);
        let show_attacks = unit_area.height() >= 8;
        self.draw_unit_panel(surface, unit_area, show_attacks);

        self.draw_end_turn(surface, end_turn_area);
    }

    fn status(&self) -> String {
        format!(
            "{:?}'s turn  {} ({:?})  {} units",
            self.active_side,
            self.phase().0,
            self.phase().2,
            self.units.len()
        )
    }
}

/// Draws a labelled `current/max` gauge on one row: a two-letter tag, a
/// short bar, and the numeric readout, which stays pinned text even while
/// the bar beside it is a smooth-looking fill (the fill itself is still
/// half-cell stepped, per [`panel::bar`]; nothing here tweens).
fn draw_stat(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    label: &str,
    value: i32,
    max: i32,
    color: Color,
) {
    let (x, y) = at;
    surface.print((x, y), label, Style::new().fg(ui::DIM).bg(panel::PANEL_BG));
    let bar_x = x + 3;
    let text = format!(" {value}/{max}");
    let bar_w = width.saturating_sub(3 + text.len() as u16).max(3);
    let t = value as f32 / max.max(1) as f32;
    panel::bar(surface, (bar_x, y), bar_w, t, color, scale(color, 0.25));
    surface.print(
        (bar_x + bar_w, y),
        &text,
        Style::new().fg(ui::FG).bg(panel::PANEL_BG),
    );
}

/// `A1`-style label for a tile: row letter, column number (1-based), matching
/// the coordinate convention the other hex demos in this gallery use.
fn tile_label(tile: Tile) -> String {
    let letter = (b'A' + (tile.row.rem_euclid(26)) as u8) as char;
    format!("{letter}{}", tile.col + 1)
}

impl Demo for DuskField {
    const NAME: &'static str = "44_dusk_field";
    const TITLE: &'static str = "44 Dusk Field";
    const BLURB: &'static str =
        "Wesnoth sidebar: minimap, terrain defence percent, day/night swing.";
    const GRID: (u16, u16) = (150, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("arrows", "move cursor"),
            ("Enter", "select/move/inspect"),
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

        let (title, content, status) = ui::split_chrome(term.area());
        let shape = Shape::of(content);

        // Hotspots are rebuilt fresh every frame during layout, per
        // `ui::touch`'s own contract: a control not drawn this frame cannot
        // be tapped this frame.
        self.hotspots.clear();

        let (map_area, sidebar_area, portrait) = match shape {
            Shape::Portrait => {
                let map_h = content.height() * 5 / 9;
                let (map_area, sidebar_area) = panel::split_top(content, map_h);
                (map_area, sidebar_area, true)
            }
            Shape::Landscape => {
                let side_w = 30u16.min(content.width().saturating_sub(40));
                let (map_area, sidebar_area) = panel::split_right(content, side_w);
                (map_area, sidebar_area, false)
            }
            Shape::Desktop => {
                let side_w = 38u16.min(content.width().saturating_sub(50));
                let (map_area, sidebar_area) = panel::split_right(content, side_w);
                (map_area, sidebar_area, false)
            }
        };

        // Resolve this frame's tap, if any, against the End Turn hotspot
        // first and the hex board second: the button is real screen-space
        // rectangle, so `Hotspots` owns it; a hex is not a rectangle, so its
        // tap goes through the same `cell_to_tile` query the board itself is
        // painted with, which is what keeps a tap resolving to the same hex
        // a viewer would say they tapped.
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap {
            if self.hotspots.hit(pos) == Some(&Action::EndTurn) {
                self.end_turn();
            } else if map_area.contains_pos(pos) {
                let origin = Self::map_origin(map_area);
                let wx = origin.x + i32::from(pos.x - map_area.left());
                let wy = origin.y + i32::from(pos.y - map_area.top());
                let tapped = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                if tile_index(tapped).is_some() {
                    self.activate_tile(tapped);
                }
            }
        }

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_board(&mut surface, map_area);
        if sidebar_area.width() > 0 || portrait {
            if portrait {
                self.draw_sidebar_portrait(&mut surface, sidebar_area);
            } else if sidebar_area.width() > 0 {
                self.draw_sidebar(&mut surface, sidebar_area);
            }
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(DuskField);
