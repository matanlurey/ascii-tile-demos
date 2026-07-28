//! 64: Edict Scales -- Tyranny's opposed standing meters and spell assembly.
//!
//! Tyranny (Obsidian, 2016) never lets you slide along one axis from
//! "hated" to "loved". Every companion accrues Loyalty *and* Fear as two
//! independent totals, and every faction accrues Favor *and* Wrath the same
//! way; you can be a magistrate a companion is devoted to and terrified of at
//! once, and the game means that as a real, distinct state rather than a
//! contradiction to be averaged away. A single bar with a villain end and a
//! hero end cannot show that: it forces two numbers into one position and the
//! "high in both" state becomes indistinguishable from "middling in both".
//!
//! Techniques on show:
//!
//! - **The centre-out opposed bar** ([`opposed_bar`]): two independent
//!   meters sharing one track and one midpoint, each growing outward in its
//!   own colour, with threshold ticks marking the bands Tyranny's own UI
//!   calls out (Devoted/Trusted/Cold and the like). Half-cell fill precision
//!   via CP437 `█`/`▌`, the same constraint documented in
//!   [`ascii_tile_demos::ui::panel`].
//! - **Compositional spell preview**: a spell is a Core Sigil (element and
//!   base effect) plus an Expression (delivery shape) plus a pool of
//!   Accents (cost-for-effect trades), and changing any part regenerates the
//!   spell's name, cost, and description live, in the spirit of the
//!   rich-text colouring used elsewhere in this gallery for generated
//!   descriptions ([`ascii_tile_demos::ui::panel::spans`]).
//! - **An ornate double frame with a tab strip** ([`tilekit::autotile::BOX_DOUBLE`]):
//!   Tyranny's own chrome is heavy gold-and-verdigris scrollwork; the nearest
//!   a terminal gets is a doubled box-drawing border with corner ornaments,
//!   scaled down rather than dropped at 80x24.
//!
//! Reference for the threshold-band convention: `RogueBasin`'s notes on
//! multi-axis reputation systems, and Obsidian's own Tyranny companion
//! journal, which labels Loyalty/Fear bands rather than showing raw numbers.
//!
//! ```sh
//! cargo run --example 64_edict_scales --features crossterm
//! cargo run --example 64_edict_scales --features software
//! cargo run --example 64_edict_scales --features gl
//! cargo run --example 64_edict_scales  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, ACCENT, DIM, FG};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{E, W};
use tilekit::noise::hash01;
use tilekit::palette::{self, mix, rgb};

/// Colour for the meter that grows leftward: Fear on a companion, Wrath on a
/// faction. Both read as the same "this will hurt you" channel.
const LEFT_COLOR: Color = rgb(206, 84, 78);
/// Colour for the meter that grows rightward: Loyalty on a companion, Favor
/// on a faction.
const RIGHT_COLOR: Color = rgb(96, 196, 148);
/// Track colour under an unfilled half.
const TRACK: Color = rgb(38, 40, 54);
/// Row background inside the standing list.
const ROW_BG: Color = rgb(14, 15, 22);
/// Row background for the currently selected entity.
const ROW_SELECTED_BG: Color = rgb(30, 28, 20);

/// One meter's ceiling. Tyranny's own values run 0-100 per axis, and both
/// axes share the same ceiling since neither is a remainder of the other.
const MAX_STANDING: f32 = 100.0;

/// Fractions of [`MAX_STANDING`] at which [`opposed_bar`] marks a threshold
/// tick, matching the band boundaries Tyranny's journal uses to name a
/// standing (e.g. Wary / Cold / Hostile).
const TICK_FRACTIONS: [f32; 3] = [0.25, 0.5, 0.75];

/// Seconds between automatic edicts. An edict lands on its own so the board
/// is never static, but slowly enough that a deliberate action still reads
/// as the dominant cause of a bar's movement.
const AUTO_EDICT_INTERVAL: f32 = 3.2;

/// How much one edict moves its meter.
const EDICT_STEP: f32 = 14.0;

/// Who a standing entry belongs to: a companion (Loyalty/Fear) or a faction
/// (Favor/Wrath). The two labels differ; the widget and the arithmetic do
/// not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StandingKind {
    Companion,
    Faction,
}

impl StandingKind {
    /// `(left label, right label)`, left growing on a harsh edict, right on
    /// a merciful one.
    const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Companion => ("Fear", "Loyalty"),
            Self::Faction => ("Wrath", "Favor"),
        }
    }
}

/// One companion or faction, tracking both of its opposed totals.
struct Standing {
    name: &'static str,
    kind: StandingKind,
    left: f32,
    right: f32,
}

impl Standing {
    const fn new(name: &'static str, kind: StandingKind, left: f32, right: f32) -> Self {
        Self {
            name,
            kind,
            left,
            right,
        }
    }

    /// Applies one edict. `harsh` pushes the left (Fear/Wrath) meter;
    /// otherwise the right (Loyalty/Favor) meter grows. Real edicts in
    /// Tyranny are rarely pure: a harsh edict that keeps its promise also
    /// nudges the opposite meter up a little, which is what keeps "loved and
    /// feared" reachable rather than the two totals staying mutually
    /// exclusive in practice.
    fn apply_edict(&mut self, harsh: bool) {
        if harsh {
            self.left = (self.left + EDICT_STEP).min(MAX_STANDING);
            self.right = EDICT_STEP.mul_add(0.15, self.right).min(MAX_STANDING);
        } else {
            self.right = (self.right + EDICT_STEP).min(MAX_STANDING);
            self.left = EDICT_STEP.mul_add(0.15, self.left).min(MAX_STANDING);
        }
    }
}

/// Starting roster: three companions and two factions, enough that the
/// "high in both" case is visible in the initial frame rather than only
/// after play.
fn seed_standings() -> Vec<Standing> {
    vec![
        Standing::new("Verse", StandingKind::Companion, 62.0, 71.0),
        Standing::new("Barik", StandingKind::Companion, 18.0, 84.0),
        Standing::new("Lantry", StandingKind::Companion, 74.0, 22.0),
        Standing::new("The Disfavored", StandingKind::Faction, 40.0, 40.0),
        Standing::new("The Scarlet Chorus", StandingKind::Faction, 66.0, 30.0),
    ]
}

/// A Core Sigil: the element and base effect a spell is built around.
#[derive(Clone, Copy)]
struct Sigil {
    name: &'static str,
    color: Color,
    effect: &'static str,
    base_cost: i32,
}

const SIGILS: [Sigil; 4] = [
    Sigil {
        name: "Fire",
        color: rgb(226, 116, 64),
        effect: "burns",
        base_cost: 12,
    },
    Sigil {
        name: "Frost",
        color: rgb(120, 196, 224),
        effect: "chills",
        base_cost: 10,
    },
    Sigil {
        name: "Lightning",
        color: rgb(236, 214, 120),
        effect: "shocks",
        base_cost: 14,
    },
    Sigil {
        name: "Bone",
        color: rgb(196, 190, 172),
        effect: "withers",
        base_cost: 11,
    },
];

/// An Expression: the shape a spell's effect is delivered in.
#[derive(Clone, Copy)]
struct Expression {
    name: &'static str,
    shape: &'static str,
    cost_mult: f32,
}

const EXPRESSIONS: [Expression; 4] = [
    Expression {
        name: "Bolt",
        shape: "a single target",
        cost_mult: 1.0,
    },
    Expression {
        name: "Ring",
        shape: "everything at range",
        cost_mult: 1.4,
    },
    Expression {
        name: "Aura",
        shape: "everyone nearby",
        cost_mult: 1.7,
    },
    Expression {
        name: "Wall",
        shape: "a line across the field",
        cost_mult: 1.3,
    },
];

/// An Accent: a modifier traded against cost. Any number can be toggled on.
#[derive(Clone, Copy)]
struct Accent {
    name: &'static str,
    cost_delta: i32,
    effect: &'static str,
}

const ACCENTS: [Accent; 5] = [
    Accent {
        name: "Empower",
        cost_delta: 6,
        effect: "with greater force",
    },
    Accent {
        name: "Extend",
        cost_delta: 3,
        effect: "lasting longer",
    },
    Accent {
        name: "Quicken",
        cost_delta: 4,
        effect: "cast without delay",
    },
    Accent {
        name: "Focus",
        cost_delta: -4,
        effect: "narrowed to strike harder",
    },
    Accent {
        name: "Ration",
        cost_delta: -5,
        effect: "drawn thin to cost less",
    },
];

/// Which top-level view is showing. Switched by the tab strip / `Tab` key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Standing,
    Spellcraft,
}

/// Which pool has keyboard focus while building a spell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pool {
    Core,
    Expression,
    Accents,
}

/// A tappable region's meaning, for [`Hotspots`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    SwitchView(View),
    SelectStanding(usize),
    Merciful,
    Harsh,
    SelectPool(Pool),
    PickCore(usize),
    PickExpression(usize),
    ToggleAccent(usize),
}

/// Demo state: the standing roster, the spell-builder selections, and the
/// automatic-edict clock.
pub struct EdictScales {
    standings: Vec<Standing>,
    selected: usize,
    view: View,
    pool: Pool,
    core: usize,
    expression: usize,
    accent_idx: usize,
    accents_on: [bool; ACCENTS.len()],
    auto_timer: f32,
    /// Advances every automatic edict so the deterministic hash draws a
    /// different (but reproducible) target and direction each time.
    edict_tick: u32,
    fps: FpsMeter,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
}

impl Default for EdictScales {
    fn default() -> Self {
        Self {
            standings: seed_standings(),
            selected: 0,
            view: View::Standing,
            pool: Pool::Core,
            core: 1,
            expression: 0,
            accent_idx: 0,
            accents_on: [false; ACCENTS.len()],
            auto_timer: 0.0,
            edict_tick: 0,
            fps: FpsMeter::new(),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
        }
    }
}

impl EdictScales {
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
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap
            && let Some(action) = self.hotspots.hit(pos).copied()
        {
            self.run_action(action);
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => {
                self.view = match self.view {
                    View::Standing => View::Spellcraft,
                    View::Spellcraft => View::Standing,
                };
            }
            KeyCode::Char('r' | 'R') => {
                self.standings = seed_standings();
                self.accents_on = [false; ACCENTS.len()];
                self.core = 1;
                self.expression = 0;
            }
            _ => match self.view {
                View::Standing => self.handle_standing_key(code),
                View::Spellcraft => self.handle_spellcraft_key(code),
            },
        }
    }

    fn handle_standing_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('w' | 'W') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('s' | 'S') => {
                self.selected = (self.selected + 1).min(self.standings.len() - 1);
            }
            KeyCode::Char('m' | 'M') => self.run_action(Action::Merciful),
            KeyCode::Char('h' | 'H') => self.run_action(Action::Harsh),
            _ => {}
        }
    }

    fn handle_spellcraft_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left | KeyCode::Char('a' | 'A') => {
                self.pool = match self.pool {
                    Pool::Core => Pool::Accents,
                    Pool::Expression => Pool::Core,
                    Pool::Accents => Pool::Expression,
                };
            }
            KeyCode::Right | KeyCode::Char('d' | 'D') => {
                self.pool = match self.pool {
                    Pool::Core => Pool::Expression,
                    Pool::Expression => Pool::Accents,
                    Pool::Accents => Pool::Core,
                };
            }
            KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_pool_cursor(-1),
            KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_pool_cursor(1),
            KeyCode::Enter | KeyCode::Char(' ') => self.commit_pool_cursor(),
            _ => {}
        }
    }

    const fn move_pool_cursor(&mut self, dir: i32) {
        match self.pool {
            Pool::Core => {
                self.core = wrap_index(self.core, dir, SIGILS.len());
            }
            Pool::Expression => {
                self.expression = wrap_index(self.expression, dir, EXPRESSIONS.len());
            }
            Pool::Accents => {
                self.accent_idx = wrap_index(self.accent_idx, dir, ACCENTS.len());
            }
        }
    }

    fn commit_pool_cursor(&mut self) {
        match self.pool {
            Pool::Core => self.run_action(Action::PickCore(self.core)),
            Pool::Expression => self.run_action(Action::PickExpression(self.expression)),
            Pool::Accents => self.run_action(Action::ToggleAccent(self.accent_idx)),
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::SwitchView(view) => self.view = view,
            Action::SelectStanding(i) => self.selected = i,
            Action::Merciful => {
                if let Some(s) = self.standings.get_mut(self.selected) {
                    s.apply_edict(false);
                }
            }
            Action::Harsh => {
                if let Some(s) = self.standings.get_mut(self.selected) {
                    s.apply_edict(true);
                }
            }
            Action::SelectPool(pool) => self.pool = pool,
            Action::PickCore(i) => {
                self.pool = Pool::Core;
                self.core = i;
            }
            Action::PickExpression(i) => {
                self.pool = Pool::Expression;
                self.expression = i;
            }
            Action::ToggleAccent(i) => {
                self.pool = Pool::Accents;
                self.accent_idx = i;
                if let Some(on) = self.accents_on.get_mut(i) {
                    *on = !*on;
                }
            }
        }
    }

    /// Advances the automatic-edict clock and, when it rolls over, applies
    /// one edict to one entity. Driven entirely from `delta` so two renders
    /// of the same accumulated time produce the same edict, which the
    /// determinism test requires.
    fn tick_auto_edict(&mut self, delta: f32) {
        self.auto_timer += delta;
        if self.auto_timer < AUTO_EDICT_INTERVAL {
            return;
        }
        self.auto_timer -= AUTO_EDICT_INTERVAL;
        self.edict_tick += 1;
        let n = self.standings.len() as u32;
        let target = (hash01(0x1BAD_C0DE, self.edict_tick as i32, 0) * n as f32) as usize;
        let harsh = hash01(0x5EED_F00D, self.edict_tick as i32, 1) < 0.5;
        if let Some(s) = self.standings.get_mut(target.min(n as usize - 1)) {
            s.apply_edict(harsh);
        }
    }

    /// Builds the generated spell's name, total cost, and description spans
    /// from the current pool selections.
    fn assembled_spell(&self) -> (String, i32, Vec<(String, Color)>) {
        let sigil = SIGILS[self.core];
        let expr = EXPRESSIONS[self.expression];
        let name = format!("{} of {}", expr.name, sigil.name);

        let mut cost = (f32::from(sigil.base_cost as i16) * expr.cost_mult).round() as i32;
        let mut accent_fragments = Vec::new();
        for (i, accent) in ACCENTS.iter().enumerate() {
            if self.accents_on[i] {
                cost += accent.cost_delta;
                accent_fragments.push(accent);
            }
        }
        cost = cost.max(1);

        let mut spans = vec![
            (sigil.name.to_string(), sigil.color),
            (format!(" {} ", sigil.effect), FG),
            (expr.shape.to_string(), ACCENT),
        ];
        for accent in &accent_fragments {
            spans.push((", ".to_string(), DIM));
            spans.push((accent.effect.to_string(), rgb(200, 176, 226)));
        }
        spans.push((".".to_string(), FG));
        (name, cost, spans)
    }

    fn status(&self) -> String {
        match self.view {
            View::Standing => {
                let s = &self.standings[self.selected];
                let (left, right) = s.kind.labels();
                format!(
                    "{}  {left} {:.0}  {right} {:.0}  next edict in {:.1}s  M merciful  H harsh",
                    s.name,
                    s.left,
                    s.right,
                    AUTO_EDICT_INTERVAL - self.auto_timer
                )
            }
            View::Spellcraft => {
                let (name, cost, _) = self.assembled_spell();
                format!("{name}  cost {cost}  Tab: standing  Enter/Space: toggle accent")
            }
        }
    }

    fn draw(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(area);

        let frame = Panel::new()
            .title("Edict Scales")
            .border(Border::Double)
            .badge(match self.view {
                View::Standing => "Standing",
                View::Spellcraft => "Spellcraft",
            })
            .frame(rgb(150, 122, 58))
            .bg(ui::BG);
        let inner = frame.draw(surface, area);
        if inner.width() < 4 || inner.height() < 4 {
            return;
        }

        let (tabs, rest) = panel::split_top(inner, 1);
        self.draw_tabs(surface, tabs);
        let (_rule, body) = panel::split_top(rest, 1);
        draw_rule(surface, Rect::new(rest.left(), rest.top(), rest.width(), 1));

        match self.view {
            View::Standing => self.draw_standing(surface, body),
            View::Spellcraft => self.draw_spellcraft(surface, body, shape),
        }
    }

    fn draw_tabs(&mut self, surface: &mut Surface<'_>, area: Rect) {
        surface.fill_rect(area, ' ', Style::new().bg(ui::BG));
        let labels = [
            (View::Standing, " Standing "),
            (View::Spellcraft, " Spellcraft "),
        ];
        let mut x = area.left();
        for (view, label) in labels {
            let w = label.chars().count() as u16;
            if x + w > area.right() {
                break;
            }
            let rect = Rect::new(x, area.top(), w, 1);
            let active = view == self.view;
            let style = if active {
                Style::new().fg(palette::BLACK).bg(ACCENT)
            } else {
                Style::new().fg(DIM).bg(ui::BG)
            };
            surface.print((x, area.top()), label, style);
            self.hotspots.push(rect, Action::SwitchView(view));
            x += w + 1;
        }
    }

    fn draw_standing(&mut self, surface: &mut Surface<'_>, area: Rect) {
        // Two rows per entry (name, bar) plus a blank separator, so the list
        // degrades gracefully: a short viewport just shows fewer entries
        // rather than crushing every row.
        let row_h = 3;
        let mut y = area.top();
        for (i, standing) in self.standings.iter().enumerate() {
            if y + row_h > area.bottom() {
                break;
            }
            let row = Rect::new(area.left(), y, area.width(), row_h);
            let selected = i == self.selected;
            let bg = if selected { ROW_SELECTED_BG } else { ROW_BG };
            surface.fill_rect(row, ' ', Style::new().bg(bg));

            let kind_tag = match standing.kind {
                StandingKind::Companion => "companion",
                StandingKind::Faction => "faction",
            };
            let name_style = Style::new().fg(if selected { ACCENT } else { FG }).bg(bg);
            surface.print((row.left(), row.top()), standing.name, name_style);
            let tag_x = row.left() + standing.name.chars().count() as u16 + 1;
            if tag_x < row.right() {
                surface.print((tag_x, row.top()), kind_tag, Style::new().fg(DIM).bg(bg));
            }

            let (left_label, right_label) = standing.kind.labels();
            let prefix = format!("{left_label:>7} {:>3.0}", standing.left);
            let suffix = format!("{:<3.0} {right_label:<7}", standing.right);
            let prefix_w = prefix.chars().count() as u16;
            let suffix_w = suffix.chars().count() as u16;
            surface.print(
                (row.left(), row.top() + 1),
                &prefix,
                Style::new().fg(LEFT_COLOR).bg(bg),
            );
            if row.width() > prefix_w + suffix_w + 4 {
                let bar_x = row.left() + prefix_w + 1;
                let bar_w = row.width() - prefix_w - suffix_w - 2;
                opposed_bar(
                    surface,
                    (bar_x, row.top() + 1),
                    bar_w,
                    Meter::new(standing.left, LEFT_COLOR),
                    Meter::new(standing.right, RIGHT_COLOR),
                    TRACK,
                    bg,
                );
                surface.print(
                    (bar_x + bar_w + 1, row.top() + 1),
                    &suffix,
                    Style::new().fg(RIGHT_COLOR).bg(bg),
                );
            }

            self.hotspots.push(row, Action::SelectStanding(i));
            y += row_h;
        }
    }

    fn draw_spellcraft(&mut self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        let (pools_area, preview_area) = if shape.stacks() {
            // Portrait: the pools are tall enough to stack and still leave
            // the preview readable underneath.
            panel::split_bottom(area, (area.height() / 2).max(6))
        } else {
            panel::split_bottom(area, 8.min(area.height().saturating_sub(6)).max(4))
        };
        self.draw_pools(surface, pools_area, shape);
        self.draw_preview(surface, preview_area);
    }

    fn draw_pools(&mut self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        if shape.stacks() {
            // One pool at a time, switched by the same Left/Right the
            // side-by-side layout uses to move focus between columns: the
            // key that used to change *which* column is now the key that
            // changes *which pool is shown*.
            self.draw_pool_header(surface, area);
            let (_header, list_area) = panel::split_top(area, 1);
            match self.pool {
                Pool::Core => self.draw_core_pool(surface, list_area),
                Pool::Expression => self.draw_expression_pool(surface, list_area),
                Pool::Accents => self.draw_accent_pool(surface, list_area),
            }
            return;
        }
        let cols = panel::columns(area, 3, 1);
        self.draw_labeled_pool(surface, cols[0], "Core Sigil", Pool::Core);
        self.draw_core_pool(surface, panel::split_top(cols[0], 1).1);
        self.draw_labeled_pool(surface, cols[1], "Expression", Pool::Expression);
        self.draw_expression_pool(surface, panel::split_top(cols[1], 1).1);
        self.draw_labeled_pool(surface, cols[2], "Accents", Pool::Accents);
        self.draw_accent_pool(surface, panel::split_top(cols[2], 1).1);
    }

    fn draw_pool_header(&self, surface: &mut Surface<'_>, area: Rect) {
        let label = match self.pool {
            Pool::Core => "Core Sigil",
            Pool::Expression => "Expression",
            Pool::Accents => "Accents",
        };
        let text = format!("{label} (A/D to switch)");
        surface.print(
            (area.left(), area.top()),
            &text,
            Style::new().fg(ACCENT).bg(ui::BG),
        );
    }

    fn draw_labeled_pool(
        &mut self,
        surface: &mut Surface<'_>,
        area: Rect,
        label: &str,
        pool: Pool,
    ) {
        let header = Rect::new(area.left(), area.top(), area.width(), 1);
        let color = if pool == self.pool { ACCENT } else { DIM };
        surface.print(
            (header.left(), header.top()),
            label,
            Style::new().fg(color).bg(ui::BG),
        );
        self.hotspots.push(header, Action::SelectPool(pool));
    }

    fn draw_core_pool(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for (i, sigil) in SIGILS.iter().enumerate() {
            let Some(row) = pool_row(area, i) else { break };
            let picked = i == self.core;
            let focused = picked && self.pool == Pool::Core;
            let bg = row_bg(picked, focused);
            surface.fill_rect(row, ' ', Style::new().bg(bg));
            let marker = if picked { '\u{2022}' } else { '\u{25CB}' };
            surface.put(
                (row.left(), row.top()),
                marker,
                Style::new().fg(sigil.color).bg(bg),
            );
            surface.print(
                (row.left() + 2, row.top()),
                sigil.name,
                Style::new().fg(sigil.color).bg(bg),
            );
            self.hotspots.push(row, Action::PickCore(i));
        }
    }

    fn draw_expression_pool(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for (i, expr) in EXPRESSIONS.iter().enumerate() {
            let Some(row) = pool_row(area, i) else { break };
            let picked = i == self.expression;
            let focused = picked && self.pool == Pool::Expression;
            let bg = row_bg(picked, focused);
            surface.fill_rect(row, ' ', Style::new().bg(bg));
            let marker = if picked { '\u{2022}' } else { '\u{25CB}' };
            surface.put(
                (row.left(), row.top()),
                marker,
                Style::new().fg(ACCENT).bg(bg),
            );
            surface.print(
                (row.left() + 2, row.top()),
                expr.name,
                Style::new().fg(FG).bg(bg),
            );
            self.hotspots.push(row, Action::PickExpression(i));
        }
    }

    fn draw_accent_pool(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for (i, accent) in ACCENTS.iter().enumerate() {
            let Some(row) = pool_row(area, i) else { break };
            let on = self.accents_on[i];
            let focused = i == self.accent_idx && self.pool == Pool::Accents;
            let bg = row_bg(on, focused);
            surface.fill_rect(row, ' ', Style::new().bg(bg));
            // No CP437 checkbox glyphs exist, so the on/off state is spelled
            // out in brackets rather than reached for a Unicode `\u{2610}`/`\u{2612}`
            // pair, which would render as a solid block on the pixel backends.
            let marker = if on { "[x]" } else { "[ ]" };
            let sign = if accent.cost_delta >= 0 { '+' } else { '-' };
            let text = format!("{marker} {} {sign}{}", accent.name, accent.cost_delta.abs());
            surface.print(
                (row.left(), row.top()),
                &text,
                Style::new().fg(if on { RIGHT_COLOR } else { DIM }).bg(bg),
            );
            self.hotspots.push(row, Action::ToggleAccent(i));
        }
    }

    fn draw_preview(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new()
            .title("Preview")
            .border(Border::Single)
            .bg(ui::BG)
            .draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        let (name, cost, description) = self.assembled_spell();
        let title = format!("{name}  ({cost} favor)");
        surface.print(
            (inner.left(), inner.top()),
            &title,
            Style::new().fg(ACCENT).bg(ui::BG),
        );
        if inner.height() > 1 {
            let spans: Vec<Span<'_>> = description
                .iter()
                .map(|(text, color)| Span::new(text, *color))
                .collect();
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &spans,
                ui::BG,
            );
        }
    }
}

/// Which cell row an item at index `i` occupies inside `area`, or `None` once
/// the list runs out of vertical room.
fn pool_row(area: Rect, i: usize) -> Option<Rect> {
    let y = area.top() + i as u16;
    if y >= area.bottom() {
        return None;
    }
    Some(Rect::new(area.left(), y, area.width(), 1))
}

fn row_bg(picked: bool, focused: bool) -> Color {
    if focused {
        ROW_SELECTED_BG
    } else if picked {
        mix(ROW_SELECTED_BG, ui::BG, 0.5)
    } else {
        ui::BG
    }
}

/// Moves a pool cursor by `dir`, wrapping so Up from the first item lands on
/// the last and Down from the last wraps to the first.
const fn wrap_index(i: usize, dir: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i32;
    let next = (i as i32 + dir).rem_euclid(len);
    next as usize
}

/// Draws a plain horizontal rule across `area`, the divider between the tab
/// strip and the body of the frame.
fn draw_rule(surface: &mut Surface<'_>, area: Rect) {
    let style = Style::new().fg(rgb(90, 76, 46)).bg(ui::BG);
    for x in area.left()..area.right() {
        surface.put((x, area.top()), BOX_SINGLE_DASH, style);
    }
}

/// The horizontal single-weight rule glyph, used for the strip under the
/// tabs. Pulled from the same box-drawing mask [`ascii_tile_demos::ui::panel`]
/// uses for its own rules, so the two rule weights the frame mixes (double
/// outer border, single inner rule) read as one convention rather than two.
const BOX_SINGLE_DASH: char = tilekit::autotile::BOX_SINGLE[(E | W) as usize];

/// One meter's live value and the colour it fills in, for [`opposed_bar`].
#[derive(Clone, Copy)]
struct Meter {
    value: f32,
    color: Color,
}

impl Meter {
    const fn new(value: f32, color: Color) -> Self {
        Self { value, color }
    }
}

/// Draws a centre-out opposed reputation bar into one row.
///
/// `left`/`right` are independent totals, each in `0..=100` (see
/// [`MAX_STANDING`]). The track is split at its midpoint; the left meter
/// fills from the centre outward to the left in its own colour, the right
/// meter fills from the centre outward to the right in its own colour. Both
/// can reach full at once, which is the entire point: a companion who is
/// both terrified and devoted fills the whole track, and a single-axis bar
/// has no way to draw that.
///
/// Fill precision is half a cell (`█`/`▌`), the same trade [`panel::bar`]
/// makes and for the same reason: the eighth blocks that would give finer
/// precision are outside CP437 and render as solid colourless slabs on the
/// pixel backends. Unfilled cells at a threshold fraction of
/// [`TICK_FRACTIONS`] show a middle-dot tick instead of blank track, so a
/// glance says which named band ("about halfway", "past the third mark") a
/// meter is in without reading the number.
fn opposed_bar(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    left: Meter,
    right: Meter,
    track: Color,
    bg: Color,
) {
    let (x0, y) = at;
    if width < 3 {
        return;
    }
    let right_w = width / 2;
    let left_w = width - right_w - 1;
    let cx = x0 + left_w;

    surface.put((cx, y), '\u{2502}', Style::new().fg(track).bg(bg));

    let ticks: Vec<u16> = TICK_FRACTIONS
        .iter()
        .map(|f| (f32::from(right_w.max(left_w)) * f).round() as u16)
        .collect();

    let track_ref = Track {
        color: track,
        bg,
        ticks: &ticks,
    };
    draw_half(surface, (cx + 1, y), right_w, right, &track_ref, false);
    draw_half(surface, (cx, y), left_w, left, &track_ref, true);
}

/// Track colour, cell background, and threshold-tick positions shared by
/// both halves of [`opposed_bar`], bundled so [`draw_half`] stays under the
/// argument-count lint without folding unrelated values together.
struct Track<'a> {
    color: Color,
    bg: Color,
    ticks: &'a [u16],
}

/// Draws one side of [`opposed_bar`]. `mirrored` reverses the fill and tick
/// direction so the left half grows toward the centre from the far edge
/// while the right half grows away from it, without duplicating the loop.
fn draw_half(
    surface: &mut Surface<'_>,
    edge: (u16, u16),
    half_w: u16,
    meter: Meter,
    track: &Track<'_>,
    mirrored: bool,
) {
    let (edge_x, y) = edge;
    let t = (meter.value / MAX_STANDING).clamp(0.0, 1.0);
    let halves = (f32::from(half_w) * 2.0 * t).round() as u16;
    for i in 0..half_w {
        // Distance from the centre, in cells: 0 is the cell touching the
        // midpoint, `half_w - 1` is the far edge. Filling proceeds outward
        // from 0, matching how both meters grow from the shared centre.
        let dist = i;
        let x = if mirrored {
            edge_x.saturating_sub(1).saturating_sub(dist)
        } else {
            edge_x + dist
        };
        let filled = halves.saturating_sub(dist * 2).min(2);
        let is_tick = track.ticks.contains(&(dist + 1));
        let (glyph, fg) = match filled {
            2 => ('\u{2588}', meter.color),
            1 if mirrored => ('\u{2590}', meter.color),
            1 => ('\u{258C}', meter.color),
            _ if is_tick => ('\u{00B7}', mix(track.color, meter.color, 0.6)),
            _ => (' ', track.color),
        };
        surface.put((x, y), glyph, Style::new().fg(fg).bg(track.bg));
    }
}

impl Demo for EdictScales {
    const NAME: &'static str = "64_edict_scales";
    const TITLE: &'static str = "64 Edict scales";
    const BLURB: &'static str =
        "Tyranny's opposed standing meters and a compositional spell builder.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "switch view"),
            ("W/S", "select"),
            ("M/H", "merciful/harsh edict"),
            ("A/D", "switch pool"),
            ("Enter", "pick/toggle"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        self.tick_auto_edict(frame.delta.as_secs_f32());

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(EdictScales);
