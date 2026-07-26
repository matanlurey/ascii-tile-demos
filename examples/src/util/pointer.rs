//! Pointer-event decoding for the WASM FFI.
//!
//! The browser side of a `wasm-terminal` demo has no way to construct a
//! [`MouseEvent`] directly (wasm-bindgen can't pass Rust enums), so JS sends
//! three primitives and this module reassembles them.

use retroglyph_core::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use retroglyph_core::{KeyModifiers, Pos};

/// Left button pressed at `(x, y)`.
pub const KIND_DOWN: u8 = 0;
/// Left button released at `(x, y)`.
pub const KIND_UP: u8 = 1;
/// Pointer moved to `(x, y)` with no button held.
pub const KIND_MOVE: u8 = 2;
/// Wheel scrolled up (or a two-finger swipe down, which browsers report the
/// same way).
pub const KIND_SCROLL_UP: u8 = 3;
/// Wheel scrolled down.
pub const KIND_SCROLL_DOWN: u8 = 4;
/// Pointer moved to `(x, y)` with the left button held.
///
/// Browsers report a drag as a `pointermove` with a nonzero `buttons` mask;
/// the JS glue splits that into its own kind rather than making Rust track
/// button state across events.
pub const KIND_DRAG: u8 = 5;

/// Decodes a `(x, y, kind)` triple from JS into a [`MouseEvent`].
///
/// Returns `None` for an unrecognized `kind`, so an older HTML template
/// paired with a newer binary degrades to dropping the event rather than
/// panicking across the FFI boundary.
#[must_use]
pub const fn decode_mouse(x: u16, y: u16, kind: u8) -> Option<Event> {
    let kind = match kind {
        KIND_DOWN => MouseEventKind::Down(MouseButton::Left),
        KIND_UP => MouseEventKind::Up(MouseButton::Left),
        KIND_MOVE => MouseEventKind::Moved,
        KIND_SCROLL_UP => MouseEventKind::ScrollUp,
        KIND_SCROLL_DOWN => MouseEventKind::ScrollDown,
        KIND_DRAG => MouseEventKind::Drag(MouseButton::Left),
        _ => return None,
    };
    Some(Event::Mouse(MouseEvent {
        kind,
        position: Pos::new(x, y),
        pixel_position: None,
        modifiers: KeyModifiers::NONE,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        KIND_DOWN, KIND_DRAG, KIND_MOVE, KIND_SCROLL_DOWN, KIND_SCROLL_UP, KIND_UP, decode_mouse,
    };
    use retroglyph_core::event::{Event, MouseButton, MouseEventKind};

    fn kind_of(kind: u8) -> Option<MouseEventKind> {
        match decode_mouse(3, 4, kind) {
            Some(Event::Mouse(m)) => {
                assert_eq!(m.position.x, 3);
                assert_eq!(m.position.y, 4);
                Some(m.kind)
            }
            _ => None,
        }
    }

    #[test]
    fn decodes_every_documented_kind() {
        assert_eq!(
            kind_of(KIND_DOWN),
            Some(MouseEventKind::Down(MouseButton::Left))
        );
        assert_eq!(
            kind_of(KIND_UP),
            Some(MouseEventKind::Up(MouseButton::Left))
        );
        assert_eq!(kind_of(KIND_MOVE), Some(MouseEventKind::Moved));
        assert_eq!(kind_of(KIND_SCROLL_UP), Some(MouseEventKind::ScrollUp));
        assert_eq!(kind_of(KIND_SCROLL_DOWN), Some(MouseEventKind::ScrollDown));
        assert_eq!(
            kind_of(KIND_DRAG),
            Some(MouseEventKind::Drag(MouseButton::Left))
        );
    }

    #[test]
    fn unknown_kind_is_dropped_not_panicked() {
        assert!(decode_mouse(0, 0, 200).is_none());
    }
}
