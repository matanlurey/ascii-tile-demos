//! 60: Tyrant Age -- Lands of Achra's stat sheet, where a single sentence
//! routinely carries six colors and nobody blinks.
//!
//! Every reference screenshot from Lands of Achra says the same thing in a
//! different panel: color is not decoration here, it is the label. An
//! ability line like `Bladestone Coin: on being damaged, deal (Armor) Slash
//! damage to a random enemy` has an ability name, a trigger clause, a verb,
//! a stat reference, an element, a target phrase, and a charge count, each in
//! its own fixed color, glued into one sentence that still has to read as a
//! sentence. Nothing else in this gallery asks a single line of prose to
//! carry that much simultaneous meaning.
//!
//! Techniques on show:
//!
//! - **A wrapped, multi-span rich-text run** ([`draw_rich`]): the headline
//!   technique. See that function's doc comment for the two existing helpers
//!   this demo does *not* use and why, and for which route it takes instead.
//! - **A horizontal message log** ([`TyrantAge::draw_log`]): one or two rows,
//!   full width, pinned to the bottom, on a band a shade lighter than the
//!   map -- the opposite of the tall sidebar log most roguelikes use, and
//!   the first thing about Achra's screen the brief called out by name.
//! - **A multi-column stat grid** ([`TyrantAge::draw_sheet`]): label dim,
//!   value colored by meaning, `Attack`'s base and bonus in two different
//!   colors so a buffed stat visibly says where the extra came from.
//! - **Dashed magenta selection boxes** ([`draw_dashed_box`]): drawn by
//!   alternating a box-drawing dash with a gap that leaves the tile under it
//!   alone, rather than a solid frame, since a solid rectangle in the same
//!   bright magenta reads as a filled block at this scale.
//! - **An unattended battle**: the targeting cursor advances through the
//!   enemy line on its own, driven by `frame.delta`, and each swap both
//!   relocates the selection box and appends a new colored log line -- see
//!   [`TyrantAge::simulate`] and [`TyrantAge::engage`].
//!
//! ```sh
//! cargo run --example 60_tyrant_age --features crossterm
//! cargo run --example 60_tyrant_age --features software
//! cargo run --example 60_tyrant_age --features gl
//! cargo run --example 60_tyrant_age  # headless, prints a few frames
//! ```

use std::collections::VecDeque;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::text::{Line as RichLine, Span as RichSpan};
use retroglyph_core::{
    Backend, Color, Frame, HAlign, Rect, Style, Surface, Terminal, TextLayout, VAlign,
};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::rgb;

// ── Palette ──────────────────────────────────────────────────────────────

/// Very dark desaturated purple, near-black: the page background every
/// reference screenshot sits on.
const BG: Color = rgb(14, 10, 18);
/// Header/log band: a step lighter than [`BG`] and tinted more purple, so the
/// log reads as "a shelf under the map" rather than more map.
const BAND_BG: Color = rgb(26, 18, 34);
/// Header banner background: the purple band the brief calls out by name.
const HEADER_BG: Color = rgb(36, 20, 46);
/// Bright saturated magenta, reserved for the one thing in this demo that
/// means "this is what you're looking at": the dashed selection box.
const SELECT_MAGENTA: Color = rgb(244, 64, 220);

/// Fixed color for the `Life` value.
const LIFE_COLOR: Color = rgb(120, 206, 120);
/// Fixed color for `Attack`'s base component.
const ATTACK_BASE_COLOR: Color = rgb(226, 100, 92);
/// Fixed color for `Attack`'s bonus component: the same green a heal or a
/// buff uses everywhere else on the sheet, so "this number went up" reads
/// consistently whether it is a stat bonus or a triggered heal.
const ATTACK_BONUS_COLOR: Color = LIFE_COLOR;
/// Fixed color for `Dodge`.
const DODGE_COLOR: Color = rgb(120, 198, 226);
/// Fixed color for `Armor`, reused wherever an ability references `(Armor)`
/// as a damage source, since that parenthetical is naming this exact stat.
const ARMOR_COLOR: Color = rgb(190, 180, 210);

/// The ability-name color: the one warm accent on an otherwise cool sheet,
/// so a name is always the first thing the eye lands on in its own line.
const ABILITY_COLOR: Color = rgb(246, 196, 96);
/// Trigger clauses ("on being damaged,") and charge counts ("2 uses"): both
/// connective, both quiet.
const TRIGGER_COLOR: Color = rgb(120, 112, 140);
/// Plain verbs and prose ("deal", "damage to").
const PROSE_COLOR: Color = rgb(206, 202, 216);
/// Target phrases ("a random enemy"): a pale blue that reads as "who", set
/// apart from the elements, which read as "how".
const TARGET_COLOR: Color = rgb(150, 190, 226);
/// Numbers: the brightest neutral on the sheet, since a number is the one
/// token a player is scanning the whole line to find.
const NUMBER_COLOR: Color = rgb(248, 230, 150);

/// One of Achra's nine damage/affinity keywords. Each keeps one fixed color
/// everywhere it appears: in an ability line, in a resist/weakness list, and
/// (via [`EnemyDef::element`]) as the flavor of a log line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Element {
    Fire,
    Ice,
    Slash,
    Blunt,
    Pierce,
    Astral,
    Psychic,
    Blood,
    Death,
}

impl Element {
    const fn color(self) -> Color {
        match self {
            Self::Fire => rgb(228, 118, 62),
            Self::Ice => rgb(118, 202, 228),
            Self::Slash => rgb(210, 210, 218),
            Self::Blunt => rgb(182, 142, 90),
            Self::Pierce => rgb(228, 208, 96),
            Self::Astral => rgb(178, 122, 228),
            Self::Psychic => rgb(228, 122, 202),
            Self::Blood => rgb(198, 42, 50),
            Self::Death => rgb(122, 162, 98),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Fire => "Fire",
            Self::Ice => "Ice",
            Self::Slash => "Slash",
            Self::Blunt => "Blunt",
            Self::Pierce => "Pierce",
            Self::Astral => "Astral",
            Self::Psychic => "Psychic",
            Self::Blood => "Blood",
            Self::Death => "Death",
        }
    }
}

/// The color role one run of text in an ability or log line plays. A tag
/// rather than a raw `Color`, so the meaning survives being written down: a
/// call site says `Tag::Trigger`, not "grey", and the palette can move
/// without touching a single ability definition.
#[derive(Clone, Copy)]
enum Tag {
    /// The ability's own name, opening the line.
    Ability,
    /// A trigger clause: "on being damaged,".
    Trigger,
    /// A bare verb: "deal", "heal", "gain".
    Verb,
    /// A target phrase: "a random enemy".
    Target,
    /// A bare number.
    Number,
    /// A charge count, closing the line: "2 uses".
    Charges,
    /// Connective prose with no other role.
    Prose,
    /// A parenthetical stat reference, colored as the stat it names.
    Stat,
    /// An element keyword, colored as that element.
    Elem(Element),
}

impl Tag {
    const fn color(self) -> Color {
        match self {
            Self::Ability => ABILITY_COLOR,
            Self::Trigger | Self::Charges => TRIGGER_COLOR,
            Self::Verb | Self::Prose => PROSE_COLOR,
            Self::Target => TARGET_COLOR,
            Self::Number => NUMBER_COLOR,
            Self::Stat => ARMOR_COLOR,
            Self::Elem(element) => element.color(),
        }
    }
}

/// A tagged run of text, the unit an ability or log line is built from.
type Run = (&'static str, Tag);

/// One triggered ability, as a list of tagged runs. See [`draw_rich`] for how
/// this becomes wrapped, colored text.
type Ability = &'static [Run];

/// The player's own triggered abilities, straight out of the brief's
/// reference lines with Achra's exact wording kept intact.
const WOHL: Ability = &[
    ("Wohl: ", Tag::Ability),
    ("on battle-turn, ", Tag::Trigger),
    ("heal ", Tag::Verb),
    ("a random earth-born ally ", Tag::Target),
    ("for ", Tag::Prose),
    ("50", Tag::Number),
    ("   10 uses", Tag::Charges),
];

/// See [`WOHL`]. Six colors in one sentence: ability name, trigger, number,
/// verb, stat reference, element, prose, target, and charges -- the line the
/// module doc singles out as the reason this demo exists.
const BLADESTONE_COIN: Ability = &[
    ("Bladestone Coin: ", Tag::Ability),
    ("on being damaged, ", Tag::Trigger),
    ("repeat x5: ", Tag::Number),
    ("deal ", Tag::Verb),
    ("(Armor) ", Tag::Stat),
    ("Slash ", Tag::Elem(Element::Slash)),
    ("damage ", Tag::Prose),
    ("to a random enemy", Tag::Target),
    ("   2 uses", Tag::Charges),
];

/// A third ability, added so the sheet has enough color variety (three
/// elements across three lines) to make the wrap-preserves-color claim land
/// on more than one example.
const EMBER_WARD: Ability = &[
    ("Ember Ward: ", Tag::Ability),
    ("on taking ", Tag::Trigger),
    ("Fire ", Tag::Elem(Element::Fire)),
    ("damage, ", Tag::Trigger),
    ("gain ", Tag::Verb),
    ("(Armor) ", Tag::Stat),
    ("Astral ", Tag::Elem(Element::Astral)),
    ("shield ", Tag::Target),
    ("for ", Tag::Prose),
    ("3 turns", Tag::Number),
    ("   4 uses", Tag::Charges),
];

/// The abilities drawn in the stat sheet, in display order.
const PLAYER_ABILITIES: [Ability; 3] = [WOHL, BLADESTONE_COIN, EMBER_WARD];

/// The player's resistances, drawn as a run of element keywords in their own
/// colors -- see [`Element::color`].
const PLAYER_RESISTS: [Element; 2] = [Element::Ice, Element::Astral];
/// See [`PLAYER_RESISTS`].
const PLAYER_WEAKNESSES: [Element; 2] = [Element::Psychic, Element::Death];

/// The player's own name, used both on the sheet and in log lines.
const PLAYER_NAME: &str = "Wohl";
/// The player's glyph on the battlefield map. Plain ASCII `@`, the
/// traditional roguelike player marker.
const PLAYER_GLYPH: char = '@';

// ── Battlefield ──────────────────────────────────────────────────────────

/// Battlefield width, in map cells.
const BATTLE_COLS: u16 = 9;
/// Battlefield height, in map cells.
const BATTLE_ROWS: u16 = 5;
/// The player's fixed cell on the battlefield.
const PLAYER_CELL: (u16, u16) = (1, 2);

/// One enemy on the battlefield: a name, an ASCII glyph, a fixed cell, and
/// the element it is fought over in the log. Nine entries, one per
/// [`Element`], so a full rotation through the line touches every color in
/// the palette exactly once.
struct EnemyDef {
    name: &'static str,
    glyph: char,
    cell: (u16, u16),
    element: Element,
}

/// The enemy line, positioned across the battlefield away from
/// [`PLAYER_CELL`]. Cell choices are hand-placed rather than generated, both
/// because nine is few enough to place by eye and because a generated
/// layout would have to prove it never lands two enemies on one cell, which
/// a fixed list proves once by inspection instead.
const ENEMIES: [EnemyDef; 9] = [
    EnemyDef {
        name: "Cinder Whelp",
        glyph: 'c',
        cell: (3, 0),
        element: Element::Fire,
    },
    EnemyDef {
        name: "Salt Revenant",
        glyph: 's',
        cell: (5, 0),
        element: Element::Ice,
    },
    EnemyDef {
        name: "Bramble Stalker",
        glyph: 'b',
        cell: (7, 0),
        element: Element::Slash,
    },
    EnemyDef {
        name: "Marrow Hound",
        glyph: 'm',
        cell: (4, 1),
        element: Element::Blunt,
    },
    EnemyDef {
        name: "Rust Automaton",
        glyph: 'r',
        cell: (6, 1),
        element: Element::Pierce,
    },
    EnemyDef {
        name: "Void Adept",
        glyph: 'v',
        cell: (8, 1),
        element: Element::Astral,
    },
    EnemyDef {
        name: "Screaming Idol",
        glyph: 'i',
        cell: (3, 3),
        element: Element::Psychic,
    },
    EnemyDef {
        name: "Gaunt Wisp",
        glyph: 'g',
        cell: (5, 3),
        element: Element::Blood,
    },
    EnemyDef {
        name: "Lost Hyperborath",
        glyph: 'l',
        cell: (7, 3),
        element: Element::Death,
    },
];

/// Seconds between automatic target swaps. Long enough to read the whole log
/// line before the next one lands, short enough that an unattended screen
/// visibly does something within a few seconds.
const ENGAGE_SECONDS: f32 = 3.4;

/// The age names the header cycles through, deterministically, over a much
/// longer period than [`ENGAGE_SECONDS`] -- the header banner's own small
/// motion, independent of the battle underneath it.
const AGE_NAMES: [&str; 4] = ["Age of Flowers", "Age of Ash", "Age of Bone", "Age of Salt"];
/// Seconds each age name holds before advancing.
const AGE_SECONDS: f32 = 18.0;

// ── Rich text: the headline technique ───────────────────────────────────

/// Draws `runs` into `area`, word-wrapped and colored per [`Tag`], never
/// splitting a color mid-word.
///
/// ## Why this and not what is already in the gallery
///
/// Two helpers already exist and both fall short of what an Achra sentence
/// needs:
///
/// - [`ui::panel::spans`] draws a run of [`ui::panel::Span`]s in their own
///   colors, but it is single-line: it truncates at `width` and returns.
///   `Bladestone Coin: on being damaged, ...` is 68 characters with nine
///   colors in it; truncating it to whatever fits one row is not a smaller
///   version of the sentence, it is a different, wrong one.
/// - [`ui::panel::Log`] stores `(String, Color)`: one color for an entire
///   line. It has no way to say "this word is red and that one is cyan"
///   inside a single line at all, which is the one property this whole demo
///   exists to show.
///
/// [`retroglyph_core::layout::TextLayout`] does what both need and neither
/// has: it word-wraps a [`retroglyph_core::text::Line`] built from several
/// [`retroglyph_core::text::Span`]s, and its wrap pass operates at the
/// *grapheme* level while carrying each grapheme's originating span style
/// forward (see `wrap_line` in `retroglyph_core::layout`). That is what
/// "never splitting a run" actually requires: not that a run must stay on
/// one visual line (a long target phrase is allowed to wrap across two), but
/// that wherever it breaks, both halves keep the color the run was given.
/// A hand-rolled wrapper in this file would have to reimplement exactly that
/// grapheme-to-style bookkeeping to avoid the bug it exists to avoid, so
/// this demo uses the shared one instead. As far as a repo-wide grep shows,
/// no demo in this gallery has reached for `TextLayout` before this one.
///
/// The one cost worth naming: [`retroglyph_core::text::Span::styled`] takes
/// `impl Into<String>`, so building the [`RichLine`] allocates one `String`
/// per run, every frame. At this demo's call volume -- a handful of ability
/// and log lines, redrawn once per frame rather than once per cell -- that
/// is well inside what an immediate-mode UI already spends on `format!` per
/// frame elsewhere in this gallery (see `WarbandSheet::draw_fighter`), so it
/// is not a cost this demo works around.
///
/// Returns the number of rows the wrapped text actually used, so a caller
/// stacking several rich lines can advance past exactly what was drawn
/// rather than reserving a worst-case height for each one.
fn draw_rich(surface: &mut Surface<'_>, area: Rect, runs: &[Run], bg: Color) -> u16 {
    if area.width() == 0 || area.height() == 0 {
        return 0;
    }
    surface.fill_rect(area, ' ', Style::new().bg(bg));
    let spans: Vec<RichSpan> = runs
        .iter()
        .map(|&(text, tag)| RichSpan::styled(text, Style::new().fg(tag.color()).bg(bg)))
        .collect();
    let line = RichLine::from(spans);
    let layout = TextLayout::new(&line).rect(area).h_align(HAlign::Left);
    let used = layout.measure().height.min(area.height());
    layout.render_to_surface(surface);
    used
}

/// [`draw_rich`] for log lines, which are built at runtime (a name and a
/// number are not known until the engagement rotates) rather than declared
/// as `&'static` [`Run`]s. Bottom-anchored, so a message short enough to fit
/// one row sits on the row nearest the map rather than floating at the top
/// of a two-row band.
fn draw_rich_owned(
    surface: &mut Surface<'_>,
    area: Rect,
    parts: &[(String, Color)],
    bg: Color,
) -> u16 {
    if area.width() == 0 || area.height() == 0 {
        return 0;
    }
    surface.fill_rect(area, ' ', Style::new().bg(bg));
    let spans: Vec<RichSpan> = parts
        .iter()
        .map(|(text, color)| RichSpan::styled(text.clone(), Style::new().fg(*color).bg(bg)))
        .collect();
    let line = RichLine::from(spans);
    let layout = TextLayout::new(&line)
        .rect(area)
        .h_align(HAlign::Left)
        .v_align(VAlign::Bottom);
    let used = layout.measure().height.min(area.height());
    layout.render_to_surface(surface);
    used
}

/// Prints `parts` on one row, colored per part, clipped rather than wrapped.
/// For the compact label/value readouts (the stat grid, the resist and
/// weakness lines) that are short by construction and have no business
/// spilling onto a second row; [`draw_rich`]/[`draw_rich_owned`] are for the
/// lines that actually need to wrap.
fn print_runs(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    parts: &[(String, Color)],
    bg: Color,
) -> u16 {
    let spans: Vec<panel::Span<'_>> = parts
        .iter()
        .map(|(t, c)| panel::Span::new(t.as_str(), *c))
        .collect();
    panel::spans(surface, at, width, &spans, bg)
}

/// Draws a dashed rectangle around `rect` in `color`, on top of whatever
/// `bg` the cell already has.
///
/// Dashed rather than solid: at the cell sizes a phone-width battlefield
/// grid uses (as little as 5x3), a solid single-line box and a solid double
/// box are barely distinguishable, but a magenta box that is solid on every
/// edge reads as a filled block rather than a frame. Alternating a
/// box-drawing dash with a gap -- literally skipping the `put` call on the
/// gap positions rather than drawing a blank over them -- keeps the
/// underlying tile visible through the box, the way the reference
/// screenshots' selection outlines do.
fn draw_dashed_box(surface: &mut Surface<'_>, rect: Rect, color: Color, bg: Color) {
    if rect.width() < 3 || rect.height() < 2 {
        return;
    }
    let style = Style::new().fg(color).bg(bg);
    let (left, top) = (rect.left(), rect.top());
    let right = rect.right() - 1;
    let bottom = rect.bottom() - 1;

    surface.put((left, top), '\u{250C}', style);
    surface.put((right, top), '\u{2510}', style);
    if bottom > top {
        surface.put((left, bottom), '\u{2514}', style);
        surface.put((right, bottom), '\u{2518}', style);
    }

    let mut cx = left + 1;
    while cx < right {
        if (cx - left) % 2 == 1 {
            surface.put((cx, top), '\u{2500}', style);
            if bottom > top {
                surface.put((cx, bottom), '\u{2500}', style);
            }
        }
        cx += 1;
    }
    let mut cy = top + 1;
    while cy < bottom {
        if (cy - top) % 2 == 1 {
            surface.put((left, cy), '\u{2502}', style);
            surface.put((right, cy), '\u{2502}', style);
        }
        cy += 1;
    }
}

// ── Message log ──────────────────────────────────────────────────────────

/// Builds the log line for the engagement that just landed on `enemy`, on
/// rotation number `n`. Every third rotation is a heal proc from
/// [`WOHL`] instead of an attack, so the unattended log reads as a real
/// fight rather than one move repeated nine times.
fn engagement_line(enemy: &EnemyDef, n: u32) -> Vec<(String, Color)> {
    if n % 3 == 2 {
        return vec![
            (PLAYER_NAME.to_string(), ABILITY_COLOR),
            (
                " heals a random earth-born ally for ".to_string(),
                TRIGGER_COLOR,
            ),
            ("50".to_string(), NUMBER_COLOR),
            (".".to_string(), TRIGGER_COLOR),
        ];
    }
    // Deterministic flavor damage, bounded and varied without any RNG: a
    // pure function of the rotation count, so replaying the same tick
    // sequence always produces the same log, which the determinism test
    // requires.
    let dealt = 34 + (n * 13) % 41;
    vec![
        (PLAYER_NAME.to_string(), ABILITY_COLOR),
        (" strikes ".to_string(), PROSE_COLOR),
        (enemy.name.to_string(), TARGET_COLOR),
        (" for ".to_string(), PROSE_COLOR),
        (dealt.to_string(), NUMBER_COLOR),
        (" ".to_string(), PROSE_COLOR),
        (enemy.element.name().to_string(), enemy.element.color()),
        (" damage.".to_string(), PROSE_COLOR),
    ]
}

// ── Input / touch ────────────────────────────────────────────────────────

/// What tapping a battlefield cell means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Target(usize),
}

/// State for the Tyrant Age demo: which enemy is targeted, the engagement
/// and age timers, the message log, and the touch/keyboard plumbing every
/// interface demo in this gallery shares.
pub struct TyrantAge {
    target: usize,
    engage_timer: f32,
    rotation: u32,
    age_index: usize,
    age_timer: f32,
    time: f32,
    log: VecDeque<Vec<(String, Color)>>,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

/// Messages the log keeps; only the newest one or two are ever drawn (the
/// brief's "less-imposing" horizontal log), but a small backlog means a
/// resize to a taller log band has real history to show rather than padding.
const LOG_CAPACITY: usize = 6;

impl Default for TyrantAge {
    fn default() -> Self {
        let mut log = VecDeque::with_capacity(LOG_CAPACITY);
        log.push_back(vec![("The battle begins.".to_string(), ui::DIM)]);
        Self {
            target: 0,
            engage_timer: ENGAGE_SECONDS,
            rotation: 0,
            age_index: 0,
            age_timer: AGE_SECONDS,
            time: 0.0,
            log,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl TyrantAge {
    fn push_log(&mut self, line: Vec<(String, Color)>) {
        self.log.push_back(line);
        while self.log.len() > LOG_CAPACITY {
            self.log.pop_front();
        }
    }

    /// Rotates the target forward and logs the engagement that produces.
    /// Called both by the automatic timer and by a manual Left/Right/tap, so
    /// every path that changes the target goes through the same log entry.
    fn engage(&mut self, delta: i32) {
        let len = ENEMIES.len() as i32;
        self.target = (self.target as i32 + delta).rem_euclid(len) as usize;
        self.rotation += 1;
        let line = engagement_line(&ENEMIES[self.target], self.rotation);
        self.push_log(line);
    }

    fn set_target(&mut self, index: usize) {
        if index < ENEMIES.len() && index != self.target {
            self.target = index;
            self.rotation += 1;
            let line = engagement_line(&ENEMIES[self.target], self.rotation);
            self.push_log(line);
        }
    }

    /// Advances the unattended battle and the header's age clock. Both are
    /// pure functions of accumulated `dt`, never of wall time, so two runs
    /// fed the same sequence of frame deltas produce the same target, the
    /// same log, and the same age name.
    fn simulate(&mut self, dt: f32) {
        self.time += dt;

        self.engage_timer -= dt;
        if self.engage_timer <= 0.0 {
            self.engage_timer += ENGAGE_SECONDS;
            self.engage(1);
        }

        self.age_timer -= dt;
        if self.age_timer <= 0.0 {
            self.age_timer += AGE_SECONDS;
            self.age_index = (self.age_index + 1) % AGE_NAMES.len();
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left | KeyCode::Char('a' | 'A') => {
                self.engage(-1);
                self.engage_timer = ENGAGE_SECONDS;
            }
            KeyCode::Right | KeyCode::Char('d' | 'D' | ' ') | KeyCode::Enter => {
                self.engage(1);
                self.engage_timer = ENGAGE_SECONDS;
            }
            _ => {}
        }
    }

    fn handle_gesture(&mut self, gesture: &Gesture) {
        if let Some(pos) = gesture.tap
            && let Some(&Action::Target(i)) = self.hotspots.hit(pos)
        {
            self.set_target(i);
            self.engage_timer = ENGAGE_SECONDS;
        }
    }

    fn status_line(&self) -> String {
        format!(
            "targeting {}  turn {}",
            ENEMIES[self.target].name, self.rotation
        )
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    /// The purple header band: an ornament-flanked title, then the year/age
    /// line and the turn/remaining line split left and right.
    fn draw_header(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(HEADER_BG));
        if area.width() < 6 {
            return;
        }

        let title = " TYRANT AGE ";
        let title_w = title.chars().count() as u16;
        let tx = area.left() + (area.width().saturating_sub(title_w)) / 2;
        let ornament = Style::new().fg(ABILITY_COLOR).bg(HEADER_BG);
        surface.put((tx.saturating_sub(2), area.top()), '\u{2561}', ornament);
        surface.print((tx, area.top()), title, ornament);
        surface.put((tx + title_w, area.top()), '\u{255E}', ornament);

        if area.height() < 2 {
            return;
        }
        let y = area.top() + 1;
        let left_w = area.width() * 2 / 3;
        let left_runs: [(String, Color); 3] = [
            ("Year 3001, ".to_string(), TRIGGER_COLOR),
            ("Age of ".to_string(), TRIGGER_COLOR),
            (
                AGE_NAMES[self.age_index]
                    .trim_start_matches("Age of ")
                    .to_string(),
                Element::Fire.color(),
            ),
        ];
        print_runs(surface, (area.left() + 1, y), left_w, &left_runs, HEADER_BG);

        let right_text = format!(
            "Turn {}   {} enemies remain...",
            self.rotation,
            ENEMIES.len()
        );
        let right_w = right_text.chars().count() as u16;
        if area.width() > right_w + 2 {
            let rx = area.right() - right_w - 1;
            surface.print((rx, y), &right_text, Style::new().fg(ui::DIM).bg(HEADER_BG));
        }
    }

    /// The horizontal message log: a lighter band than the map, one or two
    /// rows, showing the newest entries with the most recent flush to the
    /// bottom row. This is the layout shape the brief singles out by name --
    /// "a less-imposing horizontal message log" -- as against the tall
    /// right-hand sidebar log most roguelikes default to.
    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        surface.fill_rect(area, ' ', Style::new().bg(BAND_BG));
        let Some(newest) = self.log.back() else {
            return;
        };
        let inner = Rect::new(
            area.left() + 1,
            area.top(),
            area.width().saturating_sub(2),
            area.height(),
        );
        // Bottom-anchored and free to wrap into every row it's given: a
        // short line sits on the row nearest the map, a long one (Achra's
        // battle reports run to a full sentence) spreads upward into the
        // second row rather than being cut off.
        let used = draw_rich_owned(surface, inner, newest, BAND_BG);

        // If the newest message left a row spare, the previous one fades
        // into it -- the same recency convention `ui::panel::Log` uses, so
        // the log still reads as "most urgent at the bottom" even with only
        // two rows of history ever shown.
        if used < inner.height() && self.log.len() > 1 {
            let older = &self.log[self.log.len() - 2];
            let faded: Vec<(String, Color)> = older
                .iter()
                .map(|(text, color)| (text.clone(), tilekit::palette::mix(*color, BAND_BG, 0.5)))
                .collect();
            let top = Rect::new(
                inner.left(),
                inner.top(),
                inner.width(),
                inner.height() - used,
            );
            draw_rich_owned(surface, top, &faded, BAND_BG);
        }
    }

    /// The tactical battlefield: a grid of cells, floor texture underneath,
    /// the player and every enemy drawn on their fixed cell, and a dashed
    /// magenta box around both the player (always "selected") and whichever
    /// enemy is currently targeted.
    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Battlefield").border(Border::Double);
        let inner = panel.draw(surface, area);
        if inner.width() < BATTLE_COLS || inner.height() < BATTLE_ROWS {
            return;
        }

        let cell_w = inner.width() / BATTLE_COLS;
        let cell_h = inner.height() / BATTLE_ROWS;
        let used_w = cell_w * BATTLE_COLS;
        let used_h = cell_h * BATTLE_ROWS;
        let ox = inner.left() + (inner.width() - used_w) / 2;
        let oy = inner.top() + (inner.height() - used_h) / 2;

        for row in 0..BATTLE_ROWS {
            for col in 0..BATTLE_COLS {
                let cell = Rect::new(ox + col * cell_w, oy + row * cell_h, cell_w, cell_h);
                self.draw_battle_cell(surface, cell, inner, col, row);
            }
        }
    }

    /// Draws one battlefield cell: floor texture, then the player's or an
    /// enemy's glyph if one occupies it, then -- for the player (always
    /// "selected") and whichever enemy is targeted -- the dashed box.
    ///
    /// The glyph sits in the cell's *interior*, one cell in from every edge,
    /// and the box is drawn on the cell's outer edge around it, rather than
    /// the two sharing space: a box drawn directly on top of a 1-row-tall
    /// glyph+label would either erase the glyph (if the box's top edge lands
    /// on the glyph's row) or the label (if its bottom edge does). Reserving
    /// the border ring for the box and nothing else is what keeps both
    /// visible at once, at the cost of the cell needing at least 3x3 cells
    /// before a box is drawn; below that, selection falls back to a tinted
    /// background, which every cell size can show.
    fn draw_battle_cell(
        &mut self,
        surface: &mut Surface<'_>,
        cell: Rect,
        bounds: Rect,
        col: u16,
        row: u16,
    ) {
        // Deterministic rubble scatter, keyed on cell position only (never
        // on `self.time`), so the floor texture never changes frame to
        // frame -- required for the determinism snapshot, which renders
        // twice and diffs.
        let rubble = hash01(0x7A79_5A41, i32::from(col), i32::from(row)) < 0.12;
        let floor_bg = rgb(20, 15, 24);
        surface.fill_rect(cell, ' ', Style::new().bg(floor_bg));
        if rubble && cell.width() > 1 {
            surface.put(
                (cell.left() + 1, cell.top()),
                '.',
                Style::new().fg(rgb(60, 50, 62)).bg(floor_bg),
            );
        }

        let is_player = (col, row) == PLAYER_CELL;
        let enemy_idx = ENEMIES.iter().position(|e| e.cell == (col, row));
        let occupant = if is_player {
            Some((PLAYER_GLYPH, ABILITY_COLOR))
        } else {
            enemy_idx.map(|idx| (ENEMIES[idx].glyph, ENEMIES[idx].element.color()))
        };
        let Some((glyph, color)) = occupant else {
            return;
        };

        let selected = is_player || enemy_idx == Some(self.target);
        let boxed = selected && cell.width() >= 5 && cell.height() >= 3;
        let gx = if boxed {
            cell.left() + cell.width() / 2
        } else {
            cell.left()
        };
        let gy = if boxed {
            cell.top() + cell.height() / 2
        } else {
            cell.top()
        };
        let bg = if selected && !boxed {
            tilekit::palette::mix(floor_bg, SELECT_MAGENTA, 0.35)
        } else {
            floor_bg
        };
        if !boxed && selected {
            surface.fill_rect(cell, ' ', Style::new().bg(bg));
        }
        surface.put((gx, gy), glyph, Style::new().fg(color).bg(bg));

        if let Some(idx) = enemy_idx {
            self.hotspots
                .push_tappable(cell, bounds, Action::Target(idx));
        }
        if boxed {
            draw_dashed_box(surface, cell, SELECT_MAGENTA, floor_bg);
        }
    }

    /// The stat sheet: the multi-column label/value grid, the resist and
    /// weakness runs, and the triggered-ability list, which is where
    /// [`draw_rich`]'s wrapping earns its keep -- a sidebar panel is rarely
    /// wide enough to hold `Bladestone Coin: on being damaged, ...` on one
    /// line.
    fn draw_sheet(surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Character").badge(PLAYER_NAME);
        let inner = panel.draw(surface, area);
        if inner.width() < 4 || inner.height() == 0 {
            return;
        }

        let mut y = inner.top();
        let bottom = inner.bottom();

        if y < bottom {
            let cols = panel::columns(Rect::new(inner.left(), y, inner.width(), 1), 2, 2);
            let life_runs = [
                ("Life ".to_string(), ui::DIM),
                ("720".to_string(), LIFE_COLOR),
                ("/720".to_string(), TRIGGER_COLOR),
            ];
            print_runs(
                surface,
                (cols[0].left(), y),
                cols[0].width(),
                &life_runs,
                panel::PANEL_BG,
            );
            let attack_runs = [
                ("Attack ".to_string(), ui::DIM),
                ("548".to_string(), ATTACK_BASE_COLOR),
                ("+125".to_string(), ATTACK_BONUS_COLOR),
            ];
            print_runs(
                surface,
                (cols[1].left(), y),
                cols[1].width(),
                &attack_runs,
                panel::PANEL_BG,
            );
            y += 1;
        }
        if y < bottom {
            let cols = panel::columns(Rect::new(inner.left(), y, inner.width(), 1), 2, 2);
            let dodge_runs = [
                ("Dodge ".to_string(), ui::DIM),
                ("0%".to_string(), DODGE_COLOR),
            ];
            print_runs(
                surface,
                (cols[0].left(), y),
                cols[0].width(),
                &dodge_runs,
                panel::PANEL_BG,
            );
            let armor_runs = [
                ("Armor ".to_string(), ui::DIM),
                ("10".to_string(), ARMOR_COLOR),
            ];
            print_runs(
                surface,
                (cols[1].left(), y),
                cols[1].width(),
                &armor_runs,
                panel::PANEL_BG,
            );
            y += 2;
        }

        y = Self::draw_element_line(surface, inner, y, bottom, "Resists  ", &PLAYER_RESISTS);
        y = Self::draw_element_line(surface, inner, y, bottom, "Weakness ", &PLAYER_WEAKNESSES);
        if y < bottom {
            y += 1;
        }

        if y < bottom {
            let title = " Abilities ";
            let dash = Style::new().fg(ui::DIM).bg(panel::PANEL_BG);
            surface.put((inner.left(), y), '\u{2561}', dash);
            surface.print(
                (inner.left() + 1, y),
                title,
                Style::new().fg(ABILITY_COLOR).bg(panel::PANEL_BG),
            );
            let used = title.chars().count() as u16 + 1;
            if inner.width() > used + 1 {
                for x in (inner.left() + used + 1)..inner.right() {
                    surface.put((x, y), '\u{2500}', dash);
                }
            }
            y += 1;
        }

        for ability in &PLAYER_ABILITIES {
            if y >= bottom {
                break;
            }
            let room = Rect::new(inner.left(), y, inner.width(), bottom - y);
            let used = draw_rich(surface, room, ability, panel::PANEL_BG);
            y += used.max(1) + 1;
        }
    }

    fn draw_element_line(
        surface: &mut Surface<'_>,
        inner: Rect,
        y: u16,
        bottom: u16,
        label: &str,
        elements: &[Element],
    ) -> u16 {
        if y >= bottom {
            return y;
        }
        let mut runs: Vec<(String, Color)> = vec![(label.to_string(), ui::DIM)];
        for (i, element) in elements.iter().enumerate() {
            if i > 0 {
                runs.push((", ".to_string(), TRIGGER_COLOR));
            }
            runs.push((element.name().to_string(), element.color()));
        }
        print_runs(
            surface,
            (inner.left(), y),
            inner.width(),
            &runs,
            panel::PANEL_BG,
        );
        y + 1
    }

    // ── Layout ───────────────────────────────────────────────────────────

    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(content);

        let header_h = if content.height() >= 6 { 2 } else { 1 };
        let (header, rest) = panel::split_top(content, header_h);
        self.draw_header(surface, header);
        if rest.height() == 0 {
            return;
        }

        let log_h = if rest.height() >= 8 {
            2
        } else {
            u16::from(rest.height() > 2)
        };
        let (mid, log) = panel::split_bottom(rest, log_h);
        self.draw_log(surface, log);
        if mid.height() == 0 {
            return;
        }

        if shape.stacks() {
            let map_h = (mid.height() * 3 / 5)
                .max(BATTLE_ROWS + 2)
                .min(mid.height());
            let (map_area, sheet_area) = panel::split_top(mid, map_h);
            self.draw_map(surface, map_area);
            Self::draw_sheet(surface, sheet_area);
        } else {
            let sidebar_w = (mid.width() * 7 / 20).clamp(20, 44).min(mid.width());
            let (map_area, sheet_area) = panel::split_right(mid, sidebar_w);
            self.draw_map(surface, map_area);
            Self::draw_sheet(surface, sheet_area);
        }
    }
}

impl Demo for TyrantAge {
    const NAME: &'static str = "60_tyrant_age";
    const TITLE: &'static str = "60 Tyrant Age";
    const BLURB: &'static str =
        "Achra's stat sheet: wrapped multi-span rich text, one color per idea.";
    const GRID: (u16, u16) = (140, 44);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[("Left/Right", "cycle target"), ("Enter/tap", "engage")]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
            self.pointer.feed(&event);
        }
        let gesture = self.pointer.take();
        self.handle_gesture(&gesture);

        self.simulate(dt);

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(BG));
        self.layout_and_draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let status_text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &status_text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(TyrantAge);

#[cfg(test)]
mod tests {
    use super::{ENEMIES, Element};

    #[test]
    fn every_enemy_cell_is_unique_and_avoids_the_player() {
        let mut cells: Vec<(u16, u16)> = ENEMIES.iter().map(|e| e.cell).collect();
        cells.sort_unstable();
        let mut unique = cells.clone();
        unique.dedup();
        assert_eq!(
            cells.len(),
            unique.len(),
            "two enemies must not share a cell"
        );
        assert!(
            !cells.contains(&super::PLAYER_CELL),
            "no enemy may sit on the player's cell"
        );
    }

    #[test]
    fn every_element_is_represented_exactly_once_on_the_battlefield() {
        let all = [
            Element::Fire,
            Element::Ice,
            Element::Slash,
            Element::Blunt,
            Element::Pierce,
            Element::Astral,
            Element::Psychic,
            Element::Blood,
            Element::Death,
        ];
        for element in all {
            let count = ENEMIES.iter().filter(|e| e.element == element).count();
            assert_eq!(
                count,
                1,
                "{} should appear on exactly one enemy",
                element.name()
            );
        }
    }
}
