//! Additive colored lighting over a grid, with flicker and tone mapping.
//!
//! Lighting is not field of view, and conflating them is the usual mistake.
//! [`fov`](crate::fov) answers "can the player see this cell"; this module
//! answers "how much light, of what color, falls on it". They are independent:
//! a torch two rooms away lights cells you cannot see, and a cell you can see
//! may be pitch dark. Modelling them separately is what buys the atmosphere in
//! Diablo and Brogue, where the dark is a thing you carry a lamp into rather
//! than a fog that clears as you walk.
//!
//! ## Why accumulate in floating point
//!
//! Two overlapping torches are brighter than one. If light is accumulated in
//! 8-bit color it clips at white, and a room with three braziers becomes a
//! white disc with colored edges: exactly the wrong result, since the overlap
//! is where the interesting color mixing should be. So [`LightMap`] sums in
//! `f32` with no ceiling and maps down only at the end, in [`resolve`], which
//! is the same reason a renderer keeps an HDR buffer.
//!
//! The tone curve is [`Reinhard`](ToneMap::Reinhard) by default: `x / (1 + x)`
//! compresses without ever quite reaching white, so a bright overlap stays
//! *colored* rather than blowing out. Clamping is available for the cases
//! where a hard ceiling reads better, but it is not the default because
//! clipping is what makes cheap lighting look cheap.

use alloc::vec;
use alloc::vec::Vec;

use retroglyph_core::Color;

use crate::noise::hash01;
use crate::palette::rgb;

extern crate alloc;

/// How light intensity falls off with distance.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Falloff {
    /// `1 - d/r`. A hard-edged cone; the boundary is visible as a ring.
    Linear,
    /// `(1 - d/r)^2`. The recommended curve at grid resolution: it leaves no
    /// visible edge, because the derivative goes to zero at the radius rather
    /// than stepping off a cliff.
    #[default]
    Quadratic,
    /// `1 / (1 + 2d + d^2)`, normalized. Physically motivated, and usually
    /// the wrong look at this scale: the centre is so much brighter than the
    /// surround that a torch reads as a single bright cell with a dim halo.
    InverseSquare,
}

impl Falloff {
    /// Attenuation at distance `d` for radius `r`, in `0.0..=1.0`.
    #[must_use]
    pub fn at(self, d: f32, r: f32) -> f32 {
        if r <= 0.0 || d >= r {
            return 0.0;
        }
        let t = (1.0 - d / r).clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Quadratic => t * t,
            Self::InverseSquare => {
                let raw = 1.0 / d.mul_add(2.0, d.mul_add(d, 1.0));
                // Rescaled so d == 0 is exactly 1.0 and the radius is still
                // the cutoff; the raw curve never reaches zero, which would
                // light the whole map faintly.
                (raw * t).clamp(0.0, 1.0)
            }
        }
    }
}

/// How accumulated light above 1.0 is brought back into displayable range.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ToneMap {
    /// `x / (1 + x)`. Asymptotic, so overlapping lights keep their hue.
    #[default]
    Reinhard,
    /// `min(x, 1)`. Cheaper, and clips overlaps to white.
    Clamp,
}

impl ToneMap {
    /// Maps an accumulated channel into `0.0..=1.0`.
    #[must_use]
    pub fn apply(self, x: f32) -> f32 {
        match self {
            Self::Reinhard => x / (1.0 + x),
            Self::Clamp => x.min(1.0),
        }
    }
}

/// A point light.
#[derive(Clone, Copy, Debug)]
pub struct Light {
    /// Position in grid cells.
    pub x: i32,
    /// Position in grid cells.
    pub y: i32,
    /// Radius in cells, before flicker.
    pub radius: f32,
    /// Peak intensity at the centre, before flicker. Values above 1.0 are
    /// useful: they widen the region that survives tone mapping at full
    /// strength, which is how a brazier reads as hotter than a candle rather
    /// than merely larger.
    pub intensity: f32,
    /// Light color.
    pub color: Color,
    /// Flicker depth in `0.0..=1.0`. Zero is a steady lamp.
    pub flicker: f32,
    /// Distinguishes this light's flicker phase from its neighbours'. Two
    /// torches sharing a seed pulse in unison, which reads as a fault in the
    /// renderer rather than as fire.
    pub seed: u32,
}

impl Light {
    /// A steady white light.
    #[must_use]
    pub const fn new(x: i32, y: i32, radius: f32, color: Color) -> Self {
        Self {
            x,
            y,
            radius,
            intensity: 1.0,
            color,
            flicker: 0.0,
            seed: 0,
        }
    }

    /// A warm flickering torch.
    #[must_use]
    pub const fn torch(x: i32, y: i32, radius: f32, seed: u32) -> Self {
        Self {
            x,
            y,
            radius,
            intensity: 1.15,
            color: rgb(255, 176, 92),
            flicker: 0.22,
            seed,
        }
    }

    /// Sets the peak intensity.
    #[must_use]
    pub const fn intensity(mut self, intensity: f32) -> Self {
        self.intensity = intensity;
        self
    }

    /// Sets the flicker depth and phase seed.
    #[must_use]
    pub const fn flicker(mut self, depth: f32, seed: u32) -> Self {
        self.flicker = depth;
        self.seed = seed;
        self
    }

    /// This light's radius and intensity at time `t`, with flicker applied.
    ///
    /// Two sine waves at incommensurable rates rather than one, plus a slow
    /// hash-driven drift. A single sine is recognisably periodic within a
    /// couple of seconds, which reads as a pulsing machine; summing rates that
    /// never re-align gives a signal with no audible period, which is what
    /// makes it read as fire. Radius and intensity are modulated together but
    /// out of phase, because a real flame gets both smaller and dimmer as it
    /// gutters, just not at exactly the same instant.
    #[must_use]
    pub fn at_time(&self, t: f32) -> (f32, f32) {
        if self.flicker <= 0.0 {
            return (self.radius, self.intensity);
        }
        let phase = hash01(self.seed, 1, 1) * core::f32::consts::TAU;
        let fast = t.mul_add(11.0, phase).sin();
        let slow = t.mul_add(4.3, phase * 1.7).sin();
        // Weighted so the fast component provides the shimmer and the slow one
        // the breathing, summing to at most 1.
        let wobble = fast.mul_add(0.35, slow * 0.65);
        let radius = self.radius * self.flicker.mul_add(wobble * 0.5, 1.0);
        let intensity = self.intensity * self.flicker.mul_add(wobble, 1.0);
        (radius.max(0.0), intensity.max(0.0))
    }
}

/// An accumulation buffer of linear light, one RGB triple per cell.
#[derive(Clone, Debug)]
pub struct LightMap {
    width: i32,
    height: i32,
    /// Interleaved RGB, in linear units with no ceiling.
    light: Vec<f32>,
    ambient: [f32; 3],
    falloff: Falloff,
    tone: ToneMap,
}

impl LightMap {
    /// A map of `width` x `height` cells, cleared to `ambient`.
    ///
    /// Ambient is not zero in most rooms: a pitch-black cell shows nothing at
    /// all, so a dungeon lit only by torches loses its own architecture
    /// between the pools. A small ambient term keeps walls readable while
    /// still making the torchlight the thing you navigate by.
    #[must_use]
    pub fn new(width: i32, height: i32, ambient: Color) -> Self {
        Self {
            width: width.max(0),
            height: height.max(0),
            light: vec![0.0; (width.max(0) * height.max(0) * 3) as usize],
            ambient: channels(ambient),
            falloff: Falloff::default(),
            tone: ToneMap::default(),
        }
    }

    /// Sets the falloff curve.
    #[must_use]
    pub const fn falloff(mut self, falloff: Falloff) -> Self {
        self.falloff = falloff;
        self
    }

    /// Sets the tone curve.
    #[must_use]
    pub const fn tone_map(mut self, tone: ToneMap) -> Self {
        self.tone = tone;
        self
    }

    /// Grid size in cells.
    #[must_use]
    pub const fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    /// Clears every cell back to darkness, keeping the configured ambient.
    ///
    /// Cheaper than rebuilding the map each frame, which matters because a
    /// flickering light has to be re-accumulated every frame by definition.
    pub fn clear(&mut self) {
        self.light.fill(0.0);
    }

    /// Adds one light's contribution at time `t`.
    ///
    /// Only the light's own bounding square is visited, so cost scales with
    /// the lights rather than with the map: a hundred candles in a large cave
    /// is a hundred small loops, not a hundred full-map passes.
    pub fn add(&mut self, light: &Light, t: f32) {
        let (radius, intensity) = light.at_time(t);
        if radius <= 0.0 || intensity <= 0.0 {
            return;
        }
        let tint = channels(light.color);
        let reach = radius.ceil() as i32;

        for y in (light.y - reach).max(0)..=(light.y + reach).min(self.height - 1) {
            for x in (light.x - reach).max(0)..=(light.x + reach).min(self.width - 1) {
                // Cells are twice as tall as wide, so a circle in cell space
                // is an ellipse on screen. Scaling y by the aspect makes a
                // torch pool look round rather than like a vertical slot.
                let dx = (x - light.x) as f32;
                let dy = (y - light.y) as f32 * crate::palette::CELL_ASPECT;
                let amount = self.falloff.at(dx.hypot(dy), radius) * intensity;
                if amount <= 0.0 {
                    continue;
                }
                let at = ((y * self.width + x) * 3) as usize;
                for (channel, level) in tint.iter().enumerate() {
                    self.light[at + channel] += level * amount;
                }
            }
        }
    }

    /// Adds every light in `lights` at time `t`.
    pub fn add_all(&mut self, lights: &[Light], t: f32) {
        for light in lights {
            self.add(light, t);
        }
    }

    /// The accumulated (pre-tone-map) light at a cell, or ambient outside.
    #[must_use]
    pub fn raw(&self, x: i32, y: i32) -> [f32; 3] {
        let Some(i) = self.index(x, y) else {
            return self.ambient;
        };
        [
            self.light[i] + self.ambient[0],
            self.light[i + 1] + self.ambient[1],
            self.light[i + 2] + self.ambient[2],
        ]
    }

    /// Applies `base` to the light at a cell, returning the color to draw.
    ///
    /// Multiplicative, then tone-mapped: light *reveals* a surface's own color
    /// rather than replacing it, so a blue tile under a warm torch goes a
    /// muted green-grey exactly as it should, instead of turning orange. This
    /// is the single decision that most separates lighting that looks like
    /// lighting from lighting that looks like a colored overlay.
    #[must_use]
    pub fn resolve(&self, x: i32, y: i32, base: Color) -> Color {
        let l = self.raw(x, y);
        let surface = channels(base);
        rgb(
            to_byte(self.tone.apply(surface[0] * l[0])),
            to_byte(self.tone.apply(surface[1] * l[1])),
            to_byte(self.tone.apply(surface[2] * l[2])),
        )
    }

    /// Perceived brightness at a cell in `0.0..=1.0`, after tone mapping.
    ///
    /// For picking a glyph from a density ramp, where only the magnitude
    /// matters. Rec. 601 luma weights, because equal-energy channels are not
    /// equally bright to the eye and a green light would otherwise pick a
    /// denser glyph than a blue one of the same intensity.
    #[must_use]
    pub fn luma(&self, x: i32, y: i32) -> f32 {
        let l = self.raw(x, y);
        let lit = [
            self.tone.apply(l[0]),
            self.tone.apply(l[1]),
            self.tone.apply(l[2]),
        ];
        0.114f32.mul_add(lit[2], 0.299f32.mul_add(lit[0], 0.587 * lit[1]))
    }

    /// The buffer index of a cell, or `None` if out of bounds.
    const fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        Some(((y * self.width + x) * 3) as usize)
    }
}

/// Splits a color into linear `0.0..=1.0` channels.
///
/// Non-RGB colors resolve to mid-grey rather than to black: a palette color
/// arriving here means the caller mixed indexed and true color, and silently
/// blacking out that cell hides the mistake in the least debuggable way.
fn channels(color: Color) -> [f32; 3] {
    match color {
        Color::Rgb { r, g, b } => [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
        ],
        _ => [0.5, 0.5, 0.5],
    }
}

/// Converts a `0.0..=1.0` channel back to a byte.
fn to_byte(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::{Falloff, Light, LightMap, ToneMap};
    use crate::palette::rgb;

    const BLACK: retroglyph_core::Color = rgb(0, 0, 0);
    const WHITE: retroglyph_core::Color = rgb(255, 255, 255);

    #[test]
    fn falloff_is_full_at_the_centre_and_zero_at_the_radius() {
        for curve in [Falloff::Linear, Falloff::Quadratic, Falloff::InverseSquare] {
            assert!((curve.at(0.0, 8.0) - 1.0).abs() < 1e-6, "{curve:?} centre");
            assert!(curve.at(8.0, 8.0).abs() < 1e-6, "{curve:?} at the radius");
            assert!(curve.at(9.0, 8.0).abs() < 1e-6, "{curve:?} past the radius");
        }
    }

    #[test]
    fn falloff_decreases_monotonically() {
        for curve in [Falloff::Linear, Falloff::Quadratic, Falloff::InverseSquare] {
            let mut last = f32::INFINITY;
            for step in 0..=16 {
                let a = curve.at(step as f32 * 0.5, 8.0);
                assert!(a <= last + 1e-6, "{curve:?} rose at {step}");
                last = a;
            }
        }
    }

    #[test]
    fn quadratic_falls_faster_than_linear_in_the_midrange() {
        assert!(Falloff::Quadratic.at(4.0, 8.0) < Falloff::Linear.at(4.0, 8.0));
    }

    #[test]
    fn a_zero_radius_light_lights_nothing() {
        assert!(Falloff::Quadratic.at(0.0, 0.0).abs() < 1e-6);
    }

    #[test]
    fn reinhard_never_reaches_white_but_clamping_does() {
        assert!(ToneMap::Reinhard.apply(1000.0) < 1.0);
        assert!((ToneMap::Clamp.apply(1000.0) - 1.0).abs() < 1e-6);
        // Both agree that nothing is nothing.
        assert!(ToneMap::Reinhard.apply(0.0).abs() < 1e-6);
        assert!(ToneMap::Clamp.apply(0.0).abs() < 1e-6);
    }

    #[test]
    fn overlapping_lights_are_brighter_than_either_alone() {
        let mut map = LightMap::new(16, 16, BLACK);
        let a = Light::new(6, 8, 6.0, WHITE);
        let b = Light::new(10, 8, 6.0, WHITE);

        map.add(&a, 0.0);
        let one = map.luma(8, 8);
        map.add(&b, 0.0);
        let both = map.luma(8, 8);

        assert!(one > 0.0, "the first light reached the midpoint");
        assert!(both > one, "the second light did not accumulate");
    }

    #[test]
    fn overlapping_colored_lights_keep_their_hue_under_reinhard() {
        let mut map = LightMap::new(8, 8, BLACK);
        // Three strong red lights stacked on one cell.
        for _ in 0..3 {
            map.add(&Light::new(4, 4, 4.0, rgb(255, 0, 0)).intensity(3.0), 0.0);
        }
        let out = map.resolve(4, 4, WHITE);
        let retroglyph_core::Color::Rgb { r, g, b } = out else {
            panic!("expected rgb");
        };
        assert!(r > 200, "the red channel should be strong, got {r}");
        assert_eq!(
            (g, b),
            (0, 0),
            "clipping to white would have raised g and b"
        );
    }

    #[test]
    fn light_multiplies_the_surface_colour_rather_than_replacing_it() {
        let mut map = LightMap::new(8, 8, BLACK);
        // A pure blue surface under a pure red light reflects nothing.
        map.add(&Light::new(4, 4, 6.0, rgb(255, 0, 0)).intensity(4.0), 0.0);
        let out = map.resolve(4, 4, rgb(0, 0, 255));
        assert_eq!(
            out,
            rgb(0, 0, 0),
            "a red light cannot make a blue tile glow"
        );
    }

    #[test]
    fn ambient_keeps_unlit_cells_readable() {
        let dark = LightMap::new(8, 8, BLACK);
        let dim = LightMap::new(8, 8, rgb(40, 40, 50));
        assert_eq!(dark.resolve(0, 0, WHITE), rgb(0, 0, 0));
        assert!(
            dim.luma(0, 0) > 0.0,
            "an ambient term must survive to unlit cells"
        );
    }

    #[test]
    fn clear_removes_lights_but_keeps_ambient() {
        let mut map = LightMap::new(8, 8, rgb(20, 20, 20));
        map.add(&Light::new(4, 4, 5.0, WHITE).intensity(2.0), 0.0);
        let lit = map.luma(4, 4);
        map.clear();
        let cleared = map.luma(4, 4);
        assert!(cleared < lit);
        assert!(cleared > 0.0, "ambient was cleared too");
    }

    #[test]
    fn out_of_bounds_reads_return_ambient_rather_than_panicking() {
        let map = LightMap::new(8, 8, rgb(10, 20, 30));
        for (x, y) in [(-1, 0), (0, -1), (8, 0), (0, 8), (99, 99)] {
            let _ = map.resolve(x, y, WHITE);
            assert!(map.luma(x, y) > 0.0, "({x}, {y})");
        }
    }

    #[test]
    fn a_light_outside_the_map_still_spills_onto_it() {
        let mut map = LightMap::new(8, 8, BLACK);
        map.add(&Light::new(-2, 4, 6.0, WHITE).intensity(2.0), 0.0);
        assert!(
            map.luma(0, 4) > 0.0,
            "the visible part of the pool was clipped away"
        );
    }

    #[test]
    fn a_torch_pool_is_round_on_screen_not_in_cell_space() {
        let mut map = LightMap::new(21, 21, BLACK);
        map.add(&Light::new(10, 10, 8.0, WHITE).intensity(2.0), 0.0);
        // Cells are twice as tall as wide, so the pool must reach twice as far
        // horizontally as vertically to look circular.
        assert!(map.luma(16, 10) > 0.0, "6 cells east should be lit");
        assert!(
            map.luma(10, 16).abs() < 1e-6,
            "6 cells south should not be: that is 12 cells of screen distance"
        );
    }

    #[test]
    fn a_steady_light_does_not_flicker() {
        let light = Light::new(0, 0, 6.0, WHITE);
        let (r0, i0) = light.at_time(0.0);
        let (r1, i1) = light.at_time(3.7);
        assert!((r0 - r1).abs() < 1e-6 && (i0 - i1).abs() < 1e-6);
    }

    #[test]
    fn a_torch_varies_over_time_but_stays_positive() {
        let torch = Light::torch(0, 0, 8.0, 7);
        let mut seen = Vec::new();
        for step in 0..200 {
            let (r, i) = torch.at_time(step as f32 * 0.05);
            assert!(r >= 0.0 && i >= 0.0, "flicker went negative at {step}");
            assert!(r < 8.0 * 2.0, "flicker exploded at {step}");
            seen.push(r);
        }
        let min = seen.iter().copied().fold(f32::INFINITY, f32::min);
        let max = seen.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max - min > 0.3,
            "a torch that never varies is not flickering"
        );
    }

    #[test]
    fn two_torches_with_different_seeds_do_not_pulse_in_unison() {
        let a = Light::torch(0, 0, 8.0, 1);
        let b = Light::torch(5, 0, 8.0, 2);
        let differ = (0..64)
            .map(|s| s as f32 * 0.05)
            .filter(|&t| (a.at_time(t).0 - b.at_time(t).0).abs() > 0.05)
            .count();
        assert!(
            differ > 40,
            "seeds did not decorrelate the phase ({differ}/64)"
        );
    }

    #[test]
    fn luma_weights_green_above_blue() {
        let mut green = LightMap::new(4, 4, BLACK);
        let mut blue = LightMap::new(4, 4, BLACK);
        green.add(&Light::new(2, 2, 4.0, rgb(0, 255, 0)), 0.0);
        blue.add(&Light::new(2, 2, 4.0, rgb(0, 0, 255)), 0.0);
        assert!(green.luma(2, 2) > blue.luma(2, 2));
    }
}
