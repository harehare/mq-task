# Dependency Graph

```sh
mq-task graph ci                  # tree for one task
mq-task graph --format mermaid    # Mermaid diagram of all public tasks
```

```text
ci [parallel]
├── lint
└── test
    └── build
```

The Mermaid output can be pasted into GitHub Markdown or any Mermaid renderer.
