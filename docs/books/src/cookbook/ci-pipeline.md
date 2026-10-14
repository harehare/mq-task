# A CI pipeline in one file

````markdown
## format

```bash
cargo fmt --check
```

## lint

```meta
sources = ["src/**/*.rs"]
```

```bash
cargo clippy -- -D warnings
```

## test

```meta
sources = ["src/**/*.rs", "tests/**/*.rs"]
retries = 1
```

```bash
cargo test
```

## ci

```meta
depends = ["format", "lint", "test"]
parallel = true
```

```bash
echo ok
```
````

Run it with `mq-task ci -k` to see every failure at once, or `mq-task ci -j 2` to limit concurrency.
