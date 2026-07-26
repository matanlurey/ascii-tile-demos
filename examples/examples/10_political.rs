//! 10: Political -- province ownership as a translucent overlay on terrain.
//!
//! Every 4X strategy map needs a political layer, and the mistake that is easy
//! to make is letting it *replace* the terrain instead of sitting on top of
//! it. A player still needs to see the forest and the mountain range under a
//! province's color, or the map stops being a map and becomes a stained-glass
//! window. This demo keeps the terrain from `01_terrain_cells` fully legible
//! and washes a faction color over it at low strength instead.
//!
//! The underlying provinces come from [`tilekit::world::World::build_provinces`]
//! (Voronoi assignment around settlements, relaxed twice toward each region's
//! centroid): see [Voronoi diagrams for territory maps](https://thegoodtheorist.substack.com/p/application-of-voronoi-diagrams-to)
//! for why this is the standard approach to procedural province generation.
//!
//! Techniques on show:
//!
//! - **Translucent territory wash**: [`tilekit::palette::mix`] blends a
//!   [`tilekit::palette::faction`] color into the terrain color at a fixed,
//!   low strength, so the biome underneath stays readable.
//! - **One-cell province borders**: a cell is a border cell iff its north or
//!   west neighbor belongs to a different province. Checking only those two
//!   directions (not all four) means every boundary is counted exactly once,
//!   giving a clean one-cell-thick line with no doubled edges -- the same
//!   trick `19_overworld` in the `retroglyph` examples gallery uses for its
//!   hex/square grid overlays.
//! - **Greedy label placement**: capital labels are placed left-to-right,
//!   skipping any label whose bounding rectangle overlaps one already placed,
//!   which is the simplest label-declutter algorithm that still produces a
//!   readable result on a moderately dense map.
//! - **Diplomacy tinting**: a synthetic ally/neutral/hostile relation, derived
//!   deterministically from the seed, recolors every foreign province by
//!   stance rather than by identity -- the "who do I need to worry about"
//!   view every 4X diplomacy screen offers as an alternative to the raw map.
//!
//! ```sh
//! cargo run --example 10_political --features crossterm
//! cargo run --example 10_political --features software
//! cargo run --example 10_political --features gl
//! cargo run --example 10_political  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, KeyModifiers, Rect, Style, Terminal};

use ascii_tile_demos::ui::{self, PrintStr};
use ascii_tile_demos::util::perf::FpsMeter;
use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS};
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::glyphs::terrain;
use tilekit::noise::hash01;
use tilekit::palette::{self, faction, hillshade_nw, mix, scale};
use tilekit::world::{Biome, Site, World};

/// World size in cells, matching `01_terrain_cells` so the two demos show the
/// same kind of map at the same density.
const WORLD_W: i32 = 260;
/// See [`WORLD_W`].
const WORLD_H: i32 = 170;

/// Vertical exaggeration for hillshading. See `01_terrain_cells` for why this
/// needs to be this large: raw normalized-heightmap gradients are tiny.
const RELIEF: f32 = 55.0;

/// How strongly the province wash tints terrain in [`View::Political`].
///
/// Low enough that a forest under a province still reads as forest; high
/// enough that two adjacent provinces are obviously different colors even
/// when they share a biome.
const WASH_STRENGTH: f32 = 0.34;

/// How strongly the diplomacy tint darkens/recolors foreign territory in
/// [`View::Diplomacy`]. Stronger than the political wash because the whole
/// point of this view is "ignore what the terrain is, tell me who owns it and
/// whether that is a problem".
const DIPLOMACY_STRENGTH: f32 = 0.5;

/// A capital's label, already measured, for the greedy declutter pass.
struct Label {
    /// Anchor cell (the capital marker's position).
    anchor: Cell,
    text: String,
    /// The label's screen-space bounding rect, computed fresh each frame
    /// since panning moves it; `None` once it has failed to place.
    rect: Option<Rect>,
}

/// Which map layer is active. `T` cycles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    /// Full terrain (as `01_terrain_cells`) with a translucent province wash
    /// and borders.
    Political,
    /// Plain terrain, borders only -- what the map looks like with the
    /// political layer stripped down to just its boundaries.
    BordersOnly,
    /// Every foreign province recolored by its relation to the player's,
    /// rather than by its own identity.
    Diplomacy,
}

impl View {
    const fn next(self) -> Self {
        match self {
            Self::Political => Self::BordersOnly,
            Self::BordersOnly => Self::Diplomacy,
            Self::Diplomacy => Self::Political,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Political => "political",
            Self::BordersOnly => "borders only",
            Self::Diplomacy => "diplomacy",
        }
    }
}

/// A diplomatic stance, for [`View::Diplomacy`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stance {
    Player,
    Ally,
    Neutral,
    Hostile,
}

impl Stance {
    const fn color(self) -> Color {
        match self {
            Self::Player => palette::rgb(90, 180, 230),
            Self::Ally => palette::rgb(96, 190, 110),
            Self::Neutral => palette::rgb(196, 182, 110),
            Self::Hostile => palette::rgb(206, 82, 74),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Player => "you",
            Self::Ally => "ally",
            Self::Neutral => "neutral",
            Self::Hostile => "hostile",
        }
    }
}

/// State: the world, a camera over it, and the view mode.
pub struct Political {
    world: World,
    camera: TileCamera,
    time: f32,
    cursor: Cell,
    fps: FpsMeter,
    view: View,
    /// The player's home province: wherever the capital landed.
    player_province: usize,
    /// Deterministic per-province stance toward the player, indexed by
    /// province id. Computed once per world so it doesn't flicker.
    stances: Vec<Stance>,
}

impl Default for Political {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 11);
        let (sx, sy) = world.start_position();
        let mut camera =
            TileCamera::new(i32::from(GRID_COLS), i32::from(GRID_ROWS), WORLD_W, WORLD_H);
        camera.center_on(Cell::new(sx, sy));
        let player_province = world.province_at(sx, sy);
        let stances = compute_stances(&world, player_province);
        Self {
            world,
            camera,
            time: 0.0,
            cursor: Cell::new(sx, sy),
            fps: FpsMeter::new(),
            view: View::Political,
            player_province,
            stances,
        }
    }
}

/// Assigns every province (other than the player's) a stance, deterministic
/// in the world's seed so the diplomatic picture doesn't reshuffle every
/// frame or every reroll-adjacent seed.
fn compute_stances(world: &World, player_province: usize) -> Vec<Stance> {
    (0..world.province_count())
        .map(|p| {
            if p == player_province {
                return Stance::Player;
            }
            let (sx, sy) = world.province_seeds.get(p).copied().unwrap_or((0, 0));
            let roll = hash01(world.seed() ^ 0x5CA1_AB1E, sx, sy);
            if roll < 0.35 {
                Stance::Hostile
            } else if roll < 0.65 {
                Stance::Neutral
            } else {
                Stance::Ally
            }
        })
        .collect()
}

impl Political {
    fn reroll(&mut self, delta: u32) {
        let seed = self.world.seed().wrapping_add(delta);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (sx, sy) = self.world.start_position();
        self.camera.center_on(Cell::new(sx, sy));
        self.cursor = Cell::new(sx, sy);
        self.player_province = self.world.province_at(sx, sy);
        self.stances = compute_stances(&self.world, self.player_province);
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
                        KeyCode::Char('t' | 'T') => self.view = self.view.next(),
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

    /// The province a cell straddling the map edge should be compared
    /// against for the border test: real provinces off-map, or its own
    /// province at the boundary (so the map edge itself is never drawn as a
    /// border).
    fn province_or_self(&self, x: i32, y: i32, here: usize) -> usize {
        if self.world.in_bounds(x, y) {
            self.world.province_at(x, y)
        } else {
            here
        }
    }

    /// Whether `(x, y)` sits on a province boundary: its north or west
    /// neighbor belongs to a different province. Checking only these two
    /// directions (rather than all four) counts each edge between two
    /// provinces exactly once, from the cell below/right of it, which is what
    /// keeps the drawn boundary a single cell thick instead of two.
    fn is_border(&self, x: i32, y: i32) -> bool {
        if !self.world.in_bounds(x, y) || self.world.biome_at(x, y).is_water() {
            return false;
        }
        let here = self.world.province_at(x, y);
        self.province_or_self(x, y - 1, here) != here
            || self.province_or_self(x - 1, y, here) != here
    }

    /// Base terrain glyph/color exactly as `01_terrain_cells` computes it,
    /// minus the reticle (this demo draws its own selection highlight scoped
    /// to a province instead of a single cell).
    fn render_terrain(&self, x: i32, y: i32) -> (char, Color, Color) {
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

    /// Glyph/fg/bg for one world cell in the current [`View`].
    fn render_cell(&self, x: i32, y: i32) -> (char, Color, Color) {
        let (glyph, mut fg, mut bg) = self.render_terrain(x, y);
        let biome = self.world.biome_at(x, y);
        if biome.is_water() {
            return (glyph, fg, bg);
        }
        let province = self.world.province_at(x, y);

        match self.view {
            View::Political => {
                let wash = faction(province);
                fg = mix(fg, wash, WASH_STRENGTH * 0.6);
                bg = mix(bg, wash, WASH_STRENGTH);
            }
            View::BordersOnly => {}
            View::Diplomacy => {
                let stance = self
                    .stances
                    .get(province)
                    .copied()
                    .unwrap_or(Stance::Neutral);
                let tint = stance.color();
                fg = mix(fg, tint, DIPLOMACY_STRENGTH * 0.6);
                bg = mix(bg, tint, DIPLOMACY_STRENGTH);
            }
        }

        if self.is_border(x, y) {
            // The selected (player's) province border shimmers so it stands
            // out from the thirty-odd other borders on a busy map; every
            // other border stays a static, slightly-brightened line.
            let selected = province == self.player_province || {
                let north = self.province_or_self(x, y - 1, province);
                let west = self.province_or_self(x - 1, y, province);
                north == self.player_province || west == self.player_province
            };
            let line_color = if selected {
                let pulse = (self.time * 3.0).sin().mul_add(0.5, 0.5);
                mix(palette::WHITE, palette::rgb(255, 224, 140), pulse)
            } else {
                mix(bg, palette::WHITE, 0.55)
            };
            bg = mix(bg, line_color, if selected { 0.85 } else { 0.55 });
        }

        (glyph, fg, bg)
    }

    fn draw_map<B: Backend>(&mut self, term: &mut Terminal<B>, area: Rect) {
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

                let (glyph, mut fg, mut bg) = self.render_cell(wx, wy);

                if let Some(landmark) = self.world.landmark_at(wx, wy) {
                    let (marker, marker_color) = landmark.site.glyph_color();
                    term.put_styled(sx, sy, marker, Style::new().fg(marker_color).bg(bg));
                    continue;
                }

                if wx == self.cursor.x && wy == self.cursor.y {
                    bg = mix(bg, palette::rgb(255, 236, 170), 0.45);
                    fg = mix(fg, palette::WHITE, 0.30);
                }
                term.put_styled(sx, sy, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        self.draw_labels(term, area);
    }

    /// Places capital labels with greedy overlap rejection: each label's
    /// screen rectangle is tested against every rectangle already accepted
    /// this frame, and the label is simply dropped if it collides. Labels are
    /// considered in a stable order (world position) so which one wins a
    /// contested spot doesn't change frame to frame.
    fn draw_labels<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let mut labels: Vec<Label> = self
            .world
            .landmarks
            .iter()
            .filter(|l| l.site == Site::Capital || l.site == Site::City)
            .map(|l| Label {
                anchor: Cell::new(l.x, l.y),
                text: l.name.clone(),
                rect: None,
            })
            .collect();

        let mut placed: Vec<Rect> = Vec::new();
        for label in &mut labels {
            let screen = self.camera.world_to_screen(label.anchor);
            if !self.camera.on_screen(screen) {
                continue;
            }
            // Anchor the label one cell right of its marker, vertically
            // centered on it; clip to the visible content area so a label
            // near the edge doesn't panic put_styled.
            let text_len = label.text.chars().count() as u16;
            let lx = area.left() + screen.x as u16 + 2;
            let ly = area.top() + screen.y as u16;
            if lx + text_len > area.right() || ly >= area.bottom() {
                continue;
            }
            let rect = Rect::new(lx.saturating_sub(1), ly, text_len + 1, 1);
            if placed.iter().any(|p| rects_overlap(*p, rect)) {
                continue;
            }
            placed.push(rect);
            label.rect = Some(rect);
        }

        for label in &labels {
            if label.rect.is_none() {
                continue;
            }
            let screen = self.camera.world_to_screen(label.anchor);
            let lx = area.left() + screen.x as u16 + 2;
            let ly = area.top() + screen.y as u16;
            term.print_styled_str(
                lx,
                ly,
                &label.text,
                Style::new().fg(ui::FG).bg(mix(ui::BG, palette::BLACK, 0.4)),
            );
        }
    }

    /// One line describing the province and settlement under the cursor.
    fn status(&self) -> String {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let biome = self.world.biome_at(x, y);
        if biome.is_water() {
            return format!("({x}, {y})  {}  view: {}", biome.name(), self.view.label());
        }
        let province = self.world.province_at(x, y);
        let capital = self
            .world
            .landmarks
            .iter()
            .find(|l| l.site == Site::Capital && l.province == province)
            .map_or("no capital", |l| l.name.as_str());
        let area = self
            .world
            .province
            .iter()
            .filter(|&&p| p == province)
            .count();
        let mut parts = vec![
            format!("({x}, {y})"),
            format!("province {province}"),
            format!("capital: {capital}"),
            format!("area: {area} cells"),
        ];
        if self.view == View::Diplomacy {
            let stance = self
                .stances
                .get(province)
                .copied()
                .unwrap_or(Stance::Neutral);
            parts.push(stance.label().to_owned());
        }
        parts.push(format!("view: {}", self.view.label()));
        parts.join("  ")
    }
}

/// Whether two screen rectangles overlap, including sharing an edge (a
/// one-cell gap between adjacent labels is what keeps them from visually
/// running together).
fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

/// See `01_terrain_cells` for the rationale; identical table so the two
/// demos' terrain reads the same way.
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

impl Demo for Political {
    const NAME: &'static str = "10_political";
    const TITLE: &'static str = "10 Political";
    const BLURB: &'static str = "Province ownership as a translucent wash with declining borders.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("T", "cycle view"),
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

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(Political);
