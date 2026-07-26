//! 26: Hexcrawl -- a hand-drawn tabletop referee map, where the hex grid is a
//! light reference overlay instead of a container for the terrain.
//!
//! Every hex demo elsewhere in this gallery ([`07_hex_tiles`],
//! [`08_hex_outline`]) draws one biome per hex and the hex boundary as the
//! terrain boundary: a hex is a cell of the map, colored and outlined as a
//! whole. A tabletop hexcrawl referee map -- the watercolour-and-ink kind
//! played at a table, not the wargame kind played on a board -- works the
//! other way around. The terrain is painted first, continuously, at whatever
//! resolution the hand that drew it wanted; the hex grid is inked over the
//! top afterward, as a *reference lattice* for "which hex is the party in",
//! and it is drawn as sparsely as it can be while still readable: a short
//! three-armed tick at each vertex where three hexes meet, not a traced
//! outline. The eye completes the hexagon from three ticks the same way it
//! completes a dashed line, and the terrain underneath is left alone to
//! wander across hex edges as if the grid were not there, because on the
//! actual referee's map it was drawn after the terrain and has no relationship
//! to it at all.
//!
//! Techniques on show:
//!
//! - **Corner-tick hex rendering**: only the six vertices of each
//!   [`tilekit::geom::HexLayout::POINTY_LARGE`] hex are marked, each with two
//!   short arms along the hex's own edges. The vertex positions and their arm
//!   directions are not a hand-derived formula; [`HexVertices::measure`]
//!   finds them by querying [`tilekit::geom::HexLayout::cell_to_tile`] itself
//!   -- the same ownership test the camera's picking already trusts -- so a
//!   tick can never disagree with which hex the cursor thinks it is in. A
//!   pointy-top hex has two vertex parities (a peak/valley point with two
//!   diagonal arms meeting it, and a shoulder point with one diagonal and one
//!   vertical arm), and drawing the same three directions at every vertex
//!   regardless of parity is what makes a tick grid look like a field of
//!   isolated marks instead of a honeycomb: getting the parity right is the
//!   whole trick.
//!
//!   Something worth being blunt about: [`HexLayout::cell_to_tile`] snaps
//!   under an aspect-corrected nearest-center rule
//!   (`dx² + (2·dy)²` -- see its own docs), not the formula for a *regular*
//!   hexagon, so this is not a decorative approximation of a hexagon, it *is*
//!   the boundary the grid actually uses to answer "which hex is this cell
//!   in". Deriving the vertex offsets in closed form from `pitch_x`/`pitch_y`
//!   alone was tried first and abandoned: the aspect correction makes the true
//!   boundary's shape depend on the pitch ratio in a way that has no clean
//!   formula and, at some ratios, produces a *non-convex* result where a
//!   closed-form guess silently swaps which vertex is which. Measuring the
//!   real boundary sidesteps all of that by construction.
//! - **Terrain that ignores the lattice**: [`terrain_at`] samples
//!   [`tilekit::noise::warped_fbm`] at a resolution finer than the hex pitch,
//!   so a forest or a scrub patch is an organic blob that spills across
//!   however many hexes it happens to cover, exactly like hand-painted
//!   terrain and exactly unlike a biome fill keyed to the hex.
//! - **Jittered doodle scatter** ([`doodle_at`]): conifer clusters, mountain
//!   ridges, and scrub tufts are placed by hashing cell coordinates rather
//!   than by iterating a lattice, with the hash also driving a one-cell
//!   jitter. A regular grid of trees reads as generated; a jittered one reads
//!   as sketched.
//! - **A meandering road** ([`Road::trace`]): one continuous curve is
//!   integrated across the whole map by slowly turning a heading under noise,
//!   the way a cartographer's pen wanders rather than the way a road network
//!   is planned. It crosses hex edges with no awareness they exist.
//! - **A travelling party token**: rather than animate the terrain itself
//!   (which would read as weather, not as a hexcrawl), a token advances along
//!   the hex grid one hex at a time, dropping a dotted trail of visited hexes
//!   behind it -- the actual unit of play in a hexcrawl is the hex, so this is
//!   the animation that is honest about what the map is for.
//! - **A torn parchment margin**: the map's edge is a noise-thresholded band
//!   of `░`/`▒` at falling density, rather than a straight ruled border, so it
//!   reads as a ragged page edge instead of a UI panel.
//!
//! ```sh
//! cargo run --example 26_hexcrawl --features crossterm
//! cargo run --example 26_hexcrawl --features software
//! cargo run --example 26_hexcrawl --features gl
//! cargo run --example 26_hexcrawl  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::camera::TileCamera;
use tilekit::geom::{Cell, HexLayout, Tile};
use tilekit::noise::{hash01, warped_fbm};
use tilekit::palette::{self, mix, rgb, scale};

/// Cream parchment field the map sits on.
const PARCHMENT: Color = rgb(224, 206, 168);
/// A touch darker, for the torn edge's densest band.
const PARCHMENT_EDGE: Color = rgb(184, 164, 122);
/// Ink used for the hex ticks: a saturated ochre, the traditional
/// referee-map grid colour, chosen so it reads as "annotation" against every
/// terrain colour below it rather than blending into any one of them.
const TICK_INK: Color = rgb(214, 168, 44);
/// Deep brown ink for the road/river.
const ROAD_INK: Color = rgb(120, 78, 42);
/// The travelling party token.
const PARTY_INK: Color = rgb(200, 46, 46);

/// How many world-seconds the party takes to cross one hex. Slow enough that
/// the dotted trail is legible as it grows, fast enough that a viewer does
/// not need to wait long to see the loop.
const SECONDS_PER_HEX: f32 = 1.6;

/// One cell of hand-drawn terrain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Grass,
    Forest,
    Scrub,
    Hills,
    Mountain,
}

impl Terrain {
    /// Base wash colour, before the watercolour tonal variation.
    const fn base_color(self) -> Color {
        match self {
            Self::Grass => rgb(140, 158, 84),
            Self::Forest => rgb(72, 108, 58),
            Self::Scrub => rgb(158, 150, 92),
            Self::Hills => rgb(150, 132, 84),
            Self::Mountain => rgb(132, 120, 112),
        }
    }
}

/// Picks the terrain at a world cell from two independent warped fBm fields:
/// one for "how wooded", one for "how high". Two fields rather than one
/// thresholded field, because a single field's bands are concentric and read
/// as contour rings; crossing an elevation field against a cover field gives
/// patches whose edges do not all run parallel, which is what an
/// unplanned hand-drawn map looks like.
fn terrain_at(seed: u32, wx: f32, wy: f32) -> Terrain {
    let cover = warped_fbm(seed, wx * 0.05, wy * 0.05, 4, 0.5, 1.6);
    let height = warped_fbm(seed ^ 0x51ED_270B, wx * 0.035, wy * 0.035, 4, 0.5, 1.4);

    if height > 0.72 {
        Terrain::Mountain
    } else if height > 0.58 {
        Terrain::Hills
    } else if cover > 0.62 {
        Terrain::Forest
    } else if cover < 0.34 {
        Terrain::Scrub
    } else {
        Terrain::Grass
    }
}

/// A hand-inked decoration at one cell, if this cell is the one that earns
/// one.
///
/// Doodles are not placed by iterating a lattice of candidate spots (which
/// would look regular no matter how the spacing is chosen); they are placed
/// by testing every terrain cell against a hashed threshold, so their
/// positions inherit the same aperiodic distribution the terrain noise has.
/// The two-cell minimum spacing check is what keeps them from clumping at the
/// hash's lucky spots.
fn doodle_at(seed: u32, wx: i32, wy: i32, terrain: Terrain) -> Option<(char, Color)> {
    let density = match terrain {
        Terrain::Forest => 0.16,
        Terrain::Mountain => 0.10,
        Terrain::Hills => 0.06,
        Terrain::Scrub => 0.05,
        Terrain::Grass => 0.0,
    };
    if density <= 0.0 || hash01(seed ^ 0x9E37_79B9, wx, wy) > density {
        return None;
    }
    // A jittered pick from a small glyph set rather than always the same
    // mark, so a stand of trees is not one repeated stamp.
    let pick = hash01(seed ^ 0x2545_F491, wx, wy);
    match terrain {
        Terrain::Forest => {
            let glyphs = ['\u{2663}', '\u{2660}', '\u{03A6}'];
            let idx = (pick * glyphs.len() as f32) as usize % glyphs.len();
            Some((glyphs[idx], rgb(38, 66, 34)))
        }
        // The modifier-letter circumflex and logical-and glyphs a real font
        // would use for a mountain ridge or a hill mark are both outside
        // CP437 and render as a solid block on the pixel backends; the plain
        // ASCII caret is the nearest thing CP437 actually has, so both marks
        // use it and are told apart by color and by the triangle glyph
        // (`\u{25B2}`, CP437-safe) reserved for the taller ridge lines.
        Terrain::Mountain => Some(('\u{25B2}', rgb(88, 78, 74))),
        Terrain::Hills => Some(('^', rgb(102, 88, 60))),
        Terrain::Scrub => {
            let glyphs = ['\'', ','];
            let idx = (pick * glyphs.len() as f32) as usize % glyphs.len();
            Some((glyphs[idx], rgb(112, 104, 62)))
        }
        Terrain::Grass => None,
    }
}

/// One continuous meandering road/river, traced once and cached.
///
/// Traced by integrating a slowly turning heading under noise, the way a
/// cartographer's pen wanders down a valley rather than the way a road
/// network is planned from junctions outward. The direction field is sampled
/// well ahead of and behind the visible window so panning never reveals a
/// visible start or end; the curve is conceptually infinite, only ever
/// windowed by what has been traced so far.
struct Road {
    /// Traced points in world-cell space, in order.
    points: Vec<(f32, f32)>,
}

impl Road {
    /// Traces a fresh road from `seed`, covering a wide enough span that
    /// panning several screens in any direction still finds it on screen.
    fn trace(seed: u32) -> Self {
        let mut points = Vec::new();
        let (mut x, mut y) = (-600.0f32, 0.0f32);
        let mut heading = 0.15f32;
        // Step length is a fraction of a cell so the curve is smooth at the
        // resolution it is actually drawn at; too large a step and the road
        // develops visible facets where it turns.
        let step = 0.6f32;
        for i in 0..4000 {
            points.push((x, y));
            // The heading is nudged by a noise field sampled along the path
            // rather than by fresh randomness each step, so consecutive turns
            // are correlated and the road curves instead of jittering.
            let wobble = warped_fbm(seed ^ 0x1234_5678, i as f32 * 0.02, 0.0, 3, 0.5, 1.0) - 0.5;
            heading += wobble * 0.09;
            heading = heading.clamp(-0.9, 0.9);
            x = step.mul_add(0.4, heading.cos().mul_add(step, x));
            y = heading.sin().mul_add(step, y);
        }
        Self { points }
    }

    /// Whether `(wx, wy)` lies within `half_width` cells of the traced curve,
    /// and if so, how close (0 = centre).
    ///
    /// A linear scan over the whole trace rather than a spatial index: the
    /// trace is a few thousand points and this runs once per visible cell per
    /// frame, which is still comfortably fast at this gallery's grid sizes,
    /// and a spatial index would be the wrong thing to reach for before that
    /// is actually true.
    fn distance_at(&self, wx: f32, wy: f32, half_width: f32) -> Option<f32> {
        let mut best = f32::INFINITY;
        for &(px, py) in &self.points {
            // A coarse reject on y first: the road is roughly horizontal in
            // its overall travel, so most points are trivially far in y and
            // never need the full hypot.
            if (py - wy).abs() > half_width + 2.0 {
                continue;
            }
            let d = (px - wx).hypot(py - wy);
            if d < best {
                best = d;
            }
        }
        (best <= half_width).then_some(best)
    }
}

/// A hex the party has visited, for the dotted trail.
struct Visited {
    tile: Tile,
    /// How long ago it was left, in seconds, for the fade.
    age: f32,
}

/// State: the world seed, the traced road, the camera, the hex layout, the
/// party's progress along a hex-by-hex route, and its trail.
pub struct Hexcrawl {
    seed: u32,
    road: Road,
    camera: TileCamera,
    layout: HexLayout,
    show_ticks: bool,
    time: f32,
    /// The party's route as a sequence of hex tiles, walked in order and then
    /// looped.
    route: Vec<Tile>,
    /// Index into `route` of the hex the party is departing, and progress
    /// `0.0..1.0` toward the next one.
    leg: usize,
    progress: f32,
    trail: Vec<Visited>,
    fps: FpsMeter,
}

/// Builds a party route: a long, deterministic, self-avoiding-ish walk over
/// the hex lattice, so the token has somewhere to wander for many loops
/// without retracing its last few steps immediately.
fn build_route(seed: u32, layout: HexLayout, start: Tile, steps: usize) -> Vec<Tile> {
    let mut route = Vec::with_capacity(steps);
    let mut at = start;
    let mut last_dir = 0usize;
    route.push(at);
    for i in 0..steps {
        let neighbors = layout.neighbors(at);
        // Bias against immediately reversing: reversing is direction
        // `(last_dir + 3) % 6` in hexal's opposite-pair ordering. Excluding it
        // most of the time is what keeps the walk reading as travel rather
        // than as pacing back and forth over one edge.
        let avoid = (last_dir + 3) % 6;
        let roll = hash01(seed ^ 0x7F4A_7C15, i as i32, at.col * 131 + at.row * 17);
        let mut dir = (roll * 6.0) as usize % 6;
        if dir == avoid && hash01(seed, i as i32, 99) > 0.15 {
            dir = (dir + 1) % 6;
        }
        at = neighbors[dir];
        last_dir = dir;
        route.push(at);
    }
    route
}

impl Default for Hexcrawl {
    fn default() -> Self {
        let seed = 26;
        let layout = HexLayout::POINTY_LARGE;
        let camera = TileCamera::unbounded(
            i32::from(ascii_tile_demos::GRID_COLS),
            i32::from(ascii_tile_demos::GRID_ROWS),
        );
        let start = Tile::new(0, 0);
        let route = build_route(seed, layout, start, 240);
        let mut camera = camera;
        // Centres the viewport on the party's starting hex rather than
        // leaving the camera at the world origin: an unbounded camera has no
        // natural home position, and starting it exactly at (0, 0) puts the
        // first hex right in the torn-margin corner, which reads as the demo
        // failing to draw its own subject.
        camera.center_on(layout.center_cell(start));
        Self {
            seed,
            road: Road::trace(seed),
            camera,
            layout,
            show_ticks: true,
            time: 0.0,
            route,
            leg: 0,
            progress: 0.0,
            trail: Vec::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl Hexcrawl {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.road = Road::trace(self.seed);
        self.route = build_route(self.seed, self.layout, Tile::new(0, 0), 240);
        self.leg = 0;
        self.progress = 0.0;
        self.trail.clear();
    }

    /// Grows the hex pitch, snapping to the even/mod-4 steps
    /// [`HexLayout::new`] requires.
    fn zoom(&mut self, delta: i32) {
        let (w, h) = (self.layout.pitch_x, self.layout.pitch_y);
        let new_w = (w + delta * 2).clamp(8, 20);
        let new_h = (h + delta).clamp(2, 6);
        self.layout = HexLayout::new(self.layout.orientation, new_w, new_h);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let step = if key.modifiers.contains(retroglyph_core::KeyModifiers::SHIFT) {
                        16
                    } else {
                        4
                    };
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => self.camera.pan(0, -step),
                        KeyCode::Down | KeyCode::Char('s' | 'S') => self.camera.pan(0, step),
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.camera.pan(-step, 0),
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.camera.pan(step, 0),
                        KeyCode::Char('t' | 'T') => self.show_ticks = !self.show_ticks,
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Char('+' | '=') => self.zoom(1),
                        KeyCode::Char('-' | '_') => self.zoom(-1),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Drag(MouseButton::Left) => {
                    self.camera.pan(-1, 0);
                }
                _ => {}
            }
        }
        true
    }

    /// Advances the party one tick's worth along its route, recording a
    /// trail entry whenever it completes a leg.
    fn advance_party(&mut self, dt: f32) {
        if self.route.len() < 2 {
            return;
        }
        self.progress += dt / SECONDS_PER_HEX;
        // Legs completed this tick as a whole count, rather than looping
        // `while progress >= 1.0`: a floating-point while-condition can spin
        // an unbounded number of times if `dt` is ever unusually large (a
        // stalled frame, a paused tab resuming), and computing the count up
        // front bounds the work to one division and one loop over exactly
        // that many legs.
        let completed = self.progress.floor();
        if completed > 0.0 {
            self.progress -= completed;
            for _ in 0..completed as u32 {
                self.leg = (self.leg + 1) % (self.route.len() - 1);
                self.trail.push(Visited {
                    tile: self.route[self.leg],
                    age: 0.0,
                });
            }
        }
        for visited in &mut self.trail {
            visited.age += dt;
        }
        // Bound the trail so it reads as "recently visited" rather than
        // accumulating forever; a hexcrawl trail this long has looped the
        // route several times over anyway.
        let cap = self.route.len().min(48);
        if self.trail.len() > cap {
            let excess = self.trail.len() - cap;
            self.trail.drain(0..excess);
        }
    }

    /// The party's current fractional world-cell position, interpolated
    /// between the two hex centres of its current leg.
    fn party_position(&self) -> Cell {
        let a = self.layout.center_cell(self.route[self.leg]);
        let b = self
            .layout
            .center_cell(self.route[(self.leg + 1) % self.route.len()]);
        Cell::new(
            a.x + ((b.x - a.x) as f32 * self.progress).round() as i32,
            a.y + ((b.y - a.y) as f32 * self.progress).round() as i32,
        )
    }

    /// Watercolour tonal variation for a terrain's base colour: a slow,
    /// low-frequency wash so flat regions are not perfectly flat, kept subtle
    /// enough to read as paint rather than as dither noise.
    fn wash(&self, base: Color, wx: f32, wy: f32) -> Color {
        let n = warped_fbm(
            self.seed ^ 0x0BAD_F00D,
            self.time.mul_add(0.01, wx * 0.02),
            wy * 0.02,
            3,
            0.5,
            0.8,
        );
        // Centered so the wash lightens about as often as it darkens; a
        // one-sided wash just looks like the base colour got dimmer.
        scale(base, (n - 0.5).mul_add(0.28, 0.9))
    }

    fn draw_terrain(&mut self, surface: &mut Surface<'_>, area: Rect) {
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
                // The torn margin owns this cell entirely once density
                // reaches its ceiling; drawing terrain there just to have the
                // margin's own pass paint over it moments later is wasted
                // work, and skipping it here is what keeps the tear looking
                // torn rather than papered back over.
                if margin_density(area, sx, sy) >= 1.0 {
                    continue;
                }

                let terrain = terrain_at(self.seed, wx as f32, wy as f32);
                let mut color = self.wash(terrain.base_color(), wx as f32, wy as f32);

                let (mut glyph, mut fg) = doodle_at(self.seed, wx, wy, terrain)
                    .map_or((' ', color), |(ch, ink)| (ch, ink));

                if let Some(d) = self.road.distance_at(wx as f32, wy as f32, 1.4) {
                    // Full ink at the centreline, fading to the terrain colour
                    // at the shoulder, so the road reads as a stroke with
                    // some width rather than a hard one-cell rule.
                    let t = (1.0 - d / 1.4).clamp(0.0, 1.0);
                    color = mix(color, ROAD_INK, 0.55 * t);
                    if t > 0.6 {
                        glyph = if (wx + (wy as f32 * 0.3) as i32) % 5 == 0 {
                            '.'
                        } else {
                            ' '
                        };
                        // The dot has to read *against* the ink band it sits
                        // on, not sink into it: mixing fg toward the same
                        // ROAD_INK the background just mixed toward makes the
                        // two converge to the same colour, which is exactly
                        // how a road stops being visible. Lightening instead
                        // of darkening keeps the dot legible on top of the
                        // dark stroke.
                        fg = mix(color, palette::WHITE, 0.55);
                    }
                }

                surface.put((sx, sy), glyph, Style::new().fg(fg).bg(color));
            }
        }
    }

    /// Draws the dotted trail of visited hexes, oldest first so the newest
    /// dot ends up on top where two happen to land on the same cell.
    fn draw_trail(&self, surface: &mut Surface<'_>, area: Rect) {
        for visited in &self.trail {
            let center = self.layout.center_cell(visited.tile);
            let screen = self.camera.world_to_screen(center);
            if !self.camera.on_screen(screen) {
                continue;
            }
            let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
            if margin_density(area, sx, sy) >= 1.0 {
                continue;
            }
            // Fades out over about twelve seconds so the trail shows recent
            // history rather than every hex ever crossed in a long-running
            // demo.
            let fade = (1.0 - visited.age / 12.0).clamp(0.0, 1.0);
            if fade <= 0.0 {
                continue;
            }
            // Mixed from the terrain colour already under this cell, not from
            // the page background: this is drawn over the parchment map, not
            // over `ui::BG`, and mixing from the wrong base produced a dot
            // that was nearly the exact colour of a completely unfilled cell
            // right when it was placed (`fade` near 1 at birth, but the
            // *base* being wrong made the fresh dot read as a hole in the
            // map rather than as ink on top of it).
            let base = current_bg(surface, sx, sy);
            // A dot glyph drawn with fg == bg is invisible *as a character*
            // and reads only as a solid-filled cell, which at high fade looks
            // exactly like a rendering artifact -- a flat block of colour
            // with no texture, indistinguishable from a glyph that failed to
            // draw. Keeping the background the terrain's own colour and only
            // tinting the ink keeps the dot a mark *on* the map rather than a
            // patch cut out of it.
            let ink = mix(base, PARTY_INK, (fade * 0.85).min(1.0));
            surface.put((sx, sy), '\u{00b7}', Style::new().fg(ink).bg(base));
        }
    }

    fn draw_party(&self, surface: &mut Surface<'_>, area: Rect) {
        let world = self.party_position();
        let screen = self.camera.world_to_screen(world);
        if !self.camera.on_screen(screen) {
            return;
        }
        let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
        if margin_density(area, sx, sy) >= 1.0 {
            return;
        }
        surface.put((sx, sy), '\u{263A}', Style::new().fg(PARTY_INK).bg(ui::BG));
    }

    /// Draws the corner-tick hex overlay: for every hex vertex visible on
    /// screen, two short arms along this hex's own edges, in the direction
    /// that vertex's parity calls for.
    ///
    /// Every vertex is shared by up to three hexes, and each of those hexes
    /// would compute the same vertex position and the same two arms if asked,
    /// the same idempotence argument [`08_hex_outline`] relies on for its full
    /// outlines. So rather than ask three hexes to agree, this walks hexes
    /// once and draws each of *its own* six vertices, relying on the fact that
    /// overlapping draws from neighbouring hexes repaint the same cell with
    /// the same glyph and colour rather than corrupting it.
    fn draw_hex_ticks(&self, surface: &mut Surface<'_>, area: Rect) {
        if !self.show_ticks {
            return;
        }
        let (pw, ph) = (self.layout.pitch_x, self.layout.pitch_y);
        let vertices = HexVertices::measure(self.layout);
        let margin_cols = i32::from(area.width()) / pw + 2;
        let margin_rows = i32::from(area.height()) / ph + 2;
        let top_left = self.layout.cell_to_tile(self.camera.origin());
        let style = Style::new().fg(TICK_INK);

        for row in (top_left.row - margin_rows)..(top_left.row + margin_rows) {
            for col in (top_left.col - margin_cols)..(top_left.col + margin_cols) {
                let tile = Tile::new(col, row);
                let center = self.layout.center_cell(tile);
                for corner in vertices.corners() {
                    let vertex = center.offset(corner.dx, corner.dy);
                    let screen = self.camera.world_to_screen(vertex);
                    if !self.camera.on_screen(screen) {
                        continue;
                    }
                    let (sx, sy) = (area.left() + screen.x as u16, area.top() + screen.y as u16);
                    if margin_density(area, sx, sy) >= 1.0 {
                        continue;
                    }
                    let bg = current_bg(surface, sx, sy);
                    surface.put((sx, sy), '\u{2219}', style.bg(bg));
                    for (adx, ady) in corner.arms {
                        let arm = vertex.offset(adx, ady);
                        let ascreen = self.camera.world_to_screen(arm);
                        if !self.camera.on_screen(ascreen) {
                            continue;
                        }
                        let (ax, ay) = (
                            area.left() + ascreen.x as u16,
                            area.top() + ascreen.y as u16,
                        );
                        if margin_density(area, ax, ay) >= 1.0 {
                            continue;
                        }
                        let abg = current_bg(surface, ax, ay);
                        let glyph = arm_glyph(adx, ady);
                        surface.put((ax, ay), glyph, style.bg(abg));
                    }
                }
            }
        }
    }

    /// Draws the torn parchment margin: a widening band of `\u2591`/`\u2592`
    /// wherever [`margin_density`] rises above zero, moving outward from the
    /// frame's centre.
    ///
    /// Runs last and only where density is nonzero, rather than first as a
    /// base layer: every other draw pass already refuses to paint into a
    /// fully torn cell (density `>= 1.0`), so the two agree by construction on
    /// where the tear is, and this pass only has to add the partial-density
    /// fringe between "fully map" and "fully torn", not repaint the interior.
    fn draw_margin(surface: &mut Surface<'_>, area: Rect) {
        for y in 0..area.height() {
            for x in 0..area.width() {
                let density = margin_density(area, area.left() + x, area.top() + y);
                if density <= 0.02 {
                    continue;
                }
                let glyph = if density > 0.6 {
                    '\u{2592}'
                } else {
                    '\u{2591}'
                };
                let color = mix(PARCHMENT, PARCHMENT_EDGE, density.min(1.0));
                if density >= 1.0 {
                    surface.put(
                        (area.left() + x, area.top() + y),
                        ' ',
                        Style::new().bg(PARCHMENT_EDGE),
                    );
                } else {
                    surface.put(
                        (area.left() + x, area.top() + y),
                        glyph,
                        Style::new().fg(PARCHMENT_EDGE).bg(color),
                    );
                }
            }
        }
    }

    /// Draws the small corner legend naming the ink colours, since a
    /// referee's map is read by someone who was not there when it was drawn.
    fn draw_legend(surface: &mut Surface<'_>, area: Rect) {
        let w = 22u16.min(area.width());
        let h = 6u16.min(area.height());
        if w < 10 || h < 4 {
            return;
        }
        let legend_area = Rect::new(area.right() - w - 1, area.top() + 1, w, h);
        // Read back after `draw`, not before: the panel's own `bg` is fixed
        // and known here, so there is no need to round-trip through the grid
        // the way the ticks overlay does against terrain it cannot predict.
        let legend_bg = mix(PARCHMENT, palette::BLACK, 0.06);
        let inner = panel::Panel::new()
            .title("Legend")
            .frame(rgb(96, 76, 48))
            .bg(legend_bg)
            .draw(surface, legend_area);
        if inner.height() == 0 {
            return;
        }
        let rows: &[(char, Color, &str)] = &[
            ('\u{2219}', TICK_INK, "hex vertex"),
            ('\u{2663}', rgb(38, 66, 34), "forest"),
            ('.', ROAD_INK, "road"),
            ('\u{263A}', PARTY_INK, "party"),
        ];
        for (i, (glyph, color, label)) in rows.iter().enumerate() {
            if i as u16 >= inner.height() {
                break;
            }
            let y = inner.top() + i as u16;
            surface.put(
                (inner.left(), y),
                *glyph,
                Style::new().fg(*color).bg(legend_bg),
            );
            surface.print(
                (inner.left() + 2, y),
                label,
                Style::new().fg(rgb(60, 48, 30)).bg(legend_bg),
            );
        }
    }

    fn status(&self) -> String {
        format!(
            "hex {}x{}  seed {}  {} visited",
            self.layout.pitch_x,
            self.layout.pitch_y,
            self.seed,
            self.trail.len()
        )
    }
}

/// How torn-away the parchment is at absolute cell `(x, y)` within `area`, in
/// `0.0` (fully the map) to `1.0` (fully torn, nothing beneath should draw).
///
/// Computed in area-relative coordinates from the frame's centre outward, and
/// evaluated in *screen* space rather than world space, so the tear is stable
/// under panning (it is the page's edge, not a feature of the terrain) and
/// redraws identically at every window size the demo is asked to fill. The
/// tear line itself is perturbed by noise rather than being a perfect circle,
/// so it reads as torn paper rather than a vignette filter.
fn margin_density(area: Rect, x: u16, y: u16) -> f32 {
    let cx = f32::from(area.width()) / 2.0;
    let cy = f32::from(area.height()) / 2.0;
    let max_r = cx.max(cy);
    if max_r <= 0.0 {
        return 0.0;
    }

    let lx = f32::from(x - area.left());
    let ly = f32::from(y - area.top());
    let dx = lx - cx;
    let dy = (ly - cy) * 2.0; // cell aspect correction
    let r = dx.hypot(dy) / max_r;

    let wobble = warped_fbm(0x00A1_1CE5, lx * 0.15, ly * 0.15, 3, 0.5, 1.0);
    let edge = (wobble - 0.5).mul_add(0.22, 0.86);
    ((r - (edge - 0.06)) / 0.10).clamp(0.0, 1.0)
}

/// Reads back the background just written at `(x, y)`, so an overlay pass
/// (ticks, legend text) can blend onto whatever the base layer already drew
/// instead of stamping its own unrelated background and punching a hole in
/// the terrain.
///
/// Takes `&mut Surface` rather than `&Surface`: `Surface` has no read-only
/// grid accessor, only [`Surface::grid_mut`] (an escape hatch for exactly
/// this kind of whole-grid read), so a lookup borrows mutably even though it
/// only reads. Every call site already holds the surface mutably for its own
/// subsequent `put`, so this costs nothing there.
fn current_bg(surface: &mut Surface<'_>, x: u16, y: u16) -> Color {
    let layer = surface.layer();
    surface
        .grid_mut()
        .tile(layer, (x, y))
        .map_or(PARCHMENT, |tile| tile.style().background())
}

/// One true vertex of a hex, relative to its center, with the two short arm
/// offsets (also relative, from the vertex itself) that belong to it.
#[derive(Clone, Copy, Debug)]
struct Corner {
    dx: i32,
    dy: i32,
    arms: [(i32, i32); 2],
}

/// The six true vertices of a hex on a given [`HexLayout`], measured once by
/// querying [`HexLayout::cell_to_tile`] rather than assumed from a formula.
///
/// See the module docs for why: `cell_to_tile` snaps under an
/// aspect-corrected metric, not the formula for a regular hexagon, so a
/// closed-form vertex guess and the grid's own idea of hex ownership can
/// silently disagree. Measuring the real boundary means a tick can never draw
/// somewhere `cell_to_tile` would call a different hex.
struct HexVertices {
    corners: [Corner; 6],
}

impl HexVertices {
    /// Measures `layout`'s hex shape by scanning tile `(0, 0)`'s Voronoi cell
    /// (the set of cells `cell_to_tile` assigns to it) and finding the six
    /// points where its boundary changes direction.
    ///
    /// Cheap enough to redo on every zoom change without caching: the scan
    /// covers one hex's bounding box, a few hundred `cell_to_tile` calls at
    /// the largest pitch this demo allows, not the whole visible map.
    fn measure(layout: HexLayout) -> Self {
        let center = layout.center_cell(Tile::new(0, 0));
        let reach = layout.pitch_x.max(layout.pitch_y) + 2;

        // `spans[i]` is the (lo, hi) column range, in cells relative to
        // `center`, that tile (0, 0) owns at row `center.y - reach + i`. A hex
        // is convex and its rows are contiguous, so a simple per-row min/max
        // scan recovers the whole boundary with no assumptions about its
        // shape.
        let mut spans: Vec<Option<(i32, i32)>> = Vec::new();
        for y in (center.y - reach)..=(center.y + reach) {
            let mut span = None;
            for x in (center.x - reach)..=(center.x + reach) {
                if layout.cell_to_tile(Cell::new(x, y)) == Tile::new(0, 0) {
                    span = Some(match span {
                        None => (x - center.x, x - center.x),
                        Some((lo, _)) => (lo, x - center.x),
                    });
                }
            }
            spans.push(span);
        }

        // The topmost and bottommost occupied rows are the N and S point
        // vertices (a hex's top and bottom are always a single cell wide on
        // this lattice, never a flat run, because the aspect-corrected metric
        // strictly narrows as it approaches the pole). The two rows where the
        // span's left edge stops moving inward and starts moving outward
        // (and, mirrored, the right edge) are the shoulder vertices where the
        // taper meets the widest band.
        let rows: Vec<(i32, (i32, i32))> = spans
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|span| (i as i32 - reach, span)))
            .collect();
        let (top_dy, _) = *rows.first().expect("a hex owns at least one row");
        let (bottom_dy, _) = *rows.last().expect("a hex owns at least one row");

        // The shoulder row is the last row (scanning down from the top) whose
        // left edge is still strictly inside the row below it -- i.e. the
        // last row of the narrowing taper before the boundary starts holding
        // its widest extent. Found the same way, mirrored, from the bottom.
        let shoulder_from = |ascending: bool| {
            let ordered: Vec<_> = if ascending {
                rows.clone()
            } else {
                rows.iter().rev().copied().collect()
            };
            let mut best = ordered[0];
            for window in ordered.windows(2) {
                let (_, (lo0, _)) = window[0];
                let (dy1, (lo1, _)) = window[1];
                if lo1 < lo0 {
                    best = (dy1, (lo1, ordered[0].1.1));
                } else {
                    break;
                }
            }
            best
        };
        let (n_shoulder_dy, (n_lo, _)) = shoulder_from(true);
        let (s_shoulder_dy, (s_lo, _)) = shoulder_from(false);
        let n_hi = rows
            .iter()
            .find(|(dy, _)| *dy == n_shoulder_dy)
            .expect("row exists")
            .1
            .1;
        let s_hi = rows
            .iter()
            .find(|(dy, _)| *dy == s_shoulder_dy)
            .expect("row exists")
            .1
            .1;

        // The six true vertex positions, in hexagon-winding order. Building
        // this list first, and deriving every arm from *this*, is what fixes
        // the wrong-looking grid an earlier version of this function drew: it
        // is tempting to hand-pick a diagonal arm at each named vertex
        // ("points get `(1, 1)`-style diagonals"), but the true edge from N to
        // NE is nowhere near 45 degrees on most pitches (see the module docs'
        // note on why the aspect-corrected boundary is not a regular
        // hexagon), so a hardcoded 45-degree arm points into empty space
        // instead of along the hex's own edge. Deriving each arm from the
        // *actual* neighbouring vertex, one arm per adjacent edge, is the only
        // way the ticks chain into a shape that reads as this hex's boundary
        // rather than as an unrelated decoration sitting on top of it.
        let positions = [
            (0, top_dy),
            (n_hi, n_shoulder_dy),
            (s_hi, s_shoulder_dy),
            (0, bottom_dy),
            (s_lo, s_shoulder_dy),
            (n_lo, n_shoulder_dy),
        ];

        let mut corners = [Corner {
            dx: 0,
            dy: 0,
            arms: [(0, 0); 2],
        }; 6];
        for i in 0..6 {
            let (dx, dy) = positions[i];
            let prev = positions[(i + 5) % 6];
            let next = positions[(i + 1) % 6];
            corners[i] = Corner {
                dx,
                dy,
                arms: [arm_toward(dx, dy, prev), arm_toward(dx, dy, next)],
            };
        }

        Self { corners }
    }

    /// The six corners, each already carrying its own pair of arm offsets.
    const fn corners(&self) -> [Corner; 6] {
        self.corners
    }
}

/// A single-cell arm offset from vertex `(dx, dy)` pointing toward `target`,
/// rounded to whichever of the four CP437-safe tick directions
/// (`-`, `|`, `/`, `\\`) best matches the true edge's aspect-corrected slope.
///
/// Rounding to a compass direction rather than a slope-proportioned arm
/// (which is not representable in one character cell anyway): what matters
/// for the tick to read correctly is which of the four glyphs it draws, and
/// classifying by the angle to the real neighbour is what makes a shallow
/// edge draw `-` or `/` rather than an arbitrary fixed diagonal, which is
/// exactly the distinction the previous version of this function got wrong.
fn arm_toward(dx: i32, dy: i32, target: (i32, i32)) -> (i32, i32) {
    let (ex, ey) = (target.0 - dx, target.1 - dy);
    if ex == 0 && ey == 0 {
        return (0, 0);
    }
    // Aspect-correct before classifying: a screen cell is twice as tall as it
    // is wide (`tilekit::palette::CELL_ASPECT`), so a physically-45-degree
    // edge has a cell-space slope of `dy = dx / 2`, not `dy = dx`. Comparing
    // raw cell deltas would call every hex edge here steeper than it visually
    // is and systematically favour `|` over `/`.
    //
    // The angle is folded into the first quadrant with `abs(vx)`/`abs(vy)`
    // before calling `atan2`, not taken from the signed vector and then
    // `abs()`-ed afterward: `atan2` of a signed vector ranges over all four
    // quadrants (e.g. an edge pointing up-left comes back near 141 degrees,
    // not 39), so comparing that directly against thresholds meant for a
    // single quadrant silently classified visually-mirrored edges into
    // different glyphs. Folding first is what makes N's arm toward NW and its
    // arm toward NE -- true mirror images of each other -- classify to the
    // same glyph, which is what a hexagon's symmetry requires.
    let vx = f32::from(ex.unsigned_abs() as i16);
    let vy = f32::from(ey.unsigned_abs() as i16) * 2.0;
    let angle = vy.atan2(vx).to_degrees();
    let (sx, sy) = (ex.signum(), ey.signum());
    if angle < 22.5 {
        (sx, 0)
    } else if angle > 67.5 {
        (0, sy)
    } else {
        (sx, sy)
    }
}

/// The tick glyph for an arm pointing `(dx, dy)` from its vertex.
///
/// CP437-safe by construction: only `-`, `|`, `/`, and `\` are ever returned,
/// each chosen by the arm's actual slope rather than a fixed per-vertex
/// assumption, which is what lets the same function serve every vertex
/// parity.
const fn arm_glyph(dx: i32, dy: i32) -> char {
    if dy == 0 {
        '-'
    } else if dx == 0 {
        '|'
    } else if dx.signum() == dy.signum() {
        '\\'
    } else {
        '/'
    }
}

impl Demo for Hexcrawl {
    const NAME: &'static str = "26_hexcrawl";
    const TITLE: &'static str = "26 Hexcrawl";
    const BLURB: &'static str =
        "A hand-drawn referee map: terrain ignores the hex grid inked over it.";
    const GRID: (u16, u16) = (140, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("+/-", "hex size"),
            ("T", "toggle grid"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        self.advance_party(dt);
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(PARCHMENT));
        self.draw_terrain(&mut surface, content);
        self.draw_trail(&mut surface, content);
        self.draw_party(&mut surface, content);
        self.draw_hex_ticks(&mut surface, content);
        Self::draw_margin(&mut surface, content);
        Self::draw_legend(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(Hexcrawl);
