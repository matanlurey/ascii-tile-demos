//! 34: ICE breach -- a cyberpunk intrusion run across three vertical lanes,
//! played against a meter that never stops rising.
//!
//! Android: Netrunner spends a whole card game teaching one idea: ICE comes in
//! typed flavors, and the only programs that beat it are the ones built for
//! that type. Monster Train spends its whole board on the other idea: a lane
//! is a *column*, climbed from one end toward the other, and every lane runs
//! at once. This demo fuses them into something that runs itself -- a TRACE
//! meter across the top that climbs on its own clock, so standing still is
//! itself a decision with a cost, not just a lack of one.
//!
//! Three servers sit at the top of three lanes. Between each server and the
//! runner at the bottom sits a stack of ICE: a type, a strength, and one line
//! of subroutine text. Breaking the frontmost ICE takes a hand card of the
//! matching type at or above its strength; a mismatch or a shortfall does
//! nothing to the ICE and jolts the trace instead. Clear a lane's ICE and
//! running past the top breaches the server, banking data into the run's
//! unbanked take -- which stays unbanked, and therefore losable, until you
//! jack out.
//!
//! Techniques on show:
//!
//! - **Vertical lanes as the touch target** ([`IceBreach::layout_lanes`],
//!   [`Action::SelectLane`]): a lane's hotspot spans the full content height,
//!   not just the ICE currently in play. On a portrait phone that is the
//!   single most forgiving tap target on the whole screen -- narrower than a
//!   card takes on the horizontal axis but taller than anything else the
//!   layout draws, so a thumb that lands a few rows off still lands inside
//!   the lane it was aimed at. Compare this to registering a hotspot only on
//!   the current ICE cell: that shrinks the moment the stack does, exactly
//!   when the player is most likely to be tapping in a hurry.
//! - **Typed matchups instead of tooltips** ([`IceKind`], [`ProgramKind`],
//!   [`IceBreach::play_selected`]): both an ICE and the icebreaker that beats it
//!   carry the same three-way type (Barrier, Sentry, Code Gate) and the same
//!   accent color for that type. A player never has to open a rules panel to
//!   know whether a card can break a given ICE -- the color and the printed
//!   word on both cards already say so, which is the whole reason Netrunner's
//!   type system reads as fair even to someone who has never played it.
//! - **A trace meter as a visible clock** ([`IceBreach::simulate`]): most
//!   deckbuilders are turn-based, which means time pressure has to be
//!   announced ("3 turns left") rather than seen. Driving the meter off
//!   `frame.delta` instead turns it into something that visibly creeps while
//!   you are still reading a card, which is what makes a static screenshot of
//!   this demo read as mid-emergency rather than as a paused board.
//! - **Multi-cell ICE and server nodes** ([`IceBreach::draw_lane`]): each ICE is
//!   drawn as a bordered block carrying its type, strength, and subroutine
//!   text on separate rows, the same "one glyph is never one interactive
//!   unit" rule the card hand follows. A lane that is short on rows drops the
//!   ICE furthest from the runner first (see [`IceBreach::visible_ice_count`]),
//!   because the ICE you are about to fight matters more than the ICE you
//!   have not reached yet.
//! - **The hand** ([`ui::card`]): icebreakers carry a type and a strength
//!   exactly like the ICE they are meant to beat; a virus and a bypass round
//!   out the hand as untyped utilities that trade a card for time instead of
//!   for a broken ICE.
//! - **Tap-select-then-tap, plus drag** ([`ascii_tile_demos::ui::touch`]):
//!   tapping a card selects it, tapping the lane's frontier cell plays it.
//!   Dragging a card and dropping it on the frontier does the same in one
//!   gesture, for the desktop mouse users who would rather drag than click
//!   twice. Both paths call the same [`IceBreach::play_selected`].
//! - **A confirm gate on the only irreversible tap** ([`IceBreach::pending`]):
//!   jacking out is always safe, so it fires immediately. Running past a
//!   still-live ICE fires its subroutines for real damage, so the first RUN
//!   arms it and a second RUN (or a second Enter) is what actually pulls the
//!   trigger -- the same one-more-tap pattern Into the Breach uses for its
//!   own irreversible moves.
//! - **Deterministic vertical data flow** ([`IceBreach::draw_data_flow`]): each
//!   lane's background packets are placed by [`tilekit::noise::hash01`] and
//!   animated by adding elapsed time to a per-column phase, so the flow is
//!   stable frame to frame and never depends on wall-clock time or iteration
//!   order -- required for the snapshot tests, and also just correct: a
//!   packet's *lane* should not reshuffle every frame even though its
//!   *position* should visibly slide.
//!
//! ```sh
//! cargo run --example 34_ice_breach --features crossterm
//! cargo run --example 34_ice_breach --features software
//! cargo run --example 34_ice_breach --features gl
//! cargo run --example 34_ice_breach  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{self, Card, CardState};
use ascii_tile_demos::ui::panel::{self, Log, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape, TAP_H, tappable};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, hash01};
use tilekit::palette::{mix, rgb, scale};

/// Lanes running side by side. Netrunner itself plays with as many servers as
/// a corp can afford; three is the smallest number that still reads as "a
/// choice between lanes" rather than "the one lane there is", and the largest
/// that still leaves each lane wide enough for a full ICE block on a portrait
/// phone (see [`IceBreach::layout_lanes`]).
const LANE_COUNT: usize = 3;

/// ICE stacked in each freshly generated lane, nearest the runner first.
///
/// Three is enough to force at least one type change per climb (so the
/// matchup reading is actually exercised) without making a single lane a
/// multi-minute grind that would starve the other two of attention while the
/// trace keeps rising regardless of which lane you are looking at.
const ICE_PER_LANE: usize = 3;

/// Cards held at once. Kept below the ten-plus a real deckbuilder carries so
/// the fan in [`ascii_tile_demos::ui::card::fan`] never needs to overlap on
/// any of the three [`Shape`]s this demo has to survive, and small enough
/// that every card gets its own keyboard digit alongside the three lane keys
/// (4-7, after 1-3 for lanes) without the two ranges colliding.
const HAND_SIZE: usize = 4;

/// Rows one ICE block occupies: a bordered frame (2 rows) around a
/// type-and-strength line and a subroutine line. This is the floor below
/// which an ICE stops being legible as "a thing with a rule", not a number
/// chosen for taste -- drop either content row and the demo has degraded back
/// into the single-glyph tiles the touch module argues against.
const ICE_H: u16 = 4;

/// Rows the server block occupies at the top of a lane: frame, name, then a
/// combined data/integrity line.
const SERVER_H: u16 = 4;

/// How fast the trace climbs on its own, in points per second out of 100.
/// Tuned so an idle lane (nobody playing cards, nobody jacking out) fills the
/// meter in about 45 seconds -- long enough to read every ICE in a lane once,
/// short enough that leaving the run running while you think is a real cost
/// and not just flavor text.
const TRACE_RATE: f32 = 100.0 / 45.0;

/// Trace jump for a mismatched-type or underpowered break attempt on the
/// correct (frontmost) ICE. Small: a misjudged matchup is a misplay, not a
/// catastrophe, and the hand card is already spent on it.
const TRACE_FAIL_JUMP: f32 = 6.0;

/// Trace jump for forcing a run past a still-live ICE (the confirmed RUN).
/// Large enough that it is a genuine last resort, not a routine way to skip a
/// bad matchup -- forcing three ICE in a row very nearly maxes the meter by
/// itself.
const TRACE_FORCE_JUMP: f32 = 28.0;

/// How long a played virus halves the trace's climb rate, in seconds.
const VIRUS_SLOW_SECS: f32 = 6.0;

/// Trace value at which the run is forcibly dropped and the unbanked take is
/// lost.
const TRACE_MAX: f32 = 100.0;

/// A type of ICE, and of the icebreaker that beats it.
///
/// Netrunner's own three (Barrier / Sentry / Code Gate, beaten by Fracter /
/// Killer / Decoder) are kept by name because the type triangle is the whole
/// point on show here: a player who has never seen this demo before still has
/// to be able to look at an ICE and a hand of cards and know which one goes
/// where, with no tooltip. Reusing the reference game's own vocabulary means
/// a chunk of the audience already has that reading built in, and everyone
/// else gets it from the accent color and the printed word alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IceKind {
    Barrier,
    Sentry,
    CodeGate,
}

impl IceKind {
    const ALL: [Self; 3] = [Self::Barrier, Self::Sentry, Self::CodeGate];

    const fn label(self) -> &'static str {
        match self {
            Self::Barrier => "BARRIER",
            Self::Sentry => "SENTRY",
            Self::CodeGate => "CODE GATE",
        }
    }

    const fn breaker_label(self) -> &'static str {
        match self {
            Self::Barrier => "Fracter",
            Self::Sentry => "Killer",
            Self::CodeGate => "Decoder",
        }
    }

    /// The type's neon: magenta / acid green / cyan, the three colors the
    /// brief's cyberpunk palette is built from. Carried by both an ICE block
    /// and its matching icebreaker card so the matchup is legible by color
    /// alone before a single word is read.
    const fn color(self) -> Color {
        match self {
            Self::Barrier => rgb(232, 64, 200),  // neon magenta
            Self::Sentry => rgb(96, 230, 140),   // acid green
            Self::CodeGate => rgb(72, 210, 232), // neon cyan
        }
    }

    const fn subroutine(self) -> &'static str {
        match self {
            Self::Barrier => "End the run.",
            Self::Sentry => "Trace 3, tag.",
            Self::CodeGate => "Force a redirect.",
        }
    }
}

/// One ICE program on a lane.
#[derive(Clone)]
struct Ice {
    kind: IceKind,
    strength: u32,
    broken: bool,
    /// Set when cleared by a Bypass rather than an actual break, purely for
    /// the log line -- bypassing is not a matchup win and should not read as
    /// one.
    bypassed: bool,
}

/// The server at the top of a lane: what breaching it is worth and how much
/// punishment it can absorb from a forced run before the lane is considered
/// blown wide open (cosmetic; a forced run always costs trace regardless).
struct Server {
    name: &'static str,
    data: u32,
    integrity: u32,
    max_integrity: u32,
}

/// One vertical lane: a server, its ICE stack (index 0 nearest the runner),
/// and the runner's current position in that stack.
struct Lane {
    server: Server,
    ice: Vec<Ice>,
    /// Index of the next unbroken ICE the runner must deal with. Equal to
    /// `ice.len()` once every ICE is cleared, meaning the frontier is the
    /// server itself.
    runner_at: usize,
}

impl Lane {
    const fn cleared(&self) -> bool {
        self.runner_at >= ICE_PER_LANE
    }
}

/// Generates a fresh lane: a named server and a stack of ICE whose strengths
/// climb toward the top, so the ICE nearest the server is always at least as
/// tough as the one nearest the runner. Real intrusion targets are built the
/// same way -- the outermost layer is the cheap deterrent, the innermost is
/// the one that actually stops you -- and it gives a climb a shape instead of
/// a flat sequence of interchangeable fights.
fn generate_lane(seed: u32) -> Lane {
    const SERVER_NAMES: [&str; 6] = [
        "ARASAKA-CORE",
        "NIGHT-VAULT",
        "OBELISK-NET",
        "GLASSWING",
        "HYDRA-9",
        "COLDSTAR",
    ];
    let mut rng = Rng::new(seed);
    // Indexed rather than `Rng::choose`: `choose` would hand back a
    // reference into this function's local `const` array, and a plain copy
    // out of a `Copy` element sidesteps that lifetime entirely.
    let name = SERVER_NAMES[rng.next_below(SERVER_NAMES.len() as u32) as usize];
    let data = 4 + rng.next_below(9);
    let integrity = 3 + rng.next_below(4);

    let mut ice = Vec::with_capacity(ICE_PER_LANE);
    for i in 0..ICE_PER_LANE {
        let kind = IceKind::ALL[rng.next_below(IceKind::ALL.len() as u32) as usize];
        // Strength climbs with distance from the runner (`i` here, since
        // index 0 is nearest), plus a little noise so two lanes generated
        // back to back do not read as identical ladders.
        let strength = 2 + i as u32 + rng.next_below(3);
        ice.push(Ice {
            kind,
            strength,
            broken: false,
            bypassed: false,
        });
    }

    Lane {
        server: Server {
            name,
            data,
            integrity,
            max_integrity: integrity,
        },
        ice,
        runner_at: 0,
    }
}

/// A kind of hand card.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgramKind {
    /// Breaks ICE of the matching [`IceKind`] whose strength it meets.
    Icebreaker(IceKind, u32),
    /// Untargeted: halves the trace's climb rate for [`VIRUS_SLOW_SECS`] the
    /// instant it is played.
    Virus,
    /// Targeted at an ICE like an icebreaker, but clears it unconditionally
    /// (type and strength do not matter) rather than winning a matchup.
    Bypass,
}

/// One card in the hand.
#[derive(Clone, Copy)]
struct Program {
    kind: ProgramKind,
    name: &'static str,
}

impl Program {
    const fn accent(self) -> Color {
        match self.kind {
            ProgramKind::Icebreaker(kind, _) => kind.color(),
            ProgramKind::Virus => rgb(180, 120, 240),
            ProgramKind::Bypass => rgb(230, 200, 90),
        }
    }

    const fn kind_label(self) -> &'static str {
        match self.kind {
            ProgramKind::Icebreaker(kind, _) => kind.breaker_label(),
            ProgramKind::Virus => "Virus",
            ProgramKind::Bypass => "Utility",
        }
    }

    fn cost_label(self) -> String {
        match self.kind {
            ProgramKind::Icebreaker(_, strength) => format!("{strength}"),
            ProgramKind::Virus | ProgramKind::Bypass => "-".to_string(),
        }
    }

    const fn body(self) -> &'static str {
        match self.kind {
            ProgramKind::Icebreaker(kind, _) => match kind {
                IceKind::Barrier => "Break Barrier subs.",
                IceKind::Sentry => "Break Sentry subs.",
                IceKind::CodeGate => "Break Code Gate subs.",
            },
            ProgramKind::Virus => "Slows trace 6s.",
            ProgramKind::Bypass => "Skips one ICE.",
        }
    }
}

/// Draws one deterministic program from a fixed rotation, keyed by `draw`.
///
/// A rotation rather than a live RNG draw on every refill: the hand has to be
/// identical across the two renders the determinism test performs, and
/// nothing in this demo advances `draw` except an actual card being played,
/// which never happens with no injected input. Reading off a hashed rotation
/// still gives an unpredictable-looking sequence (see [`hash01`]) without a
/// mutable generator that would need to be threaded through every draw site.
fn draw_program(draw: u32) -> Program {
    const ICEBREAKERS: [(IceKind, &str, u32); 6] = [
        (IceKind::Barrier, "Crowbar", 3),
        (IceKind::Barrier, "Battering Ram", 5),
        (IceKind::Sentry, "Switchblade", 2),
        (IceKind::Sentry, "Mongoose", 4),
        (IceKind::CodeGate, "Cipher", 3),
        (IceKind::CodeGate, "Torch", 5),
    ];
    let roll = hash01(0x1CE0, draw as i32, 0);
    if roll < 0.7 {
        let (kind, name, strength) = ICEBREAKERS[(hash01(0x1CE1, draw as i32, 0)
            * ICEBREAKERS.len() as f32) as usize
            % ICEBREAKERS.len()];
        Program {
            kind: ProgramKind::Icebreaker(kind, strength),
            name,
        }
    } else if roll < 0.85 {
        Program {
            kind: ProgramKind::Virus,
            name: "Coolant",
        }
    } else {
        Program {
            kind: ProgramKind::Bypass,
            name: "Ghost Key",
        }
    }
}

/// A hotspot action, resolved from either a tap or a card-drop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    /// The whole-height lane region. See the module docs: this is the
    /// forgiving fallback target, hit whenever a tap lands in a lane but not
    /// on the smaller frontier block within it.
    SelectLane(usize),
    /// The frontier cell (current ICE, or the server once cleared) of a lane.
    Frontier(usize),
    SelectCard(usize),
    Run,
    JackOut,
}

/// A run confirmation pending a second tap or Enter. See the module docs on
/// why only this one action needs it.
#[derive(Clone, Copy)]
struct PendingRun {
    lane: usize,
}

/// State: three lanes, a hand, the trace meter, and the running/unbanked
/// score.
pub struct IceBreach {
    lanes: Vec<Lane>,
    hand: Vec<Program>,
    /// Next value handed to [`draw_program`]; only ever advances when a card
    /// is actually consumed.
    draw_counter: u32,
    active_lane: Option<usize>,
    selected_card: Option<usize>,
    dragging_card: Option<usize>,
    pending: Option<PendingRun>,
    trace: f32,
    virus_slow_until: f32,
    banked: u32,
    run_take: u32,
    time: f32,
    log: Log,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for IceBreach {
    fn default() -> Self {
        // Distinct seeds per lane so the three servers never generate
        // identically even though `generate_lane` is otherwise deterministic.
        let lanes = (0..LANE_COUNT)
            .map(|i| generate_lane(0x5EED + i as u32 * 0x91))
            .collect();
        let hand = (0..HAND_SIZE as u32).map(draw_program).collect();

        let mut log = Log::new(48);
        log.push("Connection open. Select a lane to begin the run.", ui::DIM);

        Self {
            lanes,
            hand,
            draw_counter: HAND_SIZE as u32,
            active_lane: None,
            selected_card: None,
            dragging_card: None,
            pending: None,
            trace: 0.0,
            virus_slow_until: 0.0,
            banked: 0,
            run_take: 0,
            time: 0.0,
            log,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl IceBreach {
    /// Replaces `self.lanes` with fresh ones and resets the runner in each,
    /// without touching trace, hand, or score -- the pure "the servers moved,
    /// nothing else happened" case used after a breach or after a dropped
    /// connection.
    fn regenerate_lanes(&mut self) {
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            *lane = generate_lane(0x5EED + i as u32 * 0x91 + (self.time * 37.0) as u32);
        }
    }

    /// Replaces one consumed hand slot with the next card in the rotation.
    fn refill(&mut self, slot: usize) {
        self.hand[slot] = draw_program(self.draw_counter);
        self.draw_counter += 1;
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
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c @ '1'..='3') => {
                self.select_lane(c as usize - '1' as usize);
            }
            // Cards live on 4-7 rather than continuing 1-3, so lane selection
            // and card selection never fight over the same key: see
            // `HAND_SIZE`'s doc comment.
            KeyCode::Char(c @ '4'..='7') => {
                let idx = c as usize - '4' as usize;
                if idx < self.hand.len() {
                    self.selected_card = Some(idx);
                }
            }
            KeyCode::Enter => self.trigger_run(),
            KeyCode::Escape => self.jack_out(),
            _ => {}
        }
    }

    const fn select_lane(&mut self, lane: usize) {
        if lane < self.lanes.len() {
            self.active_lane = Some(lane);
            // A pending confirmation only ever means "run past *this* lane's
            // live ICE"; switching lanes without resolving it should not let
            // a stale confirm fire against a lane the player has moved on
            // from.
            self.pending = None;
        }
    }

    /// RUN, from either the button or Enter: advances the active lane if its
    /// frontier is already broken (or the lane is cleared), otherwise arms
    /// the confirm described in the module docs.
    fn trigger_run(&mut self) {
        let Some(lane_idx) = self.active_lane else {
            self.log.push("Select a lane first.", ui::DIM);
            return;
        };
        if let Some(pending) = self.pending
            && pending.lane == lane_idx
        {
            self.force_run(lane_idx);
            self.pending = None;
            return;
        }

        let lane = &mut self.lanes[lane_idx];
        if lane.cleared() {
            self.breach(lane_idx);
            return;
        }
        let live = !lane.ice[lane.runner_at].broken;
        if live {
            self.pending = Some(PendingRun { lane: lane_idx });
            self.log.push(
                "ICE is still live. RUN again to force it (costly).",
                rgb(230, 160, 70),
            );
        } else {
            lane.runner_at += 1;
            if lane.cleared() {
                self.breach(lane_idx);
            } else {
                self.log.push("Advanced past broken ICE.", ui::DIM);
            }
        }
    }

    /// Forces past a still-live ICE: the trace pays for it, the run does not
    /// stop, but the ICE itself is not broken -- forcing is a way past a bad
    /// matchup, not a way to skip paying for one.
    fn force_run(&mut self, lane_idx: usize) {
        self.trace = (self.trace + TRACE_FORCE_JUMP).min(TRACE_MAX);
        let lane = &mut self.lanes[lane_idx];
        lane.runner_at += 1;
        self.log.push(
            format!(
                "Forced past live ICE on lane {}. Subroutines fired!",
                lane_idx + 1
            ),
            rgb(226, 90, 90),
        );
        if lane.cleared() {
            self.breach(lane_idx);
        }
        self.check_trace();
    }

    /// Breaches a cleared server: banks its data into the run's unbanked
    /// take and immediately regenerates that one lane, so a cleared lane
    /// never just sits idle waiting for the player to notice.
    fn breach(&mut self, lane_idx: usize) {
        let data = self.lanes[lane_idx].server.data;
        self.run_take += data;
        self.log.push(
            format!(
                "Breached {} on lane {}: +{data} data (unbanked).",
                self.lanes[lane_idx].server.name,
                lane_idx + 1
            ),
            ui::ACCENT,
        );
        self.lanes[lane_idx] =
            generate_lane(0x5EED + lane_idx as u32 * 0x91 + 1 + (self.time * 53.0) as u32);
    }

    /// The safe action: banks the unbanked take and resets trace and every
    /// lane. Always available, always succeeds, which is the point -- a
    /// player under pressure needs one action that is never a mistake.
    fn jack_out(&mut self) {
        if self.run_take > 0 {
            self.banked += self.run_take;
            self.log
                .push(format!("Jacked out. Banked {}.", self.run_take), ui::ACCENT);
            self.run_take = 0;
        } else {
            self.log.push("Jacked out.", ui::DIM);
        }
        self.trace = 0.0;
        self.pending = None;
        self.regenerate_lanes();
    }

    /// Plays `slot` against the active lane's frontier. Shared by the
    /// tap-select-then-tap path and the drag-and-drop path, so both read
    /// identically in the log and neither can diverge in behavior.
    fn play_selected(&mut self, slot: usize) {
        let Some(lane_idx) = self.active_lane else {
            self.log.push("Select a lane first.", ui::DIM);
            return;
        };
        if slot >= self.hand.len() {
            return;
        }
        let program = self.hand[slot];

        if program.kind == ProgramKind::Virus {
            self.virus_slow_until = self.virus_slow_until.max(self.time) + VIRUS_SLOW_SECS;
            self.log.push(
                format!(
                    "Ran {} -- trace slowed for {VIRUS_SLOW_SECS:.0}s.",
                    program.name
                ),
                program.accent(),
            );
            self.refill(slot);
            self.selected_card = None;
            return;
        }

        let lane = &mut self.lanes[lane_idx];
        let Some(ice) = lane.ice.get_mut(lane.runner_at) else {
            self.log.push("That lane's ICE is already clear.", ui::DIM);
            return;
        };
        if ice.broken {
            self.log.push("That ICE is already broken.", ui::DIM);
            return;
        }

        match program.kind {
            ProgramKind::Bypass => {
                ice.broken = true;
                ice.bypassed = true;
                self.log.push(
                    format!("{} bypassed a {} unbroken.", program.name, ice.kind.label()),
                    program.accent(),
                );
                self.refill(slot);
            }
            ProgramKind::Icebreaker(kind, strength) => {
                if kind == ice.kind && strength >= ice.strength {
                    ice.broken = true;
                    self.log.push(
                        format!(
                            "{} broke the {} (str {}).",
                            program.name,
                            ice.kind.label(),
                            ice.strength
                        ),
                        program.accent(),
                    );
                } else {
                    self.trace = (self.trace + TRACE_FAIL_JUMP).min(TRACE_MAX);
                    let reason = if kind == ice.kind {
                        "not strong enough"
                    } else {
                        "wrong program type"
                    };
                    self.log.push(
                        format!(
                            "{} failed on the {} ({reason}). Trace up.",
                            program.name,
                            ice.kind.label()
                        ),
                        rgb(226, 90, 90),
                    );
                    self.check_trace();
                }
                self.refill(slot);
            }
            ProgramKind::Virus => unreachable!("handled above"),
        }
        self.selected_card = None;
    }

    /// Drops the run and loses the unbanked take once the trace meter fills.
    fn check_trace(&mut self) {
        if self.trace < TRACE_MAX {
            return;
        }
        if self.run_take > 0 {
            self.log.push(
                format!(
                    "TRACE MAXED. Connection dropped, lost {} unbanked.",
                    self.run_take
                ),
                rgb(226, 90, 90),
            );
        } else {
            self.log
                .push("TRACE MAXED. Connection dropped.", rgb(226, 90, 90));
        }
        self.run_take = 0;
        self.trace = 0.0;
        self.pending = None;
        self.regenerate_lanes();
    }

    /// Advances the trace clock and the data-flow animation phase by `dt`
    /// world-seconds. This is the whole reason the demo animates with no
    /// input at all: the meter and the flowing packets both key off
    /// `self.time`, which this is the only place that moves.
    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        let slowed = self.time < self.virus_slow_until;
        let rate = if slowed { TRACE_RATE * 0.5 } else { TRACE_RATE };
        self.trace = (self.trace + rate * dt).min(TRACE_MAX);
        self.check_trace();
    }

    /// How many of a lane's ICE fit in `ice_rows` rows, nearest the runner
    /// first. See [`ICE_H`] and the module docs: the ICE about to be fought
    /// is worth more screen space than the ICE still waiting its turn, so a
    /// squeeze drops from the top (server end) down rather than truncating
    /// arbitrarily.
    const fn visible_ice_count(ice_rows: u16) -> usize {
        let n = (ice_rows / ICE_H) as usize;
        if n > ICE_PER_LANE { ICE_PER_LANE } else { n }
    }

    /// Lays out `count` lanes as equal-width columns across `area`, with a
    /// one-column gutter for the data-flow trail on the left of each.
    fn layout_lanes(area: Rect) -> Vec<Rect> {
        panel::columns(area, LANE_COUNT as u16, 1)
    }

    fn tick_layout(content: Rect) -> (Rect, Rect, Rect, Rect) {
        // Trace bar always gets 3 rows (a title line plus the bar itself
        // plus one row of breathing room); everything below it splits into
        // lanes, hand, and the JACK OUT/RUN control row, in that order of
        // priority, since those two controls are the one thing that must
        // never disappear under a squeeze -- see the module docs on the
        // confirm gate, which only works if the button is reachable.
        let (trace_area, rest) = panel::split_top(content, 3);
        let controls_h = TAP_H.max(4).min(rest.height());
        let (rest, controls_area) = panel::split_bottom(rest, controls_h);

        let hand_h = if Shape::of(content).stacks() {
            (rest.height() * 2 / 5).clamp(5, 9).min(rest.height())
        } else {
            7u16.min(rest.height())
        };
        let (lanes_area, hand_area) = panel::split_bottom(rest, hand_h.min(rest.height()));
        (trace_area, lanes_area, hand_area, controls_area)
    }

    fn draw_trace(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 4 || area.height() == 0 {
            return;
        }
        let t = self.trace / TRACE_MAX;
        // Trace reads as a threat gauge, not a health gauge, so the color
        // ramp is inverted from `panel::threshold`: calm cyan while low,
        // urgent magenta once it is close to ending the run.
        let fill = mix(rgb(72, 210, 232), rgb(232, 64, 200), t);
        let label = format!(
            "TRACE {:>3.0}/{:.0}{}",
            self.trace,
            TRACE_MAX,
            if self.time < self.virus_slow_until {
                "  (slowed)"
            } else {
                ""
            }
        );
        surface.print(
            (area.left(), area.top()),
            &label,
            Style::new().fg(ui::FG).bg(ui::BG),
        );
        if area.height() > 1 {
            panel::bar(
                surface,
                (area.left(), area.top() + 1),
                area.width(),
                t,
                fill,
                rgb(30, 12, 30),
            );
        }
    }

    fn draw_lane(&self, surface: &mut Surface<'_>, area: Rect, lane_idx: usize, active: bool) {
        if area.width() < 6 || area.height() < SERVER_H {
            return;
        }
        let lane = &self.lanes[lane_idx];
        self.draw_data_flow(surface, area);

        let frame = if active { ui::ACCENT } else { rgb(70, 74, 96) };
        let (server_area, rest) = panel::split_top(area, SERVER_H);
        Self::draw_server(surface, server_area, lane, frame);

        let ice_rows = rest.height();
        let visible = Self::visible_ice_count(ice_rows);
        let hidden = ICE_PER_LANE.saturating_sub(visible);

        if hidden > 0 && rest.height() > 0 {
            let text = format!("+{hidden} more ICE above");
            surface.print(
                (rest.left(), rest.top()),
                &text[..text.len().min(rest.width_usize())],
                Style::new().fg(ui::DIM).bg(ui::BG),
            );
        }
        let ice_top = if hidden > 0 {
            rest.top() + 1
        } else {
            rest.top()
        };
        let ice_area = Rect::new(
            rest.left(),
            ice_top,
            rest.width(),
            rest.height().saturating_sub(u16::from(hidden > 0)),
        );

        // Drawn nearest-the-server-first (index descending toward 0) so the
        // frontmost ICE -- the one the runner is actually fighting -- always
        // lands at the bottom of the stack, right above the runner marker.
        for slot in 0..visible {
            let ice_idx = lane.runner_at + (visible - 1 - slot);
            let Some(ice) = lane.ice.get(ice_idx) else {
                continue;
            };
            let y = ice_area.top() + slot as u16 * ICE_H;
            let cell = Rect::new(
                ice_area.left(),
                y,
                ice_area.width(),
                ICE_H.min(ice_area.bottom().saturating_sub(y)),
            );
            let is_frontier = ice_idx == lane.runner_at;
            Self::draw_ice(surface, cell, ice, is_frontier);
        }

        Self::draw_runner(surface, area);
    }

    fn draw_server(surface: &mut Surface<'_>, area: Rect, lane: &Lane, frame: Color) {
        let inner = panel::Panel::new()
            .title(lane.server.name)
            .border(panel::Border::Double)
            .frame(frame)
            .bg(rgb(14, 8, 20))
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let integrity_t = lane.server.integrity as f32 / lane.server.max_integrity.max(1) as f32;
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::keyword(&format!("DATA {}", lane.server.data)),
                Span::plain("  "),
                Span::new(
                    &format!("INT {:.0}%", integrity_t * 100.0),
                    panel::threshold(integrity_t),
                ),
            ],
            rgb(14, 8, 20),
        );
    }

    fn draw_ice(surface: &mut Surface<'_>, area: Rect, ice: &Ice, is_frontier: bool) {
        if area.height() < 2 {
            return;
        }
        let color = ice.kind.color();
        let (frame, bg) = if ice.broken {
            (scale(color, 0.4), rgb(10, 14, 12))
        } else if is_frontier {
            (color, rgb(20, 8, 20))
        } else {
            (scale(color, 0.7), rgb(12, 10, 18))
        };
        let border = if is_frontier {
            panel::Border::Double
        } else {
            panel::Border::Single
        };
        let inner = panel::Panel::new()
            .border(border)
            .frame(frame)
            .bg(bg)
            .focused(is_frontier && !ice.broken)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let status = if ice.broken {
            if ice.bypassed { "BYPASSED" } else { "BROKEN" }
        } else {
            "LIVE"
        };
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::new(ice.kind.label(), color),
                Span::plain(" "),
                Span::dim(&format!("str {}", ice.strength)),
            ],
            bg,
        );
        if inner.height() > 1 {
            let text = if ice.broken {
                status
            } else {
                ice.kind.subroutine()
            };
            surface.print(
                (inner.left(), inner.top() + 1),
                &text[..text.len().min(inner.width_usize())],
                Style::new()
                    .fg(if ice.broken { ui::DIM } else { ui::FG })
                    .bg(bg),
            );
        }
    }

    /// The runner marker: one row pinned to the very bottom of the lane, an
    /// `@` in inverted video. It never moves on screen -- the *ICE* moves
    /// past it as the stack shrinks -- which is the same trick a side-on
    /// runner/endless game uses to keep the player's own token as the one
    /// fixed point in a scrolling world.
    fn draw_runner(surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let y = area.bottom() - 1;
        surface.put(
            (area.left(), y),
            '@',
            Style::new().fg(rgb(10, 10, 12)).bg(ui::ACCENT),
        );
    }

    /// A thin vertical trail of shaded blocks sliding downward, one column
    /// per lane, behind the ICE stack. Positions come from `hash01` keyed on
    /// the column so the *set* of lit cells is stable frame to frame; only
    /// the brightness phase (`self.time` added to a per-cell offset) moves,
    /// which is what makes the lane read as a live wire instead of static.
    fn draw_data_flow(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 {
            return;
        }
        let x = area.right() - 1;
        let left = i32::from(area.left());
        for row in 0..area.height() {
            let y = area.top() + row;
            let irow = i32::from(row);
            if hash01(0x0DA7A, left, irow) > 0.4 {
                continue;
            }
            let phase = hash01(0x0DA7B, left, irow) * core::f32::consts::TAU;
            let speed = 2.2;
            let wave = 0.5f32.mul_add(
                (f32::from(row).mul_add(-0.6, self.time * speed) + phase).sin(),
                0.5,
            );
            let glyph = if wave > 0.75 {
                '\u{2588}'
            } else if wave > 0.5 {
                '\u{2593}'
            } else if wave > 0.25 {
                '\u{2592}'
            } else {
                '\u{2591}'
            };
            let v = 140.0f32.mul_add(wave, 60.0) as u8;
            surface.put(
                (x, y),
                glyph,
                Style::new().fg(rgb(20, v, v.saturating_sub(20))).bg(ui::BG),
            );
        }
    }

    /// The hand's card rects for `area`, in hand order. Pure geometry (no
    /// drawing, no hotspot mutation) so it can be called once to populate
    /// [`Hotspots`] before input is resolved, and again -- with the same
    /// answer, since it depends on nothing but `area` and the hand's length
    /// -- to actually draw after.
    fn hand_rects(&self, area: Rect) -> Vec<Rect> {
        card::fan(area, self.hand.len(), card::FULL_W)
    }

    /// The JACK OUT and RUN button rects for `area`, grown to a legal touch
    /// target. See [`hand_rects`](Self::hand_rects) for why this is a pure
    /// function of `area` alone.
    fn control_rects(area: Rect) -> (Rect, Rect) {
        let cols = panel::columns(area, 2, 1);
        (tappable(cols[0], area), tappable(cols[1], area))
    }

    fn draw_hand(&self, surface: &mut Surface<'_>, rects: &[Rect]) {
        for (i, (program, rect)) in self.hand.iter().zip(rects).enumerate() {
            let selected = self.selected_card == Some(i);
            let state = if selected {
                CardState::Selected
            } else {
                CardState::Idle
            };
            let cost = program.cost_label();
            let card = Card::new(program.name)
                .cost(&cost)
                .kind(program.kind_label())
                .body(program.body())
                .accent(program.accent())
                .state(state);
            card.draw(surface, *rect);
        }
    }

    fn draw_controls(&self, surface: &mut Surface<'_>, jack: Rect, run: Rect) {
        Self::draw_button(
            surface,
            jack,
            "JACK OUT",
            "banks & ends run",
            rgb(72, 210, 232),
            false,
        );

        let confirming = self
            .pending
            .is_some_and(|p| Some(p.lane) == self.active_lane);
        let (run_label, run_sub, run_color) = if confirming {
            ("CONFIRM RUN", "ICE is live!", rgb(226, 90, 90))
        } else {
            ("RUN", "advance the lane", ui::ACCENT)
        };
        Self::draw_button(surface, run, run_label, run_sub, run_color, confirming);
    }

    fn draw_button(
        surface: &mut Surface<'_>,
        rect: Rect,
        label: &str,
        sub: &str,
        color: Color,
        urgent: bool,
    ) {
        let bg = if urgent {
            rgb(28, 10, 10)
        } else {
            rgb(12, 16, 20)
        };
        let inner = panel::Panel::new()
            .border(panel::Border::Double)
            .frame(color)
            .bg(bg)
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }
        let pad = inner.width().saturating_sub(label.chars().count() as u16) / 2;
        surface.print(
            (inner.left() + pad, inner.top()),
            label,
            Style::new().fg(color).bg(bg),
        );
        if inner.height() > 1 {
            let pad2 = inner.width().saturating_sub(sub.chars().count() as u16) / 2;
            surface.print(
                (inner.left() + pad2, inner.top() + 1),
                sub,
                Style::new().fg(ui::DIM).bg(bg),
            );
        }
    }

    fn draw_log_status(&self) -> String {
        let lane_txt = self
            .active_lane
            .map_or_else(|| "no lane".to_string(), |l| format!("lane {}", l + 1));
        format!(
            "banked {}  unbanked {}  {lane_txt}",
            self.banked, self.run_take
        )
    }

    fn resolve_gesture(&mut self, gesture: Gesture) {
        // Track a press that started on a card as a drag candidate, so a
        // drop elsewhere plays it even without a prior tap-select. This is
        // additive to tap-select-then-tap, not a replacement for it: dense
        // boards are exactly where a finger occludes what it's dragging, so
        // the two-tap path stays the primary one and drag is offered as
        // desktop-mouse convenience.
        if let Some(press) = gesture.press
            && self.dragging_card.is_none()
            && let Some(Action::SelectCard(i)) = self.hotspots.hit(press).copied()
        {
            self.dragging_card = Some(i);
        }

        if let Some(tap) = gesture.tap {
            self.dragging_card = None;
            if let Some(action) = self.hotspots.hit(tap).copied() {
                self.dispatch(action);
            }
        }

        if let Some(drop) = gesture.drop {
            let dragged = self.dragging_card.take();
            if let (Some(slot), Some(action)) = (dragged, self.hotspots.hit(drop).copied())
                && let Action::Frontier(lane) | Action::SelectLane(lane) = action
            {
                self.select_lane(lane);
                self.play_selected(slot);
            }
        }
    }

    fn dispatch(&mut self, action: Action) {
        match action {
            Action::SelectLane(lane) => self.select_lane(lane),
            Action::Frontier(lane) => {
                self.select_lane(lane);
                if let Some(slot) = self.selected_card {
                    self.play_selected(slot);
                }
            }
            Action::SelectCard(i) => {
                self.selected_card = if self.selected_card == Some(i) {
                    None
                } else {
                    Some(i)
                };
                // Selecting an untargeted Virus plays it immediately: it has
                // no ICE to aim at, so waiting for a second tap that could
                // never come would just be a dead end in the UI. Copied out
                // rather than matched by reference so the borrow of
                // `self.hand` ends before the `&mut self` call below.
                let is_virus = self
                    .hand
                    .get(i)
                    .is_some_and(|p| matches!(p.kind, ProgramKind::Virus));
                if is_virus && self.selected_card == Some(i) {
                    self.play_selected(i);
                }
            }
            Action::Run => self.trigger_run(),
            Action::JackOut => self.jack_out(),
        }
    }
}

impl Demo for IceBreach {
    const NAME: &'static str = "34_ice_breach";
    const TITLE: &'static str = "34 ICE Breach";
    const BLURB: &'static str =
        "Three vertical server lanes, typed ICE, and a trace meter that never stops.";
    const GRID: (u16, u16) = (150, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-3", "select lane"),
            ("4-7", "select card"),
            ("Enter", "run / confirm run"),
            ("Esc", "jack out"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.simulate(dt);
        let gesture = self.pointer.take();

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let (trace_area, lanes_area, hand_area, controls_area) = Self::tick_layout(content);

        // Layout first, as pure geometry, and only then touch input or draw:
        // the hotspots below have to reflect *this* frame's rects before
        // `resolve_gesture` reads them, and the draw calls further down want
        // the identical rects so what is drawn is what was tappable.
        let lane_rects = Self::layout_lanes(lanes_area);
        let hand_rects = self.hand_rects(hand_area);
        let (jack_rect, run_rect) = Self::control_rects(controls_area);

        self.hotspots.clear();
        for (i, rect) in lane_rects.iter().enumerate() {
            // Registered before the frontier below it, so the frontier's
            // smaller hotspot wins where the two overlap (see `Hotspots`).
            self.hotspots.push(*rect, Action::SelectLane(i));
        }
        for (i, rect) in lane_rects.iter().enumerate() {
            let lane = &self.lanes[i];
            let frontier_h = if lane.cleared() { SERVER_H } else { ICE_H };
            let frontier_y = if lane.cleared() {
                rect.top()
            } else {
                rect.bottom().saturating_sub(1) - frontier_h.min(rect.height())
            };
            let frontier_rect = Rect::new(
                rect.left(),
                frontier_y,
                rect.width(),
                frontier_h.min(rect.bottom().saturating_sub(frontier_y)),
            );
            self.hotspots
                .push_tappable(frontier_rect, *rect, Action::Frontier(i));
        }
        for (i, rect) in hand_rects.iter().enumerate() {
            self.hotspots
                .push_tappable(*rect, hand_area, Action::SelectCard(i));
        }
        self.hotspots.push(jack_rect, Action::JackOut);
        self.hotspots.push(run_rect, Action::Run);

        self.resolve_gesture(gesture);

        for (i, rect) in lane_rects.iter().enumerate() {
            self.draw_lane(&mut surface, *rect, i, self.active_lane == Some(i));
        }
        self.draw_trace(&mut surface, trace_area);
        self.draw_hand(&mut surface, &hand_rects);
        self.draw_controls(&mut surface, jack_rect, run_rect);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.draw_log_status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(IceBreach);
