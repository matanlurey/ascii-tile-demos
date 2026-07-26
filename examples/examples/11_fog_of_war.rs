//! 11: Fog of war -- exploration memory versus live field of view.
//!
//! Field of view and fog of war look like the same feature and are not. Field
//! of view is a geometric query recomputed every time a unit moves: from here,
//! with these obstacles, what can I see right now? Fog of war is accumulated
//! *memory* of that query over time, and it is what makes scouting a real
//! decision in a 4X game -- what you remember about a tile can be stale, and
//! finding out it changed is why you send a unit back.
//!
//! This demo drives [`tilekit::fov::shadowcast`] from two units: an automatic
//! scout that patrols the settlement road network end to end, and a
//! player-controlled scout moved with the keyboard. Both feed the same
//! [`tilekit::fov::FogMap`], so watch what happens when they diverge -- the
//! area only the automatic scout has seen fades to "explored" the moment the
//! player scout's own view moves on, while both remain "visible" only in the
//! overlap.
//!
//! Techniques on show:
//!
//! - **Recursive shadowcasting** ([`tilekit::fov::shadowcast`]): symmetric,
//!   artifact-free line of sight computed in time proportional to the visible
//!   area rather than to the number of rays cast. See
//!   [RogueBasin's write-up](https://www.roguebasin.com/index.php/FOV_using_recursive_shadowcasting).
//! - **Three-state fog** ([`tilekit::fov::Visibility`]): unknown tiles are
//!   fully hidden, explored tiles show remembered terrain with no units or
//!   animation (memory is stale terrain, not a live camera), and visible
//!   tiles show everything. See Battle for Wesnoth's
//!   [vision model](https://www.wesnoth.org/devdocs/vision_8hpp_source.html)
//!   for the same three-state design in a shipped game.
//! - **Terrain-aware blocking**: mountains, peaks, and forest canopy block
//!   sight; everything else does not, so standing beside a mountain range
//!   casts a clean wedge-shaped shadow behind it -- the single most legible
//!   proof that the shadowcasting is actually correct.
//!
//! ```sh
//! cargo run --example 11_fog_of_war --features crossterm
//! cargo run --example 11_fog_of_war --features software
//! cargo run --example 11_fog_of_war --features gl
//! cargo run --example 11_fog_of_war  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, KeyModifiers, Rect, Style, Surface, Terminal};

use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::camera::TileCamera;
use tilekit::fov::{FogMap, Visibility, shadowcast};
use tilekit::geom::Cell;
use tilekit::glyphs::terrain;
use tilekit::noise::hash01;
use tilekit::palette::{self, hillshade_nw, mix, remembered, scale, unexplored};
use tilekit::world::{Biome, World};

/// World size in cells. Smaller than `01_terrain_cells`: the automatic
/// scout's patrol has to cover the whole road network in a demo-friendly
/// amount of time, and a huge map would make that take minutes.
const WORLD_W: i32 = 170;
/// See [`WORLD_W`].
const WORLD_H: i32 = 110;

const RELIEF: f32 = 55.0;

/// Sight radius, in cells, for both scouts.
///
/// Generous for a strategy game, and deliberately so: at a realistic radius
/// the demo opens on a nearly black screen with a coin-sized patch of terrain
/// in the middle, which reads as a broken page rather than as fog of war. A
/// wide radius makes the shadowcasting itself legible too, since a wedge cast
/// by a mountain range needs room to be seen as a wedge.
const SIGHT_RADIUS: i32 = 22;

/// How far the automatic scout is advanced before the first frame.
///
/// See [`FogOfWar::default`]: without a head start the map has no explored
/// (as opposed to currently visible) terrain at all, and the difference
/// between those two states is what the demo exists to show.
const PREWALK_STEPS: usize = 90;

/// Seconds between automatic-scout steps. Slow enough that the shadow the
/// scout casts around a mountain is easy to watch sweep as it moves, fast
/// enough that a full patrol finishes in well under a minute.
const AUTO_SCOUT_INTERVAL: f32 = 0.35;

/// Whether a biome blocks line of sight.
///
/// Relief only: mountains and peaks. Blocking on forest canopy as well sounds
/// more realistic and is much worse, because forest is *contiguous*. A scout
/// standing in woodland then sees two or three cells in every direction, the
/// shadowcasting has no room to cast a shadow of any interesting shape, and
/// the demo renders as a coin-sized dot on a black page. Terrain that blocks
/// sight has to be sparse relative to the sight radius or there is no field of
/// view left to look at, which is why strategy games put ridgelines in the way
/// and let you see over trees.
const fn blocks_sight(biome: Biome) -> bool {
    matches!(biome, Biome::Mountain | Biome::Peak)
}

/// State: the world, both scouts, the shared fog map, and view controls.
pub struct FogOfWar {
    world: World,
    camera: TileCamera,
    time: f32,
    cursor: Cell,
    fps: FpsMeter,
    fog: FogMap,

    /// Automatic scout: walks the road network road-cell by road-cell.
    auto_pos: Cell,
    auto_path: Vec<Cell>,
    auto_index: usize,
    auto_timer: f32,

    /// Player scout, moved with WASD/arrows.
    player_pos: Cell,

    reveal_all: bool,
}

impl Default for FogOfWar {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 23);
        let (sx, sy) = world.start_position();
        let mut camera =
            TileCamera::new(i32::from(GRID_COLS), i32::from(GRID_ROWS), WORLD_W, WORLD_H);
        camera.center_on(Cell::new(sx, sy));

        let auto_path = road_tour(&world);
        let auto_pos = auto_path
            .first()
            .copied()
            .unwrap_or_else(|| Cell::new(sx, sy));

        let mut demo = Self {
            fog: FogMap::new(world.width() as u16, world.height() as u16),
            world,
            camera,
            time: 0.0,
            cursor: Cell::new(sx, sy),
            fps: FpsMeter::new(),
            auto_pos,
            auto_path,
            auto_index: 0,
            auto_timer: 0.0,
            player_pos: Cell::new(sx, sy),
            reveal_all: false,
        };
        // Walk the auto scout a little way along its tour before handing the
        // demo over, so the map opens with a trail of remembered terrain
        // behind it rather than a single lit circle. The three visibility
        // states are the whole subject of this demo, and with no history there
        // are only two of them on screen.
        for _ in 0..PREWALK_STEPS {
            demo.step_auto_scout();
        }
        demo.recompute_fov();
        demo
    }
}

/// Builds a tour of every road cell, ordered by a depth-first walk from the
/// first road cell found. Not a shortest tour (that's a much harder problem
/// this demo has no need to solve) -- just a path that a scout can follow
/// step by step and eventually cover the whole network, doubling back along
/// already-visited cells rather than jumping.
fn road_tour(world: &World) -> Vec<Cell> {
    let start = (0..world.height())
        .flat_map(|y| (0..world.width()).map(move |x| (x, y)))
        .find(|&(x, y)| world.road_at(x, y));
    let Some(start) = start else {
        return vec![Cell::new(world.width() / 2, world.height() / 2)];
    };

    let mut visited = vec![false; (world.width() * world.height()) as usize];
    let mut path = Vec::new();
    let mut stack = vec![start];
    while let Some((x, y)) = stack.pop() {
        let Some(idx) = world.idx(x, y) else { continue };
        if visited[idx] {
            continue;
        }
        visited[idx] = true;
        path.push(Cell::new(x, y));
        for (nx, ny) in world.neighbors4(x, y) {
            if world.road_at(nx, ny)
                && let Some(nidx) = world.idx(nx, ny)
                && !visited[nidx]
            {
                stack.push((nx, ny));
            }
        }
    }
    path
}

impl FogOfWar {
    fn reroll(&mut self, delta: u32) {
        let seed = self.world.seed().wrapping_add(delta);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (sx, sy) = self.world.start_position();
        self.camera.center_on(Cell::new(sx, sy));
        self.cursor = Cell::new(sx, sy);
        self.player_pos = Cell::new(sx, sy);
        self.auto_path = road_tour(&self.world);
        self.auto_index = 0;
        self.auto_pos = self.auto_path.first().copied().unwrap_or(self.player_pos);
        self.auto_timer = 0.0;
        self.fog = FogMap::new(self.world.width() as u16, self.world.height() as u16);
        self.recompute_fov();
    }

    fn reset_exploration(&mut self) {
        self.fog.reset();
        self.reveal_all = false;
        self.recompute_fov();
    }

    /// Advances the automatic scout one step along its patrol, looping back
    /// to the start once it reaches the end so the demo keeps moving forever.
    fn step_auto_scout(&mut self) {
        if self.auto_path.is_empty() {
            return;
        }
        self.auto_index = (self.auto_index + 1) % self.auto_path.len();
        self.auto_pos = self.auto_path[self.auto_index];
    }

    /// Recomputes visibility from both scouts. Calling `begin_turn` once
    /// before revealing from either is what lets two independent sources
    /// contribute to a single turn's visibility: a tile stays `Visible` if
    /// *either* scout can currently see it, and only demotes to `Explored`
    /// once neither can.
    fn recompute_fov(&mut self) {
        self.fog.begin_turn();
        if self.reveal_all {
            self.fog.reveal_all();
            return;
        }
        for scout in [self.auto_pos, self.player_pos] {
            let world = &self.world;
            shadowcast(
                scout.x,
                scout.y,
                SIGHT_RADIUS,
                |x, y| !world.in_bounds(x, y) || blocks_sight(world.biome_at(x, y)),
                |x, y| self.fog.reveal(x, y),
            );
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        let mut moved = false;
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let pan_step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        10
                    } else {
                        2
                    };
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => {
                            self.player_pos = self.step_player(0, -1);
                            moved = true;
                        }
                        KeyCode::Down | KeyCode::Char('s' | 'S') => {
                            self.player_pos = self.step_player(0, 1);
                            moved = true;
                        }
                        KeyCode::Left | KeyCode::Char('a' | 'A') => {
                            self.player_pos = self.step_player(-1, 0);
                            moved = true;
                        }
                        KeyCode::Right | KeyCode::Char('d' | 'D') => {
                            self.player_pos = self.step_player(1, 0);
                            moved = true;
                        }
                        KeyCode::PageUp => self.camera.pan(0, -pan_step),
                        KeyCode::PageDown => self.camera.pan(0, pan_step),
                        KeyCode::Char('v' | 'V') => {
                            self.reveal_all = !self.reveal_all;
                            self.recompute_fov();
                        }
                        KeyCode::Char('x' | 'X') => self.reset_exploration(),
                        KeyCode::Char('r' | 'R') => self.reroll(1),
                        KeyCode::Home => self.camera.center_on(self.player_pos),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        if moved {
            self.camera.center_on(self.player_pos);
            self.recompute_fov();
        }
        true
    }

    /// Moves the player scout by `(dx, dy)` if the destination is passable,
    /// clamped to the map. Bumping into impassable terrain (a mountain, deep
    /// water) is a no-op rather than an error, matching how the automatic
    /// scout can never leave the road network in the first place.
    fn step_player(&self, dx: i32, dy: i32) -> Cell {
        let next = self.player_pos.offset(dx, dy);
        if self.world.in_bounds(next.x, next.y) && self.world.biome_at(next.x, next.y).is_passable()
        {
            next
        } else {
            self.player_pos
        }
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: retroglyph_core::Pos) {
        let screen = Cell::new(i32::from(pos.x), i32::from(pos.y) - 1);
        match kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                self.cursor = self.camera.screen_to_world(screen);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let world = self.camera.screen_to_world(screen);
                let (dx, dy) = (self.cursor.x - world.x, self.cursor.y - world.y);
                self.camera.pan(dx, dy);
            }
            MouseEventKind::ScrollUp => self.camera.pan(0, -3),
            MouseEventKind::ScrollDown => self.camera.pan(0, 3),
            _ => {}
        }
    }

    /// Full-color terrain, exactly as `01_terrain_cells` renders it. Only
    /// called for [`Visibility::Visible`] cells.
    fn render_visible(&self, x: i32, y: i32) -> (char, Color, Color) {
        let biome = self.world.biome_at(x, y);
        let mut color = biome.color();
        let mut glyph = if hash01(0x9E37_79B9, x, y) < biome_density(biome) {
            biome.glyph()
        } else {
            ' '
        };

        if self.world.river_at(x, y) {
            glyph = terrain::WAVE;
            color = palette::rgb(96, 156, 214);
        } else if self.world.road_at(x, y) {
            glyph = '\u{00b7}';
            color = palette::rgb(214, 196, 156);
        }

        let mut shade = 1.0;
        if biome.is_water() {
            let phase = self
                .time
                .mul_add(1.4, (x as f32).mul_add(0.55, y as f32 * 0.31));
            let swell = phase.sin().mul_add(0.5, 0.5);
            glyph = if swell > 0.80 { terrain::WAVE } else { ' ' };
            color = mix(color, palette::WHITE, swell * 0.16);
        } else {
            let (slope_x, slope_y) = self.world.gradient_at(x, y, RELIEF);
            shade = hillshade_nw(slope_x, slope_y).mul_add(0.85, 0.45);
            color = scale(color, shade);
        }

        let bg = scale(mix(biome.color(), ui::BG, 0.68), shade);
        (glyph, color, bg)
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.camera
            .set_viewport(i32::from(area.width()), i32::from(area.height()));
        let (left, top, right, bottom) = self.camera.visible_cells();

        for wy in top..=bottom {
            for wx in left..=right {
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);

                let visibility = self.fog.get(wx, wy);
                let (mut glyph, mut fg, mut bg) = match visibility {
                    Visibility::Unknown => (' ', ui::BG, unexplored(ui::BG)),
                    Visibility::Explored => {
                        let (g, f, b) = self.render_visible(wx, wy);
                        (g, remembered(f, ui::BG), remembered(b, ui::BG))
                    }
                    Visibility::Visible => self.render_visible(wx, wy),
                };

                // Units and landmarks only ever draw on currently-visible
                // tiles: this is the entire point of the three-state model.
                // An explored tile shows the terrain as last seen, not a
                // live camera, so a unit that has since moved away must not
                // still appear to be standing there.
                if visibility.shows_units() {
                    if let Some(landmark) = self.world.landmark_at(wx, wy) {
                        let (marker, marker_color) = landmark.site.glyph_color();
                        glyph = marker;
                        fg = marker_color;
                    }
                    if Cell::new(wx, wy) == self.auto_pos {
                        glyph = '\u{2691}';
                        fg = palette::rgb(140, 200, 255);
                    }
                    if Cell::new(wx, wy) == self.player_pos {
                        glyph = '\u{263a}';
                        fg = palette::rgb(255, 220, 130);
                    }
                }

                if wx == self.cursor.x && wy == self.cursor.y {
                    bg = mix(bg, palette::rgb(255, 236, 170), 0.35);
                }
                surface.put((sx, sy), glyph, Style::new().fg(fg).bg(bg));
            }
        }
    }

    fn status(&self) -> String {
        let visibility = self.fog.get(self.cursor.x, self.cursor.y);
        let vis_label = match visibility {
            Visibility::Unknown => "unknown",
            Visibility::Explored => "explored (remembered)",
            Visibility::Visible => "visible",
        };
        format!(
            "({}, {})  {vis_label}  explored {:.0}%  scouts at ({},{}) auto / ({},{}) you  reveal-all: {}",
            self.cursor.x,
            self.cursor.y,
            self.fog.explored_fraction() * 100.0,
            self.auto_pos.x,
            self.auto_pos.y,
            self.player_pos.x,
            self.player_pos.y,
            if self.reveal_all { "on" } else { "off" },
        )
    }
}

const fn biome_density(biome: Biome) -> f32 {
    match biome {
        Biome::Mountain | Biome::Peak | Biome::Jungle => 0.92,
        Biome::Forest | Biome::Taiga => 0.80,
        Biome::Ice => 0.30,
        Biome::Grassland | Biome::Savanna => 0.26,
        Biome::Marsh | Biome::Tundra => 0.34,
        Biome::Desert | Biome::Scrubland => 0.18,
        Biome::Coast => 0.22,
        _ => 0.0,
    }
}

impl Demo for FogOfWar {
    const NAME: &'static str = "11_fog_of_war";
    const TITLE: &'static str = "11 Fog of war";
    const BLURB: &'static str = "Shadowcasting FOV feeding a three-state exploration memory.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move your scout"),
            ("PgUp/PgDn", "pan"),
            ("V", "reveal all"),
            ("X", "reset fog"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        self.auto_timer += frame.delta.as_secs_f32();
        if self.auto_timer >= AUTO_SCOUT_INTERVAL {
            self.auto_timer -= AUTO_SCOUT_INTERVAL;
            self.step_auto_scout();
            self.recompute_fov();
        }

        let (title, content, status) = ui::split_chrome(term.area());

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_map(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(FogOfWar);
