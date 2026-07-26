//! 41: Riven route -- a weighted node web you plan a multi-hop journey
//! across, adapted from Vagrus: The Riven Realms' campaign map.
//!
//! Every strategy map elsewhere in this gallery either has no travel cost
//! (30's sector map charges fuel per jump, not per edge) or no route to plan
//! at all. This one is built entirely around the one thing Vagrus's map does
//! that neither does: *every edge carries its own movement-point cost,
//! printed on the line itself*, and the decision the player is making is not
//! "which node next" but "which whole route, and can the larder survive it".
//! Tapping a node previews the cheapest path there before anything is spent;
//! only a separate confirm actually moves the caravan. That gap between
//! preview and commitment is the whole demo.
//!
//! Techniques on show:
//!
//! - **Dijkstra over a hand-authored graph, not a grid**
//!   ([`shortest_path`]): the map is fourteen nodes and twenty-six edges, not
//!   a tile field, so `tilekit::path::find` (which walks a rectangular grid)
//!   does not apply. A small dense-array Dijkstra is cheaper to write
//!   correctly here than adapting a grid search to a sparse graph would be.
//! - **Cost-preview-before-commit** ([`RivenRoute::extend_plan`],
//!   [`RivenRoute::preview`]): tapping a node appends the cheapest path from
//!   wherever the plan currently ends, and the running movement-point total,
//!   day count, and supply cost are recomputed and shown immediately. Nothing
//!   is spent, and no node moves, until the crew taps Depart -- the same
//!   separation Into the Breach uses for undo, applied to a resource spend
//!   instead of a position.
//! - **Edges drawn as real lines with the cost sitting on them**
//!   ([`RivenRoute::draw_edge`]): a free-angle Bresenham walk between two
//!   ring centres, glyph chosen per step from its local direction (`-`, `|`,
//!   `/`, `\`), with the movement cost punched into the cell nearest the
//!   midpoint. 30's lanes are Manhattan-routed box-drawing wires between
//!   square node panels; this map's nodes are open rings and its edges are
//!   free lines, which is what makes the cost numbers read as sitting on a
//!   desert track rather than on a circuit diagram.
//! - **Morale as named tiers, not a bar** ([`tier_counts`]): Fervent,
//!   Subservient, Invigorated, and Sustained are discrete headcounts that
//!   redistribute as supplies run low, computed once per in-game day rather
//!   than smoothly animated -- the crew panel is a report, not a gauge.
//! - **A calendar rosette as a real radial ornament** ([`draw_calendar`]):
//!   an ellipse traced by sampling angle rather than four corners and a
//!   label, with a sweep marker that circles it continuously (ambient
//!   animation) while the year/month/day/weekday text underneath stays
//!   pinned to the cell grid and only changes on an actual day rollover.
//! - **[`ui::touch::Shape`]-driven reflow**: desktop and landscape show the
//!   crew report and calendar side by side above the web; portrait collapses
//!   both to a two-row summary and the log to one line. The web's own scale
//!   is never allowed to shrink an edge cost below readable -- a panel
//!   smaller than the world pans a native 1:1 map ([`RivenRoute::camera_scroll`]);
//!   a panel *larger* than the world spreads the whole graph and its terrain
//!   zones to fill it ([`RivenRoute::draw_map`]) rather than leaving the
//!   surplus space blank, which is what a fixed-extent hand-authored graph
//!   does by default on a wide desktop window.
//!
//! ```sh
//! cargo run --example 41_riven_route --features crossterm
//! cargo run --example 41_riven_route --features software
//! cargo run --example 41_riven_route --features gl
//! cargo run --example 41_riven_route  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Log, Panel, Span};
use ascii_tile_demos::ui::touch::{self, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{fbm, hash01};
use tilekit::palette::{mix, rgb, scale};

/// World size in cells the graph is laid out in. Fixed and hand-authored
/// (see [`RivenRoute::default`]) rather than generated, because the point on
/// show is the weighted-edge web itself, not a map generator; a fixed layout
/// also keeps the rendered output identical run to run with no seed to pin.
const WORLD_W: i32 = 94;
/// See [`WORLD_W`].
const WORLD_H: i32 = 40;

/// Radius, in cells, of a node's drawn ring. A single glyph cannot carry
/// "named location", "visited", "reachable", and "currently selected" all at
/// once; a ring this size has room for a hollow centre (the caravan token
/// when occupied) plus a labelled name underneath, and it is legible at the
/// map's native 1:1 scale on every [`Shape`].
const NODE_RX: i32 = 2;
/// See [`NODE_RX`]. Half the horizontal radius because cells are twice as
/// tall as wide, so a symmetric *visual* ring needs an asymmetric cell shape.
const NODE_RY: i32 = 1;

/// Movement points a caravan can spend before a day turns over.
///
/// Vagrus prices a "normal" day's travel at a handful of points; six is
/// enough that most single edges (costs of 2-6 in [`RivenRoute::default`])
/// take a fraction of a day but a long chained route visibly costs several,
/// which is what makes the day/supply preview worth showing at all.
const MP_PER_DAY: f32 = 6.0;
/// Movement points the caravan crosses per idle-simulated second while
/// travelling. Slow enough that a multi-edge route is watchable, fast enough
/// that the demo does not sit idle for tens of seconds between arrivals.
const MP_PER_SECOND: f32 = 1.6;
/// Supply units consumed per in-game day. Chosen so `supplies` (started at
/// [`RivenRoute::default`]'s value) reads directly as "days of food left"
/// without a second conversion the crew panel would have to spell out.
const SUPPLY_PER_DAY: f32 = 1.0;
/// Water units consumed per in-game day. Slightly faster than supplies so
/// water is usually the tighter constraint, matching the caravan-survival
/// games this demo is drawn from.
const WATER_PER_DAY: f32 = 1.25;
/// Money spent per in-game day in wages, deducted alongside supplies so the
/// crew panel's money figure is not a static prop.
const WAGE_PER_DAY: i32 = 4;

/// Days per month on this calendar. An even number keeps month/weekday
/// rollover arithmetic simple; the exact count is flavour, not mechanics.
const DAYS_PER_MONTH: u32 = 30;

/// Month names for the rosette. Thematic rather than the real calendar,
/// matching a caravan crossing a continent that is not Earth.
const MONTH_NAMES: [&str; 6] = [
    "Emberwane",
    "Duskmere",
    "Frostfall",
    "Sunscour",
    "Ashtide",
    "Windrest",
];

/// How many angle samples trace the calendar rosette's ring. Enough that the
/// ellipse reads as a smooth curve rather than a dashed polygon at the panel
/// sizes this demo actually draws at.
const CALENDAR_RING_STEPS: u32 = 40;

/// Weekday names, indexed by an absolute day count modulo 7.
const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday", "Ashday", "Boneday", "Windday", "Saltday", "Duskday", "Restday",
];

/// Where terrain shading changes across the world, in world cells. A demo
/// about route *cost* still needs the ground to look like the desert the
/// screenshot shows, so the background is tinted by fixed zones rather than
/// left flat: a salt flat in the south-centre, an ice field in the
/// north-east, desert everywhere else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Terrain {
    Desert,
    SaltFlat,
    IceField,
}

impl Terrain {
    /// Classifies a world cell by fixed zone rectangles.
    fn at(x: i32, y: i32) -> Self {
        if x >= 58 && y < 22 {
            Self::IceField
        } else if (34..60).contains(&x) && y >= 18 {
            Self::SaltFlat
        } else {
            Self::Desert
        }
    }

    /// Base tint before dune/crack shading is mixed in.
    const fn base(self) -> Color {
        match self {
            Self::Desert => rgb(96, 74, 42),
            Self::SaltFlat => rgb(70, 74, 78),
            Self::IceField => rgb(52, 70, 86),
        }
    }

    /// The speckle glyph used for this terrain's surface texture.
    const fn speckle(self) -> char {
        match self {
            Self::Desert => '.',
            Self::SaltFlat => '\'',
            Self::IceField => '*',
        }
    }
}

/// One node in the route graph: a position, and a name for named
/// settlements. Junction nodes (`name: None`) exist purely to make the web
/// read as dense as the reference screenshot's, the way real road maps mark
/// an unnamed crossroads without promoting it to a place.
struct MapNode {
    name: Option<&'static str>,
    x: i32,
    y: i32,
}

impl MapNode {
    const fn pos(&self) -> (i32, i32) {
        (self.x, self.y)
    }
}

/// An undirected edge with its movement-point cost.
struct Edge {
    a: usize,
    b: usize,
    cost: u32,
}

/// The fixed route graph.
struct Graph {
    nodes: Vec<MapNode>,
    edges: Vec<Edge>,
}

impl Graph {
    /// The desert web: eight named settlements (one of them "Crystal", so the
    /// discovery log's example line has somewhere to fire) and six unnamed
    /// junctions, connected by twenty-six weighted edges. Hand-authored, like
    /// 30's sector table, because the layout itself is the content on show,
    /// not something a generator should be trusted to reproduce identically.
    fn new() -> Self {
        const RAW_NODES: [(Option<&str>, i32, i32); 14] = [
            (Some("Kelmouth"), 8, 20),
            (None, 20, 10),
            (None, 20, 30),
            (Some("Sandholt"), 34, 6),
            (Some("Raspwell"), 34, 22),
            (None, 32, 34),
            (Some("Verdant"), 50, 4),
            (None, 48, 16),
            (Some("Greysalt"), 52, 27),
            (None, 46, 35),
            (Some("Ashenmoor"), 66, 9),
            (Some("Cinder"), 64, 21),
            (None, 62, 33),
            (Some("Crystal"), 80, 14),
        ];
        const RAW_EDGES: [(usize, usize, u32); 26] = [
            (0, 1, 3),
            (0, 2, 3),
            (1, 3, 4),
            (1, 4, 3),
            (1, 7, 5),
            (2, 4, 3),
            (2, 5, 3),
            (3, 6, 3),
            (3, 7, 4),
            (4, 7, 3),
            (4, 5, 3),
            (4, 8, 4),
            (5, 9, 3),
            (5, 12, 5),
            (6, 7, 3),
            (6, 10, 4),
            (7, 8, 4),
            (7, 11, 5),
            (8, 9, 3),
            (8, 11, 3),
            (9, 12, 4),
            (10, 11, 3),
            (10, 13, 5),
            (11, 12, 4),
            (11, 13, 4),
            (12, 13, 5),
        ];
        Self {
            nodes: RAW_NODES
                .into_iter()
                .map(|(name, x, y)| MapNode { name, x, y })
                .collect(),
            edges: RAW_EDGES
                .into_iter()
                .map(|(a, b, cost)| Edge { a, b, cost })
                .collect(),
        }
    }

    /// Every `(neighbour, cost)` pair reachable directly from `node`.
    fn neighbors(&self, node: usize) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.edges.iter().filter_map(move |e| {
            if e.a == node {
                Some((e.b, e.cost))
            } else if e.b == node {
                Some((e.a, e.cost))
            } else {
                None
            }
        })
    }

    /// The cost of the direct edge between `a` and `b`, if one exists.
    fn edge_cost(&self, a: usize, b: usize) -> Option<u32> {
        self.neighbors(a).find(|&(n, _)| n == b).map(|(_, c)| c)
    }
}

/// Finds the cheapest path from `from` to `to`, inclusive of both ends.
///
/// A plain O(V^2) Dijkstra rather than a binary heap: the graph has fourteen
/// nodes, so the heap's better asymptotics buy nothing and the array version
/// has no priority-queue plumbing to get wrong. Returns `None` only if the
/// graph is disconnected between the two nodes, which it never is here, but
/// the caller still has to handle it rather than index out of bounds.
fn shortest_path(graph: &Graph, from: usize, to: usize) -> Option<(Vec<usize>, u32)> {
    let n = graph.nodes.len();
    if from == to {
        return Some((vec![from], 0));
    }
    let mut dist = vec![u32::MAX; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    dist[from] = 0;

    for _ in 0..n {
        let mut u = None;
        let mut best = u32::MAX;
        for (i, &d) in dist.iter().enumerate() {
            if !done[i] && d < best {
                best = d;
                u = Some(i);
            }
        }
        let Some(u) = u else { break };
        if u == to {
            break;
        }
        done[u] = true;
        for (v, cost) in graph.neighbors(u) {
            if done[v] {
                continue;
            }
            let candidate = dist[u].saturating_add(cost);
            if candidate < dist[v] {
                dist[v] = candidate;
                prev[v] = u;
            }
        }
    }

    if dist[to] == u32::MAX {
        return None;
    }
    let mut path = vec![to];
    let mut cur = to;
    while cur != from {
        cur = prev[cur];
        path.push(cur);
    }
    path.reverse();
    Some((path, dist[to]))
}

/// The in-game calendar. Advances only on an actual day rollover
/// ([`RivenRoute::simulate`]), never smoothly, so the rosette's printed
/// numbers stay pinned to the cell grid even while its ornament animates.
#[derive(Clone, Copy)]
struct Calendar {
    year: u32,
    month: u32,
    day: u32,
    absolute_day: u32,
}

impl Calendar {
    const fn new() -> Self {
        Self {
            year: 1247,
            month: 0,
            day: 1,
            absolute_day: 0,
        }
    }

    const fn advance(&mut self) {
        self.absolute_day += 1;
        self.day += 1;
        if self.day > DAYS_PER_MONTH {
            self.day = 1;
            self.month += 1;
            if self.month as usize >= MONTH_NAMES.len() {
                self.month = 0;
                self.year += 1;
            }
        }
    }

    const fn weekday(self) -> &'static str {
        WEEKDAY_NAMES[(self.absolute_day as usize) % WEEKDAY_NAMES.len()]
    }

    const fn month_name(self) -> &'static str {
        MONTH_NAMES[self.month as usize]
    }
}

/// Counts across the four named morale tiers, always summing to `total`.
///
/// A named tier is a report ("six hands are Fervent"), not a gauge, so this
/// is computed fresh from the supply ratio once per day rather than eased
/// frame to frame -- the crew panel's numbers change in the same discrete
/// steps the calendar's do, for the same reason (see the module doc's fourth
/// bullet).
fn tier_counts(total: u32, ratio: f32) -> [u32; 4] {
    let r = ratio.clamp(0.0, 1.0);
    // Four raw weights that shift smoothly from "mostly Fervent" at full
    // supply to "mostly Sustained" (barely holding together) near empty,
    // with Subservient and Invigorated forming the middle ground either
    // side of half rations.
    let weights = [
        r * r + 0.05,
        r * (1.0 - r).mul_add(2.0, 0.2),
        (1.0 - r) * r.mul_add(2.0, 0.2),
        (1.0 - r).mul_add(1.0 - r, 0.05),
    ];
    let sum: f32 = weights.iter().sum();
    let raw: Vec<f32> = weights.iter().map(|w| w / sum * total as f32).collect();

    // Largest-remainder rounding: floor every share, then hand the leftover
    // units to whichever tiers had the biggest fractional part. Straight
    // rounding can over- or under-shoot `total` by a couple of heads; this
    // is the standard apportionment fix and it is exact.
    let mut counts = [0u32; 4];
    let mut used = 0u32;
    for (i, &v) in raw.iter().enumerate() {
        counts[i] = v.floor() as u32;
        used += counts[i];
    }
    let mut remainder = total.saturating_sub(used);
    let mut order = [0usize, 1, 2, 3];
    order.sort_by(|&a, &b| {
        let fa = raw[a] - raw[a].floor();
        let fb = raw[b] - raw[b].floor();
        fb.partial_cmp(&fa).unwrap_or(core::cmp::Ordering::Equal)
    });
    for &i in &order {
        if remainder == 0 {
            break;
        }
        counts[i] += 1;
        remainder -= 1;
    }
    counts
}

/// What tapping a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Node(usize),
    Depart,
    Clear,
}

/// State: the graph, the caravan's place in it, the planned route, resources,
/// the calendar, and everything needed to draw and animate the map.
pub struct RivenRoute {
    graph: Graph,
    current: usize,
    /// The planned route, `plan[0]` always equal to `current`. A single
    /// element means no route is queued.
    plan: Vec<usize>,
    visited: Vec<bool>,
    departed: bool,
    /// Movement points spent on the edge currently being crossed.
    edge_progress: f32,

    supplies: f32,
    supplies_max: f32,
    water: f32,
    water_max: f32,
    money: i32,
    crew_total: u32,
    calendar: Calendar,
    /// Movement points accumulated toward the next day rollover.
    day_progress_mp: f32,

    log: Log,
    time: f32,
    scroll: (i32, i32),
    /// How many screen cells one world cell currently occupies, on each
    /// axis independently.
    ///
    /// The graph is authored once at [`WORLD_W`]x[`WORLD_H`], but the map
    /// panel's inner rect can be smaller (a phone) or much larger (a wide
    /// desktop window) than that, and can be tight on one axis while loose
    /// on the other (a short-but-wide landscape window is the common case).
    /// Neither component ever drops below 1.0 -- the web only ever *shrinks*
    /// the world's occupied share of the panel, it does not zoom out past
    /// native scale, because a cost digit or a carved-caps label squeezed
    /// below one cell per glyph stops being legible. The two axes are
    /// allowed to differ (unlike a photograph, this graph has no real angles
    /// to preserve): node rings are drawn at a fixed absolute screen radius
    /// regardless of zoom, and edge-glyph selection only reads the sign of
    /// each Bresenham step, not its magnitude, so stretching x and y by
    /// different amounts does not bend a diagonal or turn a ring into an
    /// ellipse the way it would if those things were computed from the
    /// zoomed *distance* instead. Recomputed once per frame in
    /// [`RivenRoute::draw_map`] from the live panel size, so resizing the
    /// window (or rotating a phone) reflows immediately.
    zoom: (f32, f32),
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for RivenRoute {
    fn default() -> Self {
        let graph = Graph::new();
        let mut visited = vec![false; graph.nodes.len()];
        visited[0] = true;

        let mut log = Log::new(48);
        log.push("Caravan makes camp at Kelmouth.", ui::FG);
        log.push(
            "Tap a node to preview a route, tap Depart to commit.",
            ui::DIM,
        );

        Self {
            graph,
            current: 0,
            plan: vec![0],
            visited,
            departed: false,
            edge_progress: 0.0,
            supplies: 30.0,
            supplies_max: 30.0,
            water: 34.0,
            water_max: 34.0,
            money: 140,
            crew_total: 24,
            calendar: Calendar::new(),
            day_progress_mp: 0.0,
            log,
            time: 0.0,
            scroll: (0, 0),
            zoom: (1.0, 1.0),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl RivenRoute {
    /// The cumulative `(movement points, node count)` of the pending plan
    /// beyond the current position.
    fn plan_totals(&self) -> (u32, usize) {
        let mut mp = 0;
        for w in self.plan.windows(2) {
            mp += self.graph.edge_cost(w[0], w[1]).unwrap_or(0);
        }
        (mp, self.plan.len().saturating_sub(1))
    }

    /// Days and supply cost the pending plan would take, rounding partial
    /// days up: a route that spends into a new day owes that whole day's
    /// upkeep, the same way Vagrus counts a partial travel day as a full one.
    fn preview(&self) -> (u32, f32, f32) {
        let (mp, _) = self.plan_totals();
        let days = (f32::from(mp as u16) / MP_PER_DAY).ceil();
        (mp, days, days * SUPPLY_PER_DAY)
    }

    /// Extends the plan with the cheapest path from its current end to
    /// `target`. A no-op if there is no path (never happens on this graph)
    /// or if `target` is already where the plan ends.
    fn extend_plan(&mut self, target: usize) {
        let from = *self.plan.last().unwrap_or(&self.current);
        if from == target {
            return;
        }
        if let Some((path, _)) = shortest_path(&self.graph, from, target) {
            self.plan.extend_from_slice(&path[1..]);
        }
    }

    fn clear_plan(&mut self) {
        self.plan = vec![self.current];
        self.departed = false;
        self.edge_progress = 0.0;
    }

    /// Advances travel, supplies, and the calendar by `dt` simulated seconds.
    fn simulate(&mut self, dt: f32) {
        if self.departed && self.plan.len() > 1 {
            let step_mp = MP_PER_SECOND * dt;
            self.edge_progress += step_mp;
            self.day_progress_mp += step_mp;

            let cost = self
                .graph
                .edge_cost(self.plan[0], self.plan[1])
                .unwrap_or(1) as f32;
            if self.edge_progress >= cost {
                self.edge_progress -= cost;
                self.current = self.plan[1];
                self.plan.remove(0);
                self.arrive();
                if self.plan.len() <= 1 {
                    self.departed = false;
                    self.edge_progress = 0.0;
                }
            }
        }

        // Each iteration subtracts a fixed positive constant, so this always
        // terminates; clippy's float-comparison lint does not know that.
        #[allow(clippy::while_float)]
        while self.day_progress_mp >= MP_PER_DAY {
            self.day_progress_mp -= MP_PER_DAY;
            self.calendar.advance();
            self.supplies = (self.supplies - SUPPLY_PER_DAY).max(0.0);
            self.water = (self.water - WATER_PER_DAY).max(0.0);
            self.money -= WAGE_PER_DAY;
        }
    }

    /// Records arrival at the caravan's new current node: logs a discovery
    /// the first time a named location is reached, exactly the "codex entry"
    /// reward Vagrus gives for stepping somewhere new.
    fn arrive(&mut self) {
        let idx = self.current;
        if self.visited[idx] {
            return;
        }
        self.visited[idx] = true;
        if let Some(name) = self.graph.nodes[idx].name {
            self.log.push(
                format!("1 Insight awarded for discovering {name}."),
                ui::ACCENT,
            );
        }
    }

    /// The `(x, y)` zoom a map panel of `inner` size should use: on each
    /// axis independently, the factor (never below 1.0) that makes the
    /// scaled world exactly fill that axis.
    ///
    /// A panel smaller than the world on an axis keeps that axis at 1.0
    /// (native scale, pan instead of shrink -- an edge cost or a carved-caps
    /// label squeezed below one screen cell per glyph stops being legible).
    /// A panel *larger* than the world on an axis stretches that axis to
    /// fill it, independently of what the other axis needs: a short, wide
    /// landscape window (tight on height, slack on width) is the case that
    /// motivated this, and a uniform zoom tied to whichever axis is tighter
    /// would leave that slack width blank forever. See [`Self::zoom`]'s own
    /// doc for why the two axes are allowed to differ here without
    /// distorting anything that matters.
    fn zoom_for(viewport: (u16, u16)) -> (f32, f32) {
        let zx = (f32::from(viewport.0) / WORLD_W as f32).max(1.0);
        let zy = (f32::from(viewport.1) / WORLD_H as f32).max(1.0);
        (zx, zy)
    }

    /// The camera's top-left offset in *scaled* world cells: follows the
    /// caravan's current position (interpolated mid-edge while travelling),
    /// clamped so the viewport never shows past the scaled map's edge. At
    /// zoom `(1.0, 1.0)` this is a plain 1:1 pan; on an axis that is fully
    /// spread (see [`Self::zoom_for`]) the clamp bounds on that axis
    /// collapse to zero and scroll on it always resolves to `0`, so a fully
    /// spread axis never jitters.
    fn camera_scroll(&self, viewport: (u16, u16)) -> (i32, i32) {
        let (cx, cy) = self.caravan_world_pos();
        let (zoom_x, zoom_y) = self.zoom;
        let cam_x = (cx as f32 * zoom_x).round() as i32;
        let cam_y = (cy as f32 * zoom_y).round() as i32;
        let half_w = i32::from(viewport.0) / 2;
        let half_h = i32::from(viewport.1) / 2;
        let scaled_w = (WORLD_W as f32 * zoom_x).round() as i32;
        let scaled_h = (WORLD_H as f32 * zoom_y).round() as i32;
        let max_x = (scaled_w - i32::from(viewport.0)).max(0);
        let max_y = (scaled_h - i32::from(viewport.1)).max(0);
        (
            (cam_x - half_w).clamp(0, max_x),
            (cam_y - half_h).clamp(0, max_y),
        )
    }

    /// The caravan's current world position, interpolated along the edge
    /// being crossed. Art, not a readout, so smooth interpolation here is the
    /// right call even though the module doc warns against it for text.
    fn caravan_world_pos(&self) -> (i32, i32) {
        if self.departed && self.plan.len() > 1 {
            let cost = self
                .graph
                .edge_cost(self.plan[0], self.plan[1])
                .unwrap_or(1) as f32;
            let t = (self.edge_progress / cost).clamp(0.0, 1.0);
            let (x0, y0) = self.graph.nodes[self.plan[0]].pos();
            let (x1, y1) = self.graph.nodes[self.plan[1]].pos();
            (
                f32::from(x0 as i16).mul_add(1.0 - t, f32::from(x1 as i16) * t) as i32,
                f32::from(y0 as i16).mul_add(1.0 - t, f32::from(y1 as i16) * t) as i32,
            )
        } else {
            self.graph.nodes[self.current].pos()
        }
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
                match key.code {
                    KeyCode::Enter => self.confirm_route(),
                    KeyCode::Char('c' | 'C') => self.clear_plan(),
                    _ => {}
                }
            }
        }
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            match action {
                Action::Node(idx) => self.extend_plan(idx),
                Action::Depart => self.confirm_route(),
                Action::Clear => self.clear_plan(),
            }
        }
        true
    }

    const fn confirm_route(&mut self) {
        if self.plan.len() > 1 {
            self.departed = true;
        }
    }

    fn status(&self) -> String {
        format!(
            "at {}  plan {} legs  {}",
            self.graph.nodes[self.current].name.unwrap_or("a waypoint"),
            self.plan.len().saturating_sub(1),
            if self.departed { "en route" } else { "at rest" }
        )
    }

    // ---- layout -------------------------------------------------------

    /// Splits the content area for the current [`Shape`]: `(crew, calendar,
    /// map, log, actions)`.
    fn layout(content: Rect, shape: Shape) -> (Rect, Rect, Rect, Rect, Rect) {
        if shape.stacks() {
            // Portrait: a two-row top summary (crew left half, calendar
            // right half), a one-line log, and a button row above the status
            // bar -- the map keeps every remaining row rather than giving any
            // of it back to chrome, per the brief's "keeps its size" rule.
            let (top, rest) = panel::split_top(content, 4.min(content.height()));
            let (crew, cal) = panel::split_left(top, top.width() / 2);
            let (rest2, actions) = panel::split_bottom(rest, (touch::TAP_H + 2).min(rest.height()));
            let (map, log) = panel::split_bottom(rest2, 3.min(rest2.height()));
            (crew, cal, map, log, actions)
        } else {
            let top_h = 11.min(content.height() / 2);
            let (top, rest) = panel::split_top(content, top_h);
            let cal_w = 30.min(top.width() / 3);
            let (crew, cal) = panel::split_right(top, cal_w);
            let bottom_h = 8.min(rest.height() / 3);
            let (map, bottom) = panel::split_bottom(rest, bottom_h);
            let action_w = 22.min(bottom.width() / 3);
            let (log, actions) = panel::split_right(bottom, action_w);
            (crew, cal, map, log, actions)
        }
    }

    // ---- drawing --------------------------------------------------------

    /// Converts an *unscaled* world cell (the graph's own authored
    /// coordinates) to a screen position, applying [`Self::zoom`] and the
    /// current scroll. Returns `None` for anything that lands outside
    /// `area`, same as before zoom existed -- callers do not need to know
    /// scaling happened at all.
    /// The signed `(x, y)` offset from `area`'s top-left corner that an
    /// unscaled world coordinate lands on, before any bounds check. Signed
    /// and unclamped so a caller anchoring several cells to one world point
    /// (see [`Self::draw_node`]) can step whole cells away from it and still
    /// have each step's own in-bounds test succeed independently, the same
    /// way it would if every cell had gone through [`Self::world_to_screen`]
    /// on its own world coordinate.
    fn world_offset(&self, wx: i32, wy: i32) -> (i32, i32) {
        let (zoom_x, zoom_y) = self.zoom;
        (
            (wx as f32 * zoom_x).round() as i32 - self.scroll.0,
            (wy as f32 * zoom_y).round() as i32 - self.scroll.1,
        )
    }

    fn world_to_screen(&self, area: Rect, wx: i32, wy: i32) -> Option<(u16, u16)> {
        let (sx, sy) = self.world_offset(wx, wy);
        if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
            return None;
        }
        Some((area.left() + sx as u16, area.top() + sy as u16))
    }

    /// Terrain is sampled in unscaled world space and then *drawn* at scaled
    /// screen positions -- the reverse of every other draw function here,
    /// which start from a world coordinate and scale it forward. Terrain has
    /// no discrete authored positions to scale; it is a continuous field
    /// (`Terrain::at`'s zone rectangles, `fbm`'s dune noise), so covering a
    /// bigger panel means sampling that same field more sparsely -- each
    /// screen cell maps back to a fractional world coordinate one zoom-th of
    /// a cell apart from its neighbour -- which is exactly what "spread the
    /// terrain zones to fill it" means: the desert/salt-flat/ice-field
    /// rectangles are unchanged in world space, so they now occupy a
    /// proportionally larger share of a larger panel.
    fn draw_terrain(&self, surface: &mut Surface<'_>, area: Rect) {
        let (zoom_x, zoom_y) = self.zoom;
        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let world_x = (self.scroll.0 + i32::from(sx)) as f32 / zoom_x;
                let world_y = (self.scroll.1 + i32::from(sy)) as f32 / zoom_y;
                let wx = world_x.floor() as i32;
                let wy = world_y.floor() as i32;
                if wx < 0 || wy < 0 || wx >= WORLD_W || wy >= WORLD_H {
                    continue;
                }
                let terrain = Terrain::at(wx, wy);
                let dune = fbm(0x5EED, world_x * 0.12, world_y * 0.12, 3, 0.5);
                let bg = scale(terrain.base(), 0.7f32.mul_add(dune, 0.7));
                let speckle = hash01(0x1234, wx, wy);
                let (glyph, fg) = if speckle < 0.06 {
                    (terrain.speckle(), mix(bg, rgb(220, 210, 190), 0.5))
                } else {
                    (' ', bg)
                };
                surface.put(
                    (area.left() + sx, area.top() + sy),
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
    }

    /// Dust motes that drift horizontally across the map over time, wrapping
    /// at the world edge. Position depends only on `self.time` (simulated,
    /// never wall-clock), so two runs fed identical deltas drift identically.
    fn draw_dust(&self, surface: &mut Surface<'_>, area: Rect) {
        const DUST_COUNT: i32 = 36;
        for i in 0..DUST_COUNT {
            let base_x = hash01(0xD057, i, 0) * WORLD_W as f32;
            let base_y = hash01(0xD057, i, 1) * WORLD_H as f32;
            let speed = 2.5f32.mul_add(hash01(0xD057, i, 2), 3.0);
            let wx = self.time.mul_add(speed, base_x).rem_euclid(WORLD_W as f32) as i32;
            let wy = base_y as i32;
            if let Some((sx, sy)) = self.world_to_screen(area, wx, wy) {
                let twinkle =
                    0.5f32.mul_add((self.time.mul_add(0.8, f32::from(i as i16))).sin(), 0.5);
                let v = 60.0f32.mul_add(twinkle, 120.0) as u8;
                surface.put(
                    (sx, sy),
                    '\u{00b7}',
                    Style::new().fg(rgb(v, v, v.saturating_sub(20))),
                );
            }
        }
    }

    /// Draws one edge as a free-angle line (not Manhattan-routed, unlike 30's
    /// lanes): a cell-stepped Bresenham walk between ring edges, glyph chosen
    /// per step from its local direction, with the cost punched in near the
    /// midpoint.
    /// Draws one edge by Bresenham-walking in *scaled* space -- unlike every
    /// other draw function here, which start from an unscaled world cell and
    /// scale forward, an edge is not a single point: at any zoom above 1.0,
    /// stepping through unscaled world cells and scaling each one
    /// individually would leave `zoom - 1` blank screen cells between every
    /// step, since adjacent world cells are no longer adjacent on screen.
    /// Walking the already-scaled endpoints keeps the line unbroken at any
    /// zoom, at the cost of this one function subtracting `self.scroll`
    /// itself instead of going through [`Self::world_to_screen`].
    fn draw_edge(&self, surface: &mut Surface<'_>, area: Rect, edge: &Edge, on_plan: bool) {
        let (ax, ay) = self.graph.nodes[edge.a].pos();
        let (bx, by) = self.graph.nodes[edge.b].pos();
        let (zoom_x, zoom_y) = self.zoom;
        let scaled = |x: i32, y: i32| -> (i32, i32) {
            (
                (x as f32 * zoom_x).round() as i32,
                (y as f32 * zoom_y).round() as i32,
            )
        };
        let (ax, ay) = scaled(ax, ay);
        let (bx, by) = scaled(bx, by);
        let color = if on_plan {
            rgb(226, 184, 96)
        } else {
            rgb(96, 84, 60)
        };
        let style = Style::new().fg(color);

        let cells = bresenham_cells(ax, ay, bx, by);
        let mid = cells.len() / 2;
        // Cost text width in cells, so the digits either side of the exact
        // midpoint are skipped and do not get overdrawn by the line glyph
        // loop below.
        let cost_text = edge.cost.to_string();
        let cost_w = cost_text.chars().count();
        let cost_start = mid.saturating_sub(cost_w / 2);

        let to_screen = |sx: i32, sy: i32| -> Option<(u16, u16)> {
            let sx = sx - self.scroll.0;
            let sy = sy - self.scroll.1;
            if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
                return None;
            }
            Some((area.left() + sx as u16, area.top() + sy as u16))
        };

        for (i, &(sx, sy)) in cells.iter().enumerate() {
            if i >= cost_start && i < cost_start + cost_w {
                continue;
            }
            let glyph = if i + 1 < cells.len() {
                let (nx, ny) = cells[i + 1];
                edge_glyph(nx - sx, ny - sy)
            } else if i > 0 {
                let (px, py) = cells[i - 1];
                edge_glyph(sx - px, sy - py)
            } else {
                '.'
            };
            if let Some(pos) = to_screen(sx, sy) {
                surface.put(pos, glyph, style);
            }
        }

        if let Some(&(mx, my)) = cells.get(mid) {
            let cost_color = if on_plan { ui::ACCENT } else { ui::DIM };
            for (j, ch) in cost_text.chars().enumerate() {
                let sx = mx - (cost_w as i32) / 2 + j as i32;
                if let Some(pos) = to_screen(sx, my) {
                    surface.put(pos, ch, Style::new().fg(cost_color).bg(rgb(10, 10, 14)));
                }
            }
        }
    }

    fn draw_node(&self, surface: &mut Surface<'_>, area: Rect, idx: usize) {
        let node = &self.graph.nodes[idx];
        let (cx, cy) = node.pos();
        let is_current = idx == self.current;
        let on_plan = self.plan.contains(&idx);
        let visited = self.visited[idx];

        let phase = hash01(0x9A0E, idx as i32, 0) * core::f32::consts::TAU;
        let pulse = 0.5f32.mul_add((self.time.mul_add(1.1, phase)).sin(), 0.5);
        let base = if is_current {
            ui::ACCENT
        } else if on_plan {
            rgb(226, 184, 96)
        } else if visited {
            rgb(150, 150, 170)
        } else {
            rgb(90, 90, 110)
        };
        let ring_color = mix(base, rgb(255, 255, 255), pulse * 0.18);
        let style = Style::new().fg(ring_color).bg(rgb(10, 10, 14));

        // Resolve the node's own centre once (as a signed, unclamped screen
        // offset -- see `Self::world_offset`) and then step in *screen*
        // cells for everything anchored to it (the ring and the label below
        // it), rather than re-deriving every offset cell's position by
        // scaling its world coordinate independently. Under a non-integer
        // zoom, `world_to_screen(cx + dx, ...)` for consecutive `dx` can
        // round to screen columns that are sometimes one cell apart and
        // sometimes two, opening an unblanked gap a crossing edge line shows
        // through; stepping from a single resolved anchor is exact by
        // construction, since it never re-multiplies by zoom per offset.
        // The anchor itself is allowed to land out of bounds (a node whose
        // centre sits one row above the panel but whose bottom ring row is
        // still visible is a normal, harmless crop, not a reason to hide the
        // whole node), so each stepped cell is bounds-checked on its own,
        // same as every other cell drawn through `world_to_screen`.
        let (anchor_x, anchor_y) = self.world_offset(cx, cy);
        let to_screen = |dx: i32, dy: i32| -> Option<(u16, u16)> {
            let sx = anchor_x + dx;
            let sy = anchor_y + dy;
            if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
                return None;
            }
            Some((area.left() + sx as u16, area.top() + sy as u16))
        };

        for dy in -NODE_RY..=NODE_RY {
            for dx in -NODE_RX..=NODE_RX {
                if dx == 0 && dy == 0 {
                    continue;
                }
                // A ring, not a filled box: only cells at (roughly) the
                // radius count, which is what makes the centre free for the
                // caravan token. Corners are dropped (the top/bottom rows
                // stop one cell short of the full radius) so the shape reads
                // as rounded rather than as a plain rectangle.
                let on_ring = if dy == 0 {
                    dx.abs() == NODE_RX
                } else {
                    dy.abs() == NODE_RY && dx.abs() < NODE_RX
                };
                if !on_ring {
                    continue;
                }
                if let Some(pos) = to_screen(dx, dy) {
                    surface.put(pos, '\u{25cb}', style);
                }
            }
        }

        if let Some(name) = node.name {
            // Carved-caps: a space between every letter, the look of stone
            // lettering rather than a printed label -- and it doubles as the
            // spacing that keeps a long name from crowding whatever glyph is
            // directly under it.
            let spaced: String = name.to_uppercase().chars().flat_map(|c| [c, ' ']).collect();
            let spaced = spaced.trim_end();
            let start_dx = -(spaced.chars().count() as i32) / 2;
            let label_dy = NODE_RY + 1;
            let label_bg = Style::new().fg(base).bg(rgb(10, 10, 14));
            for (j, ch) in spaced.chars().enumerate() {
                // Blank the whole span first, including the letter-spacing
                // gaps: without this an edge line that happens to cross under
                // the label shows through the gaps and fuses into the word
                // (e.g. an untouched gap plus a `\` reading as part of a
                // letter).
                if let Some(pos) = to_screen(start_dx + j as i32, label_dy) {
                    surface.put(pos, if ch == ' ' { ' ' } else { ch }, label_bg);
                }
            }
        }
    }

    fn draw_caravan(&self, surface: &mut Surface<'_>, area: Rect) {
        let (wx, wy) = self.caravan_world_pos();
        if let Some(pos) = self.world_to_screen(area, wx, wy) {
            surface.put(
                pos,
                '\u{263c}',
                Style::new().fg(rgb(20, 16, 10)).bg(ui::ACCENT),
            );
        }
    }

    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title("The Riven Web")
            .border(Border::Double)
            .bg(rgb(10, 10, 14));
        let inner = panel.draw(surface, area);
        if inner.width() < 4 || inner.height() < 4 {
            return;
        }

        self.zoom = Self::zoom_for((inner.width(), inner.height()));
        self.scroll = self.camera_scroll((inner.width(), inner.height()));
        self.draw_terrain(surface, inner);
        self.draw_dust(surface, inner);

        for edge in &self.graph.edges {
            let on_plan = self
                .plan
                .windows(2)
                .any(|w| (w[0] == edge.a && w[1] == edge.b) || (w[0] == edge.b && w[1] == edge.a));
            self.draw_edge(surface, inner, edge, on_plan);
        }
        for idx in 0..self.graph.nodes.len() {
            self.draw_node(surface, inner, idx);
            let (nx, ny) = self.graph.nodes[idx].pos();
            if let Some((sx, sy)) = self.world_to_screen(inner, nx, ny) {
                let rect = Rect::new(
                    sx.saturating_sub(NODE_RX as u16),
                    sy.saturating_sub(NODE_RY as u16),
                    (NODE_RX as u16) * 2 + 1,
                    (NODE_RY as u16) * 2 + 1,
                );
                self.hotspots.push_tappable(rect, inner, Action::Node(idx));
            }
        }
        self.draw_caravan(surface, inner);
    }

    fn draw_crew(&self, surface: &mut Surface<'_>, area: Rect, compact: bool) {
        let panel = Panel::new().title("Crew").bg(panel::PANEL_BG);
        let inner = panel.draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let ratio = if self.supplies_max > 0.0 {
            self.supplies / self.supplies_max
        } else {
            0.0
        };
        let tiers = tier_counts(self.crew_total, ratio);

        if compact || inner.height() < 5 {
            self.draw_crew_compact(surface, inner, tiers);
        } else {
            self.draw_crew_full(surface, inner, tiers);
        }
    }

    /// The one-or-two-row summary used on portrait and on a squeezed sidebar:
    /// all four tier counts on one line, resources on the next if there is
    /// room for it.
    fn draw_crew_compact(&self, surface: &mut Surface<'_>, inner: Rect, tiers: [u32; 4]) {
        let [fervent, subservient, invigorated, sustained] = tiers;
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[
                Span::plain("Fervent "),
                Span::keyword(&fervent.to_string()),
                Span::plain("  Subserv "),
                Span::keyword(&subservient.to_string()),
                Span::plain("  Invig "),
                Span::keyword(&invigorated.to_string()),
                Span::plain("  Sustain "),
                Span::keyword(&sustained.to_string()),
            ],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[
                    Span::plain("Supplies "),
                    Span::keyword(&format!("{:.0}/{:.0}", self.supplies, self.supplies_max)),
                    Span::plain("  Water "),
                    Span::keyword(&format!("{:.0}/{:.0}", self.water, self.water_max)),
                    Span::plain("  Coin "),
                    Span::keyword(&self.money.to_string()),
                ],
                panel::PANEL_BG,
            );
        }
    }

    /// The full report used on desktop and landscape: one row per named
    /// tier, then three rows of resource figures.
    fn draw_crew_full(&self, surface: &mut Surface<'_>, inner: Rect, tiers: [u32; 4]) {
        let [fervent, subservient, invigorated, sustained] = tiers;
        let rows: [(&str, u32, Color); 4] = [
            ("Fervent", fervent, rgb(226, 184, 96)),
            ("Subservient", subservient, rgb(150, 178, 214)),
            ("Invigorated", invigorated, rgb(150, 200, 150)),
            ("Sustained", sustained, rgb(180, 150, 150)),
        ];
        for (i, (label, count, color)) in rows.iter().enumerate() {
            let y = inner.top() + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    Span::new(label, *color),
                    Span::plain(": "),
                    Span::keyword(&count.to_string()),
                ],
                panel::PANEL_BG,
            );
        }

        let days_left = if SUPPLY_PER_DAY > 0.0 {
            self.supplies / SUPPLY_PER_DAY
        } else {
            0.0
        };
        let stats: [(&str, String); 3] = [
            (
                "Supplies ",
                format!("{:.0}/{:.0}", self.supplies, self.supplies_max),
            ),
            ("Water ", format!("{:.0}/{:.0}", self.water, self.water_max)),
            (
                "Days of food ",
                format!("{days_left:.0}  Money {}", self.money),
            ),
        ];
        for (i, (label, value)) in stats.iter().enumerate() {
            let y = inner.top() + 4 + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::plain(label), Span::keyword(value)],
                panel::PANEL_BG,
            );
        }
    }

    /// Draws the calendar as a real radial ornament: an ellipse traced by
    /// sampling angle, with a sweep marker orbiting it continuously while
    /// the year/month/day/weekday text stays fixed to its own cell.
    fn draw_calendar(&self, surface: &mut Surface<'_>, area: Rect) {
        let badge = format!("Y{}", self.calendar.year);
        let panel = Panel::new()
            .title("Calendar")
            .badge(&badge)
            .bg(panel::PANEL_BG);
        let inner = panel.draw(surface, area);
        if inner.width() < 8 || inner.height() < 5 {
            // Too small for the ornament: at least show the date as text.
            if inner.height() > 0 {
                panel::spans(
                    surface,
                    (inner.left(), inner.top()),
                    inner.width(),
                    &[Span::keyword(&format!(
                        "{} {}, {}",
                        self.calendar.month_name(),
                        self.calendar.day,
                        self.calendar.weekday()
                    ))],
                    panel::PANEL_BG,
                );
            }
            return;
        }

        let rx = f32::from(inner.width() / 2).max(2.0) - 1.0;
        let ry = f32::from(inner.height() / 2).max(2.0) - 1.0;
        let cx = f32::from(inner.left()) + f32::from(inner.width()) / 2.0;
        let cy = f32::from(inner.top()) + f32::from(inner.height()) / 2.0;

        for i in 0..CALENDAR_RING_STEPS {
            let angle = f32::from(i as u16) / f32::from(CALENDAR_RING_STEPS as u16)
                * core::f32::consts::TAU;
            let x = angle.cos().mul_add(rx, cx).round() as i32;
            let y = angle.sin().mul_add(ry, cy).round() as i32;
            if x >= 0
                && y >= 0
                && (x as u16) < inner.right()
                && (y as u16) < inner.bottom()
                && (x as u16) >= inner.left()
                && (y as u16) >= inner.top()
            {
                surface.put(
                    (x as u16, y as u16),
                    '\u{00b7}',
                    Style::new().fg(rgb(150, 130, 90)).bg(panel::PANEL_BG),
                );
            }
        }

        // The sweep marker: ambient motion tied to elapsed simulated time,
        // not to any resource, so it keeps moving even while the caravan is
        // camped -- the ornament's whole reason for existing over a static
        // date stamp.
        let sweep_angle = self.time * 0.6;
        let sx = sweep_angle.cos().mul_add(rx, cx).round() as i32;
        let sy = sweep_angle.sin().mul_add(ry, cy).round() as i32;
        if sx >= 0 && sy >= 0 && (sx as u16) < inner.right() && (sy as u16) < inner.bottom() {
            surface.put(
                (sx as u16, sy as u16),
                '\u{263c}',
                Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
            );
        }

        let dial_x = cx.round() as u16;
        let dial_y = cy.round() as u16;
        if dial_y > inner.top() {
            print_centered(surface, dial_x, dial_y - 1, self.calendar.weekday(), ui::FG);
        }
        print_centered(
            surface,
            dial_x,
            dial_y,
            &format!("Day {}", self.calendar.day),
            ui::ACCENT,
        );
        if dial_y + 1 < inner.bottom() {
            print_centered(
                surface,
                dial_x,
                dial_y + 1,
                self.calendar.month_name(),
                ui::DIM,
            );
        }
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Discovery Log").bg(panel::PANEL_BG);
        let inner = panel.draw(surface, area);
        self.log.draw(surface, inner, panel::PANEL_BG);
    }

    fn draw_actions(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().bg(panel::PANEL_BG);
        let inner = panel.draw(surface, area);
        if inner.height() == 0 {
            return;
        }

        let (mp, days, supply_cost) = self.preview();
        let can_depart = self.plan.len() > 1;
        let over_budget = supply_cost > self.supplies;

        let half_h = inner.height() / 2;
        let (top_half, bottom_half) = if half_h == 0 {
            (inner, Rect::new(inner.left(), inner.top(), 0, 0))
        } else {
            panel::split_top(inner, half_h)
        };

        let depart_color = if !can_depart {
            scale(rgb(150, 180, 150), 0.5)
        } else if over_budget {
            rgb(216, 88, 84)
        } else {
            rgb(150, 200, 150)
        };
        let depart_label = if self.departed {
            "EN ROUTE"
        } else if can_depart {
            "DEPART"
        } else {
            "NO ROUTE"
        };
        surface.fill_rect(top_half, ' ', Style::new().bg(rgb(18, 20, 16)));
        print_centered(
            surface,
            top_half.left() + top_half.width() / 2,
            top_half.top() + top_half.height() / 2,
            depart_label,
            depart_color,
        );
        self.hotspots.push_tappable(top_half, inner, Action::Depart);

        if bottom_half.height() > 0 {
            surface.fill_rect(bottom_half, ' ', Style::new().bg(rgb(22, 16, 16)));
            print_centered(
                surface,
                bottom_half.left() + bottom_half.width() / 2,
                bottom_half.top() + bottom_half.height() / 2,
                "CLEAR",
                rgb(200, 140, 100),
            );
            self.hotspots
                .push_tappable(bottom_half, inner, Action::Clear);
        }

        if mp > 0 && !self.departed {
            // Show the preview one row above the button strip if there is
            // room, so the cost is visible right where the confirm decision
            // is made.
            let text = format!("{mp} MP, {days:.0}d, {supply_cost:.0} food");
            let color = if over_budget {
                rgb(216, 88, 84)
            } else {
                ui::DIM
            };
            if area.top() > 0 {
                let msg_area = Rect::new(area.left(), area.top() - 1, area.width(), 1);
                surface.fill_rect(msg_area, ' ', Style::new().bg(ui::BG));
                surface.print(
                    (msg_area.left(), msg_area.top()),
                    &text,
                    Style::new().fg(color).bg(ui::BG),
                );
            }
        }
    }
}

/// Steps a Bresenham line from `(x0, y0)` to `(x1, y1)` and returns every
/// cell it passes through, inclusive of both ends.
fn bresenham_cells(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let mut cells = Vec::new();
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (i32::from(x0 < x1) * 2 - 1, i32::from(y0 < y1) * 2 - 1);
    let (mut x, mut y) = (x0, y0);
    let mut err = dx + dy;
    loop {
        cells.push((x, y));
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
    cells
}

/// Picks a line glyph from a local step direction. CP437 has no diagonal
/// box-drawing glyphs, so `/` and `\` (plain ASCII, always available) stand
/// in for the two diagonal cases, matching the same trade every other demo
/// in this gallery makes for angled connectors.
const fn edge_glyph(dx: i32, dy: i32) -> char {
    match (dx.signum(), dy.signum()) {
        (0, _) => '\u{2502}',
        (_, 0) => '\u{2500}',
        (1, 1) | (-1, -1) => '\\',
        _ => '/',
    }
}

/// Prints `text` centred on `(cx, cy)`, clipped to nothing if it would start
/// off the left edge. Used for the calendar rosette's fixed labels.
fn print_centered(surface: &mut Surface<'_>, cx: u16, cy: u16, text: &str, color: Color) {
    let half = text.chars().count() as u16 / 2;
    let Some(x0) = cx.checked_sub(half) else {
        return;
    };
    surface.print((x0, cy), text, Style::new().fg(color).bg(panel::PANEL_BG));
}

impl Demo for RivenRoute {
    const NAME: &'static str = "41_riven_route";
    const TITLE: &'static str = "41 Riven Route";
    const BLURB: &'static str =
        "Vagrus caravan: weighted node graph, route cost preview, supply attrition.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("tap node", "extend route"),
            ("Enter", "depart"),
            ("C", "clear route"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let shape = Shape::of(content);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.hotspots.clear();
        let (crew_area, cal_area, map_area, log_area, action_area) = Self::layout(content, shape);

        self.draw_map(&mut surface, map_area);
        self.draw_crew(&mut surface, crew_area, shape.stacks());
        self.draw_calendar(&mut surface, cal_area);
        self.draw_log(&mut surface, log_area);
        self.draw_actions(&mut surface, action_area);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(RivenRoute);
