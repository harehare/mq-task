# Variables

`vars` gives names to values, so several tasks can share them. A value is either fixed or the output of a shell command.

````markdown
```meta
vars = { NAME = "app", VERSION = { sh = "git describe --tags" }, IMAGE = "${NAME}:${VERSION}" }
```

## build

```bash
docker build -t "$IMAGE" .
```
````

A `meta` block before the first heading defines variables for every task; a task's own `meta` adds to them or replaces them. Each variable reaches the task as an environment variable, whatever language the code block uses.

## Values

| Form | Value |
|---|---|
| `NAME = "app"` | the string; numbers and booleans are turned into strings |
| `VERSION = { sh = "git describe" }` | the output of the command, without trailing newlines |

Commands run with `sh -c` (`cmd /C` on Windows) in the task's working directory. If one fails, the task does not run and the error shows the command, its exit code and the first line of its error output.

## Referring to other variables

`${NAME}` inside a value, or inside an `sh` command, is replaced by that variable:

```toml
vars = { REGISTRY = "ghcr.io/me", IMAGE = "${REGISTRY}/app:${VERSION}", VERSION = { sh = "git describe" } }
```

- Order does not matter; mq-task evaluates what a variable needs first. A loop (`A` needs `B` needs `A`) is an error that names the path.
- `${NAME}` can also name a `.env` value, an `env` entry, a [parameter](params-env.md) (`${MX_PARAM_PROFILE}`), a loop variable (`${MX_ITEM}`), a dependency's output (`${MX_OUTPUT_VERSION}`) or any variable of the process environment.
- With `for = { from = "task" }` the item is known only when the task runs, so a variable that uses `${MX_ITEM}` (directly or through another variable) is evaluated once per item. Variables that do not use it are evaluated once.
- A variable that mentions its own name extends the outer value: `PATH = "${PATH}:/opt/tools"`.
- In a plain value an unknown name is an error. Inside an `sh` command it is left alone, so the shell can use its own `${i}` or `${x:-default}`.
- `$${NAME}` writes a literal `${NAME}`. Only the braced form is expanded: `$NAME` and `$(...)` are left to the shell.
- Variables that were evaluated earlier are also exported to later `sh` commands. Mention a variable as `${NAME}` when you depend on it, so the order is guaranteed.

## Precedence

From lowest to highest: `.env` files, `vars`, `env`, [parameters](params-env.md), `--env`. A variable of the same name as an `env` entry or a `--env` value is not evaluated at all, and the variables that refer to it see that value:

```sh
mq-task release --env VERSION=2.0.0   # the `git describe` command is skipped
```

## When commands run

- A command runs **once per run** for the same text, directory and environment, however many tasks use it, and again on the next run (`--watch` evaluates it afresh each time). Tasks whose environment differs (for example `REGION` set by `env`) each get their own result.
- Commands run before the task starts, so they run even when the task is then skipped (`if` is false, it is up to date) or its `prompt` is declined. They need the variables to decide that, so keep them free of side effects.
- A command is stopped by the task's `timeout` and when the run is cancelled, like the task itself.
- Variables are part of what decides whether a task is up to date: if a computed value changes, a task with `sources` runs again.
- `if`, `requires` and `status` see the variables.
- `--dry-run` does not run commands; it shows `<sh: command>` instead.

## Imports

The variables of a document-wide `meta` block in an imported file apply to that file's tasks, as `env` does.
