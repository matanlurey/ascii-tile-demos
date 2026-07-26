//! 40: Shard realm -- an Eador: Genesis province map built around one thing:
//! the ornate hero panel.
//!
//! Every other strategy-map demo in this gallery treats its sidebar as
//! secondary chrome next to the map. Eador: Genesis inverts that: the panel
//! showing the hero -- a gilded, framed slab with a portrait bust, a red HP
//! box, carved nameplates, a stat grid, a parchment quest note, and a row of
//! equipment slots -- is the single most recognizable thing on screen, and
//! the province map exists to be looked at *around* it. This demo is that
//! panel, built with real ornamental framing rather than a bordered list, and
//! the province map behind it is deliberately the quieter half of the
//! layout.
//!
//! Techniques on show:
//!
//! - **A hand-carved portrait bust** ([`PORTRAIT`]): hairline, brow, eyes,
//!   and a set jaw drawn as seven rows of character art inside its own frame,
//!   not a letter in a box. The armoured shoulders below it give the
//!   silhouette a distinct outline from every other portrait in the gallery
//!   (compare [`36_court_reigns`](../36_court_reigns)'s mitre/plume/coin/hood
//!   set, which reads iconographic where this reads anatomical).
//! - **Voronoi-style provinces without hexes** ([`build_world`]): every map
//!   cell is assigned to whichever named province seed is nearest, which
//!   gives irregular, HoMM/Eador-shaped province borders on a square grid.
//!   Six other demos in this batch already draw hex maps; this is the
//!   deliberate alternative for a game whose real screen is not hex-gridded.
//! - **Framed site icons** ([`draw_site`]): a mine, a shrine, a lair, and a
//!   shop each get a small bordered 3x3 glyph scattered in their province,
//!   matching the reference screenshot's little site markers. Tapping one
//!   always previews its cost; tapping one the hero currently stands in also
//!   spends the turn's one action to explore it.
//! - **One action per turn** ([`Hero::action_ready`]): move, attack, explore,
//!   or return to the capital, exactly as Eador's hero economy works. The
//!   hero panel always shows which of those is still available this turn.
//! - **A shimmering gilt frame** ([`shimmer`], [`torch_flicker`]): the outer
//!   double-line border brightens in a slow travelling band around its own
//!   perimeter, and the corner and shield ornaments flicker independently on
//!   a per-glyph phase, the two effects the brief asks for layered on the
//!   same frame without disturbing a single stat number or line of text.
//! - **Priority-ordered degradation** ([`ShardRealm::panel_budget`]): the
//!   item grid drops first, then the stat grid, so a squeezed viewport keeps
//!   the portrait, the nameplates, and the quest note -- the panel's own
//!   longest-surviving core -- exactly as the brief specifies.
//!
//! ```sh
//! cargo run --example 40_shard_realm --features crossterm
//! cargo run --example 40_shard_realm --features software
//! cargo run --example 40_shard_realm --features gl
//! cargo run --example 40_shard_realm  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Span};
use ascii_tile_demos::ui::touch::{self, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb};

/// The world's base size in map cells, at the reference scale every
/// [`PROVINCES`] seed is authored against. A viewport smaller than this
/// (portrait phones, the 80x24 headless grid) just shows a window onto it via
/// [`ShardRealm::camera`]; a viewport *larger* than this -- the actual bug
/// this constant used to cause -- would otherwise leave the province map
/// panel's overflow as dead unrendered space, since `province_at` returned
/// `None` for any cell outside a fixed extent. [`ShardRealm::ensure_world`]
/// rescales the whole world (seeds included) up from this base any time the
/// panel is bigger than it, so the map always fills its panel instead of
/// capping out at a fixed pixel size.
const MAP_W: i32 = 72;
/// See [`MAP_W`].
const MAP_H: i32 = 32;

/// A province's terrain, which drives both its base color and its glyph
/// texture. Deliberately three, not the game's full set: enough to make the
/// map read as varied ground without the demo becoming a terrain-generation
/// showcase, which is [`20_realm_map`](../20_realm_map)'s job, not this one's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terrain {
    Plains,
    Forest,
    Hills,
}

impl Terrain {
    const fn base(self) -> Color {
        match self {
            Self::Plains => rgb(84, 98, 52),
            Self::Forest => rgb(38, 68, 40),
            Self::Hills => rgb(94, 76, 50),
        }
    }
}

/// Who holds a province. Colors the terrain with a light tint rather than
/// replacing it, so ownership reads at a glance without the terrain itself
/// becoming illegible, the same rule [`20_realm_map`](../20_realm_map) and
/// the Dominions research both converge on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Player,
    Neutral,
    Enemy,
}

impl Owner {
    fn tint(self, base: Color) -> Color {
        match self {
            Self::Player => mix(base, rgb(214, 182, 92), 0.16),
            Self::Enemy => mix(base, rgb(158, 46, 46), 0.28),
            Self::Neutral => base,
        }
    }
}

/// A location within a province, drawn as a small framed icon on the map
/// (see [`draw_site`]) and as the thing an explore action spends the hero's
/// one action of the turn on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    Mine,
    Shrine,
    Lair,
    Shop,
}

impl Site {
    const fn glyph(self) -> char {
        match self {
            Self::Mine => 'M',
            Self::Shrine => '+',
            Self::Lair => 'X',
            Self::Shop => '$',
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::Shrine => "shrine",
            Self::Lair => "lair",
            Self::Shop => "shop",
        }
    }

    /// What exploring costs, shown by [`ShardRealm::preview_site`] whether or
    /// not the hero is actually standing there -- a player deciding whether a
    /// detour is worth it needs this before committing the turn's one
    /// action, not after.
    const fn cost(self) -> &'static str {
        match self {
            Self::Mine => "guarded by 4 brigands; costs the turn, yields ore income",
            Self::Shrine => "unguarded; costs the turn, may grant a blessing",
            Self::Lair => "guarded by a beast; costs the turn, risks the army",
            Self::Shop => "no risk; costs the turn, opens a trade menu",
        }
    }

    /// Gold and gems awarded for a successful explore. Flat numbers rather
    /// than a combat model: this demo is about the panel and the map, not
    /// about simulating Eador's battle resolution.
    const fn reward(self) -> (u32, u32) {
        match self {
            Self::Mine => (18, 0),
            Self::Shrine => (0, 3),
            Self::Lair => (30, 1),
            Self::Shop => (5, 0),
        }
    }
}

/// One province: a Voronoi seed plus what is drawn and known about it.
struct Province {
    name: &'static str,
    seed: (i32, i32),
    terrain: Terrain,
    owner: Owner,
    site: Option<Site>,
}

/// The shard's provinces. Ten is enough for the map to show real adjacency
/// and variety without becoming a second economy-management demo; province
/// count elsewhere in Eador runs into the hundreds, which is the wrong scale
/// for a screen this panel-dominated.
const PROVINCES: [Province; 10] = [
    Province {
        name: "Kingshome",
        seed: (10, 9),
        terrain: Terrain::Plains,
        owner: Owner::Player,
        site: None,
    },
    Province {
        name: "Millbrook",
        seed: (20, 6),
        terrain: Terrain::Plains,
        owner: Owner::Player,
        site: Some(Site::Shop),
    },
    Province {
        name: "Stonemarch",
        seed: (15, 21),
        terrain: Terrain::Hills,
        owner: Owner::Player,
        site: Some(Site::Mine),
    },
    Province {
        name: "Archers Thicket",
        seed: (34, 11),
        terrain: Terrain::Forest,
        owner: Owner::Neutral,
        site: Some(Site::Shrine),
    },
    Province {
        name: "Wolfsden Hollow",
        seed: (44, 23),
        terrain: Terrain::Forest,
        owner: Owner::Neutral,
        site: Some(Site::Lair),
    },
    Province {
        name: "Cragspire",
        seed: (54, 8),
        terrain: Terrain::Hills,
        owner: Owner::Neutral,
        site: Some(Site::Mine),
    },
    Province {
        name: "Windmere",
        seed: (30, 27),
        terrain: Terrain::Plains,
        owner: Owner::Neutral,
        site: Some(Site::Shop),
    },
    Province {
        name: "Ashwood Vale",
        seed: (47, 4),
        terrain: Terrain::Forest,
        owner: Owner::Neutral,
        site: None,
    },
    Province {
        name: "Ironfall Ridge",
        seed: (61, 26),
        terrain: Terrain::Hills,
        owner: Owner::Neutral,
        site: None,
    },
    Province {
        name: "Blackreed Fen",
        seed: (63, 15),
        terrain: Terrain::Forest,
        owner: Owner::Enemy,
        site: Some(Site::Lair),
    },
];

/// Index into [`PROVINCES`] of the hero's home capital, the destination of
/// the Return action.
const CAPITAL: usize = 0;
/// Index into [`PROVINCES`] of the quest target, matching the reference
/// screenshot's own quest line almost verbatim.
const QUEST_TARGET: usize = 3;

/// Assigns every map cell to its nearest scaled province seed (squared
/// Euclidean distance; there is no need for a real distance metric at this
/// cell count), then derives each province's screen-space bounding box from
/// the result.
///
/// A true Voronoi diagram rather than hand-tiled rectangles because the point
/// of this map is Eador's irregular province shapes -- a rectangle grid would
/// read as a spreadsheet, not a shard. `world_w`/`world_h` and `seeds` are
/// parameters rather than the [`MAP_W`]/[`MAP_H`] constants and [`PROVINCES`]
/// directly, so [`ShardRealm::ensure_world`] can rebuild at a larger scale
/// when the panel is bigger than the base world -- see that method for why a
/// fixed extent used to leave the rest of a large panel blank.
///
/// Cached by the caller and only rebuilt when the panel's size demands a
/// bigger world: the assignment never changes at a fixed scale, and
/// re-deriving it every frame would be thousands of wasted distance checks
/// for a map that never moves.
/// A province's screen-space bounding box, `(minx, miny, maxx, maxy)`.
type BBox = (i32, i32, i32, i32);

fn build_world(world_w: i32, world_h: i32, seeds: &[(i32, i32)]) -> (Vec<u8>, Vec<BBox>) {
    let mut cells = vec![0u8; (world_w * world_h) as usize];
    let mut bbox: Vec<BBox> = seeds.iter().map(|_| (world_w, world_h, -1, -1)).collect();

    for y in 0..world_h {
        for x in 0..world_w {
            let mut best = 0usize;
            let mut best_d = i32::MAX;
            for (i, &(sx, sy)) in seeds.iter().enumerate() {
                let d = (x - sx) * (x - sx) + (y - sy) * (y - sy);
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            cells[(y * world_w + x) as usize] = best as u8;
            let b = &mut bbox[best];
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        }
    }
    (cells, bbox)
}

/// Scales every [`PROVINCES`] seed from the base [`MAP_W`]x[`MAP_H`] extent up
/// to `world_w`x`world_h`, preserving each province's relative position so a
/// bigger world reads as the same shard at a larger size, not a different
/// layout. Integer scaling (not floating point) keeps [`build_world`] and
/// this function trivially deterministic.
fn scaled_seeds(world_w: i32, world_h: i32) -> Vec<(i32, i32)> {
    PROVINCES
        .iter()
        .map(|p| {
            let (sx, sy) = p.seed;
            (sx * world_w / MAP_W, sy * world_h / MAP_H)
        })
        .collect()
}

/// The world position a province's site icon draws at: a fixed offset from
/// the seed so it never overlaps the settlement glyph drawn at the seed
/// itself, clamped inside `(world_w, world_h)` so a seed near an edge doesn't
/// push its site off it.
const fn site_pos(seed: (i32, i32), world_w: i32, world_h: i32) -> (i32, i32) {
    let x = if seed.0 + 4 < world_w {
        seed.0 + 4
    } else {
        seed.0 - 4
    };
    let y = if seed.1 + 2 < world_h {
        seed.1 + 2
    } else {
        seed.1 - 2
    };
    (x, y)
}

/// A hand-carved multi-line portrait: hairline, brow, eyes, nose, a set jaw,
/// then armoured shoulders. Every row is exactly [`PORTRAIT_W`] cells so the
/// caller can print each line at a fixed left column without measuring.
///
/// The shape (widow's peak, angular brow, square jaw, blocky pauldrons) is
/// chosen to silhouette differently from every other bust in the gallery: no
/// mitre, no plume, no round coin-eyes, no soft hood. See the module docs.
const PORTRAIT: [&str; 9] = [
    "  .-===-.  ",
    " /  ^^^  \\ ",
    "|  o   o  |",
    "|    ^    |",
    "|   ===   |",
    " \\   -   / ",
    "  '-___-'  ",
    " /#######\\ ",
    "|##  |  ##|",
];
/// Width of every [`PORTRAIT`] row, in cells.
const PORTRAIT_W: u16 = 11;

/// A small heraldic shield ornament flanking the portrait on both sides.
/// Reused mirrored (its own shape is symmetric enough) rather than authoring
/// a second one, since the point is the flanking silhouette, not left/right
/// asymmetry.
const SHIELD: [&str; 5] = [" .-. ", "/ o \\", "| ^ |", "\\   /", " '-' "];
/// Width of every [`SHIELD`] row, in cells.
const SHIELD_W: u16 = 5;

/// One equipment slot's contents.
#[derive(Clone, Copy)]
struct Item {
    name: &'static str,
    glyph: char,
    desc: &'static str,
}

/// The hero's 4x2 equipment grid, left to right then top to bottom. `None`
/// slots are empty, which the reference screenshot also shows (not every
/// slot is always filled).
const ITEMS: [Option<Item>; 8] = [
    Some(Item {
        name: "Iron Blade",
        glyph: '/',
        desc: "+2 attack in melee.",
    }),
    Some(Item {
        name: "Round Shield",
        glyph: 'O',
        desc: "+3 defense vs melee.",
    }),
    Some(Item {
        name: "Steel Cap",
        glyph: '^',
        desc: "+1 defense, +1 morale.",
    }),
    Some(Item {
        name: "Swift Boots",
        glyph: '=',
        desc: "+1 movement per turn.",
    }),
    Some(Item {
        name: "Sigil Ring",
        glyph: 'o',
        desc: "+1 leadership.",
    }),
    Some(Item {
        name: "Grey Cloak",
        glyph: '~',
        desc: "+1 initiative in forest.",
    }),
    Some(Item {
        name: "Healing Draught",
        glyph: '!',
        desc: "Restores 20 HP when used.",
    }),
    None,
];

/// The one action a hero may spend per turn, per Eador's own rule: move,
/// attack, explore, or return to the capital. [`ShardRealm`] tracks only
/// whether the action is still available; which of the four it becomes is
/// decided by what gets tapped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Move,
    Attack,
    Explore,
    Return,
}

impl ActionKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Move => "MOVE",
            Self::Attack => "ATTACK",
            Self::Explore => "EXPLORE",
            Self::Return => "RETURN",
        }
    }
}

/// The hero piece on the map, and the panel's own state.
struct Hero {
    name: &'static str,
    class: &'static str,
    hp: u8,
    hp_max: u8,
    stats: [(&'static str, u8); 6],
    items: [Option<Item>; 8],
    province: usize,
    /// `None` once the action has been spent this turn; `Some` names which
    /// kind it was spent as, shown dimmed in the panel until End Turn.
    last_action: Option<ActionKind>,
    action_ready: bool,
}

impl Hero {
    const fn new() -> Self {
        Self {
            name: "Kessa Vondrak",
            class: "Commander",
            hp: 42,
            hp_max: 60,
            stats: [
                ("STR", 14),
                ("DEF", 11),
                ("INI", 9),
                ("MOV", 4),
                ("LDR", 6),
                ("LCK", 7),
            ],
            items: ITEMS,
            province: CAPITAL,
            last_action: None,
            action_ready: true,
        }
    }

    const fn spend(&mut self, kind: ActionKind) {
        self.action_ready = false;
        self.last_action = Some(kind);
    }
}

/// What a tap hit, resolved from this frame's [`Hotspots`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Province(usize),
    Site(usize),
    Item(usize),
    Return,
    EndTurn,
}

/// Demo state: the generated province Voronoi, the hero and their panel's
/// live values, the shard's economy, and this frame's input plumbing.
pub struct ShardRealm {
    /// Province index per map cell; see [`build_world`].
    cells: Vec<u8>,
    /// Each province's `(minx, miny, maxx, maxy)` world-space bounding box.
    bbox: Vec<(i32, i32, i32, i32)>,
    /// The province seeds actually in effect, at [`Self::world_w`]x
    /// [`Self::world_h`] scale -- each [`PROVINCES`] entry's own `seed`
    /// scaled up by [`scaled_seeds`] once the map panel outgrows the base
    /// [`MAP_W`]x[`MAP_H`] extent. Indexed exactly like [`PROVINCES`].
    seeds: Vec<(i32, i32)>,
    /// The world's current extent in map cells; starts at [`MAP_W`]x[`MAP_H`]
    /// and only grows, via [`Self::ensure_world`], to whatever the map panel
    /// has actually needed so far.
    world_w: i32,
    /// See [`Self::world_w`].
    world_h: i32,
    hero: Hero,
    gold: u32,
    gems: u32,
    turn: u32,
    /// Overrides the quest line with a transient tooltip: what a tapped site
    /// would cost, or what a tapped item does. Cleared by tapping the map
    /// background, so the quest is never permanently lost behind a tooltip.
    info: Option<(String, Color)>,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Hit>,
    fps: FpsMeter,
}

impl Default for ShardRealm {
    fn default() -> Self {
        let seeds = PROVINCES.iter().map(|p| p.seed).collect::<Vec<_>>();
        let (cells, bbox) = build_world(MAP_W, MAP_H, &seeds);
        Self {
            cells,
            bbox,
            seeds,
            world_w: MAP_W,
            world_h: MAP_H,
            hero: Hero::new(),
            gold: 240,
            gems: 12,
            turn: 1,
            info: None,
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl ShardRealm {
    /// Regenerates the Voronoi world at a bigger scale if the map panel is
    /// now larger than the current world extent, so the province map always
    /// fills its panel instead of capping out once the panel outgrows the
    /// base [`MAP_W`]x[`MAP_H`] size (a real bug: a large landscape/desktop
    /// panel used to render the world only in its top-left corner and leave
    /// the rest of the panel as unrendered black space, since
    /// [`Self::province_at`] returned `None` past a fixed extent).
    ///
    /// Only rebuilds when `visible` actually exceeds the current world, and
    /// only grows (never shrinks) the cached extent, so a panel that
    /// oscillates around a size at or below the current world never pays
    /// the rebuild cost twice. The rebuild itself is deterministic: the same
    /// `visible` size always yields the same rescaled world, since
    /// [`scaled_seeds`] is a pure integer scaling of [`PROVINCES`].
    fn ensure_world(&mut self, visible: (i32, i32)) {
        let need_w = self.world_w.max(visible.0);
        let need_h = self.world_h.max(visible.1);
        if need_w <= self.world_w && need_h <= self.world_h {
            return;
        }
        self.world_w = need_w;
        self.world_h = need_h;
        self.seeds = scaled_seeds(self.world_w, self.world_h);
        let (cells, bbox) = build_world(self.world_w, self.world_h, &self.seeds);
        self.cells = cells;
        self.bbox = bbox;
    }

    /// The camera offset that centers the map on the hero's current
    /// province, clamped so the visible window never runs off the world.
    /// Following the hero rather than scrolling by hand keeps a moved hero
    /// always inside the (fixed-size, non-scrollable) map panel, which
    /// matters because there is no pan gesture to fall back on here -- the
    /// map's whole interaction surface is tapping provinces and sites.
    fn camera(&self, visible: (i32, i32)) -> (i32, i32) {
        let (sx, sy) = self.seeds[self.hero.province];
        let x = (sx - visible.0 / 2).clamp(0, (self.world_w - visible.0).max(0));
        let y = (sy - visible.1 / 2).clamp(0, (self.world_h - visible.1).max(0));
        (x, y)
    }

    fn province_at(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.world_w || y >= self.world_h {
            return None;
        }
        Some(self.cells[(y * self.world_w + x) as usize] as usize)
    }

    /// Previews a site's cost without spending anything: shown for any site
    /// tap, whether or not the hero is currently there.
    fn preview_site(&mut self, province: usize) {
        let Some(site) = PROVINCES[province].site else {
            return;
        };
        self.info = Some((
            format!(
                "{} ({}): {}",
                PROVINCES[province].name,
                site.label(),
                site.cost()
            ),
            rgb(224, 196, 118),
        ));
    }

    /// Explores the site in `province` if the hero is standing there and the
    /// action is still available; otherwise falls back to a preview. This is
    /// what makes tapping a site do double duty: a cost preview from afar, a
    /// real explore action once the hero has actually moved there.
    fn tap_site(&mut self, province: usize) {
        let Some(site) = PROVINCES[province].site else {
            return;
        };
        if province != self.hero.province || !self.hero.action_ready {
            self.preview_site(province);
            return;
        }
        let (g, gem) = site.reward();
        self.gold += g;
        self.gems += gem;
        self.hero.spend(ActionKind::Explore);
        self.info = Some((
            format!(
                "Explored the {} at {}: +{g} gold, +{gem} gems.",
                site.label(),
                PROVINCES[province].name
            ),
            rgb(140, 214, 140),
        ));
    }

    /// Orders a move (or, onto an enemy province, an attack) if the action is
    /// still available this turn.
    fn tap_province(&mut self, province: usize) {
        if province == self.hero.province || !self.hero.action_ready {
            return;
        }
        if PROVINCES[province].owner == Owner::Enemy {
            self.hero.spend(ActionKind::Attack);
            self.info = Some((
                format!(
                    "Attacked {}. The garrison is driven off.",
                    PROVINCES[province].name
                ),
                rgb(214, 130, 110),
            ));
            self.hero.province = province;
            return;
        }
        self.hero.province = province;
        self.hero.spend(ActionKind::Move);
        if province == QUEST_TARGET {
            self.info = Some((
                "Quest complete! Return to the capital to claim your reward.".to_owned(),
                rgb(224, 196, 118),
            ));
        }
    }

    fn tap_return(&mut self) {
        if !self.hero.action_ready || self.hero.province == CAPITAL {
            return;
        }
        self.hero.province = CAPITAL;
        self.hero.spend(ActionKind::Return);
        self.info = Some(("Returned to the capital.".to_owned(), rgb(180, 190, 214)));
    }

    const fn end_turn(&mut self) {
        self.turn += 1;
        self.hero.action_ready = true;
        self.hero.last_action = None;
        self.gold += 10;
    }

    fn tap_item(&mut self, slot: usize) {
        match self.hero.items.get(slot).copied().flatten() {
            Some(item) => {
                self.info = Some((format!("{}: {}", item.name, item.desc), rgb(210, 208, 224)));
            }
            None => self.info = None,
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            self.pointer.feed(&event);
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Char('r' | 'R') => self.tap_return(),
                    KeyCode::Enter | KeyCode::Char(' ') => self.end_turn(),
                    _ => {}
                }
            }
        }

        let gesture = self.pointer.take();
        if let Some(tap) = gesture.tap
            && let Some(hit) = self.hotspots.hit(tap).copied()
        {
            match hit {
                Hit::Province(i) => self.tap_province(i),
                Hit::Site(i) => self.tap_site(i),
                Hit::Item(i) => self.tap_item(i),
                Hit::Return => self.tap_return(),
                Hit::EndTurn => self.end_turn(),
            }
        }
        true
    }

    /// Draws the map into `area` and registers province/site hotspots,
    /// centered on the hero's own province; see [`Self::camera`].
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = panel::Panel::new()
            .title("Province Map")
            .border(panel::Border::Single)
            .draw(surface, area);
        if panel.width() < 4 || panel.height() < 4 {
            return;
        }
        let visible = (i32::from(panel.width()), i32::from(panel.height()));
        let (ox, oy) = self.camera(visible);

        for sy in 0..panel.height() {
            for sx in 0..panel.width() {
                let (wx, wy) = (ox + i32::from(sx), oy + i32::from(sy));
                let Some(pi) = self.province_at(wx, wy) else {
                    continue;
                };
                let (glyph, fg) = terrain_glyph(&PROVINCES[pi], wx, wy, self.time);
                surface.put(
                    (panel.left() + sx, panel.top() + sy),
                    glyph,
                    Style::new().fg(fg).bg(rgb(8, 10, 8)),
                );
            }
        }

        for (i, province) in PROVINCES.iter().enumerate() {
            let seed = self.seeds[i];
            let (sx, sy) = (seed.0 - ox, seed.1 - oy);
            if sx >= 0 && sy >= 0 && sx < visible.0 && sy < visible.1 {
                let at = (panel.left() + sx as u16, panel.top() + sy as u16);
                let color = province.owner.tint(rgb(224, 214, 190));
                surface.put(at, '\u{25B2}', Style::new().fg(color).bg(rgb(8, 10, 8)));
                if sy + 1 < visible.1 {
                    surface.put(
                        (at.0, at.1 + 1),
                        '\u{2588}',
                        Style::new().fg(color).bg(rgb(8, 10, 8)),
                    );
                }
            }

            if let Some(site) = province.site {
                let world = (self.world_w, self.world_h);
                draw_site(surface, panel, (ox, oy), seed, world, site, self.time);
            }
        }

        self.draw_hero_marker(surface, panel, (ox, oy));
    }

    /// Registers a tappable hotspot for every province's site icon, grown to
    /// a legal touch target even though the icon itself is tiny, per
    /// [`touch::tappable`]. Kept separate from [`draw_site`] (a free
    /// function, using the same [`site_screen_rect`] geometry) because
    /// hotspots must be registered before this frame's input is read, while
    /// drawing happens afterward once the surface is borrowed; see `tick`.
    fn register_site_hotspots(&mut self, panel: Rect) {
        if panel.width() < 4 || panel.height() < 4 {
            return;
        }
        let visible = (i32::from(panel.width()), i32::from(panel.height()));
        let offset = self.camera(visible);
        let world = (self.world_w, self.world_h);
        for (i, province) in PROVINCES.iter().enumerate() {
            if province.site.is_none() {
                continue;
            }
            if let Some(rect) = site_screen_rect(panel, offset, self.seeds[i], world) {
                self.hotspots.push_tappable(rect, panel, Hit::Site(i));
            }
        }
    }

    /// The hero's own marker: a bright glyph over its own dark tile, distinct
    /// from the flat settlement icons so the eye finds "where is my hero"
    /// first among everything else on the map.
    fn draw_hero_marker(&self, surface: &mut Surface<'_>, panel: Rect, offset: (i32, i32)) {
        let (wx, wy) = self.seeds[self.hero.province];
        let (sx, sy) = (wx - offset.0, wy - offset.1 - 2);
        if sx < 0 || sy < 0 || sx >= i32::from(panel.width()) || sy >= i32::from(panel.height()) {
            return;
        }
        let at = (panel.left() + sx as u16, panel.top() + sy as u16);
        surface.put(
            at,
            '\u{263A}',
            Style::new().fg(rgb(20, 16, 10)).bg(rgb(246, 196, 96)),
        );
    }

    /// Registers every province's own hotspot from its precomputed bounding
    /// box, before any input is read this frame -- rebuilt fresh every frame
    /// like every other demo's [`Hotspots`], never retained.
    fn register_map_hotspots(&mut self, panel: Rect) {
        if panel.width() < 4 || panel.height() < 4 {
            return;
        }
        let visible = (i32::from(panel.width()), i32::from(panel.height()));
        let (ox, oy) = self.camera(visible);
        for (i, &(minx, miny, maxx, maxy)) in self.bbox.iter().enumerate() {
            let l = (minx - ox).max(0);
            let t = (miny - oy).max(0);
            let r = (maxx - ox + 1).min(visible.0);
            let b = (maxy - oy + 1).min(visible.1);
            if r <= l || b <= t {
                continue;
            }
            let rect = Rect::new(
                panel.left() + l as u16,
                panel.top() + t as u16,
                (r - l) as u16,
                (b - t) as u16,
            );
            self.hotspots.push_tappable(rect, panel, Hit::Province(i));
        }
    }

    /// Height of every fixed-size section above the quest box: portrait,
    /// HP box, two nameplates, a divider, and the action line. The quest
    /// box itself is *not* fixed -- see [`Self::quest_box_h`] -- so it is not
    /// part of this constant.
    const FIXED_HEAD_H: u16 = PORTRAIT.len() as u16 + 3 + 2 + 1 + 1;

    /// Which of the panel's optional sections fit in `interior_h`, dropping
    /// the item grid first and then the stat grid -- the priority order the
    /// brief asks for, so the portrait, nameplates, and quest note are always
    /// the last things to go.
    const fn panel_budget(interior_h: u16) -> (bool, bool) {
        const MIN_QUEST: u16 = 3;
        let core = Self::FIXED_HEAD_H + MIN_QUEST;
        let with_stats = interior_h >= core + 1 + 4;
        let with_items = with_stats && interior_h >= core + 1 + 4 + 1 + ITEM_GRID_H;
        (with_stats, with_items)
    }

    /// How tall the quest box should be drawn, given the panel's own
    /// interior height and which optional sections will follow it: whatever
    /// is left over after every fixed-size section and any shown grid is
    /// reserved, so free vertical room becomes more quest/tooltip text
    /// instead of a dead gap below a grid that stopped growing. Never
    /// shrinks below the 3 rows [`Self::panel_budget`] already guarantees a
    /// room for.
    fn quest_box_h(interior_h: u16, with_stats: bool, with_items: bool) -> u16 {
        let mut reserved = Self::FIXED_HEAD_H;
        if with_stats {
            reserved += 1 + 4;
        }
        if with_items {
            reserved += 1 + ITEM_GRID_H;
        }
        interior_h.saturating_sub(reserved).max(3)
    }

    /// Draws the whole ornate hero panel: the frame first, then each section
    /// in priority order within whatever interior height [`Self::panel_budget`]
    /// says is available.
    fn draw_hero_panel(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = draw_ornate_frame(surface, area, self.time, "Hero");
        if inner.width() < PORTRAIT_W + 2 * SHIELD_W || inner.height() < 10 {
            return;
        }
        let (with_stats, with_items) = Self::panel_budget(inner.height());
        let quest_h = Self::quest_box_h(inner.height(), with_stats, with_items);

        let mut y = inner.top();
        y = self.draw_portrait_row(surface, inner, y);
        y = self.draw_hp_box(surface, inner, y);
        y = self.draw_nameplates(surface, inner, y);
        y = Self::draw_divider(surface, inner, y);
        y = self.draw_action_line(surface, inner, y);
        y = self.draw_quest_box(surface, inner, y, quest_h);

        if with_stats {
            y = Self::draw_divider(surface, inner, y);
            y = self.draw_stat_grid(surface, inner, y);
        }
        if with_items {
            y = Self::draw_divider(surface, inner, y);
            self.draw_item_grid(surface, inner, y);
        }
    }

    fn draw_portrait_row(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        let rows = PORTRAIT.len() as u16;
        if y0 + rows > inner.bottom() {
            return y0;
        }
        let block_w = PORTRAIT_W + 2 * SHIELD_W + 2;
        let x0 = inner.left() + (inner.width().saturating_sub(block_w)) / 2;
        let bg = panel::PANEL_BG;

        for (row, line) in PORTRAIT.iter().enumerate() {
            let y = y0 + row as u16;
            surface.print(
                (x0 + SHIELD_W + 1, y),
                line,
                Style::new().fg(rgb(206, 188, 150)).bg(bg),
            );
        }

        // Shields occupy the middle rows of the portrait block, flanking it
        // left and right; a torch-flicker phase keyed to each cell's own
        // position keeps the ornament visibly animating without moving any
        // text drawn elsewhere in the panel this frame.
        let shield_y0 = y0 + 1;
        for (row, line) in SHIELD.iter().enumerate() {
            let y = shield_y0 + row as u16;
            if y >= y0 + rows {
                break;
            }
            let flicker = torch_flicker(self.time, (row as u32).wrapping_mul(7).wrapping_add(3));
            let fg = mix(rgb(120, 110, 70), rgb(230, 196, 110), flicker);
            surface.print((x0, y), line, Style::new().fg(fg).bg(bg));
            surface.print(
                (x0 + SHIELD_W + PORTRAIT_W + 1, y),
                line,
                Style::new().fg(fg).bg(bg),
            );
        }

        y0 + rows
    }

    fn draw_hp_box(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        if y0 + 3 > inner.bottom() {
            return y0;
        }
        let w = inner.width().min(24);
        let x0 = inner.left() + (inner.width().saturating_sub(w)) / 2;
        let rect = Rect::new(x0, y0, w, 3);
        let frame_fg = rgb(150, 60, 56);
        let bg = rgb(46, 16, 16);
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        for x in rect.left()..rect.right() {
            surface.put(
                (x, rect.top()),
                '\u{2500}',
                Style::new().fg(frame_fg).bg(bg),
            );
            surface.put(
                (x, rect.bottom() - 1),
                '\u{2500}',
                Style::new().fg(frame_fg).bg(bg),
            );
        }
        surface.put(
            (rect.left(), rect.top()),
            '\u{250C}',
            Style::new().fg(frame_fg).bg(bg),
        );
        surface.put(
            (rect.right() - 1, rect.top()),
            '\u{2510}',
            Style::new().fg(frame_fg).bg(bg),
        );
        surface.put(
            (rect.left(), rect.bottom() - 1),
            '\u{2514}',
            Style::new().fg(frame_fg).bg(bg),
        );
        surface.put(
            (rect.right() - 1, rect.bottom() - 1),
            '\u{2518}',
            Style::new().fg(frame_fg).bg(bg),
        );

        let text = format!("HP {}/{}", self.hero.hp, self.hero.hp_max);
        let bar_w = rect
            .width()
            .saturating_sub(text.chars().count() as u16 + 3)
            .max(3);
        surface.print(
            (rect.left() + 1, rect.top() + 1),
            &text,
            Style::new().fg(rgb(240, 210, 200)).bg(bg),
        );
        let t = f32::from(self.hero.hp) / f32::from(self.hero.hp_max.max(1));
        panel::bar(
            surface,
            (
                rect.left() + text.chars().count() as u16 + 2,
                rect.top() + 1,
            ),
            bar_w,
            t,
            rgb(214, 90, 84),
            rgb(70, 24, 24),
        );
        y0 + 3
    }

    /// Two carved nameplate bars: the hero's name, then their class, each
    /// framed by mixed single/double box-drawing junctions so they read as
    /// inset plaques rather than plain centered text.
    fn draw_nameplates(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        let bg = panel::PANEL_BG;
        let mut y = y0;
        for (text, color) in [
            (self.hero.name.to_owned(), rgb(240, 220, 160)),
            (self.hero.class.to_owned(), rgb(190, 196, 214)),
        ] {
            if y >= inner.bottom() {
                break;
            }
            let plate = format!("\u{255E}\u{2550} {text} \u{2550}\u{2561}");
            let x = inner.left() + (inner.width().saturating_sub(plate.chars().count() as u16)) / 2;
            surface.print((x, y), &plate, Style::new().fg(color).bg(bg));
            y += 1;
        }
        y
    }

    /// A one-row ornamental rule with a gem at its centre, used both as a
    /// section divider and (via [`draw_ornate_frame`]'s corners) as the
    /// visual grammar the whole panel repeats.
    fn draw_divider(surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        if y0 >= inner.bottom() {
            return y0;
        }
        let bg = panel::PANEL_BG;
        let w = inner.width_usize();
        let mid = w / 2;
        for x in 0..w as u16 {
            let ch = if usize::from(x) == mid {
                '\u{2666}'
            } else {
                '\u{2500}'
            };
            surface.put(
                (inner.left() + x, y0),
                ch,
                Style::new().fg(rgb(96, 84, 56)).bg(bg),
            );
        }
        y0 + 1
    }

    /// Shows which of the four Eador actions the hero still has this turn,
    /// or that it has been spent and End Turn is the only way forward -- the
    /// live state the brief requires the panel to actually reflect.
    fn draw_action_line(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        if y0 >= inner.bottom() {
            return y0;
        }
        let bg = panel::PANEL_BG;
        let width = usize::from(inner.width());
        let color = if self.hero.action_ready {
            rgb(150, 214, 150)
        } else {
            rgb(150, 100, 90)
        };
        let candidates: [String; 3] = if self.hero.action_ready {
            [
                "Action: move / attack / explore / return".to_owned(),
                "Action: move/attack/explore/return".to_owned(),
                "Action ready".to_owned(),
            ]
        } else {
            let spent = self.hero.last_action.map_or("spent", ActionKind::label);
            [
                format!("Action used ({spent}) -- End Turn"),
                format!("Used ({spent}) -- End Turn"),
                "Action spent".to_owned(),
            ]
        };
        // Fit the widest candidate that isn't wider than the panel: the
        // action line has to state which turn state it is ("ready" versus
        // "spent") legibly at every panel width, and mid-word truncation
        // (which `panel::spans` would otherwise do) reads as a bug, not a
        // narrower panel -- see the `40_shard_realm` layout-fix report for
        // the clipped "... retu" this replaces.
        let text = candidates
            .iter()
            .find(|c| c.chars().count() <= width)
            .unwrap_or(&candidates[2]);
        panel::spans(
            surface,
            (inner.left(), y0),
            inner.width(),
            &[Span::new(text, color)],
            bg,
        );
        y0 + 1
    }

    /// The parchment quest note, or -- when the player has just tapped a site
    /// or an item -- a transient tooltip in its place. Overriding rather than
    /// appending keeps the panel's height budget fixed regardless of what is
    /// selected.
    fn draw_quest_box(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16, quest_h: u16) -> u16 {
        let h = quest_h.min(inner.bottom().saturating_sub(y0));
        if h == 0 {
            return y0;
        }
        let rect = Rect::new(inner.left(), y0, inner.width(), h);
        let bg = rgb(58, 46, 26);
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let frame_fg = rgb(150, 120, 70);
        for x in rect.left()..rect.right() {
            surface.put((x, rect.top()), '~', Style::new().fg(frame_fg).bg(bg));
        }
        let (text, color) = self.info.clone().unwrap_or_else(|| {
            (
                format!("Quest: Go to Province {}", PROVINCES[QUEST_TARGET].name),
                rgb(230, 210, 150),
            )
        });
        if rect.height() > 1 {
            wrap_into(surface, rect, 1, &text, color, bg);
        }
        y0 + h
    }

    /// A 2x3 grid of stat boxes: STR/DEF/INI on top, MOV/LDR/LCK below,
    /// matching the reference screenshot's two rows of small stat boxes.
    fn draw_stat_grid(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) -> u16 {
        let h = 4u16.min(inner.bottom().saturating_sub(y0));
        if h < 4 {
            return y0;
        }
        let rect = Rect::new(inner.left(), y0, inner.width(), h);
        let bg = panel::PANEL_BG;
        let frame_fg = rgb(90, 96, 116);
        draw_box(surface, rect, frame_fg, bg);

        let cols = panel::columns(
            Rect::new(rect.left() + 1, rect.top() + 1, rect.width() - 2, 2),
            3,
            1,
        );
        for (i, (label, value)) in self.hero.stats.iter().enumerate() {
            let col = &cols[i % 3];
            let row = u16::try_from(i / 3).unwrap_or(0);
            let text = format!("{label} {value}");
            surface.print(
                (col.left(), col.top() + row),
                &text,
                Style::new().fg(rgb(210, 214, 226)).bg(bg),
            );
        }
        y0 + h
    }

    /// The 4x2 equipment grid along the bottom, one framed cell per slot,
    /// empty slots drawn dim rather than omitted so the grid shape itself
    /// stays legible. Hotspots for the slots are registered separately by
    /// [`Self::register_item_hotspots`], from the same rect math, before
    /// events are read this frame; see that function for why the two are
    /// split rather than combined here.
    fn draw_item_grid(&self, surface: &mut Surface<'_>, inner: Rect, y0: u16) {
        let Some(rect) = item_grid_rect(inner, y0) else {
            return;
        };
        let bg = panel::PANEL_BG;
        draw_box(surface, rect, rgb(90, 96, 116), bg);

        for slot in 0..8u16 {
            let Some((x, y)) = item_cell_pos(rect, slot) else {
                continue;
            };
            let item = self.hero.items[usize::from(slot)];
            let (glyph, color) =
                item.map_or(('.', rgb(70, 72, 84)), |it| (it.glyph, rgb(226, 200, 130)));
            surface.print(
                (x, y),
                &format!("[{glyph}]"),
                Style::new().fg(color).bg(rgb(30, 30, 40)),
            );
        }
    }

    /// Registers the item grid's tap targets from the same geometry
    /// [`Self::draw_item_grid`] draws from, computed on the same rect so the
    /// two can never disagree. Kept separate from drawing because hotspots
    /// must exist *before* `handle_events` reads this frame's tap, while the
    /// surface they would draw into is only borrowed afterward -- see the
    /// `tick` method.
    fn register_item_hotspots(&mut self, inner: Rect, y0: u16, bounds: Rect) {
        let Some(rect) = item_grid_rect(inner, y0) else {
            return;
        };
        let cell_w = (rect.width().saturating_sub(2)) / 4;
        for slot in 0..8u16 {
            let Some((x, y)) = item_cell_pos(rect, slot) else {
                continue;
            };
            let cell_rect = Rect::new(x, y, cell_w.saturating_sub(1).max(3), 2);
            self.hotspots
                .push_tappable(cell_rect, bounds, Hit::Item(usize::from(slot)));
        }
    }

    /// The bottom game bar: gold and gems on the left, the current province
    /// name centred, and a row of icon buttons on the right -- the chrome the
    /// reference screenshot's own bottom bar shows, separate from the
    /// gallery's universal FPS/keys status line below it.
    fn draw_game_bar(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        panel::band(surface, area);
        let bg = ui::CHROME_BG;
        let y = area.top() + area.height() / 2;

        let left = format!(
            "Gold {}   Gems {}   Turn {}",
            self.gold, self.gems, self.turn
        );
        panel::spans(
            surface,
            (area.left() + 1, y),
            area.width() / 3,
            &[Span::keyword(&left)],
            bg,
        );

        let name = PROVINCES[self.hero.province].name;
        let name_x = area.left() + (area.width().saturating_sub(name.chars().count() as u16)) / 2;
        surface.print((name_x, y), name, Style::new().fg(ui::FG).bg(bg));

        for (rect, label, hit) in band_button_rects(area) {
            let enabled = !matches!(hit, Hit::Return) || self.hero.action_ready;
            let fg = if enabled { ui::ACCENT } else { ui::DIM };
            surface.fill_rect(rect, ' ', Style::new().bg(rgb(30, 30, 42)));
            panel::spans(
                surface,
                (rect.left(), rect.top() + rect.height() / 2),
                rect.width(),
                &[Span::new(label, fg)],
                rgb(30, 30, 42),
            );
        }
    }

    /// Registers the bottom bar's icon buttons from the same geometry
    /// [`Self::draw_game_bar`] draws from; see [`band_button_rects`] and the
    /// note on [`Self::register_site_hotspots`] for why this is split out.
    fn register_band_hotspots(&mut self, area: Rect) {
        for (rect, _, hit) in band_button_rects(area) {
            self.hotspots.push_tappable(rect, area, hit);
        }
    }

    fn status(&self) -> String {
        format!(
            "turn {}  {} the {}  hp {}/{}",
            self.turn, self.hero.name, self.hero.class, self.hero.hp, self.hero.hp_max
        )
    }
}

/// The screen-space rect a province's site icon occupies, given the map
/// panel, the current camera `offset`, and the live world size -- or `None`
/// if it would fall outside the visible window. Shared by [`draw_site`] and
/// [`ShardRealm::register_site_hotspots`] so drawing and hit-testing can
/// never disagree about where the icon actually is.
fn site_screen_rect(
    panel: Rect,
    offset: (i32, i32),
    seed: (i32, i32),
    world: (i32, i32),
) -> Option<Rect> {
    let (wx, wy) = site_pos(seed, world.0, world.1);
    let (sx, sy) = (wx - offset.0, wy - offset.1);
    if sx < 1 || sy < 1 || sx + 1 >= i32::from(panel.width()) || sy + 1 >= i32::from(panel.height())
    {
        return None;
    }
    Some(Rect::new(
        panel.left() + sx as u16 - 1,
        panel.top() + sy as u16 - 1,
        3,
        3,
    ))
}

/// Draws one province's site icon: a small framed 3x3 glyph (a mine, shrine,
/// lair, or shop), matching the reference screenshot's bordered site
/// markers. A free function taking its rect from [`site_screen_rect`] rather
/// than a method that also registers a hotspot, because hotspots have to
/// exist before this frame's input is read while drawing happens afterward;
/// see [`ShardRealm::register_site_hotspots`].
fn draw_site(
    surface: &mut Surface<'_>,
    panel: Rect,
    offset: (i32, i32),
    seed: (i32, i32),
    world: (i32, i32),
    site: Site,
    time: f32,
) {
    let Some(rect) = site_screen_rect(panel, offset, seed, world) else {
        return;
    };
    draw_site_icon(surface, rect, site, time);
}

/// Draws the 3x3 framed glyph into an already-resolved `rect`.
fn draw_site_icon(surface: &mut Surface<'_>, rect: Rect, site: Site, time: f32) {
    let flicker = torch_flicker(
        time,
        u32::from(rect.left()) ^ u32::from(rect.top()).rotate_left(9),
    );
    let fg = mix(rgb(140, 118, 70), rgb(224, 196, 118), flicker);
    let bg = rgb(20, 16, 10);
    let corners = ['\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}'];
    surface.put(
        (rect.left(), rect.top()),
        corners[0],
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.right() - 1, rect.top()),
        corners[1],
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left(), rect.bottom() - 1),
        corners[2],
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.right() - 1, rect.bottom() - 1),
        corners[3],
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left() + 1, rect.top()),
        '\u{2500}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left() + 1, rect.bottom() - 1),
        '\u{2500}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left(), rect.top() + 1),
        '\u{2502}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.right() - 1, rect.top() + 1),
        '\u{2502}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left() + 1, rect.top() + 1),
        site.glyph(),
        Style::new().fg(rgb(232, 220, 180)).bg(bg),
    );
}

/// Height of the item grid's own box: a top border, two 2-row-tall slot
/// rows (see [`item_cell_pos`]'s `row * 2` spacing), and a bottom border.
/// This used to be a plain `4`, which fits the border and the *first* slot
/// row only -- [`item_cell_pos`] silently dropped the second row (slots 4-7)
/// every time, and the panel was left with dead space below a half-drawn
/// box instead of either showing the whole grid or reclaiming the room.
/// Named so [`ShardRealm::panel_budget`] and [`layout_items_y0`] can share
/// the exact same height the box is actually drawn at.
const ITEM_GRID_H: u16 = 6;

/// The rect a panel's 4x2 item grid box occupies, or `None` if it does not
/// fit within `inner` starting at `y0`. Shared by
/// [`ShardRealm::draw_item_grid`] and [`ShardRealm::register_item_hotspots`].
fn item_grid_rect(inner: Rect, y0: u16) -> Option<Rect> {
    if y0 + ITEM_GRID_H > inner.bottom() {
        return None;
    }
    Some(Rect::new(inner.left(), y0, inner.width(), ITEM_GRID_H))
}

/// One item slot's top-left cell position within an already-resolved item
/// grid `rect`, or `None` if the row does not fit.
fn item_cell_pos(rect: Rect, slot: u16) -> Option<(u16, u16)> {
    let cell_w = (rect.width().saturating_sub(2)) / 4;
    if cell_w == 0 {
        return None;
    }
    let col = slot % 4;
    let row = slot / 4;
    let x = rect.left() + 1 + col * cell_w;
    let y = rect.top() + 1 + row * 2;
    if y + 1 >= rect.bottom() {
        return None;
    }
    Some((x, y))
}

/// The bottom bar's two icon buttons -- Return and End Turn -- as
/// `(rect, label, hit)` triples. A free function so [`ShardRealm::draw_game_bar`]
/// and [`ShardRealm::register_band_hotspots`] compute identical rects.
fn band_button_rects(area: Rect) -> [(Rect, &'static str, Hit); 2] {
    let btn_w = 9u16.min(area.width() / 4).max(4);
    let btn_h = area.height().clamp(1, touch::TAP_H);
    let labels: [(&str, Hit); 2] = [("Return", Hit::Return), ("EndTurn", Hit::EndTurn)];
    let x0 = area.right().saturating_sub(btn_w * labels.len() as u16 + 1);
    [
        (
            Rect::new(x0, area.top(), btn_w, btn_h),
            labels[0].0,
            labels[0].1,
        ),
        (
            Rect::new(x0 + btn_w, area.top(), btn_w, btn_h),
            labels[1].0,
            labels[1].1,
        ),
    ]
}

/// The hero panel's interior rect inside its ornamental frame, without
/// drawing anything. Shared by [`draw_ornate_frame`] (which draws the frame
/// and returns the same rect) and [`layout_items_y0`] (which needs to know
/// the interior before the frame has actually been drawn this frame).
const fn frame_interior(area: Rect) -> Rect {
    if area.width() < 4 || area.height() < 4 {
        return Rect::new(area.left(), area.top(), 0, 0);
    }
    Rect::new(
        area.left() + 1,
        area.top() + 1,
        area.width() - 2,
        area.height() - 2,
    )
}

/// Where the item grid would start inside `inner`, mirroring the fixed
/// section heights [`ShardRealm::draw_hero_panel`] lays out in sequence, or
/// `None` if [`ShardRealm::panel_budget`] says the grid is dropped. Kept as
/// a pure function (no drawing) so hotspots can be registered before the
/// frame the panel is actually drawn.
fn layout_items_y0(inner: Rect) -> Option<u16> {
    let bottom = inner.bottom();
    let step = |y: u16, h: u16| if y + h <= bottom { y + h } else { y };
    let (with_stats, with_items) = ShardRealm::panel_budget(inner.height());
    let quest_h = ShardRealm::quest_box_h(inner.height(), with_stats, with_items);

    let mut y = inner.top();
    y = step(y, PORTRAIT.len() as u16);
    y = step(y, 3);
    y = step(y, 2);
    y = step(y, 1);
    y = step(y, 1);
    y = step(y, quest_h);
    if with_stats {
        y = step(y, 1);
        y = step(y, 4);
    }
    if !with_items {
        return None;
    }
    y = step(y, 1);
    Some(y)
}

/// Draws a plain single-line box, for the stat and item grids.
fn draw_box(surface: &mut Surface<'_>, rect: Rect, fg: Color, bg: Color) {
    surface.fill_rect(rect, ' ', Style::new().bg(bg));
    if rect.width() < 2 || rect.height() < 2 {
        return;
    }
    for x in rect.left()..rect.right() {
        surface.put((x, rect.top()), '\u{2500}', Style::new().fg(fg).bg(bg));
        surface.put(
            (x, rect.bottom() - 1),
            '\u{2500}',
            Style::new().fg(fg).bg(bg),
        );
    }
    for y in rect.top()..rect.bottom() {
        surface.put((rect.left(), y), '\u{2502}', Style::new().fg(fg).bg(bg));
        surface.put(
            (rect.right() - 1, y),
            '\u{2502}',
            Style::new().fg(fg).bg(bg),
        );
    }
    surface.put(
        (rect.left(), rect.top()),
        '\u{250C}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.right() - 1, rect.top()),
        '\u{2510}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.left(), rect.bottom() - 1),
        '\u{2514}',
        Style::new().fg(fg).bg(bg),
    );
    surface.put(
        (rect.right() - 1, rect.bottom() - 1),
        '\u{2518}',
        Style::new().fg(fg).bg(bg),
    );
}

/// Prints `text` word-wrapped into `rect`'s interior, starting `top_pad` rows
/// down, clipped to however many rows actually fit. Used for the quest box,
/// whose text length varies (the default quest line versus a tooltip).
fn wrap_into(
    surface: &mut Surface<'_>,
    rect: Rect,
    top_pad: u16,
    text: &str,
    color: Color,
    bg: Color,
) {
    let width = usize::from(rect.width().saturating_sub(2));
    if width == 0 {
        return;
    }
    let mut line = String::new();
    let mut row = 0u16;
    let max_rows = rect.height().saturating_sub(top_pad);
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if candidate.chars().count() > width {
            if row >= max_rows {
                return;
            }
            surface.print(
                (rect.left() + 1, rect.top() + top_pad + row),
                &line,
                Style::new().fg(color).bg(bg),
            );
            row += 1;
            word.clone_into(&mut line);
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() && row < max_rows {
        surface.print(
            (rect.left() + 1, rect.top() + top_pad + row),
            &line,
            Style::new().fg(color).bg(bg),
        );
    }
}

/// The terrain glyph and color at one map cell: a hash-driven texture variant
/// per terrain, plus a slow canopy sway for forest cells and a glinting pond
/// overlay, both purely decorative and both driven by `time` so the map
/// visibly animates on its own.
fn terrain_glyph(province: &Province, wx: i32, wy: i32, time: f32) -> (char, Color) {
    let pond = hash01(0xF0A7, wx, wy) > 0.975 && province.terrain != Terrain::Hills;
    if pond {
        let glint = 0.5f32.mul_add(
            (time.mul_add(2.2, hash01(0x1122, wx, wy) * core::f32::consts::TAU)).sin(),
            0.5,
        );
        return ('\u{2248}', mix(rgb(30, 60, 96), rgb(120, 180, 220), glint));
    }

    let v = hash01(0x51C3, wx, wy);
    let base = province.terrain.base();
    let (glyph, variant) = match province.terrain {
        Terrain::Forest => {
            let sway = 0.5f32.mul_add((time.mul_add(0.35, v * core::f32::consts::TAU)).sin(), 0.5);
            let g = if v > 0.55 { '\u{2663}' } else { '.' };
            (g, mix(base, rgb(70, 118, 62), sway * 0.4))
        }
        Terrain::Hills => {
            let g = if v > 0.7 { '\u{2229}' } else { '\u{00B7}' };
            (g, base)
        }
        Terrain::Plains => {
            let g = if v > 0.8 { '\u{00B7}' } else { ' ' };
            (g, base)
        }
    };
    (glyph, province.owner.tint(variant))
}

/// A slow, hash-seeded flicker in `0.0..=1.0`, for torch-lit ornamentation.
/// `salt` gives every ornament its own phase so a whole panel of flickering
/// glyphs doesn't pulse in lockstep like a single blinking light.
fn torch_flicker(time: f32, salt: u32) -> f32 {
    let phase = hash01(0x7A11, salt as i32, (salt >> 3) as i32) * core::f32::consts::TAU;
    (0.5 * (time.mul_add(3.1, phase)).sin()).mul_add(0.6, 0.5)
}

/// Draws the outer ornamental frame: a double-line border with corner
/// flourishes and a slow shimmer travelling around its own perimeter, and
/// returns the interior rect. Bespoke rather than [`panel::Panel`] because
/// the brief specifically asks for corner flourishes and a gilt shimmer that
/// a generic bordered panel doesn't have a vocabulary for.
fn draw_ornate_frame(surface: &mut Surface<'_>, area: Rect, time: f32, title: &str) -> Rect {
    if area.width() < 4 || area.height() < 4 {
        surface.fill_rect(area, ' ', Style::new().bg(panel::PANEL_BG));
        return Rect::new(area.left(), area.top(), 0, 0);
    }
    let bg = panel::PANEL_BG;
    surface.fill_rect(area, ' ', Style::new().bg(bg));

    let (l, t) = (area.left(), area.top());
    let (r, b) = (area.right() - 1, area.bottom() - 1);
    let perimeter = 2
        * (u32::from(area.width()) + u32::from(area.height()))
            .saturating_sub(4)
            .max(1);
    let phase = (time * 0.06).fract();

    let shimmer_fg = |index: u32| -> Color {
        let pos = index as f32 / perimeter as f32;
        let dist = (pos - phase)
            .abs()
            .min((pos - phase + 1.0).abs())
            .min((pos - phase - 1.0).abs());
        let boost = (1.0 - (dist * 6.0).min(1.0)).max(0.0);
        mix(rgb(150, 122, 60), rgb(246, 220, 140), boost)
    };

    let mut idx = 0u32;
    for x in l..=r {
        let ch = if x == l {
            '\u{2554}'
        } else if x == r {
            '\u{2557}'
        } else {
            '\u{2550}'
        };
        surface.put((x, t), ch, Style::new().fg(shimmer_fg(idx)).bg(bg));
        idx += 1;
    }
    for y in (t + 1)..b {
        surface.put((r, y), '\u{2551}', Style::new().fg(shimmer_fg(idx)).bg(bg));
        idx += 1;
    }
    for x in (l..=r).rev() {
        let ch = if x == l {
            '\u{255A}'
        } else if x == r {
            '\u{255D}'
        } else {
            '\u{2550}'
        };
        surface.put((x, b), ch, Style::new().fg(shimmer_fg(idx)).bg(bg));
        idx += 1;
    }
    for y in ((t + 1)..b).rev() {
        surface.put((l, y), '\u{2551}', Style::new().fg(shimmer_fg(idx)).bg(bg));
        idx += 1;
    }

    // Corner flourishes: a gem glyph one cell inset from each corner,
    // flickering on its own torch phase independent of the shimmer sweep.
    for (cx, cy, salt) in [
        (l + 1, t, 1u32),
        (r - 1, t, 2),
        (l + 1, b, 3),
        (r - 1, b, 4),
    ] {
        let flicker = torch_flicker(time, salt);
        surface.put(
            (cx, cy),
            '\u{2666}',
            Style::new()
                .fg(mix(rgb(150, 122, 60), rgb(246, 220, 140), flicker))
                .bg(bg),
        );
    }

    if !title.is_empty() && area.width() > title.chars().count() as u16 + 4 {
        let text = format!(" {title} ");
        surface.print(
            (l + 3, t),
            &text,
            Style::new().fg(rgb(236, 214, 150)).bg(bg),
        );
    }

    Rect::new(l + 1, t + 1, area.width() - 2, area.height() - 2)
}

impl Demo for ShardRealm {
    const NAME: &'static str = "40_shard_realm";
    const TITLE: &'static str = "40 Shard Realm";
    const BLURB: &'static str =
        "Eador's province map, built around one ornate hero panel: stats, quest, gear.";
    const GRID: (u16, u16) = (160, 50);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap province", "move / attack"),
            ("tap site", "preview cost / explore"),
            ("tap item", "inspect"),
            ("R", "return to capital"),
            ("Enter/Space", "end turn"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let shape = Shape::of(content);

        // A full-width game bar (gold/gems, province name, icon buttons) sits
        // above the gallery's own status line; height degrades on very short
        // viewports rather than disappearing, since it carries live state
        // (province name, action buttons) the universal bar doesn't.
        let band_h = if content.height() >= 14 {
            4
        } else {
            2.min(content.height())
        };
        let (body, band) = panel::split_bottom(content, band_h);

        let panel_w = if shape.stacks() {
            body.width()
        } else {
            body.width().saturating_sub(30).clamp(28, 40)
        };

        let (map_area, panel_area) = if shape.stacks() {
            // Portrait: the hero panel goes full-width beneath a shortened
            // map, per the brief -- it never collapses into a plain list.
            let map_h = (body.height() * 2 / 5).max(10);
            panel::split_top(body, map_h)
        } else {
            panel::split_right(body, panel_w)
        };

        // Grow the Voronoi world before anything reads its extent this
        // frame (hotspots, then drawing): the map panel has a 1-cell border
        // on every side, so its interior -- not the panel rect itself -- is
        // what the world needs to cover. See `ensure_world`.
        let map_interior = (
            i32::from(map_area.width().saturating_sub(2)),
            i32::from(map_area.height().saturating_sub(2)),
        );
        self.ensure_world(map_interior);

        // Every hotspot is rebuilt fresh from this frame's own layout,
        // before any input is read, and none are registered from inside a
        // draw call: the draw calls below borrow `term`'s surface, which
        // would conflict with `handle_events`'s own borrow of `term` if the
        // two were interleaved. See `ui::touch::Hotspots`.
        let panel_inner = frame_interior(panel_area);
        self.hotspots.clear();
        self.register_map_hotspots(map_area);
        self.register_site_hotspots(map_area);
        if let Some(y0) = layout_items_y0(panel_inner) {
            self.register_item_hotspots(panel_inner, y0, panel_inner);
        }
        self.register_band_hotspots(band);

        if !self.handle_events(term) {
            return false;
        }

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_map(&mut surface, map_area);
        self.draw_hero_panel(&mut surface, panel_area);
        self.draw_game_bar(&mut surface, band);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(ShardRealm);

#[cfg(test)]
mod shape_smoke_tests {
    use super::ShardRealm;
    use ascii_tile_demos::Demo;
    use retroglyph_core::{Frame, Headless, Terminal};
    use std::time::Duration;

    fn render_at(cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(Headless::new(cols, rows));
        let mut demo = ShardRealm::init(&mut term);
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
        assert!(view.chars().any(|c| !c.is_whitespace()), "blank:\n{view}");
    }

    #[test]
    fn landscape_renders_something() {
        let view = render_at(158, 36);
        assert!(view.chars().any(|c| !c.is_whitespace()), "blank:\n{view}");
    }

    #[test]
    fn desktop_renders_something() {
        let view = render_at(160, 50);
        assert!(view.chars().any(|c| !c.is_whitespace()), "blank:\n{view}");
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let view = render_at(80, 24);
        assert!(view.chars().any(|c| !c.is_whitespace()), "blank:\n{view}");
    }

    #[test]
    fn two_identical_runs_render_identically() {
        assert_eq!(render_at(80, 24), render_at(80, 24));
    }
}
