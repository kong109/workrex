---
title: "Nix"
description: Installing and configuring workrex with Nix and Home Manager
---

Requires [Nix with flakes enabled](https://nixos.wiki/wiki/Flakes).

The workrex repo has a flake that builds the `workrex` binary from source. CI builds it on x86_64 Linux when a change touches the flake, the Cargo files, `src/` or `tests/` (the Nix workflow runs `nix build .#`). The flake also lists aarch64 Linux and both macOS architectures, but CI does not build those. If the flake does not build on your system, install a [release build](/guide/installation/) instead.

## Quick start

Run workrex without installing it:

```bash
nix run github:kong109/workrex -- --help
```

## Flake input

```nix
inputs.workrex.url = "github:kong109/workrex";
```

## Installation

### Home Manager

Install the package and write the config file. The config path keeps the workrex name, so an existing workrex config works. See [Configuration](/guide/configuration/).

```nix
{ inputs, pkgs, ... }:

{
  home.packages = [ inputs.workrex.packages.${pkgs.system}.default ];

  xdg.configFile."workrex/config.yaml".text = ''
    merge_strategy: rebase
    agent: claude
    panes:
      - command: <agent>
        focus: true
      - split: horizontal
  '';
}
```

See [Configuration](/guide/configuration/) for all config options.

### NixOS

```nix
{ inputs, pkgs, ... }:

{
  environment.systemPackages = [
    inputs.workrex.packages.${pkgs.system}.default
  ];
}
```

The package's Nix name is `workrex`, after the Cargo package. The binary it installs is `workrex`.

## Shell completions

The flake installs completions for Bash, Zsh and Fish. Do not add the manual `eval "$(workrex completions ...)"` lines from the [Installation](/guide/installation/#shell-completions) guide.
