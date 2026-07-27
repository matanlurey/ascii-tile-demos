//! 22: Overworld quest -- a Zelda-style action-RPG overworld, after the
//! ASCII game `Zelda RL`.
//!
//! Where most of this gallery's demos treat the grid as a strategy map read
//! from a distance, `Zelda RL` treats it as an action-game viewport: a dozen
//! or so creatures on screen at once, each a single colored letter, moving
//! and fighting in real time while the player explores. The interesting
//! problem that shape poses is not the terrain (it is ordinary noise-based
//! ground and cliffs) but the *bookkeeping*: with entities constantly
//! spawning, wandering, fighting, and dying, how does the interface stay
//! honest about who is currently on screen without either hand-maintaining
//! a list or drawing something stale?
//!
//! Techniques on show:
//!
//! - **Elevation as dithered terraces, not a color ramp.** Height is
//!   quantized into a handful of bands and drawn flat within each band; where
//!   two bands meet, a strip of [`dithered_glyph`] cliff-face glyphs marks the
//!   step. A hard elevation discontinuity reads as a *rock face* this way,
//!   which a smooth color gradient (this gallery's usual elevation treatment,
//!   see [`06_iso_elevation`](../06_iso_elevation)) cannot do: a gradient says
//!   "higher", dithering says "here is where you cannot walk".
//! - **A two-tone canopy.** Forest tiles pick between two colors from a
//!   second, decorrelated noise field, so a wood reads as textured rather
//!   than as one flat green, without a second terrain pass or any new state.
//! - **A legend derived from the frame, not maintained by hand.** Every tick
//!   collects the distinct entities currently inside the viewport and renders
//!   the legend from that set, in first-seen order. There is no
//!   "add to legend" call anywhere in the combat code; the legend is a pure
//!   function of "what is on screen right now", so it can never drift from
//!   the map and shrinks correctly the moment something walks off the edge.
//! - **A life meter built from whole and half units.** Each heart is one `V`
//!   (full, bright red) or `v` (a fought-down remainder, dim red), which is
//!   the same half-unit-of-precision idea [`ui::panel::bar`] uses for a
//!   gauge, applied to discrete hearts instead of a continuous fill.
//! - **An ASCII-art (not box-drawing) inventory frame.** Drawn from literal
//!   `/-\`, `|=|`, `\-/` punctuation rows rather than `┌─┐│└┘`, deliberately:
//!   next to this gallery's box-drawn panels (see
//!   [`18_panel_chrome`](../18_panel_chrome)), a punctuation frame reads as a
//!   different, older register of ASCII art, which is exactly the register
//!   the reference game itself uses.
//! - **A combat log assembled from templates.** Two nearby, opposed entities
//!   occasionally fight; the outcome picks a verb from a small phrase bank
//!   keyed to how much damage landed, so the log reads as prose rather than
//!   as a number stream, while staying entirely procedural.
//!
//! ```sh
//! cargo run --example 22_overworld_quest --features crossterm
//! cargo run --example 22_overworld_quest --features software
//! cargo run --example 22_overworld_quest --features gl
//! cargo run --example 22_overworld_quest  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::glyphs::dithered_glyph;
use tilekit::noise::{Rng, fbm, hash01};
use tilekit::palette::{mix, rgb, scale};

/// World size in cells. Larger than the viewport in both axes so panning
/// (walking) has somewhere to go.
const WORLD_W: i32 = 140;
/// See [`WORLD_W`].
const WORLD_H: i32 = 90;

/// How many discrete elevation terraces the heightfield is quantized into.
///
/// Four reads as "lowland, upland, high ground, peak" at a glance; more than
/// about five and the bands become too close in color to tell apart at this
/// palette's saturation.
const TERRACES: i32 = 4;

/// World-seconds between wander/combat ticks for the entity simulation.
///
/// Deliberately much slower than the frame rate: this is a turn-taking sim
/// wearing a real-time frame, and ticking it every frame would make every
/// entity twitch every frame instead of taking one legible action at a time.
const SIM_PERIOD: f32 = 0.6;

/// Maximum entities alive at once, matching the reference screenshot's
/// density: enough to feel like a living map, not so many that the legend
/// panel cannot fit them.
const MAX_ENTITIES: usize = 9;

/// A kind of creature or feature, with its glyph, base color, and combat
/// flavor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Player,
    Crow,
    Zola,
    Octoroc,
    Monastery,
}

impl Kind {
    /// CP437 glyph. `@` for both the player and hostile humanoids
    /// (differentiated by color, exactly as the reference legend does with
    /// green vs. red `@`), plain letters for the rest.
    const fn glyph(self) -> char {
        match self {
            Self::Player | Self::Zola => '@',
            Self::Crow => 'c',
            Self::Octoroc => 'o',
            Self::Monastery => '#',
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Player => rgb(96, 220, 96),
            Self::Crow => rgb(224, 224, 224),
            Self::Zola => rgb(96, 200, 96),
            Self::Octoroc => rgb(214, 72, 64),
            Self::Monastery => rgb(224, 140, 180),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Player => "Archer",
            Self::Crow => "Crow",
            Self::Zola => "Zola",
            Self::Octoroc => "Red Octoroc",
            Self::Monastery => "Forest Monastery",
        }
    }

    /// Whether this kind ever attacks or is attacked. The monastery is
    /// scenery wearing an entity's clothes (it needs a legend entry and a
    /// map position, nothing else).
    const fn combatant(self) -> bool {
        !matches!(self, Self::Monastery)
    }

    /// Whether this kind is hostile to the player, for log coloring and
    /// targeting.
    const fn hostile(self) -> bool {
        matches!(self, Self::Crow | Self::Octoroc)
    }
}

/// A single creature or feature on the map.
struct Entity {
    kind: Kind,
    x: i32,
    y: i32,
    hp: f32,
    max_hp: f32,
    alive: bool,
}

impl Entity {
    const fn new(kind: Kind, x: i32, y: i32, max_hp: f32) -> Self {
        Self {
            kind,
            x,
            y,
            hp: max_hp,
            max_hp,
            alive: true,
        }
    }
}

/// Terrain classification, coarser than a full biome model: this demo only
/// needs "can I stand here" and "which glyph family", not the whole
/// Whittaker gamut.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ground {
    Open,
    ForestLight,
    ForestDark,
    Cliff,
}

/// A combat outcome phrase bank, keyed by how hard the hit landed, so the log
/// reads as description rather than as a damage number.
const GLANCING: &[&str] = &["barely scratches", "grazes", "fails to trouble"];
const SOLID: &[&str] = &["strikes", "catches", "lands a blow on"];
const HEAVY: &[&str] = &["slams into", "batters", "crushes"];
const DODGE: &[&str] = &["shuns the attack", "slips aside", "dances back"];

/// State: the generated ground, the entity roster, camera offset, combat
/// log, and simulation clock.
pub struct OverworldQuest {
    seed: u32,
    entities: Vec<Entity>,
    camera_x: i32,
    camera_y: i32,
    time: f32,
    next_sim: f32,
    log: panel::Log,
    fps: FpsMeter,
    player_hp: f32,
    player_max_hp: f32,
    rng: Rng,
}

impl Default for OverworldQuest {
    fn default() -> Self {
        Self::from_seed(7)
    }
}

impl OverworldQuest {
    fn from_seed(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let mut entities = vec![Entity::new(Kind::Player, WORLD_W / 2, WORLD_H / 2, 16.0)];
        // The monastery is a fixed landmark near the player's start, matching
        // the reference's stationary building.
        entities.push(Entity::new(
            Kind::Monastery,
            WORLD_W / 2 + 6,
            WORLD_H / 2 + 4,
            f32::INFINITY,
        ));
        let mut demo = Self {
            seed,
            entities,
            camera_x: WORLD_W / 2,
            camera_y: WORLD_H / 2,
            time: 0.0,
            next_sim: SIM_PERIOD,
            log: panel::Log::new(6),
            fps: FpsMeter::new(),
            player_hp: 16.0,
            player_max_hp: 16.0,
            rng: Rng::new(seed ^ 0x9E37_79B9),
        };
        for _ in 0..(MAX_ENTITIES - demo.entities.len()) {
            demo.spawn_wild();
        }
        let _ = &mut rng; // seeds demo.rng instead; kept for clarity of intent.
        demo
    }

    fn reroll(&mut self) {
        *self = Self::from_seed(self.seed.wrapping_add(1));
    }

    /// Heightfield in `0.0..=1.0` before terracing, via `fbm` so the result is
    /// a plain function of position rather than stored state.
    fn height(&self, x: i32, y: i32) -> f32 {
        fbm(self.seed, x as f32 * 0.06, y as f32 * 0.06, 4, 0.55)
    }

    /// Which terrace band `(x, y)` falls in, in `0..TERRACES`.
    fn terrace(&self, x: i32, y: i32) -> i32 {
        (self.height(x, y) * TERRACES as f32).floor() as i32
    }

    /// Ground classification at `(x, y)`, from the terrace and a second,
    /// decorrelated noise field that decides forest cover.
    ///
    /// A different seed and coordinate scale than [`height`](Self::height) is
    /// the whole trick: reusing the same field would make forest cover
    /// perfectly correlated with elevation, so every terrace would be either
    /// all trees or all clear. Decorrelating them is what gives the map
    /// patches of forest that cross terrace boundaries, the way the reference
    /// screenshot's tree clumps sit astride its terraces.
    fn ground(&self, x: i32, y: i32) -> Ground {
        if self.terrace(x, y) != self.terrace(x + 1, y)
            || self.terrace(x, y) != self.terrace(x, y + 1)
            || self.terrace(x, y) != self.terrace(x - 1, y)
            || self.terrace(x, y) != self.terrace(x, y - 1)
        {
            return Ground::Cliff;
        }
        let cover = fbm(
            self.seed ^ 0xA5A5_5A5A,
            (x as f32).mul_add(0.11, 100.0),
            (y as f32).mul_add(0.11, 100.0),
            3,
            0.5,
        );
        if cover > 0.62 {
            // A third noise sample, at a different frequency again, chooses
            // which of the two canopy colors: a coarser field than `cover`'s
            // own, so each patch of forest holds one shade for several tiles
            // rather than flickering tile to tile.
            let tone = fbm(
                self.seed ^ 0x1234_5678,
                x as f32 * 0.03,
                y as f32 * 0.03,
                2,
                0.5,
            );
            if tone > 0.5 {
                Ground::ForestDark
            } else {
                Ground::ForestLight
            }
        } else {
            Ground::Open
        }
    }

    /// Whether an entity may stand or move onto `(x, y)`.
    fn passable(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < WORLD_W && y < WORLD_H && self.ground(x, y) != Ground::Cliff
    }

    fn player_pos(&self) -> (i32, i32) {
        let p = &self.entities[0];
        (p.x, p.y)
    }

    fn try_move_player(&mut self, dx: i32, dy: i32) {
        let (x, y) = self.player_pos();
        let (nx, ny) = (x + dx, y + dy);
        if self.passable(nx, ny) {
            self.entities[0].x = nx;
            self.entities[0].y = ny;
        }
    }

    /// Places a new hostile or ally at a passable cell within wander range of
    /// the player, replacing whatever cap the roster has room for.
    fn spawn_wild(&mut self) {
        let (px, py) = self.player_pos();
        for _ in 0..40 {
            let dx = (self.rng.next_below(23) as i32) - 11;
            let dy = (self.rng.next_below(23) as i32) - 11;
            let (x, y) = (px + dx, py + dy);
            if !self.passable(x, y) || (dx.abs() < 3 && dy.abs() < 3) {
                continue;
            }
            let kind = *self
                .rng
                .choose(&[Kind::Crow, Kind::Zola, Kind::Octoroc])
                .unwrap_or(&Kind::Crow);
            let hp = match kind {
                Kind::Zola => 8.0,
                Kind::Octoroc => 10.0,
                Kind::Crow | Kind::Player | Kind::Monastery => 6.0,
            };
            self.entities.push(Entity::new(kind, x, y, hp));
            return;
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        let mut attack = false;
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.try_move_player(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.try_move_player(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.try_move_player(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.try_move_player(1, 0),
                    KeyCode::Char(' ') => attack = true,
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                }
            }
        }
        if attack {
            self.player_attack_adjacent();
        }
        true
    }

    /// The player swings at the nearest hostile within one step, if any.
    fn player_attack_adjacent(&mut self) {
        let (px, py) = self.player_pos();
        let target = self.entities.iter().position(|e| {
            e.alive && e.kind.hostile() && (e.x - px).abs() <= 1 && (e.y - py).abs() <= 1
        });
        if let Some(i) = target {
            let dmg = self.rng.next_f32().mul_add(2.5, 2.0);
            self.resolve_hit(Kind::Player, i, dmg);
        }
    }

    /// Applies `dmg` from the player (or narration only, for entity-on-entity
    /// fights) to `target`, logs a phrase for it, and clears the target if it
    /// dies.
    fn resolve_hit(&mut self, attacker: Kind, target: usize, dmg: f32) {
        let name = self.entities[target].kind.name();
        let attacker_name = attacker.name();
        let bank = if dmg < 2.0 {
            GLANCING
        } else if dmg < 4.5 {
            SOLID
        } else {
            HEAVY
        };
        let verb = self.rng.choose(bank).copied().unwrap_or("hits");
        let color = if self.entities[target].kind.hostile() {
            rgb(214, 120, 96)
        } else {
            rgb(150, 214, 150)
        };
        self.log
            .push(format!("The {attacker_name} {verb} the {name}..."), color);
        self.entities[target].hp -= dmg;
        if self.entities[target].hp <= 0.0 {
            self.entities[target].alive = false;
            self.log
                .push(format!("The {name} falls!"), rgb(224, 200, 96));
        }
    }

    /// One step of the slow-tick simulation: wander idle entities, and let
    /// two adjacent opposed entities occasionally trade a blow. This is the
    /// whole "living overworld" effect; everything else in `tick` is drawing.
    fn simulate(&mut self) {
        // Sweep indices rather than entities directly, so the borrow checker
        // does not have to referee two live entities at once; `resolve_hit`
        // and the wander step each take one index at a time.
        let alive: Vec<usize> = (1..self.entities.len())
            .filter(|&i| self.entities[i].alive)
            .collect();

        for &i in &alive {
            if !self.entities[i].kind.combatant() {
                continue;
            }
            // Look for an adjacent opposed combatant to fight instead of
            // wandering; hostiles fight the player or allies, allies fight
            // hostiles.
            let (x, y, hostile) = (
                self.entities[i].x,
                self.entities[i].y,
                self.entities[i].kind.hostile(),
            );
            let foe = (0..self.entities.len()).find(|&j| {
                j != i
                    && self.entities[j].alive
                    && self.entities[j].kind.combatant()
                    && self.entities[j].kind.hostile() != hostile
                    && (self.entities[j].x - x).abs() <= 1
                    && (self.entities[j].y - y).abs() <= 1
            });

            if let Some(j) = foe {
                // A third of the time nothing lands, which is what makes
                // "shuns the attack" a real outcome rather than flavor text
                // that never fires.
                if self.rng.next_f32() < 0.32 {
                    let attacker = self.entities[i].kind;
                    let defender = self.entities[j].kind.name();
                    let verb = self.rng.choose(DODGE).copied().unwrap_or("dodges");
                    self.log
                        .push(format!("The {defender} {verb}!"), rgb(150, 170, 214));
                    let _ = attacker;
                } else {
                    let dmg = self.rng.next_f32().mul_add(3.0, 1.0);
                    let attacker = self.entities[i].kind;
                    if j == 0 {
                        self.player_hp = (self.player_hp - dmg).max(0.0);
                        let name = attacker.name();
                        self.log.push(
                            format!("The {name} strikes you for {dmg:.0}!"),
                            rgb(214, 96, 96),
                        );
                    } else {
                        self.resolve_hit(attacker, j, dmg);
                    }
                }
            } else {
                self.wander(i);
            }
        }

        // Retire the dead and keep the roster topped up, so the map never
        // quietly empties out over a long session.
        self.entities.retain(|e| e.alive);
        while self.entities.len() < MAX_ENTITIES {
            self.spawn_wild();
        }

        // Slow regen keeps the player's life meter animating even when no
        // fight is underway, which is what the reference's meter is doing
        // most of the time.
        self.player_hp = (self.player_hp + 0.15).min(self.player_max_hp);
    }

    fn wander(&mut self, i: usize) {
        let (x, y) = (self.entities[i].x, self.entities[i].y);
        let dirs = [(0, -1), (0, 1), (-1, 0), (1, 0), (0, 0)];
        let (dx, dy) = *self.rng.choose(&dirs).unwrap_or(&(0, 0));
        let (nx, ny) = (x + dx, y + dy);
        if self.passable(nx, ny) {
            self.entities[i].x = nx;
            self.entities[i].y = ny;
        }
    }

    /// The ground's own background color at `(wx, wy)`, independent of
    /// whatever glyph is drawn there.
    ///
    /// Factored out of [`draw_ground`](Self::draw_ground) so an entity glyph
    /// drawn on top of a tile (see [`draw_map`](Self::draw_map)) can recompute
    /// the same color rather than reading it back from the surface: `Surface`
    /// has no read accessor for a cell it already wrote, only `grid_mut`'s
    /// unclipped escape hatch, so a pure function of world position is both
    /// simpler and cheaper than round-tripping through the grid.
    fn ground_bg(&self, wx: i32, wy: i32) -> Color {
        let terrace = self.terrace(wx, wy);
        let band = terrace as f32 / (TERRACES - 1).max(1) as f32;

        match self.ground(wx, wy) {
            Ground::Cliff => {
                if terrace % 2 == 0 {
                    rgb(46, 46, 52)
                } else {
                    rgb(58, 54, 30)
                }
            }
            Ground::ForestDark => rgb(24, 30, 20),
            Ground::ForestLight => rgb(28, 36, 24),
            Ground::Open => mix(rgb(14, 20, 14), rgb(26, 34, 22), band),
        }
    }

    /// Draws the ground layer (terraces, cliffs, forest) for one screen cell.
    fn draw_ground(&self, surface: &mut Surface<'_>, sx: u16, sy: u16, wx: i32, wy: i32) {
        let terrace = self.terrace(wx, wy);
        let bg = self.ground_bg(wx, wy);

        match self.ground(wx, wy) {
            Ground::Cliff => {
                // Two cliff-face palettes alternate with the terrace parity,
                // matching the reference's grey-vs-olive terrace faces.
                let hi = if terrace % 2 == 0 {
                    rgb(96, 96, 106)
                } else {
                    rgb(122, 112, 58)
                };
                let t = hash01(self.seed ^ 0x51, wx, wy);
                let glyph = dithered_glyph(&['\u{2591}', '\u{2592}', '\u{2593}'], t, wx, wy);
                surface.put((sx, sy), glyph, Style::new().fg(hi).bg(bg));
            }
            Ground::ForestLight | Ground::ForestDark => {
                let fg = if self.ground(wx, wy) == Ground::ForestDark {
                    rgb(150, 150, 96)
                } else {
                    rgb(146, 156, 118)
                };
                surface.put((sx, sy), '\u{00a5}', Style::new().fg(fg).bg(bg));
            }
            Ground::Open => {
                let dot = hash01(self.seed ^ 0x99, wx, wy) < 0.35;
                if dot {
                    surface.put((sx, sy), '.', Style::new().fg(rgb(70, 110, 66)).bg(bg));
                } else {
                    surface.put((sx, sy), ' ', Style::new().bg(bg));
                }
            }
        }
    }

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let w = i32::from(area.width());
        let h = i32::from(area.height());
        let left = self.camera_x - w / 2;
        let top = self.camera_y - h / 2;

        for row in 0..h {
            for col in 0..w {
                let (wx, wy) = (left + col, top + row);
                let (sx, sy) = (area.left() + col as u16, area.top() + row as u16);
                if wx < 0 || wy < 0 || wx >= WORLD_W || wy >= WORLD_H {
                    surface.put((sx, sy), ' ', Style::new().bg(rgb(4, 4, 6)));
                    continue;
                }
                self.draw_ground(surface, sx, sy, wx, wy);
            }
        }

        // Entities drawn in a second pass, back to front by row, so a lower
        // (visually nearer) entity's glyph is never eclipsed by a farther
        // one sharing the same cell mid-transition.
        let mut order: Vec<usize> = (0..self.entities.len()).collect();
        order.sort_by_key(|&i| self.entities[i].y);
        for i in order {
            let e = &self.entities[i];
            if !e.alive {
                continue;
            }
            let (col, row) = (e.x - left, e.y - top);
            if col < 0 || row < 0 || col >= w || row >= h {
                continue;
            }
            let (sx, sy) = (area.left() + col as u16, area.top() + row as u16);
            let bg = self.ground_bg(e.x, e.y);
            surface.put(
                (sx, sy),
                e.kind.glyph(),
                Style::new().fg(e.kind.color()).bg(bg),
            );
        }
    }

    /// Collects the entities currently inside `area` (given the same camera
    /// math [`draw_map`](Self::draw_map) uses) as `(glyph, color, name)`
    /// triples, first-seen order, each name appearing once.
    ///
    /// This is the legend's entire data source. There is deliberately no
    /// other list anywhere that decides what belongs in the legend: whatever
    /// this returns is what is drawn, so the legend can never show something
    /// that walked off screen or omit something that walked on.
    ///
    /// The color dims for an entry whose *every* visible instance is below
    /// half health, so a legend name can itself say "this one is hurt"
    /// without a second panel: a wounded Crow's row goes duller the way a
    /// wounded unit's portrait desaturates in the reference strategy games.
    fn visible_legend(&self, area: Rect) -> Vec<(char, Color, &'static str)> {
        let w = i32::from(area.width());
        let h = i32::from(area.height());
        let left = self.camera_x - w / 2;
        let top = self.camera_y - h / 2;

        let mut seen: Vec<(char, Color, &'static str, bool)> = Vec::new();
        for e in &self.entities {
            if !e.alive {
                continue;
            }
            let (col, row) = (e.x - left, e.y - top);
            if col < 0 || row < 0 || col >= w || row >= h {
                continue;
            }
            let name = e.kind.name();
            let wounded = e.hp < e.max_hp * 0.5;
            if let Some(entry) = seen.iter_mut().find(|(_, _, n, _)| *n == name) {
                entry.3 &= wounded;
            } else {
                seen.push((e.kind.glyph(), e.kind.color(), name, wounded));
            }
        }
        seen.into_iter()
            .map(|(glyph, color, name, wounded)| {
                let color = if wounded { scale(color, 0.6) } else { color };
                (glyph, color, name)
            })
            .collect()
    }

    /// Draws the top-left ASCII-art equipment box: a sword/scroll frame built
    /// from `/-\`, `|=|`, `\-/` rows, deliberately not box-drawing glyphs. See
    /// the module docs for why the distinction matters.
    fn draw_inventory(&self, surface: &mut Surface<'_>, area: Rect) {
        const ROWS: [&str; 6] = [
            "/-\\ /---\\",
            "|=|  $  |",
            "|=| @   |",
            "|=|  *  |",
            "|=|     |",
            "\\-/ \\---/",
        ];
        let panel_w = 11u16;
        let panel_h = ROWS.len() as u16;
        if area.width() < panel_w || area.height() < panel_h {
            return;
        }
        let frame = rgb(150, 150, 96);
        let (x0, y0) = (area.left(), area.top());
        for (row, text) in ROWS.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let color = match ch {
                    '$' => rgb(224, 204, 96),
                    '@' => Kind::Player.color(),
                    '*' => rgb(224, 96, 96),
                    _ => frame,
                };
                surface.put(
                    (x0 + col as u16, y0 + row as u16),
                    ch,
                    Style::new().fg(color).bg(ui::BG),
                );
            }
        }
        surface.print(
            (x0 + 4, y0 + 1),
            &format!("{:>4}", (self.time as u32 * 37 % 900) + 12),
            Style::new().fg(rgb(224, 204, 96)).bg(ui::BG),
        );
    }

    /// Draws the top-right region name, life meter, and legend.
    ///
    /// Only reachable once the window is wide enough to afford a sidebar;
    /// below that threshold [`tick`](Demo::tick) still draws the life meter
    /// on its own, directly over the map, since a life total is the one
    /// reading in this whole panel that must survive every window size.
    fn draw_side_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 14 || area.height() < 2 {
            return;
        }
        surface.print(
            (area.left(), area.top()),
            "Hyrule",
            Style::new().fg(ui::FG).bg(ui::BG),
        );
        let label = "-- LIFE --";
        let label_x = area.right().saturating_sub(label.chars().count() as u16);
        surface.print(
            (label_x, area.top()),
            label,
            Style::new().fg(ui::DIM).bg(ui::BG),
        );
        self.draw_life_meter(surface, (area.left(), area.top() + 1), area.width());

        if area.height() < 4 {
            return;
        }
        let legend_area = Rect::new(area.left(), area.top() + 3, area.width(), area.height() - 3);
        self.draw_legend(surface, legend_area);
    }

    /// The `V`/`v` life meter: one glyph per whole heart, a dim `v` for a
    /// fought-down remainder, matching the reference's row of hearts.
    fn draw_life_meter(&self, surface: &mut Surface<'_>, at: (u16, u16), width: u16) {
        let hearts = (self.player_max_hp / 2.0).round() as u16;
        let hearts = hearts.min(width).max(1);
        let per_heart = self.player_max_hp / f32::from(hearts);
        let (x0, y) = at;

        for i in 0..hearts {
            let remaining = f32::from(i)
                .mul_add(-per_heart, self.player_hp)
                .clamp(0.0, per_heart);
            let full = remaining >= per_heart * 0.5;
            let (glyph, color) = if remaining <= 0.0 {
                ('v', scale(rgb(214, 64, 64), 0.35))
            } else if full {
                ('V', rgb(224, 64, 64))
            } else {
                ('v', rgb(224, 64, 64))
            };
            surface.put((x0 + i, y), glyph, Style::new().fg(color).bg(ui::BG));
        }
    }

    /// Draws the legend from [`visible_legend`](Self::visible_legend).
    fn draw_legend(&self, surface: &mut Surface<'_>, area: Rect) {
        surface.print(
            (area.left(), area.top()),
            "LEGEND",
            Style::new().fg(ui::DIM).bg(ui::BG),
        );
        let rows = usize::from(area.height().saturating_sub(1));
        for (i, (glyph, color, name)) in
            self.visible_legend(area).into_iter().take(rows).enumerate()
        {
            let y = area.top() + 1 + i as u16;
            surface.put((area.left(), y), glyph, Style::new().fg(color).bg(ui::BG));
            surface.print(
                (area.left() + 2, y),
                retroglyph_widgets::truncate(name, area.width_usize().saturating_sub(2)),
                Style::new().fg(ui::FG).bg(ui::BG),
            );
        }
    }

    fn status(&self) -> String {
        let (x, y) = self.player_pos();
        format!("({x}, {y})  {} entities alive", self.entities.len())
    }
}

impl Demo for OverworldQuest {
    const NAME: &'static str = "22_overworld_quest";
    const TITLE: &'static str = "22 Overworld Quest";
    const BLURB: &'static str = "A Zelda-style ASCII overworld with a live entity legend.";
    const GRID: (u16, u16) = (150, 44);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move"),
            ("Space", "attack"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        self.next_sim -= frame.delta.as_secs_f32();
        if self.next_sim <= 0.0 {
            self.next_sim += SIM_PERIOD;
            self.simulate();
        }

        let (px, py) = self.player_pos();
        self.camera_x = px;
        self.camera_y = py;

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(rgb(4, 4, 6)));

        // Reserve panels first, at thresholds the reference's own layout does
        // not exceed, then hand whatever remains to the map. A narrow window
        // drops the legend before it drops the map itself: the map is the one
        // thing every size of this demo must still show.
        let show_side = content.width() >= 110;
        let show_inventory = content.width() >= 85;

        let (rest, side) = if show_side {
            panel::split_right(content, 26)
        } else {
            (content, Rect::new(content.right(), content.top(), 0, 0))
        };
        self.draw_map(&mut surface, rest);
        if show_side {
            self.draw_side_panel(&mut surface, side);
        } else if rest.width() > 0 {
            // The life meter still has to be somewhere: it is the one
            // reading from the side panel this demo does not let a narrow
            // window drop, so below the side-panel threshold it is drawn
            // directly over the top-right corner of the map instead.
            let meter_w = rest.width().min(12);
            self.draw_life_meter(&mut surface, (rest.right() - meter_w, rest.top()), meter_w);
        }
        if show_inventory {
            let inv_area = Rect::new(rest.left() + 1, rest.top(), 11, 6.min(rest.height()));
            self.draw_inventory(&mut surface, inv_area);
        }

        // The combat log is the bottom-most row of the map area, matching the
        // reference's single message line under the field.
        if rest.height() > 0 {
            let log_row = Rect::new(rest.left(), rest.bottom() - 1, rest.width(), 1);
            let mut clipped = surface.clip(log_row);
            self.log.draw(&mut clipped, log_row, rgb(4, 4, 6));
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(OverworldQuest);
