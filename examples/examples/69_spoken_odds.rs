//! 69: Spoken Odds -- a dialogue list whose length depends on your build,
//! from The Age of Decadence.
//!
//! Iron Tower's The Age of Decadence checks every dialogue option against
//! your character sheet with no dice involved: you either meet the listed
//! skill (and, sometimes, reputation) thresholds or you do not. What makes
//! the system distinctive is not the arithmetic but what happens to an
//! option you fail -- it is not greyed out, it is not shown at all. The
//! menu a Persuasion-heavy diplomat sees at a checkpoint and the menu a
//! Dexterity-heavy cutthroat sees at the *same* checkpoint can share as
//! little as one row, and neither character ever learns what the other
//! could have said. That is a sharper way to teach "your build is your
//! dialogue tree" than a greyed-out row ever could, so this demo puts three
//! prebuilt characters on one hotkey and lets a viewer watch the same node's
//! option count and contents change as they switch, with an explicit
//! (off-by-default) reveal that shows what each character is missing --
//! the demo's own teaching aid, not something the source game ever does.
//!
//! Techniques on show:
//!
//! - **Inline, deterministic skill-check annotation**
//!   ([`req_segments`], [`Character::passes`]): every gated option carries
//!   its check in the option text itself -- `Persuasion:[4]`, a combined
//!   `Persuasion+Streetwise=[8]`, or an AND of two individually-thresholded
//!   skills like `Dexterity:[7]+Dodge:[2]=[11]` -- reproduced from the exact
//!   wiki-quoted format, colored so the skill name, the required number, and
//!   the spoken line each read as a distinct kind of information.
//! - **Hidden, not disabled, options** ([`SpokenOdds::draw_options`]): a
//!   failed check removes its row from the list rather than dimming it, so
//!   switching characters changes how many numbered options exist, not just
//!   which ones are enabled.
//! - **A reputation-gated option** ([`Req::Rep`]): one row checks a general
//!   reputation track (Body Count) instead of a skill, shown with the same
//!   bracketed-threshold format, because the source game gates on both.
//! - **Consequence prose over bullet outcomes**
//!   ([`SpokenOdds::draw_scene`], [`wrap`]): picking an option replaces the
//!   node with a wrapped paragraph and a fresh option list, matching the
//!   source game's habit of narrating an outcome in prose rather than a
//!   one-line result.
//! - **A character sheet that highlights the skills in play**
//!   ([`SpokenOdds::draw_sheet`]): the sidebar always shows the six skills
//!   this node's options check, brightening the ones actually referenced by
//!   the node on screen, so a viewer can read *why* a row appeared or
//!   vanished without hunting through the option text.
//!
//! ```sh
//! cargo run --example 69_spoken_odds --features crossterm
//! cargo run --example 69_spoken_odds --features software
//! cargo run --example 69_spoken_odds --features gl
//! cargo run --example 69_spoken_odds  # headless, prints a few frames
//! ```

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Backend, Color, Frame, Rect, Style, Surface, Terminal};

use ascii_tile_demos::Demo;
use ascii_tile_demos::ui::panel::{self, Border, Panel, Span};
use ascii_tile_demos::ui::touch::{Hotspots, Pointer, Shape};
use ascii_tile_demos::ui::{self, ACCENT, DIM, FG};
use ascii_tile_demos::util::perf::FpsMeter;
use tilekit::palette::rgb;

/// Color for a skill or reputation name inside a check annotation. Cool and
/// desaturated so it reads as a label, not as content to be read aloud --
/// that job belongs to [`SPEECH_COLOR`].
const SKILL_COLOR: Color = rgb(120, 188, 214);

/// Color for the bracketed required number in a check annotation. Shares
/// [`ACCENT`] so the number that decides whether a row exists at all draws
/// the eye the same way the rest of the gallery's "the number that matters"
/// convention does.
const VALUE_COLOR: Color = ACCENT;

/// Color for spoken dialogue text, once a check (if any) has been read.
const SPEECH_COLOR: Color = FG;

/// Color for a hidden option shown only because the reveal toggle is on.
/// Distinct from [`DIM`] chrome text so a revealed row still reads as
/// "content that exists but failed", not as inert chrome.
const HIDDEN_COLOR: Color = rgb(150, 78, 78);

/// One civil skill or stat The Age of Decadence checks in dialogue. Only the
/// six this demo's nodes actually reference -- the real game's sheet runs to
/// a dozen more, but a sheet that shows skills no visible node ever checks
/// would just be decoration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Skill {
    Persuasion,
    Streetwise,
    Sneak,
    Dexterity,
    Dodge,
    Intelligence,
}

impl Skill {
    const ALL: [Self; 6] = [
        Self::Persuasion,
        Self::Streetwise,
        Self::Sneak,
        Self::Dexterity,
        Self::Dodge,
        Self::Intelligence,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Persuasion => "Persuasion",
            Self::Streetwise => "Streetwise",
            Self::Sneak => "Sneak",
            Self::Dexterity => "Dexterity",
            Self::Dodge => "Dodge",
            Self::Intelligence => "Intelligence",
        }
    }
}

/// A general reputation track, tallied across the whole game rather than
/// per-faction. Only Body Count is actually checked by a node here; Word of
/// Honor and Treachery are carried on the sheet because the brief asks for
/// the reputation line to be visible even where nothing gates on it, the
/// same way most of a character's stat block never gets checked in any one
/// conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rep {
    BodyCount,
    WordOfHonor,
    Treachery,
}

impl Rep {
    const fn name(self) -> &'static str {
        match self {
            Self::BodyCount => "Body Count",
            Self::WordOfHonor => "Word of Honor",
            Self::Treachery => "Treachery",
        }
    }
}

/// A dialogue gate. Deliberately three shapes, not one, because the source
/// material uses three: a lone threshold, an averaged/summed pair, and an
/// AND of two individually-thresholded skills that also happens to print
/// its sum. See [`req_segments`] for how each renders.
#[derive(Clone, Copy)]
enum Req {
    /// No check: always available, like the source game's plain "Fight" option.
    None,
    /// `Skill:[value]`.
    Skill(Skill, i32),
    /// `SkillA+SkillB=[total]`: passes if the two skills' values *sum* to at
    /// least `total`. Neither skill is thresholded on its own -- a character
    /// strong in one and weak in the other can still clear it.
    Sum(Skill, Skill, i32),
    /// `SkillA:[a]+SkillB:[b]` (`show_total` adds `=[a+b]`): passes only if
    /// *both* individual thresholds are met. The displayed sum, when shown,
    /// is decoration; the check itself is the AND of the two brackets.
    And {
        a: Skill,
        na: i32,
        b: Skill,
        nb: i32,
        show_total: bool,
    },
    /// `Rep:[value]`: gated on a general reputation track instead of a
    /// skill, using the same bracketed-threshold notation.
    Rep(Rep, i32),
}

/// Which of the six sidebar skills a requirement references, for
/// highlighting them in [`SpokenOdds::draw_sheet`].
fn req_skills(req: &Req) -> Vec<Skill> {
    match req {
        Req::None | Req::Rep(..) => Vec::new(),
        Req::Skill(s, _) => vec![*s],
        Req::Sum(a, b, _) | Req::And { a, b, .. } => vec![*a, *b],
    }
}

/// One gated dialogue option: its check, the line it speaks or the action it
/// takes, and which node picking it leads to.
#[derive(Clone, Copy)]
struct Opt {
    req: Req,
    /// The separator between the check annotation and `tail` -- some of the
    /// source game's own lines put a comma before the text, some do not
    /// (compare `Sneak:[4] Get closer` with `Dexterity:[7]+Dodge:[2]=[11],
    /// Make a run for it` in the wiki's own examples), so this is carried
    /// per option rather than derived from a rule that does not hold.
    sep: &'static str,
    /// The spoken line (quoted) or action text, exactly as it should render
    /// after the check and separator.
    tail: &'static str,
    /// Index into [`NODES`] this option leads to.
    target: usize,
}

const fn opt(req: Req, sep: &'static str, tail: &'static str, target: usize) -> Opt {
    Opt {
        req,
        sep,
        tail,
        target,
    }
}

/// One dialogue node: a speaker and scene line, a prose paragraph, and the
/// gated options branching from it.
struct Node {
    speaker: &'static str,
    scene: &'static str,
    prose: &'static str,
    options: Vec<Opt>,
}

/// Builds the dialogue graph: one checkpoint node with every check shape the
/// module docs promise, and one short consequence node per option, each
/// looping back to the checkpoint so a viewer can try the next character
/// against the same gate without losing the thread.
///
/// A function rather than a `const` table because [`Opt`]/[`Node`] hold a
/// `Vec`, which cannot appear in a `const`; called once, in
/// [`SpokenOdds::default`], and never mutated afterward.
#[allow(clippy::too_many_lines)]
fn build_nodes() -> Vec<Node> {
    vec![
        // Node 0: the checkpoint. Every option here reproduces one of the
        // check shapes from the wiki-sourced brief, several verbatim.
        Node {
            speaker: "Gate Warden",
            scene: "The eastern gate of Teron, an hour before the watch changes.",
            prose: "\"Road's closed past this point. Orders. Turn back or give me a \
                reason I should care.\"",
            options: vec![
                opt(
                    Req::Skill(Skill::Persuasion, 4),
                    ", ",
                    "\"If I'm stuck here, I'll get nothing.\"",
                    1,
                ),
                opt(
                    Req::Skill(Skill::Streetwise, 3),
                    ", ",
                    "\"The Ordu are coming.\"",
                    2,
                ),
                opt(Req::Skill(Skill::Sneak, 4), " ", "Get closer.", 3),
                opt(
                    Req::Sum(Skill::Persuasion, Skill::Streetwise, 8),
                    ", ",
                    "\"Look, I'm just an employee.\"",
                    4,
                ),
                opt(
                    Req::And {
                        a: Skill::Dexterity,
                        na: 7,
                        b: Skill::Dodge,
                        nb: 2,
                        show_total: true,
                    },
                    ", ",
                    "Make a run for it.",
                    5,
                ),
                opt(
                    Req::And {
                        a: Skill::Intelligence,
                        na: 8,
                        b: Skill::Persuasion,
                        nb: 6,
                        show_total: false,
                    },
                    ", ",
                    "\"Because we bring money to the table...\"",
                    6,
                ),
                opt(Req::None, " ", "Draw steel and fight.", 7),
                opt(
                    Req::Rep(Rep::BodyCount, 3),
                    ", ",
                    "\"You don't want trouble with someone like me.\"",
                    8,
                ),
            ],
        },
        // 1: Persuasion
        Node {
            speaker: "Gate Warden",
            scene: "The warden weighs your point.",
            prose: "He turns the reasoning over the way he'd turn a coin, looking for the \
                counterfeit in it, and finds none. \"Fine. Nothing in it for me to keep you \
                here. Go, before I remember I could still make trouble.\" He steps aside \
                without another word.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 2: Streetwise
        Node {
            speaker: "Gate Warden",
            scene: "The word Ordu drains the color from his face.",
            prose: "\"The Ordu. Here.\" It isn't a question. He waves you through with the \
                same hand that was blocking you a moment ago, already turning to shout for \
                the sergeant of the watch. Whatever business brought you to this gate has \
                just become the least urgent thing happening at it.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 3: Sneak
        Node {
            speaker: "Gate Warden",
            scene: "You close the distance without a sound.",
            prose: "He doesn't notice you're within arm's reach until you're already there, \
                and the surprise buys a private word he wouldn't have given a shout across \
                the road. Whatever you say next, he's listening closer than he means to.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 4: Persuasion+Streetwise sum
        Node {
            speaker: "Gate Warden",
            scene: "He measures you the way a man measures cargo.",
            prose: "\"Employee.\" He looks you over once more -- the boots, the calluses, \
                the way you're not reaching for a weapon -- and decides you're telling the \
                kind of small, uninteresting truth nobody bothers inventing. \"Through, then. \
                Don't make me remember your face.\"",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 5: Dexterity+Dodge
        Node {
            speaker: "Gate Warden",
            scene: "You're past him before he's finished reaching.",
            prose: "The gap between his hand and his sword is exactly as wide as you need it \
                to be. By the time he's shouting for the watch, you're three streets gone and \
                the shout is somebody else's problem now.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 6: Intelligence+Persuasion
        Node {
            speaker: "Gate Warden",
            scene: "The argument lands somewhere he wasn't expecting.",
            prose: "You lay out the Commercium's interest in the road plainly enough that he \
                can follow the thread himself, which is worth more than if you'd simply told \
                him the conclusion. \"Didn't think of it that way,\" he admits, and steps \
                aside like a man conceding a point rather than losing an argument.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 7: Fight (always available)
        Node {
            speaker: "Gate Warden",
            scene: "Words end; the watch is already closing in.",
            prose: "It is not a fair fight and it is not meant to be one -- a single warden \
                against whoever answers his shout inside the minute. Whatever happens next \
                happens somewhere other than this gate.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
        // 8: Reputation
        Node {
            speaker: "Gate Warden",
            scene: "He recognizes the name, or near enough to it.",
            prose: "Something in the way you say it, or something he's heard said about you \
                elsewhere, changes his calculation entirely. \"Didn't say I'd stop you,\" he \
                mutters, and finds somewhere else to look while you pass.",
            options: vec![opt(Req::None, " ", "Return to the gate.", 0)],
        },
    ]
}

/// One of the three prebuilt characters the demo switches between. Each is
/// built to clear a different, mostly-disjoint subset of the checkpoint's
/// eight options, so switching characters visibly changes both the count and
/// the content of the list rather than just which single row comes and
/// goes.
#[derive(Clone, Copy)]
struct Character {
    name: &'static str,
    background: &'static str,
    persuasion: i32,
    streetwise: i32,
    sneak: i32,
    dexterity: i32,
    dodge: i32,
    intelligence: i32,
    body_count: i32,
    word_of_honor: i32,
    treachery: i32,
}

impl Character {
    const fn skill(&self, skill: Skill) -> i32 {
        match skill {
            Skill::Persuasion => self.persuasion,
            Skill::Streetwise => self.streetwise,
            Skill::Sneak => self.sneak,
            Skill::Dexterity => self.dexterity,
            Skill::Dodge => self.dodge,
            Skill::Intelligence => self.intelligence,
        }
    }

    const fn rep(&self, rep: Rep) -> i32 {
        match rep {
            Rep::BodyCount => self.body_count,
            Rep::WordOfHonor => self.word_of_honor,
            Rep::Treachery => self.treachery,
        }
    }

    /// Whether this character clears `req`. The one deterministic function
    /// every option's visibility runs through -- no roll, no margin, just a
    /// comparison, which is the entire point of the source game's system.
    const fn passes(&self, req: &Req) -> bool {
        match req {
            Req::None => true,
            Req::Skill(s, n) => self.skill(*s) >= *n,
            Req::Sum(a, b, total) => self.skill(*a) + self.skill(*b) >= *total,
            Req::And { a, na, b, nb, .. } => self.skill(*a) >= *na && self.skill(*b) >= *nb,
            Req::Rep(r, n) => self.rep(*r) >= *n,
        }
    }
}

const fn build_characters() -> [Character; 3] {
    [
        Character {
            name: "Naiah Cross",
            background: "Commercium Broker",
            persuasion: 8,
            streetwise: 6,
            sneak: 2,
            dexterity: 3,
            dodge: 2,
            intelligence: 5,
            body_count: 0,
            word_of_honor: 4,
            treachery: 1,
        },
        Character {
            name: "Rurik Dun",
            background: "Blade for Hire",
            persuasion: 2,
            streetwise: 4,
            sneak: 6,
            dexterity: 8,
            dodge: 7,
            intelligence: 3,
            body_count: 6,
            word_of_honor: 1,
            treachery: 3,
        },
        Character {
            name: "Old Maros",
            background: "Wandering Scholar",
            persuasion: 6,
            streetwise: 2,
            sneak: 3,
            dexterity: 2,
            dodge: 2,
            intelligence: 9,
            body_count: 1,
            word_of_honor: 5,
            treachery: 0,
        },
    ]
}

/// Splits a check into `(text, color)` segments: the skill or reputation
/// name in [`SKILL_COLOR`], every bracketed number in [`VALUE_COLOR`].
/// Reproduces the wiki-quoted format precisely, including the two shapes
/// that put a name on both sides of a `+` -- one that never brackets the
/// individual skills ([`Req::Sum`]) and one that always does ([`Req::And`]).
fn req_segments(req: &Req) -> Vec<(String, Color)> {
    match req {
        Req::None => Vec::new(),
        Req::Skill(s, n) => vec![
            (format!("{}:", s.name()), SKILL_COLOR),
            (format!("[{n}]"), VALUE_COLOR),
        ],
        Req::Sum(a, b, total) => vec![
            (format!("{}+", a.name()), SKILL_COLOR),
            (format!("{}=", b.name()), SKILL_COLOR),
            (format!("[{total}]"), VALUE_COLOR),
        ],
        Req::And {
            a,
            na,
            b,
            nb,
            show_total,
        } => {
            let mut segs = vec![
                (format!("{}:", a.name()), SKILL_COLOR),
                (format!("[{na}]+"), VALUE_COLOR),
                (format!("{}:", b.name()), SKILL_COLOR),
                (format!("[{nb}]"), VALUE_COLOR),
            ];
            if *show_total {
                segs.push((format!("=[{}]", na + nb), VALUE_COLOR));
            }
            segs
        }
        Req::Rep(r, n) => vec![
            (format!("{}:", r.name()), SKILL_COLOR),
            (format!("[{n}]"), VALUE_COLOR),
        ],
    }
}

/// Greedy word-wraps `text` to at most `width` columns per line. The demo's
/// own copy of the wrap every prose-heavy demo in the gallery carries (see
/// `56_open_terms.rs`, `47_hollow_talk.rs`): each one is self-contained, so
/// this stays local rather than reaching into a shared module that other
/// agents are editing.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let cols = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!current.is_empty());
        if current.chars().count() + extra + word.chars().count() > cols && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// What a tap or a key resolves to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Character(usize),
    Option(usize),
    Reveal,
}

/// State for the whole demo.
///
/// Holds the dialogue graph, the roster of three prebuilt builds, which one
/// and which node are current, and the reveal toggle that is this demo's own
/// teaching aid rather than anything the source game shows.
pub struct SpokenOdds {
    nodes: Vec<Node>,
    characters: [Character; 3],
    selected: usize,
    node: usize,
    /// Off by default, matching the source game: a failed check simply does
    /// not appear. Turning this on additionally draws the options this
    /// character fails, dimmed, so a viewer can see what switching builds
    /// actually took away.
    reveal_hidden: bool,
    time: f32,
    pointer: Pointer,
    hotspots: Hotspots<Hit>,
    fps: FpsMeter,
}

impl Default for SpokenOdds {
    fn default() -> Self {
        Self {
            nodes: build_nodes(),
            characters: build_characters(),
            selected: 0,
            node: 0,
            reveal_hidden: false,
            time: 0.0,
            pointer: Pointer::new(),
            hotspots: Hotspots::new(),
            fps: FpsMeter::new(),
        }
    }
}

impl SpokenOdds {
    const fn character(&self) -> &Character {
        &self.characters[self.selected]
    }

    /// Indices of `self.node`'s options this character passes, in authored
    /// order. What actually gets numbered and shown when the reveal toggle
    /// is off -- the list the source game would show.
    fn visible_indices(&self) -> Vec<usize> {
        let ch = self.character();
        self.nodes[self.node]
            .options
            .iter()
            .enumerate()
            .filter(|(_, o)| ch.passes(&o.req))
            .map(|(i, _)| i)
            .collect()
    }

    const fn select_character(&mut self, idx: usize) {
        self.selected = idx;
    }

    fn choose_option(&mut self, opt_idx: usize) {
        if let Some(opt) = self.nodes[self.node].options.get(opt_idx) {
            self.node = opt.target;
        }
    }

    fn handle_events<B: Backend>(&mut self, term: &mut Terminal<B>) -> bool {
        for event in term.drain_events() {
            if ui::is_quit(&event) {
                return false;
            }
            self.pointer.feed(&event);
            if let Event::Key(key) = event
                && key.is_down()
            {
                self.handle_key(key.code);
            }
        }
        true
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.select_character((self.selected + 1) % self.characters.len()),
            KeyCode::Char('1') => self.select_character(0),
            KeyCode::Char('2') => self.select_character(1),
            KeyCode::Char('3') => self.select_character(2),
            KeyCode::Char('h' | 'H') => self.reveal_hidden = !self.reveal_hidden,
            KeyCode::Char('r' | 'R') => self.node = 0,
            KeyCode::Char(c @ '4'..='9') => {
                let idx = c as usize - '1' as usize;
                let visible = self.visible_indices();
                if let Some(&opt_idx) = visible.get(idx) {
                    self.choose_option(opt_idx);
                }
            }
            _ => {}
        }
    }

    fn handle_tap(&mut self) {
        let gesture = self.pointer.take();
        let Some(pos) = gesture.tap else {
            return;
        };
        match self.hotspots.hit(pos) {
            Some(&Hit::Character(idx)) => self.select_character(idx),
            Some(&Hit::Option(opt_idx)) => self.choose_option(opt_idx),
            Some(&Hit::Reveal) => self.reveal_hidden = !self.reveal_hidden,
            None => {}
        }
    }

    fn status(&self) -> String {
        let count = self.visible_indices().len();
        format!(
            "{} ({})  {} option{} visible  reveal {}",
            self.character().name,
            self.character().background,
            count,
            if count == 1 { "" } else { "s" },
            if self.reveal_hidden { "on" } else { "off" }
        )
    }

    // -- layout ---------------------------------------------------------

    fn draw<B: Backend>(&mut self, term: &mut Terminal<B>) {
        self.hotspots.clear();
        let screen = term.area();
        let (title, content, status) = ui::split_chrome(screen);
        let mut surface = term.surface();
        ui::fill(&mut surface, content, Style::new().bg(ui::BG));

        if Shape::of(content).stacks() {
            self.draw_stacked(&mut surface, content);
        } else {
            self.draw_columns(&mut surface, content);
        }

        ui::title_bar::<Self>(&mut surface, title);
        let text = self.status();
        ui::status_bar::<Self>(&mut surface, status, &text, &self.fps);
    }

    /// Portrait phones and the 80x24 test grid: scene/prose on top, options
    /// in the middle, the roster and sheet stacked at the bottom where a
    /// column layout would have put them at the side.
    fn draw_stacked(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let h = content.height();
        let roster_h = 6u16.min(h);
        let sheet_h = 8u16.min(h.saturating_sub(roster_h));
        let scene_h = 6u16.min(h.saturating_sub(roster_h + sheet_h) / 2 + 3);
        let (scene_area, rest) = panel::split_top(content, scene_h);
        let (options_area, rest) =
            panel::split_top(rest, rest.height().saturating_sub(roster_h + sheet_h));
        let (sheet_area, roster_area) = panel::split_top(rest, sheet_h);

        self.draw_scene(surface, scene_area);
        self.draw_options(surface, options_area);
        self.draw_sheet(surface, sheet_area);
        self.draw_roster(surface, roster_area);
    }

    /// Landscape and desktop: a left sidebar carrying the roster over the
    /// character sheet, a right column carrying scene/prose over the option
    /// list -- the shape the brief describes, "character sheet in a
    /// sidebar."
    fn draw_columns(&mut self, surface: &mut Surface<'_>, content: Rect) {
        let sidebar_w = 26u16.min(content.width() / 3);
        let (sidebar, main) = panel::split_left(content, sidebar_w);
        let roster_h = 8u16.min(sidebar.height());
        let (roster_area, sheet_area) = panel::split_top(sidebar, roster_h);

        let scene_h = 7u16.min(main.height() / 2);
        let (scene_area, options_area) = panel::split_top(main, scene_h);

        self.draw_roster(surface, roster_area);
        self.draw_sheet(surface, sheet_area);
        self.draw_scene(surface, scene_area);
        self.draw_options(surface, options_area);
    }

    /// The roster: three buttons, one per prebuilt character, plus the
    /// reveal toggle. Selecting one is the whole mechanism of this demo, so
    /// it gets its own panel rather than sharing with the sheet.
    fn draw_roster(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Characters").border(Border::Single);
        let inner = panel.draw(surface, area);
        if inner.width() < 6 || inner.height() == 0 {
            return;
        }

        let rows = inner.height().min(4);
        for i in 0..self
            .characters
            .len()
            .min(usize::from(rows.saturating_sub(1)))
        {
            let y = inner.top() + i as u16;
            let ch = &self.characters[i];
            let selected = i == self.selected;
            let marker = if selected { '\u{25BA}' } else { ' ' };
            let color = if selected { ACCENT } else { FG };
            let line = format!("{} {}", i + 1, ch.name);
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[
                    Span::new(&marker.to_string(), ACCENT),
                    Span::new(" ", color),
                    Span::new(&line, color),
                ],
                panel::PANEL_BG,
            );
            let slot = Rect::new(inner.left(), y, inner.width(), 1);
            self.hotspots.push_tappable(slot, area, Hit::Character(i));
        }

        if rows > 0 {
            let y = inner.top() + rows - 1;
            let label = if self.reveal_hidden {
                "[H] hide the misses"
            } else {
                "[H] reveal the misses"
            };
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::dim(label)],
                panel::PANEL_BG,
            );
            let slot = Rect::new(inner.left(), y, inner.width(), 1);
            self.hotspots.push_tappable(slot, area, Hit::Reveal);
        }
    }

    /// The character sheet: the six skills this demo's nodes check, plus the
    /// reputation line, for whichever character is selected. Skills the
    /// *current node* actually references are brightened, so a viewer can
    /// read straight off the sheet why an option is or isn't on the list
    /// below, without re-deriving it from the annotation.
    fn draw_sheet(&self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new().title("Sheet").border(Border::Single);
        let inner = panel.draw(surface, area);
        if inner.width() < 8 || inner.height() == 0 {
            return;
        }
        let ch = self.character();
        let in_play: Vec<Skill> = self.nodes[self.node]
            .options
            .iter()
            .flat_map(|o| req_skills(&o.req))
            .collect();

        let mut y = inner.top();
        for skill in Skill::ALL {
            if y >= inner.bottom() {
                break;
            }
            let active = in_play.contains(&skill);
            let color = if active { VALUE_COLOR } else { DIM };
            let line = format!("{:<12} {:>2}", skill.name(), ch.skill(skill));
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::new(&line, color)],
                panel::PANEL_BG,
            );
            y += 1;
        }

        if y < inner.bottom() {
            y += 1;
        }
        let rep_lines: [(&str, i32); 3] = [
            (Rep::BodyCount.name(), ch.body_count),
            (Rep::WordOfHonor.name(), ch.word_of_honor),
            (Rep::Treachery.name(), ch.treachery),
        ];
        for (name, value) in rep_lines {
            if y >= inner.bottom() {
                break;
            }
            let active = self.nodes[self.node]
                .options
                .iter()
                .any(|o| matches!(&o.req, Req::Rep(r, _) if r.name() == name));
            let color = if active { VALUE_COLOR } else { DIM };
            let line = format!("{name:<12} {value:>2}");
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::new(&line, color)],
                panel::PANEL_BG,
            );
            y += 1;
        }
    }

    /// The speaker, the scene line, and the prose paragraph -- the source
    /// game's habit of narrating in a wrapped block rather than a bullet
    /// list.
    fn draw_scene(&self, surface: &mut Surface<'_>, area: Rect) {
        let node = &self.nodes[self.node];
        let panel = Panel::new()
            .title(node.speaker)
            .border(Border::Double)
            .focused(true);
        let inner = panel.draw(surface, area);
        if inner.width() < 8 || inner.height() == 0 {
            return;
        }

        let mut y = inner.top();
        panel::spans(
            surface,
            (inner.left(), y),
            inner.width(),
            &[Span::dim(node.scene)],
            panel::PANEL_BG,
        );
        y += 1;
        if y < inner.bottom() {
            y += 1;
        }

        for line in wrap(node.prose, inner.width()) {
            if y >= inner.bottom() {
                break;
            }
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &[Span::new(&line, SPEECH_COLOR)],
                panel::PANEL_BG,
            );
            y += 1;
        }
    }

    /// The numbered option list. Only rows this character passes are
    /// numbered and drawn in full color when [`Self::reveal_hidden`] is off,
    /// matching the source game exactly. With it on, failed rows are drawn
    /// too, dimmed and unnumbered, so a viewer can see what the switch just
    /// took away -- clearly marked as the demo's own addition, both here and
    /// in the roster's toggle label.
    fn draw_options(&mut self, surface: &mut Surface<'_>, area: Rect) {
        let panel = Panel::new()
            .title("What do you say?")
            .border(Border::Single);
        let inner = panel.draw(surface, area);
        if inner.width() < 10 || inner.height() == 0 {
            return;
        }

        // Pulses the selection marker's brightness on a slow sine, so the
        // list keeps a flicker of motion even when nothing has been pressed
        // -- the round-3 rule that something must animate on its own.
        let pulse = (self.time * 1.6).sin().mul_add(0.5, 0.5);
        let marker_color = tilekit::palette::mix(
            SKILL_COLOR,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            pulse * 0.4,
        );

        // Copied out of `self` before the loop, which registers hotspots on
        // `self.hotspots` as it goes: `Character` and `Opt` are plain `Copy`
        // data, so cloning the option list here is what lets the loop below
        // hold a mutable borrow of `self.hotspots` without also holding an
        // immutable borrow of `self.nodes`/`self.characters` across it.
        let ch = self.characters[self.selected];
        let options: Vec<Opt> = self.nodes[self.node].options.clone();

        let mut y = inner.top();
        let mut shown = 0usize;
        for (idx, o) in options.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            let passes = ch.passes(&o.req);
            if !passes && !self.reveal_hidden {
                continue;
            }

            let mut spans_owned: Vec<(String, Color)> = Vec::new();
            let number = if passes {
                shown += 1;
                format!("{shown}. ")
            } else {
                "  x ".to_string()
            };
            spans_owned.push((number, if passes { marker_color } else { DIM }));
            for seg in req_segments(&o.req) {
                spans_owned.push(seg);
            }
            let tail_color = if !passes {
                HIDDEN_COLOR
            } else if o.tail.starts_with('"') {
                SPEECH_COLOR
            } else {
                FG
            };
            spans_owned.push((format!("{}{}", o.sep, o.tail), tail_color));

            let spans: Vec<Span<'_>> = spans_owned
                .iter()
                .map(|(text, color)| Span::new(text, *color))
                .collect();
            panel::spans(
                surface,
                (inner.left(), y),
                inner.width(),
                &spans,
                panel::PANEL_BG,
            );

            if passes {
                let slot = Rect::new(inner.left(), y, inner.width(), 1);
                self.hotspots.push_tappable(slot, area, Hit::Option(idx));
            }
            y += 1;
        }

        if shown == 0 {
            panel::spans(
                surface,
                (inner.left(), y.min(inner.bottom().saturating_sub(1))),
                inner.width(),
                &[Span::dim("(no option this build can take)")],
                panel::PANEL_BG,
            );
        }
    }
}

impl Demo for SpokenOdds {
    const NAME: &'static str = "69_spoken_odds";
    const TITLE: &'static str = "69 Spoken Odds";
    const BLURB: &'static str =
        "The Age of Decadence: a dialogue list whose length depends on your build.";

    fn keys() -> &'static [(&'static str, &'static str)] {
        &[
            ("1-3/Tab", "switch character"),
            ("number", "pick option"),
            ("H", "reveal misses"),
            ("R", "back to the gate"),
        ]
    }

    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool {
        self.time += frame.delta.as_secs_f32();
        self.fps.record(frame.delta);
        if !self.handle_events(term) {
            return false;
        }
        self.handle_tap();
        self.draw(term);
        true
    }
}

ascii_tile_demos::demo_main!(SpokenOdds);
