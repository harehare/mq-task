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
- The prose under the heading is the task description (shown in `list` and the TUI).
- The language of the fence selects the runtime from [configuration](../start/configuration.md).
- A `meta` block configures the task; it is not executed.

## Document-wide meta

A `meta` block **before the first heading** applies to every task:

````markdown
```meta
env = ["REGION=staging"]
dir = "services/api"
```
````

Precedence, lowest to highest: document-wide `meta` → the task's own `meta` → CLI flags.
