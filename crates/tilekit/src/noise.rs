//! Deterministic value noise, fBm, and domain warping.
//!
//! Everything here is seeded integer hashing plus interpolation: no tables, no
//! allocation, no floating-point accumulation across calls. The same `(seed,
//! x, y)` always produces the same value on every platform and every backend,
//! which is what makes the demos' worlds reproducible and their tests
//! meaningful.
//!
//! Value noise rather than Perlin/simplex: for terrain at the scale a
//! character grid resolves (a few hundred cells across), fBm over value noise
//! is visually indistinguishable from gradient noise, and it is a third of the
//! code with none of the gradient-table setup. See Red Blob Games' [Making
//! maps with noise functions](https://www.redblobgames.com/maps/terrain-from-noise/)
//! for the octave/amplitude vocabulary used here, and Inigo Quilez on [domain
//! warping](https://www.iquilezles.org/articles/warp/) for [`warped_fbm`].

/// Hashes three integers to a well-distributed `u32`.
///
/// A 32-bit variant of the finalizer-style mixing used by `MurmurHash3` and
/// `SplitMix`: multiply by odd constants and xor-shift so that flipping any
/// single input bit changes about half the output bits. Not cryptographic and
/// not trying to be; it just has to have no visible axis-aligned structure,
/// because any structure here shows up as a grid pattern in the terrain.
#[must_use]
pub const fn hash3(seed: u32, x: i32, y: i32) -> u32 {
    let mut h = seed;
    h = h.wrapping_add((x as u32).wrapping_mul(0x9E37_79B9));
    h = h.wrapping_add((y as u32).wrapping_mul(0x85EB_CA6B));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    h = h.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 16;
    h
}

/// [`hash3`] mapped to `0.0..1.0`.
#[must_use]
pub fn hash01(seed: u32, x: i32, y: i32) -> f32 {
    // 24 bits, not 32: f32 has a 24-bit mantissa, so the low 8 bits of a u32
    // would be rounded away anyway, and dividing by 2^24 gives an exactly
    // representable result for every input.
    (hash3(seed, x, y) >> 8) as f32 / 16_777_216.0
}

/// Smoothstep: `3t^2 - 2t^3`.
///
/// The interpolant for [`value_noise`]. Its first derivative is zero at both
/// ends, so adjacent noise cells meet without the visible creases plain linear
/// interpolation leaves along lattice lines.
#[must_use]
pub const fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Value noise at `(x, y)` in `0.0..1.0`.
///
/// Bilinearly interpolates hashed lattice corners with [`smoothstep`] easing.
/// One "cell" of the lattice is one unit of input, so callers control feature
/// size by scaling their coordinates before calling (or by using [`fbm`],
/// which does that for them).
#[must_use]
pub fn value_noise(seed: u32, x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (smoothstep(x - x0), smoothstep(y - y0));
    let (ix, iy) = (x0 as i32, y0 as i32);

    let c00 = hash01(seed, ix, iy);
    let c10 = hash01(seed, ix + 1, iy);
    let c01 = hash01(seed, ix, iy + 1);
    let c11 = hash01(seed, ix + 1, iy + 1);

    let top = c10.mul_add(fx, c00 * (1.0 - fx));
    let bottom = c11.mul_add(fx, c01 * (1.0 - fx));
    bottom.mul_add(fy, top * (1.0 - fy))
}

/// Fractal Brownian motion: `octaves` layers of [`value_noise`], each at twice
/// the frequency and `gain` times the amplitude of the last.
///
/// Returns `0.0..1.0` (normalized by the total amplitude, so the range does
/// not depend on the octave count). `gain` of 0.5 is the classic choice and
/// gives terrain-like `1/f` "pink" noise; higher values look rougher and more
/// eroded, lower values smoother and more rolling.
///
/// Each octave is offset by a different seed rather than a coordinate shift:
/// a coordinate shift lets octaves correlate along the shift axis, which shows
/// up as faint diagonal banding once you stack four or more.
#[must_use]
pub fn fbm(seed: u32, x: f32, y: f32, octaves: u32, gain: f32) -> f32 {
    let mut sum = 0.0;
    let mut amplitude = 1.0;
    let mut total = 0.0;
    let mut frequency = 1.0;

    for octave in 0..octaves {
        let octave_seed = seed.wrapping_add(octave.wrapping_mul(0x9E37_79B9));
        sum += amplitude * value_noise(octave_seed, x * frequency, y * frequency);
        total += amplitude;
        amplitude *= gain;
        frequency *= 2.0;
    }

    if total > 0.0 { sum / total } else { 0.0 }
}

/// [`fbm`] sampled at coordinates displaced by two more `fbm` fields.
///
/// Domain warping is the cheapest way to turn the isotropic blobs of plain fBm
/// into something that reads as *geology*: coastlines gain fjords and
/// peninsulas, mountain ranges acquire ridgelines and spurs, and the result
/// stops looking like a heightmap and starts looking like a place.
/// `strength` is in input units, so it scales with the caller's coordinate
/// scale; 0.5 to 2.0 is the useful range, above which the field folds over
/// itself into noise.
///
/// See Inigo Quilez, [Domain warping](https://www.iquilezles.org/articles/warp/).
#[must_use]
pub fn warped_fbm(seed: u32, x: f32, y: f32, octaves: u32, gain: f32, strength: f32) -> f32 {
    // Two independent warp fields, one per axis. Using the same field for both
    // would displace every point along the x=y diagonal and shear the result
    // rather than warping it.
    let wx = fbm(seed ^ 0x5F35_6495, x, y, 3, 0.5) - 0.5;
    let wy = fbm(seed ^ 0x1B87_3593, x, y, 3, 0.5) - 0.5;
    fbm(
        seed,
        strength.mul_add(wx, x),
        strength.mul_add(wy, y),
        octaves,
        gain,
    )
}

/// Ridged noise: `1 - |2n - 1|`, sharpened by `exponent`.
///
/// Folds fBm around its midpoint so former mid-range values become peaks,
/// producing the sharp crests and V-shaped valleys of a real mountain range
/// instead of fBm's rounded hills. `exponent` above 1 narrows the ridges;
/// 2.0 is a good default for a mountain mask.
#[must_use]
pub fn ridged(seed: u32, x: f32, y: f32, octaves: u32, exponent: f32) -> f32 {
    let n = fbm(seed, x, y, octaves, 0.5);
    let ridge = 1.0 - (n.mul_add(2.0, -1.0)).abs();
    ridge.powf(exponent)
}

/// A tiny deterministic PRNG for one-off placement decisions.
///
/// [`hash3`] covers "what is the value at this coordinate"; this covers "give
/// me the next random number" for sequential work like scattering settlements
/// or picking names, where there is no natural coordinate to hash.
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    /// Seeds the generator. Seed 0 is remapped, since an all-zero state is a
    /// fixed point of the xorshift step and would emit only zeros.
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    /// Next raw `u32` (xorshift32).
    pub const fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Next `f32` in `0.0..1.0`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / 16_777_216.0
    }

    /// Next value in `0..n`. Returns 0 if `n` is 0.
    ///
    /// Uses the multiply-shift reduction rather than `% n`: modulo biases
    /// toward low values whenever `n` doesn't divide 2^32, which is visible
    /// when picking from a short list (say, six terrain decorations) many
    /// thousands of times across a map.
    pub const fn next_below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        ((self.next_u32() as u64 * n as u64) >> 32) as u32
    }

    /// Picks a random element of `items`, or `None` if empty.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.next_below(items.len() as u32) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rng, fbm, hash01, hash3, ridged, smoothstep, value_noise, warped_fbm};

    #[test]
    fn hashing_is_deterministic_and_seed_sensitive() {
        assert_eq!(hash3(1, 2, 3), hash3(1, 2, 3));
        assert_ne!(hash3(1, 2, 3), hash3(2, 2, 3));
        assert_ne!(hash3(1, 2, 3), hash3(1, 3, 2), "axes must not be symmetric");
    }

    #[test]
    fn hash01_stays_in_the_unit_interval() {
        for x in -50..50 {
            for y in -50..50 {
                let v = hash01(7, x, y);
                assert!((0.0..1.0).contains(&v), "hash01(7, {x}, {y}) = {v}");
            }
        }
    }

    #[test]
    fn hash01_is_roughly_uniform() {
        // Ten buckets over 10k samples; a well-distributed hash should put
        // ~1000 in each. The band is wide enough not to be flaky but tight
        // enough to catch a hash that clumps.
        let mut buckets = [0u32; 10];
        for x in 0..100 {
            for y in 0..100 {
                let v = hash01(99, x, y);
                buckets[((v * 10.0) as usize).min(9)] += 1;
            }
        }
        for (i, &n) in buckets.iter().enumerate() {
            assert!((700..1300).contains(&n), "bucket {i} had {n} samples");
        }
    }

    #[test]
    fn smoothstep_pins_both_ends_and_the_midpoint() {
        assert!((smoothstep(0.0) - 0.0).abs() < 1e-6);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
        assert!((smoothstep(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn value_noise_is_continuous_across_lattice_lines() {
        // Sampling either side of an integer boundary must not jump: a
        // discontinuity here would draw visible seams every cell.
        for i in -3..3 {
            let left = value_noise(4, i as f32 - 1e-4, 0.37);
            let right = value_noise(4, i as f32 + 1e-4, 0.37);
            assert!((left - right).abs() < 1e-3, "seam at x = {i}");
        }
    }

    #[test]
    fn value_noise_hits_lattice_corners_exactly() {
        for (x, y) in [(0, 0), (3, -2), (-5, 7)] {
            let sampled = value_noise(11, x as f32, y as f32);
            assert!((sampled - hash01(11, x, y)).abs() < 1e-5);
        }
    }

    #[test]
    fn fbm_stays_normalized_for_any_octave_count() {
        for octaves in 1..=8 {
            for i in 0..200 {
                let v = fbm(3, i as f32 * 0.37, i as f32 * 0.11, octaves, 0.5);
                assert!((0.0..=1.0).contains(&v), "{octaves} octaves gave {v}");
            }
        }
    }

    #[test]
    fn fbm_with_zero_octaves_is_defined() {
        assert!((fbm(1, 0.5, 0.5, 0, 0.5) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn warped_and_ridged_stay_in_range() {
        for i in 0..200 {
            let (x, y) = (i as f32 * 0.31, i as f32 * 0.17);
            let w = warped_fbm(5, x, y, 4, 0.5, 1.5);
            assert!((0.0..=1.0).contains(&w), "warped_fbm gave {w}");
            let r = ridged(5, x, y, 4, 2.0);
            assert!((0.0..=1.0).contains(&r), "ridged gave {r}");
        }
    }

    #[test]
    fn warping_actually_displaces_the_field() {
        // Zero strength must reduce exactly to plain fbm; nonzero must not.
        let plain = fbm(8, 2.5, 1.5, 4, 0.5);
        assert!((warped_fbm(8, 2.5, 1.5, 4, 0.5, 0.0) - plain).abs() < 1e-6);
        assert!((warped_fbm(8, 2.5, 1.5, 4, 0.5, 2.0) - plain).abs() > 1e-4);
    }

    #[test]
    fn rng_is_reproducible_and_never_stuck() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let seq: Vec<u32> = (0..64).map(|_| a.next_u32()).collect();
        assert_eq!(seq, (0..64).map(|_| b.next_u32()).collect::<Vec<_>>());
        assert!(seq.windows(2).any(|w| w[0] != w[1]), "generator is stuck");
    }

    #[test]
    fn rng_seed_zero_still_produces_a_sequence() {
        let mut rng = Rng::new(0);
        let first = rng.next_u32();
        assert_ne!(first, 0);
        assert_ne!(rng.next_u32(), first);
    }

    #[test]
    fn next_below_respects_its_bound_and_covers_it() {
        let mut rng = Rng::new(7);
        let mut seen = [false; 6];
        for _ in 0..600 {
            let v = rng.next_below(6);
            assert!(v < 6);
            seen[v as usize] = true;
        }
        assert!(
            seen.iter().all(|&s| s),
            "some values never came up: {seen:?}"
        );
        assert_eq!(rng.next_below(0), 0);
    }

    #[test]
    fn choose_handles_empty_and_populated_slices() {
        let mut rng = Rng::new(3);
        assert_eq!(rng.choose::<u8>(&[]), None);
        assert_eq!(rng.choose(&[9u8]), Some(&9));
    }
}
