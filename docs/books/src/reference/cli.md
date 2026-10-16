# CLI

```text
mq-task [OPTIONS] [TASK] [-- <ARGS>...] [COMMAND]
```

## Commands

| Command | Description |
|---|---|
| `run` | Run a task |
| `list` | List tasks (`-a/--all` includes private; `--json` prints `name`, `description`, `aliases`, `depends`, `params`, `private` and `languages` as JSON) |
| `tui` | Select and run a task interactively |
| `graph [TASK]` | Print the dependency graph (`--format text\|mermaid`) |
| `completions <SHELL>` | Generate shell completions |
| `init` | Write a sample `mq-task.toml` |

## Options

| Option | Description |
|---|---|
| `-f, --file <FILE>` | Markdown file (default `README.md`) |
| `-c, --config <CONFIG>` | Configuration file |
| `-r, --runtime <LANG:COMMAND>` | Override a runtime |
| `-e, --execution-mode <MODE>` | `stdin`, `file` or `arg` for overrides |
| `--lang <LANG>` | Only run code blocks of this language |
| `--dry-run` | Show what would run |
| `--env <KEY=VALUE>` | Set an environment variable (repeatable) |
| `-d, --dir <PATH>` | Working directory |
| `--summary` | Describe the task (description, dependencies, parameters, options) instead of running it |
| `-w, --watch` | Re-run when `sources`, `dotenv` files or the task file change |
| `--interval <DURATION>` | How often `--watch` checks for changes (default `300ms`) |
| `--parallel` | Run dependencies concurrently |
| `-j, --jobs <N>` | Max concurrent tasks |
| `-k, --keep-going` | Continue independent tasks after a failure |
| `--no-cache` | Ignore up-to-date checks (`sources`/`outputs` and `status`) |
| `-y`, `--yes` | Answer yes to every `prompt` |
