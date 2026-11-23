//! `workrex remote`: manage SSH remotes that run agents.

use crate::bin_name::BIN;
use std::thread;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::config::Config;
use crate::multiplexer::{create_backend, detect_backend};
use crate::remote::RemoteSpec;
use crate::remote::install::{self, NodeStatus, Pin};
use crate::remote::transport::Transport;
use crate::remote::{Placement, client, credentials, doctor, observe, resolve_placement};
use crate::workflow::WorkflowContext;

#[derive(Debug, Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    pub command: Option<RemoteCommand>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    /// List where new agents can go: `local` and each configured remote,
    /// default first (no SSH)
    List {
        /// Print a JSON array
        #[arg(long)]
        json: bool,
    },
    /// List laptop operation logs, or print one request's output (no SSH)
    Logs {
        /// Remote name
        name: String,
        /// Request ID printed in the remote pane; omit to list log paths
        request: Option<String>,
    },
    /// Install this workrex's node binary and the agents' status hooks on a
    /// remote
    Setup {
        /// Remote name from `remotes:` in the global config
        name: String,
    },
    /// Check that a remote can run agents
    Doctor {
        /// Remote name from `remotes:` in the global config
        name: String,
    },
    /// Log an agent in on a remote, where its remote agents find the login
    Login {
        /// Remote name from `remotes:` in the global config
        name: String,
        /// claude, codex, gemini, opencode, pi or omp
        agent: String,
        /// Copy this machine's credential files for the agent instead
        #[arg(long, conflicts_with_all = ["setup_token", "args"])]
        import: bool,
        /// Claude: run `claude setup-token` and keep the token it prints
        /// for Claude agents on the remote
        #[arg(long)]
        setup_token: bool,
        /// Extra arguments for the agent's login command
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Bring a remote workspace home: stop its agent, make its work a local
    /// worktree with a window, and remove it from the remote
    Pull {
        /// Handle of the remote workspace
        handle: String,
        /// Skip post_create hooks for the local worktree
        #[arg(long)]
        no_hooks: bool,
    },
    /// List what a remote holds that no workspace uses, and remove it
    Gc {
        /// Remote name from `remotes:` in the global config
        name: String,
        /// Only list what would be removed
        #[arg(long)]
        dry_run: bool,
        /// Remove without asking, including workspaces whose work exists only
        /// on the remote
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
    },
    /// Show a recorded remote Rex block in Rex.app
    #[command(hide = true)]
    Attach {
        name: String,
        ex_id: String,
        #[arg(long)]
        pane: usize,
    },
    /// `GIT_SSH_COMMAND` for pushing a base commit to a remote
    #[command(hide = true, name = "git-ssh")]
    GitSsh {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

pub fn run(args: RemoteArgs) -> Result<()> {
    if let Some(RemoteCommand::Logs { name, request }) = &args.command {
        return crate::remote::operation::logs(name, request.as_deref());
    }
    let config = Config::load(None)?;
    match args.command {
        Some(RemoteCommand::Logs { .. }) => unreachable!(),
        None => list(&config),
        Some(RemoteCommand::List { json }) => list_targets(&config, json),
        Some(RemoteCommand::Setup { name }) => {
            let spec = RemoteSpec::resolve(&config, &name)?;
            install::setup(&spec)?;
            crate::remote::hooks::setup(&spec, &config)?;
            observe::current(&config, &name);
            Ok(())
        }
        Some(RemoteCommand::Doctor { name }) => {
            let spec = RemoteSpec::resolve(&config, &name)?;
            if !doctor::run(&spec, &config)? {
                bail!(
                    "remote '{name}' is not ready: fix the ✗ rows above, then run \
                     `{BIN} remote doctor {name}` again"
                );
            }
            observe::current(&config, &name);
            Ok(())
        }
        Some(RemoteCommand::Login {
            name,
            agent,
            import,
            setup_token,
            args,
        }) => {
            let spec = RemoteSpec::resolve(&config, &name)?;
            if import {
                credentials::import(&spec, &config, &agent)?;
            } else {
                let status = credentials::login(&spec, &config, &agent, setup_token, &args)?;
                if status != 0 {
                    std::process::exit(status);
                }
            }
            observe::current(&config, &name);
            Ok(())
        }
        Some(RemoteCommand::Pull { handle, no_hooks }) => pull(config, &handle, !no_hooks),
        Some(RemoteCommand::Gc {
            name,
            dry_run,
            force,
        }) => crate::remote::gc::run(&config, &name, dry_run, force),
        Some(RemoteCommand::Attach { name, ex_id, pane }) => {
            crate::remote::attach::run(&name, &ex_id, pane)
        }
        Some(RemoteCommand::GitSsh { name, args }) => {
            let status = client::git_ssh(&RemoteSpec::resolve(&config, &name)?, &args)?;
            std::process::exit(status)
        }
    }
}

fn pull(config: Config, handle: &str, run_hooks: bool) -> Result<()> {
    use crate::remote::cache::{self, Homecoming};
    // A pulled workspace whose remote cleanup is pending is found too, so
    // pulling it again finishes that cleanup.
    let pulled = || {
        let root = crate::git::get_main_worktree_root().ok()?;
        cache::for_project(&root).into_iter().find(|record| {
            record.handle == handle && matches!(record.homecoming, Some(Homecoming::Pulled { .. }))
        })
    };
    let Some(record) = cache::find_here(handle).or_else(pulled) else {
        bail!("No remote workspace '{handle}' in this repository");
    };
    let mux = create_backend(detect_backend());
    let context = WorkflowContext::new(config, mux, None)?;
    crate::remote::homecoming::pull(&context, record, run_hooks)
}

fn list(config: &Config) -> Result<()> {
    if config.remotes.is_empty() {
        println!(
            "No remotes configured. Add one under `remotes:` in your global config \
             (~/.config/workrex/config.yaml)."
        );
        return Ok(());
    }
    let default = match resolve_placement(config, None, false) {
        Ok(Placement::Remote(name)) => Some(name),
        _ => None,
    };
    let rows: Vec<(String, String, String)> = thread::scope(|scope| {
        let handles: Vec<_> = config
            .remotes
            .iter()
            .map(|(name, remote)| {
                scope.spawn(move || (name.clone(), remote.host.clone(), status(config, name)))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("remote status thread panicked"))
            .collect()
    });
    let name_width = rows
        .iter()
        .map(|row| row.0.len() + 10)
        .max()
        .unwrap_or(4)
        .max(4);
    let host_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(4).max(4);
    println!("{:<name_width$}  {:<host_width$}  STATUS", "NAME", "HOST");
    for (name, host, status) in rows {
        let label = if default.as_deref() == Some(name.as_str()) {
            format!("{name} (default)")
        } else {
            name
        };
        println!("{label:<name_width$}  {host:<host_width$}  {status}");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteTargetEntry {
    pub name: String,
    pub host: Option<String>,
    pub is_local: bool,
    pub is_default: bool,
}

fn targets(config: &Config) -> Vec<RemoteTargetEntry> {
    let default = resolve_placement(config, None, false).ok();
    let local = RemoteTargetEntry {
        name: "local".to_string(),
        host: None,
        is_local: true,
        is_default: default == Some(Placement::Local),
    };
    let remotes = config
        .remotes
        .iter()
        .map(|(name, remote)| RemoteTargetEntry {
            name: name.clone(),
            host: Some(remote.host.clone()),
            is_local: false,
            is_default: matches!(&default, Some(Placement::Remote(d)) if d == name),
        });
    let mut entries: Vec<_> = std::iter::once(local).chain(remotes).collect();
    entries.sort_by_key(|entry| !entry.is_default);
    entries
}

fn list_targets(config: &Config, json: bool) -> Result<()> {
    let entries = targets(config);
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    let label = |entry: &RemoteTargetEntry| {
        if entry.is_default {
            format!("{} (default)", entry.name)
        } else {
            entry.name.clone()
        }
    };
    let name_width = entries
        .iter()
        .map(|e| label(e).len())
        .max()
        .unwrap_or(0)
        .max(4);
    println!("{:<name_width$}  HOST", "NAME");
    for entry in &entries {
        println!(
            "{:<name_width$}  {}",
            label(entry),
            entry.host.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn status(config: &Config, name: &str) -> String {
    let check = || -> Result<String> {
        let spec = match RemoteSpec::resolve(config, name) {
            Ok(spec) => spec,
            Err(err) => return Ok(format!("invalid config: {err:#}")),
        };
        let transport = Transport::open(&spec)?;
        if let Err(err) = transport.connect() {
            return Ok(format!("unreachable: {err:#}"));
        }
        let pin = Pin::for_spec(&spec)?;
        Ok(match install::node_status(&transport, &spec, &pin)? {
            NodeStatus::Ready(hello) => {
                observe::current(config, name);
                format!(
                    "ready ({BIN} {}, protocol {})",
                    hello.version, hello.protocol
                )
            }
            NodeStatus::Missing => {
                format!("reachable, not set up: run {BIN} remote setup {name}")
            }
            NodeStatus::Mismatch(hello) => format!(
                "protocol mismatch: node speaks {}, run {BIN} remote doctor {name}",
                hello.protocol
            ),
            NodeStatus::Broken(_) => format!("node binary broken: run {BIN} remote setup {name}"),
        })
    };
    check().unwrap_or_else(|err| format!("error: {err:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RemoteConfig;

    fn config_with(remotes: &[(&str, &str)], placement: Option<&str>) -> Config {
        Config {
            remotes: remotes
                .iter()
                .map(|(name, host)| {
                    (
                        name.to_string(),
                        RemoteConfig {
                            host: host.to_string(),
                            path: None,
                            isolation: None,
                            binary: None,
                            terminal: None,
                            rex_client: None,
                        },
                    )
                })
                .collect(),
            placement: placement.map(str::to_string),
            ..Config::default()
        }
    }

    fn entry(name: &str, host: Option<&str>, is_default: bool) -> RemoteTargetEntry {
        RemoteTargetEntry {
            name: name.to_string(),
            host: host.map(str::to_string),
            is_local: host.is_none(),
            is_default,
        }
    }

    #[test]
    fn only_local_without_remotes() {
        assert_eq!(
            targets(&config_with(&[], None)),
            vec![entry("local", None, true)]
        );
    }

    #[test]
    fn local_default_comes_first_then_remotes_by_name() {
        let config = config_with(&[("winbox", "win.lan"), ("box", "box.lan")], None);
        assert_eq!(
            targets(&config),
            vec![
                entry("local", None, true),
                entry("box", Some("box.lan"), false),
                entry("winbox", Some("win.lan"), false),
            ]
        );
    }

    #[test]
    fn placement_remote_is_default_and_first() {
        let config = config_with(&[("winbox", "win.lan"), ("box", "box.lan")], Some("winbox"));
        assert_eq!(
            targets(&config),
            vec![
                entry("winbox", Some("win.lan"), true),
                entry("local", None, false),
                entry("box", Some("box.lan"), false),
            ]
        );
    }

    #[test]
    fn invalid_placement_marks_no_default() {
        let config = config_with(&[("box", "box.lan")], Some("gpu"));
        assert!(targets(&config).iter().all(|entry| !entry.is_default));
    }

    #[test]
    fn json_schema() {
        let config = config_with(&[("box", "box.lan")], Some("box"));
        assert_eq!(
            serde_json::to_value(targets(&config)).unwrap(),
            serde_json::json!([
                {"name": "box", "host": "box.lan", "is_local": false, "is_default": true},
                {"name": "local", "host": null, "is_local": true, "is_default": false},
            ])
        );
    }
}
