//! 12: Relief -- heightmap cartography, the physical-map counterpart to
//! `10_political`'s political map.
//!
//! Where `01_terrain_cells` colors by biome, this demo colors by elevation
//! alone, the way a physical wall map or a GIS hillshade product does. Four
//! independently toggleable layers combine: a hypsometric tint ramp, a
//! northwest-lit hillshade, elevation contour lines, and a slope-shading
//! mode. A fifth mode swaps the hillshade out for ordered dithering, to show
//! why dithering beats naive posterization on a smooth gradient with only a
//! handful of glyphs to work with.
//!
//! Techniques on show:
//!
//! - **Hypsometric tinting** ([`tilekit::palette::ELEVATION`]): a color ramp
//!   keyed to elevation rather than biome, with a deliberately sharp stop at
//!   sea level so the coastline reads as a line rather than a gradient.
//! - **Lambertian hillshading** ([`tilekit::palette::hillshade`]): shade from
//!   the dot product of the surface normal and a light direction. The sun's
//!   azimuth is adjustable (and can orbit automatically), which is the
//!   clearest possible demonstration of the northwest-lighting convention:
//!   light relief from the south instead and every hill visually inverts into
//!   a crater -- the illusion cartographers have designed around for a
//!   century. See [gdaldem hillshade](https://gdal.org/en/latest/programs/gdaldem.html)
//!   for the same model as production GIS tooling.
//! - **Elevation contour lines**: drawn wherever a cell's elevation band
//!   differs from its north or west neighbor's, using
//!   [`tilekit::autotile::marching_glyph`] to pick a line glyph oriented to
//!   the local boundary shape rather than a single flat marker everywhere.
//!   See [Catlike Coding's marching squares](https://catlikecoding.com/unity/tutorials/marching-squares/).
//! - **Ordered dithering** ([`tilekit::glyphs::dithered_glyph`]): a Bayer
//!   matrix breaks a smooth gradient into a stable crosshatch instead of
//!   visible posterization bands, and unlike error diffusion the pattern is a
//!   pure function of position, so it doesn't crawl as the map pans.
//!
//! ```sh
//! cargo run --example 12_relief --features crossterm
//! cargo run --example 12_relief --features software
//! cargo run --example 12_relief --features gl
//! cargo run --example 12_relief  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, KeyModifiers, Rect, Style, Surface, Terminal};

use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::autotile::{DualGrid, marching_glyph};
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::glyphs::{SHADE, dithered_glyph};
use tilekit::palette::{self, ELEVATION, hillshade, mix, scale};
use tilekit::world::World;

const WORLD_W: i32 = 220;
const WORLD_H: i32 = 140;

/// Vertical exaggeration. Larger than `01_terrain_cells`'s 55: this demo's
/// hillshade is the entire image rather than a modulation on top of biome
/// color, so it needs more contrast to read at a glance.
const RELIEF: f32 = 45.0;

/// Screen columns per world cell.
///
/// A character cell is about twice as tall as it is wide, so two columns by
/// one row is the closest a character grid gets to a square world cell. See
/// [`Relief::draw_map`] for why this demo pays the horizontal resolution for
/// it when the others do not.
const CELL_SPAN: i32 = 2;

/// Elevation band width for contour lines. At `0.05` a full 0..1 elevation
/// range draws 20 contours, which on a few-hundred-cell map gives visibly
/// spaced rings around peaks without the interior turning into a solid mass
/// of lines.
const CONTOUR_STEP: f32 = 0.05;

/// Degrees per second the sun orbits when auto-rotation is on. A full
/// rotation in 40 seconds is slow enough to watch the relief-inversion point
/// (light from due south) arrive and pass without the image flickering.
const ORBIT_DEGREES_PER_SEC: f32 = 9.0;

/// Which layer combination is active. `T` cycles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Hypsometric tint alone, no shading: the flattest, most map-like view.
    Tint,
    /// Tint plus hillshade: the standard relief-map look.
    Shaded,
    /// Tint, hillshade, and contour lines.
    Contours,
    /// Slope steepness only, grayscale: steep is bright, flat is dark.
    Slope,
    /// Grayscale elevation via ordered dithering instead of the smooth tint
    /// ramp, to contrast against [`Self::Tint`].
    Dithered,
}

impl Mode {
    const fn next(self) -> Self {
        match self {
            Self::Tint => Self::Shaded,
            Self::Shaded => Self::Contours,
            Self::Contours => Self::Slope,
            Self::Slope => Self::Dithered,
            Self::Dithered => Self::Tint,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Tint => "tint only",
            Self::Shaded => "tint + hillshade",
            Self::Contours => "tint + hillshade + contours",
            Self::Slope => "slope shading",
            Self::Dithered => "dithered grayscale",
        }
    }
}

/// State: the world, camera, sun position, and view mode.
pub struct Relief {
    world: World,
    camera: TileCamera,
    cursor: Cell,
    fps: FpsMeter,
    mode: Mode,
    /// Sun bearing, radians clockwise from north. `[` / `]` adjust it by
    /// hand; `O` toggles automatic orbiting.
    azimuth: f32,
    orbit: bool,
}

impl Default for Relief {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 5);
        let (sx, sy) = world.start_position();
        let mut camera =
            TileCamera::new(i32::from(GRID_COLS), i32::from(GRID_ROWS), WORLD_W, WORLD_H);
        camera.center_on(Cell::new(sx, sy));
        Self {
            world,
            camera,
            cursor: Cell::new(sx, sy),
            fps: FpsMeter::new(),
            mode: Mode::Shaded,
            azimuth: palette::SUN_NW,
            orbit: false,
        }
    }
}

impl Relief {
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
                    let step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        10
                    } else {
                        2
                    };
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => self.camera.pan(0, -step),
                        KeyCode::Down | KeyCode::Char('s' | 'S') => self.camera.pan(0, step),
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.camera.pan(-step, 0),
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.camera.pan(step, 0),
                        KeyCode::Char('t' | 'T') => self.mode = self.mode.next(),
                        KeyCode::Char('o' | 'O') => self.orbit = !self.orbit,
                        KeyCode::Char('[') => {
                            self.orbit = false;
                            self.azimuth -= 0.2;
                        }
                        KeyCode::Char(']') => {
                            self.orbit = false;
                            self.azimuth += 0.2;
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
        // Divide the column back out by `CELL_SPAN`, or picking lands two
        // world cells to the east of the pointer: drawing and picking have to
        // agree on one transform, and this demo's is not one-to-one.
        let screen = Cell::new(i32::from(pos.x) / CELL_SPAN, i32::from(pos.y) - 1);
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

    /// Elevation band index for contour comparisons. Cells on either side of
    /// a `CONTOUR_STEP` multiple fall into different bands, which is the
    /// condition a contour line is drawn on.
    fn band(&self, x: i32, y: i32) -> i32 {
        (self.world.elevation_at(x, y) / CONTOUR_STEP) as i32
    }

    /// Whether a contour line crosses between `(x, y)` and its north or west
    /// neighbor. Checking only those two (as `10_political`'s province
    /// borders do) means each line segment is drawn from exactly one side,
    /// giving a one-cell-thick line instead of a doubled one.
    fn is_contour(&self, x: i32, y: i32) -> bool {
        if !self.world.in_bounds(x, y) {
            return false;
        }
        let here = self.band(x, y);
        (self.world.in_bounds(x, y - 1) && self.band(x, y - 1) != here)
            || (self.world.in_bounds(x - 1, y) && self.band(x - 1, y) != here)
    }

    /// A marching-squares case for the 2x2 block of contour bands anchored at
    /// `(x, y)`, used to orient the contour glyph to the local boundary shape
    /// rather than drawing a flat line character everywhere.
    fn contour_case(&self, x: i32, y: i32) -> u8 {
        let here = self.band(x, y);
        let block = [
            self.band(x, y) == here,
            self.band(x + 1, y) == here,
            self.band(x, y + 1) == here,
            self.band(x + 1, y + 1) == here,
        ];
        DualGrid::corner_mask(block)
    }

    /// Glyph and color for one cell in the current [`Mode`].
    fn render_cell(&self, x: i32, y: i32) -> (char, Color, Color) {
        let elevation = self.world.elevation_at(x, y);
        let (slope_x, slope_y) = self.world.gradient_at(x, y, RELIEF);

        match self.mode {
            Mode::Tint => {
                let color = ELEVATION.sample(elevation);
                (' ', color, color)
            }
            Mode::Shaded | Mode::Contours => {
                let base = ELEVATION.sample(elevation);
                // The y slope is divided by the cell aspect for the same
                // reason `palette::hillshade_cells` does it: a character cell
                // is about twice as tall as it is wide, so an uncorrected
                // north-south gradient is lit as though it were twice as
                // steep, and the map develops vertical streaks. This demo
                // cannot use `hillshade_cells` directly because it rotates the
                // sun, so it applies the same correction by hand.
                let shade = hillshade(
                    slope_x,
                    slope_y / palette::CELL_ASPECT,
                    self.azimuth,
                    palette::SUN_ALTITUDE,
                );
                let color = scale(base, shade.mul_add(0.9, 0.35));
                let glyph = if self.mode == Mode::Contours && self.is_contour(x, y) {
                    marching_glyph(self.contour_case(x, y))
                } else {
                    ' '
                };
                let fg = if glyph == ' ' {
                    color
                } else {
                    mix(color, palette::BLACK, 0.55)
                };
                (glyph, fg, color)
            }
            Mode::Slope => {
                let slope = slope_x.hypot(slope_y).atan() / core::f32::consts::FRAC_PI_2;
                let color = mix(palette::BLACK, palette::WHITE, slope.clamp(0.0, 1.0));
                (' ', color, color)
            }
            Mode::Dithered => {
                let glyph = dithered_glyph(&SHADE, elevation, x, y);
                let color = mix(palette::BLACK, palette::WHITE, elevation);
                (glyph, palette::rgb(230, 230, 236), scale(color, 0.9))
            }
        }
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        // One world cell is drawn `CELL_SPAN` columns wide and one row tall,
        // so it covers a square patch of screen. Every other demo draws one
        // world cell per character cell and accepts a map stretched 2:1
        // vertically, which the glyph texture largely hides. This one is a
        // *relief* map: it is all smooth gradient and no texture, so the
        // stretch is the first thing the eye sees, and a hillshaded landform
        // with the wrong aspect reads as a different landform.
        self.camera.set_viewport(
            i32::from(area.width()) / CELL_SPAN,
            i32::from(area.height()),
        );
        let (left, top, right, bottom) = self.camera.visible_cells();

        for wy in top..=bottom {
            for wx in left..=right {
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let sy = area.top() + screen.y as u16;

                let (glyph, mut fg, mut bg) = self.render_cell(wx, wy);
                if wx == self.cursor.x && wy == self.cursor.y {
                    bg = mix(bg, palette::rgb(255, 236, 170), 0.5);
                    fg = mix(fg, palette::WHITE, 0.4);
                }
                for dx in 0..CELL_SPAN {
                    let sx = area.left() + (screen.x * CELL_SPAN + dx) as u16;
                    if sx >= area.right() {
                        break;
                    }
                    // The glyph goes in the first column only; a contour line
                    // or dither mark repeated across both would read as twice
                    // as dense as it is.
                    let ch = if dx == 0 { glyph } else { ' ' };
                    surface.put((sx, sy), ch, Style::new().fg(fg).bg(bg));
                }
            }
        }

        self.draw_compass(surface, area);
    }

    /// A small sun-direction indicator in the map's top-right corner, so the
    /// azimuth being adjusted (or orbiting) has a visible reference outside
    /// of watching shadows move.
    fn draw_compass(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 12 || area.height() < 5 {
            return;
        }
        let cx = area.right() - 6;
        let cy = area.top() + 2;
        let style = Style::new().fg(palette::rgb(250, 214, 120)).bg(ui::BG);
        // The sun sits on a small ring around the center, at the compass
        // bearing `self.azimuth` (clockwise from north / up).
        let sun_x = i32::from(cx) + (self.azimuth.sin() * 3.0).round() as i32;
        let sun_y = i32::from(cy) - (self.azimuth.cos() * 1.5).round() as i32;
        surface.put((cx, cy), '+', Style::new().fg(ui::DIM).bg(ui::BG));
        if sun_x >= 0 && sun_y >= 0 {
            surface.put((sun_x as u16, sun_y as u16), '*', style);
        }
    }

    fn status(&self) -> String {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let elevation = self.world.elevation_at(x, y);
        let (slope_x, slope_y) = self.world.gradient_at(x, y, RELIEF);
        let slope_pct = slope_x.hypot(slope_y).atan().to_degrees();
        let bearing = self.azimuth.to_degrees().rem_euclid(360.0);
        format!(
            "({x}, {y})  elev {:.0}%  slope {:.0}deg  sun {:.0}deg{}  mode: {}",
            elevation * 100.0,
            slope_pct,
            bearing,
            if self.orbit { " (orbiting)" } else { "" },
            self.mode.label(),
        )
    }
}

impl Demo for Relief {
    const NAME: &'static str = "12_relief";
    const TITLE: &'static str = "12 Relief";
    const BLURB: &'static str =
        "Hillshaded elevation cartography with contour lines and dithering.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("T", "cycle mode"),
            ("[ ]", "rotate sun"),
            ("O", "auto-orbit sun"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        if self.orbit {
            self.azimuth = dt.mul_add(ORBIT_DEGREES_PER_SEC.to_radians(), self.azimuth);
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

ascii_tile_demos::demo_main!(Relief);
