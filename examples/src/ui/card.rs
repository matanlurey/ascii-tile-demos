//! Multi-cell cards: the smallest unit of interface that is worth tapping.
//!
//! A card is the clearest case of the constraint [`touch`](super::touch)
//! describes. One glyph is not a card, and not because it looks poor: a card
//! has to carry a cost, a name, and a rule, and it has to be big enough for a
//! finger. [`FULL_W`]x[`FULL_H`] is what that costs in cells, and it is the
//! reason the demos from 27 on are drawn at interface scale rather than at one
//! glyph per unit.
//!
//! ## Tiers, not scaling
//!
//! Text does not scale. A card cannot be drawn at 70% and stay readable, so
//! shrinking is a matter of *dropping* things in a fixed order rather than
//! resizing them. [`Tier`] names the three sizes worth supporting, and the
//! order things are dropped is chosen so what remains is still enough to play
//! with:
//!
//! ```text
//! Full (11x9)        Compact (9x5)     Stub (5x3)
//! ┌─────────┐        ┌───────┐         ┌───┐
//! │(2) Strike        │(2)Strik         │(2)│
//! ├─────────┤        │Deal 6 │         └───┘
//! │  Attack │        │       │
//! ├─────────┤        └───────┘
//! │ Deal 6  │
//! │ damage  │
//! └─────────┘
//! ```
//!
//! The cost survives to the last tier, and the art is the first thing cut.
//! That ordering is not arbitrary: a hand of cards is scanned for what can be
//! *afforded* before it is read for what it does, so a stub that shows only a
//! cost still supports the decision the player is actually making. A stub that
//! showed only a name would not.
//!
//! ## Why cards overlap
//!
//! [`fan`] deliberately overlaps cards once the hand outgrows the row rather
//! than shrinking them below a readable tier. This is what every physical card
//! game does and what Slay the Spire does on a phone, and the reason is the
//! same in all three: an overlapped card still shows its left edge, and the
//! left edge is where the cost is. Ten cards shrunk to fit are ten cards that
//! cannot be read; ten cards fanned are eight costs plus two readable cards.

use retroglyph_core::{Color, Rect, Style, Surface};
use retroglyph_widgets::truncate;
use tilekit::palette::{mix, rgb, scale};

use super::panel::{self, Border, Panel};
use super::{ACCENT, DIM, FG};

/// Width of a card drawn at [`Tier::Full`].
///
/// Eleven rather than the nine [`touch::TAP_W`](super::touch::TAP_W) demands,
/// because nine columns of interior minus two of border leaves seven for text,
/// and seven columns cannot hold a two-word rule without hyphenating. Eleven
/// leaves nine, which fits "Deal 6 dmg" exactly.
pub const FULL_W: u16 = 11;

/// Height of a card drawn at [`Tier::Full`]: two border rows, a title, a type
/// line, two divider rows, and three rows of rule text.
pub const FULL_H: u16 = 9;

/// Width of a card drawn at [`Tier::Compact`], which is
/// [`touch::TAP_W`](super::touch::TAP_W) exactly. Below this a card is no
/// longer tappable on a phone, whatever it still manages to display.
pub const COMPACT_W: u16 = 9;

/// Height of a card drawn at [`Tier::Compact`].
pub const COMPACT_H: u16 = 5;

/// How much of a card must stay visible when a hand is fanned.
///
/// Four columns: a border, a two-character cost, and one column of the name.
/// Anything less and the overlap has eaten the one thing a covered card still
/// needs to say.
pub const FAN_MIN: u16 = 4;

/// Which layout a card was drawn at.
///
/// Returned by [`Card::draw`] so a caller can tell whether the rule text made
/// it onto the screen, and put it somewhere else (a detail panel) if not.
/// Touch has no hover, so a card whose text was dropped has nowhere else to
/// explain itself unless the demo arranges one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Tier {
    /// Too small to draw anything at all.
    None,
    /// Cost only, in a frame. What a fanned card behind another shows.
    Stub,
    /// Cost, name, and one line of rule text.
    Compact,
    /// Cost, name, type line, and wrapped rule text.
    Full,
}

impl Tier {
    /// The largest tier that fits `rect`.
    #[must_use]
    pub const fn for_rect(rect: Rect) -> Self {
        if rect.width() >= FULL_W && rect.height() >= FULL_H {
            Self::Full
        } else if rect.width() >= COMPACT_W && rect.height() >= COMPACT_H {
            Self::Compact
        } else if rect.width() >= FAN_MIN && rect.height() >= 3 {
            Self::Stub
        } else {
            Self::None
        }
    }
}

/// How a card is currently being interacted with.
///
/// Four states rather than a bool because each has to be told apart *without
/// hover*, which is the state touch does not have. Selection has to be visible
/// from the card itself, so it is carried by border weight and brightness
/// rather than by a highlight that only appears when a pointer is over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CardState {
    /// In hand, playable, not selected.
    #[default]
    Idle,
    /// Chosen and awaiting a target. Drawn with a heavier border, which is the
    /// one channel that survives both a colorblind viewer and a grayscale
    /// screenshot.
    Selected,
    /// Currently under the finger. Drawn brighter still, and usually lifted a
    /// row by the caller so it is not entirely covered by the hand it came
    /// from.
    Held,
    /// Not playable right now, e.g. unaffordable. Dimmed toward the page
    /// background rather than grayed, so it reads as receding rather than as a
    /// second palette.
    Disabled,
}

impl CardState {
    /// The frame weight this state draws with.
    const fn border(self) -> Border {
        match self {
            Self::Selected | Self::Held => Border::Double,
            Self::Idle | Self::Disabled => Border::Single,
        }
    }

    /// Adjusts a card's accent for this state.
    fn tint(self, accent: Color) -> Color {
        match self {
            Self::Idle => accent,
            Self::Selected => mix(accent, rgb(255, 255, 255), 0.35),
            Self::Held => mix(accent, rgb(255, 255, 255), 0.6),
            Self::Disabled => scale(accent, 0.4),
        }
    }
}

/// A card: a cost, a name, a type line, and a rule.
///
/// Borrowed rather than owned so a demo can build one per frame from state it
/// already has, which is what keeps the hand a plain `Vec<CardId>` rather than
/// a parallel tree of widgets that has to be kept in sync with the game.
#[derive(Clone, Copy)]
pub struct Card<'a> {
    cost: Option<&'a str>,
    title: &'a str,
    kind: Option<&'a str>,
    body: &'a str,
    accent: Color,
    state: CardState,
}

impl<'a> Card<'a> {
    /// A card with only a name.
    #[must_use]
    pub const fn new(title: &'a str) -> Self {
        Self {
            cost: None,
            title,
            kind: None,
            body: "",
            accent: ACCENT,
            state: CardState::Idle,
        }
    }

    /// Sets the cost badge, drawn top-left and kept at every tier.
    #[must_use]
    pub const fn cost(mut self, cost: &'a str) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Sets the type line ("Attack", "Terrain", "Daemon"), shown only at
    /// [`Tier::Full`].
    #[must_use]
    pub const fn kind(mut self, kind: &'a str) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Sets the rule text, wrapped to the card's interior.
    #[must_use]
    pub const fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }

    /// Sets the accent color, which carries the card's faction or school.
    #[must_use]
    pub const fn accent(mut self, accent: Color) -> Self {
        self.accent = accent;
        self
    }

    /// Sets the interaction state.
    #[must_use]
    pub const fn state(mut self, state: CardState) -> Self {
        self.state = state;
        self
    }

    /// Draws the card into `rect` at the largest tier that fits, returning
    /// which tier that was.
    pub fn draw(&self, surface: &mut Surface<'_>, rect: Rect) -> Tier {
        let tier = Tier::for_rect(rect);
        let accent = self.state.tint(self.accent);
        let bg = if self.state == CardState::Disabled {
            scale(panel::PANEL_BG, 0.7)
        } else {
            panel::PANEL_BG
        };

        match tier {
            Tier::None => return tier,
            Tier::Stub => self.draw_stub(surface, rect, accent, bg),
            Tier::Compact => self.draw_compact(surface, rect, accent, bg),
            Tier::Full => self.draw_full(surface, rect, accent, bg),
        }
        tier
    }

    /// The frame every tier shares, returning the interior.
    fn frame(&self, surface: &mut Surface<'_>, rect: Rect, accent: Color, bg: Color) -> Rect {
        Panel::new()
            .border(self.state.border())
            .frame(accent)
            .bg(bg)
            .draw(surface, rect)
    }

    /// Cost only. What a card covered by the one in front of it still shows.
    fn draw_stub(&self, surface: &mut Surface<'_>, rect: Rect, accent: Color, bg: Color) {
        let inner = self.frame(surface, rect, accent, bg);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let text = self.cost.unwrap_or(self.title);
        surface.print(
            (inner.left(), inner.top()),
            truncate(text, inner.width_usize()),
            Style::new().fg(accent).bg(bg),
        );
    }

    /// Cost and name on one row, then as much rule text as the rest allows.
    fn draw_compact(&self, surface: &mut Surface<'_>, rect: Rect, accent: Color, bg: Color) {
        let inner = self.frame(surface, rect, accent, bg);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        let mut spans = Vec::new();
        if let Some(cost) = self.cost {
            spans.push(panel::Span::new(cost, accent));
            spans.push(panel::Span::plain(" "));
        }
        spans.push(panel::Span::new(self.title, self.title_color()));
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &spans,
            bg,
        );

        let rows = inner.height().saturating_sub(1);
        for (i, line) in wrap(self.body, inner.width_usize())
            .into_iter()
            .take(usize::from(rows))
            .enumerate()
        {
            surface.print(
                (inner.left(), inner.top() + 1 + i as u16),
                &line,
                Style::new().fg(self.body_color()).bg(bg),
            );
        }
    }

    /// Cost, centered name, type line, a rule, each separated by a divider.
    fn draw_full(&self, surface: &mut Surface<'_>, rect: Rect, accent: Color, bg: Color) {
        let inner = self.frame(surface, rect, accent, bg);
        if inner.width() == 0 || inner.height() < 3 {
            return;
        }
        let w = inner.width();

        // Cost sits in the top border rather than the interior: it is the one
        // field that must survive being overlapped, and the border row is the
        // only row a card in front cannot cover without covering the frame.
        if let Some(cost) = self.cost {
            let text = format!("({cost})");
            surface.print(
                (inner.left(), rect.top()),
                truncate(&text, inner.width_usize()),
                Style::new().fg(accent).bg(bg),
            );
        }

        let mut y = inner.top();
        print_centered(
            surface,
            inner.left(),
            y,
            w,
            self.title,
            self.title_color(),
            bg,
        );
        y += 1;

        if let Some(kind) = self.kind
            && y < inner.bottom()
        {
            print_centered(surface, inner.left(), y, w, kind, DIM, bg);
            y += 1;
        }

        if y < inner.bottom() {
            surface.fill_rect(
                Rect::new(inner.left(), y, w, 1),
                '\u{2500}',
                Style::new().fg(scale(accent, 0.6)).bg(bg),
            );
            y += 1;
        }

        for line in wrap(self.body, inner.width_usize()) {
            if y >= inner.bottom() {
                break;
            }
            surface.print(
                (inner.left(), y),
                &line,
                Style::new().fg(self.body_color()).bg(bg),
            );
            y += 1;
        }
    }

    const fn title_color(&self) -> Color {
        match self.state {
            CardState::Disabled => DIM,
            _ => FG,
        }
    }

    const fn body_color(&self) -> Color {
        match self.state {
            CardState::Disabled => scale_const(DIM),
            _ => DIM,
        }
    }
}

/// A compile-time dim, for the `const fn` above.
///
/// [`scale`](tilekit::palette::scale) is not `const` (it does float math on
/// the channels), and a disabled card's body color is the one place that is
/// wanted in a `const fn`. Halving the channels by hand is equivalent at the
/// only input it is ever given.
const fn scale_const(color: Color) -> Color {
    match color {
        Color::Rgb { r, g, b } => Color::Rgb {
            r: r / 2,
            g: g / 2,
            b: b / 2,
        },
        other => other,
    }
}

/// Prints `text` centered in `width` cells starting at `x`.
fn print_centered(
    surface: &mut Surface<'_>,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    color: Color,
    bg: Color,
) {
    let text = truncate(text, usize::from(width));
    let pad = (width - text.chars().count() as u16) / 2;
    surface.print((x + pad, y), text, Style::new().fg(color).bg(bg));
}

/// Greedily wraps `text` to `width` columns.
///
/// Word-wrapping rather than hard truncation because a card's rule is the part
/// a player reads to decide, and "Deal 6 damage to all" cut to "Deal 6 dam" is
/// a different card. A word longer than the line is broken rather than dropped,
/// since a card whose only word does not fit should still show what it can.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let line_len = line.chars().count();

        if line_len == 0 {
            if word_len <= width {
                line.push_str(word);
            } else {
                // Longer than a whole line: break it across as many as needed.
                let mut rest = word;
                while rest.chars().count() > width {
                    let head: String = rest.chars().take(width).collect();
                    lines.push(head);
                    rest = &rest[rest
                        .char_indices()
                        .nth(width)
                        .map_or(rest.len(), |(i, _)| i)..];
                }
                line.push_str(rest);
            }
        } else if line_len + 1 + word_len <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(core::mem::take(&mut line));
            line.push_str(truncate(word, width));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Lays out `count` cards across `area`, overlapping them if they do not fit.
///
/// Returns one rect per card, left to right. While the hand fits, cards are
/// `card_w` wide with one column between them. Once it does not, they overlap
/// by however much is needed, down to [`FAN_MIN`] columns of each visible; past
/// that the hand genuinely cannot be shown and the extra cards are given
/// zero-width rects rather than being stacked invisibly on the last one.
///
/// Draw the returned rects in order and the *last* card ends up on top, which
/// is why a selected card should be drawn last and registered last with
/// [`Hotspots`](super::touch::Hotspots): both resolve overlap the same way, so
/// what is visible is what is tappable.
#[must_use]
pub fn fan(area: Rect, count: usize, card_w: u16) -> Vec<Rect> {
    if count == 0 || area.width() == 0 || card_w == 0 {
        return Vec::new();
    }
    let n = count as u16;
    let card_w = card_w.min(area.width());

    // Step: how far apart consecutive cards start. Ideally the card plus a
    // gap; at minimum FAN_MIN, which keeps each cost visible.
    let ideal = card_w + 1;
    let needed = u32::from(ideal) * u32::from(n.saturating_sub(1)) + u32::from(card_w);
    let step = if needed <= u32::from(area.width()) {
        ideal
    } else if n > 1 {
        let available = area.width().saturating_sub(card_w);
        (available / (n - 1)).max(FAN_MIN)
    } else {
        ideal
    };

    // Center the run when it is narrower than the area, so a two-card hand
    // sits under the thumb rather than jammed against the left edge.
    let span = step * (n - 1) + card_w;
    let x0 = if span < area.width() {
        area.left() + (area.width() - span) / 2
    } else {
        area.left()
    };

    (0..n)
        .map(|i| {
            let x = x0 + step * i;
            if x >= area.right() {
                return Rect::new(area.right(), area.top(), 0, area.height());
            }
            let w = card_w.min(area.right() - x);
            Rect::new(x, area.top(), w, area.height())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{COMPACT_H, COMPACT_W, Card, CardState, FAN_MIN, FULL_H, FULL_W, Tier, fan, wrap};
    use retroglyph_core::{Grid, Rect, Surface};

    #[test]
    fn tiers_step_down_with_the_space_available() {
        assert_eq!(Tier::for_rect(Rect::new(0, 0, FULL_W, FULL_H)), Tier::Full);
        assert_eq!(
            Tier::for_rect(Rect::new(0, 0, COMPACT_W, COMPACT_H)),
            Tier::Compact
        );
        assert_eq!(Tier::for_rect(Rect::new(0, 0, FAN_MIN, 3)), Tier::Stub);
        assert_eq!(Tier::for_rect(Rect::new(0, 0, 2, 2)), Tier::None);
    }

    #[test]
    fn a_card_reports_the_tier_it_actually_drew() {
        let mut grid = Grid::new(40, 20);
        let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 40, 20), 0);
        let card = Card::new("Strike").cost("2").body("Deal 6 damage.");
        assert_eq!(
            card.draw(&mut surface, Rect::new(0, 0, FULL_W, FULL_H)),
            Tier::Full
        );
        assert_eq!(
            card.draw(&mut surface, Rect::new(0, 0, COMPACT_W, COMPACT_H)),
            Tier::Compact
        );
    }

    #[test]
    fn a_card_too_small_to_draw_reports_none_and_does_not_panic() {
        let mut grid = Grid::new(8, 8);
        let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 8, 8), 0);
        let card = Card::new("Strike").cost("2");
        assert_eq!(card.draw(&mut surface, Rect::new(0, 0, 1, 1)), Tier::None);
        assert_eq!(card.draw(&mut surface, Rect::new(0, 0, 0, 0)), Tier::None);
    }

    #[test]
    fn every_card_state_draws_without_panicking() {
        let mut grid = Grid::new(20, 12);
        let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 20, 12), 0);
        for state in [
            CardState::Idle,
            CardState::Selected,
            CardState::Held,
            CardState::Disabled,
        ] {
            let card = Card::new("Zap").cost("1").kind("Skill").state(state);
            assert_ne!(
                card.draw(&mut surface, Rect::new(0, 0, FULL_W, FULL_H)),
                Tier::None,
                "{state:?}"
            );
        }
    }

    #[test]
    fn wrapping_breaks_on_words() {
        assert_eq!(
            wrap("Deal 6 damage to all enemies.", 9),
            vec!["Deal 6", "damage to", "all", "enemies."]
        );
    }

    #[test]
    fn wrapping_breaks_a_word_longer_than_the_line() {
        // Dropping it would lose the only content the card has.
        let lines = wrap("Supercalifragilistic", 8);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| l.chars().count() <= 8));
        assert!(lines.concat().starts_with("Supercali"));
    }

    #[test]
    fn wrapping_a_zero_width_line_produces_nothing() {
        assert!(wrap("anything", 0).is_empty());
        assert!(wrap("", 10).is_empty());
    }

    #[test]
    fn a_hand_that_fits_is_laid_out_without_overlap() {
        let rects = fan(Rect::new(0, 0, 80, 9), 5, FULL_W);
        for pair in rects.windows(2) {
            assert!(
                pair[0].right() <= pair[1].left(),
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
        assert!(rects.iter().all(|r| r.width() == FULL_W));
    }

    #[test]
    fn a_hand_too_wide_to_fit_overlaps_instead_of_shrinking() {
        let rects = fan(Rect::new(0, 0, 40, 9), 8, FULL_W);
        assert_eq!(rects.len(), 8);
        // Cards keep their width (so the top one stays readable) and simply
        // start closer together.
        assert!(rects[0].width() >= FAN_MIN);
        let step = rects[1].left() - rects[0].left();
        assert!(step >= FAN_MIN, "step {step} hid a card's cost");
        assert!(step < FULL_W, "the hand should have overlapped");
    }

    #[test]
    fn every_fanned_card_stays_inside_the_area() {
        let area = Rect::new(3, 2, 40, 9);
        for count in 1..20usize {
            for rect in fan(area, count, FULL_W) {
                assert!(rect.left() >= area.left(), "{rect:?} left of {area:?}");
                assert!(rect.right() <= area.right(), "{rect:?} right of {area:?}");
            }
        }
    }

    #[test]
    fn a_short_hand_is_centered_under_the_thumb() {
        let rects = fan(Rect::new(0, 0, 80, 9), 2, FULL_W);
        let span = rects[1].right() - rects[0].left();
        let left_margin = rects[0].left();
        let right_margin = 80 - rects[1].right();
        assert!(
            left_margin.abs_diff(right_margin) <= 1,
            "span {span} was not centered: {left_margin} vs {right_margin}"
        );
    }

    #[test]
    fn an_empty_hand_lays_out_nothing() {
        assert!(fan(Rect::new(0, 0, 40, 9), 0, FULL_W).is_empty());
    }
}
