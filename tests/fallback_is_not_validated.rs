//! A configuration file that is going to be skipped must not be able to abort
//! startup with an error in a part of it nobody reads.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("j-menu")
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j-menu-fallback-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the temporary directory");
    dir
}

fn show_config(dir: &Path, home: &Path) -> std::process::Output {
    Command::new(bin())
        .arg("--show-config")
        .arg("--cwd")
        .arg(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .output()
        .expect("run j-menu")
}

#[test]
fn an_invalid_fallback_file_does_not_abort_startup() {
    let root = tempdir("invalid-but-skipped");
    let empty_home = root.join(".empty-home");
    fs::create_dir_all(&empty_home).unwrap();
    let nested = root.join("a");
    fs::create_dir_all(&nested).unwrap();

    // The ancestor is marked as a fallback and has an unknown key. Since the
    // nested file applies, the ancestor is inactive and its contents are never
    // used, so the unknown key must not matter.
    fs::write(
        root.join(".j-menu.yaml"),
        "merge: false\nmenu:\n  - titel: typo\n",
    )
    .unwrap();
    fs::write(
        nested.join(".j-menu.yaml"),
        "menu:\n  - title: t\n    shell: 'true'\n",
    )
    .unwrap();

    let output = show_config(&nested, &empty_home);

    assert!(
        output.status.success(),
        "startup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("menu entries from configuration: 1"),
        "{stdout}"
    );
}

#[test]
fn an_invalid_file_that_actually_applies_still_reports_the_error() {
    let root = tempdir("invalid-and-applies");
    let empty_home = root.join(".empty-home");
    fs::create_dir_all(&empty_home).unwrap();

    // Same file, but now nothing nearer exists, so it does apply and the
    // unknown key has to be reported.
    fs::write(
        root.join(".j-menu.yaml"),
        "merge: false\nmenu:\n  - titel: typo\n",
    )
    .unwrap();

    let output = show_config(&root, &empty_home);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("titel"), "{stderr}");
}
