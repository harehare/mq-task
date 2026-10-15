# mq-task

`mq-task` is a task runner that executes code blocks in Markdown files, selected by section title. Your `README.md` (or any Markdown file) *is* the task file: the documentation and the automation are the same text.

It is built on [mq](https://mqlang.org/), a jq-like tool for Markdown, which parses the document and extracts sections.

````markdown
## build

```bash
cargo build --release
```

## ci

```meta
depends = ["lint", "test", "build"]
parallel = true
```

```bash
echo "all checks passed"
```
````

```sh
mq-task ci
```

## Features

- Run code blocks from any section, in any language with a configured runtime
- Task dependencies, with parallel fan-out / fan-in and calls with arguments
- Loops (`for` over lists, globs and matrices)
- Conditions (`if`), retries, timeouts, and output passing between tasks
- Import tasks from other Markdown files
- Skip up-to-date tasks (`sources` / `outputs`) and `--watch` mode
- Dependency graph (text and Mermaid) and shell completions
- Interactive TUI with search, argument prompts and live progress

> [!WARNING]
> `mq-task` is under active development.

Head to [Getting Started](start/index.md) to install it and run your first task.
