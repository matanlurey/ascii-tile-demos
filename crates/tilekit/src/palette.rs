//! Color ramps, biome palettes, and the tint passes that restyle a whole map
//! without touching its terrain data.
//!
//! The organising idea is that a map's *data* (elevation, biome, ownership)
//! and its *look* (parchment, night, autumn) are independent. A demo generates
//! the world once and then runs it through one of these passes, which is why
//! the seasons/day-night demo can cycle through six completely different
//! moods without regenerating anything.

use retroglyph_core::Color;

/// Shorthand for an opaque 24-bit color.
#[must_use]
pub const fn rgb(red: u8, green: u8, blue: u8) -> Color {
    Color::Rgb {
        r: red,
        g: green,
        b: blue,
    }
}

/// True RGB black.
///
/// Not [`Color::BLACK`], which is `Ansi(Black)`: an ANSI color resolves to
/// whatever the terminal's palette says, so a blend against it is only as
/// predictable as the user's theme. Every blend in this crate wants an exact
/// value.
pub const BLACK: Color = rgb(0, 0, 0);
/// True RGB white. See [`BLACK`].
pub const WHITE: Color = rgb(255, 255, 255);

/// Blends `a` toward `b` by `t`, clamping `t` and resolving non-RGB inputs.
///
/// Overlaps [`Color::lerp`], which resolves non-`Rgb` inputs the same way, and
/// exists for the two things it adds. `t` is clamped to `0.0..=1.0`, so an
/// unclamped animation parameter cannot extrapolate past either endpoint into a
/// wrapped channel; and the blend is per-channel in sRGB space with no
/// dependency on `retroglyph-core`'s optional `gem` feature, which is what
/// `Color::lerp` is gated behind.
#[must_use]
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    // The fallbacks only matter for `Color::Default`, which has no intrinsic
    // value; black for the source and white for the destination keeps a blend
    // against an unset color from silently darkening or brightening the whole
    // map depending on which side it landed on.
    let (ar, ag, ab) = a.resolve_rgb((0, 0, 0));
    let (br, bg, bb) = b.resolve_rgb((255, 255, 255));
    let lerp = |from: u8, to: u8| {
        let from = f32::from(from);
        (f32::from(to) - from).mul_add(t, from).round() as u8
    };
    rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

/// Scales a color's brightness by `factor`, clamped at white.
///
/// The operation a hillshade wants: `mix(BLACK, color, shade)` darkens toward
/// black but can never brighten, while this can do both, so a sunlit slope
/// reads as genuinely lit rather than merely less shadowed.
#[must_use]
pub fn scale(color: Color, factor: f32) -> Color {
    let factor = factor.max(0.0);
    let (r, g, b) = color.resolve_rgb((0, 0, 0));
    let apply = |c: u8| (f32::from(c) * factor).clamp(0.0, 255.0) as u8;
    rgb(apply(r), apply(g), apply(b))
}

/// A color ramp: stops at increasing positions in `0.0..=1.0`, linearly
/// interpolated between.
///
/// Deliberately a slice of pairs rather than a fixed-size array, so a ramp can
/// be a `const` of any length and callers can define their own inline.
#[derive(Debug, Clone, Copy)]
pub struct Ramp<'a>(pub &'a [(f32, Color)]);

impl Ramp<'_> {
    /// Samples the ramp at `t`, clamped to `0.0..=1.0`.
    ///
    /// Returns [`Color::Default`] for an empty ramp rather than panicking: a
    /// missing ramp should render as "unstyled", not take down the frame.
    #[must_use]
    pub fn sample(&self, t: f32) -> Color {
        let stops = self.0;
        match stops {
            [] => Color::Default,
            [(_, only)] => *only,
            _ => {
                let t = t.clamp(0.0, 1.0);
                // Linear scan: ramps here are at most a dozen stops, so a
                // binary search would cost more in branch misprediction than
                // it saves in comparisons.
                for pair in stops.windows(2) {
                    let ((low_pos, low_color), (high_pos, high_color)) = (pair[0], pair[1]);
                    if t <= high_pos {
                        let span = high_pos - low_pos;
                        let local = if span > 0.0 {
                            (t - low_pos) / span
                        } else {
                            0.0
                        };
                        return mix(low_color, high_color, local);
                    }
                }
                stops[stops.len() - 1].1
            }
        }
    }
}

/// Elevation ramp for a physical/relief map: ocean depths through shelf,
/// lowland green, upland tan, rock, and snow.
///
/// The stop positions matter more than the colors: the jump from shelf blue to
/// lowland green at 0.42 is the coastline, and putting it at a *sharp* stop
/// (0.40 to 0.42) rather than a gradual one is what makes the shore read as a
/// line instead of a smear.
pub const ELEVATION: Ramp<'static> = Ramp(&[
    (0.00, rgb(8, 18, 44)),
    (0.28, rgb(16, 44, 86)),
    (0.40, rgb(38, 84, 128)),
    (0.42, rgb(198, 184, 132)),
    (0.46, rgb(96, 132, 68)),
    (0.60, rgb(78, 110, 54)),
    (0.72, rgb(126, 116, 82)),
    (0.84, rgb(108, 100, 96)),
    (0.93, rgb(150, 146, 148)),
    (1.00, rgb(238, 240, 246)),
]);

/// A grayscale ramp, for hillshade and any single-channel overlay.
pub const GRAYSCALE: Ramp<'static> = Ramp(&[(0.0, rgb(0, 0, 0)), (1.0, rgb(255, 255, 255))]);

/// Sepia ink on aged paper, for the cartography style.
pub const PARCHMENT: Ramp<'static> = Ramp(&[
    (0.00, rgb(58, 42, 28)),
    (0.50, rgb(146, 116, 78)),
    (1.00, rgb(226, 206, 168)),
]);

/// Ocean depth, shallow to abyssal.
pub const OCEAN: Ramp<'static> = Ramp(&[
    (0.0, rgb(6, 14, 36)),
    (0.6, rgb(18, 52, 96)),
    (1.0, rgb(58, 116, 158)),
]);

/// Heat: cold blue through temperate green to hot red. For temperature
/// overlays and any diverging data.
pub const HEAT: Ramp<'static> = Ramp(&[
    (0.00, rgb(56, 96, 176)),
    (0.35, rgb(96, 168, 176)),
    (0.55, rgb(140, 168, 84)),
    (0.75, rgb(206, 152, 62)),
    (1.00, rgb(186, 68, 48)),
]);

/// Distinct hues for political factions.
///
/// Chosen to stay distinguishable after the desaturation that fog of war and
/// the "not your territory" dimming apply, which rules out the obvious
/// saturated primaries: a pure red and a pure orange become the same color
/// once both are pulled 40% toward the background.
pub const FACTIONS: [Color; 8] = [
    rgb(206, 84, 78),
    rgb(74, 130, 196),
    rgb(120, 172, 88),
    rgb(200, 158, 66),
    rgb(150, 104, 186),
    rgb(80, 172, 164),
    rgb(206, 122, 168),
    rgb(140, 138, 152),
];

/// The faction color for `index`, wrapping so any number of factions works.
#[must_use]
pub const fn faction(index: usize) -> Color {
    FACTIONS[index % FACTIONS.len()]
}

/// Time of day, for the tint pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeOfDay {
    /// Low warm light from the east.
    Dawn,
    /// Neutral overhead light. The identity tint.
    #[default]
    Noon,
    /// Low warm light from the west, redder than dawn.
    Dusk,
    /// Dim, cold, blue.
    Night,
}

impl TimeOfDay {
    /// All four, in cycle order.
    pub const ALL: [Self; 4] = [Self::Dawn, Self::Noon, Self::Dusk, Self::Night];

    /// The next phase, wrapping.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Dawn => Self::Noon,
            Self::Noon => Self::Dusk,
            Self::Dusk => Self::Night,
            Self::Night => Self::Dawn,
        }
    }

    /// Display name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dawn => "dawn",
            Self::Noon => "noon",
            Self::Dusk => "dusk",
            Self::Night => "night",
        }
    }

    /// The `(tint, strength)` this phase applies.
    ///
    /// Noon is the identity (strength 0), which is the point: the base
    /// palettes are authored *as* noon, so every other phase is a departure
    /// from them rather than all four being separately tuned.
    #[must_use]
    pub const fn tint(self) -> (Color, f32) {
        match self {
            Self::Dawn => (rgb(255, 186, 138), 0.22),
            Self::Noon => (rgb(255, 255, 255), 0.0),
            Self::Dusk => (rgb(232, 126, 96), 0.28),
            Self::Night => (rgb(38, 54, 110), 0.52),
        }
    }
}

/// Season, for the tint pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Season {
    /// Fresh, light green.
    #[default]
    Spring,
    /// Deep green; nearly the identity.
    Summer,
    /// Warm orange.
    Autumn,
    /// Washed toward snow.
    Winter,
}

impl Season {
    /// All four, in cycle order.
    pub const ALL: [Self; 4] = [Self::Spring, Self::Summer, Self::Autumn, Self::Winter];

    /// The next season, wrapping.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Spring => Self::Summer,
            Self::Summer => Self::Autumn,
            Self::Autumn => Self::Winter,
            Self::Winter => Self::Spring,
        }
    }

    /// Display name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Spring => "spring",
            Self::Summer => "summer",
            Self::Autumn => "autumn",
            Self::Winter => "winter",
        }
    }

    /// The `(tint, strength)` this season applies to vegetated terrain.
    #[must_use]
    pub const fn tint(self) -> (Color, f32) {
        match self {
            Self::Spring => (rgb(150, 216, 120), 0.16),
            Self::Summer => (rgb(120, 180, 70), 0.06),
            Self::Autumn => (rgb(212, 138, 54), 0.34),
            Self::Winter => (rgb(226, 234, 246), 0.46),
        }
    }
}

/// Applies a `(tint, strength)` pair to a color.
#[must_use]
pub fn apply_tint(base: Color, tint: (Color, f32)) -> Color {
    let (color, strength) = tint;
    if strength <= 0.0 {
        return base;
    }
    mix(base, color, strength)
}

/// Lambertian hillshade: the cosine between a surface normal and a light
/// direction, clamped to `0.0..=1.0` where 1 is fully lit.
///
/// `dzdx` and `dzdy` are local slopes in height units per cell, in *screen*
/// axes: `x` increases east, `y` increases **south** (down the screen), which
/// is the convention a row-major heightmap already uses. `azimuth` is the
/// compass bearing the light comes *from*, in radians measured clockwise from
/// north; `altitude` is its angle above the horizon.
///
/// Stated as a normal-dot-light rather than ported from GIS slope/aspect
/// formulas, because those are written for north-up rasters where `y`
/// increases north, and silently flipping that sign inverts every hill on
/// screen into a crater.
///
/// The northwest default is not arbitrary either: cartographic convention
/// lights relief from the upper left because the visual system reads top-lit
/// surfaces as convex. Light a map from the south and the relief inverts, an
/// illusion strong enough that it survives knowing about it.
#[must_use]
pub fn hillshade(slope_x: f32, slope_y: f32, azimuth: f32, altitude: f32) -> f32 {
    // Surface normal of z = f(x, y), unnormalized: (-dz/dx, -dz/dy, 1).
    let (norm_x, norm_y, norm_z) = (-slope_x, -slope_y, 1.0);
    let inv_len = norm_x
        .mul_add(norm_x, norm_y.mul_add(norm_y, norm_z * norm_z))
        .sqrt()
        .recip();

    // Light vector pointing from the surface toward the sun. Bearing is
    // clockwise from north, and north is -y on screen, so the horizontal
    // components are (sin, -cos) rather than the usual (cos, sin).
    let horizontal = altitude.cos();
    let (light_x, light_y, light_z) = (
        horizontal * azimuth.sin(),
        -horizontal * azimuth.cos(),
        altitude.sin(),
    );

    let dot = norm_x.mul_add(light_x, norm_y.mul_add(light_y, norm_z * light_z));
    (dot * inv_len).clamp(0.0, 1.0)
}

/// Azimuth of the conventional cartographic sun: northwest, i.e. a bearing of
/// 315 degrees clockwise from north.
pub const SUN_NW: f32 = 7.0 * core::f32::consts::FRAC_PI_4;
/// Altitude of the conventional cartographic sun: 45 degrees above horizon.
pub const SUN_ALTITUDE: f32 = core::f32::consts::FRAC_PI_4;

/// [`hillshade`] with the conventional northwest sun at 45 degrees elevation.
#[must_use]
pub fn hillshade_nw(slope_x: f32, slope_y: f32) -> f32 {
    hillshade(slope_x, slope_y, SUN_NW, SUN_ALTITUDE)
}

/// Aspect ratio of a character cell: how many times taller than wide.
///
/// Not a universal constant (it depends on the font), but 2 is right for the
/// embedded 8x16 bitmap font and close enough for every common terminal font.
pub const CELL_ASPECT: f32 = 2.0;

/// [`hillshade_nw`] corrected for the aspect of a character cell.
///
/// Terrain gradients come out of a heightmap in *world cells per world cell*,
/// but a character cell is about twice as tall as it is wide, so one cell of
/// north-south distance covers twice as much screen as one cell of east-west
/// distance. Feeding raw gradients to [`hillshade`] therefore overstates every
/// north-south slope by a factor of two, and the map develops vertical
/// streaks: ridges that run north-south look like cliffs while identical
/// ridges running east-west look like gentle swells.
///
/// Dividing the y gradient by [`CELL_ASPECT`] expresses both slopes in the
/// same screen units, which is what the lighting model assumes.
#[must_use]
pub fn hillshade_cells(slope_x: f32, slope_y: f32) -> f32 {
    hillshade_nw(slope_x, slope_y / CELL_ASPECT)
}

/// Darkens `base` toward `bg` for a tile that has been seen but is not
/// currently visible.
///
/// Fog of war has to keep terrain *legible* while making it obviously stale.
/// Pulling toward the background (rather than desaturating, or dropping alpha)
/// preserves the hue relationships that let you still tell forest from desert
/// at a glance, which is the whole point of remembering the tile at all.
#[must_use]
pub fn remembered(base: Color, bg: Color) -> Color {
    mix(base, bg, 0.62)
}

/// The color of never-explored map.
#[must_use]
pub fn unexplored(bg: Color) -> Color {
    mix(bg, BLACK, 0.45)
}

#[cfg(test)]
mod tests {
    use super::{
        BLACK, ELEVATION, FACTIONS, GRAYSCALE, HEAT, OCEAN, PARCHMENT, Ramp, SUN_ALTITUDE, Season,
        TimeOfDay, WHITE, apply_tint, faction, hillshade, hillshade_cells, hillshade_nw, mix,
        remembered, rgb, scale, unexplored,
    };
    use retroglyph_core::{AnsiColor, Color};

    fn channels(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb { r, g, b } => (r, g, b),
            other => panic!("expected rgb, got {other:?}"),
        }
    }

    #[test]
    fn mix_resolves_a_non_rgb_endpoint_instead_of_skipping_the_blend() {
        // Blending against an `Ansi`/`Indexed`/`Default` color used to return
        // the first argument unchanged, so a hillshade written against
        // `Color::BLACK` (which is `Ansi(Black)`) rendered every cell black.
        // Both this and `Color::lerp` resolve first now; this pins the
        // behaviour rather than the historical bug.
        let ansi_black = Color::Ansi(AnsiColor::Black);
        let target = rgb(200, 100, 50);
        assert_eq!(channels(mix(ansi_black, target, 1.0)), (200, 100, 50));
        assert_eq!(channels(mix(ansi_black, target, 0.0)), (0, 0, 0));
    }

    #[test]
    fn mix_clamps_t_where_color_lerp_extrapolates() {
        // The remaining reason to prefer `mix`: it is total over any `t`.
        let (a, b) = (rgb(10, 20, 30), rgb(210, 220, 230));
        assert_eq!(channels(mix(a, b, 2.0)), channels(b));
        assert_eq!(channels(mix(a, b, -1.0)), channels(a));
    }

    #[test]
    fn mix_hits_both_endpoints_and_the_midpoint() {
        let (a, b) = (rgb(0, 0, 0), rgb(100, 200, 40));
        assert_eq!(channels(mix(a, b, 0.0)), (0, 0, 0));
        assert_eq!(channels(mix(a, b, 1.0)), (100, 200, 40));
        assert_eq!(channels(mix(a, b, 0.5)), (50, 100, 20));
    }

    #[test]
    fn mix_clamps_out_of_range_t() {
        let (a, b) = (rgb(10, 10, 10), rgb(200, 200, 200));
        assert_eq!(channels(mix(a, b, -4.0)), (10, 10, 10));
        assert_eq!(channels(mix(a, b, 4.0)), (200, 200, 200));
    }

    #[test]
    fn black_and_white_constants_are_real_rgb() {
        // If these were the ANSI constants, every blend against them would be
        // a silent no-op. Assert the variant, not just the value.
        assert!(matches!(BLACK, Color::Rgb { .. }));
        assert!(matches!(WHITE, Color::Rgb { .. }));
        assert_eq!(channels(BLACK), (0, 0, 0));
        assert_eq!(channels(WHITE), (255, 255, 255));
    }

    #[test]
    fn scale_dims_and_brightens_and_clamps() {
        let base = rgb(100, 50, 25);
        assert_eq!(channels(scale(base, 0.0)), (0, 0, 0));
        assert_eq!(channels(scale(base, 1.0)), (100, 50, 25));
        assert_eq!(channels(scale(base, 2.0)), (200, 100, 50));
        assert_eq!(channels(scale(base, 100.0)), (255, 255, 255), "clamped");
        assert_eq!(channels(scale(base, -5.0)), (0, 0, 0), "negatives floor");
    }

    #[test]
    fn ramp_hits_its_endpoints_exactly() {
        assert_eq!(channels(GRAYSCALE.sample(0.0)), (0, 0, 0));
        assert_eq!(channels(GRAYSCALE.sample(1.0)), (255, 255, 255));
    }

    #[test]
    fn ramp_clamps_out_of_range_input() {
        assert_eq!(channels(GRAYSCALE.sample(-5.0)), (0, 0, 0));
        assert_eq!(channels(GRAYSCALE.sample(9.0)), (255, 255, 255));
    }

    #[test]
    fn ramp_interpolates_monotonically() {
        let mut last = 0u16;
        for i in 0..=20 {
            let (r, _, _) = channels(GRAYSCALE.sample(i as f32 / 20.0));
            assert!(u16::from(r) >= last, "ramp went backwards at {i}");
            last = u16::from(r);
        }
    }

    #[test]
    fn degenerate_ramps_do_not_panic() {
        assert_eq!(Ramp(&[]).sample(0.5), Color::Default);
        let single = Ramp(&[(0.3, rgb(1, 2, 3))]);
        assert_eq!(channels(single.sample(0.0)), (1, 2, 3));
        assert_eq!(channels(single.sample(1.0)), (1, 2, 3));
        // Two stops at the same position must not divide by zero.
        let flat = Ramp(&[(0.5, rgb(0, 0, 0)), (0.5, rgb(255, 255, 255))]);
        assert!(matches!(flat.sample(0.5), Color::Rgb { .. }));
    }

    #[test]
    fn every_named_ramp_is_sampleable_across_its_range() {
        for ramp in [ELEVATION, PARCHMENT, OCEAN, HEAT, GRAYSCALE] {
            for i in 0..=10 {
                assert!(matches!(ramp.sample(i as f32 / 10.0), Color::Rgb { .. }));
            }
        }
    }

    #[test]
    fn elevation_ramp_puts_a_hard_edge_at_the_coast() {
        // Just below the 0.42 stop must still be water-blue; just above must
        // be beach-tan. This is the ramp's single most visible property.
        let (_, _, below_b) = channels(ELEVATION.sample(0.40));
        let (above_r, _, above_b) = channels(ELEVATION.sample(0.43));
        assert!(below_b > 100, "shelf should be blue");
        assert!(above_r > above_b, "beach should be warm");
    }

    #[test]
    fn faction_colors_wrap_and_are_distinct() {
        assert_eq!(faction(0), FACTIONS[0]);
        assert_eq!(faction(FACTIONS.len()), FACTIONS[0]);
        assert_eq!(faction(FACTIONS.len() * 3 + 2), FACTIONS[2]);
        for (i, &a) in FACTIONS.iter().enumerate() {
            for &b in &FACTIONS[i + 1..] {
                assert_ne!(a, b, "duplicate faction color");
            }
        }
    }

    #[test]
    fn noon_and_zero_strength_tints_are_the_identity() {
        let base = rgb(100, 120, 140);
        assert_eq!(apply_tint(base, TimeOfDay::Noon.tint()), base);
        assert_eq!(apply_tint(base, (rgb(255, 0, 0), 0.0)), base);
    }

    #[test]
    fn night_darkens_and_cools() {
        let base = rgb(180, 180, 180);
        let (r, _, b) = channels(apply_tint(base, TimeOfDay::Night.tint()));
        assert!(r < 180, "night should darken");
        assert!(b > r, "night should cool");
    }

    #[test]
    fn winter_lightens_vegetation() {
        let green = rgb(80, 140, 60);
        let (r, _, b) = channels(apply_tint(green, Season::Winter.tint()));
        assert!(r > 80 && b > 60, "winter should wash toward snow");
    }

    #[test]
    fn phase_cycles_return_to_their_start() {
        let mut t = TimeOfDay::Dawn;
        for _ in 0..TimeOfDay::ALL.len() {
            t = t.next();
        }
        assert_eq!(t, TimeOfDay::Dawn);

        let mut s = Season::Spring;
        for _ in 0..Season::ALL.len() {
            s = s.next();
        }
        assert_eq!(s, Season::Spring);
    }

    #[test]
    fn every_phase_has_a_distinct_label() {
        let mut labels: Vec<_> = TimeOfDay::ALL.iter().map(|t| t.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), TimeOfDay::ALL.len());
    }

    #[test]
    fn hillshade_is_brightest_on_flat_ground() {
        let flat = hillshade_nw(0.0, 0.0);
        assert!(flat > 0.6, "flat ground should be well lit, got {flat}");
        assert!((0.0..=1.0).contains(&flat));
    }

    #[test]
    fn hillshade_lights_slopes_facing_the_sun_more_than_those_away() {
        // Screen axes: x east, y south. A slope that descends toward the
        // northwest gains height toward the southeast, so both gradients are
        // positive; the southeast-facing slope is its negation.
        let toward_nw = hillshade_nw(0.8, 0.8);
        let toward_se = hillshade_nw(-0.8, -0.8);
        assert!(
            toward_nw > toward_se,
            "NW-facing {toward_nw} should beat SE-facing {toward_se}"
        );
        assert!(toward_se < 0.2, "a slope facing away is nearly unlit");
    }

    #[test]
    fn cell_aspect_correction_evens_out_the_two_axes() {
        // A north-south ridge and an east-west ridge of the same *screen*
        // steepness must shade the same. Without the correction the
        // north-south one is lit as though it were twice as steep, which is
        // what makes an uncorrected relief map look vertically streaked.
        let east_west = hillshade_cells(0.5, 0.0);
        let north_south = hillshade_cells(0.0, 0.5 * super::CELL_ASPECT);
        // Different azimuth response is expected (the sun is northwest, not
        // overhead), so this only asserts they are in the same ballpark,
        // which the uncorrected pair is not.
        assert!(
            (east_west - north_south).abs() < 0.25,
            "corrected shades differ too much: {east_west} vs {north_south}"
        );
    }

    #[test]
    fn hillshade_relief_does_not_invert() {
        // The single most important property: a ridge must read as a ridge.
        // Walking west-to-east over a hill, the western (near) flank is lit
        // and the eastern (far) flank is shadowed. Getting this backwards is
        // the classic crater illusion.
        let west_flank = hillshade_nw(0.6, 0.0);
        let east_flank = hillshade_nw(-0.6, 0.0);
        assert!(
            west_flank > east_flank,
            "west flank {west_flank} must be brighter than east {east_flank}"
        );
    }

    #[test]
    fn hillshade_azimuth_actually_rotates_the_light() {
        // Same slope, opposite suns: what was lit must become shadowed.
        let slope = (0.8, 0.0);
        let west = 3.0 * core::f32::consts::FRAC_PI_2;
        let east = core::f32::consts::FRAC_PI_2;
        let from_west = hillshade(slope.0, slope.1, west, SUN_ALTITUDE);
        let from_east = hillshade(slope.0, slope.1, east, SUN_ALTITUDE);
        assert!(from_west > from_east, "{from_west} vs {from_east}");
    }

    #[test]
    fn hillshade_always_stays_in_range() {
        for i in -20..20 {
            for j in -20..20 {
                let v = hillshade(i as f32 * 0.3, j as f32 * 0.3, 1.0, 0.6);
                assert!((0.0..=1.0).contains(&v), "hillshade gave {v}");
            }
        }
    }

    #[test]
    fn fog_states_move_toward_the_background_in_order() {
        let bg = rgb(10, 10, 16);
        let base = rgb(200, 200, 200);
        let (rem_r, _, _) = channels(remembered(base, bg));
        let (unx_r, _, _) = channels(unexplored(bg));
        assert!(rem_r < 200, "remembered must be dimmer than visible");
        assert!(unx_r < rem_r, "unexplored must be dimmer than remembered");
    }
}
