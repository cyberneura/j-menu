//! Entries from the `scripts` block of a `package.json`.
//!
//! npm, pnpm, yarn and bun all resolve the package root themselves, so the
//! generated commands do not need a `cd` prefix.

use std::path::Path;

use super::{LauncherGroup, quote};
use crate::config::MenuItem;

/// Read `package.json` and turn its `scripts` into menu entries.
pub fn scan(path: &Path) -> Option<LauncherGroup> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let scripts = json.get("scripts")?.as_object()?;
    if scripts.is_empty() {
        return None;
    }

    let dir = path.parent()?;
    let runner = detect_package_manager(dir);

    // serde_json preserves document order only with the `preserve_order`
    // feature, which is not enabled, so sort for a stable menu instead.
    let mut names: Vec<&String> = scripts.keys().collect();
    names.sort();

    let items: Vec<MenuItem> = names
        .into_iter()
        // A name starting with `-` is parsed as an option by every one of
        // these runners, whatever the shell does with it: `npm run --silent`
        // is npm's own flag and exits without running the script. None of them
        // has a way to say "this is a script name", so the entry is left out
        // rather than offered as one that quietly does nothing.
        .filter(|name| !name.starts_with('-'))
        .map(|name| {
            // A script name is an arbitrary JSON key, so it can contain shell
            // metacharacters. Quoting keeps `build; rm -rf /` a single
            // argument that npm simply fails to find.
            MenuItem::command(
                format!("{runner} run {name}"),
                format!("{runner} run {}", quote(name)),
            )
        })
        .collect();
    if items.is_empty() {
        return None;
    }

    Some(LauncherGroup {
        source: "package.json".to_string(),
        items,
    })
}

/// Pick the package manager from the nearest lock file.
///
/// Running `npm run` in a pnpm workspace usually still works but can resolve
/// different binaries, so the lock file is the more reliable signal.
///
/// The search walks up from `dir`, because in a workspace the lock file lives
/// at the root while each package has its own `package.json`.
///
/// It stops at the first boundary it meets — the repository root (a directory
/// holding `.git`), the user's home directory, or the filesystem root — so an
/// unrelated lock file somewhere above the project cannot decide the package
/// manager. The home directory is checked too because not every project is a
/// git repository, and stopping only at `.git` would then walk all the way up.
fn detect_package_manager(dir: &Path) -> &'static str {
    const LOCKFILES: &[(&str, &str)] = &[
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("package-lock.json", "npm"),
    ];

    let home = dirs::home_dir();
    let mut current = Some(dir);

    while let Some(here) = current {
        for (lockfile, runner) in LOCKFILES {
            if here.join(lockfile).is_file() {
                return runner;
            }
        }
        // `.git` is a directory in a normal clone and a file in a worktree or
        // submodule, so test for either.
        if here.join(".git").exists() || home.as_deref() == Some(here) {
            break;
        }
        current = here.parent();
    }

    // Nothing found: npm is the one that is always present with Node.
    "npm"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-pkg-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn turns_scripts_into_entries_sorted_by_name() {
        let dir = tempdir("scripts");
        let path = dir.join("package.json");
        fs::write(
            &path,
            r#"{"scripts": {"test": "vitest", "build": "vite build"}}"#,
        )
        .unwrap();

        let group = scan(&path).unwrap();
        let labels: Vec<String> = group.items.iter().map(|i| i.label()).collect();
        assert_eq!(labels, vec!["npm run build", "npm run test"]);
    }

    #[test]
    fn skips_a_script_whose_name_would_be_read_as_an_option() {
        // `npm run --silent` is npm's own flag: it exits 0 without running
        // the script, and no runner has a way to say "this is a name".
        let dir = tempdir("option-like");
        let path = dir.join("package.json");
        fs::write(
            &path,
            r#"{"scripts": {"--silent": "echo hi", "build": "vite build"}}"#,
        )
        .unwrap();

        let group = scan(&path).unwrap();
        let labels: Vec<String> = group.items.iter().map(|i| i.label()).collect();
        assert_eq!(labels, vec!["npm run build"]);
    }

    #[test]
    fn produces_nothing_when_every_script_is_option_like() {
        let dir = tempdir("all-option-like");
        let path = dir.join("package.json");
        fs::write(&path, r#"{"scripts": {"--silent": "echo hi"}}"#).unwrap();
        assert!(scan(&path).is_none());
    }

    #[test]
    fn uses_the_package_manager_from_the_lock_file() {
        let dir = tempdir("pnpm");
        let path = dir.join("package.json");
        fs::write(&path, r#"{"scripts": {"dev": "vite"}}"#).unwrap();
        fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();

        let group = scan(&path).unwrap();
        assert_eq!(group.items[0].script().unwrap(), "pnpm run 'dev'");
    }

    #[test]
    fn finds_the_lock_file_at_the_workspace_root() {
        let root = tempdir("workspace");
        let package = root.join("packages/app");
        fs::create_dir_all(&package).unwrap();
        fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        let path = package.join("package.json");
        fs::write(&path, r#"{"scripts": {"dev": "vite"}}"#).unwrap();

        let group = scan(&path).unwrap();
        assert_eq!(group.items[0].script().unwrap(), "pnpm run 'dev'");
    }

    #[test]
    fn does_not_look_for_a_lock_file_outside_the_repository() {
        let outside = tempdir("outside-repo");
        fs::write(outside.join("pnpm-lock.yaml"), "").unwrap();
        let repo = outside.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let path = repo.join("package.json");
        fs::write(&path, r#"{"scripts": {"dev": "vite"}}"#).unwrap();

        let group = scan(&path).unwrap();
        assert_eq!(
            group.items[0].script().unwrap(),
            "npm run 'dev'",
            "the lock file above the repository must not be used"
        );
    }

    #[test]
    fn quotes_a_script_name_that_carries_shell_metacharacters() {
        // The name comes from a file in the repository, not from the person
        // running the menu, so it must not be able to add a command.
        let dir = tempdir("injection");
        let path = dir.join("package.json");
        fs::write(&path, r#"{"scripts": {"build; touch /tmp/pwned": "x"}}"#).unwrap();

        let group = scan(&path).unwrap();
        assert_eq!(
            group.items[0].script().unwrap(),
            "npm run 'build; touch /tmp/pwned'"
        );
    }

    #[test]
    fn produces_nothing_without_scripts() {
        let dir = tempdir("noscripts");
        let path = dir.join("package.json");
        fs::write(&path, r#"{"name": "x"}"#).unwrap();
        assert!(scan(&path).is_none());
    }

    #[test]
    fn produces_nothing_for_an_empty_scripts_block() {
        let dir = tempdir("emptyscripts");
        let path = dir.join("package.json");
        fs::write(&path, r#"{"scripts": {}}"#).unwrap();
        assert!(scan(&path).is_none());
    }

    #[test]
    fn ignores_a_broken_package_json_instead_of_failing() {
        let dir = tempdir("broken");
        let path = dir.join("package.json");
        fs::write(&path, "{not json").unwrap();
        assert!(scan(&path).is_none());
    }
}
