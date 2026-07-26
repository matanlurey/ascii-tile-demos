//! 28: Spire deck -- a torch-lit card battle, played entirely with a thumb.
//!
//! Slay the Spire on a phone is the reference this demo is built against, and
//! the two things that make its mobile port work are the two things this demo
//! exists to show: every enemy tells you what it is about to do before it does
//! it, and every card that costs more than you can pay renders that way
//! *before* you reach for it. Everything else here (the dungeon palette, the
//! block-built energy numeral, the fanned hand) is in service of those two
//! ideas staying legible on a screen the size of a paperback.
//!
//! Techniques on show:
//!
//! - **Telegraphed intent** ([`Intent`], [`intent_display`]): each enemy shows
//!   an icon and a number for what it does *next* turn, not what it is doing
//!   now. On a desktop strategy game a threat range or a highlighted tile can
//!   carry this; on a character grid at phone width there is no room for
//!   either, so the number has to do all the work. Telegraphing matters more
//!   here than on a big screen precisely because there is nowhere else for
//!   the warning to live -- no tooltip, no hover state, no second monitor with
//!   a wiki open. If the icon is wrong or missing, the only way to learn what
//!   an enemy does is to eat the hit once, and a game that teaches its rules
//!   by punishing the player once is a game that feels unfair on the exact
//!   device it is least forgiving on.
//! - **Cards from [`ui::card`]**: [`card::fan`] lays out the hand, overlapping
//!   once it does not fit rather than shrinking every card below a readable
//!   tier (see that module's doc comment for why). The selected or held card
//!   is always drawn, and its hotspot always registered, *last* -- both
//!   overlap and hit-testing resolve top-to-bottom in registration order, so
//!   drawing it last is what makes "the card on top" and "the card that
//!   answers a tap" the same card. Getting that ordering backwards is the
//!   single easiest way to build a hand that looks right and plays wrong.
//! - **[`CardState::Disabled`] for unaffordable cards**: a card costing more
//!   than the player's current energy dims rather than disappears. Hiding it
//!   would save no space worth having and would cost the player the ability
//!   to plan two cards ahead ("if I play Defend now I can't afford Bash");
//!   removing it would make the hand jump size, breaking every remembered tap
//!   position an instant before the next tap lands. Dimming says "not now"
//!   without saying "never" or "gone".
//! - **Tap-select-then-tap-target as the primary path, drag as the
//!   alternate** ([`SpireDeck::handle_gesture`]): tapping a card selects it,
//!   tapping an enemy plays it there; tapping the same card again or empty
//!   space deselects. A card that needs no target (a block/defend card)
//!   resolves on the very first tap, because asking the player to aim at
//!   nothing would just be a second tap for no information. Drag also works
//!   -- press a card, drag it onto an enemy, release to play -- but it is the
//!   secondary path on purpose: a thumb dragged over a 9x8-cell enemy portrait
//!   covers the exact thing it is trying to aim at, which is the classic
//!   failure mode of drag-to-target on a small touchscreen. Two taps let the
//!   player see the whole board between them.
//! - **A drawn pointer line while dragging** ([`draw_pointer_line`]): the one
//!   piece of information drag needs that tap-select does not, since a finger
//!   commits to *some* point on the glass, but that point is usually still
//!   under the finger when it releases. The line, plus a highlighted border on
//!   whichever enemy the pointer is currently over, is what tells the player
//!   where the drag will land before they let go.
//! - **A block-built energy numeral** ([`draw_numeral`]): energy is the
//!   resource every card in the hand is priced against, so it is drawn larger
//!   than anything else on screen -- a 3x5-cell digit built from `\u{2588}`
//!   rather than a printed character -- while HP and block stay as ordinary
//!   text. The asymmetry is deliberate: HP is read, energy is *counted*, and
//!   counting is what a big glyph is for.
//! - **Card [`Tier`] as a phone-viewport concession**: on the portrait shape a
//!   hand of five cards gets the full 11x9 layout with body text; squeezed
//!   into a landscape phone's shallow rows it drops to the 9x5 compact tier
//!   automatically, because [`Card::draw`] picks the largest tier that fits
//!   the rect it is handed. Nothing in this file branches on `Shape` to make
//!   that happen -- the tier system already is the portrait-phone answer, so
//!   the layout code only has to decide *how much rectangle* the hand gets,
//!   never how to redraw it smaller.
//!
//! ```sh
//! cargo run --example 28_spire_deck --features crossterm
//! cargo run --example 28_spire_deck --features software
//! cargo run --example 28_spire_deck --features gl
//! cargo run --example 28_spire_deck  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{self, Card, CardState};
use ascii_tile_demos::ui::panel::{self, Log};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::Rng;
use tilekit::palette::{mix, rgb, scale};

/// How many cards a full hand holds.
///
/// Five rather than the ten Slay the Spire itself deals, because at hand-size
/// ten the fan (see [`card::fan`]) would overlap down to stub width on every
/// shape this demo has to survive, including the 80x24 headless grid the
/// snapshot tests pin. Five keeps at least the compact tier readable on a
/// landscape phone without shrinking the deck to the point that the fan
/// technique has nothing left to demonstrate.
const HAND_SIZE: usize = 5;

/// How many enemies stand in one encounter.
const ENEMY_COUNT: usize = 3;

/// Rows reserved for the bottom control strip: player status, the energy
/// numeral, both pile counts, and the End Turn button.
///
/// Five rows for the numeral (see [`draw_numeral`]) plus two for labels above
/// and below it -- the numeral is the tallest thing in the strip, so it sets
/// the budget everything else in the row has to fit inside.
const CONTROL_H: u16 = 7;

/// Width of the End Turn button.
///
/// Comfortably past [`touch::TAP_W`](ascii_tile_demos::ui::touch::TAP_W): the
/// brief for this button is "impossible to mis-tap", and a button exactly at
/// the tap minimum is one slipped finger away from missing. Bigger than it
/// needs to be is the point.
const END_TURN_W: u16 = 16;

/// Columns of dead space kept between End Turn and whatever sits to its left.
///
/// This is the whole reason End Turn is drawn last and measured from the
/// right edge inward: irreversible actions (ending a turn commits every
/// enemy's telegraphed intent) need a moat around them, not just a border,
/// because a mis-tap that lands one cell short of the *intended* target
/// should land on nothing rather than on the discard pile's hotspot.
const END_TURN_GAP: u16 = 3;

/// Fixed height of the enemy row. Tall enough for the full 5-row portrait
/// plus an intent line, a name line, and an HP line (see [`SpireDeck::draw_enemy`]);
/// unlike the hand band this does not grow on a tall viewport, because a
/// bigger portrait would not show anything a player needs that the fixed size
/// doesn't already -- the spare room on a portrait phone is better spent on
/// the combat log, which has unbounded content.
const ENEMY_ROW_H: u16 = 10;

// ── Cards ─────────────────────────────────────────────────────────────────

/// What playing a card does.
#[derive(Clone, Copy)]
enum Effect {
    /// Damage to one chosen enemy.
    Damage(u32),
    /// Damage to every enemy at once; never needs a target.
    DamageAll(u32),
    /// Block for the player; never needs a target.
    Block(u32),
    /// Damage to one enemy, plus block for the player.
    DamageAndBlock(u32, u32),
}

/// A card's fixed definition. The hand and both piles hold indices into
/// [`CARD_POOL`] rather than owned copies, since nothing about a card
/// changes once the game starts -- only how many copies are where.
struct CardDef {
    name: &'static str,
    cost: u8,
    cost_str: &'static str,
    kind: &'static str,
    body: &'static str,
    accent: Color,
    /// Whether playing this card requires picking an enemy. Determines
    /// whether a tap on the card alone resolves it (see
    /// [`SpireDeck::tap_card`]) or must wait for a second tap on a target.
    targeted: bool,
    effect: Effect,
}

/// The five card types in play, two copies of each in the starting deck.
/// A small, fixed pool rather than a card-builder: the point on show is the
/// hand/targeting interaction, not deckbuilding depth.
const CARD_POOL: [CardDef; 5] = [
    CardDef {
        name: "Strike",
        cost: 1,
        cost_str: "1",
        kind: "Attack",
        body: "Deal 6 damage.",
        accent: rgb(214, 110, 90),
        targeted: true,
        effect: Effect::Damage(6),
    },
    CardDef {
        name: "Bash",
        cost: 2,
        cost_str: "2",
        kind: "Attack",
        body: "Deal 10 damage.",
        accent: rgb(214, 110, 90),
        targeted: true,
        effect: Effect::Damage(10),
    },
    CardDef {
        name: "Defend",
        cost: 1,
        cost_str: "1",
        kind: "Skill",
        body: "Gain 5 block.",
        accent: rgb(96, 150, 214),
        targeted: false,
        effect: Effect::Block(5),
    },
    CardDef {
        name: "Cleave",
        cost: 1,
        cost_str: "1",
        kind: "Attack",
        body: "Deal 4 dmg to all.",
        accent: rgb(214, 110, 90),
        targeted: false,
        effect: Effect::DamageAll(4),
    },
    CardDef {
        name: "Iron Wave",
        cost: 1,
        cost_str: "1",
        kind: "Attack",
        body: "Deal 5. Gain 5 block.",
        accent: rgb(196, 150, 90),
        targeted: true,
        effect: Effect::DamageAndBlock(5, 5),
    },
];

/// Shuffles `cards` in place with a Fisher-Yates pass driven by `rng`.
///
/// Deck order matters for what gets drawn next, so it has to come from the
/// same seeded generator every other piece of game state does -- see the
/// crate-wide rule against wall-clock seeding, which is what keeps a demo
/// rendered twice in a row producing identical frames.
fn shuffle(cards: &mut [usize], rng: &mut Rng) {
    for i in (1..cards.len()).rev() {
        let j = rng.next_below(i as u32 + 1) as usize;
        cards.swap(i, j);
    }
}

// ── Enemies ───────────────────────────────────────────────────────────────

/// Which telegraphed action an enemy will take at the next End Turn.
///
/// Carried as data rather than folded into flavor text, because it also
/// drives targeting priority for the reasoning above the icon-and-number: a
/// player deciding which enemy to block against reads the *number*, not a
/// sentence.
#[derive(Clone, Copy)]
enum Intent {
    /// Attacks the player for this much, after block.
    Attack(u32),
    /// Gains this much block for itself.
    Defend(u32),
    /// Raises this enemy's future attack rolls.
    Buff,
}

/// The icon, number text, and color an [`Intent`] displays as.
///
/// A single lookup used everywhere an intent is drawn, so the icon vocabulary
/// (attack = a solid triangle, defend = a block glyph, buff = an up arrow)
/// stays consistent between the full and compact enemy layouts.
fn intent_display(intent: Intent) -> (char, String, Color) {
    match intent {
        Intent::Attack(n) => ('\u{25BA}', format!("{n}"), rgb(220, 96, 90)),
        Intent::Defend(n) => ('\u{25A0}', format!("{n}"), rgb(120, 170, 214)),
        Intent::Buff => ('\u{2191}', String::new(), rgb(214, 176, 96)),
    }
}

/// Which monster an [`Enemy`] is, fixing its name, portrait, and color.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnemyKind {
    Cultist,
    Ooze,
    Golem,
}

impl EnemyKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Cultist => "Cultist",
            Self::Ooze => "Ooze",
            Self::Golem => "Golem",
        }
    }

    const fn accent(self) -> Color {
        match self {
            Self::Cultist => rgb(196, 150, 214),
            Self::Ooze => rgb(126, 196, 120),
            Self::Golem => rgb(170, 170, 180),
        }
    }

    const fn max_hp(self) -> i32 {
        match self {
            Self::Cultist => 48,
            Self::Ooze => 38,
            Self::Golem => 62,
        }
    }

    /// A 9-wide, 5-tall ASCII portrait, the "multi-cell figure" this demo's
    /// brief asks for. Every glyph here is plain ASCII or one of the block
    /// characters from the CP437-safe set; nothing outside either renders as
    /// a colored shape on the pixel backends (see the crate-wide CP437 rule).
    const fn art(self) -> [&'static str; 5] {
        match self {
            Self::Cultist => [
                "   /^\\   ",
                "  /   \\  ",
                " | o o | ",
                " |  v  | ",
                "  \\___/  ",
            ],
            Self::Ooze => [
                "  _____  ",
                " /\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}\\ ",
                "| o   o |",
                " \\\u{2593}\u{2593}\u{2593}\u{2593}\u{2593}/ ",
                "  -----  ",
            ],
            Self::Golem => [
                "  \u{2584}\u{2584}\u{2584}\u{2584}\u{2584}  ",
                " \u{2588} \u{25A0} \u{25A0} \u{2588} ",
                " \u{2588}\u{2584}\u{2584}\u{2584}\u{2584}\u{2584}\u{2588} ",
                "\u{2588}\u{258C}     \u{2590}\u{2588}",
                "\u{2588}       \u{2588}",
            ],
        }
    }
}

/// One monster in the encounter.
struct Enemy {
    kind: EnemyKind,
    hp: i32,
    max_hp: i32,
    block: i32,
    /// Accumulated from [`Intent::Buff`], added to every future attack roll.
    attack_bonus: i32,
    intent: Intent,
}

impl Enemy {
    fn new(kind: EnemyKind, rng: &mut Rng) -> Self {
        let max_hp = kind.max_hp();
        let mut enemy = Self {
            kind,
            hp: max_hp,
            max_hp,
            block: 0,
            attack_bonus: 0,
            intent: Intent::Attack(0),
        };
        enemy.intent = pick_intent(kind, enemy.attack_bonus, rng);
        enemy
    }

    const fn alive(&self) -> bool {
        self.hp > 0
    }
}

/// Rolls a new [`Intent`] for `kind`, biased by its flavor: the Golem mostly
/// defends and buffs, the Ooze mostly attacks, the Cultist splits the
/// difference. `attack_bonus` (from past [`Intent::Buff`] turns) scales any
/// attack rolled this way, so a buffed enemy's telegraph already shows the
/// bigger number before it swings.
fn pick_intent(kind: EnemyKind, attack_bonus: i32, rng: &mut Rng) -> Intent {
    let roll = rng.next_f32();
    let (attack_chance, defend_chance) = match kind {
        EnemyKind::Ooze => (0.75, 0.15),
        EnemyKind::Cultist => (0.55, 0.15),
        EnemyKind::Golem => (0.35, 0.40),
    };
    if roll < attack_chance {
        let base = 6 + rng.next_below(7) as i32 + attack_bonus;
        Intent::Attack(base.max(1) as u32)
    } else if roll < attack_chance + defend_chance {
        Intent::Defend(4 + rng.next_below(5))
    } else {
        Intent::Buff
    }
}

// ── Player ────────────────────────────────────────────────────────────────

/// The player's own resources: health, this-turn block, and this-turn energy.
struct Player {
    hp: i32,
    max_hp: i32,
    block: i32,
    energy: u8,
    max_energy: u8,
}

// ── Digits built from blocks ────────────────────────────────────────────────

/// A 3-wide, 5-tall glyph for each decimal digit, drawn with `\u{2588}` (full
/// block). This is what makes the energy counter read as a *number you count*
/// rather than a character you read: at three cells wide it is legible from
/// across a phone screen the way a single glyph in the default font cannot be.
const DIGIT_GLYPHS: [[&str; 5]; 10] = [
    [
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "  \u{2588}",
        "  \u{2588}",
        "  \u{2588}",
        "  \u{2588}",
        "  \u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588}  ",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "\u{2588} \u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "  \u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588}  ",
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588}  ",
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "  \u{2588}",
        "  \u{2588}",
        "  \u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
    [
        "\u{2588}\u{2588}\u{2588}",
        "\u{2588} \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
        "  \u{2588}",
        "\u{2588}\u{2588}\u{2588}",
    ],
];

/// Draws `value` as stacked block-digit glyphs starting at `at`, returning the
/// width consumed.
fn draw_numeral(surface: &mut Surface<'_>, at: (u16, u16), value: u8, color: Color, bg: Color) {
    let text = format!("{value}");
    let style = Style::new().fg(color).bg(bg);
    for (i, ch) in text.chars().enumerate() {
        let Some(digit) = ch.to_digit(10) else {
            continue;
        };
        let x = at.0 + i as u16 * 4;
        for (row, line) in DIGIT_GLYPHS[digit as usize].iter().enumerate() {
            surface.print((x, at.1 + row as u16), line, style);
        }
    }
}

// ── Pointer-drag arrow ──────────────────────────────────────────────────────

/// Picks a directional arrow glyph for a line running `(dx, dy)`.
///
/// Chooses whichever axis dominates rather than blending eight true
/// octants: on a character grid a cell is roughly twice as tall as it is
/// wide (see `ui::touch`'s cell-geometry derivation), so a diagonal drag
/// reads more like a compass heading than a precise angle, and four choices
/// is enough to say "up-ish", "down-ish", "left", or "right".
const fn arrow_glyph(dx: i32, dy: i32) -> char {
    if dx.abs() * 2 >= dy.abs() {
        if dx >= 0 { '\u{25BA}' } else { '\u{25C4}' }
    } else if dy >= 0 {
        '\u{25BC}'
    } else {
        '\u{25B2}'
    }
}

/// Draws a dotted line from `from` to `to` with an arrowhead at `to`.
///
/// This is the one piece of information a drag needs and a tap-select does
/// not: a finger commits to *a* point on the glass, and that point is
/// usually still hidden under the finger itself when the drag releases. The
/// line is what tells the player where the card is actually aimed before
/// they let go, standing in for the pointer arrow a mouse-only interface
/// gets for free.
fn draw_pointer_line(surface: &mut Surface<'_>, from: Pos, to: Pos, color: Color, bg: Color) {
    let (x0, y0) = (i32::from(from.x), i32::from(from.y));
    let (x1, y1) = (i32::from(to.x), i32::from(to.y));
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    let style = Style::new().fg(color).bg(bg);

    loop {
        if (x, y) != (x0, y0) && (x, y) != (x1, y1) {
            surface.put((x as u16, y as u16), '.', style);
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    surface.put((x1 as u16, y1 as u16), arrow_glyph(x1 - x0, y1 - y0), style);
}

// ── Interaction ──────────────────────────────────────────────────────────

/// What a registered hit region means, looked up by [`Hotspots::hit`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// A card in the hand, by index.
    Card(usize),
    /// An enemy, by index.
    Enemy(usize),
    EndTurn,
}

/// State for the whole encounter.
pub struct SpireDeck {
    player: Player,
    enemies: Vec<Enemy>,
    hand: Vec<usize>,
    draw_pile: Vec<usize>,
    discard_pile: Vec<usize>,

    /// The hand-card index selected via tap-select-then-tap-target, or via
    /// the 1-9 keys. `None` means nothing is queued.
    selected: Option<usize>,
    /// The enemy currently aimed at, whether by drag, arrow keys, or a plain
    /// tap on an enemy with no card selected (harmless eyeballing).
    target: Option<usize>,
    /// The hand-card index a press landed on, kept until the pointer either
    /// releases as a tap or moves far enough to become [`Self::drag_card`].
    press_card: Option<usize>,
    /// The hand-card index actively being dragged, if a press has moved past
    /// the tap slop. Distinct from `press_card` so a tap on a card and a
    /// drag from a card can be told apart before the gesture resolves.
    drag_card: Option<usize>,
    /// The live pointer position while `drag_card` is held, for
    /// [`draw_pointer_line`]. Cleared whenever the drag ends.
    drag_pos: Option<Pos>,

    pointer: Pointer,
    hotspots: Hotspots<Action>,

    log: Log,
    turn: u32,
    time: f32,
    rng: Rng,
    fps: FpsMeter,
}

impl Default for SpireDeck {
    fn default() -> Self {
        // Fixed rather than time-seeded: every backend, including the
        // headless renderer the snapshot tests pin, must reach the same
        // opening hand and the same first round of enemy intents.
        let mut rng = Rng::new(0x5310_4EC4);

        let mut draw_pile: Vec<usize> = (0..CARD_POOL.len())
            .flat_map(|i| core::iter::repeat_n(i, 2))
            .collect();
        shuffle(&mut draw_pile, &mut rng);

        let mut hand = Vec::new();
        let mut discard_pile = Vec::new();
        draw_n(
            &mut hand,
            &mut draw_pile,
            &mut discard_pile,
            HAND_SIZE,
            &mut rng,
            None,
        );

        let kinds = [EnemyKind::Cultist, EnemyKind::Ooze, EnemyKind::Golem];
        let mut enemies = Vec::with_capacity(ENEMY_COUNT);
        for kind in kinds.into_iter().take(ENEMY_COUNT) {
            enemies.push(Enemy::new(kind, &mut rng));
        }

        let mut log = Log::new(48);
        log.push(
            "The torches gutter. Three shapes bar the stairwell.",
            ui::ACCENT,
        );
        log.push(
            "Tap a card, then tap a target. Or drag one onto an enemy.",
            ui::DIM,
        );

        Self {
            player: Player {
                hp: 68,
                max_hp: 68,
                block: 0,
                energy: 3,
                max_energy: 3,
            },
            enemies,
            hand,
            draw_pile,
            discard_pile,
            selected: None,
            target: None,
            press_card: None,
            drag_card: None,
            drag_pos: None,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            log,
            turn: 1,
            time: 0.0,
            rng,
            fps: FpsMeter::new(),
        }
    }
}

/// Draws up to `n` cards from `draw_pile` into `hand`, reshuffling
/// `discard_pile` back in when the draw pile runs dry. `log` is optional so
/// the same helper serves the silent initial deal and the logged mid-game
/// reshuffle.
fn draw_n(
    hand: &mut Vec<usize>,
    draw_pile: &mut Vec<usize>,
    discard_pile: &mut Vec<usize>,
    n: usize,
    rng: &mut Rng,
    mut log: Option<&mut Log>,
) {
    for _ in 0..n {
        if draw_pile.is_empty() {
            if discard_pile.is_empty() {
                break;
            }
            draw_pile.append(discard_pile);
            shuffle(draw_pile, rng);
            if let Some(log) = log.as_deref_mut() {
                log.push("Reshuffling the discard pile into the draw pile.", ui::DIM);
            }
        }
        if let Some(card) = draw_pile.pop() {
            hand.push(card);
        }
    }
}

impl SpireDeck {
    /// Applies one card's effect, spending its energy cost, discarding it
    /// from the hand, and logging what happened. No-ops (with a log line)
    /// if the player cannot afford it or a targeted card has no target yet
    /// -- the latter should be unreachable from the tap/drag paths, which
    /// both supply a target before calling this, but keyboard Enter can
    /// still reach it with `target` unset.
    fn resolve_play(&mut self, hand_idx: usize, target: Option<usize>) {
        let Some(&card_id) = self.hand.get(hand_idx) else {
            return;
        };
        let def = &CARD_POOL[card_id];
        if def.cost > self.player.energy {
            self.log
                .push(format!("Not enough energy for {}.", def.name), ui::DIM);
            return;
        }
        if def.targeted && target.is_none() {
            self.log.push(
                format!("{} needs a target -- tap an enemy.", def.name),
                ui::DIM,
            );
            self.selected = Some(hand_idx);
            return;
        }

        self.player.energy -= def.cost;
        match def.effect {
            Effect::Damage(n) => {
                if let Some(t) = target {
                    let name = self.damage_enemy(t, n);
                    self.log.push(
                        format!("Played {} on the {name} for {n} damage.", def.name),
                        def.accent,
                    );
                }
            }
            Effect::DamageAll(n) => {
                for t in 0..self.enemies.len() {
                    self.damage_enemy(t, n);
                }
                self.log.push(
                    format!("Played {}: {n} damage to every enemy.", def.name),
                    def.accent,
                );
            }
            Effect::Block(n) => {
                self.player.block += i32::try_from(n).unwrap_or(i32::MAX);
                self.log.push(
                    format!("Played {}: gained {n} block.", def.name),
                    def.accent,
                );
            }
            Effect::DamageAndBlock(dmg, blk) => {
                if let Some(t) = target {
                    let name = self.damage_enemy(t, dmg);
                    self.player.block += i32::try_from(blk).unwrap_or(i32::MAX);
                    self.log.push(
                        format!(
                            "Played {} on the {name}: {dmg} damage, {blk} block.",
                            def.name
                        ),
                        def.accent,
                    );
                }
            }
        }

        self.hand.remove(hand_idx);
        self.discard_pile.push(card_id);
        self.selected = None;
        self.target = None;
    }

    /// Applies `n` damage to enemy `t`, block first, and returns its name for
    /// the log line. Clamps at 0 rather than allowing overkill into the
    /// negatives, which would otherwise make a dead enemy's HP bar report
    /// nonsense if a second card lands on it before End Turn clears it out.
    fn damage_enemy(&mut self, t: usize, n: u32) -> &'static str {
        let Some(enemy) = self.enemies.get_mut(t) else {
            return "enemy";
        };
        let mut dmg = i32::try_from(n).unwrap_or(i32::MAX);
        if enemy.block > 0 {
            let absorbed = dmg.min(enemy.block);
            enemy.block -= absorbed;
            dmg -= absorbed;
        }
        enemy.hp = (enemy.hp - dmg).max(0);
        enemy.kind.name()
    }

    /// Tapping a card: deselect if it was already selected, resolve
    /// immediately if it needs no target (a block/defend card has nothing
    /// left to ask), otherwise select it and wait for a target tap. This is
    /// the primary interaction path; see the module doc for why it beats
    /// drag on a small screen.
    fn tap_card(&mut self, i: usize) {
        let Some(&card_id) = self.hand.get(i) else {
            return;
        };
        let def = &CARD_POOL[card_id];
        if def.cost > self.player.energy {
            self.log.push(
                format!(
                    "{} costs {} energy -- can't afford it yet.",
                    def.name, def.cost
                ),
                ui::DIM,
            );
            return;
        }
        if self.selected == Some(i) {
            self.selected = None;
            self.target = None;
            return;
        }
        if def.targeted {
            self.selected = Some(i);
            if self.target.is_none_or(|t| t >= self.enemies.len()) {
                self.target = Some(0);
            }
        } else {
            self.resolve_play(i, None);
        }
    }

    /// Tapping an enemy: plays the selected card on it if one is queued,
    /// otherwise just marks it as the eyeballed target (harmless, and what
    /// lets arrow-key targeting start from wherever the player last looked).
    fn tap_enemy(&mut self, j: usize) {
        if let Some(i) = self.selected {
            self.resolve_play(i, Some(j));
        } else {
            self.target = Some(j);
        }
    }

    /// Resolves every enemy's telegraphed [`Intent`], resets block (block
    /// only protects through the enemy's turn, the same rule Slay the Spire
    /// itself uses), discards the hand, draws a fresh one, and rolls new
    /// intents for next turn.
    fn end_turn(&mut self) {
        self.log
            .push(format!("-- End of turn {} --", self.turn), ui::DIM);
        for i in 0..self.enemies.len() {
            if !self.enemies[i].alive() {
                continue;
            }
            match self.enemies[i].intent {
                Intent::Attack(n) => {
                    let mut dmg = i32::try_from(n).unwrap_or(i32::MAX);
                    if self.player.block > 0 {
                        let absorbed = dmg.min(self.player.block);
                        self.player.block -= absorbed;
                        dmg -= absorbed;
                    }
                    self.player.hp = (self.player.hp - dmg).max(0);
                    self.log.push(
                        format!("{} attacks for {n}.", self.enemies[i].kind.name()),
                        rgb(220, 96, 90),
                    );
                }
                Intent::Defend(n) => {
                    self.enemies[i].block += i32::try_from(n).unwrap_or(i32::MAX);
                    self.log.push(
                        format!("{} braces, gaining {n} block.", self.enemies[i].kind.name()),
                        rgb(120, 170, 214),
                    );
                }
                Intent::Buff => {
                    self.enemies[i].attack_bonus += 3;
                    self.log.push(
                        format!(
                            "{} channels power for its next strike.",
                            self.enemies[i].kind.name()
                        ),
                        rgb(214, 176, 96),
                    );
                }
            }
        }

        self.player.block = 0;
        self.player.energy = self.player.max_energy;
        self.discard_pile.append(&mut self.hand);
        draw_n(
            &mut self.hand,
            &mut self.draw_pile,
            &mut self.discard_pile,
            HAND_SIZE,
            &mut self.rng,
            Some(&mut self.log),
        );

        for enemy in &mut self.enemies {
            if enemy.alive() {
                enemy.intent = pick_intent(enemy.kind, enemy.attack_bonus, &mut self.rng);
            }
        }

        self.turn += 1;
        self.selected = None;
        self.target = None;
        self.drag_card = None;
        self.press_card = None;
        self.drag_pos = None;
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c @ '1'..='9') => {
                let idx = usize::from(c as u8 - b'1');
                self.tap_card(idx);
            }
            KeyCode::Left | KeyCode::Up => self.nudge_target(-1),
            KeyCode::Right | KeyCode::Down => self.nudge_target(1),
            KeyCode::Enter => {
                if let Some(i) = self.selected {
                    self.resolve_play(i, self.target);
                }
            }
            KeyCode::Char('e' | 'E') => self.end_turn(),
            _ => {}
        }
    }

    /// Moves the keyboard target by `dir` (`-1` or `1`), wrapping. Arrow keys
    /// are the keyboard's counterpart to dragging the pointer across enemies
    /// -- see [`Demo::keys`] -- and exist so the whole card/target flow has a
    /// keyboard-only path, not just a touch one.
    fn nudge_target(&mut self, dir: i32) {
        if self.enemies.is_empty() {
            return;
        }
        let n = self.enemies.len() as i32;
        let cur = self.target.map_or(0, |t| t as i32);
        self.target = Some((cur + dir).rem_euclid(n) as usize);
    }

    /// Applies one frame's [`Gesture`] against `self.hotspots` as it was left
    /// by the *previous* frame's layout pass. This one-frame lag (test this
    /// frame's input against last frame's rects, then redraw and register
    /// fresh rects for the next one) is the standard trade in an immediate
    /// mode UI: layout is stable from frame to frame, so it is invisible in
    /// practice, and the alternative (laying out before reading input) would
    /// mean every frame does its layout work twice.
    fn handle_gesture(&mut self, g: &Gesture) {
        if let Some(pos) = g.press
            && self.press_card.is_none()
            && self.drag_card.is_none()
            && let Some(&Action::Card(i)) = self.hotspots.hit(pos)
        {
            self.press_card = Some(i);
        }

        if let Some(pos) = g.drag {
            if self.drag_card.is_none() {
                self.drag_card = self.press_card;
            }
            self.drag_pos = Some(pos);
            self.target = match self.hotspots.hit(pos) {
                Some(&Action::Enemy(j)) => Some(j),
                _ => None,
            };
        }

        if let Some(_pos) = g.drop {
            if let Some(i) = self.drag_card.take() {
                self.resolve_play(i, self.target);
            }
            self.press_card = None;
            self.drag_pos = None;
            self.target = None;
        }

        if let Some(pos) = g.tap {
            self.press_card = None;
            match self.hotspots.hit(pos) {
                Some(&Action::Card(i)) => self.tap_card(i),
                Some(&Action::Enemy(j)) => self.tap_enemy(j),
                Some(&Action::EndTurn) => self.end_turn(),
                None => {
                    self.selected = None;
                    self.target = None;
                }
            }
        }
    }

    // ── Layout / drawing ────────────────────────────────────────────────

    /// How tall the bottom hand band (cards plus the control strip) should
    /// be, given the total content height. Capped at the full-tier hand's
    /// natural size, floored at whatever the smallest useful hand needs, and
    /// otherwise a fraction of what's available -- the same "smallest-first,
    /// capped by what remains" budgeting [`panel`] documents, applied to a
    /// two-band split instead of a stack of sidebar panels.
    fn hand_band_height(content_h: u16) -> u16 {
        let max_full = card::FULL_H + 1 + CONTROL_H;
        let min_h = 3 + CONTROL_H;
        let budget = (content_h * 55 / 100).max(min_h.min(content_h));
        budget.min(max_full).min(content_h)
    }

    fn draw_enemies(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 || self.enemies.is_empty() {
            return;
        }
        let cols = panel::columns(area, self.enemies.len() as u16, 1);
        for (i, col) in cols.into_iter().enumerate() {
            let highlighted =
                self.target == Some(i) && (self.drag_card.is_some() || self.selected.is_some());
            self.draw_enemy(surface, col, i, highlighted);
            self.hotspots.push_tappable(col, area, Action::Enemy(i));
        }
    }

    fn draw_enemy(&self, surface: &mut Surface<'_>, rect: Rect, idx: usize, highlighted: bool) {
        let enemy = &self.enemies[idx];
        let bg = if highlighted {
            mix(ui::BG, enemy.kind.accent(), 0.22)
        } else {
            ui::BG
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if highlighted && rect.width() >= 3 && rect.height() >= 3 {
            let style = Style::new().fg(ui::ACCENT).bg(bg);
            for x in rect.left()..rect.right() {
                surface.put((x, rect.top()), '-', style);
                surface.put((x, rect.bottom() - 1), '-', style);
            }
            for y in rect.top()..rect.bottom() {
                surface.put((rect.left(), y), '|', style);
                surface.put((rect.right() - 1, y), '|', style);
            }
        }
        if rect.width() < 4 || rect.height() == 0 {
            return;
        }

        let art_w = 9u16.min(rect.width().saturating_sub(2)).max(4);
        let x0 = rect.left() + (rect.width() - art_w) / 2;
        let dead = !enemy.alive();
        let accent = if dead {
            scale(enemy.kind.accent(), 0.35)
        } else {
            enemy.kind.accent()
        };
        let fg = if dead { ui::DIM } else { ui::FG };
        let mut y = rect.top() + 1;
        let bottom = rect.bottom() - 1;

        // Intent is drawn first, above the portrait, because on a screen this
        // small the eye scans top to bottom -- see the module doc for why
        // telegraphing matters more here than it would in a full desktop
        // client with room for a tooltip.
        if y < bottom {
            let text = if dead {
                "defeated".to_string()
            } else {
                let (icon, num, color) = intent_display(enemy.intent);
                surface.print(
                    (x0, y),
                    &format!("{icon}{num}"),
                    Style::new().fg(color).bg(bg),
                );
                y += 1;
                String::new()
            };
            if dead {
                surface.print(
                    (x0, y),
                    truncate(&text, art_w as usize),
                    Style::new().fg(ui::DIM).bg(bg),
                );
                y += 1;
            }
        }

        // Idle sway: a living creature's art drifts one column back and forth
        // on a slow, per-enemy phase, so a board you are still thinking about
        // never reads as a frozen screenshot. Only the art moves. The name,
        // the intent, and the HP bar stay pinned, because those are read for
        // exact values and a number that will not hold still is harder to
        // read than one that does. A dead enemy stops swaying, which is the
        // cheapest possible way to say so without another glyph.
        //
        // The offset is a two-state step rather than a smooth interpolation
        // because a character grid cannot draw half a column: rounding a
        // continuous sway would produce the same two states anyway, but with
        // a stutter where the value hovers near the rounding boundary.
        let sway = u16::from(!dead && self.time.mul_add(1.1, idx as f32 * 2.1).sin() > 0.0);

        let remaining = bottom.saturating_sub(y);
        if remaining >= 7 {
            surface.print(
                (x0, y),
                truncate(enemy.kind.name(), art_w as usize),
                Style::new().fg(fg).bg(bg),
            );
            y += 1;
            for line in enemy.kind.art() {
                surface.print(
                    (x0 + sway, y),
                    truncate(line, art_w as usize),
                    Style::new().fg(accent).bg(bg),
                );
                y += 1;
            }
            Self::draw_enemy_hp(surface, (x0, y), art_w, enemy, bg);
        } else if remaining >= 2 {
            surface.print(
                (x0, y),
                truncate(enemy.kind.name(), art_w as usize),
                Style::new().fg(fg).bg(bg),
            );
            y += 1;
            Self::draw_enemy_hp(surface, (x0, y), art_w, enemy, bg);
        } else if remaining >= 1 {
            let text = format!("{} {}/{}", enemy.kind.name(), enemy.hp, enemy.max_hp);
            surface.print(
                (x0, y),
                truncate(&text, art_w as usize),
                Style::new().fg(fg).bg(bg),
            );
        }
    }

    fn draw_enemy_hp(
        surface: &mut Surface<'_>,
        at: (u16, u16),
        width: u16,
        enemy: &Enemy,
        bg: Color,
    ) {
        let t = if enemy.max_hp > 0 {
            enemy.hp as f32 / enemy.max_hp as f32
        } else {
            0.0
        };
        let label = format!("{}/{}", enemy.hp, enemy.max_hp);
        let bar_w = width
            .saturating_sub(label.chars().count() as u16 + 1)
            .max(2);
        panel::bar(surface, at, bar_w, t, panel::threshold(t), rgb(40, 20, 20));
        surface.print(
            (at.0 + bar_w + 1, at.1),
            &label,
            Style::new().fg(ui::DIM).bg(bg),
        );
    }

    /// Lays out the hand with [`card::fan`] and draws every card, selected or
    /// held drawn (and re-registered as a hotspot) last -- see the module doc
    /// for why that ordering has to match the draw order exactly.
    fn draw_hand(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if self.hand.is_empty() || area.width() == 0 || area.height() < 3 {
            return;
        }
        let lift: u16 = u16::from(area.height() > card::COMPACT_H);
        let card_h = if area.height() >= card::FULL_H + lift {
            card::FULL_H
        } else if area.height() >= card::COMPACT_H + lift {
            card::COMPACT_H
        } else {
            area.height().saturating_sub(lift).max(3)
        };
        let card_w = if card_h >= card::FULL_H {
            card::FULL_W
        } else if card_h >= card::COMPACT_H {
            card::COMPACT_W
        } else {
            card::FAN_MIN
        };
        let card_area = Rect::new(area.left(), area.bottom() - card_h, area.width(), card_h);
        let base_rects = card::fan(card_area, self.hand.len(), card_w);

        let mut order: Vec<usize> = (0..self.hand.len()).collect();
        let lifted = self.selected.or(self.drag_card);
        if let Some(sel) = lifted {
            order.retain(|&i| i != sel);
            order.push(sel);
        }

        for i in order {
            let Some(&rect) = base_rects.get(i) else {
                continue;
            };
            if rect.width() == 0 {
                continue;
            }
            let is_lifted = lifted == Some(i);
            let rect = if is_lifted && rect.top() >= area.top() + lift {
                Rect::new(rect.left(), rect.top() - lift, rect.width(), rect.height())
            } else {
                rect
            };

            let card_id = self.hand[i];
            let def = &CARD_POOL[card_id];
            let affordable = def.cost <= self.player.energy;
            let state = if !affordable {
                CardState::Disabled
            } else if self.drag_card == Some(i) {
                CardState::Held
            } else if self.selected == Some(i) {
                CardState::Selected
            } else {
                CardState::Idle
            };

            let card = Card::new(def.name)
                .cost(def.cost_str)
                .kind(def.kind)
                .body(def.body)
                .accent(def.accent)
                .state(state);
            card.draw(surface, rect);
            self.hotspots.push(rect, Action::Card(i));
        }
    }

    fn draw_player_status(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 6 || area.height() == 0 {
            return;
        }
        let bg = ui::BG;
        let show_numeral = area.width() >= 10 && area.height() >= 5;
        let text_w = if show_numeral {
            area.width().saturating_sub(6)
        } else {
            area.width()
        };

        let hp_label = format!("HP {}/{}", self.player.hp, self.player.max_hp);
        surface.print(
            (area.left(), area.top()),
            truncate(&hp_label, text_w as usize),
            Style::new().fg(ui::FG).bg(bg),
        );

        if area.height() >= 2 {
            let hp_t = if self.player.max_hp > 0 {
                self.player.hp as f32 / self.player.max_hp as f32
            } else {
                0.0
            };
            panel::bar(
                surface,
                (area.left(), area.top() + 1),
                text_w.max(3),
                hp_t,
                panel::threshold(hp_t),
                rgb(40, 20, 20),
            );
        }
        if area.height() >= 3 {
            let block_label = format!("BLOCK {}", self.player.block);
            surface.print(
                (area.left(), area.top() + 2),
                truncate(&block_label, text_w as usize),
                Style::new().fg(rgb(120, 170, 214)).bg(bg),
            );
        }

        if show_numeral {
            let numeral_x = area.left() + text_w + 1;
            draw_numeral(
                surface,
                (numeral_x, area.top()),
                self.player.energy,
                ui::ACCENT,
                bg,
            );
            if area.height() >= 6 {
                let label = format!("/{}", self.player.max_energy);
                surface.print(
                    (numeral_x, area.top() + 5),
                    &label,
                    Style::new().fg(ui::DIM).bg(bg),
                );
            }
        }
    }

    fn draw_pile_box(surface: &mut Surface<'_>, area: Rect, label: &str, count: usize) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let bg = ui::BG;
        surface.fill_rect(area, ' ', Style::new().bg(bg));
        surface.print(
            (area.left(), area.top()),
            truncate(label, area.width_usize()),
            Style::new().fg(ui::DIM).bg(bg),
        );
        if area.height() >= 2 {
            let text = format!("{count}");
            surface.print(
                (area.left(), area.top() + 1),
                truncate(&text, area.width_usize()),
                Style::new().fg(ui::FG).bg(bg),
            );
        }
    }

    fn draw_end_turn(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let accent = rgb(226, 140, 70);
        let bg = mix(ui::BG, accent, 0.16);
        let inner = panel::Panel::new()
            .border(panel::Border::Double)
            .frame(accent)
            .bg(bg)
            .draw(surface, area);
        if inner.width() >= 4 && inner.height() >= 1 {
            let label = if inner.width() >= 9 {
                "END TURN"
            } else {
                "END"
            };
            let pad = (inner.width() as usize).saturating_sub(label.chars().count()) / 2;
            surface.print(
                (inner.left() + pad as u16, inner.top() + inner.height() / 2),
                label,
                Style::new().fg(ui::FG).bg(bg),
            );
        }
        self.hotspots.push_tappable(area, area, Action::EndTurn);
    }

    fn draw_control_row(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(ui::BG));

        let end_w = END_TURN_W.min(area.width());
        let end_area = Rect::new(area.right() - end_w, area.top(), end_w, area.height());
        self.draw_end_turn(surface, end_area);

        let usable_right = end_area
            .left()
            .saturating_sub(END_TURN_GAP)
            .max(area.left());
        let pile_w = 11u16.min(area.width() / 4);

        let discard_left = usable_right.saturating_sub(pile_w).max(area.left());
        let discard_area = Rect::new(
            discard_left,
            area.top(),
            usable_right - discard_left,
            area.height(),
        );
        Self::draw_pile_box(surface, discard_area, "Discard", self.discard_pile.len());

        let player_w = 18u16.min(area.width() / 3);
        let player_area = Rect::new(area.left(), area.top(), player_w, area.height());
        self.draw_player_status(surface, player_area);

        let draw_left = player_area.right() + 2;
        if draw_left < discard_area.left() {
            let draw_w = pile_w.min(discard_area.left() - draw_left);
            let draw_area = Rect::new(draw_left, area.top(), draw_w, area.height());
            Self::draw_pile_box(surface, draw_area, "Draw", self.draw_pile.len());
        }
    }

    /// Draws whatever room is left between the enemies and the hand as a
    /// scrolling combat log. Skipped entirely below a few rows -- a log too
    /// short to show more than its own border is not worth the frame it
    /// occupies, and a landscape phone or the 80x24 snapshot grid hits that
    /// case routinely.
    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() < 3 {
            return;
        }
        let inner = panel::Panel::new().title("Log").draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    fn draw_drag_arrow(&self, surface: &mut Surface<'_>) {
        let (Some(i), Some(pos)) = (self.drag_card, self.drag_pos) else {
            return;
        };
        let Some(from) = self
            .hotspots
            .rect_where(|a| matches!(a, Action::Card(x) if *x == i))
        else {
            return;
        };
        let start = Pos::new(from.left() + from.width() / 2, from.top());
        draw_pointer_line(surface, start, pos, ui::ACCENT, ui::BG);
    }

    fn status_text(&self) -> String {
        format!(
            "turn {}  hp {}/{}  block {}  energy {}/{}  hand {}  draw {}  discard {}",
            self.turn,
            self.player.hp,
            self.player.max_hp,
            self.player.block,
            self.player.energy,
            self.player.max_energy,
            self.hand.len(),
            self.draw_pile.len(),
            self.discard_pile.len(),
        )
    }
}

impl Demo for SpireDeck {
    const NAME: &'static str = "28_spire_deck";
    const TITLE: &'static str = "28 Spire Deck";
    const BLURB: &'static str =
        "A torch-lit card battle: telegraphed enemies, a fanned hand, tap or drag.";
    const GRID: (u16, u16) = (160, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-9", "select card"),
            ("arrows", "pick target"),
            ("Enter", "play selected"),
            ("E", "end turn"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

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
        let gesture = self.pointer.take();
        self.handle_gesture(&gesture);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.hotspots.clear();

        let hand_h = Self::hand_band_height(content.height());
        let (top_area, hand_band) = panel::split_bottom(content, hand_h);
        let control_h = CONTROL_H.min(hand_band.height());
        let (fan_area, control_area) = panel::split_bottom(hand_band, control_h);

        // Enemies get a fixed-height row; whatever is left above the hand
        // (plentiful on a tall portrait phone, near zero at 80x24) goes to
        // the combat log rather than sitting blank. A phone held upright has
        // rows to spare that a landscape phone or the 80x24 snapshot grid
        // does not, and a log is the one panel that is purely additive: it
        // has something to say at every size down to zero rows.
        let enemy_h = ENEMY_ROW_H.min(top_area.height());
        let (enemy_area, log_area) = panel::split_top(top_area, enemy_h);
        self.draw_enemies(&mut surface, enemy_area);
        self.draw_log(&mut surface, log_area);
        self.draw_hand(&mut surface, fan_area);
        self.draw_control_row(&mut surface, control_area);
        self.draw_drag_arrow(&mut surface);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(SpireDeck);
