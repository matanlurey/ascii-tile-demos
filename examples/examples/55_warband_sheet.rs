//! 55: Warband Sheet -- a Mordheim roster form, printed rather than played.
//!
//! Every roster-adjacent demo elsewhere in this gallery is picture-first, with
//! numbers hung off a portrait or a card (43's minion bench, 46's portrait
//! column). This one inverts that: there is almost no illustration, and the
//! subject is a dense printed form -- the 1999 Mordheim warband roster sheet
//! -- so the whole design has to be carried by typography and rule weight, the
//! thing a terminal is actually good at. Nothing here scrolls, glows, or
//! occludes; it just fills in, the way a real character sheet does across a
//! campaign.
//!
//! Techniques on show:
//!
//! - **A ruled profile table** ([`compute_columns`], [`draw_rule`]): the nine
//!   Warhammer stats (M/WS/BS/S/T/W/I/A/Ld) as a real spreadsheet grid with
//!   vertical bars and `┬`/`┼`/`┴` junctions, computed once from the live
//!   panel width so every column lines up exactly down the whole sheet no
//!   matter how many fighters are on it.
//! - **Two rule weights** (`═` for section boundaries, `─` for row
//!   boundaries): the same convention a printed form uses to tell a reader
//!   where a *section* ends versus where a *row* ends, which a uniform grid of
//!   boxes cannot say at all.
//! - **A pip-box experience track** ([`Fighter::flash_t`], [`advance`]):
//!   filled and empty boxes with the advance thresholds printed underneath, so
//!   the reader can see both how far a fighter has come and how far to the
//!   next roll.
//! - **In-place advancement** ([`WarbandSheet::apply_xp_tick`]): experience
//!   accrues on its own (driven by `frame.delta`, one fighter at a time so the
//!   sheet reads as "someone is being trained" rather than "everyone levels at
//!   once"), and crossing a threshold rolls a stat increase or an injury right
//!   onto the cell that changed, flashed briefly so the update is legible
//!   without becoming a jitter.
//! - **A textured but deterministic parchment ground** ([`texture_bg`]): every
//!   cell's background comes from a hash of its own coordinates, not the
//!   clock, so two renders of the same frame match exactly (required for the
//!   determinism test) while the page still reads as aged paper rather than a
//!   flat fill.
//! - **Tap-select-then-tap-roll** ([`WarbandSheet::tap_fighter`]): the touch
//!   idiom this gallery uses for dense boards, applied to table rows instead
//!   of board tiles. A first tap on a row selects the fighter; a second tap on
//!   the same row forces an immediate advancement roll, with full keyboard
//!   parity via Up/Down and Enter.
//!
//! ```sh
//! cargo run --example 55_warband_sheet --features crossterm
//! cargo run --example 55_warband_sheet --features software
//! cargo run --example 55_warband_sheet --features gl
//! cargo run --example 55_warband_sheet  # headless, prints a few frames
//! ```

use std::f32::consts::TAU;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{self, mix, rgb};

/// Dark umber ink, the body-text colour on every row.
const INK: Color = rgb(46, 34, 22);
/// Faded ink for connective labels ("Equipment:", "XP:").
const INK_DIM: Color = rgb(104, 82, 58);
/// Rubric red, the colour period forms used for headings and rulings drawn by
/// a different hand than the body text. Used here for section labels and the
/// warband's vital numbers, so the header reads as authored rather than typed.
const RUBRIC: Color = rgb(126, 34, 24);
/// Frame ink for box-drawing rules: darker than the rubric, lighter than pure
/// black so it still reads as drawn-on-paper rather than printed structure.
const RULE_INK: Color = rgb(80, 58, 40);
/// Wash applied to a selected fighter's rows.
const SELECT_BG: Color = rgb(214, 182, 118);
/// Wash applied to a cell whose stat just advanced, briefly.
const FLASH_BG: Color = rgb(232, 158, 64);

/// Warhammer profile order, matched everywhere a stat is indexed 0..9.
const STAT_LABELS: [&str; 9] = ["M", "WS", "BS", "S", "T", "W", "I", "A", "Ld"];

/// Maximum a human fighter's profile can reach in each stat, in the same
/// order as [`STAT_LABELS`]. Movement is already at its ceiling for every
/// fighter at creation, which is what keeps `M` out of the advance pool
/// without needing a special case: [`advance`] simply never finds it below
/// its own cap.
const STAT_CAPS: [u8; 9] = [4, 6, 6, 4, 4, 3, 6, 4, 9];

/// Experience totals at which an advance roll is made. Eight thresholds
/// because eight pip boxes is what a Full-tier row can show without the track
/// crowding the rest of the line; the real Mordheim chart runs longer for a
/// fighter who survives that many campaigns, which this demo does not model.
const XP_THRESHOLDS: [u32; 8] = [1, 2, 4, 6, 9, 12, 16, 20];

/// Canned injury/event text an advance roll can land on instead of a stat
/// increase. `&'static str` rather than an owned `String` so a rolled note
/// costs nothing to store and cannot be a source of nondeterminism.
const INJURIES: [&str; 5] = [
    "Old Battle Wound",
    "Multiple Injuries -- missed next battle",
    "Robbed -- treasury docked",
    "Captured by a rival warband",
    "Horrible Scars -- feared by lesser foes",
];

/// How long a just-changed stat cell stays flashed, in seconds. Long enough
/// to be seen on an unattended screen refreshing a few times a second, short
/// enough that two rolls in a row do not leave the sheet permanently lit.
const FLASH_DURATION: f32 = 1.6;

/// Seconds between automatic experience ticks. One fighter earns one point at
/// a time, cycling the roster, so the unattended sheet reads as a single
/// training log rather than every row jittering in unison.
const ROLL_INTERVAL: f32 = 2.2;

/// Rows scrolled per key press or wheel notch.
const SCROLL_STEP: u16 = 3;

/// A fighter's role on the warband roster, the second column of every row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FighterType {
    Captain,
    Champion,
    Youngblood,
    Swordsman,
    Marksman,
}

impl FighterType {
    /// The printed label. `Youngblood` is the longest at ten characters and
    /// sets the minimum width of the type column everywhere it is laid out.
    const fn label(self) -> &'static str {
        match self {
            Self::Captain => "Captain",
            Self::Champion => "Champion",
            Self::Youngblood => "Youngblood",
            Self::Swordsman => "Swordsman",
            Self::Marksman => "Marksman",
        }
    }
}

/// One row (plus its sub-rows) of the roster.
struct Fighter {
    name: &'static str,
    kind: FighterType,
    stats: [u8; 9],
    equipment: &'static str,
    special_rules: &'static str,
    xp: u32,
    /// Last rolled note (an injury or event), if any. Persists once set,
    /// matching how a real sheet keeps a scratched-in annotation for the rest
    /// of the campaign rather than clearing it next battle.
    note: Option<&'static str>,
    /// Which stat index most recently advanced, while [`Self::flash_t`] is
    /// still counting down.
    flash_stat: Option<usize>,
    /// Countdown to when the flashed cell returns to its normal colour.
    flash_t: f32,
}

impl Fighter {
    const fn new(
        name: &'static str,
        kind: FighterType,
        stats: [u8; 9],
        equipment: &'static str,
        special_rules: &'static str,
    ) -> Self {
        Self {
            name,
            kind,
            stats,
            equipment,
            special_rules,
            xp: 0,
            note: None,
            flash_stat: None,
            flash_t: 0.0,
        }
    }
}

/// Builds the starting roster. Names are unique (see
/// `every_fighter_name_is_unique`) and equipment/special-rules strings are
/// kept short enough to fit the narrowest supported table width without
/// truncation, since this demo's one hard rule is that nothing gets clipped.
fn seed_fighters() -> Vec<Fighter> {
    vec![
        Fighter::new(
            "Otto Kessler",
            FighterType::Captain,
            [4, 4, 4, 3, 3, 1, 4, 1, 8],
            "Sword, main gauche, light armour",
            "Leader",
        ),
        Fighter::new(
            "Falk Bruckner",
            FighterType::Champion,
            [4, 4, 3, 3, 3, 1, 3, 1, 7],
            "Halberd, buckler",
            "Strike to Injure",
        ),
        Fighter::new(
            "Gerhard Voss",
            FighterType::Champion,
            [4, 4, 3, 3, 3, 1, 3, 1, 7],
            "Sword, sword, helmet",
            "--",
        ),
        Fighter::new(
            "Reinhard Steiner",
            FighterType::Marksman,
            [4, 3, 4, 3, 3, 1, 3, 1, 7],
            "Crossbow, hand weapon",
            "Quick Shot",
        ),
        Fighter::new(
            "Ansgar Hoff",
            FighterType::Marksman,
            [4, 3, 4, 3, 3, 1, 3, 1, 7],
            "Bow, dagger",
            "--",
        ),
        Fighter::new(
            "Dietrich Aue",
            FighterType::Swordsman,
            [4, 3, 3, 3, 3, 1, 3, 1, 7],
            "Sword, shield",
            "--",
        ),
        Fighter::new(
            "Wilhelm Krantz",
            FighterType::Swordsman,
            [4, 3, 3, 3, 3, 1, 3, 1, 7],
            "Spear, light armour",
            "--",
        ),
        Fighter::new(
            "Jorg Fell",
            FighterType::Youngblood,
            [4, 2, 2, 3, 3, 1, 2, 1, 6],
            "Hand weapon",
            "--",
        ),
    ]
}

/// Rolls one advance for `fighter`, deterministic given only its index and
/// current xp (both already-committed state, never wall time), so replaying
/// the same event sequence always produces the same sheet.
///
/// Four in five rolls land on a stat increase, picked from whichever stats
/// have not hit [`STAT_CAPS`] yet; the rest land on a canned injury/event
/// note. A fighter whose whole profile is capped gets a "veteran" note
/// instead of silently doing nothing, so a maxed-out row still visibly
/// resolves its roll.
fn advance(fighter: &mut Fighter, idx: usize) {
    let event_roll = hash01(0x5741_AB03, idx as i32, fighter.xp as i32);
    if event_roll < 0.2 {
        let pick =
            (hash01(0x1234_5678, idx as i32, fighter.xp as i32) * INJURIES.len() as f32) as usize;
        fighter.note = Some(INJURIES[pick.min(INJURIES.len() - 1)]);
        return;
    }

    let candidates: Vec<usize> = (0..9)
        .filter(|&i| fighter.stats[i] < STAT_CAPS[i])
        .collect();
    let Some(&stat_i) = (if candidates.is_empty() {
        None
    } else {
        let pick =
            (hash01(0x0BAD_F00D, idx as i32, fighter.xp as i32) * candidates.len() as f32) as usize;
        candidates.get(pick.min(candidates.len() - 1))
    }) else {
        fighter.note = Some("Grizzled Veteran -- profile maxed");
        return;
    };

    fighter.stats[stat_i] += 1;
    fighter.flash_stat = Some(stat_i);
    fighter.flash_t = FLASH_DURATION;
}

/// A deterministic parchment tone for one cell, so the base fill and every
/// glyph drawn over it agree on the same background with no separate
/// texture pass to keep in sync. Biased toward the pale end of
/// [`palette::PARCHMENT`] (a form is paper, not ink) with a rare darker fleck
/// standing in for foxing.
fn texture_bg(x: u16, y: u16) -> Color {
    let n = hash01(0x50A1_7000, i32::from(x), i32::from(y));
    let base = palette::PARCHMENT.sample(0.030f32.mul_add(n, 0.55));
    let spot = hash01(0x50A1_7001, i32::from(x), i32::from(y));
    if spot < 0.015 {
        mix(base, rgb(120, 96, 66), 0.35)
    } else {
        base
    }
}

/// Rough page dimensions, in cells, used only to place the drifting
/// highlight in [`candle_warmth`] at a scale that reads sensibly against
/// this demo's fixed [`WarbandSheet::GRID`]. Not measured from the live
/// content rect: the drift only needs to wander across roughly the whole
/// page, and a few cells of slop at the margin costs nothing.
const DRIFT_W: f32 = 150.0;
const DRIFT_H: f32 = 40.0;

/// A slow, position-dependent warmth standing in for a candle drifting
/// across the page while the sheet sits unattended -- the brief's "very slow
/// lighting drift". Two out-of-phase periods with no small common factor
/// (23s and 31s) keep the hotspot's path from ever visibly repeating. It is
/// a pure function of `time` (itself just accumulated `frame.delta`), so
/// replaying the same tick sequence reproduces the same page exactly, which
/// is what the determinism test requires.
fn candle_warmth(x: u16, y: u16, time: f32) -> f32 {
    let cx = 0.4f32.mul_add((time * (TAU / 23.0)).sin(), 0.5);
    let cy = 0.4f32.mul_add((time.mul_add(TAU / 31.0, 1.7)).sin(), 0.5);
    let dx = f32::from(x) / DRIFT_W - cx;
    let dy = f32::from(y) / DRIFT_H - cy;
    let dist2 = dx.mul_add(dx, dy * dy);
    (0.10 - dist2 * 0.5).max(0.0)
}

/// [`texture_bg`] plus the candle drift from [`candle_warmth`]. Every
/// on-screen background goes through this rather than `texture_bg` directly,
/// so the whole page breathes together instead of the ruled table looking
/// pasted onto a static margin.
fn lit_bg(x: u16, y: u16, time: f32) -> Color {
    let base = texture_bg(x, y);
    let warmth = candle_warmth(x, y, time);
    if warmth > 0.0 {
        mix(base, rgb(214, 158, 74), warmth)
    } else {
        base
    }
}

/// The selected fighter's wash, breathing slowly rather than sitting at a
/// flat intensity -- the brief's "slow highlight breath". 5.3s is
/// deliberately not a round number so the breath never lines up with
/// [`ROLL_INTERVAL`] and reads as its own rhythm rather than a side effect of
/// the training cursor.
fn select_bg(under: Color, time: f32) -> Color {
    let breath = 0.5f32.mul_add((time * (TAU / 5.3)).sin(), 0.5);
    mix(under, SELECT_BG, 0.85f32.mul_add(breath, 0.15))
}

/// Column geometry for the ruled profile table, recomputed every frame from
/// the live content width. Growing the name column (rather than the fixed
/// type/stat columns) is what lets the sheet use extra desktop width without
/// distorting the part of the table whose widths are meaningful (a stat cell
/// wider than its values just looks unfinished).
#[derive(Clone, Copy)]
struct Cols {
    table_x: u16,
    name_w: u16,
    type_w: u16,
    stat_w: u16,
}

impl Cols {
    const NAME_MIN: u16 = 14;
    const NAME_MAX: u16 = 26;
    const TYPE_W: u16 = 10; // fits "Youngblood" exactly.

    fn compute(content_w: u16) -> Self {
        let stat_w: u16 = if content_w >= 170 {
            4
        } else if content_w >= 110 {
            3
        } else {
            2
        };
        let min_total = 12 + Self::NAME_MIN + Self::TYPE_W + 9 * stat_w;
        let extra = content_w.saturating_sub(min_total);
        let name_w =
            (Self::NAME_MIN + extra.min(Self::NAME_MAX - Self::NAME_MIN)).min(Self::NAME_MAX);
        let total = 12 + name_w + Self::TYPE_W + 9 * stat_w;
        let margin = content_w.saturating_sub(total) / 2;
        Self {
            table_x: margin,
            name_w,
            type_w: Self::TYPE_W,
            stat_w,
        }
    }

    /// Total table width including both outer vertical bars.
    const fn table_w(self) -> u16 {
        12 + self.name_w + self.type_w + 9 * self.stat_w
    }

    /// The x position of each of the table's twelve vertical bars: one before
    /// the name column, one between every pair of columns, one after `Ld`.
    fn boundaries(self) -> [u16; 12] {
        let mut out = [0u16; 12];
        let mut x = self.table_x;
        out[0] = x;
        x += 1 + self.name_w;
        out[1] = x;
        x += 1 + self.type_w;
        out[2] = x;
        for i in 0..9 {
            x += 1 + self.stat_w;
            out[3 + i] = x;
        }
        out
    }

    /// The `(x, width)` of column `i`: 0 is Name, 1 is Type, 2..11 are the
    /// nine stats in [`STAT_LABELS`] order.
    fn column(self, i: usize) -> (u16, u16) {
        let b = self.boundaries();
        (b[i] + 1, b[i + 1] - b[i] - 1)
    }
}

/// Draws one row of `═`, the heavy weight used only at section boundaries
/// (around the header block, and closing the roster).
fn heavy_rule(surface: &mut Surface<'_>, x0: u16, y: u16, w: u16, time: f32) {
    for i in 0..w {
        let x = x0 + i;
        surface.put(
            (x, y),
            '\u{2550}',
            Style::new().fg(RULE_INK).bg(lit_bg(x, y, time)),
        );
    }
}

/// Draws a light `─` rule with `┬`/`┼`/`┴`/`├`/`┤` junctions at `boundaries`,
/// used between table rows. `top`/`bottom` select which junction glyph a
/// boundary gets, since the same rule can sit above the first data row
/// (`┬` under the header labels) or between two data rows (`┼`) or under the
/// very last row (`┴`).
enum JunctionRow {
    Top,
    Mid,
    Bottom,
}

fn light_rule(
    surface: &mut Surface<'_>,
    y: u16,
    boundaries: &[u16; 12],
    which: &JunctionRow,
    time: f32,
) {
    let x0 = boundaries[0];
    let x1 = boundaries[11];
    for x in x0..=x1 {
        let is_boundary = boundaries.contains(&x);
        let ch = if x == x0 {
            match which {
                JunctionRow::Top => '\u{252C}',
                JunctionRow::Mid => '\u{251C}',
                JunctionRow::Bottom => '\u{2514}',
            }
        } else if x == x1 {
            match which {
                JunctionRow::Top => '\u{252C}',
                JunctionRow::Mid => '\u{2524}',
                JunctionRow::Bottom => '\u{2518}',
            }
        } else if is_boundary {
            match which {
                JunctionRow::Top => '\u{252C}',
                JunctionRow::Mid => '\u{253C}',
                JunctionRow::Bottom => '\u{2534}',
            }
        } else {
            '\u{2500}'
        };
        surface.put((x, y), ch, Style::new().fg(RULE_INK).bg(lit_bg(x, y, time)));
    }
}

/// Prints `text` left-aligned into a column, padded with texture-backed
/// spaces so the column's own background continues under short values.
/// `text` must already fit `width`; every caller here sizes its strings
/// against a column no narrower than the string requires; a debug assertion
/// catches a caller that stops being true rather than silently clipping.
fn put_col(
    surface: &mut Surface<'_>,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    bg: Color,
    fg: Color,
) {
    debug_assert!(
        text.chars().count() <= usize::from(width),
        "column too narrow for {text:?}: needs {}, has {width}",
        text.chars().count()
    );
    surface.print((x, y), text, Style::new().fg(fg).bg(bg));
    let used = text.chars().count() as u16;
    for i in used..width {
        surface.put((x + i, y), ' ', Style::new().fg(fg).bg(bg));
    }
}

/// Prints `text` right-aligned into a column of `width`, same contract as
/// [`put_col`].
fn put_col_right(
    surface: &mut Surface<'_>,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    bg: Color,
    fg: Color,
) {
    let used = text.chars().count() as u16;
    debug_assert!(used <= width, "column too narrow for {text:?}");
    for i in 0..(width - used) {
        surface.put((x + i, y), ' ', Style::new().fg(fg).bg(bg));
    }
    surface.print((x + width - used, y), text, Style::new().fg(fg).bg(bg));
}

/// Prints `left` at `table_x` and `right` flush with the table's right edge,
/// unless the table is narrow enough that flush-right would overlap `left`,
/// in which case `right` follows immediately after `left` with a two-column
/// gap instead. Every header vital line goes through this rather than a
/// fixed centre/right split, which is what the three-field version of this
/// row used to get wrong: a hand-centred label collides with its neighbour
/// the moment the table is narrower than the sum of both strings.
fn draw_two_field(
    surface: &mut Surface<'_>,
    cols: Cols,
    y: u16,
    left: &str,
    right: &str,
    fg: Color,
    time: f32,
) {
    let table_x = cols.table_x;
    let bg = lit_bg(table_x, y, time);
    surface.print((table_x, y), left, Style::new().fg(fg).bg(bg));
    let left_end = table_x + left.chars().count() as u16;
    let right_w = right.chars().count() as u16;
    let flush_x = (table_x + cols.table_w()).saturating_sub(right_w);
    let rx = flush_x.max(left_end + 2);
    surface.print((rx, y), right, Style::new().fg(fg).bg(bg));
}

/// A tap target, one per fighter row.
#[derive(Clone, Copy)]
enum Action {
    Fighter(usize),
}

/// State: the roster, the warband's vitals, the training cursor, and the
/// touch/keyboard plumbing every interface demo shares.
pub struct WarbandSheet {
    fighters: Vec<Fighter>,
    warband_name: &'static str,
    warband_type: &'static str,
    treasury: u32,
    wyrdstone: u32,
    time: f32,
    roll_timer: f32,
    roll_cursor: usize,
    selected: Option<usize>,
    scroll: u16,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for WarbandSheet {
    fn default() -> Self {
        Self {
            fighters: seed_fighters(),
            warband_name: "The Reikland Company",
            warband_type: "Reikland Mercenaries",
            treasury: 87,
            wyrdstone: 3,
            time: 0.0,
            roll_timer: ROLL_INTERVAL,
            roll_cursor: 0,
            selected: Some(0),
            scroll: 0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl WarbandSheet {
    /// Warband rating: a flavour formula (base cost per fighter plus total
    /// earned experience), not the exact Mordheim computation, so the header
    /// has a number that visibly grows as the roster trains without needing
    /// the full points-cost model this demo does not otherwise track.
    fn rating(&self) -> u32 {
        let xp_total: u32 = self.fighters.iter().map(|f| f.xp).sum();
        40 + self.fighters.len() as u32 * 8 + xp_total
    }

    fn move_selection(&mut self, delta: i32) {
        let n = self.fighters.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.selected.map_or(0, |i| i as i32);
        self.selected = Some((cur + delta).rem_euclid(n) as usize);
    }

    /// Applies one experience point to fighter `idx`, and rolls an advance
    /// if that lands exactly on a threshold in [`XP_THRESHOLDS`].
    fn apply_xp_tick(&mut self, idx: usize) {
        let Some(fighter) = self.fighters.get_mut(idx) else {
            return;
        };
        fighter.xp += 1;
        if XP_THRESHOLDS.contains(&fighter.xp) {
            advance(fighter, idx);
        }
    }

    /// First tap on a row selects it; a second tap on the row already
    /// selected forces an immediate roll, which is the tap-select-then-tap-
    /// target idiom this gallery uses so a finger never has to hit a tiny
    /// separate "roll" button.
    fn tap_fighter(&mut self, i: usize) {
        if self.selected == Some(i) {
            self.apply_xp_tick(i);
        } else {
            self.selected = Some(i);
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_selection(1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(i) = self.selected {
                    self.apply_xp_tick(i);
                }
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(SCROLL_STEP),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(SCROLL_STEP),
            _ => {}
        }
    }

    fn handle_gesture(&mut self, g: &Gesture) {
        if let Some(pos) = g.tap
            && let Some(&Action::Fighter(i)) = self.hotspots.hit(pos)
        {
            self.tap_fighter(i);
        }
        if g.scroll != 0 {
            let delta = i32::from(SCROLL_STEP) * -g.scroll;
            self.scroll = (i32::from(self.scroll) + delta).max(0) as u16;
        }
        // Dragging follows the finger (content moves with the touch point),
        // which is why the sign is inverted against the raw row delta: a
        // finger moving up must reveal rows further down the sheet.
        if g.delta.1 != 0 {
            self.scroll = (i32::from(self.scroll) - g.delta.1).max(0) as u16;
        }
    }

    /// Advances the training cursor and fades any flashed stat cells.
    /// Everything here is driven by `dt`, never by a frame count or the
    /// clock, so the demo animates identically regardless of how fast the
    /// backend ticks and reproduces exactly under the determinism test.
    fn simulate(&mut self, dt: f32) {
        for fighter in &mut self.fighters {
            if fighter.flash_t > 0.0 {
                fighter.flash_t -= dt;
                if fighter.flash_t <= 0.0 {
                    fighter.flash_t = 0.0;
                    fighter.flash_stat = None;
                }
            }
        }
        self.roll_timer -= dt;
        if self.roll_timer <= 0.0 && !self.fighters.is_empty() {
            self.roll_timer += ROLL_INTERVAL;
            self.apply_xp_tick(self.roll_cursor);
            self.roll_cursor = (self.roll_cursor + 1) % self.fighters.len();
        }
    }

    /// Total rows the whole sheet needs at `cols`, for clamping scroll.
    const fn total_rows(&self) -> u16 {
        HEADER_ROWS + self.fighters.len() as u16 * FIGHTER_ROWS + 1
    }

    fn clamp_scroll(&mut self, content_h: u16) {
        let max_scroll = self.total_rows().saturating_sub(content_h);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn status_text(&self) -> String {
        let name = self
            .selected
            .and_then(|i| self.fighters.get(i))
            .map_or("none", |f| f.name);
        format!(
            "rating {}  fighters {}  selected: {name}",
            self.rating(),
            self.fighters.len()
        )
    }

    /// Draws the whole sheet (header block plus every fighter row) starting
    /// `self.scroll` rows into its virtual layout. Rows above or below the
    /// visible content rect are skipped for drawing and hotspot registration,
    /// but the layout cursor still advances past them, which is what makes
    /// scrolling work: the geometry is computed once for the full sheet and
    /// only the visible slice is realized.
    fn draw_sheet(&self, surface: &mut Surface<'_>, area: Rect, cols: Cols) {
        let mut cols_shifted = cols;
        cols_shifted.table_x = cols.table_x + area.left();
        let table_x = cols_shifted.table_x;

        // Rows are addressed by a signed offset from the top of `area` minus
        // the scroll, rather than a `u16` cursor, so scrolling past the top
        // of the sheet clamps cleanly instead of wrapping around zero.
        let top = i32::from(area.top());
        let bottom = i32::from(area.bottom());
        let scroll = i32::from(self.scroll);
        let mut visible_y = move |row: i32| -> Option<u16> {
            let y = top - scroll + row;
            (y >= top && y < bottom).then_some(y as u16)
        };
        let mut row: i32 = 0;

        self.draw_header(surface, area, cols_shifted, row, &mut visible_y);
        row += i32::from(HEADER_ROWS);

        let fighter_count = self.fighters.len();
        for i in 0..fighter_count {
            let is_last = i + 1 == fighter_count;
            self.draw_fighter(surface, cols_shifted, i, row, is_last, &mut visible_y);
            row += i32::from(FIGHTER_ROWS);
        }

        if let Some(y) = visible_y(row) {
            heavy_rule(surface, table_x, y, cols_shifted.table_w(), self.time);
        }
    }

    fn draw_header(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        cols: Cols,
        row0: i32,
        visible_y: &mut impl FnMut(i32) -> Option<u16>,
    ) {
        let table_w = cols.table_w();
        if let Some(y) = visible_y(row0) {
            heavy_rule(surface, cols.table_x, y, table_w, self.time);
            // Ornaments sit in the true margin outside the ruled table, not
            // on top of it, and only where there is at least 4 clear columns
            // to hold a 3-wide box plus a gutter: a form with no margin gets
            // no ornaments rather than a clipped one.
            let left_margin = cols.table_x.saturating_sub(area.left());
            if left_margin >= 4 {
                draw_ornament(surface, cols.table_x - 4, y, self.time);
            }
            let right_margin = area.right().saturating_sub(cols.table_x + table_w);
            if right_margin >= 4 {
                draw_ornament(surface, cols.table_x + table_w + 1, y, self.time);
            }
        }
        if let Some(y) = visible_y(row0 + 1) {
            let left = format!("WARBAND: {}", self.warband_name);
            let right = format!("RATING: {}", self.rating());
            draw_two_field(surface, cols, y, &left, &right, RUBRIC, self.time);
        }
        if let Some(y) = visible_y(row0 + 2) {
            // On its own row rather than sharing one with treasury/wyrdstone:
            // three fields on a single line does not fit the narrowest
            // supported table (a portrait phone's minimum column budget is
            // smaller than "TYPE: Reikland Mercenaries" alone plus two more
            // labels), so the type gets the room it needs and the other two
            // vitals share the row below.
            let bg = lit_bg(cols.table_x, y, self.time);
            let left = format!("TYPE: {}", self.warband_type);
            surface.print((cols.table_x, y), &left, Style::new().fg(INK).bg(bg));
        }
        if let Some(y) = visible_y(row0 + 3) {
            let left = format!("TREASURY: {} gc", self.treasury);
            let right = format!("WYRDSTONE: {}", self.wyrdstone);
            draw_two_field(surface, cols, y, &left, &right, INK, self.time);
        }
        if let Some(y) = visible_y(row0 + 4) {
            heavy_rule(surface, cols.table_x, y, table_w, self.time);
        }
        if let Some(y) = visible_y(row0 + 5) {
            Self::draw_column_header(surface, cols, y, self.time);
        }
        if let Some(y) = visible_y(row0 + 6) {
            light_rule(surface, y, &cols.boundaries(), &JunctionRow::Top, self.time);
        }
    }

    /// The column-label row: `Name`/`Type` plus the nine
    /// [`STAT_LABELS`], in rubric ink, right-aligned in the stat columns to
    /// match how the values under them are aligned.
    fn draw_column_header(surface: &mut Surface<'_>, cols: Cols, y: u16, time: f32) {
        for &x in &cols.boundaries() {
            surface.put(
                (x, y),
                '\u{2502}',
                Style::new().fg(RULE_INK).bg(lit_bg(x, y, time)),
            );
        }
        let (nx, nw) = cols.column(0);
        put_col(surface, nx, y, nw, "Name", lit_bg(nx, y, time), RUBRIC);
        let (tx, tw) = cols.column(1);
        put_col(surface, tx, y, tw, "Type", lit_bg(tx, y, time), RUBRIC);
        for (i, label) in STAT_LABELS.iter().enumerate() {
            let (sx, sw) = cols.column(2 + i);
            put_col_right(surface, sx, y, sw, label, lit_bg(sx, y, time), RUBRIC);
        }
    }

    /// Fills `area` with the deterministic parchment texture plus the slow
    /// candle drift from [`candle_warmth`]. Not a `&self` method: it takes
    /// `time` as a plain argument rather than reading `self.time`, so it
    /// stays a pure function of its inputs and two calls at the same `time`
    /// always draw the same ground, which is what the determinism test and
    /// [`texture_bg`]'s own contract both require.
    fn draw_texture(surface: &mut Surface<'_>, area: Rect, time: f32) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                surface.put((x, y), ' ', Style::new().bg(lit_bg(x, y, time)));
            }
        }
    }

    fn draw_fighter(
        &self,
        surface: &mut Surface<'_>,
        cols: Cols,
        idx: usize,
        row0: i32,
        is_last: bool,
        visible_y: &mut impl FnMut(i32) -> Option<u16>,
    ) {
        let fighter = &self.fighters[idx];
        let selected = self.selected == Some(idx);
        let is_training = self.roll_cursor == idx;

        if let Some(y) = visible_y(row0) {
            self.draw_stat_row(surface, cols, fighter, y, selected, is_training);
        }
        if let Some(y) = visible_y(row0 + 1) {
            let ground = lit_bg(cols.table_x, y, self.time);
            let bg = if selected {
                select_bg(ground, self.time)
            } else {
                ground
            };
            let text = format!("  Equipment: {}", fighter.equipment);
            put_col(surface, cols.table_x, y, cols.table_w(), &text, bg, INK_DIM);
        }
        if let Some(y) = visible_y(row0 + 2) {
            let ground = lit_bg(cols.table_x, y, self.time);
            let bg = if selected {
                select_bg(ground, self.time)
            } else {
                ground
            };
            let text = format!("  Special Rules: {}", fighter.special_rules);
            put_col(surface, cols.table_x, y, cols.table_w(), &text, bg, INK_DIM);
        }
        if let Some(y) = visible_y(row0 + 3) {
            Self::draw_xp_row(surface, cols, fighter, y, selected, self.time);
        }
        if let Some(y) = visible_y(row0 + 4) {
            let which = if is_last {
                JunctionRow::Bottom
            } else {
                JunctionRow::Mid
            };
            light_rule(surface, y, &cols.boundaries(), &which, self.time);
        }
    }

    fn draw_stat_row(
        &self,
        surface: &mut Surface<'_>,
        cols: Cols,
        fighter: &Fighter,
        y: u16,
        selected: bool,
        is_training: bool,
    ) {
        let b = cols.boundaries();
        for &x in &b {
            let ground = lit_bg(x, y, self.time);
            let bg = if selected {
                select_bg(ground, self.time)
            } else {
                ground
            };
            surface.put((x, y), '\u{2502}', Style::new().fg(RULE_INK).bg(bg));
        }
        let (nx, nw) = cols.column(0);
        let ground = lit_bg(nx, y, self.time);
        let bg = if selected {
            select_bg(ground, self.time)
        } else {
            ground
        };
        // A blinking marker beside the row currently accruing experience --
        // the quill resting on the line it is about to write -- so an
        // unattended viewer can see which fighter the sheet is "about to"
        // update next, not only the update itself once it lands.
        let marker = if is_training && (self.time * 1.6).fract() < 0.5 {
            '\u{2022}'
        } else {
            ' '
        };
        surface.put((nx, y), marker, Style::new().fg(RUBRIC).bg(bg));
        put_col(surface, nx + 1, y, nw - 1, fighter.name, bg, INK);

        let (tx, tw) = cols.column(1);
        let ground = lit_bg(tx, y, self.time);
        let bg = if selected {
            select_bg(ground, self.time)
        } else {
            ground
        };
        put_col(surface, tx, y, tw, fighter.kind.label(), bg, INK_DIM);

        for (i, &value) in fighter.stats.iter().enumerate() {
            let (sx, sw) = cols.column(2 + i);
            let flashed = fighter.flash_t > 0.0 && fighter.flash_stat == Some(i);
            let ground = lit_bg(sx, y, self.time);
            let bg = if flashed {
                FLASH_BG
            } else if selected {
                select_bg(ground, self.time)
            } else {
                ground
            };
            let fg = if flashed { rgb(40, 24, 12) } else { INK };
            put_col_right(surface, sx, y, sw, &value.to_string(), bg, fg);
        }
    }

    /// The XP pip track: one bracketed box per [`XP_THRESHOLDS`] entry, filled
    /// once `fighter.xp` reaches that threshold, plus the running total and the
    /// next threshold still to come. A free function (not a method) because it
    /// reads only its arguments, which is also what keeps `draw_fighter` from
    /// needing `&mut self` just to reach it.
    fn draw_xp_row(
        surface: &mut Surface<'_>,
        cols: Cols,
        fighter: &Fighter,
        y: u16,
        selected: bool,
        time: f32,
    ) {
        let ground = lit_bg(cols.table_x, y, time);
        let bg = if selected {
            select_bg(ground, time)
        } else {
            ground
        };
        // Fill the whole row first so a short line still shows the selected
        // wash (or the parchment texture) to the table's right edge.
        for x in cols.table_x..cols.table_x + cols.table_w() {
            let cell_ground = lit_bg(x, y, time);
            let cell_bg = if selected {
                select_bg(cell_ground, time)
            } else {
                cell_ground
            };
            surface.put((x, y), ' ', Style::new().bg(cell_bg));
        }

        let mut x = cols.table_x + 2;
        surface.print((x, y), "XP:", Style::new().fg(INK_DIM).bg(bg));
        x += 4;
        for &threshold in &XP_THRESHOLDS {
            let filled = fighter.xp >= threshold;
            let cell_style = if filled {
                Style::new().fg(RUBRIC).bg(bg)
            } else {
                Style::new().fg(mix(bg, INK, 0.15)).bg(bg)
            };
            surface.put((x, y), '[', Style::new().fg(INK_DIM).bg(bg));
            surface.put(
                (x + 1, y),
                if filled { '\u{2588}' } else { ' ' },
                cell_style,
            );
            surface.put((x + 2, y), ']', Style::new().fg(INK_DIM).bg(bg));
            x += 3;
        }
        x += 1;
        let count = format!("{} xp", fighter.xp);
        surface.print((x, y), &count, Style::new().fg(INK).bg(bg));
        x += count.chars().count() as u16 + 2;

        let next = XP_THRESHOLDS.iter().find(|&&t| t > fighter.xp);
        if let Some(next) = next {
            let text = format!("(next @ {next})");
            let room = (cols.table_x + cols.table_w()).saturating_sub(x + 2);
            if (text.chars().count() as u16) <= room {
                surface.print((x, y), &text, Style::new().fg(INK_DIM).bg(bg));
                x += text.chars().count() as u16 + 2;
            }
        }

        if let Some(note) = fighter.note {
            let room = (cols.table_x + cols.table_w()).saturating_sub(x + 2);
            let text = format!("-- {note}");
            if (text.chars().count() as u16) <= room {
                surface.print((x, y), &text, Style::new().fg(RUBRIC).bg(bg));
            }
            // A note that would not fit is simply not printed this row; it
            // stays recorded on the fighter and will show once the table
            // widens or the name column shrinks, but it is never truncated
            // mid-word, which would misreport what happened.
        }
    }

    /// Rebuilds hotspots for every visible fighter row, so a tap anywhere in
    /// a fighter's block (its stat row through its light rule) selects or
    /// rolls that fighter. Registered against the *content* area, not the
    /// table, so the tap target on a narrow phone (where the table already
    /// spans the width) is still comfortably above `touch::TAP_H`.
    fn rebuild_hotspots(&mut self, area: Rect) {
        self.hotspots.clear();
        let mut row: i32 = -i32::from(self.scroll) + i32::from(HEADER_ROWS);
        for i in 0..self.fighters.len() {
            let top = i32::from(area.top()) + row;
            let bottom = top + i32::from(FIGHTER_ROWS);
            let clip_top = top.max(i32::from(area.top()));
            let clip_bottom = bottom.min(i32::from(area.bottom()));
            if clip_bottom > clip_top {
                let rect = Rect::new(
                    area.left(),
                    clip_top as u16,
                    area.width(),
                    (clip_bottom - clip_top) as u16,
                );
                self.hotspots.push(rect, Action::Fighter(i));
            }
            row += i32::from(FIGHTER_ROWS);
        }
    }
}

/// Rows the header block occupies: two heavy rules, two data lines, the
/// column-header line, and its own light rule.
const HEADER_ROWS: u16 = 7;
/// Rows one fighter occupies: the ruled stat row, two label sub-rows, the XP
/// track, and the light rule that closes the block.
const FIGHTER_ROWS: u16 = 5;

/// A restrained 3x1 woodcut flourish: a bordered box holding a single suit
/// glyph, drawn once on each side of the header's rating line if there is
/// margin to spare. Two is the ceiling the brief sets for this sheet, and
/// this is deliberately the smallest thing that still reads as an ornament
/// rather than a stray box -- anything busier would compete with the
/// typography that is supposed to carry the page.
fn draw_ornament(surface: &mut Surface<'_>, x: u16, y: u16, time: f32) {
    let bg = lit_bg(x, y, time);
    let style = Style::new().fg(RULE_INK).bg(bg);
    surface.put((x, y), '\u{250C}', style);
    surface.put((x + 1, y), '\u{2666}', style);
    surface.put((x + 2, y), '\u{2510}', style);
}

impl Demo for WarbandSheet {
    const NAME: &'static str = "55_warband_sheet";
    const TITLE: &'static str = "55 Warband Sheet";
    const BLURB: &'static str =
        "Mordheim roster form: ruled stat columns, XP pips, advanced in place.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Up/Down", "select fighter"),
            ("Enter/Space", "roll advance"),
            ("PgUp/PgDn", "scroll sheet"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
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

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();

        Self::draw_texture(&mut surface, content, self.time);
        let cols = Cols::compute(content.width());
        self.clamp_scroll(content.height());
        self.rebuild_hotspots(content);
        self.draw_sheet(&mut surface, content, cols);

        ui::title_bar::<Self>(&mut surface, title);
        let status_text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &status_text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(WarbandSheet);

#[cfg(test)]
mod tests {
    use super::{Cols, seed_fighters};

    #[test]
    fn every_fighter_name_is_unique() {
        let fighters = seed_fighters();
        let mut names: Vec<&str> = fighters.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            fighters.len(),
            "the roster must not repeat a fighter name"
        );
    }

    #[test]
    fn column_boundaries_are_strictly_increasing() {
        for width in [73, 80, 100, 158, 220] {
            let cols = Cols::compute(width);
            let b = cols.boundaries();
            for pair in b.windows(2) {
                assert!(
                    pair[1] > pair[0],
                    "boundaries must not collapse at width {width}: {b:?}"
                );
            }
        }
    }

    #[test]
    fn every_column_fits_its_widest_possible_label() {
        for width in [73, 80, 100, 158, 220] {
            let cols = Cols::compute(width);
            let (_, name_w) = cols.column(0);
            assert!(name_w as usize >= "Wilhelm Krantz".len());
            let (_, type_w) = cols.column(1);
            assert!(type_w as usize >= "Youngblood".len());
            for i in 0..9 {
                let (_, w) = cols.column(2 + i);
                assert!(
                    w >= 2,
                    "stat column must fit two-letter labels like WS/BS/Ld"
                );
            }
        }
    }
}
