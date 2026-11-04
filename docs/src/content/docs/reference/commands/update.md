---
title: "update"
description: Update workrex to the latest release from kong109/workrex
---

Replaces the running `workrex` binary with the latest release from
[kong109/workrex](https://github.com/kong109/workrex/releases).

```bash
workrex update
```

## What happens

1. workrex asks the GitHub API for the latest release of `kong109/workrex`.
2. If that release is the installed version, workrex prints
   `Already up to date` and exits.
3. workrex downloads `workrex-<platform>.tar.gz` and its `.sha256` file, and
   checks the SHA-256 checksum.
4. workrex copies the new binary next to the old one, then renames it into
   place. If the rename fails, workrex restores the old binary.

## Platforms

Releases include binaries for macOS on Apple Silicon (`darwin-arm64`) and Linux
on x86_64 (`linux-amd64`). On any other platform, `workrex update` stops and
tells you to build from source:

```bash
cargo install --git https://github.com/kong109/workrex --bin workrex
```

workrex has no Homebrew formula. `workrex update` refuses to run when the
binary's path contains `/Cellar/` or `/homebrew/`, so keep the binary out of a
Homebrew prefix such as `/opt/homebrew/bin`.

## Update notices

Once every 24 hours at most, workrex checks for a new release in a background
process. When one exists, it prints a notice to standard error:

```
Update available: workrex v0.1.278-rex.1 -> v0.1.279 (run `workrex update`)
```

workrex prints the notice only when standard output and standard error are
both terminals. After three notices, it waits seven days before it shows the
notice again.

To turn the check off, do one of these:

- Set `auto_update_check: false` in `~/.config/workmux/config.yaml`.
- Set the environment variable `WORKMUX_NO_UPDATE_CHECK=1`.

The configuration path and the variable keep their workmux names, so existing
workmux [configuration](/guide/configuration/) works with workrex.

## Requirements

- `curl` and `tar` on `PATH`.
- Write permission on the directory that holds the `workrex` binary.
