//! 46: Party Pause -- Baldur's Gate II's real-time-with-pause, and the
//! portrait column that makes it manageable.
//!
//! BG2's whole tactical model rests on one control: the world runs
//! continuously until you hit pause, at which point everyone freezes and you
//! can queue an order for each party member, all of which fire together the
//! instant you resume. That is the element this demo builds around. The
//! other half of the same idea is the six-portrait column on the right: HP
//! printed above each bust, status pips below, and a red tide that rises up
//! the frame as a character takes damage, which is how the original game
//! lets you read the state of a fight without ever looking away from the
//! party sidebar. Everything else here -- the icon rail, the message line,
//! the class-specific action bar -- is present because the screenshot is,
//! but it stays quiet; the pause and the portraits are what the demo is for.
//!
//! Techniques on show:
//!
//! - **Freeze-and-queue orders** ([`PartyPause::simulate`],
//!   [`PartyPause::issue_order`]): tapping the ground or a hostile while
//!   running gives the selected character an immediate [`Order`]; the same
//!   tap while paused writes into `queued` instead and is drawn as a dim
//!   ghost marker on the map. Resuming (manually via the pause button, or
//!   automatically -- see below) moves every member's `queued` into `order`
//!   in one pass, which is what makes six separate taps during a pause
//!   resolve as one simultaneous volley on resume, exactly like the game.
//! - **An unattended pause/resume cycle** ([`PartyPause::tick`]): the demo
//!   flips between running and paused on its own timer and, on each auto
//!   pause, scripts one attack order for the (also auto-rotating) selected
//!   character, so a viewer who never touches the input still sees a queued
//!   ghost marker appear, the border go into its paused treatment, and the
//!   order fire and land as a stepped HP change on resume.
//! - **Health as a stepped quantity, not a tween**
//!   ([`PartyPause::resolve_strikes`]): HP only ever changes at the instant
//!   an attack order lands, by a fixed integer amount. A wound has a moment
//!   it happened, and pinning it to that moment is what keeps the number in
//!   the portrait column legible frame to frame -- see the gallery brief's
//!   warning about smooth interpolation on text. The color wash that climbs
//!   the portrait from the bottom is free to animate continuously; the
//!   digits next to it never do.
//! - **Multi-line bust portraits** ([`BUST`], [`PartyPause::draw_portrait`]):
//!   six hand-authored 3-row ASCII faces, distinguishable by silhouette
//!   (helm, pointed ears, a hood's shadow, a braided beard, a narrower hood,
//!   wings) rather than by a palette swap on one glyph.
//! - **Tap-select, drag-to-reorder on the same control**
//!   ([`PartyPause::apply_gesture`]): a tap on a portrait selects that
//!   character (an edge, from [`touch::Gesture::tap`]); a press that starts
//!   on one portrait and releases over another swaps their order in the
//!   party line, using the drop position rather than the live drag position
//!   so the swap commits once, not once per frame it drifts past a
//!   neighbour.
//! - **[`Shape`]-driven reflow between two silhouettes, not three**
//!   ([`PartyPause::draw`]): desktop and landscape keep the screenshot's
//!   side columns (icon rail left, portraits right, full height); portrait
//!   phone -- and, just as importantly, the 80x24 headless grid the snapshot
//!   test actually runs at, which is shorter than six stacked portraits ever
//!   fit -- collapse to one stacked column with the rail and the portraits
//!   both turned sideways into rows. The switch is driven by measuring
//!   whether six portraits or seven rail buttons actually fit the live
//!   `content` rect, not by guessing a device class from its aspect ratio.
//! - **Torchlight and drifting spell washes** ([`PartyPause::draw_play_area`]):
//!   ambient light and colour blooms computed per cell from distance and a
//!   slow phase offset, so the play area is never a static screenshot even
//!   while every order is frozen mid-pause.
//!
//! ```sh
//! cargo run --example 46_party_pause --features crossterm
//! cargo run --example 46_party_pause --features software
//! cargo run --example 46_party_pause --features gl
//! cargo run --example 46_party_pause  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb, scale};

/// How many characters make up the party. Six is the number BG2 itself uses,
/// and the portrait math below (bust rows, fallback thresholds) is built
/// around it rather than being generic over an arbitrary roster size.
const PARTY_SIZE: usize = 6;

/// The play area's abstract coordinate space, independent of how many cells
/// wide the panel actually is this frame. Positions are stored in this space
/// and mapped to screen cells at draw time, which is what lets the whole
/// scene rescale cleanly across [`Shape`]s instead of needing a scroll
/// camera.
const WORLD_W: f32 = 64.0;
/// See [`WORLD_W`].
const WORLD_H: f32 = 28.0;

/// World-units per second a character covers while walking an order.
const MOVE_SPEED: f32 = 14.0;
/// How close an attacker must get to a hostile before the strike resolves.
const ATTACK_RANGE: f32 = 4.0;
/// Fixed damage dealt by one resolved strike. A constant, not a roll: combat
/// here exists to make the portrait column's HP fill move, not to be a real
/// combat system, so there is nothing to gain from adding variance.
const STRIKE_DAMAGE: f32 = 9.0;
/// World-seconds a slain hostile stays down before it is restored to full
/// health, so the demo has something to strike again on the next auto cycle.
const RESPAWN_SECONDS: f32 = 6.0;

/// World-seconds the auto-cycle stays in "running" before it pauses itself.
const AUTO_RUN_SECONDS: f32 = 8.0;
/// World-seconds the auto-cycle stays paused before it resumes itself.
const AUTO_PAUSE_SECONDS: f32 = 3.4;

/// How fast an idle (order-less) character eases toward its formation slot.
/// Applied as `pos += (target - pos) * (rate * dt).min(1.0)`, an exponential
/// approach rather than a fixed step, so idle drift never overshoots and
/// never needs its own arrival check.
const FOLLOW_RATE: f32 = 2.2;

/// Left icon rail width when drawn as a vertical column.
const RAIL_W: u16 = 9;
/// Icon rail height when drawn as a horizontal row (portrait phone, or any
/// shape too short for the vertical column; see [`PartyPause::draw`]).
const RAIL_ROW_H: u16 = touch::TAP_H;
/// Portrait column width when drawn vertically.
const PORTRAIT_W: u16 = 15;
/// Portrait row height when drawn horizontally.
const PORTRAIT_ROW_H: u16 = touch::TAP_H;
/// Height of the single-line dialogue/combat message.
const MESSAGE_H: u16 = 1;
/// Height of the quickslot action bar (and its pause button).
const ACTION_BAR_H: u16 = touch::TAP_H;

/// A party member's class, which decides the five buttons in the action bar.
/// Real BG2 quickslots differ by class in exactly this way (see the research
/// brief): a fighter gets four weapon sets, a caster gets memorised spells
/// plus a "cast" catch-all, a thief gets its skill buttons. Six classes
/// across six party members means every portrait tap shows a genuinely
/// different action bar, which is the point of wiring class into the bar at
/// all rather than giving everyone the same five buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Fighter,
    Ranger,
    Mage,
    Cleric,
    Thief,
    Paladin,
}

impl Class {
    const fn label(self) -> &'static str {
        match self {
            Self::Fighter => "Fighter",
            Self::Ranger => "Ranger",
            Self::Mage => "Mage",
            Self::Cleric => "Cleric",
            Self::Thief => "Thief",
            Self::Paladin => "Paladin",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Fighter => rgb(196, 108, 84),
            Self::Ranger => rgb(120, 176, 108),
            Self::Mage => rgb(140, 120, 220),
            Self::Cleric => rgb(224, 196, 120),
            Self::Thief => rgb(120, 168, 196),
            Self::Paladin => rgb(226, 226, 196),
        }
    }

    /// The five quickslot buttons, as `(hotkey, label)`, from the manual's
    /// per-class layout (fighter: four weapon sets; caster: a weapon plus
    /// spells plus a cast-menu catch-all; thief: weapons plus its skills).
    const fn quickslots(self) -> [(&'static str, &'static str); 5] {
        match self {
            Self::Fighter => [
                ("F3", "Weap1"),
                ("F4", "Weap2"),
                ("F5", "Weap3"),
                ("F6", "Weap4"),
                ("F12", "Special"),
            ],
            Self::Ranger => [
                ("F3", "Weap1"),
                ("F4", "Weap2"),
                ("F5", "Stealth"),
                ("F8", "Item"),
                ("F12", "Special"),
            ],
            Self::Mage => [
                ("F3", "Weap1"),
                ("F4", "Spell1"),
                ("F5", "Spell2"),
                ("F6", "Spell3"),
                ("F7", "Cast"),
            ],
            Self::Cleric => [
                ("F3", "Weap1"),
                ("F4", "Turn"),
                ("F5", "Spell1"),
                ("F6", "Spell2"),
                ("F7", "Cast"),
            ],
            Self::Thief => [
                ("F3", "Weap1"),
                ("F4", "Weap2"),
                ("F5", "Traps"),
                ("F6", "Steal"),
                ("F7", "Hide"),
            ],
            Self::Paladin => [
                ("F3", "Weap1"),
                ("F4", "Weap2"),
                ("F5", "Weap3"),
                ("F6", "Turn"),
                ("F7", "Cast"),
            ],
        }
    }
}

/// One party member's 3-row bust, top to bottom: hair/hood silhouette, the
/// eyes/face line that carries the distinguishing detail, then jaw/collar.
/// Kept as plain ASCII (a couple of lines use a raw string purely to include
/// a literal backslash without an escape) rather than aiming for pixel-art
/// symmetry: the goal is a recognizable silhouette per class, not a portrait
/// that would survive being zoomed in on.
const BUST: [[&str; 3]; PARTY_SIZE] = [
    [" .-===-. ", "/  X-o  \\", " \\_____/ "], // Garrick: helm + scar
    [")   ,   (", "( o   o )", " \\  ~  / "],  // Aelathe: pointed ears
    [" ___^___ ", "/  . .  \\", " \\  ~  / "], // Neera: deep hood
    [" .-vvv-. ", "|  o o  |", " ~~~~~~~ "],   // Bronwyn: braided beard
    [" ,-----, ", "(  -- - )", " `-----` "],   // Silas: narrow hooded eyes
    [" <(-+-)> ", "/   Y   \\", " \\_____/ "], // Tavos: winged helm, holy symbol
];

/// Formation offsets (from the party's shared centre) for each roster slot,
/// a shallow V so six tokens never fully overlap on the map.
const FORMATION: [(f32, f32); PARTY_SIZE] = [
    (0.0, 0.0),
    (-3.4, 1.6),
    (3.4, 1.6),
    (-6.4, 3.2),
    (6.4, 3.2),
    (0.0, 4.6),
];

/// An order queued or in flight for one party member.
#[derive(Clone, Copy, PartialEq)]
enum Order {
    /// Walk to a world position and stop.
    Move(f32, f32),
    /// Walk into range of hostile `usize` and strike it once.
    Attack(usize),
}

/// One party member.
struct Member {
    name: &'static str,
    class: Class,
    hp: f32,
    hp_max: f32,
    pos: (f32, f32),
    /// In flight right now; drained by [`PartyPause::simulate`] while running.
    order: Option<Order>,
    /// Set by a tap while paused; ghosted on the map, and moved into `order`
    /// wholesale the moment the game resumes. This is the entire mechanic:
    /// see the module doc's first bullet.
    queued: Option<Order>,
}

impl Member {
    const fn hp_frac(&self) -> f32 {
        if self.hp_max <= 0.0 {
            0.0
        } else {
            self.hp / self.hp_max
        }
    }
}

/// A hostile token in the play area.
struct Hostile {
    home: (f32, f32),
    hp: f32,
    hp_max: f32,
    alive: bool,
    respawn: f32,
}

/// A drifting coloured spell-effect bloom over the play area.
struct Wash {
    pos: (f32, f32),
    vel: (f32, f32),
    age: f32,
    life: f32,
    color: Color,
    radius: f32,
}

/// One scripted line of dialogue/combat text, colored by whichever party
/// member (or the narrator, for unattributed lines) speaks it.
struct ScriptLine {
    speaker: &'static str,
    text: &'static str,
}

/// The scripted message reel the bottom line cycles through on its own
/// clock, independent of anything the player does -- BG2's dialogue line
/// narrates the party's state constantly, not only in response to input.
const SCRIPT: [ScriptLine; 7] = [
    ScriptLine {
        speaker: "NEERA",
        text: "Ready for anything.",
    },
    ScriptLine {
        speaker: "GARRICK",
        text: "Keep them off Neera's back.",
    },
    ScriptLine {
        speaker: "",
        text: "A cold wind rolls in from the treeline.",
    },
    ScriptLine {
        speaker: "BRONWYN",
        text: "The light of Ilmater guard us.",
    },
    ScriptLine {
        speaker: "SILAS",
        text: "I don't like this quiet.",
    },
    ScriptLine {
        speaker: "TAVOS",
        text: "Hold formation until my word.",
    },
    ScriptLine {
        speaker: "AELATHE",
        text: "Movement, west side, forty paces.",
    },
];

/// Left icon rail buttons, narrow and ornamental like the screenshot's menu
/// column: a name and a one-letter glyph, since the CP437 constraint rules
/// out most of the game's actual icon-font symbols.
const ICONS: [(&str, &str); 7] = [
    ("M", "Map"),
    ("J", "Journal"),
    ("I", "Inventory"),
    ("R", "Record"),
    ("S", "Spells"),
    ("Z", "Rest"),
    ("O", "Options"),
];

/// What tapping a hotspot means. `PlayAreaTap` is registered over the whole
/// play area first, so any token drawn on top of it can still win a more
/// specific action by being pushed afterward -- see [`Hotspots`]'s
/// last-registration-wins rule.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    SelectPortrait(usize),
    Icon(usize),
    Quickslot(usize),
    PauseToggle,
    PlayAreaTap,
    AttackHostile(usize),
}

/// State: the roster, the hostiles, the pause/queue machinery, the ambient
/// dressing (washes, message reel), and the touch plumbing every demo in
/// this gallery shares.
pub struct PartyPause {
    fps: FpsMeter,
    pointer: Pointer,
    hotspots: Hotspots<Action>,

    /// Elapsed seconds, always advancing: drives cosmetic animation (torch
    /// flicker, wash drift, the message reel) that must keep moving even
    /// while the game itself is paused.
    time: f32,
    /// Elapsed seconds only while running: drives the party's shared
    /// formation centre, which must hold perfectly still during a pause.
    motion_time: f32,
    paused: bool,
    /// Counts down to the next automatic pause/resume flip.
    auto_timer: f32,

    party: [Member; PARTY_SIZE],
    selected: usize,
    hostiles: [Hostile; 3],
    washes: Vec<Wash>,
    wash_timer: f32,
    wash_count: u32,

    message_idx: usize,
    message_timer: f32,

    /// Portrait rects from the last draw, keyed by roster slot, used to
    /// resolve a drag's start/end for the reorder gesture.
    portrait_rects: [Rect; PARTY_SIZE],
    /// Play area rect from the last draw, used to map a tap back into world
    /// coordinates for [`Action::PlayAreaTap`].
    play_area: Rect,
    reorder_from: Option<usize>,
}

impl Default for PartyPause {
    fn default() -> Self {
        let names = ["Garrick", "Aelathe", "Neera", "Bronwyn", "Silas", "Tavos"];
        let classes = [
            Class::Fighter,
            Class::Ranger,
            Class::Mage,
            Class::Cleric,
            Class::Thief,
            Class::Paladin,
        ];
        // Damaged by varying amounts already, so the portrait column shows
        // its fill-from-the-bottom effect from the very first frame rather
        // than only after the first scripted strike lands.
        let hp_fracs = [1.0f32, 0.82, 0.55, 0.94, 0.38, 0.7];

        let mut i = 0;
        let party = core::array::from_fn(|idx| {
            i = idx;
            let hp_max = 40.0;
            Member {
                name: names[idx],
                class: classes[idx],
                hp: hp_max * hp_fracs[idx],
                hp_max,
                pos: (
                    WORLD_W.mul_add(0.5, FORMATION[idx].0),
                    WORLD_H.mul_add(0.55, FORMATION[idx].1),
                ),
                order: None,
                queued: None,
            }
        });
        let _ = i;

        let hostile_homes = [
            (WORLD_W * 0.18, WORLD_H * 0.3),
            (WORLD_W * 0.82, WORLD_H * 0.28),
            (WORLD_W * 0.5, WORLD_H * 0.85),
        ];
        let hostiles = core::array::from_fn(|idx| Hostile {
            home: hostile_homes[idx],
            hp: 30.0,
            hp_max: 30.0,
            alive: true,
            respawn: 0.0,
        });

        Self {
            fps: FpsMeter::new(),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            time: 0.0,
            motion_time: 0.0,
            paused: false,
            auto_timer: AUTO_RUN_SECONDS,
            party,
            selected: 0,
            hostiles,
            washes: Vec::new(),
            wash_timer: 0.0,
            wash_count: 0,
            message_idx: 0,
            message_timer: 3.2,
            portrait_rects: [Rect::new(0, 0, 0, 0); PARTY_SIZE],
            play_area: Rect::new(0, 0, 0, 0),
            reorder_from: None,
        }
    }
}

impl PartyPause {
    // -- Pause / order machinery -----------------------------------------

    /// Enters the paused state. Orders already in flight stay in flight
    /// (freezing mid-stride is the whole visual point); only new orders from
    /// here on are diverted into `queued`.
    fn enter_pause(&mut self) {
        self.paused = true;
        self.auto_timer = AUTO_PAUSE_SECONDS;
        // Script one attack order during the pause so an unattended viewer
        // sees a ghost marker appear and then fire on the next resume, not
        // only when a real tap happens to land during this phase.
        self.selected = (self.selected + 1) % PARTY_SIZE;
        let target = self.next_alive_hostile(0);
        if let Some(target) = target {
            self.party[self.selected].queued = Some(Order::Attack(target));
        }
    }

    /// Leaves the paused state, moving every member's queued order into its
    /// active one. This one function is the entire "orders fire together on
    /// resume" mechanic: it does not matter whether zero, one, or six
    /// members have a queued order, or whether the resume was a manual tap
    /// on the pause button or the automatic cycle -- the same drain runs
    /// either way.
    fn resume(&mut self) {
        self.paused = false;
        self.auto_timer = AUTO_RUN_SECONDS;
        for member in &mut self.party {
            if let Some(order) = member.queued.take() {
                member.order = Some(order);
            }
        }
    }

    fn toggle_pause(&mut self) {
        if self.paused {
            self.resume();
        } else {
            self.enter_pause();
        }
    }

    /// Records `order` for the selected character: queued while paused
    /// (ghosted on the map, nothing moves yet), issued immediately while
    /// running.
    const fn issue_order(&mut self, order: Order) {
        let member = &mut self.party[self.selected];
        if self.paused {
            member.queued = Some(order);
        } else {
            member.order = Some(order);
        }
    }

    fn next_alive_hostile(&self, after: usize) -> Option<usize> {
        (0..self.hostiles.len())
            .map(|i| (after + i) % self.hostiles.len())
            .find(|&i| self.hostiles[i].alive)
    }

    // -- Simulation ---------------------------------------------------------

    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        self.advance_message(dt);
        self.advance_washes(dt);

        self.auto_timer -= dt;
        if self.auto_timer <= 0.0 {
            self.toggle_pause();
        }

        for hostile in &mut self.hostiles {
            if !hostile.alive {
                hostile.respawn -= dt;
                if hostile.respawn <= 0.0 {
                    hostile.alive = true;
                    hostile.hp = hostile.hp_max;
                }
            }
        }

        if self.paused {
            // Frozen: orders in flight hold their position, idle members do
            // not ease toward the formation centre either, so the whole
            // scene genuinely stops rather than merely stopping issuing new
            // orders.
            return;
        }
        self.motion_time += dt;

        let center = Self::formation_center(self.motion_time);
        let hostile_positions: [(f32, f32); 3] =
            core::array::from_fn(|i| self.hostiles[i].pos_now(self.motion_time));

        // Collected rather than resolved inline: a strike needs `&mut self`
        // (it touches the hostile roster and spawns a wash), which cannot
        // happen while `self.party` is still borrowed mutably by this loop.
        let mut strikes: Vec<(usize, usize)> = Vec::new();

        for (idx, member) in self.party.iter_mut().enumerate() {
            match member.order {
                Some(Order::Move(tx, ty)) => {
                    if step_toward(&mut member.pos, (tx, ty), MOVE_SPEED * dt) {
                        member.order = None;
                    }
                }
                Some(Order::Attack(target)) => {
                    if self.hostiles[target].alive {
                        let waypoint = hostile_positions[target];
                        let range = distance(member.pos, waypoint);
                        if range <= ATTACK_RANGE {
                            member.order = None;
                            strikes.push((idx, target));
                        } else {
                            step_toward(&mut member.pos, waypoint, MOVE_SPEED * dt);
                        }
                    } else {
                        member.order = None;
                    }
                }
                None => {
                    let slot = FORMATION[idx];
                    let home = (center.0 + slot.0, center.1 + slot.1);
                    let follow = (FOLLOW_RATE * dt).min(1.0);
                    member.pos.0 = (home.0 - member.pos.0).mul_add(follow, member.pos.0);
                    member.pos.1 = (home.1 - member.pos.1).mul_add(follow, member.pos.1);
                }
            }
        }

        for (attacker, target) in strikes {
            self.resolve_strike(attacker, target);
        }
    }

    /// Applies [`STRIKE_DAMAGE`] once. The one place HP ever changes: a
    /// single fixed step at a single identifiable moment (an order landing),
    /// never a per-frame drift -- see the module doc on stepped HP.
    fn resolve_strike(&mut self, attacker: usize, target: usize) {
        let hostile = &mut self.hostiles[target];
        hostile.hp = (hostile.hp - STRIKE_DAMAGE).max(0.0);
        if hostile.hp <= 0.0 {
            hostile.alive = false;
            hostile.respawn = RESPAWN_SECONDS;
        }
        let attacker_name = self.party[attacker].name;
        self.wash_timer = 0.0; // a strike spawns its own bloom immediately
        self.spawn_wash(
            self.hostiles[target].pos_now(self.motion_time),
            rgb(226, 96, 84),
        );
        let _ = attacker_name;
    }

    /// The party's shared formation centre: a slow Lissajous drift standing
    /// in for "the party is walking through the night on patrol" whenever no
    /// one has an active order, frozen at whatever `motion_time` last
    /// reached when the game is paused (since `motion_time` itself stops
    /// advancing then).
    fn formation_center(motion_time: f32) -> (f32, f32) {
        let cx = (WORLD_W * 0.5).mul_add(1.0, (WORLD_W * 0.16) * (motion_time * 0.12).cos());
        let cy = (WORLD_H * 0.55).mul_add(1.0, (WORLD_H * 0.12) * (motion_time * 0.19).sin());
        (cx, cy)
    }

    fn advance_message(&mut self, dt: f32) {
        self.message_timer -= dt;
        if self.message_timer <= 0.0 {
            self.message_timer = 3.6;
            self.message_idx = (self.message_idx + 1) % SCRIPT.len();
        }
    }

    fn advance_washes(&mut self, dt: f32) {
        self.wash_timer -= dt;
        if self.wash_timer <= 0.0 {
            self.wash_timer = 4.5;
            let colors = [rgb(120, 90, 210), rgb(90, 180, 210), rgb(200, 160, 70)];
            let color = colors[self.wash_count as usize % colors.len()];
            let x = WORLD_W * 0.6f32.mul_add(hash01(0x5A11, self.wash_count as i32, 0), 0.2);
            let y = WORLD_H * 0.6f32.mul_add(hash01(0x5A12, self.wash_count as i32, 1), 0.2);
            self.spawn_wash((x, y), color);
        }
        for wash in &mut self.washes {
            wash.age += dt;
            wash.pos.0 = wash.vel.0.mul_add(dt, wash.pos.0);
            wash.pos.1 = wash.vel.1.mul_add(dt, wash.pos.1);
        }
        self.washes.retain(|w| w.age < w.life);
    }

    fn spawn_wash(&mut self, pos: (f32, f32), color: Color) {
        self.wash_count += 1;
        let angle = hash01(0x9A00, self.wash_count as i32, 0) * core::f32::consts::TAU;
        self.washes.push(Wash {
            pos,
            vel: (angle.cos() * 1.4, angle.sin() * 1.4),
            age: 0.0,
            life: 6.0,
            color,
            radius: 4.5,
        });
    }

    // -- Input ---------------------------------------------------------

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
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Tab => self.selected = (self.selected + 1) % PARTY_SIZE,
            KeyCode::Char(c @ '1'..='6') => {
                self.selected = (c as u8 - b'1') as usize;
            }
            KeyCode::Char('a' | 'A') => {
                if let Some(target) = self.next_alive_hostile(0) {
                    self.issue_order(Order::Attack(target));
                }
            }
            KeyCode::Char('g' | 'G') => {
                self.issue_order(Order::Move(WORLD_W * 0.5, WORLD_H * 0.5));
            }
            _ => {}
        }
    }

    /// Resolves this frame's pointer gesture: a tap fires whatever hotspot
    /// (or map token) it landed on; a press that started over one portrait
    /// and dropped over another swaps their roster slots.
    fn apply_gesture(&mut self, gesture: Gesture) {
        if let Some(pos) = gesture.press
            && self.reorder_from.is_none()
        {
            self.reorder_from = self.portrait_index_at(pos);
        }

        if let Some(pos) = gesture.tap {
            if let Some(&action) = self.hotspots.hit(pos) {
                self.apply_action(action, pos);
            }
            self.reorder_from = None;
        }

        if let Some(pos) = gesture.drop {
            if let Some(from) = self.reorder_from
                && let Some(to) = self.portrait_index_at(pos)
                && from != to
            {
                self.party.swap(from, to);
                if self.selected == from {
                    self.selected = to;
                } else if self.selected == to {
                    self.selected = from;
                }
            }
            self.reorder_from = None;
        }
    }

    fn portrait_index_at(&self, pos: Pos) -> Option<usize> {
        self.portrait_rects.iter().position(|r| r.contains_pos(pos))
    }

    fn apply_action(&mut self, action: Action, pos: Pos) {
        match action {
            Action::SelectPortrait(i) => self.selected = i,
            Action::PauseToggle => self.toggle_pause(),
            Action::Icon(_) | Action::Quickslot(_) => {
                // Ornamental in this demo: the icon rail and the quickslots
                // exist to complete the screenshot's information hierarchy,
                // not to open real sub-screens. Neither needs the tap
                // position, so both fall through with no further effect.
            }
            Action::AttackHostile(i) => self.issue_order(Order::Attack(i)),
            Action::PlayAreaTap => {
                if let Some(world) = self.screen_to_world(pos) {
                    self.issue_order(Order::Move(world.0, world.1));
                }
            }
        }
    }

    /// Maps a tap's screen position back into world space through the play
    /// area rect recorded at the last draw (see [`Self::world_to_screen`] for
    /// the forward direction both share).
    fn screen_to_world(&self, pos: Pos) -> Option<(f32, f32)> {
        let area = self.play_area;
        if area.width() == 0 || area.height() == 0 || !area.contains_pos(pos) {
            return None;
        }
        let rx = f32::from(pos.x - area.left()) / f32::from(area.width());
        let ry = f32::from(pos.y - area.top()) / f32::from(area.height());
        Some((rx * WORLD_W, ry * WORLD_H))
    }

    fn status_line(&self) -> String {
        let member = &self.party[self.selected];
        format!(
            "{}  selected: {} ({})  hostiles: {}",
            if self.paused { "PAUSED" } else { "running" },
            member.name,
            member.class.label(),
            self.hostiles.iter().filter(|h| h.alive).count()
        )
    }
}

/// Steps `pos` toward `target` by at most `max_step`, returning `true` once
/// it has arrived (and snapping exactly onto `target` on that frame, so an
/// order never lingers one pixel short of "arrived").
fn step_toward(pos: &mut (f32, f32), target: (f32, f32), max_step: f32) -> bool {
    let dx = target.0 - pos.0;
    let dy = target.1 - pos.1;
    let dist = dx.hypot(dy);
    if dist <= max_step || dist < 1e-4 {
        *pos = target;
        true
    } else {
        let t = max_step / dist;
        pos.0 = dx.mul_add(t, pos.0);
        pos.1 = dy.mul_add(t, pos.1);
        false
    }
}

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

impl Hostile {
    /// Position including a small ambient sway around `home`, frozen at
    /// whatever `motion_time` last was while paused (same trick as
    /// [`PartyPause::formation_center`]).
    fn pos_now(&self, motion_time: f32) -> (f32, f32) {
        if !self.alive {
            return self.home;
        }
        let sway_x = motion_time.mul_add(0.5, self.home.0).sin() * 1.6;
        let sway_y = motion_time.mul_add(0.4, self.home.1).cos() * 1.1;
        (self.home.0 + sway_x, self.home.1 + sway_y)
    }
}

// ---------------------------------------------------------------------------
// Layout and drawing
// ---------------------------------------------------------------------------

impl PartyPause {
    fn draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(content);

        // The vertical portrait column needs six slots of at least
        // `TAP_H` rows each; the vertical rail needs seven buttons of the
        // same. Below either threshold -- which includes every portrait
        // phone, but also the 80x24 headless grid the snapshot test actually
        // renders at -- both flip to a horizontal row and the whole layout
        // stacks top to bottom instead of splitting into side columns. This
        // is a measurement of the live rect, not a guess from `shape` alone,
        // which is what keeps the 80x24 case from silently clipping content
        // the way a pure `Shape::Portrait` check would miss.
        let stacked = shape.stacks()
            || content.height() < PARTY_SIZE as u16 * touch::TAP_H
            || content.height() < ICONS.len() as u16 * touch::TAP_H;

        if stacked {
            self.draw_stacked(surface, content);
        } else {
            self.draw_side_columns(surface, content);
        }
    }

    fn draw_side_columns(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let (rail_area, rest) = panel::split_left(content, RAIL_W);
        let (rest, portrait_area) = panel::split_right(rest, PORTRAIT_W);
        self.draw_rail_vertical(surface, rail_area);
        self.draw_portraits_vertical(surface, portrait_area);

        let (top, bottom) = panel::split_bottom(rest, MESSAGE_H + ACTION_BAR_H);
        let (message_area, action_area) = panel::split_top(bottom, MESSAGE_H);
        self.play_area = top;
        self.draw_play_area(surface, top);
        self.draw_message(surface, message_area);
        self.draw_action_bar(surface, action_area);
    }

    fn draw_stacked(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let (rail_area, rest) = panel::split_top(content, RAIL_ROW_H);
        let (rest, bottom) = panel::split_bottom(rest, MESSAGE_H + PORTRAIT_ROW_H + ACTION_BAR_H);
        self.play_area = rest;
        self.draw_play_area(surface, rest);

        let (message_area, bottom) = panel::split_top(bottom, MESSAGE_H);
        let (portrait_area, action_area) = panel::split_top(bottom, PORTRAIT_ROW_H);
        self.draw_rail_horizontal(surface, rail_area);
        self.draw_message(surface, message_area);
        self.draw_portraits_horizontal(surface, portrait_area);
        self.draw_action_bar(surface, action_area);
    }

    // -- Icon rail --------------------------------------------------------

    fn draw_rail_vertical(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let n = ICONS.len() as u16;
        let h = (area.height() / n).max(1);
        for (i, (glyph, label)) in ICONS.iter().enumerate() {
            let y0 = area.top() + i as u16 * h;
            if y0 >= area.bottom() {
                break;
            }
            let rect = Rect::new(area.left(), y0, area.width(), h.min(area.bottom() - y0));
            Self::draw_icon_button(surface, rect, glyph, label, true);
            self.hotspots.push_tappable(rect, area, Action::Icon(i));
        }
    }

    fn draw_rail_horizontal(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, ICONS.len() as u16, 0);
        for (i, (glyph, label)) in ICONS.iter().enumerate() {
            Self::draw_icon_button(surface, cols[i], glyph, label, false);
            self.hotspots.push_tappable(cols[i], area, Action::Icon(i));
        }
    }

    fn draw_icon_button(
        surface: &mut Surface<'_>,
        rect: Rect,
        glyph: &str,
        label: &str,
        show_label: bool,
    ) {
        let bg = panel::PANEL_BG;
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let cy = rect.top() + rect.height() / 2;
        print_centered(
            surface,
            rect,
            cy.saturating_sub(u16::from(show_label)),
            glyph,
            ui::ACCENT,
            bg,
        );
        if show_label && rect.height() > 1 {
            print_centered(surface, rect, cy + 1, label, ui::DIM, bg);
        }
    }

    // -- Portraits ---------------------------------------------------------

    fn draw_portraits_vertical(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 {
            return;
        }
        let bust_rows: u16 = if area.height() >= PARTY_SIZE as u16 * 5 {
            3
        } else {
            2
        };
        let ph = bust_rows + 2;
        for i in 0..PARTY_SIZE {
            let y0 = area.top() + i as u16 * ph;
            if y0 + ph > area.bottom() {
                self.portrait_rects[i] = Rect::new(0, 0, 0, 0);
                continue;
            }
            let rect = Rect::new(area.left(), y0, area.width(), ph);
            self.portrait_rects[i] = rect;
            self.draw_portrait_vertical(surface, rect, i, bust_rows);
            self.hotspots
                .push_tappable(rect, area, Action::SelectPortrait(i));
        }
    }

    fn draw_portrait_vertical(
        &self,
        surface: &mut Surface<'_>,
        rect: Rect,
        i: usize,
        bust_rows: u16,
    ) {
        let member = &self.party[i];
        let selected = i == self.selected;
        let base_bg = if selected {
            scale(member.class.color(), 0.22)
        } else {
            panel::PANEL_BG
        };
        surface.fill_rect(rect, ' ', Style::new().bg(base_bg));

        let hp_text = format!("{:.0}/{:.0}", member.hp, member.hp_max);
        let hp_color = panel::threshold(member.hp_frac());
        print_centered(surface, rect, rect.top(), &hp_text, hp_color, base_bg);

        let missing = 1.0 - member.hp_frac();
        for row in 0..bust_rows {
            let y = rect.top() + 1 + row;
            let line = BUST[i][row as usize];
            // Row-from-bottom fraction of `missing` this row should carry:
            // the same bottom-up ramp `panel::draw_orb` uses for its fill,
            // applied here to a background tint instead of a glyph so the
            // bust art itself stays legible under the wash.
            let row_from_bottom = bust_rows - 1 - row;
            let row_t =
                (missing * f32::from(bust_rows) - f32::from(row_from_bottom)).clamp(0.0, 1.0);
            let row_bg = mix(base_bg, rgb(140, 30, 24), row_t * 0.8);
            let text = center_pad(line, rect.width());
            surface.print((rect.left(), y), &text, Style::new().fg(ui::FG).bg(row_bg));
            // fill the rest of the row's background past the printed text
            surface.fill_rect(
                Rect::new(rect.left(), y, rect.width(), 1),
                ' ',
                Style::new().bg(row_bg),
            );
            surface.print((rect.left(), y), &text, Style::new().fg(ui::FG).bg(row_bg));
        }

        let pip_y = rect.bottom() - 1;
        let queued_mark = if member.queued.is_some() { "Q!" } else { "" };
        let low_hp_mark = if member.hp_frac() < 0.3 { "!" } else { "" };
        let pips = format!("{queued_mark} {low_hp_mark}");
        panel::spans(
            surface,
            (rect.left() + 1, pip_y),
            rect.width().saturating_sub(2),
            &[
                Span::new(&member.name[..1], member.class.color()),
                Span::plain(" "),
                Span::new(&pips, ui::ACCENT),
            ],
            base_bg,
        );
    }

    fn draw_portraits_horizontal(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, PARTY_SIZE as u16, 0);
        for (i, rect) in cols.into_iter().enumerate() {
            self.portrait_rects[i] = rect;
            self.draw_portrait_horizontal(surface, rect, i);
            self.hotspots
                .push_tappable(rect, area, Action::SelectPortrait(i));
        }
    }

    fn draw_portrait_horizontal(&self, surface: &mut Surface<'_>, rect: Rect, i: usize) {
        let member = &self.party[i];
        let selected = i == self.selected;
        let bg = if selected {
            scale(member.class.color(), 0.24)
        } else {
            panel::PANEL_BG
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.height() == 0 {
            return;
        }
        let hp_text = format!("{:.0}/{:.0}", member.hp, member.hp_max);
        print_centered(
            surface,
            rect,
            rect.top(),
            &hp_text,
            panel::threshold(member.hp_frac()),
            bg,
        );
        if rect.height() > 1 {
            print_centered(surface, rect, rect.top() + 1, BUST[i][1], ui::FG, bg);
        }
        if rect.height() > 2 {
            let queued = if member.queued.is_some() { "Q!" } else { "" };
            print_centered(
                surface,
                rect,
                rect.top() + 2,
                &format!("{}{queued}", member.name),
                member.class.color(),
                bg,
            );
        }
    }

    // -- Message + action bar -----------------------------------------------

    fn draw_message(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        let line = &SCRIPT[self.message_idx];
        let color = self
            .party
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(line.speaker))
            .map_or(ui::DIM, |m| m.class.color());
        if line.speaker.is_empty() {
            surface.print(
                (area.left() + 1, area.top()),
                retroglyph_widgets::truncate(line.text, area.width_usize().saturating_sub(2)),
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
        } else {
            panel::spans(
                surface,
                (area.left() + 1, area.top()),
                area.width().saturating_sub(2),
                &[
                    Span::new(line.speaker, color),
                    Span::plain(": "),
                    Span::plain(line.text),
                ],
                panel::PANEL_BG,
            );
        }
    }

    fn draw_action_bar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let pause_w = touch::TAP_W.min(area.width() / 2).max(4);
        let (pause_rect, rest) = panel::split_left(area, pause_w);
        self.draw_pause_button(surface, pause_rect);
        self.hotspots
            .push_tappable(pause_rect, area, Action::PauseToggle);

        if rest.width() == 0 {
            return;
        }
        let slots = self.party[self.selected].class.quickslots();
        let cols = panel::columns(rest, slots.len() as u16, 0);
        for (i, (hotkey, label)) in slots.iter().enumerate() {
            let slot_rect = cols[i];
            let bg = scale(self.party[self.selected].class.color(), 0.16);
            surface.fill_rect(slot_rect, ' ', Style::new().bg(bg));
            let cy = slot_rect.top() + slot_rect.height() / 2;
            print_centered(
                surface,
                slot_rect,
                cy.saturating_sub(1),
                hotkey,
                ui::ACCENT,
                bg,
            );
            print_centered(surface, slot_rect, cy, label, ui::FG, bg);
            self.hotspots
                .push_tappable(slot_rect, rest, Action::Quickslot(i));
        }
    }

    /// The pause/resume button. The border treatment (double frame, filled
    /// red when paused) plus the literal `PAUSED` text is what the module
    /// doc calls "the paused state visually unmistakable": nothing else on
    /// screen changes shape when the game freezes, so this control has to
    /// carry the whole signal.
    fn draw_pause_button(&self, surface: &mut Surface<'_>, rect: Rect) {
        let (bg, fg, label) = if self.paused {
            (rgb(90, 24, 20), rgb(240, 200, 190), "PAUSED")
        } else {
            (rgb(20, 60, 28), rgb(200, 240, 210), "RUN")
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.width() < 2 || rect.height() < 2 {
            return;
        }
        let border = Border::Double;
        Panel::new()
            .border(border)
            .bg(bg)
            .frame(fg)
            .draw(surface, rect);
        let cy = rect.top() + rect.height() / 2;
        print_centered(surface, rect, cy, label, fg, bg);
    }

    // -- Play area -----------------------------------------------------

    fn draw_play_area(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let paused_border = if self.paused {
            rgb(196, 90, 84)
        } else {
            panel::FRAME
        };
        let inner = Panel::new()
            .title(if self.paused {
                "PAUSED"
            } else {
                "Twilight Reach"
            })
            .border(Border::Double)
            .frame(paused_border)
            .bg(rgb(8, 10, 16))
            .draw(surface, area);
        self.play_area = inner;
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        self.hotspots.push(inner, Action::PlayAreaTap);

        Self::draw_terrain(surface, inner);
        self.draw_torchlight(surface, inner);
        self.draw_washes(surface, inner);
        self.draw_ghost_orders(surface, inner);
        self.draw_hostiles(surface, inner);
        self.draw_party_tokens(surface, inner);
    }

    /// A static scatter of grass/undergrowth glyphs, placed by [`hash01`] on
    /// world-cell coordinates rather than screen cells, so the texture holds
    /// still under the camera's implicit rescale as the panel resizes
    /// between [`Shape`]s.
    fn draw_terrain(surface: &mut Surface<'_>, area: Rect) {
        let base = rgb(16, 22, 18);
        surface.fill_rect(area, ' ', Style::new().bg(base));
        for y in 0..area.height() {
            for x in 0..area.width() {
                let (wx, wy) = screen_to_world_cell(area, x, y);
                if hash01(0x7734, wx, wy) > 0.14 {
                    continue;
                }
                let glyph = if hash01(0x7735, wx, wy) > 0.5 {
                    ','
                } else {
                    '.'
                };
                let shade = 30 + (hash01(0x7736, wx, wy) * 30.0) as u8;
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(rgb(shade, shade + 20, shade)).bg(base),
                );
            }
        }
    }

    /// Radial torchlight pools around three fixed braziers, flickering on a
    /// per-torch phase so the night scene is never fully still even at the
    /// instant everything else is paused.
    fn draw_torchlight(&self, surface: &mut Surface<'_>, area: Rect) {
        let torches = [
            (WORLD_W * 0.12, WORLD_H * 0.5),
            (WORLD_W * 0.5, WORLD_H * 0.12),
            (WORLD_W * 0.88, WORLD_H * 0.6),
        ];
        for (ti, &(tx, ty)) in torches.iter().enumerate() {
            let ti_f = ti as f32;
            let freq = ti_f.mul_add(0.4, 2.3);
            let flicker = 0.16f32.mul_add(self.time.mul_add(freq, ti_f).sin(), 0.82);
            let Some((sx, sy)) = world_to_screen(area, (tx, ty)) else {
                continue;
            };
            let radius = 6.0 * flicker;
            let r0 = (radius) as i32;
            for dy in -r0..=r0 {
                for dx in -r0..=r0 {
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    if d > radius {
                        continue;
                    }
                    let px = i32::from(sx) + dx;
                    let py = i32::from(sy) + dy;
                    if px < i32::from(area.left())
                        || px >= i32::from(area.right())
                        || py < i32::from(area.top())
                        || py >= i32::from(area.bottom())
                    {
                        continue;
                    }
                    let t = (1.0 - d / radius).clamp(0.0, 1.0);
                    let glow = mix(rgb(8, 10, 16), rgb(214, 132, 54), t * 0.5);
                    surface.put((px as u16, py as u16), ' ', Style::new().bg(glow));
                }
            }
        }
    }

    fn draw_washes(&self, surface: &mut Surface<'_>, area: Rect) {
        for wash in &self.washes {
            let fade = 1.0 - (wash.age / wash.life);
            let Some((sx, sy)) = world_to_screen(area, wash.pos) else {
                continue;
            };
            let r = wash.radius as i32;
            for dy in -r..=r {
                for dx in -r..=r {
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    if d > wash.radius {
                        continue;
                    }
                    let px = i32::from(sx) + dx;
                    let py = i32::from(sy) + dy;
                    if px < i32::from(area.left())
                        || px >= i32::from(area.right())
                        || py < i32::from(area.top())
                        || py >= i32::from(area.bottom())
                    {
                        continue;
                    }
                    let t = (1.0 - d / wash.radius) * fade * 0.4;
                    if t <= 0.0 {
                        continue;
                    }
                    surface.put(
                        (px as u16, py as u16),
                        ' ',
                        Style::new().bg(mix(rgb(8, 10, 16), wash.color, t)),
                    );
                }
            }
        }
    }

    /// Dim ghost markers for every queued (not yet executed) order, the
    /// visual half of "orders queue up and all fire on resume": a marker at
    /// the destination, plus a faint dotted line from the character to it.
    fn draw_ghost_orders(&self, surface: &mut Surface<'_>, area: Rect) {
        if !self.paused {
            return;
        }
        for member in &self.party {
            let Some(order) = member.queued else { continue };
            let dest = match order {
                Order::Move(x, y) => (x, y),
                Order::Attack(h) => self.hostiles[h].pos_now(self.motion_time),
            };
            let steps: u16 = 6;
            for step in 1..steps {
                let t = f32::from(step) / f32::from(steps);
                let px = (dest.0 - member.pos.0).mul_add(t, member.pos.0);
                let py = (dest.1 - member.pos.1).mul_add(t, member.pos.1);
                if let Some((sx, sy)) = world_to_screen(area, (px, py)) {
                    let bg = area_bg(surface, sx, sy);
                    surface.put((sx, sy), '.', Style::new().fg(scale(ui::DIM, 1.4)).bg(bg));
                }
            }
            if let Some((sx, sy)) = world_to_screen(area, dest) {
                let bg = area_bg(surface, sx, sy);
                surface.put((sx, sy), 'O', Style::new().fg(ui::ACCENT).bg(bg));
            }
        }
    }

    fn draw_hostiles(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for i in 0..self.hostiles.len() {
            if !self.hostiles[i].alive {
                continue;
            }
            let pos = self.hostiles[i].pos_now(self.motion_time);
            let Some((sx, sy)) = world_to_screen(area, pos) else {
                continue;
            };
            draw_ring(surface, area, (sx, sy), rgb(196, 60, 54));
            surface.put(
                (sx, sy),
                'g',
                Style::new().fg(rgb(20, 8, 8)).bg(rgb(196, 60, 54)),
            );
            let token_rect = touch::tappable(Rect::new(sx, sy, 1, 1), area);
            self.hotspots.push(token_rect, Action::AttackHostile(i));
        }
    }

    fn draw_party_tokens(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for i in 0..PARTY_SIZE {
            let pos = self.party[i].pos;
            let Some((sx, sy)) = world_to_screen(area, pos) else {
                continue;
            };
            let ring = if i == self.selected {
                rgb(240, 240, 240)
            } else {
                rgb(90, 190, 110)
            };
            draw_ring(surface, area, (sx, sy), ring);
            let digit = char::from(b'1' + i as u8);
            surface.put((sx, sy), digit, Style::new().fg(rgb(10, 12, 10)).bg(ring));
            let token_rect = touch::tappable(Rect::new(sx, sy, 1, 1), area);
            self.hotspots.push(token_rect, Action::SelectPortrait(i));
        }
    }
}

/// Coarse world cell under screen offset `(x, y)`, at a resolution well
/// below one-cell-per-world-unit, so the terrain scatter reads as a stable
/// field rather than one dot per glyph. A free function (not a method): it
/// depends only on its arguments and the world constants, never on `self`.
fn screen_to_world_cell(area: Rect, x: u16, y: u16) -> (i32, i32) {
    let rx = f32::from(x) / f32::from(area.width().max(1));
    let ry = f32::from(y) / f32::from(area.height().max(1));
    ((rx * WORLD_W * 0.5) as i32, (ry * WORLD_H * 0.5) as i32)
}

/// Maps a world position to a screen cell inside `area`, or `None` if it
/// falls outside the panel (nothing to draw, and nothing to hit-test). See
/// [`screen_to_world_cell`] for why this is a free function.
fn world_to_screen(area: Rect, pos: (f32, f32)) -> Option<(u16, u16)> {
    if area.width() == 0 || area.height() == 0 {
        return None;
    }
    let rx = (pos.0 / WORLD_W).clamp(0.0, 0.999);
    let ry = (pos.1 / WORLD_H).clamp(0.0, 0.999);
    let x = area.left() + (rx * f32::from(area.width())) as u16;
    let y = area.top() + (ry * f32::from(area.height())) as u16;
    Some((x, y))
}

/// Reads back whatever background color is already at `(x, y)`, so an
/// overlay glyph (a ghost marker, a ring) blends with the torchlight/terrain
/// already drawn there instead of stamping a flat color over it.
fn area_bg(surface: &mut Surface<'_>, x: u16, y: u16) -> Color {
    let layer = surface.layer();
    surface
        .grid_mut()
        .tile(layer, (x, y))
        .map_or(rgb(8, 10, 16), |t| t.style().background())
}

/// A small colored halo under a token: a plus-shape of tinted background
/// cells one step out from the centre, standing in for the reference
/// screenshot's selection circles (white/green/red) at a scale a single
/// glyph cannot show on its own.
fn draw_ring(surface: &mut Surface<'_>, area: Rect, at: (u16, u16), color: Color) {
    let (cx, cy) = at;
    let offsets: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    for (dx, dy) in offsets {
        let px = i32::from(cx) + dx;
        let py = i32::from(cy) + dy;
        if px < i32::from(area.left())
            || px >= i32::from(area.right())
            || py < i32::from(area.top())
            || py >= i32::from(area.bottom())
        {
            continue;
        }
        let bg = area_bg(surface, px as u16, py as u16);
        surface.put(
            (px as u16, py as u16),
            ' ',
            Style::new().bg(mix(bg, color, 0.35)),
        );
    }
}

fn print_centered(surface: &mut Surface<'_>, rect: Rect, y: u16, text: &str, fg: Color, bg: Color) {
    if rect.height() == 0 {
        return;
    }
    let text = retroglyph_widgets::truncate(text, rect.width_usize());
    let len = text.chars().count() as u16;
    let pad = rect.width().saturating_sub(len) / 2;
    surface.print((rect.left() + pad, y), text, Style::new().fg(fg).bg(bg));
}

/// Centers `text` within `width` cells, returning an owned, padded string so
/// the caller can hand a whole row (including its trailing padding) to one
/// `print` call and get a bust line whose margins are its panel's own
/// background rather than leftover glyphs from whatever was drawn before it.
fn center_pad(text: &str, width: u16) -> String {
    let len = text.chars().count();
    let w = usize::from(width);
    if len >= w {
        return retroglyph_widgets::truncate(text, w).to_string();
    }
    let pad = (w - len) / 2;
    format!("{}{}{}", " ".repeat(pad), text, " ".repeat(w - len - pad))
}

impl Demo for PartyPause {
    const NAME: &'static str = "46_party_pause";
    const TITLE: &'static str = "46 Party Pause";
    const BLURB: &'static str =
        "Baldur's Gate II real-time-with-pause: portrait column and action bar.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Space", "pause/resume"),
            ("Tab/1-6", "select character"),
            ("A", "attack nearest hostile"),
            ("G", "move to centre"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();
        self.apply_gesture(gesture);

        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.draw(&mut surface, content);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(PartyPause);

#[cfg(test)]
mod tests {
    use super::PartyPause;
    use retroglyph_core::{Grid, Rect, Surface};

    /// Runs a demo instance through every [`super::Shape`] the gallery
    /// requires, plus the 80x24 grid the snapshot suite renders at (which is
    /// shorter than the vertical portrait column ever fits), and pins that
    /// each one draws without panicking. Mirrors the pattern
    /// `33_onebit_quest.rs` uses for the same reason: a single fixed-size
    /// snapshot cannot tell a reflow branch that silently draws nothing from
    /// one that draws the whole scene.
    #[test]
    fn every_shape_draws_something() {
        for (w, h) in [(80, 24), (73, 79), (158, 36), (160, 50)] {
            let mut grid = Grid::new(w, h);
            let mut demo = PartyPause::default();
            demo.simulate(1.5);
            let mut surface = Surface::new(&mut grid, Rect::new(0, 0, w, h), 0);
            demo.draw(&mut surface, Rect::new(0, 0, w, h));
        }
    }

    /// A queued order during a pause must survive into `order` on resume,
    /// which is the entire mechanic the module doc leads with.
    #[test]
    fn a_queued_order_fires_on_resume() {
        let mut demo = PartyPause::default();
        demo.enter_pause();
        demo.issue_order(super::Order::Move(10.0, 10.0));
        assert!(demo.party[demo.selected].queued.is_some());
        assert!(demo.party[demo.selected].order.is_none());
        demo.resume();
        assert!(demo.party[demo.selected].order.is_some());
        assert!(demo.party[demo.selected].queued.is_none());
    }
}
