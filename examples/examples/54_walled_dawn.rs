//! 54: Walled Dawn -- Kingdom: Two Crowns as a side-scrolling elevation.
//!
//! Every other demo in this gallery draws its world from above: top-down
//! cells, isometric wedges, or hexes. None of them is a side view, and a side
//! view has its own problems that the other projections never raise. This
//! demo exists to solve those problems on a character grid, not to add
//! another Kingdom clone:
//!
//! - **Parallax.** Far mountains, a mid treeline, the ground you actually
//!   stand on, a mid and near strip of grass, and drifting cloud cover all
//!   scroll at different fractions of the camera's motion, so depth reads
//!   from motion rather than from perspective lines a character grid cannot
//!   draw.
//! - **A ground plane.** One horizon row splits the world into sky above and
//!   earth below, rather than the field-from-above every map demo already
//!   uses.
//! - **Depth ordering.** Layers are painted back to front, so a mountain
//!   silhouette against the sky is legible even though nothing here has real
//!   occlusion geometry.
//! - **A one-axis camera.** The rider walks left and right only; the camera
//!   follows and clamps to a world several screens wide, so there is always
//!   more world offscreen in both directions.
//!
//! Around that projection sits Kingdom's actual loop: by day, coins bought by
//! riding the world accrue and buy idle folk and wall levels on both sides of
//! the camp; a day/night clock that runs whether or not the player is ready
//! sends creatures in from both world edges at night, and archers stationed
//! on the walls (or the absence of them) decide who is still standing at
//! dawn.
//!
//! Techniques on show:
//!
//! - **Six-layer parallax scroll** ([`WalledDawn::draw_clouds`],
//!   [`WalledDawn::draw_far_ridge`], [`WalledDawn::draw_treeline`],
//!   [`WalledDawn::draw_ground`], [`WalledDawn::draw_midground_grass`],
//!   [`WalledDawn::draw_foreground_grass`]): each layer samples silhouette
//!   noise or ground texture at `camera_x * factor + column`, so a factor
//!   under 1 crawls behind the world and a factor over 1 rushes past it,
//!   with the ground layer itself at exactly 1 (real world coordinates,
//!   where every entity actually lives).
//! - **A day/night clock legible at a glance**
//!   ([`WalledDawn::daylight`], [`WalledDawn::draw_sky`]): sky color, a
//!   travelling sun/moon, drifting clouds, a starfield, drifting birds by
//!   day, and a text countdown are all independent readings of the same
//!   `cycle_t` field, so a screenshot at any moment says what phase it is
//!   without needing the others -- the clock is something you watch move
//!   across the sky, not just a number in the corner.
//! - **A one-axis clamped camera** ([`WalledDawn::camera_x`]): the world is
//!   several viewports wide at every terminal size; the camera follows the
//!   rider and stops at either edge, which is the side-view equivalent of
//!   [`45_night_walk`](../45_night_walk)'s top-down camera clamp.
//! - **A timer-driven night that does not wait for the player**
//!   ([`WalledDawn::simulate`]): creatures spawn at both world edges on a
//!   fixed schedule once night falls and march toward the camp regardless of
//!   where the rider is standing; only the archers already stationed on a
//!   wall when a creature arrives can turn it away.
//! - **Tap-the-site building with full keyboard parity**
//!   ([`WalledDawn::layout`], [`ui::touch::Hotspots`]): every build and
//!   recruit action is a `>= 9x4` cell button in the bottom thumb zone, and
//!   the same actions are bound to keys so a desktop player never needs the
//!   mouse.
//!
//! ```sh
//! cargo run --example 54_walled_dawn --features crossterm
//! cargo run --example 54_walled_dawn --features software
//! cargo run --example 54_walled_dawn --features gl
//! cargo run --example 54_walled_dawn  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, KeyEventKind};
use retroglyph_core::{Backend, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{Rng, hash01, value_noise};
use tilekit::palette::{mix, rgb};

/// World length in abstract ground units. One unit is one column at the
/// ground layer's parallax factor of 1.0.
///
/// Large enough that even the widest desktop viewport this gallery targets
/// (roughly 160 columns) still has a substantial amount of world beyond both
/// edges of the screen -- the whole point of a one-axis camera is that there
/// is always more world offscreen, and a world barely wider than the
/// viewport cannot demonstrate that.
const WORLD_LEN: f32 = 420.0;

/// The camp's fixed position at the centre of the world.
const CAMP_X: f32 = WORLD_LEN / 2.0;

/// Distance from the camp to each wall, in world units.
const WALL_OFFSET: f32 = 16.0;

/// Rider ground speed, world units per second.
const PLAYER_SPEED: f32 = 16.0;

/// Seconds of daylight per cycle.
const DAY_LEN: f32 = 42.0;
/// Seconds of night per cycle.
const NIGHT_LEN: f32 = 26.0;
/// Full day/night cycle length.
const CYCLE_LEN: f32 = DAY_LEN + NIGHT_LEN;
/// How long, in seconds, the sky takes to finish transitioning across each
/// dawn/dusk boundary. Short enough that day and night both hold a stable
/// look most of the time; long enough that the transition itself is visible
/// rather than a single-frame snap.
const TRANSITION: f32 = 5.0;

/// Coin income per second of daylight, before any idle-folk bonus.
const BASE_INCOME: f32 = 0.9;
/// Extra coin income per second, per idle (unassigned) folk -- a reason to
/// recruit even once both walls are fully staffed.
const IDLE_INCOME: f32 = 0.15;

/// Coin cost to recruit one folk.
const RECRUIT_COST: u32 = 12;
/// Archer capacity added to a wall per level.
const ARCHERS_PER_LEVEL: u32 = 2;
/// Highest wall level.
const MAX_WALL_LEVEL: u8 = 3;

/// Ground speed of an approaching creature, world units per second.
const CREATURE_SPEED: f32 = 3.4;
/// Range from a wall's world position at which an approaching creature is
/// engaged and the night's combat resolves for it.
const ENGAGE_RANGE: f32 = 0.6;
/// Coins lost to camp stores on an unrepelled breach.
const BREACH_COIN_LOSS: u32 = 6;

/// One side of the camp. Every per-side array in [`WalledDawn`] is indexed by
/// [`Side::index`], so "the left wall" and "the right wall" are always the
/// same slot rather than two parallel fields that could drift apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Side {
    const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    /// -1 for the left wall, +1 for the right: the direction from the camp
    /// its wall and its spawn edge lie in.
    const fn sign(self) -> f32 {
        match self {
            Self::Left => -1.0,
            Self::Right => 1.0,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Left => "L",
            Self::Right => "R",
        }
    }
}

/// One creature marching in from a world edge toward the camp.
struct Creature {
    side: Side,
    x: f32,
    /// Decided at spawn time from the defending wall's *current* archer
    /// count, so a wall built up before this creature arrives can save it
    /// where an empty one would not. Deciding this once at spawn rather than
    /// re-rolling every frame keeps the outcome legible: a creature that
    /// reaches the wall was always going to live or die there, not have its
    /// fate flip while the player watches.
    repelled: bool,
}

/// What a tap or key press means, resolved by [`WalledDawn::layout`] into
/// hotspots and consumed by [`WalledDawn::apply_action`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Recruit,
    Build(Side),
}

/// State: the rider, the economy, both walls, every creature currently on
/// the ground, and the day/night clock that drives all of it.
pub struct WalledDawn {
    /// Total elapsed world time, driving idle animation (starfield twinkle,
    /// grass sway) that must never stop moving.
    time: f32,
    /// Position within the current day/night cycle, `0..CYCLE_LEN`.
    cycle_t: f32,
    /// Completed nights, shown in the status line and folded into the spawn
    /// hash so no two nights spawn an identical sequence.
    night_count: u32,
    player_x: f32,
    move_left: bool,
    move_right: bool,
    /// Fractional coin balance; the displayed amount is its floor, so income
    /// accrues continuously without ever showing a fractional coin.
    coin_balance: f32,
    idle_folk: u32,
    wall_level: [u8; 2],
    archers: [u32; 2],
    creatures: Vec<Creature>,
    /// Advances only on spawn and combat-resolution events, never per frame,
    /// so the exact sequence of spawns and outcomes depends solely on how
    /// many world-seconds have elapsed, matching the determinism the
    /// snapshot tests require.
    rng: Rng,
    spawn_timer: f32,
    kills_tonight: u32,
    breaches_tonight: u32,
    /// Set once at each dawn and held until the next; the panel a player
    /// glances at to see what last night cost.
    dawn_report: String,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for WalledDawn {
    fn default() -> Self {
        Self {
            time: 0.0,
            // The first cycle starts already most of the way through its
            // day rather than at dawn: a fresh viewer sees the night threat
            // this demo is about within seconds instead of watching a full
            // 42-second day tick down first. Every later cycle still runs
            // its full DAY_LEN, since this only shifts where the clock
            // starts, not how far apart wraps are.
            cycle_t: DAY_LEN - 9.0,
            night_count: 0,
            player_x: CAMP_X,
            move_left: false,
            move_right: false,
            coin_balance: 20.0,
            idle_folk: 0,
            wall_level: [1, 1],
            archers: [ARCHERS_PER_LEVEL, ARCHERS_PER_LEVEL],
            creatures: Vec::new(),
            rng: Rng::new(0x57A1_0A4E),
            spawn_timer: 4.0,
            kills_tonight: 0,
            breaches_tonight: 0,
            dawn_report: "Dusk is close. The first night has not fallen yet.".to_owned(),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl WalledDawn {
    /// This wall's world position.
    fn wall_x(side: Side) -> f32 {
        side.sign().mul_add(WALL_OFFSET, CAMP_X)
    }

    /// This side's build cost for its *next* level, or `None` if already at
    /// [`MAX_WALL_LEVEL`]. Rises with level so early walls are cheap and a
    /// maxed side is a real investment.
    fn build_cost(&self, side: Side) -> Option<u32> {
        let level = self.wall_level[side.index()];
        if level >= MAX_WALL_LEVEL {
            None
        } else {
            Some(20 + u32::from(level) * 16)
        }
    }

    /// Whether it is currently day (build/recruit hours) rather than night.
    const fn is_day(&self) -> bool {
        self.cycle_t < DAY_LEN
    }

    /// Seconds remaining in the current phase, for the countdown readout.
    fn phase_remaining(&self) -> f32 {
        if self.is_day() {
            DAY_LEN - self.cycle_t
        } else {
            CYCLE_LEN - self.cycle_t
        }
    }

    /// How much daylight is in the sky right now, `0.0` at the depth of
    /// night and `1.0` at the height of day, ramping linearly across
    /// [`TRANSITION`] at both the dusk and dawn boundary.
    ///
    /// A single continuous value rather than a boolean is what lets the sky,
    /// the sun/moon glyph, and the starfield's fade all read off one number
    /// instead of three separately-tuned transitions that could drift out of
    /// sync with each other.
    fn daylight(&self) -> f32 {
        let t = self.cycle_t;
        if t < DAY_LEN - TRANSITION {
            1.0
        } else if t < DAY_LEN {
            (DAY_LEN - t) / TRANSITION
        } else if t < CYCLE_LEN - TRANSITION {
            0.0
        } else {
            (t - (CYCLE_LEN - TRANSITION)) / TRANSITION
        }
    }

    /// Moves any idle folk onto whichever wall has spare archer capacity,
    /// preferring the side with fewer archers so both walls grow together
    /// rather than one being fully staffed while the other sits empty.
    fn assign_idle(&mut self) {
        while self.idle_folk > 0 {
            let capacity = |i: usize| u32::from(self.wall_level[i]) * ARCHERS_PER_LEVEL;
            let candidates: Vec<usize> =
                (0..2).filter(|&i| self.archers[i] < capacity(i)).collect();
            let Some(&pick) = candidates.iter().min_by_key(|&&i| self.archers[i]) else {
                break;
            };
            self.archers[pick] += 1;
            self.idle_folk -= 1;
        }
    }

    fn apply_action(&mut self, action: Action) {
        if !self.is_day() {
            return;
        }
        match action {
            Action::Recruit => {
                if self.coin_balance >= f32::from(u16::try_from(RECRUIT_COST).unwrap_or(u16::MAX)) {
                    self.coin_balance -= RECRUIT_COST as f32;
                    self.idle_folk += 1;
                    self.assign_idle();
                }
            }
            Action::Build(side) => {
                if let Some(cost) = self.build_cost(side)
                    && self.coin_balance >= cost as f32
                {
                    self.coin_balance -= cost as f32;
                    self.wall_level[side.index()] += 1;
                    self.assign_idle();
                }
            }
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            self.pointer.feed(&event);
            match event {
                Event::Close => return false,
                Event::Key(key) if key.is_down() => {
                    if ui::is_quit(&event) {
                        return false;
                    }
                    match key.code {
                        KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_left = true,
                        KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_right = true,
                        KeyCode::Char('f' | 'F') => self.apply_action(Action::Recruit),
                        KeyCode::Char('z' | 'Z') => self.apply_action(Action::Build(Side::Left)),
                        KeyCode::Char('x' | 'X') => self.apply_action(Action::Build(Side::Right)),
                        _ => {}
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Release => match key.code {
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_left = false,
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_right = false,
                    _ => {}
                },
                _ => {}
            }
        }
        true
    }

    /// Resolves this frame's pointer gesture against `self.hotspots`, built
    /// fresh during layout. A tap fires the button under it once; a held
    /// press walks the rider toward whichever side of its own screen
    /// position the press landed on, so "tap left/right of the rider to
    /// move" also covers "hold left/right of the rider to keep moving" --
    /// the natural touch analogue of holding an arrow key.
    fn handle_pointer(&mut self, rider_screen_x: u16) {
        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.apply_action(action);
        }
        self.move_left = false;
        self.move_right = false;
        if let Some(pos) = gesture.press
            && self.hotspots.hit(pos).is_none()
        {
            if pos.x < rider_screen_x {
                self.move_left = true;
            } else if pos.x > rider_screen_x {
                self.move_right = true;
            }
        }
    }

    fn simulate(&mut self, dt: f32) {
        self.time += dt;

        let dx = f32::from(u8::from(self.move_right)) - f32::from(u8::from(self.move_left));
        self.player_x = (dx.mul_add(PLAYER_SPEED * dt, self.player_x)).clamp(0.0, WORLD_LEN);

        let was_day = self.is_day();
        self.cycle_t += dt;
        if self.cycle_t >= CYCLE_LEN {
            self.cycle_t -= CYCLE_LEN;
        }
        let is_day = self.is_day();

        if was_day && is_day {
            // Still day: accrue income continuously.
            self.coin_balance = IDLE_INCOME
                .mul_add(self.idle_folk as f32, BASE_INCOME)
                .mul_add(dt, self.coin_balance);
        } else if was_day && !is_day {
            self.begin_night();
        } else if !was_day && is_day {
            self.end_night();
        }

        if !is_day {
            self.spawn_timer -= dt;
            if self.spawn_timer <= 0.0 {
                self.spawn_creature();
            }
        }

        self.advance_creatures(dt);
    }

    const fn begin_night(&mut self) {
        self.kills_tonight = 0;
        self.breaches_tonight = 0;
        self.spawn_timer = 2.5;
    }

    fn end_night(&mut self) {
        self.night_count += 1;
        self.creatures.clear();
        let total_archers = self.archers[0] + self.archers[1];
        self.dawn_report = format!(
            "Night {} held: {} slain, {} breach(es), {} archers standing.",
            self.night_count, self.kills_tonight, self.breaches_tonight, total_archers
        );
    }

    fn spawn_creature(&mut self) {
        let side = if self.rng.next_f32() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let x = if side == Side::Left { 0.0 } else { WORLD_LEN };
        let archers = self.archers[side.index()] as f32;
        // A roll weighted by the defending wall's archer count: an empty
        // wall almost always fails to repel, a heavily staffed one almost
        // always succeeds, and the curve in between is what makes recruiting
        // and building visibly matter rather than being a binary switch.
        let odds = archers / (archers + 2.0);
        let repelled = self.rng.next_f32() < odds;
        self.creatures.push(Creature { side, x, repelled });

        // The interval shortens slightly with each completed night (a wave
        // gets busier), floored so early nights are never overwhelmed and
        // there is always a moment of calm between arrivals.
        let base = (self.night_count as f32).mul_add(-0.15, 3.6).max(1.2);
        self.spawn_timer = base * self.rng.next_f32().mul_add(0.8, 0.6);
    }

    fn advance_creatures(&mut self, dt: f32) {
        let mut resolved = Vec::new();
        for (i, creature) in self.creatures.iter_mut().enumerate() {
            let wall_x = Self::wall_x(creature.side);
            let toward_camp = if creature.x < wall_x { 1.0 } else { -1.0 };
            creature.x = (toward_camp * CREATURE_SPEED).mul_add(dt, creature.x);
            if (creature.x - wall_x).abs() <= ENGAGE_RANGE {
                resolved.push(i);
            }
        }
        // Removed back to front so earlier indices stay valid as later ones
        // are dropped.
        for &i in resolved.iter().rev() {
            let creature = self.creatures.remove(i);
            if creature.repelled {
                self.kills_tonight += 1;
            } else {
                self.breaches_tonight += 1;
                self.coin_balance = (self.coin_balance - BREACH_COIN_LOSS as f32).max(0.0);
                let idx = creature.side.index();
                self.archers[idx] = self.archers[idx].saturating_sub(1);
            }
        }
    }

    fn status_line(&self) -> String {
        let phase = if self.is_day() { "day" } else { "night" };
        format!(
            "night {}  {phase} {:>2}s  coins {}  idle {}  archers L{}/R{}",
            self.night_count,
            self.phase_remaining().ceil() as u32,
            self.coin_balance as u32,
            self.idle_folk,
            self.archers[0],
            self.archers[1],
        )
    }

    // ---- layout ---------------------------------------------------------

    /// Splits `content` into the info strip, the world viewport, and the
    /// bottom thumb-zone control bar, sized so every button in the bar meets
    /// [`ui::touch::TAP_W`]x[`ui::touch::TAP_H`] at every [`Shape`] this
    /// gallery targets.
    fn layout(content: Rect) -> (Rect, Rect, Rect) {
        let shape = Shape::of(content);
        let info_h = if shape == Shape::Landscape { 1 } else { 2 };
        let bar_h = ui::touch::TAP_H.min(content.height().saturating_sub(info_h + 3));
        let bar_h = bar_h.max(1);
        let info_h = info_h.min(content.height());
        let world_h = content
            .height()
            .saturating_sub(info_h)
            .saturating_sub(bar_h);

        let info = Rect::new(content.left(), content.top(), content.width(), info_h);
        let world = Rect::new(
            content.left(),
            content.top() + info_h,
            content.width(),
            world_h,
        );
        let bar = Rect::new(
            content.left(),
            content.top() + info_h + world_h,
            content.width(),
            bar_h,
        );
        (info, world, bar)
    }

    /// The camera's left edge, in world units: the rider centred in the
    /// viewport, clamped so the world's own edges are never drawn past.
    fn camera_x(&self, world_w: u16) -> f32 {
        let half = f32::from(world_w) / 2.0;
        let max_camera = (WORLD_LEN - f32::from(world_w)).max(0.0);
        (self.player_x - half).clamp(0.0, max_camera)
    }

    fn draw_info(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        ui::fill(surface, area, Style::new().bg(ui::CHROME_BG));
        let phase_glyph = if self.is_day() {
            '\u{263c}'
        } else {
            '\u{25cb}'
        };
        let phase_word = if self.is_day() { "DAY" } else { "NIGHT" };
        let left = format!(
            "{phase_glyph} {phase_word} {:>2}s  night {}",
            self.phase_remaining().ceil() as u32,
            self.night_count
        );
        surface.print(
            (area.left() + 1, area.top()),
            &left,
            Style::new().fg(ui::ACCENT).bg(ui::CHROME_BG),
        );
        let right = format!(
            "coins {}  idle {}  archers L{}/R{}",
            self.coin_balance as u32, self.idle_folk, self.archers[0], self.archers[1],
        );
        let rw = right.chars().count() as u16;
        if area.width() > rw + 2 {
            surface.print(
                (area.right() - rw - 1, area.top()),
                &right,
                Style::new().fg(ui::FG).bg(ui::CHROME_BG),
            );
        }
        if area.height() > 1 {
            surface.print(
                (area.left() + 1, area.top() + 1),
                &self.dawn_report,
                Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
            );
        }
    }

    /// Draws the bottom thumb-zone bar and (re)registers its hotspots.
    /// Called during layout, before the pointer gesture for this frame is
    /// resolved, so a tap is always checked against the buttons drawn in the
    /// same frame it landed on.
    fn draw_controls(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.hotspots.clear();
        if area.height() == 0 || area.width() < ui::touch::TAP_W * 3 {
            // Too narrow for three legal touch targets side by side: still
            // usable from the keyboard, so only the tap surface is skipped.
            ui::fill(surface, area, Style::new().bg(ui::CHROME_BG));
            return;
        }
        ui::fill(surface, area, Style::new().bg(ui::CHROME_BG));

        let slots = ui::panel::columns(area, 3, 1);
        let day = self.is_day();

        let left_label = self.build_cost(Side::Left).map_or_else(
            || format!("Wall {} MAX", Side::Left.label()),
            |cost| {
                format!(
                    "Wall {}>{} ({cost}c)",
                    Side::Left.label(),
                    self.wall_level[0] + 1
                )
            },
        );
        let recruit_label = format!("Recruit +1 ({RECRUIT_COST}c)");
        let right_label = self.build_cost(Side::Right).map_or_else(
            || format!("Wall {} MAX", Side::Right.label()),
            |cost| {
                format!(
                    "Wall {}>{} ({cost}c)",
                    Side::Right.label(),
                    self.wall_level[1] + 1
                )
            },
        );

        self.draw_button(
            surface,
            slots[0],
            &left_label,
            day,
            Action::Build(Side::Left),
        );
        self.draw_button(surface, slots[1], &recruit_label, day, Action::Recruit);
        self.draw_button(
            surface,
            slots[2],
            &right_label,
            day,
            Action::Build(Side::Right),
        );

        if !day {
            let note = "-- building resumes at dawn --";
            if area.width() as usize > note.len() + 2 {
                surface.print(
                    (
                        area.left() + (area.width() - note.len() as u16) / 2,
                        area.top(),
                    ),
                    note,
                    Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
                );
            }
        }
    }

    fn draw_button(
        &mut self,
        surface: &mut Surface<'_>,
        rect: Rect,
        label: &str,
        enabled: bool,
        action: Action,
    ) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let frame = if enabled { ui::ACCENT } else { ui::DIM };
        let bg = rgb(24, 26, 36);
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        // A plain single-line frame rather than `panel::Panel`: the panel
        // border eats two rows on every edge, which a `TAP_H` = 4 button
        // cannot spare. Top and bottom rules are enough to read as a button.
        for x in rect.left()..rect.right() {
            surface.put((x, rect.top()), '\u{2500}', Style::new().fg(frame).bg(bg));
            surface.put(
                (x, rect.bottom() - 1),
                '\u{2500}',
                Style::new().fg(frame).bg(bg),
            );
        }
        let text_y = rect.top() + rect.height() / 2;
        let text_x = rect.left() + 1;
        let room = rect.width().saturating_sub(2) as usize;
        let text: String = if label.chars().count() > room {
            label
                .chars()
                .take(room.saturating_sub(1))
                .chain(['.'])
                .collect()
        } else {
            label.to_owned()
        };
        surface.print(
            (text_x, text_y),
            &text,
            Style::new()
                .fg(if enabled { ui::FG } else { ui::DIM })
                .bg(bg),
        );
        if enabled {
            self.hotspots.push_tappable(rect, rect, action);
        }
    }

    // ---- world drawing ----------------------------------------------------

    /// Draws the whole side-on world: sky, far ridge, treeline, ground with
    /// its entities, and foreground grass, back to front.
    fn draw_world(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() < 3 {
            return;
        }
        let camera_x = self.camera_x(area.width());
        // The horizon sits close to the middle rather than low: the far
        // ridge and treeline now reach tall enough to spend most of the sky
        // band, and the ground carries its own parallax (path, strata,
        // rushing foreground grass) rather than being a flat colour slab, so
        // neither side of the horizon is left to read as empty.
        let horizon = (f32::from(area.height()) * 0.56) as u16;
        let horizon = horizon.clamp(2, area.height().saturating_sub(2));

        let daylight = self.daylight();
        self.draw_sky(surface, area, horizon, camera_x, daylight);
        Self::draw_far_ridge(surface, area, horizon, camera_x, daylight);
        Self::draw_treeline(surface, area, horizon, camera_x, daylight);
        self.draw_ground(surface, area, horizon, camera_x, daylight);
        self.draw_midground_grass(surface, area, horizon, camera_x, daylight);
        self.draw_foreground_grass(surface, area, horizon, camera_x);
    }

    fn draw_sky(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let day_top = rgb(88, 138, 206);
        let day_horizon = rgb(198, 212, 232);
        let night_top = rgb(8, 10, 26);
        let night_horizon = rgb(46, 34, 64);

        for y in 0..horizon {
            let row_t = f32::from(y) / f32::from(horizon.max(1));
            let day_col = mix(day_top, day_horizon, row_t);
            let night_col = mix(night_top, night_horizon, row_t);
            let sky = mix(night_col, day_col, daylight);
            for x in 0..area.width() {
                surface.put((area.left() + x, area.top() + y), ' ', Style::new().bg(sky));
            }
        }

        self.draw_clouds(surface, area, horizon, camera_x, daylight);
        self.draw_stars(surface, area, horizon, daylight);
        self.draw_sun_moon(surface, area, horizon, daylight);
        self.draw_birds(surface, area, horizon, daylight);
    }

    /// Stable star positions from a coordinate hash (never reshuffle),
    /// brightness driven by `self.time` (always animates), visibility gated
    /// by how dark the sky currently is.
    fn draw_stars(&self, surface: &mut Surface<'_>, area: Rect, horizon: u16, daylight: f32) {
        if daylight > 0.85 {
            return;
        }
        let opacity = 1.0 - daylight;
        for y in 0..horizon {
            for x in 0..area.width() {
                let (wx, wy) = (i32::from(x), i32::from(y));
                if hash01(0x51A2, wx, wy) > 0.045 {
                    continue;
                }
                let phase = hash01(0x1E2F, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add((self.time.mul_add(0.8, phase)).sin(), 0.5);
                let v = (140.0 * twinkle * opacity) as u8 + 40;
                surface.put(
                    (area.left() + x, area.top() + y),
                    '.',
                    Style::new().fg(rgb(v, v, v.saturating_add(15))),
                );
            }
        }
    }

    /// Drifting cloud cover, its own parallax layer at factor 0.08 (even
    /// slower than the far ridge) plus a constant wind drift from
    /// `self.time`, so clouds keep moving even while the camera and rider
    /// stand still. Confined to the upper part of the sky so it never
    /// competes with the sun/moon arc or the ridgeline silhouette below it.
    fn draw_clouds(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let band_h = (f32::from(horizon) * 0.6) as u16;
        if band_h == 0 {
            return;
        }
        let day_cloud = rgb(246, 248, 252);
        let night_cloud = rgb(52, 56, 76);
        let color = mix(night_cloud, day_cloud, daylight);
        let wind = self.time * 1.1;
        for y in 0..band_h {
            for x in 0..area.width() {
                let layer_x = camera_x.mul_add(0.08, f32::from(x) + wind);
                let n = value_noise(0x4B21, layer_x * 0.05, f32::from(y) * 0.4);
                if n < 0.63 {
                    continue;
                }
                let glyph = if n > 0.8 { '\u{2588}' } else { '\u{2591}' };
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(color),
                );
            }
        }
    }

    /// A handful of birds crossing the daytime sky, screen-space rather than
    /// world-space: they are weather, not landmarks, so they do not need
    /// world coordinates or to survive being paused off-screen. Alternating
    /// glyphs on `self.time` read as a wingbeat without a sprite sheet.
    fn draw_birds(&self, surface: &mut Surface<'_>, area: Rect, horizon: u16, daylight: f32) {
        const BIRD_COUNT: u32 = 4;
        if daylight < 0.55 || area.width() == 0 {
            return;
        }
        let color = rgb(30, 30, 40);
        for i in 0..BIRD_COUNT {
            let fi = i as f32;
            let speed = fi.mul_add(1.4, 5.0);
            let offset = fi * 37.0;
            let x = self
                .time
                .mul_add(speed, offset)
                .rem_euclid(f32::from(area.width()));
            let y = (f32::from(horizon) * 0.12f32.mul_add(fi, 0.1)) as u16;
            if y >= horizon {
                continue;
            }
            let flap = (self.time.mul_add(6.0, offset)).sin();
            let glyph = if flap > 0.0 { 'v' } else { '^' };
            surface.put(
                (area.left() + x as u16, area.top() + y),
                glyph,
                Style::new().fg(color),
            );
        }
    }

    /// The sun during the day, the moon during the night, arcing left to
    /// right across the sky on the current phase's own progress.
    fn draw_sun_moon(&self, surface: &mut Surface<'_>, area: Rect, horizon: u16, daylight: f32) {
        let (t, glyph, color) = if self.is_day() {
            (self.cycle_t / DAY_LEN, '\u{263c}', rgb(250, 214, 120))
        } else {
            (
                (self.cycle_t - DAY_LEN) / NIGHT_LEN,
                '\u{25cb}',
                rgb(224, 224, 236),
            )
        };
        let t = t.clamp(0.0, 1.0);
        if daylight <= 0.02 && self.is_day() {
            return;
        }
        let angle = t * core::f32::consts::PI;
        let arc_h = f32::from(horizon) * 0.85;
        let sx = t * f32::from(area.width().saturating_sub(1));
        let sy = angle.sin().mul_add(-arc_h, f32::from(horizon)) - 1.0;
        if sy < 0.0 || sy >= f32::from(horizon) {
            return;
        }
        surface.put(
            (area.left() + sx as u16, area.top() + sy as u16),
            glyph,
            Style::new().fg(color),
        );
    }

    /// Far mountain silhouette, parallax factor 0.15: the slowest layer,
    /// scrolling only a sixth as fast as the ground the player actually
    /// walks on.
    fn draw_far_ridge(
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let color = mix(rgb(18, 22, 38), rgb(96, 118, 150), daylight * 0.7);
        for x in 0..area.width() {
            let layer_x = camera_x.mul_add(0.15, f32::from(x));
            let n = value_noise(0x9F31, layer_x * 0.05, 0.0);
            // Tall enough to spend a real share of the sky band rather than
            // a thin fringe along the horizon: this is the layer that turns
            // an otherwise-empty gradient into a skyline.
            let height = (n * 15.0 + 3.0) as u16;
            let top = horizon.saturating_sub(height);
            for y in top..horizon {
                surface.put(
                    (area.left() + x, area.top() + y),
                    '\u{2588}',
                    Style::new().fg(color),
                );
            }
        }
    }

    /// Mid treeline, parallax factor 0.42: faster than the ridge, slower than
    /// the ground, so it visibly slides between the two as the camera moves.
    fn draw_treeline(
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let canopy = mix(rgb(10, 22, 16), rgb(58, 108, 60), daylight);
        let trunk = mix(rgb(6, 14, 10), rgb(34, 64, 38), daylight);
        for x in 0..area.width() {
            let layer_x = camera_x.mul_add(0.42, f32::from(x));
            let n = value_noise(0x2C7B, layer_x * 0.09, 0.0);
            let height = (n * 9.0 + 2.0) as u16;
            let top = horizon.saturating_sub(height);
            for y in top..horizon {
                let is_canopy = y == top;
                let ix = layer_x.floor() as i32;
                let glyph = if is_canopy {
                    if hash01(0x774A, ix, 0) > 0.5 {
                        '\u{2660}'
                    } else {
                        '\u{2588}'
                    }
                } else {
                    '\u{2588}'
                };
                let col = if is_canopy { canopy } else { trunk };
                surface.put(
                    (area.left() + x, area.top() + y),
                    glyph,
                    Style::new().fg(col),
                );
            }
        }
    }

    /// The true ground layer, parallax factor 1.0: real world coordinates,
    /// where the camp, both walls, the rider, and every creature live.
    fn draw_ground(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let grass = mix(rgb(20, 16, 14), rgb(58, 96, 44), daylight);
        let dirt = mix(rgb(10, 8, 8), rgb(74, 54, 34), daylight);
        let rock_a = rgb(46, 40, 46);
        let rock_b = rgb(70, 58, 56);
        let span = f32::from((area.height() - horizon).max(1));
        for y in horizon..area.height() {
            let depth_t = f32::from(y - horizon) / span;
            for x in 0..area.width() {
                let wx = (camera_x + f32::from(x)).floor() as i32;
                let col = if depth_t < 0.35 {
                    // Topsoil: the grass the rider actually rides over,
                    // flecked with the odd pebble or clump.
                    let base = mix(grass, dirt, depth_t / 0.35);
                    let fleck = hash01(0x3A11, wx, i32::from(y)) < 0.08;
                    if fleck { mix(base, dirt, 0.4) } else { base }
                } else {
                    // Below the topsoil: rock strata, banded horizontally
                    // with a little per-column jitter so the bands read as
                    // sediment rather than a perfectly ruled grid.
                    let jitter = hash01(0x7C44, wx / 3, i32::from(y)) * 1.6;
                    let band = ((depth_t * 9.0 + jitter) as i32).rem_euclid(2);
                    let strata = if band == 0 { rock_a } else { rock_b };
                    mix(strata, dirt, (0.5 - depth_t).clamp(0.0, 0.3))
                };
                surface.put((area.left() + x, area.top() + y), ' ', Style::new().bg(col));
            }
        }
        Self::draw_path(surface, area, horizon, camera_x, daylight);

        Self::draw_camp(surface, area, horizon, camera_x);
        self.draw_wall(surface, area, horizon, camera_x, Side::Left);
        self.draw_wall(surface, area, horizon, camera_x, Side::Right);
        for creature in &self.creatures {
            Self::draw_creature(surface, area, horizon, camera_x, creature);
        }
        self.draw_rider(surface, area, horizon, camera_x);
    }

    /// Converts a world x-coordinate to a screen column, or `None` if it
    /// falls outside `area` at the ground layer's parallax factor of 1.0.
    fn world_to_screen(area: Rect, camera_x: f32, world_x: f32) -> Option<u16> {
        let sx = world_x - camera_x;
        if sx < 0.0 || sx >= f32::from(area.width()) {
            None
        } else {
            Some(area.left() + sx as u16)
        }
    }

    /// A worn track along the top of the topsoil: a lighter, thinner strip
    /// than the grass either side of it, so the ground reads as somewhere
    /// the rider is riding rather than a field it happens to be crossing.
    fn draw_path(
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        let track = mix(rgb(24, 18, 12), rgb(150, 120, 78), daylight);
        for x in 0..area.width() {
            let wx = (camera_x + f32::from(x)).floor() as i32;
            let wobble = value_noise(0x9A02, wx as f32 * 0.06, 0.0);
            let row = horizon + (wobble * 2.0) as u16;
            if row < area.height() && hash01(0xB013, wx, 0) > 0.12 {
                surface.put(
                    (area.left() + x, area.top() + row),
                    ' ',
                    Style::new().bg(track),
                );
            }
        }
    }

    fn draw_camp(surface: &mut Surface<'_>, area: Rect, horizon: u16, camera_x: f32) {
        let Some(cx) = Self::world_to_screen(area, camera_x, CAMP_X) else {
            return;
        };
        let roof = rgb(150, 96, 60);
        let wall = rgb(198, 168, 120);
        let base = horizon;
        // Two storeys of wall beneath the roof rather than one: a camp the
        // rider can see from across the world instead of a single dark
        // pixel sitting on the horizon.
        if base >= 2 && cx > 0 && cx + 1 < area.right() {
            surface.put((cx - 1, base - 2), '/', Style::new().fg(roof));
            surface.put((cx, base - 3), '\u{252c}', Style::new().fg(roof));
            surface.put((cx + 1, base - 2), '\\', Style::new().fg(roof));
            surface.put((cx, base - 2), '\u{2588}', Style::new().fg(wall));
            surface.put((cx - 1, base - 1), '\u{2588}', Style::new().fg(wall));
            surface.put((cx, base - 1), '\u{2588}', Style::new().fg(wall));
            surface.put((cx + 1, base - 1), '\u{2588}', Style::new().fg(wall));
        }
    }

    fn draw_wall(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        side: Side,
    ) {
        let Some(cx) = Self::world_to_screen(area, camera_x, Self::wall_x(side)) else {
            return;
        };
        let level = self.wall_level[side.index()];
        let archers = self.archers[side.index()];
        // Three columns wide rather than one: a fortification the player is
        // meant to read as a building, not as a single decorative pixel lost
        // among the treeline behind it. The wall's own footprint on the
        // ground never moves; only its height and the archer count on top
        // change as it is upgraded.
        let lo = cx.saturating_sub(1).max(area.left());
        let hi = (cx + 1).min(area.right().saturating_sub(1));
        let stone = rgb(172, 172, 184);
        let shadow = rgb(96, 96, 110);
        // Three rows of rampart per level: tall enough that upgrading the
        // wall is visible from across the screen, not just in the status
        // line, and tall enough to hold its own against a bigger sky/ground
        // band either side of it.
        let rows = u16::from(level) * 3;
        for r in 0..rows {
            let y = horizon.saturating_sub(1 + r);
            let is_base = r + 1 == rows;
            for x in lo..=hi {
                let glyph = if is_base { '\u{2584}' } else { '\u{2588}' };
                let color = if x == cx { stone } else { shadow };
                surface.put((x, y), glyph, Style::new().fg(color));
            }
        }
        if rows == 0 {
            return;
        }
        // A crenellated parapet line on top, then one archer glyph per
        // stationed defender (capped at the three slots the wall's width
        // offers), so a fully staffed wall visibly bristles with archers and
        // an empty one shows bare crenellations instead.
        let parapet = horizon.saturating_sub(rows);
        for (i, x) in (lo..=hi).enumerate() {
            let merlon = i % 2 == 0;
            let glyph = if merlon { '\u{2588}' } else { ' ' };
            surface.put((x, parapet), glyph, Style::new().fg(stone));
        }
        let archer_row = parapet.saturating_sub(1);
        for (i, x) in (lo..=hi).enumerate() {
            if (i as u32) < archers.min(3) {
                surface.put(
                    (x, archer_row),
                    '\u{263a}',
                    Style::new().fg(rgb(224, 200, 150)),
                );
            }
        }
    }

    /// The rider on their mount: three rows tall so the gallery's one
    /// entity, many cells premise actually shows here, not just a single
    /// pixel lost between the walls either side of it.
    fn draw_rider(&self, surface: &mut Surface<'_>, area: Rect, horizon: u16, camera_x: f32) {
        let Some(cx) = Self::world_to_screen(area, camera_x, self.player_x) else {
            return;
        };
        let hide = rgb(150, 104, 60);
        let rider_col = rgb(226, 176, 96);
        if horizon < 3 {
            // Not enough headroom for the full mount at this viewport
            // height: fall back to a single glyph rather than clipping.
            if horizon >= 1 {
                surface.put((cx, horizon - 1), 'M', Style::new().fg(rider_col));
            }
            return;
        }
        let lo = cx.saturating_sub(1).max(area.left());
        let hi = (cx + 1).min(area.right().saturating_sub(1));
        for x in lo..=hi {
            surface.put((x, horizon - 1), '\u{2584}', Style::new().fg(hide));
        }
        surface.put((cx, horizon - 2), 'M', Style::new().fg(rider_col));
        if cx < hi {
            surface.put((cx + 1, horizon - 2), '\u{2510}', Style::new().fg(hide));
        }
        surface.put((cx, horizon - 3), 'o', Style::new().fg(rider_col));
    }

    fn draw_creature(
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        creature: &Creature,
    ) {
        let Some(cx) = Self::world_to_screen(area, camera_x, creature.x) else {
            return;
        };
        if horizon == 0 {
            return;
        }
        surface.put((cx, horizon - 1), 'w', Style::new().fg(rgb(196, 70, 70)));
    }

    /// Midground grass, parallax factor 1.15: between the true ground (1.0)
    /// and the foreground strip (1.3), one row further from the camera than
    /// the foreground so the two visibly slide past each other rather than
    /// scrolling as a single sheet.
    fn draw_midground_grass(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
        daylight: f32,
    ) {
        if area.height() < 2 || horizon >= area.height() - 1 {
            return;
        }
        let row = area.height() - 2;
        let color = mix(rgb(24, 22, 14), rgb(52, 100, 48), daylight);
        for x in 0..area.width() {
            let layer_x = camera_x.mul_add(1.15, f32::from(x));
            let ix = layer_x.floor() as i32;
            if hash01(0x5D19, ix, 0) > 0.4 {
                continue;
            }
            let sway = hash01(0x5D19, ix, 1) * core::f32::consts::TAU;
            let glyph = if (self.time.mul_add(1.1, sway)).sin() > 0.0 {
                '\''
            } else {
                ','
            };
            surface.put(
                (area.left() + x, area.top() + row),
                glyph,
                Style::new().fg(color),
            );
        }
    }

    /// Foreground grass, parallax factor 1.3: the fastest layer, drawn last
    /// so it overlaps the very front of the ground plane, the signature
    /// "rushing past the camera" strip that reads unmistakably as *nearer*
    /// than everything behind it.
    fn draw_foreground_grass(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        horizon: u16,
        camera_x: f32,
    ) {
        if horizon == 0 || horizon >= area.height() {
            return;
        }
        let row = area.height() - 1;
        let color = rgb(70, 130, 58);
        for x in 0..area.width() {
            let layer_x = camera_x.mul_add(1.3, f32::from(x));
            let ix = layer_x.floor() as i32;
            if hash01(0x6E02, ix, 0) > 0.55 {
                continue;
            }
            let sway = hash01(0x6E02, ix, 1) * core::f32::consts::TAU;
            let glyph = if (self.time.mul_add(1.4, sway)).sin() > 0.0 {
                '\''
            } else {
                ','
            };
            surface.put(
                (area.left() + x, area.top() + row),
                glyph,
                Style::new().fg(color),
            );
        }
    }
}

impl Demo for WalledDawn {
    const NAME: &'static str = "54_walled_dawn";
    const TITLE: &'static str = "Walled Dawn";
    const BLURB: &'static str =
        "Kingdom: a side-on world scrolled one axis, where night arrives on a timer.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Left/A", "ride left"),
            ("Right/D", "ride right"),
            ("F", "recruit folk"),
            ("Z / X", "build left / right wall"),
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

        let (info_area, world_area, bar_area) = Self::layout(content);
        self.draw_world(&mut surface, world_area);
        self.draw_info(&mut surface, info_area);
        self.draw_controls(&mut surface, bar_area);

        let camera_x = self.camera_x(world_area.width());
        let rider_screen_x = Self::world_to_screen(world_area, camera_x, self.player_x)
            .unwrap_or_else(|| world_area.left() + world_area.width() / 2);
        self.handle_pointer(rider_screen_x);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(WalledDawn);
