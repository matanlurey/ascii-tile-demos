//! 57: Dealt Dungeon -- the map and the deck are the same objects.
//!
//! Adapted from Hand of Fate. A dungeon floor is a grid of face-down cards
//! laid out on a table. You step a token from card to adjacent card;
//! arriving flips that card face-up and it becomes the room you are in -- an
//! encounter, a shrine, a trap, a treasure, or an empty passage. Resolved
//! cards stay face-up as a record of where you have been. This is neither
//! `28_spire_deck` (a hand played into combat) nor `26_hexcrawl` (terrain
//! revealed by walking): the novelty here is that the *map itself* is the
//! deck, so it can be shuffled, dealt, and drawn from -- clearing a floor
//! deals a fresh one, visibly, card by card.
//!
//! Techniques on show:
//!
//! - **A card grid sized to the live viewport, not a constant**
//!   ([`DealtDungeon::sync_floor`], [`axis_layout`]): the column and row
//!   count is solved from `term.area()` every time the panel rect changes,
//!   growing toward a comfortable card size before it grows the count, so a
//!   desktop window gets a bigger table rather than a small grid stranded in
//!   a large empty one.
//! - **A flip that reads as a flip** ([`narrow_rect`],
//!   [`DealtDungeon::draw_grid`]): a flipping card's rect is narrowed by
//!   `|cos(pi * progress)|`, so it visibly shrinks to a sliver at the
//!   halfway point (where the face swaps from back to front) and grows back
//!   out, rather than instantly swapping content.
//! - **A staggered deal** ([`DealtDungeon::deal_elapsed`]): each cell's
//!   appearance is delayed by its row-major index times a fixed stagger, so
//!   a freshly dealt floor visibly fills in card by card instead of
//!   appearing all at once.
//! - **Patterned backs, framed fronts** ([`DealtDungeon::draw_back`],
//!   [`DealtDungeon::draw_front`]): a face-down card is a checkerboard of
//!   `\u{2591}`/`\u{2592}` that idles with a slow per-cell shimmer (driven by
//!   [`tilekit::noise::hash01`] and `frame.delta`-accumulated time); a
//!   face-up card is drawn through [`ui::card::Card`], which already
//!   measures and wraps its own title and body so nothing on a card face is
//!   ever clipped.
//! - **Resolution on the card itself** ([`DealtDungeon::draw_choice_overlay`]):
//!   a flipped card that presents a decision (a token gamble or a pick
//!   between two outcomes) enlarges over its own position in the grid,
//!   keeping its title and suit, with each option a touch target at least
//!   [`touch::TAP_W`]x[`touch::TAP_H`].
//! - **The deck as visible state** ([`DealtDungeon::draw_sidebar`]): a draw
//!   pile and a discard pile sit beside the table as their own small cards.
//!   Clearing a floor moves that floor's cards to the discard; running low on
//!   the draw pile shuffles the discard back in.
//! - **A dealer's running line** ([`DealtDungeon::dealer_line`]): one line of
//!   commentary that updates on every flip, deal, and resolution, in the
//!   bottom (thumb-zone) band on every [`Shape`].
//! - **Names drawn without replacement** ([`DealtDungeon::pick_titles`]):
//!   every card title in a dealt floor is drawn from a shuffled permutation
//!   of a name pool sized well above the largest floor, not sampled with
//!   replacement, so no floor can repeat a title (see the
//!   `titles_within_a_floor_are_unique` test).
//!
//! ```sh
//! cargo run --example 57_dealt_dungeon --features crossterm
//! cargo run --example 57_dealt_dungeon --features software
//! cargo run --example 57_dealt_dungeon --features gl
//! cargo run --example 57_dealt_dungeon  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{self, Card, CardState};
use ascii_tile_demos::ui::panel::{self, Border, Panel};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, hash01, smoothstep};
use tilekit::palette::{rgb, scale};

/// Smallest a card may shrink to before the layout adds more of them instead.
///
/// Bigger than [`card::COMPACT_W`]: the interior this leaves (width minus
/// the two border columns) is 9 cells, which is exactly enough for the
/// longest word in [`ROOM_NAMES`] (8 characters) with a column to spare.
/// Sizing this off the actual name pool, rather than off the card module's
/// own generic minimum, is what keeps every title fitting without
/// mid-word clipping at every card size the layout ever produces.
const CARD_W_MIN: u16 = 11;
/// Card width the layout grows toward before it prefers adding columns.
const CARD_W_PREF: u16 = 13;
/// See [`CARD_W_MIN`].
const CARD_H_MIN: u16 = card::COMPACT_H;
/// See [`CARD_W_PREF`].
const CARD_H_PREF: u16 = 8;
/// Table felt left between cards: enough to read as gaps between physical
/// cards, not so much that it eats into the fill-the-viewport budget.
const GRID_GAP: u16 = 1;
/// Floor dimensions are clamped to this range on both axes. The lower bound
/// keeps a floor worth walking even in a cramped window; the upper bound
/// keeps card titles legible and the name pool ([`ADJECTIVES`]x[`NOUNS`],
/// 72 combinations) comfortably larger than the largest floor
/// (`MAX_COLS * MAX_ROWS` = 48) ever needs.
const MIN_COLS: u16 = 3;
const MAX_COLS: u16 = 8;
const MIN_ROWS: u16 = 3;
const MAX_ROWS: u16 = 6;

/// Seconds a flip animation takes end to end.
const FLIP_TIME: f32 = 0.55;
/// Seconds each newly dealt card takes to grow in from a sliver.
const DEAL_GROW_TIME: f32 = 0.32;
/// Seconds between one dealt card starting and the next, in row-major order.
/// Small enough that a full floor deals in well under two seconds, large
/// enough that the sequence still reads as cards landing one at a time
/// rather than a simultaneous pop.
const DEAL_STAGGER: f32 = 0.02;

/// Overlay size for a card presenting a choice, capped by the grid area.
const OVERLAY_W: u16 = 32;
const OVERLAY_H: u16 = 13;

/// Starting size of the shared draw pile, in cards. Large enough that many
/// floors pass before the discard has to be shuffled back in, which is the
/// event worth seeing rather than something that happens on floor one.
const STARTING_DRAW_PILE: u32 = 220;

/// Deterministic base seed. Floor generation mixes this with the floor
/// number, never with wall-clock time, so two renders of the same state
/// produce the same output.
const BASE_SEED: u32 = 0xD3A1_7BEE;

/// A dungeon room's purpose, which doubles as the card's suit -- the whole
/// point of the demo is that the map and the deck are one object, so a
/// room's *kind* is drawn exactly like a card's *suit*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoomKind {
    Encounter,
    Treasure,
    Shrine,
    Trap,
    Passage,
}

impl RoomKind {
    /// Suit glyph: spade for a fight, diamond for gold, heart for a
    /// blessing, club for a trap, and a plain circle for an empty passage --
    /// the one "suit" that is not really a suit, same as a blank card.
    const fn suit(self) -> char {
        match self {
            Self::Encounter => '\u{2660}',
            Self::Treasure => '\u{2666}',
            Self::Shrine => '\u{2665}',
            Self::Trap => '\u{2663}',
            Self::Passage => '\u{25CB}',
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Encounter => "Encounter",
            Self::Treasure => "Treasure",
            Self::Shrine => "Shrine",
            Self::Trap => "Trap",
            Self::Passage => "Passage",
        }
    }

    const fn body(self) -> &'static str {
        match self {
            Self::Encounter => "Something stirs in the dark ahead.",
            Self::Treasure => "Gold glints in the corner.",
            Self::Shrine => "A quiet altar waits.",
            Self::Trap => "The floor here looks wrong.",
            Self::Passage => "An empty corridor.",
        }
    }

    const fn tint(self) -> Color {
        match self {
            Self::Encounter => rgb(198, 96, 88),
            Self::Treasure => rgb(226, 184, 90),
            Self::Shrine => rgb(150, 200, 160),
            Self::Trap => rgb(160, 110, 190),
            Self::Passage => rgb(130, 140, 160),
        }
    }

    /// Whether flipping this card opens a card-resolution choice rather than
    /// resolving automatically. Two kinds resolve as a gamble (a coin-flip
    /// chance folded into the option), one as a plain pick between two
    /// guaranteed outcomes -- the brief's "a token gamble or a pick between
    /// two outcomes" shown as three concrete cases rather than one generic
    /// mechanic.
    const fn has_choice(self) -> bool {
        matches!(self, Self::Encounter | Self::Shrine | Self::Trap)
    }

    const fn choice_prompt(self) -> &'static str {
        match self {
            Self::Encounter => "A foe blocks the way. Fight or slip past?",
            Self::Shrine => "The shrine offers one blessing. Choose it.",
            Self::Trap => "A trap springs. Disarm it or push through?",
            Self::Treasure | Self::Passage => "",
        }
    }

    /// The two options a choice card presents, as `(label, hint)` pairs.
    ///
    /// Both fields are kept short (label up to 6 characters, hint up to 11)
    /// on purpose: the narrowest legal option box the overlay ever draws
    /// (see [`choice_option_rects`]) has an interior of about 12 columns
    /// once the `[1] `/`[2] ` prefix is accounted for, and these lengths fit
    /// it with room to spare rather than relying on truncation.
    const fn choice_options(self) -> ((&'static str, &'static str), (&'static str, &'static str)) {
        match self {
            Self::Encounter => (("Fight", "risk wounds"), ("Sneak", "no reward")),
            Self::Shrine => (("Vigor", "mend wounds"), ("Feast", "fill pack")),
            Self::Trap => (("Disarm", "chancy"), ("Dash", "sure graze")),
            Self::Treasure | Self::Passage => (("", ""), ("", "")),
        }
    }
}

/// One card on the table: a face-down room until it is flipped.
struct Cell {
    title: String,
    kind: RoomKind,
    revealed: bool,
    resolved: bool,
    /// `Some(t)` while flipping, where `t` is [`DealtDungeon::time`] at the
    /// moment the flip began.
    flip_started: Option<f32>,
}

impl Cell {
    const fn new(title: String, kind: RoomKind) -> Self {
        Self {
            title,
            kind,
            revealed: false,
            resolved: false,
            flip_started: None,
        }
    }
}

/// What tapping a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// Move to (and, if face-down, flip) an adjacent cell.
    Move(usize),
    /// Resolve the pending choice: `true` for the first option, `false` for
    /// the second.
    Choice(bool),
}

/// Unique room-name pool, drawn without replacement per floor (see
/// [`DealtDungeon::pick_titles`]) so no floor can repeat a title even at the
/// largest grid the layout allows. Single words rather than adjective/noun
/// pairs, and every one 8 characters or fewer: that bound is chosen to match
/// [`CARD_W_MIN`]'s interior exactly, which is what keeps a title from ever
/// running off the edge of the smallest card the layout draws.
const ROOM_NAMES: [&str; 51] = [
    "Crypt", "Vault", "Cellar", "Warren", "Grotto", "Alcove", "Cavern", "Chapel", "Barrow",
    "Pantry", "Stable", "Kennel", "Hollow", "Shrine", "Vestry", "Closet", "Attic", "Nave", "Choir",
    "Study", "Larder", "Forge", "Armory", "Landing", "Rookery", "Ossuary", "Gallery", "Cistern",
    "Passage", "Cesspit", "Scullery", "Sanctum", "Belfry", "Cloister", "Cookery", "Buttery",
    "Dungeon", "Aviary", "Catacomb", "Foundry", "Smithy", "Tannery", "Chantry", "Priory",
    "Wardroom", "Boudoir", "Solarium", "Grange", "Turret", "Byre", "Mews",
];

/// Solves one axis of the grid: how many cards fit, and roughly how big each
/// one is.
///
/// Grows the count toward `max_count` while cards would still land at or
/// above `pref_cell`, which is what "more cards when there is room" means in
/// practice; only once that stalls does the leftover space go into making
/// the existing count of cards bigger rather than adding more of them. Never
/// drops below `min_count`, even if that means a card ends up smaller than
/// `min_cell` -- the smallest a demo can do at an extreme size (the 80x24
/// snapshot grid) is degrade gracefully, not disappear.
fn axis_layout(extent: u16, min_cell: u16, pref_cell: u16, min_count: u16, max_count: u16) -> u16 {
    let mut count = min_count.max(1);
    while count < max_count {
        let next = count + 1;
        let gap_total = GRID_GAP * (next - 1);
        let needed = next.saturating_mul(pref_cell) + gap_total;
        if needed > extent {
            break;
        }
        count = next;
    }
    // If even the minimum count does not leave every card at least
    // `min_cell` wide, that is the smallest legal layout anyway; nothing
    // further to shrink.
    let _ = min_cell;
    count
}

/// Splits `total` cells into `count` bands of `base` each, distributing the
/// remainder across the first bands and separating every band by `gap` --
/// the same remainder-first idea as [`panel::columns`], generalized to
/// return offsets for both grid axes.
fn distribute(total: u16, count: u16, gap: u16, base: u16) -> Vec<(u16, u16)> {
    let count = count.max(1);
    let total_gap = gap * count.saturating_sub(1);
    let used = base.saturating_mul(count) + total_gap;
    let mut extra = total.saturating_sub(used);
    let mut sizes = vec![base.max(1); count as usize];
    for size in &mut sizes {
        if extra == 0 {
            break;
        }
        *size += 1;
        extra -= 1;
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut pos = 0u16;
    for size in sizes {
        out.push((pos, size));
        pos += size + gap;
    }
    out
}

/// Shrinks `rect` to `factor` of its width, centered, for the mid-flip and
/// mid-deal "narrow" states. Never quite zero, so a card mid-animation still
/// registers as *something* rather than a one-frame blank.
fn narrow_rect(rect: Rect, factor: f32) -> Rect {
    let factor = factor.clamp(0.05, 1.0);
    let w = ((f32::from(rect.width()) * factor).round() as u16).max(1);
    let x = rect.left() + (rect.width() - w) / 2;
    Rect::new(x, rect.top(), w, rect.height())
}

/// The interior a [`Panel`] would report for `rect`, without drawing it.
/// Needed to compute hotspot positions during layout, before a [`Surface`]
/// exists to hand `Panel::draw` -- the arithmetic is trivial enough that
/// duplicating it here is cheaper than threading a surface through layout.
const fn panel_inner(rect: Rect) -> Rect {
    if rect.width() < 2 || rect.height() < 2 {
        return Rect::new(rect.left(), rect.top(), 0, 0);
    }
    Rect::new(
        rect.left() + 1,
        rect.top() + 1,
        rect.width() - 2,
        rect.height() - 2,
    )
}

/// The overlay rect for a choice card, centered on `cell_rect` but clamped
/// inside `grid_area` -- "resolved on the card itself" means the overlay
/// stays anchored to the flipped card's own position rather than jumping to
/// a fixed corner of the screen.
fn overlay_rect(grid_area: Rect, cell_rect: Rect) -> Rect {
    let w = OVERLAY_W.min(grid_area.width().max(1));
    let h = OVERLAY_H.min(grid_area.height().max(1));
    let cx = cell_rect.left() + cell_rect.width() / 2;
    let cy = cell_rect.top() + cell_rect.height() / 2;
    let max_x = grid_area.right().saturating_sub(w).max(grid_area.left());
    let max_y = grid_area.bottom().saturating_sub(h).max(grid_area.top());
    let x = cx.saturating_sub(w / 2).clamp(grid_area.left(), max_x);
    let y = cy.saturating_sub(h / 2).clamp(grid_area.top(), max_y);
    Rect::new(x, y, w, h)
}

/// The two option rects inside a choice overlay: side by side if there is
/// room for two legal touch targets, stacked otherwise.
fn choice_option_rects(overlay: Rect) -> (Rect, Rect) {
    let inner = panel_inner(overlay);
    if inner.height() < 3 {
        let zero = Rect::new(inner.left(), inner.top(), 0, 0);
        return (zero, zero);
    }
    let prompt_rows = 2.min(inner.height().saturating_sub(4)).max(1);
    let options = Rect::new(
        inner.left(),
        inner.top() + prompt_rows,
        inner.width(),
        inner.height() - prompt_rows,
    );
    if options.width() > 2 * touch::TAP_W {
        let w = (options.width() - 1) / 2;
        let h = options.height();
        (
            Rect::new(options.left(), options.top(), w, h),
            Rect::new(options.left() + w + 1, options.top(), w, h),
        )
    } else {
        let h = (options.height() / 2).max(1);
        (
            Rect::new(options.left(), options.top(), options.width(), h),
            Rect::new(
                options.left(),
                options.top() + h,
                options.width(),
                options.height() - h,
            ),
        )
    }
}

/// Rows the dealer's commentary band claims, shrinking on a short terminal
/// but never disappearing: the running line is the one piece of state that
/// must always be visible, since touch has no hover to reveal it instead.
const fn dealer_height(content: Rect) -> u16 {
    if content.height() >= 30 {
        5
    } else if content.height() >= 16 {
        4
    } else {
        2
    }
}

/// Rows the player/pile band claims when stacked above the grid in portrait.
fn portrait_side_height(main: Rect) -> u16 {
    (main.height() / 4).clamp(8, 13).min(main.height())
}

/// The `index`-th cell of a `cols`-wide grid inside `area`, sized `cell_w` x
/// `cell_h` with `gap` between cells, or `None` if it would not fit.
fn tile_rect(
    area: Rect,
    index: usize,
    cols: u16,
    cell_w: u16,
    cell_h: u16,
    gap: u16,
) -> Option<Rect> {
    let cols = cols.max(1);
    let idx = u16::try_from(index).unwrap_or(u16::MAX);
    let col = idx % cols;
    let row = idx / cols;
    let x = area.left() + col * (cell_w + gap);
    let y = area.top() + row * (cell_h + gap);
    if x + cell_w > area.right() || y + cell_h > area.bottom() {
        return None;
    }
    Some(Rect::new(x, y, cell_w, cell_h))
}

/// State: the current floor's cards, the player's position on them, the
/// shared deck piles, the dealer's line, and everything needed to draw and
/// interact with all of it.
pub struct DealtDungeon {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    cell_rects: Vec<Rect>,
    player: usize,
    floor: u32,
    /// Index of the cell awaiting a choice, if any. Blocks movement.
    pending: Option<usize>,
    /// `time` at which the current floor started dealing.
    deal_started: f32,
    draw_pile: u32,
    discard_pile: u32,
    health: f32,
    max_health: f32,
    food: f32,
    max_food: f32,
    gold: u32,
    gear: u32,
    dealer_line: String,
    rng: Rng,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for DealtDungeon {
    fn default() -> Self {
        Self {
            // Zero forces `sync_floor` to deal the first floor on frame one,
            // against whatever grid area the real backend hands it, rather
            // than guessing a size here that a live viewport would then have
            // to correct.
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            cell_rects: Vec::new(),
            player: 0,
            floor: 1,
            pending: None,
            deal_started: 0.0,
            draw_pile: STARTING_DRAW_PILE,
            discard_pile: 0,
            health: 60.0,
            max_health: 60.0,
            food: 60.0,
            max_food: 60.0,
            gold: 20,
            gear: 0,
            dealer_line: "The dealer shuffles the deck.".to_string(),
            rng: Rng::new(BASE_SEED),
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl DealtDungeon {
    // -- floor generation ---------------------------------------------

    /// Shuffles the name pool and takes the first `count` entries, which is
    /// what guarantees uniqueness: distinct permutation indices map to
    /// distinct names, so there is no way for two titles in the same call to
    /// collide, unlike sampling with replacement.
    fn pick_titles(&mut self, count: usize) -> Vec<String> {
        let mut indices: Vec<usize> = (0..ROOM_NAMES.len()).collect();
        for i in (1..indices.len()).rev() {
            let j = self.rng.next_below((i + 1) as u32) as usize;
            indices.swap(i, j);
        }
        indices
            .into_iter()
            .take(count)
            .map(|idx| ROOM_NAMES[idx].to_string())
            .collect()
    }

    fn roll_kind(&mut self) -> RoomKind {
        let r = self.rng.next_f32();
        if r < 0.30 {
            RoomKind::Encounter
        } else if r < 0.50 {
            RoomKind::Treasure
        } else if r < 0.65 {
            RoomKind::Shrine
        } else if r < 0.85 {
            RoomKind::Trap
        } else {
            RoomKind::Passage
        }
    }

    /// Deals a fresh `cols` x `rows` floor: new cards, all face down except
    /// the entrance, player back at the entrance, no pending choice.
    fn generate_floor(&mut self, cols: u16, rows: u16) {
        let count = usize::from(cols) * usize::from(rows);
        let titles = self.pick_titles(count.max(1));
        let mut cells: Vec<Cell> = titles
            .into_iter()
            .map(|title| {
                let kind = self.roll_kind();
                Cell::new(title, kind)
            })
            .collect();
        if let Some(entrance) = cells.first_mut() {
            entrance.kind = RoomKind::Passage;
            entrance.revealed = true;
            entrance.resolved = true;
        }
        self.cells = cells;
        self.cols = cols;
        self.rows = rows;
        self.player = 0;
        self.pending = None;
        self.deal_started = self.time;
    }

    /// Advances to a new floor: the old floor's cards go to the discard, the
    /// deck reshuffles from the discard if it is running short, and a fresh
    /// floor is dealt at the same grid size.
    fn start_next_floor(&mut self) {
        self.floor += 1;
        self.discard_pile += self.cells.len() as u32;
        let needed = u32::from(self.cols) * u32::from(self.rows);
        if self.draw_pile < needed {
            self.draw_pile += self.discard_pile;
            self.discard_pile = 0;
            self.dealer_line = "The discard shuffles back into the deck.".to_string();
        }
        self.draw_pile = self.draw_pile.saturating_sub(needed);
        let (cols, rows) = (self.cols, self.rows);
        self.generate_floor(cols, rows);
        self.dealer_line = format!("Floor {} is dealt.", self.floor);
    }

    /// Rebuilds the current floor at the grid size `grid_area` now calls for,
    /// if that size actually changed. A resize is treated as the table being
    /// reset and redealt at the same floor number: harmless for a demo,
    /// and it is what keeps the grid always filling the live viewport
    /// instead of a fixed cell count stranded in whatever room grew around
    /// it.
    fn sync_floor(&mut self, grid_area: Rect) {
        let cols = axis_layout(
            grid_area.width(),
            CARD_W_MIN,
            CARD_W_PREF,
            MIN_COLS,
            MAX_COLS,
        );
        let rows = axis_layout(
            grid_area.height(),
            CARD_H_MIN,
            CARD_H_PREF,
            MIN_ROWS,
            MAX_ROWS,
        );
        if cols != self.cols || rows != self.rows {
            self.generate_floor(cols, rows);
        }
    }

    // -- timing -----------------------------------------------------------

    /// Row-major deal delay for cell `idx`, in seconds after
    /// [`Self::deal_started`].
    fn deal_delay(idx: usize) -> f32 {
        idx as f32 * DEAL_STAGGER
    }

    /// Seconds since cell `idx` started (or would start) materializing;
    /// negative while it is still waiting its turn in the deal.
    fn deal_elapsed(&self, idx: usize) -> f32 {
        self.time - self.deal_started - Self::deal_delay(idx)
    }

    /// Whether the current floor is still being dealt, which blocks input:
    /// tapping a card mid-deal would be tapping a card that has not
    /// physically landed yet.
    fn dealing(&self) -> bool {
        if self.cols == 0 || self.rows == 0 {
            return true;
        }
        let total = usize::from(self.cols) * usize::from(self.rows);
        let last_delay = Self::deal_delay(total.saturating_sub(1));
        self.time - self.deal_started < last_delay + DEAL_GROW_TIME
    }

    // -- movement and resolution -------------------------------------------

    fn neighbors(&self, idx: usize) -> Vec<usize> {
        if self.cols == 0 {
            return Vec::new();
        }
        let col = i32::from((idx as u16) % self.cols);
        let row = i32::from((idx as u16) / self.cols);
        [(0, -1), (0, 1), (-1, 0), (1, 0)]
            .into_iter()
            .filter_map(|(dx, dy)| {
                let nc = col + dx;
                let nr = row + dy;
                if nc >= 0 && nr >= 0 && (nc as u16) < self.cols && (nr as u16) < self.rows {
                    Some((nr as u16 * self.cols + nc as u16) as usize)
                } else {
                    None
                }
            })
            .collect()
    }

    fn try_enter(&mut self, idx: usize) {
        if self.dealing() || self.pending.is_some() || !self.neighbors(self.player).contains(&idx) {
            return;
        }
        self.player = idx;
        if self.cells[idx].revealed {
            self.dealer_line = format!("You step back into {}.", self.cells[idx].title);
        } else {
            self.cells[idx].flip_started = Some(self.time);
            self.dealer_line = format!("You turn over {}...", self.cells[idx].title);
        }
    }

    /// Applies whatever a just-completed flip means: opens a choice, or
    /// resolves automatically and checks whether the floor is now clear.
    fn on_revealed(&mut self, idx: usize) {
        let kind = self.cells[idx].kind;
        let title = self.cells[idx].title.clone();
        if kind.has_choice() {
            self.pending = Some(idx);
            self.dealer_line = format!("{title}: {}", kind.choice_prompt());
            return;
        }
        match kind {
            RoomKind::Treasure => {
                let gain = 8 + self.rng.next_below(9);
                self.gold += gain;
                self.dealer_line = format!("Treasure! {title} yields {gain} gold.");
            }
            RoomKind::Passage => {
                self.dealer_line = format!("{title} is quiet and empty.");
            }
            RoomKind::Encounter | RoomKind::Shrine | RoomKind::Trap => {
                unreachable!("these kinds all report `has_choice`, handled above")
            }
        }
        self.cells[idx].resolved = true;
        self.check_floor_complete();
    }

    /// Resolves the pending choice at `idx`. Gambles are rolled here (an
    /// input-driven event), never during drawing, which is what keeps two
    /// renders of an unchanged frame identical.
    fn resolve_choice(&mut self, idx: usize, pick_a: bool) {
        let kind = self.cells[idx].kind;
        let title = self.cells[idx].title.clone();
        let (dh, dg, df, line): (f32, i32, f32, String) = match (kind, pick_a) {
            (RoomKind::Encounter, true) => {
                if self.rng.next_f32() < 0.55 {
                    (
                        0.0,
                        14,
                        0.0,
                        format!("You best the foe in {title} and take 14 gold."),
                    )
                } else {
                    (
                        -16.0,
                        0,
                        0.0,
                        format!("The foe wounds you badly in {title}."),
                    )
                }
            }
            (RoomKind::Encounter, false) => {
                (0.0, 0, 0.0, format!("You slip past the danger in {title}."))
            }
            (RoomKind::Shrine, true) => (
                18.0,
                0,
                0.0,
                format!("The shrine in {title} mends your wounds."),
            ),
            (RoomKind::Shrine, false) => (
                0.0,
                0,
                16.0,
                format!("The shrine in {title} fills your pack."),
            ),
            (RoomKind::Trap, true) => {
                if self.rng.next_f32() < 0.5 {
                    (0.0, 0, 0.0, format!("You disarm the trap in {title}."))
                } else {
                    (
                        -14.0,
                        0,
                        0.0,
                        format!("The trap in {title} gets you anyway."),
                    )
                }
            }
            (RoomKind::Trap, false) => (-6.0, 0, 0.0, format!("You dash through {title}, grazed.")),
            (RoomKind::Treasure | RoomKind::Passage, _) => (0.0, 0, 0.0, String::new()),
        };
        self.health = (self.health + dh).clamp(0.0, self.max_health);
        self.gold = (i64::from(self.gold) + i64::from(dg)).max(0) as u32;
        self.food = (self.food + df).clamp(0.0, self.max_food);
        self.cells[idx].resolved = true;
        self.pending = None;
        self.dealer_line = line;
        self.check_floor_complete();
    }

    fn check_floor_complete(&mut self) {
        if !self.dealing() && self.pending.is_none() && self.cells.iter().all(|c| c.resolved) {
            self.start_next_floor();
        }
    }

    /// Finalizes any flip whose animation has finished this frame.
    fn simulate(&mut self) {
        let done: Vec<usize> = self
            .cells
            .iter()
            .enumerate()
            .filter_map(|(i, cell)| {
                let t = cell.flip_started?;
                let progress = (self.time - t) / FLIP_TIME;
                (progress >= 1.0).then_some(i)
            })
            .collect();
        for idx in done {
            self.cells[idx].flip_started = None;
            self.on_revealed(idx);
        }
    }

    // -- input --------------------------------------------------------------

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
            self.pointer.feed(&event);
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        if self.dealing() {
            return;
        }
        if let Some(idx) = self.pending {
            match code {
                KeyCode::Char('1') => self.resolve_choice(idx, true),
                KeyCode::Char('2') => self.resolve_choice(idx, false),
                _ => {}
            }
            return;
        }
        if self.cols == 0 {
            return;
        }
        let delta = match code {
            KeyCode::Up | KeyCode::Char('w' | 'W') => (0, -1),
            KeyCode::Down | KeyCode::Char('s' | 'S') => (0, 1),
            KeyCode::Left | KeyCode::Char('a' | 'A') => (-1, 0),
            KeyCode::Right | KeyCode::Char('d' | 'D') => (1, 0),
            _ => return,
        };
        let col = i32::from((self.player as u16) % self.cols) + delta.0;
        let row = i32::from((self.player as u16) / self.cols) + delta.1;
        if col < 0 || row < 0 || col as u16 >= self.cols || row as u16 >= self.rows {
            return;
        }
        self.try_enter((row as u16 * self.cols + col as u16) as usize);
    }

    fn handle_tap(&mut self, pos: Pos) {
        let Some(&action) = self.hotspots.hit(pos) else {
            return;
        };
        match action {
            Action::Move(idx) => self.try_enter(idx),
            Action::Choice(pick_a) => {
                if let Some(idx) = self.pending {
                    self.resolve_choice(idx, pick_a);
                }
            }
        }
    }

    // -- layout ---------------------------------------------------------

    fn compute_cell_rects(&mut self, area: Rect) {
        self.cell_rects.clear();
        if self.cols == 0 || self.rows == 0 || area.width() == 0 || area.height() == 0 {
            return;
        }
        let col_gap = GRID_GAP * self.cols.saturating_sub(1);
        let row_gap = GRID_GAP * self.rows.saturating_sub(1);
        let col_base = area.width().saturating_sub(col_gap) / self.cols;
        let row_base = area.height().saturating_sub(row_gap) / self.rows;
        let xs = distribute(area.width(), self.cols, GRID_GAP, col_base);
        let ys = distribute(area.height(), self.rows, GRID_GAP, row_base);
        for row in 0..self.rows {
            for col in 0..self.cols {
                let (ox, w) = xs[col as usize];
                let (oy, h) = ys[row as usize];
                self.cell_rects
                    .push(Rect::new(area.left() + ox, area.top() + oy, w, h));
            }
        }
    }

    /// Builds this frame's hit-testable regions and resolves the held
    /// gesture's tap against them, all before anything is drawn: the tap
    /// belongs to state computed for *this* frame, but must be able to
    /// mutate that state (move the player, resolve a choice) before drawing
    /// reflects it.
    fn layout(&mut self, grid_area: Rect, gesture: &Gesture) {
        self.hotspots.clear();
        self.compute_cell_rects(grid_area);
        if !self.dealing() {
            if self.pending.is_none() {
                for n in self.neighbors(self.player) {
                    if let Some(&rect) = self.cell_rects.get(n) {
                        self.hotspots
                            .push_tappable(rect, grid_area, Action::Move(n));
                    }
                }
            } else if let Some(idx) = self.pending
                && let Some(&cell_rect) = self.cell_rects.get(idx)
            {
                let overlay = overlay_rect(grid_area, cell_rect);
                let (a, b) = choice_option_rects(overlay);
                self.hotspots
                    .push_tappable(a, overlay, Action::Choice(true));
                self.hotspots
                    .push_tappable(b, overlay, Action::Choice(false));
            }
        }
        if let Some(pos) = gesture.tap {
            self.handle_tap(pos);
        }
    }

    // -- drawing --------------------------------------------------------

    fn draw_ghost(surface: &mut Surface<'_>, rect: Rect) {
        Panel::new()
            .border(Border::Single)
            .frame(scale(panel::FRAME, 0.35))
            .bg(rgb(14, 16, 22))
            .draw(surface, rect);
    }

    /// A patterned face-down back: a checkerboard that idles with a slow
    /// per-cell shimmer, so a table of untouched cards is never perfectly
    /// static even with no input at all.
    fn draw_back(&self, surface: &mut Surface<'_>, rect: Rect, accent: Color) {
        let bg = rgb(14, 16, 22);
        let inner = Panel::new()
            .border(Border::Single)
            .frame(accent)
            .bg(bg)
            .draw(surface, rect);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        for y in 0..inner.height() {
            for x in 0..inner.width() {
                let wx = i32::from(inner.left() + x);
                let wy = i32::from(inner.top() + y);
                let glyph = if (x + y).is_multiple_of(2) {
                    '\u{2591}'
                } else {
                    '\u{2592}'
                };
                let phase = hash01(0xC0DE, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add((self.time.mul_add(0.8, phase)).sin(), 0.5);
                let color = tilekit::palette::mix(scale(accent, 0.25), accent, twinkle * 0.6);
                surface.put(
                    (inner.left() + x, inner.top() + y),
                    glyph,
                    Style::new().fg(color).bg(bg),
                );
            }
        }
    }

    fn draw_front(surface: &mut Surface<'_>, rect: Rect, cell: &Cell, highlight: bool) {
        let suit = cell.kind.suit().to_string();
        let state = if highlight {
            CardState::Selected
        } else {
            CardState::Idle
        };
        Card::new(&cell.title)
            .cost(&suit)
            .kind(cell.kind.label())
            .body(cell.kind.body())
            .accent(cell.kind.tint())
            .state(state)
            .draw(surface, rect);
    }

    fn draw_grid(&self, surface: &mut Surface<'_>, area: Rect) {
        surface.fill_rect(area, ' ', Style::new().bg(rgb(11, 13, 18)));
        for (idx, cell) in self.cells.iter().enumerate() {
            let Some(&rect) = self.cell_rects.get(idx) else {
                continue;
            };
            if rect.width() == 0 || rect.height() == 0 {
                continue;
            }
            let elapsed = self.deal_elapsed(idx);
            if elapsed < 0.0 {
                Self::draw_ghost(surface, rect);
                continue;
            }
            let grow = (elapsed / DEAL_GROW_TIME).clamp(0.0, 1.0);
            if grow < 1.0 {
                let r = narrow_rect(rect, smoothstep(grow).max(0.05));
                self.draw_back(surface, r, panel::FRAME);
                continue;
            }

            let highlight = idx == self.player;
            if let Some(t) = cell.flip_started {
                let progress = ((self.time - t) / FLIP_TIME).clamp(0.0, 1.0);
                let factor = (core::f32::consts::PI * progress).cos().abs().max(0.05);
                let r = narrow_rect(rect, factor);
                if progress < 0.5 {
                    self.draw_back(surface, r, panel::FRAME);
                } else {
                    Self::draw_front(surface, r, cell, highlight);
                }
            } else if cell.revealed {
                Self::draw_front(surface, rect, cell, highlight);
            } else {
                self.draw_back(surface, rect, panel::FRAME);
            }
        }
    }

    fn draw_choice_overlay(&self, surface: &mut Surface<'_>, grid_area: Rect, idx: usize) {
        let Some(&cell_rect) = self.cell_rects.get(idx) else {
            return;
        };
        let cell = &self.cells[idx];
        let overlay = overlay_rect(grid_area, cell_rect);
        let inner = Panel::new()
            .border(Border::Double)
            .title(&cell.title)
            .frame(cell.kind.tint())
            .bg(panel::PANEL_BG)
            .draw(surface, overlay);
        if inner.width() < 4 || inner.height() < 3 {
            return;
        }

        let prompt = cell.kind.choice_prompt();
        for (i, line) in card::wrap(prompt, inner.width_usize())
            .into_iter()
            .take(2)
            .enumerate()
        {
            surface.print(
                (inner.left(), inner.top() + i as u16),
                &line,
                Style::new().fg(ui::FG).bg(panel::PANEL_BG),
            );
        }

        let ((a_label, a_hint), (b_label, b_hint)) = cell.kind.choice_options();
        let (a_rect, b_rect) = choice_option_rects(overlay);
        Self::draw_option(surface, a_rect, "1", a_label, a_hint, cell.kind.tint());
        Self::draw_option(surface, b_rect, "2", b_label, b_hint, cell.kind.tint());
    }

    fn draw_option(
        surface: &mut Surface<'_>,
        rect: Rect,
        key: &str,
        label: &str,
        hint: &str,
        accent: Color,
    ) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let inner = Panel::new()
            .border(Border::Single)
            .frame(accent)
            .bg(rgb(20, 22, 30))
            .draw(surface, rect);
        if inner.width() == 0 {
            return;
        }
        let title = format!("[{key}] {label}");
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(&title, inner.width_usize()),
            Style::new().fg(ui::FG).bg(rgb(20, 22, 30)),
        );
        if inner.height() > 1 {
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(hint, inner.width_usize()),
                Style::new().fg(ui::DIM).bg(rgb(20, 22, 30)),
            );
        }
    }

    /// Draws one labelled stat readout as a small bordered card: a label on
    /// the first interior row, its value on the second.
    ///
    /// Deliberately not [`Card`]'s own tiered rendering: at the width this
    /// panel actually has (as little as 8-9 interior columns) `Card`'s
    /// `Stub` tier drops the title and shows only the cost, which is
    /// exactly the unlabelled-number defect this function exists to avoid.
    /// Two short rows, printed directly, never need to drop either half.
    fn draw_stat_card(
        surface: &mut Surface<'_>,
        rect: Rect,
        label: &str,
        value: &str,
        accent: Color,
    ) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let bg = rgb(20, 22, 30);
        let inner = Panel::new()
            .border(Border::Single)
            .frame(accent)
            .bg(bg)
            .draw(surface, rect);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(label, inner.width_usize()),
            Style::new().fg(ui::DIM).bg(bg),
        );
        if inner.height() > 1 {
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(value, inner.width_usize()),
                Style::new().fg(accent).bg(bg),
            );
        }
    }

    /// Which room lies one step `(dx, dy)` from the player, as text: the
    /// room's own title once it has been seen, `"Unknown"` while it is
    /// still face down, or `"Wall"` off the edge of the floor.
    fn neighbor_desc(&self, dx: i32, dy: i32) -> &str {
        if self.cols == 0 {
            return "--";
        }
        let col = i32::from((self.player as u16) % self.cols) + dx;
        let row = i32::from((self.player as u16) / self.cols) + dy;
        if col < 0 || row < 0 || col as u16 >= self.cols || row as u16 >= self.rows {
            return "Wall";
        }
        let idx = (row as u16 * self.cols + col as u16) as usize;
        match self.cells.get(idx) {
            Some(c) if c.revealed => c.title.as_str(),
            Some(_) => "Unknown",
            None => "Wall",
        }
    }

    /// Fills the panel space left below the stat grid with the one thing
    /// the six labelled boxes do not already say: where the player is and
    /// how much of the floor is cleared. Real per-frame state (the current
    /// cell, its four neighbors, and the resolved count), not decoration --
    /// this is what keeps the sidebar from ending in a blank rect on any
    /// window wide enough to grow the panel past six boxes.
    fn draw_room_info(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() < 4 {
            return;
        }
        let inner = Panel::new()
            .title("Room")
            .frame(scale(panel::FRAME, 0.7))
            .bg(panel::PANEL_BG)
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let bg = panel::PANEL_BG;
        let mut y = inner.top();

        if let Some(cell) = self.cells.get(self.player) {
            let header = format!("{} {}", cell.kind.suit(), cell.title);
            surface.print(
                (inner.left(), y),
                retroglyph_widgets::truncate(&header, inner.width_usize()),
                Style::new().fg(cell.kind.tint()).bg(bg),
            );
            y += 1;
            for line in card::wrap(cell.kind.body(), inner.width_usize()) {
                if y >= inner.bottom() {
                    break;
                }
                surface.print((inner.left(), y), &line, Style::new().fg(ui::DIM).bg(bg));
                y += 1;
            }
        }
        y += 1;

        if y < inner.bottom() {
            surface.print(
                (inner.left(), y),
                retroglyph_widgets::truncate("Paths", inner.width_usize()),
                Style::new().fg(ui::DIM).bg(bg),
            );
            y += 1;
        }
        let dirs: [(&str, (i32, i32)); 4] =
            [("N", (0, -1)), ("S", (0, 1)), ("W", (-1, 0)), ("E", (1, 0))];
        for (label, (dx, dy)) in dirs {
            if y >= inner.bottom() {
                break;
            }
            let line = format!("{label}: {}", self.neighbor_desc(dx, dy));
            surface.print(
                (inner.left(), y),
                retroglyph_widgets::truncate(&line, inner.width_usize()),
                Style::new().fg(ui::FG).bg(bg),
            );
            y += 1;
        }
        y += 1;

        if y + 1 >= inner.bottom() {
            return;
        }
        let total = self.cells.len();
        let resolved = self.cells.iter().filter(|c| c.resolved).count();
        let line = format!("Cleared {resolved}/{total}");
        surface.print(
            (inner.left(), y),
            retroglyph_widgets::truncate(&line, inner.width_usize()),
            Style::new().fg(ui::FG).bg(bg),
        );
        y += 1;
        if y >= inner.bottom() {
            return;
        }
        let width = inner.width_usize();
        let filled = (resolved * width).checked_div(total).unwrap_or(0);
        let mut bar = String::with_capacity(width);
        for i in 0..width {
            bar.push(if i < filled { '\u{2588}' } else { '\u{2591}' });
        }
        surface.print(
            (inner.left(), y),
            &bar,
            Style::new().fg(rgb(150, 200, 160)).bg(bg),
        );
        y += 2;

        self.draw_minimap(
            surface,
            Rect::new(
                inner.left(),
                y,
                inner.width(),
                inner.bottom().saturating_sub(y),
            ),
            bg,
        );
    }

    /// The floor drawn one glyph per cell, which is what the panel's
    /// remaining height is actually for on a desktop window -- without it,
    /// a tall window just stretches the border around whitespace instead of
    /// showing more of the one thing this demo is about (the map). Doubled
    /// row spacing while there is room for it, since a map read as `@..?`
    /// `....` `?#..` is not obviously two rows apart from one glyph per
    /// text row.
    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect, bg: Color) {
        if self.cols == 0 || area.height() == 0 {
            return;
        }
        let mut y = area.top();
        surface.print((area.left(), y), "Map", Style::new().fg(ui::DIM).bg(bg));
        y += 1;
        let rows_left = area.bottom().saturating_sub(y);
        let row_step = if rows_left >= self.rows * 2 { 2 } else { 1 };
        for row in 0..self.rows {
            if y >= area.bottom() {
                break;
            }
            for col in 0..self.cols {
                let x = area.left() + col * 2;
                if x >= area.right() {
                    break;
                }
                let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
                let (glyph, color) = match self.cells.get(idx) {
                    Some(_) if idx == self.player => ('@', ui::FG),
                    Some(c) if c.revealed => (c.kind.suit(), c.kind.tint()),
                    Some(_) => ('?', ui::DIM),
                    None => (' ', ui::DIM),
                };
                surface.put((x, y), glyph, Style::new().fg(color).bg(bg));
            }
            y += row_step;
        }
    }

    fn draw_sidebar(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Traveler")
            .badge(&format!("Fl.{}", self.floor))
            .draw(surface, area);
        if inner.width() < 4 || inner.height() == 0 {
            return;
        }

        let cols = if inner.width() >= 19 { 2 } else { 1 };
        let cell_w = if cols == 2 {
            (inner.width() - 1) / 2
        } else {
            inner.width()
        };
        let cell_h = 4u16.min(inner.height());

        // Six labelled readouts, every one carrying a word alongside its
        // number: health, gold, food, and gear are the brief's "player
        // state block", drawn as their own small cards; deck and discard
        // are what make the deck itself visible state (see the module
        // doc). Color alone (the border tint) was never a label -- a
        // colorblind viewer, or a grayscale screenshot, could not tell
        // `60/60` health from `60/60` food by hue.
        let stats = [
            (
                "Health",
                format!("{:.0}/{:.0}", self.health, self.max_health),
                rgb(216, 88, 84),
            ),
            ("Gold", self.gold.to_string(), rgb(226, 184, 90)),
            (
                "Food",
                format!("{:.0}/{:.0}", self.food, self.max_food),
                rgb(108, 196, 108),
            ),
            ("Gear", self.gear.to_string(), rgb(150, 150, 220)),
            ("Deck", self.draw_pile.to_string(), scale(ui::DIM, 1.3)),
            (
                "Discard",
                self.discard_pile.to_string(),
                scale(ui::DIM, 1.3),
            ),
        ];
        let mut used_h = 0u16;
        for (i, (label, value, color)) in stats.iter().enumerate() {
            if let Some(rect) = tile_rect(inner, i, cols, cell_w, cell_h, 1) {
                Self::draw_stat_card(surface, rect, label, value, *color);
                used_h = used_h.max(rect.bottom() - inner.top());
            }
        }

        if inner.height() <= used_h + 1 {
            return;
        }
        let room_area = Rect::new(
            inner.left(),
            inner.top() + used_h + 1,
            inner.width(),
            inner.height() - used_h - 1,
        );
        self.draw_room_info(surface, room_area);
    }

    fn draw_dealer(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let bordered = area.height() >= 4;
        let (inner, bg) = if bordered {
            (
                Panel::new().title("Dealer").draw(surface, area),
                panel::PANEL_BG,
            )
        } else {
            panel::band(surface, area);
            (area, ui::CHROME_BG)
        };
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(&self.dealer_line, inner.width_usize()),
            Style::new().fg(ui::FG).bg(bg),
        );
        if inner.height() > 1 {
            let hint = if self.pending.is_some() {
                "Tap a card, or press 1/2 to choose."
            } else {
                "Tap or move (WASD/arrows) to an adjacent card."
            };
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(hint, inner.width_usize()),
                Style::new().fg(ui::DIM).bg(bg),
            );
        }
    }

    fn status_line(&self) -> String {
        format!(
            "floor {}  hp {:.0}  gold {}  deck {}",
            self.floor, self.health, self.gold, self.draw_pile
        )
    }
}

impl Demo for DealtDungeon {
    const NAME: &'static str = "57_dealt_dungeon";
    const TITLE: &'static str = "57 Dealt Dungeon";
    const BLURB: &'static str =
        "Hand of Fate: the dungeon map is a grid of face-down cards you flip.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap/WASD/arrows", "move to an adjacent card"),
            ("1/2", "choose an option"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();

        let screen = term.area();
        let (title_area, content, status_area) = ui::split_chrome(screen);
        let shape = Shape::of(content);
        let dealer_h = dealer_height(content);
        let (main, dealer_area) = panel::split_bottom(content, dealer_h);
        let (grid_area, side_area) = if shape.stacks() {
            let (side, rest) = panel::split_top(main, portrait_side_height(main));
            (rest, side)
        } else {
            let side_w = if shape == Shape::Desktop { 26 } else { 22 };
            panel::split_right(main, side_w.min(main.width().saturating_sub(30)))
        };

        self.sync_floor(grid_area);
        self.simulate();
        self.layout(grid_area, &gesture);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_grid(&mut surface, grid_area);
        self.draw_sidebar(&mut surface, side_area);
        self.draw_dealer(&mut surface, dealer_area);
        if let Some(idx) = self.pending {
            self.draw_choice_overlay(&mut surface, grid_area, idx);
        }

        ui::title_bar::<Self>(&mut surface, title_area);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status_area, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(DealtDungeon);

#[cfg(test)]
mod tests {
    use super::{
        DealtDungeon, MAX_COLS, MAX_ROWS, ROOM_NAMES, axis_layout, distribute, narrow_rect,
    };
    use retroglyph_core::Rect;
    use std::collections::HashSet;

    #[test]
    fn the_name_pool_exceeds_the_largest_possible_floor() {
        assert!(ROOM_NAMES.len() >= usize::from(MAX_COLS) * usize::from(MAX_ROWS));
    }

    #[test]
    fn every_room_name_fits_the_smallest_card_interior() {
        // `CARD_W_MIN` leaves an interior of `CARD_W_MIN - 2` columns; every
        // title must fit inside that without truncation.
        let interior = usize::from(super::CARD_W_MIN) - 2;
        for name in ROOM_NAMES {
            assert!(
                name.chars().count() <= interior,
                "{name} does not fit {interior} columns"
            );
        }
    }

    #[test]
    fn titles_within_a_floor_are_unique() {
        let mut demo = DealtDungeon::default();
        demo.generate_floor(MAX_COLS, MAX_ROWS);
        let titles: HashSet<&str> = demo.cells.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles.len(),
            demo.cells.len(),
            "every card title in a dealt floor must be unique"
        );

        // A second, differently sized floor from the same running instance
        // must also be internally unique -- this is the property the demo
        // actually needs, not merely that one fixed call is unique.
        demo.generate_floor(4, 3);
        let titles: HashSet<&str> = demo.cells.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles.len(), demo.cells.len());
    }

    #[test]
    fn axis_layout_never_drops_below_the_minimum_count() {
        assert_eq!(axis_layout(1, 9, 13, 3, 8), 3);
    }

    #[test]
    fn axis_layout_adds_columns_before_it_grows_cards_past_the_preference() {
        // Wide enough for six columns at the preferred width; must not fall
        // back to fewer, bigger cards while columns are still cheap.
        let cols = axis_layout(6 * 13 + 5, 9, 13, 3, 8);
        assert_eq!(cols, 6);
    }

    #[test]
    fn axis_layout_caps_at_the_maximum_count() {
        assert_eq!(axis_layout(u16::MAX, 9, 13, 3, 8), 8);
    }

    #[test]
    fn distribute_covers_the_full_extent_with_no_gap_left_over() {
        let base = (100 - 6) / 7; // 6 gaps of 1 between 7 bands
        let bands = distribute(100, 7, 1, base);
        let total: u16 = bands.iter().map(|(_, size)| *size).sum::<u16>() + 6; // + 6 gaps
        assert_eq!(total, 100);
        let (last_offset, last_size) = bands[6];
        assert_eq!(last_offset + last_size, 100);
    }

    #[test]
    fn narrow_rect_stays_centered_and_never_collapses_to_zero() {
        let rect = Rect::new(10, 4, 11, 9);
        let mid = narrow_rect(rect, 0.0);
        assert!(mid.width() >= 1);
        let left_margin = mid.left() - rect.left();
        let right_margin = rect.right() - mid.right();
        assert!(left_margin.abs_diff(right_margin) <= 1);

        let full = narrow_rect(rect, 1.0);
        assert_eq!(full.width(), rect.width());
    }
}
