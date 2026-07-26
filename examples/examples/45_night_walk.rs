//! 45: Night walk -- a bestiary that fills in through repeated encounters, in
//! a world too dark to see clearly.
//!
//! Adapted from Traveller's Hymn: a top-down open-world RPG where the whole
//! screen reads as texture (dense grass, reed, and bramble tufts on a near
//! -black field) rather than as symbols, a circular dial tracks the time of
//! day, and the bottom corners carry the only two things the player actually
//! needs mid-walk -- their own condition, and whatever they last ran into.
//!
//! The element this demo is built around is the second half of that: a
//! creature card that starts almost entirely redacted (name partly blanked,
//! every stat line shown as a solid shaded bar) and reveals one more line
//! each time the species is met again. That is not a cosmetic flourish --
//! `redacted_name` and the block rows in [`NightWalk::draw_bestiary`] are the
//! actual game state (`bestiary: [u32; SPECIES_COUNT]`), not decoration
//! layered over it.
//!
//! Techniques on show:
//!
//! - **Progressive redaction** ([`redacted_name`], [`revealed_lore_count`]):
//!   text drawn as solid shaded blocks (`\u{2593}`) is a rare case where CP437
//!   gives an ASCII UI something a modern terminal font would not spell more
//!   clearly -- a redacted line does not look like a placeholder, it looks
//!   like classified information.
//! - **A circular dial with a rotating hand** ([`NightWalk::draw_dial`]): the
//!   dial is drawn as an ellipse rather than a circle in cell coordinates
//!   because a terminal cell is about twice as tall as it is wide, so a shape
//!   that should read as round on screen needs roughly twice the horizontal
//!   cell-radius as vertical. The hand's position is a continuous angle, but
//!   its sun/moon tip glyph switches in one step at the horizon rather than
//!   cross-fading, because a glyph is text and two-state steps are what keep
//!   text legible (see the module docs on `ui::touch` for the aspect-ratio
//!   arithmetic this reuses).
//! - **Deterministic textural terrain** ([`biome_glyph`], [`biome_at`]): each
//!   biome tufts a small motif pool across every cell of a multi-cell tile
//!   using a coordinate hash, and two of the five biomes additionally read a
//!   hashed per-cell phase against the demo clock so reeds sway and water
//!   glints without ever reshuffling which cell holds which motif.
//! - **A full day/night tint pass** ([`daylight`], [`tint_for_time`]): every
//!   terrain color is scaled and cast toward blue by how far the clock sits
//!   from noon, so the whole palette visibly lifts at dawn instead of the
//!   demo just swapping a "night" flag.
//! - **Tap-or-drag walking and camera clamping** ([`NightWalk::camera_for`],
//!   [`ui::touch::Pointer`], [`ui::touch::Hotspots`]): the world is larger
//!   than any viewport this gallery targets, so the camera follows the
//!   player and clamps to the map edge rather than showing void past it.
//!
//! ```sh
//! cargo run --example 45_night_walk --features crossterm
//! cargo run --example 45_night_walk --features software
//! cargo run --example 45_night_walk --features gl
//! cargo run --example 45_night_walk  # headless, prints a few frames
//! ```

use core::f32::consts::{FRAC_PI_2, TAU};

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::wrap;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape, TAP_W};
use ascii_tile_demos::ui::{self, ACCENT, DIM, FG};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, fbm, hash01};
use tilekit::palette::{mix, rgb, scale};

/// Width of one terrain tile in cells. At least 6x3 per the brief, so a tile
/// reads as a patch of ground rather than a single glyph pretending to be
/// one; 7 gives the motif pool room to avoid an obvious 2-cell repeat.
const TILE_W: i32 = 7;
/// See [`TILE_W`].
const TILE_H: i32 = 4;

/// World size in tiles. Chosen so the world in cells (182x64) comfortably
/// outsizes every viewport this gallery targets (down to the 80x24 headless
/// grid), which is what makes the camera-clamping in [`NightWalk::camera_for`]
/// meaningful rather than a no-op.
const WORLD_W: i32 = 26;
/// See [`WORLD_W`].
const WORLD_H: i32 = 16;

/// Arbitrary nonzero seed for every noise call in this file. One constant
/// rather than a per-call literal so the whole terrain can be reseeded (for
/// a variant build) by changing one number.
const SEED: u32 = 0x4E49_4748;

/// Game-hours the clock advances per real second.
///
/// Fast enough that a full day/night cycle (and the palette lift at dawn) is
/// visible within a short capture or playtest, which is the whole point of
/// an idle demo: nobody is going to sit through a realistic 24-hour cycle to
/// see the one thing this file exists to show.
const HOURS_PER_SECOND: f32 = 0.6;

/// Number of playable species. A `const` rather than `SPECIES.len()` because
/// it is needed to size `NightWalk::bestiary` before `SPECIES` is in scope of
/// the type declaration.
const SPECIES_COUNT: usize = 4;

/// One terrain biome. Each gets its own motif pool and color cast in
/// [`biome_glyph`], which is what makes the map read as distinct textures
/// (forest, grass, bramble, reed, water) rather than one undifferentiated
/// noise field.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Biome {
    Forest,
    Grass,
    Bramble,
    Reed,
    Water,
}

/// Picks the biome at tile `(tx, ty)`.
///
/// A pure function of tile coordinates rather than a stored grid: the world
/// is only ever read a viewport's worth of tiles at a time, and recomputing a
/// cheap noise sample is less code (and less state to keep deterministic)
/// than generating and storing 400-odd tiles up front for a demo that never
/// changes its own terrain.
///
/// `wet` blends the noise field with an west-to-east gradient so the map
/// reads, coarsely, as dry forest and grass in the west shading into wetland
/// reed and open water in the east -- the same "the far side of the map looks
/// like a different place" shape Traveller's Hymn's own screenshot shows
/// (dark treeline stage-left, bright reed-and-water stage-right), with the
/// noise keeping every boundary irregular rather than a straight seam.
fn biome_at(tx: i32, ty: i32) -> Biome {
    let base = fbm(SEED, tx as f32 * 0.14, ty as f32 * 0.14, 4, 0.5);
    let east = tx as f32 / WORLD_W as f32;
    let wet = base.mul_add(0.6, east * 0.4);
    if wet < 0.24 {
        Biome::Forest
    } else if wet < 0.42 {
        Biome::Grass
    } else if wet < 0.58 {
        Biome::Bramble
    } else if wet < 0.78 {
        Biome::Reed
    } else {
        Biome::Water
    }
}

/// Renders one world cell of `biome` at `(wx, wy)`, before the day/night tint
/// in [`tint_for_time`] is applied.
///
/// Reed and water are the two biomes given a time-dependent term (`sway`,
/// `glint`): each cell gets a phase from a coordinate hash so neighbouring
/// cells drift out of sync with each other, then that phase is added to the
/// shared clock `time`, which is what makes a field of reeds read as swaying
/// in a light wind rather than as a per-cell flicker. Every other biome is a
/// pure function of position, which is deliberately cheaper and reads as
/// still ground.
fn biome_glyph(biome: Biome, wx: i32, wy: i32, time: f32) -> (char, Color, Color) {
    let h = hash01(SEED ^ 0x51, wx, wy);
    let phase = hash01(SEED ^ 0x52, wx, wy) * TAU;
    match biome {
        Biome::Forest => {
            let bg = rgb(6, 9, 6);
            if h < 0.12 {
                ('|', rgb(96, 70, 52), bg)
            } else if h < 0.4 {
                ('"', rgb(42, 66, 42), bg)
            } else if h < 0.7 {
                (':', rgb(34, 54, 34), bg)
            } else {
                ('.', rgb(26, 42, 28), bg)
            }
        }
        Biome::Grass => {
            let bg = rgb(5, 8, 4);
            let sway = time.mul_add(0.6, phase).sin();
            let glyph = if h < 0.3 {
                ','
            } else if h < 0.6 {
                '`'
            } else if h < 0.85 {
                '"'
            } else {
                '.'
            };
            let bright = 0.15_f32.mul_add(sway, 0.85);
            (glyph, scale(rgb(96, 120, 62), bright), bg)
        }
        Biome::Bramble => {
            let bg = rgb(8, 5, 10);
            let glyph = if h < 0.15 {
                '+'
            } else if h < 0.45 {
                'x'
            } else if h < 0.75 {
                '*'
            } else {
                '%'
            };
            (glyph, rgb(128, 98, 134), bg)
        }
        Biome::Reed => {
            let bg = rgb(4, 10, 9);
            let sway = time.mul_add(0.9, phase).sin();
            let glyph = if sway > 0.2 {
                '|'
            } else if sway > -0.2 {
                '\''
            } else {
                ':'
            };
            (glyph, rgb(80, 152, 120), bg)
        }
        Biome::Water => {
            let bg = rgb(3, 7, 14);
            let glint = time.mul_add(1.4, phase).sin();
            if glint > 0.9 {
                ('*', rgb(192, 222, 240), bg)
            } else {
                let glyph = if h < 0.5 { '~' } else { ':' };
                let shimmer = 0.25_f32.mul_add(f32::midpoint(glint, 1.0), 0.75);
                (glyph, scale(rgb(74, 126, 178), shimmer), bg)
            }
        }
    }
}

/// Daylight fraction (0 at midnight, 1 at noon) for hour-of-day `time_of_day`.
///
/// A raised cosine rather than a step: the terrain tint and the mind drain
/// both read this, and a hard day/night cutoff would make both change in one
/// visible jump at a fixed minute, which reads as a bug rather than as dusk.
fn daylight(time_of_day: f32) -> f32 {
    let frac = time_of_day / 24.0;
    let c = ((frac - 0.5) * TAU).cos();
    f32::midpoint(c, 1.0).powf(0.8)
}

/// Applies the day/night pass to a terrain color pair.
///
/// Brightness scales toward black at night (never fully, so the map stays
/// legible enough to still be a game) and the foreground additionally mixes
/// toward a cool blue, which is what sells "night" as a color cast rather
/// than as the same picture with the lights turned down.
fn tint_for_time(fg: Color, bg: Color, daylight: f32) -> (Color, Color) {
    let brightness = 0.84_f32.mul_add(daylight, 0.16);
    let night = 1.0 - daylight;
    let fg = scale(fg, brightness);
    let fg = mix(fg, rgb(60, 80, 150), night * 0.3);
    let bg = scale(bg, 0.6_f32.mul_add(brightness, 0.3));
    (fg, bg)
}

/// A creature species: what it looks like on the map and what its bestiary
/// card eventually says once enough of `lore` has been revealed.
struct Species {
    name: &'static str,
    glyph: char,
    color: Color,
    /// Three lines, revealed one per encounter after the first. See
    /// [`revealed_lore_count`].
    lore: [&'static str; 3],
}

const SPECIES: [Species; SPECIES_COUNT] = [
    Species {
        name: "Moonlight Spider",
        glyph: '\u{03c6}', // phi
        color: rgb(200, 215, 235),
        lore: [
            "Threat: low, but relentless once it senses warmth.",
            "Seen most often where bramble meets reed.",
            "Its web glimmers faintly under moonlight.",
        ],
    },
    Species {
        name: "Marsh Toad",
        glyph: '\u{03b4}', // delta
        color: rgb(140, 190, 120),
        lore: [
            "Threat: negligible; startles more than it harms.",
            "Keeps to standing water and reed banks.",
            "Croaks loudest just before the weather turns.",
        ],
    },
    Species {
        name: "Reed Wisp",
        glyph: '\u{03a9}', // omega
        color: rgb(120, 220, 210),
        lore: [
            "Threat: moderate; drains warmth from a lantern.",
            "Drifts wherever the reeds grow thickest.",
            "Follows a light for hours before it tires.",
        ],
    },
    Species {
        name: "Bramble Hound",
        glyph: '\u{2229}', // intersection, a hunched silhouette at this size
        color: rgb(196, 120, 90),
        lore: [
            "Threat: moderate; hunts in the deepest dark.",
            "Dens beneath the thorniest bramble.",
            "Goes quiet the moment it has your scent.",
        ],
    },
];

/// Redacts `name` according to how many times its species has been met.
///
/// Zero encounters blanks the whole name; one reveals the first half; two or
/// more reveals it in full. Spaces are never blanked (a fully redacted two-
/// word name should still show its word break, the same way a censored
/// document leaves the gaps between words visible), which is what keeps a
/// redacted "Moonlight Spider" reading as two blocks rather than one solid
/// bar that could be any length.
fn redacted_name(name: &str, encounters: u32) -> String {
    let total = name.chars().count();
    let reveal = match encounters {
        0 => 0,
        1 => total / 2,
        _ => total,
    };
    name.chars()
        .enumerate()
        .map(|(i, c)| {
            if c == ' ' || i < reveal {
                c
            } else {
                '\u{2593}'
            }
        })
        .collect()
}

/// How many of a species' three lore lines are revealed at `encounters`.
///
/// The first encounter is spent revealing the name (see [`redacted_name`]),
/// so lore only starts unlocking from the second encounter on -- each further
/// encounter reveals exactly one more line, which is the mechanic the brief
/// asks for built as real state rather than as a fixed "met once" flag.
const fn revealed_lore_count(encounters: u32) -> usize {
    let n = encounters.saturating_sub(1);
    if n > 3 { 3 } else { n as usize }
}

/// A creature wandering the world.
#[derive(Clone, Copy)]
struct Creature {
    species: usize,
    x: i32,
    y: i32,
    /// Tile the creature is currently walking toward.
    target: (i32, i32),
    /// Seconds until the next wander decision.
    wait: f32,
    /// Seconds remaining before this creature can trigger another encounter.
    /// Set after every encounter so a lingering creature does not re-trigger
    /// every single frame it stays adjacent.
    cooldown: f32,
}

/// Deterministic starting placement for every creature: two per species,
/// spread out by a fixed offset rather than by any RNG, so the opening frame
/// is identical on every run without needing to seed from anything.
fn build_creatures() -> Vec<Creature> {
    let mut creatures = Vec::with_capacity(SPECIES_COUNT * 2);
    for species in 0..SPECIES_COUNT {
        for k in 0..2i32 {
            let s = species as i32;
            let x = 3 + (s * 5 + k * 9) % (WORLD_W - 6);
            let y = 3 + (s * 4 + k * 6) % (WORLD_H - 6);
            creatures.push(Creature {
                species,
                x,
                y,
                target: (x, y),
                wait: 0.6_f32.mul_add(s as f32, 1.0),
                cooldown: 0.0,
            });
        }
    }
    creatures
}

/// What tapping or dragging over a registered region means.
#[derive(Clone, Copy)]
enum Action {
    /// The world viewport, carrying the camera offset live when it was
    /// registered so a tap can be converted straight to a world tile.
    World {
        camera: (i32, i32),
    },
    Bag,
    Camp,
    /// The dial and the bestiary card float over the world; tapping them
    /// must not also walk the player toward whatever tile is underneath.
    Decor,
}

/// Rows the footer (status frame + Bag + Camp) claims at the bottom of the
/// screen. Six gives every box a 4-row interior after its own border, which
/// is exactly [`TAP_H`] -- any less and the buttons stop being legal touch
/// targets no matter how they are drawn.
const FOOTER_H: u16 = 6;

/// Rows the bestiary card claims when it is promoted to its own band above
/// the footer on portrait, per the brief's addendum for narrow screens.
const BESTIARY_BAND_H: u16 = 7;

/// State: player and creature positions, the two survival meters, the clock,
/// and how much of each species has been met.
pub struct NightWalk {
    player: (i32, i32),
    /// Seconds until another tile of movement is allowed. Throttles both
    /// keyboard steps and a held drag to one step at a time, so a drag reads
    /// as walking rather than as teleporting to wherever the finger is.
    move_cooldown: f32,
    creatures: Vec<Creature>,
    /// Encounters per species, indexed by the same index as [`SPECIES`].
    bestiary: [u32; SPECIES_COUNT],
    body: f32,
    mind: f32,
    day: u32,
    /// Hour of day, `0.0..24.0`.
    time_of_day: f32,
    /// Free-running elapsed seconds, read by every animated glyph. Kept
    /// separate from `time_of_day` because the clock's rate is a game design
    /// choice ([`HOURS_PER_SECOND`]) and the animation's rate is a visual
    /// one; tying reed-sway speed to how fast the in-game day passes would
    /// make the two impossible to tune independently.
    time: f32,
    status_text: String,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for NightWalk {
    fn default() -> Self {
        Self {
            player: (WORLD_W / 2, WORLD_H / 2),
            move_cooldown: 0.0,
            creatures: build_creatures(),
            bestiary: [0; SPECIES_COUNT],
            body: 10.0,
            mind: 10.0,
            day: 2,
            time_of_day: 4.25,
            time: 0.0,
            status_text: "The dark presses close. Something rustles nearby.".to_string(),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl NightWalk {
    /// The center cell of tile `tile`, for placing an actor mid-tile rather
    /// than pinned to its corner.
    const fn tile_center(tile: (i32, i32)) -> (i32, i32) {
        (tile.0 * TILE_W + TILE_W / 2, tile.1 * TILE_H + TILE_H / 2)
    }

    /// The top-left world cell the camera should show, centering
    /// `center_cell` in a viewport of `world_rect`'s size and clamped so the
    /// map edge is never shown past as void.
    fn camera_for(world_rect: Rect, center_cell: (i32, i32)) -> (i32, i32) {
        let w = i32::from(world_rect.width());
        let h = i32::from(world_rect.height());
        let max_x = (WORLD_W * TILE_W - w).max(0);
        let max_y = (WORLD_H * TILE_H - h).max(0);
        let cx = (center_cell.0 - w / 2).clamp(0, max_x);
        let cy = (center_cell.1 - h / 2).clamp(0, max_y);
        (cx, cy)
    }

    fn daylight(&self) -> f32 {
        daylight(self.time_of_day)
    }

    /// The creature nearest the player by Manhattan distance in tiles. Ties
    /// resolve to the first in `creatures`, which is stable across frames
    /// since the vector's order never changes.
    fn nearest_creature(&self) -> Option<&Creature> {
        self.creatures
            .iter()
            .min_by_key(|c| (c.x - self.player.0).abs() + (c.y - self.player.1).abs())
    }

    // ── input ────────────────────────────────────────────────────────────

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            self.pointer.feed(&event);
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.step(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.step(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.step(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.step(1, 0),
                    KeyCode::Char('b' | 'B') => self.bag(),
                    KeyCode::Char('c' | 'C') => self.camp(),
                    _ => {}
                }
            }
        }
        true
    }

    /// Takes one cardinal step if the move cooldown has elapsed, deducting
    /// the Body cost of walking. Shared by the keyboard, tap, and drag paths
    /// so all three cost the same and are throttled the same way.
    fn step(&mut self, dx: i32, dy: i32) {
        if self.move_cooldown > 0.0 || (dx == 0 && dy == 0) {
            return;
        }
        let (x, y) = self.player;
        let (nx, ny) = (
            (x + dx).clamp(0, WORLD_W - 1),
            (y + dy).clamp(0, WORLD_H - 1),
        );
        if (nx, ny) == (x, y) {
            return;
        }
        self.player = (nx, ny);
        self.move_cooldown = 0.12;
        self.body = (self.body - 0.4).max(0.0);
        self.status_text = "You trudge onward.".to_string();
    }

    /// Steps once toward `target`, preferring whichever axis is further off,
    /// so a tap on a distant tile walks a straight-ish line toward it one
    /// tile per cooldown rather than jumping there.
    fn move_toward(&mut self, target: (i32, i32)) {
        let (px, py) = self.player;
        let (ddx, ddy) = (target.0 - px, target.1 - py);
        if ddx.abs() >= ddy.abs() && ddx != 0 {
            self.step(ddx.signum(), 0);
        } else if ddy != 0 {
            self.step(0, ddy.signum());
        }
    }

    fn bag(&mut self) {
        self.status_text = "You check your bag: nothing new since camp.".to_string();
    }

    /// Advances time and restores Body. The one place time moves other than
    /// its own idle drift, which is the tradeoff the brief describes:
    /// resting costs the clock rather than costing nothing.
    fn camp(&mut self) {
        self.body = (self.body + 6.0).min(10.0);
        self.time_of_day += 4.0;
        if self.time_of_day >= 24.0 {
            self.time_of_day -= 24.0;
            self.day += 1;
        }
        self.status_text = "You make camp and rest until the sky lightens.".to_string();
    }

    // ── simulation ───────────────────────────────────────────────────────

    fn simulate(&mut self, dt: f32) {
        self.move_cooldown = (self.move_cooldown - dt).max(0.0);
        self.time_of_day = dt.mul_add(HOURS_PER_SECOND, self.time_of_day);
        if self.time_of_day >= 24.0 {
            self.time_of_day -= 24.0;
            self.day += 1;
        }

        // Mind drains faster the darker it is and recovers a little in
        // daylight, so the gauge moves in both directions rather than only
        // ever falling -- the same shape 21_deck_plan uses for O2.
        let daylight = self.daylight();
        let night = 1.0 - daylight;
        let mind_delta = daylight.mul_add(0.02, -(night * 0.05)) * dt;
        self.mind = (self.mind + mind_delta).clamp(0.0, 10.0);

        // Seeded from the free-running clock rather than any wall-clock
        // read, matching the pattern 21_deck_plan uses for its crew: the
        // same delta sequence always produces the same wander, which is
        // what the snapshot tests require.
        let tick_seed = (self.time * 1000.0) as u32;
        for i in 0..self.creatures.len() {
            let mut rng = Rng::new(tick_seed ^ (i as u32).wrapping_mul(0x9E37_79B9) ^ SEED);
            let c = &mut self.creatures[i];
            c.cooldown = (c.cooldown - dt).max(0.0);
            c.wait -= dt;
            if c.wait > 0.0 {
                continue;
            }
            if c.x == c.target.0 && c.y == c.target.1 {
                let dx = rng.next_below(5) as i32 - 2;
                let dy = rng.next_below(5) as i32 - 2;
                c.target = (
                    (c.x + dx).clamp(0, WORLD_W - 1),
                    (c.y + dy).clamp(0, WORLD_H - 1),
                );
                c.wait = 2.0_f32.mul_add(rng.next_f32(), 1.0);
            } else {
                let dx = (c.target.0 - c.x).signum();
                let dy = (c.target.1 - c.y).signum();
                if dx != 0 && (dy == 0 || rng.next_f32() < 0.5) {
                    c.x = (c.x + dx).clamp(0, WORLD_W - 1);
                } else if dy != 0 {
                    c.y = (c.y + dy).clamp(0, WORLD_H - 1);
                }
                c.wait = 0.5;
            }
        }

        self.check_encounters();
    }

    /// Auto-resolves an exchange for every creature adjacent to the player
    /// whose grace period has elapsed: records one more encounter for its
    /// species (which is the entire bestiary reveal mechanic -- see
    /// [`redacted_name`] and [`revealed_lore_count`]), costs a little Body
    /// and Mind, and sends the creature retreating so it cannot immediately
    /// re-trigger the same exchange next frame.
    fn check_encounters(&mut self) {
        let (px, py) = self.player;
        for i in 0..self.creatures.len() {
            let (cx, cy, cooldown, species) = {
                let c = self.creatures[i];
                (c.x, c.y, c.cooldown, c.species)
            };
            if cooldown > 0.0 || (cx - px).abs() + (cy - py).abs() > 1 {
                continue;
            }

            let first_ever = self.bestiary[species] == 0;
            self.bestiary[species] += 1;
            self.body = (self.body - 0.6).max(0.0);
            self.mind = (self.mind - 0.3).max(0.0);
            self.status_text = if first_ever {
                "Something moves in the dark -- you can't tell what.".to_string()
            } else {
                format!("You cross paths with a {} again.", SPECIES[species].name)
            };

            // Retreat directly away from the player rather than back into
            // its own wander target, so the same creature cannot be walked
            // straight back into on the very next step.
            let ddx = (cx - px).signum().max(if cx == px { 1 } else { cx - px });
            let ddy = (cy - py).signum();
            let nx = (cx + ddx.signum() * 3).clamp(0, WORLD_W - 1);
            let ny = (cy + ddy * 3).clamp(0, WORLD_H - 1);
            let c = &mut self.creatures[i];
            c.cooldown = 8.0;
            c.wait = 3.0;
            c.x = nx;
            c.y = ny;
            c.target = (nx, ny);
        }
    }

    // ── drawing: world ───────────────────────────────────────────────────

    fn draw_world(&self, surface: &mut Surface<'_>, area: Rect, camera: (i32, i32)) {
        let daylight = self.daylight();
        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let wx = camera.0 + i32::from(sx);
                let wy = camera.1 + i32::from(sy);
                let at = (area.left() + sx, area.top() + sy);
                let tx = wx.div_euclid(TILE_W);
                let ty = wy.div_euclid(TILE_H);
                if tx < 0 || ty < 0 || tx >= WORLD_W || ty >= WORLD_H {
                    surface.put(at, ' ', Style::new().bg(rgb(0, 0, 0)));
                    continue;
                }
                let (glyph, fg0, bg0) = biome_glyph(biome_at(tx, ty), wx, wy, self.time);
                let (fg, bg) = tint_for_time(fg0, bg0, daylight);
                surface.put(at, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        self.draw_glow(surface, area, camera);
        for creature in &self.creatures {
            let species = &SPECIES[creature.species];
            Self::draw_actor(
                surface,
                area,
                camera,
                (creature.x, creature.y),
                species.glyph,
                species.color,
                true,
            );
        }
        Self::draw_actor(
            surface,
            area,
            camera,
            self.player,
            '@',
            rgb(235, 150, 120),
            false,
        );
    }

    /// Relights the cells around the player as if a lantern were lifting the
    /// dark, by recomputing their terrain (glyph included, so texture stays
    /// visible) and mixing the result toward a warm color. Recomputing
    /// rather than remembering the cell is what keeps this a pure function
    /// of position and clock, with nothing extra to keep in sync.
    fn draw_glow(&self, surface: &mut Surface<'_>, area: Rect, camera: (i32, i32)) {
        let (cx, cy) = Self::tile_center(self.player);
        let daylight = self.daylight();
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (wx, wy) = (cx + dx, cy + dy);
                let (sx, sy) = (wx - camera.0, wy - camera.1);
                if sx < 0
                    || sy < 0
                    || sx >= i32::from(area.width())
                    || sy >= i32::from(area.height())
                {
                    continue;
                }
                let (tx, ty) = (wx.div_euclid(TILE_W), wy.div_euclid(TILE_H));
                if tx < 0 || ty < 0 || tx >= WORLD_W || ty >= WORLD_H {
                    continue;
                }
                let (glyph, fg0, bg0) = biome_glyph(biome_at(tx, ty), wx, wy, self.time);
                let (fg, bg) = tint_for_time(fg0, bg0, daylight);
                let fg = mix(fg, rgb(255, 225, 180), 0.3);
                let bg = mix(bg, rgb(120, 90, 60), 0.35);
                surface.put(
                    (area.left() + sx as u16, area.top() + sy as u16),
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
    }

    fn draw_actor(
        surface: &mut Surface<'_>,
        area: Rect,
        camera: (i32, i32),
        tile: (i32, i32),
        glyph: char,
        color: Color,
        glow: bool,
    ) {
        let (cx, cy) = Self::tile_center(tile);
        let (sx, sy) = (cx - camera.0, cy - camera.1);
        if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
            return;
        }
        let bg = if glow {
            mix(rgb(8, 8, 16), color, 0.28)
        } else {
            rgb(10, 8, 14)
        };
        surface.put(
            (area.left() + sx as u16, area.top() + sy as u16),
            glyph,
            Style::new().fg(color).bg(bg),
        );
    }

    // ── drawing: dial ────────────────────────────────────────────────────

    fn draw_dial(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .border(Border::Double)
            .frame(rgb(198, 168, 96))
            .bg(rgb(10, 10, 18))
            .draw(surface, area);
        if inner.width() < 7 || inner.height() < 5 {
            return;
        }
        let text_rows = if inner.height() >= 6 { 2 } else { 0 };
        let circle_area = Rect::new(
            inner.left(),
            inner.top(),
            inner.width(),
            inner.height() - text_rows,
        );
        self.draw_clock_face(surface, circle_area);

        if text_rows > 0 {
            let y = circle_area.bottom();
            let day_line = format!("DAY {}", self.day);
            let hh = self.time_of_day.trunc() as u32;
            let mm = (self.time_of_day.fract() * 60.0) as u32;
            let ampm = if hh < 12 { "AM" } else { "PM" };
            let hh12 = match hh % 12 {
                0 => 12,
                other => other,
            };
            let time_line = format!("{hh12:02}:{mm:02} {ampm}");
            center_text(
                surface,
                (inner.left(), y),
                inner.width(),
                &day_line,
                FG,
                rgb(10, 10, 18),
            );
            center_text(
                surface,
                (inner.left(), y + 1),
                inner.width(),
                &time_line,
                ACCENT,
                rgb(10, 10, 18),
            );
        }
    }

    /// Draws the circular face and its rotating hand.
    ///
    /// `rx` is set to roughly twice `ry`: a terminal cell is about twice as
    /// tall as it is wide (see `ui::touch`'s own aspect-ratio arithmetic), so
    /// an ellipse with a wider horizontal radius than vertical is what
    /// actually renders as round on screen, not a shape with equal cell
    /// -counts on both axes.
    fn draw_clock_face(&self, surface: &mut Surface<'_>, area: Rect) {
        let (w, h) = (area.width(), area.height());
        if w < 5 || h < 3 {
            return;
        }
        let cx = f32::from(w - 1) / 2.0;
        let cy = f32::from(h - 1) / 2.0;
        let ry = cy.max(1.0);
        let rx = (f32::from(w - 1) / 2.0).min(ry * 2.0).max(1.0);
        let daylight = self.daylight();
        let bg = rgb(10, 10, 18);

        for y in 0..h {
            for x in 0..w {
                let nx = (f32::from(x) - cx) / rx;
                let ny = (f32::from(y) - cy) / ry;
                let d = nx.mul_add(nx, ny * ny).sqrt();
                if d > 1.08 {
                    continue;
                }
                let is_top = f32::from(y) < cy;
                let (glyph, color) = if d > 0.82 {
                    ('\u{25cb}', rgb(198, 168, 96))
                } else if is_top {
                    (
                        '.',
                        mix(rgb(60, 90, 150), rgb(230, 200, 120), daylight * 0.6),
                    )
                } else {
                    ('.', rgb(30, 34, 60))
                };
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(color).bg(bg),
                );
            }
        }

        // Angle 0 (straight up) at noon, straight down at midnight, so the
        // hand visibly points into whichever half of the face is tinted for
        // that time of day.
        let frac = (self.time_of_day / 24.0).rem_euclid(1.0);
        let angle = (frac - 0.5).mul_add(TAU, -FRAC_PI_2);
        let steps = rx.min(ry).max(1.0) as i32;
        for t in 1..=steps {
            let f = f32::from(t as i16) / f32::from(steps as i16);
            let px = angle.cos().mul_add(rx * f, cx).round();
            let py = angle.sin().mul_add(ry * f, cy).round();
            if px < 0.0 || py < 0.0 || px >= f32::from(w) || py >= f32::from(h) {
                continue;
            }
            surface.put(
                (area.left() + px as u16, area.top() + py as u16),
                '\u{2022}',
                Style::new().fg(rgb(230, 210, 140)).bg(bg),
            );
        }

        // The tip glyph is a discrete sun/moon swap rather than a fade: it is
        // effectively a character of text (see the module docs), and a
        // legible glyph beats a half-mixed one every frame in between.
        let tip_is_day = angle.sin() < 0.0;
        let (tip_glyph, tip_color) = if tip_is_day {
            ('\u{263c}', rgb(240, 210, 120))
        } else {
            (')', rgb(190, 200, 230))
        };
        let tpx = angle.cos().mul_add(rx, cx).round();
        let tpy = angle.sin().mul_add(ry, cy).round();
        if tpx >= 0.0 && tpy >= 0.0 && tpx < f32::from(w) && tpy < f32::from(h) {
            surface.put(
                (area.left() + tpx as u16, area.top() + tpy as u16),
                tip_glyph,
                Style::new().fg(tip_color).bg(bg),
            );
        }
    }

    // ── drawing: bestiary ────────────────────────────────────────────────

    fn draw_bestiary(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .border(Border::Single)
            .frame(panel::FRAME)
            .bg(panel::PANEL_BG)
            .draw(surface, area);
        if inner.width() < 10 || inner.height() < 3 {
            return;
        }

        let Some(creature) = self.nearest_creature() else {
            panel::spans(
                surface,
                (inner.left(), inner.top()),
                inner.width(),
                &[Span::dim("No creature nearby.")],
                panel::PANEL_BG,
            );
            return;
        };
        let species = &SPECIES[creature.species];
        let encounters = self.bestiary[creature.species];

        let icon = if encounters == 0 { '?' } else { species.glyph };
        let icon_color = if encounters == 0 { DIM } else { species.color };
        let name = redacted_name(species.name, encounters);
        let name_color = if encounters == 0 { DIM } else { FG };
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::new(&icon.to_string(), icon_color),
                Span::plain(" "),
                Span::new(&name, name_color),
            ],
            panel::PANEL_BG,
        );

        let mut y = inner.top() + 1;
        if y < inner.bottom() {
            let rule = "\u{2500}".repeat(inner.width_usize());
            surface.print(
                (inner.left(), y),
                &rule,
                Style::new()
                    .fg(scale(panel::FRAME, 0.7))
                    .bg(panel::PANEL_BG),
            );
            y += 1;
        }

        if encounters == 0 {
            let flavor = "Gaining deeper insights into this enemy requires more encounters.";
            for line in wrap(flavor, inner.width_usize()) {
                if y >= inner.bottom() {
                    break;
                }
                surface.print(
                    (inner.left(), y),
                    &line,
                    Style::new().fg(DIM).bg(panel::PANEL_BG),
                );
                y += 1;
            }
            return;
        }

        let revealed = revealed_lore_count(encounters);
        for (i, line) in species.lore.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            if i < revealed {
                let mut wrapped = wrap(line, inner.width_usize());
                if let Some(first) = wrapped.drain(..1.min(wrapped.len())).next() {
                    surface.print(
                        (inner.left(), y),
                        &first,
                        Style::new().fg(FG).bg(panel::PANEL_BG),
                    );
                }
            } else {
                let block = "\u{2593}".repeat(inner.width_usize());
                surface.print(
                    (inner.left(), y),
                    &block,
                    Style::new()
                        .fg(scale(panel::FRAME, 0.5))
                        .bg(panel::PANEL_BG),
                );
            }
            y += 1;
        }
    }

    // ── drawing: footer (status + Bag + Camp) ───────────────────────────

    fn draw_footer(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let gap = 1u16;
        let (status_rect, rest) = panel::split_left(area, 24.min(area.width()));
        let rest = shrink_left(rest, gap);
        let (bag_rect, rest) = panel::split_left(rest, TAP_W.max(9).min(rest.width()));
        let rest = shrink_left(rest, gap);
        let (camp_rect, _) = panel::split_left(rest, TAP_W.max(9).min(rest.width()));

        self.draw_status_box(surface, status_rect);
        Self::draw_button(surface, bag_rect, "BAG");
        Self::draw_button(surface, camp_rect, "CAMP");

        self.hotspots.push_tappable(bag_rect, area, Action::Bag);
        self.hotspots.push_tappable(camp_rect, area, Action::Camp);
    }

    fn draw_status_box(&self, surface: &mut Surface<'_>, rect: Rect) {
        let inner = Panel::new()
            .border(Border::Single)
            .frame(panel::FRAME)
            .bg(panel::PANEL_BG)
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }
        let body_text = format!("{:>2}/10", self.body.round() as i32);
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::new("\u{2665}", rgb(214, 110, 120)),
                Span::plain(" Body "),
                Span::new(&body_text, FG),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            let mind_text = format!("{:>2}/10", self.mind.round() as i32);
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[
                    Span::new("\u{2666}", rgb(120, 170, 224)),
                    Span::plain(" Mind "),
                    Span::new(&mind_text, FG),
                ],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_button(surface: &mut Surface<'_>, rect: Rect, label: &str) {
        let inner = Panel::new()
            .border(Border::Single)
            .frame(panel::FRAME)
            .bg(panel::PANEL_BG)
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }
        let y = inner.top() + inner.height() / 2;
        center_text(
            surface,
            (inner.left(), y),
            inner.width(),
            label,
            ACCENT,
            panel::PANEL_BG,
        );
    }

    fn status_line(&self) -> String {
        let hh = self.time_of_day.trunc() as u32;
        let mm = (self.time_of_day.fract() * 60.0) as u32;
        format!("Day {} {hh:02}:{mm:02}  {}", self.day, self.status_text)
    }
}

/// `rect` with `n` columns trimmed from its left edge, clamped to empty
/// rather than underflowing when `rect` is already narrower than `n`.
fn shrink_left(rect: Rect, n: u16) -> Rect {
    let n = n.min(rect.width());
    Rect::new(rect.left() + n, rect.top(), rect.width() - n, rect.height())
}

/// Prints `text` centered in `width` cells starting at `(x, y)`.
fn center_text(
    surface: &mut Surface<'_>,
    (x, y): (u16, u16),
    width: u16,
    text: &str,
    color: Color,
    bg: Color,
) {
    let text = truncate(text, width as usize);
    let len = text.chars().count() as u16;
    let pad = (width.saturating_sub(len)) / 2;
    surface.print((x + pad, y), text, Style::new().fg(color).bg(bg));
}

impl Demo for NightWalk {
    const NAME: &'static str = "45_night_walk";
    const TITLE: &'static str = "45 Night Walk";
    const BLURB: &'static str =
        "A bestiary that fills in through encounters, in a world too dark to see.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Arrows/WASD", "walk"),
            ("tap/drag map", "walk toward"),
            ("B", "check bag"),
            ("C", "make camp"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        self.time += dt;

        if !self.handle_events(term) {
            return false;
        }
        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let portrait = Shape::of(content).stacks();
        let footer_h = FOOTER_H.min(content.height());
        let (world_area, footer_rect) = panel::split_bottom(content, footer_h);

        // On portrait the bestiary card is promoted to its own band above
        // the footer (per the brief's mobile addendum); everywhere else it
        // floats over the world's bottom-right corner instead, matching the
        // reference screenshot's four-corner arrangement.
        let (world_rect, portrait_bestiary) = if portrait {
            let h = BESTIARY_BAND_H.min(world_area.height().saturating_sub(6));
            let (rest, band) = panel::split_bottom(world_area, h);
            (rest, Some(band))
        } else {
            (world_area, None)
        };

        let camera_before = Self::camera_for(world_rect, Self::tile_center(self.player));
        self.hotspots.clear();
        self.hotspots.push(
            world_rect,
            Action::World {
                camera: camera_before,
            },
        );

        let dial_w = 20.min(world_rect.width()).max(12);
        let dial_h = 9.min(world_rect.height()).max(7);
        let dial_rect = Rect::new(world_rect.left(), world_rect.top(), dial_w, dial_h);
        self.hotspots.push(dial_rect, Action::Decor);

        let bestiary_rect = portrait_bestiary.unwrap_or_else(|| {
            let bw = 34.min(world_rect.width()).max(18);
            let bh = 9.min(world_rect.height()).max(6);
            Rect::new(world_rect.right() - bw, world_rect.bottom() - bh, bw, bh)
        });
        self.hotspots.push(bestiary_rect, Action::Decor);

        // Handle input against this frame's layout before drawing, so the
        // status line and every gauge already reflect whatever the tap did.
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap.or(gesture.drag)
            && let Some(action) = self.hotspots.hit(pos).copied()
        {
            match action {
                Action::Bag => self.bag(),
                Action::Camp => self.camp(),
                Action::World { camera } => {
                    let sx = i32::from(pos.x) - i32::from(world_rect.left());
                    let sy = i32::from(pos.y) - i32::from(world_rect.top());
                    let target = (
                        (camera.0 + sx).div_euclid(TILE_W),
                        (camera.1 + sy).div_euclid(TILE_H),
                    );
                    self.move_toward(target);
                }
                Action::Decor => {}
            }
        }

        let camera = Self::camera_for(world_rect, Self::tile_center(self.player));
        self.draw_world(&mut surface, world_rect, camera);
        self.draw_dial(&mut surface, dial_rect);
        self.draw_bestiary(&mut surface, bestiary_rect);
        self.draw_footer(&mut surface, footer_rect);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(NightWalk);
