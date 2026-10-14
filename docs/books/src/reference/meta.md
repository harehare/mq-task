# Meta keys

A fenced block with the language `meta` (TOML) in a task section. Unknown or invalid values are an error.

| Key | Type | Description |
|---|---|---|
| `depends` | array of strings | Tasks to run first |
| `parallel` | bool | Run `depends` concurrently |
| `if` | string | Condition; task skipped when false |
| `retries` | integer | Extra attempts after a failure |
| `retry_delay` | duration | Wait between attempts (default `1s`) |
| `timeout` | duration | Per-attempt limit; exit `124` |
| `capture` | bool | Expose stdout as `MX_OUTPUT_<TASK>` to dependents |
| `sources` | array of globs | Input files for up-to-date checks and `--watch` |
| `outputs` | array of paths | Files that must exist for the task to be considered up to date |
| `params` | array of strings | Named parameters (`name` required, `name=value` default) |
| `env` | array of `KEY=VALUE` | Default environment variables |
| `dir` | string | Working directory |
| `alias` | array of strings | Alternate names |
| `private` | bool | Hide from listings |
| `imports` | array of paths | Document-wide only: files to import |

Environment variables set for tasks: `MX_ARGS`, `MX_ARG_<N>`, `MX_PARAM_<NAME>`, `MX_OUTPUT_<TASK>`.
