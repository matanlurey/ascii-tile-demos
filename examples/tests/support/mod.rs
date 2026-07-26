//! Shared snapshot helpers for the demo tests.
//!
//! Two kinds of snapshot, because they catch different things:
//!
//! - **Text** ([`text_snapshot`]) renders against the `Headless` backend and
//!   captures the glyph grid. Cheap, readable in a diff, and it catches layout
//!   and content regressions. It says nothing about color.
//! - **PNG** ([`png_snapshot`]) renders against the software backend's pixel
//!   buffer. Slow, opaque in a diff, and the only thing that catches a color,
//!   sub-cell offset, or block-element regression, none of which exist in the
//!   text view at all.
//!
//! Both drive the demo through the exact same [`Demo::tick`] the real backends
//! use, so a snapshot is a genuine end-to-end render rather than a
//! test-specific code path.

use ascii_tile_demos::{Demo, HEADLESS_COLS, HEADLESS_FRAME_DELTA, HEADLESS_ROWS};
use retroglyph_core::{Frame, Headless, Terminal};

/// Renders `frames` frames of `D` against a headless grid and returns the last
/// one as text.
///
/// The *last* frame, not the first: a demo that animates has usually not
/// settled by frame one, and several build their world or camera state lazily.
/// Advancing a few frames snapshots the demo in its steady state, which is
/// both more representative and more stable.
pub fn text_snapshot<D: Demo>(frames: u32) -> String {
    text_snapshot_at::<D>(HEADLESS_COLS, HEADLESS_ROWS, frames)
}

/// [`text_snapshot`] at an explicit grid size.
///
/// The windowed backends size their grid from the window, so a demo can look
/// right at the snapshot size and wrong at the size a browser actually gives
/// it. This is how to check the second case without a browser.
pub fn text_snapshot_at<D: Demo>(cols: u16, rows: u16, frames: u32) -> String {
    let mut term = Terminal::new(Headless::new(cols, rows));
    let mut demo = D::init(&mut term);
    let mut last = String::new();

    for i in 0..frames.max(1) {
        let frame = Frame {
            delta: HEADLESS_FRAME_DELTA,
            frame: u64::from(i),
        };
        if !demo.tick(&mut term, &frame) {
            break;
        }
        last = term.backend().format_view();
    }
    last
}

/// What a demo actually put on the grid, counted over both channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Cells that were written at all, whether or not they carry a glyph.
    pub written: usize,
    /// Distinct non-space glyphs.
    pub glyphs: usize,
    /// Distinct background colors.
    pub backgrounds: usize,
}

/// Measures what `frames` frames of `D` put on the grid.
///
/// Inspects the presented [`Grid`](retroglyph_core::Grid) rather than the
/// formatted text view, because the text view is glyph-only. Several demos
/// here draw entirely in background color with a space glyph in every cell
/// (`15_minimal` does so by design; `12_relief`'s shaded mode does so
/// incidentally), and to a glyph-only check those are indistinguishable from a
/// demo that draws nothing at all.
///
/// Reads the *backend's* grid, not `Terminal::grid()`. `Terminal::present`
/// swaps its front and back buffers, so by the time `tick` returns,
/// `Terminal::grid()` is the next frame's blank canvas; the frame that was
/// just drawn lives in the backend. Reading the wrong one reports every demo
/// as empty.
pub fn coverage<D: Demo>(frames: u32) -> Coverage {
    use std::collections::HashSet;

    let mut term = Terminal::new(Headless::new(HEADLESS_COLS, HEADLESS_ROWS));
    let mut demo = D::init(&mut term);
    for i in 0..frames.max(1) {
        let frame = Frame {
            delta: HEADLESS_FRAME_DELTA,
            frame: u64::from(i),
        };
        if !demo.tick(&mut term, &frame) {
            break;
        }
    }

    let grid = term.backend().grid();
    let mut written = 0usize;
    let mut glyphs = HashSet::new();
    let mut backgrounds = HashSet::new();

    for layer in 0..=grid.max_layer() {
        let Some(cells) = grid.cells(layer) else {
            continue;
        };
        for (_, _, tile) in cells {
            if tile.is_empty() {
                continue;
            }
            written += 1;
            if tile.glyph() != ' ' {
                glyphs.insert(tile.glyph());
            }
            backgrounds.insert(format!("{:?}", tile.style().background()));
        }
    }

    Coverage {
        written,
        glyphs: glyphs.len(),
        backgrounds: backgrounds.len(),
    }
}

/// Asserts that a demo actually renders a map.
///
/// Deliberately weak, and deliberately applied to every demo. A snapshot test
/// pins whatever the demo currently does, including doing nothing: if a
/// refactor makes a demo render an empty grid, the snapshot diff is a wall of
/// blanks and is easy to accept by mistake. This check cannot be accepted by
/// mistake.
///
/// The bar is "most of the grid was written, and the result varies across at
/// least one of the two channels". A demo that fills the screen with a single
/// flat color and no glyphs is as broken as one that draws nothing, and this
/// catches both without requiring every demo to use glyphs.
pub fn assert_draws_a_map(coverage: Coverage, name: &str) {
    let cells = usize::from(HEADLESS_COLS) * usize::from(HEADLESS_ROWS);
    assert!(
        coverage.written * 2 > cells,
        "{name} wrote only {} of {cells} cells; it is probably blank",
        coverage.written
    );
    assert!(
        coverage.glyphs + coverage.backgrounds >= 4,
        "{name} rendered {} glyphs and {} background colors, which is too \
         uniform to be a map",
        coverage.glyphs,
        coverage.backgrounds
    );
}

/// Renders `frames` frames of `D` against the software backend and returns the
/// pixel buffer as an RGBA PNG.
///
/// Native only: the software backend's headless renderer needs no window, but
/// it does need the `software` feature, and there is no wasm test target here.
///
/// # Panics
///
/// Panics if the software backend fails to build or the PNG fails to encode.
#[cfg(all(feature = "software", not(target_arch = "wasm32")))]
pub fn png_snapshot<D: Demo>(frames: u32) -> Vec<u8> {
    use retroglyph_software::SoftwareBackendBuilder;

    let builder = D::configure_software(
        SoftwareBackendBuilder::new()
            // Smaller than the live grid: a PNG of a 100x40 grid at scale 1 is
            // 800x640, which is a large binary blob to commit per demo per
            // change. This is big enough to show the technique and small
            // enough to review.
            .grid_size(HEADLESS_COLS, HEADLESS_ROWS)
            .scale(1),
    );
    let renderer = builder
        .build()
        .expect("software backend must build")
        .run_headless()
        .expect("headless renderer must build");

    let mut term = Terminal::new(renderer);
    let mut demo = D::init(&mut term);
    for i in 0..frames.max(1) {
        let frame = Frame {
            delta: HEADLESS_FRAME_DELTA,
            frame: u64::from(i),
        };
        if !demo.tick(&mut term, &frame) {
            break;
        }
    }

    let pixels = term.backend().pixels();
    let (cols, rows) = (u32::from(HEADLESS_COLS), u32::from(HEADLESS_ROWS));
    // The backend's cell size, not an assumption: a custom font or a tileset
    // demo changes it, and hardcoding 8x16 would silently produce a truncated
    // or overrun image for exactly the demos most worth snapshotting.
    let width = (pixels.len() as u32).div_ceil(rows.max(1)).max(cols);
    let height = (pixels.len() as u32).div_ceil(width.max(1));

    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for &pixel in pixels {
        rgba.push((pixel >> 16) as u8);
        rgba.push((pixel >> 8) as u8);
        rgba.push(pixel as u8);
        rgba.push(0xFF);
    }

    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    image::ImageEncoder::write_image(
        encoder,
        &rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("PNG encoding must succeed");
    png
}

/// The rightmost and bottom-most written column/row at a given grid size.
///
/// Used to catch a demo that silently draws to a fixed grid instead of the
/// live viewport: on a windowed backend that fills the browser, the grid is
/// whatever the canvas is, and a demo that assumes otherwise leaves a black
/// band down the side.
pub fn extent<D: Demo>(cols: u16, rows: u16, frames: u32) -> (u16, u16) {
    let mut term = Terminal::new(Headless::new(cols, rows));
    let mut demo = D::init(&mut term);
    for i in 0..frames.max(1) {
        let frame = Frame {
            delta: HEADLESS_FRAME_DELTA,
            frame: u64::from(i),
        };
        if !demo.tick(&mut term, &frame) {
            break;
        }
    }
    let grid = term.backend().grid();
    let (mut max_x, mut max_y) = (0u16, 0u16);
    for layer in 0..=grid.max_layer() {
        let Some(cells) = grid.cells(layer) else {
            continue;
        };
        for (x, y, tile) in cells {
            if !tile.is_empty() {
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    (max_x, max_y)
}
