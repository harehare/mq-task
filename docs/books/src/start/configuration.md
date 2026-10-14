# Configuration

`mq-task init` creates an `mq-task.toml` with default runtime settings. Use `-c/--config` to point at another file.

```toml
default_task = "build"

[runtimes]
bash = "bash"
sh = "sh"
python = "python3"
ruby = "ruby"
node = "node"
javascript = "node"
js = "node"
php = "php"
perl = "perl"

[runtimes.go]
command = "go run"
execution_mode = "file"

[runtimes.jq]
command = "jq"
execution_mode = "arg"
```

## Execution modes

| Mode | Behaviour |
|---|---|
| `file` (default) | Write the code to a temp file and pass it as an argument. stdin stays interactive. |
| `stdin` | Pipe the code into the command's stdin. The script itself cannot read the terminal. Use for `psql`, `redis-cli`, … |
| `arg` | Pass the code as a command-line argument (e.g. `jq`, `mq`). |

## `default_task`

Top-level key (outside `[runtimes]`). The task run when `mq-task` is invoked without a task name.

## Overriding from the CLI

```sh
mq-task build -r python:python3.11 -e file
mq-task build --lang bash      # only run bash blocks
```
