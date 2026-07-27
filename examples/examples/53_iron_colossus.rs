//! 53: Iron Colossus -- the Ogre component damage track.
//!
//! Ogre (Steve Jackson Games) puts one enormous cybertank against a swarm of
//! ordinary units. The tank is not a bar of hit points: it is a printed
//! panel of individually targetable parts -- a main battery, a handful of
//! secondary batteries, missile racks, and a row of tread units -- and every
//! one of the game's other systems reads off that panel. Kill enough treads
//! and the Ogre's movement allowance drops a step; kill enough batteries and
//! its attacks per turn drop with them. Nothing else in this gallery derives
//! a unit's *capability* from which named parts of it still exist, which is
//! the one element this demo exists to show.
//!
//! Two supporting elements come along for the ride, both drawn straight from
//! the reference photo of a physical board: the opposing force is cardboard
//! counters printed with attack/defense/movement ratings in the wargame
//! convention (`3/2/2`), read off the counter rather than inferred from a
//! bar; and the hex field itself uses flat board-game terrain colors (green
//! fields, dark forest, blue water, pale rubble) rather than the shaded or
//! textured hex rendering [`38_hex_general`](../38_hex_general) and
//! [`44_dusk_field`](../44_dusk_field) already own. Both stay visually
//! subordinate to the damage track, which is where the eye is meant to land.
//!
//! Techniques on show:
//!
//! - **A capability derived from live component state**
//!   ([`IronColossus::move_allowance`], [`IronColossus::firepower`],
//!   [`IronColossus::attacks`], [`IronColossus::best_range`]): every one of
//!   these is a pure function over which [`Component`]s are still alive, not
//!   a stored counter that combat code has to remember to decrement in two
//!   places. Movement steps down in fixed bands as tread fraction falls
//!   ([`MOVE_BANDS`]) rather than scaling continuously, matching the rule
//!   that a wargame's move allowance is read off a printed table, not eased.
//! - **A component damage track that strikes through as parts die**
//!   ([`draw_track`], [`strike`]): `retroglyph::Style` has no underline or
//!   strikethrough attribute at all (see its own doc comment on why), so a
//!   destroyed component's label is faked by literally overwriting every
//!   other glyph with a box-drawing rule, the cheapest CP437-legal stand-in
//!   for a line through text.
//! - **Printed cardboard counters** ([`draw_counters`]): each enemy token is
//!   a multi-cell block carrying its own `atk/def/mov` rating and a class
//!   letter, exactly as printed on the physical counter in the reference
//!   photo, so strength is read off the counter rather than a color ramp.
//! - **A movement-range overlay that visibly shrinks**
//!   ([`IronColossus::draw_board`]): the highlighted hexes within the Ogre's
//!   current move allowance are recomputed from [`IronColossus::move_allowance`]
//!   every frame, so watching a tread box get struck through and watching the
//!   highlighted ring shrink are the same event seen from two panels.
//! - **Tap-to-arm, button-to-confirm** ([`IronColossus::handle_pointer`]):
//!   a tap on a component box arms it; a separate `Strike` button commits the
//!   kill. That is the confirm step [`ui::touch`] asks for on any
//!   irreversible action, backed up by a `Reset` button (full undo) per the
//!   same module's guidance.
//!
//! ```sh
//! cargo run --example 53_iron_colossus --features crossterm
//! cargo run --example 53_iron_colossus --features software
//! cargo run --example 53_iron_colossus --features gl
//! cargo run --example 53_iron_colossus  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;

use tilekit::geom::{Cell, HexLayout, HexOrientation, Tile};
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb, scale};

/// A component's role. Weapons feed [`IronColossus::firepower`],
/// [`IronColossus::attacks`], and [`IronColossus::best_range`]; treads feed
/// [`IronColossus::move_allowance`]. Nothing else distinguishes them, which
/// is the point: the derived stats are the *only* thing that reads this
/// field, so there is exactly one place damage has to be interpreted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ComponentKind {
    Weapon { attack: i32, range: i32 },
    Tread,
}

/// One targetable part of the Ogre.
#[derive(Clone, Copy)]
struct Component {
    /// Short label, sized to fit a 9-cell-wide box.
    label: &'static str,
    kind: ComponentKind,
    alive: bool,
}

impl Component {
    const fn new(label: &'static str, kind: ComponentKind) -> Self {
        Self {
            label,
            kind,
            alive: true,
        }
    }

    /// The rating line printed under the label, in the wargame convention of
    /// printing what a part does rather than a health fraction.
    const fn rating(self) -> &'static str {
        match self.kind {
            ComponentKind::Weapon {
                attack: 8,
                range: 4,
            } => "A8 R4",
            ComponentKind::Weapon {
                attack: 4,
                range: 2,
            } => "A4 R2",
            ComponentKind::Weapon {
                attack: 3,
                range: 5,
            } => "A3 R5",
            ComponentKind::Tread => "MOVE",
            ComponentKind::Weapon { .. } => "A? R?",
        }
    }

    /// The third line printed in a component's box: what striking this
    /// specific part costs, spelled out rather than left implicit. A
    /// component box that only ever shows a label and a rating has no reason
    /// to be taller than two lines, so this line is what earns the box its
    /// height -- the box grows to fit real information, not the other way
    /// around. Kept to at most 9 characters -- [`draw_track`](IronColossus::draw_track)'s
    /// `MIN_BOX_W` of 11 leaves a 9-cell interior once the panel border on
    /// each side is subtracted, and this is the string that box has to fit
    /// without running off the edge. `FP`/`MOVE` echo the gauge labels
    /// [`IronColossus::draw_capability_readout`] already uses for the same
    /// two stats, so the abbreviation reads as the same quantity, not a new
    /// one.
    const fn effect(self) -> &'static str {
        match self.kind {
            ComponentKind::Weapon { attack: 8, .. } => "-8 FP",
            ComponentKind::Weapon { attack: 4, .. } => "-4 FP",
            ComponentKind::Weapon { attack: 3, .. } => "-3 FP",
            ComponentKind::Tread => "-1/8 MOVE",
            ComponentKind::Weapon { .. } => "-? FP",
        }
    }
}

/// The Ogre's fixed loadout: one main battery, four secondary batteries, two
/// missile racks, and eight tread units. This is the Mark III/IV weapons fit
/// described in the rulebook's own component roster, trimmed to fifteen
/// parts so the whole track reads at a glance rather than requiring a scroll.
const INITIAL_COMPONENTS: [Component; 15] = [
    Component::new(
        "MAIN",
        ComponentKind::Weapon {
            attack: 8,
            range: 4,
        },
    ),
    Component::new(
        "SEC-1",
        ComponentKind::Weapon {
            attack: 4,
            range: 2,
        },
    ),
    Component::new(
        "SEC-2",
        ComponentKind::Weapon {
            attack: 4,
            range: 2,
        },
    ),
    Component::new(
        "SEC-3",
        ComponentKind::Weapon {
            attack: 4,
            range: 2,
        },
    ),
    Component::new(
        "SEC-4",
        ComponentKind::Weapon {
            attack: 4,
            range: 2,
        },
    ),
    Component::new(
        "MSL-1",
        ComponentKind::Weapon {
            attack: 3,
            range: 5,
        },
    ),
    Component::new(
        "MSL-2",
        ComponentKind::Weapon {
            attack: 3,
            range: 5,
        },
    ),
    Component::new("TRD-1", ComponentKind::Tread),
    Component::new("TRD-2", ComponentKind::Tread),
    Component::new("TRD-3", ComponentKind::Tread),
    Component::new("TRD-4", ComponentKind::Tread),
    Component::new("TRD-5", ComponentKind::Tread),
    Component::new("TRD-6", ComponentKind::Tread),
    Component::new("TRD-7", ComponentKind::Tread),
    Component::new("TRD-8", ComponentKind::Tread),
];

/// The Ogre's move allowance in hexes while every tread survives. Picked to
/// match the six-hex move of the Mark V chassis the reference photo's board
/// is scaled for; the exact number matters less than there being room for
/// four visibly distinct bands below it.
const BASE_MOVE: i32 = 6;

/// Movement bands, read off surviving tread fraction low-to-high: `(min
/// fraction alive, move allowance)`. A table rather than a formula because
/// that is how the rulebook actually specifies it -- movement loss is a
/// step function of tread damage, not a smooth percentage -- and because a
/// stepped table is what keeps the readout from tweening, which the brief
/// requires: the giant must visibly *slow in increments*, not glide down a
/// ramp.
const MOVE_BANDS: [(f32, i32); 5] = [
    (1.0, BASE_MOVE),
    (0.75, BASE_MOVE - 1),
    (0.5, BASE_MOVE - 2),
    (0.25, BASE_MOVE - 3),
    (0.0, 1),
];

/// Hex pitch for the board. Kept modest (the board is the demo's supporting
/// element, not its focus) but large enough that a hex's tapered corners
/// actually cover more than one cell: at a 6x3 pitch the taper was too thin
/// to read as anything but a rectangle, especially with no explicit edge
/// glyph. [`IronColossus::draw_board`] also darkens the seam between
/// adjacent hexes so the silhouette shows even where two neighbors happen to
/// share a terrain color.
const HEX_W: i32 = 8;
/// See [`HEX_W`].
const HEX_H: i32 = 4;

const LAYOUT: HexLayout = HexLayout::new(HexOrientation::Pointy, HEX_W, HEX_H);

/// Flat board-game terrain, the palette the reference photo's printed hexes
/// use: saturated flat colors rather than the shaded/textured terrain
/// [`38_hex_general`](../38_hex_general) and
/// [`44_dusk_field`](../44_dusk_field) already draw, which is what keeps this
/// board from reading as a third copy of either.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Field,
    Forest,
    Water,
    Rubble,
}

impl Terrain {
    const fn color(self) -> Color {
        match self {
            Self::Field => rgb(90, 156, 64),
            Self::Forest => rgb(46, 92, 50),
            Self::Water => rgb(52, 96, 176),
            Self::Rubble => rgb(158, 150, 132),
        }
    }

    fn glyph(self, seed: u32, col: i32, row: i32) -> char {
        let h = hash01(seed, col, row);
        match self {
            Self::Field => ' ',
            Self::Forest => {
                if h < 0.5 {
                    '\u{2663}'
                } else {
                    ' '
                }
            }
            Self::Water => {
                if h < 0.45 {
                    '~'
                } else {
                    ' '
                }
            }
            Self::Rubble => {
                if h < 0.4 {
                    '.'
                } else {
                    ' '
                }
            }
        }
    }
}

fn terrain_at(seed: u32, col: i32, row: i32) -> Terrain {
    let n = hash01(seed ^ 0x7a11, col, row);
    if n < 0.1 {
        Terrain::Water
    } else if n < 0.28 {
        Terrain::Forest
    } else if n < 0.36 {
        Terrain::Rubble
    } else {
        Terrain::Field
    }
}

/// A cardboard counter: the small enemy squads the reference photo shows
/// dotted across the board, each printed with its own `atk/def/mov` rating
/// so strength is read straight off the counter, never inferred.
struct Counter {
    offset: (i32, i32),
    glyph: char,
    attack: u8,
    defense: u8,
    movement: u8,
}

/// Fixed offsets from the Ogre's own hex, chosen to ring it at a couple of
/// hex-distances the way a real approach would. Fixed rather than randomly
/// scattered so the board reads identically at every run and every size --
/// counters outside the visible board are simply skipped by
/// [`IronColossus::draw_counters`], which is how this degrades on a small
/// viewport instead of clipping mid-counter.
const COUNTERS: [Counter; 8] = [
    Counter {
        offset: (-3, -1),
        glyph: 'I',
        attack: 3,
        defense: 2,
        movement: 2,
    },
    Counter {
        offset: (-3, 1),
        glyph: 'I',
        attack: 3,
        defense: 2,
        movement: 2,
    },
    Counter {
        offset: (-2, -3),
        glyph: 'A',
        attack: 6,
        defense: 4,
        movement: 6,
    },
    Counter {
        offset: (-2, 3),
        glyph: 'A',
        attack: 6,
        defense: 4,
        movement: 6,
    },
    Counter {
        offset: (4, -2),
        glyph: 'H',
        attack: 4,
        defense: 1,
        movement: 3,
    },
    Counter {
        offset: (4, 2),
        glyph: 'H',
        attack: 4,
        defense: 1,
        movement: 3,
    },
    Counter {
        offset: (-5, 0),
        glyph: 'M',
        attack: 2,
        defense: 6,
        movement: 1,
    },
    Counter {
        offset: (2, 0),
        glyph: 'A',
        attack: 6,
        defense: 4,
        movement: 6,
    },
];

/// What a tap resolves to, once translated by [`Hotspots`].
enum Action {
    Component(usize),
    Strike,
    Reset,
    Prev,
    Next,
}

/// State: the component track, the current selection, and input plumbing.
pub struct IronColossus {
    components: [Component; 15],
    /// Index into `components` currently armed for [`Action::Strike`].
    armed: usize,
    time: f32,
    board_seed: u32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for IronColossus {
    fn default() -> Self {
        Self {
            components: INITIAL_COMPONENTS,
            armed: 0,
            time: 0.0,
            board_seed: 53,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl IronColossus {
    fn treads_total(&self) -> usize {
        self.components
            .iter()
            .filter(|c| c.kind == ComponentKind::Tread)
            .count()
    }

    fn treads_alive(&self) -> usize {
        self.components
            .iter()
            .filter(|c| c.kind == ComponentKind::Tread && c.alive)
            .count()
    }

    /// The Ogre's current move allowance in hexes, stepped down from
    /// [`BASE_MOVE`] by [`MOVE_BANDS`] as treads are destroyed. This is the
    /// function the demo exists to make visible: it has no memory of its
    /// own, so a tread box getting struck through is immediately reflected
    /// here and in [`draw_board`](Self::draw_board)'s shrinking highlight.
    fn move_allowance(&self) -> i32 {
        let total = self.treads_total();
        if total == 0 {
            return BASE_MOVE;
        }
        let alive = self.treads_alive();
        if alive == 0 {
            // Every tread gone: the hull cannot move at all, distinct from
            // the "barely mobile" band above it, which still has some
            // surviving treads to crawl on.
            return 0;
        }
        let frac = alive as f32 / total as f32;
        MOVE_BANDS
            .iter()
            .find(|(min, _)| frac >= *min)
            .map_or(0, |(_, move_)| *move_)
    }

    /// Total attack strength across every surviving weapon component.
    fn firepower(&self) -> i32 {
        self.components
            .iter()
            .filter(|c| c.alive)
            .filter_map(|c| match c.kind {
                ComponentKind::Weapon { attack, .. } => Some(attack),
                ComponentKind::Tread => None,
            })
            .sum()
    }

    /// How many separate attacks the Ogre can make this turn: one per
    /// surviving weapon, matching the rule that each battery fires
    /// independently rather than pooling into a single combined strike.
    fn attacks(&self) -> usize {
        self.components
            .iter()
            .filter(|c| c.alive && matches!(c.kind, ComponentKind::Weapon { .. }))
            .count()
    }

    /// The longest range still available, or 0 once every weapon is dead.
    fn best_range(&self) -> i32 {
        self.components
            .iter()
            .filter(|c| c.alive)
            .filter_map(|c| match c.kind {
                ComponentKind::Weapon { range, .. } => Some(range),
                ComponentKind::Tread => None,
            })
            .max()
            .unwrap_or(0)
    }

    const fn cycle(&mut self, dir: i32) {
        let len = self.components.len();
        self.armed = ((self.armed as i32 + dir).rem_euclid(len as i32)) as usize;
    }

    /// Destroys the armed component, if it is still alive. The button is a
    /// no-op on an already-dead part, so mashing `Strike` cannot double-kill
    /// anything or desync the derived stats from what is actually drawn.
    const fn strike_armed(&mut self) {
        self.components[self.armed].alive = false;
    }

    /// Repairs every component. The one undo this demo needs: destruction is
    /// otherwise irreversible, so a full reset is what
    /// [`ui::touch`](ascii_tile_demos::ui::touch)'s guidance on destructive
    /// actions asks for, in place of a per-component undo stack that would
    /// outgrow its own usefulness on a fifteen-part track.
    const fn reset(&mut self) {
        self.components = INITIAL_COMPONENTS;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Left | KeyCode::Up => self.cycle(-1),
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => self.cycle(1),
                    KeyCode::Enter | KeyCode::Char(' ') => self.strike_armed(),
                    KeyCode::Char('r' | 'R') => self.reset(),
                    _ => {}
                }
            }
        }
        true
    }

    fn handle_pointer(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos) {
            Some(Action::Component(idx)) => self.armed = *idx,
            Some(Action::Strike) => self.strike_armed(),
            Some(Action::Reset) => self.reset(),
            Some(Action::Prev) => self.cycle(-1),
            Some(Action::Next) => self.cycle(1),
            None => {}
        }
    }

    /// Draws the hex board: terrain sized to fill `area` exactly (columns and
    /// rows are derived from the live rect, never a fixed constant), the
    /// Ogre's own hex, its current move-allowance ring, and the cardboard
    /// counters ringed around it.
    fn draw_board(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 2 || area.height() < 2 {
            return;
        }
        let cols = i32::from(area.width()) / HEX_W + 2;
        let rows = i32::from(area.height()) / HEX_H + 2;
        let ogre_tile = Tile::new(cols / 2, rows / 2);
        let ogre_cell = LAYOUT.center_cell(ogre_tile);
        // Center the generated board on the Ogre's own hex so the field fills
        // the panel symmetrically instead of running off one edge.
        let origin = Cell::new(
            ogre_cell.x - i32::from(area.width()) / 2,
            ogre_cell.y - i32::from(area.height()) / 2,
        );
        let move_hexes = self.move_allowance();

        for sy in area.top()..area.bottom() {
            for sx in area.left()..area.right() {
                let wx = origin.x + i32::from(sx - area.left());
                let wy = origin.y + i32::from(sy - area.top());
                let tile = LAYOUT.cell_to_tile(Cell::new(wx, wy));
                if tile.col < 0 || tile.row < 0 || tile.col >= cols || tile.row >= rows {
                    surface.put((sx, sy), ' ', Style::new().bg(ui::BG));
                    continue;
                }
                let terrain = terrain_at(self.board_seed, tile.col, tile.row);
                let mut bg = terrain.color();
                // Darken the seam where this cell's hex differs from its left
                // or upper neighbor. At this cell pitch nothing else marks a
                // hex boundary -- two adjacent hexes of the same terrain would
                // otherwise fuse into one undifferentiated rectangle -- so this
                // traces every hex's actual tapered silhouette instead of only
                // showing a boundary where terrain color happens to change too.
                let left_tile = LAYOUT.cell_to_tile(Cell::new(wx - 1, wy));
                let up_tile = LAYOUT.cell_to_tile(Cell::new(wx, wy - 1));
                if left_tile != tile || up_tile != tile {
                    bg = scale(bg, 0.72);
                }
                let in_range = LAYOUT.distance(tile, ogre_tile) <= move_hexes;
                if in_range {
                    // A flat lighten rather than a border line: the ring has
                    // to read at hex-scrap sizes as small as 6x3 cells, where
                    // a one-cell border would vanish into the terrain glyphs.
                    bg = mix(bg, rgb(255, 236, 170), 0.28);
                }
                let center = LAYOUT.center_cell(tile);
                let glyph = terrain.glyph(self.board_seed, wx - center.x, wy - center.y);
                let fg = scale(terrain.color(), 0.6);
                surface.put((sx, sy), glyph, Style::new().fg(fg).bg(bg));
            }
        }

        Self::draw_counters(surface, area, origin, ogre_tile);
        self.draw_ogre_token(surface, area, origin, ogre_tile);
    }

    fn draw_counters(surface: &mut Surface<'_>, area: Rect, origin: Cell, ogre_tile: Tile) {
        for counter in &COUNTERS {
            let tile = Tile::new(
                ogre_tile.col + counter.offset.0,
                ogre_tile.row + counter.offset.1,
            );
            let center = LAYOUT.center_cell(tile);
            let Some((sx, sy)) = to_screen(area, origin, center.x, center.y) else {
                continue;
            };
            let bg = rgb(224, 214, 186);
            let fg = rgb(24, 22, 18);
            surface.put((sx, sy), counter.glyph, Style::new().fg(fg).bg(bg));
            // The printed rating, `atk/def/mov`, directly under the symbol:
            // the wargame convention the reference photo's counters use, so
            // strength is read off the chit rather than inferred from color.
            if sy + 1 < area.bottom() {
                let rating = format!("{}{}{}", counter.attack, counter.defense, counter.movement);
                let start = sx.saturating_sub(1);
                for (i, ch) in rating.chars().enumerate() {
                    let x = start + i as u16;
                    if x < area.right() {
                        surface.put((x, sy + 1), ch, Style::new().fg(fg).bg(bg));
                    }
                }
            }
        }
    }

    /// Draws the Ogre's own multi-cell token: a hull glyph flanked by a
    /// tread-flicker column on each side, so the one visible piece of "is it
    /// still able to move" feedback on the *board* (as opposed to the track)
    /// is a live animation, not a static badge.
    fn draw_ogre_token(&self, surface: &mut Surface<'_>, area: Rect, origin: Cell, tile: Tile) {
        let center = LAYOUT.center_cell(tile);
        let Some((sx, sy)) = to_screen(area, origin, center.x, center.y) else {
            return;
        };
        let hull = rgb(210, 70, 60);
        surface.put(
            (sx, sy),
            '\u{25B2}',
            Style::new().fg(rgb(20, 8, 8)).bg(hull),
        );

        // Tread flicker alternates glyph on a fixed period scaled by how many
        // treads still work: a fully mobile Ogre flickers briskly, an
        // immobilized one (move allowance 0) holds still, which is a second,
        // independent read of the same derived state as the track and the
        // highlight ring.
        if self.move_allowance() > 0 {
            let period = (self.move_allowance() as f32).mul_add(0.4, 1.0);
            let flip = (self.time * period).fract() < 0.5;
            let tread = if flip { '=' } else { '-' };
            if sx > area.left() {
                surface.put(
                    (sx - 1, sy),
                    tread,
                    Style::new().fg(rgb(90, 84, 80)).bg(hull),
                );
            }
            if sx + 1 < area.right() {
                surface.put(
                    (sx + 1, sy),
                    tread,
                    Style::new().fg(rgb(90, 84, 80)).bg(hull),
                );
            }
        }
    }

    /// Draws the vitals strip: the four derived stats, each a plain number
    /// pinned to the grid (never eased) that changes in the same frame a
    /// component gets struck through.
    fn draw_vitals(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(panel::PANEL_BG));
        let text = format!(
            "MOVE {}   FIREPOWER {}   ATTACKS {}   RANGE {}",
            self.move_allowance(),
            self.firepower(),
            self.attacks(),
            self.best_range()
        );
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width(),
            &[Span::keyword(&text)],
            panel::PANEL_BG,
        );
    }

    /// Draws the component grid: one box per component, arranged in as many
    /// columns as fit `area`, then hands whatever height the grid does not
    /// need to [`Self::draw_capability_readout`] rather than stretching the
    /// boxes to consume it. A box is sized to its own content plus a little
    /// padding; growing it to match leftover panel height was what put the
    /// dead space *inside* every box instead of below the grid.
    fn draw_track(&mut self, surface: &mut Surface<'_>, area: Rect) {
        const MIN_BOX_W: u16 = 11;
        // Border top + label + rating + effect line + one padding row +
        // border bottom: exactly what three lines of text need, not a
        // fraction of the panel.
        const BOX_H: u16 = 6;
        surface.fill_rect(area, ' ', Style::new().bg(panel::PANEL_BG));
        if area.width() < MIN_BOX_W || area.height() < BOX_H {
            return;
        }
        let len = self.components.len() as u16;
        let columns = (area.width() / (MIN_BOX_W + 1)).max(1).min(len);
        let rows = len.div_ceil(columns);

        let pitch_w = (area.width() / columns).max(MIN_BOX_W);
        let pitch_h = BOX_H + 1;
        let box_w = pitch_w.saturating_sub(1).max(MIN_BOX_W);
        let box_h = BOX_H;

        for (idx, component) in self.components.iter().enumerate() {
            let col = idx as u16 % columns;
            let row = idx as u16 / columns;
            let x = area.left() + col * pitch_w;
            let y = area.top() + row * pitch_h;
            if y + box_h > area.bottom() || x + box_w > area.right() {
                // Ran out of room: later components stay off screen rather
                // than overlapping the last visible row.
                continue;
            }
            let rect = Rect::new(x, y, box_w, box_h);
            self.hotspots
                .push_tappable(rect, area, Action::Component(idx));
            draw_component_box(surface, rect, component, idx == self.armed, self.time);
        }

        let grid_h = rows * pitch_h;
        if area.height() > grid_h {
            let readout_area = Rect::new(
                area.left(),
                area.top() + grid_h,
                area.width(),
                area.height() - grid_h,
            );
            self.draw_capability_readout(surface, readout_area);
        }
    }

    /// Spends whatever panel height the component grid does not need on a
    /// readout of the four derived stats plus the move-band table that
    /// governs one of them, instead of leaving it as background below the
    /// last row of boxes. This is the same numbers [`Self::draw_vitals`]
    /// already prints on one line at the top of the panel, laid out here
    /// with enough real rows -- four gauges, a labeled band table, a
    /// summary line -- that spacing them out across a tall panel never
    /// degenerates into one blank slab the way stretching two lines of
    /// label/rating across a box did.
    fn draw_capability_readout(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() < 3 || area.width() < 24 {
            return;
        }
        let inner = Panel::new()
            .title("Capability Readout")
            .border(Border::Single)
            .frame(ui::DIM)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }

        let gauges: [(&str, i32, i32); 4] = [
            ("MOVE", self.move_allowance(), BASE_MOVE),
            ("FIREPOWER", self.firepower(), max_firepower()),
            ("ATTACKS", self.attacks() as i32, max_attacks()),
            ("RANGE", self.best_range(), max_range()),
        ];
        // Real content: four gauge rows, a bands header, one row per move
        // band, and a summary line -- eleven rows regardless of panel size.
        // Whatever height is left over is spent as breathing room around
        // those three sections (a top margin, a gap before the band table,
        // a gap before the summary line) rather than as one dead chunk
        // under the last row: the mistake that stretched two lines of
        // label/rating across a whole box is not fixed by moving the same
        // mistake down here.
        let content_rows = (2 * gauges.len() - 1 + 1 + MOVE_BANDS.len() + 1) as u16;
        let pad = inner.height().saturating_sub(content_rows) / 4;

        let mut y = inner.top() + pad;
        y = draw_gauge_rows(surface, inner, &gauges, y);

        y += pad;
        if y < inner.bottom() {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::new(
                    "MOVE BANDS  (tread fraction alive -> hexes)",
                    ui::DIM,
                )],
                panel::PANEL_BG,
            );
            y += 1;
        }

        let treads_total = self.treads_total();
        let frac = if treads_total == 0 {
            1.0
        } else {
            self.treads_alive() as f32 / treads_total as f32
        };
        y = draw_band_rows(surface, inner, frac, y);

        y += pad;
        if y < inner.bottom() {
            let alive = self.components.iter().filter(|c| c.alive).count();
            let summary = format!(
                "{alive}/{} components alive   {}/{treads_total} treads alive",
                self.components.len(),
                self.treads_alive(),
            );
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::plain(&summary)],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_buttons(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(ui::CHROME_BG));
        let armed = &self.components[self.armed];
        let labels: [(&str, Action); 4] = [
            ("Prev", Action::Prev),
            (
                if armed.alive { "Strike" } else { "Struck" },
                Action::Strike,
            ),
            ("Next", Action::Next),
            ("Reset", Action::Reset),
        ];
        let cols = panel::columns(area, labels.len() as u16, 1);
        for ((label, action), rect) in labels.into_iter().zip(cols) {
            let tap_rect = ui::touch::tappable(rect, area);
            self.hotspots.push(tap_rect, action);
            let bg = rgb(30, 32, 42);
            surface.fill_rect(rect, ' ', Style::new().bg(bg));
            let style = Style::new().fg(ui::ACCENT).bg(bg);
            let cx = rect.left() + rect.width().saturating_sub(label.chars().count() as u16) / 2;
            let cy = rect.top() + rect.height() / 2;
            if rect.width() > 0 && rect.height() > 0 {
                surface.print((cx, cy), label, style);
            }
        }
    }

    fn status(&self) -> String {
        let armed = &self.components[self.armed];
        let state = if armed.alive { "armed" } else { "destroyed" };
        format!(
            "{} {state}  treads {}/{}",
            armed.label,
            self.treads_alive(),
            self.treads_total()
        )
    }
}

/// Overwrites every other non-space character with a box-drawing rule.
///
/// `retroglyph_core::Style` has no strikethrough attribute (it deliberately
/// exposes only foreground/background color, per its own module doc), so
/// there is no combining-glyph or text-modifier route to a literal line
/// through text here. This alternating overwrite is the cheapest CP437-only
/// stand-in that still reads unambiguously as "crossed out" rather than as
/// corrupted text, because it leaves half the original label legible.
fn strike(text: &str) -> String {
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            if ch != ' ' && i % 2 == 1 {
                '\u{2500}'
            } else {
                ch
            }
        })
        .collect()
}

fn draw_component_box(
    surface: &mut Surface<'_>,
    rect: Rect,
    component: &Component,
    armed: bool,
    time: f32,
) {
    let alive_color = match component.kind {
        ComponentKind::Weapon { .. } => rgb(92, 140, 214),
        ComponentKind::Tread => rgb(196, 162, 74),
    };
    let dead_color = rgb(120, 60, 56);
    let frame = if component.alive {
        alive_color
    } else {
        dead_color
    };
    let frame = if armed {
        // Armed but not yet struck: pulse the frame so the selection reads
        // even on a monochrome-ish terrain palette. The pulse drives only
        // color, never the printed label, so it never risks looking like a
        // change in the component's own state.
        let pulse = 0.5f32.mul_add((time * 5.0).sin(), 0.5);
        mix(frame, rgb(255, 236, 170), 0.5 * pulse)
    } else {
        frame
    };

    let panel = Panel::new().border(Border::Single).frame(frame);
    let inner = panel.draw(surface, rect);
    if inner.height() == 0 {
        return;
    }

    let (label, rating, effect) = if component.alive {
        (
            component.label.to_string(),
            component.rating().to_string(),
            component.effect().to_string(),
        )
    } else {
        (
            strike(component.label),
            strike(component.rating()),
            strike(component.effect()),
        )
    };
    let text_color = if component.alive { ui::FG } else { dead_color };
    panel::spans(
        surface,
        (inner.left(), inner.top()),
        inner.width(),
        &[Span::new(&label, text_color)],
        panel::PANEL_BG,
    );
    if inner.height() > 1 {
        panel::spans(
            surface,
            (inner.left(), inner.top() + 1),
            inner.width(),
            &[Span::new(&rating, ui::DIM)],
            panel::PANEL_BG,
        );
    }
    // The cost line: what a strike on this specific part actually takes
    // away, spelled out rather than left for the vitals strip to imply.
    // This is the line that earns the box its extra row of height.
    if inner.height() > 2 {
        panel::spans(
            surface,
            (inner.left(), inner.top() + 2),
            inner.width(),
            &[Span::new(&effect, ui::DIM)],
            panel::PANEL_BG,
        );
    }
}

/// Total attack strength across every weapon in the undamaged loadout, the
/// denominator [`IronColossus::draw_capability_readout`] draws the firepower
/// gauge against.
fn max_firepower() -> i32 {
    INITIAL_COMPONENTS
        .iter()
        .filter_map(|c| match c.kind {
            ComponentKind::Weapon { attack, .. } => Some(attack),
            ComponentKind::Tread => None,
        })
        .sum()
}

/// How many weapons the undamaged loadout carries, the denominator for the
/// attacks gauge.
fn max_attacks() -> i32 {
    INITIAL_COMPONENTS
        .iter()
        .filter(|c| matches!(c.kind, ComponentKind::Weapon { .. }))
        .count() as i32
}

/// The longest range any weapon in the undamaged loadout carries, the
/// denominator for the range gauge.
fn max_range() -> i32 {
    INITIAL_COMPONENTS
        .iter()
        .filter_map(|c| match c.kind {
            ComponentKind::Weapon { range, .. } => Some(range),
            ComponentKind::Tread => None,
        })
        .max()
        .unwrap_or(0)
}

/// Draws one row per `(label, value, max)` gauge starting at screen row `y`.
/// Returns the next free row, so a caller drawing several sections in
/// sequence never has to recompute where the previous one stopped.
fn draw_gauge_rows(
    surface: &mut Surface<'_>,
    inner: Rect,
    gauges: &[(&str, i32, i32)],
    mut y: u16,
) -> u16 {
    const LABEL_W: u16 = 11;
    const VALUE_W: u16 = 5;
    let bar_w = inner.width().saturating_sub(LABEL_W + VALUE_W + 1);

    for &(label, value, max) in gauges {
        if y >= inner.bottom() {
            break;
        }
        panel::spans(
            surface,
            (inner.left(), y),
            LABEL_W,
            &[Span::new(label, ui::DIM)],
            panel::PANEL_BG,
        );
        let t = if max > 0 {
            value as f32 / max as f32
        } else {
            0.0
        };
        if bar_w > 0 {
            panel::bar(
                surface,
                (inner.left() + LABEL_W, y),
                bar_w,
                t,
                panel::threshold(t),
                rgb(40, 42, 54),
            );
        }
        let value_text = value.to_string();
        panel::spans(
            surface,
            (inner.left() + LABEL_W + bar_w + 1, y),
            VALUE_W,
            &[Span::keyword(&value_text)],
            panel::PANEL_BG,
        );
        // A blank row between gauges: four bars near their maximum drawn
        // flush against each other read as one undifferentiated slab
        // rather than four gauges, and there is room to spare here.
        y += 2;
    }
    y
}

/// Draws one row per entry in [`MOVE_BANDS`] starting at screen row `y`,
/// highlighting whichever band `frac` (surviving tread fraction) currently
/// falls in. Returns the next free row, same convention as
/// [`draw_gauge_rows`].
fn draw_band_rows(surface: &mut Surface<'_>, inner: Rect, frac: f32, mut y: u16) -> u16 {
    let active_min = MOVE_BANDS
        .iter()
        .find(|(min, _)| frac >= *min)
        .map(|(min, _)| *min);
    for (min_frac, move_) in MOVE_BANDS {
        if y >= inner.bottom() {
            break;
        }
        let active = active_min == Some(min_frac);
        let marker = if active { '\u{25BA}' } else { ' ' };
        let text = format!(
            "{marker} {:>3.0}%+ alive -> {move_} hexes",
            min_frac * 100.0
        );
        let color = if active { ui::ACCENT } else { ui::DIM };
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::new(&text, color)],
            panel::PANEL_BG,
        );
        y += 1;
    }
    y
}

/// Converts a world cell to a screen cell inside `area`, given `origin`.
fn to_screen(area: Rect, origin: Cell, wx: i32, wy: i32) -> Option<(u16, u16)> {
    let (dx, dy) = (wx - origin.x, wy - origin.y);
    if dx < 0 || dy < 0 || dx >= i32::from(area.width()) || dy >= i32::from(area.height()) {
        return None;
    }
    Some((area.left() + dx as u16, area.top() + dy as u16))
}

impl Demo for IronColossus {
    const NAME: &'static str = "53_iron_colossus";
    const TITLE: &'static str = "Iron Colossus";
    const BLURB: &'static str =
        "Ogre: numbered counters against one giant unit with a component track.";
    const GRID: (u16, u16) = (158, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("\u{2190}/\u{2192}", "select part"),
            ("Enter", "strike armed part"),
            ("R", "repair all"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        self.handle_pointer();

        let area = term.area();
        let mut surface = term.surface();
        ui::fill(&mut surface, area, Style::new().bg(ui::BG));

        let (title, content, status) = ui::split_chrome(area);
        ui::title_bar::<Self>(&mut surface, title);

        self.hotspots.clear();
        let shape = Shape::of(content);

        let (board_area, track_outer) = if shape.stacks() {
            let board_h = (content.height() * 32 / 100).max(8);
            panel::split_top(content, board_h)
        } else {
            let board_w = (content.width() * 38 / 100).clamp(20, 60);
            panel::split_left(content, board_w)
        };

        let board_inner = Panel::new()
            .title("Battlefield")
            .border(Border::Double)
            .frame(ui::DIM)
            .draw(&mut surface, board_area);
        self.draw_board(&mut surface, board_inner);

        let track_panel_inner = Panel::new()
            .title("Component Damage Track")
            .border(Border::Double)
            .frame(rgb(196, 162, 74))
            .draw(&mut surface, track_outer);

        let button_h = 4u16.min(track_panel_inner.height());
        let (rest, button_area) = panel::split_bottom(track_panel_inner, button_h);
        let (vitals_area, grid_area) = panel::split_top(rest, 1);

        self.draw_vitals(&mut surface, vitals_area);
        self.draw_track(&mut surface, grid_area);
        self.draw_buttons(&mut surface, button_area);

        ui::status_bar::<Self>(&mut surface, status, &self.status(), &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(IronColossus);

#[cfg(test)]
mod tests {
    use super::{BASE_MOVE, INITIAL_COMPONENTS, IronColossus};

    /// The one property this demo exists to guarantee: destroying tread
    /// components must lower the Ogre's derived move allowance, and by
    /// enough steps to be visibly different, not by a fraction of a hex.
    #[test]
    fn destroying_treads_reduces_move_allowance() {
        let mut ogre = IronColossus::default();
        assert_eq!(ogre.move_allowance(), BASE_MOVE);

        // Kill every tread on one side (four of eight): fraction alive drops
        // to 0.5, which the move-band table steps down by two.
        for idx in 7..11 {
            ogre.components[idx].alive = false;
        }
        assert_eq!(ogre.treads_alive(), 4);
        let mid_move = ogre.move_allowance();
        assert!(
            mid_move < BASE_MOVE,
            "losing half the treads must reduce move, got {mid_move}"
        );

        // Kill the rest: fully immobilized.
        for idx in 11..15 {
            ogre.components[idx].alive = false;
        }
        assert_eq!(ogre.treads_alive(), 0);
        assert_eq!(ogre.move_allowance(), 0);
        assert!(ogre.move_allowance() < mid_move);
    }

    #[test]
    fn firepower_and_attacks_drop_when_weapons_die() {
        let mut ogre = IronColossus::default();
        let full_fire = ogre.firepower();
        let full_attacks = ogre.attacks();
        assert!(full_fire > 0);
        assert!(full_attacks > 0);

        // The main battery is index 0 in `INITIAL_COMPONENTS`.
        ogre.components[0].alive = false;
        assert!(ogre.firepower() < full_fire);
        assert_eq!(ogre.attacks(), full_attacks - 1);
    }

    #[test]
    fn reset_restores_every_component() {
        let mut ogre = IronColossus::default();
        for c in &mut ogre.components {
            c.alive = false;
        }
        ogre.reset();
        assert!(ogre.components.iter().all(|c| c.alive));
        assert_eq!(ogre.move_allowance(), BASE_MOVE);
        assert_eq!(ogre.components.len(), INITIAL_COMPONENTS.len());
    }

    #[test]
    fn best_range_falls_back_to_zero_once_disarmed() {
        let mut ogre = IronColossus::default();
        assert!(ogre.best_range() > 0);
        for c in &mut ogre.components {
            if matches!(c.kind, super::ComponentKind::Weapon { .. }) {
                c.alive = false;
            }
        }
        assert_eq!(ogre.best_range(), 0);
        assert_eq!(ogre.attacks(), 0);
        assert_eq!(ogre.firepower(), 0);
    }
}
