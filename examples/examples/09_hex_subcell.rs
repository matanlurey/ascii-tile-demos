//! 09: Hex subcell -- pixel-accurate hexagons with no cell quantization,
//! versus the cell-snapped grid every other hex demo uses.
//!
//! [`07_hex_tiles`] and [`08_hex_outline`] both draw hexes whose vertices land
//! on character-cell boundaries, because [`tilekit::geom::HexLayout`] is built
//! to make that true. That is the right choice for a strategic map layer, but
//! it means the hex's edges are always a staircase at the same handful of
//! angles the cell pitch allows. This demo throws that constraint away: it
//! computes true hexagon vertices in floating-point sub-cell space (using
//! [Red Blob Games' flat/pointy hex corner
//! formulas](https://www.redblobgames.com/grids/hexagons/#hex-to-pixel)
//! directly, not `HexLayout`) and rasterizes the six edges with Bresenham
//! lines into a [`tilekit::glyphs::BrailleCanvas`], which packs 2x4 dots per
//! character cell. The result can be rotated and scaled continuously, because
//! nothing about it is snapped to a cell grid until the very last step.
//!
//! `V` toggles between three views side by side conceptually but shown one at
//! a time so each fills the screen: the braille pixel-accurate hex, the
//! `HexLayout`-quantized cell hex from `07`/`08` at the same size, and a
//! colored quadrant-canvas hex, which recovers two colors per glyph at a
//! quarter of braille's dot density. Seeing the three in immediate succession
//! at the same on-screen size is the point: the quantized hex's edge angle
//! jumps in fixed 8-cell steps as you rotate it, the braille hex's edge stays
//! smooth, and the quadrant hex sits in between.
//!
//! **Font note:** braille glyphs (U+2800-U+28FF) render as solid dot patterns
//! in `retroglyph-software`'s embedded bitmap font and in essentially all
//! modern terminal emulators and system UI fonts. A handful of very old or
//! minimal fixed-width fonts (raw VGA/CP437 ROM fonts, some embedded-device
//! fonts) lack the block entirely and show tofu (`?` or a box) instead; if
//! that happens, `V` to the quadrant view still demonstrates the
//! resolution-vs-color tradeoff without needing braille coverage. Quadrant
//! and sextant block elements (U+2580-U+259F, U+1FB00+) have slightly broader
//! legacy support than braille but are not universal either.
//!
//! Techniques on show:
//!
//! - **Sub-cell rasterization** ([`tilekit::glyphs::BrailleCanvas::line`]):
//!   drawing in a coordinate space 2x4 finer than the character grid and only
//!   quantizing to glyphs at the very end.
//! - **Continuous transforms on a discrete grid**: the hex's screen radius and
//!   rotation are `f32`, animated smoothly, and only rounded to dots (not
//!   cells) when a vertex is plotted.
//! - **Resolution vs. color tradeoff**: braille (8 dots, 1 color) versus
//!   quadrant (4 dots, 2 colors) versus a cell-quantized hex (1 "dot", 2
//!   colors) for the same hex, at the same size.
//!
//! ```sh
//! cargo run --example 09_hex_subcell --features crossterm
//! cargo run --example 09_hex_subcell --features software
//! cargo run --example 09_hex_subcell --features gl
//! cargo run --example 09_hex_subcell  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::HexLayout;
use tilekit::glyphs::{BrailleCanvas, QuadrantCanvas};
use tilekit::palette::{self, mix};

/// Which rendering mode is on screen. `V` cycles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    /// True hexagon vertices, rasterized into 2x4-dot braille cells.
    Braille,
    /// The same true vertices, rasterized into 2x2-subcell colored quadrants.
    Quadrant,
    /// `HexLayout::POINTY_LARGE`'s cell-quantized hex, for comparison.
    Quantized,
}

impl View {
    const fn next(self) -> Self {
        match self {
            Self::Braille => Self::Quadrant,
            Self::Quadrant => Self::Quantized,
            Self::Quantized => Self::Braille,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Braille => "braille (pixel-accurate, 1 color)",
            Self::Quadrant => "quadrant (pixel-accurate, 2 colors)",
            Self::Quantized => "cell-quantized (HexLayout)",
        }
    }
}

/// State: view mode, animated radius/rotation, and a ring sweep highlight.
pub struct HexSubcell {
    view: View,
    time: f32,
    /// Hex circumradius in sub-cell dots, adjustable with +/-.
    radius: f32,
    fps: FpsMeter,
}

impl Default for HexSubcell {
    fn default() -> Self {
        Self {
            view: View::Braille,
            time: 0.0,
            radius: 60.0,
            fps: FpsMeter::new(),
        }
    }
}

impl HexSubcell {
    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Char('v' | 'V') => self.view = self.view.next(),
                    KeyCode::Char('=' | '+') => self.radius = (self.radius + 8.0).min(200.0),
                    KeyCode::Char('-' | '_') => self.radius = (self.radius - 8.0).max(16.0),
                    _ => {}
                }
            }
        }
        true
    }

    /// The six pointy-top hex vertices around `(cx, cy)` at the current
    /// radius and rotation, in sub-cell dot coordinates.
    ///
    /// Red Blob Games' corner formula: vertex `i` sits at angle
    /// `60*i - 30` degrees for a pointy-top hex (the `-30` is what makes the
    /// top a point rather than a flat edge). `angle` is an extra rotation on
    /// top of that, driving the spin animation.
    fn vertices(&self, cx: f32, cy: f32) -> [(f32, f32); 6] {
        // Dots are roughly square (unlike character cells, which are ~1:2), so
        // no aspect correction is needed here -- the entire point of dropping
        // to sub-cell space is escaping that correction.
        let angle = self.time * 0.35;
        std::array::from_fn(|i| {
            let a = (60.0f32.mul_add(i as f32, -30.0)).to_radians() + angle;
            (
                self.radius.mul_add(a.cos(), cx),
                self.radius.mul_add(a.sin(), cy),
            )
        })
    }

    fn draw_braille(&self, surface: &mut Surface<'_>, area: Rect) {
        let (cols, rows) = (area.width(), area.height());
        let mut canvas = BrailleCanvas::new(cols, rows);
        let (dot_w, dot_h) = canvas.size();
        let (cx, cy) = (dot_w as f32 / 2.0, dot_h as f32 / 2.0);

        let verts = self.vertices(cx, cy);
        for i in 0..6 {
            let (x0, y0) = verts[i];
            let (x1, y1) = verts[(i + 1) % 6];
            canvas.line(
                x0.round() as i32,
                y0.round() as i32,
                x1.round() as i32,
                y1.round() as i32,
            );
        }
        // A sweep highlight around the ring: draw a short arc of extra dots
        // that circles the hexagon over time, so there is visible motion even
        // while the hex itself sits still between rotation steps.
        let sweep = self.time * 1.4;
        for i in 0..14 {
            let a = (i as f32).mul_add(0.05, sweep);
            let x = (self.radius + 3.0).mul_add(a.cos(), cx);
            let y = (self.radius + 3.0).mul_add(a.sin(), cy);
            canvas.plot(x.round() as i32, y.round() as i32);
        }

        let fg = palette::rgb(140, 200, 255);
        for (col, row, glyph) in canvas.cells() {
            if glyph != '\u{2800}' {
                surface.put(
                    (area.left() + col, area.top() + row),
                    glyph,
                    Style::new().fg(fg).bg(ui::BG),
                );
            }
        }
    }

    fn draw_quadrant(&self, surface: &mut Surface<'_>, area: Rect) {
        let (cols, rows) = (area.width(), area.height());
        let mut canvas = QuadrantCanvas::new(cols, rows, ui::BG);
        let (dot_w, dot_h) = canvas.size();
        let (cx, cy) = (dot_w as f32 / 2.0, dot_h as f32 / 2.0);

        // Fill the interior first (a scanline test against the polygon), then
        // outline it in a brighter color, so the two-color-per-cell budget is
        // spent on fill-vs-edge rather than wasted on an unfilled outline.
        let verts = self.vertices(cx, cy);
        let fill = mix(palette::rgb(120, 190, 130), ui::BG, 0.35);
        for y in 0..dot_h as i32 {
            for x in 0..dot_w as i32 {
                if point_in_hexagon((x as f32, y as f32), &verts) {
                    canvas.plot(x, y, fill);
                }
            }
        }
        let edge = palette::rgb(210, 244, 200);
        for i in 0..6 {
            plot_line_colored(&mut canvas, verts[i], verts[(i + 1) % 6], edge);
        }

        for (col, row, glyph) in canvas.cells() {
            surface.put(
                (area.left() + col, area.top() + row),
                glyph.ch,
                Style::new().fg(glyph.fg).bg(glyph.bg),
            );
        }
    }

    fn draw_quantized(surface: &mut Surface<'_>, area: Rect) {
        // The comparison case: HexLayout's own cell-snapped hex, drawn at
        // POINTY_LARGE's fixed 12x4 pitch (its geometry has no continuous
        // size knob, which is exactly the limitation this view exists to
        // show -- +/- does nothing here, unlike the other two views).
        let layout = HexLayout::POINTY_LARGE;
        let (w, h) = (layout.pitch_x, layout.pitch_y);
        let taper = w / 4;
        let cx = i32::from(area.left()) + i32::from(area.width()) / 2 - w / 2;
        let cy = i32::from(area.top()) + i32::from(area.height()) / 2 - h / 2;

        let fill = mix(palette::rgb(190, 150, 220), ui::BG, 0.35);
        for dy in 0..h {
            let (lo, hi) = if dy == 0 || dy == h - 1 {
                (taper, w - taper)
            } else {
                (0, w)
            };
            for dx in lo..hi {
                put_clipped(surface, area, cx + dx, cy + dy, ' ', Style::new().bg(fill));
            }
        }
        let edge = palette::rgb(230, 210, 244);
        let edge_style = Style::new().fg(edge).bg(fill);
        for dx in 0..taper {
            put_clipped(surface, area, cx + taper - 1 - dx, cy, '/', edge_style);
            put_clipped(surface, area, cx + w - taper + dx, cy, '\\', edge_style);
            put_clipped(
                surface,
                area,
                cx + taper - 1 - dx,
                cy + h - 1,
                '\\',
                edge_style,
            );
            put_clipped(
                surface,
                area,
                cx + w - taper + dx,
                cy + h - 1,
                '/',
                edge_style,
            );
        }
        for dy in 1..h - 1 {
            put_clipped(surface, area, cx, cy + dy, '|', edge_style);
            put_clipped(surface, area, cx + w - 1, cy + dy, '|', edge_style);
        }
    }

    fn status(&self) -> String {
        format!(
            "view: {}  radius {:.0} dots  [{}]",
            self.view.label(),
            self.radius,
            match self.view {
                View::Quantized => "fixed size; +/- has no effect here",
                _ => "+/- to resize",
            }
        )
    }
}

/// Point-in-polygon test (ray casting), used by the quadrant view's fill.
///
/// A dedicated scanline fill (sort edge crossings per row) would be faster,
/// but this canvas is at most a few hundred dots square and runs once per
/// frame, so the straightforward O(dots * vertices) test is not worth
/// replacing.
fn point_in_hexagon(p: (f32, f32), verts: &[(f32, f32); 6]) -> bool {
    let mut inside = false;
    let mut j = 5;
    for i in 0..6 {
        let (xi, yi) = verts[i];
        let (xj, yj) = verts[j];
        if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Bresenham line into a [`QuadrantCanvas`], mirroring
/// [`BrailleCanvas::line`] since the quadrant canvas has no built-in line
/// primitive (colored sub-cells need a color argument braille's monochrome
/// dots do not).
fn plot_line_colored(
    canvas: &mut QuadrantCanvas,
    from: (f32, f32),
    to: (f32, f32),
    color: retroglyph_core::Color,
) {
    let (x0, y0, x1, y1) = (
        from.0.round() as i32,
        from.1.round() as i32,
        to.0.round() as i32,
        to.1.round() as i32,
    );
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y) = (x0, y0);
    let mut err = dx + dy;
    loop {
        canvas.plot(x, y, color);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// [`Terminal::put_styled`] with clipping to `area`.
fn put_clipped(surface: &mut Surface<'_>, area: Rect, x: i32, y: i32, glyph: char, style: Style) {
    if x >= i32::from(area.left())
        && x < i32::from(area.right())
        && y >= i32::from(area.top())
        && y < i32::from(area.bottom())
    {
        surface.put((x as u16, y as u16), glyph, style);
    }
}

impl Demo for HexSubcell {
    const NAME: &'static str = "09_hex_subcell";
    const TITLE: &'static str = "09 Hex subcell";
    const BLURB: &'static str = "Pixel-accurate hex outlines via braille, versus cell-quantized.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[("V", "view"), ("+/-", "resize")]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        match self.view {
            View::Braille => self.draw_braille(&mut surface, content),
            View::Quadrant => self.draw_quadrant(&mut surface, content),
            View::Quantized => Self::draw_quantized(&mut surface, content),
        }
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(HexSubcell);
