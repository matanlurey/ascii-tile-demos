//! Shared harness for the `ascii-tile-demos` gallery: the [`Demo`] trait,
//! `launch::<D>()` backend dispatch, and the [`wasm_entry!`]/[`demo_main!`]
//! FFI-codegen macros.
//!
//! Every demo in `examples/` is a single `.rs` file implementing [`Demo`] and
//! ending in `ascii_tile_demos::demo_main!(MyDemo);`. That one line is enough
//! to run the same code on a real terminal, a native window (software or GL),
//! a browser canvas (`Canvas2D` or `WebGL2`), and a headless in-memory grid.
//!
//! ```ignore
//! #[derive(Default)]
//! struct MyDemo { /* state */ }
//!
//! impl ascii_tile_demos::Demo for MyDemo {
//!     const NAME: &'static str = "my_demo";
//!     const TITLE: &'static str = "My Demo";
//!     const BLURB: &'static str = "One sentence about the technique.";
//!
//!     fn tick<B: retroglyph_core::Backend>(
//!         &mut self,
//!         term: &mut retroglyph_core::Terminal<B>,
//!         frame: &retroglyph_core::Frame,
//!     ) -> bool { todo!() }
//! }
//!
//! ascii_tile_demos::demo_main!(MyDemo);
//! ```

// Demo-support code, not a published API. These lints exist to keep library
// crates honest; a gallery of throwaway harness helpers doesn't benefit.
#![allow(
    missing_docs,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    rustdoc::broken_intra_doc_links,
    rustdoc::private_intra_doc_links
)]
#![forbid(unsafe_code)]

mod launch;
pub mod ui;
pub mod util;
mod wasm_entry;

pub use launch::{
    Demo, HEADLESS_COLS, HEADLESS_FRAME_DELTA, HEADLESS_ROWS, META_ENV, launch,
    print_meta_if_requested, render_headless_frames, run_headless_stdout,
};

#[cfg(feature = "crossterm")]
pub use launch::run_crossterm;
#[cfg(feature = "gl")]
pub use launch::run_gl;
#[cfg(feature = "software")]
pub use launch::{run_software, run_software_with};

// Both windowed backends register it, so it is exported whenever either is on
// rather than riding along with the software re-export.
#[cfg(any(feature = "software", feature = "gl"))]
pub use launch::block_tileset;

/// The grid every demo is authored against: wide enough for a strategy map
/// plus a sidebar, short enough to fit a laptop terminal without scrolling.
///
/// Windowed backends open at exactly this size; the terminal backend uses
/// whatever the real terminal is, and every demo lays out responsively from
/// [`Terminal::size`](retroglyph_core::Terminal::size) rather than assuming
/// these numbers. They are the *design* target, not a guarantee.
pub const GRID_COLS: u16 = 100;
/// See [`GRID_COLS`].
pub const GRID_ROWS: u16 = 40;
