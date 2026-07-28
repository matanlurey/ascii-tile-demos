//! 20: Realm map -- a Heroes of Might and Magic style adventure map, painted in
//! multi-cell tiles.
//!
//! Every strategy-map demo elsewhere in this gallery draws its tiles flat:
//! [`02_chunky_tiles`](../02_chunky_tiles) bevels a solid color,
//! [`19_hex_command`](../19_hex_command) fills a hex with a texture stamp.
//! `HoMM`, Age of Wonders 2, and Lords of Magic paint theirs, and the difference
//! is not cosmetic: a painted tile varies cell by cell across its own footprint
//! (grass tufts here, bare dirt there) so a field of a hundred identical-biome
//! tiles still reads as terrain rather than as wallpaper, and adjacent tiles of
//! different biomes blend for a few cells at the seam rather than meeting on a
//! hard edge. This demo is that technique, plus the two things that actually
//! make an adventure map playable: a movement-path preview that shows exactly
//! where this turn's budget runs out, and a kingdom sidebar that turns "a map"
//! into "a game running on top of one".
//!
//! Techniques on show:
//!
//! - **Painted multi-cell tiles** ([`paint_tile`]): each world tile is
//!   [`tilekit::geom::SquareLayout::CHUNKY`] (8x4 cells). Every cell within a
//!   tile samples an independent hash-driven texture function for its biome
//!   (tufts, speckle, ripples, snow-glint, rough scree), so no two grass tiles
//!   on the map look identical, and no one tile looks like a stamped copy of
//!   its neighbour.
//! - **Terrain blending at seams** ([`blend_factor`]): a cell within a few
//!   columns/rows of a tile edge partially mixes toward whichever neighbouring
//!   tile's color sits across that edge, so two different biomes meet in a
//!   soft transition band instead of a ruler-straight border -- the effect
//!   `HoMM`'s hand-painted edge tiles achieve, produced procedurally here.
//! - **The `HoMM` path preview** ([`draw_path`]): [`tilekit::path::find`] over
//!   [`tilekit::world::Biome::move_cost`] returns a route plus
//!   [`tilekit::path::Path::reach`], the count of leading steps this turn's
//!   movement budget affords. Steps up to `reach` draw in bright green with
//!   [`tilekit::path::arrow`]'s directional glyphs; the unaffordable remainder
//!   draws in amber. One glance answers "can I get there", "how far this
//!   turn", and "what does the detour cost" without a tooltip, which is why
//!   this is the single most information-dense widget on a `HoMM` screen.
//! - **Flagged objects**: mines, forts, shrines and settlements from
//!   [`tilekit::world::World::landmarks`] draw in
//!   [`tilekit::palette::faction`]'s owner color when flagged, grey when
//!   neutral -- the same "color says who controls this" convention every game
//!   in the reference set uses.
//! - **A resource bar and kingdom panel**: gold/wood/ore/gems/crystal with a
//!   ticking Week/Day clock, a hero list with paired movement/spell
//!   [`ui::panel::bar`] gauges, a town list, and a small minimap with the
//!   camera's current viewport traced on it.
//! - **Two-tier fog**: unexplored tiles are unpainted black;
//!   explored-but-not-visible tiles fade through
//!   [`tilekit::palette::remembered`], the same "terrain stays legible, unit
//!   presence does not" rule [`11_fog_of_war`](../11_fog_of_war) established.
//!
//! ```sh
//! cargo run --example 20_realm_map --features crossterm
//! cargo run --example 20_realm_map --features software
//! cargo run --example 20_realm_map --features gl
//! cargo run --example 20_realm_map  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, SquareLayout, Tile};
use tilekit::noise::hash01;
use tilekit::palette::{self, faction, mix, remembered, scale};
use tilekit::path::{self, Diagonals};
use tilekit::world::{Biome, World};

/// World size, in world tiles (not cells). Deliberately smaller than the
/// per-cell demos' worlds: every tile here costs [`TILE_LAYOUT`]'s 32 cells to
/// draw, so a map generated at cell-demo scale would take whole seconds to
/// paint per frame. [`World::generate`] still runs at cell resolution
/// internally ([`WORLD_W`]/[`WORLD_H`]) and this demo aggregates blocks of it,
/// exactly as [`02_chunky_tiles`](../02_chunky_tiles) does.
const TILES_W: i32 = 46;
/// See [`TILES_W`].
const TILES_H: i32 = 34;

/// World size in cells, i.e. `TILES_W/H * BLOCK`.
const WORLD_W: i32 = TILES_W * BLOCK;
/// See [`WORLD_W`].
const WORLD_H: i32 = TILES_H * BLOCK;

/// World cells aggregated into one strategic tile.
const BLOCK: i32 = 5;

/// The painted tile footprint: 8x4 cells, roughly square at the usual 1:2
/// monospace aspect. See [`tilekit::geom::SquareLayout::CHUNKY`].
const TILE_LAYOUT: SquareLayout = SquareLayout::CHUNKY;

/// Sight radius around each hero, in tiles.
const SIGHT: i32 = 9;

/// Movement points a hero has this turn. `HoMM`'s own baseline (a Pathfinding-
/// less hero on flat ground) is close to this once [`Biome::move_cost`] is
/// read as movement-points-per-tile rather than the abstract road-routing
/// units it was designed for.
const MOVE_BUDGET: u32 = 14;

/// How many world-seconds one in-game day takes. Slow enough that the
/// resource bar's Week/Day readout is legible before it changes, fast enough
/// that leaving the demo running for a few seconds visibly advances it, which
/// is what the animation-must-not-be-static-by-input rule needs.
const DAY_LENGTH: f32 = 6.0;

/// One playable hero: a position, a name, and the two gauges `HoMM` tracks per
/// hero.
struct Hero {
    name: &'static str,
    pos: Tile,
    /// Movement points remaining this turn, `0.0..=1.0` of [`MOVE_BUDGET`].
    movement: f32,
    /// Spell points, `0.0..=1.0`, drained by nothing here but drawn anyway:
    /// the gauge pairing is the point being demonstrated, not spellcasting.
    spell: f32,
}

/// One town: a position and who owns it.
struct Town {
    name: &'static str,
    tile: Tile,
    owner: usize,
}

/// One strategic tile's aggregated data, built once per world/zoom change.
#[derive(Clone, Copy)]
struct StratTile {
    biome: Biome,
    has_river: bool,
    has_road: bool,
}

/// The aggregated strategic map: one [`StratTile`] per [`BLOCK`]-cell block of
/// the underlying per-cell [`World`].
struct StrategicMap {
    tiles: Vec<StratTile>,
}

impl StrategicMap {
    fn build(world: &World) -> Self {
        let mut tiles = Vec::with_capacity((TILES_W * TILES_H) as usize);
        for ty in 0..TILES_H {
            for tx in 0..TILES_W {
                let (x0, y0) = (tx * BLOCK, ty * BLOCK);
                let mut counts: Vec<(Biome, u32)> = Vec::new();
                let (mut has_river, mut has_road) = (false, false);
                for dy in 0..BLOCK {
                    for dx in 0..BLOCK {
                        let (x, y) = (x0 + dx, y0 + dy);
                        let biome = world.biome_at(x, y);
                        if let Some(slot) = counts.iter_mut().find(|(b, _)| *b == biome) {
                            slot.1 += 1;
                        } else {
                            counts.push((biome, 1));
                        }
                        has_river |= world.river_at(x, y);
                        has_road |= world.road_at(x, y);
                    }
                }
                // Ties break on the biome's own ordering rather than iteration
                // order, so the aggregated map is reproducible from its seed;
                // see 02_chunky_tiles's identical comment for why this matters.
                let biome = counts
                    .into_iter()
                    .max_by_key(|&(biome, n)| (n, core::cmp::Reverse(biome)))
                    .map_or(Biome::Ocean, |(b, _)| b);
                tiles.push(StratTile {
                    biome,
                    has_river,
                    has_road,
                });
            }
        }
        Self { tiles }
    }

    fn get(&self, tx: i32, ty: i32) -> Option<StratTile> {
        if tx < 0 || ty < 0 || tx >= TILES_W || ty >= TILES_H {
            return None;
        }
        self.tiles.get((ty * TILES_W + tx) as usize).copied()
    }
}

/// Per-tile exploration state, mirroring [`11_fog_of_war`](../11_fog_of_war)'s
/// two-tier model at the strategic-tile granularity this map actually uses.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Fog {
    #[default]
    Unexplored,
    Remembered,
    Visible,
}

/// State: the generated world and its strategic aggregation, heroes, towns,
/// fog, camera, resources, and the pending move order.
pub struct RealmMap {
    world: World,
    map: StrategicMap,
    fog: Vec<Fog>,
    heroes: Vec<Hero>,
    active_hero: usize,
    towns: Vec<Town>,
    /// Top-left visible tile.
    origin: Tile,
    /// Tile under the cursor: the path-preview destination.
    cursor: Tile,
    time: f32,
    day: u32,
    gold: u32,
    wood: u32,
    ore: u32,
    gems: u32,
    crystal: u32,
    small_tiles: bool,
    fps: FpsMeter,
}

impl Default for RealmMap {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 5);
        let map = StrategicMap::build(&world);
        let fog = vec![Fog::Unexplored; (TILES_W * TILES_H) as usize];

        let capital = tile_of_world_pos(world.start_position());
        let heroes = vec![
            Hero {
                name: "Aldric",
                pos: capital,
                movement: 1.0,
                spell: 0.7,
            },
            Hero {
                name: "Sable",
                pos: Tile::new(
                    (capital.col + 6).clamp(0, TILES_W - 1),
                    (capital.row + 2).clamp(0, TILES_H - 1),
                ),
                movement: 0.45,
                spell: 0.9,
            },
        ];

        let towns = build_towns(&world);

        let mut state = Self {
            world,
            map,
            fog,
            heroes,
            active_hero: 0,
            towns,
            origin: Tile::new((capital.col - 10).max(0), (capital.row - 8).max(0)),
            cursor: capital,
            time: 0.0,
            day: 3,
            gold: 4820,
            wood: 26,
            ore: 19,
            gems: 4,
            crystal: 7,
            small_tiles: false,
            fps: FpsMeter::new(),
        };
        state.reveal_around_heroes();
        state
    }
}

/// Places two neutral/owned towns near landmark settlements, so the town list
/// and minimap have something to show without inventing a whole city system.
fn build_towns(world: &World) -> Vec<Town> {
    let mut towns = Vec::new();
    let mut names = ["Ashgate", "Brannock", "Ivywatch"].into_iter();
    for landmark in world
        .landmarks
        .iter()
        .filter(|l| l.site.is_settlement())
        .take(3)
    {
        let Some(name) = names.next() else { break };
        towns.push(Town {
            name,
            tile: tile_of_world_pos((landmark.x, landmark.y)),
            // The first town is the player's own; the rest are neutral,
            // giving the flagged-object rendering something of each kind to
            // draw without a full diplomacy model.
            owner: usize::from(towns.is_empty()),
        });
    }
    towns
}

/// Which strategic tile a world cell position falls in.
const fn tile_of_world_pos((x, y): (i32, i32)) -> Tile {
    Tile::new(
        clamp_i32(x / BLOCK, 0, TILES_W - 1),
        clamp_i32(y / BLOCK, 0, TILES_H - 1),
    )
}

/// `i32::clamp` is not `const`.
const fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

impl RealmMap {
    /// Marks every tile within a fixed radius of each hero as visible, and
    /// everything previously visible as merely remembered. Called after any
    /// hero moves and once at startup.
    fn reveal_around_heroes(&mut self) {
        for fog in &mut self.fog {
            if *fog == Fog::Visible {
                *fog = Fog::Remembered;
            }
        }
        for hero in &self.heroes {
            for dy in -SIGHT..=SIGHT {
                for dx in -SIGHT..=SIGHT {
                    if dx * dx + dy * dy > SIGHT * SIGHT {
                        continue;
                    }
                    let (tx, ty) = (hero.pos.col + dx, hero.pos.row + dy);
                    if tx < 0 || ty < 0 || tx >= TILES_W || ty >= TILES_H {
                        continue;
                    }
                    self.fog[(ty * TILES_W + tx) as usize] = Fog::Visible;
                }
            }
        }
    }

    fn fog_at(&self, tx: i32, ty: i32) -> Fog {
        if tx < 0 || ty < 0 || tx >= TILES_W || ty >= TILES_H {
            return Fog::Unexplored;
        }
        self.fog[(ty * TILES_W + tx) as usize]
    }

    const fn tile_layout(&self) -> SquareLayout {
        if self.small_tiles {
            SquareLayout::MEDIUM
        } else {
            TILE_LAYOUT
        }
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        self.map = StrategicMap::build(&self.world);
        self.fog.fill(Fog::Unexplored);
        self.towns = build_towns(&self.world);

        let capital = tile_of_world_pos(self.world.start_position());
        self.heroes[0].pos = capital;
        self.heroes[0].movement = 1.0;
        self.cursor = capital;
        self.origin = Tile::new((capital.col - 10).max(0), (capital.row - 8).max(0));
        self.reveal_around_heroes();
    }

    fn pan(&mut self, dx: i32, dy: i32) {
        self.origin = Tile::new(
            (self.origin.col + dx).clamp(0, (TILES_W - 1).max(0)),
            (self.origin.row + dy).clamp(0, (TILES_H - 1).max(0)),
        );
    }

    /// Re-centers the camera on the cursor if it has scrolled off (or nearly
    /// off) the current viewport.
    ///
    /// Margin-triggered rather than locked dead-center every frame: a camera
    /// that recenters the instant the cursor moves gives the player no sense
    /// of where they are relative to the rest of the map, since the view
    /// never holds still long enough to read as "panning". Waiting until the
    /// cursor nears the edge is the same compromise every scrolling map in
    /// the reference games makes.
    ///
    /// This also fixes what would otherwise be a real bug at small window
    /// sizes: [`Self::small_tiles`] changes the tile footprint (and therefore
    /// how many tiles the same pixel viewport covers) without moving the
    /// camera, so a camera that only ever centered once at startup would
    /// leave most of a zoomed-out MEDIUM view sitting outside both the
    /// viewport-appropriate origin and the hero's fixed sight radius,
    /// reading as an almost entirely unexplored map.
    fn follow_cursor(&mut self, layout: SquareLayout, area: Rect) {
        let cols = (i32::from(area.width()) / layout.w).max(1);
        let rows = (i32::from(area.height()) / layout.h).max(1);
        let margin = 2;

        if self.cursor.col < self.origin.col + margin {
            self.origin.col = (self.cursor.col - margin).max(0);
        } else if self.cursor.col > self.origin.col + cols - 1 - margin {
            self.origin.col = self.cursor.col - cols + 1 + margin;
        }
        if self.cursor.row < self.origin.row + margin {
            self.origin.row = (self.cursor.row - margin).max(0);
        } else if self.cursor.row > self.origin.row + rows - 1 - margin {
            self.origin.row = self.cursor.row - rows + 1 + margin;
        }
        self.origin = Tile::new(
            self.origin.col.clamp(0, (TILES_W - cols).max(0)),
            self.origin.row.clamp(0, (TILES_H - rows).max(0)),
        );
    }

    /// Moves the cursor by one tile, clamped to the map.
    fn move_cursor(&mut self, dx: i32, dy: i32) {
        self.cursor = Tile::new(
            (self.cursor.col + dx).clamp(0, TILES_W - 1),
            (self.cursor.row + dy).clamp(0, TILES_H - 1),
        );
    }

    /// The active hero's planned route to the cursor, or `None` if the cursor
    /// is the hero's own tile or unreachable.
    fn planned_path(&self) -> Option<path::Path> {
        let hero = self.heroes.get(self.active_hero)?;
        let start = Cell::new(hero.pos.col, hero.pos.row);
        let goal = Cell::new(self.cursor.col, self.cursor.row);
        let budget = (hero.movement * MOVE_BUDGET as f32).round() as u32;

        path::find(
            start,
            goal,
            TILES_W,
            TILES_H,
            Diagonals::Costly,
            budget,
            |cell| {
                self.map
                    .get(cell.x, cell.y)
                    .map_or(path::IMPASSABLE, |t| t.biome.move_cost())
            },
        )
    }

    /// Moves the active hero as far along the planned path as this turn's
    /// movement allows, spending the cost and revealing fog along the way.
    fn commit_move(&mut self) {
        let Some(route) = self.planned_path() else {
            return;
        };
        if route.reach == 0 {
            return;
        }
        let stop = route.steps[route.reach - 1];
        let spent = route.costs[route.reach - 1];

        if let Some(hero) = self.heroes.get_mut(self.active_hero) {
            hero.pos = Tile::new(stop.x, stop.y);
            let budget_used = f32::from(u16::try_from(spent).unwrap_or(u16::MAX));
            hero.movement = (hero.movement - budget_used / MOVE_BUDGET as f32).max(0.0);
        }
        self.cursor = self.heroes[self.active_hero].pos;
        self.reveal_around_heroes();
    }

    fn next_hero(&mut self) {
        if self.heroes.is_empty() {
            return;
        }
        self.active_hero = (self.active_hero + 1) % self.heroes.len();
        self.cursor = self.heroes[self.active_hero].pos;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        let content_top = i32::from(term.size().height >= 3);
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
                    KeyCode::Enter => self.commit_move(),
                    KeyCode::Tab => self.next_hero(),
                    KeyCode::Char('m' | 'M') => self.small_tiles = !self.small_tiles,
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                },
                Event::Mouse(mouse) => self.handle_mouse(content_top, mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, content_top: i32, kind: MouseEventKind, pos: retroglyph_core::Pos) {
        let layout = self.tile_layout();
        // The map area starts after the resource bar (1 row) and, at wide
        // enough windows, after the left kingdom column; screen_to_tile only
        // needs the map's own top-left in screen space, which callers who
        // want exact hit-testing pass in. Mouse targeting here is therefore
        // approximate at the panel boundary, and exact once the pointer is
        // over the map itself, which is the only place it needs to be exact.
        let sx = i32::from(pos.x);
        let sy = i32::from(pos.y) - content_top;
        if sy < 0 {
            return;
        }
        let (tx, ty) = (
            self.origin.col + sx / layout.w,
            self.origin.row + sy / layout.h,
        );
        match kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                if tx >= 0 && ty >= 0 && tx < TILES_W && ty < TILES_H {
                    self.cursor = Tile::new(tx, ty);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => self.commit_move(),
            MouseEventKind::Scroll { dy, .. } if dy > 0.0 => self.pan(0, -1),
            MouseEventKind::Scroll { dy, .. } if dy < 0.0 => self.pan(0, 1),
            _ => {}
        }
    }

    fn status(&self) -> String {
        let hero = self.heroes.get(self.active_hero);
        let name = hero.map_or("--", |h| h.name);
        let biome = self
            .map
            .get(self.cursor.col, self.cursor.row)
            .map_or("--", |t| t.biome.name());
        format!(
            "{name}  cursor ({}, {})  {biome}  seed {}",
            self.cursor.col,
            self.cursor.row,
            self.world.seed()
        )
    }
}

/// Per-cell interior texture for a tile's biome, sampled independently at
/// every cell so a field of same-biome tiles never looks like a repeating
/// stamp. Returns a color multiplier applied to the tile's base biome color.
///
/// One function covering every biome rather than a per-biome closure map,
/// because the whole point is that each case is a one-line noise shape: no
/// case here needs enough logic to justify its own function, and reading them
/// side by side is what makes the "every biome gets its own texture language"
/// claim checkable at a glance.
fn texture(biome: Biome, wx: i32, wy: i32, cx: i32, cy: i32, time: f32) -> f32 {
    let h = |salt: u32| hash01(salt, wx * 1000 + cx, wy * 1000 + cy);
    match biome {
        Biome::Grassland | Biome::Savanna => {
            // Sparse bright tufts: most cells flat, a scattered few lifted.
            if h(0x9E17) < 0.16 { 1.14 } else { 0.97 }
        }
        Biome::Forest | Biome::Jungle => {
            // Canopy clumps: a low-frequency blob mask rather than per-cell
            // noise, so texture reads as foliage mass, not static.
            let blob = hash01(0xA11C, wx * 3 + cx / 3, wy * 3 + cy / 3);
            if blob < 0.4 { 0.86 } else { 1.05 }
        }
        Biome::Taiga => {
            let blob = hash01(0xB22D, wx * 3 + cx / 3, wy * 3 + cy / 3);
            if blob < 0.35 { 0.82 } else { 1.0 }
        }
        Biome::Desert | Biome::Scrubland => {
            // Dune ripple: a slow sine across the tile's width, animated so
            // heat-shimmer sells the desert even while the camera is still.
            let ripple = time.mul_add(0.6, (cx as f32) * 0.9).sin();
            ripple.mul_add(0.05, 1.0)
        }
        Biome::Coast => {
            if h(0xC33E) < 0.3 {
                1.1
            } else {
                0.95
            }
        }
        Biome::Ice | Biome::Peak => {
            // Glinting speckle: rare, bright, animated so a snowfield is not
            // perfectly static even at rest.
            let glint = hash01(0xD44F ^ (time as u32), wx + cx, wy + cy);
            if glint < 0.06 { 1.35 } else { 0.98 }
        }
        Biome::Tundra => {
            if h(0xE550) < 0.2 {
                1.08
            } else {
                0.94
            }
        }
        Biome::Marsh => {
            if h(0xF661) < 0.22 {
                0.8
            } else {
                1.0
            }
        }
        Biome::Mountain => {
            // Scree: coarse blocky noise, unlike grass's fine scatter, so
            // rock reads as chunky rather than speckled.
            let block = hash01(0x1772, wx * 2 + cx / 2, wy * 2 + cy / 2);
            block.mul_add(0.3, 0.82)
        }
        Biome::Ocean | Biome::Sea | Biome::Lake => {
            // A slow travelling wave, the one texture in this table driven
            // purely by time and position rather than a hash, because water
            // is the one terrain whose texture is supposed to visibly move.
            let wave = (wy as f32)
                .mul_add(0.4, time.mul_add(-1.3, (cx as f32) * 0.7))
                .sin();
            wave.mul_add(0.06, 1.0)
        }
    }
}

/// How much a cell blends toward the neighbouring tile's color, given its
/// distance in cells from that edge, within a soft blend band.
///
/// Linear falloff over a few cells reads as a hand-painted transition rather
/// than a hard seam; a wider band would blur tiles into their neighbours
/// badly enough that a single tile's own biome becomes hard to read.
fn blend_factor(distance_from_edge: i32, band: i32) -> f32 {
    if distance_from_edge >= band {
        return 0.0;
    }
    1.0 - distance_from_edge as f32 / band as f32
}

/// Path-preview colors: bright green for this turn's affordable steps,
/// amber for the unaffordable remainder.
const PATH_REACHABLE: Color = palette::rgb(120, 226, 120);
const PATH_UNREACHABLE: Color = palette::rgb(214, 158, 74);

/// Paints one strategic tile's full cell block: base texture, edge blending
/// toward its neighbours, and river/road overlays.
#[allow(clippy::too_many_arguments)]
fn paint_tile(
    surface: &mut Surface<'_>,
    area: Rect,
    map: &StrategicMap,
    fog: Fog,
    layout: SquareLayout,
    tx: i32,
    ty: i32,
    sx: i32,
    sy: i32,
    time: f32,
) {
    let Some(strat) = map.get(tx, ty) else { return };
    if fog == Fog::Unexplored {
        for dy in 0..layout.h {
            for dx in 0..layout.w {
                put_cell(
                    surface,
                    area,
                    sx + dx,
                    sy + dy,
                    ' ',
                    palette::BLACK,
                    palette::BLACK,
                );
            }
        }
        return;
    }

    let base = strat.biome.color();
    // Blend band as a fraction of the tile so it scales sensibly between the
    // full CHUNKY size and the smaller MEDIUM zoom.
    let band_x = (layout.w / 3).max(1);
    let band_y = (layout.h / 3).max(1);

    for dy in 0..layout.h {
        for dx in 0..layout.w {
            let mut color = scale(base, texture(strat.biome, tx, ty, dx, dy, time));

            // Blend toward each of the four neighbours near their shared
            // edge. Doing all four independently (rather than picking "the"
            // nearest edge) is what makes a corner cell blend toward both of
            // its adjacent neighbours at once, which is the case a hand
            // -painted tile sheet actually handles with a dedicated corner
            // tile and a procedural version has to fake by summing blends.
            let west = blend_factor(dx, band_x);
            let east = blend_factor(layout.w - 1 - dx, band_x);
            let north = blend_factor(dy, band_y);
            let south = blend_factor(layout.h - 1 - dy, band_y);

            if west > 0.0
                && let Some(n) = map.get(tx - 1, ty)
            {
                color = mix(color, n.biome.color(), west * 0.5);
            }
            if east > 0.0
                && let Some(n) = map.get(tx + 1, ty)
            {
                color = mix(color, n.biome.color(), east * 0.5);
            }
            if north > 0.0
                && let Some(n) = map.get(tx, ty - 1)
            {
                color = mix(color, n.biome.color(), north * 0.5);
            }
            if south > 0.0
                && let Some(n) = map.get(tx, ty + 1)
            {
                color = mix(color, n.biome.color(), south * 0.5);
            }

            if fog == Fog::Remembered {
                color = remembered(color, ui::BG);
            }
            put_cell(surface, area, sx + dx, sy + dy, ' ', color, color);
        }
    }

    // River and road as a single center line, drawn after texture/blend so
    // they read as features laid over the terrain rather than terrain.
    if strat.has_river && fog != Fog::Unexplored {
        let y = sy + layout.h / 2;
        let water = mix(base, palette::rgb(120, 176, 224), 0.8);
        for dx in 0..layout.w {
            put_cell(
                surface,
                area,
                sx + dx,
                y,
                '~',
                palette::rgb(210, 232, 250),
                water,
            );
        }
    } else if strat.has_road && fog != Fog::Unexplored {
        let y = sy + layout.h / 2;
        let road = mix(base, palette::rgb(214, 196, 156), 0.55);
        for dx in 0..layout.w {
            let glyph = if dx % 2 == 0 { '\u{00b7}' } else { ' ' };
            put_cell(
                surface,
                area,
                sx + dx,
                y,
                glyph,
                palette::rgb(228, 214, 176),
                road,
            );
        }
    }
}

/// Writes one cell within a painted tile, clipped to `area`.
fn put_cell(
    surface: &mut Surface<'_>,
    area: Rect,
    cx: i32,
    cy: i32,
    glyph: char,
    fg: Color,
    bg: Color,
) {
    if cx < 0 || cy < 0 || cx >= i32::from(area.width()) || cy >= i32::from(area.height()) {
        return;
    }
    surface.put(
        (area.left() + cx as u16, area.top() + cy as u16),
        glyph,
        Style::new().fg(fg).bg(bg),
    );
}

impl RealmMap {
    /// Draws the full map: painted tiles, flagged objects, heroes, path
    /// preview, and the cursor ring, back to front.
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let layout = self.tile_layout();
        let visible_cols = i32::from(area.width()) / layout.w + 1;
        let visible_rows = i32::from(area.height()) / layout.h + 1;

        for row in 0..visible_rows {
            for col in 0..visible_cols {
                let (tx, ty) = (self.origin.col + col, self.origin.row + row);
                let fog = self.fog_at(tx, ty);
                paint_tile(
                    surface,
                    area,
                    &self.map,
                    fog,
                    layout,
                    tx,
                    ty,
                    col * layout.w,
                    row * layout.h,
                    self.time,
                );
            }
        }

        self.draw_objects(surface, area, layout);
        self.draw_path(surface, area, layout);
        self.draw_heroes(surface, area, layout);
        self.draw_cursor(surface, area, layout);
    }

    /// Draws landmarks and towns, flagged in their owner's color.
    fn draw_objects(&self, surface: &mut Surface<'_>, area: Rect, layout: SquareLayout) {
        for landmark in &self.world.landmarks {
            let tile = tile_of_world_pos((landmark.x, landmark.y));
            if self.fog_at(tile.col, tile.row) == Fog::Unexplored {
                continue;
            }
            let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, tile) else {
                continue;
            };
            let (glyph, mut color) = landmark.site.glyph_color();
            if self.fog_at(tile.col, tile.row) == Fog::Remembered {
                color = remembered(color, ui::BG);
            }
            let center = (layout.w / 2, layout.h / 2);
            put_cell(
                surface,
                area,
                sx + center.0,
                sy + center.1,
                glyph,
                color,
                self.map
                    .get(tile.col, tile.row)
                    .map_or(palette::BLACK, |t| t.biome.color()),
            );
        }

        for town in &self.towns {
            if self.fog_at(town.tile.col, town.tile.row) == Fog::Unexplored {
                continue;
            }
            let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, town.tile) else {
                continue;
            };
            let owner_color = faction(town.owner);
            let center = (layout.w / 2, layout.h / 2);
            put_cell(
                surface,
                area,
                sx + center.0 - 1,
                sy + center.1,
                '\u{2302}',
                owner_color,
                palette::BLACK,
            );
        }
    }

    /// Draws the active hero's planned path: green up to `reach`, amber past
    /// it, with a destination marker.
    fn draw_path(&self, surface: &mut Surface<'_>, area: Rect, layout: SquareLayout) {
        let Some(route) = self.planned_path() else {
            return;
        };
        let Some(hero) = self.heroes.get(self.active_hero) else {
            return;
        };

        let mut prev = Cell::new(hero.pos.col, hero.pos.row);
        for (i, &step) in route.steps.iter().enumerate() {
            let color = if i < route.reach {
                PATH_REACHABLE
            } else {
                PATH_UNREACHABLE
            };
            let glyph = path::arrow(prev, step);
            let tile = Tile::new(step.x, step.y);
            if let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, tile) {
                let center = (layout.w / 2, layout.h / 2);
                put_cell(
                    surface,
                    area,
                    sx + center.0,
                    sy + center.1,
                    glyph,
                    color,
                    palette::BLACK,
                );
            }
            prev = step;
        }

        if let Some(&last) = route.steps.last() {
            let tile = Tile::new(last.x, last.y);
            if let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, tile) {
                let color = if route.complete() {
                    PATH_REACHABLE
                } else {
                    PATH_UNREACHABLE
                };
                let blink = (self.time % 0.8) < 0.5;
                if blink {
                    put_cell(surface, area, sx, sy, 'X', color, palette::BLACK);
                }
            }
        }
    }

    /// Draws every hero as a colored token.
    fn draw_heroes(&self, surface: &mut Surface<'_>, area: Rect, layout: SquareLayout) {
        for (i, hero) in self.heroes.iter().enumerate() {
            let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, hero.pos) else {
                continue;
            };
            let center = (layout.w / 2, layout.h / 2);
            let color = if i == self.active_hero {
                palette::rgb(255, 236, 170)
            } else {
                mix(palette::rgb(255, 236, 170), palette::BLACK, 0.35)
            };
            let base = self
                .map
                .get(hero.pos.col, hero.pos.row)
                .map_or(palette::BLACK, |t| t.biome.color());
            put_cell(
                surface,
                area,
                sx + center.0,
                sy + center.1,
                '@',
                color,
                base,
            );
        }
    }

    /// Draws a pulsing ring around the cursor tile.
    fn draw_cursor(&self, surface: &mut Surface<'_>, area: Rect, layout: SquareLayout) {
        let Some((sx, sy)) = tile_screen_origin(self.origin, area, layout, self.cursor) else {
            return;
        };
        let pulse = (self.time * 3.0).sin().mul_add(0.5, 0.5);
        let color = mix(palette::rgb(246, 196, 96), palette::WHITE, pulse * 0.4);
        for dx in 0..layout.w {
            put_ring(surface, area, sx + dx, sy, color);
            put_ring(surface, area, sx + dx, sy + layout.h - 1, color);
        }
        for dy in 0..layout.h {
            put_ring(surface, area, sx, sy + dy, color);
            put_ring(surface, area, sx + layout.w - 1, sy + dy, color);
        }
    }
}

/// Overlays a cursor-ring pixel onto whatever is already there, tinting
/// rather than replacing, so the ring reads as a highlight over the terrain
/// instead of punching a hole in it.
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

/// The screen-space top-left of `tile`'s cell block, or `None` if it falls
/// entirely outside `area`.
fn tile_screen_origin(
    origin: Tile,
    area: Rect,
    layout: SquareLayout,
    tile: Tile,
) -> Option<(i32, i32)> {
    let sx = (tile.col - origin.col) * layout.w;
    let sy = (tile.row - origin.row) * layout.h;
    if sx + layout.w <= 0
        || sy + layout.h <= 0
        || sx >= i32::from(area.width())
        || sy >= i32::from(area.height())
    {
        return None;
    }
    Some((sx, sy))
}

impl RealmMap {
    /// Draws the top resource bar: gold, wood, ore, gems, crystal, and the
    /// Week/Day clock.
    fn draw_resource_bar(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let week = self.day / 7 + 1;
        let day_of_week = self.day % 7 + 1;
        // U+2504 (dashed wood plank) and U+25C7 (crystal outline) both fall
        // outside CP437 and render as the solid-block fallback on the pixel
        // backends -- see `examples/tests/glyphs.rs` and
        // <https://github.com/crates-lurey-io/retroglyph/issues/539>. `=` and
        // `*` are the CP437-safe stand-ins: plain but colorable, which is the
        // trade this whole gallery makes.
        let entries: [(char, Color, u32); 5] = [
            ('$', palette::rgb(240, 200, 90), self.gold),
            ('=', palette::rgb(176, 138, 92), self.wood),
            ('\u{25AC}', palette::rgb(150, 150, 158), self.ore),
            ('\u{2666}', palette::rgb(220, 110, 200), self.gems),
            ('*', palette::rgb(120, 210, 220), self.crystal),
        ];
        let mut x = area.left() + 1;
        for (glyph, color, amount) in entries {
            if x + 8 >= area.right() {
                break;
            }
            surface.put(
                (x, area.top()),
                glyph,
                Style::new().fg(color).bg(ui::CHROME_BG),
            );
            surface.print(
                (x + 2, area.top()),
                &format!("{amount}"),
                Style::new().fg(ui::FG).bg(ui::CHROME_BG),
            );
            x += 9;
        }

        let clock = format!("Week {week}, Day {day_of_week}");
        let clock_w = clock.chars().count() as u16;
        if area.width() > clock_w + 2 {
            surface.print(
                (area.right() - clock_w - 1, area.top()),
                &clock,
                Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
            );
        }
    }

    /// Draws the right-hand kingdom panel: hero list, town list, and minimap.
    ///
    /// `map_area` is the map viewport's own rect, not this panel's: the
    /// minimap needs it to trace the camera's current viewport, and by the
    /// time this runs the map has already been laid out against a different
    /// area than this sidebar.
    fn draw_kingdom_panel(&self, surface: &mut Surface<'_>, area: Rect, map_area: Rect) {
        let hero_rows = (self.heroes.len() as u16 * 3 + 2).min(area.height());
        let (hero_area, rest) = panel::split_top(area, hero_rows);
        let town_rows = (self.towns.len() as u16 * 2 + 2).min(rest.height() / 2);
        let (town_area, minimap_area) = panel::split_top(rest, town_rows);

        self.draw_hero_list(surface, hero_area);
        self.draw_town_list(surface, town_area);
        self.draw_minimap(surface, minimap_area, map_area);
    }

    fn draw_hero_list(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Heroes")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        for (i, hero) in self.heroes.iter().enumerate() {
            let y = inner.top() + i as u16 * 3;
            if y + 1 >= inner.bottom() {
                break;
            }
            let active = i == self.active_hero;
            let color = if active { ui::ACCENT } else { ui::FG };
            // U+25B8 (small right triangle) is outside CP437; U+25BA (the
            // larger arrowhead) is the nearest one that is actually colorable.
            let marker = if active { '\u{25BA}' } else { ' ' };
            surface.print(
                (inner.left(), y),
                &format!("{marker}{}", hero.name),
                Style::new().fg(color).bg(panel::PANEL_BG),
            );
            if y + 1 < inner.bottom() {
                panel::bar(
                    surface,
                    (inner.left(), y + 1),
                    inner.width().min(12),
                    hero.movement,
                    palette::rgb(120, 210, 120),
                    palette::rgb(30, 40, 30),
                );
            }
            if y + 2 < inner.bottom() {
                panel::bar(
                    surface,
                    (inner.left(), y + 2),
                    inner.width().min(12),
                    hero.spell,
                    palette::rgb(110, 160, 230),
                    palette::rgb(24, 30, 46),
                );
            }
        }
    }

    fn draw_town_list(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Towns")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        for (i, town) in self.towns.iter().enumerate() {
            let y = inner.top() + i as u16 * 2;
            if y >= inner.bottom() {
                break;
            }
            let owner_color = faction(town.owner);
            surface.put(
                (inner.left(), y),
                '\u{2302}',
                Style::new().fg(owner_color).bg(panel::PANEL_BG),
            );
            panel::spans(
                surface,
                (inner.left() + 2, y),
                inner.width().saturating_sub(2),
                &[panel::Span::plain(town.name)],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect, map_area: Rect) {
        let inner = panel::Panel::new()
            .title("Realm")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let layout = self.tile_layout();
        let visible_cols = i32::from(inner.width()).max(1);
        let visible_rows = i32::from(inner.height()).max(1);

        for row in 0..visible_rows {
            for col in 0..visible_cols {
                let tx = col * TILES_W / visible_cols;
                let ty = row * TILES_H / visible_rows;
                let fog = self.fog_at(tx, ty);
                let color = match (fog, self.map.get(tx, ty)) {
                    (Fog::Unexplored, _) | (_, None) => palette::BLACK,
                    (Fog::Remembered, Some(t)) => remembered(t.biome.color(), ui::BG),
                    (Fog::Visible, Some(t)) => t.biome.color(),
                };
                surface.put(
                    (inner.left() + col as u16, inner.top() + row as u16),
                    ' ',
                    Style::new().bg(color),
                );
            }
        }

        for (i, town) in self.towns.iter().enumerate() {
            let col = town.tile.col * visible_cols / TILES_W;
            let row = town.tile.row * visible_rows / TILES_H;
            if col < visible_cols && row < visible_rows {
                surface.put(
                    (inner.left() + col as u16, inner.top() + row as u16),
                    char::from_digit((i as u32 + 1).min(9), 10).unwrap_or('#'),
                    Style::new().fg(faction(town.owner)).bg(palette::BLACK),
                );
            }
        }

        // The camera's current viewport, traced as a rectangle -- the
        // minimap's whole reason for existing is to answer "where am I on
        // the larger map", and a color-block fill in a corner is not
        // navigable, only decorative.
        let view_cols = (i32::from(map_area.width()) / layout.w).max(1);
        let view_rows = (i32::from(map_area.height()) / layout.h).max(1);
        let vx0 = self.origin.col * visible_cols / TILES_W;
        let vy0 = self.origin.row * visible_rows / TILES_H;
        let vx1 = ((self.origin.col + view_cols) * visible_cols / TILES_W).min(visible_cols - 1);
        let vy1 = ((self.origin.row + view_rows) * visible_rows / TILES_H).min(visible_rows - 1);
        for col in vx0..=vx1 {
            trace_ring(surface, inner, col, vy0, ui::ACCENT);
            trace_ring(surface, inner, col, vy1, ui::ACCENT);
        }
        for row in vy0..=vy1 {
            trace_ring(surface, inner, vx0, row, ui::ACCENT);
            trace_ring(surface, inner, vx1, row, ui::ACCENT);
        }
    }
}

/// Draws one cell of the minimap's viewport-trace rectangle outline.
fn trace_ring(surface: &mut Surface<'_>, inner: Rect, col: i32, row: i32, color: Color) {
    if col < 0 || row < 0 || col >= i32::from(inner.width()) || row >= i32::from(inner.height()) {
        return;
    }
    surface.put(
        (inner.left() + col as u16, inner.top() + row as u16),
        '\u{2591}',
        Style::new().fg(color).bg(palette::BLACK),
    );
}

/// Below this width the kingdom panel shrinks to just the hero list.
const HERO_ONLY_BELOW: u16 = 150;
/// Below this width the kingdom panel is dropped entirely, and tiles shrink
/// to [`SquareLayout::MEDIUM`] so the map still shows a useful amount of the
/// world rather than a handful of oversized tiles.
const HIDE_PANEL_BELOW: u16 = 100;

impl Demo for RealmMap {
    const NAME: &'static str = "20_realm_map";
    const TITLE: &'static str = "20 Realm map";
    const BLURB: &'static str = "Painted HoMM-style tiles with a live movement path preview.";
    const GRID: (u16, u16) = (170, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move cursor"),
            ("click/drag", "target"),
            ("Enter", "move hero"),
            ("Tab", "cycle hero"),
            ("M", "tile size"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        // A slow-ticking clock keeps the resource bar visibly animating on
        // its own, independent of any input -- the property the thumbnail
        // tool checks for every demo.
        let day_f = self.time / DAY_LENGTH;
        self.day = 3 + day_f as u32;

        let width = term.area().width();
        // Below HIDE_PANEL_BELOW the map itself also drops to MEDIUM tiles,
        // so a narrow window still shows a useful span of world rather than
        // three oversized CHUNKY tiles.
        self.small_tiles = width < HIDE_PANEL_BELOW;

        let (title, chrome_content, status) = ui::split_chrome(term.area());
        let (resource_area, content) = panel::split_top(chrome_content, 1);

        // Computed before drawing (and before the camera follows the cursor)
        // because both need the map's own rect, which depends on whether the
        // kingdom panel is showing at this width.
        let show_panel = width >= HIDE_PANEL_BELOW;
        let panel_w = if width >= HERO_ONLY_BELOW { 30 } else { 24 };
        let (map_area, panel_area) = if show_panel {
            panel::split_right(content, panel_w)
        } else {
            (
                content,
                Rect::new(content.right(), content.top(), 0, content.height()),
            )
        };
        self.follow_cursor(self.tile_layout(), map_area);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_resource_bar(&mut surface, resource_area);
        self.draw_map(&mut surface, map_area);
        if show_panel && panel_area.width() > 0 {
            if width >= HERO_ONLY_BELOW {
                self.draw_kingdom_panel(&mut surface, panel_area, map_area);
            } else {
                self.draw_hero_list(&mut surface, panel_area);
            }
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(RealmMap);
