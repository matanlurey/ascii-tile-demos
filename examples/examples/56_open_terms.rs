//! 56: Open Terms -- an opinion total itemised into signed, actionable
//! modifiers, from Crusader Kings' opinion breakdown and negotiation screens.
//!
//! Every diplomacy-adjacent demo elsewhere in this gallery resolves a
//! relationship as a single number or a colour on a map (52 `quiet_march`'s
//! envoys flip a province's tint and stop there). None of them show *why*
//! someone feels how they feel. This demo's anchor is the itemised ledger:
//! one character's opinion of the player is a running total assembled from
//! labelled, signed modifiers, each on its own row, arithmetic legible
//! enough that a player can read the reasoning without being told it. The
//! player then acts on individual rows -- a gift, a betrothal, a pressed
//! claim, a truce -- and watches the total recompute with the changed row
//! lit up, so cause and effect stay obvious. There is no reference
//! screenshot for this one; it is built from the mechanic itself.
//!
//! Techniques on show:
//!
//! - **A ruled, signed ledger** ([`OpenTerms::draw_breakdown`]): every
//!   modifier row aligns its signed value in a fixed-width column, followed
//!   by a fixed-width decay column, so the arithmetic reads as a column of
//!   numbers rather than a paragraph. A horizontal rule and a `Total:` row
//!   close it the way a paper ledger would.
//! - **Decay as a countdown, not a fade** ([`OpenTerms::advance_turn`],
//!   [`Modifier`]): time-limited modifiers carry a remaining-turns count
//!   that steps down on whole turns (never tweened -- see the brief's rule
//!   against easing numbers), plus a continuous sub-turn progress bar that
//!   *is* tweened, because a bar is decoration and a number is not.
//! - **An eased attitude needle, a stepped total**
//!   ([`OpenTerms::simulate`]): the printed total jumps the instant a
//!   modifier changes, because the arithmetic must be exact; a separate
//!   needle glyph on the attitude scale eases toward that total over
//!   real time, which is what proves the demo is animating from
//!   `frame.delta` without ever making a number lie.
//! - **Actions gated by the band they change**
//!   ([`OpenTerms::apply_action`]): a betrothal is refused outright below
//!   Indifferent and a truce is refused while Furious, so the attitude band
//!   is not just a label -- it is the thing that decides which actions are
//!   even on offer.
//! - **A row-count budget with an honest overflow**
//!   ([`OpenTerms::visible_rows`]): when a panel is too short to show every
//!   modifier, the tail collapses into one `+N more` aggregate row whose
//!   value is the exact sum of what it hides, so the displayed rows always
//!   add up to the displayed total, at any height.
//! - **Tap-select, tap-act, tap-inspect** ([`ui::touch::Hotspots`]): the
//!   roster, the four action buttons, and every modifier row are real touch
//!   targets grown to [`ui::touch::TAP_W`]x[`ui::touch::TAP_H`]; the same
//!   three gestures also have full keyboard parity.
//!
//! ```sh
//! cargo run --example 56_open_terms --features crossterm
//! cargo run --example 56_open_terms --features software
//! cargo run --example 56_open_terms --features gl
//! cargo run --example 56_open_terms  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, ACCENT, DIM, FG};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::palette::{mix, rgb};

/// How many characters sit in the roster. Fixed, not generated, so their
/// names can be assigned without replacement from a pool sized exactly to
/// this count -- the round-3 rule against sampling names with replacement is
/// trivially satisfied when there is no sampling at all.
const CHAR_COUNT: usize = 5;

/// Real seconds per game turn. Decaying modifiers lose one turn of duration
/// each time this elapses; short enough that the thumbnail tool's animation
/// sampler (which spans several seconds past frame 60) sees at least one
/// tick, long enough that the three-frame snapshot test never crosses one.
const TURN_SECONDS: f32 = 3.0;

/// How long a changed row stays highlighted after an action touches it, in
/// seconds. Long enough to be seen, short enough not to still be glowing by
/// the time the player has read the next line of the log.
const HIGHLIGHT_SECONDS: f32 = 2.2;

const GIFT_COST_GOLD: i32 = 50;
const BETROTHAL_COST_PRESTIGE: i32 = 40;
const CLAIM_COST_PRESTIGE: i32 = 20;
const TRUCE_COST_GOLD: i32 = 30;

/// A warm amber for refusals and low resources, distinct from the log's
/// other colors so a player scanning the log can spot "this didn't work"
/// without reading every line.
const WARN: Color = Color::Rgb {
    r: 216,
    g: 150,
    b: 90,
};

/// One labelled, signed contribution to a character's opinion.
#[derive(Clone)]
struct Modifier {
    label: &'static str,
    value: i32,
    /// Remaining duration in turns, fractional so a sub-turn progress bar can
    /// be drawn without touching the whole-turn count the label shows.
    /// `None` means permanent.
    remaining: Option<f32>,
    /// The duration a fresh copy of this modifier starts at, for computing
    /// the progress bar's denominator. Meaningless when `remaining` is
    /// `None`.
    max: f32,
    /// One line of in-world reasoning, shown when the row is tapped.
    desc: &'static str,
}

impl Modifier {
    const fn permanent(label: &'static str, value: i32, desc: &'static str) -> Self {
        Self {
            label,
            value,
            remaining: None,
            max: 0.0,
            desc,
        }
    }

    const fn decaying(label: &'static str, value: i32, turns: f32, desc: &'static str) -> Self {
        Self {
            label,
            value,
            remaining: Some(turns),
            max: turns,
            desc,
        }
    }
}

/// A member of the roster and their opinion of the player.
struct Character {
    name: &'static str,
    title: &'static str,
    traits: [char; 2],
    modifiers: Vec<Modifier>,
}

impl Character {
    /// The running total: the whole reason this demo exists is that this
    /// number is never entered directly, only ever assembled from the rows
    /// above it.
    fn total(&self) -> i32 {
        self.modifiers.iter().map(|m| m.value).sum()
    }
}

/// The five-band attitude scale every total maps onto.
///
/// Named bands rather than a raw number as the primary read: Crusader
/// Kings' own screen leads with "Displeased", not "-14", and the number is
/// there to justify the label rather than the other way round.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Band {
    Furious,
    Displeased,
    Indifferent,
    Cordial,
    Loyal,
}

impl Band {
    const fn of(total: i32) -> Self {
        if total <= -40 {
            Self::Furious
        } else if total <= -10 {
            Self::Displeased
        } else if total < 10 {
            Self::Indifferent
        } else if total < 40 {
            Self::Cordial
        } else {
            Self::Loyal
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Furious => "Furious",
            Self::Displeased => "Displeased",
            Self::Indifferent => "Indifferent",
            Self::Cordial => "Cordial",
            Self::Loyal => "Loyal",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Furious => rgb(216, 84, 78),
            Self::Displeased => rgb(214, 140, 90),
            Self::Indifferent => rgb(180, 180, 160),
            Self::Cordial => rgb(140, 196, 120),
            Self::Loyal => rgb(96, 210, 160),
        }
    }

    /// Position on the five-band scale, `0.0` at Furious's floor to `1.0` at
    /// Loyal's, for drawing the needle and the scale ruler at the same
    /// coordinates a `-100..100` total would occupy.
    fn scale_pos(total: i32) -> f32 {
        ((total as f32 + 100.0) / 200.0).clamp(0.0, 1.0)
    }
}

/// One action the player can take against the selected character.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Gift,
    Betrothal,
    Claim,
    Truce,
}

impl ActionKind {
    const ALL: [Self; 4] = [Self::Gift, Self::Betrothal, Self::Claim, Self::Truce];

    const fn label(self) -> &'static str {
        match self {
            Self::Gift => "Send Gift",
            Self::Betrothal => "Betrothal",
            Self::Claim => "Press Claim",
            Self::Truce => "Offer Truce",
        }
    }

    const fn key(self) -> char {
        match self {
            Self::Gift => 'G',
            Self::Betrothal => 'B',
            Self::Claim => 'C',
            Self::Truce => 'T',
        }
    }

    const fn cost_text(self) -> &'static str {
        match self {
            Self::Gift => "50g",
            Self::Betrothal => "40p",
            Self::Claim => "20p",
            Self::Truce => "30g",
        }
    }
}

/// What a tap or a key resolves to. Hotspots carry this so hit-testing and
/// keyboard handling both end in the same handful of state transitions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Character(usize),
    Action(ActionKind),
    /// Index into the *currently drawn* row list, which may end in one
    /// aggregate row past the real modifiers -- see [`OpenTerms::visible_rows`].
    Row(usize),
}

/// The two column widths every row in the ledger shares, bundled so
/// `draw_row`/`draw_modifier_block` take one argument instead of two --
/// keeps both under clippy's argument-count limit and keeps the two widths
/// from drifting apart between the value column and wherever the decay
/// column actually starts.
#[derive(Clone, Copy)]
struct RowCols {
    label_w: u16,
    decay_col: u16,
}

/// A row as actually drawn: either one real modifier or the summary of
/// however many did not fit. Kept separate from [`Modifier`] so the "sum of
/// displayed rows equals the displayed total" invariant can be checked
/// against exactly what was drawn, aggregate included.
struct DisplayRow {
    label: String,
    value: i32,
    /// Whole turns remaining, for the row's decay column. `None` for
    /// permanent rows and for the aggregate row (which mixes decaying and
    /// permanent modifiers and has no single duration to show).
    turns_left: Option<i32>,
    /// Fraction of the current turn already elapsed, for the sub-turn
    /// progress sliver. Only meaningful alongside `turns_left`.
    turn_frac: f32,
    /// `true` for the single synthetic "+N more" row.
    aggregate: bool,
}

/// Builds the rows a breakdown panel of `capacity` rows would actually draw.
///
/// Kept free of any [`Surface`] so it can be unit tested directly against
/// the "displayed total equals the sum of displayed rows" invariant, and so
/// the drawing code and the test share one source of truth for what
/// "displayed" means. When every modifier fits, this is just the modifiers,
/// unreordered, matching the order the player added them. When it does not,
/// the tail (not the head, which is what a scan-down reader has already
/// started reading) collapses into one aggregate row carrying the exact sum
/// of what it hides, so truncating a ledger under a squeeze can never make
/// its arithmetic stop adding up.
fn visible_rows(modifiers: &[Modifier], turn_frac: f32, capacity: usize) -> Vec<DisplayRow> {
    let row = |m: &Modifier| DisplayRow {
        label: m.label.to_string(),
        value: m.value,
        turns_left: m.remaining.map(|r| r.ceil() as i32),
        turn_frac,
        aggregate: false,
    };

    if capacity == 0 {
        return Vec::new();
    }
    if modifiers.len() <= capacity {
        return modifiers.iter().map(row).collect();
    }

    // At least one row must be sacrificed to the aggregate, so at most
    // `capacity - 1` real modifiers are shown ahead of it.
    let keep = capacity.saturating_sub(1);
    let mut rows: Vec<DisplayRow> = modifiers.iter().take(keep).map(row).collect();
    let hidden = &modifiers[keep..];
    let hidden_sum: i32 = hidden.iter().map(|m| m.value).sum();
    rows.push(DisplayRow {
        label: format!("+{} more", hidden.len()),
        value: hidden_sum,
        turns_left: None,
        turn_frac: 0.0,
        aggregate: true,
    });
    rows
}

/// Formats a signed value right-aligned in a 4-column field: `" +15"`,
/// `"  -5"`, `" -30"`. The sign always shows (even for zero, which reads as
/// `"  +0"`) so a scanning eye never has to infer a missing `+`.
fn signed4(value: i32) -> String {
    format!("{:>4}", format!("{value:+}"))
}

/// Shortens `text` to `width` columns, marking the cut with `...` rather
/// than silently chopping a word in half. `panel::spans`/`truncate` clip
/// bare, which is fine for chrome (a panel title losing its tail is still
/// legibly a panel), but wrong for prose a player is meant to read as a
/// sentence -- a roster title or a modifier label that stops mid-word reads
/// as a bug, not as "there wasn't room". Falls back to a bare clip only when
/// `width` is too small to fit the ellipsis itself.
fn fit(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width < 4 {
        return retroglyph_widgets::truncate(text, width).to_string();
    }
    format!("{}...", retroglyph_widgets::truncate(text, width - 3))
}

/// How many raw negotiation-log messages [`OpenTerms::log`] keeps before the
/// oldest is evicted. A count of messages, not of the wrapped lines they end
/// up drawing as -- `draw_log` re-wraps every message to whatever width the
/// current layout gives the panel, so how many *lines* that turns into is a
/// draw-time question, not a storage one.
const LOG_CAPACITY: usize = 48;

/// Appends one message to the negotiation log, evicting the oldest once
/// `LOG_CAPACITY` is exceeded.
///
/// A free function taking `&mut Vec<_>` rather than a method on `OpenTerms`,
/// so [`OpenTerms::default`] can build the log's first two lines before a
/// `Self` exists to call a method on.
fn log_push(log: &mut Vec<(String, Color)>, text: impl Into<String>, color: Color) {
    log.push((text.into(), color));
    if log.len() > LOG_CAPACITY {
        log.remove(0);
    }
}

/// Word-wraps `text` to `width` columns, breaking only between words.
///
/// The negotiation log's own analogue of [`fit`]: `fit` shortens a single
/// line that must stay one line (a roster title, a row label), which is the
/// right call when there is a fixed column budget either side of it. A log
/// entry has no such neighbour -- the whole panel width is its budget -- so
/// letting a long line spill onto a second row reads as what it is (one
/// sentence, wrapped) rather than as a name with its value chopped off.
/// Falls back to `fit`'s ellipsis only for a single word wider than `width`
/// on its own, which nothing this demo actually logs hits.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let cols = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let joined_len = current.chars().count() + usize::from(!current.is_empty()) + word_len;
        if !current.is_empty() && joined_len > cols {
            lines.push(std::mem::take(&mut current));
        }
        if word_len > cols {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(fit(word, width));
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Adds a fresh copy of a decaying/permanent modifier, or refreshes an
/// existing one's remaining duration back to full -- sending a second gift
/// renews the goodwill rather than stacking a duplicate line, which is both
/// truer to how these systems work and what keeps the ledger from growing
/// without bound under repeated play.
fn upsert(modifiers: &mut Vec<Modifier>, fresh: Modifier) {
    if let Some(existing) = modifiers.iter_mut().find(|m| m.label == fresh.label) {
        existing.remaining = fresh.remaining;
        existing.max = fresh.max;
    } else {
        modifiers.push(fresh);
    }
}

/// Builds the fixed five-character roster. Distinct arithmetic per
/// character on purpose: every attitude band gets at least one example on
/// screen, and character 0's modifiers are the exact worked example from
/// the design brief (`+15/+10/+8/-30/-12/-5 = -14, Displeased`), so a reader
/// who has seen the mechanic described in prose finds it verified on the
/// first row of the roster.
// A long literal table, not long logic: five hand-authored characters each
// need their own worked modifier list, and splitting that across helper
// functions would only add indirection between a reader and the data.
#[allow(clippy::too_many_lines)]
fn build_characters() -> Vec<Character> {
    vec![
        Character {
            name: "Aldric of Vasgard",
            title: "Duke of the Western March",
            traits: ['\u{2660}', '\u{2663}'], // ambitious, cruel
            modifiers: vec![
                Modifier::permanent("Same faith", 15, "Both crowns answer to the same creed."),
                Modifier::decaying(
                    "Recent gift",
                    10,
                    20.0,
                    "A gift was sent recently; the goodwill fades as the memory does.",
                ),
                Modifier::permanent(
                    "Long alliance",
                    8,
                    "Your houses have stood together for a generation.",
                ),
                Modifier::decaying(
                    "Recent war",
                    -30,
                    15.0,
                    "You fought each other within living memory.",
                ),
                Modifier::permanent(
                    "Rival claim on Aquitaine",
                    -12,
                    "You each hold a competing claim to the same seat.",
                ),
                Modifier::permanent(
                    "Different culture",
                    -5,
                    "Your court keeps foreign customs, and it shows.",
                ),
            ],
        },
        Character {
            name: "Seraphine Kestrel",
            title: "Marshal of Coastal Fleet",
            traits: ['\u{2665}', '\u{2666}'], // kind, greedy
            modifiers: vec![
                Modifier::permanent(
                    "Shared upbringing",
                    12,
                    "You were fostered in the same household as children.",
                ),
                Modifier::decaying(
                    "Generous dowry",
                    9,
                    30.0,
                    "A dowry beyond what custom required still buys warmth.",
                ),
                Modifier::permanent(
                    "Trade agreement",
                    6,
                    "A standing agreement keeps both treasuries fed.",
                ),
                Modifier::decaying(
                    "Insulted at court",
                    -8,
                    10.0,
                    "A remark at your last banquet has not been forgotten.",
                ),
                Modifier::permanent(
                    "Envies your title",
                    -6,
                    "A rank she believes should have come to her instead.",
                ),
            ],
        },
        Character {
            name: "Boru mac Cathal",
            title: "Chief of the Hollow Clans",
            traits: ['\u{2663}', '\u{2660}'], // cruel, ambitious
            modifiers: vec![
                Modifier::permanent(
                    "Clan blood-debt",
                    -25,
                    "A debt of blood between your lines, unpaid for three generations.",
                ),
                Modifier::decaying(
                    "Broken promise",
                    -14,
                    8.0,
                    "You swore an oath at the last moot and did not keep it.",
                ),
                Modifier::permanent(
                    "Razed village",
                    -10,
                    "Your levies burned a clan settlement two winters back.",
                ),
                Modifier::permanent(
                    "Shared enemy",
                    8,
                    "A common rival makes for an uneasy common cause.",
                ),
            ],
        },
        Character {
            name: "Ysolt Draven",
            title: "Lady of the Amber Coast",
            traits: ['\u{2666}', '\u{2665}'], // greedy, kind
            modifiers: vec![
                Modifier::decaying(
                    "Flattering letters",
                    7,
                    12.0,
                    "Correspondence a touch more attentive than duty required.",
                ),
                Modifier::permanent(
                    "Common enemy",
                    10,
                    "You both have reason to see the same rival humbled.",
                ),
                Modifier::permanent(
                    "Land dispute",
                    -9,
                    "A parcel of border land neither of you will concede.",
                ),
                Modifier::permanent(
                    "Distant kin",
                    4,
                    "A shared great-grandparent, twice removed.",
                ),
                Modifier::decaying(
                    "Slighted honor",
                    -11,
                    18.0,
                    "You seated her below her rank at the last council.",
                ),
            ],
        },
        Character {
            name: "Konrad Hollis",
            title: "Spymaster of Grey Court",
            traits: ['\u{2660}', '\u{2666}'], // ambitious, greedy
            modifiers: vec![
                Modifier::decaying(
                    "Owes you a favor",
                    18,
                    25.0,
                    "You covered for him once, and he has not forgotten it.",
                ),
                Modifier::permanent(
                    "Loyal service",
                    26,
                    "Years in your service, without a single lapse.",
                ),
                Modifier::permanent(
                    "Suspicious of rivals",
                    -4,
                    "He trusts your other favorites less than he trusts you.",
                ),
            ],
        },
    ]
}

/// Highlights a row that an action just changed, so the ledger's cause and
/// effect stay visible rather than only inferable from the log.
struct Highlight {
    character: usize,
    label: &'static str,
    remaining: f32,
}

/// State: the roster, the player's resources, which character and modifier
/// row are focused, the negotiation log, and the touch/animation plumbing
/// every interface-scale demo shares.
pub struct OpenTerms {
    characters: Vec<Character>,
    selected: usize,
    /// The modifier label currently under detail inspection, looked up by
    /// name each frame rather than by index, since actions insert and
    /// remove rows and a stored index would silently point at the wrong
    /// modifier after either.
    inspecting: Option<&'static str>,
    gold: i32,
    prestige: i32,
    turn: u32,
    turn_clock: f32,
    /// The needle's own eased position on the `0.0..=1.0` attitude scale.
    /// Deliberately not the same value as the printed total: the total must
    /// jump the instant a modifier changes, and the needle is what shows
    /// that a frame is still passing without ever making the number lie.
    needle: f32,
    highlight: Option<Highlight>,
    /// Raw, unwrapped negotiation messages, oldest first. Wrapped to the
    /// panel's actual width in [`Self::draw_log`] rather than here -- see
    /// [`wrap`] -- so the same log reads correctly whether it is drawn as a
    /// narrow desktop column or a full-width stacked panel.
    log: Vec<(String, Color)>,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Hit>,
    fps: FpsMeter,
}

impl Default for OpenTerms {
    fn default() -> Self {
        let characters = build_characters();
        let total0 = characters[0].total();
        let mut log = Vec::new();
        log_push(&mut log, "Negotiations opened.", FG);
        log_push(
            &mut log,
            format!(
                "{}: {} ({total0:+}).",
                characters[0].name,
                Band::of(total0).label()
            ),
            DIM,
        );
        Self {
            characters,
            selected: 0,
            inspecting: None,
            gold: 150,
            prestige: 100,
            turn: 0,
            turn_clock: 0.0,
            needle: Band::scale_pos(total0),
            highlight: None,
            log,
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl OpenTerms {
    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Char(']') => {
                self.select((self.selected + 1) % self.characters.len());
            }
            KeyCode::Up | KeyCode::Char('[') => {
                let n = self.characters.len();
                self.select((self.selected + n - 1) % n);
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if let Some(m) = self.characters[self.selected].modifiers.get(idx) {
                    self.inspecting = Some(m.label);
                }
            }
            KeyCode::Escape => self.inspecting = None,
            KeyCode::Char(c) => {
                let upper = c.to_ascii_uppercase();
                if let Some(action) = ActionKind::ALL.iter().find(|a| a.key() == upper) {
                    self.apply_action(*action);
                }
            }
            _ => {}
        }
    }

    fn handle_tap(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos) {
            Some(&Hit::Character(idx)) => self.select(idx),
            Some(&Hit::Action(action)) => self.apply_action(action),
            Some(&Hit::Row(row_idx)) => {
                let cap = self.characters[self.selected].modifiers.len().max(1);
                let rows = visible_rows(&self.characters[self.selected].modifiers, 0.0, cap);
                if let Some(row) = rows.get(row_idx)
                    && !row.aggregate
                    && let Some(m) = self.characters[self.selected]
                        .modifiers
                        .iter()
                        .find(|m| m.label == row.label)
                {
                    self.inspecting = Some(m.label);
                }
            }
            None => {}
        }
    }

    const fn select(&mut self, idx: usize) {
        self.selected = idx;
        self.inspecting = None;
    }

    /// Advances one game turn: every decaying modifier on every character
    /// loses one turn, and anything that reaches zero is removed and
    /// announced. Runs on all characters, not only the one on screen, so
    /// switching the roster selection never itself changes what has decayed.
    fn advance_turn(&mut self) {
        self.turn += 1;
        for ch in &mut self.characters {
            let mut expired: Vec<usize> = Vec::new();
            for (i, m) in ch.modifiers.iter_mut().enumerate() {
                if let Some(remaining) = m.remaining.as_mut() {
                    *remaining -= 1.0;
                    if *remaining <= 0.0 {
                        expired.push(i);
                    }
                }
            }
            for &i in expired.iter().rev() {
                let removed = ch.modifiers.remove(i);
                log_push(
                    &mut self.log,
                    format!("{}: '{}' has expired.", ch.name, removed.label),
                    DIM,
                );
                if self.inspecting == Some(removed.label) {
                    self.inspecting = None;
                }
            }
        }
    }

    /// Advances turn time and the eased needle. See the field docs on
    /// [`OpenTerms::needle`] for why the needle, not the total, is the thing
    /// that tweens.
    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        self.turn_clock += dt;
        // Counting whole turns with a division rather than looping on a
        // float comparison is both what clippy's `while_float` lint asks
        // for and the safer choice if a backend ever hands over a huge
        // single `dt`: the loop below is bounded by an integer count
        // instead of an unbounded float-subtraction loop.
        let due = (self.turn_clock / TURN_SECONDS).floor();
        if due >= 1.0 {
            self.turn_clock = due.mul_add(-TURN_SECONDS, self.turn_clock);
            for _ in 0..due as u32 {
                self.advance_turn();
            }
        }

        let target = Band::scale_pos(self.characters[self.selected].total());
        // Exponential ease toward the target rather than a fixed-rate lerp,
        // so the needle settles quickly after a big jump (an action) and
        // barely moves during a quiet turn, matching how an analogue needle
        // actually behaves rather than crawling at a constant speed.
        let rate = 5.0;
        self.needle = (target - self.needle).mul_add(1.0 - (-rate * dt).exp(), self.needle);

        if let Some(h) = &mut self.highlight {
            h.remaining -= dt;
            if h.remaining <= 0.0 {
                self.highlight = None;
            }
        }
    }

    fn refuse(&mut self, name: &str, why: &str) {
        log_push(&mut self.log, format!("{name} refuses: {why}"), WARN);
    }

    fn cannot_afford(&mut self, what: &str) {
        log_push(&mut self.log, format!("Not enough {what} for this."), WARN);
    }

    /// Applies one action to the selected character: checks funds and the
    /// attitude gate, then mutates the modifier list and logs the result.
    /// Every branch either spends a resource and changes the ledger, or
    /// changes nothing at all -- there is no partial-success path, which is
    /// what keeps "was that refused?" answerable from the log alone.
    /// Dispatches to one action's own handler. Split out per action (rather
    /// than one long match) so each handler stays short enough to read as
    /// "check funds and the gate, then mutate one modifier, then log it" --
    /// the shape every one of the four actions actually has.
    fn apply_action(&mut self, action: ActionKind) {
        match action {
            ActionKind::Gift => self.try_gift(),
            ActionKind::Betrothal => self.try_betrothal(),
            ActionKind::Claim => self.try_claim(),
            ActionKind::Truce => self.try_truce(),
        }
    }

    fn try_gift(&mut self) {
        let idx = self.selected;
        let name = self.characters[idx].name;
        if self.gold < GIFT_COST_GOLD {
            self.cannot_afford("gold");
            return;
        }
        self.gold -= GIFT_COST_GOLD;
        upsert(
            &mut self.characters[idx].modifiers,
            Modifier::decaying(
                "Recent gift",
                10,
                20.0,
                "A gift was sent recently; the goodwill fades as the memory does.",
            ),
        );
        log_push(
            &mut self.log,
            format!("Sent a gift to {name}. (+10, decays)"),
            ACCENT,
        );
        self.mark_highlight(idx, "Recent gift");
    }

    fn try_betrothal(&mut self) {
        let idx = self.selected;
        let name = self.characters[idx].name;
        let band = Band::of(self.characters[idx].total());
        if band < Band::Indifferent {
            self.refuse(name, "relations are too poor for a betrothal.");
            return;
        }
        if self.characters[idx]
            .modifiers
            .iter()
            .any(|m| m.label == "Betrothal arranged")
        {
            log_push(
                &mut self.log,
                format!("{name} is already betrothed to your line."),
                DIM,
            );
            return;
        }
        if self.prestige < BETROTHAL_COST_PRESTIGE {
            self.cannot_afford("prestige");
            return;
        }
        self.prestige -= BETROTHAL_COST_PRESTIGE;
        self.characters[idx].modifiers.push(Modifier::permanent(
            "Betrothal arranged",
            8,
            "A marriage has been arranged between your houses.",
        ));
        log_push(
            &mut self.log,
            format!("Arranged a betrothal with {name}. (+8)"),
            ACCENT,
        );
        self.mark_highlight(idx, "Betrothal arranged");
    }

    fn try_claim(&mut self) {
        let idx = self.selected;
        let name = self.characters[idx].name;
        if self.characters[idx]
            .modifiers
            .iter()
            .any(|m| m.label == "Claim pressed")
        {
            log_push(
                &mut self.log,
                format!("A claim is already pressed against {name}."),
                DIM,
            );
            return;
        }
        if self.prestige < CLAIM_COST_PRESTIGE {
            self.cannot_afford("prestige");
            return;
        }
        self.prestige -= CLAIM_COST_PRESTIGE;
        self.characters[idx].modifiers.push(Modifier::permanent(
            "Claim pressed",
            -15,
            "You have pressed a rival claim against their holdings.",
        ));
        log_push(
            &mut self.log,
            format!("Pressed a claim against {name}. (-15)"),
            WARN,
        );
        self.mark_highlight(idx, "Claim pressed");
    }

    fn try_truce(&mut self) {
        let idx = self.selected;
        let name = self.characters[idx].name;
        let band = Band::of(self.characters[idx].total());
        if band == Band::Furious {
            self.refuse(name, "too hostile to discuss a truce.");
            return;
        }
        if !self.characters[idx]
            .modifiers
            .iter()
            .any(|m| m.label == "Recent war")
        {
            log_push(
                &mut self.log,
                format!("There is no war with {name} to end."),
                DIM,
            );
            return;
        }
        if self.gold < TRUCE_COST_GOLD {
            self.cannot_afford("gold");
            return;
        }
        self.gold -= TRUCE_COST_GOLD;
        self.characters[idx]
            .modifiers
            .retain(|m| m.label != "Recent war");
        upsert(
            &mut self.characters[idx].modifiers,
            Modifier::decaying(
                "Truce honoured",
                5,
                10.0,
                "A truce was offered and accepted; the war is over for now.",
            ),
        );
        log_push(
            &mut self.log,
            format!("Offered a truce to {name}. War ended."),
            ACCENT,
        );
        self.mark_highlight(idx, "Truce honoured");
        if self.inspecting == Some("Recent war") {
            self.inspecting = None;
        }
    }

    const fn mark_highlight(&mut self, character: usize, label: &'static str) {
        self.highlight = Some(Highlight {
            character,
            label,
            remaining: HIGHLIGHT_SECONDS,
        });
    }

    fn status(&self) -> String {
        format!(
            "gold {}  prestige {}  turn {}",
            self.gold, self.prestige, self.turn
        )
    }

    // -- layout -------------------------------------------------------

    /// Whether the viewport is narrow enough that a three-column layout
    /// would leave any of roster, ledger, or log too cramped to read.
    /// Combines [`Shape::stacks`] (true for a portrait phone, where a
    /// side-by-side layout is structurally wrong) with a plain width floor
    /// (true for the 80x24 grid every snapshot test runs at, which is wide
    /// enough to read as a short desktop window but not wide enough to hold
    /// three real columns) -- the same two-part test `21_deck_plan` uses for
    /// its own sidebar.
    const fn stack(content: Rect) -> bool {
        Shape::of(content).stacks() || content.width() < 100
    }

    fn draw<B: Backend>(&mut self, term: &mut Terminal<B>) {
        self.hotspots.clear();
        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        if Self::stack(content) {
            self.draw_stacked(&mut surface, content);
        } else {
            self.draw_columns(&mut surface, content);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
    }

    /// Splits a height budget of `h` rows between the roster, the ledger,
    /// the log, and the actions bar, for the stacked layout.
    ///
    /// Two branches rather than one formula: when `h` is generous enough to
    /// give every panel its comfortable size and still have height left
    /// over, that leftover goes entirely to the ledger, which is the
    /// screen's centrepiece and the thing worth spending spare rows on. When
    /// it is not (this is what the 80x24 grid every snapshot test runs at
    /// hits), the three supporting panels drop to their bare minimums first
    /// -- each has a documented floor below which it stops working -- and
    /// the ledger gets the largest share of whatever is left, protected by
    /// its own floor so it can never be squeezed to nothing.
    fn stacked_budget(h: u16) -> (u16, u16, u16, u16) {
        const ROSTER_WANT: u16 = CHAR_COUNT as u16 * 4 + 2;
        const ACTIONS_WANT: u16 = 7;
        const LOG_WANT: u16 = 6;
        const BREAKDOWN_MIN: u16 = 8;
        const ROSTER_MIN: u16 = CHAR_COUNT as u16 + 2;
        const ACTIONS_MIN: u16 = 4;

        if ROSTER_WANT + ACTIONS_WANT + LOG_WANT + BREAKDOWN_MIN <= h {
            let breakdown_h = h - ROSTER_WANT - ACTIONS_WANT - LOG_WANT;
            return (ROSTER_WANT, breakdown_h, LOG_WANT, ACTIONS_WANT);
        }

        let roster_h = ROSTER_MIN.min(h);
        let mut remaining = h - roster_h;
        let actions_h = ACTIONS_MIN.min(remaining);
        remaining -= actions_h;
        let breakdown_h = BREAKDOWN_MIN.max(remaining * 7 / 10).min(remaining);
        remaining -= breakdown_h;
        let log_h = remaining;
        (roster_h, breakdown_h, log_h, actions_h)
    }

    /// Portrait phones and the 80x24 test grid: everything stacked top to
    /// bottom, actions pinned to the very bottom (the thumb zone), the
    /// roster a compact top strip since it is read-only-ish and the ledger
    /// is the thing worth spending height on.
    fn draw_stacked(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let (roster_h, breakdown_h, log_h, actions_h) = Self::stacked_budget(content.height());

        let (roster_area, rest) = panel::split_top(content, roster_h);
        let (breakdown_area, rest) = panel::split_top(rest, breakdown_h);
        let (log_area, actions_area) = panel::split_top(rest, log_h);
        debug_assert_eq!(
            actions_area.height(),
            actions_h,
            "the budget must account for every row"
        );

        self.draw_roster(surface, roster_area);
        self.draw_breakdown(surface, breakdown_area);
        self.draw_log(surface, log_area);
        self.draw_actions(surface, actions_area);
    }

    /// Landscape phones and desktop: roster as a left column, the log as a
    /// right column, the ledger and the actions bar sharing the centre with
    /// actions still pinned to the bottom.
    fn draw_columns(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let roster_w = 30u16.min(content.width() / 4);
        let log_w = 34u16.min(content.width() / 4);
        let (roster_area, rest) = panel::split_left(content, roster_w);
        let (center, log_area) = panel::split_right(rest, log_w);

        let actions_h = 9.min(center.height().saturating_sub(8)).max(4);
        let (breakdown_area, actions_area) = panel::split_bottom(center, actions_h);

        self.draw_roster(surface, roster_area);
        self.draw_breakdown(surface, breakdown_area);
        self.draw_log(surface, log_area);
        self.draw_actions(surface, actions_area);
    }

    fn draw_roster(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Court").border(Border::Single);
        let inner = panel.draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }

        let n = self.characters.len() as u16;
        // Down to a single combined line per entry under real pressure
        // (name and attitude share one row, see `draw_roster_entry`), up to
        // four (title, then the single modifier that swings their opinion
        // most) whenever there is room to spare -- the same "more roster
        // entries" growth valve the round-3 rule names, applied per entry
        // rather than to the count, since the count is fixed by how many
        // named characters this demo has. Never zero: a roster entry with
        // nothing drawn could still be tapped but could not be told apart
        // from its neighbour.
        let entry_h = (inner.height() / n.max(1)).clamp(1, 4);

        for i in 0..self.characters.len() {
            let y0 = inner.top() + i as u16 * entry_h;
            if y0 + entry_h > inner.bottom() && i as u16 * entry_h >= inner.height() {
                break;
            }
            let rows_left = inner.bottom().saturating_sub(y0);
            let h = entry_h.min(rows_left);
            if h == 0 {
                break;
            }
            let slot = Rect::new(inner.left(), y0, inner.width(), h);
            self.draw_roster_entry(surface, slot, i);
            self.hotspots.push_tappable(slot, area, Hit::Character(i));
        }
    }

    fn draw_roster_entry(&self, surface: &mut Surface<'_>, slot: Rect, idx: usize) {
        let ch = &self.characters[idx];
        let band = Band::of(ch.total());
        let selected = idx == self.selected;
        let bg = if selected {
            rgb(28, 30, 42)
        } else {
            panel::PANEL_BG
        };
        if selected {
            surface.fill_rect(slot, ' ', Style::new().bg(bg));
        }
        let marker = if selected { '\u{25BA}' } else { ' ' };
        let name_color = if selected { ACCENT } else { FG };

        if slot.height() == 1 {
            // The single-row fallback: name and attitude share one line,
            // since attitude is the one thing every reference brief asks
            // the roster to always show, even squeezed down to its floor.
            let total = ch.total();
            let tail = format!(" {} {total:+}", band.label());
            let name_w = slot.width().saturating_sub(2 + tail.chars().count() as u16);
            let name = fit(ch.name, name_w);
            panel::spans(
                surface,
                (slot.left(), slot.top()),
                slot.width(),
                &[
                    Span::new(&marker.to_string(), ACCENT),
                    Span::new(" ", name_color),
                    Span::new(&name, name_color),
                    Span::new(&tail, band.color()),
                ],
                bg,
            );
            return;
        }

        let name = fit(ch.name, slot.width().saturating_sub(2));
        panel::spans(
            surface,
            (slot.left(), slot.top()),
            slot.width(),
            &[
                Span::new(&marker.to_string(), ACCENT),
                Span::new(" ", name_color),
                Span::new(&name, name_color),
            ],
            bg,
        );
        if slot.height() >= 2 {
            let total = ch.total();
            let traits: String = ch.traits.iter().collect();
            panel::spans(
                surface,
                (slot.left() + 2, slot.top() + 1),
                slot.width().saturating_sub(2),
                &[
                    Span::new(&traits, DIM),
                    Span::plain(" "),
                    Span::new(band.label(), band.color()),
                    Span::plain(&format!(" {total:+}")),
                ],
                bg,
            );
        }
        if slot.height() >= 3 {
            let title = fit(ch.title, slot.width().saturating_sub(2));
            panel::spans(
                surface,
                (slot.left() + 2, slot.top() + 2),
                slot.width().saturating_sub(2),
                &[Span::dim(&title)],
                bg,
            );
        }
        if slot.height() >= 4 {
            // The single modifier with the largest magnitude: a one-line
            // preview of *why* this character feels the way they do,
            // without having to select them and read the full ledger.
            if let Some(m) = ch.modifiers.iter().max_by_key(|m| m.value.unsigned_abs()) {
                let line = format!("top: {} ({:+})", m.label, m.value);
                let text = fit(&line, slot.width().saturating_sub(2));
                panel::spans(
                    surface,
                    (slot.left() + 2, slot.top() + 3),
                    slot.width().saturating_sub(2),
                    &[Span::dim(&text)],
                    bg,
                );
            }
        }
    }

    fn draw_breakdown(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let ch = &self.characters[self.selected];
        let title = format!("Opinion -- {}", ch.name);
        let panel = Panel::new()
            .title(&title)
            .border(Border::Double)
            .focused(true);
        let inner = panel.draw(surface, area);
        if inner.width() < 16 || inner.height() < 4 {
            return;
        }

        let total = ch.total();
        let band = Band::of(total);

        // Row 0: the attitude scale with the eased needle on it.
        self.draw_scale(
            surface,
            Rect::new(inner.left(), inner.top(), inner.width(), 1),
            band,
        );

        // The breathing row between the scale and the ledger is dropped
        // under real pressure: it is polish, and polish is the first thing
        // to give way to an actual modifier row when the panel is short on
        // height (the 80x24 grid every snapshot test runs at needs this).
        let gap = u16::from(inner.height() >= 9);
        let list_top = inner.top() + 1 + gap;

        // The rule and the total are pinned to the very bottom of the panel
        // rather than placed right after however much list content there
        // is: a ledger's subtotal belongs at the foot of the page, and
        // pinning it there is also what turns any slack between the list
        // and the footer into a place `draw_detail` can use instead of
        // leaving it as bare background.
        let rule_y = inner.bottom().saturating_sub(2);
        let total_y = inner.bottom().saturating_sub(1);
        let max_list_rows = rule_y.saturating_sub(list_top);

        let count = ch.modifiers.len() as u16;
        // How many rows each modifier gets: 1 (value/label/decay only) at a
        // minimum, growing to 4 (adding a description line, a decay bar or
        // "permanent" tag, and a blank divider) whenever there is enough
        // height to give *every* modifier the richer treatment. This is the
        // round-3 "more modifier rows when there is room" rule applied to a
        // ledger whose row *count* is fixed by the character's own opinion:
        // since more rows cannot mean more modifiers, it means richer ones.
        let tier = [4u16, 3, 2, 1]
            .into_iter()
            .find(|&t| count > 0 && max_list_rows >= count * t)
            .unwrap_or(1);
        // At tier >= 2 the whole list already fits at that density, so no
        // row needs to be sacrificed to the aggregate; only the compact
        // tier ever needs `visible_rows`'s overflow handling.
        let capacity = if tier >= 2 {
            ch.modifiers.len()
        } else {
            usize::from(max_list_rows)
        };

        let turn_frac = (self.turn_clock / TURN_SECONDS).clamp(0.0, 1.0);
        let rows = visible_rows(&ch.modifiers, turn_frac, capacity);

        let decay_col = if inner.width() >= 40 { 7u16 } else { 0 };
        let label_w = inner.width().saturating_sub(4 + 2 + decay_col).max(4);
        let cols = RowCols { label_w, decay_col };

        let mut y = list_top;
        for (i, row) in rows.iter().enumerate() {
            if y >= rule_y {
                break;
            }
            let slot_h = if row.aggregate { 1 } else { tier }.min(rule_y - y);
            self.draw_modifier_block(surface, inner, y, row, cols, slot_h);
            if !row.aggregate {
                self.hotspots.push_tappable(
                    Rect::new(inner.left(), y, inner.width(), slot_h),
                    area,
                    Hit::Row(i),
                );
            }
            y += slot_h.max(1);
        }

        // Whatever list space went unused (compact mode with room to spare)
        // becomes the detail area, so tapping a row for its rationale still
        // works even when the rich tiers already show every description.
        if y < rule_y {
            self.draw_detail(surface, inner, y);
        }

        let rule: String = "\u{2500}".repeat(inner.width_usize());
        surface.print(
            (inner.left(), rule_y),
            &rule,
            Style::new().fg(DIM).bg(panel::PANEL_BG),
        );
        panel::spans(
            surface,
            (inner.left(), total_y),
            inner.width(),
            &[
                Span::new(&signed4(total), band.color()),
                Span::plain("  Total: "),
                Span::new(band.label(), band.color()),
            ],
            panel::PANEL_BG,
        );
    }

    /// Draws one modifier's row and, at richer tiers, the extra lines that
    /// go with it: the description ([`Modifier::desc`]) at tier 2, and a
    /// decay progress bar (or a "Permanent" tag) at tier 3. Tier 4 adds
    /// nothing further to draw -- its extra row is the blank divider
    /// between one modifier's block and the next, which is exactly right
    /// left blank.
    fn draw_modifier_block(
        &self,
        surface: &mut Surface<'_>,
        inner: Rect,
        y: u16,
        row: &DisplayRow,
        cols: RowCols,
        slot_h: u16,
    ) {
        self.draw_row(surface, inner, y, row, cols);
        if row.aggregate || slot_h < 2 {
            return;
        }
        let Some(m) = self.characters[self.selected]
            .modifiers
            .iter()
            .find(|m| m.label == row.label)
        else {
            return;
        };
        let desc_w = inner.width().saturating_sub(2);
        let desc = fit(m.desc, desc_w);
        surface.print(
            (inner.left() + 2, y + 1),
            &desc,
            Style::new().fg(DIM).bg(panel::PANEL_BG),
        );
        if slot_h < 3 {
            return;
        }
        if let Some(remaining) = m.remaining {
            let frac = if m.max > 0.0 {
                (remaining / m.max).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let bar_w = inner.width().saturating_sub(4).min(30);
            panel::bar(
                surface,
                (inner.left() + 2, y + 2),
                bar_w,
                frac,
                mix(DIM, WARN, row.turn_frac * 0.4),
                rgb(28, 30, 40),
            );
        } else {
            surface.print(
                (inner.left() + 2, y + 2),
                "Permanent",
                Style::new().fg(DIM).bg(panel::PANEL_BG),
            );
        }
    }

    /// Shows the rationale for whichever row is under inspection, or a hint
    /// to tap one, in whatever space the breakdown panel had left over past
    /// the ledger itself. Only drawn when [`Self::draw_breakdown`] decided
    /// there was room to spare; see its `footer_rows`.
    fn draw_detail(&self, surface: &mut Surface<'_>, inner: Rect, y: u16) {
        let ch = &self.characters[self.selected];
        let text = self.inspecting.and_then(|label| {
            ch.modifiers
                .iter()
                .find(|m| m.label == label)
                .map(|m| m.desc)
        });
        match text {
            Some(desc) => panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::plain("> "), Span::new(desc, DIM)],
                panel::PANEL_BG,
            ),
            None => panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim("tap a row, or press 1-9, for its rationale")],
                panel::PANEL_BG,
            ),
        };
    }

    fn draw_scale(&self, surface: &mut Surface<'_>, area: Rect, band: Band) {
        if area.width() < 10 {
            return;
        }
        let bg = panel::PANEL_BG;
        let bands = [
            Band::Furious,
            Band::Displeased,
            Band::Indifferent,
            Band::Cordial,
            Band::Loyal,
        ];
        let seg_w = area.width() / 5;
        for (i, b) in bands.iter().enumerate() {
            let x = area.left() + i as u16 * seg_w;
            let w = if i == 4 {
                area.width() - seg_w * 4
            } else {
                seg_w
            };
            let track = if *b == band {
                mix(
                    b.color(),
                    Color::Rgb {
                        r: 10,
                        g: 10,
                        b: 14,
                    },
                    0.25,
                )
            } else {
                mix(
                    b.color(),
                    Color::Rgb {
                        r: 10,
                        g: 10,
                        b: 14,
                    },
                    0.75,
                )
            };
            surface.fill_rect(
                Rect::new(x, area.top(), w, 1),
                '\u{2591}',
                Style::new().fg(track).bg(bg),
            );
        }
        // The needle: an eased position on the same 0..1 scale, drawn over
        // the segments. It lags the printed total by design -- see the
        // `needle` field docs.
        let nx = area.left()
            + (self.needle.clamp(0.0, 1.0) * f32::from(area.width().saturating_sub(1))) as u16;
        surface.put((nx, area.top()), '\u{25B2}', Style::new().fg(FG).bg(bg));
    }

    fn draw_row(
        &self,
        surface: &mut Surface<'_>,
        inner: Rect,
        y: u16,
        row: &DisplayRow,
        cols: RowCols,
    ) {
        let RowCols { label_w, decay_col } = cols;
        let color = if row.aggregate {
            DIM
        } else if row.value >= 0 {
            rgb(120, 196, 140)
        } else {
            rgb(216, 100, 96)
        };

        let selected = self.selected;
        let highlighted = !row.aggregate
            && self
                .highlight
                .as_ref()
                .is_some_and(|h| h.character == selected && h.label == row.label);
        let bg = if highlighted {
            let strength = self
                .highlight
                .as_ref()
                .map_or(0.0, |h| h.remaining / HIGHLIGHT_SECONDS);
            mix(
                rgb(90, 70, 20),
                panel::PANEL_BG,
                1.0 - strength.clamp(0.0, 1.0),
            )
        } else {
            panel::PANEL_BG
        };
        if highlighted {
            surface.fill_rect(
                Rect::new(inner.left(), y, inner.width(), 1),
                ' ',
                Style::new().bg(bg),
            );
        }

        let value_text = signed4(row.value);
        surface.print(
            (inner.left(), y),
            &value_text,
            Style::new().fg(color).bg(bg),
        );

        let label = fit(&row.label, label_w);
        surface.print(
            (inner.left() + 6, y),
            &label,
            Style::new().fg(if row.aggregate { DIM } else { FG }).bg(bg),
        );

        if decay_col > 0
            && let Some(turns) = row.turns_left
        {
            let text = format!("({turns}t)");
            let x = inner
                .right()
                .saturating_sub(text.chars().count() as u16 + 1);
            // Brightens toward the turn boundary and resets as soon as the
            // count steps down, a continuous decoration riding on top of a
            // number that itself only ever steps -- the animation this
            // demo needs without ever tweening the countdown text itself.
            let decay_color = mix(DIM, WARN, row.turn_frac * 0.6);
            surface.print((x, y), &text, Style::new().fg(decay_color).bg(bg));
        }
    }

    /// Draws the newest lines that fit, oldest at the top, wrapping every
    /// message to `inner`'s actual width first (see [`wrap`]) rather than
    /// clipping it the way `panel::Log::draw`'s bare per-line truncation
    /// would -- this panel is narrow enough in the desktop column layout
    /// that a name-and-value line like `Aldric of Vasgard: Displeased
    /// (-14).` does not fit on one row, and a value sliced off at the
    /// border reads as a bug rather than as "there wasn't room".
    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = Panel::new().title("Negotiations").draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        let mut wrapped: Vec<(String, Color)> = Vec::new();
        for (text, color) in &self.log {
            wrapped.extend(
                wrap(text, inner.width())
                    .into_iter()
                    .map(|line| (line, *color)),
            );
        }

        let rows = usize::from(inner.height());
        let visible = wrapped.iter().rev().take(rows).collect::<Vec<_>>();
        let n = visible.len();
        for (i, (text, color)) in visible.into_iter().rev().enumerate() {
            // Same age curve as `panel::Log::draw`: newest line at full
            // strength, oldest at 45%.
            let age = if n <= 1 {
                0.0
            } else {
                (n - 1 - i) as f32 / (n - 1) as f32
            };
            let faded = mix(*color, panel::PANEL_BG, age * 0.55);
            surface.print(
                (inner.left(), inner.top() + i as u16),
                text,
                Style::new().fg(faded).bg(panel::PANEL_BG),
            );
        }
    }

    fn draw_actions(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Actions").border(Border::Single);
        let inner = panel.draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        // A fixed 2x2 grid: four actions is the whole set. Buttons are
        // plain tinted fills rather than nested bordered panels -- a nested
        // `Panel` costs two rows just for its own frame, which is affordable
        // at desktop height but not at the 80x24 grid every snapshot test
        // runs at, where the whole actions bar may have as few as four rows
        // to work with. The *tap target* still meets `touch::TAP_W`x`TAP_H`
        // regardless of how thin the drawn button is: `push_tappable` grows
        // the hit region independently of what got drawn.
        let cols = panel::columns(inner, 2, 1);
        let gap = u16::from(inner.height() >= 5);
        let row_h = ((inner.height().saturating_sub(gap)) / 2).max(1);

        for (i, action) in ActionKind::ALL.iter().enumerate() {
            let col = &cols[i % 2];
            let row = (i / 2) as u16;
            let y0 = inner.top() + row * (row_h + gap);
            if y0 >= inner.bottom() {
                continue;
            }
            let h = row_h.min(inner.bottom().saturating_sub(y0));
            let rect = Rect::new(col.left(), y0, col.width(), h);
            self.draw_action_button(surface, rect, *action);
            self.hotspots
                .push_tappable(rect, area, Hit::Action(*action));
        }
    }

    fn draw_action_button(&self, surface: &mut Surface<'_>, rect: Rect, action: ActionKind) {
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let blocked = self.action_blocked_reason(action);
        let bg = if blocked.is_some() {
            rgb(38, 30, 26)
        } else {
            rgb(24, 28, 40)
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));

        let label_color = if blocked.is_some() { DIM } else { ACCENT };
        let label = format!("{} [{}]", action.label(), action.key());
        panel::spans(
            surface,
            (rect.left() + 1, rect.top()),
            rect.width().saturating_sub(1),
            &[Span::new(
                &fit(&label, rect.width().saturating_sub(1)),
                label_color,
            )],
            bg,
        );
        if rect.height() > 1 {
            let (text, color) =
                blocked.map_or_else(|| (action.cost_text(), DIM), |reason| (reason, WARN));
            panel::spans(
                surface,
                (rect.left() + 1, rect.top() + 1),
                rect.width().saturating_sub(1),
                &[Span::new(text, color)],
                bg,
            );
        }
    }

    /// Why `action` would currently be refused for the selected character,
    /// or `None` if it would go through. Shared between the button's own
    /// warning line and [`Self::apply_action`], so the reason a button
    /// warns about is always the exact reason a tap on it would fail.
    fn action_blocked_reason(&self, action: ActionKind) -> Option<&'static str> {
        let ch = &self.characters[self.selected];
        let band = Band::of(ch.total());
        match action {
            ActionKind::Gift if self.gold < GIFT_COST_GOLD => Some("not enough gold"),
            ActionKind::Betrothal if band < Band::Indifferent => Some("opinion too low"),
            ActionKind::Betrothal if self.prestige < BETROTHAL_COST_PRESTIGE => {
                Some("not enough prestige")
            }
            ActionKind::Claim if self.prestige < CLAIM_COST_PRESTIGE => Some("not enough prestige"),
            ActionKind::Truce if band == Band::Furious => Some("too hostile for a truce"),
            ActionKind::Truce if !ch.modifiers.iter().any(|m| m.label == "Recent war") => {
                Some("no war to end")
            }
            ActionKind::Truce if self.gold < TRUCE_COST_GOLD => Some("not enough gold"),
            _ => None,
        }
    }
}

impl Demo for OpenTerms {
    const NAME: &'static str = "56_open_terms";
    const TITLE: &'static str = "Open Terms";
    const BLURB: &'static str =
        "Crusader Kings: an opinion total itemised into signed, actionable modifiers.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab/[ ]", "switch character"),
            ("1-9", "inspect a row"),
            ("G/B/C/T", "gift/betrothal/claim/truce"),
            ("Esc", "clear inspection"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        self.handle_tap();
        self.simulate(dt);
        self.draw(term);
        true
    }
}

ascii_tile_demos::demo_main!(OpenTerms);

#[cfg(test)]
mod tests {
    use super::{Band, Modifier, build_characters, signed4, upsert, visible_rows};
    use std::collections::HashSet;

    #[test]
    fn every_character_name_is_unique() {
        let characters = build_characters();
        let names: HashSet<&str> = characters.iter().map(|c| c.name).collect();
        assert_eq!(
            names.len(),
            characters.len(),
            "duplicate character name in the roster"
        );
    }

    #[test]
    fn displayed_total_equals_the_sum_of_displayed_rows_at_every_capacity() {
        let characters = build_characters();
        for ch in &characters {
            let total = ch.total();
            for capacity in 0..=ch.modifiers.len() + 2 {
                let rows = visible_rows(&ch.modifiers, 0.0, capacity);
                let shown: i32 = rows.iter().map(|r| r.value).sum();
                if capacity == 0 {
                    assert!(rows.is_empty());
                    continue;
                }
                assert_eq!(
                    shown, total,
                    "{} at capacity {capacity}: rows summed to {shown}, total is {total}",
                    ch.name
                );
            }
        }
    }

    #[test]
    fn the_worked_example_from_the_brief_matches_exactly() {
        let characters = build_characters();
        let aldric = &characters[0];
        assert_eq!(aldric.total(), -14);
        assert_eq!(Band::of(aldric.total()), Band::Displeased);
    }

    #[test]
    fn bands_cover_the_full_scale_and_step_at_their_thresholds() {
        assert_eq!(Band::of(-100), Band::Furious);
        assert_eq!(Band::of(-40), Band::Furious);
        assert_eq!(Band::of(-39), Band::Displeased);
        assert_eq!(Band::of(-10), Band::Displeased);
        assert_eq!(Band::of(-9), Band::Indifferent);
        assert_eq!(Band::of(9), Band::Indifferent);
        assert_eq!(Band::of(10), Band::Cordial);
        assert_eq!(Band::of(39), Band::Cordial);
        assert_eq!(Band::of(40), Band::Loyal);
        assert_eq!(Band::of(100), Band::Loyal);
    }

    #[test]
    fn signed4_always_shows_a_sign() {
        assert_eq!(signed4(15), " +15");
        assert_eq!(signed4(-5), "  -5");
        assert_eq!(signed4(0), "  +0");
    }

    #[test]
    fn upsert_refreshes_rather_than_duplicates() {
        let mut modifiers = vec![Modifier::decaying("Recent gift", 10, 5.0, "d")];
        upsert(
            &mut modifiers,
            Modifier::decaying("Recent gift", 10, 20.0, "d"),
        );
        assert_eq!(modifiers.len(), 1);
        assert!((modifiers[0].remaining.unwrap() - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn every_roster_character_has_at_least_one_modifier() {
        for ch in build_characters() {
            assert!(!ch.modifiers.is_empty(), "{} has no modifiers", ch.name);
        }
    }
}
