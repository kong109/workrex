//! Rex sidebar requests and server observations.

use anyhow::Result;

use crate::config::{SidebarHeight, SidebarPosition, SidebarWidth};

use super::LivePaneInfo;
use std::collections::{HashMap, HashSet};
pub(crate) struct SidebarHostSnapshot {
    pub live_panes: HashMap<String, LivePaneInfo>,
    pub window_statuses: HashMap<String, Option<String>>,
    pub active_windows: HashSet<(String, String)>,
    pub pane_window_ids: HashMap<String, String>,
    pub pane_window_indexes: HashMap<String, u32>,
    pub active_pane_ids: HashSet<String>,
    pub window_pane_counts: HashMap<String, usize>,
    pub server_boot_id: Option<String>,
    pub position: Option<String>,
    pub layout: Option<String>,
    pub filter: Option<String>,
    pub views_enabled: Option<String>,
    pub sleeping_panes: Option<String>,
    pub group_by: Option<String>,
    pub expanded_groups: Option<String>,
}

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
    Layout(String),
    ExpandedGroups(Vec<String>),
    Wake {
        pane_id: String,
    },
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
    fn sidebar_snapshot(&self) -> Result<SidebarHostSnapshot>;
    fn set_global_option(&self, name: &str, value: &str) -> Result<()>;
    fn unset_global_option(&self, name: &str) -> Result<()>;
    fn set_window_option(&self, window: &str, name: &str, value: &str) -> Result<()>;
    fn unset_window_option(&self, window: &str, name: &str) -> Result<()>;
}
