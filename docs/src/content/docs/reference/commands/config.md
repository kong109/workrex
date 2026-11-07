---
title: "config"
description: Edit, locate, or view the global workrex configuration file
---

Manage the global workrex configuration file (`~/.config/workrex/config.yaml`). Configuration and state live under Workrex-owned paths. See [Configuration](/guide/configuration/).

## config edit

Open the global configuration file in your preferred editor.

```bash
workrex config edit
```

Uses `$VISUAL`, then `$EDITOR`, then falls back to `vi`. If the configuration file does not exist yet, it is created with commented-out defaults before opening.

## config path

Print the path to the global configuration file. Useful for scripting.

```bash
workrex config path
# Output: /home/user/.config/workrex/config.yaml
```

## config reference

Print the default configuration file with all options documented. Useful for discovering available options or piping to an AI agent for context.

```bash
workrex config reference
```

## Examples

```bash
# Edit global config
workrex config edit

# Use a specific editor
EDITOR=nano workrex config edit

# Print the config path (for use in scripts)
cat "$(workrex config path)"

# Print the default config reference
workrex config reference
```

## See also

- [Configuration guide](/guide/configuration/) for all available options
- [`init`](/reference/commands/init/) to generate a project-level `.workrex.yaml`
