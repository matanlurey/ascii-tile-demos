//! Asserts that every glyph the gallery draws actually renders, in the color
//! it was asked to render in.
//!
//! Both of those can fail silently, and both did.
//!
//! `retroglyph`'s bitmap font is CP437 and its lookup is CP437 by
//! construction: the pixel backends resolve every glyph through
//! `BitmapFont::char_to_index`, which routes through `unicode_to_cp437` and
//! substitutes a solid block (index 219) for anything it does not know. So a
//! character outside CP437 does not fail to draw, it draws as a filled
//! rectangle -- and in a map of dense terrain glyphs that reads as "some cells
//! are very bright" rather than as a bug. Five of `tilekit::glyphs::marker`'s
//! constants were in that state, including the one `Site::Capital` uses, so
//! every capital, fortress, mine, ruin, and port in the gallery was a block.
//!
//! The second failure is subtler. A tileset *can* supply glyphs CP437 lacks,
//! which is what `assets/blocks.png` does for the quadrant, sextant, and
//! braille canvases. But a sprite is composited from its own pixels and is not
//! modulated by the cell's foreground (retroglyph#537), so a sheet drawn as a
//! white mask renders white at every `fg` it is ever given. The sub-cell
//! canvases were therefore monochrome on both pixel backends while looking
//! perfectly correct in a terminal, where the same characters are real font
//! glyphs.
//!
//! Hence two assertions per glyph, not one: it must differ from the fallback
//! block, and it must change when `fg` changes. See
//! <https://github.com/crates-lurey-io/retroglyph/issues/539>.

#![cfg(all(feature = "software", not(target_arch = "wasm32")))]

use ascii_tile_demos::block_tileset;
use retroglyph_core::{Color, Style, Terminal};
use retroglyph_software::SoftwareBackendBuilder;
use tilekit::glyphs;

/// Renders `ch` alone on a 1x1 grid with the given foreground and returns the
/// cell's pixels.
///
/// A fresh backend per call rather than one grid of many cells: the point is
/// to compare one glyph's pixels against another's, and sharing a grid would
/// mean chasing cell offsets for no benefit at this size.
fn render(ch: char, fg: Color) -> Vec<u32> {
    let mut term = Terminal::new(
        SoftwareBackendBuilder::new()
            .grid_size(1, 1)
            .scale(1)
            .tileset(block_tileset())
            .build()
            .expect("software backend must build")
            .run_headless()
            .expect("headless renderer must build"),
    );
    term.surface().put(
        (0u16, 0u16),
        ch,
        Style::new().fg(fg).bg(Color::Rgb { r: 0, g: 0, b: 0 }),
    );
    term.present().expect("headless present cannot fail");
    term.backend().pixels().to_vec()
}

const RED: Color = Color::Rgb { r: 255, g: 0, b: 0 };
const GREEN: Color = Color::Rgb { r: 0, g: 255, b: 0 };

/// The pixels of a character the font has no mapping for, i.e. what the
/// solid-block fallback looks like.
///
/// `U+FFFF` is a noncharacter, so nothing can ever legitimately map it.
fn fallback() -> Vec<u32> {
    render('\u{FFFF}', RED)
}

/// Every glyph constant the gallery draws through `tilekit::glyphs`, as
/// `(name, char)` so a failure names the constant rather than a codepoint.
fn glyph_constants() -> Vec<(&'static str, char)> {
    let mut all = vec![
        ("marker::CAPITAL", glyphs::marker::CAPITAL),
        ("marker::CITY", glyphs::marker::CITY),
        ("marker::TOWN", glyphs::marker::TOWN),
        ("marker::FORT", glyphs::marker::FORT),
        ("marker::RUIN", glyphs::marker::RUIN),
        ("marker::MINE", glyphs::marker::MINE),
        ("marker::SHRINE", glyphs::marker::SHRINE),
        ("marker::PORT", glyphs::marker::PORT),
        ("marker::UNIT", glyphs::marker::UNIT),
        ("marker::SCOUT", glyphs::marker::SCOUT),
        ("terrain::WATER", glyphs::terrain::WATER),
        ("terrain::WAVE", glyphs::terrain::WAVE),
        ("terrain::GRASS", glyphs::terrain::GRASS),
        ("terrain::FOREST", glyphs::terrain::FOREST),
        ("terrain::CONIFER", glyphs::terrain::CONIFER),
        ("terrain::JUNGLE", glyphs::terrain::JUNGLE),
        ("terrain::HILLS", glyphs::terrain::HILLS),
        ("terrain::MOUNTAIN", glyphs::terrain::MOUNTAIN),
        ("terrain::PEAK", glyphs::terrain::PEAK),
        ("terrain::DUNE", glyphs::terrain::DUNE),
        ("terrain::SAND", glyphs::terrain::SAND),
        ("terrain::MARSH", glyphs::terrain::MARSH),
        ("terrain::TUNDRA", glyphs::terrain::TUNDRA),
        ("terrain::SNOW", glyphs::terrain::SNOW),
        ("terrain::ASH", glyphs::terrain::ASH),
    ];
    for (i, &ch) in glyphs::SHADE.iter().enumerate() {
        all.push((Box::leak(format!("SHADE[{i}]").into_boxed_str()), ch));
    }
    for (i, &ch) in glyphs::ASCII_RAMP.iter().enumerate() {
        all.push((Box::leak(format!("ASCII_RAMP[{i}]").into_boxed_str()), ch));
    }
    all
}

#[test]
fn every_glyph_constant_renders_as_itself_not_the_fallback_block() {
    let fallback = fallback();
    let blank = render(' ', RED);
    let mut broken = Vec::new();

    for (name, ch) in glyph_constants() {
        // A space is legitimately blank; nothing else should be.
        if ch == ' ' {
            continue;
        }
        // The fallback *is* the solid block, so the solid block cannot be
        // distinguished from it by pixels and does not need to be: it is CP437
        // 0xDB and always renders.
        if ch == '\u{2588}' {
            continue;
        }
        let pixels = render(ch, RED);
        if pixels == fallback {
            broken.push(format!("{name} ({ch:?}) renders as the fallback block"));
        } else if pixels == blank {
            broken.push(format!("{name} ({ch:?}) renders as nothing at all"));
        }
    }

    assert!(
        broken.is_empty(),
        "{} glyph constant(s) do not render on the software backend. The font is \
         CP437 and unmapped characters become a solid block, so these are drawing \
         as filled rectangles rather than failing visibly:\n  {}",
        broken.len(),
        broken.join("\n  "),
    );
}

#[test]
fn every_glyph_constant_takes_the_foreground_color() {
    let mut monochrome = Vec::new();

    for (name, ch) in glyph_constants() {
        if ch == ' ' {
            continue;
        }
        if render(ch, RED) == render(ch, GREEN) {
            monochrome.push(format!("{name} ({ch:?})"));
        }
    }

    assert!(
        monochrome.is_empty(),
        "{} glyph constant(s) render identically at two different foreground \
         colors, so they are being drawn from a tileset rather than the font \
         (retroglyph#537: sprites ignore fg). A map glyph that cannot be \
         colored cannot carry biome or faction information:\n  {}",
        monochrome.len(),
        monochrome.join("\n  "),
    );
}

/// Strips line comments from Rust source, leaving code.
///
/// Crude on purpose. The only thing this has to get right is not reporting
/// glyphs that appear in prose: several modules discuss the characters that
/// *cannot* be used, and a scanner that flagged its own documentation would be
/// useless. Over-stripping (a `//` inside a string literal) can only make the
/// scan less strict, never wrong, and no demo currently has one.
fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every demo source in the gallery, as `(name, contents)`.
fn demo_sources() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the examples directory must exist") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            out.push((
                name,
                std::fs::read_to_string(&path).expect("readable source"),
            ));
        }
    }
    out.sort();
    assert!(!out.is_empty(), "found no demo sources to scan");
    out
}

/// Scans every demo's *code* for characters that cannot be drawn.
///
/// The constant-by-constant tests above only cover `tilekit::glyphs`, which is
/// where a shared glyph lives. Most glyphs in this gallery are not shared: they
/// are literals written inline in one demo, and two separate demos were caught
/// in review picking `▸` (U+25B8) as a selection marker, which is outside CP437
/// and renders as a solid white block. Nothing in the type system distinguishes
/// a character that draws from one that does not, so the only way to keep this
/// from recurring is to read the source.
///
/// Deliberately scans text rather than requiring glyphs be declared somewhere
/// central. A rule that depends on authors remembering to register their glyphs
/// fails exactly when a new demo is written in a hurry, which is when it is
/// most needed.
#[test]
fn no_demo_uses_a_glyph_that_cannot_be_drawn() {
    let fallback = fallback();
    let mut broken: Vec<String> = Vec::new();

    for (name, source) in demo_sources() {
        let code = strip_comments(&source);
        let mut seen = std::collections::BTreeSet::new();
        for ch in code.chars() {
            // ASCII always draws. Everything else has to prove itself.
            if ch.is_ascii() || !seen.insert(ch) {
                continue;
            }
            if render(ch, RED) == fallback {
                broken.push(format!("{name}: {ch:?} (U+{:04X})", ch as u32));
            }
        }
    }

    assert!(
        broken.is_empty(),
        "{} glyph literal(s) render as the fallback block rather than as \
         themselves. CP437 is the whole repertoire the pixel backends can \
         draw; pick a character from it, or accept a filled rectangle:\n  {}",
        broken.len(),
        broken.join("\n  "),
    );
}

/// The half blocks are the widest sub-cell vocabulary that is *both* CP437 and
/// therefore colorable, which is why the gallery's bars and canvases are built
/// on them rather than on the eighths.
///
/// Pinning it here so the tradeoff is visible in a test rather than only in
/// prose: if a future retroglyph teaches the backends to resolve a custom
/// charset (retroglyph#539), the eighths become available and this test is the
/// place that should start failing.
#[test]
fn half_blocks_are_colorable_but_eighth_blocks_are_not() {
    for ch in ['\u{2588}', '\u{2580}', '\u{2584}', '\u{258C}', '\u{2590}'] {
        assert_ne!(
            render(ch, RED),
            render(ch, GREEN),
            "{ch:?} is a CP437 half block and must take the foreground color"
        );
    }

    let fallback = fallback();
    for ch in ['\u{258F}', '\u{258E}', '\u{258D}', '\u{2581}', '\u{2582}'] {
        assert_eq!(
            render(ch, RED),
            fallback,
            "{ch:?} unexpectedly renders. If retroglyph#539 landed, the gallery \
             can now use eighth-precision bars; update ui::bar and this test."
        );
    }
}
