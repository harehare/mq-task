# Loops

`for` runs a task once per item. The task body is repeated for every item, and each iteration sees the item as environment variables.

````markdown
## convert

```meta
for = ["a.png", "b.png", "c.png"]
```

```bash
convert "$MX_ITEM" "${MX_ITEM%.png}.webp"
```
````

## Sources of items

| Form | Items |
|---|---|
| `for = ["a", "b", 3]` | the listed values (strings, numbers and booleans) |
| `for = { glob = "assets/*.png" }` | files and directories matching the glob(s), sorted, relative to the task's directory |
| `for = { matrix = { os = ["linux", "macos"], arch = ["x64", "arm64"] } }` | every combination of the axes |

`glob` also accepts an array of patterns. Matrix axes are iterated in alphabetical order of their names, the last axis varying fastest.

## Variables

| Variable | Value |
|---|---|
| `MX_ITEM` | the current item (list and glob) |
| `MX_INDEX` | the zero-based position of the iteration |
| `MX_<AXIS>` | the value of each matrix axis, e.g. `MX_OS`, `MX_ARCH` |
| `MX_<AS>` | the item under the name given by `as` (list and glob) |

````markdown
## greet

```meta
for = ["alice", "bob"]
as = "name"
```

```bash
echo "hello $MX_NAME"   # same value as $MX_ITEM
```
````

## Execution

- Iterations run one after another by default. With `parallel = true` (or `--parallel`/`-j N`) they run concurrently, subject to `-j`.
- Each iteration is a separate run of the task: `if`, `retries`, `timeout`, and `sources`/`outputs` apply per iteration. The up-to-date check is recorded per item.
- The first failing iteration aborts the run. With `--keep-going` the remaining iterations still run.
- Tasks that depend on a looped task start after **all** its iterations finished, and a failed iteration fails the dependency.
- With `capture = true`, dependents see the outputs of all iterations joined by newlines in `MX_OUTPUT_<TASK>`.
- A loop without items (an empty list, a glob without matches) is reported as skipped rather than as an error.
- Iterations show up as `task[item]` in progress output, the TUI and `graph`.

## Combining with call arguments

A loop pairs well with [dependency arguments](dependencies.md#calling-a-dependency-with-arguments):

````markdown
## build

```meta
params = ["profile=dev"]
for = { matrix = { target = ["x86_64", "aarch64"] } }
```

```bash
cargo build --profile "$MX_PARAM_PROFILE" --target "$MX_TARGET-unknown-linux-gnu"
```

## release

```meta
depends = ["build profile=release"]
```
````
