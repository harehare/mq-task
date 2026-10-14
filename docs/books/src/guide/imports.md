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

- Paths are relative to the importing file; imports nest (`a:b:task`); cycles are rejected.
- Inside an imported file, `depends` use that file's own short names.
- The imported file's document-wide `env`/`dir` apply to its tasks. Relative `dir` resolves against where `mq-task` was invoked.
