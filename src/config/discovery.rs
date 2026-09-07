//! Locating configuration files.
//!
//! Files are searched in the current directory and every ancestor, nearest
//! first, followed by the per-user configuration directory. "Nearest first"
//! matters: entries defined closest to the working directory end up at the top
//! of the menu, and `merge: false` is evaluated against what was already
//! loaded.

use std::path::{Path, PathBuf};

/// Base names accepted for a project configuration file, in priority order.
///
/// `.local` variants come last so that, within a single directory, the shared
/// file is loaded before the personal override.
const PROJECT_STEMS: &[&str] = &[
    ".j-menu",
    "_j-menu",
    "j-menu",
    ".j-menu.local",
    "_j-menu.local",
    "j-menu.local",
];

/// Extensions accepted for a configuration file, in priority order.
///
/// `.cson` is intentionally absent: see README ("Unsupported formats").
const EXTENSIONS: &[&str] = &["yaml", "yml", "toml", "json"];

/// Base name of the per-user configuration file (under the config directory).
const USER_STEM: &str = "config";

/// Every candidate file name, in the order they should be loaded.
fn candidate_names(stems: &[&str]) -> Vec<String> {
    let mut names = Vec::with_capacity(stems.len() * EXTENSIONS.len());
    for stem in stems {
        for ext in EXTENSIONS {
            names.push(format!("{stem}.{ext}"));
        }
    }
    names
}

/// Configuration files found for `start_dir` and its ancestors, nearest first.
pub fn project_config_paths(start_dir: &Path) -> Vec<PathBuf> {
    let names = candidate_names(PROJECT_STEMS);
    let mut found = Vec::new();
    let mut dir = Some(start_dir);

    while let Some(current) = dir {
        for name in &names {
            let path = current.join(name);
            if path.is_file() {
                found.push(path);
            }
        }
        dir = current.parent();
    }

    found
}

/// The per-user configuration file, if present.
///
/// Looks under `$XDG_CONFIG_HOME/j-menu/` (or the platform equivalent).
pub fn user_config_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("j-menu");
    candidate_names(&[USER_STEM])
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Where a configuration file was found.
///
/// The loader needs the difference: a project file sits in the tree its
/// entries are about, while the per-user file belongs to no project at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The starting directory or one of its ancestors.
    Project,
    /// The per-user configuration directory.
    User,
}

/// A configuration file to load, and where it was found.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub scope: Scope,
}

/// All configuration files to load, in order.
pub fn all_config_paths(start_dir: &Path) -> Vec<Found> {
    let mut found: Vec<Found> = project_config_paths(start_dir)
        .into_iter()
        .map(|path| Found {
            path,
            scope: Scope::Project,
        })
        .collect();
    found.extend(user_config_path().map(|path| Found {
        path,
        scope: Scope::User,
    }));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_nothing_when_no_config_exists() {
        let dir = tempdir("empty");
        assert!(project_config_paths(&dir).is_empty());
    }

    #[test]
    fn finds_config_in_the_starting_directory() {
        let dir = tempdir("here");
        fs::write(dir.join(".j-menu.yaml"), "menu: []").unwrap();
        let found = project_config_paths(&dir);
        assert_eq!(found, vec![dir.join(".j-menu.yaml")]);
    }

    #[test]
    fn walks_up_to_ancestors_nearest_first() {
        let root = tempdir("ancestors");
        let nested = root.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("j-menu.yaml"), "menu: []").unwrap();
        fs::write(nested.join("j-menu.yaml"), "menu: []").unwrap();

        let found = project_config_paths(&nested);
        assert_eq!(found[0], nested.join("j-menu.yaml"));
        assert!(found.contains(&root.join("j-menu.yaml")));
    }

    #[test]
    fn loads_shared_file_before_local_override_in_one_directory() {
        let dir = tempdir("local-order");
        fs::write(dir.join(".j-menu.yaml"), "menu: []").unwrap();
        fs::write(dir.join(".j-menu.local.yaml"), "menu: []").unwrap();

        let found = project_config_paths(&dir);
        let shared = found
            .iter()
            .position(|p| p.ends_with(".j-menu.yaml"))
            .unwrap();
        let local = found
            .iter()
            .position(|p| p.ends_with(".j-menu.local.yaml"))
            .unwrap();
        assert!(shared < local);
    }

    #[test]
    fn cson_is_not_a_candidate() {
        let dir = tempdir("cson");
        fs::write(dir.join(".j-menu.cson"), "menu: []").unwrap();
        assert!(project_config_paths(&dir).is_empty());
    }
}
