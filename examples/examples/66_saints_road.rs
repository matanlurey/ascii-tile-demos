//! 66: Saint's Road -- Darklands' pull-down menu bar, played straight against
//! a dithered road through fifteenth-century Germany.
//!
//! Fifty-nine demos in this gallery build panels, cards, gauges, logs, and
//! touch hotspots, and not one of them is a pull-down menu bar: the oldest
//! interface idiom there is. Darklands (`MicroProse`, 1992) puts one across
//! the top of every screen -- `Game  Orders  Attack  Party` -- and its
//! dropdowns are the actual exercise: a label and a hotkey share one row
//! from opposite anchors, an item can be unavailable in a way that must read
//! as unavailable without relying on colour alone, and the panel has to sit
//! on top of the map without erasing it. That is the technique this demo is
//! built around; the party sidebar and the terrain are the frame around it.
//!
//! Techniques on show:
//!
//! - **A layered dropdown** ([`SaintsRoad::draw_dropdown`],
//!   [`retroglyph_core::Surface::on_layer`]): the open menu paints onto grid
//!   layer 1, so it composites over the map drawn on layer 0 underneath
//!   rather than needing to repaint the map by hand. Closing the menu clears
//!   exactly the footprint the previous frame's dropdown covered -- an
//!   untouched layer-1 cell is transparent, so anything left painted there
//!   would otherwise persist forever once the menu closes.
//! - **Three states without colour as the only signal**
//!   ([`SaintsRoad::draw_dropdown`]): available, highlighted, and disabled
//!   items differ in more than tint -- the highlighted row gets a leading
//!   `\u{25ba}` marker on top of its blue fill, and a disabled item drops its
//!   hotkey letter entirely rather than merely dimming it, so the row's
//!   *shape* says "you cannot choose this," not only its colour.
//! - **A flipping anchor** ([`SaintsRoad::draw_dropdown`]): a dropdown whose
//!   natural left-aligned position would run past the content area's right
//!   edge instead right-aligns to the menu label it hangs from, the same
//!   rule every desktop menu bar uses to stay on screen near a window edge.
//! - **Keyboard and mouse agreeing on one piece of state**
//!   ([`SaintsRoad::handle_key`], [`SaintsRoad::handle_mouse`]): both paths
//!   only ever set `open_menu` and `highlight`, and only ever fire an item
//!   through [`SaintsRoad::apply_item`], so a mnemonic letter, an arrow key,
//!   a click on the bar, and a hover across it all agree about what is open
//!   and what is about to happen.
//! - **Vertical bar gauges** ([`draw_vbar`]): `ui::panel::bar` is a
//!   horizontal gauge built on CP437's two half-cell steps (`\u{2588}`/
//!   `\u{258c}`). Turned on its side, the same two steps become
//!   `\u{2588}`/`\u{2584}` per row -- the finest vertical resolution CP437
//!   offers without reaching for the (now colourable, see the common brief)
//!   sextant glyphs, which were skipped here because a 3-5 row gauge does not
//!   have enough headroom over 2-per-cell to be worth the bookkeeping.
//! - **Ordered dithering** ([`tilekit::glyphs::bayer`]): cobblestone and
//!   grass are each a flat two-colour Bayer stipple on the background,
//!   exactly the ordered-dither pattern used to fake depth out of few
//!   available tones (see Bayer 1973 and the common gallery brief's note on
//!   why an ordered pattern is used over Floyd-Steinberg: it is a pure
//!   function of position, so it holds still as the road scrolls under the
//!   party instead of crawling).
//!
//! ```sh
//! cargo run --example 66_saints_road --features crossterm
//! cargo run --example 66_saints_road --features software
//! cargo run --example 66_saints_road --features gl
//! cargo run --example 66_saints_road  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode, MouseButton, MouseEventKind};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel};
use ascii_tile_demos::ui::touch::{Hotspots, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::glyphs::bayer;
use tilekit::noise::hash01;
use tilekit::palette::{self, rgb};

/// How many characters make up the party. Darklands itself shows four in
/// the sidebar on every screen; the number is small enough that all four
/// can share the map's selection outline logic without a scroll region.
const PARTY_SIZE: usize = 4;
/// Top-level menus, always in this order, left to right.
const MENU_COUNT: usize = 4;

/// Sidebar width when drawn as a left column (desktop and landscape).
const SIDEBAR_W: u16 = 17;
/// Sidebar row height when drawn along the bottom (portrait). Taller than a
/// single card's minimum so the vertical gauges keep some resolution even
/// when the layout is squeezed into one row.
const SIDEBAR_ROW_H: u16 = 7;

/// Cells per second the road scrolls under the party while marching. Picked
/// so the dither pattern's motion is readable frame to frame without being
/// distracting -- this is the "animates on its own" requirement, since
/// nothing about it depends on input.
const SCROLL_SPEED: f32 = 2.2;
/// Stamina drained per second while marching, and restored per second while
/// camped (see [`ItemAction::MakeCamp`]). Marching for a full 99-point bar
/// takes about a minute and a half at this rate, which is slow enough that a
/// viewer watching the demo idle actually sees the bar move without it
/// draining before the next camera cut.
const MARCH_DRAIN: f32 = 1.1;
const CAMP_REGEN: f32 = 6.0;

const MENU_OPEN_BG: Color = rgb(46, 82, 168);
const ROAD_DARK: Color = rgb(84, 64, 44);
const ROAD_LIGHT: Color = rgb(68, 50, 34);
const COBBLE_DARK: Color = rgb(70, 70, 78);
const COBBLE_LIGHT: Color = rgb(54, 54, 60);
const COBBLE_FLECK: Color = rgb(120, 120, 128);
const GRASS_DARK: Color = rgb(34, 58, 34);
const GRASS_LIGHT: Color = rgb(44, 72, 44);
const GRASS_FLECK: Color = rgb(96, 140, 78);

/// One of the four top-level menus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Game,
    Orders,
    Attack,
    Party,
}

const MENUS: [MenuKind; MENU_COUNT] = [
    MenuKind::Game,
    MenuKind::Orders,
    MenuKind::Attack,
    MenuKind::Party,
];

impl MenuKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Game => "Game",
            Self::Orders => "Orders",
            Self::Attack => "Attack",
            Self::Party => "Party",
        }
    }

    /// Every label here is chosen to start with its own mnemonic, so the
    /// mnemonic is always the label's first character -- one fact instead
    /// of a second table to keep in sync with [`Self::label`].
    fn mnemonic(self) -> char {
        self.label().chars().next().unwrap_or(' ')
    }
}

/// What firing a dropdown item actually does. Kept as a tag rather than a
/// closure so [`SaintsRoad::menu_items`] can build the same item list purely
/// (for hit-testing and for measuring the dropdown's width) without also
/// needing `&mut self`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemAction {
    NewJourney,
    LoadGame,
    SaveGame,
    Options,
    MakeCamp,
    BreakCamp,
    SetPace,
    ConsultMap,
    Throw,
    StdAttack,
    Vulnerable,
    Berserk,
    Parry,
    UseMissile,
    Formation,
    Rest,
    Inventory,
    Disband,
}

impl ItemAction {
    /// The stance an Attack item sets on the selected character. Only
    /// meaningful for the six Attack actions; unreachable for the rest,
    /// since [`SaintsRoad::apply_item`] never calls this outside that arm.
    const fn stance_label(self) -> &'static str {
        match self {
            Self::Throw => "Throw",
            Self::StdAttack => "Std Attack",
            Self::Vulnerable => "Vulnerable",
            Self::Berserk => "Berserk",
            Self::Parry => "Parry",
            Self::UseMissile => "Use Missile",
            _ => "Ready",
        }
    }
}

/// One row of a dropdown: a left-aligned label, a right-aligned hotkey, and
/// whether it can be chosen right now.
#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    hotkey: char,
    enabled: bool,
    action: ItemAction,
}

impl MenuItem {
    const fn new(label: &'static str, hotkey: char, enabled: bool, action: ItemAction) -> Self {
        Self {
            label,
            hotkey,
            enabled,
            action,
        }
    }
}

/// What a hotspot resolves to: which top-level label, which row of the open
/// dropdown, or which party card.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    TopMenu(usize),
    Item(usize),
    Party(usize),
}

/// One party member's road stats. `faith` and `health` only ever change at
/// a specific moment (a discrete story beat, not a tween -- see the common
/// brief's rule against animating numbers); `stamina` is the one continuous
/// quantity, draining while marching and refilling while camped.
struct Member {
    name: &'static str,
    faith: f32,
    health: f32,
    stamina: f32,
    /// Whether this character is currently carrying a throwing weapon.
    /// Differs per character on purpose: switching `selected` visibly
    /// changes which Attack items are disabled, which is the whole point of
    /// tying the dropdown's enabled state to something real instead of a
    /// constant.
    has_throwable: bool,
    has_missile: bool,
    stance: &'static str,
}

impl Member {
    const fn new(
        name: &'static str,
        faith: f32,
        health: f32,
        stamina: f32,
        has_throwable: bool,
        has_missile: bool,
    ) -> Self {
        Self {
            name,
            faith,
            health,
            stamina,
            has_throwable,
            has_missile,
            stance: "Ready",
        }
    }

    /// The leading status flag in the numbers row: `-` fine, `W` wounded.
    const fn status_flag(&self) -> char {
        if self.health < 50.0 { 'W' } else { '-' }
    }
}

/// State: the party, the open dropdown (if any) and its highlighted row,
/// the road's scroll position, and the touch/animation plumbing every demo
/// in this gallery shares.
pub struct SaintsRoad {
    party: [Member; PARTY_SIZE],
    selected: usize,

    open_menu: Option<usize>,
    /// Row index within whatever menu `open_menu` names. Kept as a plain
    /// index rather than a per-menu map since only one dropdown is ever open
    /// at a time.
    highlight: usize,
    /// Screen rect of each top-level label, recorded during
    /// [`Self::draw_menu_bar`] so [`Self::draw_dropdown`] knows where to
    /// anchor from and hit-testing has something to register against.
    menu_label_rects: [Rect; MENU_COUNT],
    /// The dropdown's rect from the previous frame, so this frame can clear
    /// exactly that footprint on layer 1 before drawing (or not drawing) a
    /// new one. See the module doc's note on layer-1 transparency.
    last_dropdown_rect: Rect,
    hotspots: Hotspots<Hit>,

    camped: bool,
    has_save: bool,
    last_action: Option<&'static str>,

    /// World-space horizontal offset of the map viewport, in cells.
    /// Increases at [`SCROLL_SPEED`] while marching and holds still while
    /// camped, so the party visibly stops moving the instant camp is made.
    scroll_x: f32,
    /// Terrain hash seed, changed by [`Self::reroll`].
    seed: u32,

    fps: FpsMeter,
}

impl Default for SaintsRoad {
    fn default() -> Self {
        let party = [
            // A thrown dagger but no crossbow: Throw is available, Use
            // Missile is not.
            Member::new("Reinhardt", 70.0, 85.0, 60.0, true, false),
            // The reverse: a crossbow but nothing to throw.
            Member::new("Ilse", 40.0, 99.0, 80.0, false, true),
            // Wounded (health < 50, so the sidebar shows `W`), unarmed at
            // range entirely: both weapon-dependent items are disabled.
            Member::new("Georg", 55.0, 32.0, 45.0, false, false),
            // Both, so every Attack item is available for this one -- the
            // contrast with the other three is what shows the disabled
            // state is really reacting to `selected`, not just decorative.
            Member::new("Brunhild", 90.0, 99.0, 99.0, true, true),
        ];
        Self {
            party,
            selected: 0,
            open_menu: None,
            highlight: 0,
            menu_label_rects: [Rect::new(0, 0, 0, 0); MENU_COUNT],
            last_dropdown_rect: Rect::new(0, 0, 0, 0),
            hotspots: Hotspots::new(),
            camped: false,
            has_save: false,
            last_action: None,
            scroll_x: 0.0,
            seed: 0x5A17_D06D,
            fps: FpsMeter::new(),
        }
    }
}

impl SaintsRoad {
    const fn reroll(&mut self) {
        self.seed = self.seed.wrapping_add(0x9E37_79B9);
        self.scroll_x = 0.0;
        self.last_action = Some("A new stretch of road begins.");
    }

    // -- Menu content -------------------------------------------------

    /// Builds the item list for `kind`, freshly evaluated against current
    /// state. Called on every draw and every input, rather than cached,
    /// since it is the single source of truth both read: caching it would
    /// risk the drawn dropdown and the hotkey dispatch disagreeing about
    /// what is enabled the instant `selected` or `camped` changes mid-frame.
    fn menu_items(&self, kind: MenuKind) -> Vec<MenuItem> {
        match kind {
            MenuKind::Game => vec![
                MenuItem::new("New Journey", 'N', true, ItemAction::NewJourney),
                MenuItem::new("Load Game", 'L', self.has_save, ItemAction::LoadGame),
                MenuItem::new("Save Game", 'S', true, ItemAction::SaveGame),
                MenuItem::new("Options", 'O', true, ItemAction::Options),
            ],
            MenuKind::Orders => vec![
                MenuItem::new("Make Camp", 'C', !self.camped, ItemAction::MakeCamp),
                MenuItem::new("Break Camp", 'B', self.camped, ItemAction::BreakCamp),
                MenuItem::new("Set Pace", 'P', true, ItemAction::SetPace),
                MenuItem::new("Consult Map", 'M', true, ItemAction::ConsultMap),
            ],
            MenuKind::Attack => {
                let m = &self.party[self.selected];
                vec![
                    MenuItem::new("Throw", 'T', m.has_throwable, ItemAction::Throw),
                    MenuItem::new("Std Attack", 'A', true, ItemAction::StdAttack),
                    MenuItem::new("Vulnerable", 'D', true, ItemAction::Vulnerable),
                    MenuItem::new("Berserk", 'B', true, ItemAction::Berserk),
                    MenuItem::new("Parry", 'P', true, ItemAction::Parry),
                    MenuItem::new("Use Missile", 'M', m.has_missile, ItemAction::UseMissile),
                ]
            }
            MenuKind::Party => vec![
                MenuItem::new("Formation", 'F', true, ItemAction::Formation),
                MenuItem::new(
                    "Rest",
                    'R',
                    self.camped && self.party[self.selected].health < 99.0,
                    ItemAction::Rest,
                ),
                MenuItem::new("Inventory", 'I', true, ItemAction::Inventory),
                // Always disabled: a lone traveller cannot disband a party
                // of four down to nothing. The one item in this demo that is
                // never enabled, which is the plainest possible proof that
                // the grey/no-hotkey treatment does not depend on colour.
                MenuItem::new("Disband", 'D', false, ItemAction::Disband),
            ],
        }
    }

    fn apply_item(&mut self, action: ItemAction) {
        match action {
            ItemAction::NewJourney => self.reroll(),
            ItemAction::LoadGame => self.last_action = Some("Loaded the saved journey."),
            ItemAction::SaveGame => {
                self.has_save = true;
                self.last_action = Some("Journey saved.");
            }
            ItemAction::Options => self.last_action = Some("Options: nothing to configure yet."),
            ItemAction::MakeCamp => {
                self.camped = true;
                self.last_action = Some("The party makes camp.");
            }
            ItemAction::BreakCamp => {
                self.camped = false;
                self.last_action = Some("The party breaks camp and marches on.");
            }
            ItemAction::SetPace => self.last_action = Some("Pace set to a steady march."),
            ItemAction::ConsultMap => self.last_action = Some("The road map is consulted."),
            ItemAction::Throw
            | ItemAction::StdAttack
            | ItemAction::Vulnerable
            | ItemAction::Berserk
            | ItemAction::Parry
            | ItemAction::UseMissile => {
                let stance = action.stance_label();
                self.party[self.selected].stance = stance;
                self.last_action = Some(stance);
            }
            ItemAction::Formation => self.last_action = Some("Formation held."),
            ItemAction::Rest => {
                let member = &mut self.party[self.selected];
                member.health = (member.health + 15.0).min(99.0);
                self.last_action = Some("Rest tends the wounded.");
            }
            ItemAction::Inventory => self.last_action = Some("Packs checked; all present."),
            // Never reached: `menu_items` never marks this one enabled.
            ItemAction::Disband => {}
        }
    }

    // -- Menu navigation ------------------------------------------------

    fn first_enabled(&self, idx: usize) -> usize {
        self.menu_items(MENUS[idx])
            .iter()
            .position(|item| item.enabled)
            .unwrap_or(0)
    }

    fn open_menu_at(&mut self, idx: usize) {
        self.open_menu = Some(idx);
        self.highlight = self.first_enabled(idx);
    }

    /// Moves the highlight by `dir` rows, skipping disabled items -- the
    /// same behaviour Windows 3.1 and every menu bar since gives keyboard
    /// navigation, so a player driving by arrow keys is never left parked
    /// on a row that Enter would do nothing to.
    fn move_highlight(&mut self, dir: i32) {
        let Some(idx) = self.open_menu else { return };
        let items = self.menu_items(MENUS[idx]);
        if items.is_empty() {
            return;
        }
        let n = items.len() as i32;
        let mut h = self.highlight as i32;
        for _ in 0..items.len() {
            h = (h + dir).rem_euclid(n);
            if items[h as usize].enabled {
                self.highlight = h as usize;
                return;
            }
        }
    }

    fn fire_highlighted(&mut self) {
        let Some(idx) = self.open_menu else { return };
        let items = self.menu_items(MENUS[idx]);
        if let Some(item) = items.get(self.highlight)
            && item.enabled
        {
            self.apply_item(item.action);
            self.open_menu = None;
        }
    }

    /// Tries `c` as a hotkey against whatever menu is currently open.
    /// Returns `true` if `c` named an item at all (even a disabled one, so
    /// the caller does not also try it as a top-level mnemonic): a disabled
    /// item's hotkey is consumed silently rather than falling through,
    /// which is what "the greyed-out row does nothing" has to mean at the
    /// keyboard, not just at the mouse.
    fn try_item_hotkey(&mut self, c: char) -> bool {
        let Some(idx) = self.open_menu else {
            return false;
        };
        let items = self.menu_items(MENUS[idx]);
        let upper = c.to_ascii_uppercase();
        let Some(item) = items.iter().find(|item| item.hotkey == upper) else {
            return false;
        };
        if item.enabled {
            self.apply_item(item.action);
            self.open_menu = None;
        }
        true
    }

    // -- Input ---------------------------------------------------------

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            match event {
                Event::Key(key) if key.is_down() => self.handle_key(key.code),
                Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.position),
                _ => {}
            }
        }
        true
    }

    /// No WASD alias here on purpose. Every letter a movement scheme would
    /// want (`W`/`A`/`S`/`D`) is already a live mnemonic somewhere in this
    /// demo's own menus (`A` is both "open Attack" and "Std Attack", `D` is
    /// both "Vulnerable" and "Disband"), so doubling them as navigation
    /// would make the keyboard disagree with itself about what a letter
    /// does depending on state -- exactly the kind of ambiguity a mnemonic
    /// system exists to avoid. Arrow keys, Tab, and `R` cover the required
    /// bindings without that collision.
    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Escape => self.open_menu = None,
            KeyCode::Enter => self.fire_highlighted(),
            KeyCode::Left => {
                if let Some(idx) = self.open_menu {
                    self.open_menu_at((idx + MENU_COUNT - 1) % MENU_COUNT);
                }
            }
            KeyCode::Right => {
                if let Some(idx) = self.open_menu {
                    self.open_menu_at((idx + 1) % MENU_COUNT);
                }
            }
            KeyCode::Up => {
                if self.open_menu.is_some() {
                    self.move_highlight(-1);
                } else {
                    self.selected = (self.selected + PARTY_SIZE - 1) % PARTY_SIZE;
                }
            }
            KeyCode::Down => {
                if self.open_menu.is_some() {
                    self.move_highlight(1);
                } else {
                    self.selected = (self.selected + 1) % PARTY_SIZE;
                }
            }
            KeyCode::Tab => self.selected = (self.selected + 1) % PARTY_SIZE,
            KeyCode::Char('r' | 'R') if self.open_menu.is_none() => self.reroll(),
            KeyCode::Char(c) if !self.try_item_hotkey(c) => {
                let upper = c.to_ascii_uppercase();
                if let Some(idx) = MENUS.iter().position(|m| m.mnemonic() == upper) {
                    self.open_menu_at(idx);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, pos: Pos) {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match self.hotspots.hit(pos).copied() {
                Some(Hit::TopMenu(idx)) => {
                    if self.open_menu == Some(idx) {
                        self.open_menu = None;
                    } else {
                        self.open_menu_at(idx);
                    }
                }
                Some(Hit::Item(row)) => {
                    if let Some(idx) = self.open_menu
                        && let Some(item) = self.menu_items(MENUS[idx]).get(row)
                    {
                        if item.enabled {
                            self.apply_item(item.action);
                            self.open_menu = None;
                        } else {
                            // A click on a disabled row does nothing to the
                            // model, but it does move the highlight: the
                            // pointer is telling us where it is, and the row
                            // still deserves to look focused even though it
                            // cannot be chosen.
                            self.highlight = row;
                        }
                    }
                }
                Some(Hit::Party(idx)) => self.selected = idx,
                None => self.open_menu = None,
            },
            MouseEventKind::Moved => match self.hotspots.hit(pos).copied() {
                // Hovering a different top-level label while a menu is open
                // switches to it, agreeing with what Left/Right does at the
                // keyboard: the dropdown stays open, only its contents
                // change.
                Some(Hit::TopMenu(idx))
                    if self.open_menu.is_some() && self.open_menu != Some(idx) =>
                {
                    self.open_menu_at(idx);
                }
                Some(Hit::Item(row)) => self.highlight = row,
                _ => {}
            },
            _ => {}
        }
    }

    // -- Simulation ---------------------------------------------------------

    fn simulate(&mut self, dt: f32) {
        if self.camped {
            for member in &mut self.party {
                member.stamina = dt.mul_add(CAMP_REGEN, member.stamina).min(99.0);
            }
        } else {
            self.scroll_x = dt.mul_add(SCROLL_SPEED, self.scroll_x);
            for member in &mut self.party {
                member.stamina = dt.mul_add(-MARCH_DRAIN, member.stamina).max(0.0);
            }
        }
    }

    fn status_line(&self) -> String {
        let member = &self.party[self.selected];
        format!(
            "{}  {}  selected: {} ({})",
            if self.camped { "CAMPED" } else { "on the road" },
            self.last_action.unwrap_or("Awaiting orders."),
            member.name,
            member.stance,
        )
    }

    // -- Layout ----------------------------------------------------------

    fn draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let (menu_row, rest) = panel::split_top(content, 1);
        self.draw_menu_bar(surface, menu_row);

        let portrait = Shape::of(content).stacks();
        let (sidebar_area, map_area) = if portrait {
            let sidebar_h = SIDEBAR_ROW_H.min(rest.height());
            let (map, sidebar) = panel::split_bottom(rest, sidebar_h);
            (sidebar, map)
        } else {
            panel::split_left(rest, SIDEBAR_W.min(rest.width()))
        };

        self.draw_map(surface, map_area);
        if portrait {
            self.draw_sidebar_row(surface, sidebar_area);
        } else {
            self.draw_sidebar_column(surface, sidebar_area);
        }
        // Drawn last so its hotspots (pushed after the sidebar's) win any
        // overlap, and so its layer-1 content paints over whatever the map
        // and sidebar just drew on layer 0.
        self.draw_dropdown(surface, content, menu_row);
    }

    // -- Menu bar --------------------------------------------------------

    fn draw_menu_bar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        let mut x = area.left() + 1;
        for (i, kind) in MENUS.iter().enumerate() {
            let label = kind.label();
            let w = label.chars().count() as u16 + 2;
            if x + w > area.right() {
                break;
            }
            let rect = Rect::new(x, area.top(), w, 1);
            let open = self.open_menu == Some(i);
            let bg = if open { MENU_OPEN_BG } else { ui::CHROME_BG };
            if open {
                surface.fill_rect(rect, ' ', Style::new().bg(bg));
            }
            let fg = if open { palette::WHITE } else { ui::FG };
            print_mnemonic(surface, (rect.left() + 1, rect.top()), label, fg, bg);
            self.menu_label_rects[i] = rect;
            self.hotspots.push(rect, Hit::TopMenu(i));
            x += w;
        }
    }

    // -- Dropdown --------------------------------------------------------

    fn draw_dropdown(&mut self, surface: &mut Surface<'_>, content: Rect, menu_row: Rect) {
        // Clear last frame's footprint unconditionally, whether or not a
        // menu is open this frame: an untouched layer-1 cell is transparent
        // and shows layer 0 through it (see `Surface::on_layer`'s docs), so
        // without this a closed dropdown would stay painted over the map
        // forever instead of revealing it again.
        surface.on_layer(1).clear_region(self.last_dropdown_rect);

        let Some(open) = self.open_menu else {
            self.last_dropdown_rect = Rect::new(0, 0, 0, 0);
            return;
        };
        let items = self.menu_items(MENUS[open]);
        if items.is_empty() {
            self.last_dropdown_rect = Rect::new(0, 0, 0, 0);
            return;
        }

        let label_w = items
            .iter()
            .map(|item| item.label.chars().count())
            .max()
            .unwrap_or(0) as u16;
        // marker column + label + gap + one-letter hotkey + border, both sides.
        let width = (label_w + 8).max(16);
        let height = items.len() as u16 + 2;

        let anchor = self.menu_label_rects[open];
        let x = if anchor.left() + width > content.right() {
            // Flip: line the dropdown's right edge up with the menu label's
            // right edge instead of its left, so a menu near the right
            // border does not run its dropdown off the screen.
            content.right().saturating_sub(width).max(content.left())
        } else {
            anchor.left()
        };
        let y = menu_row.bottom();
        let rect = Rect::new(x, y, width, height.min(content.bottom().saturating_sub(y)));
        self.last_dropdown_rect = rect;

        let mut layer = surface.on_layer(1);
        let inner = Panel::new()
            .border(Border::Single)
            .bg(panel::PANEL_BG)
            .frame(ui::ACCENT)
            .draw(&mut layer, rect);

        for (row, item) in items.iter().enumerate() {
            if row as u16 >= inner.height() {
                break;
            }
            let row_rect = Rect::new(inner.left(), inner.top() + row as u16, inner.width(), 1);
            let highlighted = row == self.highlight;
            // Three states, distinguished by more than colour: a disabled
            // row drops its hotkey letter (see the print below) on top of
            // being dim; a highlighted row gets a marker glyph on top of
            // its blue fill; an available row gets neither decoration.
            let (marker, fg, bg) = if !item.enabled {
                (' ', ui::DIM, panel::PANEL_BG)
            } else if highlighted {
                ('\u{25BA}', palette::WHITE, MENU_OPEN_BG)
            } else {
                (' ', ui::FG, panel::PANEL_BG)
            };
            layer.fill_rect(row_rect, ' ', Style::new().bg(bg));
            layer.put(
                (row_rect.left(), row_rect.top()),
                marker,
                Style::new().fg(fg).bg(bg),
            );
            layer.print(
                (row_rect.left() + 2, row_rect.top()),
                item.label,
                Style::new().fg(fg).bg(bg),
            );
            if item.enabled {
                let hotkey = item.hotkey.to_string();
                let hk_x = row_rect
                    .right()
                    .saturating_sub(1 + hotkey.chars().count() as u16);
                layer.print((hk_x, row_rect.top()), &hotkey, Style::new().fg(fg).bg(bg));
            }
            self.hotspots.push(row_rect, Hit::Item(row));
        }
    }

    // -- Party sidebar -----------------------------------------------------

    fn draw_sidebar_column(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let card_h = (area.height() / PARTY_SIZE as u16).max(1);
        for i in 0..PARTY_SIZE {
            let y0 = area.top() + i as u16 * card_h;
            if y0 >= area.bottom() {
                break;
            }
            let rect = Rect::new(
                area.left(),
                y0,
                area.width(),
                card_h.min(area.bottom() - y0),
            );
            self.draw_party_card(surface, rect, i);
            self.hotspots.push(rect, Hit::Party(i));
        }
    }

    fn draw_sidebar_row(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, PARTY_SIZE as u16, 1);
        for (i, rect) in cols.into_iter().enumerate() {
            self.draw_party_card(surface, rect, i);
            self.hotspots.push(rect, Hit::Party(i));
        }
    }

    fn draw_party_card(&self, surface: &mut Surface<'_>, rect: Rect, idx: usize) {
        let member = &self.party[idx];
        let selected = idx == self.selected;
        let bg = if selected {
            rgb(28, 32, 50)
        } else {
            panel::PANEL_BG
        };
        surface.fill_rect(rect, ' ', Style::new().bg(bg));
        if rect.width() < 4 || rect.height() == 0 {
            return;
        }

        let marker = if selected { '\u{25BA}' } else { ' ' };
        let name_fg = if selected { ui::ACCENT } else { ui::FG };
        surface.put(
            (rect.left(), rect.top()),
            marker,
            Style::new().fg(ui::ACCENT).bg(bg),
        );
        surface.print(
            (rect.left() + 1, rect.top()),
            retroglyph_widgets::truncate(member.name, rect.width_usize().saturating_sub(1)),
            Style::new().fg(name_fg).bg(bg),
        );

        let numbers = format!(
            "{} {:>2} {:>2} {:>2}",
            member.status_flag(),
            member.faith as i32,
            member.health as i32,
            member.stamina as i32,
        );

        if rect.height() >= 3 && rect.width() >= 9 {
            let bar_h = (rect.height() - 2).clamp(1, 5);
            let track = rgb(24, 26, 34);
            draw_vbar(
                surface,
                rect.left() + 1,
                rect.top() + 1,
                bar_h,
                member.faith / 99.0,
                rgb(96, 140, 224),
                track,
            );
            draw_vbar(
                surface,
                rect.left() + 3,
                rect.top() + 1,
                bar_h,
                member.health / 99.0,
                rgb(108, 196, 108),
                track,
            );
            draw_vbar(
                surface,
                rect.left() + 5,
                rect.top() + 1,
                bar_h,
                member.stamina / 99.0,
                rgb(226, 196, 90),
                track,
            );
            let numbers_y = rect.top() + 1 + bar_h;
            if numbers_y < rect.bottom() {
                surface.print(
                    (rect.left() + 1, numbers_y),
                    &numbers,
                    Style::new().fg(ui::DIM).bg(bg),
                );
            }
        } else if rect.height() >= 2 {
            surface.print(
                (rect.left() + 1, rect.top() + 1),
                &numbers,
                Style::new().fg(ui::DIM).bg(bg),
            );
        }
    }

    // -- Map ---------------------------------------------------------------

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title(if self.camped { "Camp" } else { "Saint's Road" })
            .border(Border::Double)
            .bg(GRASS_DARK);
        let inner = panel.draw(surface, area);
        if inner.width() == 0 || inner.height() == 0 {
            return;
        }
        let scroll = self.scroll_x as i32;
        for y in 0..inner.height() {
            for x in 0..inner.width() {
                let wx = i32::from(x) + scroll;
                let wy = i32::from(y);
                let (glyph, fg, bg) = self.terrain_cell(wx, wy);
                surface.put(
                    (inner.left() + x, inner.top() + y),
                    glyph,
                    Style::new().fg(fg).bg(bg),
                );
            }
        }
        self.draw_party_tokens(surface, inner);
    }

    /// The glyph, foreground, and background for one world cell: a diagonal
    /// dirt road cutting through alternating patches of cobbled dooryard and
    /// open grass, each patch rendered as a flat two-colour Bayer stipple
    /// rather than a solid fill (see the module doc).
    fn terrain_cell(&self, wx: i32, wy: i32) -> (char, Color, Color) {
        let on_road = (wx + wy).rem_euclid(14) < 2;
        if on_road {
            let bg = if bayer(wx, wy) < 0.5 {
                ROAD_DARK
            } else {
                ROAD_LIGHT
            };
            return (' ', ROAD_DARK, bg);
        }

        // Low-frequency block hash: which 10x8 patch of ground this cell
        // falls in decides cobble vs. grass, so a patch reads as a coherent
        // dooryard rather than salt-and-pepper noise.
        let block_x = wx.div_euclid(10);
        let block_y = wy.div_euclid(8);
        let cobbled = hash01(self.seed, block_x, block_y) < 0.32;

        if cobbled {
            let bg = if bayer(wx, wy) < 0.5 {
                COBBLE_DARK
            } else {
                COBBLE_LIGHT
            };
            // A sparser, phase-shifted second dither pass adds a scatter of
            // light-shade flecks on top of the base stipple.
            let glyph = if bayer(wx + 2, wy + 1) < 0.2 {
                '\u{2591}'
            } else {
                ' '
            };
            (glyph, COBBLE_FLECK, bg)
        } else {
            let bg = if bayer(wx, wy) < 0.5 {
                GRASS_DARK
            } else {
                GRASS_LIGHT
            };
            let glyph = if bayer(wx + 1, wy + 3) < 0.35 {
                ','
            } else {
                ' '
            };
            (glyph, GRASS_FLECK, bg)
        }
    }

    /// Draws all four party tokens at fixed screen offsets from the map's
    /// centre (the world scrolls under them, not the other way round -- see
    /// [`SCROLL_SPEED`]), with a white outline box around whichever one is
    /// `selected`.
    fn draw_party_tokens(&self, surface: &mut Surface<'_>, inner: Rect) {
        const OFFSETS: [(i32, i32); PARTY_SIZE] = [(-3, -1), (3, -1), (-2, 1), (2, 1)];
        let cx = i32::from(inner.left()) + i32::from(inner.width()) * 2 / 5;
        let cy = i32::from(inner.top()) + i32::from(inner.height()) / 2;

        for (i, member) in self.party.iter().enumerate() {
            let (ox, oy) = OFFSETS[i];
            let (px, py) = (cx + ox, cy + oy);
            if !in_rect(inner, px, py) {
                continue;
            }
            let (x, y) = (px as u16, py as u16);
            let selected = i == self.selected;
            let glyph = member.name.chars().next().unwrap_or('?');
            let fg = if selected { palette::WHITE } else { ui::FG };
            let bg = if member.health < 50.0 {
                rgb(90, 30, 26)
            } else {
                rgb(40, 34, 58)
            };
            surface.put((x, y), glyph, Style::new().fg(fg).bg(bg));
            if selected {
                draw_outline(surface, inner, px, py);
            }
        }
    }
}

/// `true` if world cell `(x, y)` falls inside `bounds`. A signed check: a
/// token offset can land left of or above the map panel, and `Rect`'s
/// coordinates are `u16`, so this has to happen before either casts down.
fn in_rect(bounds: Rect, x: i32, y: i32) -> bool {
    x >= i32::from(bounds.left())
        && x < i32::from(bounds.right())
        && y >= i32::from(bounds.top())
        && y < i32::from(bounds.bottom())
}

/// Draws a single-line box outline in the eight cells surrounding
/// `(cx, cy)`, clipped to `bounds`. The centre cell itself is left alone --
/// callers draw the token there separately -- so the box always reads as a
/// frame around something rather than a filled square.
fn draw_outline(surface: &mut Surface<'_>, bounds: Rect, cx: i32, cy: i32) {
    const RING: [(i32, i32, char); 8] = [
        (-1, -1, '\u{250C}'),
        (0, -1, '\u{2500}'),
        (1, -1, '\u{2510}'),
        (-1, 0, '\u{2502}'),
        (1, 0, '\u{2502}'),
        (-1, 1, '\u{2514}'),
        (0, 1, '\u{2500}'),
        (1, 1, '\u{2518}'),
    ];
    let style = Style::new().fg(palette::WHITE).bg(rgb(20, 22, 30));
    for (dx, dy, glyph) in RING {
        let (px, py) = (cx + dx, cy + dy);
        if in_rect(bounds, px, py) {
            surface.put((px as u16, py as u16), glyph, style);
        }
    }
}

/// Draws one vertical bar gauge, `rows` cells tall, in a single column.
///
/// CP437 gives two vertical steps per cell: `\u{2588}` (full) and `\u{2584}`
/// (bottom half filled). That is the same resolution `ui::panel::bar` gets
/// horizontally from `\u{2588}`/`\u{258c}`, just turned ninety degrees --
/// the eighth-block glyphs that would give finer steps are outside CP437 and
/// render as a solid slab on the pixel backends (see the common brief).
/// `track` is drawn under every unfilled cell so the gauge's full extent
/// stays legible at zero, the same reasoning `ui::panel::bar` documents for
/// the horizontal case.
fn draw_vbar(
    surface: &mut Surface<'_>,
    x: u16,
    top: u16,
    rows: u16,
    t: f32,
    fill: Color,
    track: Color,
) {
    let t = t.clamp(0.0, 1.0);
    let steps = (f32::from(rows) * 2.0 * t).round() as u16;
    for row in 0..rows {
        // Rows are addressed from the bottom, since a gauge fills upward.
        let from_bottom = rows - 1 - row;
        let cell_steps = steps.saturating_sub(from_bottom * 2).min(2);
        let glyph = match cell_steps {
            2 => '\u{2588}',
            1 => '\u{2584}',
            _ => ' ',
        };
        surface.put((x, top + row), glyph, Style::new().fg(fill).bg(track));
    }
}

/// Prints `label` with its first character (always the menu's mnemonic; see
/// [`MenuKind::mnemonic`]) in the accent colour and the rest in `fg`, which
/// is how this demo marks a menu bar's mnemonic letters without an
/// underline attribute -- `retroglyph`'s `Style` deliberately has none (see
/// its doc comment: a terminal backend cannot fake bold or underline
/// without a real font variant).
fn print_mnemonic(surface: &mut Surface<'_>, pos: (u16, u16), label: &str, fg: Color, bg: Color) {
    let mut chars = label.chars();
    if let Some(first) = chars.next() {
        surface.put(pos, first, Style::new().fg(ui::ACCENT).bg(bg));
    }
    let rest: String = chars.collect();
    surface.print((pos.0 + 1, pos.1), &rest, Style::new().fg(fg).bg(bg));
}

impl Demo for SaintsRoad {
    const NAME: &'static str = "66_saints_road";
    const TITLE: &'static str = "66 Saint's Road";
    const BLURB: &'static str =
        "A pull-down menu bar with keyboard mnemonics, disabled items, and a flipping dropdown.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("G/O/A/P", "open a menu"),
            ("hotkey", "choose a dropdown item"),
            ("\u{2190}/\u{2192}", "switch open menu"),
            ("\u{2191}/\u{2193}", "navigate / cycle party"),
            ("Enter", "choose highlighted"),
            ("Esc", "close menu"),
            ("Tab", "cycle party"),
            ("R", "reroll the road"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.fps.record(frame.delta);
        self.simulate(frame.delta.as_secs_f32());
        if !self.handle_events(term) {
            return false;
        }

        let (title, content, status) = ui::split_chrome(term.area());
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));
        self.draw(&mut surface, content);
        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(SaintsRoad);
