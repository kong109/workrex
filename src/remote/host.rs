//! Read-only checks for executables on the remote user's login PATH.

use std::time::Duration;

use anyhow::{Result, bail, ensure};

use super::RemoteSpec;
use super::transport::{Input, Transport};
use crate::shell::shell_quote;

/// Probe without installing the node or creating workspace state. Only the
/// presence of each executable is returned, never the login environment.
pub fn available(transport: &Transport, programs: &[String]) -> Result<Vec<bool>> {
    if programs.is_empty() {
        return Ok(Vec::new());
    }
    let mut body = String::new();
    for (index, program) in programs.iter().enumerate() {
        body.push_str(&format!(
            "if command -v -- {} >/dev/null 2>&1; then printf 'workmux-cli-{index}=1\\n'; else printf 'workmux-cli-{index}=0\\n'; fi\n",
            shell_quote(program)
        ));
    }
    let text = login_probe(transport, &body)?;
    programs
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let key = format!("workmux-cli-{index}=");
            match text.lines().find_map(|line| line.strip_prefix(&key)) {
                Some("1") => Ok(true),
                Some("0") => Ok(false),
                _ => bail!("could not check the remote login PATH: missing probe result"),
            }
        })
        .collect()
}

/// What a login shell runs for `program`, compared with `expected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Expected,
    /// Another file comes first on the login PATH.
    Other(String),
    Missing,
}

/// Where `program` resolves on the login PATH. Only that one path is
/// returned, never the login environment.
pub fn resolve(transport: &Transport, program: &str, expected: &str) -> Result<Resolution> {
    let body = format!(
        "p=$(command -v -- {})\nif [ \"$p\" = {} ]; then echo workmux-cli=expected; \
         elif [ -n \"$p\" ]; then printf 'workmux-cli=other:%s\\n' \"$p\"; \
         else echo workmux-cli=missing; fi\n",
        shell_quote(program),
        shell_quote(expected)
    );
    let text = login_probe(transport, &body)?;
    Ok(
        match text
            .lines()
            .find_map(|line| line.strip_prefix("workmux-cli="))
        {
            Some("expected") => Resolution::Expected,
            Some("missing") => Resolution::Missing,
            Some(other) if other.starts_with("other:") => {
                Resolution::Other(other["other:".len()..].to_string())
            }
            _ => bail!("could not check the remote login PATH: missing probe result"),
        },
    )
}

/// Run the POSIX `body` with the PATH of the remote user's login shell and
/// return its stdout.
fn login_probe(transport: &Transport, body: &str) -> Result<String> {
    // The login shell supplies exported PATH; a POSIX child parses the probe
    // even when the user's shell is fish or another non-POSIX shell.
    let command = format!("exec sh -c {}", shell_quote(body));
    let script = format!(
        "exec \"${{SHELL:-/bin/sh}}\" -l -c {}\n",
        shell_quote(&command)
    );
    let output = transport.run(&script, Input::Null, Some(Duration::from_secs(60)))?;
    ensure!(
        output.success(),
        "could not check the remote login PATH: {}",
        output.stderr_line()
    );
    Ok(output.stdout_text())
}

pub fn missing_message(spec: &RemoteSpec, program: &str) -> String {
    format!(
        "{program} is not installed on {}; install it on the host login PATH, or set remotes.{}.isolation: container",
        spec.name, spec.name
    )
}

pub fn require(spec: &RemoteSpec, programs: &[String]) -> Result<()> {
    if programs.is_empty() {
        return Ok(());
    }
    let transport = Transport::open(spec)?;
    transport.connect()?;
    for (program, found) in programs.iter().zip(available(&transport, programs)?) {
        if !found {
            bail!("{}", missing_message(spec, program));
        }
    }
    Ok(())
}
