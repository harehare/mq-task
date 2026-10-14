# CLI

```text
mq-task [OPTIONS] [TASK] [-- <ARGS>...] [COMMAND]
```

## Commands

| Command | Description |
|---|---|
| `run` | Run a task |
| `list` | List tasks (`-a/--all` includes private) |
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
| `-w, --watch` | Re-run when `sources` or the task file change |
| `--parallel` | Run dependencies concurrently |
| `-j, --jobs <N>` | Max concurrent tasks |
| `-k, --keep-going` | Continue independent tasks after a failure |
| `--no-cache` | Ignore up-to-date checks |
