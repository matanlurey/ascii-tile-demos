//! 27: Rhythm crypt -- Crypt of the `NecroDancer`'s beat-locked movement,
//! adapted to a character grid and a touchscreen.
//!
//! Every earlier demo in this gallery treats input as instantaneous: press a
//! key, the thing happens. A rhythm game breaks that assumption on purpose.
//! Here every action -- step, bump-attack, dodge -- is legal at any moment,
//! but it is only *rewarded* (a rising combo multiplier, a flash on the beat
//! track) if it lands inside a window around the beat. That is also exactly
//! why this is the hardest demo in the batch for touch: a mouse click and a
//! key press both resolve in under the eye's own reaction time, but a finger
//! has to travel to a target first, and on a phone that travel time can
//! itself be larger than the timing window it is trying to hit. Two things
//! compensate for that here, and both are load-bearing rather than
//! decorative:
//!
//! 1. The [D-pad](Self::draw_dpad) is oversized and fixed in the same screen
//!    location every frame (see [`ui::touch::TAP_W`]/[`TAP_H`](ui::touch::TAP_H)).
//!    A finger that has already found "down" once can find it again without
//!    looking, the same way a real controller's D-pad works by feel. A
//!    precisely-placed but small target would ask the player to re-aim under
//!    time pressure, which is the one thing a rhythm game cannot afford to ask.
//! 2. The timing window is drawn on the beat track itself
//!    ([`draw_beat_track`](Self::draw_beat_track)), not implied by a sound cue
//!    or hidden in the rules text. Sound is exactly the channel a touch
//!    device is least likely to have the volume up for (silenced phones,
//!    browser autoplay restrictions), so the window has to be legible from
//!    pixels alone.
//!
//! The beat itself is driven by [`frame.delta`](retroglyph_core::Frame::delta)
//! accumulated into a single `f32` clock ([`RhythmCrypt::simulate`]), never by
//! counting ticks. This is not a style preference: the harness runs this same
//! code on a 60 Hz terminal poll loop, an uncapped native window, and a
//! browser `requestAnimationFrame` callback, and those deliver wildly
//! different numbers of `tick` calls per wall-clock second. A tick-counted
//! beat would run at three different tempos on three different backends; a
//! delta-accumulated one runs at 140 BPM on all of them, because the clock
//! only ever asks "how much real time passed", never "how many frames have
//! there been".
//!
//! The dungeon floor is drawn at [`TILE_W_MAX`]x[`TILE_H_MAX`] cells per tile
//! (never less than the 5x3 floor the brief sets, though very small viewports
//! may be clipped rather than shrunk below it -- see [`draw_board`]). A tile
//! this large is what lets three unrelated things share one grid cell without
//! colliding: [`draw_floor_tile`] paints the stone-and-mortar slab across the
//! *whole* footprint, [`draw_wall_tile`] optionally lays a flickering torch
//! over a couple of cells of a wall tile without touching the rest of its
//! coursing, and [`draw_occupant`] then overlays a player or enemy figure only
//! on the handful of cells its own art actually uses, leaving the mortar
//! lines around it showing through. At one glyph per tile none of that would
//! be representable at once: the floor pattern, the torch, and the occupant
//! would all be fighting for the same single cell, and something would have
//! to be dropped. At interface scale nothing has to be.
//!
//! Techniques on show:
//!
//! - **A delta-accumulated beat clock** ([`RhythmCrypt::simulate`],
//!   [`RhythmCrypt::beat_phase`]): see above.
//! - **A visible timing window** ([`draw_beat_track`](Self::draw_beat_track)):
//!   drawn at both ends of the bar, because the window wraps -- the instant a
//!   beat lands is position 0 *and* position 1 of the same cycle.
//! - **Deterministic, legible enemy cadences** ([`EnemyKind::acts_on`]): a
//!   slime that steps every second beat and a skeleton that steps on the
//!   beats the slime doesn't, so together they read as two independently
//!   timed actors rather than one enemy AI reused twice.
//! - **Chunky multi-cell floor tiles with mortar lines** ([`draw_floor_tile`],
//!   [`draw_wall_tile`]): see above.
//! - **Multi-cell occupant figures** ([`draw_occupant`], [`PLAYER_ART`],
//!   [`SLIME_ART`], [`SKELETON_ART`]): two rows of glyphs rather than one, so
//!   a player and an enemy standing on adjacent tiles are shapes, not just
//!   differently-colored dots.
//! - **A fixed, oversized D-pad plus tap-adjacent-tile plus swipe-anywhere**
//!   ([`RhythmCrypt::handle_events`], [`Action`]): three ways to issue the
//!   same move, because "tap a target near the thing you want" and "swipe in
//!   the direction you want" suit different situations (see the mobile-first
//!   rules on `push_tappable`/two-tap-vs-drag in [`ui::touch`]) and a rhythm
//!   game cannot afford to make the player hunt for the one that works.
//! - **Multi-cell hearts and a framed equipment strip**
//!   ([`draw_hearts`], [`EQUIPMENT`]): the game's HP and loadout status,
//!   always visible (never hover-gated), because a beat-locked game gives the
//!   player no time to go looking for information mid-action.
//!
//! ```sh
//! cargo run --example 27_rhythm_crypt --features crossterm
//! cargo run --example 27_rhythm_crypt --features software
//! cargo run --example 27_rhythm_crypt --features gl
//! cargo run --example 27_rhythm_crypt  # headless, prints a few frames
//! ```

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, panel, touch};
use ascii_tile_demos::util::perf::FpsMeter;
use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb};

/// Tempo the whole demo runs at. Crypt of the `NecroDancer`'s own reference
/// tracks mostly sit in the 130-150 BPM range; 140 lands a beat every ~429ms,
/// fast enough that the mechanic reads as a rhythm game rather than a slow
/// turn timer, slow enough that a first-time player can still find the
/// window without music playing.
const BPM: f32 = 140.0;

/// Seconds per beat. See [`BPM`].
const BEAT_PERIOD: f32 = 60.0 / BPM;

/// Half-width of the "on beat" window, as a fraction of one beat cycle.
///
/// Generous by rhythm-game standards (`NecroDancer`'s own window is much
/// tighter) on purpose: this demo has no audio track to cue against, only
/// the visible track, so the window has to be wide enough to hit by eye
/// alone against a silent, possibly-laggy touch input path.
const BEAT_WINDOW: f32 = 0.16;

/// Interior floor tiles, not counting the surrounding wall ring.
const BOARD_W: i32 = 6;
/// See [`BOARD_W`].
const BOARD_H: i32 = 4;

/// Ceiling on one tile's screen footprint, in cells.
///
/// Capped rather than left to grow with the viewport: past roughly this size
/// a tile's mortar cross and occupant figure start floating in a sea of
/// empty stone, which reads as sparse rather than chunky. See [`TILE_H_MAX`].
const TILE_W_MAX: u16 = 9;
/// See [`TILE_W_MAX`].
const TILE_H_MAX: u16 = 5;

/// Player starting hit points, drawn as this many multi-cell hearts.
const MAX_HP: i32 = 3;

/// Wall tiles (in absolute, border-inclusive grid coordinates) that carry a
/// torch. Placed to flank both the top and bottom wall so light reaches every
/// corner of the room, the same spread-out-sconces reasoning `24_torchlit_
/// crypt` uses.
const TORCH_TILES: [(i32, i32); 4] = [
    (2, 0),
    (BOARD_W - 1, 0),
    (2, BOARD_H + 1),
    (BOARD_W - 1, BOARD_H + 1),
];

/// Equipment HUD slots: a glyph and a name each. Cosmetic (nothing here is
/// tappable), but always visible per the mobile-first "no hover-only
/// information" rule -- a beat-locked game gives the player no spare moment
/// to go hunting for their own loadout.
const EQUIPMENT: [(char, &str); 3] = [('\u{ac}', "Shovel"), ('/', "Blade"), ('!', "Torch")];

/// A player figure: two rows, three columns. Space cells stay transparent so
/// the floor's mortar lines show through around the figure instead of being
/// erased by a solid block; see the module docs on why that matters.
const PLAYER_ART: [&str; 2] = [" \u{263a} ", "/\u{2588}\\"];
/// A slime: low and round, to read as a different silhouette from the
/// skeleton at a glance even before the color registers.
const SLIME_ART: [&str; 2] = [" _ ", "(_)"];
/// A skeleton: taller and angular.
const SKELETON_ART: [&str; 2] = [" ^ ", "/|\\"];

/// The four movement directions, shared by keyboard, D-pad taps, tile taps,
/// and swipes -- one action vocabulary, four ways to reach it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    const fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }

    const fn arrow(self) -> char {
        match self {
            Self::Up => '\u{2191}',
            Self::Down => '\u{2193}',
            Self::Left => '\u{2190}',
            Self::Right => '\u{2192}',
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

/// Maps a key to a movement direction. WASD and the arrow keys both work, so
/// keyboard parity holds regardless of which hand is on the board.
const fn key_dir(code: KeyCode) -> Option<Dir> {
    match code {
        KeyCode::Up | KeyCode::Char('w' | 'W') => Some(Dir::Up),
        KeyCode::Down | KeyCode::Char('s' | 'S') => Some(Dir::Down),
        KeyCode::Left | KeyCode::Char('a' | 'A') => Some(Dir::Left),
        KeyCode::Right | KeyCode::Char('d' | 'D') => Some(Dir::Right),
        _ => None,
    }
}

/// Picks the dominant axis of a raw column/row delta.
///
/// No aspect-ratio correction: [`touch::TAP_SLOP_X`]/[`TAP_SLOP_Y`] already
/// encode this grid's 2:1 column:row physical aspect (a column is half a
/// row's physical height), so comparing raw cell counts directly is already
/// calibrated the same way the rest of this module's touch handling is.
const fn dominant_dir(dx: i32, dy: i32) -> Option<Dir> {
    if dx == 0 && dy == 0 {
        return None;
    }
    if dx.abs() >= dy.abs() {
        Some(if dx > 0 { Dir::Right } else { Dir::Left })
    } else {
        Some(if dy > 0 { Dir::Down } else { Dir::Up })
    }
}

/// What a registered touch region means when tapped.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// A D-pad button.
    Move(Dir),
    /// An interior board tile, in board-local (not screen) coordinates. Only
    /// acted on if it turns out to be adjacent to the player when tapped; see
    /// [`RhythmCrypt::try_move_to`].
    Tile(i32, i32),
}

/// Which kind of enemy, and the deterministic beat pattern that drives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EnemyKind {
    Slime,
    Skeleton,
}

impl EnemyKind {
    /// Whether this enemy takes its one step on `beat_count`.
    ///
    /// The two kinds are given complementary parities rather than the same
    /// one, which is the whole point: if both moved on even beats they would
    /// read as one enemy type reused with a different glyph. Interleaving
    /// them means every single beat has *something* stepping, and which
    /// thing it is is fully determined by the beat count, so a player who
    /// has watched one cycle can predict every future one exactly.
    const fn acts_on(self, beat_count: u32) -> bool {
        match self {
            Self::Slime => beat_count.is_multiple_of(2),
            Self::Skeleton => !beat_count.is_multiple_of(2),
        }
    }

    const fn art(self) -> [&'static str; 2] {
        match self {
            Self::Slime => SLIME_ART,
            Self::Skeleton => SKELETON_ART,
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Slime => rgb(120, 200, 120),
            Self::Skeleton => rgb(224, 220, 200),
        }
    }

    const fn max_hp() -> i32 {
        2
    }
}

/// One enemy on the board.
struct Enemy {
    kind: EnemyKind,
    pos: (i32, i32),
    hp: i32,
    /// Decays after acting; brightens the figure briefly so a beat-locked
    /// step reads as a discrete action rather than a silent teleport.
    step_flash: f32,
}

/// State: player, enemies, the beat clock, combo/currency counters, and the
/// touch plumbing every interactive demo from here on shares.
pub struct RhythmCrypt {
    player_pos: (i32, i32),
    player_hp: i32,
    player_hit_flash: f32,
    enemies: Vec<Enemy>,
    /// Accumulated world-seconds. Never reset, never counted in ticks -- see
    /// the module docs on why the beat is driven from this rather than from
    /// `Frame` call counts.
    time: f32,
    beat_count: u32,
    combo: u32,
    best_combo: u32,
    coins: u32,
    diamonds: u32,
    /// Decays after a successful on-beat action; brightens the beat track.
    hit_flash: f32,
    /// Decays after an off-beat action broke the combo; reddens the track.
    miss_flash: f32,
    pointer: Pointer,
    /// Where the currently-held press started, tracked independently of
    /// [`Pointer`] so a released drag's vector can be measured end to end
    /// (the "press-to-drop vector" swipe path; see [`RhythmCrypt::handle_events`]).
    press_origin: Option<Pos>,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for RhythmCrypt {
    fn default() -> Self {
        Self {
            player_pos: (1, 2),
            player_hp: MAX_HP,
            player_hit_flash: 0.0,
            enemies: vec![
                Enemy {
                    kind: EnemyKind::Slime,
                    pos: (4, 1),
                    hp: EnemyKind::max_hp(),
                    step_flash: 0.0,
                },
                Enemy {
                    kind: EnemyKind::Skeleton,
                    pos: (4, 3),
                    hp: EnemyKind::max_hp(),
                    step_flash: 0.0,
                },
            ],
            time: 0.0,
            beat_count: 0,
            combo: 0,
            best_combo: 0,
            coins: 0,
            diamonds: 0,
            hit_flash: 0.0,
            miss_flash: 0.0,
            pointer: Pointer::new(),
            press_origin: None,
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl RhythmCrypt {
    const fn in_interior(pos: (i32, i32)) -> bool {
        pos.0 >= 0 && pos.0 < BOARD_W && pos.1 >= 0 && pos.1 < BOARD_H
    }

    /// Progress through the current beat cycle, `0.0` (just landed) to `1.0`
    /// (about to land again). The one function every timing decision and the
    /// track's own cursor position both read from, so they can never disagree
    /// about where in the beat the game currently is.
    fn beat_phase(&self) -> f32 {
        (self.time % BEAT_PERIOD) / BEAT_PERIOD
    }

    /// Whether right now falls inside the visible timing window. The window
    /// straddles the wrap point (phase 0 and phase 1 are the same instant),
    /// so it is one test in two pieces rather than a single contiguous range.
    fn on_beat(&self) -> bool {
        let p = self.beat_phase();
        !(BEAT_WINDOW..=1.0 - BEAT_WINDOW).contains(&p)
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match &event {
                Event::Key(key) if key.is_down() => {
                    if let Some(dir) = key_dir(key.code) {
                        self.try_move(dir);
                    }
                }
                Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
                    self.press_origin = Some(m.position);
                }
                _ => {}
            }
            self.pointer.feed(&event);
        }

        let gesture = self.pointer.take();

        if let Some(tap) = gesture.tap {
            if let Some(action) = self.hotspots.hit(tap).copied() {
                match action {
                    Action::Move(dir) => self.try_move(dir),
                    Action::Tile(tx, ty) => self.try_move_to((tx, ty)),
                }
            }
            self.press_origin = None;
        }

        if let Some(drop) = gesture.drop {
            if let Some(origin) = self.press_origin {
                let dx = i32::from(drop.x) - i32::from(origin.x);
                let dy = i32::from(drop.y) - i32::from(origin.y);
                if let Some(dir) = dominant_dir(dx, dy) {
                    self.try_move(dir);
                }
            }
            self.press_origin = None;
        }

        true
    }

    /// Attempts one cardinal step: attacks if an enemy occupies the target
    /// tile, otherwise walks. Always attempted -- a rhythm game that refused
    /// off-beat input entirely would be unplayable without a metronome in the
    /// room -- but only registered against the combo via [`register_action`]
    /// (`Self::register_action`).
    fn try_move(&mut self, dir: Dir) {
        let (dx, dy) = dir.delta();
        let next = (self.player_pos.0 + dx, self.player_pos.1 + dy);
        if !Self::in_interior(next) {
            return;
        }
        if let Some(idx) = self.enemies.iter().position(|e| e.pos == next) {
            self.attack_enemy(idx);
        } else {
            self.player_pos = next;
        }
        self.register_action();
    }

    /// A tap on a specific tile only acts if that tile is directly adjacent
    /// to the player: a tap two tiles away has no unambiguous single-step
    /// meaning, so it is ignored rather than guessed at. See the mobile-first
    /// "prefer tap-select-then-tap-target for dense boards" guidance -- this
    /// demo's version of that is "the target must already be reachable".
    fn try_move_to(&mut self, target: (i32, i32)) {
        let dx = target.0 - self.player_pos.0;
        let dy = target.1 - self.player_pos.1;
        if dx.abs() + dy.abs() != 1 {
            return;
        }
        if let Some(dir) = dominant_dir(dx, dy) {
            self.try_move(dir);
        }
    }

    fn attack_enemy(&mut self, idx: usize) {
        self.enemies[idx].hp -= 1;
        self.enemies[idx].step_flash = 1.0;
        if self.enemies[idx].hp <= 0 {
            self.enemies.remove(idx);
            self.coins += 1;
        }
    }

    /// Scores the action just taken against the beat: on time, the combo
    /// climbs and the track flashes gold; off time, the combo breaks back to
    /// zero and the track flashes red. This is the entire mechanic the demo
    /// exists to show, so both outcomes get an immediate, unmissable signal
    /// rather than a number changing quietly in a corner.
    fn register_action(&mut self) {
        if self.on_beat() {
            self.combo += 1;
            self.best_combo = self.best_combo.max(self.combo);
            self.hit_flash = 1.0;
            if self.combo.is_multiple_of(5) {
                self.diamonds += 1;
            }
        } else {
            self.combo = 0;
            self.miss_flash = 1.0;
        }
    }

    /// Advances the beat clock and, on every beat landed this frame, runs
    /// enemy AI. Detected by phase wraparound rather than a modular tick
    /// count so a single long frame (a stalled backend, a slow first frame)
    /// still advances the beat count correctly instead of silently losing
    /// beats.
    fn simulate(&mut self, dt: f32) {
        let prev_phase = self.beat_phase();
        self.time += dt;
        let phase = self.beat_phase();
        if phase < prev_phase {
            self.beat_count += 1;
            self.on_beat_tick();
        }

        let decay = |v: f32| dt.mul_add(-3.0, v).max(0.0);
        self.player_hit_flash = decay(self.player_hit_flash);
        self.hit_flash = dt.mul_add(-2.5, self.hit_flash).max(0.0);
        self.miss_flash = dt.mul_add(-2.5, self.miss_flash).max(0.0);
        for enemy in &mut self.enemies {
            enemy.step_flash = decay(enemy.step_flash);
        }
    }

    /// Runs one beat's worth of enemy movement: each enemy whose
    /// [`EnemyKind::acts_on`] this beat takes exactly one cardinal step
    /// toward the player, or attacks in place if that step would land on the
    /// player's own tile.
    fn on_beat_tick(&mut self) {
        let target = self.player_pos;
        for i in 0..self.enemies.len() {
            let kind = self.enemies[i].kind;
            if !kind.acts_on(self.beat_count) {
                continue;
            }
            let (ex, ey) = self.enemies[i].pos;
            let (dx, dy) = (target.0 - ex, target.1 - ey);
            if dx == 0 && dy == 0 {
                continue;
            }
            let step = if dx.abs() >= dy.abs() {
                (dx.signum(), 0)
            } else {
                (0, dy.signum())
            };
            let next = (ex + step.0, ey + step.1);

            if next == self.player_pos {
                self.player_hp = (self.player_hp - 1).max(0);
                self.player_hit_flash = 1.0;
            } else {
                let occupied = self
                    .enemies
                    .iter()
                    .enumerate()
                    .any(|(j, o)| j != i && o.pos == next);
                if Self::in_interior(next) && !occupied {
                    self.enemies[i].pos = next;
                }
            }
            self.enemies[i].step_flash = 1.0;
        }
    }

    /// Splits `content` per [`Shape`] and draws every panel.
    ///
    /// Portrait stacks everything top to bottom (status up top, the D-pad at
    /// the very bottom, in the thumb zone). Landscape and desktop instead
    /// give the HUD a left-hand column, because those shapes are short on
    /// rows and generous on columns -- the same rows-vs-columns tradeoff
    /// [`Shape`] itself is built to answer.
    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect, shape: Shape) {
        const TRACK_H: u16 = 3;
        let dpad_h = touch::TAP_H;

        if shape.stacks() {
            let hud_h = 3u16.min(content.height());
            let (hud_area, rest) = panel::split_top(content, hud_h);
            let (rest, dpad_area) = panel::split_bottom(rest, dpad_h.min(rest.height()));
            let (board_area, track_area) = panel::split_bottom(rest, TRACK_H.min(rest.height()));
            self.draw_hud_row(surface, hud_area);
            self.draw_board(surface, board_area);
            self.draw_beat_track(surface, track_area);
            self.draw_dpad(surface, dpad_area);
        } else {
            let side_w = 22u16.min(content.width() / 3);
            let (side_area, rest) = panel::split_left(content, side_w);
            let (rest, dpad_area) = panel::split_bottom(rest, dpad_h.min(rest.height()));
            let (board_area, track_area) = panel::split_bottom(rest, TRACK_H.min(rest.height()));
            self.draw_hud_column(surface, side_area);
            self.draw_board(surface, board_area);
            self.draw_beat_track(surface, track_area);
            self.draw_dpad(surface, dpad_area);
        }
    }

    /// Draws the dungeon: a ring of wall tiles (some carrying torches) around
    /// a floor of interior tiles, each holding at most one occupant.
    ///
    /// Tile size is fit to whatever room `area` actually has, clamped at
    /// [`TILE_W_MAX`]/[`TILE_H_MAX`] and never allowed to exceed the area:
    /// on a very short viewport (the 80x24 headless case in particular) this
    /// can put tiles below the 5x3 the brief sets as a floor, in which case
    /// the board is drawn smaller and possibly clipped at the edges rather
    /// than overflowing into neighbouring panels. A demo that panicked or
    /// spilled its chrome at the hardest test size would be worse than one
    /// that draws a slightly cramped board there.
    fn draw_board(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cols = u16::try_from(BOARD_W + 2).unwrap_or(u16::MAX);
        let rows = u16::try_from(BOARD_H + 2).unwrap_or(u16::MAX);
        let tile_w = (area.width() / cols).clamp(1, TILE_W_MAX);
        let tile_h = (area.height() / rows).clamp(1, TILE_H_MAX);
        let footprint_w = cols * tile_w;
        let footprint_h = rows * tile_h;
        let ox = area.left() + (area.width().saturating_sub(footprint_w)) / 2;
        let oy = area.top() + (area.height().saturating_sub(footprint_h)) / 2;

        for gy in 0..rows {
            for gx in 0..cols {
                let rect = Rect::new(ox + gx * tile_w, oy + gy * tile_h, tile_w, tile_h);
                if rect.left() >= area.right() || rect.top() >= area.bottom() {
                    continue;
                }
                let clipped = clip_rect(rect, area);
                let is_wall = gx == 0 || gy == 0 || gx == cols - 1 || gy == rows - 1;
                if is_wall {
                    let torch = TORCH_TILES.contains(&(i32::from(gx), i32::from(gy)));
                    draw_wall_tile(surface, clipped, gx, gy, torch, self.time);
                    continue;
                }

                let ix = i32::from(gx) - 1;
                let iy = i32::from(gy) - 1;
                let floor_color = draw_floor_tile(surface, clipped, ix, iy);

                // Tapping an interior tile is a second way to issue the same
                // move a D-pad button or a swipe would; see the module docs
                // on why a rhythm game needs more than one path to the same
                // action.
                self.hotspots.push(rect, Action::Tile(ix, iy));

                if self.player_pos == (ix, iy) {
                    draw_occupant(
                        surface,
                        clipped,
                        PLAYER_ART,
                        ui::ACCENT,
                        floor_color,
                        self.player_hit_flash,
                    );
                } else if let Some(enemy) = self.enemies.iter().find(|e| e.pos == (ix, iy)) {
                    draw_occupant(
                        surface,
                        clipped,
                        enemy.kind.art(),
                        enemy.kind.color(),
                        floor_color,
                        enemy.step_flash,
                    );
                }
            }
        }
    }

    /// The beat track: a horizontal bar whose cursor sweeps from left to
    /// right across one beat cycle, landing on the target windows drawn at
    /// both ends, plus the combo readout.
    fn draw_beat_track(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Beat").draw(surface, area);
        if inner.width() < 4 || inner.height() == 0 {
            return;
        }
        let w = inner.width();
        let bar_y = inner.top();

        // The window is drawn at *both* ends of the bar because the phase it
        // tests wraps: position 0 and position 1 are the same instant (a beat
        // landing), so showing the window only at the right edge would hide
        // the half of it that opens the moment a beat lands at the left.
        let window_cells = ((f32::from(w) * BEAT_WINDOW).round() as u16).max(1);
        for i in 0..w {
            let in_window = i < window_cells || i + window_cells >= w;
            let base = if in_window {
                rgb(96, 74, 32)
            } else {
                rgb(40, 34, 54)
            };
            let color = if self.hit_flash > 0.0 {
                mix(base, rgb(255, 220, 120), self.hit_flash)
            } else if self.miss_flash > 0.0 {
                mix(base, rgb(210, 70, 70), self.miss_flash)
            } else {
                base
            };
            surface.put(
                (inner.left() + i, bar_y),
                '\u{2500}',
                Style::new().fg(color).bg(panel::PANEL_BG),
            );
        }

        let phase = self.beat_phase();
        let marker_x = (phase * f32::from(w.saturating_sub(1))).round() as u16;
        surface.put(
            (inner.left() + marker_x, bar_y),
            '\u{2588}',
            Style::new().fg(rgb(255, 235, 180)).bg(panel::PANEL_BG),
        );

        if inner.height() > 1 {
            let text = format!(
                "combo x{}  best x{}  bpm {:.0}",
                self.combo, self.best_combo, BPM
            );
            let color = if self.combo > 0 { ui::ACCENT } else { ui::DIM };
            surface.print(
                (inner.left(), bar_y + 1),
                truncate(&text, inner.width_usize()),
                Style::new().fg(color).bg(panel::PANEL_BG),
            );
        }
    }

    /// The D-pad: four large, fixed buttons in a row. A row rather than a
    /// cross shape so it stays [`touch::TAP_H`] tall (not three times that)
    /// regardless of [`Shape`], which is what lets landscape and even the
    /// 80x24 headless size keep every button at full legal touch size instead
    /// of shrinking the one control this demo cannot afford to shrink.
    fn draw_dpad(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let dirs = [Dir::Left, Dir::Up, Dir::Down, Dir::Right];
        let cols = panel::columns(area, dirs.len() as u16, 1);
        for (rect, dir) in cols.into_iter().zip(dirs) {
            let target = touch::tappable(rect, area);
            self.hotspots.push(target, Action::Move(dir));
            let inner = panel::Panel::new().title(dir.label()).draw(surface, target);
            if inner.width() == 0 || inner.height() == 0 {
                continue;
            }
            let cx = inner.left() + inner.width() / 2;
            let cy = inner.top() + inner.height() / 2;
            surface.put(
                (cx, cy),
                dir.arrow(),
                Style::new().fg(ui::ACCENT).bg(panel::PANEL_BG),
            );
        }
    }

    /// The horizontal HUD used when [`Shape::stacks`]: an equipment line,
    /// then the hearts and currency below it.
    fn draw_hud_row(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let (equip_area, rest) = panel::split_top(area, 1.min(area.height()));
        Self::draw_equipment_line(surface, equip_area);
        if rest.height() > 0 {
            self.draw_hearts(surface, rest);
            self.draw_currency(surface, rest);
        }
    }

    /// The vertical HUD used in landscape/desktop, stacked in a left column
    /// alongside the board rather than above it.
    fn draw_hud_column(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }
        let (equip_area, rest) = panel::split_top(
            area,
            u16::try_from(EQUIPMENT.len())
                .unwrap_or(0)
                .min(area.height()),
        );
        Self::draw_equipment_column(surface, equip_area);
        let (hearts_area, rest2) = panel::split_top(rest, 1.min(rest.height()));
        self.draw_hearts(surface, hearts_area);
        let currency_area = Rect::new(
            rest2.left(),
            rest2.top(),
            rest2.width(),
            1.min(rest2.height()),
        );
        self.draw_currency(surface, currency_area);
    }

    fn draw_equipment_line(surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        let mut text = String::new();
        for (glyph, name) in EQUIPMENT {
            text.push(glyph);
            text.push_str(name);
            text.push_str("  ");
        }
        surface.print(
            (area.left(), area.top()),
            truncate(&text, area.width_usize()),
            Style::new().fg(ui::FG).bg(ui::CHROME_BG),
        );
    }

    fn draw_equipment_column(surface: &mut Surface<'_>, area: Rect) {
        for (i, (glyph, name)) in EQUIPMENT.iter().enumerate() {
            let y = area.top() + i as u16;
            if y >= area.bottom() {
                break;
            }
            let text = format!("{glyph} {name}");
            surface.print(
                (area.left(), y),
                truncate(&text, area.width_usize()),
                Style::new().fg(ui::FG).bg(ui::CHROME_BG),
            );
        }
    }

    /// [`MAX_HP`] multi-cell hearts, drawn left to right. Each is a small
    /// filled swatch (not a single glyph) so a lost heart reads as a shape
    /// going dark, not as a color change on one character easy to miss.
    fn draw_hearts(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        let heart_w = 3u16;
        for i in 0..MAX_HP {
            let x = area.left() + u16::try_from(i).unwrap_or(0) * (heart_w + 1);
            if x + heart_w > area.right() {
                break;
            }
            let filled = i < self.player_hp;
            let (fg, bg) = if filled {
                (rgb(255, 130, 150), rgb(120, 24, 44))
            } else {
                (rgb(90, 62, 68), rgb(30, 18, 22))
            };
            let rect = Rect::new(x, area.top(), heart_w, area.height());
            surface.fill_rect(rect, '\u{2588}', Style::new().fg(bg).bg(bg));
            let cx = x + heart_w / 2;
            surface.put((cx, area.top()), '\u{2665}', Style::new().fg(fg).bg(bg));
        }
    }

    fn draw_currency(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        let text = format!("$ {}  \u{2666} {}", self.coins, self.diamonds);
        let x = area
            .right()
            .saturating_sub(u16::try_from(text.chars().count()).unwrap_or(0))
            .max(area.left());
        surface.print(
            (x, area.top()),
            truncate(&text, area.width_usize()),
            Style::new().fg(ui::FG).bg(ui::CHROME_BG),
        );
    }

    fn status(&self) -> String {
        format!(
            "hp {}/{}  beat {}  coins {}",
            self.player_hp, MAX_HP, self.beat_count, self.coins
        )
    }
}

/// Shrinks `rect` so it never draws past `bounds`'s right/bottom edge,
/// without moving its origin. Used once the board's tile grid has been sized
/// to fill `area` as closely as an integer tile count allows: the last
/// column/row of tiles can still overhang by a few cells, and this is what
/// keeps that overhang from writing into whatever panel sits past the edge.
fn clip_rect(rect: Rect, bounds: Rect) -> Rect {
    let w = rect.width().min(bounds.right().saturating_sub(rect.left()));
    let h = rect
        .height()
        .min(bounds.bottom().saturating_sub(rect.top()));
    Rect::new(rect.left(), rect.top(), w, h)
}

/// A stable (time-independent) stone shade for floor tile `(ix, iy)`.
///
/// Only the torches flicker; the floor itself must not, or the whole room
/// would read as shimmering rather than lit. Per-tile variation comes from a
/// position hash rather than a shared constant so the floor still has visible
/// texture instead of being one flat color repeated across every tile.
fn floor_stone_color(ix: i32, iy: i32) -> Color {
    let shade = hash01(0x4472, ix, iy);
    mix(rgb(54, 36, 70), rgb(80, 58, 100), shade)
}

/// Draws one interior floor tile as a brick slab: a filled stone color with
/// mortar seams drawn as box-drawing strokes, offset between the tile's upper
/// and lower half so the joints read as a running bond rather than a single
/// centered cross. Returns the stone color used, so the caller can pass it on
/// to [`draw_occupant`] as the shadow color under a figure standing here.
fn draw_floor_tile(surface: &mut Surface<'_>, rect: Rect, ix: i32, iy: i32) -> Color {
    let stone = floor_stone_color(ix, iy);
    let mortar = rgb(26, 16, 34);
    let (w, h) = (rect.width(), rect.height());
    let mid = h / 2;
    let seam_upper = (w / 3).max(1);
    let seam_lower = (2 * w / 3).max(1);

    for ly in 0..h {
        for lx in 0..w {
            let h_seam = h >= 3 && ly == mid;
            let v_seam = if ly < mid {
                lx == seam_upper
            } else {
                lx == seam_lower
            };
            let (glyph, fg) = if h_seam {
                ('\u{2500}', mortar)
            } else if v_seam {
                ('\u{2502}', mortar)
            } else {
                ('\u{2588}', stone)
            };
            surface.put(
                (rect.left() + lx, rect.top() + ly),
                glyph,
                Style::new().fg(fg).bg(stone),
            );
        }
    }
    stone
}

/// A stable stone shade for wall tile `(gx, gy)`, in absolute grid
/// coordinates. See [`floor_stone_color`].
fn wall_stone_color(gx: u16, gy: u16) -> Color {
    let shade = hash01(0x91A1, i32::from(gx), i32::from(gy));
    mix(rgb(40, 30, 52), rgb(58, 46, 72), shade)
}

/// Draws one wall tile: horizontal coursing (every other row is a mortar
/// band, the vertical analogue of a floor tile's running bond), plus a
/// flickering torch overlay if `torch` is set.
fn draw_wall_tile(surface: &mut Surface<'_>, rect: Rect, gx: u16, gy: u16, torch: bool, time: f32) {
    let stone = wall_stone_color(gx, gy);
    let mortar = rgb(20, 14, 28);
    let (w, h) = (rect.width(), rect.height());
    for ly in 0..h {
        for lx in 0..w {
            let coursed = h >= 2 && ly % 2 == 1;
            let (glyph, fg) = if coursed {
                ('\u{2550}', mortar)
            } else {
                ('\u{2588}', stone)
            };
            surface.put(
                (rect.left() + lx, rect.top() + ly),
                glyph,
                Style::new().fg(fg).bg(stone),
            );
        }
    }
    if torch {
        draw_torch(surface, rect, time);
    }
}

/// A single flickering torch, placed at the centre of its wall tile.
///
/// The flicker's phase comes from the torch's own screen position, not a
/// shared clock, so neighbouring torches never pulse in lockstep -- the same
/// decorrelation trick `24_torchlit_crypt`'s sconces use. Brightness (not
/// position) is what animates, and it animates from `time` (accumulated
/// delta), which is what keeps it moving at the same visual rate on every
/// backend regardless of that backend's own frame rate.
fn draw_torch(surface: &mut Surface<'_>, rect: Rect, time: f32) {
    if rect.width() == 0 || rect.height() == 0 {
        return;
    }
    let cx = rect.left() + rect.width() / 2;
    let cy = rect.top() + rect.height() / 2;
    let phase = hash01(0x7733, i32::from(cx), i32::from(cy)) * core::f32::consts::TAU;
    let flicker = 0.5f32.mul_add((time.mul_add(9.0, phase)).sin(), 0.5);
    let color = mix(rgb(120, 50, 20), rgb(255, 190, 90), flicker);
    surface.put((cx, cy), '!', Style::new().fg(color).bg(rgb(20, 14, 20)));
}

/// Overlays a two-row, three-column occupant figure onto a tile already
/// carrying floor art. Only the figure's non-space cells are written, so
/// [`draw_floor_tile`]'s mortar lines keep showing through the gaps in the
/// figure's own silhouette -- the coexistence the module docs describe.
///
/// `extra_flash` brightens the figure briefly (0.0 = resting color, 1.0 =
/// fully lightened), used for the beat-locked step/attack pulse both the
/// player and enemies get.
fn draw_occupant(
    surface: &mut Surface<'_>,
    tile: Rect,
    art: [&str; 2],
    color: Color,
    floor_color: Color,
    extra_flash: f32,
) {
    let shadow = mix(floor_color, rgb(0, 0, 0), 0.4);
    let lit = mix(color, rgb(255, 255, 255), extra_flash.clamp(0.0, 1.0) * 0.5);

    if tile.width() < 3 || tile.height() < 2 {
        // Too small for the full figure: still show something rather than
        // leaving the tile blank, the same graceful-degradation rule the
        // rest of this gallery applies at extreme viewport sizes.
        if tile.width() > 0 && tile.height() > 0 {
            surface.put(
                (tile.left(), tile.top()),
                '@',
                Style::new().fg(lit).bg(shadow),
            );
        }
        return;
    }

    let ox = tile.left() + (tile.width() - 3) / 2;
    let oy = tile.top() + (tile.height() - 2) / 2;
    for (row_i, row) in art.iter().enumerate() {
        for (col_i, ch) in row.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let pos = (ox + col_i as u16, oy + row_i as u16);
            if pos.0 >= tile.right() || pos.1 >= tile.bottom() {
                continue;
            }
            surface.put(pos, ch, Style::new().fg(lit).bg(shadow));
        }
    }
}

impl Demo for RhythmCrypt {
    const NAME: &'static str = "27_rhythm_crypt";
    const TITLE: &'static str = "27 Rhythm Crypt";
    const BLURB: &'static str =
        "Beat-locked dungeon crawling: act on the beat for combo, miss it and it breaks.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[("Arrows/WASD", "move / attack (on beat for combo)")]
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

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(RhythmCrypt);

#[cfg(test)]
mod tests {
    use super::{BEAT_PERIOD, Dir, EnemyKind, RhythmCrypt, dominant_dir};

    #[test]
    fn beat_phase_wraps_at_the_period() {
        let demo = RhythmCrypt {
            time: BEAT_PERIOD * 1.5,
            ..Default::default()
        };
        assert!((demo.beat_phase() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn a_beat_landing_advances_the_count_exactly_once() {
        // Just before the wrap, then a step that crosses it exactly once.
        let mut demo = RhythmCrypt {
            time: BEAT_PERIOD * 0.9,
            ..Default::default()
        };
        demo.simulate(BEAT_PERIOD * 0.2);
        assert_eq!(demo.beat_count, 1);
    }

    #[test]
    fn slime_and_skeleton_act_on_complementary_beats() {
        for beat in 0..8u32 {
            assert_ne!(
                EnemyKind::Slime.acts_on(beat),
                EnemyKind::Skeleton.acts_on(beat),
                "beat {beat}"
            );
        }
    }

    #[test]
    fn dominant_dir_picks_the_larger_axis() {
        assert_eq!(dominant_dir(5, 1), Some(Dir::Right));
        assert_eq!(dominant_dir(-5, 1), Some(Dir::Left));
        assert_eq!(dominant_dir(1, 5), Some(Dir::Down));
        assert_eq!(dominant_dir(1, -5), Some(Dir::Up));
        assert_eq!(dominant_dir(0, 0), None);
    }

    #[test]
    fn an_off_beat_action_breaks_an_existing_combo() {
        // Dead centre of the cycle is as far off the beat as it gets.
        let mut demo = RhythmCrypt {
            combo: 4,
            time: BEAT_PERIOD * 0.5,
            ..Default::default()
        };
        demo.register_action();
        assert_eq!(demo.combo, 0);
        assert!(demo.miss_flash > 0.0);
    }

    #[test]
    fn an_on_beat_action_grows_the_combo() {
        // Phase 0 is the dead centre of the timing window.
        let mut demo = RhythmCrypt {
            time: 0.0,
            ..Default::default()
        };
        demo.register_action();
        assert_eq!(demo.combo, 1);
        assert!(demo.hit_flash > 0.0);
    }
}
