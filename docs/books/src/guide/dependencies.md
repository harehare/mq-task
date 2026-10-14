# Dependencies

````markdown
## format

```bash
cargo fmt
```

## lint

```meta
depends = ["format"]
```

```bash
cargo clippy
```

## test

```meta
depends = ["lint"]
```

```bash
cargo test
```
````

`mq-task test` runs `format → lint → test`.

- Multiple dependencies: `depends = ["format", "lint"]`
- A shared dependency runs only once
- Circular dependencies are reported as an error
