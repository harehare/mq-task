# Multi-language tasks

Each fenced block uses the runtime for its language.

````markdown
## report

```python
import json, sys
print(json.dumps({"ok": True}))
```

```bash
echo "done"
```
````

Add or change runtimes in [`mq-task.toml`](../start/configuration.md), or override one per run with `-r python:python3.12`.
