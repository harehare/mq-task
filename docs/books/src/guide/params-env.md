# Parameters, Env and Working Directory

## Named parameters

A bare name is required; `name=value` sets a default.

````markdown
## deploy

```meta
params = ["env=staging", "region"]
```

```bash
echo "deploying to $MX_PARAM_ENV / $MX_PARAM_REGION"
```
````

```sh
mq-task deploy -- region=eu           # env falls back to "staging"
mq-task deploy -- prod eu             # positional, in declaration order
mq-task deploy -- env=prod region=eu  # named
```

Parameters are exposed as `MX_PARAM_<NAME>`. A required parameter without a value is an error.

## Environment and directory

```sh
mq-task deploy --env REGION=eu --env DEBUG=1
mq-task deploy --dir ../other-project
```

Defaults can be declared in `meta`:

````markdown
## deploy

```meta
env = ["REGION=staging", "LOG_LEVEL=info"]
dir = "services/api"
```
````

CLI `--env` / `--dir` override the `meta` defaults for that run.

## `.env` files

`dotenv` loads variables from `.env`-style files. It works per task and, in a `meta` block before the first heading, for every task:

````markdown
```meta
dotenv = [".env"]
```

## deploy

```meta
dotenv = [".env.production"]
```

```bash
echo "deploying with $API_URL"
```
````

- Paths are relative to the task's working directory (`dir`). A file that does not exist is ignored, so optional files such as `.env.local` are safe to list.
- Files are read in the order listed and later files win.
- Precedence, lowest first: `dotenv` files → document-wide `env` → the task's `env` → params → `--env`. A variable that is already set in the process environment is **not** overridden by a file, so `API_URL=... mq-task deploy` still wins over `.env`.
- Supported syntax: `KEY=value`, `export KEY=value`, `#` comments, single quotes (literal) and double quotes (with `\n`, `\t`, `\"`, `\\` escapes). There is no `$VAR` expansion. A malformed line is an error that names the file and line.
