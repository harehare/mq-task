# Quick Start

A task is a **heading** followed by one or more **code blocks**. `mq-task init` writes a small `tasks.md` to start from; or create one yourself (the tasks can also live in your `README.md`):

````markdown
## hello

```bash
echo "Hello from mq-task"
```
````

List the tasks and run one:

```sh
mq-task            # list tasks in tasks.md (or README.md)
mq-task hello      # run the "hello" task
mq-task -f docs/dev.md hello   # use another file
```

`mq-task hello` is shorthand for `mq-task run hello`.

## Passing arguments

Arguments after `--` are exposed as `MX_ARGS` (space-joined) and `MX_ARG_0`, and so on

````markdown
## greet

```bash
echo "Hello, $MX_ARG_0"
```
````

```sh
mq-task greet -- world
```

## Dry run

`--dry-run` prints what would be executed without running anything.

## Next

- [Writing Tasks](../guide/tasks.md)
- [Dependencies](../guide/dependencies.md)

## Which file is used

Without `-f`, mq-task looks for `tasks.md` and falls back to `README.md`. The search starts in the current directory and walks up to the repository root (the directory holding `.git`), so tasks work from any subdirectory. A `tasks.md` anywhere on the way up wins over a nearer `README.md`.

When the file is found in a parent directory, mq-task switches to that directory before running (like `just`), so relative paths in tasks refer to the project root. It prints `using <path>` in that case. With `-f` (or `MQ_TASK_FILE`) nothing is searched and the working directory is left alone.
