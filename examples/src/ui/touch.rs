//! Touch-first pointer handling: how big a control has to be before a finger
//! can hit it, how a tap is told apart from a drag, and where the reflow
//! breakpoints are.
//!
//! Every demo from 27 on is meant to be usable on a phone, and that is a
//! rendering constraint before it is an input one. The numbers below are not
//! taste; they fall out of the gallery's own pixel geometry, so they are
//! derived here once rather than guessed at per demo.
//!
//! ## Where [`TAP_W`] and [`TAP_H`] come from
//!
//! The pixel backends draw one cell as one 8x16 glyph (the embedded CP437
//! bitmap font, and `assets/blocks.png` matches it). On the browser build,
//! `retroglyph-window` fills the viewport and caps the device pixel ratio at
//! 1.5 for `present()` cost, so the backing store is `css_px * 1.5` and one
//! cell is:
//!
//! ```text
//! 8 physical px / 1.5 = 5.33 CSS px wide
//! 16 physical px / 1.5 = 10.67 CSS px tall
//! ```
//!
//! Apple's HIG asks for a 44x44 pt hit target and Material for 48x48 dp. Take
//! the smaller of the two and divide:
//!
//! ```text
//! 44 / 5.33  = 8.25 cells wide
//! 44 / 10.67 = 4.12 cells tall
//! ```
//!
//! Hence 9x4 cells, rounded up. That is the entire argument for why these
//! demos are drawn at "interface" resolution rather than one glyph per unit: a
//! 1x1 cell control is a 5x11 CSS px target, a quarter the linear size of the
//! smallest thing a finger can reliably hit. A map whose tiles are single
//! glyphs is not a map you can play on a phone, whatever it looks like on a
//! desktop monitor.
//!
//! The same arithmetic sets the grid a phone actually gets, which is worth
//! knowing because it is not small and it is not the shape a terminal usually
//! is:
//!
//! ```text
//! portrait  390x844 CSS px -> 585x1266 physical ->  73 x 79 cells
//! landscape 844x390 CSS px -> 1266x585 physical -> 158 x 36 cells
//! ```
//!
//! So the responsive range a demo has to survive is not "narrow to wide", it
//! is *tall and narrow* to *wide and short*, with a factor of four between the
//! two aspect ratios. [`Shape`] is the classification every demo branches on.

use retroglyph_core::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use retroglyph_core::{Pos, Rect};

/// Minimum width in cells for anything meant to be tapped.
///
/// See the module docs: 44 CSS px against a 5.33 CSS px cell. A control
/// narrower than this is a control a finger will miss on a phone, however
/// legible it looks on a desktop.
pub const TAP_W: u16 = 9;

/// Minimum height in cells for anything meant to be tapped. See [`TAP_W`].
///
/// Half the width because cells are twice as tall as they are wide, so the
/// same physical square is fewer rows than columns. This asymmetry is why
/// touch targets in this gallery are wide, squat rectangles rather than the
/// squares a pixel UI would use.
pub const TAP_H: u16 = 4;

/// How far a pointer may move between press and release and still count as a
/// tap rather than a drag, in columns.
///
/// A finger is not a mouse: it rolls a little on the way down, and a press
/// that reports a one-cell move is still a tap as far as the person doing it
/// is concerned. Roughly 10 CSS px, the slop Android's `ViewConfiguration`
/// uses, which lands at two columns.
pub const TAP_SLOP_X: i32 = 2;

/// [`TAP_SLOP_X`] in rows. One rather than two because a row is twice a
/// column's height, so the same physical distance is half the count.
pub const TAP_SLOP_Y: i32 = 1;

/// The viewport shape a demo is laying out into.
///
/// Three cases rather than a width threshold, because width alone cannot tell
/// a landscape phone (158x36) from a desktop window (158x50): they agree on
/// columns and disagree on everything that matters. What actually drives the
/// layout decision is whether rows or columns are the scarce resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// Tall and narrow: a phone held upright. Panels stack vertically, the
    /// board takes the top, controls sit at the bottom within thumb reach.
    Portrait,
    /// Wide and short: a phone or tablet on its side. Rows are the scarce
    /// resource, so panels go side by side and anything stacked has to be
    /// spent carefully.
    Landscape,
    /// Wide and tall: a desktop window or a large tablet. Both axes are
    /// affordable, so the full three-column layout fits.
    Desktop,
}

impl Shape {
    /// Classifies `area`.
    ///
    /// `Desktop` requires *both* a wide viewport and enough rows to stack a
    /// map over a sidebar without either collapsing; a short-but-wide window
    /// is a landscape phone as far as layout is concerned, whatever is
    /// actually running it. The thresholds are the smallest sizes at which the
    /// respective layouts still show what they exist to show, not device
    /// classes: a demo that branched on a guess about the hardware would be
    /// wrong on a resized desktop window, which is the case most people
    /// actually hit.
    #[must_use]
    pub const fn of(area: Rect) -> Self {
        if area.width() < 100 && area.height() >= area.width() / 2 {
            Self::Portrait
        } else if area.height() < 40 {
            Self::Landscape
        } else {
            Self::Desktop
        }
    }

    /// Whether the layout should stack panels top to bottom rather than side
    /// by side.
    #[must_use]
    pub const fn stacks(self) -> bool {
        matches!(self, Self::Portrait)
    }
}

/// Grows `rect` to at least [`TAP_W`] x [`TAP_H`], keeping it inside `bounds`.
///
/// For a control whose *content* is small (a one-glyph icon, a two-digit
/// count) but which still has to be tappable. Grows right and down first,
/// then shifts back from the far edge if that would overflow, so a button in
/// the bottom-right corner ends up inside the panel rather than clipped by it.
///
/// Returns `rect` unchanged if `bounds` is itself too small to hold a legal
/// target: there is nothing useful to do there, and silently returning a rect
/// outside the panel would be worse than returning one that is merely small.
#[must_use]
pub fn tappable(rect: Rect, bounds: Rect) -> Rect {
    if bounds.width() < TAP_W || bounds.height() < TAP_H {
        return rect;
    }
    let w = rect.width().max(TAP_W);
    let h = rect.height().max(TAP_H);
    let x = rect.left().min(bounds.right().saturating_sub(w));
    let y = rect.top().min(bounds.bottom().saturating_sub(h));
    Rect::new(x.max(bounds.left()), y.max(bounds.top()), w, h)
}

/// A frame's worth of tappable regions, each carrying what tapping it means.
///
/// Rebuilt every frame during layout rather than retained, which is what keeps
/// it honest: a hotspot cannot outlive the thing it points at, and a control
/// that was not drawn this frame cannot be tapped this frame. That is the
/// property immediate-mode UI gets for free and a retained widget tree has to
/// maintain by hand.
///
/// `T` is whatever the demo wants to learn from a hit, usually a small
/// action enum. Hit-testing walks in reverse insertion order so a panel drawn
/// over another wins the tap, matching what the eye expects from overlapping
/// controls.
pub struct Hotspots<T> {
    spots: Vec<(Rect, T)>,
}

impl<T> Default for Hotspots<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Hotspots<T> {
    /// An empty hit list.
    #[must_use]
    pub const fn new() -> Self {
        Self { spots: Vec::new() }
    }

    /// Drops every registered region. Call once at the start of layout.
    pub fn clear(&mut self) {
        self.spots.clear();
    }

    /// Registers `rect` as meaning `action`.
    ///
    /// Zero-area rects are dropped rather than stored: a collapsed panel
    /// registering a 0x0 hotspot cannot be hit anyway, and keeping them makes
    /// [`len`](Self::len) lie about how many controls are live.
    pub fn push(&mut self, rect: Rect, action: T) {
        if rect.width() > 0 && rect.height() > 0 {
            self.spots.push((rect, action));
        }
    }

    /// Registers `rect` grown to a legal touch target inside `bounds`.
    ///
    /// The hit region is deliberately allowed to be larger than the drawn
    /// control. A 1x1 close button that *looks* like one glyph but *hits* like
    /// 9x4 is the standard fix for small affordances on touch, and it costs
    /// nothing as long as no two grown regions overlap.
    pub fn push_tappable(&mut self, rect: Rect, bounds: Rect, action: T) {
        self.push(tappable(rect, bounds), action);
    }

    /// The action whose region contains `pos`, latest registration first.
    #[must_use]
    pub fn hit(&self, pos: Pos) -> Option<&T> {
        self.spots
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains_pos(pos))
            .map(|(_, action)| action)
    }

    /// The region registered for the action `pred` accepts, for drawing a
    /// highlight over a control the demo knows by name rather than by rect.
    #[must_use]
    pub fn rect_where(&self, pred: impl Fn(&T) -> bool) -> Option<Rect> {
        self.spots
            .iter()
            .find(|(_, action)| pred(action))
            .map(|(rect, _)| *rect)
    }

    /// How many regions are registered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.spots.len()
    }

    /// Whether no regions are registered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.spots.is_empty()
    }
}

/// What a pointer did over one frame, after tap-versus-drag has been decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Gesture {
    /// Where a press landed that was released without becoming a drag.
    pub tap: Option<Pos>,
    /// Where a press first went down, while it is still held.
    pub press: Option<Pos>,
    /// Where the held pointer is now, if it has become a drag.
    pub drag: Option<Pos>,
    /// Where a drag was released, once per drag.
    pub drop: Option<Pos>,
    /// Columns and rows moved since the previous frame while dragging.
    pub delta: (i32, i32),
    /// Where an unpressed pointer is, on backends that have one. Always
    /// `None` on touch, which is the whole reason hover cannot carry meaning.
    pub hover: Option<Pos>,
    /// Wheel notches this frame: positive up, negative down.
    pub scroll: i32,
}

/// Decodes raw pointer events into taps, drags, and drops.
///
/// Exists because "was that a tap or a drag?" cannot be answered by a single
/// event, and every demo that pans a map *and* has buttons needs the answer.
/// A press that moves past [`TAP_SLOP_X`]/[`TAP_SLOP_Y`] becomes a drag and can
/// never go back to being a tap, so a map pan that happens to end over a
/// button does not also press the button. That one rule is the difference
/// between a map that can be panned and a map that fires a command every time
/// you look at it.
///
/// [`Gesture`] is per frame and consumed by [`take`](Self::take): taps and
/// drops are edges, and an edge that is readable twice gets acted on twice.
#[derive(Default)]
pub struct Pointer {
    gesture: Gesture,
    down_at: Option<Pos>,
    last: Option<Pos>,
    dragging: bool,
}

impl Pointer {
    /// A pointer with nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gesture: Gesture {
                tap: None,
                press: None,
                drag: None,
                drop: None,
                delta: (0, 0),
                hover: None,
                scroll: 0,
            },
            down_at: None,
            last: None,
            dragging: false,
        }
    }

    /// Feeds one event. Ignores anything that is not a pointer event, so a
    /// demo can hand it the whole queue.
    ///
    /// Touch arrives here already disguised as a mouse: `retroglyph-window`
    /// synthesizes a left-button `Down`/`Moved`/`Up` sequence from the first
    /// finger, so a tap and a drag on a phone are the same events a click and
    /// a click-drag are on a desktop. Nothing in this module (or in any demo)
    /// needs a second input path for touch.
    ///
    /// `Drag` and `Moved` are deliberately handled by the same arm, and that
    /// is not defensive coding: `MouseEventKind::Drag` is emitted only by the
    /// crossterm backend. The winit backends report every pointer motion as
    /// `Moved` whether or not a button is held, because `WindowApp` tracks the
    /// cursor position but never which buttons are down
    /// ([retroglyph#554](https://github.com/crates-lurey-io/retroglyph/issues/554)).
    /// Matching on `Drag` alone -- the obvious way to write this -- therefore
    /// works in a terminal and silently does nothing in a browser, which is
    /// the one backend where touch actually happens. Since a drag is decided
    /// here from the tracked press anyway (see [`moved`](Self::moved)), the
    /// kind is only a hint and both spellings mean the same thing.
    pub fn feed(&mut self, event: &Event) {
        let Event::Mouse(mouse) = event else {
            return;
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.press(mouse.position),
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                self.moved(mouse);
            }
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse.position),
            MouseEventKind::ScrollUp => self.gesture.scroll += 1,
            MouseEventKind::ScrollDown => self.gesture.scroll -= 1,
            _ => {}
        }
    }

    const fn press(&mut self, at: Pos) {
        self.down_at = Some(at);
        self.last = Some(at);
        self.dragging = false;
        self.gesture.press = Some(at);
        self.gesture.hover = Some(at);
    }

    fn moved(&mut self, mouse: &MouseEvent) {
        let at = mouse.position;
        self.gesture.hover = Some(at);

        let Some(origin) = self.down_at else {
            // No button held: a real hover, which only desktop ever produces.
            self.last = Some(at);
            return;
        };

        // Promote to a drag once past the slop, and never demote: a press that
        // wandered and came back is still a drag, because the map already
        // moved under it.
        let dx = i32::from(at.x) - i32::from(origin.x);
        let dy = i32::from(at.y) - i32::from(origin.y);
        if dx.abs() > TAP_SLOP_X || dy.abs() > TAP_SLOP_Y {
            self.dragging = true;
        }

        if self.dragging {
            if let Some(prev) = self.last {
                self.gesture.delta.0 += i32::from(at.x) - i32::from(prev.x);
                self.gesture.delta.1 += i32::from(at.y) - i32::from(prev.y);
            }
            self.gesture.drag = Some(at);
        }
        self.last = Some(at);
    }

    const fn release(&mut self, at: Pos) {
        if self.down_at.is_some() {
            if self.dragging {
                self.gesture.drop = Some(at);
            } else {
                self.gesture.tap = Some(at);
            }
        }
        self.down_at = None;
        self.last = None;
        self.dragging = false;
        self.gesture.press = None;
        self.gesture.drag = None;
    }

    /// Whether a press is currently held and has become a drag.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Where the press that is currently held started, if any.
    #[must_use]
    pub const fn press_origin(&self) -> Option<Pos> {
        self.down_at
    }

    /// Takes this frame's gesture and resets the per-frame parts.
    ///
    /// The held-pointer fields (`press`, `drag`, `hover`) are re-seeded from
    /// live state rather than cleared, so a demo that reads this once per
    /// frame still sees a continuous drag; only the edges (`tap`, `drop`,
    /// `delta`, `scroll`) are consumed.
    pub const fn take(&mut self) -> Gesture {
        let out = self.gesture;
        self.gesture = Gesture {
            tap: None,
            press: self.down_at,
            drag: if self.dragging { self.last } else { None },
            drop: None,
            delta: (0, 0),
            hover: out.hover,
            scroll: 0,
        };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{Gesture, Hotspots, Pointer, Shape, TAP_H, TAP_W, tappable};
    use retroglyph_core::event::{Event, MouseButton, MouseEvent, MouseEventKind};
    use retroglyph_core::{KeyModifiers, Pos, Rect};

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            position: Pos::new(x, y),
            pixel_position: None,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn down(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn drag(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Drag(MouseButton::Left), x, y)
    }

    fn up(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Up(MouseButton::Left), x, y)
    }

    fn play(events: &[Event]) -> Gesture {
        let mut pointer = Pointer::new();
        for event in events {
            pointer.feed(event);
        }
        pointer.take()
    }

    #[test]
    fn a_press_and_release_in_place_is_a_tap() {
        let g = play(&[down(10, 5), up(10, 5)]);
        assert_eq!(g.tap, Some(Pos::new(10, 5)));
        assert_eq!(g.drop, None);
    }

    #[test]
    fn a_finger_that_rolls_within_the_slop_is_still_a_tap() {
        // Two columns and one row is exactly the slop, so still a tap: a
        // press that reports a small wobble is a tap to the person doing it.
        let g = play(&[down(10, 5), drag(12, 6), up(12, 6)]);
        assert_eq!(g.tap, Some(Pos::new(12, 6)));
    }

    #[test]
    fn a_press_that_travels_becomes_a_drag_and_never_a_tap() {
        let g = play(&[down(10, 5), drag(30, 5), up(30, 5)]);
        assert_eq!(g.tap, None, "a pan must not also press what it ends over");
        assert_eq!(g.drop, Some(Pos::new(30, 5)));
    }

    #[test]
    fn a_drag_that_returns_to_its_origin_stays_a_drag() {
        // The map already moved, so treating the release as a tap would fire
        // a command the person was not aiming at.
        let g = play(&[down(10, 5), drag(40, 5), drag(10, 5), up(10, 5)]);
        assert_eq!(g.tap, None);
        assert_eq!(g.drop, Some(Pos::new(10, 5)));
    }

    #[test]
    fn drag_delta_accumulates_across_events_within_one_frame() {
        let g = play(&[down(10, 5), drag(20, 5), drag(26, 9)]);
        assert_eq!(g.delta, (16, 4));
        assert_eq!(g.drag, Some(Pos::new(26, 9)));
    }

    #[test]
    fn taking_a_gesture_consumes_edges_but_keeps_the_held_drag() {
        let mut pointer = Pointer::new();
        for event in [down(10, 5), drag(40, 5)] {
            pointer.feed(&event);
        }
        let first = pointer.take();
        assert_eq!(first.delta, (30, 0));
        assert!(first.drag.is_some());

        let second = pointer.take();
        assert_eq!(second.delta, (0, 0), "the delta is an edge, not a level");
        assert!(
            second.drag.is_some(),
            "the drag is still held, so it must survive the take"
        );
        assert_eq!(second.tap, None);
    }

    #[test]
    fn scroll_notches_sum_and_then_reset() {
        let mut pointer = Pointer::new();
        for event in [
            mouse(MouseEventKind::ScrollUp, 0, 0),
            mouse(MouseEventKind::ScrollUp, 0, 0),
            mouse(MouseEventKind::ScrollDown, 0, 0),
        ] {
            pointer.feed(&event);
        }
        assert_eq!(pointer.take().scroll, 1);
        assert_eq!(pointer.take().scroll, 0);
    }

    #[test]
    fn a_release_with_no_press_produces_nothing() {
        // The browser can deliver an `Up` after focus was lost mid-press.
        let g = play(&[up(4, 4)]);
        assert_eq!(g.tap, None);
        assert_eq!(g.drop, None);
    }

    #[test]
    fn hotspots_resolve_the_topmost_registration() {
        let mut spots = Hotspots::new();
        spots.push(Rect::new(0, 0, 20, 10), "panel");
        spots.push(Rect::new(4, 4, 6, 3), "button");
        assert_eq!(spots.hit(Pos::new(5, 5)), Some(&"button"));
        assert_eq!(spots.hit(Pos::new(1, 1)), Some(&"panel"));
        assert_eq!(spots.hit(Pos::new(40, 40)), None);
    }

    #[test]
    fn hotspots_drop_collapsed_regions() {
        let mut spots = Hotspots::new();
        spots.push(Rect::new(0, 0, 0, 4), "collapsed");
        spots.push(Rect::new(0, 0, 4, 0), "collapsed");
        assert!(spots.is_empty());
    }

    #[test]
    fn a_tiny_control_grows_to_a_legal_target() {
        let bounds = Rect::new(0, 0, 40, 20);
        let grown = tappable(Rect::new(2, 2, 1, 1), bounds);
        assert!(grown.width() >= TAP_W && grown.height() >= TAP_H);
    }

    #[test]
    fn growing_a_corner_control_keeps_it_inside_its_bounds() {
        let bounds = Rect::new(0, 0, 40, 20);
        let grown = tappable(Rect::new(39, 19, 1, 1), bounds);
        assert!(
            grown.right() <= bounds.right() && grown.bottom() <= bounds.bottom(),
            "{grown:?} escaped {bounds:?}"
        );
    }

    #[test]
    fn a_control_in_bounds_too_small_to_hold_a_target_is_left_alone() {
        let bounds = Rect::new(0, 0, 4, 2);
        let rect = Rect::new(0, 0, 2, 1);
        assert_eq!(tappable(rect, bounds), rect);
    }

    #[test]
    fn shapes_split_by_which_axis_is_scarce_not_by_width_alone() {
        // A landscape phone and a desktop window agree on columns.
        assert_eq!(Shape::of(Rect::new(0, 0, 158, 36)), Shape::Landscape);
        assert_eq!(Shape::of(Rect::new(0, 0, 158, 50)), Shape::Desktop);
        assert_eq!(Shape::of(Rect::new(0, 0, 73, 79)), Shape::Portrait);
        assert!(Shape::of(Rect::new(0, 0, 73, 79)).stacks());
        assert!(!Shape::of(Rect::new(0, 0, 158, 36)).stacks());
    }

    #[test]
    fn the_headless_test_grid_is_a_landscape_layout() {
        // 80x24 is what every snapshot test runs at, so whichever branch it
        // takes is the one most exercised in CI.
        assert_eq!(Shape::of(Rect::new(0, 0, 80, 22)), Shape::Landscape);
    }
}
