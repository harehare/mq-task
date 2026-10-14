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
