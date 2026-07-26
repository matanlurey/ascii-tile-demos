//! 29: Ship breach -- a starship interior as a system-allocation puzzle, not a
//! map to walk.
//!
//! Every other demo in this gallery projects the world from above: a hex
//! field, a deck plan, a political map. A ship in combat (FTL, Faster Than
//! Light) is drawn differently on purpose. The player never needs to know
//! how far apart two rooms are in metres; they need to know which rooms share
//! a wall, which crew member can reach which fire fastest, and which room a
//! shot is about to land in. That is a *graph* wearing a floor plan, not a
//! terrain map, so the "camera" here is fixed and schematic rather than
//! scrollable: every room is always fully visible, at a size big enough to
//! tap, and the corridors exist only to say which rooms are adjacent. A
//! demo that tried to make this pannable or zoomable would be solving a
//! problem the genre does not have.
//!
//! The reactor's power is spent as discrete pips, not a continuous slider,
//! for the same reason FTL itself draws it that way: power is a countable,
//! fought-over resource with hard integer costs (a shield bubble needs
//! exactly one point; weapons need exactly two), so the UI has to answer "how
//! many can I afford" at a glance. A continuous bar can show 61% full; it
//! cannot show "one more pip and Shields drops to zero", which is the actual
//! decision the reactor panel exists to support.
//!
//! Fire spreads deterministically: rather than rolling dice against the wall
//! clock, [`ShipBreach::check_fire_spread`] only consults
//! [`tilekit::noise::Rng`], reseeded from a monotonically incrementing check
//! counter rather than time or a stored generator. Two runs fed the same
//! sequence of frame deltas make the same sequence of spread rolls in the
//! same order, which is what lets the snapshot tests render this demo twice
//! and diff the result.
//!
//! Pause is drawn as a large control in the thumb zone, not offered only as
//! a keyboard shortcut, because FTL's whole combat model is real-time-with-
//! pause: on a desk with a keyboard, pausing to reassign power is a
//! reflex; on a phone, if the only way to stop the clock is a key nobody's
//! finger is touching, the puzzle this demo exists to show off becomes
//! unplayable exactly where it matters most.
//!
//! Techniques on show:
//!
//! - **Discrete reactor pips** ([`ShipBreach::draw_reactor`]): each system's
//!   power is drawn as a row of `[fi]`/`.` glyphs and the reactor enforces a
//!   hard cap; adding power past the cap fails visibly (a red flash and a log
//!   line) rather than silently stealing from another system, so causality
//!   stays legible without having to watch every other gauge.
//! - **A ship cross-section as a fixed adjacency graph**
//!   ([`build_hull`], [`CORRIDORS`]): rooms are carved directly into a plate
//!   grid at pre-chosen coordinates (no BSP -- a warship's rooms are
//!   engineered, not grown), and walls are derived from floor adjacency the
//!   same way `21_deck_plan.rs` derives them, so every corner autotiles
//!   correctly with no per-room special-casing.
//! - **Manned systems** ([`ShipBreach::manned`]): a system whose room
//!   contains a crew member charges weapons, recharges shields, and
//!   replenishes oxygen faster, and the demo's own log narrates the
//!   difference so the effect isn't only a number nobody reads.
//! - **Deterministic fire spread** ([`ShipBreach::check_fire_spread`]): see
//!   above.
//! - **Discrete recharging shield bubbles** ([`ShipBreach::draw_status`]):
//!   `shield_charge` is a continuous float internally (so the recharge
//!   animates smoothly frame to frame) but is only ever *shown* rounded down
//!   to whole bubbles, because a shield either blocks a hit or it doesn't --
//!   a fractional bubble is not a thing the player can spend.
//! - **Tap-select-then-tap-target** ([`Action`], [`ShipBreach::handle_tap`]):
//!   crew are sent to a room, and weapons are fired at one, by selecting the
//!   actor first and the destination second. A dense grid of 7-9 cell rooms
//!   has no room left for a dragged token that doesn't occlude its own
//!   drop target, so two taps are the reliable path; drag is offered too
//!   (`ShipBreach::handle_drag`), for the desk, where a dragged token does
//!   not hide anything.
//! - **A live target highlight before release**
//!   ([`ShipBreach::hover_target`]): while a weapon is selected, the enemy
//!   room currently under the held pointer is outlined *before* the tap that
//!   fires completes, because firing cannot be undone and the two-tap flow
//!   is only a real confirmation if the second tap's target is visible in
//!   advance.
//!
//! ```sh
//! cargo run --example 29_ship_breach --features crossterm
//! cargo run --example 29_ship_breach --features software
//! cargo run --example 29_ship_breach --features gl
//! cargo run --example 29_ship_breach  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Frame, Pos, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Log, Span};
use ascii_tile_demos::ui::touch::{Gesture, Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::autotile::{BOX_SINGLE, mask4};
use tilekit::noise::Rng;
use tilekit::palette::{mix, rgb};

/// Width/height of the player hull's plate grid, in local cells. Rooms are
/// carved directly at fixed coordinates below rather than generated, because
/// a warship's interior is engineered around its systems, not grown by a
/// space-filling algorithm the way `21_deck_plan.rs`'s corridors are.
const PSHIP_W: i32 = 31;
/// See [`PSHIP_W`].
const PSHIP_H: i32 = 14;

/// Enemy hull plate grid. Smaller: it only needs to exist as something to
/// aim at, not to walk around in.
const ESHIP_W: i32 = 31;
/// See [`ESHIP_W`].
const ESHIP_H: i32 = 7;

/// Every player room is exactly [`touch::TAP_W`](ui::touch::TAP_W) wide and
/// taller than [`touch::TAP_H`](ui::touch::TAP_H), so a room is a legal touch
/// target as drawn, with no [`Hotspots::push_tappable`] growth needed that
/// would otherwise have to eat into a neighbour only one plate away.
const ROOM_W: i32 = 9;
/// See [`ROOM_W`].
const ROOM_H: i32 = 5;

/// One plate of a hull's cross-section.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Plate {
    Void,
    Floor,
    Wall,
}

/// A room's footprint in local plate coordinates.
#[derive(Clone, Copy)]
struct Box2 {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Box2 {
    const fn right(self) -> i32 {
        self.x + self.w
    }

    const fn bottom(self) -> i32 {
        self.y + self.h
    }

    fn center(self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        )
    }

    fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x as f32
            && px < self.right() as f32
            && py >= self.y as f32
            && py < self.bottom() as f32
    }
}

/// One of the player ship's six rooms, each of which doubles as the system it
/// houses -- a real difference from `21_deck_plan.rs`, where rooms are just
/// places. Here the room *is* the interface for its system: its power pips
/// live in the reactor panel, but its floor is where fire, crew, and repair
/// actually happen.
struct PlayerRoom {
    name: &'static str,
    rect: Box2,
    power: u8,
    max_power: u8,
    /// 0 = no fire, 1 = fully involved. Continuous so the tint animates
    /// smoothly; only ever compared against thresholds for gameplay effects.
    fire: f32,
}

/// A room on the enemy hull: a target and nothing else.
struct EnemyRoom {
    name: &'static str,
    rect: Box2,
    hp: f32,
}

/// A crew member walking the player ship.
struct Crew {
    name: &'static str,
    x: f32,
    y: f32,
    target_room: Option<usize>,
}

/// One of the ship's two weapon mounts.
struct Weapon {
    name: &'static str,
    /// 0..1. Charges only while its room has power (see
    /// [`ShipBreach::simulate`]), which is what makes the reactor panel and
    /// the weapons panel feel like one system rather than two.
    charge: f32,
}

impl Weapon {
    fn ready(&self) -> bool {
        self.charge >= 1.0
    }
}

/// Adjacent room-index pairs on the player hull, used both to carve
/// corridors between them and, later, as the graph fire spreads across. One
/// list serving both purposes is deliberate: the same corridors that let
/// crew reach a fire are the only paths the fire itself is allowed to
/// travel, so the two systems can never disagree about what "next door"
/// means.
const CORRIDORS: [(usize, usize); 7] = [(0, 1), (1, 2), (3, 4), (4, 5), (0, 3), (1, 4), (2, 5)];

/// Which system index does what. Rooms and systems are the same six things
/// here (no ship in this genre has a system without a room, or a room
/// without a job), so one index space serves both.
const ROOM_PILOTING: usize = 0;
const ROOM_WEAPONS: usize = 1;
const ROOM_SHIELDS: usize = 2;
const ROOM_ENGINES: usize = 3;
const ROOM_OXYGEN: usize = 4;
const ROOM_MEDBAY: usize = 5;

/// Total power the reactor can distribute. Deliberately less than the sum of
/// every system's own cap (3+4+4+4+3+2 = 20), so scarcity is guaranteed from
/// the first frame: there is always at least one upgrade the player cannot
/// afford without taking power from something else.
const REACTOR_MAX: u8 = 14;

/// What tapping (or a keyboard binding for) a hotspot means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    AddPower(usize),
    RemovePower(usize),
    SelectCrew(usize),
    SendCrewTo(usize),
    SelectWeapon(usize),
    FireAt(usize),
    TogglePause,
}

/// State: both hulls, the reactor, crew, weapons, hazards, and the touch
/// plumbing every interface-scale demo in this batch shares.
pub struct ShipBreach {
    time: f32,
    paused: bool,
    reactor_flash: f32,
    rooms: [PlayerRoom; 6],
    player_plates: Vec<Plate>,
    enemy_rooms: [EnemyRoom; 3],
    enemy_plates: Vec<Plate>,
    crew: Vec<Crew>,
    selected_crew: Option<usize>,
    dragging_crew: Option<usize>,
    weapons: [Weapon; 2],
    selected_weapon: Option<usize>,
    /// Which enemy room the keyboard (rather than the pointer) currently has
    /// targeted, cycled with `[`/`]` and fired at with `F`. Independent of
    /// `hover_at`, which only ever reflects the live pointer.
    keyboard_target: Option<usize>,
    shield_charge: f32,
    hull: f32,
    oxygen: f32,
    breach_room: Option<usize>,
    breach_triggered: bool,
    breach_repair: f32,
    volley_timer: f32,
    volley_count: u32,
    fire_check_timer: f32,
    fire_check_count: u32,
    enemy_defeated: bool,
    log: Log,
    pointer: Pointer,
    hover_at: Option<Pos>,
    hotspots: Hotspots<Action>,
    fps: FpsMeter,
}

/// The player ship's six rooms at their starting power and health, in the
/// fixed layout described at the top of the file.
const fn initial_rooms() -> [PlayerRoom; 6] {
    [
        PlayerRoom {
            name: "Piloting",
            rect: Box2 {
                x: 1,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 2,
            max_power: 3,
            fire: 0.0,
        },
        PlayerRoom {
            name: "Weapons",
            rect: Box2 {
                x: 11,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 2,
            max_power: 4,
            fire: 0.0,
        },
        PlayerRoom {
            name: "Shields",
            rect: Box2 {
                x: 21,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 2,
            max_power: 4,
            fire: 0.0,
        },
        PlayerRoom {
            name: "Engines",
            rect: Box2 {
                x: 1,
                y: 8,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 2,
            max_power: 4,
            // Starts alight, so the fire-spread and repair mechanics are
            // both visible from the very first frame rather than only
            // after a scripted event fires later.
            fire: 0.4,
        },
        PlayerRoom {
            name: "Oxygen",
            rect: Box2 {
                x: 11,
                y: 8,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 2,
            max_power: 3,
            fire: 0.0,
        },
        PlayerRoom {
            name: "Medbay",
            rect: Box2 {
                x: 21,
                y: 8,
                w: ROOM_W,
                h: ROOM_H,
            },
            power: 1,
            max_power: 2,
            fire: 0.0,
        },
    ]
}

/// The enemy ship's three rooms, all healthy.
const fn initial_enemy_rooms() -> [EnemyRoom; 3] {
    [
        EnemyRoom {
            name: "Bridge",
            rect: Box2 {
                x: 1,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            hp: 1.0,
        },
        EnemyRoom {
            name: "Wpn Bay",
            rect: Box2 {
                x: 11,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            hp: 1.0,
        },
        EnemyRoom {
            name: "Engine",
            rect: Box2 {
                x: 21,
                y: 1,
                w: ROOM_W,
                h: ROOM_H,
            },
            hp: 1.0,
        },
    ]
}

/// Starting crew, one per role room that matters most early: piloting,
/// engines, weapons, and medbay.
fn initial_crew(rooms: &[PlayerRoom; 6]) -> Vec<Crew> {
    [
        ("Mora", ROOM_PILOTING),
        ("Tarn", ROOM_ENGINES),
        ("Ilse", ROOM_WEAPONS),
        ("Bax", ROOM_MEDBAY),
    ]
    .into_iter()
    .map(|(name, room)| {
        let (cx, cy) = rooms[room].rect.center();
        Crew {
            name,
            x: cx,
            y: cy,
            target_room: None,
        }
    })
    .collect()
}

impl Default for ShipBreach {
    fn default() -> Self {
        let rooms = initial_rooms();
        let player_plates = build_hull(
            PSHIP_W,
            PSHIP_H,
            rooms.iter().map(|r| r.rect),
            &corridor_spans(&rooms),
        );

        let enemy_rooms = initial_enemy_rooms();
        let enemy_corridors = [(10, 3, 10, 3), (20, 3, 20, 3)];
        let enemy_plates = build_hull(
            ESHIP_W,
            ESHIP_H,
            enemy_rooms.iter().map(|r| r.rect),
            &enemy_corridors,
        );

        let crew = initial_crew(&rooms);

        let mut log = Log::new(48);
        log.push("Contact: hostile signature closing.", ui::ACCENT);
        log.push(
            "Reactor at 11/14. Tap a system's power row to add pips.",
            ui::DIM,
        );

        Self {
            time: 0.0,
            paused: false,
            reactor_flash: 0.0,
            rooms,
            player_plates,
            enemy_rooms,
            enemy_plates,
            crew,
            selected_crew: None,
            dragging_crew: None,
            weapons: [
                Weapon {
                    name: "Laser 1",
                    charge: 0.2,
                },
                Weapon {
                    name: "Laser 2",
                    charge: 0.0,
                },
            ],
            selected_weapon: None,
            keyboard_target: None,
            shield_charge: 2.0,
            hull: 1.0,
            oxygen: 1.0,
            breach_room: None,
            breach_triggered: false,
            breach_repair: 0.0,
            volley_timer: 6.0,
            volley_count: 0,
            fire_check_timer: 0.0,
            fire_check_count: 0,
            enemy_defeated: false,
            log,
            pointer: Pointer::new(),
            hover_at: None,
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

/// Straight one-cell-wide corridor spans linking every pair in [`CORRIDORS`],
/// derived from the rooms' own coordinates rather than hand-duplicated, so
/// moving a room in the layout above can never desync it from the corridors
/// meant to reach it.
fn corridor_spans(rooms: &[PlayerRoom; 6]) -> Vec<(i32, i32, i32, i32)> {
    CORRIDORS
        .iter()
        .map(|&(a, b)| {
            let ra = rooms[a].rect;
            let rb = rooms[b].rect;
            let (acx, acy) = ra.center();
            if ra.y == rb.y {
                // Same row: a horizontal span between the room edges at the
                // shared row centre.
                let y = acy as i32;
                let (x0, x1) = if ra.x < rb.x {
                    (ra.right(), rb.x - 1)
                } else {
                    (rb.right(), ra.x - 1)
                };
                (x0, y, x1, y)
            } else {
                // Same column: a vertical span between the room edges at the
                // shared column centre.
                let x = acx as i32;
                let (y0, y1) = if ra.y < rb.y {
                    (ra.bottom(), rb.y - 1)
                } else {
                    (rb.bottom(), ra.y - 1)
                };
                (x, y0, x, y1)
            }
        })
        .collect()
}

/// Carves floor for every room and corridor span into a `w`x`h` plate grid,
/// then derives walls from floor adjacency: a plate that is not floor but
/// touches floor on a cardinal side becomes a wall, everything else stays
/// void. This is the same two-pass technique `21_deck_plan.rs` uses for its
/// generated deck; reused here even though the layout is hand-placed rather
/// than generated, because autotiling walls from adjacency is what makes
/// every corner and T-junction join correctly with no per-room glyph
/// special-casing.
fn build_hull(
    w: i32,
    h: i32,
    rooms: impl Iterator<Item = Box2>,
    corridors: &[(i32, i32, i32, i32)],
) -> Vec<Plate> {
    let mut plates = vec![Plate::Void; (w * h) as usize];
    let index = |x: i32, y: i32| -> Option<usize> {
        if x < 0 || y < 0 || x >= w || y >= h {
            None
        } else {
            Some((y * w + x) as usize)
        }
    };
    for room in rooms {
        for yy in room.y..room.bottom() {
            for xx in room.x..room.right() {
                if let Some(i) = index(xx, yy) {
                    plates[i] = Plate::Floor;
                }
            }
        }
    }
    for &(x0, y0, x1, y1) in corridors {
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                if let Some(i) = index(xx, yy) {
                    plates[i] = Plate::Floor;
                }
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let Some(i) = index(x, y) else { continue };
            if plates[i] == Plate::Floor {
                continue;
            }
            let touches_floor = [(0, -1), (1, 0), (0, 1), (-1, 0)].iter().any(|&(dx, dy)| {
                matches!(index(x + dx, y + dy).map(|j| plates[j]), Some(Plate::Floor))
            });
            if touches_floor {
                plates[i] = Plate::Wall;
            }
        }
    }
    plates
}

impl ShipBreach {
    fn manned(&self, room: usize) -> bool {
        let rect = self.rooms[room].rect;
        self.crew.iter().any(|c| rect.contains(c.x, c.y))
    }

    fn reactor_used(&self) -> u8 {
        self.rooms.iter().map(|r| r.power).sum()
    }

    /// Adds one pip to `room`'s system, failing visibly (a flash on the
    /// reactor bar plus a log line) rather than borrowing from another
    /// system if the reactor is already fully committed.
    ///
    /// Stealing power automatically was the other option FTL-likes
    /// sometimes take, and it was rejected here on purpose: a silent steal
    /// changes a *second* gauge the player did not tap, and on a screen too
    /// small to watch all six gauges at once that second change can go
    /// unnoticed until a system nobody touched mysteriously goes dark. A
    /// failed request is a single, attributable event.
    fn add_power(&mut self, room: usize) {
        let used = self.reactor_used();
        let r = &mut self.rooms[room];
        if r.power >= r.max_power {
            self.reactor_flash = 0.35;
            self.log.push(
                format!("{} is already at capacity.", r.name),
                rgb(210, 120, 90),
            );
            return;
        }
        if used >= REACTOR_MAX {
            self.reactor_flash = 0.35;
            self.log.push(
                "Reactor overloaded -- no spare power.".to_string(),
                rgb(216, 88, 84),
            );
            return;
        }
        r.power += 1;
        self.log.push(
            format!("{} power +1 ({}/{}).", r.name, r.power, r.max_power),
            ui::DIM,
        );
    }

    fn remove_power(&mut self, room: usize) {
        let r = &mut self.rooms[room];
        if r.power > 0 {
            r.power -= 1;
            self.log.push(
                format!("{} power -1 ({}/{}).", r.name, r.power, r.max_power),
                ui::DIM,
            );
        }
    }

    fn select_crew(&mut self, i: usize) {
        self.selected_crew = if self.selected_crew == Some(i) {
            None
        } else {
            Some(i)
        };
    }

    fn send_crew_to(&mut self, crew_idx: usize, room: usize) {
        if let Some(c) = self.crew.get_mut(crew_idx) {
            c.target_room = Some(room);
            let name = c.name;
            let dest = self.rooms[room].name;
            self.log.push(format!("{name} moving to {dest}."), ui::DIM);
        }
    }

    fn fire_weapon(&mut self, weapon_idx: usize, target: usize) {
        let Some(w) = self.weapons.get_mut(weapon_idx) else {
            return;
        };
        if !w.ready() {
            return;
        }
        w.charge = 0.0;
        self.selected_weapon = None;
        if let Some(room) = self.enemy_rooms.get_mut(target) {
            room.hp = (room.hp - 0.4).max(0.0);
            let name = room.name;
            self.log.push(format!("Hit on enemy {name}."), ui::ACCENT);
        }
        if self.enemy_rooms.iter().all(|r| r.hp <= 0.0) && !self.enemy_defeated {
            self.enemy_defeated = true;
            self.log
                .push("Enemy hull breaking apart.".to_string(), ui::ACCENT);
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
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char(c @ '1'..='6') => {
                let room = c as usize - '1' as usize;
                if let Some(crew) = self.selected_crew {
                    self.send_crew_to(crew, room);
                } else {
                    self.add_power(room);
                }
            }
            KeyCode::Char(c @ ('!' | '@' | '#' | '$' | '%' | '^')) => {
                let room = match c {
                    '!' => 0,
                    '@' => 1,
                    '#' => 2,
                    '$' => 3,
                    '%' => 4,
                    _ => 5,
                };
                self.remove_power(room);
            }
            KeyCode::Char('c' | 'C') => {
                self.selected_crew = match self.selected_crew {
                    None => (!self.crew.is_empty()).then_some(0),
                    Some(i) if i + 1 < self.crew.len() => Some(i + 1),
                    Some(_) => None,
                };
            }
            KeyCode::Char('w' | 'W') => {
                let ready: Vec<usize> = self
                    .weapons
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.ready())
                    .map(|(i, _)| i)
                    .collect();
                self.selected_weapon = self.selected_weapon.map_or_else(
                    || ready.first().copied(),
                    |cur| ready.iter().find(|&&i| i > cur).copied(),
                );
            }
            KeyCode::Char('[') => self.cycle_target(-1),
            KeyCode::Char(']') => self.cycle_target(1),
            KeyCode::Char('f' | 'F') => {
                if let (Some(w), Some(t)) = (self.selected_weapon, self.keyboard_target) {
                    self.fire_weapon(w, t);
                }
            }
            _ => {}
        }
    }

    fn cycle_target(&mut self, dir: i32) {
        let n = self.enemy_rooms.len() as i32;
        let cur = self.keyboard_target.unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(n);
        self.keyboard_target = Some(next as usize);
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.log
            .push(if self.paused { "Paused." } else { "Resumed." }, ui::ACCENT);
    }

    /// Advances the whole simulation by `dt` world-seconds. Gated entirely
    /// behind `!self.paused` by the caller: everything in here is the game,
    /// and freezing the game (not the screen -- the starfield-equivalent
    /// idle motion still runs in `tick`) is the entire point of pause.
    fn simulate(&mut self, dt: f32) {
        self.move_crew(dt);
        self.charge_weapons(dt);
        self.recharge_shields(dt);
        self.update_oxygen(dt);
        self.tick_fire(dt);
        self.check_fire_spread(dt);
        self.tick_breach(dt);
        self.tick_volley(dt);
        if self.reactor_flash > 0.0 {
            self.reactor_flash = (self.reactor_flash - dt).max(0.0);
        }
    }

    fn move_crew(&mut self, dt: f32) {
        const SPEED: f32 = 3.0;
        for crew in &mut self.crew {
            let Some(room) = crew.target_room else {
                continue;
            };
            let (tx, ty) = self.rooms[room].rect.center();
            // Move one axis to completion before the other. Both source and
            // destination coordinates are always room centres, which by
            // construction sit on a corridor row or column (see
            // `corridor_spans`), so this Manhattan path never has to leave
            // floor -- a straight diagonal would.
            let dx = tx - crew.x;
            let dy = ty - crew.y;
            let step = SPEED * dt;
            if dx.abs() > 0.05 {
                crew.x = dx.signum().mul_add(step.min(dx.abs()), crew.x);
            } else if dy.abs() > 0.05 {
                crew.y = dy.signum().mul_add(step.min(dy.abs()), crew.y);
            } else {
                crew.target_room = None;
            }
        }
    }

    fn charge_weapons(&mut self, dt: f32) {
        let power = self.rooms[ROOM_WEAPONS].power;
        let manned_bonus = if self.manned(ROOM_WEAPONS) { 1.5 } else { 1.0 };
        let rate = if power == 0 {
            0.0
        } else {
            0.035f32.mul_add(f32::from(power), 0.05) * manned_bonus
        };
        for w in &mut self.weapons {
            w.charge = (w.charge + rate * dt).min(1.0);
        }
    }

    fn recharge_shields(&mut self, dt: f32) {
        let cap = f32::from(self.rooms[ROOM_SHIELDS].power);
        self.shield_charge = self.shield_charge.min(cap);
        if self.shield_charge >= cap {
            return;
        }
        let manned_bonus = if self.manned(ROOM_SHIELDS) { 1.5 } else { 1.0 };
        self.shield_charge = (0.08_f32 * manned_bonus)
            .mul_add(dt, self.shield_charge)
            .min(cap);
    }

    fn update_oxygen(&mut self, dt: f32) {
        let power = f32::from(self.rooms[ROOM_OXYGEN].power);
        let manned_bonus = if self.manned(ROOM_OXYGEN) { 1.5 } else { 1.0 };
        let replenish = power * 0.01 * manned_bonus;
        let breach_drain = if self.breach_room.is_some() {
            0.02
        } else {
            0.0
        };
        let net = replenish - breach_drain - 0.004;
        self.oxygen = net.mul_add(dt, self.oxygen).clamp(0.0, 1.0);
    }

    fn tick_fire(&mut self, dt: f32) {
        for room in &mut self.rooms {
            if room.fire <= 0.0 {
                continue;
            }
            let crew_here = self.crew.iter().any(|c| room.rect.contains(c.x, c.y));
            if crew_here {
                room.fire = (-0.25f32).mul_add(dt, room.fire).max(0.0);
                if room.fire == 0.0 {
                    self.log
                        .push(format!("Fire in {} extinguished.", room.name), ui::ACCENT);
                }
            } else {
                room.fire = 0.02f32.mul_add(dt, room.fire).min(1.0);
            }
        }
    }

    /// Rolls, deterministically, whether an already-burning room ignites one
    /// of its unburnt neighbours.
    ///
    /// The check only fires when `fire_check_timer` crosses a fixed
    /// threshold, and each crossing draws from a fresh [`Rng`] seeded by a
    /// counter that increments once per check -- never from `self.time` or
    /// any other quantity a wall clock could perturb. Two runs fed identical
    /// frame deltas reach the same threshold crossings in the same order and
    /// therefore make the same spread rolls, which is what the snapshot
    /// tests rely on when they render this demo twice and diff the frames.
    // A float loop condition is usually a red flag for stalling on rounding
    // error, but `dt` is always positive and bounded (a frame is never
    // longer than a handful of seconds even on the slowest headless tick),
    // so this drains in at most a handful of iterations and cannot spin.
    #[allow(clippy::while_float)]
    fn check_fire_spread(&mut self, dt: f32) {
        const INTERVAL: f32 = 4.0;
        const SEED: u32 = 0xF12E_5EED;
        self.fire_check_timer += dt;
        while self.fire_check_timer >= INTERVAL {
            self.fire_check_timer -= INTERVAL;
            self.fire_check_count += 1;
            let mut rng = Rng::new(SEED ^ self.fire_check_count);
            for &(a, b) in &CORRIDORS {
                for (from, to) in [(a, b), (b, a)] {
                    if self.rooms[from].fire > 0.5
                        && self.rooms[to].fire == 0.0
                        && rng.next_f32() < 0.2
                    {
                        self.rooms[to].fire = 0.15;
                        let name = self.rooms[to].name;
                        self.log
                            .push(format!("Fire spreads to {name}!"), rgb(226, 140, 70));
                    }
                }
            }
        }
    }

    fn tick_breach(&mut self, dt: f32) {
        if !self.breach_triggered && self.time >= 22.0 {
            self.breach_triggered = true;
            self.breach_room = Some(ROOM_OXYGEN);
            self.log.push(
                "Hull breach in Oxygen! Venting atmosphere.".to_string(),
                rgb(216, 88, 84),
            );
        }
        let Some(room) = self.breach_room else { return };
        if self.manned(room) {
            self.breach_repair = 0.15f32.mul_add(dt, self.breach_repair);
        }
        if self.breach_repair >= 1.0 {
            self.breach_room = None;
            self.breach_repair = 0.0;
            self.log.push("Breach sealed.".to_string(), ui::ACCENT);
        }
    }

    fn tick_volley(&mut self, dt: f32) {
        if self.enemy_defeated {
            return;
        }
        self.volley_timer -= dt;
        if self.volley_timer > 0.0 {
            return;
        }
        self.volley_timer = 7.5;
        self.volley_count += 1;
        let mut rng = Rng::new(0xB047_1E5E ^ self.volley_count);
        let evasion = ((f32::from(self.rooms[ROOM_PILOTING].power)
            + f32::from(self.rooms[ROOM_ENGINES].power))
            * 0.04)
            .clamp(0.0, 0.5);
        if rng.next_f32() < evasion {
            self.log.push("Volley evaded.".to_string(), ui::ACCENT);
            return;
        }
        if self.shield_charge >= 1.0 {
            self.shield_charge -= 1.0;
            self.log.push("Shields absorb a hit.".to_string(), ui::DIM);
            return;
        }
        self.hull = (self.hull - 0.08).max(0.0);
        self.log
            .push("Hull hit! Damage sustained.".to_string(), rgb(216, 88, 84));
        let target = (rng.next_below(self.rooms.len() as u32)) as usize;
        if self.rooms[target].fire == 0.0 {
            self.rooms[target].fire = 0.2;
            let name = self.rooms[target].name;
            self.log.push(
                format!("Impact starts a fire in {name}."),
                rgb(226, 140, 70),
            );
        }
    }

    // -- layout & drawing -------------------------------------------------

    fn draw_hull(
        surface: &mut Surface<'_>,
        origin: (u16, u16),
        area: Rect,
        plates: &[Plate],
        w: i32,
        h: i32,
    ) {
        for y in 0..h {
            for x in 0..w {
                let Some(at) = Self::local_to_screen(origin, area, x, y) else {
                    continue;
                };
                let plate = plates[(y * w + x) as usize];
                let (glyph, fg, bg) = match plate {
                    Plate::Void => continue,
                    Plate::Floor => ('\u{00b7}', rgb(80, 96, 118), rgb(14, 18, 28)),
                    Plate::Wall => {
                        let connects = |px: i32, py: i32| -> bool {
                            if px < 0 || py < 0 || px >= w || py >= h {
                                false
                            } else {
                                plates[(py * w + px) as usize] == Plate::Wall
                            }
                        };
                        let mask = mask4([
                            connects(x, y - 1),
                            connects(x + 1, y),
                            connects(x, y + 1),
                            connects(x - 1, y),
                        ]);
                        (
                            BOX_SINGLE[(mask & 0x0F) as usize],
                            rgb(140, 170, 210),
                            rgb(8, 10, 16),
                        )
                    }
                };
                surface.put(at, glyph, Style::new().fg(fg).bg(bg));
            }
        }
    }

    fn local_to_screen(origin: (u16, u16), area: Rect, x: i32, y: i32) -> Option<(u16, u16)> {
        if x < 0 || y < 0 {
            return None;
        }
        let (sx, sy) = (origin.0 + x as u16, origin.1 + y as u16);
        if sx >= area.right() || sy >= area.bottom() {
            return None;
        }
        Some((sx, sy))
    }

    fn draw_player_ship(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        hotspots: &mut Hotspots<Action>,
    ) {
        let panel = panel::Panel::new()
            .title("Player Hull")
            .border(panel::Border::Double)
            .frame(rgb(90, 140, 200))
            .draw(surface, area);
        if panel.width() < 4 || panel.height() < 4 {
            return;
        }
        let origin = (panel.left(), panel.top());
        Self::draw_hull(
            surface,
            origin,
            panel,
            &self.player_plates,
            PSHIP_W,
            PSHIP_H,
        );

        for (i, room) in self.rooms.iter().enumerate() {
            // Tint the room's floor toward warning-amber with its own fire
            // level, so a burning room reads as hot without needing a
            // separate icon layer.
            if room.fire > 0.0 {
                for yy in room.rect.y..room.rect.bottom() {
                    for xx in room.rect.x..room.rect.right() {
                        if let Some(at) = Self::local_to_screen(origin, panel, xx, yy) {
                            let bg = mix(rgb(14, 18, 28), rgb(200, 90, 30), room.fire * 0.6);
                            surface.put(at, '\u{00b7}', Style::new().fg(rgb(230, 170, 90)).bg(bg));
                        }
                    }
                }
            }
            if let Some(breach) = self.breach_room
                && breach == i
            {
                let (cx, cy) = room.rect.center();
                if let Some(at) = Self::local_to_screen(origin, panel, cx as i32, cy as i32) {
                    surface.put(
                        at,
                        '\u{2193}',
                        Style::new().fg(rgb(140, 190, 230)).bg(rgb(10, 14, 22)),
                    );
                }
            }
            if let Some(at) = Self::local_to_screen(origin, panel, room.rect.x + 1, room.rect.y + 1)
            {
                let text = truncate7(room.name);
                let manned = self.manned(i);
                let fg = if manned { ui::ACCENT } else { ui::FG };
                surface.print((at.0, at.1), &text, Style::new().fg(fg).bg(rgb(14, 18, 28)));
            }
            if let Some(at) = Self::local_to_screen(origin, panel, room.rect.x + 1, room.rect.y + 2)
            {
                let pips = pip_string(room.power, room.max_power);
                surface.print(
                    (at.0, at.1),
                    &pips,
                    Style::new().fg(rgb(120, 170, 220)).bg(rgb(14, 18, 28)),
                );
            }

            let rect = Rect::new(
                origin.0 + room.rect.x as u16,
                origin.1 + room.rect.y as u16,
                ROOM_W as u16,
                ROOM_H as u16,
            );
            hotspots.push(rect, Action::SendCrewTo(i));
        }

        for (i, crew) in self.crew.iter().enumerate() {
            let Some(at) =
                Self::local_to_screen(origin, panel, crew.x.round() as i32, crew.y.round() as i32)
            else {
                continue;
            };
            let selected = self.selected_crew == Some(i);
            let bg = if selected {
                ui::ACCENT
            } else {
                rgb(80, 130, 90)
            };
            let glyph = char::from(b'1' + i.min(8) as u8);
            surface.put(at, glyph, Style::new().fg(rgb(10, 12, 14)).bg(bg));
            // The crew token is one plate, but its hit region is grown to a
            // legal touch target so a finger doesn't have to land on that
            // exact cell. It is registered *after* the room hotspots above,
            // so within its own footprint it wins the tap even though the
            // grown region overlaps the room beneath it.
            let token = Rect::new(at.0, at.1, 1, 1);
            hotspots.push_tappable(token, panel, Action::SelectCrew(i));
        }
    }

    fn draw_enemy_ship(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        hotspots: &mut Hotspots<Action>,
    ) {
        let weapon_selected = self.selected_weapon.is_some();
        let panel = panel::Panel::new()
            .title("Enemy Hull")
            .frame(rgb(196, 108, 108))
            .draw(surface, area);
        if panel.width() < 4 || panel.height() < 4 {
            return;
        }
        let origin = (panel.left(), panel.top());
        Self::draw_hull(surface, origin, panel, &self.enemy_plates, ESHIP_W, ESHIP_H);

        for (i, room) in self.enemy_rooms.iter().enumerate() {
            let destroyed = room.hp <= 0.0;
            let target_rect = Rect::new(
                origin.0 + room.rect.x as u16,
                origin.1 + room.rect.y as u16,
                ROOM_W as u16,
                ROOM_H as u16,
            );
            let highlighted = weapon_selected
                && self
                    .hover_at
                    .is_some_and(|p| target_rect.contains(p.x, p.y));

            for yy in room.rect.y..room.rect.bottom() {
                for xx in room.rect.x..room.rect.right() {
                    if let Some(at) = Self::local_to_screen(origin, panel, xx, yy) {
                        let bg = if highlighted {
                            rgb(70, 20, 20)
                        } else if destroyed {
                            rgb(20, 20, 24)
                        } else {
                            rgb(14, 18, 28)
                        };
                        surface.put(at, '\u{00b7}', Style::new().fg(rgb(90, 70, 70)).bg(bg));
                    }
                }
            }
            if let Some(at) = Self::local_to_screen(origin, panel, room.rect.x + 1, room.rect.y + 1)
            {
                let label = if destroyed {
                    "Wrecked".to_string()
                } else {
                    truncate7(room.name)
                };
                surface.print(
                    (at.0, at.1),
                    &label,
                    Style::new().fg(ui::FG).bg(rgb(14, 18, 28)),
                );
            }
            if let Some(at) = Self::local_to_screen(origin, panel, room.rect.x + 1, room.rect.y + 2)
            {
                panel::bar(
                    surface,
                    at,
                    7,
                    room.hp,
                    panel::threshold(room.hp),
                    rgb(30, 20, 20),
                );
            }

            if !destroyed && !self.enemy_defeated {
                // Only tappable at all while a weapon is selected: firing is
                // irreversible, and the room hotspot doing nothing without a
                // weapon armed is what keeps a stray tap here from reading
                // as "I meant to fire".
                if weapon_selected {
                    hotspots.push(target_rect, Action::FireAt(i));
                }
            }
        }
    }

    /// Draws the reactor panel: one row per system, each a legal touch
    /// target for "add a pip" across its full width, with a smaller "[-]"
    /// zone at the left registered afterward so it wins the overlap. Growing
    /// each row's hitbox down by [`touch::TAP_H`](ui::touch::TAP_H) rather
    /// than reserving four real rows per system is what lets all six systems
    /// fit in the height a phone or a narrow sidebar actually has; the
    /// overlap this creates between one row's grown hitbox and the next
    /// row's own territory resolves correctly because [`Hotspots::hit`]
    /// always prefers the most recently registered region, and rows are
    /// registered top to bottom.
    fn draw_reactor(&self, surface: &mut Surface<'_>, area: Rect, hotspots: &mut Hotspots<Action>) {
        let flashing = self.reactor_flash > 0.0;
        let frame = if flashing {
            rgb(216, 88, 84)
        } else {
            rgb(90, 140, 200)
        };
        let used = self.reactor_used();
        let panel = panel::Panel::new()
            .title("Reactor")
            .badge(&format!("{used}/{REACTOR_MAX}"))
            .frame(frame)
            .draw(surface, area);
        if panel.height() == 0 || panel.width() < 14 {
            return;
        }
        for (i, room) in self.rooms.iter().enumerate() {
            let y = panel.top() + i as u16;
            if y >= panel.bottom() {
                break;
            }
            let row = Rect::new(panel.left(), y, panel.width(), 1);
            let text = format!(
                "[-] {:<8}{} {}/{}",
                room.name,
                pip_string(room.power, room.max_power),
                room.power,
                room.max_power
            );
            let name_fg = if self.manned(i) { ui::ACCENT } else { ui::FG };
            surface.print(
                (row.left(), row.top()),
                "[-] ",
                Style::new().fg(rgb(200, 120, 90)).bg(panel::PANEL_BG),
            );
            surface.print(
                (row.left() + 4, row.top()),
                &text[4..],
                Style::new().fg(name_fg).bg(panel::PANEL_BG),
            );

            hotspots.push_tappable(row, panel, Action::AddPower(i));
            let minus = Rect::new(row.left(), row.top(), 3, 1);
            hotspots.push_tappable(minus, panel, Action::RemovePower(i));
        }
    }

    fn draw_weapons(&self, surface: &mut Surface<'_>, area: Rect, hotspots: &mut Hotspots<Action>) {
        let panel = panel::Panel::new()
            .title("Weapons")
            .frame(rgb(196, 150, 90))
            .draw(surface, area);
        if panel.height() == 0 || panel.width() < 10 {
            return;
        }
        for (i, w) in self.weapons.iter().enumerate() {
            let y = panel.top() + i as u16;
            if y >= panel.bottom() {
                break;
            }
            let selected = self.selected_weapon == Some(i);
            let label = if w.ready() { "RDY" } else { "chg" };
            let fg = if selected {
                ui::ACCENT
            } else if w.ready() {
                rgb(140, 220, 140)
            } else {
                ui::DIM
            };
            let room = panel.width().saturating_sub(14).max(4);
            panel::spans(
                surface,
                (panel.left(), y),
                8,
                &[Span::new(w.name, fg)],
                panel::PANEL_BG,
            );
            surface.print(
                (panel.left() + 8, y),
                &format!("{label} "),
                Style::new().fg(fg).bg(panel::PANEL_BG),
            );
            panel::bar(
                surface,
                (panel.left() + 12, y),
                room,
                w.charge,
                panel::threshold(w.charge),
                rgb(30, 30, 36),
            );

            if w.ready() {
                let row = Rect::new(panel.left(), y, panel.width(), 1);
                hotspots.push_tappable(row, panel, Action::SelectWeapon(i));
            }
        }
    }

    fn draw_crew_panel(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        hotspots: &mut Hotspots<Action>,
    ) {
        let panel = panel::Panel::new()
            .title("Crew")
            .badge(&format!("{}", self.crew.len()))
            .draw(surface, area);
        if panel.height() == 0 || self.crew.is_empty() {
            return;
        }
        for (i, crew) in self.crew.iter().enumerate() {
            let y = panel.top() + i as u16;
            if y >= panel.bottom() {
                break;
            }
            let selected = self.selected_crew == Some(i);
            let room = crew.target_room.map_or_else(
                || {
                    self.rooms
                        .iter()
                        .position(|r| r.rect.contains(crew.x, crew.y))
                        .map_or("corridor", |idx| self.rooms[idx].name)
                },
                |idx| self.rooms[idx].name,
            );
            let moving = crew.target_room.is_some();
            let text = format!("{} {}", crew.name, if moving { "-> " } else { "@ " });
            let fg = if selected { ui::ACCENT } else { ui::FG };
            surface.print(
                (panel.left(), y),
                &text,
                Style::new().fg(fg).bg(panel::PANEL_BG),
            );
            let used = text.chars().count() as u16;
            if panel.width() > used {
                panel::spans(
                    surface,
                    (panel.left() + used, y),
                    panel.width() - used,
                    &[Span::dim(room)],
                    panel::PANEL_BG,
                );
            }
            let row = Rect::new(panel.left(), y, panel.width(), 1);
            hotspots.push_tappable(row, panel, Action::SelectCrew(i));
        }
    }

    /// The pause control: a large button pinned in the thumb zone, not a
    /// keyboard-only convenience. FTL's whole loop is real-time-with-pause;
    /// on a phone the thumb is already resting near the bottom edge, and a
    /// pause control anywhere else is a control the player has to travel to
    /// find at the exact moment the ship is on fire.
    fn draw_pause(&self, surface: &mut Surface<'_>, area: Rect, hotspots: &mut Hotspots<Action>) {
        let bg = if self.paused {
            rgb(70, 110, 60)
        } else {
            rgb(60, 70, 100)
        };
        surface.fill_rect(area, ' ', Style::new().bg(bg));
        let label = if self.paused {
            "II RESUME (Space)"
        } else {
            "II PAUSE (Space)"
        };
        let x = area.left() + area.width().saturating_sub(label.chars().count() as u16) / 2;
        let y = area.top() + area.height() / 2;
        if area.height() > 0 {
            surface.print((x, y), label, Style::new().fg(ui::FG).bg(bg));
        }
        hotspots.push_tappable(area, area, Action::TogglePause);
    }

    fn draw_status(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = panel::Panel::new().title("Status").draw(surface, area);
        if panel.height() == 0 {
            return;
        }
        let bar_w = panel.width().saturating_sub(14).clamp(4, 30);
        if panel.height() >= 1 {
            surface.print(
                (panel.left(), panel.top()),
                "HULL ",
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
            panel::bar(
                surface,
                (panel.left() + 5, panel.top()),
                bar_w,
                self.hull,
                panel::threshold(self.hull),
                rgb(30, 30, 36),
            );
        }
        if panel.height() >= 2 {
            let y = panel.top() + 1;
            surface.print(
                (panel.left(), y),
                "SHLD ",
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
            let bubbles = self.shield_charge.floor() as u32;
            let cap = u32::from(self.rooms[ROOM_SHIELDS].power);
            let mut s = String::new();
            for b in 0..cap.max(bubbles) {
                s.push(if b < bubbles { '\u{25cb}' } else { '\u{00b7}' });
                s.push(' ');
            }
            surface.print(
                (panel.left() + 5, y),
                s.trim_end(),
                Style::new().fg(rgb(120, 180, 230)).bg(panel::PANEL_BG),
            );
        }
        if panel.height() >= 3 {
            let y = panel.top() + 2;
            surface.print(
                (panel.left(), y),
                "O2   ",
                Style::new().fg(ui::DIM).bg(panel::PANEL_BG),
            );
            panel::bar(
                surface,
                (panel.left() + 5, y),
                bar_w,
                self.oxygen,
                panel::threshold(self.oxygen),
                rgb(30, 30, 36),
            );
        }
    }

    fn draw_log(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = panel::Panel::new().title("Log").draw(surface, area);
        self.log.draw(surface, panel, panel::PANEL_BG);
    }

    /// Applies whatever a tap resolved to.
    fn apply_action(&mut self, action: Action) {
        match action {
            Action::AddPower(r) => self.add_power(r),
            Action::RemovePower(r) => self.remove_power(r),
            Action::SelectCrew(i) => self.select_crew(i),
            Action::SendCrewTo(room) => {
                if let Some(crew) = self.selected_crew {
                    self.send_crew_to(crew, room);
                    self.selected_crew = None;
                }
            }
            Action::SelectWeapon(i) => {
                self.selected_weapon = if self.selected_weapon == Some(i) {
                    None
                } else {
                    Some(i)
                };
            }
            Action::FireAt(room) => {
                if let Some(w) = self.selected_weapon {
                    self.fire_weapon(w, room);
                }
            }
            Action::TogglePause => self.toggle_pause(),
        }
    }

    fn handle_gesture(&mut self, gesture: Gesture) {
        self.hover_at = gesture.press.or(gesture.hover);

        if let Some(p) = gesture.press
            && self.dragging_crew.is_none()
            && let Some(Action::SelectCrew(i)) = self.hotspots.hit(p)
        {
            self.dragging_crew = Some(*i);
        }
        if gesture.press.is_none() && gesture.drag.is_none() {
            self.dragging_crew = None;
        }
        if let Some(p) = gesture.drop
            && let Some(crew_idx) = self.dragging_crew.take()
            && let Some(Action::SendCrewTo(room)) = self.hotspots.hit(p)
        {
            self.send_crew_to(crew_idx, *room);
            self.selected_crew = None;
        }
        if let Some(p) = gesture.tap
            && let Some(action) = self.hotspots.hit(p)
        {
            self.apply_action(*action);
        }
    }

    fn status_text(&self) -> String {
        format!(
            "Hull {:.0}%  O2 {:.0}%  Reactor {}/{REACTOR_MAX}  Crew {}{}",
            self.hull * 100.0,
            self.oxygen * 100.0,
            self.reactor_used(),
            self.crew.len(),
            if self.paused { "  [PAUSED]" } else { "" }
        )
    }
}

/// Truncates a room name to the 7 columns available inside a 9-wide room
/// after its 2-cell border.
fn truncate7(name: &str) -> String {
    name.chars().take(7).collect()
}

/// A pip row: `[fi]` glyphs for spent power, `.` for unspent, up to `max`.
fn pip_string(power: u8, max: u8) -> String {
    let mut s = String::with_capacity(max as usize);
    for i in 0..max {
        s.push(if i < power { '\u{25a0}' } else { '\u{00b7}' });
    }
    s
}

impl Demo for ShipBreach {
    const NAME: &'static str = "29_ship_breach";
    const TITLE: &'static str = "29 Ship Breach";
    const BLURB: &'static str =
        "An FTL-style ship cross-section: reactor pips, crew, fire, and weapons.";
    const GRID: (u16, u16) = (150, 46);

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-6", "add power / send selected crew"),
            ("!@#$%^", "remove power (shift+1-6)"),
            ("C", "cycle crew selection"),
            ("W", "cycle ready weapon"),
            ("[ ]", "cycle enemy target"),
            ("F", "fire selected weapon"),
            ("Space", "pause"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        // Elapsed demo time always advances, pause or not: the starfield-
        // equivalent idle motion (the reactor's flash decay aside, mostly
        // the crew tokens and hazard tints) has to keep the frame from going
        // static, which is what the animation-liveness check demands, while
        // the game clock (`simulate`) is the thing pause actually freezes.
        self.time += dt;
        self.fps.record(frame.delta);

        if !self.handle_events(term) {
            return false;
        }
        let gesture = self.pointer.take();

        if !self.paused {
            self.simulate(dt);
        }

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        let shape = Shape::of(content);
        self.layout_and_draw(&mut surface, content, shape);

        // The gesture is resolved against hotspots registered *this* frame's
        // layout, which is what keeps immediate-mode hit-testing honest: a
        // control that moved or vanished this frame cannot still be tapped.
        self.handle_gesture(gesture);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_text();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

impl ShipBreach {
    /// Lays out and draws every panel for `content`, branching on [`Shape`]
    /// the way every demo from 27 on has to: `Shape::Portrait` stacks every
    /// panel top to bottom because rows are cheap and columns are scarce;
    /// the other two shapes put the two hulls side by side because there is
    /// width to spare, and only differ in how generous the budget for the
    /// lower-priority panels (weapons, crew) is.
    ///
    /// Hotspots live in a local table for the duration of layout rather than
    /// being borrowed straight out of `self.hotspots`, because every `draw_*`
    /// helper needs `&self` (to read room/crew/weapon state) at the same time
    /// it needs `&mut` access to the table -- two borrows of `self` that
    /// cannot coexist. Building into a local and writing it back once at the
    /// end sidesteps that without threading the table through every method
    /// signature as a second, unrelated parameter list.
    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect, shape: Shape) {
        let mut hotspots = core::mem::take(&mut self.hotspots);
        hotspots.clear();

        let status_h = 5u16.min(content.height());
        let (status_area, rest) = panel::split_top(content, status_h);
        self.draw_status(surface, status_area);

        if rest.height() == 0 {
            self.hotspots = hotspots;
            return;
        }

        // The pause control is claimed first and from the bottom edge,
        // whatever the shape: it is the one control that must never be
        // squeezed out by everything else fighting for room, since it is
        // the only way to stop the clock on a screen too small to show
        // everything else at once.
        let pause_h = 4u16.min(rest.height());
        let (rest, pause_area) = panel::split_bottom(rest, pause_h);
        self.draw_pause(surface, pause_area, &mut hotspots);

        if shape.stacks() {
            self.layout_stacked(surface, rest, &mut hotspots);
        } else {
            self.layout_wide(surface, rest, &mut hotspots);
        }

        self.hotspots = hotspots;
    }

    /// Portrait: one column, panels claimed top to bottom in priority order.
    /// The reactor (the puzzle this demo exists to show) and the player hull
    /// both get their full wanted size before weapons or crew see anything,
    /// so a short portrait window degrades by losing the enemy hull and crew
    /// list before it loses the power grid.
    fn layout_stacked(
        &self,
        surface: &mut Surface<'_>,
        area: Rect,
        hotspots: &mut Hotspots<Action>,
    ) {
        let ship_h = (PSHIP_H as u16 + 2).min(area.height());
        let (ship_area, rest) = panel::split_top(area, ship_h);
        self.draw_player_ship(surface, ship_area, hotspots);

        let enemy_h = (ESHIP_H as u16 + 2).min(rest.height());
        let (enemy_area, rest) = panel::split_top(rest, enemy_h);
        self.draw_enemy_ship(surface, enemy_area, hotspots);

        let reactor_h = 8u16.min(rest.height());
        let (reactor_area, rest) = panel::split_top(rest, reactor_h);
        self.draw_reactor(surface, reactor_area, hotspots);

        let weapons_h = 4u16.min(rest.height());
        let (weapons_area, rest) = panel::split_top(rest, weapons_h);
        self.draw_weapons(surface, weapons_area, hotspots);

        let crew_h = (self.crew.len() as u16 + 2).min(rest.height());
        let (crew_area, log_area) = panel::split_top(rest, crew_h);
        self.draw_crew_panel(surface, crew_area, hotspots);
        self.draw_log(surface, log_area);
    }

    /// Landscape/desktop: the two hulls stack in a left column (the enemy
    /// hull is dropped first if the column is too short for both, since
    /// aiming at a ship you can still see the reactor and crew for is more
    /// useful than the reverse); the reactor, weapons, and crew share a
    /// right column, reactor claimed first for the same reason as above.
    fn layout_wide(&self, surface: &mut Surface<'_>, area: Rect, hotspots: &mut Hotspots<Action>) {
        let left_w = (PSHIP_W as u16 + 2).min(area.width());
        let (left, right) = panel::split_left(area, left_w);

        let ship_h = (PSHIP_H as u16 + 2).min(left.height());
        let (ship_area, rest) = panel::split_top(left, ship_h);
        self.draw_player_ship(surface, ship_area, hotspots);
        let enemy_h_wanted = ESHIP_H as u16 + 2;
        if rest.height() >= enemy_h_wanted {
            self.draw_enemy_ship(surface, rest, hotspots);
        }

        if right.width() == 0 {
            return;
        }
        let reactor_h = 8u16.min(right.height());
        let (reactor_area, rest) = panel::split_top(right, reactor_h);
        self.draw_reactor(surface, reactor_area, hotspots);

        let weapons_h = 4u16.min(rest.height());
        let (weapons_area, rest) = panel::split_top(rest, weapons_h);
        self.draw_weapons(surface, weapons_area, hotspots);

        let crew_h = (self.crew.len() as u16 + 2).min(rest.height());
        let (crew_area, log_area) = panel::split_top(rest, crew_h);
        self.draw_crew_panel(surface, crew_area, hotspots);
        self.draw_log(surface, log_area);
    }
}

ascii_tile_demos::demo_main!(ShipBreach);
