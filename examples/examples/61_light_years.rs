//! 61: Light Years -- Frontier: Elite II's Short Range Chart and Galactic
//! Map, adapted around a parallel projection of a 3D star volume.
//!
//! Every other map in this gallery is drawn from a 2D world. This one is not:
//! each star lives at `(x, y, z)` in light years, and the screen position is
//! a rotatable orthographic projection of that point. A plain scatter of dots
//! from such a projection is unreadable as a volume -- two stars at the same
//! screen position could be light years apart in depth, or right next to each
//! other -- so every star grows a **Z-stalk**: a vertical line down (or up) to
//! its own footprint on the galactic plane (`z = 0`). The stalk is what turns
//! a scatter into a volume, and rotating the camera is the proof: watch stars
//! that looked coplanar separate as soon as the elevation changes.
//!
//! Techniques on show:
//!
//! - **Parallel (orthographic) projection with a rotatable camera**
//!   ([`project`]): a yaw around the vertical galactic axis, then a pitch
//!   that tilts the plane away from the viewer, dropping the resulting depth
//!   component. This is the axonometric projection every 3D game world map
//!   since Elite (1984) has used, because it preserves parallel lines and
//!   relative scale exactly, unlike a perspective camera that would make
//!   distant sectors of the chart shrink for no navigational reason.
//! - **Z-stalks** ([`LightYears::draw_star`]): each star projects twice, once
//!   at its own height and once with `z` forced to `0`; the two share a
//!   screen column because yaw and pitch never move a point sideways when
//!   only its height changes, so the stalk is always a single vertical run of
//!   [`tilekit::autotile::box_glyph`]'s `│`, no diagonal rasterization needed.
//! - **Painter's-algorithm depth sort**: stars are drawn far-to-near by the
//!   camera-space depth [`project`] returns, so a near star's dot and label
//!   correctly overwrite a far star's stalk where they overlap on screen.
//! - **Rasterizing a shape onto cells at a regular stride**
//!   ([`LightYears::draw_range_circle`], [`LightYears::draw_sector_grid`]):
//!   the dotted jump-range circle and the galactic map's sector grid are both
//!   parametric shapes sampled at even steps and stamped onto whatever cell
//!   each sample lands in, the standard way to make a guide read as a guide
//!   rather than as content (see Red Blob Games' notes on line and circle
//!   rasterization for the general technique).
//! - **Spectral classification**: color and glyph size are driven by a
//!   star's class (O B A F G K M, blue through red) and luminosity, the same
//!   Morgan-Keenan scheme the reference game's system-info screen quotes
//!   verbatim ("Type 'K' orange star").
//!
//! ```sh
//! cargo run --example 61_light_years --features crossterm
//! cargo run --example 61_light_years --features software
//! cargo run --example 61_light_years --features gl
//! cargo run --example 61_light_years  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, KeyModifiers, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::Panel;
use ascii_tile_demos::ui::touch::Shape;
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{self, mix, rgb, scale};

/// System names, real Elite/Frontier ones from the reference screenshots.
/// Index 0 is always the current location ("Lave" in the reference short
/// range chart).
const NAMES: [&str; 24] = [
    "Lave", "Leesti", "Diso", "Zaonce", "Riedquat", "Orerve", "Reorte", "Enge", "Andiceb",
    "Isinor", "Ceerdi", "Biarge", "Errora", "Inbibe", "Xeer", "Usleri", "Arden", "Meso",
    "Tionisla", "Isis", "Onrira", "Soleddor", "Aganippe", "Zeessze",
];

/// Extent of the generated volume on each plane axis, in light years. Large
/// enough that stars visibly separate in depth once the camera tilts, small
/// enough that a 7 ly jump range circle still reads as local rather than
/// microscopic.
const PLANE_EXTENT: f32 = 18.0;
/// Extent on the height axis. The galaxy's stellar disk is much thinner than
/// it is wide, which is exactly the asymmetry that makes the Z-stalk
/// technique necessary: without it a flattened-but-nonzero spread of heights
/// would be nearly invisible in a flat top-down view.
const HEIGHT_EXTENT: f32 = 6.0;

/// The current ship's jump range, in light years. Drives the dotted range
/// circle on the short range chart.
const JUMP_RANGE: f32 = 7.0;

/// Sector grid stride on the galactic map, in light years.
const SECTOR_STEP: f32 = 10.0;

/// Camera elevation is clamped to this range, in radians above the plane's
/// own horizon (`0` = edge-on, `PI/2` = looking straight down). Below the low
/// end the plane collapses to a line and the grid and range circle stop
/// being legible as shapes; above the high end the view is close enough to
/// top-down that the stalks compress into their own dot and the third
/// dimension the whole demo exists to show stops reading.
const ELEVATION_RANGE: (f32, f32) = (0.12, 1.25);

/// One of the seven Morgan-Keenan spectral classes, ordered hottest to
/// coolest as the reference material always lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SpectralClass {
    O,
    B,
    A,
    F,
    G,
    K,
    M,
}

impl SpectralClass {
    /// Picks a class from a `0..1` draw, weighted toward the cool end: real
    /// stellar populations are overwhelmingly M dwarfs and O stars are rare
    /// by design, which also keeps the chart from being wall-to-wall blue.
    fn from_draw(t: f32) -> Self {
        match t {
            t if t < 0.02 => Self::O,
            t if t < 0.06 => Self::B,
            t if t < 0.13 => Self::A,
            t if t < 0.24 => Self::F,
            t if t < 0.42 => Self::G,
            t if t < 0.68 => Self::K,
            _ => Self::M,
        }
    }

    /// Body color, blue through red, matching the reference galactic map's
    /// per-class star coloring.
    const fn color(self) -> Color {
        match self {
            Self::O => rgb(155, 176, 255),
            Self::B => rgb(170, 191, 255),
            Self::A => rgb(202, 215, 255),
            Self::F => rgb(248, 247, 235),
            Self::G => rgb(255, 244, 180),
            Self::K => rgb(255, 178, 100),
            Self::M => rgb(255, 124, 90),
        }
    }

    /// Relative luminosity, `0..1`. Not physically accurate (a real O star
    /// outshines an M dwarf by a factor of millions), just enough spread that
    /// [`Self::glyph`] picks visibly different sizes across the class range.
    const fn luminosity(self) -> f32 {
        match self {
            Self::O => 1.00,
            Self::B => 0.86,
            Self::A => 0.68,
            Self::F => 0.52,
            Self::G => 0.38,
            Self::K => 0.26,
            Self::M => 0.16,
        }
    }

    /// Glyph sized by luminosity: a single pixel up to a fat blob, the same
    /// range the reference short range chart uses for star size. `\u{263c}`
    /// (`SUN`) is CP437 0x0F and reads as a filled disc with rays, the
    /// biggest mark available without reaching for a non-CP437 block.
    fn glyph(self) -> char {
        let l = self.luminosity();
        if l > 0.75 {
            '\u{263c}' // ☼ SUN, CP437 0x0F
        } else if l > 0.45 {
            '*'
        } else if l > 0.22 {
            '\u{2219}' // ∙ BULLET OPERATOR, CP437 0xF9
        } else {
            '\u{00b7}' // · MIDDLE DOT, CP437 0xFA
        }
    }

    /// The one-letter class tag the reference system-info line quotes, e.g.
    /// `Type 'K' orange star`.
    const fn letter(self) -> char {
        match self {
            Self::O => 'O',
            Self::B => 'B',
            Self::A => 'A',
            Self::F => 'F',
            Self::G => 'G',
            Self::K => 'K',
            Self::M => 'M',
        }
    }

    /// The color name the reference description line spells out in prose.
    const fn color_name(self) -> &'static str {
        match self {
            Self::O | Self::B => "blue",
            Self::A | Self::F => "white",
            Self::G => "yellow",
            Self::K => "orange",
            Self::M => "red",
        }
    }
}

/// One star system: a 3D position in light years, a spectral class, and a
/// name.
struct Star {
    x: f32,
    y: f32,
    z: f32,
    class: SpectralClass,
    name: &'static str,
}

/// World description lines, cycled by system index so every star reads as
/// having its own place without needing real per-planet content.
const DESCRIPTIONS: [&str; 6] = [
    "Frontier outdoor world. Some farming and tourism.",
    "High technology, agricultural exports to nearby systems.",
    "Poor industrial world under corporate charter.",
    "Rich agricultural world, a major food producer.",
    "Low technology world, mostly extraction and mining.",
    "Multi-government world, seat of a regional trade court.",
];

/// Generates a deterministic volume of stars from a seed. Index 0 is always
/// the home system, fixed at the origin so the range circle and the sector
/// label have a stable center to read from.
fn generate_stars(seed: u32) -> Vec<Star> {
    let mut stars = Vec::with_capacity(NAMES.len());
    for (i, &name) in NAMES.iter().enumerate() {
        let idx = i as i32;
        if i == 0 {
            stars.push(Star {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                class: SpectralClass::G,
                name,
            });
            continue;
        }
        // Four independent hashes per star (x, y, z, class) so the axes
        // don't correlate; sharing one hash across axes would visibly line
        // stars up along a diagonal.
        let x = (hash01(seed, idx, 1) - 0.5) * 2.0 * PLANE_EXTENT;
        let y = (hash01(seed, idx, 2) - 0.5) * 2.0 * PLANE_EXTENT;
        let z = (hash01(seed, idx, 3) - 0.5) * 2.0 * HEIGHT_EXTENT;
        let class = SpectralClass::from_draw(hash01(seed, idx, 4));
        stars.push(Star {
            x,
            y,
            z,
            class,
            name,
        });
    }
    stars
}

/// Which reference screen is on show. They share the star data and the
/// projection; only the guide overlay and the zoom differ.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    ShortRange,
    Galactic,
}

/// Coarse time-acceleration steps a transport control click cycles through,
/// mirroring the reference's pause/play/fast-forward row.
const TIME_STEPS: [f32; 5] = [0.0, 1.0, 4.0, 16.0, 64.0];

/// Projects a world point to `(screen_x, screen_y, depth)` under the current
/// camera.
///
/// The rotation is yaw-then-pitch: yaw spins the galactic plane around its
/// own vertical (`z`) axis, pitch then tilts that spun plane away from the
/// viewer. Because pitch only ever mixes `y` and `z`, a point's `x` after yaw
/// is untouched by height, which is the property [`LightYears::draw_star`]
/// depends on to put a star and its `z = 0` footprint in the same screen
/// column. `depth` is the camera-space `y` after both rotations: larger means
/// farther from the viewer, so sorting stars by it and drawing ascending
/// gives a correct painter's-algorithm far-to-near order.
fn project(x: f32, y: f32, z: f32, azimuth: f32, elevation: f32) -> (f32, f32, f32) {
    let (sa, ca) = azimuth.sin_cos();
    let x1 = x.mul_add(ca, -(y * sa));
    let y1 = x.mul_add(sa, y * ca);
    let (se, ce) = elevation.sin_cos();
    let depth = y1.mul_add(ce, -(z * se));
    let up = y1.mul_add(se, z * ce);
    (x1, -up, depth)
}

/// State: the star volume, the camera, the selected system, and the sim
/// clock the bottom chrome strip reports.
pub struct LightYears {
    stars: Vec<Star>,
    seed: u32,
    selected: usize,
    mode: Mode,
    azimuth: f32,
    elevation: f32,
    /// Screen cells per light year. `+`/`-` (and `,`/`.`) zoom.
    scale: f32,
    /// Sim seconds since the reference epoch (`1-Jan-3200 00:00:00`),
    /// advanced by `frame.delta * time_step` and frozen at `time_step == 0`.
    sim_seconds: f64,
    /// Index into [`TIME_STEPS`]. `Space` or a transport-control click
    /// advances it.
    time_step_idx: usize,
    /// Wall-clock seconds, unaffected by pause. Nothing else in this demo is
    /// exempt from the transport controls, so this is the one clock that
    /// keeps the selection ring pulsing even at `time_step == 0` -- proof
    /// the frame is still live, not just idle.
    wall_time: f32,
    fps: FpsMeter,
    // Screen-space bounds of the last drawn chart, used to hit-test a click
    // against the star nearest the pointer.
    last_chart_area: Rect,
}

impl Default for LightYears {
    fn default() -> Self {
        let seed = 7;
        Self {
            stars: generate_stars(seed),
            seed,
            selected: 1,
            mode: Mode::ShortRange,
            azimuth: 0.5,
            elevation: 0.55,
            scale: 1.6,
            sim_seconds: 58.0,
            time_step_idx: 1,
            wall_time: 0.0,
            fps: FpsMeter::new(),
            last_chart_area: Rect::new(0, 0, 0, 0),
        }
    }
}

impl LightYears {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.stars = generate_stars(self.seed);
        self.selected = 1;
    }

    const fn time_step(&self) -> f32 {
        TIME_STEPS[self.time_step_idx]
    }

    const fn cycle_time_step(&mut self) {
        self.time_step_idx = (self.time_step_idx + 1) % TIME_STEPS.len();
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let fast = key.modifiers.contains(KeyModifiers::SHIFT);
                    let step = if fast { 0.12 } else { 0.04 };
                    match key.code {
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.azimuth -= step * 4.0,
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.azimuth += step * 4.0,
                        KeyCode::Up | KeyCode::Char('w' | 'W') => {
                            self.elevation =
                                (self.elevation - step).clamp(ELEVATION_RANGE.0, ELEVATION_RANGE.1);
                        }
                        KeyCode::Down | KeyCode::Char('s' | 'S') => {
                            self.elevation =
                                (self.elevation + step).clamp(ELEVATION_RANGE.0, ELEVATION_RANGE.1);
                        }
                        KeyCode::Char('+' | '=') => self.scale = (self.scale * 1.1).min(8.0),
                        KeyCode::Char('-' | '_') => self.scale = (self.scale / 1.1).max(0.4),
                        KeyCode::Tab | KeyCode::Char('m' | 'M') => {
                            self.mode = match self.mode {
                                Mode::ShortRange => Mode::Galactic,
                                Mode::Galactic => Mode::ShortRange,
                            };
                        }
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Char(' ') => self.cycle_time_step(),
                        KeyCode::Char('[' | ',') => {
                            self.selected =
                                (self.selected + self.stars.len() - 1) % self.stars.len();
                        }
                        KeyCode::Char(']' | '.') => {
                            self.selected = (self.selected + 1) % self.stars.len();
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
        if !matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        let area = self.last_chart_area;
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cx = f32::from(area.left() + area.width() / 2);
        let cy = f32::from(area.top() + area.height() / 2);
        let click_x = f32::from(pos.x);
        let click_y = f32::from(pos.y);

        // Nearest star to the click in *projected* screen space, not world
        // space: a far star drawn near the cursor is what the player is
        // actually pointing at, whatever its true depth.
        let mut best: Option<(usize, f32)> = None;
        for (i, star) in self.stars.iter().enumerate() {
            let (sx, sy, _) = project(star.x, star.y, star.z, self.azimuth, self.elevation);
            let px = sx.mul_add(self.scale, cx);
            let py = (sy * self.scale).mul_add(0.5, cy);
            let d = (px - click_x).hypot(py - click_y);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        if let Some((i, d)) = best
            && d < 3.0
        {
            self.selected = i;
        }
    }

    /// Projects a star (or its `z = 0` footprint) to a screen cell inside
    /// `area`, centered on `origin`. Screen `y` is halved relative to `x`
    /// before the cell aspect ratio widens it back out at 0.5, since a
    /// terminal cell is roughly twice as tall as it is wide and an
    /// unadjusted projection would draw the volume visibly stretched
    /// vertically.
    fn project_to_screen(&self, area: Rect, x: f32, y: f32, z: f32) -> (i32, i32, f32) {
        let (sx, sy, depth) = project(x, y, z, self.azimuth, self.elevation);
        let cx = i32::from(area.left()) + i32::from(area.width()) / 2;
        let cy = i32::from(area.top()) + i32::from(area.height()) / 2;
        let px = cx + (sx * self.scale).round() as i32;
        let py = cy + (sy * self.scale * 0.5).round() as i32;
        (px, py, depth)
    }

    /// Draws one star's geometry: its Z-stalk down to the galactic plane, a
    /// foot tick, and the body glyph sized and colored by spectral class.
    ///
    /// Names are a separate pass ([`draw_star_label`](Self::draw_star_label)).
    /// Drawing each star complete before starting the next lets a near star's
    /// foot tick land in the middle of a far star's already-drawn name, which
    /// renders as `-D-so` rather than `Diso`. Depth order is the right
    /// priority for the geometry and the wrong one for text, so the two are
    /// split and every label is drawn after every tick.
    fn draw_star(&self, surface: &mut Surface<'_>, area: Rect, star: &Star, is_selected: bool) {
        let (fx, fy, _) = self.project_to_screen(area, star.x, star.y, 0.0);
        let (sx, sy, _) = self.project_to_screen(area, star.x, star.y, star.z);
        let color = star.class.color();
        let dim = scale(color, 0.35);

        // The stalk shares fx == sx by construction (see `project`'s doc
        // comment), so this is a single vertical run, never a diagonal.
        if fy != sy {
            let (lo, hi) = (fy.min(sy), fy.max(sy));
            for y in (lo + 1)..hi {
                put_clipped(surface, area, sx, y, '\u{2502}', Style::new().fg(dim));
            }
            // Foot tick: a short horizontal mark on the plane, exactly as
            // the reference chart draws under Lave.
            put_clipped(surface, area, fx - 1, fy, '\u{2500}', Style::new().fg(dim));
            put_clipped(surface, area, fx + 1, fy, '\u{2500}', Style::new().fg(dim));
        }

        let glyph = star.class.glyph();
        let body_color = if is_selected { palette::WHITE } else { color };
        put_clipped(surface, area, sx, sy, glyph, Style::new().fg(body_color));

        if is_selected {
            // The pulsing ring is the one thing in this demo that moves with
            // zero input and independent of the transport controls: proof
            // the frame is live even paused.
            let pulse = (self.wall_time * 2.4).sin().mul_add(0.5, 0.5);
            let ring = mix(color, palette::WHITE, pulse);
            put_clipped(surface, area, sx - 1, sy, '(', Style::new().fg(ring));
            put_clipped(surface, area, sx + 1, sy, ')', Style::new().fg(ring));
        }
    }

    /// Draws one star's name, if there is room and the row is not already
    /// spoken for. Runs as a second pass over every star; see
    /// [`draw_star`](Self::draw_star) for why.
    fn draw_star_label(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        star: &Star,
        labeled_rows: &mut Vec<i32>,
    ) {
        let (sx, sy, _) = self.project_to_screen(area, star.x, star.y, star.z);

        // Skip a row that already has a name, so a dense cluster reads as dots
        // with a few labels rather than a wall of overlapping text.
        if labeled_rows.contains(&sy) {
            return;
        }
        let label = format!(" {}", star.name);
        let max_len = i32::from(area.right()) - (sx + 2);
        if max_len > 2 {
            let text: String = label.chars().take(max_len as usize).collect();
            for (i, ch) in text.chars().enumerate() {
                put_clipped(
                    surface,
                    area,
                    sx + 2 + i as i32,
                    sy,
                    ch,
                    Style::new().fg(star.class.color()),
                );
            }
            labeled_rows.push(sy);
        }
    }

    /// Depth-sorted star order, far to near, for painter's-algorithm
    /// drawing. Sorting by a float key with `total_cmp` rather than
    /// `partial_cmp` keeps this deterministic even if a depth ever lands
    /// exactly on a tie or (it never will here, but the type doesn't know
    /// that) NaN.
    fn depth_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.stars.len()).collect();
        let depth_of = |i: usize| {
            let s = &self.stars[i];
            project(s.x, s.y, s.z, self.azimuth, self.elevation).2
        };
        order.sort_by(|&a, &b| depth_of(b).total_cmp(&depth_of(a)));
        order
    }

    /// The dotted jump-range circle on the short range chart: [`JUMP_RANGE`]
    /// light years, sampled at a fixed angular stride and stamped onto
    /// whichever cell each sample lands in.
    fn draw_range_circle(&self, surface: &mut Surface<'_>, area: Rect) {
        const SAMPLES: usize = 96;
        let color = rgb(90, 96, 116);
        for i in 0..SAMPLES {
            // Every third sample only: a solid ring would compete with the
            // stars sitting on it, a sparse dotted one reads as a guide.
            if i % 3 != 0 {
                continue;
            }
            let theta = (i as f32 / SAMPLES as f32) * std::f32::consts::TAU;
            let (x, y) = (JUMP_RANGE * theta.cos(), JUMP_RANGE * theta.sin());
            let (px, py, _) = self.project_to_screen(area, x, y, 0.0);
            put_clipped(surface, area, px, py, '\u{00b7}', Style::new().fg(color));
        }
    }

    /// Small crosshair marking the current position (the home system, always
    /// on the `z = 0` plane).
    fn draw_position_crosshair(&self, surface: &mut Surface<'_>, area: Rect) {
        let (px, py, _) = self.project_to_screen(area, 0.0, 0.0, 0.0);
        let color = palette::WHITE;
        put_clipped(
            surface,
            area,
            px - 2,
            py,
            '\u{2500}',
            Style::new().fg(color),
        );
        put_clipped(
            surface,
            area,
            px + 2,
            py,
            '\u{2500}',
            Style::new().fg(color),
        );
        put_clipped(
            surface,
            area,
            px,
            py.saturating_sub(1),
            '|',
            Style::new().fg(color),
        );
        put_clipped(surface, area, px, py + 1, '|', Style::new().fg(color));
    }

    /// Bright green sector grid on the galactic map: vertical and horizontal
    /// lines every [`SECTOR_STEP`] light years across the visible plane,
    /// each sampled along its length at a stride fine enough to look solid
    /// rather than dotted (the galactic map's grid is a border, not a
    /// range guide, so it earns the denser stride the range circle does not
    /// get).
    fn draw_sector_grid(&self, surface: &mut Surface<'_>, area: Rect) {
        const SAMPLES: usize = 160;
        let color = rgb(40, 150, 70);
        let extent = PLANE_EXTENT * 1.6;
        let lines = (extent / SECTOR_STEP) as i32;
        for i in -lines..=lines {
            let fixed = i as f32 * SECTOR_STEP;
            for s in 0..=SAMPLES {
                let t = (s as f32 / SAMPLES as f32).mul_add(2.0 * extent, -extent);
                let (px1, py1, _) = self.project_to_screen(area, fixed, t, 0.0);
                let (px2, py2, _) = self.project_to_screen(area, t, fixed, 0.0);
                put_clipped(surface, area, px1, py1, '\u{00b7}', Style::new().fg(color));
                put_clipped(surface, area, px2, py2, '\u{00b7}', Style::new().fg(color));
            }
        }
    }

    /// Magenta political-boundary arcs: fixed sine curves in screen space,
    /// crossing the panel the way the reference's territory borders cut
    /// across the galactic map without regard to the star field under them.
    fn draw_boundary_arcs(&self, surface: &mut Surface<'_>, area: Rect) {
        let color = rgb(190, 60, 190);
        let w = f32::from(area.width());
        let h = f32::from(area.height());
        for arc in 0..2 {
            let phase = (self.seed as f32).mul_add(0.01, arc as f32 * 2.1);
            let amplitude = h * 0.18;
            let base = h * (arc as f32).mul_add(0.4, 0.3);
            for col in 0..area.width() {
                let t = f32::from(col) / w.max(1.0);
                let y = amplitude.mul_add((t * 5.0 + phase).sin(), base);
                if (f32::from(col) as i32) % 2 == 0 {
                    continue; // dashed, so the grid under it stays legible
                }
                put_clipped(
                    surface,
                    area,
                    i32::from(area.left()) + i32::from(col),
                    i32::from(area.top()) + y as i32,
                    '\u{00b7}',
                    Style::new().fg(color),
                );
            }
        }
    }

    /// Small filled ring around the selected system on the galactic map, in
    /// place of the `+` mark every other star gets.
    fn draw_selection_ring(&self, surface: &mut Surface<'_>, area: Rect) {
        let star = &self.stars[self.selected];
        let (px, py, _) = self.project_to_screen(area, star.x, star.y, star.z);
        let color = rgb(60, 220, 110);
        for (dx, dy) in [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, 1),
            (-1, 1),
            (1, -1),
        ] {
            put_clipped(
                surface,
                area,
                px + dx,
                py + dy,
                '\u{2219}',
                Style::new().fg(color),
            );
        }
    }

    fn draw_chart(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let title = match self.mode {
            Mode::ShortRange => "SHORT RANGE CHART",
            Mode::Galactic => "GALACTIC MAP",
        };
        let bg = match self.mode {
            Mode::ShortRange => palette::BLACK,
            Mode::Galactic => rgb(6, 8, 22),
        };
        let interior = Panel::new()
            .title(title)
            .frame(palette::WHITE)
            .bg(bg)
            .draw(surface, area);
        surface
            .clip(interior)
            .fill_rect(interior, ' ', Style::new().bg(bg));
        self.last_chart_area = interior;
        if interior.width() < 4 || interior.height() < 3 {
            return;
        }

        match self.mode {
            Mode::ShortRange => {
                self.draw_range_circle(surface, interior);
                self.draw_position_crosshair(surface, interior);
            }
            Mode::Galactic => {
                self.draw_sector_grid(surface, interior);
                self.draw_boundary_arcs(surface, interior);
                self.draw_selection_ring(surface, interior);
            }
        }

        // Geometry for every star first, then every label, so no star's foot
        // tick can strike through another star's name. Both passes walk the
        // same depth order, so which name wins a contested row is still the
        // nearer star's, exactly as before.
        let order = self.depth_order();
        for &i in &order {
            self.draw_star(surface, interior, &self.stars[i], i == self.selected);
        }
        let mut labeled_rows = Vec::new();
        for &i in &order {
            self.draw_star_label(surface, interior, &self.stars[i], &mut labeled_rows);
        }

        if self.mode == Mode::Galactic {
            let sx = (self.stars[0].x / SECTOR_STEP).floor() as i32;
            let sy = (self.stars[0].y / SECTOR_STEP).floor() as i32;
            let label = format!("Sector: {sx},{sy}");
            put_str(
                surface,
                interior,
                interior.left(),
                interior.bottom() - 1,
                &label,
                rgb(60, 220, 110),
            );
        }
    }

    /// The side info panel shown on landscape/desktop layouts: a detail card
    /// for the selected system instead of repeating the chart at a second
    /// scale.
    fn draw_info_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let star = &self.stars[self.selected];
        let interior = Panel::new().title("SYSTEM").draw(surface, area);
        if interior.width() < 6 {
            return;
        }
        let dist = star.x.hypot(star.y).hypot(star.z);
        let lines = [
            format!("Name    {}", star.name),
            format!("Class   {}", star.class.letter()),
            format!("Color   {}", star.class.color_name()),
            format!("X       {:+.2} ly", star.x),
            format!("Y       {:+.2} ly", star.y),
            format!("Z       {:+.2} ly", star.z),
            format!("Dist.   {dist:.2} ly"),
        ];
        for (row, line) in lines.iter().enumerate() {
            if row as u16 >= interior.height() {
                break;
            }
            put_str(
                surface,
                interior,
                interior.left(),
                interior.top() + row as u16,
                line,
                ui::FG,
            );
        }
    }

    /// The bottom chrome strip: clock and transport controls, system name
    /// and distance, docked/mode label, description, and the icon row. This
    /// is the densest, most referential part of the reference screens, and
    /// it is deliberately packed rather than simplified.
    fn draw_chrome_strip(&self, surface: &mut Surface<'_>, area: Rect, compact: bool) {
        surface
            .clip(area)
            .fill_rect(area, ' ', Style::new().bg(ui::CHROME_BG));
        if area.height() == 0 {
            return;
        }
        let star = &self.stars[self.selected];
        let dist = star.x.hypot(star.y).hypot(star.z);
        let orange = rgb(230, 150, 70);
        let green = rgb(90, 210, 120);
        let yellow = rgb(230, 210, 90);
        let red = rgb(220, 90, 90);

        let mut row = area.top();
        let clock = format_clock(self.sim_seconds);
        put_str(surface, area, area.left(), row, &clock, orange);

        let transport = transport_glyphs(self.time_step_idx);
        let tx = area.left() + clock.chars().count() as u16 + 2;
        put_str(surface, area, tx, row, &transport, orange);

        let docked = "Galactic Map";
        let docked_x = area
            .right()
            .saturating_sub(docked.chars().count() as u16 + 1);
        put_str(surface, area, docked_x, row, docked, orange);
        row += 1;
        if row >= area.bottom() {
            return;
        }

        put_str(surface, area, area.left(), row, star.name, green);
        let dist_text = format!("Dist. {dist:.2} light years");
        let dx = area.left() + star.name.chars().count() as u16 + 2;
        put_str(surface, area, dx, row, &dist_text, yellow);
        row += 1;
        if compact || row >= area.bottom() {
            return;
        }

        let type_line = format!(
            "Type '{}' {} star",
            star.class.letter(),
            star.class.color_name()
        );
        put_str(surface, area, area.left(), row, &type_line, red);
        row += 1;
        if row >= area.bottom() {
            return;
        }

        let desc = DESCRIPTIONS[self.selected % DESCRIPTIONS.len()];
        put_str(surface, area, area.left(), row, desc, red);
        row += 1;
        if row >= area.bottom() {
            return;
        }

        // Ten small numbered icons, static except that the icon matching the
        // current mode brightens, the same way a real console highlights the
        // panel it is currently showing.
        let mut x = area.left();
        for n in 1..=10u32 {
            let active = (self.mode == Mode::ShortRange && n == 1)
                || (self.mode == Mode::Galactic && n == 2);
            let color = if active { palette::WHITE } else { ui::DIM };
            let label = format!("[{n}]");
            put_str(surface, area, x, row, &label, color);
            x += label.chars().count() as u16 + 1;
            if x >= area.right() {
                break;
            }
        }
    }

    fn status(&self) -> String {
        let star = &self.stars[self.selected];
        format!(
            "{}  seed {}  az {:.0} deg  el {:.0} deg  {}",
            star.name,
            self.seed,
            self.azimuth.to_degrees(),
            self.elevation.to_degrees(),
            if self.mode == Mode::ShortRange {
                "short range"
            } else {
                "galactic"
            },
        )
    }
}

/// `Terminal::put` with clipping to `area` and negative-coordinate safety.
/// `Pos` is `u16`, so a world cell above or left of the viewport still needs
/// this signed check before the call even though `Surface::put` is already a
/// no-op past the right/bottom edge.
fn put_clipped(surface: &mut Surface<'_>, area: Rect, x: i32, y: i32, glyph: char, style: Style) {
    if x >= i32::from(area.left())
        && x < i32::from(area.right())
        && y >= i32::from(area.top())
        && y < i32::from(area.bottom())
    {
        surface.put((x as u16, y as u16), glyph, style);
    }
}

/// Draws a left-aligned string, truncated to whatever fits inside `area`
/// from `x`.
fn put_str(surface: &mut Surface<'_>, area: Rect, x: u16, y: u16, text: &str, color: Color) {
    for (i, ch) in text.chars().enumerate() {
        put_clipped(
            surface,
            area,
            i32::from(x) + i as i32,
            i32::from(y),
            ch,
            Style::new().fg(color),
        );
    }
}

/// Formats sim seconds since the reference epoch as `HH:MM:SS D-Mon-YYYY`,
/// matching the reference chrome strip's clock exactly.
fn format_clock(sim_seconds: f64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let total = sim_seconds.max(0.0) as u64;
    let s = total % 60;
    let m = (total / 60) % 60;
    let h = (total / 3600) % 24;
    let day_index = total / 86400;
    let day_of_year = day_index % 365;
    let year = 3200 + day_index / 365;
    let month = (day_of_year / 30).min(11);
    let day = (day_of_year % 30) + 1;
    format!(
        "{h:02}:{m:02}:{s:02} {day}-{}-{year}",
        MONTHS[month as usize]
    )
}

/// CP437 transport-control row: pause bars, a play triangle, and up to three
/// fast-forward chevrons, with the active control brightened.
fn transport_glyphs(step_idx: usize) -> String {
    let mut s = String::new();
    s.push(if step_idx == 0 { '\u{258c}' } else { ' ' }); // ▌ pause, CP437 0xDD
    s.push(if step_idx == 0 { '\u{2590}' } else { ' ' }); // ▐ pause, CP437 0xDE
    s.push(' ');
    // Active controls get the CP437 triangle and guillemet; inactive ones fall
    // back to ASCII rather than to their "small" Unicode cousins. U+2023
    // (triangular bullet) and U+203A (single angle quote) look like the right
    // answer and are not: neither is in CP437 nor in the block sheet's
    // codepage, so both draw as a solid rectangle on the pixel backends. The
    // row is drawn in one colour, so shape is the only thing distinguishing
    // active from inactive, which is why this is not just a dimmed repeat.
    s.push(if step_idx == 1 { '\u{25ba}' } else { '>' }); // ► play
    s.push(' ');
    for level in 2..TIME_STEPS.len() {
        s.push(if step_idx == level { '\u{00bb}' } else { '>' }); // » fast forward
    }
    s
}

impl Demo for LightYears {
    const NAME: &'static str = "61_light_years";
    const TITLE: &'static str = "61 Light Years";
    const BLURB: &'static str = "Frontier: Elite II -- a projected 3D star volume with Z-stalks.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "rotate camera"),
            ("+/-", "zoom"),
            ("[ / ]", "select system"),
            ("Tab/M", "chart/map"),
            ("Space", "time speed"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.wall_time += dt;
        self.sim_seconds = f64::from(dt).mul_add(f64::from(self.time_step()), self.sim_seconds);
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        let chrome_h: u16 = if shape.stacks() { 3 } else { 5 };
        let chrome_h = chrome_h.min(content.height().saturating_sub(4));
        let chart_bottom = content.bottom().saturating_sub(chrome_h);
        let main = Rect::new(
            content.left(),
            content.top(),
            content.width(),
            chart_bottom.saturating_sub(content.top()),
        );
        let chrome_area = Rect::new(content.left(), chart_bottom, content.width(), chrome_h);

        if shape.stacks() {
            self.draw_chart(&mut surface, main);
        } else {
            let side_w = (main.width() / 4)
                .clamp(18, 30)
                .min(main.width().saturating_sub(20));
            if side_w >= 18 && main.width() > side_w + 20 {
                let chart_w = main.width() - side_w;
                let chart_area = Rect::new(main.left(), main.top(), chart_w, main.height());
                let side_area = Rect::new(main.left() + chart_w, main.top(), side_w, main.height());
                self.draw_chart(&mut surface, chart_area);
                self.draw_info_panel(&mut surface, side_area);
            } else {
                self.draw_chart(&mut surface, main);
            }
        }

        self.draw_chrome_strip(&mut surface, chrome_area, shape.stacks());
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(LightYears);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_system_name_is_unique() {
        use std::collections::HashSet;
        let names: HashSet<_> = NAMES.iter().collect();
        assert_eq!(names.len(), NAMES.len(), "duplicate system name");
    }

    #[test]
    fn home_system_is_always_at_the_origin() {
        let stars = generate_stars(42);
        assert_eq!((stars[0].x, stars[0].y, stars[0].z), (0.0, 0.0, 0.0));
    }

    #[test]
    fn star_generation_is_deterministic() {
        let a = generate_stars(11);
        let b = generate_stars(11);
        for (sa, sb) in a.iter().zip(b.iter()) {
            assert_eq!((sa.x, sa.y, sa.z), (sb.x, sb.y, sb.z));
        }
    }

    #[test]
    fn a_star_and_its_footprint_share_a_screen_column() {
        // The Z-stalk is only ever a vertical run if this holds for every
        // camera angle, not just the default one.
        for azimuth in [0.0, 0.7, 1.9, 4.4] {
            for elevation in [0.15, 0.5, 1.1] {
                let (sx, _, _) = project(3.0, -4.0, 2.5, azimuth, elevation);
                let (fx, _, _) = project(3.0, -4.0, 0.0, azimuth, elevation);
                assert!(
                    (sx - fx).abs() < 1e-4,
                    "azimuth {azimuth} elevation {elevation}"
                );
            }
        }
    }

    #[test]
    fn top_down_elevation_collapses_height_to_zero_screen_offset() {
        // At elevation PI/2 the camera looks straight down the height axis,
        // so no amount of z should move the projected point.
        let elevation = std::f32::consts::FRAC_PI_2;
        let (_, y_with_height, _) = project(1.0, 1.0, 5.0, 0.3, elevation);
        let (_, y_without_height, _) = project(1.0, 1.0, 0.0, 0.3, elevation);
        assert!((y_with_height - y_without_height).abs() < 1e-5);
    }
}
