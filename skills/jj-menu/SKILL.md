---
name: jj-menu
description: Write and maintain jj-menu configuration files (.jj-menu.yaml / .toml / .json) — the per-project TUI menu reached by typing `jj`. Use when adding, editing or debugging menu entries, submenus, prompted arguments, commands run in parallel, merge behaviour or automatic launchers, or when the user mentions jj-menu, `.jj-menu.yaml`, or asks to put a command "in the jj menu".
---

# jj-menu configuration

`jj-menu` is a TUI launcher: the user types `jj`, picks an entry, and the entry's
shell script runs with the TTY attached. Entries come from configuration files
plus automatic launchers. Unix only.

Full documentation: https://github.com/cyberneura/jj-menu

## Where the configuration lives

Files are searched in the current directory and every ancestor, nearest first,
then the per-user `config.{yaml,yml,toml,json}` under the platform configuration
directory — `~/.config/jj-menu/` on Linux, **`~/Library/Application Support/jj-menu/`
on macOS**. Every file found is merged, in that order, unless one of them opts out
with `merge: false` (below).

Base names: `.jj-menu`, `_jj-menu`, `jj-menu`, and the `.local` variants
(`.jj-menu.local`, …). Extensions: `.yaml`, `.yml`, `.toml`, `.json`.
`.cson` is ignored, not an error.

Within one directory the shared file loads before the `.local` one, so a
`.jj-menu.local.yaml` holds personal entries and stays out of the repository.

**Adding an entry: choose the file by scope, then reuse it.**
`jj-menu --show-config` lists exactly the files that were loaded, but those span
scopes — an ancestor's file is shared with every sibling project, and the
per-user one is outside the checkout altogether. Reuse the loaded file whose
scope matches the request; when only broader ones are loaded, create the right
file rather than editing what is there, or the entry turns up in projects that
never asked for it.

| Scope of the request | File |
| --- | --- |
| This repository, shared with whoever clones it | `.jj-menu.yaml` at the repository root |
| This repository, yours only | `.jj-menu.local.yaml` — check that it is gitignored |
| Every project | the per-user file |

## Format

```yaml
menu:
  - title: Run the dev server
    shell: pnpm dev

  - title: Clean and rebuild
    # A list is joined into one script and runs in a single shell,
    # so `cd` carries over to the next line.
    shell:
      - cd build
      - make clean
      - make -j

  - title: Dev servers
    # Each entry is its own shell and they all run at once.
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

A bare list with no `menu:` key is also valid. The same structure works in TOML
(`[[menu]]`) and JSON.

| Field | Meaning |
| --- | --- |
| `title` | Label. Defaults to `shell`. |
| `shell` | Command, or a list of commands run as one script. |
| `parallel` | Commands run at the same time, one shell each. |
| `help` | Description shown next to the entry, and in the detail view (`l` / `→`). |
| `submenu` | Nested entries. |
| `args` | Values prompted for and substituted into `shell`. |

### parallel

A `shell` list runs sequentially in one shell; `parallel` starts every entry
under it in its own shell at the same time and finishes when the last one exits.
Use it for the things a person would otherwise open two terminals for — a dev
server plus its API, a watcher plus a test runner.

A member takes **`title` and `shell` only** (`shell` may be a list, which then
runs as one script in that member's shell). `submenu`, `help` and `args` are not
accepted there — a member is not a menu level. `args` go on the entry that owns
the group and are substituted into every member, so the value is asked for once.

**`shell` and `parallel` on the same entry is an error**, not a combination.

What to tell the user about it:

- Ctrl-C stops the whole group, including what a command started rather than
  exec'd; a second one kills whatever ignored the first.
- The exit code is the first failure in written order, or 0 if all succeed. A
  command killed by the interrupt reports `128 + signal`, so an interrupted
  group normally exits 130.
- Output interleaves, and stdin is `/dev/null` — nothing in a group can prompt
  for input. Anything interactive needs a plain `shell` entry.
- The shell wrapper cannot evaluate a group, so `cd` in one has no effect on the
  user's shell (it never could: each member is a separate process).

### args

Each entry has `name` (the `{name}` placeholder in `shell`), optionally `prompt`
and `default`. Arguments are prompted when the entry runs, not in the detail
view.

Substitution is verbatim — the value is pasted into the script before the shell
parses it, and jj-menu never escapes it. **Quoting is the template's job**:
`rg {pattern}` lets the input carry flags and its own quoting, `rg "{pattern}"`
keeps whitespace in a single argument. Double quotes do **not** make it literal —
`$(...)`, backticks and `$VAR` still expand inside them, and a `"` in the input
ends the quoted section. Treat the value as code the user typed, not as data.

A placeholder with no matching argument is left alone, so `${HOME}` and `a{1,2}`
survive.

### merge: false

```yaml
merge: false
menu:
  - title: Only this
    shell: echo hi
```

A file marked this way is skipped when another file has already been loaded.
Since loading is nearest-first, it makes the file a **fallback**: it contributes
only when nothing closer to the working directory exists. Use it on a
repository-wide or per-user file whose entries would be noise inside a directory
that has its own menu. It never affects the nearest file, which is always loaded
first.

## Automatic launchers

Entries are also derived from `package.json` scripts (run with the package
manager implied by the nearest lock file, npm when there is none), `Makefile`
targets that need no extra arguments, Cargo (`build` / `test` / `check`
unconditionally, `run` only when the package has a binary — one entry per binary
when there are several, none for a virtual workspace or a library-only package —
and `fmt` / `clippy` when the component is installed) and Gradle.
**This happens whether or not a configuration file
exists** — writing one does not turn the launchers off. Configured entries come
first, then one group per source; a lone group is flattened into the top level
when nothing is configured.

Only entries that can run as-is are listed. Make pattern rules, targets built
from variables, and anything inside `define` or a conditional are skipped; so
are npm scripts whose name starts with `-`. Tools taking positional arguments
(`fab`, `cap`, …) are not scanned at all — **define those explicitly with
`args`**.

Turning them off:

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

The **whole block** is decided by the nearest file that sets `auto_launchers` at
all; a file further up is then ignored, rather than merged switch by switch. So
`{makefile: false}` in the nearest file silences a `{cargo: false}` in an
ancestor, and cargo stays on — every switch you need has to be in the same file.

## Checking a change

**Do not run bare `jj-menu` yourself.** It needs a terminal on both stdin and
stderr and refuses without one — `jj-menu: no terminal available (stdin is not a
TTY)`, exit 1. That message means your shell has no TTY, not that the
configuration is broken. Given a TTY it blocks until a key is pressed. Use:

```shell
jj-menu --show-config   # lists the configuration files that were loaded, then exits
```

A file that fails to parse is reported here, including one with an unknown key:
every configuration struct rejects fields it does not know, so a typo is an error
rather than a silently ignored line. Do not invent fields.

**This only covers the files that actually applied.** A `merge: false` file that
was skipped is never parsed — an inactive fallback deliberately cannot abort
startup with an error in a part of it nobody reads. So a clean `--show-config` is
no evidence that a fallback is valid: run it from a directory where that file is
the nearest one.

To see the menu, ask the user to run `jj`.

`--print` prints the selected command instead of running it, but still opens the
menu, so it is for the user, not for you.

## Installing jj-menu

Only if it is missing (`command -v jj-menu`):

```shell
brew install cyberneura/tap/jj-menu    # or: cargo install jj-menu
```

The short `jj` command is a shell function — without it, `cd` or `export` in an
entry cannot affect the user's shell:

```shell
eval "$(jj-menu --shell-init bash)"    # ~/.bashrc
eval "$(jj-menu --shell-init zsh)"     # ~/.zshrc
jj-menu --shell-init fish | source     # ~/.config/fish/config.fish, fish 3.4+
```

Invoking the binary by a path emits a function that calls that path, so a
build outside `PATH` still gives a working `jj`.

The three snippets differ, so pass the shell that is actually being configured —
the zsh one pushes to history with `print -s`, which bash does not have.
