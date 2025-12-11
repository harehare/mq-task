<h1 align="center">mq-task</h1>

Markdown Task Runner

[![ci](https://github.com/harehare/mq-task/actions/workflows/ci.yml/badge.svg)](https://github.com/harehare/mq-task/actions/workflows/ci.yml)

`mq-task` is a task runner that executes code blocks in Markdown files based on section titles.
It is implemented using [mq](https://github.com/harehare/mq), a jq-like command-line tool for Markdown processing, to parse and extract sections from Markdown documents.

![demo](assets/demo.gif)

> [!WARNING]
> `mq-task` is currently under active development.

> [!NOTE]
> This project was previously named `mx` and has been renamed to `mq-task`.

## Features

- Execute code blocks from specific sections in Markdown files
- Configurable runtimes for different programming languages
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
- Install it to `~/.mq/bin/`
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

### List available tasks

```bash
# List tasks from README.md (default)
mq-task

# List tasks from a specific file
mq-task -f tasks.md
mq-task list --file tasks.md
```

### Initialize configuration

```bash
mq-task init
```

This creates an `mq-task.toml` file with default runtime settings.

## Configuration

Create an `mq-task.toml` file to customize runtime behavior:

```toml
# Heading level for sections (default: 2, i.e., ## headings)
heading_level = 2

# Runtimes configuration
# Simple format: language = "command"
# The execution mode defaults to "stdin"
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
jq = "jq"

# Detailed format with execution mode
# Execution modes: "stdin" (default), "file", or "arg"
# - stdin: Pass code via standard input
# - file: Write code to a temporary file and pass it as an argument
# - arg: Pass code as a command-line argument

[runtimes.go]
command = "go run"
execution_mode = "file"  # Go requires file-based execution

[runtimes.golang]
command = "go run"
execution_mode = "file"

[runtimes.mq]
command = "mq"
execution_mode = "arg"  # mq uses query as argument
```

You can also mix both formats:

```toml
[runtimes]
python = "python3"  # Simple format, uses default stdin mode

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

## License

MIT
