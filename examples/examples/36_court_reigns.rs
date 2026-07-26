//! 36: Court of Reigns -- a medieval court where every petition is answered
//! by a swipe, and both extremes of every power are fatal.
//!
//! This is the gallery's most mobile-native demo. Unlike the board- and
//! deck-shaped demos elsewhere, it has exactly one thing to do: an advisor
//! brings a petition, and the monarch answers yes or no. That is a single
//! horizontal decision, which is precisely what a swipe gesture is for, so
//! the drag handling here is not a convenience alongside some richer
//! interface -- it *is* the interface. The two tap buttons and the arrow
//! keys exist as parity paths, not as the primary design.
//!
//! Techniques on show:
//!
//! - **Drag-to-preview, release-to-commit** ([`CourtReigns::handle_events`],
//!   [`CourtReigns::draw_card`]): dragging the card leans it toward the
//!   answer and reveals what that answer would do to the four meters, but
//!   nothing happens until release. A tap-to-commit interface (answer the
//!   instant a finger lands past some line) is one accidental flick away from
//!   a decision the player did not mean to make; a released-and-snaps-back
//!   interface costs the player nothing to explore. [`COMMIT_THRESHOLD`] is
//!   read at *release*, not during the drag, so wandering past the line and
//!   back is free right up until the finger lifts -- the same rule
//!   [`ui::touch::Pointer`] itself uses to tell a tap from a drag: a gesture
//!   is not final until it ends.
//! - **The preview replaces hover** ([`CourtReigns::draw_meters`]): a mouse
//!   UI would show the consequence of a choice on hover, before commitment.
//!   Touch has no hover ([`Gesture::hover`](ui::touch::Gesture::hover) is
//!   `None` on every touch backend), so the only way to show a consequence
//!   before it is chosen is to tie it to the gesture that *is* available: the
//!   drag itself. The ghosted bar segment and the delta arrow are what a
//!   tooltip would have said, spoken through the same motion that will
//!   commit the choice, at an intensity ([`preview_alpha`]) that scales with
//!   how far the finger has travelled toward the commit line.
//! - **Both extremes are fatal** ([`CourtReigns::commit`]): Church, Army,
//!   People, and Coin end the reign at zero *or* at full. This is Reigns'
//!   cleverest rule and it is reproduced deliberately: a meter that only
//!   fails at zero turns every decision into "avoid the low score," which
//!   collapses to always picking whichever answer raises the threatened
//!   meter. A meter that fails at both ends means every answer that helps
//!   one problem creates the opposite one somewhere else, so there is no
//!   answer that is simply correct -- only trade-offs, which is what makes a
//!   petition worth pausing over instead of pattern-matching.
//! - **Sheared, not scaled, lean** ([`row_shift`], [`CourtReigns::draw_card`]):
//!   the card does not rotate (there is no sub-cell rotation in a character
//!   grid) and it does not scale (text does not resample). Instead each row
//!   is translated by an amount that grows toward the top, which is the
//!   cheapest transform a character grid can do that still reads as a card
//!   tilting on a pivot near its base.
//! - **Tier-dropping across [`ui::touch::Shape`]** ([`CourtReigns::layout`],
//!   [`CourtReigns::draw_card`]): the card's *width* is capped
//!   ([`CARD_MAX_W`]) independently of how wide the viewport is, because a
//!   line of prose does not get more readable by getting longer -- past
//!   about fifty columns the eye starts losing its place tracking back to
//!   the start of the next line. Growing into a big viewport instead adds
//!   margin around a card that stays print-width. The card's *height* is
//!   spent in a fixed order when it is scarce (portrait art first, then the
//!   divider, then wrapped petition lines), the same principle
//!   [`ui::card`] documents for its own tiers: the thing a decision actually
//!   needs (the text) is the last thing this demo will cut.

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use std::collections::VecDeque;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::wrap;
use ascii_tile_demos::ui::panel::{self, Log};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, DIM, FG};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb};

/// Cap on the card's own width, independent of viewport width.
///
/// A desktop window can be twice as wide as a phone, but a petition read
/// comfortably at forty-odd columns does not become *more* readable at
/// eighty; it just gets a longer line for the eye to track back across. This
/// is the difference between a demo that fills its viewport and one that
/// scales its content: past this cap, extra width becomes margin around the
/// card, not more card.
const CARD_MAX_W: u16 = 78;

/// Wrap width for the petition's body text, applied inside the card's own
/// interior (which may be narrower than this on a small phone). Kept
/// distinct from the card's width so the two can be tuned independently: the
/// card is sized for its portrait and frame, the text column for legibility.
const TEXT_MAX_W: u16 = 44;

/// Cap on the card's own height, matching [`CARD_MAX_W`]'s reasoning: past
/// this, extra vertical room becomes margin above and below a card that
/// keeps a believable proportion, rather than a card that grows into a tall
/// phone's viewport and leaves most of its own interior blank.
const CARD_MAX_H: u16 = 26;

/// How far, as a fraction of half the card's width, a released drag has to
/// have travelled for its answer to commit rather than snap back.
///
/// Read at the moment of release, never while the finger is still down --
/// see the module docs on why release-based commit is the safe version of
/// this gesture. Half rather than some more timid fraction because Reigns'
/// own convention (and Tinder's) is roughly a third to a half of the card's
/// width; shorter than that and an ordinary reposition of the thumb starts
/// accidentally committing answers.
const COMMIT_THRESHOLD: f32 = 0.5;

/// Portrait height in rows, shared by every advisor so the card's tier
/// arithmetic only needs one number.
const PORTRAIT_H: u16 = 7;

/// How long the "reign ends" epitaph holds the screen before a new monarch
/// is enthroned and play continues. Long enough to be read, short enough
/// that the loop -- the whole point of the succession mechanic -- keeps
/// moving.
const DEATH_DURATION: f32 = 2.6;

/// Index order shared by the meter array, the petition deltas, and every
/// color/name lookup table below. A named `usize` per meter rather than an
/// enum because the deltas live in a plain `[f32; 4]` (so they can sit in a
/// `const` petition table); the constants are what keep the indices from
/// being magic numbers at every call site.
const METER_CHURCH: usize = 0;
const METER_ARMY: usize = 1;
const METER_PEOPLE: usize = 2;
const METER_COIN: usize = 3;
const METER_COUNT: usize = 4;

const METER_NAMES: [&str; METER_COUNT] = ["Church", "Army", "People", "Coin"];

const fn meter_color(i: usize) -> Color {
    match i {
        METER_CHURCH => rgb(176, 140, 224),
        METER_ARMY => rgb(198, 92, 88),
        METER_PEOPLE => rgb(140, 178, 108),
        _ => rgb(226, 190, 92),
    }
}

/// Which side of the card an answer sits on. Doubles as the swipe direction,
/// the hotspot action for the two tap buttons, and the lean's sign, so a
/// keyboard press, a button tap, and a completed swipe all funnel through
/// exactly one path: [`CourtReigns::commit`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
}

impl Side {
    const fn sign(self) -> f32 {
        match self {
            Self::Left => -1.0,
            Self::Right => 1.0,
        }
    }
}

/// A visiting advisor's role: fixed art, a fixed name, and a fixed accent, so
/// the court reads as a recurring cast rather than a shuffled name generator.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Advisor {
    Bishop,
    General,
    Merchant,
    Peasant,
}

impl Advisor {
    const fn name(self) -> &'static str {
        match self {
            Self::Bishop => "Bishop Aldric",
            Self::General => "General Rurik",
            Self::Merchant => "Guildmaster Yvana",
            Self::Peasant => "Toma the Farrier",
        }
    }

    const fn role(self) -> &'static str {
        match self {
            Self::Bishop => "BISHOP",
            Self::General => "GENERAL",
            Self::Merchant => "MERCHANT",
            Self::Peasant => "PEASANT",
        }
    }

    const fn accent(self) -> Color {
        match self {
            Self::Bishop => rgb(196, 168, 232),
            Self::General => rgb(206, 118, 96),
            Self::Merchant => rgb(224, 196, 118),
            Self::Peasant => rgb(160, 182, 132),
        }
    }

    /// Multi-cell portrait art. Every advisor gets a body distinct enough to
    /// silhouette-recognize at a glance, because a phone player is reading
    /// this at speed between swipes, not studying it: the mitre, the plume,
    /// the coin sacks, and the plain hood are chosen to differ in outline
    /// first and detail second.
    const fn portrait(self) -> &'static [&'static str] {
        match self {
            Self::Bishop => &[
                "   .+.   ",
                "  /   \\  ",
                " |==+==| ",
                " |  o  | ",
                " |#####| ",
                " |# + #| ",
                " \\#####/ ",
            ],
            Self::General => &[
                "   _/\\_   ",
                "  [=====]  ",
                "  |[o-o]|  ",
                "  |==V==|  ",
                " /#######\\ ",
                " |# --- #| ",
                " \\#######/ ",
            ],
            Self::Merchant => &[
                "   .---.   ",
                "  ( o.o )  ",
                "   \\_-_/   ",
                "  /$$$$$\\  ",
                " |#/---\\#| ",
                " |#|$$$|#| ",
                "  \\#####/  ",
            ],
            Self::Peasant => &[
                "    ___    ",
                "   / . \\   ",
                "  ( -.- )  ",
                "   \\___/   ",
                "  /|###|\\  ",
                " | |###| | ",
                "  \\|___|/  ",
            ],
        }
    }
}

/// One side of a petition: what the monarch says, and what it costs.
struct Answer {
    label: &'static str,
    /// Deltas in meter order (see [`meter`]), applied to a meter already
    /// scaled `0.0..=1.0`. Kept small (rarely past 0.2) because the reign is
    /// meant to survive several dozen petitions, not two.
    delta: [f32; METER_COUNT],
}

/// A petition: who brought it, what they are asking, and the two ways to
/// answer.
struct Petition {
    advisor: Advisor,
    text: &'static str,
    left: Answer,
    right: Answer,
}

/// The deck. At least a dozen petitions with real trade-offs on both sides --
/// see the module docs on why fatal-at-both-extremes forces every answer to
/// help one meter at some other meter's expense.
const PETITIONS: &[Petition] = &[
    Petition {
        advisor: Advisor::Bishop,
        text: "Grant me gold to gild the chapel roof, that pilgrims may see it shine from the old road.",
        left: Answer {
            label: "Deny the gilding",
            delta: [-0.10, 0.0, 0.0, 0.05],
        },
        right: Answer {
            label: "Grant the gold",
            delta: [0.13, 0.0, 0.0, -0.16],
        },
    },
    Petition {
        advisor: Advisor::General,
        text: "The garrison wants double rations before the siege season begins. They will not march hungry.",
        left: Answer {
            label: "Refuse",
            delta: [0.0, -0.14, 0.03, 0.06],
        },
        right: Answer {
            label: "Approve",
            delta: [0.0, 0.11, -0.03, -0.12],
        },
    },
    Petition {
        advisor: Advisor::Merchant,
        text: "Lift the toll on the river road, your grace. Every cart that turns back is coin we both lose.",
        left: Answer {
            label: "Keep the toll",
            delta: [0.0, 0.0, -0.09, 0.10],
        },
        right: Answer {
            label: "Lift it",
            delta: [0.0, 0.0, 0.12, -0.08],
        },
    },
    Petition {
        advisor: Advisor::Peasant,
        text: "The harvest failed in three villages. Open the royal granary before the frost takes the rest of us.",
        left: Answer {
            label: "Let them go hungry",
            delta: [0.0, 0.0, -0.18, 0.05],
        },
        right: Answer {
            label: "Open the granary",
            delta: [0.0, 0.0, 0.15, -0.13],
        },
    },
    Petition {
        advisor: Advisor::Bishop,
        text: "A heretic preaches against the throne in the square. The faithful want him burned.",
        left: Answer {
            label: "Spare him",
            delta: [-0.12, 0.0, 0.06, 0.0],
        },
        right: Answer {
            label: "Burn him",
            delta: [0.15, 0.0, -0.10, 0.0],
        },
    },
    Petition {
        advisor: Advisor::General,
        text: "Send the knights on a raid across the border while the enemy lord is weak.",
        left: Answer {
            label: "Hold the line",
            delta: [0.0, -0.06, 0.0, 0.0],
        },
        right: Answer {
            label: "Raid",
            delta: [-0.05, 0.12, 0.0, 0.06],
        },
    },
    Petition {
        advisor: Advisor::Merchant,
        text: "A new trading guild wants your royal seal on their charter. They offer a generous fee for it.",
        left: Answer {
            label: "Refuse the seal",
            delta: [0.0, 0.0, 0.04, -0.03],
        },
        right: Answer {
            label: "Sell the seal",
            delta: [-0.05, 0.0, 0.0, 0.16],
        },
    },
    Petition {
        advisor: Advisor::Peasant,
        text: "My daughter was taken by the tax collector's men for a debt we do not owe, my liege.",
        left: Answer {
            label: "Ignore it",
            delta: [0.0, 0.0, -0.16, 0.0],
        },
        right: Answer {
            label: "Punish the collector",
            delta: [0.0, 0.0, 0.12, -0.08],
        },
    },
    Petition {
        advisor: Advisor::Bishop,
        text: "Fund a crusade to reclaim the shrine at the eastern pass. The Church will remember it.",
        left: Answer {
            label: "Stay home",
            delta: [-0.11, 0.03, 0.0, 0.0],
        },
        right: Answer {
            label: "Muster the crusade",
            delta: [0.17, -0.11, 0.0, -0.14],
        },
    },
    Petition {
        advisor: Advisor::General,
        text: "The old marshal grumbles of usurpers over his cups. Shall I have him executed?",
        left: Answer {
            label: "Let him live",
            delta: [0.0, -0.11, 0.0, 0.0],
        },
        right: Answer {
            label: "Execute him",
            delta: [0.0, 0.11, -0.08, 0.0],
        },
    },
    Petition {
        advisor: Advisor::Merchant,
        text: "Debase the coinage, sire. It is the fastest way to pay down the court's debts before winter.",
        left: Answer {
            label: "Keep coin honest",
            delta: [0.0, 0.0, 0.0, -0.09],
        },
        right: Answer {
            label: "Debase it",
            delta: [0.0, 0.0, -0.11, 0.18],
        },
    },
    Petition {
        advisor: Advisor::Peasant,
        text: "Forgive our debts this winter, my liege, and the villages will sing your name for a generation.",
        left: Answer {
            label: "Collect in full",
            delta: [0.0, 0.0, -0.12, 0.10],
        },
        right: Answer {
            label: "Forgive the debts",
            delta: [0.0, 0.0, 0.15, -0.16],
        },
    },
    Petition {
        advisor: Advisor::Bishop,
        text: "The cathedral bells ring for a royal tithe on the coming wedding season. Will you levy it?",
        left: Answer {
            label: "No tithe",
            delta: [-0.08, 0.0, 0.05, 0.0],
        },
        right: Answer {
            label: "Levy it",
            delta: [0.10, 0.0, -0.08, 0.05],
        },
    },
    Petition {
        advisor: Advisor::General,
        text: "Volunteers alone will not fill the ranks before spring. Shall I conscript the villages?",
        left: Answer {
            label: "Volunteers only",
            delta: [0.0, -0.08, 0.05, 0.0],
        },
        right: Answer {
            label: "Conscript them",
            delta: [0.0, 0.16, -0.15, 0.0],
        },
    },
];

/// Which meter ended a reign, and in which direction, for the epitaph text.
/// Both directions of every meter are represented -- see the module docs.
const fn epitaph(meter_i: usize, maxed: bool) -> &'static str {
    match (meter_i, maxed) {
        (METER_CHURCH, false) => "The clergy names you heretic, and the throne falls with you.",
        (METER_CHURCH, true) => {
            "The Church's grip closes over the crown; a theocracy needs no monarch."
        }
        (METER_ARMY, false) => {
            "The garrison deserts in the night. Rebels take the gate by morning."
        }
        (METER_ARMY, true) => {
            "The generals tire of a monarch who only asks. They stop asking back."
        }
        (METER_PEOPLE, false) => "A starving mob drags you from the palace steps.",
        (METER_PEOPLE, true) => {
            "Your name becomes a cult the nobles cannot compete with, so they end it."
        }
        (METER_COIN, false) => {
            "The treasury is bare. The guards go unpaid, and so do their loyalties."
        }
        _ => "Hoarded gold breeds jealous heirs. One finds a blade before the others.",
    }
}

/// Layout computed once per frame from the live viewport. Pure function of
/// `(content, shape)`, kept separate from [`CourtReigns`] itself so it can be
/// built before events are read and reused for both hit-testing and drawing
/// without borrowing `self` twice.
struct Layout {
    card: Rect,
    meters: Rect,
    meters_compact: bool,
    left_btn: Rect,
    right_btn: Rect,
}

impl Layout {
    /// Lays out the card, the meter block, and the two answer buttons for
    /// `content`, degrading in a fixed order as `shape` gets tighter.
    ///
    /// The buttons claim their [`touch::TAP_H`] band first, and the meters
    /// claim next, both before the card sees any space -- the same
    /// smallest-first budgeting `21_deck_plan`'s sidebar uses, because the
    /// card is the one element allowed to *use* leftover space rather than
    /// need a guaranteed minimum: it degrades gracefully (see
    /// [`CourtReigns::draw_card`]) in a way a clipped button cannot.
    fn build(content: Rect, _shape: Shape) -> Self {
        if content.width() < 8 || content.height() < 8 {
            let empty = Rect::new(content.left(), content.top(), 0, 0);
            return Self {
                card: content,
                meters: empty,
                meters_compact: true,
                left_btn: empty,
                right_btn: empty,
            };
        }

        let button_band_h = (touch::TAP_H + 1).min(content.height());
        let (rest, button_band) = panel::split_bottom(content, button_band_h);

        // Two rows per meter (a label line, then its own bar) reads far
        // better than one, but a landscape phone cannot always spare nine
        // rows for it. Rather than shrinking the bars themselves (which
        // would just make the fill harder to read), drop to one row per
        // meter -- label and bar sharing a line -- the same
        // full-vs-compact trade `21_deck_plan`'s roster makes.
        let full_h = 1 + METER_COUNT as u16 * 2;
        let compact_h = 1 + METER_COUNT as u16;
        let meters_compact = rest.height().saturating_sub(full_h) < 6;
        let meters_h = if meters_compact { compact_h } else { full_h }.min(rest.height());
        let (card_area, meters) = panel::split_bottom(rest, meters_h);

        // Height is capped too, and independently of width: an uncapped
        // card on a tall phone would still be the single biggest element on
        // screen, but almost all of it would be blank interior below a few
        // lines of text, which reads as broken rather than generous. Capping
        // it and centering the result in the leftover vertical space turns
        // that same leftover space into deliberate margin around a card that
        // stays a believable card shape at every height.
        let card_w = card_area.width().min(CARD_MAX_W);
        let card_h = card_area.height().min(CARD_MAX_H);
        let card_x = card_area.left() + (card_area.width() - card_w) / 2;
        let card_y = card_area.top() + (card_area.height() - card_h) / 2;
        let card = Rect::new(card_x, card_y, card_w, card_h);

        // Buttons: as wide as they can be while sitting under the card with
        // a one-column gap, never below the tappable minimum. Centered under
        // the card rather than the full content width, so a wide desktop
        // window does not spread them so far apart that a thumb -- or an eye
        // scanning between them -- has to travel across the whole screen.
        let total_w = card_w.min(button_band.width());
        let bx0 = button_band.left() + (button_band.width().saturating_sub(total_w)) / 2;
        let gap = 2u16.min(total_w);
        let half = (total_w.saturating_sub(gap)) / 2;
        let by = button_band.top() + button_band.height().saturating_sub(touch::TAP_H);
        let bh = touch::TAP_H.min(button_band.height());
        let left_btn = Rect::new(bx0, by, half.max(1), bh);
        let right_btn = Rect::new(
            bx0 + half + gap,
            by,
            total_w.saturating_sub(half + gap).max(1),
            bh,
        );

        Self {
            card,
            meters,
            meters_compact,
            left_btn,
            right_btn,
        }
    }
}

/// A reign that has just ended: which meter broke it, and which direction.
struct DeathInfo {
    meter_i: usize,
    maxed: bool,
}

/// State: the meters, the current petition queue, the swipe/lean animation,
/// succession bookkeeping, and the shared touch plumbing.
pub struct CourtReigns {
    meters: [f32; METER_COUNT],
    queue: VecDeque<usize>,
    rng_seed: u32,
    year: u32,
    monarch: u32,
    time: f32,
    /// Current visual lean, `-1.0` (full left) to `1.0` (full right). Driven
    /// 1:1 by an active drag; eases toward [`Self::lean_target`] otherwise,
    /// which is what produces both the release-short snap-back and the
    /// settle-in of a freshly committed card. See the module docs.
    lean: f32,
    lean_target: f32,
    /// Column the current press started at, in screen space, iff the press
    /// landed on the card. `None` means either nothing is held, or something
    /// is held that started outside the card (which must not steer it).
    press_x: Option<i32>,
    death: Option<DeathInfo>,
    death_timer: f32,
    log: Log,
    pointer: Pointer,
    hotspots: Hotspots<Side>,
    fps: FpsMeter,
}

impl Default for CourtReigns {
    fn default() -> Self {
        let mut state = Self {
            meters: [0.5; METER_COUNT],
            queue: VecDeque::new(),
            rng_seed: 0x5216_C047,
            year: 1201,
            monarch: 1,
            time: 0.0,
            lean: 0.0,
            lean_target: 0.0,
            press_x: None,
            death: None,
            death_timer: 0.0,
            log: Log::new(32),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        };
        state.refill_deck();
        state
            .log
            .push("The court convenes. Long may you reign.", ui::ACCENT);
        state
    }
}

impl CourtReigns {
    fn current_petition(&self) -> &'static Petition {
        &PETITIONS[self.queue[0]]
    }

    /// Refills and reshuffles the petition queue with every index once,
    /// deterministically. The seed advances by a fixed odd constant each
    /// call rather than reading any wall-clock source, so two renders of the
    /// same session produce the same deck order -- required by the
    /// determinism test every demo in this gallery has to pass.
    fn refill_deck(&mut self) {
        self.rng_seed = self.rng_seed.wrapping_add(0x9E37_79B9);
        let mut rng = Rng::new(self.rng_seed);
        let mut order: Vec<usize> = (0..PETITIONS.len()).collect();
        for i in (1..order.len()).rev() {
            let j = rng.next_below(i as u32 + 1) as usize;
            order.swap(i, j);
        }
        self.queue = order.into();
    }

    /// Applies one answer: logs it, moves the meters, and -- if any meter
    /// crossed either extreme -- ends the reign. Ignored while a death
    /// epitaph is already on screen, so a stray keypress during the pause
    /// cannot skip the moment the whole mechanic hinges on.
    fn commit(&mut self, side: Side) {
        if self.death_timer > 0.0 {
            return;
        }
        let petition = self.current_petition();
        let answer = match side {
            Side::Left => &petition.left,
            Side::Right => &petition.right,
        };
        self.log.push(
            format!("{}: \"{}\"", petition.advisor.name(), answer.label),
            petition.advisor.accent(),
        );

        // Both directions of every meter are checked, and the first one
        // found breaks the reign -- see the module docs on why fatal-at-both
        // -ends is the rule that gives every petition a real trade-off.
        let mut fatal = None;
        for i in 0..METER_COUNT {
            let raw = self.meters[i] + answer.delta[i];
            if fatal.is_none() && raw <= 0.0 {
                fatal = Some((i, false));
            } else if fatal.is_none() && raw >= 1.0 {
                fatal = Some((i, true));
            }
            self.meters[i] = raw.clamp(0.0, 1.0);
        }

        self.year += 1;
        // Snap the lean to a full swipe regardless of how far the finger
        // actually travelled (a keyboard commit never dragged at all): this
        // is the "same lean animation" keyboard parity promises. The target
        // stays zero, so the very next frame starts easing back to neutral
        // as the incoming card settles.
        self.lean = side.sign();
        self.lean_target = 0.0;

        self.queue.pop_front();
        if self.queue.is_empty() {
            self.refill_deck();
        }

        if let Some((meter_i, maxed)) = fatal {
            self.log.push(epitaph(meter_i, maxed), rgb(216, 100, 96));
            self.death = Some(DeathInfo { meter_i, maxed });
            self.death_timer = DEATH_DURATION;
        }
    }

    fn finish_death(&mut self) {
        self.death = None;
        self.monarch += 1;
        self.meters = [0.5; METER_COUNT];
        self.log.push(
            format!("Monarch {} takes the throne.", self.monarch),
            ui::ACCENT,
        );
    }

    /// The lean actually drawn this frame: 1:1 with the drag while one is
    /// live, otherwise the eased [`Self::lean`] plus a small idle sway that
    /// fades out while any lean is still settling, so the sway never fights
    /// the snap-back or settle-in animations it shares the same value with.
    fn visual_lean(&self) -> f32 {
        if self.death.is_some() {
            return 0.0;
        }
        if self.press_x.is_some() {
            return self.lean;
        }
        let sway = 0.045 * (self.time * 0.7).sin();
        self.lean + sway * (1.0 - self.lean.abs()).max(0.0)
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>, layout: &Layout) -> bool {
        for event in term.drain_events() {
            self.pointer.feed(&event);
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Left => self.commit(Side::Left),
                    KeyCode::Right => self.commit(Side::Right),
                    _ => {}
                }
            }
        }

        let gesture: Gesture = self.pointer.take();
        self.apply_gesture(&gesture, layout);
        true
    }

    /// Turns this frame's pointer gesture into card lean, a possible commit,
    /// or a button tap. A press only starts steering the card if it landed
    /// on the card itself ([`Layout::card`]); a press starting elsewhere
    /// (over a button, or empty chrome) never drags the card, which is what
    /// lets the two tap buttons and the swipe coexist without either
    /// stealing the other's gestures.
    fn apply_gesture(&mut self, gesture: &Gesture, layout: &Layout) {
        if let Some(p) = gesture.press {
            if self.press_x.is_none() && self.death.is_none() && layout.card.contains_pos(p) {
                self.press_x = Some(i32::from(p.x));
            }
        } else {
            self.press_x = None;
        }

        let half_card = (f32::from(layout.card.width()) / 2.0).max(1.0);

        if let (Some(px), Some(d)) = (self.press_x, gesture.drag) {
            let t = ((f32::from(d.x) - px as f32) / half_card).clamp(-1.0, 1.0);
            self.lean = t;
            self.lean_target = t;
        }

        if let Some(drop) = gesture.drop {
            if let Some(px) = self.press_x {
                let t = ((f32::from(drop.x) - px as f32) / half_card).clamp(-1.0, 1.0);
                if t.abs() >= COMMIT_THRESHOLD {
                    self.commit(if t < 0.0 { Side::Left } else { Side::Right });
                } else {
                    // Short of the line: ease back to neutral rather than
                    // snapping instantly, so a rejected swipe reads as the
                    // card declining to leave rather than an input error.
                    self.lean_target = 0.0;
                }
            }
            self.press_x = None;
        }

        if let Some(tap) = gesture.tap
            && self.death.is_none()
            && let Some(side) = self.hotspots.hit(tap)
        {
            self.commit(*side);
        }
    }

    fn simulate(&mut self, dt: f32) {
        if self.press_x.is_none() {
            // Exponential ease rather than a linear step, so the snap-back
            // and settle-in start fast (the moment the finger lifts, or the
            // moment the new card appears) and slow into place rather than
            // arriving with a jolt.
            let k = 1.0 - (-9.0 * dt).exp();
            self.lean = (self.lean_target - self.lean).mul_add(k, self.lean);
            if self.lean.abs() < 0.001 {
                self.lean = 0.0;
            }
        }
        if self.death_timer > 0.0 {
            self.death_timer -= dt;
            if self.death_timer <= 0.0 {
                self.finish_death();
            }
        }
    }

    fn status(&self) -> String {
        format!("Monarch {}  -  Year {}", self.monarch, self.year)
    }

    // -- drawing --------------------------------------------------------

    fn draw(&self, surface: &mut Surface<'_>, layout: &Layout) {
        self.draw_card(surface, layout.card);
        self.draw_meters(surface, layout.meters, layout.meters_compact);
        self.draw_buttons(surface, layout.left_btn, layout.right_btn);
    }

    /// The card: a sheared frame, the advisor's portrait (dropped first if
    /// height is scarce), their name, and their wrapped petition, with the
    /// armed answer's label overlaid on the bottom border while a drag is
    /// live. See the module docs for why the lean is a per-row shear rather
    /// than any kind of scale or true rotation.
    fn draw_card(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 6 || area.height() < 4 {
            return;
        }
        let petition = self.current_petition();
        let lean = self.visual_lean();
        let dragging_now = self.press_x.is_some();
        let armed = dragging_now && lean.abs() >= COMMIT_THRESHOLD;
        let dying = self.death.is_some();

        let bg = panel::PANEL_BG;
        let base_accent = petition.advisor.accent();
        let frame_color = if dying {
            rgb(150, 62, 60)
        } else if armed {
            mix(base_accent, rgb(255, 255, 255), 0.5)
        } else {
            base_accent
        };
        let heavy = armed || dying;

        let w = area.width();
        let h = area.height();
        for row in 0..h {
            let y = area.top() + row;
            let shift = row_shift(row, h, lean, w);
            Self::draw_card_row(surface, area, row, y, shift, w, h, frame_color, bg, heavy);
        }

        Self::draw_role_badge(surface, area, lean, petition, frame_color, bg);

        self.draw_card_content(surface, area, lean, petition, dying);

        if dragging_now && lean.abs() > 0.03 {
            Self::draw_swipe_label(surface, area, lean, petition, armed);
        }
    }

    /// The advisor's role in the top border, echoing how a playing card's
    /// cost sits in its own border row in [`ui::card`]: the border is the
    /// one row a wider or narrower card interior never reshuffles, so a
    /// short badge parked there survives every [`Shape`] without competing
    /// with the portrait or the petition text for interior rows.
    fn draw_role_badge(
        surface: &mut Surface<'_>,
        area: Rect,
        lean: f32,
        petition: &Petition,
        frame_color: Color,
        bg: Color,
    ) {
        if area.width() < 12 {
            return;
        }
        let text = format!(" {} ", petition.advisor.role());
        let shift = row_shift(0, area.height(), lean, area.width());
        if let Some(x) = shift_x(area.left() + 2, shift) {
            surface.print((x, area.top()), &text, Style::new().fg(frame_color).bg(bg));
        }
    }

    /// One row of the card's frame: background fill, then whichever border
    /// glyphs belong on this row, all placed at the row's own sheared x.
    #[allow(clippy::too_many_arguments)]
    fn draw_card_row(
        surface: &mut Surface<'_>,
        area: Rect,
        row: u16,
        y: u16,
        shift: i32,
        w: u16,
        h: u16,
        frame_color: Color,
        bg: Color,
        heavy: bool,
    ) {
        let Some(x0) = shift_x(area.left(), shift) else {
            return;
        };
        let bg_style = Style::new().bg(bg);
        surface.print((x0, y), &" ".repeat(usize::from(w)), bg_style);

        let style = Style::new().fg(frame_color).bg(bg);
        let (tl, tr, bl, br, dash, vert) = if heavy {
            (
                '\u{2554}', '\u{2557}', '\u{255A}', '\u{255D}', '\u{2550}', '\u{2551}',
            )
        } else {
            (
                '\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{2500}', '\u{2502}',
            )
        };

        if row == 0 {
            let line = format!(
                "{tl}{}{tr}",
                dash.to_string().repeat(usize::from(w.saturating_sub(2)))
            );
            surface.print((x0, y), &line, style);
        } else if row + 1 == h {
            let line = format!(
                "{bl}{}{br}",
                dash.to_string().repeat(usize::from(w.saturating_sub(2)))
            );
            surface.print((x0, y), &line, style);
        } else {
            surface.put((x0, y), vert, style);
            if let Some(xr) = shift_x(area.left() + w - 1, shift) {
                surface.put((xr, y), vert, style);
            }
        }
    }

    /// Portrait, name, divider, and wrapped petition text, each printed at
    /// its own row's sheared x so the block leans as one piece. Rows are
    /// dropped from the top of this list first when the interior is short:
    /// portrait, then the divider, then the text is simply given whatever
    /// remains -- the art is decoration, the words are the decision.
    fn draw_card_content(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        lean: f32,
        petition: &Petition,
        dying: bool,
    ) {
        let interior_w = area.width().saturating_sub(2);
        let interior_h = area.height().saturating_sub(2);
        if interior_w == 0 || interior_h == 0 {
            return;
        }
        let bg = panel::PANEL_BG;
        let accent = petition.advisor.accent();

        let show_portrait = interior_h >= PORTRAIT_H + 1 + 1 + 2;
        let show_divider = interior_h >= 1 + 1 + 2;

        let mut lines: Vec<(String, Color)> = Vec::new();
        if show_portrait {
            for l in petition.advisor.portrait() {
                lines.push(((*l).to_string(), accent));
            }
        }
        lines.push((petition.advisor.name().to_string(), FG));
        if show_divider {
            lines.push(("-".repeat(usize::from(interior_w.min(TEXT_MAX_W))), DIM));
        }

        let used = lines.len() as u16;
        let text_rows = interior_h.saturating_sub(used);
        if text_rows > 0 {
            let wrap_w = usize::from(interior_w.min(TEXT_MAX_W));
            let text_color = if dying { rgb(200, 130, 128) } else { DIM };
            for line in wrap(petition.text, wrap_w)
                .into_iter()
                .take(usize::from(text_rows))
            {
                lines.push((line, text_color));
            }
        }
        lines.truncate(usize::from(interior_h));

        // The block is left-anchored at one shared x per row (not centered
        // per line), so multi-row art stays internally aligned exactly as
        // authored instead of each row centering independently around
        // whatever text happens to be on it.
        let art_w = lines
            .iter()
            .map(|(l, _)| l.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let pad = interior_w.saturating_sub(art_w) / 2;

        // Centered vertically, not pinned to the top: a card that grows with
        // the viewport (see the module docs on why width is capped but
        // height is not) would otherwise leave one big gap under a short
        // wrapped petition instead of a balanced margin above and below it.
        let top_gap = interior_h.saturating_sub(lines.len() as u16) / 2;

        for (i, (line, color)) in lines.iter().enumerate() {
            let row = 1 + top_gap + i as u16;
            let shift = row_shift(row, area.height(), lean, area.width());
            if let Some(x) = shift_x(area.left() + 1 + pad, shift) {
                surface.print((x, area.top() + row), line, Style::new().fg(*color).bg(bg));
            }
        }

        // Spare margin (a tall phone card comfortably outsizes its own
        // content) is spent on a pair of flickering candles rather than left
        // blank, which is what keeps the card animating even when nothing is
        // being dragged -- see the module docs' idle-sway note and
        // `flicker`.
        self.draw_candles(surface, area, lean, pad, top_gap);

        if dying {
            self.draw_epitaph_overlay(surface, area, lean);
        }
    }

    /// Two candle flames in the card's side margins, present only when there
    /// is real margin to spend them in (a narrow card's whole interior is
    /// already claimed by the portrait and text). Brightness comes from
    /// [`flicker`], a sum of two sine waves at deliberately non-integer-ratio
    /// frequencies plus a `time`-seeded hash jitter: pure sine breathes too
    /// evenly to read as fire, and pure hash noise has no continuity between
    /// frames, so this is the same "noise on top of a slow signal" trick the
    /// starfield twinkle in `21_deck_plan` uses, tuned faster.
    fn draw_candles(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        lean: f32,
        pad: u16,
        top_gap: u16,
    ) {
        if self.death.is_some() || pad < 5 || area.height() < 8 {
            return;
        }
        let interior_h = area.height().saturating_sub(2);
        let mid_row = 1 + top_gap.min(interior_h.saturating_sub(1)) + top_gap.min(interior_h) / 4;
        let bg = panel::PANEL_BG;

        for (seed, at_left) in [(11_u32, true), (29_u32, false)] {
            let b = flicker(self.time, seed);
            let flame = mix(rgb(60, 24, 8), rgb(255, 178, 70), b);
            let glow = mix(bg, rgb(90, 40, 12), b * 0.5);
            let x = if at_left {
                area.left() + 1
            } else {
                area.right() - 2
            };

            let rows: [(i32, char, Color); 3] = [
                (-1, '\u{25B2}', flame),
                (0, '|', mix(rgb(200, 180, 150), rgb(255, 220, 160), b)),
                (1, '_', glow),
            ];
            for (dy, glyph, color) in rows {
                let row = (i32::from(mid_row) + dy).clamp(1, i32::from(interior_h)) as u16;
                let shift = row_shift(row, area.height(), lean, area.width());
                if let Some(sx) = shift_x(x, shift) {
                    surface.put((sx, area.top() + row), glyph, Style::new().fg(color).bg(bg));
                }
            }
        }
    }

    /// While a reign has just ended, the epitaph replaces the card's own
    /// content for a few seconds ([`DEATH_DURATION`]) rather than sharing the
    /// space with it -- the whole point of the pause is that nothing else is
    /// competing for the player's attention at the one moment the run
    /// actually resolves.
    fn draw_epitaph_overlay(&self, surface: &mut Surface<'_>, area: Rect, lean: f32) {
        let Some(death) = &self.death else { return };
        let interior_w = usize::from(area.width().saturating_sub(4));
        if interior_w < 8 || area.height() < 6 {
            return;
        }
        let bg = rgb(24, 12, 12);
        let text = epitaph(death.meter_i, death.maxed);
        let header = format!("Reign of Monarch {} ends.", self.monarch);
        let lines: Vec<String> = std::iter::once(header)
            .chain(wrap(text, interior_w))
            .collect();
        let start = area.top() + (area.height().saturating_sub(lines.len() as u16)) / 2;
        for (i, line) in lines.iter().enumerate() {
            let row = start - area.top() + i as u16;
            let shift = row_shift(row, area.height(), lean, area.width());
            let pad =
                (area.width().saturating_sub(2)).saturating_sub(line.chars().count() as u16) / 2;
            if let Some(x) = shift_x(area.left() + 1 + pad, shift) {
                surface.print(
                    (x, area.top() + row),
                    line,
                    Style::new().fg(rgb(224, 160, 156)).bg(bg),
                );
            }
        }
    }

    /// Overlays the leaning-toward answer's label onto the bottom border, the
    /// same trick [`ui::card`] uses to keep a cost badge visible even when a
    /// card is covered: the border row is the one row nothing else needs, so
    /// spending it on the preview costs no interior content space.
    fn draw_swipe_label(
        surface: &mut Surface<'_>,
        area: Rect,
        lean: f32,
        petition: &Petition,
        armed: bool,
    ) {
        let side = if lean < 0.0 { Side::Left } else { Side::Right };
        let answer = match side {
            Side::Left => &petition.left,
            Side::Right => &petition.right,
        };
        let alpha = (lean.abs() / COMMIT_THRESHOLD).clamp(0.0, 1.0);
        let base = if side == Side::Left {
            rgb(210, 96, 92)
        } else {
            rgb(120, 190, 120)
        };
        let color = mix(panel::PANEL_BG, base, alpha);
        let text = if side == Side::Left {
            format!("<< {}", answer.label.to_uppercase())
        } else {
            format!("{} >>", answer.label.to_uppercase())
        };
        let row = area.height().saturating_sub(1);
        let shift = row_shift(row, area.height(), lean, area.width());
        let x = if side == Side::Left {
            area.left() + 1
        } else {
            area.right().saturating_sub(1 + text.chars().count() as u16)
        };
        let style = if armed {
            Style::new().fg(rgb(20, 16, 16)).bg(color)
        } else {
            Style::new().fg(color).bg(panel::PANEL_BG)
        };
        if let Some(sx) = shift_x(x, shift) {
            surface.print((sx, area.top() + row), &text, style);
        }
    }

    /// The four power meters. While a drag is live, the meter(s) the leaning
    /// answer would affect get a ghosted preview segment and a signed delta
    /// -- this is the panel [`Gesture::hover`] would have populated on a
    /// desktop UI, rebuilt on the one input touch actually has.
    fn draw_meters(&self, surface: &mut Surface<'_>, area: Rect, compact: bool) {
        if area.width() < 10 || area.height() == 0 {
            return;
        }
        let bg = panel::PANEL_BG;
        surface.fill_rect(area, ' ', Style::new().bg(bg));

        let preview = self.current_preview();
        let mut y = area.top();
        if !compact {
            surface.print(
                (area.left() + 1, y),
                "PILLARS OF POWER",
                Style::new().fg(DIM).bg(bg),
            );
            y += 1;
        }

        let row_h = if compact { 1 } else { 2 };
        for i in 0..METER_COUNT {
            if y >= area.bottom() {
                break;
            }
            let delta = preview.and_then(|(answer, alpha)| {
                let d = answer.delta[i];
                (d != 0.0).then_some((d, alpha))
            });
            self.draw_meter(surface, area, y, compact, i, delta);
            y += row_h;
        }
    }

    /// This frame's preview answer and its intensity, or `None` when nothing
    /// is being dragged. Intensity scales from 0 at the first pixel of drag
    /// to 1 at the commit line, so the ghost fades in rather than snapping
    /// on -- the graded version of "replacing hover" the module docs
    /// describe: a tooltip is binary, this is continuous with the gesture
    /// that will commit it.
    fn current_preview(&self) -> Option<(&'static Answer, f32)> {
        if self.press_x.is_none() || self.death.is_some() {
            return None;
        }
        let lean = self.lean;
        if lean.abs() < 0.02 {
            return None;
        }
        let petition = self.current_petition();
        let answer = if lean < 0.0 {
            &petition.left
        } else {
            &petition.right
        };
        Some((answer, (lean.abs() / COMMIT_THRESHOLD).clamp(0.0, 1.0)))
    }

    fn draw_meter(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        y: u16,
        compact: bool,
        i: usize,
        delta: Option<(f32, f32)>,
    ) {
        let bg = panel::PANEL_BG;
        let value = self.meters[i];
        let color = meter_color(i);
        let label = METER_NAMES[i];

        let (label_x, bar_x, bar_w, bar_y) = if compact {
            let label_w = 7u16;
            let bar_x = area.left() + label_w;
            (
                area.left(),
                bar_x,
                area.width().saturating_sub(label_w + 5),
                y,
            )
        } else {
            (
                area.left(),
                area.left(),
                area.width().saturating_sub(5),
                y + 1,
            )
        };

        surface.print((label_x, y), label, Style::new().fg(FG).bg(bg));
        if bar_w == 0 {
            return;
        }

        panel::bar(
            surface,
            (bar_x, bar_y),
            bar_w,
            value,
            color,
            rgb(30, 30, 36),
        );

        if let Some((d, alpha)) = delta {
            let preview_value = (value + d).clamp(0.0, 1.0);
            let cur_cells = (value * f32::from(bar_w)).round() as i32;
            let prev_cells = (preview_value * f32::from(bar_w)).round() as i32;
            let ghost_base = if d >= 0.0 {
                rgb(140, 214, 140)
            } else {
                rgb(224, 100, 96)
            };
            let ghost = mix(bg, ghost_base, alpha);
            if prev_cells > cur_cells {
                for c in cur_cells..prev_cells {
                    if c < 0 || c >= i32::from(bar_w) {
                        continue;
                    }
                    surface.put(
                        (bar_x + c as u16, bar_y),
                        '\u{2591}',
                        Style::new().fg(ghost).bg(rgb(30, 30, 36)),
                    );
                }
            } else if prev_cells < cur_cells {
                for c in prev_cells..cur_cells {
                    if c < 0 || c >= i32::from(bar_w) {
                        continue;
                    }
                    surface.put(
                        (bar_x + c as u16, bar_y),
                        '\u{2592}',
                        Style::new().fg(ghost).bg(rgb(30, 30, 36)),
                    );
                }
            }

            let arrow = if d >= 0.0 { '\u{2191}' } else { '\u{2193}' };
            let arrow_x = bar_x + bar_w + 1;
            surface.put(
                (arrow_x, bar_y),
                arrow,
                Style::new().fg(mix(bg, ghost_base, alpha)).bg(bg),
            );
        }
    }

    /// The two tap buttons: the parity path for desktop and for anyone who
    /// never discovers the swipe. Sized to at least [`touch::TAP_W`] x
    /// [`touch::TAP_H`] by [`Layout::build`], and registered fresh into
    /// [`Hotspots`] every frame, matching every other demo's immediate-mode
    /// rule that a hotspot cannot outlive the control it points at.
    fn draw_buttons(&self, surface: &mut Surface<'_>, left: Rect, right: Rect) {
        self.draw_button(surface, left, "REFUSE", Side::Left, rgb(196, 92, 88));
        self.draw_button(surface, right, "ACCEPT", Side::Right, rgb(112, 176, 112));
    }

    fn draw_button(
        &self,
        surface: &mut Surface<'_>,
        rect: Rect,
        label: &str,
        side: Side,
        color: Color,
    ) {
        if rect.width() < 3 || rect.height() < 2 {
            return;
        }
        let armed = self.press_x.is_some()
            && self.lean.abs() >= COMMIT_THRESHOLD
            && ((side == Side::Left) == (self.lean < 0.0));
        let frame = if armed {
            mix(color, rgb(255, 255, 255), 0.4)
        } else {
            color
        };
        let bg = panel::PANEL_BG;
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let style = Style::new().fg(frame).bg(bg);
        let (l, t, r, b) = (rect.left(), rect.top(), rect.right() - 1, rect.bottom() - 1);
        surface.put((l, t), '\u{250C}', style);
        surface.put((r, t), '\u{2510}', style);
        surface.put((l, b), '\u{2514}', style);
        surface.put((r, b), '\u{2518}', style);
        for x in (l + 1)..r {
            surface.put((x, t), '\u{2500}', style);
            surface.put((x, b), '\u{2500}', style);
        }
        for y in (t + 1)..b {
            surface.put((l, y), '\u{2502}', style);
            surface.put((r, y), '\u{2502}', style);
        }
        let pad = rect.width().saturating_sub(label.chars().count() as u16) / 2;
        let ty = t + (rect.height().saturating_sub(1)) / 2;
        surface.print((l + pad, ty), label, Style::new().fg(FG).bg(bg));
    }
}

/// Horizontal shift for `row` of a card `height` rows tall leaning by
/// `lean` (`-1.0..=1.0`), given the card is `card_w` columns wide.
///
/// A character grid cannot rotate a rectangle by a few degrees or scale text
/// -- both need sub-cell resolution this medium does not have. What it can
/// do cheaply is translate each row by a different amount, and choosing an
/// amount that grows toward the top (the pivot sits near the bottom) is
/// enough for the eye to read the result as a card tilting rather than a
/// block sliding sideways. `card_w` sets the cap on how far that shear can
/// reach, so a narrow phone card leans a plausible few columns while a wide
/// desktop card is allowed a slightly wider swing without either looking
/// like it barely moved or flying off its own frame.
fn row_shift(row: u16, height: u16, lean: f32, card_w: u16) -> i32 {
    let max_shift = (f32::from(card_w) * 0.12).clamp(1.0, 6.0);
    let factor = if height <= 1 {
        1.0
    } else {
        let t = 1.0 - f32::from(row) / f32::from(height - 1); // 1 at top, 0 at bottom
        0.7f32.mul_add(t, 0.3)
    };
    (lean * max_shift * factor).round() as i32
}

/// Candle brightness in `0.3..=1.0` at world-time `time` for a given `seed`
/// (so the two candles flicker independently rather than in lockstep). See
/// [`CourtReigns::draw_candles`] for why this mixes a sine signal with a
/// hashed jitter rather than using either alone.
fn flicker(time: f32, seed: u32) -> f32 {
    let phase = f32::from(seed as u16);
    let wave_a = time.mul_add(3.1, phase).sin().mul_add(0.5, 0.5);
    let wave_b = time.mul_add(5.3, phase * 1.7).sin().mul_add(0.5, 0.5);
    let slow = 0.5f32.mul_add(wave_a, 0.5 * wave_b);
    let jitter = hash01(seed, (time * 14.0) as i32, 0) * 0.2;
    0.5f32.mul_add(slow, 0.4 + jitter).clamp(0.3, 1.0)
}

/// `base + shift` as a `u16`, or `None` if that would be negative.
/// [`Surface::put`]/[`Surface::print`] already no-op past the *positive*
/// edge of their area, but a `u16` cannot represent a negative column at
/// all, so the negative side needs its own guard before the cast.
const fn shift_x(base: u16, shift: i32) -> Option<u16> {
    let v = base as i32 + shift;
    if v < 0 { None } else { Some(v as u16) }
}

impl Demo for CourtReigns {
    const NAME: &'static str = "36_court_reigns";
    const TITLE: &'static str = "36 Court of Reigns";
    const BLURB: &'static str =
        "A medieval court answered by swipe: preview the cost, then commit past the line.";
    const GRID: (u16, u16) = (150, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Left/Right", "refuse / accept (same lean as a swipe)"),
            (
                "drag card",
                "swipe to preview, release past the line to commit",
            ),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        let area = term.area();
        let (title, content, status) = ui::split_chrome(area);
        let shape = Shape::of(content);
        let layout = Layout::build(content, shape);

        // Hotspots are rebuilt every frame from this frame's own layout,
        // before events are read, never retained: a button that was not
        // drawn this frame cannot be hit this frame. See
        // `ui::touch::Hotspots`.
        self.hotspots.clear();
        self.hotspots.push(layout.left_btn, Side::Left);
        self.hotspots.push(layout.right_btn, Side::Right);

        if !self.handle_events(term, &layout) {
            return false;
        }
        self.simulate(dt);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, &layout);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(CourtReigns);

#[cfg(test)]
mod shape_smoke_tests {
    use super::{COMMIT_THRESHOLD, CourtReigns, METER_COUNT, Side};
    use ascii_tile_demos::Demo;
    use retroglyph_core::event::{Event, MouseButton, MouseEvent, MouseEventKind};
    use retroglyph_core::{Frame, Headless, KeyModifiers, Pos, Terminal};
    use std::time::Duration;

    fn render_at(cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(Headless::new(cols, rows));
        let mut demo = CourtReigns::init(&mut term);
        for i in 0..5u64 {
            let frame = Frame {
                delta: Duration::from_millis(16),
                frame: i,
            };
            assert!(
                demo.tick(&mut term, &frame),
                "demo should not quit on its own"
            );
        }
        term.present().expect("present");
        term.backend().format_view()
    }

    #[test]
    fn portrait_renders_something() {
        let view = render_at(73, 79);
        assert!(
            view.chars().any(|c| !c.is_whitespace()),
            "portrait view was blank:\n{view}"
        );
    }

    #[test]
    fn landscape_renders_something() {
        let view = render_at(158, 36);
        assert!(
            view.chars().any(|c| !c.is_whitespace()),
            "landscape view was blank:\n{view}"
        );
    }

    #[test]
    fn desktop_renders_something() {
        let view = render_at(160, 50);
        assert!(
            view.chars().any(|c| !c.is_whitespace()),
            "desktop view was blank:\n{view}"
        );
    }

    #[test]
    fn very_small_terminal_does_not_panic() {
        let view = render_at(80, 24);
        assert!(
            view.chars().any(|c| !c.is_whitespace()),
            "80x24 view was blank:\n{view}"
        );
    }

    /// Every animated element here (the sway, the flicker) is driven by
    /// `frame.delta` accumulated into `self.time`, never by wall-clock time
    /// or a seed the gallery's determinism test would catch drifting.
    #[test]
    fn two_identical_runs_render_identically() {
        assert_eq!(render_at(80, 24), render_at(80, 24));
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            position: Pos::new(x, y),
            pixel_position: None,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn tick(term: &mut Terminal<Headless>, demo: &mut CourtReigns, i: u64) {
        let frame = Frame {
            delta: Duration::from_millis(16),
            frame: i,
        };
        assert!(demo.tick(term, &frame), "demo should not quit on its own");
        term.present().expect("present");
    }

    /// A drag that ends short of the commit line must leave the meters
    /// untouched and ease the card back to neutral -- the "release short of
    /// the threshold costs nothing" guarantee the module docs describe.
    #[test]
    fn a_short_drag_previews_but_does_not_commit() {
        let mut term = Terminal::new(Headless::new(160, 50));
        let mut demo = CourtReigns::init(&mut term);
        tick(&mut term, &mut demo, 0);
        let before = demo.meters;

        let card = demo_card_center(&demo, &term);
        term.backend_mut().push_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.0,
            card.1,
        ));
        tick(&mut term, &mut demo, 1);
        assert!(
            demo.press_x.is_some(),
            "a press on the card must start tracking a drag"
        );

        // A handful of columns is well short of half the card's width.
        term.backend_mut().push_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            card.0 + 4,
            card.1,
        ));
        tick(&mut term, &mut demo, 2);
        assert!(demo.lean > 0.0, "the card should lean toward the drag");
        assert!(
            demo.lean.abs() < COMMIT_THRESHOLD,
            "a short drag must stay under the commit line"
        );

        term.backend_mut().push_event(mouse(
            MouseEventKind::Up(MouseButton::Left),
            card.0 + 4,
            card.1,
        ));
        tick(&mut term, &mut demo, 3);
        assert!(
            meters_match(demo.meters, before),
            "releasing short of the line must not change any meter"
        );
    }

    /// The keyboard path commits immediately (no preview needed: the player
    /// has already decided) and produces the same full-lean snap the module
    /// docs promise for keyboard parity.
    #[test]
    fn a_keyboard_answer_commits_immediately_and_leans_fully() {
        let mut term = Terminal::new(Headless::new(160, 50));
        let mut demo = CourtReigns::init(&mut term);
        tick(&mut term, &mut demo, 0);
        let before_year = demo.year;

        demo.commit(Side::Right);
        assert_eq!(
            demo.year,
            before_year + 1,
            "answering must advance the year"
        );
        assert!(
            (demo.lean - 1.0).abs() < f32::EPSILON,
            "a commit snaps the lean fully toward its side"
        );
    }

    /// Both directions of a meter are fatal, not just running out: pushing a
    /// meter to its ceiling ends the reign exactly as running it to zero
    /// does. This is the rule the whole module leans on; if it silently stops
    /// being true the demo stops being Reigns.
    #[test]
    fn a_meter_pinned_to_either_extreme_ends_the_reign() {
        let mut term = Terminal::new(Headless::new(160, 50));
        let mut demo = CourtReigns::init(&mut term);
        tick(&mut term, &mut demo, 0);
        let monarch = demo.monarch;

        demo.meters[0] = 0.95;
        demo.queue.push_front(0); // "Grant the gold": +0.13 church, pins it past 1.0
        demo.commit(Side::Right);

        assert!(
            demo.death.is_some(),
            "a meter pushed past its ceiling must end the reign"
        );
        // DEATH_DURATION seconds at 16ms/frame, plus a margin.
        for i in 1..220u64 {
            tick(&mut term, &mut demo, i);
        }
        assert!(
            demo.death.is_none(),
            "the epitaph pause must eventually clear"
        );
        assert_eq!(
            demo.monarch,
            monarch + 1,
            "a new monarch must take the throne after a death"
        );
        assert!(
            meters_match(demo.meters, [0.5; METER_COUNT]),
            "the new reign must start from neutral meters"
        );
    }

    /// Compares two meter sets within a tolerance.
    ///
    /// The meters are `f32` and the assertions here mean "unchanged" and "back
    /// to neutral", not "bit-identical": a direct `assert_eq!` on a float array
    /// is both a denied lint and the wrong question, since a meter that had a
    /// zero-sized delta applied is equal in intent even if not in bits.
    fn meters_match(a: [f32; METER_COUNT], b: [f32; METER_COUNT]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    /// Finds a screen column/row inside the currently drawn card by scanning
    /// for its top-left corner glyph, so the drag tests do not have to
    /// duplicate `Layout::build`'s arithmetic to know where to press.
    fn demo_card_center(_demo: &CourtReigns, term: &Terminal<Headless>) -> (u16, u16) {
        let view = term.backend().format_view();
        for (y, line) in view.lines().enumerate() {
            if let Some(x) = line.find('\u{250C}') {
                return (x as u16 + 4, y as u16 + 2);
            }
        }
        panic!("card frame not found in view:\n{view}");
    }
}
