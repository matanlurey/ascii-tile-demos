//! Snapshot tests for every demo.
//!
//! Each demo gets three tests: a text snapshot pinning what it draws, a sanity
//! check that it draws anything at all, and a determinism check.
//!
//! A demo's `.rs` file is an `[[example]]` target, which cannot be imported as
//! a normal module, so `#[path]` includes its source directly. The include has
//! to happen at the crate root of this file rather than inside a nested
//! module: `#[path]` on a module nested inside an inline module resolves
//! relative to a directory named after the enclosing module, which does not
//! exist. Every demo is self-contained apart from the harness and tilekit, so
//! including the source outright works. The `main` each one declares via
//! `demo_main!` becomes an unused function here, hence the `dead_code` allow.

#![allow(
    dead_code,
    unreachable_pub,
    unused_imports,
    clippy::pedantic,
    clippy::nursery
)]

mod support;

use support::{assert_draws_a_map, coverage, text_snapshot};

/// How many frames to advance before snapshotting.
///
/// Three, not one: several demos build state lazily on the first tick, and
/// animated ones have barely moved by then. Enough to reach a steady state,
/// few enough that the test stays fast.
const FRAMES: u32 = 3;

/// Pulls one demo's source in as a module.
macro_rules! include_demo {
    ($module:ident, $file:literal) => {
        #[path = $file]
        mod $module;
    };
}

include_demo!(d01, "../examples/01_terrain_cells.rs");
include_demo!(d02, "../examples/02_chunky_tiles.rs");
include_demo!(d03, "../examples/03_dual_grid.rs");
include_demo!(d04, "../examples/04_autotile_gallery.rs");
include_demo!(d05, "../examples/05_iso_diamond.rs");
include_demo!(d06, "../examples/06_iso_elevation.rs");
include_demo!(d07, "../examples/07_hex_tiles.rs");
include_demo!(d08, "../examples/08_hex_outline.rs");
include_demo!(d09, "../examples/09_hex_subcell.rs");
include_demo!(d10, "../examples/10_political.rs");
include_demo!(d11, "../examples/11_fog_of_war.rs");
include_demo!(d12, "../examples/12_relief.rs");
include_demo!(d13, "../examples/13_parchment.rs");
include_demo!(d14, "../examples/14_seasons.rs");
include_demo!(d15, "../examples/15_minimal.rs");
include_demo!(d16, "../examples/16_subcell_canvas.rs");
include_demo!(d17, "../examples/17_tileset_sprites.rs");
include_demo!(d18, "../examples/18_panel_chrome.rs");
include_demo!(d19, "../examples/19_hex_command.rs");
include_demo!(d20, "../examples/20_realm_map.rs");
include_demo!(d21, "../examples/21_deck_plan.rs");
include_demo!(d22, "../examples/22_overworld_quest.rs");
include_demo!(d23, "../examples/23_iso_tactics.rs");
include_demo!(d24, "../examples/24_torchlit_crypt.rs");
include_demo!(d25, "../examples/25_flag_war.rs");
include_demo!(d26, "../examples/26_hexcrawl.rs");

/// Declares the three standard tests for one demo.
macro_rules! demo_tests {
    ($name:ident, $module:ident :: $demo:ident) => {
        mod $name {
            use super::{FRAMES, assert_draws_a_map, coverage, text_snapshot, $module};
            use $module::$demo;

            #[test]
            fn draws_a_map() {
                assert_draws_a_map(coverage::<$demo>(FRAMES), stringify!($demo));
            }

            #[test]
            fn text_snapshot_is_stable() {
                let view = text_snapshot::<$demo>(FRAMES);
                insta::assert_snapshot!(stringify!($name), view);
            }

            #[test]
            fn rendering_is_deterministic() {
                // Two renders at the same frame count must agree. Anything
                // seeded from the clock, from hash map iteration order, or
                // from uninitialized state shows up here rather than as a
                // snapshot that fails once a week.
                assert_eq!(
                    text_snapshot::<$demo>(FRAMES),
                    text_snapshot::<$demo>(FRAMES),
                    "{} is not deterministic",
                    stringify!($demo)
                );
            }
        }
    };
}

demo_tests!(terrain_cells, d01::TerrainCells);
demo_tests!(chunky_tiles, d02::ChunkyTiles);
demo_tests!(dual_grid, d03::DualGridDemo);
demo_tests!(autotile_gallery, d04::AutotileGallery);
demo_tests!(iso_diamond, d05::IsoDiamond);
demo_tests!(iso_elevation, d06::IsoElevation);
demo_tests!(hex_tiles, d07::HexTiles);
demo_tests!(hex_outline, d08::HexOutline);
demo_tests!(hex_subcell, d09::HexSubcell);
demo_tests!(political, d10::Political);
demo_tests!(fog_of_war, d11::FogOfWar);
demo_tests!(relief, d12::Relief);
demo_tests!(parchment, d13::Parchment);
demo_tests!(seasons, d14::Seasons);
demo_tests!(minimal, d15::Minimal);
demo_tests!(subcell_canvas, d16::SubcellCanvas);
demo_tests!(tileset_sprites, d17::TilesetSprites);
demo_tests!(panel_chrome, d18::PanelChrome);
demo_tests!(hex_command, d19::HexCommand);
demo_tests!(realm_map, d20::RealmMap);
demo_tests!(deck_plan, d21::DeckPlan);
demo_tests!(overworld_quest, d22::OverworldQuest);
demo_tests!(iso_tactics, d23::IsoTactics);
demo_tests!(torchlit_crypt, d24::TorchlitCrypt);
demo_tests!(flag_war, d25::FlagWar);
demo_tests!(hexcrawl, d26::Hexcrawl);

/// Every demo must draw to the live grid, not to a fixed one.
///
/// A windowed backend sizes its grid from the window, and the browser build
/// fills the whole viewport, so a demo that hardcodes a width leaves a black
/// band down the side of the page. That is invisible in the default-sized
/// headless snapshot and immediately obvious to a visitor.
mod fills_the_grid {
    use super::*;
    use support::extent;

    /// Deliberately wider and taller than `GRID_COLS`/`GRID_ROWS`, so a demo
    /// that draws to those constants instead of `term.area()` fails here.
    const COLS: u16 = 160;
    const ROWS: u16 = 60;

    fn assert_fills<D: ascii_tile_demos::Demo>(name: &str) {
        let (max_x, max_y) = extent::<D>(COLS, ROWS, FRAMES);
        assert!(
            max_x >= COLS - 2,
            "{name} drew no further right than column {max_x} of {COLS}"
        );
        assert!(
            max_y >= ROWS - 2,
            "{name} drew no further down than row {max_y} of {ROWS}"
        );
    }

    macro_rules! fills {
        ($($test:ident => $module:ident :: $demo:ident),* $(,)?) => {
            $(
                #[test]
                fn $test() {
                    assert_fills::<$module::$demo>(stringify!($demo));
                }
            )*
        };
    }

    fills! {
        terrain_cells => d01::TerrainCells,
        chunky_tiles => d02::ChunkyTiles,
        dual_grid => d03::DualGridDemo,
        autotile_gallery => d04::AutotileGallery,
        iso_diamond => d05::IsoDiamond,
        iso_elevation => d06::IsoElevation,
        hex_tiles => d07::HexTiles,
        hex_outline => d08::HexOutline,
        hex_subcell => d09::HexSubcell,
        political => d10::Political,
        fog_of_war => d11::FogOfWar,
        relief => d12::Relief,
        parchment => d13::Parchment,
        seasons => d14::Seasons,
        minimal => d15::Minimal,
        subcell_canvas => d16::SubcellCanvas,
        tileset_sprites => d17::TilesetSprites,
    }
}
