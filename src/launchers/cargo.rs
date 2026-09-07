//! Entries for a Cargo project.
//!
//! Cargo finds the manifest itself, so the commands need no `cd` prefix.
//! A `cargo clippy` entry that fails because the component is missing would be
//! worse than no entry at all, so the subcommands that ship as optional
//! toolchain components are offered only once they are known to be installed.

use std::path::Path;
use std::process::{Command, Stdio};

use super::{LauncherGroup, quote};
use crate::config::MenuItem;

/// Subcommands built into Cargo, available for any manifest.
const BUILTIN_COMMANDS: &[&str] = &["build", "test", "check"];

/// Subcommands provided by optional toolchain components (`rustfmt`,
/// `clippy`), which a minimal rustup installation leaves out.
const OPTIONAL_COMMANDS: &[&str] = &["fmt", "clippy"];

/// Produce the standard Cargo entries, plus one `cargo run --bin` per extra
/// binary target declared in the manifest.
pub fn scan(path: &Path) -> Option<LauncherGroup> {
    scan_with(path, is_installed)
}

/// `scan` with the availability check injected, so the tests do not depend on
/// which components the toolchain running them happens to have.
fn scan_with(path: &Path, is_installed: impl Fn(&str) -> bool + Sync) -> Option<LauncherGroup> {
    let text = std::fs::read_to_string(path).ok()?;
    let manifest: toml::Value = toml::from_str(&text).ok()?;

    // The checks are independent and each costs a process, so they overlap
    // rather than adding up while the menu is opening.
    let installed: Vec<&str> = std::thread::scope(|scope| {
        let is_installed = &is_installed;
        let probes: Vec<_> = OPTIONAL_COMMANDS
            .iter()
            .map(|&command| (command, scope.spawn(move || is_installed(command))))
            .collect();
        probes
            .into_iter()
            .filter_map(|(command, probe)| probe.join().unwrap_or(false).then_some(command))
            .collect()
    });

    let mut items: Vec<MenuItem> = BUILTIN_COMMANDS
        .iter()
        .copied()
        .chain(installed)
        .map(|c| MenuItem::command(format!("cargo {c}"), format!("cargo {c}")))
        .collect();

    // `cargo run` is only offered when it can actually resolve a target:
    //
    // - a virtual workspace or a library-only package has nothing to run
    // - two or more binaries make a bare `cargo run` ambiguous, so name each
    let bins = binary_targets(&manifest, path.parent());
    match bins.as_slice() {
        [] => {}
        [only] => items.push(MenuItem::command(
            "cargo run",
            format!("cargo run{}", features_flag(only)),
        )),
        many => {
            for bin in many {
                // The name comes from a manifest in the repository, so quote
                // it for the same reason npm script names are quoted.
                items.push(MenuItem::command(
                    format!("cargo run --bin {}", bin.name),
                    format!("cargo run --bin {}{}", quote(&bin.name), features_flag(bin)),
                ));
            }
        }
    }

    Some(LauncherGroup {
        source: "Cargo.toml".to_string(),
        items,
    })
}

/// Whether `cargo <command>` can actually run.
///
/// `cargo --list` is not enough to tell: rustup ships a `cargo-fmt` and a
/// `cargo-clippy` shim with every toolchain, so both are listed even when the
/// component behind them is missing and the command fails on use. Running the
/// shim is what settles it, and `--version` is the cheapest way to do that —
/// it builds nothing and does not even read the manifest.
///
/// It runs from the filesystem root rather than the project, because Cargo
/// reads `.cargo/config.toml` from the working directory upwards and an
/// `[alias]` there can point `fmt` at any other Cargo command. Opening a menu
/// inside a repository must not run something the repository chose.
///
/// That answers for the default toolchain, so a repository pinning a
/// different one through `rust-toolchain.toml` can still be told wrong. It is
/// the better trade: honouring the pin means either letting the repository's
/// Cargo configuration take effect, or handing the pinned name to rustup,
/// which installs a missing toolchain on the spot — a menu keystroke must not
/// start a download.
fn is_installed(command: &str) -> bool {
    Command::new("cargo")
        .arg(command)
        .arg("--version")
        .current_dir(Path::new("/"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A binary target Cargo can run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinTarget {
    name: String,
    /// `required-features` from the manifest. Without passing these, Cargo
    /// refuses to run the target and asks for `--features`.
    required_features: Vec<String>,
}

impl BinTarget {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required_features: Vec::new(),
        }
    }
}

/// The `--features` argument a target needs, or an empty string.
fn features_flag(bin: &BinTarget) -> String {
    if bin.required_features.is_empty() {
        return String::new();
    }
    format!(" --features {}", quote(&bin.required_features.join(",")))
}

/// Every binary Cargo would see for this manifest.
///
/// Counting only `[[bin]]` is not enough: Cargo also auto-discovers
/// `src/main.rs` and `src/bin/*.rs`. Missing those makes a library-only
/// package look runnable, and makes a package with `src/main.rs` plus one
/// `[[bin]]` look unambiguous when it is not.
///
/// A virtual workspace (no `[package]`) has no targets of its own.
fn binary_targets(manifest: &toml::Value, dir: Option<&Path>) -> Vec<BinTarget> {
    if manifest.get("package").is_none() {
        return Vec::new();
    }

    let entries = manifest
        .get("bin")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();

    let mut bins: Vec<BinTarget> = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            Some(BinTarget {
                name: name.to_string(),
                required_features: string_list(entry.get("required-features")),
            })
        })
        .collect();

    // Source files an explicit target already owns. Cargo does not
    // auto-discover a file that a `[[bin]]` points at, so a target declared as
    // `name = "renamed", path = "src/main.rs"` must not also produce an
    // implicit target named after the package — that target does not exist,
    // and `cargo run --bin <package>` would fail.
    let claimed: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.get("path")?.as_str().map(normalize_path))
        .collect();

    // `autobins = false` turns the auto-discovery off.
    let autobins = manifest
        .get("package")
        .and_then(|p| p.get("autobins"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let Some(dir) = dir.filter(|_| autobins) else {
        return bins;
    };

    // The implicit binary is named after the package.
    if dir.join("src/main.rs").is_file()
        && !claimed.iter().any(|p| p == "src/main.rs")
        && let Some(name) = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
        && !bins.iter().any(|b| b.name == name)
    {
        bins.push(BinTarget::new(name));
    }

    // `src/bin/foo.rs` and `src/bin/foo/main.rs` are binaries named `foo`.
    if let Ok(entries) = std::fs::read_dir(dir.join("src/bin")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let (name, source) = if path.is_dir() && path.join("main.rs").is_file() {
                (
                    path.file_name().map(|n| n.to_string_lossy().into_owned()),
                    path.join("main.rs"),
                )
            } else if path.extension().is_some_and(|e| e == "rs") {
                (
                    path.file_stem().map(|n| n.to_string_lossy().into_owned()),
                    path.clone(),
                )
            } else {
                (None, path.clone())
            };

            // Same rule as `src/main.rs`: a file an explicit target points at
            // is not auto-discovered.
            let relative = source
                .strip_prefix(dir)
                .ok()
                .map(|p| normalize_path(&p.to_string_lossy()));
            if relative.is_some_and(|r| claimed.contains(&r)) {
                continue;
            }
            if let Some(name) = name
                && !bins.iter().any(|b| b.name == name)
            {
                bins.push(BinTarget::new(name));
            }
        }
    }

    // Directory order is filesystem-dependent; sort for a stable menu.
    bins.sort_by(|a, b| a.name.cmp(&b.name));
    bins
}

/// Normalise a manifest path for comparison: `./src/main.rs` and
/// `src\\main.rs` both become `src/main.rs`.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Read a TOML array of strings, ignoring anything that is not one.
fn string_list(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_manifest(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-cargo-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cargo.toml");
        fs::write(&path, body).unwrap();
        path
    }

    /// A manifest plus the source files Cargo would auto-discover.
    fn write_package(name: &str, body: &str, sources: &[&str]) -> std::path::PathBuf {
        let path = write_manifest(name, body);
        let dir = path.parent().unwrap();
        for source in sources {
            let file = dir.join(source);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "fn main() {}").unwrap();
        }
        path
    }

    const PACKAGE: &str = "[package]\nname = \"x\"\nversion = \"0.1.0\"\n";

    /// Labels as seen on a toolchain with every optional component installed,
    /// so the tests below do not depend on the one running them.
    fn labels(path: &std::path::Path) -> Vec<String> {
        labels_with(path, |_| true)
    }

    fn labels_with(
        path: &std::path::Path,
        is_installed: impl Fn(&str) -> bool + Sync,
    ) -> Vec<String> {
        scan_with(path, is_installed)
            .unwrap()
            .items
            .iter()
            .map(|i| i.label())
            .collect()
    }

    #[test]
    fn offers_the_standard_subcommands() {
        let path = write_package("basic", PACKAGE, &["src/main.rs"]);
        let labels = labels(&path);
        assert!(labels.contains(&"cargo build".to_string()));
        assert!(labels.contains(&"cargo test".to_string()));
        assert!(labels.contains(&"cargo run".to_string()));
    }

    #[test]
    fn offers_the_optional_subcommands_only_when_they_are_installed() {
        let path = write_package("optional", PACKAGE, &["src/main.rs"]);

        let all = labels_with(&path, |_| true);
        assert!(all.contains(&"cargo fmt".to_string()));
        assert!(all.contains(&"cargo clippy".to_string()));

        // A minimal rustup toolchain: the shims exist, the components do not.
        let minimal = labels_with(&path, |_| false);
        assert!(!minimal.contains(&"cargo fmt".to_string()));
        assert!(!minimal.contains(&"cargo clippy".to_string()));
        // The built-in subcommands are never gated.
        assert!(minimal.contains(&"cargo build".to_string()));
        assert!(minimal.contains(&"cargo check".to_string()));
    }

    #[test]
    fn omits_run_for_a_virtual_workspace_where_it_cannot_resolve_a_target() {
        let path = write_manifest("virtual", "[workspace]\nmembers = [\"a\"]\n");
        assert!(!labels(&path).iter().any(|l| l.starts_with("cargo run")));
    }

    #[test]
    fn omits_run_for_a_library_only_package() {
        // No `src/main.rs`, no `src/bin/`, no `[[bin]]`: `cargo run` would
        // fail with "a bin target must be available".
        let path = write_package("lib-only", PACKAGE, &["src/lib.rs"]);
        assert!(!labels(&path).iter().any(|l| l.starts_with("cargo run")));
    }

    #[test]
    fn counts_the_implicit_binary_from_src_main_rs() {
        // `src/main.rs` plus one `[[bin]]` is two binaries, so a bare
        // `cargo run` would be ambiguous even though `bin` has one entry.
        let manifest = format!("{PACKAGE}\n[[bin]]\nname = \"other\"\npath = \"other.rs\"\n");
        let path = write_package("implicit-main", &manifest, &["src/main.rs"]);
        let labels = labels(&path);
        assert!(!labels.contains(&"cargo run".to_string()), "{labels:?}");
        assert!(
            labels.contains(&"cargo run --bin x".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"cargo run --bin other".to_string()),
            "{labels:?}"
        );
    }

    #[test]
    fn counts_binaries_auto_discovered_from_src_bin() {
        let path = write_package("src-bin", PACKAGE, &["src/bin/a.rs", "src/bin/b/main.rs"]);
        let labels = labels(&path);
        assert!(
            labels.contains(&"cargo run --bin a".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"cargo run --bin b".to_string()),
            "{labels:?}"
        );
    }

    #[test]
    fn honours_autobins_false() {
        let manifest = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nautobins = false\n\n\
             [[bin]]\nname = \"only\"\npath = \"only.rs\"\n";
        let path = write_package("autobins-off", manifest, &["src/main.rs"]);
        let labels = labels(&path);
        assert!(labels.contains(&"cargo run".to_string()), "{labels:?}");
    }

    #[test]
    fn names_binaries_explicitly_only_when_there_are_several() {
        let one = write_manifest(
            "one-bin",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"a\"\npath = \"a.rs\"\n",
        );
        let one_labels = labels(&one);
        assert!(!one_labels.iter().any(|l| l.contains("--bin")));
        assert!(one_labels.contains(&"cargo run".to_string()));

        let two = write_manifest(
            "two-bins",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"a\"\npath = \"a.rs\"\n\n[[bin]]\nname = \"b\"\npath = \"b.rs\"\n",
        );
        let two_labels = labels(&two);
        assert!(two_labels.contains(&"cargo run --bin a".to_string()));
        assert!(two_labels.contains(&"cargo run --bin b".to_string()));
        assert!(
            !two_labels.contains(&"cargo run".to_string()),
            "a bare `cargo run` is ambiguous with several binaries"
        );
    }

    #[test]
    fn quotes_a_binary_name_that_carries_shell_metacharacters() {
        let path = write_manifest(
            "bin-injection",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n             [[bin]]\nname = \"a; touch /tmp/pwned\"\npath = \"a.rs\"\n\n             [[bin]]\nname = \"b\"\npath = \"b.rs\"\n",
        );
        let scripts: Vec<String> = scan(&path)
            .unwrap()
            .items
            .iter()
            .filter_map(|i| i.script())
            .collect();
        assert!(
            scripts.contains(&"cargo run --bin 'a; touch /tmp/pwned'".to_string()),
            "{scripts:?}"
        );
    }

    #[test]
    fn does_not_invent_an_implicit_target_for_a_claimed_source_file() {
        // Cargo does not auto-discover a file an explicit [[bin]] points at,
        // so `src/main.rs` here belongs to "renamed" only. Inventing a
        // package-named target would produce `cargo run --bin x`, which fails.
        let manifest = format!("{PACKAGE}\n[[bin]]\nname = \"renamed\"\npath = \"src/main.rs\"\n");
        let path = write_package("renamed-main", &manifest, &["src/main.rs"]);
        let labels = labels(&path);
        assert!(
            !labels.iter().any(|l| l.contains("--bin x")),
            "no target named after the package exists: {labels:?}"
        );
        assert!(
            labels.contains(&"cargo run".to_string()),
            "one binary, so a bare run is unambiguous: {labels:?}"
        );
    }

    #[test]
    fn does_not_invent_an_implicit_target_for_a_claimed_src_bin_file() {
        let manifest =
            format!("{PACKAGE}\n[[bin]]\nname = \"renamed\"\npath = \"./src/bin/a.rs\"\n");
        let path = write_package("renamed-src-bin", &manifest, &["src/bin/a.rs"]);
        let labels = labels(&path);
        assert!(
            !labels.iter().any(|l| l.contains("--bin a")),
            "src/bin/a.rs belongs to \"renamed\": {labels:?}"
        );
    }

    #[test]
    fn passes_required_features_of_a_single_binary() {
        // Without `--features`, Cargo refuses to run the target and asks
        // for exactly these features.
        let manifest = format!(
            "{PACKAGE}\n[features]\ncli = []\n\n\
             [[bin]]\nname = \"x\"\npath = \"src/main.rs\"\nrequired-features = [\"cli\"]\n"
        );
        let path = write_package("required-features-one", &manifest, &["src/main.rs"]);
        let scripts: Vec<String> = scan(&path)
            .unwrap()
            .items
            .iter()
            .filter_map(|i| i.script())
            .collect();
        assert!(
            scripts.contains(&"cargo run --features 'cli'".to_string()),
            "{scripts:?}"
        );
    }

    #[test]
    fn passes_required_features_of_a_named_binary() {
        let manifest = format!(
            "{PACKAGE}\n[features]\ncli = []\nextra = []\n\n\
             [[bin]]\nname = \"a\"\npath = \"a.rs\"\nrequired-features = [\"cli\", \"extra\"]\n\n\
             [[bin]]\nname = \"b\"\npath = \"b.rs\"\n"
        );
        let path = write_package("required-features-many", &manifest, &[]);
        let scripts: Vec<String> = scan(&path)
            .unwrap()
            .items
            .iter()
            .filter_map(|i| i.script())
            .collect();
        assert!(
            scripts.contains(&"cargo run --bin 'a' --features 'cli,extra'".to_string()),
            "{scripts:?}"
        );
        assert!(
            scripts.contains(&"cargo run --bin 'b'".to_string()),
            "a target without required-features gets no flag: {scripts:?}"
        );
    }

    #[test]
    fn ignores_a_broken_manifest_instead_of_failing() {
        let path = write_manifest("broken", "this is not toml =");
        assert!(scan(&path).is_none());
    }
}
