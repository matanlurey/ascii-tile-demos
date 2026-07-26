//! 01: Terrain cells -- one glyph per tile, the classic ASCII overworld.
//!
//! The baseline every other demo is a departure from: one world cell, one
//! character cell, colored by biome. This is what Dwarf Fortress, Cataclysm,
//! and `UnReal World` look like, and it is still the highest information density
//! per screen area of anything in this gallery.
//!
//! Techniques on show:
//!
//! - **Whittaker biome classification** ([`tilekit::world::whittaker`]):
//!   terrain from temperature and moisture rather than from elevation alone.
//! - **Hillshading** ([`tilekit::palette::hillshade_cells`]): the biome color is
//!   modulated by a northwest-lit relief shade computed from the local
//!   elevation gradient, which is what stops a large forest from being a flat
//!   green slab and gives the map its sense of terrain.
//! - **Animated water**: a travelling sine swell over the ocean, phase-shifted
//!   per cell, so the sea moves without anything else having to.
//!
//! ```sh
//! cargo run --example 01_terrain_cells --features crossterm
//! cargo run --example 01_terrain_cells --features software
//! cargo run --example 01_terrain_cells --features gl
//! cargo run --example 01_terrain_cells  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Style, Terminal};

use ascii_tile_demos::ui::{self, PrintStr};
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::glyphs::terrain;
use tilekit::noise::hash01;
use tilekit::palette::{self, hillshade_cells, mix, scale};
use tilekit::world::{Biome, World};

/// World size in cells. Comfortably larger than any viewport, so panning
/// always has somewhere to go.
const WORLD_W: i32 = 260;
/// See [`WORLD_W`].
const WORLD_H: i32 = 170;

/// How much to exaggerate elevation gradients before hillshading.
///
/// Raw gradients over a normalized heightmap are around 0.01 per cell, which
/// hillshades to a nearly uniform surface. This is the "vertical
/// exaggeration" knob every relief map has; 55 makes ridges legible without
/// turning gentle slopes into cliffs.
const RELIEF: f32 = 55.0;

/// State: the world, a camera over it, and the animation clock.
pub struct TerrainCells {
    world: World,
    camera: TileCamera,
    /// Wall-clock seconds since start, driving the water swell.
    time: f32,
    /// Cell the mouse last hovered, in world coordinates.
    cursor: Cell,
    fps: FpsMeter,
    /// Whether hillshading is applied. `H` toggles it, which is the clearest
    /// possible demonstration of what it contributes.
    shaded: bool,
}

impl Default for TerrainCells {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 7);
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
            shaded: true,
        }
    }
}

impl TerrainCells {
    /// Rerolls the world and recenters on its capital.
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
                        KeyCode::Char('h' | 'H') => self.shaded = !self.shaded,
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
        // The status bar reports whatever is under the pointer, so a hover has
        // to update the cursor even when nothing is being dragged.
        let screen = Cell::new(i32::from(pos.x), i32::from(pos.y) - 1);
        match kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                self.cursor = self.camera.screen_to_world(screen);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Drag-to-pan: move the map opposite the pointer so the cell
                // under the cursor stays under the cursor, which is what every
                // map application does and what hands expect.
                let world = self.camera.screen_to_world(screen);
                let (dx, dy) = (self.cursor.x - world.x, self.cursor.y - world.y);
                self.camera.pan(dx, dy);
            }
            MouseEventKind::ScrollUp => self.camera.pan(0, -3),
            MouseEventKind::ScrollDown => self.camera.pan(0, 3),
            _ => {}
        }
    }

    /// The glyph, foreground, and background for one world cell.
    ///
    /// Two channels carry the terrain, and they do different jobs. The
    /// **background** is a dimmed, hillshaded biome color covering every cell,
    /// so a biome reads as a continuous *region* with visible relief. The
    /// **glyph** is scattered over only a fraction of cells, so the biome also
    /// has a texture that says what it is made of.
    ///
    /// Drawing the glyph on every cell instead (a solid field of `"`) reads as
    /// noise rather than as grass; omitting the background instead leaves
    /// sparse biomes as scattered specks on black, with no sense of extent.
    /// The scatter is keyed on absolute world position, so the pattern stays
    /// put when the map pans rather than crawling across the terrain.
    fn render_cell(&self, x: i32, y: i32) -> (char, Color, Color) {
        let biome = self.world.biome_at(x, y);
        let mut color = biome.color();
        let mut glyph = if hash01(0x9E37_79B9, x, y) < biome_density(biome) {
            biome.glyph()
        } else {
            ' '
        };

        // Rivers and roads override the biome they cross: they are what the
        // player is actually looking for on an overland map, so they win.
        if self.world.river_at(x, y) {
            glyph = terrain::WAVE;
            color = palette::rgb(96, 156, 214);
        } else if self.world.road_at(x, y) {
            glyph = '\u{00b7}';
            color = palette::rgb(214, 196, 156);
        }

        let mut shade = 1.0;
        if biome.is_water() {
            // A travelling wave: the phase term makes the swell move diagonally
            // across the sea rather than every cell pulsing in unison.
            let phase = self
                .time
                .mul_add(1.4, (x as f32).mul_add(0.55, y as f32 * 0.31));
            let swell = phase.sin().mul_add(0.5, 0.5);
            glyph = if swell > 0.80 { terrain::WAVE } else { ' ' };
            color = mix(color, palette::WHITE, swell * 0.16);
        } else if self.shaded {
            let (slope_x, slope_y) = self.world.gradient_at(x, y, RELIEF);
            // Remap the raw 0..1 cosine into a band around 1.0 so lit slopes
            // genuinely brighten and shadowed ones stay legible. Multiplying
            // by the raw shade instead would darken *everything*, since even a
            // fully lit surface only returns about 0.7.
            shade = hillshade_cells(slope_x, slope_y).mul_add(0.85, 0.45);
            color = scale(color, shade);
        }

        // The background is the same terrain color pulled most of the way to
        // the page background: enough to say "this region is forest", not so
        // much that it competes with the glyph drawn on top of it.
        let bg = scale(mix(biome.color(), ui::BG, 0.68), shade);

        if let Some(landmark) = self.world.landmark_at(x, y) {
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
                    // The reticle tints the background rather than replacing
                    // the glyph, so you can still read the terrain you picked.
                    bg = mix(bg, palette::rgb(255, 236, 170), 0.45);
                    color = mix(color, palette::WHITE, 0.30);
                }
                term.put_styled(sx, sy, glyph, Style::new().fg(color).bg(bg));
            }
        }
    }

    /// One line describing whatever the cursor is over.
    fn status(&self) -> String {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let biome = self.world.biome_at(x, y);
        let mut parts = vec![format!("({x}, {y})"), biome.name().to_owned()];
        if self.world.river_at(x, y) {
            parts.push("river".to_owned());
        }
        if self.world.road_at(x, y) {
            parts.push("road".to_owned());
        }
        if let Some(landmark) = self.world.landmark_at(x, y) {
            parts.push(format!("{} ({})", landmark.name, landmark.site.name()));
        }
        if biome != Biome::Ocean {
            let elevation = self.world.elevation_at(x, y);
            parts.push(format!("elev {:.0}%", elevation * 100.0));
        }
        parts.push(format!("seed {}", self.world.seed()));
        parts.join("  ")
    }
}

/// What fraction of a biome's cells draw its glyph.
///
/// Tuned by eye for how each biome should read: a forest is nearly solid
/// canopy, grassland is sparse tufts over its background color, and a desert
/// is mostly bare sand with the occasional dune. Mountains are solid because a
/// range with gaps in it looks like scree, not rock.
const fn biome_density(biome: Biome) -> f32 {
    match biome {
        Biome::Mountain | Biome::Peak | Biome::Jungle => 0.92,
        Biome::Forest | Biome::Taiga => 0.80,
        Biome::Ice => 0.30,
        Biome::Grassland | Biome::Savanna => 0.26,
        Biome::Marsh | Biome::Tundra => 0.34,
        Biome::Desert | Biome::Scrubland => 0.18,
        Biome::Coast => 0.22,
        // Water is drawn by the swell instead; a static density here would
        // fight the animation for the same cells.
        _ => 0.0,
    }
}

impl Demo for TerrainCells {
    const NAME: &'static str = "01_terrain_cells";
    const TITLE: &'static str = "01 Terrain cells";
    const BLURB: &'static str = "One glyph per tile: Whittaker biomes with northwest hillshading.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("H", "hillshade"),
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
        // `print_styled_str` comes from the PrintStr extension trait; importing
        // it here keeps the trait in scope for the chrome helpers above.
        let _ = |t: &mut Terminal<B>| t.print_styled_str(0, 0, "", Style::new());

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(TerrainCells);
