//! 15: Minimal -- flat color fields, no glyph detail.
//!
//! The antithesis of the ASCII-art demos in this gallery: every cell is a
//! space with a background color, nothing else. A character grid is, among
//! other things, a low-resolution pixel canvas, and this demo takes that
//! literally: no texture, no shading gradient, just crisp flat regions with a
//! one-cell darker outline where biomes meet. The result reads closer to a
//! modern strategy game's minimap or a vector political map than to a
//! roguelike.
//!
//! Techniques on show:
//!
//! - **Flat color fill**: every world cell is a single background color, no
//!   glyph. Proves the character grid works as a pure raster surface, not
//!   only as a text display.
//! - **Boundary tracing**: a cell is outlined iff its north or west neighbour
//!   differs (by whichever criterion the current mode uses), giving crisp
//!   one-cell borders with no separate edge-detection pass over the whole
//!   grid -- the same trick [`01_terrain_cells`](../01_terrain_cells) uses for
//!   its grid overlay.
//! - **Multiple flat encodings of the same data**: biome color, a two-tone
//!   land/water silhouette, a quantized [`tilekit::palette::ELEVATION`] ramp,
//!   and a province/political fill, cycled with one key. Same world, four
//!   readings of it.
//! - **A day/night terminator**: a soft north-south light band sweeps slowly
//!   across the map, darkening whatever it has passed, independent of mode.
//!
//! ```sh
//! cargo run --example 15_minimal --features crossterm
//! cargo run --example 15_minimal --features software
//! cargo run --example 15_minimal --features gl
//! cargo run --example 15_minimal  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Style, Terminal};

use ascii_tile_demos::ui::{self, PrintStr};
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::palette::{self, faction, mix, scale};
use tilekit::world::World;

/// World size in cells.
const WORLD_W: i32 = 220;
/// See [`WORLD_W`].
const WORLD_H: i32 = 140;

/// How many world-seconds the terminator takes to sweep fully across the map
/// and wrap back to the start. Slow enough to read as "time passing", not
/// fast enough to look like a strobing bug.
const TERMINATOR_PERIOD: f32 = 40.0;

/// One of the flat encodings `M` cycles through.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Biome,
    Silhouette,
    Elevation,
    Political,
}

impl Mode {
    const fn next(self) -> Self {
        match self {
            Self::Biome => Self::Silhouette,
            Self::Silhouette => Self::Elevation,
            Self::Elevation => Self::Political,
            Self::Political => Self::Biome,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Biome => "biome fill",
            Self::Silhouette => "land/water silhouette",
            Self::Elevation => "elevation ramp",
            Self::Political => "political fill",
        }
    }
}

/// State: the world, camera, active flat mode, and the terminator's phase.
pub struct Minimal {
    world: World,
    camera: TileCamera,
    mode: Mode,
    time: f32,
    fps: FpsMeter,
    cursor: Cell,
}

impl Default for Minimal {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 3);
        let (sx, sy) = world.start_position();
        let mut camera =
            TileCamera::new(i32::from(GRID_COLS), i32::from(GRID_ROWS), WORLD_W, WORLD_H);
        camera.center_on(Cell::new(sx, sy));
        Self {
            world,
            camera,
            mode: Mode::default(),
            time: 0.0,
            fps: FpsMeter::new(),
            cursor: Cell::new(sx, sy),
        }
    }
}

impl Minimal {
    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (sx, sy) = self.world.start_position();
        self.camera.center_on(Cell::new(sx, sy));
        self.cursor = Cell::new(sx, sy);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let step = if key.modifiers.contains(retroglyph_core::KeyModifiers::SHIFT) {
                        10
                    } else {
                        2
                    };
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => self.camera.pan(0, -step),
                        KeyCode::Down | KeyCode::Char('s' | 'S') => self.camera.pan(0, step),
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.camera.pan(-step, 0),
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.camera.pan(step, 0),
                        KeyCode::Char('m' | 'M') => self.mode = self.mode.next(),
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Home => {
                            let (sx, sy) = self.world.start_position();
                            self.camera.center_on(Cell::new(sx, sy));
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
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

    /// The base fill color for `(x, y)` under the active mode, before the
    /// terminator and cursor highlight are applied.
    fn fill_color(&self, x: i32, y: i32) -> Color {
        let biome = self.world.biome_at(x, y);
        match self.mode {
            Mode::Biome => biome.color(),
            Mode::Silhouette => {
                if biome.is_water() {
                    palette::rgb(18, 34, 64)
                } else {
                    palette::rgb(214, 208, 188)
                }
            }
            Mode::Elevation => {
                let e = self.world.elevation_at(x, y);
                palette::ELEVATION.sample(e)
            }
            Mode::Political => {
                if biome.is_water() {
                    palette::rgb(14, 22, 40)
                } else {
                    let province = self.world.province_at(x, y);
                    scale(faction(province), 0.85)
                }
            }
        }
    }

    /// What counts as "the same region" for the current mode's border trace.
    /// Two adjacent cells with different keys get a border between them.
    fn region_key(&self, x: i32, y: i32) -> u32 {
        let biome = self.world.biome_at(x, y);
        match self.mode {
            Mode::Biome => biome as u32,
            Mode::Silhouette => u32::from(biome.is_water()),
            Mode::Elevation => {
                // Bucket into the same ten bands ELEVATION's own stops fall
                // into, so the traced border lines up with where the ramp's
                // color actually changes rather than firing every cell.
                (self.world.elevation_at(x, y) * 10.0) as u32
            }
            Mode::Political => {
                if biome.is_water() {
                    u32::MAX
                } else {
                    self.world.province_at(x, y) as u32
                }
            }
        }
    }

    /// Terminator brightness multiplier at `(x, y)`: a soft band that sweeps
    /// left to right and wraps, independent of the active mode. `1.0` is
    /// full daylight, near `0.35` is the shadowed side.
    fn terminator_factor(&self, x: i32) -> f32 {
        let phase = (self.time / TERMINATOR_PERIOD).fract();
        let sun_x = phase * WORLD_W as f32;
        // Wrapped distance to the sun's position, so the band does not jump
        // when it crosses the map edge.
        let raw = (x as f32 - sun_x).rem_euclid(WORLD_W as f32);
        let dist = raw.min(WORLD_W as f32 - raw);
        // A wide soft falloff (a third of the map) rather than a hard line,
        // so the effect reads as a slow gradient sweep, not a moving wall.
        let half_width = WORLD_W as f32 / 3.0;
        let t = (dist / half_width).clamp(0.0, 1.0);
        // Smoothstep-shaped rather than linear, so the darkest and lightest
        // bands each hold their value for a while instead of the whole map
        // being mid-transition at once.
        let eased = t * t * 2.0f32.mul_add(-t, 3.0);
        0.65f32.mul_add(eased, 0.35)
    }

    fn draw_map<B: Backend>(&mut self, term: &mut Terminal<B>, area: retroglyph_core::Rect) {
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

                let mut color = self.fill_color(wx, wy);
                color = scale(color, self.terminator_factor(wx));

                let border = self.region_key(wx - 1, wy) != self.region_key(wx, wy)
                    || self.region_key(wx, wy - 1) != self.region_key(wx, wy);
                if border {
                    color = scale(color, 0.55);
                }

                if self.world.river_at(wx, wy) && self.mode != Mode::Political {
                    color = mix(color, palette::rgb(120, 176, 224), 0.75);
                } else if self.world.road_at(wx, wy) && self.mode == Mode::Biome {
                    color = mix(color, palette::rgb(224, 208, 168), 0.6);
                }

                if let Some(landmark) = self.world.landmark_at(wx, wy)
                    && landmark.site.is_settlement()
                {
                    color = palette::WHITE;
                }

                if wx == self.cursor.x && wy == self.cursor.y {
                    color = mix(color, palette::rgb(255, 236, 170), 0.5);
                }

                term.put_styled(sx, sy, ' ', Style::new().bg(color));
            }
        }
    }

    fn status(&self) -> String {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let biome = self.world.biome_at(x, y);
        format!(
            "({x}, {y})  {}  mode: {}  seed {}",
            biome.name(),
            self.mode.label(),
            self.world.seed()
        )
    }
}

impl Demo for Minimal {
    const NAME: &'static str = "15_minimal";
    const TITLE: &'static str = "15 Minimal";
    const BLURB: &'static str = "Flat color fields with crisp borders; no glyph detail at all.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("M", "cycle mode"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        ui::fill(term, content, Style::new().bg(ui::BG));
        self.draw_map(term, content);
        ui::title_bar::<B, Self>(term, title);
        let text = self.status();
        ui::status_bar::<B, Self>(term, status, &text, &self.fps);
        let _ = |t: &mut Terminal<B>| t.print_styled_str(0, 0, "", Style::new());

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(Minimal);
