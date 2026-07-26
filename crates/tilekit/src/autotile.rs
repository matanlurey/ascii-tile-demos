//! Autotiling: choosing a tile's appearance from what its neighbours are.
//!
//! The problem every tile map has: you paint a region as "water", and the
//! renderer has to work out which cells are open sea, which are north shore,
//! which are an inside corner where two coasts meet. Autotiling solves it by
//! turning the neighbourhood into a bitmask and looking up an answer.
//!
//! This module implements four approaches, in increasing order of quality and
//! cost:
//!
//! | Approach | Neighbours | Variants | Good for |
//! | --- | --- | --- | --- |
//! | [`mask4`] + [`box_glyph`] | 4 cardinal | 16 | Roads, rivers, walls, borders |
//! | [`mask8`] + [`blob_index`] | 8 | 47 | Terrain edges with proper corners |
//! | [`DualGrid`] | 4 corners | 16 | Smooth organic terrain transitions |
//! | [`marching_case`] | 4 corners | 16 | Isolines, coastline extraction |
//!
//! ## References
//!
//! - The 47-tile blob set: [Bitmask autotile guide](https://jaconir.online/blogs/bitmask-autotile-guide)
//! - Corner-based Wang tiles: [Boris the Brave on 2-corner tiles](http://www.boristhebrave.com/permanent/24/06/cr31/stagecast/wang/2corn.html)
//! - Dual-grid: [Jess Hammer's dual-grid tilemap system](https://github.com/jess-hammer/dual-grid-tilemap-system-godot)
//! - Marching squares: [Catlike Coding](https://catlikecoding.com/unity/tutorials/marching-squares/)

// ── 4-bit cardinal masks ────────────────────────────────────────────────────

/// North neighbour matches.
pub const N: u8 = 0b0001;
/// East neighbour matches.
pub const E: u8 = 0b0010;
/// South neighbour matches.
pub const S: u8 = 0b0100;
/// West neighbour matches.
pub const W: u8 = 0b1000;

/// Builds a 4-bit cardinal mask from the four cardinal neighbours, in
/// clockwise order from north: `[north, east, south, west]`.
///
/// Bit order matches the [`N`]/[`E`]/[`S`]/[`W`] constants. Any order works as
/// long as the lookup table agrees; clockwise-from-north is the convention
/// most tileset templates ship with. Taking an array rather than four separate
/// `bool` parameters is not just a lint appeasement: four positional booleans
/// at a call site are indistinguishable, and transposing two of them produces
/// a map whose corners are subtly wrong.
#[must_use]
pub const fn mask4(neighbors: [bool; 4]) -> u8 {
    (neighbors[0] as u8)
        | ((neighbors[1] as u8) << 1)
        | ((neighbors[2] as u8) << 2)
        | ((neighbors[3] as u8) << 3)
}

/// Single-line box-drawing glyphs indexed by [`mask4`].
///
/// Index 0 (no connections) is a lone `·` rather than a blank, so an isolated
/// road tile is still visible; a blank there would silently swallow one-cell
/// features, which is exactly the case worth seeing when debugging a network.
pub const BOX_SINGLE: [char; 16] = [
    '·', '╵', '╶', '└', '╷', '│', '┌', '├', '╴', '┘', '─', '┴', '┐', '┤', '┬', '┼',
];

/// Double-line box drawing, indexed by [`mask4`].
///
/// Double lines have no single-arm stubs in Unicode, so the four
/// one-connection cases fall back to the nearest single-line stub. A double
/// border drawn against a single-line one reads as a hierarchy (major road vs
/// minor, national border vs provincial), which is why both sets are here.
pub const BOX_DOUBLE: [char; 16] = [
    '·', '╵', '╶', '╚', '╷', '║', '╔', '╠', '╴', '╝', '═', '╩', '╗', '╣', '╦', '╬',
];

/// Heavy box drawing, indexed by [`mask4`].
pub const BOX_HEAVY: [char; 16] = [
    '·', '╹', '╺', '┗', '╻', '┃', '┏', '┣', '╸', '┛', '━', '┻', '┓', '┫', '┳', '╋',
];

/// Rounded-corner box drawing, indexed by [`mask4`].
pub const BOX_ROUNDED: [char; 16] = [
    '·', '╵', '╶', '╰', '╷', '│', '╭', '├', '╴', '╯', '─', '┴', '╮', '┤', '┬', '┼',
];

/// The single-line box glyph for a cardinal mask.
#[must_use]
pub const fn box_glyph(mask: u8) -> char {
    BOX_SINGLE[(mask & 0x0F) as usize]
}

// ── 8-bit blob masks ────────────────────────────────────────────────────────

/// Northwest diagonal matches.
pub const NW: u8 = 0b0000_0001;
/// North matches.
pub const N8: u8 = 0b0000_0010;
/// Northeast diagonal matches.
pub const NE: u8 = 0b0000_0100;
/// West matches.
pub const W8: u8 = 0b0000_1000;
/// East matches.
pub const E8: u8 = 0b0001_0000;
/// Southwest diagonal matches.
pub const SW: u8 = 0b0010_0000;
/// South matches.
pub const S8: u8 = 0b0100_0000;
/// Southeast diagonal matches.
pub const SE: u8 = 0b1000_0000;

/// Builds an 8-bit mask from the eight neighbours, in reading order:
/// `[NW, N, NE, W, E, SW, S, SE]`.
#[must_use]
pub const fn mask8(neighbors: [bool; 8]) -> u8 {
    let mut mask = 0u8;
    let mut i = 0;
    while i < 8 {
        if neighbors[i] {
            mask |= 1 << i;
        }
        i += 1;
    }
    mask
}

/// Normalizes an 8-bit mask by discarding diagonals that can't be seen.
///
/// This is the insight that collapses 256 combinations down to 47: a diagonal
/// neighbour only affects a tile's appearance if *both* adjacent cardinals
/// also match. If the north and east cells are empty, it makes no visual
/// difference whether the northeast cell is filled, because the corner is
/// already an outside corner either way. Clearing those irrelevant bits maps
/// many raw masks onto the same normalized one.
#[must_use]
pub const fn normalize8(mask: u8) -> u8 {
    let mut out = mask;
    if mask & N8 == 0 || mask & W8 == 0 {
        out &= !NW;
    }
    if mask & N8 == 0 || mask & E8 == 0 {
        out &= !NE;
    }
    if mask & S8 == 0 || mask & W8 == 0 {
        out &= !SW;
    }
    if mask & S8 == 0 || mask & E8 == 0 {
        out &= !SE;
    }
    out
}

/// The 47 normalized masks, ascending. Index into this with [`blob_index`].
///
/// Built once at first use rather than hardcoded: a hand-written table of 47
/// magic bytes is unreviewable and one transposed digit produces a bug that
/// only shows on one rare corner configuration.
static BLOB_MASKS: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    let mut masks: Vec<u8> = (0u16..=255).map(|m| normalize8(m as u8)).collect();
    masks.sort_unstable();
    masks.dedup();
    masks
});

/// The number of distinct normalized 8-neighbour configurations: 47.
///
/// The famous number behind every "47-tile blob set" tileset template.
#[must_use]
pub fn blob_count() -> usize {
    BLOB_MASKS.len()
}

/// Maps an 8-bit mask to a dense index in `0..47`.
///
/// The index a 47-tile blob tileset is laid out against. Two raw masks that
/// differ only in an invisible diagonal map to the same index, which is the
/// whole point.
#[must_use]
pub fn blob_index(mask: u8) -> usize {
    let normalized = normalize8(mask);
    BLOB_MASKS
        .binary_search(&normalized)
        .unwrap_or_else(|_| unreachable!("normalize8 is closed over its own output"))
}

/// Which of a tile's four corners are inside corners: places where both
/// adjacent cardinals match but the diagonal between them does not.
///
/// Returns `[NW, NE, SW, SE]`. An inside corner is the little notch where two
/// walls meet and a third cell pokes into the join; it needs a different glyph
/// from both the straight edge and the outside corner, and it is the case
/// naive 4-bit autotiling cannot express at all.
#[must_use]
pub const fn inside_corners(mask: u8) -> [bool; 4] {
    [
        mask & N8 != 0 && mask & W8 != 0 && mask & NW == 0,
        mask & N8 != 0 && mask & E8 != 0 && mask & NE == 0,
        mask & S8 != 0 && mask & W8 != 0 && mask & SW == 0,
        mask & S8 != 0 && mask & E8 != 0 && mask & SE == 0,
    ]
}

// ── Dual grid ───────────────────────────────────────────────────────────────

/// The dual-grid tiling scheme: display tiles sit at the *corners* of world
/// tiles, so each display tile sees a 2x2 block of world cells.
///
/// This is the trick that gets smooth organic terrain out of 16 tiles instead
/// of 47. Because a display tile straddles four world cells, every terrain
/// boundary automatically falls through the middle of a display tile rather
/// than along its edge, so the transition is drawn *inside* a tile where it
/// can curve, instead of at a seam where it must be straight.
///
/// The cost is a half-tile offset: the display grid is one tile larger in each
/// dimension and shifted half a tile up and left. Forgetting that offset is
/// the single most common way to get dual-grid wrong, so
/// [`display_origin`](Self::display_origin) exists to make it explicit.
///
/// See [Jess Hammer's implementation](https://github.com/jess-hammer/dual-grid-tilemap-system-godot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DualGrid;

impl DualGrid {
    /// Corner mask from a 2x2 block of world cells, in reading order:
    /// `[top_left, top_right, bottom_left, bottom_right]`.
    ///
    /// Bit order is chosen so the mask reads as a binary picture of the block:
    /// bit 0 top-left, bit 1 top-right, bit 2 bottom-left, bit 3 bottom-right.
    #[must_use]
    pub const fn corner_mask(block: [bool; 4]) -> u8 {
        (block[0] as u8)
            | ((block[1] as u8) << 1)
            | ((block[2] as u8) << 2)
            | ((block[3] as u8) << 3)
    }

    /// The world cell coordinates a display tile at `(col, row)` samples.
    ///
    /// Returns `[top_left, top_right, bottom_left, bottom_right]` as
    /// `(col, row)` pairs, already shifted by the dual grid's half-tile
    /// offset. Callers clamp or treat out-of-bounds as "not the terrain",
    /// which is what makes a map's outer edge render as a coastline rather
    /// than a hard cut.
    #[must_use]
    pub const fn samples(col: i32, row: i32) -> [(i32, i32); 4] {
        [
            (col - 1, row - 1),
            (col, row - 1),
            (col - 1, row),
            (col, row),
        ]
    }

    /// The display grid's origin offset, in half-tiles.
    ///
    /// The display grid is drawn shifted up and left by half a tile. In a
    /// character grid that means shifting by `tile_size / 2` cells, which is
    /// why the dual-grid demo uses even tile sizes.
    #[must_use]
    pub const fn display_origin(tile_w: i32, tile_h: i32) -> (i32, i32) {
        (-tile_w / 2, -tile_h / 2)
    }

    /// A shade-ramp glyph approximating how much of a display tile is covered.
    ///
    /// The character-grid stand-in for a dual-grid tileset's 16 hand-drawn
    /// transition sprites: with only one glyph per cell there is nowhere to
    /// draw a curve, so coverage is expressed as density instead. Renderers
    /// that give a display tile several cells should draw the real corner
    /// shape instead; this is for the one-cell-per-tile case.
    #[must_use]
    pub const fn coverage_glyph(mask: u8) -> char {
        match (mask & 0x0F).count_ones() {
            0 => ' ',
            1 => '░',
            2 => '▒',
            3 => '▓',
            _ => '█',
        }
    }

    /// A quadrant block element drawing exactly which corners are covered.
    ///
    /// Strictly better than [`coverage_glyph`](Self::coverage_glyph) when the
    /// font has quadrant coverage: it shows *which* corners are set, not just
    /// how many, so a diagonal boundary reads as a diagonal instead of a
    /// uniform mid-gray. Requires a font with Unicode block elements, which
    /// the embedded bitmap font has and a bare VGA terminal may not.
    #[must_use]
    pub const fn quadrant_glyph(mask: u8) -> char {
        // Index order matches corner_mask's bit order.
        const QUADS: [char; 16] = [
            ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
        ];
        QUADS[(mask & 0x0F) as usize]
    }
}

// ── Marching squares ────────────────────────────────────────────────────────

/// The marching-squares case for a 2x2 sample block, in reading order:
/// `[top_left, top_right, bottom_left, bottom_right]`.
///
/// Same 16 cases as [`DualGrid::corner_mask`], viewed differently: dual-grid
/// asks "what does this tile look like", marching squares asks "where does the
/// boundary cross this tile". Cases 6 and 9 are the ambiguous saddles, where
/// two opposite corners are set and the contour could be drawn either as two
/// separate corners or as a single crossing; [`marching_glyph`] resolves them
/// consistently rather than picking per-cell, since an inconsistent choice
/// makes a contour line fork and rejoin at random.
#[must_use]
pub const fn marching_case(block: [bool; 4]) -> u8 {
    DualGrid::corner_mask(block)
}

/// A glyph approximating the contour line through a marching-squares case.
///
/// Diagonal cases use `/` and `\`, straight cases use box-drawing halves, and
/// the fully-inside and fully-outside cases draw nothing (there is no boundary
/// to draw). Good enough to trace a coastline or a contour line at one glyph
/// per cell.
#[must_use]
pub const fn marching_glyph(case: u8) -> char {
    match case & 0x0F {
        // Uniform: no boundary crosses this cell.
        0 | 15 => ' ',
        // One corner cut off, or three: the boundary runs corner to corner.
        1 | 7 | 8 | 14 => '\\',
        2 | 4 | 11 | 13 => '/',
        // A whole edge is inside: the boundary runs straight across.
        3 | 12 => '─',
        // Vertical split, plus the two saddles (6 and 9), resolved to a single
        // crossing rather than two corners. Choosing per-cell would make a
        // contour fork and rejoin at random.
        _ => '│',
    }
}

/// Whether a marching-squares case sits on a boundary at all.
#[must_use]
pub const fn is_boundary(case: u8) -> bool {
    let c = case & 0x0F;
    c != 0 && c != 15
}

#[cfg(test)]
mod tests {
    use super::{
        BOX_DOUBLE, BOX_HEAVY, BOX_ROUNDED, BOX_SINGLE, DualGrid, E, E8, N, N8, NE, NW, S, S8, SE,
        SW, W, W8, blob_count, blob_index, box_glyph, inside_corners, is_boundary, marching_case,
        marching_glyph, mask4, mask8, normalize8,
    };

    // ── 4-bit ───────────────────────────────────────────────────────────────

    #[test]
    fn mask4_sets_one_bit_per_direction() {
        assert_eq!(mask4([false, false, false, false]), 0);
        assert_eq!(mask4([true, false, false, false]), N);
        assert_eq!(mask4([false, true, false, false]), E);
        assert_eq!(mask4([false, false, true, false]), S);
        assert_eq!(mask4([false, false, false, true]), W);
        assert_eq!(mask4([true, true, true, true]), N | E | S | W);
    }

    #[test]
    fn box_glyphs_match_their_connections() {
        // The four-way junction, the two straights, and the four corners are
        // the cases a wrong table gets visibly wrong.
        assert_eq!(box_glyph(N | E | S | W), '┼');
        assert_eq!(box_glyph(N | S), '│');
        assert_eq!(box_glyph(E | W), '─');
        assert_eq!(box_glyph(S | E), '┌');
        assert_eq!(box_glyph(S | W), '┐');
        assert_eq!(box_glyph(N | E), '└');
        assert_eq!(box_glyph(N | W), '┘');
        assert_eq!(box_glyph(0), '·', "isolated tiles stay visible");
    }

    #[test]
    fn every_box_set_covers_all_sixteen_cases_distinctly() {
        for set in [BOX_SINGLE, BOX_DOUBLE, BOX_HEAVY, BOX_ROUNDED] {
            assert_eq!(set.len(), 16);
            // The four-way case must be unique to it; a duplicated junction
            // glyph is the usual sign of a mis-transcribed table.
            let junction = set[15];
            assert_eq!(
                set.iter().filter(|&&c| c == junction).count(),
                1,
                "junction glyph {junction:?} repeats"
            );
        }
    }

    #[test]
    fn box_glyph_ignores_high_bits() {
        assert_eq!(box_glyph(0xF0 | N | S), '│');
    }

    // ── 8-bit blob ──────────────────────────────────────────────────────────

    #[test]
    fn mask8_reads_neighbors_in_order() {
        assert_eq!(mask8([false; 8]), 0);
        assert_eq!(
            mask8([true, false, false, false, false, false, false, false]),
            NW
        );
        assert_eq!(
            mask8([false, true, false, false, false, false, false, false]),
            N8
        );
        assert_eq!(
            mask8([false, false, false, false, false, false, false, true]),
            SE
        );
        assert_eq!(mask8([true; 8]), 0xFF);
    }

    #[test]
    fn there_are_exactly_forty_seven_blob_cases() {
        assert_eq!(blob_count(), 47, "the canonical blob-set size");
    }

    #[test]
    fn normalize_drops_diagonals_without_both_cardinals() {
        // NW set but neither N nor W: the corner is invisible, so drop it.
        // Only the diagonal bit is affected; cardinals always survive.
        assert_eq!(normalize8(NW), 0);
        assert_eq!(normalize8(NW | N8), N8, "needs W too");
        assert_eq!(normalize8(NW | W8), W8, "needs N too");
        assert_eq!(normalize8(NW | N8 | W8), NW | N8 | W8, "now it matters");
        // Every diagonal behaves the same way.
        assert_eq!(normalize8(NE | N8 | E8), NE | N8 | E8);
        assert_eq!(normalize8(SW | S8), S8);
        assert_eq!(normalize8(SE | E8), E8);
    }

    #[test]
    fn normalize_is_idempotent() {
        for m in 0u16..=255 {
            let once = normalize8(m as u8);
            assert_eq!(normalize8(once), once, "mask {m:#010b}");
        }
    }

    #[test]
    fn blob_index_is_dense_and_total() {
        let mut seen = vec![false; blob_count()];
        for m in 0u16..=255 {
            let i = blob_index(m as u8);
            assert!(i < blob_count(), "index {i} out of range for {m}");
            seen[i] = true;
        }
        assert!(seen.iter().all(|&s| s), "some blob indices are unreachable");
    }

    #[test]
    fn masks_differing_only_in_hidden_diagonals_share_an_index() {
        // No cardinals at all: every diagonal combination is the same tile.
        let base = blob_index(0);
        for diag in [NW, NE, SW, SE] {
            assert_eq!(blob_index(diag), base, "diagonal {diag:#010b} leaked");
        }
        // But with both cardinals present, the diagonal must matter.
        assert_ne!(blob_index(N8 | W8), blob_index(N8 | W8 | NW));
    }

    #[test]
    fn inside_corners_need_both_cardinals_and_a_missing_diagonal() {
        assert_eq!(inside_corners(0), [false; 4]);
        assert_eq!(inside_corners(N8 | W8), [true, false, false, false]);
        assert_eq!(
            inside_corners(N8 | W8 | NW),
            [false; 4],
            "diagonal fills it"
        );
        assert_eq!(inside_corners(S8 | E8), [false, false, false, true]);
        assert_eq!(
            inside_corners(N8 | E8 | S8 | W8),
            [true; 4],
            "a lone hole has four inside corners"
        );
    }

    // ── Dual grid ───────────────────────────────────────────────────────────

    #[test]
    fn corner_mask_reads_the_block_as_a_picture() {
        assert_eq!(DualGrid::corner_mask([false; 4]), 0b0000);
        assert_eq!(DualGrid::corner_mask([true, false, false, false]), 0b0001);
        assert_eq!(DualGrid::corner_mask([false, false, false, true]), 0b1000);
        assert_eq!(DualGrid::corner_mask([true; 4]), 0b1111);
    }

    #[test]
    fn samples_are_the_four_cells_up_and_left_of_the_display_tile() {
        assert_eq!(
            DualGrid::samples(3, 5),
            [(2, 4), (3, 4), (2, 5), (3, 5)],
            "display tile straddles the world-cell corner"
        );
        // Must work at the origin and below it, where a naive unsigned
        // implementation would wrap.
        assert_eq!(
            DualGrid::samples(0, 0),
            [(-1, -1), (0, -1), (-1, 0), (0, 0)]
        );
    }

    #[test]
    fn display_origin_is_half_a_tile_up_and_left() {
        assert_eq!(DualGrid::display_origin(8, 4), (-4, -2));
        assert_eq!(DualGrid::display_origin(2, 2), (-1, -1));
    }

    #[test]
    fn coverage_glyph_is_monotonic_in_corner_count() {
        assert_eq!(DualGrid::coverage_glyph(0b0000), ' ');
        assert_eq!(DualGrid::coverage_glyph(0b0001), '░');
        assert_eq!(DualGrid::coverage_glyph(0b0011), '▒');
        assert_eq!(DualGrid::coverage_glyph(0b0111), '▓');
        assert_eq!(DualGrid::coverage_glyph(0b1111), '█');
    }

    #[test]
    fn quadrant_glyph_distinguishes_every_corner_pattern() {
        let mut glyphs: Vec<char> = (0u8..16).map(DualGrid::quadrant_glyph).collect();
        let total = glyphs.len();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), total, "quadrant glyphs must all differ");
        // Endpoints anchor the bit order.
        assert_eq!(DualGrid::quadrant_glyph(0b0000), ' ');
        assert_eq!(DualGrid::quadrant_glyph(0b1111), '█');
        assert_eq!(DualGrid::quadrant_glyph(0b0001), '▘', "top-left only");
        assert_eq!(DualGrid::quadrant_glyph(0b1000), '▗', "bottom-right only");
    }

    // ── Marching squares ────────────────────────────────────────────────────

    #[test]
    fn marching_case_agrees_with_the_dual_grid_mask() {
        for i in 0u8..16 {
            let block = [i & 1 != 0, i & 2 != 0, i & 4 != 0, i & 8 != 0];
            assert_eq!(marching_case(block), DualGrid::corner_mask(block));
        }
    }

    #[test]
    fn only_the_uniform_cases_are_not_boundaries() {
        assert!(!is_boundary(0));
        assert!(!is_boundary(15));
        for case in 1u8..15 {
            assert!(is_boundary(case), "case {case} should be a boundary");
        }
    }

    #[test]
    fn uniform_cases_draw_nothing() {
        assert_eq!(marching_glyph(0), ' ');
        assert_eq!(marching_glyph(15), ' ');
    }

    #[test]
    fn complementary_cases_draw_the_same_line() {
        // Inverting inside and outside must not change where the boundary is,
        // only which side is which.
        for case in 1u8..15 {
            if case == 6 || case == 9 {
                continue; // saddles are resolved by fiat; see marching_glyph
            }
            assert_eq!(
                marching_glyph(case),
                marching_glyph(15 - case),
                "case {case} and its complement disagree"
            );
        }
    }

    #[test]
    fn saddle_cases_resolve_consistently() {
        assert_eq!(marching_glyph(6), marching_glyph(9));
    }
}
