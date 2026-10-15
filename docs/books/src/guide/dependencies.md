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

## Calling a dependency with arguments

A dependency can be followed by arguments, bound exactly like command-line arguments: `name=value` for [named parameters](params-env.md), the rest positionally.

````markdown
## deploy

```meta
params = ["env", "region=us"]
```

```bash
echo "deploying to $MX_PARAM_ENV ($MX_PARAM_REGION)"
```

## release

```meta
depends = ["deploy staging", "deploy env=prod 'region=eu west'"]
```
````

- Words are split on whitespace; use single or double quotes for values containing spaces. A task title containing spaces still works as a plain dependency (`depends = ["Build App"]`).
- The same task called with different arguments runs once per distinct argument list; identical calls are shared.
- Calls appear as `deploy[env=prod ...]` in progress output and as `deploy (env=prod ...)` in `graph`.
- Arguments apply to that call only; the arguments given on the command line still belong to the requested task.
