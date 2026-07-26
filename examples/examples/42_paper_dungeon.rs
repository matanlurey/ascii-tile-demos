//! 42: Paper dungeon -- movement on rails through a papercraft crypt.
//!
//! Adapted from Book of Demons. Every other dungeon demo in this gallery lets
//! you walk anywhere the floor allows; this one does not let you walk at all.
//! The hero is a token glued to a fixed, branching ribbon that winds through
//! the crypt, and the only choice a tap makes is *which point on the ribbon*
//! to slide toward. The ribbon is a tree: reaching any node picks a unique
//! route through it, so tapping a spot past a fork *is* choosing the fork,
//! with no separate branch-select step to design around.
//!
//! Techniques on show:
//!
//! - **A track as a tree, not a grid** ([`Track::generate`]): nodes carry a
//!   parent and a list of children. Because a tree has exactly one simple
//!   path between any two nodes, "go here" never needs a pathfinder --
//!   [`Track::path_between`] just walks both nodes up to their common
//!   ancestor and concatenates the two climbs. That is the entire routing
//!   algorithm the rails need.
//! - **Isometric placement reused, not reinvented**
//!   ([`tilekit::geom::IsoLayout`]): track nodes are tile coordinates
//!   projected through the same dimetric transform `23_iso_tactics` uses for
//!   its floor, so the ribbon sits on a real diamond-tiled crypt floor rather
//!   than floating over a blank background.
//! - **A thick ribbon from a thin line**: an edge between two nodes is one
//!   interpolated line of points, widened to three rows by drawing the row
//!   above and below it in a darker (fold-shadow) and lighter (fold-highlight)
//!   tone. That is the entire paper-fold illusion -- no separate bevel pass.
//! - **A hard offset drop shadow**: every cut-out (the ribbon, the hero, each
//!   monster, each card, both orbs) is drawn twice: once one cell down-right
//!   in a flat dark tone, then again in its real color on top. Consistent
//!   offset in one direction on every element is what reads as "flat shapes
//!   standing on a lit stage" rather than as noise.
//! - **Rails-gated combat**: an edge with a living monster on it cannot be
//!   crossed. [`PaperDungeon::advance_hero`] discovers this the moment it
//!   tries to take that edge and stops the hero at the near node instead,
//!   which is what makes a monster read as an obstacle *on the path* instead
//!   of a wandering enemy that happens to be nearby.
//! - **Pip-filled orbs, not bars** ([`draw_orb`]): health and mana are drawn
//!   as a grid of heart/gem glyphs, one per point, in heavy double-frame
//!   borders. `card::Card` was not reached for here or for the action bar
//!   (see [`draw_card`]) because neither orb nor card is one of the cases the
//!   shared widget covers: a card here needs a key badge *and* a separate
//!   charge count in the same small frame, and an orb has no shared widget at
//!   all yet.
//! - **Two-state idle sway, not sub-cell easing**: decoration (the floor
//!   glyphs, the hero and monster shadows) steps between two offsets on a
//!   slow timer, per the Round 2 note that smoothly-eased text reads as a
//!   jitter bug. Card text and pip counts never move at all; only the hero's
//!   and monsters' continuous positions along the rails actually glide.
//!
//! ```sh
//! cargo run --example 42_paper_dungeon --features crossterm
//! cargo run --example 42_paper_dungeon --features software
//! cargo run --example 42_paper_dungeon --features gl
//! cargo run --example 42_paper_dungeon  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Panel};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::geom::{IsoLayout, Tile};
use tilekit::noise::Rng;
use tilekit::palette::{mix, rgb, scale};

/// Nodes in the main descent, entrance to the deepest point.
const MAIN_LEN: usize = 11;
/// Nodes in the side spur that forks off the main descent.
const BRANCH_LEN: usize = 5;
/// Which main-chain node the spur forks from.
const BRANCH_AT: usize = 4;

/// How many rails-seconds the hero takes to cross one edge. Slow enough that
/// a tap-to-travel reads as a deliberate slide rather than a teleport, fast
/// enough that crossing the whole track does not feel like waiting.
const EDGE_SECONDS: f32 = 0.9;

/// How long an idle hero waits at a node before the autopilot picks its next
/// destination, in seconds. This is what keeps the scene visibly animating
/// with no input at all: the animation-gate tooling renders a settled frame
/// and a later one and fails the build if nothing moved, and a hero that only
/// ever moves in response to a tap would sit dead between taps.
const AUTOPILOT_PAUSE: f32 = 1.6;

/// Chip damage per second while blocked by a living monster. Small: the point
/// is that stalling costs something, not that it is punishing.
const COMBAT_DPS: f32 = 1.4;

/// Health and mana pip caps. Kept small enough that every pip fits an 18-wide
/// orb without wrapping past a couple of rows, and large enough that a card's
/// mana cost reads as a real fraction of the pool rather than 1 of 99.
const HEALTH_MAX: i32 = 18;
/// See [`HEALTH_MAX`].
const MANA_MAX: i32 = 14;

/// Card layout: a title, a one-glyph emblem, a mana cost, a charge count, and
/// the number key that plays it. `charges` is `None` for cards with unlimited
/// uses (Mighty Blow spends mana, not itself).
struct CardDef {
    key: char,
    title: &'static str,
    emblem: char,
    mana_cost: i32,
    charges: Option<u32>,
    accent: Color,
}

/// The fixed hand. Book of Demons has no other inventory: whatever is not one
/// of these cards is not equippable at all, which is the whole point of its
/// system and why there is no separate equipment panel here.
const CARDS: [CardDef; 5] = [
    CardDef {
        key: '1',
        title: "Remedy",
        emblem: '+',
        mana_cost: 0,
        charges: Some(3),
        accent: rgb(196, 92, 84),
    },
    CardDef {
        key: '2',
        title: "Mighty Blow",
        emblem: '/',
        mana_cost: 2,
        charges: None,
        accent: rgb(96, 132, 196),
    },
    CardDef {
        key: '3',
        title: "Health Potion",
        emblem: '!',
        mana_cost: 0,
        charges: Some(8),
        accent: rgb(196, 92, 84),
    },
    CardDef {
        key: '4',
        title: "Shield",
        emblem: 'O',
        mana_cost: 0,
        charges: Some(4),
        accent: rgb(150, 158, 176),
    },
    CardDef {
        key: '5',
        title: "Boots",
        emblem: '^',
        mana_cost: 0,
        charges: Some(5),
        accent: rgb(150, 128, 90),
    },
];

/// One node on the rails: a tile position plus tree links. `children` is
/// empty for a dead end and holds more than one entry only at the fork, which
/// is the only place a tap has to pick a direction rather than just a
/// distance.
struct Node {
    tile: Tile,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// A monster occupying one edge of the rails. While `hp > 0` the edge between
/// `from` and `to` cannot be crossed.
struct Monster {
    name: &'static str,
    gives: &'static str,
    threatens: &'static str,
    from: usize,
    to: usize,
    hp: f32,
    hp_max: f32,
    /// Position along its edge, oscillating so an idle monster still reads as
    /// pacing its stretch rather than standing frozen. Purely decorative:
    /// combat and blocking do not depend on it.
    sway: f32,
}

/// The generated rails: every node plus the monsters guarding specific edges.
struct Track {
    nodes: Vec<Node>,
    monsters: Vec<Monster>,
}

impl Track {
    /// Builds the main descent and its one spur with a short biased random
    /// walk in tile space, then pins three monsters onto specific edges.
    ///
    /// Biased rather than uniformly random: a walk that reverses direction
    /// freely produces a tangle that self-occludes on an isometric projection
    /// (draw order only handles overlap by depth, not a path crossing itself
    /// at the same depth). Preferring the previous step's direction most of
    /// the time keeps the ribbon reading as a corridor with occasional turns,
    /// which is what an actual dungeon corridor looks like from above.
    fn generate(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let dirs: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

        let mut nodes = vec![Node {
            tile: Tile::new(0, 0),
            parent: None,
            children: Vec::new(),
        }];
        let mut visited = vec![Tile::new(0, 0)];
        let mut last_dir = 0usize;

        let mut chain = vec![0usize];
        for _ in 1..MAIN_LEN {
            let parent = *chain.last().unwrap();
            let (dir, tile) = next_step(nodes[parent].tile, last_dir, &dirs, &visited, &mut rng);
            last_dir = dir;
            let idx = nodes.len();
            nodes.push(Node {
                tile,
                parent: Some(parent),
                children: Vec::new(),
            });
            nodes[parent].children.push(idx);
            visited.push(tile);
            chain.push(idx);
        }

        // The spur forks off in whichever direction was *not* the main
        // chain's own heading at that point, so the branch is visually
        // distinguishable from a straight continuation rather than looking
        // like the main chain merely kept going.
        let fork_dir = (last_dir + 1) % dirs.len();
        let mut branch_last_dir = fork_dir;
        let mut branch_parent = chain[BRANCH_AT];
        let mut branch = Vec::new();
        for i in 0..BRANCH_LEN {
            let start_dir = if i == 0 { fork_dir } else { branch_last_dir };
            let (dir, tile) = next_step(
                nodes[branch_parent].tile,
                start_dir,
                &dirs,
                &visited,
                &mut rng,
            );
            branch_last_dir = dir;
            let idx = nodes.len();
            nodes.push(Node {
                tile,
                parent: Some(branch_parent),
                children: Vec::new(),
            });
            nodes[branch_parent].children.push(idx);
            visited.push(tile);
            branch.push(idx);
            branch_parent = idx;
        }

        let monsters = vec![
            Monster {
                name: "Skeleton Guard",
                gives: "+40 gold",
                threatens: "-2 hp/s",
                from: chain[2],
                to: chain[3],
                hp: 14.0,
                hp_max: 14.0,
                sway: 0.15,
            },
            Monster {
                name: "Jelly-belly Bomb",
                gives: "+3 mana",
                threatens: "poison",
                from: branch[0],
                to: branch[1],
                hp: 10.0,
                hp_max: 10.0,
                sway: 0.6,
            },
            Monster {
                name: "Crypt Wraith",
                gives: "+1 card charge",
                threatens: "curse",
                from: chain[7],
                to: chain[8],
                hp: 20.0,
                hp_max: 20.0,
                sway: 0.4,
            },
        ];

        Self { nodes, monsters }
    }

    /// The unique simple path from `a` to `b`: climb both to their common
    /// ancestor, then walk back down. A tree needs no pathfinder for this --
    /// see the module doc.
    fn path_between(&self, a: usize, b: usize) -> Vec<usize> {
        let anc = |mut i: usize| {
            let mut chain = vec![i];
            while let Some(p) = self.nodes[i].parent {
                chain.push(p);
                i = p;
            }
            chain
        };
        let up_a = anc(a);
        let up_b = anc(b);
        let common = up_a.iter().find(|n| up_b.contains(n)).copied().unwrap_or(a);

        let mut path: Vec<usize> = up_a.into_iter().take_while(|&n| n != common).collect();
        path.push(common);
        let mut down: Vec<usize> = up_b.into_iter().take_while(|&n| n != common).collect();
        down.reverse();
        path.extend(down);
        path
    }

    /// The living monster (if any) blocking the edge between two adjacent
    /// nodes, checked in both directions since the hero can walk either way
    /// along its own rails.
    fn blocker(&self, a: usize, b: usize) -> Option<usize> {
        self.monsters
            .iter()
            .position(|m| m.hp > 0.0 && ((m.from == a && m.to == b) || (m.from == b && m.to == a)))
    }
}

/// Picks the next tile from `from`, preferring `preferred_dir` 65% of the
/// time and a different unvisited direction otherwise, retrying a few times
/// to dodge revisits before giving up and forcing the preferred direction
/// anyway (a short walk has nowhere else to go once it is boxed in).
fn next_step(
    origin: Tile,
    preferred_dir: usize,
    dirs: &[(i32, i32); 4],
    visited: &[Tile],
    rng: &mut Rng,
) -> (usize, Tile) {
    for _ in 0..5 {
        let dir = if rng.next_f32() < 0.65 {
            preferred_dir
        } else {
            rng.next_below(dirs.len() as u32) as usize
        };
        let (dc, dr) = dirs[dir];
        let candidate = Tile::new(origin.col + dc, origin.row + dr);
        if !visited.contains(&candidate) {
            return (dir, candidate);
        }
    }
    let (dc, dr) = dirs[preferred_dir];
    (preferred_dir, Tile::new(origin.col + dc, origin.row + dr))
}

/// The card's remaining charges, or `u32::MAX` for the unlimited (mana-only)
/// Mighty Blow, so a single `charges[i] > 0` check works for every card.
const UNLIMITED: u32 = u32::MAX;

/// Where a tap or key landed, resolved during layout and acted on once per
/// frame.
#[derive(Clone, Copy)]
enum Action {
    Node(usize),
    Card(usize),
}

/// State: the generated rails, the hero's position on them, the party's
/// resources, and the fixed hand of cards.
pub struct PaperDungeon {
    track: Track,
    seed: u32,
    hero_node: usize,
    hero_next: Option<usize>,
    hero_t: f32,
    /// The node the player (or the autopilot) most recently asked to reach.
    target: Option<usize>,
    /// Set while the hero is stalled at an edge a living monster occupies.
    combat: Option<usize>,
    autopilot_wait: f32,
    health: f32,
    mana: f32,
    gold: u32,
    session: f32,
    shield: f32,
    charges: [u32; CARDS.len()],
    time: f32,
    scroll: (i32, i32),
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for PaperDungeon {
    fn default() -> Self {
        let seed = 4242;
        let track = Track::generate(seed);
        let charges = core::array::from_fn(|i| CARDS[i].charges.unwrap_or(UNLIMITED));
        Self {
            track,
            seed,
            hero_node: 0,
            hero_next: None,
            hero_t: 0.0,
            target: None,
            combat: None,
            autopilot_wait: AUTOPILOT_PAUSE,
            health: f32::from(HEALTH_MAX as i16),
            mana: f32::from(MANA_MAX as i16),
            gold: 4308,
            session: 0.0,
            shield: 0.0,
            charges,
            time: 0.0,
            scroll: (0, 0),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl PaperDungeon {
    /// One frame of hero movement: keeps sliding along a held edge, or (once
    /// arrived at a node) tries to take the next step of the current route.
    /// Stopping at a blocked edge rather than skipping it is the entire
    /// "monsters gate the rails" mechanic.
    fn advance_hero(&mut self, dt: f32) {
        if let Some(next) = self.hero_next {
            self.hero_t += dt / EDGE_SECONDS;
            if self.hero_t >= 1.0 {
                self.hero_node = next;
                self.hero_next = None;
                self.hero_t = 0.0;
                if self.target == Some(next) {
                    self.target = None;
                }
            }
            return;
        }
        if self.combat.is_some() {
            return;
        }

        let Some(target) = self.target else {
            self.run_autopilot(dt);
            return;
        };
        if target == self.hero_node {
            self.target = None;
            return;
        }
        let route = self.track.path_between(self.hero_node, target);
        let Some(&step) = route.get(1) else {
            self.target = None;
            return;
        };
        if let Some(monster) = self.track.blocker(self.hero_node, step) {
            self.combat = Some(monster);
            return;
        }
        self.hero_next = Some(step);
        self.hero_t = 0.0;
    }

    /// Picks a fresh destination once the hero has been idle for a while.
    /// Driven by `self.time`, not wall-clock, so replaying an identical frame
    /// sequence always picks the identical destinations -- required for the
    /// determinism test, and also just correct: a demo seeded from the real
    /// clock cannot be reproduced from a bug report.
    fn run_autopilot(&mut self, dt: f32) {
        self.autopilot_wait -= dt;
        if self.autopilot_wait > 0.0 {
            return;
        }
        self.autopilot_wait = AUTOPILOT_PAUSE;
        let mut rng = Rng::new(self.seed ^ (self.time * 1000.0) as u32);
        let pick = rng.next_below(self.track.nodes.len() as u32) as usize;
        self.target = Some(pick);
    }

    /// Advances monster pacing, chip damage while stalled, and the session
    /// clock.
    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        self.session += dt;
        self.shield = (self.shield - dt).max(0.0);

        for monster in &mut self.track.monsters {
            if monster.hp <= 0.0 {
                continue;
            }
            // Bounce back and forth along its own edge: a triangle wave
            // rather than a sine keeps the monster's speed constant, which
            // reads as pacing rather than drifting.
            monster.sway = dt.mul_add(0.5, monster.sway);
            monster.sway %= 2.0;
        }

        if let Some(idx) = self.combat {
            let hp = self.track.monsters[idx].hp;
            if hp <= 0.0 {
                self.combat = None;
            } else {
                let absorbed = if self.shield > 0.0 { 0.5 } else { 1.0 };
                self.health = (COMBAT_DPS * absorbed).mul_add(-dt, self.health).max(1.0);
            }
        }
        self.advance_hero(dt);
    }

    /// Plays card `i`: spends its cost, applies its effect, and (for Mighty
    /// Blow) damages whichever monster currently blocks the hero.
    fn play_card(&mut self, i: usize) {
        let def = &CARDS[i];
        if self.charges[i] == 0 || self.mana < f32::from(def.mana_cost as i16) {
            return;
        }
        match def.title {
            "Remedy" => self.health = (self.health + 4.0).min(f32::from(HEALTH_MAX as i16)),
            "Health Potion" => self.health = (self.health + 10.0).min(f32::from(HEALTH_MAX as i16)),
            "Shield" => self.shield = 5.0,
            "Boots" => {
                if let Some(idx) = self.combat {
                    self.track.monsters[idx].hp = 0.0;
                    self.combat = None;
                }
            }
            "Mighty Blow" => {
                if let Some(idx) = self.combat {
                    self.track.monsters[idx].hp -= 6.0;
                    if self.track.monsters[idx].hp <= 0.0 {
                        self.combat = None;
                    }
                }
            }
            _ => {}
        }
        self.mana -= f32::from(def.mana_cost as i16);
        if self.charges[i] != UNLIMITED {
            self.charges[i] -= 1;
        }
    }

    /// The monster the top banner should describe: whichever one is currently
    /// blocking the hero, or else the nearest living monster ahead on the
    /// tree, so the banner always names something even between fights.
    fn banner_monster(&self) -> Option<usize> {
        if let Some(idx) = self.combat {
            return Some(idx);
        }
        self.track
            .monsters
            .iter()
            .enumerate()
            .filter(|(_, m)| m.hp > 0.0)
            .min_by_key(|(_, m)| self.track.path_between(self.hero_node, m.from).len())
            .map(|(i, _)| i)
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
            KeyCode::Char(c @ '1'..='5') => {
                let idx = c as usize - '1' as usize;
                self.play_card(idx);
            }
            KeyCode::Right | KeyCode::Down => {
                if let Some(&child) = self.track.nodes[self.hero_node].children.first() {
                    self.target = Some(child);
                }
            }
            KeyCode::Left | KeyCode::Up => {
                if let Some(parent) = self.track.nodes[self.hero_node].parent {
                    self.target = Some(parent);
                }
            }
            KeyCode::Tab => {
                // Rotate which child counts as "forward" at a fork, so Right
                // reaches every branch from the keyboard alone.
                let node = &mut self.track.nodes[self.hero_node];
                if node.children.len() > 1 {
                    node.children.rotate_left(1);
                }
            }
            _ => {}
        }
    }

    fn status(&self) -> String {
        let combat = self
            .combat
            .map(|i| format!(" -- fighting {}", self.track.monsters[i].name))
            .unwrap_or_default();
        format!(
            "hp {}/{}  mana {}/{}{}",
            self.health as i32, HEALTH_MAX, self.mana as i32, MANA_MAX, combat
        )
    }
}

/// Shadow offset shared by every papercraft cut-out: one cell down and one
/// right, which is the entire "flat card lit from the upper left" illusion.
const SHADOW: (i32, i32) = (1, 1);

/// Aged-paper background for the crypt floor.
const PAPER_BG: Color = rgb(28, 24, 18);
/// The ribbon's own paper tone.
const RIBBON: Color = rgb(196, 180, 148);
/// Flat drop-shadow tone; darker than the page so a shadow reads as a gap
/// under the cut-out rather than another cut-out.
const SHADOW_COLOR: Color = rgb(12, 10, 8);

/// World-cell bounding box of every track node, before any camera offset.
/// Bundled into a struct rather than a tuple so `draw_floor`'s field accesses
/// stay named instead of positional.
#[derive(Clone, Copy)]
struct Bounds {
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

impl PaperDungeon {
    fn track_bounds(&self, layout: IsoLayout) -> Bounds {
        let mut bounds = Bounds {
            min_x: i32::MAX,
            max_x: i32::MIN,
            min_y: i32::MAX,
            max_y: i32::MIN,
        };
        for node in &self.track.nodes {
            let c = layout.tile_to_cell(node.tile);
            bounds.min_x = bounds.min_x.min(c.x);
            bounds.max_x = bounds.max_x.max(c.x);
            bounds.min_y = bounds.min_y.min(c.y);
            bounds.max_y = bounds.max_y.max(c.y);
        }
        bounds
    }

    /// Picks the isometric half-extents that make the fixed tile tree fill
    /// `area`, instead of always drawing it at [`IsoLayout::SMALL`].
    ///
    /// The tree's node count is fixed at generation time, so on a small
    /// terminal the ribbon reads fine at `SMALL`, but on a wide desktop
    /// window the same handful of nodes drawn at a fixed pitch leaves most
    /// of the play area empty. `tile_to_cell` is linear in `half_w`/`half_h`
    /// (`x = (col - row) * half_w`, `y = (col + row) * half_h`), so the
    /// tile-space spread (measured once at `half_w = half_h = 1`) can be
    /// scaled up by picking bigger half-extents until the projected bounding
    /// box, plus a fixed margin for the floor/hero/monster art drawn past
    /// each node, fills `area`. Clamped at both ends: never smaller than
    /// `SMALL` (the smallest pitch the ribbon and its fold-highlight/shadow
    /// rows still read as three distinct lines at), never so large that a
    /// huge window would space nodes farther apart than a player can track
    /// visually across taps.
    fn fitted_layout(&self, area: Rect) -> IsoLayout {
        const UNIT: IsoLayout = IsoLayout {
            half_w: 1,
            half_h: 1,
        };
        const MARGIN_W: i32 = 8;
        const MARGIN_H: i32 = 6;
        const MAX_HALF_W: i32 = 10;
        const MAX_HALF_H: i32 = 5;

        let unit_bounds = self.track_bounds(UNIT);
        let unit_w = (unit_bounds.max_x - unit_bounds.min_x).max(1);
        let unit_h = (unit_bounds.max_y - unit_bounds.min_y).max(1);

        let avail_w = i32::from(area.width()).saturating_sub(MARGIN_W).max(1);
        let avail_h = i32::from(area.height()).saturating_sub(MARGIN_H).max(1);

        let half_w = (avail_w / unit_w).clamp(IsoLayout::SMALL.half_w, MAX_HALF_W);
        let half_h = (avail_h / unit_h).clamp(IsoLayout::SMALL.half_h, MAX_HALF_H);
        IsoLayout::new(half_w, half_h)
    }

    /// The hero's continuous world-cell position: interpolated along the
    /// held edge, or resting at its node.
    fn hero_cell(&self, layout: IsoLayout) -> (f32, f32) {
        let a = layout.tile_to_cell(self.track.nodes[self.hero_node].tile);
        self.hero_next.map_or((a.x as f32, a.y as f32), |next| {
            let b = layout.tile_to_cell(self.track.nodes[next].tile);
            let t = self.hero_t.clamp(0.0, 1.0);
            (
                (a.x as f32).mul_add(1.0 - t, b.x as f32 * t),
                (a.y as f32).mul_add(1.0 - t, b.y as f32 * t),
            )
        })
    }

    fn monster_cell(&self, layout: IsoLayout, monster: &Monster) -> (f32, f32) {
        let a = layout.tile_to_cell(self.track.nodes[monster.from].tile);
        let b = layout.tile_to_cell(self.track.nodes[monster.to].tile);
        // Triangle wave in [0, 2) folded to [0, 1] and back gives a steady
        // back-and-forth pace along the edge without ever teleporting at the
        // ends.
        let t = if monster.sway < 1.0 {
            monster.sway
        } else {
            2.0 - monster.sway
        };
        (
            (a.x as f32).mul_add(1.0 - t, b.x as f32 * t),
            (a.y as f32).mul_add(1.0 - t, b.y as f32 * t),
        )
    }

    /// Draws the crypt floor, the ribbon, monsters, and the hero into `area`,
    /// registering one tappable hotspot per node.
    fn draw_track(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() < 4 || area.height() < 4 {
            return;
        }
        ui::fill(surface, area, Style::new().bg(PAPER_BG));

        let layout = self.fitted_layout(area);
        let bounds = self.track_bounds(layout);
        let span_w = (bounds.max_x - bounds.min_x + 6) as u16;
        let span_h = (bounds.max_y - bounds.min_y + 6) as u16;

        // Center the whole track if it fits; otherwise follow the hero, so a
        // narrow phone viewport still keeps the moving hero on screen instead
        // of scrolled off the edge of a track drawn at full size. That
        // following-camera *is* the "track view pans" behaviour asked for on
        // portrait -- it simply has nothing to do on a desktop window wide
        // enough to show the whole tree at once.
        let origin = if span_w <= area.width() && span_h <= area.height() {
            (
                i32::from(area.left()) + i32::from(area.width()) / 2
                    - i32::midpoint(bounds.min_x, bounds.max_x),
                i32::from(area.top()) + i32::from(area.height()) / 2
                    - i32::midpoint(bounds.min_y, bounds.max_y),
            )
        } else {
            let (hx, hy) = self.hero_cell(layout);
            (
                i32::from(area.left()) + i32::from(area.width()) / 2 - hx as i32,
                i32::from(area.top()) + i32::from(area.height()) / 2 - hy as i32,
            )
        };
        self.scroll = origin;

        self.draw_floor(surface, area, layout, bounds);
        self.draw_ribbon(surface, area, layout);
        self.draw_monsters(surface, area, layout);
        self.draw_hero(surface, area, layout);
        self.register_node_hotspots(area, layout);
    }

    /// A sparse diamond checkerboard behind the ribbon: just enough crypt
    /// floor showing through that the ribbon reads as sitting on a place
    /// rather than floating on a solid color.
    fn draw_floor(&self, surface: &mut Surface<'_>, area: Rect, layout: IsoLayout, bounds: Bounds) {
        let Bounds {
            min_x,
            max_x,
            min_y,
            max_y,
        } = bounds;
        let cols = (max_x - min_x) / layout.width().max(1) + 3;
        let rows = (max_y - min_y) / layout.height().max(1) + 3;
        let start_col = min_x / layout.half_w.max(1) - 2;
        let start_row = min_y / layout.half_h.max(1) - 2;
        for r in 0..rows {
            for c in 0..cols {
                let tile = Tile::new(start_col + c, start_row + r);
                let cell = layout.tile_to_cell(tile);
                let (sx, sy) = (self.scroll.0 + cell.x, self.scroll.1 + cell.y);
                if sx < i32::from(area.left()) || sy < i32::from(area.top()) {
                    continue;
                }
                let (x, y) = (sx as u16, sy as u16);
                if x >= area.right() || y >= area.bottom() {
                    continue;
                }
                let dark = (tile.col + tile.row).rem_euclid(2) == 0;
                let shade = if dark {
                    scale(PAPER_BG, 1.3)
                } else {
                    scale(PAPER_BG, 1.6)
                };
                surface.put((x, y), '.', Style::new().fg(shade).bg(PAPER_BG));
            }
        }
    }

    /// Draws one edge as a widened line: shadow first, then the ribbon body
    /// with a lighter fold-highlight row above and a darker fold-shadow row
    /// below the spine.
    fn draw_ribbon(&self, surface: &mut Surface<'_>, area: Rect, layout: IsoLayout) {
        for node_idx in 0..self.track.nodes.len() {
            let node = &self.track.nodes[node_idx];
            let Some(parent) = node.parent else { continue };
            let from_cell = layout.tile_to_cell(self.track.nodes[parent].tile);
            let to_cell = layout.tile_to_cell(node.tile);
            let blocked = self.track.blocker(parent, node_idx).is_some();
            self.draw_edge(surface, area, from_cell, to_cell, blocked);
        }
    }

    fn draw_edge(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        from_cell: tilekit::geom::Cell,
        to_cell: tilekit::geom::Cell,
        blocked: bool,
    ) {
        // One step per cell of travel (never fewer than 8), so a ribbon
        // stretched wide by a large fitted layout still samples densely
        // enough to stay a solid line instead of a dotted one.
        let span = (to_cell.x - from_cell.x)
            .abs()
            .max((to_cell.y - from_cell.y).abs());
        let steps = span.max(8);
        let ribbon = if blocked { scale(RIBBON, 0.6) } else { RIBBON };
        let highlight = mix(ribbon, rgb(255, 250, 235), 0.35);
        let fold_shadow = scale(ribbon, 0.65);

        for step in 0..=steps {
            let frac = f32::from(step as i16) / f32::from(steps as i16);
            let px = (from_cell.x as f32).mul_add(1.0 - frac, to_cell.x as f32 * frac);
            let py = (from_cell.y as f32).mul_add(1.0 - frac, to_cell.y as f32 * frac);
            let (cx, cy) = (px.round() as i32, py.round() as i32);

            // Shadow pass, offset down-right, drawn before the ribbon body so
            // the body always wins where the two overlap.
            for row in -1..=1 {
                self.put_world(
                    surface,
                    area,
                    cx + SHADOW.0,
                    cy + row + SHADOW.1,
                    '\u{2591}',
                    Style::new().fg(SHADOW_COLOR).bg(PAPER_BG),
                );
            }
            self.put_world(
                surface,
                area,
                cx,
                cy - 1,
                '\u{2500}',
                Style::new().fg(highlight).bg(PAPER_BG),
            );
            self.put_world(
                surface,
                area,
                cx,
                cy,
                '\u{2588}',
                Style::new().fg(ribbon).bg(PAPER_BG),
            );
            self.put_world(
                surface,
                area,
                cx,
                cy + 1,
                '\u{2500}',
                Style::new().fg(fold_shadow).bg(PAPER_BG),
            );
        }
    }

    /// Writes one glyph at a world cell, translated by the current camera
    /// scroll and clipped to `area`. Every drawing routine below funnels
    /// through this so the camera-follow logic in [`draw_track`] lives in
    /// exactly one place.
    fn put_world(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        wx: i32,
        wy: i32,
        ch: char,
        style: Style,
    ) {
        let (sx, sy) = (self.scroll.0 + wx, self.scroll.1 + wy);
        if sx < i32::from(area.left()) || sy < i32::from(area.top()) {
            return;
        }
        let (x, y) = (sx as u16, sy as u16);
        if x >= area.right() || y >= area.bottom() {
            return;
        }
        surface.put((x, y), ch, style);
    }

    fn draw_monsters(&self, surface: &mut Surface<'_>, area: Rect, layout: IsoLayout) {
        for monster in &self.track.monsters {
            if monster.hp <= 0.0 {
                continue;
            }
            let (fx, fy) = self.monster_cell(layout, monster);
            let (wx, wy) = (fx.round() as i32, fy.round() as i32);
            self.put_world(
                surface,
                area,
                wx + SHADOW.0,
                wy + SHADOW.1,
                '\u{2588}',
                Style::new().fg(SHADOW_COLOR).bg(PAPER_BG),
            );
            self.put_world(
                surface,
                area,
                wx,
                wy,
                'M',
                Style::new().fg(rgb(150, 60, 60)).bg(PAPER_BG),
            );

            // A tiny hp pip row above the token: three or fewer heart glyphs
            // scaled to the monster's remaining fraction, enough to tell "hurt"
            // from "fresh" without a numeric readout competing for space.
            let frac = monster.hp / monster.hp_max;
            let pips = (frac * 3.0).ceil() as i32;
            for p in 0..3 {
                let ch = if p < pips { '\u{2665}' } else { '\u{00b0}' };
                let color = if p < pips {
                    rgb(196, 92, 84)
                } else {
                    scale(RIBBON, 0.5)
                };
                self.put_world(
                    surface,
                    area,
                    wx - 1 + p,
                    wy - 2,
                    ch,
                    Style::new().fg(color).bg(PAPER_BG),
                );
            }
        }
    }

    fn draw_hero(&self, surface: &mut Surface<'_>, area: Rect, layout: IsoLayout) {
        let (fx, fy) = self.hero_cell(layout);
        let (wx, wy) = (fx.round() as i32, fy.round() as i32);
        // A two-state bob rather than a continuous one: the hero's *position*
        // along the rails already glides continuously (that motion is real,
        // not decoration), but the idle "standing in a draught" sway on top
        // of it steps between two offsets on a slow timer so it never looks
        // like sub-cell jitter on what is otherwise a crisp paper cut-out.
        let bob = i32::from((self.time * 1.3) as i16 % 2 == 0);
        self.put_world(
            surface,
            area,
            wx + SHADOW.0,
            wy + 1 + SHADOW.1,
            '\u{2588}',
            Style::new().fg(SHADOW_COLOR).bg(PAPER_BG),
        );
        self.put_world(
            surface,
            area,
            wx,
            wy - bob,
            '\u{263a}',
            Style::new().fg(rgb(230, 210, 160)).bg(PAPER_BG),
        );
        self.put_world(
            surface,
            area,
            wx,
            wy + 1 - bob,
            '\u{2302}',
            Style::new().fg(rgb(90, 70, 140)).bg(PAPER_BG),
        );
    }

    /// Registers one tappable hotspot per node, in world-to-screen space, so
    /// a tap anywhere near a node counts even though the node itself draws as
    /// a single point on the ribbon.
    fn register_node_hotspots(&mut self, area: Rect, layout: IsoLayout) {
        for (i, node) in self.track.nodes.iter().enumerate() {
            let cell = layout.tile_to_cell(node.tile);
            let (sx, sy) = (self.scroll.0 + cell.x, self.scroll.1 + cell.y);
            if sx < i32::from(area.left()) - 4
                || sy < i32::from(area.top()) - 4
                || sx > i32::from(area.right()) + 4
                || sy > i32::from(area.bottom()) + 4
            {
                continue;
            }
            let rect = Rect::new(
                sx.clamp(0, i32::from(u16::MAX)) as u16,
                sy.clamp(0, i32::from(u16::MAX)) as u16,
                1,
                1,
            );
            self.hotspots.push_tappable(rect, area, Action::Node(i));
        }
    }
}

/// What one pip-filled orb needs to draw itself: bundled into a struct rather
/// than passed as six loose parameters, since every field is used together
/// and a struct literal at the call site reads as a resource description
/// rather than a positional argument list.
struct OrbSpec<'a> {
    label: &'a str,
    current: i32,
    max: i32,
    filled: char,
    empty: char,
    color: Color,
}

/// A pip-filled orb: a heavy ornamental frame around a grid of glyphs, one per
/// point of the resource, wrapping to more rows as the frame narrows. This is
/// the "pip count is the value" requirement -- there is no bar underneath it,
/// only the pips themselves plus a small numeric echo for players who prefer
/// reading a number.
fn draw_orb(surface: &mut Surface<'_>, rect: Rect, spec: &OrbSpec<'_>) {
    let inner = Panel::new()
        .title(spec.label)
        .border(panel::Border::Double)
        .frame(spec.color)
        .draw(surface, rect);
    if inner.width() < 3 || inner.height() < 2 {
        return;
    }
    let cols = inner.width().max(1);
    let pip_rows = inner.height().saturating_sub(1).max(1);
    let capacity = i32::from(cols) * i32::from(pip_rows);
    let shown_max = spec.max.min(capacity.max(1));

    for i in 0..shown_max {
        let row = i / i32::from(cols);
        let col = i % i32::from(cols);
        if row >= i32::from(pip_rows) {
            break;
        }
        let (ch, fg) = if i < spec.current {
            (spec.filled, spec.color)
        } else {
            (spec.empty, scale(spec.color, 0.35))
        };
        surface.put(
            (inner.left() + col as u16, inner.top() + row as u16),
            ch,
            Style::new().fg(fg).bg(panel::PANEL_BG),
        );
    }

    let count = format!("{}/{}", spec.current, spec.max);
    let y = inner.bottom().saturating_sub(1);
    if inner.width_usize() >= count.chars().count() {
        surface.print(
            (inner.left(), y),
            &count,
            Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
        );
    }
}

/// Draws one action-bar card by hand: a key badge and mana cost in the top
/// border, an emblem, the title, and a charge count. `ui::card::Card` was not
/// used because none of its tiers reserve a slot for *both* a key binding and
/// a separate charge count at once -- this hand and this game need both
/// visible even on the smallest legal card size, so the interior layout is
/// bespoke rather than composed from the shared widget.
fn draw_card(surface: &mut Surface<'_>, rect: Rect, def: &CardDef, charges: u32, active: bool) {
    if rect.width() < 4 || rect.height() < 3 {
        return;
    }
    let dim = charges == 0;
    let accent = if dim {
        scale(def.accent, 0.4)
    } else {
        def.accent
    };

    // Drop shadow first, one cell down-right, then the card frame on top --
    // the same rule every other cut-out in this demo follows.
    if rect.right() < surface.width() && rect.bottom() < surface.height() {
        surface.fill_rect(
            Rect::new(rect.left() + 1, rect.top() + 1, rect.width(), rect.height()),
            '\u{2591}',
            Style::new().fg(SHADOW_COLOR).bg(PAPER_BG),
        );
    }

    let border = if active {
        panel::Border::Double
    } else {
        panel::Border::Single
    };
    let inner = Panel::new()
        .border(border)
        .frame(accent)
        .bg(panel::PANEL_BG)
        .draw(surface, rect);
    if inner.width() == 0 || inner.height() == 0 {
        return;
    }

    // Key badge, top-left of the border row, and mana cost top-right: the two
    // fields that must survive even a maximally squeezed card, since they are
    // what a player checks before committing to a play.
    let key_text = format!("[{}]", def.key);
    surface.print(
        (rect.left() + 1, rect.top()),
        &key_text,
        Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
    );
    if def.mana_cost > 0 {
        let cost = format!("{}mp", def.mana_cost);
        let x = rect.right().saturating_sub(cost.chars().count() as u16 + 1);
        if x > rect.left() + key_text.chars().count() as u16 + 1 {
            surface.print(
                (x, rect.top()),
                &cost,
                Style::new().fg(rgb(120, 150, 210)).bg(panel::PANEL_BG),
            );
        }
    }

    if inner.height() == 0 {
        return;
    }
    surface.put(
        (inner.left(), inner.top()),
        def.emblem,
        Style::new().fg(accent).bg(panel::PANEL_BG),
    );

    if inner.height() > 1 {
        let title_row = inner.top() + 1;
        let text = retroglyph_widgets::truncate(def.title, inner.width_usize());
        surface.print(
            (inner.left(), title_row),
            text,
            Style::new()
                .fg(if dim { ui::DIM } else { ui::FG })
                .bg(panel::PANEL_BG),
        );
    }
    if inner.height() > 2 {
        let row = inner.top() + 2;
        let text = if charges == UNLIMITED {
            "unlimited".to_string()
        } else {
            format!("x{charges}")
        };
        surface.print(
            (inner.left(), row),
            retroglyph_widgets::truncate(&text, inner.width_usize()),
            Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
        );
    }
}

impl Demo for PaperDungeon {
    const NAME: &'static str = "42_paper_dungeon";
    const TITLE: &'static str = "42 Paper Dungeon";
    const BLURB: &'static str = "Book of Demons on-rails path with card slots and pip-filled orbs.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap node", "travel there"),
            ("Right/Left", "step forward/back"),
            ("Tab", "choose branch at a fork"),
            ("1-5", "play card"),
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

        // Resolve the tap only after layout has (re)registered every hotspot
        // for this frame -- immediate-mode UI's usual rule, so a hotspot from
        // last frame's layout can never be hit this frame.
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            match action {
                Action::Node(idx) => self.target = Some(idx),
                Action::Card(idx) => self.play_card(idx),
            }
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

impl PaperDungeon {
    /// Splits the content area into the top strip (gold, enemy banner,
    /// clock), the track view, and the bottom strip (cards plus orbs), then
    /// draws each. Portrait wraps the card row to two rows and gives the
    /// track view whatever height is left; landscape and desktop keep a
    /// single card row and a taller track view.
    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect, shape: Shape) {
        let top_h = if content.height() >= 24 {
            5
        } else if content.height() >= 16 {
            4
        } else {
            3
        };
        let (top, rest) = panel::split_top(content, top_h.min(content.height()));

        // Book of Demons' own hand never wraps because its screen is fixed
        // landscape; a phone held upright has to do something else with five
        // cards; abreast at 9 columns each, they alone would eat more than
        // half of a 73-column portrait screen. Wrapping unconditionally on
        // `Portrait` (rather than only below some width) is what the brief
        // asks for, and it costs nothing on a portrait screen wide enough to
        // have fit them in one row anyway -- two shorter rows still clear the
        // 9x4 minimum easily.
        let two_card_rows = shape == Shape::Portrait;
        let card_row_h: u16 = if content.height() >= 34 { 8 } else { 6 };
        let bottom_h = if two_card_rows {
            (card_row_h * 2).min(rest.height().saturating_sub(6))
        } else {
            card_row_h.min(rest.height().saturating_sub(6))
        };
        let (track, bottom) = panel::split_bottom(rest, bottom_h);

        self.draw_top_strip(surface, top);
        self.draw_track(surface, track);
        self.draw_bottom_strip(surface, bottom, two_card_rows);
    }

    fn draw_top_strip(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let gold_w = 16u16.min(area.width() / 4);
        let clock_w = 10u16.min(area.width() / 4);
        let (gold_area, rest) = panel::split_left(area, gold_w);
        let (banner_area, clock_area) = panel::split_right(rest, clock_w.min(rest.width()));

        let gold_inner = Panel::new().title("Gold").draw(surface, gold_area);
        if gold_inner.height() > 0 {
            surface.print(
                (gold_inner.left(), gold_inner.top()),
                retroglyph_widgets::truncate(&format!("{}", self.gold), gold_inner.width_usize()),
                Style::new().fg(rgb(226, 190, 110)).bg(panel::PANEL_BG),
            );
        }

        let mins = (self.session / 60.0) as u32;
        let secs = (self.session as u32) % 60;
        let clock_inner = Panel::new().draw(surface, clock_area);
        if clock_inner.height() > 0 {
            surface.print(
                (clock_inner.left(), clock_inner.top()),
                retroglyph_widgets::truncate(
                    &format!("{mins:02}:{secs:02}"),
                    clock_inner.width_usize(),
                ),
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
        }

        self.draw_banner(surface, banner_area);
    }

    /// The enemy banner: a title, then a green half (what it gives) and a red
    /// half (what it threatens), matching the reference screenshot's split
    /// bar.
    fn draw_banner(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new().draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }
        let Some(idx) = self.banner_monster() else {
            surface.print(
                (inner.left(), inner.top()),
                retroglyph_widgets::truncate("Crypt clear", inner.width_usize()),
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
            return;
        };
        let monster = &self.track.monsters[idx];
        surface.print(
            (inner.left(), inner.top()),
            retroglyph_widgets::truncate(monster.name, inner.width_usize()),
            Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
        );
        if inner.height() < 2 {
            return;
        }
        let half = inner.width() / 2;
        let green = Rect::new(inner.left(), inner.top() + 1, half, 1);
        let red = Rect::new(
            inner.left() + half,
            inner.top() + 1,
            inner.width() - half,
            1,
        );
        surface.fill_rect(green, ' ', Style::new().bg(rgb(30, 62, 30)));
        surface.fill_rect(red, ' ', Style::new().bg(rgb(62, 26, 26)));
        surface.print(
            (green.left(), green.top()),
            retroglyph_widgets::truncate(monster.gives, green.width_usize()),
            Style::new().fg(rgb(150, 220, 150)).bg(rgb(30, 62, 30)),
        );
        surface.print(
            (red.left(), red.top()),
            retroglyph_widgets::truncate(monster.threatens, red.width_usize()),
            Style::new().fg(rgb(230, 150, 150)).bg(rgb(62, 26, 26)),
        );
    }

    /// Cards in the middle, an orb flanking either side -- the screenshot's
    /// layout exactly. `two_rows` wraps the cards (portrait); the orbs keep
    /// their cell size regardless and only their pip *row count* changes,
    /// per the brief.
    fn draw_bottom_strip(&mut self, surface: &mut Surface<'_>, area: Rect, two_rows: bool) {
        if area.width() < 20 || area.height() == 0 {
            return;
        }
        let orb_w = 12u16.min(area.width() / 5);
        let (health_area, rest) = panel::split_left(area, orb_w);
        let (cards_area, mana_area) = panel::split_right(rest, orb_w.min(rest.width()));

        draw_orb(
            surface,
            health_area,
            &OrbSpec {
                label: "HP",
                current: self.health as i32,
                max: HEALTH_MAX,
                filled: '\u{2665}',
                empty: '\u{00b0}',
                color: rgb(196, 92, 84),
            },
        );
        draw_orb(
            surface,
            mana_area,
            &OrbSpec {
                label: "MP",
                current: self.mana as i32,
                max: MANA_MAX,
                filled: '\u{2666}',
                empty: '\u{00b0}',
                color: rgb(96, 132, 196),
            },
        );

        let n = CARDS.len();
        let rows: usize = if two_rows { 2 } else { 1 };
        let per_row = n.div_ceil(rows);
        let row_h = cards_area.height() / rows as u16;
        for row in 0..rows {
            let row_area = Rect::new(
                cards_area.left(),
                cards_area.top() + row as u16 * row_h,
                cards_area.width(),
                row_h,
            );
            let start = row * per_row;
            let end = (start + per_row).min(n);
            if start >= end {
                continue;
            }
            let count = end - start;
            let card_w = (row_area.width() / count as u16).max(9);
            for (offset, i) in (start..end).enumerate() {
                let x = row_area.left() + offset as u16 * card_w;
                if x >= row_area.right() {
                    break;
                }
                let w = card_w.min(row_area.right() - x);
                let card_rect = Rect::new(x, row_area.top(), w, row_area.height());
                let active = self.combat.is_some() && CARDS[i].title == "Mighty Blow";
                draw_card(surface, card_rect, &CARDS[i], self.charges[i], active);
                self.hotspots
                    .push_tappable(card_rect, row_area, Action::Card(i));
            }
        }
    }
}

ascii_tile_demos::demo_main!(PaperDungeon);
