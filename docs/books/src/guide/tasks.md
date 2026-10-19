# Writing Tasks

Every heading is a task. The task name is the heading text; the commands are the code blocks beneath it. By default `##` headings are tasks.

````markdown
## build

Compile the project.

```bash
cargo build
```
````

- Several code blocks in one section run in order.
- The prose under the heading is the task description (shown in `list` and the TUI). `mq-task <task> --summary` prints it together with the task's dependencies, parameters and options, without running anything.
- The language of the fence selects the runtime from [configuration](../start/configuration.md).
- A `meta` block configures the task; it is not executed.

## Code blocks that are not commands

When the task file is a `README.md`, sections often hold examples next to the commands. Blocks that are not commands are skipped:

- A block whose info string contains `no-run` is never run: ```` ```bash no-run ````.
- A block in a data or output language (`text`, `console`, `output`, `log`, `json`, `yaml`, `toml`, `xml`, `html`, `css`, `csv`, `diff`, `ini`, `markdown`, `mermaid`, `dockerfile`, ...) is skipped, unless the language has a runtime under `[runtimes]` (then it runs, e.g. a `json` block for a `jq` runtime). Other languages without a runtime are still an error, so a typo such as `pyhton` is caught.
- A block without a language is skipped.

A section whose blocks are all skipped is not a task: it is not listed and `check` ignores it.

## Document-wide meta

A `meta` block **before the first heading** applies to every task:

````markdown
```meta
env = ["REGION=staging"]
dir = "services/api"
```
````

Precedence, lowest to highest: document-wide `meta` → the task's own `meta` → CLI flags.

## Seeing what runs

`-v`/`--verbose` prints every code block just before it runs, on stderr so that captured output and pipes are not affected:

```text
$ mq-task build -v
▶ build
▸ bash
  cargo build --release
```

The block is shown for each loop item and each deferred task, and with the working directory when the task has one (`▸ bash in services/api`). With `--output group` it is part of the task's block. `--dry-run` already shows the code and its context, so `-v` adds nothing there.

## Typos

A task name that does not exist is reported with the closest match, and so is a mistyped entry in `depends`:

```text
$ mq-task deploi
Error: Section not found: deploi (did you mean `deploy`?)
```
