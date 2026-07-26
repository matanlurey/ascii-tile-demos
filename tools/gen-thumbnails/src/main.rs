//! Renders one gallery thumbnail per demo, plus an animation check.
//!
//! The gallery index needs a picture of each demo. Screenshotting the built
//! web pages would need a browser, a headless driver, and a GPU that CI does
//! not have; the software backend already rasterizes the exact same grid to
//! the exact same pixels with none of that, which is the whole point of it
//! having a headless mode. So this renders natively, in one process, in about
//! a minute, and the result is deterministic.
//!
//! It also asserts every demo animates. Each demo here advances on its own,
//! and the ways that silently stop (a tick that no longer reads
//! [`Frame::delta`], a cached frame that is never invalidated) produce a page
//! that looks perfectly correct in a screenshot. One frame cannot catch that;
//! two, compared, can. Cheap to add here because the renderer is already
//! running, and it fails the build with a name instead of a blank stare.
//!
//! Note this checks that a demo *would* animate when ticked. Whether the
//! browser's event loop actually keeps ticking it is a different question,
//! answered by the harness's frame pump.
//!
//! ```sh
//! cargo run -p gen-thumbnails -- dist
//! ```

use std::path::{Path, PathBuf};

use ascii_tile_demos::{Demo, GRID_COLS, GRID_ROWS, block_tileset};
use retroglyph_core::{Frame, Output, Terminal};
use retroglyph_software::SoftwareBackendBuilder;
use retroglyph_window::Presenter;

/// Pulls one demo's source in as a module.
///
/// The same trick the snapshot tests use, and for the same reason: an
/// `[[example]]` target is a binary, so there is no library to import and no
/// way to reach the `Demo` impl other than compiling the source again here.
/// The demo's generated `main` comes along and is simply never called.
macro_rules! include_demo {
    ($module:ident, $file:literal) => {
        // The demo types are `pub` so the snapshot tests can name them; in a
        // binary crate's private module that reads as unreachable.
        #[allow(
            unreachable_pub,
            reason = "the demo sources are shared with the test harness"
        )]
        #[path = $file]
        mod $module;
    };
}

include_demo!(d01, "../../../examples/examples/01_terrain_cells.rs");
include_demo!(d02, "../../../examples/examples/02_chunky_tiles.rs");
include_demo!(d03, "../../../examples/examples/03_dual_grid.rs");
include_demo!(d04, "../../../examples/examples/04_autotile_gallery.rs");
include_demo!(d05, "../../../examples/examples/05_iso_diamond.rs");
include_demo!(d06, "../../../examples/examples/06_iso_elevation.rs");
include_demo!(d07, "../../../examples/examples/07_hex_tiles.rs");
include_demo!(d08, "../../../examples/examples/08_hex_outline.rs");
include_demo!(d09, "../../../examples/examples/09_hex_subcell.rs");
include_demo!(d10, "../../../examples/examples/10_political.rs");
include_demo!(d11, "../../../examples/examples/11_fog_of_war.rs");
include_demo!(d12, "../../../examples/examples/12_relief.rs");
include_demo!(d13, "../../../examples/examples/13_parchment.rs");
include_demo!(d14, "../../../examples/examples/14_seasons.rs");
include_demo!(d15, "../../../examples/examples/15_minimal.rs");
include_demo!(d16, "../../../examples/examples/16_subcell_canvas.rs");
include_demo!(d17, "../../../examples/examples/17_tileset_sprites.rs");

/// Frames to advance before the thumbnail is taken.
///
/// Not frame one: several demos build their world or camera lazily, and the
/// ones that fade or sweep have not reached anything representative yet. A
/// second of simulated time is past every intro and still well before the
/// slow cycles (seasons, day/night) drift away from their opening state.
const SETTLE_FRAMES: u32 = 60;

/// Frame offsets past [`SETTLE_FRAMES`] to compare the thumbnail against.
///
/// Several rather than one because "animates" and "differs from one specific
/// later instant" are not the same claim. `16_subcell_canvas` sweeps a narrow
/// highlight across the field on an 8.3-second cycle, and its effect is a
/// dither-threshold nudge that only shows where the terrain sits near the
/// threshold, so two arbitrary instants can genuinely match while the demo is
/// working perfectly. Spreading samples over a few seconds costs a second of
/// build time and removes that whole class of false alarm.
///
/// A demo passes if *any* sample differs, which still catches the regression
/// this exists for: a genuinely frozen demo matches at every offset.
const ANIMATION_FRAMES: &[u32] = &[30, 120, 240, 420];

/// Simulated time per frame. A fixed step, not wall-clock, so a loaded CI
/// machine renders the same thumbnail as a laptop.
const FRAME_DELTA: std::time::Duration = std::time::Duration::from_millis(1000 / 60);

/// Minimum pixels that must change for a demo to count as animating.
///
/// A token threshold rather than 1: it only has to separate "genuinely still"
/// from "something moved", and every demo here moves far more than this.
const MIN_CHANGED_PIXELS: usize = 4;

/// Demos that are correctly still when left alone.
///
/// An explicit list rather than a softer check, because "this demo stopped
/// moving" is exactly the regression worth failing on and a threshold low
/// enough to accommodate a static demo would not catch it anywhere else.
/// Adding an entry should mean the demo is *designed* to sit still, not that
/// it is inconveniently failing.
const STATIC_BY_DESIGN: &[(&str, &str)] = &[(
    "12_relief",
    "the sun orbits only while `O` is toggled on, so that the relief-inversion \
     point can be studied at a fixed azimuth",
)];

/// Demos this tool cannot render faithfully, and why.
///
/// A wrong thumbnail is worse than none: it tells a visitor the demo is broken
/// when the page they would land on is fine. The gallery falls back to a
/// placeholder card for anything listed here.
///
/// `17_tileset_sprites` renders its 16x16 sprites over two 8x16 cells via the
/// tileset's `spacing(2, 1)`. The surfaced renderer honors that; the headless
/// one blits only the sprite's first cell, so every second column keeps
/// whatever was underneath and the map comes out in vertical stripes. The live
/// page is correct, which is why this is a fidelity limit of the headless
/// renderer rather than a demo bug. Confirmed independent of grid size and of
/// whether the block tileset is also registered.
const UNRENDERABLE_HEADLESS: &[(&str, &str)] = &[(
    "17_tileset_sprites",
    "the headless renderer blits multi-cell sprites into a single cell",
)];

/// Builds the backend a demo would get from `run_software`.
///
/// Fidelity is the whole job: a thumbnail built from a different backend
/// configuration is a picture of something no visitor will ever see. The block
/// tileset in particular is not optional, because without it every braille,
/// quadrant, and sextant glyph falls back to CP437's solid block and the
/// sub-cell demos render as a featureless slab of foreground color.
fn backend<D: Demo>() -> SoftwareBackendBuilder {
    D::configure_software(
        SoftwareBackendBuilder::new()
            .grid_size(GRID_COLS, GRID_ROWS)
            .scale(1)
            .tileset(block_tileset()),
    )
}

/// Renders `frames` frames and returns the pixel buffer, its width, and height.
fn render<D: Demo>(frames: u32) -> (Vec<u32>, u32, u32) {
    let renderer = backend::<D>()
        .build()
        .expect("software backend must build")
        .run_headless()
        .expect("headless renderer must build");

    let mut term = Terminal::new(renderer);
    let mut demo = D::init(&mut term);
    for i in 0..frames.max(1) {
        let frame = Frame {
            delta: FRAME_DELTA,
            frame: u64::from(i),
        };
        if !demo.tick(&mut term, &frame) {
            break;
        }
    }

    // Ask the presenter for its cell size rather than deriving dimensions from
    // the buffer length. Length alone only gives the *product* of the two, so
    // dividing by the row count yields cells-tall-by-far-too-wide rather than
    // the real pixel rectangle, and hardcoding 8x16 would be wrong for the
    // tileset demo, which is the one most worth looking at.
    let (cell_w, cell_h) = term.backend().cell_size();
    let grid = term.backend().size();
    let width = u32::from(grid.width) * cell_w;
    let height = u32::from(grid.height) * cell_h;

    let pixels = term.backend().pixels().to_vec();
    assert_eq!(
        pixels.len(),
        (width * height) as usize,
        "pixel buffer does not match {width}x{height}"
    );
    (pixels, width, height)
}

/// Writes `pixels` to `path` as a PNG.
fn write_png(path: &Path, pixels: &[u32], width: u32, height: u32) -> std::io::Result<()> {
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for &pixel in pixels {
        rgba.push((pixel >> 16) as u8);
        rgba.push((pixel >> 8) as u8);
        rgba.push(pixel as u8);
        rgba.push(0xFF);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let encoder = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    image::ImageEncoder::write_image(
        encoder,
        &rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(std::io::Error::other)
}

/// Renders one demo's thumbnail and reports the most pixels it moved across
/// any of the [`ANIMATION_FRAMES`] samples.
///
/// Still runs the animation check for [`UNRENDERABLE_HEADLESS`] demos: the
/// pixels are wrong, but "did any of them change" is still a valid question
/// and still catches a frozen tick.
fn thumbnail<D: Demo>(out: &Path, slug: &str) -> std::io::Result<usize> {
    let (settled, width, height) = render::<D>(SETTLE_FRAMES);

    let mut changed = 0;
    for offset in ANIMATION_FRAMES {
        let (later, _, _) = render::<D>(SETTLE_FRAMES + offset);
        let diff = settled.iter().zip(&later).filter(|(a, b)| a != b).count();
        changed = changed.max(diff);
        // Nothing to gain from the slower samples once one has answered the
        // question, and this is the difference between a fast tool and a slow
        // one.
        if changed >= MIN_CHANGED_PIXELS {
            break;
        }
    }

    if !UNRENDERABLE_HEADLESS.iter().any(|(name, _)| *name == slug) {
        write_png(&out.join(slug).join("thumb.png"), &settled, width, height)?;
    }
    Ok(changed)
}

/// Calls [`thumbnail`] for one demo and records a failure if it did not move.
macro_rules! capture {
    ($out:expr, $still:expr, $slug:literal, $ty:path) => {{
        let changed = thumbnail::<$ty>($out, $slug)?;
        let moved = changed >= MIN_CHANGED_PIXELS;
        let expected_still = STATIC_BY_DESIGN.iter().any(|(slug, _)| *slug == $slug);
        println!(
            "  {:<22} {changed:>7} px changed  {}",
            $slug,
            match (moved, expected_still) {
                _ if UNRENDERABLE_HEADLESS.iter().any(|(n, _)| *n == $slug) =>
                    "animating, no thumbnail (see UNRENDERABLE_HEADLESS)",
                (true, false) => "animating",
                (false, true) => "still (by design)",
                (true, true) => "animating (expected still)",
                (false, false) => "STILL",
            }
        );
        if !moved && !expected_still {
            $still.push($slug);
        }
    }};
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "dist".to_string()),
    );

    let mut still = Vec::new();
    capture!(&out, still, "01_terrain_cells", d01::TerrainCells);
    capture!(&out, still, "02_chunky_tiles", d02::ChunkyTiles);
    capture!(&out, still, "03_dual_grid", d03::DualGridDemo);
    capture!(&out, still, "04_autotile_gallery", d04::AutotileGallery);
    capture!(&out, still, "05_iso_diamond", d05::IsoDiamond);
    capture!(&out, still, "06_iso_elevation", d06::IsoElevation);
    capture!(&out, still, "07_hex_tiles", d07::HexTiles);
    capture!(&out, still, "08_hex_outline", d08::HexOutline);
    capture!(&out, still, "09_hex_subcell", d09::HexSubcell);
    capture!(&out, still, "10_political", d10::Political);
    capture!(&out, still, "11_fog_of_war", d11::FogOfWar);
    capture!(&out, still, "12_relief", d12::Relief);
    capture!(&out, still, "13_parchment", d13::Parchment);
    capture!(&out, still, "14_seasons", d14::Seasons);
    capture!(&out, still, "15_minimal", d15::Minimal);
    capture!(&out, still, "16_subcell_canvas", d16::SubcellCanvas);
    capture!(&out, still, "17_tileset_sprites", d17::TilesetSprites);

    if !still.is_empty() {
        eprintln!(
            "\n{} demo(s) rendered an identical frame at every sampled offset: {}\n\
             Every demo here is supposed to animate on its own, so this is either a tick that \
             stopped reading Frame::delta or a cached frame that is never invalidated. If the \
             demo is meant to sit still, add it to STATIC_BY_DESIGN with a reason.",
            still.len(),
            still.join(", ")
        );
        std::process::exit(1);
    }

    println!(
        "\nWrote {} thumbnails to {}",
        17 - UNRENDERABLE_HEADLESS.len(),
        out.display()
    );
    Ok(())
}
