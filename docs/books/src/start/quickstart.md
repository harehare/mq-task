# Quick Start

A task is a **heading** followed by one or more **code blocks**. Create `README.md`:

````markdown
## hello

```bash
echo "Hello from mq-task"
```
````

List the tasks and run one:

```sh
mq-task            # list tasks in README.md
mq-task hello      # run the "hello" task
mq-task -f tasks.md hello   # use another file
```

`mq-task hello` is shorthand for `mq-task run hello`.

## Passing arguments

Arguments after `--` are exposed as `MX_ARGS` (space-joined) and `MX_ARG_0`, `MX_ARG_1`, …

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
