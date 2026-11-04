---
title: "changelog"
description: Show what changed in each version
---

Prints the changelog bundled with this build. In a terminal, `changelog` renders it and opens it in `$PAGER` (default `less -R`). Otherwise it prints the raw Markdown.

```bash
workrex changelog
```

The bundled changelog is upstream workmux's `CHANGELOG.md`. It lists upstream releases up to the one this build is based on, v0.1.278 for 0.1.278-rex.1, and does not list workrex's own changes. To install a newer version, run [`workrex update`](/reference/commands/update/).
