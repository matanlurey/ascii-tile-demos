//! 03: Dual grid -- smooth coastlines from 16 tiles instead of 47.
//!
//! Split the screen in half and render the same land/water mask two ways.
//! **Left**: naive per-cell rendering, one glyph per world cell, land or
//! water with nothing in between -- the coastline is a staircase of hard
//! blocky corners. **Right**: the dual-grid trick
//! ([`tilekit::autotile::DualGrid`]): a display tile is drawn at every
//! *corner* of the world grid, sampling the four world cells that meet
//! there, so the land/water boundary always falls through the middle of a
//! display tile where a quadrant glyph can angle it, rather than along a
//! tile edge where it has no choice but to be straight.
//!
//! This is the same 16-configuration corner mask
//! [`marching_case`](tilekit::autotile::marching_case) uses to trace a
//! contour, applied instead to decide *what a tile looks like* rather than
//! *where a line crosses it*. Compare against `04_autotile_gallery.rs`, which
//! puts this side by side with 4-bit, 47-blob, and marching-squares on the
//! same patch of coastline.
//!
//! Techniques on show:
//!
//! - **Dual-grid corner sampling** ([`DualGrid::samples`],
//!   [`DualGrid::display_origin`]): the display grid is offset half a tile up
//!   and left from the world grid, which is the detail that is easy to get
//!   wrong -- forgetting the offset draws the right *shape* one tile in the
//!   wrong *place*.
//! - **Quadrant glyphs** ([`DualGrid::quadrant_glyph`]): each of the 16 corner
//!   configurations gets a distinct Unicode block-element glyph, so a
//!   diagonal coastline actually reads as diagonal instead of a uniform gray
//!   density.
//! - **A live, animated sea level**: raising and lowering
//!   [`tilekit::world::SEA_LEVEL`] by a slow sine sweep advances and retreats
//!   the coastline every frame, which is the clearest way to see the
//!   autotiling actually *react* rather than judging it from one static frame.
//!
//! See Jess Hammer's [dual-grid tilemap system](https://github.com/jess-hammer/dual-grid-tilemap-system-godot)
//! and Oskar Stalberg's original talk on building organic towns from square
//! tiles, and Boris the Brave on
//! [2-corner Wang tiles](http://www.boristhebrave.com/permanent/24/06/cr31/stagecast/wang/2corn.html)
//! for the underlying corner-matching idea.
//!
//! ```sh
//! cargo run --example 03_dual_grid --features crossterm
//! cargo run --example 03_dual_grid --features software
//! cargo run --example 03_dual_grid --features gl
//! cargo run --example 03_dual_grid  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::DualGrid;
use tilekit::geom::Cell;
use tilekit::palette::{self, mix};
use tilekit::world::{SEA_LEVEL, World};

/// World size in cells. Kept modest: this demo redraws the whole visible
/// patch of world twice (once per half) every frame the sea level is
/// animating, and doesn't need `01_terrain_cells`' scale to make its point.
const WORLD_W: i32 = 160;
/// See [`WORLD_W`].
const WORLD_H: i32 = 100;

/// Amplitude of the sea-level oscillation, in the same units as
/// [`SEA_LEVEL`]. Large enough to visibly redraw whole stretches of coast,
/// small enough that the map doesn't flip from mostly land to mostly ocean.
const TIDE_AMPLITUDE: f32 = 0.05;
/// How fast the tide cycles, in radians per second.
const TIDE_SPEED: f32 = 0.35;

/// Land colors: dry sand fading into a darker beach.
const LAND: retroglyph_core::Color = palette::rgb(196, 178, 128);
/// Water colors.
const WATER: retroglyph_core::Color = palette::rgb(26, 58, 102);

/// State: the world (only its elevation field is used -- this demo is about
/// autotiling, not biomes), a live sea level, and a shared pan position for
/// both halves.
pub struct DualGridDemo {
    world: World,
    time: f32,
    /// Current effective sea level, oscillating around [`SEA_LEVEL`].
    sea_level: f32,
    /// Top-left world cell shown at the left edge of each half.
    origin: Cell,
    /// Whether the tide animates. `P` pauses it, useful for comparing the two
    /// halves at a fixed coastline rather than a moving target.
    tide_running: bool,
    /// Whether to overlay the underlying world-cell grid as faint dots, so
    /// the dual grid's half-tile offset from it is visible.
    show_grid: bool,
    fps: FpsMeter,
}

impl Default for DualGridDemo {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 3);
        let (sx, sy) = world.start_position();
        Self {
            world,
            time: 0.0,
            sea_level: SEA_LEVEL,
            origin: Cell::new((sx - 20).max(0), (sy - 12).max(0)),
            tide_running: true,
            show_grid: false,
            fps: FpsMeter::new(),
        }
    }
}

impl DualGridDemo {
    /// Whether `(x, y)` is land at the current animated sea level. Off-map
    /// cells count as water, which is what makes the map edge itself read as
    /// a coastline instead of a hard cutoff.
    fn is_land(&self, x: i32, y: i32) -> bool {
        self.world.in_bounds(x, y) && self.world.elevation_at(x, y) > self.sea_level
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (sx, sy) = self.world.start_position();
        self.origin = Cell::new((sx - 20).max(0), (sy - 12).max(0));
    }

    fn pan(&mut self, dx: i32, dy: i32) {
        self.origin = Cell::new((self.origin.x + dx).max(0), (self.origin.y + dy).max(0));
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.pan(0, -2),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.pan(0, 2),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.pan(-2, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.pan(2, 0),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    KeyCode::Char('p' | 'P') => self.tide_running = !self.tide_running,
                    KeyCode::Char('g' | 'G') => self.show_grid = !self.show_grid,
                    _ => {}
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Drag(MouseButton::Left) => self.pan(0, 0),
                    MouseEventKind::Scroll { dy, .. } if dy > 0.0 => self.pan(0, -2),
                    MouseEventKind::Scroll { dy, .. } if dy < 0.0 => self.pan(0, 2),
                    _ => {}
                },
                _ => {}
            }
        }
        true
    }

    /// Left half: one glyph per world cell, land or water, no in-between.
    fn draw_naive(&self, surface: &mut Surface<'_>, area: Rect) {
        for row in 0..i32::from(area.height()) {
            for col in 0..i32::from(area.width()) {
                let (wx, wy) = (self.origin.x + col, self.origin.y + row);
                let land = self.is_land(wx, wy);
                let color = if land { LAND } else { WATER };
                let glyph = if land { '#' } else { '~' };
                surface.put(
                    (area.left() + col as u16, area.top() + row as u16),
                    glyph,
                    Style::new().fg(mix(color, palette::WHITE, 0.25)).bg(color),
                );
            }
        }
    }

    /// Right half: dual-grid quadrant glyphs. Every screen cell is a display
    /// tile whose four corner samples are world cells offset by
    /// [`DualGrid::display_origin`] from the naive grid, so the same world
    /// region produces a display grid one cell larger on every side.
    fn draw_dual_grid(&self, surface: &mut Surface<'_>, area: Rect) {
        let (offset_x, offset_y) = DualGrid::display_origin(2, 2);
        for row in 0..i32::from(area.height()) {
            for col in 0..i32::from(area.width()) {
                // The display tile at this screen position samples the world
                // cell block whose corner sits here, shifted by the dual
                // grid's own half-tile offset so it lines up with the same
                // world region the naive view on the left is showing.
                let (dx, dy) = (
                    self.origin.x + col + offset_x,
                    self.origin.y + row + offset_y,
                );
                let samples = DualGrid::samples(dx + 1, dy + 1);
                let corners = samples.map(|(sx, sy)| self.is_land(sx, sy));
                let mask = DualGrid::corner_mask(corners);
                let glyph = DualGrid::quadrant_glyph(mask);

                // Foreground carries whichever terrain is more present in the
                // block (land if 2+ corners are land), background the other,
                // which is what makes a half-and-half quadrant glyph read as
                // exactly half land rather than as one indistinct blend color.
                let land_corners = corners.iter().filter(|&&c| c).count();
                let (fg, bg) = if land_corners >= 2 {
                    (LAND, WATER)
                } else {
                    (WATER, LAND)
                };

                // Marking the bottom-right sample (the world cell this
                // display tile's own coordinate would naively land on) shows
                // the half-tile stagger against the left view: the marked
                // corner sits offset by half a cell from where the naive
                // view drew that same world cell.
                let style = if self.show_grid && corners[3] {
                    Style::new().fg(mix(fg, palette::BLACK, 0.4)).bg(bg)
                } else {
                    Style::new().fg(fg).bg(bg)
                };
                surface.put(
                    (area.left() + col as u16, area.top() + row as u16),
                    glyph,
                    style,
                );
            }
        }

        if self.show_grid {
            // Faint dots at the underlying world-cell lattice, so the
            // half-tile offset between this grid and the naive one is visible
            // rather than asserted in prose.
            for row in 0..i32::from(area.height()) {
                for col in 0..i32::from(area.width()) {
                    if (col + row) % 2 == 0 {
                        continue;
                    }
                    surface.put(
                        (area.left() + col as u16, area.top() + row as u16),
                        '\u{00b7}',
                        Style::new()
                            .fg(palette::rgb(255, 255, 255))
                            .bg(palette::BLACK),
                    );
                }
            }
        }
    }

    fn draw(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 4 || area.height() == 0 {
            return;
        }
        let half = area.width() / 2;
        let left = Rect::new(area.left(), area.top(), half, area.height());
        let right = Rect::new(
            area.left() + half,
            area.top(),
            area.width() - half,
            area.height(),
        );

        self.draw_naive(surface, left);
        self.draw_dual_grid(surface, right);

        // A one-cell divider plus labels, drawn last so they sit on top of
        // both halves.
        for y in area.top()..area.bottom() {
            surface.put(
                (right.left(), y),
                '\u{2502}',
                Style::new().fg(ui::DIM).bg(ui::BG),
            );
        }
        surface.print(
            (left.left() + 1, area.top()),
            "NAIVE (per-cell)",
            Style::new().fg(ui::FG).bg(mix(LAND, ui::BG, 0.5)),
        );
        surface.print(
            (right.left() + 2, area.top()),
            "DUAL GRID (quadrant glyphs)",
            Style::new().fg(ui::FG).bg(mix(LAND, ui::BG, 0.5)),
        );
    }

    fn status(&self) -> String {
        format!(
            "sea level {:.3}  tide {}  grid {}  seed {}",
            self.sea_level,
            if self.tide_running {
                "running"
            } else {
                "paused"
            },
            if self.show_grid { "on" } else { "off" },
            self.world.seed()
        )
    }
}

impl Demo for DualGridDemo {
    const NAME: &'static str = "03_dual_grid";
    const TITLE: &'static str = "03 Dual grid";
    const BLURB: &'static str =
        "Corner-sampled dual grid vs. naive per-cell: smooth coasts from 16 tiles.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("P", "pause tide"),
            ("G", "show grid"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        if self.tide_running {
            self.sea_level = (self.time * TIDE_SPEED)
                .sin()
                .mul_add(TIDE_AMPLITUDE, SEA_LEVEL);
        }

        let (title, content, status) = ui::split_chrome(term.area());

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(DualGridDemo);
