---
title: "direnv integration"
description: Automatically set up direnv in new worktrees
---

Workrex uses `.workrex.yaml`, `~/.config/workrex/config.yaml`, and `WORKREX_*` environment variables. See [Configuration](/guide/configuration/).

If your project uses [direnv](https://direnv.net/) for environment management, you can configure workrex to automatically set it up in new worktrees:

```yaml
# .workrex.yaml
post_create:
  - direnv allow

files:
  symlink:
    - .envrc
```

See also [Using direnv for port isolation](/guide/monorepos/#using-direnv) in monorepos.
