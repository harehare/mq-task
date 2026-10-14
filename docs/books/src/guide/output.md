# Passing Output Between Tasks

`capture = true` exposes a task's stdout (trailing newline removed) to dependents as `MX_OUTPUT_<TASK>` (the title upper-cased with other characters replaced by `_`). The output is still printed.

````markdown
## version

```meta
capture = true
```

```bash
git describe --tags
```

## release

```meta
depends = ["version"]
```

```bash
echo "releasing $MX_OUTPUT_VERSION"
```
````

Captured output reaches direct and transitive dependents. Because stdout is piped, programs may stop colouring it.
