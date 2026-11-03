//! Sidebar requests for backends that host the sidebar themselves. tmux has
//! no host and keeps the code in this module's parent.

mod rex;

use anyhow::{Result, anyhow, bail};

use crate::cli::{SidebarAction, SidebarAppearanceArgs};
use crate::multiplexer::{
    Appearance, Change, DaemonIo, HostPane, Multiplexer, Nav, Reach, SidebarRequest, TmuxBackend,
    create_backend, detect_backend,
};

use super::app::HostIdentity;

pub(crate) fn hosted() -> bool {
    create_backend(detect_backend()).sidebar().is_some()
}

pub(crate) fn handle_cli(
    session: bool,
    appearance: SidebarAppearanceArgs,
    action: Option<SidebarAction>,
) -> Result<()> {
    let mux = create_backend(detect_backend());
    let host = mux
        .sidebar()
        .ok_or_else(|| anyhow!("{} does not host a sidebar", mux.name()))?;
    let reach = if session {
        Reach::CurrentSession
    } else {
        Reach::Global
    };
    let switch = |change, appearance: SidebarAppearanceArgs| SidebarRequest::Switch {
        change,
        reach,
        appearance: Appearance {
            position: appearance.position,
            width: appearance.width,
            height: appearance.height,
        },
    };
    let request = match action {
        Some(SidebarAction::On { appearance }) => switch(Change::On, appearance),
        Some(SidebarAction::Off) => {
            if appearance.position.is_some()
                || appearance.width.is_some()
                || appearance.height.is_some()
            {
                bail!("--position, --width, and --height only apply when enabling the sidebar");
            }
            switch(Change::Off, SidebarAppearanceArgs::default())
        }
        Some(SidebarAction::Next) => SidebarRequest::Navigate(Nav::Next),
        Some(SidebarAction::Prev) => SidebarRequest::Navigate(Nav::Prev),
        Some(SidebarAction::Jump { index }) => {
            SidebarRequest::Navigate(Nav::Jump(usize::try_from(index)?))
        }
        Some(SidebarAction::View { mode }) => SidebarRequest::Views(mode),
        Some(SidebarAction::Filter { mode }) => SidebarRequest::Filter(mode),
        Some(SidebarAction::Group { mode, clear }) => SidebarRequest::Group { mode, clear },
        None => switch(Change::Toggle, appearance),
    };
    host.handle(request)
}

/// The daemon's server reads and writes: the host's, or tmux's.
pub(super) fn daemon_io(mux: &dyn Multiplexer, tmux: TmuxBackend) -> Result<Box<dyn DaemonIo>> {
    match mux.sidebar() {
        Some(host) => host.attach_daemon(),
        None => Ok(Box::new(tmux)),
    }
}

impl From<HostPane> for HostIdentity {
    fn from(pane: HostPane) -> Self {
        Self {
            session_name: pane.session_name,
            session_id: pane.session_id,
            window_id: pane.window_id,
            pane_id: pane.pane_id,
        }
    }
}

impl From<HostIdentity> for HostPane {
    fn from(identity: HostIdentity) -> Self {
        Self {
            session_name: identity.session_name,
            session_id: identity.session_id,
            window_id: identity.window_id,
            pane_id: identity.pane_id,
        }
    }
}
