//! 13: Parchment -- the map as an antique document, not a game board.
//!
//! Every other demo in this gallery colors terrain; this one draws it, the way
//! an engraver would have: no fill colors, just ink density and hatching
//! patterns on aged paper. The same generated world as every other demo, run
//! through a completely different rendering philosophy.
//!
//! Techniques on show:
//!
//! - **Hatching instead of color** ([`biome_ink`]): each biome gets a hand
//!   -drawn-looking pattern (chevrons for mountains, tick marks for marsh,
//!   sparse dots for desert) rather than a fill, because pen-and-ink maps have
//!   only one or two ink tones to work with.
//! - **Traced coastline** ([`trace_coast`]): the land/water boundary is found
//!   by a four-neighbour edge test and drawn as a heavier ink line, with a
//!   parallel offshore line for the classic engraved-sea ripple. See the
//!   [Whittaker biome](https://www.redblobgames.com/maps/terrain-from-noise/#biomes)
//!   approach `01_terrain_cells.rs` uses for the underlying land/water field;
//!   this demo only needs its boundary.
//! - **Paper texture**: a low-frequency noise field mottles the parchment
//!   background so it reads as a physical material rather than flat digital
//!   beige.
//! - **Greedy label placement** ([`place_labels`]): settlement names are set
//!   beside their marker, trying a small ring of candidate offsets and keeping
//!   the first one that doesn't overlap a label already placed. No global
//!   optimization; the map has a few dozen labels at most, so a a greedy pass
//!   in placement order is indistinguishable from anything fancier.
//! - **Cartouche and border**: a drawn double-ruled frame and a title box
//!   naming the map and its seed, the way real historical maps carry their own
//!   caption.
//!
//! See [`tilekit::palette::PARCHMENT`] for the two-tone ramp everything here
//! is drawn in, and Tom Patterson's [Shaded relief in
//! cartography](http://www.shadedrelief.com/hypso/) for the broader convention
//! of using engraving-derived textures instead of photographic color.
//!
//! ```sh
//! cargo run --example 13_parchment --features crossterm
//! cargo run --example 13_parchment --features software
//! cargo run --example 13_parchment --features gl
//! cargo run --example 13_parchment  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::camera::TileCamera;
use tilekit::geom::Cell;
use tilekit::noise::hash01;
use tilekit::palette::{self, mix};
use tilekit::world::{Biome, Site, World};

/// World size in cells, matching `01_terrain_cells` so the same seed produces
/// a directly comparable map in both styles.
const WORLD_W: i32 = 220;
/// See [`WORLD_W`].
const WORLD_H: i32 = 150;

/// Which ink scheme to render in. `M` toggles between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheme {
    /// Warm sepia ink on aged paper -- [`tilekit::palette::PARCHMENT`].
    Parchment,
    /// Cold white-on-blue linework, the look of a blueprint or a technical
    /// survey rather than a historical map. Same hatching, different ramp,
    /// which is the point: the *drawing* carries the information, the *ramp*
    /// only carries the mood.
    Blueprint,
}

impl Scheme {
    const fn next(self) -> Self {
        match self {
            Self::Parchment => Self::Blueprint,
            Self::Blueprint => Self::Parchment,
        }
    }

    /// `(paper, ink)` base colors for this scheme.
    const fn colors(self) -> (Color, Color) {
        match self {
            Self::Parchment => (palette::rgb(214, 194, 152), palette::rgb(64, 46, 30)),
            Self::Blueprint => (palette::rgb(20, 42, 78), palette::rgb(224, 234, 246)),
        }
    }
}

/// State: the world, a camera over it, the active scheme, and the animation
/// clock driving the "wet ink" pulse on the coastline.
pub struct Parchment {
    world: World,
    camera: TileCamera,
    time: f32,
    cursor: Cell,
    fps: FpsMeter,
    scheme: Scheme,
    /// Settlement labels placed once per world (not per frame: placement is a
    /// search, and nothing about it depends on the camera or the clock).
    labels: Vec<Label>,
}

/// A placed name, in world cells, anchored to the left of its first
/// character.
struct Label {
    x: i32,
    y: i32,
    text: String,
}

impl Default for Parchment {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 3);
        let (sx, sy) = world.start_position();
        let mut camera = TileCamera::new(
            i32::from(ascii_tile_demos::GRID_COLS),
            i32::from(ascii_tile_demos::GRID_ROWS),
            WORLD_W,
            WORLD_H,
        );
        camera.center_on(Cell::new(sx, sy));
        let labels = place_labels(&world);
        Self {
            world,
            camera,
            time: 0.0,
            cursor: Cell::new(sx, sy),
            fps: FpsMeter::new(),
            scheme: Scheme::Parchment,
            labels,
        }
    }
}

impl Parchment {
    fn reroll(&mut self, delta: u32) {
        let seed = self.world.seed().wrapping_add(delta);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (sx, sy) = self.world.start_position();
        self.camera.center_on(Cell::new(sx, sy));
        self.cursor = Cell::new(sx, sy);
        self.labels = place_labels(&self.world);
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
                        KeyCode::Char('m' | 'M') => self.scheme = self.scheme.next(),
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
            MouseEventKind::Scroll { dy, .. } if dy > 0.0 => self.camera.pan(0, -3),
            MouseEventKind::Scroll { dy, .. } if dy < 0.0 => self.camera.pan(0, 3),
            _ => {}
        }
    }

    /// Draws the map: paper texture, hatched terrain, coastline, labels,
    /// frame, and cartouche, in that back-to-front order.
    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.camera
            .set_viewport(i32::from(area.width()), i32::from(area.height()));
        let (paper, ink) = self.scheme.colors();
        let (left, top, right, bottom) = self.camera.visible_cells();

        for wy in top..=bottom {
            for wx in left..=right {
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                let (glyph, ink_strength) = self.cell_ink(wx, wy);

                // Paper mottling ([`cell_background`]) is a slow, large-scale
                // noise field so the texture reads as fibrous variation in the
                // sheet, not as per-cell speckle.
                let bg = cell_background(paper, wx, wy);
                let fg = mix(bg, ink, ink_strength);
                put_clipped(surface, area, sx, sy, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        self.draw_coastline(surface, area);
        self.draw_labels(surface, area, ink);
        draw_frame(surface, area, paper, ink);
        self.draw_cartouche(surface, area, paper, ink);
    }

    /// The glyph and ink strength (`0.0` bare paper, `1.0` full ink) for one
    /// world cell, from its biome's hatching pattern.
    fn cell_ink(&self, x: i32, y: i32) -> (char, f32) {
        let biome = self.world.biome_at(x, y);
        if biome.is_water() {
            // Open water carries no hatching at all; the coastline pass draws
            // its only ink. A blank sea is exactly how 18th-century charts
            // left unsounded water: the coast was surveyed, the deep was not.
            return (' ', 0.0);
        }
        if self.world.river_at(x, y) {
            return ('~', 0.9);
        }
        if self.world.road_at(x, y) {
            return ('.', 0.7);
        }
        biome_ink(biome, x, y)
    }

    /// Draws the coastline directly over whatever `draw_map`'s terrain pass
    /// already put down, recomputing the same paper/mottle background each
    /// cell used so the ink composites onto the correct color instead of onto
    /// an assumed default. Cheap to recompute (one hash lookup) and keeps this
    /// pass from needing to read back the grid.
    fn draw_coastline(&self, surface: &mut Surface<'_>, area: Rect) {
        let (paper, ink) = self.scheme.colors();
        // A slow pulse (period ~6s) on the coastline's darkness: "wet ink"
        // that never fully dries, the one animated flourish that belongs on
        // an otherwise static document.
        let wet = (self.time * 1.05).sin().mul_add(0.5, 0.5);

        let (left, top, right, bottom) = self.camera.visible_cells();
        for wy in top..=bottom {
            for wx in left..=right {
                let Some((is_water_side, _)) = coast_edge(&self.world, wx, wy) else {
                    continue;
                };
                let screen = self.camera.world_to_screen(Cell::new(wx, wy));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                let glyph = if is_water_side { '~' } else { '\u{2500}' };
                let strength = 0.55f32.mul_add(wet, 0.75);
                let bg = cell_background(paper, wx, wy);
                let fg = mix(bg, ink, strength);
                put_clipped(surface, area, sx, sy, glyph, Style::new().fg(fg).bg(bg));

                // One offshore ripple line, one cell further out to sea: the
                // engraved-sea convention of parallel lines paralleling the
                // shore. Only drawn from the water side to avoid doubling up
                // when both neighbours in a strait qualify.
                if is_water_side && let Some((ox, oy)) = coast_offshore(&self.world, wx, wy) {
                    let oscreen = self.camera.world_to_screen(Cell::new(ox, oy));
                    if self.camera.on_screen(oscreen) {
                        let (osx, osy) = (
                            area.left() + oscreen.x as u16,
                            area.top() + oscreen.y as u16,
                        );
                        let obg = cell_background(paper, ox, oy);
                        let ofg = mix(obg, ink, strength * 0.5);
                        put_clipped(surface, area, osx, osy, '~', Style::new().fg(ofg).bg(obg));
                    }
                }
            }
        }
    }

    fn draw_labels(&self, surface: &mut Surface<'_>, area: Rect, ink: Color) {
        let (paper, _) = self.scheme.colors();
        for label in &self.labels {
            for (i, ch) in label.text.chars().enumerate() {
                let (lx, ly) = (label.x + i as i32, label.y);
                let screen = self.camera.world_to_screen(Cell::new(lx, ly));
                if !self.camera.on_screen(screen) {
                    continue;
                }
                let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                let bg = cell_background(paper, lx, ly);
                put_clipped(surface, area, sx, sy, ch, Style::new().fg(ink).bg(bg));
            }
        }
    }

    fn draw_cartouche(&self, surface: &mut Surface<'_>, area: Rect, paper: Color, ink: Color) {
        let title = format!(" A MAPPE OF THE REALM -- seed {} ", self.world.seed());
        let w = title.chars().count() as u16 + 2;
        if area.width() < w + 4 || area.height() < 4 {
            return;
        }
        let x0 = area.left() + (area.width() - w) / 2;
        let y0 = area.top() + 1;
        for dx in 0..w {
            put_clipped(
                surface,
                area,
                x0 + dx,
                y0,
                '\u{2500}',
                Style::new().fg(ink).bg(paper),
            );
            put_clipped(
                surface,
                area,
                x0 + dx,
                y0 + 2,
                '\u{2500}',
                Style::new().fg(ink).bg(paper),
            );
        }
        put_clipped(
            surface,
            area,
            x0,
            y0 + 1,
            '\u{2502}',
            Style::new().fg(ink).bg(paper),
        );
        put_clipped(
            surface,
            area,
            x0 + w - 1,
            y0 + 1,
            '\u{2502}',
            Style::new().fg(ink).bg(paper),
        );
        for (i, ch) in title.chars().enumerate() {
            put_clipped(
                surface,
                area,
                x0 + 1 + i as u16,
                y0 + 1,
                ch,
                Style::new().fg(ink).bg(paper),
            );
        }
    }

    fn status(&self) -> String {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let biome = self.world.biome_at(x, y);
        let mut parts = vec![format!("({x}, {y})"), biome.name().to_owned()];
        parts.push(match self.scheme {
            Scheme::Parchment => "parchment".to_owned(),
            Scheme::Blueprint => "blueprint".to_owned(),
        });
        parts.push(format!("seed {}", self.world.seed()));
        parts.join("  ")
    }
}

/// The mottled paper background color for one world cell.
///
/// The coastline and label passes redraw the same background their cell
/// received in `draw_map`'s terrain pass instead of reading it back from the
/// grid, since both passes derive it from the same pure function of `(x, y)`
/// and a lookup would cost more than recomputing one hash.
fn cell_background(paper: Color, x: i32, y: i32) -> Color {
    let mottle = hash01(0xC0FF_EE01, x / 3, y / 3) * 0.10;
    mix(paper, palette::BLACK, mottle)
}

/// `Terminal::put_styled` with clipping: tiles and overlays at the map edges
/// legitimately hang partly outside the viewport.
fn put_clipped(surface: &mut Surface<'_>, area: Rect, x: u16, y: u16, glyph: char, style: Style) {
    if x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom() {
        surface.put((x, y), glyph, style);
    }
}

/// Draws a double-ruled border around `area`.
fn draw_frame(surface: &mut Surface<'_>, area: Rect, paper: Color, ink: Color) {
    if area.width() < 2 || area.height() < 2 {
        return;
    }
    let style = Style::new().fg(ink).bg(paper);
    for x in area.left()..area.right() {
        put_clipped(surface, area, x, area.top(), '\u{2550}', style);
        put_clipped(surface, area, x, area.bottom() - 1, '\u{2550}', style);
    }
    for y in area.top()..area.bottom() {
        put_clipped(surface, area, area.left(), y, '\u{2551}', style);
        put_clipped(surface, area, area.right() - 1, y, '\u{2551}', style);
    }
    put_clipped(surface, area, area.left(), area.top(), '\u{2554}', style);
    put_clipped(
        surface,
        area,
        area.right() - 1,
        area.top(),
        '\u{2557}',
        style,
    );
    put_clipped(
        surface,
        area,
        area.left(),
        area.bottom() - 1,
        '\u{255a}',
        style,
    );
    put_clipped(
        surface,
        area,
        area.right() - 1,
        area.bottom() - 1,
        '\u{255d}',
        style,
    );
}

/// The hatching glyph and ink strength for one cell of `biome`.
///
/// A position hash rather than a fixed pattern picks which glyph in a
/// biome's small set appears at each cell, so the hatching has the irregular,
/// hand-drawn look of real engraving rather than a mechanically repeating
/// wallpaper tile.
fn biome_ink(biome: Biome, x: i32, y: i32) -> (char, f32) {
    let h = hash01(0x5EED_1234, x, y);
    match biome {
        Biome::Mountain | Biome::Peak => {
            // Chevron chains: the standard cartographic mountain hatch.
            let glyph = if h < 0.5 { '^' } else { '\u{2227}' };
            (glyph, 0.85)
        }
        Biome::Forest | Biome::Taiga | Biome::Jungle => {
            // Stylised trees, sparse enough to read as individual marks
            // rather than a solid fill -- ink is expensive on real paper.
            if h < 0.35 {
                ('\u{2663}', 0.75)
            } else {
                (' ', 0.0)
            }
        }
        Biome::Marsh => {
            // Horizontal tick marks: the conventional wetland hatch.
            if h < 0.4 { ('-', 0.5) } else { (' ', 0.0) }
        }
        Biome::Desert | Biome::Scrubland => {
            // Sparse dots: bare ground gets the least ink of any land biome.
            if h < 0.12 { ('.', 0.4) } else { (' ', 0.0) }
        }
        Biome::Tundra | Biome::Ice => {
            if h < 0.2 {
                ('\'', 0.35)
            } else {
                (' ', 0.0)
            }
        }
        Biome::Grassland | Biome::Savanna => {
            if h < 0.15 {
                ('\'', 0.3)
            } else {
                (' ', 0.0)
            }
        }
        Biome::Coast | Biome::Ocean | Biome::Sea | Biome::Lake => (' ', 0.0),
    }
}

/// Whether `(x, y)` sits on the coastline (land with a water neighbour, or
/// water with a land neighbour), and if so, which side it is on.
///
/// Returns `(is_water_side, is_land_side)` as a convenience for the caller,
/// which only needs to know which glyph to draw; both are computed from the
/// same neighbour scan so the two questions never disagree.
fn coast_edge(world: &World, x: i32, y: i32) -> Option<(bool, bool)> {
    let here_water = world.biome_at(x, y).is_water();
    let mut touches_other = false;
    for (nx, ny) in world.neighbors4(x, y) {
        if world.biome_at(nx, ny).is_water() != here_water {
            touches_other = true;
            break;
        }
    }
    if !touches_other {
        return None;
    }
    Some((here_water, !here_water))
}

/// One cell further out to sea from a coastal water cell `(x, y)`, for the
/// offshore ripple line. Picks the neighbour that moves *away* from the
/// nearest land, so the ripple sits seaward of the coast rather than back on
/// top of it.
fn coast_offshore(world: &World, x: i32, y: i32) -> Option<(i32, i32)> {
    let mut best: Option<(i32, i32, i32)> = None;
    for (nx, ny) in world.neighbors4(x, y) {
        if !world.biome_at(nx, ny).is_water() {
            continue;
        }
        let (dx, dy) = (nx - x, ny - y);
        let candidate = (nx + dx, ny + dy);
        if world.in_bounds(candidate.0, candidate.1) {
            let score = i32::from(world.biome_at(candidate.0, candidate.1).is_water());
            if best.is_none_or(|(_, _, s)| score > s) {
                best = Some((candidate.0, candidate.1, score));
            }
        }
    }
    best.map(|(x, y, _)| (x, y))
}

/// Distance (Chebyshev) at which a new label candidate is rejected for
/// overlapping an already-placed one, including its text width.
const LABEL_CLEARANCE: i32 = 1;

/// Places one label per settlement, trying a small ring of offsets around the
/// marker and keeping the first that does not overlap a previously placed
/// label.
///
/// Greedy in placement order (largest settlements first, since `landmarks` is
/// already generated capital-first): a capital's name is more important to
/// keep than a town's, so if only one can have its preferred spot, the
/// capital should get it. See Boris the Brave's overview of label placement
/// tradeoffs for why a full solver is unwarranted at this scale; a few dozen
/// labels is well within where greedy-by-priority stops being visibly wrong.
fn place_labels(world: &World) -> Vec<Label> {
    // Offsets tried in order: directly right first (the natural reading
    // position), then a ring of alternates if that space is already taken.
    const OFFSETS: [(i32, i32); 8] = [
        (2, 0),
        (2, -1),
        (2, 1),
        (-1, -1),
        (-1, 1),
        (0, -1),
        (0, 1),
        (-1, 0),
    ];

    let mut placed: Vec<Label> = Vec::new();
    for landmark in world.landmarks.iter().filter(|l| l.site.is_settlement()) {
        let text = if landmark.site == Site::Capital {
            format!("{} \u{2605}", landmark.name)
        } else {
            landmark.name.clone()
        };
        let width = text.chars().count() as i32;

        for &(dx, dy) in &OFFSETS {
            let (lx, ly) = (landmark.x + dx.max(0), landmark.y + dy);
            if !world.in_bounds(lx, ly) {
                continue;
            }
            let overlaps = placed.iter().any(|p| {
                let p_width = p.text.chars().count() as i32;
                (ly - p.y).abs() <= LABEL_CLEARANCE
                    && lx < p.x + p_width + LABEL_CLEARANCE
                    && lx + width + LABEL_CLEARANCE > p.x
            });
            if !overlaps {
                placed.push(Label { x: lx, y: ly, text });
                break;
            }
        }
    }
    placed
}

impl Demo for Parchment {
    const NAME: &'static str = "13_parchment";
    const TITLE: &'static str = "13 Parchment";
    const BLURB: &'static str =
        "Hatched ink on aged paper instead of color: the map as a document.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("M", "parchment/blueprint"),
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

        let mut surface = term.surface();
        let (paper, _) = self.scheme.colors();
        ui::fill(&mut surface, content, Style::new().bg(paper));
        self.draw_map(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(Parchment);
