# Caching and Watch Mode

Declare the files a task reads with `sources` (globs). The task is skipped when sources, code, arguments and environment are unchanged since its last successful run. With `outputs`, it also runs if an output is missing.

````markdown
## build

```meta
sources = ["src/**/*.rs", "Cargo.toml"]
outputs = ["target/release/app"]
```

```bash
cargo build --release
```
````

Fingerprints live in `.mq-task/cache.json` in the invocation directory. Consider git-ignoring `.mq-task/`. `--no-cache` ignores them.

## Custom up-to-date checks with `status`

When "unchanged inputs" is not the right question, `status` lists shell commands that decide it. If **all** of them exit `0` the task is up to date and skipped:

````markdown
## install

```meta
status = ["test -d node_modules", "test node_modules -nt package-lock.json"]
```

```bash
npm ci
```
````

- Commands run with `sh -c` (`cmd /C` on Windows) in the task's working directory and environment, with output discarded.
- `status` is checked before `sources`/`outputs`; if it does not pass, the normal fingerprint check still applies.
- `--no-cache` ignores `status` too, and `--dry-run` never runs it.

## Watch

```sh
mq-task test --watch
```

Re-runs whenever any `sources` of the task or its dependencies, or the task file, change.
