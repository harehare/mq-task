<div align="center">
  <img src="assets/logo.svg" width="96" height="96" alt="mq-task logo" />

  <h1>mq-task</h1>

[![ci](https://img.shields.io/github/actions/workflow/status/harehare/mq-task/ci.yml?style=flat-square&logo=github-actions&label=ci)](https://github.com/harehare/mq-task/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/harehare/mq-task?style=flat-square&logo=github&label=release)](https://github.com/harehare/mq-task/releases)
[![license](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)](LICENSE)

</div>

`mq-task` is a task runner that executes code blocks in Markdown files based on section titles.
It is implemented using [mq](https://github.com/harehare/mq), a jq-like command-line tool for Markdown processing, to parse and extract sections from Markdown documents.

![demo](assets/demo.gif)

> [!WARNING]
> `mq-task` is currently under active development.

## Features

- Execute code blocks from specific sections in Markdown files
- Task dependencies with automatic execution ordering
- Named task parameters with defaults, plus positional args
- Per-run `--env`/`--dir` overrides, plus `meta`-declared defaults for environment variables and working directory (per task or document-wide)
- Default task, aliases, and private (hidden) tasks
- Configurable runtimes for different programming languages, with real exit codes and interactive stdin
- Support for custom heading levels
- TOML-based configuration
- Built on top of the mq query language

## Installation

### Quick Install

```bash
curl -sSL https://raw.githubusercontent.com/harehare/mq-task/refs/heads/main/bin/install.sh | bash
```

The installer will:
- Download the latest mq binary for your platform
- Install it to `~/.local/bin/`
- Update your shell profile to add mq to your PATH

### Cargo

```sh
$ cargo install --git https://github.com/harehare/mq-task.git
```

## Usage

### Run a task (shorthand)

```bash
# Run from README.md (default)
mq-task "Task Name"

# Run from a specific file
mq-task -f tasks.md "Task Name"
```

### Run a task (explicit)

```bash
mq-task run "Task Name"
mq-task run --file tasks.md "Task Name"
```

### Pass arguments to a task

You can pass arguments to your task using `--` separator:

```bash
# Pass arguments to a task
mq-task "Task Name" -- arg1 arg2 arg3

# With explicit run command
mq-task run "Task Name" -- arg1 arg2 arg3

# From a specific file
mq-task -f tasks.md "Task Name" -- arg1 arg2
```

Arguments are accessible via environment variables:
- `MX_ARGS`: All arguments joined by space (e.g., "arg1 arg2 arg3")
- `MX_ARG_0`, `MX_ARG_1`, ...: Individual arguments

Example in a Markdown task:

````markdown
## My Task

```bash
echo "All args: $MX_ARGS"
echo "First arg: $MX_ARG_0"
echo "Second arg: $MX_ARG_1"
```
````

### Environment variables and working directory

Like `just`, you can set environment variables and a working directory for a task at invocation time — no declaration needed in the Markdown file:

```bash
# Set one or more environment variables (repeatable)
mq-task deploy --env REGION=eu --env DEBUG=1

# Run the task's commands in a specific working directory
mq-task deploy --dir ../other-project

# Combine both
mq-task run deploy --env REGION=eu --dir ../other-project
```

`--env KEY=VALUE` variables are available in the task's shell alongside `MX_ARGS`/`MX_PARAM_*`. `--dir` applies to every command in the task (and its dependencies) for that run. Both flags are available on the shorthand form, `run`, and `tui`.

You can also declare default environment variables directly in a task's `meta` block:

````markdown
## deploy

```meta
env = ["REGION=staging", "LOG_LEVEL=info"]
```

```bash
echo "deploying to $REGION ($LOG_LEVEL)"
```
````

A CLI `--env` flag of the same name overrides the `meta` default for that run:

```bash
mq-task deploy                    # REGION=staging
mq-task deploy --env REGION=prod  # REGION=prod
```

A working directory can be declared the same way with `dir` (relative paths resolve against the directory `mq-task` was invoked from):

````markdown
## deploy

```meta
dir = "services/api"
```

```bash
pwd  # services/api
```
````

The CLI `--dir` flag overrides a `meta`-declared `dir` for that run:

```bash
mq-task deploy                        # runs in services/api
mq-task deploy --dir services/worker  # runs in services/worker instead
```

#### Document-wide defaults

A `meta` block placed **before the first heading** in the file applies to every task, not just one. It's useful for defaults you'd otherwise have to repeat in each task's own `meta` block:

````markdown
```meta
env = ["REGION=staging", "LOG_LEVEL=info"]
dir = "services/api"
```

## deploy

```bash
echo "deploying to $REGION ($LOG_LEVEL) from $(pwd)"
```

## build

```bash
echo "building in $(pwd)"
```
````

Both tasks inherit `REGION`, `LOG_LEVEL`, and `dir` from the document-wide block. Precedence, from lowest to highest, is: document-wide `meta` → task's own `meta` → CLI `--env`/`--dir`. A task's `meta` only needs to declare the keys it wants to change:

````markdown
```meta
env = ["REGION=staging"]
```

## deploy

```meta
env = ["REGION=prod"]
```

```bash
echo "$REGION"   # prod — the task's own meta wins
```

## build

```bash
echo "$REGION"   # staging — falls back to the document-wide default
```
````

### Task dependencies

You can declare dependencies between tasks using a `meta` code block (TOML format). When a task is run, its dependencies are automatically executed first in the correct order.

````markdown
## format

```bash
cargo fmt
```

## lint

```meta
depends = ["format"]
```

```bash
cargo clippy
```

## test

```meta
depends = ["lint"]
```

```bash
cargo test
```
````

Running `mq-task test` will automatically execute `format → lint → test` in order:

```
▶ test
↳ format (dependency)
...
↳ lint (dependency)
...
(test output)
```

- Multiple dependencies are supported: `depends = ["format", "lint"]`
- Shared dependencies are executed only once even if multiple tasks depend on them
- Circular dependencies are detected and reported as an error

### Named parameters

Declare parameters in the `meta` block. A bare name is required; `name=value` sets a default.

````markdown
## deploy

```meta
params = ["env=staging", "region"]
```

```bash
echo "deploying to $MX_PARAM_ENV / $MX_PARAM_REGION"
```
````

```bash
mq-task deploy -- region=eu           # env falls back to "staging"
mq-task deploy -- prod eu             # positional, filled in declaration order
mq-task deploy -- env=prod region=eu  # named
```

Parameters are exposed as `MX_PARAM_<NAME>` environment variables. A required parameter with no value bound is an error.

### Aliases and the default task

Give a task alternate names with `alias` in its `meta` block:

````markdown
## build

```meta
alias = ["b"]
```

```bash
cargo build
```
````

`mq-task b` now runs `build`. Set `default_task` in `mq-task.toml` to run a task when `mq-task` is invoked with no task name:

```toml
default_task = "build"
```

### Private tasks

A task whose title starts with `_`, or whose `meta` block has `private = true`, is hidden from `list`/`tui` output but can still be run directly or used as a dependency:

````markdown
## _cleanup

```bash
rm -rf tmp/
```
````

### List available tasks

```bash
# List tasks from README.md (default)
mq-task

# List tasks from a specific file
mq-task -f tasks.md
mq-task list --file tasks.md

# Include private tasks
mq-task list --all
```

### Initialize configuration

```bash
mq-task init
```

This creates an `mq-task.toml` file with default runtime settings.

## Configuration

Create an `mq-task.toml` file to customize runtime behavior:

```toml
# Runtimes configuration
# Simple format: language = "command", execution mode defaults to "file"
[runtimes]
bash = "bash"
sh = "sh"
python = "python3"
ruby = "ruby"
node = "node"
javascript = "node"
js = "node"
php = "php"
perl = "perl"

# Detailed format with execution mode
# Execution modes: "file" (default), "stdin", or "arg"
# - file: write code to a temp file and run it as an argument (keeps stdin interactive)
# - stdin: pipe code into the command's standard input (stdin unavailable to the script itself)
# - arg: pass code as a command-line argument

[runtimes.go]
command = "go run"
execution_mode = "file"

[runtimes.jq]
command = "jq"
execution_mode = "arg"  # jq's filter is a CLI argument, not read from stdin

[runtimes.mq]
command = "mq"
execution_mode = "arg"  # mq uses query as argument
```

Use `execution_mode = "stdin"` only when a command needs the code piped in literally (e.g. `psql`, `redis-cli`) — the task script then can't also read from the terminal.

You can also mix both formats:

```toml
[runtimes]
python = "python3"  # Simple format, uses default file mode

[runtimes.go]       # Detailed format with custom execution mode
command = "go run"
execution_mode = "file"
```

```bash
# Using shorthand (from tasks.md by default)
mq-task Build

# From a specific file
mq-task -f tasks.md Build

# Using explicit run command
mq-task run Build
mq-task run --file tasks.md Build
```

`default_task` (top-level, outside `[runtimes]`) sets which task runs when `mq-task` is invoked with no task name — see [Aliases and the default task](#aliases-and-the-default-task).

## Exit codes

`mq-task` exits with the same code the task's process exited with, so it composes correctly with `&&`, CI, and git hooks.

## License

MIT
