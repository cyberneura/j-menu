//! Reading configuration files and merging them into one menu.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::discovery;
use super::model::{AutoLaunchers, ConfigFile, MenuItem};

/// Checks that serde cannot express, run on every parsed file.
///
/// Only relationships *between* fields end up here; a field that is wrong on
/// its own is rejected by the derived `Deserialize`.
fn validate(items: &[MenuItem], path: &Path) -> Result<()> {
    for item in items {
        if item.shell.is_some() && !item.parallel.is_empty() {
            anyhow::bail!(
                "{}: entry {:?} has both `shell` and `parallel`; \
                 use one or the other (a `shell` list already runs sequentially)",
                path.display(),
                item.label()
            );
        }
        validate(&item.submenu, path)?;
    }
    Ok(())
}

/// One configuration file that was merged in.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    /// Where the entries of this file run unless one of them says otherwise;
    /// `None` is the directory `j-menu` was started from.
    pub cwd: Option<PathBuf>,
}

/// The merged configuration used to build the menu.
#[derive(Debug, Default)]
pub struct Config {
    pub menu: Vec<MenuItem>,
    pub auto_launchers: AutoLaunchers,
    /// Files that were actually merged, in load order. Reported by
    /// `--show-config`.
    pub sources: Vec<Source>,
}

/// Whether a file declares `merge: false`, read on its own.
///
/// Only this one key is looked at, and every other key is tolerated, so the
/// answer does not depend on the rest of the file being valid.
fn reads_as_fallback(path: &Path, text: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct MergeOnly {
        #[serde(default = "default_true")]
        merge: bool,
    }

    fn default_true() -> bool {
        true
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Each format has its own error type; only "did it say merge: false"
    // matters here, so collapse them all to an Option.
    let merge: Option<bool> = match ext.as_str() {
        "yaml" | "yml" => serde_yaml_ng::from_str::<MergeOnly>(text)
            .ok()
            .map(|m| m.merge),
        "toml" => toml::from_str::<MergeOnly>(text).ok().map(|m| m.merge),
        "json" => serde_json::from_str::<MergeOnly>(text)
            .ok()
            .map(|m| m.merge),
        _ => None,
    };

    // Unreadable even in this permissive shape: let the real parse report it.
    merge == Some(false)
}

/// Parse one configuration file, dispatching on the extension.
///
/// A bare top-level list is accepted as a shorthand for `menu:`, so the
/// shortest possible config is just a list of entries.
pub fn parse(path: &Path, text: &str) -> Result<ConfigFile> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let config = match ext.as_str() {
        "yaml" | "yml" => parse_yaml(text),
        "toml" => parse_toml(text),
        "json" => parse_json(text),
        other => anyhow::bail!("unsupported configuration format: .{other}"),
    }?;
    validate(&config.menu, path)?;
    Ok(config)
}

fn parse_yaml(text: &str) -> Result<ConfigFile> {
    match serde_yaml_ng::from_str::<ConfigFile>(text) {
        Ok(config) => Ok(config),
        Err(err) => match serde_yaml_ng::from_str::<Vec<MenuItem>>(text) {
            Ok(menu) => Ok(ConfigFile {
                menu,
                ..Default::default()
            }),
            // Report the error from the documented shape, not from the
            // shorthand: it is much more likely to point at the real mistake.
            Err(_) => Err(err.into()),
        },
    }
}

fn parse_toml(text: &str) -> Result<ConfigFile> {
    // TOML has no top-level array, so only the documented shape applies.
    Ok(toml::from_str::<ConfigFile>(text)?)
}

fn parse_json(text: &str) -> Result<ConfigFile> {
    match serde_json::from_str::<ConfigFile>(text) {
        Ok(config) => Ok(config),
        Err(err) => match serde_json::from_str::<Vec<MenuItem>>(text) {
            Ok(menu) => Ok(ConfigFile {
                menu,
                ..Default::default()
            }),
            Err(_) => Err(err.into()),
        },
    }
}

/// Record the directory each entry runs in.
///
/// `here` is what the file, or the enclosing entry, decided. An entry that
/// states `run_in_current_directory` itself overrides that for its own command
/// and for its submenu, which is what makes the setting readable at any depth:
/// it always describes the entry it is written on and everything under it.
fn assign_cwd(items: &mut [MenuItem], config_dir: &Path, here: bool) {
    for item in items {
        let here = item.run_in_current_directory.unwrap_or(here);
        item.cwd = (!here).then(|| config_dir.to_path_buf());
        assign_cwd(&mut item.submenu, config_dir, here);
    }
}

/// Load and merge every configuration file that applies to `start_dir`.
///
/// A file whose `merge` is `false` is skipped once anything has been loaded.
/// Because files arrive nearest-first, that makes such a file a fallback: it
/// contributes only when nothing closer to `start_dir` was found.
pub fn load(start_dir: &Path) -> Result<Config> {
    let mut config = Config::default();
    let mut auto_launchers: Option<AutoLaunchers> = None;

    for found in discovery::all_config_paths(start_dir) {
        let path = found.path;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        // Decide `merge` before validating anything else. A fallback file that
        // will be skipped must not be able to abort startup with an error in a
        // part of it nobody is going to read — that is the whole point of it
        // being inactive.
        if !config.sources.is_empty() && reads_as_fallback(&path, &text) {
            continue;
        }

        let mut file =
            parse(&path, &text).with_context(|| format!("failed to parse {}", path.display()))?;

        // The early check above already skipped this case, but keep the
        // condition intact so the rule survives if that fast path changes.
        if !file.merge && !config.sources.is_empty() {
            continue;
        }

        // The nearest file that sets `auto_launchers` at all takes the whole
        // block: the switches inside it are not merged with an ancestor's, so
        // a nearer `{makefile: false}` leaves an ancestor's `{cargo: false}`
        // with no effect. Merging them would mean a repository-wide file could
        // turn a launcher off in a directory that deliberately asked for it,
        // with nothing local saying so.
        if auto_launchers.is_none() {
            auto_launchers = file.auto_launchers;
        }

        // Entries run where they were written, so a command does not have to
        // start with a `cd` to find the project it is about. The per-user file
        // is the exception: it belongs to no project, and its entries are
        // written to be run wherever you happen to be.
        let config_dir = path.parent().unwrap_or(start_dir);
        let here = file
            .run_in_current_directory
            .unwrap_or(found.scope == discovery::Scope::User);
        assign_cwd(&mut file.menu, config_dir, here);

        config.menu.extend(file.menu);
        config.sources.push(Source {
            cwd: (!here).then(|| config_dir.to_path_buf()),
            path,
        });
    }

    config.auto_launchers = auto_launchers.unwrap_or_default();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(name: &str, text: &str) -> Result<ConfigFile> {
        parse(Path::new(name), text)
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-loader-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonical, because that is what `load` is given in `main` and what
        // the entries are compared against below.
        std::fs::canonicalize(&dir).unwrap()
    }

    /// The entry titled `title`, looked up by label.
    ///
    /// By title rather than by index: `load` also reads the per-user file, so
    /// the developer running the tests may well contribute entries of their
    /// own, and those must not decide what position anything is at.
    fn entry<'a>(config: &'a Config, title: &str) -> &'a MenuItem {
        config
            .menu
            .iter()
            .find(|item| item.label() == title)
            .unwrap_or_else(|| panic!("no entry titled {title:?}"))
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn one_entry(title: &str) -> String {
        format!("menu:\n  - title: {title}\n    shell: 'true'\n")
    }

    #[test]
    fn parses_the_documented_yaml_shape() {
        let config =
            parse_str("a.yaml", "menu:\n  - title: hello\n    shell: echo hello\n").unwrap();
        assert_eq!(config.menu.len(), 1);
        assert_eq!(config.menu[0].label(), "hello");
        assert_eq!(config.menu[0].script().unwrap(), "echo hello");
        assert!(config.merge);
    }

    #[test]
    fn parses_a_bare_top_level_list_as_the_menu() {
        let config = parse_str("a.yaml", "- title: hello\n  shell: echo hello\n").unwrap();
        assert_eq!(config.menu.len(), 1);
        assert_eq!(config.menu[0].label(), "hello");
    }

    #[test]
    fn joins_a_shell_list_into_one_script() {
        let config = parse_str(
            "a.yaml",
            "menu:\n  - title: t\n    shell:\n      - cd /tmp\n      - ls\n",
        )
        .unwrap();
        assert_eq!(config.menu[0].script().unwrap(), "cd /tmp\nls");
    }

    #[test]
    fn parses_a_parallel_group() {
        let config = parse_str(
            "a.yaml",
            "menu:\n  - title: servers\n    parallel:\n      - shell: npm run dev\n      - title: api\n        shell:\n          - cd api\n          - npm start\n",
        )
        .unwrap();
        let item = &config.menu[0];
        assert!(item.script().is_none(), "a group has no single script");
        assert_eq!(item.parallel.len(), 2);
        assert_eq!(item.parallel[0].label(), "npm run dev");
        assert_eq!(item.parallel[1].label(), "api");
        assert_eq!(item.parallel[1].shell.script(), "cd api\nnpm start");
    }

    #[test]
    fn labels_a_parallel_group_with_its_commands_when_no_title_is_given() {
        let config = parse_str(
            "a.yaml",
            "menu:\n  - parallel:\n      - shell: npm run dev\n      - shell: npm run api\n",
        )
        .unwrap();
        assert_eq!(config.menu[0].label(), "npm run dev & npm run api");
    }

    #[test]
    fn rejects_an_entry_that_is_both_sequential_and_parallel() {
        // Which of the two Enter should run would be a guess, and a `shell`
        // list already exists for running commands one after the other.
        let err = parse_str(
            "a.yaml",
            "menu:\n  - title: t\n    shell: echo hi\n    parallel:\n      - shell: echo there\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("both `shell` and `parallel`"), "{err}");
        assert!(err.contains("\"t\""), "the entry is named: {err}");
    }

    #[test]
    fn rejects_the_same_entry_inside_a_submenu() {
        assert!(
            parse_str(
                "a.yaml",
                "menu:\n  - title: t\n    submenu:\n      - shell: a\n        parallel:\n          - shell: b\n",
            )
            .is_err(),
            "a nested entry is loaded like any other, so it is checked like any other"
        );
    }

    #[test]
    fn a_parallel_command_takes_no_fields_of_its_own() {
        // No submenu, help or args on a group member: it is not a menu level,
        // so those would have nowhere to show up.
        assert!(
            parse_str(
                "a.yaml",
                "menu:\n  - parallel:\n      - shell: a\n        help: nope\n",
            )
            .is_err()
        );
        assert!(
            parse_str("a.yaml", "menu:\n  - parallel:\n      - title: no shell\n").is_err(),
            "`shell` is what a group member is"
        );
    }

    #[test]
    fn falls_back_to_the_command_when_no_title_is_given() {
        let config = parse_str("a.yaml", "menu:\n  - shell: echo hello\n").unwrap();
        assert_eq!(config.menu[0].label(), "echo hello");
    }

    #[test]
    fn parses_toml() {
        let config = parse_str(
            "a.toml",
            "[[menu]]\ntitle = \"hello\"\nshell = \"echo hello\"\n",
        )
        .unwrap();
        assert_eq!(config.menu[0].label(), "hello");
    }

    #[test]
    fn parses_json() {
        let config = parse_str(
            "a.json",
            r#"{"menu": [{"title": "hello", "shell": "echo hello"}]}"#,
        )
        .unwrap();
        assert_eq!(config.menu[0].label(), "hello");
    }

    #[test]
    fn parses_a_bare_json_array_as_the_menu() {
        let config = parse_str("a.json", r#"[{"title": "hello", "shell": "echo hello"}]"#).unwrap();
        assert_eq!(config.menu[0].label(), "hello");
    }

    #[test]
    fn rejects_unknown_extensions() {
        assert!(parse_str("a.cson", "menu: []").is_err());
    }

    #[test]
    fn reports_the_error_of_the_documented_shape_not_the_shorthand() {
        // `shell` is a mapping, which neither shape accepts. The message must
        // point at the offending entry, not say "expected a sequence" (which
        // is all the shorthand attempt can report about a top-level mapping).
        let err = parse_str("a.yaml", "menu:\n  - title: t\n    shell:\n      a: b\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("menu[0]"), "unexpected message: {err}");
        assert!(err.contains("Shell"), "unexpected message: {err}");
    }

    #[test]
    fn a_file_that_says_nothing_about_auto_launchers_leaves_it_undecided() {
        let config = parse_str("a.yaml", "menu: []").unwrap();
        assert!(
            config.auto_launchers.is_none(),
            "otherwise this file would override a parent that did decide"
        );
        assert!(AutoLaunchers::default().any(), "the default is all enabled");
    }

    #[test]
    fn auto_launchers_can_be_turned_off_at_once() {
        let config = parse_str("a.yaml", "auto_launchers: false\nmenu: []").unwrap();
        assert!(!config.auto_launchers.unwrap().any());
    }

    #[test]
    fn auto_launchers_can_be_turned_off_individually() {
        let config = parse_str("a.yaml", "auto_launchers:\n  makefile: false\nmenu: []").unwrap();
        let flags = config.auto_launchers.unwrap();
        assert!(!flags.makefile());
        assert!(flags.package_json());
    }

    #[test]
    fn recognises_a_fallback_file_even_when_the_rest_is_invalid() {
        // A file that is going to be skipped must not be able to abort
        // startup with an error nobody would have read.
        let text = "merge: false\nmenu:\n  - titel: typo\n";
        assert!(reads_as_fallback(Path::new("a.yaml"), text));
        assert!(
            parse_str("a.yaml", text).is_err(),
            "still invalid on its own"
        );
    }

    #[test]
    fn does_not_treat_a_normal_file_as_a_fallback() {
        assert!(!reads_as_fallback(Path::new("a.yaml"), "menu: []"));
        assert!(!reads_as_fallback(
            Path::new("a.yaml"),
            "merge: true\nmenu: []"
        ));
    }

    #[test]
    fn leaves_an_unreadable_file_to_the_real_parser() {
        // Not valid in any shape: reporting it is the real parse's job.
        assert!(!reads_as_fallback(Path::new("a.yaml"), "\t: : ["));
    }

    #[test]
    fn recognises_a_fallback_file_in_every_format() {
        assert!(reads_as_fallback(Path::new("a.toml"), "merge = false\n"));
        assert!(reads_as_fallback(
            Path::new("a.json"),
            r#"{"merge": false}"#
        ));
    }

    #[test]
    fn rejects_unknown_keys_so_typos_are_not_silently_ignored() {
        assert!(parse_str("a.yaml", "menuu: []").is_err());
        assert!(parse_str("a.yaml", "menu:\n  - titel: t\n").is_err());
    }

    #[test]
    fn an_entry_runs_where_the_file_that_declared_it_lives() {
        // The point of the default: a command written in the project's file
        // does not have to start with a `cd` to find the project.
        let root = tempdir("declaring-dir");
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        write(&root, ".j-menu.yaml", &one_entry("from root"));
        write(&nested, ".j-menu.yaml", &one_entry("from nested"));

        let config = load(&nested).unwrap();
        assert_eq!(entry(&config, "from nested").cwd.as_deref(), Some(&*nested));
        assert_eq!(entry(&config, "from root").cwd.as_deref(), Some(&*root));
    }

    #[test]
    fn a_file_can_ask_for_the_working_directory_instead() {
        let dir = tempdir("file-opts-out");
        let nested = dir.join("a");
        std::fs::create_dir_all(&nested).unwrap();
        write(
            &dir,
            ".j-menu.yaml",
            &format!("run_in_current_directory: true\n{}", one_entry("here")),
        );

        let config = load(&nested).unwrap();
        assert_eq!(
            entry(&config, "here").cwd,
            None,
            "None is what stands for the directory j-menu was started from"
        );
        assert_eq!(config.sources[0].cwd, None, "and --show-config says so");
    }

    #[test]
    fn an_entry_overrides_the_file_in_both_directions() {
        let dir = tempdir("entry-overrides");
        write(
            &dir,
            ".j-menu.yaml",
            "run_in_current_directory: true\n\
             menu:\n\
             \x20 - title: follows the file\n\
             \x20   shell: 'true'\n\
             \x20 - title: back to the file's directory\n\
             \x20   shell: 'true'\n\
             \x20   run_in_current_directory: false\n",
        );

        let config = load(&dir).unwrap();
        assert_eq!(entry(&config, "follows the file").cwd, None);
        assert_eq!(
            entry(&config, "back to the file's directory")
                .cwd
                .as_deref(),
            Some(&*dir)
        );
    }

    #[test]
    fn a_submenu_inherits_its_entry_and_can_override_it_in_turn() {
        let dir = tempdir("submenu-inherits");
        write(
            &dir,
            ".j-menu.yaml",
            "menu:\n\
             \x20 - title: group\n\
             \x20   run_in_current_directory: true\n\
             \x20   submenu:\n\
             \x20     - title: inherited\n\
             \x20       shell: 'true'\n\
             \x20     - title: overridden\n\
             \x20       shell: 'true'\n\
             \x20       run_in_current_directory: false\n",
        );

        let config = load(&dir).unwrap();
        let group = entry(&config, "group");
        assert_eq!(group.submenu[0].cwd, None, "inherited from the entry");
        assert_eq!(
            group.submenu[1].cwd.as_deref(),
            Some(&*dir),
            "a nested entry has the last word"
        );
    }

    #[test]
    fn cwd_is_not_something_a_file_can_set() {
        // It is filled in while loading. A file naming it is a typo, not a
        // way to point an entry at an arbitrary directory.
        assert!(parse_str("a.yaml", "menu:\n  - shell: 'true'\n    cwd: /tmp\n").is_err());
    }
}
