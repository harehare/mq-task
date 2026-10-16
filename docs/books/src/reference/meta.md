# Meta keys

A fenced block with the language `meta` (TOML) in a task section. Unknown or invalid values are an error.

| Key | Type | Description |
|---|---|---|
| `depends` | array of strings | Tasks to run first; each may carry arguments (`"deploy env=prod"`) |
| `once` | bool | Run at most once per run, even when called with different arguments |
| `parallel` | bool | Run `depends` (and loop iterations) concurrently |
| `for` | array, `{ glob }` or `{ matrix }` | Run the task once per item ([Loops](../guide/loops.md)) |
| `as` | string | Extra variable name for the loop item (`as = "file"` → `MX_FILE`) |
| `defer` | array of strings | Tasks run after this one, even on failure ([defer](../guide/defer.md)) |
| `if` | string | Condition; task skipped when false |
| `retries` | integer | Extra attempts after a failure |
| `retry_delay` | duration | Wait between attempts (default `1s`) |
| `timeout` | duration | Per-attempt limit; exit `124` |
| `prompt` | string | Ask for confirmation before running ([Prompts](../guide/prompts.md)) |
| `ignore_error` | bool | Treat a failed run as success ([Retries and Timeouts](../guide/retries-timeouts.md#ignoring-failures)) |
| `capture` | bool | Expose stdout as `MX_OUTPUT_<TASK>` to dependents |
| `sources` | array of globs | Input files for up-to-date checks and `--watch` |
| `status` | array of strings | Shell commands; task skipped when all exit `0` ([guide](../guide/cache-watch.md#custom-up-to-date-checks-with-status)) |
| `outputs` | array of paths | Files that must exist for the task to be considered up to date |
| `params` | array of strings | Named parameters (`name` required, `name=value` default) |
| `env` | array of `KEY=VALUE` | Default environment variables |
| `dir` | string | Working directory |
| `dotenv` | array of paths | `.env` files to load ([guide](../guide/params-env.md#env-files)) |
| `alias` | array of strings | Alternate names |
| `private` | bool | Hide from listings |
| `imports` | array of paths | Document-wide only: files to import |

Environment variables set for tasks: `MX_ARGS`, `MX_ARG_<N>`, `MX_PARAM_<NAME>`, `MX_OUTPUT_<TASK>`, and in loops `MX_ITEM`, `MX_INDEX`, `MX_<AXIS>`.
