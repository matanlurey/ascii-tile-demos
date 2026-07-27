//! 50: Veiled Hand -- the hidden villain's challenge tracker, racing two doom
//! meters, from Shadow of the Forbidden Gods.
//!
//! Every other strategy demo in this gallery casts the player as a kingdom.
//! This one casts the player as the thing kingdoms are afraid of: a cultist
//! agent working alone, whose "turn" is not "move a unit" but "keep working
//! the same multi-turn task without being noticed". The screenshot this is
//! built from shows exactly one number worth building a demo around --
//! `Progress: 2/50 (+2 per Turn)` -- a task that *accrues* across many turns
//! rather than resolving on the turn it is chosen, run against a clock
//! (`Seal Breaks In: 2 Turns`) that advances whether or not the player acts.
//! Everything else on screen (the agent sheet, the hex world, the challenge
//! list) exists to give that race a place to happen, and stays quiet so the
//! race stays legible.
//!
//! The world map is deliberately the smallest, dimmest thing on screen.
//! Demos 38 and 44 already own "the hex grid is the point"; here the hexes
//! are backdrop texture for a node-and-banner overlay, sized to whatever
//! fraction of the viewport the doom meters and challenge panel leave behind
//! rather than to a fixed extent, so it never competes with the tracker for
//! attention.
//!
//! Techniques on show:
//!
//! - **An accruing challenge, not an instant action**
//!   ([`VeiledHand::advance_turn`]): progress is `min(progress + rate,
//!   complexity)`, ticked once per turn rather than resolved on selection,
//!   so the player watches a task complete over many turns instead of
//!   choosing and being done.
//! - **Two doom meters racing the player's own clock**
//!   ([`VeiledHand::draw_doom`]): `Seals Broken` counts up on a shrinking
//!   cadence independent of anything the player does, while `World Panic`
//!   climbs only because the player's own chosen challenge has a `Profile`
//!   above zero -- exposure is a cost of *acting*, not an ambient hazard.
//! - **A one-move economy** ([`VeiledHand::try_select`]): the agent gets one
//!   challenge choice per turn (`Moves Remaining`), mirroring the source
//!   game's action economy and giving "do nothing, lie low" real weight
//!   instead of being a lesser option with no cost of its own.
//! - **A subordinate hex backdrop with a node-and-banner overlay**
//!   ([`VeiledHand::draw_map`]): hex terrain sized from the live panel rect
//!   (not a fixed extent, per the round-3 fill-the-viewport rule), a white
//!   line network linking settlement markers, and kingdom pennants -- a
//!   pole and a triangular flag -- hanging over two of them.
//! - **Tap-select-then-confirm-by-turn** ([`ui::touch::Hotspots`]): every
//!   challenge row and the End Turn control are real touch targets grown to
//!   [`ui::touch::TAP_W`]x[`ui::touch::TAP_H`]; picking a challenge is a
//!   single tap, spending the turn's one move.
//!
//! ```sh
//! cargo run --example 50_veiled_hand --features crossterm
//! cargo run --example 50_veiled_hand --features software
//! cargo run --example 50_veiled_hand --features gl
//! cargo run --example 50_veiled_hand  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;

use tilekit::geom::{Cell, HexLayout, HexOrientation, Tile};
use tilekit::glyphs::terrain;
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb, scale};

/// How many seals stand between the player and the win condition. Matches
/// the source game's own count (`SEALS BROKEN: 0 OF 9`).
const SEALS_TOTAL: u32 = 9;

/// How many `world_panic` samples the [`VeiledHand::panic_history`] strip
/// keeps. Comfortably wider than any panel this demo lays the strip out in,
/// so the strip is always the one being trimmed to the panel and never the
/// other way around.
const PANIC_HISTORY_LEN: usize = 64;

/// One task the agent can spend a turn's move committing to.
///
/// `profile` is exposure risk (how much it raises World Panic per turn it
/// stays active) and `complexity` is the total progress it needs, matching
/// the two numbers the source game prints on every challenge card. `rate` is
/// how much of that `complexity` one turn buys, so `complexity / rate` turns
/// (rounded up) is exactly the `Turns Left` figure shown in the tracker.
struct Challenge {
    name: &'static str,
    profile: u8,
    menace: u8,
    complexity: u32,
    rate: u32,
    /// One line of flavor, shown in the Challenges panel's detail block for
    /// whichever row is selected -- the panel has five rows and room for
    /// far more than five, and a fact about the task that is not already one
    /// of its three stat columns is what belongs in the rest of it.
    blurb: &'static str,
}

/// The five committable tasks. `LAY_LOW` is not a special case in the data --
/// it is just a challenge with zero profile and zero rate, whose "progress"
/// is meaningless and never shown, which is what makes lying low a real
/// choice on the same list rather than a separate button with different
/// rules.
const CHALLENGES: [Challenge; 5] = [
    Challenge {
        name: "Infiltrate Coven of Witches",
        profile: 2,
        menace: 7,
        complexity: 50,
        rate: 2,
        blurb: "Witches ward their coven well; every turn inside costs sanity.",
    },
    Challenge {
        name: "Lay Low",
        profile: 0,
        menace: 0,
        complexity: 7,
        rate: 0,
        blurb: "No progress, no exposure -- panic cools while the agent waits.",
    },
    Challenge {
        name: "Enshadow the Deep Woods",
        profile: 3,
        menace: 3,
        complexity: 25,
        rate: 2,
        blurb: "Cultists move easier once the woods go dark.",
    },
    Challenge {
        name: "Dark Worship at the Circle",
        profile: 3,
        menace: 6,
        complexity: 25,
        rate: 2,
        blurb: "The circle wants blood before it will listen.",
    },
    Challenge {
        name: "Corrupt the Harvest Fair",
        profile: 4,
        menace: 5,
        complexity: 40,
        rate: 3,
        blurb: "A crowd this size is exposure and opportunity both.",
    },
];

/// Index of the always-safe, always-available "do nothing dangerous" choice.
const LAY_LOW: usize = 1;

/// A settlement on the subordinate world map, positioned as a fraction of the
/// map panel so it lands sensibly at any viewport size rather than at a fixed
/// cell offset tuned for one layout.
struct Settlement {
    name: &'static str,
    frac_x: f32,
    frac_y: f32,
    /// Index into [`KINGDOMS`], for the two settlements that fly a banner.
    kingdom: Option<usize>,
}

/// Every name on the map is written out once, by hand, so uniqueness is a
/// property of the source rather than something a sampler has to guarantee at
/// runtime; `tests::settlement_names_are_unique` pins it.
const SETTLEMENTS: [Settlement; 7] = [
    Settlement {
        name: "Gise's Village",
        frac_x: 0.12,
        frac_y: 0.70,
        kingdom: None,
    },
    Settlement {
        name: "Ashgrave Rest",
        frac_x: 0.30,
        frac_y: 0.20,
        kingdom: None,
    },
    Settlement {
        name: "Estaire Hold",
        frac_x: 0.48,
        frac_y: 0.45,
        kingdom: Some(0),
    },
    Settlement {
        name: "Bramble Hollow",
        frac_x: 0.62,
        frac_y: 0.18,
        kingdom: None,
    },
    Settlement {
        name: "Raite Hold",
        frac_x: 0.80,
        frac_y: 0.55,
        kingdom: Some(1),
    },
    Settlement {
        name: "Widow's Landing",
        frac_x: 0.68,
        frac_y: 0.85,
        kingdom: None,
    },
    Settlement {
        name: "Cairn's Reach",
        frac_x: 0.30,
        frac_y: 0.90,
        kingdom: None,
    },
];

/// A settlement's banner overlord: the short name printed under its pennant
/// in [`VeiledHand::draw_map`] (kept short, rather than reusing the
/// settlement's own longer name, so two nearby banners never overlap on a
/// narrow map) plus the pennant color.
struct Kingdom {
    name: &'static str,
    color: Color,
}

const KINGDOMS: [Kingdom; 2] = [
    Kingdom {
        name: "Estaire",
        color: rgb(140, 70, 190),
    },
    Kingdom {
        name: "Raite",
        color: rgb(80, 150, 110),
    },
];

/// The hidden agent's sheet: the four attributes, two pools, and combat pair
/// the source game's left panel shows for "The Cursed".
struct Agent {
    might: u8,
    lore: u8,
    intrigue: u8,
    command: u8,
    hp: u8,
    hp_max: u8,
    sanity: u8,
    sanity_max: u8,
    attack: u8,
    defence: u8,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            might: 2,
            lore: 2,
            intrigue: 2,
            command: 2,
            hp: 5,
            hp_max: 5,
            sanity: 18,
            sanity_max: 18,
            attack: 2,
            defence: 2,
        }
    }
}

/// How the run currently stands. Freezes [`VeiledHand::advance_turn`] once it
/// leaves [`Self::Playing`], but the demo keeps animating regardless (the
/// blinking result banner), since a frozen *simulation* is not the same thing
/// as a frozen *frame*.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GameState {
    Playing,
    /// World Panic reached 100: the world found the agent first.
    Exposed,
    /// All nine seals broke: the agent won.
    Victorious,
}

/// What tapping a hotspot means.
#[derive(Clone, Copy)]
enum Action {
    Select(usize),
    EndTurn,
}

/// Seconds of simulated time between automatic turns. Slow enough to read as
/// discrete steps rather than a flicker, fast enough that an unattended
/// viewer sees several turns resolve within the animation-check window.
const TURN_PERIOD: f32 = 2.2;

/// How many turns until the next seal breaks, once `seals_broken` reaches the
/// given count. Shrinks as more seals fall, so the back half of a run reads
/// as visibly more urgent than the front half without any randomness.
const fn seal_cadence(seals_broken: u32) -> u32 {
    if seals_broken >= 6 {
        2
    } else if seals_broken >= 3 {
        3
    } else {
        4
    }
}

/// A `0.0..=1.0` breathing value, `phase` seconds ahead of `time` so several
/// callers driven off the same clock do not all peak in lockstep.
///
/// Every ambient animation in this file (bar shimmer, seal-pip breathing, the
/// selected challenge row, banner flutter, map pulses) is a pure function of
/// [`VeiledHand::time`] through this, never of stored state: the numbers the
/// tracker prints still only change on a turn boundary, but the picture
/// keeps moving between turns instead of sitting frozen for `TURN_PERIOD`
/// seconds at a stretch.
fn pulse01(time: f32, period: f32, phase: f32) -> f32 {
    (core::f32::consts::TAU * (time / period + phase))
        .sin()
        .mul_add(0.5, 0.5)
}

/// Full demo state.
pub struct VeiledHand {
    time: f32,
    turn: u32,
    turn_clock: f32,
    active: usize,
    /// Progress is kept per challenge, not reset on switch: abandoning a
    /// task to lie low for a turn does not erase the work already spent on
    /// it, so switching challenges is never a destructive choice a player
    /// needs an undo for.
    progress: [u32; CHALLENGES.len()],
    moves_remaining: u8,
    seals_broken: u32,
    seal_breaks_in: u32,
    world_panic: f32,
    /// Recent `world_panic` samples, oldest first, one pushed per turn and
    /// capped at [`PANIC_HISTORY_LEN`]. Fills the otherwise-empty lower half
    /// of the World panel with a strip that reads at a glance, rather than
    /// leaving the single current number as the only thing that panel says.
    panic_history: Vec<f32>,
    victory: f32,
    state: GameState,
    agent: Agent,
    minions_used: u8,
    minions_cap: u8,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for VeiledHand {
    fn default() -> Self {
        Self {
            time: 0.0,
            turn: 0,
            turn_clock: 0.0,
            active: 0,
            progress: [0; CHALLENGES.len()],
            moves_remaining: 1,
            seals_broken: 0,
            seal_breaks_in: seal_cadence(0),
            world_panic: 1.0,
            panic_history: vec![1.0],
            victory: 0.0,
            state: GameState::Playing,
            agent: Agent::default(),
            minions_used: 0,
            minions_cap: 2,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl VeiledHand {
    /// Spends the turn's one move committing to `idx`, if a move is still
    /// available. Silently ignored once spent, which is the whole point of
    /// tracking it: the player can look at every challenge's Profile and
    /// Complexity before picking, but picking twice in one turn is not on
    /// the menu, same as the source game's `Moves Remaining 1/1`.
    fn try_select(&mut self, idx: usize) {
        if self.state != GameState::Playing || self.moves_remaining == 0 || idx >= CHALLENGES.len()
        {
            return;
        }
        self.active = idx;
        self.moves_remaining = 0;
    }

    /// Forces the next turn immediately and resets the auto-advance clock,
    /// so pressing End Turn never causes a near-simultaneous automatic turn
    /// right behind it.
    fn force_turn(&mut self) {
        self.turn_clock = 0.0;
        self.advance_turn();
    }

    /// Resolves one turn: the active challenge accrues (or, for Lay Low,
    /// bleeds off panic instead), the seal clock ticks down, and the result
    /// is checked. Every field here changes by a fixed step, never eased --
    /// the numbers in the tracker are the whole point and must read as
    /// exact, not as a value mid-flight toward one.
    fn advance_turn(&mut self) {
        if self.state != GameState::Playing {
            return;
        }
        self.turn += 1;

        let challenge = &CHALLENGES[self.active];
        if self.active == LAY_LOW {
            // Lying low costs the turn's move but actively cools the world
            // down, which is what makes it a real tactical choice instead of
            // just "the turn where nothing happens".
            self.world_panic = (self.world_panic - 3.0).max(0.0);
        } else {
            self.progress[self.active] =
                (self.progress[self.active] + challenge.rate).min(challenge.complexity);
            let exposure = f32::from(challenge.profile).mul_add(0.8, 1.0);
            self.world_panic = (self.world_panic + exposure).min(100.0);
            self.agent.sanity = self.agent.sanity.saturating_sub(challenge.menace / 4);
        }
        self.panic_history.push(self.world_panic);
        if self.panic_history.len() > PANIC_HISTORY_LEN {
            self.panic_history.remove(0);
        }

        if self.seal_breaks_in > 0 {
            self.seal_breaks_in -= 1;
        }
        if self.seal_breaks_in == 0 && self.seals_broken < SEALS_TOTAL {
            self.seals_broken += 1;
            self.seal_breaks_in = seal_cadence(self.seals_broken);
        }
        self.victory = self.seals_broken as f32 / SEALS_TOTAL as f32 * 100.0;
        self.moves_remaining = 1;

        if self.seals_broken >= SEALS_TOTAL {
            self.state = GameState::Victorious;
        } else if self.world_panic >= 100.0 {
            self.state = GameState::Exposed;
        }
    }

    /// Turns left on the active challenge, rounded up: `complexity / rate`
    /// with the remaining progress subtracted first, exactly the arithmetic
    /// the source game's own `Turns Left` figure implies.
    const fn turns_left(&self) -> u32 {
        let challenge = &CHALLENGES[self.active];
        if challenge.rate == 0 {
            return 0;
        }
        let remaining = challenge
            .complexity
            .saturating_sub(self.progress[self.active]);
        remaining.div_ceil(challenge.rate)
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
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c @ '1'..='5') => {
                let idx = c as usize - '1' as usize;
                self.try_select(idx);
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.force_turn(),
            _ => {}
        }
    }

    fn handle_tap(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos) {
            Some(Action::Select(idx)) => {
                let idx = *idx;
                self.try_select(idx);
            }
            Some(Action::EndTurn) => self.force_turn(),
            None => {}
        }
    }

    /// The doom meters: seals broken (counting up on the fixed clock) on the
    /// left, world panic and victory (both driven by the player's own
    /// choices) on the right. Two panels rather than one wide one, because
    /// the source screenshot keeps them at opposite corners of the header --
    /// they are racing each other, and putting them in separate frames says
    /// so before either number does.
    fn draw_doom(&self, surface: &mut Surface<'_>, area: Rect) {
        let cols = panel::columns(area, 2, 1);
        self.draw_seals_panel(surface, cols[0]);
        self.draw_world_panel(surface, cols[1]);
    }

    /// The left doom meter: seals broken, counting up on the fixed clock
    /// independent of anything the player does. Split out of
    /// [`Self::draw_doom`] purely to keep each panel's drawing under one
    /// function's readable length; the two panels are still laid out side
    /// by side by their shared caller.
    fn draw_seals_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let seals_inner = Panel::new()
            .title("The Seals")
            .border(Border::Double)
            .draw(surface, area);
        if seals_inner.height() > 0 {
            panel::spans(
                surface,
                (seals_inner.left(), seals_inner.top()),
                seals_inner.width(),
                &[
                    Span::dim("Seals Broken: "),
                    Span::keyword(&format!("{} of {SEALS_TOTAL}", self.seals_broken)),
                ],
                panel::PANEL_BG,
            );
        }
        if seals_inner.height() > 1 {
            shimmer_bar(
                surface,
                (seals_inner.left(), seals_inner.top() + 1),
                seals_inner.width(),
                self.seals_broken as f32 / SEALS_TOTAL as f32,
                Shimmer {
                    fill: ui::ACCENT,
                    track: rgb(30, 30, 36),
                    time: self.time,
                    phase: 0.0,
                },
            );
        }
        if seals_inner.height() > 2 {
            draw_seal_pips(
                surface,
                (seals_inner.left(), seals_inner.top() + 2),
                seals_inner.width(),
                self.seals_broken,
                self.time,
            );
        }
        if seals_inner.height() > 3 {
            let text = if self.state == GameState::Victorious {
                "All seals broken.".to_string()
            } else {
                format!("Breaks in {} turns", self.seal_breaks_in)
            };
            panel::spans(
                surface,
                (seals_inner.left(), seals_inner.top() + 3),
                seals_inner.width(),
                &[Span::dim(&text)],
                panel::PANEL_BG,
            );
        }
    }

    /// The right doom meter: world panic and victory, both driven by the
    /// player's own choices. See [`Self::draw_seals_panel`] for why this is
    /// split out of [`Self::draw_doom`].
    fn draw_world_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let world_inner = Panel::new()
            .title("World")
            .border(Border::Double)
            .draw(surface, area);
        if world_inner.height() > 0 {
            panel::spans(
                surface,
                (world_inner.left(), world_inner.top()),
                world_inner.width(),
                &[
                    Span::dim("Panic: "),
                    Span::new(
                        &format!("{:.0}%", self.world_panic),
                        panel::threshold(1.0 - self.world_panic / 100.0),
                    ),
                ],
                panel::PANEL_BG,
            );
        }
        if world_inner.height() > 1 {
            shimmer_bar(
                surface,
                (world_inner.left(), world_inner.top() + 1),
                world_inner.width(),
                self.world_panic / 100.0,
                Shimmer {
                    fill: panel::threshold(1.0 - self.world_panic / 100.0),
                    track: rgb(30, 30, 36),
                    time: self.time,
                    phase: 0.33,
                },
            );
        }
        if world_inner.height() > 2 {
            draw_panic_strip(
                surface,
                (world_inner.left(), world_inner.top() + 2),
                world_inner.width(),
                &self.panic_history,
            );
        }
        if world_inner.height() > 3 {
            let text = match self.state {
                GameState::Exposed => "EXPOSED -- the world found you.".to_string(),
                _ => format!("Victory: {:.0}%", self.victory),
            };
            panel::spans(
                surface,
                (world_inner.left(), world_inner.top() + 3),
                world_inner.width(),
                &[Span::new(
                    &text,
                    if self.state == GameState::Exposed {
                        rgb(216, 88, 84)
                    } else {
                        ui::DIM
                    },
                )],
                panel::PANEL_BG,
            );
        }
    }

    /// The centre anchor: the active challenge's accrual, the whole point of
    /// this demo. Kept as its own panel (not folded into the challenge list)
    /// so it can sit near the top, next to the doom meters it is racing,
    /// while the list of alternatives lives further from the eye.
    fn draw_progress(&self, surface: &mut Surface<'_>, area: Rect) {
        let challenge = &CHALLENGES[self.active];
        let inner = Panel::new()
            .title("Challenge")
            .border(Border::Double)
            .focused(true)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let mut y = inner.top();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::keyword(challenge.name)],
            panel::PANEL_BG,
        );
        y += 1;
        if y >= inner.bottom() {
            return;
        }
        let progress = self.progress[self.active];
        if challenge.rate == 0 {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim("Cooling down, no exposure this turn.")],
                panel::PANEL_BG,
            );
        } else {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    Span::plain(&format!("Progress: {progress}/{} ", challenge.complexity)),
                    Span::dim(&format!("(+{} per turn)", challenge.rate)),
                ],
                panel::PANEL_BG,
            );
        }
        y += 1;
        if y < inner.bottom() {
            shimmer_bar(
                surface,
                (inner.left(), y),
                inner.width(),
                progress as f32 / challenge.complexity.max(1) as f32,
                Shimmer {
                    fill: ui::ACCENT,
                    track: rgb(30, 30, 36),
                    time: self.time,
                    phase: 0.66,
                },
            );
            y += 1;
        }
        if y < inner.bottom() {
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(&format!(
                    "Turns Left: {}   Profile: {}   Menace: {}",
                    self.turns_left(),
                    challenge.profile,
                    challenge.menace
                ))],
                panel::PANEL_BG,
            );
            y += 1;
        }
        if y < inner.bottom() {
            let color = if self.moves_remaining > 0 {
                ui::ACCENT
            } else {
                ui::DIM
            };
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::new(
                    &format!("Moves Remaining: {}/1", self.moves_remaining),
                    color,
                )],
                panel::PANEL_BG,
            );
        }
    }

    /// The agent sheet: attributes, pools, and the two equipment rails, all
    /// read-only status rather than controls, so it always lives above the
    /// thumb zone regardless of layout shape.
    fn draw_agent(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new().title("The Cursed").draw(surface, area);
        if inner.width() < 8 || inner.height() == 0 {
            return;
        }
        let mut y = inner.top();
        let mut line = |surface: &mut Surface<'_>, spans: &[Span<'_>]| {
            if y < inner.bottom() {
                panel::spans(
                    surface,
                    (inner.left(), y),
                    inner.width(),
                    spans,
                    panel::PANEL_BG,
                );
                y += 1;
            }
        };

        line(
            surface,
            &[
                Span::dim("Might "),
                Span::keyword(&self.agent.might.to_string()),
                Span::dim("  Lore "),
                Span::keyword(&self.agent.lore.to_string()),
            ],
        );
        line(
            surface,
            &[
                Span::dim("Intrigue "),
                Span::keyword(&self.agent.intrigue.to_string()),
                Span::dim("  Command "),
                Span::keyword(&self.agent.command.to_string()),
            ],
        );
        line(
            surface,
            &[Span::dim(&format!(
                "Hitpoints {}/{}",
                self.agent.hp, self.agent.hp_max
            ))],
        );
        line(
            surface,
            &[Span::dim(&format!(
                "Sanity {}/{}",
                self.agent.sanity, self.agent.sanity_max
            ))],
        );
        line(
            surface,
            &[
                Span::dim("Attack "),
                Span::keyword(&self.agent.attack.to_string()),
                Span::dim("  Defence "),
                Span::keyword(&self.agent.defence.to_string()),
            ],
        );
        line(surface, &[Span::dim("Items")]);
        line(surface, &[Span::plain("[ ] [ ] [ ] [ ]")]);
        line(
            surface,
            &[Span::dim(&format!(
                "Minions {}/{}",
                self.minions_used, self.minions_cap
            ))],
        );
    }

    /// The alternatives list, each row a real touch target: tapping a row
    /// spends the turn's move, the same as pressing a number key.
    fn draw_challenges(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Challenges")
            .badge(&format!("{}", CHALLENGES.len()))
            .draw(surface, area);
        if inner.width() < TAP_W || inner.height() < TAP_H {
            return;
        }
        let rows_avail = usize::from(inner.height() / TAP_H).max(1);
        let shown = rows_avail.min(CHALLENGES.len());
        // Scroll so the active row is always in view, rather than always
        // showing the first N and hiding a challenge the player already
        // committed to.
        let start = if self.active < shown {
            0
        } else {
            (self.active + 1 - shown).min(CHALLENGES.len() - shown)
        };

        for i in 0..shown {
            let idx = start + i;
            let challenge = &CHALLENGES[idx];
            let y = inner.top() + i as u16 * TAP_H;
            let row = Rect::new(inner.left(), y, inner.width(), TAP_H);
            self.hotspots.push(row, Action::Select(idx));

            let selected = idx == self.active;
            let bg = if selected {
                // Breathes rather than sitting at a fixed color: the row a
                // player already committed to is the one place on this
                // panel worth an ambient pulse, since it is also the row
                // [`VeiledHand::draw_progress`] is racing the clock on.
                mix(
                    rgb(40, 32, 20),
                    rgb(76, 58, 30),
                    pulse01(self.time, 2.4, 0.0),
                )
            } else {
                panel::PANEL_BG
            };
            surface.fill_rect(row, ' ', Style::new().bg(bg));

            let marker = if selected { "> " } else { "  " };
            panel::spans(
                surface,
                (row.left(), row.top()),
                row.width(),
                &[
                    Span::new(marker, ui::ACCENT),
                    Span::new(challenge.name, if selected { ui::ACCENT } else { ui::FG }),
                ],
                bg,
            );
            if row.height() > 1 {
                panel::spans(
                    surface,
                    (row.left() + 2, row.top() + 1),
                    row.width().saturating_sub(2),
                    &[Span::dim(&format!("Profile: {}", challenge.profile))],
                    bg,
                );
            }
            if row.height() > 2 {
                panel::spans(
                    surface,
                    (row.left() + 2, row.top() + 2),
                    row.width().saturating_sub(2),
                    &[Span::dim(&format!(
                        "Menace: {}  Complexity: {}",
                        challenge.menace, challenge.complexity
                    ))],
                    bg,
                );
            }
        }

        self.draw_challenge_detail(surface, inner, inner.top() + shown as u16 * TAP_H);
    }

    /// Fills whatever the five challenge rows leave below them with detail
    /// on the currently active challenge -- the one
    /// [`VeiledHand::draw_progress`] is racing -- rather than leaving the
    /// panel's bottom half blank once its five rows run out.
    fn draw_challenge_detail(&self, surface: &mut Surface<'_>, inner: Rect, list_bottom: u16) {
        if inner.bottom() < list_bottom + 4 {
            return;
        }
        let top = list_bottom + 1;
        let active = &CHALLENGES[self.active];
        panel::spans(
            surface,
            (inner.left(), top),
            inner.width(),
            &[Span::dim("Active assessment")],
            panel::PANEL_BG,
        );
        panel::spans(
            surface,
            (inner.left(), top + 1),
            inner.width(),
            &[Span::keyword(active.name)],
            panel::PANEL_BG,
        );
        let lines = wrap_text(active.blurb, usize::from(inner.width()));
        for (i, line) in lines.into_iter().enumerate() {
            let y = top + 2 + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(&line)],
                panel::PANEL_BG,
            );
        }
    }

    /// The subordinate hex backdrop: filled terrain sized to whatever `area`
    /// this frame's layout hands it (never a fixed extent, so it fills a
    /// desktop panel instead of sitting centred in a sea of black), a white
    /// dotted line network linking settlements with its own traveling
    /// pulses, and two kingdom pennants.
    fn draw_map(surface: &mut Surface<'_>, area: Rect, time: f32) {
        let inner = Panel::new().title("The Contested Vale").draw(surface, area);
        if inner.width() < 6 || inner.height() < 4 {
            return;
        }

        draw_hex_terrain(surface, inner);

        let points: [(u16, u16); SETTLEMENTS.len()] =
            core::array::from_fn(|i| settlement_screen_pos(&SETTLEMENTS[i], inner));

        // Node network: a simple chain through every settlement in listed
        // order, which is enough to read as "these places connect" without
        // needing a real routing pass -- the map is backdrop, not a puzzle.
        for (i, pair) in points.windows(2).enumerate() {
            draw_line(
                surface,
                inner,
                pair[0],
                pair[1],
                '\u{00b7}',
                rgb(150, 152, 160),
            );
            // A bright pulse travels each edge on its own phase (staggered
            // by segment index) so the network reads as live traffic rather
            // than a static route -- the one piece of this map that is
            // allowed to be the brightest thing in the panel, since it is
            // motion, not a fixed mark, competing for the eye.
            draw_edge_pulse(surface, inner, pair[0], pair[1], time, i as f32 * 0.37);
        }

        for (settlement, &(sx, sy)) in SETTLEMENTS.iter().zip(points.iter()) {
            if let Some(k) = settlement.kingdom {
                draw_banner(surface, inner, (sx, sy), KINGDOMS[k].color, time, k as f32);
                // The banner carries the kingdom's own (short) name, kept
                // separate from the settlement's name so two nearby capitals
                // never collide on a narrow map -- see the `Kingdom` doc.
                draw_label(
                    surface,
                    inner,
                    (sx, sy.saturating_sub(5)),
                    KINGDOMS[k].name,
                    KINGDOMS[k].color,
                );
            }
            if inner.contains(sx, sy) {
                // `\u{25cb}` (white circle) was the original node mark, but
                // at this glyph size it shares its outline with the digit
                // `0` closely enough that a settlement node reads as a
                // stray zero rather than a place. A filled square has no
                // digit it can be confused with and still reads as a
                // discrete point on the network.
                surface.put(
                    (sx, sy),
                    '\u{25a0}',
                    Style::new().fg(rgb(226, 224, 236)).bg(rgb(14, 15, 22)),
                );
            }
        }

        // Settlement names are never printed at their own node: seven of
        // them packed into a panel this narrow sit only a few columns apart,
        // and a per-node label at that density guarantees two names collide.
        // A single legend line along the bottom, built from whole names
        // only, gives every name shown a place to appear without ever
        // clipping or overlapping one.
        draw_settlement_legend(surface, inner);
    }

    /// Bottom-of-screen primary action: End Turn, sized and placed in the
    /// thumb zone per the gallery's mobile-first rules. Also reports the
    /// countdown to the next automatic turn, which is what makes an
    /// unattended viewer's screen keep changing even with no input at all.
    fn draw_controls(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < TAP_W || area.height() < TAP_H {
            return;
        }
        panel::band(surface, area);
        let button = Rect::new(
            area.left(),
            area.top(),
            TAP_W.max(area.width() / 4),
            area.height(),
        );
        self.hotspots.push_tappable(button, area, Action::EndTurn);
        surface.fill_rect(button, ' ', Style::new().bg(rgb(46, 34, 18)));
        let label = "[ End Turn ]";
        let ly = button.top() + button.height() / 2;
        surface.print(
            (button.left() + 1, ly),
            label,
            Style::new().fg(ui::ACCENT).bg(rgb(46, 34, 18)),
        );

        let remaining = (TURN_PERIOD - self.turn_clock).max(0.0);
        let status = match self.state {
            GameState::Playing => {
                format!("Turn {}   next turn in {:.0}s", self.turn, remaining.ceil())
            }
            GameState::Victorious => "The seals are broken. Victory.".to_string(),
            GameState::Exposed => "World Panic reached 100%. Exposed.".to_string(),
        };
        let sx = button.right() + 2;
        if sx < area.right() {
            surface.print(
                (sx, ly),
                &status,
                Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
            );
        }
    }

    fn status(&self) -> String {
        format!(
            "turn {}   active: {}",
            self.turn, CHALLENGES[self.active].name
        )
    }
}

/// [`panel::bar`] plus a traveling highlight riding across its filled
/// portion, so a meter whose value has not changed this frame is still
/// visibly alive rather than a static picture repainted every tick.
///
/// The highlight is a pure function of `time`; it never touches `t`, which
/// stays exactly the value the caller passed. A gauge that shimmered its own
/// fill fraction would be lying about the number it is showing, and that
/// number is the one thing in this demo that is never allowed to drift.
#[derive(Clone, Copy)]
struct Shimmer {
    fill: Color,
    track: Color,
    time: f32,
    phase: f32,
}

fn shimmer_bar(surface: &mut Surface<'_>, at: (u16, u16), width: u16, t: f32, shimmer: Shimmer) {
    let Shimmer {
        fill,
        track,
        time,
        phase,
    } = shimmer;
    panel::bar(surface, at, width, t, fill, track);
    let filled = (f32::from(width) * t.clamp(0.0, 1.0)).round() as u16;
    if filled == 0 {
        return;
    }
    let (x0, y) = at;
    let sweep = pulse01(time, 2.6, phase) * f32::from(filled.saturating_sub(1));
    for i in 0..filled {
        let dist = (f32::from(i) - sweep).abs();
        if dist < 1.6 {
            let glow = 1.0 - dist / 1.6;
            let bright = scale(fill, glow.mul_add(0.8, 1.0));
            surface.put((x0 + i, y), '\u{2588}', Style::new().fg(bright).bg(track));
        }
    }
}

/// Nine seal pips, one per [`SEALS_TOTAL`], filled for every seal already
/// broken. The one about to break breathes, so the seals panel reads as live
/// between breaks instead of only changing once every `seal_cadence` turns,
/// and the row itself uses the width the panel was otherwise leaving empty
/// next to its single line of text.
fn draw_seal_pips(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    seals_broken: u32,
    time: f32,
) {
    let (x0, y) = at;
    let shown = usize::from(width / 2).min(SEALS_TOTAL as usize);
    for i in 0..shown {
        let x = x0 + i as u16 * 2;
        let idx = i as u32;
        // `\u{25cb}` (white circle) used to stand for every pip that was not
        // yet broken, filled or otherwise. At this glyph size its bitmap is
        // an unadorned oval that this tileset renders almost identically to
        // the digit `0`, so a row of nine unbroken seals read as "0 0 0 0 0
        // 0 0 0 0" next to a counter that already says "0 of 9" -- the
        // panel's headline feature looked like a printing error. `\u{25a0}`
        // (a solid square) and `\u{2022}` (a small bullet) are unrelated to
        // any digit's shape at any size, so a broken/unbroken pip pair built
        // from them cannot be misread as a number.
        let (glyph, color) = match idx.cmp(&seals_broken) {
            core::cmp::Ordering::Less => ('\u{25a0}', ui::ACCENT),
            core::cmp::Ordering::Equal => {
                let glow = pulse01(time, 1.6, 0.0);
                (
                    '\u{2022}',
                    scale(rgb(200, 176, 110), glow.mul_add(0.7, 0.5)),
                )
            }
            core::cmp::Ordering::Greater => ('\u{2022}', ui::DIM),
        };
        surface.put((x, y), glyph, Style::new().fg(color).bg(panel::PANEL_BG));
    }
}

/// A one-row heat strip of recent `world_panic` samples, oldest at the left,
/// filling the World panel's otherwise-empty second half with the trend the
/// single current percentage cannot show on its own. Glyph density and color
/// both encode the sampled value, so the strip still reads correctly in a
/// monochrome terminal that drops color.
fn draw_panic_strip(surface: &mut Surface<'_>, at: (u16, u16), width: u16, history: &[f32]) {
    if history.is_empty() || width == 0 {
        return;
    }
    let (x0, y) = at;
    let last = history.len() - 1;
    for i in 0..width {
        let frac = if width <= 1 {
            1.0
        } else {
            f32::from(i) / f32::from(width - 1)
        };
        let idx = (frac * last as f32).round() as usize;
        let v = history[idx.min(last)] / 100.0;
        let glyph = if v < 0.25 {
            '\u{2591}'
        } else if v < 0.5 {
            '\u{2592}'
        } else if v < 0.75 {
            '\u{2593}'
        } else {
            '\u{2588}'
        };
        // `panel::threshold` only names three fixed colors (green, amber,
        // red), so without this, every sample under 40% panic -- whether it
        // is 1% or 39% -- painted in the exact same full-brightness green.
        // A run that has sat near 1% panic the whole game then filled this
        // entire strip with solid bright green immediately under the actual
        // panic bar, and a bar and a strip that are both "full-brightness
        // green, full width" read as one continuous nearly-full meter even
        // though the real bar above is a single filled cell. Blending the
        // hue toward the strip's own background by how small `v` actually
        // is keeps the glyph-density encoding intact (still the primary
        // signal in a monochrome terminal) while making a strip of
        // near-zero samples fade toward invisible instead of glowing at
        // full strength.
        let hue = panel::threshold(1.0 - v);
        let strength = (v / 0.35).clamp(0.12, 1.0);
        surface.put(
            (x0 + i, y),
            glyph,
            Style::new()
                .fg(mix(rgb(20, 20, 26), hue, strength))
                .bg(rgb(20, 20, 26)),
        );
    }
}

/// Terrain fill, glyph, and glyph color for the hex `tile`, hashed from its
/// own coordinates (never from any per-frame clock, and never from screen
/// position, so the terrain does not reshuffle as the panel resizes). Four
/// biomes drawn from [`tilekit::glyphs::terrain`]'s own CP437 vocabulary --
/// water, forest, hills, marsh -- rather than the three near-identical dim
/// shades the first draft used, which read as noise because nothing about
/// them said *what* was on the ground.
fn hex_terrain(tile: Tile) -> (Color, Color, char) {
    let h = hash01(0x5eed, tile.col, tile.row);
    if h < 0.2 {
        (rgb(16, 32, 50), rgb(96, 138, 168), terrain::WATER)
    } else if h < 0.55 {
        (rgb(16, 32, 20), rgb(72, 122, 80), terrain::FOREST)
    } else if h < 0.8 {
        (rgb(30, 33, 19), rgb(118, 126, 74), terrain::HILLS)
    } else {
        (rgb(24, 30, 26), rgb(92, 112, 92), terrain::MARSH)
    }
}

/// A tessellated, filled hex backdrop sized to whatever `area` this frame's
/// layout hands it (never a fixed extent, so it fills a desktop panel
/// instead of sitting centred in a sea of black).
///
/// Every screen cell asks [`HexLayout::cell_to_tile`] which hex owns it and
/// paints that hex's fill, the same per-cell-ownership technique
/// [`19_hex_command`](../19_hex_command)'s `draw_hex_field` uses to get a
/// gapless tessellation without an edge-tapering formula to get wrong. The
/// seam between two hexes (the cell north or west of a given cell belonging
/// to a different tile) gets a one-shade-darker fill rather than a drawn
/// line, and one terrain glyph is stamped at each hex's own center on a
/// second pass -- together that is what turns the old single-glyph-per-hex
/// scatter into a field of actual, differently-colored tiles that reads as
/// ground rather than as marks floating on black, while staying dim enough
/// to sit under the panels around it.
fn draw_hex_terrain(surface: &mut Surface<'_>, area: Rect) {
    if area.width() == 0 || area.height() == 0 {
        return;
    }
    let layout = HexLayout::new(HexOrientation::Pointy, 8, 4);

    for sy in area.top()..area.bottom() {
        for sx in area.left()..area.right() {
            let wx = i32::from(sx) - i32::from(area.left());
            let wy = i32::from(sy) - i32::from(area.top());
            let cell = Cell::new(wx, wy);
            let tile = layout.cell_to_tile(cell);
            let (fill, _, _) = hex_terrain(tile);
            let north = layout.cell_to_tile(Cell::new(wx, wy - 1));
            let west = layout.cell_to_tile(Cell::new(wx - 1, wy));
            let bg = if north != tile || west != tile {
                scale(fill, 0.7)
            } else {
                fill
            };
            surface.put((sx, sy), ' ', Style::new().bg(bg));
        }
    }

    let cols = area.width() / 8 + 2;
    let rows = area.height() / 4 + 2;
    for row in -1..i32::from(rows) {
        for col in -1..i32::from(cols) {
            let tile = Tile::new(col, row);
            let center = layout.center_cell(tile);
            let sx = i32::from(area.left()) + center.x;
            let sy = i32::from(area.top()) + center.y;
            if sx < i32::from(area.left())
                || sy < i32::from(area.top())
                || sx >= i32::from(area.right())
                || sy >= i32::from(area.bottom())
            {
                continue;
            }
            let (fill, glyph_color, glyph) = hex_terrain(tile);
            surface.put(
                (sx as u16, sy as u16),
                glyph,
                Style::new().fg(glyph_color).bg(fill),
            );
        }
    }
}

/// Greedy word wrap: appends whole words to the current line until the next
/// one would not fit, then starts a new line, the same append-while-it-fits
/// discipline [`draw_settlement_legend`] uses for names. `width` of zero
/// yields no lines rather than looping forever on a word that can never fit.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate_len = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > width && !line.is_empty() {
            lines.push(core::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A settlement's fractional position mapped onto `inner`. Reserves six rows
/// at the top -- one for a kingdom's name label, four for its pole-and-flag
/// pennant, one of clearance -- and one at the bottom for
/// [`draw_settlement_legend`], so a node is never placed somewhere its own
/// pennant or the legend line would immediately overwrite it.
fn settlement_screen_pos(settlement: &Settlement, inner: Rect) -> (u16, u16) {
    let w = f32::from(inner.width().saturating_sub(2)).max(1.0);
    let h = f32::from(inner.height().saturating_sub(8)).max(1.0);
    let x = inner.left() + 1 + (settlement.frac_x * w) as u16;
    let y = inner.top() + 6 + (settlement.frac_y * h) as u16;
    (
        x.min(inner.right().saturating_sub(1)),
        y.min(inner.bottom().saturating_sub(2)),
    )
}

/// Prints `text` at `at` inside `area`, whole or not at all: a label
/// truncated mid-word (or one that has fallen outside the panel entirely)
/// reads as a bug, so this checks the room on both axes first rather than
/// letting [`panel::spans`] clip it silently.
fn draw_label(surface: &mut Surface<'_>, area: Rect, at: (u16, u16), text: &str, color: Color) {
    let (x, y) = at;
    if y < area.top() || y >= area.bottom() || x >= area.right() {
        return;
    }
    let room = usize::from(area.right() - x);
    if room < text.chars().count() {
        return;
    }
    panel::spans(
        surface,
        (x, y),
        room as u16,
        &[Span::new(text, color)],
        rgb(10, 11, 16),
    );
}

/// A one-line legend along the bottom of the map panel, listing whole
/// settlement names separated by a mid-dot until the next one would not fit.
/// Built this way (append-while-it-fits, never truncate-what-was-appended)
/// rather than truncating the whole joined string, so a narrow panel simply
/// shows fewer names instead of half of one.
fn draw_settlement_legend(surface: &mut Surface<'_>, area: Rect) {
    if area.height() == 0 {
        return;
    }
    let y = area.bottom() - 1;
    let room = usize::from(area.width());
    let mut line = String::new();
    for settlement in &SETTLEMENTS {
        let sep = if line.is_empty() { "" } else { " \u{00b7} " };
        let candidate_len =
            line.chars().count() + sep.chars().count() + settlement.name.chars().count();
        if candidate_len > room {
            break;
        }
        line.push_str(sep);
        line.push_str(settlement.name);
    }
    if !line.is_empty() {
        panel::spans(
            surface,
            (area.left(), y),
            area.width(),
            &[Span::dim(&line)],
            rgb(10, 11, 16),
        );
    }
}

/// A pole and a flapping flag hanging above a settlement, the ASCII
/// equivalent of the source game's kingdom banners planted over their
/// territory -- four rows tall so it reads as a pennant rather than the
/// two-cell colored blob the first draft drew. Skipped rather than clamped
/// if there is no room above the node or to its right, since a banner
/// drawn on top of its own settlement (or clipped mid-flag) would read as a
/// mistake rather than as a flag.
///
/// The flag's width alternates between two and three cells on a `time`-
/// driven cycle offset by `phase`, the CP437-safe stand-in for cloth moving
/// in wind: real motion (defect: static banners), without inventing a glyph
/// outside the code page.
fn draw_banner(
    surface: &mut Surface<'_>,
    area: Rect,
    at: (u16, u16),
    color: Color,
    time: f32,
    phase: f32,
) {
    let (x, y) = at;
    if y < area.top() + 4 || x + 3 > area.right() {
        return;
    }
    let bg = rgb(10, 11, 16);
    for i in 1..=4 {
        surface.put(
            (x, y - i),
            '\u{2502}',
            Style::new().fg(scale(color, 0.55)).bg(bg),
        );
    }
    let flap = pulse01(time, 1.1, phase);
    let width: u16 = if flap > 0.5 { 3 } else { 2 };
    for i in 0..width {
        surface.put(
            (x + 1 + i, y - 4),
            '\u{2588}',
            Style::new().fg(color).bg(bg),
        );
    }
    for i in 0..width.saturating_sub(1) {
        surface.put(
            (x + 1 + i, y - 3),
            '\u{2588}',
            Style::new().fg(color).bg(bg),
        );
    }
    surface.put((x + 1, y - 2), '\u{2588}', Style::new().fg(color).bg(bg));
}

/// A single bright glyph traveling from `from` to `to` and back on a
/// `time`-driven cycle offset by `phase`, drawn after [`draw_line`] so it
/// sits above the faint dotted network rather than in it: the moving dot
/// is the network's live-traffic overlay, not another static mark.
fn draw_edge_pulse(
    surface: &mut Surface<'_>,
    area: Rect,
    from: (u16, u16),
    to: (u16, u16),
    time: f32,
    phase: f32,
) {
    let t = pulse01(time, 3.1, phase);
    let x = (f32::from(to.0) - f32::from(from.0))
        .mul_add(t, f32::from(from.0))
        .round();
    let y = (f32::from(to.1) - f32::from(from.1))
        .mul_add(t, f32::from(from.1))
        .round();
    if x < 0.0 || y < 0.0 {
        return;
    }
    let (x, y) = (x as u16, y as u16);
    if area.contains(x, y) {
        surface.put(
            (x, y),
            '\u{2022}',
            Style::new().fg(rgb(255, 244, 210)).bg(rgb(10, 11, 16)),
        );
    }
}

/// A Bresenham line between two points already in `area`'s screen space,
/// clipped to `area` and drawn with a single glyph faded toward the map
/// background -- the "white node network" overlay, kept faint enough to
/// read as connective tissue rather than as the loudest thing in the panel.
fn draw_line(
    surface: &mut Surface<'_>,
    area: Rect,
    from: (u16, u16),
    to: (u16, u16),
    glyph: char,
    color: Color,
) {
    let (mut x0, mut y0) = (i32::from(from.0), i32::from(from.1));
    let (x1, y1) = (i32::from(to.0), i32::from(to.1));
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    // Faded toward black, but only a third of the way: enough that the
    // network reads as quieter than the accent-colored panels around it
    // (this map is meant to be background), while still being visibly
    // there -- fully at `color` would compete with the settlement markers,
    // and the 0.8 fade tried earlier turned out indistinguishable from the
    // terrain it was drawn over.
    let faded = mix(color, rgb(10, 11, 16), 0.3);

    loop {
        if area.contains(x0 as u16, y0 as u16) && (x0, y0) != (x1, y1) {
            surface.put(
                (x0 as u16, y0 as u16),
                glyph,
                Style::new().fg(faded).bg(rgb(10, 11, 16)),
            );
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

impl Demo for VeiledHand {
    const NAME: &'static str = "50_veiled_hand";
    const TITLE: &'static str = "Veiled Hand";
    const BLURB: &'static str =
        "Forbidden Gods: a hidden villain's challenge tracker against two doom meters.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-5", "select challenge"),
            ("Space/Enter", "end turn"),
            ("tap", "select / end turn"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.handle_tap();

        if self.state == GameState::Playing {
            self.turn_clock += dt;
            // Counting whole turns with a division rather than looping on a
            // float comparison (`while self.turn_clock >= TURN_PERIOD`) is
            // both what `clippy::while_float` asks for and the safer choice
            // for a stalled backend: a huge single `dt` produces a bounded
            // integer count instead of an unbounded float-subtraction loop.
            let due_turns = (self.turn_clock / TURN_PERIOD).floor();
            if due_turns >= 1.0 {
                self.turn_clock = due_turns.mul_add(-TURN_PERIOD, self.turn_clock);
                for _ in 0..due_turns as u32 {
                    self.advance_turn();
                    if self.state != GameState::Playing {
                        self.turn_clock = 0.0;
                        break;
                    }
                }
            }
        }

        self.hotspots.clear();

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        self.layout_and_draw(&mut surface, content, shape);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

impl VeiledHand {
    /// Splits `content` per [`Shape`] and draws every panel into it.
    ///
    /// Portrait stacks everything top to bottom because rows are cheap there
    /// and columns are not; landscape and desktop both use the three-column
    /// reading the source screenshot uses (agent left, map centre, challenges
    /// right), differing only in how much height each gets to work with.
    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect, shape: Shape) {
        const DOOM_H: u16 = 6;
        const PROGRESS_H: u16 = 6;
        const CONTROLS_H: u16 = 4;
        const AGENT_H: u16 = 9;

        let (doom_area, rest) = panel::split_top(content, DOOM_H.min(content.height()));
        self.draw_doom(surface, doom_area);

        let (rest, controls_area) = panel::split_bottom(rest, CONTROLS_H.min(rest.height()));

        if shape.stacks() {
            let (progress_area, rest) = panel::split_top(rest, PROGRESS_H.min(rest.height()));
            self.draw_progress(surface, progress_area);

            let (agent_area, rest) = panel::split_top(rest, AGENT_H.min(rest.height()));
            self.draw_agent(surface, agent_area);

            let map_h = rest.height() * 3 / 5;
            let (map_area, list_area) = panel::split_top(rest, map_h);
            Self::draw_map(surface, map_area, self.time);
            self.draw_challenges(surface, list_area);
        } else {
            const AGENT_W: u16 = 24;
            const LIST_W: u16 = 34;
            let (agent_area, rest) = panel::split_left(rest, AGENT_W.min(rest.width() / 3));
            let (centre, list_area) = panel::split_right(rest, LIST_W.min(rest.width() / 3));

            self.draw_agent(surface, agent_area);
            self.draw_challenges(surface, list_area);

            let (progress_area, map_area) =
                panel::split_top(centre, PROGRESS_H.min(centre.height()));
            self.draw_progress(surface, progress_area);
            Self::draw_map(surface, map_area, self.time);
        }

        self.draw_controls(surface, controls_area);
    }
}

ascii_tile_demos::demo_main!(VeiledHand);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{CHALLENGES, SETTLEMENTS};

    #[test]
    fn settlement_names_are_unique() {
        let names: HashSet<_> = SETTLEMENTS.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), SETTLEMENTS.len(), "duplicate settlement name");
    }

    #[test]
    fn challenge_names_are_unique() {
        let names: HashSet<_> = CHALLENGES.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), CHALLENGES.len(), "duplicate challenge name");
    }

    #[test]
    fn seal_cadence_shrinks_and_never_hits_zero() {
        let mut prev = u32::MAX;
        for seals in 0..=9 {
            let c = super::seal_cadence(seals);
            assert!(c > 0);
            assert!(c <= prev);
            prev = c;
        }
    }
}
