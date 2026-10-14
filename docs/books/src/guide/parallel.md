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
- Output is prefixed with the task name (for example `lint │ output`); each task reports `✔`/`✘` with its duration.
- The first failure aborts the run: running tasks (and the processes they started) are stopped. `-k/--keep-going` lets independent tasks finish; dependents of a failed task are not run.
- `--parallel` forces concurrency regardless of the file; `-j N/--jobs N` caps concurrent tasks (`-j 1` is sequential). Default: number of CPUs.
- stdin is closed for tasks in parallel runs; keep interactive tasks sequential. Dry-run is always sequential.
