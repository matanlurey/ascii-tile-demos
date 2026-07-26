//! 30: Fleet command -- a Crying Suns-style sector crawl paired with a live
//! squadron battle, the two halves of one bridge screen rather than two
//! separate demos.
//!
//! Crying Suns runs its whole campaign as an alternation between "where do we
//! go" (a node map, spend fuel, learn what a system holds) and "how do we
//! fight" (three lanes, rock-paper-scissors squadrons, a flagship with named
//! systems to knock out one at a time). This demo puts both on screen
//! together: exploring the sector map earns the scrap the battle spends on
//! reinforcements, and the battle runs continuously underneath whichever
//! decision the map is asking for. Neither panel is a mockup of the other;
//! both are live and share the same resource pool.
//!
//! Techniques on show:
//!
//! - **Multi-cell node boxes, not single glyphs**
//!   ([`FleetCommand::node_screen_rect`], [`FleetCommand::draw_node`]): a
//!   star system needs to show its label, its kind, whether it is in fuel
//!   range, and the cost to jump there, all at once and without a hover state
//!   -- four independent facts. A single colored glyph can carry at most one
//!   of those (color) before the player has to guess or tap to find out. A
//!   node the size of a real touch target ([`touch::TAP_W`]x[`touch::TAP_H`])
//!   has room to print all four, so the decision is legible before the tap,
//!   not after it.
//! - **Manhattan-routed travel lanes**
//!   ([`FleetCommand::draw_lane_connector`]): edges between node boxes are
//!   drawn with actual box-drawing corners (via the same [`mask4`]-style
//!   direction-to-glyph mapping the panel borders use), not a dotted
//!   approximation. A node map read as a graph needs its edges to look like
//!   wires, because "which lanes lead out of here" is exactly the question a
//!   captain choosing a jump is asking.
//! - **Rock-paper-scissors made readable in flight, not in a tooltip**
//!   ([`SquadronType::matchup`], [`FleetCommand::draw_squadron_token`]): each
//!   lane shows the enemy type it is currently defended by, and every
//!   friendly squadron token carries a live `+`/`=`/`-` suffix showing
//!   whether its matchup against that defender is winning, even, or losing,
//!   the whole time it is crossing the lane. The card that deploys it also
//!   states its counter in words ("beats bombers"). Between the static rule
//!   on the card and the dynamic indicator on the token, the matchup is
//!   never a fact you have to hold in your head or hover to recall.
//! - **[`ui::touch::Shape`]-driven reflow**: desktop and landscape show the
//!   sector map and the battle side by side, because both fit. Portrait
//!   cannot fit two independent screens' worth of a bordered map plus a
//!   three-lane battle plus a command deck without shrinking every panel
//!   below a legible size -- shrinking is not an option this gallery allows,
//!   see [`ui::card`] -- so portrait collapses to one screen at a time behind
//!   a bottom tab bar ([`FleetCommand::draw_tab_bar`]) instead. A tab switch
//!   costs one extra tap; a screen nobody can read costs the whole demo.
//! - **Drag-to-pan plus tap-to-select-then-tap-to-confirm**
//!   ([`ui::touch::Pointer`], [`FleetCommand::handle_gesture`]): panning the
//!   sector map is a drag, because a map is a large surface with an obvious
//!   destination for the gesture. Choosing a jump is tap-select (shows a
//!   detail panel and cost) then tap-confirm on a dedicated button, because a
//!   jump spends fuel permanently and cannot be undone -- see rule 7 in the
//!   brief. Both paths run through the one [`Pointer`](ui::touch::Pointer),
//!   which is what keeps a map drag from also firing whatever node the
//!   finger happened to end over.
//! - **Deterministic continuous simulation** ([`FleetCommand::simulate`]):
//!   squadrons advance, lane defenders rotate, and card cooldowns tick, all
//!   scaled by `frame.delta` and none of it seeded from anything but elapsed
//!   simulated time, so the battle animates on its own and renders
//!   identically across two runs fed the same deltas.
//!
//! ```sh
//! cargo run --example 30_fleet_command --features crossterm
//! cargo run --example 30_fleet_command --features software
//! cargo run --example 30_fleet_command --features gl
//! cargo run --example 30_fleet_command  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card::{self, Card, CardState};
use ascii_tile_demos::ui::panel::{self, Border, Log, Panel, Span};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb, scale};

/// Width of a sector node box: exactly [`touch::TAP_W`]. Anything narrower
/// fails the touch-target minimum; anything wider is space the label and
/// glyph don't need.
const NODE_W: u16 = touch::TAP_W;
/// Height of a sector node box: exactly [`touch::TAP_H`]. See [`NODE_W`].
const NODE_H: u16 = touch::TAP_H;

/// Horizontal spacing between sector map columns, in world cells.
///
/// Node width plus enough room for a lane connector's horizontal run and its
/// elbow to read clearly; too tight and a diagonal connector's corner glyphs
/// collide with the node border next to them.
const COL_DX: i32 = 17;
/// Vertical spacing between sector map rows, in world cells. See [`COL_DX`].
const ROW_DY: i32 = 8;

/// Scrap spent to deploy one squadron. A flat cost rather than per-type,
/// because the decision the player is making is *which lane*, not *which
/// type is cheaper* -- pricing them differently would smuggle in a second
/// decision axis the rock-paper-scissors matchup doesn't need.
const DEPLOY_COST: f32 = 3.0;

/// Seconds a squadron card stays disabled after deploying. Long enough that
/// spamming one lane is a real cost, short enough that a three-lane battle
/// never idles waiting on cooldowns.
const CARD_COOLDOWN: f32 = 4.0;

/// Seconds before a lane's defender type rotates to the next one in the
/// rock-paper-scissors cycle. Purely a function of elapsed simulated time
/// (see [`FleetCommand::simulate`]), never of wall-clock time, so two runs
/// fed identical deltas stay identical.
const DEFENDER_PERIOD: f32 = 7.0;

/// Fraction of a lane's length a squadron crosses per second at base speed.
/// ~4.5 seconds edge to edge, which is slow enough to watch a matchup play
/// out and fast enough that the lane never reads as stalled.
const SQUAD_SPEED: f32 = 0.22;

/// What a star system holds, shown by glyph and color rather than by a label
/// alone, since color is readable at the smaller [`card::Tier::Compact`]-ish
/// scale a dimmed/unreachable node still needs to communicate at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NodeKind {
    Resource,
    Distress,
    Hostile,
    Shop,
    Unknown,
}

impl NodeKind {
    const fn glyph(self) -> char {
        match self {
            Self::Resource => '$',
            Self::Distress => '!',
            Self::Hostile => '\u{263B}', // solid smiley: reads as hostile intent
            Self::Shop => '\u{00A7}',    // section sign: the closest CP437 has to a shop mark
            Self::Unknown => '?',
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Resource => "ORE",
            Self::Distress => "SOS",
            Self::Hostile => "HOST",
            Self::Shop => "DOCK",
            Self::Unknown => "SCAN",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Resource => rgb(96, 200, 214),
            Self::Distress => rgb(226, 184, 90),
            Self::Hostile => rgb(216, 88, 84),
            Self::Shop => rgb(140, 196, 158),
            Self::Unknown => rgb(150, 150, 178),
        }
    }
}

/// One star system: a label, a kind, and a position in world cells (see
/// [`COL_DX`]/[`ROW_DY`]), laid out in columns so the fleet's crossing of the
/// sector reads left to right.
struct SectorNode {
    label: &'static str,
    kind: NodeKind,
    col: i32,
    row: i32,
}

impl SectorNode {
    const fn origin(&self) -> (i32, i32) {
        (self.col * COL_DX, self.row * ROW_DY)
    }
}

/// A directed travel lane between two systems, and what it costs in fuel.
/// Directed rather than bidirectional: the fleet is crossing a dead empire on
/// one course, not shuttling back and forth, so every edge only ever runs
/// toward a higher column.
struct Edge {
    from: usize,
    to: usize,
    cost: u32,
}

/// The sector graph and the fleet's place in it.
struct SectorMap {
    nodes: Vec<SectorNode>,
    edges: Vec<Edge>,
    current: usize,
}

impl SectorMap {
    /// A fixed, hand-laid sector: one dock, three branching columns of
    /// systems, one flagship-adjacent terminus. Hand-authored rather than
    /// generated because the point on show is the box-and-lane rendering,
    /// not a graph generator, and a fixed layout is what keeps the demo's
    /// output identical run to run without needing a seeded RNG to prove it.
    fn new() -> Self {
        // `(label, kind, col, row)`: a flat tuple table reads as a
        // spreadsheet of the sector rather than as eleven nested struct
        // literals, which is the point -- the layout is the content here,
        // not the construction code.
        const RAW_NODES: [(&str, NodeKind, i32, i32); 11] = [
            ("DOCK", NodeKind::Shop, 0, 1),
            ("CETI", NodeKind::Resource, 1, 0),
            ("VESK", NodeKind::Hostile, 1, 1),
            ("NYX", NodeKind::Unknown, 1, 2),
            ("ORLA", NodeKind::Distress, 2, 0),
            ("THULE", NodeKind::Resource, 2, 1),
            ("KRAG", NodeKind::Shop, 2, 2),
            ("VAAL", NodeKind::Hostile, 3, 0),
            ("ESH", NodeKind::Unknown, 3, 1),
            ("IONE", NodeKind::Distress, 3, 2),
            ("OMEGA", NodeKind::Resource, 4, 1),
        ];
        // `(from, to, fuel cost)`, forward-only: the fleet is crossing the
        // sector on one course, never backtracking, so no edge ever points
        // to a lower column.
        const RAW_EDGES: [(usize, usize, u32); 16] = [
            (0, 1, 2),
            (0, 2, 1),
            (0, 3, 2),
            (1, 4, 2),
            (1, 5, 1),
            (2, 5, 2),
            (2, 6, 1),
            (3, 6, 2),
            (4, 7, 1),
            (4, 8, 2),
            (5, 8, 1),
            (5, 9, 2),
            (6, 9, 1),
            (7, 10, 2),
            (8, 10, 1),
            (9, 10, 2),
        ];
        let nodes = RAW_NODES
            .into_iter()
            .map(|(label, kind, col, row)| SectorNode {
                label,
                kind,
                col,
                row,
            })
            .collect();
        let edges = RAW_EDGES
            .into_iter()
            .map(|(from, to, cost)| Edge { from, to, cost })
            .collect();
        Self {
            nodes,
            edges,
            current: 0,
        }
    }

    /// The edge out of the current node leading to `to`, if one exists.
    fn edge_to(&self, to: usize) -> Option<&Edge> {
        self.edges
            .iter()
            .find(|e| e.from == self.current && e.to == to)
    }

    /// Every node directly reachable from the current one, in index order.
    fn reachable(&self) -> Vec<usize> {
        self.edges
            .iter()
            .filter(|e| e.from == self.current)
            .map(|e| e.to)
            .collect()
    }
}

/// A squadron's class. Three types in a closed rock-paper-scissors loop --
/// [`beats`](Self::beats) -- is the smallest matchup that makes lane
/// assignment a real decision: with only two types the counter is always the
/// same, and with no cycle at all there is a single dominant type and the
/// other two are unplayable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SquadronType {
    Fighter,
    Bomber,
    Armor,
}

/// The outcome of one type facing another, per [`SquadronType::matchup`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Matchup {
    Win,
    Neutral,
    Loss,
}

impl SquadronType {
    const fn glyph(self) -> char {
        match self {
            Self::Fighter => 'F',
            Self::Bomber => 'B',
            Self::Armor => 'A',
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Fighter => "Fighter",
            Self::Bomber => "Bomber",
            Self::Armor => "Armor",
        }
    }

    /// What this type counters, stated in words for the squadron card. This
    /// is the static half of the matchup; [`Self::matchup`] is the dynamic
    /// half a deployed token shows live. Together they are what makes the
    /// rock-paper-scissors readable without a tooltip: the card explains the
    /// rule once, the token confirms it is currently applying.
    const fn rule(self) -> &'static str {
        match self {
            Self::Fighter => "Beats bombers.",
            Self::Bomber => "Beats armor.",
            Self::Armor => "Beats fighters.",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Fighter => rgb(120, 190, 226),
            Self::Bomber => rgb(226, 150, 90),
            Self::Armor => rgb(160, 196, 120),
        }
    }

    const fn beats(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Fighter, Self::Bomber)
                | (Self::Bomber, Self::Armor)
                | (Self::Armor, Self::Fighter)
        )
    }

    const fn matchup(self, other: Self) -> Matchup {
        if matches!(
            (self, other),
            (Self::Fighter, Self::Fighter)
                | (Self::Bomber, Self::Bomber)
                | (Self::Armor, Self::Armor)
        ) {
            Matchup::Neutral
        } else if self.beats(other) {
            Matchup::Win
        } else {
            Matchup::Loss
        }
    }

    /// The next type in the cycle, for lane defender rotation.
    const fn next(self) -> Self {
        match self {
            Self::Fighter => Self::Bomber,
            Self::Bomber => Self::Armor,
            Self::Armor => Self::Fighter,
        }
    }
}

/// One of the flagship's three named systems, each tied to a lane: knocking
/// one out is the tactical payoff for winning that lane's matchup
/// repeatedly.
struct FlagshipSystem {
    name: &'static str,
    hp: f32,
    hp_max: f32,
}

/// The enemy flagship: overall hull plus one system per lane.
struct Flagship {
    hull: f32,
    hull_max: f32,
    systems: [FlagshipSystem; 3],
}

impl Flagship {
    const fn new() -> Self {
        Self {
            hull: 60.0,
            hull_max: 60.0,
            systems: [
                FlagshipSystem {
                    name: "POINT DEF",
                    hp: 30.0,
                    hp_max: 30.0,
                },
                FlagshipSystem {
                    name: "MAIN GUN",
                    hp: 30.0,
                    hp_max: 30.0,
                },
                FlagshipSystem {
                    name: "HANGAR",
                    hp: 30.0,
                    hp_max: 30.0,
                },
            ],
        }
    }
}

/// A lane's current defender and how long until it rotates.
struct Lane {
    defender: SquadronType,
    defender_timer: f32,
}

/// A deployed squadron in flight along its lane. `pos` runs `0.0` (just
/// launched) to `1.0` (reached the flagship).
struct Squadron {
    kind: SquadronType,
    lane: usize,
    pos: f32,
}

/// A card in the command deck: which type it deploys and its cooldown.
struct SquadronCard {
    kind: SquadronType,
    cooldown: f32,
}

/// An officer permanently assigned to buff one lane. Fixed assignments
/// rather than a roster to manage: the point on show is that a lane's
/// speed can be read from a badge next to it, not officer management.
struct Officer {
    name: &'static str,
    lane: usize,
    buff: &'static str,
}

/// The squadron battle: lanes, flagship, deployed squadrons, and the command
/// deck that feeds them.
struct Battle {
    lanes: [Lane; 3],
    flagship: Flagship,
    squadrons: Vec<Squadron>,
    cards: [SquadronCard; 3],
    officers: [Officer; 3],
    selected_card: Option<usize>,
}

impl Battle {
    fn new() -> Self {
        Self {
            lanes: [
                Lane {
                    defender: SquadronType::Bomber,
                    defender_timer: DEFENDER_PERIOD,
                },
                Lane {
                    defender: SquadronType::Armor,
                    defender_timer: DEFENDER_PERIOD * 1.3,
                },
                Lane {
                    defender: SquadronType::Fighter,
                    defender_timer: DEFENDER_PERIOD * 1.6,
                },
            ],
            flagship: Flagship::new(),
            squadrons: Vec::new(),
            cards: [
                SquadronCard {
                    kind: SquadronType::Fighter,
                    cooldown: 0.0,
                },
                SquadronCard {
                    kind: SquadronType::Bomber,
                    cooldown: 0.0,
                },
                SquadronCard {
                    kind: SquadronType::Armor,
                    cooldown: 0.0,
                },
            ],
            officers: [
                Officer {
                    name: "Cmdr. Reyes",
                    lane: 0,
                    buff: "+40% speed, Lane 1",
                },
                Officer {
                    name: "Lt. Okoro",
                    lane: 1,
                    buff: "+40% speed, Lane 2",
                },
                Officer {
                    name: "Ens. Vance",
                    lane: 2,
                    buff: "+40% speed, Lane 3",
                },
            ],
            selected_card: None,
        }
    }

    /// Selects card `i` if it is off cooldown, or deselects it if it is
    /// already selected. Tapping a card on cooldown does nothing: the card
    /// itself shows why (see [`FleetCommand::draw_squadron_cards`]).
    fn select_card(&mut self, i: usize) {
        let Some(card) = self.cards.get(i) else {
            return;
        };
        if card.cooldown > 0.0 {
            return;
        }
        self.selected_card = if self.selected_card == Some(i) {
            None
        } else {
            Some(i)
        };
    }

    /// Deploys the selected card's squadron into `lane`, if a card is
    /// selected and scrap covers the cost. Consumes the selection either
    /// way it fails past this point, so a mis-tapped lane doesn't leave a
    /// card silently armed for a later, unrelated tap.
    fn deploy(&mut self, lane: usize, scrap: &mut f32, log: &mut Log) {
        let Some(ci) = self.selected_card else { return };
        self.selected_card = None;
        let Some(card) = self.cards.get_mut(ci) else {
            return;
        };
        if card.cooldown > 0.0 {
            return;
        }
        if *scrap < DEPLOY_COST {
            log.push("Not enough scrap to deploy.", rgb(216, 88, 84));
            return;
        }
        *scrap -= DEPLOY_COST;
        let kind = card.kind;
        card.cooldown = CARD_COOLDOWN;
        self.squadrons.push(Squadron {
            kind,
            lane,
            pos: 0.0,
        });
        log.push(
            format!("{} launched into lane {}.", kind.name(), lane + 1),
            ui::ACCENT,
        );
    }

    /// Advances cooldowns, rotates lane defenders, and moves every squadron
    /// by `dt`, resolving impacts and losses. Purely a function of `dt`: no
    /// system time and no `HashMap` iteration order enters this, which is
    /// what keeps two renders of the same delta sequence identical.
    fn simulate(&mut self, dt: f32, log: &mut Log) {
        for card in &mut self.cards {
            card.cooldown = (card.cooldown - dt).max(0.0);
        }
        for lane in &mut self.lanes {
            lane.defender_timer -= dt;
            if lane.defender_timer <= 0.0 {
                lane.defender = lane.defender.next();
                lane.defender_timer = DEFENDER_PERIOD;
            }
        }

        let buffed_lanes: [bool; 3] =
            core::array::from_fn(|i| self.officers.iter().any(|o| o.lane == i));
        let mut destroyed = Vec::new();
        let mut impacts = Vec::new();
        for (i, sq) in self.squadrons.iter_mut().enumerate() {
            let speed = if buffed_lanes[sq.lane] {
                SQUAD_SPEED * 1.4
            } else {
                SQUAD_SPEED
            };
            sq.pos += speed * dt;
            let outcome = sq.kind.matchup(self.lanes[sq.lane].defender);
            // A losing matchup never reaches the flagship: it is shot down
            // partway, which is the visible cost of sending the wrong type
            // into a defended lane rather than a hidden roll.
            if outcome == Matchup::Loss && sq.pos >= 0.65 {
                destroyed.push(i);
            } else if sq.pos >= 1.0 {
                let dmg = if outcome == Matchup::Win { 10.0 } else { 5.0 };
                impacts.push((i, sq.lane, dmg));
            }
        }
        for (_, lane, dmg) in &impacts {
            self.apply_damage(*lane, *dmg, log);
        }
        let mut remove: Vec<usize> = destroyed
            .into_iter()
            .chain(impacts.into_iter().map(|(i, ..)| i))
            .collect();
        remove.sort_unstable();
        remove.dedup();
        for i in remove.into_iter().rev() {
            self.squadrons.remove(i);
        }
    }

    /// Applies `dmg` to lane `lane`'s system while it still has hit points,
    /// then spills over into the hull once the system is offline. A system
    /// that has already fallen silent no longer absorbs hits, so continuing
    /// to press an already-broken lane keeps paying off rather than hitting
    /// a wall.
    fn apply_damage(&mut self, lane: usize, dmg: f32, log: &mut Log) {
        let sys = &mut self.flagship.systems[lane];
        if sys.hp > 0.0 {
            let was_online = sys.hp > 0.0;
            sys.hp = (sys.hp - dmg).max(0.0);
            if was_online && sys.hp <= 0.0 {
                log.push(format!("{} disabled!", sys.name), rgb(226, 184, 90));
            }
        } else {
            self.flagship.hull = dmg.mul_add(-0.6, self.flagship.hull).max(0.0);
        }
    }
}

/// Which sector-map/battle screen a portrait layout is showing. Irrelevant
/// on desktop and landscape, which show both at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScreenTab {
    Map,
    Battle,
}

impl ScreenTab {
    const fn other(self) -> Self {
        match self {
            Self::Map => Self::Battle,
            Self::Battle => Self::Map,
        }
    }
}

/// What tapping a registered hotspot means, resolved once per frame in
/// [`FleetCommand::handle_gesture`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    SelectNode(usize),
    ConfirmJump,
    SelectCard(usize),
    DeployLane(usize),
    SwitchTab(ScreenTab),
}

/// State: the sector map, the battle, shared resources, and the touch/layout
/// plumbing every interface-scale demo needs.
pub struct FleetCommand {
    map: SectorMap,
    battle: Battle,
    fuel: f32,
    fuel_max: f32,
    scrap: f32,
    crew: u32,
    selected_node: Option<usize>,
    scroll: (i32, i32),
    map_rect: Rect,
    tab: ScreenTab,
    log: Log,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    time: f32,
    fps: FpsMeter,
}

impl Default for FleetCommand {
    fn default() -> Self {
        let mut log = Log::new(48);
        log.push("Fleet Command online. Course laid in.", ui::ACCENT);
        Self {
            map: SectorMap::new(),
            battle: Battle::new(),
            fuel: 10.0,
            fuel_max: 10.0,
            scrap: 8.0,
            crew: 4,
            selected_node: None,
            scroll: (0, 0),
            map_rect: Rect::new(0, 0, 0, 0),
            tab: ScreenTab::Map,
            log,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            time: 0.0,
            fps: FpsMeter::new(),
        }
    }
}

impl FleetCommand {
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
            KeyCode::Left => self.cycle_selection(-1),
            KeyCode::Right => self.cycle_selection(1),
            KeyCode::Enter => self.confirm_jump(),
            KeyCode::Backspace => self.selected_node = None,
            KeyCode::Char('1') => self.battle.select_card(0),
            KeyCode::Char('2') => self.battle.select_card(1),
            KeyCode::Char('3') => self.battle.select_card(2),
            KeyCode::Char('j' | 'J') => self.battle.deploy(0, &mut self.scrap, &mut self.log),
            KeyCode::Char('k' | 'K') => self.battle.deploy(1, &mut self.scrap, &mut self.log),
            KeyCode::Char('l' | 'L') => self.battle.deploy(2, &mut self.scrap, &mut self.log),
            KeyCode::Tab => self.tab = self.tab.other(),
            _ => {}
        }
    }

    /// Moves the highlighted-but-not-yet-armed selection to the next/previous
    /// reachable node. The keyboard path to the same tap-select step the
    /// touch flow uses, so arming a jump never requires a pointer.
    fn cycle_selection(&mut self, dir: i32) {
        let reachable = self.map.reachable();
        if reachable.is_empty() {
            return;
        }
        let idx = self
            .selected_node
            .and_then(|n| reachable.iter().position(|&r| r == n))
            .map_or(0, |p| {
                ((p as i32 + dir).rem_euclid(reachable.len() as i32)) as usize
            });
        self.selected_node = Some(reachable[idx]);
    }

    /// The second, explicit step of a jump: spends fuel and moves the fleet.
    /// Selecting a node ([`Action::SelectNode`]) only shows its detail panel;
    /// this is the only path that actually commits, matching rule 7 (an
    /// irreversible action needs a confirm, not a first tap).
    fn confirm_jump(&mut self) {
        let Some(target) = self.selected_node else {
            return;
        };
        let Some(cost) = self.map.edge_to(target).map(|e| e.cost) else {
            return;
        };
        if self.fuel < cost as f32 {
            self.log.push("Not enough fuel to jump.", rgb(216, 88, 84));
            return;
        }
        self.fuel -= cost as f32;
        self.map.current = target;
        self.selected_node = None;
        self.resolve_arrival(target);
    }

    /// Applies the kind-specific consequence of arriving at `idx`. Every
    /// kind alters exactly one shared resource, so a run at either panel is
    /// visibly funded or drained by decisions made at the other.
    fn resolve_arrival(&mut self, idx: usize) {
        let node = &self.map.nodes[idx];
        let msg = match node.kind {
            NodeKind::Resource => {
                self.scrap += 5.0;
                format!("{}: salvage recovered. +5 scrap.", node.label)
            }
            NodeKind::Shop => {
                self.fuel = (self.fuel + 3.0).min(self.fuel_max);
                format!("{}: refueled at dock. +3 fuel.", node.label)
            }
            NodeKind::Distress => {
                self.crew += 1;
                format!("{}: survivors taken aboard. +1 crew.", node.label)
            }
            NodeKind::Hostile => {
                // An ambush costs the fleet, not the enemy flagship (which
                // lives on the other screen and is only ever hurt by winning
                // a lane in the battle) -- fuel spent on evasive burns is
                // the resource that makes the most sense to lose here.
                self.fuel = (self.fuel - 2.0).max(0.0);
                format!("{}: ambushed en route. Evasive burn -2 fuel.", node.label)
            }
            NodeKind::Unknown => {
                if hash01(0x30FC, idx as i32, self.map.current as i32) > 0.5 {
                    self.scrap += 2.0;
                    format!("{}: derelict stripped. +2 scrap.", node.label)
                } else {
                    self.crew += 1;
                    format!("{}: stowaway recruited. +1 crew.", node.label)
                }
            }
        };
        self.log.push(msg, ui::FG);
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::SelectNode(i) => {
                if self.map.reachable().contains(&i) {
                    self.selected_node = Some(i);
                }
            }
            Action::ConfirmJump => self.confirm_jump(),
            Action::SelectCard(i) => self.battle.select_card(i),
            Action::DeployLane(lane) => self.battle.deploy(lane, &mut self.scrap, &mut self.log),
            Action::SwitchTab(tab) => self.tab = tab,
        }
    }

    /// Resolves this frame's pointer gesture against the hotspots this same
    /// frame's layout just registered, then applies drag panning.
    ///
    /// Panning only applies when the *press* originated inside the map
    /// viewport: [`Pointer`] already tells a tap from a drag, but a drag
    /// that started on a card and ended over the map must not also scroll
    /// the map, so the origin (not the current position) is what gates it.
    fn handle_gesture(&mut self, g: Gesture) {
        if let Some(origin) = self.pointer.press_origin()
            && self.map_rect.width() > 0
            && self.map_rect.contains_pos(origin)
            && (g.delta.0 != 0 || g.delta.1 != 0)
        {
            self.scroll.0 -= g.delta.0;
            self.scroll.1 -= g.delta.1;
            self.clamp_scroll();
        }
        if let Some(pos) = g.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.apply_action(action);
        }
    }

    /// Keeps the camera from wandering far enough that the whole sector
    /// scrolls out of view. Generous rather than exact: it does not depend
    /// on the current viewport width, only on the map's own extent, which is
    /// enough to stop a long drag from losing the map without needing to
    /// recompute the bound every layout pass.
    fn clamp_scroll(&mut self) {
        let max_col = self.map.nodes.iter().map(|n| n.col).max().unwrap_or(0);
        let max_row = self.map.nodes.iter().map(|n| n.row).max().unwrap_or(0);
        let map_w = max_col * COL_DX + i32::from(NODE_W);
        let map_h = max_row * ROW_DY + i32::from(NODE_H);
        let pad = 6;
        self.scroll.0 = self.scroll.0.clamp(-pad, (map_w - 12).max(-pad));
        self.scroll.1 = self.scroll.1.clamp(-pad, (map_h - 6).max(-pad));
    }

    /// Converts a world-space point to a screen cell inside `area`, or
    /// `None` if the point falls outside it. The one place scroll enters the
    /// map's drawing code, so panning is "change one offset" rather than a
    /// second coordinate system threaded through every draw call.
    fn to_screen(&self, area: Rect, wx: i32, wy: i32) -> Option<(u16, u16)> {
        let sx = i32::from(area.left()) + wx - self.scroll.0;
        let sy = i32::from(area.top()) + wy - self.scroll.1;
        if sx < i32::from(area.left()) || sy < i32::from(area.top()) {
            return None;
        }
        if sx >= i32::from(area.right()) || sy >= i32::from(area.bottom()) {
            return None;
        }
        Some((sx as u16, sy as u16))
    }

    /// The on-screen rect for node `idx`, or `None` if any part of it falls
    /// outside `area`. Nodes that are only partially visible are skipped
    /// rather than clipped: a half-drawn border reads as a rendering bug,
    /// while a node that simply isn't there yet reads as "pan to see more",
    /// which is the truth.
    fn node_screen_rect(&self, idx: usize, area: Rect) -> Option<Rect> {
        let (wx, wy) = self.map.nodes[idx].origin();
        let (sx, sy) = self.to_screen(area, wx, wy)?;
        let rect = Rect::new(sx, sy, NODE_W, NODE_H);
        if rect.right() > area.right() || rect.bottom() > area.bottom() {
            return None;
        }
        Some(rect)
    }

    // -- layout --------------------------------------------------------

    fn layout_wide(&mut self, surface: &mut Surface<'_>, body: Rect) {
        let battle_w = (body.width() * 2 / 5)
            .clamp(44, 72)
            .min(body.width().saturating_sub(40));
        let (map_area, battle_area) = panel::split_right(body, battle_w);
        self.draw_map_screen(surface, map_area);
        self.draw_battle_screen(surface, battle_area, Shape::Desktop);
    }

    fn layout_portrait(&mut self, surface: &mut Surface<'_>, body: Rect) {
        let (content, tabbar) = panel::split_bottom(body, touch::TAP_H + 1);
        match self.tab {
            ScreenTab::Map => {
                self.draw_map_screen(surface, content);
            }
            ScreenTab::Battle => {
                self.map_rect = Rect::new(0, 0, 0, 0);
                self.draw_battle_screen(surface, content, Shape::Portrait);
            }
        }
        self.draw_tab_bar(surface, tabbar);
    }

    fn draw_tab_bar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < touch::TAP_W * 2 {
            return;
        }
        let half = area.width() / 2;
        let tabs = [
            (
                Rect::new(area.left(), area.top(), half, area.height()),
                ScreenTab::Map,
                "MAP",
            ),
            (
                Rect::new(
                    area.left() + half,
                    area.top(),
                    area.width() - half,
                    area.height(),
                ),
                ScreenTab::Battle,
                "BATTLE",
            ),
        ];
        for (rect, tab, label) in tabs {
            let active = self.tab == tab;
            let accent = if active { ui::ACCENT } else { ui::DIM };
            Panel::new()
                .border(if active {
                    Border::Double
                } else {
                    Border::Single
                })
                .frame(accent)
                .focused(active)
                .draw(surface, rect);
            let text_x = rect.left() + rect.width().saturating_sub(label.len() as u16) / 2;
            surface.print(
                (text_x, rect.top() + rect.height() / 2),
                label,
                Style::new().fg(accent).bg(panel::PANEL_BG),
            );
            self.hotspots.push(rect, Action::SwitchTab(tab));
        }
    }

    // -- sector map ------------------------------------------------------

    fn draw_map_screen(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Sector Map")
            .border(Border::Double)
            .draw(surface, area);
        if inner.width() < NODE_W || inner.height() < NODE_H + 3 {
            self.map_rect = inner;
            return;
        }
        let detail_h = (NODE_H + 3).min(inner.height() / 3);
        let (map_view, detail_area) = panel::split_bottom(inner, detail_h);
        self.map_rect = map_view;
        self.draw_starfield(surface, map_view);
        self.draw_lanes_layer(surface, map_view);
        for i in 0..self.map.nodes.len() {
            self.draw_node(surface, map_view, i);
        }
        self.draw_node_detail(surface, detail_area);
    }

    /// A stable field of dim background stars, positioned by a hash of grid
    /// coordinates so it never reshuffles from one frame to the next; only
    /// twinkle phase drifts with elapsed time.
    fn draw_starfield(&self, surface: &mut Surface<'_>, area: Rect) {
        for y in 0..area.height() {
            for x in 0..area.width() {
                let (wx, wy) = (i32::from(x), i32::from(y));
                if hash01(0x5741, wx, wy) > 0.04 {
                    continue;
                }
                let phase = hash01(0x1357, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add((self.time.mul_add(0.5, phase)).sin(), 0.5);
                let v = 90.0f32.mul_add(twinkle, 40.0) as u8;
                surface.put(
                    (area.left() + x, area.top() + y),
                    '\u{00b7}',
                    Style::new()
                        .fg(rgb(v, v, v.saturating_add(30)))
                        .bg(rgb(4, 6, 14)),
                );
            }
        }
    }

    fn draw_lanes_layer(&self, surface: &mut Surface<'_>, area: Rect) {
        let reachable = self.map.reachable();
        for edge in &self.map.edges {
            let from = self.map.nodes[edge.from].origin();
            let to = self.map.nodes[edge.to].origin();
            let (fx, fy) = (from.0 + i32::from(NODE_W), from.1 + i32::from(NODE_H) / 2);
            let (tx, ty) = (to.0, to.1 + i32::from(NODE_H) / 2);
            let color = if edge.from == self.map.current && reachable.contains(&edge.to) {
                rgb(120, 196, 226)
            } else {
                rgb(48, 54, 74)
            };
            self.draw_lane_connector(surface, area, (fx, fy), (tx, ty), color);
        }
    }

    /// Draws one travel lane as a Manhattan path of real box-drawing glyphs
    /// between two world points, both already known (by the caller) to lie
    /// inside `area`.
    ///
    /// A single elbow (horizontal, corner, vertical, corner, horizontal)
    /// rather than a straight diagonal, because CP437 has no diagonal
    /// line-drawing glyphs; a Bresenham line built from `/` and `\` would
    /// also fail the "renders as a solid block outside CP437" constraint for
    /// nothing gained. The corner glyphs are chosen the same way a panel
    /// border chooses its corners: by which two cardinal directions meet
    /// there, so a lane reads as wiring rather than as a trail of dots.
    fn draw_lane_connector(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        from: (i32, i32),
        to: (i32, i32),
        color: Color,
    ) {
        let Some((x0, y0)) = self.to_screen(area, from.0, from.1) else {
            return;
        };
        let Some((x1, y1)) = self.to_screen(area, to.0, to.1) else {
            return;
        };
        let bg = rgb(4, 6, 14);
        let style = Style::new().fg(color).bg(bg);

        if y0 == y1 {
            for x in x0..=x1 {
                surface.put((x, y0), '\u{2500}', style);
            }
            return;
        }

        let midx = (x0 + 1)
            .max(u16::midpoint(x0, x1))
            .min(x1.saturating_sub(1))
            .max(x0);
        for x in x0..midx {
            surface.put((x, y0), '\u{2500}', style);
        }
        let (ylo, yhi) = (y0.min(y1), y0.max(y1));
        for y in ylo..=yhi {
            surface.put((midx, y), '\u{2502}', style);
        }
        for x in (midx + 1)..=x1 {
            surface.put((x, y1), '\u{2500}', style);
        }

        let down = y1 > y0;
        let top_corner = if down { '\u{2510}' } else { '\u{2518}' };
        let bottom_corner = if down { '\u{2514}' } else { '\u{250C}' };
        surface.put((midx, y0), top_corner, style);
        surface.put((midx, y1), bottom_corner, style);
    }

    /// Draws one sector node as a bordered, labelled box, never as a single
    /// glyph. See the module doc for why: a node has to carry its kind, its
    /// reachability, and (for reachable nodes) its fuel cost, all at once and
    /// without a hover state, and only a box big enough to hold real text can
    /// do that.
    fn draw_node(&mut self, surface: &mut Surface<'_>, area: Rect, idx: usize) {
        let Some(rect) = self.node_screen_rect(idx, area) else {
            return;
        };
        let node = &self.map.nodes[idx];
        let is_current = idx == self.map.current;
        let reachable = self.map.reachable();
        let is_reachable = reachable.contains(&idx);
        let is_selected = self.selected_node == Some(idx);

        let base = node.kind.color();
        let (frame, border) = if is_current {
            (ui::ACCENT, Border::Double)
        } else if is_selected {
            (mix(base, rgb(255, 255, 255), 0.4), Border::Double)
        } else if is_reachable {
            (base, Border::Single)
        } else {
            (scale(base, 0.4), Border::Single)
        };

        let badge = if is_current {
            "HERE"
        } else if is_selected {
            "SEL"
        } else {
            ""
        };
        let mut panel = Panel::new()
            .title(node.label)
            .border(border)
            .frame(frame)
            .bg(rgb(10, 12, 20));
        if !badge.is_empty() {
            panel = panel.badge(badge);
        }
        let inner = panel.draw(surface, rect);
        if inner.height() == 0 {
            return;
        }

        let text_color = if is_reachable || is_current {
            ui::FG
        } else {
            ui::DIM
        };
        let kind_line = format!("{} {}", node.kind.glyph(), node.kind.tag());
        surface.print(
            (inner.left(), inner.top()),
            &kind_line,
            Style::new().fg(text_color).bg(rgb(10, 12, 20)),
        );
        if inner.height() > 1 {
            let cost_line = if is_current {
                "CURRENT".to_string()
            } else if let Some(edge) = self.map.edge_to(idx) {
                format!("-{} FUEL", edge.cost)
            } else {
                String::new()
            };
            surface.print(
                (inner.left(), inner.top() + 1),
                truncate(&cost_line, inner.width_usize()),
                Style::new()
                    .fg(if is_reachable { ui::ACCENT } else { ui::DIM })
                    .bg(rgb(10, 12, 20)),
            );
        }

        if is_reachable {
            self.hotspots
                .push_tappable(rect, area, Action::SelectNode(idx));
        }
    }

    /// The bottom strip of the map screen: detail on the selected node plus
    /// the confirm control. Kept as its own always-visible band rather than a
    /// popup, since touch has no hover and the confirm button has to be a
    /// legal tap target regardless of where the node itself scrolled to.
    fn draw_node_detail(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new().title("Contact").draw(surface, area);
        if inner.width() < 4 || inner.height() == 0 {
            return;
        }
        let bg = panel::PANEL_BG;

        let Some(idx) = self.selected_node else {
            panel::spans(
                surface,
                (inner.left(), inner.top()),
                inner.width(),
                &[Span::dim("Tap a reachable node to inspect it.")],
                bg,
            );
            return;
        };
        let node = &self.map.nodes[idx];
        let cost = self.map.edge_to(idx).map_or(0, |e| e.cost);
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::keyword(node.label),
                Span::plain(" -- "),
                Span::new(node.kind.tag(), node.kind.color()),
            ],
            bg,
        );
        if inner.height() > 1 {
            let afford = self.fuel >= cost as f32;
            let text = if afford {
                format!("Jump cost: {cost} fuel.")
            } else {
                format!(
                    "Jump cost: {cost} fuel (short {:.0}).",
                    cost as f32 - self.fuel
                )
            };
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::new(
                    &text,
                    if afford { ui::DIM } else { rgb(216, 88, 84) },
                )],
                bg,
            );
        }

        if inner.height() >= 3 && inner.width() >= touch::TAP_W {
            let btn = Rect::new(
                inner.right().saturating_sub(touch::TAP_W),
                inner
                    .bottom()
                    .saturating_sub(touch::TAP_H)
                    .max(inner.top() + 2),
                touch::TAP_W,
                touch::TAP_H.min(inner.height().saturating_sub(2)),
            );
            let afford = self.fuel >= cost as f32;
            let accent = if afford { rgb(216, 88, 84) } else { ui::DIM };
            Panel::new()
                .border(Border::Double)
                .frame(accent)
                .bg(rgb(24, 12, 12))
                .draw(surface, btn);
            let label = "CONFIRM JUMP";
            let tx = btn.left() + btn.width().saturating_sub(label.len() as u16) / 2;
            surface.print(
                (tx, btn.top() + btn.height() / 2),
                truncate(label, btn.width_usize()),
                Style::new().fg(accent).bg(rgb(24, 12, 12)),
            );
            if afford {
                self.hotspots.push(btn, Action::ConfirmJump);
            }
        }
    }

    // -- squadron battle --------------------------------------------------

    fn draw_battle_screen(&mut self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let deck_h = if shape.stacks() {
            (area.height() * 2 / 5).clamp(10, 20)
        } else {
            (area.height() * 3 / 8).clamp(9, 16)
        };
        let deck_h = deck_h.min(area.height().saturating_sub(9));
        let (lanes_area, deck_area) = panel::split_bottom(area, deck_h);
        self.draw_lanes(surface, lanes_area);
        self.draw_command_deck(surface, deck_area, shape);
    }

    fn draw_lanes(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Squadron Battle")
            .border(Border::Double)
            .draw(surface, area);
        if inner.height() < 6 {
            return;
        }
        let lane_rects = rows(inner, 3);
        for (i, rect) in lane_rects.into_iter().enumerate() {
            self.draw_lane(surface, rect, i);
        }
    }

    /// Draws one lane: a header naming its system and current defender, a
    /// travel track drawn with the same box-drawing line the sector map
    /// uses, and every squadron currently in flight along it.
    ///
    /// Lanes claim their whole share of the battle panel's height rather
    /// than being drawn as thin one-row rules, so each is comfortably above
    /// the [`touch::TAP_H`] minimum: "tap a lane" has to mean "tap a
    /// generous strip", not "tap the one row a line happens to occupy".
    fn draw_lane(&mut self, surface: &mut Surface<'_>, rect: Rect, idx: usize) {
        if rect.height() < 2 || rect.width() < 12 {
            return;
        }
        let bg = rgb(8, 10, 18);
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let lane = &self.battle.lanes[idx];
        let sys = &self.battle.flagship.systems[idx];
        let buffed = self.battle.officers.iter().any(|o| o.lane == idx);
        let sys_online = sys.hp > 0.0;

        panel::spans(
            surface,
            (rect.left(), rect.top()),
            rect.width(),
            &[
                Span::keyword(&format!("LANE {}", idx + 1)),
                Span::plain(" "),
                Span::new(
                    sys.name,
                    if sys_online {
                        ui::DIM
                    } else {
                        rgb(216, 88, 84)
                    },
                ),
                Span::plain(if buffed { " *" } else { "" }),
            ],
            bg,
        );
        let bar_w = 10u16.min(rect.width().saturating_sub(28));
        if bar_w > 0 {
            panel::bar(
                surface,
                (rect.right() - bar_w - 6, rect.top()),
                bar_w,
                sys.hp / sys.hp_max,
                panel::threshold(sys.hp / sys.hp_max),
                rgb(30, 30, 36),
            );
        }
        let defender = lane.defender;
        let dtext = format!("[{}]", defender.glyph());
        surface.print(
            (rect.right().saturating_sub(4), rect.top()),
            &dtext,
            Style::new().fg(defender.color()).bg(bg),
        );

        if rect.height() < 3 {
            return;
        }
        let track = Rect::new(rect.left(), rect.top() + 1, rect.width(), rect.height() - 1);
        let mid_y = track.top() + track.height() / 2;
        surface.print(
            (track.left(), mid_y),
            "OUR",
            Style::new().fg(ui::DIM).bg(bg),
        );
        for x in (track.left() + 4)..track.right().saturating_sub(4) {
            surface.put(
                (x, mid_y),
                '\u{2500}',
                Style::new().fg(rgb(40, 46, 64)).bg(bg),
            );
        }
        surface.print(
            (track.right().saturating_sub(3), mid_y),
            "ENY",
            Style::new().fg(ui::DIM).bg(bg),
        );

        let lane_left = f32::from(track.left() + 4);
        let lane_right = f32::from(track.right().saturating_sub(4));
        for sq in self.battle.squadrons.iter().filter(|s| s.lane == idx) {
            let x = sq
                .pos
                .clamp(0.0, 1.0)
                .mul_add(lane_right - lane_left, lane_left);
            Self::draw_squadron_token(
                surface,
                (x as u16, mid_y),
                sq.kind,
                sq.kind.matchup(defender),
                bg,
            );
        }

        self.hotspots.push(rect, Action::DeployLane(idx));
    }

    /// Draws a deployed squadron as a 4-cell token: a bracketed type glyph
    /// plus a live matchup suffix. `+` means this squadron currently beats
    /// the lane's defender, `-` means it is losing and will be shot down
    /// before reaching the flagship, `=` means neither. This is the dynamic
    /// half of making rock-paper-scissors readable without a tooltip: the
    /// squadron card states the rule once at deploy time, and every token in
    /// flight keeps restating whether that rule is currently paying off, so
    /// the player never has to recall it from memory mid-battle.
    fn draw_squadron_token(
        surface: &mut Surface<'_>,
        at: (u16, u16),
        kind: SquadronType,
        outcome: Matchup,
        bg: Color,
    ) {
        let (suffix, suffix_color) = match outcome {
            Matchup::Win => ('+', rgb(120, 214, 120)),
            Matchup::Neutral => ('=', ui::DIM),
            Matchup::Loss => ('-', rgb(216, 88, 84)),
        };
        let text = format!("[{}]", kind.glyph());
        surface.print(at, &text, Style::new().fg(kind.color()).bg(bg));
        surface.put(
            (at.0 + 3, at.1),
            suffix,
            Style::new().fg(suffix_color).bg(bg),
        );
    }

    fn draw_command_deck(&mut self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let inner = Panel::new()
            .title("Command Deck")
            .border(Border::Single)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        if shape.stacks() {
            let cards_h = card::COMPACT_H.min(inner.height().saturating_sub(4)).max(3);
            let (cards_area, info_area) = panel::split_top(inner, cards_h);
            self.draw_squadron_cards(surface, cards_area);
            self.draw_flagship_info(surface, info_area);
        } else {
            let cards_w = (card::FULL_W + 1) * 3 + 1;
            let (cards_area, info_area) = panel::split_left(inner, cards_w.min(inner.width() / 2));
            self.draw_squadron_cards(surface, cards_area);
            self.draw_flagship_info(surface, info_area);
        }
    }

    /// The three squadron cards. Tier degrades automatically with the space
    /// [`draw_command_deck`] hands it ([`card::Tier::for_rect`]), so a narrow
    /// portrait command deck still shows a legible, tappable card -- just
    /// with the rule text dropped first, per [`ui::card`]'s tier ordering.
    fn draw_squadron_cards(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < card::FAN_MIN || area.height() < 3 {
            return;
        }
        let rects = card::fan(area, self.battle.cards.len(), card::FULL_W);
        for (i, rect) in rects.into_iter().enumerate() {
            if rect.width() == 0 {
                continue;
            }
            let c = &self.battle.cards[i];
            let on_cooldown = c.cooldown > 0.0;
            let afford = self.scrap >= DEPLOY_COST;
            let state = if on_cooldown || !afford {
                CardState::Disabled
            } else if self.battle.selected_card == Some(i) {
                CardState::Selected
            } else {
                CardState::Idle
            };
            let body = if on_cooldown {
                format!("Reloading {:.1}s", c.cooldown)
            } else {
                c.kind.rule().to_string()
            };
            let cost = format!("{DEPLOY_COST:.0}");
            Card::new(c.kind.name())
                .cost(&cost)
                .kind("Squadron")
                .body(&body)
                .accent(c.kind.color())
                .state(state)
                .draw(surface, rect);
            if !on_cooldown && afford {
                self.hotspots
                    .push_tappable(rect, area, Action::SelectCard(i));
            }
        }
    }

    fn draw_flagship_info(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 6 || area.height() == 0 {
            return;
        }
        let bg = ui::BG;
        let fs = &self.battle.flagship;
        let mut y = area.top();
        panel::spans(
            surface,
            (area.left(), y),
            area.width(),
            &[Span::keyword("ENEMY FLAGSHIP")],
            bg,
        );
        y += 1;
        if y >= area.bottom() {
            return;
        }
        let bar_w = area.width().saturating_sub(6).min(20);
        panel::spans(surface, (area.left(), y), 5, &[Span::dim("Hull")], bg);
        if bar_w > 0 {
            panel::bar(
                surface,
                (area.left() + 5, y),
                bar_w,
                fs.hull / fs.hull_max,
                panel::threshold(fs.hull / fs.hull_max),
                rgb(30, 30, 36),
            );
        }
        y += 1;
        for sys in &fs.systems {
            if y >= area.bottom() {
                break;
            }
            let online = sys.hp > 0.0;
            let label = truncate(sys.name, 9);
            surface.print(
                (area.left(), y),
                label,
                Style::new()
                    .fg(if online { ui::FG } else { ui::DIM })
                    .bg(bg),
            );
            if bar_w > 0 {
                panel::bar(
                    surface,
                    (area.left() + 10, y),
                    bar_w.saturating_sub(5),
                    sys.hp / sys.hp_max,
                    if online {
                        panel::threshold(sys.hp / sys.hp_max)
                    } else {
                        ui::DIM
                    },
                    rgb(30, 30, 36),
                );
            }
            y += 1;
        }
        y += 1;
        if y < area.bottom() {
            panel::spans(
                surface,
                (area.left(), y),
                area.width(),
                &[Span::keyword("OFFICERS")],
                bg,
            );
            y += 1;
        }
        for officer in &self.battle.officers {
            if y >= area.bottom() {
                break;
            }
            panel::spans(
                surface,
                (area.left(), y),
                area.width(),
                &[
                    Span::plain(officer.name),
                    Span::plain(" "),
                    Span::dim(officer.buff),
                ],
                bg,
            );
            y += 1;
        }

        // The log gets whatever is left rather than a reserved share: on a
        // squeezed layout the flagship readout (what to shoot at) matters
        // more than the history of how it got there.
        if y + 1 < area.bottom() {
            let log_area = Rect::new(area.left(), y + 1, area.width(), area.bottom() - y - 1);
            self.log.draw(surface, log_area, bg);
        }
    }

    fn draw_resource_band(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < 20 {
            return;
        }
        panel::spans(
            surface,
            (area.left() + 1, area.top()),
            area.width().saturating_sub(2),
            &[
                Span::dim("FUEL "),
                Span::new(
                    &format!("{:.0}/{:.0}", self.fuel, self.fuel_max),
                    threshold_fuel(self.fuel, self.fuel_max),
                ),
                Span::plain("   "),
                Span::dim("SCRAP "),
                Span::keyword(&format!("{:.0}", self.scrap)),
                Span::plain("   "),
                Span::dim("CREW "),
                Span::keyword(&format!("{}", self.crew)),
            ],
            ui::CHROME_BG,
        );
    }

    fn status_text(&self) -> String {
        format!(
            "at {}  tab {}",
            self.map.nodes[self.map.current].label,
            match self.tab {
                ScreenTab::Map => "map",
                ScreenTab::Battle => "battle",
            }
        )
    }
}

/// Threshold color for the fuel readout, using the same green/amber/red
/// convention every gauge in the gallery uses (see [`panel::threshold`]).
fn threshold_fuel(fuel: f32, max: f32) -> Color {
    panel::threshold(if max > 0.0 { fuel / max } else { 0.0 })
}

/// Splits `area` into `n` equal-height rows, remainder distributed to the
/// first rows. The vertical mirror of [`panel::columns`], needed here (and
/// not exported from `panel`) because the battle lanes are the one place in
/// this gallery that stacks more than two bands and still wants the leftover
/// row accounted for rather than dropped.
fn rows(area: Rect, n: u16) -> Vec<Rect> {
    let n = n.max(1);
    let base = area.height() / n;
    let extra = area.height() % n;
    let mut out = Vec::with_capacity(usize::from(n));
    let mut y = area.top();
    for i in 0..n {
        let h = base + u16::from(i < extra);
        out.push(Rect::new(area.left(), y, area.width(), h));
        y += h;
    }
    out
}

impl Demo for FleetCommand {
    const NAME: &'static str = "30_fleet_command";
    const TITLE: &'static str = "30 Fleet Command";
    const BLURB: &'static str = "A sector node map paired with a live three-lane squadron battle.";
    const GRID: (u16, u16) = (168, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Left/Right", "select reachable node"),
            ("Enter", "confirm jump"),
            ("Backspace", "cancel selection"),
            ("1/2/3", "select squadron card"),
            ("J/K/L", "deploy into lane 1/2/3"),
            ("Tab", "switch Map/Battle (portrait)"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let (readout, body) = panel::split_top(content, 1);
        self.draw_resource_band(&mut surface, readout);

        let shape = Shape::of(body);
        self.hotspots.clear();
        if shape.stacks() {
            self.layout_portrait(&mut surface, body);
        } else {
            self.layout_wide(&mut surface, body);
        }

        let gesture = self.pointer.take();
        self.handle_gesture(gesture);
        self.battle.simulate(dt, &mut self.log);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(FleetCommand);
