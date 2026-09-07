//! Built-in launchers.
//!
//! When a project already describes its own tasks (npm scripts, make targets,
//! Cargo, Gradle) there is no reason to repeat them in a j-menu file, so they
//! are picked up automatically. Everything here is best effort: an entry is
//! only produced when it can be run as-is, because a menu entry that always
//! fails is worse than a missing one.

pub mod cargo;
pub mod gradle;
pub mod makefile;
pub mod package_json;

use std::path::{Path, PathBuf};

use crate::config::MenuItem;
use crate::config::model::AutoLaunchers;

/// A group of entries contributed by one launcher.
pub struct LauncherGroup {
    /// Human readable source, e.g. `package.json`.
    pub source: String,
    pub items: Vec<MenuItem>,
}

/// Scan `start_dir` and its ancestors for supported project files.
pub fn discover(start_dir: &Path, enabled: &AutoLaunchers) -> Vec<LauncherGroup> {
    let mut groups = Vec::new();

    if enabled.package_json()
        && let Some(path) = find_up(start_dir, &["package.json"])
    {
        groups.extend(package_json::scan(&path));
    }
    if enabled.makefile()
        && let Some(path) = find_up(start_dir, &["Makefile", "makefile", "GNUmakefile"])
    {
        groups.extend(makefile::scan(&path, start_dir));
    }
    if enabled.cargo()
        && let Some(path) = find_up(start_dir, &["Cargo.toml"])
    {
        groups.extend(cargo::scan(&path));
    }
    if enabled.gradle() {
        // The wrapper is looked up separately from the build script: in a
        // multi-project build `gradlew` lives at the root while the
        // subproject only has its own `build.gradle`. A combined search would
        // stop at the subproject and fall back to a global `gradle`, which
        // fails on wrapper-only setups.
        let wrapper = find_up(start_dir, &["gradlew"]);
        let script = find_up(start_dir, &["build.gradle", "build.gradle.kts"]);
        if wrapper.is_some() || script.is_some() {
            groups.extend(gradle::scan(
                wrapper.as_deref(),
                script.as_deref(),
                start_dir,
            ));
        }
    }

    groups
}

/// The nearest ancestor of `start_dir` (inclusive) holding one of `names`.
///
/// `names` is ordered, so a directory holding several of them resolves to the
/// first one listed.
pub fn find_up(start_dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut dir = Some(start_dir);
    while let Some(current) = dir {
        for name in names {
            let path = current.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
        dir = current.parent();
    }
    None
}

/// Prefix a command with `cd` when the project lives outside the working
/// directory, so entries found in an ancestor still run in the right place.
///
/// For a launcher's own command, which is a single one. A configured entry can
/// be a whole script and needs `crate::in_dir_script` instead.
///
/// The path is quoted with single quotes; an embedded single quote is escaped
/// the POSIX way (`'\''`).
pub fn in_dir(dir: &Path, command: &str, start_dir: &Path) -> String {
    if dir == start_dir {
        return command.to_string();
    }
    format!("cd {} && {command}", quote(&dir.to_string_lossy()))
}

/// Quote a string for POSIX shells.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_posix_shells() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("with space"), "'with space'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn skips_the_cd_prefix_when_already_in_the_directory() {
        let dir = Path::new("/tmp/project");
        assert_eq!(in_dir(dir, "make build", dir), "make build");
    }

    #[test]
    fn prefixes_cd_when_the_project_is_in_an_ancestor() {
        assert_eq!(
            in_dir(
                Path::new("/tmp/project"),
                "make build",
                Path::new("/tmp/project/sub")
            ),
            "cd '/tmp/project' && make build"
        );
    }
}
