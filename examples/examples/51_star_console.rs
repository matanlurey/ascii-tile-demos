//! 51: Star Console -- a floating, draggable, stacking window manager set
//! inside a console bezel, in the register of Star Wars Rebellion's galaxy
//! screen.
//!
//! Every other interface demo in this gallery draws one fixed layout. This
//! one draws a *desktop*: real windows with title bars, close and minimize
//! buttons, a z-order that a tap or a drag can change, and content that
//! drills down through the game's own hierarchy (a sector of star systems,
//! down to a planet's fleet, down to one ship in that fleet) by opening a new
//! window rather than replacing the old one. That is the one thing nothing
//! else in the gallery has, and it is the whole point of this file; the
//! console bezel around it (the left power-cell strip, the droid, the icon
//! rail, the console strip, the three-counter readout) is dressing borrowed
//! from the reference screenshots and stays quiet compared to the windows.
//!
//! Techniques on show:
//!
//! - **Painter's-algorithm z-order** ([`StarConsole::draw_windows`]): windows
//!   are stored bottom-to-top in a `Vec` and drawn in that order, so a window
//!   drawn later simply overwrites whatever an earlier one put in the same
//!   cells. No layer indirection is needed for correct clipping; the vec
//!   order *is* the z-order, and [`Hotspots`] resolving "last registration
//!   wins" (see `ui::touch`) means hit-testing agrees with what is on screen
//!   for free, as long as each window's hotspots are pushed in the same
//!   bottom-to-top order its rect was drawn in.
//! - **Title-bar dragging through [`Pointer`]**: a press is attributed to a
//!   window once, at the frame it lands on that window's title-bar hotspot,
//!   and remembered by window id rather than by hit-testing every frame
//!   (the pointer's origin does not track the window once it starts moving
//!   under it). Every later frame just adds `Gesture::delta` to that window's
//!   stored position, the same accumulate-a-delta pattern `30_fleet_command`
//!   uses for panning its map. See [`Pointer::feed`]'s doc comment for why
//!   `Drag` and `Moved` have to be treated as the same event on this path.
//! - **Drill-down by opening, not replacing**
//!   ([`StarConsole::activate_row`]): tapping a row in a Sector window opens
//!   a Planet window; a row there opens a Fleet window; a row there opens a
//!   Ship window. Each open is deduplicated by [`Kind`] equality
//!   ([`StarConsole::open_or_raise`]), so drilling into the same ship twice
//!   raises the existing window instead of stacking duplicates.
//! - **Portrait windows become full-width sheets**
//!   ([`window_size`], [`StarConsole::clamp_windows`]): on a phone held
//!   upright a floating box the size a desktop window wants is unusable, so
//!   width is forced to fill the desk and horizontal drag is disabled; only
//!   the vertical position (and therefore the stacking order a drag can
//!   express) stays live.
//! - **World extent from the live rect, not a constant**
//!   ([`StarConsole::draw_galaxy`], [`Regions::compute`]): the starfield and
//!   the sector clusters are placed at desk-relative fractions and scaled to
//!   whatever `desk` turns out to be at the current terminal size, per the
//!   round-3 rule against sizing a world for the snapshot grid and leaving
//!   the desktop grid mostly black.
//!
//! ```sh
//! cargo run --example 51_star_console --features crossterm
//! cargo run --example 51_star_console --features software
//! cargo run --example 51_star_console --features gl
//! cargo run --example 51_star_console  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};
use retroglyph_widgets::truncate;

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel;
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape, TAP_H, TAP_W};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{rgb, scale};

/// Sector names, in the order [`SYSTEMS`] is grouped by. Real Star Wars
/// system names from the reference screenshots, kept distinct at both levels
/// (three sector names, nine system names) so nothing ever shows the same
/// place-name twice on screen at once.
const SECTOR_NAMES: [&str; 3] = ["Sector Alpha", "Sector Beta", "Sector Gamma"];

/// Every system, three per sector, flattened in sector order. See
/// [`sector_systems`] and [`global_system_idx`] for how a (sector, local)
/// pair maps into this array.
const SYSTEMS: [&str; 9] = [
    "Palanhi",
    "Deltaya",
    "Obroa-skai",
    "Mrisst",
    "Carida",
    "Bimmisaari",
    "Ralltiir",
    "Fakir",
    "Hapes",
];

/// The fleet number orbiting each system, indexed the same way as
/// [`SYSTEMS`]. A fixed permutation of 1-9 rather than a formula, so it reads
/// as assigned fleet designations (as in the reference screenshot's "Fleet
/// 4") rather than an obviously generated sequence, while still guaranteeing
/// every fleet number on screen is unique.
const FLEET_NUMBER: [u32; 9] = [4, 7, 2, 9, 3, 6, 1, 8, 5];

/// Ship classes a fleet can be built from. Six entries is comfortably more
/// than the 2-4 ships any one fleet has (see [`ship_count`]), so picking a
/// contiguous run starting at a per-system offset never repeats a class
/// within one fleet's own window.
const SHIP_CLASSES: [&str; 6] = [
    "Star Destroyer",
    "Carrack Light Cruiser",
    "TIE Fighter Squadron",
    "Assault Frigate",
    "Corellian Corvette",
    "Nebulon-B Frigate",
];

/// Normalized (fraction of desk width, fraction of desk height) centers for
/// the three sector clusters drawn on the galaxy backdrop. Fractions, not
/// cell offsets, so the spiral keeps its shape at any desk size instead of
/// huddling in a corner of a desktop-sized rect (the round-3 "fill the
/// viewport" rule).
const CLUSTER_POS: [(f32, f32); 3] = [(0.22, 0.32), (0.68, 0.22), (0.46, 0.74)];

/// Height in rows of one list row inside a window's content, tied to
/// [`TAP_H`] rather than restated as a separate magic number: a row a finger
/// cannot reliably hit is a row that cannot be drilled into.
const ROW_H: u16 = TAP_H;

/// Smallest a floating window may be on a wide layout, in cells.
const WIN_MIN_W: u16 = 22;
/// Preferred window width on a wide (landscape/desktop) layout. Comfortably
/// past [`WIN_MIN_W`] so the title, the two buttons, and a row of text never
/// fight each other for space.
const WIN_DESKTOP_W: u16 = 34;
/// Smallest a window may be tall: one title row, one content row at
/// [`ROW_H`], one border row.
const WIN_MIN_H: u16 = ROW_H + 2;
/// Content rows a Ship window reserves for its static readout (class name,
/// hull bar, shield bar, crew line), independent of [`ROW_H`] because that
/// content is not a tappable list.
const SHIP_CONTENT_H: u16 = 6;

const BG_DESK: Color = rgb(6, 9, 20);
const BEZEL_BG: Color = rgb(26, 29, 40);
const CHROME_BAND_BG: Color = rgb(18, 26, 24);
const WINDOW_BG: Color = rgb(15, 19, 17);
const ROW_ALT_BG: Color = rgb(19, 24, 21);
const FRAME_FOCUSED: Color = rgb(150, 226, 180);
const FRAME_DIM: Color = rgb(72, 100, 86);
const TITLE_FOCUSED: Color = rgb(42, 132, 74);
const TITLE_DIM: Color = rgb(26, 58, 40);
const TITLE_FG: Color = rgb(232, 240, 224);
const FG_LIGHT: Color = rgb(206, 214, 198);
const DIM_FG: Color = rgb(120, 130, 118);
const RAIL_BTN: Color = rgb(38, 44, 60);
const RAIL_BTN_ACTIVE: Color = rgb(58, 92, 150);
const ACCENT_GREEN: Color = rgb(120, 220, 150);

/// Decorative console-strip icon glyphs, cycled by index. Purely visual.
const CONSOLE_ICONS: [char; 6] = [
    '\u{263C}', '\u{266A}', '\u{2126}', '\u{2660}', '\u{2663}', '\u{25A0}',
];
/// Decorative right-rail icon glyphs, for the slots past the three
/// functional sector buttons. Purely visual.
const RAIL_DECOR: [char; 7] = [
    '\u{263C}', '\u{2660}', '\u{2663}', '\u{2126}', '\u{03A6}', '\u{03B4}', '\u{25A0}',
];

/// What content a window shows, and what it was opened from. Equality is the
/// dedup key [`StarConsole::open_or_raise`] uses, so opening the same ship
/// twice raises the existing window rather than stacking a duplicate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A sector's list of systems.
    Sector(usize),
    /// The fleet in orbit at one system.
    Planet(usize),
    /// The ships in the fleet at one system (one fleet per system, so the
    /// system index doubles as the fleet key).
    Fleet(usize),
    /// One ship: `(system, ship-in-fleet index)`.
    Ship(usize, usize),
}

/// A floating window: identity, content, and the mutable state a drag or a
/// close/minimize button changes.
struct Window {
    id: u32,
    title: String,
    kind: Kind,
    /// Top-left corner, in cells relative to the desk rect, not the screen.
    /// Desk-relative so a window's position survives the desk moving (the
    /// left bezel or the right rail changing width) without every window
    /// needing to be re-anchored.
    pos: (i32, i32),
    size: (u16, u16),
    minimized: bool,
}

/// What tapping or pressing a registered region means. Carries a window's
/// stable `id` rather than its position in [`StarConsole::windows`]: that
/// position changes the instant a window is raised, and an action captured
/// from this frame's hotspots must still resolve correctly after this same
/// frame's press handling has already reordered the vec.
#[derive(Clone, Copy)]
enum Action {
    /// Anywhere in a window not covered by a more specific region: raises it.
    Body(u32),
    /// The draggable strip of the title bar (the title text, not the
    /// buttons): raises the window and can start a drag.
    TitleBar(u32),
    Close(u32),
    Minimize(u32),
    /// One row of a window's content list, by index.
    Row(u32, u8),
    /// A sector-opening control: a galaxy cluster, a rail button, or a
    /// portrait action-bar button.
    OpenSector(usize),
}

/// Which window (if any) a held press is dragging, and by what id.
struct Drag {
    id: u32,
}

/// The panel rects one frame's layout resolves to. Computed once per tick and
/// threaded through drawing, hit-testing, and keyboard handling, so all three
/// agree on where the desk is.
struct Regions {
    counters: Rect,
    console: Rect,
    left: Rect,
    right: Rect,
    desk: Rect,
    action_bar: Rect,
}

impl Regions {
    /// Lays out the bezel chrome around whatever is left as the desk.
    ///
    /// Portrait collapses the side rails (there is no width to spare for
    /// them) and moves their one interactive job -- opening a sector -- into
    /// a full-width strip at the bottom, the thumb zone. Wide layouts keep
    /// the console's own geometry: counters on top, a console strip and a
    /// power-cell rail on the sides, the desk in the middle.
    fn compute(content: Rect, shape: Shape) -> Self {
        let empty_h = |r: Rect| Rect::new(r.left(), r.bottom(), r.width(), 0);
        let empty_w = |r: Rect| Rect::new(r.right(), r.top(), 0, r.height());

        if shape == Shape::Portrait {
            let (desk, action_bar) = if content.height() >= 24 {
                panel::split_bottom(content, 5)
            } else {
                (content, empty_h(content))
            };
            return Self {
                counters: empty_h(content),
                console: empty_h(content),
                left: empty_w(content),
                right: empty_w(content),
                desk,
                action_bar,
            };
        }

        let (counters, rest) = if content.height() >= 16 {
            panel::split_top(content, 1)
        } else {
            (empty_h(content), content)
        };
        let (rest, console) = if rest.height() >= 14 {
            panel::split_bottom(rest, 4)
        } else {
            (rest, empty_h(rest))
        };
        let (left, rest) = if rest.width() >= 60 {
            panel::split_left(rest, 11)
        } else {
            (Rect::new(rest.left(), rest.top(), 0, rest.height()), rest)
        };
        let (desk, right) = if rest.width() >= 40 {
            panel::split_right(rest, 12)
        } else {
            (rest, empty_w(rest))
        };
        Self {
            counters,
            console,
            left,
            right,
            desk,
            action_bar: empty_h(content),
        }
    }
}

/// State: every open window (bottom to top) plus the shared pointer, hit
/// regions, and a clock for the idle animation.
#[derive(Default)]
pub struct StarConsole {
    windows: Vec<Window>,
    next_id: u32,
    pointer: Pointer,
    /// Whether the current held press has already been attributed to a
    /// window (raised, and started a drag if it landed on a title bar).
    /// `Pointer::press_origin` stays constant for the life of a press, so
    /// without this flag every frame of a long drag would re-run the same
    /// hit test against the origin, which is wrong: see the module doc for
    /// why the drag target is decided once and then remembered by id.
    press_claimed: bool,
    drag: Option<Drag>,
    hotspots: Hotspots<Action>,
    time: f32,
    fps: FpsMeter,
}

impl StarConsole {
    /// Opens a window for `kind`, or raises and un-minimizes the existing one
    /// if a window for the same [`Kind`] is already open. The dedup is what
    /// makes drilling into the same ship twice a no-op instead of a pile of
    /// duplicate windows, and what makes the rail/galaxy "open sector"
    /// controls double as "bring it back" once a sector window exists.
    fn open_or_raise(&mut self, kind: Kind, title: String, desk: Rect, shape: Shape) {
        if let Some(idx) = self.windows.iter().position(|w| w.kind == kind) {
            let mut w = self.windows.remove(idx);
            w.minimized = false;
            self.windows.push(w);
            return;
        }
        let size = window_size(&kind, desk, shape);
        let pos = cascade_pos(desk, size, self.windows.len());
        let id = self.next_id;
        self.next_id += 1;
        self.windows.push(Window {
            id,
            title,
            kind,
            pos,
            size,
            minimized: false,
        });
    }

    fn open_sector(&mut self, sector: usize, desk: Rect, shape: Shape) {
        if sector >= SECTOR_NAMES.len() {
            return;
        }
        self.open_or_raise(
            Kind::Sector(sector),
            SECTOR_NAMES[sector].to_string(),
            desk,
            shape,
        );
    }

    /// Moves the window with `id` to the top of the z-order. A no-op if it is
    /// already there or does not exist, so callers do not need to check
    /// first.
    fn raise(&mut self, id: u32) {
        if let Some(idx) = self.windows.iter().position(|w| w.id == id) {
            let w = self.windows.remove(idx);
            self.windows.push(w);
        }
    }

    fn close(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
        if self.drag.as_ref().is_some_and(|d| d.id == id) {
            self.drag = None;
        }
    }

    fn toggle_minimize(&mut self, id: u32) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.minimized = !w.minimized;
        }
    }

    /// Moves window `id` by `(dx, dy)` cells, clamped to stay inside `desk`.
    /// On a portrait phone the x axis is locked: a full-width sheet has
    /// nowhere useful to go sideways, so only the vertical position (and the
    /// stacking a vertical drag can express) is live. See the module doc.
    fn nudge_window(&mut self, id: u32, dx: i32, dy: i32, desk: Rect, shape: Shape) {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        let max_x = i32::from(desk.width())
            .saturating_sub(i32::from(w.size.0))
            .max(0);
        let max_y = i32::from(desk.height())
            .saturating_sub(i32::from(w.size.1))
            .max(0);
        if shape != Shape::Portrait {
            w.pos.0 = (w.pos.0 + dx).clamp(0, max_x);
        }
        w.pos.1 = (w.pos.1 + dy).clamp(0, max_y);
    }

    /// Opens the window one level down the drill-down chain for row `row` of
    /// window `id`'s content, per its [`Kind`]: Sector -> Planet -> Fleet ->
    /// Ship. A Ship window is the leaf and has no rows.
    fn activate_row(&mut self, id: u32, row: usize, desk: Rect, shape: Shape) {
        let Some(win) = self.windows.iter().find(|w| w.id == id) else {
            return;
        };
        match win.kind {
            Kind::Sector(sector) if row < 3 => {
                let sys = global_system_idx(sector, row);
                self.open_or_raise(Kind::Planet(sys), SYSTEMS[sys].to_string(), desk, shape);
            }
            Kind::Planet(sys) if row == 0 => {
                let title = format!("Fleet {}", FLEET_NUMBER[sys]);
                self.open_or_raise(Kind::Fleet(sys), title, desk, shape);
            }
            Kind::Fleet(sys) if row < ship_count(sys) => {
                let title = ship_window_title(sys, row);
                self.open_or_raise(Kind::Ship(sys, row), title, desk, shape);
            }
            _ => {}
        }
    }

    /// Keeps every window's position and (on a portrait phone) width valid
    /// for the current desk rect, every frame rather than only when a window
    /// is created. A demo that only clamps at open time breaks the moment the
    /// desk resizes out from under an already-open window.
    fn clamp_windows(&mut self, desk: Rect, shape: Shape) {
        for w in &mut self.windows {
            if shape == Shape::Portrait {
                let min_w = WIN_MIN_W.min(desk.width().max(1));
                w.size.0 = desk.width().saturating_sub(2).max(min_w);
                w.pos.0 = 0;
            }
            let max_x = i32::from(desk.width())
                .saturating_sub(i32::from(w.size.0))
                .max(0);
            let max_y = i32::from(desk.height())
                .saturating_sub(i32::from(w.size.1))
                .max(0);
            w.pos.0 = w.pos.0.clamp(0, max_x);
            w.pos.1 = w.pos.1.clamp(0, max_y);
        }
    }

    fn handle_key(&mut self, code: KeyCode, desk: Rect, shape: Shape) {
        match code {
            KeyCode::Tab => {
                // Cycling means "bring the bottom-most window to the top",
                // which visits every window in turn without needing a
                // separate focus index: the topmost window already is the
                // keyboard's target for every other binding here.
                if !self.windows.is_empty() {
                    let w = self.windows.remove(0);
                    self.windows.push(w);
                }
            }
            KeyCode::Up => self.nudge_top(0, -2, desk, shape),
            KeyCode::Down => self.nudge_top(0, 2, desk, shape),
            KeyCode::Left => self.nudge_top(-2, 0, desk, shape),
            KeyCode::Right => self.nudge_top(2, 0, desk, shape),
            KeyCode::Char('c' | 'C') => {
                if let Some(id) = self.windows.last().map(|w| w.id) {
                    self.close(id);
                }
            }
            KeyCode::Char('m' | 'M') => {
                if let Some(id) = self.windows.last().map(|w| w.id) {
                    self.toggle_minimize(id);
                }
            }
            KeyCode::Char('1') => self.open_sector(0, desk, shape),
            KeyCode::Char('2') => self.open_sector(1, desk, shape),
            KeyCode::Char('3') => self.open_sector(2, desk, shape),
            _ => {}
        }
    }

    fn nudge_top(&mut self, dx: i32, dy: i32, desk: Rect, shape: Shape) {
        if let Some(id) = self.windows.last().map(|w| w.id) {
            self.nudge_window(id, dx, dy, desk, shape);
        }
    }

    /// Interprets this frame's [`Gesture`] against the hotspots layout just
    /// registered.
    ///
    /// A fresh press is attributed to whatever it landed on exactly once (see
    /// [`press_claimed`](Self::press_claimed)): that raises the hit window
    /// and, if the hit was specifically the title-bar region, remembers it as
    /// the drag target by id. Every frame the drag target is still set, this
    /// frame's `Gesture::delta` is applied to it directly -- the same
    /// accumulate-a-delta shape `30_fleet_command` uses for panning, chosen
    /// over recomputing an offset from the (possibly stale) press origin
    /// because the origin does not move with the window once dragging starts.
    fn handle_gesture(&mut self, gesture: &Gesture, desk: Rect, shape: Shape) {
        let origin = self.pointer.press_origin();
        if origin.is_none() {
            self.press_claimed = false;
            self.drag = None;
        } else if !self.press_claimed {
            self.press_claimed = true;
            if let Some(pos) = origin {
                if let Some(&action) = self.hotspots.hit(pos) {
                    self.raise_for(action);
                    if let Action::TitleBar(id) = action {
                        self.drag = Some(Drag { id });
                    }
                } else {
                    self.drag = None;
                }
            }
        }

        if let Some(id) = self.drag.as_ref().map(|d| d.id) {
            let (dx, dy) = gesture.delta;
            if dx != 0 || dy != 0 {
                self.nudge_window(id, dx, dy, desk, shape);
            }
        }

        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.apply_action(action, desk, shape);
        }
    }

    fn raise_for(&mut self, action: Action) {
        let id = match action {
            Action::Body(id)
            | Action::TitleBar(id)
            | Action::Close(id)
            | Action::Minimize(id)
            | Action::Row(id, _) => Some(id),
            Action::OpenSector(_) => None,
        };
        if let Some(id) = id {
            self.raise(id);
        }
    }

    fn apply_action(&mut self, action: Action, desk: Rect, shape: Shape) {
        match action {
            Action::Close(id) => self.close(id),
            Action::Minimize(id) => self.toggle_minimize(id),
            Action::Row(id, row) => self.activate_row(id, usize::from(row), desk, shape),
            Action::OpenSector(idx) => self.open_sector(idx, desk, shape),
            Action::Body(_) | Action::TitleBar(_) => {}
        }
    }

    fn draw_windows(&mut self, surface: &mut Surface<'_>, desk: Rect) {
        let n = self.windows.len();
        for i in 0..n {
            let focused = i + 1 == n;
            let win = &self.windows[i];
            let rect = window_rect(win, desk);
            draw_window(surface, rect, win, focused);
            window_hotspots(&mut self.hotspots, rect, win);
        }
    }

    fn draw_counters(&self, surface: &mut Surface<'_>, rect: Rect) {
        panel::band(surface, rect);
        let cols = panel::columns(rect, 3, 1);
        let fleets = self
            .windows
            .iter()
            .filter(|w| matches!(w.kind, Kind::Fleet(_) | Kind::Ship(..)))
            .count();
        let sectors = self
            .windows
            .iter()
            .filter(|w| matches!(w.kind, Kind::Sector(_)))
            .count();
        let support = 30.0f32
            .mul_add((self.time * 0.35).sin(), 50.0)
            .clamp(0.0, 100.0);
        let items = [
            format!("Fleets {fleets}"),
            format!("Sectors {sectors}"),
            format!("Support {support:.0}%"),
        ];
        for (col, text) in cols.iter().zip(items.iter()) {
            if col.width() < 2 {
                continue;
            }
            let t = truncate(text, usize::from(col.width().saturating_sub(1)));
            surface.print(
                (col.left() + 1, col.top()),
                t,
                Style::new().fg(ACCENT_GREEN).bg(CHROME_BAND_BG),
            );
        }
    }

    fn draw_console(&self, surface: &mut Surface<'_>, rect: Rect) {
        surface.fill_rect(rect, ' ', Style::new().bg(BEZEL_BG));
        if rect.width() < 8 || rect.height() < 3 {
            return;
        }
        let count = (rect.width() / 14).clamp(1, 6);
        let cols = panel::columns(rect, count, 1);
        for (i, col) in cols.iter().enumerate() {
            if col.width() < 4 {
                continue;
            }
            let panel_h = col.height().saturating_sub(1).max(1);
            let inner = Rect::new(col.left(), col.top(), col.width().min(12), panel_h);
            surface.fill_rect(inner, ' ', Style::new().bg(rgb(18, 20, 30)));
            let pulse = (self.time.mul_add(0.8, i as f32 * 0.5)).sin() > 0.4;
            let color = if pulse {
                rgb(120, 220, 150)
            } else {
                rgb(50, 90, 60)
            };
            surface.put(
                (inner.left() + 1, inner.top() + inner.height() / 2),
                CONSOLE_ICONS[i % CONSOLE_ICONS.len()],
                Style::new().fg(color).bg(rgb(18, 20, 30)),
            );
        }
    }

    fn draw_left_bezel(&self, surface: &mut Surface<'_>, rect: Rect) {
        surface.fill_rect(rect, ' ', Style::new().bg(BEZEL_BG));
        if rect.width() < 5 || rect.height() < 6 {
            return;
        }
        let strip_h = rect.height().saturating_sub(4);
        let inset = Rect::new(
            rect.left() + 1,
            rect.top() + 1,
            rect.width().saturating_sub(2),
            strip_h,
        );
        let seg_h = 2u16;
        let segs = (inset.height() / seg_h).max(1);
        // A single lit segment sweeps down the strip over a few seconds, the
        // classic "scanner" idle tell that says a panel is live rather than
        // painted on. Driven by `self.time`, not a frame counter, so it moves
        // at the same real-world rate on every backend.
        let glow = (self.time * 1.4) as u16 % segs;
        for s in 0..segs {
            let y = inset.top() + s * seg_h;
            if y >= inset.bottom() {
                break;
            }
            let h = seg_h.min(inset.bottom() - y).saturating_sub(1).max(1);
            let color = if s == glow {
                rgb(130, 180, 255)
            } else {
                rgb(40, 70, 150)
            };
            surface.fill_rect(
                Rect::new(inset.left(), y, inset.width(), h),
                '\u{2588}',
                Style::new().fg(color).bg(BEZEL_BG),
            );
        }
        draw_droid(surface, rect);
    }

    fn draw_right_rail(&mut self, surface: &mut Surface<'_>, rect: Rect) {
        surface.fill_rect(rect, ' ', Style::new().bg(BEZEL_BG));
        if rect.width() < 6 {
            return;
        }
        let slot_h = 3u16;
        let slots = (rect.height() / slot_h).clamp(1, 10);
        for i in 0..slots {
            let y = rect.top() + i * slot_h;
            if y >= rect.bottom() {
                break;
            }
            let h = slot_h.min(rect.bottom() - y).saturating_sub(1).max(1);
            let btn = Rect::new(rect.left() + 1, y, rect.width().saturating_sub(2), h);
            if (i as usize) < SECTOR_NAMES.len() {
                let sector = i as usize;
                let active = self.windows.iter().any(|w| w.kind == Kind::Sector(sector));
                let bg = if active { RAIL_BTN_ACTIVE } else { RAIL_BTN };
                surface.fill_rect(btn, ' ', Style::new().bg(bg));
                let label = &SECTOR_NAMES[sector][..1];
                surface.print(
                    (btn.left() + 1, btn.top()),
                    label,
                    Style::new().fg(FG_LIGHT).bg(bg),
                );
                self.hotspots
                    .push_tappable(btn, rect, Action::OpenSector(sector));
            } else {
                surface.fill_rect(btn, ' ', Style::new().bg(RAIL_BTN));
                let glyph = RAIL_DECOR[(i as usize - SECTOR_NAMES.len()) % RAIL_DECOR.len()];
                surface.put(
                    (btn.left() + 1, btn.top()),
                    glyph,
                    Style::new().fg(rgb(90, 100, 120)).bg(RAIL_BTN),
                );
            }
        }
    }

    /// The portrait bottom action bar: one big button per sector, in the
    /// thumb zone, standing in for the desktop's side rail and galaxy
    /// clusters, neither of which fit a phone held upright.
    fn draw_action_bar(&mut self, surface: &mut Surface<'_>, rect: Rect) {
        panel::band(surface, rect);
        let cols = panel::columns(rect, 3, 1);
        for (i, col) in cols.iter().enumerate() {
            if col.width() < TAP_W || col.height() < TAP_H {
                continue;
            }
            let active = self.windows.iter().any(|w| w.kind == Kind::Sector(i));
            let bg = if active { RAIL_BTN_ACTIVE } else { RAIL_BTN };
            surface.fill_rect(*col, ' ', Style::new().bg(bg));
            let text = truncate(SECTOR_NAMES[i], usize::from(col.width().saturating_sub(2)));
            surface.print(
                (col.left() + 1, col.top() + col.height() / 2),
                text,
                Style::new().fg(FG_LIGHT).bg(bg),
            );
            self.hotspots.push(*col, Action::OpenSector(i));
        }
    }

    fn draw_galaxy(&mut self, surface: &mut Surface<'_>, desk: Rect) {
        surface.fill_rect(desk, ' ', Style::new().bg(BG_DESK));
        if desk.width() == 0 || desk.height() == 0 {
            return;
        }
        for y in 0..desk.height() {
            for x in 0..desk.width() {
                let (wx, wy) = (i32::from(x), i32::from(y));
                if hash01(0x5741, wx, wy) > 0.05 {
                    continue;
                }
                let phase = hash01(0x1357, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add(self.time.mul_add(0.6, phase).sin(), 0.5);
                let v = 140.0f32.mul_add(twinkle, 60.0) as u8;
                surface.put(
                    (desk.left() + x, desk.top() + y),
                    '.',
                    Style::new().fg(rgb(v, v, v.saturating_add(30))).bg(BG_DESK),
                );
            }
        }
        for (idx, &(fx, fy)) in CLUSTER_POS.iter().enumerate() {
            self.draw_cluster(surface, desk, idx, fx, fy);
        }
    }

    fn draw_cluster(
        &mut self,
        surface: &mut Surface<'_>,
        desk: Rect,
        idx: usize,
        fx: f32,
        fy: f32,
    ) {
        let cx = fx.mul_add(f32::from(desk.width()), f32::from(desk.left()));
        let cy = fy.mul_add(f32::from(desk.height()), f32::from(desk.top()));
        let active = self.windows.iter().any(|w| w.kind == Kind::Sector(idx));
        let base = if active {
            rgb(120, 220, 150)
        } else {
            rgb(150, 230, 255)
        };

        for p in 0..7i32 {
            let ox = (hash01(0x2200 + idx as u32, p, 0) - 0.5) * 7.0;
            let oy = (hash01(0x3300 + idx as u32, p, 1) - 0.5) * 3.0;
            let px = cx + ox;
            let py = cy + oy;
            if px < f32::from(desk.left()) || py < f32::from(desk.top()) {
                continue;
            }
            let (x, y) = (px as u16, py as u16);
            if x >= desk.right() || y >= desk.bottom() {
                continue;
            }
            let twinkle = 0.5f32.mul_add(self.time.mul_add(0.9, p as f32).sin(), 0.5);
            let lit = if twinkle > 0.4 {
                base
            } else {
                scale(base, 0.5)
            };
            surface.put((x, y), '+', Style::new().fg(lit).bg(BG_DESK));
        }

        let label = truncate(SECTOR_NAMES[idx], 14);
        let label_x = cx as u16;
        let label_y = (cy as u16 + 2).min(desk.bottom().saturating_sub(1));
        if label_x < desk.right() {
            surface.print((label_x, label_y), label, Style::new().fg(base).bg(BG_DESK));
        }

        let w = 10u16.min(desk.width());
        let h = 4u16.min(desk.height());
        let hx = (cx as i32 - i32::from(w) / 2).clamp(
            i32::from(desk.left()),
            i32::from(desk.right()) - i32::from(w).max(1),
        );
        let hy = (cy as i32 - 1).clamp(
            i32::from(desk.top()),
            i32::from(desk.bottom()) - i32::from(h).max(1),
        );
        let hit = Rect::new(hx as u16, hy as u16, w, h);
        self.hotspots
            .push_tappable(hit, desk, Action::OpenSector(idx));
    }

    fn status_text(&self) -> String {
        let top = self.windows.last().map_or("none", |w| w.title.as_str());
        format!("windows {}  top: {}", self.windows.len(), top)
    }
}

/// Left/right cushion for a window's title from its 1-cell `_`/`X` buttons.
/// The two buttons live at fixed offsets from the right edge, shared by the
/// drawing pass and the hit-testing pass so they can never disagree about
/// where the buttons are.
fn title_button_x(rect: Rect) -> (u16, u16) {
    let r = rect.right() - 1;
    (r.saturating_sub(4), r.saturating_sub(1))
}

fn draw_window(surface: &mut Surface<'_>, rect: Rect, win: &Window, focused: bool) {
    if rect.width() < 6 || rect.height() == 0 {
        return;
    }
    surface.fill_rect(rect, ' ', Style::new().bg(WINDOW_BG));
    let title_bg = if focused { TITLE_FOCUSED } else { TITLE_DIM };
    let frame = if focused { FRAME_FOCUSED } else { FRAME_DIM };
    let (l, t) = (rect.left(), rect.top());
    let r = rect.right() - 1;
    let (min_x, close_x) = title_button_x(rect);

    surface.fill_rect(
        Rect::new(l, t, rect.width(), 1),
        ' ',
        Style::new().bg(title_bg),
    );
    let title_room = min_x.saturating_sub(l + 1);
    let text = format!(" {}", truncate(&win.title, usize::from(title_room)));
    surface.print((l, t), &text, Style::new().fg(TITLE_FG).bg(title_bg));
    if rect.width() > 6 {
        surface.put((min_x, t), '_', Style::new().fg(TITLE_FG).bg(title_bg));
        surface.put((close_x, t), 'X', Style::new().fg(TITLE_FG).bg(title_bg));
    }

    if win.minimized || rect.height() < 2 {
        return;
    }
    let b = rect.bottom() - 1;
    for y in (t + 1)..b {
        surface.put((l, y), '\u{2551}', Style::new().fg(frame).bg(WINDOW_BG));
        surface.put((r, y), '\u{2551}', Style::new().fg(frame).bg(WINDOW_BG));
    }
    surface.put((l, b), '\u{255A}', Style::new().fg(frame).bg(WINDOW_BG));
    surface.put((r, b), '\u{255D}', Style::new().fg(frame).bg(WINDOW_BG));
    for x in (l + 1)..r {
        surface.put((x, b), '\u{2550}', Style::new().fg(frame).bg(WINDOW_BG));
    }

    draw_window_content(surface, window_inner(rect), &win.kind);
}

fn draw_window_content(surface: &mut Surface<'_>, inner: Rect, kind: &Kind) {
    if inner.width() == 0 || inner.height() == 0 {
        return;
    }
    if let Kind::Ship(sys, idx) = *kind {
        draw_ship_readout(surface, inner, sys, idx);
        return;
    }
    let rows = window_rows(kind);
    for (i, label) in rows.iter().enumerate() {
        let y = inner.top() + i as u16 * ROW_H;
        if y >= inner.bottom() {
            break;
        }
        let h = ROW_H.min(inner.bottom() - y);
        let row_rect = Rect::new(inner.left(), y, inner.width(), h);
        let shade = if i % 2 == 0 { WINDOW_BG } else { ROW_ALT_BG };
        surface.fill_rect(row_rect, ' ', Style::new().bg(shade));
        let text = format!("\u{25BA} {label}");
        surface.print(
            (inner.left() + 1, y + h / 2),
            truncate(&text, usize::from(inner.width().saturating_sub(2))),
            Style::new().fg(FG_LIGHT).bg(shade),
        );
    }
}

fn draw_ship_readout(surface: &mut Surface<'_>, inner: Rect, sys: usize, idx: usize) {
    if inner.width() < 6 {
        return;
    }
    let bg = WINDOW_BG;
    let class = ship_class(sys, idx);
    surface.print(
        (inner.left(), inner.top()),
        truncate(class, usize::from(inner.width())),
        Style::new().fg(FG_LIGHT).bg(bg),
    );

    let bar_w = inner.width().saturating_sub(8).max(1);
    if inner.height() > 2 {
        let hull = ship_hull(sys, idx);
        surface.print(
            (inner.left(), inner.top() + 2),
            "Hull  ",
            Style::new().fg(DIM_FG).bg(bg),
        );
        panel::bar(
            surface,
            (inner.left() + 6, inner.top() + 2),
            bar_w,
            hull,
            panel::threshold(hull),
            rgb(30, 30, 36),
        );
    }
    if inner.height() > 3 {
        let shield = ship_shield(sys, idx);
        surface.print(
            (inner.left(), inner.top() + 3),
            "Shield",
            Style::new().fg(DIM_FG).bg(bg),
        );
        panel::bar(
            surface,
            (inner.left() + 6, inner.top() + 3),
            bar_w,
            shield,
            panel::threshold(shield),
            rgb(30, 30, 36),
        );
    }
    if inner.height() > 5 {
        let text = format!("Crew: {}", ship_crew(sys, idx));
        surface.print(
            (inner.left(), inner.top() + 5),
            truncate(&text, usize::from(inner.width())),
            Style::new().fg(FG_LIGHT).bg(bg),
        );
    }
}

fn draw_droid(surface: &mut Surface<'_>, rect: Rect) {
    if rect.height() < 4 || rect.width() < 5 {
        return;
    }
    let y0 = rect.bottom() - 3;
    let x0 = rect.left() + rect.width().saturating_sub(4) / 2;
    let color = rgb(210, 70, 60);
    surface.print((x0, y0), " O ", Style::new().fg(color).bg(BEZEL_BG));
    surface.print((x0, y0 + 1), "/|\\", Style::new().fg(color).bg(BEZEL_BG));
    surface.print((x0, y0 + 2), "/ \\", Style::new().fg(color).bg(BEZEL_BG));
}

/// The window's screen rect, converting its desk-relative position and
/// clamping its footprint so it never draws past the desk it lives in.
fn window_rect(win: &Window, desk: Rect) -> Rect {
    let x = desk.left() + win.pos.0.max(0) as u16;
    let y = desk.top() + win.pos.1.max(0) as u16;
    let w = win.size.0.min(desk.right().saturating_sub(x));
    let h = if win.minimized {
        1
    } else {
        win.size.1.min(desk.bottom().saturating_sub(y))
    };
    Rect::new(x, y, w, h)
}

/// The content rect inside a window's border: one row down for the title,
/// one column in from each side, one row up from the bottom border. Shared by
/// [`draw_window_content`] and [`window_hotspots`] so the drawn rows and the
/// tappable rows are always the same rows.
const fn window_inner(rect: Rect) -> Rect {
    if rect.width() < 3 || rect.height() < 3 {
        return Rect::new(rect.left(), rect.top(), 0, 0);
    }
    Rect::new(
        rect.left() + 1,
        rect.top() + 1,
        rect.width() - 2,
        rect.height() - 2,
    )
}

/// Registers every tappable region of one window: the catch-all body, its
/// content rows, the title-drag strip, and the two buttons -- in that order,
/// so a control nearer the top-right corner (a button) wins over a broader
/// region under it (the title drag strip, or the body) wherever their grown
/// touch targets overlap. See [`ascii_tile_demos::ui::touch::tappable`] for
/// why a 1-row title bar needs growing at all.
fn window_hotspots(hotspots: &mut Hotspots<Action>, rect: Rect, win: &Window) {
    if rect.width() < 4 || rect.height() == 0 {
        return;
    }
    hotspots.push(rect, Action::Body(win.id));

    if !win.minimized {
        let inner = window_inner(rect);
        let rows = window_rows(&win.kind);
        for i in 0..rows.len() {
            let y = inner.top() + i as u16 * ROW_H;
            if y >= inner.bottom() {
                break;
            }
            let h = ROW_H.min(inner.bottom() - y);
            let row_rect = Rect::new(inner.left(), y, inner.width(), h);
            hotspots.push_tappable(row_rect, rect, Action::Row(win.id, i as u8));
        }
    }

    let (min_x, close_x) = title_button_x(rect);
    let drag_w = min_x.saturating_sub(rect.left() + 1);
    let drag_rect = Rect::new(rect.left() + 1, rect.top(), drag_w, 1);
    hotspots.push_tappable(drag_rect, rect, Action::TitleBar(win.id));
    hotspots.push_tappable(
        Rect::new(min_x, rect.top(), 1, 1),
        rect,
        Action::Minimize(win.id),
    );
    hotspots.push_tappable(
        Rect::new(close_x, rect.top(), 1, 1),
        rect,
        Action::Close(win.id),
    );
}

/// The desired size for a freshly opened window of `kind`, before it is
/// clamped to `desk`. Sized to its own content (a Sector with three systems
/// needs three rows; a Ship needs its fixed readout) rather than one constant
/// for every kind, so a short Planet window does not waste space matching a
/// tall Fleet window.
fn window_size(kind: &Kind, desk: Rect, shape: Shape) -> (u16, u16) {
    let rows = match *kind {
        Kind::Sector(i) => sector_systems(i).len() as u16,
        Kind::Planet(_) => 1,
        Kind::Fleet(sys) => ship_count(sys) as u16,
        Kind::Ship(..) => 0,
    };
    let content_h = if matches!(kind, Kind::Ship(..)) {
        SHIP_CONTENT_H
    } else {
        rows * ROW_H
    };
    let desired_h = content_h + 2;
    let min_h = WIN_MIN_H.min(desk.height().max(1));
    let max_h = desk.height().saturating_sub(1).max(min_h);
    let h = desired_h.clamp(min_h, max_h);

    let min_w = WIN_MIN_W.min(desk.width().max(1));
    let max_w = desk.width().saturating_sub(1).max(min_w);
    let w = match shape {
        Shape::Portrait => max_w,
        _ => WIN_DESKTOP_W.clamp(min_w, max_w),
    };
    (w, h)
}

/// A cascaded position for the `n`-th window opened, so windows opened in
/// sequence (galaxy -> sector -> planet -> fleet -> ship) land staggered
/// rather than stacked exactly on top of each other, which is both how a real
/// window manager opens new windows and what makes the overlap the z-order
/// exists to resolve visible without the player having to drag anything.
fn cascade_pos(desk: Rect, size: (u16, u16), n: usize) -> (i32, i32) {
    let max_x = i32::from(desk.width())
        .saturating_sub(i32::from(size.0))
        .max(0);
    let max_y = i32::from(desk.height())
        .saturating_sub(i32::from(size.1))
        .max(0);
    let step = 3;
    let x = if max_x > 0 {
        (n as i32 * step) % (max_x + 1)
    } else {
        0
    };
    let y = if max_y > 0 {
        (n as i32 * step) % (max_y + 1)
    } else {
        0
    };
    (x, y)
}

const fn global_system_idx(sector: usize, local: usize) -> usize {
    sector * 3 + local
}

fn sector_systems(sector: usize) -> &'static [&'static str] {
    let start = sector * 3;
    &SYSTEMS[start..start + 3]
}

/// Ships in the fleet at `sys`: 2, 3, or 4, varied by system so not every
/// Fleet window looks identical.
const fn ship_count(sys: usize) -> usize {
    2 + sys % 3
}

const fn ship_class(sys: usize, idx: usize) -> &'static str {
    SHIP_CLASSES[(sys + idx) % SHIP_CLASSES.len()]
}

const fn ship_hull_number(sys: usize, idx: usize) -> u32 {
    (sys as u32 + idx as u32) % 4 + 1
}

fn ship_row_label(sys: usize, idx: usize) -> String {
    format!("{} {}", ship_class(sys, idx), ship_hull_number(sys, idx))
}

/// A Ship window's title, folding in the fleet number so two ships of the
/// same class in different fleets never carry the same title on screen at
/// once (every [`FLEET_NUMBER`] entry is distinct, so this is always unique).
fn ship_window_title(sys: usize, idx: usize) -> String {
    format!("{} (Fleet {})", ship_row_label(sys, idx), FLEET_NUMBER[sys])
}

fn ship_hull(sys: usize, idx: usize) -> f32 {
    0.6f32.mul_add(hash01(0xA17A, sys as i32, idx as i32), 0.35)
}

fn ship_shield(sys: usize, idx: usize) -> f32 {
    0.7f32.mul_add(hash01(0x517E, sys as i32 + 17, idx as i32), 0.2)
}

fn ship_crew(sys: usize, idx: usize) -> u32 {
    400.0f32.mul_add(hash01(0x9C0D, sys as i32, idx as i32), 40.0) as u32
}

fn window_rows(kind: &Kind) -> Vec<String> {
    match *kind {
        Kind::Sector(i) => sector_systems(i).iter().map(|s| (*s).to_string()).collect(),
        Kind::Planet(sys) => vec![format!("Fleet {} in orbit", FLEET_NUMBER[sys])],
        Kind::Fleet(sys) => (0..ship_count(sys))
            .map(|k| ship_row_label(sys, k))
            .collect(),
        Kind::Ship(..) => Vec::new(),
    }
}

impl Demo for StarConsole {
    const NAME: &'static str = "51_star_console";
    const TITLE: &'static str = "Star Console";
    const BLURB: &'static str =
        "Star Wars Rebellion: a draggable, closable, z-stacking window manager with drill-down.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("drag titlebar", "move window"),
            ("tap", "raise / open / close / minimize"),
            ("Tab", "cycle windows"),
            ("arrows", "nudge top window"),
            ("C / M", "close / minimize top window"),
            ("1/2/3", "open a sector"),
        ]
    }

    fn init<B: Backend>(term: &mut Terminal<B>) -> Self {
        let mut demo = Self::default();
        let area = term.area();
        let (_, content, _) = ui::split_chrome(area);
        let shape = Shape::of(content);
        let regions = Regions::compute(content, shape);
        // Seed two overlapping windows (a sector list and the planet it was
        // drilled into) so the console reads as a window manager from the
        // very first frame, before anyone has tapped anything.
        demo.open_sector(0, regions.desk, shape);
        demo.open_or_raise(Kind::Planet(0), SYSTEMS[0].to_string(), regions.desk, shape);
        demo.open_or_raise(
            Kind::Fleet(0),
            format!("Fleet {}", FLEET_NUMBER[0]),
            regions.desk,
            shape,
        );
        demo
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);

        let area = term.area();
        let (title, content, status) = ui::split_chrome(area);
        let shape = Shape::of(content);
        let regions = Regions::compute(content, shape);

        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code, regions.desk, shape);
            }
        }

        self.clamp_windows(regions.desk, shape);

        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(BG_DESK));

        self.hotspots.clear();
        if regions.counters.height() > 0 {
            self.draw_counters(&mut surface, regions.counters);
        }
        if regions.console.height() > 0 {
            self.draw_console(&mut surface, regions.console);
        }
        if regions.left.width() > 0 {
            self.draw_left_bezel(&mut surface, regions.left);
        }
        if regions.right.width() > 0 {
            self.draw_right_rail(&mut surface, regions.right);
        }
        if regions.action_bar.height() > 0 {
            self.draw_action_bar(&mut surface, regions.action_bar);
        }
        self.draw_galaxy(&mut surface, regions.desk);
        self.draw_windows(&mut surface, regions.desk);

        let gesture = self.pointer.take();
        self.handle_gesture(&gesture, regions.desk, shape);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(StarConsole);

#[cfg(test)]
mod tests {
    use super::{
        FLEET_NUMBER, Kind, SECTOR_NAMES, SYSTEMS, Window, draw_window, ship_count,
        ship_window_title, window_rect,
    };
    use retroglyph_core::{Grid, Rect, Surface, Tile};
    use std::collections::HashSet;

    fn window(id: u32, kind: Kind, pos: (i32, i32), size: (u16, u16)) -> Window {
        Window {
            id,
            title: format!("Window {id}"),
            kind,
            pos,
            size,
            minimized: false,
        }
    }

    /// The technical showpiece the task calls out explicitly: raising a
    /// window must change what is composited at a cell the two windows
    /// share. Draws the same two overlapping windows twice, with the z-order
    /// swapped the second time, and asserts the overlap cell differs.
    #[test]
    fn raising_a_window_changes_the_composited_cell() {
        let desk = Rect::new(0, 0, 40, 20);
        let a = window(1, Kind::Sector(0), (0, 0), (20, 10));
        let b = window(2, Kind::Sector(1), (5, 2), (20, 10));
        let overlap = (10u16, 2u16); // inside both rects, on b's title row.

        let mut lower_raised = Grid::new(40, 20);
        {
            let mut surface = Surface::new(&mut lower_raised, desk, 0);
            draw_window(&mut surface, window_rect(&a, desk), &a, false);
            draw_window(&mut surface, window_rect(&b, desk), &b, true);
        }
        let glyph_lower_raised = lower_raised.tile(0, overlap).map(Tile::glyph);

        let mut upper_raised = Grid::new(40, 20);
        {
            let mut surface = Surface::new(&mut upper_raised, desk, 0);
            draw_window(&mut surface, window_rect(&b, desk), &b, false);
            draw_window(&mut surface, window_rect(&a, desk), &a, true);
        }
        let glyph_upper_raised = upper_raised.tile(0, overlap).map(Tile::glyph);

        assert_ne!(
            glyph_lower_raised, glyph_upper_raised,
            "raising a window must change what is drawn at a cell the two windows share"
        );
    }

    #[test]
    fn every_sector_and_system_name_is_unique() {
        let sectors: HashSet<_> = SECTOR_NAMES.iter().collect();
        assert_eq!(sectors.len(), SECTOR_NAMES.len(), "duplicate sector name");
        let systems: HashSet<_> = SYSTEMS.iter().collect();
        assert_eq!(systems.len(), SYSTEMS.len(), "duplicate system name");
    }

    #[test]
    fn every_fleet_number_is_unique() {
        let numbers: HashSet<_> = FLEET_NUMBER.iter().collect();
        assert_eq!(numbers.len(), FLEET_NUMBER.len(), "duplicate fleet number");
    }

    #[test]
    fn every_ship_window_title_is_unique() {
        let mut titles = HashSet::new();
        for sys in 0..SYSTEMS.len() {
            for idx in 0..ship_count(sys) {
                let title = ship_window_title(sys, idx);
                assert!(
                    titles.insert(title.clone()),
                    "duplicate ship window title: {title}"
                );
            }
        }
    }
}
