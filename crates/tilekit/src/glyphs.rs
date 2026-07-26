//! Glyph banks and sub-cell drawing: getting more resolution out of a
//! character grid than one pixel per cell.
//!
//! A terminal cell is a single glyph with one foreground and one background
//! color, which sounds like a hard limit of one "pixel" per cell. It isn't.
//! Unicode's block elements partition a cell into halves, quadrants, or
//! sextants, and because each glyph's set and clear regions can take different
//! colors, one cell can carry two colors in up to six independently-chosen
//! regions. Braille goes further still: 2x4 dots per cell, at the cost of
//! being monochrome.
//!
//! | Bank | Sub-cells | Colors | Font requirement |
//! | --- | --- | --- | --- |
//! | [`SHADE`] | 1 (density only) | 2 | CP437, universal |
//! | [`HalfBlockCanvas`] | 1x2 | 2 independent | Block elements |
//! | [`QuadrantCanvas`] | 2x2 | 2 shared | Block elements |
//! | [`BrailleCanvas`] | 2x4 | 1 | Braille (U+2800) |
//!
//! `retroglyph-core` already provides the *quantizers* ([`quantize_half_block`]
//! and friends) that pick the best glyph for a block of colors. What this
//! module adds is the *canvas* side: a place to plot sub-cell points and then
//! read the whole thing back out as cells.

use retroglyph_core::Color;
use retroglyph_core::subcell::{Glyph, quantize_half_block, quantize_quadrant, quantize_sextant};

// ── Glyph banks ─────────────────────────────────────────────────────────────

/// Classic density ramp, sparse to solid. The only sub-cell trick that works
/// on a bare CP437 terminal, which is why it is still worth having.
pub const SHADE: [char; 5] = [' ', '░', '▒', '▓', '█'];

/// An ASCII-only brightness ramp, darkest first.
///
/// The fallback when even block elements are unavailable, and the traditional
/// look of an ASCII-art heightmap. Ordering is by ink coverage of a typical
/// monospace font, which is why `@` and `#` sit at the dense end.
pub const ASCII_RAMP: [char; 10] = [' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// The eight partial upper blocks, from empty to full, for vertical bar charts
/// and precise gauges.
pub const UPPER_EIGHTHS: [char; 9] = [' ', '▔', '▔', '▀', '▀', '▆', '▇', '█', '█'];

/// The eight partial left blocks, from empty to full.
pub const LEFT_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Terrain decoration glyphs, roughly by biome family. Kept here so demos
/// share one visual vocabulary instead of each inventing its own.
pub mod terrain {
    /// Open water, still.
    pub const WATER: char = '~';
    /// Open water, breaking. Alternate with [`WATER`] for a swell.
    pub const WAVE: char = '≈';
    /// Grass and steppe.
    pub const GRASS: char = '"';
    /// Broadleaf forest.
    pub const FOREST: char = '♣';
    /// Conifer forest and taiga.
    pub const CONIFER: char = '♠';
    /// Jungle and rainforest.
    pub const JUNGLE: char = '§';
    /// Hills and rough ground.
    pub const HILLS: char = 'n';
    /// Mountains.
    pub const MOUNTAIN: char = '▲';
    /// Peaks above the snow line.
    pub const PEAK: char = '^';
    /// Desert dunes.
    pub const DUNE: char = '∩';
    /// Sand and beach.
    pub const SAND: char = '·';
    /// Swamp and marsh.
    pub const MARSH: char = ',';
    /// Tundra.
    pub const TUNDRA: char = '`';
    /// Snow and ice.
    pub const SNOW: char = '*';
    /// Volcanic and scorched ground.
    pub const ASH: char = '%';
}

/// Settlement, landmark, and unit markers.
///
/// Every glyph here is CP437, and that is a hard constraint rather than a
/// stylistic one. `retroglyph`'s pixel backends resolve glyphs through
/// `BitmapFont::char_to_index`, which is CP437-only and substitutes a solid
/// block for anything else, so a marker outside CP437 renders as a filled
/// rectangle instead of failing visibly. The obvious workaround, supplying the
/// character from a tileset, trades one bug for a worse one: a sprite ignores
/// the cell's foreground (retroglyph#537), so a tileset marker can never carry
/// the faction or biome color that is most of the point of a marker.
///
/// So the palette below is chosen from what CP437 actually has, which is why
/// these are typographic stand-ins (`Ω` for a gate, `π` for a broken
/// colonnade) rather than the pictographic `♜`/`⚒`/`⚓` a modern font would
/// offer. `examples/tests/glyphs.rs` pins both properties for every constant
/// here; see <https://github.com/crates-lurey-io/retroglyph/issues/539>.
pub mod marker {
    /// A capital city. CP437 `0x0F`.
    pub const CAPITAL: char = '☼';
    /// A city. CP437 `0x7F`.
    pub const CITY: char = '⌂';
    /// A town or village. CP437 `0xA9`.
    pub const TOWN: char = '⌐';
    /// A fortress or watchtower: a gate arch. CP437 `0xEA`.
    pub const FORT: char = 'Ω';
    /// A ruin: a standing pair of columns and a lintel. CP437 `0xE3`.
    pub const RUIN: char = 'π';
    /// A mine or quarry: an adit mouth. CP437 `0xE9`.
    pub const MINE: char = 'Θ';
    /// A shrine or magical site. CP437 `0x04`.
    pub const SHRINE: char = '♦';
    /// A port: a quay seen from above. CP437 `0x16`.
    pub const PORT: char = '▬';
    /// A friendly unit: a filled token. CP437 `0x02`.
    pub const UNIT: char = '☻';
    /// A scout or explorer: a hollow token. CP437 `0x01`.
    pub const SCOUT: char = '☺';
}

/// Picks a glyph from `ramp` by normalized intensity `t` in `0.0..=1.0`.
///
/// Returns `' '` for an empty ramp rather than panicking, on the same
/// principle as [`Ramp::sample`](crate::palette::Ramp::sample): a missing
/// asset should degrade, not crash the frame.
#[must_use]
pub fn ramp_glyph(ramp: &[char], t: f32) -> char {
    if ramp.is_empty() {
        return ' ';
    }
    let scaled = t.clamp(0.0, 1.0) * (ramp.len() - 1) as f32;
    ramp[scaled.round() as usize % ramp.len()]
}

/// A 4x4 Bayer matrix, normalized to `0.0..1.0`.
///
/// Ordered dithering trades a little spatial noise for a lot of apparent
/// depth: with only five [`SHADE`] levels available, a smooth gradient
/// posterizes into visible bands, and offsetting each cell's threshold by this
/// matrix breaks those bands into a stable crosshatch the eye averages out.
/// Ordered rather than error-diffused deliberately: Floyd-Steinberg's output
/// changes wholesale when the map scrolls by one cell, which crawls; a Bayer
/// pattern is a pure function of position and stays put.
pub const BAYER_4X4: [[f32; 4]; 4] = [
    [0.0 / 16.0, 8.0 / 16.0, 2.0 / 16.0, 10.0 / 16.0],
    [12.0 / 16.0, 4.0 / 16.0, 14.0 / 16.0, 6.0 / 16.0],
    [3.0 / 16.0, 11.0 / 16.0, 1.0 / 16.0, 9.0 / 16.0],
    [15.0 / 16.0, 7.0 / 16.0, 13.0 / 16.0, 5.0 / 16.0],
];

/// The Bayer threshold for a cell, in `0.0..1.0`.
#[must_use]
pub const fn bayer(x: i32, y: i32) -> f32 {
    BAYER_4X4[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize]
}

/// Picks a ramp glyph with ordered dithering applied.
///
/// The dither offset is centred (`bayer - 0.5`) and scaled by one ramp step,
/// so a value exactly on a step boundary picks each neighbouring glyph about
/// half the time, and a value in the middle of a step never wavers.
#[must_use]
pub fn dithered_glyph(ramp: &[char], t: f32, x: i32, y: i32) -> char {
    if ramp.is_empty() {
        return ' ';
    }
    let step = 1.0 / (ramp.len() - 1).max(1) as f32;
    ramp_glyph(ramp, (bayer(x, y) - 0.5).mul_add(step, t))
}

// ── Sub-cell canvases ───────────────────────────────────────────────────────

/// A color plotted at a sub-cell resolution, read back one cell at a time.
///
/// The three canvases below differ only in their sub-cell layout, so they
/// share this shape: plot into a buffer sized in sub-cells, then iterate cells
/// and let `retroglyph`'s quantizers choose each cell's glyph and two colors.
///
/// The alternative (choosing a glyph as you plot) cannot work: a cell's glyph
/// depends on *all* of its sub-cells, so nothing can be decided until the last
/// plot lands.
#[derive(Debug, Clone)]
struct SubCanvas {
    /// Sub-cell width, i.e. `cols * sub_w`.
    width: usize,
    /// Sub-cell height, i.e. `rows * sub_h`.
    height: usize,
    pixels: Vec<Color>,
    clear: Color,
}

impl SubCanvas {
    fn new(width: usize, height: usize, clear: Color) -> Self {
        Self {
            width,
            height,
            pixels: vec![clear; width * height],
            clear,
        }
    }

    fn clear(&mut self) {
        self.pixels.fill(self.clear);
    }

    fn set(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        self.pixels[y * self.width + x] = color;
    }

    fn get(&self, x: usize, y: usize) -> Color {
        self.pixels
            .get(y * self.width + x)
            .copied()
            .unwrap_or(self.clear)
    }

    /// Resolves a sub-cell to the `(r, g, b)` triple the quantizers expect.
    ///
    /// `resolve_rgb`'s fallback matters: a `Color::Default` sub-cell has no
    /// intrinsic RGB, and quantizing it as black would draw a hard black block
    /// where the caller meant "nothing here". Resolving against the canvas'
    /// own clear color instead makes unset sub-cells vanish into the
    /// background, which is what "clear" should mean.
    fn rgb_at(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let fallback = self.clear.resolve_rgb((0, 0, 0));
        self.get(x, y).resolve_rgb(fallback)
    }
}

/// A canvas with 1x2 sub-cells (upper and lower half of each cell).
///
/// The most compatible sub-cell mode: `▀` and `▄` are in CP437 and render
/// correctly in essentially every terminal and bitmap font. Doubles vertical
/// resolution, which is exactly what a character grid is short of, given cells
/// are already about twice as tall as they are wide. Net effect: square
/// pixels.
#[derive(Debug, Clone)]
pub struct HalfBlockCanvas {
    canvas: SubCanvas,
    cols: usize,
    rows: usize,
}

impl HalfBlockCanvas {
    /// A canvas covering `cols` x `rows` cells, i.e. `cols` x `rows * 2`
    /// plottable points.
    #[must_use]
    pub fn new(cols: u16, rows: u16, clear: Color) -> Self {
        let (cols, rows) = (cols as usize, rows as usize);
        Self {
            canvas: SubCanvas::new(cols, rows * 2, clear),
            cols,
            rows,
        }
    }

    /// Plottable size, in sub-cells.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.canvas.width, self.canvas.height)
    }

    /// Resets every sub-cell to the clear color.
    pub fn clear(&mut self) {
        self.canvas.clear();
    }

    /// Plots one sub-cell. Out-of-bounds coordinates are ignored, so callers
    /// can draw shapes that overhang the canvas without clipping first.
    pub fn plot(&mut self, x: i32, y: i32, color: Color) {
        self.canvas.set(x, y, color);
    }

    /// Yields `(col, row, glyph)` for every cell.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, Glyph)> + '_ {
        (0..self.rows).flat_map(move |row| {
            (0..self.cols).map(move |col| {
                let glyph = quantize_half_block([
                    self.canvas.rgb_at(col, row * 2),
                    self.canvas.rgb_at(col, row * 2 + 1),
                ]);
                (col as u16, row as u16, glyph)
            })
        })
    }
}

/// A canvas with 2x2 sub-cells per cell.
///
/// Doubles resolution on both axes. The tradeoff against
/// [`HalfBlockCanvas`]: four sub-cells still share only two colors, so a
/// quadrant cell containing three different colors loses one of them, where a
/// half-block cell containing two keeps both exactly.
#[derive(Debug, Clone)]
pub struct QuadrantCanvas {
    canvas: SubCanvas,
    cols: usize,
    rows: usize,
}

impl QuadrantCanvas {
    /// A canvas covering `cols` x `rows` cells, i.e. `cols * 2` x `rows * 2`
    /// plottable points.
    #[must_use]
    pub fn new(cols: u16, rows: u16, clear: Color) -> Self {
        let (cols, rows) = (cols as usize, rows as usize);
        Self {
            canvas: SubCanvas::new(cols * 2, rows * 2, clear),
            cols,
            rows,
        }
    }

    /// Plottable size, in sub-cells.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.canvas.width, self.canvas.height)
    }

    /// Resets every sub-cell to the clear color.
    pub fn clear(&mut self) {
        self.canvas.clear();
    }

    /// Plots one sub-cell.
    pub fn plot(&mut self, x: i32, y: i32, color: Color) {
        self.canvas.set(x, y, color);
    }

    /// Yields `(col, row, glyph)` for every cell.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, Glyph)> + '_ {
        (0..self.rows).flat_map(move |row| {
            (0..self.cols).map(move |col| {
                let (x, y) = (col * 2, row * 2);
                let glyph = quantize_quadrant([
                    self.canvas.rgb_at(x, y),
                    self.canvas.rgb_at(x + 1, y),
                    self.canvas.rgb_at(x, y + 1),
                    self.canvas.rgb_at(x + 1, y + 1),
                ]);
                (col as u16, row as u16, glyph)
            })
        })
    }
}

/// A canvas with 2x3 sub-cells per cell, using Unicode sextants.
///
/// The highest-resolution *colored* mode: six sub-cells per glyph, still two
/// colors. Needs a font with "Symbols for Legacy Computing" (Unicode 13,
/// 2020); without it every cell renders as tofu. The embedded bitmap font
/// covers them; a random system terminal font may not, which is why the
/// demos that use sextants also offer a quadrant fallback.
#[derive(Debug, Clone)]
pub struct SextantCanvas {
    canvas: SubCanvas,
    cols: usize,
    rows: usize,
}

impl SextantCanvas {
    /// A canvas covering `cols` x `rows` cells, i.e. `cols * 2` x `rows * 3`
    /// plottable points.
    #[must_use]
    pub fn new(cols: u16, rows: u16, clear: Color) -> Self {
        let (cols, rows) = (cols as usize, rows as usize);
        Self {
            canvas: SubCanvas::new(cols * 2, rows * 3, clear),
            cols,
            rows,
        }
    }

    /// Plottable size, in sub-cells.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.canvas.width, self.canvas.height)
    }

    /// Resets every sub-cell to the clear color.
    pub fn clear(&mut self) {
        self.canvas.clear();
    }

    /// Plots one sub-cell.
    pub fn plot(&mut self, x: i32, y: i32, color: Color) {
        self.canvas.set(x, y, color);
    }

    /// Yields `(col, row, glyph)` for every cell.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, Glyph)> + '_ {
        (0..self.rows).flat_map(move |row| {
            (0..self.cols).map(move |col| {
                let (x, y) = (col * 2, row * 3);
                let glyph = quantize_sextant([
                    self.canvas.rgb_at(x, y),
                    self.canvas.rgb_at(x + 1, y),
                    self.canvas.rgb_at(x, y + 1),
                    self.canvas.rgb_at(x + 1, y + 1),
                    self.canvas.rgb_at(x, y + 2),
                    self.canvas.rgb_at(x + 1, y + 2),
                ]);
                (col as u16, row as u16, glyph)
            })
        })
    }
}

/// The base of the Braille Patterns block.
pub const BRAILLE_BASE: u32 = 0x2800;

/// Bit position of each of the 8 Braille dots, indexed `[x][y]`.
///
/// Braille dot numbering is historical, not raster order: dots 1 through 6
/// were numbered down the left column then down the right, and dots 7 and 8
/// were appended underneath when 8-dot Braille was added. So the bit for
/// `(x=0, y=3)` is bit 6, not bit 3, and hardcoding the "obvious" order
/// produces a canvas whose bottom row is scrambled.
pub const BRAILLE_DOTS: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];

/// A monochrome canvas with 2x4 sub-cells per cell, using Braille patterns.
///
/// The densest character-grid raster there is: 8 dots per cell, which on a
/// 100x40 grid is a 200x160 bitmap. The catch is in the name of the trade:
/// all 8 dots share one foreground color, so this draws *shapes*, not
/// pictures. Ideal for contour lines, a pixel-accurate hex outline, or a
/// high-resolution minimap silhouette.
#[derive(Debug, Clone)]
pub struct BrailleCanvas {
    cols: usize,
    rows: usize,
    /// One byte of dot bits per cell.
    dots: Vec<u8>,
}

impl BrailleCanvas {
    /// A canvas covering `cols` x `rows` cells, i.e. `cols * 2` x `rows * 4`
    /// plottable points.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols as usize, rows as usize);
        Self {
            cols,
            rows,
            dots: vec![0; cols * rows],
        }
    }

    /// Plottable size, in dots.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.cols * 2, self.rows * 4)
    }

    /// Clears every dot.
    pub fn clear(&mut self) {
        self.dots.fill(0);
    }

    /// Sets the dot at `(x, y)`. Out-of-bounds coordinates are ignored.
    pub fn plot(&mut self, x: i32, y: i32) {
        if let Some((index, bit)) = self.locate(x, y) {
            self.dots[index] |= 1 << bit;
        }
    }

    /// Clears the dot at `(x, y)`.
    pub fn unplot(&mut self, x: i32, y: i32) {
        if let Some((index, bit)) = self.locate(x, y) {
            self.dots[index] &= !(1 << bit);
        }
    }

    /// Whether the dot at `(x, y)` is set.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> bool {
        self.locate(x, y)
            .is_some_and(|(index, bit)| self.dots[index] & (1 << bit) != 0)
    }

    const fn locate(&self, x: i32, y: i32) -> Option<(usize, u8)> {
        if x < 0 || y < 0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        let (cell_x, cell_y) = (x / 2, y / 4);
        if cell_x >= self.cols || cell_y >= self.rows {
            return None;
        }
        Some((cell_y * self.cols + cell_x, BRAILLE_DOTS[x % 2][y % 4]))
    }

    /// Draws a line between two dot coordinates (Bresenham).
    ///
    /// The one drawing primitive worth having built in: contours, hex
    /// outlines, and route overlays are all lines, and hand-rolling Bresenham
    /// per demo is exactly the sort of duplication a shared crate exists to
    /// prevent.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y) = (x0, y0);
        let mut err = dx + dy;
        loop {
            self.plot(x, y);
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

    /// The Braille character for cell `(col, row)`, or `'\u{2800}'` (blank)
    /// if out of bounds.
    #[must_use]
    pub fn glyph(&self, col: usize, row: usize) -> char {
        let bits = if col < self.cols && row < self.rows {
            self.dots[row * self.cols + col]
        } else {
            0
        };
        char::from_u32(BRAILLE_BASE + u32::from(bits)).unwrap_or('\u{2800}')
    }

    /// Yields `(col, row, glyph)` for every cell.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, char)> + '_ {
        (0..self.rows).flat_map(move |row| {
            (0..self.cols).map(move |col| (col as u16, row as u16, self.glyph(col, row)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ASCII_RAMP, BRAILLE_BASE, BRAILLE_DOTS, BrailleCanvas, HalfBlockCanvas, QuadrantCanvas,
        SHADE, SextantCanvas, bayer, dithered_glyph, ramp_glyph,
    };
    use retroglyph_core::Color;
    use retroglyph_core::subcell::{Glyph, HALF_BLOCKS, QUADRANTS};

    const RED: Color = Color::Rgb { r: 255, g: 0, b: 0 };
    const BLUE: Color = Color::Rgb { r: 0, g: 0, b: 255 };
    const BLACK: Color = Color::Rgb { r: 0, g: 0, b: 0 };

    // ── Ramps and dithering ─────────────────────────────────────────────────

    #[test]
    fn ramp_glyph_spans_its_whole_range() {
        assert_eq!(ramp_glyph(&SHADE, 0.0), ' ');
        assert_eq!(ramp_glyph(&SHADE, 1.0), '█');
        assert_eq!(ramp_glyph(&ASCII_RAMP, 0.0), ' ');
        assert_eq!(ramp_glyph(&ASCII_RAMP, 1.0), '@');
    }

    #[test]
    fn ramp_glyph_clamps_and_tolerates_an_empty_ramp() {
        assert_eq!(ramp_glyph(&SHADE, -3.0), ' ');
        assert_eq!(ramp_glyph(&SHADE, 7.0), '█');
        assert_eq!(ramp_glyph(&[], 0.5), ' ');
    }

    #[test]
    fn ramp_glyph_is_monotonic() {
        let mut last = 0usize;
        for i in 0..=20 {
            let glyph = ramp_glyph(&SHADE, i as f32 / 20.0);
            let index = SHADE.iter().position(|&c| c == glyph).expect("in ramp");
            assert!(index >= last, "ramp went backwards at step {i}");
            last = index;
        }
    }

    #[test]
    fn bayer_matrix_covers_its_range_and_tiles() {
        let mut seen = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let v = bayer(x, y);
                assert!((0.0..1.0).contains(&v));
                seen.push((v * 16.0).round() as i32);
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..16).collect::<Vec<_>>(), "not a full Bayer matrix");
        // Tiles in both directions, including below zero.
        for (x, y) in [(0, 0), (4, 4), (-4, -4), (8, -8)] {
            assert!((bayer(x, y) - bayer(0, 0)).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn dithering_breaks_a_value_between_steps_across_both_neighbours() {
        // SHADE has 5 glyphs, so its steps sit at 0.0, 0.25, 0.5, 0.75, 1.0.
        // A value *between* two steps must produce both of them across a 4x4
        // block; that spread is the entire point of dithering.
        let mut glyphs: Vec<char> = (0..4)
            .flat_map(|y| (0..4).map(move |x| dithered_glyph(&SHADE, 0.375, x, y)))
            .collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(
            glyphs,
            vec!['░', '▒'],
            "expected a mix of the two steps 0.375 sits between"
        );
    }

    #[test]
    fn dithering_leaves_values_on_a_step_alone() {
        // The flip side: a value sitting exactly on a ramp step needs no
        // noise, and adding some would make flat regions shimmer.
        for (t, expected) in [(0.0, ' '), (0.25, '░'), (0.5, '▒'), (1.0, '█')] {
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(dithered_glyph(&SHADE, t, x, y), expected, "t={t}");
                }
            }
        }
    }

    #[test]
    fn dithering_never_escapes_the_ramp() {
        for i in 0..=10 {
            for y in 0..4 {
                for x in 0..4 {
                    let g = dithered_glyph(&SHADE, i as f32 / 10.0, x, y);
                    assert!(SHADE.contains(&g), "{g:?} is not in the ramp");
                }
            }
        }
        assert_eq!(dithered_glyph(&[], 0.5, 0, 0), ' ');
    }

    // ── Sub-cell canvases ───────────────────────────────────────────────────

    #[test]
    fn half_block_canvas_has_double_vertical_resolution() {
        let canvas = HalfBlockCanvas::new(10, 5, BLACK);
        assert_eq!(canvas.size(), (10, 10));
        assert_eq!(canvas.cells().count(), 50);
    }

    /// Resolves a quantized [`Glyph`] back into its per-sub-cell colors.
    ///
    /// Necessary because a posterizer has two equally correct answers for
    /// every cell: `'▀'` with `(fg, bg)` renders pixel-for-pixel identically
    /// to `'▄'` with `(bg, fg)`. Asserting on the glyph alone would therefore
    /// be asserting on an implementation detail; asserting on what actually
    /// gets drawn is both stricter and stable.
    fn resolve(glyph: Glyph, bank: &[char]) -> Vec<Color> {
        let mask = bank
            .iter()
            .position(|&c| c == glyph.ch)
            .expect("glyph must come from its own bank");
        // A bank of 2^n glyphs enumerates every subset of n sub-cells, so the
        // sub-cell count is log2 of the bank length.
        let sub_cells = bank.len().trailing_zeros() as usize;
        (0..sub_cells)
            .map(|i| {
                if mask & (1 << i) != 0 {
                    glyph.fg
                } else {
                    glyph.bg
                }
            })
            .collect()
    }

    #[test]
    fn half_block_renders_the_half_that_was_plotted() {
        let mut canvas = HalfBlockCanvas::new(1, 1, BLACK);
        canvas.plot(0, 0, RED);
        let (_, _, glyph) = canvas.cells().next().expect("one cell");
        assert_eq!(resolve(glyph, &HALF_BLOCKS), vec![RED, BLACK], "top set");

        canvas.clear();
        canvas.plot(0, 1, RED);
        let (_, _, glyph) = canvas.cells().next().expect("one cell");
        assert_eq!(resolve(glyph, &HALF_BLOCKS), vec![BLACK, RED], "bottom set");
    }

    #[test]
    fn quadrant_canvas_has_double_resolution_on_both_axes() {
        let canvas = QuadrantCanvas::new(8, 4, BLACK);
        assert_eq!(canvas.size(), (16, 8));
        assert_eq!(canvas.cells().count(), 32);
    }

    #[test]
    fn quadrant_renders_the_corner_that_was_plotted() {
        for (x, y, index) in [(0, 0, 0), (1, 0, 1), (0, 1, 2), (1, 1, 3)] {
            let mut canvas = QuadrantCanvas::new(1, 1, BLACK);
            canvas.plot(x, y, RED);
            let (_, _, glyph) = canvas.cells().next().expect("one cell");
            let mut expected = vec![BLACK; 4];
            expected[index] = RED;
            assert_eq!(resolve(glyph, &QUADRANTS), expected, "corner ({x}, {y})");
        }
    }

    #[test]
    fn canvases_preserve_plotted_colors_exactly() {
        // A two-color cell must survive quantization unchanged: the whole
        // reason to use half-blocks over a shade ramp is that they are exact.
        let mut canvas = HalfBlockCanvas::new(1, 1, BLACK);
        canvas.plot(0, 0, RED);
        canvas.plot(0, 1, BLUE);
        let (_, _, glyph) = canvas.cells().next().expect("one cell");
        assert_eq!(resolve(glyph, &HALF_BLOCKS), vec![RED, BLUE]);
    }

    #[test]
    fn sextant_canvas_is_two_by_three() {
        let canvas = SextantCanvas::new(6, 3, BLACK);
        assert_eq!(canvas.size(), (12, 9));
        assert_eq!(canvas.cells().count(), 18);
    }

    #[test]
    fn canvases_ignore_out_of_bounds_plots() {
        let mut half = HalfBlockCanvas::new(2, 2, BLACK);
        let mut quad = QuadrantCanvas::new(2, 2, BLACK);
        let mut sext = SextantCanvas::new(2, 2, BLACK);
        for (x, y) in [(-1, 0), (0, -1), (999, 0), (0, 999), (-999, -999)] {
            half.plot(x, y, RED);
            quad.plot(x, y, RED);
            sext.plot(x, y, RED);
        }
        // Nothing was written, so every cell is still the clear color.
        assert!(half.cells().all(|(_, _, g)| g.ch == ' ' || g.fg == BLACK));
        assert!(quad.cells().all(|(_, _, g)| g.ch == ' ' || g.fg == BLACK));
        assert!(sext.cells().all(|(_, _, g)| g.ch == ' ' || g.fg == BLACK));
    }

    #[test]
    fn clearing_a_canvas_erases_everything() {
        let mut canvas = QuadrantCanvas::new(2, 2, BLACK);
        canvas.plot(0, 0, RED);
        canvas.clear();
        let glyphs: Vec<char> = canvas.cells().map(|(_, _, g)| g.ch).collect();
        assert!(glyphs.iter().all(|&c| c == ' '), "left over: {glyphs:?}");
    }

    // ── Braille ─────────────────────────────────────────────────────────────

    #[test]
    fn braille_canvas_is_two_by_four() {
        let canvas = BrailleCanvas::new(20, 10);
        assert_eq!(canvas.size(), (40, 40));
        assert_eq!(canvas.cells().count(), 200);
    }

    #[test]
    fn braille_dot_numbering_matches_the_unicode_standard() {
        // Every one of the 8 bit positions must appear exactly once, or some
        // dot is unreachable and another is double-booked.
        let mut bits: Vec<u8> = BRAILLE_DOTS.iter().flatten().copied().collect();
        bits.sort_unstable();
        assert_eq!(bits, (0..8).collect::<Vec<u8>>());
        // Spot-check the historical ordering that raster order gets wrong.
        assert_eq!(BRAILLE_DOTS[0][3], 6, "left column dot 7 is bit 6");
        assert_eq!(BRAILLE_DOTS[1][3], 7, "right column dot 8 is bit 7");
    }

    #[test]
    fn braille_plot_round_trips_through_get() {
        let mut canvas = BrailleCanvas::new(4, 2);
        for y in 0..8 {
            for x in 0..8 {
                assert!(!canvas.get(x, y));
                canvas.plot(x, y);
                assert!(canvas.get(x, y), "dot ({x}, {y}) did not stick");
                canvas.unplot(x, y);
                assert!(!canvas.get(x, y), "dot ({x}, {y}) did not clear");
            }
        }
    }

    #[test]
    fn braille_glyph_is_blank_when_empty_and_full_when_saturated() {
        let mut canvas = BrailleCanvas::new(1, 1);
        assert_eq!(canvas.glyph(0, 0), '\u{2800}');
        for y in 0..4 {
            for x in 0..2 {
                canvas.plot(x, y);
            }
        }
        assert_eq!(canvas.glyph(0, 0), '\u{28FF}', "all 8 dots set");
    }

    #[test]
    fn braille_glyphs_stay_inside_the_unicode_block() {
        let mut canvas = BrailleCanvas::new(3, 3);
        canvas.line(0, 0, 5, 11);
        for (_, _, glyph) in canvas.cells() {
            let code = glyph as u32;
            assert!(
                (BRAILLE_BASE..BRAILLE_BASE + 256).contains(&code),
                "{glyph:?} is outside the Braille block"
            );
        }
    }

    #[test]
    fn braille_out_of_bounds_is_ignored_not_panicked() {
        let mut canvas = BrailleCanvas::new(2, 2);
        for (x, y) in [(-1, 0), (0, -1), (500, 0), (0, 500)] {
            canvas.plot(x, y);
            assert!(!canvas.get(x, y));
        }
        assert_eq!(canvas.glyph(99, 99), '\u{2800}');
    }

    #[test]
    fn braille_line_connects_both_endpoints() {
        let mut canvas = BrailleCanvas::new(8, 4);
        canvas.line(0, 0, 15, 15);
        assert!(canvas.get(0, 0), "start not drawn");
        assert!(canvas.get(15, 15), "end not drawn");
    }

    #[test]
    fn braille_line_is_continuous() {
        // Every plotted dot must have a neighbour in the previous column, or
        // the line has a gap the eye will see.
        let mut canvas = BrailleCanvas::new(8, 4);
        canvas.line(0, 0, 15, 7);
        for x in 1..=15 {
            let column_has_dot = (0..16).any(|y| canvas.get(x, y));
            assert!(column_has_dot, "column {x} is empty");
        }
    }

    #[test]
    fn braille_line_handles_every_direction() {
        let mut canvas = BrailleCanvas::new(8, 4);
        for (x0, y0, x1, y1) in [
            (0, 0, 15, 0),  // horizontal
            (0, 0, 0, 15),  // vertical
            (15, 15, 0, 0), // reversed diagonal
            (0, 15, 15, 0), // anti-diagonal
            (3, 3, 3, 3),   // degenerate single point
        ] {
            canvas.clear();
            canvas.line(x0, y0, x1, y1);
            assert!(canvas.get(x0, y0), "start of ({x0},{y0})-({x1},{y1})");
            assert!(canvas.get(x1, y1), "end of ({x0},{y0})-({x1},{y1})");
        }
    }
}
