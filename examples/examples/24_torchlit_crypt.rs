//! 24: Torchlit crypt -- colored dynamic lighting, and why it is not field of
//! view.
//!
//! Two questions look alike and are not. Field of view asks "can the player
//! see this cell right now": a pure geometry query over line-of-sight
//! obstacles, answered here by [`tilekit::fov::shadowcast`]. Lighting asks "how
//! much light, of what color, falls on this cell": an independent question
//! about where photons happen to be, answered by [`tilekit::light::LightMap`].
//! A torch glowing around a corner lights a wall you cannot see past; a
//! moonlit clearing you can see perfectly is not lit at all. Diablo and Brogue
//! both keep these as separate systems for exactly that reason, and a renderer
//! that conflates them (draw whatever is "visible" at full brightness) cannot
//! produce either effect.
//!
//! This demo makes the difference into the thing being demonstrated: a `M`
//! key cycles between viewing field of view alone, lighting alone, and the two
//! combined, which is what a real dungeon crawler ships. In the combined mode,
//! a cell is drawn at full color only where it is *both* visible and lit;
//! visible-but-unlit ground is a near-black silhouette (you can tell there is
//! floor there, not what color it is); and a lit-but-not-visible wall glows
//! from a torch around the corner without revealing the room beyond it.
//!
//! Techniques on show:
//!
//! - **Additive colored lighting** ([`tilekit::light`]): warm torches, one
//!   cold magical light, and a light the player carries, accumulated in
//!   floating point and tone-mapped with `T` toggling
//!   [`Reinhard`](tilekit::light::ToneMap::Reinhard) against
//!   [`Clamp`](tilekit::light::ToneMap::Clamp) so the difference between
//!   "overlapping torches mix to a warm color" and "overlapping torches clip to
//!   white" is visible on demand.
//! - **Flicker with decorrelated phase**: every torch flickers, none of them
//!   in step, because [`tilekit::light::Light::torch`] seeds its phase from
//!   position.
//! - **A brightness ramp as a second channel** (`G`): below a luma threshold,
//!   the floor's usual masonry glyph gives way to
//!   [`tilekit::glyphs::ASCII_RAMP`], so the darkest reaches of the crypt read
//!   as darker glyphs as well as darker color -- useful on a monochrome
//!   terminal, and worth seeing even on a color one.
//! - **A decorated floor with per-tile variation**: a repeating masonry pattern
//!   built from [`tilekit::noise::hash01`] rather than a single repeated
//!   glyph, plus scattered cracks and bones, so there is texture for the light
//!   to actually reveal.
//! - **A HUD built for a crawler**: a hotbar of framed skill icons with charge
//!   counts and cooldown shading, and a health/mana orb pair using `▀▄█` for
//!   half-row fill precision, which is the vertical analogue of
//!   [`ui::panel::bar`]'s half-cell horizontal precision.
//!
//! ```sh
//! cargo run --example 24_torchlit_crypt --features crossterm
//! cargo run --example 24_torchlit_crypt --features software
//! cargo run --example 24_torchlit_crypt --features gl
//! cargo run --example 24_torchlit_crypt  # headless, prints a few frames
//! ```

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::{self, panel};
use ascii_tile_demos::util::perf::FpsMeter;
use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use tilekit::fov::shadowcast;
use tilekit::glyphs::ASCII_RAMP;
use tilekit::light::{Falloff, Light, LightMap, ToneMap};
use tilekit::noise::hash01;
use tilekit::palette::{mix, rgb};

/// Crypt size in cells. Fixed rather than generated per demo, because the
/// point here is lighting, and a hand-placed room with a legible patrol loop
/// shows that off better than a procedural maze would.
const CRYPT_W: i32 = 46;
/// See [`CRYPT_W`].
const CRYPT_H: i32 = 22;

/// How large one logical crypt cell is drawn, in screen cells, at the
/// largest scale this demo will use.
///
/// A lighting demo lives or dies on whether a torch's falloff is actually
/// visible, and a falloff over 8-11 cells of *radius* reads as a gradient
/// only if each of those cells is large enough on screen to carry a visibly
/// different shade from its neighbour. At 1 screen cell per logical cell the
/// whole 46x22 room only ever occupies a fraction of this demo's own
/// 150x46 [`Demo::GRID`], which is what the unscaled version got wrong: the
/// room was a small dark square adrift in a much bigger black field, most of
/// which had nothing to do with the room at all. Blowing each logical cell up
/// to a 2x2 (or, room permitting, 3x3) block of screen cells is the same
/// multi-cell-tile technique `02_chunky_tiles` and `06_iso_elevation` use, and
/// it is what actually fills the content area with *crypt* rather than with
/// padding.
const MAX_BLOCK: u16 = 3;

/// Which of the two systems (or both) the map currently renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ViewMode {
    /// Field of view only: full color wherever visible, black elsewhere. No
    /// lighting is applied at all, which is the point -- it shows what a
    /// renderer that only tracks sight (and nothing else) actually gives you.
    FovOnly,
    /// Lighting only, ignoring visibility entirely: every lit cell shows at
    /// full color whether or not the player could actually see it, including
    /// through walls. Deliberately "wrong" for a real game, and exactly what
    /// makes the point that lighting alone is not enough either.
    LightOnly,
    /// Both combined, which is what a real dungeon crawler ships: a cell reads
    /// at full color only where it is visible AND lit, a visible-unlit cell is
    /// a dim silhouette, and a lit-invisible cell (a torch glow bleeding
    /// through a doorway) shows only as light on the wall you can see.
    #[default]
    Combined,
}

impl ViewMode {
    const fn next(self) -> Self {
        match self {
            Self::FovOnly => Self::LightOnly,
            Self::LightOnly => Self::Combined,
            Self::Combined => Self::FovOnly,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::FovOnly => "field of view only",
            Self::LightOnly => "lighting only (ignores sight)",
            Self::Combined => "combined (visible AND lit)",
        }
    }
}

/// One cell of the crypt: whether it blocks sight, and its base (unlit) floor
/// or wall color plus glyph.
#[derive(Clone, Copy)]
struct Cell {
    wall: bool,
    glyph: char,
    color: Color,
}

/// The hand-authored crypt layout, its lights, and the patrol route the
/// player walks.
struct Crypt {
    cells: Vec<Cell>,
}

impl Crypt {
    fn generate(seed: u32) -> Self {
        let mut cells = Vec::with_capacity((CRYPT_W * CRYPT_H) as usize);
        for y in 0..CRYPT_H {
            for x in 0..CRYPT_W {
                cells.push(floor_or_wall(x, y, seed));
            }
        }
        Self { cells }
    }

    const fn in_bounds(x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < CRYPT_W && y < CRYPT_H
    }

    fn cell(&self, x: i32, y: i32) -> Cell {
        if !Self::in_bounds(x, y) {
            return Cell {
                wall: true,
                glyph: '#',
                color: rgb(64, 60, 68),
            };
        }
        self.cells[(y * CRYPT_W + x) as usize]
    }

    fn blocks(&self, x: i32, y: i32) -> bool {
        self.cell(x, y).wall
    }
}

/// Perimeter walls, four interior pillars, and a masonry floor pattern with
/// per-tile crack and bone dressing.
///
/// Not flat: a lighting demo over a featureless floor shows nothing, because
/// there is no texture for the light to reveal. The masonry seam pattern
/// (mortar lines every few cells) gives the eye a grid to read brightness
/// against, and `hash01`-driven speckle keeps identical-brightness cells from
/// looking like a single stamped tile repeated.
fn floor_or_wall(x: i32, y: i32, seed: u32) -> Cell {
    let border = x == 0 || y == 0 || x == CRYPT_W - 1 || y == CRYPT_H - 1;
    // Four pillars, evenly spaced, each a single wall cell: enough to break
    // sightlines and cast readable shadows without turning the room into a
    // maze the patrol can get lost in.
    let pillar = matches!((x, y), (12 | 34, 7 | 15));
    if border || pillar {
        // Mortar coursing: every third row offset, so the wall reads as
        // stacked stone rather than a flat rectangle.
        let coursed = (y + (x / 3)) % 3 == 0;
        return Cell {
            wall: true,
            glyph: if coursed { '=' } else { '#' },
            color: rgb(96, 90, 98),
        };
    }

    let h = hash01(seed, x, y);
    // Masonry seams: a mortar line every 4 columns and every 2 rows, using the
    // box-drawing set rather than a plain '-'/'|' so it visually agrees with
    // the wall glyphs above.
    let seam_x = x % 4 == 0;
    let seam_y = y % 2 == 0;
    let (glyph, base) = match (seam_x, seam_y) {
        (true, true) => ('+', rgb(120, 112, 104)),
        (true, false) => ('|', rgb(112, 104, 96)),
        (false, true) => ('-', rgb(112, 104, 96)),
        _ => {
            // Flagstone speckle: three bands of stone tone plus rare dressing
            // (cracks, bones, a cobweb), each keyed off a different hash so
            // they do not all land on the same cells.
            if hash01(seed ^ 0x9E37, x, y) < 0.03 {
                ('%', rgb(80, 76, 84))
            } else if hash01(seed ^ 0x517C, x, y) < 0.02 {
                ('"', rgb(190, 184, 160))
            } else if hash01(seed ^ 0xC2B2, x, y) < 0.015 {
                ('\'', rgb(160, 154, 146))
            } else if h < 0.4 {
                ('.', rgb(128, 120, 108))
            } else if h < 0.75 {
                (',', rgb(120, 112, 100))
            } else {
                ('`', rgb(134, 126, 114))
            }
        }
    };
    Cell {
        wall: false,
        glyph,
        color: base,
    }
}

/// A fixed torch position, for the wall sconces.
///
/// Lines both long walls and both short walls at roughly even intervals
/// (rather than only the four corners the original layout used), so the room
/// reads as lit by many sconces the way the brief's Diablo reference is, and
/// so neighbouring pools overlap enough that the seam between them is always
/// on screen somewhere, not just in one corner a viewer might miss.
const TORCHES: [(i32, i32, u32); 10] = [
    (4, 4, 11),
    (4, 11, 17),
    (4, 18, 23),
    (41, 4, 37),
    (41, 11, 41),
    (41, 18, 53),
    (18, 4, 61),
    (28, 18, 67),
    (14, 18, 71),
    (32, 4, 89),
];

/// The cold magical light's position: hovering a few cells from the
/// `(18, 4)` wall sconce, close enough that their pools visibly overlap in a
/// still frame.
///
/// That overlap is not incidental. A screenshot with one warm pool and one
/// cool pool that never touch makes exactly the wrong argument for
/// `ToneMap::Reinhard`: the whole point of accumulating light in floating
/// point rather than clamping is that where warm and cool overlap, the result
/// is a third, blended color rather than a washed-out white, and that claim
/// is invisible unless two differently-colored pools actually share cells.
const SHARD: (i32, i32) = (23, 6);

/// Waypoints of the player's patrol loop, walked in order and looped.
const PATROL: [(i32, i32); 8] = [
    (8, 11),
    (18, 6),
    (28, 6),
    (38, 11),
    (28, 16),
    (18, 16),
    (8, 11),
    (23, 11),
];

/// Seconds spent walking between two consecutive patrol waypoints.
const LEG_DURATION: f32 = 2.6;

/// State: the crypt, view mode, tone curve, glyph-ramp toggle, patrol clock,
/// and the resource orbs.
pub struct TorchlitCrypt {
    crypt: Crypt,
    seed: u32,
    mode: ViewMode,
    tone: ToneMap,
    glyph_ramp: bool,
    time: f32,
    health: f32,
    mana: f32,
    fps: FpsMeter,
}

impl Default for TorchlitCrypt {
    fn default() -> Self {
        let seed = 1;
        Self {
            crypt: Crypt::generate(seed),
            seed,
            mode: ViewMode::default(),
            tone: ToneMap::default(),
            glyph_ramp: false,
            time: 0.0,
            health: 1.0,
            mana: 1.0,
            fps: FpsMeter::new(),
        }
    }
}

impl TorchlitCrypt {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1).max(1);
        self.crypt = Crypt::generate(self.seed);
        self.time = 0.0;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Char('m' | 'M') => self.mode = self.mode.next(),
                    KeyCode::Char('t' | 'T') => {
                        self.tone = match self.tone {
                            ToneMap::Reinhard => ToneMap::Clamp,
                            ToneMap::Clamp => ToneMap::Reinhard,
                        };
                    }
                    KeyCode::Char('g' | 'G') => self.glyph_ramp = !self.glyph_ramp,
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                }
            }
        }
        true
    }

    /// The player's current position and its light, interpolated linearly
    /// along the patrol loop by wall-clock time.
    ///
    /// A fixed patrol rather than input-driven movement: the point of this
    /// demo is what happens to fixed geometry as light and sight sweep across
    /// it, and a scripted route guarantees every corner of the room gets
    /// visited without relying on a reviewer to walk there.
    fn player_position(&self) -> (f32, f32) {
        let total = LEG_DURATION * (PATROL.len() - 1) as f32;
        let t = self.time.rem_euclid(total);
        let leg = (t / LEG_DURATION) as usize;
        let leg = leg.min(PATROL.len() - 2);
        let local = (leg as f32).mul_add(-LEG_DURATION, t) / LEG_DURATION;
        // Smoothstep, not linear: a patrol that eases in and out of each
        // waypoint reads as someone walking, not as a camera on rails.
        let eased = local * local * 2.0f32.mul_add(-local, 3.0);
        let (x0, y0) = PATROL[leg];
        let (x1, y1) = PATROL[leg + 1];
        (
            ((x1 - x0) as f32).mul_add(eased, x0 as f32),
            ((y1 - y0) as f32).mul_add(eased, y0 as f32),
        )
    }

    /// Builds this frame's light map: torch sconces, the magical shard, and
    /// the player's own light, all accumulated at the current time so their
    /// flicker phases stay in motion.
    fn build_lights(&self, px: i32, py: i32) -> LightMap {
        // Ambient is a real fraction of the visible range, not a token
        // amount: a torchlit room in a real building is not lightless
        // between the pools, and if it reads as pitch black except in a
        // one-cell halo, the demo has nothing to contrast the light against.
        // Against this room's masonry tones (roughly (100-130) per channel),
        // this ambient resolves an unlit floor to about (35-40) per channel
        // after Reinhard: dim enough to read as deep shadow, bright enough
        // that the floor's own texture is never simply invisible.
        let mut map = LightMap::new(CRYPT_W, CRYPT_H, rgb(85, 78, 86))
            .falloff(Falloff::Quadratic)
            .tone_map(self.tone);

        for &(x, y, seed) in &TORCHES {
            map.add(&Light::torch(x, y, 9.0, seed).intensity(3.2), self.time);
        }

        // The magical shard: steady and cold, so it reads as a different kind
        // of light source from the flickering torches rather than as one more
        // torch in a different color. Its radius is deliberately large enough
        // to reach the `(18, 4)` wall sconce a few cells away, so the two
        // pools -- one warm, one cold -- visibly overlap and blend rather
        // than sitting in separate, unconnected halos.
        map.add(
            &Light::new(SHARD.0, SHARD.1, 10.0, rgb(120, 210, 220)).intensity(3.0),
            self.time,
        );

        // The player's own light: small, warm, and it moves. This is the one
        // that makes the pools visibly slide across the masonry rather than
        // sitting fixed.
        map.add(
            &Light::new(px, py, 8.0, rgb(255, 200, 140))
                .intensity(2.2)
                .flicker(0.12, 131),
            self.time,
        );

        map
    }

    /// Field of view from the player's rounded position, out to a fixed
    /// radius, using the crypt's own walls and pillars as blockers.
    fn build_fov(&self, px: i32, py: i32) -> Vec<bool> {
        let mut seen = vec![false; (CRYPT_W * CRYPT_H) as usize];
        shadowcast(
            px,
            py,
            9,
            |x, y| self.crypt.blocks(x, y),
            |x, y| {
                if Crypt::in_bounds(x, y) {
                    seen[(y * CRYPT_W + x) as usize] = true;
                }
            },
        );
        seen
    }

    /// How many screen cells one logical crypt cell occupies horizontally
    /// and vertically, given the screen space actually available.
    ///
    /// Independent per axis rather than one uniform factor, and that is the
    /// fix rather than a refinement: this room is 46x22 (roughly 2:1), the
    /// content area under [`Demo::GRID`] is roughly 150x41 (roughly 3.7:1),
    /// and a terminal cell itself is not square (`CELL_ASPECT`, in
    /// `tilekit::palette`, is 2:1 tall). A single uniform scale is capped by
    /// whichever axis fills up first -- here, height, at a block of 1 -- and
    /// leaves most of the width unused no matter how the room is sized.
    /// Scaling each axis to its own best integer fit uses the screen the
    /// layout actually offers instead of the smaller square that a shared
    /// factor would settle for.
    ///
    /// Never zero: a block this demo cannot fit at all still draws at 1x
    /// rather than vanishing, the same graceful-degradation rule the chrome
    /// bars use.
    fn block_size(area: Rect) -> (u16, u16) {
        let fit_w = area.width() / CRYPT_W.max(1) as u16;
        let fit_h = area.height() / CRYPT_H.max(1) as u16;
        (fit_w.clamp(1, MAX_BLOCK), fit_h.clamp(1, MAX_BLOCK))
    }

    /// Draws the crypt into `area`, applying the active `ViewMode`.
    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let (px, py) = self.player_position();
        let (pxi, pyi) = (px.round() as i32, py.round() as i32);
        let lights = self.build_lights(pxi, pyi);
        let visible = self.build_fov(pxi, pyi);

        let (block_w, block_h) = Self::block_size(area);
        let footprint_w = CRYPT_W as u16 * block_w;
        let footprint_h = CRYPT_H as u16 * block_h;

        // Centre the (now block-scaled) crypt in whatever area is available,
        // so a wide window shows it framed rather than pinned to the corner.
        let ox = area.left() + (area.width().saturating_sub(footprint_w)) / 2;
        let oy = area.top() + (area.height().saturating_sub(footprint_h)) / 2;

        for y in 0..CRYPT_H {
            for x in 0..CRYPT_W {
                let is_visible = visible[(y * CRYPT_W + x) as usize];
                let cell = self.crypt.cell(x, y);

                let (color, dim) = match self.mode {
                    ViewMode::FovOnly => {
                        if is_visible {
                            (cell.color, false)
                        } else {
                            (rgb(0, 0, 0), true)
                        }
                    }
                    ViewMode::LightOnly => (lights.resolve(x, y, cell.color), false),
                    ViewMode::Combined => {
                        if is_visible {
                            (lights.resolve(x, y, cell.color), false)
                        } else {
                            // Lit-but-unseen still shows through as a glow: a
                            // torch behind a wall you can see brightens that
                            // wall, without revealing what is past it. Scaled
                            // down and desaturated toward the wall's own base
                            // tone so the effect is a glow on stone, not a
                            // window into the next room.
                            let glow = lights.resolve(x, y, cell.color);
                            (mix(rgb(6, 6, 9), glow, 0.55), true)
                        }
                    }
                };

                let glyph = if self.glyph_ramp && !dim {
                    let t = lights.luma(x, y);
                    // Below this threshold the masonry detail is unreadable
                    // anyway, so hand off to a density ramp that at least
                    // shows brightness through glyph shape as well as color.
                    if t < 0.35 { ascii_shade(t) } else { cell.glyph }
                } else if dim && self.mode != ViewMode::LightOnly {
                    // A visible-but-unlit or unseen-but-lit cell reads best as
                    // texture-free: showing the full masonry detail at near
                    // black is just noise, whereas a plain glyph reads as
                    // "shape, no detail", which is the correct claim to make.
                    if cell.wall { '#' } else { '.' }
                } else {
                    cell.glyph
                };

                // `color` is the *surface*, not a stroke: it goes in `bg` so
                // the whole cell reads as lit stone, with the masonry glyph
                // drawn as a darker (or, in deep shadow, a barely lighter)
                // mark on top of it for texture. Putting a bright resolved
                // light color only in `fg` was the original bug here: most of
                // this gallery's floor glyphs (`. , \` ' "`) cover only a
                // handful of pixels out of the cell, so the fill color never
                // reached the vast majority of each cell's area and a
                // brightly-lit room still rendered as almost entirely black.
                let ink = if dim {
                    // A pitch-black or near-black cell has nothing to darken
                    // toward, so the glyph would vanish; lightening instead
                    // keeps the same-shape detail visible as a faint mark.
                    mix(color, rgb(255, 255, 255), 0.22)
                } else {
                    mix(color, rgb(0, 0, 0), 0.35)
                };
                let style = Style::new().fg(ink).bg(color);

                // One logical cell becomes a `block_w x block_h` rectangle of
                // screen cells, all sharing this cell's color and glyph: the
                // same multi-cell-tile technique `02_chunky_tiles` uses, and
                // the only way a torch's falloff spans enough screen space to
                // read as a gradient rather than a handful of adjacent pixels.
                for by in 0..block_h {
                    for bx in 0..block_w {
                        let (sx, sy) = (ox + x as u16 * block_w + bx, oy + y as u16 * block_h + by);
                        if sx >= area.right() || sy >= area.bottom() {
                            continue;
                        }
                        surface.put((sx, sy), glyph, style);
                    }
                }
            }
        }

        Self::draw_player(surface, (ox, oy), (block_w, block_h), px, py, &lights);
    }

    /// Draws the player glyph at its interpolated position, lit by its own
    /// torch so it never goes fully dark even deep in shadow.
    ///
    /// Placed at the centre of its `block_w x block_h` footprint rather than
    /// at the block's top-left corner, so `@` sits visually inside the cell
    /// it occupies instead of pinned to one edge of it once a block covers
    /// more than one screen cell.
    ///
    /// `origin` and `block` are grouped into tuples (rather than four loose
    /// `u16`s) purely to stay under clippy's argument-count lint; see
    /// [`OrbSpec`] below for the same trick applied to a different draw call.
    fn draw_player(
        surface: &mut Surface<'_>,
        origin: (u16, u16),
        block: (u16, u16),
        px: f32,
        py: f32,
        lights: &LightMap,
    ) {
        let (ox, oy) = origin;
        let (block_w, block_h) = block;
        let (xi, yi) = (px.round() as i32, py.round() as i32);
        let (cx, cy) = (block_w / 2, block_h / 2);
        let (sx, sy) = (ox + xi as u16 * block_w + cx, oy + yi as u16 * block_h + cy);
        let color = lights.resolve(xi, yi, rgb(235, 225, 200));
        // A dark backing rather than the floor's own lit background: `@` is
        // a token, not a patch of surface, and needs to read as a distinct
        // thing standing on the floor rather than as one more textured tile.
        surface.put((sx, sy), '@', Style::new().fg(color).bg(rgb(10, 9, 12)));
    }

    fn status(&self) -> String {
        format!(
            "mode: {}  tone: {}  glyph ramp: {}",
            self.mode.label(),
            match self.tone {
                ToneMap::Reinhard => "reinhard (mixes)",
                ToneMap::Clamp => "clamp (clips to white)",
            },
            if self.glyph_ramp { "on" } else { "off" },
        )
    }

    /// Draws the bottom HUD: a skill hotbar (hidden below ~100 columns) and
    /// the health/mana orb pair, which are drawn regardless of width.
    fn draw_hud(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.height() == 0 {
            return;
        }

        let orb_width: u16 = 14;
        let (hotbar, orbs) = if area.width() > 100 {
            panel::split_right(area, orb_width * 2 + 2)
        } else {
            // Below this width there is no room for six framed slots and two
            // orbs together; the orbs matter more, since they are the
            // ever-present readout, so the hotbar is the one that gives way.
            let (rest, orbs) = panel::split_right(area, orb_width * 2 + 2);
            (Rect::new(rest.left(), rest.top(), 0, rest.height()), orbs)
        };

        if hotbar.width() > 0 {
            self.draw_hotbar(surface, hotbar);
        }
        self.draw_orbs(surface, orbs, orb_width);
    }

    /// A row of framed skill slots, each with a charge count and a cooldown
    /// shade that darkens with elapsed time since last use.
    fn draw_hotbar(&self, surface: &mut Surface<'_>, area: Rect) {
        const SKILLS: [(char, &str, u32); 4] = [
            ('/', "Slash", 3),
            ('*', "Nova", 1),
            ('+', "Heal", 2),
            ('^', "Ward", 1),
        ];
        let slots = panel::columns(area, SKILLS.len() as u16, 1);
        for (slot, (glyph, name, charges)) in slots.iter().zip(SKILLS) {
            // Each skill's cooldown phase is offset from the others so the
            // shading sweep is staggered, the same decorrelation principle
            // torch flicker uses: four bars ticking in lockstep would read as
            // one broken bar.
            let phase = f32::from(glyph as u8)
                .mul_add(0.37, self.time * 0.25)
                .fract();
            let cooling = phase < 0.4;
            let border = if cooling {
                panel::dimmed(panel::FRAME)
            } else {
                panel::FRAME
            };
            let inner = panel::Panel::new()
                .frame(border)
                .title(name)
                .draw(surface, *slot);
            if inner.height() == 0 {
                continue;
            }
            let icon_color = if cooling {
                panel::dimmed(ui::ACCENT)
            } else {
                ui::ACCENT
            };
            surface.put(
                (inner.left(), inner.top()),
                glyph,
                Style::new().fg(icon_color),
            );
            if inner.width() > 2 {
                let text = format!("x{charges}");
                surface.print(
                    (inner.left() + 2, inner.top()),
                    &text,
                    Style::new().fg(ui::DIM),
                );
            }
        }
    }

    /// The health/mana orb pair: two vertical gauges built from `▀▄█`, filling
    /// from the bottom by half-row steps.
    ///
    /// The vertical analogue of [`panel::bar`]'s half-cell horizontal
    /// precision: a cell can show empty, half-full (bottom half lit via `▄`
    /// with the fill color as its background), or full (`█`), which is three
    /// times the resolution a whole-glyph-per-row gauge would give in the same
    /// space.
    fn draw_orbs(&self, surface: &mut Surface<'_>, area: Rect, orb_width: u16) {
        if area.height() < 2 {
            return;
        }
        let cols = panel::columns(area, 2, 0);
        let health_color = panel::threshold(self.health);
        let mana_color = rgb(96, 150, 224);
        let track = rgb(28, 26, 36);

        Self::draw_orb(
            surface,
            cols[0],
            orb_width,
            self.health,
            &OrbSpec {
                fill: health_color,
                track,
                label: "HP",
            },
        );
        Self::draw_orb(
            surface,
            cols[1],
            orb_width,
            self.mana,
            &OrbSpec {
                fill: mana_color,
                track,
                label: "MP",
            },
        );
    }

    /// One vertical orb: `rows` tall, filling upward from the bottom.
    fn draw_orb(surface: &mut Surface<'_>, area: Rect, width: u16, t: f32, spec: &OrbSpec<'_>) {
        let &OrbSpec { fill, track, label } = spec;
        let w = width.min(area.width());
        let x0 = area.left() + (area.width().saturating_sub(w)) / 2;
        let rows = area.height();
        let t = t.clamp(0.0, 1.0);
        // Half-rows filled, bottom-up: two per row, same halving trick
        // `panel::bar` uses horizontally.
        let half_rows_filled = (f32::from(rows) * 2.0 * t).round() as u16;

        for row in 0..rows {
            // Row 0 is the top; the fill grows from the bottom, so measure
            // this row's distance from the bottom edge.
            let from_bottom = rows - 1 - row;
            let filled_here = half_rows_filled.saturating_sub(from_bottom * 2).min(2);
            let (glyph, fg, bg) = match filled_here {
                2 => ('\u{2588}', fill, fill),
                // The bottom half of the cell is lit: draw the lower-half
                // block in the fill color over the track background, which
                // reads as "half a row of orb" rather than as a stray glyph.
                1 => ('\u{2584}', fill, track),
                _ => (' ', fill, track),
            };
            for x in x0..x0 + w {
                surface.put((x, area.top() + row), glyph, Style::new().fg(fg).bg(bg));
            }
        }

        // Label centred over the orb's own footprint, one row above it if
        // there is room, otherwise overlaid on the top row.
        if area.top() > 0 {
            let lx = x0 + (w.saturating_sub(label.chars().count() as u16)) / 2;
            surface.print((lx, area.top() - 1), label, Style::new().fg(ui::DIM));
        }
    }
}

/// The fixed parameters of one orb draw, grouped so [`TorchlitCrypt::draw_orb`]
/// stays under the arity limit without losing named fields at the call site.
#[derive(Clone, Copy)]
struct OrbSpec<'a> {
    fill: Color,
    track: Color,
    label: &'a str,
}

/// A darkness-band glyph for luma `t` in `0.0..=1.0`, from `ASCII_RAMP`'s
/// darkest members.
///
/// Only the bottom of the ramp is used (space through `=`): the point is to
/// carry brightness through glyph density exactly in the band where color
/// alone is hardest to read, not to replace the masonry glyphs everywhere.
fn ascii_shade(t: f32) -> char {
    let scaled = (t / 0.35).clamp(0.0, 1.0);
    let idx = (scaled * 4.0).round() as usize;
    ASCII_RAMP[idx.min(4)]
}

impl Demo for TorchlitCrypt {
    const NAME: &'static str = "24_torchlit_crypt";
    const TITLE: &'static str = "24 Torchlit Crypt";
    const BLURB: &'static str =
        "Colored dynamic lighting, and why it isn't the same as field of view.";
    const GRID: (u16, u16) = (150, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("M", "cycle fov/light/both"),
            ("T", "toggle tone curve"),
            ("G", "toggle glyph ramp"),
            ("R", "reroll"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        // Health and mana drift so the orbs animate independently of the
        // patrol: a slow sine each, out of phase, so they do not empty and
        // fill in lockstep.
        self.health = 0.42f32.mul_add((self.time * 0.15).sin(), 0.5);
        self.mana = 0.42f32.mul_add(self.time.mul_add(0.11, 1.7).sin(), 0.5);

        let (title, content, status) = ui::split_chrome(term.area());
        let (map_area, hud_area) = panel::split_bottom(content, 3);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw_map(&mut surface, map_area);
        self.draw_hud(&mut surface, hud_area);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(TorchlitCrypt);

#[cfg(test)]
mod tests {
    use super::{Crypt, PATROL, TorchlitCrypt};

    #[test]
    fn every_patrol_waypoint_lands_on_open_floor() {
        let crypt = Crypt::generate(1);
        for &(x, y) in &PATROL {
            assert!(
                !crypt.blocks(x, y),
                "patrol waypoint ({x}, {y}) is inside a wall"
            );
        }
    }

    #[test]
    fn the_crypt_is_walled_on_every_edge() {
        let crypt = Crypt::generate(1);
        for x in 0..super::CRYPT_W {
            assert!(crypt.blocks(x, 0));
            assert!(crypt.blocks(x, super::CRYPT_H - 1));
        }
        for y in 0..super::CRYPT_H {
            assert!(crypt.blocks(0, y));
            assert!(crypt.blocks(super::CRYPT_W - 1, y));
        }
    }

    #[test]
    fn player_position_interpolates_smoothly_along_the_patrol_loop() {
        let mut demo = TorchlitCrypt::default();
        let start = demo.player_position();
        demo.time = 0.01;
        let moved = demo.player_position();
        assert_ne!(start, moved, "the player must move almost immediately");
    }

    #[test]
    fn view_mode_cycles_through_all_three_states() {
        use super::ViewMode;
        let a = ViewMode::FovOnly;
        let b = a.next();
        let c = b.next();
        let d = c.next();
        assert_eq!(b, ViewMode::LightOnly);
        assert_eq!(c, ViewMode::Combined);
        assert_eq!(d, ViewMode::FovOnly, "the cycle must return to the start");
    }

    #[test]
    fn ascii_shade_is_darkest_at_zero_and_lighter_above_the_threshold() {
        assert_eq!(super::ascii_shade(0.0), ' ');
        assert_ne!(super::ascii_shade(0.3), ' ');
    }
}
