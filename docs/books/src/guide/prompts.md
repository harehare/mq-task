# Confirmation Prompts

`prompt` asks for confirmation before a task runs. It is meant for tasks with side effects that are hard to undo.

````markdown
## deploy

```meta
prompt = "Deploy to production?"
```

```bash
./deploy.sh
```
````

```
$ mq-task deploy
? Deploy to production? [y/N] y
```

- `y` or `yes` (any case) confirms; anything else, including end of input, declines and fails the task with exit code `1`.
- `--yes`/`-y` answers yes to every prompt, for scripts and CI. `echo y | mq-task deploy` works too.
- The question is asked after the task's `if` and up-to-date checks, so a skipped task never prompts.
- A [looped](loops.md) task is confirmed once, not once per item.
- `--dry-run` prints the prompt instead of asking.
- Prompts need the terminal, so they cannot be asked in parallel runs, with `--output prefixed` or `group`, or inside the TUI: those fail with a message asking for `--yes`. In the TUI, press `x` to run the task in the terminal instead.
