# Cleanup with `defer`

`defer` lists tasks that run **after** a task's body finishes, whether it succeeded or failed — a place for cleanup that must not be skipped.

````markdown
## cleanup

```bash
docker compose down
```

## integration

```meta
defer = ["cleanup"]
```

```bash
docker compose up -d
./run-integration-tests.sh
```
````

- Deferred tasks run in the order they are listed, and each may be called with [arguments](dependencies.md#calling-a-dependency-with-arguments): `defer = ["notify status=done"]`.
- They run only if the task actually started: a task skipped by `if` or because it is up to date does not run its `defer`.
- They also run when the body failed, timed out, or the run was aborted by another failing task.
- With `retries`, deferred tasks run once, after the last attempt.
- The task's result is the body's result. If the body succeeded but a deferred task fails, the task fails with the deferred task's exit code. If the body failed, its error is kept and a failing deferred task is only reported.
- Deferred tasks see the same loop variables and dependency outputs as the task, and in a [loop](loops.md) they run after every iteration.
- A deferred task must not have `depends` of its own.
