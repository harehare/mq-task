<div align="center">
  <img src="assets/logo.svg" width="96" height="96" alt="mq-task logo" />

<a href="https://task.mqlang.org">Visit the site 🌐</a>
—
<a href="https://task.mqlang.org/book/">Read the book 📖</a>

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

- Run the code blocks under any heading, in any language with a configured [runtime](https://task.mqlang.org/book/start/configuration.html)
- [Dependencies](https://task.mqlang.org/book/guide/dependencies.html), [parallel execution](https://task.mqlang.org/book/guide/parallel.html), [loops](https://task.mqlang.org/book/guide/loops.html), [`defer`](https://task.mqlang.org/book/guide/defer.html) cleanup and shared [variables](https://task.mqlang.org/book/guide/vars.html)
- [Conditions](https://task.mqlang.org/book/guide/conditions.html), [retries and timeouts](https://task.mqlang.org/book/guide/retries-timeouts.html), [confirmation prompts](https://task.mqlang.org/book/guide/prompts.html) and [output passing](https://task.mqlang.org/book/guide/output.html)
- [Named parameters, env and working directory](https://task.mqlang.org/book/guide/params-env.html), [imports](https://task.mqlang.org/book/guide/imports.html), [aliases and private tasks](https://task.mqlang.org/book/guide/aliases-private.html)
- Skip [up-to-date tasks](https://task.mqlang.org/book/guide/cache-watch.html) and `--watch` mode
- [`mq-task check`](https://task.mqlang.org/book/guide/check.html), [dependency graph](https://task.mqlang.org/book/guide/graph.html), [interactive TUI](https://task.mqlang.org/book/guide/tui.html) and [shell completions](https://task.mqlang.org/book/guide/completions.html)
- Options for scripts and CI: see the [CLI reference](https://task.mqlang.org/book/reference/cli.html)

## Installation

```bash
curl -sSL https://task.mqlang.org/install.sh | bash
```

or with Cargo:

```sh
cargo install --git https://github.com/harehare/mq-task.git
```

## Usage

A task is a heading followed by code blocks. A `meta` block (TOML) adds options such as dependencies, parameters or conditions:

````markdown
## build

Compile the project.

```bash
cargo build --release
```

## test

```meta
depends = ["build"]
sources = ["src/**/*.rs", "tests/**/*.rs"]  # skipped when unchanged
retries = 2
```

```bash
cargo test
```

## deploy

```meta
params = ["env=staging", "region"]   # name=default; a bare name is required
depends = ["test"]
prompt = "Deploy?"
if = 'flag("CI") || which("kubectl")'
```

```bash
./deploy.sh "$MX_PARAM_ENV" "$MX_PARAM_REGION"
```

## ci

```meta
depends = ["lint", "test", "build"]
parallel = true
```

```bash
echo "all checks passed"
```

## report

Code blocks can use any language with a runtime.

```python
print("report generated")
```
````

```bash
mq-task                          # list tasks from tasks.md, else README.md
mq-task build                    # run a task (and its dependencies first)
mq-task deploy -- region=eu      # named parameters; positional args also work
mq-task ci -j 4                  # run dependencies in parallel
mq-task test --watch             # re-run when sources change
mq-task -f docs/dev.md           # use another file
mq-task init                     # write a starter tasks.md
```

## Documentation

- [Quick Start](https://task.mqlang.org/book/start/quickstart.html) and [Configuration](https://task.mqlang.org/book/start/configuration.html) (`mq-task.toml`, runtimes)
- [Guide](https://task.mqlang.org/book/guide/tasks.html), [meta keys](https://task.mqlang.org/book/reference/meta.html), [CLI](https://task.mqlang.org/book/reference/cli.html) and [exit codes](https://task.mqlang.org/book/reference/exit-codes.html)
- [Coming from `just` or Task](https://task.mqlang.org/book/start/migrating.html)

## License

MIT
