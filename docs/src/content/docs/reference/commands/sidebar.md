---
title: "sidebar"
description: Control the agent status sidebar in Rex
---

Controls the live status sidebar in the selected Rex server. Each sidebar is a
block on the left of a window, or a full-width bar above its content.

```bash
workrex sidebar [--session] [on|off]
workrex sidebar next
workrex sidebar prev
workrex sidebar jump <index>
workrex sidebar filter <all|session>
workrex sidebar group <project|session>
workrex sidebar group --clear
workrex sidebar view [on|off]
```

Without a subcommand, `sidebar` toggles visibility. `on` and `off` are idempotent.
`--session` limits visibility changes to the caller's Rex session.

## Size

```bash
workrex sidebar on --position left --width 30
workrex sidebar on --position top --height 3
```

Set `sidebar.width` for left placement and `sidebar.height` for top placement. Both accept an absolute size or a percentage. Without a configured size, a dragged border records the width or height for that Rex server. The two positions keep their saved sizes separately. Command-line sizes remain in effect until the sidebar is turned off.

## Navigation

`next`, `prev`, and `jump` navigate in sidebar display order. Jumps ask Rex.app to
show the target's session and tab, then focus the agent's block. Cross-tab jumps
need Remote Control in Rex Settings and fail visibly if the app refuses them.

## Filtering and grouping

`filter all` shows agents across the server; `filter session` selects the
caller's session. `group project` and `group session` divide rows by repository
or Rex session. `group --clear` removes the override. These display preferences
are shared by sidebars on the same server.

## Named views

`sidebar.views` defines named session or project scopes. Exact session labels
are case-sensitive. A matching view controls the agents listed for a window;
otherwise the normal filter applies. `view on` enables configured views and
`view off` disables them. Press `V` in the sidebar to toggle the preference.

See [Sidebar](/guide/sidebar/) and
[Customization](/guide/sidebar/customization/) for layout and template options.

## Runtime behavior

The daemon consumes Rex events and observes agents, then sends snapshots to
sidebar blocks. It creates a sidebar for each new eligible window without
changing focus. Per-server state lives under
`~/.local/state/workrex/sidebar/`. Closing a sidebar manually keeps that window
closed until the next explicit `sidebar on`.

Workrex does not infer session identity from a repository basename. Keep
`REX_SERVER`, `REX_SESSION`, and `REX_BLOCK` attached to commands launched by Rex.
