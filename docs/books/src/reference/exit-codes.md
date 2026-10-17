# Exit codes

`mq-task` exits with the code of the task's process, so it composes with `&&`, CI and git hooks.

- A task killed by `timeout` exits with `124`.
- Under `--keep-going`, the first failure's code is used.
- Other errors (a bad task file, an unknown task, a declined prompt) exit with `1`.

## Which task failed

A failure names the task, and the loop item for `for` loops, so you do not have to guess from the output:

```text
Error: Task exited with status code 4 (in task 'build')
Error: Task exited with status code 5 (in task 'each[b]')
```

Timeouts, missing parameters or variables and declined prompts already name the task in their own message.
