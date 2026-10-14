# Release with captured version

````markdown
## version

```meta
capture = true
```

```bash
git describe --tags --abbrev=0
```

## build

```meta
sources = ["src/**/*.rs"]
outputs = ["target/release/app"]
```

```bash
cargo build --release
```

## release

```meta
depends = ["version", "build"]
if = 'env.GITHUB_ACTIONS'
```

```bash
gh release upload "$MX_OUTPUT_VERSION" target/release/app
```
````
