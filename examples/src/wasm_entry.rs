//! `wasm_entry!`: the one bit of per-demo codegen `launch::<D>()` can't replace.
//!
//! `wasm-bindgen` needs concrete, statically-named exported functions; it
//! cannot attach to a function still generic over `D: Demo`, because there is
//! no such thing as a generic FFI symbol. Every other backend is a plain call
//! to [`launch::<D>()`](crate::launch), but the WASM entry points genuinely
//! have to be generated per demo, once `D` is a concrete type. This is
//! deliberately the only `macro_rules!` codegen in the crate.

/// Emits `fn main()` (calling [`launch::<$D>()`](crate::launch)) and the
/// [`wasm_entry!`] FFI surface in one call. The usual way to close out a demo:
///
/// ```ignore
/// ascii_tile_demos::demo_main!(MyDemo);
/// ```
///
/// Write the two out separately if a demo needs a non-default `main` body; it
/// still has to call `wasm_entry!($D)` itself afterwards, since the windowed
/// branch's generated shim calls the demo's own `main`.
#[macro_export]
macro_rules! demo_main {
    ($D:ty) => {
        fn main() {
            $crate::launch::<$D>();
        }
        $crate::wasm_entry!($D);
    };
}

/// Emits the `wasm-bindgen` FFI surface for `$D: Demo` on `wasm32`.
///
/// Expands to nothing off `wasm32`, and to nothing for a feature combination
/// it doesn't recognize (a demo built with no wasm-capable feature just
/// doesn't get an FFI surface, which is correct since nothing would call it).
///
/// - `software` or `gl`: a `#[wasm_bindgen(start)]` shim that calls the demo's
///   own `main()`. Both windowed backends are already portable to `wasm32` via
///   winit (canvas for software, WebGL2 for gl); they just need something to
///   invoke them when the module loads.
/// - `wasm-terminal` (if neither windowed backend is on): drives a
///   `Terminal<TerminalWasm>` from a browser terminal emulator such as
///   xterm.js, pushed in from JS a frame at a time.
#[macro_export]
macro_rules! wasm_entry {
    ($D:ty) => {
        // Both windowed backends run their winit event loop from `main()`;
        // this shim is the module-load hook that invokes it.
        #[cfg(all(any(feature = "software", feature = "gl"), target_arch = "wasm32"))]
        #[allow(missing_docs)]
        #[::wasm_bindgen::prelude::wasm_bindgen(start)]
        pub fn __atd_wasm_start() -> ::std::result::Result<(), ::wasm_bindgen::JsValue> {
            main();
            ::std::result::Result::Ok(())
        }

        $crate::__wasm_terminal_entry!($D);
    };
}

/// The `wasm-terminal` arm of [`wasm_entry!`]. Broken out only so that macro
/// stays a short, readable dispatch table; not meant to be called directly.
#[doc(hidden)]
#[macro_export]
macro_rules! __wasm_terminal_entry {
    ($D:ty) => {
        #[cfg(all(
            feature = "wasm-terminal",
            not(any(feature = "software", feature = "gl")),
            target_arch = "wasm32"
        ))]
        const _: () = {
            struct State {
                term: ::retroglyph_core::Terminal<::retroglyph_terminal_wasm::TerminalWasm>,
                demo: $D,
                last_tick: ::web_time::Instant,
                frame_count: u64,
            }

            ::std::thread_local! {
                static STATE: ::std::cell::RefCell<::std::option::Option<State>> =
                    ::std::cell::RefCell::new(::std::option::Option::None);
            }

            /// Build the `Terminal<TerminalWasm>` at the given size in cells
            /// and run `$D::init` once. Call after sizing the host terminal
            /// emulator (e.g. xterm.js's `fitAddon.fit()`), before the first
            /// tick.
            #[::wasm_bindgen::prelude::wasm_bindgen]
            #[allow(missing_docs)]
            pub fn wasm_terminal_demo_init(width: u16, height: u16) {
                ::console_error_panic_hook::set_once();
                let mut backend = ::retroglyph_terminal_wasm::TerminalWasm::new(width, height);
                ::retroglyph_core::backend::Cursor::set_cursor_visible(&mut backend, false);
                let mut term = ::retroglyph_core::Terminal::new(backend);
                let demo = <$D as $crate::Demo>::init(&mut term);
                STATE.with(|cell| {
                    *cell.borrow_mut() = ::std::option::Option::Some(State {
                        term,
                        demo,
                        last_tick: ::web_time::Instant::now(),
                        frame_count: 0,
                    });
                });
            }

            /// Report a new size in cells, e.g. after the host terminal
            /// emulator re-fits on a window resize.
            #[::wasm_bindgen::prelude::wasm_bindgen]
            #[allow(missing_docs)]
            pub fn wasm_terminal_demo_resize(width: u16, height: u16) {
                STATE.with(|cell| {
                    if let ::std::option::Option::Some(s) = cell.borrow_mut().as_mut() {
                        ::retroglyph_core::backend::Output::resize(
                            s.term.backend_mut(),
                            ::retroglyph_core::Size { width, height },
                        );
                        s.term.resize(width, height);
                    }
                });
            }

            /// Decode and queue a key event. `code` is a Unicode codepoint for
            /// printable keys, or `0x11_0000 + n` for a named key; `mods` is
            /// the shift/control/alt bitmask. See
            /// `retroglyph_terminal_wasm::decode_key_event`.
            #[::wasm_bindgen::prelude::wasm_bindgen]
            #[allow(missing_docs)]
            pub fn wasm_terminal_demo_push_key(code: u32, mods: u8) {
                let Some(event) = ::retroglyph_terminal_wasm::decode_key_event(code, mods) else {
                    return;
                };
                STATE.with(|cell| {
                    if let ::std::option::Option::Some(s) = cell.borrow_mut().as_mut() {
                        ::retroglyph_core::Input::push_event(
                            s.term.backend_mut(),
                            ::retroglyph_core::event::Event::Key(event),
                        );
                    }
                });
            }

            /// Decode and queue a pointer event. See
            /// [`util::pointer::decode_mouse`](crate::util::pointer::decode_mouse)
            /// for the `(x, y, kind)` encoding.
            #[::wasm_bindgen::prelude::wasm_bindgen]
            #[allow(missing_docs)]
            pub fn wasm_terminal_demo_push_mouse(x: u16, y: u16, kind: u8) {
                let Some(event) = $crate::util::pointer::decode_mouse(x, y, kind) else {
                    return;
                };
                STATE.with(|cell| {
                    if let ::std::option::Option::Some(s) = cell.borrow_mut().as_mut() {
                        ::retroglyph_core::Input::push_event(s.term.backend_mut(), event);
                    }
                });
            }

            /// Run one tick and return the ANSI bytes rendered since the last
            /// call, ready to hand to `term.write(...)`. Empty string if
            /// called before `wasm_terminal_demo_init`.
            #[::wasm_bindgen::prelude::wasm_bindgen]
            #[allow(missing_docs)]
            pub fn wasm_terminal_demo_tick() -> ::std::string::String {
                STATE.with(|cell| {
                    let mut guard = cell.borrow_mut();
                    let Some(s) = guard.as_mut() else {
                        return ::std::string::String::new();
                    };
                    let now = ::web_time::Instant::now();
                    let frame = ::retroglyph_core::Frame {
                        delta: now.duration_since(s.last_tick),
                        frame: s.frame_count,
                    };
                    s.last_tick = now;
                    s.frame_count = s.frame_count.wrapping_add(1);
                    $crate::Demo::tick(&mut s.demo, &mut s.term, &frame);
                    // JS drives this loop, so there is no `retroglyph` driver
                    // to present after `tick` returns; without this the
                    // backend has no output to hand back.
                    let _ = ::retroglyph_core::Terminal::present(&mut s.term);
                    s.term.backend_mut().take_output()
                })
            }

            // Required symbol for the wasm32 binary target; JS never calls it
            // (there is no event loop to kick off -- everything is pushed in).
            fn main() {}
        };
    };
}
