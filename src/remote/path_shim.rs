//! The node user's `~/.local/bin` PATH shim: installed by `workmux remote
//! setup`, reported by `workmux remote doctor`.
//!
//! Docker mode installs none. Its shell panes are node sessions on the host
//! without the `run/bin` wrapper, so a shim there would turn `workmux` in
//! those panes into an install that does not know the workspace.

use std::path::Path;

use crate::bin_name::BIN;
use anyhow::{Result, bail};

use super::RemoteSpec;
use super::client::Node;
use super::doctor::{Level, Row};
use super::host::{self, Resolution};
use super::install::Pin;
use super::node::path_shim::{PathShimRequest, PathShimResponse, ShimState};
use super::protocol;
use crate::config::RemoteIsolation;

fn query(node: &Node, install: bool) -> Result<PathShimResponse> {
    node.call(protocol::PATH_SHIM, &PathShimRequest { install }, false)
}

/// Point the shim at `node`'s pin and print the outcome. A failure is a
/// warning: the node works without it.
pub fn setup(node: &Node) {
    if node.spec.isolation == RemoteIsolation::Container {
        return;
    }
    if let Err(err) = install(node) {
        println!(
            "! {}: could not put {BIN} on the login PATH: {err:#}",
            node.spec.name
        );
    }
}

fn install(node: &Node) -> Result<()> {
    let name = &node.spec.name;
    let shim = query(node, true)?;
    match shim.state {
        ShimState::Foreign => {
            println!(
                "- {name}: left {} alone; it was not written by `{BIN} remote setup`",
                shim.path
            );
            return Ok(());
        }
        ShimState::Missing => bail!("{} was not written", shim.path),
        ShimState::Ours => {}
    }
    let already = if shim.changed { "" } else { "already " };
    println!("✓ {name}: {} {already}runs {}", shim.path, node.pin.name);
    if let Some(hint) = path_hint(
        &shim.path,
        &host::resolve(&node.transport, BIN, &shim.path)?,
    ) {
        println!("! {name}: {hint}");
    }
    Ok(())
}

/// What to change when login shells do not run the shim at `path`.
fn path_hint(path: &str, resolution: &Resolution) -> Option<String> {
    let dir = Path::new(path)
        .parent()
        .unwrap_or(Path::new(path))
        .display();
    match resolution {
        Resolution::Expected => None,
        Resolution::Missing => Some(format!(
            "login shells do not find {path}; add {dir} to PATH in your shell profile"
        )),
        Resolution::Other(other) => Some(format!(
            "login shells run {other}, not {path}; put {dir} earlier on PATH in your shell profile"
        )),
    }
}

/// `remote doctor` row; `ready` is whether the pinned node binary answers.
pub fn doctor_rows(spec: &RemoteSpec, ready: bool) -> Vec<Row> {
    if spec.isolation == RemoteIsolation::Container {
        return vec![Row::new(
            "path",
            Level::Skip,
            format!("Docker mode puts no {BIN} on the login PATH"),
        )];
    }
    if !ready {
        return vec![Row::new(
            "path",
            Level::Skip,
            "the node binary is not ready",
        )];
    }
    let setup = format!("{BIN} remote setup {}", spec.name);
    let row = (|| -> Result<Row> {
        let node = Node::connect_reader(spec, protocol::PATH_SHIM)?;
        let shim = query(&node, false)?;
        let pin = &Pin::for_spec(spec)?.name;
        let target = shim.target.as_deref().map(Path::new);
        Ok(match shim.state {
            ShimState::Missing => Row::new(
                "path",
                Level::Warn,
                format!(
                    "{} is missing: shells on the server have no {BIN}",
                    shim.path
                ),
            )
            .fix(setup.clone()),
            ShimState::Foreign => Row::new(
                "path",
                Level::Skip,
                format!("{} is not the setup shim; setup leaves it alone", shim.path),
            ),
            ShimState::Ours if target.and_then(Path::file_name) != Some(pin.as_ref()) => Row::new(
                "path",
                Level::Warn,
                format!(
                    "{} runs {}, not {pin}",
                    shim.path,
                    shim.target.as_deref().unwrap_or("nothing")
                ),
            )
            .fix(setup.clone()),
            ShimState::Ours => {
                let resolution = host::resolve(&node.transport, BIN, &shim.path)?;
                match path_hint(&shim.path, &resolution) {
                    None => Row::new(
                        "path",
                        Level::Ok,
                        format!("{} runs {pin} in login shells", shim.path),
                    ),
                    Some(hint) => Row::new("path", Level::Warn, hint),
                }
            }
        })
    })();
    vec![row.unwrap_or_else(|err| Row::new("path", Level::Warn, format!("{err:#}")).fix(setup))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_name_the_shim_directory() {
        let path = "/home/u/.local/bin/workrex";
        assert_eq!(path_hint(path, &Resolution::Expected), None);
        assert_eq!(
            path_hint(path, &Resolution::Missing).unwrap(),
            "login shells do not find /home/u/.local/bin/workrex; add /home/u/.local/bin to \
             PATH in your shell profile"
        );
        assert_eq!(
            path_hint(path, &Resolution::Other("/usr/bin/workrex".into())).unwrap(),
            "login shells run /usr/bin/workrex, not /home/u/.local/bin/workrex; put \
             /home/u/.local/bin earlier on PATH in your shell profile"
        );
    }
}
