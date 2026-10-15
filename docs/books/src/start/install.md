# Install

## Quick install

```sh
curl -sSL https://task.mqlang.org/install.sh | bash
```

The installer will:

- Download the latest `mq-task` binary for your platform (Linux, macOS or Windows; x86_64 or aarch64) from GitHub Releases
- Verify its SHA-256 checksum
- Install it to `~/.local/bin/`
- Add `~/.local/bin` to your `PATH` in your shell profile (bash, zsh or fish)

Restart your terminal or `source` your shell profile afterwards.

## Cargo

```sh
cargo install --git https://github.com/harehare/mq-task.git
```

## Verify

```sh
mq-task --version
```
