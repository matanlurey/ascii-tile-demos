//! 52: Quiet March -- Divine Right's map is drawn like a printed atlas plate,
//! not a UI screen.
//!
//! Every other hex demo in this gallery treats the grid as the point:
//! [`10_political`](../10_political) overlays borders on it,
//! [`26_hexcrawl`](../26_hexcrawl) explores it,
//! [`20_realm_map`](../20_realm_map) paints strategic tiles on it. Divine
//! Right's own board (1979, TSR) does something none of them do: it sets
//! realm names in large letter-spaced capitals laid directly across the hex
//! field -- `M U E T A R`, `E L F L A N D`, `T H E   S E A   O F
//! D R O W N I N G   M E N` -- so the map reads as cartography with a hex
//! grid printed under it, rather than a hex grid with labels stuck on top.
//! That typography is the one element this demo owns; everything else
//! (flat realm fills, small terrain vignettes, a heavy river line, a compass
//! rose) exists only to support the read.
//!
//! The map is not static. Divine Right's actual subject is envoy diplomacy:
//! send an envoy to an independent realm, roll against its disposition, and
//! watch the fill color change underneath you. Realms also drift and
//! occasionally fall to a rival on their own over time, so the map keeps
//! moving even with no input at all.
//!
//! Techniques on show:
//!
//! - **Letter-spaced names following their own territory**
//!   ([`QuietMarch::draw_region_name`]): each realm's name is spread one
//!   letter per cell with a wider gap between words, centered on the actual
//!   span of hexes it owns in its seed row -- not a fixed-width label box --
//!   so a small realm's name visibly strains against its own borders exactly
//!   as the source board's hand lettering does.
//! - **Two typographic scales** ([`spaced_name`] vs. [`QuietMarch::draw_place_names`]):
//!   realm names are wide and letter-spaced across many hexes; place names are
//!   compact, unspaced words confined to one hex's width. The contrast in
//!   spacing -- not font size, which a character grid cannot offer -- is what
//!   separates "region" from "settlement" at a glance.
//! - **Letters under terrain** (draw order in
//!   [`QuietMarch::draw_map`]): big names are stamped into the hex fill first;
//!   the per-hex terrain vignette and capital markers are drawn afterward
//!   directly on top, so a letter that lands on a mountain hex is legitimately
//!   occluded by it, matching the source board where the lettering sits under
//!   the illustration.
//! - **Envoy diplomacy that repaints the map**
//!   ([`QuietMarch::send_envoy`], [`QuietMarch::ambient_tick`]): a deterministic
//!   roll (seeded from realm id and attempt count, never wall-clock) against a
//!   realm's disposition flips it to the player's banner on success; an
//!   independent, tick-driven world clock lets territory occasionally fall to
//!   a rival with no input at all, so the map is provably not a still image.
//! - **Voronoi realms sized to the live viewport**
//!   ([`nearest_region`]): realm seed points are stored as fractions of the
//!   hex field, not fixed tile coordinates, so the same nine-realm shape
//!   redraws at whatever column/row count the current panel derives -- the
//!   map fills a phone panel and a desktop panel alike without a second
//!   layout.
//!
//! ```sh
//! cargo run --example 52_quiet_march --features crossterm
//! cargo run --example 52_quiet_march --features software
//! cargo run --example 52_quiet_march --features gl
//! cargo run --example 52_quiet_march  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;

use tilekit::geom::{Cell, HexLayout, HexOrientation, Tile, hex_line};
use tilekit::noise::hash01;
use tilekit::palette::{self, mix, rgb, scale};

/// Hex cell pitch. Small enough that a modest panel still fits a dozen-plus
/// columns of realm (the minimum for a letter-spaced name to have room to
/// breathe), large enough that a terrain vignette glyph and a capital marker
/// both fit inside one hex without touching. Pointy-top, so rows read left
/// to right the way the source board's names do.
const PITCH_X: i32 = 8;
/// See [`PITCH_X`]. Pointy hexes draw `PITCH_Y + 1` screen rows (a shared
/// taper row above and below a full-width middle band; see
/// [`tilekit::geom::HexLayout`]), so 3 gives a hex four rows tall -- enough
/// for a taper, a name/vignette row, a second interior row, and a taper.
const PITCH_Y: i32 = 3;
const LAYOUT: HexLayout = HexLayout::new(HexOrientation::Pointy, PITCH_X, PITCH_Y);

/// Fewest hex columns/rows the field will render even in a very small panel,
/// so the map never degenerates to one or two oversized hexes.
const MIN_COLS: i32 = 8;
const MIN_ROWS: i32 = 5;
/// Most hex columns/rows rendered even in a very large panel, purely so the
/// per-frame O(cols*rows) fill pass stays cheap on an oversized desktop
/// window; comfortably above anything a real terminal reaches.
const MAX_COLS: i32 = 90;
const MAX_ROWS: i32 = 60;

/// How many realms can be courted or lost. The ninth region (index
/// `NUM_REALMS`) is the sea: named and drawn like a realm, but never owned.
const NUM_REALMS: usize = 8;
const NUM_REGIONS: usize = NUM_REALMS + 1;

/// The player's starting realm. Never flippable, by envoy or by drift.
const HOME_REALM: usize = 0;
/// Realms held by a rival power from the start.
const RIVAL_REALMS: [usize; 2] = [1, 6];

/// World-seconds between ambient drift ticks: slow enough that a glance at
/// the map still shows the same picture, frequent enough that leaving the
/// demo running for under a minute visibly changes who holds what.
const AMBIENT_INTERVAL: f32 = 5.0;
/// World-seconds an envoy needs to recover after a rebuff before trying that
/// realm again -- a soft rate limit, not a hard lock, so a mis-tap costs time
/// rather than being irreversible.
const ENVOY_COOLDOWN: f32 = 4.0;

const DISPOSITION_SEED: u32 = 0x0715_D150;
const ENVOY_SEED: u32 = 0xE411_0411;
const DRIFT_SEED: u32 = 0x0D41_F700;
const TERRAIN_SEED: u32 = 0x7E11_A110;

/// One named region of the map: a realm, or the sea.
struct RegionDef {
    /// Set in caps; [`spaced_name`] adds the letter spacing at draw time
    /// rather than baking it into the literal, so the source string stays
    /// grep-able and the uniqueness test can compare it directly.
    name: &'static str,
    color: Color,
    /// Fraction of the hex field's width/height, not a fixed tile: this is
    /// what lets the same nine-region shape fill whatever column/row count
    /// [`QuietMarch::draw_map`] derives from the live panel.
    seed: (f32, f32),
    sea: bool,
}

/// The nine named regions, positioned to echo Divine Right's own board:
/// forest realm in the northwest, the sea in the southwest, the rest fanned
/// out to the east and south the way the source plate lays them out.
const REGIONS: [RegionDef; NUM_REGIONS] = [
    RegionDef {
        name: "ELFLAND",
        color: rgb(88, 168, 96),
        seed: (0.10, 0.14),
        sea: false,
    },
    RegionDef {
        name: "IMMER",
        color: rgb(198, 74, 64),
        seed: (0.46, 0.16),
        sea: false,
    },
    RegionDef {
        name: "ZORN",
        color: rgb(92, 196, 182),
        seed: (0.82, 0.12),
        sea: false,
    },
    RegionDef {
        name: "MUETAR",
        color: rgb(212, 188, 84),
        seed: (0.55, 0.42),
        sea: false,
    },
    RegionDef {
        name: "PON",
        color: rgb(86, 156, 214),
        seed: (0.88, 0.46),
        sea: false,
    },
    RegionDef {
        name: "HOTHIOR",
        color: rgb(216, 132, 68),
        seed: (0.20, 0.58),
        sea: false,
    },
    RegionDef {
        name: "SHUCASSAM",
        color: rgb(198, 150, 96),
        seed: (0.58, 0.74),
        sea: false,
    },
    RegionDef {
        name: "ROMBUNE",
        color: rgb(208, 96, 158),
        seed: (0.22, 0.90),
        sea: false,
    },
    RegionDef {
        name: "THE SEA OF DROWNING MEN",
        color: rgb(20, 40, 74),
        seed: (0.05, 0.80),
        sea: true,
    },
];

/// Sixteen fixed short place names (two per realm), all no longer than
/// [`PITCH_X`] `- 1` characters so none of them can be truncated -- the
/// earlier adjective+noun generator truncated to fit one hex's width and
/// silently produced duplicate *visible* text ("Sun Scorched" and "Sun
/// Scowl" both read as "Sun Sco" once cut to seven characters), which broke
/// the on-screen uniqueness guarantee even though the untruncated strings
/// were distinct. A fixed pool that already fits removes the truncation step
/// that caused the collision, rather than trying to out-clever it.
const PLACE_NAMES: [&str; NUM_REALMS * 2] = [
    "Loris", "Axu", "Larkin", "Scum", "Khardul", "Aptete", "Jipols", "Heath", "Worn", "Ooze",
    "Vahka", "Olde", "Withers", "Shrine", "Oasis", "Forbid",
];

/// The `i`-th place name. See [`PLACE_NAMES`] for the uniqueness argument.
const fn place_name(i: usize) -> &'static str {
    PLACE_NAMES[i % PLACE_NAMES.len()]
}

/// Inserts a single space between letters and a wider gap between words, the
/// literal mechanism behind every big name on the map (`"M U E T A R"`,
/// `"T H E   S E A   O F   D R O W N I N G   M E N"`). A character grid has
/// no font size to shrink for a "small hand", so this spacing -- rather than
/// glyph size -- is what makes a name read as monumental lettering instead of
/// an ordinary label; [`QuietMarch::draw_place_names`] draws its names with
/// none of it, which is the contrasting scale.
fn spaced_name(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(' ') {
        for (i, ch) in word.chars().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push(ch);
        }
        out.push_str("   ");
    }
    out.truncate(out.trim_end().len());
    out
}

/// What a hex under a realm's control is drawn as, before the capital
/// override. Deterministic from tile position alone (see [`terrain_at`]), so
/// a realm's terrain never reshuffles from one frame to the next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Plain,
    Forest,
    Mountain,
    Swamp,
    City,
    Castle,
}

impl Terrain {
    /// The vignette glyph and its ink color. `Plain` has no glyph: leaving
    /// most of the field unmarked is what makes the marked hexes (forest,
    /// mountain, a town) read as picked out rather than as noise.
    const fn glyph_color(self) -> (Option<char>, Color) {
        match self {
            Self::Plain => (None, palette::BLACK),
            Self::Forest => (Some('\u{2663}'), rgb(58, 112, 62)),
            Self::Mountain => (Some('\u{25B2}'), rgb(102, 98, 94)),
            Self::Swamp => (Some('\u{2248}'), rgb(64, 92, 78)),
            Self::City => (Some('\u{2666}'), rgb(218, 184, 100)),
            Self::Castle => (Some('\u{2229}'), rgb(236, 226, 200)),
        }
    }
}

/// Weighted terrain pick for a non-capital land hex. Weights favor open
/// ground (the majority terrain on the source board too), with mountains
/// rarest since they read strongest and should stay sparse.
fn terrain_at(tile: Tile) -> Terrain {
    let h = hash01(TERRAIN_SEED, tile.col, tile.row);
    if h < 0.10 {
        Terrain::Mountain
    } else if h < 0.30 {
        Terrain::Forest
    } else if h < 0.36 {
        Terrain::Swamp
    } else if h < 0.40 {
        Terrain::City
    } else {
        Terrain::Plain
    }
}

/// Who holds a realm, and how contested it currently is.
#[derive(Clone, Copy)]
enum Allegiance {
    /// The player's own starting realm. Never changes.
    Home,
    /// Won by envoy, or held by a rival, no longer independent.
    Player,
    Rival,
    /// Not yet sworn to anyone. The `f32` is disposition, `0.0..=1.0`: an
    /// envoy's roll must land under it to succeed.
    Independent(f32),
}

impl Allegiance {
    /// The small marker drawn at a realm's capital hex, or `None` for a
    /// realm still up for grabs -- an independent realm is deliberately left
    /// unmarked so the map does not pre-announce which way it is leaning;
    /// only the bottom bar's disposition readout does that, and only once
    /// selected.
    const fn marker(self) -> Option<(char, Color)> {
        match self {
            Self::Home | Self::Player => Some(('\u{2665}', rgb(246, 196, 96))),
            Self::Rival => Some(('\u{2660}', rgb(198, 74, 64))),
            Self::Independent(_) => None,
        }
    }
}

/// Runtime state for one realm (index `0..NUM_REALMS`, mirroring
/// [`REGIONS`]).
struct RealmState {
    allegiance: Allegiance,
    /// Seconds remaining before another envoy may be sent, see
    /// [`ENVOY_COOLDOWN`].
    cooldown: f32,
    /// Envoy attempts sent so far, folded into the roll's seed so repeated
    /// attempts against the same realm draw a fresh (but still reproducible)
    /// roll each time rather than the same one forever.
    attempts: u32,
}

/// What a tap or key hit.
#[derive(Clone, Copy)]
enum Action {
    Select(usize),
    SendEnvoy,
    Cycle(i32),
}

/// State: one [`RealmState`] per realm, the two place names attached to each,
/// the current selection, and the world clock everything else derives from.
pub struct QuietMarch {
    realms: Vec<RealmState>,
    place_names: Vec<[&'static str; 2]>,
    selected: usize,
    time: f32,
    /// Highest ambient tick already applied, so [`QuietMarch::tick`] can
    /// catch up exactly once per crossed threshold regardless of how coarse
    /// or fine `frame.delta` happens to be -- see the module's determinism
    /// note in [`QuietMarch::ambient_tick`].
    last_tick: u32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    message: String,
    message_ok: bool,
    fps: FpsMeter,
}

impl Default for QuietMarch {
    fn default() -> Self {
        let mut realms = Vec::with_capacity(NUM_REALMS);
        for idx in 0..NUM_REALMS {
            let allegiance = if idx == HOME_REALM {
                Allegiance::Home
            } else if RIVAL_REALMS.contains(&idx) {
                Allegiance::Rival
            } else {
                let d = 0.6f32.mul_add(hash01(DISPOSITION_SEED, idx as i32, 0), 0.2);
                Allegiance::Independent(d)
            };
            realms.push(RealmState {
                allegiance,
                cooldown: 0.0,
                attempts: 0,
            });
        }

        let place_names = (0..NUM_REALMS)
            .map(|r| [place_name(r * 2), place_name(r * 2 + 1)])
            .collect();

        let selected = realms
            .iter()
            .position(|r| matches!(r.allegiance, Allegiance::Independent(_)))
            .unwrap_or(0);

        Self {
            realms,
            place_names,
            selected,
            time: 0.0,
            last_tick: 0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            message: "Select a realm, then send an envoy.".to_owned(),
            message_ok: false,
            fps: FpsMeter::new(),
        }
    }
}

impl QuietMarch {
    const fn cycle(&mut self, dir: i32) {
        if self.realms.is_empty() {
            return;
        }
        let n = self.realms.len() as i32;
        self.selected = (self.selected as i32 + dir).rem_euclid(n) as usize;
    }

    /// Sends an envoy to the selected realm: rolls against its disposition
    /// and, on success, repaints it to the player's banner.
    ///
    /// The roll is `hash01(ENVOY_SEED, realm index, attempt count)`, never a
    /// wall-clock or frame-count seed, so replaying the exact same sequence
    /// of taps against the exact same realm always resolves the same way --
    /// the property the snapshot determinism test depends on.
    fn send_envoy(&mut self) {
        let Some(realm) = self.realms.get_mut(self.selected) else {
            return;
        };
        let Allegiance::Independent(disposition) = realm.allegiance else {
            self.message = format!("{} is not independent.", REGIONS[self.selected].name);
            self.message_ok = false;
            return;
        };
        if realm.cooldown > 0.0 {
            "That envoy is still resting.".clone_into(&mut self.message);
            self.message_ok = false;
            return;
        }

        realm.attempts += 1;
        let roll = hash01(ENVOY_SEED, self.selected as i32, realm.attempts as i32);
        if roll < disposition {
            realm.allegiance = Allegiance::Player;
            self.message = format!("{} pledges allegiance.", REGIONS[self.selected].name);
            self.message_ok = true;
        } else {
            // A rebuff still buys a little goodwill for next time, so
            // repeated envoys are not pure chance -- persistence has a
            // visible payoff even while it keeps failing.
            let warmed = (disposition + 0.05).min(0.95);
            realm.allegiance = Allegiance::Independent(warmed);
            realm.cooldown = ENVOY_COOLDOWN;
            self.message = format!("{} rebuffs the envoy.", REGIONS[self.selected].name);
            self.message_ok = false;
        }
    }

    /// Advances the world by one ambient tick: one independent realm's
    /// disposition drifts, and a realm whose disposition drifts low enough
    /// falls to a rival with no envoy involved at all.
    ///
    /// Which realm moves is `tick % independent_count`, a rotation rather
    /// than a fresh random pick, so every independent realm is revisited on a
    /// predictable cadence instead of the same one or two dominating by
    /// chance -- and the choice stays a pure function of `tick`, so this is
    /// exactly as replay-safe as [`Self::send_envoy`]'s roll.
    fn ambient_tick(&mut self, tick: u32) {
        let independents: Vec<usize> = self
            .realms
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.allegiance, Allegiance::Independent(_)))
            .map(|(i, _)| i)
            .collect();
        let Some(&idx) = independents.get(tick as usize % independents.len().max(1)) else {
            return;
        };
        let Allegiance::Independent(d) = self.realms[idx].allegiance else {
            return;
        };
        let drift = 0.1f32.mul_add(hash01(DRIFT_SEED, idx as i32, tick as i32), -0.05);
        let next = (d + drift).clamp(0.05, 0.95);
        if next < 0.12 {
            self.realms[idx].allegiance = Allegiance::Rival;
            self.message = format!("{} falls to a rival power.", REGIONS[idx].name);
            self.message_ok = false;
        } else {
            self.realms[idx].allegiance = Allegiance::Independent(next);
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
                    KeyCode::Left | KeyCode::Char('[') => self.cycle(-1),
                    KeyCode::Right | KeyCode::Char(']') => self.cycle(1),
                    KeyCode::Enter | KeyCode::Char(' ') => self.send_envoy(),
                    _ => {}
                }
            }
        }
        true
    }

    /// Resolves this frame's tap, if any, against the hotspots the *previous*
    /// frame's draw registered. The one-frame lag is standard for this
    /// gallery's immediate-mode layout (see `38_hex_general`'s
    /// `handle_pointer`): hotspots can only describe what was actually drawn,
    /// and that is only known once drawing has happened.
    fn handle_pointer(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos).copied() {
            Some(Action::Select(idx)) => self.selected = idx,
            Some(Action::SendEnvoy) => self.send_envoy(),
            Some(Action::Cycle(dir)) => self.cycle(dir),
            None => {}
        }
    }

    fn status(&self) -> String {
        format!("realm {}/{}", self.selected + 1, self.realms.len())
    }

    // ── Map ──────────────────────────────────────────────────────────────

    /// Fill color for a land hex: the realm's flat source-book color, tinted
    /// by who currently holds it. Independent realms stay close to the flat
    /// source color; the player's own ground is lifted toward white so it
    /// reads as "yours" without needing a separate border layer; a rival's
    /// ground is pulled down so a glance answers "who is winning" before any
    /// text is read.
    fn land_face(&self, realm_idx: usize, base: Color) -> Color {
        let tint = match self.realms[realm_idx].allegiance {
            Allegiance::Home | Allegiance::Player => mix(base, palette::WHITE, 0.16),
            Allegiance::Rival => scale(base, 0.5),
            Allegiance::Independent(_) => scale(base, 0.86),
        };
        mix(tint, ui::BG, 0.12)
    }

    /// Fill color for a sea hex: a slow traveling ripple, the one place on
    /// the map whose color is meant to visibly move rather than only its
    /// occupants.
    fn sea_face(&self, tile: Tile) -> Color {
        let wave = (tile.col as f32).mul_add(0.5, self.time * -1.4).sin();
        scale(rgb(20, 40, 74), wave.mul_add(0.05, 1.0))
    }

    fn face_of(&self, owner: usize, tile: Tile) -> Color {
        if REGIONS[owner].sea {
            self.sea_face(tile)
        } else {
            self.land_face(owner, REGIONS[owner].color)
        }
    }

    /// Derives the hex field's own extent from the live panel rather than a
    /// fixed constant: the same nine fractional realm seeds then redraw at
    /// whatever column/row count this rect affords, so the map fills a phone
    /// panel and a desktop panel alike. See the module docs' note on
    /// `nearest_region`.
    fn field_extent(area: Rect) -> (i32, i32) {
        let cols = (i32::from(area.width()) / PITCH_X).clamp(MIN_COLS, MAX_COLS);
        let rows = (i32::from(area.height()) / PITCH_Y).clamp(MIN_ROWS, MAX_ROWS);
        (cols, rows)
    }

    fn seed_tiles(total_cols: i32, total_rows: i32) -> [Tile; NUM_REGIONS] {
        let mut out = [Tile::new(0, 0); NUM_REGIONS];
        for (slot, region) in out.iter_mut().zip(REGIONS.iter()) {
            *slot = Tile::new(
                ((region.seed.0 * total_cols as f32) as i32).clamp(0, total_cols - 1),
                ((region.seed.1 * total_rows as f32) as i32).clamp(0, total_rows - 1),
            );
        }
        out
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 4 || area.height() < 4 {
            return;
        }
        let (total_cols, total_rows) = Self::field_extent(area);
        let seeds = Self::seed_tiles(total_cols, total_rows);

        for (owner, &capital) in seeds.iter().enumerate().take(NUM_REALMS) {
            self.register_capital_hotspot(area, capital, owner);
        }

        // Pass 1: flat hex fills. Every hex, land and sea alike, gets its
        // face before anything else is drawn on top of it. Pointy-top rows
        // stagger by half a hex (see `HexLayout::stagger`), so a plain
        // `0..total_cols` sweep leaves a triangular notch of bare panel at
        // the left edge on odd rows and at the right edge on even rows.
        // Widening the sweep by one column on each side fills those notches
        // with the neighbouring off-field hex instead of leaving them bare;
        // the extra column is clipped by `draw_hex_face` wherever it falls
        // outside `area`, so this costs nothing on panels with no stagger
        // gap to begin with.
        for row in 0..total_rows {
            for col in -1..=total_cols {
                let tile = Tile::new(col, row);
                let owner = nearest_region(tile, &seeds);
                let cell = LAYOUT.tile_to_cell(tile);
                draw_hex_face(surface, area, cell.x, cell.y, self.face_of(owner, tile));
            }
        }

        // Pass 2: the big letter-spaced names, stamped straight onto the
        // fills. Terrain and markers draw after this and will sit on top of
        // any letter that shares their cell -- the "names under terrain"
        // ordering the module docs describe. `reserved` tracks which screen
        // row each name already claimed, so a very small panel (where two
        // realms' seed rows round to the same value) shifts the later name
        // to a free row instead of interleaving both names into one
        // unreadable line.
        let mut reserved: Vec<(i32, i32, i32)> = Vec::new();
        for owner in 0..NUM_REGIONS {
            self.draw_region_name(
                surface,
                area,
                owner,
                &seeds,
                total_cols,
                total_rows,
                &mut reserved,
            );
        }

        // Pass 3: per-hex terrain vignette, capital markers, place names.
        for row in 0..total_rows {
            for col in 0..total_cols {
                let tile = Tile::new(col, row);
                let owner = nearest_region(tile, &seeds);
                if REGIONS[owner].sea {
                    continue;
                }
                self.draw_hex_detail(surface, area, tile, owner, seeds[owner]);
            }
        }
        self.draw_place_names(surface, area, &seeds, total_cols, total_rows);

        self.draw_river(surface, area, &seeds, total_cols, total_rows);
        self.draw_selection(surface, area, &seeds);
        draw_compass(surface, area);
    }

    fn register_capital_hotspot(&mut self, area: Rect, capital: Tile, owner: usize) {
        let cell = LAYOUT.tile_to_cell(capital);
        let w = i32::from(area.width());
        let h = i32::from(area.height());
        if cell.x < 0 || cell.y < 0 || cell.x >= w || cell.y >= h {
            return;
        }
        let rw = PITCH_X.min(w - cell.x);
        let rh = (PITCH_Y + 1).min(h - cell.y);
        if rw <= 0 || rh <= 0 {
            return;
        }
        let rect = Rect::new(
            area.left() + cell.x as u16,
            area.top() + cell.y as u16,
            rw as u16,
            rh as u16,
        );
        self.hotspots
            .push_tappable(rect, area, Action::Select(owner));
    }

    /// Centers `region`'s letter-spaced name on the actual span of hexes it
    /// owns along its seed row, not on a fixed box -- a realm pinched thin at
    /// its own capital's latitude gets a name that visibly strains against
    /// its borders, the same way the source board's hand lettering does when
    /// a realm is small.
    #[allow(clippy::too_many_arguments)]
    fn draw_region_name(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        owner: usize,
        seeds: &[Tile; NUM_REGIONS],
        total_cols: i32,
        total_rows: i32,
        reserved: &mut Vec<(i32, i32, i32)>,
    ) {
        let seed_row = seeds[owner].row.clamp(0, total_rows - 1);
        let spaced = spaced_name(REGIONS[owner].name);
        let len = spaced.chars().count() as i32;

        // Try the seed's own row first, then search outward. A candidate row
        // is accepted only if this name's own span on that row (which varies
        // row to row, since ownership does) does not overlap any interval
        // already reserved on it -- letting two realms far apart in columns
        // share a row, while still stopping two overlapping names from being
        // interleaved into one unreadable line, which is the failure mode a
        // simpler one-name-per-row rule hit on a small panel with more named
        // regions than rows.
        let mut rows: Vec<i32> = (0..total_rows).collect();
        rows.sort_by_key(|&r| (r - seed_row).abs());

        let mut placed = None;
        for row in rows {
            let mut min_col = seeds[owner].col.clamp(0, total_cols - 1);
            let mut max_col = min_col;
            let mut found = false;
            for col in 0..total_cols {
                if nearest_region(Tile::new(col, row), seeds) == owner {
                    if !found {
                        min_col = col;
                        found = true;
                    }
                    max_col = col;
                }
            }
            if !found {
                continue;
            }

            let span_cells = (max_col - min_col + 1) * PITCH_X;
            let origin = LAYOUT.tile_to_cell(Tile::new(min_col, row));
            // Centering on the region's own span is right for most realms,
            // but a name can be wider than the sliver of hexes its owner
            // holds at this latitude (the sea's full title is 24 letters
            // wide once spaced, far wider than the strip of sea hexes on
            // any one row) -- centering alone then pushes `start_x` negative
            // and the leading letters run off the left of the panel.
            // Clamping to the *field's* extent, not just the region's own
            // span, keeps the name whole while still centering it on the
            // region wherever the region's span is wide enough to hold it.
            let field_width = total_cols * PITCH_X;
            let start_x = (origin.x + (span_cells - len) / 2).clamp(0, (field_width - len).max(0));
            let (x0, x1) = (start_x - 1, start_x + len + 1);
            let overlaps = reserved
                .iter()
                .any(|&(r, a, b)| r == row && a < x1 && x0 < b);
            if !overlaps {
                placed = Some((row, origin.y + 1, start_x));
                reserved.push((row, x0, x1));
                break;
            }
        }
        // On a panel too cramped to give this name any row without
        // overlapping a neighbour's letters (only the sea's 24-letter full
        // title, on the smallest supported panels, hits this), skip it
        // rather than draw it garbled -- an absent name is a worse map than
        // an ideal one, but a better map than one with two names interleaved
        // into an unreadable smear.
        let Some((_, y, start_x)) = placed else {
            return;
        };

        let ink = rgb(22, 17, 13);
        for (i, ch) in spaced.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let cx = start_x + i as i32;
            let here = nearest_region(LAYOUT.cell_to_tile(Cell::new(cx, y)), seeds);
            let bg = self.face_of(here, Tile::new(cx, y));
            put_clipped(surface, area, cx, y, ch, Style::new().fg(ink).bg(bg));
        }
    }

    fn draw_hex_detail(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tile: Tile,
        owner: usize,
        capital: Tile,
    ) {
        let face = self.land_face(owner, REGIONS[owner].color);
        let cell = LAYOUT.tile_to_cell(tile);
        let center_x = cell.x + PITCH_X / 2;
        let center_y = cell.y + PITCH_Y / 2 + 1;

        let is_capital = tile == capital;
        let terrain = if is_capital {
            Terrain::Castle
        } else {
            terrain_at(tile)
        };
        if let (Some(glyph), fg) = terrain.glyph_color() {
            put_clipped(
                surface,
                area,
                center_x,
                center_y,
                glyph,
                Style::new().fg(fg).bg(face),
            );
        }

        if is_capital && let Some((marker, color)) = self.realms[owner].allegiance.marker() {
            put_clipped(
                surface,
                area,
                center_x - 2,
                center_y,
                marker,
                Style::new().fg(color).bg(face),
            );
        }
    }

    /// Draws two compact, unspaced place names per land realm, offset from
    /// its capital -- the "finer hand" scale, in deliberate typographic
    /// contrast with [`Self::draw_region_name`]'s wide letter spacing.
    fn draw_place_names(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        seeds: &[Tile; NUM_REGIONS],
        total_cols: i32,
        total_rows: i32,
    ) {
        const OFFSETS: [(i32, i32); 2] = [(-3, -1), (3, 1)];
        let ink = rgb(212, 208, 198);
        for owner in 0..NUM_REALMS {
            let capital = seeds[owner];
            for (which, &(dx, dy)) in OFFSETS.iter().enumerate() {
                let tile = Tile::new(
                    (capital.col + dx).clamp(0, total_cols - 1),
                    (capital.row + dy).clamp(0, total_rows - 1),
                );
                if tile == capital {
                    continue;
                }
                let here = nearest_region(tile, seeds);
                let bg = self.face_of(here, tile);
                let cell = LAYOUT.tile_to_cell(tile);
                let max_len = usize::try_from(PITCH_X - 1).unwrap_or(3).max(3);
                let text = truncate(self.place_names[owner][which], max_len);
                let text_len = text.chars().count() as i32;
                let start_x = cell.x + (PITCH_X - text_len) / 2;
                for (i, ch) in text.chars().enumerate() {
                    put_clipped(
                        surface,
                        area,
                        start_x + i as i32,
                        cell.y,
                        ch,
                        Style::new().fg(ink).bg(bg),
                    );
                }
            }
        }
    }

    /// Traces the river along a chain of hex centers as a one-cell-wide
    /// linework stroke, not a filled block: Divine Right's rivers are a fine
    /// dark line following hex edges on a printed sheet, and a solid run of
    /// filled cells (an earlier cut of this function) reproduced the wrong
    /// weight entirely -- against the flat realm fills it read as a hole
    /// punched through the map rather than a watercourse. Each step keeps
    /// the realm or sea fill already painted underneath and draws only a
    /// single directional glyph (`-`, `|`, `/`, `\`) over it in a dark
    /// water blue, so the river has both a thinner silhouette and a water
    /// identity instead of reading as pure-black absence.
    fn draw_river(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        seeds: &[Tile; NUM_REGIONS],
        total_cols: i32,
        total_rows: i32,
    ) {
        let waypoints: Vec<Tile> = RIVER_WAYPOINTS
            .iter()
            .map(|&(fx, fy)| {
                Tile::new(
                    ((fx * total_cols as f32) as i32).clamp(0, total_cols - 1),
                    ((fy * total_rows as f32) as i32).clamp(0, total_rows - 1),
                )
            })
            .collect();

        let mut tiles: Vec<Tile> = Vec::new();
        for pair in waypoints.windows(2) {
            for tile in hex_line(LAYOUT, pair[0], pair[1]) {
                if tiles.last() != Some(&tile) {
                    tiles.push(tile);
                }
            }
        }

        // Walking every intervening screen cell between two hex centers
        // (rather than only the centers themselves) is still required:
        // adjacent hex centers on this pitch can sit up to a full
        // [`PITCH_X`] apart in screen columns, and painting only the
        // endpoints leaves gaps wide enough to read as scattered
        // disconnected marks instead of a continuous river. What changed is
        // *how* each intervening cell is painted: a directional glyph over
        // the existing fill, not a solid space over a flat river color.
        let mut points: Vec<Cell> = Vec::new();
        for pair in tiles.windows(2) {
            let a = LAYOUT.center_cell(pair[0]);
            let b = LAYOUT.center_cell(pair[1]);
            for cell in bresenham_cells(a.x, a.y, b.x, b.y) {
                if points.last() != Some(&cell) {
                    points.push(cell);
                }
            }
        }

        let river = rgb(24, 54, 98);
        for i in 0..points.len() {
            let cur = points[i];
            // The glyph at a point is chosen from the step *leaving* it
            // (falling back to the step arriving at it for the final
            // point), so a run of same-row steps draws as a dash, a run of
            // same-column steps as a pipe, and a diagonal step picks the
            // slash that matches its slope -- a drawn line's texture
            // instead of a uniform block.
            let (dx, dy) = if i + 1 < points.len() {
                (points[i + 1].x - cur.x, points[i + 1].y - cur.y)
            } else if i > 0 {
                (cur.x - points[i - 1].x, cur.y - points[i - 1].y)
            } else {
                (1, 0)
            };
            let glyph = river_glyph(dx, dy);
            self.draw_river_cell(surface, area, seeds, cur.x, cur.y, glyph, river);
        }
    }

    /// Writes one river glyph at `(x, y)`, over whatever fill is already
    /// there rather than replacing it: looking the underlying tile's owner
    /// back up (the same `nearest_region` + `face_of` pair
    /// [`Self::draw_region_name`] uses for its letters) keeps the realm or
    /// sea color visible on both sides of the stroke, so the river reads as
    /// a line drawn on the map rather than a gap cut through it.
    #[allow(clippy::too_many_arguments)]
    fn draw_river_cell(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        seeds: &[Tile; NUM_REGIONS],
        x: i32,
        y: i32,
        glyph: char,
        color: Color,
    ) {
        if x < 0 || y < 0 || x >= i32::from(area.width()) || y >= i32::from(area.height()) {
            return;
        }
        let tile = LAYOUT.cell_to_tile(Cell::new(x, y));
        let owner = nearest_region(tile, seeds);
        let bg = self.face_of(owner, tile);
        put_clipped(surface, area, x, y, glyph, Style::new().fg(color).bg(bg));
    }

    /// Pulsing outline around the selected realm's capital hex. The pulse
    /// itself is what keeps this demo visibly animating when no envoy is in
    /// flight and no ambient tick has just landed.
    fn draw_selection(&self, surface: &mut Surface<'_>, area: Rect, seeds: &[Tile; NUM_REGIONS]) {
        let Some(capital) = seeds.get(self.selected) else {
            return;
        };
        let cell = LAYOUT.tile_to_cell(*capital);
        let pulse = (self.time * 3.0).sin().mul_add(0.5, 0.5);
        let color = mix(rgb(246, 196, 96), palette::WHITE, pulse * 0.5);
        let rows = PITCH_Y + 1;
        for dx in 0..PITCH_X {
            put_ring(surface, area, cell.x + dx, cell.y, color);
            put_ring(surface, area, cell.x + dx, cell.y + rows - 1, color);
        }
        for dy in 0..rows {
            put_ring(surface, area, cell.x, cell.y + dy, color);
            put_ring(surface, area, cell.x + PITCH_X - 1, cell.y + dy, color);
        }
    }

    // ── Chrome ───────────────────────────────────────────────────────────

    fn draw_action_bar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let (status_row, buttons_area) = panel::split_top(area, 1);
        self.draw_status_line(surface, status_row);
        if buttons_area.height() == 0 {
            return;
        }

        let realm = &self.realms[self.selected];
        let can_send =
            matches!(realm.allegiance, Allegiance::Independent(_)) && realm.cooldown <= 0.0;

        let cols = panel::columns(buttons_area, 3, 1);
        self.draw_button(surface, cols[0], "< PREV", Action::Cycle(-1), true);
        self.draw_button(surface, cols[1], "SEND ENVOY", Action::SendEnvoy, can_send);
        self.draw_button(surface, cols[2], "NEXT >", Action::Cycle(1), true);
    }

    fn draw_status_line(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() < 4 {
            return;
        }
        let region = &REGIONS[self.selected];
        let realm = &self.realms[self.selected];
        let allegiance_text = match realm.allegiance {
            Allegiance::Home => "your home realm".to_owned(),
            Allegiance::Player => "sworn to you".to_owned(),
            Allegiance::Rival => "held by a rival".to_owned(),
            Allegiance::Independent(d) => format!("independent, disposition {:.0}%", d * 100.0),
        };
        let cooldown_text = if realm.cooldown > 0.0 {
            format!("  (envoy resting {:.1}s)", realm.cooldown)
        } else {
            String::new()
        };
        let ok_color = if self.message_ok {
            rgb(120, 210, 120)
        } else {
            ui::DIM
        };
        panel::spans(
            surface,
            (area.left() + 1, area.top()),
            area.width().saturating_sub(2),
            &[
                panel::Span::keyword(region.name),
                panel::Span::dim(&format!(" -- {allegiance_text}{cooldown_text}   ")),
                panel::Span::new(&self.message, ok_color),
            ],
            ui::CHROME_BG,
        );
    }

    fn draw_button(
        &mut self,
        surface: &mut Surface<'_>,
        rect: Rect,
        label: &str,
        action: Action,
        enabled: bool,
    ) {
        let bg = if enabled {
            rgb(64, 52, 28)
        } else {
            rgb(28, 28, 34)
        };
        let fg = if enabled { ui::ACCENT } else { ui::DIM };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.width() > 2 && rect.height() > 0 {
            let text = truncate(label, rect.width_usize().saturating_sub(2));
            let text_len = text.chars().count() as u16;
            let x = rect.left() + rect.width().saturating_sub(text_len) / 2;
            let y = rect.top() + rect.height() / 2;
            surface.print((x, y), text, Style::new().fg(fg).bg(bg));
        }
        if enabled {
            self.hotspots.push_tappable(rect, rect, action);
        }
    }
}

/// Nearest region by hex distance to each region's seed tile: a Voronoi
/// partition of the hex field, recomputed every frame from whatever seed
/// tiles [`QuietMarch::seed_tiles`] derived for the current panel size.
/// Iterates the fixed-size `REGIONS` array in order, so ties (which only
/// happen exactly at a boundary) always resolve to the lower index rather
/// than to whichever region happened to be considered last -- the ordinary
/// "deterministic tie-break" rule every generated map in this gallery needs.
fn nearest_region(tile: Tile, seeds: &[Tile; NUM_REGIONS]) -> usize {
    seeds
        .iter()
        .enumerate()
        .map(|(i, &s)| (LAYOUT.distance(tile, s), i))
        .min_by_key(|&(d, _)| d)
        .map_or(0, |(_, i)| i)
}

/// Draws one hex's flat footprint: the taper-row shape [`07_hex_tiles`] uses,
/// without its bevel -- Divine Right's own hexes are printed as one flat
/// color, not shaded, and reproducing the bevel here would read as a modern
/// UI tile rather than a printed plate.
fn draw_hex_face(surface: &mut Surface<'_>, area: Rect, sx: i32, sy: i32, face: Color) {
    let rows = PITCH_Y + 1;
    for dy in 0..rows {
        let taper = if dy == 0 || dy == rows - 1 {
            PITCH_X / 4
        } else {
            0
        };
        for dx in taper..(PITCH_X - taper) {
            put_clipped(surface, area, sx + dx, sy + dy, ' ', Style::new().bg(face));
        }
    }
}

/// Three fixed waypoints (as fractions of the hex field, like the realm
/// seeds) that a river runs through on its way to the sea.
const RIVER_WAYPOINTS: [(f32, f32); 3] = [(0.52, 0.36), (0.40, 0.58), (0.14, 0.76)];

/// Every cell on the Bresenham line between `(x0, y0)` and `(x1, y1)`,
/// inclusive of both endpoints, in walk order.
///
/// Adjacent hex centers on this pitch can sit up to a full [`PITCH_X`] apart
/// in screen columns, so [`QuietMarch::draw_river`] needs every intervening
/// cell (not just the two endpoints) to keep the river continuous: painting
/// only the endpoints left wide unpainted gaps that, on a hex field with
/// varied fill colors underneath, read as scattered disconnected marks
/// rather than a river.
fn bresenham_cells(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<Cell> {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    let mut out = Vec::new();
    loop {
        out.push(Cell::new(x, y));
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    out
}

/// The line-drawing glyph for a step of `(dx, dy)` screen cells: a dash for
/// a level step, a pipe for a vertical step, and the slash matching the
/// step's diagonal otherwise. Picking a glyph per step (rather than a
/// uniform filled cell) is what lets [`QuietMarch::draw_river`] read as a
/// drawn line with texture instead of a blocky staircase.
const fn river_glyph(dx: i32, dy: i32) -> char {
    match (dx.signum(), dy.signum()) {
        (0, _) => '|',
        (_, 0) => '-',
        (a, b) if a == b => '\\',
        _ => '/',
    }
}

/// A small static compass rose in the map's top-right corner. Static rather
/// than animated: a compass needle that moves on its own would read as a
/// broken instrument, not a decoration -- the demo's animation budget is
/// spent on the things that are actually supposed to move (the sea, the
/// selection ring, the map's own allegiance).
fn draw_compass(surface: &mut Surface<'_>, area: Rect) {
    if area.width() < 10 || area.height() < 8 {
        return;
    }
    let cx = i32::from(area.width()) - 5;
    let cy = 3;
    let dim = rgb(160, 156, 172);
    let glyphs: [(i32, i32, char); 5] = [
        (0, -2, 'N'),
        (0, -1, '\u{2191}'),
        (0, 1, '\u{2193}'),
        (0, 2, 'S'),
        (0, 0, '+'),
    ];
    for &(dx, dy, ch) in &glyphs {
        put_ring_glyph(surface, area, cx + dx, cy + dy, ch, dim);
    }
    put_ring_glyph(surface, area, cx - 2, cy, 'W', dim);
    put_ring_glyph(surface, area, cx - 1, cy, '\u{2190}', dim);
    put_ring_glyph(surface, area, cx + 1, cy, '\u{2192}', dim);
    put_ring_glyph(surface, area, cx + 2, cy, 'E', dim);
}

fn put_ring_glyph(
    surface: &mut Surface<'_>,
    area: Rect,
    cx: i32,
    cy: i32,
    glyph: char,
    color: Color,
) {
    if cx < 0 || cy < 0 || cx >= i32::from(area.width()) || cy >= i32::from(area.height()) {
        return;
    }
    surface.put(
        (area.left() + cx as u16, area.top() + cy as u16),
        glyph,
        Style::new().fg(color).bg(palette::BLACK),
    );
}

/// Writes one background-only cell (a solid block of `color`), clipped to
/// `area`. Used for the river and the selection ring, both of which overlay
/// whatever the hex fill pass already drew rather than replacing a glyph.
fn put_ring(surface: &mut Surface<'_>, area: Rect, cx: i32, cy: i32, color: Color) {
    if cx < 0 || cy < 0 || cx >= i32::from(area.width()) || cy >= i32::from(area.height()) {
        return;
    }
    surface.put(
        (area.left() + cx as u16, area.top() + cy as u16),
        ' ',
        Style::new().bg(color),
    );
}

/// Writes one glyph, clipped to `area`.
fn put_clipped(surface: &mut Surface<'_>, area: Rect, cx: i32, cy: i32, glyph: char, style: Style) {
    if cx < 0 || cy < 0 || cx >= i32::from(area.width()) || cy >= i32::from(area.height()) {
        return;
    }
    surface.put(
        (area.left() + cx as u16, area.top() + cy as u16),
        glyph,
        style,
    );
}

impl Demo for QuietMarch {
    const NAME: &'static str = "52_quiet_march";
    const TITLE: &'static str = "52 Quiet March";
    const BLURB: &'static str = "Divine Right: letter-spaced realm names printed across a hex map.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("[ / ]", "cycle realm"),
            ("Enter/Space", "send envoy"),
            ("tap", "select / act"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        for realm in &mut self.realms {
            realm.cooldown = (realm.cooldown - dt).max(0.0);
        }
        // A `while` rather than an `if`: a very large `dt` (a stalled
        // backend, or the headless harness stepping in big jumps) must still
        // apply every ambient tick it crossed, one at a time, so the map's
        // state only ever depends on total elapsed time, never on how that
        // time was chopped into frames.
        let due = (self.time / AMBIENT_INTERVAL) as u32;
        while self.last_tick < due {
            self.last_tick += 1;
            self.ambient_tick(self.last_tick);
        }

        if !self.handle_events(term) {
            return false;
        }
        self.hotspots.clear();
        self.handle_pointer();

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        let bar_h = if shape.stacks() { 6 } else { 5 };
        let (map_area, bar_area) = panel::split_bottom(content, bar_h);

        self.draw_map(&mut surface, map_area);
        self.draw_action_bar(&mut surface, bar_area);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(QuietMarch);

#[cfg(test)]
mod tests {
    use super::{NUM_REALMS, REGIONS, place_name, spaced_name};
    use std::collections::HashSet;

    #[test]
    fn region_names_are_unique() {
        let mut seen = HashSet::new();
        for region in &REGIONS {
            assert!(
                seen.insert(region.name),
                "duplicate region name: {}",
                region.name
            );
        }
    }

    #[test]
    fn place_names_are_unique_and_distinct_from_region_names() {
        let mut seen: HashSet<String> = REGIONS.iter().map(|r| r.name.to_owned()).collect();
        let before = seen.len();
        for realm in 0..NUM_REALMS {
            for which in 0..2 {
                let name = place_name(realm * 2 + which);
                assert!(seen.insert(name.to_owned()), "duplicate place name: {name}");
            }
        }
        assert_eq!(seen.len(), before + NUM_REALMS * 2);
    }

    #[test]
    fn spaced_name_letter_spaces_within_a_word_and_widens_between_words() {
        assert_eq!(spaced_name("MUETAR"), "M U E T A R");
        assert_eq!(spaced_name("THE SEA"), "T H E   S E A");
    }
}
