# Parallel Execution

By default dependencies run one after another in declaration order. Set `parallel = true` to run them concurrently; the task starts after **all** of them finish (fan-out / fan-in).

````markdown
## ci

```meta
depends = ["lint", "test", "build"]
parallel = true
```

```bash
echo "all checks passed"
```
````

- Only real dependency edges constrain ordering: a task starts as soon as *its own* dependencies are done.
- Output is prefixed with the task name (for example `lint │ output`; see [Output modes](#output-modes)); each task reports `✔`/`✘` with its duration.
- The first failure aborts the run: running tasks (and the processes they started) are stopped. `-k/--keep-going` lets independent tasks finish; dependents of a failed task are not run.
- `--parallel` forces concurrency regardless of the file; `-j N/--jobs N` caps concurrent tasks (`-j 1` is sequential). Default: number of CPUs.
- stdin is closed for tasks in parallel runs; keep interactive tasks sequential. Dry-run is always sequential.

## Output modes

`--output` chooses how task output is shown:

| Mode | Behavior |
|---|---|
| (default) | raw output for sequential runs, prefixed lines (`lint │ ...`) for concurrent ones |
| `interleaved` | raw lines as they arrive, never prefixed |
| `prefixed` | every line tagged with its task name, even when sequential |
| `group` | each task's output is held back and printed as one block when the task finishes, which keeps CI logs readable (each task holds at most 64 MiB; past that the held part is printed and the rest streams with a `task │` prefix) |

```sh
mq-task ci --output group
```

`prefixed` and `group` pipe task output, so stdin is closed for tasks (as in parallel runs) and programs may stop colouring their output. The TUI draws its own log pane and ignores `--output`.
