# Aliases, Default and Private Tasks

## Aliases

````markdown
## build

```meta
alias = ["b"]
```
````

`mq-task b` runs `build`.

## Default task

```toml
# mq-task.toml
default_task = "build"
```

`mq-task` with no task name runs `build`.

## Private tasks

A title starting with `_`, or `private = true` in `meta`, hides the task from `list`/`tui` (`list --all` shows it). It can still be run and used as a dependency.

````markdown
## _cleanup

```bash
rm -rf tmp/
```
````
