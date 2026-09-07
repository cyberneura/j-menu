//! Data model for the menu configuration.
//!
//! The same model is produced from YAML, TOML and JSON, so parsing lives in
//! [`super::loader`] and only the shape is described here.

use std::path::PathBuf;

use serde::Deserialize;

/// A whole configuration file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Menu entries defined by this file.
    #[serde(default)]
    pub menu: Vec<MenuItem>,

    /// When `true`, the entries of this file run in the directory `j-menu`
    /// was started from instead of the directory holding the file.
    ///
    /// `None` means the file said nothing, which is not the same as `false`:
    /// the per-user file has no project to belong to, so it defaults the
    /// other way round (see [`super::loader::load`]).
    #[serde(default)]
    pub run_in_current_directory: Option<bool>,

    /// When `false`, this file is skipped if another configuration file has
    /// already been loaded. Defaults to `true` (files are merged).
    #[serde(default = "default_true")]
    pub merge: bool,

    /// Controls the built-in launchers (`package.json`, `Makefile`, Cargo,
    /// Gradle). `None` means the file said nothing, which is different from
    /// spelling out the defaults: only a file that decides stops files further
    /// up from deciding.
    #[serde(default)]
    pub auto_launchers: Option<AutoLaunchers>,
}

/// A single menu entry.
///
/// `shell` accepts either a single command or a list of commands. A list is
/// joined with newlines and handed to the shell as one script, so the commands
/// run sequentially in the same shell (this is how `cd` followed by another
/// command keeps working). `parallel` is the other way to run several commands:
/// each of its entries gets its own shell and they all run at once.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MenuItem {
    /// Label shown in the menu. Falls back to `shell` when omitted.
    #[serde(default)]
    pub title: Option<String>,

    /// Command(s) to run.
    #[serde(default)]
    pub shell: Option<Shell>,

    /// Commands to run at the same time, each in its own shell.
    ///
    /// Mutually exclusive with `shell`; the loader rejects an entry that has
    /// both, since which one Enter should run would be a guess.
    #[serde(default)]
    pub parallel: Vec<ParallelCommand>,

    /// Long description shown in the detail view.
    #[serde(default)]
    pub help: Option<String>,

    /// Nested entries, opened from the detail view.
    #[serde(default)]
    pub submenu: Vec<MenuItem>,

    /// Placeholders that are prompted for before running `shell`.
    #[serde(default)]
    pub args: Vec<ArgSpec>,

    /// Overrides the file's `run_in_current_directory` for this entry, and for
    /// its `submenu` unless a nested entry overrides it in turn.
    #[serde(default)]
    pub run_in_current_directory: Option<bool>,

    /// Directory this entry runs in, filled in while loading; `None` means the
    /// directory `j-menu` was started from.
    ///
    /// Not a configuration key — `deny_unknown_fields` rejects a file that
    /// tries to set it, which is why it is skipped rather than defaulted.
    #[serde(skip)]
    pub cwd: Option<PathBuf>,
}

/// One command of a `parallel` group.
///
/// Has the same `title` / `shell` pair as a menu entry, but nothing else: a
/// group member is not a menu level of its own, so a submenu or arguments on it
/// would have nowhere to appear. Arguments belong to the entry that owns the
/// group and are substituted into every member.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParallelCommand {
    /// Name used when the group is announced before it runs. Falls back to
    /// `shell`.
    #[serde(default)]
    pub title: Option<String>,

    /// Command(s) this member runs, as one script in one shell.
    pub shell: Shell,
}

impl ParallelCommand {
    /// Label for this member of the group.
    pub fn label(&self) -> String {
        match &self.title {
            Some(title) if !title.is_empty() => title.clone(),
            _ => first_line(&self.shell.script()).to_string(),
        }
    }
}

/// What an entry runs when it is selected, with arguments already substituted.
#[derive(Debug, Clone)]
pub enum Launch {
    /// One script in one shell.
    Script(String),
    /// Several scripts at once, one shell each.
    Parallel(Vec<Job>),
}

/// One command of a `parallel` group, ready to be spawned.
#[derive(Debug, Clone)]
pub struct Job {
    pub title: String,
    pub script: String,
}

/// One or many shell commands.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Shell {
    One(String),
    Many(Vec<String>),
}

impl Shell {
    /// Render as a single script.
    pub fn script(&self) -> String {
        match self {
            Shell::One(s) => s.clone(),
            Shell::Many(v) => v.join("\n"),
        }
    }
}

/// A value asked for interactively and substituted into `shell` as `{name}`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    /// Placeholder name. `{name}` in `shell` is replaced with the input.
    pub name: String,

    /// Text shown while asking. Falls back to `name`.
    #[serde(default)]
    pub prompt: Option<String>,

    /// Value used when the input is left empty.
    #[serde(default)]
    pub default: Option<String>,
}

/// Which built-in launchers to scan for.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AutoLaunchers {
    /// `auto_launchers: false` turns every launcher off at once.
    All(bool),
    /// Per-launcher switches.
    Each(AutoLauncherFlags),
}

impl Default for AutoLaunchers {
    fn default() -> Self {
        AutoLaunchers::All(true)
    }
}

impl AutoLaunchers {
    pub fn package_json(&self) -> bool {
        self.flag(|f| f.package_json)
    }

    pub fn makefile(&self) -> bool {
        self.flag(|f| f.makefile)
    }

    pub fn cargo(&self) -> bool {
        self.flag(|f| f.cargo)
    }

    pub fn gradle(&self) -> bool {
        self.flag(|f| f.gradle)
    }

    /// `true` when at least one launcher is enabled.
    pub fn any(&self) -> bool {
        self.package_json() || self.makefile() || self.cargo() || self.gradle()
    }

    fn flag(&self, pick: impl Fn(&AutoLauncherFlags) -> bool) -> bool {
        match self {
            AutoLaunchers::All(enabled) => *enabled,
            AutoLaunchers::Each(flags) => pick(flags),
        }
    }
}

/// Per-launcher switches. Every launcher defaults to enabled, so a file only
/// needs to name the ones it wants to turn off.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoLauncherFlags {
    #[serde(default = "default_true")]
    pub package_json: bool,
    #[serde(default = "default_true")]
    pub makefile: bool,
    #[serde(default = "default_true")]
    pub cargo: bool,
    #[serde(default = "default_true")]
    pub gradle: bool,
}

impl Default for AutoLauncherFlags {
    fn default() -> Self {
        Self {
            package_json: true,
            makefile: true,
            cargo: true,
            gradle: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl MenuItem {
    /// Label to render in the menu.
    pub fn label(&self) -> String {
        if let Some(title) = &self.title
            && !title.is_empty()
        {
            return title.clone();
        }
        if let Some(shell) = &self.shell {
            return first_line(&shell.script()).to_string();
        }
        // A `parallel` entry with no title reads as what a shell would be
        // asked to do: `a & b`.
        self.parallel
            .iter()
            .map(|command| command.label())
            .collect::<Vec<_>>()
            .join(" & ")
    }

    /// The script this entry runs, if any.
    ///
    /// A `parallel` entry has no single script; use [`MenuItem::launch`] to
    /// cover both kinds.
    pub fn script(&self) -> Option<String> {
        self.shell.as_ref().map(|s| s.script())
    }

    /// What Enter on this entry runs, or `None` when it only opens.
    pub fn launch(&self) -> Option<Launch> {
        // `shell` first: an entry carrying both is rejected while loading, so
        // the order only decides what a hand-built item does.
        if let Some(script) = self.script() {
            return Some(Launch::Script(script));
        }
        if self.parallel.is_empty() {
            return None;
        }
        Some(Launch::Parallel(
            self.parallel
                .iter()
                .map(|command| Job {
                    title: command.label(),
                    script: command.shell.script(),
                })
                .collect(),
        ))
    }

    /// Whether the detail view has anything to show for this entry.
    ///
    /// Arguments deliberately do not count. They are prompted for when the
    /// entry runs, so a detail view would add nothing — and the view puts the
    /// entry's own command in a `Run: ...` line that carries the same
    /// arguments, which would then be openable again, and again.
    pub fn has_detail(&self) -> bool {
        !self.submenu.is_empty() || self.help.is_some()
    }

    /// Build a plain entry, used by the built-in launchers.
    pub fn command(title: impl Into<String>, shell: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            shell: Some(Shell::One(shell.into())),
            ..Default::default()
        }
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}
