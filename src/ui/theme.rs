//! Colours used by the menu.
//!
//! Everything is written through [`paint`]. With colours off, the runs marked
//! [`Style::highlight`] — the selected row, the status line — fall back to
//! reverse video and the rest is written undecorated. Only palette entries the
//! terminal itself defines
//! (colours 0–15) are named, so the result follows the user's theme instead of
//! fighting whatever background they have. Note that crossterm writes these as
//! 256-colour sequences (`ESC [ 38;5;14 m` for [`Color::Cyan`]) rather than the
//! short `ESC [ 36 m` form; the index still points into the terminal's own
//! palette.

use std::env;
use std::ffi::OsStr;
use std::io::{Result, Write};
use std::sync::OnceLock;

use crossterm::queue;
use crossterm::style::{Attribute, Color, SetAttribute, SetBackgroundColor, SetForegroundColor};

/// Menu title.
pub const TITLE: Color = Color::Cyan;
/// Breadcrumb hint: present, but out of the way.
pub const MUTED: Color = Color::DarkGrey;
/// Help text. Out of the way like [`MUTED`], but a step brighter: help is
/// meant to be read, and DarkGrey (ANSI bright black) sits close enough to a
/// dark terminal background to be hard to make out.
pub const HELP: Color = Color::Grey;
/// Entries with a detail view — a submenu, a help text, or both — so a
/// container is distinguishable from a plain command before the cursor
/// reaches it.
pub const CONTAINER: Color = Color::Cyan;
/// The `*>` cursor marker, which only ever appears on the selected row. Yellow
/// rather than green: it is drawn on [`SELECTED_BG`], where green has too
/// little contrast.
pub const CURSOR: Color = Color::Yellow;
pub const SELECTED_FG: Color = Color::White;
pub const SELECTED_BG: Color = Color::Blue;
pub const FOOTER_FG: Color = Color::White;
pub const FOOTER_BG: Color = Color::DarkGrey;
/// The sigil at the head of the status line (`$` for a command, `>` for a
/// submenu), and the `$` of the echo before a command runs. What follows the
/// sigil is not painted with this: the status line uses [`FOOTER_FG`], the
/// echo the terminal's default colour.
pub const COMMAND: Color = Color::Green;
pub const ERROR: Color = Color::Red;

/// How one run of text is drawn.
#[derive(Default, Clone, Copy)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    /// This run marks something out from its neighbours (the selected row, the
    /// status line). See [`Style::highlight`].
    pub highlight: bool,
}

impl Style {
    pub fn fg(color: Color) -> Self {
        Self {
            fg: Some(color),
            ..Self::default()
        }
    }

    pub fn on(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Mark this run as a highlight, so it stays distinguishable when colours
    /// are off. Ignored while colours are on, where the background already
    /// does the job — reversing it as well would undo it.
    pub fn highlight(mut self) -> Self {
        self.highlight = true;
        self
    }
}

/// Write `text` with `style`.
pub fn paint(out: &mut impl Write, style: Style, text: &str) -> Result<()> {
    paint_with(enabled(), out, style, text)
}

/// [`paint`], with the colour mode passed in rather than read from the
/// environment. [`enabled`] caches its answer for the life of the process, so
/// this is the only way to exercise both modes in one test run.
pub fn paint_with(color: bool, out: &mut impl Write, style: Style, text: &str) -> Result<()> {
    if !color {
        // `NO_COLOR` asks for no *colour*; reverse video is not colour, and
        // dropping it too would leave the selected row looking exactly like
        // the rest of the list. This is what the menu drew before it had any
        // colours at all.
        if !style.highlight {
            return write!(out, "{text}");
        }
        queue!(out, SetAttribute(Attribute::Reverse))?;
        write!(out, "{text}")?;
        return queue!(out, SetAttribute(Attribute::Reset));
    }

    // An unstyled run needs no reset either. Most of a menu is unstyled rows,
    // so this keeps the frame from carrying a pair of escapes per line for
    // nothing.
    if style.fg.is_none() && style.bg.is_none() && !style.bold {
        return write!(out, "{text}");
    }

    if let Some(fg) = style.fg {
        queue!(out, SetForegroundColor(fg))?;
    }
    if let Some(bg) = style.bg {
        queue!(out, SetBackgroundColor(bg))?;
    }
    if style.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    write!(out, "{text}")?;
    // `Attribute::Reset` is SGR 0, which drops the colours as well as the
    // bold; without it both would carry into the next run and the next line.
    // (`ResetColor` writes the very same `ESC [ 0 m`, so adding it too would
    // only send the sequence twice.)
    queue!(out, SetAttribute(Attribute::Reset))
}

/// Whether to emit colour at all.
///
/// Read once: the menu redraws on every key press, and the answer cannot
/// change while it is open.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        // `var_os`, not `var`: only the length of `NO_COLOR` matters, and a
        // value that is not UTF-8 would come back as an error from `var` and
        // be taken for "not set" — the opposite of what it asks for.
        decide(
            env::var_os("NO_COLOR").as_deref(),
            env::var("TERM").ok().as_deref(),
        )
    })
}

/// Split out so the rules can be tested without touching the environment.
///
/// An empty `NO_COLOR` does *not* turn colour off: the convention is "present
/// and not an empty string, regardless of its value".
///
/// There is no TTY check here — whoever calls [`paint`] owns that. The menu is
/// only drawn once stderr is known to be a terminal, and the error path in
/// `main` tests `is_terminal()` itself.
fn decide(no_color: Option<&OsStr>, term: Option<&str>) -> bool {
    if no_color.is_some_and(|value| !value.is_empty()) {
        return false;
    }
    !matches!(term, Some("dumb") | Some(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_wins_over_everything() {
        assert!(!decide(Some(OsStr::new("1")), Some("xterm-256color")));
        assert!(!decide(Some(OsStr::new("anything")), None));
    }

    #[test]
    fn an_empty_no_color_is_not_a_request_for_no_color() {
        // The convention is "present and not an empty string".
        assert!(decide(Some(OsStr::new("")), Some("xterm-256color")));
    }

    #[test]
    #[cfg(unix)]
    fn a_no_color_that_is_not_utf8_still_counts() {
        // Reading it with `var` would turn this into an error, and an error
        // into "not set" — colour would stay on although it was asked off.
        use std::os::unix::ffi::OsStrExt;
        let value = OsStr::from_bytes(&[0xff]);
        assert!(!decide(Some(value), Some("xterm-256color")));
    }

    #[test]
    fn a_dumb_or_empty_term_gets_no_color() {
        assert!(!decide(None, Some("dumb")));
        assert!(!decide(None, Some("")));
    }

    #[test]
    fn an_ordinary_terminal_gets_color() {
        assert!(decide(None, Some("xterm-256color")));
        assert!(decide(None, Some("screen")));
        // No TERM at all: the caller has already checked for a TTY, so this is
        // a terminal that simply did not export one.
        assert!(decide(None, None));
    }

    fn painted(color: bool, style: Style) -> String {
        let mut out = Vec::new();
        paint_with(color, &mut out, style, "hello").unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn ordinary_text_is_written_untouched_when_color_is_off() {
        assert_eq!(painted(false, Style::fg(TITLE).bold()), "hello");
    }

    #[test]
    fn a_highlight_falls_back_to_reverse_video_when_color_is_off() {
        // Without this the selected row is indistinguishable from the rest.
        let written = painted(false, Style::fg(SELECTED_FG).on(SELECTED_BG).highlight());
        assert_eq!(written, "\u{1b}[7mhello\u{1b}[0m");
    }

    #[test]
    fn color_is_emitted_and_reset_when_color_is_on() {
        let written = painted(true, Style::fg(TITLE).bold());
        assert!(written.contains("hello"));
        assert!(written.starts_with('\u{1b}'), "got {written:?}");
        // Nothing may leak into the next run. The exact sequence is
        // crossterm's business, so only the reset at the end is asserted.
        assert!(written.ends_with("\u{1b}[0m"), "got {written:?}");
    }

    #[test]
    fn an_unstyled_run_carries_no_escapes_at_all() {
        assert_eq!(painted(true, Style::default()), "hello");
    }

    #[test]
    fn a_highlight_is_not_reversed_when_color_is_on() {
        // Reversing a row that already has a background would undo it.
        let written = painted(true, Style::fg(SELECTED_FG).on(SELECTED_BG).highlight());
        assert!(!written.contains("\u{1b}[7m"), "got {written:?}");
    }
}
