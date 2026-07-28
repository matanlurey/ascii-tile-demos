//! 65: Domesday shire -- an isometric manor map beside a minimap with a
//! viewport rectangle, adapted from Conqueror AD 1086 (Sierra, 1995).
//!
//! Fifty-nine demos precede this one and not a single one draws the most
//! common strategy-game widget there is: a small overview of the whole world
//! with a box showing where the main camera is looking. This demo's headline
//! is building that widget properly rather than decoratively. Three things
//! make it real:
//!
//! - an **aggregation rule** for turning the world into minimap cells
//!   (majority-vote dominant biome per block, the same rule
//!   [`20_realm_map`](../20_realm_map) and
//!   [`02_chunky_tiles`](../02_chunky_tiles) use, justified there and reused
//!   here rather than reinvented);
//! - a **viewport rectangle derived from the camera**, not tracked
//!   separately, so panning the manor map can never let the two drift; and
//! - **clicking the minimap moving the camera** through the exact inverse of
//!   the forward projection, pinned by [`mod@tests::minimap_round_trip`].
//!
//! The world doubles as Domesday England: each Voronoi province
//! ([`tilekit::world::World::province_at`]) is a manor with a generated
//! tenant-in-chief and a census, read off in the sidebar and rewritten
//! whenever the camera moves to a new manor -- the second technique, a
//! stone-framed ledger panel in the Domesday Book's own vocabulary (`Home
//! Of`, `Village Census`, `Wealth`).
//!
//! Techniques on show:
//!
//! - **Dimetric projection** ([`tilekit::geom::IsoLayout`]): the manor map is
//!   drawn exactly as [`05_iso_diamond`](../05_iso_diamond) draws its world,
//!   reusing the library projection rather than a second hand-rolled one.
//! - **Minimap aggregation** ([`Minimap::build`]): majority-vote dominant
//!   biome per block. A mean-elevation or "any river" pick would blur a
//!   coastline into mush at this many-cells-per-block ratio; dominant biome
//!   is what actually answers "what color is this region" at a glance, which
//!   is the only question a minimap has to answer.
//! - **An exact round-trip coordinate map** ([`world_x_to_col`],
//!   [`col_to_world_x`]): the world width is constructed as an exact multiple
//!   of the minimap width, so integer division has no remainder to lose and
//!   forward-then-inverse (or inverse-then-forward) is the identity for every
//!   valid coordinate, not merely close.
//! - **Ploughed strip fields**: arable tiles alternate furrow color by a
//!   fixed-width strip index, which is what makes the map read as
//!   eleventh-century open-field England rather than generic grassland.
//! - **Voronoi provinces as manors** ([`tilekit::world::World::province_at`]):
//!   the same field [`52_quiet_march`](../52_quiet_march) uses for named
//!   regions, read here as the unit a Domesday census is taken over.
//!
//! ```sh
//! cargo run --example 65_domesday_shire --features crossterm
//! cargo run --example 65_domesday_shire --features software
//! cargo run --example 65_domesday_shire --features gl
//! cargo run --example 65_domesday_shire  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel, touch::Shape};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, IsoLayout, Tile};
use tilekit::glyphs::terrain;
use tilekit::noise::Rng;
use tilekit::palette::{self, mix, scale};
use tilekit::world::{Biome, World, generate_name};

/// World width in cells. An exact multiple of [`MINIMAP_COLS`]: see the
/// module doc's round-trip claim and [`world_x_to_col`]/[`col_to_world_x`].
const WORLD_W: i32 = 180;
/// World height in cells. An exact multiple of [`MINIMAP_ROWS`].
const WORLD_H: i32 = 120;

/// Minimap width in cells.
const MINIMAP_COLS: i32 = 30;
/// Minimap height in cells.
const MINIMAP_ROWS: i32 = 20;

const _: () = assert!(
    WORLD_W % MINIMAP_COLS == 0,
    "WORLD_W must divide evenly by MINIMAP_COLS for the coordinate round trip to be exact"
);
const _: () = assert!(
    WORLD_H % MINIMAP_ROWS == 0,
    "WORLD_H must divide evenly by MINIMAP_ROWS for the coordinate round trip to be exact"
);

/// World cells per minimap column. See the divisibility asserts above.
const BLOCK_W: i32 = WORLD_W / MINIMAP_COLS;
/// World cells per minimap row.
const BLOCK_H: i32 = WORLD_H / MINIMAP_ROWS;

/// The manor map's projection. One world cell is one iso tile, matching
/// [`05_iso_diamond`](../05_iso_diamond)'s convention that `Tile::col`/`row`
/// *are* world x/y rather than a separate coordinate space.
const MANOR_LAYOUT: IsoLayout = IsoLayout::STANDARD;

/// Width in world cells of one ploughed field strip.
///
/// Three is the smallest width that still reads as a "strip" rather than a
/// dither pattern once bevel-shaded on an 8-wide diamond; anything narrower
/// disappears into the tile's own north/south shading bands.
const STRIP_WIDTH: i32 = 3;

/// How many in-game seconds one census day takes at 1x time.
const DAY_LENGTH: f32 = 2.5;

/// Norman-sounding given names for the tenant-in-chief, since
/// [`generate_name`] alone reads as a place name, not a person.
const TENANT_GIVEN_NAMES: &[&str] = &[
    "Simon", "Robert", "William", "Roger", "Hugh", "Geoffrey", "Walter", "Ranulf", "Baldwin", "Odo",
];

/// The time-acceleration factor, colour-coded and driving the simulation
/// clock, matching the `1X`/`2X`/`4X` readout in the reference screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeSpeed {
    X1,
    X2,
    X4,
}

impl TimeSpeed {
    const fn factor(self) -> f32 {
        match self {
            Self::X1 => 1.0,
            Self::X2 => 2.0,
            Self::X4 => 4.0,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::X1 => "1X",
            Self::X2 => "2X",
            Self::X4 => "4X",
        }
    }

    /// Faster reads as more urgent, so the ramp runs cool to hot: a calm
    /// green baseline through amber to a red top speed, the same threshold
    /// convention [`panel::threshold`] uses elsewhere in the gallery.
    const fn color(self) -> Color {
        match self {
            Self::X1 => palette::rgb(120, 210, 120),
            Self::X2 => palette::rgb(230, 190, 90),
            Self::X4 => palette::rgb(230, 90, 80),
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::X1 => Self::X2,
            Self::X2 => Self::X4,
            Self::X4 => Self::X1,
        }
    }
}

/// Which sidebar tab is showing. `Map` is the minimap and census; `Orders`
/// and `Help` are real, distinct panels, not decoration -- clicking the tab
/// strip has to change what is on screen or it is not a tab strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Map,
    Orders,
    Help,
}

/// One Voronoi province, read as a Domesday manor: a name, a tenant-in-chief,
/// and the census numbers the sidebar reports.
struct Manor {
    name: String,
    tenant: String,
    /// Passable land cells belonging to this province, counted once at
    /// generation time rather than scanned per frame.
    land_cells: u32,
    population: u32,
    wealth: u32,
}

/// A wagon walking a fixed loop of road tiles, purely to prove the manor map
/// animates on its own and to give the ploughed fields something moving
/// across them.
struct Wagon {
    path: Vec<Tile>,
    progress: f32,
}

impl Wagon {
    fn tile(&self) -> Tile {
        if self.path.is_empty() {
            return Tile::new(0, 0);
        }
        let idx = self.progress.floor() as usize % self.path.len();
        self.path[idx]
    }

    fn advance(&mut self, dt: f32) {
        if self.path.len() < 2 {
            return;
        }
        self.progress = dt.mul_add(1.6, self.progress);
        self.progress %= self.path.len() as f32;
    }
}

/// State: the world, the manor census built from it, the camera, the
/// sidebar's live selections, and the animation clock.
pub struct DomesdayShire {
    world: World,
    manors: Vec<Manor>,
    /// The downsampled world, built once per world rather than per frame.
    ///
    /// [`Minimap::build`] is a full raster scan of all `WORLD_W * WORLD_H`
    /// cells with a per-block majority vote. That is cheap once and wasteful
    /// sixty times a second, and nothing about it changes until the world is
    /// regenerated, which only [`reroll`](Self::reroll) does.
    minimap: Minimap,
    /// Name of the shire the whole world represents, generated once per
    /// world so it does not change as the camera moves between manors.
    shire_name: String,
    /// The manor-map camera. `Tile::col`/`row` are world x/y directly, so
    /// panning is a plain coordinate clamp with no cell-space round trip.
    camera: Tile,
    wagon: Wagon,
    tab: Tab,
    speed: TimeSpeed,
    /// In-game day of the month, `1..=30`, advanced by [`DAY_LENGTH`] scaled
    /// by [`TimeSpeed::factor`].
    day: f32,
    time: f32,
    fps: FpsMeter,
    /// This frame's manor-map rect, recorded so a click can be converted back
    /// to a world tile through the same projection the map itself drew with.
    map_area: Rect,
    /// This frame's minimap interior rect, recorded for the same reason.
    minimap_area: Rect,
    /// This frame's tab-strip rects (`Map`, `Orders`, `Help`), recorded so a
    /// click can select a tab.
    tab_areas: [Rect; 3],
    drag_from: Option<Pos>,
}

impl Default for DomesdayShire {
    fn default() -> Self {
        Self::from_seed(1086)
    }
}

impl DomesdayShire {
    fn from_seed(seed: u32) -> Self {
        let world = World::generate(WORLD_W, WORLD_H, seed);
        let manors = build_manors(&world);
        let minimap = Minimap::build(&world);
        let mut rng = Rng::new(seed ^ 0x5348_4952);
        let shire_name = format!("{}shire", generate_name(&mut rng));
        let (sx, sy) = world.start_position();
        let camera = Tile::new(sx.clamp(0, WORLD_W - 1), sy.clamp(0, WORLD_H - 1));
        let wagon = spawn_wagon(&world);
        Self {
            world,
            manors,
            minimap,
            shire_name,
            camera,
            wagon,
            tab: Tab::Map,
            speed: TimeSpeed::X1,
            day: 17.0,
            time: 0.0,
            fps: FpsMeter::new(),
            map_area: Rect::new(0, 0, 0, 0),
            minimap_area: Rect::new(0, 0, 0, 0),
            tab_areas: [Rect::new(0, 0, 0, 0); 3],
            drag_from: None,
        }
    }

    fn reroll(&mut self) {
        let seed = self.world.seed().wrapping_add(1);
        *self = Self::from_seed(seed);
    }

    /// The manor under the camera: the census panel's whole reason for
    /// existing is to answer "what am I looking at", so it is read straight
    /// off the camera's own province rather than a separately tracked
    /// selection that could disagree with it.
    fn current_manor(&self) -> &Manor {
        let province = self.world.province_at(self.camera.col, self.camera.row);
        self.manors.get(province).unwrap_or_else(|| &self.manors[0])
    }

    fn pan(&mut self, dcol: i32, drow: i32) {
        self.camera = Tile::new(
            (self.camera.col + dcol).clamp(0, WORLD_W - 1),
            (self.camera.row + drow).clamp(0, WORLD_H - 1),
        );
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
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
                        KeyCode::Up | KeyCode::Char('w' | 'W') => self.pan(0, -step),
                        KeyCode::Down | KeyCode::Char('s' | 'S') => self.pan(0, step),
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.pan(-step, 0),
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.pan(step, 0),
                        KeyCode::Char('t' | 'T') => self.speed = self.speed.next(),
                        KeyCode::Char('r' | 'R') => self.reroll(),
                        KeyCode::Tab => {
                            self.tab = match self.tab {
                                Tab::Map => Tab::Orders,
                                Tab::Orders => Tab::Help,
                                Tab::Help => Tab::Map,
                            };
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: Pos) {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = self.tab_areas.iter().position(|r| r.contains_pos(pos)) {
                    self.tab = [Tab::Map, Tab::Orders, Tab::Help][tab];
                } else if self.minimap_area.contains_pos(pos) {
                    self.click_minimap(pos);
                } else if self.map_area.contains_pos(pos) {
                    self.drag_from = Some(pos);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                if let Some(from) = self.drag_from {
                    // Drag-to-pan in screen cells: the manor map is one cell
                    // per world cell in projected space, so a screen delta is
                    // a projected-cell delta directly, converted through the
                    // same iso inverse the render pass uses.
                    let origin_before = projected_origin(self.camera, self.map_area);
                    let before = Cell::new(
                        origin_before.x + i32::from(from.x) - i32::from(self.map_area.left()),
                        origin_before.y + i32::from(from.y) - i32::from(self.map_area.top()),
                    );
                    let after = Cell::new(
                        origin_before.x + i32::from(pos.x) - i32::from(self.map_area.left()),
                        origin_before.y + i32::from(pos.y) - i32::from(self.map_area.top()),
                    );
                    let before_tile = MANOR_LAYOUT.cell_to_tile(before);
                    let after_tile = MANOR_LAYOUT.cell_to_tile(after);
                    self.pan(
                        before_tile.col - after_tile.col,
                        before_tile.row - after_tile.row,
                    );
                    self.drag_from = Some(pos);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::ScrollUp => self.pan(0, -3),
            MouseEventKind::ScrollDown => self.pan(0, 3),
            _ => {}
        }
    }

    /// Recenters the camera on the world position the click landed on,
    /// through the exact inverse of the projection [`Self::draw_minimap`]
    /// used to place the viewport rectangle. See [`col_to_world_x`].
    fn click_minimap(&mut self, pos: Pos) {
        let col = i32::from(pos.x) - i32::from(self.minimap_area.left());
        let row = i32::from(pos.y) - i32::from(self.minimap_area.top());
        // The center of the clicked block, not its top-left corner: clicking
        // anywhere in a block should aim the camera at its middle, not snap
        // toward whichever corner integer division happens to floor to.
        let wx = col_to_world_x(col) + BLOCK_W / 2;
        let wy = row_to_world_y(row) + BLOCK_H / 2;
        self.camera = Tile::new(wx.clamp(0, WORLD_W - 1), wy.clamp(0, WORLD_H - 1));
    }

    fn draw_manor_map(&self, surface: &mut Surface<'_>, area: Rect) {
        ui::fill(surface, area, Style::new().bg(ui::BG));
        let (min, max) = visible_tile_bounds(self.camera, area);
        let origin = projected_origin(self.camera, area);

        let mut tiles: Vec<Tile> = Vec::new();
        // A one-tile buffer beyond the bounding box so diamonds whose center
        // sits just off screen but whose corner still overlaps it are not
        // clipped at the edge.
        for row in (min.row - 1)..=(max.row + 1) {
            for col in (min.col - 1)..=(max.col + 1) {
                if col < 0 || row < 0 || col >= WORLD_W || row >= WORLD_H {
                    continue;
                }
                tiles.push(Tile::new(col, row));
            }
        }
        tiles.sort_by_key(|&t| IsoLayout::depth(t));

        for tile in tiles {
            self.draw_manor_tile(surface, area, tile, origin);
        }

        let wagon_tile = self.wagon.tile();
        if wagon_tile.col >= min.col - 1
            && wagon_tile.col <= max.col + 1
            && wagon_tile.row >= min.row - 1
            && wagon_tile.row <= max.row + 1
        {
            let center = MANOR_LAYOUT.tile_to_cell(wagon_tile);
            put_clipped(
                surface,
                area,
                center.x - origin.x,
                center.y - origin.y,
                'w',
                Style::new().fg(palette::rgb(150, 100, 60)).bg(ui::BG),
            );
        }
    }

    fn draw_manor_tile(&self, surface: &mut Surface<'_>, area: Rect, tile: Tile, origin: Cell) {
        let center = MANOR_LAYOUT.tile_to_cell(tile);
        let (ox, oy) = (center.x - origin.x, center.y - origin.y);
        let biome = self.world.biome_at(tile.col, tile.row);
        let river = self.world.river_at(tile.col, tile.row);
        let road = self.world.road_at(tile.col, tile.row);
        let landmark = self.world.landmark_at(tile.col, tile.row);

        for dy in -MANOR_LAYOUT.half_h..=MANOR_LAYOUT.half_h {
            let Some(span) = MANOR_LAYOUT.span_at(dy) else {
                continue;
            };
            for dx in -span..=span {
                let color = self.face_color(tile, biome, river, road, dx, dy);
                put_clipped(surface, area, ox + dx, oy + dy, ' ', Style::new().bg(color));
            }
        }

        // Every tile carries a center glyph, the same convention
        // `05_iso_diamond` uses: the face color alone reads as terrain *type*
        // on a color backend, but the glyph is what still distinguishes
        // forest from field from water on the headless text dump this
        // gallery's snapshot tests run against. A landmark's marker takes
        // priority; the river glyph next; the terrain's own glyph otherwise.
        let face = self.face_color(tile, biome, river, road, 0, 0);
        if let Some(landmark) = landmark {
            let (glyph, color) = landmark.site.glyph_color();
            put_clipped(
                surface,
                area,
                ox,
                oy,
                glyph,
                Style::new().fg(color).bg(face),
            );
        } else if river {
            put_clipped(
                surface,
                area,
                ox,
                oy,
                terrain::WAVE,
                Style::new().fg(palette::rgb(210, 228, 240)).bg(face),
            );
        } else if !biome.is_water() {
            let glyph = biome.glyph();
            put_clipped(
                surface,
                area,
                ox,
                oy,
                glyph,
                Style::new().fg(scale(biome.color(), 1.5)).bg(face),
            );
        }
    }

    /// Face color for one sub-cell of a tile's diamond footprint.
    ///
    /// Rivers and roads override the ploughed-field base color, and every
    /// face is bevelled north-lit/south-shadowed the way
    /// [`05_iso_diamond`](../05_iso_diamond) shades its tiles, so a strip
    /// field reads as a raised block rather than a flat paint swatch.
    fn face_color(
        &self,
        tile: Tile,
        biome: Biome,
        river: bool,
        road: bool,
        dx: i32,
        dy: i32,
    ) -> Color {
        let mut base = if river {
            palette::rgb(96, 156, 214)
        } else if road {
            palette::rgb(196, 172, 128)
        } else if matches!(biome, Biome::Grassland | Biome::Savanna) {
            ploughed_strip_color(tile.col)
        } else {
            biome.color()
        };
        if biome.is_water() {
            let phase = self
                .time
                .mul_add(1.3, (tile.col as f32).mul_add(0.6, tile.row as f32 * 0.4));
            let swell = phase.sin().mul_add(0.5, 0.5);
            base = mix(base, palette::WHITE, swell * 0.18);
        }
        let factor = if dy < 0 {
            1.16
        } else if dy > 0 {
            0.74
        } else if dx < 0 {
            1.05
        } else {
            0.93
        };
        scale(base, factor)
    }

    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect) -> Rect {
        let inner = panel::Panel::new()
            .title("England")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return inner;
        }

        for row in 0..MINIMAP_ROWS.min(i32::from(inner.height())) {
            for col in 0..MINIMAP_COLS.min(i32::from(inner.width())) {
                let biome = self.minimap.get(col, row);
                // A shade glyph in the biome's colour rather than a space over
                // the biome's background. Both look the same wherever colour
                // renders, but a space carries no information at all in a
                // text dump, which is what the snapshot test and the headless
                // backend both produce. The minimap is this demo's headline,
                // so a snapshot that cannot show it is a snapshot that cannot
                // regress it.
                surface.put(
                    (inner.left() + col as u16, inner.top() + row as u16),
                    '\u{2592}',
                    Style::new()
                        .fg(biome.color())
                        .bg(scale(biome.color(), 0.45)),
                );
            }
        }

        for landmark in self
            .world
            .landmarks
            .iter()
            .filter(|l| l.site.is_settlement())
        {
            let col = world_x_to_col(landmark.x);
            let row = world_y_to_row(landmark.y);
            if col < i32::from(inner.width()) && row < i32::from(inner.height()) {
                let highlighted =
                    landmark.province == self.world.province_at(self.camera.col, self.camera.row);
                surface.put(
                    (inner.left() + col as u16, inner.top() + row as u16),
                    '\u{00b7}',
                    Style::new()
                        .fg(if highlighted {
                            palette::rgb(255, 220, 120)
                        } else {
                            palette::WHITE
                        })
                        .bg(palette::BLACK),
                );
            }
        }

        // The viewport rectangle: derived here from `self.camera` and the
        // manor map's own drawn area, never from a value tracked separately.
        // Deriving it any other way is exactly the drift this demo exists to
        // avoid.
        let (min, max) = visible_tile_bounds(self.camera, self.map_area);
        let vx0 = world_x_to_col(min.col.max(0)).clamp(0, i32::from(inner.width()) - 1);
        let vy0 = world_y_to_row(min.row.max(0)).clamp(0, i32::from(inner.height()) - 1);
        // Degenerate case: a viewport rectangle narrower than one minimap
        // cell (a tightly zoomed map, or a tiny window) still has to draw as
        // a visible point rather than vanish, so the max edge is floored to
        // the min rather than left to cross under it.
        let vx1 = world_x_to_col(max.col.min(WORLD_W - 1)).clamp(vx0, i32::from(inner.width()) - 1);
        let vy1 =
            world_y_to_row(max.row.min(WORLD_H - 1)).clamp(vy0, i32::from(inner.height()) - 1);

        for col in vx0..=vx1 {
            trace(surface, inner, col, vy0);
            trace(surface, inner, col, vy1);
        }
        for row in vy0..=vy1 {
            trace(surface, inner, vx0, row);
            trace(surface, inner, vx1, row);
        }

        inner
    }

    /// The stone-framed census ledger for [`Self::current_manor`].
    fn draw_census(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Domesday")
            .border(panel::Border::Single)
            .bg(palette::rgb(46, 40, 34))
            .draw(surface, area);
        if inner.width() < 4 || inner.height() < 4 {
            return;
        }
        let manor = self.current_manor();
        let text = Style::new().fg(palette::rgb(224, 210, 180)).bg(inner_bg());
        let dim = Style::new().fg(palette::rgb(160, 148, 128)).bg(inner_bg());

        let mut y = inner.top();
        let line = |surface: &mut Surface<'_>, s: &str, style: Style, y: &mut u16| {
            if *y < inner.bottom() {
                surface.print((inner.left(), *y), s, style);
                *y += 1;
            }
        };

        line(surface, &self.shire_name, text, &mut y);
        line(surface, &manor.name, text, &mut y);
        y += 1;
        line(surface, "Home Of", dim, &mut y);
        line(surface, &format!("{}.", manor.tenant), text, &mut y);
        y += 1;
        line(surface, "Village Census", dim, &mut y);
        line(
            surface,
            &format!(
                "{} {}, 1086",
                month_name((self.day / 30.0) as i64),
                (self.day % 30.0).floor() as i64 + 1
            ),
            text,
            &mut y,
        );
        if y < inner.bottom() {
            surface.print((inner.left(), y), "Time         ", dim);
            surface.print(
                (inner.left() + 13, y),
                self.speed.label(),
                Style::new().fg(self.speed.color()).bg(inner_bg()),
            );
            y += 1;
        }
        line(
            surface,
            &format!("Wealth       {}", manor.wealth),
            text,
            &mut y,
        );
        line(
            surface,
            &format!("Census       {}", manor.population),
            text,
            &mut y,
        );
    }

    fn draw_tabs(&self, surface: &mut Surface<'_>, area: Rect) -> [Rect; 3] {
        let cols = panel::columns(area, 3, 1);
        let labels = [
            ("Map", Tab::Map),
            ("Orders", Tab::Orders),
            ("Help", Tab::Help),
        ];
        let mut rects = [area; 3];
        for (i, (label, tab)) in labels.into_iter().enumerate() {
            let rect = cols[i];
            rects[i] = rect;
            let active = self.tab == tab;
            let style = Style::new()
                .fg(if active { palette::BLACK } else { ui::FG })
                .bg(if active { ui::ACCENT } else { panel::PANEL_BG });
            surface.fill_rect(rect, ' ', style);
            let x = rect.left() + rect.width().saturating_sub(label.len() as u16) / 2;
            surface.print((x, rect.top()), label, style);
        }
        rects
    }

    fn draw_orders(surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Orders").draw(surface, area);
        if inner.width() < 4 {
            return;
        }
        let style = Style::new().fg(ui::FG).bg(panel::PANEL_BG);
        for (i, line) in [
            "Raise levy",
            "Collect tithe",
            "Send messenger",
            "Muster fyrd",
        ]
        .into_iter()
        .enumerate()
        {
            let y = inner.top() + i as u16;
            if y < inner.bottom() {
                surface.print((inner.left(), y), line, style);
            }
        }
    }

    fn draw_help(surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Help").draw(surface, area);
        if inner.width() < 4 {
            return;
        }
        let style = Style::new().fg(ui::FG).bg(panel::PANEL_BG);
        for (i, (key, desc)) in Self::keys().iter().enumerate() {
            let y = inner.top() + i as u16;
            if y < inner.bottom() {
                surface.print((inner.left(), y), &format!("{key}: {desc}"), style);
            }
        }
    }

    fn status(&self) -> String {
        let manor = self.current_manor();
        format!(
            "{}  {} land cells  wealth {}  census {}  {} time  seed {}",
            manor.name,
            manor.land_cells,
            manor.wealth,
            manor.population,
            self.speed.label(),
            self.world.seed()
        )
    }
}

/// Background the census panel's rows sit on, matching [`Panel::bg`].
const fn inner_bg() -> Color {
    palette::rgb(46, 40, 34)
}

/// Alternating furrow color for the ploughed strip at world column `x`.
///
/// Alternation is keyed on `x` alone (a vertical strip running north-south)
/// rather than on both axes, because open-field strip farming genuinely ran
/// long and thin in one direction; keying on both axes would produce a
/// checkerboard, which is what a fantasy-map generator does and specifically
/// not what eleventh-century England looked like from above.
const fn ploughed_strip_color(x: i32) -> Color {
    if x.div_euclid(STRIP_WIDTH) % 2 == 0 {
        palette::rgb(107, 142, 60)
    } else {
        palette::rgb(150, 122, 78)
    }
}

/// The projected-cell origin the manor map is drawn from: the camera's own
/// projected position, offset back by half the viewport so the camera tile
/// lands in the center of `area`. Both the render pass and every screen<->
/// world conversion (drag, bounds) go through this one function, which is
/// what keeps them from disagreeing.
fn projected_origin(camera: Tile, area: Rect) -> Cell {
    let center = MANOR_LAYOUT.tile_to_cell(camera);
    Cell::new(
        center.x - i32::from(area.width()) / 2,
        center.y - i32::from(area.height()) / 2,
    )
}

/// The bounding box, in world tile coordinates, of every tile whose diamond
/// can touch `area` under the current camera.
///
/// Computed by unprojecting `area`'s four screen corners rather than by
/// tracking a separate "visible range": this is the single source of truth
/// both the manor map's draw loop and the minimap's viewport rectangle read
/// from, which is what guarantees they can never disagree about what is on
/// screen.
fn visible_tile_bounds(camera: Tile, area: Rect) -> (Tile, Tile) {
    let origin = projected_origin(camera, area);
    let w = i32::from(area.width()).max(1);
    let h = i32::from(area.height()).max(1);
    let corners = [
        Cell::new(origin.x, origin.y),
        Cell::new(origin.x + w - 1, origin.y),
        Cell::new(origin.x, origin.y + h - 1),
        Cell::new(origin.x + w - 1, origin.y + h - 1),
    ];
    let tiles: Vec<Tile> = corners
        .into_iter()
        .map(|c| MANOR_LAYOUT.cell_to_tile(c))
        .collect();
    let min_col = tiles.iter().map(|t| t.col).min().unwrap_or(0);
    let max_col = tiles.iter().map(|t| t.col).max().unwrap_or(0);
    let min_row = tiles.iter().map(|t| t.row).min().unwrap_or(0);
    let max_row = tiles.iter().map(|t| t.row).max().unwrap_or(0);
    (
        Tile::new(min_col.clamp(0, WORLD_W - 1), min_row.clamp(0, WORLD_H - 1)),
        Tile::new(max_col.clamp(0, WORLD_W - 1), max_row.clamp(0, WORLD_H - 1)),
    )
}

/// World x to minimap column. Truncating integer division, block-aligned:
/// see [`col_to_world_x`] for why this pair round-trips exactly.
const fn world_x_to_col(x: i32) -> i32 {
    x * MINIMAP_COLS / WORLD_W
}

/// World y to minimap row. See [`world_x_to_col`].
const fn world_y_to_row(y: i32) -> i32 {
    y * MINIMAP_ROWS / WORLD_H
}

/// Minimap column to the world x at its left edge.
///
/// `WORLD_W` is constructed as `MINIMAP_COLS * BLOCK_W` (enforced by the
/// module's `const _: () = assert!(..)`), so `col * WORLD_W / MINIMAP_COLS`
/// reduces to `col * BLOCK_W` exactly, with no remainder for the forward
/// mapping to round away. That is what makes
/// `world_x_to_col(col_to_world_x(col)) == col` hold for every `col` in
/// range, pinned in [`tests::minimap_round_trip`], rather than merely being
/// close for most of them.
const fn col_to_world_x(col: i32) -> i32 {
    col * WORLD_W / MINIMAP_COLS
}

/// Minimap row to the world y at its top edge. See [`col_to_world_x`].
const fn row_to_world_y(row: i32) -> i32 {
    row * WORLD_H / MINIMAP_ROWS
}

/// Draws one cell of the minimap's viewport-rectangle outline.
fn trace(surface: &mut Surface<'_>, inner: Rect, col: i32, row: i32) {
    if col < 0 || row < 0 || col >= i32::from(inner.width()) || row >= i32::from(inner.height()) {
        return;
    }
    let pos = (inner.left() + col as u16, inner.top() + row as u16);
    // Read the cell already drawn there and swap it under a bright outline
    // color rather than blanking it, so the traced box still shows the
    // terrain underneath it.
    surface.put(
        pos,
        '\u{2588}',
        Style::new()
            .fg(palette::rgb(230, 70, 60))
            .bg(palette::BLACK),
    );
}

fn put_clipped(surface: &mut Surface<'_>, area: Rect, x: i32, y: i32, glyph: char, style: Style) {
    if x < 0 || y < 0 {
        return;
    }
    let (sx, sy) = (i32::from(area.left()) + x, i32::from(area.top()) + y);
    if sx < 0 || sy < 0 || sx >= i32::from(area.right()) || sy >= i32::from(area.bottom()) {
        return;
    }
    surface.put((sx as u16, sy as u16), glyph, style);
}

/// The whole world downsampled to [`MINIMAP_COLS`] x [`MINIMAP_ROWS`] cells.
///
/// Built once per world rather than per frame: at 180x120 cells the full
/// majority-vote scan is cheap once, but re-running it every frame while the
/// minimap draws would be wasted work identical to the previous frame's.
struct Minimap {
    biomes: Vec<Biome>,
}

impl Minimap {
    /// Aggregates by dominant (majority-vote) biome per block.
    ///
    /// The alternative aggregation rules -- mean elevation, "does this block
    /// contain a river" -- either answer a question nobody is asking of a
    /// minimap ("how tall, on average, is this region") or throw away most
    /// of the block's information for a single boolean. Dominant biome is
    /// what a minimap actually needs to answer: "if I clicked here, roughly
    /// what would I see". Ties break on the biome's own `Ord` (declared once
    /// in [`tilekit::world::Biome`], not on iteration order), so the map is
    /// reproducible from its seed alone.
    fn build(world: &World) -> Self {
        let mut biomes = Vec::with_capacity((MINIMAP_COLS * MINIMAP_ROWS) as usize);
        for row in 0..MINIMAP_ROWS {
            for col in 0..MINIMAP_COLS {
                let (x0, y0) = (col_to_world_x(col), row_to_world_y(row));
                let mut counts: Vec<(Biome, u32)> = Vec::new();
                for dy in 0..BLOCK_H {
                    for dx in 0..BLOCK_W {
                        let biome = world.biome_at(x0 + dx, y0 + dy);
                        if let Some(slot) = counts.iter_mut().find(|(b, _)| *b == biome) {
                            slot.1 += 1;
                        } else {
                            counts.push((biome, 1));
                        }
                    }
                }
                let dominant = counts
                    .into_iter()
                    .max_by_key(|&(biome, n)| (n, core::cmp::Reverse(biome)))
                    .map_or(Biome::Ocean, |(b, _)| b);
                biomes.push(dominant);
            }
        }
        Self { biomes }
    }

    fn get(&self, col: i32, row: i32) -> Biome {
        if col < 0 || row < 0 || col >= MINIMAP_COLS || row >= MINIMAP_ROWS {
            return Biome::Ocean;
        }
        self.biomes[(row * MINIMAP_COLS + col) as usize]
    }
}

/// Builds the per-province census. Runs once at world generation, not per
/// frame: a full-map scan every frame just to read whichever manor the
/// camera happens to sit over would be pointless work repeated 60 times a
/// second for numbers that never change between rerolls.
fn build_manors(world: &World) -> Vec<Manor> {
    let count = world.province_count().max(1);
    let mut land_cells = vec![0u32; count];
    let mut fertility = vec![0u32; count];
    for y in 0..world.height() {
        for x in 0..world.width() {
            let biome = world.biome_at(x, y);
            if !biome.is_passable() {
                continue;
            }
            let p = world.province_at(x, y);
            land_cells[p] += 1;
            fertility[p] += manor_fertility(biome);
        }
    }

    (0..count)
        .map(|p| {
            // Seeded from the world seed and the province index, not from any
            // `HashMap` order: two runs of the same seed must produce the
            // same manors, and iterating `province_at` in raster order above
            // already guarantees `land_cells`/`fertility` are deterministic,
            // but the *names* need their own seed per province rather than
            // one shared `Rng` whose state depends on how many provinces were
            // visited before this one in some future refactor.
            let mut rng =
                Rng::new(world.seed() ^ (p as u32).wrapping_mul(0x0100_0193).wrapping_add(1));
            let name = generate_name(&mut rng);
            let given = rng.choose(TENANT_GIVEN_NAMES).copied().unwrap_or("Odo");
            let surname = generate_name(&mut rng);
            Manor {
                name,
                tenant: format!("{given} Le {surname}"),
                land_cells: land_cells[p],
                // Domesday's own census counted households, not souls; the
                // multiplier is an abstract "people per settled cell" and not
                // a historical claim, just enough to make the number read as
                // a population rather than a cell count.
                population: land_cells[p] * 4,
                wealth: fertility[p],
            }
        })
        .collect()
}

/// Per-cell economic value for [`build_manors`]'s wealth total. Arable and
/// coastal land supports a manor; mountains and ice do not.
const fn manor_fertility(biome: Biome) -> u32 {
    match biome {
        Biome::Grassland | Biome::Savanna => 3,
        Biome::Forest | Biome::Taiga | Biome::Coast => 2,
        Biome::Marsh | Biome::Tundra | Biome::Scrubland => 1,
        _ => 0,
    }
}

const fn month_name(months_elapsed: i64) -> &'static str {
    const MONTHS: [&str; 12] = [
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
        "January",
        "February",
    ];
    MONTHS[(months_elapsed.rem_euclid(12)) as usize]
}

/// Builds a wagon patrol along the first contiguous stretch of road found, or
/// a small fixed loop near the world center if the seed rolled no roads.
fn spawn_wagon(world: &World) -> Wagon {
    let mut road_tiles = Vec::new();
    for y in 0..world.height() {
        for x in 0..world.width() {
            if world.road_at(x, y) {
                road_tiles.push(Tile::new(x, y));
                if road_tiles.len() >= 20 {
                    return Wagon {
                        path: road_tiles,
                        progress: 0.0,
                    };
                }
            }
        }
    }
    if road_tiles.len() >= 2 {
        return Wagon {
            path: road_tiles,
            progress: 0.0,
        };
    }
    let (cx, cy) = (world.width() / 2, world.height() / 2);
    Wagon {
        path: vec![
            Tile::new(cx - 2, cy),
            Tile::new(cx, cy - 2),
            Tile::new(cx + 2, cy),
            Tile::new(cx, cy + 2),
        ],
        progress: 0.0,
    }
}

impl Demo for DomesdayShire {
    const NAME: &'static str = "65_domesday_shire";
    const TITLE: &'static str = "65 Domesday shire";
    const BLURB: &'static str =
        "An isometric manor beside a minimap with a viewport rectangle, Conqueror AD 1086.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan manor"),
            ("drag/click", "pan/select"),
            ("T", "time speed"),
            ("Tab", "switch tab"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.day += dt * self.speed.factor() / DAY_LENGTH;
        if self.day >= 361.0 {
            self.day -= 360.0;
        }
        self.wagon.advance(dt * self.speed.factor());
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let shape = Shape::of(content);

        let (map_area, sidebar) = if shape.stacks() {
            let sidebar_h = (content.height() / 2).max(12);
            panel::split_bottom(content, sidebar_h)
        } else {
            let sidebar_w = if shape == Shape::Landscape { 30 } else { 36 };
            panel::split_right(content, sidebar_w)
        };
        self.map_area = map_area;

        let (minimap_band, rest) = panel::split_top(sidebar, MINIMAP_ROWS as u16 + 2);
        let (tab_band, census_band) = panel::split_top(rest, 1);

        let mut surface = term.surface();
        self.draw_manor_map(&mut surface, map_area);
        self.minimap_area = self.draw_minimap(&mut surface, minimap_band);
        self.tab_areas = self.draw_tabs(&mut surface, tab_band);
        match self.tab {
            Tab::Map => self.draw_census(&mut surface, census_band),
            Tab::Orders => Self::draw_orders(&mut surface, census_band),
            Tab::Help => Self::draw_help(&mut surface, census_band),
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(DomesdayShire);

#[cfg(test)]
mod tests {
    use super::{
        MINIMAP_COLS, MINIMAP_ROWS, col_to_world_x, row_to_world_y, world_x_to_col, world_y_to_row,
    };

    /// The property the whole minimap-click feature depends on: converting a
    /// minimap column to a world x and back must return the same column for
    /// every valid column, exactly, or a click near the edge of its block
    /// would recenter the camera on the wrong block and the viewport
    /// rectangle drawn afterward would visibly disagree with where the click
    /// landed.
    #[test]
    fn minimap_round_trip() {
        for col in 0..MINIMAP_COLS {
            assert_eq!(
                world_x_to_col(col_to_world_x(col)),
                col,
                "column {col} did not round-trip"
            );
        }
        for row in 0..MINIMAP_ROWS {
            assert_eq!(
                world_y_to_row(row_to_world_y(row)),
                row,
                "row {row} did not round-trip"
            );
        }
    }

    /// The other direction: every world x maps to some column whose own
    /// block, converted back to world space, contains that x. Otherwise a
    /// camera sitting at some world position could compute a viewport
    /// rectangle that does not actually contain the column it is derived
    /// from.
    #[test]
    fn world_to_minimap_lands_in_the_correct_block() {
        for x in [0, 1, 89, 90, 91, 179] {
            let col = world_x_to_col(x);
            let block_start = col_to_world_x(col);
            let block_end = block_start + super::BLOCK_W;
            assert!(
                x >= block_start && x < block_end,
                "world x {x} mapped to column {col} spanning [{block_start}, {block_end})"
            );
        }
    }
}
