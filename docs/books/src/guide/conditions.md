# Conditions

`if` skips a task when its condition is false. A skipped task does **not** block its dependents.

````markdown
## deploy

```meta
if = 'flag("CI") && os() == "linux"'
```

```bash
./deploy.sh
```
````

| Expression | True when |
|---|---|
| `flag("NAME")` | the variable is set and not empty, `0` or `false` |
| `getenv("NAME") == "x"`, `getenv("NAME") != "x"` | the variable equals / differs from `x` |
| `os() == "linux"` | the OS is `linux`, `macos` or `windows` |
| `exists("path")` | the path exists (relative to the task's working directory) |
| `which("cmd")` | `cmd` is on `PATH` |
| `!expr`, `a && b`, `a \|\| b` | boolean combinations; parentheses group |

Variables are looked up in `--env`, the task's `meta`/`params`, then the process environment.

A condition is an [mq](https://mqlang.org) expression that must evaluate to `true` or `false`, so anything else mq offers (string functions, `contains` and so on) works too. Use `getenv`/`flag` rather than mq's built-in `env()`, which only sees the process environment.
