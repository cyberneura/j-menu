# AGENTS.md

Notes for AI agents working on `j-menu`. See `README.md` for what the tool is
and how a user configures it.

## What this is

A TUI menu launcher in Rust (edition 2024, MSRV 1.85). The user types `j`,
picks an entry, and the selected shell script runs with the TTY fully attached.
Unix-like systems only.

Dependencies: `crossterm` (terminal), `clap` (CLI), `serde` +
`serde_yaml_ng` / `toml` / `serde_json` (configuration), `anyhow` (errors),
`dirs`, and `libc` — the last one only for restoring the terminal from a signal
handler, where nothing that locks may be called.

## Layout

| Path | Contents |
|---|---|
| `src/main.rs` | CLI, wiring, the `$ <script>` echo before a command runs |
| `src/config/` | Discovery of configuration files, loading, merging, the data model |
| `src/launchers/` | Entries derived from `package.json`, `Makefile`, Cargo and Gradle projects |
| `src/menu.rs` | Builds the menu from configuration plus launchers |
| `src/ui/` | The interactive menu: `mod.rs` drawing, `state.rs` navigation and the incremental search, `prompt.rs` line editing, `theme.rs` colours |
| `src/exec.rs` | Running the selected script |
| `src/parallel.rs` | Running a `parallel:` group: one shell per member, and passing Ctrl-C on to all of them |
| `src/signal.rs` | Restoring the terminal when a signal kills the process |
| `src/shell_init.rs` | The `j` wrapper function for bash / zsh / fish |
| `tests/` | Integration tests for configuration merging |
| `skills/` | The agent skill published from this repository (`npx skills add cyberneura/j-menu`) |

## Commands

Run these before pushing — they are what CI (`.github/workflows/ci.yml`)
checks, in this order, plus `cargo build --release`:

```shell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

For looking at the thing by hand, when you have a terminal:

```shell
cargo run                  # opens the menu in the current directory
cargo run -- --show-config # which configuration files were loaded, then exits
```

`cargo run` opens the interactive menu and waits for a key, so it is not part
of any automated check.

## Working on the configuration

- **Where an entry runs is resolved while loading, not while running.**
  `config::loader::assign_cwd` walks the parsed entries and writes
  `MenuItem::cwd`: the directory of the file that declared them, or `None` for
  "wherever `j-menu` was started". Nothing downstream re-derives it, so an
  entry that arrives from somewhere other than a file (the launchers,
  `MenuItem::command`) is `None` and keeps running in `start_dir`.
- **`run_in_current_directory` is `Option<bool>` on both the file and the
  entry, and that matters.** `None` means "said nothing", which is what lets the
  per-user file default the other way round (its entries belong to no project)
  while a project file defaults to its own directory. Collapsing it to `bool`
  loses that distinction.
- **`--print` has to carry the directory itself.** The wrapper evaluates the
  command in the user's shell, so `main::in_dir_script` prefixes it.
  `cd <dir>;` and not `cd <dir> &&`: `&&` binds tighter than `&`, so an entry
  starting `server &` would background the `cd` with it and leave the rest of
  the script where the shell already was. A subshell would keep the shell from
  being moved, but has no form bash, zsh *and* fish all accept, and would drop
  the `cd` / `export` effects the wrapper exists for. `launchers::in_dir` is
  the single-command version and keeps its `&&`.
- **`start_dir` is not where the calling shell is standing.** `--cwd` moves the
  first and not the second, so `main` keeps `invoked_from` separately, and that
  is what decides whether a printed command needs a `cd` at all. Comparing
  against `start_dir` makes `j --cwd /project` from /tmp print a bare command
  that the shell then runs in /tmp. `invoked_from` is an `Option`, because a
  process outlives its working directory being deleted and `--cwd` is what you
  reach for then; unknown means printing the `cd` rather than leaving it out.
- **A printed `cd` is checked before it is printed, not by the shell.** For the
  same reason there is no portable grouping there is no portable "cd, or stop",
  so a `cd` that fails in the caller's shell would run the script wherever that
  shell already was. `in_dir_script` returns an error instead, which is what
  `Command::current_dir` does for `exec::run`. The check is
  `metadata(dir.join("."))` and not `is_dir()`: entering a directory needs the
  search bit *on* it, which only resolving a component inside it asks for. It
  also refuses a name that is not UTF-8, since the command is text and the path
  is bytes. `echo_script` is the cosmetic version and falls back to the bare
  script — the direct path hands the bytes to `Command::current_dir` and works.
- The same prefix goes on the `$` echo, so what is on screen is what the child
  is actually given as its working directory.

## Working on the terminal code

- **The menu is drawn on stderr**, so stdout stays free for `--print`. Keep it
  that way: the shell wrapper pipes stdout.
- **Everything the terminal sees goes through `ui::theme::paint_with`.** It
  takes the colour mode as an argument rather than reading the environment,
  because `theme::enabled()` is decided once per process — passing it in is what
  lets a test cover both modes in one run.
- **A frame is built in a `Vec<u8>` and written once.** `stderr` is unbuffered,
  so painting straight to it turns every colour change into its own write.
- **Text that comes from a file in the checkout is untrusted.** An npm script
  name or a make target can carry a real ESC; `ui::truncate` strips control
  characters so that merely opening the menu in a hostile repository cannot run
  escape sequences.
- **Colours may be off** (`NO_COLOR`, `TERM=dumb`). Anything that has to stand
  out then needs `Style::highlight()`, which falls back to reverse video.
- **The cursor is an index into what the search left, not into the entries.**
  `Frame` keeps the full `items` plus `visible`, the indices that match the
  query; `cursor` walks `visible`. Reading `items[cursor]` anywhere would pick
  the wrong entry as soon as a filter is on — go through `MenuState::selected`.
- **The search string lives on the frame, not on the menu.** That is what makes
  a submenu open unfiltered while going back restores the parent's filter. The
  `searching` flag in `run` is only about where typing goes.
- **The search row is drawn in place of the blank line under the title**, so
  turning the search on does not take a row off the list. `CHROME_ROWS` counts
  that blank line; if the row ever moves, the height arithmetic moves with it.
- **The selected entry's help is a block above the status line, and the block is
  reserved for the whole menu.** `description_budget` returns `DESCRIPTION_ROWS`
  when anything in the menu has a `help` and 0 when nothing does;
  `description_lines` always fills exactly that many rows, blanks included.
  **Neither number may depend on the selected entry**: a block sized to the text
  would move the bottom of the list on every keystroke.
  It was an overlay drawn over the entries below (CYBERNEURA-DEV-582), which kept
  the list still but hid the entries it covered — a long description swallowed
  the next few and there was no way to read them (CYBERNEURA-DEV-696). The block
  costs those rows from the list instead, and only in menus that have a `help`
  to show. What does not fit is dropped. `detail_help` wraps the same text over
  as much as half the screen in the detail view, which is where a long help is
  read — and drops the rest in its turn, since neither view scrolls. Both caps
  are documented in `README.md`; move one and the other has to say so.
- **`Ctrl-C` leaves from everywhere, including the search.** It is the one key
  that always stops what is going on; routing it to "cancel the search" would
  make the search the single place where it does not.
- **`Esc` is not simply quit.** The status row offers it as the way to drop a
  filter, so outside the search it clears one when there is one and only leaves
  the menu when there is not (`escape`). `q` and `Ctrl-C` always leave.
- **Letters are text while the search is open**, which is why `classify_search`
  maps movement to the arrows and to `Ctrl-N` / `Ctrl-P` only, and why the
  search string has no cursor of its own (that would need ← and →).

## Verifying a change

`cargo test` is the whole of it in a sandbox — a TUI cannot be driven without a
pty, and agents often cannot open one (`openpty: Operation not permitted`).
Rather than skipping verification, render a frame and assert on the bytes:
`ui::render(menu, cols, rows, color)` returns the frame as a `Vec<u8>` without
touching a terminal, and the tests in `src/ui/mod.rs` show the pattern.

Where a pty *is* allowed, Python's `pty.fork` drives the real binary end to end
without adding a dependency here: fork, write the keys, read what comes back.
Point stdout at a pipe rather than the pty to read `--print` on its own. That is
how the working-directory behaviour was checked by hand; it is not part of
`cargo test`, since it does not run everywhere.

When a test claims to guard behaviour, check that it actually fails when the
behaviour is removed. Several assertions here look convincing while passing on
mutated code.

Ask the user to run `cargo run` when the change is visual; they have a terminal.

## Working on the skill

`skills/j-menu/SKILL.md` restates the configuration format for an agent that has
not read this repository. Nothing compiles it, so it rots silently: check any
claim against the implementation rather than against `README.md`, which itself
carries hedges (`dirs::config_dir()` is not `~/.config` on macOS) that are easy to
drop when condensing.

That it is still discoverable is checkable:

```shell
npx skills add . --list   # must report the skill with its name and description
```

## Releasing

Changing the version in `Cargo.toml` on `main` is what releases.
`.github/workflows/tag-on-version-change.yml` tags the merge commit when that
version has not been released -- it asks the releases API rather than reading
the diff, so a squash, a rebase and a direct push all behave the same -- and
`cargo-dist` (`dist-workspace.toml`, `.github/workflows/release.yml`) builds the
macOS and Linux binaries from that tag. Windows is deliberately not a target: a
menu entry is a shell script run through `$SHELL -c`.

Nothing is pushed to `cyberneura/homebrew-tap` from here. The tap updates its
own cask and formula files hourly from each project's latest release, which is
what keeps a token that can write to it out of this repository.
