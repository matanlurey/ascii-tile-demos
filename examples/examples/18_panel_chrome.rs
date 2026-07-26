//! 18: Panel chrome -- the multi-panel roguelike interface, at high
//! resolution.
//!
//! Every demo so far has been one map filling the whole grid. This one is the
//! opposite case, and just as common in the genre: a dungeon crawler's screen
//! is mostly *chrome* -- character sheet, power list, equipment, threat
//! roster, party list, message log -- with the map itself sharing space
//! rather than owning it. `ui::panel` exists because this is the demo that
//! needed nine different bordered boxes to agree with each other.
//!
//! The design grid is 160x46: wide enough that the three columns each get a
//! usable width, following Kyzrati's own rule for Cogmind's original layout
//! (see the `Full UI Upscaling` series on the Grid Sage Games blog) that a
//! wargame-density interface has a resolution below which it stops
//! demonstrating a *layout* and starts demonstrating a *compromise*. The
//! responsive rule below is that compromise made explicit rather than
//! implicit: panels drop in a fixed order as the terminal narrows, so the
//! demo degrades to "a legible dungeon plus a log" rather than to a
//! window full of half-truncated boxes.
//!
//! Techniques on show:
//!
//! - **Panel composition** ([`ui::panel::Panel`]): every box on screen, from
//!   the character sheet down to the message log's frame, is the same builder
//!   with different titles, badges, and focus state. `Tab` cycles which panel
//!   is focused, brightening its frame -- Cogmind's convention of dimming
//!   everything *except* the active window, rather than decorating the active
//!   one, so the resting state of the screen is the readable one.
//! - **Gauges through `panel::bar`** ([`draw_character`]): Health, Hunger,
//!   Thirst, and Fatigue, each colored by [`panel::threshold`] so the reader
//!   never has to learn what "half full" means more than once.
//! - **A selectable list** ([`draw_powers`]): the Powers panel is what
//!   `panel::Span`/`spans` are for -- a hotkey in the accent color, a name in
//!   body text, and the selected row inverted by swapping foreground and
//!   background rather than by a border, so selection reads at a glance
//!   scanning down a list of six.
//! - **Dimmed-vs-highlighted equipment slots** ([`draw_equipment`]): an empty
//!   slot is a dash in the dim color; a filled one is the item name on a
//!   raised background. Doubles as the model Dungeons of Everchange uses for
//!   its slot list.
//! - **Procedural dungeon generation** ([`Dungeon::generate`]): random rooms
//!   connected by L-shaped corridors, walls chosen by
//!   [`tilekit::autotile::mask4`] so a run of wall segments joins into
//!   continuous box-drawing lines instead of a field of identical `#`.
//! - **Shadowcasting FOV over remembered terrain** ([`draw_map`]):
//!   [`tilekit::fov::shadowcast`] lights what the player can currently see;
//!   everything else that has ever been seen renders through
//!   [`tilekit::palette::remembered`], and everything never seen is the
//!   unexplored color. Monsters and items only draw in the lit region --
//!   remembering a room does not mean remembering what is standing in it.
//! - **A colored message log with inline spans** ([`SpanLog`]): every line is
//!   built from several [`panel::Span`]s, so a monster's name keeps its own
//!   color inside a sentence, item names read in cyan, and damage numbers
//!   read in red, the RexPaint-roguelike convention this whole demo borrows.
//!   [`ui::panel::Log`] stores one color per whole line, which is enough for
//!   most demos but not this one, so `SpanLog` is a small local sibling built
//!   on the same fixed-capacity, age-fading shape.
//! - **A RogueNet-style party roster** ([`draw_party`]): a small list of
//!   named companions, each in their own color, alongside the dungeon's own
//!   threat list -- present without being interactive, because the point here
//!   is the panel vocabulary, not a chat protocol.
//!
//! ```sh
//! cargo run --example 18_panel_chrome --features crossterm
//! cargo run --example 18_panel_chrome --features software
//! cargo run --example 18_panel_chrome --features gl
//! cargo run --example 18_panel_chrome  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::BOX_SINGLE;
use tilekit::autotile::mask4;
use tilekit::fov::shadowcast;
use tilekit::geom::Cell;
use tilekit::noise::Rng;
use tilekit::palette::{mix, remembered, rgb, scale, unexplored};
use tilekit::path::{self, Diagonals};

/// Dungeon size in cells.
///
/// Sized against the demo's own map panel, not picked in isolation: at
/// [`PanelChrome::GRID`] (160x46) the map panel interior comes out to roughly
/// 100x33 cells once the side columns, chrome, and log band are subtracted.
/// A dungeon much smaller than that in *floor area* (as opposed to bounding
/// box) leaves most of the panel showing unexplored black no matter how well
/// the camera centers, which is what the original 70x40 dungeon at 11 rooms
/// did: roughly 450 floor cells scattered as small islands in a 2,800-cell
/// box is under 20% coverage, and FOV only ever lights a small disc of that
/// at once. Panning still means something here -- the dungeon is larger than
/// the panel in both axes -- it is just not so much larger that "fill the
/// panel" and "generate a dungeon" are fighting each other.
const DUNGEON_W: i32 = 120;
/// See [`DUNGEON_W`].
const DUNGEON_H: i32 = 60;

/// Rooms placed per generated level. See [`Dungeon::generate`] for why this
/// number (rather than the original 11) is what makes the map panel read as
/// full.
const ROOM_COUNT: usize = 42;

/// How many milliseconds between simulation steps.
///
/// Slow enough to read as turn-like -- the reference games are all
/// step-based, not continuously animated -- fast enough that the thumbnail
/// tool's two sampled frames (a few seconds apart) never land on an identical
/// state. Milliseconds as an integer rather than seconds as a float so the
/// per-frame accumulator can be compared and subtracted exactly, with no
/// epsilon: `frame.delta` is real wall-clock time, not a multiple of the step,
/// so a float accumulator would need a fuzz margin to reliably fire and this
/// sidesteps that entirely.
const SIM_STEP_MS: u32 = 400;

/// Sight radius in cells, for [`shadowcast`].
const SIGHT_RADIUS: i32 = 9;

// ── Dungeon generation ───────────────────────────────────────────────────────

/// A rectangular room, in dungeon cells.
#[derive(Clone, Copy)]
struct Room {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Room {
    const fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    const fn intersects(self, other: Self) -> bool {
        // A one-cell margin so rooms never share a wall, which would let two
        // room outlines merge into one wide opening.
        self.x - 1 < other.x + other.w
            && other.x - 1 < self.x + self.w
            && self.y - 1 < other.y + other.h
            && other.y - 1 < self.y + self.h
    }
}

/// A monster wandering the dungeon.
struct Monster {
    x: i32,
    y: i32,
    glyph: char,
    color: Color,
    name: &'static str,
    hp: i32,
    max_hp: i32,
}

/// An item lying on the floor.
struct Item {
    x: i32,
    y: i32,
    glyph: char,
    name: &'static str,
}

/// The generated level: which cells are floor, plus what is standing on it.
struct Dungeon {
    floor: Vec<bool>,
    rooms: Vec<Room>,
    monsters: Vec<Monster>,
    items: Vec<Item>,
    player: (i32, i32),
}

impl Dungeon {
    const fn in_bounds(x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < DUNGEON_W && y < DUNGEON_H
    }

    const fn index(x: i32, y: i32) -> usize {
        (y * DUNGEON_W + x) as usize
    }

    fn is_floor(&self, x: i32, y: i32) -> bool {
        Self::in_bounds(x, y) && self.floor[Self::index(x, y)]
    }

    fn carve_room(&mut self, room: Room) {
        for y in room.y..room.y + room.h {
            for x in room.x..room.x + room.w {
                if Self::in_bounds(x, y) {
                    self.floor[Self::index(x, y)] = true;
                }
            }
        }
    }

    /// Carves one cell, plus its neighbor one step clockwise of `axis`, so a
    /// corridor is two cells wide rather than one.
    ///
    /// A single-cell corridor is correct for a dungeon crawler played one
    /// cell at a time, but it draws as a thin thread of floor surrounded by
    /// walls on both sides, which at this demo's scale (a ~100-cell-wide map
    /// panel) reads as almost nothing: most of a corridor's screen footprint
    /// is the wall bracketing it, not the floor itself. Widening corridors is
    /// the cheapest way to raise floor coverage without changing the room
    /// layout algorithm at all.
    fn widen(&mut self, x: i32, y: i32, horizontal: bool) {
        if Self::in_bounds(x, y) {
            self.floor[Self::index(x, y)] = true;
        }
        let (wx, wy) = if horizontal { (x, y + 1) } else { (x + 1, y) };
        if Self::in_bounds(wx, wy) {
            self.floor[Self::index(wx, wy)] = true;
        }
    }

    fn carve_corridor(&mut self, from: (i32, i32), to: (i32, i32), rng: &mut Rng) {
        let (x0, y0) = from;
        let (x1, y1) = to;
        // An L-shaped corridor, horizontal-then-vertical or vertical-then
        // -horizontal chosen at random, which is what keeps a generated level
        // from reading as a rigid grid of right angles all bent the same way.
        let horizontal_first = rng.next_below(2) == 0;
        let (mid_x, mid_y) = if horizontal_first { (x1, y0) } else { (x0, y1) };

        for x in x0.min(mid_x)..=x0.max(mid_x) {
            self.widen(x, y0, true);
        }
        for y in y0.min(mid_y)..=y0.max(mid_y) {
            self.widen(mid_x, y, false);
        }
        for x in x1.min(mid_x)..=x1.max(mid_x) {
            self.widen(x, y1, true);
        }
        for y in y1.min(mid_y)..=y1.max(mid_y) {
            self.widen(mid_x, y, false);
        }
    }

    /// Builds a new level from `seed`: random rooms, rejected on overlap,
    /// connected by L-corridors, then populated.
    ///
    /// Room count and size are the whole fix for the mostly-black map this
    /// generator used to produce: 11 rooms of 5-10 x 4-7 cells in a 70x40
    /// dungeon is about 450 floor cells, or 16% coverage, scattered as
    /// islands. A ~100x33 map panel showing a dungeon that sparse is going to
    /// be almost entirely wall and unexplored black no matter how well the
    /// camera centers, because there simply isn't enough floor to fill it.
    /// [`ROOM_COUNT`] and the room size range below roughly quadruple both
    /// the room count and the corridor width (see [`Dungeon::widen`]), enough
    /// that a [`DUNGEON_W`] x [`DUNGEON_H`] level covers close to half its
    /// bounding box in floor, which is what makes panning around it read as
    /// "a dungeon" rather than "a hallway in a black void".
    ///
    /// Connections are also denser than a simple chain: every room connects
    /// to its nearest not-yet-connected neighbor by placement order *and* to
    /// the room before that, which is what keeps the level from being one
    /// single corridor with rooms hanging off it -- a shape that looks fine
    /// on a minimap and terrible in a panel that shows only a 30-cell-wide
    /// slice of it at a time, since half of any given view is then a dead
    /// corridor rather than a junction.
    fn generate(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let floor = vec![false; (DUNGEON_W * DUNGEON_H) as usize];
        let mut rooms: Vec<Room> = Vec::new();

        for _ in 0..600 {
            if rooms.len() >= ROOM_COUNT {
                break;
            }
            let w = 6 + rng.next_below(8) as i32;
            let h = 5 + rng.next_below(6) as i32;
            let x = 1 + rng.next_below((DUNGEON_W - w - 2).max(1) as u32) as i32;
            let y = 1 + rng.next_below((DUNGEON_H - h - 2).max(1) as u32) as i32;
            let candidate = Room { x, y, w, h };
            if rooms.iter().any(|&r| candidate.intersects(r)) {
                continue;
            }
            rooms.push(candidate);
        }

        // A generator that (rarely, for a small unlucky seed) placed nothing
        // still has to produce a walkable dungeon rather than an empty one.
        if rooms.is_empty() {
            rooms.push(Room {
                x: DUNGEON_W / 2 - 4,
                y: DUNGEON_H / 2 - 3,
                w: 8,
                h: 6,
            });
        }

        let mut dungeon = Self {
            floor,
            rooms: rooms.clone(),
            monsters: Vec::new(),
            items: Vec::new(),
            player: (0, 0),
        };
        for &room in &rooms {
            dungeon.carve_room(room);
        }
        for pair in rooms.windows(2) {
            dungeon.carve_corridor(pair[0].center(), pair[1].center(), &mut rng);
        }
        // A second, skip-one pass so the level is a loosely connected mesh
        // rather than a single chain: every third or so junction gets an
        // extra corridor, which is what makes the auto-explorer's route
        // through the level double back and cross itself instead of walking
        // one corridor once and never returning.
        for pair in rooms.windows(3) {
            dungeon.carve_corridor(pair[0].center(), pair[2].center(), &mut rng);
        }

        dungeon.player = rooms[0].center();
        dungeon.populate(&mut rng);
        dungeon
    }

    fn populate(&mut self, rng: &mut Rng) {
        const BESTIARY: [(char, Color, &str, i32); 6] = [
            ('r', rgb(176, 140, 96), "rat", 6),
            ('k', rgb(150, 196, 108), "kobold", 10),
            ('s', rgb(196, 196, 210), "skeleton", 14),
            ('o', rgb(120, 176, 96), "orc", 18),
            ('b', rgb(170, 96, 196), "bat", 5),
            ('g', rgb(214, 176, 96), "goblin", 9),
        ];
        const LOOT: [(char, &str); 5] = [
            ('!', "potion"),
            ('?', "scroll"),
            ('$', "gold"),
            ('/', "wand"),
            (')', "blade"),
        ];

        // Skip the player's own starting room so nothing spawns on top of
        // them. Odds raised from the original 1-in-3 monster / 1-in-4 item (a
        // level of 11 rooms already read as sparse at those odds; 42 rooms at
        // the same odds would read as *emptier*, since the Threats panel is
        // meant to usually have something in it and a monster only ever
        // shows there while it is in the player's small sight radius).
        for room in self.rooms.iter().skip(1) {
            if rng.next_below(2) != 0 {
                let (glyph, color, name, hp) = *rng.choose(&BESTIARY).unwrap_or(&BESTIARY[0]);
                let x = room.x + 1 + rng.next_below((room.w - 2).max(1) as u32) as i32;
                let y = room.y + 1 + rng.next_below((room.h - 2).max(1) as u32) as i32;
                self.monsters.push(Monster {
                    x,
                    y,
                    glyph,
                    color,
                    name,
                    hp,
                    max_hp: hp,
                });
            }
            if rng.next_below(2) == 0 {
                let (glyph, name) = *rng.choose(&LOOT).unwrap_or(&LOOT[0]);
                let x = room.x + 1 + rng.next_below((room.w - 2).max(1) as u32) as i32;
                let y = room.y + 1 + rng.next_below((room.h - 2).max(1) as u32) as i32;
                self.items.push(Item { x, y, glyph, name });
            }
        }
    }

    /// One cardinal step toward `(tx, ty)`, or `(0, 0)` if already there.
    /// Greedy rather than pathed: a wandering monster bumping into a wall and
    /// trying a different cell next step reads as animal behavior, not as a
    /// bug, which is exactly why real roguelikes use it for ambient movement.
    fn step_toward(&self, from: (i32, i32), to: (i32, i32)) -> (i32, i32) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let mut candidates = [
            (dx.signum(), dy.signum()),
            (dx.signum(), 0),
            (0, dy.signum()),
        ];
        candidates.sort_by_key(|&(sx, sy)| (sx == 0 && sy == 0, 0));
        for (sx, sy) in candidates {
            if sx == 0 && sy == 0 {
                continue;
            }
            let next = (from.0 + sx, from.1 + sy);
            if self.is_floor(next.0, next.1) {
                return (sx, sy);
            }
        }
        (0, 0)
    }
}

// ── The player character ─────────────────────────────────────────────────────

/// A power the character can select in the Powers panel.
struct Power {
    key: char,
    name: &'static str,
}

const POWERS: [Power; 6] = [
    Power {
        key: '1',
        name: "Strike",
    },
    Power {
        key: '2',
        name: "Guard",
    },
    Power {
        key: '3',
        name: "Fireball",
    },
    Power {
        key: '4',
        name: "Heal",
    },
    Power {
        key: '5',
        name: "Shadow Step",
    },
    Power {
        key: '6',
        name: "Rally",
    },
];

/// An equipment slot: a name, and what fills it if anything.
struct Slot {
    label: &'static str,
    item: Option<&'static str>,
}

/// A party companion, for the roster panel.
struct Companion {
    name: &'static str,
    color: Color,
}

const PARTY: [Companion; 4] = [
    Companion {
        name: "Odila",
        color: rgb(214, 140, 200),
    },
    Companion {
        name: "Kesh",
        color: rgb(140, 200, 214),
    },
    Companion {
        name: "Brannor",
        color: rgb(200, 190, 120),
    },
    Companion {
        name: "Vess",
        color: rgb(150, 210, 140),
    },
];

/// One colored run of text, owned rather than borrowed.
///
/// [`panel::Span`] borrows its text, which is the right choice for a line
/// built once per frame from data already alive that long. A log is the
/// opposite case: each entry must outlive the frame it was written on, often
/// by dozens of frames, so this owns its `String` and only borrows out to a
/// [`panel::Span`] at the moment of drawing.
struct Run {
    text: String,
    color: Color,
}

/// A message log whose lines are built from several colored runs, so a
/// monster's name keeps its own color inside a sentence instead of the whole
/// line taking one color.
///
/// [`panel::Log`] stores one `(String, Color)` per line, which is enough for
/// most demos but not for this one: the whole point of the reference games'
/// logs is that a monster name, an item name, and a damage number each read in
/// their own color within the same sentence. Built on top of `panel::spans`
/// for the actual per-line draw and on the same fixed-capacity,
/// oldest-eviction, age-fade shape as `panel::Log`, so it reads as the same
/// kind of widget rather than a competing one.
struct SpanLog {
    lines: Vec<Vec<Run>>,
    capacity: usize,
}

impl SpanLog {
    fn new(capacity: usize) -> Self {
        Self {
            lines: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    fn push(&mut self, runs: &[panel::Span<'_>]) {
        self.lines.push(
            runs.iter()
                .map(|s| Run {
                    text: s.text.to_string(),
                    color: s.color,
                })
                .collect(),
        );
        if self.lines.len() > self.capacity {
            self.lines.remove(0);
        }
    }

    fn push_plain(&mut self, text: impl Into<String>, color: Color) {
        self.lines.push(vec![Run {
            text: text.into(),
            color,
        }]);
        if self.lines.len() > self.capacity {
            self.lines.remove(0);
        }
    }

    /// Draws the newest lines that fit `area`, oldest at the top, fading each
    /// run's color toward `bg` with age -- the same rule
    /// [`panel::Log::draw`] uses, reimplemented here because it operates on
    /// several colors per line rather than one.
    fn draw(&self, surface: &mut Surface<'_>, area: Rect, bg: Color) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        let rows = usize::from(area.height());
        let visible: Vec<&Vec<Run>> = self.lines.iter().rev().take(rows).collect();
        let n = visible.len();

        for (i, runs) in visible.into_iter().rev().enumerate() {
            let age = if n <= 1 {
                0.0
            } else {
                (n - 1 - i) as f32 / (n - 1) as f32
            };
            let spans: Vec<panel::Span<'_>> = runs
                .iter()
                .map(|r| panel::Span::new(&r.text, mix(r.color, bg, age * 0.55)))
                .collect();
            panel::spans(
                surface,
                (area.left(), area.top() + i as u16),
                area.width(),
                &spans,
                bg,
            );
        }
    }
}

/// Which panel `Tab` is currently focusing. Purely cosmetic (brightens one
/// panel's frame) except for [`Focus::Powers`], which is also where 1-6 are
/// read.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Map,
    Character,
    Powers,
    Equipment,
    Threats,
}

impl Focus {
    const fn next(self) -> Self {
        match self {
            Self::Map => Self::Character,
            Self::Character => Self::Powers,
            Self::Powers => Self::Equipment,
            Self::Equipment => Self::Threats,
            Self::Threats => Self::Map,
        }
    }
}

/// State: the dungeon, the character sheet, the sim clock, and the UI's own
/// bits (focus, selection, camera).
pub struct PanelChrome {
    dungeon: Dungeon,
    seed: u32,
    health: f32,
    hunger: f32,
    thirst: f32,
    fatigue: f32,
    selected_power: usize,
    slots: [Slot; 6],
    fog: Vec<tilekit::fov::Visibility>,
    camera_x: i32,
    camera_y: i32,
    focus: Focus,
    paused: bool,
    time: f32,
    sim_accum_ms: u32,
    log: SpanLog,
    fps: FpsMeter,
}

impl Default for PanelChrome {
    fn default() -> Self {
        let seed = 7;
        let dungeon = Dungeon::generate(seed);
        let fog = vec![tilekit::fov::Visibility::Unknown; (DUNGEON_W * DUNGEON_H) as usize];

        let mut log = SpanLog::new(64);
        log.push_plain("You descend into the dungeon.", ui::FG);

        let mut demo = Self {
            dungeon,
            seed,
            health: 0.82,
            hunger: 0.64,
            thirst: 0.71,
            fatigue: 0.2,
            selected_power: 0,
            slots: [
                Slot {
                    label: "Head",
                    item: None,
                },
                Slot {
                    label: "Body",
                    item: Some("Leather Cuirass"),
                },
                Slot {
                    label: "Hands",
                    item: None,
                },
                Slot {
                    label: "Feet",
                    item: Some("Traveler's Boots"),
                },
                Slot {
                    label: "Weapon",
                    item: Some("Iron Shortsword"),
                },
                Slot {
                    label: "Ring",
                    item: None,
                },
            ],
            fog,
            camera_x: 0,
            camera_y: 0,
            focus: Focus::default(),
            paused: false,
            time: 0.0,
            sim_accum_ms: 0,
            log,
            fps: FpsMeter::new(),
        };
        demo.preexplore();
        demo.recompute_fov();
        demo
    }
}

impl PanelChrome {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.dungeon = Dungeon::generate(self.seed);
        self.fog
            .iter_mut()
            .for_each(|v| *v = tilekit::fov::Visibility::Unknown);
        self.log.push_plain(
            format!("A new level unfolds. (seed {})", self.seed),
            ui::DIM,
        );
        self.preexplore();
        self.recompute_fov();
    }

    /// Marks a generous radius around the starting room as `Explored`.
    ///
    /// The gallery thumbnail tool and every browser visitor's first frame are
    /// the same frame: whatever the demo looks like before a single tick of
    /// simulation has run. A fresh `FogMap` starting fully `Unknown` is right
    /// for a game a human is about to sit down and play move by move, and
    /// wrong for a demo whose whole job is to be looked at -- an opening
    /// frame that is 90% unexplored black is not demonstrating remembered-vs-
    /// visible shading, it is hiding the dungeon this demo exists to show.
    /// Marking a few rooms' worth of the level `Explored` (not `Visible`: that
    /// distinction is still worth keeping, since only the small `Visible` disc
    /// around the player should ever show monsters or items) means the first
    /// frame already reads as "a dungeon someone has been exploring" rather
    /// than "a black screen with one lit room", while shadowcasting on every
    /// subsequent step still does the real work of lighting the current room.
    fn preexplore(&mut self) {
        // Comfortably past the map panel's own half-diagonal (at GRID's
        // ~100x33 interior that is ~53 cells), so the opening frame is full
        // regardless of exactly where within the starting cluster of rooms
        // the camera happens to center once drawing begins.
        const PREEXPLORE_RADIUS: i32 = 60;

        let (px, py) = self.dungeon.player;
        for dy in -PREEXPLORE_RADIUS..=PREEXPLORE_RADIUS {
            for dx in -PREEXPLORE_RADIUS..=PREEXPLORE_RADIUS {
                let (x, y) = (px + dx, py + dy);
                if !Dungeon::in_bounds(x, y)
                    || dx * dx + dy * dy > PREEXPLORE_RADIUS * PREEXPLORE_RADIUS
                {
                    continue;
                }
                // Floor cells are marked directly. A wall cell is marked too,
                // but only if it borders floor: marking *every* wall in the
                // radius would also reveal the far side of walls the player
                // has no way to have ever seen, which defeats the point of
                // remembered-vs-unknown shading existing at all. A wall with
                // no floor neighbor anywhere in the radius is deep rock no
                // room has ever opened onto, and stays black correctly.
                let bordered_floor = self.dungeon.is_floor(x, y)
                    || self.dungeon.is_floor(x, y - 1)
                    || self.dungeon.is_floor(x, y + 1)
                    || self.dungeon.is_floor(x - 1, y)
                    || self.dungeon.is_floor(x + 1, y);
                if bordered_floor {
                    self.fog[Dungeon::index(x, y)] = tilekit::fov::Visibility::Explored;
                }
            }
        }
    }

    /// Recomputes the player's visible set and folds it into the fog map.
    ///
    /// Demoting every `Visible` cell to `Explored` before casting is what
    /// makes a fog map accumulate rather than only show the current instant;
    /// see [`tilekit::fov::FogMap::begin_turn`], which this mirrors without
    /// pulling in the whole type (the dungeon needs per-cell writes the fixed
    /// `FogMap` API does not expose alongside a plain `Vec`, so it is inlined
    /// here instead).
    fn recompute_fov(&mut self) {
        for v in &mut self.fog {
            if *v == tilekit::fov::Visibility::Visible {
                *v = tilekit::fov::Visibility::Explored;
            }
        }
        let (px, py) = self.dungeon.player;
        let dungeon = &self.dungeon;
        let fog = &mut self.fog;
        shadowcast(
            px,
            py,
            SIGHT_RADIUS,
            |x, y| !dungeon.is_floor(x, y),
            |x, y| {
                if Dungeon::in_bounds(x, y) {
                    fog[Dungeon::index(x, y)] = tilekit::fov::Visibility::Visible;
                }
            },
        );
    }

    fn move_player(&mut self, dx: i32, dy: i32) {
        let (x, y) = self.dungeon.player;
        let (nx, ny) = (x + dx, y + dy);
        if !self.dungeon.is_floor(nx, ny) {
            return;
        }
        if let Some(i) = self
            .dungeon
            .monsters
            .iter()
            .position(|m| m.x == nx && m.y == ny)
        {
            self.attack(i);
            return;
        }
        self.dungeon.player = (nx, ny);
        if let Some(i) = self
            .dungeon
            .items
            .iter()
            .position(|it| it.x == nx && it.y == ny)
        {
            let item = self.dungeon.items.remove(i);
            self.log.push(&[
                panel::Span::plain("You pick up a "),
                panel::Span::new(item.name, rgb(120, 210, 220)),
                panel::Span::plain("."),
            ]);
        }
        self.recompute_fov();
    }

    fn attack(&mut self, index: usize) {
        let power = &POWERS[self.selected_power];
        let damage = 3 + (self.selected_power as i32) * 2;
        let monster = &mut self.dungeon.monsters[index];
        monster.hp -= damage;
        let name = monster.name;
        let color = monster.color;
        let dead = monster.hp <= 0;

        self.log.push(&[
            panel::Span::plain("You use "),
            panel::Span::keyword(power.name),
            panel::Span::plain(" on the "),
            panel::Span::new(name, color),
            panel::Span::plain(" for "),
            panel::Span::new(&damage.to_string(), rgb(220, 90, 84)),
            panel::Span::plain(" damage."),
        ]);

        if dead {
            self.dungeon.monsters.remove(index);
            self.log.push(&[
                panel::Span::plain("The "),
                panel::Span::new(name, color),
                panel::Span::plain(" falls."),
            ]);
        }
    }

    /// One simulation tick: monsters that can see the player shuffle a step
    /// closer, gauges drift, and the player auto-explores toward the nearest
    /// unexplored floor cell it can see. This is what the thumbnail tool's two
    /// sampled frames need to differ on, and what makes the demo read as a
    /// living dungeon rather than a static screenshot with a cursor in it.
    fn simulate(&mut self) {
        let (px, py) = self.dungeon.player;

        for i in 0..self.dungeon.monsters.len() {
            let (mx, my) = (self.dungeon.monsters[i].x, self.dungeon.monsters[i].y);
            if self.fog[Dungeon::index(mx, my)] != tilekit::fov::Visibility::Visible {
                continue;
            }
            let (dx, dy) = self.dungeon.step_toward((mx, my), (px, py));
            let (nx, ny) = (mx + dx, my + dy);
            if (nx, ny) == (px, py) {
                let name = self.dungeon.monsters[i].name;
                let color = self.dungeon.monsters[i].color;
                self.log.push(&[
                    panel::Span::new(name, color),
                    panel::Span::plain(" lunges at you for "),
                    panel::Span::new("2", rgb(220, 90, 84)),
                    panel::Span::plain("."),
                ]);
                self.health = (self.health - 0.04).max(0.0);
            } else if self.dungeon.is_floor(nx, ny)
                && !self.dungeon.monsters.iter().any(|m| (m.x, m.y) == (nx, ny))
            {
                self.dungeon.monsters[i].x = nx;
                self.dungeon.monsters[i].y = ny;
            }
        }

        // Hunger, thirst, and fatigue oscillate slowly rather than draining
        // monotonically: this demo has no food, drink, or rest to restore
        // them, so a one-way drain would run every gauge to zero and pin it
        // there for the rest of an unattended session, which is a worse
        // demonstration of `panel::threshold` than a gauge that actually
        // crosses all three of its color bands. Combat still costs health
        // directly (see the lunge and `attack` below), so health is the one
        // gauge a fight can actually move.
        self.hunger = 0.45f32.mul_add((self.time * 0.07).sin(), 0.5);
        self.thirst = 0.45f32.mul_add((self.time.mul_add(0.05, 1.7)).sin(), 0.5);
        self.fatigue = 0.4f32.mul_add((self.time.mul_add(0.03, 0.6)).sin(), 0.5);
        if self.hunger > 0.6 && self.thirst > 0.6 && self.fatigue < 0.4 {
            self.health = (self.health + 0.01).min(1.0);
        } else {
            // A slow passive regeneration regardless of the other three, so a
            // health bar emptied by a hard fight recovers before the next
            // one rather than staying pinned at zero for the rest of the run.
            self.health = (self.health + 0.002).min(1.0);
        }

        // A monster adjacent to the player fights back automatically. This
        // is what an unattended demo needs and a real roguelike does not: a
        // human player chooses to attack by walking into a monster (see
        // `move_player`'s attack branch), but nothing here is choosing
        // anything, so without this the log would fill forever with the
        // monster's side of a fight the player never joins.
        if let Some(index) = self.dungeon.monsters.iter().position(|m| {
            (m.x - self.dungeon.player.0).abs() <= 1 && (m.y - self.dungeon.player.1).abs() <= 1
        }) {
            // Cycling rather than always the first power: the Powers panel
            // exists to show a selection, and a selection that never moves on
            // its own is a static picture of a dynamic widget.
            self.selected_power = (self.selected_power + 1) % POWERS.len();
            self.attack(index);
        } else {
            self.wander_player();
        }
        self.recompute_fov();
    }

    /// Auto-explore: route to the nearest floor cell the player has never
    /// seen, anywhere in the dungeon, or wander toward the map center once
    /// every cell has been seen.
    ///
    /// Searches the *whole* dungeon rather than only within [`SIGHT_RADIUS`]
    /// of the player. `SIGHT_RADIUS` bounds what FOV reveals per step, which
    /// is a different question from what auto-explore should look for: a
    /// dungeon has corridors longer than the sight radius, so once every cell
    /// within it has been seen at least once, a search that never looks
    /// further finds nothing and falls back to the map center -- which is
    /// often a cell already visited, so the fallback check below returns
    /// immediately and the player never moves again for the rest of the run.
    /// [`DUNGEON_W`] x [`DUNGEON_H`] is small enough (2,800 cells) that
    /// scanning it once per simulation step is not a real cost.
    fn wander_player(&mut self) {
        let (px, py) = self.dungeon.player;
        let mut target = None;
        let mut best = i32::MAX;
        for y in 0..DUNGEON_H {
            for x in 0..DUNGEON_W {
                if !self.dungeon.is_floor(x, y) {
                    continue;
                }
                if self.fog[Dungeon::index(x, y)] != tilekit::fov::Visibility::Unknown {
                    continue;
                }
                let d = (x - px).abs() + (y - py).abs();
                if d < best {
                    best = d;
                    target = Some((x, y));
                }
            }
        }
        let Some(target) = target else {
            // Everything has been seen. Idle in place rather than pacing
            // back and forth to the map center forever, which would read as
            // the demo being stuck in a different, more visible way.
            return;
        };

        let dungeon = &self.dungeon;
        let route = path::find(
            Cell::new(px, py),
            Cell::new(target.0, target.1),
            DUNGEON_W,
            DUNGEON_H,
            Diagonals::Never,
            path::IMPASSABLE,
            |cell| {
                if dungeon.is_floor(cell.x, cell.y) {
                    1
                } else {
                    path::IMPASSABLE
                }
            },
        );
        // No route to that specific cell (it can happen the instant a target
        // is chosen from a diagonal-adjacent room the corridor generator
        // never actually connected): fall back to one greedy step so the
        // player still moves rather than freezing until the next target pick.
        let Some(next) = route.and_then(|p| p.steps.first().copied()) else {
            let (dx, dy) = self.dungeon.step_toward((px, py), target);
            self.try_step(px, py, px + dx, py + dy);
            return;
        };
        self.try_step(px, py, next.x, next.y);
    }

    /// Moves the player from `(px, py)` to `(nx, ny)` if that cell is floor
    /// and unoccupied, picking up any item found there.
    fn try_step(&mut self, px: i32, py: i32, nx: i32, ny: i32) {
        if (nx, ny) == (px, py)
            || !self.dungeon.is_floor(nx, ny)
            || self.dungeon.monsters.iter().any(|m| (m.x, m.y) == (nx, ny))
        {
            return;
        }
        self.dungeon.player = (nx, ny);
        if let Some(i) = self
            .dungeon
            .items
            .iter()
            .position(|it| it.x == nx && it.y == ny)
        {
            let item = self.dungeon.items.remove(i);
            self.log.push(&[
                panel::Span::plain("You find a "),
                panel::Span::new(item.name, rgb(120, 210, 220)),
                panel::Span::plain("."),
            ]);
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_player(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_player(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_player(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_player(1, 0),
                    KeyCode::Tab => self.focus = self.focus.next(),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    KeyCode::Char(' ') => self.paused = !self.paused,
                    KeyCode::Char(c @ '1'..='6') => {
                        self.selected_power = c as usize - '1' as usize;
                    }
                    _ => {}
                }
            }
        }
        true
    }

    // ── Drawing ───────────────────────────────────────────────────────────

    /// Character panel: name, class, and the four gauges.
    fn draw_character(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Character")
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Character)
            .draw(surface, area);
        // Name, class, and 4 gauge rows starting at interior row 3: the last
        // gauge lands on row 6, so 7 rows are needed. The caller sizes this
        // panel's area to match.
        if inner.height() < 7 {
            return;
        }
        let bg = panel::PANEL_BG;
        surface.print(
            (inner.left(), inner.top()),
            "Roc",
            Style::new().fg(ui::ACCENT).bg(bg),
        );
        surface.print(
            (inner.left(), inner.top() + 1),
            "Death Speaker",
            Style::new().fg(ui::DIM).bg(bg),
        );

        let gauges: [(&str, f32); 4] = [
            ("Health", self.health),
            ("Hunger", self.hunger),
            ("Thirst", self.thirst),
            ("Fatigue", 1.0 - self.fatigue),
        ];
        let label_w = 8u16;
        let bar_w = inner.width().saturating_sub(label_w + 1);
        for (row, (label, value)) in gauges.iter().enumerate() {
            let y = inner.top() + 3 + row as u16;
            if y >= inner.bottom() {
                break;
            }
            surface.print((inner.left(), y), label, Style::new().fg(ui::FG).bg(bg));
            if bar_w > 0 {
                panel::bar(
                    surface,
                    (inner.left() + label_w, y),
                    bar_w,
                    *value,
                    panel::threshold(*value),
                    scale(bg, 1.6),
                );
            }
        }
    }

    /// Powers panel: a hotkey plus name per row, the selected row inverted.
    fn draw_powers(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Powers")
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Powers)
            .draw(surface, area);
        let bg = panel::PANEL_BG;
        for (row, power) in POWERS.iter().enumerate() {
            let y = inner.top() + row as u16;
            if y >= inner.bottom() {
                break;
            }
            let selected = row == self.selected_power;
            let (row_bg, key_fg, name_fg) = if selected {
                (ui::ACCENT, panel::PANEL_BG, panel::PANEL_BG)
            } else {
                (bg, ui::ACCENT, ui::FG)
            };
            surface.fill_rect(
                Rect::new(inner.left(), y, inner.width(), 1),
                ' ',
                Style::new().bg(row_bg),
            );
            surface.print(
                (inner.left(), y),
                &format!("{}:", power.key),
                Style::new().fg(key_fg).bg(row_bg),
            );
            surface.print(
                (inner.left() + 3, y),
                power.name,
                Style::new().fg(name_fg).bg(row_bg),
            );
        }
    }

    /// Equipment panel: filled slots highlighted, empty ones dimmed.
    fn draw_equipment(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Equipment")
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Equipment)
            .draw(surface, area);
        let bg = panel::PANEL_BG;
        for (row, slot) in self.slots.iter().enumerate() {
            let y = inner.top() + row as u16;
            if y >= inner.bottom() {
                break;
            }
            if let Some(name) = slot.item {
                surface.fill_rect(
                    Rect::new(inner.left(), y, inner.width(), 1),
                    ' ',
                    Style::new().bg(scale(bg, 1.8)),
                );
                surface.print(
                    (inner.left(), y),
                    slot.label,
                    Style::new().fg(ui::DIM).bg(scale(bg, 1.8)),
                );
                let x = inner.left() + 9;
                if x < inner.right() {
                    surface.print((x, y), name, Style::new().fg(ui::FG).bg(scale(bg, 1.8)));
                }
            } else {
                surface.print(
                    (inner.left(), y),
                    slot.label,
                    Style::new().fg(ui::DIM).bg(bg),
                );
                let x = inner.left() + 9;
                if x < inner.right() {
                    surface.print(
                        (x, y),
                        "-empty-",
                        Style::new().fg(scale(ui::DIM, 0.6)).bg(bg),
                    );
                }
            }
        }
    }

    /// Threats panel: visible monsters, each with a name and a small HP bar.
    fn draw_threats(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Threats")
            .badge(&format!("{}", self.visible_monster_count()))
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Threats)
            .draw(surface, area);
        let bg = panel::PANEL_BG;

        let visible: Vec<&Monster> = self
            .dungeon
            .monsters
            .iter()
            .filter(|m| self.fog[Dungeon::index(m.x, m.y)] == tilekit::fov::Visibility::Visible)
            .collect();

        if visible.is_empty() {
            surface.print(
                (inner.left(), inner.top()),
                "(nothing in sight)",
                Style::new().fg(ui::DIM).bg(bg),
            );
            return;
        }

        for (row, monster) in visible.iter().enumerate() {
            let y = inner.top() + row as u16 * 2;
            if y + 1 >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    panel::Span::new(&monster.glyph.to_string(), monster.color),
                    panel::Span::plain(" "),
                    panel::Span::plain(monster.name),
                ],
                bg,
            );
            let frac = monster.hp as f32 / monster.max_hp.max(1) as f32;
            panel::bar(
                surface,
                (inner.left(), y + 1),
                inner.width().min(10),
                frac,
                panel::threshold(frac),
                scale(bg, 1.6),
            );
        }
    }

    fn visible_monster_count(&self) -> usize {
        self.dungeon
            .monsters
            .iter()
            .filter(|m| self.fog[Dungeon::index(m.x, m.y)] == tilekit::fov::Visibility::Visible)
            .count()
    }

    /// Party roster: a RogueNet-style list of named companions, present but
    /// not simulated -- this demo is about the panel, not about a second cast
    /// of characters to animate.
    fn draw_party(surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Party")
            .border(panel::Border::Single)
            .draw(surface, area);
        let bg = panel::PANEL_BG;
        for (row, companion) in PARTY.iter().enumerate() {
            let y = inner.top() + row as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    panel::Span::new("@", companion.color),
                    panel::Span::plain(" "),
                    panel::Span::new(companion.name, companion.color),
                ],
                bg,
            );
        }
    }

    /// The dungeon map itself: walls, floor, remembered/unexplored shading,
    /// items, monsters, and the player.
    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Dungeon")
            .badge(if self.paused { "paused" } else { "" })
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Map)
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        // Center the camera on the player, clamped so the view never shows
        // past the dungeon's own edge.
        let (px, py) = self.dungeon.player;
        self.camera_x = (px - i32::from(inner.width()) / 2)
            .clamp(0, (DUNGEON_W - i32::from(inner.width())).max(0));
        self.camera_y = (py - i32::from(inner.height()) / 2)
            .clamp(0, (DUNGEON_H - i32::from(inner.height())).max(0));

        for sy in 0..inner.height() {
            for sx in 0..inner.width() {
                let (wx, wy) = (self.camera_x + i32::from(sx), self.camera_y + i32::from(sy));
                let at = (inner.left() + sx, inner.top() + sy);
                self.draw_cell(surface, at, wx, wy);
            }
        }
    }

    fn draw_cell(&self, surface: &mut Surface<'_>, at: (u16, u16), wx: i32, wy: i32) {
        let bg = panel::PANEL_BG;
        if !Dungeon::in_bounds(wx, wy) {
            surface.put(at, ' ', Style::new().bg(bg));
            return;
        }
        let seen = self.fog[Dungeon::index(wx, wy)];
        if seen == tilekit::fov::Visibility::Unknown {
            surface.put(at, ' ', Style::new().bg(unexplored(bg)));
            return;
        }

        let floor = self.dungeon.is_floor(wx, wy);
        let ink = rgb(150, 150, 160);
        let wall_bg = if floor {
            rgb(30, 28, 34)
        } else {
            rgb(14, 13, 18)
        };

        let (mut glyph, mut fg, mut cell_bg) = if floor {
            ('\u{00b7}', scale(ink, 0.5), wall_bg)
        } else if self.dungeon.is_floor(wx, wy - 1)
            || self.dungeon.is_floor(wx, wy + 1)
            || self.dungeon.is_floor(wx - 1, wy)
            || self.dungeon.is_floor(wx + 1, wy)
        {
            // A wall cell adjacent to floor: draw it as a connected wall run.
            let mask = mask4([
                !self.dungeon.is_floor(wx, wy - 1),
                !self.dungeon.is_floor(wx + 1, wy),
                !self.dungeon.is_floor(wx, wy + 1),
                !self.dungeon.is_floor(wx - 1, wy),
            ]);
            (BOX_SINGLE[(mask & 0x0F) as usize], ink, wall_bg)
        } else {
            (' ', ink, rgb(8, 8, 10))
        };

        if seen == tilekit::fov::Visibility::Explored {
            fg = remembered(fg, bg);
            cell_bg = remembered(cell_bg, bg);
        } else {
            // Visible: items and monsters only draw here, never in a merely
            // remembered room, since remembering a room is not remembering
            // what is currently standing in it.
            if let Some(item) = self
                .dungeon
                .items
                .iter()
                .find(|it| it.x == wx && it.y == wy)
            {
                glyph = item.glyph;
                fg = rgb(120, 210, 220);
            }
            if let Some(monster) = self
                .dungeon
                .monsters
                .iter()
                .find(|m| m.x == wx && m.y == wy)
            {
                glyph = monster.glyph;
                fg = monster.color;
            }
            if (wx, wy) == self.dungeon.player {
                glyph = '@';
                fg = rgb(255, 236, 170);
            }
        }

        surface.put(at, glyph, Style::new().fg(fg).bg(cell_bg));
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Log")
            .border(panel::Border::Single)
            .draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    fn status(&self) -> String {
        format!(
            "seed {}  {}  {}",
            self.seed,
            if self.paused { "paused" } else { "exploring" },
            match self.focus {
                Focus::Map => "focus: map",
                Focus::Character => "focus: character",
                Focus::Powers => "focus: powers",
                Focus::Equipment => "focus: equipment",
                Focus::Threats => "focus: threats",
            }
        )
    }
}

impl Demo for PanelChrome {
    const NAME: &'static str = "18_panel_chrome";
    const TITLE: &'static str = "18 Panel chrome";
    const BLURB: &'static str = "A three-column roguelike interface built from ui::panel.";
    const GRID: (u16, u16) = (160, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move"),
            ("Tab", "cycle panel focus"),
            ("1-6", "select power"),
            ("Space", "pause"),
            ("R", "regenerate"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        if !self.paused {
            self.sim_accum_ms += frame.delta.as_millis() as u32;
            while self.sim_accum_ms >= SIM_STEP_MS {
                self.sim_accum_ms -= SIM_STEP_MS;
                self.simulate();
            }
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        // Responsive panel layout: the right column (Threats + Party) is the
        // first to go, since it is the least essential to "can I read the
        // dungeon"; the left column (Character/Powers/Equipment) is next,
        // since a narrow terminal cannot afford three stacked panels plus a
        // map and still be legible. Below that, the map and log alone.
        let w = content.width();
        let show_right = w >= 110;
        let show_left = w >= 80;

        let (left, rest) = if show_left {
            panel::split_left(content, 30)
        } else {
            (Rect::new(content.left(), content.top(), 0, 0), content)
        };
        let (center_and_map, right) = if show_right {
            panel::split_right(rest, 28)
        } else {
            (rest, Rect::new(rest.right(), rest.top(), 0, 0))
        };
        let (map_area, log_area) =
            panel::split_bottom(center_and_map, 8.max(center_and_map.height() / 5));

        if left.width() > 0 {
            // The Character panel needs 9 rows (2 border rows, a name row, a
            // class row, then 4 gauge rows: `draw_character` writes its last
            // gauge at its interior's row 6, so the interior needs 7 rows).
            // Powers needs 2 border rows plus one per power. Equipment gets
            // whatever is left. Fixed minimums rather than a proportional
            // split, because a panel given less than its own content needs is
            // not a smaller panel, it is a panel that silently clips its last
            // row -- which is exactly what a three-way even split of a short
            // left column did here.
            let char_h = 9u16.min(left.height());
            let (char_area, remainder) = panel::split_top(left, char_h);
            let powers_h = (POWERS.len() as u16 + 2).min(remainder.height());
            let (powers_area, equip_area) = panel::split_top(remainder, powers_h);
            self.draw_character(&mut surface, char_area);
            self.draw_powers(&mut surface, powers_area);
            self.draw_equipment(&mut surface, equip_area);
        }

        if right.width() > 0 {
            let threats_h = (right.height() * 3 / 5).max(6);
            let (threats_area, party_area) = panel::split_top(right, threats_h);
            self.draw_threats(&mut surface, threats_area);
            Self::draw_party(&mut surface, party_area);
        }

        self.draw_map(&mut surface, map_area);
        self.draw_log(&mut surface, log_area);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(PanelChrome);
