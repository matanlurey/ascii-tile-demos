//! Framed panels, gauges, and logs: the widget vocabulary the interface-heavy
//! demos share.
//!
//! Everything in [`ui`](crate::ui) above this module is chrome the *gallery*
//! imposes on every demo. Everything here is chrome a *demo* opts into, and it
//! exists because the alternative was nine demos each inventing its own idea of
//! what a bordered box looks like. A gallery whose panels disagree about corner
//! glyphs reads as nine student projects rather than one survey.
//!
//! ## The colorable-glyph constraint
//!
//! Every glyph used here is CP437, and that is load-bearing rather than
//! nostalgic. `retroglyph`'s pixel backends resolve glyphs through a CP437
//! table and draw a solid block for anything else, and the tileset escape
//! hatch produces sprites that ignore the cell's foreground
//! (retroglyph#537/#539). A red health bar therefore cannot be built from the
//! eighth blocks `▏▎▍▌▋▊▉` that a modern terminal UI would reach for: six of
//! those eight are outside CP437, so the bar would render as a white slab at
//! every value.
//!
//! CP437 does have `█` and `▌`, which is exactly half-cell precision, so
//! [`bar`] is built on those. It is a real loss of resolution against the
//! eighth-block bars in Cogmind, and it is worth understanding why the trade
//! goes this way: a bar that is accurate to half a cell *in the right color*
//! communicates far more than one accurate to an eighth of a cell that is
//! always white, because the color is carrying the threshold (green/amber/red)
//! and the fill is only carrying the magnitude. `examples/tests/glyphs.rs`
//! pins both properties.

use retroglyph_core::{Color, Rect, Style, Surface};
use retroglyph_widgets::truncate;
use tilekit::autotile::{BOX_DOUBLE, BOX_SINGLE, E, N, S, W};
use tilekit::palette::{mix, scale};

use super::{ACCENT, CHROME_BG, DIM, FG};

/// Panel frame background: a touch lighter than the page so a panel reads as
/// a raised surface rather than a hole.
pub const PANEL_BG: Color = Color::Rgb {
    r: 16,
    g: 18,
    b: 26,
};

/// Default frame color: dim enough to be structure, not content.
pub const FRAME: Color = Color::Rgb {
    r: 72,
    g: 78,
    b: 100,
};

/// Which box-drawing set a frame is built from.
///
/// Two weights rather than one because a nested panel needs to read as
/// subordinate to its container, and the cheapest way to say so is line
/// weight. `Dungeons of Everchange` uses doubles throughout for a heavy
/// terminal look; Cogmind uses singles for a quieter one; both are here so a
/// demo can pick the register it wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Border {
    /// `┌─┐│└┘`. The quieter weight, for panels inside panels.
    #[default]
    Single,
    /// `╔═╗║╚╝`. The heavier weight, for top-level panels.
    Double,
}

impl Border {
    /// The glyph for a cardinal connection mask, per
    /// [`tilekit::autotile::mask4`].
    const fn glyph(self, mask: u8) -> char {
        match self {
            Self::Single => BOX_SINGLE[(mask & 0x0F) as usize],
            Self::Double => BOX_DOUBLE[(mask & 0x0F) as usize],
        }
    }

    /// The horizontal run used to pad a title.
    const fn dash(self) -> char {
        self.glyph(E | W)
    }
}

/// A bordered box with an optional title and an optional right-aligned badge.
///
/// Built rather than drawn directly so a caller can set only what it cares
/// about. [`draw`](Self::draw) returns the *interior* rect, which is the part
/// callers actually want and the part that is fiddliest to get right: a panel
/// one cell too tall silently clips its last row, and that is much easier to
/// see as a returned rectangle than as an off-by-one in every call site.
#[derive(Clone, Copy)]
pub struct Panel<'a> {
    title: Option<&'a str>,
    badge: Option<&'a str>,
    border: Border,
    frame: Color,
    title_color: Color,
    bg: Color,
    focused: bool,
}

impl Default for Panel<'_> {
    fn default() -> Self {
        Self {
            title: None,
            badge: None,
            border: Border::Single,
            frame: FRAME,
            title_color: FG,
            bg: PANEL_BG,
            focused: false,
        }
    }
}

impl<'a> Panel<'a> {
    /// An untitled single-line panel in the default colors.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the title, drawn into the top border.
    #[must_use]
    pub const fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// Sets a short right-aligned badge in the top border: a count, a hotkey,
    /// or a collapse affordance like `[-]`.
    #[must_use]
    pub const fn badge(mut self, badge: &'a str) -> Self {
        self.badge = Some(badge);
        self
    }

    /// Sets the line weight.
    #[must_use]
    pub const fn border(mut self, border: Border) -> Self {
        self.border = border;
        self
    }

    /// Sets the frame color. The title follows it unless
    /// [`title_color`](Self::title_color) overrides.
    #[must_use]
    pub const fn frame(mut self, frame: Color) -> Self {
        self.frame = frame;
        self.title_color = frame;
        self
    }

    /// Sets the title color independently of the frame.
    #[must_use]
    pub const fn title_color(mut self, color: Color) -> Self {
        self.title_color = color;
        self
    }

    /// Sets the interior background.
    #[must_use]
    pub const fn bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    /// Marks the panel as focused, brightening its frame and title.
    ///
    /// Brightness rather than a different border weight, because weight
    /// already means nesting depth here and one channel cannot carry two
    /// meanings. This is the convention Cogmind uses: dim everything that is
    /// not the active window rather than decorating the one that is.
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Draws the panel into `area` and returns the interior rect.
    ///
    /// Degrades rather than panicking on a rect too small to frame: under 2
    /// cells in either axis there is no room for two borders plus content, so
    /// it fills the area and reports an empty interior. Callers are laying out
    /// responsively and will hand this a squeezed rect at some window size.
    pub fn draw(&self, surface: &mut Surface<'_>, area: Rect) -> Rect {
        let (frame, title_color) = if self.focused {
            (brighten(self.frame), brighten(self.title_color))
        } else {
            (self.frame, self.title_color)
        };

        if area.width() < 2 || area.height() < 2 {
            surface.fill_rect(area, ' ', Style::new().bg(self.bg));
            return Rect::new(area.left(), area.top(), 0, 0);
        }

        surface.fill_rect(area, ' ', Style::new().bg(self.bg));
        let style = Style::new().fg(frame).bg(self.bg);
        let (l, t) = (area.left(), area.top());
        let (r, b) = (area.right() - 1, area.bottom() - 1);

        surface.put((l, t), self.border.glyph(E | S), style);
        surface.put((r, t), self.border.glyph(S | W), style);
        surface.put((l, b), self.border.glyph(N | E), style);
        surface.put((r, b), self.border.glyph(N | W), style);
        for x in (l + 1)..r {
            surface.put((x, t), self.border.dash(), style);
            surface.put((x, b), self.border.dash(), style);
        }
        for y in (t + 1)..b {
            surface.put((l, y), self.border.glyph(N | S), style);
            surface.put((r, y), self.border.glyph(N | S), style);
        }

        let inner_w = area.width() - 2;
        self.draw_header(surface, area, inner_w, title_color, frame);

        Rect::new(l + 1, t + 1, inner_w, area.height() - 2)
    }

    /// Writes the title and badge into the already-drawn top border.
    ///
    /// The badge is placed first and the title truncated against what is left,
    /// so a long title crowds out its own text rather than overwriting a count
    /// the caller considered important enough to pin to the corner.
    fn draw_header(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        inner_w: u16,
        title_color: Color,
        frame: Color,
    ) {
        let mut right = area.right() - 1;
        let mut room = usize::from(inner_w);

        if let Some(badge) = self.badge
            && room > badge.chars().count() + 2
        {
            let text = format!(" {badge} ");
            let n = text.chars().count();
            right -= n as u16;
            surface.print(
                (right, area.top()),
                &text,
                Style::new().fg(frame).bg(self.bg),
            );
            room -= n;
        }

        if let Some(title) = self.title
            && room > 4
        {
            let text = format!(" {} ", truncate(title, room.saturating_sub(2)));
            surface.print(
                (area.left() + 1, area.top()),
                &text,
                Style::new().fg(title_color).bg(self.bg),
            );
        }
    }
}

/// Lightens a frame color for the focused state.
fn brighten(color: Color) -> Color {
    mix(
        color,
        Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        0.45,
    )
}

/// Draws a horizontal gauge filling `width` cells with `t` in `0.0..=1.0`.
///
/// Half-cell precision, using `█` for a full cell and `▌` for a half. See the
/// module docs for why not eighths: the eighth blocks are outside CP437 and
/// would render as a colorless slab.
///
/// `track` is drawn under the unfilled remainder rather than left blank, so
/// the gauge's full extent stays legible at zero; a bar that vanishes when
/// empty is indistinguishable from a bar that is missing.
pub fn bar(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    t: f32,
    fill: Color,
    track: Color,
) {
    let (x0, y) = at;
    let t = t.clamp(0.0, 1.0);
    // Halves, not cells: the unit of precision this can actually draw.
    let halves = (f32::from(width) * 2.0 * t).round() as u16;

    for i in 0..width {
        let filled = halves.saturating_sub(i * 2).min(2);
        let (glyph, fg) = match filled {
            2 => ('\u{2588}', fill),
            1 => ('\u{258C}', fill),
            _ => (' ', fill),
        };
        surface.put((x0 + i, y), glyph, Style::new().fg(fg).bg(track));
    }
}

/// Threshold colors for a gauge, the convention every game in the references
/// uses: green while healthy, amber while worth watching, red while urgent.
///
/// A function rather than three constants because the *thresholds* are the
/// shared decision, not the colors. Demos that pick their own cutoffs end up
/// disagreeing about what "low" means from panel to panel.
#[must_use]
pub fn threshold(t: f32) -> Color {
    if t > 0.6 {
        Color::Rgb {
            r: 108,
            g: 196,
            b: 108,
        }
    } else if t > 0.3 {
        Color::Rgb {
            r: 226,
            g: 184,
            b: 90,
        }
    } else {
        Color::Rgb {
            r: 216,
            g: 88,
            b: 84,
        }
    }
}

/// A run of text with its own color, for a line assembled from several.
#[derive(Clone, Copy)]
pub struct Span<'a> {
    /// The text.
    pub text: &'a str,
    /// Its color.
    pub color: Color,
}

impl<'a> Span<'a> {
    /// A span in an explicit color.
    #[must_use]
    pub const fn new(text: &'a str, color: Color) -> Self {
        Self { text, color }
    }

    /// A span in the default body color.
    #[must_use]
    pub const fn plain(text: &'a str) -> Self {
        Self::new(text, FG)
    }

    /// A span in the accent color, for names and numbers worth finding.
    #[must_use]
    pub const fn keyword(text: &'a str) -> Self {
        Self::new(text, ACCENT)
    }

    /// A span in the dim color, for connective prose.
    #[must_use]
    pub const fn dim(text: &'a str) -> Self {
        Self::new(text, DIM)
    }
}

/// Draws `spans` end to end on one row, clipped to `width`.
///
/// Returns the number of cells written, so a caller can right-align or append.
/// Truncates the span that crosses the boundary rather than dropping it, since
/// a half-written item name still tells the reader what happened.
pub fn spans(
    surface: &mut Surface<'_>,
    at: (u16, u16),
    width: u16,
    spans: &[Span<'_>],
    bg: Color,
) -> u16 {
    let (x0, y) = at;
    let mut used = 0usize;
    let room = usize::from(width);

    for span in spans {
        if used >= room {
            break;
        }
        let text = truncate(span.text, room - used);
        if text.is_empty() {
            continue;
        }
        surface.print(
            (x0 + used as u16, y),
            text,
            Style::new().fg(span.color).bg(bg),
        );
        used += text.chars().count();
    }
    used as u16
}

/// A scrolling message log with per-line color and age fade.
///
/// Fixed-capacity and oldest-first, so drawing is a tail slice rather than a
/// scroll offset. The fade is the part worth having: every reference game
/// dims older lines, and it is what stops a log from reading as a wall of
/// equally urgent text. Cogmind's rule is the one modelled here, that recency
/// is the only ranking a log can offer without understanding its own contents.
#[derive(Default)]
pub struct Log {
    lines: Vec<(String, Color)>,
    capacity: usize,
}

impl Log {
    /// A log holding at most `capacity` lines.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Appends a line, evicting the oldest if at capacity.
    pub fn push(&mut self, text: impl Into<String>, color: Color) {
        self.lines.push((text.into(), color));
        if self.lines.len() > self.capacity {
            self.lines.remove(0);
        }
    }

    /// The number of lines held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Draws the newest lines that fit `area`, oldest at the top.
    ///
    /// Fades toward `bg` with age rather than toward gray, so the effect reads
    /// as receding into the panel instead of as a second, muddier palette.
    pub fn draw(&self, surface: &mut Surface<'_>, area: Rect, bg: Color) {
        if area.height() == 0 || area.width() == 0 {
            return;
        }
        let rows = usize::from(area.height());
        let visible = self.lines.iter().rev().take(rows).collect::<Vec<_>>();
        let n = visible.len();

        for (i, (text, color)) in visible.into_iter().rev().enumerate() {
            // Newest line at full strength, oldest at 45%.
            let age = if n <= 1 {
                0.0
            } else {
                (n - 1 - i) as f32 / (n - 1) as f32
            };
            let faded = mix(*color, bg, age * 0.55);
            surface.print(
                (area.left(), area.top() + i as u16),
                truncate(text, area.width_usize()),
                Style::new().fg(faded).bg(bg),
            );
        }
    }
}

/// Draws spreadsheet-style coordinate rulers around `area`.
///
/// `cols` labels run along the top and bottom, `rows` down both sides, which
/// is how Remnant Humanity labels its deck plan and how a wargame labels its
/// hex field. Repeating them on all four edges is not redundancy: on a wide
/// map the eye is usually nearer the far edge than the near one, and the whole
/// point of a ruler is to be readable without traversing the map to reach it.
///
/// `step` labels every `step`-th cell, so a dense grid can be labelled every
/// fifth column without the labels colliding.
pub fn rulers(
    surface: &mut Surface<'_>,
    area: Rect,
    cols: &[char],
    rows: &[char],
    step: u16,
    color: Color,
    bg: Color,
) {
    let step = step.max(1);
    let style = Style::new().fg(color).bg(bg);

    for (i, &ch) in cols.iter().enumerate() {
        let x = area.left() + i as u16;
        if x >= area.right() || !(i as u16).is_multiple_of(step) {
            continue;
        }
        if area.top() > 0 {
            surface.put((x, area.top() - 1), ch, style);
        }
        surface.put((x, area.bottom()), ch, style);
    }

    for (i, &ch) in rows.iter().enumerate() {
        let y = area.top() + i as u16;
        if y >= area.bottom() || !(i as u16).is_multiple_of(step) {
            continue;
        }
        if area.left() > 0 {
            surface.put((area.left() - 1, y), ch, style);
        }
        surface.put((area.right(), y), ch, style);
    }
}

/// Splits `area` into `n` columns with `gap` cells between them.
///
/// Distributes the remainder across the leftmost columns rather than letting
/// it fall off the right edge, so `columns(w, 3, 0)` always tiles `w` exactly.
/// A layout helper rather than arithmetic at each call site because
/// off-by-ones here are invisible until a specific window width shows a
/// one-cell seam.
#[must_use]
pub fn columns(area: Rect, n: u16, gap: u16) -> Vec<Rect> {
    let n = n.max(1);
    let total_gap = gap * (n - 1);
    let usable = area.width().saturating_sub(total_gap);
    let base = usable / n;
    let extra = usable % n;

    let mut out = Vec::with_capacity(usize::from(n));
    let mut x = area.left();
    for i in 0..n {
        let w = base + u16::from(i < extra);
        out.push(Rect::new(x, area.top(), w, area.height()));
        x += w + gap;
    }
    out
}

/// Splits `area` into a fixed-height band and the remainder.
///
/// Returns `(band, rest)`. Clamps rather than underflowing when `height`
/// exceeds the area, which is the case every responsive layout hits first.
#[must_use]
pub fn split_top(area: Rect, height: u16) -> (Rect, Rect) {
    let h = height.min(area.height());
    (
        Rect::new(area.left(), area.top(), area.width(), h),
        Rect::new(area.left(), area.top() + h, area.width(), area.height() - h),
    )
}

/// [`split_top`] from the bottom edge. Returns `(rest, band)`.
#[must_use]
pub fn split_bottom(area: Rect, height: u16) -> (Rect, Rect) {
    let h = height.min(area.height());
    (
        Rect::new(area.left(), area.top(), area.width(), area.height() - h),
        Rect::new(area.left(), area.bottom() - h, area.width(), h),
    )
}

/// Splits `area` into a fixed-width sidebar on the left and the remainder.
///
/// Returns `(side, rest)`. A sidebar wider than the area collapses to zero
/// rather than consuming everything, so a narrow terminal shows the map
/// instead of only chrome -- the same principle
/// [`split_chrome`](super::split_chrome) applies to the title bars.
#[must_use]
pub const fn split_left(area: Rect, width: u16) -> (Rect, Rect) {
    let w = if width >= area.width() { 0 } else { width };
    (
        Rect::new(area.left(), area.top(), w, area.height()),
        Rect::new(area.left() + w, area.top(), area.width() - w, area.height()),
    )
}

/// [`split_left`] from the right edge. Returns `(rest, side)`.
#[must_use]
pub fn split_right(area: Rect, width: u16) -> (Rect, Rect) {
    let w = if width >= area.width() { 0 } else { width };
    (
        Rect::new(area.left(), area.top(), area.width() - w, area.height()),
        Rect::new(area.right() - w, area.top(), w, area.height()),
    )
}

/// Dims a color toward the page background, for anything out of focus.
#[must_use]
pub fn dimmed(color: Color) -> Color {
    scale(color, 0.55)
}

/// Fills `area` with the chrome background, for a bar that is not a panel.
pub fn band(surface: &mut Surface<'_>, area: Rect) {
    surface.fill_rect(area, ' ', Style::new().bg(CHROME_BG));
}

#[cfg(test)]
mod tests {
    use super::{Border, Panel, bar, columns, split_left, split_right, threshold};
    use retroglyph_core::{Color, Grid, Rect, Surface};

    /// A surface over a scratch grid, for tests that need to read cells back.
    fn scratch(cols: u16, rows: u16) -> Grid {
        Grid::new(cols, rows)
    }

    #[test]
    fn a_panel_reports_the_interior_inside_its_border() {
        let mut grid = scratch(20, 10);
        let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 20, 10), 0);
        let inner = Panel::new()
            .title("Crew")
            .draw(&mut surface, Rect::new(2, 1, 12, 6));
        assert_eq!((inner.left(), inner.top()), (3, 2));
        assert_eq!((inner.width(), inner.height()), (10, 4));
    }

    #[test]
    fn a_panel_too_small_to_frame_reports_an_empty_interior() {
        let mut grid = scratch(8, 8);
        let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 8, 8), 0);
        for (w, h) in [(0, 4), (4, 0), (1, 1), (2, 1)] {
            let inner = Panel::new().draw(&mut surface, Rect::new(0, 0, w, h));
            assert_eq!(inner.width().min(inner.height()), 0, "{w}x{h}");
        }
    }

    #[test]
    fn panel_corners_use_the_requested_line_weight() {
        let mut grid = scratch(10, 6);
        {
            let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 10, 6), 0);
            Panel::new()
                .border(Border::Double)
                .draw(&mut surface, Rect::new(0, 0, 6, 4));
        }
        assert_eq!(
            grid.tile(0, (0u16, 0u16)).map(retroglyph_core::Tile::glyph),
            Some('╔')
        );
        assert_eq!(
            grid.tile(0, (5u16, 3u16)).map(retroglyph_core::Tile::glyph),
            Some('╝')
        );
    }

    #[test]
    fn a_bar_draws_half_cells_so_odd_values_are_not_rounded_away() {
        let mut grid = scratch(8, 2);
        let fill = Color::Rgb { r: 1, g: 2, b: 3 };
        let track = Color::Rgb { r: 0, g: 0, b: 0 };
        {
            let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 8, 2), 0);
            // 3/8 of a 4-cell bar is 1.5 cells: one full, one half.
            bar(&mut surface, (0, 0), 4, 0.375, fill, track);
        }
        let glyphs: Vec<char> = (0..4u16)
            .map(|x| {
                grid.tile(0, (x, 0u16))
                    .map_or(' ', retroglyph_core::Tile::glyph)
            })
            .collect();
        assert_eq!(glyphs, vec!['█', '▌', ' ', ' ']);
    }

    #[test]
    fn a_full_bar_is_solid_and_an_empty_one_is_blank() {
        let mut grid = scratch(8, 2);
        let c = Color::Rgb { r: 9, g: 9, b: 9 };
        {
            let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 8, 2), 0);
            bar(&mut surface, (0, 0), 4, 1.0, c, c);
            bar(&mut surface, (0, 1), 4, 0.0, c, c);
        }
        for x in 0..4u16 {
            assert_eq!(
                grid.tile(0, (x, 0u16)).map(retroglyph_core::Tile::glyph),
                Some('█'),
                "full at {x}"
            );
            assert_eq!(
                grid.tile(0, (x, 1u16)).map(retroglyph_core::Tile::glyph),
                Some(' '),
                "empty at {x}"
            );
        }
    }

    #[test]
    fn a_bar_clamps_values_outside_the_unit_range() {
        let mut grid = scratch(4, 2);
        let c = Color::Rgb { r: 9, g: 9, b: 9 };
        {
            let mut surface = Surface::new(&mut grid, Rect::new(0, 0, 4, 2), 0);
            bar(&mut surface, (0, 0), 4, 9.0, c, c);
            bar(&mut surface, (0, 1), 4, -9.0, c, c);
        }
        assert_eq!(
            grid.tile(0, (3u16, 0u16)).map(retroglyph_core::Tile::glyph),
            Some('█')
        );
        assert_eq!(
            grid.tile(0, (0u16, 1u16)).map(retroglyph_core::Tile::glyph),
            Some(' ')
        );
    }

    #[test]
    fn thresholds_step_down_through_green_amber_red() {
        let (hi, mid, lo) = (threshold(1.0), threshold(0.5), threshold(0.1));
        assert_ne!(hi, mid);
        assert_ne!(mid, lo);
        assert_eq!(threshold(0.9), hi, "the healthy band is flat");
    }

    #[test]
    fn columns_tile_the_area_exactly_when_there_is_no_gap() {
        let cols = columns(Rect::new(0, 0, 100, 4), 3, 0);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].left(), 0);
        assert_eq!(cols[2].right(), 100, "the remainder must not be dropped");
        assert_eq!(cols.iter().map(Rect::width).sum::<u16>(), 100);
    }

    #[test]
    fn columns_account_for_every_gap() {
        let cols = columns(Rect::new(0, 0, 100, 4), 4, 2);
        assert_eq!(cols.iter().map(Rect::width).sum::<u16>(), 100 - 3 * 2);
        assert_eq!(cols[3].right(), 100);
    }

    #[test]
    fn a_sidebar_wider_than_its_area_collapses_instead_of_taking_everything() {
        let (side, rest) = split_left(Rect::new(0, 0, 20, 5), 40);
        assert_eq!(side.width(), 0);
        assert_eq!(rest.width(), 20, "the map keeps the space");

        let (rest, side) = split_right(Rect::new(0, 0, 20, 5), 20);
        assert_eq!(side.width(), 0);
        assert_eq!(rest.width(), 20);
    }

    #[test]
    fn sidebars_abut_the_content_area_exactly() {
        let (side, rest) = split_left(Rect::new(3, 1, 40, 5), 12);
        assert_eq!(side.right(), rest.left());
        assert_eq!(side.width() + rest.width(), 40);

        let (rest, side) = split_right(Rect::new(3, 1, 40, 5), 12);
        assert_eq!(rest.right(), side.left());
        assert_eq!(rest.width() + side.width(), 40);
    }
}
