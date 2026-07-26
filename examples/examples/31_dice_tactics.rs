//! 31: Dice Tactics -- a party's turn decided by what the dice give it.
//!
//! Slice & Dice's premise, reduced to a character grid: five heroes and a
//! knot of monsters face off, and every hero's action for the round comes
//! from a die that hero rolls. There is no menu of abilities to pick from --
//! there is only what came up, and the tactics are entirely in how the roll
//! gets *assigned* once it has landed.
//!
//! ## Why a die face is the canonical multi-cell argument
//!
//! A six-sided die has exactly six faces and each is a fixed, memorized
//! arrangement of one to six dots. Every literate adult already knows this
//! layout, which makes it the cleanest possible demonstration of why a board
//! entity needs many cells: draw it as one glyph (`3`, `⚂`, whatever) and you
//! have a label that must be *read*; draw it as a 7x5 framed grid of pips at
//! the real arrangement and you have a shape that is *recognized*, the way a
//! real die is recognized face-down on felt from across a table. No other
//! object in this gallery has an audience-wide pre-existing shape vocabulary
//! to exploit -- that is what makes the die the strongest single argument for
//! "one glyph is never one interactive unit" that this whole batch of demos
//! is built to make (see [`ui::touch`]).
//!
//! [`PIP_LAYOUT`] is that vocabulary, transcribed once:
//!
//! ```text
//! 1        2        3        4        5        6
//! . . .    . . o    . . o    o . o    o . o    o . o
//! . o .    . . .    . o .    . . .    . o .    o . o
//! . . .    o . .    o . .    o . o    o . o    o . o
//! ```
//!
//! (`o` marks a pip; the real render uses `\u{2022}`, never `\u{25CF}`, which
//! is outside CP437 and renders as a solid block -- see the module docs on
//! [`ui::touch`] for the whole CP437 constraint.) Each row of three positions
//! is a `(row, col)` pair into a conceptual 3x3 grid, which [`pip_pos`] maps
//! onto whatever interior rect the die is actually drawn into, so the same
//! table produces a correct face whether the die is drawn at the 7x5 floor or
//! larger on a roomy desktop panel.
//!
//! ## Assignment is undoable; resolution is not
//!
//! Tapping a die and then a target is a *plan*, not a commitment: the die
//! visibly marks its target (a small tag appended to that unit's row) so the
//! plan is legible before anything happens, tapping the same die again clears
//! it, and [`DiceTactics::undo`] unwinds the single most recent assignment
//! regardless of which die it belonged to. This mirrors Into the Breach's
//! rule that a *plan* costs nothing to change -- a mis-tap on a five-person
//! roster is not a rare event on a phone, and undoing one has to be at least
//! as cheap as making it.
//!
//! Resolution is the opposite on purpose. [`DiceTactics::resolve`] applies
//! every assignment at once, the monsters answer immediately after, and none
//! of it can be undone: damage taken, healing spent, and rerolls burned are
//! the actual stakes of the round, and a game where every consequence could
//! be undone would have no tactics left in it, only trial and error. The
//! asymmetry -- unlimited undo of the *plan*, zero undo of the *outcome* -- is
//! the same one the brief asks every demo in this batch to draw a hard line
//! on, and it is drawn here at the one button (`RESOLVE`) that the whole
//! layout keeps physically far from every other control.
//!
//! ## The roll is animated from time, not from a per-frame draw
//!
//! [`Die::displayed_face`] is what makes the dice look like they are visibly
//! tumbling: while a die is rolling, the face shown each frame comes from
//! `hash01(seed, die_index, elapsed_ticks)`, where `elapsed_ticks` is the
//! rolling time divided into fixed-width steps. That is a *pure function of
//! accumulated time*, not a mutating RNG draw taken once per rendered frame.
//! The distinction matters for two reasons this gallery cares about
//! specifically: a render-twice-identical test calls `tick` with the same
//! sequence of deltas and must get the same pixels both times, which a
//! per-frame `rng.next_u32()` cannot guarantee once frame timing varies by a
//! single call; and backends do not agree on frame rate, so an animation
//! driven by *ticks* rather than *elapsed seconds* would tumble faster on a
//! backend that renders more often. The die's actual, final result (what
//! resolves) is decided once, at the moment the roll starts, by a real
//! [`Rng`] draw seeded from the round and die index -- only the *display*
//! during the tumble is a time function, and only because a display has no
//! state to protect.
//!
//! Techniques on show:
//!
//! - **Large multi-cell dice with a real pip table** ([`PIP_LAYOUT`],
//!   [`Die::draw`]): the demo's centrepiece, at a floor of 7x5 pips inside a
//!   [`ui::panel::Panel`] frame.
//! - **A legend baked into the die's own frame** ([`Die::badge`]): the
//!   ability a face grants is printed into that die's border every frame, so
//!   nobody has to memorize the mapping to play; a one-line summary is also
//!   always visible at the top of the screen.
//! - **Deterministic time-driven tumble** ([`Die::displayed_face`]): see
//!   above.
//! - **Tap-select-then-tap-target with drag as the alternate path**
//!   ([`DiceTactics::handle_tap`], [`DiceTactics::handle_drop`]), built on
//!   [`ui::touch::Pointer`] and [`ui::touch::Hotspots`] exactly as the brief
//!   for this batch of demos requires.
//! - **Undo for a destructive UI action, no undo for a destructive game
//!   action** ([`DiceTactics::undo`] vs. [`DiceTactics::resolve`]): see above.
//! - **Responsive layout across [`ui::touch::Shape`]**
//!   ([`DiceTactics::layout`]): the party and the monsters trade between a
//!   full three-line roster and a one-line compact roster depending on
//!   available height, exactly as `21_deck_plan.rs`'s crew roster does, and
//!   the dice tray wraps to more rows before it will ever shrink a die below
//!   the 7x5 floor.
//!
//! ```sh
//! cargo run --example 31_dice_tactics --features crossterm
//! cargo run --example 31_dice_tactics --features software
//! cargo run --example 31_dice_tactics --features gl
//! cargo run --example 31_dice_tactics  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{Card, CardState};
use ascii_tile_demos::ui::panel::{self, Border, Log, Panel, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb};

/// How many heroes stand in the party, and how many dice are rolled each
/// round -- one per hero, never a shared pool. Fixed rather than
/// configurable: five is few enough to fit every touch target on a phone
/// screen at once and many enough that the roster reads as a party rather
/// than a duel.
const PARTY_SIZE: usize = 5;

/// How many monsters a wave starts with.
const WAVE_SIZE: usize = 3;

/// Rerolls granted per round. Two, not zero and not unlimited: zero would
/// make a bad opening roll simply unplayable, and unlimited would let a
/// patient player grind every die to a 6, which erases the tension the brief
/// asks this demo to keep front and center.
const REROLLS_PER_ROUND: u32 = 2;

/// World-seconds a freshly rolled die spends visibly tumbling before it can
/// settle. See [`Die::settle_at`] for how this is staggered per die.
const ROLL_TUMBLE: f32 = 0.55;

/// World-seconds between one die settling and the next, applied per die
/// index during [`DiceTactics::roll_round`]. Staggering rather than settling
/// all five at once is what makes the roll read as five individual dice
/// landing rather than one instantaneous flash of numbers -- the
/// "characterful" animation the brief calls for, produced with no extra
/// state beyond an index-scaled offset.
const ROLL_STAGGER: f32 = 0.16;

/// Shorter tumble for a single rerolled die: a full 5-die stagger would make
/// a reroll feel like it costs as much attention as the whole opening roll,
/// when it is meant to read as a quick correction.
const REROLL_TUMBLE: f32 = 0.4;

/// How often the *displayed* face changes while a die is tumbling, in
/// world-seconds. See the module docs for why this is a function of time,
/// not a per-frame draw.
const TUMBLE_STEP: f32 = 0.07;

/// Interior width of a die's pip grid, in cells. The floor the brief asks
/// for: below this a die stops being recognizable as die-shaped and becomes
/// a small grid of unrelated dots.
const DIE_W: u16 = 7;
/// Interior height of a die's pip grid. See [`DIE_W`].
const DIE_H: u16 = 5;
/// Outer footprint of one die, pip grid plus its one-cell frame on every
/// side.
const DIE_OUTER_W: u16 = DIE_W + 2;
const DIE_OUTER_H: u16 = DIE_H + 2;
/// Gap between adjacent dice in the tray.
const DIE_GAP: u16 = 1;

/// The pip vocabulary every six-sided die shares, transcribed once. Each
/// face is a list of `(row, col)` pairs into a 3x3 conceptual grid; see the
/// module docs for the ASCII transcription and [`pip_pos`] for how a pair
/// becomes a screen position. Index `0` is unused (there is no zero-pip
/// face) so `PIP_LAYOUT[face as usize]` can index directly by the rolled
/// value without an off-by-one.
const PIP_LAYOUT: [&[(u8, u8)]; 7] = [
    &[],                                               // 0: unused
    &[(1, 1)],                                         // 1: center
    &[(0, 2), (2, 0)],                                 // 2: diagonal
    &[(0, 2), (1, 1), (2, 0)],                         // 3: diagonal + center
    &[(0, 0), (0, 2), (2, 0), (2, 2)],                 // 4: four corners
    &[(0, 0), (0, 2), (1, 1), (2, 0), (2, 2)],         // 5: corners + center
    &[(0, 0), (0, 2), (1, 0), (1, 2), (2, 0), (2, 2)], // 6: two columns of three
];

/// Maps a `(row, col)` pip position in the conceptual 3x3 grid onto a screen
/// position inside `interior`.
///
/// Uses sixths of the interior's own width/height rather than a fixed
/// three-column stride, so the same table draws correctly whether the die
/// is at the 7x5 floor or stretched larger on a roomy desktop panel: three
/// pips centered at 1/6, 3/6, 5/6 of the span always land symmetrically no
/// matter how wide that span actually is.
fn pip_pos(interior: Rect, row: u8, col: u8) -> (u16, u16) {
    let x = interior.left() + interior.width() * u16::from(2 * col + 1) / 6;
    let y = interior.top() + interior.height() * u16::from(2 * row + 1) / 6;
    (x, y)
}

/// What a rolled face lets its hero do. Named after the four ability
/// families the brief asks for: a blank face that does nothing, a shield
/// that blocks, a sword that hits, and a heart that heals.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ability {
    Blank,
    Shield,
    Sword(i32),
    Heart(i32),
}

impl Ability {
    /// The ability a rolled `face` (1..=6) grants.
    ///
    /// Two swords rather than one keeps the sword the majority ability (as
    /// a "mostly you're hitting things" party game should read) while still
    /// giving the top face a reason to be worth chasing with a reroll: 3 as
    /// the reliable hit, 6 as the crit a reroll is gambling for.
    const fn for_face(face: u8) -> Self {
        match face {
            2 => Self::Shield,
            3 => Self::Sword(3),
            4 => Self::Sword(4),
            5 => Self::Heart(3),
            6 => Self::Sword(6),
            _ => Self::Blank,
        }
    }

    /// Short badge text baked into the die's own frame, so the mapping is
    /// legible without a separate legend -- see the module docs.
    fn badge(self) -> String {
        match self {
            Self::Blank => "--".into(),
            Self::Shield => "BLOCK".into(),
            Self::Sword(dmg) => format!("DMG {dmg}"),
            Self::Heart(heal) => format!("HEAL {heal}"),
        }
    }

    /// Whether this ability can legally be assigned to a monster (an
    /// offensive face) or must go to a hero (support faces). A blank has no
    /// legal target at all.
    const fn targets_monster(self) -> bool {
        matches!(self, Self::Sword(_))
    }

    const fn targets_hero(self) -> bool {
        matches!(self, Self::Shield | Self::Heart(_))
    }

    const fn accent(self) -> Color {
        match self {
            Self::Blank => rgb(90, 90, 100),
            Self::Shield => rgb(120, 170, 226),
            Self::Sword(_) => rgb(220, 108, 96),
            Self::Heart(_) => rgb(140, 210, 140),
        }
    }
}

/// One of the twenty places a die's effect can land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Hero(usize),
    Monster(usize),
}

/// A hero rolls this every round. `face` is the *committed* result, decided
/// once at roll time by a real RNG draw; while `settle_at` has not yet been
/// reached the die is still visibly tumbling and shows a time-derived face
/// instead (see [`Die::displayed_face`] and the module docs).
struct Die {
    face: u8,
    rolling: bool,
    roll_started: f32,
    settle_at: f32,
    assigned: Option<Target>,
    /// Bumped on every reroll of this specific die so its tumble/settle RNG
    /// draw differs from the original roll and from any earlier reroll.
    reroll_count: u32,
}

impl Die {
    const fn ability(&self) -> Ability {
        Ability::for_face(self.face)
    }

    /// The face to actually draw this frame: the committed `face` once
    /// settled, otherwise a value derived purely from elapsed time. See the
    /// module docs for why this must not be a per-frame RNG draw.
    fn displayed_face(&self, index: usize, now: f32) -> u8 {
        if !self.rolling {
            return self.face;
        }
        let elapsed = (now - self.roll_started).max(0.0);
        let step = (elapsed / TUMBLE_STEP) as i32;
        1 + (hash01(0xD1CE_0001, index as i32, step) * 6.0) as u8 % 6
    }

    /// Draws this die into `rect`, returning the interior [`Panel`] frame so
    /// callers can register the exact tappable region. `title` is the
    /// owning hero's short tag (e.g. "H1"); the badge is always the current
    /// ability, so the mapping never has to be memorized.
    fn draw(&self, surface: &mut Surface<'_>, rect: Rect, index: usize, now: f32, selected: bool) {
        let displayed = self.displayed_face(index, now);
        let ability = Ability::for_face(displayed);

        // A gentle brightness pulse independent of every game phase, so the
        // dice tray is never a perfectly static image even while the demo
        // sits idle in the Assign phase waiting on a tap. See the module
        // docs' note on the render-twice-identical / must-animate tests.
        let phase = (index as f32).mul_add(0.9, now * 1.6);
        let pulse = 0.5f32.mul_add(phase.sin(), 0.5);
        let base = ability.accent();
        let accent = if self.rolling {
            mix(base, rgb(255, 255, 255), 0.25)
        } else {
            mix(base, rgb(255, 255, 255), 0.12 * pulse)
        };

        let border = if selected || self.assigned.is_some() {
            Border::Double
        } else {
            Border::Single
        };
        let title = format!("H{}", index + 1);
        let badge = ability.badge();
        let inner = Panel::new()
            .title(&title)
            .badge(&badge)
            .border(border)
            .frame(accent)
            .focused(selected)
            .draw(surface, rect);

        if inner.width() < DIE_W || inner.height() < DIE_H {
            return;
        }
        // Center a DIE_W x DIE_H pip field inside whatever interior the
        // panel actually returned, so a die drawn larger than the floor
        // (a roomy desktop tray) still shows evenly spaced pips rather than
        // pips crammed into one corner.
        let ox = inner.left() + (inner.width() - DIE_W) / 2;
        let oy = inner.top() + (inner.height() - DIE_H) / 2;
        let field = Rect::new(ox, oy, DIE_W, DIE_H);
        let style = Style::new().fg(accent).bg(panel::PANEL_BG);
        for &(row, col) in PIP_LAYOUT[usize::from(displayed.clamp(1, 6))] {
            let (x, y) = pip_pos(field, row, col);
            surface.put((x, y), '\u{2022}', style);
        }

        if let Some(target) = self.assigned {
            let mark = match target {
                Target::Hero(i) => format!("-> H{}", i + 1),
                Target::Monster(i) => format!("-> M{}", i + 1),
            };
            if inner.height() > 0 {
                surface.print(
                    (inner.left(), inner.bottom().saturating_sub(1)),
                    &mark,
                    Style::new().fg(rgb(240, 220, 140)).bg(panel::PANEL_BG),
                );
            }
        }
    }
}

/// A member of the party.
struct Hero {
    name: &'static str,
    class: &'static str,
    color: Color,
    hp: i32,
    max_hp: i32,
    /// Raised for the round by a shield face; halves the next hit taken
    /// during the monster turn that immediately follows resolution, then
    /// clears regardless of whether it was used. A block that persisted
    /// across rounds would let a single shield face blunt every future
    /// attack forever, which would make shields strictly better than heals
    /// and collapse the choice the game is asking the player to make.
    blocking: bool,
}

impl Hero {
    const fn alive(&self) -> bool {
        self.hp > 0
    }
}

/// One monster in the current wave.
struct Monster {
    name: &'static str,
    hp: i32,
    max_hp: i32,
    atk: i32,
}

impl Monster {
    const fn alive(&self) -> bool {
        self.hp > 0
    }
}

/// Which stage of the round the player is in. `Roll` is the tumble; once
/// every die has settled the demo moves itself to `Assign` (no input is
/// needed to reach it, only time), and stays there until `RESOLVE` is
/// pressed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Roll,
    Assign,
}

/// What tapping (or dropping onto) a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Die(usize),
    Hero(usize),
    Monster(usize),
    Reroll,
    Resolve,
    Undo,
}

/// State: the party, the current wave, this round's dice, and the touch/
/// keyboard plumbing every demo in this batch shares.
pub struct DiceTactics {
    heroes: [Hero; PARTY_SIZE],
    monsters: Vec<Monster>,
    dice: Vec<Die>,
    phase: Phase,
    round: u32,
    wave: u32,
    seed: u32,
    rerolls_left: u32,
    selected_die: Option<usize>,
    /// The die index behind an in-progress drag, captured the frame the
    /// press landed on a die so a drop anywhere else can be resolved as an
    /// assignment without re-hit-testing the origin.
    drag_source: Option<usize>,
    /// Die indices assigned this round, in order, so [`Self::undo`] can pop
    /// the most recent one. A `Vec` rather than a single `Option` because a
    /// player may assign several dice before resolving and still expects
    /// "undo" to mean "undo the last thing I did," not "undo everything."
    undo_stack: Vec<usize>,
    /// Keyboard-only cursor over heroes-then-monsters, used by arrow-key
    /// target selection; touch never reads it, since a tap names its target
    /// directly.
    cursor: usize,
    time: f32,
    log: Log,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

const HERO_SEED: [(&str, &str, Color, i32); PARTY_SIZE] = [
    ("Bram", "Warrior", rgb(214, 122, 100), 22),
    ("Sable", "Rogue", rgb(150, 150, 200), 16),
    ("Elowen", "Cleric", rgb(130, 206, 168), 18),
    ("Ashka", "Mage", rgb(206, 160, 100), 14),
    ("Doran", "Guard", rgb(196, 190, 100), 24),
];

const MONSTER_NAMES: [(&str, i32, i32); 4] = [
    ("Grave Rat", 10, 3),
    ("Bog Ooze", 15, 4),
    ("Fang Wolf", 12, 5),
    ("Cinder Imp", 13, 4),
];

impl Default for DiceTactics {
    fn default() -> Self {
        let heroes = core::array::from_fn(|i| {
            let (name, class, color, hp) = HERO_SEED[i];
            Hero {
                name,
                class,
                color,
                hp,
                max_hp: hp,
                blocking: false,
            }
        });
        let dice = (0..PARTY_SIZE)
            .map(|_| Die {
                face: 1,
                rolling: false,
                roll_started: 0.0,
                settle_at: 0.0,
                assigned: None,
                reroll_count: 0,
            })
            .collect();

        let mut log = Log::new(48);
        log.push("The party rolls for its turn.", ui::ACCENT);
        log.push(
            "Tap a die, then tap a target -- or drag a die onto one.",
            ui::DIM,
        );

        let mut game = Self {
            heroes,
            monsters: Vec::new(),
            dice,
            phase: Phase::Roll,
            round: 0,
            wave: 0,
            seed: 0xD1CE,
            rerolls_left: REROLLS_PER_ROUND,
            selected_die: None,
            drag_source: None,
            undo_stack: Vec::new(),
            cursor: 0,
            time: 0.0,
            log,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        };
        game.spawn_wave();
        game.roll_round();
        game
    }
}

impl DiceTactics {
    /// Spawns the next monster wave, deterministically from `self.wave`.
    fn spawn_wave(&mut self) {
        self.wave += 1;
        let mut rng = Rng::new(self.seed ^ self.wave.wrapping_mul(0x2545_F491));
        self.monsters = (0..WAVE_SIZE)
            .map(|i| {
                let (name, hp, atk) = MONSTER_NAMES[(i + self.wave as usize) % MONSTER_NAMES.len()];
                let bonus = self.wave.saturating_sub(1) as i32 * 2;
                let _ = rng.next_u32(); // keep the wave's own RNG stream distinct per monster
                Monster {
                    name,
                    hp: hp + bonus,
                    max_hp: hp + bonus,
                    atk,
                }
            })
            .collect();
        self.log.push(
            format!(
                "Wave {}: {} monsters emerge.",
                self.wave,
                self.monsters.len()
            ),
            rgb(220, 120, 110),
        );
    }

    /// Starts a new round: every die rolls, staggered so they settle one at
    /// a time. The *result* of each die is drawn now, from a real RNG seeded
    /// by round and die index; only the display during the tumble is
    /// time-derived (see the module docs).
    fn roll_round(&mut self) {
        self.round += 1;
        self.rerolls_left = REROLLS_PER_ROUND;
        self.selected_die = None;
        self.undo_stack.clear();
        for h in &mut self.heroes {
            h.blocking = false;
        }
        let mut rng = Rng::new(
            self.seed ^ self.round.wrapping_mul(0x9E37_79B9) ^ self.wave.wrapping_mul(0x1B87_3593),
        );
        for (i, die) in self.dice.iter_mut().enumerate() {
            die.face = 1 + rng.next_below(6) as u8;
            die.rolling = true;
            die.roll_started = self.time;
            die.settle_at = (i as f32).mul_add(ROLL_STAGGER, self.time + ROLL_TUMBLE);
            die.assigned = None;
            die.reroll_count = 0;
        }
        self.phase = Phase::Roll;
        self.log.push(
            format!("Round {} -- dice are rolling.", self.round),
            ui::DIM,
        );
    }

    /// Rerolls a single settled, unassigned die, consuming one of the
    /// round's limited rerolls. This is the game's core tension: a bad face
    /// can be fixed, but not every bad face, and not for free.
    fn reroll(&mut self, idx: usize) {
        if self.phase != Phase::Assign || self.rerolls_left == 0 {
            return;
        }
        let Some(die) = self.dice.get_mut(idx) else {
            return;
        };
        if die.rolling || die.assigned.is_some() {
            return;
        }
        die.reroll_count += 1;
        let mut rng = Rng::new(
            self.seed
                ^ self.round.wrapping_mul(0x9E37_79B9)
                ^ (idx as u32).wrapping_mul(0x2545_F491)
                ^ die.reroll_count.wrapping_mul(0x27D4_EB2F),
        );
        die.face = 1 + rng.next_below(6) as u8;
        die.rolling = true;
        die.roll_started = self.time;
        die.settle_at = self.time + REROLL_TUMBLE;
        self.rerolls_left -= 1;
        self.log.push(
            format!("Die {} rerolled ({} left).", idx + 1, self.rerolls_left),
            ui::DIM,
        );
    }

    /// Attempts to assign `die_idx`'s ability onto `target`, rejecting the
    /// pairing (with a log line, no state change) when the face's ability
    /// cannot legally land there. Shared by the tap-select-then-tap path and
    /// the drag path, which is what keeps the two paths from ever disagreeing
    /// about what counts as a legal assignment.
    fn try_assign(&mut self, die_idx: usize, target: Target) {
        if self.phase != Phase::Assign {
            return;
        }
        let Some(die) = self.dice.get(die_idx) else {
            return;
        };
        if die.rolling {
            return;
        }
        let ability = die.ability();
        let legal = match target {
            Target::Hero(_) => ability.targets_hero(),
            Target::Monster(_) => ability.targets_monster(),
        };
        if !legal {
            let why = if ability == Ability::Blank {
                "That face is blank -- nothing to assign.".to_string()
            } else {
                "That die can't target that.".to_string()
            };
            self.log.push(why, rgb(200, 110, 100));
            return;
        }
        self.dice[die_idx].assigned = Some(target);
        self.undo_stack.push(die_idx);
        self.selected_die = None;
        self.log.push(
            format!("Die {} assigned ({}).", die_idx + 1, ability.badge()),
            ui::FG,
        );
    }

    /// Undoes the most recent assignment, regardless of which die it was.
    /// Unlimited within a round -- see the module docs on why the *plan* is
    /// always cheap to change even though *resolving* it is not.
    fn undo(&mut self) {
        if let Some(idx) = self.undo_stack.pop() {
            if let Some(die) = self.dice.get_mut(idx) {
                die.assigned = None;
            }
            self.log
                .push(format!("Undid Die {}'s assignment.", idx + 1), ui::DIM);
        }
    }

    /// Applies every assignment, then answers with the monster turn. This is
    /// the one irreversible action in the whole loop; see the module docs.
    fn resolve(&mut self) {
        if self.phase != Phase::Assign {
            return;
        }
        for i in 0..self.dice.len() {
            let Some(target) = self.dice[i].assigned else {
                continue;
            };
            let ability = self.dice[i].ability();
            let hero_name = self.heroes[i].name;
            match (ability, target) {
                (Ability::Sword(dmg), Target::Monster(m)) => {
                    if let Some(mon) = self.monsters.get_mut(m)
                        && mon.alive()
                    {
                        mon.hp = (mon.hp - dmg).max(0);
                        let mon_name = mon.name;
                        let died = !mon.alive();
                        self.log.push(
                            format!(
                                "{hero_name} strikes {mon_name} for {dmg}{}",
                                if died { " -- it falls!" } else { "" }
                            ),
                            rgb(220, 108, 96),
                        );
                    }
                }
                (Ability::Shield, Target::Hero(h)) => {
                    if let Some(hero) = self.heroes.get_mut(h) {
                        hero.blocking = true;
                        self.log.push(
                            format!("{hero_name} raises a shield for {}.", hero.name),
                            rgb(120, 170, 226),
                        );
                    }
                }
                (Ability::Heart(heal), Target::Hero(h)) => {
                    if let Some(hero) = self.heroes.get_mut(h) {
                        hero.hp = (hero.hp + heal).min(hero.max_hp);
                        self.log.push(
                            format!("{hero_name} heals {} for {heal}.", hero.name),
                            rgb(140, 210, 140),
                        );
                    }
                }
                _ => {}
            }
        }
        for die in &mut self.dice {
            die.assigned = None;
        }
        self.monster_turn();

        if self.monsters.iter().all(|m| !m.alive()) {
            self.spawn_wave();
        } else if self.heroes.iter().all(|h| !h.alive()) {
            for h in &mut self.heroes {
                h.hp = h.max_hp;
            }
            self.log.push(
                "The party falls, then regroups at full health.".to_string(),
                rgb(220, 120, 110),
            );
        }
        self.roll_round();
    }

    /// Every living monster hits a hero, chosen deterministically from the
    /// round and monster index rather than any live input -- monsters do not
    /// get a choice the player can see or influence, so there is nothing to
    /// gain from making their target selection depend on anything but the
    /// state that already produced this round.
    fn monster_turn(&mut self) {
        let alive_heroes: Vec<usize> = (0..self.heroes.len())
            .filter(|&i| self.heroes[i].alive())
            .collect();
        if alive_heroes.is_empty() {
            return;
        }
        for m in 0..self.monsters.len() {
            if !self.monsters[m].alive() {
                continue;
            }
            let mut rng = Rng::new(
                self.seed
                    ^ self.round.wrapping_mul(0x2545_F491)
                    ^ (m as u32).wrapping_mul(0x27D4_EB2F),
            );
            let pick = alive_heroes[rng.next_below(alive_heroes.len() as u32) as usize];
            let atk = self.monsters[m].atk;
            let mon_name = self.monsters[m].name;
            let hero = &mut self.heroes[pick];
            let dmg = if hero.blocking { (atk / 2).max(1) } else { atk };
            hero.hp = (hero.hp - dmg).max(0);
            let hero_name = hero.name;
            let blocked = hero.blocking;
            let fell = !hero.alive();
            self.log.push(
                format!(
                    "{mon_name} hits {hero_name} for {dmg}{}{}",
                    if blocked { " (blocked)" } else { "" },
                    if fell { " -- down!" } else { "" }
                ),
                rgb(200, 110, 100),
            );
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            self.pointer.feed(&event);
            match event {
                Event::Close => return false,
                Event::Key(key) if key.is_down() => {
                    if ui::is_quit(&event) {
                        return false;
                    }
                    self.handle_key(key.code);
                }
                _ => {}
            }
        }
        true
    }

    /// Keyboard parity for every touch action: number keys pick a die,
    /// arrows step a keyboard cursor across heroes-then-monsters, Enter
    /// assigns the selected die to the cursor's target, R rerolls the
    /// selected die, U undoes, and Space resolves.
    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c @ '1'..='5') => {
                let idx = c as usize - '1' as usize;
                self.select_die(idx);
            }
            KeyCode::Left | KeyCode::Up => self.step_cursor(-1),
            KeyCode::Right | KeyCode::Down => self.step_cursor(1),
            KeyCode::Enter => {
                if let (Some(die), Some(target)) = (self.selected_die, self.cursor_target()) {
                    self.try_assign(die, target);
                }
            }
            KeyCode::Char('r' | 'R') => {
                if let Some(idx) = self.selected_die {
                    self.reroll(idx);
                }
            }
            KeyCode::Char('u' | 'U') => self.undo(),
            KeyCode::Char(' ') => self.resolve(),
            _ => {}
        }
    }

    /// The keyboard cursor lives here rather than as a separate field: it is
    /// simply "one past the last hero" wrapped over heroes-then-monsters, so
    /// [`Self::cursor`] and [`Self::cursor_target`] both derive it from
    /// `selected_die`'s absence being irrelevant -- the cursor persists
    /// independent of selection, stored in `undo_stack`'s sibling field
    /// below.
    const fn step_cursor(&mut self, delta: i32) {
        let total = self.heroes.len() + self.monsters.len();
        if total == 0 {
            return;
        }
        let cur = self.cursor as i32;
        self.cursor = (cur + delta).rem_euclid(total as i32) as usize;
    }

    fn cursor_target(&self) -> Option<Target> {
        if self.cursor < self.heroes.len() {
            Some(Target::Hero(self.cursor))
        } else {
            let m = self.cursor - self.heroes.len();
            (m < self.monsters.len()).then_some(Target::Monster(m))
        }
    }

    fn select_die(&mut self, idx: usize) {
        if self.phase != Phase::Assign || idx >= self.dice.len() {
            return;
        }
        if self.dice[idx].rolling {
            return;
        }
        if self.selected_die == Some(idx) {
            // A second tap/keypress on the already-selected die: if it holds
            // an assignment, clear it (the touch-facing unassign gesture the
            // brief asks for); otherwise just drop the selection.
            if self.dice[idx].assigned.is_some() {
                self.dice[idx].assigned = None;
                self.undo_stack.retain(|&i| i != idx);
                self.log
                    .push(format!("Unassigned Die {}.", idx + 1), ui::DIM);
            }
            self.selected_die = None;
        } else {
            self.selected_die = Some(idx);
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Die(idx) => self.select_die(idx),
            Action::Hero(i) => {
                if let Some(die) = self.selected_die {
                    self.try_assign(die, Target::Hero(i));
                } else {
                    self.cursor = i;
                }
            }
            Action::Monster(i) => {
                if let Some(die) = self.selected_die {
                    self.try_assign(die, Target::Monster(i));
                } else {
                    self.cursor = self.heroes.len() + i;
                }
            }
            Action::Reroll => {
                if let Some(idx) = self.selected_die {
                    self.reroll(idx);
                } else {
                    self.log.push("Select a die before rerolling it.", ui::DIM);
                }
            }
            Action::Resolve => self.resolve(),
            Action::Undo => self.undo(),
        }
    }

    /// Resolves a completed drag: `die_idx` was under the finger at press
    /// time (captured in [`Self::drag_source`]); if the release lands on a
    /// target hotspot, that is the same [`Self::try_assign`] a tap-select
    /// path would have produced -- the two input paths share one outcome
    /// function so they can never disagree about what is legal.
    fn handle_drop(&mut self, die_idx: usize, action: Action) {
        match action {
            Action::Hero(i) => self.try_assign(die_idx, Target::Hero(i)),
            Action::Monster(i) => self.try_assign(die_idx, Target::Monster(i)),
            _ => {}
        }
    }

    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        if self.phase == Phase::Roll {
            let mut all_settled = true;
            for die in &mut self.dice {
                if die.rolling {
                    if self.time >= die.settle_at {
                        die.rolling = false;
                    } else {
                        all_settled = false;
                    }
                }
            }
            if all_settled {
                self.phase = Phase::Assign;
                self.log
                    .push("All dice settled -- assign them, then RESOLVE.", ui::ACCENT);
            }
        } else {
            for die in &mut self.dice {
                if die.rolling && self.time >= die.settle_at {
                    die.rolling = false;
                }
            }
        }
    }

    // ---- layout & drawing ----------------------------------------------

    fn draw_legend(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < 8 {
            return;
        }
        let phase = match self.phase {
            Phase::Roll => "ROLLING",
            Phase::Assign => "ASSIGN",
        };
        let round_text = format!("Round {} ", self.round);
        let phase_text = format!("[{phase}] ");
        let rerolls = format!("rerolls {}", self.rerolls_left);
        let spans = [
            Span::new(&round_text, ui::ACCENT),
            Span::dim(&phase_text),
            Span::plain("1"),
            Span::dim(":-- "),
            Span::new("2", rgb(120, 170, 226)),
            Span::dim(":block "),
            Span::new("3/4/6", rgb(220, 108, 96)),
            Span::dim(":sword "),
            Span::new("5", rgb(140, 210, 140)),
            Span::dim(":heal "),
            Span::plain(&rerolls),
        ];
        panel::spans(
            surface,
            (area.left(), area.top()),
            area.width(),
            &spans,
            ui::CHROME_BG,
        );
    }
}

impl Demo for DiceTactics {
    const NAME: &'static str = "31_dice_tactics";
    const TITLE: &'static str = "31 Dice Tactics";
    const BLURB: &'static str =
        "A party's turn decided by a roll: big multi-cell dice, tap to assign.";
    const GRID: (u16, u16) = (168, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-5", "pick die"),
            ("arrows", "pick target"),
            ("Enter", "assign"),
            ("R", "reroll"),
            ("U", "undo"),
            ("Space", "resolve"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.hotspots.clear();
        let shape = Shape::of(content);
        self.layout_and_draw(&mut surface, content, shape);

        ui::title_bar::<Self>(&mut surface, title);
        let text = format!(
            "wave {} round {} rerolls {}",
            self.wave, self.round, self.rerolls_left
        );
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);

        // Resolve input against this frame's freshly rebuilt hotspots. Taps
        // and drags are handled after layout on purpose: a hotspot that was
        // not drawn this frame cannot be hit this frame, which is the
        // property immediate-mode input needs to stay honest with what is
        // actually on screen (see `ui::touch`'s module docs).
        let gesture: Gesture = self.pointer.take();
        if let Some(p) = gesture.press
            && self.drag_source.is_none()
            && let Some(Action::Die(idx)) = self.hotspots.hit(p)
        {
            self.drag_source = Some(*idx);
        }
        if gesture.press.is_none() {
            self.drag_source = None;
        }
        if let Some(tap) = gesture.tap
            && let Some(action) = self.hotspots.hit(tap).copied()
        {
            self.handle_action(action);
        }
        if let Some(drop) = gesture.drop
            && let Some(die_idx) = self.drag_source.take()
            && let Some(action) = self.hotspots.hit(drop).copied()
        {
            self.handle_drop(die_idx, action);
        }

        true
    }
}

impl DiceTactics {
    /// One pass: build every rect for the current [`Shape`], draw into it,
    /// and register its hotspot, all together rather than split across a
    /// separate layout pass and a separate hit-testing pass. Keeping them in
    /// one function is what guarantees the drawn rect and the tappable rect
    /// are the same rect -- the two going out of sync is the classic bug in
    /// any UI that computes layout twice.
    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect, shape: Shape) {
        let (legend_area, rest) = panel::split_top(content, 1);
        self.draw_legend(surface, legend_area);

        // Buttons always claim the thumb-zone bottom strip first, ahead of
        // everything else: see the brief's "primary actions live at the
        // bottom" rule. Two TAP_H-tall rows so RESOLVE and REROLL/UNDO never
        // compete for the same band a thumb is already resting near.
        let (rest, button_area) = panel::split_bottom(rest, TAP_H);

        if shape.stacks() {
            // Portrait: heroes, monsters, dice, each a compact horizontal
            // band, stacked so the whole board reads top to bottom the way a
            // scroll of paper would.
            let party_h = (rest.height() / 4).max(6);
            let (party_area, rest) = panel::split_top(rest, party_h);
            let (monster_area, dice_area) = panel::split_top(rest, party_h);
            self.draw_party(surface, party_area);
            self.draw_monsters(surface, monster_area);
            self.draw_dice(surface, dice_area);
        } else {
            // Landscape / desktop: heroes and monsters flank the dice tray,
            // the layout the brief describes as "one side / other side /
            // centrepiece."
            let side_w = if rest.width() >= 140 { 26 } else { 16 };
            let (party_area, rest2) = panel::split_left(rest, side_w);
            let (dice_area, monster_area) = panel::split_right(rest2, side_w);
            self.draw_party(surface, party_area);
            self.draw_monsters(surface, monster_area);
            self.draw_dice(surface, dice_area);
        }

        self.draw_buttons(surface, button_area);
    }

    fn draw_party(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Party")
            .badge(&format!("{}", self.heroes.len()))
            .draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }
        let n = self.heroes.len();
        let full_h = n as u16 * 3;
        let full = inner.height() >= full_h;
        for i in 0..n {
            let h = &self.heroes[i];
            let note = self.hero_assignment_note(i);
            if full {
                let y0 = inner.top() + i as u16 * 3;
                if y0 + 2 >= inner.bottom() {
                    break;
                }
                let row = Rect::new(inner.left(), y0, inner.width(), 3);
                self.hotspots.push_tappable(row, area, Action::Hero(i));
                let mut spans = vec![Span::new(h.name, h.color)];
                if !h.alive() {
                    spans.push(Span::dim(" (down)"));
                } else if h.blocking {
                    spans.push(Span::new(" [shield]", rgb(120, 170, 226)));
                }
                if let Some(n) = &note {
                    spans.push(Span::plain(" "));
                    spans.push(Span::new(n, rgb(240, 220, 140)));
                }
                panel::spans(
                    surface,
                    (inner.left(), y0),
                    inner.width(),
                    &spans,
                    panel::PANEL_BG,
                );
                let t = h.hp as f32 / h.max_hp as f32;
                panel::bar(
                    surface,
                    (inner.left(), y0 + 1),
                    inner.width(),
                    t,
                    panel::threshold(t),
                    rgb(30, 30, 36),
                );
                surface.print(
                    (inner.left(), y0 + 2),
                    &format!("{} {}/{}", h.class, h.hp, h.max_hp),
                    Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
                );
            } else {
                if i as u16 >= inner.height() {
                    break;
                }
                let y = inner.top() + i as u16;
                let row = Rect::new(inner.left(), y, inner.width(), 1);
                self.hotspots.push_tappable(row, area, Action::Hero(i));
                let tag: String = h.name.chars().take(4).collect();
                let bar_w = inner.width().saturating_sub(6).max(3);
                surface.print(
                    (inner.left(), y),
                    &tag,
                    Style::new().fg(h.color).bg(panel::PANEL_BG),
                );
                let t = h.hp as f32 / h.max_hp as f32;
                panel::bar(
                    surface,
                    (inner.left() + 5, y),
                    bar_w,
                    t,
                    panel::threshold(t),
                    rgb(30, 30, 36),
                );
            }
        }
    }

    fn draw_monsters(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Monsters")
            .badge(&format!("{}", self.monsters.len()))
            .draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }
        let n = self.monsters.len();
        let full_h = n as u16 * 3;
        let full = inner.height() >= full_h;
        for i in 0..n {
            let m = &self.monsters[i];
            let note = self.monster_assignment_note(i);
            let color = if m.alive() {
                rgb(216, 130, 120)
            } else {
                ui::DIM
            };
            if full {
                let y0 = inner.top() + i as u16 * 3;
                if y0 + 2 >= inner.bottom() {
                    break;
                }
                let row = Rect::new(inner.left(), y0, inner.width(), 3);
                self.hotspots.push_tappable(row, area, Action::Monster(i));
                let mut spans = vec![Span::new(m.name, color)];
                if !m.alive() {
                    spans.push(Span::dim(" (dead)"));
                }
                if let Some(n) = &note {
                    spans.push(Span::plain(" "));
                    spans.push(Span::new(n, rgb(240, 220, 140)));
                }
                panel::spans(
                    surface,
                    (inner.left(), y0),
                    inner.width(),
                    &spans,
                    panel::PANEL_BG,
                );
                let t = if m.max_hp > 0 {
                    m.hp as f32 / m.max_hp as f32
                } else {
                    0.0
                };
                panel::bar(
                    surface,
                    (inner.left(), y0 + 1),
                    inner.width(),
                    t,
                    panel::threshold(t),
                    rgb(30, 30, 36),
                );
                surface.print(
                    (inner.left(), y0 + 2),
                    &format!("atk {} hp {}/{}", m.atk, m.hp, m.max_hp),
                    Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
                );
            } else {
                if i as u16 >= inner.height() {
                    break;
                }
                let y = inner.top() + i as u16;
                let row = Rect::new(inner.left(), y, inner.width(), 1);
                self.hotspots.push_tappable(row, area, Action::Monster(i));
                let tag: String = m.name.chars().take(4).collect();
                let bar_w = inner.width().saturating_sub(6).max(3);
                surface.print(
                    (inner.left(), y),
                    &tag,
                    Style::new().fg(color).bg(panel::PANEL_BG),
                );
                let t = if m.max_hp > 0 {
                    m.hp as f32 / m.max_hp as f32
                } else {
                    0.0
                };
                panel::bar(
                    surface,
                    (inner.left() + 5, y),
                    bar_w,
                    t,
                    panel::threshold(t),
                    rgb(30, 30, 36),
                );
            }
        }
    }

    fn hero_assignment_note(&self, hero: usize) -> Option<String> {
        self.dice.iter().enumerate().find_map(|(i, d)| {
            (d.assigned == Some(Target::Hero(hero))).then(|| format!("<- D{}", i + 1))
        })
    }

    fn monster_assignment_note(&self, monster: usize) -> Option<String> {
        self.dice.iter().enumerate().find_map(|(i, d)| {
            (d.assigned == Some(Target::Monster(monster))).then(|| format!("<- D{}", i + 1))
        })
    }

    /// Lays out the dice tray, wrapping to more rows before ever shrinking a
    /// die below the [`DIE_OUTER_W`]x[`DIE_OUTER_H`] floor -- the one rule
    /// in this whole layout that is not allowed to bend, since a smaller die
    /// is exactly the failure mode this demo exists to argue against.
    fn draw_dice(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < DIE_OUTER_W || area.height() < DIE_OUTER_H {
            // Not enough room for even one die at the floor size: say so
            // rather than drawing a broken die, which is more honest about
            // what a badly squeezed viewport can and cannot show.
            panel::band(surface, area);
            if area.width() > 4 && area.height() > 0 {
                surface.print(
                    (area.left(), area.top()),
                    "(widen for dice)",
                    Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
                );
            }
            return;
        }
        let cols = (area.width() / (DIE_OUTER_W + DIE_GAP))
            .max(1)
            .min(self.dice.len() as u16);
        let n = self.dice.len();
        let now = self.time;
        for i in 0..n {
            let col = i as u16 % cols;
            let row = i as u16 / cols;
            let x = area.left() + col * (DIE_OUTER_W + DIE_GAP);
            let y = area.top() + row * (DIE_OUTER_H + DIE_GAP);
            if x + DIE_OUTER_W > area.right() || y + DIE_OUTER_H > area.bottom() {
                continue;
            }
            let rect = Rect::new(x, y, DIE_OUTER_W, DIE_OUTER_H);
            self.hotspots.push_tappable(rect, area, Action::Die(i));
            let selected = self.selected_die == Some(i);
            self.dice[i].draw(surface, rect, i, now, selected);
        }
    }

    fn draw_buttons(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < TAP_W * 2 + 2 || area.height() < TAP_H {
            return;
        }
        // REROLL and UNDO share the left half, RESOLVE owns the right half
        // alone and wider than it needs to be -- the brief's "well separated"
        // requirement means physical distance, not just a different color, so
        // a thumb reaching for RESOLVE cannot also clip REROLL.
        let cols = panel::columns(area, 3, 1);
        let (reroll_rect, undo_rect, resolve_rect) = (cols[0], cols[1], cols[2]);

        Self::draw_button(
            surface,
            reroll_rect,
            "REROLL",
            &format!("{} left", self.rerolls_left),
            self.rerolls_left > 0 && self.selected_die.is_some(),
        );
        self.hotspots
            .push_tappable(reroll_rect, area, Action::Reroll);

        Self::draw_button(
            surface,
            undo_rect,
            "UNDO",
            &format!("{} pending", self.undo_stack.len()),
            !self.undo_stack.is_empty(),
        );
        self.hotspots.push_tappable(undo_rect, area, Action::Undo);

        Self::draw_button(
            surface,
            resolve_rect,
            "RESOLVE",
            "locks in the round",
            self.phase == Phase::Assign,
        );
        self.hotspots
            .push_tappable(resolve_rect, area, Action::Resolve);
    }

    fn draw_button(surface: &mut Surface<'_>, rect: Rect, label: &str, sub: &str, enabled: bool) {
        let state = if enabled {
            CardState::Idle
        } else {
            CardState::Disabled
        };
        let accent = if enabled { ui::ACCENT } else { ui::DIM };
        Card::new(label)
            .body(sub)
            .accent(accent)
            .state(state)
            .draw(surface, rect);
    }
}

ascii_tile_demos::demo_main!(DiceTactics);
