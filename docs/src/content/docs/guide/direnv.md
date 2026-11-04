---
title: "direnv integration"
description: Automatically set up direnv in new worktrees
---

workrex keeps the workmux names for config files (`.workmux.yaml`, `~/.config/workmux/config.yaml`) and `WORKMUX_*` environment variables, so an existing workmux config works. See [Configuration](/guide/configuration/).

If your project uses [direnv](https://direnv.net/) for environment management, you can configure workrex to automatically set it up in new worktrees:

```yaml
# .workmux.yaml
post_create:
  - direnv allow

files:
  symlink:
    - .envrc
```

See also [Using direnv for port isolation](/guide/monorepos/#using-direnv) in monorepos.
