//! 16: Sub-cell canvas -- the character grid as a pixel canvas, not an ASCII
//! art surface.
//!
//! Every mode here renders the *same* heightmap/biome color field. The only
//! thing that changes is how many independently-colored sub-cell points each
//! character carries, from one (a single glyph per cell) up to eight (Braille
//! dots). The point is to make the resolution difference visible rather than
//! described: at [`Mode::Braille`] this should look like a smooth, if
//! monochrome, image, not like ASCII art.
//!
//! Techniques on show:
//!
//! - **[`tilekit::glyphs::HalfBlockCanvas`]**: 1x2 sub-cells (upper/lower
//!   half of each character), two independently-colored regions. `▀`/`▄` are
//!   in CP437 and render correctly in essentially any terminal or bitmap
//!   font; this is the safest sub-cell mode there is.
//! - **[`tilekit::glyphs::QuadrantCanvas`]**: 2x2 sub-cells, still two colors
//!   shared across four regions -- doubles resolution on *both* axes over
//!   half-blocks, at the cost of losing a color when a 2x2 block spans three
//!   or more distinct source colors.
//! - **[`tilekit::glyphs::SextantCanvas`]**: 2x3 sub-cells, the highest
//!   resolution that still carries two real colors. Needs a font with
//!   Unicode 13's "Symbols for Legacy Computing" block (2020); on a font
//!   without it, sextant cells render as tofu/replacement boxes instead of
//!   partial blocks -- the embedded bitmap fonts on the software and GL
//!   backends include them, but a bare system terminal font may not.
//! - **[`tilekit::glyphs::BrailleCanvas`]**: 2x4 dots per cell, the densest
//!   raster available (on a 100x40 grid, 200x160 dots), at the cost of being
//!   strictly monochrome: all 8 dots share one foreground color, so this
//!   canvas draws *shapes* (via a threshold), not full-color images.
//!
//! ```sh
//! cargo run --example 16_subcell_canvas --features crossterm
//! cargo run --example 16_subcell_canvas --features software
//! cargo run --example 16_subcell_canvas --features gl
//! cargo run --example 16_subcell_canvas  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::glyphs::{
    BrailleCanvas, HalfBlockCanvas, QuadrantCanvas, SHADE, SextantCanvas, bayer, ramp_glyph,
};
use tilekit::noise::warped_fbm;
use tilekit::palette::{self, ELEVATION, mix};

/// Which sub-cell mode is active. `M` cycles through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One glyph per cell, from a five-level shade ramp -- the baseline every
    /// other mode is a multiple of.
    Plain,
    /// [`HalfBlockCanvas`]: 1x2 sub-cells.
    HalfBlock,
    /// [`QuadrantCanvas`]: 2x2 sub-cells.
    Quadrant,
    /// [`SextantCanvas`]: 2x3 sub-cells. May render as tofu on a font without
    /// Unicode 13 "Symbols for Legacy Computing" coverage.
    Sextant,
    /// [`BrailleCanvas`]: 2x4 dots, monochrome.
    Braille,
}

impl Mode {
    const fn next(self) -> Self {
        match self {
            Self::Plain => Self::HalfBlock,
            Self::HalfBlock => Self::Quadrant,
            Self::Quadrant => Self::Sextant,
            Self::Sextant => Self::Braille,
            Self::Braille => Self::Plain,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Plain => "plain (1 sub-cell)",
            Self::HalfBlock => "half-block (1x2)",
            Self::Quadrant => "quadrant (2x2)",
            Self::Sextant => "sextant (2x3, needs Unicode 13 font)",
            Self::Braille => "braille (2x4, monochrome)",
        }
    }

    /// Sub-cell dimensions per character, `(w, h)`.
    const fn sub_cells(self) -> (u32, u32) {
        match self {
            Self::Plain => (1, 1),
            Self::HalfBlock => (1, 2),
            Self::Quadrant => (2, 2),
            Self::Sextant => (2, 3),
            Self::Braille => (2, 4),
        }
    }
}

/// State: the height field (generated once; only the rendering mode and the
/// sweep animation change per frame), the active mode, and a zoom/pan offset
/// into the field.
pub struct SubcellCanvas {
    time: f32,
    fps: FpsMeter,
    mode: Mode,
    /// World-space top-left of the visible window into the (effectively
    /// infinite, noise-generated) height field, in field units.
    offset_x: f32,
    offset_y: f32,
    /// Field units per character cell. Smaller values zoom in.
    zoom: f32,
    seed: u32,
}

impl Default for SubcellCanvas {
    fn default() -> Self {
        Self {
            time: 0.0,
            fps: FpsMeter::new(),
            mode: Mode::Braille,
            offset_x: 0.0,
            offset_y: 0.0,
            zoom: 0.045,
            seed: 5,
        }
    }
}

impl SubcellCanvas {
    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                let step = self.zoom * 6.0;
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.offset_y -= step,
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.offset_y += step,
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.offset_x -= step,
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.offset_x += step,
                    KeyCode::Char('m' | 'M') => self.mode = self.mode.next(),
                    KeyCode::Char('+' | '=') => self.zoom = (self.zoom * 0.8).max(0.004),
                    KeyCode::Char('-' | '_') => self.zoom = (self.zoom * 1.25).min(0.3),
                    KeyCode::Char('r' | 'R') => self.seed = self.seed.wrapping_add(1),
                    _ => {}
                }
            }
        }
        true
    }

    /// Elevation in `0.0..=1.0` at field coordinates `(fx, fy)`.
    ///
    /// The same domain-warped fBm `tilekit::world` uses for terrain, sampled
    /// directly rather than through a generated [`tilekit::world::World`]:
    /// this demo zooms and pans continuously, including to field scales a
    /// discrete world grid was never generated at, so it needs the underlying
    /// continuous field rather than a fixed-resolution array.
    fn elevation_at(&self, fx: f32, fy: f32) -> f32 {
        warped_fbm(self.seed, fx, fy, 5, 0.5, 1.3)
    }

    /// A slow highlight sweeping left to right across the field, in field
    /// units. Demonstrates the sub-cell resolution in motion: at low
    /// resolution the sweep's edge visibly steps from cell to cell, while at
    /// [`Mode::Braille`] it glides.
    fn sweep_x(&self, view_w: f32) -> f32 {
        let phase = (self.time * 0.12).fract();
        let advance = (phase * view_w).mul_add(1.4, self.offset_x);
        view_w.mul_add(-0.2, advance)
    }

    /// Elevation color, with the moving sweep highlight blended in.
    fn color_at(&self, fx: f32, fy: f32, sweep_x: f32) -> Color {
        let elevation = self.elevation_at(fx, fy);
        let base = ELEVATION.sample(elevation);
        let dist = (fx - sweep_x).abs();
        // The sweep band is a few field-units wide; band width scales with
        // zoom so it covers a consistent fraction of the view at any zoom
        // level instead of vanishing to nothing zoomed out.
        let band = self.zoom * 3.0;
        if dist < band {
            let strength = (1.0 - dist / band) * 0.5;
            mix(base, palette::WHITE, strength)
        } else {
            base
        }
    }

    fn draw(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let view_w = f32::from(area.width()) * self.zoom;
        let sweep_x = self.sweep_x(view_w);

        match self.mode {
            Mode::Plain => self.draw_plain(surface, area, sweep_x),
            Mode::HalfBlock => self.draw_half_block(surface, area, sweep_x),
            Mode::Quadrant => self.draw_quadrant(surface, area, sweep_x),
            Mode::Sextant => self.draw_sextant(surface, area, sweep_x),
            Mode::Braille => self.draw_braille(surface, area, sweep_x),
        }
    }

    /// Maps a sub-cell coordinate within `area` (in the given sub-cell grid
    /// units) to field-space coordinates.
    fn field_coord(&self, sub_x: f32, sub_y: f32, sub_w: f32, sub_h: f32) -> (f32, f32) {
        let fx = sub_x.mul_add(self.zoom / sub_w, self.offset_x);
        let fy = sub_y.mul_add(self.zoom / sub_h, self.offset_y);
        (fx, fy)
    }

    fn draw_plain(&self, surface: &mut Surface<'_>, area: Rect, sweep_x: f32) {
        for y in 0..area.height() {
            for x in 0..area.width() {
                let (fx, fy) = self.field_coord(f32::from(x), f32::from(y), 1.0, 1.0);
                let elevation = self.elevation_at(fx, fy);
                let glyph = ramp_glyph(&SHADE, elevation);
                let color = self.color_at(fx, fy, sweep_x);
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(color).bg(ui::BG),
                );
            }
        }
    }

    fn draw_half_block(&self, surface: &mut Surface<'_>, area: Rect, sweep_x: f32) {
        let mut canvas = HalfBlockCanvas::new(area.width(), area.height(), ui::BG);
        let (sw, sh) = canvas.size();
        for sy in 0..sh {
            for sx in 0..sw {
                let (fx, fy) = self.field_coord(sx as f32, sy as f32, 1.0, 2.0);
                canvas.plot(sx as i32, sy as i32, self.color_at(fx, fy, sweep_x));
            }
        }
        for (col, row, glyph) in canvas.cells() {
            surface.put(
                (area.left() + col, area.top() + row),
                glyph.ch,
                Style::new().fg(glyph.fg).bg(glyph.bg),
            );
        }
    }

    fn draw_quadrant(&self, surface: &mut Surface<'_>, area: Rect, sweep_x: f32) {
        let mut canvas = QuadrantCanvas::new(area.width(), area.height(), ui::BG);
        let (sw, sh) = canvas.size();
        for sy in 0..sh {
            for sx in 0..sw {
                let (fx, fy) = self.field_coord(sx as f32, sy as f32, 2.0, 2.0);
                canvas.plot(sx as i32, sy as i32, self.color_at(fx, fy, sweep_x));
            }
        }
        for (col, row, glyph) in canvas.cells() {
            surface.put(
                (area.left() + col, area.top() + row),
                glyph.ch,
                Style::new().fg(glyph.fg).bg(glyph.bg),
            );
        }
    }

    fn draw_sextant(&self, surface: &mut Surface<'_>, area: Rect, sweep_x: f32) {
        let mut canvas = SextantCanvas::new(area.width(), area.height(), ui::BG);
        let (sw, sh) = canvas.size();
        for sy in 0..sh {
            for sx in 0..sw {
                let (fx, fy) = self.field_coord(sx as f32, sy as f32, 2.0, 3.0);
                canvas.plot(sx as i32, sy as i32, self.color_at(fx, fy, sweep_x));
            }
        }
        for (col, row, glyph) in canvas.cells() {
            surface.put(
                (area.left() + col, area.top() + row),
                glyph.ch,
                Style::new().fg(glyph.fg).bg(glyph.bg),
            );
        }
    }

    /// Braille has no color channel to carry elevation directly, so the field
    /// is thresholded: a dot is plotted where elevation exceeds a level tied
    /// to the animated sweep, producing a moving contour-like silhouette
    /// rather than a shaded relief. This is the honest way to show a scalar
    /// field in a canvas that only has "dot" and "no dot" to work with.
    fn draw_braille(&self, surface: &mut Surface<'_>, area: Rect, sweep_x: f32) {
        let mut canvas = BrailleCanvas::new(area.width(), area.height());
        let (sw, sh) = canvas.size();
        for sy in 0..sh {
            for sx in 0..sw {
                let (fx, fy) = self.field_coord(sx as f32, sy as f32, 2.0, 4.0);
                let elevation = self.elevation_at(fx, fy);
                // The sweep band lifts the local elevation, so the stipple
                // visibly thickens wherever the sweep currently is -- a moving
                // distortion of an otherwise static field.
                let dist = (fx - sweep_x).abs();
                let band = self.zoom * 3.0;
                let boost = if dist < band {
                    (1.0 - dist / band) * 0.12
                } else {
                    0.0
                };

                // Ordered dithering, not a hard threshold. Braille dots are
                // monochrome, so a threshold can only ever produce a
                // silhouette: one flat region of "on" and one of "off". A
                // Bayer-dithered threshold instead makes dot *density* track
                // elevation, which is how a one-bit medium renders a
                // continuous tone at all, and turns the same 2x4 dot grid
                // from a shape into an image.
                //
                // The 0.30..0.70 window is centred on 0.5 because that is
                // where raw fBm lives; thresholding against
                // `world::SEA_LEVEL` (0.42, which is calibrated for the
                // island-masked heightmap in `world`, not for a bare noise
                // field) puts almost the entire field on the "land" side and
                // fills the screen solid.
                let dither = bayer(sx as i32, sy as i32);
                if (elevation + boost - 0.30) / 0.40 > dither {
                    canvas.plot(sx as i32, sy as i32);
                }
            }
        }
        for (col, row, glyph) in canvas.cells() {
            surface.put(
                (area.left() + col, area.top() + row),
                glyph,
                Style::new().fg(palette::rgb(140, 200, 255)).bg(ui::BG),
            );
        }
    }

    /// Effective dot/sub-cell resolution of the active mode over `area`, as
    /// `"WxH dots"` or `"WxH cells"` for the plain baseline.
    fn resolution_text(&self, area: Rect) -> String {
        let (sub_w, sub_h) = self.mode.sub_cells();
        let (w, h) = (
            u32::from(area.width()) * sub_w,
            u32::from(area.height()) * sub_h,
        );
        let unit = if self.mode == Mode::Braille {
            "dots"
        } else {
            "sub-cells"
        };
        format!("{w}x{h} {unit}")
    }
}

impl Demo for SubcellCanvas {
    const NAME: &'static str = "16_subcell_canvas";
    const TITLE: &'static str = "16 Sub-cell canvas";
    const BLURB: &'static str =
        "The character grid as a pixel canvas: half-block, quadrant, sextant, braille.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("+/-", "zoom"),
            ("M", "mode"),
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
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = format!(
            "{}  {}  seed {}",
            self.mode.label(),
            self.resolution_text(content),
            self.seed
        );
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(SubcellCanvas);
