//! Generates `examples/assets/blocks.png`, the supplementary glyph sheet that
//! makes quadrant, sextant, and braille characters render on the pixel
//! backends.
//!
//! ## Why this has to exist
//!
//! `retroglyph`'s embedded bitmap font is CP437, and its character lookup is
//! CP437 by construction: `BitmapFont::try_char_to_index` runs every `char`
//! through `unicode_to_cp437`, and a `FallbackFontChain` calls the same
//! function on each font it tries. A font in the chain can therefore only ever
//! supply glyphs CP437 already names; there is no way to extend the
//! *repertoire*, only to fill gaps within it.
//!
//! CP437 covers the shade ramp and the four half blocks, which is why
//! [`HalfBlockCanvas`](tilekit::glyphs::HalfBlockCanvas) works on every
//! backend. It does not cover the ten remaining quadrants, any of the 64
//! sextants (Unicode 13, 2020), or any of the 256 braille patterns. Those all
//! resolve to the solid-block fallback, so a braille canvas renders as a
//! rectangle of solid color: not subtly wrong, completely wrong.
//!
//! Tilesets are the way out. A registered tileset overrides the bitmap font
//! for whatever glyphs its codepage maps, and
//! [`Codepage::Custom`](retroglyph_window::tileset::Codepage::Custom) maps an
//! arbitrary list of `char`s. So this sheet draws each missing glyph as an
//! 8x16 sprite (the font's own cell size) and hands the renderer an explicit
//! char list.
//!
//! Drawing them is trivial because they are all geometry: a quadrant is four
//! rectangles, a sextant is six, a braille cell is eight dots. There is no art
//! here to get wrong, which is exactly why generating them beats hunting for a
//! font with the right coverage and a compatible license.

use image::{ImageBuffer, Rgba, RgbaImage};

/// Glyph cell size in pixels. Must match the embedded Unscii 16 font, or the
/// supplemented glyphs would not line up with ordinary text on the same row.
pub const CELL_W: u32 = 8;
/// See [`CELL_W`].
pub const CELL_H: u32 = 16;

/// Opaque white. Sprites are drawn as a white mask so the renderer's
/// foreground color comes through; a colored sprite would ignore the cell's
/// style and paint every braille dot the same shade forever.
const INK: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// Fully transparent, so the cell's own background shows through.
const CLEAR: Rgba<u8> = Rgba([0, 0, 0, 0]);

/// The ten quadrant glyphs CP437 lacks, in mask order.
///
/// Bit order matches [`tilekit::autotile::DualGrid::corner_mask`]: bit 0
/// top-left, 1 top-right, 2 bottom-left, 3 bottom-right. The six CP437 already
/// has (space, the four half blocks, and full block) are deliberately absent:
/// overriding a glyph the font renders correctly would only risk making it
/// look different from the same character drawn elsewhere on screen.
const QUADRANTS: [(char, u8); 10] = [
    ('\u{2598}', 0b0001),
    ('\u{259D}', 0b0010),
    ('\u{2596}', 0b0100),
    ('\u{2597}', 0b1000),
    ('\u{259E}', 0b0110),
    ('\u{259A}', 0b1001),
    ('\u{259B}', 0b0111),
    ('\u{259C}', 0b1011),
    ('\u{2599}', 0b1101),
    ('\u{259F}', 0b1110),
];

/// Every glyph this sheet supplies, in sheet order, paired with a painter.
///
/// The order is the codepage the demo side must use; [`codepage`] returns
/// exactly this list of `char`s so the two cannot drift.
pub struct Sheet {
    /// One entry per sprite: the character it stands for.
    pub chars: Vec<char>,
    /// The rendered sheet, one sprite per column.
    pub image: RgbaImage,
}

/// Draws one glyph into the sheet at a given x origin.
type Painter = Box<dyn Fn(&mut RgbaImage, u32)>;

/// Builds the supplementary sheet.
#[must_use]
pub fn build() -> Sheet {
    let mut chars = Vec::new();
    let mut painters: Vec<Painter> = Vec::new();

    for (ch, mask) in QUADRANTS {
        chars.push(ch);
        painters.push(Box::new(move |img, ox| paint_quadrant(img, ox, mask)));
    }

    // Sextants: U+1FB00 onward, skipping the two the block itself omits
    // because they duplicate existing characters (mask 0 is a space and mask
    // 63 is a full block, both already in CP437). The block's codepoints are
    // therefore *not* contiguous with the mask value, which is the one fiddly
    // part of the sextant encoding and the reason this loop computes the
    // codepoint rather than assuming `0x1FB00 + mask`.
    //
    // Masks 21 and 42 are skipped outright. They map to `▌` and `▐`, which
    // are not in the 1FB00 block precisely because CP437 already has them, so
    // including them here would override two glyphs the font draws correctly.
    // That is not merely redundant: a tileset sprite ignores the cell's
    // foreground (retroglyph#537), so supplying them from this sheet turns two
    // working colorable half blocks into white-only ones, and `▌` is the
    // building block of every half-width bar and gauge in the gallery. The
    // quadrant table above omits the CP437 members for the same reason; this
    // loop simply has to say so explicitly, because it derives its characters
    // rather than listing them.
    for mask in 1u8..63 {
        if mask == 21 || mask == 42 {
            continue;
        }
        let Some(ch) = sextant_char(mask) else {
            continue;
        };
        chars.push(ch);
        painters.push(Box::new(move |img, ox| paint_sextant(img, ox, mask)));
    }

    // Braille: the whole 256-glyph block. Every pattern is reachable from a
    // `BrailleCanvas`, so there is no subset worth picking.
    for bits in 0u16..256 {
        let ch = char::from_u32(0x2800 + u32::from(bits)).expect("valid braille codepoint");
        chars.push(ch);
        let bits = bits as u8;
        painters.push(Box::new(move |img, ox| paint_braille(img, ox, bits)));
    }

    let cols = chars.len() as u32;
    let mut image: RgbaImage = ImageBuffer::from_pixel(cols * CELL_W, CELL_H, CLEAR);
    for (index, paint) in painters.iter().enumerate() {
        paint(&mut image, index as u32 * CELL_W);
    }

    Sheet { chars, image }
}

/// The sextant character for a 6-bit mask, or `None` for the two masks the
/// Unicode block omits.
///
/// Bit order is reading order: bit 0 top-left, 1 top-right, 2 middle-left, 3
/// middle-right, 4 bottom-left, 5 bottom-right, matching
/// [`retroglyph_core::subcell::quantize_sextant`]'s pixel argument order.
#[must_use]
fn sextant_char(mask: u8) -> Option<char> {
    match mask {
        // U+1FB00 is mask 1; the block skips mask 21 (left half block) and
        // mask 42 (right half block) because CP437-era characters already
        // exist for them, so every codepoint past each gap shifts down by one.
        0 | 63 => None,
        21 => Some('\u{258C}'),
        42 => Some('\u{2590}'),
        _ => {
            let mut offset = u32::from(mask) - 1;
            if mask > 21 {
                offset -= 1;
            }
            if mask > 42 {
                offset -= 1;
            }
            char::from_u32(0x1FB00 + offset)
        }
    }
}

/// Fills a rectangle in cell-local coordinates.
fn rect(img: &mut RgbaImage, ox: u32, x0: u32, y0: u32, x1: u32, y1: u32) {
    for y in y0..y1.min(CELL_H) {
        for x in x0..x1.min(CELL_W) {
            img.put_pixel(ox + x, y, INK);
        }
    }
}

/// Paints one quadrant glyph: up to four filled corners of the cell.
fn paint_quadrant(img: &mut RgbaImage, ox: u32, mask: u8) {
    let (mx, my) = (CELL_W / 2, CELL_H / 2);
    if mask & 0b0001 != 0 {
        rect(img, ox, 0, 0, mx, my);
    }
    if mask & 0b0010 != 0 {
        rect(img, ox, mx, 0, CELL_W, my);
    }
    if mask & 0b0100 != 0 {
        rect(img, ox, 0, my, mx, CELL_H);
    }
    if mask & 0b1000 != 0 {
        rect(img, ox, mx, my, CELL_W, CELL_H);
    }
}

/// Paints one sextant glyph: up to six filled cells of a 2x3 subdivision.
///
/// The row boundaries are 0, 5, 11, 16 rather than an even 16/3 split: 16 does
/// not divide by three, and rounding every band the same way leaves the bottom
/// band a pixel short, which shows up as a visible seam between vertically
/// adjacent cells in a filled region.
fn paint_sextant(img: &mut RgbaImage, ox: u32, mask: u8) {
    const BANDS: [(u32, u32); 3] = [(0, 5), (5, 11), (11, 16)];
    let mx = CELL_W / 2;
    for (row, &(y0, y1)) in BANDS.iter().enumerate() {
        let bit = row * 2;
        if mask & (1 << bit) != 0 {
            rect(img, ox, 0, y0, mx, y1);
        }
        if mask & (1 << (bit + 1)) != 0 {
            rect(img, ox, mx, y0, CELL_W, y1);
        }
    }
}

/// Paints one braille glyph: up to eight dots in a 2x4 arrangement.
///
/// Bit positions follow the historical braille numbering, the same table
/// [`tilekit::glyphs::BRAILLE_DOTS`] encodes: dots 1-3 run down the left
/// column, 4-6 down the right, then 7 and 8 are the extra bottom pair. Laying
/// them out in raster order instead scrambles the bottom row of every
/// character.
fn paint_braille(img: &mut RgbaImage, ox: u32, bits: u8) {
    // Dot centers. Two columns at x = 2 and 5, four rows spread over the cell
    // with a little margin top and bottom so a fully-set cell still reads as
    // eight dots rather than as a solid block.
    const COLS: [u32; 2] = [1, 4];
    const ROWS: [u32; 4] = [1, 5, 9, 13];
    /// Bit index for `[column][row]`, in braille's historical dot order.
    const DOTS: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];

    for (cx, col) in COLS.iter().enumerate() {
        for (cy, row) in ROWS.iter().enumerate() {
            if bits & (1 << DOTS[cx][cy]) != 0 {
                // 3x3 dots on a 8x16 cell: large enough to see at 1x, small
                // enough that adjacent dots stay visually separate.
                rect(img, ox, *col, *row, col + 3, row + 3);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL_H, CELL_W, build, sextant_char};

    #[test]
    fn the_sheet_covers_every_glyph_cp437_lacks() {
        let sheet = build();
        // 10 quadrants + 60 sextants (the block omits 2 of 64, and 2 more are
        // the CP437 half blocks this sheet must not override) + 256 braille.
        assert_eq!(sheet.chars.len(), 10 + 60 + 256);
        assert_eq!(sheet.image.height(), CELL_H);
        assert_eq!(sheet.image.width(), sheet.chars.len() as u32 * CELL_W);
    }

    #[test]
    fn the_sheet_never_overrides_a_glyph_cp437_already_has() {
        // Every character here would otherwise be drawn by the bitmap font, in
        // the cell's own foreground color. A tileset sprite is not modulated by
        // `fg` (retroglyph#537), so shadowing one of these swaps a colorable
        // glyph for a permanently white one.
        let chars = build().chars;
        for ch in [
            ' ', '\u{2588}', // space and full block
            '\u{2580}', '\u{2584}', // upper and lower halves
            '\u{258C}', '\u{2590}', // left and right halves
            '\u{2591}', '\u{2592}', '\u{2593}', // the shade ramp
        ] {
            assert!(
                !chars.contains(&ch),
                "{ch:?} is in CP437; supplying it from the tileset makes it white-only"
            );
        }
    }

    #[test]
    fn every_character_appears_exactly_once() {
        let mut chars = build().chars;
        let total = chars.len();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), total, "a character is listed twice");
    }

    #[test]
    fn braille_covers_the_whole_block() {
        let chars = build().chars;
        for bits in 0u32..256 {
            let ch = char::from_u32(0x2800 + bits).expect("valid");
            assert!(
                chars.contains(&ch),
                "missing braille U+{:04X}",
                0x2800 + bits
            );
        }
    }

    #[test]
    fn sextant_codepoints_skip_the_two_the_block_omits() {
        // Mask 0 and 63 are a space and a full block, which the Unicode block
        // does not duplicate; masks 21 and 42 map to the pre-existing half
        // blocks rather than into the 1FB00 range.
        assert_eq!(sextant_char(0), None);
        assert_eq!(sextant_char(63), None);
        assert_eq!(sextant_char(21), Some('\u{258C}'));
        assert_eq!(sextant_char(42), Some('\u{2590}'));
        assert_eq!(sextant_char(1), Some('\u{1FB00}'));
        // Mask 22 is the first past the first gap, so it lands one below the
        // naive 0x1FB00 + mask - 1.
        assert_eq!(sextant_char(22), Some('\u{1FB14}'));
    }

    #[test]
    fn every_sextant_codepoint_is_distinct() {
        let mut seen: Vec<char> = (0u8..64).filter_map(sextant_char).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "two masks share a codepoint");
    }

    #[test]
    fn an_empty_braille_cell_draws_nothing_and_a_full_one_draws_eight_dots() {
        let sheet = build();
        let blank = sheet
            .chars
            .iter()
            .position(|&c| c == '\u{2800}')
            .expect("blank braille is in the sheet");
        let full = sheet
            .chars
            .iter()
            .position(|&c| c == '\u{28FF}')
            .expect("full braille is in the sheet");

        let lit = |index: usize| {
            let ox = index as u32 * CELL_W;
            (0..CELL_H)
                .flat_map(|y| (0..CELL_W).map(move |x| (x, y)))
                .filter(|&(x, y)| sheet.image.get_pixel(ox + x, y).0[3] > 0)
                .count()
        };
        assert_eq!(lit(blank), 0, "the blank pattern must draw nothing");
        assert_eq!(lit(full), 8 * 9, "eight 3x3 dots");
    }
}
