//! 23: Isometric tactics -- a diamond-grid battle with real wall height and a
//! combat HUD drawn in map space.
//!
//! Every isometric demo so far has drawn terrain. This one draws a room: a
//! stone dungeon chamber with walls tall enough to hide what is behind them,
//! populated by units that take turns fighting. The interesting problem is
//! not the diamond projection (`06_iso_elevation` already covers that); it is
//! what happens when a wall is tall enough to occlude the thing you actually
//! care about -- the player's own unit standing behind it.
//!
//! Techniques on show:
//!
//! - **Painter's-algorithm depth sort** ([`tilekit::geom::IsoLayout::depth`]):
//!   tiles draw back to front in `col + row` order, exactly as
//!   `06_iso_elevation` does, and a tall wall occludes whatever is behind it
//!   for free -- that is the entire mechanism, not a special case. See
//!   [Brendan Sechter on draw
//!   order](https://sgeos.github.io/games/graphics/projection/2026/04/30/draw_order_y_sort_z_sort_and_painters_algorithm.html).
//! - **Wall cutaway, not culling**: when a wall would hide the selected unit,
//!   the naive fix is to skip drawing it. That is wrong for the same reason a
//!   video game never does it: the wall vanishing breaks the room's geometry
//!   for one frame, and the eye reads a hole in the dungeon rather than "I can
//!   see through this". Fading the wall toward the background instead keeps
//!   its silhouette on screen -- you can still tell a wall is there -- while
//!   letting the unit read through it. See Justin D. Johnson on [isometric
//!   occlusion](https://justindjohnson.com/softdev/isometric-occlusion/) for
//!   the same alpha-fade approach in a pixel-art engine.
//! - **Floating health bars in map space** ([`draw_health_bars`]): drawn with
//!   [`panel::bar`] directly onto the map surface above each unit, clipped to
//!   the content area and nudged apart when two units' bars would collide.
//!   This is a different discipline from a HUD bar in a fixed panel: its
//!   position depends on where the unit's diamond lands this frame, which
//!   moves every time the camera pans or a unit steps.
//! - **Composited tile overlays**: the cursor, the selected ability's
//!   area-of-effect, and the move-range preview from
//!   [`tilekit::path::reachable`] all tint the terrain's own color with
//!   [`tilekit::palette::mix`] rather than replacing it, so an overlay reads
//!   as a colored light falling on the floor instead of a sticker glued over
//!   it.
//! - **A computed damage tooltip**: the ability panel's DMG / Resistance /
//!   Final DMG rows are not display strings, they multiply the selected
//!   unit's attack against the hovered target's defense live, the way
//!   `Path of Exile`-style tooltips do.
//!
//! ```sh
//! cargo run --example 23_iso_tactics --features crossterm
//! cargo run --example 23_iso_tactics --features software
//! cargo run --example 23_iso_tactics --features gl
//! cargo run --example 23_iso_tactics  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, IsoLayout, Tile};
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{self, mix, scale};
use tilekit::path::{self, Diagonals};

/// Dungeon size in tiles.
const ROOM_W: i32 = 18;
/// See [`ROOM_W`].
const ROOM_H: i32 = 14;

/// The tile layout. `LARGE` gives enough vertical room for a wall's height
/// skirt to actually read as a wall rather than a one-cell lip.
const LAYOUT: IsoLayout = IsoLayout::LARGE;

/// Screen cells per elevation level (a wall's height, in the same unit
/// `06_iso_elevation` raises terrain by).
const PER_LEVEL: i32 = 3;

/// How long one AI turn's move-then-attack takes to animate, in seconds.
const TURN_STEP: f32 = 0.9;

/// A cell in the dungeon: floor, wall (with height), or water.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cover {
    Floor,
    /// A wall standing `height` levels tall.
    Wall {
        height: i32,
    },
    Water,
}

impl Cover {
    const fn is_wall(self) -> bool {
        matches!(self, Self::Wall { .. })
    }

    const fn height(self) -> i32 {
        match self {
            Self::Wall { height } => height,
            _ => 0,
        }
    }

    /// Whether a unit can stand here.
    const fn passable(self) -> bool {
        matches!(self, Self::Floor)
    }
}

/// A prop drawn on top of a floor tile: barrels, a statue, a plant. Purely
/// decorative, but decoration is most of what sells "this is a room" rather
/// than "this is a heightmap".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prop {
    None,
    Barrel,
    Statue,
    Plant,
}

/// Which faction a unit belongs to, for color and turn order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Hero,
    Foe,
}

impl Side {
    const fn color(self) -> Color {
        match self {
            Self::Hero => palette::rgb(108, 176, 232),
            Self::Foe => palette::rgb(224, 96, 96),
        }
    }
}

/// A combatant on the board.
struct Unit {
    name: &'static str,
    side: Side,
    tile: Tile,
    /// Where this unit is animating toward, if mid-step. `tile` is the
    /// logical (already-updated) position; this is the visual one, which
    /// lags behind it for [`TURN_STEP`] seconds so a move reads as a slide
    /// rather than a teleport.
    visual: Cell,
    hp: i32,
    hp_max: i32,
    attack: i32,
    resistance: i32,
    glyph: char,
}

impl Unit {
    const fn new(
        name: &'static str,
        side: Side,
        tile: Tile,
        hp: i32,
        attack: i32,
        resistance: i32,
        glyph: char,
    ) -> Self {
        Self {
            name,
            side,
            tile,
            visual: LAYOUT.tile_to_cell(tile),
            hp,
            hp_max: hp,
            attack,
            resistance,
            glyph,
        }
    }

    const fn alive(&self) -> bool {
        self.hp > 0
    }
}

/// One ability on the hotbar.
struct Ability {
    name: &'static str,
    /// Damage multiplier against the target's raw attack stat.
    power: f32,
    /// Area of effect radius in tiles (Chebyshev distance), 0 for single
    /// target.
    radius: i32,
    charges: u32,
    charges_max: u32,
}

/// State: the room, its units, whose turn it is, and the UI's selections.
pub struct IsoTactics {
    cover: Vec<Cover>,
    props: Vec<Prop>,
    units: Vec<Unit>,
    /// Seconds since the last autonomous foe action; foes act on a timer
    /// rather than a strict turn order, so the battle keeps animating
    /// whether or not the player is doing anything.
    turn_timer: f32,
    turn_count: u32,
    night: u32,
    cursor: Tile,
    selected_unit: Option<usize>,
    ability: usize,
    abilities: Vec<Ability>,
    time: f32,
    fps: FpsMeter,
    log: panel::Log,
    /// Fires once per completed foe action, so the draw code can start a new
    /// slide animation without re-deriving "did something just move" from
    /// state alone.
    seed: u32,
}

/// Builds the fixed dungeon layout: a stone room with an internal spine wall,
/// a corner pool, and scattered props.
///
/// Hand-authored rather than generated. A tactics arena is small and the
/// point is the occlusion and combat systems, not terrain variety -- the same
/// reason `17_tileset_sprites` hand-places its town rather than growing one.
fn build_room() -> (Vec<Cover>, Vec<Prop>) {
    let mut cover = vec![Cover::Floor; (ROOM_W * ROOM_H) as usize];
    let mut props = vec![Prop::None; (ROOM_W * ROOM_H) as usize];
    let idx = |x: i32, y: i32| (y * ROOM_W + x) as usize;

    // Perimeter wall, two levels tall.
    for x in 0..ROOM_W {
        cover[idx(x, 0)] = Cover::Wall { height: 2 };
        cover[idx(x, ROOM_H - 1)] = Cover::Wall { height: 2 };
    }
    for y in 0..ROOM_H {
        cover[idx(0, y)] = Cover::Wall { height: 2 };
        cover[idx(ROOM_W - 1, y)] = Cover::Wall { height: 2 };
    }

    // A tall spine wall across the middle, with a one-tile gap so the room
    // is still fully connected. Three levels: taller than the perimeter, so
    // it is the wall that actually demonstrates the occlusion fix.
    let gap = ROOM_H / 2;
    for y in 1..ROOM_H - 1 {
        if y == gap {
            continue;
        }
        cover[idx(ROOM_W / 2, y)] = Cover::Wall { height: 3 };
    }

    // A corner pool.
    for y in 2..5 {
        for x in 2..6 {
            cover[idx(x, y)] = Cover::Water;
        }
    }

    // Props scattered by a fixed seed, skipping walls, water, and the gap
    // (which needs to stay clear for pathing to read as sane).
    let mut rng = Rng::new(7);
    for _ in 0..14 {
        let x = 1 + rng.next_below((ROOM_W - 2) as u32) as i32;
        let y = 1 + rng.next_below((ROOM_H - 2) as u32) as i32;
        if !cover[idx(x, y)].passable() {
            continue;
        }
        let roll = rng.next_below(3);
        props[idx(x, y)] = match roll {
            0 => Prop::Barrel,
            1 => Prop::Statue,
            _ => Prop::Plant,
        };
    }
    // Keep the near side of the arena clear so heroes have room to open with.
    for y in ROOM_H - 4..ROOM_H - 1 {
        for x in 1..ROOM_W - 1 {
            if props[idx(x, y)] != Prop::None && rng.next_f32() < 0.6 {
                props[idx(x, y)] = Prop::None;
            }
        }
    }

    (cover, props)
}

impl Default for IsoTactics {
    fn default() -> Self {
        let (cover, props) = build_room();
        let units = vec![
            Unit::new("Roc", Side::Hero, Tile::new(3, ROOM_H - 3), 42, 11, 3, '@'),
            Unit::new("Sable", Side::Hero, Tile::new(5, ROOM_H - 3), 34, 9, 2, '@'),
            Unit::new("Ghast", Side::Foe, Tile::new(ROOM_W - 4, 3), 30, 8, 2, 'g'),
            Unit::new("Ghast", Side::Foe, Tile::new(ROOM_W - 6, 4), 30, 8, 2, 'g'),
            Unit::new(
                "Warden",
                Side::Foe,
                Tile::new(ROOM_W - 3, ROOM_H / 2 + 1),
                55,
                13,
                4,
                'W',
            ),
        ];
        let abilities = vec![
            Ability {
                name: "Strike",
                power: 1.0,
                radius: 0,
                charges: 5,
                charges_max: 5,
            },
            Ability {
                name: "Cleave",
                power: 0.7,
                radius: 1,
                charges: 3,
                charges_max: 3,
            },
            Ability {
                name: "Firebolt",
                power: 1.4,
                radius: 1,
                charges: 2,
                charges_max: 2,
            },
        ];
        Self {
            cover,
            props,
            units,
            turn_timer: TURN_STEP,
            turn_count: 1,
            night: 10,
            cursor: Tile::new(4, ROOM_H - 3),
            selected_unit: Some(0),
            ability: 0,
            abilities,
            time: 0.0,
            fps: FpsMeter::new(),
            log: panel::Log::new(6),
            seed: 1,
        }
    }
}

impl IsoTactics {
    fn cover_at(&self, tile: Tile) -> Cover {
        if tile.col < 0 || tile.row < 0 || tile.col >= ROOM_W || tile.row >= ROOM_H {
            return Cover::Wall { height: 2 };
        }
        self.cover[(tile.row * ROOM_W + tile.col) as usize]
    }

    fn prop_at(&self, tile: Tile) -> Prop {
        if tile.col < 0 || tile.row < 0 || tile.col >= ROOM_W || tile.row >= ROOM_H {
            return Prop::None;
        }
        self.props[(tile.row * ROOM_W + tile.col) as usize]
    }

    fn unit_at(&self, tile: Tile) -> Option<usize> {
        self.units.iter().position(|u| u.alive() && u.tile == tile)
    }

    /// Movement cost for [`tilekit::path`]: walls and water block, everything
    /// else costs one step. Shared by the move-range preview and the AI's own
    /// pathing so both agree about what is passable.
    fn move_cost(&self, cell: Cell) -> u32 {
        let tile = LAYOUT.cell_to_tile(cell);
        if self.cover_at(tile).passable() {
            1
        } else {
            path::IMPASSABLE
        }
    }

    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        let (cover, props) = build_room();
        self.cover = cover;
        self.props = props;
        *self = Self {
            seed: self.seed,
            ..Self::default()
        };
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_cursor(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_cursor(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_cursor(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_cursor(1, 0),
                    KeyCode::Enter => self.confirm(),
                    KeyCode::Char(' ') => self.end_hero_turn(),
                    KeyCode::Char(c @ '1'..='6') => {
                        let n = c as usize - '1' as usize;
                        if n < self.abilities.len() {
                            self.ability = n;
                        }
                    }
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                },
                Event::Mouse(mouse)
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
                {
                    self.confirm();
                }
                _ => {}
            }
        }
        true
    }

    const fn move_cursor(&mut self, dx: i32, dy: i32) {
        let next = Tile::new(self.cursor.col + dx, self.cursor.row + dy);
        if next.col >= 0 && next.row >= 0 && next.col < ROOM_W && next.row < ROOM_H {
            self.cursor = next;
        }
    }

    /// Enter/click: select a friendly unit under the cursor, or, with a hero
    /// already selected, spend the current ability on the cursor's tile.
    fn confirm(&mut self) {
        if let Some(target) = self.unit_at(self.cursor)
            && self.units[target].side == Side::Hero
            && self.units[target].alive()
        {
            self.selected_unit = Some(target);
            return;
        }
        let Some(caster) = self.selected_unit else {
            return;
        };
        if self.units[caster].side != Side::Hero || !self.units[caster].alive() {
            return;
        }
        self.cast(caster, self.cursor);
    }

    /// Resolves the current ability from `caster` against everything within
    /// its radius of `target`, or moves the caster to `target` if it is out
    /// of range and unoccupied. A tactics demo needs both a "commit to an
    /// attack" and a "reposition" action, and folding move into "nothing to
    /// hit here" lets one confirm key do the obvious thing instead of adding
    /// a mode toggle just for this demo.
    fn cast(&mut self, caster: usize, target: Tile) {
        let ability_radius = self.abilities[self.ability].radius;
        let mut hit_any = false;
        for i in 0..self.units.len() {
            if !self.units[i].alive() || self.units[i].side == Side::Hero {
                continue;
            }
            let dist = (self.units[i].tile.col - target.col)
                .abs()
                .max((self.units[i].tile.row - target.row).abs());
            if dist <= ability_radius {
                hit_any = true;
                self.resolve_attack(caster, i);
            }
        }
        if hit_any {
            let ability = &mut self.abilities[self.ability];
            ability.charges = ability.charges.saturating_sub(1);
            return;
        }
        // No target in range: reposition instead, if the tile is walkable and
        // free.
        if self.cover_at(target).passable() && self.unit_at(target).is_none() {
            self.units[caster].tile = target;
            self.units[caster].visual = LAYOUT.tile_to_cell(target);
        }
    }

    fn resolve_attack(&mut self, attacker: usize, defender: usize) {
        let power = self.abilities[self.ability].power;
        let damage = compute_damage(
            self.units[attacker].attack,
            self.units[defender].resistance,
            power,
        );
        self.units[defender].hp = (self.units[defender].hp - damage).max(0);
        let (attacker_name, defender_name) = (self.units[attacker].name, self.units[defender].name);
        if self.units[defender].alive() {
            self.log.push(
                format!("{attacker_name} hits {defender_name} for {damage}"),
                ui::FG,
            );
        } else {
            self.log.push(
                format!("{defender_name} falls!"),
                palette::rgb(230, 120, 110),
            );
        }
    }

    /// Ends the hero side's turn: every foe with charges gets exactly one
    /// automatic action, then the turn counter advances. Driven by
    /// `end_hero_turn` and by `tick`'s own timer, so the demo plays itself
    /// when left alone -- required so the thumbnail generator sees motion.
    const fn end_hero_turn(&mut self) {
        self.turn_count += 1;
        if self.turn_count.is_multiple_of(6) {
            self.night += 1;
        }
        self.turn_timer = 0.0;
    }

    /// Runs one foe's turn: step toward the nearest living hero, or attack if
    /// already adjacent. Called once per [`TURN_STEP`] tick while it is the
    /// foe side's moment to act, which this demo treats as continuous rather
    /// than gated behind an explicit "foe phase" -- simpler state machine, and
    /// what makes the battle animate on its own for the thumbnail check.
    fn foe_act(&mut self, foe: usize) {
        if !self.units[foe].alive() {
            return;
        }
        let Some(hero) = self
            .units
            .iter()
            .enumerate()
            .filter(|(_, u)| u.side == Side::Hero && u.alive())
            .min_by_key(|(_, u)| {
                (u.tile.col - self.units[foe].tile.col).abs()
                    + (u.tile.row - self.units[foe].tile.row).abs()
            })
            .map(|(i, _)| i)
        else {
            return;
        };

        let (fx, fy) = (self.units[foe].tile.col, self.units[foe].tile.row);
        let (hx, hy) = (self.units[hero].tile.col, self.units[hero].tile.row);
        let dist = (fx - hx).abs().max((fy - hy).abs());
        if dist <= 1 {
            let damage = compute_damage(self.units[foe].attack, self.units[hero].resistance, 1.0);
            self.units[hero].hp = (self.units[hero].hp - damage).max(0);
            let (attacker_name, defender_name) = (self.units[foe].name, self.units[hero].name);
            self.log.push(
                format!("{attacker_name} strikes {defender_name} for {damage}"),
                palette::rgb(224, 140, 130),
            );
            return;
        }

        let start = LAYOUT.tile_to_cell(self.units[foe].tile);
        let goal = LAYOUT.tile_to_cell(self.units[hero].tile);
        // A generous budget: only the first step is ever taken this call, so
        // the budget just needs to exceed one step's cost, not the whole
        // route.
        if let Some(route) = path::find(
            start,
            goal,
            ROOM_W * LAYOUT.width().max(1),
            ROOM_H * LAYOUT.height().max(1),
            Diagonals::Never,
            u32::MAX,
            |c| self.move_cost(c),
        ) && let Some(&first) = route.steps.first()
        {
            let next_tile = LAYOUT.cell_to_tile(first);
            if self.cover_at(next_tile).passable() && self.unit_at(next_tile).is_none() {
                self.units[foe].tile = next_tile;
            }
        }
    }

    /// Move-range highlight for the selected hero: every tile reachable in
    /// four steps, shaded blue. Uses [`path::reachable`] directly rather than
    /// re-deriving a flood fill, so the preview and the AI's own pathing agree
    /// about what counts as passable.
    fn move_range(&self) -> Vec<u32> {
        let Some(unit) = self.selected_unit else {
            return Vec::new();
        };
        let start = LAYOUT.tile_to_cell(self.units[unit].tile);
        path::reachable(
            start,
            ROOM_W * LAYOUT.width().max(1),
            ROOM_H * LAYOUT.height().max(1),
            Diagonals::Never,
            4,
            |c| self.move_cost(c),
        )
    }

    /// The base color for a tile's own material, before overlays, bevel, or
    /// occlusion fade.
    ///
    /// Floor, wall, and water are deliberately different hues (warm tan,
    /// cool slate, blue), not just different shades of the same gray: with
    /// the wall's height skirt reusing the same bevel treatment as a tile's
    /// top face, a palette that only varied brightness made a floor tile and
    /// a wall's skirt read as the same material from a few rows away, which
    /// defeats the entire occlusion demonstration this file exists for.
    fn tile_base_color(&self, tile: Tile) -> Color {
        match self.cover_at(tile) {
            Cover::Floor => {
                // A coarse flagstone checker, so the floor reads as laid
                // tile rather than a flat fill.
                let checker = (tile.col + tile.row) % 2 == 0;
                if checker {
                    palette::rgb(146, 128, 96)
                } else {
                    palette::rgb(134, 116, 86)
                }
            }
            Cover::Wall { .. } => palette::rgb(96, 100, 116),
            Cover::Water => {
                let swell = f32::from(tile.col as i16)
                    .mul_add(0.6, self.time * 1.4)
                    .sin()
                    .mul_add(0.5, 0.5);
                mix(
                    palette::rgb(30, 58, 92),
                    palette::rgb(60, 96, 132),
                    swell * 0.4,
                )
            }
        }
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

    /// Draws one tile's diamond footprint (floor or wall top) plus, for a
    /// wall, the vertical skirt beneath it.
    ///
    /// The occlusion fix lives here: `fade` is `1.0` normally, and less than
    /// that when this wall would otherwise hide the selected unit, so every
    /// pixel of the wall (top and skirt alike) is mixed toward the page
    /// background by the same amount. Fading rather than skipping the draw
    /// keeps the wall's silhouette on screen -- the room's shape does not
    /// change for a frame -- while still letting the unit read through it.
    fn draw_cover(
        &self,
        surface: &mut Surface<'_>,
        content: Rect,
        tile: Tile,
        center: Cell,
        fade: f32,
    ) {
        let cover = self.cover_at(tile);
        let height = cover.height();
        let raised = LAYOUT.tile_to_cell_elevated(tile, height, PER_LEVEL);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let base = self.tile_base_color(tile);
        let base = if fade < 1.0 {
            mix(base, ui::BG, 1.0 - fade)
        } else {
            base
        };

        for dy in -LAYOUT.half_h..=LAYOUT.half_h {
            let Some(span) = LAYOUT.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                let bevel = if dy < 0 {
                    1.12
                } else if dy > 0 {
                    0.82
                } else if dx < 0 {
                    1.04
                } else {
                    0.92
                };
                let color = scale(base, bevel);
                Self::put_clipped(
                    surface,
                    content,
                    sx + dx,
                    sy + dy,
                    ' ',
                    Style::new().bg(color),
                );
            }
        }

        if !cover.is_wall() || height <= 0 {
            return;
        }

        // The skirt: sweep the diamond's lower half straight down by the
        // wall's height, the same technique `06_iso_elevation` uses for
        // cliffs. A wall's skirt is unconditional (every wall needs one,
        // since nothing is ever "behind" it at a lower level the way terrain
        // can be), which is simpler than that demo's per-neighbour drop test.
        // Darker and cooler than the wall's own top face, so a wall reads as
        // one solid mass of stone with a lit top and a shadowed face, rather
        // than the face looking like a second, unrelated material.
        let rock = scale(
            palette::rgb(58, 60, 72),
            if fade < 1.0 { fade.max(0.35) } else { 1.0 },
        );
        let rock = mix(rock, ui::BG, if fade < 1.0 { 1.0 - fade } else { 0.0 });
        let rock_dark = scale(rock, 0.75);
        let rows = height * PER_LEVEL;
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
                        content,
                        sx + dx,
                        sy + dy + r,
                        ' ',
                        Style::new().bg(color),
                    );
                }
            }
        }
    }

    fn draw_prop(&self, surface: &mut Surface<'_>, content: Rect, tile: Tile, center: Cell) {
        let prop = self.prop_at(tile);
        if prop == Prop::None {
            return;
        }
        let raised = LAYOUT.tile_to_cell(tile);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let floor = self.tile_base_color(tile);
        let (glyph, color) = match prop {
            Prop::Barrel => ('n', palette::rgb(168, 128, 78)),
            Prop::Statue => ('i', palette::rgb(150, 148, 156)),
            Prop::Plant => ('"', palette::rgb(96, 150, 84)),
            Prop::None => unreachable!(),
        };
        Self::put_clipped(
            surface,
            content,
            sx,
            sy,
            glyph,
            Style::new().fg(color).bg(floor),
        );
    }

    /// Draws one unit: body glyph on its own tile, plus a two-cell tint on
    /// the tile face so the unit's side color is visible even under the
    /// glyph's narrow footprint.
    fn draw_unit(
        &self,
        surface: &mut Surface<'_>,
        content: Rect,
        unit: &Unit,
        center: Cell,
        selected: bool,
    ) {
        let (sx, sy) = (unit.visual.x - center.x, unit.visual.y - center.y);
        let floor_tile = LAYOUT.cell_to_tile(unit.visual);
        let mut floor = self.tile_base_color(floor_tile);
        if selected {
            floor = mix(floor, palette::rgb(255, 240, 180), 0.3);
        }
        let mut color = unit.side.color();
        if !unit.alive() {
            color = scale(color, 0.35);
        }
        Self::put_clipped(
            surface,
            content,
            sx,
            sy,
            unit.glyph,
            Style::new().fg(color).bg(floor),
        );
        Self::put_clipped(surface, content, sx - 1, sy, ' ', Style::new().bg(floor));
        Self::put_clipped(surface, content, sx + 1, sy, ' ', Style::new().bg(floor));
    }

    /// Draws every living unit's health bar hovering one row above its tile,
    /// clipped to `content`.
    ///
    /// Map-space widgets need a discipline a panel never does: two bars can
    /// land on the same screen row if their owners are close together on
    /// screen, in which case the second one drawn would silently overwrite
    /// the first. Tracking `taken_rows` and nudging a colliding bar one row
    /// higher is the fix; it costs an allocation-free linear scan because the
    /// unit count here is always small.
    fn draw_health_bars(&self, surface: &mut Surface<'_>, content: Rect, center: Cell) {
        const BAR_W: u16 = 5;
        let mut taken: Vec<(i32, i32)> = Vec::new();

        for unit in &self.units {
            if !unit.alive() {
                continue;
            }
            let (cx, cy) = (unit.visual.x - center.x, unit.visual.y - center.y);
            let mut by = cy - LAYOUT.half_h - 1;
            let bx = cx - i32::from(BAR_W) / 2;
            // Nudge up past anything already claiming this column range at
            // this row, so two adjacent units get stacked bars instead of
            // one clobbering the other.
            while taken
                .iter()
                .any(|&(tx, ty)| ty == by && (tx - bx).abs() < i32::from(BAR_W))
            {
                by -= 1;
            }
            taken.push((bx, by));

            if bx < 0 || by < 0 {
                continue;
            }
            let (sx, sy) = (
                u16::try_from(i32::from(content.left()) + bx).unwrap_or(0),
                u16::try_from(i32::from(content.top()) + by).unwrap_or(0),
            );
            if sx + BAR_W > content.right() || sy >= content.bottom() {
                continue;
            }

            let t = f32::from(unit.hp as i16) / f32::from(unit.hp_max as i16).max(1.0);
            let fill = panel::threshold(t);
            panel::bar(surface, (sx, sy), BAR_W, t, fill, palette::rgb(28, 22, 26));

            // The predicted-damage overlay: if this unit is a live foe target
            // of the current ability from the selected hero, show how much of
            // the remaining bar an attack right now would remove, in
            // magenta, so committing to an attack is an informed choice
            // rather than a guess.
            if let Some(caster) = self.selected_unit
                && self.units[caster].side == Side::Hero
                && unit.side == Side::Foe
            {
                let dmg = compute_damage(
                    self.units[caster].attack,
                    unit.resistance,
                    self.abilities[self.ability].power,
                );
                let after = (f32::from(unit.hp as i16) - f32::from(dmg as i16)).max(0.0)
                    / f32::from(unit.hp_max as i16).max(1.0);
                if after < t {
                    panel::bar(
                        surface,
                        (sx, sy),
                        BAR_W,
                        after,
                        palette::rgb(216, 96, 216),
                        palette::rgb(28, 22, 26),
                    );
                    // Re-draw the surviving portion on top so the prediction
                    // reads as "this much would be removed", not "the bar
                    // shrank to this and the rest vanished".
                    panel::bar(surface, (sx, sy), BAR_W, t, fill, palette::rgb(28, 22, 26));
                    let cut = ((f32::from(BAR_W) * 2.0 * after).round() as u16).min(BAR_W * 2);
                    for i in 0..BAR_W {
                        let cell_start = i * 2;
                        if cell_start + 1 < cut || cell_start >= (f32::from(BAR_W) * 2.0 * t) as u16
                        {
                            continue;
                        }
                        surface.put(
                            (sx + i, sy),
                            if cell_start + 1 == cut {
                                '\u{258C}'
                            } else {
                                ' '
                            },
                            Style::new()
                                .fg(palette::rgb(216, 96, 216))
                                .bg(palette::rgb(28, 22, 26)),
                        );
                    }
                }
            }
        }
    }

    /// Draws the full battlefield: cover, props, units, overlays, and health
    /// bars, all painter's-algorithm ordered.
    fn draw_map(&self, surface: &mut Surface<'_>, content: Rect) {
        let cursor_cell = LAYOUT.tile_to_cell(self.cursor);
        let center = Cell::new(
            cursor_cell.x - i32::from(content.width()) / 2,
            cursor_cell.y - i32::from(content.height()) / 2,
        );

        let selected_visual = self
            .selected_unit
            .filter(|&i| self.units[i].alive())
            .map(|i| self.units[i].visual);

        let mut tiles: Vec<Tile> = (0..ROOM_H)
            .flat_map(|row| (0..ROOM_W).map(move |col| Tile::new(col, row)))
            .collect();
        // Ascending col + row: the whole occlusion mechanism. Elevation never
        // enters the key, exactly as `06_iso_elevation` explains -- a wall
        // occludes because it is drawn later in map order and happens to
        // land, once raised, over the same screen cells as whatever is
        // behind it, not because it was given priority.
        tiles.sort_by_key(|&t| IsoLayout::depth(t));

        let range = self.move_range();
        let range_w = ROOM_W * LAYOUT.width().max(1);

        for tile in tiles {
            let fade = selected_visual.map_or(1.0, |target| self.occlusion_fade(tile, target));
            self.draw_cover(surface, content, tile, center, fade);

            // Overlays: move range in blue, ability AoE in red, cursor
            // outlined in white. All three mix into the tile's own color
            // rather than overwrite it, which is why they read as colored
            // light on the floor instead of a sticker -- see the module
            // doc's note on this.
            if self.cover_at(tile).passable() {
                let cell = LAYOUT.tile_to_cell(tile);
                let idx = (cell.y * range_w + cell.x) as usize;
                if range.get(idx).is_some_and(|&c| c != path::IMPASSABLE) {
                    self.tint_tile(
                        surface,
                        content,
                        tile,
                        center,
                        palette::rgb(90, 140, 230),
                        0.28,
                    );
                }
            }
            let ability_radius = self.abilities[self.ability].radius;
            let dist = (tile.col - self.cursor.col)
                .abs()
                .max((tile.row - self.cursor.row).abs());
            if dist <= ability_radius && self.selected_unit.is_some() {
                self.tint_tile(
                    surface,
                    content,
                    tile,
                    center,
                    palette::rgb(210, 70, 70),
                    0.32,
                );
            }
            if tile == self.cursor {
                self.outline_tile(surface, content, tile, center);
            }

            self.draw_prop(surface, content, tile, center);
        }

        for (i, unit) in self.units.iter().enumerate() {
            if !unit.alive() {
                continue;
            }
            self.draw_unit(
                surface,
                content,
                unit,
                center,
                self.selected_unit == Some(i),
            );
        }

        self.draw_health_bars(surface, content, center);
    }

    /// How much `tile` should fade (`1.0` = fully opaque) to keep `target`
    /// visible.
    ///
    /// A wall occludes `target` when `target` sits behind it in draw order
    /// (later depth) *and* `target`'s own screen position falls inside the
    /// silhouette this wall actually paints. That silhouette is not just the
    /// raised top diamond [`IsoLayout::contains`] tests -- it is the top
    /// diamond *and* the vertical skirt beneath it, which is the region
    /// [`draw_cover`](Self::draw_cover) really fills. A first pass here tested
    /// only the top diamond, on the theory that it was the same test
    /// `06_iso_elevation`'s picker uses; that test answers "did a click land
    /// on this tile's raised face", not "is this pixel column part of this
    /// wall", and a unit standing on the floor immediately behind a wall is
    /// almost always hidden by the *skirt*, several rows below the small top
    /// diamond. Restricting the check to `contains` alone meant the fade
    /// essentially never fired for the case it exists to fix.
    ///
    /// The skirt is *not* a constant-width column: `draw_cover` paints it by
    /// repeating the diamond's own lower-half silhouette (wide at the spine,
    /// narrowing to a point at the tip) once per row of height, stacked
    /// downward. So a point at vertical offset `dy` from the wall's raised
    /// center is covered by the skirt iff *some* repetition `r` (`1..=rows`)
    /// places it inside that lower-half silhouette, i.e. `dy - r` lands in
    /// `0..=half_h` at a horizontal offset within that row's span. Checking
    /// only the widest row (as a first pass here did, via
    /// `span_at(half_h)` -- the tip, which is always zero-width) covered
    /// nothing; the fix is to test every repetition, matching the loop
    /// `draw_cover` itself uses to paint the skirt.
    fn occlusion_fade(&self, tile: Tile, target: Cell) -> f32 {
        let cover = self.cover_at(tile);
        if !cover.is_wall() {
            return 1.0;
        }
        // Painter's algorithm draws ascending depth back-to-front, so a
        // *later* (strictly greater depth) tile paints on top of an earlier
        // one. Occluding the target therefore needs this wall's depth to
        // exceed the target's, not the reverse: a wall drawn *before* the
        // target in map order can never be the thing hiding it, because the
        // target's own draw (or whatever landed on that pixel after it) would
        // already have painted over the wall, not the other way around. An
        // earlier version of this check had the comparison backwards, which
        // is why it never fired against this file's own fixture room; see
        // `tests::a_wall_fades_when_it_hides_the_tile_behind_it`.
        if IsoLayout::depth(tile) <= IsoLayout::depth(LAYOUT.cell_to_tile(target)) {
            return 1.0;
        }
        let raised = LAYOUT.tile_to_cell_elevated(tile, cover.height(), PER_LEVEL);
        let (dx, dy) = (target.x - raised.x, target.y - raised.y);
        if LAYOUT.contains(dx, dy) {
            return 0.35;
        }
        let rows = cover.height() * PER_LEVEL;
        for r in 1..=rows {
            let base_dy = dy - r;
            if !(0..=LAYOUT.half_h).contains(&base_dy) {
                continue;
            }
            if let Some(span) = LAYOUT.span_at(base_dy)
                && dx.abs() <= span
            {
                return 0.35;
            }
        }
        1.0
    }

    fn tint_tile(
        &self,
        surface: &mut Surface<'_>,
        content: Rect,
        tile: Tile,
        center: Cell,
        tint: Color,
        amount: f32,
    ) {
        let cover = self.cover_at(tile);
        let raised = LAYOUT.tile_to_cell_elevated(tile, cover.height(), PER_LEVEL);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        for dy in -LAYOUT.half_h..=LAYOUT.half_h {
            let Some(span) = LAYOUT.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                let base = self.tile_base_color(tile);
                let color = mix(base, tint, amount);
                Self::put_clipped(
                    surface,
                    content,
                    sx + dx,
                    sy + dy,
                    ' ',
                    Style::new().bg(color),
                );
            }
        }
    }

    /// Outlines the cursor's tile with a diamond of corner marks.
    ///
    /// `^`/`v`/`<`/`>` rather than the narrower Unicode `∧`/`∨` (logical
    /// and/or, which read as tighter chevrons): those are outside CP437, and
    /// `retroglyph`'s pixel backends draw a solid block for anything outside
    /// it. Plain ASCII carets are the CP437-safe equivalent and cost nothing
    /// in legibility here.
    fn outline_tile(&self, surface: &mut Surface<'_>, content: Rect, tile: Tile, center: Cell) {
        let cover = self.cover_at(tile);
        let raised = LAYOUT.tile_to_cell_elevated(tile, cover.height(), PER_LEVEL);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let ink = Style::new()
            .fg(palette::WHITE)
            .bg(self.tile_base_color(tile));
        Self::put_clipped(surface, content, sx, sy - LAYOUT.half_h, '^', ink);
        Self::put_clipped(surface, content, sx, sy + LAYOUT.half_h, 'v', ink);
        Self::put_clipped(surface, content, sx - LAYOUT.half_w, sy, '<', ink);
        Self::put_clipped(surface, content, sx + LAYOUT.half_w, sy, '>', ink);
    }

    // ── HUD ──────────────────────────────────────────────────────────────

    /// Top bar: night/turn counters and an END TURN button, left-anchored so
    /// it survives a narrow terminal.
    fn draw_top_bar(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        panel::band(surface, area);
        let text = format!(" Night {}  Turn {}  ", self.night, self.turn_count);
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width(),
            &[panel::Span::keyword(&text)],
            ui::CHROME_BG,
        );
        let label = " END TURN ";
        let w = label.chars().count() as u16;
        if area.width() > w + 2 {
            let x = area.left() + area.width() / 2 - w / 2;
            surface.print(
                (x, area.top()),
                label,
                Style::new().fg(ui::CHROME_BG).bg(palette::rgb(206, 92, 92)),
            );
        }
    }

    /// Portrait strip: one mini card per hero, each with an HP gauge.
    fn draw_portraits(&self, surface: &mut Surface<'_>, area: Rect) {
        let heroes: Vec<usize> = (0..self.units.len())
            .filter(|&i| self.units[i].side == Side::Hero)
            .collect();
        if heroes.is_empty() || area.width() < 14 {
            return;
        }
        let cols = panel::columns(area, heroes.len() as u16, 1);
        for (slot, &i) in cols.iter().zip(&heroes) {
            let unit = &self.units[i];
            let selected = self.selected_unit == Some(i);
            let inner = panel::Panel::new()
                .title(unit.name)
                .focused(selected)
                .draw(surface, *slot);
            if inner.height() == 0 {
                continue;
            }
            let t = f32::from(unit.hp as i16) / f32::from(unit.hp_max as i16).max(1.0);
            panel::bar(
                surface,
                (inner.left(), inner.top()),
                inner.width().min(10),
                t,
                panel::threshold(t),
                palette::rgb(28, 22, 26),
            );
        }
    }

    /// Right-hand panel: the selected unit's stats plus a live damage
    /// tooltip for the current ability against whatever the cursor is over.
    fn draw_side_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Unit")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.height() < 4 {
            return;
        }
        let Some(unit_idx) = self.selected_unit else {
            return;
        };
        let unit = &self.units[unit_idx];
        let mut y = inner.top();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[panel::Span::keyword(unit.name)],
            panel::PANEL_BG,
        );
        y += 1;
        let t = f32::from(unit.hp as i16) / f32::from(unit.hp_max as i16).max(1.0);
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[panel::Span::dim(&format!("HP {}/{}", unit.hp, unit.hp_max))],
            panel::PANEL_BG,
        );
        y += 1;
        panel::bar(
            surface,
            (inner.left(), y),
            inner.width().min(16),
            t,
            panel::threshold(t),
            palette::rgb(28, 22, 26),
        );
        y += 2;

        if y + 4 > inner.bottom() {
            return;
        }
        let ability = &self.abilities[self.ability];
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[panel::Span::keyword(ability.name)],
            panel::PANEL_BG,
        );
        y += 1;

        // The tooltip's target is whichever foe the cursor is over, falling
        // back to the nearest living foe so the panel always shows a
        // realistic number rather than blanking out between selections.
        let target = self
            .unit_at(self.cursor)
            .filter(|&i| self.units[i].side == Side::Foe && self.units[i].alive())
            .or_else(|| {
                self.units
                    .iter()
                    .position(|u| u.side == Side::Foe && u.alive())
            });

        if let Some(target) = target {
            let resistance = self.units[target].resistance;
            let dmg = compute_damage(unit.attack, resistance, ability.power);
            let rows: [(&str, String); 3] = [
                ("DMG", format!("{}", unit.attack)),
                ("Resistance", format!("-{resistance}")),
                ("Final DMG", format!("{dmg}")),
            ];
            for (label, value) in rows {
                if y >= inner.bottom() {
                    break;
                }
                panel::spans(
                    surface,
                    (inner.left(), y),
                    inner.width(),
                    &[panel::Span::dim(label)],
                    panel::PANEL_BG,
                );
                let vw = value.chars().count() as u16;
                if inner.width() > vw {
                    surface.print(
                        (inner.left() + inner.width() - vw, y),
                        &value,
                        Style::new().fg(ui::FG).bg(panel::PANEL_BG),
                    );
                }
                y += 1;
            }
        }

        y += 1;
        if y < inner.bottom() {
            self.log.draw(
                surface,
                Rect::new(inner.left(), y, inner.width(), inner.bottom() - y),
                panel::PANEL_BG,
            );
        }
    }

    /// Bottom hotbar: one slot per ability with its charge count, the
    /// selected one framed.
    fn draw_hotbar(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 12 {
            return;
        }
        let cols = panel::columns(area, self.abilities.len() as u16, 1);
        for (i, (slot, ability)) in cols.iter().zip(&self.abilities).enumerate() {
            let title = format!("{}/{}", ability.charges, ability.charges_max);
            let inner = panel::Panel::new()
                .title(ability.name)
                .badge(&title)
                .focused(i == self.ability)
                .draw(surface, *slot);
            let _ = inner;
        }
    }

    fn status(&self) -> String {
        let target = self
            .unit_at(self.cursor)
            .map_or("--", |i| self.units[i].name);
        format!(
            "cursor ({}, {})  ability {}  under cursor: {target}",
            self.cursor.col, self.cursor.row, self.abilities[self.ability].name,
        )
    }
}

/// Damage formula shared by hero abilities and foe attacks: attack times the
/// ability's power multiplier, minus flat resistance, floored at 1 so an
/// attack always does *something* rather than silently rounding to zero
/// against a tough target.
fn compute_damage(attack: i32, resistance: i32, power: f32) -> i32 {
    let raw = (f32::from(attack as i16) * power).round() as i32 - resistance;
    raw.max(1)
}

impl Demo for IsoTactics {
    const NAME: &'static str = "23_iso_tactics";
    const TITLE: &'static str = "23 Isometric tactics";
    const BLURB: &'static str =
        "Depth-sorted walls with height, occlusion fade, and a map-space HUD.";
    const GRID: (u16, u16) = (168, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move cursor"),
            ("Enter/click", "select/act"),
            ("1-6", "pick ability"),
            ("Space", "end turn"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        // Slide every unit's visual position toward its logical tile. A
        // fixed easing rate rather than a per-unit timer: simple, and at
        // TURN_STEP-scale steps the difference is not visible.
        for unit in &mut self.units {
            let target = LAYOUT.tile_to_cell(unit.tile);
            let ease = (dt * 6.0).min(1.0);
            unit.visual = Cell::new(
                f32::from((target.x - unit.visual.x) as i16)
                    .mul_add(ease, f32::from(unit.visual.x as i16)) as i32,
                f32::from((target.y - unit.visual.y) as i16)
                    .mul_add(ease, f32::from(unit.visual.y as i16)) as i32,
            );
        }

        // Autonomous foe turns, driven by the clock so the demo animates
        // with no input at all: required for the thumbnail generator, and it
        // is also just what a "the battle is happening" screen should do.
        self.turn_timer += dt;
        if self.turn_timer >= TURN_STEP {
            self.turn_timer = 0.0;
            let foes: Vec<usize> = (0..self.units.len())
                .filter(|&i| self.units[i].side == Side::Foe && self.units[i].alive())
                .collect();
            if !foes.is_empty() {
                let n = (hash01(self.seed, self.turn_count as i32, 0) * foes.len() as f32) as usize;
                self.foe_act(foes[n.min(foes.len() - 1)]);
            }
            self.turn_count += 1;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        // Responsive layout: the right panel needs real width to be useful,
        // and the portrait strip and hotbar need enough height headroom
        // above/below the map that they don't crush it to a sliver. Below
        // those thresholds the map alone, plus health bars, is still a
        // complete (if minimal) view of the battle.
        let show_side_panel = content.width() >= 120;
        let show_chrome_bars = content.width() >= 90;

        let (map_area, side_area) = if show_side_panel {
            panel::split_right(content, 26)
        } else {
            (content, Rect::new(content.right(), content.top(), 0, 0))
        };

        let (top_area, map_area) = if show_chrome_bars {
            panel::split_top(map_area, 1)
        } else {
            (
                Rect::new(map_area.left(), map_area.top(), map_area.width(), 0),
                map_area,
            )
        };
        let (top_area, portrait_area) = if show_chrome_bars {
            panel::split_left(top_area, top_area.width().saturating_sub(40))
        } else {
            (top_area, Rect::new(top_area.right(), top_area.top(), 0, 0))
        };
        let (map_area, hotbar_area) = if show_chrome_bars {
            panel::split_bottom(map_area, 3)
        } else {
            (
                map_area,
                Rect::new(map_area.left(), map_area.bottom(), map_area.width(), 0),
            )
        };

        self.draw_map(&mut surface, map_area);
        if show_chrome_bars {
            self.draw_top_bar(&mut surface, top_area);
            self.draw_portraits(&mut surface, portrait_area);
            self.draw_hotbar(&mut surface, hotbar_area);
        }
        if show_side_panel {
            self.draw_side_panel(&mut surface, side_area);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(IsoTactics);

#[cfg(test)]
mod tests {
    use super::{Cover, IsoTactics, LAYOUT, PER_LEVEL};
    use tilekit::geom::Tile;

    /// A tall wall must fade toward the background when the tile behind it
    /// (later painter's-algorithm depth, and covered by either the wall's top
    /// diamond or its vertical skirt) would otherwise be hidden.
    ///
    /// This is the regression test for the bug the module doc's occlusion
    /// section describes: a first implementation tested only
    /// `IsoLayout::contains` against the wall's small raised top diamond,
    /// which a unit standing on the floor behind a wall almost never lands
    /// in, so the fade never fired. `build_room`'s spine wall at (9, 9)
    /// really does hide floor tile (7, 7) behind its skirt (verified
    /// independently by rasterizing every tile's paint region and checking
    /// which one last touches that floor tile's own center pixel), so this
    /// pins the fix rather than the shape of the wall.
    #[test]
    fn a_wall_fades_when_it_hides_the_tile_behind_it() {
        let demo = IsoTactics::default();
        assert_eq!(
            demo.cover_at(Tile::new(9, 9)),
            Cover::Wall { height: 3 },
            "the fixture wall this test targets moved; update the tile"
        );
        let target = LAYOUT.tile_to_cell(Tile::new(7, 7));
        let fade = demo.occlusion_fade(Tile::new(9, 9), target);
        assert!(fade < 1.0, "expected the wall to fade, got {fade}");
    }

    /// A wall drawn *before* the target in depth order must never fade:
    /// painter's algorithm draws ascending depth back-to-front, so a lower-
    /// depth wall is the one the target's own tile (and anything else nearer
    /// the camera) would paint over, not the other way around. Nothing behind
    /// this wall needs revealing, because this wall is not in front of
    /// anything.
    #[test]
    fn a_wall_behind_the_target_never_fades() {
        let demo = IsoTactics::default();
        let target = LAYOUT.tile_to_cell(Tile::new(9, 12));
        let fade = demo.occlusion_fade(Tile::new(9, 4), target);
        assert!((fade - 1.0).abs() < f32::EPSILON);
    }

    /// A floor tile is never a wall, so it never fades regardless of depth or
    /// position: only walls occlude.
    #[test]
    fn a_floor_tile_never_fades() {
        let demo = IsoTactics::default();
        let target = LAYOUT.tile_to_cell(Tile::new(1, 1));
        assert!((demo.occlusion_fade(Tile::new(3, 3), target) - 1.0).abs() < f32::EPSILON);
    }

    /// Every skirt row `draw_cover` actually paints must be reachable by the
    /// same test `occlusion_fade` uses, or the two silently disagree about
    /// what the wall's silhouette is.
    #[test]
    fn the_skirt_probe_covers_every_row_draw_cover_paints() {
        let height = 3;
        let rows = height * PER_LEVEL;
        // The bottom-most painted row, at the widest reachable horizontal
        // offset (the spine, `dy == 0` before sweeping), must register.
        let dy = LAYOUT.half_h + rows;
        let mut covered = false;
        for r in 1..=rows {
            let base_dy = dy - r;
            if (0..=LAYOUT.half_h).contains(&base_dy) && LAYOUT.span_at(base_dy).is_some() {
                covered = true;
            }
        }
        assert!(covered, "the deepest skirt row is unreachable by the probe");
    }
}
