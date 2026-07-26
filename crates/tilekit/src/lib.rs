//! Shared building blocks for the `ascii-tile-demos` gallery: everything the
//! demos need that isn't *about* the demo itself.
//!
//! Nothing here is `retroglyph`-specific except [`palette`] (which speaks
//! [`retroglyph_core::Color`]) and the drawing helpers in [`glyphs`]. The
//! geometry, noise, world generation, autotiling, and field-of-view modules
//! are plain math over integer grids, so they're unit-testable without a
//! backend and reusable outside this repo.
//!
//! | Module | What it covers |
//! | --- | --- |
//! | [`noise`] | Value noise, fBm, domain warping, deterministic hashing |
//! | [`world`] | Heightmap → moisture/temperature → Whittaker biomes, rivers, roads, settlements, provinces |
//! | [`geom`] | Square / isometric / hex projections, screen↔world transforms, picking |
//! | [`autotile`] | 4-bit and 8-bit bitmasks, the 47-tile blob set, dual-grid corner tiling, marching squares |
//! | [`palette`] | Biome palettes, parchment, day/night and seasonal tints, hillshading, color ramps |
//! | [`glyphs`] | Box drawing, quadrant/sextant/octant blocks, braille, shade ramps, sub-cell canvases |
//! | [`fov`] | Recursive shadowcasting on squares, and its hex analogue |
//! | [`camera`] | Pan/zoom viewport with fractional zoom, on top of `retroglyph`'s own `Camera` |

#![forbid(unsafe_code)]

pub mod autotile;
pub mod camera;
pub mod fov;
pub mod geom;
pub mod glyphs;
pub mod noise;
pub mod palette;
pub mod world;
