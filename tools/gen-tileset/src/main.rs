//! Generates `examples/assets/terrain.png`, the sprite sheet used by
//! `17_tileset_sprites`.
//!
//! A committed generator rather than a committed piece of art. Three reasons:
//! the output is license-clean with no attribution to track, it can be
//! regenerated at any tile size if the demo's grid changes, and the generator
//! itself is executable documentation of the sprite sheet format
//! (`retroglyph_window::tileset::TilesetOptions`) in a way a binary PNG can
//! never be.
//!
//! ```sh
//! cargo run -p gen-tileset
//! ```
//!
//! ## Sheet layout
//!
//! Tiles are [`TILE_PX`] x [`TILE_PX`] pixels laid out in a single row, in
//! [`SPRITES`] order. The demo loads them with
//! [`Codepage::Unicode`](retroglyph_window::tileset::Codepage::Unicode)
//! starting at [`FIRST_CODEPOINT`], so sprite `i` is addressed as the glyph
//! `U+E000 + i` -- a Unicode private-use codepoint, which is exactly what the
//! private-use area is for and guarantees no collision with a real character
//! the demo might also want to draw.

use std::path::PathBuf;

use image::{ImageBuffer, Rgba, RgbaImage};

#[allow(unreachable_pub, reason = "a binary crate module, not a published API")]
mod blocks;

/// Tile edge length in pixels.
///
/// 16 matches the embedded Unscii 16 font's cell height exactly, and is twice
/// its 8-pixel width, so a sprite occupies a clean 2x1 block of grid cells
/// with no fractional scaling. Picking a size that is not a whole multiple of
/// the font cell is the fastest way to get sprites that visibly shear against
/// the text drawn beside them.
const TILE_PX: u32 = 16;

/// First codepoint the sheet maps to. `U+E000` is the start of the Basic
/// Multilingual Plane's private use area.
const FIRST_CODEPOINT: u32 = 0xE000;

/// Fully transparent.
const CLEAR: Rgba<u8> = Rgba([0, 0, 0, 0]);

/// One sprite: a name (used only for the manifest this prints) and a painter.
struct Sprite {
    name: &'static str,
    paint: fn(&mut RgbaImage, u32),
}

/// Every sprite in sheet order. The demo's own constants must match this
/// order; the manifest printed at the end of a run is what to check against.
const SPRITES: [Sprite; 12] = [
    Sprite {
        name: "ocean",
        paint: paint_ocean,
    },
    Sprite {
        name: "shallows",
        paint: paint_shallows,
    },
    Sprite {
        name: "sand",
        paint: paint_sand,
    },
    Sprite {
        name: "grass",
        paint: paint_grass,
    },
    Sprite {
        name: "forest",
        paint: paint_forest,
    },
    Sprite {
        name: "conifer",
        paint: paint_conifer,
    },
    Sprite {
        name: "hills",
        paint: paint_hills,
    },
    Sprite {
        name: "mountain",
        paint: paint_mountain,
    },
    Sprite {
        name: "snow",
        paint: paint_snow,
    },
    Sprite {
        name: "desert",
        paint: paint_desert,
    },
    Sprite {
        name: "town",
        paint: paint_town,
    },
    Sprite {
        name: "keep",
        paint: paint_keep,
    },
];

fn main() {
    let cols = SPRITES.len() as u32;
    let mut sheet: RgbaImage = ImageBuffer::from_pixel(cols * TILE_PX, TILE_PX, CLEAR);

    for (index, sprite) in SPRITES.iter().enumerate() {
        (sprite.paint)(&mut sheet, index as u32 * TILE_PX);
    }

    let out = asset_path();
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("failed to create the assets directory");
    }
    sheet.save(&out).expect("failed to write the sprite sheet");

    println!("wrote {} ({cols} sprites at {TILE_PX}px)", out.display());

    // The supplementary block/braille sheet. Separate from the terrain sheet
    // because it serves a different purpose (making glyphs render at all,
    // rather than replacing them with art) and every demo registers it, not
    // just the sprite one.
    let sheet = blocks::build();
    let blocks_out = asset_dir().join("blocks.png");
    sheet
        .image
        .save(&blocks_out)
        .expect("failed to write the block glyph sheet");
    println!(
        "wrote {} ({} glyphs at {}x{}px)",
        blocks_out.display(),
        sheet.chars.len(),
        blocks::CELL_W,
        blocks::CELL_H
    );

    let codepage = sheet.chars.iter().collect::<String>();
    let listing = asset_dir().join("blocks.codepage.txt");
    std::fs::write(&listing, &codepage).expect("failed to write the codepage listing");
    println!("wrote {} ({} chars)", listing.display(), sheet.chars.len());
    println!();
    println!("// Sheet manifest -- 17_tileset_sprites must agree with this order.");
    for (index, sprite) in SPRITES.iter().enumerate() {
        let codepoint = FIRST_CODEPOINT + index as u32;
        println!("// {index:>2}  U+{codepoint:04X}  {}", sprite.name);
    }
}

/// `examples/assets/terrain.png`, resolved from this crate's manifest
/// directory rather than the process working directory, so `cargo run
/// -p gen-tileset` writes to the right place regardless of where it is run
/// from.
fn asset_path() -> PathBuf {
    asset_dir().join("terrain.png")
}

/// `examples/assets/`, resolved from this crate's manifest directory.
fn asset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets")
}

// ── Painting helpers ────────────────────────────────────────────────────────

/// Sets a pixel, given tile-local coordinates and the tile's x origin.
fn put(sheet: &mut RgbaImage, ox: u32, x: u32, y: u32, color: [u8; 4]) {
    if x < TILE_PX && y < TILE_PX {
        sheet.put_pixel(ox + x, y, Rgba(color));
    }
}

/// Fills the whole tile.
fn fill(sheet: &mut RgbaImage, ox: u32, color: [u8; 4]) {
    for y in 0..TILE_PX {
        for x in 0..TILE_PX {
            put(sheet, ox, x, y, color);
        }
    }
}

/// A cheap deterministic hash, so texture speckle is reproducible across runs
/// and platforms. Regenerating the sheet must produce a byte-identical file,
/// or every run shows up as a diff.
const fn speckle(x: u32, y: u32, salt: u32) -> u32 {
    let mut h = x
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(y.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(salt.wrapping_mul(0xC2B2_AE35));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    h
}

/// Scatters `color` over the tile wherever the hash falls below `chance`
/// (out of 256).
fn scatter(sheet: &mut RgbaImage, ox: u32, salt: u32, chance: u32, color: [u8; 4]) {
    for y in 0..TILE_PX {
        for x in 0..TILE_PX {
            if speckle(x, y, salt) % 256 < chance {
                put(sheet, ox, x, y, color);
            }
        }
    }
}

/// Darkens the bottom two rows and lightens the top row, so tiles laid edge to
/// edge read as beveled blocks rather than one continuous field.
fn bevel(sheet: &mut RgbaImage, ox: u32, light: [u8; 4], shadow: [u8; 4]) {
    for x in 0..TILE_PX {
        put(sheet, ox, x, 0, light);
        put(sheet, ox, x, TILE_PX - 1, shadow);
    }
}

/// Draws a filled triangle with its apex at `(apex_x, apex_y)` widening
/// downward to `base_y`. The primitive behind every mountain, tree, and roof
/// in the sheet.
fn triangle(sheet: &mut RgbaImage, ox: u32, apex_x: u32, apex_y: u32, base_y: u32, color: [u8; 4]) {
    let height = base_y.saturating_sub(apex_y);
    for row in 0..=height {
        let half = row / 2;
        let y = apex_y + row;
        for dx in 0..=half {
            put(sheet, ox, apex_x.saturating_sub(dx), y, color);
            put(sheet, ox, apex_x + dx, y, color);
        }
    }
}

// ── Sprites ─────────────────────────────────────────────────────────────────

fn paint_ocean(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [14, 32, 68, 255]);
    // Two offset wave crests rather than random speckle: horizontal strokes
    // read as water, isotropic noise reads as static.
    for x in 0..TILE_PX {
        let crest = 5 + (x / 3) % 2;
        put(sheet, ox, x, crest, [30, 58, 104, 255]);
        put(sheet, ox, x, crest + 6, [26, 50, 94, 255]);
    }
}

fn paint_shallows(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [40, 88, 134, 255]);
    for x in 0..TILE_PX {
        put(sheet, ox, x, 4 + (x / 2) % 2, [72, 126, 170, 255]);
        put(sheet, ox, x, 11 - (x / 2) % 2, [64, 112, 156, 255]);
    }
}

fn paint_sand(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [196, 178, 128, 255]);
    scatter(sheet, ox, 11, 40, [210, 194, 148, 255]);
    scatter(sheet, ox, 12, 24, [176, 158, 110, 255]);
    bevel(sheet, ox, [214, 198, 152, 255], [156, 140, 96, 255]);
}

fn paint_grass(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [92, 130, 62, 255]);
    scatter(sheet, ox, 21, 48, [108, 150, 72, 255]);
    // Short vertical tufts: two pixels tall, so they read as blades rather
    // than as noise.
    for x in (1..TILE_PX).step_by(4) {
        let y = 4 + speckle(x, 0, 22) % 8;
        put(sheet, ox, x, y, [126, 168, 84, 255]);
        put(sheet, ox, x, y + 1, [108, 148, 70, 255]);
    }
    bevel(sheet, ox, [110, 152, 74, 255], [66, 96, 44, 255]);
}

fn paint_forest(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [58, 92, 48, 255]);
    // Three overlapping canopies at different heights, so a field of forest
    // tiles has depth instead of a repeating stamp.
    for (cx, cy) in [(4u32, 4u32), (11, 6), (7, 9)] {
        for y in cy..cy + 5 {
            for x in cx.saturating_sub(3)..=(cx + 3).min(TILE_PX - 1) {
                let dx = x.abs_diff(cx);
                let dy = y - cy;
                if dx + dy / 2 <= 3 {
                    let shade = if dx > 1 { 0 } else { 14 };
                    put(sheet, ox, x, y, [40 + shade, 82 + shade, 40 + shade, 255]);
                }
            }
        }
        put(sheet, ox, cx, cy + 5, [58, 44, 30, 255]);
    }
}

fn paint_conifer(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [44, 72, 58, 255]);
    for (cx, apex) in [(4u32, 2u32), (11, 4), (7, 7)] {
        triangle(sheet, ox, cx, apex, apex + 6, [36, 78, 54, 255]);
        put(sheet, ox, cx, apex + 7, [50, 38, 26, 255]);
    }
}

fn paint_hills(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [118, 124, 74, 255]);
    // Two rounded humps. Half-width per row gives a dome rather than the
    // sharp cone `triangle` produces.
    for (cx, top) in [(5u32, 6u32), (11, 8)] {
        for row in 0..5 {
            let half = 3 - row / 2;
            for dx in 0..=half {
                let y = top + row;
                put(sheet, ox, cx.saturating_sub(dx), y, [136, 142, 88, 255]);
                put(sheet, ox, cx + dx, y, [126, 132, 80, 255]);
            }
        }
    }
    bevel(sheet, ox, [140, 146, 92, 255], [88, 92, 54, 255]);
}

fn paint_mountain(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [94, 90, 86, 255]);
    triangle(sheet, ox, 6, 2, 15, [128, 124, 118, 255]);
    triangle(sheet, ox, 11, 5, 15, [110, 106, 100, 255]);
    // A snow cap on the taller peak only: capping both makes the tile read as
    // flat, since the eye loses the height difference.
    for row in 0..2 {
        for dx in 0..=row {
            put(sheet, ox, 6 - dx, 2 + row, [232, 234, 240, 255]);
            put(sheet, ox, 6 + dx, 2 + row, [220, 222, 230, 255]);
        }
    }
}

fn paint_snow(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [224, 230, 240, 255]);
    scatter(sheet, ox, 31, 30, [240, 244, 250, 255]);
    // A faint blue in the hollows: pure white with white speckle is invisible.
    scatter(sheet, ox, 32, 22, [198, 210, 228, 255]);
    bevel(sheet, ox, [244, 248, 252, 255], [186, 198, 216, 255]);
}

fn paint_desert(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [206, 182, 116, 255]);
    // Dune crests as shallow arcs, each with a shadow line beneath, which is
    // what makes a dune read as a ridge rather than a stripe.
    for (base, offset) in [(5u32, 0u32), (11, 6)] {
        for x in 0..TILE_PX {
            let lift = u32::from((x + offset) % 8 < 4);
            put(sheet, ox, x, base - lift, [222, 200, 138, 255]);
            put(sheet, ox, x, base - lift + 1, [180, 156, 96, 255]);
        }
    }
}

fn paint_town(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [96, 128, 66, 255]);
    // Two houses: wall block, roof triangle, and a lit window. The window is
    // one pixel and does most of the work of saying "inhabited".
    for (x0, y0) in [(2u32, 6u32), (9, 8)] {
        for y in y0..y0 + 5 {
            for x in x0..x0 + 5 {
                put(sheet, ox, x, y, [174, 154, 128, 255]);
            }
        }
        triangle(
            sheet,
            ox,
            x0 + 2,
            y0.saturating_sub(3),
            y0,
            [148, 76, 62, 255],
        );
        put(sheet, ox, x0 + 2, y0 + 2, [246, 214, 130, 255]);
    }
}

fn paint_keep(sheet: &mut RgbaImage, ox: u32) {
    fill(sheet, ox, [104, 122, 72, 255]);
    // Curtain wall with crenellations, plus a taller tower.
    for y in 7..15 {
        for x in 2..14 {
            put(sheet, ox, x, y, [154, 150, 142, 255]);
        }
    }
    for x in (2..14).step_by(3) {
        put(sheet, ox, x, 6, [166, 162, 154, 255]);
        put(sheet, ox, x + 1, 6, [166, 162, 154, 255]);
    }
    for y in 3..15 {
        for x in 6..10 {
            put(sheet, ox, x, y, [176, 172, 164, 255]);
        }
    }
    put(sheet, ox, 7, 10, [40, 34, 30, 255]);
    put(sheet, ox, 8, 10, [40, 34, 30, 255]);
    put(sheet, ox, 7, 11, [40, 34, 30, 255]);
    put(sheet, ox, 8, 11, [40, 34, 30, 255]);
}

#[cfg(test)]
mod tests {
    use super::{FIRST_CODEPOINT, SPRITES, TILE_PX, speckle};

    #[test]
    fn every_sprite_maps_to_a_valid_private_use_codepoint() {
        for index in 0..SPRITES.len() {
            let codepoint = FIRST_CODEPOINT + index as u32;
            let ch = char::from_u32(codepoint).expect("valid codepoint");
            assert!(
                ('\u{E000}'..='\u{F8FF}').contains(&ch),
                "sprite {index} escaped the private use area"
            );
        }
    }

    #[test]
    fn sprite_names_are_unique() {
        let mut names: Vec<_> = SPRITES.iter().map(|s| s.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate sprite name");
    }

    #[test]
    fn the_tile_size_is_a_whole_multiple_of_the_font_cell() {
        // Unscii 16 is 8x16. A sprite that is not a whole multiple of that
        // shears against text drawn beside it.
        assert_eq!(TILE_PX % 8, 0, "width must divide into 8px font cells");
        assert_eq!(TILE_PX % 16, 0, "height must divide into 16px font cells");
    }

    #[test]
    fn speckle_is_deterministic_and_varies() {
        assert_eq!(speckle(3, 4, 5), speckle(3, 4, 5));
        assert_ne!(speckle(3, 4, 5), speckle(4, 3, 5), "axes are symmetric");
        assert_ne!(speckle(3, 4, 5), speckle(3, 4, 6), "salt does nothing");
    }
}
