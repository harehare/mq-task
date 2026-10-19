# Coming from just or Task

mq-task covers what most `justfile`s and `Taskfile.yml`s do, but it is not a drop-in replacement: the task file is a Markdown document, and a few things work differently. This page maps the concepts and says plainly what is missing. There is no automatic converter; moving a file by hand takes a few minutes.

## How mq-task thinks

- A **heading** is a task and the **code blocks** under it are what runs. The prose is the description (`mq-task list`, `--summary`, shell completion).
- A block is one **script**, not one command per line. In `just` every line runs in its own shell; here a `bash` block runs top to bottom in a single shell, so `cd` and variables carry over. Add `set -e` yourself if you want to stop at the first failure.
- The block's **language** picks the interpreter (`bash`, `python`, `node`, ...), so what `just` calls a shebang recipe is simply a ```python block.
- Values reach scripts as **environment variables**, whatever the language. There is no `{{ }}` text substitution into the script (see [Variables](../guide/vars.md)).
- mq-task does not echo commands. `-v` prints each block before it runs.
- Without `-f`, the task file is searched for upwards from where you are (`tasks.md` first), and mq-task changes to its directory before running, like `just`. With `-f`, tasks run where you are.

## From just

| just | mq-task |
|---|---|
| `build:` with indented lines | `## build` with a ```bash block |
| `test: build` | ```` ```meta ```` with `depends = ["build"]` |
| `deploy env="staging":` and `{{env}}` | `params = ["env=staging"]` and `$MX_PARAM_ENV` |
| `*args` / `+args` | the rest of the command line: `$MX_ARGS`, `$MX_ARG_0`, ... |
| `version := "1.0"` | `vars = { version = "1.0" }` (referenced as `$version` or `${version}`) |
| ``v := `git describe` `` | `vars = { v = { sh = "git describe" } }` |
| `set dotenv-load` | `dotenv = [".env"]` |
| `set shell := ["zsh", "-c"]` | runtimes in `mq-task.toml`, e.g. `bash = "zsh"` |
| shebang recipe `#!/usr/bin/env python3` | a ```python block |
| `[private]` or a leading `_` | a leading `_` or `private = true` |
| `[confirm("Sure?")]` | `prompt = "Sure?"` (`--yes` skips it) |
| `[linux]`, `[macos]`, `[windows]`, `[unix]` | `if = 'os() == "linux"'` |
| `env_var("HOME")`, `os()` | `${HOME}` in `vars`; `getenv("HOME")` and `os()` in an `if` condition |
| `alias b := build` | `alias = ["b"]` |
| `import "x.just"` / `mod x` | `imports = ["x.md"]`, used as `x:task` |
| `@recipe` / `set quiet` | the default: nothing is echoed |
| `just --list`, `--summary` | `mq-task`, `mq-task <task> --summary`, `mq-task list --json` |
| `just --choose` | `mq-task tui` |
| `just --dry-run` | `mq-task <task> --dry-run` |
| `just a b` | `mq-task a b` (arguments after `--` go to a single task only) |
| `just --no-deps` | `mq-task <task> --no-deps` |
| `just --quiet` | `mq-task <task> -q` |
| first recipe is the default | `default_task = "build"` in `mq-task.toml` |
| `just --fmt`, `--evaluate` | no equivalent (`mq-task check` validates the file) |

Not available: recipe groups (`[group]`), `{{ }}` interpolation of arguments or functions into the script text, `invocation_directory()` (a task always runs in the file's directory unless you set `dir`), and running a single line of a recipe.

## From Task

| Task | mq-task |
|---|---|
| `tasks:` map, `desc`/`summary` | headings, with the prose as the description |
| `cmds` | the lines of a code block |
| `deps` | `depends`. **Task runs `deps` in parallel by default; mq-task runs them in order** unless the task sets `parallel = true` |
| `task: other` with `vars` inside `deps` | `depends = ["other name=value"]` |
| `vars`, `sh:` | `vars = { A = "x", B = { sh = "cmd" } }` |
| `{{.VAR}}` templates | `$VAR` / `${VAR}`; `${VAR}` inside `vars` values is expanded by mq-task |
| `env`, `dotenv`, `dir` | `env`, `dotenv`, `dir` |
| `sources` / `generates` | `sources` / `outputs` (content checksums, like Task's default) |
| `status` | `status` (commands that exit 0 when the task is up to date) |
| `preconditions` | `requires` for variables, `if` to skip, or a check in the script |
| `for` over a list, a matrix, `sources` | `for = [..]`, `{ matrix = .. }`, `{ glob = .. }`; over a variable: `{ from = "task" }` |
| `defer` | `defer = ["cleanup"]` |
| `ignore_error` | `ignore_error = true` |
| `prompt` | `prompt = "..."` |
| `run: once` | `once = true` |
| `platforms` | `if = 'os() == "linux"'` |
| `silent` | the default (use `-v` to see the code) |
| `internal` | a leading `_` or `private = true` |
| `aliases` | `alias` |
| `includes` | `imports`, namespaced as `file:task` |
| `includes` with `taskfile`, `flatten`, `optional` | `imports = [{ path = "x.md", as = "ns", flatten = true, optional = true }]` |
| `requires: vars` | `requires` |
| `requires: vars` with `enum` | `params` table with `choices` |
| `--watch`, `interval` | `--watch`, `--interval` |
| `--parallel`, `--concurrency` | `--parallel`, `-j` |
| `--force` | `--no-cache` |
| `--list`, `--summary` | `mq-task`, `--summary` |
| `--dry`, `--yes` | `--dry-run`, `--yes` |
| `--status` | `--status` (exit `1` when a task would run) |
| `--silent` | `-q`/`--quiet` |
| `task a b` | `mq-task a b` |
| `sources` with `exclude:` | `sources = ["src/**", "!src/gen/**"]` |
| `--output group` with `group.begin`/`end` | `--output group` (on GitHub Actions it emits `::group::` automatically) |

Not available: Go-template functions and conditionals, per-include `vars`, `method: timestamp`, `label`, calling another task from the middle of a script (use `depends`, `defer`, or run `mq-task other` from the script), and `set`/`shopt` options.

## A small example

A `justfile`:

```just
default: test

build profile="debug":
    cargo build --profile {{profile}}

test: build
    cargo test

deploy env="staging": test
    @echo "deploying to {{env}}"
```

The same tasks in `tasks.md`:

````markdown
## build

```meta
params = ["profile=debug"]
```

```bash
cargo build --profile "$MX_PARAM_PROFILE"
```

## test

```meta
depends = ["build"]
```

```bash
cargo test
```

## deploy

```meta
depends = ["test"]
params = ["env=staging"]
```

```bash
echo "deploying to $MX_PARAM_ENV"
```
````

and `default_task = "test"` in `mq-task.toml`. A `Taskfile.yml` converts the same way: each key under `tasks:` becomes a heading, `cmds` becomes the code block and `deps`, `vars` and the other keys move into the `meta` block as in the table above.

## Checking the result

```sh
mq-task check          # finds typos, missing dependencies, cycles, missing runtimes
mq-task                # lists the tasks
mq-task <task> --dry-run
```
