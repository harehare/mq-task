# Retries and Timeouts

````markdown
## flaky-integration

```meta
retries = 3          # up to 4 attempts in total
retry_delay = "2s"   # default: 1s
timeout = "5m"       # per attempt; exit code 124 when exceeded
```

```bash
./run-integration-tests.sh
```
````

- Durations accept `ms`, `s`, `m`, `h`; a bare number is seconds.
- Only runtime failures are retried. Missing runtimes or parameters fail immediately.
- A task with `timeout` runs in its own process group so the whole tree is stopped. It therefore cannot read from the terminal.

## Ignoring failures

`ignore_error = true` lets a task fail without failing the run:

````markdown
## lint-docs

```meta
ignore_error = true
```

```bash
markdownlint docs/
```
````

- The failure is reported on stderr, but the task counts as successful: dependents still run and the exit code stays `0`.
- It applies after `retries` are exhausted, and also to a `timeout`.
- Only runtime failures are ignored. Missing runtimes, missing parameters and cancellation still stop the run.
- An ignored failure is never recorded as "up to date" for `sources`/`outputs`, so the task runs again next time.
- `defer` tasks still run.
