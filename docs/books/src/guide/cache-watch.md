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

## Excluding files

A pattern starting with `!` removes the files it matches from `sources`, wherever it stands in the list:

```toml
sources = ["src/**/*.rs", "!src/generated/**"]
```

Like any pattern it is relative to the task's directory, and a single `*` does not cross directories (`!*.rs` excludes only top-level files). Exclusions also apply to `--watch`.

## Checking without running

`--status` reports whether each task would run, without running it, and exits with `1` when any would. Use it in scripts and hooks:

```text
$ mq-task build --status
✘ build out of date (sources changed, outputs are missing or it has not run yet)
$ mq-task build && mq-task build --status
✔ build (up to date)
```

A task is up to date when its `status` commands pass or its `sources` and `outputs` are unchanged (or its `if` is false). A task with neither always runs. Tasks that depend on one that is out of date are reported as not checked. `-q` prints only the out-of-date ones.

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
mq-task test --watch --interval 1s   # check for changes every second (default 300ms)
```

Re-runs whenever any `sources` or `dotenv` file of the task or its dependencies, or the task file, change.
