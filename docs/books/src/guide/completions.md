# Shell Completions

`mq-task` completes subcommands, options **and the task names of your task file**:

```sh
mq-task completions zsh  > ~/.zfunc/_mq-task
mq-task completions bash > /etc/bash_completion.d/mq-task
mq-task completions fish > ~/.config/fish/completions/mq-task.fish
```

Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

The installed script asks `mq-task` for candidates every time you press Tab, so new tasks show up immediately:

```text
$ mq-task de<TAB>
deploy  (Ship it to production.)
```

- Task names and aliases are offered, with the first line of the description where the shell can show it. Private tasks (`_name`) are hidden.
- The file is found like for a run: `MQ_TASK_FILE`, else `tasks.md`/`README.md` searched upwards from the current directory. A `-f` typed earlier on the same command line is not taken into account.
- If there is no task file, or it cannot be parsed, nothing is offered and no error is shown.
