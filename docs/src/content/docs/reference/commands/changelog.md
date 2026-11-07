---
title: "changelog"
description: Show what changed in each version
---

Prints the changelog bundled with this build. In a terminal, `changelog` renders it and opens it in `$PAGER` (default `less -R`). Otherwise it prints the raw Markdown.

```bash
workrex changelog
```

The bundled `CHANGELOG.md` lists Workrex releases and unreleased changes.
Original project history remains in Git. To install a newer published version,
run [`workrex update`](/reference/commands/update/).
