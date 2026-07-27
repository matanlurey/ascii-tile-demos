//! 48: Twin Planes -- the same map coordinates, two overlaid worlds.
//!
//! Master of Magic runs its whole campaign on two planes, Arcanus and Myrror,
//! that share one coordinate system: tile `(12, 4)` on Arcanus is a real place
//! directly "above" tile `(12, 4)` on Myrror, and a single `Plane` toolbar
//! button swaps which one is on screen. Nothing else in this gallery does
//! that -- every other overworld ([`20_realm_map`](../20_realm_map),
//! [`22_overworld_quest`](../22_overworld_quest)) shows one world, so this
//! demo's entire reason to exist is making that swap unmistakable: a
//! distinct terrain vocabulary per plane, a transition that visibly sweeps
//! across the map rather than cutting, and Towers of Wizardry that sit at the
//! same coordinate on both planes and act as the toggle's in-world anchor.
//!
//! Techniques on show:
//!
//! - **One elevation field, two dressings** ([`terrain_visual`]): both planes
//!   sample the same [`tilekit::noise::fbm`] elevation/moisture fields at
//!   every world coordinate, so a coastline or a mountain range lands in the
//!   same place on both planes -- exactly the `MoM` rule that the two worlds
//!   are the same *shape* wearing different *skins*. Only the palette and
//!   glyph vocabulary change per [`Plane`], which is what lets a glance tell
//!   you which one you are looking at without reading a label.
//! - **A staggered wipe, not a cut** ([`TwinPlanes::draw_map`]): toggling the
//!   plane starts a countdown; every cell decides when it flips from old to
//!   new based on a diagonal sweep position plus a per-cell hash offset, so
//!   the transition reads as a shimmer crossing the map over ~0.6s rather
//!   than an instant palette swap.
//! - **Crossing points** ([`TOWERS`]): a handful of world coordinates are
//!   marked with a Tower of Wizardry glyph on *both* planes. Tapping one (or
//!   pressing Enter with it selected) crosses planes on the spot, which is
//!   the literal `MoM` mechanic for plane travel rather than a generic toggle.
//! - **Touch-first chrome** ([`ascii_tile_demos::ui::touch`]): the menu bar,
//!   the bottom action row, and every city/tower marker register grown
//!   [`ascii_tile_demos::ui::touch::Hotspots`] regions so a small glyph still
//!   has a full [`ascii_tile_demos::ui::touch::TAP_W`]x
//!   [`ascii_tile_demos::ui::touch::TAP_H`] hit target, and the map itself
//!   pans by drag through [`ascii_tile_demos::ui::touch::Pointer`].
//! - **Fill-the-viewport terrain**: the map has no stored grid or fixed
//!   extent to run out of. Every visible cell samples the noise fields fresh
//!   from the live panel rect, so the map covers exactly as much of the
//!   screen as the layout gives it at any [`ascii_tile_demos::ui::touch::Shape`].
//!
//! ```sh
//! cargo run --example 48_twin_planes --features crossterm
//! cargo run --example 48_twin_planes --features software
//! cargo run --example 48_twin_planes --features gl
//! cargo run --example 48_twin_planes  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::{fbm, hash01};
use tilekit::palette::{mix, rgb};

/// Frequency the elevation/moisture fields are sampled at. Small enough that
/// a screenful of cells covers several ridgelines and coastlines rather than
/// one undifferentiated slope.
const FEATURE_SCALE: f32 = 0.085;

/// How long a plane swap's wipe takes to cross the whole map, in seconds.
/// Long enough to read as motion, short enough that repeated toggling never
/// feels laggy.
const TRANSITION_SECS: f32 = 0.65;

/// Rows given to the top menu bar. [`ascii_tile_demos::ui::touch::TAP_H`] so
/// every button's *drawn* rect is already a legal touch target and its
/// hotspot never has to steal rows from the map below it.
const MENU_H: u16 = ui::touch::TAP_H;
/// Rows given to the bottom action row. See [`MENU_H`].
const BOTTOM_H: u16 = ui::touch::TAP_H;

/// World cells per minimap cell. See [`TwinPlanes::draw_minimap`].
const MINIMAP_STEP: i32 = 4;

/// One of the two overlaid worlds. Order matters for [`Plane::other`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Plane {
    /// The temperate "home" world: green lowlands, forests, grey stone.
    #[default]
    Arcanus,
    /// The chaotic "far" world: violet chaos seas, crystal peaks, brimstone
    /// plains. Same landmass shape as Arcanus, entirely different skin.
    Myrror,
}

impl Plane {
    const fn other(self) -> Self {
        match self {
            Self::Arcanus => Self::Myrror,
            Self::Myrror => Self::Arcanus,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Arcanus => "Arcanus",
            Self::Myrror => "Myrror",
        }
    }

    /// The plane's signature color, used for the toolbar button and the
    /// transition flash so the swap has a color, not just new terrain.
    const fn accent(self) -> Color {
        match self {
            Self::Arcanus => rgb(150, 210, 110),
            Self::Myrror => rgb(210, 130, 230),
        }
    }
}

/// The physical landform at one world coordinate, shared by both planes so
/// the coastline lands in the same place regardless of which one is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Land {
    Ocean,
    Coast,
    Low,
    Forest,
    Hill,
    Peak,
}

/// Samples elevation at a world coordinate.
///
/// Both planes read this same field: `MoM`'s two worlds share a landmass shape,
/// they just look and are inhabited differently, and that is the detail that
/// makes the toggle feel like "the same place" rather than "a new map".
fn elevation(wx: i32, wy: i32) -> f32 {
    fbm(
        0xA53C_11F0,
        wx as f32 * FEATURE_SCALE,
        wy as f32 * FEATURE_SCALE,
        5,
        0.55,
    )
}

/// Samples a moisture field, used only to decide forest-vs-plain within the
/// lowland elevation band.
fn moisture(wx: i32, wy: i32) -> f32 {
    fbm(
        0x77CE_9931,
        wx as f32 * FEATURE_SCALE * 1.3,
        wy as f32 * FEATURE_SCALE * 1.3,
        4,
        0.5,
    )
}

fn land_at(wx: i32, wy: i32) -> Land {
    let e = elevation(wx, wy);
    let m = moisture(wx, wy);
    if e < 0.34 {
        Land::Ocean
    } else if e < 0.40 {
        Land::Coast
    } else if e < 0.64 {
        if m > 0.52 { Land::Forest } else { Land::Low }
    } else if e < 0.82 {
        Land::Hill
    } else {
        Land::Peak
    }
}

/// The glyph and colors [`Land`] takes on the given [`Plane`].
///
/// The stop positions in [`land_at`] are shared, so a shore is a shore on
/// both planes; this table is the *only* thing that differs between them,
/// which is deliberately the whole demo. `jitter` is a per-cell hash used to
/// pick between two near-identical glyphs, breaking up flat runs of open
/// or plain terrain without needing a second noise field.
fn terrain_visual(plane: Plane, land: Land, jitter: f32) -> (char, Color, Color) {
    match (plane, land) {
        (Plane::Arcanus, Land::Ocean) => ('~', rgb(70, 120, 190), rgb(8, 22, 54)),
        (Plane::Arcanus, Land::Coast) => ('.', rgb(150, 190, 210), rgb(22, 56, 88)),
        (Plane::Arcanus, Land::Low) => {
            let g = if jitter > 0.6 { '"' } else { ',' };
            (g, rgb(126, 182, 100), rgb(18, 50, 24))
        }
        (Plane::Arcanus, Land::Forest) => ('\u{2660}', rgb(64, 132, 74), rgb(12, 34, 16)),
        (Plane::Arcanus, Land::Hill) => ('\u{2229}', rgb(172, 150, 98), rgb(46, 46, 28)),
        (Plane::Arcanus, Land::Peak) => ('\u{25B2}', rgb(228, 228, 236), rgb(58, 58, 64)),

        (Plane::Myrror, Land::Ocean) => ('\u{2248}', rgb(172, 92, 212), rgb(34, 8, 52)),
        (Plane::Myrror, Land::Coast) => ('\u{00B7}', rgb(212, 140, 232), rgb(52, 18, 72)),
        (Plane::Myrror, Land::Low) => {
            let g = if jitter > 0.6 { ';' } else { ':' };
            (g, rgb(222, 150, 78), rgb(46, 20, 10))
        }
        (Plane::Myrror, Land::Forest) => ('\u{2663}', rgb(108, 218, 168), rgb(14, 34, 26)),
        (Plane::Myrror, Land::Hill) => ('\u{2229}', rgb(218, 150, 230), rgb(40, 24, 52)),
        (Plane::Myrror, Land::Peak) => ('\u{25B2}', rgb(236, 178, 248), rgb(44, 18, 56)),
    }
}

/// Whether magic nodes ("sparklies") can appear on `land`. Oceans and bare
/// coast never carry one; they are meant to mark a resource-bearing tile.
const fn hosts_nodes(land: Land) -> bool {
    !matches!(land, Land::Ocean | Land::Coast)
}

/// A city fixed to one plane: `MoM` cities do not have a twin the way Towers
/// do, so seeing a city is itself information about which plane you must be
/// on.
struct CityDef {
    x: i32,
    y: i32,
    name: &'static str,
    plane: Plane,
    owner: Owner,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Player,
    Rival,
}

impl Owner {
    const fn color(self) -> Color {
        match self {
            Self::Player => rgb(110, 160, 230),
            Self::Rival => rgb(220, 100, 100),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Player => "yours",
            Self::Rival => "rival",
        }
    }
}

/// Hand-placed rather than generated: six cities, unique names, a mix of
/// owners and planes, close enough together that a default camera position
/// sees several at once on a desktop-sized viewport.
const CITIES: &[CityDef] = &[
    CityDef {
        x: 4,
        y: 3,
        name: "Corwyn",
        plane: Plane::Arcanus,
        owner: Owner::Player,
    },
    CityDef {
        x: 20,
        y: 9,
        name: "Duncastle",
        plane: Plane::Arcanus,
        owner: Owner::Player,
    },
    CityDef {
        x: 13,
        y: -5,
        name: "Ashvale",
        plane: Plane::Arcanus,
        owner: Owner::Rival,
    },
    CityDef {
        x: 7,
        y: 7,
        name: "Vrask Hold",
        plane: Plane::Myrror,
        owner: Owner::Player,
    },
    CityDef {
        x: 22,
        y: 1,
        name: "Zurn Spire",
        plane: Plane::Myrror,
        owner: Owner::Rival,
    },
    CityDef {
        x: 15,
        y: 13,
        name: "K'thal Reach",
        plane: Plane::Myrror,
        owner: Owner::Player,
    },
];

/// A crossing point: the same world coordinate marked on *both* planes, per
/// the `MoM` Tower of Wizardry mechanic this demo builds around.
struct TowerDef {
    x: i32,
    y: i32,
    name: &'static str,
}

const TOWERS: &[TowerDef] = &[
    TowerDef {
        x: 12,
        y: 4,
        name: "Tower of the Veil",
    },
    TowerDef {
        x: 19,
        y: -2,
        name: "Tower of Storms",
    },
];

/// A menu-bar entry. Only [`Self::Plane`] does anything mechanically; the
/// rest exist because the reference screenshot's chrome is part of what makes
/// this read as Master of Magic, and each still needs to be a real, tappable
/// control with keyboard parity, not a decal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Game,
    Spells,
    Armies,
    Cities,
    Magic,
    Info,
    Plane,
}

const MENU_ITEMS: &[MenuItem] = &[
    MenuItem::Game,
    MenuItem::Spells,
    MenuItem::Armies,
    MenuItem::Cities,
    MenuItem::Magic,
    MenuItem::Info,
    MenuItem::Plane,
];

impl MenuItem {
    const fn label(self) -> &'static str {
        match self {
            Self::Game => "Game",
            Self::Spells => "Spells",
            Self::Armies => "Armies",
            Self::Cities => "Cities",
            Self::Magic => "Magic",
            Self::Info => "Info",
            Self::Plane => "Plane",
        }
    }

    const fn key(self) -> char {
        match self {
            Self::Game => 'g',
            Self::Spells => 's',
            Self::Armies => 'a',
            Self::Cities => 'c',
            Self::Magic => 'm',
            Self::Info => 'i',
            Self::Plane => 'p',
        }
    }
}

/// A bottom-row command. `Done` is the only one with a lasting effect (it
/// advances the turn); the rest post a notice, matching how little most of
/// these buttons do on the average `MoM` turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomAction {
    Done,
    Patrol,
    Wait,
    Build,
}

const BOTTOM_ACTIONS: &[BottomAction] = &[
    BottomAction::Done,
    BottomAction::Patrol,
    BottomAction::Wait,
    BottomAction::Build,
];

impl BottomAction {
    const fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Patrol => "Patrol",
            Self::Wait => "Wait",
            Self::Build => "Build",
        }
    }

    const fn key(self) -> char {
        match self {
            Self::Done => 'd',
            Self::Patrol => 'r',
            Self::Wait => 'w',
            Self::Build => 'b',
        }
    }
}

/// What tapping a registered hotspot means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Menu(MenuItem),
    Bottom(BottomAction),
    City(usize),
    Tower(usize),
}

/// State: camera, which plane is showing, the in-flight transition, and the
/// bits of session flavor (turn, treasury, a one-line notice) that make the
/// chrome feel inhabited rather than static.
pub struct TwinPlanes {
    plane: Plane,
    prev_plane: Plane,
    /// Seconds remaining in the current wipe; `0.0` once settled.
    transition: f32,
    time: f32,
    scroll: (i32, i32),
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    map_rect: Rect,
    selected_city: Option<usize>,
    turn: u32,
    gold: i32,
    mana: i32,
    food: i32,
    notice: String,
    notice_ttl: f32,
    fps: FpsMeter,
}

impl Default for TwinPlanes {
    fn default() -> Self {
        Self {
            plane: Plane::Arcanus,
            prev_plane: Plane::Arcanus,
            transition: 0.0,
            time: 0.0,
            scroll: (-6, -8),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            map_rect: Rect::new(0, 0, 0, 0),
            selected_city: None,
            turn: 1,
            gold: 7580,
            mana: 1242,
            food: 13,
            notice: "Welcome to Arcanus. Tap Plane to cross to Myrror.".to_owned(),
            notice_ttl: 4.0,
            fps: FpsMeter::new(),
        }
    }
}

impl TwinPlanes {
    /// Starts a plane swap. Ignored mid-swap rather than restarted, so a
    /// flurry of taps cannot leave the wipe forever chasing a moving target.
    fn toggle_plane(&mut self) {
        if self.transition > 0.0 {
            return;
        }
        self.prev_plane = self.plane;
        self.plane = self.plane.other();
        self.transition = TRANSITION_SECS;
        self.set_notice(format!("Crossing to {}...", self.plane.label()), 2.2);
    }

    fn set_notice(&mut self, text: String, ttl: f32) {
        self.notice = text;
        self.notice_ttl = ttl;
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Menu(MenuItem::Plane) | Action::Tower(_) => {
                if let Action::Tower(i) = action
                    && let Some(tower) = TOWERS.get(i)
                {
                    self.set_notice(
                        format!(
                            "You step through the {} and cross to {}.",
                            tower.name,
                            self.plane.other().label()
                        ),
                        2.6,
                    );
                    self.prev_plane = self.plane;
                    self.plane = self.plane.other();
                    self.transition = TRANSITION_SECS;
                } else {
                    self.toggle_plane();
                }
            }
            Action::Menu(other) => {
                self.set_notice(format!("{}: not modeled in this demo.", other.label()), 2.0);
            }
            Action::Bottom(BottomAction::Done) => {
                self.turn += 1;
                self.gold += 53 + (self.turn as i32 % 7) * 4;
                self.mana += 11 + (self.turn as i32 % 5);
                self.food = 10 + (self.turn as i32 % 6);
                self.set_notice(format!("Turn {} begins.", self.turn), 2.0);
            }
            Action::Bottom(BottomAction::Patrol) => {
                self.set_notice("Garrison ordered to patrol.".to_owned(), 2.0);
            }
            Action::Bottom(BottomAction::Wait) => {
                self.set_notice("Unit holds position.".to_owned(), 2.0);
            }
            Action::Bottom(BottomAction::Build) => {
                self.set_notice("Build queue opened (not modeled here).".to_owned(), 2.0);
            }
            Action::City(i) => {
                self.selected_city = Some(i);
                if let Some(city) = CITIES.get(i) {
                    self.set_notice(format!("Selected {}.", city.name), 2.0);
                }
            }
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
            KeyCode::Up | KeyCode::Char('w' | 'W') => self.scroll.1 -= 2,
            KeyCode::Down | KeyCode::Char('s' | 'S') => self.scroll.1 += 2,
            KeyCode::Left | KeyCode::Char('a' | 'A') => self.scroll.0 -= 2,
            KeyCode::Right | KeyCode::Char('d' | 'D') => self.scroll.0 += 2,
            KeyCode::Char('p' | 'P' | ' ') => self.toggle_plane(),
            KeyCode::Char(c) => {
                let lower = c.to_ascii_lowercase();
                if let Some(item) = MENU_ITEMS.iter().find(|item| item.key() == lower) {
                    self.handle_action(Action::Menu(*item));
                    return;
                }
                if let Some(action) = BOTTOM_ACTIONS.iter().find(|a| a.key() == lower) {
                    self.handle_action(Action::Bottom(*action));
                }
            }
            _ => {}
        }
        self.clamp_scroll();
    }

    /// Keeps the camera near the hand-placed cities and towers rather than
    /// drifting into noise that has nothing on it. Generous bounds: this is a
    /// sanity clamp on an otherwise-unbounded procedural field, not a map
    /// edge.
    fn clamp_scroll(&mut self) {
        self.scroll.0 = self.scroll.0.clamp(-60, 90);
        self.scroll.1 = self.scroll.1.clamp(-60, 90);
    }

    fn handle_gesture(&mut self, gesture: &Gesture) {
        if let Some(origin) = self.pointer.press_origin()
            && self.map_rect.width() > 0
            && self.map_rect.contains_pos(origin)
            && (gesture.delta.0 != 0 || gesture.delta.1 != 0)
        {
            self.scroll.0 -= gesture.delta.0;
            self.scroll.1 -= gesture.delta.1;
            self.clamp_scroll();
        }
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.handle_action(action);
        }
    }

    /// Splits the mid band into `(map, sidebar)`, stacking under a portrait
    /// viewport and sitting side by side otherwise. The sidebar's width (or
    /// height, stacked) is a fraction of what remains rather than a constant,
    /// so a wide desktop window gives the map the space a fixed sidebar width
    /// would otherwise leave idle.
    fn split_mid(mid: Rect, shape: Shape) -> (Rect, Rect) {
        if shape.stacks() {
            let side_h = (mid.height() / 3).clamp(10, 22).min(mid.height());
            let (map, side) = panel::split_bottom(mid, side_h);
            (map, side)
        } else {
            let side_w = (mid.width() / 4).clamp(24, 36);
            panel::split_right(mid, side_w)
        }
    }

    fn draw_menu(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < 4 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, MENU_ITEMS.len() as u16, 0);
        for (item, rect) in MENU_ITEMS.iter().zip(cols.iter()) {
            let is_plane = *item == MenuItem::Plane;
            let (fg, bg) = if is_plane {
                (rgb(20, 16, 8), self.plane.accent())
            } else {
                (ui::FG, rgb(46, 40, 24))
            };
            surface.fill_rect(*rect, ' ', Style::new().bg(bg));
            let label = if is_plane {
                format!("Plane: {}", self.plane.label())
            } else {
                item.label().to_owned()
            };
            let text = retroglyph_widgets::truncate(&label, rect.width_usize().saturating_sub(1));
            let tx = rect.left() + (rect.width().saturating_sub(text.chars().count() as u16)) / 2;
            let ty = rect.top() + rect.height() / 2;
            surface.print((tx, ty), text, Style::new().fg(fg).bg(bg));
            self.hotspots.push(*rect, Action::Menu(*item));
        }
    }

    fn draw_bottom(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < 4 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, BOTTOM_ACTIONS.len() as u16, 1);
        for (action, rect) in BOTTOM_ACTIONS.iter().zip(cols.iter()) {
            let bg = rgb(38, 34, 46);
            surface.fill_rect(*rect, ' ', Style::new().bg(bg));
            let label = action.label();
            let tx = rect.left() + (rect.width().saturating_sub(label.chars().count() as u16)) / 2;
            let ty = rect.top() + rect.height() / 2;
            surface.print((tx, ty), label, Style::new().fg(ui::ACCENT).bg(bg));
            self.hotspots.push(*rect, Action::Bottom(*action));
        }
    }

    /// Draws the terrain, magic nodes, cities, and towers for the current
    /// (possibly mid-transition) plane.
    ///
    /// Every visible cell is sampled fresh from the noise fields rather than
    /// read from a stored grid, so the map always covers `area` exactly: it
    /// has no fixed extent to run out and leave the rest of the panel black.
    fn draw_map(&mut self, surface: &mut Surface<'_>, area: Rect) {
        self.map_rect = area;
        if area.width() == 0 || area.height() == 0 {
            return;
        }

        let sweeping = self.transition > 0.0;
        // `progress` is 0 at the moment of toggling and 1 once the wipe has
        // fully crossed the map; cells flip from `prev_plane` to `plane` as
        // it passes their own threshold below.
        let progress = if sweeping {
            1.0 - self.transition / TRANSITION_SECS
        } else {
            1.0
        };
        let span = f32::from(area.width() + area.height()).max(1.0);

        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let wx = self.scroll.0 + i32::from(sx);
                let wy = self.scroll.1 + i32::from(sy);
                let at = (area.left() + sx, area.top() + sy);

                let land = land_at(wx, wy);
                let jitter = hash01(0x1234_5678, wx, wy);

                let (glyph, fg, bg) = if sweeping {
                    // Diagonal sweep position plus a per-cell offset: the
                    // wipe front is not a straight line, it shimmers, which
                    // is what separates "a transition" from "a diagonal
                    // wipe effect" -- the reference is a magical world, not
                    // a slide deck.
                    let wipe = f32::from(sx + sy) / span;
                    let cell_at = 0.85f32.mul_add(wipe, hash01(0x9911, wx, wy) * 0.15);
                    if progress >= cell_at {
                        terrain_visual(self.plane, land, jitter)
                    } else {
                        let (g, f, b) = terrain_visual(self.prev_plane, land, jitter);
                        // Cells within a hair of their own threshold flash
                        // toward white, giving the front itself a bright
                        // edge rather than a hard color seam.
                        if cell_at - progress < 0.04 {
                            (g, mix(f, rgb(255, 255, 255), 0.5), b)
                        } else {
                            (g, f, b)
                        }
                    }
                } else {
                    terrain_visual(self.plane, land, jitter)
                };
                surface.put(at, glyph, Style::new().fg(fg).bg(bg));
            }
        }

        // Nodes and landmarks only make sense once the wipe has actually
        // reached the plane they belong to, so they are skipped entirely
        // mid-transition rather than drawn inconsistently on top of a
        // half-old, half-new terrain field.
        if !sweeping {
            self.draw_nodes(surface, area);
            self.draw_cities(surface, area);
        }
        self.draw_towers(surface, area);
    }

    fn draw_nodes(&self, surface: &mut Surface<'_>, area: Rect) {
        let node_seed = match self.plane {
            Plane::Arcanus => 0x0A0A_C1F1,
            Plane::Myrror => 0x0111_F00D,
        };
        let node_color = match self.plane {
            Plane::Arcanus => rgb(240, 220, 120),
            Plane::Myrror => rgb(200, 150, 250),
        };
        for sy in 0..area.height() {
            for sx in 0..area.width() {
                let wx = self.scroll.0 + i32::from(sx);
                let wy = self.scroll.1 + i32::from(sy);
                if hash01(node_seed, wx, wy) > 0.02 {
                    continue;
                }
                if !hosts_nodes(land_at(wx, wy)) {
                    continue;
                }
                let phase = hash01(node_seed ^ 0x55, wx, wy) * core::f32::consts::TAU;
                let twinkle = 0.5f32.mul_add((self.time.mul_add(2.4, phase)).sin(), 0.5);
                let glyph = if twinkle > 0.5 { '*' } else { '+' };
                let bright = mix(node_color, rgb(255, 255, 255), twinkle * 0.4);
                surface.put(
                    (area.left() + sx, area.top() + sy),
                    glyph,
                    Style::new().fg(bright),
                );
            }
        }
    }

    fn draw_cities(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for (i, city) in CITIES.iter().enumerate() {
            if city.plane != self.plane {
                continue;
            }
            let (sx, sy) = (city.x - self.scroll.0, city.y - self.scroll.1);
            if sx < 0 || sy < 1 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
                continue;
            }
            let base = (area.left() + sx as u16, area.top() + sy as u16);
            let selected = self.selected_city == Some(i);
            let color = if selected {
                mix(city.owner.color(), rgb(255, 255, 255), 0.4)
            } else {
                city.owner.color()
            };
            surface.put(base, '|', Style::new().fg(color).bg(rgb(20, 20, 24)));
            let flag_x = base.0.saturating_add(1);
            let flag_y = base.1.saturating_sub(1);
            surface.put(
                (flag_x, flag_y),
                '\u{25BA}',
                Style::new().fg(color).bg(rgb(20, 20, 24)),
            );
            self.hotspots.push_tappable(
                Rect::new(base.0, base.1.min(flag_y), 2, 2),
                area,
                Action::City(i),
            );
        }
    }

    fn draw_towers(&mut self, surface: &mut Surface<'_>, area: Rect) {
        for (i, tower) in TOWERS.iter().enumerate() {
            let (sx, sy) = (tower.x - self.scroll.0, tower.y - self.scroll.1);
            if sx < 0 || sy < 0 || sx >= i32::from(area.width()) || sy >= i32::from(area.height()) {
                continue;
            }
            let at = (area.left() + sx as u16, area.top() + sy as u16);
            // Both planes' accent colors mixed together: the glyph belongs
            // to neither plane exclusively, which is what a coordinate that
            // exists on both should look like.
            let pulse = 0.5f32.mul_add((self.time * 1.6).sin(), 0.5);
            let color = mix(
                mix(Plane::Arcanus.accent(), Plane::Myrror.accent(), 0.5),
                rgb(255, 255, 255),
                pulse * 0.3,
            );
            surface.put(at, '\u{03A9}', Style::new().fg(color).bg(rgb(24, 20, 30)));
            self.hotspots
                .push_tappable(Rect::new(at.0, at.1, 1, 1), area, Action::Tower(i));
        }
    }

    fn draw_sidebar(&self, surface: &mut Surface<'_>, area: Rect, shape: Shape) {
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        if shape.stacks() {
            // Portrait: not enough width for a stacked minimap-then-panels
            // column, so the sidebar becomes two rows: minimap+totals side by
            // side, then the treasury bars.
            let minimap_w = (area.width() / 3).clamp(10, 20);
            let (top_row, treasury_area) = panel::split_top(area, area.height() / 2);
            let (minimap_area, totals_area) = panel::split_left(top_row, minimap_w);
            self.draw_minimap(surface, minimap_area);
            self.draw_totals(surface, totals_area);
            self.draw_treasury(surface, treasury_area);
        } else {
            let minimap_h = (area.height() / 3).clamp(8, 12);
            let (minimap_area, rest) = panel::split_top(area, minimap_h);
            let (totals_area, treasury_area) = panel::split_top(rest, 3.min(rest.height()));
            self.draw_minimap(surface, minimap_area);
            self.draw_totals(surface, totals_area);
            self.draw_treasury(surface, treasury_area);
        }
    }

    /// A coarse overview of the current plane: one cell per several world
    /// tiles, with the live viewport traced on top so the sidebar always
    /// shows both "what this plane looks like" and "where the main view is
    /// looking".
    fn draw_minimap(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new()
            .title("Map")
            .frame(self.plane.accent())
            .draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let origin = (
            self.scroll.0 - i32::from(inner.width()) * MINIMAP_STEP / 2,
            self.scroll.1 - i32::from(inner.height()) * MINIMAP_STEP / 2,
        );
        for sy in 0..inner.height() {
            for sx in 0..inner.width() {
                let wx = origin.0 + i32::from(sx) * MINIMAP_STEP;
                let wy = origin.1 + i32::from(sy) * MINIMAP_STEP;
                let land = land_at(wx, wy);
                let (glyph, fg, bg) = terrain_visual(self.plane, land, hash01(0x42, wx, wy));
                surface.put(
                    (inner.left() + sx, inner.top() + sy),
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
        // Viewport rectangle: the bit of the minimap the main map is showing.
        let vp_w = (i32::from(self.map_rect.width()) / MINIMAP_STEP).max(1);
        let vp_h = (i32::from(self.map_rect.height()) / MINIMAP_STEP).max(1);
        let vp_x = (self.scroll.0 - origin.0) / MINIMAP_STEP;
        let vp_y = (self.scroll.1 - origin.1) / MINIMAP_STEP;
        for x in vp_x..(vp_x + vp_w) {
            mark_edge(surface, inner, x, vp_y, ui::ACCENT);
            mark_edge(surface, inner, x, vp_y + vp_h - 1, ui::ACCENT);
        }
        for y in vp_y..(vp_y + vp_h) {
            mark_edge(surface, inner, vp_x, y, ui::ACCENT);
            mark_edge(surface, inner, vp_x + vp_w - 1, y, ui::ACCENT);
        }
    }

    fn draw_totals(&self, surface: &mut Surface<'_>, area: Rect) {
        let inner = panel::Panel::new().title("Turn").draw(surface, area);
        if inner.height() == 0 {
            return;
        }
        panel::spans(
            surface,
            (inner.left(), inner.top()),
            inner.width(),
            &[Span::keyword(&format!("Turn {}", self.turn))],
            panel::PANEL_BG,
        );
        if inner.height() > 1 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 1),
                inner.width(),
                &[Span::dim("Gold "), Span::plain(&format!("{}", self.gold))],
                panel::PANEL_BG,
            );
        }
        if inner.height() > 2 {
            panel::spans(
                surface,
                (inner.left(), inner.top() + 2),
                inner.width(),
                &[Span::dim("Mana "), Span::plain(&format!("{}", self.mana))],
                panel::PANEL_BG,
            );
        }
    }

    /// Three stacked resource readouts (Gold, Food, Mana), the sidebar detail
    /// the reference screenshot gives its own shelf art. Bars stand in for
    /// that art here: the number is precise, the bar is the at-a-glance read.
    fn draw_treasury(&self, surface: &mut Surface<'_>, area: Rect) {
        let selected = self.selected_city.and_then(|i| CITIES.get(i));
        let badge = selected.map(|c| format!("{} ({})", c.name, c.owner.label()));
        let mut panel = panel::Panel::new().title("Treasury");
        if let Some(badge) = badge.as_deref() {
            panel = panel.badge(badge);
        }
        let inner = panel.draw(surface, area);
        if inner.height() == 0 || inner.width() < 8 {
            return;
        }
        let rows: [(&str, f32, Color); 3] = [
            (
                "Gold",
                (self.gold as f32 / 9000.0).clamp(0.0, 1.0),
                rgb(226, 190, 90),
            ),
            (
                "Food",
                (self.food as f32 / 20.0).clamp(0.0, 1.0),
                rgb(150, 200, 110),
            ),
            (
                "Mana",
                (self.mana as f32 / 1600.0).clamp(0.0, 1.0),
                rgb(150, 150, 230),
            ),
        ];
        let bar_w = inner.width().saturating_sub(7).max(3);
        for (i, (label, t, color)) in rows.into_iter().enumerate() {
            let y = inner.top() + i as u16;
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                6,
                &[Span::dim(label)],
                panel::PANEL_BG,
            );
            panel::bar(
                surface,
                (inner.left() + 6, y),
                bar_w,
                t,
                color,
                rgb(30, 30, 36),
            );
        }
    }

    fn status_text(&self) -> String {
        if self.notice_ttl > 0.0 {
            self.notice.clone()
        } else {
            format!("{}  turn {}  drag to pan", self.plane.label(), self.turn)
        }
    }
}

/// Puts a single minimap-viewport edge cell, silently doing nothing outside
/// `inner` -- the traced rectangle routinely runs past the minimap's own
/// bounds when the camera nears the edge of the clamped scroll range.
fn mark_edge(surface: &mut Surface<'_>, inner: Rect, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 || x >= i32::from(inner.width()) || y >= i32::from(inner.height()) {
        return;
    }
    let at = (inner.left() + x as u16, inner.top() + y as u16);
    surface.put(at, '\u{2022}', Style::new().fg(color));
}

impl Demo for TwinPlanes {
    const NAME: &'static str = "48_twin_planes";
    const TITLE: &'static str = "Twin Planes";
    const BLURB: &'static str =
        "Master of Magic: one map coordinate existing in two overlaid worlds.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("WASD/arrows", "pan"),
            ("P/Space", "toggle plane"),
            ("D/R/W/B", "Done/Patrol/Wait/Build"),
            ("drag", "pan map"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        if self.transition > 0.0 {
            self.transition = (self.transition - dt).max(0.0);
        }
        if self.notice_ttl > 0.0 {
            self.notice_ttl = (self.notice_ttl - dt).max(0.0);
        }

        if !self.handle_events(term) {
            return false;
        }

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.hotspots.clear();
        let shape = Shape::of(content);
        let (menu_area, rest) = panel::split_top(content, MENU_H.min(content.height()));
        let (mid, bottom_area) = panel::split_bottom(rest, BOTTOM_H.min(rest.height()));
        let (map_area, side_area) = Self::split_mid(mid, shape);

        self.draw_menu(&mut surface, menu_area);
        self.draw_map(&mut surface, map_area);
        self.draw_sidebar(&mut surface, side_area, shape);
        self.draw_bottom(&mut surface, bottom_area);

        let gesture = self.pointer.take();
        self.handle_gesture(&gesture);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(TwinPlanes);
