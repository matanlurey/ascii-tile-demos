//! Shared chrome every demo draws: a title bar, a status/help line, and the
//! common key handling behind them.
//!
//! Keeping this in one place means the gallery reads as one coherent thing
//! rather than sixteen slightly different UIs, and it keeps each demo file
//! focused on the technique it's actually demonstrating.

pub mod card;
pub mod panel;
pub mod touch;

use retroglyph_core::event::{Event, KeyCode};
use retroglyph_core::{Color, Rect, Style, Surface};
use retroglyph_widgets::truncate;

use crate::Demo;
use crate::util::perf::FpsMeter;

/// Page background: near-black with a hint of blue, so a pure-black tile
/// still reads as a distinct thing drawn on top of it.
pub const BG: Color = Color::Rgb { r: 9, g: 10, b: 15 };
/// Chrome background for the title and status bars.
pub const CHROME_BG: Color = Color::Rgb {
    r: 20,
    g: 22,
    b: 32,
};
/// Primary chrome text.
pub const FG: Color = Color::Rgb {
    r: 214,
    g: 212,
    b: 226,
};
/// Secondary chrome text: key names, hints, units.
pub const DIM: Color = Color::Rgb {
    r: 124,
    g: 122,
    b: 148,
};
/// The single accent color, used for the demo title and for whatever each
/// demo considers "selected".
pub const ACCENT: Color = Color::Rgb {
    r: 246,
    g: 196,
    b: 96,
};

/// Bindings every demo honors, appended to each demo's own [`Demo::keys`] in
/// the help line so no demo has to restate them.
pub const UNIVERSAL_KEYS: &[(&str, &str)] = &[("Q/Esc", "quit")];

/// Rows of chrome: one title bar at the top, one status line at the bottom.
pub const CHROME_ROWS: u16 = 2;

/// Splits the screen into `(title_bar, content, status_bar)`.
///
/// Degenerate on a very short terminal: below 3 rows the content area wins and
/// the bars collapse to zero height, so a demo drawn into a 1-row window still
/// shows its map rather than only chrome.
#[must_use]
pub fn split_chrome(screen: Rect) -> (Rect, Rect, Rect) {
    if screen.height() < 3 {
        return (
            Rect::new(screen.left(), screen.top(), screen.width(), 0),
            screen,
            Rect::new(screen.left(), screen.bottom(), screen.width(), 0),
        );
    }
    let title = Rect::new(screen.left(), screen.top(), screen.width(), 1);
    let status = Rect::new(screen.left(), screen.bottom() - 1, screen.width(), 1);
    let content = Rect::new(
        screen.left(),
        screen.top() + 1,
        screen.width(),
        screen.height() - CHROME_ROWS,
    );
    (title, content, status)
}

/// Fills `rect` with `style`'s background and a space glyph.
///
/// Deliberately an explicit space rather than a cleared cell: a cleared cell is
/// transparent and composites whatever the layer below drew, which is exactly
/// wrong for chrome that must occlude the map behind it.
pub fn fill(surface: &mut Surface<'_>, rect: Rect, style: Style) {
    surface.fill_rect(rect, ' ', style);
}

/// Draws the top bar: demo number and title on the left, blurb on the right if
/// it fits.
pub fn title_bar<D: Demo>(surface: &mut Surface<'_>, area: Rect) {
    if area.height() == 0 {
        return;
    }
    let mut bar = surface.clip(area);
    bar.fill_rect(area, ' ', Style::new().bg(CHROME_BG));
    let w = area.width_usize();
    if w < 4 {
        return;
    }

    let title = format!(" {} ", D::TITLE);
    bar.print(
        (area.left(), area.top()),
        truncate(&title, w),
        Style::new().fg(ACCENT).bg(CHROME_BG),
    );

    // The blurb is the first thing to go on a narrow terminal: the title
    // already says which demo this is, and the map matters more than prose.
    let used = title.chars().count();
    let room = w.saturating_sub(used + 2);
    if room >= 24 {
        bar.print(
            (area.left() + used as u16 + 1, area.top()),
            truncate(D::BLURB, room),
            Style::new().fg(DIM).bg(CHROME_BG),
        );
    }
}

/// Draws the bottom bar: `left` (whatever the demo wants to say about its own
/// state) then the key hints, with the FPS readout pinned to the right edge.
pub fn status_bar<D: Demo>(surface: &mut Surface<'_>, area: Rect, left: &str, fps: &FpsMeter) {
    if area.height() == 0 {
        return;
    }
    let mut bar = surface.clip(area);
    bar.fill_rect(area, ' ', Style::new().bg(CHROME_BG));
    let w = area.width_usize();
    if w < 4 {
        return;
    }
    let y = area.top();

    // FPS first: it's fixed-width and right-anchored, so reserving it up front
    // means the variable-width left side can never overrun it.
    let fps_text = format!("{:>5.1} fps ", fps.fps());
    let fps_w = fps_text.chars().count();
    if w > fps_w {
        bar.print(
            (area.right() - fps_w as u16, y),
            &fps_text,
            Style::new().fg(DIM).bg(CHROME_BG),
        );
    }

    let room = w.saturating_sub(fps_w + 1);
    let mut x = area.left() + 1;
    let mut spent = 1usize;

    if !left.is_empty() {
        let text = truncate(left, room.saturating_sub(spent));
        bar.print((x, y), text, Style::new().fg(FG).bg(CHROME_BG));
        let n = text.chars().count();
        x += n as u16;
        spent += n;
    }

    // Key hints fill whatever is left, dropping whole bindings rather than
    // truncating one mid-word into something unreadable.
    for (keys, what) in D::keys().iter().chain(UNIVERSAL_KEYS) {
        let chunk = format!("  {keys} {what}");
        let n = chunk.chars().count();
        if spent + n > room {
            break;
        }
        bar.print(
            (x, y),
            &format!("  {keys} "),
            Style::new().fg(ACCENT).bg(CHROME_BG),
        );
        bar.print(
            (x + keys.chars().count() as u16 + 3, y),
            what,
            Style::new().fg(DIM).bg(CHROME_BG),
        );
        x += n as u16;
        spent += n;
    }
}

/// Handles the universal quit bindings, returning `false` if the demo should
/// exit.
///
/// Demos call this from their own event loop for events they don't consume, so
/// `q`/`Escape`/window-close behave identically everywhere.
#[must_use]
pub const fn is_quit(event: &Event) -> bool {
    match event {
        Event::Close => true,
        Event::Key(key) => {
            key.is_down() && matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Escape)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{CHROME_ROWS, split_chrome};
    use retroglyph_core::Rect;

    #[test]
    fn chrome_splits_into_three_stacked_bands() {
        let (title, content, status) = split_chrome(Rect::new(0, 0, 80, 24));
        assert_eq!((title.top(), title.height()), (0, 1));
        assert_eq!((content.top(), content.height()), (1, 24 - CHROME_ROWS));
        assert_eq!((status.top(), status.height()), (23, 1));
        assert_eq!(content.bottom(), status.top());
    }

    #[test]
    fn short_screens_give_every_row_to_content() {
        for h in 0..3u16 {
            let (title, content, status) = split_chrome(Rect::new(0, 0, 40, h));
            assert_eq!(title.height(), 0);
            assert_eq!(status.height(), 0);
            assert_eq!(content.height(), h, "height {h}");
        }
    }
}
