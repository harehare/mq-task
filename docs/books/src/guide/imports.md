# Importing Tasks

List files in the document-wide `meta` block. Their tasks become available as `<file name>:<task>`.

````markdown
```meta
imports = ["ci/checks.md", "deploy.md"]
```

## release

```meta
depends = ["checks:lint", "deploy:staging"]
```
````

## Options

An entry can be a table instead of a path:

````markdown
```meta
imports = [
  "ci/checks.md",
  { path = "deploy.md", as = "ship" },
  { path = "local.md", optional = true },
  { path = "common.md", flatten = true },
  "tasks/*.md",
]
```
````

| Key | Meaning |
|---|---|
| `path` | The file, or a glob such as `tasks/*.md` (matches are imported in sorted order, each named after its file; a glob without matches imports nothing, and the importing file is never matched) |
| `as` | The namespace to use instead of the file name: `ship:staging` for `deploy.md`. Not with a glob or `flatten` |
| `optional` | A missing file is ignored instead of being an error. A file that exists but is broken is still an error |
| `flatten` | Add the tasks under their own names, without `<name>:`. When a name is already taken by a task of the importing file, `check` reports the clash and the importing file's task wins |

Paths and the other rules:

- Paths are relative to the importing file; imports nest (`a:b:task`); cycles are rejected.
- Inside an imported file, `depends` use that file's own short names.
- The imported file's document-wide `env`/`dir` apply to its tasks. Relative `dir` resolves against where `mq-task` was invoked.
