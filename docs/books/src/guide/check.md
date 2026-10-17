# Checking a Task File

`mq-task check` looks for mistakes in the task file without running anything, and reports **all** of them at once. Use it in CI or in a pre-commit hook.

```text
$ mq-task check
error: task 'build': `meta` block: unknown key `retires` on line 1 (did you mean `retries`?)
error: task 'deploy': depends on unknown task 'buil' (did you mean `build`?)
error: task 'test': circular dependency: test -> lint -> test
error: task 'lint': no runtime is configured for language 'cobol' (used by 'lint'); add it under [runtimes] in mq-task.toml
warning: task 'run': the name 'run' is also a command; run the task with `mq-task run run`

tasks.md: 4 errors, 1 warning
```

The command exits with `1` when there are errors, and with `0` otherwise. `--strict` makes warnings fail too.

## What is checked

| Problem | Severity |
|---|---|
| Unknown `meta` keys and invalid values (durations, `vars`, `for`, ...), in the file and in its imports | error |
| `depends`, `defer` or `for = { from }` naming a task that does not exist, with a suggestion | error |
| Dependency cycles, reported once as `a -> b -> a` | error |
| `for = { from }` pointing at a task that is not a dependency or does not set `capture = true`; a deferred task that has dependencies | error |
| A language without a configured runtime | error |
| A runtime that is configured but not found in `PATH` | warning |
| Invalid glob patterns in `sources`, `outputs` and `for = { glob }` | error |
| An `if` condition that does not parse | error |
| A task defined twice, an alias that clashes with another name, a parameter declared twice | error |
| A task or alias named like a command (`run`, `list`, ...) | warning |
| An `imports` file that is missing or imported twice under the same name | error |

Headings that contain nothing to run (such as a document title) are not tasks and are not checked.

## Options

| Option | Meaning |
|---|---|
| `-f, --file <FILE>` | the file to check (default: the nearest `tasks.md`, else `README.md`) |
| `-c, --config <CONFIG>` | the configuration whose `[runtimes]` are used |
| `--strict` | exit with `1` on warnings too |
| `--json` | print `{ "file", "tasks", "diagnostics": [{ "severity", "task", "message" }] }` |

## In CI

```yaml
- run: mq-task check --strict
```
