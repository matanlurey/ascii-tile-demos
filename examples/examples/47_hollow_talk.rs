//! 47: Hollow Talk -- in-world speech balloons with pointer tails.
//!
//! Zorbus keeps its dungeon crawler's dialogue in two places at once: a
//! scrolling text log at the bottom, and a bordered balloon floating over
//! whichever monster or NPC is talking, with a tail pointing back down at the
//! speaker. Every other tile-dungeon demo in this gallery (`24_torchlit_crypt`
//! for lighting, `35_stealth_grid` for patrol AI) treats the map itself as the
//! whole show; this one treats the map as a stage the balloons perform on top
//! of, which is the one thing neither of those demos does. There is
//! deliberately no scrolling log here -- the balloon *is* the log entry, drawn
//! where the eye is already looking instead of at the bottom of the screen
//! where it has to travel to be read.
//!
//! Techniques on show:
//!
//! - **Placed, non-overlapping speech balloons** ([`place_balloon`]): each
//!   balloon starts centered above its speaker, but is nudged sideways to stay
//!   inside the map panel when the speaker is near an edge, and nudged
//!   further from the speaker (stacking upward or downward) when it would
//!   collide with an already-placed balloon this frame. The nudging is
//!   resolved fresh every frame in speaker order, so it is a pure function of
//!   current positions rather than retained state that could drift out of
//!   sync with them.
//! - **Tails that bend instead of clip** ([`draw_tail`]): the tail is a short
//!   run of diagonal box-drawing-adjacent glyphs (`/`, `\`, `|`) linearly
//!   interpolated between the balloon's attachment column and the speaker's
//!   head, so a balloon shoved sideways to fit the panel still visibly points
//!   at who is talking instead of pointing at empty air.
//! - **World extent derived from the live panel rect** ([`dungeon_extent`]):
//!   the dungeon is generated at whatever tile count makes
//!   `tiles * BLOCK_W/H` fill the map panel for the viewport actually handed
//!   to `tick`, not at a fixed size that happens to fit one demo grid. A
//!   portrait phone gets a tall narrow dungeon; a wide desktop window gets a
//!   short wide one; both fill their panel.
//! - **Multi-cell entities**: every monster and NPC is a 3x2 glyph cluster
//!   (a face row over a body row), not a single character, matching the
//!   "one entity is many cells" rule the whole gallery uses for
//!   touch-scale interfaces.
//! - **Autotiled walls** ([`tilekit::autotile`]) over a small BSP dungeon,
//!   reused from the same technique `21_deck_plan` and `24_torchlit_crypt`
//!   use, kept deliberately quiet here so the balloons stay the point.
//! - **A touch-first control rail** ([`ascii_tile_demos::ui::touch`]): four
//!   direction buttons, a reroll button, and a mute toggle, all grown to
//!   [`ascii_tile_demos::ui::touch::TAP_W`]x[`TAP_H`](ascii_tile_demos::ui::touch::TAP_H)
//!   and mirrored on the keyboard.
//!
//! ```sh
//! cargo run --example 47_hollow_talk --features crossterm
//! cargo run --example 47_hollow_talk --features software
//! cargo run --example 47_hollow_talk --features gl
//! cargo run --example 47_hollow_talk  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, mask4};
use tilekit::noise::{Rng, hash01};
use tilekit::palette::rgb;

/// Screen cells one dungeon tile occupies, width and height.
///
/// 4 wide by 2 tall rather than 1x1: cells are about twice as tall as wide
/// (see `ui::touch`'s cell-aspect derivation), so 4x2 is close to visually
/// square and, just as importantly, exactly matches the 2-row sprite every
/// entity is drawn with (see [`SPRITE`]) -- a tile block is precisely one
/// sprite's worth of rows, with one spare column to breathe.
const BLOCK_W: u16 = 4;
/// See [`BLOCK_W`].
const BLOCK_H: u16 = 2;

/// Smallest dungeon extent, in tiles, that is still worth generating a BSP
/// split over. Below this a single open room is the honest answer rather than
/// a maze of closets.
const MIN_TILES_W: u16 = 10;
/// See [`MIN_TILES_W`].
const MIN_TILES_H: u16 = 6;
/// Largest dungeon extent this demo will ever generate, a safety ceiling far
/// past any real viewport rather than a target anyone is expected to hit.
const MAX_TILES_W: u16 = 60;
/// See [`MAX_TILES_W`].
const MAX_TILES_H: u16 = 34;

/// Rooms a generated dungeon should have, roughly, regardless of its tile
/// extent. [`min_leaf_for`] solves for the BSP leaf size that hits this
/// target so a large desktop dungeon gets *bigger rooms*, not more of them:
/// the alternative (a fixed minimum leaf size) is what produced a corridor
/// lattice at large extents, since the O(n^2) corridor pass in
/// [`Dungeon::generate`] connects every pair of rooms whose spans happen to
/// align, and that count grows quadratically with room count.
const TARGET_ROOMS: f32 = 6.0;

/// Smallest a BSP leaf may be, in tiles, before the split stops. The
/// reference dungeon (see the module docs) is a handful of large chambers
/// linked by short corridors, not a warren of closets, so this floor is
/// deliberately high: a small viewport should render as one open room
/// rather than as many tiny ones.
const MIN_LEAF_FLOOR: i32 = 6;
/// Largest a BSP leaf may be forced to, in tiles, even for a very large
/// dungeon -- past this, rooms would read as featureless halls rather than
/// rooms.
const MIN_LEAF_CEIL: i32 = 24;

/// Solves for the BSP minimum-leaf size that produces roughly
/// [`TARGET_ROOMS`] rooms from a `w`x`h` tile dungeon: each leaf has area on
/// the order of `(2 * min_leaf)^2`, so `min_leaf = sqrt(area / (4 *
/// target))`.
fn min_leaf_for(w: i32, h: i32) -> i32 {
    let leaf = ((w as f32 * h as f32) / (4.0 * TARGET_ROOMS)).sqrt();
    (leaf.round() as i32).clamp(MIN_LEAF_FLOOR, MIN_LEAF_CEIL)
}

/// How many rows of empty gap sit between a balloon's border and the speaker
/// it points at, filled by [`draw_tail`]. Enough to read as a pointer rather
/// than a smudge against the border.
const TAIL_GAP: i32 = 2;

/// Widest a balloon's text is ever wrapped to, in cells. Long enough to hold
/// a full sentence without wrapping to five lines, short enough that even a
/// landscape phone's map panel can fit two side by side.
const MAX_BALLOON_TEXT_W: usize = 26;

/// World-seconds a balloon stays up once spoken. Long enough to read a full
/// sentence at a comfortable pace, short enough that the dungeon never has
/// more than a handful on screen at once.
const TALK_DURATION: f32 = 5.5;

/// World-seconds an entity waits, on average, between lines. Varied per
/// entity by [`next_pause`] so nobody speaks in lockstep.
const BASE_PAUSE: f32 = 3.0;

/// World-seconds an idle entity waits in a room before choosing a new
/// destination.
const WANDER_PAUSE: f32 = 2.2;

/// One dungeon tile.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Void,
    Floor,
    Wall,
    Door,
}

/// A generated dungeon: the tile grid plus the rooms cut into it.
struct Dungeon {
    tiles: Vec<Tile>,
    w: i32,
    h: i32,
    rooms: Vec<Rect>,
}

impl Dungeon {
    const fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        Some((y * self.w + x) as usize)
    }

    fn tile(&self, x: i32, y: i32) -> Tile {
        self.index(x, y).map_or(Tile::Void, |i| self.tiles[i])
    }

    /// Generates a dungeon `w` x `h` tiles, via BSP split and one carved
    /// corridor per adjacent pair of leaves. See `21_deck_plan.rs`'s `Deck`
    /// for the fuller write-up of the same technique; kept smaller and
    /// undocumented per-step here because this demo's point is the balloons,
    /// not the generator.
    fn generate(seed: u32, w: i32, h: i32) -> Self {
        let mut rng = Rng::new(seed);
        let mut tiles = vec![Tile::Void; (w * h) as usize];
        let root = Rect::new(1, 1, (w - 2).max(1) as u16, (h - 2).max(1) as u16);
        let mut leaves = Vec::new();
        split(root, &mut rng, &mut leaves, min_leaf_for(w, h));

        let mut rooms = Vec::new();
        for leaf in &leaves {
            let margin_w = (leaf.width() / 6).max(1);
            let margin_h = (leaf.height() / 6).max(1);
            if leaf.width() <= margin_w * 2 + 1 || leaf.height() <= margin_h * 2 + 1 {
                continue;
            }
            let rect = Rect::new(
                leaf.left() + margin_w,
                leaf.top() + margin_h,
                leaf.width() - margin_w * 2,
                leaf.height() - margin_h * 2,
            );
            rooms.push(rect);
        }
        if rooms.is_empty() {
            rooms.push(root);
        }

        let mut dungeon = Self {
            tiles,
            w,
            h,
            rooms: rooms.clone(),
        };
        for room in &rooms {
            for y in room.top()..room.bottom() {
                for x in room.left()..room.right() {
                    if let Some(i) = dungeon.index(i32::from(x), i32::from(y)) {
                        dungeon.tiles[i] = Tile::Floor;
                    }
                }
            }
        }

        let mut doors = Vec::new();
        for i in 0..rooms.len() {
            for j in (i + 1)..rooms.len() {
                if let Some(corridor) = shared_wall_span(rooms[i], rooms[j], &mut rng) {
                    let (x0, y0, x1, y1) = corridor.span;
                    for x in x0..=x1 {
                        for y in y0..=y1 {
                            if let Some(idx) = dungeon.index(x, y) {
                                dungeon.tiles[idx] = Tile::Floor;
                            }
                        }
                    }
                    doors.push(corridor.door_a);
                    doors.push(corridor.door_b);
                }
            }
        }

        for y in 0..h {
            for x in 0..w {
                let Some(i) = dungeon.index(x, y) else {
                    continue;
                };
                if dungeon.tiles[i] == Tile::Floor {
                    continue;
                }
                let touches_floor = [(0, -1), (1, 0), (0, 1), (-1, 0)]
                    .iter()
                    .any(|&(dx, dy)| matches!(dungeon.tile(x + dx, y + dy), Tile::Floor));
                if touches_floor {
                    dungeon.tiles[i] = Tile::Wall;
                }
            }
        }
        for (x, y) in doors {
            if let Some(idx) = dungeon.index(x, y) {
                dungeon.tiles[idx] = Tile::Door;
            }
        }

        tiles = dungeon.tiles;
        Self { tiles, w, h, rooms }
    }

    fn room_center(&self, index: usize) -> (i32, i32) {
        let r = self.rooms[index % self.rooms.len().max(1)];
        (
            i32::from(r.left()) + i32::from(r.width()) / 2,
            i32::from(r.top()) + i32::from(r.height()) / 2,
        )
    }

    fn walkable(&self, x: i32, y: i32) -> bool {
        matches!(self.tile(x, y), Tile::Floor | Tile::Door)
    }
}

fn split(area: Rect, rng: &mut Rng, leaves: &mut Vec<Rect>, min_leaf: i32) {
    let min: u16 = min_leaf.try_into().unwrap_or(u16::MAX);
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
        split(
            Rect::new(area.left(), area.top(), at, area.height()),
            rng,
            leaves,
            min_leaf,
        );
        split(
            Rect::new(
                area.left() + at,
                area.top(),
                area.width() - at,
                area.height(),
            ),
            rng,
            leaves,
            min_leaf,
        );
    } else {
        let span = area.height() - min * 2;
        let at = min + rng.next_below(u32::from(span.max(1))) as u16;
        split(
            Rect::new(area.left(), area.top(), area.width(), at),
            rng,
            leaves,
            min_leaf,
        );
        split(
            Rect::new(
                area.left(),
                area.top() + at,
                area.width(),
                area.height() - at,
            ),
            rng,
            leaves,
            min_leaf,
        );
    }
}

/// A straight corridor connecting two rooms, plus the door plate on each end.
struct Corridor {
    span: (i32, i32, i32, i32),
    door_a: (i32, i32),
    door_b: (i32, i32),
}

/// Finds a straight corridor connecting two cardinally-adjacent rooms. See
/// `21_deck_plan.rs`'s function of the same name for the full reasoning.
fn shared_wall_span(a: Rect, b: Rect, rng: &mut Rng) -> Option<Corridor> {
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

    let vertical_overlap = at.max(bt)..ab.min(bb);
    if !vertical_overlap.is_empty() {
        let (gap_lo, gap_hi) = if ar <= bl {
            (ar, bl)
        } else if br <= al {
            (br, al)
        } else {
            (0, 0)
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

    let horizontal_overlap = al.max(bl)..ar.min(br);
    if !horizontal_overlap.is_empty() {
        let (gap_lo, gap_hi) = if ab <= bt {
            (ab, bt)
        } else if bb <= at {
            (bb, at)
        } else {
            (0, 0)
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

/// Computes the dungeon extent, in tiles, that fills `map_area` exactly under
/// [`BLOCK_W`]x[`BLOCK_H`] tile blocks (round-3 rule: derive world extent from
/// the live rect, never from a constant). Clamped to a sane range so a
/// degenerate rect still produces a generatable dungeon and a huge one
/// doesn't runs away.
fn dungeon_extent(map_area: Rect) -> (u16, u16) {
    let w = (map_area.width() / BLOCK_W).clamp(MIN_TILES_W, MAX_TILES_W);
    let h = (map_area.height() / BLOCK_H).clamp(MIN_TILES_H, MAX_TILES_H);
    (w, h)
}

/// What kind of speaker an entity is: controls its sprite, color, and quote
/// pool.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Drunk,
    Courtesan,
    Skeleton,
    Innkeeper,
    Rat,
}

impl Kind {
    /// The 3-wide, 2-tall glyph cluster every entity of this kind is drawn
    /// as. Row 0 is the face, row 1 is the body -- see the module docs on why
    /// this is exactly the [`BLOCK_H`] of one tile.
    const fn sprite(self) -> [[char; 3]; 2] {
        match self {
            Self::Drunk => [[' ', '☺', ' '], ['\\', '\u{2588}', '/']],
            Self::Courtesan => [[' ', '☺', ' '], ['(', '\u{2588}', ')']],
            Self::Skeleton => [[' ', '☻', ' '], ['|', '\u{2588}', '|']],
            Self::Innkeeper => [[' ', '☻', ' '], ['[', '\u{2588}', ']']],
            Self::Rat => [[' ', ' ', ' '], ['~', '·', '~']],
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Drunk => rgb(214, 170, 90),
            Self::Courtesan => rgb(216, 120, 168),
            Self::Skeleton => rgb(220, 222, 210),
            Self::Innkeeper => rgb(176, 132, 84),
            Self::Rat => rgb(150, 148, 140),
        }
    }

    /// A short pool of in-character lines. Cycled sequentially per entity
    /// rather than sampled, so the same seed always produces the same
    /// dialogue order -- required for the determinism test, and it also
    /// means a line is never repeated back-to-back by accident.
    const fn quotes(self) -> &'static [&'static str] {
        match self {
            Self::Drunk => &[
                "You an adventurer? So was I, once.",
                "Bottoms up! Join us for a moment.",
                "Left that life behind some time ago.",
                "You'll want a party before venturing forth.",
            ],
            Self::Courtesan => &[
                "Good evening. Are you looking for company?",
                "You look like you could use a rest.",
                "The dungeon can wait one more hour.",
            ],
            Self::Skeleton => &[
                "...bones remember the sun...",
                "Who disturbs this quiet hall?",
                "Rest here, traveler. Rest is easy now.",
            ],
            Self::Innkeeper => &[
                "Rooms are two gold, no questions asked.",
                "Watch the cellar door, it sticks.",
                "We do not serve past the eighth bell.",
            ],
            Self::Rat => &["squeak.", "skrrk skrrk", "..."],
        }
    }
}

/// A monster or NPC wandering the dungeon and, on its own timer, speaking.
struct Being {
    name: &'static str,
    kind: Kind,
    x: i32,
    y: i32,
    target_room: usize,
    wander_wait: f32,
    speak_wait: f32,
    quote_idx: usize,
}

impl Being {
    fn wander(&mut self, dungeon: &Dungeon, dt: f32, rng: &mut Rng) {
        self.wander_wait -= dt;
        if self.wander_wait > 0.0 {
            return;
        }
        let (tx, ty) = dungeon.room_center(self.target_room);
        if self.x == tx && self.y == ty {
            self.target_room = rng.next_below(dungeon.rooms.len().max(1) as u32) as usize;
            self.wander_wait = WANDER_PAUSE * (0.6 + rng.next_f32());
            return;
        }
        let (dx, dy) = (tx - self.x, ty - self.y);
        let (nx, ny) = if dx.abs() >= dy.abs() && dx != 0 {
            (self.x + dx.signum(), self.y)
        } else if dy != 0 {
            (self.x, self.y + dy.signum())
        } else {
            (self.x, self.y)
        };
        if dungeon.walkable(nx, ny) {
            self.x = nx;
            self.y = ny;
        }
        self.wander_wait = 0.5;
    }
}

/// How long an entity waits before its next line, derived from its index and
/// a monotonic cycle counter rather than the clock, so it stays deterministic
/// while still varying entity to entity and line to line.
fn next_pause(entity_index: usize, cycle: u32) -> f32 {
    let t = hash01(0x4841_4c4b, entity_index as i32, cycle as i32);
    BASE_PAUSE + t * BASE_PAUSE
}

/// An active balloon: which entity said it, the raw (unwrapped) text, and how
/// much longer it stays up.
struct Balloon {
    entity: usize,
    text: &'static str,
    remaining: f32,
}

/// Greedy word-wraps `text` to at most `width` columns per line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!current.is_empty());
        if current.chars().count() + extra + word.chars().count() > width && !current.is_empty() {
            lines.push(core::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Places one balloon: wraps its text, finds a rect that fits inside
/// `map_area` and does not overlap anything in `placed`, and reports whether
/// it landed above (`true`) or below (`false`) the speaker.
///
/// `anchor` is the speaker's head cell (top-center of its sprite). The
/// returned rect is horizontally clamped to stay inside `map_area` -- the
/// "flip rather than clip" rule -- and vertically resolved against `placed`
/// by walking further away from the speaker until clear or the panel edge is
/// reached, in up to a handful of steps (bounded, since the panel is small
/// enough that unresolved overlap after that many attempts means there is
/// simply no room, and drawing a slightly crowded balloon beats an infinite
/// loop).
fn place_balloon(
    map_area: Rect,
    anchor: (i32, i32),
    lines: &[String],
    placed: &[Rect],
) -> (Rect, bool) {
    let text_w = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(4);
    let rect_w = u16::try_from(text_w + 4).unwrap_or(u16::MAX);
    let rect_h = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);

    let bound_l = i32::from(map_area.left()) + 1;
    let bound_r = i32::from(map_area.right()) - 1 - i32::from(rect_w);
    let desired_left = anchor.0 - i32::from(rect_w) / 2;
    let left = desired_left.clamp(bound_l, bound_l.max(bound_r));

    let bound_t = i32::from(map_area.top()) + 1;
    let bound_b = i32::from(map_area.bottom()) - 1 - i32::from(rect_h);

    let above_top = anchor.1 - TAIL_GAP - 1 - i32::from(rect_h);
    let above_fits = above_top >= bound_t;
    let mut above = above_fits;
    let mut top = if above_fits {
        above_top
    } else {
        anchor.1 + 2 + TAIL_GAP
    };

    for _ in 0..8 {
        let candidate = Rect::new(
            left.clamp(bound_l, bound_l.max(bound_r)) as u16,
            top.clamp(bound_t, bound_t.max(bound_b)) as u16,
            rect_w,
            rect_h,
        );
        if let Some(other) = placed.iter().find(|r| r.overlaps(candidate)) {
            if above {
                top = i32::from(other.top()) - i32::from(rect_h) - 1;
                if top < bound_t {
                    above = false;
                    top = anchor.1 + 2 + TAIL_GAP;
                }
            } else {
                top = i32::from(other.bottom()) + 1;
            }
            continue;
        }
        return (candidate, above);
    }
    (
        Rect::new(
            left.clamp(bound_l, bound_l.max(bound_r)) as u16,
            top.clamp(bound_t, bound_t.max(bound_b)) as u16,
            rect_w,
            rect_h,
        ),
        above,
    )
}

/// Draws the diagonal tail linking a balloon's attachment column to the
/// speaker's head, choosing `/`, `\`, or `|` per row from the local slope of
/// a straight interpolation between the two endpoints.
///
/// `row_a`/`x_a` is the row and column immediately touching the balloon's own
/// border (on the gap side); `row_b`/`x_b` is the row and column immediately
/// touching the speaker (also on the gap side). Which one is numerically
/// smaller depends on whether the balloon landed above or below, so the
/// interpolation is written to work either way rather than assuming order.
#[allow(clippy::too_many_arguments)]
fn draw_tail(
    surface: &mut Surface<'_>,
    map_area: Rect,
    row_a: i32,
    x_a: i32,
    row_b: i32,
    x_b: i32,
    color: Color,
) {
    if row_a == row_b {
        return;
    }
    let (lo, hi) = (row_a.min(row_b), row_a.max(row_b));
    let x_at = |row: i32| -> i32 {
        let t = (row - row_a) as f32 / (row_b - row_a) as f32;
        t.mul_add((x_b - x_a) as f32, x_a as f32).round() as i32
    };
    for row in (lo + 1)..hi {
        let x = x_at(row);
        let x_prev = x_at(row - 1);
        let glyph = match x.cmp(&x_prev) {
            core::cmp::Ordering::Greater => '\\',
            core::cmp::Ordering::Less => '/',
            core::cmp::Ordering::Equal => '\u{2502}',
        };
        if x < i32::from(map_area.left()) || x >= i32::from(map_area.right()) {
            continue;
        }
        if row < i32::from(map_area.top()) || row >= i32::from(map_area.bottom()) {
            continue;
        }
        surface.put(
            (x as u16, row as u16),
            glyph,
            Style::new().fg(color).bg(ui::BG),
        );
    }
}

/// The screen-cell origin (top-left) of dungeon tile `(tx, ty)` within
/// `area`, under [`BLOCK_W`]x[`BLOCK_H`] blocks.
const fn tile_origin(area: Rect, tx: i32, ty: i32) -> (i32, i32) {
    (
        area.left() as i32 + tx * BLOCK_W as i32,
        area.top() as i32 + ty * BLOCK_H as i32,
    )
}

/// Whether screen cell `(x, y)` falls inside `area`. Shared by every
/// block-drawing helper below so a block straddling the panel edge clips a
/// cell at a time instead of being skipped (or overdrawn) wholesale.
fn in_area(area: Rect, x: i32, y: i32) -> bool {
    x >= i32::from(area.left())
        && y >= i32::from(area.top())
        && x < i32::from(area.right())
        && y < i32::from(area.bottom())
}

/// Fills the [`BLOCK_W`]x[`BLOCK_H`] block whose top-left is `(ox, oy)` with
/// one glyph, clipping to `area` a cell at a time so a block straddling the
/// panel edge draws its visible half rather than being skipped entirely.
///
/// Only meant for a glyph that still reads as architecture when repeated on
/// both axes -- a straight box-drawing line, or a flat shade/hatch tone. A
/// glyph with its own distinct shape (a corner, a junction, a door) tiled
/// this way is exactly what turned the dungeon into a page of letterforms;
/// see [`HollowTalk::draw_wall_block`] and [`HollowTalk::draw_door_block`]
/// for the cases that draw such a glyph once instead.
fn fill_block(surface: &mut Surface<'_>, area: Rect, ox: i32, oy: i32, glyph: char, style: Style) {
    for dy in 0..i32::from(BLOCK_H) {
        for dx in 0..i32::from(BLOCK_W) {
            let (x, y) = (ox + dx, oy + dy);
            if in_area(area, x, y) {
                surface.put((x as u16, y as u16), glyph, style);
            }
        }
    }
}

/// Draws one floor tile's block, one screen cell at a time.
///
/// The moss speck is a function of the *screen cell*, not the dungeon tile:
/// deciding it once per tile and filling the whole block with the result
/// (the original bug) turns an 8% chance into a whole tile-sized blotch of
/// literal `,` characters, which is exactly the kind of run this pass is
/// supposed to avoid. Per-cell keeps the same 8% density but scatters it, so
/// it reads as flecked stone instead of as punctuation.
fn draw_floor_block(surface: &mut Surface<'_>, area: Rect, ox: i32, oy: i32) {
    for dy in 0..i32::from(BLOCK_H) {
        for dx in 0..i32::from(BLOCK_W) {
            let (x, y) = (ox + dx, oy + dy);
            if !in_area(area, x, y) {
                continue;
            }
            let speck = hash01(0x1f2e, x, y) < 0.08;
            let glyph = if speck { ',' } else { '.' };
            surface.put(
                (x as u16, y as u16),
                glyph,
                Style::new().fg(rgb(84, 90, 78)).bg(rgb(20, 22, 18)),
            );
        }
    }
}

/// Draws one door tile's block: a threshold hatch with the door glyph placed
/// once at the center, rather than the door glyph itself tiled across all
/// eight cells (which is where the `+++++++` run came from -- a door is one
/// object, not a wall of them).
fn draw_door_block(surface: &mut Surface<'_>, area: Rect, ox: i32, oy: i32) {
    let bg = rgb(30, 24, 12);
    fill_block(
        surface,
        area,
        ox,
        oy,
        '\u{2591}',
        Style::new().fg(rgb(90, 76, 46)).bg(bg),
    );
    let (cx, cy) = (ox + i32::from(BLOCK_W) / 2, oy + i32::from(BLOCK_H) / 2);
    if in_area(area, cx, cy) {
        surface.put(
            (cx as u16, cy as u16),
            '+',
            Style::new().fg(rgb(226, 190, 110)).bg(bg),
        );
    }
}

/// Draws a 3x2 multi-cell sprite (a face row over a body row) with its
/// top-left at `(ox, oy)`, skipping blank cells so the sprite's own
/// background shows the tile behind it rather than a solid box.
fn draw_sprite(
    surface: &mut Surface<'_>,
    area: Rect,
    ox: i32,
    oy: i32,
    glyphs: [[char; 3]; 2],
    fg: Color,
) {
    for (row, cells) in glyphs.iter().enumerate() {
        for (col, &ch) in cells.iter().enumerate() {
            if ch == ' ' {
                continue;
            }
            let (x, y) = (ox + col as i32, oy + row as i32);
            if x < i32::from(area.left())
                || y < i32::from(area.top())
                || x >= i32::from(area.right())
                || y >= i32::from(area.bottom())
            {
                continue;
            }
            surface.put(
                (x as u16, y as u16),
                ch,
                Style::new().fg(fg).bg(rgb(20, 22, 18)),
            );
        }
    }
}

/// Draws the left status panel: a small glyph bust, HP/SP gauges, and the
/// current weapon. Static content -- there is nothing here that changes
/// per-frame -- so it is a free function rather than a method, matching the
/// other panel-drawing helpers on this page.
fn draw_status(surface: &mut Surface<'_>, area: Rect) {
    let inner = panel::Panel::new().title("Adventurer").draw(surface, area);
    if inner.height() == 0 || inner.width() < 6 {
        return;
    }
    let bust = ['☺', '\u{2591}', '\u{2591}'];
    panel::spans(
        surface,
        (inner.left(), inner.top()),
        inner.width(),
        &[Span::new(
            &bust.iter().collect::<String>(),
            rgb(140, 196, 226),
        )],
        panel::PANEL_BG,
    );
    if inner.height() > 1 {
        panel::spans(
            surface,
            (inner.left(), inner.top() + 1),
            6,
            &[Span::dim("HP")],
            panel::PANEL_BG,
        );
        let bar_w = inner.width().saturating_sub(3).max(3);
        panel::bar(
            surface,
            (inner.left() + 3, inner.top() + 1),
            bar_w,
            0.82,
            panel::threshold(0.82),
            rgb(30, 30, 26),
        );
    }
    if inner.height() > 2 {
        panel::spans(
            surface,
            (inner.left(), inner.top() + 2),
            6,
            &[Span::dim("SP")],
            panel::PANEL_BG,
        );
        let bar_w = inner.width().saturating_sub(3).max(3);
        panel::bar(
            surface,
            (inner.left() + 3, inner.top() + 2),
            bar_w,
            0.55,
            panel::threshold(0.55),
            rgb(30, 30, 26),
        );
    }
    if inner.height() > 4 {
        panel::spans(
            surface,
            (inner.left(), inner.top() + 4),
            inner.width(),
            &[Span::dim("Weapon:"), Span::plain(" Staff")],
            panel::PANEL_BG,
        );
    }
}

/// Draws the right hotbar: a couple of static item slots, echoing the F1/F2
/// item rail in the source game. Decorative rather than tappable -- unlike
/// the movement rail, nothing in this demo consumes an item.
fn draw_hotbar(surface: &mut Surface<'_>, area: Rect) {
    let inner = panel::Panel::new().title("Items").draw(surface, area);
    if inner.height() == 0 || inner.width() < 6 {
        return;
    }
    let rows: &[(&str, &str, Color)] = &[
        ("F1", "Lantern", rgb(226, 176, 96)),
        ("F2", "Potion", rgb(120, 176, 226)),
    ];
    for (i, (key, label, color)) in rows.iter().enumerate() {
        if i as u16 >= inner.height() {
            break;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top() + i as u16),
            inner.width(),
            &[
                Span::new(key, ui::ACCENT),
                Span::plain(" "),
                Span::new(label, *color),
            ],
            panel::PANEL_BG,
        );
    }
}

/// Which control the player pressed, from either a touch button or its
/// keyboard twin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Up,
    Down,
    Left,
    Right,
    Reroll,
    Mute,
}

/// Roster of unique display names, dealt out in order (never sampled with
/// replacement) so two speakers are never confused for one another. Sized
/// comfortably above [`ENTITY_COUNT`].
const NAMES: [&str; 8] = [
    "Aldric the Sot",
    "Otho Grimjaw",
    "Mireille",
    "The Rattling Thane",
    "Bram Hollowmug",
    "a cave rat",
    "Pell Ashenwick",
    "Voss the Unlucky",
];

const ENTITY_COUNT: usize = 5;

/// State: the generated dungeon, the wandering cast, active balloons, the
/// player, and input.
pub struct HollowTalk {
    seed: u32,
    extent: (u16, u16),
    dungeon: Dungeon,
    beings: Vec<Being>,
    balloons: Vec<Balloon>,
    player: (i32, i32),
    time: f32,
    cycle: u32,
    muted: bool,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for HollowTalk {
    fn default() -> Self {
        let seed = 47;
        let extent = (MIN_TILES_W.max(24), MIN_TILES_H.max(14));
        let dungeon = Dungeon::generate(seed, i32::from(extent.0), i32::from(extent.1));
        let player = dungeon.room_center(0);
        let beings = spawn_beings(&dungeon, seed);
        Self {
            seed,
            extent,
            dungeon,
            beings,
            balloons: Vec::new(),
            player,
            time: 0.0,
            cycle: 0,
            muted: false,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

fn spawn_beings(dungeon: &Dungeon, seed: u32) -> Vec<Being> {
    let mut rng = Rng::new(seed ^ 0x8ee2_2846);
    let kinds = [
        Kind::Drunk,
        Kind::Drunk,
        Kind::Courtesan,
        Kind::Skeleton,
        Kind::Innkeeper,
        Kind::Rat,
    ];
    (0..ENTITY_COUNT)
        .map(|i| {
            let room = (i + 1) % dungeon.rooms.len().max(1);
            let (x, y) = dungeon.room_center(room);
            Being {
                name: NAMES[i % NAMES.len()],
                kind: kinds[i % kinds.len()],
                x,
                y,
                target_room: room,
                wander_wait: WANDER_PAUSE * rng.next_f32(),
                speak_wait: BASE_PAUSE * rng.next_f32(),
                quote_idx: i % 3,
            }
        })
        .collect()
}

impl HollowTalk {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(0x9e37_79b9);
        self.dungeon = Dungeon::generate(
            self.seed,
            i32::from(self.extent.0),
            i32::from(self.extent.1),
        );
        self.player = self.dungeon.room_center(0);
        self.beings = spawn_beings(&self.dungeon, self.seed);
        self.balloons.clear();
    }

    /// Regenerates the dungeon at a new extent only when the extent actually
    /// changed, so a stable viewport does not re-roll the layout every frame
    /// (which would also make entity wander paths jump).
    fn ensure_extent(&mut self, extent: (u16, u16)) {
        if extent == self.extent {
            return;
        }
        self.extent = extent;
        self.dungeon = Dungeon::generate(self.seed, i32::from(extent.0), i32::from(extent.1));
        self.player = self.dungeon.room_center(0);
        self.beings = spawn_beings(&self.dungeon, self.seed);
        self.balloons.clear();
    }

    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        let mut rng = Rng::new(self.seed ^ (self.cycle << 1));
        for being in &mut self.beings {
            being.wander(&self.dungeon, dt, &mut rng);
        }

        for (i, being) in self.beings.iter_mut().enumerate() {
            being.speak_wait -= dt;
            if being.speak_wait <= 0.0 && !self.balloons.iter().any(|b| b.entity == i) {
                let quotes = being.kind.quotes();
                let text = quotes[being.quote_idx % quotes.len()];
                being.quote_idx += 1;
                self.balloons.push(Balloon {
                    entity: i,
                    text,
                    remaining: TALK_DURATION,
                });
                self.cycle += 1;
                being.speak_wait = next_pause(i, self.cycle);
            }
        }

        for balloon in &mut self.balloons {
            balloon.remaining -= dt;
        }
        self.balloons.retain(|b| b.remaining > 0.0);
    }

    fn move_player(&mut self, dx: i32, dy: i32) {
        let (nx, ny) = (self.player.0 + dx, self.player.1 + dy);
        if self.dungeon.walkable(nx, ny) {
            self.player = (nx, ny);
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_player(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_player(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_player(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_player(1, 0),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    KeyCode::Char('m' | 'M') => self.muted = !self.muted,
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
            Some(Action::Up) => self.move_player(0, -1),
            Some(Action::Down) => self.move_player(0, 1),
            Some(Action::Left) => self.move_player(-1, 0),
            Some(Action::Right) => self.move_player(1, 0),
            Some(Action::Reroll) => self.reroll(),
            Some(Action::Mute) => self.muted = !self.muted,
            None => {}
        }
    }

    // ── layout ───────────────────────────────────────────────────────────

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = panel::Panel::new()
            .title("The Hollow")
            .border(panel::Border::Double);
        let inner = panel.draw(surface, area);
        if inner.width() < BLOCK_W || inner.height() < BLOCK_H {
            return;
        }
        self.draw_tiles(surface, inner);
        self.draw_beings(surface, inner);
        if !self.muted {
            self.draw_balloons(surface, inner);
        }
    }

    fn draw_tiles(&self, surface: &mut Surface<'_>, area: Rect) {
        for ty in 0..self.dungeon.h {
            for tx in 0..self.dungeon.w {
                let tile = self.dungeon.tile(tx, ty);
                if tile == Tile::Void {
                    continue;
                }
                let (ox, oy) = tile_origin(area, tx, ty);
                if ox + i32::from(BLOCK_W) <= i32::from(area.left())
                    || oy + i32::from(BLOCK_H) <= i32::from(area.top())
                    || ox >= i32::from(area.right())
                    || oy >= i32::from(area.bottom())
                {
                    continue;
                }
                match tile {
                    Tile::Floor => draw_floor_block(surface, area, ox, oy),
                    Tile::Wall => self.draw_wall_block(surface, area, tx, ty, ox, oy),
                    Tile::Door => draw_door_block(surface, area, ox, oy),
                    Tile::Void => unreachable!("filtered above"),
                }
            }
        }
    }

    /// Draws one wall tile's block. A straight run (mask 5 = `│`, mask 10 =
    /// `─`) tiles its line glyph across the whole block, since a line
    /// repeated along its own axis still reads as a line. Every other mask
    /// (a corner, a T-junction, the cross, the isolated dot) is a *shape*,
    /// not a line: tiling one of those across all eight cells of the block
    /// is exactly what turned corners and junctions into runs of `[[[[[` and
    /// `JJJJJ` that read as letters instead of masonry. So those draw a flat
    /// masonry hatch across the block and place the shape once, at its
    /// center, the way a single stone in a wall carries one visible seam
    /// rather than the same crack repeated eight times.
    fn draw_wall_block(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        tx: i32,
        ty: i32,
        ox: i32,
        oy: i32,
    ) {
        let connects = |t: Tile| matches!(t, Tile::Wall | Tile::Door);
        let mask = mask4([
            connects(self.dungeon.tile(tx, ty - 1)),
            connects(self.dungeon.tile(tx + 1, ty)),
            connects(self.dungeon.tile(tx, ty + 1)),
            connects(self.dungeon.tile(tx - 1, ty)),
        ]) & 0x0F;
        let fg = rgb(150, 152, 140);
        let bg = rgb(10, 11, 9);
        let glyph = BOX_SINGLE[mask as usize];
        if mask == 5 || mask == 10 {
            fill_block(surface, area, ox, oy, glyph, Style::new().fg(fg).bg(bg));
            return;
        }
        fill_block(
            surface,
            area,
            ox,
            oy,
            '\u{2591}',
            Style::new().fg(rgb(46, 48, 42)).bg(bg),
        );
        let (cx, cy) = (ox + i32::from(BLOCK_W) / 2, oy + i32::from(BLOCK_H) / 2);
        if in_area(area, cx, cy) {
            surface.put((cx as u16, cy as u16), glyph, Style::new().fg(fg).bg(bg));
        }
    }

    fn draw_beings(&self, surface: &mut Surface<'_>, area: Rect) {
        for being in &self.beings {
            let (ox, oy) = tile_origin(area, being.x, being.y);
            draw_sprite(
                surface,
                area,
                ox,
                oy,
                being.kind.sprite(),
                being.kind.color(),
            );
        }
        let (px, py) = tile_origin(area, self.player.0, self.player.1);
        let player_sprite = [[' ', '☺', ' '], ['/', '\u{2588}', '\\']];
        draw_sprite(surface, area, px, py, player_sprite, rgb(140, 196, 226));
    }

    /// Places and draws every active balloon, in a fixed order (by which
    /// entity's balloon was spawned first) so overlap resolution is
    /// deterministic frame to frame.
    fn draw_balloons(&self, surface: &mut Surface<'_>, area: Rect) {
        let mut placed: Vec<Rect> = Vec::new();
        let text_w = MAX_BALLOON_TEXT_W.min(usize::from(area.width()).saturating_sub(8).max(6));

        for balloon in &self.balloons {
            let Some(being) = self.beings.get(balloon.entity) else {
                continue;
            };
            let (ox, oy) = tile_origin(area, being.x, being.y);
            let anchor = (ox + 1, oy);
            let lines = wrap(balloon.text, text_w);
            let (rect, above) = place_balloon(area, anchor, &lines, &placed);
            draw_balloon_box(surface, area, rect, &lines, being.kind.color(), being.name);

            let attach_x = anchor
                .0
                .clamp(i32::from(rect.left()) + 1, i32::from(rect.right()) - 2);
            let (row_a, row_b) = if above {
                (i32::from(rect.bottom()) - 1, anchor.1)
            } else {
                (i32::from(rect.top()), anchor.1 + 1)
            };
            draw_tail(
                surface,
                area,
                row_a,
                attach_x,
                row_b,
                anchor.0,
                being.kind.color(),
            );

            placed.push(rect);
        }
    }
}

/// Draws one balloon's frame and wrapped text into `rect`. A free function
/// (not a method: it touches no field of `HollowTalk`) so it can be reused
/// for a balloon belonging to any entity without borrowing `self`.
fn draw_balloon_box(
    surface: &mut Surface<'_>,
    area: Rect,
    rect: Rect,
    lines: &[String],
    accent: Color,
    speaker: &str,
) {
    if rect.left() < area.left() || rect.right() > area.right() {
        return;
    }
    let panel = panel::Panel::new()
        .title(speaker)
        .border(panel::Border::Single)
        .frame(accent)
        .bg(rgb(14, 15, 12));
    let inner = panel.draw(surface, rect);
    for (i, line) in lines.iter().enumerate() {
        if i as u16 >= inner.height() {
            break;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top() + i as u16),
            inner.width(),
            &[Span::new(line, ui::FG)],
            rgb(14, 15, 12),
        );
    }
}

impl HollowTalk {
    /// Draws the touch control rail (movement, reroll, mute) into `area`,
    /// registering hotspots as it goes.
    fn draw_rail(&mut self, surface: &mut Surface<'_>, area: Rect) {
        surface.fill_rect(area, ' ', Style::new().bg(panel::PANEL_BG));
        let buttons: [(Action, &str); 6] = [
            (Action::Left, "\u{2190}"),
            (Action::Up, "\u{2191}"),
            (Action::Down, "\u{2193}"),
            (Action::Right, "\u{2192}"),
            (Action::Reroll, "New"),
            (
                Action::Mute,
                if self.muted { "Talk:Off" } else { "Talk:On" },
            ),
        ];
        let n = buttons.len() as u16;
        if area.width() < TAP_W || area.height() < TAP_H {
            return;
        }
        let slot_w = (area.width() / n).max(TAP_W);
        for (i, (action, label)) in buttons.iter().enumerate() {
            let x = area.left() + i as u16 * slot_w;
            if x + TAP_W > area.right() {
                break;
            }
            let rect = Rect::new(x, area.top(), slot_w.min(area.right() - x), area.height());
            let btn = panel::Panel::new().bg(rgb(26, 28, 22)).draw(surface, rect);
            if btn.width() > 0 && btn.height() > 0 {
                let cx = btn.left() + btn.width().saturating_sub(label.chars().count() as u16) / 2;
                let cy = btn.top() + btn.height() / 2;
                surface.print(
                    (cx, cy),
                    label,
                    Style::new().fg(ui::ACCENT).bg(rgb(26, 28, 22)),
                );
            }
            self.hotspots.push(rect, *action);
        }
    }

    fn status_text(&self) -> String {
        format!(
            "seed {}  talking {}  balloons {}",
            self.seed,
            self.beings.len(),
            self.balloons.len()
        )
    }
}

impl Demo for HollowTalk {
    const NAME: &'static str = "47_hollow_talk";
    const TITLE: &'static str = "Hollow Talk";
    const BLURB: &'static str =
        "Zorbus: in-world speech balloons with pointer tails over a tile dungeon.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "move"),
            ("R", "new dungeon"),
            ("M", "mute balloons"),
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

        // Reserve the thumb-zone control rail before anything else claims
        // rows: it is the one primary, always-tappable action set, so it is
        // sized first and everything else divides what remains.
        let rail_h = 4u16.min(content.height());
        let (top_area, rail_area) = panel::split_bottom(content, rail_h);

        // Portrait stacks a compact status/hotbar row above the map; desktop
        // and landscape put both as narrow columns either side of it instead,
        // since a phone has rows to spare that a landscape window does not.
        // Below the width where a sidebar would squeeze the map to nothing,
        // collapse to zero-size rects, which every draw call below treats as
        // "draw nothing" rather than as a special case.
        let stack = shape.stacks();
        let (status_area, map_area, hotbar_area) = if stack {
            let status_h = 4u16.min(top_area.height());
            let (info_row, map_area) = panel::split_top(top_area, status_h);
            let (status_area, hotbar_area) = panel::split_left(info_row, info_row.width() / 2);
            (status_area, map_area, hotbar_area)
        } else if top_area.width() >= 70 {
            let side_w = 18u16;
            let (status_area, rest) = panel::split_left(top_area, side_w);
            let (map_area, hotbar_area) = panel::split_right(rest, side_w);
            (status_area, map_area, hotbar_area)
        } else {
            let empty = Rect::new(top_area.left(), top_area.top(), 0, 0);
            (empty, top_area, empty)
        };

        let extent = dungeon_extent(map_area);
        self.ensure_extent(extent);
        self.simulate(dt);
        self.handle_pointer();
        self.hotspots.clear();

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        draw_status(&mut surface, status_area);
        draw_hotbar(&mut surface, hotbar_area);
        self.draw_map(&mut surface, map_area);
        self.draw_rail(&mut surface, rail_area);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(HollowTalk);
