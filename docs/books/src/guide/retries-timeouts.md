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
