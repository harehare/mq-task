# Exit codes

`mq-task` exits with the code of the task's process, so it composes with `&&`, CI and git hooks.

- A task killed by `timeout` exits with `124`.
- Under `--keep-going`, the first failure's code is used.
