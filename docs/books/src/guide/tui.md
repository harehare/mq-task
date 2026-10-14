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

While running, the left pane shows per-task progress (`○` pending, spinner running, `✔`, `✘`, `⊘` skipped, `↻` retry) and the right pane the log. `Tab` toggles combined / selected-task log, `PgUp`/`PgDn` scroll, `q`/`Ctrl+C` aborts, `r` re-runs. `--parallel`, `-j`, `-k` and `--no-cache` apply too.
