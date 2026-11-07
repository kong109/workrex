mod agent_display;
mod agent_identity;
mod agent_setup;
mod agent_staleness;
mod bin_name;
mod claude;
mod cli;
mod cmd;
mod command;
mod config;
mod creation_time;
mod frozen_config;
mod git;
mod github;
mod gitlab;
mod llm;
mod logger;
mod markdown;
mod multiplexer;
mod naming;
mod nerdfont;
mod prompt;
mod remote;
mod sandbox;
mod shell;
mod skills;
mod spinner;
mod state;
mod template;
mod terminal_style;
#[cfg(test)]
mod test_support;
mod tips;
mod ui;
mod util;
mod workflow;
mod xdg;

use anyhow::Result;
use tracing::{error, info};

fn main() -> Result<()> {
    logger::init()?;
    let context = LogContext::current();
    let args = std::env::args().collect::<Vec<_>>();
    let deferred_cleanup_worker = args.get(1).is_some_and(|arg| arg == "_deferred-cleanup");
    info!(
        args = ?args,
        cwd = ?context.cwd,
        rex_block = ?context.rex_block,
        "workrex start"
    );

    match cli::run() {
        Ok(result) => {
            info!(
                cwd = ?context.cwd,
                rex_block = ?context.rex_block,
                "workrex finished successfully"
            );
            Ok(result)
        }
        Err(err) => {
            if !deferred_cleanup_worker {
                error!(error = ?err, "workrex failed");
            }
            Err(err)
        }
    }
}

struct LogContext {
    cwd: Option<std::path::PathBuf>,
    rex_block: Option<String>,
}

impl LogContext {
    fn current() -> Self {
        Self {
            cwd: std::env::current_dir().ok(),
            rex_block: std::env::var("REX_BLOCK").ok(),
        }
    }
}
