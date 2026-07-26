//! Procedural overland worlds: heightmap, climate, biomes, rivers, roads,
//! settlements, and provinces.
//!
//! One [`World::generate`] call produces everything the demos need, so a demo
//! is free to concentrate on *rendering* rather than on inventing its own
//! terrain. Generation is deterministic in the seed and takes a few
//! milliseconds for a map of a few hundred cells a side, which is fast enough
//! to reroll interactively.
//!
//! The pipeline, in order:
//!
//! 1. **Elevation** from domain-warped fBm, multiplied by a radial falloff so
//!    the map is an island rather than terrain running off the edges.
//! 2. **Temperature** from latitude and altitude, the two things that actually
//!    determine it.
//! 3. **Moisture** from its own noise field, biased upward near water.
//! 4. **Biomes** by [Whittaker](https://en.wikipedia.org/wiki/Robert_Whittaker_(ecologist))
//!    classification over temperature and moisture.
//! 5. **Rivers** by steepest-descent from high, wet cells down to the sea.
//! 6. **Settlements** by scored site selection with a minimum spacing.
//! 7. **Roads** by greedy pathfinding between nearby settlements.
//! 8. **Provinces** by Voronoi assignment with Lloyd relaxation.
//!
//! See Red Blob Games, [Making maps with noise functions](https://www.redblobgames.com/maps/terrain-from-noise/),
//! for the elevation/moisture/biome approach this follows.

use std::collections::{BinaryHeap, HashMap};

use crate::noise::{Rng, fbm, hash01, warped_fbm};
use retroglyph_core::Color;

/// Terrain classification.
///
/// Ordered water-first so `biome <= Biome::Coast` is a water test, and the
/// land biomes then run cold to hot within each moisture band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Biome {
    /// Deep ocean.
    #[default]
    Ocean,
    /// Shallow sea over the continental shelf.
    Sea,
    /// A freshwater lake.
    Lake,
    /// Beach and littoral.
    Coast,
    /// Permanent ice.
    Ice,
    /// Cold, treeless.
    Tundra,
    /// Cold conifer forest.
    Taiga,
    /// Cold, dry, rocky.
    Scrubland,
    /// Temperate open ground.
    Grassland,
    /// Temperate broadleaf forest.
    Forest,
    /// Wetland.
    Marsh,
    /// Hot and dry.
    Desert,
    /// Hot with a wet season.
    Savanna,
    /// Hot and wet.
    Jungle,
    /// High rocky ground.
    Mountain,
    /// Above the snow line.
    Peak,
}

impl Biome {
    /// Whether this is any kind of water.
    #[must_use]
    pub const fn is_water(self) -> bool {
        matches!(self, Self::Ocean | Self::Sea | Self::Lake)
    }

    /// Whether units can generally cross it. Used by road routing and by the
    /// demos' movement overlays.
    #[must_use]
    pub const fn is_passable(self) -> bool {
        !matches!(self, Self::Ocean | Self::Sea | Self::Lake | Self::Peak)
    }

    /// Movement cost, in abstract points. Feeds road routing, so it is also
    /// what makes roads bend around mountains and follow valleys, which is the
    /// entire reason a generated road network looks plausible.
    #[must_use]
    pub const fn move_cost(self) -> u32 {
        match self {
            Self::Coast | Self::Grassland | Self::Savanna => 2,
            Self::Tundra | Self::Scrubland | Self::Desert => 3,
            Self::Forest | Self::Taiga => 4,
            Self::Marsh | Self::Jungle => 6,
            Self::Mountain => 9,
            Self::Ice => 12,
            // Impassable terrain still needs a finite cost so a search never
            // has to special-case it; make it expensive enough never to win.
            _ => 1000,
        }
    }

    /// The representative glyph for this biome.
    #[must_use]
    pub const fn glyph(self) -> char {
        use crate::glyphs::terrain;
        match self {
            Self::Ocean | Self::Sea => terrain::WATER,
            Self::Lake => terrain::WAVE,
            Self::Coast => terrain::SAND,
            Self::Ice | Self::Peak => terrain::SNOW,
            Self::Tundra => terrain::TUNDRA,
            Self::Taiga => terrain::CONIFER,
            Self::Grassland | Self::Savanna => terrain::GRASS,
            Self::Forest => terrain::FOREST,
            Self::Marsh => terrain::MARSH,
            // Scrubland and desert share a glyph; their colors differ enough
            // (olive versus sand) to tell them apart, and inventing a second
            // dune-like glyph would be a distinction without a difference.
            Self::Scrubland | Self::Desert => terrain::DUNE,
            Self::Jungle => terrain::JUNGLE,
            Self::Mountain => terrain::MOUNTAIN,
        }
    }

    /// The representative color for this biome.
    #[must_use]
    pub const fn color(self) -> Color {
        use crate::palette::rgb;
        match self {
            Self::Ocean => rgb(12, 26, 58),
            Self::Sea => rgb(26, 58, 102),
            Self::Lake => rgb(44, 92, 140),
            Self::Coast => rgb(196, 178, 128),
            Self::Ice => rgb(228, 236, 244),
            Self::Tundra => rgb(150, 156, 140),
            Self::Taiga => rgb(52, 84, 68),
            Self::Scrubland => rgb(140, 132, 90),
            Self::Grassland => rgb(104, 140, 66),
            Self::Forest => rgb(58, 100, 50),
            Self::Marsh => rgb(78, 100, 74),
            Self::Desert => rgb(206, 182, 116),
            Self::Savanna => rgb(164, 156, 76),
            Self::Jungle => rgb(40, 96, 52),
            Self::Mountain => rgb(112, 106, 100),
            Self::Peak => rgb(238, 240, 246),
        }
    }

    /// Display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ocean => "ocean",
            Self::Sea => "sea",
            Self::Lake => "lake",
            Self::Coast => "coast",
            Self::Ice => "ice",
            Self::Tundra => "tundra",
            Self::Taiga => "taiga",
            Self::Scrubland => "scrubland",
            Self::Grassland => "grassland",
            Self::Forest => "forest",
            Self::Marsh => "marsh",
            Self::Desert => "desert",
            Self::Savanna => "savanna",
            Self::Jungle => "jungle",
            Self::Mountain => "mountain",
            Self::Peak => "peak",
        }
    }

    /// Every biome, for legends and tests.
    pub const ALL: [Self; 16] = [
        Self::Ocean,
        Self::Sea,
        Self::Lake,
        Self::Coast,
        Self::Ice,
        Self::Tundra,
        Self::Taiga,
        Self::Scrubland,
        Self::Grassland,
        Self::Forest,
        Self::Marsh,
        Self::Desert,
        Self::Savanna,
        Self::Jungle,
        Self::Mountain,
        Self::Peak,
    ];
}

/// Whittaker classification: biome from temperature and moisture, both in
/// `0.0..=1.0`.
///
/// A lookup table rather than a chain of thresholds, because the thresholds
/// are the *design* and burying them in control flow makes them impossible to
/// tune. Reading down a column shows how a fixed rainfall plays out from pole
/// to equator, which is exactly the question you ask when a generated map
/// looks wrong.
#[must_use]
pub fn whittaker(temperature: f32, moisture: f32) -> Biome {
    // Rows: cold to hot (polar, boreal, temperate, tropical).
    // Columns: dry to wet.
    //
    // Only the polar row carries tundra. An earlier version also put tundra in
    // the boreal row, which sounds harmless and is not: the boreal band is the
    // single widest slice of a latitude-driven temperature field, so that one
    // cell turned a fifth of every generated map into tundra and left barely
    // any temperate grassland. Cool and moderately wet is woodland, not
    // permafrost.
    const TABLE: [[Biome; 4]; 4] = [
        [Biome::Ice, Biome::Tundra, Biome::Tundra, Biome::Taiga],
        [
            Biome::Scrubland,
            Biome::Grassland,
            Biome::Taiga,
            Biome::Taiga,
        ],
        [
            Biome::Scrubland,
            Biome::Grassland,
            Biome::Forest,
            Biome::Marsh,
        ],
        [Biome::Desert, Biome::Savanna, Biome::Jungle, Biome::Jungle],
    ];
    let t = ((temperature.clamp(0.0, 1.0) * 4.0) as usize).min(3);
    let m = ((moisture.clamp(0.0, 1.0) * 4.0) as usize).min(3);
    TABLE[t][m]
}

/// A point of interest on the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Site {
    /// The capital of a province.
    Capital,
    /// A city.
    City,
    /// A town.
    Town,
    /// A fortress guarding a pass or border.
    Fort,
    /// A ruin.
    Ruin,
    /// A mine.
    Mine,
    /// A shrine.
    Shrine,
}

impl Site {
    /// Whether this site is inhabited (and therefore a road destination).
    #[must_use]
    pub const fn is_settlement(self) -> bool {
        matches!(self, Self::Capital | Self::City | Self::Town)
    }

    /// The glyph and color for this site.
    #[must_use]
    pub const fn glyph_color(self) -> (char, Color) {
        use crate::glyphs::marker;
        use crate::palette::rgb;
        match self {
            Self::Capital => (marker::CAPITAL, rgb(250, 214, 120)),
            Self::City => (marker::CITY, rgb(238, 232, 220)),
            Self::Town => (marker::TOWN, rgb(206, 196, 180)),
            Self::Fort => (marker::FORT, rgb(196, 150, 130)),
            Self::Ruin => (marker::RUIN, rgb(150, 140, 130)),
            Self::Mine => (marker::MINE, rgb(190, 170, 120)),
            Self::Shrine => (marker::SHRINE, rgb(170, 150, 220)),
        }
    }

    /// Display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Capital => "capital",
            Self::City => "city",
            Self::Town => "town",
            Self::Fort => "fortress",
            Self::Ruin => "ruins",
            Self::Mine => "mine",
            Self::Shrine => "shrine",
        }
    }
}

/// A placed point of interest.
#[derive(Debug, Clone)]
pub struct Landmark {
    /// Map position.
    pub x: i32,
    /// Map position.
    pub y: i32,
    /// What it is.
    pub site: Site,
    /// Its generated name.
    pub name: String,
    /// Which province owns it.
    pub province: usize,
}

/// A generated world.
///
/// All the per-cell fields are row-major `width * height` vectors indexed by
/// [`idx`](Self::idx). Parallel vectors rather than a vector of structs
/// deliberately: rendering passes read one field across the whole map (every
/// elevation, then every biome), and that access pattern wants each field
/// contiguous.
#[derive(Debug, Clone)]
pub struct World {
    width: i32,
    height: i32,
    seed: u32,
    /// Terrain height, `0.0..=1.0`. Sea level is [`SEA_LEVEL`].
    pub elevation: Vec<f32>,
    /// Temperature, `0.0..=1.0`, cold to hot.
    pub temperature: Vec<f32>,
    /// Moisture, `0.0..=1.0`, dry to wet.
    pub moisture: Vec<f32>,
    /// Classified terrain.
    pub biome: Vec<Biome>,
    /// Whether a river runs through this cell.
    pub river: Vec<bool>,
    /// Whether a road runs through this cell.
    pub road: Vec<bool>,
    /// Which province owns this cell.
    pub province: Vec<usize>,
    /// Placed points of interest.
    pub landmarks: Vec<Landmark>,
    /// Province seed positions, one per province.
    pub province_seeds: Vec<(i32, i32)>,
}

/// Elevation at or below which terrain is underwater.
pub const SEA_LEVEL: f32 = 0.42;
/// Elevation above which terrain is mountainous regardless of climate.
pub const MOUNTAIN_LEVEL: f32 = 0.74;
/// Elevation above which terrain is bare peak.
pub const PEAK_LEVEL: f32 = 0.86;

impl World {
    /// Generates a `width` x `height` world from `seed`.
    ///
    /// # Panics
    ///
    /// Panics if either dimension is below 8; below that the falloff mask
    /// leaves no land at all and every later stage has nothing to work with,
    /// which produces confusing empty output rather than an obvious error.
    #[must_use]
    pub fn generate(width: i32, height: i32, seed: u32) -> Self {
        assert!(width >= 8 && height >= 8, "world must be at least 8x8");
        let cells = (width * height) as usize;

        let mut world = Self {
            width,
            height,
            seed,
            elevation: vec![0.0; cells],
            temperature: vec![0.0; cells],
            moisture: vec![0.0; cells],
            biome: vec![Biome::Ocean; cells],
            river: vec![false; cells],
            road: vec![false; cells],
            province: vec![0; cells],
            landmarks: Vec::new(),
            province_seeds: Vec::new(),
        };

        world.build_elevation();
        world.build_climate();
        world.classify();
        world.carve_rivers();
        world.place_landmarks();
        world.build_roads();
        world.build_provinces();
        world
    }

    /// Map width in cells.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.width
    }

    /// Map height in cells.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// The seed this world was generated from.
    #[must_use]
    pub const fn seed(&self) -> u32 {
        self.seed
    }

    /// Whether `(x, y)` is on the map.
    #[must_use]
    pub const fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.width && y < self.height
    }

    /// Row-major index of `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub const fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if self.in_bounds(x, y) {
            Some((y * self.width + x) as usize)
        } else {
            None
        }
    }

    /// Biome at `(x, y)`, or [`Biome::Ocean`] outside the map.
    ///
    /// Off-map reading as ocean is what lets renderers sample past the edge
    /// without special cases: an island world is surrounded by sea anyway, so
    /// the fiction stays consistent right up to the border.
    #[must_use]
    pub fn biome_at(&self, x: i32, y: i32) -> Biome {
        self.idx(x, y).map_or(Biome::Ocean, |i| self.biome[i])
    }

    /// Elevation at `(x, y)`, or 0 outside the map.
    #[must_use]
    pub fn elevation_at(&self, x: i32, y: i32) -> f32 {
        self.idx(x, y).map_or(0.0, |i| self.elevation[i])
    }

    /// Whether a river runs through `(x, y)`.
    #[must_use]
    pub fn river_at(&self, x: i32, y: i32) -> bool {
        self.idx(x, y).is_some_and(|i| self.river[i])
    }

    /// Whether a road runs through `(x, y)`.
    #[must_use]
    pub fn road_at(&self, x: i32, y: i32) -> bool {
        self.idx(x, y).is_some_and(|i| self.road[i])
    }

    /// Province owning `(x, y)`, or 0 outside the map.
    #[must_use]
    pub fn province_at(&self, x: i32, y: i32) -> usize {
        self.idx(x, y).map_or(0, |i| self.province[i])
    }

    /// The landmark at `(x, y)`, if any.
    #[must_use]
    pub fn landmark_at(&self, x: i32, y: i32) -> Option<&Landmark> {
        self.landmarks.iter().find(|l| l.x == x && l.y == y)
    }

    /// Local elevation gradient at `(x, y)`, as `(dz/dx, dz/dy)` in height
    /// units per cell, for hillshading.
    ///
    /// Central differences where possible, one-sided at the edges. Scaled by
    /// `relief` because raw elevation deltas between adjacent cells are tiny
    /// (a map spanning 0 to 1 over 200 cells has gradients around 0.005), and
    /// hillshading them directly produces an almost perfectly flat surface.
    #[must_use]
    pub fn gradient_at(&self, x: i32, y: i32, relief: f32) -> (f32, f32) {
        let sample = |dx: i32, dy: i32| {
            let (sx, sy) = (
                (x + dx).clamp(0, self.width - 1),
                (y + dy).clamp(0, self.height - 1),
            );
            self.elevation_at(sx, sy)
        };
        let half_relief = 0.5 * relief;
        (
            (sample(1, 0) - sample(-1, 0)) * half_relief,
            (sample(0, 1) - sample(0, -1)) * half_relief,
        )
    }

    /// The four cardinal neighbours of `(x, y)` that are on the map.
    #[must_use]
    pub fn neighbors4(&self, x: i32, y: i32) -> Vec<(i32, i32)> {
        [(0, -1), (1, 0), (0, 1), (-1, 0)]
            .into_iter()
            .map(|(dx, dy)| (x + dx, y + dy))
            .filter(|&(nx, ny)| self.in_bounds(nx, ny))
            .collect()
    }

    // ── Generation stages ───────────────────────────────────────────────────

    /// Domain-warped fBm shaped into an island by a radial falloff.
    fn build_elevation(&mut self) {
        // Scale so feature size is a fixed fraction of the map, not a fixed
        // number of cells: a 400-wide map should look like a bigger version of
        // a 200-wide one, not like the same terrain zoomed out.
        let scale = 3.5 / self.width.max(self.height) as f32;

        for y in 0..self.height {
            for x in 0..self.width {
                let (fx, fy) = (x as f32 * scale, y as f32 * scale);
                let base = warped_fbm(self.seed, fx, fy, 5, 0.5, 1.4);

                // Ridged detail, weighted toward high ground, so mountains get
                // crests without the lowlands turning into a bed of nails.
                let ridge =
                    crate::noise::ridged(self.seed ^ 0xA53C_1D77, fx * 1.7, fy * 1.7, 3, 2.0);
                let mixed = base.mul_add(0.82, ridge * base * 0.34);

                let masked = mixed * self.falloff(x, y);
                self.elevation[(y * self.width + x) as usize] = masked.clamp(0.0, 1.0);
            }
        }
        self.normalize_elevation();
    }

    /// Radial falloff in `0.0..=1.0`, 1 in the interior and 0 at the border.
    ///
    /// Uses the Chebyshev-style max of the two normalized axis distances
    /// rather than Euclidean radius, so the shape follows the map rectangle
    /// instead of inscribing a circle in it and wasting the corners.
    fn falloff(&self, x: i32, y: i32) -> f32 {
        let nx = (x as f32 / (self.width - 1) as f32)
            .mul_add(2.0, -1.0)
            .abs();
        let ny = (y as f32 / (self.height - 1) as f32)
            .mul_add(2.0, -1.0)
            .abs();
        let d = nx.max(ny);
        // Cubic ease keeps the interior flat and drops sharply near the edge,
        // which reads as a continent with a coastline rather than a dome.
        (d * d).mul_add(-d, 1.0).clamp(0.0, 1.0)
    }

    /// Rescales elevation so the map actually spans its range.
    ///
    /// Without this, the falloff multiply drags the maximum well below 1 and
    /// [`SEA_LEVEL`] drowns almost everything, in a way that varies with seed.
    /// Normalizing makes the land/water ratio stable across seeds, which is
    /// what makes a fixed sea level meaningful at all.
    fn normalize_elevation(&mut self) {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for &e in &self.elevation {
            lo = lo.min(e);
            hi = hi.max(e);
        }
        let span = hi - lo;
        if span <= f32::EPSILON {
            return;
        }
        for e in &mut self.elevation {
            *e = (*e - lo) / span;
        }
    }

    /// Temperature from latitude and altitude; moisture from noise, wetter
    /// near water.
    fn build_climate(&mut self) {
        let scale = 2.5 / self.width.max(self.height) as f32;
        for y in 0..self.height {
            // Latitude band: hot at the equator (map center), cold at the
            // poles (top and bottom edges).
            let lat = (y as f32 / (self.height - 1) as f32)
                .mul_add(2.0, -1.0)
                .abs();
            // Quadratic rather than linear falloff. A linear term spends half
            // the map below the temperate/boreal boundary, because |2y/H - 1|
            // exceeds 0.5 over half of any map; squaring it keeps the middle
            // two thirds temperate and confines real cold to the actual poles,
            // which is both more habitable and closer to how latitude works.
            let warmth = lat.mul_add(-lat * 0.88, 1.0);
            for x in 0..self.width {
                let i = (y * self.width + x) as usize;
                let elevation = self.elevation[i];

                // Lapse rate: only altitude *above sea level* cools things, or
                // deep ocean would come out arctic.
                let altitude = (elevation - SEA_LEVEL).max(0.0);
                let wobble = fbm(
                    self.seed ^ 0x2B7E_1516,
                    x as f32 * scale,
                    y as f32 * scale,
                    3,
                    0.5,
                );
                let temp = altitude.mul_add(-0.95, wobble.mul_add(0.18, warmth) - 0.09);
                self.temperature[i] = temp.clamp(0.0, 1.0);

                // Stretched around its midpoint so the field actually reaches
                // both extremes. Raw fBm clusters near 0.5 (it is a sum of
                // independent octaves, so it tends to the mean), which leaves
                // no cell dry enough for desert or wet enough for jungle.
                let raw = fbm(
                    self.seed ^ 0x3A5F_09B2,
                    x as f32 * scale * 1.6,
                    y as f32 * scale * 1.6,
                    4,
                    0.5,
                );
                let base = (raw - 0.5).mul_add(1.7, 0.5).clamp(0.0, 1.0);
                // Rain shadow: high ground wrings moisture out of the air, so
                // mountain interiors turn arid the way real ones do.
                let dryness = (elevation - MOUNTAIN_LEVEL).max(0.0) * 1.6;
                self.moisture[i] = (base - dryness).clamp(0.0, 1.0);
            }
        }
        self.humidify_near_water();
    }

    /// Raises moisture near the coast, where a purely noise-driven field would
    /// happily put a desert on a beach.
    fn humidify_near_water(&mut self) {
        const REACH: i32 = 6;
        let is_water: Vec<bool> = self.elevation.iter().map(|&e| e <= SEA_LEVEL).collect();

        for y in 0..self.height {
            for x in 0..self.width {
                let i = (y * self.width + x) as usize;
                if is_water[i] {
                    self.moisture[i] = 1.0;
                    continue;
                }
                // Nearest water within REACH, by expanding square rings; close
                // enough to a true distance transform at this radius and far
                // cheaper than one for a map this size.
                let mut nearest = None;
                'search: for r in 1..=REACH {
                    for dy in -r..=r {
                        for dx in -r..=r {
                            if dx.abs() != r && dy.abs() != r {
                                continue;
                            }
                            if let Some(j) = self.idx(x + dx, y + dy)
                                && is_water[j]
                            {
                                nearest = Some(r);
                                break 'search;
                            }
                        }
                    }
                }
                if let Some(r) = nearest {
                    // Kept modest: a large coastal boost on an island world
                    // (where most land is within a few cells of water) floods
                    // the moisture field and erases the dry biomes entirely.
                    let boost = (1.0 - r as f32 / REACH as f32) * 0.24;
                    self.moisture[i] = (self.moisture[i] + boost).min(1.0);
                }
            }
        }
    }

    /// Assigns biomes from elevation, temperature, and moisture.
    fn classify(&mut self) {
        for i in 0..self.elevation.len() {
            let e = self.elevation[i];
            self.biome[i] = if e <= SEA_LEVEL - 0.10 {
                Biome::Ocean
            } else if e <= SEA_LEVEL {
                Biome::Sea
            } else if e <= SEA_LEVEL + 0.015 {
                Biome::Coast
            } else if e >= PEAK_LEVEL {
                Biome::Peak
            } else if e >= MOUNTAIN_LEVEL {
                Biome::Mountain
            } else {
                whittaker(self.temperature[i], self.moisture[i])
            };
        }
    }

    /// Traces rivers downhill from high, wet cells to the sea.
    ///
    /// Steepest descent with a step limit rather than a full hydrology
    /// simulation: real drainage networks need depression filling and flow
    /// accumulation, which is a lot of machinery for something a strategy map
    /// reads as "a blue squiggle that ends in the sea". The step limit is what
    /// stops a river caught in a local minimum from looping forever.
    fn carve_rivers(&mut self) {
        let mut rng = Rng::new(self.seed ^ 0x7F4A_7C15);
        let target = ((self.width * self.height) / 900).clamp(3, 24);
        let max_steps = (self.width + self.height) * 2;

        let mut placed = 0;
        for _ in 0..target * 12 {
            if placed >= target {
                break;
            }
            let x = rng.next_below(self.width as u32) as i32;
            let y = rng.next_below(self.height as u32) as i32;
            let Some(start) = self.idx(x, y) else {
                continue;
            };
            // Sources are high and wet, which is where real rivers start.
            if self.elevation[start] < MOUNTAIN_LEVEL - 0.06 || self.moisture[start] < 0.35 {
                continue;
            }

            let mut path = Vec::new();
            let (mut cx, mut cy) = (x, y);
            let mut reached_sea = false;
            for _ in 0..max_steps {
                let Some(i) = self.idx(cx, cy) else { break };
                if self.biome[i].is_water() {
                    reached_sea = true;
                    break;
                }
                path.push(i);

                // Steepest descent, with a tiny hash jitter so rivers on a
                // smooth slope wander instead of running dead straight.
                let here = self.elevation[i];
                let mut best: Option<(f32, i32, i32)> = None;
                for (nx, ny) in self.neighbors4(cx, cy) {
                    let Some(j) = self.idx(nx, ny) else { continue };
                    let jitter = hash01(self.seed ^ 0x51ED_270B, nx, ny) * 0.004;
                    let score = self.elevation[j] + jitter;
                    if score < here && best.is_none_or(|(b, _, _)| score < b) {
                        best = Some((score, nx, ny));
                    }
                }
                if let Some((_, nx, ny)) = best {
                    cx = nx;
                    cy = ny;
                } else {
                    // A local minimum with no lower neighbour: pool into a
                    // lake and stop, which is what actually happens.
                    self.biome[i] = Biome::Lake;
                    break;
                }
            }

            // Only keep rivers long enough to read as rivers. A three-cell
            // stub is visual noise, not a feature.
            if path.len() >= 6 || (reached_sea && path.len() >= 4) {
                for i in path {
                    self.river[i] = true;
                    if self.moisture[i] < 0.6 {
                        self.moisture[i] = 0.6;
                    }
                }
                placed += 1;
            }
        }
    }

    /// Scores every land cell as a settlement site and places the best,
    /// keeping them apart.
    fn place_landmarks(&mut self) {
        let mut rng = Rng::new(self.seed ^ 0x1D8E_3B41);
        let spacing = (self.width.min(self.height) / 6).max(5);
        let wanted = ((self.width * self.height) / 1400).clamp(4, 20);

        let mut scored: Vec<(i32, i32, i32)> = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                let i = (y * self.width + x) as usize;
                let biome = self.biome[i];
                if !biome.is_passable() || biome.is_water() {
                    continue;
                }
                let mut score = match biome {
                    Biome::Grassland => 100,
                    Biome::Forest | Biome::Savanna => 80,
                    Biome::Coast => 70,
                    Biome::Taiga | Biome::Scrubland => 45,
                    Biome::Desert | Biome::Tundra => 20,
                    _ => 30,
                };
                // Real settlements cluster on fresh water and natural harbours.
                if self.river[i] {
                    score += 90;
                }
                if self
                    .neighbors4(x, y)
                    .iter()
                    .any(|&(nx, ny)| self.idx(nx, ny).is_some_and(|j| self.biome[j].is_water()))
                {
                    score += 60;
                }
                score += (hash01(self.seed ^ 0x64C1_9A03, x, y) * 40.0) as i32;
                scored.push((score, x, y));
            }
        }
        // Descending by score, so the best sites get first refusal on space.
        scored.sort_unstable_by_key(|&(score, _, _)| std::cmp::Reverse(score));

        let mut chosen: Vec<(i32, i32)> = Vec::new();
        for (_, x, y) in scored {
            if chosen.len() >= wanted as usize {
                break;
            }
            if chosen
                .iter()
                .any(|&(cx, cy)| (cx - x).abs() + (cy - y).abs() < spacing)
            {
                continue;
            }
            chosen.push((x, y));
        }

        for (rank, (x, y)) in chosen.into_iter().enumerate() {
            let site = match rank {
                0 => Site::Capital,
                1..=3 => Site::City,
                _ => Site::Town,
            };
            let name = generate_name(&mut rng);
            self.landmarks.push(Landmark {
                x,
                y,
                site,
                name,
                province: 0,
            });
        }

        self.scatter_minor_sites(&mut rng);
    }

    /// Scatters ruins, mines, and shrines in terrain that suits them.
    fn scatter_minor_sites(&mut self, rng: &mut Rng) {
        let wanted = ((self.width * self.height) / 2200).clamp(3, 14);
        let mut placed = 0;
        for _ in 0..wanted * 40 {
            if placed >= wanted {
                break;
            }
            let x = rng.next_below(self.width as u32) as i32;
            let y = rng.next_below(self.height as u32) as i32;
            let Some(i) = self.idx(x, y) else { continue };
            let biome = self.biome[i];
            if !biome.is_passable() || biome.is_water() {
                continue;
            }
            // Minimum spacing from everything already placed, so minor sites
            // don't pile up on a settlement's doorstep.
            if self
                .landmarks
                .iter()
                .any(|l| (l.x - x).abs() + (l.y - y).abs() < 4)
            {
                continue;
            }
            let site = match biome {
                Biome::Mountain | Biome::Scrubland => Site::Mine,
                Biome::Jungle | Biome::Desert => Site::Ruin,
                Biome::Forest | Biome::Taiga => Site::Shrine,
                _ => {
                    if rng.next_below(2) == 0 {
                        Site::Fort
                    } else {
                        Site::Ruin
                    }
                }
            };
            let name = generate_name(rng);
            self.landmarks.push(Landmark {
                x,
                y,
                site,
                name,
                province: 0,
            });
            placed += 1;
        }
    }

    /// Connects each settlement to its nearest neighbours by least-cost paths.
    fn build_roads(&mut self) {
        let towns: Vec<(i32, i32)> = self
            .landmarks
            .iter()
            .filter(|l| l.site.is_settlement())
            .map(|l| (l.x, l.y))
            .collect();
        if towns.len() < 2 {
            return;
        }

        // Connect each settlement to its two nearest peers rather than
        // building a minimum spanning tree: an MST is a tree, and a road
        // network that is a tree has no loops, which is immediately readable
        // as artificial. Two links each gives redundancy and the occasional
        // triangle, like a real road network.
        for (i, &from) in towns.iter().enumerate() {
            let mut others: Vec<(i32, usize)> = towns
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(j, &(x, y))| ((from.0 - x).abs() + (from.1 - y).abs(), j))
                .collect();
            others.sort_unstable();
            for &(_, j) in others.iter().take(2) {
                if let Some(path) = self.route(from, towns[j]) {
                    for (x, y) in path {
                        if let Some(k) = self.idx(x, y) {
                            self.road[k] = true;
                        }
                    }
                }
            }
        }
    }

    /// Least-cost path between two cells over [`Biome::move_cost`], or `None`
    /// if unreachable.
    ///
    /// Dijkstra rather than A*: the maps are small, the terrain costs vary
    /// enough that a Manhattan heuristic barely prunes anything, and an
    /// admissible heuristic here would have to assume the cheapest possible
    /// terrain, which makes it nearly useless. Not worth the extra code.
    fn route(&self, from: (i32, i32), to: (i32, i32)) -> Option<Vec<(i32, i32)>> {
        let start = self.idx(from.0, from.1)?;
        let goal = self.idx(to.0, to.1)?;

        let mut dist: HashMap<usize, u32> = HashMap::new();
        let mut prev: HashMap<usize, usize> = HashMap::new();
        // Reverse ordering via a min-heap over (cost, index): BinaryHeap is a
        // max-heap, so costs are negated by storing them in Reverse.
        let mut open = BinaryHeap::new();
        dist.insert(start, 0);
        open.push(std::cmp::Reverse((0u32, start)));

        while let Some(std::cmp::Reverse((cost, at))) = open.pop() {
            if at == goal {
                let mut path = vec![at];
                let mut cursor = at;
                while let Some(&p) = prev.get(&cursor) {
                    path.push(p);
                    cursor = p;
                }
                path.reverse();
                return Some(
                    path.into_iter()
                        .map(|i| {
                            let i = i as i32;
                            (i % self.width, i / self.width)
                        })
                        .collect(),
                );
            }
            if dist.get(&at).is_some_and(|&d| cost > d) {
                continue;
            }
            let (x, y) = ((at as i32) % self.width, (at as i32) / self.width);
            for (nx, ny) in self.neighbors4(x, y) {
                let Some(j) = self.idx(nx, ny) else { continue };
                let biome = self.biome[j];
                if !biome.is_passable() {
                    continue;
                }
                // Existing roads are cheap to follow, so separate routes merge
                // into shared trunk roads instead of running side by side.
                let mut step = biome.move_cost();
                if self.road[j] {
                    step = step.div_ceil(3);
                }
                let next = cost + step;
                if dist.get(&j).is_none_or(|&d| next < d) {
                    dist.insert(j, next);
                    prev.insert(j, at);
                    open.push(std::cmp::Reverse((next, j)));
                }
            }
        }
        None
    }

    /// Partitions the land into provinces by Voronoi assignment around
    /// settlements, then relaxes.
    fn build_provinces(&mut self) {
        let mut seeds: Vec<(i32, i32)> = self
            .landmarks
            .iter()
            .filter(|l| l.site.is_settlement())
            .map(|l| (l.x, l.y))
            .collect();
        if seeds.is_empty() {
            seeds.push((self.width / 2, self.height / 2));
        }

        // Two rounds of Lloyd relaxation. Raw Voronoi around settlement sites
        // gives lopsided provinces because settlements cluster on rivers and
        // coasts; relaxing toward each region's centroid evens them out while
        // keeping the borders following the same terrain.
        for _ in 0..2 {
            self.assign_provinces(&seeds);
            let mut sums = vec![(0i64, 0i64, 0i64); seeds.len()];
            for y in 0..self.height {
                for x in 0..self.width {
                    let i = (y * self.width + x) as usize;
                    if self.biome[i].is_water() {
                        continue;
                    }
                    let p = self.province[i];
                    sums[p].0 += i64::from(x);
                    sums[p].1 += i64::from(y);
                    sums[p].2 += 1;
                }
            }
            for (seed, &(sx, sy, n)) in seeds.iter_mut().zip(&sums) {
                if n > 0 {
                    *seed = ((sx / n) as i32, (sy / n) as i32);
                }
            }
        }
        self.assign_provinces(&seeds);
        self.province_seeds = seeds;

        for landmark in &mut self.landmarks {
            landmark.province = self
                .province
                .get((landmark.y * self.width + landmark.x) as usize)
                .copied()
                .unwrap_or(0);
        }
    }

    /// Assigns every cell to the nearest seed.
    fn assign_provinces(&mut self, seeds: &[(i32, i32)]) {
        for y in 0..self.height {
            for x in 0..self.width {
                let i = (y * self.width + x) as usize;
                let mut best = (i64::MAX, 0usize);
                for (p, &(sx, sy)) in seeds.iter().enumerate() {
                    let (dx, dy) = (i64::from(x - sx), i64::from(y - sy));
                    let d = dx * dx + dy * dy;
                    if d < best.0 {
                        best = (d, p);
                    }
                }
                self.province[i] = best.1;
            }
        }
    }

    /// Number of provinces.
    #[must_use]
    pub fn province_count(&self) -> usize {
        self.province_seeds.len().max(1)
    }

    /// A good starting camera position: the capital, or the map center if
    /// there isn't one.
    #[must_use]
    pub fn start_position(&self) -> (i32, i32) {
        self.landmarks
            .iter()
            .find(|l| l.site == Site::Capital)
            .map_or((self.width / 2, self.height / 2), |l| (l.x, l.y))
    }

    /// Fraction of the map that is land.
    #[must_use]
    pub fn land_fraction(&self) -> f32 {
        if self.biome.is_empty() {
            return 0.0;
        }
        let land = self.biome.iter().filter(|b| !b.is_water()).count();
        land as f32 / self.biome.len() as f32
    }
}

/// Syllable pool for place names.
const SYLLABLES: [&str; 32] = [
    "ard", "bel", "cor", "dun", "eth", "fen", "gar", "hal", "ith", "jor", "kel", "lor", "mor",
    "nar", "oth", "pel", "quen", "rin", "sar", "tor", "ulm", "vale", "wyn", "xan", "yr", "zel",
    "brack", "dorn", "gwyn", "myr", "thal", "vor",
];

/// Suffixes that make a syllable pair read as a place.
const SUFFIXES: [&str; 10] = [
    "", "", "ia", "mere", "ford", "holm", "gard", "wick", "fell", "haven",
];

/// Generates a place name from two or three syllables plus an optional suffix.
///
/// Deliberately simple: a Markov chain over real toponyms would read better
/// but needs a corpus, and the names here only have to be pronounceable and
/// distinct enough that two settlements on screen never look like the same
/// one.
#[must_use]
pub fn generate_name(rng: &mut Rng) -> String {
    let count = 2 + rng.next_below(2) as usize;
    let mut name = String::new();
    for _ in 0..count {
        name.push_str(SYLLABLES[rng.next_below(SYLLABLES.len() as u32) as usize]);
    }
    name.push_str(SUFFIXES[rng.next_below(SUFFIXES.len() as u32) as usize]);

    // Capitalize. `to_uppercase` on the first char rather than
    // `make_ascii_uppercase` because it is the correct operation, and these
    // syllables are ASCII only by current happenstance.
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Biome, HashMap, MOUNTAIN_LEVEL, PEAK_LEVEL, Rng, SEA_LEVEL, Site, World, generate_name,
        whittaker,
    };

    /// Small enough to generate quickly, large enough for every stage to have
    /// something to do.
    fn world() -> World {
        World::generate(120, 80, 7)
    }

    // ── Biome ───────────────────────────────────────────────────────────────

    #[test]
    fn water_biomes_are_water_and_impassable() {
        for biome in [Biome::Ocean, Biome::Sea, Biome::Lake] {
            assert!(biome.is_water(), "{biome:?}");
            assert!(!biome.is_passable(), "{biome:?}");
        }
        assert!(!Biome::Coast.is_water(), "coast is land");
        assert!(Biome::Coast.is_passable());
        assert!(!Biome::Peak.is_passable(), "peaks are impassable");
    }

    #[test]
    fn passable_terrain_is_always_cheaper_than_impassable() {
        let worst_passable = Biome::ALL
            .iter()
            .filter(|b| b.is_passable())
            .map(|b| b.move_cost())
            .max()
            .expect("some terrain is passable");
        let best_impassable = Biome::ALL
            .iter()
            .filter(|b| !b.is_passable())
            .map(|b| b.move_cost())
            .min()
            .expect("some terrain is impassable");
        assert!(
            worst_passable < best_impassable,
            "{worst_passable} vs {best_impassable}"
        );
    }

    #[test]
    fn every_biome_has_distinct_presentation() {
        let mut names: Vec<_> = Biome::ALL.iter().map(|b| b.name()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "biome names must be unique");

        // Colors must differ too, or the map is a lie about its own legend.
        let mut colors: Vec<_> = Biome::ALL
            .iter()
            .map(|b| format!("{:?}", b.color()))
            .collect();
        colors.sort_unstable();
        colors.dedup();
        assert_eq!(colors.len(), total, "biome colors must be unique");
    }

    // ── Whittaker ───────────────────────────────────────────────────────────

    #[test]
    fn whittaker_covers_the_full_climate_square() {
        for t in 0..=10 {
            for m in 0..=10 {
                let biome = whittaker(t as f32 / 10.0, m as f32 / 10.0);
                assert!(!biome.is_water(), "climate produced water: {biome:?}");
            }
        }
    }

    #[test]
    fn whittaker_clamps_out_of_range_input() {
        assert_eq!(whittaker(-5.0, -5.0), whittaker(0.0, 0.0));
        assert_eq!(whittaker(9.0, 9.0), whittaker(1.0, 1.0));
    }

    #[test]
    fn whittaker_puts_the_extremes_where_ecology_says() {
        assert_eq!(whittaker(0.0, 0.0), Biome::Ice, "cold and dry");
        assert_eq!(whittaker(1.0, 0.0), Biome::Desert, "hot and dry");
        assert_eq!(whittaker(1.0, 1.0), Biome::Jungle, "hot and wet");
        assert_eq!(
            whittaker(0.5, 0.3),
            Biome::Grassland,
            "temperate and dryish"
        );
        assert_eq!(
            whittaker(0.3, 0.3),
            Biome::Grassland,
            "cool and dryish is woodland, not permafrost"
        );
        assert_eq!(whittaker(0.1, 0.3), Biome::Tundra, "only the poles freeze");
        assert_eq!(whittaker(0.5, 0.6), Biome::Forest, "temperate and damp");
        // Moisture is monotonic within a temperature band: wetter never
        // yields a drier biome.
        assert_eq!(whittaker(0.9, 0.1), Biome::Desert);
        assert_eq!(whittaker(0.9, 0.4), Biome::Savanna);
        assert_eq!(whittaker(0.9, 0.7), Biome::Jungle);
    }

    // ── Generation ──────────────────────────────────────────────────────────

    #[test]
    fn generation_is_deterministic() {
        let a = World::generate(80, 60, 3);
        let b = World::generate(80, 60, 3);
        assert_eq!(a.elevation, b.elevation);
        assert_eq!(a.biome, b.biome);
        assert_eq!(a.river, b.river);
        assert_eq!(a.road, b.road);
        assert_eq!(a.province, b.province);
        assert_eq!(a.landmarks.len(), b.landmarks.len());
        for (x, y) in a.landmarks.iter().zip(&b.landmarks) {
            assert_eq!((x.x, x.y, x.site, &x.name), (y.x, y.y, y.site, &y.name));
        }
    }

    #[test]
    fn different_seeds_give_different_worlds() {
        let a = World::generate(80, 60, 1);
        let b = World::generate(80, 60, 2);
        assert_ne!(a.elevation, b.elevation);
    }

    #[test]
    #[should_panic(expected = "world must be at least 8x8")]
    fn tiny_worlds_are_rejected() {
        let _ = World::generate(4, 4, 1);
    }

    #[test]
    fn every_field_is_sized_to_the_map() {
        let w = world();
        let cells = (w.width() * w.height()) as usize;
        assert_eq!(w.elevation.len(), cells);
        assert_eq!(w.temperature.len(), cells);
        assert_eq!(w.moisture.len(), cells);
        assert_eq!(w.biome.len(), cells);
        assert_eq!(w.river.len(), cells);
        assert_eq!(w.road.len(), cells);
        assert_eq!(w.province.len(), cells);
    }

    #[test]
    fn scalar_fields_stay_normalized() {
        let w = world();
        for (i, &e) in w.elevation.iter().enumerate() {
            assert!((0.0..=1.0).contains(&e), "elevation[{i}] = {e}");
        }
        for (i, &t) in w.temperature.iter().enumerate() {
            assert!((0.0..=1.0).contains(&t), "temperature[{i}] = {t}");
        }
        for (i, &m) in w.moisture.iter().enumerate() {
            assert!((0.0..=1.0).contains(&m), "moisture[{i}] = {m}");
        }
    }

    #[test]
    fn elevation_actually_spans_its_range_after_normalization() {
        let w = world();
        let lo = w.elevation.iter().copied().fold(f32::MAX, f32::min);
        let hi = w.elevation.iter().copied().fold(f32::MIN, f32::max);
        assert!(lo < 0.01, "minimum should reach the floor, got {lo}");
        assert!(hi > 0.99, "maximum should reach the ceiling, got {hi}");
    }

    #[test]
    fn the_world_is_an_island() {
        let w = world();
        for x in 0..w.width() {
            assert!(w.biome_at(x, 0).is_water(), "top edge at x={x}");
            assert!(w.biome_at(x, w.height() - 1).is_water(), "bottom at x={x}");
        }
        for y in 0..w.height() {
            assert!(w.biome_at(0, y).is_water(), "left edge at y={y}");
            assert!(w.biome_at(w.width() - 1, y).is_water(), "right at y={y}");
        }
    }

    #[test]
    fn the_land_water_balance_is_stable_across_seeds() {
        // The whole reason elevation is normalized: a fixed sea level has to
        // mean the same thing on every seed, or some worlds are all ocean.
        for seed in [1, 2, 7, 42, 999] {
            let w = World::generate(100, 70, seed);
            let land = w.land_fraction();
            assert!(
                (0.20..0.65).contains(&land),
                "seed {seed}: land fraction {land:.2} outside the usable band"
            );
        }
    }

    #[test]
    fn biomes_agree_with_the_elevation_bands_that_produced_them() {
        let w = world();
        for y in 0..w.height() {
            for x in 0..w.width() {
                let i = w.idx(x, y).expect("in bounds");
                let (e, b) = (w.elevation[i], w.biome[i]);
                if b == Biome::Peak {
                    assert!(e >= PEAK_LEVEL, "peak at elevation {e}");
                } else if b == Biome::Mountain {
                    assert!(e >= MOUNTAIN_LEVEL, "mountain at elevation {e}");
                } else if b == Biome::Ocean || b == Biome::Sea {
                    assert!(e <= SEA_LEVEL, "sea at elevation {e}");
                }
            }
        }
    }

    #[test]
    fn a_reasonable_variety_of_biomes_appears() {
        let w = World::generate(200, 140, 7);
        let mut present: Vec<Biome> = w.biome;
        present.sort_unstable();
        present.dedup();
        assert!(
            present.len() >= 8,
            "only {} biomes on a large map: {present:?}",
            present.len()
        );
    }

    #[test]
    fn no_single_land_biome_swamps_the_map() {
        // A regression guard on the climate model, not a style preference.
        // Two bugs this catches, both of which produced maps that looked
        // plausible in a thumbnail and monotonous up close: a linear latitude
        // falloff that left a fifth of every map as tundra, and an fBm
        // moisture field clustered so tightly around its mean that no cell was
        // ever dry enough for desert or wet enough for jungle.
        for (w, h, seed) in [(200, 130, 12), (200, 140, 7), (120, 80, 3)] {
            let world = World::generate(w, h, seed);
            let land: Vec<Biome> = world
                .biome
                .iter()
                .copied()
                .filter(|b| !b.is_water())
                .collect();
            assert!(!land.is_empty(), "seed {seed} generated no land at all");

            let mut counts: HashMap<Biome, usize> = HashMap::new();
            for biome in &land {
                *counts.entry(*biome).or_default() += 1;
            }
            for (biome, n) in &counts {
                let share = *n as f32 / land.len() as f32;
                assert!(
                    share < 0.42,
                    "seed {seed}: {} covers {:.0}% of all land",
                    biome.name(),
                    share * 100.0
                );
            }
            assert!(
                counts.len() >= 7,
                "seed {seed}: only {} land biomes present",
                counts.len()
            );
        }
    }

    #[test]
    fn the_dry_and_wet_extremes_are_both_reachable() {
        // Desert and jungle are the two ends of the moisture axis. If the
        // moisture field never reaches its extremes, neither appears, and
        // every map is a uniform temperate green.
        let world = World::generate(240, 160, 7);
        for biome in [Biome::Desert, Biome::Jungle] {
            assert!(
                world.biome.contains(&biome),
                "{} never appears on a large map",
                biome.name()
            );
        }
    }

    #[test]
    fn out_of_bounds_lookups_are_ocean_not_a_panic() {
        let w = world();
        for (x, y) in [(-1, 0), (0, -1), (9999, 0), (0, 9999)] {
            assert!(!w.in_bounds(x, y));
            assert_eq!(w.idx(x, y), None);
            assert_eq!(w.biome_at(x, y), Biome::Ocean);
            assert!((w.elevation_at(x, y) - 0.0).abs() < f32::EPSILON);
            assert!(!w.river_at(x, y));
            assert!(!w.road_at(x, y));
            assert_eq!(w.province_at(x, y), 0);
        }
    }

    #[test]
    fn neighbors_never_leave_the_map() {
        let w = world();
        for (x, y) in [(0, 0), (w.width() - 1, 0), (0, w.height() - 1), (5, 5)] {
            let n = w.neighbors4(x, y);
            assert!(!n.is_empty());
            assert!(n.len() <= 4);
            for (nx, ny) in n {
                assert!(w.in_bounds(nx, ny), "({nx}, {ny}) escaped the map");
            }
        }
    }

    // ── Rivers, roads, landmarks ────────────────────────────────────────────

    #[test]
    fn rivers_exist_and_stay_on_land_or_end_in_water() {
        let w = World::generate(200, 140, 7);
        assert!(w.river.iter().any(|&r| r), "no rivers were carved");
        // Every river cell was land when it was carved; the only water a
        // river cell may be is a lake it pooled into.
        for (i, &r) in w.river.iter().enumerate() {
            if r {
                let b = w.biome[i];
                assert!(
                    !b.is_water() || b == Biome::Lake,
                    "river ran through {b:?} at index {i}"
                );
            }
        }
    }

    #[test]
    fn landmarks_are_on_passable_land_and_spaced_apart() {
        let w = world();
        assert!(!w.landmarks.is_empty(), "no landmarks placed");
        for l in &w.landmarks {
            assert!(w.in_bounds(l.x, l.y), "{} is off the map", l.name);
            let b = w.biome_at(l.x, l.y);
            assert!(b.is_passable(), "{} sits on {b:?}", l.name);
            assert!(!b.is_water(), "{} sits in water", l.name);
            assert!(!l.name.is_empty(), "unnamed landmark");
        }
        for (i, a) in w.landmarks.iter().enumerate() {
            for b in &w.landmarks[i + 1..] {
                assert!(
                    (a.x, a.y) != (b.x, b.y),
                    "{} and {} are stacked",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn exactly_one_capital_is_placed() {
        let w = world();
        let capitals = w
            .landmarks
            .iter()
            .filter(|l| l.site == Site::Capital)
            .count();
        assert_eq!(capitals, 1);
        assert_eq!(w.start_position(), {
            let c = w
                .landmarks
                .iter()
                .find(|l| l.site == Site::Capital)
                .expect("a capital");
            (c.x, c.y)
        });
    }

    #[test]
    fn roads_connect_settlements_over_passable_ground() {
        let w = world();
        assert!(w.road.iter().any(|&r| r), "no roads were built");
        for (i, &r) in w.road.iter().enumerate() {
            if r {
                assert!(
                    w.biome[i].is_passable(),
                    "road crosses impassable {:?}",
                    w.biome[i]
                );
            }
        }
        // Every settlement must sit on the network it was built for.
        for l in w.landmarks.iter().filter(|l| l.site.is_settlement()) {
            assert!(
                w.road_at(l.x, l.y),
                "{} ({:?}) is not connected",
                l.name,
                l.site
            );
        }
    }

    #[test]
    fn roads_are_contiguous_paths_not_scattered_cells() {
        let w = world();
        // Every road cell must touch another road cell, or routing emitted a
        // disconnected speck.
        for y in 0..w.height() {
            for x in 0..w.width() {
                if w.road_at(x, y) {
                    let touching = w
                        .neighbors4(x, y)
                        .iter()
                        .filter(|&&(nx, ny)| w.road_at(nx, ny))
                        .count();
                    assert!(touching > 0, "isolated road cell at ({x}, {y})");
                }
            }
        }
    }

    // ── Provinces ───────────────────────────────────────────────────────────

    #[test]
    fn every_cell_belongs_to_a_real_province() {
        let w = world();
        let count = w.province_count();
        assert!(count > 0);
        for (i, &p) in w.province.iter().enumerate() {
            assert!(p < count, "cell {i} claims province {p} of {count}");
        }
        assert_eq!(w.province_seeds.len(), count);
    }

    #[test]
    fn provinces_are_contiguous_around_their_seeds() {
        // Voronoi regions are convex by construction, so a seed must lie in
        // its own province. If it doesn't, relaxation and assignment have
        // fallen out of sync.
        let w = world();
        for (p, &(sx, sy)) in w.province_seeds.iter().enumerate() {
            assert_eq!(w.province_at(sx, sy), p, "seed {p} is not in province {p}");
        }
    }

    #[test]
    fn every_province_owns_some_land() {
        let w = World::generate(200, 140, 7);
        let mut land_counts = vec![0usize; w.province_count()];
        for y in 0..w.height() {
            for x in 0..w.width() {
                if !w.biome_at(x, y).is_water() {
                    land_counts[w.province_at(x, y)] += 1;
                }
            }
        }
        for (p, &n) in land_counts.iter().enumerate() {
            assert!(n > 0, "province {p} has no land");
        }
    }

    #[test]
    fn landmarks_know_which_province_they_are_in() {
        let w = world();
        for l in &w.landmarks {
            assert_eq!(
                l.province,
                w.province_at(l.x, l.y),
                "{} disagrees about its province",
                l.name
            );
        }
    }

    // ── Gradients and names ─────────────────────────────────────────────────

    #[test]
    fn gradients_are_finite_everywhere_including_the_edges() {
        let w = world();
        for y in 0..w.height() {
            for x in 0..w.width() {
                let (dx, dy) = w.gradient_at(x, y, 40.0);
                assert!(dx.is_finite() && dy.is_finite(), "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn gradients_point_uphill() {
        // Construct the check on real terrain: find a cell with a strong
        // east-west slope and confirm the gradient sign matches.
        let w = world();
        let mut checked = 0;
        for y in 1..w.height() - 1 {
            for x in 1..w.width() - 1 {
                let (dzdx, _) = w.gradient_at(x, y, 1.0);
                if dzdx.abs() < 0.02 {
                    continue;
                }
                let east = w.elevation_at(x + 1, y);
                let west = w.elevation_at(x - 1, y);
                assert_eq!(
                    dzdx > 0.0,
                    east > west,
                    "gradient sign disagrees with terrain at ({x}, {y})"
                );
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} sloped cells to check");
    }

    #[test]
    fn generated_names_are_capitalized_and_nonempty() {
        let mut rng = Rng::new(11);
        for _ in 0..200 {
            let name = generate_name(&mut rng);
            assert!(!name.is_empty());
            let first = name.chars().next().expect("nonempty");
            assert!(first.is_uppercase(), "{name:?} is not capitalized");
        }
    }

    #[test]
    fn generated_names_vary() {
        let mut rng = Rng::new(5);
        let mut names: Vec<String> = (0..100).map(|_| generate_name(&mut rng)).collect();
        names.sort();
        names.dedup();
        assert!(
            names.len() > 80,
            "only {} distinct names in 100",
            names.len()
        );
    }
}
