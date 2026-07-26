//! 21: Deck plan -- a starship deck as an architectural blueprint, not a map.
//!
//! Every map demo elsewhere in this gallery is drawn from above at a scale
//! where one cell is a chunk of terrain. This one is drawn at the scale of a
//! floor plan: one cell is one deck plate, walls are single-line box-drawing
//! runs, and the whole thing is labelled like a spreadsheet so a crew member
//! can be told "go to E7" and know exactly where that is. It is the layout
//! Remnant Humanity uses for its deck-plan screen, adapted here as a
//! standalone technique rather than as one panel of a larger game.
//!
//! Techniques on show:
//!
//! - **Four-sided coordinate rulers** ([`ui::panel::rulers`]): capital letters
//!   across the top and bottom, lowercase down both sides, so the map is
//!   readable from wherever the eye happens to be resting. The rulers track
//!   the map's own scroll offset, so `A`/`a` always mean the same physical
//!   plate no matter how far the deck extends past the visible panel.
//! - **BSP room generation with shared walls** ([`Deck::generate`]): the deck
//!   rectangle is recursively split until the pieces are room-sized, and a
//!   corridor is punched between each split pair. Walls are derived from
//!   adjacency (a plate is a wall iff at least one side borders a room and it
//!   is not itself inside one), not stamped twice, so two rooms sharing an
//!   edge share one wall between them.
//! - **Autotiled walls** ([`tilekit::autotile::mask4`] + `BOX_SINGLE`): each
//!   wall plate looks at its four cardinal wall neighbours and picks the
//!   matching box-drawing glyph, so corners, T-junctions, and straight runs
//!   all join cleanly with no per-room special-casing.
//! - **Bar-meter roster** ([`ui::panel::bar`] + [`ui::panel::threshold`]):
//!   each crew member gets two half-cell-precision gauges (health, O2), each
//!   colored by how urgent its value is, so a crew member in trouble is
//!   visibly in trouble without reading a number.
//! - **A typed command prompt**: an actual line editor, not a hotkey. Focus
//!   toggles between the map and the prompt with Tab; while the prompt has
//!   focus, printable characters append to a buffer, Backspace deletes,
//!   Enter submits the line as a command and writes a response into the log.
//!   The caret blinks on a timer independent of any key press, which is the
//!   detail that makes a static screenshot of a prompt read as "waiting for
//!   you" rather than as a text field that happens to be empty.
//! - **A drifting starfield**: background stars are placed by
//!   [`tilekit::noise::hash01`] rather than a per-frame RNG, so the field is
//!   stable from one frame to the next; only their brightness phase drifts,
//!   which is what makes the field read as slowly twinkling instead of
//!   reshuffling.
//!
//! ```sh
//! cargo run --example 21_deck_plan --features crossterm
//! cargo run --example 21_deck_plan --features software
//! cargo run --example 21_deck_plan --features gl
//! cargo run --example 21_deck_plan  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Log, Span};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, mask4};
use tilekit::noise::{Rng, hash01};
use tilekit::palette::rgb;

/// Deck size in plates. Large enough that the rulers matter (you cannot see
/// the whole thing labelled A-Z without scrolling) and small enough that a
/// BSP split still produces room-sized leaves rather than a maze of closets.
const DECK_W: i32 = 46;
/// See [`DECK_W`].
const DECK_H: i32 = 30;

/// Smallest a BSP leaf may be before the split stops, in plates.
const MIN_LEAF: i32 = 7;

/// How many crew members walk the deck.
const CREW_COUNT: usize = 5;

/// How many world-seconds a crew member waits in a room before choosing a new
/// destination. Long enough to read as "someone is doing something there",
/// short enough that the deck never looks abandoned.
const WANDER_PAUSE: f32 = 3.0;

/// One deck plate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Plate {
    Void,
    Floor,
    Wall,
    Door,
}

/// A generated room: its rectangle in plate space, plus what it is for.
struct Room {
    rect: Rect,
    label: &'static str,
}

/// The generated deck: plate grid plus the rooms cut into it.
struct Deck {
    plates: Vec<Plate>,
    rooms: Vec<Room>,
    /// The room index every debris/item marker sits in, paired with its
    /// local offset, so redrawing never has to re-scatter them.
    debris: Vec<(i32, i32)>,
    items: Vec<(i32, i32)>,
    /// The room considered the current mission objective, tinted in the map.
    objective: usize,
}

impl Deck {
    const fn index(x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= DECK_W || y >= DECK_H {
            return None;
        }
        Some((y * DECK_W + x) as usize)
    }

    fn plate(&self, x: i32, y: i32) -> Plate {
        Self::index(x, y).map_or(Plate::Void, |i| self.plates[i])
    }

    /// Recursively splits `area` until leaves are room-sized, then carves a
    /// room inset by one plate inside each leaf (the outer ring becomes wall)
    /// and a corridor between the two children of every split.
    ///
    /// BSP rather than a hand-authored layout because it is the standard
    /// technique for guaranteeing every room is reachable: a corridor is cut
    /// at every split, so the recursion tree itself is a spanning tree of the
    /// rooms with no separate connectivity pass needed.
    fn generate(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let mut plates = vec![Plate::Void; (DECK_W * DECK_H) as usize];
        let root = Rect::new(1, 1, (DECK_W - 2) as u16, (DECK_H - 2) as u16);
        let mut leaves = Vec::new();
        split(root, &mut rng, &mut leaves);

        // Carve one room per leaf, inset so a one-plate margin separates
        // neighbouring rooms even before walls are derived from adjacency.
        let mut room_rects = Vec::new();
        for leaf in &leaves {
            let margin_w = (leaf.width() / 6).max(1);
            let margin_h = (leaf.height() / 6).max(1);
            if leaf.width() <= margin_w * 2 + 2 || leaf.height() <= margin_h * 2 + 2 {
                continue;
            }
            let rect = Rect::new(
                leaf.left() + margin_w,
                leaf.top() + margin_h,
                leaf.width() - margin_w * 2,
                leaf.height() - margin_h * 2,
            );
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    if let Some(i) = Self::index(i32::from(x), i32::from(y)) {
                        plates[i] = Plate::Floor;
                    }
                }
            }
            room_rects.push(rect);
        }

        let names: [&str; 8] = [
            "Bridge",
            "Engineering",
            "Med Bay",
            "Cargo Hold",
            "Barracks",
            "Armory",
            "Reactor",
            "Galley",
        ];
        let rooms: Vec<Room> = room_rects
            .into_iter()
            .enumerate()
            .map(|(i, rect)| Room {
                rect,
                label: names[i % names.len()],
            })
            .collect();

        // Corridors: a straight run of floor connecting each pair of
        // cardinally adjacent rooms, carved before walls are derived so the
        // wall pass below simply encloses whatever floor already exists
        // rather than needing a second pass to punch through it. Doing it
        // the other way round -- deriving walls first, then trying to open a
        // single doorway plate through a gap that might be several plates of
        // solid void -- cannot connect anything wider than one plate.
        let rects: Vec<Rect> = rooms.iter().map(|r| r.rect).collect();
        let mut doors = Vec::new();
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                if let Some(corridor) = shared_wall_span(rects[i], rects[j], &mut rng) {
                    for (x, y) in corridor.plates() {
                        if let Some(idx) = Self::index(x, y) {
                            plates[idx] = Plate::Floor;
                        }
                    }
                    doors.push(corridor.door_a);
                    doors.push(corridor.door_b);
                }
            }
        }

        // Walls: a plate is a wall iff it is not floor and touches a floor
        // plate on at least one of its four sides. Anything not floor and not
        // adjacent to floor stays void (drawn as open space between rooms),
        // which is what stops two nearby-but-unconnected rooms from
        // acquiring a solid perimeter that then has to be punched through
        // twice.
        for y in 0..DECK_H {
            for x in 0..DECK_W {
                let Some(i) = Self::index(x, y) else {
                    continue;
                };
                if plates[i] == Plate::Floor {
                    continue;
                }
                let touches_floor = [(0, -1), (1, 0), (0, 1), (-1, 0)].iter().any(|&(dx, dy)| {
                    matches!(
                        Self::index(x + dx, y + dy).map(|j| plates[j]),
                        Some(Plate::Floor)
                    )
                });
                if touches_floor {
                    plates[i] = Plate::Wall;
                }
            }
        }

        // The two plates where each corridor crosses a room's own wall ring
        // become doors, drawn over the wall the pass above just placed there.
        for (x, y) in doors {
            if let Some(idx) = Self::index(x, y) {
                plates[idx] = Plate::Door;
            }
        }

        let mut deck = Self {
            plates,
            rooms,
            debris: Vec::new(),
            items: Vec::new(),
            objective: 0,
        };
        deck.scatter(&mut rng);
        deck.objective = if deck.rooms.is_empty() {
            0
        } else {
            rng.next_below(deck.rooms.len() as u32) as usize
        };
        deck
    }

    /// Scatters debris (`x`) and item (`$`) markers across floor plates,
    /// keeping stable positions for the demo's lifetime.
    fn scatter(&mut self, rng: &mut Rng) {
        for room in &self.rooms {
            let area = i32::from(room.rect.width()) * i32::from(room.rect.height());
            let debris_count = (area / 14).clamp(0, 3);
            let item_count = i32::from(u8::from(rng.next_f32() < 0.6));
            for _ in 0..debris_count {
                let x = i32::from(room.rect.left())
                    + rng.next_below(u32::from(room.rect.width())) as i32;
                let y = i32::from(room.rect.top())
                    + rng.next_below(u32::from(room.rect.height())) as i32;
                self.debris.push((x, y));
            }
            for _ in 0..item_count {
                let x = i32::from(room.rect.left())
                    + rng.next_below(u32::from(room.rect.width())) as i32;
                let y = i32::from(room.rect.top())
                    + rng.next_below(u32::from(room.rect.height())) as i32;
                self.items.push((x, y));
            }
        }
    }

    /// The centre plate of room `index`, for spawning crew and the player.
    fn room_center(&self, index: usize) -> (i32, i32) {
        let r = self.rooms[index].rect;
        (
            i32::from(r.left()) + i32::from(r.width()) / 2,
            i32::from(r.top()) + i32::from(r.height()) / 2,
        )
    }
}

/// Recursively splits `area` into leaves at least [`MIN_LEAF`] wide/tall on
/// both axes, alternating split orientation by whichever dimension is more
/// elongated so leaves stay roughly room-shaped rather than degenerating into
/// slivers.
fn split(area: Rect, rng: &mut Rng, leaves: &mut Vec<Rect>) {
    let min: u16 = MIN_LEAF.try_into().unwrap_or(u16::MAX);
    let can_split_w = area.width() > min * 2;
    let can_split_h = area.height() > min * 2;
    if !can_split_w && !can_split_h {
        leaves.push(area);
        return;
    }

    let split_horizontally = if can_split_w && can_split_h {
        area.width() > area.height()
    } else {
        can_split_w
    };

    if split_horizontally {
        let span = area.width() - min * 2;
        let at = min + rng.next_below(u32::from(span.max(1))) as u16;
        let left = Rect::new(area.left(), area.top(), at, area.height());
        let right = Rect::new(
            area.left() + at,
            area.top(),
            area.width() - at,
            area.height(),
        );
        split(left, rng, leaves);
        split(right, rng, leaves);
    } else {
        let span = area.height() - min * 2;
        let at = min + rng.next_below(u32::from(span.max(1))) as u16;
        let top = Rect::new(area.left(), area.top(), area.width(), at);
        let bottom = Rect::new(
            area.left(),
            area.top() + at,
            area.width(),
            area.height() - at,
        );
        split(top, rng, leaves);
        split(bottom, rng, leaves);
    }
}

/// A straight corridor connecting two rooms, plus the one plate on each end
/// that lands inside the room's own wall ring and should become a door.
struct Corridor {
    /// The corridor's own floor plates, not including either endpoint.
    span: (i32, i32, i32, i32), // (x0, y0, x1, y1) inclusive
    door_a: (i32, i32),
    door_b: (i32, i32),
}

impl Corridor {
    /// Every plate the corridor should set to floor, endpoints included: the
    /// endpoints get overwritten to `Door` afterward, but they must be floor
    /// first so the wall-derivation pass does not also try to wall them in.
    fn plates(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        let (x0, y0, x1, y1) = self.span;
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
    }
}

/// Finds a straight corridor connecting two rooms, if they are cardinally
/// adjacent, i.e. their spans overlap on one axis while sitting back to back
/// with a small gap on the other.
///
/// Carving a corridor rather than a single doorway plate is what makes this
/// work for any gap width the BSP margins produce: two rooms separated by
/// several plates of void need a run of floor crossing all of them, not one
/// plate that would sit surrounded by void on both sides.
fn shared_wall_span(a: Rect, b: Rect, rng: &mut Rng) -> Option<Corridor> {
    // How wide a gap between two rooms still counts as adjacent enough to
    // connect. Wide enough to bridge the BSP inset margins (a handful of
    // plates), not so wide that distant rooms get linked.
    const MAX_GAP: i32 = 5;

    let (al, at, ar, ab) = (
        i32::from(a.left()),
        i32::from(a.top()),
        i32::from(a.right()),
        i32::from(a.bottom()),
    );
    let (bl, bt, br, bb) = (
        i32::from(b.left()),
        i32::from(b.top()),
        i32::from(b.right()),
        i32::from(b.bottom()),
    );

    // Rooms side by side: a horizontal corridor at some row within the
    // shared vertical span, crossing the gap between them left to right.
    let vertical_overlap = at.max(bt)..ab.min(bb);
    if !vertical_overlap.is_empty() {
        let (gap_lo, gap_hi) = if ar <= bl {
            (ar, bl)
        } else if br <= al {
            (br, al)
        } else {
            (0, 0) // rooms overlap on x; not a side-by-side pair
        };
        if gap_hi > gap_lo && gap_hi - gap_lo <= MAX_GAP {
            let y0 = vertical_overlap.start;
            let y1 = vertical_overlap.end - 1;
            let span: u32 = (y1 - y0 + 1).try_into().unwrap_or(1);
            let y = y0 + rng.next_below(span) as i32;
            return Some(Corridor {
                span: (gap_lo, y, gap_hi - 1, y),
                door_a: (gap_lo, y),
                door_b: (gap_hi - 1, y),
            });
        }
    }

    // Rooms stacked vertically: the mirror of the above on the other axis.
    let horizontal_overlap = al.max(bl)..ar.min(br);
    if !horizontal_overlap.is_empty() {
        let (gap_lo, gap_hi) = if ab <= bt {
            (ab, bt)
        } else if bb <= at {
            (bb, at)
        } else {
            (0, 0) // rooms overlap on y; not a stacked pair
        };
        if gap_hi > gap_lo && gap_hi - gap_lo <= MAX_GAP {
            let x0 = horizontal_overlap.start;
            let x1 = horizontal_overlap.end - 1;
            let span: u32 = (x1 - x0 + 1).try_into().unwrap_or(1);
            let x = x0 + rng.next_below(span) as i32;
            return Some(Corridor {
                span: (x, gap_lo, x, gap_hi - 1),
                door_a: (x, gap_lo),
                door_b: (x, gap_hi - 1),
            });
        }
    }

    None
}

/// A crew member's role, which colors their roster tag.
#[derive(Clone, Copy)]
enum Role {
    Eng,
    Med,
    Tac,
}

impl Role {
    const fn tag(self) -> &'static str {
        match self {
            Self::Eng => "ENG",
            Self::Med => "MED",
            Self::Tac => "TAC",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Eng => rgb(214, 156, 82),
            Self::Med => rgb(120, 196, 158),
            Self::Tac => rgb(196, 108, 108),
        }
    }
}

/// A crew member wandering the deck.
struct Crew {
    name: &'static str,
    role: Role,
    x: i32,
    y: i32,
    target_room: usize,
    wait: f32,
    health: f32,
    o2: f32,
}

impl Crew {
    /// Advances one wander step: count down the pause, then either walk one
    /// plate toward the target room's centre or, on arrival, pick a new
    /// target and pause again.
    fn wander(&mut self, deck: &Deck, dt: f32, rng: &mut Rng) {
        self.wait -= dt;
        if self.wait > 0.0 {
            return;
        }
        let (tx, ty) = deck.room_center(self.target_room);
        if self.x == tx && self.y == ty {
            self.target_room = rng.next_below(deck.rooms.len().max(1) as u32) as usize;
            self.wait = WANDER_PAUSE * (0.5 + rng.next_f32());
            return;
        }
        // One cardinal step toward the target per pause, so movement reads as
        // discrete steps between plates rather than a continuous glide -- the
        // right feel for a deck plan, which is a blueprint, not an animation.
        let (dx, dy) = (tx - self.x, ty - self.y);
        if dx.abs() >= dy.abs() && dx != 0 {
            self.x += dx.signum();
        } else if dy != 0 {
            self.y += dy.signum();
        }
        self.wait = 0.35;
    }
}

/// Which panel currently receives keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Map,
    Prompt,
}

/// State: the generated deck, the player, the crew, the log, the command
/// prompt, and the camera scroll offset.
pub struct DeckPlan {
    deck: Deck,
    seed: u32,
    player: (i32, i32),
    crew: Vec<Crew>,
    log: Log,
    prompt: String,
    caret_on: bool,
    focus: Focus,
    time: f32,
    scroll: (i32, i32),
    fps: FpsMeter,
}

impl Default for DeckPlan {
    fn default() -> Self {
        let seed = 14;
        let deck = Deck::generate(seed);
        let player = deck.room_center(deck.rooms.len().saturating_sub(1));

        let mut rng = Rng::new(seed ^ 0x00C0_FFEE);
        let crew_names = ["John", "Peter", "George", "Amara", "Voss"];
        let roles = [Role::Eng, Role::Med, Role::Tac, Role::Eng, Role::Tac];
        let crew = (0..CREW_COUNT)
            .map(|i| {
                let room = rng.next_below(deck.rooms.len().max(1) as u32) as usize;
                let (x, y) = deck.room_center(room);
                Crew {
                    name: crew_names[i % crew_names.len()],
                    role: roles[i % roles.len()],
                    x,
                    y,
                    target_room: room,
                    wait: WANDER_PAUSE * rng.next_f32(),
                    health: 0.3f32.mul_add(rng.next_f32(), 0.7),
                    o2: 0.4f32.mul_add(rng.next_f32(), 0.6),
                }
            })
            .collect();

        let mut log = Log::new(64);
        log.push("Mission started.", ui::FG);
        log.push(
            format!("Objective: secure {}.", deck.rooms[deck.objective].label),
            ui::ACCENT,
        );
        log.push("Type 'help' for a list of commands.", ui::DIM);

        Self {
            deck,
            seed,
            player,
            crew,
            log,
            prompt: String::new(),
            caret_on: true,
            focus: Focus::Map,
            time: 0.0,
            scroll: (0, 0),
            fps: FpsMeter::new(),
        }
    }
}

impl DeckPlan {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        *self = Self {
            seed: self.seed,
            ..Self::default()
        };
        // `Self::default()` reseeds from the literal 14, so force the new
        // seed through a fresh generation rather than keeping the reroll from
        // ever changing anything.
        self.deck = Deck::generate(self.seed);
        self.player = self
            .deck
            .room_center(self.deck.rooms.len().saturating_sub(1));
        self.log
            .push(format!("Deck regenerated (seed {}).", self.seed), ui::DIM);
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            match event {
                Event::Key(key) if key.is_down() => {
                    if self.focus == Focus::Prompt {
                        if self.handle_prompt_key(key.code) {
                            continue;
                        }
                        // Escape falls through to quit even with the prompt
                        // focused, so there is always a way out that does not
                        // depend on remembering to tab away first.
                        if matches!(key.code, KeyCode::Escape) {
                            return false;
                        }
                        continue;
                    }
                    if ui::is_quit(&event) {
                        return false;
                    }
                    self.handle_map_key(key.code);
                }
                Event::Close => return false,
                _ => {}
            }
        }
        true
    }

    /// Handles one key while the prompt has focus. Returns `true` if it was
    /// consumed as prompt input (including Tab, which hands focus back).
    fn handle_prompt_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Tab => {
                self.focus = Focus::Map;
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.prompt.push(c);
                true
            }
            KeyCode::Backspace => {
                self.prompt.pop();
                true
            }
            KeyCode::Enter => {
                self.submit_prompt();
                true
            }
            _ => false,
        }
    }

    fn handle_map_key(&mut self, code: KeyCode) {
        let (dx, dy) = match code {
            KeyCode::Up | KeyCode::Char('w' | 'W') => (0, -1),
            KeyCode::Down | KeyCode::Char('s' | 'S') => (0, 1),
            KeyCode::Left | KeyCode::Char('a' | 'A') => (-1, 0),
            KeyCode::Right | KeyCode::Char('d' | 'D') => (1, 0),
            KeyCode::Tab => {
                self.focus = Focus::Prompt;
                return;
            }
            KeyCode::Char('r' | 'R') => {
                self.reroll();
                return;
            }
            _ => return,
        };
        let (nx, ny) = (self.player.0 + dx, self.player.1 + dy);
        if !matches!(self.deck.plate(nx, ny), Plate::Void) {
            self.player = (nx, ny);
        }
    }

    /// Runs the typed line as a command, clears the buffer, and writes a
    /// response into the log. A small fixed set rather than a real parser:
    /// the point on show is the line-editing interaction, not a command
    /// language, so the handlers only need to be plausible.
    fn submit_prompt(&mut self) {
        let line = core::mem::take(&mut self.prompt);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        self.log.push(format!("> {trimmed}"), ui::DIM);

        let mut words = trimmed.split_whitespace();
        match words.next().unwrap_or("").to_ascii_lowercase().as_str() {
            "help" => {
                self.log
                    .push("Commands: help, status, scan, restart.", ui::ACCENT);
            }
            "status" => {
                for crew in &self.crew {
                    self.log.push(
                        format!(
                            "{} ({}): health {:.0}%, O2 {:.0}%",
                            crew.name,
                            crew.role.tag(),
                            crew.health * 100.0,
                            crew.o2 * 100.0
                        ),
                        crew.role.color(),
                    );
                }
            }
            "scan" => {
                let room = &self.deck.rooms[self.deck.objective];
                self.log.push(
                    format!(
                        "Objective room: {} at ({}, {}).",
                        room.label,
                        room.rect.left(),
                        room.rect.top()
                    ),
                    ui::ACCENT,
                );
            }
            "restart" => {
                self.reroll();
            }
            other => {
                self.log.push(
                    format!("Unknown command: '{other}'. Try 'help'."),
                    rgb(200, 100, 90),
                );
            }
        }
    }

    /// Advances crew wander and O2 drain by `dt` world-seconds.
    fn simulate(&mut self, dt: f32) {
        let mut rng = Rng::new((self.time * 1000.0) as u32 ^ self.seed);
        let deck = &self.deck;
        for crew in &mut self.crew {
            crew.wander(deck, dt, &mut rng);
        }
        // O2 drains slowly and recovers a little near the objective room
        // (read as: life support is prioritized there), which is enough to
        // make the gauges move independently rather than only ever falling.
        let (ox, oy) = self.deck.room_center(self.deck.objective);
        for crew in &mut self.crew {
            let near_objective = (crew.x - ox).abs() + (crew.y - oy).abs() < 6;
            let rate = if near_objective { 0.004 } else { -0.01 };
            crew.o2 = (crew.o2 + rate * dt).clamp(0.0, 1.0);
        }
    }

    /// The visible map panel width/height needed for the current deck, capped
    /// so the sidebar always keeps at least a usable width.
    fn map_panel_rect(content: Rect, stack_vertically: bool) -> (Rect, Rect) {
        if stack_vertically {
            // The sidebar stacks four panels (mission, roster, log, prompt),
            // each needing at least 3 rows to show a single line of content
            // inside its own border, so it needs at least 12 regardless of
            // how tall the map would like to be. Reserving that first, and
            // giving the map the usual 60% only when there is room to spare
            // beyond it, is what keeps every sidebar panel legible instead of
            // collapsing to a bare frame under a short terminal.
            const SIDE_MIN: u16 = 12;
            let ideal_map_h = content.height() * 3 / 5;
            let map_h = ideal_map_h.min(content.height().saturating_sub(SIDE_MIN));
            panel::split_top(content, map_h)
        } else {
            let side_w = 40u16.min(content.width().saturating_sub(30));
            panel::split_right(content, side_w)
        }
    }

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = panel::Panel::new()
            .title("Deck 4 -- Plan View")
            .border(panel::Border::Double)
            .focused(self.focus == Focus::Map);
        let inner = panel.draw(surface, area);
        if inner.width() < 4 || inner.height() < 4 {
            return;
        }

        // Rulers need one row/column of margin on every side, so the actual
        // plate viewport is one cell smaller than the panel interior.
        let map_area = Rect::new(
            inner.left() + 1,
            inner.top() + 1,
            inner.width() - 2,
            inner.height() - 2,
        );
        self.draw_starfield(surface, inner);
        self.draw_plates(surface, map_area);
        self.draw_rulers(surface, map_area);
    }

    /// A stable field of background stars: position comes from a hash of grid
    /// coordinates (so it never reshuffles), brightness comes from a slow
    /// per-star phase offset added to elapsed time (so it visibly drifts).
    fn draw_starfield(&self, surface: &mut Surface<'_>, area: Rect) {
        for y in 0..area.height() {
            for x in 0..area.width() {
                let wx = i32::from(x);
                let wy = i32::from(y);
                if hash01(0x5741, wx, wy) > 0.05 {
                    continue;
                }
                let phase = hash01(0x1357, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add((self.time.mul_add(0.6, phase)).sin(), 0.5);
                let glyph = if hash01(0x9911, wx, wy) > 0.5 {
                    '.'
                } else {
                    '\u{00b7}'
                };
                let v = 120.0f32.mul_add(twinkle, 60.0) as u8;
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new()
                        .fg(rgb(v, v, v.saturating_add(20)))
                        .bg(rgb(3, 3, 8)),
                );
            }
        }
    }

    fn draw_plates(&self, surface: &mut Surface<'_>, area: Rect) {
        let (ox, oy) = self.scroll;
        let objective_rect = self.deck.rooms.get(self.deck.objective).map(|r| r.rect);

        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let (wx, wy) = (ox + i32::from(sx), oy + i32::from(sy));
                let at = (area.left() + sx, area.top() + sy);
                let plate = self.deck.plate(wx, wy);
                if plate == Plate::Void {
                    continue; // leave the starfield showing through
                }

                let in_objective = objective_rect.is_some_and(|r| {
                    r.contains(wx.try_into().unwrap_or(0), wy.try_into().unwrap_or(0))
                });

                let (glyph, fg, bg) = match plate {
                    Plate::Floor => {
                        let base_bg = if in_objective {
                            rgb(58, 62, 24)
                        } else {
                            rgb(18, 20, 28)
                        };
                        ('\u{00b7}', rgb(90, 96, 110), base_bg)
                    }
                    Plate::Wall => {
                        // The glyph mask connects to other wall (or door)
                        // neighbours only, not to the floor inside the room
                        // the wall encloses: a plain wall run bordering floor
                        // on one side must still draw as a straight `─`, not
                        // as a T-junction just because a room happens to sit
                        // behind it. Floor adjacency already decided this
                        // plate *is* a wall (see `Deck::generate`); it plays
                        // no further part in choosing which wall glyph.
                        let connects = |p: Plate| matches!(p, Plate::Wall | Plate::Door);
                        let mask = mask4([
                            connects(self.deck.plate(wx, wy - 1)),
                            connects(self.deck.plate(wx + 1, wy)),
                            connects(self.deck.plate(wx, wy + 1)),
                            connects(self.deck.plate(wx - 1, wy)),
                        ]);
                        (
                            BOX_SINGLE[(mask & 0x0F) as usize],
                            rgb(200, 202, 210),
                            rgb(10, 11, 16),
                        )
                    }
                    Plate::Door => ('+', rgb(226, 190, 110), rgb(28, 24, 14)),
                    Plate::Void => unreachable!("filtered above"),
                };
                surface.put(at, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        for &(dx, dy) in &self.deck.debris {
            self.draw_marker(surface, area, (dx, dy), 'x', rgb(150, 130, 110));
        }
        for &(dx, dy) in &self.deck.items {
            self.draw_marker(surface, area, (dx, dy), '$', rgb(230, 200, 90));
        }
        for (i, crew) in self.crew.iter().enumerate() {
            let digit = char::from(b'1' + (i.min(8) as u8));
            // Inverted video: the role color as background, a dark digit on
            // top, so a crew token reads as a solid tag rather than as a
            // colored letter competing with the floor tint behind it.
            self.draw_marker_styled(
                surface,
                area,
                (crew.x, crew.y),
                digit,
                Style::new().fg(rgb(10, 10, 12)).bg(crew.role.color()),
            );
        }
        self.draw_marker_styled(
            surface,
            area,
            self.player,
            '@',
            Style::new().fg(rgb(226, 90, 90)).bg(rgb(28, 12, 12)),
        );
    }

    fn draw_marker(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        world: (i32, i32),
        glyph: char,
        color: Color,
    ) {
        let bg = if matches!(self.deck.plate(world.0, world.1), Plate::Floor) {
            rgb(18, 20, 28)
        } else {
            rgb(10, 11, 16)
        };
        self.draw_marker_styled(surface, area, world, glyph, Style::new().fg(color).bg(bg));
    }

    fn draw_marker_styled(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        world: (i32, i32),
        glyph: char,
        style: Style,
    ) {
        let (sx, sy) = (world.0 - self.scroll.0, world.1 - self.scroll.1);
        if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
            return;
        }
        surface.put(
            (area.left() + sx as u16, area.top() + sy as u16),
            glyph,
            style,
        );
    }

    /// Builds the ruler label sequences and draws them, offset by the current
    /// scroll so the letters always name the same physical plate.
    fn draw_rulers(&self, surface: &mut Surface<'_>, map_area: Rect) {
        let cols: Vec<char> = (0..map_area.width())
            .map(|i| letter((i32::from(i) + self.scroll.0).rem_euclid(26), true))
            .collect();
        let rows: Vec<char> = (0..map_area.height())
            .map(|i| letter((i32::from(i) + self.scroll.1).rem_euclid(26), false))
            .collect();
        panel::rulers(surface, map_area, &cols, &rows, 1, ui::DIM, panel::PANEL_BG);
    }

    fn draw_sidebar(&self, surface: &mut Surface<'_>, area: Rect, stack_vertically: bool) {
        // Budgeted smallest-first, each claim capped by what is actually left
        // rather than by a fixed constant, so a short terminal shrinks every
        // panel instead of only the last one asked for. A panel needs at
        // least 3 rows to show even one line of content (two go to its own
        // top/bottom border); a claim below that rounds to 0 and the panel
        // draws nothing, which `Panel::draw` already handles on an empty
        // rect rather than underflowing.
        /// Minimum log rows to keep the panel worth having, below which the
        /// roster should shrink to its compact layout instead of crowding it
        /// out entirely.
        const LOG_MIN: u16 = 6;

        let mut remaining = area.height();

        let mission_h = mission_want(stack_vertically).min(remaining);
        remaining -= mission_h;
        let prompt_h = 3u16.min(remaining);
        remaining -= prompt_h;

        // The roster prefers the full three-rows-per-crew layout (see
        // `draw_roster`), but reserving that unconditionally would starve the
        // log on a merely average-height terminal even though the full
        // layout is not actually needed there. So it claims the full amount
        // only if enough remains to also leave the log a usable minimum;
        // otherwise it falls back to the one-row-per-crew compact claim,
        // which is the same threshold `draw_roster` itself switches on.
        let roster_full = 2 + self.crew.len() as u16 * 3;
        let roster_compact = 2 + self.crew.len() as u16;
        let roster_wanted = if remaining.saturating_sub(roster_full) >= LOG_MIN {
            roster_full
        } else {
            roster_compact
        };
        let roster_h = roster_wanted.min(remaining);
        remaining -= roster_h;
        let log_h = remaining;

        let (mission_area, rest) = panel::split_top(area, mission_h);
        let (roster_area, rest) = panel::split_top(rest, roster_h);
        let (log_area, prompt_area) = panel::split_top(rest, log_h);
        debug_assert_eq!(
            prompt_area.height(),
            prompt_h,
            "the budget must account for every row"
        );

        self.draw_mission(surface, mission_area);
        self.draw_roster(surface, roster_area);
        self.draw_log(surface, log_area);
        self.draw_prompt(surface, prompt_area);
    }

    fn draw_mission(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Mission").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let objective = self.deck.rooms[self.deck.objective].label;
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::plain("Secure "),
                Span::keyword(objective),
                Span::plain(" and hold position."),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 2 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[Span::dim("Crew status is in the roster below.")],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_roster(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Crew")
            .badge(&format!("{}", self.crew.len()))
            .draw(surface, area);
        if inner.width() < 10 || inner.height() == 0 || self.crew.is_empty() {
            return;
        }

        // Three rows per crew member (name, HP bar, O2 bar) is the readable
        // layout, but a narrow sidebar under a short terminal cannot always
        // afford it. Rather than silently clipping crew off the bottom, drop
        // to one row per member -- name plus both bars packed side by side --
        // once there is not enough height for everyone at full size. Losing
        // label precision is a smaller loss than losing crew members outright.
        let wants = self.crew.len() as u16 * 3;
        if inner.height() >= wants {
            self.draw_roster_full(surface, inner);
        } else {
            self.draw_roster_compact(surface, inner);
        }
    }

    fn draw_roster_full(&self, surface: &mut Surface<'_>, inner: Rect) {
        let bar_w = inner.width().saturating_sub(11).max(4);
        for (i, crew) in self.crew.iter().enumerate() {
            let y0 = inner.top() + i as u16 * 3;
            if y0 + 2 >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y0),
                inner.width(),
                &[
                    Span::new(crew.role.tag(), crew.role.color()),
                    Span::plain(" "),
                    Span::keyword(crew.name),
                ],
                panel::PANEL_BG,
            );
            panel::spans(
                surface,
                (inner.left(), y0 + 1),
                6,
                &[Span::dim("HP ")],
                panel::PANEL_BG,
            );
            panel::bar(
                surface,
                (inner.left() + 3, y0 + 1),
                bar_w,
                crew.health,
                panel::threshold(crew.health),
                rgb(30, 30, 36),
            );
            panel::spans(
                surface,
                (inner.left(), y0 + 2),
                6,
                &[Span::dim("O2 ")],
                panel::PANEL_BG,
            );
            panel::bar(
                surface,
                (inner.left() + 3, y0 + 2),
                bar_w,
                crew.o2,
                panel::threshold(crew.o2),
                rgb(30, 30, 36),
            );
        }
    }

    /// One row per crew member: a one-letter role tag, then two short bars.
    /// Used when the sidebar is too short for [`draw_roster_full`]'s three
    /// rows per member; see [`draw_roster`].
    fn draw_roster_compact(&self, surface: &mut Surface<'_>, inner: Rect) {
        if inner.width() < 14 || inner.height() == 0 {
            return;
        }
        let bar_w = ((inner.width().saturating_sub(6)) / 2).max(3);

        // If even one row per crew member does not fit, the last visible row
        // becomes a count of how many are not shown, so a badly squeezed
        // window says "2 more" instead of silently dropping crew with no
        // indication anything is missing.
        let rows = usize::from(inner.height());
        let (shown, overflow) = if self.crew.len() > rows {
            (rows.saturating_sub(1), self.crew.len() - (rows - 1))
        } else {
            (self.crew.len(), 0)
        };

        for (i, crew) in self.crew.iter().take(shown).enumerate() {
            let y = inner.top() + i as u16;
            let tag = &crew.role.tag()[..1];
            panel::spans(
                surface,
                (inner.left(), y),
                4,
                &[Span::new(tag, crew.role.color())],
                panel::PANEL_BG,
            );
            panel::bar(
                surface,
                (inner.left() + 1, y),
                bar_w,
                crew.health,
                panel::threshold(crew.health),
                rgb(30, 30, 36),
            );
            panel::bar(
                surface,
                (inner.left() + 2 + bar_w, y),
                bar_w,
                crew.o2,
                panel::threshold(crew.o2),
                rgb(30, 30, 36),
            );
        }

        if overflow > 0 {
            let y = inner.top() + shown as u16;
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(&format!("+{overflow} more"))],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Log").draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    fn draw_prompt(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Command")
            .focused(self.focus == Focus::Prompt)
            .draw(surface, area);
        if inner.height() == 0 || inner.width() < 4 {
            return;
        }
        let bg = panel::PANEL_BG;
        let prefix = "> ";
        let room = inner.width_usize().saturating_sub(prefix.len() + 1);
        // Show the tail of the buffer if it has grown past the visible width,
        // so a long command stays legible instead of running off the edge.
        let visible: String = if self.prompt.chars().count() > room {
            self.prompt
                .chars()
                .skip(self.prompt.chars().count() - room)
                .collect()
        } else {
            self.prompt.clone()
        };
        surface.print(
            (inner.left(), inner.top()),
            prefix,
            Style::new().fg(ui::ACCENT).bg(bg),
        );
        surface.print(
            (inner.left() + prefix.len() as u16, inner.top()),
            &visible,
            Style::new().fg(ui::FG).bg(bg),
        );
        if self.caret_on && self.focus == Focus::Prompt {
            let cx = inner.left() + prefix.len() as u16 + visible.chars().count() as u16;
            if cx < inner.right() {
                surface.put(
                    (cx, inner.top()),
                    '\u{2588}',
                    Style::new().fg(ui::ACCENT).bg(bg),
                );
            }
        }
    }

    fn status(&self) -> String {
        format!(
            "seed {}  focus: {}  crew {}",
            self.seed,
            match self.focus {
                Focus::Map => "map",
                Focus::Prompt => "prompt",
            },
            self.crew.len()
        )
    }
}

/// Rows the mission panel claims: one line less when the sidebar is stacked
/// under a short map, since a stacked layout is already the tightest case.
const fn mission_want(stack_vertically: bool) -> u16 {
    if stack_vertically { 4 } else { 5 }
}

/// The `n`-th ruler letter, `A`/`a`-based and wrapping every 26.
fn letter(n: i32, upper: bool) -> char {
    let base = if upper { b'A' } else { b'a' };
    char::from(base + (n.rem_euclid(26)) as u8)
}

impl Demo for DeckPlan {
    const NAME: &'static str = "21_deck_plan";
    const TITLE: &'static str = "21 Deck Plan";
    const BLURB: &'static str =
        "A starship deck as a labelled blueprint, with a typed command line.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "switch focus"),
            ("WASD/arrows", "move (map focus)"),
            ("type/Enter", "command (prompt focus)"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        // The caret blinks on its own clock, independent of input, so a
        // screenshot of an idle prompt still reads as "waiting for you" and
        // not as a dead text field.
        self.caret_on = (self.time * 1.6).fract() < 0.5;

        if !self.handle_events(term) {
            return false;
        }
        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let stack_vertically = content.width() < 110;
        let show_sidebar = content.width() >= 85 || stack_vertically;

        if show_sidebar {
            let (map_area, side_area) = Self::map_panel_rect(content, stack_vertically);
            self.draw_map(&mut surface, map_area);
            self.draw_sidebar(&mut surface, side_area, stack_vertically);
        } else {
            self.draw_map(&mut surface, content);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(DeckPlan);
