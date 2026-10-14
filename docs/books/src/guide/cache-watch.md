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

## Watch

```sh
mq-task test --watch
```

Re-runs whenever any `sources` of the task or its dependencies, or the task file, change.
