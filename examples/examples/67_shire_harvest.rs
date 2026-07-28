//! 67: Shire Harvest -- Lords of the Realm II's labor slider, where the
//! preview *is* the outcome.
//!
//! Lords of the Realm II splits every peasant between farming and industry
//! with one horizontal slider. Underneath it, a column of production
//! figures rewrites itself as the slider moves: green for a resource that
//! grows next season, red for one that shrinks. Nothing else in this
//! gallery has a control where moving it rewrites a prediction; every gauge
//! elsewhere reports what already happened. The discipline that makes this
//! trustworthy rather than decorative is that the preview and the turn
//! resolution call the *same* function -- [`ShireHarvest::project`] -- so
//! the number on screen while you are still dragging is the number you get
//! once the season turns. `tests::preview_matches_the_actual_outcome` pins
//! this down directly, and every draw call reads the same `Projection`
//! rather than keeping its own copy of any of these numbers.
//!
//! Techniques on show:
//!
//! - **A control that previews its own consequence**
//!   ([`ShireHarvest::project`], [`ShireHarvest::draw_production`]): one
//!   pure function of `(allocation, season, tax rate)` computes every
//!   number the panel shows and every number the next turn applies. Idle
//!   labor turns the slider's handle blue; an understaffed side brackets
//!   its own numbers -- `[+3 stone]` -- so the state reads without color,
//!   which the gallery treats as scarce.
//! - **A seasonal loop that changes the rules, not just the palette**
//!   ([`tilekit::palette::Season`]): the same slider position predicts a
//!   grain surplus in autumn and a grain deficit in winter, because
//!   [`grain_multiplier`] looks up a season-specific yield rather than a
//!   season-specific color.
//! - **Turn-stepped simulation**: stockpiles and happiness only change on a
//!   turn boundary ([`ShireHarvest::step_turn`]), driven off accumulated
//!   [`retroglyph_core::Frame::delta`] via [`ShireHarvest::advance_turns`];
//!   between turns the preview holds steady rather than creeping toward its
//!   own answer.
//!
//! Plain grid rather than isometric: at the county's actual footprint on
//! screen -- six fields, two pastures, three works, one keep -- an
//! isometric offset would spend most of its rows on the diagonal seams
//! between sixteen small tiles. The information that matters here (which
//! tiles are worked, what season they are in) reads better as a flat,
//! legible grid than as a diagonal one; see `01_terrain_cells.rs` for where
//! a plain grid earns its keep at a much larger scale too.
//!
//! ```sh
//! cargo run --example 67_shire_harvest --features crossterm
//! cargo run --example 67_shire_harvest --features software
//! cargo run --example 67_shire_harvest --features gl
//! cargo run --example 67_shire_harvest  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui;
use ascii_tile_demos::ui::panel::{self, Panel, Span};
use ascii_tile_demos::ui::touch::{Gesture, Pointer, Shape};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::noise::hash01;
use tilekit::palette::{Season, rgb, scale};

// ── Tuning constants ────────────────────────────────────────────────────
//
// These are the rules [`ShireHarvest::project`] applies, not display values:
// changing one changes what the panel predicts and what the turn delivers,
// which is the whole point of computing both from the same place.

/// Peasants in the shire. Fixed rather than growing, so `project` stays a
/// pure function of the allocation alone -- a growing population would make
/// it a function of turn count too, and the preview-equals-outcome
/// guarantee only needs to hold for the inputs a person can actually move.
const POPULATION: i32 = 46;
/// Grain fields on the map (see [`MAP`]); each seats [`WORKERS_PER_FIELD`].
const FIELD_COUNT: i32 = 6;
/// Workers one field can usefully hold before the rest stand idle.
const WORKERS_PER_FIELD: i32 = 4;
/// Farm labor capacity: fields fully worked. Above this, extra farm-side
/// peasants have nowhere to stand and count as idle.
const FARM_CAPACITY: i32 = FIELD_COUNT * WORKERS_PER_FIELD;
/// Industry works on the map: the wood, stone, and iron tiles.
const INDUSTRY_SITES: i32 = 3;
/// Workers one works site can usefully hold.
const WORKERS_PER_SITE: i32 = 6;
/// Industry labor capacity. See [`FARM_CAPACITY`].
const INDUSTRY_CAPACITY: i32 = INDUSTRY_SITES * WORKERS_PER_SITE;

/// Farm labor tending grain fields, as a fraction of farm workers; the rest
/// tend the pastures. A field circuit needs more hands than a herd does, so
/// grain gets the larger share.
const GRAIN_SHARE_NUM: i32 = 2;
/// See [`GRAIN_SHARE_NUM`].
const GRAIN_SHARE_DEN: i32 = 3;

/// Grain one grain-worker yields per turn, before the seasonal multiplier.
const GRAIN_PER_WORKER: f32 = 14.0;
/// Grain one peasant eats per turn, regardless of what they work.
const GRAIN_PER_CAPITA: f32 = 2.0;
/// Cattle one pasture-worker yields per turn, before the seasonal
/// multiplier.
const CATTLE_PER_WORKER: f32 = 1.6;
/// Flat herd loss to cold and feed shortage in winter, independent of how
/// many hands are tending the pasture.
const WINTER_CATTLE_LOSS: f32 = 6.0;
/// Wood one wood-worker yields per turn.
const WOOD_PER_WORKER: f32 = 0.9;
/// Stone one stone-worker yields per turn.
const STONE_PER_WORKER: f32 = 0.7;
/// Iron one iron-worker yields per turn.
const IRON_PER_WORKER: f32 = 0.5;

/// The tax rates `T` cycles through.
const TAX_RATES: [i32; 5] = [0, 5, 10, 15, 20];
/// Happiness lost per turn per percentage point of tax.
const HAPPY_TAX_WEIGHT: f32 = 0.15;
/// Happiness gained per turn when grain is in surplus.
const HAPPY_RATION_BONUS: f32 = 2.0;
/// Happiness lost per turn when grain is in deficit -- a bigger swing than
/// the bonus, since a starving shire remembers it faster than a fed one
/// forgets.
const HAPPY_RATION_PENALTY: f32 = 4.0;
/// Happiness gained per turn when the herd is growing.
const HAPPY_CATTLE_BONUS: f32 = 0.5;
/// Happiness lost per turn when the herd is shrinking.
const HAPPY_CATTLE_PENALTY: f32 = 1.0;

/// Simulated seconds per game turn. Long enough that a season change reads
/// as a discrete step rather than a flicker, matching `59_city_works`'s
/// reasoning for its own turn clock.
const TURN_SECONDS: f32 = 6.0;
/// Percentage points one keypress moves the slider.
const SLIDER_STEP: i32 = 5;
/// Below this magnitude a delta counts as zero for coloring purposes, so
/// floating point noise from the season multipliers never paints a hair-thin
/// sliver of the wrong color.
const DELTA_EPS: f32 = 0.001;

/// Grain yield multiplier for one season: the seasonal loop that changes
/// what a slider position *means*, not just how the map looks. Winter's
/// fields are dormant (fully sown and waiting), spring's are freshly sown
/// and yield little yet, summer tends what is already growing, and autumn
/// is the harvest the whole year's farm labor was for.
const fn grain_multiplier(season: Season) -> f32 {
    match season {
        Season::Spring => 0.6,
        Season::Summer => 0.25,
        Season::Autumn => 2.2,
        Season::Winter => 0.0,
    }
}

/// Cattle yield multiplier for one season. Grazing is best in high summer
/// and worst in winter, when [`WINTER_CATTLE_LOSS`] also applies on top.
const fn cattle_multiplier(season: Season) -> f32 {
    match season {
        Season::Spring => 1.0,
        Season::Summer => 1.2,
        Season::Autumn => 0.9,
        Season::Winter => 0.5,
    }
}

/// One turn's projected consequences of a labor split, computed fresh from
/// `(allocation, season, tax rate)` and nothing else. See [`ShireHarvest::project`].
#[derive(Clone, Copy)]
struct Projection {
    farm_workers: i32,
    industry_workers: i32,
    /// Peasants assigned to a side that has no room left for them.
    idle: i32,
    understaffed_farm: bool,
    understaffed_industry: bool,
    grain_delta: f32,
    cattle_delta: f32,
    wood_delta: f32,
    stone_delta: f32,
    iron_delta: f32,
    happiness_delta: f32,
}

/// State for the Shire Harvest demo: the labor slider, the season/tax
/// context it is read against, and the stockpiles a turn boundary updates.
pub struct ShireHarvest {
    seed: u32,
    /// Percent of peasants assigned to industry; the rest farm. `0` is all
    /// farm, `100` is all industry, matching the source game's slider
    /// running plow-left to hammer-right.
    alloc: i32,
    tax_index: usize,
    season: Season,
    year: i32,
    turn: u32,
    turn_timer: f32,
    time: f32,
    grain_stock: f32,
    cattle_stock: f32,
    wood_stock: f32,
    stone_stock: f32,
    iron_stock: f32,
    happiness: f32,
    /// The slider's screen rect from the last frame it was drawn, used to
    /// map a tap or drag position back to an allocation.
    slider_rect: Rect,
    pointer: Pointer,
    fps: FpsMeter,
}

impl Default for ShireHarvest {
    fn default() -> Self {
        Self {
            seed: 1,
            alloc: 50,
            tax_index: 1,
            season: Season::Spring,
            year: 1275,
            turn: 0,
            turn_timer: 0.0,
            time: 0.0,
            grain_stock: 400.0,
            cattle_stock: 40.0,
            wood_stock: 0.0,
            stone_stock: 0.0,
            iron_stock: 0.0,
            happiness: 60.0,
            slider_rect: Rect::new(0, 0, 0, 0),
            pointer: Pointer::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl ShireHarvest {
    /// The single source of truth for what a labor split means: called by
    /// every draw function to preview a turn, and by [`step_turn`](Self::step_turn)
    /// to resolve one. Nothing about the current state of the shire (stocks,
    /// happiness, turn count) feeds into it -- only the three things a
    /// person can actually move -- which is what makes it possible to prove
    /// preview and outcome agree with a plain equality test.
    fn project(alloc: i32, season: Season, tax_rate: i32) -> Projection {
        let alloc = alloc.clamp(0, 100);
        let farm_target = ((POPULATION * (100 - alloc)) as f32 / 100.0).round() as i32;
        let industry_target = POPULATION - farm_target;

        let farm_workers = farm_target.min(FARM_CAPACITY);
        let farm_idle = (farm_target - farm_workers).max(0);
        let industry_workers = industry_target.min(INDUSTRY_CAPACITY);
        let industry_idle = (industry_target - industry_workers).max(0);

        let understaffed_farm = farm_workers < FARM_CAPACITY;
        let understaffed_industry = industry_workers < INDUSTRY_CAPACITY;

        let grain_workers = farm_workers * GRAIN_SHARE_NUM / GRAIN_SHARE_DEN;
        let cattle_workers = farm_workers - grain_workers;

        let grain_delta = (POPULATION as f32).mul_add(
            -GRAIN_PER_CAPITA,
            grain_workers as f32 * GRAIN_PER_WORKER * grain_multiplier(season),
        );
        let winter_loss = if matches!(season, Season::Winter) {
            WINTER_CATTLE_LOSS
        } else {
            0.0
        };
        let cattle_delta = (cattle_workers as f32 * CATTLE_PER_WORKER)
            .mul_add(cattle_multiplier(season), -winter_loss);

        // Industry workers split as evenly as three sites allow; the
        // remainder (0, 1, or 2 extra hands) goes to wood first, then
        // stone, so the split is deterministic rather than depending on
        // which site happened to be understaffed last.
        let base = industry_workers / 3;
        let rem = industry_workers % 3;
        let wood_workers = base + i32::from(rem > 0);
        let stone_workers = base + i32::from(rem > 1);
        let iron_workers = base;

        let ration_component = if grain_delta >= 0.0 {
            HAPPY_RATION_BONUS
        } else {
            -HAPPY_RATION_PENALTY
        };
        let cattle_component = if cattle_delta >= 0.0 {
            HAPPY_CATTLE_BONUS
        } else {
            -HAPPY_CATTLE_PENALTY
        };
        let happiness_delta =
            (tax_rate as f32).mul_add(-HAPPY_TAX_WEIGHT, ration_component + cattle_component);

        Projection {
            farm_workers,
            industry_workers,
            idle: farm_idle + industry_idle,
            understaffed_farm,
            understaffed_industry,
            grain_delta,
            cattle_delta,
            wood_delta: wood_workers as f32 * WOOD_PER_WORKER,
            stone_delta: stone_workers as f32 * STONE_PER_WORKER,
            iron_delta: iron_workers as f32 * IRON_PER_WORKER,
            happiness_delta,
        }
    }

    const fn tax_rate(&self) -> i32 {
        TAX_RATES[self.tax_index]
    }

    const fn cycle_tax(&mut self) {
        self.tax_index = (self.tax_index + 1) % TAX_RATES.len();
    }

    fn set_alloc(&mut self, alloc: i32) {
        self.alloc = alloc.clamp(0, 100);
    }

    fn nudge_alloc(&mut self, delta: i32) {
        self.set_alloc(self.alloc + delta);
    }

    fn reroll(&mut self) {
        let seed = self.seed.wrapping_add(0x9E37_79B9);
        *self = Self {
            seed,
            ..Self::default()
        };
    }

    /// Applies exactly the projection [`project`](Self::project) reports for
    /// the current allocation, season, and tax rate: this is the "outcome"
    /// half of the preview-equals-outcome guarantee. Stocks clamp at zero
    /// (a shire cannot owe grain back), which is the only place this can
    /// legitimately diverge from a raw delta -- see the unit test, which
    /// picks stock levels the clamp never engages.
    fn step_turn(&mut self) {
        let p = Self::project(self.alloc, self.season, self.tax_rate());
        self.grain_stock = (self.grain_stock + p.grain_delta).max(0.0);
        self.cattle_stock = (self.cattle_stock + p.cattle_delta).max(0.0);
        self.wood_stock += p.wood_delta;
        self.stone_stock += p.stone_delta;
        self.iron_stock += p.iron_delta;
        self.happiness = (self.happiness + p.happiness_delta).clamp(0.0, 100.0);
        self.turn += 1;
        self.season = self.season.next();
        if self.season == Season::Spring {
            self.year += 1;
        }
    }

    /// Advances however many whole turns `dt` seconds cover, capped at a
    /// handful per call so a huge `dt` cannot spin through years in one
    /// frame; the timer keeps the remainder for next time. Identical
    /// reasoning to `59_city_works::CityWorks::advance_turns`.
    fn advance_turns(&mut self, dt: f32) {
        self.turn_timer += dt;
        let mut guard = 0;
        while self.turn_timer >= TURN_SECONDS && guard < 8 {
            self.turn_timer -= TURN_SECONDS;
            self.step_turn();
            guard += 1;
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left | KeyCode::Char('a' | 'A') => self.nudge_alloc(-SLIDER_STEP),
            KeyCode::Right | KeyCode::Char('d' | 'D') => self.nudge_alloc(SLIDER_STEP),
            KeyCode::Char('t' | 'T') => self.cycle_tax(),
            KeyCode::Char('r' | 'R') => self.reroll(),
            _ => {}
        }
    }

    /// Maps a screen column inside [`slider_rect`](Self::slider_rect) to an
    /// allocation, for both a tap (jump to that point) and a drag
    /// (continuously follow the finger) -- the same technique
    /// `49_planet_fall.rs` uses for its budget sliders.
    fn set_alloc_from_x(&mut self, x: u16) {
        let rect = self.slider_rect;
        if rect.width() == 0 {
            return;
        }
        let frac = f32::from(x.saturating_sub(rect.left())) / f32::from(rect.width());
        self.set_alloc((frac.clamp(0.0, 1.0) * 100.0).round() as i32);
    }

    fn handle_gesture(&mut self, gesture: &Gesture) {
        if let Some(pos) = gesture.tap
            && self.slider_rect.contains_pos(pos)
        {
            self.set_alloc_from_x(pos.x);
        }
        if let Some(pos) = gesture.drag
            && self.slider_rect.contains_pos(pos)
        {
            self.set_alloc_from_x(pos.x);
        }
    }

    fn status_line(&self) -> String {
        let p = Self::project(self.alloc, self.season, self.tax_rate());
        format!(
            "{} {}  alloc {}% industry  farm {}/{}  industry {}/{}  idle {}",
            self.season.label(),
            self.year,
            self.alloc,
            p.farm_workers,
            FARM_CAPACITY,
            p.industry_workers,
            INDUSTRY_CAPACITY,
            p.idle
        )
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    fn draw_header(&self, surface: &mut Surface<'_>, area: Rect) {
        if area.height() == 0 {
            return;
        }
        panel::band(surface, area);
        let text = format!(
            "Aldermoor Shire -- Turn {}  Happiness {:.0}",
            self.turn,
            self.happiness.round()
        );
        panel::spans(
            surface,
            (area.left() + 1, area.top()),
            area.width().saturating_sub(2),
            &[Span::keyword(&text)],
            ui::CHROME_BG,
        );
    }

    /// The field glyph for the current season: bare soil in winter, sprouts
    /// in spring, full stalks in summer, ripe heads in autumn -- the same
    /// visual step `14_seasons.rs` uses for its snow line, applied to a
    /// crop cycle instead of a temperature band.
    const fn field_glyph(&self) -> char {
        match self.season {
            Season::Winter => '.',
            Season::Spring => ',',
            Season::Summer => '"',
            Season::Autumn => '*',
        }
    }

    const fn field_color(&self) -> Color {
        match self.season {
            Season::Winter => rgb(120, 110, 96),
            Season::Spring => rgb(140, 176, 96),
            Season::Summer => rgb(170, 196, 70),
            Season::Autumn => rgb(214, 176, 64),
        }
    }

    fn draw_map(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Shire").border(panel::Border::Double);
        let inner = panel.draw(surface, area);
        if inner.width() < 8 || inner.height() < 4 {
            return;
        }

        let cell_w = inner.width() / 4;
        let cell_h = inner.height() / 4;
        if cell_w == 0 || cell_h == 0 {
            return;
        }
        let used_w = cell_w * 4;
        let used_h = cell_h * 4;
        let ox = inner.left() + (inner.width() - used_w) / 2;
        let oy = inner.top() + (inner.height() - used_h) / 2;

        let p = Self::project(self.alloc, self.season, self.tax_rate());
        for (row, tiles) in MAP.iter().enumerate() {
            for (col, tile) in tiles.iter().enumerate() {
                let cell = Rect::new(
                    ox + col as u16 * cell_w,
                    oy + row as u16 * cell_h,
                    cell_w,
                    cell_h,
                );
                self.draw_tile(surface, cell, *tile, row as i32, col as i32, &p);
            }
        }
    }

    fn draw_tile(
        &self,
        surface: &mut Surface<'_>,
        cell: Rect,
        tile: Tile,
        row: i32,
        col: i32,
        p: &Projection,
    ) {
        let (glyph, base_color, dim) = match tile {
            Tile::Keep => ('#', rgb(196, 178, 128), false),
            Tile::Field => (self.field_glyph(), self.field_color(), p.understaffed_farm),
            // Horns-shaped stand-in for grazing cattle; the pasture shares
            // the farm-side labor pool with the fields, so it dims on the
            // same condition.
            Tile::Pasture => ('U', rgb(214, 190, 140), p.understaffed_farm),
            // Spade for a tree stand, the same convention `59_city_works.rs`
            // uses for its Forest terrain.
            Tile::Wood => ('\u{2660}', rgb(96, 140, 86), p.understaffed_industry),
            // Hill-hump glyph for a quarry face, reused from
            // `59_city_works.rs`'s Hills terrain.
            Tile::Stone => ('\u{2229}', rgb(150, 148, 150), p.understaffed_industry),
            Tile::Iron => ('\u{00a7}', rgb(196, 118, 96), p.understaffed_industry),
            // Alternating dashes for the county's stone wall border; see
            // the research notes on Lords of the Realm II's map borders.
            Tile::Wall => ('-', rgb(120, 112, 96), false),
        };
        let bg = rgb(16, 15, 12);
        let color = if dim {
            scale(base_color, 0.55)
        } else {
            base_color
        };
        surface.fill_rect(cell, ' ', Style::new().bg(bg));
        surface.put(
            (cell.left(), cell.top()),
            glyph,
            Style::new().fg(color).bg(bg),
        );

        // A second, hashed glyph gives a field some texture instead of a
        // single character per tile reading as a flat icon; keyed on
        // absolute grid position and the current seed so it stays put
        // between frames but reshuffles on reroll, the same trick
        // `01_terrain_cells.rs` uses for its biome scatter.
        if cell.width() > 2 && matches!(tile, Tile::Field) && hash01(self.seed, col, row) < 0.45 {
            surface.put(
                (cell.left() + 1, cell.top()),
                self.field_glyph(),
                Style::new().fg(color).bg(bg),
            );
        }
    }

    /// The labor slider: a two-color bar (farm left, industry right) with a
    /// handle marking the split, plus a marker on either end when that side
    /// cannot absorb all the labor pointed at it. The handle turns blue when
    /// any labor at all is idle -- the source game's own convention for the
    /// slider figure -- and stays visible without color as the `!` markers
    /// either side of the bar.
    fn draw_slider(&mut self, surface: &mut Surface<'_>, inner: Rect, y: u16, p: &Projection) {
        if inner.width() < 6 || y >= inner.bottom() {
            self.slider_rect = Rect::new(0, 0, 0, 0);
            return;
        }
        let bg = panel::PANEL_BG;
        let farm_marker = if p.understaffed_farm { '!' } else { ' ' };
        let farm_marker_color = if p.understaffed_farm {
            rgb(216, 96, 90)
        } else {
            ui::DIM
        };
        surface.put(
            (inner.left(), y),
            farm_marker,
            Style::new().fg(farm_marker_color).bg(bg),
        );
        let industry_marker = if p.understaffed_industry { '!' } else { ' ' };
        surface.put(
            (inner.right() - 1, y),
            industry_marker,
            Style::new().fg(farm_marker_color).bg(bg),
        );

        let bar_x = inner.left() + 1;
        let bar_w = inner.width().saturating_sub(2);
        if bar_w == 0 {
            self.slider_rect = Rect::new(0, 0, 0, 0);
            return;
        }
        let farm_cols = (f32::from(bar_w) * (100 - self.alloc) as f32 / 100.0).round() as u16;
        let farm_cols = farm_cols.min(bar_w);
        for i in 0..bar_w {
            let is_farm = i < farm_cols;
            let color = if is_farm {
                rgb(150, 176, 90)
            } else {
                rgb(150, 128, 90)
            };
            surface.put((bar_x + i, y), '\u{2588}', Style::new().fg(color).bg(bg));
        }
        let handle_x = farm_cols.min(bar_w.saturating_sub(1));
        let handle_color = if p.idle > 0 {
            rgb(110, 170, 255)
        } else {
            rgb(240, 240, 240)
        };
        surface.put(
            (bar_x + handle_x, y),
            '\u{2551}',
            Style::new().fg(handle_color).bg(bg),
        );

        self.slider_rect = Rect::new(bar_x, y, bar_w, 1);
    }

    /// The production column: three rows of two figures each, exactly the
    /// layout Lords of the Realm II's own panel uses (cows and grain on the
    /// first row, idle and stone on the second, wood and iron on the
    /// third). Every figure is [`format_delta`] applied to a field already
    /// on `p`, so there is nothing here for a draw call to get wrong that
    /// the projection itself did not already decide.
    fn draw_production(surface: &mut Surface<'_>, inner: Rect, y0: u16, p: &Projection) {
        let idle_delta = -(p.idle as f32);
        let rows = [
            (
                format_delta(p.cattle_delta, p.understaffed_farm, "cows"),
                format_delta(p.grain_delta, p.understaffed_farm, "grain"),
            ),
            (
                format_delta(idle_delta, false, "idle"),
                format_delta(p.stone_delta, p.understaffed_industry, "stone"),
            ),
            (
                format_delta(p.wood_delta, p.understaffed_industry, "wood"),
                format_delta(p.iron_delta, p.understaffed_industry, "iron"),
            ),
        ];
        let deltas = [
            (p.cattle_delta, p.grain_delta),
            (idle_delta, p.stone_delta),
            (p.wood_delta, p.iron_delta),
        ];
        let col_w = inner.width() / 2;
        for (i, ((left_text, right_text), (left_v, right_v))) in
            rows.iter().zip(deltas.iter()).enumerate()
        {
            let y = y0 + i as u16;
            if y >= inner.bottom() {
                break;
            }
            surface.print(
                (inner.left(), y),
                retroglyph_widgets::truncate(left_text, col_w as usize),
                Style::new().fg(delta_color(*left_v)).bg(panel::PANEL_BG),
            );
            let right_x = inner.left() + col_w;
            if right_x < inner.right() {
                surface.print(
                    (right_x, y),
                    retroglyph_widgets::truncate(right_text, (inner.width() - col_w) as usize),
                    Style::new().fg(delta_color(*right_v)).bg(panel::PANEL_BG),
                );
            }
        }
    }

    fn draw_control(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title("County: Aldermoor")
            .border(panel::Border::Double);
        let inner = panel.draw(surface, area);
        if inner.width() < 10 || inner.height() == 0 {
            self.slider_rect = Rect::new(0, 0, 0, 0);
            return;
        }
        let p = Self::project(self.alloc, self.season, self.tax_rate());
        let mut y = inner.top();

        let header = format!("{} {}   turn {}", self.season.label(), self.year, self.turn);
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::keyword(&header)],
            panel::PANEL_BG,
        );
        y += 1;
        if y >= inner.bottom() {
            self.slider_rect = Rect::new(0, 0, 0, 0);
            return;
        }

        let labor_line = format!(
            "Peasants {POPULATION}  Farm {}/{FARM_CAPACITY}  Industry {}/{INDUSTRY_CAPACITY}",
            p.farm_workers, p.industry_workers
        );
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::dim(&labor_line)],
            panel::PANEL_BG,
        );
        y += 1;
        if y >= inner.bottom() {
            self.slider_rect = Rect::new(0, 0, 0, 0);
            return;
        }

        self.draw_slider(surface, inner, y, &p);
        y += 1;
        if y >= inner.bottom() {
            return;
        }

        Self::draw_production(surface, inner, y, &p);
        y += 3;
        if y >= inner.bottom() {
            return;
        }

        let stocks = format!(
            "Stock: grain {}  cattle {}  wood {}  stone {}  iron {}",
            self.grain_stock.round() as i32,
            self.cattle_stock.round() as i32,
            self.wood_stock.round() as i32,
            self.stone_stock.round() as i32,
            self.iron_stock.round() as i32
        );
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::dim(retroglyph_widgets::truncate(
                &stocks,
                inner.width() as usize,
            ))],
            panel::PANEL_BG,
        );
        y += 1;
        if y >= inner.bottom() {
            return;
        }

        let happy_text = format!(
            "\u{2665} {} ({})  Tax {}% [T]",
            self.happiness.round() as i32,
            format_signed(p.happiness_delta),
            self.tax_rate()
        );
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::new(
                retroglyph_widgets::truncate(&happy_text, inner.width() as usize),
                delta_color(p.happiness_delta),
            )],
            panel::PANEL_BG,
        );
    }

    // ── Layout ───────────────────────────────────────────────────────────

    fn layout_and_draw(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let shape = Shape::of(content);
        let (header, rest) = panel::split_top(content, 1);
        self.draw_header(surface, header);
        if rest.height() == 0 {
            return;
        }

        if shape.stacks() {
            let map_h = (rest.height() * 2 / 5).clamp(8, rest.height());
            let (map_area, control_area) = panel::split_top(rest, map_h);
            self.draw_map(surface, map_area);
            self.draw_control(surface, control_area);
        } else {
            let control_w = (rest.width() * 2 / 5).clamp(28, 50).min(rest.width());
            let (map_area, control_area) = panel::split_right(rest, control_w);
            self.draw_map(surface, map_area);
            self.draw_control(surface, control_area);
        }
    }
}

/// One kind of ground in the shire's worked area.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Keep,
    Field,
    Pasture,
    Wood,
    Stone,
    Iron,
    Wall,
}

/// The shire's fixed 4x4 layout: one keep, six grain fields (matching
/// [`FARM_CAPACITY`]'s six-field assumption), two pastures, the three works
/// (matching [`INDUSTRY_CAPACITY`]'s three-site assumption), and border
/// wall tiles standing in for the source game's dashed stone boundary.
const MAP: [[Tile; 4]; 4] = [
    [Tile::Keep, Tile::Field, Tile::Field, Tile::Field],
    [Tile::Field, Tile::Field, Tile::Field, Tile::Wall],
    [Tile::Pasture, Tile::Pasture, Tile::Wood, Tile::Stone],
    [Tile::Wall, Tile::Wall, Tile::Iron, Tile::Wall],
];

/// Formats a signed delta with its unit, bracketing it when `understaffed`
/// is true. The bracket is the non-color half of the understaffed signal --
/// see the module docs -- so it is applied here, once, rather than left for
/// each call site to remember.
fn format_delta(value: f32, understaffed: bool, unit: &str) -> String {
    let n = value.round() as i32;
    let text = format!("{n:+} {unit}");
    if understaffed {
        format!("[{text}]")
    } else {
        text
    }
}

fn format_signed(value: f32) -> String {
    format!("{:+}", value.round() as i32)
}

/// Green for a surplus, red for a deficit, dim for a wash -- sign, not
/// magnitude, which is the rule the module docs call out explicitly.
fn delta_color(value: f32) -> Color {
    if value > DELTA_EPS {
        rgb(120, 206, 120)
    } else if value < -DELTA_EPS {
        rgb(216, 96, 90)
    } else {
        ui::DIM
    }
}

impl Demo for ShireHarvest {
    const NAME: &'static str = "67_shire_harvest";
    const TITLE: &'static str = "Shire Harvest";
    const BLURB: &'static str =
        "Lords of the Realm II: a labor slider whose production preview is the turn's own math.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("Left/Right, A/D", "shift labor split"),
            ("drag slider", "shift labor split"),
            ("T", "cycle tax rate"),
            ("R", "reroll shire"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        let dt = frame.delta.as_secs_f32();
        self.time += dt;
        self.fps.record(frame.delta);
        self.advance_turns(dt);

        let mut keep_running = true;
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                keep_running = false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = &event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        if !keep_running {
            return false;
        }

        let gesture = self.pointer.take();
        self.handle_gesture(&gesture);

        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        self.layout_and_draw(&mut surface, content);

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status_line();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Season, ShireHarvest};

    /// The headline guarantee: the number the panel shows while dragging the
    /// slider is the number the shire actually receives once the season
    /// turns. Autumn, mid-slider, zero tax is chosen so grain and cattle are
    /// both in surplus; stocks start at zero so the addition the assertion
    /// checks never engages `step_turn`'s zero-floor clamp, and so a large
    /// base value never swallows the delta in `f32` rounding the way a
    /// stock of 10,000 would.
    #[test]
    fn preview_matches_the_actual_outcome() {
        let mut demo = ShireHarvest {
            alloc: 40,
            season: Season::Autumn,
            tax_index: 0,
            grain_stock: 0.0,
            cattle_stock: 0.0,
            ..ShireHarvest::default()
        };
        let preview = ShireHarvest::project(demo.alloc, demo.season, demo.tax_rate());
        let (grain_before, cattle_before, wood_before, stone_before, iron_before, happy_before) = (
            demo.grain_stock,
            demo.cattle_stock,
            demo.wood_stock,
            demo.stone_stock,
            demo.iron_stock,
            demo.happiness,
        );

        demo.step_turn();

        assert!((demo.grain_stock - grain_before - preview.grain_delta).abs() < 1e-4);
        assert!((demo.cattle_stock - cattle_before - preview.cattle_delta).abs() < 1e-4);
        assert!((demo.wood_stock - wood_before - preview.wood_delta).abs() < 1e-4);
        assert!((demo.stone_stock - stone_before - preview.stone_delta).abs() < 1e-4);
        assert!((demo.iron_stock - iron_before - preview.iron_delta).abs() < 1e-4);
        assert!((demo.happiness - happy_before - preview.happiness_delta).abs() < 1e-4);
    }

    #[test]
    fn full_industry_allocation_leaves_farm_capacity_idle() {
        let p = ShireHarvest::project(100, Season::Summer, 0);
        assert_eq!(p.farm_workers, 0);
        assert!(p.understaffed_farm);
        // At full industry allocation every peasant targets industry, which
        // cannot hold all of them: the excess above INDUSTRY_CAPACITY is
        // idle, not silently dropped or double-counted onto the farm side.
        assert!(p.idle > 0);
    }

    #[test]
    fn winter_grain_is_a_deficit_at_the_same_split_summer_is_not() {
        let winter = ShireHarvest::project(50, Season::Winter, 0);
        let summer = ShireHarvest::project(50, Season::Summer, 0);
        assert!(winter.grain_delta < 0.0);
        // Same allocation, different season, different sign: the seasonal
        // loop changes what the slider means, not just how the map looks.
        assert!(winter.grain_delta < summer.grain_delta);
    }

    #[test]
    fn tax_only_ever_lowers_the_happiness_projection() {
        let low = ShireHarvest::project(50, Season::Spring, 0);
        let high = ShireHarvest::project(50, Season::Spring, 20);
        assert!(high.happiness_delta < low.happiness_delta);
    }
}

ascii_tile_demos::demo_main!(ShireHarvest);
