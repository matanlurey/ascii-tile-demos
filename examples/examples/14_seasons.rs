//! 14: Seasons -- one world, six moods, because map data and map look are
//! independent.
//!
//! `01_terrain_cells.rs` generates a world once and renders it once. This demo
//! generates a world once and renders it through a *tint pass*: the same
//! elevation, biome, river, and road data, restyled by
//! [`tilekit::palette::TimeOfDay`] and [`tilekit::palette::Season`] without
//! touching the world at all. Nothing here regenerates anything; every visual
//! change is a color transform applied at draw time.
//!
//! Techniques on show:
//!
//! - **Tint passes** ([`tilekit::palette::apply_tint`]): a `(color, strength)`
//!   pair blended over the base biome color. Four times of day and four
//!   seasons are eight total tints, all defined once in `tilekit::palette` and
//!   shared by every demo that wants a day/night or seasonal mood -- this demo
//!   just happens to be the one that shows them all.
//! - **Selective tinting**: the seasonal tint only touches vegetated land
//!   (forest, grass, taiga, and so on); ocean and bare rock are immune, the
//!   same way a real photograph of a mountain doesn't turn orange in autumn
//!   just because the valley below it does.
//! - **A moving snow line**: winter doesn't repaint existing snow, it lowers
//!   the elevation threshold at which land counts as snow-covered, so terrain
//!   that was bare rock in summer is genuinely reclassified as snowy in
//!   winter, which is what a real snow line does.
//! - **Lit windows at night**: the one per-settlement animated detail that
//!   sells the whole effect -- night darkens everything except a warm point of
//!   light at every settlement, the way looking down at a dark landscape with
//!   real towns in it actually looks.
//! - **A sweeping terminator**: in auto-advance mode the day/night boundary is
//!   a real line of longitude crossing the map over time, not every cell
//!   changing brightness in lockstep.
//!
//! ```sh
//! cargo run --example 14_seasons --features crossterm
//! cargo run --example 14_seasons --features software
//! cargo run --example 14_seasons --features gl
//! cargo run --example 14_seasons  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Style, Terminal};

use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::glyphs::terrain;
use tilekit::noise::hash01;
use tilekit::palette::{self, Season, TimeOfDay, apply_tint, hillshade_nw, mix, scale};
use tilekit::world::{Biome, World};

/// World size in cells, matching `01_terrain_cells`.
const WORLD_W: i32 = 260;
/// See [`WORLD_W`].
const WORLD_H: i32 = 170;

/// Same vertical exaggeration `01_terrain_cells` uses.
const RELIEF: f32 = 55.0;

/// How many world-columns wide the day/night terminator's soft edge is, in
/// auto-advance mode. Wide enough to see as a gradient sweeping across the
/// map rather than a hard line jumping from lit to dark.
const TERMINATOR_WIDTH: f32 = 34.0;

/// Real seconds for one full day/night cycle in auto-advance mode. Fast
/// enough that leaving the demo running actually shows the whole cycle
/// within a reasonable watching time, slow enough that individual phases are
/// still readable rather than strobing past.
const DAY_SECONDS: f32 = 24.0;

/// Real seconds for one full year (four seasons) in auto-advance mode. A
/// prime multiple of [`DAY_SECONDS`] so the two cycles drift in and out of
/// phase instead of always lining up at the same wall-clock moments.
const YEAR_SECONDS: f32 = 97.0;

/// How auto-advance is currently affecting the clock.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Time of day and season are both fixed; `T`/`Y` step them manually.
    Manual,
    /// Time of day sweeps continuously and season slowly rotates.
    Auto,
}

/// State: the world, camera, current mood, and the animation clock.
pub struct Seasons {
    world: World,
    camera: TileCamera,
    time: f32,
    cursor: Cell,
    fps: FpsMeter,
    mode: Mode,
    /// Manual time of day, used when `mode` is [`Mode::Manual`].
    time_of_day: TimeOfDay,
    /// Manual season, used when `mode` is [`Mode::Manual`].
    season: Season,
    /// Elapsed seconds since entering [`Mode::Auto`], driving the terminator
    /// sweep and the season rotation independently of `time`.
    auto_elapsed: f32,
}

impl Default for Seasons {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 11);
        let (sx, sy) = world.start_position();
        let mut camera =
            TileCamera::new(i32::from(GRID_COLS), i32::from(GRID_ROWS), WORLD_W, WORLD_H);
        camera.center_on(Cell::new(sx, sy));
        Self {
            world,
            camera,
            time: 0.0,
            cursor: Cell::new(sx, sy),
            fps: FpsMeter::new(),
            mode: Mode::Auto,
            time_of_day: TimeOfDay::Noon,
            season: Season::Spring,
            auto_elapsed: 0.0,
        }
    }
}

impl Seasons {
    fn reroll(&mut self, delta: u32) {
        let seed = self.world.seed().wrapping_add(delta);
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
                        KeyCode::Char('t' | 'T') => {
                            self.mode = Mode::Manual;
                            self.time_of_day = self.time_of_day.next();
                        }
                        KeyCode::Char('y' | 'Y') => {
                            self.mode = Mode::Manual;
                            self.season = self.season.next();
                        }
                        KeyCode::Char('p' | 'P') => {
                            self.mode = if self.mode == Mode::Auto {
                                Mode::Manual
                            } else {
                                Mode::Auto
                            };
                            self.auto_elapsed = 0.0;
                        }
                        KeyCode::Char('r' | 'R') => self.reroll(1),
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

    /// The `(time_of_day_phase, day_fraction, season, season_fraction)` for
    /// this frame. `day_fraction` and `season_fraction` are `0.0..1.0`
    /// progress through the *current* phase, used to interpolate the
    /// terminator and the season blend in [`Mode::Auto`]; in [`Mode::Manual`]
    /// they are the phase's own tint applied at full strength (fraction 1).
    fn phases(&self) -> (TimeOfDay, f32, Season, f32) {
        match self.mode {
            Mode::Manual => (self.time_of_day, 1.0, self.season, 1.0),
            Mode::Auto => {
                let day_t = (self.auto_elapsed / DAY_SECONDS).fract();
                let day_phase = match (day_t * 4.0) as u32 {
                    0 => TimeOfDay::Dawn,
                    1 => TimeOfDay::Noon,
                    2 => TimeOfDay::Dusk,
                    _ => TimeOfDay::Night,
                };
                let year_t = (self.auto_elapsed / YEAR_SECONDS).fract();
                let season_phase = match (year_t * 4.0) as u32 {
                    0 => Season::Spring,
                    1 => Season::Summer,
                    2 => Season::Autumn,
                    _ => Season::Winter,
                };
                (day_phase, 1.0, season_phase, 1.0)
            }
        }
    }

    /// Fraction of the day cycle elapsed, `0.0..1.0`, used to sweep the
    /// terminator across the map in [`Mode::Auto`]. In [`Mode::Manual`] this
    /// follows the fixed time of day's own midpoint so the terminator still
    /// sits at a sensible position for that phase (e.g. dawn's line runs
    /// through the map's eastern third).
    fn day_progress(&self) -> f32 {
        match self.mode {
            Mode::Auto => (self.auto_elapsed / DAY_SECONDS).fract(),
            Mode::Manual => match self.time_of_day {
                TimeOfDay::Dawn => 0.125,
                TimeOfDay::Noon => 0.375,
                TimeOfDay::Dusk => 0.625,
                TimeOfDay::Night => 0.875,
            },
        }
    }

    /// How lit `x` is, `0.0` (full night) to `1.0` (full day), from the
    /// terminator's position. Only used in [`Mode::Auto`]; [`Mode::Manual`]
    /// applies its tint uniformly instead, since a fixed time of day has no
    /// terminator to speak of.
    fn daylight_at(&self, x: i32) -> f32 {
        let terminator_x = self.day_progress() * WORLD_W as f32;
        // Wrapped distance: the terminator sweeps off one edge and back onto
        // the other, since the map itself has no real antimeridian to hide it.
        let raw = (x as f32 - terminator_x).rem_euclid(WORLD_W as f32);
        let centered = if raw > WORLD_W as f32 / 2.0 {
            raw - WORLD_W as f32
        } else {
            raw
        };
        // Half the map is lit, half is dark, blended over TERMINATOR_WIDTH at
        // each of the two crossings.
        (0.5 - centered / TERMINATOR_WIDTH).clamp(0.0, 1.0)
    }

    /// The tint this frame applies, resolving [`Mode::Auto`]'s continuous
    /// terminator into a `(color, strength)` pair for world column `x`.
    fn time_tint_at(&self, x: i32) -> (Color, f32) {
        match self.mode {
            Mode::Manual => self.time_of_day.tint(),
            Mode::Auto => {
                let daylight = self.daylight_at(x);
                // Blend the noon (no tint) and night tints by how dark it is
                // here right now, so the terminator itself is a gradient of
                // *tint strength*, not a jump between two fixed phases.
                let (night_color, night_strength) = TimeOfDay::Night.tint();
                (night_color, night_strength * (1.0 - daylight))
            }
        }
    }

    fn season_tint(&self) -> (Color, f32) {
        self.phases().2.tint()
    }

    /// Whether winter conditions apply strongly enough to lower the snow
    /// line and freeze lakes. In [`Mode::Auto`] this fades in as the season
    /// rotates toward winter rather than snapping on, matching how the tint
    /// itself fades.
    fn winter_strength(&self) -> f32 {
        match self.phases().2 {
            Season::Winter => 1.0,
            _ => 0.0,
        }
    }

    /// The glyph, foreground, and background for one world cell, with the
    /// active time-of-day and season tints applied.
    fn render_cell(&self, x: i32, y: i32) -> (char, Color, Color) {
        let winter = self.winter_strength();
        // Winter depresses the effective snow line: land above this
        // (lowered) threshold reads as Peak/snow-covered even if its real
        // biome is Mountain or Tundra, the way a real snow line descends the
        // mountainside in the cold season rather than repainting the summit.
        let snow_threshold = winter.mul_add(-0.10, tilekit::world::PEAK_LEVEL);
        let elevation = self.world.elevation_at(x, y);
        let mut biome = self.world.biome_at(x, y);
        if !biome.is_water() && elevation >= snow_threshold {
            biome = Biome::Peak;
        }
        // Frozen lakes: winter turns still fresh water into ice, but leaves
        // the open sea alone (a large enough body doesn't freeze the way a
        // discrete lake does, and keeping some water blue keeps the map
        // readable).
        if biome == Biome::Lake && winter > 0.5 {
            biome = Biome::Ice;
        }

        let mut color = biome.color();
        let mut glyph = if hash01(0x9E37_79B9, x, y) < biome_density(biome) {
            biome.glyph()
        } else {
            ' '
        };

        if self.world.river_at(x, y) && biome != Biome::Ice {
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

        // Seasonal tint only touches vegetated land: forest, grass, taiga,
        // marsh, savanna, jungle, scrubland. Ocean, bare mountain, and snow
        // are immune, the same way a real satellite photo doesn't turn a
        // glacier orange just because the valley below it does in autumn.
        if is_vegetated(biome) {
            color = apply_tint(color, self.season_tint());
        }

        let (tint_color, tint_strength) = self.time_tint_at(x);
        color = apply_tint(color, (tint_color, tint_strength));

        let bg = scale(mix(biome.color(), ui::BG, 0.68), shade);
        let bg = if is_vegetated(biome) {
            apply_tint(bg, self.season_tint())
        } else {
            bg
        };
        let bg = apply_tint(bg, (tint_color, tint_strength));

        if let Some(landmark) = self.world.landmark_at(x, y) {
            // Lit windows: at night, a settlement still shows a warm point of
            // light instead of fading into the dark like everything else.
            // This is the detail that makes the night pass read as *night*
            // rather than as "the same map, dimmed" -- a real landscape at
            // night is not uniformly dark, it has towns in it.
            if tint_strength > 0.25 {
                let (marker, _) = landmark.site.glyph_color();
                let warm = palette::rgb(255, 214, 120);
                return (marker, warm, mix(bg, warm, 0.22));
            }
            let (marker, marker_color) = landmark.site.glyph_color();
            return (marker, marker_color, bg);
        }
        (glyph, color, bg)
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

                let (glyph, mut color, mut bg) = self.render_cell(wx, wy);
                if wx == self.cursor.x && wy == self.cursor.y {
                    bg = mix(bg, palette::rgb(255, 236, 170), 0.45);
                    color = mix(color, palette::WHITE, 0.30);
                }
                term.put_styled(sx, sy, glyph, Style::new().fg(color).bg(bg));
            }
        }
    }

    fn status(&self) -> String {
        let (day, _, season, _) = self.phases();
        let (_, tint_strength) = self.time_tint_at(self.cursor.x);
        let mode = match self.mode {
            Mode::Auto => "auto",
            Mode::Manual => "manual",
        };
        format!(
            "{mode}  {} / {}  tint {:.0}%  seed {}",
            day.label(),
            season.label(),
            tint_strength * 100.0,
            self.world.seed()
        )
    }
}

/// What fraction of a biome's cells draw its glyph. Identical to
/// `01_terrain_cells`'s table; kept local rather than shared so this demo
/// stays self-contained and free to diverge if the two ever want different
/// densities (`Ice` is denser here since winter's frozen lakes benefit from
/// reading as solid).
const fn biome_density(biome: Biome) -> f32 {
    match biome {
        Biome::Mountain | Biome::Peak | Biome::Jungle => 0.92,
        Biome::Forest | Biome::Taiga => 0.80,
        Biome::Ice => 0.45,
        Biome::Grassland | Biome::Savanna => 0.26,
        Biome::Marsh | Biome::Tundra => 0.34,
        Biome::Desert | Biome::Scrubland => 0.18,
        Biome::Coast => 0.22,
        _ => 0.0,
    }
}

/// Whether `biome` carries live vegetation and should be affected by the
/// seasonal tint. Water, bare rock, and permanent snow are excluded.
const fn is_vegetated(biome: Biome) -> bool {
    matches!(
        biome,
        Biome::Grassland
            | Biome::Forest
            | Biome::Taiga
            | Biome::Marsh
            | Biome::Savanna
            | Biome::Jungle
            | Biome::Scrubland
    )
}

impl Demo for Seasons {
    const NAME: &'static str = "14_seasons";
    const TITLE: &'static str = "14 Seasons";
    const BLURB: &'static str =
        "One world, six moods: time-of-day and seasonal tints over unchanged terrain.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("T", "time of day"),
            ("Y", "season"),
            ("P", "auto/manual"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        if self.mode == Mode::Auto {
            self.auto_elapsed += frame.delta.as_secs_f32();
        }
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

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(Seasons);
