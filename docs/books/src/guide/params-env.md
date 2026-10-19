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

## Choices and descriptions

An entry of `params` can also be a table, to restrict the values and explain the parameter:

````markdown
## deploy

```meta
params = [
  { name = "env", default = "staging", choices = ["staging", "prod"], desc = "target environment" },
  "tag=latest",
]
```
````

| Key | Meaning |
|---|---|
| `name` | The parameter name (required) |
| `default` | The value used when none is given; without it the parameter is required |
| `choices` | The accepted values; anything else is an error that names the choices and suggests the closest one |
| `desc` | A line of help shown by `--summary`, the TUI and `list --json` (`description`) |
| | Tables and the short `"name=default"` strings can be mixed in one list |

```text
$ mq-task deploy --summary
  params   env=staging [staging|prod] - target environment; tag=latest
$ mq-task deploy -- prd
Error: Invalid value 'prd' for parameter 'env' of task 'deploy' (one of: staging, prod; did you mean `prod`?)
```

The default must be one of the choices, and an empty `choices` list is rejected when the file is loaded. Values given through `depends = ["deploy env=prod"]` are checked too.

A word of the form `name=value` is a named argument, so a name the task does not declare is an error (with a suggestion for a near miss) instead of becoming a value:

```text
$ mq-task deploy -- regoin=eu
Error: Unknown parameter 'regoin' for task 'deploy' (did you mean `region`?)
```

This applies to tasks that declare `params`; a task without them takes any arguments. A value that merely contains `=` (`https://x.test/?a=1`) is still a plain value, because it is not of the form `name=value`.

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

## Required variables

`requires` fails a task early, before anything runs, when variables it needs are missing or empty:

````markdown
## deploy

```meta
requires = ["API_TOKEN", "REGION"]
```

```bash
./deploy.sh
```
````

```text
Error: Task 'deploy' requires API_TOKEN, REGION to be set
```

Variables are looked up in `--env`, the task's `meta`, params and `dotenv` files, and the process environment. All missing names are reported at once. Tasks skipped by `if` are not checked, and `--dry-run` does not check at all.

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
- With `--watch`, changes to the listed `.env` files re-run the task.
- Supported syntax: `KEY=value`, `export KEY=value`, `#` comments, single quotes (literal) and double quotes (with `\n`, `\t`, `\"`, `\\` escapes). There is no `$VAR` expansion. A malformed line is an error that names the file and line.
