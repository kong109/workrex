//! Sidebar hosting for backends that run the sidebar themselves.
//!
//! tmux keeps its own sidebar code and returns `None` from
//! [`Multiplexer::sidebar`](super::Multiplexer::sidebar). A backend that
//! returns a host takes every sidebar request at the entry points.

use anyhow::Result;

use crate::config::{SidebarHeight, SidebarPosition, SidebarWidth};

use super::TmuxBackend;
use super::tmux::TmuxSidebarSnapshot;

pub(crate) trait SidebarHost: Send + Sync {
    /// Apply one user request end to end. On and Off are idempotent.
    fn handle(&self, request: SidebarRequest) -> Result<()>;

    /// Wake the daemon for an observation now. Best effort.
    fn refresh(&self);

    /// The sidebar pane this process runs in, if it runs in one.
    fn pane_identity(&self) -> Option<HostPane>;

    /// Called once in the daemon process. The returned value serves the
    /// daemon's reads and writes, and keeps the host's own work running for
    /// as long as it lives.
    fn attach_daemon(&self) -> Result<Box<dyn DaemonIo>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostPane {
    pub session_name: String,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug)]
pub(crate) enum SidebarRequest {
    Switch {
        change: Change,
        reach: Reach,
        appearance: Appearance,
    },
    Navigate(Nav),
    Filter(Option<String>),
    Group {
        mode: Option<String>,
        clear: bool,
    },
    Views(Option<String>),
    ExpandedGroups(Vec<String>),
    ToggleSleeping {
        pane_id: String,
    },
    Quit {
        pane: HostPane,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    On,
    Off,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    Global,
    CurrentSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nav {
    Next,
    Prev,
    Jump(usize),
}

#[derive(Debug, Default)]
pub(crate) struct Appearance {
    pub position: Option<SidebarPosition>,
    pub width: Option<SidebarWidth>,
    pub height: Option<SidebarHeight>,
}

/// The server reads and writes of one sidebar daemon tick.
pub(crate) trait DaemonIo: Send {
    fn sidebar_snapshot(&self) -> Result<TmuxSidebarSnapshot>;
    fn set_global_option(&self, name: &str, value: &str) -> Result<()>;
    fn unset_global_option(&self, name: &str) -> Result<()>;
    fn set_window_option(&self, window: &str, name: &str, value: &str) -> Result<()>;
    fn unset_window_option(&self, window: &str, name: &str) -> Result<()>;
}

impl DaemonIo for TmuxBackend {
    fn sidebar_snapshot(&self) -> Result<TmuxSidebarSnapshot> {
        TmuxBackend::sidebar_snapshot(self)
    }

    fn set_global_option(&self, name: &str, value: &str) -> Result<()> {
        TmuxBackend::set_global_option(self, name, value)
    }

    fn unset_global_option(&self, name: &str) -> Result<()> {
        TmuxBackend::unset_global_option(self, name)
    }

    fn set_window_option(&self, window: &str, name: &str, value: &str) -> Result<()> {
        TmuxBackend::set_window_option(self, window, name, value)
    }

    fn unset_window_option(&self, window: &str, name: &str) -> Result<()> {
        TmuxBackend::unset_window_option(self, window, name)
    }
}
