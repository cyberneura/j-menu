# jj-menu

A simple TUI menu launcher. Type `jj`, press Enter, pick a command with `j`/`k`,
run it with Enter.

This is a Rust rewrite of the Python [ytyng/jj-menu](https://github.com/ytyng/jj-menu).
The idea is the same — a per-project list of commands you can reach in two
keystrokes — with a YAML/TOML/JSON configuration, submenus, prompted arguments,
and entries picked up automatically from `package.json`, `Makefile`, Cargo and
Gradle projects.

The selected command runs with the TTY fully attached, so `ssh`, `vim`, `top`
and anything else interactive behaves exactly as if you had typed it.

**Unix-like systems only** (macOS, Linux). A menu entry is a shell script run
through `$SHELL -c`, which has no meaningful Windows equivalent, so no Windows
binaries are published.

## Install

### Homebrew

```shell
brew install cyberneura/tap/jj-menu
```

### Cargo

```shell
cargo install jj-menu
```

### Shell function

The binary is `jj-menu`. Add the wrapper function to get the short `jj` command,
and so that entries like `cd /tmp` or `export FOO=1` affect *your* shell rather
than a child process:

```shell
# ~/.zshrc
eval "$(jj-menu --shell-init zsh)"

# ~/.bashrc
eval "$(jj-menu --shell-init bash)"

# ~/.config/fish/config.fish
jj-menu --shell-init fish | source
```

Calling the binary by a path -- `eval "$(~/src/jj-menu/target/release/jj-menu
--shell-init zsh)"` -- works too: the generated function then calls that same
path, so `jj` works when the binary is not on `PATH`.

The fish function needs **fish 3.4 or newer**. It relies on quoted command
substitution (`"$(...)"`), which is what keeps a multi-line entry in one piece;
older versions split it on newlines.

The wrapper also pushes the command into your shell history, so the usual
recall-and-edit workflow keeps working.

Without the wrapper, `jj-menu` runs the command itself; everything works except
changes to the shell's own state.

## Usage

```
jj-menu [OPTIONS]

  --print              Print the selected command instead of running it
  --shell-init <SHELL> Print the shell function (bash, zsh, fish)
  --cwd <DIR>          Search for configuration from here instead of $PWD
  --show-config        List the configuration files that were loaded
  -h, --help           Show help
  -V, --version        Show the version
```

### Keys

| Key | Action |
| --- | --- |
| `j`, `↓`, `Ctrl-N` | Down |
| `k`, `↑`, `Ctrl-P` | Up |
| `g`, `Home` | First entry |
| `G`, `End` | Last entry |
| `l`, `→` | Open the detail view (help, submenu) |
| `h`, `←` | Back (leaves the menu at the top level) |
| `Enter` | Run the entry |
| `/` | Incremental search |
| `q`, `Ctrl-C` | Quit |
| `Esc` | Drop the search, or quit when there is none |

### Incremental search

`/` narrows the list as you type: only the entries whose label contains what you
have typed stay on screen, matched without regard to case, and the cursor sits on
the first of them. `Enter` runs it. It is the fastest way through a long menu —
`/dep` then `Enter`.

**The letter keys are text while you are typing**, so moving around is `↑` / `↓`
(or `Ctrl-P` / `Ctrl-N`) and `←` / `→`, none of which can be typed. `Esc` (or
backspacing past the start) drops the search and shows everything again, leaving
the cursor on the entry it was on; `Ctrl-U` empties what you typed without
leaving the search. `Ctrl-C` quits the menu, from here as from anywhere else.

The search belongs to the level it was typed on: opening a submenu starts
unfiltered, and going back brings the filter with you. A filtered list keeps
saying `/…` above it after the search is accepted, so a menu that is missing
entries never looks like a menu that does not have them.

The `help` of the entry the cursor is on is shown in a block of its own, just
above the status line — three rows of it, or fewer on a terminal with no room to
spare. The block is reserved for the whole menu rather than sized to the entry,
so the list stays where it is as the cursor walks past entries that have help
and entries that do not, and a menu where nothing has a `help` spends no rows on
it at all. Help too long for the block carries on in the detail view, which
wraps it over as much as half the screen; a description longer than that is cut,
in both places.

Entries with a detail view are marked with `>`: those are the ones with a
`submenu`, a `help` text, or both. The view shows the help, the entry's own
command as a `Run: ...` line, and the submenu.

Arguments are not part of the detail view — they are prompted for when the entry
runs, whichever level you run it from.

While entering an argument: `Enter` accepts, `Esc` cancels, and the usual
`Ctrl-A` / `Ctrl-E` / `Ctrl-U` motions work.

### Colours

The menu is coloured: a cyan title, entries with a detail view (a submenu, a
help text, or both) in cyan, the selected row as a blue bar with a yellow `*>`
marker, the selected entry's help in grey above the status line, and a status
line carrying the command preview (or the entry count of a submenu). Only colours the
terminal itself defines (palette entries 0–15) are used, so it follows your
theme.

Set `NO_COLOR` to a non-empty value to turn the colours off; `TERM=dumb` does
the same. The selected row and the status line then fall back to reverse video,
so they stay visible.

### Exit codes

`jj-menu` exits with the exit code of the command it ran, so `jj && something`
behaves as expected. A command killed by a signal reports `128 + signal`, the
same as in bash and zsh. Dismissing the menu without choosing anything exits
`130` (the conventional "interrupted" code), which the shell wrapper treats as
"do nothing".

## Configuration

Configuration files are searched for in the current directory and every ancestor,
nearest first, and then in `~/.config/jj-menu/`. Every file that is found is
merged, in that order.

### File names

Any of these base names:

```
.jj-menu   _jj-menu   jj-menu
.jj-menu.local   _jj-menu.local   jj-menu.local
```

with any of these extensions:

```
.yaml   .yml   .toml   .json
```

The per-user file is `~/.config/jj-menu/config.{yaml,yml,toml,json}` (or the
platform equivalent of `$XDG_CONFIG_HOME`).

Within one directory the shared file is loaded before the `.local` one, so a
`.jj-menu.local.yaml` can add personal entries without touching the file that is
committed to the repository.

### Format

```yaml
menu:
  - title: Run the dev server
    shell: pnpm dev

  - title: Clean and rebuild
    # A list is joined into one script and runs in a single shell, so `cd`
    # carries over to the next line.
    shell:
      - cd build
      - make clean
      - make -j

  - title: Dev servers
    # Each of these gets its own shell and they all run at once.
    parallel:
      - shell: npm run dev
      - title: api
        shell: npm run api

  - title: Deploy
    help: Pick the environment to deploy to.
    submenu:
      - title: staging
        shell: ./deploy.sh staging
      - title: production
        shell: ./deploy.sh production

  - title: Search the repository
    shell: rg {pattern}
    args:
      - name: pattern
        prompt: Pattern
        default: TODO
```

The shortest possible file is a bare list, with no `menu:` key:

```yaml
- title: List files
  shell: ls -la
```

`title` may be omitted, in which case the command itself is the label.

The same structure works in TOML and JSON:

```toml
[[menu]]
title = "Run the dev server"
shell = "pnpm dev"
```

### Entry fields

| Field | Meaning |
| --- | --- |
| `title` | Label shown in the menu. Defaults to `shell`. |
| `shell` | Command, or a list of commands run as one script. |
| `parallel` | Commands to run at the same time, one shell each. |
| `help` | Description shown above the status line while the entry is selected, and in the detail view. |
| `submenu` | Nested entries, opened with `l` / `→`. |
| `args` | Values prompted for and substituted into `shell`. |
| `run_in_current_directory` | Run this entry where `jj` was typed instead of where the file lives. |

Each `args` entry has a `name` (the `{name}` placeholder in `shell`), and
optionally a `prompt` and a `default`.

Values are substituted verbatim — the input is pasted into the script before the
shell parses it, and nothing escapes it — so quoting is up to the template: write
`rg {pattern}` if you want the input to be able to carry flags and its own
quoting, and `rg "{pattern}"` if you want whitespace in it to stay in a single
argument. The quotes do not make it literal: `$(...)`, backticks and `$VAR` still
expand inside them, and a `"` in the input ends the quoted section. An entry is
as trusted as whoever types into its prompt. A placeholder with no matching
argument is left alone, so `${HOME}` and `a{1,2}` survive unharmed.

### Where a command runs

**An entry runs in the directory of the configuration file that declared it**,
not in the directory you happened to type `jj` in. A file at the root of a
repository can therefore say

```yaml
menu:
  - title: Run the tests
    shell: pytest
```

and that entry works from anywhere in the checkout, without a `cd` in front of
it and without knowing how deep you are.

The per-user file is the exception. It belongs to no project, its entries are
written to be run wherever you are, and running them in
`~/.config/jj-menu/` would be useless — so its entries default to the working
directory instead.

`run_in_current_directory: true` asks for the working directory explicitly. It
can be set on the file, where it covers every entry in it, and on an entry,
where it covers that entry and its `submenu`. The nearest one wins, so an entry
can also opt back *in* with `run_in_current_directory: false`:

```yaml
run_in_current_directory: true    # everything in this file runs where you are
menu:
  - title: Count the files here
    shell: ls | wc -l

  - title: Run the tests
    shell: pytest
    run_in_current_directory: false   # ... except this one
```

`jj-menu --show-config` reports the directory each file's entries run in by
default; an entry that overrides it is not listed separately.

Two things follow from this:

- **Under the shell wrapper the command is prefixed with `cd <dir>;`**, since
  the wrapper evaluates it in your shell rather than in a child process. That
  `cd` stays in effect afterwards — a directory change reaching your own shell
  is what the wrapper is for. An entry that should leave you where you are
  needs `run_in_current_directory: true`.
- Entries from the automatic launchers are unaffected: they are found relative
  to the working directory and already carry their own `cd` when the project
  they belong to is in an ancestor.

### Running commands at once

A `shell` list runs its commands one after the other in a single shell.
`parallel` is the other case: every entry under it is a separate shell, all of
them are started together, and the menu is done when the last one has exited.

```yaml
menu:
  - title: Dev servers
    parallel:
      - shell: npm run dev
      - title: api
        shell:
          - cd api
          - npm start
```

A member takes `title` and `shell` and nothing else — it is not a menu level of
its own, so `submenu`, `help` and `args` have nowhere to appear on it. `shell`
may be a list there too, which then runs as one script in that member's shell.
`args` belong to the entry that owns the group and are substituted into every
member, so the value is asked for once.

An entry may not have both `shell` and `parallel`; that is an error rather than
a guess about which of the two Enter should run.

- **Ctrl-C stops the whole group.** Each command runs in a process group of its
  own, and the signal goes to the group, so it also reaches whatever that
  command started — `sleep 300; echo done` stops, not just the shell in front of
  it. `jj-menu` waits for them all before returning, so the group is not left
  writing to the terminal behind your prompt, and a second Ctrl-C kills what has
  not stopped by then. A process a command *detached* on purpose (`something &`,
  `nohup`) is not followed, the same as when a shell you typed into exits.
- **The exit code is the first failure**, in the order the commands are written,
  or 0 when they all succeed. A command killed by the interrupt reports
  `128 + signal`, so a group stopped with Ctrl-C normally exits 130.
- **Output is interleaved**, exactly as it would be from `a & b & wait`.
- **Nothing can read from the keyboard**: the commands get `/dev/null` on
  stdin, because several processes taking turns at the terminal cannot be told
  apart. Anything interactive belongs in a plain `shell`.
- **The shell wrapper does not evaluate a group.** `--print` hands a single
  command back to your shell so that `cd` and `export` reach it; a group is
  several separate processes, none of which could do that, so `jj-menu` runs it
  itself and prints nothing. Everything above applies either way.

### Not merging

A file marked `merge: false` is skipped when another configuration file has
already been loaded:

```yaml
merge: false
menu:
  - title: Only this
    shell: echo hi
```

Since files are loaded nearest-first, this turns a file into a **fallback**: it
contributes only when nothing closer to the working directory was found. That is
what you want on a repository-wide or per-user file whose entries would be noise
once a directory has its own menu.

It has no effect on the nearest file itself, which is always the one loaded
first.

A file skipped this way is not parsed either, so an error inside it is not
reported while it stays inactive: a fallback nobody is reading must not be able
to stop `jj` from opening. The flip side is that `--show-config` says nothing
about such a file — check it from a directory where it is the nearest one.

## Automatic launchers

With no configuration at all, `jj-menu` still has something to show: it looks
for project files in the current directory and its ancestors.

| Source | Entries |
| --- | --- |
| `package.json` | Every `scripts` entry, run with the package manager implied by the nearest lock file — searched upwards, stopping at the repository root or your home directory (pnpm, yarn, bun, npm; npm if none is found) |
| `Makefile` | Every target that can run without extra arguments |
| `Cargo.toml` | `build`, `test`, `check`, and `run` — named per binary when there are several, omitted when there is none, and carrying `--features` for a target with `required-features` — plus `fmt` and `clippy` when those components are installed |
| Gradle | `tasks`, plus the lifecycle tasks (`build`, `clean`, `assemble`, `check`, and `test`) that the plugins declared by the build script define, using `./gradlew` when present — the wrapper is looked up separately from the build script, so a root wrapper is used for a subproject |

In a Node project with no jj-menu file, `jj` is therefore just a list of npm
scripts.

When several sources are found, each becomes a submenu so the top level stays
short. Configured entries always come first.

### What is deliberately left out

Only entries that can run as-is are listed, because a menu entry that always
fails is worse than a missing one:

- **Makefile**: pattern rules (`%.o: %.c`) and targets built from variables
  (`$(BIN):`) are skipped — they need a concrete file name that only you know.
  So is anything inside a `define ... endef`, which make does not read as
  makefile syntax until the variable is expanded, and anything inside a
  conditional (`ifeq` … `endif`), since which branch make takes depends on
  variables that cannot be expanded here. Recipe lines are recognised by the
  active recipe prefix, so a `.RECIPEPREFIX` other than tab is honoured, and a
  target that looks like a flag is run as `make -- '-n'`.
- **package.json**: a script whose name starts with `-` is skipped. Every one
  of the runners parses it as an option (`npm run --silent` exits without
  running anything) and none has a way to say "this is a script name".
- **Gradle**: the real task list can only be obtained by running
  `./gradlew tasks`, which starts a JVM and evaluates the build script. That is
  far too slow to do while opening a menu, so the tasks are inferred from the
  plugins the build script applies — for a subproject, the root's
  `allprojects` and `subprojects` blocks count as well, since that is where a
  multi-project build usually applies them. A plugin listed with `apply false`
  does not, and neither does one referenced through a version catalog
  (`alias(libs.plugins.foo)`), since the id behind the alias lives in
  `libs.versions.toml`. A build with nothing recognisable has no lifecycle
  tasks to offer — `build` there silently runs the unrelated
  `buildEnvironment` — so it gets only the `tasks` entry to discover the rest.
- **Cargo**: `cargo run` is omitted when the package has no binary at all (a
  virtual workspace, or a library-only package) and replaced by one
  `cargo run --bin ...` per target when there are several, because a bare
  `cargo run` would be ambiguous. Binaries auto-discovered from `src/main.rs`
  and `src/bin/` are counted, and `required-features` is passed through as
  `--features`. `fmt` and `clippy` are offered only once the component behind
  them answers, because rustup installs the shim for both whether or not the
  component is there.
- **Gradle**: commands are run from the directory holding the build script.
  Gradle takes the project directory from the working directory and rejects one
  that is not part of the build, so opening the menu in, say, `src/main/java`
  would otherwise fail.
- Tools that take positional arguments (`fab`, `cap`, ...) are not scanned at
  all. Define those explicitly with `args`, which is exactly what `args` is for.

Names taken from a project file (npm script names, make targets, Cargo binary
names) are quoted before being put in a command, and every string drawn on
screen has its control characters replaced with `·`. They come from the
repository rather than from whoever wrote the menu, so a name like
`build; rm -rf /` must not be able to add a second command, and one containing
an ESC must not be able to drive the terminal just by being displayed.

### Turning them off

```yaml
auto_launchers: false
```

or per launcher:

```yaml
auto_launchers:
  package_json: true
  makefile: false
  cargo: true
  gradle: false
```

The nearest file that sets `auto_launchers` at all decides the whole block; one
further up is then ignored rather than merged switch by switch. A nearer
`{makefile: false}` therefore silences an ancestor's `{cargo: false}`, and cargo
stays on — put every switch you need in the same file.

## Agent skill

`skills/jj-menu/` is an agent skill describing the configuration format, so a
coding agent can write and edit menu files for you. Install it with the
[skills](https://github.com/vercel-labs/skills) CLI:

```shell
npx skills add cyberneura/jj-menu            # into ./<agent>/skills/
npx skills add cyberneura/jj-menu -g         # into ~/<agent>/skills/, all projects
```

It works with Claude Code, Codex, Cursor, OpenCode and the rest of the agents
that CLI supports. It detects the ones you have installed and prompts when the
choice is ambiguous; `-a <agent>` picks explicitly.

## Unsupported formats

`.cson` (CoffeeScript Object Notation) configuration files are **not** read.
There is no maintained CSON parser for Rust, and writing one is out of
proportion to the format's use. A `.jj-menu.cson` is ignored, not reported as an
error. Use YAML, which CSON was modelled on.

## Development

```shell
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run
```

Releasing is changing the version in `Cargo.toml` on `main`:
`.github/workflows/tag-on-version-change.yml` reads the version, tags the
merge commit if that version has not been released, and
[dist](https://opensource.axo.dev/cargo-dist/) takes it from the tag and builds
the macOS and Linux binaries. `cyberneura/homebrew-tap` updates its own formula
from the latest release within the hour; nothing is pushed to it from here.

## License

MIT
