//! 38: Hex General -- the projected-losses attack preview, framed like a 1996
//! wargame.
//!
//! Every other hex demo in this gallery ([`07_hex_tiles`](../07_hex_tiles),
//! [`08_hex_outline`](../08_hex_outline), [`09_hex_subcell`](../09_hex_subcell),
//! [`19_hex_command`](../19_hex_command), [`26_hexcrawl`](../26_hexcrawl), and
//! this batch's own [`44_dusk_field`](../44_dusk_field)) treats the
//! hex grid itself as the point. Fantasy General's tactical screen is not
//! about the grid -- it is about the two panels underneath it. Select your
//! unit, tap an enemy in range, and the bottom of the screen fills with a
//! forecast: what you will lose, what they will lose, in kills and wounds,
//! *before* you commit to anything. That prediction is the one thing this
//! genre's other member, Battle for Wesnoth, deliberately does not do (Wesnoth
//! shows a reactive terrain-defense percentage on hover instead), which is why
//! 44 gets the sidebar-and-minimap treatment and this demo gets two big
//! comparison panels and an ornate frame instead.
//!
//! The forecast is the centre of the interface, not a corner of it: the
//! attacker's card sits bottom-left, the defender's sits bottom-right, and the
//! projected exchange runs across the strip between them. On a touch device
//! there is no hover to reveal a forecast on approach, so the two-tap
//! contract is load-bearing: the *first* tap on an enemy in range fills the
//! forecast, and only a *second* tap on the same enemy commits the attack. A
//! tap anywhere else clears it. This is a shrunk two-tap version of the
//! original's mouse-hover-then-click flow, and it is the only faithful way to
//! port that flow to a device with no hover at all.
//!
//! Techniques on show:
//!
//! - **A projected combat forecast built from real unit state**
//!   ([`compute_forecast`]): both sides' expected kills and wounds are a pure
//!   function of current strength, experience, and the defender's terrain --
//!   not a canned number -- so damaging a unit in one exchange visibly changes
//!   the odds of the next.
//! - **Wounds and kills as separate, asymmetric pools** ([`Unit::rest`],
//!   [`Unit::apply_losses`]): wounds subtract from current strength and heal
//!   on rest; kills subtract from *maximum* strength and never do, which is
//!   the detail the manual singles out as the game's core attrition rule.
//! - **Experience as filled shield pips** ([`Unit::pips`]): a unit that has
//!   won survives to a colored fraction of a five-pip bar, carried across the
//!   whole session rather than reset by any one fight.
//! - **Ornate framed chrome** ([`HexGeneral::draw_frame`]): a heavy
//!   double-line border, icon rails down both edges built from
//!   [`tilekit::glyphs`] terrain markers, and a banner carrying gold, region,
//!   and the selected hex's coordinate -- reproducing the one visual habit
//!   that makes a Fantasy General screenshot recognizable at a glance, box
//!   drawing and shading only.
//! - **Multi-cell unit tokens with a strength badge**
//!   ([`HexGeneral::draw_units`]): each token is a class glyph over a colored
//!   strength badge, with a fluttering pennant above it driven by
//!   `frame.delta` on a per-unit phase so the field never looks frozen even
//!   with nothing selected.
//! - **Tap-select-then-tap-target, never hover-only**
//!   ([`HexGeneral::handle_tap`]): built on [`ui::touch::Pointer`] and
//!   [`ui::touch::Hotspots`], per the touch module's own guidance for dense
//!   boards where a dragged target would sit under the player's own finger.
//!
//! ```sh
//! cargo run --example 38_hex_general --features crossterm
//! cargo run --example 38_hex_general --features software
//! cargo run --example 38_hex_general --features gl
//! cargo run --example 38_hex_general  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;

use tilekit::geom::{Cell, HexLayout, HexOrientation, Tile};
use tilekit::glyphs::terrain as tterrain;
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb, scale};

/// Tactical grid size in hexes: the same 12-wide-by-8-high board Fantasy
/// General's own tactical map uses (see the manual's screen layout section),
/// kept exact because the grid dimensions are part of what makes the frame
/// read as this game rather than a generic wargame.
const HEXES_W: i32 = 12;
/// See [`HEXES_W`].
const HEXES_H: i32 = 8;

/// A hex's cell pitch. Wide and tall enough to hold a one-glyph unit token
/// plus a strength badge directly beneath it -- the "numbered flag" the
/// screenshot shows under every unit -- which is why this is roughly the size
/// of [`19_hex_command`](../19_hex_command)'s blob hexes rather than the
/// thin honeycomb the earlier hex demos use.
const HEX_W: i32 = 12;
/// See [`HEX_W`].
const HEX_H: i32 = 6;

const LAYOUT: HexLayout = HexLayout::new(HexOrientation::Pointy, HEX_W, HEX_H);

/// Width of each decorative icon rail inside the outer frame.
const RAIL_W: u16 = 3;

/// Which faction a unit belongs to. Colors follow the manual's own
/// convention for the life-flag under each token: orange for the player's
/// council, blue for the enemy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Player,
    Enemy,
}

impl Side {
    const fn color(self) -> Color {
        match self {
            Self::Player => rgb(224, 140, 64),
            Self::Enemy => rgb(92, 140, 214),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Player => "Player",
            Self::Enemy => "Enemy",
        }
    }
}

/// Terrain a hex can be. Affects only the defender's side of the forecast,
/// same as the manual describes: terrain has no visible defense percentage
/// of its own (that is Wesnoth's move, reserved for 44), it just quietly
/// changes the numbers the forecast panel prints.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Clear,
    Rough,
    Forest,
    Hills,
}

impl Terrain {
    const fn color(self) -> Color {
        match self {
            Self::Clear => rgb(78, 96, 58),
            Self::Rough => rgb(92, 88, 82),
            Self::Forest => rgb(46, 70, 44),
            Self::Hills => rgb(102, 96, 70),
        }
    }

    /// Multiplier applied to a defender's effective defense when computing
    /// [`compute_forecast`]. Better cover means fewer expected losses for
    /// whoever is standing on it, not a displayed percentage -- the forecast
    /// panel is where that shows up, not the hex itself.
    const fn defense_mult(self) -> f32 {
        match self {
            Self::Clear => 1.0,
            Self::Rough => 1.15,
            Self::Forest => 1.35,
            Self::Hills => 1.25,
        }
    }

    fn texture(self, dx: i32, dy: i32, phase: u32) -> Option<char> {
        let h = hash01(phase, dx, dy);
        match self {
            Self::Clear => None,
            Self::Rough => (h < 0.28).then_some('\u{2591}'),
            Self::Forest => (h < 0.4).then_some(tterrain::CONIFER),
            Self::Hills => (h < 0.22).then_some('\u{2229}'),
        }
    }
}

/// A unit's fighting class: glyph, base attack/defense, and attack range in
/// hexes. Archers get range 2 (the manual's "Skirmish/Missile" attacks);
/// everything else is melee-only, range 1.
#[derive(Clone, Copy)]
struct UnitKind {
    species: &'static str,
    class: &'static str,
    glyph: char,
    attack: f32,
    defense: f32,
    range: i32,
}

const PLAYER_KINDS: [UnitKind; 3] = [
    UnitKind {
        species: "Elven",
        class: "Archers",
        glyph: 'A',
        attack: 6.0,
        defense: 3.0,
        range: 2,
    },
    UnitKind {
        species: "Human",
        class: "Cavalry",
        glyph: 'C',
        attack: 8.0,
        defense: 4.0,
        range: 1,
    },
    UnitKind {
        species: "Dwarf",
        class: "Infantry",
        glyph: 'I',
        attack: 7.0,
        defense: 6.0,
        range: 1,
    },
];

const ENEMY_KINDS: [UnitKind; 3] = [
    UnitKind {
        species: "Goblin",
        class: "Archers",
        glyph: 'a',
        attack: 5.0,
        defense: 2.0,
        range: 2,
    },
    UnitKind {
        species: "Goblin",
        class: "Warriors",
        glyph: 'w',
        attack: 6.0,
        defense: 4.0,
        range: 1,
    },
    UnitKind {
        species: "Troll",
        class: "Brutes",
        glyph: 'T',
        attack: 9.0,
        defense: 5.0,
        range: 1,
    },
];

/// Full shield pips a unit can earn. Five, matching the manual's "5 empty
/// shields = level 0" description; a unit's `experience` (0..=5) is how many
/// are filled in.
const MAX_PIPS: u8 = 5;

/// A wounds/morale bucket, derived from current strength as a fraction of
/// maximum. Only the current fraction matters for combat effectiveness in
/// this simplified model; the manual's own "disordered"/"broken" states are
/// collapsed to these three so the forecast panel has one word to print.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Morale {
    Steady,
    Shaken,
    Broken,
}

impl Morale {
    const fn color(self) -> Color {
        match self {
            Self::Steady => rgb(108, 196, 108),
            Self::Shaken => rgb(226, 184, 90),
            Self::Broken => rgb(216, 88, 84),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Steady => "Steady",
            Self::Shaken => "Shaken",
            Self::Broken => "Broken",
        }
    }
}

/// One unit on the board.
///
/// Identified by a stable `id` rather than its position in
/// [`HexGeneral::units`], because a defeated unit is removed from that vector
/// mid-frame and any index-based selection would silently start pointing at
/// whatever unit happened to slide into the vacated slot.
struct Unit {
    id: u32,
    ordinal: u32,
    kind: UnitKind,
    side: Side,
    tile: Tile,
    max_strength: i32,
    /// Permanent losses: never recovered by resting. Recovering these is the
    /// Army Management "Recruit" action in the real game, out of scope for a
    /// single tactical screen, so here they are simply permanent.
    kills_taken: i32,
    /// Temporary losses: recovered by [`Unit::rest`].
    wounds: i32,
    experience: u8,
}

impl Unit {
    fn display_name(&self) -> String {
        format!(
            "{} {} {}",
            ordinal_text(self.ordinal),
            self.kind.species,
            self.kind.class
        )
    }

    fn strength(&self) -> i32 {
        (self.max_strength - self.kills_taken - self.wounds).max(0)
    }

    fn strength_frac(&self) -> f32 {
        if self.max_strength <= 0 {
            0.0
        } else {
            self.strength() as f32 / self.max_strength as f32
        }
    }

    fn morale(&self) -> Morale {
        let f = self.strength_frac();
        if f > 0.7 {
            Morale::Steady
        } else if f > 0.35 {
            Morale::Shaken
        } else {
            Morale::Broken
        }
    }

    /// Filled/total shield pips for the experience readout.
    const fn pips(&self) -> (u8, u8) {
        (self.experience, MAX_PIPS)
    }

    fn is_defeated(&self) -> bool {
        self.strength() <= 0
    }

    /// Resting recovers wounds only -- kills are permanent, per the manual's
    /// distinction between the two loss pools. Modelled as an instant full
    /// recovery rather than a per-turn trickle: this demo has no turn
    /// counter to meter it against, and the point on show is that the two
    /// pools behave differently, not the exact recovery rate.
    const fn rest(&mut self) {
        self.wounds = 0;
    }

    /// Applies a forecasted exchange to this unit, clamping so neither pool
    /// can push total losses past `max_strength`.
    fn apply_losses(&mut self, kills: i32, wounds: i32) {
        let room = (self.max_strength - self.kills_taken).max(0);
        let kills = kills.clamp(0, room);
        self.kills_taken += kills;
        let room = (self.max_strength - self.kills_taken - self.wounds).max(0);
        self.wounds += wounds.clamp(0, room);
    }
}

/// `1st`, `2nd`, `3rd`, `4th`, ... English ordinal suffixes, the naming
/// convention the manual uses for its roster ("3rd Cavalry", "1st Treemen").
fn ordinal_text(n: u32) -> String {
    let suffix = if (11..=13).contains(&(n % 100)) {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// The projected outcome of one attack: kills and wounds on each side. Both
/// halves come from [`compute_forecast`], never from a coin flip -- the whole
/// point of the panel this demo is built around is that the number on screen
/// *is* what happens when the attack is committed, not a hint about a hidden
/// roll.
#[derive(Clone, Copy)]
struct Forecast {
    attacker_kills: i32,
    attacker_wounds: i32,
    defender_kills: i32,
    defender_wounds: i32,
}

/// How much raw damage one exchange deals, in strength points, before it is
/// split between the permanent and recoverable pools.
const BASE_DAMAGE: f32 = 3.4;

/// Fraction of raw damage that becomes a permanent kill rather than a
/// recoverable wound. The manual doesn't publish an exact split; this is
/// picked so a single hit rarely finishes a fresh unit outright but a
/// weakened one can be destroyed in one more exchange, which is the shape
/// attrition has in the actual game.
const KILL_FRACTION: f32 = 0.35;

/// Retaliation is scaled down from a full attack: the defender is reacting,
/// not initiating, and giving it the attacker's full power would make every
/// attack roughly symmetric, erasing the entire point of choosing when to
/// strike.
const RETALIATION_SCALE: f32 = 0.4;

/// Computes the projected exchange between `attacker` and `defender`,
/// standing on `defender_terrain`.
///
/// Deterministic and continuous in both units' current strength and
/// experience, which is what makes the forecast meaningful to look at twice:
/// weaken the defender in one exchange and the same attacker's next forecast
/// against it visibly improves, with no hidden state and no randomness.
fn compute_forecast(attacker: &Unit, defender: &Unit, defender_terrain: Terrain) -> Forecast {
    let atk_power = attacker.kind.attack
        * attacker.strength_frac()
        * 0.08f32.mul_add(f32::from(attacker.experience), 1.0);
    let def_power =
        defender.kind.defense * defender.strength_frac() * defender_terrain.defense_mult();
    let ratio = (atk_power / def_power.max(0.5)).clamp(0.15, 5.0);

    let def_damage = (BASE_DAMAGE * ratio).min(defender.strength() as f32);
    let def_kills = (def_damage * KILL_FRACTION).round() as i32;
    let def_wounds = (def_damage - def_kills as f32).max(0.0).round() as i32;

    // Retaliation only fires if the defender is not already broken and the
    // attacker is close enough to be hit back; a pure skirmish attack from
    // range 2 against a range-1 defender draws no return fire.
    let can_retaliate =
        defender.morale() != Morale::Broken && attacker.kind.range <= defender.kind.range.max(1);
    let (atk_kills, atk_wounds) = if can_retaliate {
        let retaliation = defender.kind.attack * defender.strength_frac() * RETALIATION_SCALE;
        let guard = attacker.kind.defense * attacker.strength_frac();
        let back_ratio = (retaliation / guard.max(0.5)).clamp(0.0, 3.0);
        let atk_damage = (BASE_DAMAGE * back_ratio * 0.6).min(attacker.strength() as f32);
        let k = (atk_damage * KILL_FRACTION).round() as i32;
        let w = (atk_damage - k as f32).max(0.0).round() as i32;
        (k, w)
    } else {
        (0, 0)
    };

    Forecast {
        attacker_kills: atk_kills,
        attacker_wounds: atk_wounds,
        defender_kills: def_kills,
        defender_wounds: def_wounds,
    }
}

/// What a tap on the map area resolves to, once translated from screen space
/// back into a hex tile by [`HexGeneral::handle_map_tap`].
enum Action {
    Map,
    RestButton,
    CancelButton,
    NextButton,
}

/// State: the generated terrain, the roster, selection/forecast state, the
/// camera, and the input plumbing.
pub struct HexGeneral {
    seed: u32,
    terrain: Vec<Terrain>,
    units: Vec<Unit>,
    selected: Option<u32>,
    forecast_target: Option<u32>,
    cursor: Tile,
    gold: u32,
    region: &'static str,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    map_area: Rect,
    fps: FpsMeter,
}

impl Default for HexGeneral {
    fn default() -> Self {
        let seed = 38;
        let terrain = generate_terrain(seed);
        let units = generate_units(seed);
        let selected = units.iter().find(|u| u.side == Side::Player).map(|u| u.id);
        let cursor = selected
            .and_then(|id| units.iter().find(|u| u.id == id))
            .map_or(Tile::new(HEXES_W / 2, HEXES_H / 2), |u| u.tile);
        Self {
            seed,
            terrain,
            units,
            selected,
            forecast_target: None,
            cursor,
            gold: 340,
            region: "Greywater Marches",
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            map_area: Rect::new(0, 0, 0, 0),
            fps: FpsMeter::new(),
        }
    }
}

/// Builds the terrain grid, deterministic in `seed`.
fn generate_terrain(seed: u32) -> Vec<Terrain> {
    (0..HEXES_H)
        .flat_map(|row| (0..HEXES_W).map(move |col| (col, row)))
        .map(|(col, row)| {
            let n = hash01(seed, col, row);
            if n < 0.16 {
                Terrain::Forest
            } else if n < 0.3 {
                Terrain::Hills
            } else if n < 0.4 {
                Terrain::Rough
            } else {
                Terrain::Clear
            }
        })
        .collect()
}

/// Places a small player garrison on the western edge and a small enemy
/// warband on the eastern edge, deterministic in `seed`.
fn generate_units(seed: u32) -> Vec<Unit> {
    let mut rng = Rng::new(seed ^ 0x4647_5f31);
    let mut units = Vec::new();
    let mut id = 0u32;

    let spawn = |units: &mut Vec<Unit>,
                 id: &mut u32,
                 side: Side,
                 kinds: &[UnitKind],
                 cols: core::ops::Range<i32>,
                 rng: &mut Rng| {
        for row in 1..HEXES_H - 1 {
            if rng.next_f32() > 0.6 {
                continue;
            }
            let col = cols.start + rng.next_below((cols.end - cols.start) as u32) as i32;
            let kind = kinds[rng.next_below(kinds.len() as u32) as usize];
            *id += 1;
            units.push(Unit {
                id: *id,
                ordinal: *id,
                kind,
                side,
                tile: Tile::new(col, row),
                max_strength: 15,
                kills_taken: 0,
                wounds: rng.next_below(4) as i32,
                experience: rng.next_below(3) as u8,
            });
        }
    };

    spawn(
        &mut units,
        &mut id,
        Side::Player,
        &PLAYER_KINDS,
        0..3,
        &mut rng,
    );
    spawn(
        &mut units,
        &mut id,
        Side::Enemy,
        &ENEMY_KINDS,
        HEXES_W - 3..HEXES_W,
        &mut rng,
    );

    // Nudge apart any two units that landed on the same hex: the spawn pass
    // above is random per row and can't see other rows' choices, so a
    // collision is rare but possible, not impossible.
    for i in 0..units.len() {
        for j in 0..i {
            if units[i].tile == units[j].tile {
                units[i].tile.col = (units[i].tile.col + 1).min(HEXES_W - 1);
            }
        }
    }

    units
}

impl HexGeneral {
    const fn terrain_index(tile: Tile) -> Option<usize> {
        if tile.col < 0 || tile.row < 0 || tile.col >= HEXES_W || tile.row >= HEXES_H {
            return None;
        }
        Some((tile.row * HEXES_W + tile.col) as usize)
    }

    fn terrain_at(&self, tile: Tile) -> Terrain {
        Self::terrain_index(tile).map_or(Terrain::Clear, |i| self.terrain[i])
    }

    fn unit_by_id(&self, id: u32) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    fn unit_at(&self, tile: Tile) -> Option<&Unit> {
        self.units.iter().find(|u| u.tile == tile)
    }

    /// The attacker/defender pair currently being previewed, if both still
    /// exist (a commit can remove the defender, in which case the forecast
    /// silently has nothing left to show).
    fn pair(&self) -> Option<(&Unit, &Unit)> {
        let atk = self.selected.and_then(|id| self.unit_by_id(id))?;
        let def = self.forecast_target.and_then(|id| self.unit_by_id(id))?;
        Some((atk, def))
    }

    fn in_range(attacker: &Unit, defender: &Unit) -> bool {
        LAYOUT.distance(attacker.tile, defender.tile) <= attacker.kind.range
    }

    /// Resolves one tap on the hex field: select/reselect a friendly unit,
    /// preview an enemy on first tap, or commit the previewed attack on a
    /// second tap of the same target. This is the whole two-tap contract the
    /// module doc promises -- there is no hover path into this function at
    /// all, so a mouse and a finger go through identical logic.
    fn handle_tap(&mut self, tile: Tile) {
        self.cursor = tile;
        let Some(target) = self.unit_at(tile) else {
            self.selected = None;
            self.forecast_target = None;
            return;
        };

        match target.side {
            Side::Player => {
                self.selected = Some(target.id);
                self.forecast_target = None;
            }
            Side::Enemy => {
                let Some(sel_id) = self.selected else {
                    return;
                };
                let Some(attacker) = self.unit_by_id(sel_id) else {
                    return;
                };
                if !Self::in_range(attacker, target) {
                    return;
                }
                if self.forecast_target == Some(target.id) {
                    self.commit_attack();
                } else {
                    self.forecast_target = Some(target.id);
                }
            }
        }
    }

    /// Applies the currently previewed forecast to both units, removes
    /// whichever side was destroyed, and grants the attacker experience for
    /// a kill. Clearing `forecast_target` afterward is what makes a second
    /// press of the same button (Enter, or a third tap) a no-op rather than
    /// re-applying a stale exchange.
    fn commit_attack(&mut self) {
        let Some(atk_id) = self.selected else {
            return;
        };
        let Some(def_id) = self.forecast_target else {
            return;
        };
        let Some((attacker, defender)) = self.pair() else {
            return;
        };
        let forecast = compute_forecast(attacker, defender, self.terrain_at(defender.tile));

        if let Some(u) = self.units.iter_mut().find(|u| u.id == def_id) {
            u.apply_losses(forecast.defender_kills, forecast.defender_wounds);
        }
        let defeated = self.unit_by_id(def_id).is_some_and(Unit::is_defeated);

        if let Some(u) = self.units.iter_mut().find(|u| u.id == atk_id) {
            u.apply_losses(forecast.attacker_kills, forecast.attacker_wounds);
            if defeated {
                u.experience = (u.experience + 1).min(MAX_PIPS);
            }
        }

        self.units.retain(|u| !u.is_defeated());
        self.forecast_target = None;
        if self.unit_by_id(atk_id).is_none() {
            self.selected = None;
        }
    }

    fn rest_selected(&mut self) {
        if let Some(id) = self.selected
            && let Some(u) = self.units.iter_mut().find(|u| u.id == id)
        {
            u.rest();
        }
    }

    const fn cancel_forecast(&mut self) {
        self.forecast_target = None;
    }

    /// Cycles selection to the next surviving player unit, so the keyboard
    /// has full parity with tapping a unit on the map.
    fn select_next(&mut self) {
        let ids: Vec<u32> = self
            .units
            .iter()
            .filter(|u| u.side == Side::Player)
            .map(|u| u.id)
            .collect();
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let next = self
            .selected
            .and_then(|cur| ids.iter().position(|&i| i == cur))
            .map_or(0, |pos| (pos + 1) % ids.len());
        self.selected = Some(ids[next]);
        self.forecast_target = None;
        if let Some(u) = self.unit_by_id(ids[next]) {
            self.cursor = u.tile;
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
                match key.code {
                    KeyCode::Char('r' | 'R') => self.rest_selected(),
                    KeyCode::Char('c' | 'C') | KeyCode::Escape => self.cancel_forecast(),
                    KeyCode::Tab | KeyCode::Char('n' | 'N') => self.select_next(),
                    KeyCode::Enter if self.forecast_target.is_some() => {
                        self.commit_attack();
                    }
                    _ => {}
                }
            }
        }
        true
    }

    /// Translates the gesture's tap into a hex tile via the same
    /// origin/screen math [`draw_map`](Self::draw_map) used to place the
    /// hexes, then dispatches by hotspot action.
    fn handle_pointer(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos) {
            Some(Action::RestButton) => self.rest_selected(),
            Some(Action::CancelButton) => self.cancel_forecast(),
            Some(Action::NextButton) => self.select_next(),
            Some(Action::Map) => {
                // A hit against this hotspot already guarantees `pos` falls
                // inside `self.map_area` (see `Hotspots::hit`), so no extra
                // bounds check is needed before converting to world space.
                let origin = self.map_origin(self.map_area);
                let wx = origin.x + i32::from(pos.x - self.map_area.left());
                let wy = origin.y + i32::from(pos.y - self.map_area.top());
                let tile = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                self.handle_tap(tile);
            }
            None => {}
        }
    }

    /// The board's own bounding box in world cells, independent of the
    /// viewport. Used to clamp the camera so it never shows more empty
    /// space on one side than the other -- see [`map_origin`](Self::map_origin).
    const fn board_bounds() -> (Cell, Cell) {
        let top_left = LAYOUT.tile_to_cell(Tile::new(0, 0));
        let bottom_right = {
            let last = LAYOUT.tile_to_cell(Tile::new(HEXES_W - 1, HEXES_H - 1));
            Cell::new(last.x + HEX_W, last.y + HEX_H)
        };
        (top_left, bottom_right)
    }

    /// The world-cell origin used to draw `area`.
    ///
    /// Centering on `self.cursor` alone (the original approach, and the one
    /// [`19_hex_command`](../19_hex_command) uses) works when the cursor
    /// sits near the board's own centroid, but this demo's default cursor is
    /// the first player unit's tile -- near the western edge, not the middle
    /// -- so a pure cursor-centered camera left most of the board pushed off
    /// to one side with the opposite side showing dead space: exactly the
    /// defect this function now prevents. Each axis is handled independently:
    /// if the board is narrower than the viewport on that axis, the board is
    /// centered in the viewport instead of following the cursor (there is
    /// nothing to pan to, so panning would only look like drift); otherwise
    /// the cursor-centered origin is clamped so the viewport's edge never
    /// crosses the board's own edge, which is what actually keeps the field
    /// filling the frame instead of floating inside it.
    fn map_origin(&self, area: Rect) -> Cell {
        let center = LAYOUT.center_cell(self.cursor);
        let (top_left, bottom_right) = Self::board_bounds();
        let board_w = bottom_right.x - top_left.x;
        let board_h = bottom_right.y - top_left.y;
        let view_w = i32::from(area.width());
        let view_h = i32::from(area.height());

        let x = if board_w <= view_w {
            top_left.x - (view_w - board_w) / 2
        } else {
            (center.x - view_w / 2).clamp(top_left.x, bottom_right.x - view_w)
        };
        let y = if board_h <= view_h {
            top_left.y - (view_h - board_h) / 2
        } else {
            (center.y - view_h / 2).clamp(top_left.y, bottom_right.y - view_h)
        };
        Cell::new(x, y)
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.map_area = area;
        self.hotspots.push(area, Action::Map);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let origin = self.map_origin(area);

        for sy in area.top()..area.bottom() {
            for sx in area.left()..area.right() {
                let wx = origin.x + i32::from(sx - area.left());
                let wy = origin.y + i32::from(sy - area.top());
                let tile = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                let Some(_) = Self::terrain_index(tile) else {
                    continue;
                };
                self.paint_cell(surface, (sx, sy), tile, wx, wy);
            }
        }
        self.draw_units(surface, area, origin);
    }

    fn paint_cell(&self, surface: &mut Surface<'_>, at: (u16, u16), tile: Tile, wx: i32, wy: i32) {
        let terrain = self.terrain_at(tile);
        let mut base = terrain.color();
        if self
            .selected
            .is_some_and(|id| self.unit_by_id(id).is_some_and(|u| u.tile == tile))
        {
            // The selected unit's hex pulses: a slow sine glow rather than a
            // hard highlight, so it stays legible without drowning the
            // terrain color it is layered over.
            let pulse = 0.5f32.mul_add((self.time * 3.0).sin(), 0.5);
            base = mix(base, rgb(255, 236, 170), 0.2f32.mul_add(pulse, 0.25));
        }

        // A border cell is one whose north or west neighbour resolves to a
        // different hex; checking only those two directions (not all four)
        // is what stops a shared edge from being drawn once by each of the
        // two hexes that own it, the same trick 19_hex_command's paint_cell
        // uses. The glyph itself is picked directly rather than through a
        // connectivity mask table: a border only ever needs "this edge runs
        // horizontally", "vertically", or "a corner sits here", and asking a
        // 4-direction autotile mask for that (as an earlier draft of this
        // function did) produces lone stub glyphs instead of a continuous
        // line, because a straight run of north-differs cells never sets the
        // east/south bits a connectivity table expects to see.
        let north = LAYOUT.cell_to_tile(Cell::new(wx, wy - 1));
        let west = LAYOUT.cell_to_tile(Cell::new(wx - 1, wy));
        if (north != tile && Self::terrain_index(north).is_some())
            || (west != tile && Self::terrain_index(west).is_some())
        {
            let glyph = if north != tile && west != tile {
                '\u{256C}'
            } else if north != tile {
                '\u{2550}'
            } else {
                '\u{2551}'
            };
            let border = scale(terrain.color(), 0.5);
            surface.put(at, glyph, Style::new().fg(border).bg(base));
            return;
        }

        let center = LAYOUT.center_cell(tile);
        let phase = (tile.col as u32).wrapping_mul(2_654_435_761)
            ^ (tile.row as u32).wrapping_mul(40503)
            ^ self.seed;
        let glyph = terrain
            .texture(wx - center.x, wy - center.y, phase)
            .unwrap_or(' ');
        let fg = mix(base, tilekit::palette::WHITE, 0.35);
        surface.put(at, glyph, Style::new().fg(fg).bg(base));
    }

    /// Draws every unit token: a class glyph, a fluttering pennant above it,
    /// and a strength badge below it, colored by side.
    fn draw_units(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell) {
        for unit in &self.units {
            let center = LAYOUT.center_cell(unit.tile);
            let Some((sx, sy)) = to_screen(area, origin, center.x, center.y) else {
                continue;
            };
            let color = unit.side.color();
            let bg = scale(self.terrain_at(unit.tile).color(), 0.9);

            // Pennant flutter: a per-unit phase (from its own tile, so it is
            // stable frame to frame and stays deterministic) alternates
            // between two diagonal glyphs on a fixed period. Two discrete
            // states rather than an eased slide, matching the rule that idle
            // animation belongs on art, not on anything that has to stay
            // pinned to the grid -- this is art, but it is still cheapest and
            // clearest as a hard flip.
            let phase = hash01(0x9e37, unit.tile.col, unit.tile.row) * 0.6;
            let flip = ((self.time + phase) % 0.7) < 0.35;
            if sy > area.top() {
                let pennant = if flip { '\\' } else { '/' };
                surface.put((sx, sy - 1), pennant, Style::new().fg(color).bg(bg));
            }

            surface.put(
                (sx, sy),
                unit.kind.glyph,
                Style::new().fg(rgb(10, 10, 12)).bg(color),
            );

            if sy + 1 < area.bottom() {
                let badge = format!("{:>2}", unit.strength());
                let bx = sx.saturating_sub(1);
                for (i, ch) in badge.chars().enumerate() {
                    let bxp = bx + i as u16;
                    if bxp < area.right() {
                        surface.put(
                            (bxp, sy + 1),
                            ch,
                            Style::new().fg(rgb(20, 18, 16)).bg(color),
                        );
                    }
                }
            }
        }
    }

    /// Draws the outer ornate frame -- double border, icon rails, and a
    /// banner with gold/region/hex coordinate -- and returns the working
    /// rect left over for the map and the comparison panels.
    fn draw_frame(&self, surface: &mut Surface<'_>, area: Rect) -> Rect {
        let inner = Panel::new()
            .border(Border::Double)
            .frame(rgb(196, 162, 74))
            .draw(surface, area);
        if inner.height() < 4 || inner.width() < 8 {
            return inner;
        }

        let (banner_area, rest) = panel::split_top(inner, 1);
        self.draw_banner(surface, banner_area);
        let (rule_area, rest) = panel::split_top(rest, 1);
        Self::draw_rule(surface, rule_area);

        let show_rails = rest.width() > RAIL_W * 2 + 40;
        if show_rails {
            let (left_rail, rest) = panel::split_left(rest, RAIL_W);
            let (rest, right_rail) = panel::split_right(rest, RAIL_W);
            self.draw_rail(surface, left_rail, 0x1122);
            self.draw_rail(surface, right_rail, 0x3344);
            rest
        } else {
            rest
        }
    }

    fn draw_banner(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(panel::PANEL_BG));
        let gold = format!(" Gold: {}", self.gold);
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width().min(20),
            &[Span::keyword(&gold)],
            panel::PANEL_BG,
        );

        let coord = tile_label(self.cursor);
        let right = format!("Hex {coord} ");
        let rw = right.chars().count() as u16;
        if area.width() > rw {
            panel::spans(
                surface,
                (area.right() - rw, area.top()),
                rw,
                &[Span::dim(&right)],
                panel::PANEL_BG,
            );
        }

        let region_w = self.region.chars().count() as u16;
        if area.width() > region_w + 40 {
            let cx = area.left() + (area.width() - region_w) / 2;
            panel::spans(
                surface,
                (cx, area.top()),
                region_w,
                &[Span::plain(self.region)],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_rule(surface: &mut Surface<'_>, area: Rect) {
        let style = Style::new().fg(rgb(120, 104, 60)).bg(panel::PANEL_BG);
        for x in area.left()..area.right() {
            surface.put((x, area.top()), '\u{2550}', style);
        }
    }

    /// Draws one icon rail as a terrain legend: each row pairs a terrain
    /// glyph with its own name initial, on a plain background, so the rail
    /// reads as "what these map glyphs mean" rather than as texture. An
    /// earlier version filled the whole rail with a per-cell shimmer *and*
    /// stacked the icon above its letter on separate rows -- legible neither
    /// as a legend (icon and label were not on the same row) nor as pure
    /// decoration (the shimmer read as noise once icons were dropped on top
    /// of it) -- so this version puts icon and label side by side on one
    /// row over a flat panel background, and reserves the animated glow for
    /// a single vertical divider column instead of the whole rail.
    /// `salt` keeps the two rails' glow out of phase with each other so they
    /// read as two independent strips rather than a mirrored pair.
    fn draw_rail(&self, surface: &mut Surface<'_>, area: Rect, salt: u32) {
        if area.width() == 0 {
            return;
        }
        let bg = rgb(26, 27, 34);
        surface.fill_rect(area, ' ', Style::new().bg(bg));

        // A single glowing divider rule at the rail's outer edge (nearest
        // the frame border) is what carries the idle-animation requirement;
        // it does not compete with the legend text next to it because it is
        // a full separate column, not text painted over shimmering cells.
        if area.width() > 0 {
            let angle = self.time.mul_add(0.6, f32::from(salt as u16) * 0.01);
            let twinkle = 0.5f32.mul_add(angle.sin(), 0.5);
            let v = 40.0f32.mul_add(twinkle, 70.0) as u8;
            let glow = Style::new().fg(rgb(v, v - 10, v - 20)).bg(bg);
            for y in 0..area.height() {
                surface.put((area.left(), area.top() + y), '\u{2502}', glow);
            }
        }

        if area.width() < 3 {
            return;
        }
        let icons = [
            (tterrain::CONIFER, 'F'),
            (tterrain::HILLS, 'H'),
            (tterrain::MOUNTAIN, 'M'),
        ];
        let style = Style::new().fg(rgb(210, 200, 170)).bg(bg);
        for (i, (icon, letter)) in icons.iter().enumerate() {
            let y = area.top() + i as u16 * 2 + 1;
            if y >= area.bottom() {
                break;
            }
            // Icon and label share one row, one cell apart, so the pairing
            // is unambiguous regardless of how narrow the rail is -- unlike
            // stacking them on two rows, which reads as two unrelated marks
            // once nothing else on screen ties them together.
            surface.put((area.left() + 1, y), *icon, style);
            if area.width() > 2 {
                surface.put((area.left() + 2, y), *letter, style);
            }
        }
    }

    /// Splits the frame's working area into the hex map (top) and the
    /// forecast comparison region (bottom). The comparison region's share of
    /// the height is deliberately generous and shape-dependent -- a stacked
    /// portrait layout needs more total rows for the same two panels than a
    /// side-by-side one does -- because the forecast is the one thing this
    /// demo refuses to truncate; the map gives up rows first.
    fn split_map_bottom(area: Rect, shape: Shape) -> (Rect, Rect) {
        let bottom_pct: u32 = if shape.stacks() { 62 } else { 42 };
        let min_bottom: u16 = 14;
        let desired = (u32::from(area.height()) * bottom_pct / 100) as u16;
        let bottom_h = desired
            .max(min_bottom.min(area.height()))
            .min(area.height());
        panel::split_bottom(area, bottom_h)
    }

    fn draw_bottom(&mut self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let button_h = 4u16.min(area.height());
        let (compare_area, button_area) = panel::split_bottom(area, button_h);

        let attacker = self.selected.and_then(|id| self.unit_by_id(id));
        let target = self.forecast_target.and_then(|id| self.unit_by_id(id));
        let forecast = self
            .pair()
            .map(|(a, d)| compute_forecast(a, d, self.terrain_at(d.tile)));

        if shape.stacks() {
            let exchange_h = 7u16.min(compare_area.height() / 3).max(3);
            let side_h = (compare_area.height().saturating_sub(exchange_h)) / 2;
            let (attacker_area, rest) = panel::split_top(compare_area, side_h);
            let (exchange_area, defender_area) = panel::split_top(rest, exchange_h);
            draw_unit_panel(surface, attacker_area, "ATTACKER", attacker);
            draw_exchange(surface, exchange_area, attacker, target, forecast);
            draw_unit_panel(surface, defender_area, "DEFENDER", target);
        } else {
            let exchange_w = 30u16.min(compare_area.width() / 4).max(18);
            let side_w = (compare_area.width().saturating_sub(exchange_w)) / 2;
            let (attacker_area, rest) = panel::split_left(compare_area, side_w);
            let (exchange_area, defender_area) = panel::split_left(rest, exchange_w);
            draw_unit_panel(surface, attacker_area, "ATTACKER", attacker);
            draw_exchange(surface, exchange_area, attacker, target, forecast);
            draw_unit_panel(surface, defender_area, "DEFENDER", target);
        }

        self.draw_buttons(surface, button_area);
    }

    fn draw_buttons(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(ui::CHROME_BG));
        let labels: [(&str, Action); 3] = [
            ("Rest", Action::RestButton),
            ("Cancel", Action::CancelButton),
            ("Next", Action::NextButton),
        ];
        let cols = panel::columns(area, labels.len() as u16, 1);
        for ((label, action), rect) in labels.into_iter().zip(cols) {
            let tap_rect = ui::touch::tappable(rect, area);
            self.hotspots.push(tap_rect, action);
            surface.fill_rect(rect, ' ', Style::new().bg(rgb(30, 32, 42)));
            let style = Style::new().fg(ui::ACCENT).bg(rgb(30, 32, 42));
            let cx = rect.left() + rect.width().saturating_sub(label.chars().count() as u16) / 2;
            let cy = rect.top() + rect.height() / 2;
            if rect.width() > 0 && rect.height() > 0 {
                surface.print((cx, cy), label, style);
            }
        }
    }

    fn status(&self) -> String {
        let sel = self
            .selected
            .and_then(|id| self.unit_by_id(id))
            .map_or_else(|| "none".to_string(), Unit::display_name);
        format!("{}  selected: {sel}", tile_label(self.cursor))
    }
}

fn draw_unit_panel(surface: &mut Surface<'_>, area: Rect, title: &str, unit: Option<&Unit>) {
    let accent = unit.map_or(panel::FRAME, |u| u.side.color());
    let inner = Panel::new()
        .title(title)
        .border(Border::Double)
        .frame(accent)
        .draw(surface, area);
    if inner.height() == 0 {
        return;
    }
    let Some(u) = unit else {
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::dim("No unit selected.")],
            panel::PANEL_BG,
        );
        return;
    };

    draw_unit_panel_body(surface, inner, u);
}

/// The interior of [`draw_unit_panel`], split out to keep both functions
/// under the line-count lint: name, side/class, strength gauge, experience
/// pips, and morale, each guarded so a squeezed panel drops the least
/// important rows first rather than clipping mid-line.
fn draw_unit_panel_body(surface: &mut Surface<'_>, inner: Rect, u: &Unit) {
    let mut y = inner.top();
    panel::spans(
        surface,
        (inner.left(), y),
        inner.width(),
        &[Span::keyword(&u.display_name())],
        panel::PANEL_BG,
    );
    y += 1;

    if y < inner.bottom() {
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[
                Span::new(u.side.label(), u.side.color()),
                Span::plain(" "),
                Span::dim(u.kind.class),
            ],
            panel::PANEL_BG,
        );
        y += 1;
    }

    if y < inner.bottom() {
        let bar_w = inner.width().saturating_sub(14).max(4);
        panel::spans(
            surface,
            (inner.left(), y),
            10,
            &[Span::dim("Strength")],
            panel::PANEL_BG,
        );
        panel::bar(
            surface,
            (inner.left() + 9, y),
            bar_w,
            u.strength_frac(),
            panel::threshold(u.strength_frac()),
            rgb(30, 30, 36),
        );
        let readout = format!(" {}/{}", u.strength(), u.max_strength);
        panel::spans(
            surface,
            (inner.left() + 9 + bar_w, y),
            inner.width(),
            &[Span::plain(&readout)],
            panel::PANEL_BG,
        );
        y += 1;
    }

    if y < inner.bottom() {
        let (filled, total) = u.pips();
        let mut pips = String::new();
        for i in 0..total {
            pips.push(if i < filled { '\u{25A0}' } else { '\u{25CB}' });
        }
        panel::spans(
            surface,
            (inner.left(), y),
            10,
            &[Span::dim("Exp ")],
            panel::PANEL_BG,
        );
        panel::spans(
            surface,
            (inner.left() + 4, y),
            inner.width().saturating_sub(4),
            &[Span::new(&pips, ui::ACCENT)],
            panel::PANEL_BG,
        );
        y += 1;
    }

    if y < inner.bottom() {
        let morale = u.morale();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[
                Span::dim("Morale "),
                Span::new(morale.label(), morale.color()),
            ],
            panel::PANEL_BG,
        );
    }
}

/// Draws the exchange strip between the two panels: the projected kills and
/// wounds for each direction, clearly distinguished (each row keeps the
/// attacked side's own color, with an arrow showing who is hitting whom) and
/// a one-word risk verdict so the balance is legible without doing the
/// subtraction in your head.
fn draw_exchange(
    surface: &mut Surface<'_>,
    area: Rect,
    attacker: Option<&Unit>,
    target: Option<&Unit>,
    forecast: Option<Forecast>,
) {
    let inner = Panel::new()
        .title("Forecast")
        .border(Border::Double)
        .draw(surface, area);
    if inner.height() == 0 {
        return;
    }

    let (Some(atk), Some(def), Some(f)) = (attacker, target, forecast) else {
        let msg = if attacker.is_none() {
            "Select a unit."
        } else {
            "Tap an enemy in range to preview."
        };
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::dim(msg)],
            panel::PANEL_BG,
        );
        return;
    };

    let mut y = inner.top();
    panel::spans(
        surface,
        (inner.left(), y),
        inner.width(),
        &[
            Span::new("You ", atk.side.color()),
            Span::plain("\u{25BA} "),
            Span::new(
                &format!("K:{} W:{}", f.attacker_kills, f.attacker_wounds),
                atk.side.color(),
            ),
        ],
        panel::PANEL_BG,
    );
    y += 1;

    if y < inner.bottom() {
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[
                Span::new("Them ", def.side.color()),
                Span::plain("\u{25C4} "),
                Span::new(
                    &format!("K:{} W:{}", f.defender_kills, f.defender_wounds),
                    def.side.color(),
                ),
            ],
            panel::PANEL_BG,
        );
        y += 1;
    }

    if y < inner.bottom() {
        let own = f.attacker_kills * 2 + f.attacker_wounds;
        let their = f.defender_kills * 2 + f.defender_wounds;
        let (verdict, color) = if their > own * 2 {
            ("FAVORABLE", rgb(108, 196, 108))
        } else if their >= own {
            ("EVEN", rgb(226, 184, 90))
        } else {
            ("RISKY", rgb(216, 88, 84))
        };
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::dim("Odds: "), Span::new(verdict, color)],
            panel::PANEL_BG,
        );
    }
}

/// `A4`-style label for a tile: row letter, column number (1-based).
fn tile_label(tile: Tile) -> String {
    let letter = (b'A' + (tile.row.rem_euclid(26)) as u8) as char;
    format!("{letter}{}", tile.col + 1)
}

/// Converts a world cell to a screen cell inside `area`, given `origin`.
fn to_screen(area: Rect, origin: Cell, wx: i32, wy: i32) -> Option<(u16, u16)> {
    let (dx, dy) = (wx - origin.x, wy - origin.y);
    if dx < 0 || dy < 0 || dx >= i32::from(area.width()) || dy >= i32::from(area.height()) {
        return None;
    }
    Some((area.left() + dx as u16, area.top() + dy as u16))
}

impl Demo for HexGeneral {
    const NAME: &'static str = "38_hex_general";
    const TITLE: &'static str = "38 Hex General";
    const BLURB: &'static str =
        "Fantasy General: framed hex map with a projected-losses attack preview.";
    const GRID: (u16, u16) = (160, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap unit", "select"),
            ("tap enemy", "preview / confirm attack"),
            ("Enter", "confirm attack"),
            ("R", "rest selected"),
            ("C/Esc", "cancel forecast"),
            ("Tab", "next unit"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.hotspots.clear();
        self.handle_pointer();

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        let inner = self.draw_frame(&mut surface, content);
        let (map_area, bottom_area) = Self::split_map_bottom(inner, shape);
        self.draw_map(&mut surface, map_area);
        self.draw_bottom(&mut surface, bottom_area, shape);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(HexGeneral);
