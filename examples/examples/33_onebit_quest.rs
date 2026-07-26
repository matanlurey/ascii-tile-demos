//! 33: `OneBit` Quest -- a three-column idle-RPG shell, reshaped for a phone.
//!
//! Every other demo in this batch picks one touch technique and shows it off
//! against a board or a hand of cards. This one exists to answer a different
//! question: what happens to a *desktop application layout* -- skills on the
//! left, the world in the middle, inventory on the right, the shape every
//! idle RPG (Cookie Clicker's descendants, `OneBit` Adventure) converges on --
//! when the same screen has to run on a phone. The three-column shell is not
//! the technique on show; the reflow from it down to a single-panel,
//! tab-barred phone layout is. Everything else in the file exists to give
//! that reflow something worth reshaping.
//!
//! Techniques on show:
//!
//! - **[`Shape`]-driven reflow, not a width breakpoint**
//!   ([`OnebitQuest::tick`]): desktop keeps all three columns full height;
//!   landscape keeps the same three columns but shorter, and each panel sheds
//!   its *least* important row first rather than letting the frame clip it
//!   (see [`draw_skills`](OnebitQuest::draw_skills) and
//!   [`draw_inventory`](OnebitQuest::draw_inventory)); portrait collapses to
//!   one panel plus a bottom tab bar. See the module docs on
//!   [`ui::touch::Shape`] for why this is three cases and not a single width
//!   cutoff.
//! - **A bottom tab bar in the thumb zone** ([`draw_tab_bar`]): the standard
//!   mobile answer to "three things used to live side by side, now only one
//!   fits". A hamburger or a side drawer both hide the other two panels
//!   behind an extra tap *and* behind a label the player has to remember; a
//!   tab bar shows all three destinations at once, at the bottom of the
//!   screen where a thumb already rests, and its target size is exactly a
//!   third of the width -- comfortably above [`touch::TAP_W`].
//! - **Multi-cell HP/MP orbs from the shade ramp** ([`draw_orb`]): the
//!   reference screenshot's round health/mana gauges, built from
//!   [`tilekit::glyphs::SHADE`] rather than a single colored glyph. An oval
//!   mask (narrower top and bottom rows, full middle) makes the shade blocks
//!   read as a globe rather than a rectangle, and the fill level is drawn
//!   bottom-up through the same ramp so half-full is a real, not rounded,
//!   quantity.
//! - **Drag-to-scroll that coexists with tap-to-select**
//!   ([`OnebitQuest::scroll_list_at`]): both the skills and inventory lists are
//!   taller than their panel on a phone. [`touch::Pointer`] already tells a
//!   tap from a drag by slop before either reaches the demo, so a press that
//!   travels arrives as `Gesture::drag`/`delta` and never also fires a
//!   hotspot; a press that does not travel arrives as `Gesture::tap` and
//!   never moves the scroll offset. The two paths are mutually exclusive by
//!   construction, which is what lets one finger both scroll a list and tap
//!   a row in it without either interaction stealing the other's input.
//! - **Always-on floating combat text** ([`CombatText`],
//!   [`OnebitQuest::simulate`]): the hero fights automatically on a timer
//!   independent of any input, so the world panel is never a static
//!   screenshot -- required for the animation-liveness check every demo in
//!   this gallery has to pass, and also just the right idle-RPG feel.
//! - **A destructive action behind a confirm step**
//!   ([`OnebitQuest::draw_discard_confirm`]): discarding an item is a tap or two
//!   away from browsing that item, and touch mis-taps are common. Tapping
//!   "discard" opens a two-button confirm row instead of discarding
//!   immediately, per the gallery's Into-the-Breach-style rule for
//!   irreversible actions.
//!
//! ```sh
//! cargo run --example 33_onebit_quest --features crossterm
//! cargo run --example 33_onebit_quest --features software
//! cargo run --example 33_onebit_quest --features gl
//! cargo run --example 33_onebit_quest  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{self, Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::glyphs::SHADE;
use tilekit::noise::Rng;
use tilekit::palette::{mix, rgb, scale};

/// Indigo page background: a hair off pure black so the panels, which sit a
/// shade lighter still, read as three raised surfaces rather than holes.
const INDIGO_BG: Color = rgb(12, 11, 24);
/// Panel background: one step up from [`INDIGO_BG`].
const INDIGO_PANEL: Color = rgb(19, 18, 36);
/// The gallery's usual accent is warm gold; this demo keeps it, since the
/// idle-RPG references (`OneBit` Adventure, Cookie Clicker) all use a warm
/// highlight against a cool shell.
const GOLD: Color = ui::ACCENT;
const HP_COLOR: Color = rgb(214, 92, 92);
const MP_COLOR: Color = rgb(92, 142, 224);
const XP_COLOR: Color = rgb(196, 164, 90);

/// Height of the bottom HP/MP/XP status band, in rows. Two rows for the
/// orbs' oval mask plus one for labels underneath -- see [`draw_orb`].
const STATUS_H: u16 = 4;
/// Height of the portrait tab bar. One more than [`touch::TAP_H`] so a
/// one-line label fits under the icon row without the tap target itself
/// shrinking below the touch minimum.
const TAB_H: u16 = 5;
/// Desktop/landscape skills column width.
const SKILLS_W: u16 = 26;
/// Desktop/landscape inventory column width.
const INV_W: u16 = 30;
/// The world column must keep at least this many columns before the side
/// columns give up any more of their own width; see [`side_widths`].
const WORLD_MIN: u16 = 20;

/// How long between automatic attacks, in world-seconds. Short enough that
/// the panel is visibly never idle, long enough that each hit is legible
/// rather than a blur.
const ATTACK_PERIOD: f32 = 1.1;
/// How many rows a combat text rises before it despawns.
const COMBAT_RISE_ROWS: f32 = 3.0;
/// How long a combat text stays on screen, in world-seconds.
const COMBAT_LIFE: f32 = 1.4;

/// A skill the hero can spend points on.
struct Skill {
    icon: char,
    name: &'static str,
    level: u32,
}

impl Skill {
    /// Cost of the next level: rises with level, so early levels are cheap
    /// and late ones are a real choice about which skill to favour.
    const fn cost(&self) -> u32 {
        2 + self.level
    }
}

/// An item held in the inventory.
struct Item {
    icon: char,
    name: &'static str,
    desc: &'static str,
    qty: u32,
}

/// A copy of the fields [`draw_inventory_row`](OnebitQuest::draw_inventory_row)
/// needs, taken before the loop calls a `&mut self` method.
///
/// `Item::name`/`Item::desc` are `&'static str`, so copying them out of a
/// `&Item` borrowed from `self.inventory` does not extend that borrow: the
/// copy is a `'static` reference, not one tied to `self`. That is what lets
/// the per-row draw call take `&mut self` (it needs to push hotspots) in the
/// same loop iteration that just read the item out of `self.inventory`.
#[derive(Clone, Copy)]
struct ItemView<'a> {
    icon: char,
    name: &'a str,
    desc: &'a str,
    qty: u32,
}

const fn item_view(item: &Item) -> ItemView<'static> {
    ItemView {
        icon: item.icon,
        name: item.name,
        desc: item.desc,
        qty: item.qty,
    }
}

/// A rising, fading damage/reward number over the world panel.
struct CombatText {
    /// Column offset from the hero, fixed for the text's lifetime.
    x: f32,
    /// Row offset from the hero's feet; decreases (rises) with age.
    y: f32,
    age: f32,
    text: String,
    color: Color,
}

/// Which single panel is visible in [`Shape::Portrait`], and which panel
/// currently has keyboard focus on the wider shapes (there all three are
/// visible at once, but arrow keys still have to mean something specific).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PanelKind {
    Skills,
    World,
    Items,
}

impl PanelKind {
    const ALL: [Self; 3] = [Self::Skills, Self::World, Self::Items];

    const fn label(self) -> &'static str {
        match self {
            Self::Skills => "Skills",
            Self::World => "World",
            Self::Items => "Items",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Skills => '\u{2660}', // spade: the "attack/training" glyph
            Self::World => '\u{263c}',  // sun: the field of battle
            Self::Items => '$',
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Skills => Self::World,
            Self::World => Self::Items,
            Self::Items => Self::Skills,
        }
    }
}

/// What tapping (or activating with Enter) a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// Switch the visible/focused panel (tab bar, or Tab key elsewhere).
    Tab(PanelKind),
    /// Spend a skill point on skill `usize`.
    LevelSkill(usize),
    /// Select item `usize`, showing its description.
    SelectItem(usize),
    /// Open the discard confirmation for item `usize`.
    AskDiscard(usize),
    /// Confirm the pending discard.
    ConfirmDiscard,
    /// Cancel the pending discard.
    CancelDiscard,
}

/// State: hero stats, skills, inventory, the combat-text swarm, scroll
/// offsets, and the input plumbing (`pointer`/`hotspots`) every touch demo
/// in this gallery shares.
pub struct OnebitQuest {
    fps: FpsMeter,
    pointer: Pointer,
    hotspots: Hotspots<Action>,
    time: f32,

    panel: PanelKind,
    skills_scroll: i32,
    items_scroll: i32,
    skills_sel: usize,
    items_sel: usize,

    skills: Vec<Skill>,
    skill_points: u32,
    inventory: Vec<Item>,
    selected_item: Option<usize>,
    pending_discard: Option<usize>,

    hp: f32,
    mp: f32,
    xp: f32,
    xp_max: f32,
    level: u32,
    gold: u32,

    attack_timer: f32,
    hero_lunge: f32,
    spawn_count: u32,
    combat_texts: Vec<CombatText>,
}

impl Default for OnebitQuest {
    fn default() -> Self {
        Self {
            fps: FpsMeter::new(),
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            time: 0.0,

            panel: PanelKind::World,
            skills_scroll: 0,
            items_scroll: 0,
            skills_sel: 0,
            items_sel: 0,

            skills: vec![
                Skill {
                    icon: '\u{2660}',
                    name: "Strike",
                    level: 3,
                },
                Skill {
                    icon: '\u{2663}',
                    name: "Vigor",
                    level: 2,
                },
                Skill {
                    icon: '\u{2665}',
                    name: "Vitality",
                    level: 1,
                },
                Skill {
                    icon: '\u{2666}',
                    name: "Fortune",
                    level: 0,
                },
                Skill {
                    icon: '\u{263c}',
                    name: "Focus",
                    level: 1,
                },
                Skill {
                    icon: '\u{25cb}',
                    name: "Haste",
                    level: 0,
                },
                Skill {
                    icon: '\u{00a7}',
                    name: "Wisdom",
                    level: 0,
                },
            ],
            skill_points: 4,
            inventory: vec![
                Item {
                    icon: '!',
                    name: "Potion",
                    desc: "Restores HP on use.",
                    qty: 5,
                },
                Item {
                    icon: '?',
                    name: "Ether",
                    desc: "Restores MP on use.",
                    qty: 2,
                },
                Item {
                    icon: '/',
                    name: "Iron Sword",
                    desc: "+3 attack while held.",
                    qty: 1,
                },
                Item {
                    icon: '[',
                    name: "Leather Vest",
                    desc: "+2 defense while worn.",
                    qty: 1,
                },
                Item {
                    icon: '*',
                    name: "Ruby",
                    desc: "Sells for a fair price.",
                    qty: 3,
                },
                Item {
                    icon: '=',
                    name: "Ring",
                    desc: "+1 to every skill.",
                    qty: 1,
                },
            ],
            selected_item: None,
            pending_discard: None,

            hp: 0.8,
            mp: 0.6,
            xp: 0.2,
            xp_max: 1.0,
            level: 12,
            gold: 340,

            attack_timer: ATTACK_PERIOD,
            hero_lunge: 0.0,
            spawn_count: 0,
            combat_texts: Vec::new(),
        }
    }
}

impl OnebitQuest {
    /// Advances the whole shell by `dt` world-seconds. Runs unconditionally,
    /// every tick, regardless of what panel is visible: an idle RPG's whole
    /// premise is that the fight continues while you are looking at your
    /// inventory, so the world panel must not be the only thing driving the
    /// clock forward.
    fn simulate(&mut self, dt: f32) {
        self.time += dt;

        // HP/MP breathe on their own clock rather than drifting monotonically,
        // which is enough to satisfy "this demo must visibly animate" without
        // needing a full combat resolution loop -- the loop is not the point,
        // the reflow is.
        self.hp = 0.72f32.mul_add(0.5 * (self.time * 0.9).sin(), 0.72);
        self.mp = 0.55f32.mul_add(0.5 * self.time.mul_add(0.6, 1.7).sin(), 0.55);

        self.xp = dt.mul_add(0.06, self.xp);
        if self.xp >= self.xp_max {
            self.xp -= self.xp_max;
            self.xp_max *= 1.1;
            self.level += 1;
        }

        self.hero_lunge = dt.mul_add(-3.0, self.hero_lunge).max(0.0);
        self.attack_timer -= dt;
        if self.attack_timer <= 0.0 {
            self.attack_timer += ATTACK_PERIOD;
            self.hero_lunge = 1.0;
            self.spawn_hit();
        }

        for ct in &mut self.combat_texts {
            ct.age += dt;
            ct.y = dt.mul_add(-(COMBAT_RISE_ROWS / COMBAT_LIFE), ct.y);
        }
        self.combat_texts.retain(|ct| ct.age < COMBAT_LIFE);
    }

    /// Spawns one damage number and, every third hit, a reward number too.
    ///
    /// Seeded from [`OnebitQuest::spawn_count`], a plain counter incremented
    /// once per call, rather than from wall-clock time or `self.time`: the
    /// gallery's determinism test renders a demo twice from the same frame
    /// sequence and diffs the output, so the random stream has to depend only
    /// on how many times this has fired, not on when.
    fn spawn_hit(&mut self) {
        let mut rng = Rng::new(0x0B17_0000 ^ self.spawn_count);
        self.spawn_count = self.spawn_count.wrapping_add(1);

        let dmg = 4 + rng.next_below(9);
        self.combat_texts.push(CombatText {
            x: rng.next_f32().mul_add(2.0, -1.0),
            y: 0.0,
            age: 0.0,
            text: format!("-{dmg}"),
            color: rgb(226, 96, 96),
        });

        if self.spawn_count.is_multiple_of(3) {
            self.gold += 2 + rng.next_below(5);
            self.combat_texts.push(CombatText {
                x: rng.next_f32().mul_add(2.0, -1.0),
                y: -0.6,
                age: 0.0,
                text: format!("+{}g", 2 + rng.next_below(5)),
                color: rgb(226, 196, 96),
            });
        }
    }

    fn level_up_skill(&mut self, index: usize) {
        if let Some(skill) = self.skills.get(index) {
            let cost = skill.cost();
            if self.skill_points >= cost {
                self.skill_points -= cost;
                self.skills[index].level += 1;
            }
        }
    }

    /// Removes one unit of item `index`, dropping the item entirely once its
    /// quantity reaches zero and fixing up whatever else pointed at that
    /// index.
    fn discard_item(&mut self, index: usize) {
        if index >= self.inventory.len() {
            return;
        }
        self.inventory[index].qty = self.inventory[index].qty.saturating_sub(1);
        if self.inventory[index].qty == 0 {
            self.inventory.remove(index);
            if self.selected_item == Some(index) {
                self.selected_item = None;
            }
            if self.items_sel >= self.inventory.len() && self.items_sel > 0 {
                self.items_sel -= 1;
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

    /// Keyboard parity for everything a finger can do: Tab cycles panels
    /// (which one is *visible* in portrait, which one *receives* arrow keys
    /// elsewhere), arrows move the active panel's selection or the pending
    /// confirm's choice, and Enter activates whatever is selected.
    fn handle_key(&mut self, code: KeyCode) {
        if let Some(index) = self.pending_discard {
            match code {
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => {
                    self.discard_item(index);
                    self.pending_discard = None;
                }
                KeyCode::Escape | KeyCode::Char('n' | 'N') => self.pending_discard = None,
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Tab => self.panel = self.panel.next(),
            KeyCode::Up | KeyCode::Char('w' | 'W') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('s' | 'S') => self.move_selection(1),
            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char('d' | 'D')
                if self.panel == PanelKind::Items && !self.inventory.is_empty() =>
            {
                self.pending_discard = Some(self.items_sel.min(self.inventory.len() - 1));
            }
            _ => {}
        }
    }

    const fn move_selection(&mut self, delta: i32) {
        match self.panel {
            PanelKind::Skills => {
                self.skills_sel = wrap_index(self.skills_sel, delta, self.skills.len());
            }
            PanelKind::Items => {
                self.items_sel = wrap_index(self.items_sel, delta, self.inventory.len());
                self.selected_item = Some(self.items_sel);
            }
            PanelKind::World => {}
        }
    }

    fn activate_selection(&mut self) {
        match self.panel {
            PanelKind::Skills => self.level_up_skill(self.skills_sel),
            PanelKind::Items => {
                if !self.inventory.is_empty() {
                    self.selected_item = Some(self.items_sel);
                }
            }
            PanelKind::World => {}
        }
    }

    /// Applies this frame's pointer gesture: taps resolve through
    /// [`Hotspots`], drags and wheel notches scroll whichever list the
    /// pointer is over. A press becomes exactly one or the other (see
    /// [`touch::Pointer`]'s slop rule), so a drag that starts on a list row
    /// can never also fire that row's tap action.
    fn apply_gesture(&mut self, gesture: Gesture) {
        if let Some(pos) = gesture.tap
            && let Some(&action) = self.hotspots.hit(pos)
        {
            self.apply_action(action);
        }

        if gesture.delta.1 != 0
            && let Some(pos) = gesture.drag
        {
            self.scroll_list_at(pos, gesture.delta.1);
        }
        if gesture.scroll != 0
            && let Some(pos) = gesture.hover
        {
            // One wheel notch reads as roughly three rows, matching typical
            // desktop wheel behaviour; touch never produces `scroll`, so this
            // path is desktop-only polish layered on top of the drag path
            // every backend shares.
            self.scroll_list_at(pos, -gesture.scroll * 3);
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Tab(panel) => self.panel = panel,
            Action::LevelSkill(i) => self.level_up_skill(i),
            Action::SelectItem(i) => {
                self.selected_item = Some(i);
                self.items_sel = i;
            }
            Action::AskDiscard(i) => self.pending_discard = Some(i),
            Action::ConfirmDiscard => {
                if let Some(i) = self.pending_discard.take() {
                    self.discard_item(i);
                }
            }
            Action::CancelDiscard => self.pending_discard = None,
        }
    }

    /// Scrolls whichever list panel's rect (registered as a hotspot in
    /// [`draw_skills`]/[`draw_inventory`]) contains `pos`, by `rows`.
    ///
    /// Hit-testing the drag against the same rects the taps use, rather than
    /// against "whichever panel is currently focused", is what lets a drag
    /// that starts over the skills panel scroll skills even while the
    /// keyboard focus (from a prior Tab press) sits on inventory: touch input
    /// always targets what is under the finger, never what a keyboard cursor
    /// last pointed at.
    fn scroll_list_at(&mut self, pos: Pos, rows: i32) {
        if self
            .hotspots
            .rect_where(is_skills_scroll_target)
            .is_some_and(|r| r.contains_pos(pos))
        {
            self.skills_scroll -= rows;
        } else if self
            .hotspots
            .rect_where(is_items_scroll_target)
            .is_some_and(|r| r.contains_pos(pos))
        {
            self.items_scroll -= rows;
        }
    }

    fn status_line(&self) -> String {
        format!(
            "lvl {}  {}g  panel: {}",
            self.level,
            self.gold,
            self.panel.label()
        )
    }
}

/// Moves `index` by `delta`, wrapping rather than clamping: a list of a
/// handful of rows is short enough that wrapping from the last entry back to
/// the first (and back) reads as a feature, not a surprise, and it means
/// Up/Down never dead-end at an edge the player has to notice and reverse
/// out of.
const fn wrap_index(index: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i32;
    (((index as i32 + delta) % len + len) % len) as usize
}

/// Marks the scroll-drag hotspot registered for the skills list. A sentinel
/// action rather than a second hit-testing structure: the skills panel's
/// whole rect is pushed once with this action so
/// [`OnebitQuest::scroll_list_at`] can find it with the same
/// [`Hotspots::rect_where`] machinery the tap targets use.
const fn is_skills_scroll_target(action: &Action) -> bool {
    matches!(action, Action::Tab(PanelKind::Skills))
}

/// See [`is_skills_scroll_target`].
const fn is_items_scroll_target(action: &Action) -> bool {
    matches!(action, Action::Tab(PanelKind::Items))
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

impl OnebitQuest {
    /// Desktop/landscape column widths for skills and inventory, shrunk
    /// proportionally (never below zero) once the two of them plus
    /// [`WORLD_MIN`] would not fit `total_w`. A fixed sidebar width is fine
    /// down to a landscape phone (158 columns); the 80-column headless
    /// snapshot grid is narrower than any real device this demo targets, and
    /// scaling both sides down together rather than picking one to starve is
    /// what keeps every panel showing *something* at that size instead of
    /// one panel eating the other's budget.
    fn side_widths(total_w: u16) -> (u16, u16) {
        let wanted = SKILLS_W + INV_W;
        let max_sides = total_w.saturating_sub(WORLD_MIN);
        if wanted <= max_sides || wanted == 0 {
            return (SKILLS_W, INV_W);
        }
        let scale = f32::from(max_sides) / f32::from(wanted);
        (
            (f32::from(SKILLS_W) * scale) as u16,
            (f32::from(INV_W) * scale) as u16,
        )
    }

    fn draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        self.hotspots.clear();
        let shape = Shape::of(content);

        let (main, status) = panel::split_bottom(content, STATUS_H);
        match shape {
            Shape::Portrait => {
                let (main, tabs) = panel::split_bottom(main, TAB_H);
                self.draw_active_panel(surface, main);
                self.draw_tab_bar(surface, tabs);
            }
            Shape::Landscape | Shape::Desktop => {
                let (skills_w, inv_w) = Self::side_widths(main.width());
                let (skills_area, rest) = panel::split_left(main, skills_w);
                let (world_area, inv_area) = panel::split_right(rest, inv_w);
                self.draw_skills(surface, skills_area, self.panel == PanelKind::Skills);
                self.draw_world(surface, world_area, self.panel == PanelKind::World);
                self.draw_inventory(surface, inv_area, self.panel == PanelKind::Items);
            }
        }
        self.draw_status(surface, status);
    }

    fn draw_active_panel(&mut self, surface: &mut Surface<'_>, area: Rect) {
        match self.panel {
            PanelKind::Skills => self.draw_skills(surface, area, true),
            PanelKind::World => self.draw_world(surface, area, true),
            PanelKind::Items => self.draw_inventory(surface, area, true),
        }
    }

    /// The bottom tab bar shown only in [`Shape::Portrait`].
    ///
    /// A tab bar beats a hamburger or a side drawer here for two reasons
    /// specific to touch, not just taste: first, all three destinations stay
    /// visible and labelled at once, so there is nothing to remember and
    /// nothing hidden behind an icon whose meaning has to be learned; second,
    /// it lives at the very bottom of the screen, which on a phone held
    /// one-handed is where the thumb already rests, while a hamburger
    /// conventionally sits top-left -- the one corner a right-handed thumb
    /// has to stretch furthest to reach. Each tab is a third of the width by
    /// [`TAB_H`] rows, comfortably past [`touch::TAP_W`]/`TAP_H` with room to
    /// spare, so this is the easiest target in the whole shell to hit, which
    /// is exactly right for the control a player uses every few seconds.
    fn draw_tab_bar(&mut self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() == 0 || area.height() == 0 {
            return;
        }
        let cols = panel::columns(area, 3, 0);
        for (kind, rect) in PanelKind::ALL.into_iter().zip(cols) {
            let active = kind == self.panel;
            let bg = if active {
                scale(GOLD, 0.22)
            } else {
                INDIGO_PANEL
            };
            let fg = if active { GOLD } else { ui::DIM };
            surface.fill_rect(rect, ' ', Style::new().bg(bg));
            let icon_y = rect.top() + rect.height() / 2 - 1;
            let label_y = icon_y + 1;
            print_centered(surface, rect, icon_y, &kind.icon().to_string(), fg, bg);
            print_centered(surface, rect, label_y, kind.label(), fg, bg);
            self.hotspots.push_tappable(rect, area, Action::Tab(kind));
        }
    }

    // -- Skills ---------------------------------------------------------

    /// Draws the skills panel: a point counter, then one row per skill
    /// (icon, name, level, a `+` control), scrollable once the list outgrows
    /// the panel.
    ///
    /// At [`Shape::Landscape`] height the panel cannot always afford a
    /// two-line row (name/level plus a cost caption underneath); rather than
    /// clip the last row's caption off mid-character, the row height drops to
    /// a single line and the cost caption is cut entirely. The cost is still
    /// discoverable (it is the number of points spent to press `+`, and the
    /// point counter above never disappears), whereas a level readable at a
    /// glance is what actually drives the decision of *which* skill to
    /// spend on -- so the caption is the row this sheds first.
    fn draw_skills(&mut self, surface: &mut Surface<'_>, area: Rect, focused: bool) {
        let inner = Panel::new()
            .title("Skills")
            .border(Border::Double)
            .badge(&format!("{} pts", self.skill_points))
            .focused(focused)
            .bg(INDIGO_PANEL)
            .draw(surface, area);
        // Registers the whole panel as a scroll target for drag/wheel input,
        // using the sentinel `Action::Tab(Skills)` (see
        // `is_skills_scroll_target`). Pushed first so any per-row hotspot
        // drawn afterward still wins a tap at the same point, matching
        // `Hotspots`' documented "latest registration wins" order.
        self.hotspots.push(inner, Action::Tab(PanelKind::Skills));
        if inner.width() < 6 || inner.height() < 2 {
            return;
        }

        let full_row = inner.height() >= self.skills.len() as u16 * 2;
        let row_h: u16 = if full_row { 2 } else { 1 };
        let visible_rows = inner.height() / row_h;
        let max_scroll = (self.skills.len() as i32 - i32::from(visible_rows)).max(0);
        self.skills_scroll = self.skills_scroll.clamp(0, max_scroll);

        for row in 0..visible_rows {
            let i = self.skills_scroll as usize + row as usize;
            let Some(skill) = self.skills.get(i) else {
                break;
            };
            let y = inner.top() + row * row_h;
            let selected = i == self.skills_sel && focused;
            let name_color = if selected { GOLD } else { ui::FG };

            let plus_w = touch::TAP_W.min(inner.width() / 3).max(3);
            let plus_rect =
                touch::tappable(Rect::new(inner.right() - plus_w, y, plus_w, row_h), inner);
            let afford = self.skill_points >= skill.cost();
            let plus_bg = if afford {
                scale(GOLD, 0.28)
            } else {
                scale(ui::DIM, 0.15)
            };
            let plus_ink = if afford { GOLD } else { ui::DIM };
            surface.fill_rect(plus_rect, ' ', Style::new().bg(plus_bg));
            print_centered(surface, plus_rect, plus_rect.top(), "+", plus_ink, plus_bg);

            let name_w = inner.width().saturating_sub(plus_w + 5);
            panel::spans(
                surface,
                (inner.left(), y),
                name_w,
                &[
                    Span::new(&skill.icon.to_string(), name_color),
                    Span::plain(" "),
                    Span::new(skill.name, name_color),
                ],
                INDIGO_PANEL,
            );
            let level_text = format!("{:>2}", skill.level);
            surface.print(
                (inner.left() + name_w, y),
                &level_text,
                Style::new().fg(ui::DIM).bg(INDIGO_PANEL),
            );

            if full_row && y + 1 < inner.bottom() {
                let cost_text = format!("cost {}", skill.cost());
                surface.print(
                    (inner.left(), y + 1),
                    &cost_text,
                    Style::new().fg(ui::DIM).bg(INDIGO_PANEL),
                );
            }

            self.hotspots
                .push_tappable(plus_rect, inner, Action::LevelSkill(i));
        }
    }

    // -- World ------------------------------------------------------------

    /// Draws the world panel: a horizon line, two trees, the hero (lunging on
    /// its attack beat), and the rising combat-text swarm.
    ///
    /// Nothing here reacts to [`Shape`] beyond the rect it is handed: the
    /// trees and the hero are placed relative to the panel's own centre and
    /// simply omitted if the panel is too small to hold them, which is the
    /// same graceful-degradation the sidebars use, just without needing a
    /// second tier of detail (there is no "compact hero").
    fn draw_world(&self, surface: &mut Surface<'_>, area: Rect, focused: bool) {
        let inner = Panel::new()
            .title("World")
            .border(Border::Double)
            .focused(focused)
            .bg(INDIGO_BG)
            .draw(surface, area);
        if inner.width() < 6 || inner.height() < 4 {
            return;
        }

        let ground_y = inner.bottom() - 2;
        surface.fill_rect(
            Rect::new(inner.left(), ground_y, inner.width(), 1),
            '\u{2500}',
            Style::new().fg(scale(ui::DIM, 0.7)).bg(INDIGO_BG),
        );

        if inner.width() >= 24 {
            Self::draw_tree(surface, inner.left() + 3, ground_y);
            Self::draw_tree(surface, inner.right() - 5, ground_y);
        }

        let hero_x = inner.left() + inner.width() / 2;
        self.draw_hero(surface, hero_x, ground_y);

        for ct in &self.combat_texts {
            let cx = i32::from(hero_x) + ct.x.round() as i32;
            let cy = i32::from(ground_y) - 2 + ct.y.round() as i32;
            if cx < i32::from(inner.left()) || cx >= i32::from(inner.right()) {
                continue;
            }
            if cy < i32::from(inner.top()) || cy >= i32::from(inner.bottom()) {
                continue;
            }
            let fade = (ct.age / COMBAT_LIFE).clamp(0.0, 1.0);
            let color = mix(ct.color, INDIGO_BG, fade * 0.85);
            surface.print(
                (cx as u16, cy as u16),
                &ct.text,
                Style::new().fg(color).bg(INDIGO_BG),
            );
        }
    }

    /// A multi-cell tree: a shaded canopy over a trunk, anchored so `base_y`
    /// is the row the trunk meets the ground. One glyph would be a single
    /// interactive-scale unit doing the work a whole scene prop should do;
    /// this is ~3x4 cells instead, matching the "board tile is multi-cell"
    /// rule the whole gallery follows even where nothing here is tappable.
    fn draw_tree(surface: &mut Surface<'_>, cx: u16, base_y: u16) {
        let green = rgb(76, 132, 84);
        let dark_green = rgb(46, 92, 58);
        let trunk = rgb(96, 70, 52);
        let rows: [(&[i32], char, Color); 3] = [
            (&[0], '\u{2593}', dark_green),
            (&[-1, 0, 1], '\u{2592}', green),
            (&[-2, -1, 0, 1, 2], '\u{2591}', dark_green),
        ];
        for (i, (offsets, glyph, color)) in rows.iter().enumerate() {
            let y = base_y - 1 - (rows.len() - i) as u16;
            for &dx in *offsets {
                let x = (i32::from(cx) + dx) as u16;
                surface.put((x, y), *glyph, Style::new().fg(*color).bg(INDIGO_BG));
            }
        }
        surface.put(
            (cx, base_y - 1),
            '\u{2502}',
            Style::new().fg(trunk).bg(INDIGO_BG),
        );
    }

    /// The hero figure: a three-row ASCII stick pose that leans forward on
    /// `hero_lunge` (1.0 right after a hit, decaying to 0 before the next),
    /// which is the one piece of directly-authored motion in the panel --
    /// everything else animates through position/opacity, this one swaps its
    /// own glyphs so the attack beat reads as a strike rather than a twitch.
    fn draw_hero(&self, surface: &mut Surface<'_>, cx: u16, base_y: u16) {
        let lunge = self.hero_lunge > 0.4;
        let color = rgb(224, 208, 160);
        let (head, arms, legs) = if lunge {
            ('O', "/|-", "/ \\")
        } else {
            ('O', "/|\\", "/ \\")
        };
        surface.put((cx, base_y - 3), head, Style::new().fg(color).bg(INDIGO_BG));
        surface.print(
            (cx - 1, base_y - 2),
            arms,
            Style::new().fg(color).bg(INDIGO_BG),
        );
        surface.print(
            (cx - 1, base_y - 1),
            legs,
            Style::new().fg(color).bg(INDIGO_BG),
        );
    }

    // -- Inventory --------------------------------------------------------

    /// Draws the inventory panel: one row per item (quantity, icon, name,
    /// discard control), a description line under the selected/focused row,
    /// and the confirm-discard overlay when one is pending.
    ///
    /// The description line is the row this panel sheds first once height is
    /// scarce (see [`draw_skills`]'s doc comment for the general rule): a row
    /// is legible as "here is an item" from its icon and name alone, and a
    /// tapped item still opens its full description in the detail strip
    /// below the list, so nothing is unreachable, only not shown for free.
    fn draw_inventory(&mut self, surface: &mut Surface<'_>, area: Rect, focused: bool) {
        let inner = Panel::new()
            .title("Inventory")
            .border(Border::Double)
            .badge(&format!("{}", self.inventory.len()))
            .focused(focused)
            .bg(INDIGO_PANEL)
            .draw(surface, area);
        if inner.width() < 8 || inner.height() < 2 {
            return;
        }

        if let Some(index) = self.pending_discard {
            self.draw_discard_confirm(surface, inner, index);
            return;
        }

        // The detail strip (selected item's description) claims up to two
        // rows off the bottom before the list gets what remains, but only
        // when there is enough height to spare a list at all -- otherwise a
        // very short panel would show a description and nothing to select
        // one from.
        let detail_h = if self.selected_item.is_some() && inner.height() > 4 {
            2
        } else {
            0
        };
        let (list_area, detail_area) = panel::split_bottom(inner, detail_h);

        self.hotspots.push(inner, Action::Tab(PanelKind::Items));

        let row_h: u16 = if list_area.height() >= self.inventory.len() as u16 * 2 {
            2
        } else {
            1
        };
        let visible_rows = list_area.height().checked_div(row_h).unwrap_or(0);
        let max_scroll = (self.inventory.len() as i32 - i32::from(visible_rows)).max(0);
        self.items_scroll = self.items_scroll.clamp(0, max_scroll);

        for row in 0..visible_rows {
            let i = self.items_scroll as usize + row as usize;
            let Some(item) = self.inventory.get(i) else {
                break;
            };
            let y = list_area.top() + row * row_h;
            let selected = self.selected_item == Some(i);
            self.draw_inventory_row(surface, list_area, y, row_h, i, item_view(item), selected);
        }

        if detail_h > 0
            && let Some(index) = self.selected_item
            && let Some(item) = self.inventory.get(index)
        {
            surface.fill_rect(detail_area, ' ', Style::new().bg(INDIGO_PANEL));
            surface.print(
                (detail_area.left(), detail_area.top()),
                truncate_for(item.desc, detail_area.width()),
                Style::new().fg(ui::DIM).bg(INDIGO_PANEL),
            );
        }
    }

    /// One inventory row: quantity, icon, name, an optional description on a
    /// second line, and the discard control -- split out of
    /// [`draw_inventory`] purely to keep that function's body under the
    /// gallery's line-count lint; the two always change together.
    #[allow(clippy::too_many_arguments)]
    fn draw_inventory_row(
        &mut self,
        surface: &mut Surface<'_>,
        list_area: Rect,
        y: u16,
        row_h: u16,
        i: usize,
        item: ItemView<'_>,
        selected: bool,
    ) {
        let name_color = if selected { GOLD } else { ui::FG };

        let discard_w = touch::TAP_W.min(list_area.width() / 3).max(3);
        let discard_rect = touch::tappable(
            Rect::new(list_area.right() - discard_w, y, discard_w, row_h),
            list_area,
        );
        surface.fill_rect(discard_rect, ' ', Style::new().bg(scale(HP_COLOR, 0.2)));
        print_centered(
            surface,
            discard_rect,
            discard_rect.top(),
            "x",
            HP_COLOR,
            scale(HP_COLOR, 0.2),
        );

        let qty_text = format!("{:>2}x", item.qty);
        let qty_w = qty_text.chars().count() as u16;
        let name_w = list_area.width().saturating_sub(discard_w + qty_w + 2);
        panel::spans(
            surface,
            (list_area.left(), y),
            qty_w,
            &[Span::new(&qty_text, ui::DIM)],
            INDIGO_PANEL,
        );
        panel::spans(
            surface,
            (list_area.left() + qty_w + 1, y),
            name_w,
            &[
                Span::new(&item.icon.to_string(), name_color),
                Span::plain(" "),
                Span::new(item.name, name_color),
            ],
            INDIGO_PANEL,
        );

        if row_h == 2 && y + 1 < list_area.bottom() {
            surface.print(
                (list_area.left() + 3, y + 1),
                truncate_for(item.desc, list_area.width().saturating_sub(discard_w + 4)),
                Style::new().fg(ui::DIM).bg(INDIGO_PANEL),
            );
        }

        let row_rect = Rect::new(
            list_area.left(),
            y,
            list_area.width().saturating_sub(discard_w),
            row_h,
        );
        self.hotspots.push(row_rect, Action::SelectItem(i));
        self.hotspots
            .push_tappable(discard_rect, list_area, Action::AskDiscard(i));
    }

    /// The two-button confirm row shown in place of the inventory list once
    /// a discard is pending: destroying an item cannot be undone by this
    /// demo (there is no undo stack for a consumable), so it gets a confirm
    /// step instead, per the gallery's rule that irreversible actions need
    /// one or the other.
    fn draw_discard_confirm(&mut self, surface: &mut Surface<'_>, inner: Rect, index: usize) {
        let name = self.inventory.get(index).map_or("item", |item| item.name);
        let prompt_h = 2.min(inner.height());
        let (prompt_area, buttons_area) = panel::split_top(inner, prompt_h);
        surface.fill_rect(inner, ' ', Style::new().bg(INDIGO_PANEL));
        surface.print(
            (prompt_area.left(), prompt_area.top()),
            truncate_for(&format!("Discard {name}?"), prompt_area.width()),
            Style::new().fg(ui::FG).bg(INDIGO_PANEL),
        );
        if prompt_area.height() > 1 {
            surface.print(
                (prompt_area.left(), prompt_area.top() + 1),
                "This cannot be undone.",
                Style::new().fg(ui::DIM).bg(INDIGO_PANEL),
            );
        }
        if buttons_area.height() == 0 {
            return;
        }
        let cols = panel::columns(buttons_area, 2, 1);
        let confirm_rect = touch::tappable(cols[0], buttons_area);
        let cancel_rect = touch::tappable(cols[1], buttons_area);
        surface.fill_rect(confirm_rect, ' ', Style::new().bg(scale(HP_COLOR, 0.3)));
        print_centered(
            surface,
            confirm_rect,
            confirm_rect.top() + confirm_rect.height() / 2,
            "Discard",
            HP_COLOR,
            scale(HP_COLOR, 0.3),
        );
        surface.fill_rect(cancel_rect, ' ', Style::new().bg(scale(ui::DIM, 0.2)));
        print_centered(
            surface,
            cancel_rect,
            cancel_rect.top() + cancel_rect.height() / 2,
            "Cancel",
            ui::FG,
            scale(ui::DIM, 0.2),
        );
        self.hotspots.push(confirm_rect, Action::ConfirmDiscard);
        self.hotspots.push(cancel_rect, Action::CancelDiscard);
    }

    /// Draws the shared HP/MP/XP status band, present under every [`Shape`]:
    /// it is read-only status, not a control, so it does not need to move to
    /// the thumb zone the way the tab bar does -- only *actionable* things
    /// belong there.
    fn draw_status(&self, surface: &mut Surface<'_>, area: Rect) {
        panel::band(surface, area);
        if area.width() < 20 || area.height() == 0 {
            return;
        }
        let orb_w = 9u16.min(area.width() / 4);
        let (hp_area, rest) = panel::split_left(area, orb_w);
        let (rest, mp_area) = panel::split_right(rest, orb_w);

        draw_orb(surface, hp_area, self.hp, HP_COLOR, "HP");
        draw_orb(surface, mp_area, self.mp, MP_COLOR, "MP");

        if rest.width() < 6 || rest.height() == 0 {
            return;
        }
        let label = format!("Lv {}", self.level);
        surface.print(
            (rest.left() + 1, rest.top()),
            &label,
            Style::new().fg(GOLD).bg(ui::CHROME_BG),
        );
        let bar_y = rest.top() + rest.height() / 2;
        let bar_w = rest.width().saturating_sub(2);
        if bar_w > 0 {
            panel::bar(
                surface,
                (rest.left() + 1, bar_y),
                bar_w,
                self.xp / self.xp_max,
                XP_COLOR,
                scale(XP_COLOR, 0.18),
            );
        }
    }
}

/// Draws one HP/MP orb: a small oval built from [`SHADE`], filled bottom-up
/// by `t` in `0.0..=1.0`.
///
/// The oval mask (narrower top/bottom rows, full middle row) is what makes
/// this read as a globe instead of a bar chart turned sideways -- the shape
/// the reference screenshot's orbs use. Fill is drawn through the same ramp
/// [`panel::bar`] uses for its half-cell precision, one row at a time from
/// the bottom, so a half-full orb is a real fractional row, not a value
/// rounded to the nearest whole one.
fn draw_orb(surface: &mut Surface<'_>, area: Rect, t: f32, color: Color, label: &str) {
    if area.width() < 3 || area.height() < 2 {
        return;
    }
    let orb_h = area.height().saturating_sub(1).clamp(1, 3);
    let orb_w = area.width().min(9);
    let cx = area.left() + area.width() / 2;
    let left = cx.saturating_sub(orb_w / 2);

    // Row insets, widest in the middle row, narrower at top and bottom, so a
    // 3-row orb reads as round rather than square. A taller orb simply
    // repeats the middle (full-width) row rather than growing the mask, which
    // keeps this readable at both a cramped 80-column layout and a roomy one.
    let t = t.clamp(0.0, 1.0);
    let empty = scale(color, 0.12);
    for row in 0..orb_h {
        let is_edge = row == 0 || row == orb_h - 1;
        let inset = u16::from(is_edge && orb_w > 4);
        let w = orb_w.saturating_sub(inset * 2);
        if w == 0 {
            continue;
        }
        let x0 = left + inset;
        // Fraction of the orb's fillable height this row represents, counted
        // from the bottom (row `orb_h - 1`) upward.
        let row_from_bottom = orb_h - 1 - row;
        let row_t = (t * f32::from(orb_h) - f32::from(row_from_bottom)).clamp(0.0, 1.0);
        let glyph = ramp_char(row_t);
        let fg = if row_t > 0.0 { color } else { empty };
        let y = area.top() + row;
        for x in x0..(x0 + w) {
            surface.put((x, y), glyph, Style::new().fg(fg).bg(ui::CHROME_BG));
        }
    }

    if area.height() > orb_h {
        let text = format!("{label} {:>3.0}%", t * 100.0);
        let text_w = text.chars().count() as u16;
        let pad = area.width().saturating_sub(text_w) / 2;
        surface.print(
            (area.left() + pad, area.top() + orb_h),
            truncate_for(&text, area.width()),
            Style::new().fg(ui::DIM).bg(ui::CHROME_BG),
        );
    }
}

/// Picks a [`SHADE`] glyph for `t`, always returning at least the lightest
/// shade for any positive fraction: a row that is 5% full should still show
/// as "a little" rather than rounding down to nothing, which is what a plain
/// `SHADE[(t * 4.0) as usize]` index would do.
fn ramp_char(t: f32) -> char {
    if t <= 0.0 {
        SHADE[0]
    } else {
        let idx = ((t * 4.0).ceil() as usize).clamp(1, SHADE.len() - 1);
        SHADE[idx]
    }
}

fn print_centered(surface: &mut Surface<'_>, rect: Rect, y: u16, text: &str, fg: Color, bg: Color) {
    let text = truncate_for(text, rect.width());
    let text_len = text.chars().count() as u16;
    let pad = rect.width().saturating_sub(text_len) / 2;
    surface.print((rect.left() + pad, y), text, Style::new().fg(fg).bg(bg));
}

fn truncate_for(text: &str, width: u16) -> &str {
    retroglyph_widgets::truncate(text, usize::from(width))
}

impl Demo for OnebitQuest {
    const NAME: &'static str = "33_onebit_quest";
    const TITLE: &'static str = "33 OneBit Quest";
    const BLURB: &'static str = "A three-column idle-RPG shell that reshapes into a phone tab bar.";
    const GRID: (u16, u16) = (156, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Tab", "cycle panel"),
            ("Up/Down", "move selection"),
            ("Enter", "activate"),
            ("D", "discard selected item"),
            ("Y/N", "confirm/cancel discard"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();
        self.apply_gesture(gesture);

        self.simulate(dt);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(INDIGO_BG));

        self.draw(&mut surface, content);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

ascii_tile_demos::demo_main!(OnebitQuest);

#[cfg(test)]
mod tests {
    use super::{Action, OnebitQuest, PanelKind, Shape, wrap_index};
    use retroglyph_core::{Grid, Rect, Surface};

    /// One demo instance per [`Shape`], drawn into a scratch grid the size
    /// [`touch`](super::touch)'s module docs give for that shape, so this
    /// pins the one property a snapshot at a single fixed size cannot: that
    /// every reshape branch runs without panicking and puts something other
    /// than blank space on the screen.
    #[test]
    fn every_shape_draws_something() {
        for (w, h) in [(73, 79), (158, 36), (160, 50)] {
            let mut grid = Grid::new(w, h);
            let area = Rect::new(0, 0, w, h);
            let mut demo = OnebitQuest::default();
            demo.simulate(1.5); // let combat text and the attack pose kick in
            {
                let mut surface = Surface::new(&mut grid, area, 0);
                demo.draw(&mut surface, area);
            }
            let shape = Shape::of(area);
            let non_blank = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter_map(|(x, y)| grid.tile(0, (x, y)))
                .filter(|t| t.glyph() != ' ')
                .count();
            assert!(non_blank > 20, "{shape:?} at {w}x{h} drew almost nothing");
        }
    }

    #[test]
    fn portrait_shows_the_tab_bar_and_only_one_panel() {
        let (w, h) = (73, 79);
        let mut grid = Grid::new(w, h);
        let area = Rect::new(0, 0, w, h);
        assert_eq!(Shape::of(area), Shape::Portrait);
        let mut demo = OnebitQuest {
            panel: PanelKind::Skills,
            ..Default::default()
        };
        {
            let mut surface = Surface::new(&mut grid, area, 0);
            demo.draw(&mut surface, area);
        }
        // The tab bar registers exactly one hotspot per tab.
        let tabs = [PanelKind::Skills, PanelKind::World, PanelKind::Items]
            .into_iter()
            .filter(|&k| demo.hotspots.rect_where(|a| *a == Action::Tab(k)).is_some())
            .count();
        assert_eq!(tabs, 3, "all three tabs must be reachable in portrait");
    }

    #[test]
    fn selection_wraps_instead_of_clamping_at_either_edge() {
        assert_eq!(wrap_index(0, -1, 5), 4);
        assert_eq!(wrap_index(4, 1, 5), 0);
        assert_eq!(wrap_index(2, 1, 5), 3);
    }

    #[test]
    fn a_discard_confirmation_must_be_explicitly_confirmed() {
        let mut demo = OnebitQuest::default();
        let before = demo.inventory.len();
        let qty_before = demo.inventory[0].qty;
        demo.pending_discard = Some(0);
        demo.apply_action(Action::CancelDiscard);
        assert_eq!(demo.inventory.len(), before, "cancel must not discard");
        assert_eq!(demo.inventory[0].qty, qty_before);

        demo.pending_discard = Some(0);
        demo.apply_action(Action::ConfirmDiscard);
        assert_eq!(
            demo.inventory[0].qty,
            qty_before - 1,
            "confirm must discard one"
        );
    }

    #[test]
    fn leveling_a_skill_spends_exactly_its_cost() {
        let mut demo = OnebitQuest::default();
        // Fortune starts at level 0 (cost 2), affordable against the
        // starting 4 points; Strike (level 3, cost 5) is not, which is the
        // scenario the next test covers.
        let points_before = demo.skill_points;
        let cost = demo.skills[3].cost();
        let level_before = demo.skills[3].level;
        demo.level_up_skill(3);
        assert_eq!(demo.skill_points, points_before - cost);
        assert_eq!(demo.skills[3].level, level_before + 1);
    }

    #[test]
    fn leveling_a_skill_that_cannot_be_afforded_does_nothing() {
        let mut demo = OnebitQuest::default();
        let points_before = demo.skill_points;
        let level_before = demo.skills[0].level;
        assert!(
            demo.skills[0].cost() > points_before,
            "test assumes Strike is unaffordable"
        );
        demo.level_up_skill(0);
        assert_eq!(demo.skill_points, points_before);
        assert_eq!(demo.skills[0].level, level_before);
    }
}
