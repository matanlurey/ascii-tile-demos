//! 62: Quartered Arms -- procedural heraldry as a colored glyph mosaic, after
//! Ultima Ratio Regum's culture generator (Mark R. Johnson, 2012-present).
//!
//! URR's most-cited feature is not its dungeon but its culture generator:
//! every nation it invents gets a flag, a religion, and a visual language,
//! and that language then propagates onto every object the nation produces --
//! a shield, a book spine, a banner, a statue. Nothing else in this gallery
//! generates a *symbol*; every other demo generates terrain, a layout, or a
//! dungeon. This is the gap.
//!
//! **Attribution.** The research this demo is built from (Johnson's own
//! devlogs, summarized in `research-urr.md`) is explicit that URR itself does
//! not use formal heraldic terminology: it creates functional field divisions
//! and charges through shield shape, pattern placement, and color blocking,
//! not through blazons. The real heraldic vocabulary used throughout this
//! file (*per pale*, *per fess*, *quarterly*, *gyronny*, *roundel*, *lozenge*,
//! *orle*, the rule of tincture...) is **this demo's own choice**, made
//! because it is precise, centuries-documented, and gives the detail panel
//! something real to print. It is not a claim about how URR's own generator
//! is implemented.
//!
//! Techniques on show:
//!
//! - **Reject-and-retry generation** ([`generate_nations`]). Johnson's
//!   devlogs describe generators that check a freshly rolled result against
//!   everything already produced and re-roll on a near-miss, so a set of
//!   nations is guaranteed visually distinct rather than merely likely to be.
//!   Implemented here as a bounded retry loop keyed off [`fingerprint`] and
//!   [`mean_color_distance`] -- see those two functions for the exact
//!   similarity metric, defined precisely because "too similar" has to mean
//!   something a computer can check.
//! - **Chains of meaning.** [`Nation::color_at`] is one continuous function
//!   of shield-space coordinates, and it is the *only* thing that knows a
//!   nation's palette and field division. The crest mosaic, the banner
//!   pennant, and the statue's carved emblem all read a nation's identity
//!   through that one function (or, for the statue, through
//!   [`AestheticShape::charge_covers`], the boolean half of the same
//!   coordinate test), so they agree with each other by construction rather
//!   than by each artefact separately remembering to match the shield.
//! - **[`tilekit::glyphs::QuadrantCanvas`]**. Each crest is rendered at 2x2
//!   sub-cell resolution (a 7x7 shield becomes a 14x14-sample field), so a
//!   diagonal division (*per bend*, *gyronny*) or a round charge (a
//!   *roundel*) gets a smoothed edge instead of a staircase of whole cells.
//!   Both the crest and the banner artefact use it, since both sample the
//!   same continuous [`Nation::color_at`].
//! - **[`ascii_tile_demos::ui::touch::Shape`]**-driven reflow: a wide
//!   viewport puts the detail panel in a fixed-width sidebar beside the crest
//!   wall; a tall, narrow one stacks the wall above the panel instead, the
//!   same branch every touch-era demo in this gallery makes.
//!
//! Sources: Game Developer's 10-year retrospective interview with Johnson,
//! and Johnson's devlogs on shield/weapon and book generation (full citations
//! in `research-urr.md`). Those describe the aesthetic-shape vocabulary
//! (square, circle, octagon, cross, diamond, concentric square), a
//! two-or-three-tincture palette drawn from a nation's flag, and the
//! difference-checking generator this demo reproduces the spirit of.
//!
//! ```sh
//! cargo run --example 62_quartered_arms --features crossterm
//! cargo run --example 62_quartered_arms --features software
//! cargo run --example 62_quartered_arms --features gl
//! cargo run --example 62_quartered_arms  # headless, prints a few frames
//! ```

use core::f32::consts::PI;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::card;
use ascii_tile_demos::ui::panel::{self, Panel};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use retroglyph_widgets::truncate;
use tilekit::glyphs::QuadrantCanvas;
use tilekit::noise::{Rng, hash3};
use tilekit::palette::{self, rgb};

// ── Tinctures ────────────────────────────────────────────────────────────

/// One heraldic tincture: a name, a color, and whether it is a metal.
///
/// The rule of tincture (a color must not sit directly on another color, nor
/// a metal on another metal) is the one heraldic convention this generator
/// enforces; see [`Nation::generate`]. Real heraldry has dozens of subtler
/// blazon conventions this does not model.
#[derive(Clone, Copy)]
struct Tincture {
    name: &'static str,
    color: Color,
    metal: bool,
}

/// The seven tinctures a medieval European shield could actually be painted
/// in: two metals (or, argent) and five colors (gules, azure, vert, sable,
/// purpure). Real heraldry names a few more (murrey, sanguine, tenné) as
/// "stains", rarely used and omitted here.
const TINCTURES: [Tincture; 7] = [
    Tincture {
        name: "or",
        color: rgb(210, 168, 66),
        metal: true,
    },
    Tincture {
        name: "argent",
        color: rgb(224, 224, 218),
        metal: true,
    },
    Tincture {
        name: "gules",
        color: rgb(176, 34, 46),
        metal: false,
    },
    Tincture {
        name: "azure",
        color: rgb(40, 72, 156),
        metal: false,
    },
    Tincture {
        name: "vert",
        color: rgb(36, 118, 62),
        metal: false,
    },
    Tincture {
        name: "sable",
        color: rgb(26, 26, 30),
        metal: false,
    },
    Tincture {
        name: "purpure",
        color: rgb(110, 46, 128),
        metal: false,
    },
];

// ── Aesthetic shapes ─────────────────────────────────────────────────────

/// A nation's aesthetic-shape preference: the geometric motif Johnson's
/// devlogs describe recurring across a nation's shields, thrones, and
/// architecture. Here it is literally the shield's central charge, and it
/// recurs on every other artefact the nation produces (see [`draw_book_spine`],
/// [`draw_banner`], [`draw_statue`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AestheticShape {
    Square,
    Circle,
    Octagon,
    Cross,
    Diamond,
    ConcentricSquare,
}

impl AestheticShape {
    const ALL: [Self; 6] = [
        Self::Square,
        Self::Circle,
        Self::Octagon,
        Self::Cross,
        Self::Diamond,
        Self::ConcentricSquare,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Circle => "Circle",
            Self::Octagon => "Octagon",
            Self::Cross => "Cross",
            Self::Diamond => "Diamond",
            Self::ConcentricSquare => "Concentric square",
        }
    }

    /// The charge's name in a blazon. Three of six map onto real heraldic
    /// charges (*roundel*, *lozenge*, *cross throughout*); the other three
    /// have no traditional equivalent and are named plainly, which is
    /// consistent with this demo's honesty about which vocabulary is
    /// invented and which is borrowed.
    const fn charge_blazon(self) -> &'static str {
        match self {
            Self::Square => "a square",
            Self::Circle => "a roundel",
            Self::Octagon => "an octagon",
            Self::Cross => "a cross throughout",
            Self::Diamond => "a lozenge",
            // A real orle is a narrow border following the shield's own
            // outline, offset inward -- the closest traditional charge to
            // "the shield's own shape, repeated smaller inside itself",
            // which is what a concentric-square aesthetic preference is.
            Self::ConcentricSquare => "an orle",
        }
    }

    /// A single CP437 glyph standing in for the shape on artefacts too small
    /// to render the full charge silhouette (the book spine, the statue).
    /// Chosen from `examples/tests/glyphs.rs`'s safe set rather than from
    /// Unicode geometric shapes at large: `□` and a true octagon glyph are
    /// outside CP437 and would render as a solid block on the pixel
    /// backends.
    const fn glyph(self) -> char {
        match self {
            Self::Square => '\u{25A0}',  // ■ CP437 0xFE
            Self::Circle => '\u{25CB}',  // ○ CP437 0x09
            Self::Octagon => '\u{25D9}', // ◙ CP437 0x0A, a ringed stand-in
            Self::Cross => '+',
            Self::Diamond => '\u{2666}',          // ♦ CP437 0x04
            Self::ConcentricSquare => '\u{25D8}', // ◘ CP437 0x08, a bordered stand-in
        }
    }

    /// Whether shield-space point `(u, v)` (each in `-1.0..=1.0`, origin at
    /// the shield's center) falls inside this shape's charge silhouette.
    /// Sampled continuously so [`draw_crest`]'s higher sub-cell resolution
    /// actually buys a smoother edge rather than the same staircase at twice
    /// the density.
    fn charge_covers(self, u: f32, v: f32) -> bool {
        match self {
            Self::Square => u.abs() <= 0.5 && v.abs() <= 0.5,
            Self::Circle => u.mul_add(u, v * v) <= 0.36,
            Self::Octagon => u.abs() <= 0.6 && v.abs() <= 0.6 && u.abs() + v.abs() <= 0.9,
            // "Throughout" in blazon means the charge touches the shield's
            // own edge rather than being confined to the center, which is
            // why the arms of this cross run the full -1..1 span.
            Self::Cross => u.abs() <= 0.22 || v.abs() <= 0.22,
            Self::Diamond => u.abs() + v.abs() <= 0.62,
            Self::ConcentricSquare => {
                let ring = u.abs().max(v.abs());
                (0.35..=0.55).contains(&ring)
            }
        }
    }
}

// ── Field divisions ──────────────────────────────────────────────────────

/// A field division: how the shield's background is split between its first
/// two tinctures before the charge is drawn on top. Six of the real
/// vocabulary's divisions, picked because each is a cheap, distinctive
/// coordinate test.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldDivision {
    Plain,
    PerPale,
    PerFess,
    PerBend,
    Quarterly,
    Gyronny,
}

impl FieldDivision {
    const ALL: [Self; 6] = [
        Self::Plain,
        Self::PerPale,
        Self::PerFess,
        Self::PerBend,
        Self::Quarterly,
        Self::Gyronny,
    ];

    /// Whether `(u, v)` falls in the field's second tincture rather than its
    /// first. [`Self::Plain`] never does, since a plain field has only one
    /// tincture (the charge still gets a second one to sit on).
    fn second_tincture(self, u: f32, v: f32) -> bool {
        match self {
            Self::Plain => false,
            Self::PerPale => u >= 0.0,
            Self::PerFess => v >= 0.0,
            Self::PerBend => u + v >= 0.0,
            // A checkerboard of the quadrant signs is exactly heraldic
            // quarterly: quarters I and IV share a tincture, II and III
            // share the other.
            Self::Quarterly => (u >= 0.0) != (v >= 0.0),
            // Eight wedges radiating from the center, alternating tincture
            // by which 45-degree sector the point falls in.
            Self::Gyronny => {
                let sector = ((v.atan2(u) + PI) / (PI / 4.0)).floor() as i32;
                sector.rem_euclid(2) == 1
            }
        }
    }

    /// The blazon's opening clause, e.g. `"Per pale gules and or"`, given the
    /// two field tincture names. [`Self::Plain`] has no "per X ... and ..."
    /// clause at all: a plain field is blazoned by its one tincture's name
    /// alone.
    fn blazon_field(self, a: &str, b: &str) -> String {
        match self {
            Self::Plain => a.to_string(),
            Self::PerPale => format!("Per pale {a} and {b}"),
            Self::PerFess => format!("Per fess {a} and {b}"),
            Self::PerBend => format!("Per bend {a} and {b}"),
            Self::Quarterly => format!("Quarterly {a} and {b}"),
            Self::Gyronny => format!("Gyronny {a} and {b}"),
        }
    }
}

// ── Nation ───────────────────────────────────────────────────────────────

/// A generated nation: a name, a shape preference, a field division, and a
/// three-tincture palette. This is the entire unit of identity the rest of
/// the demo propagates onto every artefact.
struct Nation {
    name: String,
    shape: AestheticShape,
    division: FieldDivision,
    /// `[field primary, field secondary, charge]`. The charge tincture is
    /// picked to differ in metal-ness from the field's primary, which is the
    /// one case the rule of tincture actually governs here: the charge sits
    /// on the field's primary tincture over most of the shield (every
    /// division here favors it, by construction, for more than half the
    /// area), so that is the pairing most worth protecting.
    tinctures: [Tincture; 3],
}

impl Nation {
    /// Generates one candidate nation from `rng`. Not itself responsible for
    /// uniqueness against other nations; see [`generate_nations`] for the
    /// reject-and-retry pass that calls this in a loop.
    fn generate(rng: &mut Rng) -> Self {
        let shape = AestheticShape::ALL[rng.next_below(AestheticShape::ALL.len() as u32) as usize];
        let division = FieldDivision::ALL[rng.next_below(FieldDivision::ALL.len() as u32) as usize];

        let metal_index = usize::from(rng.next_f32() >= 0.5);
        let color_index = 2 + rng.next_below(5) as usize;
        let (field_a, field_b) = if rng.next_f32() < 0.5 {
            (metal_index, color_index)
        } else {
            (color_index, metal_index)
        };

        let charge_candidates: Vec<usize> = (0..TINCTURES.len())
            .filter(|&i| {
                i != field_a && i != field_b && TINCTURES[i].metal != TINCTURES[field_a].metal
            })
            .collect();
        let charge = if charge_candidates.is_empty() {
            // Every tincture of the opposite metal-ness is already taken by
            // the field (impossible with 7 tinctures and 2 metals, but
            // guarded rather than assumed): fall back to anything distinct
            // from both field tinctures so the charge is still visible as
            // its own color.
            (0..TINCTURES.len())
                .find(|&i| i != field_a && i != field_b)
                .unwrap_or(field_a)
        } else {
            charge_candidates[rng.next_below(charge_candidates.len() as u32) as usize]
        };

        Self {
            name: generate_name(rng),
            shape,
            division,
            tinctures: [TINCTURES[field_a], TINCTURES[field_b], TINCTURES[charge]],
        }
    }

    /// The color at continuous shield coordinates `(u, v)`, each in
    /// `-1.0..=1.0` with the origin at the shield's center. This one
    /// function is the entire visual definition of a nation's arms: the crest
    /// mosaic samples it on a grid, the banner artefact samples it on a
    /// notched grid, and nothing downstream ever re-derives a nation's colors
    /// by any other path. That is what makes the "chains of meaning" the
    /// brief asks for automatic rather than a matter of remembering to keep
    /// several drawing functions in sync.
    fn color_at(&self, u: f32, v: f32) -> Color {
        let field = if self.division.second_tincture(u, v) {
            self.tinctures[1].color
        } else {
            self.tinctures[0].color
        };
        if self.shape.charge_covers(u, v) {
            self.tinctures[2].color
        } else {
            field
        }
    }

    /// The full blazon, e.g. `"Per pale gules and or, a cross throughout
    /// sable"`.
    fn blazon(&self) -> String {
        let field = self
            .division
            .blazon_field(self.tinctures[0].name, self.tinctures[1].name);
        format!(
            "{field}, {} {}",
            self.shape.charge_blazon(),
            self.tinctures[2].name
        )
    }
}

// ── Name generation ──────────────────────────────────────────────────────
//
// `tilekit::world::generate_name` exists but is private to that module (it
// backs `World`'s own landmark naming and is not part of `tilekit`'s public
// surface), so this is a small generator of its own, built in the same
// archetype spirit the brief quotes from URR's devlogs: "The Riven Lands",
// "The Artificer Kingdom of Upumnyx".

const NAME_EPITHETS: [&str; 10] = [
    "Riven",
    "Gilded",
    "Sundered",
    "Ashen",
    "Drowned",
    "Wandering",
    "Iron",
    "Hollow",
    "Verdant",
    "Thorned",
];
const NAME_LANDS: [&str; 10] = [
    "Lands", "Reaches", "Marches", "Coast", "Wastes", "Isles", "Downs", "Fens", "Steppe", "Crown",
];
const NAME_TITLES: [&str; 8] = [
    "Kingdom",
    "Dominion",
    "Republic",
    "Hegemony",
    "Free Cities",
    "Satrapy",
    "Confederacy",
    "Khanate",
];
const NAME_KIND_EPITHETS: [&str; 10] = [
    "Artificer",
    "Sunken",
    "Cinder",
    "Salt",
    "Bone",
    "Amber",
    "Storm",
    "Weaver",
    "Marrow",
    "Coral",
];
const NAME_SYLLABLE_ONSETS: [&str; 14] = [
    "p", "b", "t", "d", "k", "g", "m", "n", "r", "l", "s", "v", "th", "zh",
];
const NAME_SYLLABLE_RIMES: [&str; 10] = ["a", "e", "i", "o", "u", "um", "ix", "yx", "an", "or"];

/// A short invented place-name, e.g. "Upumnyx": two or three syllables, each
/// a consonant onset plus a vowel-ish rime, capitalized.
fn invented_word(rng: &mut Rng) -> String {
    let syllables = 2 + usize::from(rng.next_f32() < 0.4);
    let mut word = String::new();
    for _ in 0..syllables {
        word.push_str(
            NAME_SYLLABLE_ONSETS[rng.next_below(NAME_SYLLABLE_ONSETS.len() as u32) as usize],
        );
        word.push_str(
            NAME_SYLLABLE_RIMES[rng.next_below(NAME_SYLLABLE_RIMES.len() as u32) as usize],
        );
    }
    let mut chars = word.chars();
    chars.next().map_or_else(
        || word.clone(),
        |first| first.to_uppercase().collect::<String>() + chars.as_str(),
    )
}

/// One of two name archetypes, chosen with equal probability: "The `{epithet}`
/// `{lands}`" (e.g. "The Riven Lands") or "The `{epithet}` `{title}` of
/// `{invented}`" (e.g. "The Artificer Kingdom of Upumnyx").
fn generate_name(rng: &mut Rng) -> String {
    if rng.next_f32() < 0.5 {
        let epithet = NAME_EPITHETS[rng.next_below(NAME_EPITHETS.len() as u32) as usize];
        let lands = NAME_LANDS[rng.next_below(NAME_LANDS.len() as u32) as usize];
        format!("The {epithet} {lands}")
    } else {
        let epithet = NAME_KIND_EPITHETS[rng.next_below(NAME_KIND_EPITHETS.len() as u32) as usize];
        let title = NAME_TITLES[rng.next_below(NAME_TITLES.len() as u32) as usize];
        let place = invented_word(rng);
        format!("The {epithet} {title} of {place}")
    }
}

// ── Reject-and-retry generation ──────────────────────────────────────────

/// Side length of a nation's similarity "fingerprint" grid: [`Nation::color_at`]
/// sampled at `FINGERPRINT_N` x `FINGERPRINT_N` fixed points across the
/// shield. Independent of any on-screen crest's resolution -- this exists
/// purely to compare two nations cheaply during generation, long before
/// either one is drawn.
const FINGERPRINT_N: usize = 5;

type Fingerprint = [[Color; FINGERPRINT_N]; FINGERPRINT_N];

/// Samples [`Nation::color_at`] on a `FINGERPRINT_N`x`FINGERPRINT_N` grid
/// spanning the shield.
fn fingerprint(nation: &Nation) -> Fingerprint {
    let mut grid = [[palette::BLACK; FINGERPRINT_N]; FINGERPRINT_N];
    for (row, line) in grid.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            let u = (col as f32 / (FINGERPRINT_N - 1) as f32).mul_add(2.0, -1.0);
            let v = (row as f32 / (FINGERPRINT_N - 1) as f32).mul_add(2.0, -1.0);
            *cell = nation.color_at(u, v);
        }
    }
    grid
}

/// Euclidean RGB distance between two colors. `Color::Rgb` is the only
/// variant this file ever constructs (every tincture and every mix comes from
/// [`palette::rgb`]), so unmatched variants fall back to pure black rather
/// than adding a case that can never actually be reached from this file's own
/// data.
fn color_distance(a: Color, b: Color) -> f32 {
    let Color::Rgb {
        r: ar,
        g: ag,
        b: ab,
    } = a
    else {
        return 0.0;
    };
    let Color::Rgb {
        r: br,
        g: bg,
        b: bb,
    } = b
    else {
        return 0.0;
    };
    let (dr, dg, db) = (
        f32::from(ar) - f32::from(br),
        f32::from(ag) - f32::from(bg),
        f32::from(ab) - f32::from(bb),
    );
    dr.mul_add(dr, dg.mul_add(dg, db * db)).sqrt()
}

/// The similarity metric the reject-and-retry pass in [`generate_nations`]
/// runs on every candidate: the mean per-sample-point RGB distance between
/// two nations' [`fingerprint`]s, normalized to `0.0..=1.0` by the RGB cube's
/// diagonal (`sqrt(3 * 255^2)`, the largest distance two colors can have).
///
/// This is deliberately simple and deliberately not shape-aware: two crests
/// read as "the same" to a viewer exactly when most of the 25 fixed sample
/// points land on close colors, which happens when two nations share a field
/// division (so corresponding points fall on the same tincture) and a nearby
/// palette -- regardless of what the shield's own charge is named. A result
/// near `0.0` means the two crests are close to indistinguishable; a result
/// near `1.0` means every sample point landed on a maximally different color.
fn mean_color_distance(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let max_distance = (3.0f32 * 255.0 * 255.0).sqrt();
    let mut total = 0.0f32;
    for row in 0..FINGERPRINT_N {
        for col in 0..FINGERPRINT_N {
            total += color_distance(a[row][col], b[row][col]);
        }
    }
    total / (FINGERPRINT_N * FINGERPRINT_N) as f32 / max_distance
}

/// How many nations the wall of crests shows. Nine rather than a rounder
/// number so the default 3-column desktop layout (see [`QuarteredArms::draw`])
/// lands on an exact 3x3 grid, while still reflowing cleanly to 8-per-row on
/// a portrait phone (see the `phone_shapes` test).
const NUM_NATIONS: usize = 9;

/// Two crests below this [`mean_color_distance`] are rejected as too similar
/// and the candidate is rerolled. Tuned empirically against
/// [`TINCTURES`]/[`AestheticShape::ALL`]/[`FieldDivision::ALL`]'s actual
/// combinatorics (7 tinctures, 6 shapes, 6 divisions): low enough that
/// [`NUM_NATIONS`] nations almost always resolve well inside
/// [`MAX_RETRY_ATTEMPTS`], high enough to catch the case this pass exists
/// for -- two nations landing on the same division and a near-identical
/// palette, which without a retry happens often enough to be visible in a
/// wall of only nine crests.
const SIMILARITY_THRESHOLD: f32 = 0.16;

/// Reroll budget per nation before the generator gives up and accepts
/// whatever it has. Bounded so generation always terminates in a fixed
/// number of steps regardless of seed; 200 is generous relative to the
/// palette/shape/division combinatorics above; a fallback nation this far
/// into the tail no longer matters for the demo's own visual point, only for
/// making sure `generate_nations` can never hang.
const MAX_RETRY_ATTEMPTS: u32 = 200;

/// Generates [`NUM_NATIONS`] nations from `seed`, guaranteeing (up to
/// [`MAX_RETRY_ATTEMPTS`]) that no two are within [`SIMILARITY_THRESHOLD`] of
/// each other by [`mean_color_distance`], and that no two share a caption.
/// This is the reject-and-retry pass the brief asks for: Johnson's devlogs
/// describe the culture generator checking a freshly rolled result against
/// everything already produced and re-rolling on a near-miss, and this is the
/// same shape of loop against a metric cheap enough to run every attempt.
fn generate_nations(seed: u32) -> Vec<Nation> {
    let mut nations = Vec::with_capacity(NUM_NATIONS);
    let mut fingerprints: Vec<Fingerprint> = Vec::with_capacity(NUM_NATIONS);
    let mut captions: Vec<String> = Vec::with_capacity(NUM_NATIONS);

    for index in 0..NUM_NATIONS {
        let mut attempt = 0u32;
        loop {
            // A fresh Rng per attempt, seeded from `seed`, the nation's own
            // index, and the attempt number, so a reroll is a genuinely
            // different draw rather than a repeat of the same rejected one,
            // while the whole pass stays a pure function of `seed`.
            let mut rng = Rng::new(hash3(seed, index as i32, attempt as i32));
            let candidate = Nation::generate(&mut rng);
            let candidate_fp = fingerprint(&candidate);

            let too_similar = fingerprints.iter().any(|existing| {
                mean_color_distance(existing, &candidate_fp) < SIMILARITY_THRESHOLD
            });

            // Distinctness has to hold at the resolution the viewer actually
            // sees. Two nations can differ by full name and still print the
            // same caption, because the wall truncates to `TILE_W - 1`: "The
            // Gilded Coast" and "The Gilded Reach" both read "The Gild". A
            // wall with two identically-labelled crests reads as a generator
            // bug whether or not the crests differ, so the caption is part of
            // the uniqueness contract rather than a cosmetic afterthought.
            let caption = caption_for(&candidate.name);
            let caption_taken = captions.contains(&caption);

            if (!too_similar && !caption_taken) || attempt >= MAX_RETRY_ATTEMPTS {
                fingerprints.push(candidate_fp);
                captions.push(caption);
                nations.push(candidate);
                break;
            }
            attempt += 1;
        }
    }

    nations
}

/// The name as the wall actually prints it: truncated to the tile's caption
/// width. Shared by [`generate_nations`]'s uniqueness check and
/// [`draw_crest_tile`]'s rendering so the two cannot disagree about what a
/// caption is.
fn caption_for(name: &str) -> String {
    truncate(name, usize::from(TILE_W - 1)).to_owned()
}

// ── Crest and artefact rendering ─────────────────────────────────────────

/// Side length, in cells, of one crest in the wall grid.
const CREST_N: u16 = 7;
/// Width of one wall tile: [`CREST_N`] plus one cell of padding on each side,
/// which is also where the selection corner marks land (see [`draw_crest_tile`]).
const TILE_W: u16 = CREST_N + 2;
/// Height of one wall tile: top padding, the crest, bottom padding, and a
/// caption row for the nation's name.
const TILE_H: u16 = CREST_N + 3;

/// Renders `nation`'s crest into `area` via [`QuadrantCanvas`]: each screen
/// cell gets 2x2 sub-cell samples of [`Nation::color_at`], so a diagonal
/// division or a round charge gets a smoothed edge rather than the coarser
/// staircase a one-sample-per-cell fill would produce at the same footprint.
fn draw_crest(surface: &mut Surface<'_>, area: Rect, nation: &Nation) {
    if area.width() == 0 || area.height() == 0 {
        return;
    }
    let mut canvas = QuadrantCanvas::new(area.width(), area.height(), ui::BG);
    let (sub_w, sub_h) = canvas.size();
    for sy in 0..sub_h {
        for sx in 0..sub_w {
            let u = ((sx as f32 + 0.5) / sub_w as f32).mul_add(2.0, -1.0);
            let v = ((sy as f32 + 0.5) / sub_h as f32).mul_add(2.0, -1.0);
            canvas.plot(sx as i32, sy as i32, nation.color_at(u, v));
        }
    }
    for (col, row, glyph) in canvas.cells() {
        surface.put(
            (area.left() + col, area.top() + row),
            glyph.ch,
            Style::new().fg(glyph.fg).bg(glyph.bg),
        );
    }
}

/// Draws one wall tile: the crest, a caption with the nation's name, and (if
/// `selected`) corner marks in the accent color that breathe gently with
/// `time`. The corners land in the tile's own padding cells, never on the
/// crest itself, so selection never obscures the thing it is pointing at.
///
/// The pulse (rather than a static accent) is the only thing on this screen
/// that has a reason to move on its own: every other pixel is a pure function
/// of the selected nation and only changes on input. A hard on/off blink
/// would compete with the crests for attention; blending toward white and
/// back is readable as "this is the live cursor" without it.
fn draw_crest_tile(
    surface: &mut Surface<'_>,
    tile: Rect,
    nation: &Nation,
    selected: bool,
    elapsed: f32,
) {
    let corner_color = if selected {
        let pulse = (elapsed * 2.0).sin().mul_add(0.5, 0.5);
        palette::mix(ui::ACCENT, palette::WHITE, pulse * 0.4)
    } else {
        panel::FRAME
    };
    let corner_style = Style::new().fg(corner_color).bg(ui::BG);
    let (left, top) = (tile.left(), tile.top());
    let right = tile.left() + TILE_W - 1;
    let crest_bottom = top + 1 + CREST_N; // row just below the crest

    surface.put((left, top), '\u{250C}', corner_style);
    surface.put((right, top), '\u{2510}', corner_style);
    surface.put((left, crest_bottom), '\u{2514}', corner_style);
    surface.put((right, crest_bottom), '\u{2518}', corner_style);

    let crest_area = Rect::new(left + 1, top + 1, CREST_N, CREST_N);
    draw_crest(surface, crest_area, nation);

    let caption_y = crest_bottom + 1;
    let caption_style = Style::new()
        .fg(if selected { ui::ACCENT } else { ui::DIM })
        .bg(ui::BG);
    // Truncated to one cell short of the full tile width, and never padded
    // out to fill it: the unwritten column is what keeps two adjacent
    // captions from visually running together when both are close to the
    // tile's full width, since the wall grid otherwise has no gap between
    // tiles at all. Goes through `caption_for` because `generate_nations`
    // rejects duplicate captions using the same function; if these two ever
    // truncated differently, the uniqueness guarantee would be checking a
    // string the wall never prints.
    let caption = caption_for(&nation.name);
    let pad = (TILE_W - 1 - caption.chars().count() as u16) / 2;
    surface.print((left + pad, caption_y), &caption, caption_style);
}

/// Width/height of the book-spine, banner, and statue artefacts in the
/// detail panel's "chains of meaning" row. All three share a height so they
/// sit flush along one baseline.
const ARTEFACT_H: u16 = 9;
const BOOK_W: u16 = 5;
const BANNER_W: u16 = 7;
const STATUE_W: u16 = 7;
/// Horizontal gap between artefacts.
const ARTEFACT_GAP: u16 = 2;

/// A book spine: horizontal tincture bands (research: "lines on spine
/// indicate aesthetic preference... spine shows flag colors") with the
/// nation's shape glyph stamped on the top band, standing in for the title
/// panel a real spine would carve or gild.
fn draw_book_spine(surface: &mut Surface<'_>, rect: Rect, nation: &Nation) {
    let field = nation.tinctures[0].color;
    let band = nation.tinctures[1].color;
    let charge = nation.tinctures[2].color;
    let h = rect.height();
    for row in 0..h {
        // A band every third row plus the top and base rows, so the pattern
        // reads as deliberate ruling rather than a single stripe.
        let is_band = row == 0 || row + 1 == h || row % 3 == 0;
        let bg = if is_band { band } else { field };
        for col in 0..rect.width() {
            surface.put(
                (rect.left() + col, rect.top() + row),
                ' ',
                Style::new().bg(bg),
            );
        }
    }
    let cx = rect.left() + rect.width() / 2;
    surface.put(
        (cx, rect.top()),
        nation.shape.glyph(),
        Style::new().fg(charge).bg(band),
    );
}

/// A banner: [`Nation::color_at`] sampled directly across the pennant, with a
/// swallow-tail notch carved out of the bottom third. Reusing the crest's own
/// color function (rather than re-deriving a simplified palette) is the
/// clearest single instance of "chains of meaning" this demo draws: the
/// banner is not *matching* the shield, it is *reading the same function* the
/// shield reads.
fn draw_banner(surface: &mut Surface<'_>, rect: Rect, nation: &Nation) {
    if rect.width() == 0 || rect.height() == 0 {
        return;
    }
    let mut canvas = QuadrantCanvas::new(rect.width(), rect.height(), ui::BG);
    let (sub_w, sub_h) = canvas.size();
    for sy in 0..sub_h {
        let v_frac = (sy as f32 + 0.5) / sub_h as f32; // 0 (top) .. 1 (bottom)
        for sx in 0..sub_w {
            let u = ((sx as f32 + 0.5) / sub_w as f32).mul_add(2.0, -1.0);
            let in_tail = v_frac > 0.7 && {
                let notch_depth = (v_frac - 0.7) / 0.3;
                u.abs() < notch_depth * 0.85
            };
            if in_tail {
                continue; // leave the notch as the canvas's own clear color
            }
            let v = v_frac.mul_add(2.0, -1.0);
            canvas.plot(sx as i32, sy as i32, nation.color_at(u, v));
        }
    }
    for (col, row, glyph) in canvas.cells() {
        surface.put(
            (rect.left() + col, rect.top() + row),
            glyph.ch,
            Style::new().fg(glyph.fg).bg(glyph.bg),
        );
    }
}

/// A statue plinth: the shape glyph carved into a solid tincture-primary
/// body, on a tincture-secondary base course. Research: statues etch a
/// religion's or nation's symbol onto a shaded ASCII shape; this is that
/// idea's smallest legible form.
fn draw_statue(surface: &mut Surface<'_>, rect: Rect, nation: &Nation) {
    let h = rect.height();
    let base_rows = 2.min(h);
    let body_rows = h - base_rows;
    let body = nation.tinctures[0].color;
    let base = nation.tinctures[1].color;
    let charge = nation.tinctures[2].color;

    for row in 0..body_rows {
        for col in 0..rect.width() {
            surface.put(
                (rect.left() + col, rect.top() + row),
                ' ',
                Style::new().bg(body),
            );
        }
    }
    if body_rows > 0 {
        let cx = rect.left() + rect.width() / 2;
        let cy = rect.top() + body_rows / 2;
        surface.put(
            (cx, cy),
            nation.shape.glyph(),
            Style::new().fg(charge).bg(body),
        );
    }
    for row in 0..base_rows {
        let y = rect.top() + body_rows + row;
        let ch = if row == 0 { '\u{2550}' } else { ' ' };
        for col in 0..rect.width() {
            surface.put((rect.left() + col, y), ch, Style::new().fg(base).bg(base));
        }
    }
}

// ── Layout ───────────────────────────────────────────────────────────────

/// Width of the fixed-width detail sidebar in non-stacking (wide) layouts.
const DETAIL_W: u16 = 34;

/// A tap target: one crest tile.
#[derive(Clone, Copy)]
struct Action(usize);

/// State: the generated nations, the current selection, the world seed, and
/// the touch/keyboard plumbing every interface demo shares.
pub struct QuarteredArms {
    nations: Vec<Nation>,
    selected: usize,
    seed: u32,
    /// Columns the wall grid last laid out with, recorded during
    /// [`QuarteredArms::draw_wall`] so arrow-key navigation (which runs
    /// before the next draw) can reason about the same grid the player is
    /// looking at.
    wall_cols: u16,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for QuarteredArms {
    fn default() -> Self {
        let seed = 11;
        Self {
            nations: generate_nations(seed),
            selected: 0,
            seed,
            wall_cols: 1,
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl QuarteredArms {
    fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.nations = generate_nations(self.seed);
        self.selected = 0;
    }

    /// Moves the selection by one grid step, wrapping within the row/column
    /// it moved along. The last row of the wall grid may be short (9 nations
    /// does not evenly divide most column counts), so the result is clamped
    /// back onto a real nation rather than landing past the end of it.
    fn move_selection(&mut self, dx: i32, dy: i32) {
        let cols = i32::from(self.wall_cols.max(1));
        let n = self.nations.len() as i32;
        if n == 0 {
            return;
        }
        // `div_ceil` on a signed integer is still unstable (see 02_chunky_tiles.rs),
        // hence the manual ceiling division.
        let rows = ((n + cols - 1) / cols).max(1);
        let cur = self.selected as i32;
        let col = (cur % cols + dx).rem_euclid(cols);
        let row = (cur / cols + dy).rem_euclid(rows);
        let idx = (row * cols + col).min(n - 1);
        self.selected = idx as usize;
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                match key.code {
                    KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_selection(0, -1),
                    KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_selection(0, 1),
                    KeyCode::Left | KeyCode::Char('a' | 'A') => self.move_selection(-1, 0),
                    KeyCode::Right | KeyCode::Char('d' | 'D') => self.move_selection(1, 0),
                    KeyCode::Char('r' | 'R') => self.reroll(),
                    _ => {}
                }
            }
            self.pointer.feed(&event);
        }
        true
    }

    /// Draws the crest wall into `area`: a heading, then a responsive grid of
    /// [`draw_crest_tile`]s. Records the column count into
    /// [`Self::wall_cols`] and rebuilds every tile's hotspot, so this is the
    /// single source of truth both [`Self::move_selection`] and tap input
    /// read against.
    fn draw_wall(&mut self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        surface.print(
            (area.left(), area.top()),
            &format!("NATIONS ({})", self.nations.len()),
            Style::new().fg(ui::ACCENT).bg(ui::BG),
        );
        let grid_area = Rect::new(
            area.left(),
            area.top() + 1,
            area.width(),
            area.height().saturating_sub(1),
        );
        let cols = (grid_area.width() / TILE_W)
            .max(1)
            .min(self.nations.len() as u16);
        self.wall_cols = cols;

        for i in 0..self.nations.len() {
            let col = i as u16 % cols;
            let row = i as u16 / cols;
            let tx = grid_area.left() + col * TILE_W;
            let ty = grid_area.top() + row * TILE_H;
            if ty + TILE_H > grid_area.bottom() && row > 0 {
                // Ran out of vertical room past the first row: stop laying
                // out rather than drawing tiles that overrun the panel.
                break;
            }
            let tile_rect = Rect::new(tx, ty, TILE_W, TILE_H);
            self.hotspots.push(tile_rect, Action(i));
            draw_crest_tile(
                surface,
                tile_rect,
                &self.nations[i],
                i == self.selected,
                self.time,
            );
        }
    }

    /// Draws the selected nation's detail panel: name, blazon, aesthetic
    /// shape, tincture swatches, and the artefact row. Every element checks
    /// remaining room before drawing, so a panel too short to fit everything
    /// drops the artefact row (the least essential part, and the tallest)
    /// rather than overrunning or panicking.
    fn draw_detail(&self, surface: &mut Surface<'_>, area: Rect) {
        let Some(nation) = self.nations.get(self.selected) else {
            return;
        };
        let inner = Panel::new()
            .title(&nation.name)
            .bg(panel::PANEL_BG)
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let bg = panel::PANEL_BG;
        let mut y = inner.top();

        for line in card::wrap(&nation.blazon(), usize::from(inner.width()))
            .into_iter()
            .take(3)
        {
            if y >= inner.bottom() {
                return;
            }
            surface.print((inner.left(), y), &line, Style::new().fg(ui::FG).bg(bg));
            y += 1;
        }
        y += 1;

        if y < inner.bottom() {
            let text = format!("Aesthetic shape: {}", nation.shape.label());
            surface.print(
                (inner.left(), y),
                truncate(&text, usize::from(inner.width())),
                Style::new().fg(ui::DIM).bg(bg),
            );
            y += 1;
        }
        y += 1;

        if y < inner.bottom() {
            let mut x = inner.left();
            surface.print((x, y), "Tinctures: ", Style::new().fg(ui::DIM).bg(bg));
            x += 11;
            for tincture in &nation.tinctures {
                if x >= inner.right() {
                    break;
                }
                surface.put((x, y), '\u{2588}', Style::new().fg(tincture.color).bg(bg));
                x += 1;
                let label = format!("{} ", tincture.name);
                let room = usize::from(inner.right().saturating_sub(x));
                let text = truncate(&label, room);
                surface.print((x, y), text, Style::new().fg(ui::FG).bg(bg));
                x += text.chars().count() as u16;
            }
            y += 1;
        }
        y += 1;

        if y < inner.bottom() {
            let label = truncate(
                "ARTEFACTS -- same palette, same shape:",
                usize::from(inner.width()),
            );
            surface.print((inner.left(), y), label, Style::new().fg(ui::DIM).bg(bg));
            y += 1;
        }

        if y + ARTEFACT_H <= inner.bottom() {
            let mut x = inner.left();
            draw_book_spine(surface, Rect::new(x, y, BOOK_W, ARTEFACT_H), nation);
            x += BOOK_W + ARTEFACT_GAP;
            if x + BANNER_W <= inner.right() {
                draw_banner(surface, Rect::new(x, y, BANNER_W, ARTEFACT_H), nation);
                x += BANNER_W + ARTEFACT_GAP;
            }
            if x + STATUE_W <= inner.right() {
                draw_statue(surface, Rect::new(x, y, STATUE_W, ARTEFACT_H), nation);
            }
        }
    }

    fn draw(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(area);
        let (wall_area, detail_area) = if shape.stacks() {
            let cols = (area.width() / TILE_W)
                .max(1)
                .min(self.nations.len() as u16);
            let rows = (self.nations.len() as u16).div_ceil(cols);
            let wall_h = (1 + rows * TILE_H).min(area.height());
            panel::split_top(area, wall_h)
        } else {
            panel::split_right(area, DETAIL_W.min(area.width()))
        };
        self.draw_wall(surface, wall_area);
        self.draw_detail(surface, detail_area);
    }

    fn status(&self) -> String {
        let name = self
            .nations
            .get(self.selected)
            .map_or("none", |n| n.name.as_str());
        format!(
            "seed {}  nations {}  selected: {name}",
            self.seed,
            self.nations.len()
        )
    }
}

impl Demo for QuarteredArms {
    const NAME: &'static str = "62_quartered_arms";
    const TITLE: &'static str = "62 Quartered Arms";
    const BLURB: &'static str = "Procedural heraldry as a glyph mosaic, reroll-guaranteed distinct, chained onto every artefact.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "select nation"),
            ("Click/tap", "select nation"),
            ("R", "reroll world"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }

        let gesture = self.pointer.take();
        if let Some(pos) = gesture.tap
            && let Some(&Action(i)) = self.hotspots.hit(pos)
        {
            self.selected = i;
        }

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

ascii_tile_demos::demo_main!(QuarteredArms);

#[cfg(test)]
mod tests {
    use super::{
        NUM_NATIONS, SIMILARITY_THRESHOLD, caption_for, fingerprint, generate_nations,
        mean_color_distance,
    };

    /// The reject-and-retry pass's whole point: across a handful of seeds,
    /// no two of the [`NUM_NATIONS`] generated nations should land within
    /// [`SIMILARITY_THRESHOLD`] of each other. `MAX_RETRY_ATTEMPTS` is
    /// generous enough relative to the palette/shape/division combinatorics
    /// that this should hold for essentially every seed; if it starts
    /// failing, the threshold or the retry budget has drifted out of step
    /// with the tincture table.
    #[test]
    fn generated_nations_are_pairwise_distinct() {
        for seed in [1u32, 7, 42, 1000, 99_999] {
            let nations = generate_nations(seed);
            assert_eq!(nations.len(), NUM_NATIONS);
            let fingerprints: Vec<_> = nations.iter().map(fingerprint).collect();
            for i in 0..fingerprints.len() {
                for j in (i + 1)..fingerprints.len() {
                    let distance = mean_color_distance(&fingerprints[i], &fingerprints[j]);
                    assert!(
                        distance >= SIMILARITY_THRESHOLD,
                        "seed {seed}: nations {i} ({}) and {j} ({}) are too similar: {distance}",
                        nations[i].name,
                        nations[j].name
                    );
                }
            }
        }
    }

    /// Every generated name must actually be non-empty and distinct within
    /// its own nation set, since the wall's caption and the detail panel's
    /// title are the only things distinguishing two nations that happen to
    /// share a shape and a division after enough seeds.
    #[test]
    fn generated_names_are_unique_per_seed() {
        for seed in [1u32, 7, 42] {
            let nations = generate_nations(seed);
            let mut names: Vec<&str> = nations.iter().map(|n| n.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), nations.len(), "seed {seed} repeated a name");
        }
    }

    /// Distinct full names are not enough: the wall prints a truncation, and
    /// two different names can truncate to the same caption. Seed 11 did
    /// exactly that before `generate_nations` started rejecting on caption --
    /// "The Gilded Coast" and a second Gilded nation both printed "The Gild",
    /// and a wall with two identically labelled crests reads as a broken
    /// generator no matter how distinct the crests themselves are.
    #[test]
    fn generated_captions_are_unique_per_seed() {
        for seed in [1u32, 7, 11, 42, 1000, 99_999] {
            let nations = generate_nations(seed);
            let mut captions: Vec<String> = nations.iter().map(|n| caption_for(&n.name)).collect();
            captions.sort_unstable();
            captions.dedup();
            assert_eq!(
                captions.len(),
                nations.len(),
                "seed {seed} printed the same caption twice"
            );
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let a = generate_nations(123);
        let b = generate_nations(123);
        assert_eq!(a.len(), b.len());
        for (na, nb) in a.iter().zip(b.iter()) {
            assert_eq!(na.name, nb.name);
            assert_eq!(na.blazon(), nb.blazon());
        }
    }
}
