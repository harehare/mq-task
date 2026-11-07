# Exit codes

`mq-task` exits with the code of the task's process, so it composes with `&&`, CI and git hooks.

- A task killed by `timeout` exits with `124`.
- A run ended by `SIGTERM` or `SIGHUP` exits with `128` plus the signal number (`143`, `129`), as a shell reports it ([details](#signals)).
- Under `--keep-going`, the first failure's code is used.
- Other errors (a bad task file, an unknown task, a declined prompt) exit with `1`.

## Which task failed

A failure names the task, and the loop item for `for` loops, so you do not have to guess from the output:

```text
Error: Task exited with status code 4 (in task 'build', tasks.md:12)
Error: Task exited with status code 5 (in task 'each[b]', tasks.md:30)
```

Timeouts, missing parameters or variables and declined prompts already name the task in their own message.

## Where it failed

When a code block fails, the message also gives the line of its fence in the task file, `file:line`, so a terminal or editor can jump to it. For an imported task that is the line in the imported file:

```text
Error: Task exited with status code 3 (in task 'build', tasks.md:21)
```

In GitHub Actions the annotation `mq-task` prints carries the same `file` and `line`, so the error appears on that line of the Markdown in the pull request.

## Signals

`SIGINT` (Ctrl+C) goes to the whole foreground process group, so the running task receives it directly. `SIGTERM` and `SIGHUP` (a CI cancel, `kill`, a closed terminal) reach only `mq-task`, so it stops the running task for you: first with `SIGTERM` (Ctrl+Break on Windows), and after 300 ms by force. Nothing new is started, but the task's [`defer`](../guide/defer.md) tasks still run, and `mq-task` exits with `128 + signal`.

On Windows each task that is cut off from the console (parallel runs, the TUI, `timeout`) runs in a Job Object, so stopping it also stops everything it started.
