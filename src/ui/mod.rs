//! The interactive menu.
//!
//! The terminal is put into raw mode on the alternate screen while the menu is
//! open and fully restored before anything else happens, so the selected
//! command inherits a clean TTY (see [`crate::exec`]).

pub mod prompt;
pub mod state;
pub mod theme;

use std::collections::HashMap;
use std::io::{Write, stderr};
use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::config::{Job, Launch, MenuItem};
use prompt::LineEditor;
use state::{Frame, MenuState};
use theme::{Style, paint_with};

/// What the user picked.
pub enum Outcome {
    /// Run this, with any arguments already filled in, in the given directory.
    ///
    /// `None` for the directory means the one `jj-menu` was started from; the
    /// menu does not know it, so the caller substitutes it.
    Run(Launch, Option<PathBuf>),
    /// Leave without running anything.
    Cancelled,
}

/// Restores the terminal when the menu ends, including on a panic or an early
/// return, so a crash cannot leave the shell in raw mode. A signal that kills
/// the process outright runs no destructor; [`crate::signal`] covers that.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        // Before raw mode, so that what the signal handler restores is the
        // state the shell had. Drop cannot run when the process is killed
        // outright, and this is the only cover for that (see crate::signal).
        crate::signal::arm_terminal_restore();
        terminal::enable_raw_mode()?;
        // Armed before entering the alternate screen: if that fails, the guard
        // is dropped on the way out and raw mode is undone. Constructing it
        // afterwards would leave the shell in raw mode on such a failure.
        let guard = Self;
        execute!(stderr(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stderr(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        // The terminal is the caller's again, so the handler must stop
        // touching it -- the selected command runs next and owns it.
        crate::signal::disarm_terminal_restore();
    }
}

/// Open the menu and return what the user picked.
///
/// The UI is drawn on stderr so that stdout stays free for `--print`, which
/// lets `jj-menu --print | ...` work while the menu is on screen.
pub fn run(items: Vec<MenuItem>, title: &str) -> Result<Outcome> {
    let _guard = TerminalGuard::enter()?;
    let mut menu = MenuState::new(Frame::new(title, items));
    // Whether typing goes into the search string instead of being read as
    // shortcuts. The string itself lives on the frame, so a submenu opened
    // from a filtered list starts unfiltered and going back restores it.
    let mut searching = false;

    loop {
        draw(&mut menu, searching)?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports both press and release; only act on press.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if searching {
            match classify_search(&key) {
                Search::Quit => return Ok(Outcome::Cancelled),
                Search::Cancel => {
                    searching = false;
                    menu.clear_query();
                }
                Search::Clear => menu.set_query(String::new()),
                Search::Accept => {
                    searching = false;
                    if let Some(outcome) = select(&mut menu)? {
                        return Ok(outcome);
                    }
                }
                Search::Push(c) => {
                    let mut query = menu.query().to_string();
                    query.push(c);
                    menu.set_query(query);
                }
                Search::Pop => {
                    let mut query = menu.query().to_string();
                    if query.pop().is_some() {
                        menu.set_query(query);
                    } else {
                        // Backspacing past the start leaves the search rather
                        // than sitting there doing nothing on every press.
                        // Through clear_query, not set_query(""): the cursor
                        // may have been moved while the search was open, and
                        // leaving it is not a reason to move it back.
                        searching = false;
                        menu.clear_query();
                    }
                }
                Search::Nav(action) => apply(&mut menu, action),
                Search::None => {}
            }
            continue;
        }

        match classify(&key) {
            Action::Search => {
                // Resumes the existing string rather than clearing it, so a
                // filter can be refined after looking at what it left.
                searching = true;
            }
            Action::Quit => return Ok(Outcome::Cancelled),
            Action::Cancel => {
                if let Some(outcome) = escape(&mut menu) {
                    return Ok(outcome);
                }
            }
            // At the root there is nothing to go back to, so back is a way
            // out. Inside a search it only pops a level -- leaving the menu
            // on a keystroke that is next to the text being typed would be
            // too easy to hit by accident.
            Action::Back if menu.depth() == 1 => return Ok(Outcome::Cancelled),
            Action::Select => {
                if let Some(outcome) = select(&mut menu)? {
                    return Ok(outcome);
                }
            }
            action => apply(&mut menu, action),
        }
    }
}

/// Apply a movement to the menu. `Select`, `Quit` and `Search` are decided by
/// the caller, which is the only place that can leave the loop or change mode.
fn apply(menu: &mut MenuState, action: Action) {
    match action {
        Action::Down => menu.move_down(),
        Action::Up => menu.move_up(),
        Action::First => menu.move_first(),
        Action::Last => menu.move_last(),
        Action::Back => {
            menu.leave_detail();
        }
        Action::Forward => {
            // The detail view is where help, submenus and arguments live.
            menu.enter_detail();
        }
        Action::Quit | Action::Select | Action::Search | Action::Cancel | Action::None => {}
    }
}

/// What Esc does outside the search.
///
/// A search that has been accepted leaves its filter on, and the status row
/// says `Esc to clear`. Quitting there instead would close the menu on a key
/// the screen had just offered as "show me the rest again".
///
/// `Some` means leave the menu.
fn escape(menu: &mut MenuState) -> Option<Outcome> {
    if menu.query().is_empty() {
        return Some(Outcome::Cancelled);
    }
    menu.clear_query();
    None
}

/// Run what the cursor is on. `None` means stay in the menu.
fn select(menu: &mut MenuState) -> Result<Option<Outcome>> {
    let Some(item) = menu.selected().cloned() else {
        return Ok(None);
    };
    // A container without a command opens instead of running, so Enter never
    // silently does nothing.
    if item.launch().is_none() {
        menu.enter_detail();
        return Ok(None);
    }
    // Argument input was cancelled: stay in the menu.
    Ok(resolve(&item)?.map(|launch| Outcome::Run(launch, item.cwd.clone())))
}

/// Fill in the arguments of `item` and return what to run.
///
/// `None` means the user cancelled while entering arguments.
fn resolve(item: &MenuItem) -> Result<Option<Launch>> {
    let Some(launch) = item.launch() else {
        return Ok(None);
    };
    if item.args.is_empty() {
        return Ok(Some(launch));
    }

    let mut values: HashMap<String, String> = HashMap::new();
    for spec in &item.args {
        match ask(&prompt::label(spec), &prompt::initial_value(spec))? {
            Some(value) => {
                values.insert(spec.name.clone(), value);
            }
            None => return Ok(None),
        }
    }

    // The arguments belong to the entry, so every command of a parallel group
    // gets the same values: one prompt, however many shells it starts.
    let launch = match launch {
        Launch::Script(script) => Launch::Script(prompt::substitute(&script, &values)),
        Launch::Parallel(jobs) => Launch::Parallel(
            jobs.into_iter()
                .map(|job| Job {
                    title: job.title,
                    script: prompt::substitute(&job.script, &values),
                })
                .collect(),
        ),
    };
    Ok(Some(launch))
}

/// Ask for one value. `None` means cancelled.
fn ask(label: &str, initial: &str) -> Result<Option<String>> {
    let mut editor = LineEditor::with_text(initial);

    loop {
        draw_prompt(label, &editor)?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Ok(Some(editor.text())),
            KeyCode::Esc => return Ok(None),
            KeyCode::Char('c' | 'd') if ctrl => return Ok(None),
            KeyCode::Char('u') if ctrl => editor.clear_to_start(),
            KeyCode::Char('a') if ctrl => editor.home(),
            KeyCode::Char('e') if ctrl => editor.end(),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Left => editor.left(),
            KeyCode::Right => editor.right(),
            KeyCode::Home => editor.home(),
            KeyCode::End => editor.end(),
            // Any other modifier combination is a shortcut, not text.
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                editor.insert(c)
            }
            _ => {}
        }
    }
}

/// A key press, resolved to what it should do.
enum Action {
    Up,
    Down,
    First,
    Last,
    Forward,
    Back,
    Select,
    /// Start typing an incremental search.
    Search,
    /// Esc: drop the search if there is one, otherwise leave the menu.
    Cancel,
    Quit,
    None,
}

/// A key press while the incremental search is being typed.
enum Search {
    /// Add a character to the search string.
    Push(char),
    /// Remove the last character; on an empty string, leave the search.
    Pop,
    /// Run what the cursor is on.
    Accept,
    /// Drop the search and show everything again.
    Cancel,
    /// Empty the search string without leaving the search.
    Clear,
    /// Leave the menu outright.
    Quit,
    /// Move around without leaving the search.
    Nav(Action),
    None,
}

/// Map a key press to an action.
///
/// Both vi keys and arrows are accepted, plus the readline motions `Ctrl-N` /
/// `Ctrl-P`, which the original Python implementation also supported.
fn classify(key: &KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('n') if ctrl => Action::Down,
        KeyCode::Char('p') if ctrl => Action::Up,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::Char('l') | KeyCode::Right => Action::Forward,
        KeyCode::Char('h') | KeyCode::Left => Action::Back,
        KeyCode::Char('g') | KeyCode::Home => Action::First,
        KeyCode::Char('G') | KeyCode::End => Action::Last,
        KeyCode::Char('q') => Action::Quit,
        // Esc is not simply quit: the status row offers it as the way to drop
        // a filter, so it has to mean that while one is on screen.
        KeyCode::Esc => Action::Cancel,
        KeyCode::Char('/') => Action::Search,
        KeyCode::Enter => Action::Select,
        _ => Action::None,
    }
}

/// Map a key press to what it means while the search is being typed.
///
/// **Letters are text here, so the vi keys are not available**: moving around
/// is the arrows and the readline motions, which cannot be typed. The search
/// string has no cursor of its own for the same reason — it is only appended
/// to and backspaced — which is what leaves ← and → free to walk the menu.
fn classify_search(key: &KeyEvent) -> Search {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        // Ctrl-C leaves, wherever it is pressed. Making it mean "cancel the
        // search" would be the one place in the program where it does not stop
        // what is going on, which is not a distinction worth spending on a
        // filter that Esc already drops.
        KeyCode::Char('c') if ctrl => Search::Quit,
        KeyCode::Esc => Search::Cancel,
        KeyCode::Enter => Search::Accept,
        KeyCode::Backspace => Search::Pop,
        // Readline's "clear the line": empties what was typed and stays here,
        // the same as it does at the argument prompt.
        KeyCode::Char('u') if ctrl => Search::Clear,
        KeyCode::Char('n') if ctrl => Search::Nav(Action::Down),
        KeyCode::Char('p') if ctrl => Search::Nav(Action::Up),
        KeyCode::Down => Search::Nav(Action::Down),
        KeyCode::Up => Search::Nav(Action::Up),
        KeyCode::Right => Search::Nav(Action::Forward),
        KeyCode::Left => Search::Nav(Action::Back),
        KeyCode::Home => Search::Nav(Action::First),
        KeyCode::End => Search::Nav(Action::Last),
        // Any modifier combination other than shift is a shortcut, not text.
        KeyCode::Char(c) if !ctrl && !alt => Search::Push(c),
        _ => Search::None,
    }
}

/// Rows reserved around the list besides the footer: the title and the blank
/// line under it.
const CHROME_ROWS: u16 = 2;

/// How many rows the status line may grow to when the command does not fit on
/// one. Ten is enough for the scripts that motivated this (a deploy pipeline
/// written as several commands) while still leaving a short terminal a list to
/// scroll; past that the rest is cut.
const MAX_FOOTER_ROWS: u16 = 10;

/// How many rows the selected entry's description takes above the status line.
///
/// A fixed number rather than however many the text needs, and reserved for the
/// whole menu rather than per entry: the block sits between the list and the
/// status line, so a height that followed the selected entry would move the
/// bottom of the list on every keystroke. Three rows is enough for a sentence
/// or two at a usual width; what does not fit is left to the detail view
/// (`l` / `→`), which is where the whole text is.
const DESCRIPTION_ROWS: u16 = 3;

fn draw(menu: &mut MenuState, searching: bool) -> Result<()> {
    let (cols, rows) = terminal_size();
    let frame = render(menu, cols, rows, theme::enabled(), searching)?;
    flush(&frame)
}

/// Build one frame. Separate from [`draw`] so it can be rendered and inspected
/// without a terminal.
///
/// `color` is passed in rather than read inside: [`theme::enabled`] is decided
/// once per process, so a test could otherwise only ever see one of the two
/// modes — and the fallback for the other one is exactly what needs guarding.
fn render(
    menu: &mut MenuState,
    cols: u16,
    rows: u16,
    color: bool,
    searching: bool,
) -> Result<Vec<u8>> {
    let help_lines = detail_help(menu.frame().help.as_deref(), cols, rows);
    // The help sits under the title with a blank row beneath it.
    let help_rows = if help_lines.is_empty() {
        0
    } else {
        help_lines.len() as u16 + 1
    };
    // The status line is laid out before the list, because how many rows it
    // takes is what the list has left to work with.
    let footer_lines = wrap(&footer_text(menu), cols, footer_budget(rows, help_rows));
    let footer_rows = footer_lines.len() as u16;
    // Reserved for the whole menu, so the list keeps its height as the cursor
    // moves between entries that have a description and entries that do not.
    // A menu where nothing has one spends no rows on it at all.
    let description_rows = description_budget(menu, rows, help_rows, footer_rows);
    let description_lines = description_lines(menu, cols, description_rows);
    let list_height = rows
        .saturating_sub(CHROME_ROWS + help_rows + footer_rows + description_rows)
        .max(1) as usize;
    menu.scroll_into_view(list_height);

    // The whole frame is built in memory and written once. `stderr` is
    // unbuffered, so painting straight to it would turn every colour change
    // into its own write, and a redraw of a full screen into hundreds of them
    // — visibly torn over a slow link.
    let mut out = Vec::new();
    execute!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;

    let title = truncate(&menu.frame().title, cols);
    paint_with(color, &mut out, Style::fg(theme::TITLE).bold(), &title)?;
    if menu.depth() > 1 {
        // The hint only fits in whatever the title left of the line.
        let hint = truncate(
            " (h/← to go back)",
            cols.saturating_sub(title.chars().count() as u16),
        );
        paint_with(color, &mut out, Style::fg(theme::MUTED), &hint)?;
    }
    writeln!(out, "\r")?;

    for line in &help_lines {
        paint_with(color, &mut out, Style::fg(theme::HELP), line)?;
        writeln!(out, "\r")?;
    }
    if !help_lines.is_empty() {
        writeln!(out, "\r")?;
    }
    // The search line takes the place of the blank row under the title rather
    // than adding one, so turning the search on does not shorten the list.
    if searching || !menu.query().is_empty() {
        paint_with(
            color,
            &mut out,
            Style::fg(theme::COMMAND).bold(),
            &truncate(&search_line(menu.query(), searching), cols),
        )?;
    }
    writeln!(out, "\r")?;

    if menu.is_empty() {
        let empty = if menu.query().is_empty() {
            "  (no entries)"
        } else {
            // Saying which of the two it is saves the reader wondering whether
            // the menu is empty or the filter is just too narrow.
            "  (no matches)"
        };
        paint_with(color, &mut out, Style::fg(theme::MUTED), empty)?;
        writeln!(out, "\r")?;
    }

    let offset = menu.offset();
    let cursor = menu.cursor();
    let items = menu.items();

    for (index, item) in items.iter().enumerate().skip(offset).take(list_height) {
        let selected = index == cursor;
        let line = entry_line(item, selected);

        if selected {
            // Padded first, so the highlight is a bar across the whole width
            // rather than a patch the length of the label.
            let line = pad(&truncate(&line, cols), cols);
            let (marker, label) = split_marker(&line);
            // The runs share a background, so the bar reads as one block
            // with the marker picked out in front of it.
            paint_with(
                color,
                &mut out,
                Style::fg(theme::CURSOR)
                    .on(theme::SELECTED_BG)
                    .bold()
                    .highlight(),
                marker,
            )?;
            paint_with(
                color,
                &mut out,
                Style::fg(theme::SELECTED_FG)
                    .on(theme::SELECTED_BG)
                    .bold()
                    .highlight(),
                label,
            )?;
        } else {
            let style = if item.has_detail() {
                Style::fg(theme::CONTAINER)
            } else {
                Style::default()
            };
            paint_with(color, &mut out, style, &truncate(&line, cols))?;
        }
        writeln!(out, "\r")?;
    }

    // The selected entry's description, in the rows reserved for it directly
    // above the status line. Written at absolute rows for the same reason the
    // status line is: the list above may not have filled its own area.
    let description_first_row = rows.saturating_sub(footer_rows + description_rows);
    for (index, line) in description_lines.iter().enumerate() {
        execute!(out, cursor::MoveTo(0, description_first_row + index as u16))?;
        paint_with(
            color,
            &mut out,
            Style::fg(theme::HELP),
            &truncate(line, cols),
        )?;
    }

    // The status line sits at the bottom of the screen and grows upwards, so a
    // command too long for one row is readable instead of cut at the edge.
    let first_row = rows.saturating_sub(footer_rows);
    for (index, line) in footer_lines.iter().enumerate() {
        execute!(out, cursor::MoveTo(0, first_row + index as u16))?;
        let line = pad(line, cols);
        if index == 0 {
            // Only the first row carries the sigil, and only there is it
            // painted apart from the text.
            let (sigil, rest) = split_marker(&line);
            paint_with(
                color,
                &mut out,
                Style::fg(theme::COMMAND)
                    .on(theme::FOOTER_BG)
                    .bold()
                    .highlight(),
                sigil,
            )?;
            paint_with(
                color,
                &mut out,
                Style::fg(theme::FOOTER_FG).on(theme::FOOTER_BG).highlight(),
                rest,
            )?;
        } else {
            paint_with(
                color,
                &mut out,
                Style::fg(theme::FOOTER_FG).on(theme::FOOTER_BG).highlight(),
                &line,
            )?;
        }
    }

    Ok(out)
}

/// One row of the list: the cursor marker, the label, and the `>` that says the
/// entry has a detail view.
fn entry_line(item: &MenuItem, selected: bool) -> String {
    let detail_marker = if item.has_detail() { " >" } else { "  " };
    format!(
        "{} {}{detail_marker}",
        cursor_marker(selected),
        item.label()
    )
}

/// How many rows to reserve above the status line for entry descriptions.
///
/// Zero unless something in the menu has one, so a configuration with no `help:`
/// anywhere loses nothing to it. Otherwise [`DESCRIPTION_ROWS`], less whatever a
/// short terminal cannot spare — the chrome, the detail view's own help, the
/// status line and one row of list all come first, because the status line is
/// written at an absolute row and a list pushed into it is painted over rather
/// than shortened.
fn description_budget(menu: &MenuState, rows: u16, help_rows: u16, footer_rows: u16) -> u16 {
    // The whole frame, not what the filter left: a search that happens to
    // exclude every entry with a `help` would otherwise take the block away
    // mid-keystroke and move the bottom of the list, which is the thing this
    // reservation exists to prevent.
    if !menu.frame().items.iter().any(|item| item.help.is_some()) {
        return 0;
    }
    let spare = rows.saturating_sub(CHROME_ROWS + help_rows + footer_rows + 1);
    DESCRIPTION_ROWS.min(spare)
}

/// The selected entry's description, as the rows reserved for it.
///
/// Always exactly `rows` long, blank rows included: the block is a fixed part
/// of the frame, and returning fewer rows for a short description would leave
/// whatever the last frame drew there on screen.
fn description_lines(menu: &MenuState, cols: u16, rows: u16) -> Vec<String> {
    if rows == 0 {
        return Vec::new();
    }
    let text = menu
        .selected()
        .and_then(|item| item.help.as_deref())
        .map(one_paragraph)
        .unwrap_or_default();
    let mut lines = if text.is_empty() {
        Vec::new()
    } else {
        wrap(&text, cols, rows)
    };
    lines.resize(rows as usize, String::new());
    lines
}

/// The help shown under the title of a detail view, as the rows it takes.
///
/// Wrapped rather than cut at the edge: a `help:` written as a block scalar is
/// several sentences, and one row of it says nothing.
fn detail_help(help: Option<&str>, cols: u16, rows: u16) -> Vec<String> {
    match help {
        Some(help) => wrap(&one_paragraph(help), cols, help_budget(rows)),
        None => Vec::new(),
    }
}

/// How many rows the detail view's help may use on a terminal this tall.
///
/// Half the screen, the same share the status line may grow to: the view exists
/// to show the help, so it gets the larger part of what it asks for, and a help
/// longer than that is one no list would survive sharing a screen with. Never so
/// many that the chrome, the blank row under the help itself, one row of list
/// and the status line have nowhere to go — the status line is written at an
/// absolute row, so a list pushed into it is not shortened but painted over.
/// Always at least one row, which is what the help had before it could wrap at
/// all.
fn help_budget(rows: u16) -> u16 {
    (rows / 2).min(rows.saturating_sub(CHROME_ROWS + 3)).max(1)
}

/// Help text as one run of words.
///
/// The newlines of a block scalar in the configuration file are breaks in the
/// file, not in the menu, and [`sanitize`] would otherwise draw each of them as
/// a `·` in the middle of the sentence.
fn one_paragraph(help: &str) -> String {
    help.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The search row: the string being typed, or what a finished search left.
///
/// The trailing block is a cursor for a line that has none of its own — the
/// real cursor is hidden while the menu is open. Once the search is accepted
/// the row stays, without the block, because a filtered list that looked
/// unfiltered would be the worst outcome here.
fn search_line(query: &str, searching: bool) -> String {
    if searching {
        format!("  /{query}\u{2588}")
    } else {
        format!("  /{query}  (Esc to clear)")
    }
}

/// What the status line says about the selected entry.
fn footer_text(menu: &MenuState) -> String {
    match menu.selected() {
        Some(item) => match item.launch() {
            Some(Launch::Script(script)) => format!("$ {}", one_line(&script)),
            // `&` between the commands, the way a shell would be told to run
            // them at once, so the status line says which of the two an entry
            // is without spending a word on it.
            Some(Launch::Parallel(jobs)) => format!(
                "& {}",
                jobs.iter()
                    .map(|job| one_line(&job.script))
                    .collect::<Vec<_>>()
                    .join(" & ")
            ),
            None => format!("> {} entries", item.submenu.len()),
        },
        None => String::new(),
    }
}

/// How many rows the status line may use on a terminal this tall.
///
/// Capped by [`MAX_FOOTER_ROWS`], by half the screen, and by what is left once
/// the title, the blank line, the help text and one row of list have been
/// taken: a status line that grew into those would paint over them. Always at
/// least one, so the line exists even on a terminal too short for the rest —
/// which is what the previous single-row status line did.
fn footer_budget(rows: u16, help_rows: u16) -> u16 {
    let spare = rows.saturating_sub(CHROME_ROWS + help_rows + 1);
    MAX_FOOTER_ROWS.min(rows / 2).min(spare).max(1)
}

/// Break `text` into at most `max_rows` rows of at most `cols` columns.
///
/// Rows are cut at whatever cluster reaches the width — a command has no
/// spaces to prefer, and breaking a path or a flag at a space it happens to
/// contain would read as two arguments. What does not fit in `max_rows` is
/// dropped: the status line is a preview, and the entry itself is the source
/// of truth for what will run.
///
/// Always returns at least one row, so the caller can reserve a status line
/// whether or not anything is selected.
fn wrap(text: &str, cols: u16, max_rows: u16) -> Vec<String> {
    // Sanitized once, up front: doing it per row and then slicing the original
    // by the row's byte length would drift, because a control character and
    // the `·` that replaces it are not the same number of bytes.
    let sanitized: String = sanitize(text).collect();
    let max = cols.saturating_sub(1) as usize;

    let mut rows = Vec::new();
    let mut rest = sanitized.as_str();
    while !rest.is_empty() && (rows.len() as u16) < max_rows {
        let (head, tail) = split_at_width(rest, max);
        if head.is_empty() {
            // Nothing fits — a terminal one column wide, or a cluster wider
            // than the whole line. Stop rather than loop forever.
            break;
        }
        rows.push(head.to_string());
        rest = tail;
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Send a frame built by [`render`] or [`draw_prompt`] to the terminal in one
/// write.
fn flush(frame: &[u8]) -> Result<()> {
    let mut out = stderr();
    out.write_all(frame)?;
    out.flush()?;
    Ok(())
}

/// A multi-line script as one status-line-sized preview.
fn one_line(script: &str) -> String {
    script.replace('\n', " ; ")
}

/// Split the two-character sigil (`*>`, `$ `, `> `, `& `) off the front of a line so
/// it can be coloured on its own. Both halves may be empty on a very narrow
/// terminal.
fn split_marker(line: &str) -> (&str, &str) {
    match line.char_indices().nth(2) {
        Some((at, _)) => line.split_at(at),
        None => (line, ""),
    }
}

fn draw_prompt(label: &str, editor: &LineEditor) -> Result<()> {
    let (cols, rows) = terminal_size();
    let mut out = Vec::new();

    let line = format!("{label}: {}", editor.text());
    let line = truncate(&line, cols);
    let (prompt, value) = match line.char_indices().nth(label.chars().count() + 1) {
        Some((at, _)) => line.split_at(at),
        None => (line.as_str(), ""),
    };
    execute!(out, cursor::MoveTo(0, rows.saturating_sub(1)))?;
    execute!(out, terminal::Clear(terminal::ClearType::CurrentLine))?;
    let color = theme::enabled();
    paint_with(color, &mut out, Style::fg(theme::TITLE).bold(), prompt)?;
    paint_with(color, &mut out, Style::default(), value)?;

    // Put the real cursor where the text cursor is, so the terminal's own
    // caret marks the insertion point.
    let column = (label.chars().count() + 2 + editor.cursor()).min(cols as usize - 1);
    execute!(
        out,
        cursor::MoveTo(column as u16, rows.saturating_sub(1)),
        cursor::Show
    )?;

    flush(&out)
}

/// Terminal size, with a usable fallback.
///
/// A zero is treated the same as an error: a PTY that was never given a
/// winsize reports `0x0`, and every line would then be truncated to nothing.
fn terminal_size() -> (u16, u16) {
    usable_size(terminal::size().ok())
}

/// Split out so the fallback can be tested without a real terminal.
fn usable_size(reported: Option<(u16, u16)>) -> (u16, u16) {
    let (cols, rows) = reported.unwrap_or((0, 0));
    (
        if cols == 0 { 80 } else { cols },
        if rows == 0 { 24 } else { rows },
    )
}

fn cursor_marker(selected: bool) -> &'static str {
    if selected { "*>" } else { "  " }
}

/// Cut a line to the terminal width.
///
/// Control characters are removed first: labels can come from a file in the
/// checkout (an npm script name, a make target), and a JSON string can carry a
/// real ESC or newline. Writing those straight out would let merely *opening*
/// the menu in an untrusted repository run terminal escape sequences — OSC
/// clipboard writes, cursor moves — without anything being selected.
///
/// Cut `text` to what fits in `cols`, **measured in terminal columns**.
///
/// Not in `char`s: a CJK character occupies two columns, so a row of Japanese
/// counted by `char` is twice as wide as the terminal thinks it asked for. The
/// row then wraps, and for the selected row — which is padded to the full width
/// and painted with a background — the wrap shows up as a second, half-filled
/// bar under it.
///
/// One column is left unused at the right edge: writing into the last cell
/// leaves the cursor in a "pending wrap" state that some terminals resolve into
/// a blank line of their own.
fn truncate(text: &str, cols: u16) -> String {
    let sanitized: String = sanitize(text).collect();
    split_at_width(&sanitized, cols.saturating_sub(1) as usize)
        .0
        .to_string()
}

/// Split `text` at the last grapheme cluster boundary that still fits in `max`
/// terminal columns (see [`display_width`]).
///
/// The first half is empty when not even one cluster fits, which is what tells
/// a caller that is filling rows to stop instead of looping on a line it can
/// never shorten.
fn split_at_width(text: &str, max: usize) -> (&str, &str) {
    let mut end = 0;
    let mut width = 0;
    for cluster in text.graphemes(true) {
        let advance = UnicodeWidthStr::width(cluster);
        if width + advance > max {
            break;
        }
        end += cluster.len();
        width += advance;
    }
    text.split_at(end)
}

/// How many terminal columns a string takes.
///
/// Measured per grapheme cluster, not per `char`. Summing characters gets the
/// combined forms wrong in both directions: an emoji built from several code
/// points (a ZWJ sequence, a skin-tone modifier) would be counted once per
/// piece, and a base character followed by a variation selector — `❤️` — would
/// be counted as the one-column text form the terminal does not draw. Cutting
/// at a cluster boundary also keeps a modifier from being left behind without
/// the character it modifies.
fn display_width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// Replace every control character with `·` so it cannot reach the terminal
/// as a command. C1 (`U+0080`–`U+009F`) counts too: in a UTF-8 terminal those
/// are alternative forms of the C0 escape sequences.
fn sanitize(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().map(|c| {
        if c.is_control() || ('\u{80}'..='\u{9f}').contains(&c) {
            '·'
        } else {
            c
        }
    })
}

/// Fill `text` out to `cols`, measured in terminal columns (see [`truncate`]).
fn pad(text: &str, cols: u16) -> String {
    pad_to(text, cols.saturating_sub(1) as usize)
}

/// Fill `text` out to `width` terminal columns. A row that is already at least
/// that wide is left alone.
fn pad_to(text: &str, width: usize) -> String {
    let len = display_width(text);
    if len >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_a_usable_size_when_the_terminal_reports_zero() {
        assert_eq!(usable_size(None), (80, 24));
        assert_eq!(usable_size(Some((0, 0))), (80, 24));
        assert_eq!(usable_size(Some((0, 40))), (80, 40));
        assert_eq!(usable_size(Some((120, 0))), (120, 24));
        assert_eq!(usable_size(Some((120, 40))), (120, 40));
    }

    #[test]
    fn truncates_by_terminal_columns() {
        assert_eq!(truncate("abcdef", 4), "abc");
        assert_eq!(truncate("abc", 80), "abc");
        // Two columns per character, and one column is held back at the right
        // edge: 4 columns fit one of these, not three.
        assert_eq!(truncate("日本語です", 4), "日");
        assert_eq!(truncate("日本語です", 8), "日本語");
        // A character that would straddle the last usable column is dropped
        // whole rather than half-drawn.
        assert_eq!(truncate("a日本", 4), "a日");
    }

    #[test]
    fn measures_and_cuts_by_grapheme_cluster() {
        // Each of these is one cluster the terminal draws two columns wide,
        // however many code points it is made of.
        for cluster in ["❤\u{fe0f}", "👨\u{200d}👩\u{200d}👧", "🧑🏽", "🇯🇵"] {
            assert_eq!(display_width(cluster), 2, "{cluster:?}");
            // Too narrow to hold it: dropped whole, never split into the
            // pieces it is built from.
            assert_eq!(truncate(cluster, 2), "", "{cluster:?}");
            assert_eq!(truncate(cluster, 3), cluster, "{cluster:?}");
        }

        // A combining mark stays with the character it modifies.
        assert_eq!(display_width("a\u{301}"), 1);
        assert_eq!(truncate("a\u{301}b", 3), "a\u{301}b");
    }

    #[test]
    fn a_padded_row_is_exactly_one_line_wide() {
        // The bug this guards: with `char`-counted padding a row of Japanese
        // came out twice as wide as the terminal, so the selected row's
        // background wrapped onto a second line (CYBERNEURA-DEV-514).
        for text in [
            "backend の環境変数を SSM から反映して再起動 >",
            "ascii only",
            "",
            "日",
            "deploy 🚀 ❤\u{fe0f} 👨\u{200d}👩\u{200d}👧",
        ] {
            let line = pad(&truncate(text, 40), 40);
            assert_eq!(
                display_width(&line),
                39,
                "a padded row must fill the line exactly once: {line:?}"
            );
        }
    }

    #[test]
    fn a_long_command_is_wrapped_instead_of_cut() {
        let command = "$ ".to_string() + &"deploy --target production ".repeat(6);
        let rows = wrap(&command, 40, 10);

        assert!(rows.len() > 1, "a long command should use several rows");
        assert!(
            rows.iter().all(|row| display_width(row) <= 39),
            "no row may run past the edge: {rows:?}"
        );
        // Nothing is lost between the rows.
        assert_eq!(rows.concat(), command);
    }

    #[test]
    fn wrapping_stops_at_the_row_budget() {
        let command = "x".repeat(1000);
        let rows = wrap(&command, 40, 10);

        assert_eq!(rows.len(), 10);
        // Beyond the budget the rest is dropped, not squeezed in.
        assert_eq!(rows.concat().chars().count(), 10 * 39);
    }

    #[test]
    fn wrapping_replaces_control_characters_without_losing_the_rest() {
        // The replacement is a different length in bytes from what it replaces,
        // so a wrap that sliced the original text by the row's byte length
        // would drift into the middle of a character.
        let command = format!("$ echo {}", "a\u{1}".repeat(30));
        let rows = wrap(&command, 20, 10);

        assert!(rows.iter().all(|row| display_width(row) <= 19), "{rows:?}");
        assert!(!rows.concat().contains('\u{1}'), "{rows:?}");
        assert_eq!(rows.concat(), sanitize(&command).collect::<String>());
    }

    #[test]
    fn a_short_command_still_uses_one_row() {
        assert_eq!(wrap("$ ls", 40, 10), vec!["$ ls".to_string()]);
        assert_eq!(wrap("", 40, 10), vec![String::new()]);
    }

    #[test]
    fn the_status_line_leaves_the_list_something_to_show() {
        // A tall terminal: the cap is the one the task asked for.
        assert_eq!(footer_budget(24, 0), 10);
        assert_eq!(footer_budget(24, 2), 10);
        // Half the screen, and never into the title, the help or the last row
        // of list.
        assert_eq!(footer_budget(10, 0), 5);
        assert_eq!(footer_budget(10, 2), 5);
        assert_eq!(footer_budget(8, 2), 3);
        // Too short for all of it: the status line keeps the single row it had
        // before this change.
        assert_eq!(footer_budget(4, 2), 1);
        assert_eq!(footer_budget(3, 0), 1);
        assert_eq!(footer_budget(1, 0), 1);
    }

    #[test]
    fn a_wrapped_status_line_never_paints_outside_the_screen() {
        // The rows the frame moves the cursor to, from the CSI sequences.
        fn rows_painted(frame: &str) -> Vec<u16> {
            let mut rows = Vec::new();
            for part in frame.split('\u{1b}') {
                // `[<row>;<col>H`, one-based.
                if let Some(rest) = part.strip_prefix('[') {
                    if let Some((row, tail)) = rest.split_once(';') {
                        if tail.contains('H') {
                            if let Ok(row) = row.parse::<u16>() {
                                rows.push(row - 1);
                            }
                        }
                    }
                }
            }
            rows
        }

        for rows in [4u16, 6, 10, 24] {
            for help in [None, Some("some help".to_string())] {
                let mut menu = menu_with_long_command(help.clone());
                let frame = render(&mut menu, 30, rows, false, false).unwrap();
                let frame = String::from_utf8(frame).unwrap();

                assert!(
                    rows_painted(&frame).iter().all(|row| *row < rows),
                    "the frame painted past the last row (rows={rows}, help={help:?})"
                );
                // Below six rows the frame is taller than the terminal
                // whatever the status line does — title, blank, help and one
                // row of list already do not fit — so only the sizes where the
                // layout has room are checked here.
                if rows >= 6 {
                    assert!(
                        without_escapes(&frame).lines().count() <= rows as usize + 1,
                        "the frame is taller than the terminal (rows={rows}, help={help:?})"
                    );
                }
            }
        }
    }

    /// A menu whose only entry runs a command far too long for one row,
    /// optionally with help text (which reserves rows of its own).
    fn menu_with_long_command(help: Option<String>) -> MenuState {
        use crate::config::model::Shell;

        let item = MenuItem {
            title: Some("deploy".into()),
            shell: Some(Shell::One(
                "deploy --target production --and-then-some ".repeat(4),
            )),
            ..MenuItem::default()
        };
        let mut frame = Frame::new("title", vec![item]);
        frame.help = help;
        MenuState::new(frame)
    }

    #[test]
    fn padding_leaves_a_row_that_is_already_full_alone() {
        let full = "x".repeat(39);
        assert_eq!(pad(&full, 40), full);
    }

    #[test]
    fn strips_control_characters_from_repository_controlled_text() {
        // An npm script name or make target can carry a real ESC; none of it
        // may reach the terminal as a command.
        assert_eq!(truncate("a\u{1b}[2Jb", 80), "a·[2Jb");
        assert_eq!(truncate("a\nb", 80), "a·b");
        assert_eq!(truncate("a\u{7}b", 80), "a·b");
        assert_eq!(truncate("a\u{9b}b", 80), "a·b", "C1 CSI must go too");
        assert_eq!(truncate("plain", 80), "plain", "ordinary text is untouched");
    }

    #[test]
    fn pads_to_the_terminal_width() {
        assert_eq!(pad("ab", 5), "ab  ");
        assert_eq!(pad("abcdef", 4), "abcdef");
    }

    /// The scripts a resolved entry would run, whichever kind it is.
    fn scripts(launch: Launch) -> Vec<String> {
        match launch {
            Launch::Script(script) => vec![script],
            Launch::Parallel(jobs) => jobs.into_iter().map(|job| job.script).collect(),
        }
    }

    fn parallel_item(scripts: &[&str]) -> MenuItem {
        MenuItem {
            title: Some("group".into()),
            parallel: scripts
                .iter()
                .map(|script| crate::config::model::ParallelCommand {
                    title: None,
                    shell: crate::config::model::Shell::One((*script).to_string()),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn resolves_an_entry_without_arguments_directly() {
        let item = MenuItem::command("t", "echo hi");
        assert_eq!(scripts(resolve(&item).unwrap().unwrap()), ["echo hi"]);
    }

    #[test]
    fn resolves_a_parallel_entry_into_one_job_per_command() {
        let item = parallel_item(&["npm run dev", "npm run api"]);
        assert_eq!(
            scripts(resolve(&item).unwrap().unwrap()),
            ["npm run dev", "npm run api"]
        );
    }

    #[test]
    fn a_parallel_entry_is_runnable_rather_than_openable() {
        // Enter must run the group; the menu opens an entry only when there is
        // nothing to run, and a `parallel` entry has no `shell`.
        let item = parallel_item(&["true"]);
        assert!(item.launch().is_some());
        assert!(!item.has_detail());
    }

    #[test]
    fn the_status_line_tells_a_parallel_entry_apart_from_a_single_command() {
        let mut menu = MenuState::new(Frame::new(
            "title",
            vec![parallel_item(&["npm run dev", "npm run api"])],
        ));
        let frame = String::from_utf8(render(&mut menu, 60, 10, false, false).unwrap()).unwrap();
        let text = without_escapes(&frame);
        assert!(
            text.contains("& npm run dev & npm run api"),
            "the status line should show both commands: {text:?}"
        );
        assert!(
            !text.contains("> 0 entries"),
            "a parallel entry is not an empty container: {text:?}"
        );
    }

    #[test]
    fn maps_vi_keys_and_arrows_to_the_same_actions() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(matches!(classify(&key(KeyCode::Char('j'))), Action::Down));
        assert!(matches!(classify(&key(KeyCode::Down)), Action::Down));
        assert!(matches!(classify(&key(KeyCode::Char('k'))), Action::Up));
        assert!(matches!(classify(&key(KeyCode::Up)), Action::Up));
        assert!(matches!(
            classify(&key(KeyCode::Char('l'))),
            Action::Forward
        ));
        assert!(matches!(classify(&key(KeyCode::Right)), Action::Forward));
        assert!(matches!(classify(&key(KeyCode::Char('h'))), Action::Back));
        assert!(matches!(classify(&key(KeyCode::Left)), Action::Back));
        assert!(matches!(classify(&key(KeyCode::Char('q'))), Action::Quit));
        assert!(matches!(classify(&key(KeyCode::Esc)), Action::Cancel));
        assert!(matches!(classify(&key(KeyCode::Enter)), Action::Select));
    }

    #[test]
    fn slash_starts_the_search_and_letters_are_text_inside_it() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        assert!(matches!(classify(&key(KeyCode::Char('/'))), Action::Search));

        // The vi keys are text once the search is open; that is the whole
        // reason the arrows and the readline motions still navigate.
        assert!(matches!(
            classify_search(&key(KeyCode::Char('j'))),
            Search::Push('j')
        ));
        assert!(matches!(
            classify_search(&key(KeyCode::Char('q'))),
            Search::Push('q')
        ));
        assert!(matches!(
            classify_search(&key(KeyCode::Down)),
            Search::Nav(Action::Down)
        ));
        assert!(matches!(
            classify_search(&ctrl('n')),
            Search::Nav(Action::Down)
        ));
        assert!(matches!(
            classify_search(&key(KeyCode::Right)),
            Search::Nav(Action::Forward)
        ));
        assert!(matches!(
            classify_search(&key(KeyCode::Esc)),
            Search::Cancel
        ));
        // Ctrl-C leaves from here too: it is the one key that always stops
        // what is going on, and a search is not an exception to that.
        assert!(matches!(classify_search(&ctrl('c')), Search::Quit));
        assert!(matches!(classify_search(&ctrl('u')), Search::Clear));
        assert!(matches!(
            classify_search(&key(KeyCode::Enter)),
            Search::Accept
        ));
        assert!(matches!(
            classify_search(&key(KeyCode::Backspace)),
            Search::Pop
        ));
    }

    #[test]
    fn the_search_row_replaces_the_blank_line_rather_than_taking_a_row() {
        // Both entries match, so the two frames list the same number of rows
        // and any difference in height is the chrome.
        let items = vec![
            MenuItem::command("deploy", "echo deploy"),
            MenuItem::command("develop", "echo develop"),
        ];
        let rows = 10;

        let mut plain = MenuState::new(Frame::new("title", items.clone()));
        let plain = without_escapes(
            &String::from_utf8(render(&mut plain, 40, rows, false, false).unwrap()).unwrap(),
        );

        let mut searching = MenuState::new(Frame::new("title", items));
        searching.set_query("de".into());
        let searched = without_escapes(
            &String::from_utf8(render(&mut searching, 40, rows, false, true).unwrap()).unwrap(),
        );

        assert!(searched.contains("/de"), "the search string: {searched:?}");
        assert!(
            searched.contains("deploy") && searched.contains("develop"),
            "both entries match this query: {searched:?}"
        );
        // The row count is what proves the search line cost the list nothing.
        assert_eq!(
            plain.lines().count(),
            searched.lines().count(),
            "the search row takes the place of the blank line under the title"
        );
    }

    #[test]
    fn escape_drops_the_filter_before_it_leaves_the_menu() {
        let mut menu = MenuState::new(Frame::new(
            "title",
            vec![
                MenuItem::command("build", "echo build"),
                MenuItem::command("deploy", "echo deploy"),
            ],
        ));
        menu.set_query("de".into());

        // The status row says "Esc to clear" here, so Esc has to clear.
        assert!(escape(&mut menu).is_none());
        assert_eq!(menu.query(), "");
        assert_eq!(menu.items().len(), 2);

        // With nothing to clear it is the way out, as it always was.
        assert!(matches!(escape(&mut menu), Some(Outcome::Cancelled)));
    }

    #[test]
    fn says_when_a_search_matched_nothing() {
        let mut menu = MenuState::new(Frame::new(
            "title",
            vec![MenuItem::command("build", "echo build")],
        ));
        menu.set_query("zzz".into());
        let text = without_escapes(
            &String::from_utf8(render(&mut menu, 40, 10, false, true).unwrap()).unwrap(),
        );
        assert!(
            text.contains("(no matches)"),
            "an empty result is not an empty menu: {text:?}"
        );
        assert!(!text.contains("(no entries)"), "{text:?}");
    }

    #[test]
    fn an_accepted_search_still_says_the_list_is_filtered() {
        // A filtered list that looked unfiltered would be the worst outcome:
        // the entry you wanted would look as though it does not exist.
        let mut menu = MenuState::new(Frame::new(
            "title",
            vec![
                MenuItem::command("build", "echo build"),
                MenuItem::command("deploy", "echo deploy"),
            ],
        ));
        menu.set_query("de".into());
        let text = without_escapes(
            &String::from_utf8(render(&mut menu, 40, 10, false, false).unwrap()).unwrap(),
        );
        assert!(text.contains("/de"), "{text:?}");
        assert!(text.contains("Esc to clear"), "{text:?}");
    }

    /// Render a small menu and return the frame as text, escapes included.
    ///
    /// This is the only look at the drawing code that does not need a
    /// terminal. `color` is passed to [`render`] rather than taken from the
    /// environment, so one test run covers both modes.
    fn rendered(color: bool) -> String {
        rendered_with(color, Some("some help"))
    }

    /// [`rendered`], with the help of the selected entry chosen by the caller:
    /// the selected row is drawn differently with help on it, and both shapes
    /// need covering.
    fn rendered_with(color: bool, help: Option<&str>) -> String {
        let items = vec![
            MenuItem::command("plain", "echo hi"),
            MenuItem {
                title: Some("with help".into()),
                help: help.map(str::to_string),
                shell: Some(crate::config::model::Shell::One("echo hi".into())),
                ..Default::default()
            },
        ];
        let mut menu = MenuState::new(Frame::new("title", items));
        menu.move_down();
        String::from_utf8(render(&mut menu, 40, 10, color, false).unwrap()).unwrap()
    }

    /// Drop the escape sequences, leaving what the user actually sees. A row is
    /// painted in several runs, so its text is not contiguous in the frame.
    fn without_escapes(frame: &str) -> String {
        let mut text = String::new();
        let mut chars = frame.chars();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                text.push(c);
                continue;
            }
            // Past the `[` that opens a CSI sequence; it ends at the first
            // byte in `@`..=`~`, which the `[` itself would otherwise match.
            let mut chars = chars.by_ref().skip(1);
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        text
    }

    /// The selected row of a rendered frame, escapes still in place.
    ///
    /// Rows are matched on their visible text, so the assertions cannot be
    /// satisfied by some other part of the frame — the status line is
    /// highlighted too, and would otherwise stand in for a selected row that
    /// had lost its highlight entirely.
    fn selected_row(frame: &str) -> &str {
        frame
            .split("\r\n")
            .find(|row| without_escapes(row).contains("*> with help"))
            .unwrap_or_else(|| panic!("the selected entry is drawn: {frame:?}"))
    }

    #[test]
    fn the_selected_row_is_a_full_width_bar() {
        for color in [true, false] {
            let frame = rendered(color);
            let bar = selected_row(&frame);
            // Padded to the width, so the highlight covers the whole row.
            assert_eq!(
                without_escapes(bar).chars().count(),
                39,
                "the bar should run to the edge, got {bar:?}"
            );
        }
    }

    #[test]
    fn the_selected_row_is_marked_out_whether_or_not_color_is_on() {
        // The marker and the label: both runs of the bar have to carry the
        // highlight, or the bar has a hole in it. Without colour that means
        // reverse video, which is the whole point of `Style::highlight`: drop
        // it and this fails. The count does not change with the entry's
        // description, which no longer shares this row (CYBERNEURA-DEV-696).
        for help in [None, Some("some help")] {
            for color in [true, false] {
                let frame = rendered_with(color, help);
                let bar = selected_row(&frame);
                let sequence = if color {
                    "\u{1b}[48;5;12m"
                } else {
                    "\u{1b}[7m"
                };
                assert_eq!(
                    bar.matches(sequence).count(),
                    2,
                    "{sequence:?} does not cover {bar:?}"
                );
            }
        }
    }

    #[test]
    fn an_entry_with_a_detail_view_is_told_apart_from_a_plain_command() {
        let frame = rendered(true);
        let plain = frame
            .lines()
            .find(|line| line.contains("plain"))
            .expect("the unselected entry is drawn");
        // An unselected plain command is drawn as-is, with no escapes at all.
        // (`lines` has already taken the `\r` of the raw-mode line ending.)
        assert_eq!(plain, "   plain  ", "got {plain:?}");

        let detail = without_escapes(&frame);
        let detail = detail
            .lines()
            .find(|line| line.contains("with help"))
            .expect("the entry with a detail view is drawn");
        assert!(
            detail.contains("with help >"),
            "the detail marker is missing from {detail:?}"
        );
    }

    /// A menu of `count` entries where the one at `on` carries `help`, rendered
    /// as the text the user sees.
    fn with_help_on(count: usize, on: usize, help: Option<&str>, rows: u16) -> String {
        let items = (0..count)
            .map(|i| MenuItem {
                help: (i == on)
                    .then(|| help.unwrap_or_default().to_string())
                    .filter(|_| help.is_some()),
                ..MenuItem::command(format!("item {i}"), format!("echo {i}"))
            })
            .collect();
        let mut menu = MenuState::new(Frame::new("title", items));
        for _ in 0..on {
            menu.move_down();
        }
        without_escapes(
            &String::from_utf8(render(&mut menu, 40, rows, false, false).unwrap()).unwrap(),
        )
    }

    #[test]
    fn the_description_of_the_selected_entry_is_off_the_list() {
        let text = with_help_on(4, 1, Some("what it does"), 12);
        let row = text
            .lines()
            .find(|line| line.contains("*> item 1"))
            .expect("the selected entry is drawn");

        // Still on screen, but not on the entry's own row and not over the
        // entries under it: it belongs to the block above the status line now
        // (CYBERNEURA-DEV-696).
        assert!(text.contains("what it does"), "got {text:?}");
        assert!(!row.contains("what it does"), "got {row:?}");
    }

    #[test]
    fn a_description_hides_no_entry_however_long_it_is() {
        // The bug this guards (CYBERNEURA-DEV-696): the description used to be
        // drawn over the rows under the cursor, so a long one swallowed the
        // next few entries and there was no way to read them.
        let long = "本番環境にプログラムを反映します。".repeat(4);
        let helped = with_help_on(8, 1, Some(&long), 20);

        for i in 0..8 {
            assert!(helped.contains(&format!("item {i}")), "got {helped:?}");
        }
    }

    #[test]
    fn the_list_does_not_move_as_the_cursor_walks_over_descriptions() {
        // The block is reserved for the whole menu rather than sized to the
        // selected entry, so an entry with a description and one without leave
        // the list in the same place. Sizing it per entry would move every row
        // below on each keystroke, which is what the overlay this replaced was
        // avoiding in the first place (CYBERNEURA-DEV-582).
        let row_of = |text: &str, label: &str| {
            text.lines()
                .position(|line| line.contains(label))
                .unwrap_or_else(|| panic!("{label} is drawn: {text:?}"))
        };
        let on_described = with_help_on(8, 1, Some("a description"), 20);
        let on_plain = {
            let items = (0..8)
                .map(|i| MenuItem {
                    help: (i == 1).then(|| "a description".to_string()),
                    ..MenuItem::command(format!("item {i}"), format!("echo {i}"))
                })
                .collect();
            let mut menu = MenuState::new(Frame::new("title", items));
            menu.move_down();
            menu.move_down();
            without_escapes(
                &String::from_utf8(render(&mut menu, 40, 20, false, false).unwrap()).unwrap(),
            )
        };

        assert_eq!(row_of(&on_described, "item 7"), row_of(&on_plain, "item 7"));
    }

    #[test]
    fn a_search_that_hides_every_description_keeps_the_block() {
        // The reservation is a property of the menu, not of what the filter
        // left: taking it away as the query narrows would move the bottom of
        // the list while the user is still typing.
        let items = vec![
            MenuItem {
                help: Some("a description".into()),
                ..MenuItem::command("described", "echo hi")
            },
            MenuItem::command("plain", "echo hi"),
        ];
        let mut menu = MenuState::new(Frame::new("title", items));
        let unfiltered = description_budget(&menu, 20, 0, 1);

        menu.set_query("plain".into());

        assert_eq!(menu.items().len(), 1, "the filter should leave one entry");
        assert_eq!(description_budget(&menu, 20, 0, 1), unfiltered);
    }

    #[test]
    fn a_menu_with_no_descriptions_spends_no_rows_on_them() {
        let row_of = |text: &str, label: &str| {
            text.lines()
                .position(|line| line.contains(label))
                .unwrap_or_else(|| panic!("{label} is drawn: {text:?}"))
        };
        // Twenty entries and twelve rows, so the list is what the reserved
        // block would come out of.
        let plain = with_help_on(20, 0, None, 12);
        let described = with_help_on(20, 0, Some("a description"), 12);

        let visible = |text: &str| {
            (0..20)
                .filter(|i| text.contains(&format!("item {i}")))
                .count()
        };
        assert!(
            visible(&plain) > visible(&described),
            "a menu with no help should show more entries: {plain:?} vs {described:?}"
        );
        assert_eq!(row_of(&plain, "item 0"), row_of(&described, "item 0"));
    }

    #[test]
    fn a_long_description_stops_at_the_row_budget() {
        // Asserted on the block rather than on the frame: its rows are painted
        // at absolute positions with no line ending between them, so the frame
        // text runs them together and counting lines would say nothing.
        let items = vec![MenuItem {
            help: Some("あ".repeat(400)),
            ..MenuItem::command("item", "echo hi")
        }];
        let menu = MenuState::new(Frame::new("title", items));

        let lines = description_lines(&menu, 40, description_budget(&menu, 20, 0, 1));

        assert_eq!(lines.len(), DESCRIPTION_ROWS as usize);
        assert!(
            lines.iter().all(|line| display_width(line) <= 39),
            "no row may run past the edge: {lines:?}"
        );
        // The rest is the detail view's to show, not this block's.
        assert!(lines.concat().chars().count() < 400);
    }

    #[test]
    fn a_short_description_still_fills_its_block() {
        // Blank rows included, so a short description does not leave whatever
        // the last frame drew in those rows on screen.
        let items = vec![MenuItem {
            help: Some("short".into()),
            ..MenuItem::command("item", "echo hi")
        }];
        let menu = MenuState::new(Frame::new("title", items));

        let lines = description_lines(&menu, 40, description_budget(&menu, 20, 0, 1));

        assert_eq!(lines.len(), DESCRIPTION_ROWS as usize);
        assert_eq!(lines[0], "short");
        assert!(
            lines[1..].iter().all(|line| line.is_empty()),
            "got {lines:?}"
        );
    }

    #[test]
    fn a_description_never_paints_past_the_bottom_of_a_short_terminal() {
        // The block is written at absolute rows above the status line, so on a
        // terminal with no room to spare it has to shrink rather than overlap
        // what is already there. The expected height is spelled out per size:
        // asserting only that it fits would pass on a budget that had collapsed
        // to nothing.
        for (rows, expected) in [(4u16, 0u16), (5, 1), (6, 2), (10, 3), (24, 3)] {
            let items = vec![MenuItem {
                help: Some("あ".repeat(200)),
                ..MenuItem::command("item", "echo hi")
            }];
            let menu = MenuState::new(Frame::new("title", items));
            let budget = description_budget(&menu, rows, 0, 1);

            assert_eq!(budget, expected, "rows={rows}");
            // And whatever it is, the title, its blank row, one row of list and
            // the status line still have somewhere to go.
            assert!(
                budget + CHROME_ROWS + 2 <= rows || budget == 0,
                "rows={rows} left no room for the list (budget={budget})"
            );
        }
    }

    #[test]
    fn a_multi_line_help_is_readable_in_the_detail_view() {
        // A block scalar in the configuration is one paragraph, not a row per
        // line of the file — and one row of it used to be all that was shown.
        let help = "反映します。".repeat(20);
        let mut item = MenuItem::command("deploy", "echo deploy");
        item.help = Some(format!("{help}\n{help}"));
        let mut menu = MenuState::new(Frame::new("title", vec![item]));
        assert!(menu.enter_detail());

        let text = without_escapes(
            &String::from_utf8(render(&mut menu, 40, 24, false, false).unwrap()).unwrap(),
        );
        let rows = text.lines().filter(|line| line.contains('反')).count();
        assert!(
            rows > 1,
            "the help should wrap instead of being cut: {text:?}"
        );
        // The newline in the file is a space here, not the `·` a control
        // character is replaced with.
        assert!(!text.contains('·'), "got {text:?}");
    }

    #[test]
    fn the_help_rows_of_a_detail_view_never_take_the_whole_screen() {
        // Half the screen where there is room for it.
        assert_eq!(help_budget(24), 12);
        // Eight rows: the title, the blank row under it, the help's own blank
        // row, one entry and a status line have to fit around the help.
        assert_eq!(help_budget(8), 3);
        assert_eq!(help_budget(6), 1);
        assert_eq!(help_budget(4), 1, "always at least the row it always had");
        assert_eq!(help_budget(1), 1);
    }

    #[test]
    fn a_long_help_leaves_the_detail_view_an_entry_the_status_line_does_not_take() {
        // The status line is written at an absolute row: a list pushed into it
        // is painted over rather than shortened, so the only entry of the view
        // would silently disappear.
        for rows in [6u16, 8, 10, 24] {
            let mut item = MenuItem::command("deploy", "echo deploy");
            item.help = Some("あ".repeat(400));
            let mut menu = MenuState::new(Frame::new("title", vec![item]));
            assert!(menu.enter_detail());

            let text = without_escapes(
                &String::from_utf8(render(&mut menu, 30, rows, false, false).unwrap()).unwrap(),
            );
            let entry = text
                .lines()
                .position(|line| line.contains("Run: deploy"))
                .unwrap_or_else(|| panic!("the entry is drawn (rows={rows}): {text:?}"));
            assert!(
                entry < rows as usize - 1,
                "the status line owns the last row (rows={rows}): {text:?}"
            );
        }
    }

    #[test]
    fn maps_readline_motions() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert!(matches!(classify(&ctrl('n')), Action::Down));
        assert!(matches!(classify(&ctrl('p')), Action::Up));
        assert!(matches!(classify(&ctrl('c')), Action::Quit));
    }
}
