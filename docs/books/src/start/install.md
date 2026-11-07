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

## Windows

In PowerShell (5.1 or 7):

```powershell
irm https://task.mqlang.org/install.ps1 | iex
```

It downloads `mq-task-x86_64-pc-windows-msvc.exe` (Arm64 Windows runs it emulated), verifies its SHA-256 checksum, installs it to `%LOCALAPPDATA%\mq-task\bin` and adds that folder to your user `PATH`. Open a new terminal afterwards.

A piped script cannot take parameters, so it reads environment variables:

| Variable | Meaning |
|---|---|
| `MQ_TASK_VERSION` | Release tag to install, e.g. `v0.2.4` (default: the latest) |
| `MQ_TASK_INSTALL_DIR` | Where to put `mq-task.exe` |
| `MQ_TASK_NO_MODIFY_PATH` | `1` leaves `PATH` alone |

```powershell
$env:MQ_TASK_VERSION = "v0.2.4"; irm https://task.mqlang.org/install.ps1 | iex
```

The `bash` runtime needs Git Bash or WSL on `PATH`; see [Configuration](configuration.md) for choosing another runtime such as `pwsh`.

## Cargo

```sh
cargo install --git https://github.com/harehare/mq-task.git
```

## Verify

```sh
mq-task --version
```
