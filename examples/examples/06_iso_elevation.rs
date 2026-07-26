//! 06: Isometric elevation -- staggered '2.5D' stacking with cliff faces.
//!
//! Builds on [`05_iso_diamond`](../05_iso_diamond) by quantizing elevation
//! into discrete levels and raising each tile's diamond by
//! [`IsoLayout::tile_to_cell_elevated`]. Raising a tile is the entire height
//! illusion: nothing about the tile geometry changes except which screen row
//! it lands on, and the existing painter's-algorithm draw order does the rest
//! -- a tile in front naturally overlaps the base of a taller tile behind it.
//! Where a tall tile's *front* neighbour is lower, the gap between the raised
//! diamond and the ground has to be filled in, or the terrain looks like it is
//! floating; that fill is the cliff face.
//!
//! Techniques on show:
//!
//! - **Elevation stacking** ([`tilekit::geom::IsoLayout::tile_to_cell_elevated`]):
//!   discretizing a continuous heightmap into a handful of levels and drawing
//!   each tile `level * per_level` cells higher. See Erik Onarheim on
//!   [handling height in isometric tile
//!   maps](https://erikonarheim.com/posts/handling-height-in-isometric/).
//! - **Cliff faces**: a vertical skirt of cells below a raised tile's diamond,
//!   drawn only on edges facing a lower neighbour, using block glyphs so rock
//!   reads as a solid face rather than a gap in the terrain.
//! - **Depth sort excludes elevation** ([`IsoLayout::depth`]): the draw order
//!   is `col + row` alone. Mixing elevation into the sort key is the classic
//!   way to make a distant mountain incorrectly occlude something nearer the
//!   camera; a comment at the sort call explains why it is not done here.
//! - **Vertical exaggeration**: a live-adjustable `per_level` scale, the same
//!   knob every relief map has, showing how much the elevation illusion
//!   depends on it.
//!
//! ```sh
//! cargo run --example 06_iso_elevation --features crossterm
//! cargo run --example 06_iso_elevation --features software
//! cargo run --example 06_iso_elevation --features gl
//! cargo run --example 06_iso_elevation  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, PrintStr};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, IsoLayout, Tile};
use tilekit::noise::fbm;
use tilekit::palette::{self, mix, scale};
use tilekit::world::{Biome, World};

/// World size in tiles. Isometric elevation views cost more screen space per
/// tile than the flat diamond demo (every level of height adds drawn rows), so
/// this stays on the small side.
const WORLD_W: i32 = 56;
/// See [`WORLD_W`].
const WORLD_H: i32 = 56;

/// Number of discrete elevation levels above sea level. Six is enough to show
/// a coastline, foothills, and a couple of mountain tiers without the terrain
/// turning into an unreadable staircase of one-cell steps.
const LEVELS: i32 = 6;

/// Default cells of screen height per elevation level.
const DEFAULT_PER_LEVEL: i32 = 2;

/// The tile layout. `LARGE` (16x4 per diamond) rather than `STANDARD`: cliff
/// skirts need vertical room to read as more than a one-cell smudge.
const LAYOUT: IsoLayout = IsoLayout::LARGE;

/// State: the world, its quantized elevation levels, and view controls.
pub struct IsoElevation {
    world: World,
    /// Elevation level per tile, `0..LEVELS`, precomputed once at generation
    /// time rather than requantized every frame.
    levels: Vec<i32>,
    center_tile: Tile,
    per_level: i32,
    time: f32,
    fps: FpsMeter,
    cursor_tile: Tile,
    wireframe: bool,
    drag_from: Option<Cell>,
}

/// Quantizes every tile's elevation into `0..LEVELS`, with sea level pinned to
/// level 0 so water never grows a cliff skirt.
fn quantize_levels(world: &World) -> Vec<i32> {
    let mut levels = Vec::with_capacity((world.width() * world.height()) as usize);
    for y in 0..world.height() {
        for x in 0..world.width() {
            let biome = world.biome_at(x, y);
            let level = if biome.is_water() {
                0
            } else {
                let e = world.elevation_at(x, y);
                // Remap [SEA_LEVEL, 1.0] to [1, LEVELS - 1]: level 0 is
                // reserved for water, so land always visibly steps up from it
                // by at least one level rather than starting flush with sea.
                let above = ((e - tilekit::world::SEA_LEVEL) / (1.0 - tilekit::world::SEA_LEVEL))
                    .clamp(0.0, 1.0);
                1 + (above * (LEVELS - 2) as f32).round() as i32
            };
            levels.push(level.clamp(0, LEVELS - 1));
        }
    }
    levels
}

impl Default for IsoElevation {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 5);
        let levels = quantize_levels(&world);
        let center_tile = Tile::new(WORLD_W / 2, WORLD_H / 2);
        Self {
            world,
            levels,
            center_tile,
            per_level: DEFAULT_PER_LEVEL,
            time: 0.0,
            fps: FpsMeter::new(),
            cursor_tile: center_tile,
            wireframe: false,
            drag_from: None,
        }
    }
}

impl IsoElevation {
    fn level_at(&self, tile: Tile) -> i32 {
        if !self.world.in_bounds(tile.col, tile.row) {
            return 0;
        }
        self.levels[(tile.row * self.world.width() + tile.col) as usize]
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        self.levels = quantize_levels(&self.world);
    }

    const fn pan_cells(&mut self, dx: i32, dy: i32) {
        let current = LAYOUT.tile_to_cell(self.center_tile);
        self.center_tile = LAYOUT.cell_to_tile(Cell::new(current.x + dx, current.y + dy));
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>, content: Rect) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => {
                    let step = if key.modifiers.contains(retroglyph_core::KeyModifiers::SHIFT) {
                        8
                    } else {
                        2
                    };
                    match key.code {
                        KeyCode::Up | KeyCode::Char('w' | 'W') => self.pan_cells(0, -step),
                        KeyCode::Down | KeyCode::Char('s' | 'S') => self.pan_cells(0, step),
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.pan_cells(-step, 0),
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.pan_cells(step, 0),
                        KeyCode::Char('+' | '=') => self.per_level = (self.per_level + 1).min(6),
                        KeyCode::Char('-' | '_') => self.per_level = (self.per_level - 1).max(0),
                        KeyCode::Char('o' | 'O') => self.wireframe = !self.wireframe,
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Home => self.center_tile = Tile::new(WORLD_W / 2, WORLD_H / 2),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position, content),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: retroglyph_core::Pos, content: Rect) {
        let screen = Cell::new(
            i32::from(pos.x) - i32::from(content.left()),
            i32::from(pos.y) - i32::from(content.top()),
        );
        match kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                self.cursor_tile = self.pick(screen, content);
                if matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.drag_from = Some(screen);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.drag_from {
                    self.pan_cells(-(screen.x - from.x), -(screen.y - from.y));
                    self.drag_from = Some(screen);
                }
                self.cursor_tile = self.pick(screen, content);
            }
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::ScrollUp => self.pan_cells(0, -3),
            MouseEventKind::ScrollDown => self.pan_cells(0, 3),
            _ => {}
        }
    }

    /// Picks a tile under a screen cell, accounting for the elevation raise:
    /// probes a small range of levels near the ground pick and prefers the
    /// tallest tile whose *raised* diamond actually covers the click, which is
    /// what makes clicking on a mountain's visible face (rather than the flat
    /// ground behind it) select the mountain.
    fn pick(&self, screen: Cell, content: Rect) -> Tile {
        let base_cell = self.screen_to_world_cell(screen, content);
        let ground_tile = LAYOUT.cell_to_tile(base_cell);
        let mut best = ground_tile;
        for dc in -2..=2 {
            for dr in -2..=2 {
                let candidate = Tile::new(ground_tile.col + dc, ground_tile.row + dr);
                if !self.world.in_bounds(candidate.col, candidate.row) {
                    continue;
                }
                let level = self.level_at(candidate);
                let raised = LAYOUT.tile_to_cell_elevated(candidate, level, self.per_level);
                let (dx, dy) = (base_cell.x - raised.x, base_cell.y - raised.y);
                if LAYOUT.contains(dx, dy) {
                    best = candidate;
                }
            }
        }
        best
    }

    fn screen_to_world_cell(&self, screen: Cell, content: Rect) -> Cell {
        let center = LAYOUT.tile_to_cell(self.center_tile);
        let half_w = i32::from(content.width()) / 2;
        let half_h = i32::from(content.height()) / 2;
        Cell::new(screen.x - half_w + center.x, screen.y - half_h + center.y)
    }

    fn put_clipped<B: Backend>(
        term: &mut Terminal<B>,
        content: Rect,
        x: i32,
        y: i32,
        glyph: char,
        style: Style,
    ) {
        if x < 0 || y < 0 {
            return;
        }
        let (sx, sy) = (i32::from(content.left()) + x, i32::from(content.top()) + y);
        if sx >= i32::from(content.right()) || sy >= i32::from(content.bottom()) {
            return;
        }
        term.put_styled(sx as u16, sy as u16, glyph, style);
    }

    /// A soft cloud shadow: a wide, slowly-drifting noise blob that dims
    /// whatever terrain it passes over, independent of elevation or biome.
    fn cloud_shade(&self, tile: Tile) -> f32 {
        let scale = 0.09;
        let drift_x = self.time * 3.2;
        let n = fbm(
            0xC10D_5EED,
            (tile.col as f32).mul_add(scale, -drift_x * scale),
            tile.row as f32 * scale,
            3,
            0.5,
        );
        // Threshold so most of the map is unshaded and only a soft-edged
        // patch darkens, rather than a uniform dimming wash over everything.
        ((n - 0.58) / 0.22).clamp(0.0, 1.0)
    }

    fn tile_face(&self, tile: Tile, level: i32, dx: i32, dy: i32) -> retroglyph_core::Color {
        let biome = self.world.biome_at(tile.col, tile.row);
        let mut color = biome.color();
        if biome.is_water() {
            let phase = self
                .time
                .mul_add(1.2, (tile.col as f32).mul_add(0.5, tile.row as f32 * 0.35));
            let swell = phase.sin().mul_add(0.5, 0.5);
            color = mix(color, palette::WHITE, swell * 0.15);
        } else {
            // Higher tiles catch more light: a cheap stand-in for real
            // hillshading that still reads as "this is a peak" at a glance.
            let brighten = f32::from(level as i16).mul_add(0.045, 1.0);
            color = scale(color, brighten);
        }
        let bevel = if dy < 0 {
            1.15
        } else if dy > 0 {
            0.78
        } else if dx < 0 {
            1.05
        } else {
            0.90
        };
        color = scale(color, bevel);

        let shade = self.cloud_shade(tile);
        if shade > 0.0 {
            color = mix(color, palette::BLACK, shade * 0.5);
        }
        color
    }

    /// Draws the vertical rock face below a raised tile, on whichever screen
    /// edges face a lower neighbour. Facing is approximated from `IsoLayout`'s
    /// two screen-diagonal neighbour directions (the tiles whose diamonds
    /// share the drawn tile's left and right lower edges): if either is
    /// shorter, the visible gap between this tile's underside and the ground
    /// has to be filled or the terrain reads as floating.
    fn draw_cliff<B: Backend>(
        &self,
        term: &mut Terminal<B>,
        content: Rect,
        tile: Tile,
        level: i32,
        center: Cell,
    ) {
        if level == 0 {
            return;
        }
        // The two tiles that would otherwise be visible directly below this
        // one's screen footprint: south (col, row+1) and east (col+1, row),
        // the pair whose shared lower corner is this diamond's south tip.
        let south = Tile::new(tile.col, tile.row + 1);
        let east = Tile::new(tile.col + 1, tile.row);
        let south_level = self.level_at(south);
        let east_level = self.level_at(east);
        let drop = level - south_level.min(east_level);
        if drop <= 0 {
            return;
        }

        let raised = LAYOUT.tile_to_cell_elevated(tile, level, self.per_level);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let biome = self.world.biome_at(tile.col, tile.row);
        // Mostly rock, with only a hint of the biome above it: a cliff face is
        // exposed stone, not a vertical extrusion of the meadow on top of it.
        // Weighting this toward the biome instead makes faces under dark
        // terrain (jungle, taiga) so dark they read as holes in the map rather
        // than as geology, which is the thing the skirt exists to prevent.
        let rock = mix(palette::rgb(122, 112, 102), biome.color(), 0.22);
        let rock_dark = scale(rock, 0.72);

        // Fill from the diamond's bottom tip down to where the lower
        // neighbour's own raised surface would be, one screen row per drop
        // level times `per_level`. Only the bottom half-width span of the
        // diamond needs a face, since that is the only part of the silhouette
        // exposed by the drop.
        // The exposed face is the diamond's *lower half* swept straight down
        // by the height of the drop: rows below the tile's own footprint that
        // its lower neighbour no longer covers, because that neighbour is
        // drawn at a lower position.
        //
        // Sweeping the lower half rather than tapering from the south tip is
        // the whole fix for the obvious version of this bug. A skirt anchored
        // at the tip is only a couple of cells wide, so it leaves the rest of
        // the vacated footprint unpainted: a black band along every terrace
        // edge, exactly where the cliff was supposed to be.
        let rows = (drop * self.per_level).max(1);
        for r in 1..=rows {
            for dy in 0..=LAYOUT.half_h {
                let Some(span) = LAYOUT.span_at(dy) else {
                    continue;
                };
                for dx in -span..=span {
                    // A coarse vertical striping so the face reads as rock
                    // rather than as a flat slab of one color. Keyed on
                    // absolute screen position, so it does not crawl when the
                    // map pans.
                    let color = if (sx + dx).rem_euclid(3) == 0 {
                        rock_dark
                    } else {
                        rock
                    };
                    Self::put_clipped(
                        term,
                        content,
                        sx + dx,
                        sy + dy + r,
                        ' ',
                        Style::new().bg(color),
                    );
                }
            }
        }
    }

    fn draw_tile<B: Backend>(
        &self,
        term: &mut Terminal<B>,
        content: Rect,
        tile: Tile,
        center: Cell,
    ) {
        let level = self.level_at(tile);
        self.draw_cliff(term, content, tile, level, center);

        let raised = LAYOUT.tile_to_cell_elevated(tile, level, self.per_level);
        let (sx, sy) = (raised.x - center.x, raised.y - center.y);
        let highlighted = tile == self.cursor_tile;

        for dy in -LAYOUT.half_h..=LAYOUT.half_h {
            let Some(span) = LAYOUT.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                let mut color = self.tile_face(tile, level, dx, dy);
                if highlighted {
                    color = mix(color, palette::rgb(255, 236, 170), 0.35);
                }
                Self::put_clipped(term, content, sx + dx, sy + dy, ' ', Style::new().bg(color));
            }
            if self.wireframe {
                let ink = palette::rgb(18, 18, 24);
                let left = Style::new()
                    .fg(ink)
                    .bg(self.tile_face(tile, level, -span, dy));
                let right = Style::new()
                    .fg(ink)
                    .bg(self.tile_face(tile, level, span, dy));
                Self::put_clipped(term, content, sx - span, sy + dy, '\u{2502}', left);
                Self::put_clipped(term, content, sx + span, sy + dy, '\u{2502}', right);
            }
        }

        // The glyph sits on the tile's own face color. Leaving the background
        // unset is not a no-op on a pixel backend: `Color::Default` resolves
        // to the surface's clear color, so every glyph would punch a black
        // rectangle through the terrace it is standing on.
        let mut face = self.tile_face(tile, level, 0, 0);
        if highlighted {
            face = mix(face, palette::rgb(255, 236, 170), 0.35);
        }
        let biome = self.world.biome_at(tile.col, tile.row);
        if let Some(landmark) = self.world.landmark_at(tile.col, tile.row) {
            let (glyph, color) = landmark.site.glyph_color();
            let style = Style::new().fg(color).bg(face);
            Self::put_clipped(term, content, sx, sy, glyph, style);
        } else if !biome.is_water() {
            let glyph = if biome == Biome::Peak || biome == Biome::Mountain {
                biome.glyph()
            } else if level >= LEVELS - 2 {
                '\u{25b2}'
            } else {
                biome.glyph()
            };
            let style = Style::new().fg(scale(biome.color(), 1.4)).bg(face);
            Self::put_clipped(term, content, sx, sy, glyph, style);
        }
    }

    fn draw<B: Backend>(&self, term: &mut Terminal<B>, content: Rect) {
        let center_cell = LAYOUT.tile_to_cell(self.center_tile);
        let center = Cell::new(
            center_cell.x - i32::from(content.width()) / 2,
            center_cell.y - i32::from(content.height()) / 2,
        );

        // The visible tile range has to grow with the highest possible
        // elevation raise, or a tall tile scrolled just above the viewport
        // top would be culled before its raised diamond ever comes on screen.
        let max_raise = (LEVELS - 1) * self.per_level.max(1);
        let margin = 2 + max_raise / LAYOUT.height().max(1);
        let corners = [
            Cell::new(0, -max_raise),
            Cell::new(i32::from(content.width()), -max_raise),
            Cell::new(0, i32::from(content.height())),
            Cell::new(i32::from(content.width()), i32::from(content.height())),
        ];
        let mut min_col = i32::MAX;
        let mut max_col = i32::MIN;
        let mut min_row = i32::MAX;
        let mut max_row = i32::MIN;
        for corner in corners {
            let world_cell = Cell::new(corner.x + center.x, corner.y + center.y);
            let tile = LAYOUT.cell_to_tile(world_cell);
            min_col = min_col.min(tile.col - margin);
            max_col = max_col.max(tile.col + margin);
            min_row = min_row.min(tile.row - margin);
            max_row = max_row.max(tile.row + margin);
        }

        let mut visible: Vec<Tile> = Vec::new();
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                if self.world.in_bounds(col, row) {
                    visible.push(Tile::new(col, row));
                }
            }
        }
        // Ascending col + row only: painter's algorithm sorts by *map*
        // position, never by elevation. If a tall tile's height were folded
        // into the sort key, a mountain two rows behind the camera-facing
        // tile could be drawn after (and therefore on top of) a tile that is
        // actually nearer the viewer, which breaks the occlusion the whole
        // technique depends on. Elevation affects only *where on screen* a
        // tile is drawn, never *when* in the draw order.
        visible.sort_by_key(|&t| IsoLayout::depth(t));

        for tile in visible {
            self.draw_tile(term, content, tile, center);
        }
    }

    fn status(&self) -> String {
        let biome = self
            .world
            .biome_at(self.cursor_tile.col, self.cursor_tile.row);
        format!(
            "tile ({}, {})  {}  level {}/{}  per-level {}  seed {}",
            self.cursor_tile.col,
            self.cursor_tile.row,
            biome.name(),
            self.level_at(self.cursor_tile),
            LEVELS - 1,
            self.per_level,
            self.world.seed()
        )
    }
}

impl Demo for IsoElevation {
    const NAME: &'static str = "06_iso_elevation";
    const TITLE: &'static str = "06 Isometric elevation";
    const BLURB: &'static str =
        "Staggered 2.5D stacking with cliff faces; depth sort ignores height.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("+/-", "exaggerate/flatten"),
            ("O", "wireframe"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        let (title, content, status) = ui::split_chrome(term.area());
        if !self.handle_events(term, content) {
            return false;
        }

        ui::fill(term, content, Style::new().bg(ui::BG));
        self.draw(term, content);
        ui::title_bar::<B, Self>(term, title);
        let text = self.status();
        ui::status_bar::<B, Self>(term, status, &text, &self.fps);
        let _ = |t: &mut Terminal<B>| t.print_styled_str(0, 0, "", Style::new());

        term.present().ok();
        true
    }
}

ascii_tile_demos::demo_main!(IsoElevation);
