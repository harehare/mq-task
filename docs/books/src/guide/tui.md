# Interactive TUI

```sh
mq-task tui
```

A searchable task list with a details pane (description, dependencies, params, options, code).

| Key | Action |
|---|---|
| `↑`/`↓`, `j`/`k` | move |
| `/` | search titles, aliases, descriptions |
| `Enter` | run inside the TUI (prompts when a param is required) |
| `a` | run with arguments (`name=value`, positional; quote values containing spaces) |
| `x` | leave the TUI and run in the terminal (interactive stdin works) |
| `q`/`Esc` | quit |

While running, the top pane draws the dependency graph as a left-to-right workflow: one stage per column, each task with its state (`○` pending, spinner running, `✔`, `✘`, `⊘` skipped, `↻` retry) and elapsed time. The bottom pane shows the log. Arrow keys (or `hjkl`) move the selection, `v` toggles the graph and a flat task list, `Tab` toggles combined / selected-task log, `PgUp`/`PgDn` scroll, `q`/`Ctrl+C` aborts, `r` re-runs. `--parallel`, `-j`, `-k` and `--no-cache` apply too.
