//! End-to-end checks of configuration discovery and merging, driven through
//! the binary so that the documented behaviour is what is actually tested.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    // `cargo test` builds the binary next to the test executable.
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("j-menu")
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j-menu-it-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the temporary directory");
    dir
}

/// Run `j-menu --show-config` with the search starting at `dir`.
///
/// `HOME` and `XDG_CONFIG_HOME` are pointed at an empty directory so the
/// developer's own `~/.config/j-menu/` cannot change the result.
fn show_config(dir: &Path) -> String {
    let empty_home = dir.join(".empty-home");
    fs::create_dir_all(&empty_home).expect("create the fake home");
    show_config_with_home(dir, &empty_home)
}

/// The same, with the per-user configuration directory under `home`.
fn show_config_with_home(dir: &Path, home: &Path) -> String {
    let output = Command::new(bin())
        .arg("--show-config")
        .arg("--cwd")
        .arg(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .output()
        .expect("run j-menu");

    assert!(
        output.status.success(),
        "j-menu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn entry_count(report: &str) -> usize {
    report
        .lines()
        .find_map(|line| line.strip_prefix("menu entries from configuration: "))
        .expect("the report states the entry count")
        .trim()
        .parse()
        .expect("the entry count is a number")
}

fn one_entry(title: &str) -> String {
    format!("menu:\n  - title: {title}\n    shell: 'true'\n")
}

#[test]
fn merges_a_directory_with_its_ancestors() {
    let root = tempdir("merge-ancestors");
    let nested = root.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join(".j-menu.yaml"), one_entry("from root")).unwrap();
    fs::write(nested.join(".j-menu.yaml"), one_entry("from nested")).unwrap();

    let report = show_config(&nested);
    assert_eq!(entry_count(&report), 2, "{report}");

    // Nearest first: the nested file is loaded before the root one.
    let nested_at = report.find("a/b/.j-menu.yaml").expect("nested listed");
    let root_line = format!("{}/.j-menu.yaml", root.display());
    let root_at = report.find(&root_line).expect("root listed");
    assert!(nested_at < root_at, "{report}");
}

#[test]
fn skips_a_fallback_file_when_a_nearer_one_exists() {
    let root = tempdir("fallback-skipped");
    let nested = root.join("a");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        root.join(".j-menu.yaml"),
        format!("merge: false\n{}", one_entry("from root")),
    )
    .unwrap();
    fs::write(nested.join(".j-menu.yaml"), one_entry("from nested")).unwrap();

    let report = show_config(&nested);
    assert_eq!(entry_count(&report), 1, "{report}");
    let root_line = format!("{}/.j-menu.yaml", root.display());
    assert!(
        !report.contains(&root_line),
        "the fallback must not be listed as loaded: {report}"
    );
}

#[test]
fn uses_a_fallback_file_when_nothing_nearer_exists() {
    let root = tempdir("fallback-used");
    let nested = root.join("a");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        root.join(".j-menu.yaml"),
        format!("merge: false\n{}", one_entry("from root")),
    )
    .unwrap();

    let report = show_config(&nested);
    assert_eq!(entry_count(&report), 1, "{report}");
}

#[test]
fn loads_the_shared_file_before_the_local_override() {
    let dir = tempdir("local-override");
    fs::write(dir.join(".j-menu.yaml"), one_entry("shared")).unwrap();
    fs::write(dir.join(".j-menu.local.yaml"), one_entry("personal")).unwrap();

    let report = show_config(&dir);
    assert_eq!(entry_count(&report), 2, "{report}");
    let shared = report.find(".j-menu.yaml").expect("shared listed");
    let local = report.find(".j-menu.local.yaml").expect("local listed");
    assert!(shared < local, "{report}");
}

#[test]
fn reports_no_configuration_when_none_exists() {
    let dir = tempdir("none");
    let report = show_config(&dir);
    assert!(report.contains("no configuration file found"), "{report}");
    assert_eq!(entry_count(&report), 0, "{report}");
}

#[test]
fn reads_yaml_toml_and_json_alike() {
    let cases = [
        (".j-menu.yaml", "menu:\n  - title: t\n    shell: 'true'\n"),
        (
            ".j-menu.toml",
            "[[menu]]\ntitle = \"t\"\nshell = \"true\"\n",
        ),
        (
            ".j-menu.json",
            r#"{"menu": [{"title": "t", "shell": "true"}]}"#,
        ),
    ];

    for (file, body) in cases {
        let dir = tempdir(&format!("format-{file}"));
        fs::write(dir.join(file), body).unwrap();
        let report = show_config(&dir);
        assert_eq!(entry_count(&report), 1, "{file}: {report}");
    }
}

#[test]
fn fails_with_a_message_naming_the_broken_file() {
    let dir = tempdir("broken");
    let path = dir.join(".j-menu.yaml");
    fs::write(&path, "menu:\n  - titel: typo\n").unwrap();
    let empty_home = dir.join(".empty-home");
    fs::create_dir_all(&empty_home).unwrap();

    let output = Command::new(bin())
        .arg("--show-config")
        .arg("--cwd")
        .arg(&dir)
        .env("HOME", &empty_home)
        .env("XDG_CONFIG_HOME", &empty_home)
        .output()
        .expect("run j-menu");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(".j-menu.yaml"), "{stderr}");
    assert!(stderr.contains("titel"), "{stderr}");
}

/// The directory `--show-config` reports for the file whose path contains
/// `needle`.
fn runs_in(report: &str, needle: &str) -> String {
    const PREFIX: &str = "(entries run in ";
    const SUFFIX: &str = " by default)";

    let line = report
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is not listed: {report}"));
    let start = line
        .find(PREFIX)
        .unwrap_or_else(|| panic!("no run directory on {line:?}"));
    line[start + PREFIX.len()..]
        .strip_suffix(SUFFIX)
        .unwrap_or_else(|| panic!("no run directory on {line:?}"))
        .to_string()
}

/// `dir` as the binary reports it: it canonicalises what `--cwd` is given, and
/// a temporary directory is behind a symlink on some platforms.
fn resolved(dir: &Path) -> String {
    fs::canonicalize(dir)
        .expect("resolve the temporary directory")
        .display()
        .to_string()
}

#[test]
fn reports_the_directory_each_file_runs_in() {
    let root = tempdir("run-dir-reported");
    let nested = root.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join(".j-menu.yaml"), one_entry("from root")).unwrap();
    fs::write(nested.join(".j-menu.yaml"), one_entry("from nested")).unwrap();

    let report = show_config(&nested);
    assert_eq!(runs_in(&report, "a/b/.j-menu.yaml"), resolved(&nested));
    assert_eq!(
        runs_in(&report, &format!("{}/.j-menu.yaml", resolved(&root))),
        resolved(&root),
        "an ancestor's entries run at the ancestor, not here: {report}"
    );
}

#[test]
fn a_file_can_send_its_entries_back_to_the_working_directory() {
    let root = tempdir("run-dir-opt-out");
    let nested = root.join("a");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        root.join(".j-menu.yaml"),
        format!("run_in_current_directory: true\n{}", one_entry("from root")),
    )
    .unwrap();

    let report = show_config(&nested);
    let line = report
        .lines()
        .find(|line| line.contains(".j-menu.yaml"))
        .unwrap_or_else(|| panic!("the file is not listed: {report}"));
    assert!(
        line.contains("(entries run in the working directory by default)"),
        "{line}"
    );
}

#[test]
fn the_per_user_file_runs_in_the_working_directory_by_default() {
    // It belongs to no project, so running its entries in
    // ~/.config/j-menu would be useless for every one of them.
    let root = tempdir("user-config-default");
    let home = root.join("home");
    let user_dir = home.join("j-menu");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        user_dir.join("config.yaml"),
        one_entry("from the user file"),
    )
    .unwrap();

    let report = show_config_with_home(&root, &home);
    let line = report
        .lines()
        .find(|line| line.contains("j-menu/config.yaml"))
        .unwrap_or_else(|| panic!("the per-user file is not listed: {report}"));
    assert!(
        line.contains("(entries run in the working directory by default)"),
        "{line}"
    );
}

#[test]
fn the_per_user_file_can_still_ask_for_its_own_directory() {
    let root = tempdir("user-config-opts-in");
    let home = root.join("home");
    let user_dir = home.join("j-menu");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        user_dir.join("config.yaml"),
        format!(
            "run_in_current_directory: false\n{}",
            one_entry("from the user file")
        ),
    )
    .unwrap();

    let report = show_config_with_home(&root, &home);
    assert_eq!(
        runs_in(&report, "j-menu/config.yaml"),
        resolved(&user_dir),
        "{report}"
    );
}
