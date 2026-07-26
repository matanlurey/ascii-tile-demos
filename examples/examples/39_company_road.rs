//! 39: Company Road -- a mercenary company's overland map, adapted from
//! Battle Brothers' strategic layer (not its hex tactical screen, which is
//! [`38_hex_general`](../38_hex_general)'s subject).
//!
//! The element this demo is built around is the one thing every other map in
//! this gallery lacks: a moving vision disc paired with a speed control.
//! [`11_fog_of_war`](../11_fog_of_war) already has shadowcast field-of-view
//! and a three-state exploration memory, but its camera and its clock both
//! sit still while the player thinks. Here the company never stops: real
//! seconds are game-hours, the vision circle travels with the company rather
//! than snapping to a turn boundary, and daylight itself is one of the
//! variables under the pause/play/fast-forward cluster's control. Pausing
//! does not just stop the company -- it stops the sun.
//!
//! Techniques on show:
//!
//! - **A vision disc that travels in continuous time**
//!   ([`CompanyRoad::vis_dist`], [`CompanyRoad::simulate`]): every cell's
//!   distance to the company is measured in an aspect-corrected metric (the
//!   same `dy * 2.0` trick [`26_hexcrawl`](../26_hexcrawl) uses for its hex
//!   distance), so a disc that would be an ellipse in raw cell coordinates
//!   reads as a circle against cells that are twice as tall as they are wide.
//!   The disc shrinks at night ([`CompanyRoad::vision_radius`]), which is the
//!   legible consequence of pausing at 2 a.m. instead of noon.
//! - **A three-state exploration memory with no camera to reset it**
//!   ([`CompanyRoad::mark_visited_disc`]): a persistent `visited` bitmap plus
//!   a live vision test blends three looks per cell -- lit, remembered
//!   (dimmed, no longer live), and cloud (never seen) -- driven by
//!   [`tilekit::palette::mix`] rather than a hard cutover, so the disc's edge
//!   is a soft gradient, not a ring.
//! - **A clock that is also the palette** ([`CompanyRoad::daylight`]): one
//!   continuous function of game-hours drives the map's day/night tint, the
//!   vision radius, and the sun/moon dial simultaneously, so "what time is
//!   it" is answered by three different parts of the screen agreeing with
//!   each other rather than three separate timers that could drift apart.
//! - **Heraldic banners as multi-cell landmarks** ([`draw_settlement`]): each
//!   settlement is a bordered shield carrying a house charge over a field
//!   color, with its name in caps beneath -- the ASCII equivalent of the
//!   painted banners the reference screenshot uses to make towns
//!   recognizable at a glance, not one glyph guessing at a coat of arms.
//! - **Tap-to-path with terrain cost** ([`CompanyRoad::advance_company`]):
//!   a tapped point becomes a destination the company walks toward over real
//!   time, slowed on marsh, wood, and hills and turned back at open water,
//!   rather than a turn-based jump.
//! - **Full keyboard parity for a touch-first map** ([`CompanyRoad::keys`],
//!   [`CompanyRoad::handle_key`]): a cursor moved with the arrow keys and
//!   confirmed with Enter reaches any point a finger could tap, Tab cycles
//!   the settlement banners a finger would otherwise have to hunt for, and
//!   Space/F drive the same pause/play/fast-forward state a mouse would.
//!
//! ```sh
//! cargo run --example 39_company_road --features crossterm
//! cargo run --example 39_company_road --features software
//! cargo run --example 39_company_road --features gl
//! cargo run --example 39_company_road  # headless, prints a few frames
//! ```

use core::f32::consts::TAU;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::glyphs::terrain as tterrain;
use tilekit::noise::{fbm, hash01};
use tilekit::palette::{mix, rgb, scale};

/// World size in cells. Large enough that the company spends real minutes
/// crossing it (this is a map to live on, not a puzzle to solve in one
/// glance) and small enough that [`CompanyRoad::visited`] -- one `bool` per
/// cell -- stays a trivial allocation.
const WORLD_W: i32 = 140;
/// See [`WORLD_W`].
const WORLD_H: i32 = 92;

/// Noise seed for the terrain field. Fixed rather than reseedable: the point
/// on show is the vision disc and the clock, not world generation, so one
/// good-looking continent beats a reroll control competing for the same
/// screen space.
const SEED: u32 = 0x39C0_7A0D;
/// A second, unrelated seed for the cloud texture over unseen terrain, so the
/// cloud pattern does not visibly correlate with the terrain hiding under it.
const CLOUD_SEED: u32 = 0x0C10_0D01;

/// World-units the company crosses per second at 1x speed, on clear ground.
/// Chosen so a tap across a third of the map resolves in comfortably under a
/// minute at 1x and under twenty seconds at the fast-forward multiplier --
/// fast enough that the destination marker is not a promise you forget about,
/// slow enough that the walk itself is watchable.
const BASE_SPEED: f32 = 5.2;

/// Speed multiplier applied while standing on rough ground (marsh, wood, or
/// hills). Roughly half, which is enough to be visibly slower without making
/// wooded terrain feel like a wall -- the manual's own description of terrain
/// cost on the strategic layer is "slower", not "nearly impassable".
const ROUGH_FACTOR: f32 = 0.45;

/// World-units short of a destination that counts as "arrived". Larger than
/// zero because a company closing at [`BASE_SPEED`] can overshoot a point
/// target by more than a hair in one frame at a large `dt` (a stalled tab, a
/// slow backend); without slack it would oscillate around the destination
/// forever instead of settling.
const ARRIVE_DIST: f32 = 0.2;

/// How many world-units wide the vision disc's soft edge band is. Zero would
/// make the disc a hard-edged coin, which is the one thing the reference
/// screenshot's cloud-and-fog boundary explicitly is not.
const VISION_SOFT: f32 = 3.2;

/// Real seconds per in-game hour at 1x. A full day is nine minutes of real
/// time at 1x, which is long enough that the company visibly travels between
/// dawn and dusk within one sitting at the gallery's default demo length, and
/// short enough that the fast-forward multiplier below has something to be
/// fast relative to.
const SECONDS_PER_HOUR: f32 = 9.0;

/// Terrain a world cell can be. Only six kinds because the strategic map's
/// job is to be legible at a glance and to give the company somewhere rough
/// to be slowed by -- not to model a whole biome system.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Water,
    Marsh,
    Wood,
    Plain,
    Hills,
    Snow,
}

impl Terrain {
    /// Deterministic terrain at `(x, y)`: two independent noise fields
    /// (elevation, moisture) plus a latitude term for the snowline, all pure
    /// functions of the coordinates and [`SEED`]. No stored terrain grid is
    /// needed anywhere in this file; every cell's terrain is recomputed from
    /// scratch whenever it is drawn, which is what makes the persistent
    /// `visited` bitmap the *only* per-cell state this demo has to keep.
    fn at(x: i32, y: i32) -> Self {
        let (fx, fy) = (x as f32 * 0.045, y as f32 * 0.045);
        let elevation = fbm(SEED, fx, fy, 4, 0.5);
        let moisture = fbm(SEED ^ 0x51A7_2B33, fx * 1.3, fy * 1.3, 3, 0.55);
        // 0 at the top edge, 1 at the bottom: the reference screenshot's
        // snowline runs along its northern edge, so this demo's "north" (row
        // 0) gets the same treatment rather than a snowline at a random
        // latitude that would not read as a compass direction at all.
        let lat = y as f32 / WORLD_H as f32;

        if elevation < 0.30 {
            Self::Water
        } else if lat < 0.16 && elevation > 0.34 {
            Self::Snow
        } else if elevation > 0.72 {
            Self::Hills
        } else if moisture > 0.68 && elevation < 0.46 {
            Self::Marsh
        } else if moisture > 0.56 {
            Self::Wood
        } else {
            Self::Plain
        }
    }

    const fn glyph(self) -> char {
        match self {
            Self::Water => tterrain::WATER,
            Self::Marsh => tterrain::MARSH,
            Self::Wood => tterrain::CONIFER,
            Self::Plain => tterrain::GRASS,
            Self::Hills => tterrain::HILLS,
            Self::Snow => tterrain::SNOW,
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Water => rgb(28, 62, 98),
            Self::Marsh => rgb(88, 98, 58),
            Self::Wood => rgb(38, 64, 40),
            Self::Plain => rgb(94, 112, 58),
            Self::Hills => rgb(122, 108, 74),
            Self::Snow => rgb(214, 220, 230),
        }
    }

    /// Whether standing on this terrain halves the company's speed.
    const fn rough(self) -> bool {
        matches!(self, Self::Marsh | Self::Wood | Self::Hills)
    }

    /// Whether the company can enter this terrain at all. Only open water is
    /// blocked -- a company on foot fords marsh and climbs hills slowly, but
    /// does not swim a river on a whim.
    const fn passable(self) -> bool {
        !matches!(self, Self::Water)
    }
}

/// A settlement's house: the charge drawn on its banner and the field color
/// it sits on. Five houses, not one per settlement, because the reference
/// screenshot's own map repeats one dominant house's banner across most of
/// its towns and reserves a second house for a couple of others -- a banner
/// reads as *identity*, not as a unique snowflake per dot on the map.
struct House {
    charge: char,
    field: Color,
    ink: Color,
}

/// The charges are drawn from the CP437 suit glyphs and two marker glyphs
/// already in the gallery's shared vocabulary
/// ([`tilekit::glyphs::marker`]), rather than invented pictograms: every one
/// of them survives the CP437-only pixel backends, which a hand-drawn wolf's
/// head silhouette could not.
const HOUSES: [House; 3] = [
    // The dominant house: a pale field, wolf-grey ink, spade charge -- the
    // silver-and-black wolf banner the reference screenshot repeats across
    // most of its towns.
    House {
        charge: '\u{2660}', // spade
        field: rgb(150, 150, 158),
        ink: rgb(18, 18, 22),
    },
    // The second house: green field, the reference's other recurring banner
    // color (Tannenweiler, Wolfenstein).
    House {
        charge: '\u{2663}', // club
        field: rgb(58, 104, 62),
        ink: rgb(224, 232, 220),
    },
    // A minor third house for variety at the map's edges.
    House {
        charge: '\u{2666}', // diamond
        field: rgb(120, 66, 58),
        ink: rgb(230, 216, 196),
    },
];

/// One settlement: a fixed world position, a house, and a name drawn in
/// caps beneath its banner, matching the reference screenshot's label style
/// exactly.
struct Settlement {
    name: &'static str,
    house: usize,
    x: i32,
    y: i32,
}

/// The map's landmarks. Names and rough layout borrow directly from the
/// reference screenshot (Lonneberg in the north, Tannenweiler and
/// Wolfenstein as the odd house out in the southwest, a lake splitting the
/// map roughly down its middle) so a player who knows Battle Brothers'
/// worldmap recognizes the shape of this one.
const SETTLEMENTS: [Settlement; 9] = [
    Settlement {
        name: "LONNEBERG",
        house: 0,
        x: 62,
        y: 10,
    },
    Settlement {
        name: "BIRKETORN",
        house: 0,
        x: 96,
        y: 18,
    },
    Settlement {
        name: "TONDER",
        house: 0,
        x: 122,
        y: 26,
    },
    Settlement {
        name: "MOOSBURG",
        house: 0,
        x: 24,
        y: 34,
    },
    Settlement {
        name: "STOHLHOVEN",
        house: 0,
        x: 88,
        y: 42,
    },
    Settlement {
        name: "GROLLFESTE",
        house: 0,
        x: 50,
        y: 48,
    },
    Settlement {
        name: "TANNENWEILER",
        house: 1,
        x: 18,
        y: 60,
    },
    Settlement {
        name: "WOLFENSTEIN",
        house: 1,
        x: 64,
        y: 72,
    },
    Settlement {
        name: "GRAFENHEIDE",
        house: 2,
        x: 102,
        y: 62,
    },
];

/// Roster of named brothers, for the strength readout's roster peek. Just
/// enough to fill a small overlay list -- the roster screen itself is
/// [`36_court_reigns`](../36_court_reigns)'s and this batch's other
/// party-management demos' subject, not this one's.
const ROSTER: [&str; 12] = [
    "Grymm", "Adso", "Halbard", "Ivo", "Reinke", "Osric", "Bertram", "Wilbur", "Conrad", "Feodor",
    "Alaric", "Hanno",
];

/// Company strength: currently fielded over the roster's authorized maximum.
const STRENGTH: (u32, u32) = (STRENGTH_CURRENT, 20);
const STRENGTH_CURRENT: u32 = ROSTER.len() as u32;

/// How fast the clock runs. Three states rather than a continuous slider
/// because the reference screenshot's own control cluster is exactly this:
/// a pause button, a play button, and a fast-forward button, nothing
/// in-between.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeScale {
    Paused,
    Play,
    Fast,
}

impl TimeScale {
    const fn multiplier(self) -> f32 {
        match self {
            Self::Paused => 0.0,
            Self::Play => 1.0,
            Self::Fast => 3.0,
        }
    }
}

/// What a tap or a key press can resolve to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    /// The open map background: a tap here that hits nothing more specific
    /// becomes a destination (or closes an open panel; see
    /// [`CompanyRoad::apply_tap`]).
    Map,
    Pause,
    Play,
    Fast,
    RosterBadge,
    Settlement(usize),
}

/// State: the clock, the company's continuous position, its exploration
/// memory, and whatever overlay (a settlement's info panel, the roster peek)
/// is currently open.
pub struct CompanyRoad {
    game_hours: f32,
    /// Elapsed real seconds, independent of [`Self::time_scale`]. Drives the
    /// cursor blink and the dial's idle animation, which must keep moving
    /// even while the clock itself is paused -- otherwise pausing the game
    /// would also, misleadingly, freeze the UI chrome that lets you unpause
    /// it.
    ui_time: f32,
    time_scale: TimeScale,
    /// The company's position in continuous world coordinates. Kept as
    /// `f32`, not a cell, because the whole point of "pausable real time" is
    /// that the company can be a third of the way across a cell when you
    /// hit pause.
    company: (f32, f32),
    /// Index into [`SETTLEMENTS`] the standing patrol route is currently
    /// walking toward, used whenever [`Self::destination`] is `None`.
    heading: usize,
    /// A player-set destination, overriding the patrol route until reached.
    destination: Option<(f32, f32)>,
    /// Keyboard destination cursor, in world cells -- the arrow-key
    /// equivalent of a fingertip, see [`Self::handle_key`].
    cursor: (i32, i32),
    /// One bit of memory per world cell: has the company's vision disc ever
    /// covered it. `Vec<bool>` rather than a `HashSet` of visited
    /// coordinates because the whole world is a fixed, small, dense grid --
    /// a hash set would cost more per lookup for no benefit and would also
    /// invite the nondeterministic iteration order the project's rules
    /// forbid outright.
    visited: Vec<bool>,
    selected: Option<usize>,
    show_roster: bool,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for CompanyRoad {
    fn default() -> Self {
        let start = (SETTLEMENTS[0].x as f32 + 5.0, SETTLEMENTS[0].y as f32 + 6.0);
        let mut state = Self {
            // Day 3, afternoon: the same clock reading the reference
            // screenshot shows, so the very first frame already looks like a
            // session in progress rather than a fresh start.
            game_hours: 24.0f32.mul_add(2.0, 13.5),
            ui_time: 0.0,
            time_scale: TimeScale::Play,
            company: start,
            heading: 1,
            destination: None,
            cursor: (start.0.round() as i32, start.1.round() as i32),
            visited: vec![false; (WORLD_W * WORLD_H) as usize],
            selected: None,
            show_roster: false,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        };

        // Three days into the contract, the local roads are already charted:
        // every settlement is known (a small bubble of memory around each),
        // plus a wide bubble around the company's own start. That is why the
        // first frame reads like the reference screenshot's mid-session map
        // -- mostly explored with cloud only at the far edges -- rather than
        // an empty map that has to be walked from scratch before it means
        // anything.
        for settlement in &SETTLEMENTS {
            state.mark_visited_disc(settlement.x as f32 + 0.5, settlement.y as f32 + 0.5, 6.0);
        }
        state.mark_visited_disc(state.company.0, state.company.1, 24.0);
        state
    }
}

impl CompanyRoad {
    const fn world_index(x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= WORLD_W || y >= WORLD_H {
            return None;
        }
        Some((y * WORLD_W + x) as usize)
    }

    fn is_visited(&self, x: i32, y: i32) -> bool {
        Self::world_index(x, y).is_some_and(|i| self.visited[i])
    }

    /// Marks every cell within `radius` world-units of `(cx, cy)` as visited,
    /// using the same aspect-corrected metric as [`Self::vis_dist`] so the
    /// remembered region left behind matches the disc's own shape exactly --
    /// a memory footprint that were a raw-cell circle would grow visibly
    /// egg-shaped as the company walked past it.
    fn mark_visited_disc(&mut self, cx: f32, cy: f32, radius: f32) {
        let span = radius.ceil() as i32 + 1;
        let (icx, icy) = (cx.floor() as i32, cy.floor() as i32);
        for y in (icy - span)..=(icy + span) {
            for x in (icx - span)..=(icx + span) {
                let dx = cx - (x as f32 + 0.5);
                let dy = (cy - (y as f32 + 0.5)) * 2.0;
                if dx.mul_add(dx, dy * dy) > radius * radius {
                    continue;
                }
                if let Some(i) = Self::world_index(x, y) {
                    self.visited[i] = true;
                }
            }
        }
    }

    /// Aspect-corrected distance from the company to world cell `(x, y)`, in
    /// column-units. Cells are twice as tall as they are wide (see
    /// `ui::touch`'s own derivation of that ratio from the pixel backends'
    /// cell geometry), so a row of vertical distance covers twice the
    /// physical space a column of horizontal distance does; doubling `dy`
    /// before combining the two is what makes the disc this produces read as
    /// round rather than as a vertically flattened ellipse.
    fn vis_dist(company: (f32, f32), x: i32, y: i32) -> f32 {
        let dx = company.0 - (x as f32 + 0.5);
        let dy = (company.1 - (y as f32 + 0.5)) * 2.0;
        dx.mul_add(dx, dy * dy).sqrt()
    }

    /// Fraction of a full day cycle that has elapsed, `0.0` at midnight.
    fn day_fraction(hours: f32) -> f32 {
        (hours.rem_euclid(24.0)) / 24.0
    }

    /// `1.0` at noon, `0.0` at midnight, smoothly in between. One cosine
    /// rather than the four discrete [`tilekit::palette::TimeOfDay`] phases:
    /// those are designed for a per-scene palette swap, but this map's tint,
    /// vision radius, and dial position all need to move continuously with
    /// the clock, not jump at four fixed hours.
    fn daylight(hours: f32) -> f32 {
        let angle = (Self::day_fraction(hours) - 0.5) * TAU;
        0.5 * (1.0 + angle.cos())
    }

    /// The vision disc's radius in world-units (already the aspect-corrected
    /// column metric [`Self::vis_dist`] uses). Shrinks toward
    /// [`NIGHT_RADIUS`] as [`Self::daylight`] falls toward zero -- the
    /// legible consequence of pausing after dark that the brief calls for.
    fn vision_radius(daylight: f32) -> f32 {
        const NIGHT_RADIUS: f32 = 9.0;
        const DAY_RADIUS: f32 = 18.0;
        (DAY_RADIUS - NIGHT_RADIUS).mul_add(daylight, NIGHT_RADIUS)
    }

    fn phase_label(hours: f32) -> &'static str {
        match hours.rem_euclid(24.0) as u32 {
            5..=6 => "Dawn",
            7..=11 => "Morning",
            12..=17 => "Afternoon",
            18..=20 => "Dusk",
            _ => "Night",
        }
    }

    const fn day_number(hours: f32) -> u32 {
        (hours / 24.0) as u32 + 1
    }

    /// Resources depleted by a pure function of elapsed game-days rather than
    /// an accumulator ticked every frame: the starting figures match the
    /// reference screenshot exactly (2,434 crowns, 35 food, 80 ammunition, 30
    /// medicine), and deriving the current values from `game_hours` means
    /// pausing the clock also pauses supply consumption for free, with no
    /// separate "don't drain while paused" branch to keep in sync.
    fn resources(&self) -> [(&'static str, i32); 5] {
        let days = self.game_hours / 24.0;
        [
            ("Crowns", (2434.0 - days * 38.0).max(0.0) as i32),
            ("Food", (35.0 - days * 2.6).max(0.0) as i32),
            ("Ammo", (80.0 - days * 4.5).max(0.0) as i32),
            ("Tools", 40),
            ("Medicine", 30),
        ]
    }

    /// The point the company is currently walking toward: the player's
    /// override if one is set, otherwise the next stop on the standing
    /// patrol route.
    fn destination_point(&self) -> (f32, f32) {
        self.destination.unwrap_or_else(|| {
            let s = &SETTLEMENTS[self.heading];
            (s.x as f32 + 0.5, s.y as f32 + 0.5)
        })
    }

    /// Advances the clock, the company's position, and its exploration
    /// memory by `dt` real seconds. Pausing sets the multiplier to zero, so
    /// every one of these effects legitimately stops -- there is no separate
    /// "paused" branch to keep in sync with this one.
    fn simulate(&mut self, dt: f32) {
        let mult = self.time_scale.multiplier();
        self.game_hours += dt * mult / SECONDS_PER_HOUR;
        if mult > 0.0 {
            self.advance_company(dt * mult);
        }
        let daylight = Self::daylight(self.game_hours);
        let radius = Self::vision_radius(daylight);
        // The reveal radius is generously larger than the live vision
        // radius (plus [`VISION_SOFT`]'s own band) so that every cell the
        // soft edge could possibly blend toward "lit" this frame is already
        // marked visited -- otherwise the soft edge would visibly flicker
        // between "remembered" and "lit" as the disc's radius itself
        // breathes with the day/night cycle.
        self.mark_visited_disc(self.company.0, self.company.1, radius + VISION_SOFT + 1.0);
    }

    fn advance_company(&mut self, dt: f32) {
        let (tx, ty) = self.destination_point();
        let (cx, cy) = self.company;
        let (dx, dy) = (tx - cx, ty - cy);
        let dist = dx.hypot(dy);
        if dist <= ARRIVE_DIST {
            if self.destination.take().is_some() {
                // A player-set order was fulfilled; the standing patrol
                // route resumes from wherever it was pointed before the
                // detour, rather than restarting from the nearest stop --
                // the company was given an errand, not reassigned.
            } else {
                self.heading = (self.heading + 1) % SETTLEMENTS.len();
            }
            return;
        }

        let terrain = Terrain::at(cx.floor() as i32, cy.floor() as i32);
        let speed = if terrain.rough() {
            BASE_SPEED * ROUGH_FACTOR
        } else {
            BASE_SPEED
        };
        let step = (speed * dt).min(dist);
        let (ux, uy) = (dx / dist, dy / dist);
        let next = (ux.mul_add(step, cx), uy.mul_add(step, cy));
        if Terrain::at(next.0.floor() as i32, next.1.floor() as i32).passable() {
            self.company = next;
        } else {
            // Open water ahead: the company cannot ford it, so the order is
            // dropped rather than left to push uselessly against the
            // shoreline every frame. The patrol route resumes on the next
            // tick, same as a fulfilled order above.
            self.destination = None;
        }
    }

    /// Sets a new destination at world cell `(x, y)`, unless it is open
    /// water. Shared by the tap handler and the keyboard cursor's Enter
    /// binding, so both input paths are guaranteed to agree on what counts
    /// as a legal order.
    fn set_destination(&mut self, x: i32, y: i32) {
        if Terrain::at(x, y).passable() {
            self.destination = Some((x as f32 + 0.5, y as f32 + 0.5));
        }
    }

    // ── Input ────────────────────────────────────────────────────────────

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.cursor.1 -= 1,
            KeyCode::Down => self.cursor.1 += 1,
            KeyCode::Left => self.cursor.0 -= 1,
            KeyCode::Right => self.cursor.0 += 1,
            KeyCode::Enter => {
                let (cx, cy) = self.cursor;
                if let Some(i) = SETTLEMENTS.iter().position(|s| s.x == cx && s.y == cy) {
                    self.selected = Some(i);
                } else {
                    self.set_destination(cx, cy);
                }
            }
            KeyCode::Tab => {
                let next = self.selected.map_or(0, |i| (i + 1) % SETTLEMENTS.len());
                self.selected = Some(next);
                let s = &SETTLEMENTS[next];
                self.cursor = (s.x, s.y);
            }
            KeyCode::Char(' ') => {
                self.time_scale = if self.time_scale == TimeScale::Paused {
                    TimeScale::Play
                } else {
                    TimeScale::Paused
                };
            }
            KeyCode::Char('f' | 'F') => {
                self.time_scale = if self.time_scale == TimeScale::Fast {
                    TimeScale::Play
                } else {
                    TimeScale::Fast
                };
            }
            KeyCode::Char('r' | 'R') => self.show_roster = !self.show_roster,
            KeyCode::Char('c' | 'C') => {
                self.selected = None;
                self.show_roster = false;
            }
            _ => {}
        }
        self.cursor.0 = self.cursor.0.clamp(0, WORLD_W - 1);
        self.cursor.1 = self.cursor.1.clamp(0, WORLD_H - 1);
    }

    /// Resolves this frame's tap (if any) against the hotspots built during
    /// layout. Built and consumed once per frame so a tap always lands
    /// against the geometry that was actually just drawn, never a stale one
    /// from the previous size.
    fn apply_tap(&mut self, pos: Pos, scroll: (i32, i32)) {
        match self.hotspots.hit(pos) {
            Some(Action::Pause) => self.time_scale = TimeScale::Paused,
            Some(Action::Play) => self.time_scale = TimeScale::Play,
            Some(Action::Fast) => self.time_scale = TimeScale::Fast,
            Some(Action::RosterBadge) => self.show_roster = !self.show_roster,
            Some(&Action::Settlement(i)) => {
                self.selected = if self.selected == Some(i) {
                    None
                } else {
                    Some(i)
                };
            }
            Some(Action::Map) => {
                // A tap on the open map first dismisses whatever overlay is
                // showing (the finger that opened a panel is also the
                // fastest way to close it); only once nothing is open does a
                // map tap set a destination, so a fat-fingered tap meant to
                // close a panel can never also send the company somewhere.
                if self.selected.take().is_some() || core::mem::take(&mut self.show_roster) {
                    return;
                }
                let x = scroll.0 + i32::from(pos.x);
                let y = scroll.1 + i32::from(pos.y);
                self.cursor = (x.clamp(0, WORLD_W - 1), y.clamp(0, WORLD_H - 1));
                self.set_destination(x, y);
            }
            None => {}
        }
    }

    // ── Layout ───────────────────────────────────────────────────────────

    /// The scroll offset (world cell shown at the map area's top-left
    /// corner) that keeps the company centred. Shared by the map painter and
    /// the tap handler so a tap is always converted back to the same world
    /// cell it was drawn over.
    fn scroll_for(&self, area: Rect) -> (i32, i32) {
        (
            self.company.0.round() as i32 - i32::from(area.width()) / 2,
            self.company.1.round() as i32 - i32::from(area.height()) / 2,
        )
    }

    /// The three time-control buttons' rects, left to right, sized to the
    /// touch minimum and centred in `at_center` around `y`.
    const fn control_rects(area: Rect, y: u16) -> [Rect; 3] {
        let total = TAP_W * 3 + 2;
        let x0 = area.left() + (area.width().saturating_sub(total)) / 2;
        [
            Rect::new(x0, y, TAP_W, TAP_H),
            Rect::new(x0 + TAP_W + 1, y, TAP_W, TAP_H),
            Rect::new(x0 + (TAP_W + 1) * 2, y, TAP_W, TAP_H),
        ]
    }

    /// Where the control row sits: directly under the day panel on
    /// landscape/desktop, but down in the bottom thumb zone on portrait,
    /// per the brief's mobile requirement that primary controls never sit
    /// under a thumb reaching all the way to the top of a tall phone screen.
    fn control_row_y(area: Rect, shape: Shape) -> u16 {
        if shape == Shape::Portrait {
            area.bottom().saturating_sub(TAP_H + 1)
        } else {
            area.top() + 4
        }
    }

    /// Registers every tappable region for this frame and returns the areas
    /// the draw pass needs (the resource panel, day panel, and roster badge
    /// rects), so layout is computed exactly once and both hit-testing and
    /// drawing agree on it.
    fn build_hotspots(&mut self, area: Rect, shape: Shape) {
        self.hotspots.clear();
        self.hotspots.push(area, Action::Map);

        let scroll = self.scroll_for(area);
        for (i, settlement) in SETTLEMENTS.iter().enumerate() {
            if !self.is_visited(settlement.x, settlement.y) {
                continue;
            }
            let sx = settlement.x - scroll.0;
            let sy = settlement.y - scroll.1;
            if sx < -3
                || sy < -3
                || sx > i32::from(area.width()) + 3
                || sy > i32::from(area.height()) + 3
            {
                continue;
            }
            let rect = Rect::new(
                (i32::from(area.left()) + sx - 2).max(0) as u16,
                (i32::from(area.top()) + sy - 2).max(0) as u16,
                5,
                4,
            );
            self.hotspots
                .push_tappable(rect, area, Action::Settlement(i));
        }

        let [pause, play, fast] = Self::control_rects(area, Self::control_row_y(area, shape));
        self.hotspots.push(pause, Action::Pause);
        self.hotspots.push(play, Action::Play);
        self.hotspots.push(fast, Action::Fast);

        let badge = Self::roster_badge_rect(area, shape);
        self.hotspots
            .push_tappable(badge, area, Action::RosterBadge);
    }

    fn roster_badge_rect(area: Rect, shape: Shape) -> Rect {
        let w = 12.max(TAP_W);
        let h = TAP_H;
        let x = area.right().saturating_sub(w + 1);
        let y = if shape == Shape::Portrait {
            area.top() + 6
        } else {
            area.top() + 1
        };
        Rect::new(x, y, w, h)
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let daylight = Self::daylight(self.game_hours);
        let radius = Self::vision_radius(daylight);
        let scroll = self.scroll_for(area);
        // Deep blue at night, white at noon: mixed into every lit cell's
        // color below, so the whole visible map breathes with the clock
        // rather than only the sky (which this map, drawn top-down, has no
        // room to show separately).
        let night_tint = rgb(18, 26, 52);

        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let wx = scroll.0 + i32::from(sx);
                let wy = scroll.1 + i32::from(sy);
                let at = (area.left() + sx, area.top() + sy);
                let in_bounds = wx >= 0 && wy >= 0 && wx < WORLD_W && wy < WORLD_H;

                if !in_bounds || !self.is_visited(wx, wy) {
                    // Never explored (or off the edge of the county
                    // entirely): a stable cloud texture, hashed rather than
                    // random so it does not reshuffle from frame to frame.
                    let dense = hash01(CLOUD_SEED, wx, wy) > 0.55;
                    surface.put(
                        at,
                        if dense { '\u{2593}' } else { '\u{2591}' },
                        Style::new().fg(rgb(60, 62, 70)).bg(rgb(30, 31, 37)),
                    );
                    continue;
                }

                let terrain = Terrain::at(wx, wy);
                let dist = Self::vis_dist(self.company, wx, wy);
                let vis_t = ((radius - dist) / VISION_SOFT).clamp(0.0, 1.0);

                let base = terrain.color();
                let lit_shade = mix(base, night_tint, (1.0 - daylight) * 0.55);
                let lit_glow = mix(lit_shade, rgb(255, 255, 255), 0.22);
                let memory_shade = scale(base, 0.38);
                let memory_glow = scale(base, 0.55);

                let bg = mix(memory_shade, lit_shade, vis_t);
                let fg = mix(memory_glow, lit_glow, vis_t);
                surface.put(at, terrain.glyph(), Style::new().fg(fg).bg(bg));
            }
        }

        for (i, settlement) in SETTLEMENTS.iter().enumerate() {
            if self.is_visited(settlement.x, settlement.y) {
                Self::draw_settlement(surface, area, scroll, settlement, self.selected == Some(i));
            }
        }

        self.draw_destination_marker(surface, area, scroll);
        self.draw_company(surface, area, scroll);
        self.draw_cursor(surface, area, scroll);
    }

    fn draw_settlement(
        surface: &mut Surface<'_>,
        area: Rect,
        scroll: (i32, i32),
        settlement: &Settlement,
        focused: bool,
    ) {
        let house = &HOUSES[settlement.house];
        let sx = settlement.x - scroll.0;
        let sy = settlement.y - scroll.1;
        let put = |surface: &mut Surface<'_>, dx: i32, dy: i32, ch: char, style: Style| {
            let (x, y) = (sx + dx, sy + dy);
            if x < 0 || y < 0 || x >= i32::from(area.width()) || y >= i32::from(area.height()) {
                return;
            }
            surface.put((area.left() + x as u16, area.top() + y as u16), ch, style);
        };

        let border_color = if focused {
            rgb(250, 220, 130)
        } else {
            mix(house.field, rgb(255, 255, 255), 0.3)
        };
        let frame_style = Style::new().fg(border_color).bg(house.field);
        let charge_style = Style::new().fg(house.ink).bg(house.field);

        // A five-wide, three-tall shield: a top and bottom rule plus a
        // charge row between two side rules, exactly wide enough to hold
        // one charge glyph with breathing room -- the "5x4 charge on a
        // field" the round-2 addendum names, with the fourth row spent on
        // the name below instead of more shield.
        put(surface, -2, -2, '\u{250C}', frame_style);
        put(surface, -1, -2, '\u{2500}', frame_style);
        put(surface, 0, -2, '\u{2500}', frame_style);
        put(surface, 1, -2, '\u{2500}', frame_style);
        put(surface, 2, -2, '\u{2510}', frame_style);
        put(surface, -2, -1, '\u{2502}', frame_style);
        put(surface, -1, -1, ' ', charge_style);
        put(surface, 0, -1, house.charge, charge_style);
        put(surface, 1, -1, ' ', charge_style);
        put(surface, 2, -1, '\u{2502}', frame_style);
        put(surface, -2, 0, '\u{2514}', frame_style);
        put(surface, -1, 0, '\u{2500}', frame_style);
        put(surface, 0, 0, '\u{2500}', frame_style);
        put(surface, 1, 0, '\u{2500}', frame_style);
        put(surface, 2, 0, '\u{2518}', frame_style);

        let name_color = if focused { rgb(250, 220, 130) } else { ui::FG };
        let name_style = Style::new().fg(name_color).bg(rgb(10, 10, 14));
        let start = -(settlement.name.chars().count() as i32) / 2;
        for (i, ch) in settlement.name.chars().enumerate() {
            put(surface, start + i as i32, 1, ch, name_style);
        }
    }

    fn draw_company(&self, surface: &mut Surface<'_>, area: Rect, scroll: (i32, i32)) {
        let x = self.company.0.round() as i32 - scroll.0;
        let y = self.company.1.round() as i32 - scroll.1;
        if x < 0 || y < 0 || x >= i32::from(area.width()) || y >= i32::from(area.height()) {
            return;
        }
        surface.put(
            (area.left() + x as u16, area.top() + y as u16),
            '\u{263B}', // marker::UNIT
            Style::new().fg(rgb(250, 200, 90)).bg(rgb(40, 26, 10)),
        );
    }

    fn draw_destination_marker(&self, surface: &mut Surface<'_>, area: Rect, scroll: (i32, i32)) {
        let Some((dx, dy)) = self.destination else {
            return;
        };
        let x = dx.round() as i32 - scroll.0;
        let y = dy.round() as i32 - scroll.1;
        if x < 0 || y < 0 || x >= i32::from(area.width()) || y >= i32::from(area.height()) {
            return;
        }
        surface.put(
            (area.left() + x as u16, area.top() + y as u16),
            'x',
            Style::new().fg(rgb(240, 120, 100)).bg(rgb(30, 14, 12)),
        );
    }

    /// The keyboard destination cursor: a blinking outline so its position
    /// is legible without smoothly animating (per the round-2 note that
    /// idle animation belongs on decoration, not on anything that has to
    /// stay pinned to an exact cell).
    fn draw_cursor(&self, surface: &mut Surface<'_>, area: Rect, scroll: (i32, i32)) {
        if !((self.ui_time * 1.6) as u32).is_multiple_of(2) {
            return;
        }
        let x = self.cursor.0 - scroll.0;
        let y = self.cursor.1 - scroll.1;
        if x < 0 || y < 0 || x >= i32::from(area.width()) || y >= i32::from(area.height()) {
            return;
        }
        surface.put(
            (area.left() + x as u16, area.top() + y as u16),
            '\u{253C}',
            Style::new().fg(rgb(255, 255, 255)).bg(rgb(0, 0, 0)),
        );
    }

    /// Width the resource strip may claim before it would run into the
    /// centred day panel. Both panels float over the map rather than
    /// splitting it into exclusive columns, so without this clamp a wide
    /// resource strip and the day panel visibly overlap on anything
    /// narrower than a desktop window -- the 80-column headless grid this
    /// gallery's snapshot tests pin included.
    fn resource_panel_max_width(area: Rect) -> u16 {
        let day = Self::day_rect(area);
        day.left().saturating_sub(area.left() + 2)
    }

    fn draw_resource_panel(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let two_rows = shape == Shape::Portrait;
        let height = if two_rows { 5 } else { 4 };
        let desired = if two_rows { 34 } else { 46 };
        let width = desired
            .min(area.width().saturating_sub(1))
            .min(Self::resource_panel_max_width(area));
        let rect = Rect::new(area.left() + 1, area.top() + 1, width, height);
        let inner = Panel::new().title("Company").draw(surface, rect);
        if inner.width() < 8 || inner.height() == 0 {
            return;
        }

        let resources = self.resources();
        let row0 = &resources[..3];
        let row1 = &resources[3..];

        // Each resource gets a dim label directly followed by an accented
        // value, printed as one pass rather than built through `Span` (which
        // wants one color per whole run, not an alternating label/value
        // pattern within a single call).
        let render_row = |surface: &mut Surface<'_>, y: u16, items: &[(&str, i32)]| {
            let mut x = inner.left();
            for (label, value) in items {
                let text = format!("{label} ");
                surface.print(
                    (x, inner.top() + y),
                    &text,
                    Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
                );
                x += text.chars().count() as u16;
                let value_text = format!("{value} ");
                surface.print(
                    (x, inner.top() + y),
                    &value_text,
                    Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
                );
                x += value_text.chars().count() as u16;
            }
        };

        if two_rows {
            render_row(surface, 0, row0);
            render_row(surface, 1, row1);
        } else {
            let mut all = Vec::with_capacity(5);
            all.extend_from_slice(row0);
            all.extend_from_slice(row1);
            render_row(surface, 0, &all);
        }

        let objective_y = if two_rows { 2 } else { 1 };
        if inner.height() > objective_y {
            let dest_name = if self.destination.is_some() {
                "the marked position"
            } else {
                SETTLEMENTS[self.heading].name
            };
            panel::spans(
                surface,
                (inner.left(), inner.top() + objective_y),
                inner.width(),
                &[
                    Span::dim("Contract: reach "),
                    Span::keyword(dest_name),
                    Span::dim(" before the ledger runs dry."),
                ],
                panel::PANEL_BG,
            );
        }
    }

    /// The day/time panel's rect: fixed width, centred across `area`. Also
    /// used by [`Self::resource_panel_max_width`], so the two panels'
    /// positions can never disagree about where the boundary between them
    /// falls.
    fn day_rect(area: Rect) -> Rect {
        let width = 21.min(area.width());
        let x = area.left() + (area.width().saturating_sub(width)) / 2;
        Rect::new(x, area.top() + 1, width, 3)
    }

    fn draw_day_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let rect = Self::day_rect(area);
        let inner = Panel::new().draw(surface, rect);
        if inner.width() < 8 || inner.height() == 0 {
            return;
        }

        let day = Self::day_number(self.game_hours);
        let phase = Self::phase_label(self.game_hours);
        let header = format!("Day {day} - {phase}");
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::keyword(&header)],
            panel::PANEL_BG,
        );

        if inner.height() > 1 {
            // A tiny arc dial: a track of dots with the sun (day) or moon
            // (night) riding along it in step with `day_fraction`, so the
            // dial's own position always agrees with the tint the map is
            // wearing and the radius the vision disc currently has.
            let track_w = inner.width().min(15);
            let frac = Self::day_fraction(self.game_hours);
            let pos = (frac * f32::from(track_w.saturating_sub(1))).round() as u16;
            let daylight = Self::daylight(self.game_hours);
            let icon = if daylight > 0.4 {
                '\u{263C}'
            } else {
                '\u{25D8}'
            };
            let y = inner.top() + 1;
            for i in 0..track_w {
                let (ch, color) = if i == pos {
                    (icon, ui::ACCENT)
                } else {
                    ('\u{00B7}', ui::DIM)
                };
                surface.put(
                    (inner.left() + i, y),
                    ch,
                    Style::new().fg(color).bg(panel::PANEL_BG),
                );
            }
        }
    }

    fn draw_controls(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let y = Self::control_row_y(area, shape);
        let [pause, play, fast] = Self::control_rects(area, y);
        let entries = [
            (pause, "||", TimeScale::Paused),
            (play, "\u{25BA}", TimeScale::Play),
            (fast, "\u{25BA}\u{25BA}", TimeScale::Fast),
        ];
        for (rect, label, scale_kind) in entries {
            let active = self.time_scale == scale_kind;
            let panel = Panel::new().focused(active);
            let inner = panel.draw(surface, rect);
            if inner.width() == 0 || inner.height() == 0 {
                continue;
            }
            let color = if active { ui::ACCENT } else { ui::FG };
            let cx = inner.left() + inner.width().saturating_sub(label.chars().count() as u16) / 2;
            let cy = inner.top() + inner.height() / 2;
            surface.print((cx, cy), label, Style::new().fg(color).bg(panel::PANEL_BG));
        }
    }

    fn draw_roster_badge(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let rect = Self::roster_badge_rect(area, shape);
        let inner = Panel::new()
            .title("Company")
            .focused(self.show_roster)
            .draw(surface, rect);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let text = format!("{}/{}", STRENGTH.0, STRENGTH.1);
        surface.print(
            (inner.left(), inner.top()),
            &text,
            Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
        );
    }

    fn draw_roster_peek(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        if !self.show_roster {
            return;
        }
        let badge = Self::roster_badge_rect(area, shape);
        let height = (ROSTER.len() as u16 + 2).min(area.height().saturating_sub(badge.bottom()));
        if height < 3 {
            return;
        }
        let rect = Rect::new(badge.left(), badge.bottom(), badge.width().max(14), height);
        let inner = Panel::new().title("Roster").draw(surface, rect);
        for (i, name) in ROSTER.iter().enumerate().take(inner.height_usize()) {
            surface.print(
                (inner.left(), inner.top() + i as u16),
                name,
                Style::new().fg(ui::FG).bg(panel::PANEL_BG),
            );
        }
    }

    fn draw_settlement_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let Some(index) = self.selected else {
            return;
        };
        let settlement = &SETTLEMENTS[index];
        let house = &HOUSES[settlement.house];

        let width = 34.min(area.width().saturating_sub(4));
        let height = 6.min(area.height().saturating_sub(4));
        if width < 10 || height < 4 {
            return;
        }
        let x = area.left() + (area.width() - width) / 2;
        let y = area.top() + (area.height() - height) / 2;
        let rect = Rect::new(x, y, width, height);

        let inner = Panel::new()
            .title(settlement.name)
            .border(panel::Border::Double)
            .frame(mix(house.field, rgb(255, 255, 255), 0.2))
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }

        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::dim("House charge: "),
                Span::new(&house.charge.to_string(), house.field),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 2 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[Span::plain("Recruiting grounds, friendly relations.")],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 4 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 4),
                inner.width(),
                &[Span::dim("Tap the banner again, or C, to close.")],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_hud(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        self.draw_resource_panel(surface, area, shape);
        self.draw_day_panel(surface, area);
        self.draw_controls(surface, area, shape);
        self.draw_roster_badge(surface, area, shape);
        self.draw_roster_peek(surface, area, shape);
        self.draw_settlement_panel(surface, area);
    }

    fn status(&self) -> String {
        let (cx, cy) = self.company;
        format!(
            "day {} pos ({:.0},{:.0})",
            Self::day_number(self.game_hours),
            cx,
            cy
        )
    }
}

impl Demo for CompanyRoad {
    const NAME: &'static str = "39_company_road";
    const TITLE: &'static str = "39 Company Road";
    const BLURB: &'static str =
        "Battle Brothers overland: banner settlements, a travelling vision disc, pausable time.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Arrows", "move cursor"),
            ("Enter", "set destination / open banner"),
            ("Tab", "cycle settlements"),
            ("Space", "pause/play"),
            ("F", "fast-forward"),
            ("R", "roster peek"),
            ("C", "close panel"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.ui_time += dt;
        self.fps.record(frame.delta);

        for event in term.drain_events() {
            self.pointer.feed(&event);
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let shape = Shape::of(content);

        self.build_hotspots(content, shape);
        let scroll = self.scroll_for(content);
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap {
            self.apply_tap(pos, scroll);
        }

        self.simulate(dt);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_map(&mut surface, content);
        self.draw_hud(&mut surface, content, shape);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(CompanyRoad);
