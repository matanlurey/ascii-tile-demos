//! The [`Demo`] trait and `launch::<D>()` backend dispatch.
//!
//! `launch::<D>()` picks a backend from the crate's enabled Cargo features
//! (`software` > `gl` > `crossterm` > headless-stdout fallback) and drives a
//! [`Demo`] on it. Nothing here is generated per demo -- every demo calls the
//! exact same `launch::<Self>()`.
//!
//! The one thing that genuinely needs per-demo codegen is the `wasm-bindgen`
//! FFI surface, because `wasm-bindgen` cannot attach to a function that is
//! still generic over `D: Demo`. See [`wasm_entry!`](crate::wasm_entry).

use std::time::Duration;

#[cfg(any(feature = "crossterm", feature = "software", feature = "gl"))]
use retroglyph_core::{App, Flow};
use retroglyph_core::{Backend, Frame, Terminal};

// Only the windowed backends size their own grid; crossterm takes the real
// terminal's size and headless uses its own smaller test grid.
#[cfg(any(feature = "software", feature = "gl"))]
use crate::{GRID_COLS, GRID_ROWS};

/// A runnable demo: `init` builds the state once, `tick` advances and draws
/// one frame.
///
/// Implement this once, generic over the backend, and call
/// `ascii_tile_demos::launch::<Self>()` from `main` (or just use
/// [`demo_main!`](crate::demo_main)). The same implementation runs on every
/// backend the crate is built with.
pub trait Demo: Default + Sized + 'static {
    /// Filename-safe identifier. Must match the demo's `.rs` file stem, since
    /// the WASM build scripts derive URLs from the filename.
    const NAME: &'static str;

    /// Human-readable title, used as the window title and in the gallery.
    const TITLE: &'static str;

    /// One sentence naming the technique on show. Appears in the gallery index
    /// and in `--list` output, so keep it under about 90 characters.
    const BLURB: &'static str;

    /// Key bindings, as `(keys, what it does)` pairs, rendered into the
    /// demo's own help footer and the gallery page.
    ///
    /// Default: just the universal quit binding. Override to document
    /// demo-specific controls; the universal ones are appended automatically
    /// by [`ui::help_line`](crate::ui::help_line), so list only what's yours.
    fn keys() -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Build the initial state, given the first live `Terminal<B>`.
    ///
    /// Called once, before the first `tick`, against the backend that is
    /// actually running -- so this is the hook for anything that depends on
    /// the real starting grid size, which varies by backend (crossterm: the
    /// real terminal; windowed: [`GRID_COLS`]x[`GRID_ROWS`]; headless: a
    /// fixed test grid). Centering a camera is the usual reason to override.
    ///
    /// `Demo` requires `Default` so this default body works: `init` is called
    /// generically as `D::init(term)` from shared driver code that only knows
    /// `D: Demo`, and a default method can't add its own extra bound.
    fn init<B: Backend>(_term: &mut Terminal<B>) -> Self {
        Self::default()
    }

    /// Customize the software backend's builder before it is built.
    ///
    /// Default: the standard [`GRID_COLS`]x[`GRID_ROWS`] grid at scale 1.
    /// Override for a demo that needs a different grid, scale, font, or a
    /// tileset -- see `16_tileset_sprites.rs` for a real override.
    #[cfg(feature = "software")]
    fn configure_software(
        builder: retroglyph_software::SoftwareBackendBuilder,
    ) -> retroglyph_software::SoftwareBackendBuilder {
        builder
    }

    /// Customize the GL backend's builder before it is built.
    ///
    /// The GPU counterpart of [`configure_software`](Self::configure_software).
    /// Separate rather than one shared hook because the two builders are
    /// unrelated types with different capabilities, and a demo that registers
    /// a tileset usually wants it on both.
    #[cfg(feature = "gl")]
    fn configure_gl(builder: retroglyph_gl::GlBackendBuilder) -> retroglyph_gl::GlBackendBuilder {
        builder
    }

    /// Whether the windowed backends should fill the browser viewport on
    /// `wasm32` instead of rendering at their natural grid size.
    ///
    /// Default: `true`. Unlike a fixed-size widget demo, every demo here is a
    /// pannable map that benefits from every cell the viewport can offer, and
    /// the gallery pages are full-bleed. Has no effect on native.
    #[cfg(any(feature = "software", feature = "gl"))]
    fn fill_viewport() -> bool {
        true
    }

    /// Advance and render one frame. Return `false` to quit.
    ///
    /// `frame.delta` is real wall-clock time since the previous tick, measured
    /// by whichever driver is running. Anything that animates should scale by
    /// it rather than counting raw ticks: the crossterm driver is an
    /// unthrottled spin loop, while the windowed drivers are vsync-paced, so
    /// per-tick animation runs at wildly different speeds across backends.
    ///
    /// Responsible for calling [`Terminal::present`].
    fn tick<B: Backend>(&mut self, term: &mut Terminal<B>, frame: &Frame) -> bool;
}

/// Adapts a [`Demo`] into an [`App`], creating the state lazily on the first
/// frame so the same adapter works for both the blocking (crossterm) driver
/// and the inverted (winit) driver.
/// Applies any pending [`Event::Resize`] to the terminal's grid, leaving every
/// event in the queue for the demo to see.
///
/// The windowed drivers report a resize by resizing the backend's *surface*
/// and pushing an `Event::Resize`; neither calls
/// [`Terminal::resize`](retroglyph_core::Terminal::resize), and the backend's
/// own `size()` keeps reporting the configured grid, so nothing about the
/// terminal changes until the app acts on that event. On the browser build,
/// where the window opens at the configured grid size and then grows to fill
/// the viewport, the symptom is the map rendering at
/// [`GRID_COLS`]x[`GRID_ROWS`] in the corner of a much larger canvas with a
/// black band down two sides.
///
/// Handling it here rather than in each demo means no demo can forget, and no
/// demo has to carry an event arm for something it otherwise has no interest
/// in. The drain-and-requeue is what makes that possible: the resize arrives
/// through the same queue a demo drains for input, so simply reading it would
/// consume the notification and break every demo's mouse handling instead.
#[cfg(any(feature = "crossterm", feature = "software", feature = "gl"))]
fn sync_size<B: Backend>(term: &mut Terminal<B>) {
    use retroglyph_core::event::Event;

    let pending: Vec<Event> = term.drain_events().collect();
    if pending.is_empty() {
        return;
    }

    // Only the last resize matters; a drag across the screen delivers dozens,
    // and resizing the grid for each one would reallocate both buffers every
    // frame for no visible benefit.
    let resize = pending.iter().rev().find_map(|event| match *event {
        Event::Resize(width, height) if width > 0 && height > 0 => Some((width, height)),
        _ => None,
    });
    if let Some((width, height)) = resize {
        let current = term.size();
        if current.width != width || current.height != height {
            term.resize(width, height);
        }
    }

    for event in pending {
        term.backend_mut().push_event(event);
    }
}

// The event-loop proxy, once the driver hands it over. A plain comment rather
// than a doc comment because `thread_local!` does not forward one.
//
// A thread-local rather than a field on `DemoApp` because it is genuinely
// process-global: `retroglyph-window` supports one `WindowBackend` per
// process, `run_app_with_proxy` creates the event loop *after* taking
// ownership of the app, and threading an `Rc` through the constructor forces
// every backend's `DemoApp::new` to have a different signature for a value
// only one target ever reads.
#[cfg(all(target_arch = "wasm32", any(feature = "software", feature = "gl")))]
thread_local! {
    static FRAME_PUMP: std::cell::RefCell<Option<retroglyph_window::winit::EventProxy>> =
        const { std::cell::RefCell::new(None) };
}

/// Records the proxy the driver just created. Called from `on_proxy`, which
/// runs synchronously before the event loop starts.
#[cfg(any(feature = "software", feature = "gl"))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    expect(
        unused_variables,
        clippy::needless_pass_by_value,
        reason = "the body is wasm-only, so native sees an empty function"
    )
)]
fn install_frame_pump(proxy: retroglyph_window::winit::EventProxy) {
    #[cfg(target_arch = "wasm32")]
    FRAME_PUMP.with(|slot| *slot.borrow_mut() = Some(proxy));
}

/// Keeps the event loop awake on `wasm32` by injecting one event per frame.
///
/// [`TARGET_FPS`] is enough on native, but in the published
/// `retroglyph-window` 0.3.1 the entire `frame_interval` branch of
/// `about_to_wait` sits behind `#[cfg(not(target_arch = "wasm32"))]`. On wasm
/// `target_fps` is therefore not merely ignored, it is compiled out, and the
/// only surviving path is the `needs_redraw` gate. With nothing setting that
/// flag the browser build renders one frame and freezes until you move the
/// mouse.
///
/// Injecting through the proxy sets `needs_redraw` (see the driver's
/// `handle_user_event`), so `about_to_wait` requests a redraw, which the web
/// backend services on the next `requestAnimationFrame`. Doing it once per
/// frame makes that self-sustaining at display refresh.
///
/// Native is excluded deliberately: there the throttled `WaitUntil` branch
/// returns before consulting `needs_redraw`, so this would buy nothing and
/// cost a stray `Event::Custom` per frame for every demo to skip past.
///
/// Removable once a `retroglyph-window` release carries the fix. Upstream
/// already has it: retroglyph's own examples animate in the browser because
/// their unreleased tree moves the cfg to wrap only the *sleep*, leaving
/// `request_redraw()` on the wasm path. See
/// <https://github.com/crates-lurey-io/retroglyph/issues/510>.
#[cfg(any(feature = "software", feature = "gl"))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    expect(
        clippy::missing_const_for_fn,
        reason = "the body is wasm-only, so native sees an empty function"
    )
)]
fn pump_next_frame() {
    #[cfg(target_arch = "wasm32")]
    FRAME_PUMP.with(|slot| {
        if let Some(proxy) = slot.borrow().as_ref() {
            // A closed loop means the app is shutting down, which is exactly
            // when there is no next frame to ask for.
            let _ = proxy.send_event(0);
        }
    });
}

#[cfg(any(feature = "crossterm", feature = "software", feature = "gl"))]
struct DemoApp<D> {
    state: Option<D>,
}

#[cfg(any(feature = "crossterm", feature = "software", feature = "gl"))]
impl<D> DemoApp<D> {
    const fn new() -> Self {
        Self { state: None }
    }
}

#[cfg(any(feature = "crossterm", feature = "software", feature = "gl"))]
impl<B: Backend, D: Demo> App<B> for DemoApp<D> {
    fn update(&mut self, term: &mut Terminal<B>, frame: &Frame) -> Flow {
        sync_size(term);
        #[cfg(any(feature = "software", feature = "gl"))]
        pump_next_frame();
        let state = self.state.get_or_insert_with(|| D::init(term));
        if state.tick(term, frame) {
            Flow::Continue
        } else {
            Flow::Exit
        }
    }
}

// ── Software backend (native window + browser canvas) ───────────────────────

/// Frame rate the windowed backends are driven at.
///
/// Not a performance cap; it is what makes the demos animate at all on native.
/// `retroglyph-window`'s winit driver is event-driven by default: with
/// `target_fps: None` it leaves `ControlFlow::Wait` set and only requests a
/// redraw when something happened (input, resize, an injected event), on the
/// reasonable assumption that a retro/terminal-style app is idle most of the
/// time and should not spin at 100% CPU re-rendering an unchanged frame.
///
/// That assumption is exactly wrong for this gallery. Every demo here animates
/// on its own, and with no target set the map only advances while you wiggle
/// the mouse. Passing one switches the driver to its `WaitUntil` path, which
/// redraws unconditionally once each deadline passes.
///
/// No effect on `wasm32`, where the driver is always
/// `requestAnimationFrame`-driven, or on the terminal backend, whose blocking
/// driver is already an unthrottled loop. 60 rather than uncapped so an idle
/// demo costs a predictable slice of one core instead of whatever the GPU will
/// give it.
#[cfg(any(feature = "software", feature = "gl"))]
const TARGET_FPS: Option<u32> = Some(60);

/// The supplementary block-glyph sheet, and the characters it supplies.
///
/// `retroglyph`'s embedded bitmap font is CP437, and its character lookup is
/// CP437 by construction: both `BitmapFont::try_char_to_index` and every font
/// in a `FallbackFontChain` route each `char` through `unicode_to_cp437`. A
/// fallback font can therefore only fill gaps *within* CP437; it cannot add
/// characters CP437 never named. Anything outside it resolves to the
/// solid-block fallback glyph.
///
/// CP437 has the shade ramp and the four half blocks, so
/// [`HalfBlockCanvas`](tilekit::glyphs::HalfBlockCanvas) works everywhere. It
/// does not have the other ten quadrants, any sextant, or any braille
/// pattern, so a braille canvas renders as a rectangle of solid color on the
/// pixel backends: not subtly off, completely wrong.
///
/// A tileset does override the font for the glyphs its codepage names, so this
/// registers one covering exactly the missing characters, generated by
/// `cargo run -p gen-tileset`. It is registered for *every* demo rather than
/// opted into per demo, because "this character renders" is not a
/// demo-specific concern, and the cost is one 7 KB PNG decoded once at
/// startup.
#[cfg(any(feature = "software", feature = "gl"))]
pub fn block_tileset() -> retroglyph_window::tileset::TilesetOptions {
    use retroglyph_window::tileset::{Codepage, TilesetOptions};

    // Both files come from the same generator run, so the sheet's sprite order
    // and this codepage cannot disagree. `include_str!` rather than a parallel
    // `const` list for the same reason: one source of truth, checked at build
    // time by the file simply existing.
    let png = include_bytes!("../assets/blocks.png").to_vec();
    let chars: Vec<char> = include_str!("../assets/blocks.codepage.txt")
        .chars()
        .collect();

    TilesetOptions::from_bytes(png)
        .tile_size(8, 16)
        .codepage(Codepage::Custom(chars))
        .build()
        .expect("the generated block tileset must decode")
}

/// Runs `D` on the software (winit + softbuffer / `Canvas2D`) backend.
///
/// # Panics
///
/// Panics if the software backend fails to initialize, or if the event loop
/// fails to start.
#[cfg(feature = "software")]
pub fn run_software<D: Demo>() {
    run_software_with::<D>(D::configure_software(
        retroglyph_software::SoftwareBackendBuilder::new()
            .grid_size(GRID_COLS, GRID_ROWS)
            .scale(1)
            .tileset(block_tileset()),
    ));
}

/// Runs `D` on the software backend with a caller-supplied builder.
///
/// The lower-level building block [`run_software`] delegates to via
/// [`Demo::configure_software`], exposed for a hand-written `main` that needs
/// something the builder-in/builder-out shape can't express.
///
/// # Panics
///
/// Panics if the software backend fails to initialize, or if the event loop
/// fails to start.
#[cfg(feature = "software")]
pub fn run_software_with<D: Demo>(builder: retroglyph_software::SoftwareBackendBuilder) {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    let renderer = builder
        .build()
        .expect("failed to initialize software backend")
        .run_headless()
        .expect("failed to build headless renderer");
    let config = retroglyph_window::winit::WindowConfig::fit(&renderer, D::TITLE, TARGET_FPS)
        .fill_viewport(D::fill_viewport());
    retroglyph_window::winit::run_app_with_proxy(
        config,
        renderer,
        DemoApp::<D>::new(),
        install_frame_pump,
    )
    .expect("event loop failed");
}

// ── GL backend (native OpenGL 3.3 + browser WebGL2) ─────────────────────────

/// Runs `D` on the GPU backend.
///
/// # Panics
///
/// Panics if the GL backend fails to initialize, or if the event loop fails to
/// start.
#[cfg(feature = "gl")]
pub fn run_gl<D: Demo>() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    let renderer = D::configure_gl(
        retroglyph_gl::GlBackendBuilder::new()
            .grid_size(GRID_COLS, GRID_ROWS)
            .scale(1)
            .tileset(block_tileset()),
    )
    .build()
    .expect("failed to initialize gl backend");
    let config = retroglyph_window::winit::WindowConfig::fit(&renderer, D::TITLE, TARGET_FPS)
        .fill_viewport(D::fill_viewport());
    retroglyph_window::winit::run_app_with_proxy(
        config,
        renderer,
        DemoApp::<D>::new(),
        install_frame_pump,
    )
    .expect("event loop failed");
}

// ── Crossterm backend (real TTY) ────────────────────────────────────────────

/// Runs `D` on the crossterm backend, blocking until it quits.
///
/// # Errors
///
/// Returns an error if the terminal fails to initialize.
#[cfg(feature = "crossterm")]
pub fn run_crossterm<D: Demo>() -> std::io::Result<()> {
    retroglyph_crossterm::Crossterm::run(DemoApp::<D>::new())
}

// ── Headless (stdout) fallback ──────────────────────────────────────────────

/// The synthetic per-call [`Frame::delta`] fed to [`Demo::tick`] in headless
/// runs.
///
/// No real clock is involved (headless never runs against a live backend), so
/// this is a fixed "one call is worth this much simulated time" stand-in.
/// 100ms means a demo animating at a visible pace advances one perceptible
/// step per headless frame, which keeps snapshots readable.
pub const HEADLESS_FRAME_DELTA: Duration = Duration::from_millis(100);

/// Grid size used by headless runs and snapshot tests.
///
/// Deliberately smaller than [`GRID_COLS`]x[`GRID_ROWS`]: snapshot files are
/// read by humans in diffs, and a 100x40 grid of box-drawing characters is
/// not. Every demo lays out responsively, so this exercises the narrow path
/// as a side benefit.
pub const HEADLESS_COLS: u16 = 80;
/// See [`HEADLESS_COLS`].
pub const HEADLESS_ROWS: u16 = 24;

/// Renders up to `frames` frames of `D` against a fresh
/// [`Headless`](retroglyph_core::Headless) backend and returns each frame's
/// [`format_view`](retroglyph_core::Headless::format_view) text.
///
/// No terminal or window is involved and no input is injected, so `tick` only
/// ever sees an empty event queue. Shared by [`run_headless_stdout`] and the
/// crate's snapshot tests, so both go through the exact same path.
#[must_use]
pub fn render_headless_frames<D: Demo>(frames: u32) -> Vec<String> {
    let backend = retroglyph_core::Headless::new(HEADLESS_COLS, HEADLESS_ROWS);
    let mut term = Terminal::new(backend);
    let mut state = D::init(&mut term);

    let mut views = Vec::with_capacity(frames as usize);
    for i in 0..frames {
        let frame = Frame {
            delta: HEADLESS_FRAME_DELTA,
            frame: u64::from(i),
        };
        if !state.tick(&mut term, &frame) {
            break;
        }
        views.push(term.backend().format_view());
    }
    views
}

/// Fallback `main` body when no backend feature is enabled: ticks a few frames
/// against a headless backend and prints each to stdout.
///
/// Keeps every demo `cargo run`-able with the crate's default feature set, and
/// gives the gallery a uniform "no backend? still works" story. Frame count
/// defaults to 3, overridable with `ATD_HEADLESS_FRAMES`.
pub fn run_headless_stdout<D: Demo>() {
    let frames: u32 = std::env::var("ATD_HEADLESS_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(3);

    println!("{} -- {}", D::TITLE, D::BLURB);
    for (i, view) in render_headless_frames::<D>(frames).into_iter().enumerate() {
        println!("--- frame {} ---", i + 1);
        println!("{view}");
    }
}

/// Environment variable that makes any demo print its catalog metadata and
/// exit instead of running.
///
/// The gallery's index page needs each demo's title, blurb, and key bindings.
/// Those already exist as associated constants on [`Demo`], but a `[[example]]`
/// target is a separate binary with no way to expose a constant to a build
/// script. Rather than maintain a parallel catalog file that silently drifts
/// out of sync, the build script runs each demo once with this set and reads
/// the answer from the binary that actually defines it.
pub const META_ENV: &str = "ATD_PRINT_META";

/// Prints `D`'s catalog metadata as one tab-separated record and returns
/// `true`, if [`META_ENV`] is set. Otherwise does nothing and returns `false`.
///
/// Tab-separated rather than JSON so the build script can parse it with `cut`
/// and needs no dependencies; none of the fields may contain a tab, which is
/// enforced by a test rather than by hope.
#[must_use]
pub fn print_meta_if_requested<D: Demo>() -> bool {
    if std::env::var_os(META_ENV).is_none() {
        return false;
    }
    let keys = D::keys()
        .iter()
        .map(|(k, what)| format!("{k}: {what}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{}\t{}\t{}\t{}", D::NAME, D::TITLE, D::BLURB, keys);
    true
}

// ── Backend dispatch ────────────────────────────────────────────────────────
//
// Mutually exclusive by construction: at most one `launch` is compiled in for
// any feature set. `wasm-terminal` on a non-wasm32 host falls through to the
// headless arm, so every feature combination stays host-checkable.
//
// Every arm checks `print_meta_if_requested` first, so the gallery build can
// harvest metadata from a demo built for any backend, not just the headless
// one. The check is a single env lookup on a path that runs once per process.

/// Picks a backend from the crate's enabled Cargo features and runs `D` on it.
/// Call this (and nothing else) from every demo's `main`.
#[cfg(feature = "software")]
pub fn launch<D: Demo>() {
    if print_meta_if_requested::<D>() {
        return;
    }
    run_software::<D>();
}

/// See [`launch`]'s software overload. `software` wins if both are enabled.
#[cfg(all(feature = "gl", not(feature = "software")))]
pub fn launch<D: Demo>() {
    if print_meta_if_requested::<D>() {
        return;
    }
    run_gl::<D>();
}

/// See [`launch`]'s software overload.
#[cfg(all(feature = "crossterm", not(any(feature = "software", feature = "gl"))))]
pub fn launch<D: Demo>() {
    if print_meta_if_requested::<D>() {
        return;
    }
    run_crossterm::<D>().expect("crossterm backend failed");
}

/// No-op on `wasm32`: the real entry points for this backend are the
/// `#[wasm_bindgen]` functions generated by [`wasm_entry!`](crate::wasm_entry),
/// which JS calls directly instead of going through `main`.
#[cfg(all(
    feature = "wasm-terminal",
    not(any(feature = "software", feature = "gl", feature = "crossterm")),
    target_arch = "wasm32"
))]
pub fn launch<D: Demo>() {
    let _ = core::marker::PhantomData::<D>;
}

/// Fallback: no backend feature enabled (or `wasm-terminal` enabled but not
/// building for `wasm32`).
#[cfg(not(any(
    feature = "crossterm",
    feature = "software",
    feature = "gl",
    all(feature = "wasm-terminal", target_arch = "wasm32"),
)))]
pub fn launch<D: Demo>() {
    if print_meta_if_requested::<D>() {
        return;
    }
    run_headless_stdout::<D>();
}
