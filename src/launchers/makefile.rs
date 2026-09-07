//! Entries from the targets of a `Makefile`.
//!
//! The Makefile is scanned line by line rather than handed to `make -qp`,
//! because running make just to list targets can execute `$(shell ...)` in the
//! file. Reading is cheap and cannot have side effects.

use std::path::Path;

use super::{LauncherGroup, in_dir, quote};
use crate::config::MenuItem;

/// Read a Makefile and turn its targets into menu entries.
///
/// `start_dir` is the directory the menu was opened in; when the Makefile
/// lives in an ancestor, the command is prefixed with `cd` (unlike npm or
/// cargo, `make` does not search upwards).
pub fn scan(path: &Path, start_dir: &Path) -> Option<LauncherGroup> {
    let text = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;

    let mut targets = Vec::new();
    for name in parse_targets(&text) {
        if !targets.contains(&name) {
            targets.push(name);
        }
    }
    if targets.is_empty() {
        return None;
    }

    let items = targets
        .into_iter()
        .map(|target| {
            // Quoting protects the shell, not make: a target named `-n` is
            // still read as make's own dry-run flag. `--` ends the options,
            // and is only added where it is needed so the common command
            // stays the one you would have typed.
            let options_end = if target.starts_with('-') { "-- " } else { "" };
            // A target name can hold shell metacharacters, and this one came
            // from a file in the repository rather than from the menu author.
            let command = in_dir(
                dir,
                &format!("make {options_end}{}", quote(&target)),
                start_dir,
            );
            MenuItem::command(format!("make {target}"), command)
        })
        .collect();

    Some(LauncherGroup {
        source: "Makefile".to_string(),
        items,
    })
}

/// Target names that can be run without extra arguments.
///
/// Skipped on purpose:
///
/// - special targets (`.PHONY`, `.DEFAULT_GOAL`, ...) — not runnable
/// - pattern rules (`%.o: %.c`) — need a concrete file name
/// - names built from variables (`$(BIN): ...`) — the value is unknown here
/// - variable assignments (`CC := gcc`) and recipe lines, which start with the
///   recipe prefix: a tab, or whatever `.RECIPEPREFIX` was last set to
/// - the body of a `define`, which make does not read as makefile syntax
///   until the variable is expanded, so a `fake:` line in there is text
/// - anything inside a conditional (`ifeq` ... `endif`). Which branch make
///   takes depends on variables this cannot expand, so *both* are skipped:
///   offering a rule from the branch that was not taken would be a menu entry
///   that always fails, which is the one thing worth avoiding here
fn parse_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_define = false;
    // Nesting depth of `ifeq` / `ifdef` / ... blocks, whose contents are
    // skipped entirely.
    let mut conditional_depth = 0usize;
    // `.RECIPEPREFIX` applies from where it is set, so it is tracked while
    // reading rather than looked up once.
    let mut recipe_prefix = '\t';

    for line in text.lines() {
        let start = line.trim_start();
        if in_define {
            in_define = !start.starts_with("endef");
            continue;
        }
        if is_conditional_start(start) {
            conditional_depth += 1;
            continue;
        }
        if start.starts_with("endif") {
            conditional_depth = conditional_depth.saturating_sub(1);
            continue;
        }
        if conditional_depth > 0 {
            continue;
        }
        let start = start
            .strip_prefix("override ")
            .unwrap_or(start)
            .trim_start();
        if start.starts_with("define ") || start == "define" {
            in_define = true;
            continue;
        }
        if let Some(prefix) = recipe_prefix_assignment(start) {
            recipe_prefix = prefix;
            continue;
        }
        if line.starts_with(recipe_prefix) || start.starts_with('#') {
            continue;
        }
        let Some((left, right)) = line.split_once(':') else {
            continue;
        };
        // `::` is a double-colon rule; `:=` and friends are assignments.
        let right = right.strip_prefix(':').unwrap_or(right);
        if right.starts_with('=') {
            continue;
        }
        // A `=` before the colon means this is an assignment such as `A ?= b`.
        if left.contains('=') {
            continue;
        }

        for name in left.split_whitespace() {
            // `a b &:` is one grouped rule building both a and b; the `&` is
            // the marker, not a third target.
            if name.starts_with('.') || name.contains('%') || name.contains('$') || name == "&" {
                continue;
            }
            targets.push(name.to_string());
        }
    }

    targets
}

/// Whether this line opens a conditional block.
///
/// `else ifeq (...)` continues the block it is already in rather than opening
/// a new one, so only the leading keyword counts.
fn is_conditional_start(line: &str) -> bool {
    ["ifeq", "ifneq", "ifdef", "ifndef"]
        .iter()
        .any(|keyword| match line.strip_prefix(keyword) {
            // `ifeq(a,b)` with no space is valid, so the next character only
            // has to not continue the word.
            Some(rest) => !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'),
            None => false,
        })
}

/// The recipe prefix a `.RECIPEPREFIX` assignment sets, if this line is one.
///
/// make takes the first character of the value, and an empty value puts the
/// tab back.
fn recipe_prefix_assignment(line: &str) -> Option<char> {
    let rest = line.strip_prefix(".RECIPEPREFIX")?;
    // `=`, `:=`, `::=`, `?=` and `+=` all end in the `=` that starts the value.
    let value = rest.trim_start().split_once('=')?.1;
    Some(value.trim_start().chars().next().unwrap_or('\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_plain_targets() {
        let targets = parse_targets("build:\n\tcargo build\n\ntest: build\n\tcargo test\n");
        assert_eq!(targets, vec!["build", "test"]);
    }

    #[test]
    fn collects_every_name_of_a_multi_target_rule() {
        assert_eq!(parse_targets("a b c:\n\techo hi\n"), vec!["a", "b", "c"]);
    }

    #[test]
    fn skips_special_targets() {
        assert_eq!(
            parse_targets(".PHONY: build\nbuild:\n\ttrue\n"),
            vec!["build"]
        );
    }

    #[test]
    fn skips_the_body_of_a_define() {
        // make does not read a `define` body as makefile syntax until the
        // variable is expanded, so a colon in there is not a rule.
        let targets = parse_targets("define recipe\nfake:\n\techo hi\nendef\n\nbuild:\n\ttrue\n");
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn skips_the_body_of_an_assigning_or_overridden_define() {
        let targets = parse_targets("override define recipe :=\nfake:\nendef\nbuild:\n\ttrue\n");
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn honours_a_recipe_prefix_other_than_tab() {
        // With `.RECIPEPREFIX = >`, `>echo fake:` is a recipe line, not a rule.
        let targets = parse_targets(".RECIPEPREFIX = >\nbuild:\n>echo fake:\n>true\n");
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn treats_a_tab_as_an_ordinary_line_once_the_prefix_has_changed() {
        // make only strips the active prefix, so a tab is plain indentation
        // and the rule written after it is a real one.
        let targets = parse_targets(".RECIPEPREFIX = >\n\treal:\n>true\n");
        assert_eq!(targets, vec!["real"]);
    }

    #[test]
    fn an_empty_recipe_prefix_assignment_restores_the_tab() {
        let targets = parse_targets(".RECIPEPREFIX = >\n.RECIPEPREFIX =\nbuild:\n\tfake:\n");
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn skips_rules_inside_a_conditional() {
        // Which branch make takes depends on variables this cannot expand, so
        // neither branch is advertised.
        let targets = parse_targets("build:\n\ttrue\nifeq (1,0)\ninactive:\n\ttrue\nendif\n");
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn skips_both_arms_of_a_conditional_and_resumes_after_it() {
        let targets = parse_targets(
            "ifdef DEBUG\ndebug:\n\ttrue\nelse\nrelease:\n\ttrue\nendif\nafter:\n\ttrue\n",
        );
        assert_eq!(targets, vec!["after"]);
    }

    #[test]
    fn a_target_named_like_a_conditional_keyword_is_still_a_target() {
        // `ifeq` only opens a block when it is the keyword, not when it is the
        // start of a longer name.
        let targets = parse_targets("ifeqx:\n\ttrue\n");
        assert_eq!(targets, vec!["ifeqx"]);
    }

    #[test]
    fn drops_the_grouped_target_marker() {
        // `a b &:` builds both a and b in one rule; `&` is not a target.
        assert_eq!(parse_targets("a b &:\n\ttrue\n"), vec!["a", "b"]);
    }

    #[test]
    fn ends_the_options_for_a_target_that_looks_like_a_flag() {
        // Quoting stops the shell, not make: bare `make '-n'` is a dry run.
        let dir = std::env::temp_dir().join("j-menu-make-dash");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Makefile");
        std::fs::write(&path, "-n:\n\ttrue\nbuild:\n\ttrue\n").unwrap();

        let group = scan(&path, &dir).unwrap();
        let scripts: Vec<String> = group
            .items
            .iter()
            .map(|i| i.script().unwrap().to_string())
            .collect();
        assert_eq!(scripts, ["make -- '-n'", "make 'build'"]);
    }

    #[test]
    fn skips_pattern_rules_because_they_need_a_file_name() {
        assert!(parse_targets("%.o: %.c\n\t$(CC) -c $<\n").is_empty());
    }

    #[test]
    fn skips_targets_built_from_variables() {
        assert!(parse_targets("$(BIN): main.o\n\ttrue\n").is_empty());
    }

    #[test]
    fn skips_variable_assignments() {
        assert!(parse_targets("CC := gcc\nFLAGS ?= -O2\nX = 1\n").is_empty());
    }

    #[test]
    fn skips_recipe_lines_that_contain_a_colon() {
        assert!(parse_targets("build:\n\techo a:b\n").contains(&"build".to_string()));
        assert_eq!(parse_targets("build:\n\techo a:b\n").len(), 1);
    }

    #[test]
    fn skips_comments() {
        assert!(parse_targets("# note: this is a comment\n").is_empty());
    }

    #[test]
    fn quotes_the_target_name_in_the_generated_command() {
        let dir = std::env::temp_dir().join("j-menu-make-quote");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Makefile");
        std::fs::write(&path, "build:\n\ttrue\n").unwrap();

        let group = scan(&path, &dir).unwrap();
        assert_eq!(group.items[0].script().unwrap(), "make 'build'");
        assert_eq!(group.items[0].label(), "make build");
    }

    #[test]
    fn accepts_double_colon_rules() {
        assert_eq!(parse_targets("build::\n\ttrue\n"), vec!["build"]);
    }
}
