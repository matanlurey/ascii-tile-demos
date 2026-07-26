//! 19: Hex command -- a hex is a multi-cell blob, not a glyph.
//!
//! Every other hex demo in this gallery ([`07_hex_tiles`](../07_hex_tiles),
//! [`08_hex_outline`](../08_hex_outline), [`09_hex_subcell`](../09_hex_subcell))
//! packs a hex into a handful of cells: enough to see the tessellation, not
//! enough to draw *inside* one. Armoured Commander II's tactical map goes the
//! other way. Its hexes are roughly 7 cells wide and 5 tall, built from
//! pre-painted `.xp` sprites, because a WW2 tank battle needs to show a hex's
//! terrain, its unit, and whether it is under fire all at once, and none of
//! that fits in one glyph. This demo is the ASCII-native version of that idea:
//! each hex is a [`HEX_W`] x [`HEX_H`] block with a filled interior, a border,
//! and interior texture that reads as its terrain from across the room, laid
//! out on [`tilekit::geom::HexLayout`]'s pointy-top pitch.
//!
//! The interesting part is that this hex has no fixed footprint at all. A
//! pointy-top hex's bounding box is not the hex: the box's own corners belong
//! to a neighbour, and a taper formula that has to independently get every
//! one of the six edges right, with no gap and no overlap, is exactly the
//! kind of thing that is subtly wrong in a way a thin outline hides and a
//! solid fill does not (this demo's first draft had precisely that bug: a
//! one-cell gap at every seam). So instead of a formula, [`draw_hex_field`]
//! asks [`tilekit::geom::HexLayout::cell_to_tile`] which hex owns each screen
//! cell directly, one cell at a time. That function *is* this layout's
//! definition of ownership -- it is also what turns a mouse click into a tile
//! pick in every other hex demo -- so a fill built by asking it is gapless by
//! construction. A border glyph falls out of the same query for free: a cell
//! is a border cell exactly when the cell to its north or west resolves to a
//! different tile, which is also how a border is drawn exactly once rather
//! than twice in two disagreeing colors (see [`paint_cell`]).
//!
//! Techniques on show:
//!
//! - **A hex as a filled, bordered, textured block** rather than a single
//!   glyph or a bevelled few-cell tile: interior texture (`♠` scatter for
//!   forest, a `/` hatch for fields, `≈` for marsh) is what makes a zone
//!   readable as terrain from across a wide map, the way a tileset sprite
//!   would, using only colorable CP437 glyphs.
//! - **Ownership as a per-cell query**: a hex's footprint and its border are
//!   both derived from [`HexLayout::cell_to_tile`] rather than from a
//!   geometric formula, which is what makes the tessellation gapless.
//! - **Roads and rivers as center-to-center lines**: drawn independently of
//!   the hex fill, proving the hex grid is a real coordinate system a route
//!   can be plotted over, not wallpaper.
//! - **A three-panel command interface**: unit info and a command menu on the
//!   left, the hex field with A-letter/1-digit coordinate rulers in the
//!   centre, weather/mission/zone info on the right -- collapsing panels in
//!   that order as the terminal narrows.
//!
//! ```sh
//! cargo run --example 19_hex_command --features crossterm
//! cargo run --example 19_hex_command --features software
//! cargo run --example 19_hex_command --features gl
//! cargo run --example 19_hex_command  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{Cell, HexLayout, Tile};
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{self, mix, rgb, scale};

/// World size in hexes.
const HEXES_W: i32 = 20;
/// See [`HEXES_W`].
const HEXES_H: i32 = 14;

/// A hex's bounding box in cells: wide and tall enough for interior texture
/// and a legible border, which is the whole point of this demo over
/// [`08_hex_outline`](../08_hex_outline)'s thin honeycomb.
///
/// Also the layout's cell pitch directly (`pitch_x == HEX_W`, `pitch_y ==
/// HEX_H`). The exact footprint is not defined here at all: see
/// [`draw_hex_field`](HexCommand::draw_hex_field) for why it is derived from
/// [`HexLayout::cell_to_tile`] per screen cell rather than from a taper
/// formula over this bounding box.
const HEX_W: i32 = 16;
/// See [`HEX_W`].
const HEX_H: i32 = 6;

/// The layout every hex is drawn on.
const LAYOUT: HexLayout = HexLayout::new(tilekit::geom::HexOrientation::Pointy, HEX_W, HEX_H);

/// Terrain a hex can be. Distinct from [`tilekit::world::Biome`]: a wargame
/// zone is a coarser, front-line-relevant classification (fields vs. forest
/// vs. hills matters for a tank; tundra vs. jungle does not), so this is its
/// own small enum rather than a reuse that would drag in biomes this demo has
/// no use for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Terrain {
    Fields,
    Forest,
    Hills,
    Marsh,
    Rough,
    Village,
}

impl Terrain {
    /// Base fill color.
    const fn color(self) -> Color {
        match self {
            Self::Fields => rgb(90, 108, 58),
            Self::Forest => rgb(48, 74, 46),
            Self::Hills => rgb(108, 104, 74),
            Self::Marsh => rgb(58, 82, 74),
            Self::Rough => rgb(96, 92, 88),
            Self::Village => rgb(120, 108, 88),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Fields => "Fields",
            Self::Forest => "Forest",
            Self::Hills => "Hills",
            Self::Marsh => "Marsh",
            Self::Rough => "Rough",
            Self::Village => "Village",
        }
    }

    /// Capture value the reference games attach to a controlled zone: better
    /// terrain (village, hills) is worth more to hold.
    const fn capture_value(self) -> u32 {
        match self {
            Self::Village => 3,
            Self::Hills => 2,
            Self::Fields | Self::Forest | Self::Rough => 1,
            Self::Marsh => 0,
        }
    }
}

/// Interior texture for a hex, given its terrain and a per-hex random phase so
/// neighbouring hexes of the same terrain do not tile identically.
///
/// `(dx, dy)` are cell-local coordinates within the hex's `HEX_W` x `HEX_H`
/// bounding box; `inside` has already been checked by the caller, so this only
/// decides *what* goes in a cell that is inside the hex, not whether one is.
fn texture(terrain: Terrain, dx: i32, dy: i32, phase: u32) -> Option<char> {
    let h = |salt: u32| hash01(phase, dx.wrapping_add(salt as i32), dy);
    match terrain {
        Terrain::Fields => {
            // A diagonal hatch: fields read as plowed rows from a distance,
            // which a scatter of dots would not convey.
            if (dx + dy * 2).rem_euclid(4) == 0 {
                Some('/')
            } else {
                None
            }
        }
        Terrain::Forest => {
            if h(11) < 0.4 {
                Some(tilekit::glyphs::terrain::CONIFER)
            } else {
                None
            }
        }
        Terrain::Hills => {
            if h(22) < 0.22 {
                Some('\u{2229}') // small arc, reads as a rounded rise
            } else {
                None
            }
        }
        Terrain::Marsh => {
            if h(33) < 0.35 {
                Some('\u{2248}')
            } else {
                None
            }
        }
        Terrain::Rough => {
            if h(44) < 0.3 {
                Some('\u{2591}')
            } else {
                None
            }
        }
        Terrain::Village => {
            // A tiny cluster of roofs near the hex centre only, so the
            // village reads as a settlement rather than as noise filling the
            // whole hex.
            if dx.abs() <= 1 && dy == 0 {
                Some('\u{25B2}')
            } else if (dx.abs(), dy.abs()) == (2, 0) {
                Some('\u{2302}')
            } else {
                None
            }
        }
    }
}

/// Who holds a hex.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    Player,
    Enemy,
    Neutral,
}

impl Control {
    const fn tint(self) -> Option<Color> {
        match self {
            Self::Player => Some(rgb(90, 130, 210)),
            Self::Enemy => Some(rgb(200, 90, 84)),
            Self::Neutral => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Player => "Friendly",
            Self::Enemy => "Enemy controlled",
            Self::Neutral => "Neutral",
        }
    }
}

/// One hex's static data.
#[derive(Clone, Copy)]
struct Zone {
    terrain: Terrain,
    control: Control,
    /// Random per-hex seed so [`texture`] does not tile identically.
    phase: u32,
}

/// Weather, drifting slowly and affecting nothing but the mood of the right
/// panel -- exactly the role it plays in `ArmCom2`'s zone info readout.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Weather {
    Clear,
    Overcast,
    Rain,
    Fog,
}

impl Weather {
    const fn color(self) -> Color {
        match self {
            Self::Clear => rgb(120, 176, 224),
            Self::Overcast => rgb(140, 140, 150),
            Self::Rain => rgb(90, 110, 150),
            Self::Fog => rgb(180, 180, 184),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Overcast => "Overcast",
            Self::Rain => "Rain",
            Self::Fog => "Fog",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Clear => Self::Overcast,
            Self::Overcast => Self::Rain,
            Self::Rain => Self::Fog,
            Self::Fog => Self::Clear,
        }
    }
}

/// A road or river between two hex centres, drawn as its own pass over the
/// grid so it reads as a route crossing zones rather than as zone decoration.
struct Route {
    from: Tile,
    to: Tile,
    river: bool,
}

/// Command tabs, the `ArmCom2` convention of Supply/Crew/Travel/Group collapsed
/// to what this demo can actually act on.
const COMMANDS: [&str; 4] = ["Advance", "Recon", "Support", "Wait"];

/// State: the generated zone map, routes, the player's unit, weather, the
/// selected hex, and the active command tab.
pub struct HexCommand {
    seed: u32,
    zones: Vec<Zone>,
    routes: Vec<Route>,
    selected: Tile,
    command: usize,
    weather: Weather,
    time: f32,
    day: u32,
    minute: u32,
    fps: FpsMeter,
}

impl Default for HexCommand {
    fn default() -> Self {
        let seed = 7;
        let (zones, routes) = generate(seed);
        Self {
            seed,
            zones,
            routes,
            selected: Tile::new(HEXES_W / 2, HEXES_H / 2),
            command: 0,
            weather: Weather::Clear,
            time: 0.0,
            day: 9,
            minute: 5 * 60 + 25,
            fps: FpsMeter::new(),
        }
    }
}

/// Builds the zone grid and a handful of road/river routes threading through
/// it, deterministic in `seed`.
fn generate(seed: u32) -> (Vec<Zone>, Vec<Route>) {
    let mut zones = Vec::with_capacity((HEXES_W * HEXES_H) as usize);
    for row in 0..HEXES_H {
        for col in 0..HEXES_W {
            let n = hash01(seed, col, row);
            let terrain = if n < 0.12 {
                Terrain::Marsh
            } else if n < 0.32 {
                Terrain::Forest
            } else if n < 0.48 {
                Terrain::Hills
            } else if n < 0.58 {
                Terrain::Rough
            } else if n < 0.62 {
                Terrain::Village
            } else {
                Terrain::Fields
            };
            // A rough front line: enemy-held toward the east, friendly toward
            // the west, with a contested band in between -- enough structure
            // for the zone-info panel to say something different hex to hex
            // without a real front-line simulation.
            let control = if col < HEXES_W / 3 {
                Control::Player
            } else if col > 2 * HEXES_W / 3 {
                Control::Enemy
            } else {
                Control::Neutral
            };
            zones.push(Zone {
                terrain,
                control,
                phase: seed
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add((row * HEXES_W + col) as u32),
            });
        }
    }

    let mut rng = Rng::new(seed ^ 0x5bd1_e995);
    let mut routes = Vec::new();
    // One road roughly following a middle row, one river roughly following
    // another: enough to prove routes cross zone boundaries, without a real
    // path generator this demo does not need.
    let road_row = HEXES_H / 3 + (rng.next_below(2) as i32);
    for col in 0..HEXES_W - 1 {
        routes.push(Route {
            from: Tile::new(col, road_row),
            to: Tile::new(col + 1, road_row),
            river: false,
        });
    }
    let river_row = 2 * HEXES_H / 3;
    for col in 0..HEXES_W - 1 {
        routes.push(Route {
            from: Tile::new(col, river_row),
            to: Tile::new(col + 1, river_row),
            river: true,
        });
    }
    let _ = rng.next_f32();
    (zones, routes)
}

impl HexCommand {
    const fn zone_index(tile: Tile) -> Option<usize> {
        if tile.col < 0 || tile.row < 0 || tile.col >= HEXES_W || tile.row >= HEXES_H {
            return None;
        }
        Some((tile.row * HEXES_W + tile.col) as usize)
    }

    fn zone(&self, tile: Tile) -> Option<Zone> {
        Self::zone_index(tile).map(|i| self.zones[i])
    }

    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        let (zones, routes) = generate(self.seed);
        self.zones = zones;
        self.routes = routes;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                let (mut dc, mut dr) = (0, 0);
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => dr = -1,
                    KeyCode::Down | KeyCode::Char('s' | 'S') => dr = 1,
                    KeyCode::Left | KeyCode::Char('a' | 'A') => dc = -1,
                    KeyCode::Right | KeyCode::Char('d' | 'D') => dc = 1,
                    KeyCode::Char('1') => self.command = 0,
                    KeyCode::Char('2') => self.command = 1,
                    KeyCode::Char('3') => self.command = 2,
                    KeyCode::Char('4') => self.command = 3,
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    KeyCode::Enter => self.command = (self.command + 1) % COMMANDS.len(),
                    _ => {}
                }
                if dc != 0 || dr != 0 {
                    let next = Tile::new(self.selected.col + dc, self.selected.row + dr);
                    if next.col >= 0 && next.row >= 0 && next.col < HEXES_W && next.row < HEXES_H {
                        self.selected = next;
                    }
                }
            }
        }
        true
    }

    /// Advances the clock and the drifting weather.
    fn tick_state(&mut self, dt: f32) {
        self.time += dt;
        // One in-game minute per two real seconds: fast enough to see the
        // clock move within a headless thumbnail capture, slow enough to
        // still read as a clock rather than a stopwatch.
        let minutes = (dt / 2.0) as u32 + u32::from((self.time % 2.0) < dt);
        self.minute += minutes;
        while self.minute >= 24 * 60 {
            self.minute -= 24 * 60;
            self.day += 1;
        }
        // Weather drifts on its own slow cycle, independent of player input,
        // which is what makes the demo animate even while nothing is pressed.
        if (self.time % 26.0) < dt {
            self.weather = self.weather.next();
        }
    }

    /// The origin (top-left cell, in world-cell space) that centres the
    /// selected hex within `area`.
    fn map_origin(&self, area: Rect) -> Cell {
        let center = LAYOUT.center_cell(self.selected);
        Cell::new(
            center.x - i32::from(area.width()) / 2,
            center.y - i32::from(area.height()) / 2,
        )
    }

    /// Draws every hex whose bounding box could touch `area`, plus routes and
    /// the selection highlight, then the coordinate rulers.
    /// Draws every hex whose footprint could touch `area`, in one pass over
    /// screen cells.
    ///
    /// Per-cell rather than per-hex-then-iterate-its-bounding-box, which is
    /// the fix for a real bug the per-hex version had: a pointy-top hex's
    /// bounding box is not the hex itself (the corners of the box belong to a
    /// neighbour), and getting a taper formula to claim every cell exactly
    /// once, with no gap and no overlap, independently at all six edges, is
    /// fragile in a way that is easy to get subtly wrong -- it was wrong here
    /// at first, leaving a one-cell gap at every seam that only showed up
    /// once the hex was drawn solid rather than as a thin outline. Asking
    /// [`HexLayout::cell_to_tile`] which hex owns a given screen cell
    /// sidesteps the problem entirely: that function *is* this layout's
    /// definition of hex ownership (it is also what turns a mouse click into
    /// a tile pick in every other hex demo), so a fill built by asking it one
    /// cell at a time is gapless by construction, not by a formula that has
    /// to independently agree with itself at every edge.
    fn draw_hex_field(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let origin = self.map_origin(area);

        for sy in area.top()..area.bottom() {
            for sx in area.left()..area.right() {
                let wx = origin.x + i32::from(sx - area.left());
                let wy = origin.y + i32::from(sy - area.top());
                let tile = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                let Some(zone) = self.zone(tile) else {
                    continue;
                };
                self.paint_cell(surface, (sx, sy), tile, zone, wx, wy);
            }
        }

        for route in &self.routes {
            self.draw_route(surface, area, origin, route);
        }

        self.draw_selection(surface, area, origin, self.selected);
        Self::draw_rulers(surface, area, origin);
        self.draw_objective(surface, area, origin);
    }

    /// The fill color for `zone`: its terrain color, tinted toward its
    /// controller. Shared by the hex fill itself and by anything drawn across
    /// a hex (a route, the selection cursor) that needs to keep the same
    /// background rather than punching through to the page color.
    fn base_color(zone: Zone) -> Color {
        zone.control.tint().map_or_else(
            || zone.terrain.color(),
            |tint| mix(zone.terrain.color(), tint, 0.28),
        )
    }

    /// Paints one screen cell that [`draw_hex_field`] has already resolved to
    /// `tile`/`zone`: fill and interior texture, or a border glyph if this
    /// cell sits on the edge shared with a differently-owned neighbour.
    ///
    /// The border is the same edge-ownership trick
    /// [`08_hex_outline`](../08_hex_outline) uses for its province borders,
    /// expressed the natural way once ownership is a per-cell query rather
    /// than a per-hex loop: a cell is a border cell if the *cell* north or
    /// west of it belongs to a different hex. Checking only those two
    /// directions, not all four, is what keeps a border from being drawn
    /// twice by two different hexes -- see the module docs.
    fn paint_cell(
        &self,
        surface: &mut Surface<'_>,
        (sx, sy): (u16, u16),
        tile: Tile,
        zone: Zone,
        wx: i32,
        wy: i32,
    ) {
        let base = Self::base_color(zone);
        let mut color = base;
        if tile == self.selected {
            color = mix(color, rgb(255, 236, 170), 0.35);
        }

        let north = LAYOUT.cell_to_tile(Cell::new(wx, wy - 1));
        let west = LAYOUT.cell_to_tile(Cell::new(wx - 1, wy));
        if north != tile || west != tile {
            let border = edge_color(self.zone(tile));
            let glyph = if north != tile && west != tile {
                '\u{253C}' // corner where three hexes meet
            } else if north != tile {
                '\u{2500}'
            } else {
                '\u{2502}'
            };
            surface.put((sx, sy), glyph, Style::new().fg(border).bg(color));
            return;
        }

        let center = LAYOUT.center_cell(tile);
        let glyph = texture(zone.terrain, wx - center.x, wy - center.y, zone.phase).unwrap_or(' ');
        let fg = mix(color, palette::WHITE, 0.4);
        surface.put((sx, sy), glyph, Style::new().fg(fg).bg(color));
    }

    /// Draws a route (road or river) as a straight cell-space line between
    /// two hex centres.
    ///
    /// Each step looks up the zone it lands in and draws over that zone's own
    /// fill color, not the page background: without that, a route reads as a
    /// string of holes punched through the terrain rather than a line drawn
    /// on top of it.
    fn draw_route(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell, route: &Route) {
        let a = LAYOUT.center_cell(route.from);
        let b = LAYOUT.center_cell(route.to);
        let steps = (a.x - b.x).abs().max((a.y - b.y).abs()).max(1);
        let (color, glyph) = if route.river {
            (rgb(70, 110, 168), '\u{2248}')
        } else {
            (rgb(150, 132, 96), '\u{2500}')
        };
        for i in 0..=steps {
            let t = f32::from(i as u16) / f32::from(steps as u16);
            let wx = ((b.x - a.x) as f32).mul_add(t, a.x as f32);
            let wy = ((b.y - a.y) as f32).mul_add(t, a.y as f32);
            let (wx, wy) = (wx.round() as i32, wy.round() as i32);
            let Some((sx, sy)) = to_screen(area, origin, wx, wy) else {
                continue;
            };
            let fill = self
                .zone(LAYOUT.cell_to_tile(Cell::new(wx, wy)))
                .map_or(ui::BG, Self::base_color);
            surface.put((sx, sy), glyph, Style::new().fg(color).bg(fill));
        }
    }

    /// Draws a bracket highlight around the selected hex's bounding box, over
    /// that hex's own fill color for the same reason [`draw_route`] does.
    fn draw_selection(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell, tile: Tile) {
        let hex_origin = LAYOUT.tile_to_cell(tile);
        let fill = self.zone(tile).map_or(ui::BG, Self::base_color);
        let style = Style::new().fg(rgb(255, 246, 200)).bg(fill);
        let mid = HEX_H / 2;
        let corners = [
            (hex_origin.x, hex_origin.y + mid),
            (hex_origin.x + HEX_W - 1, hex_origin.y + mid),
        ];
        for (i, (wx, wy)) in corners.into_iter().enumerate() {
            if let Some((sx, sy)) = to_screen(area, origin, wx, wy) {
                surface.put((sx, sy), if i == 0 { '[' } else { ']' }, style);
            }
        }
    }

    /// Draws A-letter rows and 1-digit columns aligned to hex centres, on all
    /// four edges of the map area.
    fn draw_rulers(surface: &mut Surface<'_>, area: Rect, origin: Cell) {
        let style = Style::new().fg(ui::DIM).bg(ui::CHROME_BG);
        for row in 0..HEXES_H {
            let center = LAYOUT.center_cell(Tile::new(0, row));
            if let Some((_, sy)) = to_screen(area, origin, center.x, center.y) {
                let label = (b'A' + (row % 26) as u8) as char;
                if area.left() > 0 {
                    surface.put((area.left() - 1, sy), label, style);
                }
            }
        }
        for col in 0..HEXES_W {
            let center = LAYOUT.center_cell(Tile::new(col, 0));
            if let Some((sx, _)) = to_screen(area, origin, center.x, center.y) {
                let label = char::from(b'0' + ((col + 1) % 10) as u8);
                if area.bottom() < u16::MAX {
                    surface.put((sx, area.bottom()), label, style);
                }
            }
        }
    }

    /// A blinking objective marker on a fixed hex, so there is always
    /// something animating even if the camera and weather have not changed
    /// this frame.
    fn draw_objective(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell) {
        let objective = Tile::new(HEXES_W / 2, HEXES_H / 2 - 2);
        if objective.row < 0 {
            return;
        }
        let center = LAYOUT.center_cell(objective);
        let Some((sx, sy)) = to_screen(area, origin, center.x, center.y) else {
            return;
        };
        // A slow on/off blink rather than a fade: an objective flag is meant
        // to be unmissable, and full contrast reads at a glance where a
        // subtle pulse would not.
        if (self.time % 1.2) < 0.7 {
            let fill = self.zone(objective).map_or(ui::BG, Self::base_color);
            surface.put(
                (sx, sy),
                '\u{25B2}',
                Style::new().fg(rgb(250, 214, 120)).bg(fill),
            );
        }
    }

    /// Draws the player's unit card and command menu.
    fn draw_left_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Sherman VC")
            .border(panel::Border::Double)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }

        let mut y = inner.top();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[panel::Span::dim("Medium Tank")],
            panel::PANEL_BG,
        );
        y += 2;

        if inner.height() > 2 {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    panel::Span::keyword("ARM"),
                    panel::Span::plain(" hull 8/6  turret 8/4"),
                ],
                panel::PANEL_BG,
            );
            y += 2;
        }

        if inner.height() > 5 && inner.width() > 4 {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[panel::Span::dim("Command Menu")],
                panel::PANEL_BG,
            );
            y += 1;
            for (i, label) in COMMANDS.iter().enumerate() {
                if y >= inner.bottom() {
                    break;
                }
                let selected = i == self.command;
                let marker = if selected { '\u{25BA}' } else { ' ' };
                let color = if selected { ui::ACCENT } else { ui::FG };
                surface.print(
                    (inner.left(), y),
                    &format!("{marker}{}:{label}", i + 1),
                    Style::new().fg(color).bg(panel::PANEL_BG),
                );
                y += 1;
            }
        }

        // A small ASCII silhouette near the bottom, if there is room: a hull
        // rectangle with a turret block and barrel, all CP437 box-drawing so
        // it stays colorable rather than reaching for a real vehicle glyph.
        if inner.height() > 10 {
            let base_y = inner.bottom() - 3;
            let x0 = inner.left();
            let hull = Style::new().fg(rgb(140, 168, 120)).bg(panel::PANEL_BG);
            surface.print(
                (x0, base_y),
                "\u{2554}\u{2550}\u{2550}\u{2550}\u{2557}",
                hull,
            );
            surface.print(
                (x0, base_y + 1),
                "\u{2551}\u{2584}\u{2584}\u{2584}\u{2551}",
                hull,
            );
            surface.print(
                (x0, base_y + 2),
                "\u{255A}\u{2550}\u{2550}\u{2550}\u{255D}",
                hull,
            );
            surface.put(
                (x0 + 5, base_y),
                '\u{2500}',
                Style::new().fg(rgb(140, 168, 120)).bg(panel::PANEL_BG),
            );
        }
    }

    /// Draws weather, mission, and the selected hex's zone info.
    fn draw_right_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let (weather_area, rest) = panel::split_top(area, 6);
        let (mission_area, zone_area) = panel::split_top(rest, 6);

        let inner = panel::Panel::new()
            .title("Weather")
            .border(panel::Border::Double)
            .draw(surface, weather_area);
        if inner.height() > 0 {
            surface.fill_rect(
                Rect::new(inner.left(), inner.top(), inner.width(), 1),
                ' ',
                Style::new().bg(self.weather.color()),
            );
            if inner.height() > 2 {
                surface.print(
                    (inner.left(), inner.top() + 2),
                    self.weather.label(),
                    Style::new().fg(ui::FG).bg(panel::PANEL_BG),
                );
            }
            if inner.height() > 3 {
                surface.print(
                    (inner.left(), inner.top() + 3),
                    &format!(
                        "Day {} {:02}:{:02}",
                        self.day,
                        self.minute / 60,
                        self.minute % 60
                    ),
                    Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
                );
            }
        }

        let inner = panel::Panel::new()
            .title("Mission")
            .border(panel::Border::Double)
            .draw(surface, mission_area);
        if inner.height() > 0 {
            panel::spans(
                surface,
                (inner.left(), inner.top()),
                inner.width(),
                &[panel::Span::plain("Advance")],
                panel::PANEL_BG,
            );
            if inner.height() > 1 {
                panel::spans(
                    surface,
                    (inner.left(), inner.top() + 1),
                    inner.width(),
                    &[panel::Span::dim("VP today: "), panel::Span::keyword("0")],
                    panel::PANEL_BG,
                );
            }
        }

        let inner = panel::Panel::new()
            .title("Zone Info")
            .badge(&tile_label(self.selected))
            .border(panel::Border::Double)
            .draw(surface, zone_area);
        if inner.height() == 0 {
            return;
        }
        let Some(zone) = self.zone(self.selected) else {
            return;
        };
        let mut y = inner.top();
        surface.print(
            (inner.left(), y),
            zone.terrain.name(),
            Style::new().fg(ui::FG).bg(panel::PANEL_BG),
        );
        y += 1;
        if inner.height() > 1 {
            let color = zone.control.tint().unwrap_or(ui::DIM);
            surface.print(
                (inner.left(), y),
                zone.control.label(),
                Style::new().fg(color).bg(panel::PANEL_BG),
            );
            y += 1;
        }
        if inner.height() > 2 {
            surface.print(
                (inner.left(), y),
                &format!("Capture VP: {}", zone.terrain.capture_value()),
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
        }
    }

    fn status(&self) -> String {
        let zone = self.zone(self.selected);
        let terrain = zone.map_or("--", |z| z.terrain.name());
        format!(
            "{}  {terrain}  cmd: {}",
            tile_label(self.selected),
            COMMANDS[self.command]
        )
    }
}

/// The border color for a hex, derived from its control: this is what keeps
/// two adjacent hexes from disagreeing about a shared edge's color, since the
/// edge is drawn once by whichever hex owns it (see the module docs) and its
/// color is a pure function of that one hex's data.
fn edge_color(zone: Option<Zone>) -> Color {
    zone.map_or_else(|| scale(ui::DIM, 0.6), |z| scale(z.terrain.color(), 0.55))
}

/// Converts a world cell to a screen cell inside `area`, given the map's
/// world-space `origin`. Returns `None` if the result falls outside `area`.
fn to_screen(area: Rect, origin: Cell, wx: i32, wy: i32) -> Option<(u16, u16)> {
    let (dx, dy) = (wx - origin.x, wy - origin.y);
    if dx < 0 || dy < 0 || dx >= i32::from(area.width()) || dy >= i32::from(area.height()) {
        return None;
    }
    Some((area.left() + dx as u16, area.top() + dy as u16))
}

/// `A7`-style label for a tile: row letter, column number (1-based).
fn tile_label(tile: Tile) -> String {
    let letter = (b'A' + (tile.row.rem_euclid(26)) as u8) as char;
    format!("{letter}{}", tile.col + 1)
}

/// Below this width the right panel (weather/mission/zone info) is dropped,
/// since it is the least essential of the three columns: the map and the
/// player's own unit matter more than a status readout.
const HIDE_RIGHT_BELOW: u16 = 120;
/// Below this width the left panel (unit card, command menu) is dropped too,
/// leaving only the hex field and its rulers -- still a complete picture of
/// the technique this demo is about.
const HIDE_LEFT_BELOW: u16 = 90;

impl Demo for HexCommand {
    const NAME: &'static str = "19_hex_command";
    const TITLE: &'static str = "19 Hex Command";
    const BLURB: &'static str = "A hex drawn as a multi-cell blob with a real command interface.";
    const GRID: (u16, u16) = (168, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move selection"),
            ("1-4", "command tab"),
            ("Enter", "cycle command"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        self.tick_state(frame.delta.as_secs_f32());

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let show_right = content.width() >= HIDE_RIGHT_BELOW;
        let show_left = content.width() >= HIDE_LEFT_BELOW;

        let (left_area, rest) = if show_left {
            panel::split_left(content, 26)
        } else {
            (
                Rect::new(content.left(), content.top(), 0, content.height()),
                content,
            )
        };
        let (map_area, right_area) = if show_right {
            panel::split_right(rest, 30)
        } else {
            (rest, Rect::new(rest.right(), rest.top(), 0, rest.height()))
        };

        if show_left && left_area.width() > 0 {
            self.draw_left_panel(&mut surface, left_area);
        }
        // The rulers need one cell of margin on the left/bottom of the map
        // area, which `split_left`/`split_right` already provide by not
        // overlapping the panels; when panels are hidden the ruler simply has
        // nowhere to draw the outer label and quietly skips it.
        self.draw_hex_field(&mut surface, map_area);
        if show_right && right_area.width() > 0 {
            self.draw_right_panel(&mut surface, right_area);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(HexCommand);
