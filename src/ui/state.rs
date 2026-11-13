//! Navigation state of the menu.
//!
//! Kept free of any terminal handling so it can be tested without a TTY.

use crate::config::MenuItem;

/// One level of the menu. Opening a submenu pushes a frame; going back pops
/// it, which is what keeps the cursor position of the parent level.
#[derive(Debug, Clone)]
pub struct Frame {
    pub title: String,
    /// Help text of the entry this frame was opened from.
    pub help: Option<String>,
    pub items: Vec<MenuItem>,
    /// Index into [`Frame::visible`], not into [`Frame::items`].
    ///
    /// Everything the user moves through is what the filter left, so keeping
    /// the cursor in that space is what stops it pointing at a hidden entry.
    pub cursor: usize,
    /// The incremental search string. Empty means no filtering.
    query: String,
    /// Indices into `items` that match `query`, in the original order.
    visible: Vec<usize>,
}

impl Frame {
    pub fn new(title: impl Into<String>, items: Vec<MenuItem>) -> Self {
        let visible = (0..items.len()).collect();
        Self {
            title: title.into(),
            help: None,
            items,
            cursor: 0,
            query: String::new(),
            visible,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The entries the filter leaves, in the original order.
    pub fn visible(&self) -> Vec<&MenuItem> {
        self.visible.iter().map(|&i| &self.items[i]).collect()
    }

    /// Apply `query` and put the cursor on the first match.
    ///
    /// Matching is a case-insensitive substring of the label — the text on
    /// screen. Searching the command as well would show entries with nothing
    /// visible to explain why they matched.
    fn set_query(&mut self, query: String) {
        let needle = query.to_lowercase();
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| needle.is_empty() || item.label().to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        self.query = query;
        // Typing narrows the list under the cursor, so it has to come back to
        // a row that still exists. The first match is where the eye is anyway.
        self.cursor = 0;
    }
}

/// The whole menu, as a stack of frames.
#[derive(Debug, Clone)]
pub struct MenuState {
    frames: Vec<Frame>,
    /// Index of the first visible row, updated as the cursor moves.
    offset: usize,
}

impl MenuState {
    pub fn new(root: Frame) -> Self {
        Self {
            frames: vec![root],
            offset: 0,
        }
    }

    pub fn frame(&self) -> &Frame {
        self.frames.last().expect("the root frame is never popped")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("the root frame is never popped")
    }

    /// The entries currently on screen: what the incremental search left.
    pub fn items(&self) -> Vec<&MenuItem> {
        self.frame().visible()
    }

    /// The current search string; empty when the search is not in use.
    pub fn query(&self) -> &str {
        self.frame().query()
    }

    /// Replace the search string of this level.
    pub fn set_query(&mut self, query: String) {
        self.frame_mut().set_query(query);
        // A shorter list needs the window pulled back up; the next draw calls
        // scroll_into_view with the real height, and starting at the top is
        // right for a cursor that just moved to the first match.
        self.offset = 0;
    }

    /// Drop the search, keeping the cursor on the entry it is on.
    ///
    /// Leaving the cursor where it visually is (rather than resetting to the
    /// top) is what makes Esc feel like "show me the rest again" instead of
    /// "start over".
    pub fn clear_query(&mut self) {
        let selected = self.frame().visible.get(self.frame().cursor).copied();
        let frame = self.frame_mut();
        frame.set_query(String::new());
        if let Some(index) = selected {
            frame.cursor = index;
        }
        self.offset = 0;
    }

    pub fn cursor(&self) -> usize {
        self.frame().cursor
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Depth of the stack; 1 means the root menu.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub fn selected(&self) -> Option<&MenuItem> {
        let frame = self.frame();
        frame
            .visible
            .get(frame.cursor)
            .map(|&index| &frame.items[index])
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    pub fn move_down(&mut self) {
        let len = self.items().len();
        if len == 0 {
            return;
        }
        let cursor = self.cursor();
        if cursor + 1 < len {
            self.frame_mut().cursor = cursor + 1;
        }
    }

    pub fn move_up(&mut self) {
        let cursor = self.cursor();
        if cursor > 0 {
            self.frame_mut().cursor = cursor - 1;
        }
    }

    pub fn move_first(&mut self) {
        self.frame_mut().cursor = 0;
    }

    pub fn move_last(&mut self) {
        let len = self.items().len();
        if len > 0 {
            self.frame_mut().cursor = len - 1;
        }
    }

    /// Open the detail view of the selected entry. Returns `false` when the
    /// entry has no detail to show.
    ///
    /// The view holds the entry's help text, its own command (so it can still
    /// be run from in here) and its submenu. Building it this way is what lets
    /// an entry with only `help` be opened at all — with just the submenu, it
    /// would show the `>` marker and then do nothing when opened.
    pub fn enter_detail(&mut self) -> bool {
        let Some(item) = self.selected() else {
            return false;
        };
        if !item.has_detail() {
            return false;
        }

        let mut items = Vec::with_capacity(item.submenu.len() + 1);
        if item.launch().is_some() {
            items.push(MenuItem {
                title: Some(format!("Run: {}", item.label())),
                shell: item.shell.clone(),
                parallel: item.parallel.clone(),
                parallel_merged: item.parallel_merged.clone(),
                args: item.args.clone(),
                // Everything that decides what running it does has to be
                // carried over: this row is the same command, reached the
                // long way round, and it must not run somewhere else for it.
                cwd: item.cwd.clone(),
                ..Default::default()
            });
        }
        items.extend(item.submenu.iter().cloned());

        let mut frame = Frame::new(item.label(), items);
        frame.help = item.help.clone();
        self.frames.push(frame);
        self.offset = 0;
        true
    }

    /// Go back to the parent level. Returns `false` at the root.
    pub fn leave_detail(&mut self) -> bool {
        if self.frames.len() <= 1 {
            return false;
        }
        self.frames.pop();
        self.offset = 0;
        true
    }

    /// Scroll so that the cursor is visible in a window of `height` rows.
    ///
    /// Called after every cursor move; `height` can change at any time because
    /// the terminal can be resized while the menu is open.
    pub fn scroll_into_view(&mut self, height: usize) {
        if height == 0 {
            self.offset = 0;
            return;
        }
        let cursor = self.cursor();
        if cursor < self.offset {
            self.offset = cursor;
        } else if cursor >= self.offset + height {
            self.offset = cursor + 1 - height;
        }

        // A shrinking list (or a growing window) can leave a gap at the
        // bottom; pull the window back so the last row stays in use.
        let len = self.items().len();
        let max_offset = len.saturating_sub(height);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn items(n: usize) -> Vec<MenuItem> {
        (0..n)
            .map(|i| MenuItem::command(format!("item {i}"), format!("echo {i}")))
            .collect()
    }

    fn arg(name: &str) -> crate::config::ArgSpec {
        crate::config::ArgSpec {
            name: name.to_string(),
            prompt: None,
            default: None,
        }
    }

    fn state(n: usize) -> MenuState {
        MenuState::new(Frame::new("root", items(n)))
    }

    #[test]
    fn moves_within_bounds() {
        let mut s = state(3);
        assert_eq!(s.cursor(), 0);
        s.move_up();
        assert_eq!(s.cursor(), 0, "must not move above the first entry");
        s.move_down();
        s.move_down();
        assert_eq!(s.cursor(), 2);
        s.move_down();
        assert_eq!(s.cursor(), 2, "must not move past the last entry");
    }

    #[test]
    fn jumps_to_first_and_last() {
        let mut s = state(5);
        s.move_last();
        assert_eq!(s.cursor(), 4);
        s.move_first();
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn handles_an_empty_menu() {
        let mut s = state(0);
        s.move_down();
        s.move_last();
        assert_eq!(s.cursor(), 0);
        assert!(s.selected().is_none());
        assert!(s.is_empty());
    }

    #[test]
    fn opens_and_leaves_a_submenu_keeping_the_parent_cursor() {
        let mut parent = MenuItem::command("parent", "true");
        parent.submenu = items(2);
        let mut s = MenuState::new(Frame::new(
            "root",
            vec![MenuItem::command("a", "true"), parent],
        ));

        s.move_down();
        assert_eq!(s.cursor(), 1);
        assert!(s.enter_detail());
        assert_eq!(s.depth(), 2);
        // The parent has a command of its own, so the detail view is
        // "Run: parent" plus the two submenu entries.
        assert_eq!(s.items().len(), 3);
        assert_eq!(s.items()[0].label(), "Run: parent");

        s.move_down();
        assert!(s.leave_detail());
        assert_eq!(s.depth(), 1);
        assert_eq!(s.cursor(), 1, "the parent cursor is restored");
    }

    #[test]
    fn the_detail_views_run_row_keeps_the_entrys_directory() {
        // The row is the same command reached the long way round. Losing the
        // directory here would run an ancestor's entry in the wrong place,
        // but only when it was started from the detail view.
        let mut parent = MenuItem::command("parent", "true");
        parent.help = Some("has a detail view".into());
        parent.cwd = Some(PathBuf::from("/tmp/project"));
        let mut s = MenuState::new(Frame::new("root", vec![parent]));

        assert!(s.enter_detail());
        assert_eq!(s.items()[0].label(), "Run: parent");
        assert_eq!(s.items()[0].cwd.as_deref(), Some(Path::new("/tmp/project")));
    }

    #[test]
    fn refuses_to_open_an_entry_with_no_detail() {
        let mut s = state(1);
        assert!(!s.enter_detail());
        assert_eq!(s.depth(), 1);
    }

    #[test]
    fn opens_an_entry_that_only_has_help() {
        let mut item = MenuItem::command("a", "true");
        item.help = Some("what this does".into());
        let mut s = MenuState::new(Frame::new("root", vec![item]));

        assert!(s.enter_detail());
        assert_eq!(s.frame().help.as_deref(), Some("what this does"));
        assert_eq!(s.items().len(), 1, "the entry's own command stays runnable");
    }

    #[test]
    fn does_not_open_an_entry_that_only_has_arguments() {
        // Arguments are prompted for when the entry runs, so there is nothing
        // for a detail view to add.
        let mut item = MenuItem::command("search", "rg {pattern}");
        item.args = vec![arg("pattern")];
        let mut s = MenuState::new(Frame::new("root", vec![item]));

        assert!(!s.enter_detail());
        assert_eq!(s.depth(), 1);
    }

    #[test]
    fn the_run_line_of_a_detail_view_carries_the_arguments_but_cannot_recurse() {
        let mut item = MenuItem::command("search", "rg {pattern}");
        item.help = Some("Search the repository.".into());
        item.args = vec![arg("pattern")];
        let mut s = MenuState::new(Frame::new("root", vec![item]));

        assert!(s.enter_detail());
        let run = &s.items()[0];
        assert_eq!(run.args.len(), 1, "the arguments are carried over");
        assert!(!run.has_detail(), "otherwise the view would nest forever");
    }

    #[test]
    fn refuses_to_leave_the_root() {
        let mut s = state(1);
        assert!(!s.leave_detail());
        assert_eq!(s.depth(), 1);
    }

    #[test]
    fn scrolls_the_window_to_follow_the_cursor() {
        let mut s = state(10);
        s.scroll_into_view(3);
        assert_eq!(s.offset(), 0);

        for _ in 0..3 {
            s.move_down();
            s.scroll_into_view(3);
        }
        assert_eq!(s.cursor(), 3);
        assert_eq!(s.offset(), 1, "the window follows the cursor downwards");

        s.move_first();
        s.scroll_into_view(3);
        assert_eq!(s.offset(), 0, "the window follows the cursor upwards");
    }

    #[test]
    fn pulls_the_window_back_when_it_grows_past_the_end() {
        let mut s = state(10);
        s.move_last();
        s.scroll_into_view(3);
        assert_eq!(s.offset(), 7);

        // The terminal was made taller: the whole list now fits.
        s.scroll_into_view(20);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn tolerates_a_zero_height_window() {
        let mut s = state(10);
        s.move_last();
        s.scroll_into_view(0);
        assert_eq!(s.offset(), 0);
    }

    fn named(labels: &[&str]) -> MenuState {
        let items = labels
            .iter()
            .map(|label| MenuItem::command(*label, "echo hi"))
            .collect();
        MenuState::new(Frame::new("root", items))
    }

    fn labels(s: &MenuState) -> Vec<String> {
        s.items().iter().map(|item| item.label()).collect()
    }

    #[test]
    fn a_query_keeps_only_the_matching_entries_in_order() {
        let mut s = named(&["build", "deploy", "deploy staging", "test"]);
        s.set_query("deploy".into());
        assert_eq!(labels(&s), ["deploy", "deploy staging"]);
        assert_eq!(s.selected().map(|i| i.label()).as_deref(), Some("deploy"));
    }

    #[test]
    fn a_query_matches_regardless_of_case() {
        let mut s = named(&["Build", "DEPLOY"]);
        s.set_query("depl".into());
        assert_eq!(labels(&s), ["DEPLOY"]);
    }

    #[test]
    fn the_cursor_goes_to_the_first_match() {
        let mut s = named(&["build", "deploy", "test"]);
        s.move_last();
        assert_eq!(s.cursor(), 2);
        s.set_query("deploy".into());
        // Without this the cursor would still be at 2, which no longer exists.
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.selected().map(|i| i.label()).as_deref(), Some("deploy"));
    }

    #[test]
    fn moving_stays_inside_the_matches() {
        let mut s = named(&["build", "deploy", "deploy staging", "test"]);
        s.set_query("deploy".into());
        s.move_down();
        assert_eq!(
            s.selected().map(|i| i.label()).as_deref(),
            Some("deploy staging")
        );
        s.move_down();
        assert_eq!(
            s.selected().map(|i| i.label()).as_deref(),
            Some("deploy staging"),
            "must not walk past the last match into a filtered-out entry"
        );
    }

    #[test]
    fn a_query_that_matches_nothing_leaves_an_empty_list() {
        let mut s = named(&["build", "deploy"]);
        s.set_query("zzz".into());
        assert!(s.is_empty());
        assert!(s.selected().is_none());
        // Enter on an empty result must not do anything, not pick entry 0.
        s.move_down();
        assert!(s.selected().is_none());
    }

    #[test]
    fn clearing_the_query_keeps_the_entry_the_cursor_is_on() {
        let mut s = named(&["build", "deploy", "test"]);
        s.set_query("e".into());
        // "build" has no e, so the matches are deploy and test
        assert_eq!(labels(&s), ["deploy", "test"]);
        s.move_down();
        assert_eq!(s.selected().map(|i| i.label()).as_deref(), Some("test"));

        s.clear_query();
        assert_eq!(labels(&s), ["build", "deploy", "test"]);
        assert_eq!(
            s.selected().map(|i| i.label()).as_deref(),
            Some("test"),
            "clearing shows the rest again without moving off the entry"
        );
    }

    #[test]
    fn clearing_an_empty_query_leaves_the_cursor_alone() {
        // Backspacing out of a search that was never typed into goes through
        // here, and moving the selection on the way out would be a surprise.
        let mut s = named(&["build", "deploy", "test"]);
        s.move_last();
        s.clear_query();
        assert_eq!(s.selected().map(|i| i.label()).as_deref(), Some("test"));
    }

    #[test]
    fn a_submenu_starts_unfiltered_and_going_back_restores_the_filter() {
        let parent = MenuItem {
            title: Some("deploy".into()),
            submenu: vec![
                MenuItem::command("staging", "echo staging"),
                MenuItem::command("production", "echo production"),
            ],
            ..Default::default()
        };
        let mut s = MenuState::new(Frame::new(
            "root",
            vec![MenuItem::command("build", "echo build"), parent],
        ));

        s.set_query("deploy".into());
        assert_eq!(labels(&s), ["deploy"]);
        assert!(s.enter_detail());

        // The filter belongs to the level it was typed on.
        assert_eq!(s.query(), "");
        assert_eq!(labels(&s), ["staging", "production"]);

        assert!(s.leave_detail());
        assert_eq!(s.query(), "deploy");
        assert_eq!(labels(&s), ["deploy"]);
    }
}
