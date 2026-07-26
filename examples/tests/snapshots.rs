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
include_demo!(d27, "../examples/27_rhythm_crypt.rs");
include_demo!(d28, "../examples/28_spire_deck.rs");
include_demo!(d29, "../examples/29_ship_breach.rs");
include_demo!(d30, "../examples/30_fleet_command.rs");
include_demo!(d31, "../examples/31_dice_tactics.rs");
include_demo!(d32, "../examples/32_loop_track.rs");
include_demo!(d33, "../examples/33_onebit_quest.rs");
include_demo!(d34, "../examples/34_ice_breach.rs");
include_demo!(d35, "../examples/35_stealth_grid.rs");
include_demo!(d36, "../examples/36_court_reigns.rs");

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
demo_tests!(rhythm_crypt, d27::RhythmCrypt);
demo_tests!(spire_deck, d28::SpireDeck);
demo_tests!(ship_breach, d29::ShipBreach);
demo_tests!(fleet_command, d30::FleetCommand);
demo_tests!(dice_tactics, d31::DiceTactics);
demo_tests!(loop_track, d32::LoopTrack);
demo_tests!(onebit_quest, d33::OnebitQuest);
demo_tests!(ice_breach, d34::IceBreach);
demo_tests!(stealth_grid, d35::StealthGrid);
demo_tests!(court_reigns, d36::CourtReigns);

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

/// Every touch-era demo must still lay out at the two grid shapes a phone
/// actually produces.
///
/// The snapshot tests all run at 80x24, which is neither of them. A browser
/// filling the viewport at the capped device pixel ratio hands over roughly
/// 73x79 cells in portrait and 158x36 in landscape (see `ui::touch`), and the
/// two differ by a factor of four in aspect ratio. A demo that reflows on
/// width alone passes at 80x24 and then leaves half the screen blank on a
/// phone held upright, which is exactly the failure this batch of demos exists
/// to avoid.
///
/// Deliberately scoped to demos 27 and up. The earlier interface demos lay out
/// responsively too, but they were authored against a landscape target and are
/// not held to the portrait requirement.
mod phone_shapes {
    use super::*;
    use support::extent;

    /// A phone held upright: tall and narrow.
    const PORTRAIT: (u16, u16) = (73, 79);
    /// The same phone on its side: wide and short.
    const LANDSCAPE: (u16, u16) = (158, 36);

    fn assert_fills_shape<D: ascii_tile_demos::Demo>(
        name: &str,
        shape: &str,
        (cols, rows): (u16, u16),
    ) {
        let (max_x, max_y) = extent::<D>(cols, rows, FRAMES);
        // Two cells of slack per axis, the same tolerance `fills_the_grid`
        // uses: a demo may leave a margin, but it may not leave a band.
        assert!(
            max_x + 2 >= cols,
            "{name} drew no further right than column {max_x} of {cols} in {shape}"
        );
        assert!(
            max_y + 2 >= rows,
            "{name} drew no further down than row {max_y} of {rows} in {shape}"
        );
    }

    macro_rules! phone {
        ($($test:ident => $module:ident :: $demo:ident),* $(,)?) => {
            $(
                mod $test {
                    use super::{LANDSCAPE, PORTRAIT, assert_fills_shape, $module};

                    #[test]
                    fn lays_out_on_a_portrait_phone() {
                        assert_fills_shape::<$module::$demo>(
                            stringify!($demo),
                            "portrait",
                            PORTRAIT,
                        );
                    }

                    #[test]
                    fn lays_out_on_a_landscape_phone() {
                        assert_fills_shape::<$module::$demo>(
                            stringify!($demo),
                            "landscape",
                            LANDSCAPE,
                        );
                    }
                }
            )*
        };
    }

    phone! {
        rhythm_crypt => d27::RhythmCrypt,
        spire_deck => d28::SpireDeck,
        ship_breach => d29::ShipBreach,
        fleet_command => d30::FleetCommand,
        dice_tactics => d31::DiceTactics,
        loop_track => d32::LoopTrack,
        onebit_quest => d33::OnebitQuest,
        ice_breach => d34::IceBreach,
        stealth_grid => d35::StealthGrid,
        court_reigns => d36::CourtReigns,
    }
}
