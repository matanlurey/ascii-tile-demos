//! 43: Bone Lord -- a hand-drawn dungeon plan over a ranked minion bench.
//!
//! Adapted from Iratus: Lord of the Dead. The reference screenshot is not a
//! battle: it is the campaign map between fights, a route inked on creased
//! parchment inside a gothic frame, with a strip of sixteen minion portraits
//! running along the bottom in four labelled squads. This demo lands that
//! pairing -- route-planning above, roster below -- on a character grid: a
//! floor plan of rooms and corridors you tap through, over a bench you can
//! rearrange between fights.
//!
//! Techniques on show:
//!
//! - **An authored floor plan, not a generated one** ([`Dungeon::build`]):
//!   unlike the BSP deck in `21_deck_plan`, this map is a fixed hand-placed
//!   graph of rooms in a snake path, because a campaign map is a designed
//!   route the player reads end to end, not a randomized space to explore.
//!   Corridor plates are filled directly between authored-adjacent rooms,
//!   and walls fall out of floor adjacency the same way `21_deck_plan`
//!   derives them, autotiled with [`tilekit::autotile::BOX_DOUBLE`] for the
//!   "double-ruled ink line" the brief asks for.
//! - **Tap-select-then-tap-target on a dense board**
//!   ([`BoneLord::handle_tap`]): a room is first selected (revealing what
//!   waits there), and a second tap on the same, still-reachable room
//!   advances into it. This is the touch module's recommended pattern for
//!   boards too dense for a finger to drag precisely on, used here for both
//!   the map and the roster.
//! - **Both tap and drag for the same action** ([`BoneLord::handle_swap`]):
//!   two minion slots swap either by tap-select-then-tap-target, exactly as
//!   the map does, or by a direct drag from one slot to another -- the two
//!   input paths [`ui::touch`] asks every demo to support side by side.
//! - **A grown hotspot over a small drawn control**
//!   ([`ui::touch::Hotspots::push_tappable`]): rooms are drawn at their true
//!   plate size (as small as 4x3 cells) but hit-test against a touch target
//!   grown to [`ui::touch::TAP_W`]x[`ui::touch::TAP_H`], so the map can stay
//!   dense without any room becoming unreliable to tap on a phone.
//! - **Scrolling a strip instead of shrinking its tiles**
//!   ([`BoneLord::roster_scroll_max`]): sixteen 9-wide minion busts do not
//!   fit in 80 columns, and the brief is explicit that busts must not shrink
//!   below tap size to make them fit. The strip instead scrolls horizontally,
//!   dragged directly or snapped to a squad when its numeral is tapped.
//! - **[`ui::touch::Shape`]-driven reflow**: portrait splits the sixteen
//!   busts into two rows of eight so the map keeps a usable height; landscape
//!   keeps one row of sixteen and gives the map whatever rows are left over.
//! - **Idle animation confined to decoration**: a warm gradient drifts across
//!   the parchment as if lit by candle flame, embers drift and fade over a
//!   stable hashed field (the same technique `21_deck_plan` uses for its
//!   starfield), and reachable rooms pulse gently. Minion portraits blink on
//!   a per-minion hashed phase. Every number on screen (levels, HP percentage,
//!   gold, mana, material counts) is pinned to the grid and never animates,
//!   per the addendum's warning that a jittering label reads as a bug.
//!
//! ```sh
//! cargo run --example 43_bone_lord --features crossterm
//! cargo run --example 43_bone_lord --features software
//! cargo run --example 43_bone_lord --features gl
//! cargo run --example 43_bone_lord  # headless, prints a few frames
//! ```

use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_DOUBLE, mask4};
use tilekit::noise::hash01;
use tilekit::palette::{PARCHMENT, mix, rgb, scale};

/// Base (scale-1) width of the plate grid the dungeon is authored in, in
/// plate units. Deliberately small (a hallway-and-five-rooms-per-row snake)
/// rather than the sprawling BSP deck `21_deck_plan` builds: a campaign map
/// is read whole at a glance, not scrolled through. The 80x24 headless grid
/// draws this at `scale == 1`; a larger viewport draws it at a larger
/// integer `scale` (see [`Dungeon::build`]) so the plan fills the parchment
/// instead of sitting pinned to its top-left corner.
const BASE_MAP_W: i32 = 30;
/// See [`BASE_MAP_W`].
const BASE_MAP_H: i32 = 11;

/// Base (scale-1) footprint of one room, in plates. Uniform on purpose: the
/// boss room reads as distinct through color, icon, and a heavier pulse, not
/// through being physically bigger, which would have broken the snake's
/// straight corridors.
const BASE_ROOM_W: i32 = 4;
/// See [`BASE_ROOM_W`].
const BASE_ROOM_H: i32 = 3;

/// Number of squads and minions per squad. Matches the reference screenshot's
/// four labelled groups of four busts.
const SQUAD_COUNT: usize = 4;
/// See [`SQUAD_COUNT`].
const SQUAD_SIZE: usize = 4;
/// Total minions on the bench.
const MINION_COUNT: usize = SQUAD_COUNT * SQUAD_SIZE;

/// Width of one minion portrait slot, exactly [`TAP_W`]. Going smaller would
/// make a bust untappable on a phone; the brief is explicit that the fix for
/// sixteen not fitting is a scrolling strip, not a shrunk bust.
const BUST_W: u16 = TAP_W;
/// Height of one minion portrait slot: a top and bottom border, two rows of
/// bust art, a level line, and an HP bar.
const BUST_H: u16 = 6;
/// Width of the ornate divider column between squads, carrying the roman
/// numeral. Also grown to a legal tap target since tapping it switches squads.
const DIVIDER_W: u16 = 3;

/// Rows the top resource band claims: one line of material counters, one of
/// gold and mana.
const RESOURCE_H: u16 = 2;

/// A room's purpose, each with its own icon and tint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoomKind {
    Start,
    Fight,
    Loot,
    Event,
    Boss,
}

impl RoomKind {
    /// The glyph drawn at a room's centre. All CP437: `@` for the standing
    /// camp banner, `X` for crossed blades, `$` for a coin spilling from a
    /// chest, `!` for an omen banner, and `\u{03A9}` (Greek omega, in the
    /// gallery's allowed punctuation set) standing in for a crown, since a
    /// real crown glyph is outside CP437.
    const fn glyph(self) -> char {
        match self {
            Self::Start => '@',
            Self::Fight => 'X',
            Self::Loot => '$',
            Self::Event => '!',
            Self::Boss => '\u{03A9}',
        }
    }

    const fn tint(self) -> Color {
        match self {
            Self::Start => rgb(146, 176, 150),
            Self::Fight => rgb(198, 96, 88),
            Self::Loot => rgb(214, 176, 96),
            Self::Event => rgb(140, 150, 210),
            Self::Boss => rgb(190, 110, 210),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Start => "Camp",
            Self::Fight => "Fight",
            Self::Loot => "Loot",
            Self::Event => "Event",
            Self::Boss => "Boss",
        }
    }

    /// Every kind, for the legend column.
    const ALL: [Self; 5] = [
        Self::Start,
        Self::Fight,
        Self::Loot,
        Self::Event,
        Self::Boss,
    ];
}

/// One room on the plan.
struct Room {
    rect: Rect,
    kind: RoomKind,
    label: &'static str,
}

/// One plate of the dungeon plan.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Plate {
    Void,
    Floor,
    Wall,
}

/// The authored dungeon: a fixed snake of thirteen rooms across a 5x3 grid of
/// plate-space cells, linked by straight corridors, with walls derived from
/// floor adjacency exactly as `21_deck_plan` derives them.
///
/// Built at an integer `scale` (see [`Dungeon::build`]) so the same snake can
/// be rasterized larger on a roomier viewport: room footprints and the grid
/// of column/row anchors they sit on both grow by `scale`, so the plan's
/// overall footprint ([`Self::map_w`] x [`Self::map_h`]) grows with it
/// rather than staying pinned to a fixed plate extent regardless of screen
/// size, which was the fixed-top-left-corner defect this replaces.
struct Dungeon {
    plates: Vec<Plate>,
    rooms: Vec<Room>,
    /// Consecutive-index edges: `edges[i]` connects `rooms[i]` and
    /// `rooms[i + 1]`. A plain chain rather than a general graph because the
    /// campaign route in the reference is read start to end, not branched.
    edges: usize,
    /// This dungeon's plate-grid width at the scale it was built with.
    map_w: i32,
    /// This dungeon's plate-grid height at the scale it was built with.
    map_h: i32,
}

impl Dungeon {
    const fn index_in(map_w: i32, map_h: i32, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= map_w || y >= map_h {
            return None;
        }
        Some((y * map_w + x) as usize)
    }

    const fn index(&self, x: i32, y: i32) -> Option<usize> {
        Self::index_in(self.map_w, self.map_h, x, y)
    }

    fn plate(&self, x: i32, y: i32) -> Plate {
        self.index(x, y).map_or(Plate::Void, |i| self.plates[i])
    }

    /// Builds the snake at `scale` (1 or greater): five columns
    /// (`x = 2, 8, 14, 20, 26`, times `scale`) at three rows (`y = 8, 4, 0`,
    /// times `scale`, bottom to top), visited bottom row left-to-right, mid
    /// row right-to-left, top row left-to-right -- a boustrophedon path,
    /// which is what keeps every consecutive pair of rooms cardinally
    /// adjacent (sharing either an x or a y) without any diagonal jump a
    /// straight corridor could not reach. Every anchor and room footprint
    /// scales together, so a bigger `scale` reproduces the exact same
    /// silhouette, just larger, rather than spreading rooms out with no
    /// larger corridors or bigger room boxes to fill the gained space.
    fn build(scale: i32) -> Self {
        let scale = scale.max(1);
        let map_w = BASE_MAP_W * scale;
        let map_h = BASE_MAP_H * scale;
        let room_w = BASE_ROOM_W * scale;
        let room_h = BASE_ROOM_H * scale;

        // Row spacing leaves a one-plate (pre-scale) gap between bands
        // (`BASE_ROOM_H` is 3, and consecutive base rows are 4 apart), so
        // vertical corridors have a real, nonzero-length strip to draw
        // rather than two rooms simply touching with no wall or corridor
        // between them. Scaling the anchors and the room size by the same
        // factor keeps that gap (and therefore the corridor) proportional.
        let cols: [i32; 5] = [2 * scale, 8 * scale, 14 * scale, 20 * scale, 26 * scale];
        let rows: [i32; 3] = [8 * scale, 4 * scale, 0];

        let plan: [(usize, usize, RoomKind, &str); 13] = [
            (0, 0, RoomKind::Start, "Camp"),
            (1, 0, RoomKind::Fight, "Crypt"),
            (2, 0, RoomKind::Fight, "Hollow"),
            (3, 0, RoomKind::Loot, "Vault"),
            (4, 0, RoomKind::Fight, "Ossuary"),
            (4, 1, RoomKind::Event, "Shrine"),
            (3, 1, RoomKind::Fight, "Warren"),
            (2, 1, RoomKind::Loot, "Cache"),
            (1, 1, RoomKind::Fight, "Gallery"),
            (0, 1, RoomKind::Event, "Chapel"),
            (0, 2, RoomKind::Fight, "Cairn"),
            (1, 2, RoomKind::Loot, "Relics"),
            (2, 2, RoomKind::Boss, "Throne"),
        ];

        let rooms: Vec<Room> = plan
            .iter()
            .map(|&(col, row, kind, label)| Room {
                rect: Rect::new(
                    cols[col] as u16,
                    rows[row] as u16,
                    room_w as u16,
                    room_h as u16,
                ),
                kind,
                label,
            })
            .collect();

        let mut plates = vec![Plate::Void; (map_w * map_h) as usize];
        for room in &rooms {
            fill_rect(&mut plates, map_w, map_h, room.rect);
        }
        for pair in rooms.windows(2) {
            let corridor = straight_corridor(pair[0].rect, pair[1].rect);
            fill_rect(&mut plates, map_w, map_h, corridor);
        }

        // A plate is a wall iff it is not floor and touches floor on at least
        // one cardinal side; everything else stays void, drawn as bare
        // parchment. See `21_deck_plan::Deck::generate` for why walls are
        // derived rather than stamped: it is what lets two rooms sharing an
        // edge share one wall instead of drawing it twice.
        for y in 0..map_h {
            for x in 0..map_w {
                let Some(i) = Self::index_in(map_w, map_h, x, y) else {
                    continue;
                };
                if plates[i] == Plate::Floor {
                    continue;
                }
                let touches_floor = [(0, -1), (1, 0), (0, 1), (-1, 0)].iter().any(|&(dx, dy)| {
                    matches!(
                        Self::index_in(map_w, map_h, x + dx, y + dy).map(|j| plates[j]),
                        Some(Plate::Floor)
                    )
                });
                if touches_floor {
                    plates[i] = Plate::Wall;
                }
            }
        }

        let edges = rooms.len().saturating_sub(1);
        Self {
            plates,
            rooms,
            edges,
            map_w,
            map_h,
        }
    }
}

/// Sets every plate inside `rect` to [`Plate::Floor`].
fn fill_rect(plates: &mut [Plate], map_w: i32, map_h: i32, rect: Rect) {
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            if let Some(i) = Dungeon::index_in(map_w, map_h, i32::from(x), i32::from(y)) {
                plates[i] = Plate::Floor;
            }
        }
    }
}

/// The one-plate-wide floor strip joining two authored-adjacent rooms.
///
/// Assumes `a` and `b` share either an x or a y (true of every consecutive
/// pair `Dungeon::build` emits), so there is exactly one straight line to
/// draw: no gap search or corridor-carving algorithm is needed the way
/// `21_deck_plan` needs one for its procedurally split rooms, because here
/// the adjacency was chosen by hand rather than discovered after the fact.
fn straight_corridor(a: Rect, b: Rect) -> Rect {
    if a.left() == b.left() {
        // Vertically stacked: a corridor down the shared column's centre.
        let (top, bottom) = if a.top() < b.top() { (a, b) } else { (b, a) };
        let cx = top.left() + top.width() / 2;
        Rect::new(cx, top.bottom(), 1, bottom.top() - top.bottom())
    } else {
        // Side by side: a corridor across the shared row's centre.
        let (left, right) = if a.left() < b.left() { (a, b) } else { (b, a) };
        let cy = left.top() + left.height() / 2;
        Rect::new(left.right(), cy, right.left() - left.right(), 1)
    }
}

/// A minion's drawn silhouette. Four distinct two-row busts so a squad reads
/// at a glance even before the level badge is legible.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Skull,
    Hooded,
    Horned,
    Wraith,
}

impl Kind {
    const ALL: [Self; 4] = [Self::Skull, Self::Hooded, Self::Horned, Self::Wraith];

    /// Two rows of bust art, each at most [`BUST_W`] - 2 columns wide (the
    /// panel interior).
    const fn art(self) -> (&'static str, &'static str) {
        match self {
            Self::Skull => ("(o..o)", " \\==/ "),
            Self::Hooded => ("/^^^^\\", "|o..o|"),
            Self::Horned => ("\\.  ./", "(o''o)"),
            Self::Wraith => (".-''-.", "( >< )"),
        }
    }

    /// The eyes-closed variant of the second art row, for the idle blink.
    const fn blink(self) -> &'static str {
        match self {
            Self::Skull => " \\--/ ",
            Self::Hooded => "|-..-|",
            Self::Horned => "(-''-)",
            Self::Wraith => "( -- )",
        }
    }

    const fn tint(self) -> Color {
        match self {
            Self::Skull => rgb(210, 210, 200),
            Self::Hooded => rgb(150, 120, 180),
            Self::Horned => rgb(200, 120, 90),
            Self::Wraith => rgb(130, 170, 200),
        }
    }
}

/// One minion on the bench.
struct Minion {
    name: &'static str,
    kind: Kind,
    level: u8,
    hp: f32,
}

/// A stable, deterministic bench: sixteen minions with varied kinds, levels,
/// and HP fractions, built from index-derived hashes rather than any RNG
/// state, so the initial roster is identical on every run.
fn build_bench() -> [[Minion; SQUAD_SIZE]; SQUAD_COUNT] {
    const NAMES: [&str; MINION_COUNT] = [
        "Grix", "Mard", "Ossa", "Thane", "Vael", "Doran", "Yska", "Krel", "Nemet", "Ulra", "Fenn",
        "Sable", "Orin", "Petra", "Cael", "Rook",
    ];
    let mut names = NAMES.into_iter();
    core::array::from_fn(|squad| {
        core::array::from_fn(|slot| {
            let i = squad * SQUAD_SIZE + slot;
            let kind = Kind::ALL[i % Kind::ALL.len()];
            let level = 3 + (hash01(0x4D0B, i as i32, 0) * 14.0) as u8;
            let hp = 0.35f32.mul_add(hash01(0x9111, i as i32, 7), 0.55);
            Minion {
                name: names.next().unwrap_or("Bone"),
                kind,
                level,
                hp,
            }
        })
    })
}

/// What tapping a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Room(usize),
    Minion(usize),
    Squad(usize),
}

/// A drifting ember over the parchment: stable position, fading brightness.
/// Same technique as `21_deck_plan`'s starfield -- position from a hash so it
/// never reshuffles frame to frame, brightness from a phase added to elapsed
/// time so it visibly drifts.
fn ember_at(x: i32, y: i32, time: f32) -> Option<f32> {
    if hash01(0x0E4B, x, y) > 0.035 {
        return None;
    }
    let phase = hash01(0x2AA1, x, y) * core::f32::consts::TAU;
    let rise = (phase / core::f32::consts::TAU).mul_add(6.0, time * 0.4) % 6.0;
    // Fades in over the first second, holds, then fades out, so an ember
    // reads as guttering rather than as a fixed dot with a brightness wobble.
    let alpha = if rise < 1.0 {
        rise
    } else if rise > 4.0 {
        (6.0 - rise) / 2.0
    } else {
        1.0
    };
    Some(alpha.clamp(0.0, 1.0))
}

/// State: the dungeon plan, the bench, and everything needed to draw and
/// interact with both.
pub struct BoneLord {
    dungeon: Dungeon,
    /// The integer scale [`dungeon`](Self::dungeon) was last built at. Kept
    /// alongside the dungeon so a frame where the map area hasn't changed
    /// size can skip rebuilding it; see [`Self::sync_map_scale`].
    map_scale: i32,
    /// Top-left padding, in cells, added when drawing the plate grid inside
    /// the map area, so a scaled-up plan (which rarely fills its area to the
    /// exact cell) sits centred rather than pinned to the area's own
    /// top-left corner; see [`Self::sync_map_scale`].
    map_offset: (u16, u16),
    bench: [[Minion; SQUAD_SIZE]; SQUAD_COUNT],
    /// Room index the party currently occupies.
    current: usize,
    /// Rooms already advanced through, including `current`.
    visited: Vec<bool>,
    /// Room awaiting a confirming second tap.
    selected_room: Option<usize>,
    /// Minion index (`squad * SQUAD_SIZE + slot`) awaiting a swap target.
    selected_minion: Option<usize>,
    active_squad: usize,
    /// Materials: bone, flesh, ichor, iron. A simple fixed set rather than
    /// the reference's dozen counters, since the point on show is the row's
    /// layout, not an exhaustive crafting economy.
    materials: [u32; 4],
    gold: u32,
    mana: (u32, u32),
    /// Columns scrolled off the left edge of the roster strip.
    roster_scroll: u16,
    /// Viewport width the roster strip last drew into, used to clamp
    /// scrolling; see [`Self::roster_scroll_max`].
    roster_view_w: u16,
    /// How many rows the roster is currently split into (1 landscape/desktop,
    /// 2 portrait), kept in sync with the layout each tick so scroll
    /// clamping matches what is actually on screen; see
    /// [`Self::roster_content_width`].
    roster_rows: u16,
    /// Where a still-held press began, remembered across frames because the
    /// pointer clears it the instant a release fires; see
    /// [`Self::handle_drag_drop`].
    drag_from: Option<Pos>,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    time: f32,
    fps: FpsMeter,
}

const MATERIAL_NAMES: [&str; 4] = ["Bone", "Flesh", "Ichor", "Iron"];

impl Default for BoneLord {
    fn default() -> Self {
        let dungeon = Dungeon::build(1);
        let mut visited = vec![false; dungeon.rooms.len()];
        visited[0] = true;
        Self {
            dungeon,
            map_scale: 1,
            map_offset: (0, 0),
            bench: build_bench(),
            current: 0,
            visited,
            selected_room: None,
            selected_minion: None,
            active_squad: 0,
            materials: [12, 7, 3, 9],
            gold: 240,
            mana: (60, 100),
            roster_scroll: 0,
            roster_view_w: 0,
            roster_rows: 1,
            drag_from: None,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            time: 0.0,
            fps: FpsMeter::new(),
        }
    }
}

impl BoneLord {
    /// Whether `room` is one step from `current` along the authored chain and
    /// not yet visited -- the only rooms the party may advance into.
    fn reachable(&self, room: usize) -> bool {
        if self.visited[room] {
            return false;
        }
        let adjacent_next = room == self.current + 1 && self.current + 1 < self.dungeon.rooms.len();
        let adjacent_prev = self.current > 0 && room + 1 == self.current;
        (adjacent_next || adjacent_prev) && self.dungeon.edges > 0
    }

    /// Advances into `room`, granting a reward keyed to its kind. Numbers
    /// change on this discrete event, not continuously, so they never
    /// conflict with the rule that idle animation stays off text.
    fn advance_into(&mut self, room: usize) {
        self.current = room;
        self.visited[room] = true;
        match self.dungeon.rooms[room].kind {
            RoomKind::Fight => self.gold += 15,
            RoomKind::Loot => {
                self.materials[room % self.materials.len()] += 3;
                self.gold += 5;
            }
            RoomKind::Event => self.mana.0 = (self.mana.0 + 10).min(self.mana.1),
            RoomKind::Boss => {
                self.gold += 80;
                self.mana.0 = self.mana.1;
            }
            RoomKind::Start => {}
        }
        self.selected_room = None;
    }

    fn handle_tap(&mut self, at: Pos) {
        let Some(&action) = self.hotspots.hit(at) else {
            self.selected_room = None;
            self.selected_minion = None;
            return;
        };
        match action {
            Action::Room(room) => {
                if self.selected_room == Some(room) && self.reachable(room) {
                    self.advance_into(room);
                } else {
                    self.selected_room = Some(room);
                    self.selected_minion = None;
                }
            }
            Action::Minion(idx) => self.handle_minion_tap(idx),
            Action::Squad(squad) => {
                self.active_squad = squad;
                self.roster_scroll = self.squad_start_scroll(squad);
            }
        }
    }

    /// Tap-select-then-tap-target for swapping two minions: the first tap on
    /// a minion marks it; a second tap on a *different* minion swaps them; a
    /// second tap on the *same* one clears the selection. This is the same
    /// pattern the room map uses, applied to the bench.
    fn handle_minion_tap(&mut self, idx: usize) {
        self.selected_room = None;
        match self.selected_minion {
            None => self.selected_minion = Some(idx),
            Some(prev) if prev == idx => self.selected_minion = None,
            Some(prev) => {
                self.swap_minions(prev, idx);
                self.selected_minion = None;
            }
        }
    }

    fn swap_minions(&mut self, a: usize, b: usize) {
        let (sa, la) = (a / SQUAD_SIZE, a % SQUAD_SIZE);
        let (sb, lb) = (b / SQUAD_SIZE, b % SQUAD_SIZE);
        if sa == sb {
            self.bench[sa].swap(la, lb);
        } else {
            let (left, right) = if sa < sb {
                let (l, r) = self.bench.split_at_mut(sb);
                (&mut l[sa], &mut r[0])
            } else {
                let (l, r) = self.bench.split_at_mut(sa);
                (&mut r[0], &mut l[sb])
            };
            core::mem::swap(&mut left[la], &mut right[lb]);
        }
    }

    /// The scroll offset, in columns, that brings `squad`'s first slot to the
    /// left edge of the strip, clamped to the legal range.
    fn squad_start_scroll(&self, squad: usize) -> u16 {
        let col = (squad * SQUAD_SIZE) as u16 * BUST_W + squad as u16 * DIVIDER_W;
        col.min(self.roster_scroll_max())
    }

    /// How far the strip can scroll before its right edge would show past
    /// the last bust: total content width minus the last-drawn viewport
    /// width, floored at zero for a viewport wide enough to show everything.
    /// `roster_view_w` lags one frame behind a resize, which only matters for
    /// the one frame a resize happens on.
    const fn roster_scroll_max(&self) -> u16 {
        self.roster_content_width()
            .saturating_sub(self.roster_view_w)
    }

    /// Resolves a drag-drop as a minion swap, if both the origin and the drop
    /// point land on bust slots. Runs alongside
    /// [`handle_tap`](Self::handle_tap)'s tap-select-then-tap-target path
    /// rather than instead of it, satisfying the touch module's requirement
    /// that both tap and drag work for the same action.
    fn handle_drag_drop(&mut self, origin: Pos, drop_at: Pos) {
        let from = self.hotspots.hit(origin).copied();
        let to = self.hotspots.hit(drop_at).copied();
        if let (Some(Action::Minion(a)), Some(Action::Minion(b))) = (from, to)
            && a != b
        {
            self.swap_minions(a, b);
        }
        self.selected_minion = None;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
        }
        true
    }

    fn simulate(&mut self, dt: f32, gesture: &Gesture, roster_area: Rect) {
        self.time += dt;

        // A drag over the roster strip pans it; a drag anywhere else (or a
        // drag that started outside the strip) is ignored, so panning the
        // roster cannot be triggered by dragging over the map.
        if let Some(origin) = self.pointer.press_origin()
            && roster_area.contains_pos(origin)
            && gesture.delta.0 != 0
        {
            let next = i32::from(self.roster_scroll) - gesture.delta.0;
            self.roster_scroll = next.clamp(0, i32::from(self.roster_scroll_max())) as u16;
        }

        // `Gesture::press` holds the position a hold started at for as long
        // as it stays held, and disappears the instant it releases (see
        // `touch::Pointer::release`), so the origin has to be remembered here
        // across frames to still be available on the frame a drop lands.
        if let Some(p) = gesture.press {
            self.drag_from = Some(p);
        }
        if let Some(tap) = gesture.tap {
            self.handle_tap(tap);
            self.drag_from = None;
        }
        if let Some(drop_at) = gesture.drop
            && let Some(origin) = self.drag_from.take()
        {
            self.handle_drag_drop(origin, drop_at);
        }
    }

    /// Total width, in columns, of one roster row: the busts and squad
    /// dividers that actually land in it. Two rows (portrait) each show half
    /// the squads, so each row's content -- and therefore the legal scroll
    /// range -- is narrower than the single-row (landscape/desktop) case;
    /// clamping against the wrong one would let the strip scroll into blank
    /// space past a row's real content.
    const fn roster_content_width(&self) -> u16 {
        // `roster_rows` is always 1 or 2 (see the field docs), never 0, so
        // no zero-guard is needed on the division.
        let per_row = MINION_COUNT as u16 / self.roster_rows;
        let squads_per_row = per_row / SQUAD_SIZE as u16;
        per_row * BUST_W + squads_per_row.saturating_sub(1) * DIVIDER_W
    }

    fn status(&self) -> String {
        let room = &self.dungeon.rooms[self.current];
        format!(
            "at {} ({})  squad {}",
            room.label,
            room.kind.label(),
            roman(self.active_squad + 1)
        )
    }

    // -- drawing -----------------------------------------------------------

    fn draw_resources(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let counts: Vec<String> = self.materials.iter().map(ToString::to_string).collect();
        let mut spans = Vec::new();
        for (name, count) in MATERIAL_NAMES.iter().zip(&counts) {
            spans.push(Span::dim(name));
            spans.push(Span::plain(" "));
            spans.push(Span::keyword(count));
            spans.push(Span::plain("   "));
        }
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width(),
            &spans,
            ui::CHROME_BG,
        );

        if area.height() < 2 {
            return;
        }
        let gold_text = format!("Gold {}", self.gold);
        let mana_label = format!("Mana {}/{} ", self.mana.0, self.mana.1);
        let used = panel::spans(
            surface,
            (area.left(), area.top() + 1),
            area.width(),
            &[
                Span::keyword(&gold_text),
                Span::plain("   "),
                Span::dim(&mana_label),
            ],
            ui::CHROME_BG,
        );
        // The bar is placed *after* the label it belongs to, using the cell
        // count `spans` actually wrote, rather than at a fixed guessed
        // column: a fixed offset silently overlaps the label's own text the
        // moment gold or mana grows past however many digits were assumed.
        let bar_x = area.left() + used;
        if area.width() > used + 10 {
            panel::bar(
                surface,
                (bar_x, area.top() + 1),
                10,
                self.mana.0 as f32 / self.mana.1.max(1) as f32,
                rgb(120, 150, 226),
                rgb(30, 30, 40),
            );
        }
    }

    /// A candlelit parchment field: a warm gradient that drifts across the
    /// whole map area, plus a scatter of drifting embers. Both are
    /// decoration -- they never touch a glyph carrying text or a number, per
    /// the addendum's rule that idle animation stays off content.
    fn draw_parchment(&self, surface: &mut Surface<'_>, area: Rect) {
        for y in 0..area.height() {
            for x in 0..area.width() {
                let (wx, wy) = (i32::from(x), i32::from(y));
                let base = PARCHMENT.sample(hash01(0x7731, wx, wy) * 0.3 + 0.35);

                // A slow warm wash drifting left to right, as if a candle
                // just out of frame were guttering: a sine of position plus
                // time rather than of time alone, so the highlight visibly
                // travels across the page instead of merely pulsing in place.
                let wave =
                    0.15f32.mul_add((0.08f32.mul_add(f32::from(x), self.time * 0.6)).sin(), 0.15);
                let lit = mix(base, rgb(255, 220, 150), wave.max(0.0));

                let color = ember_at(wx, wy, self.time)
                    .map_or(lit, |alpha| mix(lit, rgb(255, 140, 60), alpha * 0.8));
                // Faint bleed-through of writing behind the parchment: a
                // sparse scatter of very dim marks, stable per cell.
                let glyph = if hash01(0x5502, wx, wy) < 0.02 {
                    '.'
                } else {
                    ' '
                };
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(scale(color, 0.6)).bg(color),
                );
            }
        }
    }

    /// Picks the largest integer scale at which the authored plan still
    /// fits `map_area` (never shrinking it below the 80x24 headless grid's
    /// scale of 1), rebuilds [`Self::dungeon`] at that scale if it changed,
    /// and recomputes the centering pad in [`Self::map_offset`]. Rebuilding
    /// only on an actual scale change (not every frame) keeps this cheap
    /// despite running once per tick: most frames see an unchanged terminal
    /// size and take the early return.
    fn sync_map_scale(&mut self, map_area: Rect) {
        let max_by_w = i32::from(map_area.width()) / BASE_MAP_W;
        let max_by_h = i32::from(map_area.height()) / BASE_MAP_H;
        let scale = max_by_w.min(max_by_h).max(1);
        if scale != self.map_scale {
            self.dungeon = Dungeon::build(scale);
            self.map_scale = scale;
        }
        let pad_x = (i32::from(map_area.width()) - self.dungeon.map_w).max(0) / 2;
        let pad_y = (i32::from(map_area.height()) - self.dungeon.map_h).max(0) / 2;
        self.map_offset = (pad_x as u16, pad_y as u16);
    }

    /// Draws the full parchment background across `area`, then the plate
    /// grid and rooms offset to `plates_area` -- the centering pad computed
    /// by [`Self::sync_map_scale`] sits between the two, so a scaled-down
    /// plan still leaves candlelit parchment (not a blank gap) around it
    /// rather than only covering its own top-left corner.
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect, plates_area: Rect) {
        self.draw_parchment(surface, area);
        let map_w = self.dungeon.map_w as u16;
        let map_h = self.dungeon.map_h as u16;
        for y in 0..plates_area.height().min(map_h) {
            for x in 0..plates_area.width().min(map_w) {
                let (wx, wy) = (i32::from(x), i32::from(y));
                let plate = self.dungeon.plate(wx, wy);
                if plate == Plate::Void {
                    continue;
                }
                let at = (plates_area.left() + x, plates_area.top() + y);
                match plate {
                    Plate::Wall => {
                        let connects = |p: Plate| p == Plate::Wall;
                        let mask = mask4([
                            connects(self.dungeon.plate(wx, wy - 1)),
                            connects(self.dungeon.plate(wx + 1, wy)),
                            connects(self.dungeon.plate(wx, wy + 1)),
                            connects(self.dungeon.plate(wx - 1, wy)),
                        ]);
                        surface.put(
                            at,
                            BOX_DOUBLE[(mask & 0x0F) as usize],
                            Style::new().fg(rgb(74, 54, 36)).bg(rgb(196, 176, 138)),
                        );
                    }
                    Plate::Floor => {
                        // Hatched floor: alternating tick marks rather than a
                        // flat fill, so a corridor reads as inked cross-hatch
                        // rather than a solid ink block.
                        let glyph = if (x + y).is_multiple_of(2) {
                            '\u{00b7}'
                        } else {
                            ' '
                        };
                        surface.put(
                            at,
                            glyph,
                            Style::new().fg(rgb(150, 128, 92)).bg(rgb(214, 196, 158)),
                        );
                    }
                    Plate::Void => unreachable!("filtered above"),
                }
            }
        }

        for (i, room) in self.dungeon.rooms.iter().enumerate() {
            self.draw_room(surface, plates_area, i, room);
        }
    }

    fn draw_room(&self, surface: &mut Surface<'_>, area: Rect, index: usize, room: &Room) {
        let rx = area.left() + room.rect.left();
        let ry = area.top() + room.rect.top();
        if rx >= area.right() || ry >= area.bottom() {
            return;
        }
        let w = room.rect.width().min(area.right().saturating_sub(rx));
        let h = room.rect.height().min(area.bottom().saturating_sub(ry));
        let rect = Rect::new(rx, ry, w, h);

        let current = index == self.current;
        let reachable = self.reachable(index);
        let selected = self.selected_room == Some(index);

        // Reachable rooms pulse gently: the ink brightens and dims on a slow
        // sine, which is the one piece of idle motion allowed on the map
        // itself, since it is a property of the room (can I go there?) and
        // not a label.
        let pulse = if reachable {
            0.15f32.mul_add((self.time * 2.2).sin(), 0.15)
        } else {
            0.0
        };
        let base_tint = if self.visited[index] {
            scale(room.kind.tint(), 0.65)
        } else {
            room.kind.tint()
        };
        let tint = mix(base_tint, rgb(255, 255, 255), pulse.max(0.0));
        let bg = if current {
            rgb(70, 54, 30)
        } else if selected {
            rgb(52, 46, 34)
        } else {
            rgb(24, 20, 16)
        };

        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let border = if current { rgb(246, 196, 96) } else { tint };
        for x in rect.left()..rect.right() {
            surface.put((x, rect.top()), '\u{2550}', Style::new().fg(border).bg(bg));
            surface.put(
                (x, rect.bottom() - 1),
                '\u{2550}',
                Style::new().fg(border).bg(bg),
            );
        }
        for y in rect.top()..rect.bottom() {
            surface.put((rect.left(), y), '\u{2551}', Style::new().fg(border).bg(bg));
            surface.put(
                (rect.right() - 1, y),
                '\u{2551}',
                Style::new().fg(border).bg(bg),
            );
        }
        surface.put(
            (rect.left(), rect.top()),
            '\u{2554}',
            Style::new().fg(border).bg(bg),
        );
        surface.put(
            (rect.right() - 1, rect.top()),
            '\u{2557}',
            Style::new().fg(border).bg(bg),
        );
        surface.put(
            (rect.left(), rect.bottom() - 1),
            '\u{255A}',
            Style::new().fg(border).bg(bg),
        );
        surface.put(
            (rect.right() - 1, rect.bottom() - 1),
            '\u{255D}',
            Style::new().fg(border).bg(bg),
        );

        if rect.width() > 2 && rect.height() > 2 {
            surface.put(
                (rect.left() + rect.width() / 2, rect.top() + 1),
                room.kind.glyph(),
                Style::new().fg(tint).bg(bg),
            );
        }
    }

    fn draw_legend(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new().title("Legend").draw(surface, area);
        if inner.width() < 4 {
            return;
        }
        for (i, kind) in RoomKind::ALL.iter().enumerate() {
            let y = inner.top() + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    Span::new("[", ui::DIM),
                    Span::new(&kind.glyph().to_string(), kind.tint()),
                    Span::new("] ", ui::DIM),
                    Span::plain(kind.label()),
                ],
                panel::PANEL_BG,
            );
        }

        let detail_y = inner.top() + RoomKind::ALL.len() as u16 + 1;
        if detail_y + 1 >= inner.bottom() {
            return;
        }
        let lines: Vec<String> = self.selected_room.map_or_else(
            || {
                self.selected_minion.map_or_else(
                    || {
                        vec![
                            "Tap a room".to_string(),
                            "or a minion".to_string(),
                            "for detail.".to_string(),
                        ]
                    },
                    |idx| {
                        let m = &self.bench[idx / SQUAD_SIZE][idx % SQUAD_SIZE];
                        vec![
                            m.name.to_string(),
                            format!("Lv {}", m.level),
                            format!("HP {:.0}%  tap another to swap", m.hp * 100.0),
                        ]
                    },
                )
            },
            |room| {
                let r = &self.dungeon.rooms[room];
                let status = if room == self.current {
                    "Here now".to_string()
                } else if self.visited[room] {
                    "Cleared".to_string()
                } else if self.reachable(room) {
                    "Tap again to enter".to_string()
                } else {
                    "Not reachable".to_string()
                };
                vec![r.label.to_string(), r.kind.label().to_string(), status]
            },
        );
        for (i, line) in lines.iter().enumerate() {
            let y = detail_y + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(line)],
                panel::PANEL_BG,
            );
        }
    }

    /// Draws one minion slot and returns its screen rect (for hotspot
    /// registration), or `None` if it fell entirely outside `area`.
    fn draw_minion(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        idx: usize,
        x: i32,
        y0: u16,
    ) -> Option<Rect> {
        if x + i32::from(BUST_W) <= 0 || x >= i32::from(area.width()) {
            return None;
        }
        let left = i32::from(area.left()) + x;
        let clipped_left = left.max(i32::from(area.left()));
        let right = (left + i32::from(BUST_W)).min(i32::from(area.right()));
        if right <= clipped_left {
            return None;
        }
        let rect = Rect::new(
            clipped_left as u16,
            area.top() + y0,
            (right - clipped_left) as u16,
            BUST_H.min(area.height().saturating_sub(y0)),
        );
        if rect.height() == 0 {
            return None;
        }

        let minion = &self.bench[idx / SQUAD_SIZE][idx % SQUAD_SIZE];
        let selected = self.selected_minion == Some(idx);
        let in_active_squad = idx / SQUAD_SIZE == self.active_squad;
        let accent = if selected {
            mix(minion.kind.tint(), rgb(255, 255, 255), 0.5)
        } else if in_active_squad {
            minion.kind.tint()
        } else {
            scale(minion.kind.tint(), 0.7)
        };
        let bg = if in_active_squad {
            rgb(26, 22, 30)
        } else {
            rgb(14, 12, 16)
        };

        let border = if selected {
            Border::Double
        } else {
            Border::Single
        };
        let inner = Panel::new()
            .border(border)
            .frame(accent)
            .bg(bg)
            .draw(surface, rect);
        if inner.width() == 0 || inner.height() == 0 {
            return Some(rect);
        }

        // Blink: swap the second art row to its closed-eyes variant during a
        // short window on a per-minion hashed phase, so the strip is never
        // perfectly static but no two minions blink in lockstep.
        let phase = hash01(0x66A1, idx as i32, 0) * 6.0;
        let cycle = (self.time + phase) % 6.0;
        let (top_art, bottom_art) = minion.kind.art();
        let bottom_art = if cycle < 0.15 {
            minion.kind.blink()
        } else {
            bottom_art
        };

        if inner.height() > 0 {
            surface.print(
                (inner.left(), inner.top()),
                retroglyph_widgets::truncate(top_art, inner.width_usize()),
                Style::new().fg(accent).bg(bg),
            );
        }
        if inner.height() > 1 {
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(bottom_art, inner.width_usize()),
                Style::new().fg(accent).bg(bg),
            );
        }
        if inner.height() > 2 {
            let level = format!("Lv{}", minion.level);
            surface.print(
                (inner.left(), inner.top() + 2),
                &level,
                Style::new().fg(ui::FG).bg(bg),
            );
        }
        if inner.height() > 3 {
            panel::bar(
                surface,
                (inner.left(), inner.top() + 3),
                inner.width(),
                minion.hp,
                panel::threshold(minion.hp),
                rgb(40, 24, 24),
            );
        }
        Some(rect)
    }

    /// Draws one divider between squads, carrying its roman numeral, and
    /// returns its screen rect for hotspot registration.
    fn draw_divider(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        squad_after: usize,
        x: i32,
        y0: u16,
        h: u16,
    ) -> Option<Rect> {
        if x + i32::from(DIVIDER_W) <= 0 || x >= i32::from(area.width()) {
            return None;
        }
        let left = (i32::from(area.left()) + x).max(i32::from(area.left()));
        let right =
            (i32::from(area.left()) + x + i32::from(DIVIDER_W)).min(i32::from(area.right()));
        if right <= left {
            return None;
        }
        let rect = Rect::new(
            left as u16,
            area.top() + y0,
            (right - left) as u16,
            h.min(area.height().saturating_sub(y0)),
        );
        let active = squad_after == self.active_squad;
        let color = if active { ui::ACCENT } else { ui::DIM };
        for y in rect.top()..rect.bottom() {
            surface.put(
                (rect.left(), y),
                '\u{2551}',
                Style::new().fg(color).bg(ui::CHROME_BG),
            );
        }
        if rect.width() > 1 && rect.height() > 1 {
            let label = roman(squad_after + 1);
            surface.print(
                (rect.left() + 1, rect.top() + rect.height() / 2),
                label,
                Style::new().fg(color).bg(ui::CHROME_BG),
            );
        }
        Some(rect)
    }

    /// Lays out and draws the roster strip. `rows` is 1 (landscape/desktop,
    /// all sixteen busts side by side, scrolled) or 2 (portrait, split into
    /// two ranks of eight so the map keeps a usable height).
    fn draw_roster(&mut self, surface: &mut Surface<'_>, area: Rect, rows: u16) {
        panel::band(surface, area);
        self.roster_view_w = area.width();
        if area.width() < BUST_W || area.height() == 0 {
            return;
        }
        let per_row = MINION_COUNT / rows as usize;
        let row_h = (area.height() / rows).max(1);

        // Clamp scroll here (rather than only in `simulate`) so a resize
        // that shrinks the strip cannot leave it scrolled past its own
        // content, which would otherwise show a blank gap at the right edge.
        let content_w = self.roster_content_width();
        let max_scroll = content_w.saturating_sub(area.width());
        self.roster_scroll = self.roster_scroll.min(max_scroll);

        for row in 0..rows {
            let y0 = row * row_h;
            let mut cursor: i32 = -i32::from(self.roster_scroll);
            let start = row as usize * per_row;
            for local in 0..per_row {
                let idx = start + local;
                if local > 0 && local.is_multiple_of(SQUAD_SIZE) {
                    let squad_after = idx / SQUAD_SIZE;
                    if let Some(rect) =
                        self.draw_divider(surface, area, squad_after, cursor, y0, row_h)
                    {
                        self.hotspots
                            .push_tappable(rect, area, Action::Squad(squad_after));
                    }
                    cursor += i32::from(DIVIDER_W);
                }
                if let Some(rect) = self.draw_minion(surface, area, idx, cursor, y0) {
                    self.hotspots.push_tappable(rect, area, Action::Minion(idx));
                }
                cursor += i32::from(BUST_W);
            }
        }
    }
}

/// Formats `n` (1-8) as an uppercase roman numeral, which is all the squad
/// count and squad-after-divider values ever need. Not a general converter:
/// a lookup keeps this obviously correct rather than reimplementing
/// subtractive notation for a domain of four values.
const fn roman(n: usize) -> &'static str {
    match n {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        6 => "VI",
        7 => "VII",
        8 => "VIII",
        _ => "?",
    }
}

impl Demo for BoneLord {
    const NAME: &'static str = "43_bone_lord";
    const TITLE: &'static str = "43 Bone Lord";
    const BLURB: &'static str = "Iratus parchment dungeon plan over a four-rank minion bench.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap", "select/advance/swap"),
            ("drag", "pan roster or swap minions"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let shape = Shape::of(content);

        let (resource_area, rest) = panel::split_top(content, RESOURCE_H);
        let roster_rows: u16 = if shape == Shape::Portrait { 2 } else { 1 };
        self.roster_rows = roster_rows;
        let roster_h = roster_rows * BUST_H;
        let (rest, roster_area) = panel::split_bottom(rest, roster_h);

        let legend_w = if rest.width() >= 40 {
            14
        } else if rest.width() >= 24 {
            10
        } else {
            0
        };
        let (legend_area, map_area) = panel::split_left(rest, legend_w);

        // Picks (and, if the viewport changed size since last frame,
        // rebuilds) the dungeon at the largest scale that still fits
        // `map_area`, so the plan fills the parchment on a roomy viewport
        // instead of sitting pinned to a fixed small plate extent. The
        // centering pad this produces is folded into `plates_area` below,
        // which both hotspot registration and drawing then share, so a tap
        // can never land on a room the frame did not actually draw there.
        self.sync_map_scale(map_area);
        let (pad_x, pad_y) = self.map_offset;
        let plates_area = Rect::new(
            map_area.left() + pad_x,
            map_area.top() + pad_y,
            map_area.width().saturating_sub(pad_x),
            map_area.height().saturating_sub(pad_y),
        );

        // Resolve this frame's input against *last* frame's hotspot
        // registrations before clearing and rebuilding them: `Hotspots` is
        // rebuilt fresh every tick (see its module docs), so a tap has to be
        // read while the previous frame's layout is still the one on record.
        let gesture = self.pointer.take();
        self.simulate(dt, &gesture, roster_area);

        self.hotspots.clear();
        for (i, room) in self.dungeon.rooms.iter().enumerate() {
            let rx = plates_area.left() + room.rect.left();
            let ry = plates_area.top() + room.rect.top();
            if rx >= map_area.right() || ry >= map_area.bottom() {
                continue;
            }
            let w = room.rect.width().min(map_area.right().saturating_sub(rx));
            let h = room.rect.height().min(map_area.bottom().saturating_sub(ry));
            if w == 0 || h == 0 {
                continue;
            }
            self.hotspots
                .push_tappable(Rect::new(rx, ry, w, h), map_area, Action::Room(i));
        }

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_resources(&mut surface, resource_area);
        self.draw_map(&mut surface, map_area, plates_area);
        if legend_area.width() > 0 {
            self.draw_legend(&mut surface, legend_area);
        }
        self.draw_roster(&mut surface, roster_area, roster_rows);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(BoneLord);

#[cfg(test)]
mod tests {
    use super::{
        Action, BoneLord, Dungeon, MINION_COUNT, Room, RoomKind, SQUAD_SIZE, straight_corridor,
    };
    use retroglyph_core::Pos;

    #[test]
    fn the_snake_has_thirteen_rooms_and_twelve_edges() {
        let dungeon = Dungeon::build(1);
        assert_eq!(dungeon.rooms.len(), 13);
        assert_eq!(dungeon.edges, 12);
    }

    #[test]
    fn a_larger_scale_reproduces_the_same_room_count_at_a_bigger_footprint() {
        let base = Dungeon::build(1);
        let scaled = Dungeon::build(4);
        assert_eq!(scaled.rooms.len(), base.rooms.len());
        assert_eq!(scaled.map_w, base.map_w * 4);
        assert_eq!(scaled.map_h, base.map_h * 4);
    }

    #[test]
    fn only_the_next_unvisited_room_in_the_chain_is_reachable() {
        let lord = BoneLord::default();
        assert!(!lord.reachable(0), "the starting room is already visited");
        assert!(lord.reachable(1), "the room right after the start is next");
        assert!(
            !lord.reachable(2),
            "nothing past the immediate neighbour opens up early"
        );
    }

    #[test]
    fn advancing_marks_the_room_visited_and_moves_the_party() {
        let mut lord = BoneLord::default();
        lord.advance_into(1);
        assert_eq!(lord.current, 1);
        assert!(lord.visited[1]);
        assert!(lord.reachable(2), "the chain opens up one room at a time");
    }

    #[test]
    fn a_fight_room_pays_gold() {
        let mut lord = BoneLord::default();
        let gold_before = lord.gold;
        lord.advance_into(1); // Crypt: Fight
        assert!(lord.gold > gold_before, "a fight should pay out gold");
    }

    #[test]
    fn a_loot_room_pays_materials() {
        let mut lord = BoneLord {
            current: 2,
            ..BoneLord::default()
        };
        lord.visited[2] = true;
        let materials_before = lord.materials;
        lord.advance_into(3); // Vault: Loot
        assert_ne!(
            lord.materials, materials_before,
            "loot should add materials"
        );
    }

    #[test]
    fn swapping_two_minions_in_different_squads_exchanges_their_slots() {
        let mut lord = BoneLord::default();
        let a_name = lord.bench[0][0].name;
        let b_name = lord.bench[1][2].name;
        assert_ne!(a_name, b_name);
        lord.swap_minions(0, SQUAD_SIZE + 2);
        assert_eq!(lord.bench[0][0].name, b_name);
        assert_eq!(lord.bench[1][2].name, a_name);
    }

    #[test]
    fn tap_select_then_tap_target_swaps_two_minions() {
        let mut lord = BoneLord::default();
        let a_name = lord.bench[0][0].name;
        let b_name = lord.bench[0][1].name;
        lord.handle_minion_tap(0);
        assert_eq!(lord.selected_minion, Some(0));
        lord.handle_minion_tap(1);
        assert_eq!(
            lord.selected_minion, None,
            "a completed swap clears the selection"
        );
        assert_eq!(lord.bench[0][0].name, b_name);
        assert_eq!(lord.bench[0][1].name, a_name);
    }

    #[test]
    fn tapping_the_same_minion_twice_deselects_it_without_swapping() {
        let mut lord = BoneLord::default();
        let name = lord.bench[0][0].name;
        lord.handle_minion_tap(0);
        lord.handle_minion_tap(0);
        assert_eq!(lord.selected_minion, None);
        assert_eq!(lord.bench[0][0].name, name);
    }

    #[test]
    fn tapping_a_squad_numeral_makes_it_active_and_scrolls_to_it() {
        let mut lord = BoneLord::default();
        lord.hotspots
            .push(retroglyph_core::Rect::new(0, 0, 9, 4), Action::Squad(2));
        lord.handle_tap(Pos::new(1, 1));
        assert_eq!(lord.active_squad, 2);
    }

    #[test]
    fn a_room_is_selected_on_first_tap_and_entered_on_the_second() {
        let mut lord = BoneLord::default();
        lord.hotspots
            .push(retroglyph_core::Rect::new(0, 0, 9, 4), Action::Room(1));
        lord.handle_tap(Pos::new(1, 1));
        assert_eq!(lord.selected_room, Some(1));
        assert_eq!(lord.current, 0, "one tap only selects, it does not advance");
        lord.handle_tap(Pos::new(1, 1));
        assert_eq!(
            lord.current, 1,
            "a second tap on a reachable, selected room advances"
        );
    }

    #[test]
    fn straight_corridor_connects_adjacent_rooms_in_a_single_line() {
        let a = Room {
            rect: retroglyph_core::Rect::new(2, 7, 4, 3),
            kind: RoomKind::Start,
            label: "a",
        };
        let b = Room {
            rect: retroglyph_core::Rect::new(8, 7, 4, 3),
            kind: RoomKind::Fight,
            label: "b",
        };
        let corridor = straight_corridor(a.rect, b.rect);
        assert_eq!(corridor.top(), a.rect.top() + a.rect.height() / 2);
        assert_eq!(corridor.height(), 1);
        assert!(
            corridor.width() > 0,
            "adjacent rooms must have a nonzero gap to bridge"
        );
    }

    #[test]
    fn roster_content_width_is_narrower_per_row_once_split_in_two() {
        let mut lord = BoneLord {
            roster_rows: 1,
            ..BoneLord::default()
        };
        let one_row = lord.roster_content_width();
        lord.roster_rows = 2;
        let two_rows = lord.roster_content_width();
        assert!(
            two_rows < one_row,
            "each portrait row shows half the squads"
        );
        assert_eq!(one_row as usize, MINION_COUNT * 9 + 3 * 3);
    }
}
