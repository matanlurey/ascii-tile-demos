//! 04: Autotile gallery -- four schemes, one coastline, side by side.
//!
//! The same land/water mask, drawn through all four autotiling approaches in
//! [`tilekit::autotile`] at once, arranged as a 2x2 grid of labelled panels so
//! the differences are visible in the same glance rather than remembered
//! across separate demos:
//!
//! 1. **4-bit cardinal** ([`mask4`] + [`box_glyph`]): only the four cardinal
//!    neighbours matter, so every inside corner looks identical to a straight
//!    edge -- cheap, and visibly wrong at corners.
//! 2. **8-bit / 47-blob** ([`mask8`] + [`blob_index`]): all eight neighbours
//!    matter, collapsed to 47 distinct visual cases by discarding diagonals
//!    that can't be seen. Rendered here as a shade-ramp intensity keyed to the
//!    blob index, with [`inside_corners`] marked separately in a different
//!    color, since a real tileset would draw genuinely different art for that
//!    case and a shade ramp alone can't show it.
//! 3. **Dual grid** ([`DualGrid::quadrant_glyph`]): corner sampling, smooth by
//!    construction (see `03_dual_grid.rs` for this one on its own).
//! 4. **Marching squares** ([`marching_case`] + [`marching_glyph`]): the same
//!    16 corner configurations as dual grid, but drawing *only the boundary
//!    line* rather than a filled tile, which is the right tool for tracing a
//!    coastline or a contour rather than for filling terrain.
//!
//! Hovering (or, headless, the tracked cursor) reports which mask value each
//! of the four schemes resolves the same cell to -- seeing the same
//! coastline point produce a 4-bit mask of `0b0110`, a blob index of 23, a
//! dual-grid quadrant of `0b1001`, and a marching-squares case of `9` is the
//! actual pedagogical point of putting them next to each other.
//!
//! ```sh
//! cargo run --example 04_autotile_gallery --features crossterm
//! cargo run --example 04_autotile_gallery --features software
//! cargo run --example 04_autotile_gallery --features gl
//! cargo run --example 04_autotile_gallery  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, PrintStr};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{
    self, DualGrid, blob_index, box_glyph, inside_corners, marching_case, marching_glyph, mask4,
    mask8,
};
use tilekit::glyphs::{SHADE, ramp_glyph};
use tilekit::palette;
use tilekit::world::World;

/// World size in cells. Small: every panel resamples the same patch every
/// frame, and a coastline this size already exercises every mask case.
const WORLD_W: i32 = 160;
/// See [`WORLD_W`].
const WORLD_H: i32 = 100;

/// How many world cells the shared viewport pans per second while animating,
/// slow enough that all four panels are clearly showing the same drifting
/// patch rather than flickering independently.
const PAN_SPEED: f32 = 2.2;

/// One of the four panels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Cardinal4,
    Blob47,
    DualGrid,
    Marching,
}

impl Panel {
    const ALL: [Self; 4] = [
        Self::Cardinal4,
        Self::Blob47,
        Self::DualGrid,
        Self::Marching,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Cardinal4 => "4-BIT CARDINAL (16 cases)",
            Self::Blob47 => "8-BIT BLOB (47 cases)",
            Self::DualGrid => "DUAL GRID (16 corner cases)",
            Self::Marching => "MARCHING SQUARES (contour)",
        }
    }
}

/// State: the world, a shared drifting viewport, and whether the drift runs.
pub struct AutotileGallery {
    world: World,
    time: f32,
    origin_x: f32,
    origin_y: i32,
    drifting: bool,
    /// Cursor cell, relative to each panel's own top-left, for the mask
    /// readout. Shared across panels since they all show the same patch.
    cursor: (i32, i32),
    fps: FpsMeter,
}

impl Default for AutotileGallery {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 5);
        let (ox, oy) = starting_coastline(&world);
        Self {
            world,
            time: 0.0,
            origin_x: ox as f32,
            origin_y: oy,
            drifting: true,
            cursor: (10, 5),
            fps: FpsMeter::new(),
        }
    }
}

/// Picks a viewport origin that actually straddles a coastline, rather than
/// [`World::start_position`]'s capital (which sits well inland and would open
/// this demo on four panels of uniform "all land").
///
/// Scans outward from the map center along one row for the first land/water
/// transition, which for the island-shaped worlds this crate generates is
/// reliably near the coast ringing the continent.
fn starting_coastline(world: &World) -> (i32, i32) {
    let y = world.height() / 2;
    let is_land = |x: i32| world.elevation_at(x, y) > tilekit::world::SEA_LEVEL;
    for x in 1..world.width() {
        if is_land(x) != is_land(x - 1) {
            return ((x - 20).max(0), (y - 10).max(0));
        }
    }
    // No transition on the center row (a landlocked seed): fall back to the
    // capital, which is at least guaranteed to be on the map.
    let (sx, sy) = world.start_position();
    ((sx - 20).max(0), (sy - 10).max(0))
}

impl AutotileGallery {
    fn is_land(&self, x: i32, y: i32) -> bool {
        self.world.in_bounds(x, y) && self.world.elevation_at(x, y) > tilekit::world::SEA_LEVEL
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        let (ox, oy) = starting_coastline(&self.world);
        self.origin_x = ox as f32;
        self.origin_y = oy;
    }

    fn pan(&mut self, dx: i32, dy: i32) {
        self.origin_x += dx as f32;
        self.origin_y = (self.origin_y + dy).max(0);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.pan(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.pan(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.pan(-2, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.pan(2, 0),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    KeyCode::Char('p' | 'P') => self.drifting = !self.drifting,
                    _ => {}
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                        // Only meaningful within a panel's own local
                        // coordinates; a rough top-left-relative estimate is
                        // enough for a readout that is illustrative rather
                        // than pixel-exact.
                        self.cursor = (
                            i32::from(mouse.position.x) % 40,
                            i32::from(mouse.position.y) % 12,
                        );
                    }
                    MouseEventKind::ScrollUp => self.pan(0, -1),
                    MouseEventKind::ScrollDown => self.pan(0, 1),
                    _ => {}
                },
                _ => {}
            }
        }
        true
    }

    /// The 8 neighbours of world cell `(x, y)`, in [`mask8`]'s
    /// `[NW, N, NE, W, E, SW, S, SE]` order.
    fn neighbors8(&self, x: i32, y: i32) -> [bool; 8] {
        [
            self.is_land(x - 1, y - 1),
            self.is_land(x, y - 1),
            self.is_land(x + 1, y - 1),
            self.is_land(x - 1, y),
            self.is_land(x + 1, y),
            self.is_land(x - 1, y + 1),
            self.is_land(x, y + 1),
            self.is_land(x + 1, y + 1),
        ]
    }

    /// Draws one panel: `sample(local_x, local_y) -> world (x, y)`, filling
    /// `area` one glyph per screen cell via `render`.
    fn draw_panel<B, R>(term: &mut Terminal<B>, area: Rect, render: R)
    where
        B: Backend,
        R: Fn(i32, i32) -> (char, retroglyph_core::Color, retroglyph_core::Color),
    {
        let inner = Rect::new(
            area.left(),
            area.top() + 1,
            area.width(),
            area.height().saturating_sub(1),
        );
        for row in 0..i32::from(inner.height()) {
            for col in 0..i32::from(inner.width()) {
                let (glyph, fg, bg) = render(col, row);
                term.put_styled(
                    inner.left() + col as u16,
                    inner.top() + row as u16,
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
    }

    fn draw_cardinal4<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let ox = self.origin_x as i32;
        Self::draw_panel(term, area, |col, row| {
            let (x, y) = (ox + col, self.origin_y + row);
            let land = self.is_land(x, y);
            if !land {
                return (' ', palette::BLACK, palette::rgb(20, 40, 70));
            }
            let mask = mask4([
                self.is_land(x, y - 1),
                self.is_land(x + 1, y),
                self.is_land(x, y + 1),
                self.is_land(x - 1, y),
            ]);
            (
                box_glyph(mask),
                palette::rgb(214, 196, 156),
                palette::rgb(90, 78, 54),
            )
        });
    }

    fn draw_blob47<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let ox = self.origin_x as i32;
        let total = autotile::blob_count().max(1) as f32;
        Self::draw_panel(term, area, |col, row| {
            let (x, y) = (ox + col, self.origin_y + row);
            let land = self.is_land(x, y);
            if !land {
                return (' ', palette::BLACK, palette::rgb(20, 40, 70));
            }
            let mask = mask8(self.neighbors8(x, y));
            let index = blob_index(mask);
            let corners = inside_corners(mask);
            if corners.iter().any(|&c| c) {
                // Inside corners are the case 4-bit autotiling cannot
                // express at all; a real tileset would draw distinct art
                // here, so this marks them with their own accent color
                // rather than folding them into the shade ramp.
                return (
                    '\u{2022}',
                    palette::rgb(255, 210, 120),
                    palette::rgb(120, 90, 40),
                );
            }
            let t = index as f32 / total;
            (
                ramp_glyph(&SHADE, t),
                palette::rgb(214, 196, 156),
                palette::rgb(90, 78, 54),
            )
        });
    }

    fn draw_dual_grid<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let ox = self.origin_x as i32;
        let (offset_x, offset_y) = DualGrid::display_origin(2, 2);
        Self::draw_panel(term, area, |col, row| {
            let (dx, dy) = (ox + col + offset_x, self.origin_y + row + offset_y);
            let samples = DualGrid::samples(dx + 1, dy + 1);
            let corners = samples.map(|(sx, sy)| self.is_land(sx, sy));
            let glyph = DualGrid::quadrant_glyph(DualGrid::corner_mask(corners));
            let land_corners = corners.iter().filter(|&&c| c).count();
            if land_corners >= 2 {
                (glyph, palette::rgb(214, 196, 156), palette::rgb(20, 40, 70))
            } else {
                (glyph, palette::rgb(20, 40, 70), palette::rgb(214, 196, 156))
            }
        });
    }

    fn draw_marching<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        let ox = self.origin_x as i32;
        let (offset_x, offset_y) = DualGrid::display_origin(2, 2);
        Self::draw_panel(term, area, |col, row| {
            let (dx, dy) = (ox + col + offset_x, self.origin_y + row + offset_y);
            let samples = DualGrid::samples(dx + 1, dy + 1);
            let corners = samples.map(|(sx, sy)| self.is_land(sx, sy));
            let case = marching_case(corners);
            let bg = if corners[3] {
                palette::rgb(60, 52, 38)
            } else {
                palette::rgb(14, 28, 48)
            };
            if autotile::is_boundary(case) {
                (marching_glyph(case), palette::rgb(255, 236, 170), bg)
            } else {
                (' ', palette::BLACK, bg)
            }
        });
    }

    fn draw<B: Backend>(&self, term: &mut Terminal<B>, area: Rect) {
        if area.width() < 8 || area.height() < 4 {
            return;
        }
        let half_w = area.width() / 2;
        let half_h = area.height() / 2;
        let quads = [
            Rect::new(area.left(), area.top(), half_w, half_h),
            Rect::new(
                area.left() + half_w,
                area.top(),
                area.width() - half_w,
                half_h,
            ),
            Rect::new(
                area.left(),
                area.top() + half_h,
                half_w,
                area.height() - half_h,
            ),
            Rect::new(
                area.left() + half_w,
                area.top() + half_h,
                area.width() - half_w,
                area.height() - half_h,
            ),
        ];

        for (panel, rect) in Panel::ALL.into_iter().zip(quads) {
            term.print_styled_str(
                rect.left(),
                rect.top(),
                panel.label(),
                Style::new().fg(ui::ACCENT).bg(ui::CHROME_BG),
            );
            match panel {
                Panel::Cardinal4 => self.draw_cardinal4(term, rect),
                Panel::Blob47 => self.draw_blob47(term, rect),
                Panel::DualGrid => self.draw_dual_grid(term, rect),
                Panel::Marching => self.draw_marching(term, rect),
            }
        }
    }

    /// What each scheme resolves the tracked cell to, for the readout.
    fn mask_readout(&self) -> String {
        let (cx, cy) = self.cursor;
        let (x, y) = (self.origin_x as i32 + cx, self.origin_y + cy);
        let m4 = mask4([
            self.is_land(x, y - 1),
            self.is_land(x + 1, y),
            self.is_land(x, y + 1),
            self.is_land(x - 1, y),
        ]);
        let m8 = mask8(self.neighbors8(x, y));
        let (offset_x, offset_y) = DualGrid::display_origin(2, 2);
        let (dx, dy) = (x + offset_x, y + offset_y);
        let corners = DualGrid::samples(dx + 1, dy + 1).map(|(sx, sy)| self.is_land(sx, sy));
        let dual_mask = DualGrid::corner_mask(corners);
        let march = marching_case(corners);
        format!(
            "cell ({x},{y}): 4-bit={m4:#06b}  blob=#{}  dual={dual_mask:#06b}  march={march}",
            blob_index(m8)
        )
    }

    fn status(&self) -> String {
        format!(
            "{}  drift {}  seed {}",
            self.mask_readout(),
            if self.drifting { "on" } else { "off" },
            self.world.seed()
        )
    }
}

impl Demo for AutotileGallery {
    const NAME: &'static str = "04_autotile_gallery";
    const TITLE: &'static str = "04 Autotile gallery";
    const BLURB: &'static str =
        "Four autotiling schemes on the same coastline, compared side by side.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("P", "pause drift"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        if self.drifting {
            self.origin_x = frame.delta.as_secs_f32().mul_add(PAN_SPEED, self.origin_x);
        }

        let (title, content, status) = ui::split_chrome(term.area());
        ui::fill(term, content, Style::new().bg(ui::BG));
        self.draw(term, content);
        ui::title_bar::<B, Self>(term, title);
        let text = self.status();
        ui::status_bar::<B, Self>(term, status, &text, &self.fps);

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(AutotileGallery);
