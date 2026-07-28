//! 68: Mage Thrones -- Lords of Magic: Special Edition's nested party roster,
//! where the grouping *is* the mechanic.
//!
//! A Lords of Magic party is not a flat list. It holds up to three champions
//! and nine units, but they are not siblings: each champion opens a "unit
//! window" -- itself plus up to three units under it -- and a party can hold
//! at most three such windows. The grouping is not cosmetic. When a battle
//! starts, every window deploys as its own cluster: the units grouped under
//! a champion arrive standing next to that champion, and units in a
//! different window arrive somewhere else on the field entirely. Moving a
//! unit between windows before the battle is therefore a real tactical
//! decision, not a roster tidy-up.
//!
//! This gallery already has two demos about Lords of Magic's other visible
//! feature, the eight-colour faith identity that flies over every party and
//! building (`25_flag_war`, `37_faith_war`). Faith colour carries no other
//! information in the source material -- there is no per-faith sigil to
//! draw, only the banner colour -- so a third coloured-factions-on-a-map demo
//! would repeat both. What neither of those demos has is a container with
//! capacity rules whose *arrangement* changes an outcome, so that is what
//! this one builds: the roster on the left, and a formation preview on the
//! right that answers the question the roster alone cannot -- "if the horn
//! sounded right now, where would everyone actually stand?"
//!
//! Techniques on show:
//!
//! - **A two-level container with capacity rules**
//!   ([`MageThrones::move_unit`]): three windows, each a champion slot plus
//!   up to three unit slots. A move that would violate either limit is
//!   refused and reported in the status line rather than silently dropped,
//!   so illegal moves are as visible as legal ones -- the same "attempted
//!   but refused" convention `34_ice_breach` uses for its own illegal moves.
//! - **Grouping as a rendered consequence, not just a rule**
//!   ([`MageThrones::draw_formation`]): the formation preview lays out one
//!   cluster per occupied window, so moving a unit across a window boundary
//!   visibly relocates its whole cluster instead of only changing a list
//!   position. This is the panel the brief calls for: the point of the demo
//!   is that the grouping and its consequence are on screen together.
//! - **[`ui::touch::Shape`]-driven reflow** ([`MageThrones::draw`]): desktop
//!   and landscape show roster and formation side by side; portrait and the
//!   80x24 headless grid stack them, since three ornamented panels do not
//!   fit one above the other in a phone's width.
//! - **Faith colour as a one-channel palette** ([`Faith::color`]): applied to
//!   frames and glyphs, never asked to distinguish anything else -- exactly
//!   the amount of information the research found it actually carries.
//!
//! Sources: the Lords of Magic manual's party-composition section (pp.
//! 63-64) and the accompanying research brief describe unit windows as one
//! champion in the first slot plus up to three units, with the window
//! deciding starting combat position; see also `RogueBasin`'s general notes
//! on roster-driven deployment in tactical RPGs.
//!
//! ```sh
//! cargo run --example 68_mage_thrones --features crossterm
//! cargo run --example 68_mage_thrones --features software
//! cargo run --example 68_mage_thrones --features gl
//! cargo run --example 68_mage_thrones  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::palette::{mix, rgb, scale};

/// Windows a party can hold. The manual's own ceiling: a fourth champion (or
/// a fourth unit in a full window) has nowhere to go and simply cannot be
/// added.
const WINDOWS: usize = 3;
/// Unit slots under each window's champion.
const UNIT_SLOTS: usize = 3;
/// Total unit slots across the whole party, for the roster-wide `alive/total`
/// readout in the status line.
const TOTAL_UNIT_SLOTS: usize = WINDOWS * UNIT_SLOTS;

/// How often the roster reroutes a unit on its own, so an unattended screen
/// still demonstrates the move-and-refuse mechanic.
const AUTO_MOVE_SECONDS: f32 = 3.0;

/// One of Lords of Magic's eight faiths, standing in for the whole party's
/// allegiance. The manual documents faith almost entirely as a banner
/// colour with no separate sigil, so that is the only thing this type
/// carries: see the module doc for why a fuller faith model would just
/// repeat `25_flag_war`/`37_faith_war`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Faith {
    Life,
    Fire,
    Water,
}

impl Faith {
    const fn color(self) -> Color {
        match self {
            // Yellow: Life's banner colour in the manual.
            Self::Life => rgb(214, 186, 90),
            // Red: Fire's.
            Self::Fire => rgb(198, 84, 64),
            // Dark blue: Water's.
            Self::Water => rgb(88, 118, 196),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Life => "Life",
            Self::Fire => "Fire",
            Self::Water => "Water",
        }
    }
}

/// A champion: the one member that can open a window at all.
#[derive(Clone, Copy)]
struct Champion {
    name: &'static str,
    class: &'static str,
}

/// A rank-and-file unit, always living inside some window's unit slots.
#[derive(Clone, Copy)]
struct Unit {
    name: &'static str,
    /// Living members out of a nominal three-strong unit, the "2/3" readout
    /// the research brief calls out.
    alive: u8,
    /// Experience marks, drawn as tick marks under the icon. Capped at 4 so
    /// the marker row never grows past what a slot can show.
    marks: u8,
}

impl Unit {
    const fn max_strength() -> u8 {
        3
    }
}

/// One of a party's up to three unit windows: a champion in the first slot,
/// up to [`UNIT_SLOTS`] units after it. An empty window (no champion) exists
/// only as the destination for a move that opens a new window; the roster
/// never starts with one.
struct Window {
    champion: Option<Champion>,
    units: [Option<Unit>; UNIT_SLOTS],
}

impl Window {
    fn is_full(&self) -> bool {
        self.units.iter().all(Option::is_some)
    }

    fn first_empty_slot(&self) -> Option<usize> {
        self.units.iter().position(Option::is_none)
    }

    fn unit_count(&self) -> usize {
        self.units.iter().filter(|u| u.is_some()).count()
    }
}

/// Where one unit currently sits: which window, which of its
/// [`UNIT_SLOTS`] slots.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot {
    window: usize,
    unit: usize,
}

/// What tapping a roster cell means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    SelectUnit(Slot),
    /// Tap the destination window's champion row to move the selected unit
    /// into that window's first empty slot.
    TargetWindow(usize),
}

/// State: the party (three windows of champion + units), the selection used
/// to drive a move, the auto-move clock, and the touch/keyboard plumbing
/// every interface demo in this gallery shares.
pub struct MageThrones {
    faith: Faith,
    windows: [Window; WINDOWS],
    selected: Option<Slot>,
    /// Most recent move attempt, legal or not, shown in the status line.
    last_move: String,
    time: f32,
    auto_timer: f32,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

impl Default for MageThrones {
    fn default() -> Self {
        let champions = [
            Champion {
                name: "Iolanthe",
                class: "Mage",
            },
            Champion {
                name: "Baldric",
                class: "Warrior",
            },
            Champion {
                name: "Sable",
                class: "Thief",
            },
        ];
        // Seeded unevenly on purpose: window 0 starts full (so the first
        // auto-move has to reroute out of it rather than into it), window 2
        // starts with one empty slot and no champion mismatch, and one unit
        // starts already below full strength so the alive/total readout has
        // something to say from frame one.
        let windows = [
            Window {
                champion: Some(champions[0]),
                units: [
                    Some(Unit {
                        name: "Salamanders",
                        alive: 3,
                        marks: 2,
                    }),
                    Some(Unit {
                        name: "Fire Drakes",
                        alive: 2,
                        marks: 3,
                    }),
                    Some(Unit {
                        name: "Pyromancers",
                        alive: 3,
                        marks: 1,
                    }),
                ],
            },
            Window {
                champion: Some(champions[1]),
                units: [
                    Some(Unit {
                        name: "Phalanx",
                        alive: 3,
                        marks: 1,
                    }),
                    None,
                    None,
                ],
            },
            Window {
                champion: Some(champions[2]),
                units: [
                    Some(Unit {
                        name: "Footpads",
                        alive: 3,
                        marks: 0,
                    }),
                    Some(Unit {
                        name: "Sappers",
                        alive: 2,
                        marks: 2,
                    }),
                    None,
                ],
            },
        ];

        Self {
            faith: Faith::Fire,
            windows,
            selected: None,
            last_move: "select a unit, then a window to move it".to_owned(),
            time: 0.0,
            auto_timer: AUTO_MOVE_SECONDS,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl MageThrones {
    /// Moves the unit at `from` into `to`'s first empty unit slot. Refuses
    /// (leaving the roster untouched, and saying why in `last_move`) if the
    /// source is empty, the destination window has no champion of its own,
    /// or the destination window is already at [`UNIT_SLOTS`]. This is the
    /// whole mechanic: a party is not a flat list you can rearrange freely,
    /// it is three capacity-limited groups, and the group boundary a unit
    /// lands on is what decides where it starts a battle.
    fn move_unit(&mut self, from: Slot, to_window: usize) {
        let Some(unit) = self.windows[from.window].units[from.unit] else {
            "no unit in that slot".clone_into(&mut self.last_move);
            return;
        };
        if self.windows[to_window].champion.is_none() {
            self.last_move = format!("window {} has no champion to lead it", to_window + 1);
            return;
        }
        if from.window == to_window {
            self.last_move = format!("{} is already in that window", unit.name);
            return;
        }
        let Some(dest_slot) = self.windows[to_window].first_empty_slot() else {
            self.last_move = format!(
                "window {} is full ({UNIT_SLOTS}/{UNIT_SLOTS}) -- move refused",
                to_window + 1
            );
            return;
        };

        self.windows[from.window].units[from.unit] = None;
        self.windows[to_window].units[dest_slot] = Some(unit);
        self.selected = Some(Slot {
            window: to_window,
            unit: dest_slot,
        });
        self.last_move = format!(
            "{} moved into window {} -- deploys beside {}",
            unit.name,
            to_window + 1,
            self.windows[to_window]
                .champion
                .map_or("nobody", |c| c.name),
        );
    }

    /// Picks a unit and a legal destination window at random-by-clock and
    /// moves it, so an unattended screen still shows the refusal/consequence
    /// loop the demo exists to demonstrate. Skips silently on a tick where
    /// no legal move exists (every window full or only one window occupied),
    /// rather than forcing an illegal one just to have something to show.
    fn auto_move(&mut self) {
        let tick = self.time as u32;
        for offset in 0..(WINDOWS * UNIT_SLOTS) {
            let idx = (tick as usize + offset) % (WINDOWS * UNIT_SLOTS);
            let from = Slot {
                window: idx / UNIT_SLOTS,
                unit: idx % UNIT_SLOTS,
            };
            if self.windows[from.window].units[from.unit].is_none() {
                continue;
            }
            for step in 1..WINDOWS {
                let to = (from.window + step) % WINDOWS;
                if self.windows[to].champion.is_some() && !self.windows[to].is_full() {
                    self.selected = Some(from);
                    self.move_unit(from, to);
                    return;
                }
            }
        }
    }

    fn simulate(&mut self, dt: f32) {
        self.time += dt;
        self.auto_timer -= dt;
        if self.auto_timer <= 0.0 {
            self.auto_timer = AUTO_MOVE_SECONDS;
            self.auto_move();
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.selected = Some(self.next_occupied_slot()),
            KeyCode::Char(c @ '1'..='3') => {
                let window = (c as u8 - b'1') as usize;
                if let Some(from) = self.selected {
                    self.move_unit(from, window);
                } else {
                    "select a unit first (Tab)".clone_into(&mut self.last_move);
                }
            }
            KeyCode::Char('r' | 'R') => {
                self.faith = match self.faith {
                    Faith::Life => Faith::Fire,
                    Faith::Fire => Faith::Water,
                    Faith::Water => Faith::Life,
                };
            }
            _ => {}
        }
    }

    fn next_occupied_slot(&self) -> Slot {
        let start = self
            .selected
            .map_or(0, |s| s.window * UNIT_SLOTS + s.unit + 1);
        for offset in 0..TOTAL_UNIT_SLOTS {
            let idx = (start + offset) % TOTAL_UNIT_SLOTS;
            let slot = Slot {
                window: idx / UNIT_SLOTS,
                unit: idx % UNIT_SLOTS,
            };
            if self.windows[slot.window].units[slot.unit].is_some() {
                return slot;
            }
        }
        Slot { window: 0, unit: 0 }
    }

    fn apply_gesture(&mut self, gesture: &Gesture) {
        let Some(pos) = gesture.tap else { return };
        let Some(&action) = self.hotspots.hit(pos) else {
            return;
        };
        match action {
            Action::SelectUnit(slot) => self.selected = Some(slot),
            Action::TargetWindow(window) => {
                if let Some(from) = self.selected {
                    self.move_unit(from, window);
                } else {
                    "select a unit first, then tap a window".clone_into(&mut self.last_move);
                }
            }
        }
    }

    fn status_text(&self) -> String {
        let mut alive = 0u32;
        let mut total = 0u32;
        for window in &self.windows {
            for unit in window.units.iter().flatten() {
                alive += u32::from(unit.alive);
                total += u32::from(Unit::max_strength());
            }
        }
        let windows_open = self.windows.iter().filter(|w| w.champion.is_some()).count();
        format!(
            "{} party  {windows_open}/{WINDOWS} windows open  {alive}/{total} strength  {}",
            self.faith.label(),
            self.last_move
        )
    }

    // -- Layout ----------------------------------------------------------

    fn draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(content);
        // Two panels side by side need roughly a roster's worth of columns
        // plus a formation's worth; below that (every portrait shape, and
        // the 80x24 headless grid) they stack, same convention `46` and `55`
        // use for their own two-panel layouts.
        let stacked = shape.stacks() || content.width() < 70;

        if stacked {
            let (roster_area, formation_area) = panel::split_top(content, content.height() * 3 / 5);
            self.draw_roster(surface, roster_area);
            self.draw_formation(surface, formation_area);
        } else {
            let (roster_area, formation_area) = panel::split_left(content, content.width() * 3 / 5);
            self.draw_roster(surface, roster_area);
            self.draw_formation(surface, formation_area);
        }
    }

    /// The roster: one bordered sub-panel per window, a champion header row,
    /// then up to [`UNIT_SLOTS`] unit rows. Selection and the empty "no
    /// champion" window are drawn distinctly so the capacity rule that
    /// [`Self::move_unit`] enforces is visible before a move is ever
    /// attempted, not only when one is refused.
    fn draw_roster(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let outer = Panel::new()
            .title("Party Roster")
            .badge(&format!("{} party", self.faith.label()))
            .border(Border::Double)
            .frame(self.faith.color())
            .draw(surface, area);
        if outer.height() == 0 {
            return;
        }

        let win_h = (outer.height() / WINDOWS as u16).max(1);
        for w in 0..self.windows.len() {
            let y0 = outer.top() + w as u16 * win_h;
            if y0 >= outer.bottom() {
                break;
            }
            let rect = Rect::new(
                outer.left(),
                y0,
                outer.width(),
                win_h.min(outer.bottom() - y0),
            );
            self.draw_window(surface, rect, w);
        }
    }

    /// Draws window `index` by looking it up from `self.windows` itself
    /// rather than taking a `&Window` parameter: this method also needs
    /// `&mut self.hotspots` for its tap targets, and a borrowed parameter
    /// tied to `self.windows` would conflict with that mutable borrow for
    /// the whole call.
    fn draw_window(&mut self, surface: &mut Surface<'_>, rect: Rect, index: usize) {
        let accent = self.faith.color();
        let (champion, units, unit_count) = {
            let window = &self.windows[index];
            (window.champion, window.units, window.unit_count())
        };
        let inner = Panel::new()
            .title(&format!("Window {}", index + 1))
            .badge(&format!("{unit_count}/{UNIT_SLOTS}"))
            .frame(scale(accent, 0.75))
            .draw(surface, rect);
        if inner.height() == 0 {
            return;
        }
        // The header row doubles as a tap target for "move the selected unit
        // here": tapping a champion is how the demo asks "put it under this
        // leader", which is exactly what a window boundary means.
        let header = Rect::new(inner.left(), inner.top(), inner.width(), 1);
        Self::draw_champion_row(surface, header, champion, accent);
        self.hotspots.push(header, Action::TargetWindow(index));

        for (u, unit) in units.iter().enumerate() {
            let y = inner.top() + 1 + u as u16;
            if y >= inner.bottom() {
                break;
            }
            let row = Rect::new(inner.left(), y, inner.width(), 1);
            let slot = Slot {
                window: index,
                unit: u,
            };
            self.draw_unit_row(surface, row, unit.as_ref(), slot);
            if unit.is_some() {
                self.hotspots.push(row, Action::SelectUnit(slot));
            }
        }
    }

    fn draw_champion_row(
        surface: &mut Surface<'_>,
        rect: Rect,
        champion: Option<Champion>,
        accent: Color,
    ) {
        let bg = panel::PANEL_BG;
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let Some(champion) = champion else {
            surface.print(
                (rect.left(), rect.top()),
                "(no champion -- cannot receive units)",
                Style::new().fg(ui::DIM).bg(bg),
            );
            return;
        };
        panel::spans(
            surface,
            (rect.left(), rect.top()),
            rect.width(),
            &[
                Span::new(champion.name, accent),
                Span::plain(" the "),
                Span::new(champion.class, ui::FG),
            ],
            bg,
        );
    }

    fn draw_unit_row(
        &self,
        surface: &mut Surface<'_>,
        rect: Rect,
        unit: Option<&Unit>,
        slot: Slot,
    ) {
        let selected = self.selected == Some(slot);
        let base_bg = panel::PANEL_BG;
        let bg = if selected {
            mix(base_bg, ui::ACCENT, 0.28)
        } else {
            base_bg
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        let Some(unit) = unit else {
            surface.print(
                (rect.left() + 2, rect.top()),
                "-- empty slot --",
                Style::new().fg(ui::DIM).bg(bg),
            );
            return;
        };

        let marker = if selected { '>' } else { ' ' };
        surface.put(
            (rect.left(), rect.top()),
            marker,
            Style::new().fg(ui::ACCENT).bg(bg),
        );

        let strength = format!("{}/{}", unit.alive, Unit::max_strength());
        let strength_color =
            panel::threshold(f32::from(unit.alive) / f32::from(Unit::max_strength()));
        panel::spans(
            surface,
            (rect.left() + 2, rect.top()),
            rect.width().saturating_sub(2),
            &[
                Span::new(unit.name, if selected { ui::ACCENT } else { ui::FG }),
                Span::plain(" "),
                Span::new(&strength, strength_color),
            ],
            bg,
        );

        // Experience marks, right-aligned: a run of tick glyphs, one per
        // mark, matching the research brief's "white line marks beneath
        // each unit icon".
        let marks: String = "\u{2502}".repeat(usize::from(unit.marks));
        if rect.width() > 8 {
            let x = rect
                .right()
                .saturating_sub(marks.chars().count() as u16 + 1);
            surface.print((x, rect.top()), &marks, Style::new().fg(ui::DIM).bg(bg));
        }
    }

    /// The formation preview: one cluster per occupied window, laid out
    /// left to right across the panel with the champion at the cluster's
    /// centre and its units ringed around it. This is the panel the module
    /// doc calls the point of the demo -- it renders the *consequence* of
    /// the roster's grouping, which a champion-plus-list roster alone cannot
    /// show.
    fn draw_formation(&self, surface: &mut Surface<'_>, area: Rect) {
        let accent = self.faith.color();
        let inner = Panel::new()
            .title("Formation Preview")
            .badge("on battle start")
            .border(Border::Double)
            .frame(scale(accent, 0.85))
            .bg(rgb(10, 14, 12))
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }

        let occupied: Vec<(usize, &Window)> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.champion.is_some())
            .collect();
        if occupied.is_empty() {
            surface.print(
                (inner.left(), inner.top()),
                "no champions -- nothing deploys",
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
            return;
        }

        let cluster_w = inner.width() / occupied.len() as u16;
        for (i, (window_idx, window)) in occupied.iter().enumerate() {
            let x0 = inner.left() + i as u16 * cluster_w;
            let rect = Rect::new(x0, inner.top(), cluster_w.max(1), inner.height());
            Self::draw_cluster(surface, rect, *window_idx, window, accent);
        }
    }

    fn draw_cluster(
        surface: &mut Surface<'_>,
        rect: Rect,
        window_idx: usize,
        window: &Window,
        accent: Color,
    ) {
        let bg = rgb(10, 14, 12);
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.width() == 0 || rect.height() == 0 {
            return;
        }
        let cx = rect.left() + rect.width() / 2;

        let label = format!("W{}", window_idx + 1);
        surface.print(
            (rect.left(), rect.top()),
            &label,
            Style::new().fg(ui::DIM).bg(bg),
        );

        // The champion sits dead centre of its cluster: the point being made
        // is that whatever units share this window deploy clustered around
        // this glyph, not scattered across the field.
        let champion_y = rect.top() + 1;
        surface.put((cx, champion_y), '\u{263C}', Style::new().fg(accent).bg(bg));

        // Units ring the champion in a small arc below it, closest slot
        // first, so a unit that was just moved in visibly lands beside its
        // new leader rather than at some arbitrary map coordinate.
        let ring_y = champion_y + 1;
        let present: Vec<&Unit> = window.units.iter().flatten().collect();
        let n = present.len() as u16;
        if n > 0 && ring_y < rect.bottom() {
            let span = (n - 1) * 2;
            let start_x = cx.saturating_sub(span / 2);
            for (i, unit) in present.iter().enumerate() {
                let x = start_x + i as u16 * 2;
                if x >= rect.right() {
                    continue;
                }
                // Full strength keeps the accent's full brightness; a
                // depleted unit (the research brief's "2/3" readout) dims
                // toward the panel background instead of swapping glyphs,
                // since the outline suit glyphs (♤ etc.) that would read
                // as "the same shape, hollowed out" are not in CP437 and
                // would draw as a solid block on the pixel backends.
                let full = unit.alive == Unit::max_strength();
                let color = if full { accent } else { scale(accent, 0.45) };
                surface.put((x, ring_y), '\u{2660}', Style::new().fg(color).bg(bg));
            }
        }

        if let Some(champion) = window.champion
            && rect.height() > 3
        {
            let name_y = ring_y + 1;
            if name_y < rect.bottom() {
                let text = center_pad(champion.name, rect.width());
                surface.print((rect.left(), name_y), &text, Style::new().fg(ui::FG).bg(bg));
            }
        }
    }
}

/// Centers `text` in `width` columns, truncating rather than panicking if it
/// does not fit.
fn center_pad(text: &str, width: u16) -> String {
    let text = retroglyph_widgets::truncate(text, usize::from(width));
    let used = text.chars().count() as u16;
    let pad = (width.saturating_sub(used)) / 2;
    format!("{}{}", " ".repeat(usize::from(pad)), text)
}

impl Demo for MageThrones {
    const NAME: &'static str = "68_mage_thrones";
    const TITLE: &'static str = "68 Mage Thrones";
    const BLURB: &'static str =
        "Lords of Magic's nested party roster: unit windows that decide combat deployment.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "select next unit"),
            ("1/2/3", "move selected unit to window"),
            ("R", "cycle faith colour"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();
        self.apply_gesture(&gesture);
        self.simulate(dt);

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let status_text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &status_text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(MageThrones);

#[cfg(test)]
mod tests {
    use super::{Faith, MageThrones, Slot, UNIT_SLOTS, WINDOWS};

    #[test]
    fn moving_a_unit_into_a_champion_less_window_is_refused() {
        let mut demo = MageThrones::default();
        demo.windows[1].champion = None;
        let from = Slot { window: 0, unit: 0 };
        let before = demo.windows[0].units[0].map(|u| u.name);
        demo.move_unit(from, 1);
        assert_eq!(demo.windows[0].units[0].map(|u| u.name), before);
        assert!(demo.last_move.contains("no champion"));
    }

    #[test]
    fn moving_a_unit_into_a_full_window_is_refused() {
        let mut demo = MageThrones::default();
        // Window 0 starts full in the seeded roster.
        assert!(demo.windows[0].is_full());
        let from = Slot { window: 1, unit: 0 };
        let unit_before = demo.windows[1].units[0];
        demo.move_unit(from, 0);
        assert_eq!(
            demo.windows[1].units[0].map(|u| u.name),
            unit_before.map(|u| u.name)
        );
        assert!(demo.last_move.contains("full"));
    }

    #[test]
    fn a_legal_move_relocates_the_unit_and_frees_the_source_slot() {
        let mut demo = MageThrones::default();
        let from = Slot { window: 2, unit: 0 };
        let moved_name = demo.windows[2].units[0].unwrap().name;
        demo.move_unit(from, 1);
        assert!(demo.windows[2].units[0].is_none());
        assert!(
            demo.windows[1]
                .units
                .iter()
                .flatten()
                .any(|u| u.name == moved_name)
        );
    }

    #[test]
    fn every_window_holds_at_most_unit_slots_units() {
        let demo = MageThrones::default();
        for window in &demo.windows {
            assert!(window.unit_count() <= UNIT_SLOTS);
        }
        assert_eq!(demo.windows.len(), WINDOWS);
    }

    #[test]
    fn cycling_faith_visits_all_three_colours() {
        let mut demo = MageThrones::default();
        let mut seen = vec![demo.faith];
        for _ in 0..3 {
            demo.handle_key(retroglyph_core::event::KeyCode::Char('r'));
            seen.push(demo.faith);
        }
        assert_eq!(seen[0], Faith::Fire);
        assert_eq!(seen[3], Faith::Fire, "cycle of three returns to start");
    }
}
