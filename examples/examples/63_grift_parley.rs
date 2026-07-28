//! 63: Grift Parley -- a negotiation board, not a hand of cards.
//!
//! Adapted from Griftlands (Klei, 2021). Every fight in Griftlands can be
//! fought two ways: with fists, or with words, and the game's signature idea
//! is that talking is a full combat system with its own board, not a
//! dialogue tree. The gallery already has three card-battler demos
//! (`21_deck_plan`, `28_spire_deck`, `57_dealt_dungeon`), and none of them
//! answer the question this one exists to ask: what does it look like to
//! render *persistent, individually-damageable entities clustered around a
//! shared objective*, rather than a flat row of enemies? A hand of cards is
//! still the input device here -- `M` swaps it between a battle hand (cost
//! Blood, targets a foe row) and a negotiation hand (cost Wit, targets an
//! argument cluster) so the duality reads on the same five card slots -- but
//! the hand is not the rendering problem. The **argument cluster** is: a
//! Core Argument with satellite arguments hanging off it, each with its own
//! HP and an optional per-turn effect, connected to the core by a visible
//! link and lawing out however much space the viewport leaves it.
//!
//! Techniques on show:
//!
//! - **An autotiled connector spine** ([`spine_glyph`], [`draw_cluster`]):
//!   the link from each satellite to its core is not a fixed sprite, it is
//!   solved the same way [`tilekit::autotile`] solves a wall run in
//!   `21_deck_plan` -- a per-row cardinal mask picked against
//!   [`tilekit::autotile::BOX_SINGLE`], so the spine grows or shrinks a
//!   T-junction at a time as the satellite count changes and always joins
//!   cleanly, the same "graph laid out as a fixed topology, glyphs chosen
//!   from adjacency" idea Boris the Brave describes for
//!   [wave function collapse and constraint-based tiling](https://www.boristhebrave.com/2020/04/13/wave-function-collapse-explained/),
//!   applied here to a much smaller, fully known graph (a star, not a grid).
//! - **A viewport-sized cluster tier** ([`cluster_tier`]): satellite count
//!   and box height step down together as the available half of the board
//!   shrinks, the same tiering idea `card::Tier` uses for a single card
//!   applied to an entire cluster, so a desktop window shows three linked
//!   satellites per side and an 80x24 terminal still shows the one thing
//!   that matters (the core) rather than clipping or panicking.
//! - **Selection then targeting, not drag-to-target**
//!   ([`GriftParley::handle_key`], [`GriftParley::layout`]): picking a card
//!   arms it; a second tap or an arrow-cycled Enter resolves it against
//!   whichever entity in the opposing cluster the cursor or pointer lands
//!   on. Legal targets (alive, on the opposing side) draw with a brightened,
//!   focused [`Panel`]; illegal ones (dead, or the caller's own side) draw
//!   dimmed, so the board itself answers "what can this card hit" without a
//!   tooltip.
//! - **A turn clock with no player input at all**
//!   ([`GriftParley::advance_turn`]): every [`TURN_SECONDS`] of accumulated
//!   `frame.delta`, satellites with a per-turn effect fire, the opposing
//!   side plays one action of its own, and both sides' resource refills --
//!   the thing that proves the board is alive even if the player never
//!   touches a key.
//! - **Cards from [`ui::card`]**, the same fanned, tiered widget
//!   `28_spire_deck` and `57_dealt_dungeon` use, deliberately reused rather
//!   than reinvented: the hand is not this demo's novelty, so it is drawn
//!   with the gallery's existing card vocabulary and nothing more.
//!
//! ```sh
//! cargo run --example 63_grift_parley --features crossterm
//! cargo run --example 63_grift_parley --features software
//! cargo run --example 63_grift_parley --features gl
//! cargo run --example 63_grift_parley  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{Card, CardState};
use ascii_tile_demos::ui::panel::{self, Border, Panel};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, E, N, S, W};
use tilekit::noise::Rng;
use tilekit::palette::{rgb, scale};

/// Which half of the duality is currently in play. Swapping this swaps both
/// the hand's cost resource and target set *and* the board it targets, which
/// is the whole point: Griftlands never treats these as reskins of the same
/// fight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Battle,
    Negotiate,
}

/// Width of a satellite argument's box.
const SAT_W: u16 = 13;
/// Width of a Core Argument's box. Wider than a satellite: the core carries
/// a Resolve bar and a Composure line a satellite does not.
const CORE_W: u16 = 19;
/// Most satellites a cluster ever shows at once, on either side. Bounded so
/// the connector spine (a small, known star graph) never has to solve an
/// unbounded layout, and so a runaway `Summon` card has somewhere to stop.
const MAX_SATS: usize = 3;

/// Seconds of accumulated `frame.delta` between turn clocks. Long enough
/// that a played card is clearly the player's move and the clock's tick is
/// clearly not; short enough that a demo left running is never watched idle
/// for long.
const TURN_SECONDS: f32 = 6.0;

/// Resource both sides refill to at every turn tick, in Blood (battle) or
/// Wit (negotiation). Three lets a player afford one big play or two small
/// ones per turn, which is enough to make the cost badges matter without
/// turning the hand into a puzzle.
const RESOURCE_MAX: i32 = 3;

const PLAYER_COLOR: Color = rgb(96, 220, 226);
const ENEMY_COLOR: Color = rgb(226, 96, 210);

/// What a card does when played. Shared between the battle and negotiation
/// pools; each pool only ever constructs the variants that make sense for
/// its own mode, so a mismatch (e.g. `Summon` in the battle pool) cannot
/// occur without editing this file.
#[derive(Clone, Copy)]
enum CardEffect {
    /// Deal a random amount in `[lo, hi]`, absorbed by Composure/Block first.
    Damage(f32, f32),
    /// Deal a fixed amount, ignoring Composure/Block entirely.
    Pierce(f32),
    /// Deal a fixed amount; if the target falls, an extra amount hits the
    /// opposing Core directly. Negotiation only.
    Chain(f32, f32),
    /// Deal a fixed amount only if the target's Block is currently zero.
    /// Battle only.
    IfNoBlock(f32),
    /// Play a new satellite argument onto the caller's own cluster, with a
    /// per-turn chip-damage effect. Negotiation only.
    Summon(&'static str, f32, f32),
    /// Grant the caller's own Core this much Composure. Negotiation only.
    Composure(f32),
    /// Grant the player this much Block. Battle only.
    Block(f32),
}

/// One entry in a card pool: static data, cheap to copy, one per hand slot.
#[derive(Clone, Copy)]
struct CardDef {
    name: &'static str,
    cost: i32,
    kind_label: &'static str,
    body: &'static str,
    tint: Color,
    needs_target: bool,
    effect: CardEffect,
}

/// The negotiation hand: five cards costing Wit, aimed at an opposing
/// argument or played on the caller's own cluster.
const NEGOTIATE_CARDS: [CardDef; 5] = [
    CardDef {
        name: "Sharp Retort",
        cost: 1,
        kind_label: "Rebuttal",
        body: "Deal 4 to Resolve.",
        tint: rgb(96, 190, 226),
        needs_target: true,
        effect: CardEffect::Damage(4.0, 4.0),
    },
    CardDef {
        name: "Undermine",
        cost: 2,
        kind_label: "Attack",
        body: "Deal 6. Ignore Composure.",
        tint: rgb(210, 110, 200),
        needs_target: true,
        effect: CardEffect::Pierce(6.0),
    },
    CardDef {
        name: "Low Blow",
        cost: 2,
        kind_label: "Attack",
        body: "Deal 5. If it falls, hit Core for 3.",
        tint: rgb(230, 140, 90),
        needs_target: true,
        effect: CardEffect::Chain(5.0, 3.0),
    },
    CardDef {
        name: "Rally Point",
        cost: 1,
        kind_label: "Support",
        body: "Play Leverage: 6 HP, chips 1/turn.",
        tint: rgb(120, 220, 140),
        needs_target: false,
        effect: CardEffect::Summon("Leverage", 6.0, 1.0),
    },
    CardDef {
        name: "Compose Yourself",
        cost: 1,
        kind_label: "Guard",
        body: "Gain 4 Composure.",
        tint: rgb(210, 200, 90),
        needs_target: false,
        effect: CardEffect::Composure(4.0),
    },
];

/// The battle hand: five cards costing Blood, aimed at a foe or played on
/// the player directly. Deliberately the simpler of the two boards -- a
/// plain foe row, no cluster -- so the effort in this file goes toward the
/// negotiation side.
const BATTLE_CARDS: [CardDef; 5] = [
    CardDef {
        name: "Jab",
        cost: 1,
        kind_label: "Attack",
        body: "Deal 3 damage.",
        tint: rgb(220, 110, 90),
        needs_target: true,
        effect: CardEffect::Damage(3.0, 3.0),
    },
    CardDef {
        name: "Haymaker",
        cost: 2,
        kind_label: "Attack",
        body: "Deal 3 to 6 damage.",
        tint: rgb(220, 110, 90),
        needs_target: true,
        effect: CardEffect::Damage(3.0, 6.0),
    },
    CardDef {
        name: "Guard Up",
        cost: 1,
        kind_label: "Skill",
        body: "Gain 5 Block.",
        tint: rgb(120, 190, 220),
        needs_target: false,
        effect: CardEffect::Block(5.0),
    },
    CardDef {
        name: "Low Strike",
        cost: 2,
        kind_label: "Attack",
        body: "Deal 5. Ignore Block.",
        tint: rgb(210, 110, 200),
        needs_target: true,
        effect: CardEffect::Pierce(5.0),
    },
    CardDef {
        name: "Finisher",
        cost: 3,
        kind_label: "Attack",
        body: "Deal 9 if their Block is 0.",
        tint: rgb(230, 150, 60),
        needs_target: true,
        effect: CardEffect::IfNoBlock(9.0),
    },
];

/// Name pool AI-summoned and starting satellites draw from without
/// replacement within one side, so two arguments on the same cluster never
/// share a name.
const SAT_NAMES: [&str; 8] = [
    "Leverage", "Hearsay", "Old Debt", "Witness", "Bribe", "Grudge", "Rumor", "Alibi",
];

/// One argument on a negotiation cluster: a Core or a satellite. Both are
/// the same shape, which is exactly the point Griftlands makes -- a core is
/// a satellite with nowhere left to retreat to.
struct Argument {
    name: String,
    hp: f32,
    max_hp: f32,
    composure: f32,
    /// `(label, chip damage)` fired at a random enemy argument on every
    /// [`GriftParley::advance_turn`], or `None` for an argument with no
    /// standing effect (every Core, most satellites).
    tick: Option<(&'static str, f32)>,
}

impl Argument {
    fn core(name: &str, hp: f32, composure: f32) -> Self {
        Self {
            name: name.to_string(),
            hp,
            max_hp: hp,
            composure,
            tick: None,
        }
    }

    fn satellite(name: &str, hp: f32, tick: Option<(&'static str, f32)>) -> Self {
        Self {
            name: name.to_string(),
            hp,
            max_hp: hp,
            composure: 0.0,
            tick,
        }
    }

    const fn alive(&self) -> bool {
        self.hp > 0.0
    }

    /// Applies `dmg` to this argument, absorbing it with Composure first
    /// unless `pierce`. Returns whether this hit brought it to zero.
    fn hit(&mut self, mut dmg: f32, pierce: bool) -> bool {
        let was_alive = self.alive();
        if !pierce && self.composure > 0.0 {
            let absorbed = dmg.min(self.composure);
            self.composure -= absorbed;
            dmg -= absorbed;
        }
        self.hp = (self.hp - dmg).max(0.0);
        was_alive && !self.alive()
    }
}

/// One side of the negotiation board: a Core plus up to [`MAX_SATS`]
/// satellites.
struct Cluster {
    core: Argument,
    satellites: Vec<Argument>,
}

impl Cluster {
    fn generate(rng: &mut Rng, core_name: &str, core_hp: f32, composure: f32) -> Self {
        let mut names: Vec<&str> = SAT_NAMES.to_vec();
        for i in (1..names.len()).rev() {
            let j = rng.next_below((i + 1) as u32) as usize;
            names.swap(i, j);
        }
        let count = 1 + rng.next_below(2) as usize; // one or two, to start
        let satellites = names
            .into_iter()
            .take(count)
            .map(|name| {
                let hp = 5.0 + rng.next_below(5) as f32;
                let tick = if rng.next_f32() < 0.4 {
                    Some(("+1/turn", 1.0))
                } else {
                    None
                };
                Argument::satellite(name, hp, tick)
            })
            .collect();
        Self {
            core: Argument::core(core_name, core_hp, composure),
            satellites,
        }
    }

    /// Drops any satellite that has fallen. Called right after every hit so
    /// a destroyed argument disappears from the board rather than lingering
    /// as a zero-HP box.
    fn cull(&mut self) {
        self.satellites.retain(Argument::alive);
    }
}

/// Which entity a targeted card is aimed at: the core, or a satellite by
/// index into the opposing cluster's current `satellites`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TargetId {
    Core,
    Sat(usize),
}

/// How an argument's frame reads relative to a currently armed, targeted
/// card: nothing armed, a legal target, the legal target the keyboard
/// cursor currently sits on, or an illegal target (the caller's own side).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tint {
    Normal,
    Legal,
    Cursor,
    Illegal,
}

/// A tappable region's meaning, valid for exactly the frame it was built in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// Arm hand card `index`.
    Card(usize),
    /// Resolve the armed card against this entity in the opposing cluster
    /// (negotiation) or this foe (battle).
    Target(TargetId),
}

/// One foe in battle mode: no cluster, just HP and Block.
struct Foe {
    name: String,
    hp: f32,
    max_hp: f32,
    block: f32,
}

const FOE_NAMES: [&str; 3] = ["Enforcer", "Thug", "Bruiser"];

/// State: both boards (only one drawn at a time, per [`Mode`]), the hand,
/// the turn clock, and everything needed to draw and interact with all of
/// it.
pub struct GriftParley {
    mode: Mode,
    seed: u32,
    rng: Rng,

    // -- negotiation board --
    player: Cluster,
    enemy: Cluster,

    // -- battle board --
    foes: Vec<Foe>,
    player_hp: f32,
    player_max_hp: f32,
    player_block: f32,

    resource: i32,
    turn: u32,
    turn_timer: f32,

    /// Index into the active mode's hand cursor (keyboard selection before
    /// a card is armed).
    cursor: usize,
    armed: Option<usize>,
    /// Target cursor once a targeted card is armed.
    target_cursor: usize,

    log: String,
    /// `Some(true)` on a negotiation win, `Some(false)` on a loss. Battle
    /// mode never sets this; it is a smaller board on purpose and does not
    /// need its own end state to make the point.
    over: Option<bool>,

    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for GriftParley {
    fn default() -> Self {
        Self::from_seed(0x6712_F00D)
    }
}

impl GriftParley {
    fn from_seed(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let player = Cluster::generate(&mut rng, "Your Case", 22.0, 4.0);
        let enemy = Cluster::generate(&mut rng, "Their Case", 22.0, 4.0);
        let foes = FOE_NAMES
            .iter()
            .take(2 + rng.next_below(2) as usize)
            .map(|name| {
                let hp = 10.0 + rng.next_below(8) as f32;
                Foe {
                    name: (*name).to_string(),
                    hp,
                    max_hp: hp,
                    block: 0.0,
                }
            })
            .collect();
        Self {
            mode: Mode::Negotiate,
            seed,
            rng,
            player,
            enemy,
            foes,
            player_hp: 30.0,
            player_max_hp: 30.0,
            player_block: 0.0,
            resource: RESOURCE_MAX,
            turn: 1,
            turn_timer: 0.0,
            cursor: 0,
            armed: None,
            target_cursor: 0,
            log: "The other side makes their opening argument.".to_string(),
            over: None,
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }

    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(0x9E37_79B9);
        let mode = self.mode;
        *self = Self::from_seed(self.seed);
        self.mode = mode;
    }

    const fn hand(&self) -> &'static [CardDef; 5] {
        match self.mode {
            Mode::Battle => &BATTLE_CARDS,
            Mode::Negotiate => &NEGOTIATE_CARDS,
        }
    }

    /// The opposing cluster's current target count (core plus satellites),
    /// used to keep cursors and hit-testing in range as satellites die.
    const fn target_count(&self) -> usize {
        match self.mode {
            Mode::Battle => self.foes.len(),
            Mode::Negotiate => 1 + self.enemy.satellites.len(),
        }
    }

    // -- turn clock --------------------------------------------------------

    /// Advances the turn clock by `dt`, firing [`Self::advance_turn`] for
    /// every [`TURN_SECONDS`] that elapses. A `while` rather than an `if`
    /// so a stalled frame (or a very short `TURN_SECONDS` in a test) cannot
    /// let more than one turn's worth of time silently vanish.
    fn tick_clock(&mut self, dt: f32) {
        if self.over.is_some() {
            return;
        }
        self.turn_timer += dt;
        // `while` rather than a float `>=` early-out check clippy would flag:
        // count how many turns elapsed with integer arithmetic, then apply.
        let elapsed = (self.turn_timer / TURN_SECONDS).floor();
        if elapsed >= 1.0 {
            self.turn_timer = elapsed.mul_add(-TURN_SECONDS, self.turn_timer);
            for _ in 0..(elapsed as u32) {
                self.advance_turn();
            }
        }
    }

    /// One full turn tick: standing effects fire, the opposing side acts
    /// once on its own, and the resource pool refills. Runs with no player
    /// input at all, which is what proves the board is alive between plays.
    fn advance_turn(&mut self) {
        self.turn += 1;
        self.resource = RESOURCE_MAX;
        match self.mode {
            Mode::Negotiate => self.advance_negotiate_turn(),
            Mode::Battle => self.advance_battle_turn(),
        }
    }

    fn advance_negotiate_turn(&mut self) {
        // Standing effects: every enemy satellite with a tick chips a
        // random alive player argument. Fired before the enemy's own play so
        // a freshly summoned satellite does not act the same turn it lands.
        let chips: Vec<f32> = self
            .enemy
            .satellites
            .iter()
            .filter_map(|s| s.tick.map(|(_, dmg)| dmg))
            .collect();
        for dmg in chips {
            self.damage_random(false, dmg, false);
        }
        self.player.cull();

        // The opposing side's own action: summon if there is room and the
        // roll favors it, otherwise attack.
        if self.enemy.satellites.len() < MAX_SATS && self.rng.next_f32() < 0.35 {
            self.enemy_summon();
        } else {
            let dmg = 3.0 + self.rng.next_below(4) as f32;
            self.damage_random(false, dmg, false);
            self.player.cull();
        }

        if self.player.core.hp <= 0.0 {
            self.over = Some(false);
            self.log = "Your Case collapses. They win the room.".to_string();
        } else if self.enemy.core.hp <= 0.0 {
            self.over = Some(true);
            self.log = "Their Case falls apart. You win the room.".to_string();
        }
    }

    fn advance_battle_turn(&mut self) {
        if self.foes.is_empty() {
            return;
        }
        let idx = self.rng.next_below(self.foes.len() as u32) as usize;
        let dmg = 2.0 + self.rng.next_below(4) as f32;
        let absorbed = dmg.min(self.player_block);
        self.player_block -= absorbed;
        self.player_hp = (self.player_hp - (dmg - absorbed)).max(0.0);
        self.log = format!("{} swings for {dmg:.0}.", self.foes[idx].name);
    }

    fn enemy_summon(&mut self) {
        let taken: Vec<&str> = self
            .enemy
            .satellites
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let Some(name) = SAT_NAMES.iter().find(|n| !taken.contains(n)) else {
            return;
        };
        let hp = 5.0 + self.rng.next_below(5) as f32;
        self.enemy
            .satellites
            .push(Argument::satellite(name, hp, Some(("+1/turn", 1.0))));
        self.log = format!("They play {name}.");
    }

    /// Deals `dmg` to a random alive argument on the named side (`player`
    /// selects which cluster is hit). Used by standing effects and the AI's
    /// plain attack, both of which pick their own target rather than the
    /// player picking it for them.
    fn damage_random(&mut self, hit_player: bool, dmg: f32, pierce: bool) {
        let cluster = if hit_player {
            &mut self.player
        } else {
            &mut self.enemy
        };
        let pool = cluster.satellites.len();
        if pool > 0 && self.rng.next_f32() < 0.7 {
            let i = self.rng.next_below(pool as u32) as usize;
            cluster.satellites[i].hit(dmg, pierce);
        } else {
            cluster.core.hit(dmg, pierce);
        }
    }

    // -- playing a card ------------------------------------------------

    const fn afford(&self, cost: i32) -> bool {
        self.resource >= cost
    }

    /// Arms hand slot `idx`, or resolves it immediately if it needs no
    /// target. Affordability is re-checked here, not just at draw time: a
    /// stale hotspot from a frame where the card was still affordable must
    /// not spend resource it no longer has.
    fn select_card(&mut self, idx: usize) {
        if self.over.is_some() {
            return;
        }
        let def = self.hand()[idx];
        if !self.afford(def.cost) {
            return;
        }
        if self.armed == Some(idx) {
            self.armed = None; // tapping the armed card again cancels it
            return;
        }
        if def.needs_target {
            self.armed = Some(idx);
            self.target_cursor = 0;
        } else {
            self.resolve(idx, None);
        }
    }

    fn select_target(&mut self, target: TargetId) {
        let Some(idx) = self.armed else { return };
        self.resolve(idx, Some(target));
    }

    fn resolve(&mut self, idx: usize, target: Option<TargetId>) {
        let def = self.hand()[idx];
        if !self.afford(def.cost) {
            self.armed = None;
            return;
        }
        self.resource -= def.cost;
        self.armed = None;
        match self.mode {
            Mode::Negotiate => self.resolve_negotiate(def, target),
            Mode::Battle => self.resolve_battle(def, target),
        }
    }

    fn resolve_negotiate(&mut self, def: CardDef, target: Option<TargetId>) {
        let roll = |rng: &mut Rng, lo: f32, hi: f32| {
            if hi <= lo {
                lo
            } else {
                rng.next_f32().mul_add(hi - lo, lo)
            }
        };
        match def.effect {
            CardEffect::Damage(..) | CardEffect::Pierce(_) | CardEffect::Chain(..) => {
                let dmg = match def.effect {
                    CardEffect::Damage(lo, hi) => roll(&mut self.rng, lo, hi),
                    CardEffect::Pierce(v) | CardEffect::Chain(v, _) => v,
                    _ => unreachable!(),
                };
                let pierce = matches!(def.effect, CardEffect::Pierce(_));
                let Some(target) = target else { return };
                let fell = match target {
                    TargetId::Core => {
                        self.enemy.core.hit(dmg, pierce);
                        false
                    }
                    TargetId::Sat(i) => self
                        .enemy
                        .satellites
                        .get_mut(i)
                        .is_some_and(|s| s.hit(dmg, pierce)),
                };
                self.log = format!("You play {} for {dmg:.0}.", def.name);
                if let CardEffect::Chain(_, extra) = def.effect
                    && fell
                {
                    self.enemy.core.hit(extra, true);
                    self.log = format!("{} falls; the Core takes {extra:.0} more.", def.name);
                }
                self.enemy.cull();
            }
            CardEffect::Summon(name, hp, chip) => {
                if self.player.satellites.len() < MAX_SATS {
                    self.player.satellites.push(Argument::satellite(
                        name,
                        hp,
                        Some(("+1/turn", chip)),
                    ));
                    self.log = format!("You play {name}.");
                } else {
                    self.log = "No room on your side of the board.".to_string();
                }
            }
            CardEffect::Composure(amount) => {
                self.player.core.composure += amount;
                self.log = format!("You compose yourself. +{amount:.0} Composure.");
            }
            CardEffect::IfNoBlock(_) | CardEffect::Block(_) => {
                unreachable!("battle-only effects never appear in NEGOTIATE_CARDS")
            }
        }
        if self.enemy.core.hp <= 0.0 {
            self.over = Some(true);
            self.log = "Their Case falls apart. You win the room.".to_string();
        }
    }

    fn resolve_battle(&mut self, def: CardDef, target: Option<TargetId>) {
        let roll = |rng: &mut Rng, lo: f32, hi: f32| {
            if hi <= lo {
                lo
            } else {
                rng.next_f32().mul_add(hi - lo, lo)
            }
        };
        match def.effect {
            CardEffect::Block(amount) => {
                self.player_block += amount;
                self.log = format!("You gain {amount:.0} Block.");
            }
            CardEffect::Damage(..) | CardEffect::Pierce(_) | CardEffect::IfNoBlock(_) => {
                let TargetId::Sat(i) = target.unwrap_or(TargetId::Sat(0)) else {
                    return;
                };
                let Some(foe) = self.foes.get_mut(i) else {
                    return;
                };
                let (dmg, pierce) = match def.effect {
                    CardEffect::Damage(lo, hi) => (roll(&mut self.rng, lo, hi), false),
                    CardEffect::Pierce(v) => (v, true),
                    CardEffect::IfNoBlock(v) => {
                        if foe.block > 0.0 {
                            self.log = format!("{} still has Block; Finisher fizzles.", foe.name);
                            return;
                        }
                        (v, true)
                    }
                    _ => unreachable!(),
                };
                let absorbed = if pierce { 0.0 } else { dmg.min(foe.block) };
                foe.block -= absorbed;
                foe.hp = (foe.hp - (dmg - absorbed)).max(0.0);
                self.log = format!("You hit {} for {:.0}.", foe.name, dmg - absorbed);
                self.foes.retain(|f| f.hp > 0.0);
            }
            CardEffect::Chain(..) | CardEffect::Summon(..) | CardEffect::Composure(_) => {
                unreachable!("negotiation-only effects never appear in BATTLE_CARDS")
            }
        }
    }

    // -- input --------------------------------------------------------

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
        match code {
            KeyCode::Char('r' | 'R') => {
                self.reroll();
                return;
            }
            KeyCode::Char('m' | 'M') => {
                self.mode = match self.mode {
                    Mode::Battle => Mode::Negotiate,
                    Mode::Negotiate => Mode::Battle,
                };
                self.armed = None;
                self.cursor = 0;
                return;
            }
            _ => {}
        }
        if self.over.is_some() {
            return;
        }
        if let Some(idx) = self.armed {
            let n = self.target_count().max(1);
            match code {
                KeyCode::Left | KeyCode::Char('a' | 'A') => {
                    self.target_cursor = (self.target_cursor + n - 1) % n;
                }
                KeyCode::Right | KeyCode::Char('d' | 'D') => {
                    self.target_cursor = (self.target_cursor + 1) % n;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let target = self.cursor_target();
                    self.select_target(target);
                }
                KeyCode::Escape | KeyCode::Backspace => {
                    self.armed = None;
                    let _ = idx;
                }
                _ => {}
            }
            return;
        }
        let hand_len = self.hand().len();
        match code {
            KeyCode::Left | KeyCode::Char('a' | 'A') => {
                self.cursor = (self.cursor + hand_len - 1) % hand_len;
            }
            KeyCode::Right | KeyCode::Char('d' | 'D') => {
                self.cursor = (self.cursor + 1) % hand_len;
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.select_card(self.cursor),
            KeyCode::Char(c @ '1'..='5') => {
                let idx = c as usize - '1' as usize;
                if idx < hand_len {
                    self.select_card(idx);
                }
            }
            _ => {}
        }
    }

    /// The [`TargetId`] the keyboard target cursor currently points at:
    /// index 0 is always the opposing Core, the rest are its live
    /// satellites in display order.
    const fn cursor_target(&self) -> TargetId {
        match self.mode {
            Mode::Battle => TargetId::Sat(self.target_cursor),
            Mode::Negotiate => {
                if self.target_cursor == 0 {
                    TargetId::Core
                } else {
                    TargetId::Sat(self.target_cursor - 1)
                }
            }
        }
    }

    fn handle_tap(&mut self, pos: Pos) {
        let Some(&action) = self.hotspots.hit(pos) else {
            return;
        };
        match action {
            Action::Card(idx) => self.select_card(idx),
            Action::Target(target) => self.select_target(target),
        }
    }

    // -- layout ---------------------------------------------------------

    fn hand_rects(&self, area: Rect) -> Vec<Rect> {
        ui::card::fan(area, self.hand().len(), ui::card::FULL_W)
    }

    /// Builds this frame's hotspots and resolves the held gesture before
    /// drawing, the same "layout owns the tap" ordering `57_dealt_dungeon`
    /// uses: the tap has to be able to mutate state this frame's drawing
    /// then reflects.
    fn layout(&mut self, hand_area: Rect, board_area: Rect, gesture: &Gesture) {
        self.hotspots.clear();
        let hand_rects = self.hand_rects(hand_area);
        let armed = self.armed;
        // Draw/registration order matters: the armed card is redrawn lifted
        // and must be hit-tested last, so register every other card first
        // and the armed one last.
        for (idx, &rect) in hand_rects.iter().enumerate() {
            if Some(idx) != armed {
                self.hotspots
                    .push_tappable(rect, hand_area, Action::Card(idx));
            }
        }
        if let Some(idx) = armed
            && let Some(&rect) = hand_rects.get(idx)
        {
            self.hotspots
                .push_tappable(rect, hand_area, Action::Card(idx));
        }

        if self.over.is_none() && armed.is_some() {
            match self.mode {
                Mode::Battle => {
                    let rects = foe_row_rects(board_area, self.foes.len());
                    for (i, rect) in rects.into_iter().enumerate() {
                        self.hotspots.push_tappable(
                            rect,
                            board_area,
                            Action::Target(TargetId::Sat(i)),
                        );
                    }
                }
                Mode::Negotiate => {
                    let layout = cluster_layout(board_area, self.enemy.satellites.len());
                    self.hotspots.push_tappable(
                        layout.core,
                        board_area,
                        Action::Target(TargetId::Core),
                    );
                    for (i, rect) in layout.sats.into_iter().enumerate() {
                        self.hotspots.push_tappable(
                            rect,
                            board_area,
                            Action::Target(TargetId::Sat(i)),
                        );
                    }
                }
            }
        }

        if let Some(pos) = gesture.tap {
            self.handle_tap(pos);
        }
    }

    // -- drawing --------------------------------------------------------

    fn draw_hand(&self, surface: &mut Surface<'_>, area: Rect) {
        let rects = self.hand_rects(area);
        let hand = self.hand();
        // The armed card is drawn (and registered) last, so it visually
        // sits on top of its fanned neighbors -- see `card.rs`'s own doc on
        // why overlap order and hit-test order must agree.
        let order: Vec<usize> = (0..hand.len())
            .filter(|&i| Some(i) != self.armed)
            .chain(self.armed)
            .collect();
        for idx in order {
            let Some(&rect) = rects.get(idx) else {
                continue;
            };
            if rect.width() == 0 {
                continue;
            }
            let def = hand[idx];
            let affordable = self.afford(def.cost);
            let state = if Some(idx) == self.armed {
                CardState::Selected
            } else if idx == self.cursor && self.armed.is_none() {
                CardState::Held
            } else if !affordable {
                CardState::Disabled
            } else {
                CardState::Idle
            };
            let draw_rect = if state == CardState::Selected {
                Rect::new(
                    rect.left(),
                    rect.top().saturating_sub(1),
                    rect.width(),
                    rect.height(),
                )
            } else {
                rect
            };
            Card::new(def.name)
                .cost(&def.cost.to_string())
                .kind(def.kind_label)
                .body(def.body)
                .accent(def.tint)
                .state(state)
                .draw(surface, draw_rect);
        }
    }

    fn draw_argument(
        surface: &mut Surface<'_>,
        rect: Rect,
        arg: &Argument,
        owner: Color,
        tint: Tint,
    ) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let (frame_color, focused) = match tint {
            Tint::Cursor => (owner, true),
            Tint::Legal | Tint::Normal => (owner, false),
            Tint::Illegal => (scale(owner, 0.35), false),
        };
        let inner = Panel::new()
            .border(Border::Single)
            .title(&arg.name)
            .frame(frame_color)
            .bg(rgb(16, 18, 26))
            .focused(focused)
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }
        let bg = rgb(16, 18, 26);
        let hp_text = format!("HP {:.0}/{:.0}", arg.hp, arg.max_hp);
        let hp_color = panel::threshold(arg.hp / arg.max_hp.max(1.0));
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(&hp_text, inner.width_usize()),
            Style::new().fg(hp_color).bg(bg),
        );
        if inner.height() > 1 {
            let line = if arg.composure > 0.0 {
                format!("Comp {:.0}", arg.composure)
            } else if let Some((label, _)) = arg.tick {
                label.to_string()
            } else {
                // ASCII hyphen, not an em dash: U+2014 is outside CP437 and
                // outside the block sheet's codepage, so it draws as a solid
                // rectangle on both pixel backends.
                "-".to_string()
            };
            surface.print(
                (inner.left(), inner.top() + 1),
                retroglyph_widgets::truncate(&line, inner.width_usize()),
                Style::new().fg(ui::DIM).bg(bg),
            );
        }
        if inner.height() > 2
            && arg.composure > 0.0
            && let Some((label, _)) = arg.tick
        {
            surface.print(
                (inner.left(), inner.top() + 2),
                retroglyph_widgets::truncate(label, inner.width_usize()),
                Style::new().fg(ui::DIM).bg(bg),
            );
        }
    }

    /// Draws one cluster (core plus satellites) with its autotiled spine.
    ///
    /// `is_opposing` is the side a currently-armed targeted card can hit.
    /// While a card is armed, the opposing side's entities draw as legal
    /// targets (the one under the cursor brighter still) and the caller's
    /// own side draws dimmed, so the board itself answers "what can this
    /// card hit" without a tooltip.
    fn draw_cluster(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        cluster: &Cluster,
        owner: Color,
        is_opposing: bool,
    ) {
        let layout = cluster_layout(area, cluster.satellites.len());
        draw_spine(surface, &layout, owner);
        let armed = self.armed.is_some();

        let core_tint = match (armed, is_opposing, self.cursor_target()) {
            (false, _, _) => Tint::Normal,
            (true, true, TargetId::Core) => Tint::Cursor,
            (true, true, _) => Tint::Legal,
            (true, false, _) => Tint::Illegal,
        };
        Self::draw_argument(surface, layout.core, &cluster.core, owner, core_tint);

        for (i, &rect) in layout.sats.iter().enumerate() {
            let Some(arg) = cluster.satellites.get(i) else {
                continue;
            };
            let tint = match (armed, is_opposing, self.cursor_target()) {
                (false, _, _) => Tint::Normal,
                (true, true, TargetId::Sat(c)) if c == i => Tint::Cursor,
                (true, true, _) => Tint::Legal,
                (true, false, _) => Tint::Illegal,
            };
            Self::draw_argument(surface, rect, arg, owner, tint);
        }
    }

    fn draw_negotiate_board(&self, surface: &mut Surface<'_>, area: Rect) {
        let (opp_area, player_area) = panel::split_top(area, area.height() / 2);
        self.draw_cluster(
            surface,
            opp_area,
            &self.enemy,
            ENEMY_COLOR,
            self.armed.is_some(),
        );
        self.draw_cluster(surface, player_area, &self.player, PLAYER_COLOR, false);
    }

    fn draw_battle_board(&self, surface: &mut Surface<'_>, area: Rect) {
        let foes_h = area.height().saturating_sub(4).min(area.height());
        let (foes_area, stat_area) = panel::split_top(area, foes_h);
        let rects = foe_row_rects(foes_area, self.foes.len());
        for (i, foe) in self.foes.iter().enumerate() {
            let Some(&rect) = rects.get(i) else { continue };
            let targeting = self.armed.map(|_| true);
            let frame = if targeting == Some(true) {
                ENEMY_COLOR
            } else {
                panel::FRAME
            };
            let inner = Panel::new()
                .title(&foe.name)
                .frame(frame)
                .bg(rgb(16, 18, 26))
                .focused(targeting == Some(true) && self.cursor_target() == TargetId::Sat(i))
                .draw(surface, rect);
            if inner.height() == 0 {
                continue;
            }
            let hp_text = format!("HP {:.0}/{:.0}", foe.hp, foe.max_hp);
            surface.print(
                (inner.left(), inner.top()),
                retroglyph_widgets::truncate(&hp_text, inner.width_usize()),
                Style::new()
                    .fg(panel::threshold(foe.hp / foe.max_hp.max(1.0)))
                    .bg(rgb(16, 18, 26)),
            );
            if inner.height() > 1 {
                let block = format!("Block {:.0}", foe.block);
                surface.print(
                    (inner.left(), inner.top() + 1),
                    retroglyph_widgets::truncate(&block, inner.width_usize()),
                    Style::new().fg(ui::DIM).bg(rgb(16, 18, 26)),
                );
            }
        }

        let inner = Panel::new()
            .title("You")
            .frame(PLAYER_COLOR)
            .bg(rgb(16, 18, 26))
            .draw(surface, stat_area);
        if inner.height() == 0 {
            return;
        }
        let line = format!(
            "HP {:.0}/{:.0}   Block {:.0}",
            self.player_hp, self.player_max_hp, self.player_block
        );
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(&line, inner.width_usize()),
            Style::new().fg(ui::FG).bg(rgb(16, 18, 26)),
        );
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        panel::band(surface, area);
        let hint = self.over.map_or_else(
            || {
                if self.armed.is_some() {
                    "Tap or Left/Right + Enter to pick a target. Esc cancels."
                } else {
                    "Tap or Left/Right + Enter to pick a card. M swaps Battle/Negotiate."
                }
            },
            |won| {
                if won {
                    "You won the room. Press R for a new one."
                } else {
                    "You lost the room. Press R for a new one."
                }
            },
        );
        let text = if self.over.is_some() {
            hint
        } else {
            self.log.as_str()
        };
        surface.print(
            (area.left(), area.top()),
            retroglyph_widgets::truncate(text, area.width_usize()),
            Style::new().fg(ui::FG).bg(ui::CHROME_BG),
        );
        if area.height() > 1 && self.over.is_none() {
            surface.print(
                (area.left(), area.top() + 1),
                retroglyph_widgets::truncate(hint, area.width_usize()),
                Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
            );
        }
    }

    fn status(&self) -> String {
        let mode = match self.mode {
            Mode::Battle => "battle",
            Mode::Negotiate => "negotiate",
        };
        let resource = match self.mode {
            Mode::Battle => "blood",
            Mode::Negotiate => "wit",
        };
        format!(
            "{mode}  turn {}  {resource} {}/{}  next tick {:.0}s",
            self.turn,
            self.resource,
            RESOURCE_MAX,
            (TURN_SECONDS - self.turn_timer).max(0.0)
        )
    }
}

/// One row of foe boxes, evenly spaced across `area`.
fn foe_row_rects(area: Rect, count: usize) -> Vec<Rect> {
    if count == 0 || area.width() == 0 || area.height() == 0 {
        return Vec::new();
    }
    panel::columns(area, count as u16, 1)
        .into_iter()
        .map(|r| Rect::new(r.left(), r.top(), r.width(), r.height().clamp(1, 4)))
        .collect()
}

/// Where a cluster's core and satellites land inside `area`: satellites
/// stacked on the left, the core on the right, one column of connector
/// spine between them.
struct ClusterLayout {
    core: Rect,
    sats: Vec<Rect>,
    spine_col: u16,
}

/// Picks how many satellites to show and how tall each box is, given half
/// the board's height. Steps down together (fewer, shorter boxes) the same
/// way `card::Tier` drops fields before it drops the whole card, so a
/// cramped viewport still shows a core and at least one linked satellite
/// rather than an empty rect.
fn cluster_tier(half_h: u16, want: usize) -> (usize, u16, u16) {
    let want = want.min(MAX_SATS);
    let tiers: [(usize, u16, u16); 5] = [(3, 5, 6), (3, 4, 5), (2, 4, 5), (2, 3, 4), (1, 3, 4)];
    for &(cap, sat_h, core_h) in &tiers {
        let sats = want.min(cap);
        let stack = sats as u16 * sat_h + sats.saturating_sub(1) as u16;
        if stack.max(core_h) <= half_h {
            return (sats, sat_h, core_h);
        }
    }
    (want.min(1), 3, half_h.max(1))
}

fn cluster_layout(area: Rect, satellite_count: usize) -> ClusterLayout {
    if area.width() == 0 || area.height() == 0 {
        return ClusterLayout {
            core: Rect::new(area.left(), area.top(), 0, 0),
            sats: Vec::new(),
            spine_col: area.left(),
        };
    }
    let (sats_n, sat_h, core_h) = cluster_tier(area.height(), satellite_count);
    let stack_h = if sats_n == 0 {
        0
    } else {
        sats_n as u16 * sat_h + (sats_n as u16 - 1)
    };
    let block_h = stack_h.max(core_h);
    let block_w = SAT_W + 1 + CORE_W;
    let left = area.left() + (area.width().saturating_sub(block_w)) / 2;
    let top = area.top() + (area.height().saturating_sub(block_h)) / 2;

    let sat_top0 = top + (block_h.saturating_sub(stack_h)) / 2;
    let mut sats = Vec::with_capacity(sats_n);
    for i in 0..sats_n {
        let y = sat_top0 + i as u16 * (sat_h + 1);
        sats.push(Rect::new(left, y, SAT_W, sat_h));
    }

    let core_top = top + (block_h.saturating_sub(core_h)) / 2;
    let core = Rect::new(left + SAT_W + 1, core_top, CORE_W, core_h);

    ClusterLayout {
        core,
        sats,
        spine_col: left + SAT_W,
    }
}

/// The connector glyph for spine row `y`, chosen from the same cardinal-mask
/// table [`Panel`] uses for its own borders: `N`/`S` if the spine continues
/// above/below this row, `W` if a satellite's box meets the spine here,
/// `E` if the core's box meets the spine here.
const fn spine_glyph(mask: u8) -> char {
    if mask == 0 {
        return '\u{2500}'; // a lone connector row: a plain horizontal dash
    }
    BOX_SINGLE[(mask & 0x0F) as usize]
}

fn draw_spine(surface: &mut Surface<'_>, layout: &ClusterLayout, color: Color) {
    if layout.sats.is_empty() {
        return;
    }
    let sat_rows: Vec<u16> = layout
        .sats
        .iter()
        .map(|r| r.top() + r.height() / 2)
        .collect();
    let core_row = layout.core.top() + layout.core.height() / 2;
    let top = *sat_rows.iter().min().unwrap().min(&core_row);
    let bottom = *sat_rows.iter().max().unwrap().max(&core_row);
    let style = Style::new().fg(color).bg(rgb(9, 10, 15));

    for y in top..=bottom {
        let mut mask = 0u8;
        if y > top {
            mask |= N;
        }
        if y < bottom {
            mask |= S;
        }
        if sat_rows.contains(&y) {
            mask |= W;
        }
        if y == core_row {
            mask |= E;
        }
        surface.put((layout.spine_col, y), spine_glyph(mask), style);
    }
}

impl Demo for GriftParley {
    const NAME: &'static str = "63_grift_parley";
    const TITLE: &'static str = "63 Grift Parley";
    const BLURB: &'static str =
        "Griftlands: a negotiation board of linked argument entities, not a hand of cards.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-5/tap", "play a card"),
            ("Left/Right", "move cursor"),
            ("Enter", "confirm target"),
            ("M", "battle/negotiate"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        self.tick_clock(dt);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();

        let screen = term.area();
        let (title_area, content, status_area) = ui::split_chrome(screen);
        let shape = Shape::of(content);

        let hand_h = match shape {
            Shape::Desktop | Shape::Portrait => 9,
            Shape::Landscape => 6,
        }
        .min(content.height());
        let (upper, hand_area) = panel::split_bottom(content, hand_h);
        let log_h = if upper.height() >= 6 { 2 } else { 0 }.min(upper.height());
        let (board_area, log_area) = panel::split_bottom(upper, log_h);

        self.layout(hand_area, board_area, &gesture);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        match self.mode {
            Mode::Negotiate => self.draw_negotiate_board(&mut surface, board_area),
            Mode::Battle => self.draw_battle_board(&mut surface, board_area),
        }
        self.draw_log(&mut surface, log_area);
        self.draw_hand(&mut surface, hand_area);

        ui::title_bar::<Self>(&mut surface, title_area);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status_area, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(GriftParley);

#[cfg(test)]
mod tests {
    use super::{Argument, Cluster, GriftParley, MAX_SATS, TargetId, cluster_layout, cluster_tier};
    use retroglyph_core::Rect;

    #[test]
    fn hit_absorbs_with_composure_before_hp_unless_pierced() {
        let mut arg = Argument::core("Test", 10.0, 5.0);
        arg.hit(3.0, false);
        assert_eq!(arg.composure, 2.0);
        assert_eq!(arg.hp, 10.0);
        arg.hit(4.0, false);
        assert_eq!(arg.composure, 0.0);
        assert_eq!(arg.hp, 8.0);
        arg.hit(3.0, true);
        assert_eq!(arg.hp, 5.0, "a pierce hit must skip composure entirely");
    }

    #[test]
    fn hit_reports_the_kill_only_on_the_transition() {
        let mut arg = Argument::satellite("Sat", 4.0, None);
        assert!(!arg.hit(2.0, true));
        assert!(arg.hit(2.0, true));
        assert!(
            !arg.hit(1.0, true),
            "an already-dead argument cannot die again"
        );
    }

    #[test]
    fn cluster_cull_drops_only_the_dead() {
        let mut cluster = Cluster {
            core: Argument::core("Core", 10.0, 0.0),
            satellites: vec![
                Argument::satellite("A", 0.0, None),
                Argument::satellite("B", 3.0, None),
            ],
        };
        cluster.cull();
        assert_eq!(cluster.satellites.len(), 1);
        assert_eq!(cluster.satellites[0].name, "B");
    }

    #[test]
    fn cluster_tier_never_exceeds_the_configured_maximum() {
        let (sats, _, _) = cluster_tier(200, MAX_SATS + 5);
        assert!(sats <= MAX_SATS);
    }

    #[test]
    fn cluster_tier_degrades_to_something_that_fits_a_tiny_area() {
        let (sats, sat_h, core_h) = cluster_tier(4, 3);
        assert!(sat_h >= 1 && core_h >= 1);
        let _ = sats;
    }

    #[test]
    fn cluster_layout_places_every_satellite_inside_the_area() {
        let area = Rect::new(2, 3, 60, 20);
        let layout = cluster_layout(area, 3);
        assert!(layout.core.left() >= area.left() && layout.core.right() <= area.right());
        for rect in &layout.sats {
            assert!(rect.left() >= area.left() && rect.right() <= area.right());
            assert!(rect.top() >= area.top() && rect.bottom() <= area.bottom());
        }
    }

    #[test]
    fn cluster_layout_on_a_zero_area_returns_empty_geometry_without_panicking() {
        let layout = cluster_layout(Rect::new(0, 0, 0, 0), 3);
        assert_eq!(layout.core.width(), 0);
        assert!(layout.sats.is_empty());
    }

    #[test]
    fn target_id_variants_are_distinguishable() {
        assert_ne!(TargetId::Core, TargetId::Sat(0));
        assert_ne!(TargetId::Sat(0), TargetId::Sat(1));
    }

    #[test]
    fn a_reroll_keeps_the_active_mode() {
        let mut demo = GriftParley::from_seed(1);
        demo.mode = super::Mode::Battle;
        demo.reroll();
        assert_eq!(demo.mode, super::Mode::Battle);
    }
}
