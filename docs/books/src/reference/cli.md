# CLI

```text
mq-task [OPTIONS] [TASK]... [-- <ARGS>...] [COMMAND]
```

Several tasks can be named: `mq-task lint test` runs them in that order as one run, so a dependency they share runs once. Arguments after `--` belong to a single task, so they cannot be combined with several.

## Commands

| Command | Description |
|---|---|
| `run` | Run a task |
| `list` | List tasks (`-a/--all` includes private; `--json` prints `name`, `description`, `aliases`, `depends`, `params`, `private` and `languages` as JSON) |
| `tui` | Select and run a task interactively |
| `check` | Look for mistakes without running anything (`--strict`, `--json`; see [Checking a Task File](../guide/check.md)) |
| `graph [TASK]` | Print the dependency graph (`--format text\|mermaid`) |
| `completions <SHELL>` | Generate shell completions |
| `init` | Write a starter `tasks.md` (`-o` for another path); `init --config` writes a sample `mq-task.toml` |

## Options

| Option | Description |
|---|---|
| `-f, --file <FILE>` | Markdown file (default: nearest `tasks.md`, else `README.md`; env `MQ_TASK_FILE`) |
| `-c, --config <CONFIG>` | Configuration file (default: `mq-task.toml` next to the task file; env `MQ_TASK_CONFIG`) |
| `-r, --runtime <LANG:COMMAND>` | Override a runtime |
| `-e, --execution-mode <MODE>` | `stdin`, `file` or `arg` for overrides |
| `--lang <LANG>` | Only run code blocks of this language |
| `-v, --verbose` | Print each code block (and its directory) on stderr before it runs |
| `--dry-run` | Show what would run |
| `--env <KEY=VALUE>` | Set an environment variable (repeatable) |
| `-d, --dir <PATH>` | Working directory |
| `--summary` | Describe the task (description, dependencies, parameters, options) instead of running it |
| `--status` | Say whether each task would run (up to date per `status`, or `sources`/`outputs`) without running anything; exit `1` when any would ([details](../guide/cache-watch.md#checking-without-running)) |
| `--no-deps` | Run only the named tasks, not their `depends` |
| `-q, --quiet` | Print only task output and errors: no `▶`, `↳` or `✔` lines |
| `--timings` | Print how long each task took, and the totals, on stderr when the run ends |
| `-w, --watch` | Re-run when `sources`, `dotenv` files or the task file change |
| `--interval <DURATION>` | How often `--watch` checks for changes (default `300ms`, minimum `50ms`) |
| `--parallel` | Run dependencies concurrently |
| `-j, --jobs <N>` | Max concurrent tasks |
| `--output <MODE>` | `interleaved`, `prefixed` or `group` (see [Parallel Execution](../guide/parallel.md#output-modes)) |
| `-k, --keep-going` | Continue independent tasks after a failure |
| `--no-cache` | Ignore up-to-date checks (`sources`/`outputs` and `status`) |
| `-y`, `--yes` | Answer yes to every `prompt` |
