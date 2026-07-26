//! 05: Isometric diamond -- 2:1 dimetric projection with painter's-algorithm
//! depth sorting.
//!
//! The classic isometric look: each tile is a diamond, `col`/`row` axes run
//! diagonally on screen, and tiles are drawn back-to-front so nearer tiles
//! naturally overlap farther ones. No z-buffer, no occlusion test per pixel --
//! just a sort key and draw order.
//!
//! Techniques on show:
//!
//! - **Dimetric projection** ([`tilekit::geom::IsoLayout::tile_to_cell`]): the
//!   `(col - row, col + row)` transform that rotates a square grid 45 degrees
//!   on screen. See [GameDevMath's isometric grid
//!   math](https://gamedevmath.com/isometric-grid/).
//! - **Diamond rasterization** ([`IsoLayout::span_at`]): each tile is filled
//!   by walking its rows and asking "how wide is the diamond here", with no
//!   per-cell inside/outside test.
//! - **Painter's algorithm** ([`IsoLayout::depth`]): tiles are drawn in
//!   ascending `col + row` order (back to front), and units are inserted into
//!   that same order by the tile they stand on, so a unit walking behind a
//!   diamond is genuinely covered by it and a unit in front draws over it. See
//!   Brendan Sechter on [draw
//!   order](https://sgeos.github.io/games/graphics/projection/2026/04/30/draw_order_y_sort_z_sort_and_painters_algorithm.html).
//! - **Isometric picking** ([`IsoLayout::cell_to_tile`]): exact integer
//!   inversion of the projection, so the mouse resolves to a tile with no
//!   float rounding error at diamond edges.
//!
//! ```sh
//! cargo run --example 05_iso_diamond --features crossterm
//! cargo run --example 05_iso_diamond --features software
//! cargo run --example 05_iso_diamond --features gl
//! cargo run --example 05_iso_diamond  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, IsoLayout, Tile};
use tilekit::palette::{self, mix, scale};
use tilekit::world::{Biome, World};

/// World size in tiles. Kept modest: an isometric diamond view shows far
/// fewer tiles per screen than a flat cell view (each tile costs a whole
/// diamond's worth of cells), so a huge world would mostly sit off camera.
const WORLD_W: i32 = 70;
/// See [`WORLD_W`].
const WORLD_H: i32 = 70;

/// How many cells a unit walks per second along its road path.
const UNIT_SPEED: f32 = 2.4;

/// One of the three tile sizes `Z` cycles through.
const ZOOM_LEVELS: [IsoLayout; 3] = [IsoLayout::SMALL, IsoLayout::STANDARD, IsoLayout::LARGE];

/// A unit walking a fixed path of tiles, looping back to the start.
///
/// Exists purely to prove the depth sort is correct: a unit must be occluded
/// by any tile whose diamond is nearer the camera, and must occlude any tile
/// farther away, exactly as if it were terrain at its own position.
struct Unit {
    path: Vec<Tile>,
    /// Fractional index into `path`; the integer part is the current segment,
    /// the fraction is progress across it.
    progress: f32,
    glyph: char,
    color: retroglyph_core::Color,
}

impl Unit {
    /// Interpolated screen position between the current path segment's two
    /// tiles, so the unit glides smoothly rather than jumping tile to tile.
    fn position(&self, layout: IsoLayout) -> Cell {
        if self.path.len() < 2 {
            return self
                .path
                .first()
                .map_or(Cell::new(0, 0), |&t| layout.tile_to_cell(t));
        }
        let n = self.path.len();
        let idx = self.progress.floor() as usize % n;
        let next = (idx + 1) % n;
        let t = self.progress.fract();
        let a = layout.tile_to_cell(self.path[idx]);
        let b = layout.tile_to_cell(self.path[next]);
        Cell::new(
            ((b.x - a.x) as f32).mul_add(t, a.x as f32).round() as i32,
            ((b.y - a.y) as f32).mul_add(t, a.y as f32).round() as i32,
        )
    }

    /// The tile this unit currently occupies, for depth-sort purposes. Uses
    /// the nearer of the two endpoints of its current segment so the sort key
    /// changes exactly when the unit visually crosses into the next tile.
    fn depth_tile(&self) -> Tile {
        if self.path.is_empty() {
            return Tile::new(0, 0);
        }
        let n = self.path.len();
        let idx = self.progress.floor() as usize % n;
        let next = (idx + 1) % n;
        if self.progress.fract() < 0.5 {
            self.path[idx]
        } else {
            self.path[next]
        }
    }

    fn advance(&mut self, dt: f32) {
        if self.path.len() < 2 {
            return;
        }
        self.progress = dt.mul_add(UNIT_SPEED, self.progress);
        self.progress %= self.path.len() as f32;
    }
}

/// State: the world, the current zoom level, camera offset, and units.
pub struct IsoDiamond {
    world: World,
    zoom: usize,
    /// The tile centered under the viewport. An `IsoLayout` has no natural
    /// rectangular bound (the diamond grid is infinite in every direction), so
    /// panning is tracked as a tile position rather than a raw cell offset;
    /// converting to the projected cell center happens at draw time via
    /// [`Self::layout`]. Tracking the *tile* (rather than the cell, as
    /// `01_terrain_cells` does with `TileCamera`) is what lets `Z` re-zoom
    /// around the same map position instead of jumping, since a cell offset's
    /// meaning changes with the layout's scale but a tile position does not.
    center_tile: Tile,
    time: f32,
    fps: FpsMeter,
    cursor_tile: Tile,
    show_outlines: bool,
    units: Vec<Unit>,
    drag_from: Option<Cell>,
}

impl Default for IsoDiamond {
    fn default() -> Self {
        let world = World::generate(WORLD_W, WORLD_H, 11);
        let units = spawn_units(&world);
        // Center on the first unit's starting tile, not the map's geometric
        // center: the whole point of this demo is showing painter's-algorithm
        // occlusion between units and terrain, so the units must be on screen
        // without the viewer having to pan to find them.
        let center_tile = units
            .first()
            .and_then(|u| u.path.first().copied())
            .unwrap_or_else(|| Tile::new(WORLD_W / 2, WORLD_H / 2));
        Self {
            world,
            zoom: 1,
            center_tile,
            time: 0.0,
            fps: FpsMeter::new(),
            cursor_tile: center_tile,
            show_outlines: false,
            units,
            drag_from: None,
        }
    }
}

/// Builds a few units that patrol the road network, so painter's-algorithm
/// occlusion has something moving to demonstrate it on. Falls back to a
/// stationary patrol around the map center if the world rolled no roads.
fn spawn_units(world: &World) -> Vec<Unit> {
    let mut road_tiles: Vec<Tile> = Vec::new();
    for y in 0..world.height() {
        for x in 0..world.width() {
            if world.road_at(x, y) {
                road_tiles.push(Tile::new(x, y));
            }
        }
    }
    if road_tiles.len() < 8 {
        // No road network: walk a small square loop so the demo still shows
        // occlusion instead of an empty map.
        let (cx, cy) = (world.width() / 2, world.height() / 2);
        let loop_path = vec![
            Tile::new(cx - 3, cy - 3),
            Tile::new(cx + 3, cy - 3),
            Tile::new(cx + 3, cy + 3),
            Tile::new(cx - 3, cy + 3),
        ];
        return vec![Unit {
            path: loop_path,
            progress: 0.0,
            glyph: '\u{2691}',
            color: palette::rgb(240, 210, 120),
        }];
    }

    // Both units patrol the *same* contiguous stretch of road (one running
    // forward, one backward, out of phase) rather than disjoint halves of the
    // network: the point of this demo is showing two moving things occlude
    // each other and the terrain correctly, which only happens if they are
    // ever both on screen at once. Road tiles are collected in raster order,
    // so a short contiguous run is usually a real connected stretch of one
    // road rather than scattered points across the whole network.
    let span = road_tiles.len().clamp(4, 24);
    let stretch = road_tiles[..span].to_vec();
    let mut reversed = stretch.clone();
    reversed.reverse();
    vec![
        Unit {
            path: stretch,
            progress: 0.0,
            glyph: '\u{2691}',
            color: palette::rgb(240, 210, 120),
        },
        Unit {
            path: reversed,
            progress: span as f32 / 2.0,
            glyph: '\u{25c6}',
            color: palette::rgb(140, 190, 240),
        },
    ]
}

impl IsoDiamond {
    const fn layout(&self) -> IsoLayout {
        ZOOM_LEVELS[self.zoom]
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        self.world = World::generate(WORLD_W, WORLD_H, seed);
        self.units = spawn_units(&self.world);
        self.center_tile = self
            .units
            .first()
            .and_then(|u| u.path.first().copied())
            .unwrap_or_else(|| Tile::new(WORLD_W / 2, WORLD_H / 2));
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
                        KeyCode::Char('z' | 'Z') => self.zoom = (self.zoom + 1) % ZOOM_LEVELS.len(),
                        KeyCode::Char('o' | 'O') => self.show_outlines = !self.show_outlines,
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
                let world_cell = self.screen_to_world_cell(screen, content);
                self.cursor_tile = self.layout().cell_to_tile(world_cell);
                if matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.drag_from = Some(screen);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.drag_from {
                    // A screen-space drag delta maps straight onto the
                    // projected cell space `center_tile` is expressed through
                    // (dragging is a pixel operation, not a tile operation),
                    // so pan by re-deriving the center tile from its current
                    // projected cell minus the drag delta.
                    let layout = self.layout();
                    let current = layout.tile_to_cell(self.center_tile);
                    let moved = Cell::new(
                        current.x - (screen.x - from.x),
                        current.y - (screen.y - from.y),
                    );
                    self.center_tile = layout.cell_to_tile(moved);
                    self.drag_from = Some(screen);
                }
                let world_cell = self.screen_to_world_cell(screen, content);
                self.cursor_tile = self.layout().cell_to_tile(world_cell);
            }
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::ScrollUp => self.pan_rows(-3),
            MouseEventKind::ScrollDown => self.pan_rows(3),
            _ => {}
        }
    }

    /// Pans vertically by `dy` screen cells, at the current zoom level. Shared
    /// by scroll and (indirectly) by keyboard panning.
    const fn pan_rows(&mut self, dy: i32) {
        let layout = self.layout();
        let current = layout.tile_to_cell(self.center_tile);
        self.center_tile = layout.cell_to_tile(Cell::new(current.x, current.y + dy));
    }

    /// Pans by `(dx, dy)` screen cells, at the current zoom level.
    const fn pan_cells(&mut self, dx: i32, dy: i32) {
        let layout = self.layout();
        let current = layout.tile_to_cell(self.center_tile);
        self.center_tile = layout.cell_to_tile(Cell::new(current.x + dx, current.y + dy));
    }

    /// Converts a viewport-relative screen cell to the world-projected cell
    /// space `IsoLayout` operates in (i.e. undoes the centering offset applied
    /// in [`Self::draw`]).
    fn screen_to_world_cell(&self, screen: Cell, content: Rect) -> Cell {
        let layout = self.layout();
        let center = layout.tile_to_cell(self.center_tile);
        let half_w = i32::from(content.width()) / 2;
        let half_h = i32::from(content.height()) / 2;
        Cell::new(screen.x - half_w + center.x, screen.y - half_h + center.y)
    }

    /// Face color for a tile, hillshaded and with a lit top-left / shadowed
    /// bottom-right bevel applied per sub-cell offset within the diamond.
    fn tile_face(&self, tile: Tile, dx: i32, dy: i32) -> retroglyph_core::Color {
        let biome = self.world.biome_at(tile.col, tile.row);
        let mut color = biome.color();
        if biome.is_water() {
            let phase = self
                .time
                .mul_add(1.3, (tile.col as f32).mul_add(0.6, tile.row as f32 * 0.4));
            let swell = phase.sin().mul_add(0.5, 0.5);
            color = mix(color, palette::WHITE, swell * 0.18);
        }
        // Bevel: brighten the north-facing (upper) half of the diamond,
        // darken the south-facing (lower) half, so each tile reads as a
        // faceted block rather than a flat diamond of solid color.
        let factor = if dy < 0 {
            1.18
        } else if dy > 0 {
            0.72
        } else if dx < 0 {
            1.06
        } else {
            0.92
        };
        scale(color, factor)
    }

    fn put_clipped(
        surface: &mut Surface<'_>,
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
        surface.put((sx as u16, sy as u16), glyph, style);
    }

    /// Draws one tile's diamond footprint, its glyph, and (optionally) its
    /// outline.
    fn draw_tile(
        &self,
        surface: &mut Surface<'_>,
        content: Rect,
        tile: Tile,
        origin: Cell,
        layout: IsoLayout,
    ) {
        let center = layout.tile_to_cell(tile);
        let (sx, sy) = (center.x - origin.x, center.y - origin.y);
        let highlighted = tile == self.cursor_tile;

        for dy in -layout.half_h..=layout.half_h {
            let Some(span) = layout.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                let mut color = self.tile_face(tile, dx, dy);
                if highlighted {
                    color = mix(color, palette::rgb(255, 236, 170), 0.35);
                }
                Self::put_clipped(
                    surface,
                    content,
                    sx + dx,
                    sy + dy,
                    ' ',
                    Style::new().bg(color),
                );
            }
            if self.show_outlines {
                // Outline only the diamond's outer edge: the leftmost and
                // rightmost cell of each row, which traces the silhouette
                // without a separate edge-detection pass.
                let ink = palette::rgb(20, 20, 28);
                let left = Style::new().fg(ink).bg(self.tile_face(tile, -span, dy));
                let right = Style::new().fg(ink).bg(self.tile_face(tile, span, dy));
                Self::put_clipped(surface, content, sx - span, sy + dy, '\u{2502}', left);
                Self::put_clipped(surface, content, sx + span, sy + dy, '\u{2502}', right);
            }
        }

        // The glyph sits on the tile's own face color, not on a default
        // background. Omitting the background is not a no-op on a pixel
        // backend: `Color::Default` resolves to the surface's clear color, so
        // every glyph would punch a black rectangle through the diamond it is
        // supposed to be standing on.
        let mut face = self.tile_face(tile, 0, 0);
        if highlighted {
            face = mix(face, palette::rgb(255, 236, 170), 0.35);
        }
        let biome = self.world.biome_at(tile.col, tile.row);
        if let Some(landmark) = self.world.landmark_at(tile.col, tile.row) {
            let (glyph, color) = landmark.site.glyph_color();
            let style = Style::new().fg(color).bg(face);
            Self::put_clipped(surface, content, sx, sy, glyph, style);
        } else if !biome.is_water() && biome != Biome::Peak {
            let glyph = biome.glyph();
            let style = Style::new().fg(scale(biome.color(), 1.4)).bg(face);
            Self::put_clipped(surface, content, sx, sy, glyph, style);
        }
    }

    fn draw(&self, surface: &mut Surface<'_>, content: Rect) {
        let layout = self.layout();
        // `origin` is the projected cell that lands at the viewport's
        // top-left corner: the centered tile's own projected cell, shifted
        // up-left by half the viewport. `screen_to_world_cell` performs the
        // same subtraction, so picking and drawing agree on one transform.
        let center_cell = layout.tile_to_cell(self.center_tile);
        let center = Cell::new(
            center_cell.x - i32::from(content.width()) / 2,
            center_cell.y - i32::from(content.height()) / 2,
        );

        // Visible tile range: invert the projection at the viewport's four
        // corners plus a one-tile margin, so diamonds straddling the edge
        // still draw (and get clipped per cell) instead of popping in.
        let margin = 2;
        let corners = [
            Cell::new(0, 0),
            Cell::new(i32::from(content.width()), 0),
            Cell::new(0, i32::from(content.height())),
            Cell::new(i32::from(content.width()), i32::from(content.height())),
        ];
        let mut min_col = i32::MAX;
        let mut max_col = i32::MIN;
        let mut min_row = i32::MAX;
        let mut max_row = i32::MIN;
        for corner in corners {
            let world_cell = Cell::new(corner.x + center.x, corner.y + center.y);
            let tile = layout.cell_to_tile(world_cell);
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
        // Painter's algorithm: ascending depth draws back-to-front. A stable
        // sort keeps tiles at equal depth (the same screen row) in a
        // consistent left-to-right order frame over frame.
        visible.sort_by_key(|&t| IsoLayout::depth(t));

        // Units are inserted into the same depth-sorted sequence as tiles, at
        // the tile they currently stand on, rather than drawn in a separate
        // pass after all terrain. Drawing them separately (even "on top") is
        // exactly the bug painter's algorithm exists to avoid: a unit walking
        // behind a hill must be covered by it, not merely by nothing.
        let mut draw_order: Vec<(i32, DrawItem)> = visible
            .iter()
            .map(|&t| (IsoLayout::depth(t), DrawItem::Tile(t)))
            .collect();
        for (i, unit) in self.units.iter().enumerate() {
            let depth_tile = unit.depth_tile();
            draw_order.push((IsoLayout::depth(depth_tile), DrawItem::Unit(i)));
        }
        draw_order.sort_by_key(|(depth, _)| *depth);

        for (_, item) in draw_order {
            match item {
                DrawItem::Tile(tile) => self.draw_tile(surface, content, tile, center, layout),
                DrawItem::Unit(i) => {
                    let unit = &self.units[i];
                    let pos = unit.position(layout);
                    // On the tile the unit is standing on, not on the clear
                    // color: an unset background resolves to black on a pixel
                    // backend and would cut a hole in the ground under it.
                    let ground = self.tile_face(unit.depth_tile(), 0, 0);
                    Self::put_clipped(
                        surface,
                        content,
                        pos.x - center.x,
                        pos.y - center.y,
                        unit.glyph,
                        Style::new().fg(unit.color).bg(ground),
                    );
                }
            }
        }
    }

    fn status(&self) -> String {
        let biome = self
            .world
            .biome_at(self.cursor_tile.col, self.cursor_tile.row);
        format!(
            "tile ({}, {})  {}  zoom {}x{}  seed {}",
            self.cursor_tile.col,
            self.cursor_tile.row,
            biome.name(),
            self.layout().width(),
            self.layout().height(),
            self.world.seed()
        )
    }
}

/// One entry in the combined tile/unit draw order.
enum DrawItem {
    Tile(Tile),
    Unit(usize),
}

impl Demo for IsoDiamond {
    const NAME: &'static str = "05_iso_diamond";
    const TITLE: &'static str = "05 Isometric diamond";
    const BLURB: &'static str = "2:1 dimetric projection with painter's-algorithm depth sorting.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("drag", "pan"),
            ("Z", "zoom"),
            ("O", "outlines"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        for unit in &mut self.units {
            unit.advance(frame.delta.as_secs_f32());
        }

        let (title, content, status) = ui::split_chrome(term.area());
        if !self.handle_events(term, content) {
            return false;
        }

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(IsoDiamond);
