# Conditions

`if` skips a task when its condition is false. A skipped task does **not** block its dependents.

````markdown
## deploy

```meta
if = 'env.CI && os == "linux"'
```

```bash
./deploy.sh
```
````

| Expression | True when |
|---|---|
| `env.NAME` | set and not empty, `0` or `false` |
| `env.NAME == "x"`, `env.NAME != "x"` | equals / differs from `x` |
| `os == "linux"` | OS is `linux`, `macos` or `windows` |
| `exists("path")` | path exists (relative to the task's working directory) |
| `which("cmd")` | `cmd` is on `PATH` |
| `!expr`, `a && b`, `a \|\| b` | boolean combinations (no parentheses) |

Variables resolve from `--env`, the task's `meta`/`params`, then the process environment.
