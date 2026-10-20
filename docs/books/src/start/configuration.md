# Configuration

`mq-task init --config` creates an `mq-task.toml` with default runtime settings. It is picked up automatically when it sits next to the task file. Use `-c/--config` (or `MQ_TASK_CONFIG`) to point at another file. Unknown keys are an error.

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
| `stdin` | Pipe the code into the command's stdin. The script itself cannot read the terminal. Use for `psql`, `redis-cli` and similar clients |
| `arg` | Pass the code as a command-line argument (e.g. `jq`, `mq`). |

## `default_task`

Top-level key (outside `[runtimes]`). The task run when `mq-task` is invoked without a task name.

## In the task file

`runtimes` and `default_task` can also be written in the document-wide `meta` block (before the first heading), so that a single Markdown file carries everything it needs and no `mq-task.toml` has to sit next to it:

````markdown
```meta
default_task = "build"
runtimes = { python = "python3.12", go = { command = "go run", execution_mode = "file" } }
```
````

`runtimes` takes the same values as `[runtimes]` above. The file adds to the built-in runtimes and replaces those it names. Precedence, highest first:

1. `-r/--runtime` on the command line
2. the languages set in `mq-task.toml` (and its `default_task`)
3. the task file's own `meta` block
4. the built-in defaults

So a machine can still pin its own interpreter with `mq-task.toml`, and a file in the task file is only a default for everyone else. Imported files can add runtimes for languages the importing file leaves alone. `mq-task check` knows about them, and reports a `default_task` that names no task.

## Overriding from the CLI

```sh
mq-task build -r python:python3.11 -e file
mq-task build --lang bash      # only run bash blocks
```
